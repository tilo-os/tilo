//! CRDT write helpers that keep Automerge documents from churning.
//!
//! Splicing a whole string on every keystroke (delete all, reinsert new) costs
//! O(len) ops per edit and, since Automerge tombstones every inserted element
//! forever, O(len²) accumulated ops over a typing session — the store gets
//! slower until the app appears to freeze.
//!
//! [`splice_text_diffed`] / [`put_text_diffed`] diff old vs. new down to the
//! common prefix + suffix and splice only the changed middle span, so one
//! keystroke costs one op. [`put_if_changed`] is the scalar analogue.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value};

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

/// Returns `true` if an op was emitted. Guards against unbounded document growth
/// from re-`put`ting an unchanged scalar every frame. `f64` fields that can hold
/// `NaN` need a bit-exact guard at the call site instead — `NaN != NaN` would
/// make this re-emit forever.
pub fn put_if_changed<P: Into<Prop>, V: Into<ScalarValue>>(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: P,
    val: V,
) -> bool {
    let prop = key.into();
    let val = val.into();
    let unchanged = matches!(
        doc.get(obj, prop.clone()),
        Ok(Some((Value::Scalar(cur), _))) if cur.as_ref() == &val
    );
    if unchanged {
        return false;
    }
    let _ = doc.put(obj, prop, val);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ROOT, ReadDoc};

    fn seed_text(doc: &mut AutoCommit, key: &str, s: &str) -> ObjId {
        let id = doc.put_object(&ROOT, key, ObjType::Text).unwrap();
        doc.splice_text(&id, 0, 0, s).unwrap();
        id
    }

    #[test]
    fn single_char_append_splices_only_the_new_char() {
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

    #[test]
    fn middle_edit_splices_only_the_changed_span() {
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

    #[test]
    fn identical_value_is_a_noop() {
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

    #[test]
    fn put_text_diffed_creates_then_diffs() {
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

    #[test]
    fn typing_char_by_char_keeps_the_doc_bounded() {
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

    #[test]
    fn put_if_changed_guards_redundant_writes() {
        let mut doc = AutoCommit::new();

        assert!(
            put_if_changed(&mut doc, &ROOT, "n", 1i64),
            "first write happens"
        );
        let before = doc.save().len();

        assert!(
            !put_if_changed(&mut doc, &ROOT, "n", 1i64),
            "unchanged re-put is a no-op"
        );
        assert_eq!(
            doc.save().len(),
            before,
            "unchanged re-put must not grow the doc"
        );

        assert!(
            put_if_changed(&mut doc, &ROOT, "n", 2i64),
            "changed value writes"
        );
        let got = match doc.get(&ROOT, "n").unwrap().unwrap().0 {
            Value::Scalar(v) => v.to_i64(),
            Value::Object(_) => None,
        };
        assert_eq!(got, Some(2));
    }

    #[test]
    fn put_if_changed_handles_strings_and_bools() {
        let mut doc = AutoCommit::new();
        assert!(put_if_changed(&mut doc, &ROOT, "s", "a"));
        assert!(!put_if_changed(&mut doc, &ROOT, "s", "a"));
        assert!(put_if_changed(&mut doc, &ROOT, "s", "b"));

        assert!(put_if_changed(&mut doc, &ROOT, "flag", true));
        assert!(!put_if_changed(&mut doc, &ROOT, "flag", true));
        assert!(put_if_changed(&mut doc, &ROOT, "flag", false));
    }
}
