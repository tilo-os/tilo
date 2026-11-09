//! Where each joined workspace stands with its host, as apps in it are told
//! ([`ui::workspace_sync`]): an app shows "Connecting to Lounge…" rather
//! than an empty workspace until the workspace's first sync with its host
//! completed, and that the host (or this device) has to update when they
//! speak different peer protocol versions, or that the host cannot be
//! reached once a try to reach it failed (the apps work on what this device
//! has meanwhile).
//!
//! The network actor records what happens to each workspace's host session
//! (keyed by the workspace's topic, [`crate::workspace_topic_id`]); the session
//! reads it when it tells each app window its appearance. A workspace the
//! actor never dials, one with no host or one this device hosts itself, has
//! no entry: its apps' empty states are their own.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use tilo_lock::MutexRecover as _;
use ui::workspace_sync::WorkspaceSyncState;

static STATES: LazyLock<Mutex<HashMap<u64, WorkspaceSyncState>>> = LazyLock::new(Mutex::default);
/// This device's writes to each workspace that are not yet synced, by topic.
static UNSYNCED: LazyLock<Mutex<HashMap<u64, u32>>> = LazyLock::new(Mutex::default);
/// The workspaces whose writes this device cannot keep on disk now, by topic.
static CANNOT_KEEP: LazyLock<Mutex<std::collections::HashSet<u64>>> = LazyLock::new(Mutex::default);
/// Since when each workspace has waited for its first sync, for the log
/// line that says how long it took.
static WAITING_SINCE: LazyLock<Mutex<HashMap<u64, std::time::Instant>>> =
    LazyLock::new(Mutex::default);

/// The actor is dialling the host of the workspace on topic `ws`. One that
/// synced once stays synced: what it held then is here.
pub fn connecting(ws: u64) {
    let mut states = STATES.lock_recover();
    let state = states.entry(ws).or_insert(WorkspaceSyncState::Connecting);
    if !matches!(state, WorkspaceSyncState::Synced) {
        *state = WorkspaceSyncState::Connecting;
        WAITING_SINCE
            .lock_recover()
            .entry(ws)
            .or_insert_with(std::time::Instant::now);
    }
    drop(states);
}

/// A sync with the host of the workspace on topic `ws` completed.
pub fn synced(ws: u64) {
    let before = STATES.lock_recover().insert(ws, WorkspaceSyncState::Synced);
    let waited = WAITING_SINCE.lock_recover().remove(&ws);
    if let Some(since) = waited {
        tracing::info!(
            target: "tilo::workspace_sync",
            ws,
            after = ?before,
            connect_to_sync_ms = since.elapsed().as_secs_f64() * 1_000.0,
            "workspace synced with its host"
        );
    }
}

/// A try to reach the host of the workspace on topic `ws` failed. One that
/// synced before keeps what it holds; one whose host speaks another peer
/// protocol keeps saying which side has to update.
pub fn unreachable(ws: u64) {
    let mut states = STATES.lock_recover();
    if let Some(state) = states.get_mut(&ws)
        && matches!(state, WorkspaceSyncState::Connecting)
    {
        *state = WorkspaceSyncState::Unreachable;
    }
    drop(states);
}

/// The host of the workspace on topic `ws` speaks peer protocol `theirs`,
/// not this device's: nothing syncs until the one behind updates. One that
/// synced before keeps what it holds, and says so.
pub fn other_version(ws: u64, theirs: u32) {
    let state = if theirs > neighbour_wire::PEER_PROTOCOL_VERSION {
        WorkspaceSyncState::DeviceNeedsUpdate
    } else {
        WorkspaceSyncState::HostNeedsUpdate
    };
    let mut states = STATES.lock_recover();
    let current = states.entry(ws).or_insert(state);
    if !matches!(current, WorkspaceSyncState::Synced) {
        *current = state;
    }
    drop(states);
}

/// This device holds `count` writes to the workspace on topic `ws` that are
/// not yet synced: no host of the workspace carries them now, or the host
/// did not keep them. Whether that changed what the workspace's apps are
/// told.
pub(crate) fn unsynced(ws: u64, count: usize) -> bool {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    let mut unsynced = UNSYNCED.lock_recover();
    let before = if count == 0 {
        unsynced.remove(&ws)
    } else {
        unsynced.insert(ws, count)
    };
    drop(unsynced);
    before.unwrap_or(0) != count
}

