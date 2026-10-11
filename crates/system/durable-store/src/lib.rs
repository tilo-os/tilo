//! The storage seam behind a Tilo application's records and local views.
//!
//! Applications never name paths. They persist through a [`DurableStore`]
//! addressed by [`StorageKey`], and learn when a write is safe through
//! [`DurableStore::durability`] rather than by assuming that a returned call
//! means the bytes survived. That one contract covers a local filesystem
//! (fsync before acknowledging), a remote service, or any host that commits
//! on its own schedule and reports back ([`MemoryStore::queued`]).
//!
//! The store is deliberately byte-oriented: it knows nothing about Automerge
//! or view envelopes. Callers decide what a key's bytes mean; the store only
//! promises ordering, single ownership and honest durability.

use std::any::Any;
use std::fmt;
use std::path::PathBuf;

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
mod memory;

pub use memory::{MemoryStore, PendingWrite};

/// What a stored value is, independent of where a host keeps it. Every
/// component is an identifier, never a path: an implementation that maps
/// keys onto a filesystem must validate each one as a single name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageKey {
    /// One record's document within one workspace. A record id is unique
    /// only within its workspace, so the workspace is part of the identity.
    Record {
        package: String,
        workspace: String,
        record: String,
    },
    /// A package's own document, outside any workspace.
    Package { package: String },
    /// The index of records one workspace holds for a package.
    RoomIndex { package: String, workspace: String },
    /// One application instance's local presentation on one device.
    View {
        device: String,
        workspace: String,
        instance: u64,
    },
}

impl StorageKey {
    pub fn record(
        package: impl Into<String>,
        workspace: impl Into<String>,
        record: impl Into<String>,
    ) -> Self {
        Self::Record {
            package: package.into(),
            workspace: workspace.into(),
            record: record.into(),
        }
    }

    pub fn package(package: impl Into<String>) -> Self {
        Self::Package {
            package: package.into(),
        }
    }

    pub fn room_index(package: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self::RoomIndex {
            package: package.into(),
            workspace: workspace.into(),
        }
    }

    /// The package owning this key's data, when it has one.
    pub fn package_id(&self) -> Option<&str> {
        match self {
            Self::Record { package, .. }
            | Self::Package { package }
            | Self::RoomIndex { package, .. } => Some(package),
            Self::View { .. } => None,
        }
    }

    pub fn view(device: impl Into<String>, workspace: impl Into<String>, instance: u64) -> Self {
        Self::View {
            device: device.into(),
            workspace: workspace.into(),
            instance,
        }
    }
}

impl fmt::Display for StorageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record {
                package,
                workspace,
                record,
            } => write!(f, "record {package}/{workspace}/{record}"),
            Self::Package { package } => write!(f, "package {package}"),
            Self::RoomIndex { package, workspace } => write!(f, "room {package}/{workspace}"),
            Self::View {
                device,
                workspace,
                instance,
            } => write!(f, "view {device}/{workspace}/{instance}"),
        }
    }
}

/// A key's current value. `writer` is the opaque identity its single writer
/// persists beside the bytes (for a CRDT document, the actor id), so a
/// restarted owner continues the same history instead of forking it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stored {
    pub bytes: Vec<u8>,
    pub writer: Option<Vec<u8>>,
}

/// How a write changes a key's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOp {
    /// The key's bytes become exactly these.
    Replace(Vec<u8>),
    /// These bytes follow the key's current bytes.
    Append(Vec<u8>),
}

impl WriteOp {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Replace(bytes) | Self::Append(bytes) => bytes,
        }
    }

    pub fn is_replace(&self) -> bool {
        matches!(self, Self::Replace(_))
    }

    /// Fold a later write into this one so the pair has the same effect as
    /// applying both in order. A later replacement supersedes everything; an
    /// append extends whatever this write produces.
    pub fn coalesce(&mut self, later: Self) {
        match (self, later) {
            (this, Self::Replace(bytes)) => *this = Self::Replace(bytes),
            (Self::Replace(bytes) | Self::Append(bytes), Self::Append(more)) => {
                bytes.extend_from_slice(&more);
            }
        }
    }

    /// Apply this write to a key's current bytes.
    pub fn apply(&self, current: &mut Vec<u8>) {
        match self {
            Self::Replace(bytes) => bytes.clone_into(current),
            Self::Append(bytes) => current.extend_from_slice(bytes),
        }
    }
}

/// Names one accepted write. Revisions increase monotonically within one
/// store, so a later revision of a key always describes newer bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

/// Whether an accepted write has survived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Durability {
    /// Committed: it survives a crash of this process and of the host.
    Durable,
    /// Accepted, not yet committed.
    Pending,
    /// The commit failed. Until a later [`WriteOp::Replace`] of the same key
    /// becomes durable, appends to that key are refused: the failed write may
    /// have left a partial tail that further appends would bury.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("another writer holds {0}")]
    Busy(String),
    #[error("invalid storage key {key}: {reason}")]
    InvalidKey { key: String, reason: String },
    #[error("{0} is not leased by this writer")]
    NotLeased(String),
    #[error("the storage behind {0} was replaced underneath its writer")]
    SourceChanged(String),
    #[error("{0} has writes its host has not yet committed")]
    Pending(String),
    #[error("revision {0} is not awaiting acknowledgement")]
    UnknownRevision(u64),
    #[error("{context}: {message}")]
    Failed { context: String, message: String },
}

/// Ownership of one key, held for as long as the value lives.
pub type Lease = Box<dyn Any>;

/// A non-exclusive claim on one key, for documents several processes may
/// write at once because readers merge what each appends. `primary` is true
/// for the one holder entitled to continue the stored writer identity; every
/// other holder must write under an identity of its own.
pub struct SharedLease {
    pub lease: Lease,
    pub primary: bool,
}

