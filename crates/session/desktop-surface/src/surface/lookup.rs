//! Finding a shell's windows and ids in its tree.

use tilo_desktop_model::{GroupId, IdMinter, Layout, StripId, Tab, TabId, Workspace};

/// Note every id `tree` holds, so the shell never mints one of them again.
pub(super) fn observe_ids(minter: &mut IdMinter, tree: &Layout) {
    for workspace in &tree.workspaces {
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
pub(super) fn find_tab(tree: &Layout, strip: StripId, tab: TabId) -> Option<(&Workspace, &Tab)> {
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
    tree: &mut Layout,
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

/// The window group `group` of the strip `strip`.
pub(super) fn group_in(
    tree: &Layout,
    strip: StripId,
    group: GroupId,
) -> Option<&tilo_desktop_model::WindowGroup> {
    tree.workspaces
        .iter()
        .flat_map(|w| &w.strips)
        .find(|s| s.id == strip)?
        .window_groups
        .iter()
        .find(|g| g.id == group)
}
