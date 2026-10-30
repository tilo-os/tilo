//! Each display holds its own window groups.
//!
//! With more than one display a strip is one lane per display: every window
//! group belongs to exactly one display (the tree's
//! [`OutputPartition`](tilo_desktop_model::OutputPartition)), is laid out,
//! panned and clipped there only, and is never drawn on another. Every
//! display keeps the strip's edge gutters at both ends of its lane. New
//! windows open on the display with focus; focus moves between displays by
//! stepping past a display's last window, by pressing on a display, or by
//! asking for a display; a window moves to the neighbouring display on
//! request. The strip's order stays the shared one; the row lays it out
//! display by display ([`OutputPartition::lane_order`]).
//!
//! [`OutputPartition::lane_order`]: tilo_desktop_model::OutputPartition::lane_order

use std::collections::HashMap;

use tilo_desktop_model::{DesktopTree, Display, GroupId, Strip, TabId};
use ui::kurbo::{Point, Rect};
use ui::widget::{GroupRow, STRIP_EDGE_GUTTER_PX, StripLane, Widget as _};

use super::{DesktopHandle, DesktopSurface, Shared};
use crate::navigation::{self, Admitted, RouteIds};

/// Where stepping focus along a strip spread over several displays goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Another window of the strip.
    Window(GroupId, TabId),
    /// Onto display `.0`, to its window `.1` nearest the seam crossed, or
    /// to none when it shows no window.
    Display(u32, Option<GroupId>),
}

/// The step `steps` windows from `from` (the focused window, when focus is
/// on one) along `strip`. On one display that walks the strip's windows
/// and stops at either end. Across several it walks the focused display's
/// windows and, past its first or last, crosses to the neighbouring
/// display, landing on the window nearest the seam.
pub(crate) fn step(
    tree: &DesktopTree,
    strip: &Strip,
    from: Option<(GroupId, TabId)>,
    steps: i32,
) -> Option<Step> {
    let windows = |groups: &[GroupId]| -> Vec<(GroupId, TabId)> {
        groups
            .iter()
            .filter_map(|id| strip.window_groups.iter().find(|g| g.id == *id))
            .flat_map(|g| g.tabs.iter().map(move |t| (g.id, t.id)))
            .collect()
    };
    let target = |windows: &[(GroupId, TabId)]| {
        let at = windows.iter().position(|w| Some(*w) == from)?;
        let index = at.checked_add_signed(isize::try_from(steps).ok()?)?;
        windows.get(index).copied()
    };
    if !tree.is_partitioned() {
        let all: Vec<GroupId> = strip.window_groups.iter().map(|g| g.id).collect();
        return target(&windows(&all)).map(|(g, t)| Step::Window(g, t));
    }
    let partition = &tree.outputs;
    let display = partition.focused_display()?;
    let on = windows(&partition.groups_on(strip, display));
    if let Some((group, tab)) = target(&on) {
        return Some(Step::Window(group, tab));
    }
    // Past either end of the display, or no window focused on it: on to
    // the neighbouring display.
    let next = partition.neighbour(display, steps.signum())?;
    let arriving = partition.groups_on(strip, next);
    let pick = if steps > 0 {
        arriving.first().copied()
    } else {
        arriving.last().copied()
    };
    Some(Step::Display(next, pick))
}

/// One lane per display of `displays` (in the surface's coordinates), in
/// `order`, for a row laid out at `row` in a surface at `surface`: each
/// display's lane keeps the insets the row keeps from the surface's edges,
/// taken from that display's own top and bottom, and the strip's gutters
/// at both its ends.
pub(crate) fn lanes(
    displays: &[Display],
    order: &[u32],
    surface: Rect,
    row: Rect,
) -> Vec<StripLane> {
    let row = if row.height() > 0.0 { row } else { surface };
    let top = (row.y0 - surface.y0).max(0.0);
    let bottom = (surface.y1 - row.y1).max(0.0);
    order
        .iter()
        .filter_map(|id| displays.iter().find(|d| d.id == *id))
        .map(|display| {
            let x0 = surface.x0 + display.x;
            let y0 = surface.y0 + display.y;
            let area = Rect::new(
                x0,
                y0 + top,
                x0 + display.width,
                (y0 + display.height - bottom).max(y0 + top),
            );
            StripLane {
                output: display.id,
                area,
                usable: (display.width - 2.0 * STRIP_EDGE_GUTTER_PX).max(1.0),
                leading: true,
            }
        })
        .collect()
}

