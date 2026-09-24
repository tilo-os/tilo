//! Toolkit-side collab client: a synchronous, non-blocking `send` / `poll`
//! pair trading typed [`CollabRequest`] / [`CollabEvent`] frames, stable so
//! every downstream app keeps compiling while the transport underneath
//! changes.
//!
//! The manager delegates to the `runtime-collab` Cap'n Proto SDK on a
//! shovel thread it owns, bridging winit-side `send` / `poll` to the SDK
//! over a pair of bounded mpsc channels. Durable traffic fails the bridge
//! closed on overload; only transient presence may be shed. On
//! [`runtime_portal::NotAvailable`] it enters "disabled" mode instead of
//! panicking — `poll()` returns empty, `send()` is a no-op, no thread is
//! spawned.
//!

mod committed_document;
mod directory_client;
mod managed_sender_transport;
pub(crate) mod native_initial;
mod native_media;
pub use committed_document::CommittedDocument;
mod peer;
pub use peer::PeerTransport;

use uuid::Uuid;

/// Hard fail-closed timeout for an RPC request waiting to be handled on the
/// application's owner thread.  The UI never blocks for it: the Cap'n Proto
/// shovel awaits this reply on its own `LocalSet`.  A stopped app must look
/// unavailable to the broker rather than leaving an RPC worker pinned.
const ACTION_DIRECTORY_REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// A disconnected/slow UI must apply backpressure to broker callbacks rather
/// than allow an unbounded cross-thread queue to become a memory `DoS`.
const ACTION_DIRECTORY_REQUEST_QUEUE: usize = 64;

/// Reserved `invokePublishedAction.peerId` meaning "the application
/// connection making this request". Live broker peer ids are allocated from
/// zero upward; the broker never assigns this sentinel.
pub const CURRENT_CONNECTION_PEER_ID: u64 = u64::MAX;

pub use collab_types::DocumentMutation;
use collab_types::SignedOp;
pub use runtime_collab::{
    NearbyDestinationHandle, NearbyTargetHandle, NearbyTransferCommand, NearbyTransferDestination,
    NearbyTransferDirection, NearbyTransferHandle, NearbyTransferPhase, NearbyTransferReply,
    NearbyTransferSnapshot, NearbyTransferStatus, NearbyTransferTarget, PortableWifiSecurity,
    WifiContinuityCommand, WifiContinuityReply,
};

use crate::presence::PresenceKey;

use crate::clock::Duration;
use runtime_portal::RawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::LocalSet;

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
/// partial and late replies without blocking the shovel's LocalSet.
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
const WIFI_CONTINUITY_TIMEOUT: Duration = Duration::from_secs(3);

static WIFI_CONTINUITY_BRIDGE: std::sync::OnceLock<
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
    let sender = WIFI_CONTINUITY_BRIDGE
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

/// Inert, app-owned resume projection sent only to the local shell.
///
/// The bytes are deliberately opaque to the toolkit so each app can version
/// its CRDT-derived schema. The shell validates size and path redaction before
/// retaining them for same-identity continuation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationMetadata {
    pub schema_version: u16,
    pub bytes: Vec<u8>,
}

/// Decode the launcher-only resume payload. Invalid input is ignored so an
/// app outside Tilo or an older sender opens normally.
pub fn continuation_restore_env() -> Option<ContinuationMetadata> {
    let bytes = hex::decode(std::env::var("TILO_CONTINUITY_RESUME_HEX").ok()?).ok()?;
    let schema_version = std::env::var("TILO_CONTINUITY_RESUME_SCHEMA")
        .ok()?
        .parse()
        .ok()?;
    (!bytes.is_empty() && bytes.len() <= 32 * 1024).then_some(ContinuationMetadata {
        schema_version,
        bytes,
    })
}

#[derive(Debug, Clone)]
pub enum CollabRequest {
    Hello {
        app_id: String,
        cluster_id: Uuid,
        /// Which **record** ([`crate::record`]) this connection carries, when
        /// adopted. The broker delivers payloads only to connections
        /// announcing the same record, so two people editing different
        /// files in one room don't merge each other's state; `None` means
        /// one document per `(app, room)`.
        record: Option<String>,
        /// Name on the caret pill; `None` gets a synthetic `peer-<n>`.
        /// Informational only — never used for routing or grants.
        handle: Option<String>,
    },
    /// A typed complete snapshot or incremental change set. The SDK wraps the
    /// mutation in a signed envelope from the certified launcher identity.
    /// Keeping the kind in `DocumentMutation` avoids parallel request and wire
    /// enums drifting apart.
    Document {
        mutation: DocumentMutation,
        /// Host-local original contributor custody; never serialized on the wire.
        authorization: Option<crate::invocation_scope::DocumentAuthorization>,
    },
    /// Ask the broker to select one existing holder of this exact route as a
    /// snapshot responder. Safe to retry while no holder is available.
    RequestDocumentSnapshot,
    /// Outbound presence update — pointer moved, caret changed, selection
    /// shifted, or arrived/left a keyed object. Fanned out to every
    /// connected peer as `CollabEvent::RemotePresenceUpdate`.
    Presence {
        key: PresenceKey,
        payload: PresencePayload,
    },
    /// Signed incremental CRDT mutation. The broker validates against the
    /// channel's role table before fanning out; invalid envelopes are
    /// dropped and logged, never reaching peers.
    ///
    /// `Box`ed to keep the discriminant thin on the stack — `SignedOp` is
    /// large (32-byte pubkey + 64-byte sig + payload + UUIDs).
    SignedOperation {
        envelope: Box<SignedOp>,
    },
    /// Late-join scrollback fetch: up to `max` ops with `seq < before_seq`,
    /// answered with one [`CollabEvent::ScrollbackBatch`].
    ///
    /// Apps page by repeating with `before_seq` set to the lowest seq from
    /// the previous batch until `exhausted == true`, then fall back to the
    /// cooperative path (broker queries gossip peers).
    FetchScrollback {
        artifact: collab_types::ArtifactId,
        /// Exclusive upper bound on `seq`.
        before_seq: u64,
        /// Page size cap; chrome UX bounds pages to ~50 ops.
        max: u16,
    },
    /// Replace this application's complete exported resource/action surface.
    PublishActions {
        app_id: String,
        actions: Vec<ExportedAction>,
    },
    /// App-owned CRDT state projected for same-identity continuation. The
    /// local broker authenticates the route and stores this only for shell
    /// capture; it is never a room action or a restore command.
    ReportContinuation {
        app_id: String,
        schema_version: u16,
        metadata: Vec<u8>,
    },
    /// Remove the last continuation report after the app can no longer
    /// produce a safe projection.
    ClearContinuation {
        app_id: String,
    },
    /// Private, level-triggered playback descriptors for shell-mediated
    /// same-person handoff. Never part of the room CRDT or peer fan-out.
    ReportPlayback {
        app_id: String,
        reports_json: Vec<u8>,
    },
    /// One generic active-session continuity transition. The shell binds the
    /// command to this connection's certified app/workspace route.
    NativeMediaCommand {
        request_id: u64,
        command: runtime_collab::native_media::Request,
    },
    RealtimeCommand {
        request_id: u64,
        app_id: String,
        command: crate::realtime_continuity::RealtimeCommand,
    },
    /// One Files-owned picker/status/consent command. The broker authenticates
    /// the app route and keeps payload bytes on the shell transfer transport.
    NearbyTransferCommand {
        request_id: u64,
        app_id: String,
        command: runtime_collab::NearbyTransferCommand,
    },
    OpenWorkspaceControl {
        session_id: u64,
        target_device: [u8; 32],
    },
    WorkspaceControlInput {
        session_id: u64,
        event: runtime_collab::WorkspaceControlInput,
    },
    CloseWorkspaceControl {
        session_id: u64,
    },
    /// Report the terminal result of a directed invocation.
    CompleteInvocation {
        id: u64,
        code: String,
        message: String,
    },
    UpdateInvocation {
        id: u64,
        update: tilo_actions::InvocationUpdate,
    },
    /// Terminal receipt for an Effect initiated by this app's local UI.
    RecordLocalEffect {
        address: tilo_actions::ActionAddress,
        update: tilo_actions::InvocationUpdate,
    },
    QueryActionDirectory {
        request_id: u64,
        query: runtime_collab::ActionDirectoryQuery,
    },
    ResolvePublishedAction {
        request_id: u64,
        peer_id: u64,
        revision: String,
        resource: String,
        action: String,
    },
    /// Opaque funded work from the owner-thread managed sender. Queue acceptance
    /// does not establish durable local acceptance or receiver admission.
    #[cfg(unix)]
    ManagedInvocationAttempt {
        attempt: crate::host_worker::managed_sender::ManagedAttempt,
    },
    InvokeResolvedPublishedAction {
        request_id: u64,
        peer_id: u64,
        revision: String,
        resource: String,
        action: String,
        arguments: serde_json::Map<String, serde_json::Value>,
        confirmation: serde_json::Value,
    },
    InvokePublishedAction {
        request_id: u64,
        peer_id: u64,
        resource: String,
        action: String,
        arguments: serde_json::Map<String, serde_json::Value>,
        confirmation: serde_json::Value,
    },
    PublishedInvocationStatus {
        request_id: u64,
        id: u64,
    },
    CancelPublishedInvocation {
        request_id: u64,
        id: u64,
    },
    LaunchPublishedApplication {
        request_id: u64,
        app_id: String,
        /// Optional portable blob identity staged read-only by the session
        /// before it launches the target application.
        content_hash: Option<String>,
    },
    /// Lifecycle record for a broker-minted task lease. Only an admitted
    /// agent connection can make this request; the broker checks the lease.
    SubscribeEnrolledTasks {
        request_id: u64,
    },
    AcceptEnrolledTask {
        request_id: u64,
        acceptance: runtime_collab::EnrolledTaskAcceptance,
    },
    ReportEnrolledTask {
        request_id: u64,
        acceptance: runtime_collab::EnrolledTaskAcceptance,
        sequence: u64,
        update: runtime_collab::TaskRunUpdate,
    },
    ReportTaskRun {
        run_id: u64,
        lease: String,
        update: runtime_collab::TaskRunUpdate,
    },
    /// Invoke an already permission-filtered published action under the
    /// broker-owned task context. `request_id` correlates the async reply.
    /// `step` names the task's durable parent step: the host maps it to one
    /// child before execution and answers a re-send of the same step with
    /// that child's committed result instead of a new invocation.
    InvokeTaskAction {
        request_id: u64,
        run_id: u64,
        lease: String,
        step: Option<u32>,
        peer_id: u64,
        resource: String,
        action: String,
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// Poll the native Task Center's cooperative cancellation bit for a
    /// lease. It cannot cancel a task and carries no task state itself.
    TaskRunCancellation {
        request_id: u64,
        run_id: u64,
        lease: String,
    },
}

impl CollabRequest {
    pub fn document_snapshot(data: Vec<u8>) -> Self {
        Self::Document {
            mutation: DocumentMutation::Snapshot(data),
            authorization: None,
        }
    }

