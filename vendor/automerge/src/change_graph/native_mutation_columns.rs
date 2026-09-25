//! Actual retained-source graph-column mutation costs, separate from load.
use super::*;
use crate::resource_limits::NativeResourceError;
use crate::resource_limits::reconstruct::{Budget, product, sum};
use crate::resource_limits::retained::mutate::columns;

impl ChangeGraph {
    /// Borrowed scalar metadata for pre-mutation causal validation. This does
    /// not decode columns, copy messages/dependencies or populate clock caches.
    pub(crate) fn native_validation_metadata(
        &self,
        hash: &ChangeHash,
    ) -> Option<(usize, u64, u64)> {
        let index = self.hash_to_index(hash)?;
        Some((
            self.actors.get(index)?.0 as usize,
            u64::from(*self.seq.get(index)?),
            u64::from(*self.max_ops.get(index)?),
        ))
    }

    pub(crate) fn native_incoming_column_plan<'a, I>(
        &self,
        incoming: I,
        budget: &mut Budget,
    ) -> Result<(), NativeResourceError>
    where
        I: Iterator<Item = &'a Change> + Clone,
    {
        let mut count = 0;
        let mut messages = 0;
        for change in incoming {
            count = sum(count, 1)?;
            messages = sum(messages, change.message().map(String::len).unwrap_or(0))?;
        }
        columns::scalar(&self.num_ops, count, budget)?;
        columns::scalar(&self.timestamps, count, budget)?;
        columns::scalar(&self.extra_bytes_meta, count, budget)?;
        columns::string(&self.messages, count, messages, budget)?;
        // add_nodes supplies owning cloned Strings. flush_lit_run clones each
        // Cow into writer actions while the original literal run is still
        // alive, so two complete incoming message payloads can coexist.
        // Parsed original Change message owners keep their separate credit.
        budget.add(product(messages, 2)?)?;
        Ok(())
    }
}
