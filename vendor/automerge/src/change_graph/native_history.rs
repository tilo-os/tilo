//! Allocation layout for legacy history selection, without reconstructing clocks.
use super::*;
use crate::resource_limits::{
    NativeResourceError,
    reconstruct::{Budget, product, sum},
};

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

impl ChangeGraph {
    /// All scans here borrow inline SlabWeight tree cursors and StrCursor runs.
    /// Packable<str>::unpack returns a slice of its slab; no search or owned
    /// metadata is constructed before the caller reserves this budget.
    pub(crate) fn native_history_selection_plan(
        &self,
        heads: usize,
        budget: &mut Budget,
    ) -> Result {
        let nodes = self.hashes.len();
        // heads_to_nodes preserves repeated known heads before BTreeSet dedup.
        budget.growing_vec::<NodeIdx>(heads)?;
        budget.growing_vec::<Option<NonZeroU32>>(self.num_actors())?;
        // calculate_clock owns both to_visit and visited. Unlike cache_clock,
        // this immutable selection uses an unlimited cutoff and never recurses.
        budget.trees::<NodeIdx, ()>(2, nodes)?;
        budget.growing_vec::<NodeIdx>(nodes)?;
        budget.growing_vec::<BuildChangeMetadata<'_>>(nodes)?;
        budget.growing_vec_forest::<u64>(self.edges.len(), nodes)?;
        let mut messages = 0;
        for slab in self.messages.slabs.iter() {
            for run in slab.run_iter::<StrCursor>() {
                messages = sum(
                    messages,
                    product(run.count, run.value.as_deref().map_or(0, str::len))?,
                )?;
            }
        }
        budget.add(messages)?;
        // get_build_metadata_for_indexes performs these POD searches serially.
        fn slabs<C: ColumnCursor<SlabIndex = hexane::SlabWeight>>(column: &ColumnData<C>) -> usize {
            column.slabs.len()
        }
        let largest = [
            slabs(&self.num_ops),
            slabs(&self.timestamps),
            slabs(&self.messages),
            slabs(&self.extra_bytes_meta),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        budget.add(
            hexane::SlabTree::<hexane::SlabWeight>::search_stack_allocation_bound(largest)
                .ok_or(NativeResourceError::WorkBytes)?,
        )
    }
}
