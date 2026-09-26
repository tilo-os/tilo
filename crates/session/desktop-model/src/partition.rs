//! Which display each window group is shown on, on this device.
//!
//! A strip is one row of window groups, but a person with several displays
//! sees it as one sub-row per display: every group belongs to exactly one
//! display, is laid out and panned there only, and never appears on another.
//! Which display that is depends on the monitors this device happens to
//! have, so the partition is per-device layout state. It is persisted with
//! the device's own desktop and is never projected into a room document or
//! the device's whole-desktop document: a collaborator has their own
//! monitors and their own partition of the same strip.
//!
//! Group order stays the strip's: a display's sub-row is the strip's groups
//! that belong to it, in strip order. Moving a group onto a display places
//! it after the group last focused there, which is a reorder of the strip
//! the caller mirrors like any other.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ids::{GroupId, StripId};
use crate::layout::Strip;
use crate::workspace::Workspace;

/// A display as the partition knows it: the backend's stable output id and
/// where it sits in the logical desktop.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Display {
    fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Squared distance between the two rectangles' nearest edges; zero
    /// when they touch or overlap.
    fn gap_sq(&self, other: &Self) -> f64 {
        let dx = (other.x - (self.x + self.width))
            .max(self.x - (other.x + other.width))
            .max(0.0);
        let dy = (other.y - (self.y + self.height))
            .max(self.y - (other.y + other.height))
            .max(0.0);
        dx * dx + dy * dy
    }
}

/// The per-device assignment of window groups to displays.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputPartition {
    /// The display each group is shown on.
    #[serde(default)]
    pub home: BTreeMap<GroupId, u32>,
    /// Groups moved off a display that went away, and that display. They
    /// return when it comes back, unless they were moved since.
    #[serde(default)]
    pub origin: BTreeMap<GroupId, u32>,
    /// The displays last reconciled against, primary first. Remembered so a
    /// display missing at the next start still has a position to find the
    /// nearest remaining one from.
    #[serde(default)]
    pub displays: Vec<Display>,
    /// The display the person is working on. Follows the pointer and
    /// keyboard focus; not persisted.
    #[serde(skip)]
    pub focused: Option<u32>,
    /// The group last focused on each display, per strip; not persisted.
    #[serde(skip)]
    pub last_active: BTreeMap<(u32, StripId), GroupId>,
}

impl OutputPartition {
    pub fn is_empty(&self) -> bool {
        self.home.is_empty() && self.origin.is_empty() && self.displays.is_empty()
    }

    /// The primary display: the first one reconciled.
    pub fn primary(&self) -> Option<u32> {
        self.displays.first().map(|d| d.id)
    }

    fn is_live(&self, id: u32) -> bool {
        self.displays.iter().any(|d| d.id == id)
    }

    /// The display `group` is shown on: its home while that is connected,
    /// else the primary.
    pub fn display_of(&self, group: GroupId) -> Option<u32> {
        self.home
            .get(&group)
            .copied()
            .filter(|id| self.is_live(*id))
            .or_else(|| self.primary())
    }

    /// The display the person is working on, else the primary.
    pub fn focused_display(&self) -> Option<u32> {
        self.focused
            .filter(|id| self.is_live(*id))
            .or_else(|| self.primary())
    }

