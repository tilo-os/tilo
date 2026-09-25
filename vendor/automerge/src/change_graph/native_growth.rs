//! Incoming graph allocation plans. Retained-source column splices are planned
//! separately by native_mutation_columns; no whole-graph reconstruction factor.
use super::*;
use crate::ActorId;
use crate::resource_limits::{
    NativeResourceError,
    reconstruct::{Budget, Shape, product, sum},
};

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

impl ChangeGraphCols {
    /// The actual load-only owner used while verifying an incoming Document.
    /// It never finalizes a graph or populates any clock/hash cache.
    pub(crate) fn native_incoming_columns_plan(shape: &Shape, budget: &mut Budget) -> Result {
        budget.growing_vec::<Edge>(shape.edges)?;
        budget.growing_vec::<ActorIdx>(shape.changes)?;
        budget.growing_vec::<Option<EdgeIdx>>(shape.changes)?;
        budget.growing_vec::<u32>(shape.changes)?;
        budget.growing_vec::<u32>(shape.changes)?;
        budget.growing_vec::<u64>(shape.changes)?;
        budget.growing_vec::<Vec<NodeIdx>>(shape.actors)?;
        budget.growing_vec_forest::<NodeIdx>(shape.changes, shape.actors)?;
        budget.tree::<ChangeHash, ()>(shape.heads)?;
        let rows = product(shape.changes, 4)?;
        budget.columns::<StrCursor>(
            product(shape.documents, 4)?,
            rows,
            sum(product(rows, 20)?, sum(shape.values, shape.column_bytes)?)?,
        )?;
        budget.growing_vec::<u8>(sum(shape.raw_column_bytes, shape.extra_bytes)?)?;
        Ok(())
    }
}

// Existing residency remains charged separately. If the current capacity can
// hold the final length, no replacement is required. Otherwise final>capacity,
// so pinned RawVec growth and any old/replacement overlap are bounded by the
// standard growing-vector proof using that final length. This explicitly tests
// actual retained capacities instead of assuming length equals capacity.
fn vector_growth<T>(value: &Vec<T>, extra: usize, budget: &mut Budget) -> Result {
    let final_len = sum(value.len(), extra)?;
    if final_len > value.capacity() {
        budget.growing_vec::<T>(final_len)?;
    }
    Ok(())
}

impl ChangeGraph {
    /// Pure noncolumn plan over actual queued+incoming Changes. Deduplication
    /// may later reduce them, but no caller-supplied numeric shape is trusted.
    /// The caller must additionally reserve the four graph-column splice plan.
    pub(crate) fn native_incoming_noncolumn_plan<'a, I>(
        &self,
        source_actors: &[ActorId],
        incoming: I,
        budget: &mut Budget,
    ) -> Result
    where
        I: Iterator<Item = &'a Change> + Clone,
    {
        if source_actors.len() != self.seq_index.len() {
            return Err(NativeResourceError::Unaccounted);
        }
        let mut added = 0;
        let mut edges = 0;
        let mut actor_entries = 0;
        let mut extra = 0;
        for change in incoming.clone() {
            let maximum = change
                .start_op()
                .get()
                .checked_add(
                    u64::try_from(change.len()).map_err(|_| NativeResourceError::WorkBytes)?,
                )
                .and_then(|end| end.checked_sub(1))
                .ok_or(NativeResourceError::WorkBytes)?;
            if change.seq() == 0 {
                return Err(NativeResourceError::Invalid);
            }
            if change.seq() > u32::MAX as u64 || maximum > u32::MAX as u64 {
                return Err(NativeResourceError::WorkBytes);
            }
            added = sum(added, 1)?;
            edges = sum(edges, change.deps().len())?;
            actor_entries = sum(actor_entries, change.actors().count())?;
            extra = sum(extra, change.extra_bytes().len())?;
        }
        let nodes = sum(self.hashes.len(), added)?;
        let actors = sum(self.seq_index.len(), actor_entries)?;
        if nodes > u32::MAX as usize
            || actors > u32::MAX as usize
            || sum(self.edges.len(), edges)? >= u32::MAX as usize
        {
            return Err(NativeResourceError::WorkBytes);
        }
        vector_growth(&self.edges, edges, budget)?;
        vector_growth(&self.hashes, added, budget)?;
        vector_growth(&self.actors, added, budget)?;
        vector_growth(&self.parents, added, budget)?;
        vector_growth(&self.seq, added, budget)?;
        vector_growth(&self.max_ops, added, budget)?;
        vector_growth(&self.extra_bytes_raw, extra, budget)?;
        vector_growth(&self.seq_index, actor_entries, budget)?;
        // Source actors have real independent capacities. Actor insertion may
        // shift their indexes, but their identity/sequence vectors stay intact.
        let mut new_sequences = 0;
        for (actor, index) in source_actors.iter().zip(&self.seq_index) {
            let count = incoming.clone().filter(|c| c.actor_id() == actor).count();
            vector_growth(index, count, budget)?;
        }
        for change in incoming.clone() {
            if !source_actors.iter().any(|actor| actor == change.actor_id()) {
                new_sequences = sum(new_sequences, 1)?;
            }
        }
        budget.growing_vec_forest::<NodeIdx>(new_sequences, actor_entries)?;
        // Insert-only tables: current reported capacity is their actual
        // high-water; tombstone shrink cannot conceal a larger backing.
        if nodes > self.nodes_by_hash.capacity() {
            budget.hash::<ChangeHash, NodeIdx>(nodes)?;
        }
        if nodes > self.clock_cache.capacity() {
            budget.hash::<NodeIdx, SeqClock>(nodes)?;
        }
        budget.tree::<ChangeHash, ()>(sum(self.heads.len(), added)?)?;
        // Every live cached clock can be rewritten by inserted actors before
        // apply starts populating cache entries. Account its actual capacity.
        for clock in self.clock_cache.values() {
            vector_growth(&clock.0, actor_entries, budget)?;
        }
        if added != 0 {
            // Any graph node may be cached, including nonperiodic frontiers.
            // Each recursive descent follows >=33 strictly earlier indexes.
            let frames = if nodes == 0 {
                0
            } else {
                (nodes - 1) / (CACHE_STEP as usize * 2 + 1) + 1
            };
            let clocks = sum(nodes, sum(frames, 1)?)?;
            budget.growing_vec_forest::<Option<NonZeroU32>>(product(clocks, actors)?, clocks)?;
            budget.trees::<NodeIdx, ()>(sum(frames, 1)?, nodes)?;
        }
        Ok(())
    }
}
