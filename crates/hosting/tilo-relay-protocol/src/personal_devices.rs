//! The person's other devices as one machine records them: the devices it
//! dials directly, the workspace rooms they share, and what its running
//! session last saw of each of them.
//!
//! Enrollment writes the first two in the identity's peers directory below
//! the identity root (normally `/var/lib/tilo/peers`, beside the home-peer
//! registry; see [`peers_dir`]). The person's own account writes there while
//! joining or approving, and the session reads it on every refresh, so a new
//! entry takes effect without a restart. The presence snapshot is the
//! session's read-only projection of its live connections, for status tools
//! that run outside the session.
//!
//! Each direct peer is recorded with the identity whose certificate the
//! enrollment checked when it wrote the entry, so an entry speaks for one
//! person's device and nothing else.
//!
//! Revocations the person's root signed reach this machine through its
//! session or `tilo-identity revoke`. Either records the verified entry in
//! [`REVOCATIONS_DIR`] (sticky, so no account removes another's entry) and
//! prunes the revoked device from the direct-peer list
//! ([`prune_revoked_direct_peers`]); every reader also skips devices revoked
//! there. A revocation removes only entries recorded under the identity that
//! signed it. Nothing in the directory is trusted before its root signature
//! verifies, and a hostile file there (a symlink, a FIFO, an oversized or
//! misnamed one) is skipped without blocking the reader.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The person's other devices this machine dials directly, each with the
/// identity it belongs to, in [`peers_dir`].
pub const DIRECT_PEERS_FILE: &str = "personal-device-peers.json";

/// Workspace rooms an enrolling device adopts from the device that enrolled
/// it, in [`peers_dir`].
pub const ROOMS_FILE: &str = "personal-rooms.json";

/// Directory in [`peers_dir`] holding root-signed revocations, one
/// `<identity>-<device>.json` file each (or `<identity>-<device>.<suffix>.json`
/// when another file already holds that name).
pub const REVOCATIONS_DIR: &str = "device-revocations";

/// The identity's peers directory below `identity_root`: the home-peer
/// registry, the direct peers, the shared rooms and recorded revocations.
#[must_use]
pub fn peers_dir(identity_root: &Path) -> PathBuf {
    tilo_user_identity::home_peer_registry_path(identity_root)
        .parent()
        .map_or_else(|| identity_root.join("peers"), Path::to_path_buf)
}

/// Where the direct-peer list of `identity_root` lives.
#[must_use]
pub fn direct_peers_path(identity_root: &Path) -> PathBuf {
    peers_dir(identity_root).join(DIRECT_PEERS_FILE)
}

/// Where the shared-room list of `identity_root` lives.
#[must_use]
pub fn rooms_path(identity_root: &Path) -> PathBuf {
    peers_dir(identity_root).join(ROOMS_FILE)
}

/// Bound on valid revocations [`read_revocations`] takes from
/// [`REVOCATIONS_DIR`].
pub const MAX_REVOCATIONS: usize = 1024;

/// Bound on directory entries [`read_revocations`] looks at, valid or not.
pub const MAX_REVOCATION_DIR_ENTRIES: usize = 4096;

/// Bound on one revocation file.
const MAX_REVOCATION_FILE_BYTES: u64 = 4 * 1024;

/// How long a cached reading of [`REVOCATIONS_DIR`] is reused while the
/// directory itself is unchanged.
const REVOCATION_CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);

/// Bound on [`DIRECT_PEERS_FILE`] entries.
pub const MAX_DIRECT_PEERS: usize = 128;

/// Bound on [`ROOMS_FILE`] entries.
pub const MAX_ROOMS: usize = 256;

const MAX_LIST_BYTES: usize = 64 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Read one list file. The peers directory is writable by the identity
/// group, so the file is opened without following a symlink or blocking on a
/// FIFO, must be a regular file, and is read only up to the bound.
fn read_bounded(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(invalid("device list is not a regular file"));
    }
    let limit = u64::try_from(MAX_LIST_BYTES).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_LIST_BYTES {
        return Err(invalid("device list exceeds 64 KiB"));
    }
    Ok(Some(bytes))
}

