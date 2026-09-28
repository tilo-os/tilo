//! CRDT helpers: writes that keep Automerge documents from churning, and typed reads.
//!
//! Splicing a whole string on every keystroke (delete all, reinsert new) costs
//! O(len) ops per edit and, since Automerge tombstones every inserted element
//! forever, O(len²) accumulated ops over a typing session — the store gets
//! slower until the app appears to freeze.
//!
//! [`splice_text_diffed`] / [`put_text_diffed`] diff old vs. new down to the
//! common prefix + suffix and splice only the changed middle span, so one
//! keystroke costs one op. [`put_if_changed`] is the scalar analogue.
//!
//! The typed readers ([`read_str`], [`read_bool`], [`read_number`], …) are
//! the read side: each returns `None` for a missing or differently typed
//! value and names any conversion it applies, so callers keep their own
//! defaults explicit.

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
pub fn document<'a>(model: &'a mut dyn Any, path: &'a dyn Any) -> (&'a mut AutoCommit, &'a ObjId) {
    let (Some(doc), Some(path)) = (
        model.downcast_mut::<AutoCommit>(),
        path.downcast_ref::<ObjId>(),
    ) else {
        panic!("the host lent a model that is not an Automerge document");
    };
    (doc, path)
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

// Typed readers.
//
// Each reader returns `None` when the property is missing, holds a value of
// another type, or the lookup itself fails, so a caller chooses its own
// default (`unwrap_or_default()`, a documented fallback) at the call site.
// Readers that accept more than one stored representation say so in their
// name and document the conversion.

/// The scalar stored at `prop`, or `None` for a missing property or an object.
pub fn read_scalar<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<ScalarValue> {
    match doc.get(obj, prop) {
        Ok(Some((Value::Scalar(value), _))) => Some(value.into_owned()),
        _ => None,
    }
}

/// A string scalar. A `Text` object at `prop` is not a string scalar; see
/// [`read_str_or_text`].
pub fn read_str<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<String> {
    match read_scalar(doc, obj, prop)? {
        ScalarValue::Str(value) => Some(value.to_string()),
        _ => None,
    }
}

/// The contents of a `Text` object. A string scalar is not a `Text` object;
/// see [`read_str_or_text`].
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

/// A boolean scalar.
pub fn read_bool<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<bool> {
    match read_scalar(doc, obj, prop)? {
        ScalarValue::Boolean(value) => Some(value),
        _ => None,
    }
}

/// An `F64` scalar, exactly. Integers are not converted; see
/// [`read_number`].
pub fn read_f64<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<f64> {
    match read_scalar(doc, obj, prop)? {
        ScalarValue::F64(value) => Some(value),
        _ => None,
    }
}

/// A number stored as `F64`, `Int` or `Uint`, as `f64`. Integers beyond 2^53
/// round to the nearest representable value. Counters and timestamps are not
/// numbers here.
#[allow(
    clippy::cast_precision_loss,
    reason = "documented: integers beyond 2^53 round to the nearest f64"
)]
pub fn read_number<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<f64> {
    match read_scalar(doc, obj, prop)? {
        ScalarValue::F64(value) => Some(value),
        ScalarValue::Int(value) => Some(value as f64),
        ScalarValue::Uint(value) => Some(value as f64),
        _ => None,
    }
}

/// An integer through Automerge's own [`ScalarValue::to_i64`] coercion:
/// `Int`, `Counter` and `Timestamp` as stored, `Uint` reinterpreted as `i64`
/// (values above `i64::MAX` wrap), and `F64` truncated toward zero
/// (saturating, `NaN` as 0).
pub fn read_i64_coerced<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<i64> {
    read_scalar(doc, obj, prop)?.as_i64()
}

/// A number through Automerge's own [`ScalarValue::to_f64`] coercion:
/// `F64` as stored, and `Int`, `Uint`, `Counter` and `Timestamp` widened to
/// `f64`. See [`read_number`] for plain numbers only.
pub fn read_f64_coerced<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<f64> {
    read_scalar(doc, obj, prop)?.as_f64()
}

