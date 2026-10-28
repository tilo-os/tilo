//! A document and the history it keeps, below the toolkit: the primitive
//! that writes a past state forward as a new change, which undo, redo and
//! restore all write through.
//!
//! The toolkit (`ui::state`) builds its `State` on this and keeps its paths.

use std::collections::HashMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, PatchAction, ReadDoc, Value};
use tilo_sort::CompactSort as _;

mod built_on;
mod sequence_replay;
use sequence_replay::SequenceReplay;

/// Write the delta that leads from `from_heads` to `to_heads` into `doc`'s open
/// transaction, without committing.
///
/// The one primitive behind both undo and restore-from-history: it re-derives
/// exactly the properties and elements that differ and writes them forward,
/// rather than replacing the document with an old state and dropping every
/// concurrently-merged remote op.
/// A container the delta would remove that somebody has written inside since
/// `from_heads` — the paragraph a collaborator typed into after the action
/// that created it — is left standing, so undo takes back only the action.
///
/// The caller commits. That is deliberate: the owner's action records the
/// restore, so it lands on the undo stack with the right author and timestamp
/// exactly like any other edit. The toolkit's `State::restore_to_heads` is the
/// equivalent for a host that owns a whole `State`.
pub fn write_revert_patches(
    doc: &mut AutoCommit,
    from_heads: &[ChangeHash],
    to_heads: &[ChangeHash],
) {
    // Patches describing how to get from `from_heads` back to `to_heads`.
    let patches = doc.diff(from_heads, to_heads);
    // A container somebody wrote inside since `from_heads` stays standing.
    let patches = built_on::hold_back_built_on(doc, patches, from_heads);
    write_selected_patches(doc, patches, to_heads, from_heads);
}

/// [`write_revert_patches`] for only the patches `keep` accepts: a restore
/// of some items of a past version, or of everything but bookkeeping. The
/// rest stay in the patch stream unwritten, so the indices of later
/// sequence patches still line up. Returns how many patches were written.
pub fn write_revert_patches_where(
    doc: &mut AutoCommit,
    from_heads: &[ChangeHash],
    to_heads: &[ChangeHash],
    keep: &dyn Fn(&automerge::Patch) -> bool,
) -> usize {
    let patches = doc.diff(from_heads, to_heads);
    let patches: Vec<(automerge::Patch, bool)> =
        built_on::hold_back_built_on(doc, patches, from_heads)
            .into_iter()
            .map(|(patch, write)| {
                let write = write && keep(&patch) && !already_holds(doc, &patch);
                (patch, write)
            })
            .collect();
    let written = patches.iter().filter(|(_, write)| *write).count();
    if written > 0 {
        write_selected_patches(doc, patches, to_heads, from_heads);
    }
    written
}

/// Whether `doc` already holds the scalar `patch` would put: a diff is of
/// operations, so a value restored once differs from the version by its
/// operation while reading the same.
fn already_holds(doc: &AutoCommit, patch: &automerge::Patch) -> bool {
    use automerge::ReadDoc as _;
    let automerge::PatchAction::PutMap { key, value, .. } = &patch.action else {
        return false;
    };
    let automerge::Value::Scalar(wanted) = &value.0 else {
        return false;
    };
    matches!(
        doc.get(&patch.obj, key.as_str()),
        Ok(Some((automerge::Value::Scalar(current), _))) if current == *wanted
    )
}

/// Write an already-derived patch set forward into `doc`'s open transaction.
///
/// The half of [`write_revert_patches`] after the diff, split out so a caller
/// that already derived a full patch stream can reuse the index translation.
/// Filtered history uses `write_selected_patches` to retain skipped structural
/// steps in the logical index stream without changing their live content.
///
/// The two anchors bracket the change being inverted. `index_anchor` is the
/// state the patches start from: the first patch into a sequence indexes it as
/// of then, and each later one as the patches before it left it.
/// `insert_anchor` is the state they lead to, whose formatting a recreated
/// text object takes back. Passing them the wrong way round moves edits to
/// the wrong elements, so both are named rather than positional.
pub fn write_patches(
    doc: &mut AutoCommit,
    patches: Vec<automerge::Patch>,
    insert_anchor: &[ChangeHash],
    index_anchor: &[ChangeHash],
) {
    write_selected_patches(
        doc,
        patches.into_iter().map(|patch| (patch, true)).collect(),
        insert_anchor,
        index_anchor,
    );
}

