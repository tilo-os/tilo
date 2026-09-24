//! Source-bound canonical export. The legacy diagnostic bundle comparator is
//! deliberately absent; the dense builder and canonical finish are unchanged.
use super::*;
use crate::op_set2::types::ScalarValue;
use crate::resource_limits::{
    NativeAllowance, NativeReservation, NativeResourceError,
    reconstruct::{Budget, product, sum},
};

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

#[derive(Default)]
struct Dimensions {
    changes: usize,
    dense: usize,
    dependencies: usize,
    successors: usize,
    payload: usize,
    messages: usize,
    extra: usize,
    actors: usize,
    actor_bytes: usize,
}

impl Dimensions {
    fn plan(&self, budget: &mut Budget) -> Result {
        budget.growing_vec::<ChangeBuilder<'_>>(self.changes)?;
        budget.growing_vec_forest::<Option<OpBuilder<'_>>>(self.dense, self.changes)?;
        budget.growing_vec::<bool>(self.actors)?;
        budget.growing_vec::<Option<ActorIdx>>(self.actors)?;
        budget.growing_vec::<usize>(self.actors)?;
        budget.hash::<OpId, Vec<OpId>>(self.successors)?;
        // At most one predecessor vector per successor edge; removed vectors
        // move into builders. flush_deletes copies while the map still owns it.
        budget.growing_vec_forest::<OpId>(self.successors, self.successors)?;
        budget.growing_vec_forest::<OpId>(self.successors, self.successors)?;
        budget.growing_vec::<Change>(self.changes)?;
        budget.growing_vec_forest::<ChangeHash>(self.dependencies, self.changes)?;
        budget.growing_vec_forest::<ActorId>(product(self.changes, self.actors)?, self.changes)?;
        budget.add(product(self.changes, self.actor_bytes)?)?;
        budget.add(self.messages)?;
        // Owned builder key/value/name payload, including keys for synthetic
        // deletes. The current last key and encoder literal copy can coexist.
        budget.add(self.payload)?;
        budget.add(self.payload)?;
        budget.add(self.payload)?;
        let cells = sum(product(self.dense, 14)?, product(self.successors, 2)?)?;
        let columns = sum(product(cells, 20)?, self.payload)?;
        let prefixes = sum(
            sum(
                product(self.dependencies, size_of::<ChangeHash>())?,
                product(self.changes, 10 * (8 + 2 * 14))?,
            )?,
            sum(
                product(
                    self.changes,
                    sum(self.actor_bytes, product(self.actors, 10)?)?,
                )?,
                sum(self.messages, self.extra)?,
            )?,
        )?;
        let body = sum(columns, prefixes)?;
        // col_data, growing body, and final header+body overlap. Independent
        // per-change byte vectors retain their own RawVec minimum capacities.
        budget.growing_vec_forest::<u8>(columns, self.changes)?;
        budget.growing_vec_forest::<u8>(body, self.changes)?;
        budget.growing_vec_forest::<u8>(sum(body, product(self.changes, 20)?)?, self.changes)?;
        // Columns encode sequentially. The widest current RLE literal vector
        // is Cow<str>; numeric raw scalar encoding also creates a <=10 byte Vec.
        budget.growing_vec::<Cow<'_, str>>(self.dense.max(self.successors))?;
        budget.growing_vec::<u8>(10)?;
        budget.growing_vec::<u8>(11)?; // checksum hash type + u64 length prefix
        budget.growing_vec::<crate::storage::RawColumn<crate::storage::columns::compression::Uncompressed>>(14)?;
        Ok(())
    }
}

impl<'a> ChangeCollector<'a> {
    /// Only the guarded NativeOwned history adapter calls this private seam.
    /// The immutable source/heads are borrowed through both passes, so the
    /// dimensions cannot be exchanged for another source or a later revision.
    pub(crate) fn native_export_bounded(
        ops: &'a OpSet,
        graph: &'a ChangeGraph,
        heads: &[ChangeHash],
        allowance: &NativeAllowance,
    ) -> Result<(Vec<Change>, NativeReservation)> {
        let mut scratch = Budget::default();
        graph.native_history_selection_plan(heads.len(), &mut scratch)?;
        ops.native_history_preflight_plan(&mut scratch)?;
        let scratch_guard = allowance.reserve(scratch.bytes())?;
        // Declare this owner after its guard: every early return drops its
        // metadata/dependency allocations before releasing preflight custody.
        let changes = graph.get_build_metadata_clock(heads);
        if changes.is_empty() {
            return Ok((Vec::new(), allowance.reserve(0)?));
        }
        let min = usize::try_from(changes.iter().map(|c| c.start_op).min().unwrap_or(0))
            .map_err(|_| NativeResourceError::WorkBytes)?;
        let max = usize::try_from(changes.iter().map(|c| c.max_op).max().unwrap_or(0))
            .map_err(|_| NativeResourceError::WorkBytes)?
            .checked_add(1)
            .ok_or(NativeResourceError::WorkBytes)?;
        let mut dimensions = Dimensions {
            changes: changes.len(),
            actors: ops.actors.len(),
            ..Default::default()
        };
        for actor in &ops.actors {
            dimensions.actor_bytes = sum(dimensions.actor_bytes, actor.to_bytes().len())?;
        }
        for change in &changes {
            dimensions.dense = sum(dimensions.dense, change.num_ops())?;
            dimensions.dependencies = sum(dimensions.dependencies, change.deps.len())?;
            dimensions.messages = sum(dimensions.messages, change.message_str().len())?;
            dimensions.extra = sum(dimensions.extra, change.extra.len())?;
        }
        // Same indexed iterator as the real collector, including predecessor
        // operations reached through successor counters. No full-op scan.
        for op in ops.iter_ctr_range(min..max) {
            let edges = op.succ().len();
            dimensions.successors = sum(dimensions.successors, edges)?;
            let key = match &op.key {
                KeyRef::Map(value) => value.len(),
                _ => 0,
            };
            let value = match &op.value {
                ScalarValue::Bytes(v) | ScalarValue::Unknown { bytes: v, .. } => v.len(),
                ScalarValue::Str(v) => v.len(),
                _ => 0,
            };
            let name = op.mark_name.as_deref().map_or(0, str::len);
            // Every synthetic delete copies its register key. Counting once
            // per predecessor edge bounds duplicate keys without knowing the
            // later map grouping or allocating a preflight HashMap.
            dimensions.payload = sum(
                dimensions.payload,
                sum(sum(key, value)?, sum(name, product(key, edges)?)?)?,
            )?;
        }
        let mut final_budget = Budget::default();
        dimensions.plan(&mut final_budget)?;
        final_budget.add(
            ops.native_history_cursor_bytes()
                .ok_or(NativeResourceError::WorkBytes)?,
        )?;
        let peak = allowance.reserve(final_budget.bytes())?;
        let mut collector = Self::try_from_change_meta_dense(changes, &ops.actors)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        for op in ops.iter_ctr_range(min..max) {
            let id = op.id;
            let successors = op.succ();
            collector.process_op(op);
            for successor in successors {
                collector.process_succ(id, successor);
            }
        }
        let output = collector
            .finish(graph)
            .map_err(|_| NativeResourceError::Unaccounted)?;
        // finish consumed metadata/builders/maps and the indexed iterator has
        // ended. Only canonical owned Changes remain, covered by peak.
        drop(scratch_guard);
        Ok((output, peak))
    }
}
