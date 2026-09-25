//! Concrete indexed history cursor workspace.
use super::*;
use crate::resource_limits::{NativeResourceError, reconstruct::Budget};

impl OpSet {
    /// iter_ctr_range keeps both boxed find_by_range streams and their search
    /// stacks alive while OpIter/SuccWalker perform another POD column seek.
    pub(crate) fn native_history_cursor_bytes(&self) -> Option<usize> {
        CtrWalker::native_allocation_bytes(&self.cols.id_ctr)?
            .checked_add(CtrWalker::native_allocation_bytes(&self.cols.succ_ctr)?)?
            .checked_add(self.native_read_search_bytes()?)
    }

    pub(crate) fn native_history_preflight_plan(
        &self,
        budget: &mut Budget,
    ) -> Result<(), NativeResourceError> {
        budget.add(
            self.native_history_cursor_bytes()
                .ok_or(NativeResourceError::WorkBytes)?,
        )?;
        // Current decoded fields are slab loans in the pinned reader. Reserve
        // the actual source payload extents as well, covering any owned current
        // field and its run cursor during the counting pass (never N*raw size).
        for bytes in [
            self.cols.key_str.byte_len(),
            self.cols.mark_name.byte_len(),
            self.cols.value.byte_len(),
        ] {
            budget.growing_vec::<u8>(bytes)?;
            budget.growing_vec::<u8>(bytes)?;
        }
        Ok(())
    }
}
