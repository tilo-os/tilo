//! Tagged structural edits shared by the CLI, the live graph host, and agents.

use crate::{UiDocument, UiGraphError, UiNode};
use serde::{Deserialize, Serialize};

/// One validated edit to a serialized UI object graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UiGraphEdit {
    Insert {
        parent: String,
        index: usize,
        node: UiNode,
    },
    Remove {
        id: String,
    },
    Replace {
        id: String,
        node: UiNode,
    },
    Move {
        id: String,
        parent: String,
        index: usize,
    },
    /// Set one serialized node property. A dotted path addresses nested
    /// objects such as `text.value` or `navigate.route`.
    SetProperty {
        id: String,
        property: String,
        value: serde_json::Value,
    },
    /// Remove one serialized node property so its schema default applies.
    UnsetProperty {
        id: String,
        property: String,
    },
}

impl UiDocument {
    /// Apply one edit. On failure the document is left unchanged.
    pub fn apply_edit(&mut self, edit: UiGraphEdit) -> Result<(), UiGraphError> {
        match edit {
            UiGraphEdit::Insert {
                parent,
                index,
                node,
            } => self.insert_child(&parent, index, node),
            UiGraphEdit::Remove { id } => {
                let _ = self.remove_node(&id)?;
                Ok(())
            }
            UiGraphEdit::Replace { id, node } => self.replace_node(&id, node),
            UiGraphEdit::Move { id, parent, index } => self.move_node(&id, &parent, index),
            UiGraphEdit::SetProperty {
                id,
                property,
                value,
            } => self.set_property(&id, &property, Some(value)),
            UiGraphEdit::UnsetProperty { id, property } => self.set_property(&id, &property, None),
        }
    }

    /// Apply every edit against one in-memory candidate, then publish once.
    pub fn apply_edits(&mut self, edits: Vec<UiGraphEdit>) -> Result<(), UiGraphError> {
        if edits.is_empty() {
            return Err(UiGraphError::EmptyEditBatch);
        }
        let mut candidate = self.clone();
        for (index, edit) in edits.into_iter().enumerate() {
            candidate
                .apply_edit(edit)
                .map_err(|error| UiGraphError::EditFailed {
                    index: index.saturating_add(1),
                    message: error.to_string(),
                })?;
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Set or clear one serialized property via a dotted JSON path.
    pub fn set_property(
        &mut self,
        id: &str,
        property: &str,
        value: Option<serde_json::Value>,
    ) -> Result<(), UiGraphError> {
        let node = self
            .node(id)
            .cloned()
            .ok_or_else(|| UiGraphError::UnknownNode(id.to_owned()))?;
        let segments: Vec<_> = property.split('.').collect();
        if segments.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
            return Err(UiGraphError::InvalidPropertyPath {
                id: id.to_owned(),
                property: property.to_owned(),
            });
        }
        let mut serialized = serde_json::to_value(node)?;
        let Some((leaf, parents)) = segments.split_last() else {
            return Err(UiGraphError::InvalidPropertyPath {
                id: id.to_owned(),
                property: property.to_owned(),
            });
        };
        let mut current = &mut serialized;
        for segment in parents {
            current = current
                .as_object_mut()
                .and_then(|object| object.get_mut(*segment))
                .ok_or_else(|| UiGraphError::InvalidPropertyPath {
                    id: id.to_owned(),
                    property: property.to_owned(),
                })?;
        }
        let object = current
            .as_object_mut()
            .ok_or_else(|| UiGraphError::InvalidPropertyPath {
                id: id.to_owned(),
                property: property.to_owned(),
            })?;
        if let Some(value) = value {
            object.insert((*leaf).to_owned(), value);
        } else if object.remove(*leaf).is_none() {
            return Err(UiGraphError::UnknownProperty {
                id: id.to_owned(),
                property: property.to_owned(),
            });
        }
        let replacement: UiNode = serde_json::from_value(serialized).map_err(|error| {
            UiGraphError::InvalidPropertyValue {
                id: id.to_owned(),
                property: property.to_owned(),
                message: error.to_string(),
            }
        })?;
        self.replace_node(id, replacement)
    }
}

/// Parse a non-empty JSON array of tagged graph edits.
///
/// # Errors
///
/// Returns a JSON decode error or [`UiGraphError::EmptyEditBatch`].
pub fn parse_ui_graph_edits(source: &str) -> Result<Vec<UiGraphEdit>, UiGraphError> {
    let edits: Vec<UiGraphEdit> = serde_json::from_str(source)?;
    if edits.is_empty() {
        return Err(UiGraphError::EmptyEditBatch);
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UiTextRole, UiTextSource};

    fn sample() -> UiDocument {
        UiDocument::new(UiNode::Column {
            id: "root".into(),
            children: vec![UiNode::Text {
                id: "title".into(),
                text: UiTextSource::Static {
                    value: "Hello".into(),
                },
                role: UiTextRole::Title,
            }],
            gap: None,
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        })
    }

    #[test]
    fn set_property_and_batch_are_atomic() {
        let mut document = sample();
        document
            .set_property("title", "text.value", Some(serde_json::json!("Projects")))
            .unwrap();
        assert!(matches!(
            document.node("title"),
            Some(UiNode::Text {
                text: UiTextSource::Static { value },
                ..
            }) if value == "Projects"
        ));

        let before = document.clone();
        let error = document
            .apply_edits(vec![
                UiGraphEdit::SetProperty {
                    id: "title".into(),
                    property: "text.value".into(),
                    value: serde_json::json!("Recent"),
                },
                UiGraphEdit::SetProperty {
                    id: "missing".into(),
                    property: "text.value".into(),
                    value: serde_json::json!("x"),
                },
            ])
            .unwrap_err();
        assert!(matches!(error, UiGraphError::EditFailed { index: 2, .. }));
        assert_eq!(document, before);
    }
}
