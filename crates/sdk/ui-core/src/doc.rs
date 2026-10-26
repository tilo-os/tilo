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
use tilo_resource::text::TextSpliceError;

/// Where `doc` stands, to tell whether a settle pass changed it.
pub(crate) fn settle_mark(doc: &AutoCommit) -> (Vec<automerge::ChangeHash>, usize) {
    (doc.base_heads(), doc.pending_ops())
}

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
/// The diff is computed in characters (what [`TextEdit`] reports) and applied
/// through [`tilo_resource::text::splice_chars`], the one mutation plain text
/// and table cells share: it converts the character offsets to the document's
/// own text index (a UTF-8 or UTF-16 document splices in the right place) and
/// marks what it inserts with the author of the action applying now.
///
/// # Errors
/// Returns [`TextSpliceError`] instead of reporting an edit it did not make:
/// the object is not text any more, the document refuses the splice, or the
/// text changed under a concurrent writer such that the diff no longer
/// applies. Nothing is written when it returns an error.
pub fn splice_text_diffed(
    doc: &mut AutoCommit,
    text_id: &ObjId,
    new: &str,
) -> Result<TextEdit, TextSpliceError> {
    let cur = tilo_resource::text::text_of(doc, text_id)?;
    let (at, delete, insert) = match tilo_resource::text::splice_between(&cur, new) {
        Some(tilo_resource::text::TextEdit::Splice { at, delete, insert }) => (at, delete, insert),
        // Identical text (or a form this helper does not write): nothing to do.
        _ => {
            return Ok(TextEdit {
                pos: 0,
                del: 0,
                ins: 0,
            });
        }
    };
    let ins = insert.chars().count();
    tilo_resource::text::splice_chars(doc, text_id, at, delete, &insert)?;
    Ok(TextEdit {
        pos: at,
        del: delete,
        ins,
    })
}

