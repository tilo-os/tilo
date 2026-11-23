//! Each display holds its own window groups.
//!
//! With more than one display a strip is one section per display: every window
//! group belongs to exactly one display (the tree's
//! [`OutputPartition`](tilo_desktop_model::OutputPartition)), is laid out,
//! panned and clipped there only, and is never drawn on another. Every
//! display keeps the strip's edge gutters at both ends of its section. New
//! windows open on the display with focus; focus moves between displays by
//! stepping past a display's last window, by pressing on a display, or by
//! asking for a display; a window moves to the neighbouring display on
//! request. The strip's order stays the shared one; the row lays it out
//! display by display ([`OutputPartition::section_order`]).
//!
//! [`OutputPartition::section_order`]: tilo_desktop_model::OutputPartition::section_order

use std::collections::HashMap;

use tilo_desktop_model::{Display, GroupId, Layout, Strip, StripId, TabId};
use ui::kurbo::{Point, Rect};
use ui::widget::{STRIP_EDGE_GUTTER_PX, StripSection};

use super::{DesktopHandle, Shared};
use crate::desktop::navigation::{Admitted, RouteIds};

/// Where stepping focus along a strip spread over several displays goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Another window of the strip.
    Window(GroupId, TabId),
    /// Onto display `.0` , to its window `.1` nearest the border crossed, or to none when it shows no
    /// window.
    Display(u32, Option<GroupId>),
}

/// The step `steps` windows from `from` (the focused window, when focus is on one) along `strip` .
/// On one display that walks the strip's windows and stops at either end. Across several it walks
/// the focused display's windows and, past its first or last, crosses to the neighbouring display,
/// landing on the window nearest the border.
pub fn step(
    tree: &Layout,
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

/// One section per display of `displays` (in the surface's coordinates), in
/// `order`, for a row laid out at `row` in a surface at `surface`: each
/// display's section keeps the insets the row keeps from the surface's edges,
/// taken from that display's own top and bottom, and the strip's gutters
/// at both its ends.
pub fn sections(
    displays: &[Display],
    order: &[u32],
    surface: Rect,
    row: Rect,
) -> Vec<StripSection> {
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
            StripSection {
                output: display.id,
                area,
                usable: (display.width - 2.0 * STRIP_EDGE_GUTTER_PX).max(1.0),
                leading: true,
            }
        })
        .collect()
}

/// What one strip on the shell is laid out and shown as: the displays it is shown on, its window
/// groups in section order (display by display), and which display each belongs to.
pub struct StripPlan {
    pub strip: StripId,
    pub displays: Vec<u32>,
    pub groups: Vec<GroupId>,
    pub assignment: HashMap<u64, u32>,
}