/// An unsigned integer stored as `Uint`, or as a non-negative `Int`.
pub fn read_u64<P: Into<Prop>>(doc: &impl ReadDoc, obj: &ObjId, prop: P) -> Option<u64> {
    match read_scalar(doc, obj, prop)? {
        ScalarValue::Uint(value) => Some(value),
        ScalarValue::Int(value) => u64::try_from(value).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    fn reader_fixture() -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "str", "plain").unwrap();
        seed_text(&mut doc, "text", "rich");
        doc.put(&ROOT, "yes", true).unwrap();
        doc.put(&ROOT, "float", 2.5_f64).unwrap();
        doc.put(&ROOT, "int", -7_i64).unwrap();
        doc.put(&ROOT, "uint", 9_u64).unwrap();
        doc.put(&ROOT, "huge", u64::MAX).unwrap();
        doc.put(&ROOT, "stamp", ScalarValue::Timestamp(42)).unwrap();
        doc.put_object(&ROOT, "map", ObjType::Map).unwrap();
        doc
    }

    #[test]
    fn readers_distinguish_types_and_apply_documented_coercions() {
        // All readers are pure, so one fixture serves every probe.
        let doc = reader_fixture();
        // String readers distinguish scalars from text objects.
        assert_eq!(read_str(&doc, &ROOT, "str").as_deref(), Some("plain"));
        assert_eq!(read_str(&doc, &ROOT, "text"), None);
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

        // Scalar readers report a mismatched type as absent.
        assert_eq!(read_bool(&doc, &ROOT, "yes"), Some(true));
        assert_eq!(read_bool(&doc, &ROOT, "int"), None);
        assert_eq!(read_f64(&doc, &ROOT, "float"), Some(2.5));
        assert_eq!(read_f64(&doc, &ROOT, "int"), None, "exact F64 only");
        assert_eq!(read_scalar(&doc, &ROOT, "map"), None);
        assert_eq!(read_scalar(&doc, &ROOT, "absent"), None);
        assert_eq!(read_scalar(&doc, &ROOT, "uint"), Some(ScalarValue::Uint(9)));

        // Numeric readers apply their documented coercions.
        assert_eq!(read_number(&doc, &ROOT, "float"), Some(2.5));
        assert_eq!(read_number(&doc, &ROOT, "int"), Some(-7.0));
        assert_eq!(read_number(&doc, &ROOT, "uint"), Some(9.0));
        assert_eq!(read_number(&doc, &ROOT, "stamp"), None);
        assert_eq!(read_number(&doc, &ROOT, "str"), None);

        assert_eq!(read_i64_coerced(&doc, &ROOT, "int"), Some(-7));
        assert_eq!(read_i64_coerced(&doc, &ROOT, "float"), Some(2));
        assert_eq!(
            read_i64_coerced(&doc, &ROOT, "huge"),
            Some(-1),
            "Uint wraps"
        );
        assert_eq!(read_i64_coerced(&doc, &ROOT, "stamp"), Some(42));
        assert_eq!(read_i64_coerced(&doc, &ROOT, "yes"), None);

        assert_eq!(read_f64_coerced(&doc, &ROOT, "int"), Some(-7.0));
        assert_eq!(read_f64_coerced(&doc, &ROOT, "stamp"), Some(42.0));
        assert_eq!(read_f64_coerced(&doc, &ROOT, "str"), None);

        assert_eq!(read_u64(&doc, &ROOT, "uint"), Some(9));
        assert_eq!(read_u64(&doc, &ROOT, "huge"), Some(u64::MAX));
        assert_eq!(read_u64(&doc, &ROOT, "int"), None, "negative Int");
        assert_eq!(read_u64(&doc, &ROOT, "float"), None);

        // Readers accept list indices.
        let mut doc = AutoCommit::new();
        let list = doc.put_object(&ROOT, "list", ObjType::List).unwrap();
        doc.insert(&list, 0, "first").unwrap();
        assert_eq!(read_str(&doc, &list, 0).as_deref(), Some("first"));
        assert_eq!(read_str(&doc, &list, 1), None);
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

    #[test]
    fn put_if_changed_skips_redundant_writes_for_every_scalar_kind() {
        // Integers: the first write happens, an unchanged re-put is a no-op.
        {
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
                Value::Scalar(v) => v.as_i64(),
                Value::Object(_) => None,
            };
            assert_eq!(got, Some(2));
        }

        // Strings and bools.
        {
            let mut doc = AutoCommit::new();
            assert!(put_if_changed(&mut doc, &ROOT, "s", "a"));
            assert!(!put_if_changed(&mut doc, &ROOT, "s", "a"));
            assert!(put_if_changed(&mut doc, &ROOT, "s", "b"));

            assert!(put_if_changed(&mut doc, &ROOT, "flag", true));
            assert!(!put_if_changed(&mut doc, &ROOT, "flag", true));
            assert!(put_if_changed(&mut doc, &ROOT, "flag", false));
        }
    }
}
