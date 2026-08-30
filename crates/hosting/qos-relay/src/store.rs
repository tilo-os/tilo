//! Persistent on-disk state for the relay.
//!
//! Layout (rooted at `RelayConfig.state_root`):
//!
//! ```text
//! /var/lib/qos-relay/
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
//!   pin state — the point of a host is that a peer's phone can be
//!   offline for a week and still catch up.

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

/// Root-signed device revocations enforced during relay authentication.
pub const REVOCATIONS_FILE: &str = "revocations.json";
/// Last daemon counter snapshot, consumed by offline admin/status commands.
pub const METRICS_FILE: &str = "metrics.json";

/// Atomically persist a daemon metrics snapshot.
pub fn save_metrics(
    state_root: &Path,
    snapshot: &crate::metrics::MetricsSnapshot,
) -> Result<(), StoreError> {
    fs::create_dir_all(state_root)?;
    let bytes =
        serde_json::to_vec_pretty(snapshot).map_err(|error| StoreError::Json(error.to_string()))?;
    atomic_file::write(&state_root.join(METRICS_FILE), &bytes)?;
    Ok(())
}

/// Read the last daemon metrics snapshot, if the daemon has written one.
pub fn load_metrics(
    state_root: &Path,
) -> Result<Option<crate::metrics::MetricsSnapshot>, StoreError> {
    let path = state_root.join(METRICS_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let snapshot = serde_json::from_slice(&fs::read(path)?)
        .map_err(|error| StoreError::Json(error.to_string()))?;
    Ok(Some(snapshot))
}

/// Load root-signed revocation entries from the relay state root.
pub fn load_revocations(
    state_root: &Path,
) -> Result<Vec<qos_user_identity::RevocationEntry>, StoreError> {
    let path = state_root.join(REVOCATIONS_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&fs::read(path)?).map_err(|error| StoreError::Json(error.to_string()))
}

/// Atomically replace the relay's root-signed revocation entries.
pub fn save_revocations(
    state_root: &Path,
    entries: &[qos_user_identity::RevocationEntry],
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
    pub fn open(state_root: &Path, workspace_id: u64, pinned: bool) -> Result<Self, StoreError> {
        let root = state_root.join("workspaces").join(workspace_id.to_string());
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("pending"))?;
        fs::create_dir_all(root.join(RETAINED_DIR).join("checkpoints"))?;
        fs::create_dir_all(root.join(RETAINED_DIR).join("tails"))?;
        Ok(Self {
            root,
            workspace_id,
            pinned,
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

    /// Whether this workspace is pinned (holds a persistent CRDT
    /// snapshot).
    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// Load the pinned Automerge snapshot if present. Returns
    /// `Ok(None)` when there is no snapshot yet (first boot for
    /// this workspace).
    pub fn load_snapshot(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let p = self.root.join(SNAPSHOT_FILE);
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(fs::read(p)?))
    }

    /// Atomically write a new snapshot.
    pub fn save_snapshot(&self, bytes: &[u8]) -> Result<(), StoreError> {
        if !self.pinned {
            return Ok(());
        }
        atomic_file::write(&self.root.join(SNAPSHOT_FILE), bytes)?;
        Ok(())
    }

    /// Load device public keys previously admitted to this workspace.
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

    /// Atomically replace the durable membership set.
    pub fn save_membership(&self, devices: &[[u8; 32]]) -> Result<(), StoreError> {
        let encoded: Vec<_> = devices.iter().map(hex::encode).collect();
        let bytes = serde_json::to_vec_pretty(&encoded)
            .map_err(|error| StoreError::Json(error.to_string()))?;
        atomic_file::write(&self.root.join(MEMBERSHIP_FILE), &bytes)?;
        Ok(())
    }

    /// Append an audit log entry. Non-fatal if the log file fails —
    /// the audit trail is best-effort, forwarding must not depend
    /// on it.
    pub fn append_log(&self, entry: &LogEntry) -> Result<(), StoreError> {
        let path = self.root.join(LOG_FILE);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let line = serde_json::to_string(entry).map_err(|e| StoreError::Json(e.to_string()))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Enqueue a delta for later delivery to `device_pubkey`. Each
    /// enqueue produces a new file `<seq>.delta` where seq is a
    /// monotonically increasing hex nanosecond timestamp; the
    /// filename ordering is the delivery ordering.
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
        Ok(path)
    }

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
    /// maintenance and tests; live delivery uses [`Self::load_pending`] so a
    /// failed replay remains recoverable.
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
    pub fn count_pending(&self, device_pubkey: &[u8; 32]) -> Result<u32, StoreError> {
        let dir = self.pending_dir_for(device_pubkey);
        if !dir.exists() {
            return Ok(0);
        }
        let n = fs::read_dir(&dir)?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count();
        Ok(n as u32)
    }

    /// Replace the durable checkpoint for `channel` and discard the tail that
    /// preceded it. `bytes` is a complete encoded relay envelope.
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
        fs::create_dir_all(tail)?;
        Ok(())
    }

    /// Append one encoded relay envelope to the retained tail for `channel`.
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
        Ok(())
    }

    /// Load every retained checkpoint followed by its post-checkpoint tail.
    /// Documents are independent, so channel ordering is irrelevant; ordering
    /// within a channel is stable.
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

/// Load-or-mint the relay's iroh node secret.
///
/// The secret is a 32-byte Ed25519-like key persisted at
/// `<state_root>/iroh-secret.bin` with mode 0600 so the relay's
/// iroh node id is stable across restarts.
pub fn load_or_create_iroh_secret(state_root: &Path) -> Result<[u8; 32], StoreError> {
    let path = state_root.join("iroh-secret.bin");
    if path.exists() {
        let bytes = fs::read(&path)?;
        if bytes.len() != 32 {
            return Err(StoreError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("iroh secret is {} bytes, want 32", bytes.len()),
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        return Ok(out);
    }
    fs::create_dir_all(state_root)?;
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    atomic_file::write_with_mode(&path, &secret, 0o600)?;
    Ok(secret)
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

    #[test]
    fn iroh_secret_stable_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let a = load_or_create_iroh_secret(tmp.path()).unwrap();
        let b = load_or_create_iroh_secret(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, [0u8; 32]);
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
