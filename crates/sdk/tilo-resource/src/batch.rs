//! Writing a large amount of new content — a whole imported document, a
//! pasted book — as a few changes instead of one operation at a time.
//!
//! Automerge applies every local operation to its op set on the spot, and
//! each application pays for a seek plus an update of every column and index;
//! an inline mark costs the most, because the mark index is rebuilt around
//! each anchor. Ops a *change* makes on objects the document has not seen
//! integrate as one run at the end of the op set, marks included. A
//! [`ChangeWriter`] builds such changes directly, numbering ops exactly as
//! Automerge will, on top of the document's own heads and actor, so the
//! result is indistinguishable from the same writes made op by op — only
//! several times faster to apply.

use std::num::NonZeroU64;

use automerge::{ActorId, ChangeHash, ObjId, ObjType, ReadDoc, ScalarValue, TextEncoding, legacy};

use tilo_automerge::ApplyIncoming;

use crate::model::Doc;

/// Staged content bigger than this many ops is split over several changes,
/// bounding the memory the expanded ops take before they are encoded.
pub const OPS_PER_CHANGE: usize = 1 << 16;

/// An object a [`ChangeWriter`] created, to write inside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created(legacy::OpId);

/// An inline mark over characters `start..end` of a text the writer creates.
#[derive(Clone, Debug)]
pub struct TextMark {
    pub name: String,
    pub value: ScalarValue,
    pub start: usize,
    pub end: usize,
}

fn legacy_obj(obj: &ObjId) -> legacy::ObjectId {
    match obj {
        ObjId::Root => legacy::ObjectId::Root,
        ObjId::Id(counter, actor, _) => legacy::ObjectId::Id(legacy::OpId(*counter, actor.clone())),
    }
}

/// Builds the changes that write new content into one document. See the
/// module docs.
pub struct ChangeWriter {
    actor: ActorId,
    seq: u64,
    next: u64,
    /// Counter of the first op of the change being built.
    start: NonZeroU64,
    deps: Vec<ChangeHash>,
    ops: Vec<legacy::Op>,
}

impl ChangeWriter {
    /// A writer continuing `doc`'s history as its actor: the open
    /// transaction is committed first, and the changes this builds follow it.
    /// `None` when the document can't take changes written this way (a text
    /// encoding other than code points, an actor whose last change is
    /// unreadable); write op by op instead.
    pub fn continuing(doc: &mut Doc) -> Option<Self> {
        if doc.text_encoding() != TextEncoding::UnicodeCodePoint {
            return None;
        }
        let committed = doc.commit();
        let actor = doc.get_actor().clone();
        let last_seq = match committed {
            Some(hash) => match doc.get_change_meta_by_hash(&hash) {
                Some(meta) if meta.actor.as_ref() == &actor => meta.seq,
                _ => return None,
            },
            None => doc
                .get_changes_meta(&[])
                .iter()
                .filter(|meta| meta.actor.as_ref() == &actor)
                .map(|meta| meta.seq)
                .max()
                .unwrap_or(0),
        };
        let heads = doc.get_heads();
        let mut max_op = 0;
        for head in &heads {
            max_op = max_op.max(doc.get_change_meta_by_hash(head)?.max_op);
        }
        let start = NonZeroU64::new(max_op.saturating_add(1))?;
        Some(Self {
            actor,
            seq: last_seq.saturating_add(1),
            next: start.get(),
            start,
            deps: heads,
            ops: Vec::new(),
        })
    }

    /// Ops in the change being built.
    pub fn pending(&self) -> usize {
        self.ops.len()
    }

    fn push(
        &mut self,
        obj: legacy::ObjectId,
        key: legacy::Key,
        action: legacy::OpType,
        insert: bool,
    ) -> legacy::OpId {
        let id = legacy::OpId(self.next, self.actor.clone());
        self.next = self.next.saturating_add(1);
        self.ops.push(legacy::Op {
            action,
            obj,
            key,
            pred: legacy::SortedVec::new(),
            insert,
        });
        id
    }

    /// Create an object of `kind` under `key` of an object the document
    /// already has.
    pub fn make_in(&mut self, obj: &ObjId, key: &str, kind: ObjType) -> Created {
        Created(self.push(
            legacy_obj(obj),
            legacy::Key::Map(key.into()),
            legacy::OpType::Make(kind),
            false,
        ))
    }

    /// Create an object of `kind` under `key` of an object this writer
    /// created.
    pub fn make(&mut self, parent: &Created, key: &str, kind: ObjType) -> Created {
        Created(self.put_op(parent, key, legacy::OpType::Make(kind)))
    }

    /// Put a scalar under `key` of an object this writer created.
    pub fn put(&mut self, parent: &Created, key: &str, value: ScalarValue) {
        self.put_op(parent, key, legacy::OpType::Put(value));
    }

    fn put_op(&mut self, parent: &Created, key: &str, action: legacy::OpType) -> legacy::OpId {
        self.push(
            legacy::ObjectId::Id(parent.0.clone()),
            legacy::Key::Map(key.into()),
            action,
            false,
        )
    }

