//! Device-local Connected folders, driven by the native transaction executor.
//!
//! Hosts must compose [`crate::startup::dispatch_early`] before starting threads.
//! This is a bounded, in-process service, not session scheduling: an observer may
//! disappear without cancelling a begun step, but the thread cannot outlive its
//! host process. Durable discovery survives restart. No connection grants cloud
//! upload, workspace sharing, or authority outside the selected endpoints.
//!
//! Only native Linux configurations that qualify on every session can run.
//! Windows remains unavailable. Unsupported structural
//! operations remain pending; this facade does not enable general folder sync.

pub use crate::connection::model::{Conflict, ConnectionId, EntryState, RelativePath, Side};
use crate::connection::worker::{self, Command};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
pub use worker::{
    ConflictCursor, ConnectionCursor, RecoveryCursor, SelectedFolders, TombstoneVersion,
};

/// Every returned page has at most this many entries.
pub const PAGE_LIMIT: usize = 64;
pub const DELETION_RETENTION_SECONDS: u64 = 2_592_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    UnsupportedHost,
    Busy,
    Closed,
    LimitExceeded,
    StaleReview,
    Disconnected,
    IdentityChanged,
    Unavailable,
    PermissionDenied,
    UnsafeVolume,
    ReadOnly,
    Unverified,
    Unstable,
    RecoveryRequired,
    RecoverySpace,
    CorruptStore,
    Persistence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionLifecycle {
    Active,
    Disconnecting,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub id: ConnectionId,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub revision: u64,
    pub has_baseline: bool,
    pub lifecycle: ConnectionLifecycle,
    pub deletion_retention_seconds: u64,
}

pub struct ConnectionPage {
    pub entries: Vec<ConnectionInfo>,
    pub next: Option<ConnectionCursor>,
}

