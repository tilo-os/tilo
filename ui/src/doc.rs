//! CRDT write helpers that keep Automerge documents from churning.
//!
//! # Why this module exists
//!
//! The naive way to reflect an in-memory string into a CRDT `Text` field is to
//! delete everything and reinsert it:
//!
//! ```ignore
//! let cur = doc.text(&tid).unwrap_or_default();
//! doc.splice_text(&tid, 0, cur.chars().count() as isize, new_value);
//! ```
//!
//! That turns a single keystroke into O(len) operations and — because Automerge
//! keeps every inserted element as a tombstone forever — accumulates O(len²) ops
//! over a typing session. Each subsequent store gets slower until the app
//! appears to freeze. HIG §6 ("No unbounded per-frame work") forbids it.
//!
//! The fix is to diff the current text against the new value down to the common
//! prefix + suffix and splice **only the changed middle span**, so appending one
//! character costs one op, not `len`. [`splice_text_diffed`] /
//! [`put_text_diffed`] do exactly that, and [`put_if_changed`] does the scalar
//! analogue (write an op only when the value actually differs).
//!
//! This is the shared promotion of the pattern first proven in the calculator's
//! `doc.rs`; every text-heavy system app should route its CRDT writes through
//! here rather than re-implementing (and re-regressing) the diff.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value};

/// The character-range splice that [`splice_text_diffed`] actually applied,
/// expressed in *characters* (Automerge `Text` is char-addressed).
///
/// A no-op (the new value already equals the current text) is reported as
/// `TextEdit { pos: 0, del: 0, ins: 0 }` — see [`TextEdit::is_noop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEdit {
    /// Char offset where the splice began (== length of the common prefix).
    pub pos: usize,
    /// Number of chars deleted at `pos`.
    pub del: usize,
    /// Number of chars inserted at `pos`.
    pub ins: usize,
}

impl TextEdit {
    /// Whether nothing was changed (current text already equalled the target).
    pub fn is_noop(&self) -> bool {
        self.del == 0 && self.ins == 0
    }
}

/// Splice only the changed middle span of an existing `Text` object so that it
/// ends up equal to `new`.
///
/// Computes the longest common prefix and (non-overlapping) suffix between the
/// object's current text and `new`, then issues a single `splice_text` covering
/// just the differing middle. Appending one character is one op; an unchanged
/// value emits no op at all. Returns the [`TextEdit`] that was applied.
///
/// This is the anti-churn core — prefer [`put_text_diffed`] when you have a
/// parent object + key and want the `Text` created on first use.
pub fn splice_text_diffed(doc: &mut AutoCommit, text_id: &ObjId, new: &str) -> TextEdit {
    let cur = doc.text(text_id).unwrap_or_default();
    if cur == new {
        return TextEdit { pos: 0, del: 0, ins: 0 };
    }
    let cur_chars: Vec<char> = cur.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();

    // Longest common prefix.
    let mut p = 0;
    while p < cur_chars.len() && p < new_chars.len() && cur_chars[p] == new_chars[p] {
        p += 1;
    }
    // Longest common suffix that doesn't overlap the prefix on either side.
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
    TextEdit { pos: p, del, ins: ins_len }
}

/// Ensure a `Text` object exists at `obj[key]`, then diff-splice `new` into it,
/// touching only the changed span. Creates the `Text` object on first use.
///
/// `key` may be a map key (`&str`/`String`) or a list index (`usize`) — anything
/// that is `Into<Prop>`. Returns the [`TextEdit`] that was applied (the initial
/// create inserts the whole value; subsequent calls splice only the diff).
///
/// This is the shared replacement for the whole-text-rewrite pattern
/// `splice_text(id, 0, cur_len, value)`.
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

/// Put a scalar at `obj[key]` **only if it differs** from what the document
/// already holds. Returns `true` if an op was emitted, `false` if the value was
/// already current (a no-op that leaves the document byte-for-byte unchanged).
///
/// Re-`put`ting an unchanged scalar every frame permanently grows the document;
/// this guard makes an unchanged re-store free. For `f64` fields that can hold
/// `NaN`, keep a bit-exact guard at the call site instead (`NaN != NaN` would
/// make this re-emit forever).
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
        // The core anti-churn assertion: editing one char in a long text
        // produces a splice whose length is the *changed span* (one char),
        // NOT the whole text.
        let mut doc = AutoCommit::new();
        let long = "abcdefghijklmnopqrstuvwxyz0123456789"; // 36 chars
        let id = seed_text(&mut doc, "t", long);
        let mut target = long.to_string();
        target.push('!');

        let edit = splice_text_diffed(&mut doc, &id, &target);

        assert_eq!(
            edit,
            TextEdit { pos: long.chars().count(), del: 0, ins: 1 },
            "appending one char must splice exactly one char, not rewrite the text"
        );
        // The insert span is one char even though the text is 36 chars long.
        assert!(edit.ins < long.chars().count());
        assert_eq!(doc.text(&id).unwrap(), target, "must round-trip to the new value");
    }

    #[test]
    fn middle_edit_splices_only_the_changed_span() {
        let mut doc = AutoCommit::new();
        let before = "the quick brown fox";
        let id = seed_text(&mut doc, "t", before);

        // "quick" -> "slow": common prefix "the ", common suffix " brown fox".
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
        assert_eq!(edit, TextEdit { pos: 0, del: 0, ins: 0 });
        assert_eq!(
            doc.save().len(),
            before,
            "an idempotent no-op must not grow the document"
        );
    }

    #[test]
    fn put_text_diffed_creates_then_diffs() {
        let mut doc = AutoCommit::new();

        // First call creates the Text object and inserts the whole value.
        let e0 = put_text_diffed(&mut doc, &ROOT, "body", "hello world");
        assert_eq!(e0, TextEdit { pos: 0, del: 0, ins: 11 });

        // Second call appends one char → single-char splice, not a rewrite.
        let e1 = put_text_diffed(&mut doc, &ROOT, "body", "hello world!");
        assert_eq!(e1, TextEdit { pos: 11, del: 0, ins: 1 });

        let (v, tid) = doc.get(&ROOT, "body").unwrap().unwrap();
        assert!(matches!(v, Value::Object(ObjType::Text)), "field is a CRDT Text");
        assert_eq!(doc.text(&tid).unwrap(), "hello world!");
    }

    #[test]
    fn typing_char_by_char_keeps_the_doc_bounded() {
        // Reproduces the freeze mechanism on the real write path: append one
        // char at a time and store after each. The wholesale delete+reinsert
        // used to accumulate O(len^2) tombstoned ops; the diff-splice keeps
        // growth ~linear.
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
        assert_eq!(doc.text(&tid).unwrap(), s, "typed value must round-trip exactly");
    }

    #[test]
    fn put_if_changed_guards_redundant_writes() {
        let mut doc = AutoCommit::new();

        assert!(put_if_changed(&mut doc, &ROOT, "n", 1i64), "first write happens");
        let before = doc.save().len();

        // Same value → no write, no growth.
        assert!(!put_if_changed(&mut doc, &ROOT, "n", 1i64), "unchanged re-put is a no-op");
        assert_eq!(doc.save().len(), before, "unchanged re-put must not grow the doc");

        // Changed value → writes.
        assert!(put_if_changed(&mut doc, &ROOT, "n", 2i64), "changed value writes");
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
