//! Each display holds its own window groups.
//!
//! With more than one display, a strip is one sub-row per display: every
//! window group belongs to exactly one display ([`OutputPartition`]), is laid
//! out, panned and clipped there only, and is never drawn on another. The
//! primary display carries the wheel disc and the full top bar; every other
//! display a compact bar along its own top edge. New windows open on the
//! display with focus; focus moves between displays with the pointer and
//! with Super+Left/Right past a display's last window, and a group moves to
//! the neighbouring display with the configured chord.
//!
//! The partition is this device's layout, kept in the local desktop tree and
//! never replicated. The window groups' order stays the strip's; the rows
//! lay them out display by display.

use std::collections::HashMap;

use ui::widget::{GroupRow, StripLane};
use vello::kurbo::{Point, Rect};

use super::{
    OutputGeometry, STRIP_TRAILING_GUTTER_PX, ShellOverlayHost, sidebar_width_for_viewport,
    workspace_rail_clearance_for_viewport,
};
use crate::shell::state::{Display, GroupId, OutputPartition, Strip, StripId, WindowGroup};
use crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX;

/// What a display that doesn't touch the desktop's top edge keeps along its
/// own top edge: its top bar and the tiled layout's top rhythm (the same
/// spacing step the workspace column puts under the full bar).
fn own_top_band() -> f64 {
    f64::from(TOPBAR_HEIGHT_PX) + f64::from(ui::tokens::Spacing::BASE.s3)
}

/// The displays as the partition knows them, primary (`OutputId(0)`) first.
fn displays_of(outputs: &[OutputGeometry]) -> Vec<Display> {
    let mut displays: Vec<Display> = outputs
        .iter()
        .filter(|o| o.width > 0.0 && o.height > 0.0)
        .map(|o| Display {
            id: o.id,
            x: o.x,
            y: o.y,
            width: o.width,
            height: o.height,
        })
        .collect();
    displays.sort_by_key(|d| d.id);
    displays.dedup_by_key(|d| d.id);
    displays
}

/// A strip's groups in the order its row lays them out: display by display
/// in reading order, each display's in strip order. The strip's own order
/// when it isn't partitioned.
pub(crate) fn display_ordered<'a>(
    partition: &OutputPartition,
    strip: &'a Strip,
) -> Vec<&'a WindowGroup> {
    let mut groups: Vec<&WindowGroup> = strip.window_groups.iter().collect();
    if partition.displays.len() < 2 {
        return groups;
    }
    let order = partition.reading_order();
    groups.sort_by_key(|g| {
        partition
            .display_of(g.id)
            .and_then(|d| order.iter().position(|id| *id == d))
            .unwrap_or(0)
    });
    groups
}

/// A dragged group's neighbours on its own display, before and after, so
/// a partitioned row's reorder can be written back into the strip's order.
pub(super) type ReorderNeighbours = Option<(Option<u64>, Option<u64>)>;

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

    /// The display the wheel disc and the full top bar live on.
    pub(super) fn primary_output(&self) -> Option<OutputGeometry> {
        self.outputs.iter().min_by_key(|o| o.id).copied()
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
        sidebar_width_for_viewport(super::f64_to_f32_lossy(height))
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
            self.last_scrolled_active_group.clear();
            chrome.borrow_mut().bump();
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
                tree.desktop.outputs.home.insert(group, display);
            }
            let desktop = &mut tree.desktop;
            desktop.outputs.place_unhomed(&desktop.workspaces);
            tree.flush_to_disk();
        }
        chrome.borrow_mut().bump();
    }

    /// One lane per display, in reading order, for every row.
    fn strip_lanes(&self) -> Vec<StripLane> {
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

    /// Push the partition to every row and place the top bars: the full
    /// one on the primary display, a compact one on each other display.
    /// Cheap when nothing changed, so it runs every layout pass.
    pub(super) fn push_display_layout(&self) {
        let lanes = self.strip_lanes();
        let (assignment, focused): (HashMap<u64, u32>, Option<u32>) = match self.chrome.as_ref() {
            Some(chrome) if !lanes.is_empty() => {
                let state = chrome.borrow();
                let tree = state.tree();
                let partition = &tree.desktop.outputs;
                let assignment = tree
                    .desktop
                    .active_workspace()
                    .map(|ws| {
                        ws.strips
                            .iter()
                            .flat_map(|s| &s.window_groups)
                            .filter_map(|g| partition.display_of(g.id).map(|d| (g.id, d)))
                            .collect()
                    })
                    .unwrap_or_default();
                let focused = partition.focused_display();
                drop(tree);
                drop(state);
                (assignment, focused)
            }
            _ => (HashMap::new(), None),
        };
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            row.set_lanes(&lanes, &assignment);
            if let Some(focused) = focused {
                row.set_focus_output(focused);
            }
        }
        let primary = self.primary_output().filter(|_| !lanes.is_empty());
        let others: Vec<(u32, Rect)> = if primary.is_some() {
            let primary_id = primary.map(|p| p.id);
            self.outputs
                .iter()
                .filter(|o| Some(o.id) != primary_id)
                .map(|o| (o.id, o.rect()))
                .collect()
        } else {
            Vec::new()
        };
        if let Some(parts) = self.workspace_scene_parts {
            // SAFETY: `WorkspaceSceneParts` points into the workspace root
            // owned by `self.content`, replaced with it on rebuild; layout
            // runs sequentially under `&mut self`.
            unsafe {
                (&mut *parts.topbar).set_placement(primary.map(|p| p.rect()));
                (&mut *parts.display_bars).set_displays(&others);
            }
        }
    }

    /// Where each top bar is drawn: the full bar (flagged `true`) and every
    /// compact one, by display.
    pub fn top_bar_rects(&self) -> Vec<(u32, Rect, bool)> {
        let Some(parts) = self.workspace_scene_parts else {
            return Vec::new();
        };
        let primary = self.primary_output().map_or(0, |p| p.id);
        // SAFETY: read-only use of the stable scene-part pointers.
        let (full, compact) = unsafe {
            (
                (&*parts.topbar).bar_rect(),
                (&*parts.display_bars).bar_rects(),
            )
        };
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
        let Some(display) = crate::shell::output_scene::output_at(&self.outputs, p).map(|o| o.id)
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
            tree.desktop.outputs.focused = Some(display);
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
    /// to its own (negative for the left). It keeps focus and lands after
    /// the group last focused there.
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
            self.last_scrolled_active_group.clear();
            chrome.borrow_mut().bump();
        }
        moved
    }

    /// The groups of the active strip on each display, in the order they
    /// are laid out, for the state dump.
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
        let summary = partition
            .reading_order()
            .into_iter()
            .map(|d| (d, partition.groups_on(strip, d)))
            .collect();
        drop(tree);
        drop(state);
        summary
    }
}
