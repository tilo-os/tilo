//! Replayable Store-document actions for durable, collaborative reports.

use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionRegistry, Doc};

pub const STORE_DOCUMENT_ACTION_TYPE: &str = "store.document";
pub const REPORT_RESOURCE_KEY_PREFIX: &str = "store.report_resource.";

fn valid_app_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreDocumentAction {
    CreateReportDraft { app_id: String },
    SetReportNote { app_id: String, note: String },
}

impl Action for StoreDocumentAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::CreateReportDraft { app_id } => {
                let app_id = app_id.trim();
                if !valid_app_id(app_id) {
                    return Err(ActionError::Refused(
                        "invalid app id for report draft".into(),
                    ));
                }
                doc.put(
                    &ROOT,
                    format!("{REPORT_RESOURCE_KEY_PREFIX}{app_id}"),
                    "Draft · not submitted",
                )?;
            }
            Self::SetReportNote { app_id, note } => {
                let app_id = app_id.trim();
                if !valid_app_id(app_id) || note.chars().count() > 4_096 {
                    return Err(ActionError::Refused("invalid report note".into()));
                }
                // An absolute agent operation replaces the text object rather
                // than splicing an existing one. Concurrent absolute writes
                // therefore resolve to one authored value instead of
                // interleaving sentences; ordinary on-screen typing remains
                // character-granular CRDT text within the chosen object.
                let object =
                    doc.put_object(&ROOT, format!("store.report_note.{app_id}"), ObjType::Text)?;
                if !note.is_empty() {
                    doc.splice_text(&object, 0, 0, note)?;
                }
            }
        }
        Ok(())
    }
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<StoreDocumentAction>(STORE_DOCUMENT_ACTION_TYPE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ReadDoc, Value};

    fn note(doc: &Doc) -> String {
        doc.get(&ROOT, "store.report_note.so.tilo.notes")
            .unwrap()
            .and_then(|(value, id)| matches!(value, Value::Object(ObjType::Text)).then_some(id))
            .and_then(|id| doc.text(&id).ok())
            .unwrap_or_default()
    }
    #[test]
    fn report_draft_is_a_replayable_document_resource() {
        let mut doc = Doc::new();
        StoreDocumentAction::CreateReportDraft {
            app_id: "so.tilo.notes".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(
            doc.get(&ROOT, "store.report_resource.so.tilo.notes")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn absolute_report_note_is_bounded_and_replayable() {
        let action = StoreDocumentAction::SetReportNote {
            app_id: "so.tilo.notes".into(),
            note: "The listing omits a network dependency.".into(),
        };
        let mut doc = Doc::new();
        action.apply(&mut doc).unwrap();
        assert_eq!(note(&doc), "The listing omits a network dependency.");
        action.apply(&mut doc).unwrap();
        assert_eq!(note(&doc), "The listing omits a network dependency.");
        assert!(
            StoreDocumentAction::SetReportNote {
                app_id: "so.tilo.notes".into(),
                note: "x".repeat(4_097),
            }
            .apply(&mut Doc::new())
            .is_err()
        );
    }

    #[test]
    fn divergent_absolute_report_notes_converge_without_sentence_interleaving() {
        let mut seed = Doc::new();
        StoreDocumentAction::CreateReportDraft {
            app_id: "so.tilo.notes".into(),
        }
        .apply(&mut seed)
        .unwrap();
        let bytes = seed.save();
        let mut left = Doc::load(&bytes).unwrap();
        let mut right = Doc::load(&bytes).unwrap();
        for (doc, value) in [(&mut left, "left evidence"), (&mut right, "right evidence")] {
            StoreDocumentAction::SetReportNote {
                app_id: "so.tilo.notes".into(),
                note: value.into(),
            }
            .apply(doc)
            .unwrap();
        }
        let mut left_then_right = left.clone();
        left_then_right.merge(&mut right.clone()).unwrap();
        let mut right_then_left = right;
        right_then_left.merge(&mut left).unwrap();
        let resolved = note(&left_then_right);
        assert_eq!(resolved, note(&right_then_left));
        assert!(matches!(
            resolved.as_str(),
            "left evidence" | "right evidence"
        ));
    }
}
