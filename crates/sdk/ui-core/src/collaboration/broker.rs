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

use super::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollabTransport, CollaborationManager,
    Collaborator, DocumentMutation, DocumentPublicationError, DocumentPublicationReceipt,
    ExportedAction, ExportedInput, PresenceKey, PresencePayload, PrimaryRouteState, RouteSpec,
    SignedOp, TransportCommand, TransportError, TransportStatus, Uuid, auto_hello_env, channel_key,
    mpsc, presence_handle_env,
};

use crate::time::Duration;
use getrandom::{SysRng, rand_core::UnwrapErr};
use runtime_portal::RawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::LocalSet;

/// Hard fail-closed timeout for an RPC request waiting to be handled on the
/// application's owner thread.  The UI never blocks for it: the Cap'n Proto
/// shovel awaits this reply on its own `LocalSet`.  A stopped app must look
/// unavailable to the broker rather than leaving an RPC worker pinned.
const ACTION_DIRECTORY_REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// A disconnected/slow UI must apply backpressure to broker callbacks rather
/// than allow an unbounded cross-thread queue to become a memory `DoS`.
const ACTION_DIRECTORY_REQUEST_QUEUE: usize = 64;

/// How long an app waits for the collab connect path to report before
/// opening without it, spent on the main thread ahead of the first frame.
/// Every step before the report is a local syscall, so reaching this means
/// something upstream is wedged.
const SHOVEL_INIT_TIMEOUT: Duration = Duration::from_secs(5);
/// The worker must abandon a stuck primary RPC before the caller-side startup
/// deadline so dropping a failed manager always closes its owned transports.
const SDK_PRIMARY_ROUTE_TIMEOUT: Duration = Duration::from_millis(4_500);
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

impl RouteSpec {
    /// The single channel id this route is joined under on the broker.
    fn channel_id(&self) -> String {
        channel_key::format(
            &self.app_id,
            &self.cluster_id.to_string(),
            self.record.as_deref().unwrap_or(""),
        )
    }
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
    inbound_rx: tokio_mpsc::Receiver<QueuedCollabEvent>,
    state: BridgeState,
    /// The receiver stays on the application's owner thread.  RPC servers
    /// own only the cloneable sender, so a Cap'n Proto callback cannot touch
    /// widgets directly.
    action_directory_rx: mpsc::Receiver<ActionDirectoryRequest>,
    action_directory_tx: mpsc::SyncSender<ActionDirectoryRequest>,
    /// Reply slot of the latest [`TransportCommand::ConfirmPrimaryRoute`],
    /// answered by the shovel once it has processed every earlier command.
    pending_primary: Option<mpsc::Receiver<Result<RouteSpec, String>>>,
    /// The shovel dropped a primary-route reply unanswered: it has exited.
    worker_stopped: bool,
    /// Whether [`wifi_continuity_command`] has been pointed at this
    /// transport; done once it has connected.
    wifi_bridge_registered: bool,
    _thread: Option<thread::JoinHandle<()>>,
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
        if self.worker_stopped {
            return TransportStatus::Closed("collaboration worker stopped".into());
        }
        match self.state.connect_phase() {
            ConnectPhase::Failed(reason) => TransportStatus::Closed(reason),
            _ if !self.state.is_healthy() => {
                TransportStatus::Closed("bounded bridge overloaded or disconnected".into())
            }
            ConnectPhase::Connecting => TransportStatus::Connecting,
            ConnectPhase::Connected => TransportStatus::Ready,
        }
    }

    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
        let command = match command {
            TransportCommand::Primary(request) => SdkCommand::Primary(request),
            TransportCommand::ToRoute(route, request) => SdkCommand::ToRoute(route, request),
            TransportCommand::Join(route) => SdkCommand::Join(route),
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

    fn document_author(&self) -> Option<[u8; 32]> {
        self.state.document_author.get().copied()
    }

    fn drain(&mut self, out: &mut Vec<CollabEvent>) {
        self.state.begin_poll();
        if !self.wifi_bridge_registered
            && matches!(self.state.connect_phase(), ConnectPhase::Connected)
        {
            self.register_wifi_bridge();
        }
        while let Ok(evt) = self.inbound_rx.try_recv() {
            out.push(evt.event);
        }
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
        result
    }

    fn wait_primary_route_result(
        &mut self,
        timeout: Duration,
    ) -> Option<Result<RouteSpec, String>> {
        let result = match self.pending_primary.as_ref()?.recv_timeout(timeout) {
            Ok(result) => Some(result),
            Err(mpsc::RecvTimeoutError::Timeout) => return None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.worker_stopped = true;
                None
            }
        };
        self.pending_primary = None;
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
        self.enqueue(SdkCommand::PublicationBarrier(reply))
            .map_err(|error| DocumentPublicationError::Failed(format!("{error:?}")))?;
        Ok(DocumentPublicationReceipt::new(receiver))
    }
}

enum SdkCommand {
    Primary(CollabRequest),
    Join(RouteSpec),
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
}

impl SdkCommand {
    /// Automerge bytes of a document mutation command, if this is one.
    fn document_len(&self) -> Option<usize> {
        match self {
            Self::Primary(CollabRequest::Document { mutation })
            | Self::ToRoute(_, CollabRequest::Document { mutation }) => Some(mutation.data().len()),
            Self::Primary(CollabRequest::CommentReferences { delta })
            | Self::ToRoute(_, CollabRequest::CommentReferences { delta }) => Some(delta.len()),
            _ => None,
        }
    }

