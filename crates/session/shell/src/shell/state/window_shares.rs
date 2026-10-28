//! The windows members share with a room whose layout is personal, as this
//! device shows them ([`tilo_desktop_model::window_share`]).
//!
//! A personal room carries none of its members' windows, so a window one of
//! them shares reaches the others only through its share: the room's record
//! of who shared which window, hosted on which device, until when, and, for
//! a Tilo app's window, what it shows. Each other member shows it as a tab
//! under the sharer's own tab id in its own layout, where a window arriving
//! from a peer goes: a window group of its own at the end of the
//! workspace's last strip.
//!
//! - **An outside window** is a [`TileContent::HostedWindow`] tab, which the
//!   sharer's stream of it names.
//! - **A Tilo app's window** is the same app, opened here on the resource it
//!   shows: a [`TileContent::PeerWindow`] tab that
//!   [`crate::shell::peer_windows`] opens on the record the share names
//!   ([`WorkspaceTree::room_tab_record`]). Nothing of it is streamed; the
//!   sharer's cursor and selection show in it as any collaborator's do.
//!
//! A Tilo app's window opens at the item the sharer's window is at, and
//! follows it while the share lasts: the sharer's device says the item it
//! moves to by sharing it again under the same share id, and this device
//! opens its window there through the app's own `open`, until the person
//! here moves the window elsewhere themselves. From then on it stays where
//! they put it ([`WorkspaceTree::shared_window_opening`]).
//!
//! When the share ends the tab goes, with the rest of the windows of others
//! this device dropped from the room; but a Tilo app's window the person
//! here used (gave it input) stays, as their own window from then on.
//! Which windows were used, and which stopped following, is kept with the
//! device's layout ([`SharedWindowUse`]), so a restart during a share
//! changes neither.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tilo_desktop_model::window_share::WindowShare;

use super::{
    DesktopTree, RoomId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    WorkspaceTree, peer_salt_of,
};

/// The windows members share with each held room, as this device last read
/// them, and what this device did with the ones it shows.
#[derive(Clone, Debug, Default)]
pub struct WindowShares {
    /// By room, by tab: the room's record as its admitted document says.
    by_room: HashMap<RoomId, BTreeMap<TabId, WindowShare>>,
    /// The app windows (surfaces) of shared windows whose share ended
    /// unused, for the compositor to close.
    closing: Vec<u64>,
    /// Where each Tilo app window others shared that still follows its
    /// sharer was last asked to open, by tab.
    following: HashMap<TabId, Following>,
}

/// Where a shared Tilo app window that follows its sharer was last asked to
/// open, and whether it got there.
#[derive(Clone, Debug, Default)]
struct Following {
    /// The item it was last asked to open at.
    asked: Option<String>,
    /// It has shown that item since.
    arrived: bool,
}

/// What the person here did with the Tilo app windows others shared, by
/// tab: kept with the device's layout, so a restart during a share keeps
/// it. Device-local, never in a room's document.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SharedWindowUse {
    /// The windows they used (gave input): each stays, as theirs, when its
    /// share ends.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    used: BTreeSet<TabId>,
    /// The windows they moved elsewhere themselves: each stays where they
    /// put it rather than following its sharer.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    navigated: BTreeSet<TabId>,
}

impl SharedWindowUse {
    /// Whether nothing is kept: the layout's file leaves it out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.used.is_empty() && self.navigated.is_empty()
    }
}

