//! Persistent on-disk state for the relay.
//!
//! Layout (rooted at `RelayConfig.state_root`):
//!
//! ```text
//! /var/lib/tilo-relay/
//! ├── iroh-secret.bin          # relay's iroh node secret (0600)
//! └── workspaces/
//!     └── <workspace_id>/
//!         ├── snapshot.automerge    # full pinned CRDT snapshot
//!         ├── log.jsonl             # append-only delta metadata (audit)
//!         ├── retained/v2/<author>-<channel>/
//!         │   ├── CURRENT          # atomic generation selector
//!         │   └── <generation>/    # shared checkpoint + immutable tail
//!         └── pending/
//!             └── <device_pubkey_hex>/
//!                 └── <seq>.delta   # non-retained offline deltas
//! ```
//!
//! `snapshot.automerge` is only written when a workspace is
//! `pin = true` in the config; unpinned workspaces are pure fan-out
//! + store-and-forward. Pending directories exist regardless of
//!   pin state — a peer can be offline and still catch up when it reconnects.
//!
//! A device's pending queue ([`MAX_PENDING_ENTRIES_PER_DEVICE`],
//! [`MAX_PENDING_BYTES_PER_DEVICE`]) and the audit log ([`MAX_LOG_BYTES`])
//! are bounded here and drop their oldest entries on overflow: a pending
//! delta is also in the pinned document that anti-entropy serves. A retained
//! tail is never cut by size — dropping any entry would leave later changes
//! without their dependencies — so a lineage retires records only once a
//! checkpoint of it covers them (see `retained`).

pub(crate) mod bounded;
pub(crate) mod continuity;
pub(crate) mod replay;
pub(crate) mod retained;
#[cfg(test)]
pub(crate) mod shell_proof_fixture;
#[cfg(test)]
pub(crate) mod shell_proof_tests;
pub(crate) mod shell_selection;
#[cfg(test)]
pub(crate) mod test_barrier;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Filename used for the pinned Automerge snapshot inside a
/// workspace directory.
pub const SNAPSHOT_FILE: &str = "snapshot.automerge";

/// Filename used for the append-only audit log.
pub const LOG_FILE: &str = "log.jsonl";

/// Durable device membership for offline store-and-forward.
pub const MEMBERSHIP_FILE: &str = "membership.json";
/// Atomic native-governance projection: exact room devices plus handshake roots.
pub const GOVERNED_ACCESS_FILE: &str = "governed-access-v1.json";

/// Retained bootstrap checkpoints and the incremental tails after them.
pub const RETAINED_DIR: &str = "retained";

/// Most queued deltas one offline device may hold. Retained document
/// traffic never enters the per-device queue (the retained store replays
/// it), so this bounds only workspace-directory deltas and federation
/// traffic, which the pinned document also serves through anti-entropy.
pub const MAX_PENDING_ENTRIES_PER_DEVICE: u64 = 4096;
/// Most queued bytes one offline device may hold.
pub const MAX_PENDING_BYTES_PER_DEVICE: u64 = 64 * 1024 * 1024;
/// Size at which `log.jsonl` is rotated to `log.jsonl.1`, replacing the
/// previous rotation. The audit trail keeps between one and two of these.
pub const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;
/// Durable personal-mesh compute route bindings and lifecycle state.
pub const COMPUTE_DIR: &str = "compute";
/// Dedicated queue for the versioned personal-compute protocol.
pub const COMPUTE_PENDING_DIR: &str = "compute-pending";

/// Root-signed device revocations enforced during relay authentication.
pub const REVOCATIONS_FILE: &str = "revocations.json";
/// The metrics snapshot file is read by clients as well as the daemon.
pub use tilo_relay_protocol::metrics::{METRICS_FILE, load_metrics, save_metrics};

/// Load root-signed revocation entries from the relay state root.
///
/// # Errors
///
/// Returns an error if the file exists but can't be read, or fails to
/// decode as JSON.
pub fn load_revocations(
    state_root: &Path,
) -> Result<Vec<tilo_user_identity::RevocationEntry>, StoreError> {
    let path = state_root.join(REVOCATIONS_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&fs::read(path)?).map_err(|error| StoreError::Json(error.to_string()))
}

/// Atomically replace the relay's root-signed revocation entries.
///
/// # Errors
///
/// Returns an error if the state root can't be created, `entries`
/// can't be JSON-encoded, or the atomic write fails.
pub fn save_revocations(
    state_root: &Path,
    entries: &[tilo_user_identity::RevocationEntry],
) -> Result<(), StoreError> {
    fs::create_dir_all(state_root)?;
    let bytes =
        serde_json::to_vec_pretty(entries).map_err(|error| StoreError::Json(error.to_string()))?;
    atomic_file::write(&state_root.join(REVOCATIONS_FILE), &bytes)?;
    Ok(())
}

/// Errors from store operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// JSON encode/decode failure for the audit log.
    #[error("json: {0}")]
    Json(String),
}

/// One line in `log.jsonl` — an audit trail of every delta the
/// relay received.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    /// Workspace id.
    pub workspace_id: u64,
    /// Received-at nanoseconds since epoch (relay clock).
    pub received_at_ns: u64,
    /// Author device pubkey, hex-encoded.
    pub author_device_hex: String,
    /// Author's own authored-at timestamp.
    pub authored_at_ns: u64,
    /// Delta size in bytes.
    pub delta_len: u64,
}

/// On-disk manager for one workspace's state.
#[derive(Clone)]
pub struct WorkspaceStore {
    root: PathBuf,
    workspace_id: u64,
    pinned: bool,
    shell_authority: Option<tilo_peer_state::retained_proof::RetentionAuthority>,
    /// Entry and byte totals of the bounded directories written through this
    /// store (and its clones), keyed by directory. Filled from disk on first
    /// use and re-read from disk before it may report an overflow, since a
    /// replay acknowledges entries by removing their files outside the
    /// store; so it never drops a queue over a stale count.
    usage: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, QueueUsage>>>,
    bounds: QueueBounds,
    retention: RetentionBounds,
    /// Devices whose un-sent state was compacted or whose retained history
    /// reached its bound, waiting to be asked for a checkpoint by the
    /// connection side. One entry per device, so it stays bounded.
    checkpoint_wants: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<[u8; 32]>>>,
}