/// Creates the `Text` object on first use (inserting the whole value);
/// subsequent calls splice only the diff.
///
/// # Errors
/// Returns [`TextSpliceError`] instead of reporting a no-op edit: `obj` was
/// concurrently deleted (or retyped) by another collaborator between the read
/// and the write, so there is nowhere to put the text; or the splice into an
/// existing text refused (see [`splice_text_diffed`]).
pub fn put_text_diffed<P: Into<Prop>>(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: P,
    new: &str,
) -> Result<TextEdit, TextSpliceError> {
    let prop = key.into();
    let tid = match doc.get(obj, prop.clone()) {
        Ok(Some((Value::Object(ObjType::Text), id))) => id,
        _ => doc.put_object(obj, prop, ObjType::Text)?,
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

            let edit = splice_text_diffed(&mut doc, &id, &target).expect("the text splices");

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

            let edit =
                splice_text_diffed(&mut doc, &id, "the slow brown fox").expect("the text splices");

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

            let edit = splice_text_diffed(&mut doc, &id, "unchanged").expect("the text splices");

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

            let e0 =
                put_text_diffed(&mut doc, &ROOT, "body", "hello world").expect("the text writes");
            assert_eq!(
                e0,
                TextEdit {
                    pos: 0,
                    del: 0,
                    ins: 11
                }
            );

            let e1 =
                put_text_diffed(&mut doc, &ROOT, "body", "hello world!").expect("the text splices");
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

        {
            let mut doc = AutoCommit::new();
            let mut s = String::new();
            for i in 0..500 {
                s.push(if i % 7 == 6 { ' ' } else { 'x' });
                put_text_diffed(&mut doc, &ROOT, "b", &s).expect("the text splices");
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

    /// A character is a character: multi-byte, combining and astral text
    /// splices at the same character offset whatever the document's own text
    /// encoding counts.
    #[test]
    fn a_unicode_splice_counts_characters_whatever_the_document_encoding() {
        for encoding in [
            automerge::TextEncoding::UnicodeCodePoint,
            automerge::TextEncoding::Utf8CodeUnit,
            automerge::TextEncoding::Utf16CodeUnit,
        ] {
            let mut doc = AutoCommit::new_with_encoding(encoding);
            let id = seed_text(&mut doc, "t", "naïve café 🎨");

            let replaced =
                splice_text_diffed(&mut doc, &id, "naïve Kaffee 🎨").expect("the text splices");
            assert_eq!(
                replaced,
                TextEdit {
                    pos: 6,
                    del: 4,
                    ins: 6
                },
                "{encoding:?}: `café` is four characters"
            );
            assert_eq!(doc.text(&id).unwrap(), "naïve Kaffee 🎨");

            // An astral character plus a combining mark inserted after `naï`.
            let inserted = splice_text_diffed(&mut doc, &id, "naïe\u{301}🖌ve Kaffee 🎨")
                .expect("the text splices");
            assert_eq!(
                inserted,
                TextEdit {
                    pos: 3,
                    del: 0,
                    ins: 3
                },
                "{encoding:?}: the inserted run counts its combining mark and emoji once each"
            );
            assert_eq!(doc.text(&id).unwrap(), "naïe\u{301}🖌ve Kaffee 🎨");
        }
    }

    /// A splice that cannot be made says so instead of reporting an edit:
    /// nothing is written and the caller can refuse the action.
    #[test]
    fn a_splice_with_no_text_to_write_is_reported() {
        let mut doc = AutoCommit::new();
        let map = doc.put_object(&ROOT, "map", ObjType::Map).unwrap();
        assert_eq!(
            splice_text_diffed(&mut doc, &map, "anything"),
            Err(TextSpliceError::NotText),
            "a map is not text"
        );
        // A text object cannot hold a child text: creating one refuses.
        let text = seed_text(&mut doc, "t", "typed");
        assert!(matches!(
            put_text_diffed(&mut doc, &text, "child", "value"),
            Err(TextSpliceError::Refused(_))
        ));
        assert_eq!(doc.text(&text).unwrap(), "typed", "nothing was written");
    }

    /// What a splice inserts carries the author of the action applying now,
    /// so the table's cell edits are attributed like any resource's text.
    #[test]
    fn an_inserted_run_is_marked_with_the_applying_actions_author() {
        use automerge::marks::Mark;

        #[derive(Debug, serde::Serialize)]
        struct TypeCell {
            text: String,
        }
        impl tilo_actions::Action for TypeCell {
            fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
                put_text_diffed(doc, &ROOT, "cell", &self.text)
                    .map(drop)
                    .map_err(|error| tilo_actions::ActionError::Refused(error.to_string()))
            }
        }

        let mut doc = AutoCommit::new();
        let context = tilo_actions::ActionContext::for_resource("table", "tilo.table", "Table")
            .with_invoker(tilo_actions::ActionInvoker::actor("alice"));
        tilo_actions::apply_local_with_context(
            &mut doc,
            &TypeCell {
                text: "héllo".into(),
            },
            tilo_actions::EncodedAction::encode(
                "tilo.table",
                &TypeCell {
                    text: "héllo".into(),
                },
            )
            .unwrap(),
            None,
            context.clone(),
        )
        .expect("the action applies");
        let (_, id) = doc.get(&ROOT, "cell").unwrap().unwrap();
        let runs: Vec<(String, usize, usize)> = doc
            .marks(&id)
            .unwrap()
            .into_iter()
            .filter(|mark: &Mark| mark.name() == "author")
            .map(|mark| {
                (
                    mark.value().as_str().unwrap_or_default().to_owned(),
                    mark.start,
                    mark.end,
                )
            })
            .collect();
        assert_eq!(
            runs,
            vec![("alice".to_owned(), 0, 5)],
            "the five characters typed are alice's"
        );

        // Outside an action nobody is named, and nothing is marked.
        let mut plain = AutoCommit::new();
        put_text_diffed(&mut plain, &ROOT, "cell", "unattributed").expect("the text writes");
        let (_, id) = plain.get(&ROOT, "cell").unwrap().unwrap();
        assert!(plain.marks(&id).unwrap().is_empty());
    }
}
