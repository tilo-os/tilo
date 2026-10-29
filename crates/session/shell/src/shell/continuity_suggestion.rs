//! "Continue what you were doing on another device".
//!
//! Each of a person's devices notes, in its own part of each workspace's
//! layout record, which window it is on
//! ([`tilo_desktop_model::shared_record::continuation`]), and every other
//! device of the person holds that record. This module turns those notes into
//! one quiet offer: the window the person most recently moved to on another
//! of their certified devices, if this desktop has it and is not already
//! showing it.
//!
//! Nothing here moves focus. The offer is a value the top bar and the palette
//! present; only an explicit accept focuses the tab.
//!
//! Recency needs wall time, which the record does not carry, so
//! [`FocusRecency`] records the local monotonic instant at which each
//! device's focus was first seen to change. Focus already held when this
//! device first looks has no such instant and is never offered: it describes
//! an earlier session, not what the person is doing now.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::shell::continuation::AppInstanceId;
use crate::shell::state::{GroupId, RoomId, StripId, TabId, WorkspaceId, WorkspaceTree};
use tilo_network_actor::fabric::DeviceId;

/// How long after another device moved to a tab this desktop offers it.
pub const SUGGESTION_WINDOW: Duration = Duration::from_secs(5 * 60);

/// One device's focus as a workspace's layout record holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceFocus {
    pub device: DeviceId,
    pub room: RoomId,
    pub app: AppInstanceId,
    /// The window has since been closed: the record no longer holds it.
    pub withdrawn: bool,
}

/// One device's current focus, as last observed here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusObservation {
    pub device: DeviceId,
    pub room: RoomId,
    pub app: AppInstanceId,
    /// When this focus was first seen here. `None` for focus already held
    /// when this device first looked, which is never fresh enough to offer.
    pub observed_at: Option<Instant>,
    /// Changes exactly when this device's focus moves to a different tab, so
    /// a dismissal can last until the next move.
    pub revision: u64,
    /// The focused app has since been closed.
    pub withdrawn: bool,
}

/// Tracks when each device's focus last changed, fed from the layout
/// records' focus notes.
#[derive(Debug, Default)]
pub struct FocusRecency {
    entries: BTreeMap<DeviceId, FocusObservation>,
    next_revision: u64,
    /// Whether the focus already held at start was recorded (as stale).
    seeded: bool,
}

impl FocusRecency {
    /// Fold in the focus the records hold at `now`. The first look records
    /// every focus as stale: it was already there.
    pub fn observe(&mut self, focus: &[DeviceFocus], now: Instant) {
        let at = self.seeded.then_some(now);
        self.seeded = true;
        self.record(focus, at);
    }

    /// Forget everything, e.g. when app continuation is switched off.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn observations(&self) -> Vec<FocusObservation> {
        self.entries.values().cloned().collect()
    }

    fn record(&mut self, focus: &[DeviceFocus], at: Option<Instant>) {
        let mut focused = BTreeSet::new();
        for note in focus {
            // A device's focus is in one workspace at a time; a stale note in
            // another record does not replace the one it moved to.
            if !focused.insert(note.device.clone()) {
                continue;
            }
            match self.entries.get_mut(&note.device) {
                Some(entry) if entry.room == note.room && entry.app == note.app => {
                    entry.withdrawn = note.withdrawn;
                }
                _ => {
                    self.next_revision = self.next_revision.saturating_add(1);
                    self.entries.insert(
                        note.device.clone(),
                        FocusObservation {
                            device: note.device.clone(),
                            room: note.room,
                            app: note.app,
                            observed_at: at,
                            revision: self.next_revision,
                            withdrawn: note.withdrawn,
                        },
                    );
                }
            }
        }
        self.entries.retain(|device, _| focused.contains(device));
    }
}

/// Everything the network knows that the offer depends on, copied into
/// chrome so the selector stays a pure function of chrome state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContinuityFeed {
    /// This device; its own focus is never offered back to it.
    pub local_device: Option<DeviceId>,
    pub observations: Vec<FocusObservation>,
    /// Certified devices of the same identity, with their display label.
    /// An observation from a device missing here is never offered.
    pub devices: BTreeMap<DeviceId, String>,
}

