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
use tilo_sort::CompactSort as _;

use serde::{Deserialize, Serialize};

use crate::ids::{GroupId, StripId, WorkspaceId};
use crate::layout::{Strip, WindowGroup};
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
    /// Whether the person made this display primary. Told again with every
    /// reconciliation, so it is not part of what the partition remembers.
    #[serde(skip)]
    pub primary: bool,
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
    #[serde(default, deserialize_with = "deserialize_id_btree_map")]
    pub home: BTreeMap<GroupId, String>,
    /// Groups moved off a monitor that went away, and that monitor. They
    /// return when it comes back, unless they were moved since.
    #[serde(default, deserialize_with = "deserialize_id_btree_map")]
    pub origin: BTreeMap<GroupId, String>,
    /// The displays last reconciled against. Remembered so a monitor
    /// missing at the next start still has a position to find the nearest
    /// remaining one from.
    #[serde(default)]
    pub displays: Vec<Display>,
    /// The monitor the person is working on. Follows the pointer and
    /// keyboard focus; not persisted.
    #[serde(skip)]
    pub focused: Option<String>,
    /// The group last focused on each monitor, per strip; not persisted.
    #[serde(skip)]
    pub last_active: BTreeMap<(String, StripId), GroupId>,
    /// The strip each monitor shows, per workspace: every display shows a
    /// strip of its own, so moving between strips on one leaves the others
    /// where they are. The focused display shows the workspace's active
    /// strip whatever this says (see `DesktopTree::strip_shown_on`); not
    /// persisted.
    #[serde(skip)]
    pub shown: BTreeMap<(String, WorkspaceId), StripId>,
}

impl OutputPartition {
    pub fn is_empty(&self) -> bool {
        self.home.is_empty() && self.origin.is_empty() && self.displays.is_empty()
    }

    /// The primary display: the one the person made primary in Settings
    /// while it is connected, else the first one reconciled.
    pub fn primary(&self) -> Option<u32> {
        self.displays
            .iter()
            .find(|display| display.primary)
            .or_else(|| self.displays.first())
            .map(|display| display.id)
    }