/// Which strip each display shows in the active workspace, one plan per strip with the displays
/// showing it. Every display shows one, and the display focus is on shows the active strip
/// ([`Layout::strip_shown_on`]).
pub fn display_strips(tree: &Layout) -> Vec<StripPlan> {
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
            .section_order(strip)
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
pub fn display_at(displays: &[Display], surface: Rect, p: Point) -> Option<u32> {
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
    pub fn focus_display(&mut self, display: u32, pick: Option<GroupId>) -> bool {
        let tree = &mut self.tree;
        if !tree.is_partitioned() || !tree.outputs.displays.iter().any(|d| d.id == display) {
            return false;
        }
        // The strip the display shows, read before focus moves: the focused display answers with
        // the active strip.
        let own = tree
            .active_workspace()
            .and_then(|workspace| tree.outputs.shown_on(display, workspace.id()));
        let before = (tree.outputs.focused_display(), tree.active_group.clone());
        tree.outputs.set_focused(display);
        if let Some(own) = own {
            tree.show_strip_on(display, own);
        }
        let target = tree.active_workspace().and_then(|workspace| {
            let strip_id = tree.active_strip_for(workspace)?;
            let strip = workspace.strips.iter().find(|s| s.id == strip_id)?;
            let group = pick
                .filter(|g| tree.outputs.display_of(*g) == Some(display))
                .or_else(|| tree.outputs.focus_target(strip, display));
            let route = group.and_then(|group| {
                let model = strip.window_groups.iter().find(|g| g.id == group)?;
                Some(RouteIds {
                    workspace: workspace.id(),
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
    pub fn step_focus(&mut self, steps: i32) -> bool {
        // A step queued since the last update has moved focus already.
        let queued = self.queue.iter().rev().find_map(|admitted| match admitted {
            Admitted::Select(route) => Some(*route),
            _ => None,
        });
        let tree = &self.tree;
        let Some(workspace) = tree.active_workspace() else {
            return false;
        };
        let Some(strip_id) = tree.active_strip_for(workspace) else {
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
        match step(tree, strip, from, steps) {
            Some(Step::Window(group, tab)) => {
                self.queue.push(Admitted::Select(RouteIds {
                    workspace: workspace.id(),
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
    /// The displays the desktop spreads over, primary first, in the surface's coordinates; a mirror
    /// set is one display. Groups move between them as [`OutputPartition::reconcile`] says, and the
    /// surface lays each display's section out on its next pass. Whether anything changed.
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
    /// first. `None` while the shell carries no partition, as one display needs no telling apart.
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
        let Some(strip) = tree.active_strip_id() else {
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
        let moved = super::window_menu::move_to_display(&mut shared, group, to);
        if moved {
            shared.changed();
        }
        moved
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::Display;
    use tilo_desktop_model::{
        AppKind, Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
        WorkspaceId,
    };
    use ui::kurbo::Rect;
    use ui::widget::STRIP_EDGE_GUTTER_PX;

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
    fn docked(ids: &[GroupId], left: usize) -> Layout {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Office");
        let mut strip = Strip::new(StripId(1), "S");
        strip.window_groups = ids.iter().map(|id| group(*id)).collect();
        workspace.strips = vec![strip];
        let mut tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
        };
        assert!(tree.reconcile_displays(&[display(0, 0.0), display(1, 800.0)]));
        for (i, id) in ids.iter().enumerate() {
            let on = u32::from(i >= left);
            tree.outputs.move_group(&tree.workspaces, *id, on);
        }
        tree
    }

    fn strip(tree: &Layout) -> &Strip {
        &tree.workspaces[0].strips[0]
    }

    tilo_test_env::scenario! {
        fn each_display_has_its_section_and_takes_its_own_presses;

        phase shell_surfaces_stand_on_the_display_the_person_works_on;
        {
            let mut tree = docked(&[1, 2], 1);
            tree.outputs.set_focused(1);
            assert_eq!(
                crate::desktop::working_display(&tree),
                Some(Rect::new(800.0, 0.0, 1600.0, 600.0)),
                "the right display, not the shell across both"
            );
            tree.outputs.set_focused(0);
            assert_eq!(
                crate::desktop::working_display(&tree),
                Some(Rect::new(0.0, 0.0, 800.0, 600.0))
            );
            assert_eq!(crate::desktop::working_display(&Layout::default()), None, "one display");
        }

        phase each_display_gets_a_section_with_the_rows_insets_and_its_own_gutters;
        {
            let displays = [display(0, 0.0), display(1, 800.0)];
            let surface = Rect::new(0.0, 0.0, 1600.0, 600.0);
            let row = Rect::new(0.0, 40.0, 1600.0, 588.0);
            let sections = sections(&displays, &[1, 0], surface, row);
            assert_eq!(sections.len(), 2);
            assert_eq!(sections[0].output, 1, "in the order asked");
            assert_eq!(sections[0].area, Rect::new(800.0, 40.0, 1600.0, 588.0));
            assert_eq!(sections[1].area, Rect::new(0.0, 40.0, 800.0, 588.0));
            assert!(sections.iter().all(|section| section.leading));
            assert_eq!(sections[1].usable, 800.0 - 2.0 * STRIP_EDGE_GUTTER_PX);
        }

        phase a_shorter_display_keeps_the_bottom_gutter_above_its_own_edge;
        {
            let mut short = display(1, 800.0);
            short.height = 400.0;
            let surface = Rect::new(0.0, 0.0, 1600.0, 600.0);
            let row = Rect::new(0.0, 40.0, 1600.0, 588.0);
            let sections = sections(&[display(0, 0.0), short], &[0, 1], surface, row);
            assert_eq!(sections[1].area, Rect::new(800.0, 40.0, 1600.0, 388.0));
        }

        phase one_strip_on_both_displays_is_one_row_with_a_section_each;
        {
            let tree = docked(&[1, 2, 3], 2);
            let plans = display_strips(&tree);
            assert_eq!(plans.len(), 1, "both displays show the same strip");
            assert_eq!(plans[0].displays, [0, 1]);
            assert_eq!(plans[0].groups, [1, 2, 3], "the strip's order, by display");
            assert_eq!(plans[0].assignment.get(&1), Some(&0));
            assert_eq!(plans[0].assignment.get(&3), Some(&1));
        }

        phase a_press_finds_its_display;
        {
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

    #[test]
    fn one_display_steps_along_the_strip_and_stops_at_its_ends() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Office");
        let mut s = Strip::new(StripId(1), "S");
        s.window_groups = vec![group(1), group(2)];
        workspace.strips = vec![s];
        let tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
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
            "lands on the window nearest the border"
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
            "the strip lays every group out in its own display's section"
        );
        assert_eq!(plans[0].assignment.get(&3), Some(&1));
        assert_eq!(plans[1].groups, [4], "the other display's own strip");
        assert_eq!(plans[1].assignment.get(&4), Some(&1));
    }
}
