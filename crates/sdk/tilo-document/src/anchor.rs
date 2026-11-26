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
    tilo_resource::__own_table!(TextAnchor);

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