    pub fn document_incremental(data: Vec<u8>) -> Self {
        Self::Document {
            mutation: DocumentMutation::Incremental(data),
            authorization: None,
        }
    }
}

/// A broker directory request awaiting main-thread authority.  This crosses
/// threads as inert strings and budgets only; no widget reference or Cap'n
/// Proto handle ever leaves the UI/collaboration boundary.
pub enum ActionDirectoryRequest {
    Query {
        query: runtime_collab::ActionDirectoryQuery,
        reply: tokio::sync::oneshot::Sender<runtime_collab::ActionDirectoryPage>,
    },
    Resolve {
        revision: String,
        resource: String,
        action: String,
        reply: tokio::sync::oneshot::Sender<runtime_collab::ActionDirectoryResolution>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        wants_content: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedAction {
    pub resource: String,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    pub operation_kind: String,
    pub params_json: String,
    pub contract_json: String,
    pub input: ExportedInput,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct BrokerActionSurface {
    pub peer_id: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    pub live: bool,
    pub actions: Vec<BrokerActionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct BrokerActionDescriptor {
    pub resource: String,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    pub operation_kind: tilo_actions::OperationKind,
    pub params: Vec<tilo_actions::ActionParam>,
    pub contract: tilo_actions::ActionContract,
    pub input: BrokerActionInput,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct BrokerActionInput {
    pub kind: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub wants_content: bool,
}

#[derive(serde::Deserialize)]
struct BrokerActionCatalogueWire {
    surfaces: Vec<BrokerActionSurface>,
}

/// An explicit typed collaboration route.
///
/// A manager has one primary route (announced by its initial `Hello`) and may
/// retain additional routes for discovery or cross-app work. Callers must name
/// an additional route again when publishing, which prevents a cluster-wide
/// discovery subscription from becoming an accidental mutation target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteSpec {
    pub app_id: String,
    pub cluster_id: Uuid,
    pub record: Option<String>,
}

impl RouteSpec {
    pub fn cluster(cluster_id: Uuid) -> Self {
        Self {
            app_id: String::new(),
            cluster_id,
            record: None,
        }
    }

    pub fn app(app_id: impl Into<String>, cluster_id: Uuid) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id,
            record: None,
        }
    }

    pub fn record(app_id: impl Into<String>, cluster_id: Uuid, record: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id,
            record: Some(record.into()),
        }
    }

    fn channel_id(&self) -> String {
        channel_key::format(
            &self.app_id,
            &self.cluster_id.to_string(),
            self.record.as_deref().unwrap_or(""),
        )
    }
}

#[derive(Debug, Clone)]
pub enum CollabEvent {
    /// Original committed history from this SDK channel, applied by its document owner.
    CommittedDocument {
        route: RouteSpec,
        read: CommittedDocument,
    },
    Context {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    /// Session chrome asked this app to bind its viewport/view mode to a
    /// participant, or release that binding. This is local session control:
    /// it is never published to collaboration peers.
    FollowChanged {
        peer_id: Option<String>,
    },
    /// The broker selected this connection as the sole snapshot responder for
    /// a late joiner on the same document route.
    DocumentSnapshotRequested {
        route: RouteSpec,
    },
    RemoteCursorMoved {
        user_id: String,
        x: f32,
        y: f32,
    },
    RemotePresenceUpdate {
        user_id: String,
        key: PresenceKey,
        payload: PresencePayload,
    },
    /// A typed action another app in this room published, delivered to every
    /// connection. A receiver decodes it via its own
    /// [`ActionRegistry`](tilo_actions::ActionRegistry); an action for a
    /// different app decodes to nothing, making room-wide fan-out safe.
    ///
    /// `from_user_id` is who *sent* the frame, not necessarily who the
    /// action says authored it.
    ActionArrived {
        type_id: String,
        payload: Vec<u8>,
        from_user_id: String,
    },
    /// A validated signed document envelope from another peer. For cleartext
    /// routes the mutation kind is inside the outer signed payload. For an
    /// MLS route, `envelope.payload` remains the original signed ciphertext
    /// and `decrypted_payload` holds endpoint-only plaintext after the inner
    /// MLS authentication succeeds.
    Document {
        envelope: Box<SignedOp>,
        from_user_id: String,
        /// Endpoint-decrypted typed mutation bytes. `None` for cleartext
        /// envelopes (read directly from `envelope.payload`) and for broker
        /// events, which never possess MLS keys.
        decrypted_payload: Option<Vec<u8>>,
    },
    /// Response to a [`CollabRequest::FetchScrollback`]: a page of
    /// previously-validated ops plus `exhausted`.
    ///
    /// Targeted send — unlike every other event here, the broker writes
    /// this only to the connection that issued the matching request.
    ScrollbackBatch {
        /// Echoes `FetchScrollback::artifact` so multiple in-flight
        /// fetches route to the right history view.
        artifact: collab_types::ArtifactId,
        /// Ascending seq order. Empty when the broker has no matching
        /// range (typically evicted by compaction).
        ops: Vec<Box<SignedOp>>,
        /// Endpoint-decrypted payload parallel to `ops`. Brokers leave this
        /// empty because they have no MLS keys; SDK endpoints populate one
        /// slot per op and drop undecryptable encrypted entries.
        decrypted_payloads: Vec<Option<Vec<u8>>>,
        exhausted: bool,
    },
    /// One capability-checked invocation addressed to this application
    /// connection by the session action broker.
    Invocation {
        id: u64,
        invocation: Option<tilo_actions::InvocationId>,
        resource: String,
        action: String,
        argument: Option<String>,
        actor: String,
        /// Broker-owned task/session provenance. The concrete application
        /// resource and action remain authoritative; this only supplies the
        /// surrounding purpose and work-group for durable history.
        context: Option<Box<tilo_actions::ActionContext>>,
    },
    /// The invocation owner requested cooperative cancellation. Delivered at
    /// most once; the app-side job decides the next safe stopping boundary.
    InvocationCancellation {
        id: u64,
    },
    ActionCatalogue {
        surfaces: Vec<BrokerActionSurface>,
    },
    ActionDirectoryPage {
        request_id: u64,
        result: Result<runtime_collab::ActionDirectoryPage, String>,
    },
    PublishedActionResolution {
        request_id: u64,
        result: Result<runtime_collab::ActionDirectoryResolution, String>,
    },
    /// Original attempt custody remains with this result through final clone
    /// destruction. Only the sender's physical barrier can acknowledge it.
    #[cfg(unix)]
    ManagedInvocationObservation {
        result: crate::host_worker::managed_sender::ManagedAttemptResult,
    },
    PublishedInvocationResult {
        request_id: u64,
        result: Result<(u64, tilo_actions::InvocationUpdate), String>,
    },
    /// A run addressed to this admitted agent. The envelope is broker-minted
    /// and contains a lease, not a caller-selected authority token.
    TaskRun(runtime_collab::TaskRunEnvelope),
    EnrolledTask(runtime_collab::EnrolledTaskEnvelope),
    EnrolledTaskReply {
        request_id: u64,
        result: Result<(), String>,
    },
    EnrolledTaskDisconnected {
        detail: String,
    },
    TaskActionInvocationResult {
        request_id: u64,
        result: Result<(u64, tilo_actions::InvocationUpdate), String>,
        /// For a stepped request the host committed: the parent's committed
        /// next-step continuation (the result carries id 0 and the committed
        /// terminal update). `None` for a legacy reply, including an older
        /// broker that ignored the step identity and started an invocation.
        continuation: Option<Vec<u8>>,
        /// The step is bound but not settled yet, or its reply was lost.
        /// The caller must re-send the identical step; `result` is an error.
        retry_step: bool,
    },
    TaskRunCancellation {
        request_id: u64,
        result: Result<bool, String>,
    },
    WorkspaceControl {
        session_id: u64,
        event: runtime_collab::WorkspaceControlEvent,
    },
    NativeMediaCommandResult {
        request_id: u64,
        result: Result<runtime_collab::native_media::Snapshot, String>,
    },
    RealtimeCommandResult {
        request_id: u64,
        result: Result<crate::realtime_continuity::RealtimeReply, String>,
    },
    NearbyTransferCommandResult {
        request_id: u64,
        result: Result<runtime_collab::NearbyTransferReply, String>,
    },
    Realtime(crate::realtime_continuity::RealtimeEvent),
}

impl CollabEvent {
    /// Decode the typed payload of a validated document event. Returns `None`
    /// for presence, context, action, and scrollback events.
    pub fn document_mutation(
        &self,
    ) -> Option<Result<DocumentMutation, collab_types::DocumentMutationError>> {
        let Self::Document {
            envelope,
            decrypted_payload,
            ..
        } = self
        else {
            return None;
        };
        Some(
            DocumentMutation::decode_versioned(
                decrypted_payload.as_deref().unwrap_or(&envelope.payload),
            )
            .map(|decoded| decoded.mutation),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PresencePayload {
    Pointer {
        x: f32,
        y: f32,
    },
    Caret {
        pos: usize,
    },
    Selection {
        start: usize,
        end: usize,
    },
    /// The local user just inserted `start..end` under scope `key`; peers
    /// render a fading "just typed" swipe (see
    /// [`crate::presence::PresenceCtx::record_peer_typing`]). Unlike
    /// `Caret`/`Selection` this is an *event*, published once and decayed
    /// on the receiver.
    Typing {
        start: usize,
        end: usize,
    },
    Arrived,
    Left,
    /// A room-scoped, transient follow relationship announced by the
    /// authenticated sender. It is a visual presence cue only; the local
    /// Follow command remains private to the follower's session.
    Following {
        target: Option<String>,
    },
    /// Truthful action/resource activity. Identity is supplied by the
    /// authenticated event envelope; this payload never describes a mouse or
    /// keyboard gesture and therefore cannot create a synthetic cursor.
    Interaction(Box<tilo_actions::SemanticInteraction>),
    /// Short room-scoped intention. `automatic` distinguishes a local-model
    /// suggestion from participant-authored words; empty text clears it.
    Status {
        text: String,
        automatic: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    pub color: [f32; 3],
    /// Whether this participant is an agent rather than a person.
    pub is_agent: bool,
    /// Whether this row is the receiving machine's own participant.
    /// Apps can't derive this themselves: `id` is the address *other*
    /// machines use, and a machine never announces its own to its own apps.
    pub is_self: bool,
}

/// Failure modes for [`CollaborationManager::send_signed_op`].
/// `NoIdentity` is the common case — call before the shell provisions an
/// identity — and callers can fall back to `document_incremental`.
#[derive(Debug)]
pub enum SignedSendError {
    /// No [`SigningIdentity`] bound (see [`CollaborationManager::with_identity`]).
    NoIdentity,
    /// The identity has signed every representable sequence number. Reusing
    /// one would make a fresh edit indistinguishable from a replay attack, so
    /// the identity must be rotated before any more operations are signed.
    SequenceExhausted,
    /// Canonical-bytes serialisation of the envelope failed; in practice
    /// only fires for unrepresentable values.
    Canonical(collab_types::CanonicalError),
}

impl std::fmt::Display for SignedSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignedSendError::NoIdentity => {
                f.write_str("CollaborationManager has no signing identity bound")
            }
            SignedSendError::SequenceExhausted => {
                f.write_str("collaboration signing sequence exhausted; rotate the identity")
            }
            SignedSendError::Canonical(e) => write!(f, "canonical encoder failed: {e}"),
        }
    }
}

impl std::error::Error for SignedSendError {}

/// Identity-side handle used by [`CollaborationManager::send_signed_op`]
/// to mint signed envelopes. The secret key stays private — not
/// re-exported on the manager's public surface — so app code can only
/// sign via the manager's helper, never handle the raw secret.
pub struct SigningIdentity {
    /// Local secret half — never serialised, never logged.
    pub(crate) secret_key: ed25519_dalek::SigningKey,
    /// Public half — embedded as the `author` field on every signed op.
    pub public_key: collab_types::Ed25519PublicKey,
    /// Role attestation the broker looks up ops against. One identity may
    /// hold attestations on multiple channels; this pins to one per manager.
    pub attestation_id: collab_types::AttestationId,
    /// Monotonic per-author seq, bumped on every `sign` so the validator's
    /// replay check (`seq > last_seen`) passes. Reusing a seq is the
    /// spec-defined replay attack.
    pub(crate) seq: u64,
}

impl SigningIdentity {
    /// Construct an identity from a fresh keypair and the attestation id
    /// that authorises it. Mostly used in tests; production code builds
    /// via the shell-side identity-loading helper.
    pub fn new(
        secret_key: ed25519_dalek::SigningKey,
        attestation_id: collab_types::AttestationId,
    ) -> Self {
        let public_key =
            collab_types::Ed25519PublicKey::from_bytes(secret_key.verifying_key().to_bytes());
        Self {
            secret_key,
            public_key,
            attestation_id,
            seq: 0,
        }
    }

    /// Build a fresh `SignedOp` envelope wrapping `payload`, bumping the
    /// internal seq so callers don't need to track sequence numbers.
    ///
    /// # Errors
    ///
    /// Returns [`SignedSendError::SequenceExhausted`] rather than ever reusing
    /// a sequence number, or [`SignedSendError::Canonical`] when `payload`
    /// fails to canonicalize for signing.
    pub fn sign_op(
        &mut self,
        payload: Vec<u8>,
        artifact: collab_types::ArtifactId,
        op_class: collab_types::OpClass,
    ) -> Result<collab_types::SignedOp, SignedSendError> {
        let timestamp = crate::clock::SystemTime::now();
        let sequence = self
            .seq
            .checked_add(1)
            .ok_or(SignedSendError::SequenceExhausted)?;
        let mut op = collab_types::SignedOp::new_unsigned(
            payload,
            artifact,
            self.public_key,
            self.attestation_id,
            op_class,
            timestamp,
            sequence,
        );
        collab_types::sign_op(&mut op, &self.secret_key).map_err(SignedSendError::Canonical)?;
        self.seq = sequence;
        Ok(op)
    }
}

/// Which transport the manager is speaking. Enum-per-mode instead of
/// trait-object'ing because the match cost on `send` / `poll` is
/// negligible and the code stays flat for a reader.
enum Backend {
    /// No collab service reachable. `poll` yields nothing, `send` is a
    /// black hole; every widget already handles this via
    /// [`CollaborationManager::is_connected`].
    Disabled,
    /// Talking Cap'n Proto to `runtime-collab`. The shovel thread owns
    /// the SDK client + subscriptions; winit-side send / poll bridge
    /// through bounded mpsc pairs.
    Sdk(SdkChannel),
    Peer(Box<dyn PeerTransport>),
}

/// Owned mpsc pair + shovel-thread handle for SDK mode. Both directions are
/// bounded. Durable overflow makes the bridge unavailable rather than
/// silently losing a document, action, or invocation transition.
///
/// Dropping the manager drops `outbound_tx`; the shovel thread's
/// `.recv()` then yields `None` and the thread exits. `_thread` is kept
/// only so the `JoinHandle` isn't detached.
struct SdkChannel {
    outbound_tx: tokio_mpsc::Sender<QueuedSdkCommand>,
    media: Option<native_media::Sender>,
    inbound_rx: tokio_mpsc::Receiver<QueuedCollabEvent>,
    state: BridgeState,
    /// The receiver stays on the application's owner thread.  RPC servers
    /// own only the cloneable sender, so a Cap'n Proto callback cannot touch
    /// widgets directly.
    action_directory_rx: mpsc::Receiver<ActionDirectoryRequest>,
    action_directory_tx: mpsc::SyncSender<ActionDirectoryRequest>,
    action_directory_published: bool,
    _thread: Option<thread::JoinHandle<()>>,
}

/// Failure to complete a prefix of SDK document publications.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentPublicationError {
    /// This transport has no document-publication completion protocol.
    Unsupported,
    /// No SDK connection is available.
    Unavailable,
    /// The first signing, encoding, transport, or queue failure.
    Failed(String),
    /// The worker exited without resolving the receipt.
    WorkerStopped,
    /// The caller's existing absolute deadline elapsed.
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

/// Completion of the document prefix preceding one ordered SDK marker.
///
/// Success means signing/encoding and the broker send RPCs returned successfully.
/// It does not acknowledge peer observation, CRDT application, or durability.
/// Keep the manager (including its inbound receiver) alive while waiting.
pub struct DocumentPublicationReceipt {
    receiver: mpsc::Receiver<Result<(), DocumentPublicationError>>,
    state: BridgeState,
}

impl DocumentPublicationReceipt {
    /// Inspect completion without waiting. A completed receipt is consumed.
    ///
    /// # Errors
    /// Returns the prefix failure or the worker's disconnection cause.
    pub fn try_complete(&self) -> Result<Option<()>, DocumentPublicationError> {
        match self.receiver.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(self.disconnected()),
        }
    }