/// Whether this device cannot keep its writes to the workspace on topic
/// `ws` on disk now.
pub fn cannot_keep_writes(ws: u64) -> bool {
    CANNOT_KEEP.lock_recover().contains(&ws)
}

/// The workspace on topic `ws` is no longer held: forget it.
pub fn forget(ws: u64) {
    STATES.lock_recover().remove(&ws);
    UNSYNCED.lock_recover().remove(&ws);
    WAITING_SINCE.lock_recover().remove(&ws);
}

/// How many of this device's writes to the workspace on topic `ws` are not
/// yet synced.
pub fn unsynced_writes(ws: u64) -> u32 {
    UNSYNCED.lock_recover().get(&ws).copied().unwrap_or(0)
}

/// Where the workspace on topic `ws` stands; `None` for one with no host.
pub fn state(ws: u64) -> Option<WorkspaceSyncState> {
    STATES.lock_recover().get(&ws).copied()
}

/// What an app window in the workspace `name`, on workspace topic `ws`, is
/// told, with whether the app may write the workspace now (`writable`). A
/// workspace with no host still tells an app that answer, as one synced as
/// far as it goes.
pub fn told(
    ws: u64,
    name: &str,
    writable: Option<bool>,
) -> Option<ui::workspace_sync::WorkspaceSync> {
    let state = match (state(ws), writable) {
        (Some(state), _) => state,
        (None, Some(_)) => WorkspaceSyncState::Synced,
        (None, None) => return None,
    };
    Some(ui::workspace_sync::WorkspaceSync {
        state,
        workspace: name.to_owned(),
        writable,
        unsynced_writes: unsynced_writes(ws),
        cannot_keep_writes: cannot_keep_writes(ws),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_is_connecting_until_its_first_sync_and_says_who_has_to_update() {
        const WS: u64 = 0x5eed_0001;
        assert_eq!(
            told(WS, "Lounge", None),
            None,
            "a workspace with no host is told nothing"
        );
        assert_eq!(
            told(WS, "Lounge", Some(true)).map(|told| (told.state, told.writable)),
            Some((WorkspaceSyncState::Synced, Some(true))),
            "but whether its app may write it"
        );
        connecting(WS);
        assert_eq!(state(WS), Some(WorkspaceSyncState::Connecting));
        unreachable(WS);
        assert_eq!(
            state(WS),
            Some(WorkspaceSyncState::Unreachable),
            "a failed try says the host cannot be reached"
        );
        other_version(WS, neighbour_wire::PEER_PROTOCOL_VERSION - 1);
        assert_eq!(state(WS), Some(WorkspaceSyncState::HostNeedsUpdate));
        other_version(WS, neighbour_wire::PEER_PROTOCOL_VERSION + 1);
        assert_eq!(state(WS), Some(WorkspaceSyncState::DeviceNeedsUpdate));
        unreachable(WS);
        assert_eq!(
            state(WS),
            Some(WorkspaceSyncState::DeviceNeedsUpdate),
            "who has to update stays said"
        );
        connecting(WS);
        synced(WS);
        assert_eq!(
            told(WS, "Lounge", Some(false)),
            Some(ui::workspace_sync::WorkspaceSync {
                state: WorkspaceSyncState::Synced,
                workspace: "Lounge".into(),
                writable: Some(false),
                unsynced_writes: 0,
                cannot_keep_writes: false,
            })
        );
        // Writes held for the workspace's hosts are not yet synced, and say
        // so
        // until a host keeps them.
        assert!(unsynced(WS, 2));
        assert!(!unsynced(WS, 2), "said once");
        assert_eq!(
            told(WS, "Lounge", None).map(|told| (told.state, told.unsynced_writes)),
            Some((WorkspaceSyncState::Synced, 2))
        );
        assert!(unsynced(WS, 0));
        assert_eq!(
            told(WS, "Lounge", None).map(|told| told.unsynced_writes),
            Some(0)
        );
        // Dialling again later, or a host gone to another version, does not
        // take back what the workspace holds.
        connecting(WS);
        other_version(WS, 0);
        unreachable(WS);
        assert_eq!(state(WS), Some(WorkspaceSyncState::Synced));
        forget(WS);
        assert_eq!(state(WS), None);
    }
}
