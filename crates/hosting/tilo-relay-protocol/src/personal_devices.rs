//! The person's other devices as one machine records them: the devices it
//! dials directly, the workspace rooms they share, and what its running
//! session last saw of each of them.
//!
//! Enrollment writes the first two beside the identity root (normally
//! `/var/lib/tilo`); only a privileged enrollment path may change them. The
//! session reads them on every refresh, so a new entry takes effect without a
//! restart. The presence snapshot is the session's read-only projection of
//! its live connections, for status tools that run outside the session.
//!
//! Revocations the person's root signed reach this machine through its
//! session or `tilo-identity revoke`, neither of which may rewrite the
//! direct-peer list. They drop the verified entry into [`REVOCATIONS_DIR`]
//! (group-writable, like the enrollment nonces); every reader of the list
//! skips the devices revoked there at once, and a privileged pass
//! ([`prune_revoked_direct_peers`]) removes them from the list itself.
//! Dropping an entry can only shrink the list, and only for devices the
//! machine's own root revoked.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Device endpoint ids of the person's other devices this machine dials
/// directly, beside the identity root.
pub const DIRECT_PEERS_FILE: &str = "personal-device-peers.json";

/// Workspace rooms an enrolling device adopts from the device that enrolled
/// it, beside the identity root.
pub const ROOMS_FILE: &str = "personal-rooms.json";

/// Directory beside the identity root holding root-signed revocations, one
/// `<identity>-<device>.json` file each.
pub const REVOCATIONS_DIR: &str = "device-revocations";

/// Bound on entries [`read_revocations`] takes from [`REVOCATIONS_DIR`].
pub const MAX_REVOCATIONS: usize = 1024;

/// Bound on [`DIRECT_PEERS_FILE`] entries.
pub const MAX_DIRECT_PEERS: usize = 128;

/// Bound on [`ROOMS_FILE`] entries.
pub const MAX_ROOMS: usize = 256;

