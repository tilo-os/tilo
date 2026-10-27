//! The one seam between [`CollaborationManager`](super::CollaborationManager)
//! and whatever carries its traffic.
//!
//! A transport is driven synchronously from the manager's owner thread by
//! the existing frame loop: the manager submits commands as the app issues
//! them and drains events once per frame. A transport that runs work
//! elsewhere owns that hand-off itself and uses
//! [`CollabTransport::set_wake`] to schedule the owner's next frame.
//!
//! Transports are `Send` only so the manager that owns one can be built on
//! one thread and moved to the thread that drives it (headless agents and
//! hosted sessions do this). The manager never calls a transport from two
//! threads. A transport built from thread-bound handles can satisfy the
//! bound with a wrapper that checks, at runtime, that it is only touched
//! from its creating thread.
//!
//! # Contract
//!
//! - **Authority stays in the transport.** It authenticates joins, signs
//!   outbound document mutations and verifies inbound envelopes before they
//!   become [`CollabEvent`]s. The manager never holds a transport key.
//! - **Durable traffic fails closed.** A command or event the transport
//!   cannot retain must turn [`CollabTransport::status`] into
//!   [`TransportStatus::Closed`], never be dropped silently. Only transient
//!   presence may be shed, reported as [`TransportError::Shed`].
//! - **Document mutations are deferred, not failed.** A document change
//!   the transport cannot take right now is refused with
//!   [`TransportError::Busy`] so its sender offers it again; one above the
//!   transport's per-change limit is refused with
//!   [`TransportError::TooLarge`] before anything is queued. Neither closes
//!   the transport.
//! - **Primary-route acknowledgements are real.** A result from
//!   [`CollabTransport::take_primary_route_result`] reports what the remote
//!   end decided about the latest primary `Hello`; it is never an optimistic
//!   local echo.

use crate::time::{Duration, Instant};
use std::sync::mpsc;

use super::{ActionDirectoryRequest, CollabEvent, CollabRequest, RouteSpec};

/// Liveness of a transport as the owner sees it this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportStatus {
    /// Still establishing its connection; commands may be refused.
    Connecting,
    /// Admitting commands and delivering events.
    Ready,
    /// Still carrying traffic, with a reason it is impaired.
    Degraded(String),
    /// Permanently unavailable. A closed transport never reopens; a new
    /// connection needs a new transport.
    Closed(String),
}

impl TransportStatus {
    /// Whether the transport currently carries traffic.
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Ready | Self::Degraded(_))
    }
}

/// Why a transport refused a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// A transient presence update was dropped under backpressure. The
    /// transport stays open.
    Shed,
    /// The transport is closed, or closed itself while admitting this
    /// command because it could not retain durable traffic.
    Closed,
    /// This transport does not implement the command.
    Unsupported,
    /// Document traffic already queued fills the transport's budget. The
    /// change was not admitted; retry once earlier changes have been sent.
    Busy,
    /// The document change exceeds the transport's per-change limit and can
    /// never be sent. Nothing of it was queued.
    TooLarge { actual: usize, max: usize },
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shed => f.write_str("transient collaboration update shed"),
            Self::Closed => f.write_str("collaboration transport is closed"),
            Self::Unsupported => f.write_str("collaboration transport does not support this"),
            Self::Busy => f.write_str("collaboration transport is busy"),
            Self::TooLarge { actual, max } => write!(
                f,
                "document change is {actual} bytes, above the {max}-byte collaboration limit"
            ),
        }
    }
}

impl std::error::Error for TransportError {}

/// Everything the manager asks of its transport.
pub enum TransportCommand {
    /// A request on the primary route. A [`CollabRequest::Hello`] announces
    /// (or re-announces) that route; its acknowledgement surfaces through
    /// [`CollabTransport::take_primary_route_result`].
    Primary(CollabRequest),
    /// A request on an explicitly named additional route, joined lazily and
    /// retained. Never a `Hello`.
    ToRoute(RouteSpec, CollabRequest),
    /// Join and subscribe to an additional route without publishing traffic.
    /// Repeated joins of one route are idempotent.
    Join(RouteSpec),
    /// Report the remote end's decision about the latest primary `Hello`,
    /// ordered behind every command submitted before it.
    ConfirmPrimaryRoute,
    /// Install an already-provisioned MLS endpoint for `route`. From then on
    /// the route is fail-closed: outbound document mutations are encrypted
    /// and inbound encrypted envelopes are decrypted at this endpoint.
    /// Exactly one result must be sent on `reply`.
    InstallDocumentEncryption {
        route: RouteSpec,
        group: Box<channel_crypto::ChannelGroup>,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    /// Register this application's pull action directory on the primary
    /// route. Queries arrive through
    /// [`CollabTransport::take_action_directory_requests`].
    PublishActionDirectory { app_id: String },
}

/// A synchronous, poll-driven collaboration transport. See the module
/// documentation for the contract every implementation upholds.
pub trait CollabTransport: Send {
    /// Current liveness; the manager reports it as `is_connected`.
    fn status(&self) -> TransportStatus;

