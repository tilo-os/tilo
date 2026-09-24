//! Retained-source OpSet storage growth, independent of fresh reconstruction.
use super::*;
use crate::resource_limits::NativeResourceError;
use crate::resource_limits::reconstruct::{Budget, sum};
use crate::resource_limits::retained::mutate::{columns, plan::IncomingDimensions};

impl OpSet {
    /// Allocation-free source extents for the owning value/mark machines used
    /// by BatchApply. All source operations can be visited by an object walk;
    /// source.len() therefore bounds its simultaneously retained mark rows.
    pub(crate) fn native_incoming_source_payload(
        &self,
    ) -> Result<(usize, usize, usize), NativeResourceError> {
        Ok((
            columns::expanded_strings(&self.cols.key_str)?,
            self.cols.value.len(),
            columns::expanded_strings(&self.cols.mark_name)?,
        ))
    }

    pub(crate) fn native_incoming_storage_plan(
        &self,
        incoming: &IncomingDimensions,
        budget: &mut Budget,
    ) -> Result<(), NativeResourceError> {
        let rows = incoming.operations;
        let edges = incoming.predecessors;
        columns::scalar(&self.cols.id_actor, rows, budget)?;
        columns::scalar(&self.cols.id_ctr, rows, budget)?;
        columns::scalar(&self.cols.obj_actor, rows, budget)?;
        columns::scalar(&self.cols.obj_ctr, rows, budget)?;
        columns::scalar(&self.cols.key_actor, rows, budget)?;
        columns::scalar(&self.cols.key_ctr, rows, budget)?;
        columns::string(&self.cols.key_str, rows, incoming.key_bytes, budget)?;
        columns::scalar(&self.cols.insert, rows, budget)?;
        columns::scalar(&self.cols.action, rows, budget)?;
        columns::scalar(&self.cols.expand, rows, budget)?;
        columns::string(&self.cols.mark_name, rows, incoming.mark_name_bytes, budget)?;
        columns::scalar(&self.cols.value_meta, rows, budget)?;
        // RawCursor copies bytes directly; it has no twenty-byte scalar/RLE
        // representation per byte. Its logical event is one raw byte.
        columns::raw_insert(&self.cols.value, rows, incoming.value_bytes, budget)?;
        columns::scalar(&self.cols.succ_count, sum(rows, edges)?, budget)?;
        columns::scalar(&self.cols.succ_actor, edges, budget)?;
        columns::scalar(&self.cols.succ_ctr, edges, budget)?;
        columns::scalar(&self.cols.index.inc, edges, budget)?;
        columns::scalar(&self.cols.index.text, sum(rows, edges)?, budget)?;
        columns::scalar(&self.cols.index.visible, sum(rows, edges)?, budget)?;
        // In addition to inserted rows and successor visibility changes, the
        // object walkers can adjust one top flag for each source/imported row.
        let top_updates = sum(sum(rows, edges)?, sum(self.len(), rows)?)?;
        columns::scalar(&self.cols.index.top, top_updates, budget)?;
        self.cols.index.mark.native_mutation_plan(
            rows,
            incoming.marks,
            sum(incoming.mark_name_bytes, incoming.value_bytes)?,
            budget,
        )?;

        let actor_count = sum(self.actors.len(), incoming.actors)?;
        budget.growing_vec::<ActorId>(actor_count.max(self.actors.capacity()))?;
        let actor_bytes = self
            .actors
            .iter()
            .try_fold(incoming.actor_bytes, |bytes, actor| {
                sum(bytes, actor.to_bytes().len())
            })?;
        budget.add(actor_bytes)?;
        // BatchApply clones ObjIndex before ordering, and actor insertion can
        // independently replace it. Preserve actual old table capacity.
        let objects = sum(self.obj_info.0.len(), rows)?.max(self.obj_info.0.capacity());
        budget.hash::<types::OpId, ObjInfo>(objects)?;
        budget.hash::<types::OpId, ObjInfo>(objects)?;
        // Inactive PatchLog still migrates/retains owning actor copies.
        budget.growing_vec::<ActorId>(actor_count)?;
        budget.add(actor_bytes)?;
        // Columns::splice materializes the two successor scalar vectors.
        budget.growing_vec::<ActorIdx>(edges)?;
        budget.growing_vec::<i64>(edges)?;
        budget.add(
            self.native_read_search_bytes()
                .ok_or(NativeResourceError::WorkBytes)?,
        )?;
        Ok(())
    }
}
