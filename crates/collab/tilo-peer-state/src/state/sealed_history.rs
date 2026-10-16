//! Shared stored-envelope validation for full and bounded restore.
use super::backend::{PeerCore, Storage};
use super::{DocumentAdmission, MAX_SEALED_BYTES, MAX_SEALED_OPS, err, sealed_compaction};

pub(super) fn restore<S: Storage>(
    core: &mut PeerCore<S>,
    route: &super::Route,
    ops: Vec<collab_types::SignedOp>,
) -> Result<(), String> {
    let held = ops.iter().try_fold(0usize, |held, op| {
        held.checked_add(op.payload.len())
            .filter(|held| *held <= MAX_SEALED_BYTES)
    });
    if ops.is_empty() || ops.len() > MAX_SEALED_OPS || held.is_none() {
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
            &core.room,
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
        let admitted =
            DocumentAdmission::from_op(&core.room, &route.app, &route.record, op).map_err(err)?;
        if S::exact_admission(&core.exact_admissions, &admitted.key) != Some(&admitted.operation_id)
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
        document.sealed = ops.into();
    }
    Ok(())
}
