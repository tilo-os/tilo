//! Finding a desk's windows and ids in its tree.

use tilo_desktop_model::{DesktopTree, GroupId, IdMinter, StripId, Tab, TabId, Workspace};

/// Note every id `tree` holds, so the desk never mints one of them again.
pub(super) fn observe_ids(minter: &mut IdMinter, tree: &DesktopTree) {
    for workspace in &tree.workspaces {
        minter.observe(workspace.slot.0);
        for strip in &workspace.strips {
            minter.observe(strip.id.0);
            for group in &strip.window_groups {
                minter.observe(group.id);
                for tab in &group.tabs {
                    minter.observe(tab.id);
                }
            }
        }
    }
}

/// The tab `tab` of the strip `strip`, with the workspace it is in.
pub(super) fn find_tab(
    tree: &DesktopTree,
    strip: StripId,
    tab: TabId,
) -> Option<(&Workspace, &Tab)> {
    tree.workspaces.iter().find_map(|workspace| {
        let strip = workspace.strips.iter().find(|s| s.id == strip)?;
        strip
            .window_groups
            .iter()
            .flat_map(|group| &group.tabs)
            .find(|t| t.id == tab)
            .map(|tab| (workspace, tab))
    })
}

/// The window group `group` of the strip `strip`, to change.
pub(super) fn find_group(
    tree: &mut DesktopTree,
    strip: StripId,
    group: GroupId,
) -> Option<&mut tilo_desktop_model::WindowGroup> {
    tree.workspaces
        .iter_mut()
        .flat_map(|w| w.strips.iter_mut())
        .find(|s| s.id == strip)?
        .window_groups
        .iter_mut()
        .find(|g| g.id == group)
}
