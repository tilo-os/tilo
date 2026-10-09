//! Rich text as a field of a record.
//!
//! A record holds rich text the way it holds a string: `pub body: RichText`.
//! The field's map in the document holds the same schema a whole rich-text
//! document does, so everything that reads or edits rich text works on it.
//! Its edits are field edits at its path, carrying a [`RichTextAction`]:
//! `Note::paths().body.perform(&action)`. A value read from a document never
//! writes back; only a new value ([`RichText::from_markdown`]) writes its
//! text when the record is stored.

use automerge::{ObjId, ObjType, Value};
use tilo_resource::{ActionCx, Delta, Doc, EditOp, Model, ModelError, Operations, Segment, Slot};

use crate::{RichText, RichTextAction, RichTextState};

impl RichText {
    /// A new value holding `markdown`, written when the record holding it is
    /// stored.
    #[must_use]
    pub fn from_markdown(markdown: impl Into<String>) -> Self {
        Self {
            state: None,
            initial: Some(markdown.into()),
        }
    }

    /// What views read, once the value has been read from a document.
    pub fn state(&self) -> Option<&RichTextState> {
        self.state.as_ref()
    }

    /// The text this value holds: its body paragraphs that have been read,
    /// one per line, or the markdown of a new value.
    #[must_use]
    pub fn text(&self) -> String {
        if let Some(initial) = &self.initial {
            return initial.clone();
        }
        let Some(state) = &self.state else {
            return String::new();
        };
        state
            .body()
            .all()
            .filter_map(|id| state.block(id))
            .map(|block| block.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn refused(error: tilo_actions::ActionError) -> ModelError {
    match error {
        tilo_actions::ActionError::Refused(reason) => ModelError::Refused(reason),
        other => ModelError::invalid(other.to_string()),
    }
}

impl Operations for RichText {
    type Op = RichTextAction;

    /// Typing on in one paragraph, within a word.
    fn coalesces(previous: &RichTextAction, next: &RichTextAction) -> bool {
        <Self as tilo_resource::Resource>::coalesces(previous, next)
    }

    fn continuable(op: &RichTextAction) -> bool {
        <Self as tilo_resource::Resource>::continuable(op)
    }

    /// The caret after typed text, an inserted paragraph, a split.
    fn lands(&self, op: &RichTextAction) -> Option<tilo_resource::Place> {
        <Self as tilo_resource::Resource>::lands(self.state.as_ref()?, op)
    }
}

impl Model for RichText {
    tilo_resource::leaf_paths!();
    tilo_resource::operations_model!();

    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.text())
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(markdown) => Ok(Self::from_markdown(markdown.clone())),
            serde_json::Value::Null => Ok(Self::default()),
            _ => Err(ModelError::shape("rich text as markdown")),
        }
    }

    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("rich text"));
        }
        Ok(Self {
            state: Some(Self::read_under(doc, id)?),
            initial: None,
        })
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let obj = slot.object(doc, ObjType::Map)?;
        if tilo_doc::model::open(doc, &obj).is_err() {
            Self::initialize_under(doc, &obj)?;
        }
        if let Some(markdown) = &self.initial {
            RichTextAction::ReplaceBody {
                markdown: markdown.clone(),
            }
            .apply_under(doc, &obj)
            .map_err(refused)?;
        }
        Ok(())
    }

    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        match &mut self.state {
            Some(state) if self.initial.is_none() => Self::refresh_under(state, doc, delta),
            _ => {
                *self = Self::load(doc, value, id)?;
                Ok(())
            }
        }
    }

    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        _cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        if let Some(segment) = path.first() {
            return Err(ModelError::invalid(format!(
                "nothing inside rich text is called `{segment}`"
            )));
        }
        let action = match op {
            EditOp::Rich { rich } => serde_json::from_value::<RichTextAction>(rich.clone())
                .map_err(|error| ModelError::invalid(error.to_string()))?,
            EditOp::Replace { text } => RichTextAction::ReplaceBody {
                markdown: text.clone(),
            },
            other => return Err(ModelError::unsupported(other)),
        };
        let obj = slot.object(doc, ObjType::Map)?;
        if tilo_doc::model::open(doc, &obj).is_err() {
            Self::initialize_under(doc, &obj)?;
        }
        action.apply_under(doc, &obj).map_err(refused)
    }

    fn collect_text(&self, out: &mut Vec<String>) {
        let text = self.text();
        if !text.is_empty() {
            out.push(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use automerge::{ROOT, ReadDoc};
    use tilo_resource::{Invoke, Model, Replica, resource};

    use super::*;

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        #[model(label)]
        pub title: String,
        pub body: RichText,
    }

    #[resource(kind = "test.note", label = "Note")]
    impl Note {
        /// Replace the note's text.
        #[action]
        pub fn write(&mut self, markdown: String) {
            self.body = RichText::from_markdown(markdown);
        }
    }

    fn body_text(replica: &Replica<Note>) -> String {
        let Ok(Some((_, body))) = replica.doc().get(ROOT, "body") else {
            return String::new();
        };
        RichText::plain_text_under(replica.doc(), &body).unwrap_or_default()
    }

    fn append(replica: &mut Replica<Note>, text: &str) {
        let edit = Note::paths()
            .body
            .perform(&RichTextAction::AppendParagraph { text: text.into() })
            .unwrap();
        replica.apply(Invoke::edit(&edit).unwrap()).unwrap();
    }

    #[test]
    fn rich_text_is_a_field_edited_at_its_path() {
        let mut note = Replica::<Note>::new();
        assert!(note.state().unwrap().body.state().is_some());
        note.apply(Note::WRITE.with("hello")).unwrap();
        assert_eq!(body_text(&note), "hello");
        append(&mut note, "more");
        assert_eq!(body_text(&note), "hello\nmore");

        let mut other = note.fork();
        append(&mut note, "left");
        append(&mut other, "right");
        note.sync_with(&mut other).unwrap();
        let merged = body_text(&note);
        assert!(
            merged.contains("left") && merged.contains("right"),
            "{merged}"
        );
        assert_eq!(merged, body_text(&other));
        let fresh = note.state().unwrap();
        assert_eq!(note.live().unwrap(), &fresh);
    }

    #[test]
    fn rich_edits_coalesce_land_and_publish_their_vocabulary() {
        use tilo_resource::Resource;
        let splice = |at: usize, ins: &str| {
            let edit = Note::paths()
                .body
                .perform(&RichTextAction::SpliceText {
                    region: crate::action::RegionKind::Body,
                    block: "p1".into(),
                    at,
                    del: 0,
                    ins: ins.into(),
                })
                .unwrap();
            Note::edit_action(edit).unwrap()
        };
        assert!(Note::coalesces(&splice(0, "h"), &splice(1, "i")));
        assert!(!Note::coalesces(&splice(0, "h "), &splice(2, "i")));
        assert!(Note::continuable(&splice(0, "h")));

        let mut note = Replica::<Note>::new();
        note.apply(Note::WRITE.with("hello")).unwrap();
        let state = note.state().unwrap();
        let block = state
            .body
            .state()
            .unwrap()
            .body()
            .id_at(0)
            .unwrap()
            .to_owned();
        let typed = Note::edit_action(
            Note::paths()
                .body
                .perform(&RichTextAction::SpliceText {
                    region: crate::action::RegionKind::Body,
                    block: block.clone(),
                    at: 5,
                    del: 0,
                    ins: "!".into(),
                })
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            Note::lands(&state, &typed),
            Some(tilo_resource::Place::Caret { item, at: 6 }) if item == block
        ));

        let surface = Note::surface();
        let body = surface
            .fields
            .iter()
            .find(|field| field.path == "body")
            .unwrap();
        assert!(body.ops.is_some());
        let tree = tilo_resource::publish::<Note>("note", "Note", &state);
        let edit = tree
            .actions
            .iter()
            .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
            .unwrap();
        let schema = serde_json::to_string(edit.params()[0].schema.as_deref().unwrap()).unwrap();
        assert!(schema.contains("SpliceText"), "{schema}");
    }
}