    /// Release spare capacity so the retained budget charges only the bytes
    /// that will actually be sent.
    fn shrink_document(&mut self) {
        if let Self::Primary(CollabRequest::Document { mutation })
        | Self::ToRoute(_, CollabRequest::Document { mutation }) = self
        {
            match mutation {
                DocumentMutation::Snapshot(data) | DocumentMutation::Incremental(data) => {
                    data.shrink_to_fit();
                }
            }
        }
        if let Self::Primary(CollabRequest::CommentReferences { delta })
        | Self::ToRoute(_, CollabRequest::CommentReferences { delta }) = self
        {
            delta.shrink_to_fit();
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
    meter.string_capacity(&action.action);
    meter.string_capacity(&action.resource_kind);
    meter.string_capacity(&action.resource_label);
    meter.string_capacity(&action.action_label);
    if let Some(description) = &action.description {
        meter.string_capacity(description);
    }
    meter.string_capacity(&action.operation_kind);
    meter.string_capacity(&action.params_json);
    meter.string_capacity(&action.contract_json);
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
            handle,
            ..
        } => {
            meter.string_capacity(app_id);
            if let Some(record) = record {
                meter.string_capacity(record);
            }
            if let Some(handle) = handle {
                meter.string_capacity(handle);
            }
        }
        CollabRequest::Document { mutation } => match mutation {
            DocumentMutation::Snapshot(data) | DocumentMutation::Incremental(data) => {
                meter.bytes(data);
            }
        },
        CollabRequest::CommentReferences { delta } => meter.bytes(delta),
        CollabRequest::SignedOperation { envelope } => meter.bytes(&envelope.payload),
        CollabRequest::Presence { key, payload } => {
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
        }
        CollabRequest::FetchScrollback { .. }
        | CollabRequest::OpenWorkspaceControl { .. }
        | CollabRequest::CloseWorkspaceControl { .. }
        | CollabRequest::ReportSemanticChange { .. }
        | CollabRequest::RequestDocumentSnapshot => {}
        CollabRequest::RemoteAction { message } => serialized_bytes(message, &mut meter),
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
        CollabRequest::WorkspaceControlInput { event, .. } => serialized_bytes(event, &mut meter),
        CollabRequest::CompleteInvocation { code, message, .. } => {
            meter.string_capacity(code);
            meter.string_capacity(message);
        }
        CollabRequest::UpdateInvocation { update, .. } => serialized_bytes(update, &mut meter),
        CollabRequest::CompleteInspection { reply, .. } => {
            serialized_bytes(reply.as_ref(), &mut meter);
        }
        CollabRequest::RecordLocalEffect { address, update } => {
            meter.string_capacity(&address.resource);
            meter.string_capacity(&address.action);
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
        SdkCommand::Primary(request) => request_bytes(request),
        SdkCommand::ToRoute(route, request) => {
            let mut meter = ByteMeter(request_bytes(request));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::Join(route) => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::PrimaryReady(_) | SdkCommand::PublicationBarrier(_) => {
            Some(std::mem::size_of::<SdkCommand>())
        }
        // `ChannelGroup` intentionally hides its OpenMLS allocation graph,
        // so it cannot offer an exact capacity probe without widening the
        // crypto API. Installation is synchronous and requires `&mut self`,
        // which permits at most one group transfer at a time. Reserving the
        // entire command budget encodes that single-owner exception: no other
        // bridge payload may queue alongside it, while the group itself was
        // already allocated by the caller before entering this bridge.
        SdkCommand::InstallEncryption { .. } => Some(SDK_COMMAND_QUEUE_BYTES),
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
fn event_bytes(event: &CollabEvent) -> Option<usize> {
    let mut meter = ByteMeter(Some(std::mem::size_of::<CollabEvent>()));
    match event {
        CollabEvent::Context {
            collaborators,
            permissions,
        } => context_event_bytes(
            collaborators,
            collaborators.capacity(),
            permissions,
            permissions.capacity(),
            &mut meter,
        ),
        CollabEvent::FollowChanged { peer_id, .. } => {
            if let Some(peer_id) = peer_id {
                meter.string_capacity(peer_id);
            }
        }
        CollabEvent::DocumentSnapshotRequested { route }
        | CollabEvent::ChannelAccess { route, .. } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
        }
        CollabEvent::RemoteCursorMoved { user_id, .. } => meter.string_capacity(user_id),
        CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload,
        } => {
            meter.string_capacity(user_id);
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
        }
        CollabEvent::ActionArrived {
            type_id,
            payload,
            from_user_id,
        } => {
            meter.string_capacity(type_id);
            meter.bytes(payload);
            meter.string_capacity(from_user_id);
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
        }
        CollabEvent::ScrollbackBatch {
            ops,
            decrypted_payloads,
            ..
        } => scrollback_batch_bytes(
            ops,
            ops.capacity(),
            decrypted_payloads,
            decrypted_payloads.capacity(),
            &mut meter,
        ),
        CollabEvent::Inspection { request, .. } => serialized_bytes(request.as_ref(), &mut meter),
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
        }
        CollabEvent::InvocationCancellation { .. } => {}
        CollabEvent::RemoteAction { message } => serialized_bytes(message, &mut meter),
        CollabEvent::TaskActionInvocationResult { result, .. } => match result {
            Ok((_, update)) => serialized_bytes(update, &mut meter),
            Err(error) => meter.string_capacity(error),
        },
        CollabEvent::TaskRun(run) => task_run_bytes(run, &mut meter),
        CollabEvent::TaskRunCancellation { result, .. } => {
            if let Err(error) = result {
                meter.string_capacity(error);
            }
        }
        CollabEvent::WorkspaceControl { event, .. } => {
            workspace_control_event_bytes(event, &mut meter);
        }
        CollabEvent::RealtimeCommandResult { result, .. } => match result {
            Ok(reply) => serialized_bytes(reply, &mut meter),
            Err(error) => meter.string_capacity(error),
        },
        CollabEvent::NearbyTransferCommandResult { result, .. } => match result {
            Ok(reply) => serialized_bytes(reply, &mut meter),
            Err(error) => meter.string_capacity(error),
        },
        CollabEvent::Realtime(event) => serialized_bytes(event, &mut meter),
    }
    meter.finish()
}

/// Shared failure latch. The UI reads it through `is_connected()` even when
/// overload is detected by a subscription task on the shovel thread.
#[derive(Clone)]
struct BridgeState {
    healthy: Arc<AtomicBool>,
    command_bytes: ByteBudget,
    event_bytes: ByteBudget,
    document_command_bytes: ByteBudget,
    document_event_bytes: ByteBudget,
    owner_wake: crate::wake::WakeHandle,
    wake_pending: Arc<AtomicBool>,
    /// How far the shovel has got establishing the session. Written by the
    /// shovel, read by the owner through [`CollabTransport::status`].
    connect: Arc<std::sync::Mutex<ConnectPhase>>,
    /// The device key document envelopes are signed with, once the shovel
    /// has its identity. Owners use it to name comment records they own.
    document_author: Arc<std::sync::OnceLock<[u8; 32]>>,
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
struct ByteBudget(Arc<AtomicUsize>, Arc<tokio::sync::Notify>);

struct ByteReservation {
    budget: ByteBudget,
    bytes: usize,
}

impl ByteBudget {
    fn new() -> Self {
        Self(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(tokio::sync::Notify::new()),
        )
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
    }
}

struct QueuedSdkCommand {
    command: SdkCommand,
    _reservation: ByteReservation,
}

struct QueuedCollabEvent {
    event: CollabEvent,
    _reservation: ByteReservation,
}

impl BridgeState {
    fn new() -> Self {
        Self {
            healthy: Arc::new(AtomicBool::new(true)),
            command_bytes: ByteBudget::new(),
            event_bytes: ByteBudget::new(),
            document_command_bytes: ByteBudget::new(),
            document_event_bytes: ByteBudget::new(),
            owner_wake: crate::wake::WakeHandle::new(),
            wake_pending: Arc::new(AtomicBool::new(false)),
            connect: Arc::new(std::sync::Mutex::new(ConnectPhase::Connecting)),
            document_author: Arc::new(std::sync::OnceLock::new()),
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

    fn fail_closed(&self, direction: &'static str) {
        if self.healthy.swap(false, Ordering::AcqRel) {
            tracing::error!(
                direction,
                "collab: bounded bridge overloaded or disconnected; disabling collaboration"
            );
        }
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
        let Some(reservation) = state.document_command_bytes.reserve_sole(
            command_bytes(&command).unwrap_or(usize::MAX),
            SDK_DOCUMENT_COMMAND_BYTES,
        ) else {
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
            Err(tokio_mpsc::error::TrySendError::Full(_)) => Err(TransportError::Busy),
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
        let Some(reservation) = self.state.event_bytes.reserve(
            event_bytes(&event).unwrap_or(usize::MAX),
            SDK_EVENT_QUEUE_BYTES,
        ) else {
            self.state.fail_closed("inbound event byte budget");
            return false;
        };
        if self
            .tx
            .try_send(QueuedCollabEvent {
                event,
                _reservation: reservation,
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
    device_key: ed25519_dalek::SigningKey,
}

impl CertifiedEndpointIdentity {
    pub fn new(presentation: Vec<u8>, device_key: ed25519_dalek::SigningKey) -> Self {
        Self {
            presentation,
            device_key,
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
            .finish()
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
trait SdkCertifiedIdentity: Send {
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
    Certified(Box<dyn SdkCertifiedIdentity>),
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
        signer: SdkDocumentSigner::Certified(Box::new(identity)),
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
            let kind = if error == "collaboration primary route timed out" {
                std::io::ErrorKind::TimedOut
            } else {
                std::io::ErrorKind::ConnectionRefused
            };
            runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(kind, error))
        }),
        None if transport.status().is_open() => Err(
            runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "collaboration primary route did not become ready in time",
            )),
        ),
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
                presentation: identity.presentation,
                participant_kind: "person",
                signer: SdkDocumentSigner::Harness(Box::new(identity.device_key)),
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
            handle,
        }));
        await_primary_route_on(&mut transport, &mut primary_route)?;
        Ok(ConnectedBroker {
            transport,
            primary_route,
        })
    }

