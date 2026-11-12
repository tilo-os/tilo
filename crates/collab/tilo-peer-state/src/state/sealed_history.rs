//! Stored-envelope validation for restore.
use super::backend::PeerCore;
use super::{DocumentAdmission, err, sealed_compaction, sealed_within_bounds};

pub(super) fn restore(
    core: &mut PeerCore,
    route: &super::Route,
    ops: Vec<collab_types::SignedOp>,
) -> Result<(), String> {
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
    if !document.encryption_required || !document.sealed.is_empty() {
        return Err("sealed history for a record that is not encrypted".into());
    }
    for op in &ops {
        collab_types::verify_op(op).map_err(err)?;
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
        if !collab_types::is_compaction_segment(op)
            && core.exact_admissions.get(&admitted.key) != Some(&admitted.operation_id)
        {
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
        document.restore_sealed(ops.into())?;
    }
    Ok(())
}
