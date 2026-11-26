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
    let (leading, originals, at) = match check(core, route, &ops) {
        Ok(split) => split,
        Err(reason) => {
            return Err(Refused {
                reason,
                history: ops,
            });
        }
    };
    let history = ops.split_off(leading);
    let checkpoint_writes = ops.get(at..).unwrap_or_default().to_vec();
    ops.truncate(originals);
    let mut seen = std::collections::HashSet::new();
    ops.retain(|op| seen.insert(op.signature.0));
    if let Some(document) = core.documents.get_mut(route) {
        document
            .restore_sealed(history.into(), ops.into(), checkpoint_writes.into())
            .map_err(|reason| Refused {
                reason,
                history: Vec::new(),
            })?;
    }
    Ok(())
}

/// Every check [`restore`] makes of `ops`, which it reads as they are: where the history starts
/// after what the entry keeps beside it, and how what is beside it parts ([`split_beside`]).
fn check(
    core: &PeerCore,
    route: &super::Route,
    ops: &[collab_types::SignedOp],
) -> Result<(usize, usize, usize), String> {
    let before_reader_sets =
        |op: &collab_types::SignedOp| route.sealed_before_reader_sets(&core.workspace, op);
    // What the entry keeps beside the history leads it, ahead of the checkpoint leading the
    // history itself, which is the last checkpoint the entry holds: the originals of writes sealed
    // before reader sets whose content a device with the record's keys sealed again
    // ([`super::backend::PeerDocument::fold_sealed`]), then the writes that checkpoint stands in
    // for ([`super::backend::PeerDocument::checkpoint_writes`]). Writes sealed before reader sets
    // that no checkpoint follows are history still to be sealed again, and stay in it.
    let (leading, originals, at) = match ops.iter().rposition(super::checkpoints::is_checkpoint) {
        Some(leading) if leading > 0 => {
            let (beside, history) = ops.split_at(leading);
            let (originals, at) = split_beside(beside, history.first(), &before_reader_sets)?;
            (leading, originals, at)
        }
        _ => (0, 0, 0),
    };
    let (beside, ops) = ops.split_at(leading);
    let resealed = beside.get(..originals).unwrap_or_default();
    let checkpoint_writes = beside.get(at..).unwrap_or_default();
    // Each write the checkpoint stands in for is kept once: a fold moves a write out of the
    // history beside it, never a second copy.
    let mut kept = std::collections::HashSet::new();
    if !beside
        .get(at.max(originals)..)
        .unwrap_or_default()
        .iter()
        .all(|op| kept.insert(op.signature.0))
    {
        return Err("sealed history keeps a write beside its checkpoint twice".into());
    }
    for original in resealed.iter().chain(checkpoint_writes) {
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
    for (at, op) in ops.iter().enumerate() {
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
        // A write its author's mark covers was folded into that mark: its evidence is the mark,
        // not an entry. A compaction is numbered as a write of its author it folds, and earlier
        // builds marked an author up to a compaction's own number while a write of theirs below
        // it, which the compaction did not fold, stayed in the history.
        let exact = core.exact_admissions.get(&admitted.key) == Some(&admitted.operation_id);
        let folded = core.exact_admissions.get(&admitted.key).is_none()
            && core.exact_admissions.covers(&admitted.key);
        if !collab_types::is_compaction_segment(op) && !exact && !folded {
            let held = match (
                core.exact_admissions.get(&admitted.key),
                core.exact_admissions.mark(&admitted.key),
            ) {
                (Some(_), _) => "another write's bytes".to_owned(),
                (None, Some(mark)) => format!("its author's compaction mark at {mark}"),
                (None, None) => "nothing".to_owned(),
            };
            return Err(format!(
                "sealed history lacks exact admission evidence: the {:?} write {} of author {} \
                 to {} {}, {} of {} in the history after {} kept beside it{}, where the index \
                 holds {held}",
                op.kind,
                op.seq,
                hex::encode(&op.author.as_bytes()[..4]),
                route.app,
                route.record,
                at.saturating_add(1),
                ops.len(),
                beside.len(),
                if route.sealed_before_reader_sets(&core.workspace, op) {
                    ", sealed before reader sets"
                } else {
                    ""
                },
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
    Ok((leading, originals, at))
}

/// `beside`, what an entry keeps ahead of its history, parted into the originals of writes sealed
/// before reader sets and the writes `checkpoint`, which leads the history, stands in for: how
/// many originals lead it, and where those writes start. The originals come first, each sealed
/// before reader sets; where the writes a checkpoint stands in for start with such writes too, the
/// tail that holds exactly what the checkpoint names is them. An original the checkpoint stands in
/// for is kept both ways: as the original it is, and beside the checkpoint until every device
/// with the record's keys checked it.
fn split_beside(
    beside: &[collab_types::SignedOp],
    checkpoint: Option<&collab_types::SignedOp>,
    before_reader_sets: &dyn Fn(&collab_types::SignedOp) -> bool,
) -> Result<(usize, usize), String> {
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
        .find(|at| beside.get(*at..).is_some_and(&names_exactly))
        .unwrap_or(originals);
    // Every write kept beside the checkpoint is one it stands in for: covered by it, or by a
    // checkpoint kept beside it that it stands in for in turn. A write after it in the record's
    // history is never one of them.
    let writes = beside.get(at..).unwrap_or_default();
    let mut standing: Vec<collab_types::SealedCompaction> = wrapper.into_iter().collect();
    let mut taken = std::collections::HashSet::new();
    loop {
        let more: Vec<&collab_types::SignedOp> = writes
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
    if writes
        .iter()
        .any(|op| !standing.iter().any(|wrapper| wrapper.covers(op)))
    {
        return Err("a write kept beside the record's checkpoint is none it stands in for".into());
    }
    Ok((originals, at))
}