const MAX_LIST_BYTES: usize = 64 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_bounded(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() > MAX_LIST_BYTES => Err(invalid("device list exceeds 64 KiB")),
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_list(path: &Path, ids: &[String]) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(ids).map_err(io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tilo_files::native::atomic::write(path, &bytes)
}

fn decode_device(id: &str) -> Option<[u8; 32]> {
    if id.len() != 64 || id.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    hex::decode(id).ok()?.try_into().ok()
}

/// Parse a direct-peer list.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when it is not a bounded list of
/// lowercase hex device keys.
pub fn parse_direct_peers(bytes: &[u8]) -> io::Result<Vec<[u8; 32]>> {
    if bytes.len() > MAX_LIST_BYTES {
        return Err(invalid("device list exceeds 64 KiB"));
    }
    let ids: Vec<String> =
        serde_json::from_slice(bytes).map_err(|_| invalid("personal device list is invalid"))?;
    if ids.len() > MAX_DIRECT_PEERS {
        return Err(invalid("personal device list is full"));
    }
    ids.iter()
        .map(|id| decode_device(id).ok_or_else(|| invalid("personal device list is invalid")))
        .collect()
}

/// The devices recorded in [`DIRECT_PEERS_FILE`] at `path`; empty when the
/// file does not exist.
///
/// # Errors
///
/// When the file cannot be read or violates its bounded schema.
pub fn read_direct_peers(path: &Path) -> io::Result<Vec<[u8; 32]>> {
    read_bounded(path)?.map_or_else(|| Ok(Vec::new()), |bytes| parse_direct_peers(&bytes))
}

/// Add `devices` to the direct-peer list at `path`, keeping existing
/// entries. Returns whether the file changed.
///
/// # Errors
///
/// When the existing list is invalid, the result would exceed
/// [`MAX_DIRECT_PEERS`], or the file cannot be replaced.
pub fn remember_direct_peers(path: &Path, devices: &[[u8; 32]]) -> io::Result<bool> {
    let mut known = read_direct_peers(path)?;
    let before = known.len();
    for device in devices {
        if !known.contains(device) {
            known.push(*device);
        }
    }
    if known.len() == before && path.exists() {
        return Ok(false);
    }
    if known.len() > MAX_DIRECT_PEERS {
        return Err(invalid("personal device list is full"));
    }
    write_list(path, &known.iter().map(hex::encode).collect::<Vec<_>>())?;
    Ok(true)
}

/// Remove `device` from the direct-peer list at `path`. Returns whether the
/// file changed.
///
/// # Errors
///
/// When the existing list is invalid or the file cannot be replaced.
pub fn forget_direct_peer(path: &Path, device: [u8; 32]) -> io::Result<bool> {
    let mut known = read_direct_peers(path)?;
    let before = known.len();
    known.retain(|known| *known != device);
    if known.len() == before {
        return Ok(false);
    }
    write_list(path, &known.iter().map(hex::encode).collect::<Vec<_>>())?;
    Ok(true)
}

fn revocation_file(entry: &tilo_user_identity::RevocationEntry) -> String {
    format!(
        "{}-{}.json",
        hex::encode(entry.user_public_key),
        hex::encode(entry.revoked_device_public_key)
    )
}

/// Drop a root-signed revocation into [`REVOCATIONS_DIR`] under
/// `identity_root`. Returns whether a new file was written; an entry already
/// recorded for that identity and device is left alone.
///
/// # Errors
///
/// When the entry's signature does not verify or the file cannot be written.
pub fn record_revocation(
    identity_root: &Path,
    entry: &tilo_user_identity::RevocationEntry,
) -> io::Result<bool> {
    tilo_user_identity::verify_revocation_entry(entry)
        .map_err(|_| invalid("revocation signature does not verify"))?;
    let directory = identity_root.join(REVOCATIONS_DIR);
    let path = directory.join(revocation_file(entry));
    if path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(&directory)?;
    let bytes = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
    tilo_files::native::atomic::write(&path, &bytes)?;
    Ok(true)
}

/// The verified revocations recorded in [`REVOCATIONS_DIR`] under
/// `identity_root`. Files that are unreadable, oversized, misnamed or
/// unsigned are skipped: the directory is writable by the identity group, so
/// nothing in it is trusted before its root signature verifies.
///
/// # Errors
///
/// When the directory exists but cannot be listed.
pub fn read_revocations(identity_root: &Path) -> io::Result<tilo_user_identity::RevocationList> {
    let mut list = tilo_user_identity::RevocationList::new();
    let entries = match std::fs::read_dir(identity_root.join(REVOCATIONS_DIR)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(list),
        Err(error) => return Err(error),
    };
    for file in entries.flatten().take(MAX_REVOCATIONS) {
        let Ok(Some(bytes)) = read_bounded(&file.path()) else {
            continue;
        };
        let Ok(entry) = serde_json::from_slice::<tilo_user_identity::RevocationEntry>(&bytes)
        else {
            continue;
        };
        if file.file_name().to_str() != Some(revocation_file(&entry).as_str()) {
            continue;
        }
        let _ = list.insert(entry);
    }
    Ok(list)
}

/// The direct peers at `identity_root` that `user_public_key`'s root has not
/// revoked — what a session dials. Revocations count from
/// [`REVOCATIONS_DIR`] and from `known`, the ones the caller already holds.
///
/// # Errors
///
/// When the peer list cannot be read or violates its schema.
pub fn live_direct_peers(
    identity_root: &Path,
    user_public_key: &[u8; 32],
    known: &tilo_user_identity::RevocationList,
) -> io::Result<Vec<[u8; 32]>> {
    let recorded = read_revocations(identity_root).unwrap_or_default();
    Ok(read_direct_peers(&identity_root.join(DIRECT_PEERS_FILE))?
        .into_iter()
        .filter(|device| {
            recorded.revocation_of(user_public_key, device).is_none()
                && known.revocation_of(user_public_key, device).is_none()
        })
        .collect())
}

/// Remove from the direct-peer list at `identity_root` every device that
/// `user_public_key`'s root revoked in [`REVOCATIONS_DIR`]. Run with the
/// privilege to rewrite the list; `user_public_key` must come from this
/// machine's own certificate, never from the directory. Returns the devices
/// removed.
///
/// # Errors
///
/// When the list cannot be read or replaced.
pub fn prune_revoked_direct_peers(
    identity_root: &Path,
    user_public_key: &[u8; 32],
) -> io::Result<Vec<[u8; 32]>> {
    let revocations = read_revocations(identity_root)?;
    let path = identity_root.join(DIRECT_PEERS_FILE);
    let mut removed = Vec::new();
    for device in read_direct_peers(&path)? {
        if revocations
            .revocation_of(user_public_key, &device)
            .is_some()
            && forget_direct_peer(&path, device)?
        {
            removed.push(device);
        }
    }
    Ok(removed)
}

/// Whether `value` is a room id: a hyphenated UUID.
#[must_use]
pub fn valid_room_id(value: &str) -> bool {
    let mut parts = value.split('-');
    [8, 4, 4, 4, 12].into_iter().all(|len| {
        parts.next().is_some_and(|part| {
            part.len() == len && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }) && parts.next().is_none()
}

/// Parse a room list.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when it is not a bounded list of room ids.
pub fn parse_rooms(bytes: &[u8]) -> io::Result<Vec<String>> {
    if bytes.len() > MAX_LIST_BYTES {
        return Err(invalid("room list exceeds 64 KiB"));
    }
    let ids: Vec<String> =
        serde_json::from_slice(bytes).map_err(|_| invalid("personal room list is invalid"))?;
    if ids.len() > MAX_ROOMS || ids.iter().any(|id| !valid_room_id(id)) {
        return Err(invalid("personal room list is invalid"));
    }
    Ok(ids)
}

/// The rooms recorded at `path`; empty when the file does not exist.
///
/// # Errors
///
/// When the file cannot be read or violates its bounded schema.
pub fn read_rooms(path: &Path) -> io::Result<Vec<String>> {
    read_bounded(path)?.map_or_else(|| Ok(Vec::new()), |bytes| parse_rooms(&bytes))
}

/// Add `rooms` to the room list at `path`, keeping existing entries.
/// Returns whether the file changed.
///
/// # Errors
///
/// When a room id is invalid, the result would exceed [`MAX_ROOMS`], or the
/// file cannot be replaced.
pub fn remember_rooms(path: &Path, rooms: &[String]) -> io::Result<bool> {
    if rooms.iter().any(|room| !valid_room_id(room)) {
        return Err(invalid("personal room list is invalid"));
    }
    let mut known = read_rooms(path)?;
    let before = known.len();
    for room in rooms {
        if !known.contains(room) {
            known.push(room.clone());
        }
    }
    if known.len() == before && path.exists() {
        return Ok(false);
    }
    if known.len() > MAX_ROOMS {
        return Err(invalid("personal room list is full"));
    }
    write_list(path, &known)?;
    Ok(true)
}

/// Where a session publishes the rooms a newly enrolled device should join:
/// `$XDG_STATE_HOME/tilo/personal-rooms.json`, else under `~/.local/state`.
/// Absent when the process has neither variable.
#[must_use]
pub fn rooms_export_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("tilo").join(ROOMS_FILE))
}

/// Presence snapshot file name inside the session's runtime directory.
pub const PRESENCE_FILE: &str = "personal-devices.json";

/// Where a session running with `runtime_dir` as its `XDG_RUNTIME_DIR`
/// publishes its presence snapshot.
#[must_use]
pub fn presence_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("tilo-session").join(PRESENCE_FILE)
}