impl WorkspaceTree {
    /// The windows shared with `room` that still last at `now_ms`, by tab:
    /// the room's record as this device last took in its document.
    pub fn shared_windows(
        &self,
        room: RoomId,
        now_ms: u64,
    ) -> impl Iterator<Item = &WindowShare> + '_ {
        self.window_shares
            .by_room
            .get(&room)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(move |share| share.until_ms.is_none_or(|until| now_ms < until))
    }

    /// Whether the window `tab` is shared with `room` at `now_ms`.
    pub fn is_window_shared(&self, room: RoomId, tab: TabId, now_ms: u64) -> bool {
        self.shared_windows(room, now_ms)
            .any(|share| share.tab == tab)
    }

    /// Whether `tab` is a Tilo app's window shared with any room held here:
    /// shown as the same app opened on what it shows, never as a stream.
    pub fn is_shared_app_window(&self, tab: TabId) -> bool {
        self.window_shares
            .by_room
            .values()
            .any(|shares| shares.get(&tab).is_some_and(|share| share.shows.is_some()))
    }

    /// Whether this device streams its window `tab` to `room` because it
    /// shared it at `now_ms`: an outside window of its own only. A Tilo
    /// app's window is shared as what it shows
    /// ([`WindowShare::shows`]), which the others open; a window of
    /// another member's that this device shows is theirs to share.
    pub fn streams_shared_window(&self, room: RoomId, tab: TabId, now_ms: u64) -> bool {
        let local_salt = self.local_salt();
        local_salt != 0
            && peer_salt_of(tab) == local_salt
            && self
                .shared_windows(room, now_ms)
                .any(|share| share.tab == tab && share.shows.is_none())
    }

    /// The windows this device shared with a room that its workspace here
    /// no longer shows (closed, or moved to another workspace), at `now_ms`:
    /// shares that outlived their window, by room. A room with no workspace
    /// here is left out: leaving a workspace ends its shares on the way out,
    /// as the network releases the room.
    pub fn outlived_window_shares(&self, now_ms: u64) -> Vec<(RoomId, WindowShare)> {
        let local_salt = self.local_salt();
        if local_salt == 0 {
            return Vec::new();
        }
        let mut outlived = Vec::new();
        for &room in self.window_shares.by_room.keys() {
            let Some(workspace) = self.desktop.workspaces.iter().find(|w| w.room() == room) else {
                continue;
            };
            for share in self.shared_windows(room, now_ms) {
                let shown = workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .any(|tab| tab.id == share.tab);
                if peer_salt_of(share.tab) == local_salt && !shown {
                    outlived.push((room, share.clone()));
                }
            }
        }
        outlived
    }

    /// The windows this device shared with `room` that still last at
    /// `now_ms`: the ones it hosts.
    pub fn own_window_shares(&self, room: RoomId, now_ms: u64) -> Vec<WindowShare> {
        let local_salt = self.local_salt();
        self.shared_windows(room, now_ms)
            .filter(|share| local_salt != 0 && peer_salt_of(share.tab) == local_salt)
            .cloned()
            .collect()
    }

    /// Hold `shares` as the windows shared with `room`.
    pub(super) fn take_window_shares(
        &mut self,
        room: RoomId,
        shares: BTreeMap<TabId, WindowShare>,
    ) {
        if shares.is_empty() {
            self.window_shares.by_room.remove(&room);
        } else {
            self.window_shares.by_room.insert(room, shares);
        }
    }

    /// The record a Tilo app's window shared with `room` as `tab` shows, as
    /// its route (empty for the app's own record of the room): what this
    /// device opens the app on.
    pub(super) fn shared_window_record(&self, room: RoomId, tab: TabId) -> Option<String> {
        let share = self.window_shares.by_room.get(&room)?.get(&tab)?;
        Some(share.shows.as_ref()?.record.clone())
    }

    /// The tab of a Tilo app window another member shared that shows as the
    /// app window `surface_id` here.
    fn shared_tab_of_surface(&self, surface_id: u64) -> Option<TabId> {
        let local_salt = self.local_salt();
        self.desktop.workspaces.iter().find_map(|workspace| {
            let shares = self.window_shares.by_room.get(&workspace.room())?;
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|tab| {
                    matches!(tab.tile, TileContent::Foreign { surface_id: shown, .. } if shown == surface_id)
                })
                .map(|tab| tab.id)
                .filter(|tab| peer_salt_of(*tab) != local_salt && shares.contains_key(tab))
        })
    }

    /// Whether [`Self::note_used_and_take_ended`] has anything to do with
    /// `used`: a shared window not yet used here, or an ended one to close.
    /// Read first, so input to every other window writes nothing.
    #[must_use]
    pub fn shared_window_work(&self, used: &[u64]) -> bool {
        !self.window_shares.closing.is_empty()
            || used.iter().any(|surface| {
                self.shared_tab_of_surface(*surface)
                    .is_some_and(|tab| !self.shared_window_use.used.contains(&tab))
            })
    }

    /// The person gave input to the app windows `used` here: each that is a
    /// Tilo app window another member shared is theirs to keep once the
    /// share ends, a restart in between included. Answers the app windows of
    /// shared windows whose share ended while nobody here used them, for the
    /// compositor to close.
    pub fn note_used_and_take_ended(&mut self, used: &[u64]) -> Vec<u64> {
        let mut noted = false;
        for surface in used {
            if let Some(tab) = self.shared_tab_of_surface(*surface) {
                noted |= self.shared_window_use.used.insert(tab);
            }
        }
        if noted {
            self.queue_save();
        }
        std::mem::take(&mut self.window_shares.closing)
    }

    /// The item the Tilo app window `tab` another member shared is at, as
    /// its share says now: what this device's window of it follows.
    fn shared_item(&self, tab: TabId) -> Option<&str> {
        if peer_salt_of(tab) == self.local_salt() {
            return None;
        }
        self.window_shares
            .by_room
            .values()
            .find_map(|shares| shares.get(&tab))?
            .shows
            .as_ref()?
            .item
            .as_deref()
    }

    /// Where to open this device's window of `tab`, a Tilo app window
    /// another member shared, now that it is at `at` (the item its bar
    /// anchors, `None` before it says): the item the sharer's window is at,
    /// when this window still follows it and was not asked to open there
    /// yet. Ask the app's `open` at it, then say so
    /// ([`Self::shared_window_opened`]); until then it is answered again.
    ///
    /// A window that showed the item it was asked to open, and then shows
    /// another without being asked, was moved by the person here: it stops
    /// following, for as long as the share lasts.
    pub fn shared_window_opening(&mut self, tab: TabId, at: Option<&str>) -> Option<String> {
        let item = self.shared_item(tab)?.to_owned();
        if self.shared_window_use.navigated.contains(&tab) {
            return None;
        }
        let follow = self.window_shares.following.entry(tab).or_default();
        if let (Some(asked), Some(at)) = (follow.asked.as_deref(), at) {
            if asked == at {
                follow.arrived = true;
            } else if follow.arrived {
                self.window_shares.following.remove(&tab);
                self.shared_window_use.navigated.insert(tab);
                self.queue_save();
                return None;
            }
        }
        if at == Some(item.as_str()) {
            *follow = Following {
                asked: Some(item),
                arrived: true,
            };
            return None;
        }
        (follow.asked.as_deref() != Some(item.as_str())).then_some(item)
    }

    /// This device's window of `tab`, a Tilo app window another member
    /// shared, was asked to open at `item`
    /// ([`Self::shared_window_opening`]).
    pub fn shared_window_opened(&mut self, tab: TabId, item: &str) {
        self.window_shares.following.insert(
            tab,
            Following {
                asked: Some(item.to_owned()),
                arrived: false,
            },
        );
    }

    /// Whether this device's window of `tab`, a Tilo app window another
    /// member shared, follows the item its sharer's window is at.
    #[must_use]
    pub fn follows_shared_window(&self, tab: TabId) -> bool {
        self.shared_item(tab).is_some() && !self.shared_window_use.navigated.contains(&tab)
    }

    /// Forget what was kept about shared windows whose share in `room`
    /// ended (`shares` are the ones that last): where they followed to and
    /// whether they stopped following. Whether they were used stays until
    /// the window is kept ([`Self::keep_used_shared_windows`]).
    fn forget_ended_follows(&mut self, room: RoomId, shares: &[WindowShare]) {
        let desktop = &self.desktop;
        let ended = |tab: &TabId| {
            !shares.iter().any(|share| share.tab == *tab)
                && room_showing(desktop, *tab).is_none_or(|at| at == room)
        };
        self.window_shares.following.retain(|tab, _| !ended(tab));
        let before = self.shared_window_use.navigated.len();
        self.shared_window_use.navigated.retain(|tab| !ended(tab));
        if self.shared_window_use.navigated.len() != before {
            self.queue_save();
        }
    }

    /// Keep the workspace addressing `room`, a room whose layout is
    /// personal, to this device's own windows (`local_salt`'s) and the
    /// windows shared with the room. A personal room shares no member's
    /// windows: nothing of its contents is taken in, and what this device
    /// mirrored of other people's windows while it shared them goes, by the
    /// room's topology registry `owners`. Every window of this device's own
    /// stays where it is. A window someone shared with the room stays for
    /// as long as the share lasts, as a tab of theirs in this device's own
    /// layout. Reports whether anything changed.
    pub(super) fn keep_personal_room(
        &mut self,
        room: RoomId,
        local_salt: u16,
        owners: &BTreeMap<u16, String>,
        now_ms: u64,
    ) -> bool {
        let shares: Vec<WindowShare> = self.shared_windows(room, now_ms).cloned().collect();
        self.forget_ended_follows(room, &shares);
        let kept = self.keep_used_shared_windows(room, local_salt, &shares);
        let theirs = |tab: &Tab| {
            let salt = peer_salt_of(tab.id);
            salt != 0
                && salt != local_salt
                && salt != tilo_desktop_model::ids::RESERVED_SALT
                && !shares
                    .iter()
                    .any(|share| share.tab == tab.id && shows_share(share, &tab.tile))
                && (owners.is_empty()
                    || owners.contains_key(&salt)
                    || matches!(
                        tab.tile,
                        TileContent::PeerWindow { .. } | TileContent::HostedWindow { .. }
                    ))
        };
        let closing = self.ended_app_windows(room, &theirs);
        self.window_shares.closing.extend(closing);
        let dropped = tilo_desktop_model::projection::drop_windows_of_others(
            &mut self.desktop,
            room,
            local_salt,
            &theirs,
        );
        let device_names = self.room_peer_labels(room);
        let shown = show_window_shares(
            &mut self.desktop,
            room,
            local_salt,
            &shares,
            &device_names,
            &|app_id| crate::shell::peer_windows::app_display_name(app_id, app_id),
        );
        kept || dropped || shown
    }

    /// The app windows (surfaces) of the tabs of `room`'s workspace that
    /// `theirs` is about to take out: the shared Tilo app windows whose
    /// share ended, opened here for it.
    fn ended_app_windows(&self, room: RoomId, theirs: &dyn Fn(&Tab) -> bool) -> Vec<u64> {
        self.desktop
            .workspaces
            .iter()
            .filter(|workspace| workspace.room() == room)
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter(|tab| theirs(tab))
            .filter_map(|tab| match tab.tile {
                TileContent::Foreign { surface_id, .. } => Some(surface_id),
                _ => None,
            })
            .collect()
    }

    /// Make each Tilo app window of another member's in `room`'s workspace
    /// that the person here used, and whose share is no longer among
    /// `shares`, this device's own window: the same tab under an id of
    /// this device's (`local_salt`'s), its group too where the group was
    /// the share's, with what this device keeps about the tab's app
    /// carried over. Reports whether any was kept.
    fn keep_used_shared_windows(
        &mut self,
        room: RoomId,
        local_salt: u16,
        shares: &[WindowShare],
    ) -> bool {
        // The used windows whose share ended: those of this room's
        // workspace, to keep, and those no workspace shows any more, to
        // forget. A used window of another room waits for its own room.
        let room_of = |tab: TabId| room_showing(&self.desktop, tab);
        let ended: Vec<(TabId, Option<RoomId>)> = self
            .shared_window_use
            .used
            .iter()
            .copied()
            .filter(|tab| !shares.iter().any(|share| share.tab == *tab))
            .map(|tab| (tab, room_of(tab)))
            .filter(|(_, at)| at.is_none_or(|at| at == room))
            .collect();
        let forgot = !ended.is_empty();
        let mut kept = false;
        for (old, at) in ended {
            self.shared_window_use.used.remove(&old);
            if at.is_none() || peer_salt_of(old) == local_salt {
                continue;
            }
            let new = self.mint_id();
            let Some(workspace) = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.room() == room)
            else {
                continue;
            };
            let mut renamed = false;
            for strip in &mut workspace.strips {
                for group in &mut strip.window_groups {
                    let Some(tab) = group.tabs.iter_mut().find(|tab| tab.id == old) else {
                        continue;
                    };
                    tab.id = new;
                    renamed = true;
                    if group.active_tab == old {
                        group.active_tab = new;
                    }
                    for id in &mut group.focus_history {
                        if *id == old {
                            *id = new;
                        }
                    }
                    if group.id == old {
                        group.id = new;
                        for active in self.desktop.active_group.values_mut() {
                            if *active == old {
                                *active = new;
                            }
                        }
                    }
                }
            }
            if !renamed {
                continue;
            }
            if let Some(package) = self.package_launches.remove(&old) {
                self.package_launches.insert(new, package);
            }
            if let Some(record) = self.local_app_records.remove(&old) {
                self.local_app_records.insert(new, record);
            }
            kept = true;
        }
        if kept || forgot {
            self.queue_save();
        }
        kept
    }
}

