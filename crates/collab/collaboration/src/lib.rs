//! The collab client without the toolkit.
//!
//! A process that commits to a Tilo room links this crate, not the UI
//! toolkit: an app host running tilolang apps as wasm instances, a browser
//! worker, a host in thin mode. The toolkit builds on it and keeps its paths
//! (`ui::collaboration`, `ui::record`, `ui::app_surface`,
//! `ui::local_identity`, `ui::local_document_action`, `ui::person_chat`,
//! `ui::portable_storage`), so native apps
//! compile unchanged.
//!
//! - [`record`]: a record's identity, as it travels on the collaboration
//!   wire and names a document on disk.
//! - [`app_surface`]: the app surface every signed op names, and the claims
//!   decided against it.
//! - [`local_identity`]: who sits at this device, the process's one identity
//!   seam.
//! - [`local_document_action`]: a local action observation as it is
//!   reported to the room.
//! - The collaboration manager below: the broker client.
//!
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
//! `runtime-collab` collaboration client on a worker thread it owns.

// The broker client brought its lint debt from ui-core, scoped as it was there.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::empty_line_after_doc_comments,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::items_after_test_module,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_assert_eq,
    clippy::manual_find,
    clippy::manual_midpoint,
    clippy::many_single_char_names,
    clippy::map_unwrap_or,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_errors_doc,
    clippy::needless_collect,
    clippy::needless_pass_by_value,
    clippy::needless_range_loop,
    clippy::needless_raw_string_hashes,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_clone,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::ref_as_ptr,
    clippy::result_large_err,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unchecked_time_subtraction,
    clippy::unicode_not_nfc,
    clippy::uninlined_format_args,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_wraps,
    clippy::unreadable_literal,
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::useless_conversion,
    reason = "migration stack lint debt is scoped to this target"
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "unit tests may drive real threads and wait on the real clock; in a test these are the assertion, not a hazard"
    )
)]

pub mod action_directory;
pub mod app_surface;
pub mod document_routes;
pub mod history_source;
pub mod invocation_scope;
pub mod local_document_action;
pub mod local_identity;
pub mod person_chat;
pub mod portable_storage;
pub mod record;
pub mod state;

// The clock, background work, wake routing and host inputs, under the paths
// the toolkit names them by (`ui::time`, `ui::background`, `ui::wake`,
// `ui::host`), without the toolkit's host services.
pub(crate) mod time {
    // The broker's tests stamp records with it.
    #[cfg(all(test, feature = "broker-transport"))]
    pub use tilo_clock::SystemTime;
    // The broker stamps the documents it publishes with it.
    #[cfg(feature = "broker-transport")]
    pub use tilo_clock::wall;
    pub use tilo_clock::{Duration, Instant, now};
}
#[cfg(feature = "broker-transport")]
pub(crate) use tilo_rt::background;
pub(crate) use tilo_rt::wake;
pub(crate) use wake::WakeHandle;
#[cfg(test)]
pub(crate) use wake::WakeScope;
// The tests bound what the sender's storage allocates.

pub(crate) mod host {
    pub use tilo_process::environment::{ContinuationMetadata, environment};
}

mod annotation;
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
pub mod approved_checks;
#[cfg(feature = "broker-transport")]
mod broker;
pub mod browser_service_sessions;
#[cfg(feature = "broker-transport")]
pub mod external_moderation;
#[cfg(feature = "broker-transport")]
pub mod write_attribution_lane;
#[cfg(feature = "broker-transport")]
pub use broker::{person_chat_command, subscribe_person_chat};
pub mod channel_live_activity;
#[cfg(feature = "broker-transport")]
pub mod channel_settings;
pub mod handoff;
mod history;

mod surface_publication;
pub use runtime_collab::{SurfaceDelta, SurfaceTable};
pub use surface_publication::SurfacePublication;
pub mod record_content;
#[cfg(feature = "broker-transport")]
mod route_keys;
#[cfg(all(feature = "broker-transport", unix))]
pub use broker::SessionServices;
#[cfg(feature = "broker-transport")]
pub use broker::{
    CertifiedEndpointIdentity, EndpointChannelKeys, HostSessionConnector, SessionResource,
    display_control, speaker_command, wifi_continuity_command,
};
// Committed reads live with the broker worker that retains them. Without the
// broker transport no committed capture can exist, but the type stays part of
// `CollabEvent` and native construction custody still runs (always on the
// non-Required path).
#[cfg(feature = "broker-transport")]
pub use broker::CommittedDocument;
#[cfg(all(
    feature = "broker-transport",
    any(test, feature = "test-support"),
    unix
))]
pub use broker::JoiningProbe;
#[cfg(all(feature = "broker-transport", feature = "native"))]
pub use broker::NativeJoin;
#[cfg(feature = "broker-transport")]
pub use broker::native_initial;
#[cfg(all(feature = "broker-transport", feature = "test-support", unix))]
pub use broker::test_support;
#[cfg(not(feature = "broker-transport"))]
pub use portable_committed::CommittedDocument;
#[cfg(not(feature = "broker-transport"))]
pub mod native_initial;

#[cfg(not(feature = "broker-transport"))]
mod portable_committed {
    /// A committed-history capture. Only the broker transport produces one,
    /// so in a portable build the type is uninhabited.
    #[derive(Clone, Debug)]
    pub struct CommittedDocument {
        never: std::convert::Infallible,
    }
    impl CommittedDocument {
        pub fn creation_workspace(
            &self,
        ) -> Option<&runtime_collab::committed_document::CreationWorkspace> {
            match self.never {}
        }

        pub fn with_mutation<T>(
            &self,
            _apply: impl FnOnce(&collab_types::DocumentMutation) -> T,
        ) -> Result<T, String> {
            match self.never {}
        }
        pub fn is_current(&self) -> bool {
            match self.never {}
        }
        pub fn applied(&self) {
            match self.never {}
        }
        pub fn applies_to(&self, _document: &impl automerge::ReadDoc) -> bool {
            match self.never {}
        }
        pub fn refuse(&self) {
            match self.never {}
        }
    }
}
pub use history::{DocumentHistory, HISTORY_PAGE_SIZE, HISTORY_PREFETCH_MARGIN};

