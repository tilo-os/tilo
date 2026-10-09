//! Toolkit-side collab client: a synchronous, non-blocking `send` / `poll`
//! pair trading typed [`CollabRequest`] / [`CollabEvent`] frames, stable so
//! every downstream app keeps compiling while the transport underneath
//! changes.
//!
//! The manager speaks to exactly one [`CollabTransport`], driven from the
//! owner thread's frame loop: commands go in through
//! [`CollabTransport::submit`], events come out of
//! [`CollabTransport::drain`]. The transport owns authentication, signing
//! and verification; the manager never holds a transport key. Durable
//! traffic fails the transport closed on overload; only transient presence
//! may be shed; a document change the transport cannot take is refused
//! with a [`CollabSendError`] instead. Without a transport the manager is
//! "disabled" — `poll()`
//! returns empty and `send()` is a no-op.
//!
//! The default transport (feature `broker-transport`) delegates to the
//! `runtime-collab` Cap'n Proto broker on a worker thread it owns.

mod peer;
pub use peer::PeerTransport;
use uuid::Uuid;

mod transport;
pub use transport::{
    CollabTransport, DocumentPublicationError, DocumentPublicationReceipt, TransportCommand,
    TransportError, TransportStatus,
};

/// How an app's error begins when it could not join its collaboration route
/// and stopped. The shell recognises it in a failed launch's output to say
/// why the app did not open.
pub const ROUTE_JOIN_FAILED: &str = "joining collaboration route failed";

/// Whether this host supports the local session runtime transport.
/// This describes transport availability, not an inherited capability or permission.
#[cfg(feature = "local-daemons")]
pub const SESSION_RUNTIME_SUPPORTED: bool = runtime_portal::local_socket::SUPPORTED;
/// No local session runtime transport was included in this host.
#[cfg(not(feature = "local-daemons"))]
pub const SESSION_RUNTIME_SUPPORTED: bool = false;

#[cfg(feature = "broker-transport")]
mod broker;
pub mod channel_live_activity;
#[cfg(feature = "broker-transport")]
pub(crate) mod channel_settings;
pub mod handoff;
mod history;
pub mod owner_view;
pub mod participants;
pub mod record_content;
#[cfg(feature = "broker-transport")]
mod route_keys;
#[cfg(feature = "broker-transport")]
pub use broker::{
    CertifiedEndpointIdentity, HostSessionConnector, display_control, speaker_command,
    wifi_continuity_command,
};
// Committed reads live with the broker worker that retains them. Without the
// broker transport no committed capture can exist, but the type stays part of
// `CollabEvent` and native construction custody still runs (always on the
// non-Required path).
#[cfg(feature = "broker-transport")]
pub use broker::CommittedDocument;
#[cfg(feature = "broker-transport")]
pub(crate) use broker::native_initial;
#[cfg(not(feature = "broker-transport"))]
pub use portable_committed::CommittedDocument;
#[cfg(not(feature = "broker-transport"))]
pub(crate) mod native_initial;

#[cfg(not(feature = "broker-transport"))]
mod portable_committed {
    /// A committed-history capture. Only the broker transport produces one,
    /// so in a portable build the type is uninhabited.
    #[derive(Clone, Debug)]
    pub struct CommittedDocument {
        never: std::convert::Infallible,
    }
    impl CommittedDocument {
        pub(crate) fn creation_workspace(
            &self,
        ) -> Option<&runtime_collab::committed_document::CreationWorkspace> {
            match self.never {}
        }

        pub(crate) fn with_mutation<T>(
            &self,
            _apply: impl FnOnce(&collab_types::DocumentMutation) -> T,
        ) -> Result<T, String> {
            match self.never {}
        }
        pub(crate) fn is_current(&self) -> bool {
            match self.never {}
        }
        pub(crate) fn applied(&self) {
            match self.never {}
        }
    }
}
pub use history::{DocumentHistory, HISTORY_PAGE_SIZE, HISTORY_PREFETCH_MARGIN};

#[cfg(any(test, feature = "test-support"))]
mod loopback;
#[cfg(any(test, feature = "test-support"))]
pub use loopback::{LoopbackPeer, LoopbackTransport};

/// Reserved `invokePublishedAction.peerId` meaning "the application
/// connection making this request". Live broker peer ids are allocated from
/// zero upward; the broker never assigns this sentinel.
pub const CURRENT_CONNECTION_PEER_ID: u64 = u64::MAX;

pub use collab_types::DocumentMutation;
use collab_types::SignedOp;
pub use runtime_collab::file_clipboard;
pub use runtime_collab::{
    CalibrationInfo, CalibrationMemberInfo, CalibrationMemberState, CalibrationMicrophoneInfo,
    CalibrationMicrophoneRun, CalibrationMicrophoneState, CalibrationPhaseInfo,
    MAX_SPEAKER_NAME_CHARS, MicrophoneStatus, SpeakerCommand, SpeakerDeviceInfo,
    SpeakerEndpointInfo, SpeakerGroupInfo, SpeakerMemberInfo, SpeakerMemberSpec,
    SpeakerMemberStatus, SpeakerPlaybackInfo, SpeakerReply, SpeakerRoleSpec, SpeakerSnapshot,
    SpeakerTrimInfo,
};
pub use runtime_collab::{
    NearbyDestinationHandle, NearbyTargetHandle, NearbyTransferCommand, NearbyTransferDestination,
    NearbyTransferDirection, NearbyTransferHandle, NearbyTransferPhase, NearbyTransferReply,
    NearbyTransferSnapshot, NearbyTransferStatus, NearbyTransferTarget, PortableWifiSecurity,
    WifiContinuityCommand, WifiContinuityReply,
};