/// Bounds on the per-device pending queues. Production uses
/// [`QueueBounds::default`]; tests shrink them to exercise the same eviction
/// path without writing thousands of files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueBounds {
    /// See [`MAX_PENDING_ENTRIES_PER_DEVICE`].
    pub pending_entries: u64,
    /// See [`MAX_PENDING_BYTES_PER_DEVICE`].
    pub pending_bytes: u64,
}

impl Default for QueueBounds {
    fn default() -> Self {
        Self {
            pending_entries: MAX_PENDING_ENTRIES_PER_DEVICE,
            pending_bytes: MAX_PENDING_BYTES_PER_DEVICE,
        }
    }
}

/// Bound on one publisher's retained history for one channel. Retained
/// records are never dropped to meet it — any of them may carry a change a
/// later one depends on — so reaching it asks the publisher for a checkpoint
/// that declares what it holds, which then retires those records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionBounds {
    /// Records after which the publisher is asked for a checkpoint.
    pub soft_entries: u64,
    /// Bytes after which the publisher is asked for a checkpoint.
    pub soft_bytes: u64,
}

impl RetentionBounds {
    /// Default records per publisher and channel.
    pub const DEFAULT_SOFT_ENTRIES: u64 = 16_384;
    /// Default bytes per publisher and channel.
    pub const DEFAULT_SOFT_BYTES: u64 = 256 * 1024 * 1024;
}

impl Default for RetentionBounds {
    fn default() -> Self {
        Self {
            soft_entries: Self::DEFAULT_SOFT_ENTRIES,
            soft_bytes: Self::DEFAULT_SOFT_BYTES,
        }
    }
}

/// Running totals of one bounded queue directory.
#[derive(Clone, Copy, Debug, Default)]
struct QueueUsage {
    entries: u64,
    bytes: u64,
}

impl QueueUsage {
    /// Totals of a queue's entries. A compacted document entry (see
    /// [`WorkspaceStore::compact_pending`]) counts as an entry but not as
    /// bytes.
    fn of(entries: &[(PathBuf, u64)]) -> Self {
        Self {
            entries: entries.len() as u64,
            bytes: entries
                .iter()
                .filter(|(path, _)| !is_document_entry(path))
                .map(|(_, len)| *len)
                .sum(),
        }
    }
}

