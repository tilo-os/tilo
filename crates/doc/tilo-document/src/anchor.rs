//! A range of rich text held by the characters at its ends, as a field of a
//! record: what a comment is about.
//!
//! A [`TextAnchor`] lives outside the rich text it points into, so a record
//! can keep it in a storage unit of its own (a comment thread) and writing it
//! never writes the text. Its ends are Automerge cursors on the first and
//! last character of the range, so edits before, inside and after it move
//! it exactly as they move the characters. A new anchor is given as offsets
//! and placed (its cursors taken) when the record holding it is stored.

use automerge::{Cursor, MoveCursor, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use tilo_resource::{Doc, Model, ModelError, Slot};

use crate::RichText;

/// A range of one paragraph of rich text. See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextAnchor {
    /// The paragraph's block id.
    block: String,
    /// Where the anchor is held, once placed.
    placed: Option<Placed>,
    /// A new anchor: its offsets in the paragraph and the rich text's
    /// block container, placed when stored.
    new: Option<(usize, usize, ObjId)>,
}

/// The paragraph's text object and the cursors on the range's first and
/// last character, as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Placed {
    block: String,
    text: String,
    first: String,
    last: String,
}

impl TextAnchor {
    /// The characters `start..end` of paragraph `block` of `body`, placed
    /// when the record holding the anchor is stored.
    ///
    /// # Errors
    /// Returns why the range cannot be anchored: an empty range, or a body
    /// that has not been read.
    pub fn new(body: &RichText, block: &str, start: usize, end: usize) -> Result<Self, String> {
        if start >= end {
            return Err("an anchor covers at least one character".into());
        }
        let state = body
            .state()
            .ok_or("the text to anchor in has not been read")?;
        Ok(Self {
            block: block.to_owned(),
            placed: None,
            new: Some((start, end, state.containers().blocks.clone())),
        })
    }

    /// The paragraph the range is in.
    #[must_use]
    pub fn block(&self) -> &str {
        &self.block
    }

    /// Where the range is in `doc` now: its paragraph and character offsets.
    /// `None` when its paragraph or every character of it is gone.
    #[must_use]
    pub fn range(&self, doc: &Doc) -> Option<(String, usize, usize)> {
        let placed = self.placed.as_ref()?;
        let text = doc.import_obj(&placed.text).ok()?;
        let position = |cursor: &str| {
            let cursor = Cursor::try_from(cursor).ok()?;
            doc.get_cursor_position(&text, &cursor, None).ok()
        };
        let start = position(&placed.first)?;
        let end = position(&placed.last)?.saturating_add(1);
        (start < end).then(|| (placed.block.clone(), start, end))
    }

    /// Take the cursors of a new anchor's ends in `doc`.
    fn place(&self, doc: &Doc) -> Result<Placed, ModelError> {
        let Some((start, end, blocks)) = &self.new else {
            return self
                .placed
                .clone()
                .ok_or_else(|| ModelError::invalid("an anchor with no place"));
        };
        let refused =
            || ModelError::Refused(format!("there is no text {start}..{end} in {}", self.block));
        let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(blocks, self.block.as_str())
        else {
            return Err(refused());
        };
        let Ok(Some((Value::Object(ObjType::Text), text))) =
            doc.get(&block, tilo_doc::schema::BLOCK_TEXT)
        else {
            return Err(refused());
        };
        if *end > doc.length(&text) {
            return Err(refused());
        }
        let cursor = |at: usize, direction: MoveCursor| {
            doc.get_cursor_moving(&text, at, None, direction)
                .map(|cursor| cursor.to_string())
                .map_err(|_| refused())
        };
        Ok(Placed {
            block: self.block.clone(),
            text: text.to_string(),
            // The ends stay on the range's own characters: text typed just
            // before or after it is not commented.
            first: cursor(*start, MoveCursor::After)?,
            last: cursor(end.saturating_sub(1), MoveCursor::Before)?,
        })
    }
}

impl Model for TextAnchor {
    tilo_resource::leaf_paths!();

