//! Workspaces pane — list, rename, delete the user's workspaces.
//
// TODO(qs_actions): wire to `shell::qs_actions::workspaces::*`
//   - `list() -> Vec<WorkspaceSummary>`
//   - `rename(id: WorkspaceId, name: &str)`
//   - `delete(id: WorkspaceId)`
//   - `set_active(id: WorkspaceId)`
// The wheel disc surfaces workspaces as spokes too — this pane will
// be the canonical way to rename or destroy one without going through
// the wheel context menu.

use ui::widget::Widget;

pub fn build() -> Box<dyn Widget> {
    super::coming_soon(
        "Workspace rename and delete controls land here once \
         shell::qs_actions::workspaces is wired in.",
    )
}
