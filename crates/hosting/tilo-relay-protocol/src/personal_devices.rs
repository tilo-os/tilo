//! The person's other devices as one machine records them: the devices it
//! dials directly, the workspace rooms they share, and what its running
//! session last saw of each of them.
//!
//! A device may have several people, each with their own account. The
//! identity's peers directory below the identity root (normally
//! `/var/lib/tilo/peers`, beside the home-peer registry; see [`peers_dir`])
//! is shared by the identity group and sticky, so nobody renames or removes
//! another account's entries there. Each person keeps their lists in their
//! own [`PersonPeers`] directory inside it, named after their identity and
//! owned by their account (mode `2750`): only that account (or root, acting
//! for it) changes the direct peers, the shared rooms and the recorded
//! revocations, while the identity group, a relay on the machine included,
//! may read them. Every reader refuses a person directory, or a file in one,
//! owned by anyone but the expected account or root, or writable by anyone
//! else, so another person's directory never stands in for one's own.
//!
//! The name `<identity>` is predictable, so another account may already hold
//! it; a person then keeps their directory at `<identity>.<suffix>` instead,
//! and readers pick the one their account owns.
//!
//! Enrollment writes the first two lists. The person's own account writes
//! them while joining or approving, and the session reads them on every
//! refresh, so a new entry takes effect without a restart. The presence
//! snapshot is the session's read-only projection of its live connections,
//! for status tools that run outside the session.
//!
//! Each direct peer is recorded with the identity whose certificate the
//! enrollment checked when it wrote the entry; a person's directory holds
//! only entries of its own identity.
//!
//! Revocations the person's root signed reach this machine through its
//! session or `tilo-identity revoke`. Either records the verified entry in
//! the person's [`REVOCATIONS_DIR`] and prunes the revoked device from their
//! direct-peer list ([`PersonPeers::prune_revoked_direct_peers`]); every
//! reader also skips devices revoked there. Nothing there is trusted before
//! its root signature verifies, and a hostile file (a symlink, a FIFO, an
//! oversized, misnamed or foreign-owned one) is skipped without blocking the
//! reader.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The person's other devices this machine dials directly, each with the
/// identity it belongs to, in their [`PersonPeers`] directory.
pub const DIRECT_PEERS_FILE: &str = "personal-device-peers.json";

/// Workspace rooms an enrolling device adopts from the device that enrolled
/// it, in the person's [`PersonPeers`] directory.
pub const ROOMS_FILE: &str = "personal-rooms.json";

/// Directory in a person's [`PersonPeers`] directory holding the
/// revocations their root signed, one `<identity>-<device>.json` file each.
pub const REVOCATIONS_DIR: &str = "device-revocations";

/// The identity's peers directory below `identity_root`: the home-peer
/// registry and one [`PersonPeers`] directory per person.
#[must_use]
pub fn peers_dir(identity_root: &Path) -> PathBuf {
    tilo_user_identity::home_peer_registry_path(identity_root)
        .parent()
        .map_or_else(|| identity_root.join("peers"), Path::to_path_buf)
}

/// Bound on valid revocations read from one person's [`REVOCATIONS_DIR`].
pub const MAX_REVOCATIONS: usize = 1024;

/// Bound on directory entries a revocation read looks at, valid or not.
pub const MAX_REVOCATION_DIR_ENTRIES: usize = 4096;

/// Bound on accounts whose directory of one identity a relay reads
/// revocations from (one directory per account).
const MAX_PERSON_DIRS_PER_IDENTITY: usize = 1024;

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

/// Mode of a person directory and of their revocation directory: the owner
/// writes, the identity group (set-group-ID keeps it) reads.
pub const PERSON_DIR_MODE: u32 = 0o2750;

/// Mode of every file in a person directory.
pub const PERSON_FILE_MODE: u32 = 0o640;

/// Length of the hex suffix of a person directory named `<identity>.<suffix>`.
const PERSON_DIR_SUFFIX_LEN: usize = 16;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// The effective user id of this process.
#[must_use]
pub fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Whether `metadata` may hold data of the account `owner`: owned by it or
/// by root, and writable by nobody else.
fn trusted_by(metadata: &std::fs::Metadata, owner: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (metadata.uid() == owner || metadata.uid() == 0) && metadata.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = (metadata, owner);
        true
    }
}

/// Whether `path` is a real directory (not a symlink) trusted by `owner`.
fn trusted_dir(path: &Path, owner: u32) -> io::Result<bool> {
    Ok(private_dir_owner(path)?.is_some_and(|uid| uid == owner || uid == 0))
}

/// The account owning `path` when it is a real directory (not a symlink)
/// that nobody else may write.
fn private_dir_owner(path: &Path) -> io::Result<Option<u32>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_dir() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok((metadata.mode() & 0o022 == 0).then(|| metadata.uid()))
    }
    #[cfg(not(unix))]
    {
        Ok(Some(0))
    }
}

/// Hand `path`, which this process just created as root on behalf of
/// `owner`, to that account. Nothing to do when the process already runs as
/// `owner`.
fn give_to(path: &Path, owner: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        if current_uid() != 0 || owner == 0 {
            return Ok(());
        }
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| invalid("path contains a NUL byte"))?;
        // SAFETY: `path` is NUL-terminated; a gid of -1 leaves the group.
        if unsafe { libc::lchown(path.as_ptr(), owner, libc::gid_t::MAX) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(unix))]
    let _ = (path, owner);
    Ok(())
}

