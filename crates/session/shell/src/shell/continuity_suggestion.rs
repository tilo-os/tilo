//! "Continue what you were doing on another device".
//!
//! Personal continuity already mirrors every tab a person's other devices
//! have open into this desktop, and each device publishes which of those tabs
//! it is focused on. This module turns that into one quiet offer: the tab the
//! person most recently moved to on another of their certified devices, if
//! this desktop has it and is not already showing it.
//!
//! Nothing here moves focus. The offer is a value the top bar and the palette
//! present; only an explicit accept focuses the tab.
//!
//! Recency needs wall time, which a Lamport stamp does not carry, so
//! [`FocusRecency`] records the local monotonic instant at which each
//! device's focus was first seen to change. Focus loaded from disk at start-up
//! has no such instant and is never offered: it describes an earlier session,
//! not what the person is doing now.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::shell::personal_continuity::{
    AppInstanceId, ContinuityStamp, DeviceFocus, DeviceId, PersonalContinuity,
};
use crate::shell::state::{GroupId, RoomId, StripId, TabId, WorkspaceId, WorkspaceTree};

/// How long after another device moved to a tab this desktop offers it.
pub const SUGGESTION_WINDOW: Duration = Duration::from_secs(5 * 60);

/// One device's current focus, as last observed here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusObservation {
    pub device: DeviceId,
    pub room: RoomId,
    pub app: AppInstanceId,
    /// The snapshot stamp that carried this focus; orders concurrent offers.
    pub stamp: ContinuityStamp,
    /// When this focus was first seen here. `None` for focus restored from
    /// disk, which is never fresh enough to offer.
    pub observed_at: Option<Instant>,
    /// Changes exactly when this device's focus moves to a different tab, so
    /// a dismissal can last until the next move.
    pub revision: u64,
    /// The focused app has since been closed (tombstoned) by some device.
    pub withdrawn: bool,
}

/// Tracks when each device's focus last changed, fed from the merged
/// personal-continuity checkpoint.
#[derive(Debug, Default)]
pub struct FocusRecency {
    entries: BTreeMap<DeviceId, FocusObservation>,
    next_revision: u64,
}

impl FocusRecency {
    /// Start from state that was already known (restored from disk). Every
    /// focus seeded this way is recorded as stale.
    pub fn seeded(continuity: &PersonalContinuity) -> Self {
        let mut recency = Self::default();
        recency.record(continuity, None);
        recency
    }

    /// Fold in a newly merged checkpoint observed at `now`.
    pub fn observe(&mut self, continuity: &PersonalContinuity, now: Instant) {
        self.record(continuity, Some(now));
    }

