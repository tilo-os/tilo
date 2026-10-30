//! The desk a host starts from when it has no stored one: one workspace,
//! one strip, and a window group per tab.

use tilo_desktop_model::{
    DesktopTree, IdMinter, StripId, Tab, WindowWidth, Workspace, WorkspaceId,
};

/// The in-memory desktop a host starts from when it has no stored one: one
/// workspace, one strip, and a window group per tab in `tabs`.
pub fn seed_tree(minter: &mut IdMinter, workspace_name: &str, tabs: Vec<Tab>) -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceId(minter.mint()), workspace_name);
    let mut strip = tilo_desktop_model::Strip::new(StripId(minter.mint()), "Main");
    strip.window_groups = tabs
        .into_iter()
        .map(|tab| tilo_desktop_model::WindowGroup {
            id: minter.mint(),
            active_tab: tab.id,
            tabs: vec![tab],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        })
        .collect();
    let first_group = strip.window_groups.first().map(|g| g.id);
    let strip_id = strip.id;
    workspace.strips = vec![strip];
    let mut tree = DesktopTree {
        active: workspace.id,
        ..DesktopTree::default()
    };
    tree.active_strip.insert(workspace.id, strip_id);
    if let Some(group) = first_group {
        tree.set_active_group(strip_id, group);
    }
    tree.workspaces = vec![workspace];
    tree
}
