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

use tilo_desktop_model::{DesktopTree, Display, GroupId, Strip, StripId, TabId};
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::NodeId;
use ui::vello::Scene;
use ui::widget::{EventCtx, GroupRow, STRIP_EDGE_GUTTER_PX, StripLane, Widget as _};
use ui::winit::event::WindowEvent;
use ui::{TextRenderer, Theme};

use super::strip::build_row_lazily;
use super::{DesktopHandle, DesktopSurface, Shared};
use crate::navigation::{self, Admitted, RouteIds};

/// Where stepping focus along a strip spread over several displays goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Another window of the strip.
    Window(GroupId, TabId),
    /// Onto display `.0` , to its window `.1` nearest the seam crossed, or to none when it shows no
    /// window.
    Display(u32, Option<GroupId>),
}

/// The step `steps` windows from `from` (the focused window, when focus is on one) along `strip` .
/// On one display that walks the strip's windows and stops at either end. Across several it walks
/// the focused display's windows and, past its first or last, crosses to the neighbouring display,
/// landing on the window nearest the seam.
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
    // Past either end of the display, or no window focused on it: on to the neighbouring display,
    // onto the strip it shows.
    let next = partition.neighbour(display, steps.signum())?;
    let arriving = tree
        .strip_shown_on(next)
        .and_then(|shown| {
            tree.workspaces
                .iter()
                .flat_map(|w| &w.strips)
                .find(|s| s.id == shown)
        })
        .map_or_else(Vec::new, |shown| partition.groups_on(shown, next));
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

/// What one strip on the desk is laid out and shown as: the displays it is shown on, its window
/// groups in lane order (display by display), and which display each belongs to.
pub(crate) struct StripPlan {
    pub(crate) strip: StripId,
    pub(crate) displays: Vec<u32>,
    pub(crate) groups: Vec<GroupId>,
    pub(crate) assignment: HashMap<u64, u32>,
}

/// Which strip each display shows in the active workspace, one plan per strip with the displays
/// showing it. Every display shows one, and the display focus is on shows the active strip
/// ([`DesktopTree::strip_shown_on`]).
pub(crate) fn display_strips(tree: &DesktopTree) -> Vec<StripPlan> {
    let mut plans: Vec<StripPlan> = Vec::new();
    for display in &tree.outputs.displays {
        let Some(strip) = tree.strip_shown_on(display.id) else {
            continue;
        };
        match plans.iter_mut().find(|plan| plan.strip == strip) {
            Some(plan) => plan.displays.push(display.id),
            None => plans.push(StripPlan {
                strip,
                displays: vec![display.id],
                groups: Vec::new(),
                assignment: HashMap::new(),
            }),
        }
    }
    for plan in &mut plans {
        let Some(strip) = tree
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .find(|s| s.id == plan.strip)
        else {
            continue;
        };
        plan.groups = tree
            .outputs
            .lane_order(strip)
            .into_iter()
            .map(|g| g.id)
            .collect();
        plan.assignment = plan
            .groups
            .iter()
            .filter_map(|g| tree.outputs.display_of(*g).map(|d| (*g, d)))
            .collect();
    }
    plans
}