/// The display among `displays` (in the surface's coordinates, the surface
/// at `surface`) holding point `p`.
pub(crate) fn display_at(displays: &[Display], surface: Rect, p: Point) -> Option<u32> {
    displays
        .iter()
        .find(|d| {
            let x = p.x - surface.x0;
            let y = p.y - surface.y0;
            x >= d.x && x < d.x + d.width && y >= d.y && y < d.y + d.height
        })
        .map(|d| d.id)
}

impl Shared {
    /// Move focus to `display`, onto `pick` when it is there, else the
    /// window last focused there, else its first; a display with no window
    /// takes focus with none, so the next window opens there. Every display
    /// shows the active strip, which stays. Whether anything changed.
    pub(super) fn focus_display(&mut self, display: u32, pick: Option<GroupId>) -> bool {
        let tree = &mut self.tree;
        if !tree.is_partitioned() || !tree.outputs.displays.iter().any(|d| d.id == display) {
            return false;
        }
        let before = (tree.outputs.focused_display(), tree.active_group.clone());
        tree.outputs.set_focused(display);
        let target = tree.active_workspace().and_then(|workspace| {
            let strip_id = navigation::active_strip(tree, workspace)?;
            let strip = workspace.strips.iter().find(|s| s.id == strip_id)?;
            let group = pick
                .filter(|g| tree.outputs.display_of(*g) == Some(display))
                .or_else(|| tree.outputs.focus_target(strip, display));
            let route = group.and_then(|group| {
                let model = strip.window_groups.iter().find(|g| g.id == group)?;
                Some(RouteIds {
                    workspace: workspace.id,
                    strip: strip_id,
                    group,
                    tab: model.active_tab,
                })
            });
            Some((strip_id, route))
        });
        match target {
            Some((_, Some(route))) => {
                self.queue.push(Admitted::Select(route));
                true
            }
            Some((strip, None)) => {
                tree.active_group.remove(&strip);
                let changed = before != (tree.outputs.focused_display(), tree.active_group.clone());
                if changed {
                    tree.external_revision = tree.external_revision.wrapping_add(1);
                    self.changed();
                }
                changed
            }
            None => before.0 != tree.outputs.focused_display(),
        }
    }

    /// Step focus `steps` windows along the shown strip ([`step`]): queue
    /// the window it lands on, or move focus to the display it crosses to.
    /// False when there is nowhere to go.
    pub(super) fn step_focus(&mut self, steps: i32) -> bool {
        // A step queued since the last update has moved focus already.
        let queued = self.queue.iter().rev().find_map(|admitted| match admitted {
            Admitted::Select(route) => Some(*route),
            _ => None,
        });
        let tree = &self.tree;
        let Some(workspace) = tree.active_workspace() else {
            return false;
        };
        let Some(strip_id) = navigation::active_strip(tree, workspace) else {
            return false;
        };
        let Some(strip) = workspace.strips.iter().find(|s| s.id == strip_id) else {
            return false;
        };
        // On a display without a focused window, focus is on no window,
        // whatever the strip falls back to.
        let from = queued
            .filter(|route| route.strip == strip_id)
            .map(|route| (route.group, route.tab))
            .or_else(|| {
                let group = if tree.is_partitioned() {
                    tree.active_group_for(strip_id)
                } else {
                    tree.resolve_active_group(strip_id, |_| false)
                }?;
                let model = strip.window_groups.iter().find(|g| g.id == group)?;
                Some((group, model.active_tab))
            });
        let workspace_id = workspace.id;
        match step(tree, strip, from, steps) {
            Some(Step::Window(group, tab)) => {
                self.queue.push(Admitted::Select(RouteIds {
                    workspace: workspace_id,
                    strip: strip_id,
                    group,
                    tab,
                }));
                true
            }
            Some(Step::Display(display, pick)) => {
                self.focus_display(display, pick);
                true
            }
            None => false,
        }
    }
}

