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
//! Groups belong to a physical monitor, not to the port or output number it
//! happened to get: everything the partition remembers is keyed by
//! [`Display::monitor`], so a restart or a port swap that renumbers the
//! outputs puts every group back on the monitor it was on. Its API speaks
//! the backend's current output ids, which it maps through the displays
//! last reconciled.
//!
//! Group order stays the strip's, which is shared with every collaborator:
//! a display's sub-row is the strip's groups that belong to it, in strip
//! order. Nothing here ever reorders a strip. Unplugging a monitor or moving
//! a group to another display changes only where groups are shown, so a
//! collaborator's strip never reorders because of this device's monitors.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ids::{GroupId, StripId};
use crate::layout::Strip;
use crate::serde_ids::deserialize_id_btree_map;
use crate::workspace::Workspace;

/// A display as the partition knows it: which monitor it is, the backend's
/// output id for it right now, and where it sits in the logical desktop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Display {
    /// The backend's output id. Only meaningful until the outputs are next
    /// enumerated, so nothing remembered is keyed by it.
    pub id: u32,
    /// The physical monitor: its EDID identity, told apart from an
    /// identical monitor by its connector, or the connector alone for a
    /// sink without a readable EDID. Stable across restarts and port swaps.
    pub monitor: String,
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

/// The per-device assignment of window groups to monitors.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputPartition {
    /// The monitor each group is shown on.
    #[serde(default, deserialize_with = "deserialize_id_btree_map")]
    pub home: BTreeMap<GroupId, String>,
    /// Groups moved off a monitor that went away, and that monitor. They
    /// return when it comes back, unless they were moved since.
    #[serde(default, deserialize_with = "deserialize_id_btree_map")]
    pub origin: BTreeMap<GroupId, String>,
    /// The displays last reconciled against, primary first. Remembered so a
    /// monitor missing at the next start still has a position to find the
    /// nearest remaining one from.
    #[serde(default)]
    pub displays: Vec<Display>,
    /// The monitor the person is working on. Follows the pointer and
    /// keyboard focus; not persisted.
    #[serde(skip)]
    pub focused: Option<String>,
    /// The group last focused on each monitor, per strip; not persisted.
    #[serde(skip)]
    pub last_active: BTreeMap<(String, StripId), GroupId>,
}

impl OutputPartition {
    pub fn is_empty(&self) -> bool {
        self.home.is_empty() && self.origin.is_empty() && self.displays.is_empty()
    }

    /// The primary display: the first one reconciled.
    pub fn primary(&self) -> Option<u32> {
        self.displays.first().map(|d| d.id)
    }