/// Reserved `invokePublishedAction.peerId` meaning "the application
/// connection making this request". Live broker peer ids are allocated from
/// zero upward; the broker never assigns this sentinel.
pub const CURRENT_CONNECTION_PEER_ID: u64 = u64::MAX;

pub use collab_types::DocumentMutation;
use collab_types::SignedOp;
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
    NearbyTransferSnapshot, NearbyTransferStatus, NearbyTransferTarget, NetworkWakeStatus,
    PortableWifiSecurity, WifiContinuityCommand, WifiContinuityReply,
};
pub use runtime_collab::{file_clipboard, open_selected, write_attribution};

use collab_types::presence::PresenceKey;
pub use collab_types::presence::{Collaborator, PresencePayload};

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

/// The session's published resources need an admitted broker to reach; in
/// this build the same handle opens nothing and says so.
#[cfg(not(feature = "broker-transport"))]
#[derive(Clone)]
pub struct SessionResource;

#[cfg(not(feature = "broker-transport"))]
impl SessionResource {
    /// # Errors
    ///
    /// Always reports unavailable on this transport.
    pub fn open(_resource: &str) -> Result<Self, String> {
        Err("Published resources need the certified collaboration service.".into())
    }

    /// Nothing is watched on this transport.
    #[must_use]
    pub fn latest(&self) -> Option<Result<String, String>> {
        None
    }

    /// Nothing is watched on this transport, so no reading wakes a view.
    pub fn attach(&self, _wake: tilo_rt::wake::WakeHandle) {}

    /// # Errors
    ///
    /// Always reports unavailable on this transport.
    pub fn invoke(
        &self,
        _action: &str,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        Err("Published resources need the certified collaboration service.".into())
    }
}

pub use crate::host::ContinuationMetadata;
/// The numbers a semantic checkpoint carries for the portable schemas a
/// signed manifest can declare (`continuity.semantic_schema`). The tag says
/// what the payload means, so a device that has not installed the app can
/// still check its shape. They match `ContinuitySemanticSchema::wire_tag`
/// in `tilo-app-package`.
pub mod semantic_schema {
    pub const WEB_LOCATION_V1: u16 = 0x0101;
    pub const PUBLICATION_POSITION_V1: u16 = 0x0102;
    pub const MEDIA_POSITION_V1: u16 = 0x0103;
    pub const REALTIME_SESSION_V1: u16 = 0x0104;
    pub const TEXT_POSITION_V1: u16 = 0x0105;
}

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

/// Why a native launch's manager could not join its room.
#[cfg(feature = "native")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeLaunchError {
    /// The workspace the app was launched into has no identity selected, so
    /// it cannot join it; the launch waits for someone to choose one.
    IdentityRequired,
    /// The configured service could not be reached.
    Operation(String),
}

/// What holds up a primary route's join, as the broker reports it while the
/// join is pending ([`CollaborationManager::join_hold`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinHold {
    /// The broker holds the join until the room is ready on this device:
    /// why, in the person's words, and what last held it up when something
    /// did.
    RoomWaiting {
        reason: String,
        detail: Option<String>,
    },
    /// The broker could not be asked how the room stands: the connection to
    /// it is unreachable, and why.
    Unreachable(String),
}

/// Which connection a native launch's manager joins its room on.
#[cfg(feature = "native")]
pub enum NativeConnection {
    /// The one the launcher handed the process: its first window's.
    Launch,
    /// A window the shell opened in the running process, on the connection
    /// its request attached, if it attached one.
    Window(Option<runtime_portal::windows::Descriptor>),
}

/// Host-local original contributor custody a document change carries to the
/// transport, never serialized on the wire. The toolkit's invocation scope
/// mints it (`ui::invocation_scope::DocumentAuthorization`); the transport
/// checks it against the change it is about to sign.
pub trait DocumentCustody: Send + Sync + std::fmt::Debug {
    /// Whether the change's body and its current contributors are bound to
    /// `route`.
    ///
    /// # Errors
    /// Why the custody does not cover this change on this route.
    fn check(&self, route: &RouteSpec, mutation: &DocumentMutation) -> Result<(), String>;
    /// The bytes the custody holds while its change waits in the transport's
    /// queue, counted against the queue's budget; `None` when they overflow.
    fn retained_bytes(&self) -> Option<usize>;
}

