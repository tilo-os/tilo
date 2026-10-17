//! Typed storage-accounting and safe hydration-eviction protocol.
//!
//! The system filesystem daemon is the authority. Clients can inspect a
//! snapshot and submit selections copied from that snapshot; the daemon
//! revalidates every field before dropping any local content blobs.

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::Path};
#[cfg(unix)]
use wire_frame::FrameError;
use wire_frame::Framing;

pub const DEFAULT_STORAGE_SOCKET: &str = "/run/tilo/fs-storage.sock";

/// Every storage-socket message, in both directions, is one frame: a
/// little-endian `u32` length, then the postcard-encoded request or response.
pub const STORAGE_FRAMING: Framing = Framing::little_endian(16 * 1024 * 1024);

// Five independent on/off facts about a synced file. The struct mirrors the
// storage daemon's wire protocol; callers use each fact separately.
#[allow(
    clippy::struct_excessive_bools,
    reason = "wire-protocol struct: each bool is an independent daemon-reported fact, not a state machine"
)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncedFileState {
    pub workspace: String,
    pub app: Option<String>,
    pub logical_path: String,
    pub identity: String,
    pub inode: u64,
    /// Content hash/version at snapshot time.
    pub version: String,
    pub logical_bytes: u64,
    pub hydrated_bytes: u64,
    pub remote_only: bool,
    pub dirty: bool,
    pub pinned: bool,
    pub in_flight: bool,
    pub remotely_durable: bool,
    pub reclaimable_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageAggregate {
    pub owner: String,
    pub logical_bytes: u64,
    pub local_bytes: u64,
    pub remote_only_bytes: u64,
    pub reclaimable_bytes: u64,
    pub files: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncStorageSnapshot {
    pub online: bool,
    pub files: Vec<SyncedFileState>,
    pub workspaces: Vec<StorageAggregate>,
    pub apps: Vec<StorageAggregate>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvictionSelection {
    pub identity: String,
    pub inode: u64,
    pub version: String,
}

impl From<&SyncedFileState> for EvictionSelection {
    fn from(file: &SyncedFileState) -> Self {
        Self {
            identity: file.identity.clone(),
            inode: file.inode,
            version: file.version.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvictionReport {
    /// Requests durably accepted by the filesystem but whose physical GC has
    /// not yet produced the exact all-absent observation required to call
    /// them freed.  This makes an asynchronous, crash-safe eviction truthful
    /// to Settings rather than reporting optimistic reclaimed bytes.
    #[serde(default)]
    pub queued_files: u64,
    pub evicted_files: u64,
    pub freed_bytes: u64,
    pub skipped: Vec<String>,
}

/// How the daemon caches workspace content on this device: whether it
/// fetches the files it predicts will be opened next, and how many bytes
/// of fetched content it keeps.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachePolicy {
    /// Fetch predicted files ahead of their open. Network, battery and disk
    /// gates still apply when this is on.
    pub prefetch: bool,
    /// Bytes of fetched content to keep; `None` is the daemon's default, a
    /// share of the disk that holds the cache. Pinned content and content
    /// only this device holds are kept regardless.
    pub budget_bytes: Option<u64>,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            prefetch: true,
            budget_bytes: None,
        }
    }
}

/// The smallest cache budget the daemon accepts: below it, opening one
/// large file would evict the files just prefetched for it.
pub const MIN_CACHE_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;

/// Where one workspace's bytes on this device stand, by what losing this
/// device would cost. The parts are disjoint and add up to the bytes the
/// workspace holds here. Counts only: nothing here names a file.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidencySplit {
    /// Kept here because the person asked for it ("Keep offline"), and held
    /// by another device too.
    pub pinned_bytes: u64,
    /// Kept here, and another device acknowledged holding the same version.
    pub replicated_bytes: u64,
    /// Held by no other device this one knows of: authored here and not yet
    /// acknowledged elsewhere, or changed and not yet saved. Wiping this
    /// device loses it.
    pub only_here_bytes: u64,
    /// Files with bytes only here.
    pub only_here_files: u64,
    /// Fetched from another device and dropped again when space is short:
    /// safe to free, it downloads again when opened.
    pub cache_bytes: u64,
}

impl ResidencySplit {
    /// Every byte the workspace holds here.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.pinned_bytes
            .saturating_add(self.replicated_bytes)
            .saturating_add(self.only_here_bytes)
            .saturating_add(self.cache_bytes)
    }
}

/// One workspace's [`ResidencySplit`] on this device.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceResidency {
    pub workspace: String,
    /// Whether the person keeps the whole workspace on this device.
    pub kept_here: bool,
    pub split: ResidencySplit,
}