/// The bytes of a list file in the peers directory, read as [`read_rooms`]
/// and [`read_direct_peers`] read it (no symlink, no FIFO, bounded); `None`
/// when it does not exist.
///
/// # Errors
///
/// When the file cannot be opened, is not a regular file, or exceeds 64 KiB.
pub fn read_list_file(path: &Path) -> io::Result<Option<Vec<u8>>> {
    read_bounded(path)
}

/// Hold an exclusive lock on `path`'s lock file while a list is read,
/// changed and replaced, so a session pruning and an enrollment recording
/// at once never lose each other's change.
fn lock_list(path: &Path) -> io::Result<advisory_lock::FileLock> {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o660)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(std::path::PathBuf::from(lock))?;
    if !file.metadata()?.file_type().is_file() {
        return Err(invalid("device list lock is not a regular file"));
    }
    advisory_lock::FileLock::exclusive(file)
}

fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tilo_files::native::atomic::write(path, &bytes)
}

fn decode_key(id: &str) -> Option<[u8; 32]> {
    if id.len() != 64 || id.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    hex::decode(id).ok()?.try_into().ok()
}

/// One device this machine dials directly, and the identity whose
/// certificate for it the enrollment verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DirectPeer {
    /// The person's identity root key.
    pub identity: [u8; 32],
    /// The device's key, which is also its endpoint id.
    pub device: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectPeerJson {
    identity: String,
    device: String,
}

impl From<&DirectPeer> for DirectPeerJson {
    fn from(peer: &DirectPeer) -> Self {
        Self {
            identity: hex::encode(peer.identity),
            device: hex::encode(peer.device),
        }
    }
}

/// Encode a direct-peer list as [`DIRECT_PEERS_FILE`] holds it.
///
/// # Errors
///
/// When the list exceeds [`MAX_DIRECT_PEERS`].
pub fn encode_direct_peers(peers: &[DirectPeer]) -> io::Result<Vec<u8>> {
    if peers.len() > MAX_DIRECT_PEERS {
        return Err(invalid("personal device list is full"));
    }
    serde_json::to_vec_pretty(&peers.iter().map(DirectPeerJson::from).collect::<Vec<_>>())
        .map_err(io::Error::other)
}

/// Parse a direct-peer list.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when it is not a bounded list of
/// lowercase hex identity and device keys.
pub fn parse_direct_peers(bytes: &[u8]) -> io::Result<Vec<DirectPeer>> {
    if bytes.len() > MAX_LIST_BYTES {
        return Err(invalid("device list exceeds 64 KiB"));
    }
    let entries: Vec<DirectPeerJson> =
        serde_json::from_slice(bytes).map_err(|_| invalid("personal device list is invalid"))?;
    if entries.len() > MAX_DIRECT_PEERS {
        return Err(invalid("personal device list is full"));
    }
    entries
        .iter()
        .map(|entry| {
            decode_key(&entry.identity)
                .zip(decode_key(&entry.device))
                .map(|(identity, device)| DirectPeer { identity, device })
                .ok_or_else(|| invalid("personal device list is invalid"))
        })
        .collect()
}

/// The direct peers recorded in [`DIRECT_PEERS_FILE`] at `path`; empty when
/// the file does not exist.
///
/// # Errors
///
/// When the file cannot be read or violates its bounded schema.
pub fn read_direct_peers(path: &Path) -> io::Result<Vec<DirectPeer>> {
    read_bounded(path)?.map_or_else(|| Ok(Vec::new()), |bytes| parse_direct_peers(&bytes))
}

/// Add `peers` to the direct-peer list at `path`, keeping existing entries.
/// Returns whether the file changed.
///
/// # Errors
///
/// When the existing list is invalid, the result would exceed
/// [`MAX_DIRECT_PEERS`], or the file cannot be replaced.
pub fn remember_direct_peers(path: &Path, peers: &[DirectPeer]) -> io::Result<bool> {
    let _lock = lock_list(path)?;
    let mut known = read_direct_peers(path)?;
    let before = known.len();
    for peer in peers {
        if !known.contains(peer) {
            known.push(*peer);
        }
    }
    if known.len() == before && path.exists() {
        return Ok(false);
    }
    if known.len() > MAX_DIRECT_PEERS {
        return Err(invalid("personal device list is full"));
    }
    write_json(
        path,
        &known.iter().map(DirectPeerJson::from).collect::<Vec<_>>(),
    )?;
    Ok(true)
}

