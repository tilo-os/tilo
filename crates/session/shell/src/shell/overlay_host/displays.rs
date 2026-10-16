//! Each display holds its own window groups.
//!
//! With more than one display, a strip is one sub-row per display: every
//! window group belongs to exactly one display ([`OutputPartition`]), is laid
//! out, panned and clipped there only, and is never drawn on another. The
//! primary display carries the wheel disc and the full top bar; every other
//! display a compact bar along its own top edge. New windows open on the
//! display with focus; focus moves between displays with the pointer and
//! with Super+Left/Right past a display's last window, and a group moves to
//! the neighbouring display with the configured chord or by being dragged
//! onto another display. Every display stacks all of the workspace's strips
//! in its own height (see [`strip_bands`]).
//!
//! The partition is this device's layout, kept in the local desktop tree and
//! never replicated. The window groups' order stays the strip's; the rows
//! lay them out display by display.

use std::collections::HashMap;

use ui::widget::{GroupRow, StripLane, Widget as _};
use vello::kurbo::{Point, Rect};

use super::{
    OutputGeometry, STRIP_TRAILING_GUTTER_PX, ShellOverlayHost, sidebar_width_for_viewport,
    workspace_rail_clearance_for_viewport,
};
use crate::shell::display_bars::DisplayBars;
use crate::shell::state::{Display, GroupId, StripId};
use crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX;

/// What a display that doesn't touch the desktop's top edge keeps along its
/// own top edge: its top bar and the margin the workspace column puts under
/// the full bar.
fn own_top_band() -> f64 {
    let top = match crate::shell::WORK_AREA {
        tilo_desktop::composition::WorkArea::Flush { top, .. } => top,
        tilo_desktop::composition::WorkArea::Framed => 0.0,
    };
    f64::from(TOPBAR_HEIGHT_PX) + f64::from(top)
}

/// The outputs that are displays of their own, in the backend's order: the
/// primary chosen in Settings first. The backend lays a mirror over the
/// rectangle of the output it copies, while outputs that extend the desktop
/// tile edge to edge and never overlap; so an output overlapping one already
/// taken shows that one's image and is not another display. The first
/// output of a mirror set stands for it.
fn distinct_outputs(outputs: &[OutputGeometry]) -> Vec<&OutputGeometry> {
    let mut distinct: Vec<&OutputGeometry> = Vec::with_capacity(outputs.len());
    for output in outputs.iter().filter(|o| o.width > 0.0 && o.height > 0.0) {
        let copies = |kept: &&OutputGeometry| {
            let shared = kept.rect().intersect(output.rect());
            kept.id == output.id || (shared.width() > 0.0 && shared.height() > 0.0)
        };
        if !distinct.iter().any(copies) {
            distinct.push(output);
        }
    }
    distinct
}

/// The displays as the partition knows them, primary first, a mirror set as
/// one (see [`distinct_outputs`]). An output whose monitor
/// the backend can't name is known by its id.
fn displays_of(outputs: &[OutputGeometry]) -> Vec<Display> {
    distinct_outputs(outputs)
        .into_iter()
        .map(|o| Display {
            id: o.id,
            monitor: if o.monitor.is_empty() {
                format!("output:{}", o.id)
            } else {
                o.monitor.clone()
            },
            x: o.x,
            y: o.y,
            width: o.width,
            height: o.height,
        })
        .collect()
}

/// One strip's place in every display's height: the fractions of the
/// workspace column from its top to the strip's row's top and bottom, and
/// the column's own top and bottom.
type StripBand = ((f64, f64), (f64, f64));

/// Where each strip sits in every display's height, for rows in the order
/// the workspace column stacks them.
///
/// Every display stacks all of the workspace's strips in its own height, the
/// way a single display does, rather than showing only the part of the
/// column that happens to pass over it: with strips stacked down a column
/// taller than one display, a strip would otherwise land wholly on one
/// display of a vertical stack and its windows on the other would have
/// nowhere to be drawn. Before the rows have been laid out once, the strips
/// share the column below `top` equally.
fn strip_bands(rows: &[Rect], top: f64) -> Vec<StripBand> {
    let laid_out: Vec<&Rect> = rows.iter().filter(|r| r.height() > 0.0).collect();
    let column = (
        laid_out.iter().map(|r| r.y0).fold(f64::INFINITY, f64::min),
        laid_out
            .iter()
            .map(|r| r.y1)
            .fold(f64::NEG_INFINITY, f64::max),
    );
    let height = column.1 - column.0;
    if laid_out.is_empty() || height <= 0.0 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a workspace has a handful of strips"
        )]
        let n = rows.len().max(1) as f64;
        return (0..rows.len())
            .map(|k| {
                #[allow(clippy::cast_precision_loss, reason = "as above")]
                let k = k as f64;
                ((k / n, (k + 1.0) / n), (top, f64::INFINITY))
            })
            .collect();
    }
    rows.iter()
        .map(|r| {
            let share = |y: f64| ((y - column.0) / height).clamp(0.0, 1.0);
            ((share(r.y0), share(r.y1)), column)
        })
        .collect()
}

