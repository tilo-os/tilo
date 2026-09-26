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
//!         └── pending/
//!             └── <device_pubkey_hex>/
//!                 └── <seq>.delta   # queued deltas for offline peers
//! ```
//!
//! `snapshot.automerge` is only written when a workspace is
//! `pin = true` in the config; unpinned workspaces are pure fan-out
//! + store-and-forward. Pending directories exist regardless of
//!   pin state — a peer can be offline and still catch up when it reconnects.
//!
//! Every queue here is bounded so one busy room cannot fill the disk:
//! a device's pending queue ([`MAX_PENDING_ENTRIES_PER_DEVICE`],
//! [`MAX_PENDING_BYTES_PER_DEVICE`]), a channel's retained tail
//! ([`MAX_RETAINED_TAIL_ENTRIES`], [`MAX_RETAINED_TAIL_BYTES`]) and the audit
//! log ([`MAX_LOG_BYTES`]). Overflow drops the oldest entries: a pending
//! delta is also in the pinned document that anti-entropy serves, and a
//! retained tail is superseded by the next checkpoint a member publishes.

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

/// Retained bootstrap checkpoints and the incremental tails after them.
pub const RETAINED_DIR: &str = "retained";

/// Most queued deltas one offline device may hold. Retained document
/// traffic never enters the per-device queue (the retained store replays
/// it), so this bounds only workspace-directory deltas and federation
/// traffic, which the pinned document also serves through anti-entropy.
pub const MAX_PENDING_ENTRIES_PER_DEVICE: u64 = 4096;
/// Most queued bytes one offline device may hold.
pub const MAX_PENDING_BYTES_PER_DEVICE: u64 = 64 * 1024 * 1024;
/// Most incremental deltas retained after one channel's checkpoint. Members
/// publish a fresh checkpoint whenever they (re)attach to the host, which
/// resets the tail; the bound only matters for a channel nobody checkpoints.
pub const MAX_RETAINED_TAIL_ENTRIES: u64 = 16_384;
/// Most bytes retained after one channel's checkpoint.
pub const MAX_RETAINED_TAIL_BYTES: u64 = 256 * 1024 * 1024;
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
pub struct WorkspaceStore {
    root: PathBuf,
    workspace_id: u64,
    pinned: bool,
    /// Entry and byte totals of the bounded directories written through this
    /// store, keyed by directory. Filled from disk on first use and
    /// invalidated whenever entries may be removed from outside the store
    /// (a replay acknowledgement), so it never under-counts a queue.
    usage: std::sync::Mutex<std::collections::HashMap<PathBuf, QueueUsage>>,
    bounds: QueueBounds,
}

/// Bounds on the per-device pending queues and per-channel retained tails.
/// Production uses [`QueueBounds::default`]; tests shrink them to exercise
/// the same eviction path without writing thousands of files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueBounds {
    /// See [`MAX_PENDING_ENTRIES_PER_DEVICE`].
    pub pending_entries: u64,
    /// See [`MAX_PENDING_BYTES_PER_DEVICE`].
    pub pending_bytes: u64,
    /// See [`MAX_RETAINED_TAIL_ENTRIES`].
    pub tail_entries: u64,
    /// See [`MAX_RETAINED_TAIL_BYTES`].
    pub tail_bytes: u64,
}

