//! Containers a revert leaves standing because somebody built on them.
//!
//! Undoing an action that created a container — a paragraph, a row, a card —
//! removes the container. When anyone has written inside it since, removing
//! it would take their work along with the action being undone, so the
//! container stays: its removal is kept in the patch stream, for the indices
//! of the patches after it, but not written. Everything else the action did
//! is still reverted.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, Patch, PatchAction, ReadDoc, Value};

/// `patches` — a diff from `from` — each marked whether to write it: every
/// removal of a container that changed between `from` and now is held back.
pub(super) fn hold_back_built_on(
    doc: &mut AutoCommit,
    patches: Vec<Patch>,
    from: &[ChangeHash],
) -> Vec<(Patch, bool)> {
    let live = doc.get_heads();
    let written = if live.as_slice() == from {
        HashSet::new()
    } else {
        written_inside(doc, from, &live)
    };
    if written.is_empty() {
        return patches.into_iter().map(|patch| (patch, true)).collect();
    }
    let built_on = |value: Option<(Value<'_>, ObjId)>| {
        value.is_some_and(|(value, id)| matches!(value, Value::Object(_)) && written.contains(&id))
    };
    // Each list's elements as the patches so far leave it: the object an
    // element holds, when it is one that existed at `from`.
    let mut lists: HashMap<ObjId, Vec<Option<ObjId>>> = HashMap::new();
    let mut out = Vec::with_capacity(patches.len());
    for patch in patches {
        let is_list = matches!(doc.object_type(&patch.obj), Ok(ObjType::List));
        match &patch.action {
            PatchAction::DeleteMap { key } => {
                let keep = built_on(doc.get_at(&patch.obj, key.as_str(), from).ok().flatten());
                out.push((patch, !keep));
            }
            PatchAction::Insert { index, values } if is_list => {
                let order = list(&mut lists, doc, &patch.obj, from);
                let index = (*index).min(order.len());
                order.splice(index..index, std::iter::repeat_n(None, values.len()));
                out.push((patch, true));
            }
            PatchAction::DeleteSeq { index, length } if is_list => {
                let order = list(&mut lists, doc, &patch.obj, from);
                let end = index.saturating_add(*length).min(order.len());
                let removed: Vec<Option<ObjId>> = order.drain((*index).min(end)..end).collect();
                if !removed
                    .iter()
                    .any(|id| id.as_ref().is_some_and(|id| written.contains(id)))
                {
                    out.push((patch, true));
                    continue;
                }
                // Remove the elements nobody built on, one at a time; each
                // removal, written or not, occupies the same evolving index.
                for id in removed {
                    let keep = id.is_some_and(|id| written.contains(&id));
                    out.push((
                        Patch {
                            obj: patch.obj.clone(),
                            path: patch.path.clone(),
                            action: PatchAction::DeleteSeq {
                                index: *index,
                                length: 1,
                            },
                        },
                        !keep,
                    ));
                }
            }
            _ => out.push((patch, true)),
        }
    }
    out
}

/// Every object something was written inside between `from` and `to`: the
/// object each change of the diff lands on, and all its ancestors.
fn written_inside(doc: &mut AutoCommit, from: &[ChangeHash], to: &[ChangeHash]) -> HashSet<ObjId> {
    let mut out = HashSet::new();
    for patch in doc.diff(from, to) {
        out.extend(patch.path.into_iter().map(|(obj, _)| obj));
        out.insert(patch.obj);
    }
    out
}

fn list<'a>(
    lists: &'a mut HashMap<ObjId, Vec<Option<ObjId>>>,
    doc: &AutoCommit,
    obj: &ObjId,
    from: &[ChangeHash],
) -> &'a mut Vec<Option<ObjId>> {
    lists.entry(obj.clone()).or_insert_with(|| {
        doc.values_at(obj, from)
            .map(|(value, id)| matches!(value, Value::Object(_)).then_some(id))
            .collect()
    })
}
