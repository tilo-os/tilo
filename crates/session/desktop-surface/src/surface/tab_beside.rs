//! A tab an app opens beside one of its own: it lands in the window group
//! the app already has, right after the tab it named.
//!
//! Applications have no tab strips of their own: another page, file or
//! conversation is another window of the app, a tab of the group its first
//! window opened. The request names the tab it belongs beside, so the tab
//! lands there and nowhere else — never in another app's window group.

use tilo_desktop_model::{GroupId, StripId, Tab, TabId};

use super::tile_content::{build_tile, descriptor};
use super::{DesktopSurface, RowOp, Shared};
use crate::TileSource;
use crate::navigation::RouteIds;

/// Put a window for `app` into the group of the window at `ids`, as its tab
/// right after it: what an app's request to open beside one of its own tabs
/// does. A tab opened behind stays where it opened; one opened in front is
/// the strip's active window.
pub(super) fn open_tab_beside(
    tiles: &mut dyn TileSource,
    shared: &mut Shared,
    ids: RouteIds,
    app: &str,
    background: bool,
) -> Option<RowOp> {
    let tab_id = shared.minter.mint();
    let workspace = shared
        .tree
        .workspaces
        .iter()
        .find(|workspace| workspace.id == ids.workspace)?;
    let tab = tiles.launch(workspace, tab_id, app)?;
    let group = shared
        .tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == ids.workspace)?
        .strips
        .iter_mut()
        .find(|strip| strip.id == ids.strip)?
        .window_groups
        .iter_mut()
        .find(|group| group.id == ids.group)?;
    let index = group
        .tabs
        .iter()
        .position(|tab| tab.id == ids.tab)
        .map_or(group.tabs.len(), |position| position.saturating_add(1));
    group.tabs.insert(index, tab.clone());
    let front = if background {
        group.active_tab
    } else {
        group.activate_tab(tab.id);
        tab.id
    };
    if !background {
        shared.tree.set_active_group(ids.strip, ids.group);
    }
    shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
    Some(RowOp::TabInGroup {
        strip: ids.strip,
        group: ids.group,
        index,
        tab,
        front,
        background,
    })
}

impl DesktopSurface {
    /// Present a tab opened into a group that is already there. A strip not
    /// built yet holds it in the tree alone; its row finds it when shown. A
    /// tab opened in front comes into view with its group.
    pub(super) fn insert_tab(
        &mut self,
        strip: StripId,
        group: GroupId,
        index: usize,
        tab: &Tab,
        front: TabId,
        background: bool,
    ) {
        let workspace = self.shared.borrow().tree.active_workspace().cloned();
        if let Some(workspace) = workspace
            && let Some(widget) = self.rows.get_mut(&strip).and_then(|row| {
                row.groups
                    .iter_mut()
                    .find(|candidate| candidate.id == group)
            })
        {
            let host = build_tile(self.tiles.as_mut(), self.controls.as_ref(), &workspace, tab);
            widget.insert_detached_tab(index, descriptor(tab), host);
            // Inserting a tab puts it in front: put back the tab the tree
            // says is, so a tab opened behind takes no focus.
            widget.set_active_tab(front);
        }
        self.structure_dirty = true;
        if !background {
            self.reveal = Some((strip, group));
        }
    }
}
