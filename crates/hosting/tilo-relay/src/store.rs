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

pub(crate) mod bounded;
pub(crate) mod compaction;
pub(crate) mod continuity;
pub(crate) mod replay;
pub(crate) mod resume;
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

/// Durable device membership for offline store-and-forward.
pub const MEMBERSHIP_FILE: &str = "membership.json";
/// Atomic native-governance projection: exact room devices plus handshake roots.
pub const GOVERNED_ACCESS_FILE: &str = "governed-access-v1.json";

/// Retained bootstrap checkpoints and the incremental tails after them.
pub const RETAINED_DIR: &str = "retained";
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
    atomic_write(&state_root.join(REVOCATIONS_FILE), &bytes)?;
    Ok(())
}

/// fsyncs the relay's workspace stores issued since this process started.
static FSYNCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    /// [`FSYNCS`] of this thread only: tests run in parallel, and a store
    /// driven synchronously issues its barriers on the calling thread.
    static THREAD_FSYNCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `tilo_files::native::atomic::write` syncs the staged file, and after the
/// rename the directory holding it.
const ATOMIC_WRITE_FSYNCS: u64 = 2;

fn count_fsyncs(count: u64) {
    FSYNCS.fetch_add(count, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    THREAD_FSYNCS.with(|fsyncs| fsyncs.set(fsyncs.get().saturating_add(count)));
}

/// fsyncs (file and directory barriers) the relay's workspace stores have
/// issued in this process: the cost the hosting bench divides by edits.
#[must_use]
pub fn fsyncs() -> u64 {
    FSYNCS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn thread_fsyncs() -> u64 {
    THREAD_FSYNCS.with(std::cell::Cell::get)
}

/// `file.sync_all()`, counted in [`fsyncs`].
pub(crate) fn fsync(file: &fs::File) -> io::Result<()> {
    count_fsyncs(1);
    file.sync_all()
}

/// `tilo_files::native::atomic::write`, counted in [`fsyncs`].
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    count_fsyncs(ATOMIC_WRITE_FSYNCS);
    tilo_files::native::atomic::write(path, bytes)
}

/// Errors from store operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// JSON encode/decode failure for a metadata file.
    #[error("json: {0}")]
    Json(String),
}

/// Ledger of admitted `Commit`s, see [`CommitRecord`].
pub const COMMITS_FILE: &str = "commits.bin";

const COMMIT_RECORD_BYTES: usize = 32 + 16 + 8 + 32;

/// One commit the host admitted: resending the same `(author, op)` is
/// answered from here instead of being persisted and fanned out again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitRecord {
    /// Authoring device.
    pub author: [u8; 32],
    /// The author's operation id.
    pub op: [u8; 16],
    /// Host order it was admitted at.
    pub seq: u64,
    /// BLAKE3 of the committed body.
    pub digest: [u8; 32],
}

impl CommitRecord {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(COMMIT_RECORD_BYTES);
        out.extend_from_slice(&self.author);
        out.extend_from_slice(&self.op);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.digest);
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            author: bytes.get(..32)?.try_into().ok()?,
            op: bytes.get(32..48)?.try_into().ok()?,
            seq: u64::from_le_bytes(bytes.get(48..56)?.try_into().ok()?),
            digest: bytes.get(56..COMMIT_RECORD_BYTES)?.try_into().ok()?,
        })
    }
}

/// On-disk manager for one workspace's state.
#[derive(Clone)]
pub struct WorkspaceStore {
    root: PathBuf,
    workspace_id: u64,
    pinned: bool,
    shell_authority: Option<tilo_peer_state::retained_proof::RetentionAuthority>,
    /// Shared by every handle on `root`; see `shell_selection::tail_cache`.
    tail_cache: shell_selection::tail_cache::SharedTailCache,
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

/// Older relays appended one JSON line per admitted delta to this file and
/// never read it back, so it only grew (~170 B per keystroke). Nothing writes
/// it any more; [`WorkspaceStore::open`] reclaims what an older relay left.
const LEGACY_DELTA_LOG: &str = "log.jsonl";

fn remove_legacy_delta_log(root: &Path) {
    match fs::remove_file(root.join(LEGACY_DELTA_LOG)) {
        Ok(()) => {
            tracing::info!(root = %root.display(), "removed the legacy per-delta log");
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        // Best effort: the file is inert, and opening the store must not
        // depend on reclaiming it.
        Err(error) => {
            tracing::warn!(root = %root.display(), %error, "legacy per-delta log not removed");
        }
    }
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
        remove_legacy_delta_log(&root);
        Ok(Self {
            tail_cache: shell_selection::tail_cache::SharedTailCache::for_root(&root),
            root,
            workspace_id,
            pinned,
            shell_authority: None,
        })
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
                fsync(&fs::File::open(&path)?)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let bytes = serde_json::to_vec(&Binding {
                    version: 1,
                    authority: authority.clone(),
                })
                .map_err(|e| StoreError::Json(e.to_string()))?;
                atomic_write(&path, &bytes)?;
            }
            Err(error) => return Err(error.into()),
        }
        fsync(&fs::File::open(&self.root)?)?;
        self.shell_authority = Some(authority);
        self.tail_cache.invalidate();
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
        atomic_write(&self.root.join(SNAPSHOT_FILE), bytes)?;
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
        atomic_write(&self.root.join(MEMBERSHIP_FILE), &bytes)?;
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
        atomic_write(&self.root.join(GOVERNED_ACCESS_FILE), &bytes)?;
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
                fsync(&file)?;
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
        atomic_write(&path, &bytes)?;
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
        atomic_write(&path, bytes)?;
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