/// Filtered history must retain skipped structural patches: later diff indices
/// still include their logical effects, even though their live content survives.
#[allow(
    clippy::too_many_lines,
    reason = "the patch replay must share object remapping and sequence state across every Automerge patch variant"
)]
#[doc(hidden)]
pub fn write_selected_patches(
    doc: &mut AutoCommit,
    patches: Vec<(automerge::Patch, bool)>,
    insert_anchor: &[ChangeHash],
    index_anchor: &[ChangeHash],
) {
    // Recreated objects get a fresh ExId; map the patch's (old) ObjId to it
    // so nested child patches target the new object.
    let mut remap: HashMap<ObjId, ObjId> = HashMap::new();
    // Diff indices refer to the sequence as earlier patches transform it,
    // which the live document — holding later collaborator edits too — is
    // not; each sequence object follows both.
    let mut sequences = sequence_replay::prepare(doc, &patches, index_anchor);
    for (patch, apply) in patches {
        let target = remap
            .get(&patch.obj)
            .cloned()
            .unwrap_or_else(|| patch.obj.clone());
        let is_text = matches!(doc.object_type(&target), Ok(automerge::ObjType::Text));
        let sequence =
            if is_text || matches!(doc.object_type(&target), Ok(automerge::ObjType::List)) {
                Some(
                    sequences
                        .entry(patch.obj.clone())
                        .or_insert_with(SequenceReplay::fresh),
                )
            } else {
                None
            };
        if !apply {
            // A skipped list patch still moves the indices of the ones after
            // it. (Filtered history keeps or skips a text object whole.)
            if let Some(sequence) = sequence {
                match &patch.action {
                    PatchAction::DeleteSeq { index, length } => {
                        sequence.skipped_delete(*index, *length);
                    }
                    PatchAction::Insert { index, values } => {
                        sequence.skipped_insert(*index, values.len());
                    }
                    _ => {}
                }
            }
            continue;
        }
        match patch.action {
            PatchAction::PutMap { key, value, .. } => match value.0 {
                Value::Scalar(s) => {
                    let _ = doc.put(&target, key, s.into_owned());
                }
                Value::Object(ot) => {
                    if let Ok(new_id) = doc.put_object(&target, key, ot) {
                        remap.insert(value.1, new_id);
                    }
                }
            },
            PatchAction::PutSeq { index, value, .. } => {
                // The element still exists after the action; leave it be when
                // a collaborator removed it since.
                let Some(sequence) = sequence else { continue };
                let Some(at) = sequence.element(index) else {
                    continue;
                };
                match value.0 {
                    Value::Scalar(s) => {
                        let _ = doc.put(&target, at.unit, s.into_owned());
                    }
                    Value::Object(ot) => {
                        if let Ok(new_id) = doc.put_object(&target, at.unit, ot) {
                            remap.insert(value.1, new_id);
                        }
                    }
                }
                sequence.replaced(doc, &target, index, at);
            }
            PatchAction::Insert { index, values } => {
                // Re-inserting elements the local action deleted.
                let Some(sequence) = sequence else { continue };
                let at = sequence.insertion(index);
                let mut written = 0usize;
                for (li, (val, old_id, _)) in (at.unit..).zip(&values) {
                    let inserted = match val {
                        Value::Scalar(s) => doc.insert(&target, li, s.clone().into_owned()).is_ok(),
                        Value::Object(ot) => doc
                            .insert_object(&target, li, *ot)
                            .map(|new_id| remap.insert(old_id.clone(), new_id))
                            .is_ok(),
                    };
                    if !inserted {
                        break;
                    }
                    written = written.saturating_add(1);
                }
                sequence.inserted(doc, &target, index, at, written);
            }
            PatchAction::SpliceText {
                index,
                value,
                marks,
            } => {
                // Re-inserting text the local action deleted, including native
                // formatting when a move/undo recreates its owning object.
                let Some(sequence) = sequence else { continue };
                let at = sequence.insertion(index);
                let li = at.unit;
                let previous_length = doc.length(&target);
                if doc
                    .splice_text(&target, li, 0, &value.make_string())
                    .is_ok()
                {
                    let written = doc.length(&target).saturating_sub(previous_length);
                    sequence.inserted(doc, &target, index, at, written);
                    let end = li.saturating_add(written);
                    // Insertion can inherit formatting from its new neighbors.
                    // The patch markset describes the complete restored span,
                    // so names absent from it must be removed, including when
                    // marks is None (an intentionally unformatted insertion).
                    let mut inherited = doc
                        .marks(&target)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|mark| mark.start < end && mark.end > li)
                        .filter(|mark| {
                            !marks.as_ref().is_some_and(|wanted| {
                                wanted.iter().any(|(name, _)| name == mark.name.as_str())
                            })
                        })
                        .map(|mark| mark.name.to_string())
                        .collect::<Vec<_>>();
                    inherited.compact_sort();
                    inherited.dedup();
                    for name in inherited {
                        let _ = doc.unmark(
                            &target,
                            &name,
                            li,
                            end,
                            automerge::marks::ExpandMark::After,
                        );
                    }
                    if let Some(marks) = marks {
                        for (name, value) in marks.iter() {
                            let _ = doc.mark(
                                &target,
                                automerge::marks::Mark::new(
                                    name.to_string(),
                                    value.clone(),
                                    li,
                                    end,
                                ),
                                automerge::marks::ExpandMark::After,
                            );
                        }
                    }
                }
            }
            PatchAction::Increment { prop, value } => {
                let prop = match (prop, sequence) {
                    (automerge::Prop::Seq(index), Some(sequence)) => {
                        let Some(at) = sequence.element(index) else {
                            continue;
                        };
                        automerge::Prop::Seq(at.unit)
                    }
                    (prop, _) => prop,
                };
                let _ = doc.increment(&target, prop, value);
            }
            PatchAction::DeleteMap { key } => {
                let _ = doc.delete(&target, key);
            }
            PatchAction::DeleteSeq { index, length } => {
                // Removing elements the local action inserted, except those
                // a collaborator removed since.
                let Some(sequence) = sequence else { continue };
                for (at, units) in sequence.delete(index, length) {
                    let Ok(units) = isize::try_from(units) else {
                        continue;
                    };
                    let _ = if is_text {
                        doc.splice_text(&target, at.unit, units, "")
                    } else {
                        doc.splice(
                            &target,
                            at.unit,
                            units,
                            std::iter::empty::<automerge::ScalarValue>(),
                        )
                    };
                }
            }
            PatchAction::Mark { marks } => {
                let Some(sequence) = sequence else { continue };
                for mut mark in marks {
                    let Some((start, end)) = sequence.span(mark.start, mark.end) else {
                        continue;
                    };
                    mark.start = start;
                    mark.end = end;
                    let _ = doc.mark(&target, mark, automerge::marks::ExpandMark::After);
                }
            }
            // Conflict markers carry no content to revert.
            PatchAction::Conflict { .. } => {}
        }
    }
    // Automerge 0.8's ExposeQueue::flush_obj emits re-exposed Text contents
    // with marks=None (its TODO for text_spans). Recreated text therefore needs
    // its native marks restored from the same historical frontier as its body.
    // Only fresh objects are touched; surviving peer text is never overwritten.
    for (old, fresh) in remap {
        if !matches!(doc.object_type(&fresh), Ok(automerge::ObjType::Text)) {
            continue;
        }
        let Ok(restored) = doc.marks_at(&old, insert_anchor) else {
            continue;
        };
        for mark in doc.marks(&fresh).unwrap_or_default() {
            let _ = doc.unmark(
                &fresh,
                mark.name.as_str(),
                mark.start,
                mark.end,
                automerge::marks::ExpandMark::After,
            );
        }
        for mark in restored {
            let _ = doc.mark(&fresh, mark, automerge::marks::ExpandMark::After);
        }
    }
}