/// What this device holds of each workspace it mounts. It is what the
/// device answers its owner's other devices, so it carries counts per
/// workspace and never a path or a file name.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidencySummary {
    pub workspaces: Vec<WorkspaceResidency>,
}

/// What freeing a workspace's cache dropped.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheFreed {
    pub files: u64,
    pub freed_bytes: u64,
}

/// A request on the storage socket.
///
/// The daemon and its clients are built and updated separately, and
/// postcard numbers variants by position, so new variants go last. A
/// daemon that predates one closes the connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StorageRequest {
    Snapshot,
    Evict {
        selected: Vec<EvictionSelection>,
    },
    /// The cache policy in force.
    CachePolicy,
    /// Replace the cache policy; the daemon answers with the one it applied.
    SetCachePolicy(CachePolicy),
    /// The [`ResidencySummary`] of every workspace.
    Residency,
    /// Drop `workspace`'s cache.
    FreeCache {
        workspace: String,
    },
    /// Keep all of `workspace` on this device, or stop keeping it.
    KeepHere {
        workspace: String,
        keep: bool,
    },
    /// Switch this same managed daemon connection into private enrollment.
    /// The switch itself supplies no certified role, verifier or source access.
    BindExternalSource,
    BindExternalReadSource,
}

/// An answer on the storage socket; new variants go last, as in
/// [`StorageRequest`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StorageResponse {
    Snapshot(SyncStorageSnapshot),
    Evicted(EvictionReport),
    Error(String),
    CachePolicy(CachePolicy),
    Residency(ResidencySummary),
    CacheFreed(CacheFreed),
    Kept {
        keep: bool,
    },
    /// Transport switch only. Mutual kernel role and current certification
    /// checks on the same stream still precede every enrollment.
    ExternalSourceBinding,
    ExternalReadSourceBinding {
        schema: u32,
    },
}

#[cfg(unix)]
pub struct StorageClient {
    stream: UnixStream,
}

