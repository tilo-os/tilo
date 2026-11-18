//! The window's own document of the workspace. It holds nothing: the assistant is an item of
//! the workspace's agents, its thread is a Chat conversation, its work is issues in Tasks
//! and its progress work is Shortcuts. The window shows those resources and keeps no copy.

use tilo_resource::{self, Model, resource};

/// The Assistant window's resource of the workspace.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Window {}

impl Window {
    /// What the resource holds: nothing of its own. It answers through
    /// [`fields`](tilo_resource::read::fields), so a caller with no window open reads it as
    /// it reads any app's resource.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "assistant.window",
    read_content = Window::read_document
)]
impl Window {}