/// The room of the workspace in `desktop` that shows the tab `tab`.
fn room_showing(desktop: &DesktopTree, tab: TabId) -> Option<RoomId> {
    desktop.workspaces.iter().find_map(|workspace| {
        workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .any(|group| group.tabs.iter().any(|candidate| candidate.id == tab))
            .then(|| workspace.room())
    })
}

/// Whether `tile` is how this device shows `share`: an outside window's
/// stream, or the Tilo app's own window opened here on what it shows
/// (waiting to open, open, or reopening after a restart).
fn shows_share(share: &WindowShare, tile: &TileContent) -> bool {
    match tile {
        TileContent::HostedWindow { .. } => share.shows.is_none(),
        TileContent::PeerWindow { app_id, .. }
        | TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. } => {
            share.shows.is_some() && app_id.eq_ignore_ascii_case(&share.app_id)
        }
        _ => false,
    }
}

/// The windows shared with `room` that `doc`, its admitted document,
/// records as live at `now_ms`.
pub(super) fn window_shares_of(
    room: RoomId,
    doc: &automerge::AutoCommit,
    now_ms: u64,
) -> BTreeMap<TabId, WindowShare> {
    tilo_desktop_model::window_share::window_shares(
        &tilo_desktop_model::projection::room_policy_of(doc, &room.to_string()),
        &tilo_desktop_model::window_share::device_salts(doc),
        now_ms,
    )
}