    fn to_json(&self) -> serde_json::Value {
        self.placed
            .as_ref()
            .and_then(|placed| serde_json::to_value(placed).ok())
            .unwrap_or(serde_json::Value::Null)
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let placed: Placed = serde_json::from_value(value.clone())
            .map_err(|_| ModelError::shape("a placed text anchor"))?;
        Ok(Self {
            block: placed.block.clone(),
            placed: Some(placed),
            new: None,
        })
    }

    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        let Value::Scalar(scalar) = value else {
            return Err(ModelError::shape("a text anchor"));
        };
        let ScalarValue::Str(stored) = scalar.as_ref() else {
            return Err(ModelError::shape("a text anchor"));
        };
        let placed: Placed =
            serde_json::from_str(stored).map_err(|_| ModelError::shape("a text anchor"))?;
        Ok(Self {
            block: placed.block.clone(),
            placed: Some(placed),
            new: None,
        })
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        if self.placed.is_none() && self.new.is_none() {
            return Ok(());
        }
        let placed = self.place(doc)?;
        let stored = serde_json::to_string(&placed)
            .map_err(|error| ModelError::invalid(error.to_string()))?;
        slot.put_scalar(doc, ScalarValue::Str(stored.into()))
    }
}

#[cfg(test)]
mod tests {
    use tilo_resource::{Collection, Invoke, Model, Replica, resource};

    use super::*;
    use crate::RichTextAction;

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        pub body: RichText,
        #[model(unit)]
        pub remarks: Collection<Remark>,
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Remark {
        pub about: TextAnchor,
        #[model(label)]
        pub text: String,
    }

    #[resource(kind = "test.anchored-note", label = "Note")]
    impl Note {
        /// Remark on some text.
        #[action(writes = remarks)]
        pub fn add_remark(
            &mut self,
            cx: &mut tilo_resource::ActionCx,
            block: String,
            start: usize,
            end: usize,
            text: String,
        ) -> Result<(), tilo_resource::Refusal> {
            let about = TextAnchor::new(&self.body, &block, start, end)
                .map_err(tilo_resource::Refusal::new)?;
            self.remarks.push(cx.id(), Remark { about, text });
            Ok(())
        }
    }

    fn first_block(note: &Replica<Note>) -> String {
        let state = note.state().unwrap();
        state
            .body
            .state()
            .unwrap()
            .body()
            .id_at(0)
            .unwrap()
            .to_owned()
    }

    fn type_at(note: &mut Replica<Note>, block: &str, at: usize, text: &str) {
        let edit = Note::paths()
            .body
            .perform(&RichTextAction::SpliceText {
                region: crate::action::RegionKind::Body,
                block: block.to_owned(),
                at,
                del: 0,
                ins: text.to_owned(),
            })
            .unwrap();
        note.apply(Invoke::edit(&edit).unwrap()).unwrap();
    }

    fn anchored(note: &Replica<Note>) -> Option<(String, usize, usize)> {
        let state = note.state().unwrap();
        let (_, remark) = state.remarks.iter().next()?;
        remark.about.range(note.doc())
    }

    #[test]
    fn an_anchor_follows_its_characters_and_writes_no_text() {
        let mut note = Replica::<Note>::new();
        let block = first_block(&note);
        type_at(&mut note, &block, 0, "hello world");
        let text = |note: &Replica<Note>| {
            let Ok(Some((_, body))) = note.doc().get(automerge::ROOT, "body") else {
                return String::new();
            };
            RichText::plain_text_under(note.doc(), &body).unwrap_or_default()
        };
        note.apply(Note::ADD_REMARK.with(note::AddRemark {
            block: block.clone(),
            start: 6,
            end: 11,
            text: "which one?".into(),
        }))
        .unwrap();
        assert_eq!(anchored(&note), Some((block.clone(), 6, 11)));
        assert_eq!(text(&note), "hello world", "the remark left the text alone");

        // Typing before the range moves it; typing at either edge does not
        // grow it.
        type_at(&mut note, &block, 0, "oh, ");
        assert_eq!(anchored(&note), Some((block.clone(), 10, 15)));
        type_at(&mut note, &block, 15, "!");
        type_at(&mut note, &block, 10, "big ");
        assert_eq!(anchored(&note), Some((block.clone(), 14, 19)));

        let mut other = note.fork();
        type_at(&mut other, &block, 0, ">> ");
        note.sync_with(&mut other).unwrap();
        assert_eq!(anchored(&note), Some((block.clone(), 17, 22)));
        assert_eq!(anchored(&other), anchored(&note));

        assert!(
            note.apply(Note::ADD_REMARK.with(note::AddRemark {
                block: block.clone(),
                start: 3,
                end: 3,
                text: "nothing".into(),
            }))
            .is_err()
        );
        assert!(
            note.apply(Note::ADD_REMARK.with(note::AddRemark {
                block,
                start: 0,
                end: 500,
                text: "too far".into(),
            }))
            .is_err()
        );
    }
}
