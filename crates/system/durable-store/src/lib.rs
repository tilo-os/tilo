//! The storage seam behind a Tilo application's records and local views.

use std::any::Any;
use std::fmt;
use std::path::PathBuf;

use tilo_app_id::AppId;

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
mod memory;
pub mod waiting;

pub use memory::{MemoryStore, PendingWrite};

/// What a stored value is, wherever a host keeps it. Every part is an identifier,
/// never a path: a filesystem store validates each as one name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageKey {
    /// One document within one workspace; its id is unique only there.
    Document {
        app: AppId,
        workspace: String,
        document: String,
    },
    App {
        app: AppId,
    },
    WorkspaceIndex {
        app: AppId,
        workspace: String,
    },
    View {
        device: String,
        workspace: String,
        instance: u64,
    },
}

impl StorageKey {
    pub fn document(
        app: impl Into<AppId>,
        workspace: impl Into<String>,
        document: impl Into<String>,
    ) -> Self {
        Self::Document {
            app: app.into(),
            workspace: workspace.into(),
            document: document.into(),
        }
    }

    pub fn app(app: impl Into<AppId>) -> Self {
        Self::App { app: app.into() }
    }

    pub fn workspace_index(app: impl Into<AppId>, workspace: impl Into<String>) -> Self {
        Self::WorkspaceIndex {
            app: app.into(),
            workspace: workspace.into(),
        }
    }

    pub fn app_id(&self) -> Option<&AppId> {
        match self {
            Self::Document { app, .. } | Self::App { app } | Self::WorkspaceIndex { app, .. } => {
                Some(app)
            }
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
            Self::Document {
                app,
                workspace,
                document,
            } => write!(f, "document {app}/{workspace}/{document}"),
            Self::App { app } => write!(f, "app {app}"),
            Self::WorkspaceIndex { app, workspace } => {
                write!(f, "workspace index {app}/{workspace}")
            }
            Self::View {
                device,
                workspace,
                instance,
            } => write!(f, "view {device}/{workspace}/{instance}"),
        }
    }
}

/// A key's current value. `writer` is the opaque identity its single writer
/// persists beside the bytes, so a restarted owner continues the same history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stored {
    pub bytes: Vec<u8>,
    pub writer: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOp {
    Replace(Vec<u8>),
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

    /// Fold a later write into this one, so the pair equals applying both in order.
    pub fn coalesce(&mut self, later: Self) {
        match (self, later) {
            (this, Self::Replace(bytes)) => *this = Self::Replace(bytes),
            (Self::Replace(bytes) | Self::Append(bytes), Self::Append(more)) => {
                bytes.extend_from_slice(&more);
            }
        }
    }

    pub fn apply(&self, current: &mut Vec<u8>) {
        match self {
            Self::Replace(bytes) => bytes.clone_into(current),
            Self::Append(bytes) => current.extend_from_slice(bytes),
        }
    }
}

/// Names one accepted write; a later revision of a key has newer bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Durability {
    /// Committed: it survives a crash of this process and of the host.
    Durable,
    Pending,
    /// The commit failed. Until a later [`WriteOp::Replace`] of the same key is
    /// durable, appends to it are refused: they would bury a partial tail.
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleWatch {
    /// Writes or a sync were outstanding for the key: the wake is called once,
    /// from any thread, when they have all resolved.
    Armed,
    /// Nothing was outstanding: the wake was dropped, and
    /// [`DurableStore::durability`] says what became of it.
    Settled,
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
    /// On a thread that must not wait on storage ([`waiting`]): the store's own
    /// thread still opens or reads the key, and calls the thread's
    /// [wake](waiting::wake) once it is done.
    #[error("{0} is still being opened")]
    Opening(String),
    #[error("revision {0} is not awaiting acknowledgement")]
    UnknownRevision(u64),
    #[error("{context}: {message}")]
    Failed { context: String, message: String },
}

pub type Lease = Box<dyn Any>;

/// Called, from any thread, when work a caller must not wait for is done.
pub type ReadyWake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A non-exclusive claim on one key, for documents several processes write at once.
/// `primary` is true for the one holder that continues the stored writer identity;
/// every other holder writes under an identity of its own.
pub struct SharedLease {
    pub lease: Lease,
    pub primary: bool,
}

/// Keyed, ordered, honestly-acknowledged storage. Writes to one key apply in the
/// order they were accepted.
pub trait DurableStore {
    /// A stable identity for this profile's storage: two stores with the same
    /// namespace address the same data. Collaboration tells profiles apart by it,
    /// never to find files.
    fn namespace(&self) -> &str;

    /// The key's current value, including accepted writes not yet durable; `None`
    /// when nothing has ever been stored.
    fn read(&self, key: &StorageKey) -> Result<Option<Stored>, StoreError>;

    /// Accept a write without waiting for it to commit. `writer` replaces the
    /// identity stored beside the key. An implementation may refuse writes to a key
    /// the caller has not [leased](Self::lease).
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

