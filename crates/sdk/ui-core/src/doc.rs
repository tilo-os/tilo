//! CRDT helpers: writes that keep Automerge documents from churning, and typed reads.
//!
//! Splicing a whole string on every keystroke (delete all, reinsert new) costs
//! O(len) ops per edit and, since Automerge tombstones every inserted element
//! forever, O(len²) accumulated ops over a typing session — the store gets
//! slower until the app appears to freeze.
//!
//! [`splice_text_diffed`] / [`put_text_diffed`] diff old vs. new down to the
//! common prefix + suffix and splice only the changed middle span, so one
//! keystroke costs one op. [`read_text`] and [`read_str_or_text`] read text
//! back.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value};
use std::any::Any;

/// The document and object a widget pass lent as its
/// [`AppModel`](crate::widget::AppModel) and [`ModelPath`](crate::widget::ModelPath).
///
/// # Panics
///
/// Panics if the host lent another model: a document runtime lends an
/// `AutoCommit` and an `ObjId` in it.
#[allow(
    clippy::panic,
    reason = "a document host lends its document; any other model is a host bug"
)]
pub fn document<'a, 'doc>(
    model: &'a mut crate::widget::Edits<'doc>,
    path: &'a dyn Any,
) -> (&'a mut crate::widget::Edits<'doc>, &'a ObjId) {
    let Some(path) = path.downcast_ref::<ObjId>() else {
        panic!("the host lent a path that is not an Automerge object");
    };
    (model, path)
}

/// Read-only [`document`].
///
/// # Panics
///
/// Panics if the host lent another model.
#[allow(
    clippy::panic,
    reason = "a document host lends its document; any other model is a host bug"
)]
pub fn document_ref<'a>(model: &'a dyn Any, path: &'a dyn Any) -> (&'a AutoCommit, &'a ObjId) {
    let (Some(doc), Some(path)) = (
        model.downcast_ref::<AutoCommit>(),
        path.downcast_ref::<ObjId>(),
    ) else {
        panic!("the host lent a model that is not an Automerge document");
    };
    (doc, path)
}

/// The splice [`splice_text_diffed`] applied, in *characters* — Automerge `Text`
/// is char-addressed, not byte-addressed. A no-op reports `{pos:0, del:0, ins:0}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEdit {
    /// Char offset where the splice began (== length of the common prefix).
    pub pos: usize,
    pub del: usize,
    pub ins: usize,
}

impl TextEdit {
    pub fn is_noop(&self) -> bool {
        self.del == 0 && self.ins == 0
    }
}

/// Prefer [`put_text_diffed`] when you have a parent object + key and want the
/// `Text` created on first use.
///
/// # Panics
///
/// Panics if `text_id` does not resolve to a `Text` object in the document.
pub fn splice_text_diffed(doc: &mut AutoCommit, text_id: &ObjId, new: &str) -> TextEdit {
    let cur = doc.text(text_id).unwrap_or_default();
    if cur == new {
        return TextEdit {
            pos: 0,
            del: 0,
            ins: 0,
        };
    }
    let cur_chars: Vec<char> = cur.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();

    let prefix_len = cur_chars
        .iter()
        .zip(new_chars.iter())
        .take_while(|(a, b)| a == b)
        .count();
    // Suffix search bounds exclude the prefix so the two spans can't
    // overlap. `prefix_len` is at most `min(cur_chars.len(),
    // new_chars.len())` by construction above, so both tails are `Some`.
    let cur_tail = cur_chars.get(prefix_len..).unwrap_or(&[]);
    let new_tail = new_chars.get(prefix_len..).unwrap_or(&[]);
    let suffix_len = cur_tail
        .iter()
        .rev()
        .zip(new_tail.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    // `suffix_len` never exceeds either tail's length, so none of these underflow.
    let del = cur_chars
        .len()
        .saturating_sub(prefix_len)
        .saturating_sub(suffix_len);
    let ins_end = new_chars.len().saturating_sub(suffix_len);
    let ins: String = new_chars
        .get(prefix_len..ins_end)
        .unwrap_or(&[])
        .iter()
        .collect();
    let ins_len = ins_end.saturating_sub(prefix_len);
    // `del` is bounded by `Vec<char>::len()`, which the global allocator
    // already caps at `isize::MAX` bytes (4 bytes per `char`), so it always
    // fits `isize` on every real target.
    #[allow(
        clippy::cast_possible_wrap,
        reason = "del is bounded by Vec<char>::len(), which the allocator already caps under isize::MAX"
    )]
    let del_isize = del as isize;
    let _ = doc.splice_text(text_id, prefix_len, del_isize, &ins);
    TextEdit {
        pos: prefix_len,
        del,
        ins: ins_len,
    }
}

/// Creates the `Text` object on first use (inserting the whole value);
/// subsequent calls splice only the diff.
///
/// `put_object` only fails here if `obj` was concurrently deleted (or
/// retyped) by another collaborator between the read and the write; there is
/// no longer a place to put the text, so this reports the no-op edit rather
/// than panicking.
pub fn put_text_diffed<P: Into<Prop>>(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: P,
    new: &str,
) -> TextEdit {
    let prop = key.into();
    let tid = match doc.get(obj, prop.clone()) {
        Ok(Some((Value::Object(ObjType::Text), id))) => id,
        _ => match doc.put_object(obj, prop, ObjType::Text) {
            Ok(id) => id,
            Err(_) => {
                return TextEdit {
                    pos: 0,
                    del: 0,
                    ins: 0,
                };
            }
        },
    };
    splice_text_diffed(doc, &tid, new)
}

