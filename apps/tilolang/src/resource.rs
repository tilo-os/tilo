//! The resource the framework hosts the tilolang app over.
//!
//! The framework's root hosts every app over a resource. A tilolang app's records, their
//! history, door and search are its instance's: a tilolang host runs it
//! ([`crate::hosted::HostedWindow`]), the session's or one in this process. This resource only
//! names the record the framework keeps for the app; its state is that record's heads.

use tilolang_tilo::action::DynAction;
use ui::tilo_resource::{Doc, ModelError, Resource};

/// The resource a tilolang app's window is hosted over.
pub struct Workspace;

/// What the framework keeps of its record: its heads, so a change is a new state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceState {
    heads: Vec<ui::automerge::ChangeHash>,
}

impl Resource for Workspace {
    const KIND: &'static str = crate::RECORD_KIND;
    const LABEL: &'static str = "Apps";
    const ACTION_TYPE: &'static str = tilolang_tilo::action::ACTION_TYPE;

    type State = WorkspaceState;
    type Action = DynAction;

    fn read(doc: &Doc) -> Result<WorkspaceState, ModelError> {
        Ok(WorkspaceState {
            heads: doc.committed_document().get_heads(),
        })
    }
}