/// The single offer the desktop shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuitySuggestion {
    pub device: DeviceId,
    pub device_label: String,
    pub app_label: String,
    pub workspace: WorkspaceId,
    pub strip: StripId,
    pub group: GroupId,
    pub tab: TabId,
    pub revision: u64,
}

/// Where a tab lives locally, and its title.
struct LocalTab {
    workspace: WorkspaceId,
    strip: StripId,
    group: GroupId,
    title: String,
    focused: bool,
}

fn local_tab(tree: &WorkspaceTree, room: RoomId, tab: TabId) -> Option<LocalTab> {
    let desktop = &tree.desktop;
    let workspace = desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)?;
    for strip in &workspace.strips {
        for group in &strip.window_groups {
            let Some(found) = group.tabs.iter().find(|candidate| candidate.id == tab) else {
                continue;
            };
            let focused = desktop.active == workspace.id
                && desktop.active_strip.get(&workspace.id) == Some(&strip.id)
                && desktop.active_group.get(&strip.id) == Some(&group.id)
                && group.active_tab == tab;
            return Some(LocalTab {
                workspace: workspace.id,
                strip: strip.id,
                group: group.id,
                title: found.title.clone(),
                focused,
            });
        }
    }
    None
}

/// Choose the offer, if any.
///
/// The newest focus (first seen last, then by device) wins among
/// observations that are
/// from another certified device of this identity, changed within
/// [`SUGGESTION_WINDOW`] of `now`, were not closed since, were not
/// dismissed at this revision, and name a tab this desktop has but is not
/// already focused on.
pub fn select(
    tree: &WorkspaceTree,
    feed: &ContinuityFeed,
    dismissed: &BTreeMap<DeviceId, u64>,
    now: Instant,
) -> Option<ContinuitySuggestion> {
    let mut best: Option<(&FocusObservation, ContinuitySuggestion)> = None;
    for observation in &feed.observations {
        if feed.local_device.as_ref() == Some(&observation.device)
            || observation.withdrawn
            || dismissed.get(&observation.device) == Some(&observation.revision)
        {
            continue;
        }
        let Some(device_label) = feed.devices.get(&observation.device) else {
            continue;
        };
        let fresh = observation
            .observed_at
            .is_some_and(|at| now.saturating_duration_since(at) <= SUGGESTION_WINDOW);
        if !fresh {
            continue;
        }
        let Some(tab) = local_tab(tree, observation.room, observation.app.0) else {
            continue;
        };
        if tab.focused {
            continue;
        }
        if best.as_ref().is_some_and(|(current, _)| {
            (current.observed_at, &current.device) >= (observation.observed_at, &observation.device)
        }) {
            continue;
        }
        let suggestion = ContinuitySuggestion {
            device: observation.device.clone(),
            device_label: device_label.clone(),
            app_label: tab.title,
            workspace: tab.workspace,
            strip: tab.strip,
            group: tab.group,
            tab: observation.app.0,
            revision: observation.revision,
        };
        best = Some((observation, suggestion));
    }
    best.map(|(_, suggestion)| suggestion)
}

/// Chrome-held offer state: the latest feed plus what the person dismissed.
#[derive(Debug, Default)]
pub struct ContinuitySuggestions {
    feed: ContinuityFeed,
    /// Device -> the focus revision that was dismissed or accepted there.
    dismissed: BTreeMap<DeviceId, u64>,
}

impl ContinuitySuggestions {
    /// Replace the feed. Returns whether anything changed. A dismissal is
    /// dropped once its device's focus has moved on.
    pub fn set_feed(&mut self, feed: ContinuityFeed) -> bool {
        if feed == self.feed {
            return false;
        }
        self.dismissed.retain(|device, revision| {
            feed.observations.iter().any(|observation| {
                &observation.device == device && observation.revision == *revision
            })
        });
        self.feed = feed;
        true
    }

    pub fn current(&self, tree: &WorkspaceTree, now: Instant) -> Option<ContinuitySuggestion> {
        select(tree, &self.feed, &self.dismissed, now)
    }

