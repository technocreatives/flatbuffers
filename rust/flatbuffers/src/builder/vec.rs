use crate::primitives::FLATBUFFERS_MAX_BUFFER_SIZE;
use crate::{
    builder::{FieldLoc, FlatBufferBuilder},
    FlatBufferBuilderStorage, UOffsetT,
};
#[cfg(all(not(feature = "std"), feature = "alloc"))]
use alloc::{vec, vec::Vec};
use core::marker::PhantomData;

#[cfg(any(feature = "std", feature = "alloc"))]
pub struct VecFlatBufferBuilderStorage {
    owned_buf: Vec<u8>,
    field_locs: Vec<FieldLoc>,
    written_vtable_revpos: Vec<UOffsetT>,
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl FlatBufferBuilderStorage for VecFlatBufferBuilderStorage {
    fn bufs(&mut self) -> (&mut [u8], &mut [FieldLoc], &mut [UOffsetT]) {
        (
            self.owned_buf.as_mut_slice(),
            self.field_locs.as_mut_slice(),
            self.written_vtable_revpos.as_mut_slice(),
        )
    }

    fn resize(&mut self, size: usize) {
        self.owned_buf.resize(size, 0)
    }

    fn reset_field_locs(&mut self) {
        self.field_locs.clear();
    }

    fn reset_written_vtable_revpos(&mut self) {
        self.written_vtable_revpos.clear();
    }

    fn push_field_loc(&mut self, item: FieldLoc) {
        self.field_locs.push(item)
    }

    fn push_written_vtable_revpos(&mut self, item: UOffsetT) {
        self.written_vtable_revpos.push(item)
    }

    fn buffer_mut(&mut self) -> &mut [u8] {
        self.owned_buf.as_mut_slice()
    }

    fn buffer(&self) -> &[u8] {
        self.owned_buf.as_slice()
    }

    fn field_locs(&self) -> &[FieldLoc] {
        self.field_locs.as_slice()
    }

    fn written_vtable_revpos(&self) -> &[UOffsetT] {
        self.written_vtable_revpos.as_slice()
    }
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl<'fbb> FlatBufferBuilder<'fbb, VecFlatBufferBuilderStorage> {
    /// Create a FlatBufferBuilder that is ready for writing.
    pub fn new() -> Self {
        Self::new_with_capacity(0)
    }

    /// Create a FlatBufferBuilder that is ready for writing, with a
    /// ready-to-use capacity of the provided size.
    ///
    /// The maximum valid value is `FLATBUFFERS_MAX_BUFFER_SIZE`.
    pub fn new_with_capacity(size: usize) -> Self {
        // we need to check the size here because we create the backing buffer
        // directly, bypassing the typical way of using grow_owned_buf:
        assert!(
            size <= FLATBUFFERS_MAX_BUFFER_SIZE,
            "cannot initialize buffer bigger than 2 gigabytes"
        );

        FlatBufferBuilder {
            storage: VecFlatBufferBuilderStorage {
                owned_buf: vec![0u8; size],
                field_locs: Vec::new(),
                written_vtable_revpos: Vec::new(),
            },
            head: size,

            nested: false,
            finished: false,

            min_align: 0,
            force_defaults: false,

            _phantom: PhantomData,
        }
    }

    /// Destroy the FlatBufferBuilder, returning its internal byte vector
    /// and the index into it that represents the start of valid data.
    pub fn collapse(self) -> (Vec<u8>, usize) {
        (self.storage.owned_buf, self.head)
    }
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl<'fbb> Default for FlatBufferBuilder<'fbb, VecFlatBufferBuilderStorage> {
    fn default() -> Self {
        Self::new_with_capacity(0)
    }
}
