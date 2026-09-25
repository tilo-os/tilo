//! Canonical change export without 0.12's debug-only cross-check.
//!
//! This is `ChangeCollector::exclude_hashes` (the body of
//! `Automerge::get_changes`) with its release-build behaviour on every build:
//! `from_build_meta` additionally re-encodes the selection as a bundle under
//! `debug_assertions`, an allocation no dimension bound should have to model.
//! Internal inconsistency fails closed instead of panicking.
use crate::change_graph::ChangeGraph;
use crate::op_set2::change::ChangeCollector;
use crate::op_set2::OpSet;
use crate::resource_limits::NativeResourceError;
use crate::{Change, ChangeHash};

pub(super) fn changes_since(
    ops: &OpSet,
    graph: &ChangeGraph,
    heads: &[ChangeHash],
) -> Result<Vec<Change>, NativeResourceError> {
    let changes = graph.get_build_metadata_clock(heads);
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let min = changes.iter().map(|c| c.start_op).min().unwrap_or(0);
    let max = changes.iter().map(|c| c.max_op).max().unwrap_or(0);
    let min = usize::try_from(min).map_err(|_| NativeResourceError::Unaccounted)?;
    let max = usize::try_from(max)
        .ok()
        .and_then(|max| max.checked_add(1))
        .ok_or(NativeResourceError::Unaccounted)?;
    let mut collector = ChangeCollector::try_from_change_meta(changes, &ops.actors)
        .map_err(|_| NativeResourceError::WorkBytes)?;
    for op in ops.iter_ctr_range(min..max) {
        let id = op.id;
        let successors = op.succ();
        collector.process_op(op);
        for successor in successors {
            collector.process_succ(id, successor);
        }
    }
    collector
        .finish(graph)
        .map_err(|_| NativeResourceError::Unaccounted)
}