// Text readers: each returns `None` when the property is missing, holds a
// value of another type, or the lookup itself fails.

/// The contents of a `Text` object; see [`read_str_or_text`] for a field
/// that may hold a string scalar instead.
pub fn read_text<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<String> {
    match doc.get(obj, prop) {
        Ok(Some((Value::Object(ObjType::Text), text))) => doc.text(&text).ok(),
        _ => None,
    }
}

/// A string stored either as a string scalar or as a `Text` object, for
/// fields that migrated from one representation to the other.
pub fn read_str_or_text<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<String> {
    match doc.get(obj, prop) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Str(value) => Some(value.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(ObjType::Text), text))) => doc.text(&text).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    #[test]
    fn text_readers_distinguish_scalars_from_text_objects() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "str", "plain").unwrap();
        seed_text(&mut doc, "text", "rich");
        doc.put(&ROOT, "yes", true).unwrap();
        doc.put_object(&ROOT, "map", ObjType::Map).unwrap();
        assert_eq!(read_text(&doc, &ROOT, "text").as_deref(), Some("rich"));
        assert_eq!(read_text(&doc, &ROOT, "str"), None);
        assert_eq!(
            read_str_or_text(&doc, &ROOT, "str").as_deref(),
            Some("plain")
        );
        assert_eq!(
            read_str_or_text(&doc, &ROOT, "text").as_deref(),
            Some("rich")
        );
        for missing in ["absent", "yes", "map"] {
            assert_eq!(read_str_or_text(&doc, &ROOT, missing), None, "{missing}");
        }
    }

    fn seed_text(doc: &mut AutoCommit, key: &str, s: &str) -> ObjId {
        let id = doc.put_object(&ROOT, key, ObjType::Text).unwrap();
        doc.splice_text(&id, 0, 0, s).unwrap();
        id
    }

    #[test]
    fn text_writes_splice_only_the_changed_span_and_stay_bounded() {
        // Appending one char splices only the new char.
        {
            let mut doc = AutoCommit::new();
            let long = "abcdefghijklmnopqrstuvwxyz0123456789";
            let id = seed_text(&mut doc, "t", long);
            let mut target = long.to_string();
            target.push('!');

            let edit = splice_text_diffed(&mut doc, &id, &target);

            assert_eq!(
                edit,
                TextEdit {
                    pos: long.chars().count(),
                    del: 0,
                    ins: 1
                },
                "appending one char must splice exactly one char, not rewrite the text"
            );
            assert!(edit.ins < long.chars().count());
            assert_eq!(
                doc.text(&id).unwrap(),
                target,
                "must round-trip to the new value"
            );
        }

        // A middle edit splices only the changed span.
        {
            let mut doc = AutoCommit::new();
            let before = "the quick brown fox";
            let id = seed_text(&mut doc, "t", before);

            let edit = splice_text_diffed(&mut doc, &id, "the slow brown fox");

            assert_eq!(edit.pos, 4, "prefix 'the ' is common");
            assert_eq!(edit.del, 5, "'quick' is deleted");
            assert_eq!(edit.ins, 4, "'slow' is inserted");
            assert!(
                edit.del < before.chars().count() && edit.ins < before.chars().count(),
                "the splice must be bounded well under the full text length"
            );
            assert_eq!(doc.text(&id).unwrap(), "the slow brown fox");
        }

        // Re-storing an identical value is a no-op.
        {
            let mut doc = AutoCommit::new();
            let id = seed_text(&mut doc, "t", "unchanged");
            let before = doc.save().len();

            let edit = splice_text_diffed(&mut doc, &id, "unchanged");

            assert!(edit.is_noop(), "re-storing the same value must be a no-op");
            assert_eq!(
                edit,
                TextEdit {
                    pos: 0,
                    del: 0,
                    ins: 0
                }
            );
            assert_eq!(
                doc.save().len(),
                before,
                "an idempotent no-op must not grow the document"
            );
        }

        // `put_text_diffed` creates the Text, then diffs into it.
        {
            let mut doc = AutoCommit::new();

            let e0 = put_text_diffed(&mut doc, &ROOT, "body", "hello world");
            assert_eq!(
                e0,
                TextEdit {
                    pos: 0,
                    del: 0,
                    ins: 11
                }
            );

            let e1 = put_text_diffed(&mut doc, &ROOT, "body", "hello world!");
            assert_eq!(
                e1,
                TextEdit {
                    pos: 11,
                    del: 0,
                    ins: 1
                }
            );

            let (v, tid) = doc.get(&ROOT, "body").unwrap().unwrap();
            assert!(
                matches!(v, Value::Object(ObjType::Text)),
                "field is a CRDT Text"
            );
            assert_eq!(doc.text(&tid).unwrap(), "hello world!");
        }

        // Typing char by char keeps the document bounded.
        {
            let mut doc = AutoCommit::new();
            let mut s = String::new();
            for i in 0..500 {
                s.push(if i % 7 == 6 { ' ' } else { 'x' });
                put_text_diffed(&mut doc, &ROOT, "b", &s);
            }
            let size = doc.save().len();
            assert!(
                size < 12 * s.len(),
                "document is {size} bytes for {} typed chars — CRDT churn regressed",
                s.len()
            );
            let (_, tid) = doc.get(&ROOT, "b").unwrap().unwrap();
            assert_eq!(
                doc.text(&tid).unwrap(),
                s,
                "typed value must round-trip exactly"
            );
        }
    }
}
