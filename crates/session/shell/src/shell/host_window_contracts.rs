//! Host-owned window contracts. Applications cannot publish these contracts:
//! the shell derives them from its current local, instantiated tabs.
use crate::shell::state::{TabId, TileContent, WorkspaceTree};
use tilo_action_policy::{ActionKind, TreePath};

#[cfg(test)]
mod native_host;

/// The host's typed publication for one currently instantiated local window.
/// This is not an application resource or a grant to invoke its actions.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostWindowContract {
    pub window: TreePath,
    pub request_model: TreePath,
    pub kind: ActionKind,
    pub room: String,
    pub tab: TabId,
    pub app: String,
    pub record: Option<String>,
}

/// The semantic window API publishes exactly the contracts MODEL discovers.
/// Rows for pending or remote windows remain visible but carry no contract.
pub(crate) fn window_rows(tree: &WorkspaceTree) -> Vec<serde_json::Value> {
    let contracts = published(tree);
    let mut rows = crate::shell::test_control::window_rows(&tree.desktop);
    for row in &mut rows {
        let contract = row
            .get("tab")
            .and_then(serde_json::Value::as_u64)
            .and_then(|tab| contracts.iter().find(|contract| contract.tab == tab));
        row["host_contract"] = serde_json::json!(contract);
    }
    rows
}

/// Discover only local instantiated windows. Remote presentations, pending
/// launches and restoration placeholders do not publish host authority.
pub(crate) fn published(tree: &WorkspaceTree) -> Vec<HostWindowContract> {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter_map(move |tab| {
                    let (TileContent::Foreign { app_id: app, .. }
                    | TileContent::LocalApp { app_id: app, .. }) = &tab.tile
                    else {
                        return None;
                    };
                    let room = workspace.room.to_string();
                    // The app segment names the publisher, not the hosted app.
                    let window = TreePath::room(&room)
                        .app("so.tilo.shell")
                        .kind("host-window")
                        .resource(tab.id.to_string());
                    Some(HostWindowContract {
                        request_model: window.clone().action("requestModel"),
                        window,
                        kind: ActionKind::Outside,
                        room,
                        tab: tab.id,
                        app: app.clone(),
                        record: tree.room_tab_record(workspace.room, tab.id),
                    })
                })
        })
        .collect()
}
