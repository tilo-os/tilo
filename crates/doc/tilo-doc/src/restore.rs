//! Restore an earlier state of a document as a new change on top of its
//! current heads.
//!
//! Version history restores are CRDT operations, not byte replacement: the
//! restored document keeps every change it had and gains one that takes its
//! state back to the selected version. A peer that edited the same document
//! concurrently merges with the restore like with any other edit, and the
//! history continues instead of forking.
//!
//! The change is the structural difference between the current state and the
//! selected one (Automerge's own diff between the two head sets), so objects
//! both states share keep their identity and only what differs is written.
//! An object the selected version has but the current state lost is authored
//! afresh from the selected version, marks included. Before the caller commits
//! anything, the written state is compared with the selected one in full; a
//! difference is an error, never a silently different document.

use crate::document::DocError;
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{
    AutoCommit, ChangeHash, ObjId, ObjType, Patch, PatchAction, Prop, ROOT, ReadDoc, ScalarValue,
    Value,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use tilo_automerge::ApplyIncoming;
use tilo_resource::history::commit_stamped_with_message;
use tilo_sort::CompactSort as _;

/// Write into `doc`'s open transaction the operations that take its current
/// state to the state it had at `target`, whose changes `doc` must hold.
/// Marks written by the restore expand like `expand`.
///
/// Returns whether anything was written. The caller commits.
///
/// # Errors
///
/// [`DocError::Invalid`] when `doc` lacks a change of `target` or the written
/// state differs from the one at `target`; [`DocError::Automerge`] when a write
/// fails. Either way the open transaction holds partial work the caller
/// must roll back.
pub fn restore_to_heads(
    doc: &mut AutoCommit,
    target: &[ChangeHash],
    expand: ExpandMark,
) -> Result<bool, DocError> {
    let mut target = target.to_vec();
    target.compact_sort();
    target.dedup();
    if target
        .iter()
        .any(|hash| doc.get_change_by_hash(hash).is_none())
    {
        return Err(DocError::Invalid(
            "the version to restore is not in this document's history".into(),
        ));
    }
    let current = doc.get_heads();
    if current == target {
        return Ok(false);
    }
    let patches = doc.diff(&current, &target);
    let mut writer = Writer {
        doc,
        target: &target,
        expand,
        created: HashMap::new(),
        authored: HashSet::new(),
    };
    for patch in patches {
        writer.apply(patch)?;
    }
    if !same_state(writer.doc, &target)? {
        return Err(DocError::Invalid(
            "restoring did not reproduce the selected version".into(),
        ));
    }
    Ok(writer.doc.pending_ops() > 0)
}

/// Restore serialized document `current` to the state of the serialized
/// version `target` of the same document: `current`'s history plus one change
/// whose state equals `target`'s. A `target` from a separate history is merged
/// in first, so the result descends from both.
///
/// Returns `None` when `current` already has `target`'s heads, so a restore
/// would change nothing.
///
/// # Errors
///
/// As [`restore_to_heads`], and [`DocError::Load`] / [`DocError::Invalid`]
/// when either input or the result is not a document.
pub fn restore_serialized(
    current: &[u8],
    target: &[u8],
    message: Option<String>,
) -> Result<Option<Vec<u8>>, DocError> {
    // Loading through `Document` rejects Automerge data that is not a
    // document of this schema.
    crate::Document::load(current)?;
    crate::Document::load(target)?;
    let mut doc = AutoCommit::load(current).map_err(|e| DocError::Load(e.to_string()))?;
    let mut selected = AutoCommit::load(target).map_err(|e| DocError::Load(e.to_string()))?;
    let selected_heads = selected.get_heads();
    let before = doc.get_heads();
    doc.merge_incoming(&mut selected)?;
    restore_to_heads(&mut doc, &selected_heads, ExpandMark::After)?;
    commit_stamped_with_message(&mut doc, message);
    if doc.get_heads() == before {
        return Ok(None);
    }
    let bytes = doc.save();
    crate::Document::load(&bytes)?;
    Ok(Some(bytes))
}

struct Writer<'a> {
    doc: &'a mut AutoCommit,
    target: &'a [ChangeHash],
    expand: ExpandMark,
    /// Objects of the selected state the restore authored afresh, by their
    /// id in the selected state.
    created: HashMap<ObjId, ObjId>,
    /// Selected-state objects whose whole content the restore already wrote.
    authored: HashSet<ObjId>,
}