impl DesktopHandle {
    /// The displays the desktop spreads over, primary first, in the
    /// surface's coordinates; a mirror set is one display. Groups move
    /// between them as [`OutputPartition::reconcile`] says, and the surface
    /// lays each display's lane out on its next pass. Whether anything
    /// changed.
    ///
    /// [`OutputPartition::reconcile`]: tilo_desktop_model::OutputPartition::reconcile
    pub fn set_displays(&self, displays: &[Display]) -> bool {
        let mut shared = self.shared.borrow_mut();
        let changed = shared.tree.reconcile_displays(displays);
        if changed {
            shared.changed();
        }
        changed
    }

    /// The displays in reading order, left to right: empty on one display.
    #[must_use]
    pub fn displays(&self) -> Vec<u32> {
        self.read(|tree| {
            if tree.is_partitioned() {
                tree.outputs.reading_order()
            } else {
                Vec::new()
            }
        })
    }

    /// The display the person works on, while there are several.
    #[must_use]
    pub fn focused_display(&self) -> Option<u32> {
        self.read(|tree| {
            tree.is_partitioned()
                .then(|| tree.outputs.focused_display())
                .flatten()
        })
    }

    /// The display window `group` shows on, while there are several.
    #[must_use]
    pub fn display_of(&self, group: GroupId) -> Option<u32> {
        self.read(|tree| {
            tree.is_partitioned()
                .then(|| tree.outputs.display_of(group))
                .flatten()
        })
    }

    /// Move focus to `display`, on the next update pass: to the window last
    /// focused there, else its first, else to no window, so the next one
    /// opens there. False when there is no such display.
    pub fn focus_display(&self, display: u32) -> bool {
        let focusable = self.read(|tree| {
            tree.is_partitioned() && tree.outputs.displays.iter().any(|d| d.id == display)
        });
        focusable && {
            self.shared.borrow_mut().focus_display(display, None);
            true
        }
    }

    /// Step focus `steps` windows along the shown strip, on the next update
    /// pass, crossing to the neighbouring display past either end of the
    /// focused one. False when there is nowhere to go.
    pub fn step_focus(&self, steps: i32) -> bool {
        self.shared.borrow_mut().step_focus(steps)
    }

    /// Move the focused window to the display `steps` places to its right
    /// (left when negative). It keeps focus, and its place in the strip's
    /// order among that display's windows. False when there is no such
    /// display or no focused window.
    pub fn move_focused_to_display(&self, steps: i32) -> bool {
        let mut shared = self.shared.borrow_mut();
        let tree = &mut shared.tree;
        if !tree.is_partitioned() || steps == 0 {
            return false;
        }
        let Some(strip) = tree
            .active_workspace()
            .and_then(|workspace| navigation::active_strip(tree, workspace))
        else {
            return false;
        };
        let Some(group) = tree.active_group_for(strip) else {
            return false;
        };
        let Some(to) = tree
            .outputs
            .display_of(group)
            .and_then(|from| tree.outputs.neighbour(from, steps.signum()))
        else {
            return false;
        };
        if !tree.outputs.move_group(&tree.workspaces, group, to) {
            return false;
        }
        tree.external_revision = tree.external_revision.wrapping_add(1);
        shared.changed();
        true
    }
}