    /// Admit one command without blocking.
    ///
    /// # Errors
    ///
    /// [`TransportError::Shed`] when transient presence was dropped,
    /// [`TransportError::Closed`] when the transport is (or just became)
    /// unavailable, [`TransportError::Unsupported`] for a command it does not
    /// implement, [`TransportError::Busy`] or [`TransportError::TooLarge`]
    /// for a document mutation it did not admit.
    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError>;

    /// Move every event that is ready into `out`, in arrival order.
    fn drain(&mut self, out: &mut Vec<CollabEvent>);

    /// The remote end's decision about the latest primary `Hello`, once.
    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>>;

    /// Inert authenticated recovery provenance; unsupported transports supply none.
    fn connection_snapshot(
        &self,
        route: &RouteSpec,
    ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
        let _ = route;
        None
    }

    /// Native private window identity acknowledged by the original broker.
    /// Unsupported transports and ordinary document/identity markers grant none.
    fn private_window_binding(&self, route: &RouteSpec) -> Option<u64> {
        let _ = route;
        None
    }

    /// Like [`Self::take_primary_route_result`], but a transport that can
    /// block its caller may wait up to `timeout` for the result. A transport
    /// that timed the `Hello` from a deadline it shares with its remote end
    /// waits for that deadline instead, so both sides give up together. Used
    /// only by blocking constructors; poll-driven transports keep the default.
    fn wait_primary_route_result(
        &mut self,
        timeout: Duration,
    ) -> Option<Result<RouteSpec, String>> {
        let _ = timeout;
        self.take_primary_route_result()
    }

    /// Move pending action-directory queries into `out`. Each carries its
    /// own reply channel, answered by the owner thread.
    fn take_action_directory_requests(&mut self, out: &mut Vec<ActionDirectoryRequest>);

    /// A receipt that completes once every document mutation submitted so
    /// far has been published.
    ///
    /// # Errors
    ///
    /// [`DocumentPublicationError::Unsupported`] unless the transport
    /// implements publication completion.
    fn publication_barrier(
        &mut self,
    ) -> Result<DocumentPublicationReceipt, DocumentPublicationError> {
        Err(DocumentPublicationError::Unsupported)
    }

    /// Install the callback that schedules the owner's next frame. A
    /// transport that becomes ready outside [`Self::drain`] calls it so the
    /// events are not left waiting for an unrelated frame.
    fn set_wake(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        let _ = wake;
    }

    /// Crate-internal access to the concrete transport behind the seam, for
    /// facilities that share the built-in worker's queues.
    #[doc(hidden)]
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Scope delivery capture waits to the App’s reconciled live document owners.
    fn set_document_owner_routes(&mut self, _routes: Vec<RouteSpec>) {}

    /// Release native capture turns after the owner applies its drained batch.
    fn finish_event_batch(&mut self) {}

    /// Whether the remote end refused or failed an action-surface
    /// publication since the last call. The owner then republishes its
    /// current surface: a refused one must never count as published.
    fn take_action_surface_failed(&mut self) -> bool {
        false
    }

    /// Actual successful/failed pull-directory RPCs for this transport lifetime.
    /// None means this transport does not expose the observation. Observing
    /// enables an owner wake for later acknowledgements; unobserved callers
    /// retain their original scheduling.
    fn action_directory_publication_counts(&self) -> Option<(u64, u64)> {
        None
    }

    /// Whether committed-document custody allows publishing a document
    /// mutation on `channel` (the primary route when `None` is not yet
    /// acknowledged). Transports without committed reads always allow it.
    fn document_publishable(&self, channel: Option<&str>) -> bool {
        let _ = channel;
        true
    }

    /// Why the last join of `channel` failed, until a join of it succeeds.
    fn route_join_failure(&self, channel: &str) -> Option<String> {
        let _ = channel;
        None
    }

    /// Independently sampled read phases, for bounded test diagnostics only.
    /// These observations never authorize publication or acknowledge a read.
    #[cfg(any(test, feature = "test-support"))]
    fn controlled_document_read_phase_for_test(&self, channel: &str) -> Option<serde_json::Value> {
        let _ = channel;
        None
    }

    /// Ask `channel`'s reader for the whole committed read, as an owner being
    /// built does. Whether the ask was taken.
    #[cfg(any(test, feature = "test-support"))]
    fn controlled_request_whole_read_for_test(&self, channel: &str) -> bool {
        let _ = channel;
        false
    }