/// The presence handle of the device `host` (its hex key) names.
pub(crate) fn host_handle(host: &str) -> Option<u64> {
    let bytes: [u8; 32] = hex::decode(host).ok()?.try_into().ok()?;
    let endpoint = iroh::EndpointId::from_bytes(&bytes).ok()?;
    Some(tilo_network_actor::behaviour::peer_liveness_handle(
        &endpoint,
    ))
}

/// Show in the workspace addressing `room` each of `shares` hosted on
/// another device than this one (`local_salt`'s), that it does not show
/// yet: a tab of the sharer's window, in a window group of its own at the
/// end of the workspace's last strip, named by the app (`app_name`) and by
/// the device it runs on (`device_names`, by presence handle). Reports
/// whether anything changed.
pub(super) fn show_window_shares<'a>(
    desktop: &mut DesktopTree,
    room: RoomId,
    local_salt: u16,
    shares: impl IntoIterator<Item = &'a WindowShare>,
    device_names: &BTreeMap<u64, String>,
    app_name: &dyn Fn(&str) -> String,
) -> bool {
    let Some(workspace) = desktop.workspaces.iter_mut().find(|w| w.room() == room) else {
        return false;
    };
    let mut changed = false;
    for share in shares {
        if tilo_desktop_model::peer_salt_of(share.tab) == local_salt
            || workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|tab| tab.id == share.tab)
        {
            continue;
        }
        let label = app_name(&share.app_id);
        let host_label = host_handle(&share.host)
            .and_then(|handle| device_names.get(&handle).cloned())
            .unwrap_or_default();
        // The group takes the tab's id: nothing else of the sharer's is in
        // this layout for it to meet.
        let group = WindowGroup {
            id: share.tab,
            tabs: vec![Tab {
                id: share.tab,
                title: label.clone(),
                tile: if share.shows.is_some() {
                    // The same app, opened here on what the window shows.
                    TileContent::PeerWindow {
                        app_id: share.app_id.clone(),
                        label,
                        note: String::new(),
                        missing: None,
                    }
                } else {
                    TileContent::HostedWindow {
                        app_id: share.app_id.clone(),
                        label,
                        host: share.host.clone(),
                        host_label,
                    }
                },
                app_title: None,
            }],
            active_tab: share.tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        };
        if workspace.strips.is_empty() {
            workspace.strips.push(Strip {
                id: StripId(share.tab),
                name: String::new(),
                window_groups: Vec::new(),
            });
        }
        if let Some(strip) = workspace.strips.last_mut() {
            strip.window_groups.push(group);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests;