/// Whether a pending entry is a compacted whole document: named by its
/// sequence alone, where an enqueued delta also carries its digest.
fn is_document_entry(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.len() == 16 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Identity of the membership files (the device list and the governed-access
/// projection) as last loaded: every rewrite is an atomic rename, so a new
/// inode (or size, or mtime) of either means new contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipStamp {
    membership: Option<FileStamp>,
    governed: Option<FileStamp>,
}

/// Identity of one file's current contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    inode: u64,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl FileStamp {
    fn of(path: &Path) -> Result<Option<Self>, StoreError> {
        match fs::metadata(path) {
            Ok(metadata) => {
                #[cfg(unix)]
                let inode = std::os::unix::fs::MetadataExt::ino(&metadata);
                #[cfg(not(unix))]
                let inode = 0;
                Ok(Some(Self {
                    inode,
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// One server-local projection of the native room governor. Both sets share a
/// single atomic file so a crash cannot pair new roots with old devices (or the
/// reverse).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GovernedAccessRecord {
    pub(crate) member_devices: Vec<[u8; 32]>,
    pub(crate) member_user_roots: Vec<[u8; 32]>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernedAccessFile {
    version: u8,
    member_devices: Vec<String>,
    member_user_roots: Vec<String>,
}

fn decode_public_keys(kind: &str, values: Vec<String>) -> Result<Vec<[u8; 32]>, StoreError> {
    values
        .into_iter()
        .map(|value| {
            let bytes = hex::decode(&value).map_err(|error| {
                StoreError::Json(format!("invalid governed {kind} key {value:?}: {error}"))
            })?;
            bytes.try_into().map_err(|bytes: Vec<u8>| {
                StoreError::Json(format!(
                    "governed {kind} key must be 32 bytes, got {}",
                    bytes.len()
                ))
            })
        })
        .collect()
}

/// One durable queue entry loaded for delivery but not yet acknowledged.
pub(crate) struct PendingEntry {
    /// On-disk file removed only after the replay reaches the wire.
    pub path: PathBuf,
    /// Complete framed relay envelope.
    pub bytes: Vec<u8>,
}

impl WorkspaceStore {
    /// Open (creating if necessary) the on-disk state for
    /// `workspace_id` under `state_root/workspaces/<id>/`.
    ///
    /// `pinned=false` skips creating `snapshot.automerge` — the
    /// caller does not intend to hold a persistent CRDT.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the workspace's on-disk directories
    /// can't be created.
    pub fn open(state_root: &Path, workspace_id: u64, pinned: bool) -> Result<Self, StoreError> {
        let root = state_root.join("workspaces").join(workspace_id.to_string());
        #[cfg(test)]
        test_barrier::pause(&root, "open");
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("pending"))?;
        fs::create_dir_all(root.join(RETAINED_DIR))?;
        fs::create_dir_all(root.join(COMPUTE_DIR))?;
        fs::create_dir_all(root.join(COMPUTE_PENDING_DIR))?;
        Ok(Self {
            root,
            workspace_id,
            pinned,
            shell_authority: None,
            usage: std::sync::Arc::default(),
            bounds: QueueBounds::default(),
            retention: RetentionBounds::default(),
            checkpoint_wants: std::sync::Arc::default(),
        })
    }

    /// Replace the queue bounds (see [`QueueBounds`]).
    pub fn set_bounds(&mut self, bounds: QueueBounds) {
        self.bounds = bounds;
    }

    /// Replace the retained-history bounds (see [`RetentionBounds`]).
    pub fn set_retention_bounds(&mut self, bounds: RetentionBounds) {
        self.retention = bounds;
    }

    /// The retained-history bounds (see [`RetentionBounds`]).
    pub fn retention_bounds(&self) -> RetentionBounds {
        self.retention
    }

    /// Root directory for this workspace's state.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Workspace id.
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
    }

    /// Install a locally supplied shell pin before handing this store to the
    /// ordered persistence owner. Persisted bytes are a conflict fence, never an
    /// independent source of trust; `open` remains unbound until this call.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` if the inputs violate this operation's bounds or invariants.
    pub fn bind_shell_authority(
        &mut self,
        authority: tilo_peer_state::retained_proof::RetentionAuthority,
    ) -> Result<(), StoreError> {
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Binding {
            version: u32,
            authority: tilo_peer_state::retained_proof::RetentionAuthority,
        }
        let invalid = |message| StoreError::Io(io::Error::new(io::ErrorKind::InvalidData, message));
        if !crate::room_workspace_id_matches(authority.room(), self.workspace_id) {
            return Err(invalid("shell authority room/workspace mismatch"));
        }
        if self
            .shell_authority
            .as_ref()
            .is_some_and(|prior| prior != &authority)
        {
            return Err(invalid("conflicting local shell authority"));
        }
        let path = self.root.join("shell-authority-v1.json");
        match fs::read(&path) {
            Ok(bytes) => {
                let prior: Binding =
                    serde_json::from_slice(&bytes).map_err(|e| StoreError::Json(e.to_string()))?;
                if prior.version != 1 || prior.authority != authority {
                    return Err(invalid("conflicting persisted shell authority"));
                }
                // Re-establish the durability barrier even after an interrupted
                // previous attempt left the exact file visible.
                fs::File::open(&path)?.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let bytes = serde_json::to_vec(&Binding {
                    version: 1,
                    authority: authority.clone(),
                })
                .map_err(|e| StoreError::Json(e.to_string()))?;
                atomic_file::write(&path, &bytes)?;
            }
            Err(error) => return Err(error.into()),
        }
        fs::File::open(&self.root)?.sync_all()?;
        self.shell_authority = Some(authority);
        Ok(())
    }

    /// Exact locally supplied authority, absent for generic workspace stores.
    pub fn shell_authority(&self) -> Option<&tilo_peer_state::retained_proof::RetentionAuthority> {
        self.shell_authority.as_ref()
    }

    /// Whether this workspace is pinned (holds a persistent CRDT
    /// snapshot).
    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// Load the pinned Automerge snapshot if present. Returns
    /// `Ok(None)` when there is no snapshot yet (first boot for
    /// this workspace).
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot file exists but can't be read.
    pub fn load_snapshot(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let p = self.root.join(SNAPSHOT_FILE);
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(fs::read(p)?))
    }

    /// Atomically write a new snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails.
    pub fn save_snapshot(&self, bytes: &[u8]) -> Result<(), StoreError> {
        if !self.pinned {
            return Ok(());
        }
        atomic_file::write(&self.root.join(SNAPSHOT_FILE), bytes)?;
        Ok(())
    }

    /// Load device public keys previously admitted to this workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but can't be read, fails
    /// to decode as JSON, or contains a device key that isn't valid
    /// hex or isn't exactly 32 bytes.
    pub fn load_membership(&self) -> Result<Vec<[u8; 32]>, StoreError> {
        if let Some(access) = self.load_governed_access()? {
            return Ok(access.member_devices);
        }
        let path = self.root.join(MEMBERSHIP_FILE);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        #[cfg(test)]
        test_barrier::pause(&self.root, "membership-read");
        let encoded: Vec<String> =
            serde_json::from_slice(&bytes).map_err(|error| StoreError::Json(error.to_string()))?;
        encoded
            .into_iter()
            .map(|value| {
                let bytes = hex::decode(&value).map_err(|error| {
                    StoreError::Json(format!("invalid membership device key {value:?}: {error}"))
                })?;
                bytes.try_into().map_err(|bytes: Vec<u8>| {
                    StoreError::Json(format!(
                        "membership device key must be 32 bytes, got {}",
                        bytes.len()
                    ))
                })
            })
            .collect()
    }

    /// Identity of the current contents of the files
    /// [`Self::load_membership`] reads, `None` when neither exists. Lets a
    /// caller that polls membership skip re-reading and re-parsing unchanged
    /// files; take it before the read it vouches for.
    ///
    /// # Errors
    ///
    /// Returns an error if a file exists but its metadata can't be read.
    pub fn membership_stamp(&self) -> Result<Option<MembershipStamp>, StoreError> {
        let stamp = MembershipStamp {
            membership: FileStamp::of(&self.root.join(MEMBERSHIP_FILE))?,
            governed: FileStamp::of(&self.root.join(GOVERNED_ACCESS_FILE))?,
        };
        Ok((stamp.membership.is_some() || stamp.governed.is_some()).then_some(stamp))
    }

    /// Atomically replace the durable membership set.
    ///
    /// # Errors
    ///
    /// Returns an error if `devices` can't be JSON-encoded or the
    /// atomic write fails.
    pub fn save_membership(&self, devices: &[[u8; 32]]) -> Result<(), StoreError> {
        if let Some(mut access) = self.load_governed_access()? {
            access.member_devices = devices.to_vec();
            return self.save_governed_access(&access);
        }
        let encoded: Vec<_> = devices.iter().map(hex::encode).collect();
        let bytes = serde_json::to_vec_pretty(&encoded)
            .map_err(|error| StoreError::Json(error.to_string()))?;
        #[cfg(test)]
        test_barrier::pause(&self.root, "membership-write");
        atomic_file::write(&self.root.join(MEMBERSHIP_FILE), &bytes)?;
        Ok(())
    }

    /// Load the atomic native-governance projection. Absence is an empty
    /// handshake-root projection; device-only membership remains readable through
    /// [`Self::load_membership`] but can never admit another user root.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, malformed, wrong-version, or invalid
    /// key data. Callers fail closed rather than partially applying it.
    pub(crate) fn load_governed_access(&self) -> Result<Option<GovernedAccessRecord>, StoreError> {
        let path = self.root.join(GOVERNED_ACCESS_FILE);
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let encoded: GovernedAccessFile =
            serde_json::from_slice(&bytes).map_err(|error| StoreError::Json(error.to_string()))?;
        if encoded.version != 1 {
            return Err(StoreError::Json(format!(
                "unsupported governed access version {}",
                encoded.version
            )));
        }
        Ok(Some(GovernedAccessRecord {
            member_devices: decode_public_keys("device", encoded.member_devices)?,
            member_user_roots: decode_public_keys("user root", encoded.member_user_roots)?,
        }))
    }

    /// Atomically replace exact room devices and global-handshake roots in one
    /// crash-safe record.
    ///
    /// # Errors
    ///
    /// Returns an error if encoding or atomic replacement fails.
    pub(crate) fn save_governed_access(
        &self,
        access: &GovernedAccessRecord,
    ) -> Result<(), StoreError> {
        let encoded = GovernedAccessFile {
            version: 1,
            member_devices: access.member_devices.iter().map(hex::encode).collect(),
            member_user_roots: access.member_user_roots.iter().map(hex::encode).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&encoded)
            .map_err(|error| StoreError::Json(error.to_string()))?;
        atomic_file::write(&self.root.join(GOVERNED_ACCESS_FILE), &bytes)?;
        Ok(())
    }

    /// Claim a compute job identifier durably before routing it. Returns the
    /// existing record when the identifier was already claimed so callers can
    /// distinguish an idempotent replay from a conflicting request.
    ///
    /// # Errors
    ///
    /// Returns an error if the record cannot be encoded, created, synced, or
    /// an existing record cannot be decoded.
    pub fn claim_compute_job(
        &self,
        record: &crate::compute_transport::DurableComputeJob,
    ) -> Result<Option<crate::compute_transport::DurableComputeJob>, StoreError> {
        let path = self.compute_job_path(&record.spec.job_id);
        let bytes =
            serde_json::to_vec(record).map_err(|error| StoreError::Json(error.to_string()))?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(&bytes)?;
                file.sync_all()?;
                Ok(None)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.load_compute_job(&record.spec.job_id)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Load one durable compute route and lifecycle record.
    ///
    /// # Errors
    ///
    /// Returns an error if the record cannot be read or decoded.
    pub fn load_compute_job(
        &self,
        job_id: &[u8; 16],
    ) -> Result<Option<crate::compute_transport::DurableComputeJob>, StoreError> {
        let path = self.compute_job_path(job_id);
        if !path.exists() {
            return Ok(None);
        }
        serde_json::from_slice(&fs::read(path)?)
            .map(Some)
            .map_err(|error| StoreError::Json(error.to_string()))
    }

    /// Atomically replace one previously claimed compute record.
    ///
    /// # Errors
    ///
    /// Returns an error if the record is unknown, cannot be encoded, or the
    /// crash-safe replacement fails.
    pub fn save_compute_job(
        &self,
        record: &crate::compute_transport::DurableComputeJob,
    ) -> Result<(), StoreError> {
        let path = self.compute_job_path(&record.spec.job_id);
        if !path.exists() {
            return Err(StoreError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "compute job was not claimed",
            )));
        }
        let bytes =
            serde_json::to_vec(record).map_err(|error| StoreError::Json(error.to_string()))?;
        atomic_file::write(&path, &bytes)?;
        Ok(())
    }

    fn compute_job_path(&self, job_id: &[u8; 16]) -> PathBuf {
        self.root
            .join(COMPUTE_DIR)
            .join(format!("{}.json", hex::encode(job_id)))
    }

    /// Queue one encoded personal-compute envelope for an offline exact
    /// target. This queue is intentionally separate from frozen relay-v1
    /// envelopes so postcard enum evolution cannot corrupt either protocol.
    ///
    /// # Errors
    ///
    /// Returns an error if the target directory or crash-safe file cannot be
    /// created.
    pub fn enqueue_compute_pending(
        &self,
        device: &[u8; 32],
        sequence: u64,
        bytes: &[u8],
    ) -> Result<PathBuf, StoreError> {
        let directory = self
            .root
            .join(COMPUTE_PENDING_DIR)
            .join(hex::encode(device));
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{sequence:016x}.compute"));
        atomic_file::write(&path, bytes)?;
        Ok(path)
    }

    /// Load compute envelopes without acknowledging them. A session removes
    /// each returned path only after the matching frame reaches the wire.
    ///
    /// # Errors
    ///
    /// Returns an error if the queue cannot be enumerated or read.
    pub(crate) fn load_compute_pending(
        &self,
        device: &[u8; 32],
    ) -> Result<Vec<PendingEntry>, StoreError> {
        let directory = self
            .root
            .join(COMPUTE_PENDING_DIR)
            .join(hex::encode(device));
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut entries = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".compute"))
            .collect::<Vec<_>>();
        entries.sort_by_key(fs::DirEntry::file_name);
        entries
            .into_iter()
            .map(|entry| {
                let path = entry.path();
                Ok(PendingEntry {
                    bytes: fs::read(&path)?,
                    path,
                })
            })
            .collect()
    }

    /// Acknowledge one delivered compute envelope for the exact target.
    /// Missing entries are treated as an idempotent repeated acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns an error if an existing queue entry cannot be removed.
    pub fn ack_compute_pending(&self, device: &[u8; 32], sequence: u64) -> Result<(), StoreError> {
        let path = self
            .root
            .join(COMPUTE_PENDING_DIR)
            .join(hex::encode(device))
            .join(format!("{sequence:016x}.compute"));
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Append an audit log entry. Non-fatal if the log file fails —
    /// the audit trail is best-effort, forwarding must not depend
    /// on it.
    ///
    /// # Errors
    ///
    /// Returns an error if the log file can't be opened for append,
    /// `entry` can't be JSON-encoded, or the write fails.
    pub fn append_log(&self, entry: &LogEntry) -> Result<(), StoreError> {
        let path = self.root.join(LOG_FILE);
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES) {
            fs::rename(&path, self.root.join(format!("{LOG_FILE}.1")))?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let line = serde_json::to_string(entry).map_err(|e| StoreError::Json(e.to_string()))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Enqueue a delta for later delivery to `device_pubkey`. Each
    /// enqueue produces a new file `<seq>-<digest>.delta`; the caller
    /// supplies a strictly increasing `seq` per workspace, so filename order
    /// is delivery order and no two deltas share a file.
    ///
    /// Nothing is dropped here: once [`Self::pending_over_bounds`] reports
    /// the queue over [`MAX_PENDING_ENTRIES_PER_DEVICE`] or
    /// [`MAX_PENDING_BYTES_PER_DEVICE`], the caller compacts it with
    /// [`Self::compact_pending`].
    ///
    /// # Errors
    ///
    /// Returns an error if the per-device pending directory can't be
    /// created or the atomic write fails.
    pub fn enqueue_pending(
        &self,
        device_pubkey: &[u8; 32],
        seq: u64,
        bytes: &[u8],
    ) -> Result<PathBuf, StoreError> {
        #[cfg(test)]
        test_barrier::pause(&self.root, "pending-persist");
        let pending_dir = self.pending_dir_for(device_pubkey);
        fs::create_dir_all(&pending_dir)?;
        let filename = format!("{seq:016x}-{}.delta", blake3::hash(bytes).to_hex());
        let path = pending_dir.join(filename);
        // A crash mid-write must not leave a truncated delta for the peer to
        // receive. The temp file is process-scoped, so two writers filling the
        // same peer's queue cannot publish each other's half-written bytes —
        // the fixed `.partial` name this used to use could.
        atomic_file::write(&path, bytes)?;
        // A live fanout may now rely on this exact fallback. Require directory
        // durability instead of treating the atomic helper's best effort as a
        // successful publication barrier.
        for directory in pending_dir.ancestors().take(3) {
            fs::File::open(directory)?.sync_all()?;
        }
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(totals) = usage.get_mut(&pending_dir) {
            totals.entries = totals.entries.saturating_add(1);
            totals.bytes = totals.bytes.saturating_add(bytes.len() as u64);
        } else {
            // First write since start: the scan already includes the entry
            // just written.
            let totals = QueueUsage::of(&sorted_deltas(&pending_dir)?);
            usage.insert(pending_dir, totals);
        }
        drop(usage);
        Ok(path)
    }

    /// Record that `device` should be asked for a checkpoint.
    pub fn want_checkpoint(&self, device: [u8; 32]) {
        self.checkpoint_wants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(device);
    }

    /// Every device recorded by [`Self::want_checkpoint`] since the last
    /// call, forgetting them.
    pub fn take_checkpoint_wants(&self) -> Vec<[u8; 32]> {
        std::mem::take(
            &mut *self
                .checkpoint_wants
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_iter()
        .collect()
    }

    /// Whether `device_pubkey`'s pending queue is over its bounds (see
    /// [`QueueBounds`]). A running total that says so is confirmed against
    /// the directory first, since delivered entries leave it outside this
    /// store.
    ///
    /// # Errors
    ///
    /// Returns an error if the queue directory can't be listed.
    pub fn pending_over_bounds(&self, device_pubkey: &[u8; 32]) -> Result<bool, StoreError> {
        let dir = self.pending_dir_for(device_pubkey);
        let over = |totals: QueueUsage| {
            totals.entries > self.bounds.pending_entries || totals.bytes > self.bounds.pending_bytes
        };
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if usage.get(&dir).is_some_and(|totals| !over(*totals)) {
            return Ok(false);
        }
        let totals = QueueUsage::of(&sorted_deltas(&dir)?);
        usage.insert(dir, totals);
        drop(usage);
        Ok(over(totals))
    }

    /// Bring an over-bound pending queue back under its bounds.
    ///
    /// With `document` — one encoded delta carrying the pinned workspace's
    /// whole document — every queued Automerge change is replaced by that one
    /// entry, written as `<seq>.delta`. It holds every change it replaces, so
    /// the device receives no change whose dependencies were dropped. That
    /// entry counts as one entry but not as bytes: the entry bound holds
    /// exactly, while a document larger than the byte bound is not rewritten
    /// on every later enqueue.
    ///
    /// Without one (an unpinned workspace holds no authoritative state to
    /// replace history with) the oldest entries are dropped until the queue
    /// is under three quarters of both bounds; the same happens to queued
    /// entries that are not Automerge changes. Continuity entries, which
    /// carry their own delivery receipts, are never replaced or dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the queue can't be listed, read, written or
    /// trimmed.
    pub fn compact_pending(
        &self,
        device_pubkey: &[u8; 32],
        document: Option<(u64, &[u8])>,
    ) -> Result<(), StoreError> {
        let dir = self.pending_dir_for(device_pubkey);
        let (continuity, mut entries): (Vec<_>, Vec<_>) = sorted_deltas(&dir)?
            .into_iter()
            .partition(|(path, _)| is_continuity_entry(path));
        let mut replaced = 0u64;
        let mut documents = 0u64;
        if let Some((seq, document)) = document {
            let mut kept = Vec::with_capacity(entries.len());
            for (path, len) in entries {
                if is_automerge_delta(&fs::read(&path)?) {
                    remove_if_present(&path)?;
                    replaced = replaced.saturating_add(1);
                } else {
                    kept.push((path, len));
                }
            }
            atomic_file::write(&dir.join(format!("{seq:016x}.delta")), document)?;
            documents = 1;
            entries = kept;
        }
        let held = QueueUsage::of(&continuity);
        let mut remaining = QueueUsage::of(&entries);
        remaining.entries = remaining
            .entries
            .saturating_add(held.entries)
            .saturating_add(documents);
        remaining.bytes = remaining.bytes.saturating_add(held.bytes);
        let max_entries = self.bounds.pending_entries;
        let max_bytes = self.bounds.pending_bytes;
        let target_entries = max_entries.saturating_sub(max_entries / 4);
        let target_bytes = max_bytes.saturating_sub(max_bytes / 4);
        let mut dropped = 0u64;
        for (path, len) in entries {
            if remaining.entries <= target_entries && remaining.bytes <= target_bytes {
                break;
            }
            remove_if_present(&path)?;
            remaining.entries = remaining.entries.saturating_sub(1);
            remaining.bytes = remaining.bytes.saturating_sub(len);
            dropped = dropped.saturating_add(1);
        }
        tracing::warn!(
            workspace_id = self.workspace_id,
            queue = %dir.display(),
            replaced,
            dropped,
            "pending queue overflowed; compacted it"
        );
        self.usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(dir, remaining);
        Ok(())
    }

    pub(crate) fn enqueue_continuity_pending(
        &self,
        device: &[u8; 32],
        bytes: &[u8],
        receipt: &continuity::Receipt,
    ) -> Result<PathBuf, StoreError> {
        let directory = self.pending_dir_for(device);
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!(
            "{:016x}-continuity-{}.delta",
            receipt.sequence,
            hex::encode(rand::random::<[u8; 16]>())
        ));
        continuity::write(self, &path, bytes, receipt)?;
        atomic_file::write(&path, bytes)?;
        for directory in directory.ancestors().take(3) {
            fs::File::open(directory)?.sync_all()?;
        }
        Ok(path)
    }

    /// Eager helper; bounded storage paths must use replay cursors, not this
    /// loader.
    /// Load the pending queue in delivery order without acknowledging it.
    /// The peer task removes entries only after their replay is written.
    pub(crate) fn load_pending(
        &self,
        device_pubkey: &[u8; 32],
    ) -> Result<Vec<PendingEntry>, StoreError> {
        let dir = self.pending_dir_for(device_pubkey);
        // The caller acknowledges entries by removing their files, outside
        // this store; recount from disk on the next enqueue.
        self.usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&dir);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries: Vec<_> = fs::read_dir(&dir)?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let path = e.path();
            out.push(PendingEntry {
                bytes: fs::read(&path)?,
                path,
            });
        }
        Ok(out)
    }

    /// Drain the pending queue immediately. Primarily useful for offline
    /// maintenance and tests; live delivery uses `Self::load_pending` so a
    /// failed replay remains recoverable.
    ///
    /// # Errors
    ///
    /// Returns an error if loading the pending queue fails, or if a
    /// queued file can't be removed after being read.
    pub fn drain_pending(&self, device_pubkey: &[u8; 32]) -> Result<Vec<Vec<u8>>, StoreError> {
        let entries = self.load_pending(device_pubkey)?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            fs::remove_file(&entry.path)?;
            out.push(entry.bytes);
        }
        Ok(out)
    }

    /// Count pending deltas for `device_pubkey` without draining.
    ///
    /// # Errors
    ///
    /// Returns an error if the pending directory exists but can't be
    /// read.
    pub fn count_pending(&self, device_pubkey: &[u8; 32]) -> Result<u32, StoreError> {
        let dir = self.pending_dir_for(device_pubkey);
        if !dir.exists() {
            return Ok(0);
        }
        let n = fs::read_dir(&dir)?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count();
        // A count this large would mean a queue directory holding more
        // files than a `u32` can index; saturate rather than wrap so
        // the reported count never reads as smaller than the truth.
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Commit a checkpoint for the envelope's authenticated author/channel.
    /// Raw Automerge causal containment, or what the checkpoint itself names
    /// as covered ([`crate::coedit::Coverage`]), permits atomic replacement
    /// of covered history within that lineage; nothing else is inferred, so
    /// opaque checkpoints naming nothing are shared in receipt order.
    /// `bytes` is a full relay frame.
    ///
    /// # Errors
    /// Returns an error on invalid metadata or failed durable storage.
    pub fn retain_checkpoint(&self, channel: &[u8; 32], bytes: &[u8]) -> Result<(), StoreError> {
        #[cfg(test)]
        test_barrier::pause(&self.root, "state-persist");
        self.retain_shared(channel, None, bytes).map(|_| ())
    }

    /// Durably append a tail to the envelope's authenticated author/channel.
    /// A tail entry leaves replay only once a checkpoint of the same lineage
    /// covers it.
    ///
    /// # Errors
    /// Returns an error on invalid metadata or failed durable storage.
    pub fn retain_delta(
        &self,
        channel: &[u8; 32],
        seq: u64,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.retain_shared(channel, Some(seq), bytes).map(|_| ())
    }

    /// Eager compatibility helper for maintenance callers.
    /// New bounded replay and retention paths never call this method.
    /// Load retained replay coverage: proven raw baselines before uncovered
    /// tails, and opaque checkpoint histories in original receipt order.
    /// Documents are independent, so channel ordering is irrelevant; ordering
    /// within a channel is stable.
    ///
    /// # Errors
    ///
    /// Returns an error if a channel directory can't be listed, or a
    /// checkpoint / tail file can't be read.
    pub fn load_retained(&self) -> Result<Vec<Vec<u8>>, StoreError> {
        let mut out = self.load_shared()?;
        let mut seen = std::collections::HashSet::new();
        out.retain(|bytes| seen.insert(bytes.clone()));
        Ok(out)
    }

    /// One channel's retained envelopes in replay order: every publisher's
    /// checkpoint, each followed by its tail. Read-only.
    ///
    /// # Errors
    ///
    /// Returns an error if a retained generation or record can't be read.
    pub fn load_retained_channel(&self, channel: &[u8; 32]) -> Result<Vec<Vec<u8>>, StoreError> {
        let mut out = self.load_shared_channel(channel)?;
        let mut seen = std::collections::HashSet::new();
        out.retain(|bytes| seen.insert(bytes.clone()));
        Ok(out)
    }

    fn pending_dir_for(&self, device_pubkey: &[u8; 32]) -> PathBuf {
        self.root.join("pending").join(hex::encode(device_pubkey))
    }
}

fn remove_if_present(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Every Automerge chunk (document or change) starts with these bytes.
pub(crate) const AUTOMERGE_MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];

/// Whether an encoded pending envelope is a `Delta` carrying Automerge
/// bytes — a change the pinned document also holds.
fn is_automerge_delta(frame: &[u8]) -> bool {
    matches!(
        frame.get(4..).and_then(|payload| crate::frame::decode(payload).ok()),
        Some(crate::transport::Envelope::Delta { delta, .. }) if delta.starts_with(&AUTOMERGE_MAGIC)
    )
}

/// Whether a pending entry is a continuity delivery with its own receipt.
fn is_continuity_entry(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().contains("-continuity-"))
}