use crate::presence::PresenceKey;

use std::sync::mpsc;

/// Invoke the shell's launcher-authenticated, sealed Wi-Fi continuity path.
/// Without the broker transport there is no certified collaboration service
/// to carry it, so this reports the service as unavailable.
///
/// # Errors
///
/// Always returns an unavailable message in this build.
#[cfg(not(feature = "broker-transport"))]
pub fn wifi_continuity_command(
    _command: runtime_collab::WifiContinuityCommand,
) -> Result<runtime_collab::WifiContinuityReply, String> {
    Err("Wi-Fi continuity requires the certified collaboration service".to_string())
}

/// Ask the shell's speaker-group authority. Without the broker transport
/// there is no certified collaboration service to carry it.
///
/// # Errors
///
/// Always returns an unavailable message in this build.
#[cfg(not(feature = "broker-transport"))]
pub fn speaker_command(
    _command: runtime_collab::SpeakerCommand,
) -> Result<runtime_collab::SpeakerReply, String> {
    Err("speakers need the certified collaboration service".to_string())
}

/// The native display control service is unavailable without an admitted broker.
///
/// # Errors
/// Always reports unavailable on this transport.
#[cfg(not(feature = "broker-transport"))]
pub fn display_control(_command: Vec<u8>, _origin: DisplayOrigin) -> Result<Vec<u8>, String> {
    Err("Displays need the certified collaboration service.".into())
}

pub use crate::host::ContinuationMetadata;

/// The launcher-only resume payload the host passed this window
/// ([`crate::host::HostIdentity::continuity_resume`]).
pub fn continuation_restore_env() -> Option<ContinuationMetadata> {
    crate::host::environment()
        .identity
        .continuity_resume
        .clone()
}

/// Why a request did not reach the collaboration transport.
///
/// A document change that returns an error was **not** handed off: callers
/// that track what peers have received must leave their frontier unchanged
/// so the change is offered again.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CollabSendError {
    /// No live transport; collaboration is disabled or has failed.
    #[error("collaboration is unavailable")]
    Unavailable,
    /// Document traffic already queued fills the transport budget. Retry
    /// once earlier changes have been sent.
    #[error("collaboration transport is busy; the change will be retried")]
    Busy,
    /// The document change can never be sent: it exceeds the collaboration
    /// transport's per-change limit.
    #[error(
        "this change is {actual} bytes, above the {max}-byte collaboration limit; it stays on this device"
    )]
    TooLarge { actual: usize, max: usize },
}

impl From<TransportError> for CollabSendError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Shed | TransportError::Busy => Self::Busy,
            TransportError::Closed | TransportError::Unsupported => Self::Unavailable,
            TransportError::TooLarge { actual, max } => Self::TooLarge { actual, max },
        }
    }
}

#[derive(Debug, Clone)]
pub enum CollabRequest {
    LocalFileClipboard {
        request_id: u64,
        command: runtime_collab::file_clipboard::Command,
        on_behalf_of: Option<tilo_actions::InvocationId>,
    },
    Hello {
        app_id: String,
        cluster_id: Uuid,
        /// Which **record** ([`crate::record`]) this connection carries, when
        /// adopted. The broker delivers payloads only to connections
        /// announcing the same record, so two people editing different
        /// files in one room don't merge each other's state; `None` means
        /// one document per `(app, room)`.
        record: Option<String>,
        /// The resource kind of `record`, named so a host that creates a
        /// missing record on the app's first write knows what it holds.
        record_kind: Option<String>,
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
    /// Content this route's record references, which the app stored: the
    /// host keeps it where the record's readers are served from.
    OfferContent {
        hash: String,
        bytes: Vec<u8>,
    },
    /// Content this route's record references and the app lacks. Answered,
    /// from this device or the route's other devices, with
    /// [`CollabEvent::RecordContent`].
    WantContent {
        hash: String,
    },
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
    /// Answer a handoff request, or report a retirement
    /// ([`handoff::HandoffReport`], encoded). Session-local, on the primary
    /// route; never relayed to peers.
    ReportHandoff {
        app_id: String,
        message: Vec<u8>,
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
    /// One remote-action request to the session's action host: list the
    /// actions offered to this connection, invoke one, observe or cancel an
    /// invocation. The answer arrives as [`CollabEvent::RemoteAction`].
    RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage,
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
    /// A Store launch, with a dedicated reply vocabulary so a widget's
    /// correlation id cannot complete an unrelated SDK action invocation.
    LaunchInstalledApplication {
        request_id: u64,
        app_id: String,
        package_root: Option<String>,
        on_behalf_of: Option<tilo_actions::InvocationId>,
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
        /// The caller's id for this invocation. A retry reuses it; the
        /// broker reports the original, and [`Self::RemoteAction`] observes
        /// or cancels it.
        invocation: tilo_actions::InvocationId,
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

/// One inert operation this application exports, and what invoking it
/// carries: the one exported-action shape, shared with the broker.
pub use runtime_collab::{ExportedAction, ExportedInput};

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
    #[serde(default)]
    pub semantic_identity: Option<tilo_actions::ResourceSemanticIdentity>,
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
#[cfg_attr(
    not(feature = "broker-transport"),
    allow(
        dead_code,
        reason = "only the broker transport decodes the catalogue wire"
    )
)]
pub(crate) struct BrokerActionCatalogueWire {
    pub(crate) surfaces: Vec<BrokerActionSurface>,
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
    /// The single channel id this route is joined under on the broker.
    pub(crate) fn channel_id(&self) -> String {
        channel_key::format(
            &self.app_id,
            &self.cluster_id.to_string(),
            self.record.as_deref().unwrap_or(""),
        )
    }

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
}

/// What an endpoint's route can do with content, as apps see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelAccessState {
    /// No channel encryption applies to this endpoint (a test harness).
    Unencrypted,
    /// End-to-end encrypted; content flows.
    Encrypted,
    /// This device asked for access and has not been admitted yet, so no
    /// content can be read or written. Edits wait on the device.
    WaitingForAccess,
    /// This device was removed from the channel.
    Removed,
    /// This device cannot join the channel right now (no secret store, or
    /// the room's governor is unknown). Edits wait on the device.
    Unavailable,
}

impl ChannelAccessState {
    /// Whether content currently flows.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Unencrypted | Self::Encrypted)
    }
}