/// What a running session currently sees of the person's other devices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceSnapshot {
    /// Schema version; readers accept only [`Self::CURRENT_SCHEMA`].
    pub schema_version: u16,
    /// This session's device key, lowercase hex.
    pub local_device: String,
    /// The identity the session runs as, lowercase hex; absent while it
    /// runs without one.
    pub identity: Option<String>,
    /// Wall-clock milliseconds when the session last refreshed this file. A
    /// stale value means the session stopped.
    pub refreshed_unix_ms: u64,
    /// One entry per device the session has heard from or dials.
    pub devices: Vec<DevicePresence>,
}

impl PresenceSnapshot {
    /// Schema version written by this build.
    pub const CURRENT_SCHEMA: u16 = 1;

    /// Bound on [`Self::devices`].
    pub const MAX_DEVICES: usize = 256;
}

/// One device's presence as the local session sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevicePresence {
    /// Device key, lowercase hex.
    pub device: String,
    /// Whether an authenticated connection to the device is open now.
    pub online: bool,
    /// Wall-clock milliseconds of the device's last authenticated heartbeat,
    /// when one has arrived.
    pub last_seen_unix_ms: Option<u64>,
    /// Whether the device is recorded in [`DIRECT_PEERS_FILE`].
    pub direct_peer: bool,
    /// Rooms in which the device is connected now.
    pub shared_rooms: u32,
}