    /// Wait for the ordered reply, optionally within an existing absolute deadline.
    /// Already completed replies take precedence over an elapsed deadline.
    ///
    /// # Errors
    /// Returns a publication failure, worker disconnection, or elapsed deadline.
    pub fn wait(
        self,
        deadline: Option<crate::clock::Instant>,
    ) -> Result<(), DocumentPublicationError> {
        if self.try_complete()?.is_some() {
            return Ok(());
        }
        match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(crate::clock::Instant::now());
                match self.receiver.recv_timeout(remaining) {
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
        self.state
            .publication_result()
            .err()
            .unwrap_or(DocumentPublicationError::WorkerStopped)
    }
}

enum SdkCommand {
    Primary(CollabRequest),
    Join(RouteSpec),
    ToRoute(RouteSpec, CollabRequest),
    PrimaryReady(mpsc::SyncSender<Result<RouteSpec, String>>),
    DocumentPublicationBarrier(mpsc::SyncSender<Result<(), DocumentPublicationError>>),
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
}

impl SdkCommand {
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

fn launch_published_application_bytes(
    app_id: &String,
    content_hash: Option<&String>,
    meter: &mut ByteMeter,
) {
    meter.string_capacity(app_id);
    if let Some(content_hash) = content_hash {
        meter.string_capacity(content_hash);
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
        CollabRequest::LaunchPublishedApplication {
            app_id,
            content_hash,
            ..
        } => launch_published_application_bytes(app_id, content_hash.as_ref(), &mut meter),
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
        SdkCommand::PrimaryReady(_) | SdkCommand::DocumentPublicationBarrier(_) => {
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

fn broker_action_bytes(action: &BrokerActionDescriptor, meter: &mut ByteMeter) {
    meter.string_capacity(&action.resource);
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
fn event_bytes(event: &CollabEvent) -> Option<usize> {
    let mut meter = ByteMeter(Some(std::mem::size_of::<CollabEvent>()));
    match event {
        CollabEvent::CommittedDocument { route, .. } => {
            // Capture bytes retain their own process-wide preallocation permit.
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
        }
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
        CollabEvent::FollowChanged { peer_id } => {
            if let Some(peer_id) = peer_id {
                meter.string_capacity(peer_id);
            }
        }
        CollabEvent::DocumentSnapshotRequested { route } => {
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
        CollabEvent::ActionCatalogue { surfaces } => {
            action_catalogue_bytes(surfaces, surfaces.capacity(), &mut meter);
        }
        CollabEvent::ActionDirectoryPage { result, .. } => match result {
            Ok(page) => directory_client::page_bytes(page, &mut meter),
            Err(error) => meter.string_capacity(error),
        },
        CollabEvent::PublishedActionResolution { result, .. } => match result {
            Ok(resolution) => directory_client::resolution_bytes(resolution, &mut meter),
            Err(error) => meter.string_capacity(error),
        },
        #[cfg(unix)]
        CollabEvent::ManagedInvocationObservation { result } => meter.add(result.meter_bytes()),
        CollabEvent::PublishedInvocationResult { result, .. } => match result {
            Ok((_, update)) => serialized_bytes(update, &mut meter),
            Err(error) => meter.string_capacity(error),
        },
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
        }
        CollabEvent::EnrolledTask(run) => serialized_bytes(run, &mut meter),
        CollabEvent::EnrolledTaskReply { result, .. } => {
            if let Err(error) = result {
                meter.string_capacity(error);
            }
        }
        CollabEvent::EnrolledTaskDisconnected { detail } => meter.string_capacity(detail),
        CollabEvent::TaskRun(run) => task_run_bytes(run, &mut meter),
        CollabEvent::TaskRunCancellation { result, .. } => {
            if let Err(error) = result {
                meter.string_capacity(error);
            }
        }
        CollabEvent::WorkspaceControl { event, .. } => {
            workspace_control_event_bytes(event, &mut meter);
        }
        CollabEvent::NativeMediaCommandResult { result, .. } => match result {
            Ok(reply) => serialized_bytes(reply, &mut meter),
            Err(error) => meter.string_capacity(error),
        },
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
    document_reads: committed_document::Reads,
    healthy: Arc<AtomicBool>,
    publication_failure: Arc<std::sync::Mutex<Option<DocumentPublicationError>>>,
    command_bytes: ByteBudget,
    event_bytes: ByteBudget,
    owner_wake: crate::wake::WakeHandle,
    wake_pending: Arc<AtomicBool>,
}

/// Lock-free aggregate reservation for heap payload retained by one queue.
/// A reservation moves with its queued value and releases automatically when
/// the receiver consumes or drops that value.
#[derive(Clone)]
struct ByteBudget(Arc<AtomicUsize>);

struct ByteReservation {
    budget: ByteBudget,
    bytes: usize,
}

impl ByteBudget {
    fn new() -> Self {
        Self(Arc::new(AtomicUsize::new(0)))
    }

    fn reserve(&self, bytes: usize, limit: usize) -> Option<ByteReservation> {
        let mut used = self.0.load(Ordering::Acquire);
        loop {
            let next = used.checked_add(bytes)?;
            if next > limit {
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
            document_reads: committed_document::Reads::default(),
            healthy: Arc::new(AtomicBool::new(true)),
            publication_failure: Arc::new(std::sync::Mutex::new(None)),
            command_bytes: ByteBudget::new(),
            event_bytes: ByteBudget::new(),
            owner_wake: crate::wake::WakeHandle::new(),
            wake_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
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
        // Latch before withdrawing health or dropping any queued receipt sender.
        self.record_publication_failure(DocumentPublicationError::Failed(direction.into()));
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
    if !state.is_healthy() {
        return false;
    }
    let transient = command.is_transient_presence();
    let Some(reservation) = state.command_bytes.reserve(
        command_bytes(&command).unwrap_or(usize::MAX),
        SDK_COMMAND_QUEUE_BYTES,
    ) else {
        if !transient {
            state.fail_closed("outbound command byte budget");
        }
        return false;
    };
    match tx.try_send(QueuedSdkCommand {
        command,
        _reservation: reservation,
    }) {
        Ok(()) => true,
        Err(tokio_mpsc::error::TrySendError::Full(_)) if transient => false,
        Err(_) => {
            state.fail_closed("outbound command queue");
            false
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
}

impl InboundBridge {
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

/// A one-shot host handoff factory, evaluated on the SDK worker's LocalSet.
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
            .finish()
    }
}

/// Pending owner-backed document commit.
pub type HostDocumentCommit =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<SignedOp, String>> + Send + 'static>>;

/// Host-local capability, never selected by a wire peer. Called on the SDK
/// shovel thread, while the application's ordinary owner thread keeps pumping.
/// Implementations bind an exact route and revalidate current authority.
/// Native launcher and explicit-key endpoints retain their existing signer.
pub trait HostDocumentCommitter: Send + Sync {
    fn commit(
        &self,
        route: &RouteSpec,
        mutation: collab_types::DocumentMutation,
    ) -> HostDocumentCommit;

    /// Required custody must reach the concrete final owner intact. A legacy
    /// committer cannot silently downgrade it to its unqualified commit method.
    fn commit_scoped(
        &self,
        _route: &RouteSpec,
        _mutation: collab_types::DocumentMutation,
        _authorization: crate::invocation_scope::DocumentAuthorization,
    ) -> HostDocumentCommit {
        Box::pin(async { Err("host has no final scoped document evaluator".into()) })
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
    Certified(Box<dyn SdkCertifiedIdentity>),
    /// Explicit-FD harnesses have no launcher identity service. Their
    /// in-process broker accepts this ephemeral signer; a production broker
    /// still rejects the uncertified join before any op reaches policy.
    Harness(Box<ed25519_dalek::SigningKey>),
}

/// Identity state after the shovel thread has entered its LocalSet. The host
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
        &mut rand_core::OsRng,
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

/// Successful primary-route metadata; an epoch invalidates cursors even when
/// the same route is re-announced. This is not an application-mintable credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrimaryRouteWitness {
    pub(crate) route: RouteSpec,
    pub(crate) epoch: Uuid,
}
#[derive(Default)]
struct PrimaryRouteState {
    required_document_custody: bool,
    requested: Option<RouteSpec>,
    accepted: Option<PrimaryRouteWitness>,
}
impl PrimaryRouteState {
    fn announce(&mut self, route: RouteSpec) {
        self.accepted = None;
        self.requested = Some(route);
    }
    fn accept(&mut self, result: Result<RouteSpec, String>) -> Result<(), String> {
        self.accepted = None;
        let route = result?;
        if self.requested.as_ref() != Some(&route) {
            return Err("primary route acknowledgement differs from the latest Hello".into());
        }
        self.accepted = Some(PrimaryRouteWitness {
            route,
            epoch: Uuid::new_v4(),
        });
        Ok(())
    }
    fn current(&self, healthy: bool) -> Option<&PrimaryRouteWitness> {
        healthy.then_some(self.accepted.as_ref()).flatten()
    }
}

pub struct CollaborationManager {
    backend: Backend,
    primary_route: PrimaryRouteState,
    /// When `Some`, [`Self::send_signed_op`] mints a signed envelope and
    /// dispatches [`CollabRequest::SignedOperation`]. This explicit
    /// policy-envelope API is separate from document mutations, which the SDK
    /// signs with the certified launcher identity.
    identity: Option<SigningIdentity>,
}

impl CollaborationManager {
    pub(crate) fn require_document_authorization(&mut self) {
        self.primary_route.required_document_custody = true;
    }

    fn missing_required_document_custody(&self, request: &CollabRequest) -> bool {
        self.primary_route.required_document_custody
            && matches!(
                request,
                CollabRequest::Document {
                    authorization: None,
                    ..
                }
            )
    }
    /// Inject a host-owned ordinary authenticated peer transport. Joins remain
    /// pending until the adapter supplies the broker's matching acknowledgement.
    pub fn with_peer_transport(transport: impl PeerTransport + 'static) -> Self {
        Self {
            backend: Backend::Peer(Box::new(transport)),
            primary_route: Default::default(),
            identity: None,
        }
    }

    fn peer_result(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            tracing::warn!(%error, "collaboration peer transport unavailable");
            self.backend = Backend::Disabled;
            self.primary_route.accepted = None;
        }
    }

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
        // Browser transports intentionally remain local and do not require
        // `Send`; the shared manager enum therefore cannot cross a blocking
        // worker boundary. The connector is one-shot, so perform the same
        // fail-closed bootstrap in this caller-owned async task.
        Self::for_route_host_blocking(connector, app_id, cluster_id, record, handle)
    }

    /// Synchronous implementation retained inside the background bootstrap
    /// task and deterministic unit harnesses. Production callers only receive
    /// the async entry point above, so identity and primary-route deadlines do
    /// not stall the application's owner/frame thread.
    fn for_route_host_blocking(
        connector: HostSessionConnector,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        let app_id = app_id.into();
        let route = RouteSpec {
            app_id: app_id.clone(),
            cluster_id,
            record: record.clone(),
        };
        Self::for_route_from(
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
        let sdk = Self::spawn_sdk(source, identity)?;
        let mut manager = Self {
            primary_route: PrimaryRouteState::default(),
            backend: Backend::Sdk(sdk),
            identity: None,
        };
        manager.send(CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            handle,
        });
        manager.await_primary_route()?;
        Ok(manager)
    }

    /// Wait until the shovel has processed the initial `Hello`, admitted its
    /// primary route, and queued the route's initial action catalogue. Without
    /// this barrier a caller can publish transient presence immediately after
    /// construction, before the broker knows where to deliver it.
    fn await_primary_route(&mut self) -> Result<(), runtime_portal::NotAvailable> {
        self.primary_route.accepted = None;
        let Backend::Sdk(channel) = &mut self.backend else {
            return Err(runtime_portal::NotAvailable::ServiceFdMissing);
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        if !enqueue_sdk_command(
            &channel.outbound_tx,
            SdkCommand::PrimaryReady(ready_tx),
            &channel.state,
        ) {
            return Err(runtime_portal::NotAvailable::ConnectionFailed(
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "collaboration setup thread is unavailable or overloaded",
                ),
            ));
        }
        match ready_rx.recv_timeout(SHOVEL_INIT_TIMEOUT) {
            Ok(result) => self.primary_route.accept(result).map_err(|error| {
                let kind = if error == "collaboration primary route timed out" {
                    std::io::ErrorKind::TimedOut
                } else {
                    std::io::ErrorKind::ConnectionRefused
                };
                runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(kind, error))
            }),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(
                runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "collaboration primary route did not become ready in time",
                )),
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(
                runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "collaboration setup thread ended before reporting route readiness",
                )),
            ),
        }
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

    /// Connects through `runtime_collab::try_connect()`, entering SDK mode on
    /// `Ok` and Disabled mode on `Err`.
    ///
    /// Both modes auto-send `Hello` announcing **no record** (one document
    /// per `(app, room)`); use [`Self::for_record`] for apps that have
    /// adopted records.
    pub fn new() -> Self {
        Self::new_scoped(
            continuation_instance_env().map(|instance| format!("continuation:{instance}")),
        )
    }
}

/// Launcher-issued instance identity for an app window. It deliberately rides
/// in the existing record component of the authenticated CRDT route so the
/// broker can retain multiple Browser windows in one workspace without
/// conflating their continuation metadata.
fn continuation_instance_env() -> Option<String> {
    let value = std::env::var("TILO_CONTINUITY_INSTANCE").ok()?;
    let value = value.trim();
    (!value.is_empty() && value.len() <= 256 && !value.contains(['/', '\\']))
        .then(|| value.to_owned())
}

impl Default for CollaborationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CollaborationManager {
    pub(crate) fn has_native_committed_primary(&self) -> bool {
        match (&self.backend, &self.primary_route.accepted) {
            (Backend::Sdk(channel), Some(route)) => channel
                .state
                .document_reads
                .required(&route.route.channel_id()),
            _ => false,
        }
    }
    /// Join through the existing worker; never take the inherited RPC fd twice.
    pub(crate) fn prepare_native_record(
        &mut self,
        record: &crate::record::RecordId,
    ) -> Option<CommittedDocument> {
        let primary = self.primary_route.accepted.as_ref()?;
        let route = RouteSpec::record(
            primary.route.app_id.clone(),
            primary.route.cluster_id,
            record.as_str(),
        );
        let Backend::Sdk(channel) = &self.backend else {
            return None;
        };
        let key = route.channel_id();
        if channel.state.document_reads.joining(&key) {
            let _ = enqueue_sdk_command(
                &channel.outbound_tx,
                SdkCommand::Join(route),
                &channel.state,
            );
        }
        channel.state.document_reads.initial(&key)
    }
    pub(crate) fn switch_native_record(
        &mut self,
        record: &crate::record::RecordId,
    ) -> Result<(), String> {
        let primary = self
            .primary_route
            .accepted
            .as_ref()
            .ok_or("native primary route unavailable")?
            .route
            .clone();
        let record = (!record.is_package_primary()).then(|| record.as_str().to_owned());
        if primary.record == record {
            return Ok(());
        }
        if !self.send(CollabRequest::Hello {
            app_id: primary.app_id,
            cluster_id: primary.cluster_id,
            record,
            handle: presence_handle_env(),
        }) {
            return Err("native route queue unavailable".into());
        }
        self.await_primary_route()
            .map_err(|error| error.to_string())
    }
    /// A manager scoped to **one record** — one document, one change graph,
    /// one channel — so two people editing different files in one room
    /// don't get handed each other's state. A process with several records
    /// open holds several managers; `ui::App` swaps its manager on checkout
    /// change.
    pub fn for_record(record: &crate::record::RecordId) -> Self {
        Self::new_scoped(Some(record.as_str().to_string()))
    }

    /// Explicitly disconnected construction: no identity, service or worker discovery.
    pub(crate) fn disabled() -> Self {
        Self {
            backend: Backend::Disabled,
            primary_route: PrimaryRouteState::default(),
            identity: None,
        }
    }

    fn new_scoped(record: Option<String>) -> Self {
        let (app_id, cluster_id) = auto_hello_env();
        let identity = match sdk_join_identity(cluster_id) {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "collab: workspace join identity unavailable; disabling SDK collaboration"
                );
                return Self::disabled();
            }
        };

        match Self::spawn_sdk(SdkConnectSource::Env, Some(identity)) {
            Ok(sdk) => {
                let mut manager = Self {
                    primary_route: PrimaryRouteState::default(),
                    backend: Backend::Sdk(sdk),
                    identity: None,
                };
                manager.send(CollabRequest::Hello {
                    app_id,
                    cluster_id,
                    record,
                    handle: presence_handle_env(),
                });
                if let Err(error) = manager.await_primary_route() {
                    tracing::warn!(
                        ?error,
                        "collab: primary route unavailable; disabling SDK collaboration"
                    );
                    return Self::disabled();
                }
                manager
            }
            Err(_) => Self::disabled(),
        }
    }

    pub(crate) fn for_native_launch() -> Result<Self, String> {
        if runtime_portal::service_fd("collab").is_none() {
            if std::env::var_os("TILO_SVC_COLLAB_FD").is_some() {
                return Err("configured native collaboration service is unavailable".into());
            }
            return Ok(Self::disabled());
        }
        let (app, room) = auto_hello_env();
        Self::for_route_from(
            SdkConnectSource::Env,
            app,
            room,
            None,
            presence_handle_env(),
            None,
            "person",
        )
        .map_err(|error| error.to_string())
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
    #[cfg(all(test, unix))]
    pub(crate) fn native_initial_fixture_gated_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_fixture_gated(app, room, history, Some(gate))
    }

    /// Real socket-backed SDK transport for App lifecycle integration tests.
    #[cfg(test)]
    pub(crate) fn connected_for_lifecycle_test() -> Self {
        tests::connected_manager()
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
        let mut manager = Self {
            primary_route: PrimaryRouteState::default(),
            backend: Backend::Sdk(sdk),
            identity: None,
        };
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

    /// Stand up the shovel thread + its tokio `current_thread` runtime.
    /// Runs `try_connect` **inside** the thread's `LocalSet` because
    /// capnp-rpc futures are `!Send` and can't cross a thread boundary.
    /// The result surfaces via [`InitStatus`] on a one-shot mpsc so the
    /// caller picks Disabled vs Sdk before returning, instead of an app
    /// that only ever `poll`s silently running against a dead broker.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_sdk(
        source: SdkConnectSource,
        identity: Option<SdkJoinIdentity>,
    ) -> Result<SdkChannel, runtime_portal::NotAvailable> {
        let (outbound_tx, outbound_rx) = tokio_mpsc::channel::<QueuedSdkCommand>(SDK_COMMAND_QUEUE);
        let (inbound_tx, inbound_rx) = tokio_mpsc::channel::<QueuedCollabEvent>(SDK_EVENT_QUEUE);
        let state = BridgeState::new();
        let inbound = InboundBridge {
            tx: inbound_tx,
            state: state.clone(),
        };
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

        // Must not wait forever or panic — this runs before the app's first
        // frame. A wedged connect costs a timeout and no collaboration, never
        // an app that won't open.
        let status = match init_rx.recv_timeout(SHOVEL_INIT_TIMEOUT) {
            Ok(status) => status,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    "collab: the connect path did not report within {SHOVEL_INIT_TIMEOUT:?}; \
                     continuing without collaboration"
                );
                return Err(runtime_portal::NotAvailable::ConnectionFailed(
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "collab connect did not report in time",
                    ),
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!(
                    "collab: the connect path ended without reporting; \
                     continuing without collaboration"
                );
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
                if let Ok(mut bridge) = WIFI_CONTINUITY_BRIDGE
                    .get_or_init(|| std::sync::Mutex::new(None))
                    .lock()
                {
                    *bridge = Some(outbound_tx.downgrade());
                }
                Ok(SdkChannel {
                    outbound_tx,
                    media: Some(media_tx),
                    inbound_rx,
                    state,
                    action_directory_rx,
                    action_directory_tx,
                    action_directory_published: false,
                    _thread: Some(handle),
                })
            }
            InitStatus::Failed(e) => {
                // Let the (empty-body) thread exit cleanly.
                let _ = handle.join();
                Err(e)
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn_sdk(
        _: SdkConnectSource,
        _: Option<SdkJoinIdentity>,
    ) -> Result<SdkChannel, runtime_portal::NotAvailable> {
        Err(runtime_portal::NotAvailable::ServiceFdMissing)
    }

    /// True when connected to a live broker — used by the
    /// event loop to keep ticking so the stream drains promptly.
    pub(crate) fn primary_route(&self) -> Option<&PrimaryRouteWitness> {
        self.primary_route.current(self.is_connected())
    }

    pub fn is_connected(&self) -> bool {
        match &self.backend {
            Backend::Disabled => false,
            Backend::Sdk(channel) => channel.state.is_healthy(),
            Backend::Peer(peer) => peer.is_connected(),
        }
    }

    /// Bind a signing identity, builder-style for chaining at boot.
    /// Replaces any previous identity.
    #[must_use]
    pub fn with_identity(mut self, identity: SigningIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Replace the bound identity in place, e.g. when the user switches
    /// identities in the chrome's identity picker without tearing down the manager.
    pub fn set_identity(&mut self, identity: SigningIdentity) {
        self.identity = Some(identity);
    }

    pub fn has_identity(&self) -> bool {
        self.identity.is_some()
    }

    /// Sign and send a CRDT mutation envelope. Best-effort like
    /// [`Self::send`]: an unsent frame isn't retried, but the next live
    /// edit republishes state, bounding the loss to one round-trip.
    ///
    /// # Errors
    ///
    /// Returns [`SignedSendError::NoIdentity`] when no signing identity is
    /// bound, [`SignedSendError::SequenceExhausted`] before a sequence could
    /// be reused, or [`SignedSendError::Canonical`] when `payload` fails to
    /// canonicalize for signing.
    pub fn send_signed_op(
        &mut self,
        payload: Vec<u8>,
        artifact: collab_types::ArtifactId,
        op_class: collab_types::OpClass,
    ) -> Result<(), SignedSendError> {
        let identity = self.identity.as_mut().ok_or(SignedSendError::NoIdentity)?;
        let envelope = identity.sign_op(payload, artifact, op_class)?;
        self.send(CollabRequest::SignedOperation {
            envelope: Box::new(envelope),
        });
        Ok(())
    }

    /// Fire-and-forget and non-blocking on every backend. Presence may be
    /// shed under saturation; any durable enqueue failure visibly disconnects
    /// the manager, so callers never continue under the illusion it was sent.
    pub fn send(&mut self, req: CollabRequest) -> bool {
        if self.missing_required_document_custody(&req) {
            return false;
        }
        if let CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            ..
        } = &req
        {
            self.primary_route.announce(RouteSpec {
                app_id: app_id.clone(),
                cluster_id: *cluster_id,
                record: record.clone(),
            });
            if let Backend::Sdk(channel) = &mut self.backend {
                channel.action_directory_published = false;
            }
        }
        if let Backend::Peer(peer) = &mut self.backend {
            let result = peer.send(req);
            let queued = result.is_ok();
            self.peer_result(result);
            return queued;
        } else if let Backend::Sdk(ch) = &self.backend {
            if let CollabRequest::NativeMediaCommand {
                request_id,
                command,
            } = req
            {
                return self
                    .primary_route
                    .current(ch.state.is_healthy())
                    .is_some_and(|primary| {
                        ch.media.as_ref().is_some_and(|media| {
                            media.send(primary.route.channel_id(), request_id, command)
                        })
                    });
            }
            if matches!(
                req,
                CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
            ) && !self.primary_route.accepted.as_ref().is_some_and(|witness| {
                ch.state
                    .document_reads
                    .publishable(&witness.route.channel_id())
            }) {
                return false;
            }
            return enqueue_sdk_command(&ch.outbound_tx, SdkCommand::Primary(req), &ch.state);
        }
        false
    }

    /// Enqueue a document-publication marker without waiting on the owner thread.
    /// The returned receipt covers the preceding queue prefix, not peer durability.
    ///
    /// # Errors
    /// Returns an error for unsupported/unavailable transports or a rejected marker.
    pub fn begin_document_publication_barrier(
        &self,
    ) -> Result<DocumentPublicationReceipt, DocumentPublicationError> {
        let channel = match &self.backend {
            Backend::Sdk(channel) => channel,
            Backend::Peer(_) => return Err(DocumentPublicationError::Unsupported),
            Backend::Disabled => return Err(DocumentPublicationError::Unavailable),
        };
        let (reply, receiver) = mpsc::sync_channel(1);
        if !enqueue_sdk_command(
            &channel.outbound_tx,
            SdkCommand::DocumentPublicationBarrier(reply),
            &channel.state,
        ) {
            return Err(channel
                .state
                .publication_result()
                .err()
                .unwrap_or(DocumentPublicationError::Unavailable));
        }
        Ok(DocumentPublicationReceipt {
            receiver,
            state: channel.state.clone(),
        })
    }

    /// Register the UI-owned pull directory on the primary broker route.
    /// Re-registering is harmless: the broker atomically replaces the old
    /// capability for this live connection.
    pub fn publish_action_directory(&mut self, app_id: impl Into<String>) {
        if let Backend::Peer(peer) = &mut self.backend {
            let result = peer.publish_action_directory(app_id.into());
            self.peer_result(result);
            return;
        }
        if let Backend::Sdk(ch) = &mut self.backend {
            if ch.action_directory_published {
                return;
            }
            if enqueue_sdk_command(
                &ch.outbound_tx,
                SdkCommand::PublishActionDirectory {
                    app_id: app_id.into(),
                    requests: ch.action_directory_tx.clone(),
                },
                &ch.state,
            ) {
                ch.action_directory_published = true;
            }
        }
    }

    /// Drain requests issued by the shovel-side `ActionDirectory` server. This
    /// must be called by [`crate::app::App`] on its owner thread.
    pub(crate) fn take_action_directory_requests(&mut self) -> Vec<ActionDirectoryRequest> {
        if let Backend::Peer(peer) = &mut self.backend {
            return peer.take_action_directory_requests();
        }
        let Backend::Sdk(ch) = &mut self.backend else {
            return Vec::new();
        };
        let mut requests = Vec::new();
        while let Ok(request) = ch.action_directory_rx.try_recv() {
            requests.push(request);
        }
        requests
    }

    /// Join and subscribe to an additional route without publishing traffic.
    /// Repeated joins of the same route are idempotent.
    pub fn join_route(&mut self, route: RouteSpec) {
        if let Backend::Peer(peer) = &mut self.backend {
            let result = peer.join_route(route);
            self.peer_result(result);
            return;
        }
        if let Backend::Sdk(ch) = &mut self.backend {
            let _ = enqueue_sdk_command(&ch.outbound_tx, SdkCommand::Join(route), &ch.state);
        }
    }

    /// Install an already-provisioned MLS endpoint for `route`.
    ///
    /// From this point the route is fail-closed: locally generated document
    /// mutations are encrypted and encrypted inbound envelopes are decrypted
    /// before becoming [`CollabEvent::Document`].  The channel id must be the
    /// deterministic id for the exact app/workspace/record route.  Group
    /// provisioning and durable secret storage intentionally remain outside
    /// the UI process; callers should obtain the group from the workspace
    /// identity service and must reinstall it after a process restart.
    ///
    /// # Errors
    ///
    /// Returns an error for a route/channel mismatch, an unavailable or
    /// stopped collaboration worker, or a route that cannot be joined.
    pub fn install_document_encryption(
        &mut self,
        route: RouteSpec,
        group: channel_crypto::ChannelGroup,
    ) -> Result<(), String> {
        let record = route.record.as_deref().unwrap_or("");
        let expected =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id.to_string(), record);
        if group.channel_id() != expected {
            return Err("MLS group does not belong to the requested document route".into());
        }
        if let Backend::Peer(peer) = &mut self.backend {
            return peer.install_document_encryption(route, group);
        }
        let Backend::Sdk(ch) = &mut self.backend else {
            return Err("collaboration broker is unavailable".into());
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if !enqueue_sdk_command(
            &ch.outbound_tx,
            SdkCommand::InstallEncryption {
                spec: route,
                group: Box::new(group),
                reply: reply_tx,
            },
            &ch.state,
        ) {
            return Err("collaboration worker is unavailable or overloaded".to_string());
        }
        reply_rx
            .recv()
            .map_err(|_| "collaboration worker stopped".to_string())?
    }

    /// Publish on an explicitly named additional route. The route is joined
    /// lazily and retained, and its inbound streams feed this manager's normal
    /// [`Self::poll`] queue.
    pub fn send_to_route(&mut self, route: RouteSpec, req: CollabRequest) {
        let _ = self.try_send_to_route(route, req);
    }

    /// Exact local enqueue outcome. The SDK's later RPC barrier still owns
    /// durable acknowledgment; callers must not interpret this as commitment.
    pub fn try_send_to_route(&mut self, route: RouteSpec, req: CollabRequest) -> bool {
        if self.missing_required_document_custody(&req) {
            return false;
        }
        if matches!(req, CollabRequest::Hello { .. }) {
            tracing::warn!("collab: send_to_route ignores Hello; use join_route");
            return false;
        }
        if let Backend::Peer(peer) = &mut self.backend {
            let result = peer.send_to_route(route, req);
            let queued = result.is_ok();
            self.peer_result(result);
            queued
        } else if let Backend::Sdk(ch) = &mut self.backend {
            if let CollabRequest::NativeMediaCommand {
                request_id,
                command,
            } = req
            {
                return ch
                    .media
                    .as_ref()
                    .is_some_and(|media| media.send(route.channel_id(), request_id, command));
            }
            if matches!(
                req,
                CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
            ) && !ch.state.document_reads.publishable(&route.channel_id())
            {
                let _ = enqueue_sdk_command(&ch.outbound_tx, SdkCommand::Join(route), &ch.state);
                return false;
            }
            enqueue_sdk_command(&ch.outbound_tx, SdkCommand::ToRoute(route, req), &ch.state)
        } else {
            false
        }
    }

    pub(crate) fn document_publication_failed(&self) -> bool {
        matches!(&self.backend, Backend::Sdk(channel) if channel.state.publication_result().is_err())
    }

    #[cfg(test)]
    pub(crate) fn fail_publication_for_test(&self) {
        if let Backend::Sdk(channel) = &self.backend {
            channel
                .state
                .fail_closed("test terminal publication invalidation");
        }
    }

    pub(crate) fn stop_after_publication_failure(&mut self) {
        if let Backend::Sdk(channel) = &self.backend {
            channel
                .state
                .fail_closed("pre-terminal document publication; outcome unknown");
        }
        // Drop the sender as well as withdrawing health, so an idle shovel
        // wakes and closes its capability instead of awaiting another command.
        self.backend = Backend::Disabled;
        self.primary_route.accepted = None;
    }

    /// Drain every event pending on the current backend. Non-blocking.
    pub fn poll(&mut self) -> Vec<CollabEvent> {
        if let Backend::Peer(peer) = &mut self.backend
            && let Some(result) = peer.take_primary_route_result()
            && let Err(error) = self.primary_route.accept(result)
        {
            self.peer_result(Err(error));
        }
        match &mut self.backend {
            Backend::Disabled => Vec::new(),
            Backend::Peer(peer) => peer.poll(),
            Backend::Sdk(ch) => {
                ch.state.begin_poll();
                let mut events = Vec::new();
                while let Ok(evt) = ch.inbound_rx.try_recv() {
                    events.push(evt.event);
                }
                events
            }
        }
    }
}

/// The presence handle announced on `Hello`. `TILO_PRESENCE_HANDLE` wins,
/// then `TILO_USER_ID`; absent both, the broker's synthetic name.
pub fn presence_handle_env() -> Option<String> {
    ["TILO_PRESENCE_HANDLE", "TILO_USER_ID"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
}

/// The routing key one connection names on the capnp transport.
///
/// An app's traffic is addressed by `(app_id, cluster_id, record_id)`,
/// but capnp's `joinChannel(channelId, identity)` carries a single
/// string, so the triple travels *inside* the channel id. This pair of
/// functions is the only place the encoding is written down, so the two
/// halves cannot drift apart.
///
/// # Shape
///
/// ```text
/// <cluster>[#app:<app_id>][#record:<record_id>]
/// ```
///
/// A uuid contains no `#`. `#record:` is stripped first and takes
/// everything after it, so a `#`-containing record id (a file path may
/// have one) stays intact. Both suffixes are optional, so a missing
/// `#app:` yields an empty app id rather than failing to parse — needed
/// for the earlier two-field keys chrome traffic still rides.
///
/// Without the app id in this key, every packaged app would publish
/// deltas cluster-scoped — one app's changes delivered to every app in
/// the room.
pub mod channel_key {
    /// Encode a route triple into one channel id.
    pub fn format(app_id: &str, cluster_id: &str, record_id: &str) -> String {
        let mut key = String::from(cluster_id);
        if !app_id.is_empty() {
            key.push_str("#app:");
            key.push_str(app_id);
        }
        if !record_id.is_empty() {
            key.push_str("#record:");
            key.push_str(record_id);
        }
        key
    }

    /// Decode a channel id into `(app_id, cluster_id, record_id)`. Total:
    /// every string decodes, and one naming no marker is a bare cluster.
    pub fn parse(key: &str) -> (String, String, String) {
        let (head, record_id) = match key.split_once("#record:") {
            Some((head, record)) => (head, record.to_string()),
            None => (key, String::new()),
        };
        let (cluster, app_id) = match head.split_once("#app:") {
            Some((cluster, app)) => (cluster, app.to_string()),
            None => (head, String::new()),
        };
        (app_id, cluster.to_string(), record_id)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trips_every_combination_of_present_fields() {
            for (app, cluster, record) in [
                ("", "c", ""),
                ("app", "c", ""),
                ("", "c", "rec"),
                ("app", "c", "rec"),
            ] {
                let key = format(app, cluster, record);
                assert_eq!(
                    parse(&key),
                    (app.to_string(), cluster.to_string(), record.to_string()),
                    "key {key:?} did not round-trip",
                );
            }
        }

        #[test]
        fn keys_written_before_the_app_segment_still_parse() {
            // Both must decode to a cluster-scoped address rather than
            // nothing, or an app that hasn't been rebuilt stops routing.
            assert_eq!(
                parse("cluster-1"),
                (String::new(), "cluster-1".into(), String::new()),
            );
            assert_eq!(
                parse("cluster-1#record:/notes/a.md"),
                (String::new(), "cluster-1".into(), "/notes/a.md".into()),
            );
        }

        #[test]
        fn a_record_id_containing_a_hash_survives() {
            // Record ids are file paths and a path may contain `#`.
            // Stripping `#record:` first and keeping the whole tail is
            // what makes that safe.
            let key = format("app", "c", "/notes/draft#2.md");
            assert_eq!(
                parse(&key),
                ("app".into(), "c".into(), "/notes/draft#2.md".into()),
            );
        }
    }
}

/// The `(app_id, cluster_id)` pair this process announces in
/// [`CollabRequest::Hello`] — the broker delivers a CRDT delta only to
/// connections announcing the same pair.
///
/// # Precedence (each half resolves independently)
///
/// 1. `TILO_DURABLE_APP_ID` (falling back to `TILO_PACKAGE_ID`) /
///    `TILO_WORKSPACE_ID` — launcher-set for every compositor-started app.
///    The separate durable id preserves an existing route when the signed
///    package registration was renamed.
/// 2. `QUICK_APP_ID` / `QUICK_CLUSTER_ID` — standalone-dev override,
///    ranked below `TILO_PACKAGE_ID` so a dev identity never fragments
///    routing under a real session.
/// 3. A fresh random app id + the nil cluster — fails closed: nothing
///    else announces the same id, so nothing is ever delivered here.
///
/// `TILO_WORKSPACE_ID` counts as the cluster only when it parses as a
/// uuid — it's also read elsewhere as a bare "am I on Tilo?" flag, so an
/// unroutable value falls through instead of being rejected.
fn auto_hello_env() -> (String, Uuid) {
    auto_hello_env_uncached()
}

/// The person sitting at this machine. Actions are stamped with this so
/// undo can tell collaborators apart (see
/// [`crate::state::State::undo_as`]): the id must be **stable** across a
/// document's life and **distinct** between people, or one person's
/// Ctrl+Z reverts another's work.
///
/// Mirrors the shell-side `local_user_id` ladder rung for rung (plus a
/// sandboxed-app rung) so a compositor-granted capability and an
/// app-stamped undo entry always name the same subject:
///
/// 1. `TILO_USER_ID` — explicit override; the shell reads it first too.
/// 2. The launcher-bound runtime identity (`TILO_USER_IDENTITY`) — the
///    rung a packaged app actually lands on, since `/var/lib/tilo` is
///    outside its sandbox.
/// 3. `$XDG_CONFIG_HOME/tilo/identity/primary.json`, for the shell's
///    own unsandboxed in-process apps.
/// 4. `/var/lib/tilo/user-identity/pubkey.bin`.
/// 5. The OS account name, then a constant.
///
/// Not memoised — see [`auto_hello_env_uncached`]: a process hosting
/// more than one identity must not be frozen into the first one.
pub fn local_user_id() -> crate::permissions::UserId {
    crate::permissions::UserId(resolve_local_user_id())
}

fn resolve_local_user_id() -> String {
    if let Some(v) = non_empty_env("TILO_USER_ID") {
        return v;
    }
    #[cfg(unix)]
    {
        if let Some(id) = runtime_identity::load().ok().or_else(|| {
            config_identity_path().and_then(|p| runtime_identity::load_from_path(p).ok())
        }) {
            return person_id_from_key(id.group_public_key());
        }
    }
    if let Ok(bytes) = std::fs::read("/var/lib/tilo/user-identity/pubkey.bin")
        && let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return person_id_from_key(key);
    }
    non_empty_env("USER").unwrap_or_else(|| "local".to_string())
}

/// `$XDG_CONFIG_HOME/tilo/identity/primary.json`, else
/// `$HOME/.config/tilo/identity/primary.json`.
#[cfg(unix)]
fn config_identity_path() -> Option<std::path::PathBuf> {
    let base = match non_empty_env("XDG_CONFIG_HOME") {
        Some(v) => std::path::PathBuf::from(v),
        None => std::path::PathBuf::from(non_empty_env("HOME")?).join(".config"),
    };
    let p = base.join("tilo").join("identity").join("primary.json");
    p.is_file().then_some(p)
}

/// A public key rendered as a person, in the one canonical form — so this
/// string and the compositor's string for the same key are the same string.
fn person_id_from_key(key: [u8; 32]) -> String {
    // `person_from_key` constructs `Person` unconditionally, so the else
    // branch cannot run. It recomputes the same canonical lowercase-hex form
    // directly rather than panic on a branch that should never be reached.
    if let crate::permissions::Principal::Person(id) =
        crate::permissions::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(key),
        )
    {
        id
    } else {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(64);
        for b in &key {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

/// The application's authenticated durable route (falling back to its signed
/// package id, then `QUICK_APP_ID` for dev). Read from the same place the
/// channel key is built, so what an app publishes can't disagree with its
/// connection.
pub fn local_app_id() -> String {
    auto_hello_env().0
}

/// The room this process collaborates in. Exposed so presence scopes
/// from the same source routing uses, rather than a derivation that
/// could drift.
///
/// `Uuid::nil()` means no workspace — the broker rejects it
/// deliberately, so callers must treat it as "no room", not a room.
pub fn local_cluster_id() -> Uuid {
    auto_hello_env().1
}

fn auto_hello_env_uncached() -> (String, Uuid) {
    // Only the random fallback id is memoised — caching the env-derived
    // answer would freeze every later reader into the first caller's
    // room, since env is the routing truth (a property of the launch, not
    // the process image) and can differ per manager in one process (every
    // collaboration test does this).
    static FALLBACK_APP_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    let app_id = non_empty_env("TILO_DURABLE_APP_ID")
        .or_else(|| non_empty_env("TILO_PACKAGE_ID"))
        .or_else(|| non_empty_env("QUICK_APP_ID"))
        .unwrap_or_else(|| {
            FALLBACK_APP_ID
                .get_or_init(|| Uuid::new_v4().to_string())
                .clone()
        });

    let cluster_id = non_empty_env("TILO_WORKSPACE_ID")
        .and_then(|s| {
            if let Ok(id) = Uuid::parse_str(&s) {
                Some(id)
            } else {
                tracing::debug!(
                    "TILO_WORKSPACE_ID={s:?} is not a uuid; \
                 falling back to QUICK_CLUSTER_ID for collaboration routing"
                );
                None
            }
        })
        .or_else(|| non_empty_env("QUICK_CLUSTER_ID").and_then(|s| Uuid::parse_str(&s).ok()))
        .unwrap_or_else(Uuid::nil);

    (app_id, cluster_id)
}

/// `std::env::var` with empty treated as unset — an explicit `""` is
/// the shape an unset forwarded shell variable produces, and taking it
/// literally would put every such app in the same (empty-id) room.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// One-shot init result from the shovel thread's startup, letting
/// `spawn_sdk` decide immediately between `SdkChannel` and `Disabled`.
enum InitStatus {
    Ready,
    Failed(runtime_portal::NotAvailable),
}

/// The shovel thread's body: owns a `current_thread` runtime + `LocalSet`
/// (capnp-rpc futures are `!Send`), stands up the SDK client, subscribes
/// its three streams, and forwards outbound requests. Exits when
/// `outbound_rx.recv()` returns `None` (manager dropped).
fn run_shovel(
    source: SdkConnectSource,
    identity: Option<SdkJoinIdentity>,
    outbound_rx: tokio_mpsc::Receiver<QueuedSdkCommand>,
    media_rx: native_media::Receiver,
    inbound_tx: InboundBridge,
    init_tx: mpsc::SyncSender<InitStatus>,
) {
    // Building `current_thread` is fallible (can OOM); surface as
    // Disabled — no service means the app runs local-only.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = init_tx.send(InitStatus::Failed(
                runtime_portal::NotAvailable::ConnectionFailed(e),
            ));
            return;
        }
    };

    let local = LocalSet::new();
    local.block_on(&rt, async move {
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
                let _ = init_tx.send(InitStatus::Failed(e));
                return;
            }
        };
        // The transport is ready. The manager queues its primary Hello next
        // and waits for `PrimaryReady` before returning it to the caller.
        let _ = init_tx.send(InitStatus::Ready);

        drive_shovel(client, identity, outbound_rx, media_rx, inbound_tx).await;
    });
}

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
    let mut routes = std::collections::HashMap::<String, SdkRoute>::new();
    let mut primary: Option<String> = None;
    let mut participant_handle: Option<String> = None;
    let mut primary_result = Err("primary collaboration route was not announced".to_string());