impl Default for QueueBounds {
    fn default() -> Self {
        Self {
            pending_entries: MAX_PENDING_ENTRIES_PER_DEVICE,
            pending_bytes: MAX_PENDING_BYTES_PER_DEVICE,
            tail_entries: MAX_RETAINED_TAIL_ENTRIES,
            tail_bytes: MAX_RETAINED_TAIL_BYTES,
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
    fn of(entries: &[(PathBuf, u64)]) -> Self {
        Self {
            entries: entries.len() as u64,
            bytes: entries.iter().map(|(_, len)| *len).sum(),
        }
    }
}

/// Identity of the membership file as last loaded: every rewrite is an
/// atomic rename, so a new inode (or size, or mtime) means new contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipStamp {
    inode: u64,
    len: u64,
    modified: Option<std::time::SystemTime>,
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
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("pending"))?;
        fs::create_dir_all(root.join(RETAINED_DIR).join("checkpoints"))?;
        fs::create_dir_all(root.join(RETAINED_DIR).join("tails"))?;
        fs::create_dir_all(root.join(COMPUTE_DIR))?;
        fs::create_dir_all(root.join(COMPUTE_PENDING_DIR))?;
        Ok(Self {
            root,
            workspace_id,
            pinned,
            usage: std::sync::Mutex::new(std::collections::HashMap::new()),
            bounds: QueueBounds::default(),
        })
    }

    /// Replace the queue bounds (see [`QueueBounds`]).
    pub fn set_bounds(&mut self, bounds: QueueBounds) {
        self.bounds = bounds;
    }

    /// Root directory for this workspace's state.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Workspace id.
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
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
        let path = self.root.join(MEMBERSHIP_FILE);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let encoded: Vec<String> = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| StoreError::Json(error.to_string()))?;
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

    /// Identity of the membership file's current contents, `None` when it
    /// does not exist. Lets a caller that polls membership skip re-reading
    /// and re-parsing an unchanged file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but its metadata can't be read.
    pub fn membership_stamp(&self) -> Result<Option<MembershipStamp>, StoreError> {
        match fs::metadata(self.root.join(MEMBERSHIP_FILE)) {
            Ok(metadata) => {
                #[cfg(unix)]
                let inode = std::os::unix::fs::MetadataExt::ino(&metadata);
                #[cfg(not(unix))]
                let inode = 0;
                Ok(Some(MembershipStamp {
                    inode,
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Atomically replace the durable membership set.
    ///
    /// # Errors
    ///
    /// Returns an error if `devices` can't be JSON-encoded or the
    /// atomic write fails.
    pub fn save_membership(&self, devices: &[[u8; 32]]) -> Result<(), StoreError> {
        let encoded: Vec<_> = devices.iter().map(hex::encode).collect();
        let bytes = serde_json::to_vec_pretty(&encoded)
            .map_err(|error| StoreError::Json(error.to_string()))?;
        atomic_file::write(&self.root.join(MEMBERSHIP_FILE), &bytes)?;
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
    /// enqueue produces a new file `<seq>.delta`; the caller supplies a
    /// strictly increasing `seq` per workspace, so filename order is
    /// delivery order and no two deltas share a file.
    ///
    /// The queue is bounded by [`MAX_PENDING_ENTRIES_PER_DEVICE`] and
    /// [`MAX_PENDING_BYTES_PER_DEVICE`]; beyond either the oldest entries
    /// are dropped (see the module docs for why that is recoverable).
    ///
    /// # Errors
    ///
    /// Returns an error if the per-device pending directory can't be
    /// created, the atomic write fails, or an evicted entry can't be removed.
    pub fn enqueue_pending(
        &self,
        device_pubkey: &[u8; 32],
        seq: u64,
        bytes: &[u8],
    ) -> Result<PathBuf, StoreError> {
        let pending_dir = self.pending_dir_for(device_pubkey);
        fs::create_dir_all(&pending_dir)?;
        let filename = format!("{seq:016x}.delta");
        let path = pending_dir.join(filename);
        // A crash mid-write must not leave a truncated delta for the peer to
        // receive. The temp file is process-scoped, so two writers filling the
        // same peer's queue cannot publish each other's half-written bytes —
        // the fixed `.partial` name this used to use could.
        atomic_file::write(&path, bytes)?;
        self.account_and_bound(
            &pending_dir,
            bytes.len() as u64,
            self.bounds.pending_entries,
            self.bounds.pending_bytes,
        )?;
        Ok(path)
    }

    /// Add one just-written entry of `len` bytes to `dir`'s totals and, if
    /// either bound is now exceeded, remove the oldest `.delta` entries until
    /// the queue is back under three quarters of both bounds. Evicting a
    /// batch keeps the directory scan off the per-delta path.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the totals stay locked through eviction so a concurrent writer cannot count against a queue being trimmed"
    )]
    fn account_and_bound(
        &self,
        dir: &Path,
        len: u64,
        max_entries: u64,
        max_bytes: u64,
    ) -> Result<(), StoreError> {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let totals = if let Some(totals) = usage.get_mut(dir) {
            totals.entries = totals.entries.saturating_add(1);
            totals.bytes = totals.bytes.saturating_add(len);
            *totals
        } else {
            // First write since start (or since an external removal): the
            // scan already includes the entry just written.
            let totals = QueueUsage::of(&sorted_deltas(dir)?);
            usage.insert(dir.to_path_buf(), totals);
            totals
        };
        if totals.entries <= max_entries && totals.bytes <= max_bytes {
            return Ok(());
        }
        let entries = sorted_deltas(dir)?;
        let mut remaining = QueueUsage::of(&entries);
        let target_entries = max_entries.saturating_sub(max_entries / 4);
        let target_bytes = max_bytes.saturating_sub(max_bytes / 4);
        let mut dropped = 0u64;
        for (path, len) in entries {
            if remaining.entries <= target_entries && remaining.bytes <= target_bytes {
                break;
            }
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            remaining.entries = remaining.entries.saturating_sub(1);
            remaining.bytes = remaining.bytes.saturating_sub(len);
            dropped = dropped.saturating_add(1);
        }
        tracing::warn!(
            workspace_id = self.workspace_id,
            queue = %dir.display(),
            dropped,
            "bounded relay queue overflowed; dropped its oldest entries"
        );
        usage.insert(dir.to_path_buf(), remaining);
        Ok(())
    }

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

    /// Replace the durable checkpoint for `channel` and discard the tail that
    /// preceded it. `bytes` is a complete encoded relay envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails, the old tail
    /// directory can't be removed, or the fresh tail directory can't
    /// be created.
    pub fn retain_checkpoint(&self, channel: &[u8; 32], bytes: &[u8]) -> Result<(), StoreError> {
        let name = hex::encode(channel);
        atomic_file::write(
            &self.root.join(RETAINED_DIR).join("checkpoints").join(&name),
            bytes,
        )?;
        let tail = self.root.join(RETAINED_DIR).join("tails").join(name);
        if tail.exists() {
            fs::remove_dir_all(&tail)?;
        }
        fs::create_dir_all(&tail)?;
        self.usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(tail, QueueUsage::default());
        Ok(())
    }

    /// Append one encoded relay envelope to the retained tail for `channel`,
    /// bounded by [`MAX_RETAINED_TAIL_ENTRIES`] and
    /// [`MAX_RETAINED_TAIL_BYTES`] (oldest dropped first).
    ///
    /// # Errors
    ///
    /// Returns an error if the tail directory can't be created, the
    /// atomic write fails, or an evicted entry can't be removed.
    pub fn retain_delta(
        &self,
        channel: &[u8; 32],
        seq: u64,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let tail = self
            .root
            .join(RETAINED_DIR)
            .join("tails")
            .join(hex::encode(channel));
        fs::create_dir_all(&tail)?;
        atomic_file::write(&tail.join(format!("{seq:016x}.delta")), bytes)?;
        self.account_and_bound(
            &tail,
            bytes.len() as u64,
            self.bounds.tail_entries,
            self.bounds.tail_bytes,
        )?;
        Ok(())
    }

    /// Load every retained checkpoint followed by its post-checkpoint tail.
    /// Documents are independent, so channel ordering is irrelevant; ordering
    /// within a channel is stable.
    ///
    /// # Errors
    ///
    /// Returns an error if a channel directory can't be listed, or a
    /// checkpoint / tail file can't be read.
    pub fn load_retained(&self) -> Result<Vec<Vec<u8>>, StoreError> {
        let retained = self.root.join(RETAINED_DIR);
        let checkpoint_dir = retained.join("checkpoints");
        let tails_dir = retained.join("tails");
        let mut channels = std::collections::BTreeSet::new();
        for dir in [&checkpoint_dir, &tails_dir] {
            for entry in fs::read_dir(dir)?.flatten() {
                channels.insert(entry.file_name());
            }
        }
        let mut out = Vec::new();
        for channel in channels {
            let checkpoint = checkpoint_dir.join(&channel);
            if checkpoint.is_file() {
                out.push(fs::read(checkpoint)?);
            }
            let tail = tails_dir.join(&channel);
            if tail.is_dir() {
                let mut entries: Vec<_> = fs::read_dir(tail)?.flatten().collect();
                entries.sort_by_key(std::fs::DirEntry::file_name);
                for entry in entries {
                    if entry.path().is_file() {
                        out.push(fs::read(entry.path())?);
                    }
                }
            }
        }
        Ok(out)
    }

    fn pending_dir_for(&self, device_pubkey: &[u8; 32]) -> PathBuf {
        self.root.join("pending").join(hex::encode(device_pubkey))
    }
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
    fn open_creates_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
        assert!(store.root().is_dir());
        assert!(store.root().join("pending").is_dir());
    }

    #[test]
    fn snapshot_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
        assert!(store.load_snapshot().unwrap().is_none());
        store.save_snapshot(b"HELLO").unwrap();
        assert_eq!(store.load_snapshot().unwrap().unwrap(), b"HELLO");
    }

    #[test]
    fn snapshot_ignored_when_unpinned() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        store.save_snapshot(b"HELLO").unwrap();
        assert!(store.load_snapshot().unwrap().is_none());
    }

    #[test]
    fn pending_queue_orderly_drain() {
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

    #[test]
    fn loading_pending_does_not_acknowledge_before_wire_delivery() {
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

    #[test]
    fn pending_isolated_per_peer() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        store.enqueue_pending(&[1u8; 32], 100, b"a").unwrap();
        store.enqueue_pending(&[2u8; 32], 200, b"b").unwrap();
        let out1 = store.drain_pending(&[1u8; 32]).unwrap();
        assert_eq!(out1, vec![b"a".to_vec()]);
        let out2 = store.drain_pending(&[2u8; 32]).unwrap();
        assert_eq!(out2, vec![b"b".to_vec()]);
    }

    #[test]
    fn retained_checkpoint_replaces_its_old_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
        let channel = [9u8; 32];
        store.retain_checkpoint(&channel, b"checkpoint-1").unwrap();
        store.retain_delta(&channel, 10, b"delta-1").unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![b"checkpoint-1".to_vec(), b"delta-1".to_vec()]
        );
        store.retain_checkpoint(&channel, b"checkpoint-2").unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![b"checkpoint-2".to_vec()]
        );
    }

    fn small_bounds() -> QueueBounds {
        QueueBounds {
            pending_entries: 8,
            pending_bytes: 1024,
            tail_entries: 8,
            tail_bytes: 1024,
        }
    }

    #[test]
    fn pending_queue_drops_its_oldest_entries_beyond_the_entry_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), 9, false).unwrap();
        store.set_bounds(small_bounds());
        let peer = [5u8; 32];
        for seq in 0..9u64 {
            store
                .enqueue_pending(&peer, seq, format!("delta-{seq}").as_bytes())
                .unwrap();
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
            store
                .enqueue_pending(&peer, seq, &[u8::try_from(seq).unwrap(); 400])
                .unwrap();
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
                store.enqueue_pending(&peer, seq, b"x").unwrap();
            }
        }
        // A restarted relay counts the six queued before it started.
        let mut store = WorkspaceStore::open(tmp.path(), 10, false).unwrap();
        store.set_bounds(small_bounds());
        for seq in 6..9u64 {
            store.enqueue_pending(&peer, seq, b"x").unwrap();
        }
        assert_eq!(store.count_pending(&peer).unwrap(), 6);
        // A delivered replay acknowledges by removing files; the next
        // enqueue must not evict against the stale count.
        for entry in store.load_pending(&peer).unwrap() {
            std::fs::remove_file(entry.path).unwrap();
        }
        for seq in 9..16u64 {
            store.enqueue_pending(&peer, seq, b"x").unwrap();
        }
        assert_eq!(store.count_pending(&peer).unwrap(), 7);
    }

    #[test]
    fn retained_tail_is_bounded_and_a_checkpoint_resets_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), 11, true).unwrap();
        store.set_bounds(small_bounds());
        let channel = [3u8; 32];
        store.retain_checkpoint(&channel, b"checkpoint").unwrap();
        for seq in 0..9u64 {
            store
                .retain_delta(&channel, seq, format!("tail-{seq}").as_bytes())
                .unwrap();
        }
        let retained = store.load_retained().unwrap();
        assert_eq!(
            retained[0], b"checkpoint",
            "the checkpoint is never evicted"
        );
        assert_eq!(
            retained[1..].to_vec(),
            (3..9u64)
                .map(|seq| format!("tail-{seq}").into_bytes())
                .collect::<Vec<_>>()
        );
        store.retain_checkpoint(&channel, b"checkpoint-2").unwrap();
        for seq in 10..18u64 {
            store.retain_delta(&channel, seq, b"t").unwrap();
        }
        assert_eq!(
            store.load_retained().unwrap().len(),
            9,
            "eight tail entries fit after a fresh checkpoint"
        );
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
    fn iroh_secret_stable_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let a = load_or_create_iroh_secret(tmp.path()).unwrap();
        let b = load_or_create_iroh_secret(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, [0u8; 32]);
    }

    #[test]
    fn iroh_secret_is_never_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("iroh-secret.bin");
        std::fs::write(&path, [7u8; 32]).unwrap();
        assert_eq!(load_or_create_iroh_secret(tmp.path()).unwrap(), [7u8; 32]);
        // A malformed secret is an error, not a reason to mint a new identity.
        std::fs::write(&path, b"short").unwrap();
        assert!(load_or_create_iroh_secret(tmp.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
    }

    #[test]
    fn unreadable_iroh_secret_is_never_replaced() {
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

    #[test]
    fn simultaneous_starts_share_one_iroh_secret() {
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
    fn log_append() {
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
}