    /// Forget everything, e.g. when app continuation is switched off.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn observations(&self) -> Vec<FocusObservation> {
        self.entries.values().cloned().collect()
    }

    fn record(&mut self, continuity: &PersonalContinuity, at: Option<Instant>) {
        let mut focused = BTreeSet::new();
        for (device, snapshot) in continuity.snapshots() {
            let Some(DeviceFocus {
                room,
                app: Some(app),
            }) = snapshot.focus
            else {
                continue;
            };
            focused.insert(device.clone());
            let withdrawn = continuity.newest_app_state_in_room(room, app).is_none();
            match self.entries.get_mut(device) {
                Some(entry) if entry.room == room && entry.app == app => {
                    // Same tab: a later snapshot (other topology changes)
                    // must not make an old focus look fresh again.
                    entry.withdrawn = withdrawn;
                    if snapshot.stamp.is_newer_than(&entry.stamp) {
                        entry.stamp = snapshot.stamp.clone();
                    }
                }
                _ => {
                    self.next_revision = self.next_revision.saturating_add(1);
                    self.entries.insert(
                        device.clone(),
                        FocusObservation {
                            device: device.clone(),
                            room,
                            app,
                            stamp: snapshot.stamp.clone(),
                            observed_at: at,
                            revision: self.next_revision,
                            withdrawn,
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
/// The newest focus (by continuity stamp) wins among observations that are
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
        if best
            .as_ref()
            .is_some_and(|(current, _)| !observation.stamp.is_newer_than(&current.stamp))
        {
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
    use crate::shell::personal_continuity::{
        AppInstanceState, DeviceContinuationSnapshot, LaunchIntent, TabPlacement,
        WorkspaceContinuation,
    };
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

    fn snapshot(
        owner: &DeviceId,
        counter: u64,
        room: RoomId,
        focus: TabId,
    ) -> DeviceContinuationSnapshot {
        let stamp = ContinuityStamp {
            counter,
            device: owner.clone(),
        };
        let apps = [NOTES, TERMINAL]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                AppInstanceState::new(
                    AppInstanceId(id),
                    TabPlacement {
                        strip: 0,
                        group: u32::try_from(index).unwrap(),
                        tab: 0,
                    },
                    LaunchIntent::app("so.tilo.notes").unwrap(),
                    None,
                    stamp.clone(),
                )
            })
            .collect();
        DeviceContinuationSnapshot::new(
            owner.clone(),
            counter,
            Some(DeviceFocus {
                room,
                app: Some(AppInstanceId(focus)),
            }),
            vec![WorkspaceContinuation { room, apps }],
        )
        .unwrap()
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
        let mut continuity = PersonalContinuity::default();
        continuity
            .upsert(snapshot(&laptop, 3, room(&tree), NOTES))
            .unwrap();
        let now = Instant::now();
        let mut recency = FocusRecency::default();
        recency.observe(&continuity, now);

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
    fn own_uncertified_stale_and_focused_tabs_are_never_offered() {
        let mut tree = tree();
        let desk = device("desk");
        let laptop = device("laptop");
        let now = Instant::now();

        let mut own = PersonalContinuity::default();
        own.upsert(snapshot(&desk, 3, room(&tree), NOTES)).unwrap();
        let mut recency = FocusRecency::default();
        recency.observe(&own, now);
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

        let mut continuity = PersonalContinuity::default();
        continuity
            .upsert(snapshot(&laptop, 3, room(&tree), NOTES))
            .unwrap();
        let mut recency = FocusRecency::default();
        recency.observe(&continuity, now);
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
        assert!(
            select(
                &tree,
                &feed(&FocusRecency::seeded(&continuity), &[(&laptop, "Laptop")]),
                &BTreeMap::new(),
                now
            )
            .is_none(),
            "focus restored from disk describes an earlier session"
        );

        let strip = tree.desktop.workspaces[0].strips[0].id;
        tree.desktop.set_active_group(strip, 20);
        assert!(
            select(&tree, &certified, &BTreeMap::new(), now).is_none(),
            "a tab already focused here needs no offer"
        );
    }

    #[test]
    fn a_tab_this_desktop_lacks_or_that_was_closed_is_not_offered() {
        let tree = tree();
        let laptop = device("laptop");
        let phone = device("phone");
        let now = Instant::now();
        let mut continuity = PersonalContinuity::default();
        continuity
            .upsert(snapshot(&laptop, 3, RoomId::new(), NOTES))
            .unwrap();
        let mut recency = FocusRecency::default();
        recency.observe(&continuity, now);
        assert!(
            select(
                &tree,
                &feed(&recency, &[(&laptop, "Laptop")]),
                &BTreeMap::new(),
                now
            )
            .is_none(),
            "a room this desktop does not have offers nothing"
        );

        // The laptop focuses Notes, then the phone (which took Notes over
        // earlier) closes it: the tombstone outranks the laptop's offer.
        let mut continuity = PersonalContinuity::default();
        continuity
            .upsert(snapshot(&laptop, 3, room(&tree), NOTES))
            .unwrap();
        let mut closing = snapshot(&phone, 2, room(&tree), NOTES);
        let mut closed = snapshot(&phone, 9, room(&tree), TERMINAL);
        closed.workspaces[0]
            .apps
            .retain(|app| app.id != AppInstanceId(NOTES));
        closing.focus = None;
        closed.record_closures_from(&closing).unwrap();
        continuity.upsert(closed).unwrap();
        let mut recency = FocusRecency::default();
        recency.observe(&continuity, now);
        let offer = select(
            &tree,
            &feed(&recency, &[(&laptop, "Laptop")]),
            &BTreeMap::new(),
            now,
        );
        assert!(offer.is_none(), "a closed app is not offered: {offer:?}");
    }

    #[test]
    fn the_newest_stamp_wins_across_devices() {
        let tree = tree();
        let laptop = device("laptop");
        let tablet = device("tablet");
        let now = Instant::now();
        let mut continuity = PersonalContinuity::default();
        continuity
            .upsert(snapshot(&laptop, 4, room(&tree), NOTES))
            .unwrap();
        continuity
            .upsert(snapshot(&tablet, 8, room(&tree), NOTES))
            .unwrap();
        let mut recency = FocusRecency::default();
        recency.observe(&continuity, now);
        let offer = select(
            &tree,
            &feed(&recency, &[(&laptop, "Laptop"), (&tablet, "Tablet")]),
            &BTreeMap::new(),
            now,
        )
        .unwrap();
        assert_eq!(offer.device, tablet);
    }

    #[test]
    fn dismissal_lasts_until_that_device_focus_changes() {
        let tree = tree();
        let laptop = device("laptop");
        let start = Instant::now();
        let mut continuity = PersonalContinuity::default();
        continuity
            .upsert(snapshot(&laptop, 3, room(&tree), NOTES))
            .unwrap();
        let mut recency = FocusRecency::default();
        recency.observe(&continuity, start);
        let mut offers = ContinuitySuggestions::default();
        assert!(offers.set_feed(feed(&recency, &[(&laptop, "Laptop")])));
        let offer = offers.current(&tree, start).unwrap();
        offers.dismiss(&offer);
        assert!(offers.current(&tree, start).is_none());

        // An unrelated republish of the same focus neither revives the
        // offer nor refreshes its age.
        continuity
            .upsert(snapshot(&laptop, 5, room(&tree), NOTES))
            .unwrap();
        recency.observe(&continuity, start + Duration::from_secs(60));
        offers.set_feed(feed(&recency, &[(&laptop, "Laptop")]));
        assert!(offers.current(&tree, start).is_none());

        // Moving away and back is a new focus: it is offered again.
        continuity
            .upsert(snapshot(&laptop, 6, room(&tree), TERMINAL))
            .unwrap();
        recency.observe(&continuity, start + Duration::from_secs(61));
        offers.set_feed(feed(&recency, &[(&laptop, "Laptop")]));
        continuity
            .upsert(snapshot(&laptop, 7, room(&tree), NOTES))
            .unwrap();
        let later = start + Duration::from_secs(62);
        recency.observe(&continuity, later);
        offers.set_feed(feed(&recency, &[(&laptop, "Laptop")]));
        assert_eq!(
            offers.current(&tree, later).map(|offer| offer.tab),
            Some(NOTES)
        );
    }
}
