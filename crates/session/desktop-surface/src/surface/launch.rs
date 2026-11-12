//! A window an app asks for: it lands after the focused window of the shown
//! strip, which it makes when the workspace holds none yet.

use tilo_desktop_model::{Strip, StripId, WindowGroup, WindowWidth, Workspace};

use super::{Shared, row_op::RowOp};
use crate::TileSource;
use crate::navigation;

/// Put a window for `app` after the focused one of the shown strip, standing
/// over the desk beside it when the app asked for a window that floats.
pub(super) fn open(
    tiles: &mut dyn TileSource,
    shared: &mut Shared,
    app: &str,
    float: bool,
) -> Option<RowOp> {
    let (tab_id, group_id) = (shared.minter.mint(), shared.minter.mint());
    let slot = shared.tree.active_workspace()?.slot;
    let workspace = shared.tree.workspaces.iter().find(|w| w.slot == slot)?;
    let tab = tiles.launch(workspace, tab_id, app, None)?;
    let shown = navigation::active_strip(&shared.tree, workspace);
    // A workspace a site lets this computer into comes with the strips its
    // site holds, and one joined before its site recorded any has none. The
    // first window opened there makes the strip it goes in.
    let strip = if let Some(strip) = shown {
        strip
    } else {
        let strip = Strip::new(StripId(shared.minter.mint()), "Main");
        let id = strip.id;
        let workspace = shared.tree.workspaces.iter_mut().find(|w| w.slot == slot)?;
        workspace.strips.push(strip);
        shared.tree.active_strip.insert(slot, id);
        id
    };
    let after = shared.tree.active_group_for(strip);
    let model = shared
        .tree
        .workspaces
        .iter_mut()
        .flat_map(|w| w.strips.iter_mut())
        .find(|s| s.id == strip)?;
    let index = after
        .and_then(|id| model.window_groups.iter().position(|g| g.id == id))
        .map_or(model.window_groups.len(), |position| {
            position.saturating_add(1)
        });
    model.window_groups.insert(
        index,
        WindowGroup {
            id: group_id,
            tabs: vec![tab.clone()],
            active_tab: tab.id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
    );
    // A window group the canvas does not place is a window neither the framed
    // canvas, the spaces canvas nor the traditional desktop draws: the desk a
    // person just opened an app on would look empty. The group takes a place
    // as any other group does, by the same rule ([`Arrangement::place_missing`]).
    if let Some(workspace) = shared.tree.workspaces.iter_mut().find(|w| w.slot == slot) {
        let Workspace {
            strips,
            arrangement,
            ..
        } = workspace;
        arrangement.place_missing(strips);
    }
    // A window the app that opened it stands over the desk takes the place its
    // strip gives it, as the record keeps places: by its corner of the work
    // area, so every member finds it there. The row stands it there, since
    // only the row knows the place the strip's share makes for it.
    shared.tree.set_active_group(strip, group_id);
    shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
    Some(RowOp::Open {
        strip,
        group: group_id,
        index,
        tab,
        float,
    })
}
