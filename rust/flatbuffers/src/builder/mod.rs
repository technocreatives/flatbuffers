/*
 * Copyright 2018 Google Inc. All rights reserved.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use core::iter::{DoubleEndedIterator, ExactSizeIterator};

use core::cmp::max;
use core::marker::PhantomData;
use core::ptr::write_bytes;
use core::slice::from_raw_parts;

use crate::endian_scalar::{emplace_scalar, read_scalar_at};
use crate::primitives::*;
use crate::push::{Push, PushAlignment};
use crate::table::Table;
use crate::vector::{SafeSliceAccess, Vector};
use crate::vtable::{field_index_to_field_offset, VTable};
use crate::vtable_writer::VTableWriter;

#[cfg(any(feature = "std", feature = "alloc"))]
mod vec;
#[cfg(any(feature = "std", feature = "alloc"))]
pub use vec::VecFlatBufferBuilderStorage;

mod heapless_vec;
pub use heapless_vec::HeaplessFlatBufferBuilderStorage;

const HEAPLESS_STRING_VECTOR_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldLoc {
    off: UOffsetT,
    id: VOffsetT,
}

pub trait FlatBufferBuilderStorage {
    fn bufs(&mut self) -> (&mut [u8], &mut [FieldLoc], &mut [UOffsetT]);
    fn resize(&mut self, size: usize);

    fn reset_field_locs(&mut self);
    fn reset_written_vtable_revpos(&mut self);

    fn push_field_loc(&mut self, item: FieldLoc);
    fn push_written_vtable_revpos(&mut self, item: UOffsetT);

    fn buffer(&self) -> &[u8];
    fn buffer_mut(&mut self) -> &mut [u8];
    fn field_locs(&self) -> &[FieldLoc];
    fn written_vtable_revpos(&self) -> &[UOffsetT];
}

/// FlatBufferBuilder builds a FlatBuffer through manipulating its internal
/// state. It has an owned `Vec<u8>` that grows as needed (up to the hardcoded
/// limit of 2GiB, which is set by the FlatBuffers format).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlatBufferBuilder<'fbb, T: FlatBufferBuilderStorage> {
    storage: T,
    head: usize,

    nested: bool,
    finished: bool,

    min_align: usize,
    force_defaults: bool,

    _phantom: PhantomData<&'fbb ()>,
}

pub trait GenericFlatBufferBuilder<'fbb> {
    fn reset(&mut self);
    fn push<P: Push>(&mut self, x: P) -> WIPOffset<P::Output>;
    fn push_slot<X: Push + PartialEq>(&mut self, slotoff: VOffsetT, x: X, default: X);
    fn push_slot_always<X: Push>(&mut self, slotoff: VOffsetT, x: X);
    fn num_written_vtables(&self) -> usize;
    fn start_table(&mut self) -> WIPOffset<TableUnfinishedWIPOffset>;
    fn end_table(
        &mut self,
        off: WIPOffset<TableUnfinishedWIPOffset>,
    ) -> WIPOffset<TableFinishedWIPOffset>;
    fn start_vector<T: Push>(&mut self, num_items: usize);
    fn end_vector<T: Push>(&mut self, num_elems: usize) -> WIPOffset<Vector<'fbb, T>>;
    fn create_string<'a: 'b, 'b>(&'a mut self, s: &'b str) -> WIPOffset<&'fbb str>;
    fn create_byte_string(&mut self, data: &[u8]) -> WIPOffset<&'fbb [u8]>;
    fn create_vector_direct<'a: 'b, 'b, T: SafeSliceAccess + Push + Sized + 'b>(
        &'a mut self,
        items: &'b [T],
    ) -> WIPOffset<Vector<'fbb, T>>;
    fn create_vector_from_slices_direct<'a: 'b, 'b, T: SafeSliceAccess + Push + Sized + 'b>(
        &'a mut self,
        items: &'b [&'b [T]],
    ) -> WIPOffset<Vector<'fbb, T>>;
    fn create_vector_of_strings<'a, 'b>(
        &'a mut self,
        xs: &'b [&'b str],
    ) -> WIPOffset<Vector<'fbb, ForwardsUOffset<&'fbb str>>>;
    fn create_vector<'a: 'b, 'b, T: Push + Copy + 'b>(
        &'a mut self,
        items: &'b [T],
    ) -> WIPOffset<Vector<'fbb, T::Output>>;
    fn create_vector_from_iter<T: Push + Copy>(
        &mut self,
        items: impl ExactSizeIterator<Item = T> + DoubleEndedIterator,
    ) -> WIPOffset<Vector<'fbb, T::Output>>;
    fn force_defaults(&mut self, force_defaults: bool);
    fn unfinished_data(&self) -> &[u8];
    fn finished_data(&self) -> &[u8];
    fn required(
        &self,
        tab_revloc: WIPOffset<TableFinishedWIPOffset>,
        slot_byte_loc: VOffsetT,
        assert_msg_name: &'static str,
    );
    fn finish_size_prefixed<T>(&mut self, root: WIPOffset<T>, file_identifier: Option<&str>);
    fn finish<T>(&mut self, root: WIPOffset<T>, file_identifier: Option<&str>);
    fn finish_minimal<T>(&mut self, root: WIPOffset<T>);
}

impl<'fbb, S: FlatBufferBuilderStorage> GenericFlatBufferBuilder<'fbb>
    for FlatBufferBuilder<'fbb, S>
{
    /// Reset the FlatBufferBuilder internal state. Use this method after a
    /// call to a `finish` function in order to re-use a FlatBufferBuilder.
    ///
    /// This function is the only way to reset the `finished` state and start
    /// again.
    ///
    /// If you are using a FlatBufferBuilder repeatedly, make sure to use this
    /// function, because it re-uses the FlatBufferBuilder's existing
    /// heap-allocated `Vec<u8>` internal buffer. This offers significant speed
    /// improvements as compared to creating a new FlatBufferBuilder for every
    /// new object.
    fn reset(&mut self) {
        // memset only the part of the buffer that could be dirty:
        {
            let to_clear = self.storage.buffer().len() - self.head;
            let ptr = (&mut self.storage.buffer_mut()[self.head..]).as_mut_ptr();
            unsafe {
                write_bytes(ptr, 0, to_clear);
            }
        }

        self.head = self.storage.buffer().len();
        self.storage.reset_written_vtable_revpos();

        self.nested = false;
        self.finished = false;

        self.min_align = 0;
    }

    /// Push a Push'able value onto the front of the in-progress data.
    ///
    /// This function uses traits to provide a unified API for writing
    /// scalars, tables, vectors, and WIPOffsets.
    #[inline]
    fn push<P: Push>(&mut self, x: P) -> WIPOffset<P::Output> {
        let sz = P::size();
        self.align(sz, P::alignment());
        self.make_space(sz);
        {
            let (dst, rest) = (&mut self.storage.buffer_mut()[self.head..]).split_at_mut(sz);
            x.push(dst, rest);
        }
        WIPOffset::new(self.used_space() as UOffsetT)
    }

    /// Push a Push'able value onto the front of the in-progress data, and
    /// store a reference to it in the in-progress vtable. If the value matches
    /// the default, then this is a no-op.
    #[inline]
    fn push_slot<X: Push + PartialEq>(&mut self, slotoff: VOffsetT, x: X, default: X) {
        self.assert_nested("push_slot");
        if x != default || self.force_defaults {
            self.push_slot_always(slotoff, x);
        }
    }

    /// Push a Push'able value onto the front of the in-progress data, and
    /// store a reference to it in the in-progress vtable.
    #[inline]
    fn push_slot_always<X: Push>(&mut self, slotoff: VOffsetT, x: X) {
        self.assert_nested("push_slot_always");
        let off = self.push(x);
        self.track_field(slotoff, off.value());
    }

    /// Retrieve the number of vtables that have been serialized into the
    /// FlatBuffer. This is primarily used to check vtable deduplication.
    #[inline]
    fn num_written_vtables(&self) -> usize {
        self.storage.written_vtable_revpos().len()
    }

    /// Start a Table write.
    ///
    /// Asserts that the builder is not in a nested state.
    ///
    /// Users probably want to use `push_slot` to add values after calling this.
    #[inline]
    fn start_table(&mut self) -> WIPOffset<TableUnfinishedWIPOffset> {
        self.assert_not_nested(
            "start_table can not be called when a table or vector is under construction",
        );
        self.nested = true;

        WIPOffset::new(self.used_space() as UOffsetT)
    }

    /// End a Table write.
    ///
    /// Asserts that the builder is in a nested state.
    #[inline]
    fn end_table(
        &mut self,
        off: WIPOffset<TableUnfinishedWIPOffset>,
    ) -> WIPOffset<TableFinishedWIPOffset> {
        self.assert_nested("end_table");

        let o = self.write_vtable(off);

        self.nested = false;
        self.storage.reset_field_locs();

        WIPOffset::new(o.value())
    }

    /// Start a Vector write.
    ///
    /// Asserts that the builder is not in a nested state.
    ///
    /// Most users will prefer to call `create_vector`.
    /// Speed optimizing users who choose to create vectors manually using this
    /// function will want to use `push` to add values.
    #[inline]
    fn start_vector<T: Push>(&mut self, num_items: usize) {
        self.assert_not_nested(
            "start_vector can not be called when a table or vector is under construction",
        );
        self.nested = true;
        self.align(num_items * T::size(), T::alignment().max_of(SIZE_UOFFSET));
    }

    /// End a Vector write.
    ///
    /// Note that the `num_elems` parameter is the number of written items, not
    /// the byte count.
    ///
    /// Asserts that the builder is in a nested state.
    #[inline]
    fn end_vector<T: Push>(&mut self, num_elems: usize) -> WIPOffset<Vector<'fbb, T>> {
        self.assert_nested("end_vector");
        self.nested = false;
        let o = self.push::<UOffsetT>(num_elems as UOffsetT);
        WIPOffset::new(o.value())
    }

    /// Create a utf8 string.
    ///
    /// The wire format represents this as a zero-terminated byte vector.
    #[inline]
    fn create_string<'a: 'b, 'b>(&'a mut self, s: &'b str) -> WIPOffset<&'fbb str> {
        self.assert_not_nested(
            "create_string can not be called when a table or vector is under construction",
        );
        WIPOffset::new(self.create_byte_string(s.as_bytes()).value())
    }

    /// Create a zero-terminated byte vector.
    #[inline]
    fn create_byte_string(&mut self, data: &[u8]) -> WIPOffset<&'fbb [u8]> {
        self.assert_not_nested(
            "create_byte_string can not be called when a table or vector is under construction",
        );
        self.align(data.len() + 1, PushAlignment::new(SIZE_UOFFSET));
        self.push(0u8);
        self.push_bytes_unprefixed(data);
        self.push(data.len() as UOffsetT);
        WIPOffset::new(self.used_space() as UOffsetT)
    }

    /// Create a vector by memcpy'ing. This is much faster than calling
    /// `create_vector`, but the underlying type must be represented as
    /// little-endian on the host machine. This property is encoded in the
    /// type system through the SafeSliceAccess trait. The following types are
    /// always safe, on any platform: bool, u8, i8, and any
    /// FlatBuffers-generated struct.
    #[inline]
    fn create_vector_direct<'a: 'b, 'b, T: SafeSliceAccess + Push + Sized + 'b>(
        &'a mut self,
        items: &'b [T],
    ) -> WIPOffset<Vector<'fbb, T>> {
        self.assert_not_nested(
            "create_vector_direct can not be called when a table or vector is under construction",
        );
        let elem_size = T::size();
        self.align(items.len() * elem_size, T::alignment().max_of(SIZE_UOFFSET));

        let bytes = {
            let ptr = items.as_ptr() as *const T as *const u8;
            unsafe { from_raw_parts(ptr, items.len() * elem_size) }
        };
        self.push_bytes_unprefixed(bytes);
        self.push(items.len() as UOffsetT);

        WIPOffset::new(self.used_space() as UOffsetT)
    }

    /// Same as [create_vector_direct](Self::create_vector_direct), except that this function takes a slice of slices,
    /// each of which are concatenated to form the final vector.
    #[inline]
    fn create_vector_from_slices_direct<'a: 'b, 'b, T: SafeSliceAccess + Push + Sized + 'b>(
        &'a mut self,
        parts: &'b [&'b [T]],
    ) -> WIPOffset<Vector<'fbb, T>> {
        self.assert_not_nested(
            "create_vector_from_slices_direct can not be called when a table or vector is under construction",
        );
        let elem_size = T::size();
        let len: usize = parts.iter().map(|part| part.len()).sum();
        self.align(len * elem_size, T::alignment().max_of(SIZE_UOFFSET));

        // note that this happens in reverse, because the buffer is built back-to-front:
        for part in parts.iter().rev() {
            let bytes = {
                let ptr = part.as_ptr() as *const T as *const u8;
                unsafe { from_raw_parts(ptr, part.len() * elem_size) }
            };

            self.push_bytes_unprefixed(bytes);
        }
        self.push(len as UOffsetT);

        WIPOffset::new(self.used_space() as UOffsetT)
    }

    /// Create a vector of strings.
    ///
    /// Speed-sensitive users may wish to reduce memory usage by creating the
    /// vector manually: use `start_vector`, `push`, and `end_vector`.
    #[inline]
    fn create_vector_of_strings<'a, 'b>(
        &'a mut self,
        xs: &'b [&'b str],
    ) -> WIPOffset<Vector<'fbb, ForwardsUOffset<&'fbb str>>> {
        self.assert_not_nested("create_vector_of_strings can not be called when a table or vector is under construction");
        let mut offsets: heapless::Vec<WIPOffset<&str>, HEAPLESS_STRING_VECTOR_CAPACITY> =
            heapless::Vec::new();
        debug_assert!(
            xs.len() < offsets.capacity(),
            "string vector of length {} can't be longer than HEAPLESS_STRING_VECTOR_CAPACITY",
            xs.len()
        );
        offsets
            .resize_default(xs.len())
            .expect("string vector can't be longer than HEAPLESS_STRING_VECTOR_CAPACITY");

        // note that this happens in reverse, because the buffer is built back-to-front:
        for (i, &s) in xs.iter().enumerate().rev() {
            let o = self.create_string(s);
            offsets[i] = o;
        }
        self.create_vector(&offsets[..])
    }

    /// Create a vector of Push-able objects.
    ///
    /// Speed-sensitive users may wish to reduce memory usage by creating the
    /// vector manually: use `start_vector`, `push`, and `end_vector`.
    #[inline]
    fn create_vector<'a: 'b, 'b, T: Push + Copy + 'b>(
        &'a mut self,
        items: &'b [T],
    ) -> WIPOffset<Vector<'fbb, T::Output>> {
        let elem_size = T::size();
        self.align(items.len() * elem_size, T::alignment().max_of(SIZE_UOFFSET));
        for i in (0..items.len()).rev() {
            self.push(items[i]);
        }
        WIPOffset::new(self.push::<UOffsetT>(items.len() as UOffsetT).value())
    }

    /// Create a vector of Push-able objects.
    ///
    /// Speed-sensitive users may wish to reduce memory usage by creating the
    /// vector manually: use `start_vector`, `push`, and `end_vector`.
    #[inline]
    fn create_vector_from_iter<T: Push + Copy>(
        &mut self,
        items: impl ExactSizeIterator<Item = T> + DoubleEndedIterator,
    ) -> WIPOffset<Vector<'fbb, T::Output>> {
        let elem_size = T::size();
        let len = items.len();
        self.align(len * elem_size, T::alignment().max_of(SIZE_UOFFSET));
        for item in items.rev() {
            self.push(item);
        }
        WIPOffset::new(self.push::<UOffsetT>(len as UOffsetT).value())
    }

    /// Set whether default values are stored.
    ///
    /// In order to save space, fields that are set to their default value
    /// aren't stored in the buffer. Setting `force_defaults` to `true`
    /// disables this optimization.
    ///
    /// By default, `force_defaults` is `false`.
    #[inline]
    fn force_defaults(&mut self, force_defaults: bool) {
        self.force_defaults = force_defaults;
    }

    /// Get the byte slice for the data that has been written, regardless of
    /// whether it has been finished.
    #[inline]
    fn unfinished_data(&self) -> &[u8] {
        &self.storage.buffer()[self.head..]
    }
    /// Get the byte slice for the data that has been written after a call to
    /// one of the `finish` functions.
    #[inline]
    fn finished_data(&self) -> &[u8] {
        self.assert_finished("finished_bytes cannot be called when the buffer is not yet finished");
        &self.storage.buffer()[self.head..]
    }
    /// Assert that a field is present in the just-finished Table.
    ///
    /// This is somewhat low-level and is mostly used by the generated code.
    #[inline]
    fn required(
        &self,
        tab_revloc: WIPOffset<TableFinishedWIPOffset>,
        slot_byte_loc: VOffsetT,
        assert_msg_name: &'static str,
    ) {
        let idx = self.used_space() - tab_revloc.value() as usize;
        let tab = Table::new(&self.storage.buffer()[self.head..], idx);
        let o = tab.vtable().get(slot_byte_loc) as usize;
        assert!(o != 0, "missing required field {}", assert_msg_name);
    }

    /// Finalize the FlatBuffer by: aligning it, pushing an optional file
    /// identifier on to it, pushing a size prefix on to it, and marking the
    /// internal state of the FlatBufferBuilder as `finished`. Afterwards,
    /// users can call `finished_data` to get the resulting data.
    #[inline]
    fn finish_size_prefixed<T>(&mut self, root: WIPOffset<T>, file_identifier: Option<&str>) {
        self.finish_with_opts(root, file_identifier, true);
    }

    /// Finalize the FlatBuffer by: aligning it, pushing an optional file
    /// identifier on to it, and marking the internal state of the
    /// FlatBufferBuilder as `finished`. Afterwards, users can call
    /// `finished_data` to get the resulting data.
    #[inline]
    fn finish<T>(&mut self, root: WIPOffset<T>, file_identifier: Option<&str>) {
        self.finish_with_opts(root, file_identifier, false);
    }

    /// Finalize the FlatBuffer by: aligning it and marking the internal state
    /// of the FlatBufferBuilder as `finished`. Afterwards, users can call
    /// `finished_data` to get the resulting data.
    #[inline]
    fn finish_minimal<T>(&mut self, root: WIPOffset<T>) {
        self.finish_with_opts(root, None, false);
    }
}

impl<'fbb, S: FlatBufferBuilderStorage> FlatBufferBuilder<'fbb, S> {
    #[inline]
    fn used_space(&self) -> usize {
        self.storage.buffer().len() - self.head as usize
    }

    #[inline]
    fn track_field(&mut self, slot_off: VOffsetT, off: UOffsetT) {
        let fl = FieldLoc { id: slot_off, off };
        self.storage.push_field_loc(fl);
    }

    /// Write the VTable, if it is new.
    fn write_vtable(
        &mut self,
        table_tail_revloc: WIPOffset<TableUnfinishedWIPOffset>,
    ) -> WIPOffset<VTableWIPOffset> {
        self.assert_nested("write_vtable");

        // Write the vtable offset, which is the start of any Table.
        // We fill its value later.
        let object_revloc_to_vtable: WIPOffset<VTableWIPOffset> =
            WIPOffset::new(self.push::<UOffsetT>(0xF0F0_F0F0 as UOffsetT).value());

        // Layout of the data this function will create when a new vtable is
        // needed.
        // --------------------------------------------------------------------
        // vtable starts here
        // | x, x -- vtable len (bytes) [u16]
        // | x, x -- object inline len (bytes) [u16]
        // | x, x -- zero, or num bytes from start of object to field #0   [u16]
        // | ...
        // | x, x -- zero, or num bytes from start of object to field #n-1 [u16]
        // vtable ends here
        // table starts here
        // | x, x, x, x -- offset (negative direction) to the vtable [i32]
        // |               aka "vtableoffset"
        // | -- table inline data begins here, we don't touch it --
        // table ends here -- aka "table_start"
        // --------------------------------------------------------------------
        //
        // Layout of the data this function will create when we re-use an
        // existing vtable.
        //
        // We always serialize this particular vtable, then compare it to the
        // other vtables we know about to see if there is a duplicate. If there
        // is, then we erase the serialized vtable we just made.
        // We serialize it first so that we are able to do byte-by-byte
        // comparisons with already-serialized vtables. This 1) saves
        // bookkeeping space (we only keep revlocs to existing vtables), 2)
        // allows us to convert to little-endian once, then do
        // fast memcmp comparisons, and 3) by ensuring we are comparing real
        // serialized vtables, we can be more assured that we are doing the
        // comparisons correctly.
        //
        // --------------------------------------------------------------------
        // table starts here
        // | x, x, x, x -- offset (negative direction) to an existing vtable [i32]
        // |               aka "vtableoffset"
        // | -- table inline data begins here, we don't touch it --
        // table starts here: aka "table_start"
        // --------------------------------------------------------------------

        // fill the WIP vtable with zeros:
        let vtable_byte_len = get_vtable_byte_len(self.storage.field_locs());
        self.make_space(vtable_byte_len);

        // compute the length of the table (not vtable!) in bytes:
        let table_object_size = object_revloc_to_vtable.value() - table_tail_revloc.value();
        debug_assert!(table_object_size < 0x10000); // vTable use 16bit offsets.

        // Write the VTable (we may delete it afterwards, if it is a duplicate):
        let vt_start_pos = self.head;
        let vt_end_pos = self.head + vtable_byte_len;
        {
            let (buffer, field_locs, _) = self.storage.bufs();
            // write the vtable header:
            let vtfw = &mut VTableWriter::init(&mut buffer[vt_start_pos..vt_end_pos]);
            vtfw.write_vtable_byte_length(vtable_byte_len as VOffsetT);
            vtfw.write_object_inline_size(table_object_size as VOffsetT);

            // serialize every FieldLoc to the vtable:
            for &fl in field_locs.iter() {
                let pos: VOffsetT = (object_revloc_to_vtable.value() - fl.off) as VOffsetT;
                debug_assert_eq!(
                    vtfw.get_field_offset(fl.id),
                    0,
                    "tried to write a vtable field multiple times"
                );
                vtfw.write_field_offset(fl.id, pos);
            }
        }
        let dup_vt_use = {
            let this_vt = VTable::init(&self.storage.buffer()[..], self.head);
            self.find_duplicate_stored_vtable_revloc(this_vt)
        };

        let vt_use = match dup_vt_use {
            Some(n) => {
                VTableWriter::init(&mut self.storage.buffer_mut()[vt_start_pos..vt_end_pos])
                    .clear();
                self.head += vtable_byte_len;
                n
            }
            None => {
                let new_vt_use = self.used_space() as UOffsetT;
                self.storage.push_written_vtable_revpos(new_vt_use);
                new_vt_use
            }
        };

        {
            let n = self.head + self.used_space() - object_revloc_to_vtable.value() as usize;
            let saw = read_scalar_at::<UOffsetT>(&self.storage.buffer(), n);
            debug_assert_eq!(saw, 0xF0F0_F0F0);
            emplace_scalar::<SOffsetT>(
                &mut self.storage.buffer_mut()[n..n + SIZE_SOFFSET],
                vt_use as SOffsetT - object_revloc_to_vtable.value() as SOffsetT,
            );
        }

        self.storage.reset_field_locs();

        object_revloc_to_vtable
    }

    #[inline]
    fn find_duplicate_stored_vtable_revloc(&self, needle: VTable) -> Option<UOffsetT> {
        for &revloc in self.storage.written_vtable_revpos().iter().rev() {
            let o = VTable::init(
                self.storage.buffer(),
                self.head + self.used_space() - revloc as usize,
            );
            if needle == o {
                return Some(revloc);
            }
        }
        None
    }

    // Only call this when you know it is safe to double the size of the buffer.
    #[inline]
    fn grow_owned_buf(&mut self) {
        let old_len = self.storage.buffer().len();
        let new_len = max(1, old_len * 2);

        let starting_active_size = self.used_space();

        let diff = new_len - old_len;
        self.storage.resize(new_len);
        self.head += diff;

        let ending_active_size = self.used_space();
        debug_assert_eq!(starting_active_size, ending_active_size);

        if new_len == 1 {
            return;
        }

        // calculate the midpoint, and safely copy the old end data to the new
        // end position:
        let middle = new_len / 2;
        {
            let (left, right) = &mut self.storage.buffer_mut()[..].split_at_mut(middle);
            right.copy_from_slice(left);
        }
        // finally, zero out the old end data.
        {
            let ptr = (&mut self.storage.buffer_mut()[..middle]).as_mut_ptr();
            unsafe {
                write_bytes(ptr, 0, middle);
            }
        }
    }

    // with or without a size prefix changes how we load the data, so finish*
    // functions are split along those lines.
    fn finish_with_opts<T>(
        &mut self,
        root: WIPOffset<T>,
        file_identifier: Option<&str>,
        size_prefixed: bool,
    ) {
        self.assert_not_finished("buffer cannot be finished when it is already finished");
        self.assert_not_nested(
            "buffer cannot be finished when a table or vector is under construction",
        );
        self.storage.reset_written_vtable_revpos();

        let to_align = {
            // for the root offset:
            let a = SIZE_UOFFSET;
            // for the size prefix:
            let b = if size_prefixed { SIZE_UOFFSET } else { 0 };
            // for the file identifier (a string that is not zero-terminated):
            let c = if file_identifier.is_some() {
                FILE_IDENTIFIER_LENGTH
            } else {
                0
            };
            a + b + c
        };

        {
            let ma = PushAlignment::new(self.min_align);
            self.align(to_align, ma);
        }

        if let Some(ident) = file_identifier {
            debug_assert_eq!(ident.len(), FILE_IDENTIFIER_LENGTH);
            self.push_bytes_unprefixed(ident.as_bytes());
        }

        self.push(root);

        if size_prefixed {
            let sz = self.used_space() as UOffsetT;
            self.push::<UOffsetT>(sz);
        }
        self.finished = true;
    }

    #[inline]
    fn align(&mut self, len: usize, alignment: PushAlignment) {
        self.track_min_align(alignment.value());
        let s = self.used_space() as usize;
        self.make_space(padding_bytes(s + len, alignment.value()));
    }

    #[inline]
    fn track_min_align(&mut self, alignment: usize) {
        self.min_align = max(self.min_align, alignment);
    }

    #[inline]
    fn push_bytes_unprefixed(&mut self, x: &[u8]) -> UOffsetT {
        let n = self.make_space(x.len());
        self.storage.buffer_mut()[n..n + x.len()].copy_from_slice(x);

        n as UOffsetT
    }

    #[inline]
    fn make_space(&mut self, want: usize) -> usize {
        self.ensure_capacity(want);
        self.head -= want;
        self.head
    }

    #[inline]
    fn ensure_capacity(&mut self, want: usize) -> usize {
        if self.unused_ready_space() >= want {
            return want;
        }
        assert!(
            want <= FLATBUFFERS_MAX_BUFFER_SIZE,
            "cannot grow buffer beyond 2 gigabytes"
        );

        while self.unused_ready_space() < want {
            self.grow_owned_buf();
        }
        want
    }
    #[inline]
    fn unused_ready_space(&self) -> usize {
        self.head
    }
    #[inline]
    fn assert_nested(&self, fn_name: &'static str) {
        // we don't assert that self.field_locs.len() >0 because the vtable
        // could be empty (e.g. for empty tables, or for all-default values).
        debug_assert!(
            self.nested,
            "incorrect FlatBufferBuilder usage: {} must be called while in a nested state",
            fn_name
        );
    }
    #[inline]
    fn assert_not_nested(&self, msg: &'static str) {
        debug_assert!(!self.nested, "{}", msg);
    }
    #[inline]
    fn assert_finished(&self, msg: &'static str) {
        debug_assert!(self.finished, "{}", msg);
    }
    #[inline]
    fn assert_not_finished(&self, msg: &'static str) {
        debug_assert!(!self.finished, "{}", msg);
    }
}

/// Compute the length of the vtable needed to represent the provided FieldLocs.
/// If there are no FieldLocs, then provide the minimum number of bytes
/// required: enough to write the VTable header.
#[inline]
fn get_vtable_byte_len(field_locs: &[FieldLoc]) -> usize {
    let max_voffset = field_locs.iter().map(|fl| fl.id).max();
    match max_voffset {
        None => field_index_to_field_offset(0) as usize,
        Some(mv) => mv as usize + SIZE_VOFFSET,
    }
}

#[inline]
fn padding_bytes(buf_size: usize, scalar_size: usize) -> usize {
    // ((!buf_size) + 1) & (scalar_size - 1)
    (!buf_size).wrapping_add(1) & (scalar_size.wrapping_sub(1))
}
