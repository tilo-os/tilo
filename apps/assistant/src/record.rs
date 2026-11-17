//! The window's own record of the workspace. It holds nothing: the assistant is an item of
//! the workspace's agents, its thread is a Chat conversation, its work is issues in Tasks
//! and its standing work is Shortcuts. The window shows those records and keeps no copy.

use ui::prelude::*;

/// The Assistant window's record of the workspace.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Window {}

impl Window {
    /// What the record holds: nothing of its own. It answers through
    /// [`fields`](tilo_resource::read::fields), so a caller with no window open reads it as
    /// it reads any app's record.
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