/// Read a presence snapshot; `None` when no session has written one.
///
/// # Errors
///
/// When the file cannot be read or violates its bounded schema.
pub fn read_presence(path: &Path) -> io::Result<Option<PresenceSnapshot>> {
    let Some(bytes) = read_bounded(path)? else {
        return Ok(None);
    };
    let snapshot: PresenceSnapshot =
        serde_json::from_slice(&bytes).map_err(|_| invalid("presence snapshot is invalid"))?;
    if snapshot.schema_version != PresenceSnapshot::CURRENT_SCHEMA
        || snapshot.devices.len() > PresenceSnapshot::MAX_DEVICES
    {
        return Err(invalid("presence snapshot is invalid"));
    }
    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_merge_without_losing_entries_and_refuse_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let peers = dir.path().join(DIRECT_PEERS_FILE);
        assert!(read_direct_peers(&peers).unwrap().is_empty());
        assert!(remember_direct_peers(&peers, &[[1; 32]]).unwrap());
        assert!(remember_direct_peers(&peers, &[[2; 32], [1; 32]]).unwrap());
        assert!(!remember_direct_peers(&peers, &[[2; 32]]).unwrap());
        assert_eq!(read_direct_peers(&peers).unwrap(), vec![[1; 32], [2; 32]]);
        assert!(forget_direct_peer(&peers, [1; 32]).unwrap());
        assert_eq!(read_direct_peers(&peers).unwrap(), vec![[2; 32]]);
        std::fs::write(&peers, br#"["ABCD"]"#).unwrap();
        assert!(remember_direct_peers(&peers, &[[3; 32]]).is_err());

        let rooms = dir.path().join(ROOMS_FILE);
        let room = "0f8fad5b-d9cb-469f-a165-70867728950e".to_string();
        assert!(remember_rooms(&rooms, std::slice::from_ref(&room)).unwrap());
        assert!(!remember_rooms(&rooms, std::slice::from_ref(&room)).unwrap());
        assert_eq!(read_rooms(&rooms).unwrap(), vec![room]);
        assert!(remember_rooms(&rooms, &["../etc".into()]).is_err());
        assert!(parse_rooms(br#"["not-a-room"]"#).is_err());
    }

    fn revocation(
        root: &ed25519_dalek::SigningKey,
        device: [u8; 32],
    ) -> tilo_user_identity::RevocationEntry {
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: 5,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    #[test]
    fn recorded_revocations_prune_only_the_owners_revoked_devices() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let user = root.verifying_key().to_bytes();
        let peers = dir.path().join(DIRECT_PEERS_FILE);
        remember_direct_peers(&peers, &[[1; 32], [2; 32], [3; 32]]).unwrap();

        assert!(record_revocation(dir.path(), &revocation(&root, [1; 32])).unwrap());
        assert!(!record_revocation(dir.path(), &revocation(&root, [1; 32])).unwrap());
        // Another identity's revocation of a peer of ours does not count.
        assert!(record_revocation(dir.path(), &revocation(&stranger, [2; 32])).unwrap());
        // Unsigned entries are refused, and planted files are skipped.
        let mut forged = revocation(&root, [3; 32]);
        forged.signature[0] ^= 1;
        assert!(record_revocation(dir.path(), &forged).is_err());
        std::fs::write(
            dir.path()
                .join(REVOCATIONS_DIR)
                .join(revocation_file(&forged)),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        let renamed = revocation(&root, [3; 32]);
        std::fs::write(
            dir.path().join(REVOCATIONS_DIR).join("other.json"),
            serde_json::to_vec(&renamed).unwrap(),
        )
        .unwrap();

        let none = tilo_user_identity::RevocationList::new();
        assert_eq!(
            live_direct_peers(dir.path(), &user, &none).unwrap(),
            vec![[2; 32], [3; 32]]
        );
        let mut held = tilo_user_identity::RevocationList::new();
        held.insert(revocation(&root, [3; 32])).unwrap();
        assert_eq!(
            live_direct_peers(dir.path(), &user, &held).unwrap(),
            vec![[2; 32]]
        );
        assert_eq!(
            prune_revoked_direct_peers(dir.path(), &user).unwrap(),
            vec![[1; 32]]
        );
        assert_eq!(read_direct_peers(&peers).unwrap(), vec![[2; 32], [3; 32]]);
        assert!(
            prune_revoked_direct_peers(dir.path(), &user)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn presence_round_trips_and_rejects_other_schemas() {
        let dir = tempfile::tempdir().unwrap();
        let path = presence_path(dir.path());
        assert_eq!(read_presence(&path).unwrap(), None);
        let snapshot = PresenceSnapshot {
            schema_version: PresenceSnapshot::CURRENT_SCHEMA,
            local_device: "11".repeat(32),
            identity: Some("33".repeat(32)),
            refreshed_unix_ms: 5,
            devices: vec![DevicePresence {
                device: "22".repeat(32),
                online: true,
                last_seen_unix_ms: Some(4),
                direct_peer: true,
                shared_rooms: 1,
            }],
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert_eq!(read_presence(&path).unwrap(), Some(snapshot.clone()));
        let other = PresenceSnapshot {
            schema_version: 2,
            ..snapshot
        };
        std::fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
        assert!(read_presence(&path).is_err());
    }
}