    /// The monitor output `id` shows, while it is connected.
    pub fn monitor_of(&self, id: u32) -> Option<&str> {
        self.displays
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.monitor.as_str())
    }

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
        displays.compact_sort_by(|a, b| {
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

    /// `strip`'s groups in the order its row lays them out: display by
    /// display in reading order, each display's in strip order. The strip's
    /// own order on one display.
    pub fn lane_order<'a>(&self, strip: &'a Strip) -> Vec<&'a WindowGroup> {
        let mut groups: Vec<&WindowGroup> = strip.window_groups.iter().collect();
        if self.displays.len() < 2 {
            return groups;
        }
        let order = self.reading_order();
        groups.compact_sort_by_key(|g| {
            self.display_of(g.id)
                .and_then(|d| order.iter().position(|id| *id == d))
                .unwrap_or(0)
        });
        groups
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

    /// The strip `display` was last showing in `workspace`, if any.
    pub fn shown_on(&self, display: u32, workspace: WorkspaceId) -> Option<StripId> {
        let monitor = self.monitor_of(display)?;
        self.shown.get(&(monitor.to_owned(), workspace)).copied()
    }

    /// Remember that `display` shows `strip` in `workspace`. Returns
    /// whether that changed anything.
    pub fn set_shown(&mut self, display: u32, workspace: WorkspaceId, strip: StripId) -> bool {
        let Some(monitor) = self.monitor_of(display).map(str::to_owned) else {
            return false;
        };
        self.shown.insert((monitor, workspace), strip) != Some(strip)
    }

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

    /// Bring the partition in line with the displays now connected. Groups
    /// whose monitor went away move to the nearest remaining one, else to
    /// the display that stands in for the primary, remembering where they
    /// came from; groups whose monitor came back return to it, whatever
    /// output it is connected as now. Either way each keeps its place in the
    /// strip's order.
    pub fn reconcile(&mut self, workspaces: &[Workspace], now: &[Display]) {
        let previous = std::mem::replace(&mut self.displays, now.to_vec());
        let live: BTreeSet<&str> = now.iter().map(|d| d.monitor.as_str()).collect();
        if live.is_empty() {
            return;
        }
        // The display the person chose, else the first one there is.
        let primary = now
            .iter()
            .find(|d| d.primary)
            .or_else(|| now.first())
            .map(|d| d.id);
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
        let strips: BTreeSet<StripId> = workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .map(|s| s.id)
            .collect();
        self.shown
            .retain(|(monitor, _), s| live.contains(monitor.as_str()) && strips.contains(s));
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
                app_title: None,
            }],
            active_tab: id.saturating_mul(10),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
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
                primary: false,
            },
            Display {
                id: 1,
                monitor: MONITOR.into(),
                x: 1280.0,
                y: 0.0,
                width: 2560.0,
                height: 1440.0,
                primary: false,
            },
        ]
    }

    fn home(p: &mut OutputPartition, pairs: &[(GroupId, u32)]) {
        for &(g, d) in pairs {
            let monitor = p.monitor_of(d).expect("connected").to_owned();
            p.home.insert(g, monitor);
        }
    }

    /// `displays` with the monitor `primary` made the primary one.
    fn with_primary(mut displays: Vec<Display>, primary: &str) -> Vec<Display> {
        for display in &mut displays {
            display.primary = display.monitor == primary;
        }
        displays
    }

    /// The display the person chose in Settings is the primary one while it
    /// is connected, else the first one there is.
    #[test]
    fn the_chosen_display_is_primary_and_the_first_stands_in_without_one() {
        let ws = workspace(&[1]);
        let mut p = OutputPartition::default();
        p.reconcile(&ws, &docked());
        assert_eq!(p.primary(), Some(0), "no choice: the first one");

        p.reconcile(&ws, &with_primary(docked(), MONITOR));
        assert_eq!(p.primary(), Some(1), "the monitor the person chose");
        assert_eq!(p.focused_display(), Some(1), "focus follows it");
        assert_eq!(p.display_of(1), Some(1), "so does a window without one");

        // The chosen monitor is unplugged: the one left stands in, and the
        // choice returns with the monitor.
        p.reconcile(&ws, &with_primary(docked()[..1].to_vec(), PANEL));
        assert_eq!(p.primary(), Some(0));
        p.reconcile(&ws, &with_primary(docked(), MONITOR));
        assert_eq!(p.primary(), Some(1));
    }

    /// A monitor that goes away and comes back takes the groups it stranded
    /// to it, never to the display standing in for the primary.
    #[test]
    fn groups_a_gone_monitor_stranded_move_to_the_primary_that_is_there() {
        let ws = workspace(&[1, 2]);
        let mut p = OutputPartition::default();
        p.reconcile(&ws, &with_primary(docked(), PANEL));
        home(&mut p, &[(1, 0), (2, 1)]);
        // The monitor holding one of them goes, leaving the panel as primary.
        p.reconcile(&ws, &with_primary(docked()[..1].to_vec(), PANEL));
        assert_eq!(p.display_of(2), Some(0));
        assert_eq!(p.origin.get(&2).map(String::as_str), Some(MONITOR));
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
    fn a_row_lays_its_groups_out_display_by_display_in_strip_order() {
        let ws = workspace(&[1, 2, 3, 4]);
        let strip = &ws[0].strips[0];
        let mut p = OutputPartition::default();
        let ids = |p: &OutputPartition| -> Vec<GroupId> {
            p.lane_order(strip).into_iter().map(|g| g.id).collect()
        };
        assert_eq!(ids(&p), vec![1, 2, 3, 4], "one display: the strip's order");
        p.reconcile(&ws, &docked());
        home(&mut p, &[(1, 1), (2, 0), (3, 1), (4, 0)]);
        assert_eq!(ids(&p), vec![2, 4, 1, 3]);
    }

    #[test]
    fn a_group_moved_while_its_display_was_away_stays_where_it_was_moved() {
        let ws = workspace(&[1, 2]);
        let mut p = OutputPartition::default();
        p.reconcile(&ws, &docked());
        home(&mut p, &[(1, 0), (2, 1)]);
        p.reconcile(&ws, &docked()[..1]);
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

    /// Each display shows a strip of its own: going to another strip on
    /// the focused one leaves the other, and focus moving brings the
    /// arriving display's strip into use.
    #[test]
    fn every_display_keeps_the_strip_it_shows() {
        let mut ws = workspace(&[1, 2]).remove(0);
        let mut below = Strip::new(StripId(2), "Below");
        below.window_groups = vec![group(3), group(4)];
        ws.strips.push(below);
        let mut tree = crate::tree::DesktopTree {
            active: ws.id,
            workspaces: vec![ws.clone()],
            ..Default::default()
        };
        tree.active_strip.insert(ws.id, StripId(1));
        tree.outputs.reconcile(&tree.workspaces, &docked());
        home(&mut tree.outputs, &[(1, 0), (2, 1), (3, 0), (4, 1)]);
        tree.outputs.set_focused(0);
        assert!(tree.shown_strips_behind());
        assert!(tree.note_shown_strip());
        assert!(!tree.shown_strips_behind());
        // The panel goes to the strip below; the monitor stays.
        tree.active_strip.insert(ws.id, StripId(2));
        assert!(tree.note_shown_strip());
        assert_eq!(tree.strip_shown_on(0), Some(StripId(2)));
        assert_eq!(tree.strip_shown_on(1), Some(StripId(1)));
        // Focus crossing to the monitor brings its strip into use.
        assert!(tree.focus_display(1));
        assert_eq!(tree.active_strip_id(), Some(StripId(1)));
        assert_eq!(tree.strip_shown_on(0), Some(StripId(2)));
        // A strip shown on a display that is not focused stays there.
        assert!(tree.show_strip_on(0, StripId(1)));
        assert_eq!(tree.strip_shown_on(0), Some(StripId(1)));
        assert_eq!(tree.active_strip_id(), Some(StripId(1)));
    }

    /// A monitor is unplugged and plugged back in while the desk runs, as a
    /// docked laptop's is: what it held moves to the display left and
    /// returns with it, a group opened on it is homed to it, and the strip
    /// it shows follows the displays it has.
    #[test]
    fn a_monitor_plugged_in_while_the_desk_runs_takes_a_group_and_hands_its_groups_on_when_it_goes()
    {
        let ws = workspace(&[1, 2]).remove(0);
        let mut tree = crate::tree::DesktopTree {
            active: ws.id,
            workspaces: vec![ws],
            ..Default::default()
        };
        tree.active_strip.insert(tree.active, StripId(1));
        assert!(tree.reconcile_displays(&docked()));
        home(&mut tree.outputs, &[(1, 0), (2, 1)]);

        // The cable is pulled: its group moves to the panel, and the desk
        // remembers it came from the monitor.
        assert!(tree.reconcile_displays(&docked()[..1]));
        assert!(!tree.is_partitioned(), "one display shows one strip");
        assert_eq!(tree.outputs.display_of(2), Some(0));
        assert_eq!(
            tree.outputs.origin.get(&2).map(String::as_str),
            Some(MONITOR)
        );

        // Plugged back in mid-run, the monitor is a display again and the
        // group it held is back on it, on the strip the desk shows there.
        assert!(tree.reconcile_displays(&docked()));
        assert!(tree.is_partitioned());
        assert_eq!(tree.strip_shown_on(1), tree.active_strip_id());
        assert_eq!(
            tree.outputs.groups_on(&tree.workspaces[0].strips[0], 1),
            vec![2]
        );

        // A group opened while the monitor has focus is homed to it.
        tree.outputs.set_focused(1);
        tree.workspaces[0].strips[0].window_groups.push(group(3));
        assert!(tree.reconcile_displays(&docked()));
        assert_eq!(tree.outputs.display_of(3), Some(1));

        // And when it goes again everything it showed moves to the panel.
        assert!(tree.reconcile_displays(&docked()[..1]));
        assert_eq!(
            tree.outputs.groups_on(&tree.workspaces[0].strips[0], 0),
            vec![1, 2, 3]
        );
    }
}
