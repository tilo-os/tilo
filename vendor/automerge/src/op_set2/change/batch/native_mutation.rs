//! Workspace for the real retained-source batch engine. This is one plan
//! contribution, not permission to execute the engine or a fresh-load shape.
use super::*;
use crate::resource_limits::NativeResourceError;
use crate::resource_limits::reconstruct::{Budget, product, sum};
use crate::resource_limits::retained::mutate::plan::IncomingDimensions;

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

impl Automerge {
    pub(crate) fn native_validate_incoming(
        &self,
        incoming: &[Change],
        allowance: Option<&crate::resource_limits::NativeAllowance>,
    ) -> std::result::Result<
        super::native_validation::NativeValidated,
        crate::resource_limits::NativeMutationError,
    > {
        super::native_validation::validate_incoming(self, incoming, allowance)
    }

    /// `incoming` includes every actual queued and supplied Change before
    /// deduplication. Source residency, graph/column growth, semantic validation
    /// and final adoption have separate required contributions.
    pub(crate) fn native_incoming_workspace_plan(
        &self,
        incoming: &IncomingDimensions,
        budget: &mut Budget,
    ) -> Result {
        let changes = incoming.changes;
        let ops = incoming.operations;
        let edges = incoming.predecessors;
        let dependencies = incoming.dependencies;

        // Pool, BatchApply and Option slots. Queued Change payloads move; the
        // parser and original retention continue to hold their own custody.
        budget.growing_vec::<Change>(changes)?;
        budget.growing_vec::<Change>(changes)?;
        budget.growing_vec::<Option<Change>>(changes)?;
        // Orphans are pushed one at a time into the queue's fresh vector.
        budget.growing_vec::<Change>(changes)?;
        // Queue hashes, pool seen and BatchApply hashes are distinct tables.
        for _ in 0..3 {
            budget.hash::<ChangeHash, ()>(changes)?;
        }
        // Each change contributes one author-sequence insertion. Each author
        // table can have at most changes independent nonempty inner sets.
        for _ in 0..2 {
            budget.hash::<ActorId, HashSet<u64>>(changes)?;
            // In pinned hashbrown, a one-u64-entry table has four buckets;
            // capacity_to_buckets(n) <= 4*n for every n>=1. Charging one such
            // table (including control bytes and growth overlap) per actual
            // sequence entry also covers any grouping into fewer larger sets.
            budget.hash_many::<u64, ()>(changes, 1)?;
            budget.add(incoming.actor_bytes)?;
        }
        budget.growing_vec::<u32>(changes)?; // unsatisfied
        budget.growing_vec::<bool>(changes)?; // consumed
        budget.growing_vec::<usize>(changes)?; // topo_order
        budget.growing_vec::<usize>(changes)?; // ready VecDeque
        budget.hash::<ChangeHash, Vec<usize>>(dependencies)?;
        budget.growing_vec_forest::<usize>(dependencies, dependencies)?;

        // The accumulated imports, a per-change import and stable-sort scratch
        // can coexist. The scratch copies slots, not their owning fields.
        budget.growing_vec::<ChangeOp>(ops)?;
        budget.growing_vec::<ChangeOp>(ops)?;
        budget.array::<ChangeOp>(ops)?;
        budget.growing_vec::<usize>(incoming.actors)?; // actor mapping
        budget.growing_vec::<ObjSpan>(ops)?;
        budget.hash::<OpId, Vec<(OpId, Option<i64>)>>(edges)?;
        budget.growing_vec_forest::<(OpId, Option<i64>)>(edges, edges)?;
        // Decoder predecessor IDs coexist with mapped IDs for the current op;
        // imported predecessors remain in each ChangeOp until BatchApply dies.
        budget.growing_vec_forest::<OpId>(edges, ops)?;
        budget.growing_vec::<crate::types::OpId>(incoming.maximum_predecessors)?;
        budget.growing_vec_forest::<(OpId, Option<i64>)>(edges, ops)?;

        budget.hash::<OpId, Vec<usize>>(ops)?; // entry
        budget.hash::<usize, Vec<usize>>(ops)?; // gosub
        budget.hash::<ElemId, Vec<usize>>(ops)?; // updates
        budget.hash::<ElemId, usize>(ops)?; // e_to_i
        for _ in 0..3 {
            budget.growing_vec_forest::<usize>(ops, ops)?;
        }
        budget.growing_vec::<usize>(ops)?; // stack
        budget.growing_vec::<usize>(ops)?; // updates_stack
        budget.growing_vec::<SuccInsert>(edges)?;
        budget.growing_vec::<Adjust>(sum(self.ops.len(), ops)?)?;
        // ChangeOp's trait successor methods box a concrete slice-map cursor.
        // These are distinct from allocation-free inherent Op::succ cursors.
        budget.add(ChangeOp::native_successor_cursor_bytes())?;

        // Imported Cow-owned keys, mark names and scalar bytes. The current
        // storage decoder and import mapping can overlap an owning field copy.
        let imported_payload = sum(
            sum(incoming.key_bytes, incoming.mark_name_bytes)?,
            incoming.value_bytes,
        )?;
        budget.add(imported_payload)?;
        budget.add(imported_payload)?;
        // Large SmolStr fields decode via Arc<str>: two atomics and at most one
        // alignment padding unit per key, name and string scalar.
        budget.array::<std::sync::atomic::AtomicUsize>(product(ops, 9)?)?;

        let (source_keys, source_values, source_names) =
            self.ops.native_incoming_source_payload()?;
        let values = sum(source_values, incoming.value_bytes)?;
        let names = sum(source_names, incoming.mark_name_bytes)?;
        let keys = sum(source_keys, incoming.key_bytes)?;
        // ValueState's doc/change owners and an overlapping assignment clone.
        for _ in 0..3 {
            budget.add(values)?;
        }
        budget.add(keys)?; // prop_static's owning map key

        // Two RichTextDiff machines retain state+current sets. An exported
        // MarkDiff set and mark_above/mark_below clones overlap those machines.
        // Six complete owner slots include those concrete simultaneous owners;
        // all source rows conservatively bound source mark cardinality.
        let marks = sum(self.ops.len(), incoming.marks)?;
        for _ in 0..6 {
            budget.growing_vec::<(OpId, MarkData<'static>)>(marks)?;
            budget.tree::<smol_str::SmolStr, crate::ScalarValue>(marks)?;
            budget.add(sum(names, values)?)?;
        }
        budget.arc::<crate::marks::MarkSet>(6)?;
        budget.array::<std::sync::atomic::AtomicUsize>(product(marks, 18)?)?;
        Ok(())
    }
}