    /// Wait until the shovel has processed the initial `Hello`, admitted its
    /// primary route, and queued the route's initial action catalogue. Without
    /// this barrier a caller can publish transient presence immediately after
    /// construction, before the broker knows where to deliver it.
    #[cfg(test)]
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
                presentation: identity.presentation,
                participant_kind: "agent",
                signer: SdkDocumentSigner::Harness(Box::new(identity.device_key)),
            }),
            "agent",
        )
    }

    pub(super) fn new_scoped(record: Option<String>) -> Self {
        // Without an inherited service there is nothing to reach; say so
        // without starting a worker.
        if runtime_portal::service_fd("collab").is_none() {
            return Self::disabled();
        }
        let (app_id, cluster_id) = auto_hello_env();
        Self::connect_in_background(
            SdkConnectSource::Env,
            SdkIdentitySource::Inherited(cluster_id),
            RouteSpec {
                app_id,
                cluster_id,
                record,
            },
            presence_handle_env(),
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
        let (action_directory_tx, action_directory_rx) =
            mpsc::sync_channel::<ActionDirectoryRequest>(ACTION_DIRECTORY_REQUEST_QUEUE);
        let (init_tx, init_rx) = mpsc::sync_channel::<InitStatus>(1);

        // A thread we can't start is a service we can't reach — map to
        // NotAvailable rather than panicking; an app must open even without collab.
        let handle = match thread::Builder::new()
            .name("ui-collab-shovel".into())
            .spawn(move || run_shovel(source, identity, outbound_rx, inbound, init_tx))
        {
            Ok(handle) => handle,
            Err(e) => return Err(runtime_portal::NotAvailable::ConnectionFailed(e)),
        };

        let transport = BrokerTransport {
            outbound_tx,
            inbound_rx,
            state,
            action_directory_rx,
            action_directory_tx,
            pending_primary: None,
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
        let _ = state
            .document_author
            .set(signer_public_key(&identity.signer));
        // The transport is ready. The manager's primary Hello and its
        // `PrimaryReady` barrier are the first commands the shovel serves.
        report_init(&state, &init_tx, InitStatus::Ready);

        drive_shovel(client, identity, outbound_rx, inbound_tx).await;
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
    inbound_tx: InboundBridge,
) {
    let LocalSdkJoinIdentity {
        presentation,
        participant_kind,
        mut signer,
    } = identity;
    // Every route of a certified endpoint is an encrypted channel whose keys
    // come from the identity service; only a harness with no such service
    // stays plaintext.
    let asks_channel_keys = !matches!(
        signer,
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(_))
    );
    let mut key_refresh = tokio::time::interval(CHANNEL_KEY_REFRESH);
    key_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut routes = std::collections::HashMap::<String, SdkRoute>::new();
    let mut primary: Option<String> = None;
    let mut participant_handle: Option<String> = None;
    let mut primary_result = Err("primary collaboration route was not announced".to_string());
    // The first document mutation since the last publication barrier that
    // did not reach the broker, reported to (and cleared by) that barrier.
    let mut unpublished: Option<String> = None;

    loop {
        let queued = tokio::select! {
            queued = outbound_rx.recv() => queued,
            _ = key_refresh.tick(), if asks_channel_keys => {
                refresh_all_route_keys(&routes, &mut signer, &inbound_tx).await;
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
            SdkCommand::Primary(CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                handle,
            }) => {
                participant_handle = handle;
                let spec = RouteSpec {
                    app_id,
                    cluster_id,
                    record,
                };
                let key = spec.channel_id();
                primary_result = match tokio::time::timeout(
                    SDK_PRIMARY_ROUTE_TIMEOUT,
                    ensure_sdk_route(
                        &client,
                        &presentation,
                        participant_handle.as_deref(),
                        participant_kind,
                        &inbound_tx,
                        &mut routes,
                        &spec,
                        asks_channel_keys,
                    ),
                )
                .await
                {
                    Ok(result) => result.map(|()| spec),
                    Err(_) => Err("collaboration primary route timed out".to_string()),
                };
                primary = primary_result.is_ok().then_some(key);
            }
            SdkCommand::Primary(request) => {
                if let Some(route) = primary.as_ref().and_then(|key| routes.get_mut(key)) {
                    published =
                        dispatch_sdk_request(route, request, &mut signer, &inbound_tx).await;
                    if !inbound_tx.state.is_healthy() {
                        break;
                    }
                } else {
                    tracing::warn!("collab: request sent before the primary route joined");
                    published = false;
                }
            }
            SdkCommand::Join(spec) => {
                let _ = ensure_sdk_route(
                    &client,
                    &presentation,
                    participant_handle.as_deref(),
                    participant_kind,
                    &inbound_tx,
                    &mut routes,
                    &spec,
                    asks_channel_keys,
                )
                .await;
            }
            SdkCommand::ToRoute(spec, request) => {
                let key = spec.channel_id();
                let joined = ensure_sdk_route(
                    &client,
                    &presentation,
                    participant_handle.as_deref(),
                    participant_kind,
                    &inbound_tx,
                    &mut routes,
                    &spec,
                    asks_channel_keys,
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
                }
            }
            SdkCommand::PrimaryReady(reply) => {
                let _ = reply.send(primary_result.clone());
                // A manager still connecting settles on this answer.
                inbound_tx.state.notify_events();
            }
            SdkCommand::PublicationBarrier(reply) => {
                let _ = reply.send(match unpublished.take() {
                    None => Ok(()),
                    Some(reason) => Err(DocumentPublicationError::Failed(reason)),
                });
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
                let directory = capnp_rpc::new_client(ActionDirectoryServer { requests });
                if let Err(error) = route
                    .channel
                    .publish_action_directory(&app_id, directory)
                    .await
                {
                    tracing::warn!(%error, "collab: publishing pull resource/action directory failed");
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
        }
        if document && !published && unpublished.is_none() {
            unpublished = Some("a document mutation did not reach the broker".into());
        }
        if asks_channel_keys && routes.len() > routes_before {
            // A route joined just now learns its keys (or that it waits)
            // before its first edit rather than at the next tick.
            refresh_all_route_keys(&routes, &mut signer, &inbound_tx).await;
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
    ensure_sdk_route(
        context.client,
        context.presentation,
        context.participant_handle,
        context.participant_kind,
        context.inbound_tx,
        routes,
        &spec,
        context.asks_channel_keys,
    )
    .await?;
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

struct SdkJoinContext<'a> {
    asks_channel_keys: bool,
    client: &'a runtime_collab::CollabClient,
    presentation: &'a [u8],
    participant_handle: Option<&'a str>,
    participant_kind: &'a str,
    inbound_tx: &'a InboundBridge,
}

struct SdkRoute {
    spec: RouteSpec,
    channel: runtime_collab::ChannelHandle,
    presence: Option<runtime_collab::PresenceHandle>,
    workspace_controls: std::collections::HashMap<u64, runtime_collab::WorkspaceControlHandle>,
    encryption: SharedRouteKeys,
    retained: std::sync::Arc<std::sync::Mutex<RetainedLog>>,
}

/// The document ops this route sent or received, each with the changes it
/// carries, so a snapshot can name exactly the ones its saved bytes hold
/// (the schema's `send` `covers`). A durable host trims the document's
/// retained history only by those names, never by delivery order.
///
/// Bounded to the newest [`Self::MAX_ENTRIES`] ids; one seen again (a host
/// replay) becomes the newest, so an id a host still retains ages out only
/// after every id retained after it.
#[derive(Debug, Default)]
pub(crate) struct RetainedLog {
    /// Each logged id's generation and required changes.
    entries: std::collections::HashMap<[u8; 32], (u64, Vec<automerge::ChangeHash>)>,
    /// `(id, generation)` oldest first; a pair whose generation no longer
    /// matches `entries` was refreshed later and is skipped.
    order: std::collections::VecDeque<([u8; 32], u64)>,
    next_generation: u64,
    /// Segment ids of transfers still being reassembled, by author and
    /// transfer digest; they are held exactly when the whole mutation is.
    segments: std::collections::HashMap<SegmentTransfer, Vec<[u8; 32]>>,
}

/// One segmented document transfer: its author and transfer digest.
pub(crate) type SegmentTransfer = ([u8; 32], [u8; 32]);

impl RetainedLog {
    /// As many as one snapshot may name.
    const MAX_ENTRIES: usize = 65_536;
    /// Transfers reassembled at once; an abandoned one is forgotten.
    const MAX_TRANSFERS: usize = 64;

    /// Log the ops `entries` (a mutation and any segments it arrived in)
    /// as carrying the Automerge bytes `mutation`. Bytes this cannot read
    /// are never claimed as held.
    pub(crate) fn record(&mut self, entries: &[[u8; 32]], mutation: &[u8]) {
        let Some(required) = tilo_automerge::coverage::required_changes(mutation) else {
            return;
        };
        for entry in entries {
            let generation = self.next_generation;
            self.next_generation = self.next_generation.saturating_add(1);
            self.entries.insert(*entry, (generation, required.clone()));
            self.order.push_back((*entry, generation));
        }
        while self.entries.len() > Self::MAX_ENTRIES
            || self.order.len() > Self::MAX_ENTRIES.saturating_mul(2)
        {
            let Some((entry, generation)) = self.order.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&entry)
                .is_some_and(|(current, _)| *current == generation)
            {
                self.entries.remove(&entry);
            }
        }
    }

    /// Hold a segment's id until its transfer completes.
    pub(crate) fn segment(&mut self, transfer: SegmentTransfer, entry: [u8; 32]) {
        if !self.segments.contains_key(&transfer) && self.segments.len() >= Self::MAX_TRANSFERS {
            self.segments.clear();
        }
        self.segments.entry(transfer).or_default().push(entry);
    }

    /// The ids that together carried the mutation `last` completed: the
    /// held segments of its transfer (if it was one), plus `last`.
    pub(crate) fn complete(
        &mut self,
        transfer: Option<SegmentTransfer>,
        last: [u8; 32],
    ) -> Vec<[u8; 32]> {
        let mut entries = transfer
            .and_then(|transfer| self.segments.remove(&transfer))
            .unwrap_or_default();
        entries.push(last);
        entries
    }

    /// Whether `entry` is logged.
    #[cfg(test)]
    pub(crate) fn knows(&self, entry: &[u8; 32]) -> bool {
        self.entries.contains_key(entry)
    }

    /// The logged ops whose every change the saved document `snapshot`
    /// holds, oldest first.
    pub(crate) fn covered_by(&self, snapshot: &[u8]) -> Vec<[u8; 32]> {
        let Ok(doc) = automerge::AutoCommit::load(snapshot) else {
            return Vec::new();
        };
        let mut covered: Vec<(u64, [u8; 32])> = self
            .entries
            .iter()
            .filter(|(_, (_, required))| tilo_automerge::coverage::holds_all(&doc, required))
            .map(|(entry, (generation, _))| (*generation, *entry))
            .collect();
        covered.sort_unstable();
        covered.into_iter().map(|(_, entry)| entry).collect()
    }
}

/// The segmented transfer a document op belongs to, read from its frame
/// (endpoint plaintext on MLS routes).
fn segment_transfer(event: &CollabEvent) -> Option<SegmentTransfer> {
    let CollabEvent::Document {
        envelope,
        decrypted_payload,
        ..
    } = event
    else {
        return None;
    };
    match collab_types::DocumentFrame::decode(
        decrypted_payload.as_deref().unwrap_or(&envelope.payload),
    )
    .ok()?
    {
        collab_types::DocumentFrame::Segment(segment) => {
            Some((*envelope.author.as_bytes(), segment.digest))
        }
        collab_types::DocumentFrame::Whole(_) => None,
    }
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
    client: &runtime_collab::CollabClient,
    identity_bytes: &[u8],
    participant_handle: Option<&str>,
    participant_kind: &str,
    inbound_tx: &InboundBridge,
    routes: &mut std::collections::HashMap<String, SdkRoute>,
    spec: &RouteSpec,
    asks_channel_keys: bool,
) -> Result<(), String> {
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
        )
        .await
    {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(
                "collab: joining channel {channel_id} failed ({error}); route unavailable"
            );
            return Err(format!("joining collaboration route failed: {error}"));
        }
    };

    let encryption = std::sync::Arc::new(std::sync::Mutex::new(if asks_channel_keys {
        super::route_keys::RouteKeys::requested()
    } else {
        super::route_keys::RouteKeys::plain()
    }));
    let retained = std::sync::Arc::new(std::sync::Mutex::new(RetainedLog::default()));
    spawn_op_stream(
        &channel,
        inbound_tx,
        std::sync::Arc::clone(&encryption),
        std::sync::Arc::clone(&retained),
    )
    .await;
    spawn_context_stream(&channel, spec.clone(), inbound_tx).await;
    spawn_action_stream(&channel, inbound_tx).await;
    spawn_invocation_stream(&channel, inbound_tx).await;
    spawn_inspection_stream(&channel, inbound_tx).await;
    spawn_realtime_stream(&channel, inbound_tx).await;
    // This intentionally fails quietly for person sessions. An admitted
    // agent gets a separate broker-addressed queue; a document agent never
    // has to infer task work from room traffic.
    spawn_task_run_stream(&channel, inbound_tx).await;
    let presence = spawn_presence_stream(&channel, inbound_tx).await;
    // Subscriptions are installed first so an existing holder's response
    // cannot race ahead of our document stream.
    if let Err(error) = channel.request_document_snapshot().await {
        tracing::warn!(%error, "collab: late-join snapshot request failed");
    }

    routes.insert(
        channel_id,
        SdkRoute {
            spec: spec.clone(),
            channel,
            presence,
            workspace_controls: std::collections::HashMap::new(),
            encryption,
            retained,
        },
    );
    Ok(())
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
    retained: std::sync::Arc<std::sync::Mutex<RetainedLog>>,
) {
    if let Ok(stream) = channel.subscribe().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(event) = stream.read().await {
                let Some(complete) = inbound_document(
                    &event.op,
                    event.from_user_id,
                    &encryption,
                    &tx.reassembly.live,
                    &retained,
                ) else {
                    continue;
                };
                if !tx.send_document(complete).await {
                    break;
                }
            }
        });
    }
}