#[derive(Debug, Clone)]
pub enum CollabRequest {
    #[cfg(feature = "broker-transport")]
    ExternalModeration {
        command: runtime_collab::external_moderation::Request,
        on_behalf_of: tilo_actions::InvocationId,
        reply: crate::invocation_scope::ExternalModerationReply,
    },
    SelectedDiscovery {
        request_id: u64,
        command: runtime_collab::open_selected::discovery::Request,
        on_behalf_of: Option<tilo_actions::InvocationId>,
    },
    OpenSelectedV2 {
        request_id: u64,
        command: runtime_collab::open_selected::v2::Request,
        on_behalf_of: tilo_actions::InvocationId,
    },
    OpenSelected {
        request_id: u64,
        command: runtime_collab::open_selected::Request,
        on_behalf_of: tilo_actions::InvocationId,
    },

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
        authorization: Option<std::sync::Arc<dyn DocumentCustody>>,
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
    /// cooperative path (the broker asks the room's other members).
    FetchScrollback {
        artifact: collab_types::ArtifactId,
        /// Exclusive upper bound on `seq`.
        before_seq: u64,
        /// Page size cap; chrome UX bounds pages to ~50 ops.
        max: u16,
    },
    /// Replace this application's complete exported resource/action surface,
    /// whole. For small surfaces a caller builds directly; the toolkit's own
    /// publication sends [`Self::PublishActionSurface`].
    PublishActions {
        actions: Vec<ExportedAction>,
    },
    /// Change this application's exported resource/action surface: the
    /// delta from what the broker holds (or, when it resets, a whole new
    /// surface). See [`SurfacePublication`].
    PublishActionSurface {
        delta: SurfaceDelta,
    },
    /// App-owned CRDT state projected for same-identity continuation. The
    /// local broker authenticates the route and stores this only for shell
    /// capture; it is never a room action or a restore command.
    ReportContinuation {
        schema_version: u16,
        metadata: Vec<u8>,
    },
    /// Remove the last continuation report after the app can no longer
    /// produce a safe projection.
    ClearContinuation,
    /// Answer a handoff request, or report a retirement
    /// ([`handoff::HandoffReport`], encoded). Session-local, on the primary
    /// route; never relayed to peers.
    ReportHandoff {
        message: Vec<u8>,
    },
    /// A change to this app's entries in the system's persistent search
    /// index ([`crate::search::index_publication::IndexDelta`], encoded).
    /// Bound to the certified primary route; never relayed to peers.
    PublishSearchIndex {
        delta_json: Vec<u8>,
    },
    /// One generic active-session continuity transition. The shell binds the
    /// command to this connection's certified app/workspace route.
    NativeMediaCommand {
        request_id: u64,
        command: runtime_collab::native_media::Request,
    },
    RealtimeCommand {
        request_id: u64,
        command: collab_types::realtime::RealtimeCommand,
    },
    /// One Files-owned picker/status/consent command. The broker authenticates
    /// the app route and keeps payload bytes on the shell transfer transport.
    NearbyTransferCommand {
        request_id: u64,
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
    UpdateInvocation {
        id: u64,
        update: tilo_actions::InvocationUpdate,
    },
    /// Private speculative document report on the existing authenticated route.
    /// Capture must retain the revision of its actual published directory; this
    /// request never resolves a newer revision or grants document authorization.
    ReportLocalDocumentAction {
        request_id: u64,
        report: runtime_collab::LocalDocumentActionReport,
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
    ReportTaskRun {
        request_id: Option<u64>,
        run_id: u64,
        lease: String,
        update: runtime_collab::TaskRunUpdate,
    },
    /// Invoke an already permission-filtered published action under the
    /// broker-owned task context. `request_id` correlates the async reply.
    InvokeTaskAction {
        request_id: u64,
        /// The caller's id for this invocation. A retry reuses it; the
        /// broker reports the original, and [`Self::RemoteAction`] observes
        /// or cancels it.
        invocation: tilo_actions::InvocationId,
        run_id: u64,
        lease: String,
        peer_id: u64,
        /// Stable catalogue provider for a closed app; never a publisher identity.
        provider: String,
        resource: String,
        action: String,
        arguments: serde_json::Map<String, serde_json::Value>,
        /// On a re-ask of a native task's own child: let the broker hold
        /// the answer until the child moves on (see
        /// [`runtime_collab::TaskChildWait`]). The connection's other
        /// requests do not wait behind it.
        wait: Option<runtime_collab::TaskChildWait>,
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
    /// Only explains an already-applied local mutation; never used to execute.
    ResolveLocalDocumentAction {
        revision: String,
        resource: String,
        action: String,
        reply: tokio::sync::oneshot::Sender<runtime_collab::ActionDirectoryResolution>,
    },
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

pub use runtime_collab::BrokerActionSurface;

#[derive(serde::Deserialize)]
#[cfg(feature = "broker-transport")]
pub struct BrokerActionCatalogueWire {
    pub(crate) surfaces: Vec<BrokerActionSurface>,
}

pub use runtime_collab::route::{ChannelAccessState, RouteSpec};

/// The custody types only this toolkit's endpoint mints: the committed
/// captures its collaboration events carry.
#[derive(Clone, Copy, Debug)]
pub enum SdkCustody {}

impl runtime_collab::event::EndpointCustody for SdkCustody {
    type CommittedRead = CommittedDocument;
}

/// One event this endpoint receives, carrying the toolkit's own custody
/// types. The vocabulary is defined in [`runtime_collab::event`], which the
/// session's brokers share.
pub type CollabEvent = runtime_collab::event::CollabEvent<SdkCustody>;

/// Explicit correlation supplied by owner-backed membership, never decoded
/// from a participant label or an actor-id prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertifiedAgentIdentity {
    pub participant_id: String,
    pub agent_key: String,
}

/// The colour a roster row's packed `0x00RRGGBB` names: the presence
/// hue nearest it, `None` for `0` or a grey.
#[must_use]
pub fn colour_from_packed(packed: u32) -> Option<collab_types::presence::PresenceHue> {
    if packed == 0 {
        return None;
    }
    let [_, r, g, b] = packed.to_be_bytes();
    collab_types::presence::PresenceHue::nearest_to_srgb(r, g, b)
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

    /// Why requests do not currently reach peers: the transport's own reason
    /// when it is offline, and what is still outstanding otherwise. A host
    /// reporting a refusal can name the cause instead of leaving it unknown.
    pub fn reason(&self) -> &str {
        match self {
            Self::Ready => "connected",
            Self::Connecting => "still connecting",
            Self::Offline(reason) => reason,
        }
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
pub struct PrimaryRouteWitness {
    #[cfg(not(any(test, feature = "test-support")))]
    route: RouteSpec,
    #[cfg(not(any(test, feature = "test-support")))]
    epoch: Uuid,
    // The toolkit's tests hold up witnesses of their own to an app.
    #[cfg(any(test, feature = "test-support"))]
    pub route: RouteSpec,
    #[cfg(any(test, feature = "test-support"))]
    pub epoch: Uuid,
}
impl PrimaryRouteWitness {
    /// The route the broker accepted.
    #[must_use]
    pub fn route(&self) -> &RouteSpec {
        &self.route
    }
    /// Which acceptance of the route this is.
    #[must_use]
    pub fn epoch(&self) -> Uuid {
        self.epoch
    }
    /// A witness the toolkit's tests hold up to a manager.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn for_test(route: RouteSpec, epoch: Uuid) -> Self {
        Self { route, epoch }
    }
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

/// The action directory on the primary route: an offer the connection makes,
/// not traffic it must carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryOffer {
    /// Not offered on this route yet.
    Unoffered,
    /// Registered on this route.
    Published,
    /// The transport serves no directory; the connection carries everything else.
    Unsupported,
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
    /// Where the action directory stands on the current primary route. A new
    /// `Hello` resets it.
    action_directory: DirectoryOffer,
    /// Whether this document was meant to sync: the manager was given a
    /// collaboration service to reach. Losing the service later keeps it
    /// set; a manager that never had one is a local document.
    sync_expected: bool,
    /// Why the broker refused the primary route this manager joined in the
    /// background. The app it serves keeps working on its local replica and
    /// says so ([`Self::join_refusal`]).
    join_refusal: Option<String>,
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
            action_directory: DirectoryOffer::Unoffered,
            sync_expected: true,
            join_refusal: None,
        };
        manager.reported_state = Some(manager.state());
        manager
    }

    /// Hand the transport the callback that schedules this owner's next
    /// frame ([`CollabTransport::set_wake`]): what a transport answered
    /// outside a drain of its own calls instead of waiting for one.
    pub fn set_transport_wake(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        if let Backend::Transport(transport) = &mut self.backend {
            transport.set_wake(wake);
        }
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
    pub fn switch_native_record(
        &mut self,
        _checkout: &crate::record::RecordCheckout,
    ) -> Result<(), String> {
        Err("native document routes need the broker transport".into())
    }

    pub fn has_native_committed_primary(&self) -> bool {
        match (&self.backend, &self.primary_route.accepted) {
            (Backend::Transport(transport), Some(route)) => {
                transport.committed_document_required(&route.route.channel_id())
            }
            _ => false,
        }
    }

    /// Join through the existing worker; never take the inherited RPC fd twice.
    pub fn prepare_native_record(
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

    pub fn require_document_authorization(&mut self) {
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
    pub fn disabled() -> Self {
        Self::offline(NO_SERVICE.to_owned())
    }

    fn offline(reason: String) -> Self {
        Self {
            reported_state: Some(CollaborationState::Offline(reason.clone())),
            backend: Backend::Disabled(reason),
            primary_route: PrimaryRouteState::default(),
            awaiting_primary: false,
            action_directory: DirectoryOffer::Unoffered,
            sync_expected: false,
            join_refusal: None,
        }
    }

    /// Ask the transport to report the broker's decision about the primary
    /// route announced just before, without waiting for it: the manager
    /// reports [`CollaborationState::Connecting`] until [`Self::poll`]
    /// observes the answer.
    fn confirm_primary_route_in_background(&mut self) {
        self.awaiting_primary = true;
        self.join_refusal = None;
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
        self.action_directory = DirectoryOffer::Unoffered;
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

    /// Whether the manager is joining its primary route in the background:
    /// it has a connection, and the broker has not answered the route yet.
    #[must_use]
    pub fn is_joining(&self) -> bool {
        self.awaiting_primary && matches!(self.backend, Backend::Transport(_))
    }

    /// What holds up the primary route this manager is joining, as the
    /// broker reports it: the room is not ready on this device yet, or the
    /// broker cannot be reached. `None` while nothing is reported, and once
    /// the route is joined or refused. Each change wakes the owner.
    #[must_use]
    pub fn join_hold(&self) -> Option<JoinHold> {
        if !self.awaiting_primary {
            return None;
        }
        match &self.backend {
            Backend::Transport(transport) => transport.join_hold(),
            Backend::Disabled(_) => None,
        }
    }

    /// Why the broker refused the primary route this manager was joining
    /// in the background, with the broker's own reason. The manager is
    /// offline from then on; the app keeps its local replica.
    #[must_use]
    pub fn join_refusal(&self) -> Option<&str> {
        self.join_refusal.as_deref()
    }

    /// Whether changes were meant to reach peers and currently do not: the
    /// manager had a collaboration service and has lost it. A document that
    /// never had one (local, outside any room) is not offline, and one still
    /// connecting has not lost anything yet.
    #[must_use]
    pub fn sync_lost(&self) -> bool {
        self.sync_expected && matches!(self.state(), CollaborationState::Offline(_))
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
    pub fn requested_primary_route(&self) -> Option<&RouteSpec> {
        match self.backend {
            Backend::Disabled(_) => None,
            Backend::Transport(_) => self.primary_route.requested.as_ref(),
        }
    }

    /// Join `checkout`'s route in place of the primary route this manager
    /// is still joining, without waiting: the broker's answer to the new
    /// route arrives as the last one would have ([`Self::poll`]).
    ///
    /// # Errors
    ///
    /// No route was requested, or the connection refused the request.
    pub fn retarget_join(
        &mut self,
        checkout: &crate::record::RecordCheckout,
    ) -> Result<(), String> {
        let pending = self
            .requested_primary_route()
            .cloned()
            .ok_or("native primary route unavailable")?;
        let record = (!checkout.id.is_package_primary()).then(|| checkout.id.as_str().to_owned());
        if pending.record == record {
            return Ok(());
        }
        if !self.send(CollabRequest::Hello {
            app_id: pending.app_id,
            cluster_id: pending.cluster_id,
            record,
            record_kind: Some(checkout.kind.as_str().to_owned()),
            handle: presence_handle_env(),
        }) {
            return Err("native route queue unavailable".into());
        }
        self.confirm_primary_route_in_background();
        Ok(())
    }

    /// Without a built-in transport there is nothing to discover.
    #[cfg(not(feature = "broker-transport"))]
    fn new_scoped(_record: Option<String>, _record_kind: Option<String>) -> Self {
        Self::disabled()
    }

    /// True when connected to a live broker — used by the
    /// event loop to keep ticking so the stream drains promptly.
    pub fn primary_route(&self) -> Option<&PrimaryRouteWitness> {
        self.primary_route.current(self.is_connected())
    }

    pub fn private_window_instance(&self) -> Option<u64> {
        let witness = self.primary_route()?;
        let Backend::Transport(transport) = &self.backend else {
            return None;
        };
        if transport.committed_document_required(&witness.route.channel_id()) {
            return transport.private_window_binding(&witness.route);
        }
        let instance = witness
            .route
            .record
            .as_deref()?
            .strip_prefix("continuation:")?;
        match instance.parse::<u64>() {
            Ok(instance) => Some(instance),
            Err(error) => {
                tracing::warn!(instance, %error, "collab: continuation record names no window instance");
                None
            }
        }
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

    /// Fire-and-forget and non-blocking on every backend. Presence may be
    /// shed under saturation; any other durable command is accepted and
    /// waits for the transport's queue to have room, so a burst is
    /// backpressure, never a disconnect. Document mutations never disconnect: see
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
        self.action_directory = DirectoryOffer::Unoffered;
        self.submit(TransportCommand::Primary(req))?;
        if self.awaiting_primary {
            // The pending acknowledgement answers the previous Hello; ask
            // again, behind this one.
            self.confirm_primary_route_in_background();
        }
        Ok(())
    }

    /// Submit an exact capture without querying or rebasing its directory revision.
    /// The caller drains only its route's local-input queue and retains errors for
    /// presentation; queue acceptance alone is not admission or signed publication.
    /// Replies arrive as [`CollabEvent::LocalDocumentActionReported`].
    pub fn report_local_document_action(
        &mut self,
        request_id: u64,
        capture: crate::local_document_action::CapturedAction,
    ) -> Result<(), String> {
        let report = Self::local_document_action_report(capture)?;
        self.try_send(CollabRequest::ReportLocalDocumentAction { request_id, report })
            .map_err(|error| error.to_string())
    }

    /// Report a capture on an explicitly retained shared or local document route.
    /// The route must own the captured input and published directory revision.
    /// Queueing and admission supply neither execution proof nor publication authority.
    /// Replies arrive as [`CollabEvent::LocalDocumentActionReported`].
    pub fn report_local_document_action_to_route(
        &mut self,
        route: RouteSpec,
        request_id: u64,
        capture: crate::local_document_action::CapturedAction,
    ) -> Result<(), runtime_collab::LocalDocumentActionError> {
        let report = Self::local_document_action_report(capture)
            .map_err(runtime_collab::LocalDocumentActionError::Rejected)?;
        self.try_send_to_route(
            route,
            CollabRequest::ReportLocalDocumentAction { request_id, report },
        )
        .map_err(|error| runtime_collab::LocalDocumentActionError::Uncertain(error.to_string()))
    }

    fn local_document_action_report(
        capture: crate::local_document_action::CapturedAction,
    ) -> Result<runtime_collab::LocalDocumentActionReport, String> {
        let report = capture.into_report()?;
        // Reads/no-ops retain their empty mutation sets. Never infer a published
        // action from the encoded Rust action type, or rebase a captured revision.
        report.encode()?;
        Ok(report)
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
        if !self.is_connected() || self.action_directory != DirectoryOffer::Unoffered {
            return;
        }
        match self.submit(TransportCommand::PublishActionDirectory {
            app_id: app_id.into(),
        }) {
            Ok(()) => self.action_directory = DirectoryOffer::Published,
            // A transport that serves no directory is asked again only on a new route.
            Err(TransportError::Unsupported) => self.action_directory = DirectoryOffer::Unsupported,
            Err(_) => {}
        }
    }

    pub fn action_directory_observation(&self) -> (bool, Option<(u64, u64)>) {
        let counts = match &self.backend {
            Backend::Transport(transport) => transport.action_directory_publication_counts(),
            Backend::Disabled(_) => None,
        };
        (self.action_directory == DirectoryOffer::Published, counts)
    }

    /// Drain requests issued by the transport's `ActionDirectory` server.
    /// This must be called by [`crate::app::App`] on its owner thread.
    pub fn take_action_directory_requests(&mut self) -> Vec<ActionDirectoryRequest> {
        let mut requests = Vec::new();
        if let Backend::Transport(transport) = &mut self.backend {
            transport.take_action_directory_requests(&mut requests);
        }
        requests
    }

    /// Join and subscribe to an additional route without publishing traffic.
    /// Repeated joins of the same route are idempotent.
    /// Join `route`, whose record holds `record_kind`. The kind travels with
    /// the join so a host that must create the record when this device
    /// writes first knows what it holds; joining without it would let the
    /// host answer that the app named no kind.
    pub fn join_route(&mut self, route: RouteSpec, record_kind: crate::record::RecordKind) {
        let _ = self.submit(TransportCommand::Join(route, record_kind));
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
    /// This is the same transport gate used before document publication. A
    /// closed transport never reopens, so its routes stop being ready the
    /// moment it closes, before the worker clears their read state.
    pub fn document_route_ready(&self, route: Option<&RouteSpec>) -> bool {
        let channel = route.map(RouteSpec::channel_id);
        !self.awaiting_primary
            && match &self.backend {
                Backend::Transport(transport) => {
                    !matches!(transport.status(), TransportStatus::Closed(_))
                        && transport.document_publishable(channel.as_deref())
                }
                Backend::Disabled(_) => false,
            }
    }

    /// Ask `route`'s reader for the whole committed read again, as an owner
    /// being built does: a request the reader answers with an Open.
    #[cfg(all(feature = "native", any(test, feature = "test-support")))]
    pub fn controlled_request_whole_read_for_test(&self, route: &RouteSpec) -> bool {
        match &self.backend {
            Backend::Transport(transport) => {
                transport.controlled_request_whole_read_for_test(&route.channel_id())
            }
            Backend::Disabled(_) => false,
        }
    }

    /// Why the last join of `route` failed, until a join of it succeeds:
    /// a route that never joined (a refused committed read, a record its
    /// room cannot create) says why here, rather than staying unready.
    #[must_use]
    pub fn route_join_failure(&self, route: &RouteSpec) -> Option<String> {
        match &self.backend {
            Backend::Transport(transport) => transport.route_join_failure(&route.channel_id()),
            Backend::Disabled(_) => None,
        }
    }

    /// Independently sample the existing transport read gates without admission.
    #[cfg(all(feature = "native", any(test, feature = "test-support")))]
    pub fn controlled_document_read_phase_for_test(
        &self,
        route: &RouteSpec,
    ) -> Option<serde_json::Value> {
        if let Backend::Transport(transport) = &self.backend {
            transport.controlled_document_read_phase_for_test(&route.channel_id())
        } else {
            None
        }
    }

    /// Whether `route` takes documents now: joined, and its original
    /// history applied.
    pub fn document_route_publishable(&self, route: &RouteSpec) -> bool {
        match &self.backend {
            Backend::Transport(transport) => {
                transport.document_publishable(Some(&route.channel_id()))
            }
            Backend::Disabled(_) => false,
        }
    }

    /// Test-only: whether `route` has resolved its document mode, so a
    /// document mutation on it would be admitted rather than refused.
    #[cfg(any(test, feature = "test-support"))]
    pub fn document_route_publishable_for_test(&self, route: &RouteSpec) -> bool {
        self.document_route_publishable(route)
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
            tracing::debug!(
                cause = "missing_committed_document_custody",
                "document route handoff refused"
            );
            return Err(CollabSendError::Unavailable);
        }
        if matches!(req, CollabRequest::Hello { .. }) {
            tracing::warn!("collab: send_to_route ignores Hello; use join_route");
            return Err(CollabSendError::Unavailable);
        }
        if self.awaiting_primary {
            tracing::debug!(
                cause = "primary_join_pending",
                "document route handoff refused"
            );
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
            let required = match &self.backend {
                Backend::Transport(transport) => {
                    transport.committed_document_required(&route.channel_id())
                }
                Backend::Disabled(_) => false,
            };
            tracing::debug!(
                cause = "original_history_unapplied",
                required,
                "document route handoff refused"
            );
            // Join first; the route's original history must be applied
            // before any local mutation is published on it. The join names
            // the kind the owner opened the record with, so the host can
            // create the record when this device writes first; the owner's
            // own `join_route` is what names it, and a join repeated here
            // only reaches a host that already has the record.
            let record_kind = match &self.backend {
                Backend::Transport(transport) => transport.joined_record_kind(&route.channel_id()),
                Backend::Disabled(_) => None,
            };
            let _ = self.submit(TransportCommand::Join(
                route,
                record_kind.unwrap_or_else(|| crate::record::RecordKind::new("")),
            ));
            return Err(CollabSendError::Unavailable);
        }
        // A known committed route may be refreshing. Required original
        // custody was checked above; retain this mutation in the bounded queue.
        // The worker still waits for this exact route before checking retained
        // custody and applying the existing certification/commit policy.
        let result = self.submit(TransportCommand::ToRoute(route, req));
        if let Err(error) = &result {
            tracing::debug!(%error, cause = "transport_queue_refusal", "document route handoff refused");
        }
        Ok(result?)
    }

    pub fn document_publication_failed(&self) -> bool {
        matches!(&self.backend, Backend::Transport(transport) if transport.document_publication_failed())
    }

    /// The kind of record `route` was joined with, as the transport holds it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn joined_record_kind(&self, route: &RouteSpec) -> Option<crate::record::RecordKind> {
        match &self.backend {
            Backend::Disabled(_) => None,
            Backend::Transport(transport) => transport.joined_record_kind(&route.channel_id()),
        }
    }

    /// The last publication outcome the transport reported, for telling a
    /// refusal of one change apart from a lost connection.
    #[cfg(test)]
    pub fn document_publication_failure(&self) -> Option<DocumentPublicationError> {
        match &self.backend {
            Backend::Disabled(_) => None,
            Backend::Transport(transport) => transport.publication_outcome(),
        }
    }

    /// The last change the peer answered but would not accept, while nothing
    /// later went through. The bridge is still connected: only that change is
    /// not accepted, and the owner reconciles its local copy.
    pub fn refused_change(&self) -> Option<String> {
        match &self.backend {
            Backend::Disabled(_) => None,
            Backend::Transport(transport) => transport.refused_change(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_publication_for_test(&self) {
        if let Backend::Transport(transport) = &self.backend {
            transport.fail_closed("test terminal publication invalidation");
        }
    }

    /// Whether the broker refused or failed an action-surface publication
    /// since the last call; the caller must republish its surface.
    pub fn take_action_surface_failed(&mut self) -> bool {
        match &mut self.backend {
            Backend::Transport(transport) => transport.take_action_surface_failed(),
            Backend::Disabled(_) => false,
        }
    }

    pub fn stop_after_publication_failure(&mut self) {
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

    pub fn set_document_owner_routes(&mut self, routes: Vec<RouteSpec>, document_generation: u64) {
        if let Backend::Transport(transport) = &mut self.backend {
            transport.set_document_owner_routes(routes, document_generation);
        }
    }

    /// Whether invocation `id` from the drained batch may still run in the
    /// current document; see [`CollabTransport::invocation_owner_current`].
    pub fn invocation_owner_current(&mut self, id: u64) -> bool {
        match &mut self.backend {
            Backend::Transport(transport) => transport.invocation_owner_current(id),
            Backend::Disabled(_) => true,
        }
    }

    /// Whether this app is one of the desktop's own surfaces: what it offers
    /// lives with the desktop it speaks to, not in a document of its own.
    pub fn hosts_surface(&mut self) -> bool {
        match &mut self.backend {
            Backend::Transport(transport) => transport.hosts_surface(),
            Backend::Disabled(_) => false,
        }
    }

    pub fn finish_event_batch(&mut self) {
        if let Backend::Transport(transport) = &mut self.backend {
            transport.finish_event_batch();
        }
    }

    /// Validated inert provenance for this exact authenticated route.
    pub fn connection_snapshot(
        &self,
        route: &RouteSpec,
    ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
        let Backend::Transport(transport) = &self.backend else {
            return None;
        };
        transport.connection_snapshot(route).filter(|snapshot| {
            snapshot.valid()
                && snapshot.app_id == route.app_id
                && snapshot.room == route.cluster_id.to_string()
                && snapshot.document == route.record.as_deref().unwrap_or("")
        })
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
                (Some(Err(error)), _) => {
                    self.join_refusal = Some(error.clone());
                    self.go_offline(error);
                }
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

pub use runtime_collab::route::channel_key;

/// The `(app_id, cluster_id)` pair this process announces in
/// [`CollabRequest::Hello`] — the broker delivers a CRDT delta only to
/// connections announcing the same pair.
///
/// # Precedence (each half resolves independently)
///
/// 1. `TILO_PACKAGE_ID` / `TILO_WORKSPACE_ID` — launcher-set for every
///    compositor-started app.
/// 2. `QUICK_APP_ID` — standalone-dev app id, ranked below
///    `TILO_PACKAGE_ID` so a dev identity never fragments routing under a
///    real session.
/// 3. A fresh random app id + the nil cluster — fails closed: nothing
///    else announces the same id, so nothing is ever delivered here.
///
/// `TILO_WORKSPACE_ID` counts as the cluster only when it parses as a
/// uuid — it's also read elsewhere as a bare "am I on Tilo?" flag, so an
/// unroutable value falls through instead of being rejected.
fn auto_hello_env() -> (String, Uuid) {
    auto_hello_env_uncached()
}

/// Make the shell's records this process's settings from the start of its
/// launch, when the launcher handed it the shell's collaboration service.
/// A setting the app reads or writes while it builds its window, before its
/// collaboration manager connects, is then the shell's: a write waits in
/// the channel's outbox and the primary route's join sends it, in the
/// order it was made. Installed only when the manager was built, an early
/// write went to the sandbox's own settings file and was lost.
#[cfg(feature = "native")]
pub fn install_channel_settings() {
    if runtime_portal::service_fd("collab").is_none() {
        return;
    }
    let (app, _) = auto_hello_env();
    channel_settings::install(&app);
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
pub fn local_user_id() -> collab_types::presence::UserId {
    collab_types::presence::UserId(resolve_local_user_id())
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
    if let collab_types::caps::Principal::Person(id) =
        collab_types::caps::Principal::person_from_key(
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

    // No workspace named is the nil room; one named that does not parse
    // is the same, and logged, since the app then reaches no workspace.
    let cluster_id = identity
        .workspace_id
        .as_deref()
        .map_or(Ok(Uuid::nil()), Uuid::parse_str)
        .unwrap_or_else(|error| {
            tracing::warn!(workspace = ?identity.workspace_id, %error, "collab: launch workspace id does not parse");
            Uuid::nil()
        });

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

#[cfg(test)]
mod local_document_report_tests {
    use super::*;
    use crate::local_document_action::{CapturedAction, PublishedInvocation};
    use std::sync::{Arc, Mutex};
    use tilo_lock::MutexRecover as _;

    type Reports = Arc<Mutex<Vec<(Option<RouteSpec>, u64, Vec<u8>)>>>;
    struct CaptureTransport(Reports);
    impl CollabTransport for CaptureTransport {
        fn status(&self) -> TransportStatus {
            TransportStatus::Ready
        }
        fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
            let (route, request) = match command {
                TransportCommand::Primary(request) => (None, request),
                TransportCommand::ToRoute(route, request) => (Some(route), request),
                _ => return Err(TransportError::Unsupported),
            };
            let CollabRequest::ReportLocalDocumentAction { request_id, report } = request else {
                return Err(TransportError::Unsupported);
            };
            self.0
                .lock()
                .unwrap()
                .push((route, request_id, report.encode().unwrap()));
            Ok(())
        }
        fn drain(&mut self, _: &mut Vec<CollabEvent>) {}
        fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
            None
        }
        fn take_action_directory_requests(&mut self, _: &mut Vec<ActionDirectoryRequest>) {}
    }

    fn capture() -> CapturedAction {
        let mut capture = CapturedAction {
            custody: None,
            invocation: uuid::Uuid::from_bytes([1; 16]),
            capture_id: uuid::Uuid::from_bytes([5; 16]),
            revision: "captured-not-current".into(),
            published: Some(PublishedInvocation {
                resource: "document/child".into(),
                action: "read".into(),
                arguments: serde_json::from_value(serde_json::json!({"text": "private argument"}))
                    .unwrap(),
            }),
            encoded: tilo_actions::EncodedAction {
                type_id: "private.typed-action".into(),
                payload: br#"{"bytes":[0,255,17]}"#.to_vec(),
            },
            context: Some(tilo_actions::ActionContext {
                purpose: Some("private context".into()),
                group: Some(tilo_actions::WorkGroup {
                    id: "private-group".into(),
                    label: "Private work".into(),
                    kind: "task".into(),
                }),
                source_invocation: Some(tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                    [2; 16],
                ))),
                ..Default::default()
            }),
            original_heads: vec![],
            mutation_hashes: vec![],
        };
        capture.invocation = capture.bound_invocation().unwrap();
        capture
    }

    #[test]
    fn exact_capture_reports_on_primary_shared_and_local_routes_without_mutations() {
        let reports = Reports::default();
        let mut manager =
            CollaborationManager::with_transport(Box::new(CaptureTransport(reports.clone())));
        let captured = capture();
        manager
            .report_local_document_action(10, captured.clone())
            .unwrap();
        let shared = RouteSpec::record("app", uuid::Uuid::from_bytes([3; 16]), "shared");
        let local = RouteSpec::record("app", uuid::Uuid::from_bytes([4; 16]), "local");
        for (request_id, route) in [(11, shared.clone()), (12, local.clone())] {
            manager
                .report_local_document_action_to_route(route, request_id, captured.clone())
                .unwrap();
        }
        let reports = reports.lock_recover();
        assert_eq!(reports.len(), 3);
        for ((route, request_id, bytes), (expected_route, expected_id)) in
            reports
                .iter()
                .zip([(None, 10), (Some(shared), 11), (Some(local), 12)])
        {
            assert_eq!(route, &expected_route);
            assert_eq!(*request_id, expected_id);
            assert_eq!(bytes, &reports[0].2);
            let report: runtime_collab::LocalDocumentActionReport =
                serde_json::from_slice(bytes).unwrap();
            assert_eq!(report.invocation, captured.invocation);
            assert_eq!(report.capture_id, captured.capture_id);
            assert_eq!(report.bound_invocation().unwrap(), captured.invocation);
            assert_eq!(report.revision, captured.revision);
            assert_eq!(report.encoded, captured.encoded);
            assert_eq!(report.context, captured.context);
            assert_eq!(report.group, captured.context.as_ref().unwrap().group);
            assert_eq!(report.resource, "document/child");
            assert!(report.original_heads.is_empty());
            assert!(report.mutation_hashes.is_empty());
            let expected_argument = format!(
                "{}{{\"text\":\"private argument\"}}",
                tilo_actions::ARGUMENTS_JSON_PREFIX
            );
            assert_eq!(report.argument.as_deref(), Some(expected_argument.as_str()));
            let debug = format!("{report:?}");
            for private in ["private", "encoded", "context", "argument"] {
                assert!(!debug.contains(private));
            }
        }
    }

    #[test]
    fn unavailable_transport_and_missing_published_invocation_refuse() {
        assert!(
            CollaborationManager::disabled()
                .report_local_document_action(1, capture())
                .is_err()
        );
        let mut missing = capture();
        missing.published = None;
        let reports = Reports::default();
        let mut manager =
            CollaborationManager::with_transport(Box::new(CaptureTransport(reports.clone())));
        assert!(manager.report_local_document_action(2, missing).is_err());
        assert!(reports.lock_recover().is_empty());
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

    #[test]
    fn only_a_document_that_had_a_service_reports_sync_lost() {
        assert!(!CollaborationManager::disabled().sync_lost());
        let mut manager = CollaborationManager::with_peer_transport(ClosedPeer);
        assert!(!manager.sync_lost());
        manager.send(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(vec![1]),
            authorization: None,
        });
        assert!(manager.sync_lost());
    }
}

/// Original authority retained across display background work.
pub use runtime_collab::DisplayOrigin;

/// A primary route whose broker answers late leaves the app up: the manager
/// joins in the background and takes the answer, the broker's hold-ups and
/// its refusal as events. Nothing is retried and no clock decides.
#[cfg(all(test, feature = "broker-transport"))]
mod late_primary_route_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tilo_lock::MutexRecover as _;

    /// A broker connection whose answer to the primary route, and what it
    /// says holds the join up, the test hands out; it counts the `Hello`s
    /// it was sent.
    #[derive(Clone, Default)]
    struct Late {
        answer: Arc<Mutex<Option<Result<RouteSpec, String>>>>,
        hold: Arc<Mutex<Option<JoinHold>>>,
        hellos: Arc<Mutex<usize>>,
    }
    impl CollabTransport for Late {
        fn status(&self) -> TransportStatus {
            TransportStatus::Ready
        }
        fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
            if let TransportCommand::Primary(CollabRequest::Hello { .. }) = command {
                *self.hellos.lock_recover() += 1;
            }
            Ok(())
        }
        fn drain(&mut self, _: &mut Vec<CollabEvent>) {}
        fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
            self.answer.lock_recover().take()
        }
        fn join_hold(&self) -> Option<JoinHold> {
            self.hold.lock_recover().clone()
        }
        fn take_action_directory_requests(&mut self, _: &mut Vec<ActionDirectoryRequest>) {}
    }

    fn route() -> RouteSpec {
        RouteSpec {
            app_id: "so.tilo.notes".into(),
            cluster_id: uuid::Uuid::from_bytes([7; 16]),
            record: None,
        }
    }

    /// A manager joining `route()` on `transport` in the background, as a
    /// native launch's is.
    fn joining(transport: &Late) -> CollaborationManager {
        let mut manager = CollaborationManager::with_transport(Box::new(transport.clone()));
        let route = route();
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id,
            cluster_id: route.cluster_id,
            record: None,
            record_kind: Some("notes.notebook".into()),
            handle: None,
        }));
        manager.confirm_primary_route_in_background();
        manager
    }

    #[test]
    fn a_late_primary_route_leaves_the_app_up_and_joins_when_it_answers() {
        let transport = Late::default();
        let mut manager = joining(&transport);
        assert_eq!(manager.state(), CollaborationState::Connecting);
        assert!(manager.is_joining());
        assert!(!manager.sync_lost());
        assert_eq!(manager.join_hold(), None, "a join under way says nothing");

        // The broker has not answered: nothing happens, and nothing is
        // announced again.
        manager.poll();
        assert_eq!(manager.state(), CollaborationState::Connecting);
        assert_eq!(*transport.hellos.lock_recover(), 1);

        // The route answers: the app joins it, with nothing to do on its part.
        *transport.answer.lock_recover() = Some(Ok(route()));
        manager.poll();
        assert_eq!(manager.state(), CollaborationState::Ready);
        assert!(!manager.is_joining());
        assert_eq!(
            manager
                .primary_route()
                .map(|witness| witness.route().clone()),
            Some(route())
        );
        assert_eq!(
            manager.take_state_change(),
            Some(CollaborationState::Ready),
            "the owner does its connect-time work once joined"
        );
        assert_eq!(
            *transport.hellos.lock_recover(),
            1,
            "a joined route is not announced again"
        );
    }

    #[test]
    fn the_join_says_what_the_broker_says_holds_it_up() {
        let transport = Late::default();
        let mut manager = joining(&transport);
        let waiting = JoinHold::RoomWaiting {
            reason: "Placing the workspace on its host".into(),
            detail: Some("the host is offline".into()),
        };
        *transport.hold.lock_recover() = Some(waiting.clone());
        assert_eq!(manager.join_hold(), Some(waiting));
        let unreachable = JoinHold::Unreachable("the broker went away".into());
        *transport.hold.lock_recover() = Some(unreachable.clone());
        assert_eq!(manager.join_hold(), Some(unreachable));
        *transport.hold.lock_recover() = None;
        assert_eq!(manager.join_hold(), None);

        // Joined: nothing holds it up any more, whatever was said last.
        *transport.hold.lock_recover() = Some(JoinHold::Unreachable("stale".into()));
        *transport.answer.lock_recover() = Some(Ok(route()));
        manager.poll();
        assert_eq!(manager.join_hold(), None);
    }

    #[test]
    fn a_refused_route_still_takes_the_app_offline() {
        let transport = Late::default();
        let mut manager = joining(&transport);
        *transport.answer.lock_recover() = Some(Err("not a member of this workspace".into()));
        manager.poll();
        assert!(matches!(manager.state(), CollaborationState::Offline(_)));
        assert_eq!(
            manager.join_refusal(),
            Some("not a member of this workspace"),
            "the app says why, with the broker's reason"
        );
        assert!(manager.sync_lost());
        assert!(!manager.is_joining());
    }
}