/// The `.delta` entries of `dir`, oldest first, with their sizes. A missing
/// directory is empty.
fn sorted_deltas(dir: &Path) -> Result<Vec<(PathBuf, u64)>, StoreError> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut entries: Vec<(std::ffi::OsString, PathBuf, u64)> = read
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".delta"))
        .filter_map(|entry| {
            let len = entry.metadata().ok()?.len();
            Some((entry.file_name(), entry.path(), len))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries
        .into_iter()
        .map(|(_, path, len)| (path, len))
        .collect())
}

/// Load-or-mint the relay's iroh node secret.
///
/// The secret is a 32-byte Ed25519-like key persisted at
/// `<state_root>/iroh-secret.bin` with owner-only access so the relay's
/// iroh node id is stable across restarts. An existing secret is never
/// replaced: only a secret that is absent is minted, and it is published
/// with an exclusive create, so relays starting at the same moment all
/// return the one secret that reached disk.
///
/// # Errors
///
/// Returns an error if an existing secret file cannot be read or is not
/// exactly 32 bytes, or if creating the state root or publishing a freshly
/// minted secret fails.
pub fn load_or_create_iroh_secret(state_root: &Path) -> Result<[u8; 32], StoreError> {
    use rand::Rng;

    let path = state_root.join("iroh-secret.bin");
    match read_iroh_secret(&path) {
        Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
        existing => return existing,
    }
    fs::create_dir_all(state_root)?;
    let mut secret = [0u8; 32];
    rand::rng().fill_bytes(&mut secret);
    // `false` means another starter published first; its secret is the one
    // every later start will read, so return that rather than ours.
    private_fs::create_atomic(&path, &secret)?;
    read_iroh_secret(&path)
}