/// Keyed, ordered, honestly-acknowledged storage.
///
/// A store is shared by every owner in one profile, so each method takes
/// `&self`. Writes to one key apply in the order they were accepted.
pub trait DurableStore {
    /// A stable identity for this profile's storage. Two stores with the same
    /// namespace address the same data; history and collaboration use it to
    /// tell profiles apart, never to find files.
    fn namespace(&self) -> &str;

    /// The key's current value, including accepted writes not yet durable.
    /// `None` when nothing has ever been stored.
    ///
    /// # Errors
    /// Reports an invalid key or a failure reading the backing storage.
    fn read(&self, key: &StorageKey) -> Result<Option<Stored>, StoreError>;

    /// Accept a write without waiting for it to commit. `writer`, when given,
    /// replaces the identity stored beside the key. An implementation may
    /// refuse writes to a key the caller has not [leased](Self::lease).
    ///
    /// # Errors
    /// Refuses an invalid or unleased key, or a store that cannot accept more.
    fn write(
        &self,
        key: &StorageKey,
        op: WriteOp,
        writer: Option<&[u8]>,
    ) -> Result<Revision, StoreError>;

    /// Whether the write that returned `revision` for `key` has committed.
    /// Never blocks.
    fn durability(&self, key: &StorageKey, revision: Revision) -> Durability;

    /// Make everything read from or accepted for `key` (or every key) durable
    /// where the store can do so itself, and report whether it now is. A store
    /// that depends on its host returns [`StoreError::Pending`] instead of
    /// blocking.
    ///
    /// # Errors
    /// Reports the latest failed write, or writes still awaiting their host.
    fn flush(&self, key: Option<&StorageKey>) -> Result<(), StoreError>;

    /// Become the key's only writer until the returned lease is dropped.
    ///
    /// # Errors
    /// [`StoreError::Busy`] when another owner, in this process or another,
    /// already holds the key.
    fn lease(&self, key: &StorageKey) -> Result<Lease, StoreError>;

    /// Join the key's shared writers. Shared holders coexist with each other
    /// and exclude an exclusive [lease](Self::lease), and it them.
    ///
    /// # Errors
    /// [`StoreError::Busy`] while an exclusive lease is held; an invalid key.
    fn lease_shared(&self, key: &StorageKey) -> Result<SharedLease, StoreError>;

    /// Grant custody of `key` to exactly one `claimant`, durably: the first
    /// claim wins, and the same claimant may claim again (to resume after a
    /// crash). Used to let exactly one successor adopt a room record under a
    /// new identity.
    /// `true` when `claimant` holds custody.
    ///
    /// # Errors
    /// A key the store cannot claim, or a failure recording the claim.
    fn claim(&self, key: &StorageKey, claimant: &str) -> Result<bool, StoreError>;

    /// The `record` component of every [`StorageKey::Record`] of `package`
    /// in `workspace` that holds stored bytes, sorted. Lets a host find
    /// records it can no longer name: an app that used to keep a record per
    /// window and now keeps one per room adopts its windows' records. A
    /// store that cannot enumerate its keys answers with none.
    ///
    /// # Errors
    /// Invalid identifiers or a failure reading the backing storage.
    fn records(&self, package: &str, workspace: &str) -> Result<Vec<String>, StoreError> {
        let _ = (package, workspace);
        Ok(Vec::new())
    }

    /// Where a host that keeps the key in a local file keeps it. Only for
    /// diagnostics and tools that work on that file; `None` elsewhere.
    fn local_path(&self, _key: &StorageKey) -> Option<PathBuf> {
        None
    }

    /// Keep `hint` beside the key's value on this device: bytes a caller
    /// derived from a value it wrote, to make the next read of that value
    /// cheaper to use. A hint is never part of the value, never leaves the
    /// device and may be dropped at any time, so the caller must recognise
    /// the value it was made for itself; a store that keeps none drops it.
    /// The key must be leased, as for [`Self::write`].
    ///
    /// # Errors
    /// An unleased key, or a failure writing the hint.
    fn write_hint(&self, _key: &StorageKey, _hint: &[u8]) -> Result<(), StoreError> {
        Ok(())
    }

    /// The hint last kept beside the key by [`Self::write_hint`], if the
    /// store still has one.
    fn read_hint(&self, _key: &StorageKey) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesced_writes_apply_in_order_and_keys_display_every_component() {
        // Phase `coalesced_writes_match_applying_them_in_order`.
        {
            let ops = [
                WriteOp::Append(b"a".to_vec()),
                WriteOp::Replace(b"base".to_vec()),
                WriteOp::Append(b"+1".to_vec()),
                WriteOp::Append(b"+2".to_vec()),
            ];
            for start in 0..ops.len() {
                let mut sequential = b"old".to_vec();
                let mut combined = ops[start].clone();
                ops[start].apply(&mut sequential);
                for op in &ops[start + 1..] {
                    op.apply(&mut sequential);
                    combined.coalesce(op.clone());
                }
                let mut folded = b"old".to_vec();
                combined.apply(&mut folded);
                assert_eq!(folded, sequential, "starting at {start}");
            }
        }
        // Phase `keys_display_every_identifying_component`.
        {
            assert_eq!(
                StorageKey::record("pkg", "room", "rec").to_string(),
                "record pkg/room/rec"
            );
            assert_eq!(
                StorageKey::view("dev", "room", 7).to_string(),
                "view dev/room/7"
            );
            assert_eq!(StorageKey::package("pkg").to_string(), "package pkg");
            assert_eq!(
                StorageKey::room_index("pkg", "room").to_string(),
                "room pkg/room"
            );
        }
    }
}
