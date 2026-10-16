//! Collaborative plain text: a SQL buffer, a report note, a commit message,
//! a source file.
//!
//! [`PlainText`] is a resource whose state is one string, stored as an
//! Automerge text object, and whose edits are [`TextEdit`] splices — the
//! characters typed, not the whole text — so two people typing in the same
//! text merge character by character, and typing a word is one undo step.
//!
//! On its own a record's text lives under the root key [`TEXT_KEY`]. Inside
//! another resource it is a child ([`crate::child`]): the parent's action
//! carries one [`TextEdit`] and finds the text object it applies under with
//! [`TextEdit::apply_under`].

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, ActionParam, ParamType};

use crate::model::{Doc, ModelError};
use crate::resource::{ActionDecl, Resource, SearchEntry};

/// The root key a record's own [`PlainText`] is stored under.
pub const TEXT_KEY: &str = "text";

/// The mark naming who wrote a run of text: the author of the action that
/// inserted it. Collaborators' views highlight what each person wrote.
pub const AUTHOR_MARK: &str = "author";

/// Mark `inserted`, just written at the document text index `at` of the text
/// object `obj`, as written by the author of the action applying now. Nothing
/// is marked outside an action or for an action with no invoker.
///
/// # Errors
/// Returns the document's error when the mark cannot be written.
pub fn mark_authored(
    doc: &mut impl Transactable,
    obj: &ObjId,
    at: usize,
    inserted: &str,
) -> Result<(), automerge::AutomergeError> {
    let Some(author) = tilo_actions::invoking_context()
        .and_then(|context| context.invoker)
        .map(|invoker| invoker.actor)
    else {
        return Ok(());
    };
    tilo_automerge::mark_text_author(doc, obj, at, inserted, &author)
}

/// The registry type id [`TextEdit`]s replay under.
pub const TEXT_ACTION_TYPE: &str = "tilo.text";

/// Plain text, edited a splice at a time.
pub struct PlainText;

/// One edit of plain text. Positions count characters (Unicode scalar
/// values), whatever the document's own text encoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum TextEdit {
    /// Replace `delete` characters at `at` with `insert`: typing is a
    /// splice that deletes nothing, Backspace one that inserts nothing.
    Splice {
        at: usize,
        delete: usize,
        insert: String,
    },
    /// Replace the whole text with `text`, written as the smallest splice
    /// that turns what the document holds into it, so a collaborator's
    /// concurrent typing elsewhere survives.
    ReplaceText { text: String },
}

impl PlainText {
    /// The text in the text object `obj`.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `obj` is not a text object.
    pub fn read_under(doc: &Doc, obj: &ObjId) -> Result<String, ModelError> {
        doc.text(obj)
            .map_err(|error| ModelError::invalid(format!("not a text: {error}")))
    }

    /// The text object of a record's own text, if it has one.
    pub fn root_text(doc: &Doc) -> Option<ObjId> {
        match doc.get(ROOT, TEXT_KEY) {
            Ok(Some((Value::Object(ObjType::Text), obj))) => Some(obj),
            _ => None,
        }
    }
}

/// Where `chars` characters into `text` falls in the document's own text
/// index.
fn index(doc: &Doc, text: &str, chars: usize) -> usize {
    let prefix: &str = text
        .char_indices()
        .nth(chars)
        .and_then(|(byte, _)| text.get(..byte))
        .unwrap_or(text);
    match doc.text_encoding() {
        automerge::TextEncoding::Utf8CodeUnit => prefix.len(),
        automerge::TextEncoding::Utf16CodeUnit => prefix.encode_utf16().count(),
        _ => prefix.chars().count(),
    }
}