    /// Create a text object under `key` of an object this writer created,
    /// holding `text` with `marks` applied: per mark, a begin anchor after
    /// the character before its start (not expanding) and an end anchor after
    /// the character before its end (expanding), which is where a local
    /// `ExpandMark::After` mark puts them. Marks must be non-empty and within
    /// the text.
    pub fn text(&mut self, parent: &Created, key: &str, text: &str, marks: &[TextMark]) {
        let text_id = self.put_op(parent, key, legacy::OpType::Make(ObjType::Text));
        let obj = legacy::ObjectId::Id(text_id);
        let first = self.next;
        let mut prev = legacy::ElementId::Head;
        for ch in text.chars() {
            let id = self.push(
                obj.clone(),
                legacy::Key::Seq(prev),
                legacy::OpType::Put(ch.into()),
                true,
            );
            prev = legacy::ElementId::Id(id);
        }
        for mark in marks {
            let begin = self.anchor(first, mark.start);
            let end = self.anchor(first, mark.end);
            self.push(
                obj.clone(),
                legacy::Key::Seq(begin),
                legacy::OpType::MarkBegin(legacy::MarkData {
                    name: mark.name.as_str().into(),
                    value: mark.value.clone(),
                    expand: false,
                }),
                true,
            );
            self.push(
                obj.clone(),
                legacy::Key::Seq(end),
                legacy::OpType::MarkEnd(true),
                true,
            );
        }
    }

    /// The element a mark anchor at `pos` sits after, in a text whose
    /// character 0 has op counter `first`: the character before `pos`, or
    /// the head.
    fn anchor(&self, first: u64, pos: usize) -> legacy::ElementId {
        match pos.checked_sub(1).and_then(|p| u64::try_from(p).ok()) {
            Some(p) => {
                legacy::ElementId::Id(legacy::OpId(first.saturating_add(p), self.actor.clone()))
            }
            None => legacy::ElementId::Head,
        }
    }

    /// Close the change being built; `None` if it has no ops.
    fn seal(&mut self) -> Option<automerge::Change> {
        if self.ops.is_empty() {
            return None;
        }
        let change = automerge::Change::from(automerge::ExpandedChange {
            operations: std::mem::take(&mut self.ops),
            actor_id: self.actor.clone(),
            hash: None,
            seq: self.seq,
            start_op: self.start,
            time: 0,
            message: None,
            deps: std::mem::take(&mut self.deps),
            extra_bytes: Vec::new(),
            author: None,
        });
        self.deps = vec![change.hash()];
        self.seq = self.seq.saturating_add(1);
        // `next` only grows from `start`, which is at least one.
        self.start = NonZeroU64::new(self.next).unwrap_or(self.start);
        Some(change)
    }

    /// Seal the change being built and apply it to `doc`, the document this
    /// writer continues. Apply as soon as a change is big enough
    /// ([`OPS_PER_CHANGE`]): Automerge expands a change's ops in memory while
    /// integrating it.
    ///
    /// # Errors
    /// Returns Automerge's refusal; the document is unchanged.
    pub fn apply(&mut self, doc: &mut Doc) -> Result<(), automerge::AutomergeError> {
        match self.seal() {
            Some(change) => apply_change(doc, change),
            None => Ok(()),
        }
    }
}

/// Apply one change, dropping the patch events Automerge logs for it unless
/// a diff cursor will read them (see `tilo_automerge`).
pub fn apply_change(
    doc: &mut Doc,
    change: automerge::Change,
) -> Result<(), automerge::AutomergeError> {
    doc.apply_incoming([change])
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use automerge::marks::{ExpandMark, Mark};
    use automerge::transaction::Transactable;

    #[test]
    fn changes_written_directly_read_like_the_same_writes_op_by_op() {
        let mut direct = Doc::new().with_actor(ActorId::from(b"writer".as_slice()));
        let items = direct.put_object(ROOT, "items", ObjType::Map).unwrap();
        direct.commit();
        let mut by_ops = direct
            .fork()
            .with_actor(ActorId::from(b"writer".as_slice()));

        let mut writer = ChangeWriter::continuing(&mut direct).unwrap();
        let item = writer.make_in(&items, "a", ObjType::Map);
        writer.put(&item, "kind", "note".into());
        let bold = TextMark {
            name: "bold".into(),
            value: true.into(),
            start: 0,
            end: 5,
        };
        writer.text(&item, "text", "hello world", &[bold]);
        writer.apply(&mut direct).unwrap();

        let item = by_ops.put_object(&items, "a", ObjType::Map).unwrap();
        by_ops.put(&item, "kind", "note").unwrap();
        let text = by_ops.put_object(&item, "text", ObjType::Text).unwrap();
        by_ops.splice_text(&text, 0, 0, "hello world").unwrap();
        by_ops
            .mark(
                &text,
                Mark::new("bold".into(), true, 0, 5),
                ExpandMark::After,
            )
            .unwrap();
        by_ops.commit();

        for doc in [&direct, &by_ops] {
            let item = doc.get(&items, "a").unwrap().unwrap().1;
            let text = doc.get(&item, "text").unwrap().unwrap().1;
            assert_eq!(doc.text(&text).unwrap(), "hello world");
            let marks = doc.marks(&text).unwrap();
            assert_eq!(marks.len(), 1);
            assert_eq!((marks[0].start, marks[0].end), (0, 5));
        }
        let item = direct.get(&items, "a").unwrap().unwrap().1;
        direct.put(&item, "kind", "done").unwrap();
        assert!(direct.commit().is_some());
    }
}