/// `lanes` cut to one strip's band (see [`strip_bands`]): each display's
/// work area, clipped to the column, shared the way the column is.
fn in_band(lanes: &[StripLane], (share, column): StripBand) -> Vec<StripLane> {
    lanes
        .iter()
        .map(|lane| {
            let top = lane.area.y0.max(column.0);
            let bottom = lane.area.y1.min(column.1).max(top);
            let height = bottom - top;
            StripLane {
                area: Rect::new(
                    lane.area.x0,
                    top + share.0 * height,
                    lane.area.x1,
                    top + share.1 * height,
                ),
                ..*lane
            }
        })
        .collect()
}

/// Where Super+Left/Right takes focus on a partitioned strip.
enum Step {
    /// Another window on the same display.
    Within(StripId, GroupId, u64),
    /// Onto the neighbouring display, landing on the group nearest the seam.
    Across(u32, Option<GroupId>),
    /// Nowhere: already at the outermost window of the outermost display.
    Stay,
}

impl ShellOverlayHost {
    /// Whether the desktop is laid out one sub-row per display.
    pub fn is_partitioned(&self) -> bool {
        self.outputs.len() >= 2
            && self
                .chrome
                .as_ref()
                .is_some_and(|c| c.borrow().tree().desktop.outputs.displays.len() >= 2)
    }

    /// The display the wheel disc and the full top bar live on: the first
    /// the backend lists, which is the primary chosen in Settings.
    pub(super) fn primary_output(&self) -> Option<OutputGeometry> {
        distinct_outputs(&self.outputs)
            .first()
            .map(|o| (*o).clone())
    }

    /// The focused display, while the desktop is partitioned; the primary
    /// when focus names no display that is still connected.
    pub(super) fn focused_output(&self) -> Option<OutputGeometry> {
        if !self.is_partitioned() {
            return None;
        }
        let focused = {
            let state = self.chrome.as_ref()?.borrow();
            let tree = state.tree();
            tree.desktop.outputs.focused_display()
        };
        let outputs = distinct_outputs(&self.outputs);
        focused
            .and_then(|id| outputs.iter().find(|o| o.id == id))
            .or_else(|| outputs.first())
            .map(|o| (*o).clone())
    }

    /// Where a desktop-wide overlay opens — palette, toasts, prompts,
    /// help: the focused display, or the whole desktop when there is one.
    pub fn overlay_area(&self) -> Rect {
        self.focused_output().map_or(self.rect, |o| o.rect())
    }

    /// The display holding the full top bar, which the tray's popovers
    /// hang from.
    pub fn tray_area(&self) -> Rect {
        self.primary_output()
            .filter(|_| self.is_partitioned())
            .map_or(self.rect, |p| p.rect())
    }

    /// Wheel-disc slot plus its gap, sized for the display it sits on.
    pub(super) fn wheel_clearance(&self) -> f64 {
        if self.sidebar.is_none() {
            return 0.0;
        }
        let height = match self.primary_output() {
            Some(primary) if self.is_partitioned() => primary.height,
            _ => self.rect.height(),
        };
        workspace_rail_clearance_for_viewport(height)
    }

    /// The wheel disc's width, sized for the display it sits on.
    pub(super) fn sidebar_width(&self) -> f32 {
        let height = match self.primary_output() {
            Some(primary) if self.is_partitioned() => primary.height,
            _ => self.rect.height(),
        };
        sidebar_width_for_viewport(ui::num::f32_of_f64(height))
    }

    /// Bring the partition in line with the displays just reported. Called
    /// when the output set or arrangement changes.
    pub(super) fn reconcile_display_partition(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let displays = displays_of(&self.outputs);
        let changed = {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            tree.reconcile_displays(&displays)
        };
        if changed {
            self.reframe_desktop();
            chrome.borrow_mut().bump();
        }
    }