/// One op read from a route's stream, as the app receives it: decrypted,
/// held until its segmented transfer completes, and logged with the changes
/// it carries under every op id that carried it. `None` when there is
/// nothing to deliver yet (or the op is unusable).
fn inbound_document(
    op: &[u8],
    from_user_id: String,
    encryption: &SharedRouteKeys,
    reassembler: &std::sync::Mutex<collab_types::DocumentReassembler>,
    retained: &std::sync::Mutex<RetainedLog>,
) -> Option<CollabEvent> {
    let envelope = match postcard::from_bytes::<SignedOp>(op) {
        Ok(envelope) => envelope,
        Err(error) => {
            tracing::warn!("collab: signed op is not postcard SignedOp ({error}); dropping");
            return None;
        }
    };
    inbound_envelope(envelope, from_user_id, encryption, reassembler, retained)
}

/// [`inbound_document`] for a decoded envelope, including one that waited
/// on the route for its channel keys. An envelope that still cannot be
/// opened is held on the route again, silently.
fn inbound_envelope(
    envelope: SignedOp,
    from_user_id: String,
    encryption: &SharedRouteKeys,
    reassembler: &std::sync::Mutex<collab_types::DocumentReassembler>,
    retained: &std::sync::Mutex<RetainedLog>,
) -> Option<CollabEvent> {
    let entry = envelope.retained_entry_id();
    let translated = match signed_document_event(envelope, from_user_id, encryption) {
        Ok(event) => event,
        Err(UndeliveredDocument::Held) => return None,
        Err(error) => {
            tracing::warn!(%error, "collab: signed op has no typed document payload; dropping");
            return None;
        }
    };
    let transfer = segment_transfer(&translated);
    let Some(complete) = complete_document_event(translated, reassembler) else {
        if let (Some(transfer), Ok(mut log)) = (transfer, retained.lock()) {
            log.segment(transfer, entry);
        }
        return None;
    };
    if let (Some(Ok(mutation)), Ok(mut log)) = (complete.document_mutation(), retained.lock()) {
        let entries = log.complete(transfer, entry);
        log.record(&entries, mutation_bytes(&mutation));
    }
    Some(complete)
}

/// The Automerge bytes a typed document mutation carries.
fn mutation_bytes(mutation: &collab_types::DocumentMutation) -> &[u8] {
    match mutation {
        collab_types::DocumentMutation::Snapshot(data)
        | collab_types::DocumentMutation::Incremental(data) => data,
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
                if let Some(follow) = context.follow_changed {
                    let mode = if follow.replay {
                        crate::view::FollowMode::Replay
                    } else {
                        crate::view::FollowMode::Live
                    };
                    if !tx.send_durable(CollabEvent::FollowChanged {
                        peer_id: follow.target,
                        mode,
                    }) {
                        break;
                    }
                    continue;
                }
                let collaborators = context
                    .collaborators
                    .into_iter()
                    .map(|collaborator| Collaborator {
                        id: collaborator.user_id,
                        name: collaborator.display_name,
                        color: unpack_color(collaborator.color),
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
async fn spawn_inspection_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_inspections().await {
        let tx = inbound_tx.clone();
        let answer_channel = channel.clone();
        tokio::task::spawn_local(async move {
            while let Ok(inspection) = stream.read().await {
                match serde_json::from_str::<crate::inspect::InspectRequest>(
                    &inspection.request_json,
                ) {
                    Ok(request) => {
                        if !tx.send_durable(CollabEvent::Inspection {
                            id: inspection.id,
                            request: Box::new(request),
                        }) {
                            break;
                        }
                    }
                    // The session asked something this toolkit cannot read;
                    // it gets a refusal rather than silence.
                    Err(error) => {
                        let reply = crate::inspect::InspectReply::refused(format!(
                            "undecodable inspection request: {error}"
                        ));
                        if let Ok(json) = serde_json::to_string(&reply) {
                            let _ = answer_channel
                                .complete_inspection(inspection.id, &json)
                                .await;
                        }
                    }
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
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_invocations().await {
        let tx = inbound_tx.clone();
        let cancellation_channel = channel.clone();
        tokio::task::spawn_local(async move {
            while let Ok(invocation) = stream.read().await {
                let id = invocation.id;
                let context = parse_invocation_context(id, &invocation.context_json);
                if !tx.send_durable(CollabEvent::Invocation {
                    id,
                    resource: invocation.resource,
                    action: invocation.action,
                    argument: invocation.argument,
                    actor: invocation.actor,
                    context: context.map(Box::new),
                }) {
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
    op_class: collab_types::OpClass,
    bridge_state: &BridgeState,
) -> bool {
    let frames = match mutation.encode_frames() {
        Ok(frames) => frames,
        Err(error) => {
            // The size was checked before the change was queued, so this
            // is unreachable for SDK callers; never send a partial change.
            tracing::error!(%error, "collab: typed document mutation would not encode; dropping");
            return false;
        }
    };
    // A complete snapshot names every logged op its own bytes hold, so a
    // durable host may drop exactly those from its retained history. The
    // names ride on the first frame, which the host retains as the
    // document's checkpoint.
    let covers = match &mutation {
        collab_types::DocumentMutation::Snapshot(data) => route
            .retained
            .lock()
            .map(|log| log.covered_by(data))
            .unwrap_or_default(),
        collab_types::DocumentMutation::Incremental(_) => Vec::new(),
    };
    let count = frames.len();
    let mut sent = Vec::with_capacity(count);
    let mut held = false;
    for (index, frame) in frames.into_iter().enumerate() {
        let covering: &[[u8; 32]] = if index == 0 { &covers } else { &[] };
        match send_document_frame(route, signer, frame, op_class, covering, bridge_state).await {
            FrameSend::Sent(entry) => sent.push(entry),
            FrameSend::Held => held = true,
            FrameSend::Failed | FrameSend::Refused => {
                if count > 1 {
                    tracing::warn!(
                        segment = index,
                        segments = count,
                        "collab: segmented document transfer stopped; receivers will discard it"
                    );
                }
                return false;
            }
        }
    }
    // What this route sent is held by any later snapshot that holds its
    // changes, like what it received. A mutation that waited, even in part,
    // for channel keys is never claimed: a host then only retains more.
    if !held && let Ok(mut log) = route.retained.lock() {
        log.record(&sent, mutation_bytes(&mutation));
    }
    true
}

/// Encrypt, sign as `op_class` and send one typed document frame, naming
/// `covers` as the retained entries it holds, or hold it on the device while
/// the route waits for its channel keys. A sent frame reports its op's
/// retained entry id. A frame the broker refuses is sent once more after a
/// key refresh that found newer keys: a membership change moves the channel
/// to a new epoch, and the broker refuses content sealed under an older one.
#[allow(
    clippy::future_not_send,
    reason = "the route contains a capnp-rpc handle and runs only on the shovel LocalSet"
)]
async fn send_document_frame(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    cleartext: Vec<u8>,
    op_class: collab_types::OpClass,
    covers: &[[u8; 32]],
    bridge_state: &BridgeState,
) -> FrameSend {
    let first = try_send_frame(
        route,
        signer,
        cleartext.clone(),
        op_class,
        covers,
        bridge_state,
    );
    match first.await {
        FrameSend::Refused if refresh_route_keys(route, signer).await => {
            try_send_frame(route, signer, cleartext, op_class, covers, bridge_state).await
        }
        outcome => outcome,
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
    op_class: collab_types::OpClass,
    covers: &[[u8; 32]],
    bridge_state: &BridgeState,
) -> FrameSend {
    let workspace_id = route.spec.cluster_id.to_string();
    let record_id = route.spec.record.as_deref().unwrap_or("");
    let artifact = collab_types::document_artifact_id(&route.spec.app_id, &workspace_id, record_id);
    let (payload, encryption_metadata) =
        match prepare_document_payload(&route.encryption, artifact, cleartext, op_class, covers) {
            PreparedFrame::Ready(payload, metadata) => (payload, metadata),
            PreparedFrame::Held => return FrameSend::Held,
            PreparedFrame::Failed => return FrameSend::Failed,
        };
    let now = crate::time::wall();
    let author = collab_types::Ed25519PublicKey::from_bytes(signer_public_key(signer));
    let attestation =
        collab_types::document_attestation_id(&route.spec.app_id, &workspace_id, record_id);
    let seq = next_certified_document_seq(now);
    let mut op = match encryption_metadata {
        Some(metadata) => SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            author,
            attestation,
            op_class,
            now,
            seq,
            metadata,
        ),
        None => SignedOp::new_unsigned(payload, artifact, author, attestation, op_class, now, seq),
    };
    let unsigned_op = match postcard::to_stdvec(&op) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "collab: unsigned document envelope would not encode; dropping");
            return FrameSend::Failed;
        }
    };
    let signature = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => match identity
            .sign_collaboration_op(&workspace_id, &route.spec.app_id, record_id, &unsigned_op)
        {
            Ok(signature) => signature,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "collab: identity service denied document operation; dropping"
                );
                return FrameSend::Failed;
            }
        },
        LocalSdkDocumentSigner::Host(identity) => match identity
            .sign_collaboration_op(&workspace_id, &route.spec.app_id, record_id, &unsigned_op)
            .await
        {
            Ok(signature) => signature,
            Err(error) => {
                if !identity.is_usable() {
                    bridge_state.fail_closed("host identity signing transport");
                }
                tracing::warn!(
                    ?error,
                    "collab: host identity service denied document operation; dropping"
                );
                return FrameSend::Failed;
            }
        },
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(key)) => {
            use ed25519_dalek::Signer as _;
            let signing_bytes = match op.signing_bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "collab: document op would not canonicalize; dropping");
                    return FrameSend::Failed;
                }
            };
            key.sign(&signing_bytes).to_bytes()
        }
    };
    op.signature = collab_types::Signature::from_bytes(signature);
    match postcard::to_stdvec(&op) {
        Ok(bytes) => match route.channel.send_covering(&bytes, covers).await {
            Ok(()) => FrameSend::Sent(op.retained_entry_id()),
            Err(error) => {
                tracing::warn!(%error, "collab: broker refused document operation");
                FrameSend::Refused
            }
        },
        Err(error) => {
            tracing::warn!(%error, "collab: signed document envelope would not encode; dropping");
            FrameSend::Failed
        }
    }
}

/// The device key a document signer signs envelopes with.
fn signer_public_key(signer: &LocalSdkDocumentSigner) -> [u8; 32] {
    match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            identity.signing_public_key()
        }
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(key)) => {
            key.verifying_key().to_bytes()
        }
        LocalSdkDocumentSigner::Host(identity) => identity.signing_public_key(),
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
    op_class: collab_types::OpClass,
    covers: &[[u8; 32]],
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
            if keys.hold_outbound(super::route_keys::HeldFrame {
                cleartext,
                op_class,
                covers: covers.to_vec(),
            }) {
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
    /// Accepted by the broker, as the op with this retained entry id.
    Sent([u8; 32]),
    Held,
    /// Not sealed or not signed; retrying cannot help.
    Failed,
    /// The broker refused it, possibly because its keys were stale.
    Refused,
}

