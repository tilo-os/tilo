//! The desk a host starts from when it has no stored one: one workspace,
//! one strip, and a window group per tab.

use tilo_desktop_model::{
    DesktopTree, IdMinter, StripId, Tab, WindowWidth, Workspace, WorkspaceId, WorkspaceSlot,
};

/// The in-memory desktop a host starts from when it has no stored one: one workspace, one strip,
/// and a window group per tab in `tabs` .
pub fn seed_tree(minter: &mut IdMinter, workspace_name: &str, tabs: Vec<Tab>) -> DesktopTree {
    seed_tree_in(minter, WorkspaceId::new(), workspace_name, tabs)
}

/// [`seed_tree`] for a desk opened in the workspace `identity` names: its workspace is that one,
/// as a session's is the workspace it joined, so what keeps the workspace's records by its id
/// (its history, its agents' runs) finds them from the desk.
pub fn seed_tree_in(
    minter: &mut IdMinter,
    identity: WorkspaceId,
    workspace_name: &str,
    tabs: Vec<Tab>,
) -> DesktopTree {
    let mut workspace =
        Workspace::in_workspace(WorkspaceSlot(minter.mint()), identity, workspace_name);
    // A workspace keeps one identity on every device: the one the person starts from is opened
    // again by the id derived from it, not by an id only this desk knows.
    workspace.slot = WorkspaceSlot::of_workspace(workspace.id());
    // A strip no one has named keeps no name: the desk names it by its place, in the language of
    // whoever reads it.
    let mut strip = tilo_desktop_model::Strip::new(StripId(minter.mint()), "");
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
        active: workspace.slot,
        // Every id above is the minter's, so the desk's own range is too: what a record of this
        // desk shares is this device's to write.
        salt: minter.salt(),
        ..DesktopTree::default()
    };
    tree.active_strip.insert(workspace.slot, strip_id);
    if let Some(group) = first_group {
        tree.set_active_group(strip_id, group);
    }
    tree.workspaces = vec![workspace];
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names;

    /// The desk a host starts from keeps no name of its own: what nobody named is the desk's word
    /// to say, in the language of whoever reads it, so the record holds the same nothing on every
    /// device.
    #[test]
    fn a_fresh_desk_stores_no_name_beside_its_workspace_and_strip() {
        crate::i18n::ensure();
        let mut minter = IdMinter::default();
        let tree = seed_tree(&mut minter, "", Vec::new());
        let workspace = &tree.workspaces[0];
        assert_eq!(workspace.name, "");
        assert_eq!(workspace.strips[0].name, "");
        assert_eq!(names::workspace(&workspace.name), "Workspace");
        assert_eq!(names::strip_named(&workspace.strips[0].name, 0), "Strip 1");
    }
}
