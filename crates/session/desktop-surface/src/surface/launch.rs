//! A window an app asks for: it lands after the focused window of the shown
//! strip, which it makes when the workspace holds none yet.

use tilo_desktop_model::{Strip, StripId, WindowGroup, WindowWidth, Workspace};

use super::{Shared, row_op::RowOp};
use crate::TileSource;
use crate::navigation;

/// Put a window for `app` after the focused one of the shown strip, or at
/// the place `at` in it (a drop's [`StripSlot::index`]), standing over the
/// desk beside it when the app asked for a window that floats.
pub(super) fn open(
    tiles: &mut dyn TileSource,
    shared: &mut Shared,
    app: &str,
    float: bool,
    at: Option<usize>,
) -> Option<RowOp> {
    let (tab_id, group_id) = (shared.minter.mint(), shared.minter.mint());
    let workspace_id = shared.tree.active_workspace()?.id();
    let workspace = shared
        .tree
        .workspaces
        .iter()
        .find(|w| w.id() == workspace_id)?;
    let tab = tiles.launch(workspace, tab_id, app, None)?;
    let shown = navigation::active_strip(&shared.tree, workspace);
    // A workspace a site lets this computer into comes with the strips its
    // site holds, and one joined before its site recorded any has none. The
    // first window opened there makes the strip it goes in.
    let strip = if let Some(strip) = shown {
        strip
    } else {
        let strip = Strip::new(StripId(shared.minter.mint()), "");
        let id = strip.id;
        let workspace = shared
            .tree
            .workspaces
            .iter_mut()
            .find(|w| w.id() == workspace_id)?;
        workspace.strips.push(strip);
        shared.tree.active_strip.insert(workspace_id, id);
        id
    };
    let after = shared.tree.active_group_for(strip);
    let model = shared
        .tree
        .workspaces
        .iter_mut()
        .flat_map(|w| w.strips.iter_mut())
        .find(|s| s.id == strip)?;
    let index = match at {
        Some(at) => at.min(model.window_groups.len()),
        None => after
            .and_then(|id| model.window_groups.iter().position(|g| g.id == id))
            .map_or(model.window_groups.len(), |position| {
                position.saturating_add(1)
            }),
    };
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
    if let Some(workspace) = shared
        .tree
        .workspaces
        .iter_mut()
        .find(|w| w.id() == workspace_id)
    {
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

/// Where a window opened at a point of the shown strip goes: before the
/// first window whose middle is right of the point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StripSlot {
    /// Its place among the strip's windows, as [`DesktopHandle::open_at`]
    /// takes it.
    pub index: usize,
    /// Where it opens, in the surface's coordinates: the middle of the gap
    /// between the windows it goes between, or the point itself in a strip
    /// with none shown.
    pub x: f64,
}

/// The slot at `x` among windows standing at `windows` (each window's place
/// in its strip and its left and right edge, in strip order).
#[must_use]
pub fn slot_among(windows: &[(usize, f64, f64)], x: f64) -> StripSlot {
    let before = windows
        .iter()
        .rev()
        .find(|(_, left, right)| (left + right) / 2.0 <= x);
    let after = windows
        .iter()
        .find(|(_, left, right)| (left + right) / 2.0 > x);
    let index = before.map_or_else(
        || after.map_or(0, |(position, ..)| *position),
        |(position, ..)| position.saturating_add(1),
    );
    let x = match (before, after) {
        (Some((_, _, right)), Some((_, left, _))) => (right + left) / 2.0,
        (Some((_, _, right)), None) => right.max(x.min(right + 24.0)),
        (None, Some((_, left, _))) => left.min(x.max(left - 24.0)),
        (None, None) => x,
    };
    StripSlot { index, x }
}

impl super::DesktopHandle {
    /// Where a window opened at `x` (the surface's coordinates) goes in the
    /// shown strip: what a drop of files on the desk between windows opens
    /// them at. `None` with no workspace shown.
    #[must_use]
    pub fn slot_at(&self, x: f64) -> Option<StripSlot> {
        let shared = self.shared.borrow();
        let workspace = shared.tree.active_workspace()?;
        let Some(strip) = navigation::active_strip(&shared.tree, workspace)
            .and_then(|id| workspace.strips.iter().find(|strip| strip.id == id))
        else {
            return Some(StripSlot { index: 0, x });
        };
        let windows: Vec<(usize, f64, f64)> = strip
            .window_groups
            .iter()
            .enumerate()
            .filter_map(|(position, group)| {
                let rect = shared.shown_tabs.window_rect(group.active_tab)?;
                Some((position, rect.x0, rect.x1))
            })
            .collect();
        Some(slot_among(&windows, x))
    }

    /// Open `app` (an app id, or a launch naming a record or content) in a
    /// new window at the place `index` of the shown strip
    /// ([`Self::slot_at`]).
    ///
    /// # Errors
    ///
    /// No workspace is shown, or `app` names no application.
    pub fn open_at(&self, app: &str, index: usize) -> Result<(), String> {
        let mut shared = self.shared.borrow_mut();
        shared
            .tree
            .active_workspace()
            .ok_or("Workspace unavailable")?;
        if app.is_empty() {
            return Err("Application unavailable".into());
        }
        shared
            .queue
            .push(crate::navigation::Admitted::LaunchAt(app.to_owned(), index));
        shared.wake.wake();
        Ok(())
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    #[test]
    fn a_point_between_windows_opens_between_them_and_the_ends_open_at_the_ends() {
        let windows = [(0, 0.0, 400.0), (1, 420.0, 820.0), (2, 840.0, 1240.0)];
        assert_eq!(
            slot_among(&windows, 830.0),
            StripSlot { index: 2, x: 830.0 }
        );
        assert_eq!(slot_among(&windows, 100.0), StripSlot { index: 0, x: 0.0 });
        assert_eq!(
            slot_among(&windows, 1300.0),
            StripSlot {
                index: 3,
                x: 1264.0
            }
        );
        assert_eq!(
            slot_among(&windows, 1250.0),
            StripSlot {
                index: 3,
                x: 1250.0
            }
        );
        assert_eq!(slot_among(&windows, 700.0).index, 2);
        assert_eq!(slot_among(&[], 300.0), StripSlot { index: 0, x: 300.0 });
        // Windows scrolled out of view to the left still count before the point.
        let scrolled = [(3, -900.0, -500.0), (4, 100.0, 500.0)];
        assert_eq!(slot_among(&scrolled, 600.0).index, 5);
        assert_eq!(slot_among(&scrolled, 50.0).index, 4);
    }
}
