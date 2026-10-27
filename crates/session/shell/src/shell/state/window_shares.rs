//! The windows members share with a room whose layout is personal, as this
//! device shows them ([`tilo_desktop_model::window_share`]).
//!
//! A personal room carries none of its members' windows, so a window one of
//! them shares reaches the others only through its share: the room's record
//! of who shared which window, hosted on which device, until when. Each
//! other member shows it as a tab of the sharer's window
//! ([`TileContent::HostedWindow`], under the sharer's own tab id, which the
//! sharer's stream names) in its own layout, where a window arriving from
//! a peer goes: a window group of its own at the end of the workspace's
//! last strip. When the share ends the tab goes, with the rest of the
//! windows of others this device dropped from the room.

use std::collections::BTreeMap;

use tilo_desktop_model::window_share::WindowShare;

use super::{
    DesktopTree, RoomId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    WorkspaceTree, peer_salt_of,
};

impl WorkspaceTree {
    /// The windows shared with `room` that still last at `now_ms`, by tab:
    /// the room's record as this device last took in its document.
    pub fn shared_windows(
        &self,
        room: RoomId,
        now_ms: u64,
    ) -> impl Iterator<Item = &WindowShare> + '_ {
        self.window_shares
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

    /// Hold `shares` as the windows shared with `room`.
    pub(super) fn take_window_shares(
        &mut self,
        room: RoomId,
        shares: BTreeMap<TabId, WindowShare>,
    ) {
        if shares.is_empty() {
            self.window_shares.remove(&room);
        } else {
            self.window_shares.insert(room, shares);
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
        let theirs = |tab: &Tab| {
            let salt = peer_salt_of(tab.id);
            salt != 0
                && salt != local_salt
                && salt != tilo_desktop_model::ids::RESERVED_SALT
                && !(shares.iter().any(|share| share.tab == tab.id)
                    && matches!(tab.tile, TileContent::HostedWindow { .. }))
                && (owners.is_empty()
                    || owners.contains_key(&salt)
                    || matches!(
                        tab.tile,
                        TileContent::PeerWindow { .. } | TileContent::HostedWindow { .. }
                    ))
        };
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
        dropped || shown
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
        &tilo_desktop_model::projection::contents_owners_of(doc),
        now_ms,
    )
}

/// The presence handle of the device `host` (its hex key) names.
pub(crate) fn host_handle(host: &str) -> Option<u64> {
    let bytes: [u8; 32] = hex::decode(host).ok()?.try_into().ok()?;
    let endpoint = iroh::EndpointId::from_bytes(&bytes).ok()?;
    Some(crate::network::behaviour::peer_liveness_handle(&endpoint))
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
                tile: TileContent::HostedWindow {
                    app_id: share.app_id.clone(),
                    label,
                    host: share.host.clone(),
                    host_label,
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