impl DesktopSurface {
    /// Bring the partition and the shown row in line with the displays:
    /// a window without a display gets the focused one, the display of the
    /// focused window is the focused display, and the row lays its windows
    /// out one lane per display, display by display. On one display the
    /// row is one continuous strip again.
    pub(super) fn lay_out_displays(&mut self) {
        let surface = self.rect;
        let Some(strip_id) = self.shown else {
            return;
        };
        let plan = {
            let mut shared = self.shared.borrow_mut();
            let tree = &mut shared.tree;
            if tree.is_partitioned() {
                tree.outputs.place_unhomed(&tree.workspaces);
                if let Some(group) = tree.active_group_for(strip_id) {
                    tree.outputs.note_focus(strip_id, group);
                }
                let order = tree.outputs.reading_order();
                let strip = tree
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.strips)
                    .find(|s| s.id == strip_id);
                let groups: Vec<GroupId> = strip
                    .map(|strip| {
                        tree.outputs
                            .lane_order(strip)
                            .into_iter()
                            .map(|g| g.id)
                            .collect()
                    })
                    .unwrap_or_default();
                let assignment: HashMap<u64, u32> = groups
                    .iter()
                    .filter_map(|g| tree.outputs.display_of(*g).map(|d| (*g, d)))
                    .collect();
                Some((
                    tree.outputs.displays.clone(),
                    order,
                    groups,
                    assignment,
                    tree.outputs.focused_display(),
                ))
            } else {
                None
            }
        };
        let Some(row) = self.rows.get_mut(&strip_id) else {
            return;
        };
        let Some((displays, order, groups, assignment, focused)) = plan else {
            row.set_lanes(&[], &HashMap::new());
            return;
        };
        put_in_lane_order(row, &groups);
        let lanes = lanes(&displays, &order, surface, row.get_rect());
        row.set_lanes(&lanes, &assignment);
        if let Some(focused) = focused {
            row.set_focus_output(focused);
        }
    }

    /// A press at `p` on no window: across several displays, focus moves
    /// to the display pressed on.
    pub(super) fn focus_display_at(&self, p: Point) {
        let mut shared = self.shared.borrow_mut();
        if !shared.tree.is_partitioned() {
            return;
        }
        let Some(display) = display_at(&shared.tree.outputs.displays, self.rect, p) else {
            return;
        };
        if shared.tree.outputs.focused_display() != Some(display) {
            shared.focus_display(display, None);
        }
    }
}

