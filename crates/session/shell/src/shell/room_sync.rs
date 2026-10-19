//! Where each joined room stands with its host, as apps in it are told
//! ([`ui::room_sync`]): an app shows "Connecting to Lounge…" rather than an
//! empty room until the room's first sync with its host completed, and that
//! the host (or this device) has to update when they speak different peer
//! protocol versions.
//!
//! The network actor records what happens to each room's host session
//! (keyed by the room's topic, [`crate::shell::network::room_topic_id`]);
//! the compositor reads it when it tells each app window its appearance. A
//! room the actor never dialled has no entry: its workspace syncs with no
//! host, and its apps' empty states are their own.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use tilo_lock::MutexRecover as _;
use ui::room_sync::RoomSyncState;

static ROOMS: LazyLock<Mutex<HashMap<u64, RoomSyncState>>> = LazyLock::new(Mutex::default);

/// The actor is dialling the host of the room on topic `ws`. A room that
/// synced once stays synced: what it held then is here.
pub(crate) fn connecting(ws: u64) {
    let mut rooms = ROOMS.lock_recover();
    let state = rooms.entry(ws).or_insert(RoomSyncState::Connecting);
    if !matches!(state, RoomSyncState::Synced) {
        *state = RoomSyncState::Connecting;
    }
}

/// A sync with the host of the room on topic `ws` completed.
pub(crate) fn synced(ws: u64) {
    ROOMS.lock_recover().insert(ws, RoomSyncState::Synced);
}

/// The host of the room on topic `ws` speaks peer protocol `theirs`, not
/// this device's: nothing syncs until the one behind updates. A room that
/// synced before keeps what it holds, and says so.
pub(crate) fn other_version(ws: u64, theirs: u32) {
    let state = if theirs > neighbour_wire::PEER_PROTOCOL_VERSION {
        RoomSyncState::DeviceNeedsUpdate
    } else {
        RoomSyncState::HostNeedsUpdate
    };
    let mut rooms = ROOMS.lock_recover();
    let current = rooms.entry(ws).or_insert(state);
    if !matches!(current, RoomSyncState::Synced) {
        *current = state;
    }
}

/// The room on topic `ws` is no longer held: forget it.
pub(crate) fn forget(ws: u64) {
    ROOMS.lock_recover().remove(&ws);
}

/// Where the room on topic `ws` stands; `None` for one with no host.
pub(crate) fn state(ws: u64) -> Option<RoomSyncState> {
    ROOMS.lock_recover().get(&ws).copied()
}

/// What an app window in the workspace `name`, on room topic `ws`, is told.
pub(crate) fn told(ws: u64, name: &str) -> Option<ui::room_sync::RoomSync> {
    state(ws).map(|state| ui::room_sync::RoomSync {
        state,
        workspace: name.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // USER REQUIREMENT (2026-10-19): "If a v6 client meets the v5 relay, the user must see a clear "update needed" state, never a silent empty room."
    #[test]
    fn a_room_is_connecting_until_its_first_sync_and_says_who_has_to_update() {
        const WS: u64 = 0x5eed_0001;
        assert_eq!(
            told(WS, "Lounge"),
            None,
            "a room with no host is told nothing"
        );
        connecting(WS);
        assert_eq!(state(WS), Some(RoomSyncState::Connecting));
        other_version(WS, neighbour_wire::PEER_PROTOCOL_VERSION - 1);
        assert_eq!(state(WS), Some(RoomSyncState::HostNeedsUpdate));
        other_version(WS, neighbour_wire::PEER_PROTOCOL_VERSION + 1);
        assert_eq!(state(WS), Some(RoomSyncState::DeviceNeedsUpdate));
        connecting(WS);
        synced(WS);
        assert_eq!(
            told(WS, "Lounge"),
            Some(ui::room_sync::RoomSync {
                state: RoomSyncState::Synced,
                workspace: "Lounge".into(),
            })
        );
        // Dialling again later, or a host gone to another version, does not
        // take back what the room holds.
        connecting(WS);
        other_version(WS, 0);
        assert_eq!(state(WS), Some(RoomSyncState::Synced));
        forget(WS);
        assert_eq!(state(WS), None);
    }
}