/// The display among `displays` (in the surface's coordinates, the surface at `surface` ) holding
/// point `p` .
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
    /// Move focus to `display` , onto `pick` when it is there, else the window last focused there,
    /// else its first; a display with no window takes focus with none, so the next window opens
    /// there. The strip the display shows becomes the active one, so focus and the strip the person
    /// works in travel together; every other display keeps its own.
    pub(super) fn focus_display(&mut self, display: u32, pick: Option<GroupId>) -> bool {
        let tree = &mut self.tree;
        if !tree.is_partitioned() || !tree.outputs.displays.iter().any(|d| d.id == display) {
            return false;
        }
        // The strip the display shows, read before focus moves: the focused display answers with
        // the active strip.
        let own = tree
            .active_workspace()
            .and_then(|workspace| tree.outputs.shown_on(display, workspace.slot));
        let before = (tree.outputs.focused_display(), tree.active_group.clone());
        tree.outputs.set_focused(display);
        if let Some(own) = own {
            tree.show_strip_on(display, own);
        }
        let target = tree.active_workspace().and_then(|workspace| {
            let strip_id = navigation::active_strip(tree, workspace)?;
            let strip = workspace.strips.iter().find(|s| s.id == strip_id)?;
            let group = pick
                .filter(|g| tree.outputs.display_of(*g) == Some(display))
                .or_else(|| tree.outputs.focus_target(strip, display));
            let route = group.and_then(|group| {
                let model = strip.window_groups.iter().find(|g| g.id == group)?;
                Some(RouteIds {
                    workspace: workspace.slot,
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

    /// Step focus `steps` windows along the shown strip ([`step`]): queue the window it lands on,
    /// or move focus to the display it crosses to. False when there is nowhere to go.
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
        let slot = workspace.slot;
        match step(tree, strip, from, steps) {
            Some(Step::Window(group, tab)) => {
                self.queue.push(Admitted::Select(RouteIds {
                    workspace: slot,
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

impl DesktopSurface {
    /// Build the rows of the strips the other displays show, so every display has the strip it
    /// keeps. The one the person works in is the shown row; these are drawn behind it, each in its
    /// own display's lane.
    pub(super) fn show_display_strips(&mut self) {
        let shared = self.shared.borrow();
        let tree = &shared.tree;
        let mut behind = Vec::new();
        if tree.is_partitioned() {
            let workspace = tree.active_workspace();
            for plan in display_strips(tree) {
                if Some(plan.strip) == self.shown || behind.contains(&plan.strip) {
                    continue;
                }
                if !self.rows.contains_key(&plan.strip)
                    && let Some(workspace) = workspace
                    && let Some(strip) = workspace.strips.iter().find(|s| s.id == plan.strip)
                {
                    let focused = tree.active_group_for(plan.strip);
                    let (mut row, deferred) = build_row_lazily(
                        self.tiles.as_mut(),
                        self.controls.as_ref(),
                        workspace,
                        strip,
                        focused,
                    );
                    row.set_fullscreen(tree.fullscreen_group(), false);
                    self.unbuilt
                        .extend(deferred.into_iter().map(|tab| (plan.strip, tab)));
                    self.rows.insert(plan.strip, row);
                }
                behind.push(plan.strip);
            }
        }
        if self.behind != behind {
            self.behind = behind;
            self.structure_dirty = true;
        }
    }

    /// The strip a pointer event belongs to: the row under the pointer, else the one the person
    /// works in. A key or a resize belongs to that one.
    pub(super) fn strip_for_pointer(&self, event: &WindowEvent) -> Option<StripId> {
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        );
        pointer
            .then(|| self.strip_under_pointer())
            .flatten()
            .or(self.shown)
    }

    /// Draw the rows of the strips the other displays show, before the one the person works in.
    pub(super) fn draw_display_strips(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for strip in self.behind.clone() {
            if let Some(row) = self.rows.get_mut(&strip) {
                ui::widget::draw_child(row, scene, transform, text, theme, presence);
            }
        }
    }

    /// Hand `event` to the row it belongs to. The row's own node finds it,
    /// whatever order the rows were built in: a press moves the person's
    /// display, and with it which row stands in front.
    pub(super) fn route_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        children: &[NodeId],
    ) {
        let Some((strip, node)) = self
            .strip_for_pointer(event)
            .and_then(|strip| Some((strip, self.rows.get(&strip)?.layout_node()?)))
            .filter(|(_, node)| children.contains(node))
        else {
            return;
        };
        let path = ctx.path.clone();
        if let Some(row) = self.rows.get_mut(&strip) {
            row.handle_event(event, &mut ctx.reborrow(&path, node));
        }
    }

    /// A press on a display the person is not working on makes that display's strip the one they
    /// work in, so the window pressed answers the same press.
    pub(super) fn focus_pressed_display(&mut self) {
        if self
            .shown_row_ref()
            .is_some_and(|row| row.covers(self.pointer))
        {
            return;
        }
        self.focus_display_at(self.pointer);
        let before = self.shown;
        self.show_active_strip();
        if self.shown != before {
            self.present();
        }
    }
}

/// The rows `strips` names in `rows` , in that order.
pub(super) fn drawn_rows<'a>(
    rows: &'a HashMap<StripId, GroupRow>,
    strips: &[StripId],
) -> Vec<&'a GroupRow> {
    strips.iter().filter_map(|strip| rows.get(strip)).collect()
}

/// The rows `strips` names in `rows` , in that order, each writable.
pub(super) fn drawn_rows_mut<'a>(
    rows: &'a mut HashMap<StripId, GroupRow>,
    strips: &[StripId],
) -> Vec<&'a mut GroupRow> {
    let mut held: Vec<(StripId, &mut GroupRow)> = rows
        .iter_mut()
        .filter(|(strip, _)| strips.contains(strip))
        .map(|(strip, row)| (*strip, row))
        .collect();
    held.sort_by_key(|(strip, _)| strips.iter().position(|drawn| drawn == strip));
    held.into_iter().map(|(_, row)| row).collect()
}

impl DesktopHandle {
    /// The displays the desktop spreads over, primary first, in the surface's coordinates; a mirror
    /// set is one display. Groups move between them as [`OutputPartition::reconcile`] says, and the
    /// surface lays each display's lane out on its next pass. Whether anything changed.
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

    /// The primary display: the one the person chose in Settings while it is connected, else the
    /// first. `None` while the desk carries no partition, as one display needs no telling apart.
    #[must_use]
    pub fn primary_display(&self) -> Option<u32> {
        self.read(|tree| tree.outputs.primary())
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

    /// Move focus to `display` , on the next update pass: to the window last focused there, else
    /// its first, else to no window, so the next one opens there. False when there is no such
    /// display.
    pub fn focus_display(&self, display: u32) -> bool {
        let focusable = self.read(|tree| {
            tree.is_partitioned() && tree.outputs.displays.iter().any(|d| d.id == display)
        });
        focusable && {
            self.shared.borrow_mut().focus_display(display, None);
            true
        }
    }

    /// Step focus `steps` windows along the shown strip, on the next update pass, crossing to the
    /// neighbouring display past either end of the focused one. False when there is nowhere to go.
    pub fn step_focus(&self, steps: i32) -> bool {
        self.shared.borrow_mut().step_focus(steps)
    }

    /// Move the focused window to the display `steps` places to its right (left when negative). It
    /// keeps focus, and its place in the strip's order among that display's windows. False when
    /// there is no such display or no focused window.
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
    /// Bring the partition and every display's row in line with the
    /// displays: a window without a display gets the focused one, the
    /// display of the focused window is the focused display, and each
    /// display's strip lays its windows out in that display's lane, display
    /// by display. A strip shown on several displays keeps a lane on each;
    /// the strips the other displays show are laid out behind it. On one
    /// display the shown row is one continuous strip again.
    pub(super) fn lay_out_displays(&mut self) {
        let surface = self.rect;
        let plans = {
            let mut shared = self.shared.borrow_mut();
            let tree = &mut shared.tree;
            if tree.is_partitioned() {
                tree.outputs.place_unhomed(&tree.workspaces);
                if let Some(strip_id) = self.shown
                    && let Some(group) = tree.active_group_for(strip_id)
                {
                    tree.outputs.note_focus(strip_id, group);
                }
                // Every display keeps the strip it shows; one never told takes the active strip,
                // and the focused display follows it.
                tree.note_shown_strip();
                let displays = tree.outputs.displays.clone();
                let order = tree.outputs.reading_order();
                let focused = tree.outputs.focused_display();
                Some((display_strips(tree), displays, order, focused))
            } else {
                None
            }
        };
        let shown = self.shown;
        let Some((plans, all, order, focused)) = plans else {
            self.behind.clear();
            if let Some(row) = shown.and_then(|strip| self.rows.get_mut(&strip)) {
                row.set_lanes(&[], &HashMap::new());
            }
            return;
        };
        let mut behind = Vec::new();
        for plan in plans {
            if Some(plan.strip) != shown {
                behind.push(plan.strip);
            }
            let Some(row) = self.rows.get_mut(&plan.strip) else {
                continue;
            };
            put_in_lane_order(row, &plan.groups);
            let lanes = lanes(&all, &order, surface, row.get_rect());
            row.set_lanes(&lanes, &plan.assignment);
            row.set_shown_outputs(&plan.displays);
            if let Some(focused) = focused.filter(|focused| plan.displays.contains(focused)) {
                row.set_focus_output(focused);
            }
        }
        behind.sort_unstable();
        behind.dedup();
        if self.behind != behind {
            self.behind = behind;
            self.structure_dirty = true;
        }
    }

    /// The strips the surface draws this frame: the ones other displays show, then the focused
    /// display's, which is the row the person works in.
    pub(super) fn drawn_strips(&self) -> Vec<StripId> {
        self.behind
            .iter()
            .copied()
            .chain(self.shown)
            .filter(|strip| self.rows.contains_key(strip))
            .collect()
    }

    /// The strip shown on the display under the pointer: the row a pointer event belongs to.
    pub(super) fn strip_under_pointer(&self) -> Option<StripId> {
        self.behind.iter().copied().chain(self.shown).find(|strip| {
            self.rows
                .get(strip)
                .is_some_and(|row| row.covers(self.pointer))
        })
    }

    /// A press at `p` on no window: across several displays, focus moves to the display pressed on.
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
        Workspace, WorkspaceSlot,
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
                view: String::new(),
                pinned: false,
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
            primary: false,
        }
    }

    /// A tree whose one strip holds groups `ids` , the first `left` of them on the left display.
    fn docked(ids: &[GroupId], left: usize) -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Desk");
        let mut strip = Strip::new(StripId(1), "S");
        strip.window_groups = ids.iter().map(|id| group(*id)).collect();
        workspace.strips = vec![strip];
        let mut tree = DesktopTree {
            active: workspace.slot,
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
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Desk");
        let mut s = Strip::new(StripId(1), "S");
        s.window_groups = vec![group(1), group(2)];
        workspace.strips = vec![s];
        let tree = DesktopTree {
            active: workspace.slot,
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
    fn one_strip_on_both_displays_is_one_row_with_a_lane_each() {
        let tree = docked(&[1, 2, 3], 2);
        let plans = display_strips(&tree);
        assert_eq!(plans.len(), 1, "both displays show the same strip");
        assert_eq!(plans[0].displays, [0, 1]);
        assert_eq!(plans[0].groups, [1, 2, 3], "the strip's order, by display");
        assert_eq!(plans[0].assignment.get(&1), Some(&0));
        assert_eq!(plans[0].assignment.get(&3), Some(&1));
    }

    #[test]
    fn each_display_lays_out_the_strip_it_shows() {
        let mut tree = docked(&[1, 2, 3], 2);
        let mut other = Strip::new(StripId(2), "T");
        other.window_groups = vec![group(4)];
        tree.workspaces[0].strips.push(other);
        tree.outputs.move_group(&tree.workspaces, 4, 1);
        tree.outputs.set_focused(0);
        assert!(tree.show_strip_on(1, StripId(2)));

        let plans = display_strips(&tree);
        assert_eq!(
            plans
                .iter()
                .map(|plan| (plan.strip, plan.displays.clone()))
                .collect::<Vec<_>>(),
            [(StripId(1), vec![0]), (StripId(2), vec![1])],
            "a plan per strip, with the displays showing it"
        );
        assert_eq!(
            plans[0].groups,
            [1, 2, 3],
            "the strip lays every group out in its own display's lane"
        );
        assert_eq!(plans[0].assignment.get(&3), Some(&1));
        assert_eq!(plans[1].groups, [4], "the other display's own strip");
        assert_eq!(plans[1].assignment.get(&4), Some(&1));
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
