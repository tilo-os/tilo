//! Borrowed dimensions of verified native Changes, before owning row decode.
use super::*;
use crate::resource_limits::NativeResourceError;

/// Private measurements, derived only from the actual verified Change owner.
#[derive(Debug, Default)]
pub(crate) struct NativeChangeDimensions {
    pub(crate) operations: usize,
    pub(crate) predecessors: usize,
    pub(crate) maximum_predecessors: usize,
    pub(crate) key_bytes: usize,
    pub(crate) value_bytes: usize,
    pub(crate) mark_name_bytes: usize,
    pub(crate) marks: usize,
}

impl Change {
    pub(crate) fn native_mutation_dimensions(
        &self,
    ) -> Result<NativeChangeDimensions, NativeResourceError> {
        let dimensions = self
            .stored
            .ops_meta
            .native_mutation_dimensions(self.stored.ops_data(), self.len)?;
        Ok(dimensions)
    }
}
