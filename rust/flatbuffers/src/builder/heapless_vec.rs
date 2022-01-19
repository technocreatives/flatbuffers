use crate::{
    builder::{FieldLoc, FlatBufferBuilder},
    FlatBufferBuilderStorage, UOffsetT,
};
use core::marker::PhantomData;

use heapless::Vec;

pub struct HeaplessFlatBufferBuilderStorage<const B: usize, const F: usize, const V: usize> {
    owned_buf: heapless::Vec<u8, B>,
    field_locs: heapless::Vec<FieldLoc, F>,
    written_vtable_revpos: heapless::Vec<UOffsetT, V>,
}

impl<const B: usize, const F: usize, const V: usize> FlatBufferBuilderStorage
    for HeaplessFlatBufferBuilderStorage<B, F, V>
{
    fn bufs(&mut self) -> (&mut [u8], &mut [FieldLoc], &mut [UOffsetT]) {
        (
            self.owned_buf.as_mut(),
            self.field_locs.as_mut(),
            self.written_vtable_revpos.as_mut(),
        )
    }

    fn resize(&mut self, size: usize) {
        self.owned_buf.resize(size, 0).unwrap()
    }

    fn reset_field_locs(&mut self) {
        self.field_locs.clear();
    }

    fn reset_written_vtable_revpos(&mut self) {
        self.written_vtable_revpos.clear();
    }

    fn push_field_loc(&mut self, item: FieldLoc) {
        self.field_locs.push(item).unwrap()
    }

    fn push_written_vtable_revpos(&mut self, item: UOffsetT) {
        self.written_vtable_revpos.push(item).unwrap()
    }

    fn buffer_mut(&mut self) -> &mut [u8] {
        self.owned_buf.as_mut()
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

impl<'fbb, const B: usize, const F: usize, const V: usize>
    FlatBufferBuilder<'fbb, HeaplessFlatBufferBuilderStorage<B, F, V>>
{
    /// Create a FlatBufferBuilder that is ready for writing.
    pub fn new() -> Self {
        FlatBufferBuilder {
            storage: HeaplessFlatBufferBuilderStorage {
                owned_buf: Vec::new(),
                field_locs: Vec::new(),
                written_vtable_revpos: Vec::new(),
            },
            head: 0,

            nested: false,
            finished: false,

            min_align: 0,
            force_defaults: false,

            _phantom: PhantomData,
        }
    }

    /// Destroy the FlatBufferBuilder, returning its internal byte vector
    /// and the index into it that represents the start of valid data.
    pub fn collapse(self) -> (heapless::Vec<u8, B>, usize) {
        (self.storage.owned_buf, self.head)
    }
}

impl<'fbb, const B: usize, const F: usize, const V: usize> Default
    for FlatBufferBuilder<'fbb, HeaplessFlatBufferBuilderStorage<B, F, V>>
{
    fn default() -> Self {
        Self::new()
    }
}
