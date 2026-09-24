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
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shed => f.write_str("transient collaboration update shed"),
            Self::Closed => f.write_str("collaboration transport is closed"),
            Self::Unsupported => f.write_str("collaboration transport does not support this"),
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
    /// implement.
    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError>;

    /// Move every event that is ready into `out`, in arrival order.
    fn drain(&mut self, out: &mut Vec<CollabEvent>);

    /// The remote end's decision about the latest primary `Hello`, once.
    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>>;

    /// Like [`Self::take_primary_route_result`], but a transport that can
    /// block its caller may wait up to `timeout` for the result. Used only
    /// by blocking constructors; poll-driven transports keep the default.
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
}

impl DocumentPublicationReceipt {
    /// Wrap the receiving half a transport resolves exactly once.
    pub fn new(receiver: mpsc::Receiver<Result<(), DocumentPublicationError>>) -> Self {
        Self { receiver }
    }

    /// Inspect completion without waiting. A completed receipt is consumed.
    ///
    /// # Errors
    ///
    /// Returns the prefix failure, or [`DocumentPublicationError::WorkerStopped`]
    /// when the transport dropped the receipt unresolved.
    pub fn try_complete(&self) -> Result<Option<()>, DocumentPublicationError> {
        match self.receiver.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(DocumentPublicationError::WorkerStopped),
        }
    }

    /// Wait for completion, optionally within an absolute deadline. A
    /// completed result takes precedence over an elapsed deadline.
    ///
    /// # Errors
    ///
    /// Returns a publication failure, the transport stopping, or the
    /// deadline elapsing.
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
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        Err(DocumentPublicationError::WorkerStopped)
                    }
                }
            }
            None => self
                .receiver
                .recv()
                .unwrap_or(Err(DocumentPublicationError::WorkerStopped)),
        }
    }
}