    while let Some(QueuedSdkCommand {
        command,
        _reservation,
    }) = outbound_rx.recv().await
    {
        if !inbound_tx.state.is_healthy() {
            break;
        }
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
                        &media_routes,
                        &spec,
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
                let Some(key) = primary.as_ref() else {
                    tracing::warn!("collab: request sent before the primary route joined");
                    if matches!(
                        request,
                        CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
                    ) {
                        inbound_tx
                            .state
                            .fail_closed("document primary route unavailable");
                        break;
                    }
                    continue;
                };
                if let Some(route) = routes.get_mut(key) {
                    dispatch_sdk_request(route, request, &mut signer, &inbound_tx).await;
                    if !inbound_tx.state.is_healthy() {
                        break;
                    }
                } else if matches!(
                    request,
                    CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
                ) {
                    inbound_tx
                        .state
                        .fail_closed("document primary handle unavailable");
                    break;
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
                    &media_routes,
                    &spec,
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
                    &media_routes,
                    &spec,
                )
                .await;
                if joined.is_ok()
                    && let Some(route) = routes.get_mut(&key)
                {
                    dispatch_sdk_request(route, request, &mut signer, &inbound_tx).await;
                    if !inbound_tx.state.is_healthy() {
                        break;
                    }
                } else {
                    inbound_tx.state.fail_closed("document route join");
                }
            }
            SdkCommand::PrimaryReady(reply) => {
                let _ = reply.send(primary_result.clone());
            }
            SdkCommand::DocumentPublicationBarrier(reply) => {
                // A superseded receipt may have no receiver. Its prefix still ran.
                let _ = reply.send(inbound_tx.state.publication_result());
            }
            SdkCommand::InstallEncryption { spec, group, reply } => {
                let result = install_sdk_encryption(
                    SdkJoinContext {
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
        context.media_routes,
        &spec,
    )
    .await?;
    let route = routes
        .get_mut(&key)
        .ok_or_else(|| "joined MLS route disappeared".to_string())?;
    *route
        .encryption
        .lock()
        .map_err(|_| "MLS endpoint state is unavailable after a poisoned lock".to_string())? =
        Some(group);
    Ok(())
}

struct SdkJoinContext<'a> {
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
    encryption: std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
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
    media_routes: &native_media::Routes,
    spec: &RouteSpec,
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

    // The catalogue is level-triggered and its first snapshot is the route's
    // decision baseline. Enqueue it before subscribing to document traffic,
    // so a backlogged snapshot cannot make an agent decide against stale
    // local fallback data immediately after construction.
    spawn_action_catalogue_stream(&channel, inbound_tx).await?;
    let encryption = std::sync::Arc::new(std::sync::Mutex::new(None));
    let committed = committed_document::install(&channel, spec, inbound_tx).await?;
    if !committed {
        spawn_op_stream(&channel, inbound_tx, std::sync::Arc::clone(&encryption)).await;
    }
    spawn_context_stream(&channel, spec.clone(), inbound_tx).await;
    spawn_action_stream(&channel, inbound_tx).await;
    spawn_invocation_stream(&channel, inbound_tx).await;
    spawn_realtime_stream(&channel, inbound_tx).await;
    // This intentionally fails quietly for person sessions. An admitted
    // agent gets a separate broker-addressed queue; a document agent never
    // has to infer task work from room traffic.
    if std::env::var_os("TILO_MANAGED_AGENT_CONTEXT").is_none() {
        spawn_task_run_stream(&channel, inbound_tx).await;
    }
    let presence = spawn_presence_stream(&channel, inbound_tx).await;
    // Subscriptions are installed first so an existing holder's response
    // cannot race ahead of our document stream.
    if !committed && let Err(error) = channel.request_document_snapshot().await {
        tracing::warn!(%error, "collab: late-join snapshot request failed");
    }

    media_routes.insert(channel_id.clone(), channel.clone());
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
    encryption: std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
) {
    if let Ok(stream) = channel.subscribe().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(event) = stream.read().await {
                let translated = match postcard::from_bytes::<SignedOp>(&event.op) {
                    Ok(envelope) => {
                        match signed_document_event(envelope, event.from_user_id, &encryption) {
                            Ok(event) => event,
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
                if !tx.send_durable(translated) {
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
async fn spawn_invocation_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_stable_invocations().await {
        let tx = inbound_tx.clone();
        let cancellation_channel = channel.clone();
        tokio::task::spawn_local(async move {
            while let Ok(invocation) = stream.read().await {
                let id = invocation.id;
                let context = parse_invocation_context(id, &invocation.context_json);
                if !tx.send_durable(CollabEvent::Invocation {
                    id,
                    invocation: invocation
                        .invocation_uuid
                        .map(|bytes| tilo_actions::InvocationId(uuid::Uuid::from_bytes(bytes))),
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
                        tokio::time::sleep(crate::clock::Duration::from_millis(100)).await;
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
    let catalogue = serde_json::from_str::<BrokerActionCatalogueWire>(&initial)
        .map_err(|error| format!("decoding initial action catalogue failed: {error}"))?;
    if !inbound_tx.send_durable(CollabEvent::ActionCatalogue {
        surfaces: catalogue.surfaces,
    }) {
        return Err("delivering initial action catalogue failed".to_string());
    }

    let tx = inbound_tx.clone();
    tokio::task::spawn_local(async move {
        while let Ok(snapshot) = stream.read().await {
            match serde_json::from_str::<BrokerActionCatalogueWire>(&snapshot) {
                Ok(catalogue) => {
                    if !tx.send_durable(CollabEvent::ActionCatalogue {
                        surfaces: catalogue.surfaces,
                    }) {
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "collab: invalid broker action catalogue");
                }
            }
        }
    });
    Ok(())
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

fn next_certified_document_seq(now: crate::clock::SystemTime) -> u64 {
    use std::sync::atomic::Ordering;

    let wall = now
        .duration_since(crate::clock::SystemTime::UNIX_EPOCH)
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
/// downgrades the document to unsigned traffic.
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
            .is_ok_and(|encryption| encryption.is_none())
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
    let workspace_id = route.spec.cluster_id.to_string();
    let record_id = route.spec.record.as_deref().unwrap_or("");
    let cleartext = match mutation.encode() {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(%error, "collab: typed document mutation would not encode; dropping");
            return Err(inbound.state.publication_failed(
                "document signing or encoding",
                error,
                true,
            ));
        }
    };
    let Some((payload, encryption_metadata)) =
        prepare_document_payload(&route.encryption, cleartext)
    else {
        return Err(inbound.state.publication_failed(
            "document encryption",
            "payload unavailable",
            true,
        ));
    };
    let now = crate::clock::SystemTime::now();
    let author = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Host(_)) => {
            unreachable!("host commits above")
        }
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            identity.signing_public_key()
        }
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(key)) => {
            key.verifying_key().to_bytes()
        }
        LocalSdkDocumentSigner::Host(identity) => identity.signing_public_key(),
    };
    let artifact = collab_types::document_artifact_id(&route.spec.app_id, &workspace_id, record_id);
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
            return Err(inbound.state.publication_failed(
                "document signing or encoding",
                error,
                true,
            ));
        }
    };
    let signature = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Host(_)) => {
            unreachable!("host commits above")
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
                return Err(inbound.state.publication_failed(
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
                return Err(inbound.state.publication_failed(
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
                    return Err(inbound.state.publication_failed(
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
        Ok(bytes) => {
            if let Err(error) = route.channel.send(&bytes).await {
                tracing::warn!(%error, "collab: document commit failed; outcome unknown");
                return Err(inbound
                    .state
                    .publication_failed("document commit RPC", error, true));
            }
        }
        Err(error) => {
            tracing::warn!(%error, "collab: signed document envelope would not encode; dropping");
            return Err(inbound
                .state
                .publication_failed("document encoding", error, true));
        }
    }
    Ok(())
}

fn prepare_document_payload(
    encryption: &std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
    cleartext: Vec<u8>,
) -> Option<(Vec<u8>, Option<collab_types::EncryptionMetadata>)> {
    let Ok(mut guard) = encryption.lock() else {
        tracing::warn!("collab: MLS endpoint lock poisoned; dropping mutation");
        return None;
    };
    let Some(group) = guard.as_mut() else {
        return Some((cleartext, None));
    };
    let epoch = group.epoch();
    match group.encrypt(&cleartext) {
        Ok(ciphertext) => Some((
            ciphertext,
            Some(collab_types::EncryptionMetadata {
                epoch,
                channel: group.channel_id(),
            }),
        )),
        Err(error) => {
            tracing::warn!(%error, "collab: MLS encryption failed; dropping mutation");
            None
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

type TaskActionResult = Result<(u64, tilo_actions::InvocationUpdate), String>;

fn parse_task_update(id: u64, update: &str) -> TaskActionResult {
    serde_json::from_str(update)
        .map(|update| (id, update))
        .map_err(|error| error.to_string())
}

/// Project a stepped task reply onto the event fields: a committed step
/// carries its continuation; an older broker's legacy reply carries none; a
/// pending or lost step asks the caller to re-send the identical step.
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
        Ok(runtime_collab::TaskStepReply::Legacy { id, update_json }) => {
            (parse_task_update(id, &update_json), None, false)
        }
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
) {
    if matches!(
        request,
        CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
    ) && wait_for_document_publication(route, &inbound_tx.state)
        .await
        .is_err()
    {
        return;
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
            let _ =
                send_document_mutation(route, signer, mutation, authorization, inbound_tx).await;
        }
        CollabRequest::RequestDocumentSnapshot => {
            if route.committed {
                return;
            }
            if route.channel.request_document_snapshot().await.is_err() {
                inbound_tx
                    .state
                    .fail_closed("document snapshot request RPC");
            }
        }
        CollabRequest::Presence { key, payload } => {
            let Some(presence) = route.presence.as_ref() else {
                return;
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
            let (ops, decrypted_payloads, exhausted) = match route
                .channel
                .fetch_scrollback_for(artifact.0.as_bytes(), before_seq, u32::from(max))
                .await
            {
                Ok(batch) => {
                    let mut payloads = Vec::new();
                    let ops = batch
                        .ops
                        .into_iter()
                        .filter_map(|bytes| postcard::from_bytes::<SignedOp>(&bytes).ok())
                        .filter_map(|op| match decrypt_document_payload(&op, &route.encryption) {
                            Ok(payload) => {
                                payloads.push(payload);
                                Some(Box::new(op))
                            }
                            Err(error) => {
                                tracing::warn!(%error, "collab: dropping undecryptable scrollback envelope");
                                None
                            }
                        })
                        .collect();
                    (ops, payloads, batch.done)
                }
                Err(_) => (Vec::new(), Vec::new(), true),
            };
            let _ = inbound_tx.send_durable(CollabEvent::ScrollbackBatch {
                artifact,
                ops,
                decrypted_payloads,
                exhausted,
            });
        }
        CollabRequest::PublishActions { app_id, actions } => {
            let actions = actions
                .into_iter()
                .map(|action| runtime_collab::ExportedAction {
                    resource: action.resource,
                    action: action.action,
                    resource_kind: action.resource_kind,
                    resource_label: action.resource_label,
                    action_label: action.action_label,
                    description: action.description,
                    operation_kind: action.operation_kind,
                    params_json: action.params_json,
                    contract_json: action.contract_json,
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
                        inbound_tx
                            .state
                            .fail_closed("invocation update outcome unknown");
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
        #[cfg(unix)]
        CollabRequest::ManagedInvocationAttempt { attempt } => {
            use crate::host_worker::managed_sender::{AttemptFailure, RequestView};
            use managed_sender_transport::{Failure, Request};
            let result = match attempt.claim(&route.spec) {
                None => return, // A clone cannot dispatch the same local attempt twice.
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
            let (result, continuation, retry_step) = match step {
                Some(step) => task_step_event(
                    route
                        .channel
                        .invoke_task_step(
                            run_id,
                            &lease,
                            step,
                            peer_id,
                            &resource,
                            &action,
                            Some(&argument),
                        )
                        .await
                        .map_err(|error| error.to_string()),
                ),
                None => (
                    route
                        .channel
                        .invoke_task_action(
                            run_id,
                            &lease,
                            peer_id,
                            &resource,
                            &action,
                            Some(&argument),
                            "",
                        )
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
}
fn signed_document_event(
    envelope: SignedOp,
    from_user_id: String,
    encryption: &std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
) -> Result<CollabEvent, collab_types::DocumentMutationError> {
    let decrypted_payload = decrypt_document_payload(&envelope, encryption)?;
    Ok(CollabEvent::Document {
        envelope: Box::new(envelope),
        from_user_id,
        decrypted_payload,
    })
}

fn decrypt_document_payload(
    envelope: &SignedOp,
    encryption: &std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
) -> Result<Option<Vec<u8>>, collab_types::DocumentMutationError> {
    if let Some(metadata) = envelope.encryption {
        let mut guard = encryption
            .lock()
            .map_err(|_| collab_types::DocumentMutationError::RouteMismatch)?;
        let group = guard
            .as_mut()
            .ok_or(collab_types::DocumentMutationError::RouteMismatch)?;
        if group.channel_id() != metadata.channel || group.epoch() < metadata.epoch {
            return Err(collab_types::DocumentMutationError::RouteMismatch);
        }
        let plaintext = group
            .decrypt(&envelope.payload)
            .map_err(|_| collab_types::DocumentMutationError::RouteMismatch)?;
        drop(guard);
        let _mutation = collab_types::DocumentMutation::decode_versioned(&plaintext)?;
        Ok(Some(plaintext))
    } else {
        if encryption
            .lock()
            .map_err(|_| collab_types::DocumentMutationError::RouteMismatch)?
            .is_some()
        {
            // Once provisioned, accepting a plaintext envelope would be an
            // active downgrade. There is no opportunistic fallback.
            return Err(collab_types::DocumentMutationError::RouteMismatch);
        }
        let _mutation = collab_types::DocumentMutation::decode_versioned(&envelope.payload)?;
        Ok(None)
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
mod task4_route_tests {
    use super::{PrimaryRouteState, RouteSpec};
    #[test]
    fn task4_primary_witness_requires_latest_success_and_current_health() {
        let a = RouteSpec::record("a", uuid::Uuid::nil(), "record-a");
        let b = RouteSpec::record("b", uuid::Uuid::nil(), "record-b");
        let mut state = PrimaryRouteState::default();
        assert!(state.current(true).is_none());
        state.announce(a.clone());
        state.accept(Ok(a.clone())).unwrap();
        let first = state.current(true).unwrap().clone();
        assert_eq!(first.route, a);
        assert!(state.current(false).is_none());
        state.announce(a.clone());
        assert!(state.current(true).is_none());
        state.accept(Ok(a.clone())).unwrap();
        assert_ne!(state.current(true).unwrap().epoch, first.epoch);
        state.announce(b.clone());
        assert!(state.accept(Ok(a)).is_err());
        assert!(state.current(true).is_none());
        assert!(state.accept(Err("refused".into())).is_err());
        assert!(state.current(true).is_none());
        state.accept(Ok(b)).unwrap();
        assert!(state.current(true).is_some());
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
    #[cfg(unix)]
    include!("collaboration/invocation_custody_tests.rs");
    #[cfg(unix)]
    include!("collaboration/directory_client_tests.rs");

    use crate::clock::{Duration, Instant};
    use std::cell::RefCell;
    #[cfg(unix)]
    use std::os::fd::IntoRawFd;
    use std::rc::Rc;

    use capnp::capability::Promise;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    #[cfg(unix)]
    use tokio::net::UnixStream as TokioUnixStream;
    use tokio::task::LocalSet as TokioLocalSet;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    use runtime_collab::PresencePayload as SdkPayload;
    use runtime_collab::raw::{
        action_catalogue_stream, channel_context, channel_handle, collab_service, context_stream,
        presence_event, presence_handle, presence_payload, presence_stream, scrollback_batch,
        stream,
    };

    #[derive(Default)]
    struct Captures {
        #[cfg(unix)]
        directory: DirectoryCaptures,
        committed_history_fixture: Option<Vec<u8>>,
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

    struct FakeActionCatalogueStream {
        initial: RefCell<Option<String>>,
    }

    impl action_catalogue_stream::Server for FakeActionCatalogueStream {
        fn read(
            self: Rc<Self>,
            _params: action_catalogue_stream::ReadParams,
            mut results: action_catalogue_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let Some(initial) = self.initial.borrow_mut().take() else {
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            };
            results.get().set_catalogue_json(&initial);
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
                let reply = match captures.committed_history_fixture.as_ref() {
                    None => Reply::Legacy,
                    Some(bytes) => match request {
                        Request::Open {
                            known_revision: Some(revision),
                        } if revision == [7; 32] => Reply::Unchanged { revision },
                        Request::Open { .. } => Reply::Chunk {
                            capture: 1,
                            revision: [7; 32],
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
            _params: channel_handle::UpdateInvocationParams,
            _results: channel_handle::UpdateInvocationResults,
        ) -> Promise<(), capnp::Error> {
            let mut captures = self.captures.lock().unwrap();
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
            let client: action_catalogue_stream::Client =
                capnp_rpc::new_client(FakeActionCatalogueStream {
                    initial: RefCell::new(Some(
                        serde_json::json!({"version": 1, "surfaces": []}).to_string(),
                    )),
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
        use runtime_identity::wire::Response;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
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
            for response in responses {
                let mut len = [0; 4];
                if server.read_exact(&mut len).await.is_err() {
                    return;
                }
                let body_len = u32::from_le_bytes(len) as usize;
                let mut body = vec![0; body_len];
                if server.read_exact(&mut body).await.is_err() {
                    return;
                }
                captured.lock().unwrap().extend_from_slice(&len);
                captured.lock().unwrap().extend_from_slice(&body);
                let frame = postcard::to_allocvec(&response).unwrap();
                if server
                    .write_all(&u32::try_from(frame.len()).unwrap().to_le_bytes())
                    .await
                    .is_err()
                    || server.write_all(&frame).await.is_err()
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
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
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
            .recv_timeout(Duration::from_secs(2))
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

    fn assert_document_publication_failure(manager: &CollaborationManager, expected: &str) {
        let error = manager
            .begin_document_publication_barrier()
            .and_then(|receipt| receipt.wait(Some(Instant::now() + Duration::from_secs(2))))
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }

    #[test]
    fn host_connector_uses_certified_presentation_and_broker_document_policy() {
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let broker_captures = captures.clone();
        let (requests_tx, requests_rx) = mpsc::sync_channel(1);
        let cluster_id = Uuid::new_v4();
        let connector = HostSessionConnector::new(move |_| async move {
            let (identity, requests) = host_identity_fixture(
                Some(b"fixture certificate".to_vec()),
                vec![
                    runtime_identity::wire::Response::Signature(vec![7; 64]),
                    runtime_identity::wire::Response::Denied("fixture role revoked".into()),
                    runtime_identity::wire::Response::Signature(vec![8; 64]),
                ],
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
        let requests = requests_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        manager.send(CollabRequest::document_incremental(
            b"fixture document change".to_vec(),
        ));
        assert_document_publication_failure(&manager, "fixture role revoked");
        let requests = host_identity_requests(&requests.lock().unwrap());
        assert!(
            matches!(&requests[1], runtime_identity::wire::Request::CollaborationJoin { workspace_id } if workspace_id == &cluster_id.to_string())
        );
        match &requests[2] {
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
        assert_document_publication_failure(&manager, "fixture role revoked");
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
            attempt_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            attempt_rx.try_recv().is_err(),
            "must not retry another route"
        );
        stop_rx
            .recv_timeout(Duration::from_secs(2))
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
        let started = Instant::now();
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
            attempt_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            attempt_rx.try_recv().is_err(),
            "must not retry another route"
        );
        stop_rx
            .recv_timeout(Duration::from_secs(2))
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
        assert!(
            wait_until(|| !manager.is_connected(), Duration::from_secs(2)),
            "SDK stayed healthy after owner document RPC failed"
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
        assert!(wait_until(
            || !manager.is_connected(),
            Duration::from_secs(2)
        ));
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
        assert!(wait_until(
            || matches!(&manager.backend, Backend::Sdk(channel)
            if channel.state.document_reads.publishable(&secondary.channel_id())),
            Duration::from_secs(2)
        ));
        assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
        let first = manager.begin_document_publication_barrier().unwrap();
        assert!(manager.try_send_to_route(
            secondary,
            CollabRequest::document_incremental(b"second".to_vec())
        ));
        let second = manager.begin_document_publication_barrier().unwrap();
        let mut gates = gates.into_iter();
        let (entered, release) = gates.next().unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(first.try_complete(), Ok(None));
        assert_eq!(second.try_complete(), Ok(None));
        release.send(Ok(())).unwrap();
        let (entered, release) = gates.next().unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        // The second RPC can only begin after the first marker has resolved.
        assert_eq!(first.wait(Some(Instant::now())), Ok(()));
        assert_eq!(second.try_complete(), Ok(None));
        release.send(Ok(())).unwrap();
        assert_eq!(
            second.wait(Some(Instant::now() + Duration::from_secs(2))),
            Ok(())
        );
        assert_eq!(captures.lock().unwrap().signed_ops.len(), 2);
        assert!(manager.is_connected());
        drop(manager);
        thread.join().unwrap();
    }

    #[cfg(unix)]
    struct RefusedDocumentIdentity;

    #[cfg(unix)]
    impl SdkCertifiedIdentity for RefusedDocumentIdentity {
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
                signer: SdkDocumentSigner::Certified(Box::new(RefusedDocumentIdentity)),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::disabled();
        manager.backend = Backend::Sdk(sdk);
        assert!(manager.send(CollabRequest::Hello {
            app_id: "worker".into(),
            cluster_id: Uuid::new_v4(),
            record: None,
            handle: None,
        }));
        manager.await_primary_route().unwrap();
        assert!(manager.send(CollabRequest::document_incremental(b"refused".to_vec())));
        // Signing may fail before the marker enqueue; either path must carry the cause.
        let error = match manager.begin_document_publication_barrier() {
            Ok(receipt) => receipt
                .wait(Some(Instant::now() + Duration::from_secs(2)))
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
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(manager.send(CollabRequest::document_incremental(
            b"retained original".to_vec()
        )));
        let state = match &manager.backend {
            Backend::Sdk(sdk) => sdk.state.clone(),
            _ => unreachable!(),
        };
        let key = manager.primary_route().unwrap().route.channel_id();
        let ready = state.document_reads.close_for_test(&key);
        let receipt = manager.begin_document_publication_barrier().unwrap();
        release.send(Ok(())).unwrap();
        assert!(wait_until(
            || captures.lock().unwrap().signed_ops.len() == 1,
            Duration::from_secs(2)
        ));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            receipt.try_complete(),
            Ok(None),
            "accepted publication must hold the barrier while hydration is pending"
        );
        assert!(state.command_bytes.0.load(Ordering::Acquire) >= b"retained original".len());
        ready.store(true, Ordering::Release);
        assert_eq!(
            receipt.wait(Some(Instant::now() + Duration::from_secs(2))),
            Ok(())
        );
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
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(manager.send(CollabRequest::document_incremental(b"must replay".to_vec())));
        let state = match &manager.backend {
            Backend::Sdk(sdk) => sdk.state.clone(),
            _ => unreachable!(),
        };
        let ready = state
            .document_reads
            .close_for_test(&manager.primary_route().unwrap().route.channel_id());
        let receipt = manager.begin_document_publication_barrier().unwrap();
        release.send(Ok(())).unwrap();
        assert!(wait_until(
            || captures.lock().unwrap().signed_ops.len() == 1,
            Duration::from_secs(2)
        ));
        if invalidate {
            state.fail_closed("actual test worker invalidation");
        }
        let error = receipt
            .wait(Some(Instant::now() + Duration::from_secs(5)))
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
        assert!(wait_until(
            || state.command_bytes.0.load(Ordering::Acquire) == 0,
            Duration::from_secs(2)
        ));
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
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let receipt = manager.begin_document_publication_barrier().unwrap();
        release.send(Err("document RPC refused".into())).unwrap();
        let error = receipt
            .wait(Some(Instant::now() + Duration::from_secs(2)))
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
        manager.backend = Backend::Sdk(SdkChannel {
            outbound_tx,
            media: None,
            inbound_rx,
            state,
            action_directory_rx,
            action_directory_tx,
            action_directory_published: false,
            _thread: None,
        });
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

    #[test]
    fn document_publication_completed_prefix_is_not_poisoned_by_later_failure() {
        let state = BridgeState::new();
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = DocumentPublicationReceipt {
            receiver,
            state: state.clone(),
        };
        reply.send(state.publication_result()).unwrap();
        state.record_publication_failure(DocumentPublicationError::Failed("later".into()));
        assert_eq!(receipt.wait(Some(Instant::now())), Ok(()));
    }

    #[test]
    fn document_publication_disconnection_and_pending_deadline_are_errors() {
        let state = BridgeState::new();
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = DocumentPublicationReceipt {
            receiver,
            state: state.clone(),
        };
        assert_eq!(
            receipt.wait(Some(Instant::now())),
            Err(DocumentPublicationError::DeadlineElapsed)
        );
        drop(reply);
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = DocumentPublicationReceipt { receiver, state };
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
                .wait(Some(Instant::now() + Duration::from_secs(2))),
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
            CollaborationManager::from_sdk_fd_for_test(fd, "dev.qos.chat", Uuid::new_v4());
        assert!(manager.send(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(b"wedged document".to_vec()),
            authorization: None,
        }));
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(manager.send(CollabRequest::NativeMediaCommand {
            request_id: 1,
            command: Request::new(Command::SetMuted { muted: false })
        }));
        assert!(wait_until(
            || captures.lock().unwrap().media_commands.len() == 1,
            Duration::from_secs(1)
        ));
        let start = Instant::now();
        assert!(manager.send(CollabRequest::NativeMediaCommand {
            request_id: 2,
            command: Request::new(Command::Leave)
        }));
        assert!(
            wait_until(
                || captures.lock().unwrap().media_commands.len() == 2,
                Duration::from_secs(1)
            ),
            "Leave waited behind document/unmute RPC"
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        let requests = captures.lock().unwrap().media_commands.clone();
        assert!(matches!(requests[1].command, Command::Leave));
        assert!(requests[1].intent > requests[0].intent);
        let _ = unmute_reply.send(());
        let mut retired = false;
        assert!(
            wait_until(
                || {
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
                },
                Duration::from_secs(1)
            ),
            "superseded unmute must retire its pending request"
        );
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
            CollaborationManager::from_sdk_fd_for_test(fd, "dev.qos.chat", Uuid::new_v4());
        assert!(manager.send(CollabRequest::NativeMediaCommand {
            request_id: 1,
            command: Request::new(Command::Join {
                channel_id: "voice".into()
            })
        }));
        assert!(wait_until(
            || captures.lock().unwrap().media_commands.len() == 1,
            Duration::from_secs(1)
        ));
        assert!(manager.send(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(b"wedged document".to_vec()),
            authorization: None,
        }));
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(manager);
        assert!(
            wait_until(
                || captures
                    .lock()
                    .unwrap()
                    .media_commands
                    .iter()
                    .any(|request| request.intent == u64::MAX
                        && matches!(request.command, Command::Leave)),
                Duration::from_secs(1)
            ),
            "manager drop must use reserved teardown before document completion"
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
            CollaborationManager::from_sdk_fd_for_test(fd, "dev.qos.chat", cluster_id);
        let routes = (0..8)
            .map(|index| RouteSpec {
                app_id: "dev.qos.chat".into(),
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
            .wait(Some(Instant::now() + Duration::from_secs(2)))
            .unwrap();
        for (index, route) in routes.into_iter().enumerate() {
            assert!(manager.try_send_to_route(
                route,
                CollabRequest::NativeMediaCommand {
                    request_id: index as u64 + 1,
                    command: Request::new(Command::SetMuted { muted: false })
                }
            ));
        }
        assert!(wait_until(
            || captures.lock().unwrap().media_commands.len() == 8,
            Duration::from_secs(1)
        ));
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
        assert!(wait_until(
            || captures
                .lock()
                .unwrap()
                .media_commands
                .iter()
                .any(|request| matches!(request.command, Command::Leave)),
            Duration::from_secs(1)
        ));
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
                    let (sender, receiver) = native_media::channel(InboundBridge {
                        tx,
                        state: BridgeState::new(),
                    });
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
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while captures.lock().unwrap().media_commands.is_empty() {
                            tokio::task::yield_now().await;
                        }
                        for _ in 0..10 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .unwrap();
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
                    let (sender, receiver) = native_media::channel(InboundBridge {
                        tx,
                        state: BridgeState::new(),
                    });
                    let routes = native_media::Routes::default();
                    routes.insert("fixture-admitted-route".into(), channel);
                    let worker = native_media::start(receiver, routes);
                    assert!(sender.send(
                        "fixture-admitted-route".into(),
                        1,
                        Request::new(Command::SetMuted { muted: true })
                    ));
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while captures.lock().unwrap().media_commands.is_empty() {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .unwrap();
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
                    let later = tokio::time::timeout(Duration::from_secs(1), async {
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
                    .await
                    .unwrap();
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
        let mut initial = Captures::default();
        initial.committed_history_fixture = Some(history);
        initial.committed_recheck_gate = gate;
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

    fn drain_events(mgr: &mut CollaborationManager, deadline: Duration) -> Vec<CollabEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        while start.elapsed() < deadline {
            events.extend(mgr.poll());
            if !events.is_empty() {
                thread::sleep(Duration::from_millis(5));
                events.extend(mgr.poll());
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        events
    }

    fn wait_until<F: FnMut() -> bool>(mut cond: F, deadline: Duration) -> bool {
        let start = Instant::now();
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

                    authorization: None,
                }),
                ..
            })
                if data == b"durable"
        ));

        assert!(enqueue_sdk_command(&tx, document(), &state));
        assert!(
            !enqueue_sdk_command(&tx, document(), &state),
            "a durable overflow must be observable as a disconnected bridge"
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
        let inbound = InboundBridge {
            tx,
            state: BridgeState::new(),
        };
        assert!(inbound.send_durable(CollabEvent::ActionCatalogue {
            surfaces: Vec::new()
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
        assert!(inbound.send_durable(CollabEvent::ActionCatalogue {
            surfaces: Vec::new()
        }));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "an arrival after poll starts must schedule another wake, even during draining"
        );
        assert!(rx.try_recv().is_ok());
        scope.close();
        inbound.state.begin_poll();
        assert!(inbound.send_durable(CollabEvent::ActionCatalogue {
            surfaces: Vec::new()
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
        let inbound = InboundBridge {
            tx,
            state: state.clone(),
        };

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

        assert!(inbound.send_durable(CollabEvent::ActionCatalogue {
            surfaces: Vec::new(),
        }));
        assert!(
            !inbound.send_durable(CollabEvent::PublishedInvocationResult {
                request_id: 9,
                result: Err("would have been lost".to_owned()),
            }),
            "an invocation lifecycle result is never silently discarded"
        );
        assert!(!state.is_healthy());
        assert!(matches!(
            rx.try_recv(),
            Ok(QueuedCollabEvent {
                event: CollabEvent::ActionCatalogue { surfaces },
                ..
            }) if surfaces.is_empty()
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn oversized_durable_payload_fails_the_byte_budget_before_queueing() {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let mut data = Vec::with_capacity(5 * 1024 * 1024);
        data.push(0);
        let oversized = SdkCommand::Primary(CollabRequest::Document {
            // Capacity, not just logical length, is charged: a caller must
            // not park a mostly-empty giant allocation on the UI bridge.
            mutation: collab_types::DocumentMutation::Snapshot(data),

            authorization: None,
        });

        assert!(!enqueue_sdk_command(&tx, oversized, &state));
        assert!(!state.is_healthy());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn byte_budget_rejects_aggregate_documents_before_count_capacity() {
        let (tx, rx) = tokio_mpsc::channel(SDK_COMMAND_QUEUE);
        let state = BridgeState::new();
        let mut admitted = 0;
        loop {
            let command =
                SdkCommand::Primary(CollabRequest::document_incremental(vec![0; 64 * 1024]));
            if !enqueue_sdk_command(&tx, command, &state) {
                break;
            }
            admitted += 1;
        }

        assert!(admitted < SDK_COMMAND_QUEUE);
        assert!(!state.is_healthy());
        assert_eq!(rx.len(), admitted);
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
        let inbound = InboundBridge {
            tx,
            state: state.clone(),
        };
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
    #[serial_test::serial(tilo_svc_collab_fd)]
    #[cfg(unix)]
    fn a_broker_that_is_not_there_does_not_hold_the_app_up() {
        // A connected socket whose far end is gone — a broker that died
        // between the launcher's connect and the app's first call. `new()`
        // runs before the first frame, so this must return and keep working.
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        drop(theirs);
        let fd = ours.into_raw_fd();
        // SAFETY: serialised on `tilo_svc_collab_fd` against every other
        // test that reads or writes these two variables.
        unsafe {
            std::env::set_var("TILO_SVC_COLLAB_FD", fd.to_string());
        }

        let started = Instant::now();
        let mut mgr = CollaborationManager::new();
        let took = started.elapsed();

        // SAFETY: as above.
        unsafe { std::env::remove_var("TILO_SVC_COLLAB_FD") };

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
    #[serial_test::serial(tilo_svc_collab_fd)]
    fn disconnected_manager_is_a_no_op() {
        // No SDK, no legacy socket set — falls through to `Backend::Disabled`.
        // SAFETY: the crate is single-threaded per-test module here.
        unsafe {
            std::env::remove_var("TILO_SVC_COLLAB_FD");
        }
        let mut mgr = CollaborationManager::new();
        assert!(!mgr.is_connected(), "no service = not connected");
        mgr.send(CollabRequest::document_snapshot(b"nope".to_vec()));
        assert!(mgr.poll().is_empty(), "no events on Disabled backend");
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
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2),
            ),
            "join_channel must be observed"
        );
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].1, presentation);
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
            crate::record::RecordKind::new("document"),
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
        let mut captures = Captures::default();
        captures.committed_history_fixture = Some(original.doc.save());
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
        assert!(wait_until(
            || {
                initial = manager.prepare_native_record(&want.id);
                initial.is_some()
            },
            Duration::from_secs(3)
        ));
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
        manager.switch_native_record(&want.id).unwrap();
        assert_eq!(
            captures.lock().unwrap().joined.len(),
            2,
            "two routes must share the one inherited transport"
        );
        assert!(
            captures.lock().unwrap().signed_ops.is_empty(),
            "no default was signed or sent"
        );
        assert!(wait_until(
            || match &manager.backend {
                Backend::Sdk(channel) => channel.state.document_reads.publishable(
                    &manager
                        .primary_route
                        .accepted
                        .as_ref()
                        .unwrap()
                        .route
                        .channel_id()
                ),
                _ => false,
            },
            Duration::from_secs(3)
        ));
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
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2)
            ),
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
            crate::clock::SystemTime::UNIX_EPOCH,
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
                Duration::from_secs(2),
            ),
            "send() must reach the SDK",
        );
        let seen = captures.lock().unwrap().signed_ops.clone();
        assert_eq!(seen, vec![expected_bytes]);
    }

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
                Duration::from_secs(2)
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
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2)
            ),
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
                Duration::from_secs(2),
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
            crate::clock::SystemTime::UNIX_EPOCH,
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
        let events = drain_events(&mut mgr, Duration::from_secs(2));

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
            crate::clock::SystemTime::UNIX_EPOCH,
            43,
        );
        let event = signed_document_event(
            op,
            "peer-alice".into(),
            &std::sync::Arc::new(std::sync::Mutex::new(None)),
        )
        .unwrap();
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
    fn signed_document_event_decrypts_at_endpoint_and_rejects_replay() {
        let channel = collab_types::ChannelId::new();
        let alice_key = channel_crypto::MemberKey::generate("alice").unwrap();
        let bob_key = channel_crypto::MemberKey::generate("bob").unwrap();
        let bob_package = bob_key.fresh_key_package().unwrap();
        let mut alice = channel_crypto::ChannelGroup::create(channel, alice_key).unwrap();
        let welcome = alice.add_member(bob_package).unwrap();
        let bob =
            channel_crypto::ChannelGroup::process_welcome(channel, &welcome, bob_key).unwrap();
        let plaintext = DocumentMutation::Incremental(b"secret change".to_vec())
            .encode_for(collab_types::DocumentWireVersion::QosV1)
            .unwrap();
        let alice_endpoint = std::sync::Arc::new(std::sync::Mutex::new(Some(alice)));
        let (ciphertext, metadata) =
            prepare_document_payload(&alice_endpoint, plaintext.clone()).unwrap();
        let metadata = metadata.expect("a provisioned endpoint never sends plaintext");
        assert_eq!(metadata.channel, channel);
        let op = SignedOp::new_unsigned_with_encryption(
            ciphertext,
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([9; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            crate::clock::SystemTime::UNIX_EPOCH,
            1,
            metadata,
        );
        let endpoint = std::sync::Arc::new(std::sync::Mutex::new(Some(bob)));
        let event = signed_document_event(op.clone(), "alice".into(), &endpoint).unwrap();
        assert!(matches!(
            event.document_mutation(),
            Some(Ok(DocumentMutation::Incremental(data))) if data == b"secret change"
        ));
        let plaintext_downgrade = SignedOp::new_unsigned(
            plaintext,
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([9; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            crate::clock::SystemTime::UNIX_EPOCH,
            2,
        );
        assert!(
            signed_document_event(plaintext_downgrade, "alice".into(), &endpoint).is_err(),
            "a provisioned MLS route must reject plaintext downgrade"
        );
        assert!(
            signed_document_event(op, "alice".into(), &endpoint).is_err(),
            "the MLS receive ratchet must reject replaying one ciphertext"
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
        let events = drain_events(&mut mgr, Duration::from_secs(2));

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
        let start = Instant::now();
        let mut events = Vec::new();
        while events.len() < 7 && start.elapsed() < Duration::from_secs(2) {
            events.extend(mgr.poll());
            thread::sleep(Duration::from_millis(5));
        }

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
            crate::clock::SystemTime::UNIX_EPOCH,
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
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2)
            ),
            "join_channel must precede scrollback fetch",
        );

        let artifact = collab_types::ArtifactId::new();
        mgr.send(CollabRequest::FetchScrollback {
            artifact,
            before_seq: 10,
            max: 50,
        });
        let events = drain_events(&mut mgr, Duration::from_secs(2));
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

    #[test]
    fn task_step_replies_keep_committed_legacy_and_retry_apart() {
        let update = serde_json::to_string(&tilo_actions::InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId::new(),
            output: tilo_actions::ActionOutput::default(),
        })
        .unwrap();
        let (result, continuation, retry) =
            task_step_event(Ok(runtime_collab::TaskStepReply::Committed {
                update_json: update.clone(),
                continuation: b"{\"succeeded\":{}}".to_vec(),
            }));
        assert!(matches!(
            result,
            Ok((0, tilo_actions::InvocationUpdate::Succeeded { .. }))
        ));
        assert_eq!(continuation.as_deref(), Some(&b"{\"succeeded\":{}}"[..]));
        assert!(!retry);

        // An older broker ran the legacy path: follow its invocation, no
        // continuation, never a re-send.
        let (result, continuation, retry) =
            task_step_event(Ok(runtime_collab::TaskStepReply::Legacy {
                id: 41,
                update_json: update,
            }));
        assert!(matches!(result, Ok((41, _))));
        assert_eq!(continuation, None);
        assert!(!retry);

        let (result, continuation, retry) =
            task_step_event(Ok(runtime_collab::TaskStepReply::Retry));
        assert!(result.is_err() && continuation.is_none() && retry);

        let (result, _, retry) = task_step_event(Err("task action is no longer live".into()));
        assert_eq!(result.unwrap_err(), "task action is no longer live");
        assert!(!retry, "a refusal is terminal, not a pending step");
    }
}

#[cfg(test)]
mod portable_transport_tests {
    use super::*;
    struct ClosedPeer;
    impl PeerTransport for ClosedPeer {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, _: CollabRequest) -> Result<(), String> {
            Err("peer disconnected".into())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            vec![]
        }
    }
    #[test]
    fn peer_enqueue_failure_disconnects_the_real_manager() {
        let mut manager = CollaborationManager::with_peer_transport(ClosedPeer);
        assert!(manager.is_connected());
        manager.send(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(vec![1]),

            authorization: None,
        });
        assert!(!manager.is_connected());
        assert!(manager.poll().is_empty());
    }
}