impl Writer<'_> {
    fn apply(&mut self, patch: Patch) -> Result<(), DocError> {
        if self.authored.contains(&patch.obj)
            || patch
                .path
                .iter()
                .any(|(obj, _)| self.authored.contains(obj))
        {
            // Inside an object written whole from the selected state.
            return Ok(());
        }
        let obj = self.created.get(&patch.obj).cloned().unwrap_or(patch.obj);
        match patch.action {
            PatchAction::PutMap { key, value, .. } => {
                self.put(&obj, Prop::Map(key), value, false)?;
            }
            PatchAction::PutSeq { index, value, .. } => {
                self.put(&obj, Prop::Seq(index), value, false)?;
            }
            PatchAction::Insert { index, values } => {
                for (offset, (value, id, _)) in values.iter().enumerate() {
                    let at = index
                        .checked_add(offset)
                        .ok_or_else(|| DocError::Invalid("sequence index overflow".into()))?;
                    self.put(&obj, Prop::Seq(at), (value.clone(), id.clone()), true)?;
                }
            }
            PatchAction::SpliceText {
                index,
                value,
                marks,
            } => {
                let before = self.doc.length(&obj);
                self.doc.splice_text(&obj, index, 0, &value.make_string())?;
                let inserted = self.doc.length(&obj).saturating_sub(before);
                let wanted = marks
                    .map(|set| {
                        set.iter()
                            .map(|(name, value)| (name.to_owned(), value.clone()))
                            .collect::<BTreeMap<_, _>>()
                    })
                    .unwrap_or_default();
                self.settle_marks(&obj, index, inserted, &wanted)?;
            }
            PatchAction::Increment { prop, value } => self.doc.increment(&obj, prop, value)?,
            PatchAction::Conflict { .. } => {}
            PatchAction::DeleteMap { key } => self.doc.delete(&obj, key.as_str())?,
            PatchAction::DeleteSeq { index, length } => {
                if self.doc.object_type(&obj)? == ObjType::Text {
                    let length = isize::try_from(length)
                        .map_err(|_| DocError::Invalid("text deletion too long".into()))?;
                    self.doc.splice_text(&obj, index, length, "")?;
                } else {
                    for _ in 0..length {
                        self.doc.delete(&obj, index)?;
                    }
                }
            }
            PatchAction::Mark { marks } => {
                for mark in marks {
                    self.doc.mark(&obj, mark, self.expand)?;
                }
            }
        }
        Ok(())
    }

    /// Give the `len` characters just inserted at `index` exactly the marks
    /// `wanted`, whatever they inherited from their neighbours.
    fn settle_marks(
        &mut self,
        obj: &ObjId,
        index: usize,
        len: usize,
        wanted: &BTreeMap<String, ScalarValue>,
    ) -> Result<(), DocError> {
        if len == 0 {
            return Ok(());
        }
        let end = index
            .checked_add(len)
            .ok_or_else(|| DocError::Invalid("text index overflow".into()))?;
        let have = self
            .doc
            .get_marks(obj, index, None)?
            .iter()
            .map(|(name, value)| (name.to_owned(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        for (name, value) in wanted {
            if have.get(name) != Some(value) {
                let mark = Mark::new(name.clone(), value.clone(), index, end);
                self.doc.mark(obj, mark, self.expand)?;
            }
        }
        for name in have.keys().filter(|name| !wanted.contains_key(*name)) {
            self.doc.unmark(obj, name, index, end, self.expand)?;
        }
        Ok(())
    }

    /// Write one value at `prop` of `parent`: a scalar as itself, an object
    /// as a new object holding the selected state's copy of it.
    fn put(
        &mut self,
        parent: &ObjId,
        prop: Prop,
        (value, selected): (Value<'_>, ObjId),
        insert: bool,
    ) -> Result<(), DocError> {
        match (value, insert, prop) {
            (Value::Scalar(scalar), true, Prop::Seq(index)) => {
                self.doc.insert(parent, index, scalar.into_owned())?;
            }
            (Value::Scalar(scalar), _, prop) => self.doc.put(parent, prop, scalar.into_owned())?,
            (Value::Object(kind), true, Prop::Seq(index)) => {
                let created = self.doc.insert_object(parent, index, kind)?;
                self.author(selected, created, kind)?;
            }
            (Value::Object(kind), _, prop) => {
                let created = self.doc.put_object(parent, prop, kind)?;
                self.author(selected, created, kind)?;
            }
        }
        Ok(())
    }

    /// Fill the new object `created` with the whole content `selected` has
    /// in the selected state.
    fn author(&mut self, selected: ObjId, created: ObjId, kind: ObjType) -> Result<(), DocError> {
        let mut pending = vec![(selected, created, kind)];
        while let Some((source, destination, kind)) = pending.pop() {
            self.authored.insert(source.clone());
            self.created.insert(source.clone(), destination.clone());
            match kind {
                ObjType::Map | ObjType::Table => {
                    let keys = self.doc.keys_at(&source, self.target).collect::<Vec<_>>();
                    for key in keys {
                        match entry(self.doc, &source, key.as_str(), Some(self.target))? {
                            Some(Entry::Scalar(scalar)) => {
                                self.doc.put(&destination, key.as_str(), scalar)?;
                            }
                            Some(Entry::Object(kind, child)) => {
                                let made = self.doc.put_object(&destination, key.as_str(), kind)?;
                                pending.push((child, made, kind));
                            }
                            None => {}
                        }
                    }
                }
                ObjType::List => {
                    for index in 0..self.doc.length_at(&source, self.target) {
                        match entry(self.doc, &source, index, Some(self.target))? {
                            Some(Entry::Scalar(scalar)) => {
                                self.doc.insert(&destination, index, scalar)?;
                            }
                            Some(Entry::Object(kind, child)) => {
                                let made = self.doc.insert_object(&destination, index, kind)?;
                                pending.push((child, made, kind));
                            }
                            None => {}
                        }
                    }
                }
                ObjType::Text => {
                    let text = self.doc.text_at(&source, self.target)?;
                    self.doc.splice_text(&destination, 0, 0, &text)?;
                    for mark in self.doc.marks_at(&source, self.target)? {
                        self.doc.mark(&destination, mark, self.expand)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// One value of an object, owned.
#[derive(PartialEq)]
enum Entry {
    Scalar(ScalarValue),
    Object(ObjType, ObjId),
}

fn entry(
    doc: &AutoCommit,
    obj: &ObjId,
    prop: impl Into<Prop>,
    at: Option<&[ChangeHash]>,
) -> Result<Option<Entry>, DocError> {
    let found = match at {
        Some(heads) => doc.get_at(obj, prop, heads)?,
        None => doc.get(obj, prop)?,
    };
    Ok(found.map(|(value, id)| match value {
        Value::Object(kind) => Entry::Object(kind, id),
        Value::Scalar(scalar) => Entry::Scalar(scalar.into_owned()),
    }))
}

/// Whether `doc`'s current state equals its state at `target`: the same
/// keys, values, sequences, text and marks, whatever the objects' ids.
fn same_state(doc: &AutoCommit, target: &[ChangeHash]) -> Result<bool, DocError> {
    let mut pending = vec![(ROOT, ROOT, ObjType::Map)];
    while let Some((now, then, kind)) = pending.pop() {
        match kind {
            ObjType::Map | ObjType::Table => {
                let mut keys = doc.keys(&now).collect::<Vec<_>>();
                let mut selected = doc.keys_at(&then, target).collect::<Vec<_>>();
                keys.compact_sort();
                selected.compact_sort();
                if keys != selected {
                    return Ok(false);
                }
                for key in keys {
                    let left = entry(doc, &now, key.as_str(), None)?;
                    let right = entry(doc, &then, key.as_str(), Some(target))?;
                    if !same_entry(left, right, &mut pending) {
                        return Ok(false);
                    }
                }
            }
            ObjType::List => {
                let len = doc.length(&now);
                if len != doc.length_at(&then, target) {
                    return Ok(false);
                }
                for index in 0..len {
                    let left = entry(doc, &now, index, None)?;
                    let right = entry(doc, &then, index, Some(target))?;
                    if !same_entry(left, right, &mut pending) {
                        return Ok(false);
                    }
                }
            }
            ObjType::Text => {
                if doc.text(&now)? != doc.text_at(&then, target)?
                    || doc.marks(&now)? != doc.marks_at(&then, target)?
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

fn same_entry(
    left: Option<Entry>,
    right: Option<Entry>,
    pending: &mut Vec<(ObjId, ObjId, ObjType)>,
) -> bool {
    match (left, right) {
        (Some(Entry::Object(kind, now)), Some(Entry::Object(selected, then)))
            if kind == selected =>
        {
            pending.push((now, then, kind));
            true
        }
        (left, right) => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    fn heads_of(bytes: &[u8]) -> Vec<ChangeHash> {
        AutoCommit::load(bytes).unwrap().get_heads()
    }

    fn texts(bytes: &[u8]) -> Vec<String> {
        let document = Document::load(bytes).unwrap();
        document
            .blocks_in_order()
            .iter()
            .map(|id| document.block_text(id).unwrap_or_default())
            .collect()
    }

    /// v1: "alpha" (bold "alp"), "beta". v2: "alpha" unbolded and edited,
    /// "beta" removed, "gamma" added.
    fn versions() -> (Vec<u8>, Vec<u8>, String) {
        let mut document = Document::new();
        let alpha = document.insert_block(None, "paragraph");
        document.splice_text(&alpha, 0, 0, "alpha");
        document.add_mark(&alpha, 0, 3, "strong", true);
        let beta = document.insert_block(Some(&alpha), "paragraph");
        document.splice_text(&beta, 0, 0, "beta");
        let v1 = document.save();
        document.splice_text(&alpha, 5, 0, " and more");
        document.add_mark(&alpha, 0, 3, "strong", ScalarValue::Null);
        document.remove_block(&beta);
        let gamma = document.insert_block(Some(&alpha), "paragraph");
        document.splice_text(&gamma, 0, 0, "gamma");
        let v2 = document.save();
        (v1, v2, alpha)
    }

    #[test]
    fn restores_the_selected_state_as_a_descendant_change() {
        let (v1, v2, _) = versions();
        let restored = restore_serialized(&v2, &v1, Some("restore".into()))
            .unwrap()
            .expect("the restore changes the document");
        assert_eq!(
            Document::load(&restored).unwrap().materialize(),
            Document::load(&v1).unwrap().materialize()
        );
        assert_eq!(texts(&restored), vec!["alpha", "beta"]);
        let mut doc = AutoCommit::load(&restored).unwrap();
        for head in heads_of(&v2) {
            assert!(
                doc.get_change_by_hash(&head).is_some(),
                "the restore descends from the current version"
            );
        }
        assert_ne!(doc.get_heads(), heads_of(&v2));
    }

    #[test]
    fn restoring_the_current_version_changes_nothing() {
        let (_, v2, _) = versions();
        assert_eq!(restore_serialized(&v2, &v2, None).unwrap(), None);
    }

    #[test]
    fn restoring_forward_after_a_restore_reaches_the_later_state() {
        let (v1, v2, _) = versions();
        let back = restore_serialized(&v2, &v1, None).unwrap().unwrap();
        let forward = restore_serialized(&back, &v2, None).unwrap().unwrap();
        assert_eq!(
            Document::load(&forward).unwrap().materialize(),
            Document::load(&v2).unwrap().materialize()
        );
    }

    #[test]
    fn a_concurrent_edit_merges_with_the_restore() {
        let (v1, v2, alpha) = versions();
        let restored = restore_serialized(&v2, &v1, None).unwrap().unwrap();
        let mut peer = Document::load(&v2).unwrap();
        let end = peer.block_text(&alpha).unwrap().chars().count();
        peer.splice_text(&alpha, end, 0, "!");
        let mut merged = AutoCommit::load(&restored).unwrap();
        let mut peer = AutoCommit::load(&peer.save()).unwrap();
        merged.merge_incoming(&mut peer).unwrap();
        let merged = texts(&merged.save());
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert_eq!(merged[1], "beta");
        assert!(
            merged[0].starts_with("alpha") && merged[0].ends_with('!'),
            "the peer's edit survives the restore: {merged:?}"
        );
    }

    #[test]
    fn a_version_from_another_history_is_merged_then_restored() {
        let (_, v2, _) = versions();
        let mut other = Document::new();
        let block = other.insert_block(None, "paragraph");
        other.splice_text(&block, 0, 0, "elsewhere");
        let other = other.save();
        let restored = restore_serialized(&v2, &other, None).unwrap().unwrap();
        assert_eq!(texts(&restored), vec!["elsewhere"]);
        let doc = AutoCommit::load(&restored).unwrap();
        for head in heads_of(&v2).into_iter().chain(heads_of(&other)) {
            assert!(doc.get_change_by_hash(&head).is_some());
        }
    }

    #[test]
    fn non_documents_are_refused() {
        let (v1, _, _) = versions();
        assert!(restore_serialized(b"not a document", &v1, None).is_err());
        assert!(restore_serialized(&v1, b"not a document", None).is_err());
        let mut plain = AutoCommit::new();
        plain.put(ROOT, "key", "value").unwrap();
        assert!(restore_serialized(&v1, &plain.save(), None).is_err());
    }
}