fn read_iroh_secret(path: &Path) -> Result<[u8; 32], StoreError> {
    let bytes = fs::read(path)?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        StoreError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("iroh secret is {} bytes, want 32", bytes.len()),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_persists_layout_snapshots_logs_and_per_peer_pending_queues() {
        // Phase `open_creates_layout`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
            assert!(store.root().is_dir());
            assert!(store.root().join("pending").is_dir());
        }
        // Phase `snapshot_round_trip`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
            assert!(store.load_snapshot().unwrap().is_none());
            store.save_snapshot(b"HELLO").unwrap();
            assert_eq!(store.load_snapshot().unwrap().unwrap(), b"HELLO");
        }
        // Phase `snapshot_ignored_when_unpinned`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            store.save_snapshot(b"HELLO").unwrap();
            assert!(store.load_snapshot().unwrap().is_none());
        }
        // Phase `log_append`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
            store
                .append_log(&LogEntry {
                    workspace_id: 7,
                    received_at_ns: 1,
                    author_device_hex: "aa".repeat(32),
                    authored_at_ns: 0,
                    delta_len: 42,
                })
                .unwrap();
            let text = fs::read_to_string(store.root().join(LOG_FILE)).unwrap();
            assert!(text.contains("\"workspace_id\":7"));
        }
        // Phase `pending_queue_orderly_drain`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let peer = [3u8; 32];
            store.enqueue_pending(&peer, 100, b"one").unwrap();
            store.enqueue_pending(&peer, 200, b"two").unwrap();
            store.enqueue_pending(&peer, 300, b"three").unwrap();
            assert_eq!(store.count_pending(&peer).unwrap(), 3);
            let out = store.drain_pending(&peer).unwrap();
            assert_eq!(out.len(), 3);
            assert_eq!(out[0], b"one");
            assert_eq!(out[1], b"two");
            assert_eq!(out[2], b"three");
            // Empty after drain.
            assert_eq!(store.count_pending(&peer).unwrap(), 0);
        }
        // Phase `loading_pending_does_not_acknowledge_before_wire_delivery`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 8, false).unwrap();
            let peer = [4u8; 32];
            store.enqueue_pending(&peer, 100, b"one").unwrap();

            let loaded = store.load_pending(&peer).unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].bytes, b"one");
            assert_eq!(store.count_pending(&peer).unwrap(), 1);

            std::fs::remove_file(&loaded[0].path).unwrap();
            assert_eq!(store.count_pending(&peer).unwrap(), 0);
        }
        // Phase `pending_isolated_per_peer`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            store.enqueue_pending(&[1u8; 32], 100, b"a").unwrap();
            store.enqueue_pending(&[2u8; 32], 200, b"b").unwrap();
            let out1 = store.drain_pending(&[1u8; 32]).unwrap();
            assert_eq!(out1, vec![b"a".to_vec()]);
            let out2 = store.drain_pending(&[2u8; 32]).unwrap();
            assert_eq!(out2, vec![b"b".to_vec()]);
        }
    }

    /// A channel replay reads every publisher's retained state for that
    /// channel, checkpoint first, and nothing of any other channel. (How a
    /// tail is trimmed is `retained::tests`.)
    #[test]
    fn a_channel_replay_reads_every_publisher_of_only_its_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
        let (own_checkpoint, own_tail) = (retained(1, 9, true, 1), retained(1, 9, false, 2));
        let other_publisher = retained(2, 9, true, 3);
        let other_channel = retained(1, 8, true, 4);
        store.retain_checkpoint(&[9; 32], &own_checkpoint).unwrap();
        store.retain_delta(&[9; 32], 1, &own_tail).unwrap();
        store.retain_checkpoint(&[9; 32], &other_publisher).unwrap();
        store.retain_checkpoint(&[8; 32], &other_channel).unwrap();
        assert_eq!(
            store.load_retained_channel(&[9; 32]).unwrap(),
            vec![own_checkpoint, own_tail, other_publisher]
        );
        assert_eq!(
            store.load_retained_channel(&[8; 32]).unwrap(),
            vec![other_channel]
        );
        assert!(store.load_retained_channel(&[7; 32]).unwrap().is_empty());
    }

    fn small_bounds() -> QueueBounds {
        QueueBounds {
            pending_entries: 8,
            pending_bytes: 1024,
        }
    }

    /// Enqueue as an unpinned workspace does: compact by dropping the oldest.
    fn enqueue_bounded(store: &WorkspaceStore, peer: &[u8; 32], seq: u64, bytes: &[u8]) {
        store.enqueue_pending(peer, seq, bytes).unwrap();
        if store.pending_over_bounds(peer).unwrap() {
            store.compact_pending(peer, None).unwrap();
        }
    }

    #[test]
    fn pending_queue_drops_its_oldest_entries_beyond_the_entry_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), 9, false).unwrap();
        store.set_bounds(small_bounds());
        let peer = [5u8; 32];
        for seq in 0..9u64 {
            enqueue_bounded(&store, &peer, seq, format!("delta-{seq}").as_bytes());
        }
        // The ninth entry overflowed the bound of eight: the queue was cut
        // back to three quarters of it, oldest first.
        let kept = store.drain_pending(&peer).unwrap();
        assert_eq!(
            kept,
            (3..9u64)
                .map(|seq| format!("delta-{seq}").into_bytes())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pending_queue_drops_its_oldest_entries_beyond_the_byte_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), 9, false).unwrap();
        store.set_bounds(small_bounds());
        let peer = [6u8; 32];
        for seq in 0..3u64 {
            enqueue_bounded(&store, &peer, seq, &[u8::try_from(seq).unwrap(); 400]);
        }
        let kept = store.drain_pending(&peer).unwrap();
        assert_eq!(kept, vec![vec![2u8; 400]], "1200 bytes cut back under 768");
    }

    #[test]
    fn pending_bound_counts_entries_already_on_disk_and_forgets_acknowledged_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let peer = [7u8; 32];
        {
            let store = WorkspaceStore::open(tmp.path(), 10, false).unwrap();
            for seq in 0..6u64 {
                enqueue_bounded(&store, &peer, seq, b"x");
            }
        }
        // A restarted relay counts the six queued before it started.
        let mut store = WorkspaceStore::open(tmp.path(), 10, false).unwrap();
        store.set_bounds(small_bounds());
        for seq in 6..9u64 {
            enqueue_bounded(&store, &peer, seq, b"x");
        }
        assert_eq!(store.count_pending(&peer).unwrap(), 6);
        // A delivered replay acknowledges by removing files; the next
        // enqueue must not evict against the stale count.
        for entry in store.load_pending(&peer).unwrap() {
            std::fs::remove_file(entry.path).unwrap();
        }
        for seq in 9..16u64 {
            enqueue_bounded(&store, &peer, seq, b"x");
        }
        assert_eq!(store.count_pending(&peer).unwrap(), 7);
    }

    #[test]
    fn audit_log_rotates_instead_of_growing_without_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 12, false).unwrap();
        let log = store.root().join(LOG_FILE);
        std::fs::write(&log, vec![b'x'; usize::try_from(MAX_LOG_BYTES).unwrap()]).unwrap();
        store
            .append_log(&LogEntry {
                workspace_id: 12,
                received_at_ns: 1,
                author_device_hex: String::new(),
                authored_at_ns: 1,
                delta_len: 1,
            })
            .unwrap();
        assert_eq!(
            std::fs::metadata(store.root().join(format!("{LOG_FILE}.1")))
                .unwrap()
                .len(),
            MAX_LOG_BYTES
        );
        assert!(std::fs::metadata(&log).unwrap().len() < 1024);
    }

    #[test]
    fn iroh_secret_is_minted_once_and_never_replaced() {
        // Phase `iroh_secret_stable_across_calls`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let a = load_or_create_iroh_secret(tmp.path()).unwrap();
            let b = load_or_create_iroh_secret(tmp.path()).unwrap();
            assert_eq!(a, b);
            assert_ne!(a, [0u8; 32]);
        }
        // Phase `iroh_secret_is_never_replaced`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("iroh-secret.bin");
            std::fs::write(&path, [7u8; 32]).unwrap();
            assert_eq!(load_or_create_iroh_secret(tmp.path()).unwrap(), [7u8; 32]);
            // A malformed secret is an error, not a reason to mint a new identity.
            std::fs::write(&path, b"short").unwrap();
            assert!(load_or_create_iroh_secret(tmp.path()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"short");
        }
        // Phase `unreadable_iroh_secret_is_never_replaced`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("iroh-secret.bin");
            // A directory in the secret's place fails every read, whatever the
            // caller's privileges (a mode-000 file is still readable by root).
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("marker"), b"kept").unwrap();
            assert!(load_or_create_iroh_secret(tmp.path()).is_err());
            assert!(path.is_dir(), "the unreadable secret must not be replaced");
            assert_eq!(std::fs::read(path.join("marker")).unwrap(), b"kept");
            assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
        }
        // Phase `simultaneous_starts_share_one_iroh_secret`.
        {
            for _ in 0..8 {
                let tmp = tempfile::tempdir().unwrap();
                let root = tmp.path().to_path_buf();
                let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
                // Spawn every participant before joining: interleaving spawn/join
                // would deadlock the barrier.
                let mut workers = Vec::with_capacity(4);
                for _ in 0..4 {
                    let root = root.clone();
                    let gate = std::sync::Arc::clone(&gate);
                    workers.push(std::thread::spawn(move || {
                        gate.wait();
                        load_or_create_iroh_secret(&root).unwrap()
                    }));
                }
                let secrets: Vec<_> = workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .collect();
                let persisted = std::fs::read(root.join("iroh-secret.bin")).unwrap();
                assert!(secrets.iter().all(|secret| secret.as_slice() == persisted));
                assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn minted_iroh_secret_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        load_or_create_iroh_secret(tmp.path()).unwrap();
        let mode = std::fs::metadata(tmp.path().join("iroh-secret.bin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn governed_access_is_one_authoritative_device_and_root_record() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
        store.save_membership(&[[9; 32]]).unwrap();
        store
            .save_governed_access(&GovernedAccessRecord {
                member_devices: vec![[1; 32]],
                member_user_roots: vec![[2; 32]],
            })
            .unwrap();

        assert_eq!(store.load_membership().unwrap(), vec![[1; 32]]);
        assert_eq!(
            store.load_governed_access().unwrap().unwrap(),
            GovernedAccessRecord {
                member_devices: vec![[1; 32]],
                member_user_roots: vec![[2; 32]],
            }
        );
        store.save_membership(&[[3; 32]]).unwrap();
        assert_eq!(
            store.load_governed_access().unwrap().unwrap(),
            GovernedAccessRecord {
                member_devices: vec![[3; 32]],
                member_user_roots: vec![[2; 32]],
            },
            "device-only membership writers must preserve the governed root half"
        );
        assert!(store.root().join(GOVERNED_ACCESS_FILE).exists());
    }

    fn retained(author: u8, channel: u8, checkpoint: bool, value: u8) -> Vec<u8> {
        let payload = if checkpoint {
            crate::coedit::CoeditPayload::checkpoint_of([channel; 32], vec![value])
        } else {
            crate::coedit::CoeditPayload::retained_of([channel; 32], vec![value])
        };
        crate::transport::encode(&crate::transport::Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [author; 32],
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap()
    }

    #[test]
    fn retained_replacement_preserves_other_publishers_channels_and_unproven_history() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let a1 = retained(1, 9, true, 1);
        let a2 = retained(1, 9, true, 2);
        let b = retained(2, 9, true, 1);
        let c = retained(1, 8, true, 1);
        store.retain_checkpoint(&[9; 32], &a1).unwrap();
        store
            .retain_delta(&[9; 32], 10, &retained(1, 9, false, 5))
            .unwrap();
        store.retain_checkpoint(&[9; 32], &b).unwrap();
        store.retain_checkpoint(&[8; 32], &c).unwrap();
        store.retain_checkpoint(&[9; 32], &a2).unwrap();
        let actual = store.load_retained().unwrap();
        assert_eq!(actual.len(), 5);
        for expected in [a1, a2, b, c, retained(1, 9, false, 5)] {
            assert!(actual.contains(&expected));
        }
    }

    #[test]
    fn retry_of_current_checkpoint_does_not_erase_later_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let cp = retained(1, 9, true, 1);
        let tail = retained(1, 9, false, 1);
        store.retain_checkpoint(&[9; 32], &cp).unwrap();
        store.retain_delta(&[9; 32], 10, &tail).unwrap();
        store.retain_checkpoint(&[9; 32], &cp).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![cp, tail]);
    }
}
