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
    // The originals of writes sealed before reader sets whose content a key holder sealed again
    // lead the entry, ahead of the compaction that took their place
    // ([`super::backend::PeerDocument::fold_sealed`]). Writes sealed then that no compaction of
    // today's follows are history still to be sealed again, and stay in it.
    let originals = ops.iter().take_while(|op| before_reader_sets(op)).count();
    let resealed: Vec<collab_types::SignedOp> = match ops.get(originals) {
        Some(next)
            if originals > 0
                && next.kind == collab_types::OpKind::Compaction
                && !before_reader_sets(next) =>
        {
            ops.drain(..originals).collect()
        }
        _ => Vec::new(),
    };
    for original in &resealed {
        collab_types::verify_op(original).map_err(err)?;
        route
            .validate_sealed_before_reader_sets(&core.workspace, original)
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
        let payload = if before_reader_sets(op) {
            route.validate_sealed_before_reader_sets(&core.workspace, op)
        } else {
            collab_types::validate_whole_document_payload_route(
                &route.app,
                &core.workspace,
                &route.record,
                op,
            )
        }
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
        document.restore_sealed(ops.into(), resealed.into())?;
    }
    Ok(())
}
