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

    let mut p = 0;
    while p < cur_chars.len() && p < new_chars.len() && cur_chars[p] == new_chars[p] {
        p += 1;
    }
    // Suffix search bounds exclude the prefix so the two spans can't overlap.
    let mut s = 0;
    while s < cur_chars.len() - p
        && s < new_chars.len() - p
        && cur_chars[cur_chars.len() - 1 - s] == new_chars[new_chars.len() - 1 - s]
    {
        s += 1;
    }

    let del = cur_chars.len() - p - s;
    let ins: String = new_chars[p..new_chars.len() - s].iter().collect();
    let ins_len = new_chars.len() - s - p;
    let _ = doc.splice_text(text_id, p, del as isize, &ins);
    TextEdit {
        pos: p,
        del,
        ins: ins_len,
    }
}

/// Creates the `Text` object on first use (inserting the whole value);
/// subsequent calls splice only the diff.
pub fn put_text_diffed<P: Into<Prop>>(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: P,
    new: &str,
) -> TextEdit {
    let prop = key.into();
    let tid = match doc.get(obj, prop.clone()) {
        Ok(Some((Value::Object(ObjType::Text), id))) => id,
        _ => doc
            .put_object(obj, prop, ObjType::Text)
            .expect("put_object text"),
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
            _ => None,
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
