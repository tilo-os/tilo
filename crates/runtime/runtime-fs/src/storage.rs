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

// New variants go last: postcard numbers them in declaration order.
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
    /// Switch this same managed daemon connection into private enrollment.
    /// The switch itself supplies no certified role, verifier or source access.
    BindExternalSource,
    BindExternalReadSource,
}

// New variants go last: postcard numbers them in declaration order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StorageResponse {
    Snapshot(SyncStorageSnapshot),
    Evicted(EvictionReport),
    Error(String),
    CachePolicy(CachePolicy),
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
            StorageResponse::Error(error) => Err(std::io::Error::other(error)),
            StorageResponse::Evicted(_)
            | StorageResponse::CachePolicy(_)
            | StorageResponse::ExternalSourceBinding
            | StorageResponse::ExternalReadSourceBinding { .. } => {
                Err(std::io::Error::other("unexpected storage response"))
            }
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
            StorageResponse::Error(error) => Err(std::io::Error::other(error)),
            StorageResponse::Snapshot(_)
            | StorageResponse::CachePolicy(_)
            | StorageResponse::ExternalSourceBinding
            | StorageResponse::ExternalReadSourceBinding { .. } => {
                Err(std::io::Error::other("unexpected storage response"))
            }
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
            StorageResponse::Error(error) => Err(std::io::Error::other(error)),
            StorageResponse::Snapshot(_)
            | StorageResponse::Evicted(_)
            | StorageResponse::ExternalSourceBinding
            | StorageResponse::ExternalReadSourceBinding { .. } => {
                Err(std::io::Error::other("unexpected storage response"))
            }
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
            StorageResponse::Error(error) => Err(std::io::Error::other(error)),
            StorageResponse::Snapshot(_)
            | StorageResponse::Evicted(_)
            | StorageResponse::CachePolicy(_)
            | StorageResponse::ExternalReadSourceBinding { .. } => {
                Err(std::io::Error::other("unexpected storage response"))
            }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

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
            assert_eq!(request, [1, 0, 0, 0, 4]);
            daemon.write_all(&[1, 0, 0, 0, 4]).unwrap();
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
