//! Replayable note edits shared by the live app and headless drivers.

use automerge::{ObjType, ROOT, ReadDoc, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionRegistry, Doc, ExecutionLocus};

pub(crate) const EDIT: &str = "so.tilo.scratchpad.edit";

/// Edit the note through a diff, preserving unchanged collaborative text.
#[derive(Debug, Serialize, Deserialize)]
pub struct EditNote {
    pub text: String,
}

impl Action for EditNote {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let text = match doc.get(ROOT, "body")? {
            Some((Value::Object(ObjType::Text), text)) => text,
            _ => doc.put_object(ROOT, "body", ObjType::Text)?,
        };
        doc.update_text(&text, &self.text)?;
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Install every document action exported by Scratchpad's resources.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<EditNote>(EDIT);
}