/// Order `row`'s groups as `groups` (the strip's, display by display),
/// leaving any the row still shows closing after them.
fn put_in_lane_order(row: &mut GroupRow, groups: &[GroupId]) {
    let mut at = 0usize;
    for id in groups {
        let Some(position) = row.groups.iter().position(|g| g.id == *id) else {
            continue;
        };
        if position != at {
            row.reorder_group_by_id(*id, at);
        }
        at = at.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{
        AppKind, DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        Workspace, WorkspaceId,
    };

    fn group(id: GroupId) -> WindowGroup {
        let tab = id.saturating_mul(10);
        WindowGroup {
            id,
            tabs: vec![Tab {
                id: tab,
                title: format!("W{id}"),
                tile: TileContent::Stub {
                    kind: AppKind::Settings,
                    label: String::new(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    fn display(id: u32, x: f64) -> Display {
        Display {
            id,
            monitor: format!("monitor-{id}"),
            x,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        }
    }

    /// A tree whose one strip holds groups `ids`, the first `left` of them
    /// on the left display.
    fn docked(ids: &[GroupId], left: usize) -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(1), "Desk");
        let mut strip = Strip::new(StripId(1), "S");
        strip.window_groups = ids.iter().map(|id| group(*id)).collect();
        workspace.strips = vec![strip];
        let mut tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        assert!(tree.reconcile_displays(&[display(0, 0.0), display(1, 800.0)]));
        for (i, id) in ids.iter().enumerate() {
            let on = u32::from(i >= left);
            tree.outputs.move_group(&tree.workspaces, *id, on);
        }
        tree
    }

    fn strip(tree: &DesktopTree) -> &Strip {
        &tree.workspaces[0].strips[0]
    }

    #[test]
    fn one_display_steps_along_the_strip_and_stops_at_its_ends() {
        let mut workspace = Workspace::new(WorkspaceId(1), "Desk");
        let mut s = Strip::new(StripId(1), "S");
        s.window_groups = vec![group(1), group(2)];
        workspace.strips = vec![s];
        let tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let strip = strip(&tree);
        assert_eq!(
            step(&tree, strip, Some((1, 10)), 1),
            Some(Step::Window(2, 20))
        );
        assert_eq!(step(&tree, strip, Some((2, 20)), 1), None);
    }

    #[test]
    fn stepping_past_a_displays_last_window_crosses_to_the_next_display() {
        let mut tree = docked(&[1, 2, 3], 2);
        tree.outputs.set_focused(0);
        let s = strip(&tree).clone();
        assert_eq!(step(&tree, &s, Some((1, 10)), 1), Some(Step::Window(2, 20)));
        assert_eq!(
            step(&tree, &s, Some((2, 20)), 1),
            Some(Step::Display(1, Some(3))),
            "lands on the window nearest the seam"
        );
        tree.outputs.set_focused(1);
        assert_eq!(
            step(&tree, &s, Some((3, 30)), -1),
            Some(Step::Display(0, Some(2)))
        );
        assert_eq!(step(&tree, &s, Some((3, 30)), 1), None, "the last display");
    }

    #[test]
    fn an_empty_display_is_stepped_onto_and_out_of() {
        let mut tree = docked(&[1], 1);
        let s = strip(&tree).clone();
        assert_eq!(
            step(&tree, &s, Some((1, 10)), 1),
            Some(Step::Display(1, None))
        );
        tree.outputs.set_focused(1);
        assert_eq!(step(&tree, &s, None, -1), Some(Step::Display(0, Some(1))));
    }

    #[test]
    fn each_display_gets_a_lane_with_the_rows_insets_and_its_own_gutters() {
        let displays = [display(0, 0.0), display(1, 800.0)];
        let surface = Rect::new(0.0, 0.0, 1600.0, 600.0);
        let row = Rect::new(0.0, 40.0, 1600.0, 588.0);
        let lanes = lanes(&displays, &[1, 0], surface, row);
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].output, 1, "in the order asked");
        assert_eq!(lanes[0].area, Rect::new(800.0, 40.0, 1600.0, 588.0));
        assert_eq!(lanes[1].area, Rect::new(0.0, 40.0, 800.0, 588.0));
        assert!(lanes.iter().all(|lane| lane.leading));
        assert_eq!(lanes[1].usable, 800.0 - 2.0 * STRIP_EDGE_GUTTER_PX);
    }

    #[test]
    fn a_shorter_display_keeps_the_bottom_gutter_above_its_own_edge() {
        let mut short = display(1, 800.0);
        short.height = 400.0;
        let surface = Rect::new(0.0, 0.0, 1600.0, 600.0);
        let row = Rect::new(0.0, 40.0, 1600.0, 588.0);
        let lanes = lanes(&[display(0, 0.0), short], &[0, 1], surface, row);
        assert_eq!(lanes[1].area, Rect::new(800.0, 40.0, 1600.0, 388.0));
    }

    #[test]
    fn a_press_finds_its_display() {
        let displays = [display(0, 0.0), display(1, 800.0)];
        let surface = Rect::new(0.0, 0.0, 1600.0, 600.0);
        assert_eq!(
            display_at(&displays, surface, Point::new(10.0, 10.0)),
            Some(0)
        );
        assert_eq!(
            display_at(&displays, surface, Point::new(810.0, 10.0)),
            Some(1)
        );
        assert_eq!(
            display_at(&displays, surface, Point::new(1610.0, 10.0)),
            None
        );
    }
}