#[cfg(unix)]
impl StorageClient {
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the Unix socket at `path` doesn't
    /// exist or refuses the connection.
    pub fn connect(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self {
            stream: UnixStream::connect(path)?,
        })
    }

    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the socket write/read fails, the
    /// daemon reports [`StorageResponse::Error`], or it replies with a
    /// response other than [`StorageResponse::Snapshot`].
    pub fn snapshot(&mut self) -> std::io::Result<SyncStorageSnapshot> {
        match self.request(&StorageRequest::Snapshot)? {
            StorageResponse::Snapshot(snapshot) => Ok(snapshot),
            other => Err(unexpected(other)),
        }
    }

    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the socket write/read fails, the
    /// daemon reports [`StorageResponse::Error`], or it replies with a
    /// response other than [`StorageResponse::Evicted`].
    pub fn evict(&mut self, selected: Vec<EvictionSelection>) -> std::io::Result<EvictionReport> {
        match self.request(&StorageRequest::Evict { selected })? {
            StorageResponse::Evicted(report) => Ok(report),
            other => Err(unexpected(other)),
        }
    }

    /// The cache policy in force.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the socket write/read fails, the
    /// daemon reports [`StorageResponse::Error`], or it replies with a
    /// response other than [`StorageResponse::CachePolicy`].
    pub fn cache_policy(&mut self) -> std::io::Result<CachePolicy> {
        self.policy_request(&StorageRequest::CachePolicy)
    }

    /// Replace the cache policy, returning the one the daemon applied.
    ///
    /// # Errors
    ///
    /// As [`Self::cache_policy`]; the daemon refuses a budget below
    /// [`MIN_CACHE_BUDGET_BYTES`].
    pub fn set_cache_policy(&mut self, policy: CachePolicy) -> std::io::Result<CachePolicy> {
        self.policy_request(&StorageRequest::SetCachePolicy(policy))
    }

    fn policy_request(&mut self, request: &StorageRequest) -> std::io::Result<CachePolicy> {
        match self.request(request)? {
            StorageResponse::CachePolicy(policy) => Ok(policy),
            other => Err(unexpected(other)),
        }
    }

    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the socket write/read fails, the
    /// daemon reports [`StorageResponse::Error`], or it predates the request.
    pub fn residency(&mut self) -> std::io::Result<ResidencySummary> {
        match self.request(&StorageRequest::Residency)? {
            StorageResponse::Residency(summary) => Ok(summary),
            other => Err(unexpected(other)),
        }
    }

    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the socket write/read fails, the
    /// daemon reports [`StorageResponse::Error`], or it predates the request.
    pub fn free_cache(&mut self, workspace: &str) -> std::io::Result<CacheFreed> {
        match self.request(&StorageRequest::FreeCache {
            workspace: workspace.to_owned(),
        })? {
            StorageResponse::CacheFreed(freed) => Ok(freed),
            other => Err(unexpected(other)),
        }
    }

    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the socket write/read fails, the
    /// daemon reports [`StorageResponse::Error`], or it predates the request.
    pub fn keep_here(&mut self, workspace: &str, keep: bool) -> std::io::Result<()> {
        match self.request(&StorageRequest::KeepHere {
            workspace: workspace.to_owned(),
            keep,
        })? {
            StorageResponse::Kept { .. } => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Request the existing lane's closed enrollment switch and consume its
    /// exact stream. Callers must authenticate the actual installed daemon and
    /// current person/device before trusting any subsequent enrollment reply.
    /// # Errors
    /// Refuses unavailable enrollment or any unrelated storage response.
    pub fn bind_external_source(mut self) -> std::io::Result<UnixStream> {
        match self.request(&StorageRequest::BindExternalSource)? {
            StorageResponse::ExternalSourceBinding => Ok(self.stream),
            other => Err(unexpected(other)),
        }
    }

    /// Switch to independently authenticated Files Read enrollment. This
    /// receipt grants no source, person or record authority.
    /// # Errors
    /// Refuses every unrelated or unsupported switch response.
    pub fn bind_external_read_source(mut self) -> std::io::Result<UnixStream> {
        match self.request(&StorageRequest::BindExternalReadSource)? {
            StorageResponse::ExternalReadSourceBinding { schema: 1 } => Ok(self.stream),
            StorageResponse::Error(error) => Err(std::io::Error::other(error)),
            _ => Err(std::io::Error::other("unexpected storage response")),
        }
    }

    fn request(&mut self, request: &StorageRequest) -> std::io::Result<StorageResponse> {
        STORAGE_FRAMING.write_value(&mut self.stream, request)?;
        Ok(STORAGE_FRAMING
            .read_value(&mut self.stream)?
            .ok_or(FrameError::Closed)?)
    }
}

/// The error for an answer other than the one asked for: the daemon's own
/// refusal, or anything else.
#[cfg(unix)]
fn unexpected(response: StorageResponse) -> std::io::Error {
    match response {
        StorageResponse::Error(error) => std::io::Error::other(error),
        _ => std::io::Error::other("unexpected storage response"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The residency requests go last: the bytes of the requests an older
    /// daemon knows stay what they were, and each new one is pinned.
    #[test]
    fn residency_requests_are_appended_to_the_wire() {
        let encode = |request: &StorageRequest| {
            let mut wire = Vec::new();
            STORAGE_FRAMING.write_value(&mut wire, request).unwrap();
            wire
        };
        assert_eq!(encode(&StorageRequest::Snapshot), [1, 0, 0, 0, 0]);
        assert_eq!(encode(&StorageRequest::CachePolicy), [1, 0, 0, 0, 2]);
        assert_eq!(encode(&StorageRequest::Residency), [1, 0, 0, 0, 4]);
        assert_eq!(
            encode(&StorageRequest::FreeCache {
                workspace: "w".into()
            }),
            [3, 0, 0, 0, 5, 1, b'w']
        );
        assert_eq!(
            encode(&StorageRequest::KeepHere {
                workspace: "w".into(),
                keep: true
            }),
            [4, 0, 0, 0, 6, 1, b'w', 1]
        );

        let summary = ResidencySummary {
            workspaces: vec![WorkspaceResidency {
                workspace: "w".into(),
                kept_here: true,
                split: ResidencySplit {
                    pinned_bytes: 1,
                    replicated_bytes: 2,
                    only_here_bytes: 3,
                    only_here_files: 1,
                    cache_bytes: 4,
                },
            }],
        };
        assert_eq!(summary.workspaces[0].split.total_bytes(), 10);
        let mut wire = Vec::new();
        STORAGE_FRAMING
            .write_value(&mut wire, &StorageResponse::Residency(summary.clone()))
            .unwrap();
        assert_eq!(wire.get(4), Some(&4), "the residency answer goes last");
        let decoded: StorageResponse = STORAGE_FRAMING
            .read_value(&mut std::io::Cursor::new(wire))
            .unwrap()
            .unwrap();
        assert!(matches!(decoded, StorageResponse::Residency(back) if back == summary));
    }

    /// The cache-policy requests' bytes. Appended variants keep the
    /// existing ones' bytes and take the next numbers, so an older Settings
    /// and a newer daemon still agree.
    #[test]
    fn cache_policy_wire_bytes_are_pinned() {
        const REQUEST: [u8; 9] = [5, 0, 0, 0, 3, 0, 1, 0x80, 0x08];
        const RESPONSE: [u8; 7] = [3, 0, 0, 0, 3, 1, 0];
        let (client_end, mut daemon) = UnixStream::pair().unwrap();
        daemon
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let daemon = std::thread::spawn(move || {
            let mut request = [0_u8; REQUEST.len()];
            daemon.read_exact(&mut request).unwrap();
            daemon.write_all(&RESPONSE).unwrap();
            request
        });
        let mut client = StorageClient { stream: client_end };
        let applied = client
            .set_cache_policy(CachePolicy {
                prefetch: false,
                budget_bytes: Some(1024),
            })
            .unwrap();
        assert_eq!(daemon.join().unwrap(), REQUEST);
        assert_eq!(applied, CachePolicy::default());
    }

    #[test]
    fn enrollment_switch_appends_wire_tags_and_preserves_exact_stream() {
        let (client_end, mut daemon) = UnixStream::pair().unwrap();
        daemon
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let mut request = [0u8; 5];
            daemon.read_exact(&mut request).unwrap();
            assert_eq!(request, [1, 0, 0, 0, 7]);
            daemon.write_all(&[1, 0, 0, 0, 7]).unwrap();
            // Bytes following the switch belong to the caller's role protocol.
            daemon.write_all(b"role").unwrap();
        });
        let mut enrolled_stream = StorageClient { stream: client_end }
            .bind_external_source()
            .unwrap();
        let mut next = [0u8; 4];
        enrolled_stream.read_exact(&mut next).unwrap();
        assert_eq!(&next, b"role");
        worker.join().unwrap();
    }
    #[test]
    fn ordinary_storage_operations_refuse_enrollment_switch_response() {
        for snapshot in [true, false] {
            let (client_end, mut daemon) = UnixStream::pair().unwrap();
            let worker = std::thread::spawn(move || {
                let _: StorageRequest = STORAGE_FRAMING.read_value(&mut daemon).unwrap().unwrap();
                STORAGE_FRAMING
                    .write_value(&mut daemon, &StorageResponse::ExternalSourceBinding)
                    .unwrap();
            });
            let mut client = StorageClient { stream: client_end };
            if snapshot {
                assert!(client.snapshot().is_err());
            } else {
                assert!(client.evict(Vec::new()).is_err());
            }
            worker.join().unwrap();
        }
    }
    #[test]
    fn storage_wire_bytes_and_protocol_carry_revalidation_identity() {
        // phase: storage_wire_bytes_are_pinned
        // The storage socket's bytes: a little-endian `u32` length, then postcard.
        // The filesystem daemon on the other end is built separately, so any
        // change here must fail.
        {
            const REQUEST: [u8; 11] = [7, 0, 0, 0, 1, 1, 1, b'n', 7, 1, b'v'];
            const RESPONSE: [u8; 12] = [8, 0, 0, 0, 1, 1, 2, 0xac, 0x02, 1, 1, b'x'];
            let (client_end, mut daemon) = UnixStream::pair().unwrap();
            daemon
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let daemon = std::thread::spawn(move || {
                let mut request = [0_u8; REQUEST.len()];
                daemon.read_exact(&mut request).unwrap();
                daemon.write_all(&RESPONSE).unwrap();
                request
            });
            let mut client = StorageClient { stream: client_end };
            let report = client
                .evict(vec![EvictionSelection {
                    identity: "n".into(),
                    inode: 7,
                    version: "v".into(),
                }])
                .unwrap();
            assert_eq!(daemon.join().unwrap(), REQUEST);
            assert_eq!(
                report,
                EvictionReport {
                    queued_files: 1,
                    evicted_files: 2,
                    freed_bytes: 300,
                    skipped: vec!["x".into()],
                }
            );
        }
        // phase: selection_carries_all_revalidation_identity
        {
            let file = SyncedFileState {
                workspace: "work".into(),
                app: None,
                logical_path: "/notes.txt".into(),
                identity: "node-1".into(),
                inode: 7,
                version: "abc".into(),
                logical_bytes: 4,
                hydrated_bytes: 4,
                remote_only: false,
                dirty: false,
                pinned: false,
                in_flight: false,
                remotely_durable: true,
                reclaimable_bytes: 4,
            };
            assert_eq!(
                EvictionSelection::from(&file),
                EvictionSelection {
                    identity: "node-1".into(),
                    inode: 7,
                    version: "abc".into(),
                }
            );
        }
        // phase: protocol_roundtrips_typed_snapshot
        {
            let expected = StorageResponse::Snapshot(SyncStorageSnapshot {
                online: false,
                files: Vec::new(),
                workspaces: vec![StorageAggregate {
                    owner: "work".into(),
                    logical_bytes: 8,
                    local_bytes: 3,
                    remote_only_bytes: 5,
                    reclaimable_bytes: 0,
                    files: 1,
                }],
                apps: Vec::new(),
            });
            let mut wire = Vec::new();
            STORAGE_FRAMING.write_value(&mut wire, &expected).unwrap();
            let decoded: StorageResponse = STORAGE_FRAMING
                .read_value(&mut std::io::Cursor::new(wire))
                .unwrap()
                .unwrap();
            match decoded {
                StorageResponse::Snapshot(snapshot) => {
                    assert!(!snapshot.online);
                    assert_eq!(snapshot.workspaces[0].remote_only_bytes, 5);
                }
                _ => panic!("wrong response variant"),
            }
        }
    }
}