/// The smallest splice, in characters, that turns `before` into `after`:
/// what the two share at their start and end is kept.
#[must_use]
pub fn splice_between(before: &str, after: &str) -> Option<TextEdit> {
    if before == after {
        return None;
    }
    let before: Vec<char> = before.chars().collect();
    let after: Vec<char> = after.chars().collect();
    let start = before
        .iter()
        .zip(&after)
        .take_while(|(left, right)| left == right)
        .count();
    let before_rest = before.get(start..).unwrap_or_default();
    let after_rest = after.get(start..).unwrap_or_default();
    let end = before_rest
        .iter()
        .rev()
        .zip(after_rest.iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    Some(TextEdit::Splice {
        at: start,
        delete: before_rest.len().saturating_sub(end),
        insert: after_rest
            .get(..after_rest.len().saturating_sub(end))
            .unwrap_or_default()
            .iter()
            .collect(),
    })
}

impl TextEdit {
    /// Apply this edit to the text object `obj` of another resource.
    ///
    /// # Errors
    /// Returns [`ActionError::Refused`] when `obj` is not a text object or
    /// the splice reaches past the end of the text; nothing is written.
    pub fn apply_under(&self, doc: &mut Doc, obj: &ObjId) -> Result<(), ActionError> {
        let current = doc
            .text(obj)
            .map_err(|_| ActionError::Refused("the text is gone".into()))?;
        let (at, delete, insert) = match self {
            Self::Splice { at, delete, insert } => (*at, *delete, insert.clone()),
            Self::ReplaceText { text } => match splice_between(&current, text) {
                Some(Self::Splice { at, delete, insert }) => (at, delete, insert),
                _ => return Ok(()),
            },
        };
        let length = current.chars().count();
        if at > length || delete > length.saturating_sub(at) {
            return Err(ActionError::Refused(format!(
                "the text has {length} characters; the edit reaches past its end"
            )));
        }
        let start = index(doc, &current, at);
        let end = index(doc, &current, at.saturating_add(delete));
        let removed = isize::try_from(end.saturating_sub(start))
            .map_err(|_| ActionError::Refused("the edit is too large".into()))?;
        doc.splice_text(obj, start, removed, &insert)?;
        mark_authored(doc, obj, start, &insert)?;
        Ok(())
    }
}

impl tilo_actions::Action for TextEdit {
    /// A record's own text, created by the first edit.
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let obj = match PlainText::root_text(doc) {
            Some(obj) => obj,
            None => doc.put_object(ROOT, TEXT_KEY, ObjType::Text)?,
        };
        self.apply_under(doc, &obj)
    }
}

impl Resource for PlainText {
    const KIND: &'static str = "tilo.text";
    const LABEL: &'static str = "Text";
    const ACTION_TYPE: &'static str = TEXT_ACTION_TYPE;

    type State = String;
    type Action = TextEdit;

    fn read(doc: &Doc) -> Result<String, ModelError> {
        PlainText::root_text(doc)
            .map_or_else(|| Ok(String::new()), |obj| Self::read_under(doc, &obj))
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        if PlainText::root_text(doc).is_none() {
            doc.put_object(ROOT, TEXT_KEY, ObjType::Text)
                .map_err(|error| ModelError::invalid(error.to_string()))?;
        }
        Ok(())
    }