/// Correlation only. It cannot be constructed from durable metadata or handles.
#[derive(Clone, Debug)]
pub struct ReviewId {
    pub(crate) generation: u64,
    pub(crate) lifetime: Arc<()>,
}
impl PartialEq for ReviewId {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && Arc::ptr_eq(&self.lifetime, &other.lifetime)
    }
}
impl Eq for ReviewId {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    NotCompared,
    IncompleteObservation,
    NativeRefused,
    Structural,
    /// A bounded run exhausted its step allowance; Sync now can continue.
    RunLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Pending(Pending),
    Scanning,
    Syncing,
    AwaitingReview {
        review: ReviewId,
        entries: usize,
    },
    UpToDate,
    Conflict,
    Unavailable {
        side: Side,
        error: ConnectionError,
    },
    RecoveryRequired(ConnectionError),
    Paused,
    Disconnecting,
    Disconnected,
    /// This snapshot is no longer maintained. Reopen through the service.
    Detached,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictInfo {
    pub serial: u64,
    pub revision: u64,
    pub kind: Conflict,
    pub path: RelativePath,
    pub source: Option<EntryState>,
    pub destination: Option<EntryState>,
    pub preserved_versions: usize,
    pub resolved: bool,
    /// Bounded metadata choices; every application requires a fresh native review.
    pub choices: Vec<ConflictChoice>,
}

/// Stable correlation only; never a native object or write capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryId {
    pub connection: ConnectionId,
    pub operation: u64,
    pub slot: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryReason {
    Deletion,
    DisplacedVersion,
    Conflict { serial: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAvailability {
    /// Durable candidate metadata. Preview and apply must check current bytes.
    RequiresNativeReview,
    Incomplete,
    Purged,
    Structural,
    Disconnected,
    UnsupportedHost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionEligibility {
    Protected,
    Retaining,
    /// Recorded age is sufficient; native pins must still be evaluated. No purge is implied.
    RequiresEvaluation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryInfo {
    pub id: RecoveryId,
    pub original_side: Side,
    pub original_path: RelativePath,
    pub content: Option<EntryState>,
    pub reason: RecoveryReason,
    pub availability: RecoveryAvailability,
    pub retained_seconds: u64,
    pub retention: RetentionEligibility,
}

pub struct RecoveryPage {
    pub entries: Vec<RecoveryInfo>,
    pub next: Option<RecoveryCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictChoice {
    Recovery(RecoveryInfo),
    Tombstone(TombstoneVersion),
}

/// Opaque process/worker/session-bound review correlation; consumed once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionReviewId(pub(crate) ReviewId);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionPreview {
    pub review: SelectionReviewId,
    pub connection: ConnectionId,
    pub conflict: Option<(u64, u64)>,
    pub path: RelativePath,
    pub target: Side,
    pub source: Option<EntryState>,
    pub destination: Option<EntryState>,
    pub candidate: Option<EntryState>,
    pub original_side: Side,
    pub expires_in_seconds: u64,
}

pub struct ConflictPage {
    pub entries: Vec<ConflictInfo>,
    pub next: Option<ConflictCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitialEntry {
    pub path: RelativePath,
    pub source: Option<EntryState>,
    pub destination: Option<EntryState>,
}

pub struct InitialPage {
    pub entries: Vec<InitialEntry>,
    pub next_offset: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointIssue {
    pub side: Side,
    pub error: ConnectionError,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub connection: ConnectionInfo,
    pub phase: Phase,
    /// Completed native steps in this run, not speculative file/byte totals.
    pub verified_steps: u32,
    pub latest_conflict: Option<ConflictInfo>,
    /// Durable discovery only; this flag never grants continuation authority.
    pub pending_operation: bool,
    /// Endpoint refusal remains visible alongside a durable recovery requirement.
    pub endpoint_issue: Option<EndpointIssue>,
}

/// One coalesced value; slow or dropped observers never block native durability.
#[derive(Clone)]
pub struct Observer(pub(crate) Arc<Mutex<Snapshot>>);
impl Observer {
    pub fn snapshot(&self) -> Snapshot {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// A one-response bounded mailbox. Poll on the UI thread; `wait` is for callers
/// that can block. Dropping it never revokes an already accepted command.
pub struct Request<T>(pub(crate) mpsc::Receiver<Result<T, ConnectionError>>);
impl<T> Request<T> {
    /// # Errors
    /// Returns the command error, or Closed when its worker has exited.
    pub fn try_take(&self) -> Result<Option<T>, ConnectionError> {
        match self.0.try_recv() {
            Ok(value) => value.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(ConnectionError::Closed),
        }
    }
    /// # Errors
    /// Returns the command error, or Closed when its worker has exited.
    pub fn wait(self) -> Result<T, ConnectionError> {
        self.0.recv().map_err(|_| ConnectionError::Closed)?
    }
}

/// One exclusive store owner and at most one active pinned connection. Commands
/// use a bounded queue and return Busy immediately when it is full. Retry/start
/// reopens both endpoints with fresh identity and selected-write qualification.
#[derive(Clone)]
pub struct ConnectedService {
    sender: mpsc::SyncSender<Command>,
    command_lifetime: Arc<()>,
}
impl ConnectedService {
    /// # Errors
    /// Refuses unsupported hosts, a busy or invalid store, or failed durable storage.
    pub fn open_default() -> Result<Self, ConnectionError> {
        worker::open(None)
    }
    /// # Errors
    /// Refuses unsupported hosts before touching the path, or a busy/invalid store.
    pub fn open_at(path: &Path) -> Result<Self, ConnectionError> {
        worker::open(Some(path))
    }
    pub(crate) fn from_sender(sender: mpsc::SyncSender<Command>) -> Self {
        Self {
            sender,
            command_lifetime: Arc::new(()),
        }
    }
    pub(crate) fn command_lifetime(&self) -> std::sync::Weak<()> {
        Arc::downgrade(&self.command_lifetime)
    }
    fn submit<T>(
        &self,
        command: impl FnOnce(mpsc::SyncSender<Result<T, ConnectionError>>) -> Command,
    ) -> Result<Request<T>, ConnectionError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(command(sender))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => ConnectionError::Busy,
                mpsc::TrySendError::Disconnected(_) => ConnectionError::Closed,
            })?;
        Ok(Request(receiver))
    }
    /// Persist explicitly selected folders after fresh qualification. Does not
    /// accept an initial comparison or start synchronization.
    /// # Errors
    /// Returns Busy or Closed when enqueueing fails; the request reports fresh endpoint or store refusal.
    pub fn create(
        &self,
        selection: SelectedFolders,
    ) -> Result<Request<ConnectionInfo>, ConnectionError> {
        self.submit(|reply| Command::Create(selection, reply))
    }
    /// # Errors
    /// Refuses page limits outside 1..=64, or a full/closed command queue. The request rejects stale cursors.
    pub fn list(
        &self,
        cursor: Option<ConnectionCursor>,
        limit: usize,
    ) -> Result<Request<ConnectionPage>, ConnectionError> {
        if !(1..=PAGE_LIMIT).contains(&limit) {
            return Err(ConnectionError::LimitExceeded);
        }
        self.submit(|reply| Command::List(cursor, limit, reply))
    }
    /// # Errors
    /// Returns Busy or Closed when enqueueing fails. The request reports missing records or another active connection.
    pub fn reopen(&self, id: ConnectionId) -> Result<Request<Observer>, ConnectionError> {
        self.submit(|reply| Command::Open(id, false, reply))
    }
    /// # Errors
    /// Returns Busy or Closed when enqueueing fails. The request rejects disconnected records; endpoint refusal is visible in its observer.
    pub fn start(&self, id: ConnectionId) -> Result<Request<Observer>, ConnectionError> {
        self.submit(|reply| Command::Open(id, true, reply))
    }
    /// # Errors
    /// Returns Busy or Closed when enqueueing fails; the request rejects stale reviews and offsets beyond the comparison.
    pub fn preview(
        &self,
        review: ReviewId,
        offset: usize,
    ) -> Result<Request<InitialPage>, ConnectionError> {
        self.submit(|reply| Command::Preview(review, offset, reply))
    }
    /// Accept the complete initial comparison: safe copies may flow either way;
    /// divergent versions are preserved as conflicts, never silently chosen.
    /// Any changed observation invalidates the review before an intent is begun.
    /// # Errors
    /// Returns Busy or Closed when enqueueing fails; the request rejects expired or changed comparisons without beginning an intent.
    pub fn accept_initial(&self, review: ReviewId) -> Result<Request<()>, ConnectionError> {
        self.submit(|reply| Command::Accept(review, reply))
    }
    /// Pause after the current native step's durable boundary. This is a session
    /// control, not a persisted scheduling policy.
    /// # Errors
    /// Returns Busy or Closed when enqueueing fails; the request rejects a different or disconnected active connection.
    pub fn pause(&self, id: ConnectionId) -> Result<Request<()>, ConnectionError> {
        self.submit(|reply| Command::Pause(id, reply))
    }
    /// Durable fence, keeping both folders and all recovery bytes. Interrupted
    /// native work may leave Disconnecting/RecoveryRequired for later recovery.
    /// # Errors
    /// Returns Busy or Closed when enqueueing fails; the request exposes storage failures or recovery needed before fence completion.
    pub fn disconnect(&self, id: ConnectionId) -> Result<Request<ConnectionInfo>, ConnectionError> {
        self.submit(|reply| Command::Disconnect(id, reply))
    }
    /// # Errors
    /// Returns Busy or Closed when enqueueing fails; the request rejects missing records or stale cursors.
    pub fn conflicts(
        &self,
        id: ConnectionId,
        cursor: Option<ConflictCursor>,
    ) -> Result<Request<ConflictPage>, ConnectionError> {
        self.submit(|reply| Command::Conflicts(id, cursor, reply))
    }
    /// Discover retained versions, including after pause or disconnection.
    /// # Errors
    /// Refuses limits outside 1..=64, a full queue, invalid records or stale cursors.
    pub fn recovery(
        &self,
        id: ConnectionId,
        cursor: Option<RecoveryCursor>,
        limit: usize,
    ) -> Result<Request<RecoveryPage>, ConnectionError> {
        if !(1..=PAGE_LIMIT).contains(&limit) {
            return Err(ConnectionError::LimitExceeded);
        }
        self.submit(|reply| Command::Recovery(id, cursor, limit, reply))
    }
    /// Review a retained candidate at its original path on the selected side.
    /// # Errors
    /// Refuses unavailable sessions, candidates or fresh endpoint observations.
    pub fn preview_recovery(
        &self,
        id: RecoveryId,
        target: Side,
    ) -> Result<Request<SelectionPreview>, ConnectionError> {
        self.submit(|reply| Command::PreviewRecovery(id, target, reply))
    }
    /// Review one preserved version or recorded absence of an explicit conflict revision.
    /// # Errors
    /// Refuses stale/foreign choices, unavailable sessions and unsupported structures.
    pub fn preview_conflict(
        &self,
        id: ConnectionId,
        serial: u64,
        revision: u64,
        choice: ConflictChoice,
        target: Side,
    ) -> Result<Request<SelectionPreview>, ConnectionError> {
        self.submit(|reply| Command::PreviewConflict(id, serial, revision, choice, target, reply))
    }
    /// Apply the displayed selection. An occupied target is preserved as a conflict.
    /// # Errors
    /// Refuses stale, expired or consumed reviews and failed current native proofs.
    pub fn restore(&self, review: SelectionReviewId) -> Result<Request<()>, ConnectionError> {
        self.submit(|reply| Command::Restore(review, false, reply))
    }
    /// Affirmatively replace the target shown by a fresh selection review,
    /// preserving displaced bytes through the existing native transaction.
    /// # Errors
    /// Refuses stale, expired or consumed reviews and failed current native proofs.
    pub fn replace(&self, review: SelectionReviewId) -> Result<Request<()>, ConnectionError> {
        self.submit(|reply| Command::Restore(review, true, reply))
    }
}