    /// Frame each strip's focused group again on the next layout.
    fn reframe_desktop(&mut self) {
        if let Some(surface) = self.desktop_surface_mut() {
            surface.reframe_all();
        }
    }

    /// Give every window group without a display one. A group a row created
    /// from a tab dropped onto a display takes that display; any other new
    /// group — an app just opened, a window a collaborator added — the
    /// display with focus.
    pub(super) fn place_new_groups(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if !self.is_partitioned() {
            return;
        }
        let from_rows: Vec<(GroupId, u32)> = {
            let state = chrome.borrow();
            let tree = state.tree();
            let home = &tree.desktop.outputs.home;
            let unhomed = tree
                .desktop
                .workspaces
                .iter()
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .any(|g| !home.contains_key(&g.id));
            if !unhomed {
                return;
            }
            let from_rows = self
                .group_row_ptrs
                .values()
                .flat_map(|&ptr| {
                    // SAFETY: see field doc on `group_row_ptrs`.
                    let row: &GroupRow = unsafe { &*ptr };
                    row.groups
                        .iter()
                        .filter(|g| !home.contains_key(&g.id))
                        .filter_map(|g| row.assigned_output_of_group(g.id).map(|o| (g.id, o)))
                        .collect::<Vec<_>>()
                })
                .collect();
            drop(tree);
            drop(state);
            from_rows
        };
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            for (group, display) in from_rows {
                tree.desktop.outputs.place_new(group, display);
            }
            let desktop = &mut tree.desktop;
            desktop.outputs.place_unhomed(&desktop.workspaces);
            tree.queue_save();
        }
        chrome.borrow_mut().bump();
    }

    /// One lane per display, in reading order, over the display's whole
    /// work area: what [`in_band`] shares among the strips.
    fn display_lanes(&self) -> Vec<StripLane> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        if !self.is_partitioned() {
            return Vec::new();
        }
        let (order, primary) = {
            let state = chrome.borrow();
            let tree = state.tree();
            (
                tree.desktop.outputs.reading_order(),
                tree.desktop.outputs.primary(),
            )
        };
        let top = self
            .outputs
            .iter()
            .map(|o| o.y)
            .fold(f64::INFINITY, f64::min);
        let wheel = self.wheel_clearance();
        order
            .iter()
            .filter_map(|id| {
                let o = self.outputs.iter().find(|o| o.id == *id)?;
                let leading = Some(o.id) == primary && self.sidebar.is_some();
                // A display below the desktop's top edge draws its own bar
                // along its top; the windows start beneath it.
                let band = if o.y > top + 0.5 { own_top_band() } else { 0.0 };
                let gutter = if leading { wheel } else { 0.0 };
                Some(StripLane {
                    output: o.id,
                    area: Rect::new(o.x, o.y + band, o.x + o.width, o.y + o.height),
                    usable: (o.width - gutter - STRIP_TRAILING_GUTTER_PX).max(1.0),
                    leading,
                })
            })
            .collect()
    }

    fn fullscreen_lane_area(&self, assignment: &HashMap<GroupId, u32>) -> Option<(u32, Rect)> {
        let group = self.chrome.as_ref()?.borrow().fullscreen_group()?;
        let output = *assignment.get(&group)?;
        let display = self.outputs.iter().find(|display| display.id == output)?;
        Some((output, display.rect()))
    }

    /// Push the partition to every row and place the top bars: the full
    /// one on the primary display, a compact one on each other display.
    /// Cheap when nothing changed, so it runs every layout pass.
    pub(super) fn push_display_layout(&mut self) {
        let lanes = self.display_lanes();
        let (assignment, focused, strips): (HashMap<u64, u32>, Option<u32>, Vec<StripId>) =
            match self.chrome.as_ref() {
                Some(chrome) if !lanes.is_empty() => {
                    let state = chrome.borrow();
                    let tree = state.tree();
                    let partition = &tree.desktop.outputs;
                    let ws = tree.desktop.active_workspace();
                    let assignment = ws
                        .map(|ws| {
                            ws.strips
                                .iter()
                                .flat_map(|s| &s.window_groups)
                                .filter_map(|g| partition.display_of(g.id).map(|d| (g.id, d)))
                                .collect()
                        })
                        .unwrap_or_default();
                    let strips = ws
                        .map(|ws| ws.strips.iter().map(|s| s.id).collect())
                        .unwrap_or_default();
                    let focused = partition.focused_display();
                    drop(tree);
                    drop(state);
                    (assignment, focused, strips)
                }
                _ => (HashMap::new(), None, Vec::new()),
            };
        // Rows in the order the workspace column stacks them, with where
        // they were last laid out.
        let rows: Vec<(StripId, Rect)> = strips
            .iter()
            .filter_map(|id| {
                let &ptr = self.group_row_ptrs.get(id)?;
                // SAFETY: see field doc on `group_row_ptrs`.
                Some((*id, unsafe { (&*ptr).get_rect() }))
            })
            .collect();
        let rects: Vec<Rect> = rows.iter().map(|(_, rect)| *rect).collect();
        let bands = strip_bands(&rects, self.rect.y0 + own_top_band());
        let fullscreen_area = self.fullscreen_lane_area(&assignment);
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // A row of a strip outside the active workspace keeps one
            // continuous row until the next rebuild tears it down.
            let mut lanes = rows
                .iter()
                .position(|(id, _)| *id == strip_id)
                .and_then(|k| bands.get(k))
                .map(|band| in_band(&lanes, *band))
                .unwrap_or_default();
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            // Fullscreen owns the output, not the previous tiled strip band.
            // Keeping that band leaves the client below the hidden top bar.
            if row.is_fullscreen()
                && let Some((output, area)) = fullscreen_area
                && let Some(lane) = lanes.iter_mut().find(|lane| lane.output == output)
            {
                lane.area = area;
                lane.usable = area.width();
                lane.leading = false;
            }
            row.set_lanes(&lanes, &assignment);
            if let Some(focused) = focused {
                row.set_focus_output(focused);
            }
        }
        let primary = self.primary_output().filter(|_| !lanes.is_empty());
        let others: Vec<(u32, Rect)> = if primary.is_some() {
            let primary_id = primary.as_ref().map(|p| p.id);
            distinct_outputs(&self.outputs)
                .into_iter()
                .filter(|o| Some(o.id) != primary_id)
                .map(|o| (o.id, o.rect()))
                .collect()
        } else {
            Vec::new()
        };
        let presenting = self.focused_output();
        let beside_rail = presenting
            .as_ref()
            .is_some_and(|o| primary.as_ref().is_some_and(|p| p.id == o.id));
        if let Some(surface) = self.desktop_surface_mut() {
            if let Some(bar) = surface.topbar_mut() {
                bar.set_placement(primary.map(|p| p.rect()));
            }
            if let Some(launchpad) = surface
                .empty_workspace_mut()
                .and_then(ui::widget::Widget::as_any_mut)
                .and_then(|w| w.downcast_mut::<tilo_desktop::launchpad::CollaborationLaunchpad>())
            {
                launchpad.set_presentation_area(presenting.map(|o| o.rect()), beside_rail);
            }
            if let Some(bars) = surface
                .display_chrome_mut()
                .and_then(ui::widget::Widget::as_any_mut)
                .and_then(|layer| layer.downcast_mut::<DisplayBars>())
            {
                bars.set_displays(&others);
            }
        }
    }

    /// Where each top bar is drawn: the full bar (flagged `true`) and every
    /// compact one, by display.
    pub fn top_bar_rects(&self) -> Vec<(u32, Rect, bool)> {
        let Some(surface) = self.content.as_any().and_then(|content| {
            content.downcast_ref::<tilo_desktop::composition::DesktopSurface>()
        }) else {
            return Vec::new();
        };
        let Some(full) = surface
            .topbar()
            .map(tilo_desktop::topbar::TopBarChrome::bar_rect)
        else {
            return Vec::new();
        };
        let primary = self.primary_output().map_or(0, |p| p.id);
        let compact = surface
            .display_chrome()
            .and_then(ui::widget::Widget::as_any)
            .and_then(|layer| layer.downcast_ref::<DisplayBars>())
            .map(DisplayBars::bar_rects)
            .unwrap_or_default();
        let mut rects = vec![(primary, full, true)];
        rects.extend(compact.into_iter().map(|(id, rect)| (id, rect, false)));
        rects
    }

    /// Keep the partition's idea of the focused display in step with
    /// keyboard focus: the active group's display is the focused one, and
    /// it is remembered as that display's last focused group.
    pub(super) fn follow_focus_across_displays(&mut self) {
        if !self.is_partitioned() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let active = {
            let state = chrome.borrow();
            let tree = state.tree();
            let strip = tree.desktop.active_strip.get(&tree.desktop.active).copied();
            let active = strip.and_then(|s| tree.desktop.active_group_for(s).map(|g| (s, g)));
            drop(tree);
            drop(state);
            active
        };
        if active == self.last_noted_focus {
            return;
        }
        self.last_noted_focus = active;
        if let Some((strip, group)) = active {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            tree.desktop.outputs.note_focus(strip, group);
        }
    }

    /// The pointer moved to `p`: when that is another display, focus
    /// follows it there — to the group last focused on that display, or to
    /// no group when it has none, so the next window opens there.
    pub(super) fn focus_display_at(&mut self, p: Point) {
        if !self.is_partitioned() {
            return;
        }
        let displays: Vec<OutputGeometry> = distinct_outputs(&self.outputs)
            .into_iter()
            .cloned()
            .collect();
        let Some(display) = crate::shell::output_scene::output_at(&displays, p).map(|o| o.id)
        else {
            return;
        };
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if chrome.borrow().tree().desktop.outputs.focused_display() == Some(display) {
            return;
        }
        self.focus_display(display, None);
        chrome.borrow_mut().bump();
    }

    /// Move focus to `display`. The group taken is `pick` when given, else
    /// the one last focused there, else its first; a display with no
    /// window in the active strip clears the strip's focused group so a new
    /// window opens on it.
    fn focus_display(&mut self, display: u32, pick: Option<GroupId>) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let target = {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            tree.desktop.outputs.set_focused(display);
            let Some(strip_id) = tree.desktop.active_strip.get(&tree.desktop.active).copied()
            else {
                return true;
            };
            let target = tree
                .desktop
                .active_workspace()
                .and_then(|ws| ws.strips.iter().find(|s| s.id == strip_id))
                .and_then(|strip| {
                    pick.filter(|g| {
                        tree.desktop.outputs.display_of(*g) == Some(display)
                            && strip.window_groups.iter().any(|w| w.id == *g)
                    })
                    .or_else(|| tree.desktop.outputs.focus_target(strip, display))
                    .and_then(|g| {
                        strip
                            .window_groups
                            .iter()
                            .find(|w| w.id == g)
                            .map(|w| (strip_id, g, w.active_tab))
                    })
                });
            if target.is_none() {
                tree.desktop.active_group.remove(&strip_id);
                self.last_noted_focus = None;
            }
            drop(tree);
            drop(state);
            target
        };
        match target {
            Some((strip, group, tab)) => self.focus_window_on_strip(strip, group, tab),
            None => true,
        }
    }

    /// Super+Left/Right on a partitioned strip: along the focused display's
    /// windows, and past its first or last one onto the neighbouring
    /// display. `None` when the strip isn't partitioned, so the caller walks
    /// the whole strip instead.
    pub(super) fn step_focus_across_displays(&mut self, steps: i32) -> Option<bool> {
        if !self.is_partitioned() || steps == 0 {
            return None;
        }
        let chrome = self.chrome.clone()?;
        let step = {
            let state = chrome.borrow();
            let tree = state.tree();
            let partition = &tree.desktop.outputs;
            let ws = tree.desktop.active_workspace()?;
            let strip = tree
                .desktop
                .active_strip
                .get(&ws.id)
                .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
                .or_else(|| ws.strips.first())?;
            let display = partition.focused_display()?;
            let on: Vec<GroupId> = partition.groups_on(strip, display);
            let windows: Vec<(GroupId, u64)> = on
                .iter()
                .filter_map(|g| strip.window_groups.iter().find(|w| w.id == *g))
                .flat_map(|g| g.tabs.iter().map(|t| (g.id, t.id)))
                .collect();
            let current = tree
                .desktop
                .active_group_for(strip.id)
                .filter(|g| on.contains(g))
                .and_then(|g| strip.window_groups.iter().find(|w| w.id == g))
                .and_then(|g| {
                    windows
                        .iter()
                        .position(|&(wg, t)| wg == g.id && t == g.active_tab)
                });
            let target = match current {
                Some(at) => i64::try_from(at)
                    .ok()
                    .map(|at| at.saturating_add(i64::from(steps))),
                None => None,
            };
            let last = i64::try_from(windows.len()).unwrap_or(i64::MAX);
            let step = match target {
                Some(t) if t >= 0 && t < last => usize::try_from(t)
                    .ok()
                    .and_then(|t| windows.get(t))
                    .map_or(Step::Stay, |&(g, tab)| Step::Within(strip.id, g, tab)),
                _ => match partition.neighbour(display, steps.signum()) {
                    Some(next) => {
                        let arriving = partition.groups_on(strip, next);
                        // Entering from the left lands on the first window,
                        // from the right on the last — the one nearest the
                        // seam just crossed.
                        let pick = if steps > 0 {
                            arriving.first().copied()
                        } else {
                            arriving.last().copied()
                        };
                        Step::Across(next, pick)
                    }
                    None => Step::Stay,
                },
            };
            drop(tree);
            drop(state);
            step
        };
        Some(match step {
            Step::Within(strip, group, tab) => self.focus_window_on_strip(strip, group, tab),
            Step::Across(display, pick) => {
                let moved = self.focus_display(display, pick);
                chrome.borrow_mut().bump();
                moved
            }
            Step::Stay => false,
        })
    }

    /// Move the focused window group to the display `direction` places next
    /// to its own (negative for the left). It keeps focus and takes its
    /// place in the strip's shared order among that display's groups.
    pub fn move_focused_group_to_display(&mut self, direction: i32) -> bool {
        if !self.is_partitioned() {
            return false;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let moved = {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let Some(strip) = tree.desktop.active_strip.get(&tree.desktop.active).copied() else {
                return false;
            };
            let Some(group) = tree.desktop.active_group_for(strip) else {
                return false;
            };
            let Some(from) = tree.desktop.outputs.display_of(group) else {
                return false;
            };
            let Some(to) = tree.desktop.outputs.neighbour(from, direction.signum()) else {
                return false;
            };
            tree.move_group_to_display(group, to)
        };
        if moved {
            self.reframe_desktop();
            chrome.borrow_mut().bump();
        }
        moved
    }

    /// Reordering past the last group on a display carries the group onto
    /// the neighbouring display, where it lands beside the seam it crossed.
    pub(super) fn carry_focused_group_across_seam(&mut self, direction: i32) -> bool {
        if direction == 0 || !self.move_focused_group_to_display(direction) {
            return false;
        }
        let Some(chrome) = self.chrome.clone() else {
            return true;
        };
        let reorder = {
            let state = chrome.borrow();
            let tree = state.tree();
            let reorder = (|| {
                let partition = &tree.desktop.outputs;
                let strip_id = *tree.desktop.active_strip.get(&tree.desktop.active)?;
                let group = tree.desktop.active_group_for(strip_id)?;
                let display = partition.display_of(group)?;
                let strip = tree
                    .desktop
                    .active_workspace()?
                    .strips
                    .iter()
                    .find(|s| s.id == strip_id)?;
                let current = strip.window_groups.iter().position(|g| g.id == group)?;
                let mut neighbours = strip
                    .window_groups
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.id != group && partition.display_of(g.id) == Some(display))
                    .map(|(i, _)| i);
                // `to` is the index after the group leaves its old slot.
                let to = if direction > 0 {
                    let first = neighbours.next()?;
                    if first > current {
                        first.saturating_sub(1)
                    } else {
                        first
                    }
                } else {
                    let last = neighbours.next_back()?;
                    if last < current {
                        last.saturating_add(1)
                    } else {
                        last
                    }
                };
                (to != current).then_some((strip_id, group, to))
            })();
            drop(tree);
            reorder
        };
        if let Some((strip, group, to)) = reorder {
            let reorder = tilo_desktop::intents::SessionIntent::ReorderGroup { strip, group, to };
            if Self::dispatch_desktop_intents(
                &chrome,
                [tilo_desktop::intents::DesktopIntent::Session(reorder)],
            ) > 0
            {
                chrome.borrow_mut().bump();
            }
        }
        true
    }

    /// The groups of the active strip on each display, in the order they
    /// are laid out, for the state dump: every connected display, whether or
    /// not the desktop is partitioned. A device that has only ever had one
    /// display carries no partition, and that display shows the whole strip.
    pub fn display_partition_summary(&self) -> Vec<(u32, Vec<GroupId>)> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let state = chrome.borrow();
        let tree = state.tree();
        let partition = &tree.desktop.outputs;
        let Some(strip) = tree.desktop.active_workspace().and_then(|ws| {
            tree.desktop
                .active_strip
                .get(&ws.id)
                .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
                .or_else(|| ws.strips.first())
        }) else {
            return Vec::new();
        };
        let summary = if partition.displays.is_empty() {
            self.primary_output()
                .map(|display| {
                    (
                        display.id,
                        strip.window_groups.iter().map(|g| g.id).collect(),
                    )
                })
                .into_iter()
                .collect()
        } else {
            partition
                .reading_order()
                .into_iter()
                .map(|d| (d, partition.groups_on(strip, d)))
                .collect()
        };
        drop(tree);
        drop(state);
        summary
    }
}