    /// Typing a word is one undo step: an insertion right where the last one
    /// ended continues it until a space ends the word, and deleting runs the
    /// same way, backwards (Backspace) or forwards (Delete).
    fn coalesces(previous: &TextEdit, next: &TextEdit) -> bool {
        let (
            TextEdit::Splice {
                at: before_at,
                delete: before_delete,
                insert: before_insert,
            },
            TextEdit::Splice { at, delete, insert },
        ) = (previous, next)
        else {
            return false;
        };
        match (
            *before_delete,
            *delete,
            before_insert.is_empty(),
            insert.is_empty(),
        ) {
            (0, 0, false, false) => {
                *at == before_at.saturating_add(before_insert.chars().count())
                    && !before_insert.ends_with(char::is_whitespace)
            }
            (1.., 1.., true, true) => at.saturating_add(*delete) == *before_at || at == before_at,
            _ => false,
        }
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "replace-text",
                "Replace text",
                vec![ActionParam::required(
                    "text",
                    ParamType::Text,
                    "The whole new text",
                )],
                |args, _| {
                    Ok(TextEdit::ReplaceText {
                        text: args.text("text").unwrap_or_default().to_owned(),
                    })
                },
            )
            .describe("Replace the whole text; only what differs is written."),
        ]
    }

    fn action_schema() -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "oneOf": [
                {
                    "type": "object",
                    "description": "Replace `delete` characters at `at` with `insert`",
                    "properties": {
                        "action": {"const": "splice"},
                        "at": {"type": "integer", "minimum": 0},
                        "delete": {"type": "integer", "minimum": 0},
                        "insert": {"type": "string"}
                    },
                    "required": ["action", "at", "delete", "insert"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "description": "Replace the whole text",
                    "properties": {
                        "action": {"const": "replace-text"},
                        "text": {"type": "string"}
                    },
                    "required": ["action", "text"],
                    "additionalProperties": false
                }
            ]
        }))
    }

    fn search(state: &String) -> Vec<SearchEntry> {
        let title = state.lines().next().unwrap_or_default().trim();
        if title.is_empty() {
            return Vec::new();
        }
        vec![SearchEntry::new(title, state.clone())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Replica;
    use tilo_automerge::ApplyIncoming;

    fn spliced(text: &str, edit: &TextEdit) -> String {
        let mut doc = Doc::new();
        let obj = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&obj, 0, 0, text).unwrap();
        edit.apply_under(&mut doc, &obj).unwrap();
        doc.text(&obj).unwrap()
    }

    #[test]
    fn each_authors_typing_is_marked_with_its_author() {
        use automerge::marks::Mark;
        let typed_by = |doc: &mut Doc, who: &str, edit: TextEdit| {
            let encoded = tilo_actions::EncodedAction::encode(TEXT_ACTION_TYPE, &edit).unwrap();
            let context = tilo_actions::ActionContext::for_resource("text", "tilo.text", "Text")
                .with_invoker(tilo_actions::ActionInvoker::actor(who));
            tilo_actions::apply_local_with_context(doc, &edit, encoded, None, context).unwrap();
        };
        let mut doc = Doc::new();
        typed_by(
            &mut doc,
            "alice",
            TextEdit::Splice {
                at: 0,
                delete: 0,
                insert: "héllo".into(),
            },
        );
        typed_by(
            &mut doc,
            "bob",
            TextEdit::Splice {
                at: 5,
                delete: 0,
                insert: " wörld".into(),
            },
        );
        let obj = PlainText::root_text(&doc).unwrap();
        let text = doc.text(&obj).unwrap();
        assert_eq!(text, "héllo wörld");
        let runs: Vec<(String, String)> = doc
            .marks(&obj)
            .unwrap()
            .into_iter()
            .filter(|mark: &Mark| mark.name() == AUTHOR_MARK)
            .map(|mark| {
                let run: String = text
                    .chars()
                    .skip(mark.start)
                    .take(mark.end.saturating_sub(mark.start))
                    .collect();
                (mark.value().as_str().unwrap_or_default().to_owned(), run)
            })
            .collect();
        assert_eq!(
            runs,
            vec![
                ("alice".to_owned(), "héllo".to_owned()),
                ("bob".to_owned(), " wörld".to_owned()),
            ],
            "each person's typing carries their name"
        );

        // An edit applied outside an invoked action marks nothing.
        let mut plain = Doc::new();
        let obj = plain.put_object(ROOT, "body", ObjType::Text).unwrap();
        TextEdit::Splice {
            at: 0,
            delete: 0,
            insert: "x".into(),
        }
        .apply_under(&mut plain, &obj)
        .unwrap();
        assert!(plain.marks(&obj).unwrap().is_empty());
    }

    #[test]
    fn a_splice_counts_characters_not_bytes() {
        let edit = TextEdit::Splice {
            at: 2,
            delete: 1,
            insert: "ö".into(),
        };
        assert_eq!(spliced("naïve", &edit), "naöve");
    }

    #[test]
    fn a_splice_past_the_end_is_refused_and_writes_nothing() {
        let mut doc = Doc::new();
        let obj = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&obj, 0, 0, "abc").unwrap();
        let edit = TextEdit::Splice {
            at: 2,
            delete: 5,
            insert: String::new(),
        };
        assert!(matches!(
            edit.apply_under(&mut doc, &obj),
            Err(ActionError::Refused(_))
        ));
        assert_eq!(doc.text(&obj).unwrap(), "abc");
    }

    #[test]
    fn replacing_the_text_keeps_a_collaborators_concurrent_typing() {
        let mut ours = Doc::new();
        let obj = ours.put_object(ROOT, "body", ObjType::Text).unwrap();
        ours.splice_text(&obj, 0, 0, "select 1").unwrap();
        let mut theirs = ours.fork();
        theirs.splice_text(&obj, 8, 0, ";").unwrap();
        TextEdit::ReplaceText {
            text: "SELECT 1".into(),
        }
        .apply_under(&mut ours, &obj)
        .unwrap();
        ours.merge_incoming(&mut theirs).unwrap();
        assert_eq!(ours.text(&obj).unwrap(), "SELECT 1;");
    }

    #[test]
    fn the_smallest_splice_keeps_what_both_texts_share() {
        assert_eq!(
            splice_between("hello world", "hello brave world"),
            Some(TextEdit::Splice {
                at: 6,
                delete: 0,
                insert: "brave ".into()
            })
        );
        assert_eq!(
            splice_between("aaa", "aa"),
            Some(TextEdit::Splice {
                at: 2,
                delete: 1,
                insert: String::new()
            })
        );
        assert_eq!(splice_between("same", "same"), None);
    }

    #[test]
    fn typing_a_word_coalesces_until_a_space() {
        let typed = |at: usize, insert: &str| TextEdit::Splice {
            at,
            delete: 0,
            insert: insert.into(),
        };
        assert!(PlainText::coalesces(&typed(0, "h"), &typed(1, "i")));
        assert!(!PlainText::coalesces(&typed(0, "h"), &typed(3, "i")));
        assert!(!PlainText::coalesces(&typed(0, " "), &typed(1, "i")));
        let deleted = |at: usize| TextEdit::Splice {
            at,
            delete: 1,
            insert: String::new(),
        };
        assert!(PlainText::coalesces(&deleted(4), &deleted(3)));
        assert!(!PlainText::coalesces(&typed(4, "x"), &deleted(4)));
    }

    #[test]
    fn a_record_of_its_own_text_replays_its_edits() {
        let mut replica = Replica::<PlainText>::new();
        replica
            .apply(
                crate::Invoke::<PlainText>::apply(&TextEdit::Splice {
                    at: 0,
                    delete: 0,
                    insert: "draft".into(),
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(replica.state().unwrap().as_str(), "draft");
    }
}
