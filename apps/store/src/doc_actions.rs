//! Replayable Store-document actions (report drafts and moderation notes).

use automerge::ROOT;
use automerge::transaction::Transactable;
use qos_actions::{Action, ActionError, ActionRegistry, Doc};
use serde::{Deserialize, Serialize};

pub const STORE_DOCUMENT_ACTION_TYPE: &str = "store.document";
pub const REPORT_RESOURCE_KEY_PREFIX: &str = "store.report_resource.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreDocumentAction {
    CreateReportDraft { app_id: String },
}

impl Action for StoreDocumentAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::CreateReportDraft { app_id } => {
                let app_id = app_id.trim();
                if app_id.is_empty() || app_id.len() > 255 {
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
    use automerge::ReadDoc;

    #[test]
    fn report_draft_is_a_replayable_document_resource() {
        let mut doc = Doc::new();
        StoreDocumentAction::CreateReportDraft {
            app_id: "dev.qos.notes".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(
            doc.get(&ROOT, "store.report_resource.dev.qos.notes")
                .unwrap()
                .is_some()
        );
    }
}
