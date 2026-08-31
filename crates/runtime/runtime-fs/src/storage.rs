//! Typed storage-accounting and safe hydration-eviction protocol.
//!
//! The system filesystem daemon is the authority. Clients can inspect a
//! snapshot and submit selections copied from that snapshot; the daemon
//! revalidates every field before dropping any local content blobs.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

pub const DEFAULT_STORAGE_SOCKET: &str = "/run/qos/fs-storage.sock";
const MAX_FRAME: usize = 16 * 1024 * 1024;

// The five booleans are independent facts the daemon reports about one file —
// a file can be dirty and pinned and in flight at once — and they are the
// postcard wire format the daemon and its clients agree on. Folding them into
// a state enum would both lose combinations and break the protocol.
#[allow(clippy::struct_excessive_bools)]
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
    pub evicted_files: u64,
    pub freed_bytes: u64,
    pub skipped: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StorageRequest {
    Snapshot,
    Evict { selected: Vec<EvictionSelection> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StorageResponse {
    Snapshot(SyncStorageSnapshot),
    Evicted(EvictionReport),
    Error(String),
}

pub struct StorageClient {
    stream: UnixStream,
}

impl StorageClient {
    /// Connect to the daemon's storage socket, normally [`DEFAULT_STORAGE_SOCKET`].
    ///
    /// # Errors
    ///
    /// The `connect(2)` error: [`NotFound`](std::io::ErrorKind::NotFound) when the daemon is
    /// not running (nothing has bound the socket path),
    /// [`ConnectionRefused`](std::io::ErrorKind::ConnectionRefused) when the path is a stale
    /// socket file from a dead daemon, or
    /// [`PermissionDenied`](std::io::ErrorKind::PermissionDenied) when this process is
    /// outside the group allowed to talk to it.
    pub fn connect(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self {
            stream: UnixStream::connect(path)?,
        })
    }

    /// Ask the daemon for its current accounting of every synced file.
    ///
    /// # Errors
    ///
    /// A socket error if the request cannot be written or the reply cannot be read — the
    /// daemon exiting mid-call arrives as
    /// [`UnexpectedEof`](std::io::ErrorKind::UnexpectedEof). Otherwise
    /// [`Error::other`](std::io::Error::other) carrying either the daemon's own refusal
    /// message, or a note that it answered a different question than the one asked, which
    /// means the two sides disagree about the protocol.
    pub fn snapshot(&mut self) -> std::io::Result<SyncStorageSnapshot> {
        match self.request(&StorageRequest::Snapshot)? {
            StorageResponse::Snapshot(snapshot) => Ok(snapshot),
            StorageResponse::Error(error) => Err(std::io::Error::other(error)),
            StorageResponse::Evicted(_) => {
                Err(std::io::Error::other("unexpected storage response"))
            }
        }
    }

    /// Ask the daemon to drop the local content of `selected`. The daemon revalidates every
    /// field before freeing anything, so a selection copied from a stale snapshot is skipped
    /// (reported in [`EvictionReport::skipped`]) rather than evicting the wrong version.
    ///
    /// # Errors
    ///
    /// A socket error if the request cannot be written or the reply cannot be read.
    /// Otherwise [`Error::other`](std::io::Error::other) carrying the daemon's refusal
    /// message, or a note that it answered a different question than the one asked. An
    /// individual selection the daemon declines is **not** an error: it lands in
    /// [`EvictionReport::skipped`], so a successful return still needs that field read.
    pub fn evict(&mut self, selected: Vec<EvictionSelection>) -> std::io::Result<EvictionReport> {
        match self.request(&StorageRequest::Evict { selected })? {
            StorageResponse::Evicted(report) => Ok(report),
            StorageResponse::Error(error) => Err(std::io::Error::other(error)),
            StorageResponse::Snapshot(_) => {
                Err(std::io::Error::other("unexpected storage response"))
            }
        }
    }

    fn request(&mut self, request: &StorageRequest) -> std::io::Result<StorageResponse> {
        write_frame(&mut self.stream, request)?;
        read_frame(&mut self.stream)
    }
}

/// Write one length-prefixed postcard frame.
///
/// # Errors
///
/// [`Error::other`](std::io::Error::other) when `value` cannot be postcard-encoded, or when
/// the encoding exceeds the 16 MiB frame ceiling — the ceiling exists so a peer cannot make
/// the reader allocate arbitrarily. Otherwise the writer's own error, typically
/// [`BrokenPipe`](std::io::ErrorKind::BrokenPipe) once the far end has gone. A failure after
/// the length prefix has gone out leaves the stream desynchronised, so the connection should
/// be dropped rather than reused.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> std::io::Result<()> {
    let bytes = postcard::to_allocvec(value).map_err(std::io::Error::other)?;
    // Bounded above by MAX_FRAME, so the length always fits; `try_from` keeps that
    // a checked fact instead of a cast that would wrap if the ceiling ever moved.
    let len = u32::try_from(bytes.len())
        .ok()
        .filter(|_| bytes.len() <= MAX_FRAME)
        .ok_or_else(|| std::io::Error::other("storage frame too large"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&bytes)
}

/// Read one length-prefixed postcard frame.
///
/// # Errors
///
/// [`UnexpectedEof`](std::io::ErrorKind::UnexpectedEof) when the stream ends mid-frame,
/// which is how a daemon exiting mid-call arrives. [`Error::other`](std::io::Error::other)
/// when the length prefix exceeds the 16 MiB ceiling — refused before allocating, so a
/// hostile or desynchronised peer cannot drive this process out of memory — or when the
/// body does not decode as `T`, meaning the two sides disagree about the protocol.
pub fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(reader: &mut R) -> std::io::Result<T> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::other("storage frame too large"));
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    postcard::from_bytes(&bytes).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_carries_all_revalidation_identity() {
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

    #[test]
    fn protocol_roundtrips_typed_snapshot() {
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
        write_frame(&mut wire, &expected).unwrap();
        let decoded: StorageResponse = read_frame(&mut std::io::Cursor::new(wire)).unwrap();
        match decoded {
            StorageResponse::Snapshot(snapshot) => {
                assert!(!snapshot.online);
                assert_eq!(snapshot.workspaces[0].remote_only_bytes, 5);
            }
            _ => panic!("wrong response variant"),
        }
    }
}
