//! The app's resource of the workspace: the anchor its windows and offers belong to.

use tilo_resource::{self, Model, resource};

/// The Activity window's resource of the workspace. It holds nothing of its own: what History
/// shows is the session's workspace activity, and History's filter is the window's own view
/// state, never a change to the workspace.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Window {}

impl Window {
    /// What the resource holds: nothing of its own. It answers through
    /// [`fields`](tilo_resource::read::fields), so a caller with no window open reads it as it
    /// reads any app's resource.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "activity.window",
    read_content = Window::read_document
)]
impl Window {}