    /// Displays in reading order: left to right, top to bottom by centre.
    /// This is the order sub-rows are laid out in and the order
    /// left/right movement between displays follows.
    pub fn reading_order(&self) -> Vec<u32> {
        let mut displays = self.displays.clone();
        displays.sort_by(|a, b| {
            let (ax, ay) = a.center();
            let (bx, by) = b.center();
            ax.partial_cmp(&bx)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal))
        });
        displays.into_iter().map(|d| d.id).collect()
    }

    /// The display `step` places after `from` in reading order (negative
    /// for before), or `None` past either end.
    pub fn neighbour(&self, from: u32, step: i32) -> Option<u32> {
        let order = self.reading_order();
        let at = order.iter().position(|id| *id == from)?;
        let next = if step < 0 {
            at.checked_sub(1)?
        } else {
            at.checked_add(1)?
        };
        order.get(next).copied()
    }

    /// The groups of `strip` shown on `display`, in strip order.
    pub fn groups_on(&self, strip: &Strip, display: u32) -> Vec<GroupId> {
        strip
            .window_groups
            .iter()
            .map(|g| g.id)
            .filter(|g| self.display_of(*g) == Some(display))
            .collect()
    }

    /// Record that `group` on `strip` has keyboard focus: it becomes its
    /// display's last focused group and that display the focused one.
    pub fn note_focus(&mut self, strip: StripId, group: GroupId) {
        if let Some(display) = self.display_of(group) {
            self.last_active.insert((display, strip), group);
            self.focused = Some(display);
        }
    }

    /// The group of `strip` to focus when focus arrives on `display`: the
    /// one last focused there, else its first.
    pub fn focus_target(&self, strip: &Strip, display: u32) -> Option<GroupId> {
        let on = self.groups_on(strip, display);
        self.last_active
            .get(&(display, strip.id))
            .copied()
            .filter(|g| on.contains(g))
            .or_else(|| on.first().copied())
    }

    /// Move `ids` (already homed on `display`) to just after `display`'s
    /// last focused group in `strip`, keeping their order. With nothing to
    /// follow on that display their position is left alone: order only
    /// matters among one display's groups. Returns whether the strip's
    /// order changed.
    fn place_after_focus(&self, strip: &mut Strip, display: u32, ids: &[GroupId]) -> bool {
        let others: Vec<GroupId> = self
            .groups_on(strip, display)
            .into_iter()
            .filter(|g| !ids.contains(g))
            .collect();
        let anchor = self
            .last_active
            .get(&(display, strip.id))
            .copied()
            .filter(|g| others.contains(g))
            .or_else(|| others.last().copied());
        let Some(anchor) = anchor else {
            return false;
        };
        let before: Vec<GroupId> = strip.window_groups.iter().map(|g| g.id).collect();
        let (moving, mut rest): (Vec<_>, Vec<_>) = std::mem::take(&mut strip.window_groups)
            .into_iter()
            .partition(|g| ids.contains(&g.id));
        let at = rest
            .iter()
            .position(|g| g.id == anchor)
            .map_or(rest.len(), |i| i.saturating_add(1));
        let tail = rest.split_off(at);
        rest.extend(moving);
        rest.extend(tail);
        strip.window_groups = rest;
        strip.window_groups.iter().map(|g| g.id).ne(before)
    }

    /// Give every group that has no connected home one: the focused
    /// display. Returns whether anything changed.
    pub fn place_unhomed(&mut self, workspaces: &[Workspace]) -> bool {
        let Some(target) = self.focused_display() else {
            return false;
        };
        let mut changed = false;
        for group in workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| &s.window_groups)
        {
            if let std::collections::btree_map::Entry::Vacant(slot) = self.home.entry(group.id) {
                slot.insert(target);
                changed = true;
            }
        }
        changed
    }

    /// Show `group` on `display` from now on. It forgets where it came from,
    /// so a display coming back no longer reclaims it, and lands after the
    /// group last focused on `display`. Returns the strip whose order
    /// changed, if one did.
    pub fn move_group(
        &mut self,
        workspaces: &mut [Workspace],
        group: GroupId,
        display: u32,
    ) -> Option<StripId> {
        if !self.is_live(display) {
            return None;
        }
        self.home.insert(group, display);
        self.origin.remove(&group);
        let strip = workspaces
            .iter_mut()
            .flat_map(|w| w.strips.iter_mut())
            .find(|s| s.window_groups.iter().any(|g| g.id == group))?;
        let reordered = self.place_after_focus(strip, display, &[group]);
        let strip_id = strip.id;
        self.last_active.insert((display, strip_id), group);
        self.focused = Some(display);
        reordered.then_some(strip_id)
    }

    /// Bring the partition in line with the displays now connected,
    /// primary first. Groups whose display went away move to the nearest
    /// remaining one, after its focused group, remembering where they came
    /// from; groups whose display came back return to it. Returns the
    /// strips whose order changed.
    pub fn reconcile(&mut self, workspaces: &mut [Workspace], now: &[Display]) -> Vec<StripId> {
        let previous = std::mem::replace(&mut self.displays, now.to_vec());
        let live: BTreeSet<u32> = now.iter().map(|d| d.id).collect();
        if live.is_empty() {
            return Vec::new();
        }
        // Back home: a display that returned reclaims what left it.
        let returning: Vec<(GroupId, u32)> = self
            .origin
            .iter()
            .filter(|(_, display)| live.contains(display))
            .map(|(g, d)| (*g, *d))
            .collect();
        for (group, display) in returning {
            self.home.insert(group, display);
            self.origin.remove(&group);
        }
        // Stranded: a display that went away hands its groups on.
        let nearest = |gone: u32| -> Option<u32> {
            let was = previous.iter().find(|d| d.id == gone)?;
            now.iter()
                .min_by(|a, b| {
                    a.gap_sq(was)
                        .partial_cmp(&b.gap_sq(was))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|d| d.id)
        };
        let primary = now.first().map(|d| d.id);
        let mut reordered = Vec::new();
        for strip in workspaces.iter_mut().flat_map(|w| w.strips.iter_mut()) {
            let mut by_target: BTreeMap<u32, Vec<GroupId>> = BTreeMap::new();
            for group in &strip.window_groups {
                let Some(&home) = self.home.get(&group.id) else {
                    continue;
                };
                if live.contains(&home) {
                    continue;
                }
                let Some(target) = nearest(home).or(primary) else {
                    continue;
                };
                self.origin.entry(group.id).or_insert(home);
                by_target.entry(target).or_default().push(group.id);
            }
            let mut changed = false;
            for (target, ids) in by_target {
                for id in &ids {
                    self.home.insert(*id, target);
                }
                changed |= self.place_after_focus(strip, target, &ids);
            }
            if changed {
                reordered.push(strip.id);
            }
        }
        // Forget groups that no longer exist.
        let present: BTreeSet<GroupId> = workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| &s.window_groups)
            .map(|g| g.id)
            .collect();
        self.home.retain(|g, _| present.contains(g));
        self.origin.retain(|g, _| present.contains(g));
        self.last_active
            .retain(|(display, _), g| live.contains(display) && present.contains(g));
        if self.focused.is_some_and(|f| !live.contains(&f)) {
            self.focused = None;
        }
        reordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use crate::layout::{Tab, TileContent, WindowGroup};
    use crate::width::WindowWidth;

    fn group(id: GroupId) -> WindowGroup {
        WindowGroup {
            id,
            tabs: vec![Tab {
                id: id.saturating_mul(10),
                title: format!("T{id}"),
                tile: TileContent::Stub {
                    kind: crate::app::AppKind::Settings,
                    label: String::new(),
                },
            }],
            active_tab: id.saturating_mul(10),
            width: WindowWidth::Normal,
            maximized: false,
        }
    }

    fn workspace(ids: &[GroupId]) -> Vec<Workspace> {
        let mut ws = Workspace::new(WorkspaceId(1), "Desk");
        let mut strip = Strip::new(StripId(1), "S");
        strip.window_groups = ids.iter().map(|id| group(*id)).collect();
        ws.strips = vec![strip];
        vec![ws]
    }

    fn order(workspaces: &[Workspace]) -> Vec<GroupId> {
        workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|g| g.id)
            .collect()
    }

    /// A laptop panel with a monitor to its right.
    fn docked() -> Vec<Display> {
        vec![
            Display {
                id: 0,
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
            },
            Display {
                id: 1,
                x: 1280.0,
                y: 0.0,
                width: 2560.0,
                height: 1440.0,
            },
        ]
    }

    #[test]
    fn unplugging_moves_a_displays_groups_after_the_focused_one_and_replugging_returns_them() {
        let mut ws = workspace(&[1, 2, 3, 4, 5]);
        let mut p = OutputPartition::default();
        assert!(p.reconcile(&mut ws, &docked()).is_empty());
        for (g, d) in [(1, 0), (2, 1), (3, 0), (4, 1), (5, 0)] {
            p.home.insert(g, d);
        }
        p.note_focus(StripId(1), 3);

        let changed = p.reconcile(&mut ws, &docked()[..1]);
        assert_eq!(changed, vec![StripId(1)]);
        assert_eq!(
            order(&ws),
            vec![1, 3, 2, 4, 5],
            "after the focused group, in order"
        );
        assert_eq!(p.display_of(2), Some(0));
        assert_eq!(p.origin.get(&4), Some(&1));

        p.reconcile(&mut ws, &docked());
        assert_eq!(p.display_of(2), Some(1));
        assert_eq!(p.display_of(4), Some(1));
        assert!(p.origin.is_empty());
        let on_monitor = p.groups_on(&ws[0].strips[0], 1);
        assert_eq!(on_monitor, vec![2, 4], "back in their own order");
    }

    #[test]
    fn a_group_moved_while_its_display_was_away_stays_where_it_was_moved() {
        let mut ws = workspace(&[1, 2]);
        let mut p = OutputPartition::default();
        p.reconcile(&mut ws, &docked());
        p.home.insert(1, 0);
        p.home.insert(2, 1);
        p.reconcile(&mut ws, &docked()[..1]);
        // The person moves it again on the one remaining display.
        p.move_group(&mut ws, 2, 0);
        p.reconcile(&mut ws, &docked());
        assert_eq!(p.display_of(2), Some(0));
    }

    #[test]
    fn new_groups_go_to_the_focused_display_and_moves_follow_its_focus() {
        let mut ws = workspace(&[1, 2, 3]);
        let mut p = OutputPartition::default();
        p.reconcile(&mut ws, &docked());
        p.focused = Some(1);
        assert!(p.place_unhomed(&ws));
        assert_eq!(p.groups_on(&ws[0].strips[0], 1), vec![1, 2, 3]);
        p.note_focus(StripId(1), 1);
        assert_eq!(p.move_group(&mut ws, 3, 0), None);
        assert_eq!(p.display_of(3), Some(0));
        assert_eq!(p.focused, Some(0));
        assert_eq!(p.neighbour(0, 1), Some(1));
        assert_eq!(p.neighbour(0, -1), None);
    }
}
