//! The broker transport: the `runtime-collab` Cap'n Proto SDK on a worker
//! thread the transport owns.
//!
//! The worker (the "shovel") runs a `current_thread` Tokio runtime and a
//! `LocalSet`, because capnp-rpc futures are `!Send`. The owner thread
//! reaches it over a pair of bounded queues: [`CollabTransport::submit`]
//! enqueues commands and [`CollabTransport::drain`] empties the event queue.
//! Durable traffic fails the bridge closed on overload; only transient
//! presence may be shed. Document mutations have their own budget and are
//! deferred (`Busy`) or refused (`TooLarge`) without closing it. Joining,
//! document signing and envelope verification all happen on the worker with
//! the certified launcher (or host-supplied) identity.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the shovel thread bridges a blocking socket"
)]

#[cfg(test)]
use super::channel_key;
use super::{
    ActionDirectoryRequest, Backend, BrokerActionCatalogueWire, BrokerActionDescriptor,
    BrokerActionSurface, CollabEvent, CollabRequest, CollabTransport, CollaborationManager,
    Collaborator, DocumentMutation, DocumentPublicationError, DocumentPublicationReceipt,
    ExportedAction, ExportedInput, HostDocumentCommitter, PresenceKey, PresencePayload,
    PrimaryRouteState, RouteSpec, SignedOp, TransportCommand, TransportError, TransportStatus,
    Uuid, auto_hello_env, mpsc, presence_handle_env,
};

// Worker-owned Cloud facilities: committed-document reads, the paged
// directory client, managed sender attempts and the native media lane all
// share this worker's queues, byte budgets and route handles.
#[path = "audio_output.rs"]
mod audio_output;
#[path = "committed_document.rs"]
pub(super) mod committed_document;
#[path = "directory_client.rs"]
mod directory_client;
#[path = "managed_sender_transport.rs"]
mod managed_sender_transport;
#[path = "native_initial.rs"]
pub(crate) mod native_initial;
#[path = "native_media.rs"]
mod native_media;
pub use committed_document::CommittedDocument;

use crate::time::{Duration, Instant};
use getrandom::{SysRng, rand_core::UnwrapErr};
use runtime_portal::RawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::LocalSet;

/// A disconnected/slow UI must apply backpressure to broker callbacks rather
/// than allow an unbounded cross-thread queue to become a memory `DoS`.
const ACTION_DIRECTORY_REQUEST_QUEUE: usize = 64;

/// How long an app waits for the collab connect path to report before
/// opening without it, spent on the main thread ahead of the first frame.
/// Every step before the report is a local syscall, so reaching this means
/// something upstream is wedged.
const SHOVEL_INIT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a primary `Hello` may take once the worker starts the join. The
/// worker publishes that deadline and abandons the join at it; a blocking
/// owner waits for the published deadline plus [`PRIMARY_ROUTE_REPLY_SLACK`],
/// so the worker always decides first and dropping a failed manager always
/// closes its owned transports.
const SDK_PRIMARY_ROUTE_TIMEOUT: Duration = Duration::from_millis(4_500);
/// How long past the shared primary-route deadline the owner waits for the
/// worker's answer to arrive.
const PRIMARY_ROUTE_REPLY_SLACK: Duration = Duration::from_millis(500);
/// The one error a primary route that missed its deadline reports, whichever
/// side noticed.
const PRIMARY_ROUTE_TIMED_OUT: &str = "collaboration primary route timed out";
/// One absolute deadline for each host identity request/response exchange.
/// The identity client drops a failed stream, so this also bounds shutdown of
/// partial and late replies without blocking the shovel's `LocalSet`.
const HOST_IDENTITY_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of UI-to-shovel commands waiting behind a slow broker.
/// This deliberately caps commands, rather than bytes, because the protocol
/// already bounds its payloads at each broker boundary.
const SDK_COMMAND_QUEUE: usize = 256;
/// Maximum number of shovel-to-UI events waiting behind a busy event loop.
const SDK_EVENT_QUEUE: usize = 256;
/// Aggregate retained payload budget for each cross-thread queue. This is
/// independent of the item-count cap because a single document snapshot or
/// invocation argument may be large.
const SDK_COMMAND_QUEUE_BYTES: usize = 4 * 1024 * 1024;
const SDK_EVENT_QUEUE_BYTES: usize = 4 * 1024 * 1024;
/// Separate retained budgets for document traffic. A document change may be
/// as large as [`collab_types::MAX_DOCUMENT_TRANSFER_BYTES`]; accounting it
/// apart from control traffic means one large edit in flight can neither
/// starve action results nor make them fail the bridge. When the budget is
/// exhausted, outbound documents report [`TransportError::Busy`] so the
/// caller retries, and inbound document events wait for the owner to drain
/// rather than disconnecting.
const SDK_DOCUMENT_COMMAND_BYTES: usize =
    collab_types::MAX_DOCUMENT_TRANSFER_BYTES + SDK_COMMAND_QUEUE_BYTES;
const SDK_DOCUMENT_EVENT_BYTES: usize =
    collab_types::MAX_DOCUMENT_TRANSFER_BYTES + SDK_EVENT_QUEUE_BYTES;
/// Re-check interval while an inbound document event waits for budget.
const SDK_DOCUMENT_EVENT_RETRY: Duration = Duration::from_millis(250);
const WIFI_CONTINUITY_TIMEOUT: Duration = Duration::from_secs(3);

/// The connected transport's outbound queue, used by the Settings-only
/// commands ([`wifi_continuity_command`], [`speaker_command`]).
static SETTINGS_COMMAND_BRIDGE: std::sync::OnceLock<
    std::sync::Mutex<Option<tokio_mpsc::WeakSender<QueuedSdkCommand>>>,
> = std::sync::OnceLock::new();

/// Invoke the shell's launcher-authenticated, sealed Wi-Fi continuity path.
/// Callers should run this off the frame thread; the bounded timeout turns a
/// lost broker into an ordinary unavailable result.
///
/// # Errors
///
/// Returns an unavailable message when the collaboration bridge is absent,
/// its bounded queue rejects the command, or the broker does not reply in time.
pub fn wifi_continuity_command(
    command: runtime_collab::WifiContinuityCommand,
) -> Result<runtime_collab::WifiContinuityReply, String> {
    let sender = SETTINGS_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| "Wi-Fi continuity bridge is unavailable".to_string())?
        .as_ref()
        .and_then(tokio_mpsc::WeakSender::upgrade)
        .ok_or_else(|| {
            "Wi-Fi continuity requires the certified collaboration service".to_string()
        })?;
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let state = BridgeState::new();
    if !enqueue_sdk_command(
        &sender,
        SdkCommand::WifiContinuity {
            app_id: "so.tilo.settings".into(),
            command,
            reply: reply_tx,
        },
        &state,
    ) {
        return Err("Wi-Fi continuity command queue is unavailable".into());
    }
    reply_rx
        .recv_timeout(WIFI_CONTINUITY_TIMEOUT)
        .map_err(|_| "Wi-Fi continuity service did not respond".to_string())?
}

const SPEAKER_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

/// Ask the shell's speaker-group authority over the launcher-authenticated
/// Settings route. Run it off the frame thread; a lost broker times out into
/// an ordinary unavailable result.
///
/// # Errors
///
/// Returns an unavailable message when the collaboration bridge is absent,
/// its bounded queue rejects the command, or the broker does not reply in time.
pub fn speaker_command(
    command: runtime_collab::SpeakerCommand,
) -> Result<runtime_collab::SpeakerReply, String> {
    let sender = SETTINGS_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| "speaker service is unavailable".to_string())?
        .as_ref()
        .and_then(tokio_mpsc::WeakSender::upgrade)
        .ok_or_else(|| "speakers need the certified collaboration service".to_string())?;
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let state = BridgeState::new();
    if !enqueue_sdk_command(
        &sender,
        SdkCommand::Speakers {
            app_id: "so.tilo.settings".into(),
            command,
            reply: reply_tx,
        },
        &state,
    ) {
        return Err("speaker command queue is unavailable".into());
    }
    reply_rx
        .recv_timeout(SPEAKER_COMMAND_TIMEOUT)
        .map_err(|_| "the speaker service did not respond".to_string())?
}

static DISPLAY_COMMAND_BUDGET: std::sync::OnceLock<BridgeState> = std::sync::OnceLock::new();

/// Ask the native host to inspect or change its display transaction. Call off
/// the frame thread; the existing bridge and response wait remain bounded.
///
/// # Errors
/// Reports disconnected authority, refused requests and timed out replies.
pub fn display_control(
    command: Vec<u8>,
    origin: runtime_collab::settings::DisplayOrigin,
) -> Result<Vec<u8>, String> {
    if command.len() > 64 * 1024 {
        return Err("The display request exceeds its wire bound.".into());
    }
    let sender = SETTINGS_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| "The display service is unavailable.")?
        .as_ref()
        .and_then(tokio_mpsc::WeakSender::upgrade)
        .ok_or("Displays need the certified collaboration service.")?;
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    if !enqueue_sdk_command(
        &sender,
        SdkCommand::DisplayControl {
            origin,
            command,
            reply: reply_tx,
        },
        DISPLAY_COMMAND_BUDGET.get_or_init(BridgeState::new),
    ) {
        return Err("The display request queue is unavailable.".into());
    }
    reply_rx
        .recv_timeout(Duration::from_secs(4))
        .map_err(|_| "The display service did not respond.".to_string())?
}

/// Have the connected transport's worker send the app's unsent settings
/// writes ([`super::channel_settings`]). Without a connected transport they
/// wait for the primary route, whose join sends them.
pub(super) fn flush_settings() {
    let Some(sender) = SETTINGS_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|bridge| bridge.as_ref().and_then(tokio_mpsc::WeakSender::upgrade))
    else {
        return;
    };
    let _ = enqueue_sdk_command(&sender, SdkCommand::FlushSettings, &BridgeState::new());
}

/// Have the connected transport's worker send the app's live activity
/// ([`super::channel_live_activity`]). Without a connected transport it waits
/// for the primary route, whose join sends it.
pub(super) fn flush_live_activity() {
    let Some(sender) = SETTINGS_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|bridge| bridge.as_ref().and_then(tokio_mpsc::WeakSender::upgrade))
    else {
        return;
    };
    let _ = enqueue_sdk_command(&sender, SdkCommand::FlushLiveActivity, &BridgeState::new());
}

/// Owned mpsc pair + shovel-thread handle for SDK mode. Both directions are
/// bounded. Durable overflow makes the bridge unavailable rather than
/// silently losing an action or invocation transition; a document change
/// that does not fit is reported to its sender instead, never dropped.
///
/// Dropping the manager drops `outbound_tx`; the shovel thread's
/// `.recv()` then yields `None` and the thread exits. `_thread` is kept
/// only so the `JoinHandle` isn't detached.
pub(super) struct BrokerTransport {
    outbound_tx: tokio_mpsc::Sender<QueuedSdkCommand>,
    /// Native media rides its own bounded lane so a wedged document RPC can
    /// never delay a stop obligation.
    media: Option<native_media::Sender>,
    inbound_rx: tokio_mpsc::Receiver<QueuedCollabEvent>,
    // Native read turns remain held through the owner's event batch.
    document_turns: Vec<tokio::sync::OwnedSemaphorePermit>,
    state: BridgeState,
    /// The receiver stays on the application's owner thread.  RPC servers
    /// own only the cloneable sender, so a Cap'n Proto callback cannot touch
    /// widgets directly.
    action_directory_rx: mpsc::Receiver<ActionDirectoryRequest>,
    action_directory_tx: mpsc::SyncSender<ActionDirectoryRequest>,
    /// Reply slot of the latest [`TransportCommand::ConfirmPrimaryRoute`],
    /// answered by the shovel once it has processed every earlier command.
    pending_primary: Option<mpsc::Receiver<Result<RouteSpec, String>>>,
    /// Deadline of the latest primary `Hello`, published by the shovel when
    /// it starts the join.
    primary_deadline: Option<PrimaryDeadline>,
    /// The shovel dropped a primary-route reply unanswered: it has exited.
    worker_stopped: bool,
    /// Whether [`wifi_continuity_command`] has been pointed at this
    /// transport; done once it has connected.
    wifi_bridge_registered: bool,
    _thread: Option<thread::JoinHandle<()>>,
}

#[cfg(all(test, unix))]
pub(crate) struct NativeHistoryInvocationFixture {
    pub manager: CollaborationManager,
    pub refresh: Box<dyn Fn(Vec<u8>, tokio::sync::oneshot::Receiver<()>)>,
    pub invoke: Box<dyn Fn(u64, Uuid)>,
    pub received: Box<dyn Fn() -> usize>,
    pub execution: Box<dyn Fn(Result<bool, String>)>,
    pub terminals: Box<dyn Fn() -> usize>,
    pub updates: Box<dyn Fn() -> Vec<(u64, tilo_actions::InvocationUpdate)>>,
    pub thread: thread::JoinHandle<()>,
}

impl BrokerTransport {
    fn enqueue(&self, command: SdkCommand) -> Result<(), TransportError> {
        try_enqueue_sdk_command(&self.outbound_tx, command, &self.state)
    }

    /// Route [`wifi_continuity_command`] through this transport.
    fn register_wifi_bridge(&mut self) {
        if let Ok(mut bridge) = SETTINGS_COMMAND_BRIDGE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
        {
            *bridge = Some(self.outbound_tx.downgrade());
            self.wifi_bridge_registered = true;
        }
    }
}

impl CollabTransport for BrokerTransport {
    fn status(&self) -> TransportStatus {
        match self.state.connect_phase() {
            // Why the worker stopped, when it stopped setting up.
            ConnectPhase::Failed(reason) => TransportStatus::Closed(reason),
            _ if self.worker_stopped => {
                TransportStatus::Closed("collaboration worker stopped".into())
            }
            _ if !self.state.is_healthy() => {
                TransportStatus::Closed("bounded bridge overloaded or disconnected".into())
            }
            ConnectPhase::Connecting => TransportStatus::Connecting,
            ConnectPhase::Connected => TransportStatus::Ready,
        }
    }

    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
        let command = match command {
            TransportCommand::Primary(CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                record_kind,
                handle,
            }) => {
                let deadline = PrimaryDeadline::default();
                self.primary_deadline = Some(deadline.clone());
                SdkCommand::Hello(PrimaryHello {
                    route: RouteSpec {
                        app_id,
                        cluster_id,
                        record,
                    },
                    record_kind,
                    handle,
                    deadline,
                })
            }
            TransportCommand::Primary(request) => SdkCommand::Primary(request),
            TransportCommand::ToRoute(route, request) => SdkCommand::ToRoute(route, request),
            TransportCommand::Join(route) => SdkCommand::Join(route, None),
            TransportCommand::ConfirmPrimaryRoute => {
                let (reply, pending) = mpsc::sync_channel(1);
                self.enqueue(SdkCommand::PrimaryReady(reply))?;
                self.pending_primary = Some(pending);
                return Ok(());
            }
            TransportCommand::InstallDocumentEncryption {
                route,
                group,
                reply,
            } => SdkCommand::InstallEncryption {
                spec: route,
                group,
                reply,
            },
            TransportCommand::PublishActionDirectory { app_id } => {
                SdkCommand::PublishActionDirectory {
                    app_id,
                    requests: self.action_directory_tx.clone(),
                }
            }
        };
        self.enqueue(command)
    }

    fn drain(&mut self, out: &mut Vec<CollabEvent>) {
        self.document_turns.clear();
        self.state.begin_poll();
        if !self.wifi_bridge_registered
            && matches!(self.state.connect_phase(), ConnectPhase::Connected)
        {
            self.register_wifi_bridge();
        }
        while let Ok(mut evt) = self.inbound_rx.try_recv() {
            match evt._turn.take() {
                Some(EventTurn::Document(turns)) => self.document_turns.extend(turns),
                Some(EventTurn::Snapshot(turn)) => drop(turn),
                None => {}
            }
            out.push(evt.event);
        }
    }

    fn set_document_owner_routes(&mut self, routes: Vec<RouteSpec>) {
        self.state
            .document_reads
            .set_document_owner_routes(routes.iter().map(RouteSpec::channel_id).collect());
    }

    fn finish_event_batch(&mut self) {
        self.document_turns.clear();
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        let result = match self.pending_primary.as_ref()?.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.worker_stopped = true;
                None
            }
        };
        self.pending_primary = None;
        self.primary_deadline = None;
        result
    }

    fn wait_primary_route_result(
        &mut self,
        timeout: Duration,
    ) -> Option<Result<RouteSpec, String>> {
        // `timeout` bounds how long the worker may take to start the join;
        // once it has, it answers by the deadline it published, and waiting
        // past that by the slack means this side never gives up while the
        // worker still may.
        let start_by = crate::time::now()
            .checked_add(timeout)
            .unwrap_or_else(crate::time::now);
        let result = loop {
            let end = self
                .primary_deadline
                .as_ref()
                .and_then(PrimaryDeadline::get)
                .map_or(start_by, |deadline| {
                    deadline
                        .checked_add(PRIMARY_ROUTE_REPLY_SLACK)
                        .unwrap_or(deadline)
                });
            let remaining = end.saturating_duration_since(crate::time::now());
            if remaining.is_zero() {
                return None;
            }
            // Wake to notice a deadline published after this wait began.
            match self
                .pending_primary
                .as_ref()?
                .recv_timeout(remaining.min(PRIMARY_DEADLINE_POLL))
            {
                Ok(result) => break Some(result),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.worker_stopped = true;
                    break None;
                }
            }
        };
        self.pending_primary = None;
        self.primary_deadline = None;
        result
    }

    fn take_action_directory_requests(&mut self, out: &mut Vec<ActionDirectoryRequest>) {
        while let Ok(request) = self.action_directory_rx.try_recv() {
            out.push(request);
        }
    }

    fn publication_barrier(
        &mut self,
    ) -> Result<DocumentPublicationReceipt, DocumentPublicationError> {
        let (reply, receiver) = mpsc::sync_channel(1);
        if let Err(error) = self.enqueue(SdkCommand::PublicationBarrier(reply)) {
            return Err(self
                .state
                .publication_result()
                .err()
                .unwrap_or(DocumentPublicationError::Failed(format!("{error:?}"))));
        }
        let state = self.state.clone();
        Ok(DocumentPublicationReceipt::new(receiver)
            .with_disconnect_cause(move || state.publication_result().err()))
    }

    fn document_publishable(&self, channel: Option<&str>) -> bool {
        channel.is_some_and(|channel| self.state.document_reads.publishable(channel))
    }

    #[cfg(any(test, feature = "test-support"))]
    fn controlled_document_read_phase_for_test(&self, channel: &str) -> Option<serde_json::Value> {
        let phase = self.state.document_reads.readiness_diagnostic(channel)?;
        Some(serde_json::json!({
            "required": phase.required,
            "staged": phase.staged,
            "applied": phase.applied,
            "refresh_requested": phase.refresh_requested,
        }))
    }

    fn take_action_surface_failed(&mut self) -> bool {
        self.state
            .action_surface_failed
            .swap(false, Ordering::AcqRel)
    }

    fn action_directory_publication_counts(&self) -> Option<(u64, u64)> {
        self.state
            .directory_observation_enabled
            .store(true, Ordering::Release);
        Some((
            self.state.directory_rpc_successes.load(Ordering::Acquire),
            self.state.directory_rpc_failures.load(Ordering::Acquire),
        ))
    }

    fn take_action_directory_failed(&mut self) -> bool {
        self.state
            .action_directory_failed
            .swap(false, Ordering::AcqRel)
    }

    fn send_native_media(
        &mut self,
        channel: String,
        request_id: u64,
        command: runtime_collab::native_media::Request,
    ) -> bool {
        self.state.is_healthy()
            && self
                .media
                .as_ref()
                .is_some_and(|media| media.send(channel, request_id, command))
    }

    fn document_publication_failed(&self) -> bool {
        self.state.publication_result().is_err()
    }

    fn fail_closed(&self, reason: &'static str) {
        self.state.fail_closed(reason);
    }

    fn private_window_binding(&self, route: &RouteSpec) -> Option<u64> {
        self.state
            .is_healthy()
            .then(|| {
                self.state
                    .document_reads
                    .private_window(&route.channel_id())
            })
            .flatten()
    }

    fn committed_document_required(&self, channel: &str) -> bool {
        self.state.document_reads.required(channel)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn prepare_committed_document(
        &mut self,
        route: RouteSpec,
        record_kind: &str,
    ) -> Option<CommittedDocument> {
        let key = route.channel_id();
        if self.state.document_reads.joining(&key) {
            let _ = self.enqueue(SdkCommand::Join(route, Some(record_kind.to_owned())));
        }
        self.state.document_reads.initial(&key)
    }
}

enum SdkCommand {
    Initialize {
        route: RouteSpec,
        request: runtime_collab::committed_document::Request,
        reply: mpsc::SyncSender<Result<[u8; 32], String>>,
    },
    /// Announce the primary route; the shovel abandons the join at `deadline`.
    Hello(PrimaryHello),
    Primary(CollabRequest),
    /// Join a route, naming the resource kind of its record when known.
    Join(RouteSpec, Option<String>),
    ToRoute(RouteSpec, CollabRequest),
    PrimaryReady(mpsc::SyncSender<Result<RouteSpec, String>>),
    /// Answered once every document mutation queued before it has been
    /// handed to the broker, with the first one that was not.
    PublicationBarrier(mpsc::SyncSender<Result<(), DocumentPublicationError>>),
    InstallEncryption {
        spec: RouteSpec,
        group: Box<channel_crypto::ChannelGroup>,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    PublishActionDirectory {
        app_id: String,
        requests: mpsc::SyncSender<ActionDirectoryRequest>,
    },
    WifiContinuity {
        app_id: String,
        command: runtime_collab::WifiContinuityCommand,
        reply: mpsc::SyncSender<Result<runtime_collab::WifiContinuityReply, String>>,
    },
    Speakers {
        app_id: String,
        command: runtime_collab::SpeakerCommand,
        reply: mpsc::SyncSender<Result<runtime_collab::SpeakerReply, String>>,
    },
    DisplayControl {
        origin: runtime_collab::settings::DisplayOrigin,
        command: Vec<u8>,
        reply: mpsc::SyncSender<Result<Vec<u8>, String>>,
    },
    /// Send the app's unsent settings writes over the primary route.
    FlushSettings,
    /// Send the app's live activity over the primary route.
    FlushLiveActivity,
}

struct PrimaryHello {
    route: RouteSpec,
    record_kind: Option<String>,
    handle: Option<String>,
    /// Filled by the shovel when it starts the join, so the owner waits for
    /// the very deadline the shovel enforces.
    deadline: PrimaryDeadline,
}

/// How often a blocked owner looks for the deadline the shovel published.
const PRIMARY_DEADLINE_POLL: Duration = Duration::from_millis(50);

/// The primary-route deadline one `Hello`'s join runs under, shared between
/// the shovel that sets it and the owner that waits for it.
#[derive(Clone, Default)]
struct PrimaryDeadline(Arc<std::sync::Mutex<Option<Instant>>>);

impl PrimaryDeadline {
    fn start(&self) -> Instant {
        let deadline = crate::time::now()
            .checked_add(SDK_PRIMARY_ROUTE_TIMEOUT)
            .unwrap_or_else(crate::time::now);
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(deadline);
        deadline
    }

    fn get(&self) -> Option<Instant> {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SdkCommand {
    /// Automerge bytes of a document mutation command, if this is one.
    fn document_len(&self) -> Option<usize> {
        match self {
            Self::Primary(CollabRequest::Document { mutation, .. })
            | Self::ToRoute(_, CollabRequest::Document { mutation, .. }) => {
                Some(mutation.data().len())
            }
            // Content a record references travels like its documents: its
            // own budget, retried when full, never closing the connection.
            Self::Primary(CollabRequest::OfferContent { bytes, .. })
            | Self::ToRoute(_, CollabRequest::OfferContent { bytes, .. }) => Some(bytes.len()),
            _ => None,
        }
    }

    /// Release spare capacity so the retained budget charges only the bytes
    /// that will actually be sent.
    fn shrink_document(&mut self) {
        if let Self::Primary(CollabRequest::Document { mutation, .. })
        | Self::ToRoute(_, CollabRequest::Document { mutation, .. }) = self
        {
            match mutation {
                DocumentMutation::Snapshot(data) | DocumentMutation::Incremental(data) => {
                    data.shrink_to_fit();
                }
            }
        }
    }

    /// Presence is visual, short-lived state. All other frames represent a
    /// durable state change, an ordered request, or a lifecycle result.
    fn is_transient_presence(&self) -> bool {
        matches!(
            self,
            Self::Primary(CollabRequest::Presence { .. })
                | Self::ToRoute(_, CollabRequest::Presence { .. })
        )
    }
}

/// Checked byte meter for queue admission. `None` means a size computation
/// overflowed and is treated exactly like durable overload.
#[derive(Default)]
struct ByteMeter(Option<usize>);

impl ByteMeter {
    fn add(&mut self, bytes: usize) {
        self.0 = self.0.and_then(|total| total.checked_add(bytes));
    }

    fn add_product(&mut self, count: usize, bytes_each: usize) {
        self.0 = self.0.and_then(|total| {
            count
                .checked_mul(bytes_each)
                .and_then(|bytes| total.checked_add(bytes))
        });
    }

    fn string_capacity(&mut self, value: &String) {
        self.add(value.capacity());
    }

    fn bytes(&mut self, value: &Vec<u8>) {
        self.add(value.capacity());
    }

    fn finish(self) -> Option<usize> {
        self.0
    }
}

fn json_bytes(value: &serde_json::Value, meter: &mut ByteMeter) {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
        serde_json::Value::String(value) => meter.string_capacity(value),
        serde_json::Value::Array(values) => {
            meter.add_product(values.capacity(), std::mem::size_of::<serde_json::Value>());
            for value in values {
                json_bytes(value, meter);
            }
        }
        serde_json::Value::Object(values) => json_map_bytes(values, meter),
    }
}

fn json_map_bytes(values: &serde_json::Map<String, serde_json::Value>, meter: &mut ByteMeter) {
    // `serde_json::Map` is a BTreeMap unless preserve_order is
    // enabled. Charge a deliberately conservative node allowance so
    // either representation remains within this budget.
    meter.add_product(values.len(), 512);
    for (key, value) in values {
        meter.string_capacity(key);
        json_bytes(value, meter);
    }
}

/// Count a serializable opaque nested value without constructing a second
/// payload copy. Direct `String`/`Vec` owners still use capacities above;
/// this covers nested action/context representations whose concrete shape is
/// intentionally owned by `tilo-actions`.
fn serialized_bytes<T: serde::Serialize>(value: &T, meter: &mut ByteMeter) {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("serialized size overflow"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    if serde_json::to_writer(&mut counter, value).is_ok() {
        meter.add(counter.0);
    } else {
        meter.0 = None;
    }
}

fn exported_action_bytes(action: &ExportedAction, meter: &mut ByteMeter) {
    meter.string_capacity(&action.resource);
    if let Some(identity) = &action.semantic_identity {
        meter.string_capacity(&identity.kind);
        meter.string_capacity(&identity.key);
    }
    meter.string_capacity(&action.action);
    meter.string_capacity(&action.resource_kind);
    meter.string_capacity(&action.resource_label);
    meter.string_capacity(&action.action_label);
    if let Some(description) = &action.description {
        meter.string_capacity(description);
    }
    serialized_bytes(&action.params, meter);
    serialized_bytes(&action.contract, meter);
    if let ExportedInput::Text {
        prompt,
        placeholder,
        ..
    } = &action.input
    {
        meter.string_capacity(prompt);
        meter.string_capacity(placeholder);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive request meter must account for every bounded queue variant without a permissive fallback"
)]
fn request_bytes(request: &CollabRequest) -> Option<usize> {
    let mut meter = ByteMeter(Some(std::mem::size_of::<CollabRequest>()));
    match request {
        CollabRequest::Hello {
            app_id,
            record,
            record_kind,
            handle,
            ..
        } => {
            meter.string_capacity(app_id);
            if let Some(record) = record {
                meter.string_capacity(record);
            }
            if let Some(kind) = record_kind {
                meter.string_capacity(kind);
            }
            if let Some(handle) = handle {
                meter.string_capacity(handle);
            }
        }
        CollabRequest::Document {
            mutation,
            authorization,
        } => {
            match mutation {
                DocumentMutation::Snapshot(data) | DocumentMutation::Incremental(data) => {
                    meter.bytes(data)
                }
            }
            if let Some(authorization) = authorization {
                meter.0 = meter
                    .0
                    .and_then(|bytes| bytes.checked_add(authorization.retained_bytes()?));
            }
        }
        CollabRequest::SignedOperation { envelope } => meter.bytes(&envelope.payload),
        CollabRequest::Presence { key, payload } => {
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
        }
        CollabRequest::FetchScrollback { .. }
        | CollabRequest::OpenWorkspaceControl { .. }
        | CollabRequest::CloseWorkspaceControl { .. }
        | CollabRequest::RequestDocumentSnapshot
        | CollabRequest::PublishedInvocationStatus { .. }
        | CollabRequest::CancelPublishedInvocation { .. } => {}
        CollabRequest::LaunchPublishedApplication {
            app_id,
            content_hash,
            ..
        } => {
            meter.string_capacity(app_id);
            if let Some(content_hash) = content_hash {
                meter.string_capacity(content_hash);
            }
        }
        CollabRequest::LaunchInstalledApplication {
            app_id,
            package_root,
            ..
        } => {
            meter.string_capacity(app_id);
            if let Some(root) = package_root {
                meter.string_capacity(root);
            }
        }
        CollabRequest::RemoteAction { message } => serialized_bytes(message, &mut meter),
        CollabRequest::LocalFileClipboard { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::OfferContent { hash, bytes } => {
            meter.string_capacity(hash);
            meter.bytes(bytes);
        }
        CollabRequest::WantContent { hash } => meter.string_capacity(hash),
        CollabRequest::PublishActions { app_id, actions } => {
            meter.string_capacity(app_id);
            meter.add_product(actions.capacity(), std::mem::size_of::<ExportedAction>());
            for action in actions {
                exported_action_bytes(action, &mut meter);
            }
        }
        CollabRequest::ReportContinuation {
            app_id, metadata, ..
        } => {
            meter.string_capacity(app_id);
            meter.bytes(metadata);
        }
        CollabRequest::ReportPlayback {
            app_id,
            reports_json,
        } => {
            meter.string_capacity(app_id);
            meter.bytes(reports_json);
        }
        CollabRequest::NativeMediaCommand { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::RealtimeCommand {
            app_id, command, ..
        } => {
            meter.string_capacity(app_id);
            serialized_bytes(command, &mut meter);
        }
        CollabRequest::NearbyTransferCommand {
            app_id, command, ..
        } => {
            meter.string_capacity(app_id);
            serialized_bytes(command, &mut meter);
        }
        CollabRequest::ClearContinuation { app_id } => meter.string_capacity(app_id),
        CollabRequest::ReportHandoff { app_id, message } => {
            meter.string_capacity(app_id);
            meter.bytes(message);
        }
        CollabRequest::WorkspaceControlInput { event, .. } => serialized_bytes(event, &mut meter),
        CollabRequest::CompleteInvocation { code, message, .. } => {
            meter.string_capacity(code);
            meter.string_capacity(message);
        }
        CollabRequest::UpdateInvocation { update, .. } => serialized_bytes(update, &mut meter),
        CollabRequest::RecordLocalEffect { address, update } => {
            meter.string_capacity(&address.resource);
            meter.string_capacity(&address.action);
            serialized_bytes(update, &mut meter);
        }
        #[cfg(unix)]
        CollabRequest::ManagedInvocationAttempt { attempt } => meter.add(attempt.meter_bytes()),
        CollabRequest::QueryActionDirectory { query, .. } => {
            directory_client::query_bytes(query, &mut meter)
        }
        CollabRequest::ResolvePublishedAction {
            revision,
            resource,
            action,
            ..
        } => {
            meter.string_capacity(revision);
            meter.string_capacity(resource);
            meter.string_capacity(action);
        }
        CollabRequest::InvokeResolvedPublishedAction {
            revision,
            resource,
            action,
            arguments,
            confirmation,
            ..
        } => {
            meter.string_capacity(revision);
            meter.string_capacity(resource);
            meter.string_capacity(action);
            json_map_bytes(arguments, &mut meter);
            json_bytes(confirmation, &mut meter);
        }
        CollabRequest::InvokePublishedAction {
            resource,
            action,
            arguments,
            confirmation,
            ..
        } => {
            meter.string_capacity(resource);
            meter.string_capacity(action);
            json_map_bytes(arguments, &mut meter);
            json_bytes(confirmation, &mut meter);
        }
        CollabRequest::SubscribeEnrolledTasks { .. } => {}
        CollabRequest::AcceptEnrolledTask { acceptance, .. } => {
            serialized_bytes(acceptance, &mut meter)
        }
        CollabRequest::ReportEnrolledTask {
            acceptance, update, ..
        } => {
            serialized_bytes(acceptance, &mut meter);
            serialized_bytes(update, &mut meter);
        }
        CollabRequest::ReportTaskRun { lease, update, .. } => {
            meter.string_capacity(lease);
            serialized_bytes(update, &mut meter);
        }
        CollabRequest::InvokeTaskAction {
            lease,
            resource,
            action,
            arguments,
            ..
        } => {
            meter.string_capacity(lease);
            meter.string_capacity(resource);
            meter.string_capacity(action);
            json_map_bytes(arguments, &mut meter);
        }
        CollabRequest::TaskRunCancellation { lease, .. } => meter.string_capacity(lease),
    }
    meter.finish()
}

fn command_bytes(command: &SdkCommand) -> Option<usize> {
    match command {
        SdkCommand::Initialize { route, request, .. } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            if let runtime_collab::committed_document::Request::Initialize {
                kind, document, ..
            } = request
            {
                meter.string_capacity(kind);
                meter.add(document.capacity());
            }
            meter.finish()
        }
        SdkCommand::Hello(hello) => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&hello.route.app_id);
            if let Some(record) = &hello.route.record {
                meter.string_capacity(record);
            }
            if let Some(kind) = &hello.record_kind {
                meter.string_capacity(kind);
            }
            if let Some(handle) = &hello.handle {
                meter.string_capacity(handle);
            }
            meter.finish()
        }
        SdkCommand::Primary(request) => request_bytes(request),
        SdkCommand::ToRoute(route, request) => {
            let mut meter = ByteMeter(request_bytes(request));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::Join(route, record_kind) => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            if let Some(kind) = record_kind {
                meter.string_capacity(kind);
            }
            meter.finish()
        }
        SdkCommand::PrimaryReady(_)
        | SdkCommand::PublicationBarrier(_)
        | SdkCommand::FlushSettings
        | SdkCommand::FlushLiveActivity => Some(std::mem::size_of::<SdkCommand>()),
        // The group was allocated by the caller before entering the bridge,
        // and installation is synchronous under `&mut self`, so at most one
        // is ever queued. Only the command itself is counted: reserving the
        // whole budget would fail closed whenever any other command was still
        // queued, or its reservation not yet released after the reply.
        SdkCommand::InstallEncryption { spec, .. } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&spec.app_id);
            if let Some(record) = &spec.record {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::PublishActionDirectory { app_id, .. } => {
            std::mem::size_of::<SdkCommand>().checked_add(app_id.capacity())
        }
        SdkCommand::WifiContinuity {
            app_id, command, ..
        } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(app_id);
            serialized_bytes(command, &mut meter);
            meter.finish()
        }
        SdkCommand::DisplayControl { command, .. } => {
            std::mem::size_of::<SdkCommand>().checked_add(command.capacity())
        }
        SdkCommand::Speakers {
            app_id, command, ..
        } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(app_id);
            serialized_bytes(command, &mut meter);
            meter.finish()
        }
    }
}

fn presence_payload_bytes(payload: &PresencePayload, meter: &mut ByteMeter) {
    if let PresencePayload::Following {
        target: Some(target),
    } = payload
    {
        meter.string_capacity(target);
    }
    if let PresencePayload::Interaction(update) = payload {
        serialized_bytes(update.as_ref(), meter);
    }
    if let PresencePayload::Status { text, .. } = payload {
        meter.string_capacity(text);
    }
}

fn workspace_control_event_bytes(
    event: &runtime_collab::WorkspaceControlEvent,
    meter: &mut ByteMeter,
) {
    match event {
        runtime_collab::WorkspaceControlEvent::Frame { encoded_h264, .. } => {
            meter.bytes(encoded_h264);
        }
        runtime_collab::WorkspaceControlEvent::Closed(
            runtime_collab::WorkspaceControlCloseReason::Denied(message),
        ) => {
            meter.string_capacity(message);
        }
        _ => {}
    }
}

fn context_event_bytes(
    collaborators: &[Collaborator],
    collaborator_capacity: usize,
    permissions: &[String],
    permission_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(collaborator_capacity, std::mem::size_of::<Collaborator>());
    for collaborator in collaborators {
        meter.string_capacity(&collaborator.id);
        meter.string_capacity(&collaborator.name);
    }
    meter.add_product(permission_capacity, std::mem::size_of::<String>());
    for permission in permissions {
        meter.string_capacity(permission);
    }
}

/// Carry one request frame to the session's action host and decode its
/// answer. Every request is answered — a transport failure becomes the
/// refusal the request would have received — so a caller waiting on its
/// invocation id or correlation always hears back.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn remote_action_round_trip(
    channel: &runtime_collab::ChannelHandle,
    message: tilo_actions::remote::RemoteActionMessage,
) -> tilo_actions::remote::RemoteActionMessage {
    use tilo_actions::remote::RemoteActionFrame;

    let answer = async {
        let frame = RemoteActionFrame::new(message.clone())
            .encode()
            .map_err(|error| error.to_string())?;
        let reply = channel
            .remote_action(&frame)
            .await
            .map_err(|error| error.to_string())?;
        RemoteActionFrame::decode(&reply)
            .map(|frame| frame.message)
            .map_err(|error| error.to_string())
    }
    .await;
    answer.unwrap_or_else(|reason| {
        tilo_actions::remote::refuse(
            &message,
            tilo_actions::remote::ActionHostError::Unavailable { reason },
        )
        .unwrap_or(message)
    })
}

fn broker_action_bytes(action: &BrokerActionDescriptor, meter: &mut ByteMeter) {
    meter.string_capacity(&action.resource);
    if let Some(identity) = &action.semantic_identity {
        meter.string_capacity(&identity.kind);
        meter.string_capacity(&identity.key);
    }
    meter.string_capacity(&action.action);
    meter.string_capacity(&action.resource_kind);
    meter.string_capacity(&action.resource_label);
    meter.string_capacity(&action.action_label);
    if let Some(description) = &action.description {
        meter.string_capacity(description);
    }
    meter.add_product(
        action.params.capacity(),
        std::mem::size_of::<tilo_actions::ActionParam>(),
    );
    for param in &action.params {
        serialized_bytes(param, meter);
    }
    serialized_bytes(&action.contract, meter);
    meter.string_capacity(&action.input.kind);
    meter.string_capacity(&action.input.prompt);
    meter.string_capacity(&action.input.placeholder);
}

fn action_catalogue_bytes(
    surfaces: &[BrokerActionSurface],
    surface_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(surface_capacity, std::mem::size_of::<BrokerActionSurface>());
    for surface in surfaces {
        meter.string_capacity(&surface.app_id);
        meter.string_capacity(&surface.cluster_id);
        meter.string_capacity(&surface.record_id);
        meter.add_product(
            surface.actions.capacity(),
            std::mem::size_of::<BrokerActionDescriptor>(),
        );
        for action in &surface.actions {
            broker_action_bytes(action, meter);
        }
    }
}

fn scrollback_batch_bytes(
    ops: &[Box<SignedOp>],
    op_capacity: usize,
    decrypted_payloads: &[Option<Vec<u8>>],
    payload_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(op_capacity, std::mem::size_of::<Box<SignedOp>>());
    for op in ops {
        meter.bytes(&op.payload);
    }
    meter.add_product(payload_capacity, std::mem::size_of::<Option<Vec<u8>>>());
    for payload in decrypted_payloads.iter().flatten() {
        meter.bytes(payload);
    }
}

fn task_run_bytes(run: &runtime_collab::TaskRunEnvelope, meter: &mut ByteMeter) {
    meter.string_capacity(&run.task_id);
    meter.string_capacity(&run.title);
    meter.string_capacity(&run.lease);
    json_bytes(&run.action_context, meter);
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive event meter must account for every bounded queue variant without a permissive fallback"
)]
fn event_diagnostics(event: &CollabEvent) -> (&'static str, Option<usize>) {
    let mut meter = ByteMeter(Some(std::mem::size_of::<CollabEvent>()));
    let kind = match event {
        CollabEvent::CommittedDocument { route, .. } => {
            // Capture bytes retain their own process-wide preallocation permit.
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            "CommittedDocument"
        }
        CollabEvent::Context {
            collaborators,
            permissions,
        } => {
            context_event_bytes(
                collaborators,
                collaborators.capacity(),
                permissions,
                permissions.capacity(),
                &mut meter,
            );
            "Context"
        }
        CollabEvent::FollowChanged { peer_id } => {
            if let Some(peer_id) = peer_id {
                meter.string_capacity(peer_id);
            }
            "FollowChanged"
        }
        CollabEvent::Handoff { request } => {
            meter.add_product(1, std::mem::size_of_val(request));
            if let super::handoff::HandoffRequest::OwnerView { view } = request {
                meter.string_capacity(&view.room);
                meter.string_capacity(&view.app);
                meter.string_capacity(&view.source);
                if let Some(metadata) = &view.projection {
                    meter.add_product(metadata.bytes.capacity(), 1);
                }
            }
            if let super::handoff::HandoffRequest::Retire { successor, .. } = request {
                meter.string_capacity(&successor.room);
                meter.string_capacity(&successor.record);
                meter.string_capacity(&successor.by);
            }
            "Handoff"
        }
        CollabEvent::DocumentSnapshotRequested { route } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            "DocumentSnapshotRequested"
        }
        CollabEvent::ChannelAccess { route, .. } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            "ChannelAccess"
        }
        CollabEvent::RemoteCursorMoved { user_id, .. } => {
            meter.string_capacity(user_id);
            "RemoteCursorMoved"
        }
        CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload,
        } => {
            meter.string_capacity(user_id);
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
            "RemotePresenceUpdate"
        }
        CollabEvent::ActionArrived {
            type_id,
            payload,
            from_user_id,
        } => {
            meter.string_capacity(type_id);
            meter.bytes(payload);
            meter.string_capacity(from_user_id);
            "ActionArrived"
        }
        CollabEvent::Document {
            envelope,
            from_user_id,
            decrypted_payload,
        } => {
            meter.bytes(&envelope.payload);
            meter.string_capacity(from_user_id);
            if let Some(payload) = decrypted_payload {
                meter.bytes(payload);
            }
            "Document"
        }
        CollabEvent::ScrollbackBatch {
            ops,
            decrypted_payloads,
            ..
        } => {
            scrollback_batch_bytes(
                ops,
                ops.capacity(),
                decrypted_payloads,
                decrypted_payloads.capacity(),
                &mut meter,
            );
            "ScrollbackBatch"
        }
        CollabEvent::Invocation {
            resource,
            action,
            argument,
            actor,
            context,
            ..
        } => {
            meter.string_capacity(resource);
            meter.string_capacity(action);
            if let Some(argument) = argument {
                meter.string_capacity(argument);
            }
            meter.string_capacity(actor);
            if let Some(context) = context {
                serialized_bytes(context.as_ref(), &mut meter);
            }
            "Invocation"
        }
        CollabEvent::InvocationCancellation { .. } => "InvocationCancellation",
        CollabEvent::RemoteAction { message } => {
            serialized_bytes(message, &mut meter);
            "RemoteAction"
        }
        CollabEvent::ActionCatalogue { surfaces } => {
            action_catalogue_bytes(surfaces, surfaces.capacity(), &mut meter);
            "ActionCatalogue"
        }
        CollabEvent::ActionDirectoryPage { result, .. } => {
            match result {
                Ok(page) => directory_client::page_bytes(page, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "ActionDirectoryPage"
        }
        CollabEvent::PublishedActionResolution { result, .. } => {
            match result {
                Ok(resolution) => directory_client::resolution_bytes(resolution, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "PublishedActionResolution"
        }
        #[cfg(unix)]
        CollabEvent::ManagedInvocationObservation { result } => {
            meter.add(result.meter_bytes());
            "ManagedInvocationObservation"
        }
        CollabEvent::PublishedInvocationResult { result, .. } => {
            match result {
                Ok((_, update)) => serialized_bytes(update, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "PublishedInvocationResult"
        }
        CollabEvent::InstalledApplicationLaunchResult { result, .. } => {
            match result {
                Ok((_, update)) => serialized_bytes(update, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "InstalledApplicationLaunchResult"
        }
        CollabEvent::TaskActionInvocationResult {
            result,
            continuation,
            ..
        } => {
            match result {
                Ok((_, update)) => serialized_bytes(update, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            if let Some(continuation) = continuation {
                meter.add(continuation.capacity());
            }
            "TaskActionInvocationResult"
        }
        CollabEvent::EnrolledTask(run) => {
            serialized_bytes(run, &mut meter);
            "EnrolledTask"
        }
        CollabEvent::EnrolledTaskReply { result, .. } => {
            if let Err(error) = result {
                meter.string_capacity(error);
            }
            "EnrolledTaskReply"
        }
        CollabEvent::EnrolledTaskDisconnected { detail } => {
            meter.string_capacity(detail);
            "EnrolledTaskDisconnected"
        }
        CollabEvent::TaskRun(run) => {
            task_run_bytes(run, &mut meter);
            "TaskRun"
        }
        CollabEvent::TaskRunCancellation { result, .. } => {
            if let Err(error) = result {
                meter.string_capacity(error);
            }
            "TaskRunCancellation"
        }
        CollabEvent::WorkspaceControl { event, .. } => {
            workspace_control_event_bytes(event, &mut meter);
            "WorkspaceControl"
        }
        CollabEvent::NativeMediaCommandResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "NativeMediaCommandResult"
        }
        CollabEvent::RecordContent { route, hash, bytes } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.string_capacity(hash);
            meter.bytes(bytes);
            "RecordContent"
        }
        CollabEvent::RealtimeCommandResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "RealtimeCommandResult"
        }
        CollabEvent::LocalFileClipboardResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "LocalFileClipboardResult"
        }
        CollabEvent::NearbyTransferCommandResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "NearbyTransferCommandResult"
        }
        CollabEvent::Realtime(event) => {
            serialized_bytes(event, &mut meter);
            "Realtime"
        }
    };
    (kind, meter.finish())
}

/// Content-free names share the meter's exhaustive, platform-aware match.
fn event_kind(event: &CollabEvent) -> &'static str {
    event_diagnostics(event).0
}

fn event_bytes(event: &CollabEvent) -> Option<usize> {
    event_diagnostics(event).1
}

/// Shared failure latch. The UI reads it through `is_connected()` even when
/// overload is detected by a subscription task on the shovel thread.
#[derive(Clone)]
struct BridgeState {
    document_reads: committed_document::Reads,
    healthy: Arc<AtomicBool>,
    publication_failure: Arc<std::sync::Mutex<Option<DocumentPublicationError>>>,
    /// The broker refused or failed an action-surface publication; the owner
    /// takes this and republishes.
    action_surface_failed: Arc<AtomicBool>,
    action_directory_failed: Arc<AtomicBool>,
    directory_rpc_successes: Arc<AtomicU64>,
    directory_rpc_failures: Arc<AtomicU64>,
    directory_observation_enabled: Arc<AtomicBool>,
    command_bytes: ByteBudget,
    event_bytes: ByteBudget,
    document_command_bytes: ByteBudget,
    document_event_bytes: ByteBudget,
    owner_wake: crate::wake::WakeHandle,
    wake_pending: Arc<AtomicBool>,
    /// How far the shovel has got establishing the session. Written by the
    /// shovel, read by the owner through [`CollabTransport::status`].
    connect: Arc<std::sync::Mutex<ConnectPhase>>,
}

/// Progress of the shovel's connection setup.
#[derive(Clone, Debug)]
enum ConnectPhase {
    /// Loading the join identity and reaching the broker.
    Connecting,
    /// The broker client is up; commands are being served.
    Connected,
    /// Setup failed; the shovel has exited.
    Failed(String),
}

/// Lock-free aggregate reservation for heap payload retained by one queue.
/// A reservation moves with its queued value and releases automatically when
/// the receiver consumes or drops that value, waking anyone waiting for room.
#[derive(Clone)]
struct ByteBudget(Arc<AtomicUsize>, Arc<tokio::sync::Notify>, Option<RoomWake>);

/// Wakes the owner once outbound room frees after one of its sends was
/// refused as busy. The owner has no timer of its own for that retry, so
/// without this a refused edit would wait for unrelated input.
#[derive(Clone)]
struct RoomWake {
    wanted: Arc<AtomicBool>,
    owner: crate::wake::WakeHandle,
}

struct ByteReservation {
    budget: ByteBudget,
    bytes: usize,
}

impl ByteBudget {
    fn new() -> Self {
        Self(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(tokio::sync::Notify::new()),
            None,
        )
    }

    fn with_room_wake(room: RoomWake) -> Self {
        Self(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(tokio::sync::Notify::new()),
            Some(room),
        )
    }

    /// Ask for an owner wake at the next release of any reservation sharing
    /// this budget's latch. Arm before re-checking for room, so a release
    /// racing the refusal is never missed.
    fn arm_room_wake(&self) {
        if let Some(room) = &self.2 {
            room.wanted.store(true, Ordering::Release);
        }
    }

    fn reserve(&self, bytes: usize, limit: usize) -> Option<ByteReservation> {
        self.reserve_with(bytes, limit, false)
    }

    /// Like [`Self::reserve`], but an otherwise empty budget admits one item
    /// larger than `limit`. Retained memory stays bounded by
    /// `max(limit, largest single item)`, and a lone large item can always
    /// make progress instead of waiting for space that can never appear.
    fn reserve_sole(&self, bytes: usize, limit: usize) -> Option<ByteReservation> {
        self.reserve_with(bytes, limit, true)
    }

    fn reserve_with(&self, bytes: usize, limit: usize, sole: bool) -> Option<ByteReservation> {
        let mut used = self.0.load(Ordering::Acquire);
        loop {
            let next = used.checked_add(bytes)?;
            if next > limit && !(sole && used == 0) {
                return None;
            }
            match self
                .0
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Some(ByteReservation {
                        budget: self.clone(),
                        bytes,
                    });
                }
                Err(observed) => used = observed,
            }
        }
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.bytes, Ordering::AcqRel);
        self.budget.1.notify_waiters();
        if let Some(room) = &self.budget.2
            && room.wanted.swap(false, Ordering::AcqRel)
        {
            room.owner.notify_events();
        }
    }
}

struct QueuedSdkCommand {
    command: SdkCommand,
    _reservation: ByteReservation,
}

enum EventTurn {
    Snapshot(tokio::sync::OwnedSemaphorePermit),
    Document(Vec<tokio::sync::OwnedSemaphorePermit>),
}

struct QueuedCollabEvent {
    event: CollabEvent,
    _reservation: ByteReservation,
    /// Snapshot turns release on dequeue. Native document turns transfer to
    /// the owner until its dispatch batch ends.
    _turn: Option<EventTurn>,
}

impl BridgeState {
    fn new() -> Self {
        let owner_wake = crate::wake::WakeHandle::new();
        // Both outbound budgets share one latch: a refused document send is
        // queued behind commands of either kind.
        let outbound_room = RoomWake {
            wanted: Arc::new(AtomicBool::new(false)),
            owner: owner_wake.clone(),
        };
        Self {
            document_reads: committed_document::Reads::default(),
            healthy: Arc::new(AtomicBool::new(true)),
            publication_failure: Arc::new(std::sync::Mutex::new(None)),
            action_surface_failed: Arc::new(AtomicBool::new(false)),
            action_directory_failed: Arc::new(AtomicBool::new(false)),
            directory_rpc_successes: Arc::new(AtomicU64::new(0)),
            directory_rpc_failures: Arc::new(AtomicU64::new(0)),
            directory_observation_enabled: Arc::new(AtomicBool::new(false)),
            command_bytes: ByteBudget::with_room_wake(outbound_room.clone()),
            event_bytes: ByteBudget::new(),
            document_command_bytes: ByteBudget::with_room_wake(outbound_room),
            document_event_bytes: ByteBudget::new(),
            owner_wake,
            wake_pending: Arc::new(AtomicBool::new(false)),
            connect: Arc::new(std::sync::Mutex::new(ConnectPhase::Connecting)),
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn connect_phase(&self) -> ConnectPhase {
        self.connect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Record setup progress and wake the owner to observe it.
    fn set_connect_phase(&self, phase: ConnectPhase) {
        *self
            .connect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = phase;
        self.notify_events();
    }

    fn notify_events(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            self.owner_wake.notify_events();
        }
    }

    fn begin_poll(&self) {
        // Reset BEFORE draining: a publication racing the drain either lands
        // in this batch or schedules another wake. Clearing after draining
        // could swallow a notification for an event we did not consume.
        self.wake_pending.store(false, Ordering::Release);
    }

    fn publication_result(&self) -> Result<(), DocumentPublicationError> {
        match self.publication_failure.lock() {
            Ok(failure) => failure.clone().map_or(Ok(()), Err),
            Err(_) => Err(DocumentPublicationError::Failed(
                "publication failure state poisoned".into(),
            )),
        }
    }

    fn record_publication_failure(&self, error: DocumentPublicationError) {
        if let Ok(mut failure) = self.publication_failure.lock() {
            failure.get_or_insert(error);
        }
    }

    fn publication_failed(
        &self,
        context: &'static str,
        error: impl std::fmt::Display,
        disconnect: bool,
    ) -> DocumentPublicationError {
        let failure = DocumentPublicationError::Failed(format!("{context}: {error}"));
        self.record_publication_failure(failure.clone());
        if disconnect {
            self.fail_closed(context);
        }
        failure
    }

    fn fail_closed(&self, direction: &'static str) {
        self.fail_closed_with_rpc_error(direction, None);
    }

    fn fail_closed_with_rpc_error(
        &self,
        direction: &'static str,
        rejected: Option<(u64, &runtime_portal::NotAvailable)>,
    ) {
        // Latch before withdrawing health or dropping any queued receipt sender.
        self.record_publication_failure(DocumentPublicationError::Failed(direction.into()));
        if self.healthy.swap(false, Ordering::AcqRel) {
            if let Some((invocation_id, error)) = rejected {
                let (rpc_kind, rpc_error, rpc_error_truncated) = bounded_rpc_diagnostic(error);
                tracing::error!(
                    direction,
                    invocation_id,
                    rpc_kind,
                    rpc_error,
                    rpc_error_truncated,
                    "collab: bounded bridge overloaded or disconnected; disabling collaboration"
                );
            } else {
                tracing::error!(
                    direction,
                    "collab: bounded bridge overloaded or disconnected; disabling collaboration"
                );
            }
            self.notify_events();
        }
    }
}

/// Keep the rejected RPC's cause, never its request/update payload. A peer's
/// diagnostic is untrusted text: bound its allocation and remove control
/// characters before placing it in the ordinary session log.
fn bounded_rpc_diagnostic(error: &runtime_portal::NotAvailable) -> (&'static str, String, bool) {
    match error {
        runtime_portal::NotAvailable::Capnp(error) => {
            let mut chars = error.extra.chars();
            let detail: String = chars
                .by_ref()
                .take(512)
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect();
            let kind = match error.kind {
                capnp::ErrorKind::Failed => "capnp_failed",
                capnp::ErrorKind::Overloaded => "capnp_overloaded",
                capnp::ErrorKind::Disconnected => "capnp_disconnected",
                capnp::ErrorKind::Unimplemented => "capnp_unimplemented",
                _ => "capnp_protocol_error",
            };
            (kind, detail, chars.next().is_some())
        }
        runtime_portal::NotAvailable::ServiceFdMissing => {
            ("service_fd_missing", String::new(), false)
        }
        runtime_portal::NotAvailable::ConnectionFailed(error) => {
            ("connection_failed", format!("{:?}", error.kind()), false)
        }
        runtime_portal::NotAvailable::ProtocolMismatch { expected, got } => (
            "protocol_mismatch",
            format!("expected {expected}, got {got}"),
            false,
        ),
    }
}

/// Non-blocking UI enqueue. Presence may be dropped when the finite queue is
/// full; a durable command makes the whole bridge unavailable instead.
fn enqueue_sdk_command(
    tx: &tokio_mpsc::Sender<QueuedSdkCommand>,
    command: SdkCommand,
    state: &BridgeState,
) -> bool {
    try_enqueue_sdk_command(tx, command, state).is_ok()
}

/// Enqueue with an explicit outcome. Document mutations are checked against
/// the transport limit here, before anything is queued, and use their own
/// budget: a full budget is [`TransportError::Busy`] (retry later) and an
/// over-limit change is [`TransportError::TooLarge`]. Neither disables
/// collaboration. Other durable commands keep the fail-closed policy.
fn try_enqueue_sdk_command(
    tx: &tokio_mpsc::Sender<QueuedSdkCommand>,
    mut command: SdkCommand,
    state: &BridgeState,
) -> Result<(), TransportError> {
    if !state.is_healthy() {
        return Err(TransportError::Closed);
    }
    if let Some(len) = command.document_len() {
        if let Err(collab_types::DocumentMutationEncodeError::TooLarge { actual, max }) =
            collab_types::document_transfer::check_transfer_len(len)
        {
            tracing::error!(
                bytes = actual,
                limit = max,
                "collab: document change exceeds the transfer limit; not sent"
            );
            return Err(TransportError::TooLarge { actual, max });
        }
        command.shrink_document();
        let bytes = command_bytes(&command).unwrap_or(usize::MAX);
        let budget = &state.document_command_bytes;
        let Some(reservation) = budget
            .reserve_sole(bytes, SDK_DOCUMENT_COMMAND_BYTES)
            .or_else(|| {
                budget.arm_room_wake();
                budget.reserve_sole(bytes, SDK_DOCUMENT_COMMAND_BYTES)
            })
        else {
            tracing::debug!(
                bytes = len,
                "collab: document budget full; change will be retried"
            );
            return Err(TransportError::Busy);
        };
        return match tx.try_send(QueuedSdkCommand {
            command,
            _reservation: reservation,
        }) {
            Ok(()) => Ok(()),
            Err(tokio_mpsc::error::TrySendError::Full(refused)) => {
                // Release the refused reservation before arming, or its own
                // drop would spend the wake on a retry that finds the queue
                // still full.
                drop(refused);
                budget.arm_room_wake();
                if tx.capacity() > 0 {
                    state.owner_wake.notify_events();
                }
                Err(TransportError::Busy)
            }
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                state.fail_closed("outbound command queue");
                Err(TransportError::Closed)
            }
        };
    }
    let transient = command.is_transient_presence();
    let Some(reservation) = state.command_bytes.reserve(
        command_bytes(&command).unwrap_or(usize::MAX),
        SDK_COMMAND_QUEUE_BYTES,
    ) else {
        if transient {
            return Err(TransportError::Shed);
        }
        state.fail_closed("outbound command byte budget");
        return Err(TransportError::Closed);
    };
    match tx.try_send(QueuedSdkCommand {
        command,
        _reservation: reservation,
    }) {
        Ok(()) => Ok(()),
        Err(tokio_mpsc::error::TrySendError::Full(_)) if transient => Err(TransportError::Shed),
        Err(_) => {
            state.fail_closed("outbound command queue");
            Err(TransportError::Closed)
        }
    }
}

/// Bounded inbound half with the same split between durable events and
/// explicitly lossy presence. Stream tasks stop after a durable failure, so
/// they cannot keep pulling broker frames after the connection is failed.
#[derive(Clone)]
struct InboundBridge {
    tx: tokio_mpsc::Sender<QueuedCollabEvent>,
    state: BridgeState,
    reassembly: Arc<DocumentReassembly>,
}

/// Endpoint reassembly of segmented document mutations, shared by every
/// route of one manager so the memory bound covers them all. Live traffic
/// and history pages are tracked separately because history arrives newest
/// page first.
#[derive(Default)]
struct DocumentReassembly {
    live: std::sync::Mutex<collab_types::DocumentReassembler>,
    history: std::sync::Mutex<collab_types::DocumentReassembler>,
}

impl DocumentReassembly {
    /// Feed one validated document envelope and its endpoint payload (MLS
    /// plaintext, or `None` for cleartext). A single-frame mutation passes
    /// through unchanged as `Some(payload)`. A segment is held until its
    /// transfer completes; the complete typed payload is then returned
    /// exactly once as `Some(Some(bytes))`. `None` means nothing to deliver.
    #[allow(
        clippy::option_option,
        reason = "nothing to deliver, a pass-through payload, or a completed transfer are distinct outcomes"
    )]
    fn complete(
        reassembler: &std::sync::Mutex<collab_types::DocumentReassembler>,
        envelope: &SignedOp,
        payload: Option<Vec<u8>>,
    ) -> Option<Option<Vec<u8>>> {
        let frame = match collab_types::DocumentFrame::decode(
            payload.as_deref().unwrap_or(&envelope.payload),
        ) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "collab: document frame does not decode; dropping");
                return None;
            }
        };
        let segment = match frame {
            collab_types::DocumentFrame::Whole(_) => return Some(payload),
            collab_types::DocumentFrame::Segment(segment) => segment,
        };
        let Ok(mut reassembler) = reassembler.lock() else {
            tracing::error!("collab: document reassembly state is poisoned; dropping segment");
            return None;
        };
        let result = reassembler.accept(envelope, segment, crate::time::now());
        for abandoned in reassembler.take_abandoned() {
            tracing::warn!(
                artifact = ?abandoned.artifact,
                bytes = abandoned.total_len,
                received = abandoned.received_segments,
                segments = abandoned.segment_count,
                reason = ?abandoned.reason,
                "collab: incomplete document transfer abandoned; a fresh snapshot is required"
            );
        }
        drop(reassembler);
        match result {
            Ok(Some(done)) => {
                tracing::debug!(
                    target: "tilo.state",
                    event = "document_reassembled",
                    bytes = done.len,
                    "reassembled segmented document mutation"
                );
                Some(Some(done.payload))
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "collab: segmented document transfer rejected");
                None
            }
        }
    }
}

impl InboundBridge {
    fn new(tx: tokio_mpsc::Sender<QueuedCollabEvent>, state: BridgeState) -> Self {
        Self {
            tx,
            state,
            reassembly: Arc::default(),
        }
    }

    /// Deliver a document event without ever disconnecting for size: wait
    /// for the owner to drain earlier document events when the document
    /// budget is full. Returns false only when the bridge has failed or the
    /// owner went away.
    async fn send_document(&self, event: CollabEvent) -> bool {
        let bytes = event_bytes(&event).unwrap_or(usize::MAX);
        let budget = &self.state.document_event_bytes;
        let reservation = loop {
            let freed = budget.1.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if let Some(reservation) = budget.reserve_sole(bytes, SDK_DOCUMENT_EVENT_BYTES) {
                break reservation;
            }
            if !self.state.is_healthy() {
                return false;
            }
            let _ = tokio::time::timeout(SDK_DOCUMENT_EVENT_RETRY, freed).await;
        };
        if self
            .tx
            .send(QueuedCollabEvent {
                event,
                _reservation: reservation,
                _turn: None,
            })
            .await
            .is_ok()
        {
            self.state.notify_events();
            true
        } else {
            self.state.fail_closed("inbound document queue");
            false
        }
    }

    fn send_durable(&self, event: CollabEvent) -> bool {
        self.send_durable_turn(event, None, None)
    }

    /// Retain the producer turn through its existing owner acknowledgement.
    fn send_durable_turn(
        &self,
        event: CollabEvent,
        turn: Option<EventTurn>,
        serialized_bytes: Option<usize>,
    ) -> bool {
        let required_bytes = event_bytes(&event).unwrap_or(usize::MAX);
        let Some(reservation) = self
            .state
            .event_bytes
            .reserve(required_bytes, SDK_EVENT_QUEUE_BYTES)
        else {
            let (_catalogue_kind, surface_count, action_count) = match &event {
                CollabEvent::ActionCatalogue { surfaces } => (
                    "action_catalogue",
                    surfaces.len(),
                    surfaces
                        .iter()
                        .map(|surface| surface.actions.len())
                        .sum::<usize>(),
                ),
                _ => ("control", 0, 0),
            };
            tracing::error!(
                event_kind = event_kind(&event),
                ?serialized_bytes,
                remaining_bytes = SDK_EVENT_QUEUE_BYTES
                    .saturating_sub(self.state.event_bytes.0.load(Ordering::Acquire)),
                required_bytes,
                reserved_bytes = self.state.event_bytes.0.load(Ordering::Acquire),
                limit_bytes = SDK_EVENT_QUEUE_BYTES,
                surface_count,
                action_count,
                "collab: inbound control event exceeds its retained byte budget"
            );
            self.state.fail_closed("inbound event byte budget");
            return false;
        };
        if self
            .tx
            .try_send(QueuedCollabEvent {
                event,
                _reservation: reservation,
                _turn: turn,
            })
            .is_ok()
        {
            self.state.notify_events();
            true
        } else {
            self.state.fail_closed("inbound event queue");
            false
        }
    }

    /// Returns false only when the receiver has gone away. A full queue is a
    /// permitted transient-presence drop and does not terminate the stream.
    fn send_presence(&self, event: CollabEvent) -> bool {
        let Some(reservation) = self.state.event_bytes.reserve(
            event_bytes(&event).unwrap_or(usize::MAX),
            SDK_EVENT_QUEUE_BYTES,
        ) else {
            return true;
        };
        match self.tx.try_send(QueuedCollabEvent {
            event,
            _reservation: reservation,
            _turn: None,
        }) {
            Ok(()) => {
                self.state.notify_events();
                true
            }
            Err(tokio_mpsc::error::TrySendError::Full(_)) => true,
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                self.state.fail_closed("inbound presence queue");
                false
            }
        }
    }
}

/// A one-shot host handoff factory, evaluated on the SDK worker's `LocalSet`.
///
/// The platform host supplies this to shared toolkit bootstrap. It must verify
/// host/app/workspace handoff authority before returning its two connections.
/// This type neither authenticates a transport nor certifies a supplied grant.
/// The actual brokers still enforce certified joins and live signing policy.
/// No inherited service or app-local identity is used if the factory fails.
///
/// Consumed on both success and failure. There is no automatic reconnect. A
/// host retry must create a fresh connector and repeat admission for the same
/// durable identity and requested route; an old handoff must not be replayed.
pub struct HostSessionConnector {
    connect: Box<dyn FnOnce(RouteSpec) -> HostSessionFuture + Send>,
    identity_exchange_timeout: Duration,
}

trait HostAsyncIo: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> HostAsyncIo for T {}
type HostAsyncStream = Pin<Box<dyn HostAsyncIo>>;

type HostSessionFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<(HostAsyncStream, HostAsyncStream), runtime_portal::NotAvailable>,
            >,
    >,
>;

impl HostSessionConnector {
    /// Supply a factory for an owned collaboration stream and broker signer.
    ///
    /// The factory runs only inside the SDK worker's Tokio runtime/LocalSet and
    /// returns separate owned collaboration and identity transports. Only the
    /// factory and its captures cross threads; its future and streams may be
    /// !Send. It must authenticate both endpoints and enforce the requested
    /// route. Shared code frames and bounds every identity exchange async.
    /// Async setup is bounded by the existing shovel initialization timeout.
    ///
    /// Merely returning metadata does not establish authentication. Do not use
    /// this constructor as a replacement for host transport/grant validation.
    pub fn new<F, Fut, C, I>(connect: F) -> Self
    where
        F: FnOnce(RouteSpec) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(C, I), runtime_portal::NotAvailable>> + 'static,
        C: AsyncRead + AsyncWrite + 'static,
        I: AsyncRead + AsyncWrite + 'static,
    {
        Self {
            connect: Box::new(move |route| {
                Box::pin(async move {
                    let (collaboration, identity) = connect(route).await?;
                    Ok((
                        Box::pin(collaboration) as HostAsyncStream,
                        Box::pin(identity) as HostAsyncStream,
                    ))
                })
            }),
            identity_exchange_timeout: HOST_IDENTITY_EXCHANGE_TIMEOUT,
        }
    }
}

/// How the shovel thread reaches the broker. `Env` is production: the
/// launcher passed `TILO_SVC_COLLAB_FD`. `Fd` is an already-connected typed
/// endpoint, used by tests and headless diagnostics without environment races.
enum SdkConnectSource {
    Env,
    Fd(RawFd),
    Host(HostSessionConnector, RouteSpec),
}

impl SdkConnectSource {
    fn build_client(self) -> Result<runtime_collab::CollabClient, runtime_portal::NotAvailable> {
        match self {
            SdkConnectSource::Host(_, _) => Err(runtime_portal::NotAvailable::ConnectionFailed(
                std::io::Error::other("host session requires worker-side identity admission"),
            )),
            SdkConnectSource::Env => runtime_collab::try_connect(),
            SdkConnectSource::Fd(fd) => {
                use capnp_rpc::rpc_twoparty_capnp;
                let unix = runtime_portal::UnixCapnpClient::from_connected_fd(
                    fd,
                    rpc_twoparty_capnp::Side::Client,
                )?;
                let raw: runtime_collab::CollabServiceClient = unix.bootstrap();
                Ok(runtime_collab::CollabClient::from_raw(raw))
            }
        }
    }
}

/// Build the identity presentation for a launcher-bound collaboration join.
///
/// Build the certified device presentation used for a launcher-bound join.
/// A launcher identity without a device certificate cannot enter the
/// collaboration session.
struct SdkJoinIdentity {
    presentation: Vec<u8>,
    /// Requested broker principal kind. The broker only accepts `agent`
    /// when the named agent is admitted and this certified user may operate
    /// it; ordinary applications always leave this as `person`.
    participant_kind: &'static str,
    /// Retained on the shovel thread so document signing stays off the UI
    /// thread and the workspace broker connection is not consumed at join.
    signer: SdkDocumentSigner,
}

/// Certified identity for a client connecting to an explicit collaboration
/// endpoint rather than through a launcher-inherited identity service.
#[derive(Clone)]
pub struct CertifiedEndpointIdentity {
    presentation: Vec<u8>,
    signer: EndpointDocumentSigner,
}

#[derive(Clone)]
enum EndpointDocumentSigner {
    Device(ed25519_dalek::SigningKey),
    Host(std::sync::Arc<dyn HostDocumentCommitter>),
}

impl CertifiedEndpointIdentity {
    pub fn new(presentation: Vec<u8>, device_key: ed25519_dalek::SigningKey) -> Self {
        Self {
            presentation,
            signer: EndpointDocumentSigner::Device(device_key),
        }
    }

    /// Explicit owner-bound host publication. The host retains signing custody;
    /// this hook must return only after the exact document is durably committed.
    pub fn with_host_documents(
        presentation: Vec<u8>,
        documents: std::sync::Arc<dyn HostDocumentCommitter>,
    ) -> Self {
        Self {
            presentation,
            signer: EndpointDocumentSigner::Host(documents),
        }
    }

    fn document_signer(&self) -> SdkDocumentSigner {
        match &self.signer {
            EndpointDocumentSigner::Host(host) => SdkDocumentSigner::Host(host.clone()),
            EndpointDocumentSigner::Device(key) => {
                SdkDocumentSigner::Harness(Box::new(key.clone()))
            }
        }
    }
}

impl std::fmt::Debug for CertifiedEndpointIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertifiedEndpointIdentity")
            .field(
                "presentation",
                &format_args!("{} bytes", self.presentation.len()),
            )
            .field("device_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

// One signing boundary for inherited Unix services and supplied host services.
// This trait is private: platform bootstrap supplies the concrete broker client,
// never a caller-defined signer that could bypass that client's service policy.
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "certified inherited identities are supplied by Unix launch services"
    )
)]
trait SdkCertifiedIdentity: Send + Sync {
    fn channel_keys(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>;
    fn device_certificate(&self) -> Option<&[u8]>;
    fn signing_public_key(&self) -> [u8; 32];
    fn sign_collaboration_join(
        &self,
        workspace_id: &str,
    ) -> Result<[u8; 64], runtime_portal::NotAvailable>;
    fn sign_collaboration_op(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable>;
}

impl SdkCertifiedIdentity for runtime_identity::BrokerSigningIdentity {
    fn channel_keys(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        runtime_identity::BrokerSigningIdentity::channel_keys(
            self,
            workspace_id,
            app_id,
            record_id,
            known_generation,
        )
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        runtime_identity::BrokerSigningIdentity::device_certificate(self)
    }
    fn signing_public_key(&self) -> [u8; 32] {
        runtime_identity::BrokerSigningIdentity::signing_public_key(self)
    }
    fn sign_collaboration_join(
        &self,
        workspace_id: &str,
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        runtime_identity::BrokerSigningIdentity::sign_collaboration_join(self, workspace_id)
    }
    fn sign_collaboration_op(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        runtime_identity::BrokerSigningIdentity::sign_collaboration_op(
            self,
            workspace_id,
            app_id,
            record_id,
            unsigned_op,
        )
    }
}

#[cfg(unix)]
impl SdkCertifiedIdentity for runtime_identity::SigningIdentity {
    fn channel_keys(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        runtime_identity::SigningIdentity::channel_keys(
            self,
            workspace_id,
            app_id,
            record_id,
            known_generation,
        )
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        runtime_identity::SigningIdentity::device_certificate(self)
    }
    fn signing_public_key(&self) -> [u8; 32] {
        runtime_identity::SigningIdentity::signing_public_key(self)
    }
    fn sign_collaboration_join(
        &self,
        workspace_id: &str,
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        runtime_identity::SigningIdentity::sign_collaboration_join(self, workspace_id)
    }
    fn sign_collaboration_op(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        runtime_identity::SigningIdentity::sign_collaboration_op(
            self,
            workspace_id,
            app_id,
            record_id,
            unsigned_op,
        )
    }
}

#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "certified inherited identities are supplied by Unix launch services"
    )
)]
enum SdkDocumentSigner {
    Host(std::sync::Arc<dyn HostDocumentCommitter>),
    /// Shared so the identity service can be asked off the shovel thread.
    Certified(std::sync::Arc<dyn SdkCertifiedIdentity>),
    /// Explicit-FD harnesses have no launcher identity service. Their
    /// in-process broker accepts this ephemeral signer; a production broker
    /// still rejects the uncertified join before any op reaches policy.
    Harness(Box<ed25519_dalek::SigningKey>),
}

/// Identity state after the shovel thread has entered its `LocalSet`. The host
/// signer may be !Send because it is constructed there and never crosses the
/// thread boundary; inherited Unix and harness identities remain Send.
struct LocalSdkJoinIdentity {
    presentation: Vec<u8>,
    participant_kind: &'static str,
    signer: LocalSdkDocumentSigner,
}

enum LocalSdkDocumentSigner {
    Inherited(SdkDocumentSigner),
    Host(runtime_identity::AsyncBrokerSigningIdentity),
}

impl From<SdkJoinIdentity> for LocalSdkJoinIdentity {
    fn from(identity: SdkJoinIdentity) -> Self {
        Self {
            presentation: identity.presentation,
            participant_kind: identity.participant_kind,
            signer: LocalSdkDocumentSigner::Inherited(identity.signer),
        }
    }
}

fn harness_document_signer() -> SdkDocumentSigner {
    SdkDocumentSigner::Harness(Box::new(ed25519_dalek::SigningKey::generate(
        &mut UnwrapErr(SysRng),
    )))
}

#[cfg(unix)]
fn sdk_join_identity(cluster_id: Uuid) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    sdk_join_identity_from(runtime_identity::load()?, cluster_id)
}

#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "certified inherited identities are supplied by Unix launch services"
    )
)]
fn sdk_join_identity_from<I: SdkCertifiedIdentity + 'static>(
    identity: I,
    cluster_id: Uuid,
) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    let device_certificate = identity.device_certificate().ok_or_else(|| {
        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "workspace identity has no certified device",
        ))
    })?;

    let workspace_id = cluster_id.to_string();
    let proof = identity.sign_collaboration_join(&workspace_id)?;
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id,
        device_certificate: device_certificate.to_vec(),
        proof: proof.to_vec(),
    }
    .encode()
    .map_err(|error| {
        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("encoding certified collaboration join failed: {error}"),
        ))
    })?;
    Ok(SdkJoinIdentity {
        presentation,
        participant_kind: "person",
        signer: SdkDocumentSigner::Certified(std::sync::Arc::new(identity)),
    })
}

#[allow(
    clippy::future_not_send,
    reason = "the host identity transport is constructed and retained only on the shovel LocalSet"
)]
async fn sdk_join_identity_from_host(
    mut identity: runtime_identity::AsyncBrokerSigningIdentity,
    cluster_id: Uuid,
) -> Result<LocalSdkJoinIdentity, runtime_portal::NotAvailable> {
    let device_certificate = identity
        .device_certificate()
        .map(<[u8]>::to_vec)
        .ok_or_else(|| {
            runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "workspace identity has no certified device",
            ))
        })?;
    let workspace_id = cluster_id.to_string();
    let proof = identity.sign_collaboration_join(&workspace_id).await?;
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id,
        device_certificate,
        proof: proof.to_vec(),
    }
    .encode()
    .map_err(|error| {
        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("encoding certified collaboration join failed: {error}"),
        ))
    })?;
    Ok(LocalSdkJoinIdentity {
        presentation,
        participant_kind: "person",
        signer: LocalSdkDocumentSigner::Host(identity),
    })
}

#[cfg(not(unix))]
fn sdk_join_identity(_cluster_id: Uuid) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    Err(runtime_portal::NotAvailable::ServiceFdMissing)
}

/// Primary-route barrier shared by the blocking constructors and
/// [`CollaborationManager::await_primary_route`].
fn await_primary_route_on(
    transport: &mut dyn CollabTransport,
    primary_route: &mut PrimaryRouteState,
) -> Result<(), runtime_portal::NotAvailable> {
    primary_route.accepted = None;
    if transport
        .submit(TransportCommand::ConfirmPrimaryRoute)
        .is_err()
    {
        return Err(runtime_portal::NotAvailable::ConnectionFailed(
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "collaboration setup thread is unavailable or overloaded",
            ),
        ));
    }
    match transport.wait_primary_route_result(SHOVEL_INIT_TIMEOUT) {
        Some(result) => primary_route.accept(result).map_err(|error| {
            let kind = if error == PRIMARY_ROUTE_TIMED_OUT {
                std::io::ErrorKind::TimedOut
            } else {
                std::io::ErrorKind::ConnectionRefused
            };
            runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(kind, error))
        }),
        None if transport.status().is_open() => {
            Err(runtime_portal::NotAvailable::ConnectionFailed(
                std::io::Error::new(std::io::ErrorKind::TimedOut, PRIMARY_ROUTE_TIMED_OUT),
            ))
        }
        None => Err(runtime_portal::NotAvailable::ConnectionFailed(
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "collaboration setup thread ended before reporting route readiness",
            ),
        )),
    }
}

/// A connected broker transport and its acknowledged primary route.
struct ConnectedBroker {
    transport: BrokerTransport,
    primary_route: PrimaryRouteState,
}

#[cfg(all(test, unix))]
pub(crate) struct NativeRefreshProbe {
    replace_history: Box<dyn Fn(Vec<u8>)>,
    committed_read_count: Box<dyn Fn() -> usize>,
}
#[cfg(all(test, unix))]
impl NativeRefreshProbe {
    pub(crate) fn committed_read_count(&self) -> usize {
        (self.committed_read_count)()
    }
    pub(crate) fn replace_history(&self, history: Vec<u8>) {
        (self.replace_history)(history);
    }
}

impl CollaborationManager {
    /// Connect to the launcher-inherited typed collaboration service and
    /// announce an explicit route. Headless participants use this instead of
    /// relying on process-wide `QUICK_APP_ID` / `TILO_WORKSPACE_ID` variables.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the launcher did not
    /// hand down a collab service connection.
    pub fn for_route(
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Env,
            app_id.into(),
            cluster_id,
            record,
            handle,
            None,
            "person",
        )
    }

    /// Explicit typed endpoint for in-process and headless transport
    /// harnesses. Takes ownership of `fd`. It uses an ephemeral signer; a
    /// production broker rejects the uncertified join, so this cannot bypass
    /// workspace admission.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when `fd` does not bootstrap
    /// a working collab service connection.
    pub fn for_route_fd(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            handle,
            None,
            "person",
        )
    }

    /// Connect to an explicit endpoint with a certified device presentation.
    /// This is the headless equivalent of the launcher-provided identity path.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when `fd` cannot bootstrap a
    /// collaboration service connection.
    pub fn for_route_fd_with_identity(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            handle,
            Some(SdkJoinIdentity {
                presentation: identity.presentation.clone(),
                participant_kind: "person",
                signer: identity.document_signer(),
            }),
            "person",
        )
    }

    /// Connect using a one-shot handoff supplied by the platform host.
    ///
    /// This shared SDK seam is for host bootstrap, not app identity selection.
    /// It reuses certified joining and waits for the existing primary-route
    /// acknowledgement. The route given to the factory is the exact Hello
    /// route. Failure consumes the connector and never falls back to Env/Fd,
    /// local keys or a harness signer. Reconnecting needs a fresh host handoff.
    ///
    /// # Errors
    /// Returns the host/identity/transport error or primary-route refusal.
    pub async fn for_route_host(
        connector: HostSessionConnector,
        app_id: String,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        tokio::task::spawn_blocking(move || {
            Self::connect_route_host_blocking(connector, app_id, cluster_id, record, handle)
        })
        .await
        .map_err(|error| {
            runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::other(format!(
                "host collaboration bootstrap worker failed: {error}"
            )))
        })?
        .map(Self::from_connected)
    }

    /// Synchronous form for deterministic unit harnesses. Production callers
    /// only receive the async entry point above, so identity and
    /// primary-route deadlines do not stall the application's owner/frame
    /// thread.
    #[cfg(test)]
    fn for_route_host_blocking(
        connector: HostSessionConnector,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::connect_route_host_blocking(connector, app_id, cluster_id, record, handle)
            .map(Self::from_connected)
    }

    /// Synchronous implementation run inside the background bootstrap task.
    fn connect_route_host_blocking(
        connector: HostSessionConnector,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<ConnectedBroker, runtime_portal::NotAvailable> {
        let app_id = app_id.into();
        let route = RouteSpec {
            app_id: app_id.clone(),
            cluster_id,
            record: record.clone(),
        };
        Self::connect_route_from(
            SdkConnectSource::Host(connector, route),
            app_id,
            cluster_id,
            record,
            handle,
            None,
            "person",
        )
    }

    pub(crate) fn switch_native_record(
        &mut self,
        checkout: &crate::record::RecordCheckout,
    ) -> Result<(), String> {
        let primary = self
            .primary_route
            .accepted
            .as_ref()
            .ok_or("native primary route unavailable")?
            .route
            .clone();
        let record = (!checkout.id.is_package_primary()).then(|| checkout.id.as_str().to_owned());
        if primary.record == record {
            return Ok(());
        }
        if !self.send(CollabRequest::Hello {
            app_id: primary.app_id,
            cluster_id: primary.cluster_id,
            record,
            record_kind: Some(checkout.kind.as_str().to_owned()),
            handle: presence_handle_env(),
        }) {
            return Err("native route queue unavailable".into());
        }
        self.await_primary_route()
            .map_err(|error| error.to_string())
    }

    #[cfg(feature = "native")]
    pub(crate) fn for_native_launch(kind: &crate::record::RecordKind) -> Result<Self, String> {
        if crate::host::environment()
            .identity
            .collab_service_configured
        {
            media::audio_output::require_governed();
        }
        if runtime_portal::service_fd("collab").is_none() {
            if crate::host::environment()
                .identity
                .collab_service_configured
            {
                return Err("configured native collaboration service is unavailable".into());
            }
            return Ok(Self::disabled());
        }
        let (app, room) = auto_hello_env();
        super::channel_settings::install(&app);
        Self::connect_native_launch(SdkConnectSource::Env, app, room, kind)
    }

    #[cfg(feature = "native")]
    fn connect_native_launch(
        source: SdkConnectSource,
        app: String,
        room: Uuid,
        kind: &crate::record::RecordKind,
    ) -> Result<Self, String> {
        Self::connect_route_kind(
            source,
            RouteSpec {
                app_id: app,
                cluster_id: room,
                record: None,
            },
            Some(kind.as_str().to_owned()),
            presence_handle_env(),
            None,
            "person",
        )
        .map(Self::from_connected)
        .map_err(|error| error.to_string())
    }

    #[cfg(all(test, unix))]
    pub(crate) fn native_history_invocation_fixture_for_test(
        history: Vec<u8>,
    ) -> NativeHistoryInvocationFixture {
        tests::native_history_invocation_fixture(history)
    }

    /// Wire/SDK ordering fixture only; not a native authority proof.
    #[cfg(all(test, unix))]
    pub(crate) fn native_initial_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_fixture(app, room, history)
    }

    /// Socket-backed ordering fixture on an actual acknowledged named route.
    /// This fixture does not establish native caller authority.
    #[cfg(all(test, unix))]
    pub(crate) fn native_initial_named_fixture_for_test(
        route: RouteSpec,
        history: Vec<u8>,
    ) -> Self {
        let (mut manager, _) =
            tests::native_initial_fixture(&route.app_id, route.cluster_id, history);
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id,
            cluster_id: route.cluster_id,
            record: route.record,
            record_kind: None,
            handle: None,
        }));
        manager
            .await_primary_route()
            .expect("actual named route acknowledgement");
        manager
    }

    /// A connected route whose document reads are not Required, as a
    /// standalone runtime admits it.
    #[cfg(all(test, unix))]
    pub(crate) fn connected_fixture_for_test(app: &str, room: Uuid) -> Self {
        tests::connected_manager_for(app, room)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn native_initial_fixture_gated_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_fixture_gated(app, room, history, Some(gate))
    }

    /// Socket-backed committed-read fixture; the probe observes real recheck requests.
    #[cfg(all(test, unix))]
    pub(crate) fn native_initial_parked_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_parked_fixture(app, room, history, gate)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn native_refresh_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
    ) -> (Self, NativeRefreshProbe) {
        tests::native_refresh_fixture(app, room, history)
    }

    fn for_route_from(
        source: SdkConnectSource,
        app_id: String,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
        endpoint_identity: Option<SdkJoinIdentity>,
        participant_kind: &'static str,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::connect_route_from(
            source,
            app_id,
            cluster_id,
            record,
            handle,
            endpoint_identity,
            participant_kind,
        )
        .map(Self::from_connected)
    }

    fn from_connected(connected: ConnectedBroker) -> Self {
        let mut manager = Self::with_transport(Box::new(connected.transport));
        manager.primary_route = connected.primary_route;
        // The bootstrap already acknowledged Ready, but the owner has not
        // observed it. Its first pump must still announce the initial action
        // surface and request the document snapshot on the admitted route.
        manager.reported_state = None;
        manager
    }

    /// Start the broker transport, announce the primary route and wait for
    /// the broker's acknowledgement. Returns `Send` parts so a background
    /// bootstrap task can hand them to the owner thread.
    fn connect_route_from(
        source: SdkConnectSource,
        app_id: String,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
        endpoint_identity: Option<SdkJoinIdentity>,
        participant_kind: &'static str,
    ) -> Result<ConnectedBroker, runtime_portal::NotAvailable> {
        Self::connect_route_kind(
            source,
            RouteSpec {
                app_id,
                cluster_id,
                record,
            },
            None,
            handle,
            endpoint_identity,
            participant_kind,
        )
    }

    /// Initial native creation needs the application's actual root resource
    /// kind before the broker can open the default document.
    fn connect_route_kind(
        source: SdkConnectSource,
        route: RouteSpec,
        record_kind: Option<String>,
        handle: Option<String>,
        endpoint_identity: Option<SdkJoinIdentity>,
        participant_kind: &'static str,
    ) -> Result<ConnectedBroker, runtime_portal::NotAvailable> {
        let RouteSpec {
            app_id,
            cluster_id,
            record,
        } = route;
        let mut identity = match endpoint_identity {
            Some(identity) => Some(identity),
            None if matches!(&source, SdkConnectSource::Host(_, _)) => None,
            None if matches!(&source, SdkConnectSource::Env) => {
                Some(sdk_join_identity(cluster_id)?)
            }
            None => Some(SdkJoinIdentity {
                presentation: Vec::new(),
                participant_kind: "person",
                signer: harness_document_signer(),
            }),
        };
        if let Some(identity) = &mut identity {
            identity.participant_kind = participant_kind;
        }
        let mut transport = Self::spawn_sdk(source, identity)?;
        let mut primary_route = PrimaryRouteState::default();
        primary_route.announce(RouteSpec {
            app_id: app_id.clone(),
            cluster_id,
            record: record.clone(),
        });
        let _ = transport.submit(TransportCommand::Primary(CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            record_kind,
            handle,
        }));
        await_primary_route_on(&mut transport, &mut primary_route)?;
        Ok(ConnectedBroker {
            transport,
            primary_route,
        })
    }

    /// Wait until the shovel has processed the initial `Hello`, admitted its
    /// primary route (including the initial decision catalogue for an agent).
    /// Without this barrier a caller can publish transient presence immediately after
    /// construction, before the broker knows where to deliver it.
    fn await_primary_route(&mut self) -> Result<(), runtime_portal::NotAvailable> {
        self.primary_route.accepted = None;
        let super::Backend::Transport(transport) = &mut self.backend else {
            return Err(runtime_portal::NotAvailable::ServiceFdMissing);
        };
        await_primary_route_on(transport.as_mut(), &mut self.primary_route)
    }

    /// Connect a headless participant as a first-class workspace agent.
    /// Its friendly id is both the visible handle and the principal whose
    /// scoped capabilities the broker evaluates.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the collaboration
    /// service or certified join identity is unavailable.
    pub fn for_agent_route(
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Env,
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            None,
            "agent",
        )
    }

    /// Explicit-socket counterpart to [`Self::for_agent_route`]. Production
    /// still requires a certified endpoint identity; this method merely
    /// preserves the agent principal kind for diagnostic transports.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the socket cannot be
    /// joined or the route identity cannot be established.
    pub fn for_agent_route_fd(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            None,
            "agent",
        )
    }

    /// Certified explicit-socket agent connection used by out-of-process
    /// runtimes that do not inherit the launcher's service descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the socket or certified
    /// route join fails.
    pub fn for_agent_route_fd_with_identity(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            Some(SdkJoinIdentity {
                presentation: identity.presentation.clone(),
                participant_kind: "agent",
                signer: identity.document_signer(),
            }),
            "agent",
        )
    }

    pub(super) fn new_scoped(record: Option<String>, record_kind: Option<String>) -> Self {
        // Without an inherited service there is nothing to reach; say so
        // without starting a worker.
        if runtime_portal::service_fd("collab").is_none() {
            return Self::disabled();
        }
        let (app_id, cluster_id) = auto_hello_env();
        super::channel_settings::install(&app_id);
        Self::connect_in_background(
            SdkConnectSource::Env,
            SdkIdentitySource::Inherited(cluster_id),
            RouteSpec {
                app_id,
                cluster_id,
                record,
            },
            record_kind,
            presence_handle_env(),
        )
    }

    /// A manager on `route`, reached through the host's `connector` as an
    /// app it launched, connecting in the background: the manager reports
    /// [`super::CollaborationState::Connecting`] until the broker
    /// acknowledges the route. For a host showing an app inside its own
    /// process.
    pub(crate) fn for_route_host_in_background(
        connector: HostSessionConnector,
        route: RouteSpec,
        record_kind: Option<String>,
    ) -> Self {
        Self::connect_in_background(
            SdkConnectSource::Host(connector, route.clone()),
            SdkIdentitySource::Ready(None),
            route,
            record_kind,
            None,
        )
    }

    /// Start the broker transport and announce `route` without waiting for
    /// either. The worker loads the join identity, connects and joins; the
    /// manager reports [`super::CollaborationState::Connecting`] until
    /// [`Self::poll`] sees the primary route acknowledged, and the owner is
    /// woken at each step.
    fn connect_in_background(
        source: SdkConnectSource,
        identity: SdkIdentitySource,
        route: RouteSpec,
        record_kind: Option<String>,
        handle: Option<String>,
    ) -> Self {
        let sdk = match Self::start_sdk(source, identity) {
            Ok((sdk, _init)) => sdk,
            Err(error) => return Self::offline(error.to_string()),
        };
        let mut manager = Self::with_transport(Box::new(sdk));
        let RouteSpec {
            app_id,
            cluster_id,
            record,
        } = route;
        manager.send(CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            record_kind,
            handle,
        });
        manager.confirm_primary_route_in_background();
        manager
    }

    /// Real socket-backed SDK transport for App lifecycle integration tests.
    #[cfg(all(test, unix))]
    pub(crate) fn connected_for_lifecycle_test() -> Self {
        tests::connected_manager()
    }

    /// Retire the actual socket-backed capture generation, as a rejoin does.
    #[cfg(all(test, unix))]
    pub(crate) fn retire_document_capture_for_test(&self, route: &RouteSpec) {
        let Backend::Transport(transport) = &self.backend else {
            panic!("a broker transport");
        };
        let transport = transport
            .as_any()
            .and_then(|any| any.downcast_ref::<BrokerTransport>())
            .expect("a broker transport");
        drop(
            transport
                .state
                .document_reads
                .close_for_test(&route.channel_id()),
        );
    }

    /// Block until the broker worker has finished every command submitted so
    /// far. A route join enqueues the route's initial action catalogue before
    /// its command completes, so the next [`Self::poll`] then sees everything
    /// those commands produced. Panics at the hang bound.
    #[cfg(all(test, unix))]
    pub(crate) fn wait_for_commands_for_test(&self) {
        let Backend::Transport(transport) = &self.backend else {
            panic!("a broker transport");
        };
        let state = &transport
            .as_any()
            .and_then(|any| any.downcast_ref::<BrokerTransport>())
            .expect("a broker transport")
            .state;
        let deadline = tilo_test_env::hang_deadline();
        while state.command_bytes.0.load(Ordering::Acquire) != 0
            || state.document_command_bytes.0.load(Ordering::Acquire) != 0
        {
            assert!(
                crate::time::now() < deadline,
                "broker worker still had commands in flight after {:?}",
                tilo_test_env::hang_bound()
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Real socket-backed SDK transport plus a probe returning every signed
    /// envelope the broker accepted, in order.
    #[cfg(all(test, unix))]
    pub(crate) fn capturing_for_test() -> (Self, Box<dyn Fn() -> Vec<SignedOp>>) {
        tests::capturing_manager()
    }

    /// Like [`Self::connected_for_lifecycle_test`], but still connecting:
    /// returns the manager with the wake scope it reports to and a channel
    /// that receives each of its wakes.
    #[cfg(all(test, unix))]
    pub(crate) fn connecting_for_lifecycle_test()
    -> (Self, crate::wake::WakeScope, mpsc::Receiver<()>) {
        tests::connecting_manager()
    }

    /// Test-only: build a manager talking to an SDK broker on the given
    /// already-connected client fd, bypassing env-var lookup so two
    /// tests in one process don't race on `TILO_SVC_COLLAB_FD`.
    #[cfg(all(test, unix))]
    fn from_sdk_fd_for_test(fd: RawFd, app_id: &str, cluster_id: Uuid) -> Self {
        Self::from_sdk_fd_for_test_with_identity(fd, app_id, cluster_id, Vec::new())
    }

    /// Like [`Self::from_sdk_fd_for_test`] but stamps the `join_channel`
    /// call with a caller-supplied identity blob.
    #[cfg(all(test, unix))]
    fn from_sdk_fd_for_test_with_identity(
        fd: RawFd,
        app_id: &str,
        cluster_id: Uuid,
        identity_bytes: Vec<u8>,
    ) -> Self {
        let sdk = Self::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: identity_bytes,
                participant_kind: "person",
                signer: harness_document_signer(),
            }),
        )
        .expect("spawn_sdk from Fd source must succeed in tests");
        let mut manager = Self::with_transport(Box::new(sdk));
        manager.send(CollabRequest::Hello {
            app_id: app_id.to_owned(),
            cluster_id,
            record: None,
            record_kind: None,
            handle: presence_handle_env(),
        });
        manager
            .await_primary_route()
            .expect("test collaboration primary route becomes ready");
        manager
    }

    /// Stand up the shovel thread + its tokio `current_thread` runtime and
    /// return at once. The shovel loads the join identity and runs
    /// `try_connect` **inside** its `LocalSet` because capnp-rpc futures are
    /// `!Send` and can't cross a thread boundary.
    ///
    /// The transport reports [`TransportStatus::Connecting`] until the
    /// shovel is serving commands (or [`TransportStatus::Closed`] if setup
    /// failed); commands submitted meanwhile queue for it. The returned
    /// receiver carries the same outcome once, for callers that choose to
    /// wait for it.
    fn start_sdk(
        source: SdkConnectSource,
        identity: SdkIdentitySource,
    ) -> Result<(BrokerTransport, mpsc::Receiver<InitStatus>), runtime_portal::NotAvailable> {
        let (outbound_tx, outbound_rx) = tokio_mpsc::channel::<QueuedSdkCommand>(SDK_COMMAND_QUEUE);
        let (inbound_tx, inbound_rx) = tokio_mpsc::channel::<QueuedCollabEvent>(SDK_EVENT_QUEUE);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(inbound_tx, state.clone());
        let (media_tx, media_rx) = native_media::channel(inbound.clone());
        let (action_directory_tx, action_directory_rx) =
            mpsc::sync_channel::<ActionDirectoryRequest>(ACTION_DIRECTORY_REQUEST_QUEUE);
        let (init_tx, init_rx) = mpsc::sync_channel::<InitStatus>(1);

        // A thread we can't start is a service we can't reach — map to
        // NotAvailable rather than panicking; an app must open even without collab.
        let handle = match thread::Builder::new()
            .name("ui-collab-shovel".into())
            .spawn(move || run_shovel(source, identity, outbound_rx, media_rx, inbound, init_tx))
        {
            Ok(handle) => handle,
            Err(e) => return Err(runtime_portal::NotAvailable::ConnectionFailed(e)),
        };

        let transport = BrokerTransport {
            outbound_tx,
            media: Some(media_tx),
            inbound_rx,
            document_turns: Vec::new(),
            state,
            action_directory_rx,
            action_directory_tx,
            pending_primary: None,
            primary_deadline: None,
            worker_stopped: false,
            wifi_bridge_registered: false,
            _thread: Some(handle),
        };
        Ok((transport, init_rx))
    }

    /// [`Self::start_sdk`], then wait (bounded) for the shovel to connect.
    /// Only for the explicit, fallible headless constructors, whose callers
    /// asked for a connected manager or an error.
    fn spawn_sdk(
        source: SdkConnectSource,
        identity: Option<SdkJoinIdentity>,
    ) -> Result<BrokerTransport, runtime_portal::NotAvailable> {
        let (mut transport, init_rx) = Self::start_sdk(source, SdkIdentitySource::Ready(identity))?;

        // A wedged connect costs a timeout, never a caller that waits forever.
        let status = match init_rx.recv_timeout(SHOVEL_INIT_TIMEOUT) {
            Ok(status) => status,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    "collab: the connect path did not report within {SHOVEL_INIT_TIMEOUT:?}"
                );
                return Err(runtime_portal::NotAvailable::ConnectionFailed(
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "collab connect did not report in time",
                    ),
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!("collab: the connect path ended without reporting");
                return Err(runtime_portal::NotAvailable::ConnectionFailed(
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "collab connect ended without reporting",
                    ),
                ));
            }
        };

        match status {
            InitStatus::Ready => {
                transport.register_wifi_bridge();
                Ok(transport)
            }
            InitStatus::Failed(e) => {
                // Let the (empty-body) thread exit cleanly.
                if let Some(handle) = transport._thread.take() {
                    let _ = handle.join();
                }
                Err(e)
            }
        }
    }
}

/// Where the shovel gets the identity it joins with.
enum SdkIdentitySource {
    /// Supplied by the caller; `None` for host sessions, which obtain it
    /// from the host during setup.
    Ready(Option<SdkJoinIdentity>),
    /// The launcher-inherited workspace identity for this cluster. Loading
    /// it and signing the join are IPC round trips, so the shovel does it,
    /// never the constructing thread.
    Inherited(Uuid),
}

impl SdkIdentitySource {
    fn resolve(self) -> Result<Option<SdkJoinIdentity>, runtime_portal::NotAvailable> {
        match self {
            Self::Ready(identity) => Ok(identity),
            Self::Inherited(cluster_id) => sdk_join_identity(cluster_id).map(Some),
        }
    }
}

/// One-shot init result from the shovel thread's startup, for callers of
/// `spawn_sdk` that wait for a connected transport or an error.
enum InitStatus {
    Ready,
    Failed(runtime_portal::NotAvailable),
}

/// Publish the shovel's setup outcome: to the owner through the bridge's
/// connect phase (waking it), and to a waiting constructor, if any.
fn report_init(state: &BridgeState, init_tx: &mpsc::SyncSender<InitStatus>, status: InitStatus) {
    state.set_connect_phase(match &status {
        InitStatus::Ready => ConnectPhase::Connected,
        InitStatus::Failed(error) => ConnectPhase::Failed(error.to_string()),
    });
    let _ = init_tx.send(status);
}

/// The shovel thread's body: owns a `current_thread` runtime + `LocalSet`
/// (capnp-rpc futures are `!Send`), stands up the SDK client, subscribes
/// its three streams, and forwards outbound requests. Exits when
/// `outbound_rx.recv()` returns `None` (manager dropped).
fn run_shovel(
    source: SdkConnectSource,
    identity: SdkIdentitySource,
    outbound_rx: tokio_mpsc::Receiver<QueuedSdkCommand>,
    media_rx: native_media::Receiver,
    inbound_tx: InboundBridge,
    init_tx: mpsc::SyncSender<InitStatus>,
) {
    let state = inbound_tx.state.clone();
    // Loading an inherited identity is blocking IPC; this thread exists so
    // that the owner never waits for it.
    let identity = match identity.resolve() {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(?error, "collab: workspace join identity unavailable");
            report_init(&state, &init_tx, InitStatus::Failed(error));
            return;
        }
    };
    // Building `current_thread` is fallible (can OOM); surface as
    // Disabled — no service means the app runs local-only.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            report_init(
                &state,
                &init_tx,
                InitStatus::Failed(runtime_portal::NotAvailable::ConnectionFailed(e)),
            );
            return;
        }
    };

    let local = LocalSet::new();
    let init_state = state.clone();
    local.block_on(&rt, async move {
        let state = init_state;
        let session = match source {
            SdkConnectSource::Host(connector, route) => {
                let cluster_id = route.cluster_id;
                let identity_timeout = connector.identity_exchange_timeout;
                match tokio::time::timeout(SHOVEL_INIT_TIMEOUT, (connector.connect)(route)).await {
                    Ok(Ok((collaboration_stream, identity_stream))) => {
                        let identity =
                            runtime_identity::AsyncBrokerSigningIdentity::from_connected_io(
                                identity_stream,
                                identity_timeout,
                            )
                            .await;
                        match identity {
                            Ok(identity) => {
                                match sdk_join_identity_from_host(identity, cluster_id).await {
                                    Ok(identity) => {
                                        let rpc = runtime_portal::CapnpClient::from_connected_io(
                                            collaboration_stream,
                                            capnp_rpc::rpc_twoparty_capnp::Side::Client,
                                        );
                                        let raw: runtime_collab::CollabServiceClient =
                                            rpc.bootstrap();
                                        Ok((runtime_collab::CollabClient::from_raw(raw), identity))
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(runtime_portal::NotAvailable::ConnectionFailed(
                        std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "host session handoff timed out",
                        ),
                    )),
                }
            }
            source => source.build_client().and_then(|client| {
                identity
                    .map(|identity| (client, identity.into()))
                    .ok_or_else(|| {
                        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "missing collaboration signing identity",
                        ))
                    })
            }),
        };
        let (client, identity) = match session {
            Ok(session) => session,
            Err(e) => {
                report_init(&state, &init_tx, InitStatus::Failed(e));
                return;
            }
        };
        // The transport is ready. The manager's primary Hello and its
        // `PrimaryReady` barrier are the first commands the shovel serves.
        report_init(&state, &init_tx, InitStatus::Ready);

        drive_shovel(client, identity, outbound_rx, media_rx, inbound_tx).await;
    });
    // However the shovel ended, the owner must look again: an unanswered
    // primary-route barrier now reads as a stopped worker.
    state.notify_events();
}

/// How often an endpoint asks the identity service whether its routes'
/// channel keys changed. Also bounds how long an edit waits after a new
/// device is admitted.
const CHANNEL_KEY_REFRESH: Duration = Duration::from_secs(1);

/// SDK-side event loop: dispatches outbound requests to SDK calls, and
/// spawns one subscription task per stream on the first `join_channel`.
// Not `Send`: `client` wraps a capnp-rpc handle (`Rc`-based, `!Send`), and
// this only ever runs inside `run_shovel`'s `LocalSet` on its own thread.
#[allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn drive_shovel(
    client: runtime_collab::CollabClient,
    identity: LocalSdkJoinIdentity,
    mut outbound_rx: tokio_mpsc::Receiver<QueuedSdkCommand>,
    media_rx: native_media::Receiver,
    inbound_tx: InboundBridge,
) {
    let LocalSdkJoinIdentity {
        presentation,
        participant_kind,
        mut signer,
    } = identity;
    let media_routes = native_media::Routes::default();
    let _media_worker = native_media::start(media_rx, media_routes.clone());
    // Every route of a certified endpoint is an encrypted channel whose keys
    // come from the identity service. A harness has no such service, and an
    // inherited host signer commits cleartext through its host owner, so
    // both stay plaintext.
    let asks_channel_keys = !matches!(
        signer,
        LocalSdkDocumentSigner::Inherited(
            SdkDocumentSigner::Harness(_) | SdkDocumentSigner::Host(_)
        )
    );
    let mut key_refresh = tokio::time::interval(CHANNEL_KEY_REFRESH);
    key_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // A certified identity service is asked on a blocking thread. The
    // periodic round runs alongside this loop, which keeps sending while the
    // routes keep receiving; at most one round is in flight.
    let mut key_fetch: Option<KeyFetch> = None;
    let mut routes = std::collections::HashMap::<String, SdkRoute>::new();
    let mut primary: Option<String> = None;
    let mut participant_handle: Option<String> = None;
    let mut primary_result = Err("primary collaboration route was not announced".to_string());
    // The app's settings over the primary route, when the shell keeps them.
    let mut settings_link: Option<SettingsLink> = None;
    // The app's live activity over the primary route, when the shell
    // presents live activities.
    let mut live_activity_link: Option<LiveActivityLink> = None;
    // The first document mutation since the last publication barrier that
    // did not reach the broker, reported to (and cleared by) that barrier.
    let mut unpublished: Option<String> = None;

    loop {
        let queued = tokio::select! {
            queued = outbound_rx.recv() => queued,
            _ = key_refresh.tick(), if asks_channel_keys && key_fetch.is_none() => {
                key_fetch = refresh_keys(&routes, &mut signer, &inbound_tx).await;
                continue;
            }
            fetched = async {
                match key_fetch.as_mut() {
                    Some(fetch) => fetch.await,
                    None => std::future::pending().await,
                }
            }, if key_fetch.is_some() =>
            {
                key_fetch = None;
                apply_fetched_keys(&routes, fetched, &mut signer, &inbound_tx).await;
                continue;
            }
        };
        let Some(QueuedSdkCommand {
            command,
            _reservation,
        }) = queued
        else {
            break;
        };
        if !inbound_tx.state.is_healthy() {
            break;
        }
        let routes_before = routes.len();
        let document = command.document_len().is_some();
        let mut published = true;
        match command {
            SdkCommand::Hello(PrimaryHello {
                route: spec,
                record_kind,
                handle,
                deadline,
            }) => {
                participant_handle = handle;
                let key = spec.channel_id();
                primary_result = match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline.start()),
                    ensure_sdk_route(
                        &SdkJoinContext {
                            asks_channel_keys,
                            client: &client,
                            presentation: &presentation,
                            participant_handle: participant_handle.as_deref(),
                            participant_kind,
                            inbound_tx: &inbound_tx,
                            media_routes: &media_routes,
                        },
                        &mut routes,
                        &spec,
                        record_kind.as_deref().unwrap_or(""),
                    ),
                )
                .await
                {
                    Ok(result) => result.map(|()| spec),
                    Err(_) => Err(PRIMARY_ROUTE_TIMED_OUT.to_string()),
                };
                primary = primary_result.is_ok().then_some(key);
                if let Some(route) = primary.as_ref().and_then(|key| routes.get(key)) {
                    settings_link = link_settings(&route.channel, &inbound_tx).await;
                    if let Some(link) = settings_link.as_ref() {
                        link.flush(&inbound_tx).await;
                    }
                    live_activity_link = link_live_activity(&route.channel, &inbound_tx).await;
                    if let Some(link) = live_activity_link.as_ref() {
                        link.flush(true).await;
                    }
                }
            }
            SdkCommand::Primary(request) => {
                if let Some(route) = primary.as_ref().and_then(|key| routes.get_mut(key)) {
                    published =
                        dispatch_sdk_request(route, request, &mut signer, &inbound_tx).await;
                    if !inbound_tx.state.is_healthy() {
                        break;
                    }
                } else if matches!(
                    request,
                    CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
                ) {
                    // Document custody fails closed without a joined primary.
                    inbound_tx
                        .state
                        .fail_closed("document primary route unavailable");
                    break;
                } else {
                    tracing::warn!("collab: request sent before the primary route joined");
                    published = false;
                }
            }
            SdkCommand::Join(spec, record_kind) => {
                let _ = ensure_sdk_route(
                    &SdkJoinContext {
                        asks_channel_keys,
                        client: &client,
                        presentation: &presentation,
                        participant_handle: participant_handle.as_deref(),
                        participant_kind,
                        inbound_tx: &inbound_tx,
                        media_routes: &media_routes,
                    },
                    &mut routes,
                    &spec,
                    record_kind.as_deref().unwrap_or(""),
                )
                .await;
            }
            SdkCommand::ToRoute(spec, request) => {
                let key = spec.channel_id();
                let joined = ensure_sdk_route(
                    &SdkJoinContext {
                        asks_channel_keys,
                        client: &client,
                        presentation: &presentation,
                        participant_handle: participant_handle.as_deref(),
                        participant_kind,
                        inbound_tx: &inbound_tx,
                        media_routes: &media_routes,
                    },
                    &mut routes,
                    &spec,
                    "",
                )
                .await;
                if joined.is_ok()
                    && let Some(route) = routes.get_mut(&key)
                {
                    published =
                        dispatch_sdk_request(route, request, &mut signer, &inbound_tx).await;
                    if !inbound_tx.state.is_healthy() {
                        break;
                    }
                } else {
                    published = false;
                    inbound_tx.state.fail_closed("document route join");
                }
            }
            SdkCommand::Initialize {
                route,
                request,
                reply,
            } => {
                let result = if let Some(current) = routes.get(&route.channel_id()) {
                    match current.channel.committed_document(&request).await {
                        Ok(runtime_collab::committed_document::Reply::Initialized { revision }) => {
                            Ok(revision)
                        }
                        Ok(_) => Err("application initialization reply invalid".into()),
                        Err(error) => Err(error.to_string()),
                    }
                } else {
                    Err("application initialization route unavailable".into())
                };
                let _ = reply.send(result);
            }
            SdkCommand::PrimaryReady(reply) => {
                let _ = reply.send(primary_result.clone());
                // A manager still connecting settles on this answer.
                inbound_tx.state.notify_events();
            }
            SdkCommand::PublicationBarrier(reply) => {
                // A superseded receipt may have no receiver. Its prefix still ran.
                // A latched, specific publication failure (a refused signature
                // or commit) outranks the generic "did not reach" marker.
                let unpublished = unpublished.take();
                let _ = reply.send(inbound_tx.state.publication_result().and_then(|()| {
                    unpublished.map_or(Ok(()), |reason| {
                        Err(DocumentPublicationError::Failed(reason))
                    })
                }));
            }
            SdkCommand::InstallEncryption { spec, group, reply } => {
                let result = install_sdk_encryption(
                    SdkJoinContext {
                        asks_channel_keys,
                        client: &client,
                        presentation: &presentation,
                        participant_handle: participant_handle.as_deref(),
                        participant_kind,
                        inbound_tx: &inbound_tx,
                        media_routes: &media_routes,
                    },
                    &mut routes,
                    spec,
                    *group,
                )
                .await;
                let _ = reply.send(result);
            }
            SdkCommand::PublishActionDirectory { app_id, requests } => {
                let Some(key) = primary.as_ref() else {
                    tracing::warn!("collab: directory published before the primary route joined");
                    continue;
                };
                let Some(route) = routes.get_mut(key) else {
                    tracing::warn!(
                        "collab: primary route disappeared before directory publication"
                    );
                    continue;
                };
                let directory = capnp_rpc::new_client(ActionDirectoryServer {
                    requests,
                    owner: inbound_tx.state.owner_wake.clone(),
                });
                if let Err(error) = route
                    .channel
                    .publish_action_directory(&app_id, directory)
                    .await
                {
                    inbound_tx
                        .state
                        .directory_rpc_failures
                        .fetch_add(1, Ordering::AcqRel);
                    tracing::warn!(%error, "collab: publishing pull resource/action directory failed");
                    // Keep the owner enqueue latch armed until backoff expires.
                    // The local timer leaves the broker command pump available;
                    // a refusal cannot create a self-waking publication hot-loop.
                    let state = inbound_tx.state.clone();
                    tokio::task::spawn_local(async move {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        if state.is_healthy() {
                            state.action_directory_failed.store(true, Ordering::Release);
                            state.notify_events();
                        }
                    });
                } else {
                    inbound_tx
                        .state
                        .directory_rpc_successes
                        .fetch_add(1, Ordering::AcqRel);
                    if inbound_tx
                        .state
                        .directory_observation_enabled
                        .load(Ordering::Acquire)
                    {
                        inbound_tx.state.notify_events();
                    }
                }
            }
            SdkCommand::WifiContinuity {
                app_id,
                command,
                reply,
            } => {
                let result = if let Some(key) = primary.as_ref()
                    && let Some(route) = routes.get_mut(key)
                {
                    route
                        .channel
                        .wifi_continuity_command(&app_id, &command)
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    Err("primary collaboration route is unavailable".into())
                };
                let _ = reply.send(result);
            }
            SdkCommand::Speakers {
                app_id,
                command,
                reply,
            } => {
                let result = if let Some(key) = primary.as_ref()
                    && let Some(route) = routes.get_mut(key)
                {
                    route
                        .channel
                        .speaker_command(&app_id, &command)
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    Err("primary collaboration route is unavailable".into())
                };
                let _ = reply.send(result);
            }
            SdkCommand::DisplayControl {
                command,
                origin,
                reply,
            } => {
                let result = if let Some(link) = settings_link.as_ref() {
                    tokio::time::timeout(
                        Duration::from_secs(3),
                        link.handle.display_control(&command, origin),
                    )
                    .await
                    .map_err(|_| "The display service timed out.".to_string())
                    .and_then(|result| result.map_err(|error| error.to_string()))
                } else {
                    Err("The admitted settings channel is unavailable.".into())
                };
                let _ = reply.send(result);
            }
            SdkCommand::FlushSettings => {
                if let Some(link) = settings_link.as_ref() {
                    link.flush(&inbound_tx).await;
                }
            }
            SdkCommand::FlushLiveActivity => {
                if let Some(link) = live_activity_link.as_ref() {
                    link.flush(false).await;
                }
            }
        }
        if document && !published && unpublished.is_none() {
            unpublished = Some("a document mutation did not reach the broker".into());
        }
        if asks_channel_keys && routes.len() > routes_before {
            // A route joined just now learns its keys (or that it waits)
            // before its first edit rather than at the next tick. Only
            // sending waits for that; the routes keep receiving.
            if let Some(fetch) = key_fetch.take() {
                apply_fetched_keys(&routes, fetch.await, &mut signer, &inbound_tx).await;
            }
            if let Some(fetch) = refresh_keys(&routes, &mut signer, &inbound_tx).await {
                apply_fetched_keys(&routes, fetch.await, &mut signer, &inbound_tx).await;
            }
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
async fn install_sdk_encryption(
    context: SdkJoinContext<'_>,
    routes: &mut std::collections::HashMap<String, SdkRoute>,
    spec: RouteSpec,
    group: channel_crypto::ChannelGroup,
) -> Result<(), String> {
    let key = spec.channel_id();
    ensure_sdk_route(&context, routes, &spec, "").await?;
    let route = routes
        .get_mut(&key)
        .ok_or_else(|| "joined MLS route disappeared".to_string())?;
    route
        .encryption
        .lock()
        .map_err(|_| "MLS endpoint state is unavailable after a poisoned lock".to_string())?
        .install_group(group);
    Ok(())
}

#[derive(Clone, Copy)]
struct SdkJoinContext<'a> {
    asks_channel_keys: bool,
    client: &'a runtime_collab::CollabClient,
    presentation: &'a [u8],
    participant_handle: Option<&'a str>,
    participant_kind: &'a str,
    inbound_tx: &'a InboundBridge,
    media_routes: &'a native_media::Routes,
}

struct SdkRoute {
    spec: RouteSpec,
    channel: runtime_collab::ChannelHandle,
    presence: Option<runtime_collab::PresenceHandle>,
    workspace_controls: std::collections::HashMap<u64, runtime_collab::WorkspaceControlHandle>,
    committed: bool,
    encryption: SharedRouteKeys,
}

// Not `Send`: `client`/`channel` wrap capnp-rpc handles, which are
// `Rc`-based and inherently `!Send` — this runs only on the shovel
// thread's `LocalSet` (see `run_shovel`), never crossing a thread
// boundary, so the non-`Send` future is by design, not an oversight.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn ensure_sdk_route(
    context: &SdkJoinContext<'_>,
    routes: &mut std::collections::HashMap<String, SdkRoute>,
    spec: &RouteSpec,
    record_kind: &str,
) -> Result<(), String> {
    let SdkJoinContext {
        asks_channel_keys,
        client,
        presentation: identity_bytes,
        participant_handle,
        participant_kind,
        inbound_tx,
        media_routes,
    } = *context;
    let channel_id = spec.channel_id();
    if routes.contains_key(&channel_id) {
        return Ok(());
    }
    let channel = match client
        .join_channel_as_kind(
            &channel_id,
            identity_bytes,
            participant_handle.unwrap_or(""),
            participant_kind,
            record_kind,
        )
        .await
    {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(
                "collab: joining channel {channel_id} failed ({error}); route unavailable"
            );
            return Err(format!("{}: {error}", super::ROUTE_JOIN_FAILED));
        }
    };

    // Only the explicit agent constructors consume the eager workspace
    // decision catalogue. Ordinary App owners discard that event and expose
    // their own actions through publication and the bounded pull directory;
    // acquiring every other app's inventory can prevent an unrelated app from
    // starting. This selects a consumer, not authority: broker admission and
    // per-action permissions are unchanged.
    if participant_kind == "agent" {
        // Queue the first decision baseline before document traffic so an
        // agent cannot start deciding against stale local fallback data.
        spawn_action_catalogue_stream(&channel, inbound_tx).await?;
    }
    let committed = committed_document::install(
        &channel,
        spec,
        inbound_tx,
        committed_document::NativeReadJoin {
            client: client.clone(),
            presentation: identity_bytes.to_vec(),
            participant_handle: participant_handle.unwrap_or("").into(),
            participant_kind: participant_kind.into(),
            record_kind: record_kind.into(),
        },
    )
    .await?;
    if committed {
        // This reply contains only the original host-retained launch instance;
        // a record label or app environment never becomes private authority.
        let instance = match channel
            .committed_document(&runtime_collab::committed_document::Request::PrivateWindowBinding)
            .await
        {
            Ok(runtime_collab::committed_document::Reply::PrivateWindowBinding { instance }) => {
                instance
            }
            _ => None,
        };
        inbound_tx
            .state
            .document_reads
            .bind_private_window(&channel_id, instance)?;
    }

    // A committed route's owner admits and applies each edit itself, and a
    // committing host holds no channel keys: it could neither open a sealed
    // edit nor accept cleartext on the route after one. Committed routes
    // therefore stay cleartext end to end.
    let encryption =
        std::sync::Arc::new(std::sync::Mutex::new(if asks_channel_keys && !committed {
            super::route_keys::RouteKeys::requested()
        } else {
            super::route_keys::RouteKeys::plain()
        }));
    // The streams are independent: subscribing to them together costs the
    // join one broker round trip instead of one per stream.
    let ((), (), (), (), (), (), presence) = tokio::join!(
        async {
            if !committed {
                spawn_op_stream(&channel, inbound_tx, std::sync::Arc::clone(&encryption)).await;
            }
        },
        spawn_context_stream(&channel, spec.clone(), inbound_tx),
        spawn_action_stream(&channel, inbound_tx),
        spawn_invocation_stream(&channel, spec, inbound_tx),
        spawn_realtime_stream(&channel, inbound_tx),
        async {
            // This intentionally fails quietly for person sessions. An
            // admitted agent gets a separate broker-addressed queue; a
            // document agent never has to infer task work from room traffic.
            if !crate::host::environment().identity.managed_agent_context {
                spawn_task_run_stream(&channel, inbound_tx).await;
            }
        },
        spawn_presence_stream(&channel, inbound_tx),
    );
    spawn_content_stream(&channel, spec.clone(), inbound_tx).await;
    // Subscriptions are installed first so an existing holder's response
    // cannot race ahead of our document stream.
    if !committed && let Err(error) = channel.request_document_snapshot().await {
        tracing::warn!(%error, "collab: late-join snapshot request failed");
    }

    media_routes.insert(channel_id.clone(), channel.clone());
    if spec.record.is_none()
        && participant_kind == "person"
        && crate::host::environment()
            .identity
            .collab_service_configured
    {
        audio_output::install(channel.clone());
    }
    routes.insert(
        channel_id,
        SdkRoute {
            spec: spec.clone(),
            channel,
            presence,
            workspace_controls: std::collections::HashMap::new(),
            committed,
            encryption,
        },
    );
    Ok(())
}

/// Bridge content arriving for `channel`'s record into
/// [`CollabEvent::RecordContent`] on `inbound_tx`. A host that keeps no
/// record content refuses the subscription, and its apps keep their content
/// to themselves.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_content_stream(
    channel: &runtime_collab::ChannelHandle,
    route: RouteSpec,
    inbound_tx: &InboundBridge,
) {
    let Ok(stream) = channel.subscribe_blobs().await else {
        return;
    };
    super::record_content::ContentLane::of(&route.app_id, route.record.as_deref().unwrap_or(""))
        .serve();
    let inbound_tx = inbound_tx.clone();
    tokio::task::spawn_local(async move {
        while let Ok(event) = stream.read().await {
            if !inbound_tx
                .send_document(CollabEvent::RecordContent {
                    route: route.clone(),
                    hash: event.hash,
                    bytes: event.bytes,
                })
                .await
            {
                break;
            }
        }
    });
}

/// Bridge `channel`'s doc/incremental op stream into [`CollabEvent`]s on
/// `inbound_tx`, spawned as a `LocalSet` task so it doesn't block the
/// caller's route setup.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_op_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
    encryption: SharedRouteKeys,
) {
    if let Ok(stream) = channel.subscribe().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(event) = stream.read().await {
                let translated = match postcard::from_bytes::<SignedOp>(&event.op) {
                    Ok(envelope) => {
                        match signed_document_event(envelope, event.from_user_id, &encryption) {
                            Ok(event) => event,
                            Err(UndeliveredDocument::Held) => continue,
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "collab: signed op has no typed document payload; dropping"
                                );
                                continue;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            "collab: signed op is not postcard SignedOp ({error}); dropping"
                        );
                        continue;
                    }
                };
                let Some(complete) = complete_document_event(translated, &tx.reassembly.live)
                else {
                    continue;
                };
                if !tx.send_document(complete).await {
                    break;
                }
            }
        });
    }
}

/// Bridge `channel`'s collaborator/permissions context stream into
/// [`CollabEvent::Context`] on `inbound_tx`.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_context_stream(
    channel: &runtime_collab::ChannelHandle,
    route: RouteSpec,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_context().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(context) = stream.read().await {
                if context.snapshot_requested {
                    if !tx.send_durable(CollabEvent::DocumentSnapshotRequested {
                        route: route.clone(),
                    }) {
                        break;
                    }
                    continue;
                }
                if let Some(peer_id) = context.follow_changed {
                    if !tx.send_durable(CollabEvent::FollowChanged { peer_id }) {
                        break;
                    }
                    continue;
                }
                if let Some(message) = context.handoff {
                    if let Some(request) = super::handoff::HandoffRequest::decode(&message) {
                        if !tx.send_durable(CollabEvent::Handoff { request }) {
                            break;
                        }
                    } else {
                        tracing::warn!("collab: malformed handoff request dropped");
                    }
                    continue;
                }
                let collaborators = context
                    .collaborators
                    .into_iter()
                    .map(|collaborator| Collaborator {
                        id: collaborator.user_id,
                        name: collaborator.display_name,
                        colour: Collaborator::colour_from_packed(collaborator.color),
                        is_agent: collaborator.is_agent,
                        is_self: collaborator.is_self,
                    })
                    .collect();
                if !tx.send_durable(CollabEvent::Context {
                    collaborators,
                    permissions: context.permissions,
                }) {
                    break;
                }
            }
        });
    }
}

#[derive(Debug)]
struct NativeInvocationTurn {
    permits: Vec<tokio::sync::OwnedSemaphorePermit>,
    deadline: Instant,
}

// Use the existing three-second native publication hydration bound. The
// accepted invocation stays on its original stream task, without another queue.
async fn native_invocation_delivery_turn(
    key: &str,
    state: &BridgeState,
) -> Result<Option<NativeInvocationTurn>, String> {
    let Some(generation) = state.document_reads.delivery_generation(key) else {
        return Ok(None);
    };
    let deadline = crate::time::now() + Duration::from_secs(3);
    loop {
        if !state.is_healthy() || !state.document_reads.current(key, &generation) {
            return Err("original invocation document custody was retired".into());
        }
        if crate::time::now() >= deadline {
            return Err("original invocation capture did not become current".into());
        }
        let mut turns = Vec::new();
        let owners = state.document_reads.document_owner_routes(key)?;
        for owner in &owners {
            let semaphore = state.document_reads.delivery_turn(owner)?;
            let remaining = deadline.saturating_duration_since(crate::time::now());
            let turn = tokio::time::timeout(remaining, semaphore.acquire_owned())
                .await
                .map_err(|_| "original invocation capture did not become current")?
                .map_err(|_| "original invocation delivery turn closed")?;
            turns.push(turn);
        }
        if !state.is_healthy() || !state.document_reads.current(key, &generation) {
            return Err("original invocation document custody was retired".into());
        }
        if crate::time::now() >= deadline {
            return Err("original invocation capture did not become current".into());
        }
        if state.document_reads.document_owner_routes(key)? == owners
            && owners
                .iter()
                .all(|owner| state.document_reads.publishable(owner))
        {
            return Ok(Some(NativeInvocationTurn {
                permits: turns,
                deadline,
            }));
        }
        drop(turns);
        if crate::time::now() >= deadline {
            return Err("original invocation capture did not become current".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// This existing host verb rechecks the original channel's current read
// custody and exact pending job. A waited delivery cannot reuse stale authority.
#[allow(
    clippy::future_not_send,
    reason = "the original Rc-backed Cap'n Proto channel is driven only on the shovel LocalSet"
)]
async fn recheck_native_invocation(
    channel: &runtime_collab::ChannelHandle,
    invocation: &runtime_collab::Invocation,
) -> Result<bool, String> {
    if !channel
        .invocation_cancellation(invocation.id)
        .await
        .map_err(|error| error.to_string())?
    {
        return Ok(true);
    }
    let update = tilo_actions::InvocationUpdate::Cancelled {
        invocation: tilo_actions::InvocationId(Uuid::from_bytes(invocation.invocation_uuid)),
        message: None,
    };
    let wire = serde_json::to_string(&update).map_err(|error| error.to_string())?;
    channel
        .update_invocation(invocation.id, &wire)
        .await
        .map_err(|error| error.to_string())?;
    Ok(false)
}

/// Bridge `channel`'s action stream into [`CollabEvent::ActionArrived`] on
/// `inbound_tx`.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_action_stream(channel: &runtime_collab::ChannelHandle, inbound_tx: &InboundBridge) {
    if let Ok(stream) = channel.subscribe_actions().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(action) = stream.read().await {
                if !tx.send_durable(CollabEvent::ActionArrived {
                    type_id: action.type_id,
                    payload: action.payload,
                    from_user_id: action.from_user_id,
                }) {
                    break;
                }
            }
        });
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_invocation_stream(
    channel: &runtime_collab::ChannelHandle,
    spec: &RouteSpec,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_stable_invocations().await {
        let tx = inbound_tx.clone();
        let cancellation_channel = channel.clone();
        let key = spec.channel_id();
        tokio::task::spawn_local(async move {
            while let Ok(invocation) = stream.read().await {
                let turn = match native_invocation_delivery_turn(&key, &tx.state).await {
                    Ok(turn) => turn,
                    Err(error) => {
                        tx.state
                            .fail_closed("native invocation capture; outcome unknown");
                        tracing::warn!(%error, "accepted invocation retains unknown outcome");
                        break;
                    }
                };
                if let Some(turn) = &turn {
                    match tokio::time::timeout_at(
                        tokio::time::Instant::from_std(turn.deadline),
                        recheck_native_invocation(&cancellation_channel, &invocation),
                    )
                    .await
                    {
                        Ok(Ok(true)) if crate::time::now() < turn.deadline => {}
                        Ok(Ok(false)) => continue,
                        result => {
                            tx.state
                                .fail_closed("native invocation authority; outcome unknown");
                            tracing::warn!(?result, "accepted invocation authority unavailable");
                            break;
                        }
                    }
                }
                let id = invocation.id;
                let context = parse_invocation_context(id, &invocation.context_json);
                if !tx.send_durable_turn(
                    CollabEvent::Invocation {
                        id,
                        invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                            invocation.invocation_uuid,
                        )),
                        resource: invocation.resource,
                        action: invocation.action,
                        argument: invocation.argument,
                        actor: invocation.actor,
                        context: context.map(Box::new),
                    },
                    turn.map(|turn| EventTurn::Document(turn.permits)),
                    None,
                ) {
                    break;
                }
                let cancellation_tx = tx.clone();
                let channel = cancellation_channel.clone();
                tokio::task::spawn_local(async move {
                    loop {
                        tokio::time::sleep(crate::time::Duration::from_millis(100)).await;
                        match channel.invocation_cancellation(id).await {
                            Ok(false) => {}
                            Ok(true) => {
                                let _ = cancellation_tx
                                    .send_durable(CollabEvent::InvocationCancellation { id });
                                break;
                            }
                            // The broker removes terminal invocations, so an
                            // unknown-id error is also the watcher stop signal.
                            Err(_) => break,
                        }
                    }
                });
            }
        });
    }
}

/// The primary route's live-activity capability and the task following the
/// controls pressed on it.
struct LiveActivityLink {
    handle: runtime_collab::LiveActivityHandle,
    follower: tokio::task::JoinHandle<()>,
}

impl Drop for LiveActivityLink {
    fn drop(&mut self) {
        self.follower.abort();
    }
}

impl LiveActivityLink {
    /// Send the app's activity if it changed, or again after a join.
    #[allow(
        clippy::future_not_send,
        reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
    )]
    async fn flush(&self, resend: bool) {
        let Some(unsent) = super::channel_live_activity::take_unsent(resend) else {
            return;
        };
        if let Err(error) = self.handle.show(unsent.activity.as_ref()).await {
            tracing::warn!(%error, "live activity: the shell did not take it");
        }
    }
}

/// Open the primary route's live activity and follow the controls pressed
/// on it, waking the owner on each.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn link_live_activity(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Option<LiveActivityLink> {
    let handle = match channel.live_activity().await {
        Ok(handle) => handle,
        Err(error) => {
            tracing::debug!(%error, "live activity: the shell presents none for this app");
            return None;
        }
    };
    let follower = {
        let handle = handle.clone();
        let wake = inbound_tx.state.owner_wake.clone();
        tokio::task::spawn_local(async move {
            let mut after = 0;
            loop {
                match handle.controls(after).await {
                    Ok((serial, controls)) => {
                        after = serial;
                        super::channel_live_activity::received(controls);
                        wake.wake();
                    }
                    Err(error) => {
                        tracing::warn!(%error, "live activity: following its controls stopped");
                        break;
                    }
                }
            }
        })
    };
    Some(LiveActivityLink { handle, follower })
}

/// The primary route's settings capability and the task following it.
struct SettingsLink {
    handle: runtime_collab::AppSettingsHandle,
    settings: Arc<super::channel_settings::ChannelSettings>,
    follower: tokio::task::JoinHandle<()>,
}

impl Drop for SettingsLink {
    fn drop(&mut self) {
        self.follower.abort();
        self.settings.disconnected();
    }
}

fn settings_view(
    view: &runtime_collab::SettingsView,
) -> Result<(u64, serde_json::Map<String, serde_json::Value>), String> {
    match serde_json::from_slice(&view.document_json) {
        Ok(serde_json::Value::Object(document)) => Ok((view.revision, document)),
        _ => Err("the shell's settings view is not a JSON object".into()),
    }
}

impl SettingsLink {
    /// Send the app's unsent writes, oldest first, and show the shell's
    /// answers.
    #[allow(
        clippy::future_not_send,
        reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
    )]
    async fn flush(&self, inbound_tx: &InboundBridge) {
        for (section, values) in self.settings.take_outbox() {
            let unanswered = super::channel_settings::Unanswered::new(&self.settings);
            let answer = match serde_json::to_vec(&values) {
                Ok(values) => self
                    .handle
                    .update(&section, &values)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|view| settings_view(&view)),
                Err(error) => Err(error.to_string()),
            };
            if unanswered.answer(answer) {
                inbound_tx.state.owner_wake.wake();
            }
        }
    }
}

/// Open the primary route's settings for the installed channel settings
/// and follow the shell's view of them, waking the owner on each change.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn link_settings(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Option<SettingsLink> {
    let settings = super::channel_settings::installed()?;
    let handle = match channel.settings().await {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(%error, "settings: the shell keeps no settings for this app");
            return None;
        }
    };
    settings.linked();
    let follower = {
        let handle = handle.clone();
        let settings = Arc::clone(&settings);
        let wake = inbound_tx.state.owner_wake.clone();
        tokio::task::spawn_local(async move {
            let mut after = 0;
            loop {
                let view = match handle.read(after).await {
                    Ok(view) => view,
                    Err(error) => {
                        tracing::warn!(%error, "settings: following the shell's settings stopped");
                        settings.disconnected();
                        break;
                    }
                };
                after = view.revision;
                match settings_view(&view) {
                    Ok((revision, view)) => {
                        if settings.received(revision, view) {
                            wake.wake();
                        }
                    }
                    Err(error) => tracing::warn!(%error, "settings: ignoring a view"),
                }
            }
        })
    };
    Some(SettingsLink {
        handle,
        settings,
        follower,
    })
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_realtime_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_realtime().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(bytes) = stream.read().await {
                if bytes.len() > crate::realtime_continuity::MAX_REALTIME_WIRE_BYTES {
                    continue;
                }
                let Ok(event) =
                    serde_json::from_slice::<crate::realtime_continuity::RealtimeEvent>(&bytes)
                else {
                    continue;
                };
                if !event.valid() || !tx.send_durable(CollabEvent::Realtime(event)) {
                    break;
                }
            }
        });
    }
}

fn parse_invocation_context(id: u64, json: &str) -> Option<tilo_actions::ActionContext> {
    if json.trim().is_empty() {
        return None;
    }
    match serde_json::from_str(json) {
        Ok(context) => Some(context),
        Err(error) => {
            tracing::warn!(%error, id, "collab: ignoring invalid invocation context");
            None
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_action_catalogue_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Result<(), String> {
    let stream = channel
        .subscribe_action_catalogue()
        .await
        .map_err(|error| format!("subscribing to action catalogue failed: {error}"))?;
    let initial = stream
        .read()
        .await
        .map_err(|error| format!("reading initial action catalogue failed: {error}"))?;
    // The initial baseline occupies the same turn as subsequent watched
    // snapshots. Otherwise a busy constructor retains both before its first
    // owner poll, which can exhaust the bridge despite each baseline fitting.
    let turns = Arc::new(tokio::sync::Semaphore::new(1));
    let turn = turns
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| error.to_string())?;
    admit_initial_action_catalogue(&initial, inbound_tx, Some(turn))?;

    let tx = inbound_tx.clone();
    tokio::task::spawn_local(async move {
        forward_action_catalogues(async || stream.read().await.map_err(|_| ()), &tx, turns).await;
    });
    Ok(())
}

/// Decode and admit the initial snapshot through the same bounded owner queue.
fn admit_initial_action_catalogue(
    initial: &str,
    tx: &InboundBridge,
    turn: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<(), String> {
    let catalogue = serde_json::from_str::<BrokerActionCatalogueWire>(initial)
        .map_err(|error| format!("decoding initial action catalogue failed: {error}"))?;
    if !tx.send_durable_turn(
        CollabEvent::ActionCatalogue {
            surfaces: catalogue.surfaces,
        },
        turn.map(EventTurn::Snapshot),
        Some(initial.len()),
    ) {
        return Err("delivering initial action catalogue failed".to_string());
    }
    Ok(())
}

/// Forward catalogue snapshots from `read` until it or the bridge fails.
/// Each read returns the broker's latest full catalogue (a watch), so at
/// most one catalogue is queued at a time: the next read waits until the
/// owner has dequeued the previous one, and intermediate snapshots coalesce
/// in the broker. A busy owner (a headless worker between pumps) otherwise
/// accumulated one large catalogue per publication until the bounded bridge
/// failed closed.
async fn forward_action_catalogues(
    mut read: impl AsyncFnMut() -> Result<String, ()>,
    tx: &InboundBridge,
    turns: Arc<tokio::sync::Semaphore>,
) {
    loop {
        let Ok(turn) = turns.clone().acquire_owned().await else {
            break;
        };
        let Ok(snapshot) = read().await else {
            break;
        };
        match serde_json::from_str::<BrokerActionCatalogueWire>(&snapshot) {
            Ok(catalogue) => {
                if !tx.send_durable_turn(
                    CollabEvent::ActionCatalogue {
                        surfaces: catalogue.surfaces,
                    },
                    Some(EventTurn::Snapshot(turn)),
                    Some(snapshot.len()),
                ) {
                    break;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "collab: invalid broker action catalogue");
            }
        }
    }
}

/// Bridge broker-addressed task leases only when this connection is an
/// admitted agent. The server refuses the subscription for people and for
/// unadmitted agent identities, leaving their normal collaboration intact.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send and are driven only by the shovel LocalSet"
)]
async fn spawn_task_run_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_task_runs().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(run) = stream.read().await {
                if !tx.send_durable(CollabEvent::TaskRun(run)) {
                    break;
                }
            }
        });
    }
}

/// Open `channel`'s presence handle and, if available, bridge its stream
/// into translated [`CollabEvent`]s on `inbound_tx`. Returns the handle so
/// the caller can store it on the route (needed for outbound presence
/// sends), regardless of whether the inbound stream subscribed.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_presence_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Option<runtime_collab::PresenceHandle> {
    match channel.presence().await {
        Ok(handle) => {
            if let Ok(stream) = handle.subscribe_all().await {
                let tx = inbound_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(event) = stream.read().await {
                        if let Some(event) = translate_presence_event(event)
                            && !tx.send_presence(event)
                        {
                            break;
                        }
                    }
                });
            }
            Some(handle)
        }
        Err(_) => None,
    }
}

static LAST_CERTIFIED_DOCUMENT_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn next_certified_document_seq(now: crate::time::SystemTime) -> u64 {
    use std::sync::atomic::Ordering;

    let wall = now
        .duration_since(crate::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
        });
    let mut observed = LAST_CERTIFIED_DOCUMENT_SEQ.load(Ordering::Relaxed);
    loop {
        let candidate = wall.max(observed.saturating_add(1));
        match LAST_CERTIFIED_DOCUMENT_SEQ.compare_exchange_weak(
            observed,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return candidate,
            Err(actual) => observed = actual,
        }
    }
}

/// Keep the accepted request and its queue byte reservation on the worker
/// stack while hydration catches up. No later barrier or command can overtake
/// it. Hydration runs independently on this same LocalSet.
#[allow(
    clippy::future_not_send,
    reason = "the route contains a capnp-rpc handle and runs only on the shovel LocalSet"
)]
async fn wait_for_document_publication(
    route: &SdkRoute,
    state: &BridgeState,
) -> Result<(), DocumentPublicationError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let key = route.spec.channel_id();
    loop {
        if !state.is_healthy() {
            return Err(state.publication_failed(
                "queued document publication",
                "worker invalidated before admission",
                true,
            ));
        }
        // The deadline is terminal, even when readiness changes concurrently.
        if tokio::time::Instant::now() >= deadline {
            return Err(state.publication_failed(
                "queued document publication",
                "hydration did not become current before deadline",
                true,
            ));
        }
        if state.document_reads.publishable(&key) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Sign on the shovel thread through the workspace identity service, then use
/// the one signed Cap'n Proto verb. Missing or denied certification never
/// downgrades the document to unsigned traffic. A mutation larger than one
/// frame is sent as consecutive segments, each encrypted (on MLS routes),
/// signed and validated by the broker as its own envelope.
#[allow(
    clippy::future_not_send,
    reason = "the route contains a capnp-rpc handle and runs only on the shovel LocalSet"
)]
async fn send_document_mutation(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    mutation: collab_types::DocumentMutation,
    authorization: Option<crate::invocation_scope::DocumentAuthorization>,
    inbound: &InboundBridge,
) -> Result<(), DocumentPublicationError> {
    wait_for_document_publication(route, &inbound.state).await?;
    if let Some(authorization) = &authorization {
        authorization
            .check(&route.spec, &mutation)
            .map_err(|error| {
                inbound
                    .state
                    .publication_failed("original document contributors", error, true)
            })?;
    }
    if let LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Host(host)) = signer {
        // The standalone owner admits cleartext state. Never bypass an MLS
        // requirement or produce a signed cleartext replacement for ciphertext.
        if !route
            .encryption
            .lock()
            .is_ok_and(|encryption| matches!(encryption.keys, super::route_keys::Keys::Plain))
        {
            return Err(inbound.state.publication_failed(
                "host document encryption unavailable",
                "cleartext commit refused",
                true,
            ));
        }
        let committed_custody = authorization.clone();
        let commit = match authorization {
            Some(authorization) => host.commit_scoped(&route.spec, mutation, authorization),
            None => host.commit(&route.spec, mutation),
        };
        let result = commit.await.and_then(|op| {
            if let Some(authorization) = &committed_custody {
                authorization.check_committed_operation(&route.spec, &op)?;
            }
            postcard::to_stdvec(&op).map_err(|e| e.to_string())
        });
        match result {
            Ok(bytes) => {
                if let Err(error) = route.channel.send(&bytes).await {
                    tracing::warn!(%error, "collab: precommitted host document handoff failed; outcome unknown");
                    return Err(inbound.state.publication_failed(
                        "host document receipt handoff",
                        error,
                        true,
                    ));
                }
                if let Some(authorization) = &committed_custody {
                    authorization.acknowledge_committed();
                }
            }
            Err(error) => {
                tracing::warn!(%error, "collab: host document commit failed; outcome unknown");
                return Err(inbound
                    .state
                    .publication_failed("host document commit", error, true));
            }
        }
        return Ok(());
    }
    if authorization.is_some() {
        return Err(inbound.state.publication_failed(
            "scoped document evaluator unavailable",
            "original custody requires the actual final owner",
            true,
        ));
    }
    let frames = match mutation.encode_frames() {
        Ok(frames) => frames,
        Err(error) => {
            // The size was checked before the change was queued, so this
            // is unreachable for SDK callers; never send a partial change.
            tracing::error!(%error, "collab: typed document mutation would not encode; dropping");
            return Err(inbound.state.publication_failed(
                "document signing or encoding",
                error,
                true,
            ));
        }
    };
    let count = frames.len();
    for (index, frame) in frames.into_iter().enumerate() {
        if let Err(error) = send_document_frame(route, signer, frame, &inbound.state).await {
            if count > 1 {
                tracing::warn!(
                    segment = index,
                    segments = count,
                    "collab: segmented document transfer stopped; receivers will discard it"
                );
            }
            return Err(error);
        }
    }
    Ok(())
}

/// Encrypt, sign and send one typed document frame, or hold it on the
/// device while the route waits for its channel keys. A frame the broker
/// refuses is sent once more after a key refresh that found newer keys: a
/// membership change moves the channel to a new epoch, and the broker refuses
/// content sealed under an older one.
#[allow(
    clippy::future_not_send,
    reason = "the route contains a capnp-rpc handle and runs only on the shovel LocalSet"
)]
async fn send_document_frame(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    cleartext: Vec<u8>,
    state: &BridgeState,
) -> Result<(), DocumentPublicationError> {
    match try_send_frame(route, signer, cleartext.clone(), state).await {
        FrameSend::Sent | FrameSend::Held => Ok(()),
        FrameSend::Failed(error) => Err(error),
        FrameSend::Refused(refusal) => {
            if !refresh_route_keys(route, signer).await {
                return Err(state.publication_failed("document commit RPC", refusal, true));
            }
            match try_send_frame(route, signer, cleartext, state).await {
                FrameSend::Sent | FrameSend::Held => Ok(()),
                FrameSend::Failed(error) => Err(error),
                FrameSend::Refused(refusal) => {
                    Err(state.publication_failed("document commit RPC", refusal, true))
                }
            }
        }
    }
}

#[allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "the route contains a capnp-rpc handle and runs only on the shovel LocalSet; sealing, signing and sending one frame stay one auditable path"
)]
async fn try_send_frame(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    cleartext: Vec<u8>,
    state: &BridgeState,
) -> FrameSend {
    let workspace_id = route.spec.cluster_id.to_string();
    let record_id = route.spec.record.as_deref().unwrap_or("");
    let artifact = collab_types::document_artifact_id(&route.spec.app_id, &workspace_id, record_id);
    let (payload, encryption_metadata) =
        match prepare_document_payload(&route.encryption, artifact, cleartext) {
            PreparedFrame::Ready(payload, metadata) => (payload, metadata),
            PreparedFrame::Held => return FrameSend::Held,
            PreparedFrame::Failed => {
                return FrameSend::Failed(state.publication_failed(
                    "document encryption",
                    "payload unavailable",
                    true,
                ));
            }
        };
    let now = crate::time::wall();
    let author = match signer {
        // Host-signed documents commit through the host owner before this
        // frame path; an inherited host signer cannot sign a frame here.
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Host(_)) => {
            return FrameSend::Failed(state.publication_failed(
                "document signing or encoding",
                "inherited host signer cannot sign a document frame",
                true,
            ));
        }
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            identity.signing_public_key()
        }
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(key)) => {
            key.verifying_key().to_bytes()
        }
        LocalSdkDocumentSigner::Host(identity) => identity.signing_public_key(),
    };
    let author = collab_types::Ed25519PublicKey::from_bytes(author);
    let attestation =
        collab_types::document_attestation_id(&route.spec.app_id, &workspace_id, record_id);
    let seq = next_certified_document_seq(now);
    let mut op = match encryption_metadata {
        Some(metadata) => SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            author,
            attestation,
            collab_types::OpClass::Splice,
            now,
            seq,
            metadata,
        ),
        None => SignedOp::new_unsigned(
            payload,
            artifact,
            author,
            attestation,
            collab_types::OpClass::Splice,
            now,
            seq,
        ),
    };
    let unsigned_op = match postcard::to_stdvec(&op) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "collab: unsigned document envelope would not encode; dropping");
            return FrameSend::Failed(state.publication_failed(
                "document signing or encoding",
                error,
                true,
            ));
        }
    };
    let signature = match signer {
        // Host-signed documents commit through the host owner before this
        // frame path; an inherited host signer cannot sign a frame here.
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Host(_)) => {
            return FrameSend::Failed(state.publication_failed(
                "document signing or encoding",
                "inherited host signer cannot sign a document frame",
                true,
            ));
        }
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => match identity
            .sign_collaboration_op(&workspace_id, &route.spec.app_id, record_id, &unsigned_op)
        {
            Ok(signature) => signature,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "collab: identity service denied document operation; dropping"
                );
                return FrameSend::Failed(state.publication_failed(
                    "document signing or encoding",
                    error,
                    true,
                ));
            }
        },
        LocalSdkDocumentSigner::Host(identity) => match identity
            .sign_collaboration_op(&workspace_id, &route.spec.app_id, record_id, &unsigned_op)
            .await
        {
            Ok(signature) => signature,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "collab: host identity service denied document operation; dropping"
                );
                return FrameSend::Failed(state.publication_failed(
                    "host identity signing",
                    error,
                    !identity.is_usable(),
                ));
            }
        },
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(key)) => {
            use ed25519_dalek::Signer as _;
            let signing_bytes = match op.signing_bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "collab: document op would not canonicalize; dropping");
                    return FrameSend::Failed(state.publication_failed(
                        "document signing or encoding",
                        error,
                        true,
                    ));
                }
            };
            key.sign(&signing_bytes).to_bytes()
        }
    };
    op.signature = collab_types::Signature::from_bytes(signature);
    match postcard::to_stdvec(&op) {
        Ok(bytes) => match route.channel.send(&bytes).await {
            Ok(()) => FrameSend::Sent,
            Err(error) => {
                tracing::warn!(%error, "collab: document commit failed; outcome unknown");
                FrameSend::Refused(error.to_string())
            }
        },
        Err(error) => {
            tracing::warn!(%error, "collab: signed document envelope would not encode; dropping");
            FrameSend::Failed(state.publication_failed("document encoding", error, true))
        }
    }
}

/// Storage class of one cleartext document frame: a snapshot (or its first
/// segment) starts a new retained baseline, everything else follows it.
fn document_frame_retention(cleartext: &[u8]) -> collab_types::ContentRetention {
    match collab_types::DocumentFrame::decode(cleartext) {
        Ok(collab_types::DocumentFrame::Whole(collab_types::DocumentMutation::Snapshot(_))) => {
            collab_types::ContentRetention::Checkpoint
        }
        Ok(collab_types::DocumentFrame::Segment(segment))
            if segment.kind == collab_types::DocumentMutationKind::Snapshot
                && segment.index == 0 =>
        {
            collab_types::ContentRetention::Checkpoint
        }
        _ => collab_types::ContentRetention::Delta,
    }
}

/// Seal one frame under the route's current channel content key, so every
/// member — including one added later — can read it from history. A route
/// still waiting for its keys keeps the frame on the device instead.
fn prepare_document_payload(
    encryption: &SharedRouteKeys,
    artifact: collab_types::ArtifactId,
    cleartext: Vec<u8>,
) -> PreparedFrame {
    let Ok(mut keys) = encryption.lock() else {
        tracing::warn!("collab: route key state lock poisoned; dropping mutation");
        return PreparedFrame::Failed;
    };
    let retention = document_frame_retention(&cleartext);
    match keys.seal(artifact, retention, cleartext.clone()) {
        super::route_keys::Sealed::Ready(payload, metadata) => {
            PreparedFrame::Ready(payload, metadata)
        }
        super::route_keys::Sealed::Hold => {
            if keys.hold_outbound(cleartext) {
                PreparedFrame::Held
            } else {
                tracing::warn!("collab: too many edits waiting for channel access; dropping");
                PreparedFrame::Failed
            }
        }
        super::route_keys::Sealed::Refused(reason) => {
            tracing::warn!(%reason, "collab: dropping mutation");
            PreparedFrame::Failed
        }
    }
}

enum PreparedFrame {
    Ready(Vec<u8>, Option<collab_types::EncryptionMetadata>),
    Held,
    Failed,
}

/// How one frame's send ended.
enum FrameSend {
    Sent,
    Held,
    /// Not sealed or not signed; retrying cannot help. Already recorded.
    Failed(DocumentPublicationError),
    /// The broker refused it, possibly because its keys were stale. Not yet
    /// recorded: a retry under refreshed keys may still succeed.
    Refused(String),
}

/// Ask the identity service for `route`'s current keys. Returns whether
/// the route's state or keys changed.
#[allow(
    clippy::future_not_send,
    reason = "the host identity transport is retained only on the shovel LocalSet"
)]
async fn refresh_route_keys(route: &SdkRoute, signer: &mut LocalSdkDocumentSigner) -> bool {
    let Ok(known) = route
        .encryption
        .lock()
        .map(|keys| keys.asks_identity_service().then(|| keys.generation()))
    else {
        return false;
    };
    let Some(generation) = known else {
        return false;
    };
    let workspace_id = route.spec.cluster_id.to_string();
    let record_id = route.spec.record.as_deref().unwrap_or("");
    let reply = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            let query = KeyQuery::of(route, generation);
            let mut fetched = certified_channel_keys(std::sync::Arc::clone(identity), vec![query])
                .await
                .into_iter();
            return fetched
                .next()
                .is_some_and(|(_, _, reply)| update_route_keys(route, reply, generation));
        }
        LocalSdkDocumentSigner::Host(identity) => {
            identity
                .channel_keys(&workspace_id, &route.spec.app_id, record_id, generation)
                .await
        }
        LocalSdkDocumentSigner::Inherited(
            SdkDocumentSigner::Harness(_) | SdkDocumentSigner::Host(_),
        ) => return false,
    };
    update_route_keys(route, answered_keys(reply, generation), generation)
}

/// The keys the identity service answered, or, when it could not answer,
/// that they are unavailable.
fn answered_keys(
    reply: Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>,
    generation: u64,
) -> runtime_identity::wire::ChannelKeysReply {
    reply.unwrap_or_else(|error| {
        tracing::debug!(?error, "collab: identity service refused channel keys");
        runtime_identity::wire::ChannelKeysReply {
            access: runtime_identity::wire::ChannelAccess::Unavailable,
            generation,
            keys: None,
        }
    })
}

/// Take the identity service's `reply` for `route`, asked at `generation`.
/// Returns whether the route's state or keys changed.
fn update_route_keys(
    route: &SdkRoute,
    reply: runtime_identity::wire::ChannelKeysReply,
    generation: u64,
) -> bool {
    let Ok(mut keys) = route.encryption.lock() else {
        return false;
    };
    let state = keys.state();
    keys.update(reply.access, reply.generation, reply.keys.as_deref());
    keys.generation() != generation || keys.state() != state
}

/// One route's question to the identity service, taken off the route so it
/// can be asked on another thread.
struct KeyQuery {
    route: String,
    workspace_id: String,
    app_id: String,
    record_id: String,
    generation: u64,
}

impl KeyQuery {
    fn of(route: &SdkRoute, generation: u64) -> Self {
        Self {
            route: route.spec.channel_id(),
            workspace_id: route.spec.cluster_id.to_string(),
            app_id: route.spec.app_id.clone(),
            record_id: route.spec.record.clone().unwrap_or_default(),
            generation,
        }
    }
}

/// The identity service's answers to one round of key questions: by route,
/// with the generation each was asked at.
type FetchedKeys = Vec<(String, u64, runtime_identity::wire::ChannelKeysReply)>;

/// A round of key questions in flight on a blocking thread.
type KeyFetch = std::pin::Pin<Box<dyn std::future::Future<Output = FetchedKeys>>>;

/// Ask a certified identity service `queries` on a blocking thread: its
/// socket answers as slowly as the service does, and the shovel thread
/// carries every route's ops meanwhile.
async fn certified_channel_keys(
    identity: std::sync::Arc<dyn SdkCertifiedIdentity>,
    queries: Vec<KeyQuery>,
) -> FetchedKeys {
    tokio::task::spawn_blocking(move || {
        queries
            .into_iter()
            .map(|query| {
                let reply = identity.channel_keys(
                    &query.workspace_id,
                    &query.app_id,
                    &query.record_id,
                    query.generation,
                );
                (
                    query.route,
                    query.generation,
                    answered_keys(reply, query.generation),
                )
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Start a round of key questions for every route that asks the identity
/// service. A certified service is asked off this thread and the round is
/// returned to be awaited alongside the loop; a host's asynchronous service
/// is asked here, and its answers are taken at once.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
async fn refresh_keys(
    routes: &std::collections::HashMap<String, SdkRoute>,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
) -> Option<KeyFetch> {
    let LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) = signer else {
        refresh_all_route_keys(routes, signer, inbound_tx).await;
        return None;
    };
    let queries: Vec<KeyQuery> = routes
        .values()
        .filter_map(|route| {
            let keys = route.encryption.lock().ok()?;
            keys.asks_identity_service()
                .then(|| KeyQuery::of(route, keys.generation()))
        })
        .collect();
    if queries.is_empty() {
        return None;
    }
    Some(Box::pin(certified_channel_keys(
        std::sync::Arc::clone(identity),
        queries,
    )))
}

/// Take a finished round's answers, then deliver what waited on them.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
async fn apply_fetched_keys(
    routes: &std::collections::HashMap<String, SdkRoute>,
    fetched: FetchedKeys,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
) {
    for (key, asked, reply) in fetched {
        let Some(route) = routes.get(&key) else {
            continue;
        };
        // A resend refreshed the route while the round was out: its answer
        // is newer than this one.
        if route
            .encryption
            .lock()
            .is_ok_and(|keys| keys.generation() == asked)
        {
            update_route_keys(route, reply, asked);
        }
        if !deliver_waiting(route, signer, inbound_tx).await {
            return;
        }
    }
}

/// Bring every route's keys up to date, report access changes to the app,
/// then send the edits and deliver the ciphertext that waited on them.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
async fn refresh_all_route_keys(
    routes: &std::collections::HashMap<String, SdkRoute>,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
) {
    for route in routes.values() {
        refresh_route_keys(route, signer).await;
        if !deliver_waiting(route, signer, inbound_tx).await {
            return;
        }
    }
}

/// Report `route`'s access change to the app, then send the edits and
/// deliver the ciphertext that waited on its keys. `false` once the app
/// side is gone.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
async fn deliver_waiting(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
) -> bool {
    let (state, outbound, inbound) = {
        let Ok(mut keys) = route.encryption.lock() else {
            return true;
        };
        (
            keys.take_state_change(),
            keys.take_held_outbound(),
            keys.take_held_inbound(),
        )
    };
    if let Some(state) = state
        && !inbound_tx.send_durable(CollabEvent::ChannelAccess {
            route: route.spec.clone(),
            state,
        })
    {
        return false;
    }
    for frame in outbound {
        if send_document_frame(route, signer, frame, &inbound_tx.state)
            .await
            .is_err()
        {
            tracing::warn!("collab: an edit that waited for channel access was not sent");
        }
    }
    for (envelope, from_user_id) in inbound {
        let Ok(event) = signed_document_event(envelope, from_user_id, &route.encryption) else {
            continue;
        };
        if let Some(complete) = complete_document_event(event, &inbound_tx.reassembly.live)
            && !inbound_tx.send_document(complete).await
        {
            return false;
        }
    }
    true
}

/// Shovel-thread Cap'n Proto endpoint for an application's pull directory.
/// It deliberately contains no `Widget`, `App`, or document reference.  The
/// only path to authority is a one-shot request answered by App's main-thread
/// pump below.
struct ActionDirectoryServer {
    requests: mpsc::SyncSender<ActionDirectoryRequest>,
    owner: crate::wake::WakeHandle,
}

/// Queue one request for the App's pump and wake it: an idle owner sleeps
/// until an event arrives, and nothing else announces this one.
fn submit_directory_request(
    requests: &mpsc::SyncSender<ActionDirectoryRequest>,
    owner: &crate::wake::WakeHandle,
    request: ActionDirectoryRequest,
) -> Result<(), mpsc::TrySendError<ActionDirectoryRequest>> {
    requests.try_send(request)?;
    owner.notify_events();
    Ok(())
}

fn directory_unavailable() -> runtime_collab::ActionDirectoryResolution {
    runtime_collab::ActionDirectoryResolution::Unavailable
}

#[allow(
    refining_impl_trait,
    reason = "capnp generated server traits accept the concrete Promise type used by every existing RPC endpoint"
)]
impl runtime_collab::collab_capnp::action_directory::Server for ActionDirectoryServer {
    fn query(
        self: std::rc::Rc<Self>,
        params: runtime_collab::collab_capnp::action_directory::QueryParams,
        mut results: runtime_collab::collab_capnp::action_directory::QueryResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let query = match params.get().and_then(
            runtime_collab::collab_capnp::action_directory::query_params::Reader::get_query,
        ) {
            Ok(query) => {
                let text = |value: capnp::Result<capnp::text::Reader<'_>>| {
                    value
                        .and_then(|value| {
                            value
                                .to_str()
                                .map_err(|error| capnp::Error::failed(error.to_string()))
                        })
                        .map(str::to_owned)
                };
                match (
                    text(query.get_revision()),
                    query.get_cursor().map(<[u8]>::to_vec),
                    text(query.get_needle()),
                    text(query.get_parent_resource()),
                ) {
                    (Ok(revision), Ok(cursor), Ok(needle), Ok(parent_resource)) => {
                        Ok(runtime_collab::ActionDirectoryQuery {
                            peer_id: query.get_peer_id(),
                            revision,
                            cursor,
                            needle,
                            parent_resource,
                            max_results: query.get_max_results(),
                            max_bytes: query.get_max_bytes(),
                            max_work: query.get_max_work(),
                            max_depth: query.get_max_depth(),
                        })
                    }
                    (Err(error), ..)
                    | (_, Err(error), ..)
                    | (_, _, Err(error), ..)
                    | (_, _, _, Err(error)) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        let query = match query {
            Ok(query) => query,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let requests = self.requests.clone();
        let owner = self.owner.clone();
        capnp::capability::Promise::from_future(async move {
            let (reply, response) = tokio::sync::oneshot::channel();
            submit_directory_request(
                &requests,
                &owner,
                ActionDirectoryRequest::Query { query, reply },
            )
            .map_err(|_| {
                capnp::Error::failed("application action directory is busy or unavailable".into())
            })?;
            // Answered on the owner's next turn, however long its current one
            // runs (a heavy action or frame): a busy app is not a gone one. The
            // asker owns the deadline; when it stops waiting it cancels this
            // call, which drops this future. A stopped owner drops `reply`.
            let page = response.await.map_err(|_| {
                capnp::Error::failed("application action directory is unavailable".into())
            })?;
            runtime_collab::write_action_directory_page(results.get().init_page(), &page)
        })
    }

    fn resolve(
        self: std::rc::Rc<Self>,
        params: runtime_collab::collab_capnp::action_directory::ResolveParams,
        mut results: runtime_collab::collab_capnp::action_directory::ResolveResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let request = params.get().and_then(|request| {
            let text = |value: capnp::Result<capnp::text::Reader<'_>>| {
                value
                    .and_then(|value| {
                        value
                            .to_str()
                            .map_err(|error| capnp::Error::failed(error.to_string()))
                    })
                    .map(str::to_owned)
            };
            Ok((
                text(request.get_revision())?,
                text(request.get_resource())?,
                text(request.get_action())?,
            ))
        });
        let (revision, resource, action) = match request {
            Ok(request) => request,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let requests = self.requests.clone();
        let owner = self.owner.clone();
        capnp::capability::Promise::from_future(async move {
            let (reply, response) = tokio::sync::oneshot::channel();
            let reply_value = if submit_directory_request(
                &requests,
                &owner,
                ActionDirectoryRequest::Resolve {
                    revision,
                    resource,
                    action,
                    reply,
                },
            )
            .is_err()
            {
                directory_unavailable()
            } else {
                // As for a query: the asker's deadline, not one of ours.
                response.await.unwrap_or_else(|_| directory_unavailable())
            };
            runtime_collab::write_action_directory_resolution(
                results.get().init_resolution(),
                &reply_value,
            )
        })
    }
}

type TaskActionResult = Result<(u64, tilo_actions::InvocationUpdate), String>;

fn parse_task_update(id: u64, update: &str) -> TaskActionResult {
    serde_json::from_str(update)
        .map(|update| (id, update))
        .map_err(|error| error.to_string())
}

/// Project a stepped task reply onto the event fields: a committed step
/// carries its continuation; a pending or lost step asks the caller to
/// re-send the identical step.
fn task_step_event(
    reply: Result<runtime_collab::TaskStepReply, String>,
) -> (TaskActionResult, Option<Vec<u8>>, bool) {
    match reply {
        Ok(runtime_collab::TaskStepReply::Committed {
            update_json,
            continuation,
        }) => (
            parse_task_update(0, &update_json),
            Some(continuation),
            false,
        ),
        Ok(runtime_collab::TaskStepReply::Retry) => (
            Err("task step pending; retry the same step".into()),
            None,
            true,
        ),
        Err(error) => (Err(error), None, false),
    }
}

// Not `Send`: `route.channel` wraps a capnp-rpc handle (`Rc`-based,
// `!Send`), and this only ever runs inside the shovel thread's `LocalSet`.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
#[allow(
    clippy::too_many_lines,
    reason = "the request variants share one ordered dispatch boundary onto the SDK route"
)]
async fn dispatch_sdk_request(
    route: &mut SdkRoute,
    request: CollabRequest,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
) -> bool {
    if matches!(
        request,
        CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
    ) && wait_for_document_publication(route, &inbound_tx.state)
        .await
        .is_err()
    {
        return false;
    }
    match request {
        CollabRequest::Hello { .. } => {}
        CollabRequest::SignedOperation { envelope } => match postcard::to_stdvec(&*envelope) {
            Ok(bytes) => {
                if let Err(error) = route.channel.send(&bytes).await {
                    inbound_tx
                        .state
                        .publication_failed("signed document commit RPC", error, true);
                }
            }
            Err(error) => {
                inbound_tx
                    .state
                    .publication_failed("signed document encoding", error, true);
            }
        },
        CollabRequest::Document {
            mutation,
            authorization,
        } => {
            // The helper records every failure, including a policy denial that
            // leaves the identity connection usable, before returning its result.
            return send_document_mutation(route, signer, mutation, authorization, inbound_tx)
                .await
                .is_ok();
        }
        CollabRequest::OfferContent { hash, bytes } => {
            if let Err(error) = route.channel.offer_blob(&hash, &bytes).await {
                tracing::debug!(%error, %hash, "collab: offering record content failed");
            }
        }
        CollabRequest::WantContent { hash } => {
            if let Err(error) = route.channel.want_blob(&hash).await {
                tracing::debug!(%error, %hash, "collab: asking for record content failed");
            }
        }
        CollabRequest::RequestDocumentSnapshot => {
            if route.committed {
                return true;
            }
            if route.channel.request_document_snapshot().await.is_err() {
                inbound_tx
                    .state
                    .fail_closed("document snapshot request RPC");
            }
        }
        CollabRequest::Presence { key, payload } => {
            let Some(presence) = route.presence.as_ref() else {
                return true;
            };
            let key_bytes = postcard::to_stdvec(&key).unwrap_or_default();
            match payload {
                PresencePayload::Pointer { x, y } => {
                    let _ = presence.publish_pointer(&key_bytes, x, y).await;
                }
                PresencePayload::Caret { pos } => {
                    let _ = presence.publish_caret(&key_bytes, pos as u64).await;
                }
                PresencePayload::Selection { start, end } => {
                    let _ = presence
                        .publish_selection(&key_bytes, start as u64, end as u64)
                        .await;
                }
                PresencePayload::Typing { start, end } => {
                    let _ = presence
                        .publish_typing(&key_bytes, start as u64, end as u64)
                        .await;
                }
                PresencePayload::Arrived => {
                    let _ = presence.publish_arrived(&key_bytes).await;
                }
                PresencePayload::Left => {
                    let _ = presence.publish_left(&key_bytes).await;
                }
                // Shell chrome originates this authenticated visual cue
                // directly on the peer-presence route. Apps cannot turn it
                // into a viewport command through this generic SDK bridge.
                PresencePayload::Following { .. } => {}
                PresencePayload::Interaction(update) => {
                    if let Ok(json) = serde_json::to_string(&update) {
                        let _ = presence.publish_interaction(&key_bytes, &json).await;
                    }
                }
                PresencePayload::Status { text, automatic } => {
                    let _ = presence.publish_status(&key_bytes, &text, automatic).await;
                }
            }
        }
        CollabRequest::FetchScrollback {
            artifact,
            before_seq,
            max,
        } => {
            let (ops, decrypted_payloads, oldest_seq, exhausted) = match route
                .channel
                .fetch_scrollback_for(artifact.0.as_bytes(), before_seq, u32::from(max))
                .await
            {
                Ok(batch) => {
                    let mut payloads = Vec::new();
                    let decoded: Vec<SignedOp> = batch
                        .ops
                        .into_iter()
                        .filter_map(|bytes| postcard::from_bytes::<SignedOp>(&bytes).ok())
                        .collect();
                    let oldest_seq = decoded.iter().map(|op| op.seq).min();
                    let ops = decoded
                        .into_iter()
                        .filter_map(|op| match decrypt_document_payload(&op, &route.encryption, false) {
                            // History pages arrive newest first, so a
                            // transfer split across pages completes when its
                            // oldest segment arrives.
                            Ok(payload) => DocumentReassembly::complete(
                                &inbound_tx.reassembly.history,
                                &op,
                                payload,
                            )
                            .map(|payload| {
                                payloads.push(payload);
                                Box::new(op)
                            }),
                            Err(DecryptFailure::Later) => {
                                // Paged again once this device holds the key.
                                None
                            }
                            Err(DecryptFailure::Refused(error)) => {
                                tracing::warn!(%error, "collab: dropping undecryptable scrollback envelope");
                                None
                            }
                        })
                        .collect();
                    (ops, payloads, oldest_seq, batch.done)
                }
                Err(_) => (Vec::new(), Vec::new(), None, true),
            };
            let _ = inbound_tx
                .send_document(CollabEvent::ScrollbackBatch {
                    artifact,
                    ops,
                    decrypted_payloads,
                    oldest_seq,
                    exhausted,
                })
                .await;
        }
        CollabRequest::PublishActions { app_id, actions } => {
            if let Err(error) = route.channel.publish_actions(&app_id, &actions).await {
                tracing::warn!(%error, "collab: publishing resource/action surface failed; republishing");
                inbound_tx
                    .state
                    .action_surface_failed
                    .store(true, Ordering::Release);
                inbound_tx.state.notify_events();
            }
        }
        CollabRequest::ReportContinuation {
            app_id,
            schema_version,
            metadata,
        } => {
            if let Err(error) = route
                .channel
                .report_continuation(&app_id, schema_version, &metadata)
                .await
            {
                tracing::warn!(%error, "collab: reporting continuation metadata failed");
            }
        }
        CollabRequest::ClearContinuation { app_id } => {
            if let Err(error) = route.channel.clear_continuation(&app_id).await {
                tracing::warn!(%error, "collab: clearing continuation metadata failed");
            }
        }
        CollabRequest::ReportHandoff { app_id, message } => {
            if let Err(error) = route.channel.handoff(&app_id, &message).await {
                tracing::warn!(%error, "collab: reporting a handoff step failed");
            }
        }
        CollabRequest::ReportPlayback {
            app_id,
            reports_json,
        } => {
            if let Err(error) = route.channel.report_playback(&app_id, &reports_json).await {
                tracing::warn!(%error, "collab: reporting private playback handoff state failed");
            }
        }
        CollabRequest::NativeMediaCommand { request_id, .. } => {
            let _ = inbound_tx.send_durable(CollabEvent::NativeMediaCommandResult {
                request_id,
                result: Err("native media requires the bounded control lane".into()),
            });
        }
        CollabRequest::RealtimeCommand {
            request_id,
            app_id,
            command,
        } => {
            let result = serde_json::to_vec(&command)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    if bytes.len() > crate::realtime_continuity::MAX_REALTIME_WIRE_BYTES {
                        return Err("realtime command exceeds local protocol bound".into());
                    }
                    Ok(bytes)
                });
            let result = match result {
                Ok(bytes) => route
                    .channel
                    .realtime_command(&app_id, &bytes)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|reply| {
                        serde_json::from_slice::<crate::realtime_continuity::RealtimeReply>(&reply)
                            .map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            };
            let _ =
                inbound_tx.send_durable(CollabEvent::RealtimeCommandResult { request_id, result });
        }
        CollabRequest::NearbyTransferCommand {
            request_id,
            app_id,
            command,
        } => {
            let result = route
                .channel
                .nearby_transfer_command(&app_id, &command)
                .await
                .map_err(|error| error.to_string());
            let _ = inbound_tx
                .send_durable(CollabEvent::NearbyTransferCommandResult { request_id, result });
        }
        CollabRequest::LocalFileClipboard {
            request_id,
            command,
            on_behalf_of,
        } => {
            let result = route
                .channel
                .local_file_clipboard_command(&command, on_behalf_of.map(|id| *id.0.as_bytes()))
                .await
                .map_err(|error| error.to_string());
            let _ = inbound_tx
                .send_durable(CollabEvent::LocalFileClipboardResult { request_id, result });
        }
        CollabRequest::OpenWorkspaceControl {
            session_id,
            target_device,
        } => match route.channel.open_workspace_control(target_device).await {
            Ok(handle) => {
                let reader = handle.clone();
                let tx = inbound_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(event) = reader.read().await {
                        let terminal =
                            matches!(event, runtime_collab::WorkspaceControlEvent::Closed(_));
                        if !tx.send_durable(CollabEvent::WorkspaceControl { session_id, event })
                            || terminal
                        {
                            break;
                        }
                    }
                });
                route.workspace_controls.insert(session_id, handle);
            }
            Err(error) => {
                let _ = inbound_tx.send_durable(CollabEvent::WorkspaceControl {
                    session_id,
                    event: runtime_collab::WorkspaceControlEvent::Closed(
                        runtime_collab::WorkspaceControlCloseReason::Denied(error.to_string()),
                    ),
                });
            }
        },
        CollabRequest::WorkspaceControlInput { session_id, event } => {
            if let Some(handle) = route.workspace_controls.get(&session_id)
                && let Err(error) = handle.send_input(&event).await
            {
                let _ = inbound_tx.send_durable(CollabEvent::WorkspaceControl {
                    session_id,
                    event: runtime_collab::WorkspaceControlEvent::Closed(
                        runtime_collab::WorkspaceControlCloseReason::Denied(error.to_string()),
                    ),
                });
            }
        }
        CollabRequest::CloseWorkspaceControl { session_id } => {
            if let Some(handle) = route.workspace_controls.remove(&session_id) {
                let _ = handle.close().await;
            }
        }
        CollabRequest::CompleteInvocation { id, code, message } => {
            if let Err(error) = route.channel.complete_invocation(id, &code, &message).await {
                tracing::warn!(%error, id, "collab: completing resource/action invocation failed");
                inbound_tx
                    .state
                    .fail_closed("invocation completion outcome unknown");
            }
        }
        CollabRequest::UpdateInvocation { id, update } => match serde_json::to_string(&update) {
            Ok(json) => {
                if let Err(error) = route.channel.update_invocation(id, &json).await {
                    tracing::warn!(%error, id, "collab: updating action invocation failed");
                    // Broker-owned acceptance and rejected progress are not
                    // owner durability acknowledgments. A failed terminal may
                    // follow an effect and must stop subsequent outbound work.
                    if update.is_terminal() {
                        inbound_tx.state.fail_closed_with_rpc_error(
                            "invocation update outcome unknown",
                            Some((id, &error)),
                        );
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, id, "collab: encoding action update failed");
                inbound_tx.state.fail_closed("invocation update encoding");
            }
        },
        CollabRequest::RecordLocalEffect { address, update } => {
            match serde_json::to_string(&update) {
                Ok(json) => {
                    if let Err(error) = route
                        .channel
                        .record_local_effect(&address.resource, &address.action, &json)
                        .await
                    {
                        tracing::warn!(%error, "collab: recording local effect receipt failed");
                        inbound_tx
                            .state
                            .fail_closed("effect receipt outcome unknown");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "collab: encoding local effect receipt failed");
                    inbound_tx.state.fail_closed("effect receipt encoding");
                }
            }
        }
        CollabRequest::RemoteAction { message } => {
            let answer = remote_action_round_trip(&route.channel, message).await;
            let _ = inbound_tx.send_durable(CollabEvent::RemoteAction { message: answer });
        }
        #[cfg(unix)]
        CollabRequest::ManagedInvocationAttempt { attempt } => {
            use crate::host_worker::managed_sender::{AttemptFailure, RequestView};
            use managed_sender_transport::{Failure, Request};
            let result = match attempt.claim(&route.spec) {
                None => return true, // A clone cannot dispatch the same local attempt twice.
                Some(Err(error)) => Err(error),
                Some(Ok(view)) => {
                    let request = match view {
                        RequestView::Invoke {
                            invocation,
                            binding,
                            revision,
                            resource,
                            action,
                            argument,
                            confirmation_json,
                        } => Request::Invoke {
                            invocation,
                            binding,
                            revision,
                            resource,
                            action,
                            argument,
                            confirmation_json,
                        },
                        RequestView::Observe { host, invocation } => {
                            Request::Observe { host, invocation }
                        }
                        RequestView::Cancel { host, invocation } => {
                            Request::Cancel { host, invocation }
                        }
                    };
                    managed_sender_transport::send(route, request, attempt.deadline())
                        .await
                        .map_err(|error| match error {
                            Failure::Deadline => AttemptFailure::Deadline,
                            Failure::Unavailable => AttemptFailure::Unavailable,
                        })
                }
            };
            if let Some(result) = attempt.finish(result) {
                let _ =
                    inbound_tx.send_durable(CollabEvent::ManagedInvocationObservation { result });
            }
        }
        CollabRequest::QueryActionDirectory { request_id, query } => {
            let result = directory_client::query(route, &query).await;
            let _ =
                inbound_tx.send_durable(CollabEvent::ActionDirectoryPage { request_id, result });
        }
        CollabRequest::ResolvePublishedAction {
            request_id,
            peer_id,
            revision,
            resource,
            action,
        } => {
            let result =
                directory_client::resolve(route, peer_id, &revision, &resource, &action).await;
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedActionResolution { request_id, result });
        }
        CollabRequest::InvokeResolvedPublishedAction {
            request_id,
            peer_id,
            revision,
            resource,
            action,
            arguments,
            confirmation,
        } => {
            let result = directory_client::invoke(
                route,
                peer_id,
                &revision,
                &resource,
                &action,
                arguments,
                confirmation,
            )
            .await;
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::InvokePublishedAction {
            request_id,
            peer_id,
            resource,
            action,
            arguments,
            confirmation,
        } => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(arguments)
            );
            let confirmation = confirmation.to_string();
            let result = route
                .channel
                .invoke_published_action(
                    peer_id,
                    &resource,
                    &action,
                    Some(&argument),
                    &confirmation,
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::PublishedInvocationStatus { request_id, id } => {
            let result = route
                .channel
                .published_invocation_status(id)
                .await
                .map_err(|error| error.to_string())
                .and_then(|update| {
                    let update = update.ok_or_else(|| "unknown invocation id".to_string())?;
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::CancelPublishedInvocation { request_id, id } => {
            let result = route
                .channel
                .cancel_published_invocation(id)
                .await
                .map_err(|error| error.to_string())
                .and_then(|update| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::LaunchPublishedApplication {
            request_id,
            app_id,
            content_hash,
        } => {
            let result = route
                .channel
                .launch_published_application(&app_id, content_hash.as_deref())
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::LaunchInstalledApplication {
            request_id,
            app_id,
            package_root,
            on_behalf_of,
        } => {
            let result = route
                .channel
                .launch_installed_application_for(
                    &app_id,
                    None,
                    package_root.as_deref(),
                    on_behalf_of.map(|id| *id.0.as_bytes()),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::InstalledApplicationLaunchResult { request_id, result });
        }
        CollabRequest::SubscribeEnrolledTasks { request_id } => {
            let result = route
                .channel
                .subscribe_enrolled_tasks()
                .await
                .map_err(|error| error.to_string());
            let reply = match result {
                Ok(stream) => {
                    let tx = inbound_tx.clone();
                    tokio::task::spawn_local(async move {
                        loop {
                            match stream.read().await {
                                Ok(run) => {
                                    if !tx.send_durable(CollabEvent::EnrolledTask(run)) {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    let _ =
                                        tx.send_durable(CollabEvent::EnrolledTaskDisconnected {
                                            detail: error.to_string(),
                                        });
                                    break;
                                }
                            }
                        }
                    });
                    Ok(())
                }
                Err(error) => Err(error),
            };
            let _ = inbound_tx.send_durable(CollabEvent::EnrolledTaskReply {
                request_id,
                result: reply,
            });
        }
        CollabRequest::AcceptEnrolledTask {
            request_id,
            acceptance,
        } => {
            let result = route
                .channel
                .accept_enrolled_task(&acceptance)
                .await
                .map_err(|error| error.to_string());
            let _ = inbound_tx.send_durable(CollabEvent::EnrolledTaskReply { request_id, result });
        }
        CollabRequest::ReportEnrolledTask {
            request_id,
            acceptance,
            sequence,
            update,
        } => {
            let result = route
                .channel
                .report_enrolled_task(&acceptance, sequence, &update)
                .await
                .map_err(|error| error.to_string());
            let _ = inbound_tx.send_durable(CollabEvent::EnrolledTaskReply { request_id, result });
        }
        CollabRequest::ReportTaskRun {
            run_id,
            lease,
            update,
        } => {
            if let Err(error) = route.channel.report_task_run(run_id, &lease, &update).await {
                tracing::warn!(%error, run_id, "collab: reporting task run update failed");
            }
        }
        CollabRequest::InvokeTaskAction {
            request_id,
            invocation,
            run_id,
            lease,
            step,
            peer_id,
            resource,
            action,
            arguments,
        } => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(arguments)
            );
            let target = runtime_collab::TaskAction {
                peer_id,
                resource: &resource,
                action: &action,
                argument: Some(&argument),
            };
            let (result, continuation, retry_step) = match step {
                Some(step) => task_step_event(
                    route
                        .channel
                        .invoke_task_step(run_id, &lease, step, target)
                        .await
                        .map_err(|error| error.to_string()),
                ),
                None => (
                    route
                        .channel
                        .invoke_task_action(run_id, &lease, target, "", &invocation.0.to_string())
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|(id, update)| parse_task_update(id, &update)),
                    None,
                    false,
                ),
            };
            let _ = inbound_tx.send_durable(CollabEvent::TaskActionInvocationResult {
                request_id,
                result,
                continuation,
                retry_step,
            });
        }
        CollabRequest::TaskRunCancellation {
            request_id,
            run_id,
            lease,
        } => {
            let result = route
                .channel
                .task_run_cancelled(run_id, &lease)
                .await
                .map_err(|error| error.to_string());
            let _ =
                inbound_tx.send_durable(CollabEvent::TaskRunCancellation { request_id, result });
        }
    }
    true
}
/// Pass a single-frame document event through, or hold a segment until its
/// transfer completes and return the complete mutation exactly once.
fn complete_document_event(
    event: CollabEvent,
    reassembler: &std::sync::Mutex<collab_types::DocumentReassembler>,
) -> Option<CollabEvent> {
    let CollabEvent::Document {
        envelope,
        from_user_id,
        decrypted_payload,
    } = event
    else {
        return Some(event);
    };
    let decrypted_payload =
        DocumentReassembly::complete(reassembler, &envelope, decrypted_payload)?;
    Some(CollabEvent::Document {
        envelope,
        from_user_id,
        decrypted_payload,
    })
}

/// Shared per-route key state: the op stream opens with it, sends seal
/// with it, and key refreshes update it.
type SharedRouteKeys = std::sync::Arc<std::sync::Mutex<super::route_keys::RouteKeys>>;

/// Why an inbound document envelope was not delivered.
#[derive(Debug)]
enum UndeliveredDocument {
    /// No key for it yet; it was held for when keys arrive.
    Held,
    /// Refused: downgrade, wrong channel, tampering, or a malformed frame.
    Refused(collab_types::DocumentMutationError),
}

impl std::fmt::Display for UndeliveredDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held => f.write_str("held until this device has the channel's keys"),
            Self::Refused(error) => error.fmt(f),
        }
    }
}

/// Open a live envelope into a document event, or hold it on the route
/// while this device waits for the channel's keys.
fn signed_document_event(
    envelope: SignedOp,
    from_user_id: String,
    encryption: &SharedRouteKeys,
) -> Result<CollabEvent, UndeliveredDocument> {
    match decrypt_document_payload(&envelope, encryption, true) {
        Ok(decrypted_payload) => Ok(CollabEvent::Document {
            envelope: Box::new(envelope),
            from_user_id,
            decrypted_payload,
        }),
        Err(DecryptFailure::Later) => {
            if let Ok(mut keys) = encryption.lock() {
                keys.hold_inbound(envelope, from_user_id);
            }
            Err(UndeliveredDocument::Held)
        }
        Err(DecryptFailure::Refused(error)) => Err(UndeliveredDocument::Refused(error)),
    }
}

enum DecryptFailure {
    Later,
    Refused(collab_types::DocumentMutationError),
}

/// Open an envelope's typed payload. `live` envelopes on an encrypted
/// route must be ciphertext; history pages may still carry plaintext the
/// broker admitted before the channel was encrypted.
fn decrypt_document_payload(
    envelope: &SignedOp,
    encryption: &SharedRouteKeys,
    live: bool,
) -> Result<Option<Vec<u8>>, DecryptFailure> {
    let refused = |error| DecryptFailure::Refused(error);
    let opened = encryption
        .lock()
        .map_err(|_| refused(collab_types::DocumentMutationError::RouteMismatch))?
        .open(envelope, live);
    match opened {
        Ok(Some(plaintext)) => {
            let plaintext = plaintext.to_vec();
            collab_types::DocumentFrame::decode(&plaintext).map_err(refused)?;
            // The clear storage class hosts retained the op under must
            // describe what it actually contains.
            if envelope
                .encryption
                .and_then(|metadata| metadata.retention())
                .is_some_and(|retention| retention != document_frame_retention(&plaintext))
            {
                return Err(refused(collab_types::DocumentMutationError::RouteMismatch));
            }
            Ok(Some(plaintext))
        }
        Ok(None) => {
            collab_types::DocumentFrame::decode(&envelope.payload).map_err(refused)?;
            Ok(None)
        }
        Err(super::route_keys::Unopened::Later) => Err(DecryptFailure::Later),
        Err(super::route_keys::Unopened::Never) => {
            Err(refused(collab_types::DocumentMutationError::RouteMismatch))
        }
    }
}

/// Translate an SDK [`runtime_collab::PresenceEvent`] into a
/// `CollabEvent`. Extracted so the unit test can exercise it without a broker.
fn translate_presence_event(evt: runtime_collab::PresenceEvent) -> Option<CollabEvent> {
    // Cursor keeps the legacy `user_id + x + y` shape; every other
    // variant must carry a decodable semantic `PresenceKey`.
    match evt.payload {
        runtime_collab::PresencePayload::Cursor { x, y } => Some(CollabEvent::RemoteCursorMoved {
            user_id: evt.user_id,
            x,
            y,
        }),
        other => {
            // An undecodable scope cannot truthfully identify a target. Drop
            // it instead of inventing a tab and making malformed presence
            // look like a collaborator left some unrelated surface.
            let key: PresenceKey = postcard::from_bytes(&evt.key_bytes).ok()?;
            let payload = match other {
                runtime_collab::PresencePayload::Caret { pos } => PresencePayload::Caret {
                    // A wire-supplied offset that doesn't fit `usize` on this
                    // target can't address anything real; clamp rather than
                    // wrap it into an unrelated in-range position.
                    pos: usize::try_from(pos).unwrap_or(usize::MAX),
                },
                runtime_collab::PresencePayload::Selection { start, end } => {
                    PresencePayload::Selection {
                        start: usize::try_from(start).unwrap_or(usize::MAX),
                        end: usize::try_from(end).unwrap_or(usize::MAX),
                    }
                }
                runtime_collab::PresencePayload::Typing { start, end } => PresencePayload::Typing {
                    start: usize::try_from(start).unwrap_or(usize::MAX),
                    end: usize::try_from(end).unwrap_or(usize::MAX),
                },
                runtime_collab::PresencePayload::Following { target } => {
                    PresencePayload::Following { target }
                }
                runtime_collab::PresencePayload::InteractionJson(json) => {
                    let update: tilo_actions::SemanticInteraction =
                        serde_json::from_str(&json).ok()?;
                    update.validate().ok()?;
                    PresencePayload::Interaction(Box::new(update))
                }
                runtime_collab::PresencePayload::Status { text, automatic } => {
                    PresencePayload::Status { text, automatic }
                }
                runtime_collab::PresencePayload::Arrived => PresencePayload::Arrived,
                // `Cursor` is fully handled and returned by the outer match, so
                // it never reaches here; folding it in with `Left` keeps this
                // exhaustive without a panic on a branch that cannot run.
                runtime_collab::PresencePayload::Left
                | runtime_collab::PresencePayload::Cursor { .. } => PresencePayload::Left,
                // Keeps the coordinate space its key carries, where
                // `Cursor` has nowhere to put one.
                runtime_collab::PresencePayload::Pointer { x, y } => {
                    PresencePayload::Pointer { x, y }
                }
            };
            Some(CollabEvent::RemotePresenceUpdate {
                user_id: evt.user_id,
                key,
                payload,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    // The `Fake*` capnp-rpc mock servers below implement generated `Server`
    // traits by returning the concrete `Promise<T, capnp::Error>` capnp-rpc
    // expects, which is more specific than (though it does satisfy) the
    // trait's generic `impl Future` return -- the exact "refining" this
    // lint polices, and the rustc note on it says to allow when intended.
    // Rewriting every method's return type to the verbose `impl
    // Future<Output = ...>` spelling wouldn't change any behaviour, only
    // make capnp-generated-shaped code look less like the rest of the
    // capnp-rpc code it's mocking.
    #![allow(
        refining_impl_trait,
        reason = "mock capnp servers return the concrete Promise capnp-rpc expects"
    )]

    use super::*;

    /// The concrete broker transport's bridge state behind a test manager.
    fn broker_state(manager: &CollaborationManager) -> BridgeState {
        match &manager.backend {
            Backend::Transport(transport) => transport
                .as_any()
                .and_then(|any| any.downcast_ref::<BrokerTransport>())
                .expect("a broker transport")
                .state
                .clone(),
            Backend::Disabled(_) => panic!("a broker transport"),
        }
    }

    #[cfg(unix)]
    include!("invocation_custody_tests.rs");

    #[cfg(unix)]
    include!("directory_client_tests.rs");
    use crate::collaboration::{
        CollaborationState, HostDocumentCommit, SignedSendError, SigningIdentity,
    };

    fn plain_route_keys() -> SharedRouteKeys {
        std::sync::Arc::new(std::sync::Mutex::new(
            super::super::route_keys::RouteKeys::plain(),
        ))
    }

    fn group_route_keys(group: channel_crypto::ChannelGroup) -> SharedRouteKeys {
        let keys = plain_route_keys();
        keys.lock().unwrap().install_group(group);
        keys
    }

    use crate::time::{Duration, Instant};
    use std::cell::RefCell;
    #[cfg(unix)]
    use std::os::fd::IntoRawFd;
    use std::rc::Rc;

    use capnp::capability::Promise;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    #[cfg(unix)]
    use tokio::net::UnixStream as TokioUnixStream;
    #[cfg(unix)]
    use tokio::task::LocalSet as TokioLocalSet;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    use runtime_collab::PresencePayload as SdkPayload;
    use runtime_collab::raw::{
        action_catalogue_stream, channel_context, channel_handle, collab_service, context_stream,
        invocation_stream, presence_event, presence_handle, presence_payload, presence_stream,
        scrollback_batch, stream,
    };

    #[derive(Default)]
    struct Captures {
        /// How long after it is asked for the op stream hands out each op.
        op_stream_delay: Option<Duration>,
        /// The app and the records whose settings the channel hands out.
        settings: Option<(String, Arc<dyn settings_store::SettingsSource>)>,
        /// The app and the board whose live activity the channel hands out.
        live_activities: Option<(String, Arc<runtime_collab::LiveActivities>)>,
        #[cfg(unix)]
        directory: DirectoryCaptures,
        committed_history_fixture: Option<Vec<u8>>,
        committed_revision: Option<[u8; 32]>,
        invocation_fixture_enabled: bool,
        invocations: std::collections::VecDeque<runtime_collab::Invocation>,
        received_invocations: usize,
        invocation_execution: Option<Result<bool, String>>,
        committed_recheck_gate: Option<tokio::sync::oneshot::Receiver<()>>,
        committed_reads: Vec<runtime_collab::committed_document::Request>,
        media_commands: Vec<runtime_collab::native_media::Request>,
        media_active_fixture: bool,
        media_capture_active: bool,
        media_pending_consent: bool,
        media_joined: bool,
        media_stops: usize,
        media_gates: std::collections::VecDeque<tokio::sync::oneshot::Receiver<()>>,
        fail_document: bool,
        document_gates: std::collections::VecDeque<DocumentSendGate>,
        fail_terminal: bool,
        terminal_updates: usize,
        invocation_updates: Vec<(u64, tilo_actions::InvocationUpdate)>,
        joined: Vec<(String, Vec<u8>)>,
        joined_record_kinds: Vec<String>,
        signed_ops: Vec<Vec<u8>>,
        cursor_publishes: Vec<(f32, f32)>,
        pointer_publishes: Vec<(Vec<u8>, f32, f32)>,
        caret_publishes: Vec<(Vec<u8>, u64)>,
        selection_publishes: Vec<(Vec<u8>, u64, u64)>,
        typing_publishes: Vec<(Vec<u8>, u64, u64)>,
        arrived: Vec<Vec<u8>>,
        left: Vec<Vec<u8>>,
        scrollback_requests: Vec<(Vec<u8>, u64, u32)>,
        action_publications: Vec<(String, Vec<(String, String)>)>,
        /// Refuse this many action-surface publications, as a broker whose
        /// admission is momentarily unavailable does.
        refuse_action_publications: usize,
        refused_action_publications: usize,
        action_directories: Vec<String>,
        refuse_action_directories: usize,
        directory_attempts: Vec<std::time::Instant>,
        directory_ack: Option<Arc<tokio::sync::Notify>>,
        action_catalogue_fixture: Option<Vec<String>>,
        action_catalogue_subscriptions: usize,
        action_catalogue_reads: usize,
    }

    struct DocumentSendGate {
        entered: mpsc::Sender<()>,
        release: tokio::sync::oneshot::Receiver<Result<(), String>>,
    }

    /// Shared between the test-thread's tokio `LocalSet` and the caller.
    /// `Arc<Mutex<_>>` (not `Rc<RefCell<_>>`) because the broker
    /// struct crosses a `thread::spawn` boundary before it's driven
    /// through the RPC system.
    type SharedCaptures = std::sync::Arc<std::sync::Mutex<Captures>>;

    #[derive(Clone)]
    struct SeedPresence {
        user_id: String,
        key_bytes: Vec<u8>,
        payload: SeedPayload,
    }

    #[derive(Clone)]
    enum SeedPayload {
        Cursor { x: f32, y: f32 },
        Pointer { x: f32, y: f32 },
        Caret { pos: u64 },
        Selection { start: u64, end: u64 },
        Typing { start: u64, end: u64 },
        Arrived,
        Left,
    }

    #[derive(Clone)]
    struct SeedContext {
        collaborators: Vec<(String, String, u32)>,
        permissions: Vec<String>,
    }

    #[derive(Clone)]
    struct SeedScrollback {
        ops: Vec<Vec<u8>>,
        earliest_seq: u64,
        latest_seq: u64,
        done: bool,
    }

    struct FakeInvocationStream(SharedCaptures);
    impl invocation_stream::Server for FakeInvocationStream {
        fn read(
            self: Rc<Self>,
            _: invocation_stream::ReadParams,
            mut results: invocation_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            Promise::from_future(async move {
                loop {
                    let invocation = {
                        let mut captures = self.0.lock().unwrap();
                        let invocation = captures.invocations.pop_front();
                        if invocation.is_some() {
                            captures.received_invocations += 1;
                        }
                        invocation
                    };
                    if let Some(invocation) = invocation {
                        let mut wire = results.get();
                        wire.set_id(invocation.id);
                        wire.set_invocation_uuid(&invocation.invocation_uuid);
                        wire.set_resource(&invocation.resource);
                        wire.set_action(&invocation.action);
                        wire.set_argument(invocation.argument.as_deref().unwrap_or(""));
                        wire.set_has_argument(invocation.argument.is_some());
                        wire.set_actor(&invocation.actor);
                        wire.set_context_json(&invocation.context_json);
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
        }
    }

    struct FakePresenceStream {
        backlog: Rc<RefCell<Vec<SeedPresence>>>,
    }

    impl presence_stream::Server for FakePresenceStream {
        fn read(
            self: Rc<Self>,
            _params: presence_stream::ReadParams,
            mut results: presence_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let mut backlog = self.backlog.borrow_mut();
            if backlog.is_empty() {
                // Never resolve — clients that outrun the backlog
                // simply wait forever, which mirrors the real broker.
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            }
            let seed = backlog.remove(0);
            let mut ev: presence_event::Builder = results.get().init_event();
            ev.set_user_id(&seed.user_id);
            ev.set_key_bytes(&seed.key_bytes);
            let mut payload: presence_payload::Builder = ev.init_payload();
            match seed.payload {
                SeedPayload::Cursor { x, y } => {
                    let mut b = payload.init_cursor();
                    b.set_x(x);
                    b.set_y(y);
                }
                SeedPayload::Pointer { x, y } => {
                    let mut b = payload.init_pointer();
                    b.set_x(x);
                    b.set_y(y);
                }
                SeedPayload::Caret { pos } => {
                    payload.init_caret().set_pos(pos);
                }
                SeedPayload::Selection { start, end } => {
                    let mut b = payload.init_selection();
                    b.set_start(start);
                    b.set_end(end);
                }
                SeedPayload::Typing { start, end } => {
                    let mut b = payload.init_typing();
                    b.set_start(start);
                    b.set_end(end);
                }
                SeedPayload::Arrived => payload.set_arrived(()),
                SeedPayload::Left => payload.set_left(()),
            }
            Promise::ok(())
        }
    }

    struct FakePresence {
        captures: SharedCaptures,
        presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
    }

    impl presence_handle::Server for FakePresence {
        fn publish_cursor(
            self: Rc<Self>,
            params: presence_handle::PublishCursorParams,
            _results: presence_handle::PublishCursorResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            self.captures
                .lock()
                .unwrap()
                .cursor_publishes
                .push((r.get_x(), r.get_y()));
            Promise::ok(())
        }

        fn publish_pointer(
            self: Rc<Self>,
            params: presence_handle::PublishPointerParams,
            _results: presence_handle::PublishPointerResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().pointer_publishes.push((
                k.to_vec(),
                r.get_x(),
                r.get_y(),
            ));
            Promise::ok(())
        }

        fn publish_caret(
            self: Rc<Self>,
            params: presence_handle::PublishCaretParams,
            _results: presence_handle::PublishCaretResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures
                .lock()
                .unwrap()
                .caret_publishes
                .push((k.to_vec(), r.get_pos()));
            Promise::ok(())
        }

        fn publish_selection(
            self: Rc<Self>,
            params: presence_handle::PublishSelectionParams,
            _results: presence_handle::PublishSelectionResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().selection_publishes.push((
                k.to_vec(),
                r.get_start(),
                r.get_end(),
            ));
            Promise::ok(())
        }

        fn publish_typing(
            self: Rc<Self>,
            params: presence_handle::PublishTypingParams,
            _results: presence_handle::PublishTypingResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().typing_publishes.push((
                k.to_vec(),
                r.get_start(),
                r.get_end(),
            ));
            Promise::ok(())
        }

        fn publish_arrived(
            self: Rc<Self>,
            params: presence_handle::PublishArrivedParams,
            _results: presence_handle::PublishArrivedResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().arrived.push(k.to_vec());
            Promise::ok(())
        }

        fn publish_left(
            self: Rc<Self>,
            params: presence_handle::PublishLeftParams,
            _results: presence_handle::PublishLeftResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().left.push(k.to_vec());
            Promise::ok(())
        }

        fn subscribe_all(
            self: Rc<Self>,
            _params: presence_handle::SubscribeAllParams,
            mut results: presence_handle::SubscribeAllResults,
        ) -> Promise<(), capnp::Error> {
            let client: presence_stream::Client = capnp_rpc::new_client(FakePresenceStream {
                backlog: self.presence_backlog.clone(),
            });
            results.get().set_stream(client);
            Promise::ok(())
        }
    }

    /// `(op bytes, author)` pairs captured by a fake op/stream mock.
    type OpBacklog = Rc<RefCell<Vec<(Vec<u8>, String)>>>;

    struct FakeOpStream {
        backlog: OpBacklog,
        captures: SharedCaptures,
    }

    impl stream::Server for FakeOpStream {
        fn read(
            self: Rc<Self>,
            _params: stream::ReadParams,
            mut results: stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let mut backlog = self.backlog.borrow_mut();
            if backlog.is_empty() {
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            }
            let (op, from) = backlog.remove(0);
            let delay = self.captures.lock().unwrap().op_stream_delay;
            Promise::from_future(async move {
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                let mut r = results.get();
                r.set_op(&op);
                r.set_from_user_id(&from);
                Ok(())
            })
        }
    }

    struct FakeActionCatalogueStream {
        backlog: RefCell<std::collections::VecDeque<String>>,
        captures: SharedCaptures,
    }

    impl action_catalogue_stream::Server for FakeActionCatalogueStream {
        fn read(
            self: Rc<Self>,
            _params: action_catalogue_stream::ReadParams,
            mut results: action_catalogue_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let Some(initial) = self.backlog.borrow_mut().pop_front() else {
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            };
            self.captures.lock().unwrap().action_catalogue_reads += 1;
            results.get().set_catalogue_json(&initial);
            Promise::ok(())
        }
    }

    struct FakeContextStream {
        backlog: Rc<RefCell<Vec<SeedContext>>>,
    }

    impl context_stream::Server for FakeContextStream {
        fn read(
            self: Rc<Self>,
            _params: context_stream::ReadParams,
            mut results: context_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let mut backlog = self.backlog.borrow_mut();
            if backlog.is_empty() {
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            }
            let seed = backlog.remove(0);
            let mut ctx: channel_context::Builder = results.get().init_ctx();
            {
                let mut collabs = ctx.reborrow().init_collaborators(
                    u32::try_from(seed.collaborators.len()).unwrap_or(u32::MAX),
                );
                for (i, (uid, name, color)) in seed.collaborators.iter().enumerate() {
                    let mut c = collabs.reborrow().get(u32::try_from(i).unwrap_or(u32::MAX));
                    c.set_user_id(uid);
                    c.set_display_name(name);
                    c.set_color(*color);
                }
            }
            {
                let mut perms =
                    ctx.init_permissions(u32::try_from(seed.permissions.len()).unwrap_or(u32::MAX));
                for (i, p) in seed.permissions.iter().enumerate() {
                    perms.set(u32::try_from(i).unwrap_or(u32::MAX), p.as_str());
                }
            }
            Promise::ok(())
        }
    }

    struct FakeChannel {
        captures: SharedCaptures,
        op_backlog: OpBacklog,
        presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
        context_backlog: Rc<RefCell<Vec<SeedContext>>>,
        scrollback_response: Rc<RefCell<Option<SeedScrollback>>>,
    }

    impl channel_handle::Server for FakeChannel {
        fn invocation_cancellation(
            self: Rc<Self>,
            _: channel_handle::InvocationCancellationParams,
            mut results: channel_handle::InvocationCancellationResults,
        ) -> Promise<(), capnp::Error> {
            match self.captures.lock().unwrap().invocation_execution.as_ref() {
                Some(Ok(cancelled)) => {
                    results.get().set_cancelled(*cancelled);
                    Promise::ok(())
                }
                Some(Err(error)) => Promise::err(capnp::Error::failed(error.clone())),
                None => Promise::err(capnp::Error::unimplemented(
                    "fixture has no invocation".into(),
                )),
            }
        }

        fn subscribe_invocations(
            self: Rc<Self>,
            params: channel_handle::SubscribeInvocationsParams,
            mut results: channel_handle::SubscribeInvocationsResults,
        ) -> Promise<(), capnp::Error> {
            if !self.captures.lock().unwrap().invocation_fixture_enabled {
                return Promise::err(capnp::Error::unimplemented(
                    "fixture has no invocation stream".into(),
                ));
            }
            assert!(params.get().unwrap().get_stable_invocation_ids());
            let stream: invocation_stream::Client =
                capnp_rpc::new_client(FakeInvocationStream(self.captures.clone()));
            results.get().set_stream(stream);
            Promise::ok(())
        }

        fn settings(
            self: Rc<Self>,
            _params: channel_handle::SettingsParams,
            mut results: channel_handle::SettingsResults,
        ) -> Promise<(), capnp::Error> {
            let Some((app, source)) = self.captures.lock().unwrap().settings.clone() else {
                return Promise::err(capnp::Error::unimplemented("no settings".into()));
            };
            let settings: runtime_collab::collab_capnp::app_settings::Client =
                capnp_rpc::new_client(runtime_collab::settings::host::AppSettingsServer::new(
                    app,
                    source,
                    Arc::new(|| Ok(())),
                ));
            results.get().set_settings(settings);
            Promise::ok(())
        }

        fn live_activity(
            self: Rc<Self>,
            _params: channel_handle::LiveActivityParams,
            mut results: channel_handle::LiveActivityResults,
        ) -> Promise<(), capnp::Error> {
            let Some((app, board)) = self.captures.lock().unwrap().live_activities.clone() else {
                return Promise::err(capnp::Error::unimplemented("no live activities".into()));
            };
            let activity: runtime_collab::collab_capnp::live_activity::Client =
                capnp_rpc::new_client(runtime_collab::live_activity::LiveActivityServer::new(
                    app,
                    board,
                    Arc::new(|| Ok(())),
                ));
            results.get().set_activity(activity);
            Promise::ok(())
        }

        fn publish_actions(
            self: Rc<Self>,
            params: channel_handle::PublishActionsParams,
            _results: channel_handle::PublishActionsResults,
        ) -> Promise<(), capnp::Error> {
            let request = params.get().unwrap();
            let app_id = request.get_app_id().unwrap().to_str().unwrap().to_owned();
            let actions = request
                .get_actions()
                .unwrap()
                .iter()
                .map(|action| {
                    (
                        action
                            .get_resource_label()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                        action.get_action().unwrap().to_str().unwrap().to_owned(),
                    )
                })
                .collect();
            let mut captures = self.captures.lock().unwrap();
            if captures.refuse_action_publications > 0 {
                captures.refuse_action_publications -= 1;
                captures.refused_action_publications += 1;
                return Promise::err(capnp::Error::failed(
                    "current host worker authority unavailable".into(),
                ));
            }
            captures.action_publications.push((app_id, actions));
            Promise::ok(())
        }

        fn publish_action_directory(
            self: Rc<Self>,
            params: channel_handle::PublishActionDirectoryParams,
            _results: channel_handle::PublishActionDirectoryResults,
        ) -> Promise<(), capnp::Error> {
            let request = params.get().unwrap();
            let mut captures = self.captures.lock().unwrap();
            captures.directory_attempts.push(std::time::Instant::now());
            if captures.refuse_action_directories > 0 {
                captures.refuse_action_directories -= 1;
                return Promise::err(capnp::Error::failed("fixture directory refusal".into()));
            }
            captures
                .action_directories
                .push(request.get_app_id().unwrap().to_str().unwrap().to_owned());
            let ack = captures.directory_ack.clone();
            drop(captures);
            Promise::from_future(async move {
                if let Some(ack) = ack {
                    ack.notified().await;
                }
                Ok(())
            })
        }

        #[cfg(unix)]
        directory_fake_methods!();

        fn committed_document(
            self: Rc<Self>,
            params: channel_handle::CommittedDocumentParams,
            mut results: channel_handle::CommittedDocumentResults,
        ) -> Promise<(), capnp::Error> {
            use runtime_collab::committed_document::{Reply, Request};
            let result = (|| {
                let request = Request::decode(params.get()?.get_request()?)
                    .map_err(|error| capnp::Error::failed(error.into()))?;
                let mut captures = self.captures.lock().unwrap();
                captures.committed_reads.push(request.clone());
                let current_revision = captures.committed_revision.unwrap_or([7; 32]);
                let reply = match captures.committed_history_fixture.as_ref() {
                    None => Reply::Legacy,
                    Some(bytes) => match request {
                        Request::Open {
                            known_revision: Some(revision),
                        } if revision == current_revision => Reply::Unchanged { revision },
                        Request::Open { .. } => Reply::Chunk {
                            capture: 1,
                            revision: current_revision,
                            total: bytes.len() as u32,
                            offset: 0,
                            bytes: bytes.clone(),
                            token: [3; 16],
                        },
                        Request::Cancel { capture: 1, token } if token == [3; 16] => {
                            Reply::Cancelled
                        }
                        _ => {
                            return Err(capnp::Error::failed(
                                "fixture read continuation refused".into(),
                            ));
                        }
                    },
                };
                let gate = if matches!(reply, Reply::Unchanged { .. }) {
                    captures.committed_recheck_gate.take()
                } else {
                    None
                };
                results.get().set_reply(
                    &reply
                        .encode()
                        .map_err(|error| capnp::Error::failed(error.into()))?,
                );
                Ok(gate)
            })();
            Promise::from_future(async move {
                if let Some(gate) = result? {
                    gate.await
                        .map_err(|_| capnp::Error::failed("fixture recheck gate dropped".into()))?;
                }
                Ok(())
            })
        }

        fn native_media_command(
            self: Rc<Self>,
            params: channel_handle::NativeMediaCommandParams,
            mut results: channel_handle::NativeMediaCommandResults,
        ) -> Promise<(), capnp::Error> {
            let request = match params
                .get()
                .and_then(|params| params.get_command_json())
                .and_then(|bytes| {
                    runtime_collab::native_media::Request::decode(bytes)
                        .map_err(|e| capnp::Error::failed(e.into()))
                }) {
                Ok(request) => request,
                Err(error) => return Promise::err(error),
            };
            assert!(request.dispatch_valid());
            let gate = {
                let mut captures = self.captures.lock().unwrap();
                if captures.media_active_fixture {
                    use runtime_collab::native_media::Command;
                    match &request.command {
                        Command::SetMuted { muted: true } | Command::Leave => {
                            captures.media_capture_active = false;
                            captures.media_pending_consent = false;
                            captures.media_stops += 1;
                            if matches!(request.command, Command::Leave) {
                                captures.media_joined = false;
                            }
                        }
                        Command::SetMuted { muted: false }
                            if !captures.media_capture_active && captures.media_joined =>
                        {
                            captures.media_pending_consent = true
                        }
                        _ => {}
                    }
                }
                captures.media_commands.push(request);
                captures.media_gates.pop_front()
            };
            Promise::from_future(async move {
                if let Some(gate) = gate {
                    gate.await
                        .map_err(|_| capnp::Error::failed("native media gate dropped".into()))?;
                }
                results.get().set_reply_json(
                    &runtime_collab::native_media::Snapshot::unavailable()
                        .encode()
                        .unwrap(),
                );
                Ok(())
            })
        }

        fn update_invocation(
            self: Rc<Self>,
            params: channel_handle::UpdateInvocationParams,
            _results: channel_handle::UpdateInvocationResults,
        ) -> Promise<(), capnp::Error> {
            let mut captures = self.captures.lock().unwrap();
            if captures.invocation_fixture_enabled {
                let request = params.get().unwrap();
                let update =
                    serde_json::from_str(request.get_update_json().unwrap().to_str().unwrap())
                        .unwrap();
                captures.invocation_updates.push((request.get_id(), update));
            }
            captures.terminal_updates += 1;
            if captures.fail_terminal {
                return Promise::err(capnp::Error::failed(
                    "effect receipt barrier failed; outcome unknown".into(),
                ));
            }
            Promise::ok(())
        }

        fn send(
            self: Rc<Self>,
            params: channel_handle::SendParams,
            _results: channel_handle::SendResults,
        ) -> Promise<(), capnp::Error> {
            let op = match params
                .get()
                .and_then(channel_handle::send_params::Reader::get_op)
            {
                Ok(o) => o.to_vec(),
                Err(e) => return Promise::err(e),
            };
            let mut captures = self.captures.lock().unwrap();
            if captures.fail_document {
                return Promise::err(capnp::Error::failed("owner durable barrier failed".into()));
            }
            captures.signed_ops.push(op);
            if let Some(gate) = captures.document_gates.pop_front() {
                gate.entered.send(()).unwrap();
                return Promise::from_future(async move {
                    gate.release
                        .await
                        .map_err(|_| capnp::Error::failed("document gate dropped".into()))?
                        .map_err(capnp::Error::failed)
                });
            }
            Promise::ok(())
        }

        fn subscribe(
            self: Rc<Self>,
            _params: channel_handle::SubscribeParams,
            mut results: channel_handle::SubscribeResults,
        ) -> Promise<(), capnp::Error> {
            let client: stream::Client = capnp_rpc::new_client(FakeOpStream {
                backlog: self.op_backlog.clone(),
                captures: self.captures.clone(),
            });
            results.get().set_stream(client);
            Promise::ok(())
        }

        fn presence(
            self: Rc<Self>,
            _params: channel_handle::PresenceParams,
            mut results: channel_handle::PresenceResults,
        ) -> Promise<(), capnp::Error> {
            let client: presence_handle::Client = capnp_rpc::new_client(FakePresence {
                captures: self.captures.clone(),
                presence_backlog: self.presence_backlog.clone(),
            });
            results.get().set_handle(client);
            Promise::ok(())
        }

        fn retired_unsigned_snapshot(
            self: Rc<Self>,
            _params: channel_handle::RetiredUnsignedSnapshotParams,
            _results: channel_handle::RetiredUnsignedSnapshotResults,
        ) -> Promise<(), capnp::Error> {
            Promise::ok(())
        }

        fn retired_unsigned_incremental(
            self: Rc<Self>,
            _params: channel_handle::RetiredUnsignedIncrementalParams,
            _results: channel_handle::RetiredUnsignedIncrementalResults,
        ) -> Promise<(), capnp::Error> {
            Promise::ok(())
        }

        fn subscribe_context(
            self: Rc<Self>,
            _params: channel_handle::SubscribeContextParams,
            mut results: channel_handle::SubscribeContextResults,
        ) -> Promise<(), capnp::Error> {
            let client: context_stream::Client = capnp_rpc::new_client(FakeContextStream {
                backlog: self.context_backlog.clone(),
            });
            results.get().set_stream(client);
            Promise::ok(())
        }

        fn subscribe_action_catalogue(
            self: Rc<Self>,
            _params: channel_handle::SubscribeActionCatalogueParams,
            mut results: channel_handle::SubscribeActionCatalogueResults,
        ) -> Promise<(), capnp::Error> {
            let snapshots = {
                let mut captures = self.captures.lock().unwrap();
                captures.action_catalogue_subscriptions += 1;
                captures
                    .action_catalogue_fixture
                    .clone()
                    .unwrap_or_else(|| {
                        vec![serde_json::json!({"version": 1, "surfaces": []}).to_string()]
                    })
            };
            let client: action_catalogue_stream::Client =
                capnp_rpc::new_client(FakeActionCatalogueStream {
                    backlog: RefCell::new(snapshots.into()),
                    captures: self.captures.clone(),
                });
            results.get().set_stream(client);
            Promise::ok(())
        }

        fn fetch_scrollback_for(
            self: Rc<Self>,
            params: channel_handle::FetchScrollbackForParams,
            mut results: channel_handle::FetchScrollbackForResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(p) = params.get() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().scrollback_requests.push((
                p.get_artifact_id()
                    .map_or_else(|_| Vec::new(), <[u8]>::to_vec),
                p.get_before_seq(),
                p.get_max(),
            ));
            let seed = self
                .scrollback_response
                .borrow()
                .clone()
                .unwrap_or(SeedScrollback {
                    ops: Vec::new(),
                    earliest_seq: 0,
                    latest_seq: 0,
                    done: true,
                });
            let mut batch: scrollback_batch::Builder = results.get().init_batch();
            {
                let mut ops = batch
                    .reborrow()
                    .init_ops(u32::try_from(seed.ops.len()).unwrap_or(u32::MAX));
                for (i, op) in seed.ops.iter().enumerate() {
                    ops.set(u32::try_from(i).unwrap_or(u32::MAX), op);
                }
            }
            batch.set_earliest_seq(seed.earliest_seq);
            batch.set_latest_seq(seed.latest_seq);
            batch.set_done(seed.done);
            Promise::ok(())
        }
    }

    struct FakeBroker {
        captures: SharedCaptures,
        op_backlog: Vec<(Vec<u8>, String)>,
        presence_backlog: Vec<SeedPresence>,
        context_backlog: Vec<SeedContext>,
        scrollback_response: Option<SeedScrollback>,
    }

    impl collab_service::Server for FakeBroker {
        fn join_channel(
            self: Rc<Self>,
            params: collab_service::JoinChannelParams,
            mut results: collab_service::JoinChannelResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(reader) = params.get() else {
                return Promise::ok(());
            };
            let channel_id = reader
                .get_channel_id()
                .and_then(|t| {
                    t.to_str()
                        .map(str::to_owned)
                        .map_err(|e| capnp::Error::failed(format!("bad utf8: {e}")))
                })
                .unwrap_or_default();
            let identity = reader
                .get_identity()
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            let record_kind = reader
                .get_record_kind()
                .ok()
                .and_then(|kind| kind.to_str().ok().map(str::to_owned))
                .unwrap_or_default();
            let mut captures = self.captures.lock().unwrap();
            captures.joined.push((channel_id, identity));
            captures.joined_record_kinds.push(record_kind);
            drop(captures);

            let chan = FakeChannel {
                captures: self.captures.clone(),
                op_backlog: Rc::new(RefCell::new(self.op_backlog.clone())),
                presence_backlog: Rc::new(RefCell::new(self.presence_backlog.clone())),
                context_backlog: Rc::new(RefCell::new(self.context_backlog.clone())),
                scrollback_response: Rc::new(RefCell::new(self.scrollback_response.clone())),
            };
            let client: channel_handle::Client = capnp_rpc::new_client(chan);
            results.get().set_handle(client);
            Promise::ok(())
        }
    }

    fn host_identity_fixture(
        certificate: Option<Vec<u8>>,
        replies: Vec<runtime_identity::wire::Response>,
    ) -> (tokio::io::DuplexStream, Arc<std::sync::Mutex<Vec<u8>>>) {
        host_identity_fixture_with_keys(certificate, replies, None)
    }

    /// A scripted identity service. `ChannelKeys` requests arrive on the
    /// endpoint's own refresh cadence, so they are answered from `keys`
    /// (or as unavailable) without consuming the scripted `replies`.
    fn host_identity_fixture_with_keys(
        certificate: Option<Vec<u8>>,
        replies: Vec<runtime_identity::wire::Response>,
        keys: Option<(u64, Vec<u8>)>,
    ) -> (tokio::io::DuplexStream, Arc<std::sync::Mutex<Vec<u8>>>) {
        use runtime_identity::wire::Response;
        let mut responses = vec![Response::Metadata {
            display_name: "Host fixture".into(),
            avatar_color: [1, 2, 3],
            group_public_key: [4; 32],
            signing_public_key: [5; 32],
            device_certificate: certificate,
            threshold: 1,
            total_shares: 1,
            share_index: 1,
        }];
        responses.extend(replies);
        let output = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = output.clone();
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        tokio::task::spawn_local(async move {
            let framing = runtime_identity::wire::FRAMING;
            let mut responses = responses.into_iter();
            loop {
                let Ok(Some(body)) = framing.read_async(&mut server).await else {
                    return;
                };
                captured
                    .lock()
                    .unwrap()
                    .extend_from_slice(&framing.encode(&body).unwrap());
                let asks_keys = matches!(
                    postcard::from_bytes::<runtime_identity::wire::Request>(&body),
                    Ok(runtime_identity::wire::Request::ChannelKeys { .. })
                );
                let response = if asks_keys {
                    match &keys {
                        Some((generation, keys)) => Response::ChannelKeys {
                            access: runtime_identity::wire::ChannelAccess::Ready,
                            generation: *generation,
                            keys: keys.clone(),
                        },
                        None => Response::ChannelKeys {
                            access: runtime_identity::wire::ChannelAccess::Unavailable,
                            generation: 0,
                            keys: Vec::new(),
                        },
                    }
                } else {
                    let Some(response) = responses.next() else {
                        return;
                    };
                    response
                };
                if framing
                    .write_value_async(&mut server, &response)
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        (client, output)
    }

    fn host_identity_requests(bytes: &[u8]) -> Vec<runtime_identity::wire::Request> {
        let mut remaining = bytes;
        let mut requests = Vec::new();
        while !remaining.is_empty() {
            if remaining.len() < 4 {
                break;
            }
            let len = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
            if remaining.len() < 4 + len {
                break;
            }
            requests.push(postcard::from_bytes(&remaining[4..4 + len]).unwrap());
            remaining = &remaining[4 + len..];
        }
        requests
    }

    #[test]
    fn host_connector_handoff_fails_closed_without_fallback() {
        // Case `host_connector_failure_is_consumed_once_without_inherited_fallback`.
        {
            let calls = Arc::new(AtomicUsize::new(0));
            let seen = calls.clone();
            let cluster_id = Uuid::new_v4();
            let expected = RouteSpec {
                app_id: "so.tilo.notes".into(),
                cluster_id,
                record: Some("note".into()),
            };
            let connector = HostSessionConnector::new(move |route| async move {
                seen.fetch_add(1, Ordering::SeqCst);
                assert_eq!(route, expected);
                assert_eq!(thread::current().name(), Some("ui-collab-shovel"));
                assert!(tokio::runtime::Handle::try_current().is_ok());
                // A !Send value across await proves this future stays on LocalSet.
                let local = Rc::new(());
                tokio::task::yield_now().await;
                drop(local);
                Err::<(tokio::io::DuplexStream, tokio::io::DuplexStream), _>(
                    runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "fixture host refused its grant",
                    )),
                )
            });
            let result = CollaborationManager::for_route_host_blocking(
                connector,
                "so.tilo.notes",
                cluster_id,
                Some("note".into()),
                None,
            );
            match result {
                Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                    assert_eq!(error.to_string(), "fixture host refused its grant");
                }
                _ => panic!("host refusal must reach caller"),
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }

        // Case `host_connector_without_certificate_cannot_use_harness_identity`.
        {
            let connector = HostSessionConnector::new(move |_| async move {
                let (identity, _) = host_identity_fixture(None, vec![]);
                let (stream, _peer) = tokio::io::duplex(4096);
                Ok((stream, identity))
            });
            let result = CollaborationManager::for_route_host_blocking(
                connector,
                "so.tilo.notes",
                Uuid::new_v4(),
                None,
                None,
            );
            match result {
                Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                    assert_eq!(
                        error.to_string(),
                        "workspace identity has no certified device"
                    );
                }
                _ => panic!("missing certificate must refuse before joining"),
            }
        }

        // Case `host_connector_join_proof_refusal_reaches_caller`.
        {
            let connector = HostSessionConnector::new(move |_| async move {
                let (identity, _) = host_identity_fixture(
                    Some(b"fixture".to_vec()),
                    vec![runtime_identity::wire::Response::Denied(
                        "fixture device revoked".into(),
                    )],
                );
                let (stream, _peer) = tokio::io::duplex(4096);
                Ok((stream, identity))
            });
            let result = CollaborationManager::for_route_host_blocking(
                connector,
                "so.tilo.notes",
                Uuid::new_v4(),
                None,
                None,
            );
            match result {
                Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                    assert_eq!(error.to_string(), "fixture device revoked");
                }
                _ => panic!("proof refusal must not become an unsigned join"),
            }
        }

        // Case `host_connector_uses_certified_presentation_and_broker_document_policy`.
        {
            let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
            let broker_captures = captures.clone();
            let (requests_tx, requests_rx) = mpsc::sync_channel(1);
            let cluster_id = Uuid::new_v4();
            let channel =
                collab_types::document_channel_id("so.tilo.notes", &cluster_id.to_string(), "note");
            let keys = channel_crypto::ChannelGroup::create(
                channel,
                channel_crypto::MemberKey::generate("device").unwrap(),
            )
            .unwrap()
            .content_keys()
            .unwrap()
            .to_bytes()
            .unwrap()
            .to_vec();
            let connector = HostSessionConnector::new(move |_| async move {
                let (identity, requests) = host_identity_fixture_with_keys(
                    Some(b"fixture certificate".to_vec()),
                    vec![
                        runtime_identity::wire::Response::Signature(vec![7; 64]),
                        runtime_identity::wire::Response::Denied("fixture role revoked".into()),
                        runtime_identity::wire::Response::Signature(vec![8; 64]),
                    ],
                    Some((1, keys)),
                );
                requests_tx.send(requests).unwrap();
                let broker = FakeBroker {
                    captures: broker_captures,
                    op_backlog: vec![],
                    presence_backlog: vec![],
                    context_backlog: vec![],
                    scrollback_response: None,
                };
                let (client, server) = tokio::io::duplex(64 * 1024);
                let broker_client: collab_service::Client = capnp_rpc::new_client(broker);
                let (r, w) = tokio::io::split(server);
                let network = Box::new(twoparty::VatNetwork::new(
                    r.compat(),
                    w.compat_write(),
                    rpc_twoparty_capnp::Side::Server,
                    capnp::message::ReaderOptions::default(),
                ));
                let rpc = RpcSystem::new(network, Some(broker_client.client));
                tokio::task::spawn_local(async move {
                    let _ = rpc.await;
                });
                Ok((client, identity))
            });
            let mut manager = CollaborationManager::for_route_host_blocking(
                connector,
                "so.tilo.notes",
                cluster_id,
                Some("note".into()),
                None,
            )
            .unwrap();
            // Returning a manager must include the existing Hello/primary barrier.
            assert!(manager.is_connected());
            assert!(manager.primary_route.current(true).is_some());
            assert_eq!(manager.state(), CollaborationState::Ready);
            assert_eq!(manager.take_state_change(), Some(CollaborationState::Ready));
            assert_eq!(manager.take_state_change(), None);
            let joined = captures.lock().unwrap().joined.clone();
            assert_eq!(joined.len(), 1);
            assert_eq!(
                joined[0].0,
                channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
            );
            let presentation = runtime_collab::CertifiedJoinIdentity::decode(&joined[0].1)
                .unwrap()
                .unwrap();
            assert_eq!(presentation.workspace_id, cluster_id.to_string());
            assert_eq!(presentation.device_certificate, b"fixture certificate");
            assert_eq!(presentation.proof, vec![7; 64]);
            let requests = tilo_test_env::recv("the host identity requests", &requests_rx);
            manager.send(CollabRequest::document_incremental(
                b"fixture document change".to_vec(),
            ));
            let is_op = |request: &runtime_identity::wire::Request| {
                matches!(
                    request,
                    runtime_identity::wire::Request::CollaborationOp { .. }
                )
            };
            tilo_test_env::wait_until(
                "`host_identity_requests(&requests.lock().unwrap()).iter().any(is_op)`",
                || {
                    host_identity_requests(&requests.lock().unwrap())
                        .iter()
                        .any(is_op)
                },
            );
            let requests = host_identity_requests(&requests.lock().unwrap());
            assert!(
                matches!(&requests[1], runtime_identity::wire::Request::CollaborationJoin { workspace_id } if workspace_id == &cluster_id.to_string())
            );
            assert!(
                matches!(&requests[2], runtime_identity::wire::Request::ChannelKeys { app_id, record_id, .. } if app_id == "so.tilo.notes" && record_id == "note"),
                "a joined route asks for its channel keys before its first edit"
            );
            let op = requests.iter().find(|request| is_op(request)).unwrap();
            match op {
                runtime_identity::wire::Request::CollaborationOp {
                    workspace_id,
                    app_id,
                    record_id,
                    unsigned_op,
                } => {
                    assert_eq!(workspace_id, &cluster_id.to_string());
                    assert_eq!(app_id, "so.tilo.notes");
                    assert_eq!(record_id, "note");
                    let op: SignedOp = postcard::from_bytes(unsigned_op).unwrap();
                    assert_eq!(
                        op.author,
                        collab_types::Ed25519PublicKey::from_bytes([5; 32])
                    );
                    assert_ne!(op.payload, b"fixture document change".to_vec());
                    assert_eq!(
                        op.encryption.map(|metadata| metadata.channel),
                        Some(channel),
                        "a certified endpoint seals under the route's channel"
                    );
                }
                _ => panic!("document must use the broker policy path"),
            }
            // Synchronize behind the mutation: a denied signature cannot be sent
            // through the harness or unsigned path after this readiness barrier.
            manager.await_primary_route().unwrap();
            assert!(captures.lock().unwrap().signed_ops.is_empty());
            assert!(manager.send(CollabRequest::document_incremental(
                b"later allowed change".to_vec()
            )));
            assert_document_publication_failure(&mut manager, "fixture role revoked");
            assert!(
                manager.is_connected(),
                "policy refusal leaves identity transport usable"
            );
            assert_eq!(
                captures.lock().unwrap().signed_ops.len(),
                1,
                "later RPC succeeds without erasing the earlier prefix failure"
            );
        }

        // Case `host_connector_refused_primary_join_is_not_connected_success`.
        {
            let (attempt_tx, attempt_rx) = mpsc::channel();
            let (stop_tx, stop_rx) = mpsc::channel();
            let calls = Arc::new(AtomicUsize::new(0));
            let (connector, release) =
                host_join_gate_connector(false, calls.clone(), attempt_tx, stop_tx);
            let cluster_id = Uuid::new_v4();
            let result = CollaborationManager::for_route_host_blocking(
                connector,
                "so.tilo.notes",
                cluster_id,
                Some("note".into()),
                None,
            );
            drop(release);
            match result {
                Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
                    assert!(error.to_string().contains("fixture primary join refused"));
                }
                _ => panic!("stream and identity bootstrap must not bypass refused Hello"),
            }
            assert_eq!(
                tilo_test_env::recv("the primary join attempt", &attempt_rx),
                channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(
                attempt_rx.try_recv().is_err(),
                "must not retry another route"
            );
            tilo_test_env::recv("fixture broker stops after failed manager", &stop_rx);
        }
    }

    #[test]
    fn host_connector_pending_handoff_times_out_and_drops_its_future() {
        struct PendingHandoff(mpsc::Sender<()>);
        impl Drop for PendingHandoff {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }

        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let connector = HostSessionConnector::new(move |_| async move {
            let _handoff = PendingHandoff(dropped_tx);
            started_tx.send(()).unwrap();
            std::future::pending::<
                Result<
                    (tokio::io::DuplexStream, tokio::io::DuplexStream),
                    runtime_portal::NotAvailable,
                >,
            >()
            .await
        });
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let caller = thread::spawn(move || {
            let result = CollaborationManager::for_route_host_blocking(
                connector,
                "so.tilo.notes",
                Uuid::new_v4(),
                None,
                None,
            );
            let _ = result_tx.send(result.map(drop));
        });
        tilo_test_env::recv("the fixture broker to start", &started_rx);
        let result = tilo_test_env::recv(
            "the caller to give up on the pending host handoff",
            &result_rx,
        );
        caller.join().unwrap();
        match result {
            Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
            }
            _ => panic!("a pending handoff cannot produce a connected session"),
        }
        tilo_test_env::recv(
            "timed-out async handoff must release its owned resources",
            &dropped_rx,
        );
    }

    #[test]
    fn host_connector_unacknowledged_primary_join_obeys_timeout_without_fallback() {
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let (connector, release) =
            host_join_gate_connector(true, calls.clone(), attempt_tx, stop_tx);
        let cluster_id = Uuid::new_v4();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let started = crate::time::now();
        let caller = thread::spawn(move || {
            let result = CollaborationManager::for_route_host_blocking(
                connector,
                "so.tilo.notes",
                cluster_id,
                Some("note".into()),
                None,
            );
            // Drop any erroneously successful manager on its own caller thread.
            let result = result.map(|manager| {
                drop(manager);
            });
            let _ = result_tx.send(result);
        });
        // The route deadline itself is asserted below; this bound only turns
        // a caller that never returns into a failure.
        let result = result_rx.recv_timeout(tilo_test_env::hang_bound());
        let elapsed = started.elapsed();
        if result.is_err() {
            // A broken worker-side deadline must not leave this test's broker
            // wedged while reporting the regression.
            drop(release);
            // Do not turn an observed timeout regression into an unbounded
            // join. Give the released fixture a bounded chance to shut down.
            if result_rx.recv_timeout(Duration::from_secs(2)).is_ok() || caller.is_finished() {
                caller.join().expect("fixture caller after gate release");
            }
            panic!(
                "gave up waiting for the manager to return from an unacknowledged primary join after {:?}",
                tilo_test_env::hang_bound()
            );
        }
        caller.join().expect("bounded fixture caller");
        match result.expect("the manager returned") {
            Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
                assert_eq!(error.to_string(), PRIMARY_ROUTE_TIMED_OUT);
            }
            _ => panic!("unacknowledged Hello must never return a connected manager"),
        }
        assert!(
            elapsed >= SDK_PRIMARY_ROUTE_TIMEOUT,
            "must wait for the worker-side route deadline"
        );
        assert_eq!(
            tilo_test_env::recv("the primary join attempt", &attempt_rx),
            channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            attempt_rx.try_recv().is_err(),
            "must not retry another route"
        );
        tilo_test_env::recv(
            "timed-out primary join must cancel and close the fixture broker",
            &stop_rx,
        );
        drop(release);
    }

    // Wrap the existing successful broker, changing only its join response.
    // The delayed case delegates to that same implementation after release.
    struct HostJoinGate {
        broker: Rc<FakeBroker>,
        release: RefCell<Option<tokio::sync::oneshot::Receiver<()>>>,
        attempted: mpsc::Sender<String>,
        stopped: mpsc::Sender<()>,
    }

    impl Drop for HostJoinGate {
        fn drop(&mut self) {
            let _ = self.stopped.send(());
        }
    }

    impl collab_service::Server for HostJoinGate {
        fn join_channel(
            self: Rc<Self>,
            params: collab_service::JoinChannelParams,
            results: collab_service::JoinChannelResults,
        ) -> Promise<(), capnp::Error> {
            let route = params
                .get()
                .unwrap()
                .get_channel_id()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            self.attempted.send(route).unwrap();
            let Some(release) = self.release.borrow_mut().take() else {
                return Promise::err(capnp::Error::failed("fixture primary join refused".into()));
            };
            let broker = self.broker.clone();
            Promise::from_future(async move {
                release
                    .await
                    .map_err(|_| capnp::Error::failed("fixture gate dropped".into()))?;
                collab_service::Server::join_channel(broker, params, results).await
            })
        }
    }

    // Releasing on unwind also lets the worker finish if an assertion fails.
    struct HostJoinRelease(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for HostJoinRelease {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    fn host_join_gate_connector(
        delayed: bool,
        calls: Arc<AtomicUsize>,
        attempted: mpsc::Sender<String>,
        stopped: mpsc::Sender<()>,
    ) -> (HostSessionConnector, HostJoinRelease) {
        let (release, wait) = tokio::sync::oneshot::channel();
        let connector = HostSessionConnector::new(move |_| async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let (identity, _) = host_identity_fixture(
                Some(b"fixture certificate".to_vec()),
                vec![runtime_identity::wire::Response::Signature(vec![7; 64])],
            );
            let (client, server) = tokio::io::duplex(64 * 1024);
            // The broker is another process in production: serve it on its
            // own thread, so it ends only when the manager closes the
            // connection, never because the manager's worker ended.
            thread::Builder::new()
                .name("ui-collab-test-join-gate".into())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    tokio::task::LocalSet::new().block_on(&runtime, async move {
                        let broker = Rc::new(FakeBroker {
                            captures: Arc::new(std::sync::Mutex::new(Captures::default())),
                            op_backlog: vec![],
                            presence_backlog: vec![],
                            context_backlog: vec![],
                            scrollback_response: None,
                        });
                        let gate = HostJoinGate {
                            broker,
                            release: RefCell::new(delayed.then_some(wait)),
                            attempted,
                            stopped,
                        };
                        let broker_client: collab_service::Client = capnp_rpc::new_client(gate);
                        let (r, w) = tokio::io::split(server);
                        let network = Box::new(twoparty::VatNetwork::new(
                            r.compat(),
                            w.compat_write(),
                            rpc_twoparty_capnp::Side::Server,
                            capnp::message::ReaderOptions::default(),
                        ));
                        let _ = RpcSystem::new(network, Some(broker_client.client)).await;
                    });
                })
                .unwrap();
            Ok((client, identity))
        });
        (connector, HostJoinRelease(Some(release)))
    }

    /// Stand up a fake `CollabService` on one end of a Unix socket pair;
    /// returns the client-side raw fd + captures state for assertions.
    #[cfg(unix)]
    fn stand_up_fake_broker(broker: FakeBroker) -> (RawFd, SharedCaptures, thread::JoinHandle<()>) {
        let captures = broker.captures.clone();
        let (server_side, client_side) = std::os::unix::net::UnixStream::pair().unwrap();
        server_side.set_nonblocking(true).unwrap();
        client_side.set_nonblocking(true).unwrap();
        let client_fd = client_side.into_raw_fd();

        let handle = thread::Builder::new()
            .name("ui-collab-test-broker".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let local = TokioLocalSet::new();
                local.block_on(&rt, async move {
                    let tokio_stream =
                        TokioUnixStream::from_std(server_side).expect("wrap server side");
                    let broker_client: collab_service::Client = capnp_rpc::new_client(broker);
                    let (r, w) = tokio_stream.into_split();
                    let network = Box::new(twoparty::VatNetwork::new(
                        r.compat(),
                        w.compat_write(),
                        rpc_twoparty_capnp::Side::Server,
                        capnp::message::ReaderOptions::default(),
                    ));
                    let rpc = RpcSystem::new(network, Some(broker_client.client));
                    let _ = rpc.await;
                });
            })
            .unwrap();

        (client_fd, captures, handle)
    }

    #[test]
    fn session_owner_commit_sdk_failure_stops_queued_terminal() {
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
        captures.lock().unwrap().fail_document = true;
        manager.send(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(
                b"committed worker mutation".to_vec(),
            ),

            authorization: None,
        });
        manager.send(CollabRequest::UpdateInvocation {
            id: 42,
            update: tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId::new(),
                output: tilo_actions::ActionOutput::default(),
            },
        });
        tilo_test_env::wait_until(
            "the SDK to disconnect after the owner document RPC failed",
            || !manager.is_connected(),
        );
        assert_eq!(
            captures.lock().unwrap().terminal_updates,
            0,
            "failed document publication must not be followed by successful completion"
        );
        drop(manager);
        thread.join().unwrap();
    }

    #[test]
    fn session_owner_commit_sdk_terminal_failure_disconnects_before_next_request() {
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
        captures.lock().unwrap().fail_terminal = true;
        let invocation = tilo_actions::InvocationId::new();
        manager.send(CollabRequest::UpdateInvocation {
            id: 42,
            update: tilo_actions::InvocationUpdate::Accepted { invocation },
        });
        for id in [42, 43] {
            manager.send(CollabRequest::UpdateInvocation {
                id,
                update: tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
            });
        }
        tilo_test_env::wait_until("`!manager.is_connected()`", || !manager.is_connected());
        assert_eq!(
            captures.lock().unwrap().terminal_updates,
            2,
            "a broker-owned acceptance rejection must not kill a background worker; the subsequent failed terminal must"
        );
        drop(manager);
        thread.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn document_publication_receipt_waits_for_primary_and_routed_rpc_completion() {
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let mut gates = Vec::new();
        for _ in 0..2 {
            let (entered, observed) = mpsc::channel();
            let (release, receiver) = tokio::sync::oneshot::channel();
            captures
                .lock()
                .unwrap()
                .document_gates
                .push_back(DocumentSendGate {
                    entered,
                    release: receiver,
                });
            // Dropping the sender on unwind releases the fake RPC with Err.
            gates.push((observed, release));
        }
        let broker = FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, thread) = stand_up_fake_broker(broker);
        let cluster_id = Uuid::new_v4();
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", cluster_id);
        let secondary = RouteSpec {
            app_id: "other".into(),
            cluster_id,
            record: Some("second".into()),
        };
        manager.join_route(secondary.clone());
        tilo_test_env::wait_until(
            "`broker_state(&manager).document_reads.publishable(&secondary.channel_id())`",
            || {
                broker_state(&manager)
                    .document_reads
                    .publishable(&secondary.channel_id())
            },
        );
        assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
        let first = manager.begin_document_publication_barrier().unwrap();
        assert!(
            manager
                .try_send_to_route(
                    secondary,
                    CollabRequest::document_incremental(b"second".to_vec())
                )
                .is_ok()
        );
        let second = manager.begin_document_publication_barrier().unwrap();
        let mut gates = gates.into_iter();
        let (entered, release) = gates.next().unwrap();
        tilo_test_env::recv("the gated document RPC", &entered);
        assert_eq!(first.try_complete(), Ok(None));
        assert_eq!(second.try_complete(), Ok(None));
        release.send(Ok(())).unwrap();
        let (entered, release) = gates.next().unwrap();
        tilo_test_env::recv("the gated document RPC", &entered);
        // The second RPC can only begin after the first marker has resolved.
        assert_eq!(first.wait(Some(Instant::now())), Ok(()));
        assert_eq!(second.try_complete(), Ok(None));
        release.send(Ok(())).unwrap();
        assert_eq!(second.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
        assert_eq!(captures.lock().unwrap().signed_ops.len(), 2);
        assert!(manager.is_connected());
        drop(manager);
        thread.join().unwrap();
    }

    #[cfg(unix)]
    struct RefusedDocumentIdentity;

    /// Keys for a fresh channel group, as a ready identity service answers.
    #[cfg(unix)]
    fn ready_channel_keys() -> runtime_identity::wire::ChannelKeysReply {
        let group = channel_crypto::ChannelGroup::create(
            collab_types::ChannelId::new(),
            channel_crypto::MemberKey::generate("device").unwrap(),
        )
        .unwrap();
        runtime_identity::wire::ChannelKeysReply {
            access: runtime_identity::wire::ChannelAccess::Ready,
            generation: 1,
            keys: Some(group.content_keys().unwrap().to_bytes().unwrap().to_vec()),
        }
    }

    #[cfg(unix)]
    impl SdkCertifiedIdentity for RefusedDocumentIdentity {
        fn channel_keys(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>
        {
            // Admitted to the channel, so the edit reaches the signer.
            Ok(ready_channel_keys())
        }
        fn device_certificate(&self) -> Option<&[u8]> {
            Some(b"fixture")
        }
        fn signing_public_key(&self) -> [u8; 32] {
            [5; 32]
        }
        fn sign_collaboration_join(
            &self,
            _: &str,
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
            Ok([7; 64])
        }
        fn sign_collaboration_op(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[u8],
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "certified signing refused",
            )
            .into())
        }
    }

    /// A certified identity whose service holds keys for every channel, and
    /// counts how often an endpoint asks for them.
    #[cfg(unix)]
    struct KeyedIdentity {
        asked: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[cfg(unix)]
    impl SdkCertifiedIdentity for KeyedIdentity {
        fn channel_keys(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>
        {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            Ok(ready_channel_keys())
        }
        fn device_certificate(&self) -> Option<&[u8]> {
            Some(b"fixture")
        }
        fn signing_public_key(&self) -> [u8; 32] {
            [5; 32]
        }
        fn sign_collaboration_join(
            &self,
            _: &str,
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
            Ok([7; 64])
        }
        fn sign_collaboration_op(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[u8],
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
            Ok([9; 64])
        }
    }

    /// How often a certified endpoint asked for channel keys after joining a
    /// route the broker serves as committed (`committed`: within a few refresh
    /// intervals) or as an ordinary op stream (once it has asked at all).
    #[cfg(unix)]
    fn channel_key_requests_on_route(committed: bool) -> usize {
        let captures = Captures {
            committed_history_fixture: committed.then(|| automerge::AutoCommit::new().save()),
            ..Captures::default()
        };
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(captures)),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _captures, thread) = stand_up_fake_broker(broker);
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sdk = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: Vec::new(),
                participant_kind: "person",
                signer: SdkDocumentSigner::Certified(Arc::new(KeyedIdentity {
                    asked: Arc::clone(&asked),
                })),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::disabled();
        manager.backend = Backend::Transport(Box::new(sdk));
        assert!(manager.send(CollabRequest::Hello {
            app_id: "so.tilo.test".into(),
            cluster_id: Uuid::new_v4(),
            record: None,
            record_kind: None,
            handle: None,
        }));
        manager.await_primary_route().unwrap();
        if committed {
            // Asking would be the bug, so there is no event to wait for: give
            // the refresh timer a few ticks in which it must stay quiet.
            std::thread::sleep(CHANNEL_KEY_REFRESH * 3);
        } else {
            tilo_test_env::wait_until("the op route's endpoint to ask for channel keys", || {
                asked.load(std::sync::atomic::Ordering::Acquire) > 0
            });
        }
        let asked = asked.load(std::sync::atomic::Ordering::Acquire);
        drop(manager);
        thread.join().unwrap();
        asked
    }

    /// A certified identity service that answers its first question at
    /// once, with `keys`, and holds every later one until `released`.
    #[cfg(unix)]
    struct SlowIdentity {
        keys: Vec<u8>,
        asked: std::sync::atomic::AtomicUsize,
        released: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    }

    #[cfg(unix)]
    impl SlowIdentity {
        fn release(released: &(std::sync::Mutex<bool>, std::sync::Condvar)) {
            *released.0.lock().unwrap() = true;
            released.1.notify_all();
        }
    }

    #[cfg(unix)]
    impl SdkCertifiedIdentity for SlowIdentity {
        fn channel_keys(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>
        {
            let first = self.asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0;
            if !first {
                let (released, changed) = &*self.released;
                let _released = changed
                    .wait_timeout_while(
                        released.lock().unwrap(),
                        tilo_test_env::hang_bound(),
                        |released| !*released,
                    )
                    .unwrap();
            }
            Ok(runtime_identity::wire::ChannelKeysReply {
                access: runtime_identity::wire::ChannelAccess::Ready,
                generation: 1,
                keys: first.then(|| self.keys.clone()),
            })
        }
        fn device_certificate(&self) -> Option<&[u8]> {
            Some(b"fixture")
        }
        fn signing_public_key(&self) -> [u8; 32] {
            [5; 32]
        }
        fn sign_collaboration_join(
            &self,
            _: &str,
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
            Ok([7; 64])
        }
        fn sign_collaboration_op(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[u8],
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
            Ok([9; 64])
        }
    }

    /// An endpoint keeps receiving while its identity service is slow to
    /// answer a key refresh: an op that arrives during the refresh is
    /// delivered while the service still has not answered.
    #[test]
    #[cfg(unix)]
    fn a_slow_identity_service_never_holds_up_incoming_ops() {
        // The op arrives after the first refresh tick, which the identity
        // service holds until the op has been delivered.
        const ARRIVES: Duration = Duration::from_millis(1500);
        let app_id = "so.tilo.test";
        let cluster = Uuid::new_v4();
        let workspace = cluster.to_string();
        let channel = collab_types::document_channel_id(app_id, &workspace, "");
        let artifact = collab_types::document_artifact_id(app_id, &workspace, "");
        let group = channel_crypto::ChannelGroup::create(
            channel,
            channel_crypto::MemberKey::generate("device").unwrap(),
        )
        .unwrap();
        let frame = collab_types::DocumentMutation::Incremental(b"remote change".to_vec())
            .encode()
            .unwrap();
        let (metadata, ciphertext) = group
            .seal_content(artifact, document_frame_retention(&frame), &frame)
            .unwrap();
        let op = SignedOp::new_unsigned_with_encryption(
            ciphertext,
            artifact,
            collab_types::Ed25519PublicKey::from_bytes([3; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            crate::time::SystemTime::UNIX_EPOCH,
            1,
            metadata,
        );
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures {
                op_stream_delay: Some(ARRIVES),
                ..Captures::default()
            })),
            op_backlog: vec![(postcard::to_stdvec(&op).unwrap(), "peer".to_owned())],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _captures, thread) = stand_up_fake_broker(broker);
        let released = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let sdk = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: Vec::new(),
                participant_kind: "person",
                signer: SdkDocumentSigner::Certified(Arc::new(SlowIdentity {
                    keys: group.content_keys().unwrap().to_bytes().unwrap().to_vec(),
                    asked: std::sync::atomic::AtomicUsize::new(0),
                    released: Arc::clone(&released),
                })),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::disabled();
        manager.backend = Backend::Transport(Box::new(sdk));
        assert!(manager.send(CollabRequest::Hello {
            app_id: app_id.into(),
            cluster_id: cluster,
            record: None,
            record_kind: None,
            handle: None,
        }));
        manager.await_primary_route().unwrap();
        let events = poll_until(&mut manager, |events| {
            events
                .iter()
                .any(|event| matches!(event, CollabEvent::Document { .. }))
        });
        SlowIdentity::release(&released);
        assert!(
            events.iter().any(|event| matches!(
                event,
                CollabEvent::Document {
                    decrypted_payload: Some(payload),
                    ..
                } if *payload == frame
            )),
            "the op must arrive, opened, while the key refresh is held: {events:?}"
        );
        drop(manager);
        thread.join().unwrap();
    }

    /// A committed route's owner applies each edit itself and holds no
    /// channel keys, so its edits stay cleartext: the endpoint never asks
    /// for keys there, while an ordinary route of the same endpoint does.
    #[test]
    #[cfg(unix)]
    fn committed_routes_stay_out_of_channel_keys_while_op_routes_ask() {
        assert_eq!(channel_key_requests_on_route(true), 0);
        assert!(channel_key_requests_on_route(false) > 0);
    }

    #[test]
    #[cfg(unix)]
    fn document_publication_certified_signing_refusal_never_reports_success() {
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, thread) = stand_up_fake_broker(broker);
        let sdk = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: Vec::new(),
                participant_kind: "person",
                signer: SdkDocumentSigner::Certified(Arc::new(RefusedDocumentIdentity)),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::disabled();
        manager.backend = Backend::Transport(Box::new(sdk));
        assert!(manager.send(CollabRequest::Hello {
            app_id: "worker".into(),
            cluster_id: Uuid::new_v4(),
            record: None,
            record_kind: None,
            handle: None,
        }));
        manager.await_primary_route().unwrap();
        let route = manager.primary_route().unwrap().route.clone();
        let ready = broker_state(&manager)
            .document_reads
            .close_for_test(&route.channel_id());
        assert!(
            manager
                .try_send_to_route(
                    route,
                    CollabRequest::document_incremental(b"refused".to_vec())
                )
                .is_ok()
        );
        let pending = manager.begin_document_publication_barrier().unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            pending.try_complete(),
            Ok(None),
            "certified publication must still await exact route readiness"
        );
        assert!(captures.lock().unwrap().signed_ops.is_empty());
        ready.store(true, Ordering::Release);
        // Signing may fail before the marker enqueue; either path must carry the cause.
        let error = match manager.begin_document_publication_barrier() {
            Ok(receipt) => receipt
                .wait(Some(tilo_test_env::hang_deadline()))
                .unwrap_err(),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("certified signing refused"),
            "{error}"
        );
        assert!(captures.lock().unwrap().signed_ops.is_empty());
        assert!(!manager.is_connected());
        drop(manager);
        thread.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn committed_queued_publication_retains_payload_and_credits_until_gate_reopens() {
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let (entered, observed) = mpsc::channel();
        let (release, receiver) = tokio::sync::oneshot::channel();
        captures
            .lock()
            .unwrap()
            .document_gates
            .push_back(DocumentSendGate {
                entered,
                release: receiver,
            });
        let broker = FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
        assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
        tilo_test_env::recv("the gated document RPC", &observed);
        assert!(manager.send(CollabRequest::document_incremental(
            b"retained original".to_vec()
        )));
        let state = broker_state(&manager);
        let key = manager.primary_route().unwrap().route.channel_id();
        let ready = state.document_reads.close_for_test(&key);
        let receipt = manager.begin_document_publication_barrier().unwrap();
        release.send(Ok(())).unwrap();
        tilo_test_env::wait_until("`captures.lock().unwrap().signed_ops.len() == 1`", || {
            captures.lock().unwrap().signed_ops.len() == 1
        });
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            receipt.try_complete(),
            Ok(None),
            "accepted publication must hold the barrier while hydration is pending"
        );
        assert!(state.command_bytes.0.load(Ordering::Acquire) >= b"retained original".len());
        ready.store(true, Ordering::Release);
        assert_eq!(receipt.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
        let captured = captures.lock().unwrap().signed_ops.clone();
        assert_eq!(captured.len(), 2);
        let envelope: SignedOp = postcard::from_bytes(&captured[1]).unwrap();
        assert_eq!(
            collab_types::DocumentMutation::decode(&envelope.payload).unwrap(),
            collab_types::DocumentMutation::Incremental(b"retained original".to_vec())
        );
        drop(manager);
        thread.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn committed_queued_publication_deadline_is_terminal() {
        queued_publication_terminal_case(false);
    }

    #[test]
    #[cfg(unix)]
    fn committed_queued_publication_invalidation_is_terminal() {
        queued_publication_terminal_case(true);
    }

    #[cfg(unix)]
    fn queued_publication_terminal_case(invalidate: bool) {
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let (entered, observed) = mpsc::channel();
        let (release, receiver) = tokio::sync::oneshot::channel();
        captures
            .lock()
            .unwrap()
            .document_gates
            .push_back(DocumentSendGate {
                entered,
                release: receiver,
            });
        let broker = FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
        assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
        tilo_test_env::recv("the gated document RPC", &observed);
        assert!(manager.send(CollabRequest::document_incremental(b"must replay".to_vec())));
        let state = broker_state(&manager);
        let ready = state
            .document_reads
            .close_for_test(&manager.primary_route().unwrap().route.channel_id());
        let receipt = manager.begin_document_publication_barrier().unwrap();
        release.send(Ok(())).unwrap();
        tilo_test_env::wait_until("`captures.lock().unwrap().signed_ops.len() == 1`", || {
            captures.lock().unwrap().signed_ops.len() == 1
        });
        if invalidate {
            state.fail_closed("actual test worker invalidation");
        }
        let error = receipt
            .wait(Some(tilo_test_env::hang_deadline()))
            .unwrap_err();
        assert!(
            matches!(error, DocumentPublicationError::Failed(_)),
            "{error}"
        );
        assert!(!manager.is_connected());
        assert!(manager.document_publication_failed());
        ready.store(true, Ordering::Release);
        assert!(!manager.send(CollabRequest::document_incremental(b"late reopen".to_vec())));
        assert_eq!(captures.lock().unwrap().signed_ops.len(), 1);
        tilo_test_env::wait_until(
            "`state.command_bytes.0.load(Ordering::Acquire) == 0`",
            || state.command_bytes.0.load(Ordering::Acquire) == 0,
        );
        drop(manager);
        thread.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn document_publication_rpc_failure_survives_worker_exit() {
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let (entered, observed) = mpsc::channel();
        let (release, receiver) = tokio::sync::oneshot::channel();
        captures
            .lock()
            .unwrap()
            .document_gates
            .push_back(DocumentSendGate {
                entered,
                release: receiver,
            });
        let broker = FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _, thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
        assert!(manager.send(CollabRequest::document_incremental(b"failed".to_vec())));
        tilo_test_env::recv("the gated document RPC", &observed);
        let receipt = manager.begin_document_publication_barrier().unwrap();
        release.send(Err("document RPC refused".into())).unwrap();
        let error = receipt
            .wait(Some(tilo_test_env::hang_deadline()))
            .unwrap_err();
        assert!(
            error.to_string().contains("document RPC refused"),
            "{error}"
        );
        assert!(!manager.is_connected());
        drop(manager);
        thread.join().unwrap();
    }

    #[test]
    fn document_publication_unavailable_and_rejected_markers_are_errors() {
        let mut manager = CollaborationManager::disabled();
        assert!(matches!(
            manager.begin_document_publication_barrier(),
            Err(DocumentPublicationError::Unavailable)
        ));
        let (outbound_tx, mut outbound_rx) = tokio_mpsc::channel(1);
        let (_, inbound_rx) = tokio_mpsc::channel(1);
        let (action_directory_tx, action_directory_rx) = mpsc::sync_channel(1);
        let state = BridgeState::new();
        manager.backend = Backend::Transport(Box::new(BrokerTransport {
            outbound_tx,
            media: None,
            inbound_rx,
            document_turns: Vec::new(),
            state,
            action_directory_rx,
            action_directory_tx,
            pending_primary: None,
            primary_deadline: None,
            worker_stopped: false,
            wifi_bridge_registered: false,
            _thread: None,
        }));
        let earlier = manager.begin_document_publication_barrier().unwrap();
        assert!(matches!(
            manager.begin_document_publication_barrier(),
            Err(DocumentPublicationError::Failed(_))
        ));
        assert!(!manager.is_connected());
        drop(outbound_rx.try_recv().unwrap());
        assert!(matches!(
            earlier.wait(None),
            Err(DocumentPublicationError::Failed(_))
        ));
    }

    /// A receipt wired to the bridge's latched failure, as the transport builds it.
    fn bridge_receipt(
        receiver: mpsc::Receiver<Result<(), DocumentPublicationError>>,
        state: &BridgeState,
    ) -> DocumentPublicationReceipt {
        let state = state.clone();
        DocumentPublicationReceipt::new(receiver)
            .with_disconnect_cause(move || state.publication_result().err())
    }

    #[test]
    fn document_publication_completed_prefix_is_not_poisoned_by_later_failure() {
        let state = BridgeState::new();
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = bridge_receipt(receiver, &state);
        reply.send(state.publication_result()).unwrap();
        state.record_publication_failure(DocumentPublicationError::Failed("later".into()));
        assert_eq!(receipt.wait(Some(Instant::now())), Ok(()));
    }

    #[test]
    fn invocation_update_rpc_diagnostic_keeps_cause_and_existing_fail_closed_latch() {
        let state = BridgeState::new();
        let error = runtime_portal::NotAvailable::Capnp(capnp::Error::failed(
            "unknown invocation id".into(),
        ));
        assert_eq!(
            bounded_rpc_diagnostic(&error),
            ("capnp_failed", "unknown invocation id".into(), false)
        );
        state.fail_closed_with_rpc_error("invocation update outcome unknown", Some((389, &error)));
        assert!(!state.is_healthy());
        assert_eq!(
            state.publication_result(),
            Err(DocumentPublicationError::Failed(
                "invocation update outcome unknown".into()
            ))
        );
        state.fail_closed("later failure");
        assert_eq!(
            state.publication_result(),
            Err(DocumentPublicationError::Failed(
                "invocation update outcome unknown".into()
            ))
        );
    }

    #[test]
    fn invocation_update_rpc_diagnostic_bounds_untrusted_text_without_logging_io_payload() {
        let error = runtime_portal::NotAvailable::Capnp(capnp::Error::failed(format!(
            "refused\n\0{}",
            "🦀".repeat(1024)
        )));
        let (kind, detail, truncated) = bounded_rpc_diagnostic(&error);
        assert_eq!(kind, "capnp_failed");
        assert!(detail.starts_with("refused  "));
        assert_eq!(detail.chars().count(), 512);
        assert!(detail.len() <= 2048);
        assert!(!detail.chars().any(char::is_control));
        assert!(truncated);
        let io = runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private transport payload",
        ));
        assert_eq!(
            bounded_rpc_diagnostic(&io),
            ("connection_failed", "PermissionDenied".into(), false)
        );
    }

    #[test]
    fn document_publication_disconnection_and_pending_deadline_are_errors() {
        let state = BridgeState::new();
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = bridge_receipt(receiver, &state);
        assert_eq!(
            receipt.wait(Some(Instant::now())),
            Err(DocumentPublicationError::DeadlineElapsed)
        );
        drop(reply);
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = bridge_receipt(receiver, &state);
        drop(reply);
        assert_eq!(
            receipt.wait(None),
            Err(DocumentPublicationError::WorkerStopped)
        );
    }

    #[test]
    #[cfg(unix)]
    fn document_publication_superseded_receipt_does_not_stop_worker() {
        let mut manager = connected_manager();
        assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
        drop(manager.begin_document_publication_barrier().unwrap());
        assert!(manager.send(CollabRequest::document_incremental(b"second".to_vec())));
        assert_eq!(
            manager
                .begin_document_publication_barrier()
                .unwrap()
                .wait(Some(tilo_test_env::hang_deadline())),
            Ok(())
        );
        assert!(manager.is_connected());
    }

    #[test]
    fn native_media_leave_bypasses_wedged_document_and_retires_unmute_reply() {
        use runtime_collab::native_media::{Command, Request};
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let (entered, observed) = mpsc::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        captures
            .lock()
            .unwrap()
            .document_gates
            .push_back(DocumentSendGate {
                entered,
                release: wait,
            });
        let (unmute_reply, unmute_wait) = tokio::sync::oneshot::channel();
        captures.lock().unwrap().media_gates.push_back(unmute_wait);
        let (fd, captures, worker) = stand_up_fake_broker(FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        });
        let mut manager =
            CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", Uuid::new_v4());
        assert!(manager.send(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(b"wedged document".to_vec()),
            authorization: None,
        }));
        tilo_test_env::recv("the gated document RPC", &observed);
        assert!(manager.send(CollabRequest::NativeMediaCommand {
            request_id: 1,
            command: Request::new(Command::SetMuted { muted: false })
        }));
        tilo_test_env::wait_until(
            "`captures.lock().unwrap().media_commands.len() == 1`",
            || captures.lock().unwrap().media_commands.len() == 1,
        );
        let start = Instant::now();
        assert!(manager.send(CollabRequest::NativeMediaCommand {
            request_id: 2,
            command: Request::new(Command::Leave)
        }));
        tilo_test_env::wait_until(
            "Leave to dispatch past the wedged document and unmute RPCs",
            || captures.lock().unwrap().media_commands.len() == 2,
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        let requests = captures.lock().unwrap().media_commands.clone();
        assert!(matches!(requests[1].command, Command::Leave));
        assert!(requests[1].intent > requests[0].intent);
        let _ = unmute_reply.send(());
        let mut retired = false;
        tilo_test_env::wait_until("superseded unmute must retire its pending request", || {
            retired |= manager.poll().into_iter().any(|event| {
                matches!(
                    event,
                    CollabEvent::NativeMediaCommandResult {
                        request_id: 1,
                        result: Err(_)
                    }
                )
            });
            retired
        });
        release.send(Ok(())).unwrap();
        drop(manager);
        worker.join().unwrap();
    }

    #[test]
    fn native_media_manager_drop_tears_down_while_document_rpc_is_wedged() {
        use runtime_collab::native_media::{Command, Request};
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let (entered, observed) = mpsc::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        captures
            .lock()
            .unwrap()
            .document_gates
            .push_back(DocumentSendGate {
                entered,
                release: wait,
            });
        let (fd, captures, worker) = stand_up_fake_broker(FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        });
        let mut manager =
            CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", Uuid::new_v4());
        assert!(manager.send(CollabRequest::NativeMediaCommand {
            request_id: 1,
            command: Request::new(Command::Join {
                channel_id: "voice".into()
            })
        }));
        tilo_test_env::wait_until(
            "`captures.lock().unwrap().media_commands.len() == 1`",
            || captures.lock().unwrap().media_commands.len() == 1,
        );
        assert!(manager.send(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(b"wedged document".to_vec()),
            authorization: None,
        }));
        tilo_test_env::recv("the gated document RPC", &observed);
        drop(manager);
        tilo_test_env::wait_until(
            "manager drop must use reserved teardown before document completion",
            || {
                captures
                    .lock()
                    .unwrap()
                    .media_commands
                    .iter()
                    .any(|request| {
                        request.intent == u64::MAX && matches!(request.command, Command::Leave)
                    })
            },
        );
        release.send(Ok(())).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn native_media_full_normal_lane_still_dispatches_reserved_leave() {
        use runtime_collab::native_media::{Command, Request};
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let mut releases = Vec::new();
        for _ in 0..8 {
            let (release, wait) = tokio::sync::oneshot::channel();
            releases.push(release);
            captures.lock().unwrap().media_gates.push_back(wait);
        }
        let (fd, captures, worker) = stand_up_fake_broker(FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        });
        let cluster_id = Uuid::new_v4();
        let mut manager =
            CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", cluster_id);
        let routes = (0..8)
            .map(|index| RouteSpec {
                app_id: "so.tilo.chat".into(),
                cluster_id,
                record: (index != 0).then(|| format!("record-{index}")),
            })
            .collect::<Vec<_>>();
        for route in &routes {
            manager.join_route(route.clone());
        }
        manager
            .begin_document_publication_barrier()
            .unwrap()
            .wait(Some(tilo_test_env::hang_deadline()))
            .unwrap();
        for (index, route) in routes.into_iter().enumerate() {
            assert!(
                manager
                    .try_send_to_route(
                        route,
                        CollabRequest::NativeMediaCommand {
                            request_id: index as u64 + 1,
                            command: Request::new(Command::SetMuted { muted: false })
                        }
                    )
                    .is_ok()
            );
        }
        tilo_test_env::wait_until(
            "`captures.lock().unwrap().media_commands.len() == 8`",
            || captures.lock().unwrap().media_commands.len() == 8,
        );
        assert!(
            !manager.send(CollabRequest::NativeMediaCommand {
                request_id: 9,
                command: Request::new(Command::Status)
            }),
            "normal in-flight budget must stay bounded"
        );
        assert!(
            manager.send(CollabRequest::NativeMediaCommand {
                request_id: 10,
                command: Request::new(Command::Leave)
            }),
            "full normal lane cannot block teardown"
        );
        tilo_test_env::wait_until("the reserved Leave to reach the broker", || {
            captures
                .lock()
                .unwrap()
                .media_commands
                .iter()
                .any(|request| matches!(request.command, Command::Leave))
        });
        for release in releases {
            let _ = release.send(());
        }
        drop(manager);
        worker.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_media_newer_effect_cannot_skip_a_queued_stop_obligation() {
        use runtime_collab::native_media::{Command, Request};
        TokioLocalSet::new()
            .run_until(async {
                for leave in [false, true] {
                    let captures = Arc::new(std::sync::Mutex::new(Captures {
                        media_active_fixture: true,
                        media_capture_active: true,
                        media_pending_consent: true,
                        media_joined: true,
                        ..Default::default()
                    }));
                    let broker: collab_service::Client = capnp_rpc::new_client(FakeBroker {
                        captures: captures.clone(),
                        op_backlog: vec![],
                        presence_backlog: vec![],
                        context_backlog: vec![],
                        scrollback_response: None,
                    });
                    let channel = runtime_collab::CollabClient::from_raw(broker)
                        .join_channel("fixture-admitted-route", b"fixture")
                        .await
                        .unwrap();
                    let (tx, _events) = tokio_mpsc::channel(32);
                    let (sender, receiver) =
                        native_media::channel(InboundBridge::new(tx, BridgeState::new()));
                    let routes = native_media::Routes::default();
                    routes.insert("fixture-admitted-route".into(), channel);
                    let worker = native_media::start(receiver, routes);
                    // No yield: both real enqueues occur before the lane can run.
                    assert!(sender.send(
                        "fixture-admitted-route".into(),
                        1,
                        Request::new(if leave {
                            Command::Leave
                        } else {
                            Command::SetMuted { muted: true }
                        })
                    ));
                    assert!(sender.send(
                        "fixture-admitted-route".into(),
                        2,
                        Request::new(if leave {
                            Command::SelectInput {
                                input_id: "unavailable".into(),
                            }
                        } else {
                            Command::SetMuted { muted: false }
                        })
                    ));
                    tilo_test_env::within("the stop command to reach the broker", async {
                        while captures.lock().unwrap().media_commands.is_empty() {
                            tokio::task::yield_now().await;
                        }
                        for _ in 0..10 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await;
                    let captured = captures.lock().unwrap();
                    assert_eq!(
                        captured.media_stops, 1,
                        "latest completion must not erase the earlier stop effect"
                    );
                    assert!(!captured.media_capture_active);
                    if leave {
                        assert!(!captured.media_joined);
                        assert!(!captured.media_pending_consent);
                    } else {
                        assert!(
                            captured.media_pending_consent,
                            "later unmute requires fresh consent after actual stop"
                        );
                    }
                    drop(captured);
                    drop(sender);
                    drop(worker);
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_media_pending_or_uncertain_stop_blocks_later_effects() {
        use runtime_collab::native_media::{Command, Request};
        TokioLocalSet::new()
            .run_until(async {
                for succeed in [false, true] {
                    let captures = Arc::new(std::sync::Mutex::new(Captures {
                        media_active_fixture: true,
                        media_capture_active: true,
                        media_pending_consent: true,
                        media_joined: true,
                        ..Default::default()
                    }));
                    let (release, gate) = tokio::sync::oneshot::channel();
                    captures.lock().unwrap().media_gates.push_back(gate);
                    let broker: collab_service::Client = capnp_rpc::new_client(FakeBroker {
                        captures: captures.clone(),
                        op_backlog: vec![],
                        presence_backlog: vec![],
                        context_backlog: vec![],
                        scrollback_response: None,
                    });
                    let channel = runtime_collab::CollabClient::from_raw(broker)
                        .join_channel("fixture-admitted-route", b"fixture")
                        .await
                        .unwrap();
                    let (tx, mut events) = tokio_mpsc::channel(32);
                    let (sender, receiver) =
                        native_media::channel(InboundBridge::new(tx, BridgeState::new()));
                    let routes = native_media::Routes::default();
                    routes.insert("fixture-admitted-route".into(), channel);
                    let worker = native_media::start(receiver, routes);
                    assert!(sender.send(
                        "fixture-admitted-route".into(),
                        1,
                        Request::new(Command::SetMuted { muted: true })
                    ));
                    tilo_test_env::within("the mute command to reach the broker", async {
                        while captures.lock().unwrap().media_commands.is_empty() {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await;
                    assert!(sender.send(
                        "fixture-admitted-route".into(),
                        2,
                        Request::new(Command::SetMuted { muted: false })
                    ));
                    for _ in 0..20 {
                        tokio::task::yield_now().await;
                    }
                    assert_eq!(
                        captures.lock().unwrap().media_commands.len(),
                        1,
                        "unacknowledged stop cannot be aborted or bypassed"
                    );
                    assert!(!captures.lock().unwrap().media_pending_consent);
                    if succeed {
                        release.send(()).unwrap();
                    } else {
                        drop(release);
                    }
                    let later = tilo_test_env::within("the later unmute result", async {
                        loop {
                            if let CollabEvent::NativeMediaCommandResult {
                                request_id: 2,
                                result,
                            } = events.recv().await.unwrap().event
                            {
                                break result;
                            }
                        }
                    })
                    .await;
                    if succeed {
                        assert!(later.is_ok());
                        assert!(captures.lock().unwrap().media_pending_consent);
                    } else {
                        assert!(later.is_err());
                        assert_eq!(captures.lock().unwrap().media_commands.len(), 1);
                        assert!(
                            !sender.send(
                                "fixture-admitted-route".into(),
                                3,
                                Request::new(Command::SetMuted { muted: false })
                            ),
                            "uncertain stop remains a route barrier"
                        );
                        assert!(
                            sender.send(
                                "fixture-admitted-route".into(),
                                4,
                                Request::new(Command::Leave)
                            ),
                            "reserved teardown remains available"
                        );
                    }
                    drop(sender);
                    drop(worker);
                }
            })
            .await;
    }

    #[tokio::test]
    async fn native_invocation_delivery_turn_waits_for_every_live_owner() {
        let state = BridgeState::new();
        let primary = state.document_reads.close_for_test("original-route");
        let owner = state.document_reads.close_for_test("live-owner");
        state.document_reads.close_for_test("unowned-retained");
        primary.store(true, Ordering::Release);
        state
            .document_reads
            .set_document_owner_routes(vec!["original-route".into(), "live-owner".into()]);
        let held = state
            .document_reads
            .delivery_turn("live-owner")
            .unwrap()
            .acquire_owned()
            .await
            .unwrap();
        let wait = native_invocation_delivery_turn("original-route", &state);
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut wait)
                .await
                .is_err()
        );
        owner.store(true, Ordering::Release);
        drop(held);
        let turn = wait.await.unwrap().unwrap();
        assert_eq!(turn.permits.len(), 2);
        assert!(!state.document_reads.publishable("unowned-retained"));
    }

    #[tokio::test]
    async fn native_invocation_delivery_turn_rejects_original_generation_retirement() {
        let state = BridgeState::new();
        let original = state.document_reads.close_for_test("original-route");
        let held = state
            .document_reads
            .delivery_turn("original-route")
            .unwrap()
            .acquire_owned()
            .await
            .unwrap();
        let (result, ()) = tokio::join!(
            native_invocation_delivery_turn("original-route", &state),
            async {
                tokio::task::yield_now().await;
                state.document_reads.close_for_test("original-route");
                drop(held);
            }
        );
        assert_eq!(
            result.unwrap_err(),
            "original invocation document custody was retired"
        );
        assert!(!original.load(Ordering::Acquire));
        assert!(!state.document_reads.publishable("original-route"));
    }

    #[tokio::test]
    async fn native_invocation_delivery_turn_timeout_preserves_unapplied_original_custody() {
        let state = BridgeState::new();
        let original = state.document_reads.close_for_test("original-route");
        let result = native_invocation_delivery_turn("original-route", &state).await;
        assert_eq!(
            result.unwrap_err(),
            "original invocation capture did not become current"
        );
        assert!(state.document_reads.current("original-route", &original));
        assert!(!original.load(Ordering::Acquire));
        assert!(!state.document_reads.publishable("original-route"));
    }

    pub(super) fn native_history_invocation_fixture(
        history: Vec<u8>,
    ) -> NativeHistoryInvocationFixture {
        let captures = Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(history),
            invocation_fixture_enabled: true,
            invocation_execution: Some(Ok(false)),
            ..Captures::default()
        }));
        let broker = FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, thread) = stand_up_fake_broker(broker);
        let refresh_captures = captures.clone();
        let invocation_captures = captures.clone();
        let execution_captures = captures.clone();
        let terminal_captures = captures.clone();
        let update_captures = captures.clone();
        NativeHistoryInvocationFixture {
            manager: CollaborationManager::from_sdk_fd_for_test(
                fd,
                "so.tilo.initial-history-probe",
                Uuid::new_v4(),
            ),
            refresh: Box::new(move |history, gate| {
                let mut captures = refresh_captures.lock().unwrap();
                captures.committed_history_fixture = Some(history);
                captures.committed_revision = Some([8; 32]);
                captures.committed_recheck_gate = Some(gate);
            }),
            invoke: Box::new(move |id, uuid| {
                invocation_captures.lock().unwrap().invocations.push_back(
                    runtime_collab::Invocation {
                        id,
                        invocation_uuid: *uuid.as_bytes(),
                        resource: "initial-probe".into(),
                        action: "inspect".into(),
                        argument: None,
                        actor: "fixture-user".into(),
                        context_json: String::new(),
                    },
                )
            }),
            received: Box::new(move || captures.lock().unwrap().received_invocations),
            execution: Box::new(move |state| {
                execution_captures.lock().unwrap().invocation_execution = Some(state)
            }),
            terminals: Box::new(move || terminal_captures.lock().unwrap().terminal_updates),
            updates: Box::new(move || update_captures.lock().unwrap().invocation_updates.clone()),
            thread,
        }
    }

    pub(super) fn native_refresh_fixture(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
    ) -> (CollaborationManager, NativeRefreshProbe) {
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures {
                committed_history_fixture: Some(history),
                ..Captures::default()
            })),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let replace = captures.clone();
        let probe = NativeRefreshProbe {
            committed_read_count: Box::new(move || captures.lock().unwrap().committed_reads.len()),
            replace_history: Box::new(move |history| {
                let mut captures = replace.lock().unwrap();
                captures.committed_history_fixture = Some(history);
                captures.committed_revision = Some([8; 32]);
            }),
        };
        (
            CollaborationManager::from_sdk_fd_for_test(fd, app, room),
            probe,
        )
    }

    pub(super) fn native_initial_fixture(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
    ) -> (CollaborationManager, Box<dyn Fn() -> usize>) {
        native_initial_fixture_gated(app, room, history, None)
    }

    pub(super) fn native_initial_fixture_gated(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> (CollaborationManager, Box<dyn Fn() -> usize>) {
        let initial = Captures {
            committed_history_fixture: Some(history),
            committed_recheck_gate: gate,
            ..Captures::default()
        };
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(initial)),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        (
            CollaborationManager::from_sdk_fd_for_test(fd, app, room),
            Box::new(move || captures.lock().unwrap().joined.len()),
        )
    }

    pub(super) fn native_initial_parked_fixture(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) -> (CollaborationManager, Box<dyn Fn() -> usize>) {
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures {
                committed_history_fixture: Some(history),
                committed_recheck_gate: Some(gate),
                ..Captures::default()
            })),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        (
            CollaborationManager::from_sdk_fd_for_test(fd, app, room),
            Box::new(move || {
                captures
                    .lock()
                    .unwrap()
                    .committed_reads
                    .iter()
                    .filter(|request| {
                        matches!(
                            request,
                            runtime_collab::committed_document::Request::Open {
                                known_revision: Some(_)
                            }
                        )
                    })
                    .count()
            }),
        )
    }

    #[cfg(unix)]
    pub(super) fn capturing_manager() -> (CollaborationManager, Box<dyn Fn() -> Vec<SignedOp>>) {
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let broker = FakeBroker {
            captures: captures.clone(),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _, _thread) = stand_up_fake_broker(broker);
        let manager =
            CollaborationManager::from_sdk_fd_for_test(fd, "capture-test", Uuid::new_v4());
        let probe = Box::new(move || {
            captures
                .lock()
                .unwrap()
                .signed_ops
                .iter()
                .map(|bytes| postcard::from_bytes::<SignedOp>(bytes).unwrap())
                .collect()
        });
        (manager, probe)
    }

    #[cfg(unix)]
    pub(super) fn connected_manager() -> CollaborationManager {
        connected_manager_for("lifecycle-test", Uuid::new_v4())
    }

    #[cfg(unix)]
    pub(super) fn connected_manager_for(app: &str, room: Uuid) -> CollaborationManager {
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _, _thread) = stand_up_fake_broker(broker);
        CollaborationManager::from_sdk_fd_for_test(fd, app, room)
    }

    #[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    struct MonitorPrefs {
        confirm_kill: bool,
        interval_ms: u64,
    }

    impl crate::storage::SettingsSchema for MonitorPrefs {
        const SCOPE: crate::storage::Scope = crate::storage::Scope::Personal;
    }

    /// Wait for `done`, polling: the worker and the fake broker run on their
    /// own threads.
    fn eventually(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !done() {
            assert!(crate::time::now() < deadline, "{what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_apps_settings_reach_the_shells_records_and_follow_them() {
        use settings_store::{SessionPaths, SessionSettings, SettingsSource};
        const APP: &str = "so.tilo.monitor";
        let dir = tempfile::tempdir().unwrap();
        let records = Arc::new(
            SessionSettings::open(SessionPaths::beside(dir.path().join("settings.json"))).unwrap(),
        );
        let paths = records.paths().clone();
        records
            .update_section(
                "app_personal",
                serde_json::from_value(serde_json::json!({APP: {"interval_ms": 1000}})).unwrap(),
            )
            .unwrap();
        let captures = Captures {
            settings: Some((
                APP.to_owned(),
                Arc::clone(&records) as Arc<dyn SettingsSource>,
            )),
            ..Captures::default()
        };
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(captures)),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _, _thread) = stand_up_fake_broker(broker);
        super::super::channel_settings::install(APP);
        let _manager = CollaborationManager::from_sdk_fd_for_test(fd, APP, Uuid::new_v4());
        let prefs = crate::storage::Settings::<MonitorPrefs>::for_app(APP);

        eventually("the shell's view reaches the app", || {
            prefs.get().interval_ms == 1000
        });

        // The app's write lands in the person's record, which roams, and in
        // the copy every other process reads.
        prefs.update(|prefs| prefs.confirm_kill = true).unwrap();
        eventually("the write reaches the person's record", || {
            settings_store::load_at(&paths.person)["app_personal"][APP]["confirm_kill"] == true
        });
        assert_eq!(
            settings_store::load_at(&paths.copy)["app_personal"][APP]["interval_ms"],
            1000
        );

        // Another device's write, as the device fabric applies it, reaches
        // the app's settings.
        let before = prefs.revision();
        records
            .update_section(
                "app_personal",
                serde_json::from_value(
                    serde_json::json!({APP: {"confirm_kill": true, "interval_ms": 3000}}),
                )
                .unwrap(),
            )
            .unwrap();
        eventually("the record's change reaches the app", || {
            prefs.get().interval_ms == 3000
        });
        assert_ne!(prefs.revision(), before, "views following it refresh");

        // Another app's settings are out of reach.
        let error = crate::storage::Settings::<MonitorPrefs>::for_app("so.tilo.notes")
            .set(&MonitorPrefs::default())
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            records.load()["app_personal"]
                .get("so.tilo.notes")
                .is_none()
        );
        super::super::channel_settings::uninstall();
    }

    #[test]
    fn an_apps_live_activity_reaches_the_shell_and_its_controls_come_back() {
        const APP: &str = "so.tilo.clocks";
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let captures = Captures {
            live_activities: Some((APP.to_owned(), Arc::clone(&board))),
            ..Captures::default()
        };
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(captures)),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _, _thread) = stand_up_fake_broker(broker);
        // Shown before the channel joins: the join sends it.
        super::super::channel_live_activity::show(Some(serde_json::json!({"label": "Tea"})));
        let _manager = CollaborationManager::from_sdk_fd_for_test(fd, APP, Uuid::new_v4());
        eventually("the activity reaches the shell", || {
            board.activity(APP) == Some(serde_json::json!({"label": "Tea"}))
        });

        super::super::channel_live_activity::show(Some(serde_json::json!({"label": "Eggs"})));
        eventually("a change reaches the shell", || {
            board.activity(APP) == Some(serde_json::json!({"label": "Eggs"}))
        });

        board.press(APP, serde_json::json!({"command": "toggle"}));
        let mut taken = Vec::new();
        eventually("the control comes back to the app", || {
            taken.extend(super::super::channel_live_activity::take_controls());
            !taken.is_empty()
        });
        assert_eq!(taken, vec![serde_json::json!({"command": "toggle"})]);

        super::super::channel_live_activity::show(None);
        eventually("ending it takes it off the shell", || {
            board.activity(APP).is_none()
        });
    }

    #[cfg(unix)]
    pub(super) fn connecting_manager() -> (
        CollaborationManager,
        crate::wake::WakeScope,
        mpsc::Receiver<()>,
    ) {
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _, _thread) = stand_up_fake_broker(broker);
        let (scope, wakes) = wake_channel();
        let manager = {
            let _entry = scope.enter();
            background_manager(
                fd,
                RouteSpec {
                    app_id: "lifecycle-test".into(),
                    cluster_id: Uuid::new_v4(),
                    record: None,
                },
            )
        };
        (manager, scope, wakes)
    }

    /// Poll until the events received so far satisfy `done`, or the hang
    /// bound passes. Returns everything received either way, so the caller's
    /// assertions report what did arrive.
    fn poll_until(
        mgr: &mut CollaborationManager,
        mut done: impl FnMut(&[CollabEvent]) -> bool,
    ) -> Vec<CollabEvent> {
        let deadline = tilo_test_env::hang_deadline();
        let mut events = Vec::new();
        loop {
            events.extend(mgr.poll());
            if done(&events) || crate::time::now() >= deadline {
                return events;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn bridge_backpressure_sheds_presence_and_fails_closed_for_results() {
        // Case `saturated_outbound_bridge_sheds_only_presence_then_fails_closed`.
        {
            let (tx, mut rx) = tokio_mpsc::channel(1);
            let state = BridgeState::new();
            let document =
                || SdkCommand::Primary(CollabRequest::document_incremental(b"durable".to_vec()));
            let presence = || {
                SdkCommand::Primary(CollabRequest::Presence {
                    key: PresenceKey::Tab {
                        id: "saturated".to_owned(),
                    },
                    payload: PresencePayload::Pointer { x: 1.0, y: 2.0 },
                })
            };

            assert!(enqueue_sdk_command(&tx, document(), &state));
            for _ in 0..10_000 {
                assert!(
                    !enqueue_sdk_command(&tx, presence(), &state),
                    "a full bounded queue may shed only this transient update"
                );
            }
            assert!(state.is_healthy());
            assert!(matches!(
                rx.try_recv(),
                Ok(QueuedSdkCommand {
                    command: SdkCommand::Primary(CollabRequest::Document {
                        mutation: DocumentMutation::Incremental(data),

                        authorization: None,
                    }),
                    ..
                })
                    if data == b"durable"
            ));

            assert!(enqueue_sdk_command(&tx, document(), &state));
            assert_eq!(
                try_enqueue_sdk_command(&tx, document(), &state),
                Err(TransportError::Busy),
                "a full queue defers a document change so its sender retries it"
            );
            assert!(
                state.is_healthy(),
                "deferring a document change never disconnects"
            );
            assert!(
                !enqueue_sdk_command(
                    &tx,
                    SdkCommand::Primary(CollabRequest::RequestDocumentSnapshot),
                    &state
                ),
                "a durable control overflow must be observable as a disconnected bridge"
            );
            assert!(!state.is_healthy());
            assert!(
                rx.try_recv().is_ok() && rx.try_recv().is_err(),
                "the bounded queue retains only its admitted durable command"
            );
        }

        // Case `inbound_events_wake_the_owner_once_per_queued_burst`.
        {
            let mut scope = crate::wake::WakeScope::new();
            let wakes = Arc::new(AtomicUsize::new(0));
            let count = wakes.clone();
            scope
                .bind(move |_| {
                    count.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
            let _entry = scope.enter();
            let (tx, mut rx) = tokio_mpsc::channel(4);
            let inbound = InboundBridge::new(tx, BridgeState::new());
            assert!(inbound.send_durable(CollabEvent::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Update {
                    invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                    state: None,
                },
            }));
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                1,
                "admitted work must wake the owner instead of waiting for its 60Hz poll"
            );
            assert!(inbound.send_presence(CollabEvent::RemoteCursorMoved {
                user_id: "peer".into(),
                x: 1.0,
                y: 2.0,
            }));
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                1,
                "a queued burst needs only one wake"
            );
            assert!(rx.try_recv().is_ok());
            assert!(rx.try_recv().is_ok());
            inbound.state.begin_poll();
            assert!(inbound.send_durable(CollabEvent::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Update {
                    invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                    state: None,
                },
            }));
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                2,
                "an arrival after poll starts must schedule another wake, even during draining"
            );
            assert!(rx.try_recv().is_ok());
            scope.close();
            inbound.state.begin_poll();
            assert!(inbound.send_durable(CollabEvent::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Update {
                    invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                    state: None,
                },
            }));
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                2,
                "a closed owner cannot be woken"
            );
        }

        // Case `saturated_inbound_bridge_preserves_presence_policy_and_fails_for_results`.
        {
            let (tx, mut rx) = tokio_mpsc::channel(1);
            let state = BridgeState::new();
            let inbound = InboundBridge::new(tx, state.clone());

            for _ in 0..10_000 {
                assert!(inbound.send_presence(CollabEvent::RemoteCursorMoved {
                    user_id: "peer".to_owned(),
                    x: 1.0,
                    y: 2.0,
                }));
            }
            assert!(state.is_healthy());
            assert!(matches!(
                rx.try_recv(),
                Ok(QueuedCollabEvent {
                    event: CollabEvent::RemoteCursorMoved { .. },
                    ..
                })
            ));

            assert!(inbound.send_durable(CollabEvent::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Update {
                    invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                    state: None,
                },
            }));
            assert!(
                !inbound.send_durable(CollabEvent::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Receipt(
                        tilo_actions::remote::InvocationReceipt {
                            invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                            status: tilo_actions::remote::ReceiptStatus::Accepted,
                        }
                    ),
                }),
                "an invocation lifecycle result is never silently discarded"
            );
            assert!(!state.is_healthy());
            assert!(matches!(
                rx.try_recv(),
                Ok(QueuedCollabEvent {
                    event: CollabEvent::RemoteAction {
                        message: tilo_actions::remote::RemoteActionMessage::Update { .. }
                    },
                    ..
                })
            ));
            assert!(rx.try_recv().is_err());
        }

        // Case `byte_reservation_releases_when_the_ui_consumes_a_command`.
        {
            let (tx, mut rx) = tokio_mpsc::channel(2);
            let state = BridgeState::new();
            let document = || {
                SdkCommand::Primary(CollabRequest::document_incremental(vec![
                    0;
                    3 * 1024 * 1024
                ]))
            };

            // Consuming its queued wrapper drops its reservation before the next
            // admission, rather than leaking the aggregate budget.
            assert!(enqueue_sdk_command(&tx, document(), &state));
            drop(rx.try_recv().expect("queued document"));
            assert!(enqueue_sdk_command(&tx, document(), &state));
            assert!(state.is_healthy());
        }

        // Case `oversized_inbound_action_fails_before_queueing`.
        {
            let (tx, mut rx) = tokio_mpsc::channel(1);
            let state = BridgeState::new();
            let inbound = InboundBridge::new(tx, state.clone());
            let mut payload = Vec::with_capacity(5 * 1024 * 1024);
            payload.push(1);

            assert!(!inbound.send_durable(CollabEvent::ActionArrived {
                type_id: "test.action".to_owned(),
                payload,
                from_user_id: "peer".to_owned(),
            }));
            assert!(!state.is_healthy());
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn document_budget_defers_or_refuses_without_disconnecting() {
        // Case `over_limit_document_is_refused_explicitly_without_disconnecting`.
        {
            let (tx, mut rx) = tokio_mpsc::channel(1);
            let state = BridgeState::new();
            let limit = collab_types::MAX_DOCUMENT_TRANSFER_BYTES;
            let oversized =
                SdkCommand::Primary(CollabRequest::document_incremental(vec![0; limit + 1]));

            assert_eq!(
                try_enqueue_sdk_command(&tx, oversized, &state),
                Err(TransportError::TooLarge {
                    actual: limit + 1,
                    max: limit,
                })
            );
            assert!(state.is_healthy(), "one oversized change keeps the bridge");
            assert!(rx.try_recv().is_err(), "nothing of it was queued");
            assert!(enqueue_sdk_command(
                &tx,
                SdkCommand::Primary(CollabRequest::document_incremental(vec![1; 8])),
                &state
            ));
        }

        // Case `document_budget_is_charged_for_bytes_sent_not_spare_capacity`.
        {
            let (tx, mut rx) = tokio_mpsc::channel(1);
            let state = BridgeState::new();
            let mut data = Vec::with_capacity(SDK_DOCUMENT_COMMAND_BYTES * 2);
            data.push(0);
            let command = SdkCommand::Primary(CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Snapshot(data),
                authorization: None,
            });

            assert!(enqueue_sdk_command(&tx, command, &state));
            assert!(
                state.document_command_bytes.0.load(Ordering::Acquire) < 1024 * 1024,
                "a mostly-empty allocation is shrunk before it is parked on the bridge"
            );
            assert!(rx.try_recv().is_ok());
        }

        // Case `document_budget_defers_aggregate_documents_without_disconnecting`.
        {
            let (tx, rx) = tokio_mpsc::channel(SDK_COMMAND_QUEUE);
            let state = BridgeState::new();
            let mut admitted = 0;
            let refusal = loop {
                let command =
                    SdkCommand::Primary(CollabRequest::document_incremental(vec![0; 1024 * 1024]));
                match try_enqueue_sdk_command(&tx, command, &state) {
                    Ok(()) => admitted += 1,
                    Err(error) => break error,
                }
            };

            assert_eq!(refusal, TransportError::Busy);
            assert!(admitted < SDK_COMMAND_QUEUE, "bytes, not count, bound it");
            assert!(state.is_healthy());
            assert_eq!(rx.len(), admitted);
            assert!(
                enqueue_sdk_command(
                    &tx,
                    SdkCommand::Primary(CollabRequest::RequestDocumentSnapshot),
                    &state
                ),
                "queued document bytes never starve control traffic"
            );
        }

        // Case `a_busy_document_send_wakes_the_owner_once_room_frees`.
        {
            let mut scope = crate::wake::WakeScope::new();
            let wakes = Arc::new(AtomicUsize::new(0));
            let count = wakes.clone();
            scope
                .bind(move |_| {
                    count.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
            let _entry = scope.enter();
            let (tx, mut rx) = tokio_mpsc::channel(SDK_COMMAND_QUEUE);
            let state = BridgeState::new();
            let change =
                || SdkCommand::Primary(CollabRequest::document_incremental(vec![0; 1024 * 1024]));
            while try_enqueue_sdk_command(&tx, change(), &state).is_ok() {}
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                0,
                "a refusal on its own does not wake the owner"
            );
            drop(rx.try_recv().expect("admitted changes are queued"));
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                1,
                "the shovel taking a change frees room, and the idle owner must hear of it \
                 to retry the refused one"
            );
            assert_eq!(try_enqueue_sdk_command(&tx, change(), &state), Ok(()));
            while rx.try_recv().is_ok() {}
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                1,
                "an admitted send leaves nothing armed"
            );
        }

        // Case `a_directory_request_wakes_the_idle_owner`.
        {
            let mut scope = crate::wake::WakeScope::new();
            let wakes = Arc::new(AtomicUsize::new(0));
            let count = wakes.clone();
            scope
                .bind(move |_| {
                    count.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
            let owner = scope.handle();
            let (requests, pending) = mpsc::sync_channel(1);
            let (reply, _response) = tokio::sync::oneshot::channel();
            submit_directory_request(
                &requests,
                &owner,
                ActionDirectoryRequest::Resolve {
                    revision: String::new(),
                    resource: String::new(),
                    action: String::new(),
                    reply,
                },
            )
            .unwrap();
            assert_eq!(wakes.load(Ordering::SeqCst), 1);
            assert!(pending.try_recv().is_ok());
        }

        // Case `a_lone_document_larger_than_the_budget_still_makes_progress`.
        {
            let budget = ByteBudget::new();
            let big = budget
                .reserve_sole(SDK_DOCUMENT_EVENT_BYTES + 1, SDK_DOCUMENT_EVENT_BYTES)
                .expect("an empty budget admits one oversized item");
            assert!(
                budget.reserve_sole(1, SDK_DOCUMENT_EVENT_BYTES).is_none(),
                "nothing queues beside it"
            );
            drop(big);
            assert!(budget.reserve_sole(1, SDK_DOCUMENT_EVENT_BYTES).is_some());
        }
    }

    #[cfg(unix)]
    #[test]
    fn manager_connects_in_the_background_without_holding_the_app() {
        // Case `a_broker_that_is_not_there_does_not_hold_the_app_up`.
        {
            // A connected socket whose far end is gone — a broker that died
            // between the launcher's connect and the app's first call. `new()`
            // runs before the first frame, so this must return and keep working.
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
            drop(theirs);
            let fd = ours.into_raw_fd();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_SVC_COLLAB_FD", fd.to_string());

            let started = crate::time::now();
            let mut mgr = CollaborationManager::new();
            let took = started.elapsed();
            drop(env);

            assert!(
                took < SHOVEL_INIT_TIMEOUT * 2,
                "constructing the manager must not stall the first frame (took {took:?})"
            );
            // Whichever backend it settled on, the app goes on calling these
            // every frame. Neither may panic.
            mgr.send(CollabRequest::document_snapshot(
                b"after the broker died".to_vec(),
            ));
            let _ = mgr.poll();
        }

        // Case `construction_returns_at_once_while_the_broker_is_silent`.
        {
            // A broker that accepted the connection but never answers: the
            // primary join cannot complete until the peer goes away. The old
            // constructor waited here before the app's first frame.
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
            let (scope, wakes) = wake_channel();
            let entry = scope.enter();
            let route = RouteSpec {
                app_id: "silent-broker-test".into(),
                cluster_id: Uuid::new_v4(),
                record: None,
            };

            let started = crate::time::now();
            let mut mgr = background_manager(ours.into_raw_fd(), route.clone());
            let took = started.elapsed();
            drop(entry);

            assert!(
                took < SDK_PRIMARY_ROUTE_TIMEOUT,
                "construction must not wait for the broker (took {took:?})"
            );
            assert_eq!(mgr.state(), CollaborationState::Connecting);
            assert!(!mgr.is_connected());
            assert!(mgr.primary_route().is_none());
            assert_eq!(mgr.requested_primary_route(), Some(&route));
            let channel =
                collab_types::document_channel_id(&route.app_id, &route.cluster_id.to_string(), "");
            let group = channel_crypto::ChannelGroup::create(
                channel,
                channel_crypto::MemberKey::generate("owner").unwrap(),
            )
            .unwrap();
            assert_eq!(
                mgr.install_document_encryption(route.clone(), group),
                Err("collaboration is still connecting".into()),
                "no call may wait on a worker that is still connecting"
            );
            // The frame loop keeps calling these while connecting. With no
            // acknowledged route nothing is queued behind the connection.
            assert!(!mgr.send(CollabRequest::Presence {
                key: PresenceKey::local_surface_pointer(),
                payload: PresencePayload::Arrived,
            }));
            assert!(mgr.poll().is_empty());
            assert_eq!(mgr.take_state_change(), None, "still connecting");

            // The broker goes away: the join fails and the manager goes offline,
            // telling its owner through a wake rather than a timer.
            drop(theirs);
            let settled = settle(&mut mgr, &wakes);
            assert!(
                matches!(settled, CollaborationState::Offline(_)),
                "{settled:?}"
            );
            assert!(!mgr.is_connected());
            assert_eq!(mgr.take_state_change(), Some(settled));
            assert_eq!(mgr.take_state_change(), None);
            // Offline managers stay usable no-ops.
            mgr.send(CollabRequest::document_snapshot(b"after".to_vec()));
            assert!(mgr.poll().is_empty());
        }

        // Case `a_background_connection_becomes_ready_and_wakes_the_owner`.
        {
            let broker = FakeBroker {
                captures: Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, captures, _thread) = stand_up_fake_broker(broker);
            let (scope, wakes) = wake_channel();
            let entry = scope.enter();
            let route = RouteSpec {
                app_id: "background-connect-test".into(),
                cluster_id: Uuid::new_v4(),
                record: None,
            };
            let mut mgr = background_manager(fd, route.clone());
            drop(entry);
            assert_eq!(mgr.state(), CollaborationState::Connecting);

            let settled = settle(&mut mgr, &wakes);

            assert_eq!(settled, CollaborationState::Ready);
            assert!(mgr.is_connected());
            assert_eq!(
                mgr.primary_route().map(|witness| &witness.route),
                Some(&route)
            );
            assert_eq!(mgr.take_state_change(), Some(CollaborationState::Ready));
            let joined: Vec<String> = captures
                .lock()
                .unwrap()
                .joined
                .iter()
                .map(|(channel, _)| channel.clone())
                .collect();
            assert_eq!(
                joined,
                vec![route.channel_id()],
                "the queued Hello joined the primary route once connected"
            );
        }

        // Case `explicit_test_join_forwards_its_identity_presentation`.
        {
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, captures, _th) = stand_up_fake_broker(broker);
            let cluster = Uuid::new_v4();
            let presentation = b"certified-test-presentation".to_vec();
            let _mgr = CollaborationManager::from_sdk_fd_for_test_with_identity(
                fd,
                "alice",
                cluster,
                presentation.clone(),
            );

            tilo_test_env::wait_until("join_channel must be observed", || {
                !captures.lock().unwrap().joined.is_empty()
            });
            let joined = captures.lock().unwrap().joined.clone();
            assert_eq!(joined.len(), 1);
            assert_eq!(joined[0].1, presentation);
        }

        // Case `first_frame_publishes_actions_without_input`.
        {
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, captures, _thread) = stand_up_fake_broker(broker);
            let app_id = crate::collaboration::local_app_id();
            let manager = CollaborationManager::from_sdk_fd_for_test(fd, &app_id, Uuid::new_v4());
            let mut app = crate::App::with_collaboration_manager(
                crate::State::new(),
                Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
                manager,
            );
            assert!(captures.lock().unwrap().action_publications.is_empty());
            // This calls the production layout/draw passes. No input, palette,
            // explicit publication, or app-owned action invocation primes the app.
            crate::app::draw_prepared_component_test_frame(&mut app);
            tilo_test_env::wait_until(
                "the first rendered frame must publish its semantic actions to the broker without input",
                || {
                    let seen = captures.lock().unwrap();
                    seen.action_directories.iter().any(|id| id == &app_id)
                        && seen.action_publications.iter().any(|(id, actions)| {
                            id == &app_id
                                && actions.iter().any(|(label, action)| {
                                    label == "Complete" && !action.is_empty()
                                })
                        })
                },
            );
        }

        // Case `a_refused_action_publication_is_republished`.
        {
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, captures, _thread) = stand_up_fake_broker(broker);
            captures.lock().unwrap().refuse_action_publications = 1;
            let app_id = crate::collaboration::local_app_id();
            let manager = CollaborationManager::from_sdk_fd_for_test(fd, &app_id, Uuid::new_v4());
            let mut app = crate::App::with_collaboration_manager(
                crate::State::new(),
                Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
                manager,
            );
            crate::app::draw_prepared_component_test_frame(&mut app);
            tilo_test_env::wait_until("the broker must see the first surface", || {
                captures.lock().unwrap().refused_action_publications == 1
            });
            // Nothing about the surface changes: only the refusal can make the
            // app publish it again.
            tilo_test_env::wait_until(
                "a surface the broker refused must be republished, not counted as published",
                || {
                    app.pump_collab_presence();
                    let seen = captures.lock().unwrap();
                    seen.action_publications.iter().any(|(id, actions)| {
                        id == &app_id
                            && actions
                                .iter()
                                .any(|(label, action)| label == "Complete" && !action.is_empty())
                    })
                },
            );
        }
    }

    #[test]
    fn failed_directory_registration_retries_through_transport_and_quiets() {
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures {
                refuse_action_directories: 2,
                ..Captures::default()
            })),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, worker) = stand_up_fake_broker(broker);
        let app_id = crate::collaboration::local_app_id();
        let manager = CollaborationManager::from_sdk_fd_for_test(fd, &app_id, Uuid::new_v4());
        let mut app = crate::App::with_collaboration_manager(
            crate::State::new(),
            Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
            manager,
        );
        crate::app::draw_prepared_component_test_frame(&mut app);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let registered = loop {
            app.pump_collab_presence();
            // Server receipt precedes the asynchronous reply. Observe the real
            // transport acknowledgment before checking idle quiescence.
            if app
                .native_admission_snapshot()
                .directory
                .transport_directory_rpc_successes
                == Some(1)
                && !captures.lock().unwrap().action_directories.is_empty()
            {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(1));
        };
        // Successful registration must remain quiet on unchanged idle pumps.
        for _ in 0..100 {
            app.pump_collab_presence();
        }
        let observation = app.native_admission_snapshot();
        drop(app);
        worker.join().unwrap();
        let seen = captures.lock().unwrap();
        assert!(observation.connection.manager_connected);
        assert!(observation.history.action_history_ready);
        assert!(observation.directory.directory_enqueued);
        assert_eq!(
            observation.directory.transport_directory_rpc_successes,
            Some(1)
        );
        assert_eq!(
            observation.directory.transport_directory_rpc_failures,
            Some(2)
        );
        assert!(
            registered,
            "a real failed directory RPC must retry without input or model changes"
        );
        assert_eq!(seen.directory_attempts.len(), 3);
        assert_eq!(seen.action_directories.as_slice(), &[app_id]);
        for pair in seen.directory_attempts.windows(2) {
            assert!(
                pair[1].duration_since(pair[0]) >= Duration::from_millis(200),
                "refused directory registration must not hot-loop"
            );
        }
        assert!(
            !seen.action_publications.is_empty(),
            "the ordinary semantic surface remains published"
        );
    }

    #[test]
    fn native_admission_ack_wakes_parked_owner_without_an_action() {
        let ack = Arc::new(tokio::sync::Notify::new());
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures {
                directory_ack: Some(ack.clone()),
                ..Captures::default()
            })),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (scope, wakes) = wake_channel();
        let _entry = scope.enter();
        let (fd, captures, worker) = stand_up_fake_broker(broker);
        let manager = CollaborationManager::from_sdk_fd_for_test(
            fd,
            &crate::collaboration::local_app_id(),
            Uuid::new_v4(),
        );
        let mut app = crate::App::with_collaboration_manager(
            crate::State::new(),
            Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
            manager,
        );
        crate::app::draw_prepared_component_test_frame(&mut app);
        eventually("directory RPC reaches actual host", || {
            !captures.lock().unwrap().directory_attempts.is_empty()
        });
        app.pump_collab_presence();
        let before = app.native_admission_snapshot();
        while wakes.try_recv().is_ok() {}
        ack.notify_one();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let observed = loop {
            if wakes
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .is_err()
            {
                break false;
            }
            app.pump_collab_presence();
            if app
                .native_admission_snapshot()
                .directory
                .transport_directory_rpc_successes
                == Some(1)
            {
                break true;
            }
        };
        drop(app);
        worker.join().unwrap();
        assert_eq!(
            before.directory.transport_directory_rpc_successes,
            Some(0),
            "enqueue is not RPC success"
        );
        assert!(
            observed,
            "actual RPC acknowledgement must wake the profiled owner before any app action"
        );
    }

    #[test]
    fn disconnected_manager_is_a_no_op() {
        // No SDK, no legacy socket set — falls through to `Backend::Disabled`.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_SVC_COLLAB_FD");
        let mut mgr = CollaborationManager::new();
        assert!(!mgr.is_connected(), "no service = not connected");
        assert!(matches!(mgr.state(), CollaborationState::Offline(_)));
        assert_eq!(mgr.take_state_change(), None, "offline from the start");
        mgr.send(CollabRequest::document_snapshot(b"nope".to_vec()));
        assert!(mgr.poll().is_empty(), "no events on Disabled backend");
    }

    /// A wake scope whose wakes arrive on a channel, so a test can wait for
    /// the manager to have news instead of polling on a timer.
    fn wake_channel() -> (crate::wake::WakeScope, mpsc::Receiver<()>) {
        let mut scope = crate::wake::WakeScope::new();
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        scope
            .bind(move |_| {
                let _ = tx
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .send(());
            })
            .unwrap();
        (scope, rx)
    }

    /// Poll on every wake until the manager leaves `Connecting`.
    fn settle(mgr: &mut CollaborationManager, wakes: &mpsc::Receiver<()>) -> CollaborationState {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let _ = mgr.poll();
            let state = mgr.state();
            if state != CollaborationState::Connecting {
                return state;
            }
            let left = deadline.saturating_duration_since(crate::time::now());
            assert!(
                wakes.recv_timeout(left).is_ok(),
                "gave up waiting for the connecting manager to wake its owner as it settles; \
                 still Connecting after {:?}",
                tilo_test_env::hang_bound()
            );
        }
    }

    fn background_manager(fd: RawFd, route: RouteSpec) -> CollaborationManager {
        CollaborationManager::connect_in_background(
            SdkConnectSource::Fd(fd),
            SdkIdentitySource::Ready(Some(SdkJoinIdentity {
                presentation: Vec::new(),
                participant_kind: "person",
                signer: harness_document_signer(),
            })),
            route,
            None,
            None,
        )
    }

    #[cfg(unix)]
    #[test]
    fn sdk_transport_forwards_signed_documents_and_presence() {
        // Case `hello_joins_channel_and_send_forwards_signed_op`.
        {
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, captures, _th) = stand_up_fake_broker(broker);
            let cluster = Uuid::new_v4();
            let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", cluster);

            tilo_test_env::wait_until("join_channel must be observed", || {
                !captures.lock().unwrap().joined.is_empty()
            });
            let joined = captures.lock().unwrap().joined.clone();
            assert_eq!(joined.len(), 1);
            // The channel id carries the whole address, app id included.
            assert_eq!(
                joined[0].0,
                channel_key::format("alice", &cluster.to_string(), ""),
            );

            let mut op = SignedOp::new_unsigned(
                b"payload".to_vec(),
                collab_types::ArtifactId::new(),
                collab_types::Ed25519PublicKey::from_bytes([1u8; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpClass::Splice,
                crate::time::SystemTime::UNIX_EPOCH,
                1,
            );
            op.signature = collab_types::Signature::from_bytes([0u8; 64]);
            let expected_bytes = postcard::to_stdvec(&op).unwrap();
            mgr.send(CollabRequest::SignedOperation {
                envelope: Box::new(op),
            });

            tilo_test_env::wait_until("send() must reach the SDK", || {
                !captures.lock().unwrap().signed_ops.is_empty()
            });
            let seen = captures.lock().unwrap().signed_ops.clone();
            assert_eq!(seen, vec![expected_bytes]);
        }

        // Case `owned_route_raw_documents_use_typed_signed_sdk_transport`.
        {
            use automerge::transaction::Transactable;
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, captures, _thread) = stand_up_fake_broker(broker);
            let workspace = Uuid::new_v4();
            let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "shell", workspace);
            let route = RouteSpec::record("actual-owner", workspace, "tab:qualified");
            let mut document = automerge::AutoCommit::new();
            document.put(automerge::ROOT, "body", "before").unwrap();
            document.commit();
            let heads = document.base_heads();
            let snapshot = document.save();
            document.put(automerge::ROOT, "body", "after").unwrap();
            document.commit();
            let delta = document.save_after(&heads);
            // The route refuses documents until it is joined and its document
            // mode is resolved; the first refused attempt starts that join.
            tilo_test_env::wait_until("the owned route becomes publishable", || {
                manager
                    .try_send_to_route(
                        route.clone(),
                        CollabRequest::document_snapshot(snapshot.clone()),
                    )
                    .is_ok()
            });
            manager
                .try_send_to_route(
                    route.clone(),
                    CollabRequest::document_incremental(delta.clone()),
                )
                .unwrap();
            tilo_test_env::wait_until("both raw requests reach SDK signed RPC", || {
                captures.lock().unwrap().signed_ops.len() == 2
            });
            let captured = captures.lock().unwrap();
            assert!(captured.joined.iter().any(|(channel, _)| channel
                == &channel_key::format("actual-owner", &workspace.to_string(), "tab:qualified")));
            for (index, bytes) in captured.signed_ops.iter().enumerate() {
                let envelope: SignedOp = postcard::from_bytes(bytes).unwrap();
                collab_types::validate_document_payload_route(
                    "actual-owner",
                    &workspace.to_string(),
                    "tab:qualified",
                    &envelope,
                )
                .unwrap();
                assert!(
                    envelope.encryption.is_none(),
                    "harness route is explicitly unencrypted"
                );
                let expected = if index == 0 {
                    collab_types::DocumentMutation::Snapshot(snapshot.clone())
                } else {
                    collab_types::DocumentMutation::Incremental(delta.clone())
                };
                assert_eq!(
                    collab_types::DocumentMutation::decode(&envelope.payload).unwrap(),
                    expected
                );
                assert_ne!(
                    envelope.signature,
                    collab_types::Signature::from_bytes([0; 64])
                );
            }
        }

        // Case `publication_barrier_resolves_after_every_earlier_document_is_published`:
        // `send` only queues. A publication barrier resolves once every
        // document mutation queued before it has reached the broker, which is
        // what lets an owner end its session without discarding a change still
        // in transit.
        {
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, captures, _thread) = stand_up_fake_broker(broker);
            let mut manager =
                CollaborationManager::from_sdk_fd_for_test(fd, "shell", Uuid::new_v4());
            for n in 0..8_u8 {
                assert!(manager.send(CollabRequest::document_incremental(vec![n; 16])));
            }
            manager
                .begin_document_publication_barrier()
                .expect("the broker transport supports publication barriers")
                .wait(Some(tilo_test_env::hang_deadline()))
                .expect("every queued document was published");
            assert_eq!(
                captures.lock().unwrap().signed_ops.len(),
                8,
                "the barrier resolved before the documents ahead of it reached the broker"
            );
        }

        // Case `presence_variants_map_to_correct_sdk_publishes`:
        // Every presence variant reaches the SDK method that keeps its key —
        // a pointer publishes keyed, like a caret, rather than through
        // `publish_cursor(x, y)`, which has nowhere to put one.
        {
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, captures, _th) = stand_up_fake_broker(broker);
            let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
            tilo_test_env::wait_until("join_channel must be observed", || {
                !captures.lock().unwrap().joined.is_empty()
            });

            let key = PresenceKey::Tab {
                id: "note-1".to_owned(),
            };
            let expected_key_bytes = postcard::to_stdvec(&key).unwrap();

            mgr.send(CollabRequest::Presence {
                key: key.clone(),
                payload: PresencePayload::Pointer { x: 1.5, y: 2.5 },
            });
            mgr.send(CollabRequest::Presence {
                key: key.clone(),
                payload: PresencePayload::Caret { pos: 12 },
            });
            mgr.send(CollabRequest::Presence {
                key: key.clone(),
                payload: PresencePayload::Selection { start: 3, end: 9 },
            });
            mgr.send(CollabRequest::Presence {
                key: key.clone(),
                payload: PresencePayload::Typing { start: 4, end: 10 },
            });
            mgr.send(CollabRequest::Presence {
                key: key.clone(),
                payload: PresencePayload::Arrived,
            });
            mgr.send(CollabRequest::Presence {
                key,
                payload: PresencePayload::Left,
            });

            tilo_test_env::wait_until(
                "every presence variant must reach the corresponding SDK method",
                || {
                    let c = captures.lock().unwrap();
                    c.pointer_publishes.len() == 1
                        && c.caret_publishes.len() == 1
                        && c.selection_publishes.len() == 1
                        && c.typing_publishes.len() == 1
                        && c.arrived.len() == 1
                        && c.left.len() == 1
                },
            );
            let (
                pointer_publishes,
                cursor_publishes,
                caret_publishes,
                selection_publishes,
                typing_publishes,
                arrived,
                left,
            ) = {
                let c = captures.lock().unwrap();
                (
                    c.pointer_publishes.clone(),
                    c.cursor_publishes.clone(),
                    c.caret_publishes.clone(),
                    c.selection_publishes.clone(),
                    c.typing_publishes.clone(),
                    c.arrived.clone(),
                    c.left.clone(),
                )
            };
            assert_eq!(
                pointer_publishes,
                vec![(expected_key_bytes.clone(), 1.5, 2.5)],
                "a pointer carries the key its publisher chose, like every other \
             keyed variant here",
            );
            assert!(
                cursor_publishes.is_empty(),
                "and does not go out through the keyless call, which would drop \
             the space on the way",
            );
            assert_eq!(caret_publishes, vec![(expected_key_bytes.clone(), 12)]);
            assert_eq!(
                selection_publishes,
                vec![(expected_key_bytes.clone(), 3, 9)]
            );
            assert_eq!(typing_publishes, vec![(expected_key_bytes.clone(), 4, 10)]);
            assert_eq!(arrived, vec![expected_key_bytes.clone()]);
            assert_eq!(left, vec![expected_key_bytes]);
        }
    }

    #[test]
    fn signed_documents_seal_snapshots_and_decrypt_history() {
        // Case `signing_identity_refuses_sequence_reuse_at_u64_boundary`.
        {
            let secret_key = ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32]);
            let mut identity = SigningIdentity::new(secret_key, collab_types::AttestationId::new());
            identity.seq = u64::MAX - 1;
            let artifact = collab_types::ArtifactId::new();

            let final_op = identity
                .sign_op(
                    b"last unique operation".to_vec(),
                    artifact,
                    collab_types::OpClass::Splice,
                )
                .expect("the final unique sequence remains valid");
            assert_eq!(final_op.seq, u64::MAX);
            assert_eq!(identity.seq, u64::MAX);

            let exhausted = identity.sign_op(
                b"must not reuse zero".to_vec(),
                artifact,
                collab_types::OpClass::Splice,
            );
            assert!(matches!(exhausted, Err(SignedSendError::SequenceExhausted)));
            assert_eq!(identity.seq, u64::MAX, "failure is non-mutating");
        }

        // Case `signed_document_event_preserves_snapshot_kind`.
        {
            let op = SignedOp::new_unsigned(
                collab_types::DocumentMutation::Snapshot(b"whole document".to_vec())
                    .encode()
                    .unwrap(),
                collab_types::ArtifactId::new(),
                collab_types::Ed25519PublicKey::from_bytes([3_u8; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpClass::Splice,
                crate::time::SystemTime::UNIX_EPOCH,
                43,
            );
            let event =
                signed_document_event(op, "peer-alice".into(), &plain_route_keys()).unwrap();
            assert!(matches!(
                &event,
                CollabEvent::Document { from_user_id, .. } if from_user_id == "peer-alice"
            ));
            assert!(matches!(
                event.document_mutation(),
                Some(Ok(DocumentMutation::Snapshot(data))) if data == b"whole document"
            ));
        }

        // Case `signed_document_event_decrypts_content_keys_for_history_and_late_joiners`.
        {
            let channel = collab_types::ChannelId::new();
            let artifact = collab_types::ArtifactId::new();
            let alice_key = channel_crypto::MemberKey::generate("alice").unwrap();
            let bob_key = channel_crypto::MemberKey::generate("bob").unwrap();
            let mut alice = channel_crypto::ChannelGroup::create(channel, alice_key).unwrap();
            let addition = alice.admit(bob_key.fresh_key_package().unwrap()).unwrap();
            let bob = channel_crypto::ChannelGroup::process_welcome_with_history(
                channel,
                &addition.welcome,
                &addition.key_history,
                bob_key,
            )
            .unwrap();
            let plaintext = DocumentMutation::Incremental(b"secret change".to_vec())
                .encode()
                .unwrap();
            let alice_endpoint = group_route_keys(alice);
            let PreparedFrame::Ready(ciphertext, metadata) =
                prepare_document_payload(&alice_endpoint, artifact, plaintext.clone())
            else {
                panic!("a provisioned endpoint seals at once");
            };
            let metadata = metadata.expect("a provisioned endpoint never sends plaintext");
            assert_eq!(metadata.channel, channel);
            assert_eq!(
                metadata.retention(),
                Some(collab_types::ContentRetention::Delta)
            );
            let build = |payload: Vec<u8>, artifact, metadata| {
                SignedOp::new_unsigned_with_encryption(
                    payload,
                    artifact,
                    collab_types::Ed25519PublicKey::from_bytes([9; 32]),
                    collab_types::AttestationId::new(),
                    collab_types::OpClass::Splice,
                    crate::time::SystemTime::UNIX_EPOCH,
                    1,
                    metadata,
                )
            };
            let op = build(ciphertext.clone(), artifact, metadata);
            let endpoint = group_route_keys(bob);
            for _ in 0..2 {
                // Live delivery and a later scrollback read of the same op.
                let event = signed_document_event(op.clone(), "alice".into(), &endpoint).unwrap();
                assert!(matches!(
                    event.document_mutation(),
                    Some(Ok(DocumentMutation::Incremental(data))) if data == b"secret change"
                ));
            }
            let plaintext_downgrade = SignedOp::new_unsigned(
                plaintext,
                artifact,
                collab_types::Ed25519PublicKey::from_bytes([9; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpClass::Splice,
                crate::time::SystemTime::UNIX_EPOCH,
                2,
            );
            assert!(
                signed_document_event(plaintext_downgrade, "alice".into(), &endpoint).is_err(),
                "a provisioned MLS route must reject plaintext downgrade"
            );
            assert!(
                signed_document_event(
                    build(
                        ciphertext.clone(),
                        collab_types::ArtifactId::new(),
                        metadata
                    ),
                    "alice".into(),
                    &endpoint
                )
                .is_err(),
                "content is bound to its artifact"
            );
            let mislabeled = collab_types::EncryptionMetadata::content_key(
                metadata.epoch,
                channel,
                collab_types::ContentRetention::Checkpoint,
            );
            assert!(
                signed_document_event(
                    build(ciphertext, artifact, mislabeled),
                    "alice".into(),
                    &endpoint
                )
                .is_err(),
                "the clear storage class must match the content"
            );

            // A member added afterwards reads the op from history.
            let carol_key = channel_crypto::MemberKey::generate("carol").unwrap();
            let addition = {
                let mut keys = alice_endpoint.lock().unwrap();
                let super::super::route_keys::Keys::Group(group) = &mut keys.keys else {
                    panic!("alice's endpoint holds her group");
                };
                group.admit(carol_key.fresh_key_package().unwrap()).unwrap()
            };
            let carol = channel_crypto::ChannelGroup::process_welcome_with_history(
                channel,
                &addition.welcome,
                &addition.key_history,
                carol_key,
            )
            .unwrap();
            let carol_endpoint = group_route_keys(carol);
            let event = signed_document_event(op, "alice".into(), &carol_endpoint).unwrap();
            assert!(matches!(
                event.document_mutation(),
                Some(Ok(DocumentMutation::Incremental(data))) if data == b"secret change"
            ));
        }

        // Case `snapshots_are_sealed_as_retained_checkpoints`.
        {
            let snapshot = DocumentMutation::Snapshot(b"whole".to_vec())
                .encode()
                .unwrap();
            assert_eq!(
                document_frame_retention(&snapshot),
                collab_types::ContentRetention::Checkpoint
            );
            let big = DocumentMutation::Snapshot(
                vec![1; 3 * collab_types::document_transfer::DOCUMENT_SEGMENT_BYTES],
            )
            .encode_frames()
            .unwrap();
            let classes: Vec<_> = big
                .iter()
                .map(|frame| document_frame_retention(frame))
                .collect();
            assert_eq!(
                classes.first(),
                Some(&collab_types::ContentRetention::Checkpoint)
            );
            assert!(
                classes
                    .iter()
                    .skip(1)
                    .all(|class| *class == collab_types::ContentRetention::Delta)
            );
        }

        // Case `joiner_decrypts_fetched_history_with_no_other_member_online`:
        // A member added after the history was written fetches it through the
        // real SDK transport and decrypts every page with the key history it
        // received on joining — no other member is running.
        {
            let app_id = "so.tilo.history-test";
            let cluster = Uuid::from_u128(7);
            let workspace = cluster.to_string();
            let channel = collab_types::document_channel_id(app_id, &workspace, "");
            let artifact = collab_types::document_artifact_id(app_id, &workspace, "");
            let mut alice = channel_crypto::ChannelGroup::create(
                channel,
                channel_crypto::MemberKey::generate("alice").unwrap(),
            )
            .unwrap();
            let history: Vec<Vec<u8>> = (1..=20_u64)
                .map(|seq| {
                    let frame = collab_types::DocumentMutation::Incremental(
                        format!("change {seq}").into_bytes(),
                    )
                    .encode()
                    .unwrap();
                    let (metadata, ciphertext) = alice
                        .seal_content(artifact, document_frame_retention(&frame), &frame)
                        .unwrap();
                    let op = SignedOp::new_unsigned_with_encryption(
                        ciphertext,
                        artifact,
                        collab_types::Ed25519PublicKey::from_bytes([3; 32]),
                        collab_types::AttestationId::new(),
                        collab_types::OpClass::Splice,
                        crate::time::SystemTime::UNIX_EPOCH,
                        seq,
                        metadata,
                    );
                    postcard::to_stdvec(&op).unwrap()
                })
                .collect();
            let carol_key = channel_crypto::MemberKey::generate("carol").unwrap();
            let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
            let carol = channel_crypto::ChannelGroup::process_welcome_with_history(
                channel,
                &addition.welcome,
                &addition.key_history,
                carol_key,
            )
            .unwrap();
            drop(alice);

            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: Some(SeedScrollback {
                    ops: history,
                    earliest_seq: 1,
                    latest_seq: 20,
                    done: true,
                }),
            };
            let (fd, captures, _th) = stand_up_fake_broker(broker);
            let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, app_id, cluster);
            tilo_test_env::wait_until("`!captures.lock().unwrap().joined.is_empty()`", || {
                !captures.lock().unwrap().joined.is_empty()
            });
            mgr.install_document_encryption(
                RouteSpec {
                    app_id: app_id.to_owned(),
                    cluster_id: cluster,
                    record: None,
                },
                carol,
            )
            .unwrap();
            mgr.send(CollabRequest::FetchScrollback {
                artifact,
                before_seq: 21,
                max: 50,
            });
            let events = poll_until(&mut mgr, |events| {
                events
                    .iter()
                    .any(|e| matches!(e, CollabEvent::ScrollbackBatch { .. }))
            });
            let (ops, payloads, oldest_seq) = events
                .into_iter()
                .find_map(|e| match e {
                    CollabEvent::ScrollbackBatch {
                        ops,
                        decrypted_payloads,
                        oldest_seq,
                        ..
                    } => Some((ops, decrypted_payloads, oldest_seq)),
                    _ => None,
                })
                .unwrap();
            assert_eq!(ops.len(), 20);
            assert_eq!(oldest_seq, Some(1));
            for (index, payload) in payloads.iter().enumerate() {
                assert_eq!(
                    collab_types::DocumentMutation::decode(payload.as_deref().unwrap()).unwrap(),
                    collab_types::DocumentMutation::Incremental(
                        format!("change {}", index + 1).into_bytes()
                    )
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn sdk_streams_translate_to_ui_events() {
        // Case `signed_op_stream_translates_to_signed_crdt_incremental`.
        {
            let mut op = SignedOp::new_unsigned(
                collab_types::DocumentMutation::Incremental(b"remote-payload".to_vec())
                    .encode()
                    .unwrap(),
                collab_types::ArtifactId::new(),
                collab_types::Ed25519PublicKey::from_bytes([2u8; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpClass::Splice,
                crate::time::SystemTime::UNIX_EPOCH,
                42,
            );
            op.signature = collab_types::Signature::from_bytes([0u8; 64]);
            let op_bytes = postcard::to_stdvec(&op).unwrap();

            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![(op_bytes, "peer-bob".to_owned())],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, _captures, _th) = stand_up_fake_broker(broker);
            let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
            let events = poll_until(&mut mgr, |events| {
                events
                    .iter()
                    .any(|e| matches!(e, CollabEvent::Document { .. }))
            });

            let signed: Vec<_> = events
                .into_iter()
                .filter_map(|e| match e {
                    CollabEvent::Document {
                        envelope,
                        from_user_id,
                        ..
                    } => Some((envelope, from_user_id)),
                    _ => None,
                })
                .collect();
            assert_eq!(signed.len(), 1, "one document event expected");
            assert_eq!(signed[0].1, "peer-bob");
            assert_eq!(
                collab_types::DocumentMutation::decode(&signed[0].0.payload).unwrap(),
                collab_types::DocumentMutation::Incremental(b"remote-payload".to_vec())
            );
            assert_eq!(signed[0].0.seq, 42);
        }

        // Case `context_stream_translates_to_context_event`.
        {
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![SeedContext {
                    collaborators: vec![
                        ("alice".to_owned(), "Alice".to_owned(), 0x00FF_8000),
                        ("bob".to_owned(), "Bob".to_owned(), 0),
                    ],
                    permissions: vec!["edit".to_owned(), "share".to_owned()],
                }],
                scrollback_response: None,
            };
            let (fd, _captures, _th) = stand_up_fake_broker(broker);
            let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
            let events = poll_until(&mut mgr, |events| {
                events
                    .iter()
                    .any(|e| matches!(e, CollabEvent::Context { .. }))
            });

            let ctx = events
                .iter()
                .find_map(|e| match e {
                    CollabEvent::Context {
                        collaborators,
                        permissions,
                    } => Some((collaborators.clone(), permissions.clone())),
                    _ => None,
                })
                .expect("must receive a Context event");
            let (collabs, perms) = ctx;
            assert_eq!(collabs.len(), 2);
            assert_eq!(collabs[0].id, "alice");
            assert_eq!(collabs[0].name, "Alice");
            // 0x00FF8000 is an orange; 0 says no colour.
            assert_eq!(collabs[0].colour, Some(crate::tokens::PresenceHue::Orange));
            assert_eq!(collabs[1].colour, None);
            assert_eq!(perms, vec!["edit".to_owned(), "share".to_owned()]);
        }

        // Case `presence_stream_translates_cursor_and_all_other_variants`.
        {
            let key = PresenceKey::Tab {
                id: "note-1".to_owned(),
            };
            let key_bytes = postcard::to_stdvec(&key).unwrap();
            let presence_backlog = vec![
                SeedPresence {
                    user_id: "peer-bob".to_owned(),
                    key_bytes: key_bytes.clone(),
                    payload: SeedPayload::Cursor { x: 4.0, y: 5.0 },
                },
                SeedPresence {
                    user_id: "peer-bob".to_owned(),
                    key_bytes: key_bytes.clone(),
                    payload: SeedPayload::Pointer { x: 6.0, y: 9.0 },
                },
                SeedPresence {
                    user_id: "peer-bob".to_owned(),
                    key_bytes: key_bytes.clone(),
                    payload: SeedPayload::Caret { pos: 7 },
                },
                SeedPresence {
                    user_id: "peer-bob".to_owned(),
                    key_bytes: key_bytes.clone(),
                    payload: SeedPayload::Selection { start: 1, end: 3 },
                },
                SeedPresence {
                    user_id: "peer-bob".to_owned(),
                    key_bytes: key_bytes.clone(),
                    payload: SeedPayload::Typing { start: 2, end: 5 },
                },
                SeedPresence {
                    user_id: "peer-bob".to_owned(),
                    key_bytes: key_bytes.clone(),
                    payload: SeedPayload::Arrived,
                },
                SeedPresence {
                    user_id: "peer-bob".to_owned(),
                    key_bytes,
                    payload: SeedPayload::Left,
                },
            ];
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog,
                context_backlog: vec![],
                scrollback_response: None,
            };
            let (fd, _captures, _th) = stand_up_fake_broker(broker);
            let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
            // Other events (connection state, context) can arrive in between, so
            // wait for the seven presence events themselves, not for any seven.
            let events = poll_until(&mut mgr, |events| {
                events
                    .iter()
                    .filter(|e| {
                        matches!(
                            e,
                            CollabEvent::RemoteCursorMoved { .. }
                                | CollabEvent::RemotePresenceUpdate { .. }
                        )
                    })
                    .count()
                    >= 7
            });

            let mut cursor_moves = Vec::new();
            let mut updates = Vec::new();
            for e in events {
                match e {
                    CollabEvent::RemoteCursorMoved { x, y, .. } => cursor_moves.push((x, y)),
                    CollabEvent::RemotePresenceUpdate { key, payload, .. } => {
                        updates.push((key, payload));
                    }
                    _ => {}
                }
            }
            assert!(
                cursor_moves.contains(&(4.0, 5.0)),
                "cursor payload must land on RemoteCursorMoved"
            );
            assert!(
                updates.contains(&(key.clone(), PresencePayload::Pointer { x: 6.0, y: 9.0 })),
                "keyed pointer retains its coordinate-space key"
            );
            assert!(
                updates.contains(&(key.clone(), PresencePayload::Caret { pos: 7 })),
                "caret variant translated"
            );
            assert!(
                updates.contains(&(key.clone(), PresencePayload::Selection { start: 1, end: 3 })),
                "selection variant translated"
            );
            assert!(
                updates.contains(&(key.clone(), PresencePayload::Typing { start: 2, end: 5 })),
                "typing remains distinct from selection"
            );
            assert!(
                updates.contains(&(key.clone(), PresencePayload::Arrived)),
                "arrived variant translated"
            );
            assert!(
                updates.contains(&(key, PresencePayload::Left)),
                "left variant translated"
            );
        }

        // Case `fetch_scrollback_returns_batch_event`.
        {
            let payload = collab_types::DocumentMutation::Incremental(b"old-payload".to_vec())
                .encode()
                .unwrap();
            let mut op = SignedOp::new_unsigned(
                payload,
                collab_types::ArtifactId::new(),
                collab_types::Ed25519PublicKey::from_bytes([3u8; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpClass::Splice,
                crate::time::SystemTime::UNIX_EPOCH,
                5,
            );
            op.signature = collab_types::Signature::from_bytes([0u8; 64]);
            let op_bytes = postcard::to_stdvec(&op).unwrap();
            let seed = SeedScrollback {
                ops: vec![op_bytes],
                earliest_seq: 5,
                latest_seq: 5,
                done: true,
            };
            let broker = FakeBroker {
                captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
                op_backlog: vec![],
                presence_backlog: vec![],
                context_backlog: vec![],
                scrollback_response: Some(seed),
            };
            let (fd, captures, _th) = stand_up_fake_broker(broker);
            let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
            tilo_test_env::wait_until("join_channel must precede scrollback fetch", || {
                !captures.lock().unwrap().joined.is_empty()
            });

            let artifact = collab_types::ArtifactId::new();
            mgr.send(CollabRequest::FetchScrollback {
                artifact,
                before_seq: 10,
                max: 50,
            });
            let events = poll_until(&mut mgr, |events| {
                events
                    .iter()
                    .any(|e| matches!(e, CollabEvent::ScrollbackBatch { .. }))
            });
            let batch = events
                .into_iter()
                .find_map(|e| match e {
                    CollabEvent::ScrollbackBatch { ops, exhausted, .. } => Some((ops, exhausted)),
                    _ => None,
                })
                .expect("must receive a ScrollbackBatch event");
            assert_eq!(batch.0.len(), 1);
            assert_eq!(
                collab_types::DocumentMutation::decode(&batch.0[0].payload).unwrap(),
                collab_types::DocumentMutation::Incremental(b"old-payload".to_vec())
            );
            assert!(batch.1, "seeded broker reports exhausted");
            assert_eq!(
                captures.lock().unwrap().scrollback_requests,
                vec![(artifact.0.as_bytes().to_vec(), 10, 50)],
                "artifact / before_seq / max round-tripped verbatim",
            );
        }
    }

    #[test]
    fn wire_translation_is_symmetric_and_fail_closed() {
        // Case `translate_presence_event_is_symmetric_over_all_payload_variants`.
        {
            // Straight unit test of `translate_presence_event` — no broker or
            // threading needed. Malformed scopes are deliberately not projected.
            let key = PresenceKey::Widget {
                path: vec![crate::presence::PathSegment::Field("body".into())],
            };
            let key_bytes = postcard::to_stdvec(&key).unwrap();

            let e = translate_presence_event(runtime_collab::PresenceEvent {
                user_id: "peer".into(),
                key_bytes: key_bytes.clone(),
                payload: SdkPayload::Cursor { x: 7.0, y: 8.0 },
            })
            .expect("cursor event");
            matches!(e, CollabEvent::RemoteCursorMoved { .. });

            let e = translate_presence_event(runtime_collab::PresenceEvent {
                user_id: "peer".into(),
                key_bytes,
                payload: SdkPayload::Caret { pos: 3 },
            })
            .expect("caret event");
            if let CollabEvent::RemotePresenceUpdate {
                key: k, payload, ..
            } = e
            {
                assert_eq!(k, key);
                assert_eq!(payload, PresencePayload::Caret { pos: 3 });
            } else {
                panic!("expected RemotePresenceUpdate");
            }

            let e = translate_presence_event(runtime_collab::PresenceEvent {
                user_id: "peer".into(),
                key_bytes: vec![0xFF, 0xFF, 0xFF],
                payload: SdkPayload::Arrived,
            });
            assert!(
                e.is_none(),
                "an undecodable target is not semantic presence"
            );
        }

        // Case `invocation_context_json_is_optional_and_fail_closed`.
        {
            let context = tilo_actions::ActionContext::default()
                .with_purpose("Review the proposal")
                .with_group("task:review", "Review", "task");
            let json = serde_json::to_string(&context).unwrap();
            assert_eq!(parse_invocation_context(7, &json), Some(context));
            assert_eq!(parse_invocation_context(8, "  "), None);
            assert_eq!(parse_invocation_context(9, "{not-json"), None);
        }
    }

    #[test]
    fn task_step_replies_keep_committed_and_retry_apart() {
        let update = serde_json::to_string(&tilo_actions::InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId::new(),
            output: tilo_actions::ActionOutput::default(),
        })
        .unwrap();
        let (result, continuation, retry) =
            task_step_event(Ok(runtime_collab::TaskStepReply::Committed {
                update_json: update,
                continuation: b"{\"succeeded\":{}}".to_vec(),
            }));
        assert!(matches!(
            result,
            Ok((0, tilo_actions::InvocationUpdate::Succeeded { .. }))
        ));
        assert_eq!(continuation.as_deref(), Some(&b"{\"succeeded\":{}}"[..]));
        assert!(!retry);

        let (result, continuation, retry) =
            task_step_event(Ok(runtime_collab::TaskStepReply::Retry));
        assert!(result.is_err() && continuation.is_none() && retry);

        let (result, _, retry) = task_step_event(Err("task action is no longer live".into()));
        assert_eq!(result.unwrap_err(), "task action is no longer live");
        assert!(!retry, "a refusal is terminal, not a pending step");
    }

    fn assert_document_publication_failure(manager: &mut CollaborationManager, expected: &str) {
        let error = manager
            .begin_document_publication_barrier()
            .and_then(|receipt| receipt.wait(Some(tilo_test_env::hang_deadline())))
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
    #[test]
    fn initial_action_catalogue_admission_accounts_for_retained_payload() {
        let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(tx, state.clone());
        let snapshot = |bytes: usize| {
            format!(
                r#"{{"surfaces":[{{"peer_id":1,"app_id":"so.tilo.notes","cluster_id":"c","record_id":"{}","live":true,"actions":[]}}]}}"#,
                "r".repeat(bytes)
            )
        };
        let initial = snapshot(700 * 1024);
        admit_initial_action_catalogue(&initial, &inbound, None).unwrap();
        let queued = rx.try_recv().expect("initial catalogue reaches the owner");
        assert_eq!(event_kind(&queued.event), "ActionCatalogue");
        let retained = event_bytes(&queued.event).unwrap();
        assert_eq!(state.event_bytes.0.load(Ordering::Acquire), retained);
        assert!(retained >= 700 * 1024);
        drop(queued);
        assert_eq!(state.event_bytes.0.load(Ordering::Acquire), 0);
        assert!(state.is_healthy());

        let oversized = snapshot(SDK_EVENT_QUEUE_BYTES);
        assert!(admit_initial_action_catalogue(&oversized, &inbound, None).is_err());
        assert!(
            !state.is_healthy(),
            "oversized initial snapshots fail closed"
        );
        assert!(rx.try_recv().is_err(), "rejected payload is never retained");
        assert_eq!(state.event_bytes.0.load(Ordering::Acquire), 0);
    }

    /// A headless worker is drained only between pumps. Large catalogue
    /// snapshots (one per host publication) must not pile up in its bounded
    /// bridge: one stays queued, the rest coalesce to the latest.
    #[test]
    fn action_catalogue_reader_keeps_one_snapshot_queued_for_a_busy_owner() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(tx, state.clone());
        // ~700 KiB per snapshot, as with 34 mounted workers: six exceed the budget.
        let snapshot = |sequence: u64| {
            format!(
                r#"{{"surfaces":[{{"peer_id":{sequence},"app_id":"so.tilo.notes","cluster_id":"c","record_id":"{}","live":true,"actions":[]}}]}}"#,
                "r".repeat(700 * 1024)
            )
        };
        let published = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let reads = published.clone();
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async move {
            let forwarder = inbound.clone();
            tokio::task::spawn_local(async move {
                forward_action_catalogues(
                    async || {
                        let next = reads.get() + 1;
                        reads.set(next);
                        Ok(snapshot(next))
                    },
                    &forwarder,
                    Arc::new(tokio::sync::Semaphore::new(1)),
                )
                .await;
            });
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            assert!(state.is_healthy(), "a busy owner does not fail the bridge");
            let first = rx.try_recv().expect("one catalogue queued");
            assert!(
                rx.try_recv().is_err(),
                "only one catalogue is queued at a time"
            );
            drop(first);
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            assert!(matches!(
                rx.try_recv(),
                Ok(QueuedCollabEvent {
                    event: CollabEvent::ActionCatalogue { ref surfaces },
                    ..
                }) if surfaces[0].peer_id == 2
            ));
            assert!(state.is_healthy());
        });
        assert_eq!(published.get(), 2, "no read while a catalogue is queued");
    }
    fn notebook_action_catalogue_fixture(cluster: Uuid, count: usize) -> String {
        let actions: Vec<_> = (0..count).map(|index| serde_json::json!({
            "resource": format!("notes.page:{index:032x}"),
            "action": "open", "resource_kind": "note",
            "resource_label": format!("Notebook page {index}"),
            "action_label": "Open page",
            // Keep realistic full action metadata in this transport fixture: the
            // initial/watch and oversized tests assert the retained-byte bounds.
            "description": "Open the existing page in its notebook context through the ordinary local reader without changing its contents.",
            "operation_kind": tilo_actions::OperationKind::View,
            "params": [tilo_actions::ActionParam::required("page", tilo_actions::ParamType::Resource, "The page to open")],
            "contract": tilo_actions::ActionContract::view(),
            "input": {"kind": "none"}
        })).collect();
        serde_json::json!({"version": 1, "surfaces": [{
            "peer_id": 1, "app_id": "so.tilo.notes", "cluster_id": cluster.to_string(),
            "record_id": "notebook", "live": true, "actions": actions
        }]})
        .to_string()
    }

    /// Production SDK socket, join and initial/watch delivery; this fixture
    /// does not certify broker authorization.
    #[test]
    #[cfg(unix)]
    fn initial_catalogue_and_watch_share_one_turn_for_a_busy_native_owner() {
        let cluster = Uuid::new_v4();
        let action_count = 2049;
        let snapshot = notebook_action_catalogue_fixture(cluster, action_count);
        let decoded: BrokerActionCatalogueWire = serde_json::from_str(&snapshot).unwrap();
        let retained = event_bytes(&CollabEvent::ActionCatalogue {
            surfaces: decoded.surfaces,
        })
        .expect("the declared catalogue has a finite retained size");
        eprintln!(
            "catalogue fixture serialized_bytes={} retained_bytes={retained}",
            snapshot.len()
        );
        assert!(
            retained < SDK_EVENT_QUEUE_BYTES,
            "the complete baseline fits"
        );
        assert!(
            retained * 2 > SDK_EVENT_QUEUE_BYTES,
            "initial plus watch would overflow"
        );
        let mut updated: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        updated["surfaces"][0]["peer_id"] = serde_json::json!(2);
        let captures = Captures {
            action_catalogue_fixture: Some(vec![snapshot, updated.to_string()]),
            ..Captures::default()
        };
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(captures)),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, worker) = stand_up_fake_broker(broker);
        let sdk = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: Vec::new(),
                participant_kind: "agent",
                signer: harness_document_signer(),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::with_transport(Box::new(sdk));
        assert!(manager.send(CollabRequest::Hello {
            app_id: "so.tilo.notes".into(),
            cluster_id: cluster,
            record: None,
            record_kind: None,
            handle: None,
        }));
        let admission = manager.await_primary_route();
        let state = broker_state(&manager);
        let healthy = state.is_healthy();
        let initial_reads = captures.lock().unwrap().action_catalogue_reads;
        let initial = manager.poll().into_iter().find_map(|event| match event {
            CollabEvent::ActionCatalogue { surfaces } => Some(surfaces),
            _ => None,
        });
        // The first normal owner poll releases the initial turn. The broker
        // watch can then deliver its complete next baseline without overflow.
        let mut watched = None;
        if healthy {
            eventually(
                "the watched catalogue reaches the ordinary owner poll",
                || {
                    watched = manager.poll().into_iter().find_map(|event| match event {
                        CollabEvent::ActionCatalogue { surfaces } => Some(surfaces),
                        _ => None,
                    });
                    watched.is_some()
                },
            );
        }
        drop(manager);
        worker.join().unwrap();
        assert!(
            admission.is_ok(),
            "startup must admit the route: {admission:?}"
        );
        assert!(
            healthy,
            "initial and watched baselines must not disconnect the application"
        );
        assert_eq!(
            initial_reads, 1,
            "do not read a watched snapshot while initial is queued"
        );
        assert_eq!(
            initial.unwrap()[0].actions.len(),
            action_count,
            "complete first baseline"
        );
        let watched = watched.unwrap();
        assert_eq!(
            watched[0].actions.len(),
            action_count,
            "complete watched baseline"
        );
        assert_eq!(
            watched[0].peer_id, 2,
            "the watch delivered its next snapshot"
        );
        assert!(state.is_healthy());
    }

    /// Ordinary application startup must not acquire an unrelated workspace's
    /// eager discovery inventory. Discovery remains on the published directory;
    /// this socket fixture certifies transport readiness, not broker authority.
    #[test]
    #[cfg(unix)]
    fn ordinary_application_starts_with_an_oversized_workspace_catalogue() {
        let cluster = Uuid::new_v4();
        let snapshot = notebook_action_catalogue_fixture(cluster, 4097);
        let decoded: BrokerActionCatalogueWire = serde_json::from_str(&snapshot).unwrap();
        let retained = event_bytes(&CollabEvent::ActionCatalogue {
            surfaces: decoded.surfaces,
        })
        .unwrap();
        eprintln!(
            "startup fixture serialized_bytes={} retained_bytes={retained}",
            snapshot.len()
        );
        assert!(
            retained > SDK_EVENT_QUEUE_BYTES,
            "prove the existing bound matters"
        );
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures {
                action_catalogue_fixture: Some(vec![snapshot]),
                ..Captures::default()
            })),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, worker) = stand_up_fake_broker(broker);
        let sdk = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: Vec::new(),
                participant_kind: "person",
                signer: harness_document_signer(),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::with_transport(Box::new(sdk));
        assert!(manager.send(CollabRequest::Hello {
            app_id: "so.tilo.browser".into(),
            cluster_id: cluster,
            record: None,
            record_kind: None,
            handle: None,
        }));
        let admission = manager.await_primary_route();
        let healthy = broker_state(&manager).is_healthy();
        let subscriptions = captures.lock().unwrap().action_catalogue_subscriptions;
        if admission.is_ok() && healthy {
            // This unbound shared widget uses the normal local publication
            // identity; the broker connection still belongs to the Browser route.
            let publication_app_id = crate::collaboration::local_app_id();
            let mut app = crate::App::with_collaboration_manager(
                crate::State::new(),
                Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
                manager,
            );
            crate::app::draw_prepared_component_test_frame(&mut app);
            eventually(
                "ordinary first frame publishes its actions and pull directory",
                || {
                    let seen = captures.lock().unwrap();
                    seen.action_directories
                        .iter()
                        .any(|id| id == &publication_app_id)
                        && seen.action_publications.iter().any(|(id, actions)| {
                            id == &publication_app_id
                                && actions.iter().any(|(label, action)| {
                                    label == "Complete" && !action.is_empty()
                                })
                        })
                },
            );
            drop(app);
        } else {
            drop(manager);
        }
        worker.join().unwrap();
        assert!(
            admission.is_ok(),
            "ordinary route must become ready: {admission:?}"
        );
        assert!(
            healthy,
            "unconsumed metadata must not disconnect the application"
        );
        assert_eq!(
            subscriptions, 0,
            "ordinary startup does not subscribe to eager discovery"
        );
    }

    /// SDK transport/owner ordering only. This uncertified test endpoint cannot
    /// construct broker NativeDocumentReadContext and is not native authority proof.
    #[test]
    #[cfg(unix)]
    fn committed_initial_route_uses_one_worker_and_blocks_default_publication() {
        use automerge::transaction::Transactable;
        let cluster = Uuid::new_v4();
        let want = crate::record::RecordCheckout::new(
            crate::record::RecordId::minted(),
            crate::record::RecordKind::new("report"),
        );
        let mut source = crate::record::RecordSet::in_memory().with_home(cluster.to_string());
        let mut original = crate::state::State::new();
        assert!(source.checkout(&mut original, None, &want));
        original
            .doc
            .put(automerge::ROOT, "body", "committed real history")
            .unwrap();
        original.doc.commit();
        let heads = original.doc.get_heads();
        let captures = Captures {
            committed_history_fixture: Some(original.doc.save()),
            ..Captures::default()
        };
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(captures)),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.test", cluster);
        assert!(manager.has_native_committed_primary());
        assert!(!manager.send(CollabRequest::document_snapshot(
            b"default before hydration".to_vec()
        )));
        let mut initial = None;
        tilo_test_env::wait_until("the native record to hydrate", || {
            initial = manager.prepare_native_record(&want);
            initial.is_some()
        });
        let read = initial.unwrap();
        let mut set = crate::record::RecordSet::in_memory().with_home(cluster.to_string());
        let mut state = crate::state::State::new();
        assert!(
            read.with_mutation(|mutation| {
                set.checkout_with_initial(&mut state, None, &want, mutation)
            })
            .unwrap()
        );
        assert_eq!(state.doc.get_heads(), heads);
        read.applied();
        manager.switch_native_record(&want).unwrap();
        assert_eq!(
            captures.lock().unwrap().joined.len(),
            2,
            "two routes must share the one inherited transport"
        );
        assert_eq!(
            captures.lock().unwrap().joined_record_kinds,
            ["", "report"],
            "the record route names its resource kind for the owner; the launch route none"
        );
        assert!(
            captures.lock().unwrap().signed_ops.is_empty(),
            "no default was signed or sent"
        );
        tilo_test_env::wait_until("the primary route to become publishable", || {
            broker_state(&manager).document_reads.publishable(
                &manager
                    .primary_route
                    .accepted
                    .as_ref()
                    .unwrap()
                    .route
                    .channel_id(),
            )
        });
    }

    /// A native window that shows another record in its component's place
    /// moves its live broker connection to that record's route: one inherited
    /// transport, a second route joined with the record's kind, and the
    /// window's primary route then the new record's.
    #[test]
    #[cfg(unix)]
    fn a_native_window_showing_another_record_joins_its_route_on_the_live_broker() {
        use crate::application::{LaunchTarget, RestoreInput};
        use crate::application_test_support::{
            DEVICE, runner_inputs, runner_permissions, runner_probe_definition,
        };
        let cluster = Uuid::new_v4();
        let captures = Captures {
            committed_history_fixture: Some(automerge::AutoCommit::new().save()),
            ..Captures::default()
        };
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(captures)),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let definition = runner_probe_definition();
        let shown = crate::record::RecordId::minted();
        let manager = CollaborationManager::for_route_from(
            SdkConnectSource::Fd(fd),
            definition.app_id.to_owned(),
            cluster,
            Some(shown.as_str().to_owned()),
            None,
            None,
            "person",
        )
        .unwrap();
        assert!(manager.has_native_committed_primary());

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut owners = crate::record::RecordOwners::new(root.clone());
        let records = owners
            .open(definition.app_id, &cluster.to_string())
            .unwrap();
        let views = std::rc::Rc::new(
            crate::application_view::InstanceViewStore::new(root, DEVICE, &cluster.to_string())
                .unwrap(),
        );
        let context = crate::application::HostContext::prepare_new_record(
            definition,
            crate::application::HostContextInputs {
                records,
                views,
                device_id: DEVICE.into(),
                instance_id: 41,
                permissions: runner_permissions(),
                author: crate::permissions::UserId("probe-author".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            shown,
        )
        .unwrap();
        let reopener = context.reopener();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let mut app =
            crate::App::from_component_with_collaboration(component, runner_inputs(), manager)
                .unwrap();
        assert_eq!(captures.lock().unwrap().joined.len(), 1, "the launch route");

        let wanted = crate::record::RecordId::minted();
        let (next, target) = reopener.prepare(definition, wanted.clone()).unwrap();
        let replacement = definition
            .instantiate(next, target, RestoreInput::None)
            .unwrap();
        let outcome = reopener.replace(replacement, vec![]);
        assert!(app.swap_requested_component());
        assert_eq!(outcome.get(), Some(Ok(())));
        assert_eq!(
            captures.lock().unwrap().joined.len(),
            2,
            "the record's route shares the one inherited transport"
        );
        assert_eq!(
            captures
                .lock()
                .unwrap()
                .joined_record_kinds
                .last()
                .map(String::as_str),
            Some(definition.record_kind.as_str()),
            "the record route names its kind for the owner"
        );
        assert_eq!(
            app.collaboration_manager()
                .primary_route
                .accepted
                .as_ref()
                .and_then(|route| route.route.record.clone())
                .as_deref(),
            Some(wanted.as_str()),
            "the window's primary route is the record it shows now"
        );
        app.close_components();
    }
}