/// Create `path` as a person directory of `owner`. Fails when the name is
/// taken.
fn create_person_dir(path: &Path, owner: u32) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        builder.mode(PERSON_DIR_MODE & 0o777);
        builder.create(path)?;
        // The umask may have narrowed the mode, and `mkdir` ignores the
        // set-group-ID bit.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PERSON_DIR_MODE))?;
    }
    #[cfg(not(unix))]
    builder.create(path)?;
    give_to(path, owner)
}

/// Read one file of a person directory: opened without following a symlink
/// or blocking on a FIFO, a regular file trusted by `owner` (when given), and
/// read only up to the bound.
fn read_bounded(path: &Path, owner: Option<u32>) -> io::Result<Option<Vec<u8>>> {
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
    if owner.is_some_and(|owner| !trusted_by(&metadata, owner)) {
        return Err(refused("device list belongs to another account"));
    }
    let limit = u64::try_from(MAX_LIST_BYTES).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_LIST_BYTES {
        return Err(invalid("device list exceeds 64 KiB"));
    }
    Ok(Some(bytes))
}

/// The bytes of a list file outside a person directory, such as a session's
/// room export, read the way person lists are read (no symlink, no FIFO,
/// bounded); `None` when it does not exist.
///
/// # Errors
///
/// When the file cannot be opened, is not a regular file, or exceeds 64 KiB.
pub fn read_list_file(path: &Path) -> io::Result<Option<Vec<u8>>> {
    read_bounded(path, None)
}

/// Hold an exclusive lock on `path`'s lock file while a list is read,
/// changed and replaced, so a session pruning and an enrollment recording
/// at once never lose each other's change.
fn lock_list(path: &Path, owner: u32) -> io::Result<advisory_lock::FileLock> {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    let lock = PathBuf::from(lock);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(&lock)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(invalid("device list lock is not a regular file"));
    }
    if !trusted_by(&metadata, owner) {
        return Err(refused("device list lock belongs to another account"));
    }
    // Only the person, or root acting for them, takes this lock: it is
    // theirs alone, whatever umask created it.
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if current_uid() == 0 && owner != 0 {
            // SAFETY: the descriptor is open for the call; a gid of -1
            // leaves the group.
            if unsafe { libc::fchown(file.as_raw_fd(), owner, libc::gid_t::MAX) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let me = current_uid();
        if (me == 0 || metadata.uid() == me) && metadata.permissions().mode() & 0o7777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    advisory_lock::FileLock::exclusive(file)
}

fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T, owner: u32) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    tilo_files::native::atomic::write_with_mode(path, &bytes, PERSON_FILE_MODE)?;
    give_to(path, owner)
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

fn person_dir_suffix(name: &str, identity: &str) -> bool {
    name.strip_prefix(identity)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|suffix| {
            suffix.len() == PERSON_DIR_SUFFIX_LEN
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
}

/// The person directories of `identity` in `peers`, at most one per owning
/// account: the canonical name when it is private, else the account's
/// suffixed directory first in name order. Every name in `peers` is looked
/// at, so no number of other entries pushes a person's directory out of
/// sight, and any number of directories one account makes counts once.
fn person_dirs(peers: &Path, identity: &[u8; 32]) -> std::collections::BTreeMap<u32, PathBuf> {
    let id = hex::encode(identity);
    let mut found = std::collections::BTreeMap::new();
    let canonical = peers.join(&id);
    if let Ok(Some(uid)) = private_dir_owner(&canonical) {
        found.insert(uid, canonical);
    }
    let mut suffixed: Vec<String> = std::fs::read_dir(peers)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| person_dir_suffix(name, &id))
        .collect();
    suffixed.sort();
    for name in suffixed {
        let dir = peers.join(name);
        if let Ok(Some(uid)) = private_dir_owner(&dir) {
            found.entry(uid).or_insert(dir);
        }
    }
    found
}

fn fresh_suffix(identity: &[u8; 32], attempt: u32) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(identity);
    hasher.update(&std::process::id().to_le_bytes());
    hasher.update(&attempt.to_le_bytes());
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.update(&now.as_nanos().to_le_bytes());
    }
    hex::encode(&hasher.finalize().as_bytes()[..PERSON_DIR_SUFFIX_LEN / 2])
}

/// One person's directory in the shared peers directory: their direct
/// peers, shared rooms and recorded revocations, for one identity, owned by
/// one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonPeers {
    dir: PathBuf,
    identity: [u8; 32],
    owner: u32,
}

impl PersonPeers {
    /// The directory of `identity` in `peers` that the account `owner` (or
    /// root) owns and nobody else may write, if there is one. Directories of
    /// the identity owned by anyone else are passed over.
    ///
    /// # Errors
    ///
    /// When a candidate cannot be inspected.
    pub fn find(peers: &Path, identity: &[u8; 32], owner: u32) -> io::Result<Option<Self>> {
        Self::locate(peers, identity, owner, true)
    }

    /// The directory of `identity` in `peers` for `owner`: one `owner` owns,
    /// or else, when `or_root`, one root made for it.
    fn locate(
        peers: &Path,
        identity: &[u8; 32],
        owner: u32,
        or_root: bool,
    ) -> io::Result<Option<Self>> {
        let person = |dir| Self {
            dir,
            identity: *identity,
            owner,
        };
        let canonical = peers.join(hex::encode(identity));
        if private_dir_owner(&canonical)? == Some(owner) {
            return Ok(Some(person(canonical)));
        }
        let mut dirs = person_dirs(peers, identity);
        Ok(dirs
            .remove(&owner)
            .or_else(|| or_root.then(|| dirs.remove(&0)).flatten())
            .map(person))
    }