    /// Hand one native media command for `channel` to the transport's media
    /// side channel. Returns whether it was admitted.
    fn send_native_media(
        &mut self,
        channel: String,
        request_id: u64,
        command: runtime_collab::native_media::Request,
    ) -> bool {
        let _ = (channel, request_id, command);
        false
    }

    /// Whether the transport latched a terminal document publication failure.
    fn document_publication_failed(&self) -> bool {
        false
    }

    /// Whether that failure was the workspace refusing a change, rather than
    /// the connection or the transport failing.
    fn document_change_refused(&self) -> bool {
        false
    }

    /// Withdraw the transport after an unrecoverable publication outcome.
    fn fail_closed(&self, reason: &'static str) {
        let _ = reason;
    }

    /// Whether the original committed history of `channel` is required
    /// before any local publication on it.
    fn committed_document_required(&self, channel: &str) -> bool {
        let _ = channel;
        false
    }

    /// Join `route`, whose record holds `record_kind`, through the existing
    /// worker when committed custody needs it, returning the original
    /// committed history once it is available.
    fn prepare_committed_document(
        &mut self,
        route: RouteSpec,
        record_kind: &str,
    ) -> Option<super::CommittedDocument> {
        let _ = (route, record_kind);
        None
    }
}

/// Failure to complete a prefix of document publications.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentPublicationError {
    /// This transport has no document-publication completion protocol.
    Unsupported,
    /// No transport is available.
    Unavailable,
    /// The first signing, encoding, transport or queue failure.
    Failed(String),
    /// The workspace's policy declined to admit a change: the connection was
    /// fine, but a change it would not take cannot be followed by others.
    Refused(String),
    /// The transport stopped without resolving the receipt.
    WorkerStopped,
    /// The caller's absolute deadline elapsed.
    DeadlineElapsed,
}

impl std::fmt::Display for DocumentPublicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("document publication completion is unsupported"),
            Self::Unavailable => f.write_str("document publication transport is unavailable"),
            Self::Failed(error) => write!(f, "document publication failed: {error}"),
            Self::Refused(error) => write!(f, "document publication refused: {error}"),
            Self::WorkerStopped => f.write_str("document publication worker stopped"),
            Self::DeadlineElapsed => f.write_str("document publication deadline elapsed"),
        }
    }
}

impl std::error::Error for DocumentPublicationError {}

/// Completion of the document prefix preceding one publication barrier.
///
/// Success means the transport finished publishing that prefix. It does not
/// acknowledge peer observation, CRDT application or durability.
pub struct DocumentPublicationReceipt {
    receiver: mpsc::Receiver<Result<(), DocumentPublicationError>>,
    /// The transport's latched failure, reported when it dropped the receipt
    /// unresolved (for example after a terminal publication failure).
    disconnect_cause: Option<Box<dyn Fn() -> Option<DocumentPublicationError> + Send>>,
}

impl DocumentPublicationReceipt {
    /// Wrap the receiving half a transport resolves exactly once.
    pub fn new(receiver: mpsc::Receiver<Result<(), DocumentPublicationError>>) -> Self {
        Self {
            receiver,
            disconnect_cause: None,
        }
    }

    /// Report `cause` instead of [`DocumentPublicationError::WorkerStopped`]
    /// when the transport drops this receipt unresolved.
    #[must_use]
    pub fn with_disconnect_cause(
        mut self,
        cause: impl Fn() -> Option<DocumentPublicationError> + Send + 'static,
    ) -> Self {
        self.disconnect_cause = Some(Box::new(cause));
        self
    }

    /// Inspect completion without waiting. A completed receipt is consumed.
    pub fn try_complete(&self) -> Result<Option<()>, DocumentPublicationError> {
        match self.receiver.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(self.disconnected()),
        }
    }

    /// Wait for completion, optionally within an absolute deadline. A
    /// completed result takes precedence over an elapsed deadline.
    pub fn wait(self, deadline: Option<Instant>) -> Result<(), DocumentPublicationError> {
        if self.try_complete()?.is_some() {
            return Ok(());
        }
        match deadline {
            Some(deadline) => {
                match self
                    .receiver
                    .recv_timeout(deadline.saturating_duration_since(crate::time::now()))
                {
                    Ok(result) => result,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        Err(DocumentPublicationError::DeadlineElapsed)
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.disconnected()),
                }
            }
            None => self
                .receiver
                .recv()
                .unwrap_or_else(|_| Err(self.disconnected())),
        }
    }

    fn disconnected(&self) -> DocumentPublicationError {
        self.disconnect_cause
            .as_ref()
            .and_then(|cause| cause())
            .unwrap_or(DocumentPublicationError::WorkerStopped)
    }
}
