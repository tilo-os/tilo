//! Stored-envelope validation for restore.
use super::backend::PeerCore;
use super::{DocumentAdmission, err, sealed_compaction, sealed_within_bounds};

pub(super) fn restore(
    core: &mut PeerCore,
    route: &super::Route,
    mut ops: Vec<collab_types::SignedOp>,
) -> Result<(), String> {
    let before_reader_sets =
        |op: &collab_types::SignedOp| route.sealed_before_reader_sets(&core.workspace, op);
    // What the entry keeps beside the history leads it, ahead of the checkpoint leading the
    // history itself, which is the last checkpoint the entry holds: the originals of writes sealed
    // before reader sets whose content a device with the record's keys sealed again
    // ([`super::backend::PeerDocument::fold_sealed`]), then the writes that checkpoint stands in
    // for ([`super::backend::PeerDocument::checkpoint_writes`]). Writes sealed before reader sets
    // that no checkpoint follows are history still to be sealed again, and stay in it.
    let (resealed, checkpoint_writes) =
        match ops.iter().rposition(super::checkpoints::is_checkpoint) {
            Some(leading) if leading > 0 => {
                let beside: Vec<collab_types::SignedOp> = ops.drain(..leading).collect();
                split_beside(beside, ops.first(), &before_reader_sets)?
            }
            _ => (Vec::new(), Vec::new()),
        };
    for original in resealed.iter().chain(&checkpoint_writes) {
        collab_types::verify_op(original).map_err(err)?;
        collab_types::validate_whole_document_payload_route(
            &route.app,
            &core.workspace,
            &route.record,
            original,
        )
        .map_err(err)?;
    }
    // The compaction set leads the history; the writes after it have their own bound.
    let set = ops
        .iter()
        .take_while(|op| op.kind == collab_types::OpKind::Compaction)
        .count();
    let (leading, writes) = ops.split_at(set);
    let held = |ops: &[collab_types::SignedOp]| {
        (
            ops.len(),
            ops.iter()
                .fold(0usize, |held, op| held.saturating_add(op.payload.len())),
        )
    };
    if ops.is_empty() || !sealed_within_bounds(held(leading), held(writes)) {
        return Err("sealed record history outside its bounds".into());
    }
    let document = core
        .documents
        .get(route)
        .ok_or("sealed history for a record this state does not hold")?;
    if !document.encryption_required || document.keeps_sealed() {
        return Err("sealed history for a record that is not encrypted".into());
    }
    for op in &ops {
        collab_types::verify_op(op).map_err(err)?;
        // A write sealed before reader sets is history as its author signed it, under the
        // artifact its document had then: a key holder opens it as it is, and seals its content
        // again under the document's own scope.
        let payload = collab_types::validate_whole_document_payload_route(
            &route.app,
            &core.workspace,
            &route.record,
            op,
        )
        .map_err(err)?;
        if !matches!(
            payload,
            collab_types::WholeDocumentPayload::Encrypted { .. }
        ) {
            return Err("sealed history holds a cleartext write".into());
        }
        // A compaction's segment is admitted as part of it: the compaction
        // names it ([`sealed_compaction::check_stored`]).
        let admitted = DocumentAdmission::from_op(&core.workspace, &route.app, &route.record, op)
            .map_err(err)?;
        // A compaction is numbered as a write of its author it folds, and its
        // evidence is the mark that fold leaves rather than an entry.
        let exact = core.exact_admissions.get(&admitted.key) == Some(&admitted.operation_id);
        let folded = op.kind == collab_types::OpKind::Compaction
            && core.exact_admissions.covers(&admitted.key);
        if !collab_types::is_compaction_segment(op) && !exact && !folded {
            return Err("sealed history lacks exact admission evidence".into());
        }
        if document
            .channel
            .last_seq_per_author
            .get(&op.author)
            .is_none_or(|floor| *floor < op.seq)
        {
            return Err("sealed history is ahead of the record's watermark".into());
        }
    }
    sealed_compaction::check_stored(&ops, &document.channel.last_seq_per_author)?;
    if let Some(document) = core.documents.get_mut(route) {
        document.restore_sealed(ops.into(), resealed.into(), checkpoint_writes.into())?;
    }
    Ok(())
}

/// `beside`, what an entry keeps ahead of its history, parted into the originals of writes sealed
/// before reader sets and the writes `checkpoint`, which leads the history, stands in for. The
/// originals come first, each sealed before reader sets; where the writes a checkpoint stands in
/// for start with such writes too, the tail that holds exactly what the checkpoint names is
/// them.
fn split_beside(
    mut beside: Vec<collab_types::SignedOp>,
    checkpoint: Option<&collab_types::SignedOp>,
    before_reader_sets: &dyn Fn(&collab_types::SignedOp) -> bool,
) -> Result<(Vec<collab_types::SignedOp>, Vec<collab_types::SignedOp>), String> {
    let originals = beside
        .iter()
        .take_while(|op| before_reader_sets(op))
        .count();
    let wrapper = checkpoint
        .map(collab_types::SealedCompaction::parse)
        .transpose()
        .map_err(err)?;
    let names_exactly = |writes: &[collab_types::SignedOp]| {
        wrapper.as_ref().is_some_and(|wrapper| {
            let named: Vec<&collab_types::SignedOp> =
                writes.iter().filter(|op| wrapper.covers(op)).collect();
            !writes.is_empty()
                && collab_types::hash_sealed_prefix(named).ok() == Some(wrapper.prefix_hash)
        })
    };
    let at = std::iter::once(originals)
        .chain((0..originals).rev())
        .find(|at| {
            beside
                .get(*at..)
                .is_some_and(|writes| names_exactly(writes))
        })
        .unwrap_or(originals);
    // An original the checkpoint stands in for is kept both ways: as the original it is, and
    // beside the checkpoint until every device with the record's keys checked it.
    let writes = beside.get(at..).unwrap_or_default().to_vec();
    beside.truncate(originals);
    let mut seen = std::collections::HashSet::new();
    beside.retain(|op| seen.insert(op.signature.0));
    Ok((beside, writes))
}