/// Remove `peer` from the direct-peer list at `path`. Returns whether the
/// file changed.
///
/// # Errors
///
/// When the existing list is invalid or the file cannot be replaced.
pub fn forget_direct_peer(path: &Path, peer: DirectPeer) -> io::Result<bool> {
    let _lock = lock_list(path)?;
    let mut known = read_direct_peers(path)?;
    let before = known.len();
    known.retain(|known| *known != peer);
    if known.len() == before {
        return Ok(false);
    }
    write_json(
        path,
        &known.iter().map(DirectPeerJson::from).collect::<Vec<_>>(),
    )?;
    Ok(true)
}

fn revocation_stem(entry: &tilo_user_identity::RevocationEntry) -> String {
    format!(
        "{}-{}",
        hex::encode(entry.user_public_key),
        hex::encode(entry.revoked_device_public_key)
    )
}

/// Whether `name` is a file name [`record_revocation`] could have given
/// `entry`: the canonical name, or the canonical stem with a short hex suffix.
fn names_revocation(name: &str, entry: &tilo_user_identity::RevocationEntry) -> bool {
    let stem = revocation_stem(entry);
    let Some(rest) = name.strip_prefix(stem.as_str()) else {
        return false;
    };
    if rest == ".json" {
        return true;
    }
    rest.strip_prefix('.')
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(|suffix| {
            (1..=32).contains(&suffix.len())
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}

/// Read one entry of the shared directory without following a symlink or
/// blocking on a FIFO: only a regular file within the bound is read.
fn read_revocation_file(path: &Path) -> Option<tilo_user_identity::RevocationEntry> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_REVOCATION_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_REVOCATION_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_REVOCATION_FILE_BYTES {
        return None;
    }
    let entry: tilo_user_identity::RevocationEntry = serde_json::from_slice(&bytes).ok()?;
    tilo_user_identity::verify_revocation_entry(&entry).ok()?;
    Some(entry)
}

fn suffix_for(entry: &tilo_user_identity::RevocationEntry, attempt: u32) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&entry.signature);
    hasher.update(&std::process::id().to_le_bytes());
    hasher.update(&attempt.to_le_bytes());
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.update(&now.as_nanos().to_le_bytes());
    }
    hex::encode(&hasher.finalize().as_bytes()[..8])
}