/// Ask the identity service for `route`'s current keys. Returns whether
/// the route's state or keys changed.
#[allow(
    clippy::future_not_send,
    reason = "the host identity transport is retained only on the shovel LocalSet"
)]
async fn refresh_route_keys(route: &SdkRoute, signer: &mut LocalSdkDocumentSigner) -> bool {
    let Ok(known) = route.encryption.lock().map(|keys| {
        keys.asks_identity_service()
            .then(|| (keys.generation(), keys.state()))
    }) else {
        return false;
    };
    let Some((generation, state)) = known else {
        return false;
    };
    let workspace_id = route.spec.cluster_id.to_string();
    let record_id = route.spec.record.as_deref().unwrap_or("");
    let reply = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            identity.channel_keys(&workspace_id, &route.spec.app_id, record_id, generation)
        }
        LocalSdkDocumentSigner::Host(identity) => {
            identity
                .channel_keys(&workspace_id, &route.spec.app_id, record_id, generation)
                .await
        }
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(_)) => return false,
    };
    let reply = match reply {
        Ok(reply) => reply,
        Err(error) => {
            tracing::debug!(?error, "collab: identity service refused channel keys");
            runtime_identity::wire::ChannelKeysReply {
                access: runtime_identity::wire::ChannelAccess::Unavailable,
                generation,
                keys: None,
            }
        }
    };
    let Ok(mut keys) = route.encryption.lock() else {
        return false;
    };
    keys.update(reply.access, reply.generation, reply.keys.as_deref());
    keys.generation() != generation || keys.state() != state
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
        let (state, outbound, inbound) = {
            let Ok(mut keys) = route.encryption.lock() else {
                continue;
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
            return;
        }
        for frame in outbound {
            let outcome = send_document_frame(
                route,
                signer,
                frame.cleartext.clone(),
                frame.op_class,
                &frame.covers,
                &inbound_tx.state,
            )
            .await;
            match outcome {
                FrameSend::Sent(entry) => {
                    // A whole mutation that waited is logged once sent; a
                    // segment is not, since its transfer spans frames.
                    if let Ok(collab_types::DocumentFrame::Whole(mutation)) =
                        collab_types::DocumentFrame::decode(&frame.cleartext)
                        && let Ok(mut log) = route.retained.lock()
                    {
                        log.record(&[entry], mutation_bytes(&mutation));
                    }
                }
                FrameSend::Held => {}
                FrameSend::Failed | FrameSend::Refused => {
                    tracing::warn!("collab: an edit that waited for channel access was not sent");
                }
            }
        }
        for (envelope, from_user_id) in inbound {
            if let Some(complete) = inbound_envelope(
                envelope,
                from_user_id,
                &route.encryption,
                &inbound_tx.reassembly.live,
                &route.retained,
            ) && !inbound_tx.send_document(complete).await
            {
                return;
            }
        }
    }
}

/// Shovel-thread Cap'n Proto endpoint for an application's pull directory.
/// It deliberately contains no `Widget`, `App`, or document reference.  The
/// only path to authority is a one-shot request answered by App's main-thread
/// pump below.
struct ActionDirectoryServer {
    requests: mpsc::SyncSender<ActionDirectoryRequest>,
}

fn directory_unavailable() -> runtime_collab::ActionDirectoryResolution {
    runtime_collab::ActionDirectoryResolution::Unavailable
}

fn write_directory_descriptor(
    mut out: runtime_collab::collab_capnp::exported_action::Builder<'_>,
    descriptor: &runtime_collab::ExportedAction,
) {
    out.set_resource(&descriptor.resource);
    out.set_action(&descriptor.action);
    out.set_resource_kind(&descriptor.resource_kind);
    out.set_resource_label(&descriptor.resource_label);
    out.set_resource_sensitivity(&descriptor.resource_sensitivity);
    out.set_action_label(&descriptor.action_label);
    out.set_description(descriptor.description.as_deref().unwrap_or(""));
    out.set_operation_kind(&descriptor.operation_kind);
    out.set_params_json(&descriptor.params_json);
    out.set_contract_json(&descriptor.contract_json);
    let mut input = out.init_input();
    match &descriptor.input {
        runtime_collab::ExportedInput::None => input.set_none(()),
        runtime_collab::ExportedInput::Text {
            prompt,
            placeholder,
            wants_content,
        } => {
            let mut text = input.init_text();
            text.set_prompt(prompt);
            text.set_placeholder(placeholder);
            text.set_wants_content(*wants_content);
        }
    }
}