    /// Enqueue a delta for later delivery to `device_pubkey`. Each
    /// enqueue produces a new file `<seq>.delta` where seq is a
    /// monotonically increasing hex nanosecond timestamp; the
    /// filename ordering is the delivery ordering.
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
        atomic_write(&path, bytes)?;
        // A live fanout may now rely on this exact fallback.
        Self::pending_durable(&pending_dir)?;
        Ok(path)
    }

    /// Complete the barrier for a file `atomic_write` just published in the
    /// queue `directory`: it synced `directory` after the rename (Apple
    /// repeats that with F_FULLFSYNC, as `retained::durable_write` does).
    /// The queue's two ancestors change only when a queue is first created,
    /// so they are synced whenever their entries changed since this process
    /// last synced them.
    fn pending_durable(directory: &Path) -> Result<(), StoreError> {
        #[cfg(target_vendor = "apple")]
        fsync(&fs::File::open(directory)?)?;
        for ancestor in directory.ancestors().skip(1).take(2) {
            retained::sync_dir_once(ancestor)?;
        }
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
        atomic_write(&path, bytes)?;
        Self::pending_durable(&directory)?;
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
    /// Raw Automerge causal containment permits atomic replacement of covered
    /// history. Opaque checkpoints are shared in receipt order without inferring
    /// supersedence. `bytes` is a full relay frame.
    ///
    /// # Errors
    /// Returns an error on invalid metadata or failed durable storage.
    pub fn retain_checkpoint(&self, channel: &[u8; 32], bytes: &[u8]) -> Result<(), StoreError> {
        #[cfg(test)]
        test_barrier::pause(&self.root, "state-persist");
        self.retain_shared(channel, None, bytes).map(|_| ())
    }

    /// Durably append a tail to the envelope's authenticated author/channel.
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

    /// Load the admitted-commit ledger, oldest first. A record torn by a
    /// crash mid-append is cut off, so later appends stay aligned.
    ///
    /// # Errors
    ///
    /// Returns an error if the ledger exists but can't be read or trimmed.
    pub fn load_commits(&self) -> Result<Vec<CommitRecord>, StoreError> {
        let path = self.root.join(COMMITS_FILE);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&path)?;
        let torn = bytes.len() % COMMIT_RECORD_BYTES;
        if torn != 0 {
            fs::OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len((bytes.len().saturating_sub(torn)) as u64)?;
        }
        Ok(bytes
            .chunks_exact(COMMIT_RECORD_BYTES)
            .filter_map(CommitRecord::decode)
            .collect())
    }

    /// Append one admitted commit to the ledger. Not synced: losing the
    /// newest records to a power cut only lets a resend of those commits be
    /// admitted again, which the document merge absorbs.
    ///
    /// # Errors
    ///
    /// Returns an error if the ledger can't be opened or written.
    pub fn append_commit(&self, record: &CommitRecord) -> Result<(), StoreError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(COMMITS_FILE))?;
        file.write_all(&record.encode())?;
        Ok(())
    }

    /// Atomically replace the ledger with `records`, dropping evicted ones.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails.
    pub fn rewrite_commits(&self, records: &[CommitRecord]) -> Result<(), StoreError> {
        let mut bytes = Vec::with_capacity(records.len().saturating_mul(COMMIT_RECORD_BYTES));
        for record in records {
            bytes.extend_from_slice(&record.encode());
        }
        atomic_write(&self.root.join(COMMITS_FILE), &bytes)?;
        Ok(())
    }

    fn pending_dir_for(&self, device_pubkey: &[u8; 32]) -> PathBuf {
        self.root.join("pending").join(hex::encode(device_pubkey))
    }
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
    fn commit_ledger_round_trips_and_ignores_a_torn_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
        let record = |n: u8| CommitRecord {
            author: [n; 32],
            op: [n; 16],
            seq: u64::from(n) * 10,
            digest: [n.wrapping_add(1); 32],
        };
        store.append_commit(&record(1)).unwrap();
        store.append_commit(&record(2)).unwrap();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(store.root().join(COMMITS_FILE))
            .unwrap();
        file.write_all(&[9; 40]).unwrap();
        assert_eq!(store.load_commits().unwrap(), vec![record(1), record(2)]);
        store.append_commit(&record(3)).unwrap();
        assert_eq!(
            store.load_commits().unwrap(),
            vec![record(1), record(2), record(3)],
            "an append after a torn record stays aligned"
        );
        store.rewrite_commits(&[record(2)]).unwrap();
        assert_eq!(store.load_commits().unwrap(), vec![record(2)]);
    }

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
        // Phase `open_reclaims_the_legacy_delta_log`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("workspaces").join("7");
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join(LEGACY_DELTA_LOG), b"{\"workspace_id\":7}\n").unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
            assert!(!store.root().join(LEGACY_DELTA_LOG).exists());
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
            crate::coedit::CoeditPayload::Checkpoint {
                channel: [channel; 32],
                body: vec![value],
            }
        } else {
            crate::coedit::CoeditPayload::RetainedDelta {
                channel: [channel; 32],
                body: vec![value],
            }
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
