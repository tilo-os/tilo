//! Stored-envelope validation for restore.
use super::backend::PeerCore;
use super::{DocumentAdmission, err, sealed_compaction, sealed_within_bounds};

/// A record's stored sealed history that its checks refuse, handed back whole with why: the
/// restore sets it aside instead of reading it ([`super::quarantine`]).
pub(super) struct Refused {
    pub(super) reason: String,
    pub(super) history: Vec<collab_types::SignedOp>,
}

/// Check `ops`, `route`'s entry of the durable sealed histories, and install it as the record's
/// sealed history; refused, the entry comes back as it was, with why.
pub(super) fn restore(
    core: &mut PeerCore,
    route: &super::Route,
    mut ops: Vec<collab_types::SignedOp>,
) -> Result<(), Refused> {
    let leading = match check(core, route, &ops) {
        Ok(split) => split,
        Err(reason) => {
            return Err(Refused {
                reason,
                history: ops,
            });
        }
    };
    let history = ops.split_off(leading);
    if let Some(document) = core.documents.get_mut(route) {
        document
            .restore_sealed(history.into(), ops.into())
            .map_err(|reason| Refused {
                reason,
                history: Vec::new(),
            })?;
    }
    Ok(())
}

/// Every check [`restore`] makes of `ops`, which it reads as they are: where the history starts
/// after the writes the entry keeps beside it ([`check_beside`]).
fn check(
    core: &PeerCore,
    route: &super::Route,
    ops: &[collab_types::SignedOp],
) -> Result<usize, String> {
    // What the entry keeps beside the history leads it, ahead of the checkpoint leading the
    // history itself, which is the last checkpoint the entry holds: the writes that checkpoint
    // stands in for ([`super::backend::PeerDocument::checkpoint_writes`]).
    let leading = match ops.iter().rposition(super::checkpoints::is_checkpoint) {
        Some(leading) if leading > 0 => {
            let (beside, history) = ops.split_at(leading);
            check_beside(beside, history.first())?;
            leading
        }
        _ => 0,
    };
    let (checkpoint_writes, ops) = ops.split_at(leading);
    // Each write the checkpoint stands in for is kept once: a fold moves a write out of the
    // history beside it, never a second copy.
    let mut kept = std::collections::HashSet::new();
    if !checkpoint_writes
        .iter()
        .all(|op| kept.insert(op.signature.0))
    {
        return Err("sealed history keeps a write beside its checkpoint twice".into());
    }
    for original in checkpoint_writes {
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
    let (leading_set, writes) = ops.split_at(set);
    let held = |ops: &[collab_types::SignedOp]| {
        (
            ops.len(),
            ops.iter()
                .fold(0usize, |held, op| held.saturating_add(op.payload.len())),
        )
    };
    if ops.is_empty() || !sealed_within_bounds(held(leading_set), held(writes)) {
        return Err("sealed record history outside its bounds".into());
    }
    let document = core
        .documents
        .get(route)
        .ok_or("sealed history for a record this state does not hold")?;
    if !document.encryption_required || document.stores_sealed() {
        return Err("sealed history for a record that is not encrypted".into());
    }
    for op in ops {
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
        // A compaction is numbered as a write of its author it folds, and its
        // evidence is the mark that fold leaves rather than an entry.
        let exact = core.exact_admissions.get(&admitted.key) == Some(&admitted.operation_id);
        let folded = op.kind == collab_types::OpKind::Compaction
            && core.exact_admissions.covers(&admitted.key);
        if !collab_types::is_compaction_segment(op) && !exact && !folded {
            let held = match (
                core.exact_admissions.get(&admitted.key),
                core.exact_admissions.mark(&admitted.key),
            ) {
                (Some(_), _) => "another write's bytes",
                (None, Some(_)) => "a compaction's mark below it",
                (None, None) => "nothing",
            };
            return Err(format!(
                "sealed history lacks exact admission evidence: the {:?} write {} of author {} \
                 to {} {}, where the index holds {held}",
                op.kind,
                op.seq,
                hex::encode(&op.author.as_bytes()[..4]),
                route.app,
                route.record
            ));
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
    sealed_compaction::check_stored(ops, &document.channel.last_seq_per_author)?;
    Ok(leading)
}

/// Check `beside`, what an entry keeps ahead of its history, against `checkpoint`, which leads
/// the history.
fn check_beside(
    beside: &[collab_types::SignedOp],
    checkpoint: Option<&collab_types::SignedOp>,
) -> Result<(), String> {
    let wrapper = checkpoint
        .map(collab_types::SealedCompaction::parse)
        .transpose()
        .map_err(err)?;
    // Every write kept beside the checkpoint is one it stands in for: covered by it, or by a
    // checkpoint kept beside it that it stands in for in turn. A write after it in the record's
    // history is never one of them.
    let mut standing: Vec<collab_types::SealedCompaction> = wrapper.into_iter().collect();
    let mut taken = std::collections::HashSet::new();
    loop {
        let more: Vec<&collab_types::SignedOp> = beside
            .iter()
            .filter(|op| super::checkpoints::is_checkpoint(op) && !taken.contains(&op.signature.0))
            .filter(|op| standing.iter().any(|wrapper| wrapper.covers(op)))
            .collect();
        if more.is_empty() {
            break;
        }
        for op in more {
            taken.insert(op.signature.0);
            standing.push(collab_types::SealedCompaction::parse(op).map_err(err)?);
        }
    }
    if beside
        .iter()
        .any(|op| !standing.iter().any(|wrapper| wrapper.covers(op)))
    {
        return Err("a write kept beside the record's checkpoint is none it stands in for".into());
    }
    Ok(())
}