    /// The monitor output `id` shows, while it is connected.
    pub fn monitor_of(&self, id: u32) -> Option<&str> {
        self.displays
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.monitor.as_str())
    }

    /// The output `monitor` is connected as, if it is.
    fn output_of(&self, monitor: &str) -> Option<u32> {
        self.displays
            .iter()
            .find(|d| d.monitor == monitor)
            .map(|d| d.id)
    }

    /// The display `group` is shown on: its home while that monitor is
    /// connected, else the primary.
    pub fn display_of(&self, group: GroupId) -> Option<u32> {
        self.home
            .get(&group)
            .and_then(|monitor| self.output_of(monitor))
            .or_else(|| self.primary())
    }

    /// The display the person is working on, else the primary.
    pub fn focused_display(&self) -> Option<u32> {
        self.focused
            .as_deref()
            .and_then(|monitor| self.output_of(monitor))
            .or_else(|| self.primary())
    }

    /// Make display `id` the one the person is working on.
    pub fn set_focused(&mut self, id: u32) {
        if let Some(monitor) = self.monitor_of(id) {
            self.focused = Some(monitor.to_owned());
        }
    }

    /// Displays in reading order: left to right, top to bottom by centre.
    /// This is the order sub-rows are laid out in and the order
    /// left/right movement between displays follows.
    pub fn reading_order(&self) -> Vec<u32> {
        let mut displays: Vec<&Display> = self.displays.iter().collect();
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
        let Some(monitor) = self
            .display_of(group)
            .and_then(|d| self.monitor_of(d))
            .map(str::to_owned)
        else {
            return;
        };
        self.last_active.insert((monitor.clone(), strip), group);
        self.focused = Some(monitor);
    }

    /// The group last focused on `display` in `strip`.
    fn last_active_on(&self, display: u32, strip: StripId) -> Option<GroupId> {
        let monitor = self.monitor_of(display)?;
        self.last_active.get(&(monitor.to_owned(), strip)).copied()
    }

    /// The group of `strip` to focus when focus arrives on `display`: the
    /// one last focused there, else its first.
    pub fn focus_target(&self, strip: &Strip, display: u32) -> Option<GroupId> {
        let on = self.groups_on(strip, display);
        self.last_active_on(display, strip.id)
            .filter(|g| on.contains(g))
            .or_else(|| on.first().copied())
    }

    /// Give every group that has no home one: the focused display. Returns
    /// whether anything changed.
    pub fn place_unhomed(&mut self, workspaces: &[Workspace]) -> bool {
        let Some(target) = self
            .focused_display()
            .and_then(|d| self.monitor_of(d))
            .map(str::to_owned)
        else {
            return false;
        };
        let mut changed = false;
        for group in workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| &s.window_groups)
        {
            if let std::collections::btree_map::Entry::Vacant(slot) = self.home.entry(group.id) {
                slot.insert(target.clone());
                changed = true;
            }
        }
        changed
    }

    /// Home a group that has none yet on `display`, where it was created.
    /// Returns whether it was placed.
    pub fn place_new(&mut self, group: GroupId, display: u32) -> bool {
        let Some(monitor) = self.monitor_of(display).map(str::to_owned) else {
            return false;
        };
        match self.home.entry(group) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(monitor);
                true
            }
            std::collections::btree_map::Entry::Occupied(_) => false,
        }
    }

    /// Show `group` on `display` from now on, leaving the strip's order to
    /// the caller. It forgets where it came from, so a monitor coming back
    /// no longer reclaims it, and becomes `display`'s last focused group in
    /// `strip`, with `display` the focused one. Returns whether `display` is
    /// connected.
    pub fn rehome(&mut self, strip: StripId, group: GroupId, display: u32) -> bool {
        let Some(monitor) = self.monitor_of(display).map(str::to_owned) else {
            return false;
        };
        self.home.insert(group, monitor.clone());
        self.origin.remove(&group);
        self.last_active.insert((monitor.clone(), strip), group);
        self.focused = Some(monitor);
        true
    }

    /// Show `group` on `display` from now on (see [`Self::rehome`]), at
    /// its place in the strip's order among `display`'s groups. Returns
    /// whether it was moved: `display` is connected and `group` is in a
    /// strip.
    pub fn move_group(&mut self, workspaces: &[Workspace], group: GroupId, display: u32) -> bool {
        let Some(strip) = workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .find(|s| s.window_groups.iter().any(|g| g.id == group))
        else {
            return false;
        };
        self.rehome(strip.id, group, display)
    }

    /// Bring the partition in line with the displays now connected,
    /// primary first. Groups whose monitor went away move to the nearest
    /// remaining one, remembering where they came from; groups whose
    /// monitor came back return to it, whatever output it is connected as
    /// now. Either way each keeps its place in the strip's order.
    pub fn reconcile(&mut self, workspaces: &[Workspace], now: &[Display]) {
        let previous = std::mem::replace(&mut self.displays, now.to_vec());
        let live: BTreeSet<&str> = now.iter().map(|d| d.monitor.as_str()).collect();
        if live.is_empty() {
            return;
        }
        // Back home: a monitor that returned reclaims what left it.
        let returning: Vec<(GroupId, String)> = self
            .origin
            .iter()
            .filter(|(_, monitor)| live.contains(monitor.as_str()))
            .map(|(g, m)| (*g, m.clone()))
            .collect();
        for (group, monitor) in returning {
            self.home.insert(group, monitor);
            self.origin.remove(&group);
        }
        // Stranded: a monitor that went away hands its groups on.
        let nearest = |gone: &str| -> Option<u32> {
            let was = previous.iter().find(|d| d.monitor == gone)?;
            now.iter()
                .min_by(|a, b| {
                    a.gap_sq(was)
                        .partial_cmp(&b.gap_sq(was))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|d| d.id)
        };
        let primary = now.first().map(|d| d.id);
        for group in workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| &s.window_groups)
        {
            let Some(home) = self.home.get(&group.id).cloned() else {
                continue;
            };
            if live.contains(home.as_str()) {
                continue;
            }
            let Some(monitor) = nearest(&home)
                .or(primary)
                .and_then(|target| self.monitor_of(target))
                .map(str::to_owned)
            else {
                continue;
            };
            self.origin.entry(group.id).or_insert(home);
            self.home.insert(group.id, monitor);
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
            .retain(|(monitor, _), g| live.contains(monitor.as_str()) && present.contains(g));
        if self.focused.as_deref().is_some_and(|f| !live.contains(f)) {
            self.focused = None;
        }
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

    const PANEL: &str = "edid:Acme|Panel|1";
    const MONITOR: &str = "edid:Dell|U2720Q|CN0";

    /// A laptop panel (output 0) with a monitor (output 1) to its right.
    fn docked() -> Vec<Display> {
        vec![
            Display {
                id: 0,
                monitor: PANEL.into(),
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
            },
            Display {
                id: 1,
                monitor: MONITOR.into(),
                x: 1280.0,
                y: 0.0,
                width: 2560.0,
                height: 1440.0,
            },
        ]
    }

    fn home(p: &mut OutputPartition, pairs: &[(GroupId, u32)]) {
        for &(g, d) in pairs {
            let monitor = p.monitor_of(d).expect("connected").to_owned();
            p.home.insert(g, monitor);
        }
    }

    #[test]
    fn unplugging_moves_a_displays_groups_in_strip_order_and_replugging_returns_them() {
        let ws = workspace(&[1, 2, 3, 4, 5]);
        let mut p = OutputPartition::default();
        p.reconcile(&ws, &docked());
        home(&mut p, &[(1, 0), (2, 1), (3, 0), (4, 1), (5, 0)]);
        p.note_focus(StripId(1), 3);

        p.reconcile(&ws, &docked()[..1]);
        assert_eq!(p.groups_on(&ws[0].strips[0], 0), vec![1, 2, 3, 4, 5]);
        assert_eq!(p.origin.get(&4).map(String::as_str), Some(MONITOR));

        p.reconcile(&ws, &docked());
        assert_eq!(p.display_of(2), Some(1));
        assert_eq!(p.display_of(4), Some(1));
        assert!(p.origin.is_empty());
        assert_eq!(p.groups_on(&ws[0].strips[0], 1), vec![2, 4]);
    }

    #[test]
    fn a_group_moved_while_its_display_was_away_stays_where_it_was_moved() {
        let ws = workspace(&[1, 2]);
        let mut p = OutputPartition::default();
        p.reconcile(&ws, &docked());
        home(&mut p, &[(1, 0), (2, 1)]);
        p.reconcile(&ws, &docked()[..1]);
        // The person moves it again on the one remaining display.
        assert!(p.move_group(&ws, 2, 0));
        p.reconcile(&ws, &docked());
        assert_eq!(p.display_of(2), Some(0));
    }

    #[test]
    fn new_groups_go_to_the_focused_display_and_moves_follow_its_focus() {
        let ws = workspace(&[1, 2, 3]);
        let mut p = OutputPartition::default();
        p.reconcile(&ws, &docked());
        p.set_focused(1);
        assert!(p.place_unhomed(&ws));
        assert_eq!(p.groups_on(&ws[0].strips[0], 1), vec![1, 2, 3]);
        p.note_focus(StripId(1), 1);
        assert!(p.move_group(&ws, 3, 0));
        assert_eq!(p.display_of(3), Some(0));
        assert_eq!(p.focused_display(), Some(0));
        assert_eq!(p.neighbour(0, 1), Some(1));
        assert_eq!(p.neighbour(0, -1), None);
    }

    #[test]
    fn renumbered_outputs_keep_every_group_on_its_monitor() {
        let ws = workspace(&[1, 2, 3]);
        let mut p = OutputPartition::default();
        p.reconcile(&ws, &docked());
        home(&mut p, &[(1, 0), (2, 1), (3, 1)]);
        // Unplugged, then back on other ports: the monitor is output 0 now
        // and the panel output 1.
        p.reconcile(&ws, &docked()[..1]);
        let mut swapped = docked();
        swapped[0].id = 1;
        swapped[1].id = 0;
        swapped.reverse();
        p.reconcile(&ws, &swapped);
        assert_eq!(p.display_of(1), Some(1), "the panel's group");
        assert_eq!(p.display_of(2), Some(0), "the monitor's groups");
        assert_eq!(p.display_of(3), Some(0));
        assert!(p.origin.is_empty());
    }

    #[test]
    fn a_saved_partition_loads_inside_a_flattened_tree() {
        // The shell's persisted tree flattens the desktop into itself, which
        // hands the id-keyed maps their keys as strings.
        #[derive(Serialize, Deserialize)]
        struct Saved {
            #[serde(flatten)]
            partition: OutputPartition,
        }
        let ws = workspace(&[1, 2]);
        let mut p = OutputPartition::default();
        p.reconcile(&ws, &docked());
        home(&mut p, &[(1, 0), (2, 1)]);
        p.origin.insert(2, PANEL.into());
        let json = serde_json::to_string(&Saved {
            partition: p.clone(),
        })
        .expect("saves");
        let loaded: Saved = serde_json::from_str(&json).expect("loads");
        assert_eq!(loaded.partition, p);
    }
}