    /// [`Self::find`] for the account this process runs as.
    ///
    /// # Errors
    ///
    /// As [`Self::find`].
    pub fn find_own(peers: &Path, identity: &[u8; 32]) -> io::Result<Option<Self>> {
        Self::find(peers, identity, current_uid())
    }

    /// The directory of `identity` in `peers` for `owner`, created (mode
    /// [`PERSON_DIR_MODE`], owned by `owner`) when there is none. When
    /// another account holds the canonical name, the new directory takes a
    /// suffixed one.
    ///
    /// # Errors
    ///
    /// When `peers` cannot be read or no directory can be created.
    pub fn open_or_create(peers: &Path, identity: &[u8; 32], owner: u32) -> io::Result<Self> {
        // Only the account's own directory is written: a root-owned one
        // (such as one seeded before the account existed) is read, never
        // changed, by anyone else.
        if let Some(found) = Self::locate(peers, identity, owner, false)? {
            return Ok(found);
        }
        std::fs::create_dir_all(peers)?;
        let canonical = hex::encode(identity);
        let names = std::iter::once(canonical.clone()).chain(
            (0..8).map(|attempt| format!("{canonical}.{}", fresh_suffix(identity, attempt))),
        );
        let mut last_error = io::Error::other("no free person directory name");
        for name in names {
            let dir = peers.join(name);
            match create_person_dir(&dir, owner) {
                Ok(()) if trusted_dir(&dir, owner)? => {
                    return Ok(Self {
                        dir,
                        identity: *identity,
                        owner,
                    });
                }
                Ok(()) => {
                    // This process cannot hand the directory to `owner`.
                    let _ = std::fs::remove_dir(&dir);
                    return Err(refused("cannot create a directory for another account"));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    /// [`Self::open_or_create`] for the account this process runs as.
    ///
    /// # Errors
    ///
    /// As [`Self::open_or_create`].
    pub fn open_or_create_own(peers: &Path, identity: &[u8; 32]) -> io::Result<Self> {
        Self::open_or_create(peers, identity, current_uid())
    }

    /// The directory itself.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The identity whose lists this directory holds.
    #[must_use]
    pub const fn identity(&self) -> &[u8; 32] {
        &self.identity
    }

    /// The account that owns this directory.
    #[must_use]
    pub const fn owner(&self) -> u32 {
        self.owner
    }

    /// Where the direct-peer list lives.
    #[must_use]
    pub fn direct_peers_path(&self) -> PathBuf {
        self.dir.join(DIRECT_PEERS_FILE)
    }

    /// Where the shared-room list lives.
    #[must_use]
    pub fn rooms_path(&self) -> PathBuf {
        self.dir.join(ROOMS_FILE)
    }

    fn revocations_dir(&self) -> PathBuf {
        self.dir.join(REVOCATIONS_DIR)
    }

    /// Refuse to go on when the directory no longer belongs to its owner.
    fn check(&self) -> io::Result<()> {
        if trusted_dir(&self.dir, self.owner)? {
            Ok(())
        } else {
            Err(refused("person directory belongs to another account"))
        }
    }

    /// The direct peers recorded here, only those of this identity; empty
    /// when there is no list.
    ///
    /// # Errors
    ///
    /// When the directory or the list belongs to another account, or the
    /// list cannot be read or violates its bounded schema.
    pub fn read_direct_peers(&self) -> io::Result<Vec<DirectPeer>> {
        self.check()?;
        let peers = read_bounded(&self.direct_peers_path(), Some(self.owner))?
            .map_or_else(|| Ok(Vec::new()), |bytes| parse_direct_peers(&bytes))?;
        Ok(peers
            .into_iter()
            .filter(|peer| peer.identity == self.identity)
            .collect())
    }

    /// Add `peers` to the direct-peer list, keeping existing entries.
    /// Returns whether the file changed.
    ///
    /// # Errors
    ///
    /// When an entry belongs to another identity, the existing list is
    /// invalid or foreign, the result would exceed [`MAX_DIRECT_PEERS`], or
    /// the file cannot be replaced.
    pub fn remember_direct_peers(&self, peers: &[DirectPeer]) -> io::Result<bool> {
        if peers.iter().any(|peer| peer.identity != self.identity) {
            return Err(invalid("direct peer belongs to another identity"));
        }
        self.check()?;
        let path = self.direct_peers_path();
        let _lock = lock_list(&path, self.owner)?;
        let mut known = self.read_direct_peers()?;
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
            &path,
            &known.iter().map(DirectPeerJson::from).collect::<Vec<_>>(),
            self.owner,
        )?;
        Ok(true)
    }

    /// Remove `peer` from the direct-peer list. Returns whether the file
    /// changed.
    ///
    /// # Errors
    ///
    /// When the existing list is invalid or foreign, or the file cannot be
    /// replaced.
    pub fn forget_direct_peer(&self, peer: DirectPeer) -> io::Result<bool> {
        self.check()?;
        let path = self.direct_peers_path();
        let _lock = lock_list(&path, self.owner)?;
        let mut known = self.read_direct_peers()?;
        let before = known.len();
        known.retain(|known| *known != peer);
        if known.len() == before {
            return Ok(false);
        }
        write_json(
            &path,
            &known.iter().map(DirectPeerJson::from).collect::<Vec<_>>(),
            self.owner,
        )?;
        Ok(true)
    }

    /// The bytes of the shared-room list; `None` when there is none.
    ///
    /// # Errors
    ///
    /// When the directory or the file belongs to another account, or the
    /// file cannot be read within its bound.
    pub fn rooms_bytes(&self) -> io::Result<Option<Vec<u8>>> {
        self.check()?;
        read_bounded(&self.rooms_path(), Some(self.owner))
    }

    /// The rooms recorded here; empty when there is no list.
    ///
    /// # Errors
    ///
    /// As [`Self::rooms_bytes`], or when the list violates its schema.
    pub fn read_rooms(&self) -> io::Result<Vec<String>> {
        self.rooms_bytes()?
            .map_or_else(|| Ok(Vec::new()), |bytes| parse_rooms(&bytes))
    }

    /// Add `rooms` to the room list, keeping existing entries. Returns
    /// whether the file changed.
    ///
    /// # Errors
    ///
    /// When a room id is invalid, the existing list is invalid or foreign,
    /// the result would exceed [`MAX_ROOMS`], or the file cannot be replaced.
    pub fn remember_rooms(&self, rooms: &[String]) -> io::Result<bool> {
        if rooms.iter().any(|room| !valid_room_id(room)) {
            return Err(invalid("personal room list is invalid"));
        }
        self.check()?;
        let path = self.rooms_path();
        let _lock = lock_list(&path, self.owner)?;
        let mut known = self.read_rooms()?;
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
        write_json(&path, &known, self.owner)?;
        Ok(true)
    }

    /// Record a revocation this identity's root signed in
    /// [`REVOCATIONS_DIR`]. Returns whether a new file was written; when a
    /// file there already holds this exact verified entry it is left alone.
    ///
    /// # Errors
    ///
    /// When another identity signed the entry, its signature does not
    /// verify, the directory belongs to another account, or no file can be
    /// written.
    pub fn record_revocation(
        &self,
        entry: &tilo_user_identity::RevocationEntry,
    ) -> io::Result<bool> {
        if entry.user_public_key != self.identity {
            return Err(invalid("revocation belongs to another identity"));
        }
        tilo_user_identity::verify_revocation_entry(entry)
            .map_err(|_| invalid("revocation signature does not verify"))?;
        self.check()?;
        let directory = self.revocations_dir();
        match create_person_dir(&directory, self.owner) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if !trusted_dir(&directory, self.owner)? {
            return Err(refused("revocation directory belongs to another account"));
        }
        let stem = revocation_stem(entry);
        if let Ok(names) = std::fs::read_dir(&directory) {
            for name in names.flatten().take(MAX_REVOCATION_DIR_ENTRIES) {
                let name = name.file_name();
                if name
                    .to_str()
                    .is_some_and(|name| names_revocation(name, entry))
                    && read_revocation_file(&directory.join(&name), self.owner).as_ref()
                        == Some(entry)
                {
                    return Ok(false);
                }
            }
        }
        let bytes = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
        let write = |path: &Path| {
            tilo_files::native::atomic::write_with_mode(path, &bytes, PERSON_FILE_MODE)
                .and_then(|()| give_to(path, self.owner))
        };
        let canonical = directory.join(format!("{stem}.json"));
        if std::fs::symlink_metadata(&canonical).is_err() && write(&canonical).is_ok() {
            return Ok(true);
        }
        let mut last_error = io::Error::other("no free revocation file name");
        for attempt in 0..8 {
            let path = directory.join(format!("{stem}.{}.json", suffix_for(entry, attempt)));
            if std::fs::symlink_metadata(&path).is_ok() {
                continue;
            }
            match write(&path) {
                Ok(()) => return Ok(true),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    /// The verified revocations of this identity recorded here. Files that
    /// are not regular, oversized, misnamed, foreign-owned or unsigned are
    /// skipped and do not count toward [`MAX_REVOCATIONS`]. A reading is
    /// reused while the directory is unchanged (no entry added, renamed or
    /// removed), so frequent callers verify nothing twice.
    ///
    /// # Errors
    ///
    /// When the directory belongs to another account, or exists but cannot
    /// be listed.
    pub fn read_revocations(&self) -> io::Result<tilo_user_identity::RevocationList> {
        self.check()?;
        read_revocations_in(&self.revocations_dir(), &self.identity, self.owner)
    }

    /// The devices recorded here as direct peers that the identity's root
    /// has not revoked — what a session dials. Revocations count from
    /// [`REVOCATIONS_DIR`] and from `known`, the ones the caller already
    /// holds.
    ///
    /// # Errors
    ///
    /// When the peer list cannot be read, is foreign, or violates its schema.
    pub fn live_direct_peers(
        &self,
        known: &tilo_user_identity::RevocationList,
    ) -> io::Result<Vec<[u8; 32]>> {
        let recorded = self.read_revocations().unwrap_or_default();
        Ok(self
            .read_direct_peers()?
            .into_iter()
            .map(|peer| peer.device)
            .filter(|device| {
                recorded.revocation_of(&self.identity, device).is_none()
                    && known.revocation_of(&self.identity, device).is_none()
            })
            .collect())
    }

    /// Remove from the direct-peer list every device this identity's root
    /// revoked, as recorded in [`REVOCATIONS_DIR`]. Returns the entries
    /// removed.
    ///
    /// # Errors
    ///
    /// When the list cannot be read or replaced.
    pub fn prune_revoked_direct_peers(&self) -> io::Result<Vec<DirectPeer>> {
        let revocations = self.read_revocations()?;
        let mut removed = Vec::new();
        for peer in self.read_direct_peers()? {
            if revocations
                .revocation_of(&peer.identity, &peer.device)
                .is_some()
                && self.forget_direct_peer(peer)?
            {
                removed.push(peer);
            }
        }
        Ok(removed)
    }
}

/// Every verified revocation of `identity` recorded in any of its person
/// directories in `peers`, whoever owns them — what a relay on the machine
/// keeps. A directory writable by anyone but its owner is skipped, and in
/// each directory only files its owner (or root) owns count. Entries are
/// root-signed, so a directory another account created for the identity can
/// add true revocations at most, never hide one recorded elsewhere.
#[must_use]
pub fn recorded_revocations(
    peers: &Path,
    identity: &[u8; 32],
) -> tilo_user_identity::RevocationList {
    let mut list = tilo_user_identity::RevocationList::new();
    for (owner, dir) in person_dirs(peers, identity)
        .into_iter()
        .take(MAX_PERSON_DIRS_PER_IDENTITY)
    {
        if let Ok(found) = read_revocations_in(&dir.join(REVOCATIONS_DIR), identity, owner) {
            for entry in found.iter() {
                let _ = list.insert(entry.clone());
            }
        }
    }
    list
}

fn revocation_stem(entry: &tilo_user_identity::RevocationEntry) -> String {
    format!(
        "{}-{}",
        hex::encode(entry.user_public_key),
        hex::encode(entry.revoked_device_public_key)
    )
}

/// Whether `name` is a file name [`PersonPeers::record_revocation`] could
/// have given `entry`: the canonical name, or the canonical stem with a short
/// hex suffix.
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

/// Read one revocation file without following a symlink or blocking on a
/// FIFO: only a regular file trusted by `owner`, within the bound, is read.
fn read_revocation_file(path: &Path, owner: u32) -> Option<tilo_user_identity::RevocationEntry> {
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
    if !metadata.file_type().is_file()
        || metadata.len() > MAX_REVOCATION_FILE_BYTES
        || !trusted_by(&metadata, owner)
    {
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

#[derive(Clone, PartialEq, Eq)]
struct DirectoryStamp {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
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
            owner: metadata.uid(),
            mode: metadata.mode(),
            modified: metadata.modified().ok(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    #[cfg(not(unix))]
    {
        DirectoryStamp {
            device: 0,
            inode: 0,
            owner: 0,
            mode: 0,
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

type RevocationCacheKey = (PathBuf, [u8; 32], u32);
type RevocationCache =
    std::sync::Mutex<std::collections::HashMap<RevocationCacheKey, CachedRevocations>>;

fn revocation_cache() -> &'static RevocationCache {
    static CACHE: std::sync::OnceLock<RevocationCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn scan_revocations(
    directory: &Path,
    identity: &[u8; 32],
    owner: u32,
) -> io::Result<tilo_user_identity::RevocationList> {
    let mut list = tilo_user_identity::RevocationList::new();
    let mut valid = 0usize;
    let prefix = format!("{}-", hex::encode(identity));
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
        // Recorded names are `<identity>-<device>[.<suffix>].json` in
        // lowercase; skip anything else before opening and verifying it.
        let named_for_identity = name.as_bytes().ends_with(b".json")
            && name
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.get(..64))
                .is_some_and(|device| decode_key(device).is_some());
        if !named_for_identity {
            continue;
        }
        let Some(entry) = read_revocation_file(&file.path(), owner) else {
            continue;
        };
        if &entry.user_public_key != identity || !names_revocation(name, &entry) {
            continue;
        }
        if list.insert(entry).is_ok() {
            valid = valid.saturating_add(1);
        }
    }
    Ok(list)
}

/// The verified revocations of `identity` in `directory`, which must be a
/// real directory trusted by `owner`; cached by the directory's stat.
fn read_revocations_in(
    directory: &Path,
    identity: &[u8; 32],
    owner: u32,
) -> io::Result<tilo_user_identity::RevocationList> {
    let metadata = match std::fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(tilo_user_identity::RevocationList::new());
        }
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_dir() || !trusted_by(&metadata, owner) {
        return Err(refused("revocation directory belongs to another account"));
    }
    let stamp = directory_stamp(&metadata);
    let key = (directory.to_path_buf(), *identity, owner);
    if let Ok(cache) = revocation_cache().lock()
        && let Some(cached) = cache.get(&key)
        && cached.stamp == stamp
        && cached.read_at.elapsed() < REVOCATION_CACHE_MAX_AGE
    {
        return Ok(cached.list.clone());
    }
    let list = scan_revocations(directory, identity, owner)?;
    if let Ok(mut cache) = revocation_cache().lock() {
        cache.insert(
            key,
            CachedRevocations {
                stamp,
                read_at: std::time::Instant::now(),
                list: list.clone(),
            },
        );
    }
    Ok(list)
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

/// The rooms in a room list outside a person directory, such as a
/// session's export; empty when the file does not exist.
///
/// # Errors
///
/// When the file cannot be read or violates its bounded schema.
pub fn read_rooms(path: &Path) -> io::Result<Vec<String>> {
    read_bounded(path, None)?.map_or_else(|| Ok(Vec::new()), |bytes| parse_rooms(&bytes))
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
    let Some(bytes) = read_bounded(path, None)? else {
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

    /// Two accounts, A and B. As root, directories are handed to two
    /// ordinary uids; otherwise A is this process and B any other uid.
    fn accounts() -> (u32, u32) {
        let me = current_uid();
        if me == 0 {
            (4001, 4002)
        } else {
            (me, me.wrapping_add(1))
        }
    }

    fn person(peers: &Path, identity: &[u8; 32]) -> PersonPeers {
        PersonPeers::open_or_create(peers, identity, accounts().0).unwrap()
    }

    fn peer(identity: u8, device: u8) -> DirectPeer {
        DirectPeer {
            identity: [identity; 32],
            device: [device; 32],
        }
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    #[test]
    fn lists_merge_without_losing_entries_and_refuse_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let ours = person(dir.path(), &[9; 32]);
        assert!(ours.read_direct_peers().unwrap().is_empty());
        assert!(ours.remember_direct_peers(&[peer(9, 1)]).unwrap());
        assert!(
            ours.remember_direct_peers(&[peer(9, 2), peer(9, 1)])
                .unwrap()
        );
        assert!(!ours.remember_direct_peers(&[peer(9, 2)]).unwrap());
        // A person's directory holds only their own identity's devices.
        assert!(ours.remember_direct_peers(&[peer(8, 2)]).is_err());
        assert_eq!(
            ours.read_direct_peers().unwrap(),
            vec![peer(9, 1), peer(9, 2)]
        );
        assert!(ours.forget_direct_peer(peer(9, 1)).unwrap());
        assert!(!ours.forget_direct_peer(peer(7, 2)).unwrap());
        assert_eq!(ours.read_direct_peers().unwrap(), vec![peer(9, 2)]);
        assert_eq!(
            parse_direct_peers(&encode_direct_peers(&[peer(9, 2)]).unwrap()).unwrap(),
            vec![peer(9, 2)]
        );
        // Another identity's entries written into the file are not read.
        std::fs::write(
            ours.direct_peers_path(),
            encode_direct_peers(&[peer(9, 2), peer(8, 3)]).unwrap(),
        )
        .unwrap();
        assert_eq!(ours.read_direct_peers().unwrap(), vec![peer(9, 2)]);
        std::fs::write(
            ours.direct_peers_path(),
            br#"[{"identity":"ABCD","device":"00"}]"#,
        )
        .unwrap();
        assert!(ours.remember_direct_peers(&[peer(9, 3)]).is_err());
        std::fs::write(
            ours.direct_peers_path(),
            format!(r#"["{}"]"#, "11".repeat(32)),
        )
        .unwrap();
        assert!(ours.read_direct_peers().is_err());

        let room = "0f8fad5b-d9cb-469f-a165-70867728950e".to_string();
        assert!(ours.remember_rooms(std::slice::from_ref(&room)).unwrap());
        assert!(!ours.remember_rooms(std::slice::from_ref(&room)).unwrap());
        assert_eq!(ours.read_rooms().unwrap(), vec![room]);
        assert!(ours.remember_rooms(&["../etc".into()]).is_err());
        assert!(parse_rooms(br#"["not-a-room"]"#).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_person_directory_and_its_files_are_private_to_the_person() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let ours = person(dir.path(), &[9; 32]);
        assert_eq!(ours.dir(), dir.path().join(hex::encode([9; 32])));
        ours.remember_direct_peers(&[peer(9, 1)]).unwrap();
        ours.remember_rooms(&["0f8fad5b-d9cb-469f-a165-70867728950e".into()])
            .unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let ours = person(dir.path(), &root.verifying_key().to_bytes());
        ours.record_revocation(&revocation(&root, [1; 32])).unwrap();
        let first = person(dir.path(), &[9; 32]);

        // The owner writes; the identity group reads; nobody else may
        // change anything.
        for path in [first.dir().to_path_buf(), ours.revocations_dir()] {
            assert_eq!(mode(&path) & 0o777, PERSON_DIR_MODE & 0o777, "{path:?}");
        }
        for path in [first.direct_peers_path(), first.rooms_path()] {
            assert_eq!(mode(&path), PERSON_FILE_MODE, "{path:?}");
        }
        let revocation_file = std::fs::read_dir(ours.revocations_dir())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(mode(&revocation_file), PERSON_FILE_MODE);
        let mut lock = first.direct_peers_path().into_os_string();
        lock.push(".lock");
        assert_eq!(mode(Path::new(&lock)), 0o600);

        // Everything belongs to the person's account.
        let (a, _) = accounts();
        for path in [
            first.dir().to_path_buf(),
            first.direct_peers_path(),
            ours.revocations_dir(),
            revocation_file,
        ] {
            assert_eq!(std::fs::symlink_metadata(&path).unwrap().uid(), a);
        }
    }

    #[cfg(unix)]
    #[test]
    fn another_persons_writes_never_alter_ones_own_lists() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = accounts();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let user = root.verifying_key().to_bytes();
        let alice = PersonPeers::open_or_create(dir.path(), &user, a).unwrap();
        alice
            .remember_direct_peers(&[DirectPeer {
                identity: user,
                device: [1; 32],
            }])
            .unwrap();

        // B's view of the same identity: A's directory is not B's, and
        // nothing of A's reads as B's.
        assert_eq!(PersonPeers::find(dir.path(), &user, b).unwrap(), None);
        let as_b = PersonPeers {
            dir: alice.dir().to_path_buf(),
            identity: user,
            owner: b,
        };
        assert_eq!(
            as_b.read_direct_peers().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(as_b.read_revocations().is_err());
        assert!(
            as_b.remember_direct_peers(&[DirectPeer {
                identity: user,
                device: [2; 32],
            }])
            .is_err()
        );
        assert!(as_b.record_revocation(&revocation(&root, [1; 32])).is_err());

        // B writing for the identity lands in B's own directory, if
        // anywhere, and A's list and revocations stay as they were.
        if let Ok(bobs) = PersonPeers::open_or_create(dir.path(), &user, b) {
            assert_ne!(bobs.dir(), alice.dir());
            bobs.remember_direct_peers(&[DirectPeer {
                identity: user,
                device: [3; 32],
            }])
            .unwrap();
            bobs.record_revocation(&revocation(&root, [1; 32])).unwrap();
        }
        let alice = PersonPeers::find(dir.path(), &user, a).unwrap().unwrap();
        assert_eq!(alice.dir(), dir.path().join(hex::encode(user)));
        assert_eq!(
            alice
                .live_direct_peers(&tilo_user_identity::RevocationList::new())
                .unwrap(),
            vec![[1; 32]]
        );
        assert!(alice.read_revocations().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_taken_or_shared_name_sends_the_person_to_their_own_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let identity = [9; 32];
        // Someone pre-created the canonical name, writable by everyone, with
        // a list in it.
        let canonical = dir.path().join(hex::encode(identity));
        std::fs::create_dir(&canonical).unwrap();
        std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::write(
            canonical.join(DIRECT_PEERS_FILE),
            encode_direct_peers(&[peer(9, 6)]).unwrap(),
        )
        .unwrap();
        // And a suffixed name as a symlink to it.
        std::os::unix::fs::symlink(
            &canonical,
            dir.path()
                .join(format!("{}.{}", hex::encode(identity), "0".repeat(16))),
        )
        .unwrap();
        let (a, _) = accounts();
        assert_eq!(PersonPeers::find(dir.path(), &identity, a).unwrap(), None);
        let ours = person(dir.path(), &identity);
        assert_ne!(ours.dir(), canonical);
        assert!(person_dir_suffix(
            &ours.dir().file_name().unwrap().to_string_lossy(),
            &hex::encode(identity)
        ));
        assert!(ours.read_direct_peers().unwrap().is_empty());
        // Found again, not created a second time.
        assert_eq!(
            PersonPeers::find(dir.path(), &identity, a).unwrap(),
            Some(ours.clone())
        );
        assert_eq!(person(dir.path(), &identity), ours);

        // A list file someone else may write is refused, and the canonical
        // directory, once private, is preferred again.
        std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(0o750)).unwrap();
        if current_uid() == 0 {
            give_to(&canonical, a).unwrap();
        }
        let canonical_person = PersonPeers::find(dir.path(), &identity, a)
            .unwrap()
            .unwrap();
        assert_eq!(canonical_person.dir(), canonical);
        std::fs::set_permissions(
            canonical.join(DIRECT_PEERS_FILE),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        assert_eq!(
            canonical_person.read_direct_peers().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_root_made_is_read_but_never_written_for_a_person() {
        if current_uid() != 0 {
            // Only root can make the directory this is about.
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (a, _) = accounts();
        let canonical = dir.path().join(hex::encode([9; 32]));
        create_person_dir(&canonical, 0).unwrap();
        std::fs::write(
            canonical.join(DIRECT_PEERS_FILE),
            encode_direct_peers(&[peer(9, 1)]).unwrap(),
        )
        .unwrap();
        // Readers take it while the person has none of their own.
        let seeded = PersonPeers::find(dir.path(), &[9; 32], a).unwrap().unwrap();
        assert_eq!(seeded.dir(), canonical);
        assert_eq!(seeded.read_direct_peers().unwrap(), vec![peer(9, 1)]);
        // Writing for the person makes their own, which readers then prefer.
        let own = person(dir.path(), &[9; 32]);
        assert_ne!(own.dir(), canonical);
        own.remember_direct_peers(&[peer(9, 2)]).unwrap();
        let found = PersonPeers::find(dir.path(), &[9; 32], a).unwrap().unwrap();
        assert_eq!(found, own);
        assert_eq!(found.read_direct_peers().unwrap(), vec![peer(9, 2)]);
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
    fn a_revocation_prunes_only_its_own_identitys_devices() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let user = root.verifying_key().to_bytes();
        let ours = |device: u8| DirectPeer {
            identity: user,
            device: [device; 32],
        };
        let person_peers = person(dir.path(), &user);
        person_peers
            .remember_direct_peers(&[ours(1), ours(2), ours(3)])
            .unwrap();

        assert!(
            person_peers
                .record_revocation(&revocation(&root, [1; 32]))
                .unwrap()
        );
        assert!(
            !person_peers
                .record_revocation(&revocation(&root, [1; 32]))
                .unwrap()
        );
        // Another identity's revocation has no place here.
        assert!(
            person_peers
                .record_revocation(&revocation(&stranger, [2; 32]))
                .is_err()
        );
        // Unsigned entries are refused, and planted files are skipped.
        let mut forged = revocation(&root, [3; 32]);
        forged.signature[0] ^= 1;
        assert!(person_peers.record_revocation(&forged).is_err());
        let directory = person_peers.revocations_dir();
        std::fs::write(
            directory.join(format!("{}.json", revocation_stem(&forged))),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.join("other.json"),
            serde_json::to_vec(&revocation(&root, [3; 32])).unwrap(),
        )
        .unwrap();
        // A stranger's valid revocation under a matching name is not ours.
        let foreign = revocation(&stranger, [2; 32]);
        std::fs::write(
            directory.join(format!("{}.json", revocation_stem(&foreign))),
            serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();

        let none = tilo_user_identity::RevocationList::new();
        assert_eq!(
            person_peers.live_direct_peers(&none).unwrap(),
            vec![[2; 32], [3; 32]]
        );
        let mut held = tilo_user_identity::RevocationList::new();
        held.insert(revocation(&root, [3; 32])).unwrap();
        assert_eq!(
            person_peers.live_direct_peers(&held).unwrap(),
            vec![[2; 32]]
        );
        assert_eq!(
            person_peers.prune_revoked_direct_peers().unwrap(),
            vec![ours(1)]
        );
        assert_eq!(
            person_peers.read_direct_peers().unwrap(),
            vec![ours(2), ours(3)]
        );
        assert!(
            person_peers
                .prune_revoked_direct_peers()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_name_taken_by_something_else_does_not_hide_a_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let user = root.verifying_key().to_bytes();
        let entry = revocation(&root, [1; 32]);
        let ours = person(dir.path(), &user);
        let directory = ours.revocations_dir();
        create_person_dir(&directory, accounts().0).unwrap();
        // Garbage planted at the name the revocation would take.
        let canonical = directory.join(format!("{}.json", revocation_stem(&entry)));
        std::fs::write(&canonical, b"not a revocation").unwrap();
        assert!(ours.record_revocation(&entry).unwrap());
        assert_eq!(std::fs::read(&canonical).unwrap(), b"not a revocation");
        assert!(
            ours.read_revocations()
                .unwrap()
                .revocation_of(&user, &[1; 32])
                .is_some()
        );
        // Recorded once, the entry is found again under its suffixed name.
        assert!(!ours.record_revocation(&entry).unwrap());
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
    fn hostile_files_never_block_or_count() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let user = root.verifying_key().to_bytes();
        let ours = person(dir.path(), &user);
        let directory = ours.revocations_dir();
        create_person_dir(&directory, accounts().0).unwrap();
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
        // A valid entry anyone may rewrite is not trusted.
        let shared = directory.join(format!("{stem}.2f.json"));
        std::fs::write(&shared, serde_json::to_vec(&real).unwrap()).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o666)).unwrap();
        // Junk does not use up the bound on valid entries.
        for index in 0..(MAX_REVOCATIONS + 10) {
            std::fs::write(directory.join(format!("junk-{index}.json")), b"{}").unwrap();
        }
        assert!(
            ours.read_revocations()
                .unwrap()
                .revocation_of(&user, &[1; 32])
                .is_none()
        );
        assert!(ours.record_revocation(&real).unwrap());
        assert!(
            ours.read_revocations()
                .unwrap()
                .revocation_of(&user, &[1; 32])
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_relay_keeps_every_recorded_revocation_of_its_identity() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = accounts();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let user = root.verifying_key().to_bytes();
        let ours = person(dir.path(), &user);
        ours.record_revocation(&revocation(&root, [1; 32])).unwrap();
        // Directories someone else made for the identity, named to sort
        // first: their true revocations count, and however many there are,
        // they hide nothing of the person's.
        for index in 0..40_u32 {
            let path = dir
                .path()
                .join(format!("{}.{index:016x}", hex::encode(user)));
            create_person_dir(&path, b).unwrap();
            if current_uid() == 0 {
                give_to(&path, b).unwrap();
            }
            if index == 0 {
                PersonPeers {
                    dir: path,
                    identity: user,
                    owner: if current_uid() == 0 { b } else { a },
                }
                .record_revocation(&revocation(&root, [2; 32]))
                .unwrap();
            }
        }
        // A directory anyone may write is skipped.
        let shared = dir
            .path()
            .join(format!("{}.{}", hex::encode(user), "f".repeat(16)));
        create_person_dir(&shared, a).unwrap();
        PersonPeers {
            dir: shared.clone(),
            identity: user,
            owner: a,
        }
        .record_revocation(&revocation(&root, [3; 32]))
        .unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        // Another identity's directory holds nothing of ours.
        person(dir.path(), &stranger.verifying_key().to_bytes())
            .record_revocation(&revocation(&stranger, [4; 32]))
            .unwrap();

        let list = recorded_revocations(dir.path(), &user);
        assert!(list.revocation_of(&user, &[1; 32]).is_some());
        assert!(list.revocation_of(&user, &[3; 32]).is_none());
        assert!(list.iter().all(|entry| entry.user_public_key == user));
        if current_uid() == 0 {
            assert!(list.revocation_of(&user, &[2; 32]).is_some());
        }
        // The person still finds their own directory among them all.
        assert_eq!(
            PersonPeers::find(dir.path(), &user, a)
                .unwrap()
                .unwrap()
                .dir(),
            ours.dir()
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