    /// Hide `suggestion` until its device's focus changes again.
    pub fn dismiss(&mut self, suggestion: &ContinuitySuggestion) {
        self.dismissed
            .insert(suggestion.device.clone(), suggestion.revision);
    }
}

/// The chip and palette label, e.g. "Continue Notes from Laptop".
pub fn continue_label(suggestion: &ContinuitySuggestion) -> String {
    crate::shell::i18n::t_args(
        "continuity-continue",
        &[
            ("app", suggestion.app_label.as_str().into()),
            ("device", suggestion.device_label.as_str().into()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

    const NOTES: TabId = 7_001;
    const TERMINAL: TabId = 7_002;

    fn device(name: &str) -> DeviceId {
        DeviceId::new(name).unwrap()
    }

    fn tab(id: TabId, title: &str) -> Tab {
        Tab {
            id,
            title: title.into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: title.into(),
                note: String::new(),
            },
            app_title: None,
        }
    }

    /// One workspace whose second group holds Notes; Terminal is focused.
    fn tree() -> WorkspaceTree {
        let mut tree = WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        let strip = workspace.strips[0].id;
        workspace.strips[0].window_groups = vec![
            WindowGroup {
                id: 10,
                tabs: vec![tab(TERMINAL, "Terminal")],
                active_tab: TERMINAL,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            },
            WindowGroup {
                id: 20,
                tabs: vec![tab(NOTES, "Notes")],
                active_tab: NOTES,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            },
        ];
        let workspace_id = workspace.id;
        tree.desktop.active = workspace_id;
        tree.desktop.active_strip.insert(workspace_id, strip);
        tree.desktop.set_active_group(strip, 10);
        tree
    }

    fn room(tree: &WorkspaceTree) -> RoomId {
        tree.desktop.workspaces[0].room()
    }

    fn on(owner: &DeviceId, room: RoomId, focus: TabId) -> DeviceFocus {
        DeviceFocus {
            device: owner.clone(),
            room,
            app: AppInstanceId(focus),
            withdrawn: false,
        }
    }

    /// A recency that already took its first, stale look.
    fn looking() -> FocusRecency {
        let mut recency = FocusRecency::default();
        recency.observe(&[], Instant::now());
        recency
    }

    fn feed(recency: &FocusRecency, devices: &[(&DeviceId, &str)]) -> ContinuityFeed {
        ContinuityFeed {
            local_device: Some(device("desk")),
            observations: recency.observations(),
            devices: devices
                .iter()
                .map(|(id, label)| ((*id).clone(), (*label).to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_fresh_foreign_focus_on_an_unfocused_local_tab_is_offered() {
        let tree = tree();
        let laptop = device("laptop");
        let now = Instant::now();
        let mut recency = looking();
        recency.observe(&[on(&laptop, room(&tree), NOTES)], now);

        let offer = select(
            &tree,
            &feed(&recency, &[(&laptop, "Laptop")]),
            &BTreeMap::new(),
            now,
        )
        .expect("fresh focus from a certified device is offered");
        assert_eq!(offer.tab, NOTES);
        assert_eq!(offer.group, 20);
        assert_eq!(offer.app_label, "Notes");
        assert_eq!(offer.device_label, "Laptop");
    }

    #[test]
    fn own_uncertified_stale_closed_and_focused_tabs_are_never_offered() {
        let mut tree = tree();
        let desk = device("desk");
        let laptop = device("laptop");
        let now = Instant::now();

        let mut recency = looking();
        recency.observe(&[on(&desk, room(&tree), NOTES)], now);
        assert!(
            select(
                &tree,
                &feed(&recency, &[(&desk, "Desk")]),
                &BTreeMap::new(),
                now
            )
            .is_none(),
            "this device's own focus is not offered back to it"
        );

        let mut recency = looking();
        recency.observe(&[on(&laptop, room(&tree), NOTES)], now);
        assert!(
            select(&tree, &feed(&recency, &[]), &BTreeMap::new(), now).is_none(),
            "a device outside the certified set is not offered"
        );
        let certified = feed(&recency, &[(&laptop, "Laptop")]);
        assert!(
            select(
                &tree,
                &certified,
                &BTreeMap::new(),
                now + SUGGESTION_WINDOW + Duration::from_secs(1)
            )
            .is_none(),
            "focus older than the window is stale"
        );
        let mut first_look = FocusRecency::default();
        first_look.observe(&[on(&laptop, room(&tree), NOTES)], now);
        assert!(
            select(
                &tree,
                &feed(&first_look, &[(&laptop, "Laptop")]),
                &BTreeMap::new(),
                now
            )
            .is_none(),
            "focus held before this device looked describes an earlier session"
        );
        let mut closed = looking();
        closed.observe(
            &[DeviceFocus {
                withdrawn: true,
                ..on(&laptop, room(&tree), NOTES)
            }],
            now,
        );
        assert!(
            select(
                &tree,
                &feed(&closed, &[(&laptop, "Laptop")]),
                &BTreeMap::new(),
                now
            )
            .is_none(),
            "a closed window is not offered"
        );
        assert!(
            select(
                &tree,
                &feed(&recency, &[(&laptop, "Laptop")]),
                &BTreeMap::new(),
                now
            )
            .is_some()
        );
        let mut elsewhere = looking();
        elsewhere.observe(&[on(&laptop, RoomId::new(), NOTES)], now);
        assert!(
            select(
                &tree,
                &feed(&elsewhere, &[(&laptop, "Laptop")]),
                &BTreeMap::new(),
                now
            )
            .is_none(),
            "a room this desktop does not have offers nothing"
        );

        let strip = tree.desktop.workspaces[0].strips[0].id;
        tree.desktop.set_active_group(strip, 20);
        assert!(
            select(&tree, &certified, &BTreeMap::new(), now).is_none(),
            "a tab already focused here needs no offer"
        );
    }

    #[test]
    fn the_latest_move_wins_across_devices() {
        let tree = tree();
        let laptop = device("laptop");
        let tablet = device("tablet");
        let now = Instant::now();
        let mut recency = looking();
        recency.observe(&[on(&tablet, room(&tree), NOTES)], now);
        recency.observe(
            &[
                on(&tablet, room(&tree), NOTES),
                on(&laptop, room(&tree), NOTES),
            ],
            now + Duration::from_secs(1),
        );
        let offer = select(
            &tree,
            &feed(&recency, &[(&laptop, "Laptop"), (&tablet, "Tablet")]),
            &BTreeMap::new(),
            now + Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(offer.device, laptop);
    }

    #[test]
    fn dismissal_lasts_until_that_device_focus_changes() {
        let tree = tree();
        let laptop = device("laptop");
        let start = Instant::now();
        let mut recency = looking();
        recency.observe(&[on(&laptop, room(&tree), NOTES)], start);
        let mut offers = ContinuitySuggestions::default();
        assert!(offers.set_feed(feed(&recency, &[(&laptop, "Laptop")])));
        let offer = offers.current(&tree, start).unwrap();
        offers.dismiss(&offer);
        assert!(offers.current(&tree, start).is_none());

        // Looking again at the same focus neither revives the offer nor
        // refreshes its age.
        recency.observe(
            &[on(&laptop, room(&tree), NOTES)],
            start + Duration::from_secs(60),
        );
        offers.set_feed(feed(&recency, &[(&laptop, "Laptop")]));
        assert!(offers.current(&tree, start).is_none());

        // Moving away and back is a new focus: it is offered again.
        recency.observe(
            &[on(&laptop, room(&tree), TERMINAL)],
            start + Duration::from_secs(61),
        );
        offers.set_feed(feed(&recency, &[(&laptop, "Laptop")]));
        let later = start + Duration::from_secs(62);
        recency.observe(&[on(&laptop, room(&tree), NOTES)], later);
        offers.set_feed(feed(&recency, &[(&laptop, "Laptop")]));
        assert_eq!(
            offers.current(&tree, later).map(|offer| offer.tab),
            Some(NOTES)
        );
    }
}