#[derive(Debug, Clone)]
pub enum CollabEvent {
    LocalFileClipboardResult {
        request_id: u64,
        result: Result<runtime_collab::file_clipboard::Reply, String>,
    },
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
    /// The shell asked this instance for a step of moving its window to
    /// another workspace. Session-local control; never from a peer.
    Handoff {
        request: handoff::HandoffRequest,
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
    /// What this endpoint can do with one route's end-to-end encrypted
    /// content changed: it was admitted, is waiting for access (a new device
    /// before its welcome arrives), was removed, or cannot join. While a
    /// route is not open the SDK holds edits and incoming ciphertext on the
    /// device instead of dropping them; apps show the state.
    ChannelAccess {
        route: RouteSpec,
        state: ChannelAccessState,
    },
    /// A validated signed document envelope from another peer. For cleartext
    /// routes the mutation kind is inside the outer signed payload. For an
    /// MLS route, `envelope.payload` remains the original signed ciphertext
    /// and `decrypted_payload` holds endpoint-only plaintext after the inner
    /// MLS authentication succeeds. A mutation too large for one frame
    /// arrives as several signed segments; the SDK delivers it once, after
    /// every segment arrived and the reassembled bytes matched their signed
    /// digest, with `envelope` set to the final segment.
    Document {
        envelope: Box<SignedOp>,
        from_user_id: String,
        /// Endpoint-verified typed mutation bytes when they differ from
        /// `envelope.payload`: MLS plaintext, or a mutation reassembled from
        /// segments. `None` for single-frame cleartext envelopes (read
        /// directly from `envelope.payload`) and for broker events, which
        /// never possess MLS keys.
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
        /// Oldest `seq` the host returned, before the endpoint dropped ops
        /// it could not use yet (an incomplete segment), so paging resumes
        /// below it rather than re-fetching or stopping early.
        oldest_seq: Option<u64>,
        exhausted: bool,
    },
    /// One capability-checked invocation addressed to this application
    /// connection by the session action broker.
    Invocation {
        id: u64,
        invocation: tilo_actions::InvocationId,
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
    /// The session's answer to a [`CollabRequest::RemoteAction`].
    RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage,
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
    InstalledApplicationLaunchResult {
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
        /// terminal update). `None` for a non-step invocation reply.
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
    /// Content the record on `route` references arrived: an answer to
    /// [`CollabRequest::WantContent`], or content another app on the record
    /// offered. The bytes are verified against `hash` before use.
    RecordContent {
        route: RouteSpec,
        hash: String,
        bytes: Vec<u8>,
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
        Some(DocumentMutation::decode(
            decrypted_payload.as_deref().unwrap_or(&envelope.payload),
        ))
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
    /// The colour this participant wears, as they chose it or as their
    /// stable identity derives it. `None` when the room has not heard it;
    /// they are then painted in the colour their id derives
    /// ([`crate::presence::user_color`]).
    pub colour: Option<crate::tokens::PresenceHue>,
    /// Whether this participant is an agent rather than a person.
    pub is_agent: bool,
    /// Whether this row is the receiving machine's own participant.
    /// Apps can't derive this themselves: `id` is the address *other*
    /// machines use, and a machine never announces its own to its own apps.
    pub is_self: bool,
}

impl Collaborator {
    /// [`Self::colour`] as the roster's wire carries it: the presence
    /// colour's sRGB packed `0x00RRGGBB`, `0` for none (no presence colour
    /// is black).
    #[must_use]
    pub fn packed_colour(&self) -> u32 {
        self.colour.map_or(0, |hue| {
            let (r, g, b) = crate::presence::identity_rgb(hue);
            (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        })
    }

    /// The colour a roster row's packed `0x00RRGGBB` names: the presence
    /// hue nearest it, `None` for `0` or a grey.
    #[must_use]
    pub fn colour_from_packed(packed: u32) -> Option<crate::tokens::PresenceHue> {
        if packed == 0 {
            return None;
        }
        let [_, r, g, b] = packed.to_be_bytes();
        crate::tokens::PresenceHue::nearest(vello::peniko::Color::from_rgb8(r, g, b))
    }
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
        let timestamp = crate::time::wall();
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

/// Where a manager's connection stands, as the UI shows it.
///
/// Construction never waits for the network: a manager that has a service
/// to reach starts in [`Self::Connecting`] and moves to [`Self::Ready`] or
/// [`Self::Offline`] on a later [`CollaborationManager::poll`]. The owner is
/// woken when that happens, so a frame loop observes the change without
/// polling on a timer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollaborationState {
    /// Reaching the collaboration service and announcing the primary route.
    /// Requests sent meanwhile are queued, in order, for when it is ready.
    Connecting,
    /// Connected, with the primary route acknowledged.
    Ready,
    /// No collaboration: the service is absent, refused the route, or the
    /// connection closed. Carries a human-readable reason.
    Offline(String),
}

impl CollaborationState {
    /// Whether requests currently reach peers.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Reason reported by a manager that never had a service to reach.
const NO_SERVICE: &str = "no collaboration service is available";

/// Which transport the manager is speaking.
enum Backend {
    /// No collab service reachable. `poll` yields nothing, `send` is a
    /// black hole; every widget already handles this via
    /// [`CollaborationManager::is_connected`]. Carries the reason the
    /// manager is offline.
    Disabled(String),
    /// One transport, driven from the owner thread by `send` / `poll`.
    Transport(Box<dyn CollabTransport>),
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
    /// Set while the connection is still being established in the
    /// background: the manager reports [`CollaborationState::Connecting`]
    /// until the transport acknowledges the primary route.
    awaiting_primary: bool,
    /// The state last handed out by [`Self::take_state_change`].
    reported_state: Option<CollaborationState>,
    /// When `Some`, [`Self::send_signed_op`] mints a signed envelope and
    /// dispatches [`CollabRequest::SignedOperation`]. This explicit
    /// policy-envelope API is separate from document mutations, which the SDK
    /// signs with the certified launcher identity.
    identity: Option<SigningIdentity>,
    /// Whether the action directory was registered on the current primary
    /// route. A new `Hello` resets it.
    action_directory_published: bool,
}

impl CollaborationManager {
    /// Speak to an explicitly supplied transport.
    ///
    /// The manager announces nothing on its own: send a
    /// [`CollabRequest::Hello`] to name the primary route. The route becomes
    /// `Self::primary_route` only once the transport reports the remote
    /// end's acknowledgement through
    /// [`CollabTransport::take_primary_route_result`], observed by
    /// [`Self::poll`]. The transport is handed a wake callback that schedules
    /// this owner's next frame.
    pub fn with_transport(mut transport: Box<dyn CollabTransport>) -> Self {
        let wake = crate::wake::WakeHandle::new();
        transport.set_wake(Box::new(move || wake.notify_events()));
        let mut manager = Self {
            backend: Backend::Transport(transport),
            primary_route: PrimaryRouteState::default(),
            awaiting_primary: false,
            reported_state: None,
            identity: None,
            action_directory_published: false,
        };
        manager.reported_state = Some(manager.state());
        manager
    }

    /// Connects to the launcher-inherited collaboration service when this
    /// build has one, entering Disabled mode when it is unavailable.
    ///
    /// Returns at once: the connection is made in the background and its
    /// progress is reported by [`Self::state`]. Nothing here waits on the
    /// service, so this is safe to call on a UI thread before the first
    /// frame.
    ///
    /// Both modes auto-send `Hello` announcing **no record** (one document
    /// per `(app, room)`); use [`Self::for_record`] for apps that have
    /// adopted records.
    pub fn new() -> Self {
        Self::new_scoped(
            continuation_instance_env().map(|instance| format!("continuation:{instance}")),
            None,
        )
    }

    /// Without the broker transport there is no native document route to
    /// switch; callers keep their current record.
    #[cfg(not(feature = "broker-transport"))]
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "keeps the broker-transport signature, which switches the live route"
    )]
    pub(crate) fn switch_native_record(
        &mut self,
        _checkout: &crate::record::RecordCheckout,
    ) -> Result<(), String> {
        Err("native document routes need the broker transport".into())
    }

    pub(crate) fn has_native_committed_primary(&self) -> bool {
        match (&self.backend, &self.primary_route.accepted) {
            (Backend::Transport(transport), Some(route)) => {
                transport.committed_document_required(&route.route.channel_id())
            }
            _ => false,
        }
    }

    /// Join through the existing worker; never take the inherited RPC fd twice.
    pub(crate) fn prepare_native_record(
        &mut self,
        checkout: &crate::record::RecordCheckout,
    ) -> Option<CommittedDocument> {
        let primary = self.primary_route.accepted.as_ref()?;
        let route = RouteSpec::record(
            primary.route.app_id.clone(),
            primary.route.cluster_id,
            checkout.id.as_str(),
        );
        let Backend::Transport(transport) = &mut self.backend else {
            return None;
        };
        transport.prepare_committed_document(route, checkout.kind.as_str())
    }
}

/// Compatibility route marker for a launcher-issued app window. Native
/// document routes obtain their separate window binding from original broker
/// custody; this environment value cannot authorize native private control.
fn continuation_instance_env() -> Option<String> {
    let environment = crate::host::environment();
    let value = environment.identity.continuity_instance.as_deref()?.trim();
    (!value.is_empty() && value.len() <= 256 && !value.contains(['/', '\\']))
        .then(|| value.to_owned())
}

impl Default for CollaborationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CollaborationManager {
    /// A manager scoped to **one record** — one document, one change graph,
    /// one channel — so two people editing different files in one room
    /// don't get handed each other's state. A process with several records
    /// open holds several managers; `tilo_ui::App` swaps its manager on checkout
    /// change.
    pub fn for_record(checkout: &crate::record::RecordCheckout) -> Self {
        Self::new_scoped(
            Some(checkout.id.as_str().to_string()),
            Some(checkout.kind.as_str().to_string()),
        )
    }

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
        Self::with_transport(Box::new(peer::PeerTransportAdapter::new(transport)))
    }

    /// Explicitly disconnected construction: no identity, service or worker discovery.
    pub(crate) fn disabled() -> Self {
        Self::offline(NO_SERVICE.to_owned())
    }

    fn offline(reason: String) -> Self {
        Self {
            reported_state: Some(CollaborationState::Offline(reason.clone())),
            backend: Backend::Disabled(reason),
            primary_route: PrimaryRouteState::default(),
            awaiting_primary: false,
            identity: None,
            action_directory_published: false,
        }
    }

    /// Ask the transport to report the broker's decision about the primary
    /// route announced just before, without waiting for it: the manager
    /// reports [`CollaborationState::Connecting`] until [`Self::poll`]
    /// observes the answer.
    fn confirm_primary_route_in_background(&mut self) {
        self.awaiting_primary = true;
        if self.submit(TransportCommand::ConfirmPrimaryRoute).is_err() {
            self.go_offline("collaboration setup thread is unavailable or overloaded".into());
        }
        self.reported_state = Some(self.state());
    }

    /// Drop the transport and report `reason` as the offline cause.
    fn go_offline(&mut self, reason: String) {
        tracing::warn!(%reason, "collab: continuing without collaboration");
        self.backend = Backend::Disabled(reason);
        self.awaiting_primary = false;
        self.primary_route = PrimaryRouteState::default();
        self.action_directory_published = false;
    }

    /// Where the connection stands. Cheap; call it every frame.
    pub fn state(&self) -> CollaborationState {
        match &self.backend {
            Backend::Disabled(reason) => CollaborationState::Offline(reason.clone()),
            Backend::Transport(transport) => match transport.status() {
                TransportStatus::Closed(reason) => CollaborationState::Offline(reason),
                TransportStatus::Connecting => CollaborationState::Connecting,
                TransportStatus::Ready | TransportStatus::Degraded(_) if self.awaiting_primary => {
                    CollaborationState::Connecting
                }
                TransportStatus::Ready | TransportStatus::Degraded(_) => CollaborationState::Ready,
            },
        }
    }

    /// The current state when it differs from the one this method last
    /// returned (initially, the state the manager was constructed in). Lets
    /// an owner run connect-time work once, when the manager becomes ready.
    pub fn take_state_change(&mut self) -> Option<CollaborationState> {
        let state = self.state();
        if self.reported_state.as_ref() == Some(&state) {
            return None;
        }
        self.reported_state = Some(state.clone());
        Some(state)
    }

    /// The primary route most recently announced by a `Hello`, acknowledged
    /// or not.
    pub(crate) fn requested_primary_route(&self) -> Option<&RouteSpec> {
        match self.backend {
            Backend::Disabled(_) => None,
            Backend::Transport(_) => self.primary_route.requested.as_ref(),
        }
    }

    /// Without a built-in transport there is nothing to discover.
    #[cfg(not(feature = "broker-transport"))]
    fn new_scoped(_record: Option<String>, _record_kind: Option<String>) -> Self {
        Self::disabled()
    }

    /// True when connected to a live broker — used by the
    /// event loop to keep ticking so the stream drains promptly.
    pub(crate) fn primary_route(&self) -> Option<&PrimaryRouteWitness> {
        self.primary_route.current(self.is_connected())
    }

    pub(crate) fn private_window_instance(&self) -> Option<u64> {
        let witness = self.primary_route()?;
        let Backend::Transport(transport) = &self.backend else {
            return None;
        };
        if transport.committed_document_required(&witness.route.channel_id()) {
            return transport.private_window_binding(&witness.route);
        }
        witness
            .route
            .record
            .as_deref()?
            .strip_prefix("continuation:")?
            .parse::<u64>()
            .ok()
    }

    /// Whether requests currently reach peers: [`Self::state`] is
    /// [`CollaborationState::Ready`].
    pub fn is_connected(&self) -> bool {
        match &self.backend {
            Backend::Disabled(_) => false,
            Backend::Transport(transport) => !self.awaiting_primary && transport.status().is_open(),
        }
    }

    /// Submit one command, reporting whether the transport admitted it.
    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
        match &mut self.backend {
            Backend::Disabled(_) => Err(TransportError::Closed),
            Backend::Transport(transport) => transport.submit(command),
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
    /// shed under saturation; any other durable enqueue failure visibly
    /// disconnects the manager, so callers never continue under the illusion
    /// it was sent. Document mutations never disconnect: see
    /// [`Self::try_send`] for why one was not sent.
    ///
    /// While the manager is [`CollaborationState::Connecting`] only a
    /// `Hello` is accepted: there is no acknowledged primary route to carry
    /// anything else, so other requests return `false` exactly as they do
    /// offline. Owners re-publish their state once the manager is ready.
    pub fn send(&mut self, req: CollabRequest) -> bool {
        self.try_send(req).is_ok()
    }

    /// [`Self::send`] with the reason a request was not handed off. A
    /// document mutation larger than one signed frame is split into signed
    /// segments by the transport; one above
    /// [`collab_types::MAX_DOCUMENT_TRANSFER_BYTES`] is refused with
    /// [`CollabSendError::TooLarge`] and nothing is sent.
    ///
    /// # Errors
    ///
    /// Returns [`CollabSendError`] when the request was not queued, and
    /// [`CollabSendError::Unavailable`] for anything but a `Hello` while the
    /// manager is still connecting.
    pub fn try_send(&mut self, req: CollabRequest) -> Result<(), CollabSendError> {
        if self.missing_required_document_custody(&req) {
            return Err(CollabSendError::Unavailable);
        }
        if self.awaiting_primary && !matches!(req, CollabRequest::Hello { .. }) {
            // No acknowledged primary route carries anything but the Hello
            // yet; owners republish once the manager is ready.
            return Err(CollabSendError::Unavailable);
        }
        if let CollabRequest::NativeMediaCommand {
            request_id,
            command,
        } = req
        {
            // Native media rides the transport's side channel on the
            // acknowledged primary route, never the durable command queue.
            let healthy = self.is_connected();
            let Some(channel) = self
                .primary_route
                .current(healthy)
                .map(|primary| primary.route.channel_id())
            else {
                return Err(CollabSendError::Unavailable);
            };
            let Backend::Transport(transport) = &mut self.backend else {
                return Err(CollabSendError::Unavailable);
            };
            return transport
                .send_native_media(channel, request_id, command)
                .then_some(())
                .ok_or(CollabSendError::Unavailable);
        }
        let primary_channel = self
            .primary_route
            .accepted
            .as_ref()
            .map(|witness| witness.route.channel_id());
        let unpublishable = matches!(
            req,
            CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
        ) && match &self.backend {
            Backend::Transport(transport) => {
                !transport.document_publishable(primary_channel.as_deref())
            }
            Backend::Disabled(_) => false,
        };
        if unpublishable {
            // Committed-document custody: nothing is published on a route
            // whose original history has not been applied locally.
            return Err(CollabSendError::Unavailable);
        }
        let CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            ..
        } = &req
        else {
            return Ok(self.submit(TransportCommand::Primary(req))?);
        };
        self.primary_route.announce(RouteSpec {
            app_id: app_id.clone(),
            cluster_id: *cluster_id,
            record: record.clone(),
        });
        self.action_directory_published = false;
        self.submit(TransportCommand::Primary(req))?;
        if self.awaiting_primary {
            // The pending acknowledgement answers the previous Hello; ask
            // again, behind this one.
            self.confirm_primary_route_in_background();
        }
        Ok(())
    }

    /// Ask the transport for a completion receipt covering every document
    /// mutation submitted before this call. The receipt reports that the
    /// transport finished publishing them, not that peers applied them.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentPublicationError::Unavailable`] without a transport,
    /// or the transport's own refusal (including
    /// [`DocumentPublicationError::Unsupported`]).
    pub fn begin_document_publication_barrier(
        &mut self,
    ) -> Result<DocumentPublicationReceipt, DocumentPublicationError> {
        match &mut self.backend {
            Backend::Disabled(_) => Err(DocumentPublicationError::Unavailable),
            Backend::Transport(transport) => transport.publication_barrier(),
        }
    }

    /// Register the UI-owned pull directory on the primary broker route.
    /// Re-registering is harmless: the broker atomically replaces the old
    /// capability for this live connection.
    pub fn publish_action_directory(&mut self, app_id: impl Into<String>) {
        // The directory rides the primary route, so it waits for the route's
        // acknowledgement; the owner publishes again once the manager is ready.
        if !self.is_connected() || self.action_directory_published {
            return;
        }
        if self
            .submit(TransportCommand::PublishActionDirectory {
                app_id: app_id.into(),
            })
            .is_ok()
        {
            self.action_directory_published = true;
        }
    }

    pub(crate) fn action_directory_observation(&self) -> (bool, Option<(u64, u64)>) {
        let counts = match &self.backend {
            Backend::Transport(transport) => transport.action_directory_publication_counts(),
            Backend::Disabled(_) => None,
        };
        (self.action_directory_published, counts)
    }

    /// Drain requests issued by the transport's `ActionDirectory` server.
    /// This must be called by [`crate::app::App`] on its owner thread.
    pub(crate) fn take_action_directory_requests(&mut self) -> Vec<ActionDirectoryRequest> {
        let mut requests = Vec::new();
        if let Backend::Transport(transport) = &mut self.backend {
            transport.take_action_directory_requests(&mut requests);
        }
        requests
    }

    /// Join and subscribe to an additional route without publishing traffic.
    /// Repeated joins of the same route are idempotent.
    pub fn join_route(&mut self, route: RouteSpec) {
        let _ = self.submit(TransportCommand::Join(route));
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
        if self.awaiting_primary {
            // The worker answers only after connecting; waiting here would
            // stall the owner thread for as long as the connection takes.
            return Err("collaboration is still connecting".into());
        }
        let Backend::Transport(transport) = &mut self.backend else {
            return Err("collaboration broker is unavailable".into());
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if transport
            .submit(TransportCommand::InstallDocumentEncryption {
                route,
                group: Box::new(group),
                reply: reply_tx,
            })
            .is_err()
        {
            return Err("collaboration worker is unavailable or overloaded".to_string());
        }
        reply_rx
            .recv()
            .map_err(|_| "collaboration worker stopped".to_string())?
    }

    /// Publish on an explicitly named additional route. The route is joined
    /// lazily and retained, and its inbound streams feed this manager's normal
    /// [`Self::poll`] queue.
    ///
    /// Like [`Self::send`], this sends nothing while the manager is still
    /// connecting.
    pub fn send_to_route(&mut self, route: RouteSpec, req: CollabRequest) {
        let _ = self.try_send_to_route(route, req);
    }

    /// Whether the actual route's original document history has been applied.
    /// This is the same transport gate used before document publication.
    pub(crate) fn document_route_ready(&self, route: Option<&RouteSpec>) -> bool {
        let channel = route.map(RouteSpec::channel_id);
        !self.awaiting_primary
            && match &self.backend {
                Backend::Transport(transport) => transport.document_publishable(channel.as_deref()),
                Backend::Disabled(_) => false,
            }
    }

    /// Independently sample the existing transport read gates without admission.
    #[cfg(all(feature = "native", any(test, feature = "test-support")))]
    pub(crate) fn controlled_document_read_phase_for_test(
        &self,
        route: &RouteSpec,
    ) -> Option<serde_json::Value> {
        if let Backend::Transport(transport) = &self.backend {
            transport.controlled_document_read_phase_for_test(&route.channel_id())
        } else {
            None
        }
    }

    /// Test-only: whether `route` has resolved its document mode, so a
    /// document mutation on it would be admitted rather than refused.
    #[cfg(test)]
    pub(crate) fn document_route_publishable_for_test(&self, route: &RouteSpec) -> bool {
        match &self.backend {
            Backend::Transport(transport) => {
                transport.document_publishable(Some(&route.channel_id()))
            }
            Backend::Disabled(_) => false,
        }
    }

    /// [`Self::send_to_route`] with the reason a request was not handed off.
    /// This is the exact local enqueue outcome: the transport's later
    /// publication barrier still owns durable acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns [`CollabSendError`] when the request was not queued.
    pub fn try_send_to_route(
        &mut self,
        route: RouteSpec,
        req: CollabRequest,
    ) -> Result<(), CollabSendError> {
        if self.missing_required_document_custody(&req) {
            return Err(CollabSendError::Unavailable);
        }
        if matches!(req, CollabRequest::Hello { .. }) {
            tracing::warn!("collab: send_to_route ignores Hello; use join_route");
            return Err(CollabSendError::Unavailable);
        }
        if self.awaiting_primary {
            return Err(CollabSendError::Unavailable);
        }
        if let CollabRequest::NativeMediaCommand {
            request_id,
            command,
        } = req
        {
            let Backend::Transport(transport) = &mut self.backend else {
                return Err(CollabSendError::Unavailable);
            };
            return transport
                .send_native_media(route.channel_id(), request_id, command)
                .then_some(())
                .ok_or(CollabSendError::Unavailable);
        }
        let unpublishable = matches!(
            req,
            CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
        ) && match &self.backend {
            Backend::Transport(transport) => {
                !transport.document_publishable(Some(&route.channel_id()))
            }
            Backend::Disabled(_) => false,
        };
        let known_pending_document = matches!(&req, CollabRequest::Document { .. })
            && matches!(&self.backend, Backend::Transport(transport)
                if transport.committed_document_required(&route.channel_id()));
        if unpublishable && !known_pending_document {
            // Join first; the route's original history must be applied
            // before any local mutation is published on it.
            let _ = self.submit(TransportCommand::Join(route));
            return Err(CollabSendError::Unavailable);
        }
        // A known committed route may be refreshing. Required original
        // custody was checked above; retain this mutation in the bounded queue.
        // The worker still waits for this exact route before checking retained
        // custody and applying the route's existing certification/commit policy.
        Ok(self.submit(TransportCommand::ToRoute(route, req))?)
    }

    pub(crate) fn document_publication_failed(&self) -> bool {
        matches!(&self.backend, Backend::Transport(transport) if transport.document_publication_failed())
    }

    #[cfg(test)]
    pub(crate) fn fail_publication_for_test(&self) {
        if let Backend::Transport(transport) = &self.backend {
            transport.fail_closed("test terminal publication invalidation");
        }
    }

    /// Whether the broker refused or failed an action-surface publication
    /// since the last call; the caller must republish its surface.
    pub(crate) fn take_action_surface_failed(&mut self) -> bool {
        match &mut self.backend {
            Backend::Transport(transport) => {
                let directory_failed = transport.take_action_directory_failed();
                if directory_failed {
                    self.action_directory_published = false;
                }
                transport.take_action_surface_failed() || directory_failed
            }
            Backend::Disabled(_) => false,
        }
    }

    pub(crate) fn stop_after_publication_failure(&mut self) {
        if let Backend::Transport(transport) = &self.backend {
            transport.fail_closed("pre-terminal document publication; outcome unknown");
        }
        // Drop the sender as well as withdrawing health, so an idle shovel
        // wakes and closes its capability instead of awaiting another command.
        self.backend =
            Backend::Disabled("pre-terminal document publication; outcome unknown".into());
        self.awaiting_primary = false;
        self.primary_route.accepted = None;
    }

    pub(crate) fn set_document_owner_routes(&mut self, routes: Vec<RouteSpec>) {
        if let Backend::Transport(transport) = &mut self.backend {
            transport.set_document_owner_routes(routes);
        }
    }

    pub(crate) fn finish_event_batch(&mut self) {
        if let Backend::Transport(transport) = &mut self.backend {
            transport.finish_event_batch();
        }
    }

    /// Drain every event pending on the current backend. Non-blocking.
    ///
    /// Also records a primary-route acknowledgement the transport reported
    /// since the last poll.
    ///
    /// While the manager is [`CollaborationState::Connecting`], this is also
    /// where it settles: a primary-route acknowledgement makes it ready; a
    /// refusal or a closed connection takes it offline.
    pub fn poll(&mut self) -> Vec<CollabEvent> {
        let mut events = Vec::new();
        let Backend::Transport(transport) = &mut self.backend else {
            return events;
        };
        transport.drain(&mut events);
        let acknowledged = transport.take_primary_route_result().map(|result| {
            let outcome = self.primary_route.accept(result);
            if let Err(error) = &outcome {
                tracing::warn!(%error, "collab: primary route was not acknowledged");
            }
            outcome
        });
        if self.awaiting_primary {
            match (acknowledged, transport.status()) {
                (Some(Ok(())), _) => self.awaiting_primary = false,
                (Some(Err(error)), _) => self.go_offline(error),
                (None, TransportStatus::Closed(reason)) => self.go_offline(reason),
                (None, _) => {}
            }
        }
        events
    }
}

/// The presence handle announced on `Hello`. `TILO_PRESENCE_HANDLE` wins,
/// then `TILO_USER_ID`; absent both, the broker's synthetic name.
pub fn presence_handle_env() -> Option<String> {
    let environment = crate::host::environment();
    let identity = &environment.identity;
    [&identity.presence_handle, &identity.user_id]
        .into_iter()
        .find_map(|handle| handle.clone().filter(|v| !v.trim().is_empty()))
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
        fn channel_keys_round_trip_every_field_combination_and_legacy_form() {
            // Case `round_trips_every_combination_of_present_fields`.
            {
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

            // Case `keys_written_before_the_app_segment_still_parse`.
            {
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

            // Case `a_record_id_containing_a_hash_survives`.
            {
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
}

/// The `(app_id, cluster_id)` pair this process announces in
/// [`CollabRequest::Hello`] — the broker delivers a CRDT delta only to
/// connections announcing the same pair.
///
/// # Precedence (each half resolves independently)
///
/// 1. `TILO_PACKAGE_ID` / `TILO_WORKSPACE_ID` — launcher-set for every
///    compositor-started app.
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
/// Not memoised — see `auto_hello_env_uncached`: a process hosting
/// more than one identity must not be frozen into the first one.
pub fn local_user_id() -> crate::permissions::UserId {
    crate::permissions::UserId(resolve_local_user_id())
}

fn resolve_local_user_id() -> String {
    let environment = crate::host::environment();
    if let Some(v) = environment.identity.user_id.clone() {
        return v;
    }
    if let Some(id) = crate::local_identity::load() {
        return person_id_from_key(id.group_public_key);
    }
    if let Ok(bytes) = std::fs::read("/var/lib/tilo/user-identity/pubkey.bin")
        && let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return person_id_from_key(key);
    }
    environment
        .identity
        .account_name
        .clone()
        .unwrap_or_else(|| "local".to_string())
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
        hex::encode(key)
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

    let environment = crate::host::environment();
    let identity = &environment.identity;
    let app_id = identity
        .package_id
        .clone()
        .or_else(|| identity.development_app_id.clone())
        .unwrap_or_else(|| {
            FALLBACK_APP_ID
                .get_or_init(|| Uuid::new_v4().to_string())
                .clone()
        });

    let cluster_id = identity
        .workspace_id
        .clone()
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
        .or_else(|| {
            identity
                .cluster_id
                .as_deref()
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .unwrap_or_else(Uuid::nil);

    (app_id, cluster_id)
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

/// A host-owned document commit resolving to the broker-signed envelope.
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

/// Original authority retained across display background work.
pub use runtime_collab::DisplayOrigin;