/// Drop a root-signed revocation into [`REVOCATIONS_DIR`] in the
/// peers directory `peers` (see [`peers_dir`]). Returns whether a new file was written; when a file
/// there already holds this exact verified entry it is left alone. The
/// directory is shared, so a name taken by anything else is not trusted as
/// "recorded": the entry goes under a suffixed name instead.
///
/// # Errors
///
/// When the entry's signature does not verify or no file can be written.
pub fn record_revocation(
    peers: &Path,
    entry: &tilo_user_identity::RevocationEntry,
) -> io::Result<bool> {
    tilo_user_identity::verify_revocation_entry(entry)
        .map_err(|_| invalid("revocation signature does not verify"))?;
    let directory = peers.join(REVOCATIONS_DIR);
    std::fs::create_dir_all(&directory)?;
    let stem = revocation_stem(entry);
    if let Ok(names) = std::fs::read_dir(&directory) {
        for name in names.flatten().take(MAX_REVOCATION_DIR_ENTRIES) {
            let name = name.file_name();
            if name
                .to_str()
                .is_some_and(|name| names_revocation(name, entry))
                && read_revocation_file(&directory.join(&name)).as_ref() == Some(entry)
            {
                return Ok(false);
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
    let canonical = directory.join(format!("{stem}.json"));
    if std::fs::symlink_metadata(&canonical).is_err()
        && tilo_files::native::atomic::write(&canonical, &bytes).is_ok()
    {
        return Ok(true);
    }
    let mut last_error = io::Error::other("no free revocation file name");
    for attempt in 0..8 {
        let path = directory.join(format!("{stem}.{}.json", suffix_for(entry, attempt)));
        if std::fs::symlink_metadata(&path).is_ok() {
            continue;
        }
        match tilo_files::native::atomic::write(&path, &bytes) {
            Ok(()) => return Ok(true),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

#[derive(Clone, PartialEq, Eq)]
struct DirectoryStamp {
    device: u64,
    inode: u64,
    modified: Option<std::time::SystemTime>,
    changed: (i64, i64),
}

fn directory_stamp(metadata: &std::fs::Metadata) -> DirectoryStamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        DirectoryStamp {
            device: metadata.dev(),
            inode: metadata.ino(),
            modified: metadata.modified().ok(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    #[cfg(not(unix))]
    {
        DirectoryStamp {
            device: 0,
            inode: 0,
            modified: metadata.modified().ok(),
            changed: (0, 0),
        }
    }
}

struct CachedRevocations {
    stamp: DirectoryStamp,
    read_at: std::time::Instant,
    list: tilo_user_identity::RevocationList,
}

type RevocationCache = std::sync::Mutex<std::collections::HashMap<PathBuf, CachedRevocations>>;

fn revocation_cache() -> &'static RevocationCache {
    static CACHE: std::sync::OnceLock<RevocationCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn scan_revocations(directory: &Path) -> io::Result<tilo_user_identity::RevocationList> {
    let mut list = tilo_user_identity::RevocationList::new();
    let mut valid = 0usize;
    for file in std::fs::read_dir(directory)?
        .flatten()
        .take(MAX_REVOCATION_DIR_ENTRIES)
    {
        if valid >= MAX_REVOCATIONS {
            break;
        }
        let name = file.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Recorded names are exactly lowercase `.json`; skip anything else
        // before opening it.
        if !name.as_bytes().ends_with(b".json") {
            continue;
        }
        let Some(entry) = read_revocation_file(&file.path()) else {
            continue;
        };
        if !names_revocation(name, &entry) {
            continue;
        }
        if list.insert(entry).is_ok() {
            valid = valid.saturating_add(1);
        }
    }
    Ok(list)
}

/// The verified revocations recorded in [`REVOCATIONS_DIR`] in the
/// peers directory `peers` (see [`peers_dir`]). Files that are not regular, oversized, misnamed or
/// unsigned are skipped and do not count toward [`MAX_REVOCATIONS`]. A
/// reading is reused while the directory is unchanged (no entry added,
/// renamed or removed), so frequent callers verify nothing twice.
///
/// # Errors
///
/// When the directory exists but cannot be listed.
pub fn read_revocations(peers: &Path) -> io::Result<tilo_user_identity::RevocationList> {
    let directory = peers.join(REVOCATIONS_DIR);
    let metadata = match std::fs::metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(tilo_user_identity::RevocationList::new());
        }
        Err(error) => return Err(error),
    };
    let stamp = directory_stamp(&metadata);
    if let Ok(cache) = revocation_cache().lock()
        && let Some(cached) = cache.get(&directory)
        && cached.stamp == stamp
        && cached.read_at.elapsed() < REVOCATION_CACHE_MAX_AGE
    {
        return Ok(cached.list.clone());
    }
    let list = scan_revocations(&directory)?;
    if let Ok(mut cache) = revocation_cache().lock() {
        cache.insert(
            directory,
            CachedRevocations {
                stamp,
                read_at: std::time::Instant::now(),
                list: list.clone(),
            },
        );
    }
    Ok(list)
}

/// The devices of `user_public_key` recorded in the peers directory `peers` as direct
/// peers that its root has not revoked — what a session dials. Revocations
/// count from [`REVOCATIONS_DIR`] and from `known`, the ones the caller
/// already holds.
///
/// # Errors
///
/// When the peer list cannot be read or violates its schema.
pub fn live_direct_peers(
    peers: &Path,
    user_public_key: &[u8; 32],
    known: &tilo_user_identity::RevocationList,
) -> io::Result<Vec<[u8; 32]>> {
    let recorded = read_revocations(peers).unwrap_or_default();
    Ok(read_direct_peers(&peers.join(DIRECT_PEERS_FILE))?
        .into_iter()
        .filter(|peer| &peer.identity == user_public_key)
        .map(|peer| peer.device)
        .filter(|device| {
            recorded.revocation_of(user_public_key, device).is_none()
                && known.revocation_of(user_public_key, device).is_none()
        })
        .collect())
}

/// Remove from the direct-peer list in the peers directory `peers` every entry whose
/// device was revoked, in [`REVOCATIONS_DIR`], by the identity the entry was
/// recorded under. Returns the entries removed.
///
/// # Errors
///
/// When the list cannot be read or replaced.
pub fn prune_revoked_direct_peers(peers: &Path) -> io::Result<Vec<DirectPeer>> {
    let revocations = read_revocations(peers)?;
    let path = peers.join(DIRECT_PEERS_FILE);
    let mut removed = Vec::new();
    for peer in read_direct_peers(&path)? {
        if revocations
            .revocation_of(&peer.identity, &peer.device)
            .is_some()
            && forget_direct_peer(&path, peer)?
        {
            removed.push(peer);
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
    let _lock = lock_list(path)?;
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
    write_json(path, &known)?;
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

    fn peer(identity: u8, device: u8) -> DirectPeer {
        DirectPeer {
            identity: [identity; 32],
            device: [device; 32],
        }
    }

    #[test]
    fn lists_merge_without_losing_entries_and_refuse_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let peers = direct_peers_path(dir.path());
        assert!(read_direct_peers(&peers).unwrap().is_empty());
        assert!(remember_direct_peers(&peers, &[peer(9, 1)]).unwrap());
        assert!(remember_direct_peers(&peers, &[peer(9, 2), peer(9, 1)]).unwrap());
        assert!(!remember_direct_peers(&peers, &[peer(9, 2)]).unwrap());
        // The same device under another identity is another entry.
        assert!(remember_direct_peers(&peers, &[peer(8, 2)]).unwrap());
        assert_eq!(
            read_direct_peers(&peers).unwrap(),
            vec![peer(9, 1), peer(9, 2), peer(8, 2)]
        );
        assert!(forget_direct_peer(&peers, peer(9, 1)).unwrap());
        assert!(!forget_direct_peer(&peers, peer(7, 2)).unwrap());
        assert_eq!(
            read_direct_peers(&peers).unwrap(),
            vec![peer(9, 2), peer(8, 2)]
        );
        assert_eq!(
            parse_direct_peers(&encode_direct_peers(&[peer(9, 2)]).unwrap()).unwrap(),
            vec![peer(9, 2)]
        );
        std::fs::write(&peers, br#"[{"identity":"ABCD","device":"00"}]"#).unwrap();
        assert!(remember_direct_peers(&peers, &[peer(9, 3)]).is_err());
        std::fs::write(&peers, format!(r#"["{}"]"#, "11".repeat(32))).unwrap();
        assert!(read_direct_peers(&peers).is_err());

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
    fn a_revocation_prunes_only_entries_recorded_under_its_signer() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let user = root.verifying_key().to_bytes();
        let ours = |device: u8| DirectPeer {
            identity: user,
            device: [device; 32],
        };
        let peers = dir.path().join(DIRECT_PEERS_FILE);
        remember_direct_peers(&peers, &[ours(1), ours(2), ours(3)]).unwrap();

        assert!(record_revocation(dir.path(), &revocation(&root, [1; 32])).unwrap());
        assert!(!record_revocation(dir.path(), &revocation(&root, [1; 32])).unwrap());
        // Another identity revoking a device of ours prunes nothing: the
        // entry was recorded under our identity, not the stranger's.
        assert!(record_revocation(dir.path(), &revocation(&stranger, [2; 32])).unwrap());
        // Unsigned entries are refused, and planted files are skipped.
        let mut forged = revocation(&root, [3; 32]);
        forged.signature[0] ^= 1;
        assert!(record_revocation(dir.path(), &forged).is_err());
        std::fs::write(
            dir.path()
                .join(REVOCATIONS_DIR)
                .join(format!("{}.json", revocation_stem(&forged))),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join(REVOCATIONS_DIR).join("other.json"),
            serde_json::to_vec(&revocation(&root, [3; 32])).unwrap(),
        )
        .unwrap();

        let none = tilo_user_identity::RevocationList::new();
        assert_eq!(
            live_direct_peers(dir.path(), &user, &none).unwrap(),
            vec![[2; 32], [3; 32]]
        );
        // Only this person's entries are their direct peers.
        assert!(
            live_direct_peers(dir.path(), &stranger.verifying_key().to_bytes(), &none)
                .unwrap()
                .is_empty()
        );
        let mut held = tilo_user_identity::RevocationList::new();
        held.insert(revocation(&root, [3; 32])).unwrap();
        assert_eq!(
            live_direct_peers(dir.path(), &user, &held).unwrap(),
            vec![[2; 32]]
        );
        assert_eq!(
            prune_revoked_direct_peers(dir.path()).unwrap(),
            vec![ours(1)]
        );
        assert_eq!(read_direct_peers(&peers).unwrap(), vec![ours(2), ours(3)]);
        assert!(prune_revoked_direct_peers(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_name_taken_by_something_else_does_not_hide_a_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let user = root.verifying_key().to_bytes();
        let entry = revocation(&root, [1; 32]);
        let directory = dir.path().join(REVOCATIONS_DIR);
        std::fs::create_dir_all(&directory).unwrap();
        // Garbage planted at the name the revocation would take.
        let canonical = directory.join(format!("{}.json", revocation_stem(&entry)));
        std::fs::write(&canonical, b"not a revocation").unwrap();
        assert!(record_revocation(dir.path(), &entry).unwrap());
        assert_eq!(std::fs::read(&canonical).unwrap(), b"not a revocation");
        assert!(
            read_revocations(dir.path())
                .unwrap()
                .revocation_of(&user, &[1; 32])
                .is_some()
        );
        // Recorded once, the entry is found again under its suffixed name.
        assert!(!record_revocation(dir.path(), &entry).unwrap());
        assert!(names_revocation(
            &format!("{}.0a1b.json", revocation_stem(&entry)),
            &entry
        ));
        assert!(!names_revocation(
            &format!("{}.../x.json", revocation_stem(&entry)),
            &entry
        ));
        assert!(!names_revocation(
            &format!("{}.0A.json", revocation_stem(&entry)),
            &entry
        ));
    }

    #[cfg(unix)]
    #[test]
    fn hostile_files_in_the_shared_directory_never_block_or_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let user = root.verifying_key().to_bytes();
        let directory = dir.path().join(REVOCATIONS_DIR);
        std::fs::create_dir_all(&directory).unwrap();
        let real = revocation(&root, [1; 32]);
        let stem = revocation_stem(&real);
        // A FIFO under the very name a reader would open: a blocking read
        // would hang here forever.
        let fifo =
            std::ffi::CString::new(directory.join(format!("{stem}.0f.json")).to_str().unwrap())
                .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        // A symlink to a valid revocation elsewhere is not followed.
        let outside = dir.path().join("outside.json");
        std::fs::write(&outside, serde_json::to_vec(&real).unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, directory.join(format!("{stem}.json"))).unwrap();
        // An oversized file is not read.
        std::fs::write(
            directory.join(format!("{stem}.1f.json")),
            vec![b' '; 64 * 1024],
        )
        .unwrap();
        // Junk does not use up the bound on valid entries.
        for index in 0..(MAX_REVOCATIONS + 10) {
            std::fs::write(directory.join(format!("junk-{index}.json")), b"{}").unwrap();
        }
        assert!(
            read_revocations(dir.path())
                .unwrap()
                .revocation_of(&user, &[1; 32])
                .is_none()
        );
        assert!(record_revocation(dir.path(), &real).unwrap());
        assert!(
            read_revocations(dir.path())
                .unwrap()
                .revocation_of(&user, &[1; 32])
                .is_some()
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