    /// Make everything read from or accepted for `key` (or every key) durable where
    /// the store can, and report whether it now is. A store that depends on its host
    /// returns [`StoreError::Pending`] instead of blocking.
    ///
    /// # Errors
    /// The latest failed write, or writes still awaiting their host.
    fn flush(&self, key: Option<&StorageKey>) -> Result<(), StoreError>;

    /// [`Self::flush`] for a caller that must not wait on storage: start making
    /// everything read from or accepted for `key` durable and return at once. `Ok`
    /// when it already is, [`StoreError::Pending`] while the store's own thread
    /// finishes ([`Self::is_settled`] turns true then). A store with no thread of its
    /// own waits, as [`Self::flush`] does.
    ///
    /// # Errors
    /// The latest failed write, or [`StoreError::Pending`].
    fn flush_without_waiting(&self, key: &StorageKey) -> Result<(), StoreError> {
        self.flush(Some(key))
    }

    /// [`Self::flush`] where this thread may wait on storage; on a thread that must
    /// not ([`waiting::forbid_on_this_thread`], a frame loop) it makes the call
    /// [`Self::flush_without_waiting`] does, where [`StoreError::Pending`] means the
    /// store's own thread is committing.
    ///
    /// # Errors
    /// As the call it makes.
    fn flush_where_allowed(&self, key: &StorageKey) -> Result<(), StoreError> {
        if waiting::forbidden() {
            self.flush_without_waiting(key)
        } else {
            self.flush(Some(key))
        }
    }

    /// Whether everything read from or accepted for `key` is on stable
    /// storage now. Does not wait where the store has a thread of its own.
    fn is_settled(&self, key: &StorageKey) -> bool {
        self.flush(Some(key)).is_ok()
    }

    /// Call `wake` once, from any thread, when every write accepted for `key` and
    /// every sync asked for it before this call has committed or failed: how a window
    /// showing "Saving…" hears that it is done without polling. Nothing outstanding
    /// answers [`SettleWatch::Settled`] and calls nothing; the same wake
    /// ([`waiting::same_wake`]) armed twice is called once.
    fn wake_when_settled(&self, key: &StorageKey, wake: ReadyWake) -> SettleWatch;

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

    /// Whether a lease or read this thread makes may answer [`StoreError::Opening`]:
    /// on a thread that must not wait on storage, [`Self::lease`],
    /// [`Self::lease_shared`] and [`Self::read`] answer it while the store's own
    /// thread opens or reads the key, and that thread calls the caller's
    /// [wake](waiting::wake) once it has. Reads of a key this thread leased answer
    /// what was read for the lease plus the writes accepted since, and
    /// [`Self::read_hint`] answers only a hint already read. A store with no thread
    /// of its own works in place here and answers `false`.
    fn declined_waiting_here(&self) -> bool {
        false
    }

    /// Grant custody of `key` to exactly one `claimant`, durably: the first claim
    /// wins, and the same claimant may claim again to resume after a crash. Lets one
    /// successor adopt a workspace record under a new identity. Answers `true` when
    /// `claimant` holds custody.
    ///
    /// # Errors
    /// A key the store cannot claim, or a failure recording the claim.
    fn claim(&self, key: &StorageKey, claimant: &str) -> Result<bool, StoreError>;

    /// The document of every [`StorageKey::Document`] of `app` in `workspace` that
    /// holds stored bytes, sorted: how a host finds documents it can no longer name.
    /// A store that cannot enumerate its keys answers with none.
    fn documents(&self, app: &str, workspace: &str) -> Result<Vec<String>, StoreError> {
        let _ = (app, workspace);
        Ok(Vec::new())
    }

    /// Every [`StorageKey::WorkspaceIndex`] the store holds, as its app and
    /// `workspace`, sorted. A store that cannot enumerate its keys answers with none.
    fn workspace_indexes(&self) -> Result<Vec<(String, String)>, StoreError> {
        Ok(Vec::new())
    }

    /// Where a host that keeps the key in a local file keeps it. Only for
    /// diagnostics and tools that work on that file; `None` elsewhere.
    fn local_path(&self, _key: &StorageKey) -> Option<PathBuf> {
        None
    }

    /// Keep `hint` beside the key's value on this device: bytes a caller derived from a
    /// value it wrote, to make the next read cheaper to use. A hint is never part of
    /// the value, never leaves the device and may be dropped at any time, so the caller
    /// must recognise the value it was made for itself. The key must be leased.
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
                StorageKey::document("pkg", "room", "rec").to_string(),
                "document pkg/room/rec"
            );
            assert_eq!(
                StorageKey::view("dev", "room", 7).to_string(),
                "view dev/room/7"
            );
            assert_eq!(StorageKey::app("pkg").to_string(), "app pkg");
            assert_eq!(
                StorageKey::workspace_index("pkg", "room").to_string(),
                "workspace index pkg/room"
            );
        }
    }
}