fn write_directory_row(
    mut out: runtime_collab::collab_capnp::action_directory_row::Builder<'_>,
    row: &runtime_collab::ActionDirectoryRow,
) {
    out.set_parent_resource(&row.parent_resource);
    out.set_resource(&row.resource);
    out.set_resource_kind(&row.resource_kind);
    out.set_resource_label(&row.resource_label);
    out.set_resource_sensitivity(&row.resource_sensitivity);
    out.set_resource_presentation_json(&row.resource_presentation_json);
    out.set_action(row.action.as_deref().unwrap_or(""));
    out.set_has_children(row.has_children);
    out.set_visibility_action(row.visibility_action.as_deref().unwrap_or(""));
    out.set_visibility_contract_json(&row.visibility_contract_json);
    if let Some(descriptor) = &row.descriptor {
        write_directory_descriptor(out.reborrow().init_descriptor(), descriptor);
    }
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
        capnp::capability::Promise::from_future(async move {
            let (reply, response) = tokio::sync::oneshot::channel();
            requests
                .try_send(ActionDirectoryRequest::Query { query, reply })
                .map_err(|_| {
                    capnp::Error::failed(
                        "application action directory is busy or unavailable".into(),
                    )
                })?;
            let page = tokio::time::timeout(ACTION_DIRECTORY_REPLY_TIMEOUT, response)
                .await
                .map_err(|_| capnp::Error::failed("application action directory timed out".into()))?
                .map_err(|_| {
                    capnp::Error::failed("application action directory is unavailable".into())
                })?;
            let mut out = results.get().init_page();
            out.set_revision(&page.revision);
            out.set_next_cursor(&page.next_cursor);
            out.set_exhausted(page.exhausted);
            out.set_stale_cursor(page.stale_cursor);
            out.set_budget_exhausted(page.budget_exhausted);
            let len = u32::try_from(page.rows.len())
                .map_err(|_| capnp::Error::failed("too many directory rows".into()))?;
            let mut rows = out.reborrow().init_rows(len);
            for (index, row) in page.rows.iter().enumerate() {
                write_directory_row(
                    rows.reborrow()
                        .get(u32::try_from(index).unwrap_or(u32::MAX)),
                    row,
                );
            }
            Ok(())
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
        capnp::capability::Promise::from_future(async move {
            let (reply, response) = tokio::sync::oneshot::channel();
            let reply_value = if requests
                .try_send(ActionDirectoryRequest::Resolve {
                    revision,
                    resource,
                    action,
                    reply,
                })
                .is_err()
            {
                directory_unavailable()
            } else {
                tokio::time::timeout(ACTION_DIRECTORY_REPLY_TIMEOUT, response)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_else(directory_unavailable)
            };
            let mut out = results.get().init_resolution();
            match reply_value {
                runtime_collab::ActionDirectoryResolution::Found(row) => {
                    write_directory_row(out.reborrow().init_found(), &row);
                }
                runtime_collab::ActionDirectoryResolution::NotFound => out.set_not_found(()),
                runtime_collab::ActionDirectoryResolution::StaleRevision => {
                    out.set_stale_revision(());
                }
                runtime_collab::ActionDirectoryResolution::Unavailable => out.set_unavailable(()),
            }
            Ok(())
        })
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
    match request {
        CollabRequest::Hello { .. } => {}
        CollabRequest::SignedOperation { envelope } => {
            if let Ok(bytes) = postcard::to_stdvec(&*envelope) {
                let _ = route.channel.send(&bytes).await;
            }
        }
        CollabRequest::Document { mutation } => {
            let class = collab_types::document_op_class_for_record(
                route.spec.record.as_deref().unwrap_or(""),
            );
            return send_document_mutation(route, signer, mutation, class, &inbound_tx.state).await;
        }
        CollabRequest::CommentReferences { delta } => {
            // Only the room index takes comment references; on any other
            // route the class would be refused by every receiver anyway.
            if route.spec.record.is_some() {
                tracing::warn!("collab: comment references belong on the room route; dropping");
                return true;
            }
            return send_document_mutation(
                route,
                signer,
                DocumentMutation::Incremental(delta),
                collab_types::OpClass::Comment,
                &inbound_tx.state,
            )
            .await;
        }
        CollabRequest::RequestDocumentSnapshot => {
            let _ = route.channel.request_document_snapshot().await;
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
            let actions = actions
                .into_iter()
                .map(|action| runtime_collab::ExportedAction {
                    resource: action.resource,
                    action: action.action,
                    resource_kind: action.resource_kind,
                    resource_label: action.resource_label,
                    resource_sensitivity: action.resource_sensitivity.as_wire().to_owned(),
                    action_label: action.action_label,
                    description: action.description,
                    operation_kind: action.operation_kind,
                    params_json: action.params_json,
                    contract_json: action.contract_json,
                    view: action.view.map(|view| runtime_collab::ExportedView {
                        id: view.id,
                        label: view.label,
                        description: view.description,
                        open_action: view.open_action,
                        primary: view.primary,
                        mount: view.mount.map(|mount| runtime_collab::ExportedViewMount {
                            action: mount.action,
                            capability_param: mount.capability_param,
                            view_param: mount.view_param,
                        }),
                    }),
                    input: match action.input {
                        ExportedInput::None => runtime_collab::ExportedInput::None,
                        ExportedInput::Text {
                            prompt,
                            placeholder,
                            wants_content,
                        } => runtime_collab::ExportedInput::Text {
                            prompt,
                            placeholder,
                            wants_content,
                        },
                    },
                })
                .collect::<Vec<_>>();
            if let Err(error) = route.channel.publish_actions(&app_id, &actions).await {
                tracing::warn!(%error, "collab: publishing resource/action surface failed");
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
        CollabRequest::ReportPlayback {
            app_id,
            reports_json,
        } => {
            if let Err(error) = route.channel.report_playback(&app_id, &reports_json).await {
                tracing::warn!(%error, "collab: reporting private playback handoff state failed");
            }
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
            }
        }
        CollabRequest::CompleteInspection { id, reply } => match serde_json::to_string(&reply) {
            Ok(json) => {
                if let Err(error) = route.channel.complete_inspection(id, &json).await {
                    tracing::warn!(%error, id, "collab: answering an inspection failed");
                }
            }
            Err(error) => tracing::warn!(%error, id, "collab: encoding an inspection failed"),
        },
        CollabRequest::ReportSemanticChange { generation } => {
            if let Err(error) = route.channel.report_semantic_change(generation).await {
                tracing::debug!(%error, generation, "collab: reporting a semantic change failed");
            }
        }
        CollabRequest::UpdateInvocation { id, update } => match serde_json::to_string(&update) {
            Ok(json) => {
                if let Err(error) = route.channel.update_invocation(id, &json).await {
                    tracing::warn!(%error, id, "collab: updating action invocation failed");
                }
            }
            Err(error) => tracing::warn!(%error, id, "collab: encoding action update failed"),
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
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "collab: encoding local effect receipt failed");
                }
            }
        }
        CollabRequest::RemoteAction { message } => {
            let answer = remote_action_round_trip(&route.channel, message).await;
            let _ = inbound_tx.send_durable(CollabEvent::RemoteAction { message: answer });
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
            let result = route
                .channel
                .invoke_task_action(
                    run_id,
                    &lease,
                    peer_id,
                    &resource,
                    &action,
                    Some(&argument),
                    "",
                    &invocation.0.to_string(),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::TaskActionInvocationResult { request_id, result });
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

/// Unpack the SDK's 0x00RRGGBB u32 into [R, G, B] floats in 0..1 — the
/// shape the legacy dialect already used, so existing consumers render
/// identically.
#[allow(
    clippy::cast_precision_loss,
    reason = "each channel is masked to 0..=255 by `& 0xFF`, always exactly representable in f32"
)]
fn unpack_color(packed: u32) -> [f32; 3] {
    let r = ((packed >> 16) & 0xFF) as f32 / 255.0;
    let g = ((packed >> 8) & 0xFF) as f32 / 255.0;
    let b = (packed & 0xFF) as f32 / 255.0;
    [r, g, b]
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
    #![allow(refining_impl_trait)]

    use super::*;
    use crate::collaboration::{CollaborationState, SignedSendError, SigningIdentity};

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

    use crate::time::Duration;
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
        channel_context, channel_handle, collab_service, context_stream, presence_event,
        presence_handle, presence_payload, presence_stream, scrollback_batch, stream,
    };

    #[derive(Default)]
    struct Captures {
        joined: Vec<(String, Vec<u8>)>,
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
        action_directories: Vec<String>,
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
            let mut r = results.get();
            r.set_op(&op);
            r.set_from_user_id(&from);
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
            self.captures
                .lock()
                .unwrap()
                .action_publications
                .push((app_id, actions));
            Promise::ok(())
        }

        fn publish_action_directory(
            self: Rc<Self>,
            params: channel_handle::PublishActionDirectoryParams,
            _results: channel_handle::PublishActionDirectoryResults,
        ) -> Promise<(), capnp::Error> {
            let request = params.get().unwrap();
            self.captures
                .lock()
                .unwrap()
                .action_directories
                .push(request.get_app_id().unwrap().to_str().unwrap().to_owned());
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
            self.captures.lock().unwrap().signed_ops.push(op);
            Promise::ok(())
        }

        fn subscribe(
            self: Rc<Self>,
            _params: channel_handle::SubscribeParams,
            mut results: channel_handle::SubscribeResults,
        ) -> Promise<(), capnp::Error> {
            let client: stream::Client = capnp_rpc::new_client(FakeOpStream {
                backlog: self.op_backlog.clone(),
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
            self.captures
                .lock()
                .unwrap()
                .joined
                .push((channel_id, identity));

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
    fn host_connector_failure_is_consumed_once_without_inherited_fallback() {
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
        started_rx.recv_timeout(HANG_GUARD).unwrap();
        let result = result_rx
            .recv_timeout(SHOVEL_INIT_TIMEOUT * 2)
            .expect("a pending host handoff must not hang its caller");
        caller.join().unwrap();
        match result {
            Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
            }
            _ => panic!("a pending handoff cannot produce a connected session"),
        }
        dropped_rx
            .recv_timeout(HANG_GUARD)
            .expect("timed-out async handoff must release its owned resources");
    }

    #[test]
    fn host_connector_without_certificate_cannot_use_harness_identity() {
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

    #[test]
    fn host_connector_join_proof_refusal_reaches_caller() {
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

    #[test]
    fn host_connector_uses_certified_presentation_and_broker_document_policy() {
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
        let requests = requests_rx.recv_timeout(HANG_GUARD).unwrap();
        manager.send(CollabRequest::document_incremental(
            b"fixture document change".to_vec(),
        ));
        let is_op = |request: &runtime_identity::wire::Request| {
            matches!(
                request,
                runtime_identity::wire::Request::CollaborationOp { .. }
            )
        };
        assert!(wait_until(
            || host_identity_requests(&requests.lock().unwrap())
                .iter()
                .any(is_op),
            HANG_GUARD
        ));
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
            let (client, server) = tokio::io::duplex(64 * 1024);
            let broker_client: collab_service::Client = capnp_rpc::new_client(gate);
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
        (connector, HostJoinRelease(Some(release)))
    }

    #[test]
    fn host_connector_refused_primary_join_is_not_connected_success() {
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
            attempt_rx.recv_timeout(HANG_GUARD).unwrap(),
            channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            attempt_rx.try_recv().is_err(),
            "must not retry another route"
        );
        stop_rx
            .recv_timeout(HANG_GUARD)
            .expect("fixture broker stops after failed manager");
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
        let result = result_rx.recv_timeout(SHOVEL_INIT_TIMEOUT * 2 + Duration::from_secs(1));
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
            panic!("manager failed to return within the bounded startup window");
        }
        caller.join().expect("bounded fixture caller");
        match result.expect("manager must return within the bounded startup window") {
            Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
                assert_eq!(error.to_string(), "collaboration primary route timed out");
            }
            _ => panic!("unacknowledged Hello must never return a connected manager"),
        }
        assert!(
            elapsed >= SDK_PRIMARY_ROUTE_TIMEOUT,
            "must wait for the worker-side route deadline"
        );
        assert_eq!(
            attempt_rx.recv_timeout(HANG_GUARD).unwrap(),
            channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            attempt_rx.try_recv().is_err(),
            "must not retry another route"
        );
        stop_rx
            .recv_timeout(HANG_GUARD)
            .expect("timed-out primary join must cancel and close the fixture broker");
        drop(release);
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
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _, _thread) = stand_up_fake_broker(broker);
        CollaborationManager::from_sdk_fd_for_test(fd, "lifecycle-test", Uuid::new_v4())
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

    /// Upper bound for waits on the fake broker. Every wait returns as soon
    /// as its condition holds; the guard only turns a lost message into a
    /// failure instead of a hang, so it can be generous without slowing a
    /// passing run.
    const HANG_GUARD: Duration = Duration::from_secs(30);

    /// Poll until the events received so far satisfy `done`, or
    /// [`HANG_GUARD`] passes. Returns everything received either way, so the
    /// caller's assertions report what did arrive.
    fn poll_until(
        mgr: &mut CollaborationManager,
        mut done: impl FnMut(&[CollabEvent]) -> bool,
    ) -> Vec<CollabEvent> {
        let start = crate::time::now();
        let mut events = Vec::new();
        loop {
            events.extend(mgr.poll());
            if done(&events) || start.elapsed() >= HANG_GUARD {
                return events;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_until<F: FnMut() -> bool>(mut cond: F, deadline: Duration) -> bool {
        let start = crate::time::now();
        while start.elapsed() < deadline {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn saturated_outbound_bridge_sheds_only_presence_then_fails_closed() {
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

    #[test]
    fn inbound_events_wake_the_owner_once_per_queued_burst() {
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

    #[test]
    fn saturated_inbound_bridge_preserves_presence_policy_and_fails_for_results() {
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

    #[test]
    fn over_limit_document_is_refused_explicitly_without_disconnecting() {
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

    #[test]
    fn document_budget_is_charged_for_bytes_sent_not_spare_capacity() {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let mut data = Vec::with_capacity(SDK_DOCUMENT_COMMAND_BYTES * 2);
        data.push(0);
        let command = SdkCommand::Primary(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(data),
        });

        assert!(enqueue_sdk_command(&tx, command, &state));
        assert!(
            state.document_command_bytes.0.load(Ordering::Acquire) < 1024 * 1024,
            "a mostly-empty allocation is shrunk before it is parked on the bridge"
        );
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn document_budget_defers_aggregate_documents_without_disconnecting() {
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

    #[test]
    fn a_lone_document_larger_than_the_budget_still_makes_progress() {
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

    #[test]
    fn byte_reservation_releases_when_the_ui_consumes_a_command() {
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

    #[test]
    fn oversized_inbound_action_fails_before_queueing() {
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

    #[test]
    #[cfg(unix)]
    fn a_broker_that_is_not_there_does_not_hold_the_app_up() {
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
        let deadline = crate::time::now() + SHOVEL_INIT_TIMEOUT * 3;
        loop {
            let _ = mgr.poll();
            let state = mgr.state();
            if state != CollaborationState::Connecting {
                return state;
            }
            let left = deadline.saturating_duration_since(crate::time::now());
            wakes
                .recv_timeout(left)
                .expect("a connecting manager wakes its owner when it settles");
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
        )
    }

    #[test]
    #[cfg(unix)]
    fn construction_returns_at_once_while_the_broker_is_silent() {
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

    #[test]
    #[cfg(unix)]
    fn a_background_connection_becomes_ready_and_wakes_the_owner() {
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

    #[test]
    #[cfg(unix)]
    fn explicit_test_join_forwards_its_identity_presentation() {
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

        assert!(
            wait_until(|| !captures.lock().unwrap().joined.is_empty(), HANG_GUARD,),
            "join_channel must be observed"
        );
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].1, presentation);
    }

    #[test]
    #[cfg(unix)]
    fn first_frame_publishes_actions_without_input() {
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
            Box::new(crate::widget::Button::new(
                "complete",
                "Complete",
                Some("completed"),
            )),
            manager,
        );
        assert!(captures.lock().unwrap().action_publications.is_empty());
        // This calls the production layout/draw passes. No input, palette,
        // explicit publication, or app-owned action invocation primes the app.
        crate::app::draw_prepared_component_test_frame(&mut app);
        assert!(
            wait_until(
                || {
                    let seen = captures.lock().unwrap();
                    seen.action_directories.iter().any(|id| id == &app_id)
                        && seen.action_publications.iter().any(|(id, actions)| {
                            id == &app_id
                                && actions.iter().any(|(label, action)| {
                                    label == "Complete" && action == "activate"
                                })
                        })
                },
                Duration::from_secs(2)
            ),
            "the first rendered frame must publish its semantic actions to the broker without input"
        );
    }

    #[test]
    #[cfg(unix)]
    fn hello_joins_channel_and_send_forwards_signed_op() {
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

        assert!(
            wait_until(|| !captures.lock().unwrap().joined.is_empty(), HANG_GUARD),
            "join_channel must be observed"
        );
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

        assert!(
            wait_until(
                || !captures.lock().unwrap().signed_ops.is_empty(),
                HANG_GUARD,
            ),
            "send() must reach the SDK",
        );
        let seen = captures.lock().unwrap().signed_ops.clone();
        assert_eq!(seen, vec![expected_bytes]);
    }

    #[cfg(unix)]
    #[test]
    fn owned_route_raw_documents_use_typed_signed_sdk_transport() {
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
        manager.send_to_route(
            route.clone(),
            CollabRequest::document_snapshot(snapshot.clone()),
        );
        manager.send_to_route(
            route.clone(),
            CollabRequest::document_incremental(delta.clone()),
        );
        assert!(
            wait_until(
                || captures.lock().unwrap().signed_ops.len() == 2,
                HANG_GUARD
            ),
            "both raw requests reach SDK signed RPC"
        );
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

    /// `send` only queues. A publication barrier resolves once every
    /// document mutation queued before it has reached the broker, which is
    /// what lets an owner end its session without discarding a change still
    /// in transit.
    #[cfg(unix)]
    #[test]
    fn publication_barrier_resolves_after_every_earlier_document_is_published() {
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "shell", Uuid::new_v4());
        for n in 0..8_u8 {
            assert!(manager.send(CollabRequest::document_incremental(vec![n; 16])));
        }
        manager
            .begin_document_publication_barrier()
            .expect("the broker transport supports publication barriers")
            .wait(Some(crate::time::now() + HANG_GUARD))
            .expect("every queued document was published");
        assert_eq!(
            captures.lock().unwrap().signed_ops.len(),
            8,
            "the barrier resolved before the documents ahead of it reached the broker"
        );
    }

    /// Every presence variant reaches the SDK method that keeps its key —
    /// a pointer publishes keyed, like a caret, rather than through
    /// `publish_cursor(x, y)`, which has nowhere to put one.
    #[test]
    #[cfg(unix)]
    fn presence_variants_map_to_correct_sdk_publishes() {
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        assert!(
            wait_until(|| !captures.lock().unwrap().joined.is_empty(), HANG_GUARD),
            "join_channel must be observed",
        );

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

        assert!(
            wait_until(
                || {
                    let c = captures.lock().unwrap();
                    c.pointer_publishes.len() == 1
                        && c.caret_publishes.len() == 1
                        && c.selection_publishes.len() == 1
                        && c.typing_publishes.len() == 1
                        && c.arrived.len() == 1
                        && c.left.len() == 1
                },
                HANG_GUARD,
            ),
            "every presence variant must reach the corresponding SDK method",
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

    #[test]
    fn signing_identity_refuses_sequence_reuse_at_u64_boundary() {
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

    #[test]
    #[cfg(unix)]
    fn signed_op_stream_translates_to_signed_crdt_incremental() {
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

    #[test]
    fn signed_document_event_preserves_snapshot_kind() {
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
        let event = signed_document_event(op, "peer-alice".into(), &plain_route_keys()).unwrap();
        assert!(matches!(
            &event,
            CollabEvent::Document { from_user_id, .. } if from_user_id == "peer-alice"
        ));
        assert!(matches!(
            event.document_mutation(),
            Some(Ok(DocumentMutation::Snapshot(data))) if data == b"whole document"
        ));
    }

    #[test]
    fn signed_document_event_decrypts_content_keys_for_history_and_late_joiners() {
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
        let PreparedFrame::Ready(ciphertext, metadata) = prepare_document_payload(
            &alice_endpoint,
            artifact,
            plaintext.clone(),
            collab_types::OpClass::Splice,
            &[],
        ) else {
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

    #[test]
    fn snapshots_are_sealed_as_retained_checkpoints() {
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

    #[test]
    #[cfg(unix)]
    fn context_stream_translates_to_context_event() {
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![SeedContext {
                collaborators: vec![
                    ("alice".to_owned(), "Alice".to_owned(), 0x00FF_8000),
                    ("bob".to_owned(), "Bob".to_owned(), 0x0000_80FF),
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
        // 0x00FF8000 → R=255, G=128, B=0.
        assert!((collabs[0].color[0] - 1.0).abs() < 1e-3);
        assert!((collabs[0].color[1] - 128.0 / 255.0).abs() < 1e-3);
        assert!(collabs[0].color[2].abs() < 1e-3);
        assert_eq!(perms, vec!["edit".to_owned(), "share".to_owned()]);
    }

    #[test]
    #[cfg(unix)]
    fn presence_stream_translates_cursor_and_all_other_variants() {
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

    #[test]
    #[cfg(unix)]
    fn fetch_scrollback_returns_batch_event() {
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
        assert!(
            wait_until(|| !captures.lock().unwrap().joined.is_empty(), HANG_GUARD),
            "join_channel must precede scrollback fetch",
        );

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

    /// A member added after the history was written fetches it through the
    /// real SDK transport and decrypts every page with the key history it
    /// received on joining — no other member is running.
    #[test]
    fn joiner_decrypts_fetched_history_with_no_other_member_online() {
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
        assert!(wait_until(
            || !captures.lock().unwrap().joined.is_empty(),
            HANG_GUARD
        ));
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

    #[test]
    fn translate_presence_event_is_symmetric_over_all_payload_variants() {
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

    #[test]
    fn invocation_context_json_is_optional_and_fail_closed() {
        let context = tilo_actions::ActionContext::default()
            .with_purpose("Review the proposal")
            .with_group("task:review", "Review", "task");
        let json = serde_json::to_string(&context).unwrap();
        assert_eq!(parse_invocation_context(7, &json), Some(context));
        assert_eq!(parse_invocation_context(8, "  "), None);
        assert_eq!(parse_invocation_context(9, "{not-json"), None);
    }

    /// A snapshot names exactly the logged ops its saved bytes hold — not
    /// the ones that arrived after it was saved, and not a segmented
    /// mutation's segments until the whole mutation is held.
    #[test]
    fn a_snapshot_names_exactly_the_logged_ops_it_holds() {
        use automerge::transaction::Transactable;
        let mut doc = automerge::AutoCommit::new();
        let mut log = RetainedLog::default();
        doc.put(automerge::ROOT, "a", 1).unwrap();
        log.record(&[[1; 32]], &doc.save_incremental());
        let transfer = ([9; 32], [8; 32]);
        log.segment(transfer, [2; 32]);
        doc.put(automerge::ROOT, "b", 2).unwrap();
        let second = doc.save_incremental();
        let entries = log.complete(Some(transfer), [3; 32]);
        log.record(&entries, &second);
        let saved = doc.save();
        doc.put(automerge::ROOT, "c", 3).unwrap();
        log.record(&[[4; 32]], &doc.save_incremental());
        log.record(&[[5; 32]], b"not automerge");

        assert_eq!(log.covered_by(&saved), vec![[1; 32], [2; 32], [3; 32]]);
        assert_eq!(
            log.covered_by(&doc.save()),
            vec![[1; 32], [2; 32], [3; 32], [4; 32]]
        );
    }

    /// An app restarts with an empty retained log. Its own earlier op,
    /// replayed back to it by the host, is logged again, so its next
    /// snapshot names that entry as covered and the host can trim it.
    #[test]
    fn a_restarted_app_relearns_its_own_old_entry_ids() {
        use automerge::transaction::Transactable;
        let (app_id, cluster, record) = ("so.tilo.chat", "room", "fb");
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "remark", "mine").unwrap();
        let frame = collab_types::DocumentMutation::Incremental(doc.save_incremental())
            .encode_frames()
            .unwrap()
            .remove(0);
        let own = collab_types::SignedOp::new_unsigned(
            frame,
            collab_types::document_artifact_id(app_id, cluster, record),
            collab_types::Ed25519PublicKey::from_bytes([3; 32]),
            collab_types::document_attestation_id(app_id, cluster, record),
            collab_types::OpClass::Comment,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        );
        // After the restart: nothing logged, then the replay arrives.
        let retained = std::sync::Mutex::new(RetainedLog::default());
        assert!(!retained.lock().unwrap().knows(&own.retained_entry_id()));
        let delivered = inbound_document(
            &postcard::to_stdvec(&own).unwrap(),
            "self".into(),
            &plain_route_keys(),
            &std::sync::Mutex::new(collab_types::DocumentReassembler::default()),
            &retained,
        );
        assert!(delivered.is_some());
        let log = retained.lock().unwrap();
        assert!(log.knows(&own.retained_entry_id()));
        assert_eq!(log.covered_by(&doc.save()), vec![own.retained_entry_id()]);
    }

    /// An id seen again (a host replay) becomes the newest: the bound ages
    /// out ids a host no longer retains before the ones it still does.
    #[test]
    fn a_replayed_id_is_the_last_to_age_out() {
        let mut log = RetainedLog::default();
        let bytes = {
            use automerge::transaction::Transactable;
            let mut doc = automerge::AutoCommit::new();
            doc.put(automerge::ROOT, "a", 1).unwrap();
            doc.save_incremental()
        };
        let id = |n: usize| {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            id
        };
        log.record(&[id(0)], &bytes);
        for n in 1..RetainedLog::MAX_ENTRIES {
            log.record(&[id(n)], &bytes);
        }
        // Replayed: the oldest id is refreshed before the bound is hit.
        log.record(&[id(0)], &bytes);
        log.record(&[id(RetainedLog::MAX_ENTRIES)], &bytes);
        assert!(log.knows(&id(0)), "the replayed id survives");
        assert!(!log.knows(&id(1)), "the oldest unreplayed id aged out");
    }

    /// A segmented snapshot whose segments reach the app interleaved with
    /// other remarks — the order a host replays a checkpoint and a tail it
    /// only partly covers in — is reassembled and applied: every remark and
    /// the snapshot's content are held, no change waits for a dependency,
    /// and each id is logged under the mutation it carried.
    #[test]
    fn a_segmented_snapshot_interleaved_with_remarks_reassembles_and_applies() {
        use automerge::transaction::Transactable;
        let (app_id, cluster, record) = ("so.tilo.chat", "room", "fb");
        let artifact = collab_types::document_artifact_id(app_id, cluster, record);
        let attestation = collab_types::document_attestation_id(app_id, cluster, record);
        let op = |author: [u8; 32], payload: Vec<u8>, seq: u64| {
            collab_types::SignedOp::new_unsigned(
                payload,
                artifact,
                collab_types::Ed25519PublicKey::from_bytes(author),
                attestation,
                collab_types::OpClass::Splice,
                std::time::SystemTime::UNIX_EPOCH,
                seq,
            )
        };

        // The snapshot: large enough to travel in several segments.
        let mut holder = automerge::AutoCommit::new();
        holder.set_actor(automerge::ActorId::from(b"holder".as_slice()));
        // Incompressible, so the saved document stays several segments long.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let noise: Vec<u8> = (0..3 * collab_types::DOCUMENT_SEGMENT_BYTES)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect();
        holder.put(automerge::ROOT, "attachment", noise).unwrap();
        let snapshot = holder.save();
        let segments = collab_types::DocumentMutation::Snapshot(snapshot)
            .encode_frames()
            .unwrap();
        assert!(segments.len() >= 3, "the snapshot is segmented");

        // Remarks from another member, each depending on the one before.
        let mut member = automerge::AutoCommit::new();
        member.set_actor(automerge::ActorId::from(b"member".as_slice()));
        let mut remarks = Vec::new();
        for n in 0..segments.len() {
            member
                .put(
                    automerge::ROOT,
                    format!("remark-{n}"),
                    i64::try_from(n).unwrap(),
                )
                .unwrap();
            let frames = collab_types::DocumentMutation::Incremental(member.save_incremental())
                .encode_frames()
                .unwrap();
            remarks.push(op(
                [2; 32],
                frames.into_iter().next().unwrap(),
                100 + n as u64,
            ));
        }
        let segment_ops: Vec<_> = segments
            .into_iter()
            .enumerate()
            .map(|(index, frame)| op([1; 32], frame, 1 + index as u64))
            .collect();

        let encryption = plain_route_keys();
        let reassembler = std::sync::Mutex::new(collab_types::DocumentReassembler::default());
        let retained = std::sync::Mutex::new(RetainedLog::default());
        let mut joiner = automerge::AutoCommit::new();
        let mut delivered = 0;
        for (segment, remark) in segment_ops.iter().zip(&remarks) {
            for signed in [segment, remark] {
                let Some(event) = inbound_document(
                    &postcard::to_stdvec(signed).unwrap(),
                    "peer".into(),
                    &encryption,
                    &reassembler,
                    &retained,
                ) else {
                    continue;
                };
                delivered += 1;
                let mutation = event.document_mutation().unwrap().unwrap();
                tilo_automerge::ApplyIncoming::load_incoming(
                    &mut joiner,
                    mutation_bytes(&mutation),
                )
                .unwrap();
            }
        }
        assert_eq!(
            delivered,
            remarks.len() + 1,
            "every remark, then the snapshot once whole"
        );
        assert!(
            joiner.get_missing_deps(&[]).is_empty(),
            "no change waits for a dependency"
        );
        let mut expected = holder.clone();
        tilo_automerge::ApplyIncoming::merge_incoming(&mut expected, &mut member).unwrap();
        assert_eq!(joiner.get_heads(), expected.get_heads(), "nothing was lost");

        // Every segment id, and every remark id, is held by what it carried.
        let log = retained.lock().unwrap();
        let covered = log.covered_by(&expected.save());
        for signed in segment_ops.iter().chain(&remarks) {
            assert!(covered.contains(&signed.retained_entry_id()));
        }
        let only_snapshot = log.covered_by(&holder.save());
        assert!(
            segment_ops
                .iter()
                .all(|signed| only_snapshot.contains(&signed.retained_entry_id()))
        );
        assert!(
            remarks
                .iter()
                .all(|signed| !only_snapshot.contains(&signed.retained_entry_id())),
            "a remark is not attributed to the snapshot it interleaved with"
        );
    }
}
