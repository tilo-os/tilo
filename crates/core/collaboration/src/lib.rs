//! The collab client without the toolkit.
//!
//! A process that commits to a Tilo workspace links this crate, not the UI
//! toolkit: an app host running tilolang apps as wasm instances, a browser
//! worker, a host in thin mode. The toolkit builds on it and keeps its paths
//! (`ui::collaboration`, `ui::resource`,
//! `ui::local_identity`, `ui::local_document_action`), so native apps
//! compile unchanged.
//!
//! - [`record`]: a record's document, as the record store keeps it.
//! - [`local_identity`]: who sits at this device, the process's one identity
//!   boundary.
//! - [`local_document_action`]: a local action observation as it is
//!   reported to the workspace.
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
//! [`CollabTransport::drain`]. The transport owns authentication; the device's
//! store signs what the app writes, and the manager never holds a key for it. Durable
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
pub mod document;
mod incoming;
pub use incoming::Incoming;
pub mod invocation_scope;
pub mod local_document_action;
pub mod local_identity;
pub mod resource;
pub mod state;

// The clock, wake routing and host inputs, under the paths the toolkit
// names them by (`ui::time`, `ui::wake`, `ui::process`), without the toolkit's
// host services.
pub(crate) mod time {
    pub use tilo_clock::now;
    pub use tilo_clock::time::{Duration, Instant};
}
pub(crate) use tilo_rt::wake;
pub(crate) use wake::WakeHandle;
#[cfg(test)]
pub(crate) use wake::WakeScope;
// The tests bound what the sender's storage allocates.

pub(crate) mod process {
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
mod broker;
pub mod browser_service_sessions;
pub mod channel_live_activity;
#[cfg(feature = "broker-transport")]
pub mod channel_settings;
#[cfg(feature = "broker-transport")]
pub mod external_moderation;
pub mod handoff;
#[cfg(feature = "broker-transport")]
mod route_endpoints;

pub use runtime_collab::{SurfaceDelta, SurfacePublication, SurfaceTable};
pub mod document_content;
#[cfg(feature = "broker-transport")]
mod route_keys;
#[cfg(feature = "broker-transport")]
pub use broker::SessionServices;
#[cfg(feature = "broker-transport")]
pub use broker::{
    EndpointChannelKeys, SessionResource, display_control, has_resource_connection,
    resource_command,
};
// Committed reads live with the broker worker that retains them. Without the
// broker transport no committed capture can exist, but the type stays part of
// `CollabEvent` and native construction ownership still runs (always on the
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
        pub fn was_applied(&self) -> bool {
            match self.never {}
        }
        pub fn applies_to(&self, _document: &impl automerge::ReadDoc) -> bool {
            match self.never {}
        }
        pub fn refuse(&self) {
            match self.never {}
        }
        pub fn is_whole(&self) -> bool {
            match self.never {}
        }
        pub fn was_refused(&self) -> bool {
            match self.never {}
        }
        pub fn with_incoming<T>(
            &self,
            _apply: impl FnOnce(crate::Incoming<'_>) -> T,
        ) -> Result<T, String> {
            match self.never {}
        }
        pub fn take_into(&self, _document: &mut automerge::AutoCommit) -> Option<bool> {
            match self.never {}
        }
    }
}

pub use collab_types::{CommitAuthor, CommitPart, DocumentMutation, UnitMutation, WorkspaceCommit};
pub use runtime_collab::NearbyTransferPhase;
pub use runtime_collab::{file_clipboard, recall};

pub use collab_types::MemberId;
pub use collab_types::presence::{Member, Person, PersonId, PresencePayload};

use std::sync::mpsc;

/// The native display control service is unavailable without an admitted broker.
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
    pub fn open(_resource: &str) -> Result<Self, String> {
        Err("Published resources need the certified collaboration service.".into())
    }

    pub fn follow(_resource: &str, _wake: tilo_rt::wake::WakeHandle) -> Result<Self, String> {
        Err("Published resources need the certified collaboration service.".into())
    }

    /// Nothing is watched on this transport.
    #[must_use]
    pub fn latest(&self) -> Option<Result<String, String>> {
        None
    }

    /// Nothing is watched on this transport, so no reading wakes a view.
    pub fn attach(&self, _wake: tilo_rt::wake::WakeHandle) {}

    pub fn invoke(
        &self,
        _action: &str,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        Err("Published resources need the certified collaboration service.".into())
    }

    /// Nothing is reached on this transport, so no action is invoked.
    pub fn invoke_without_watching(
        _resource: &str,
        _action: &str,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        Err("Published resources need the certified collaboration service.".into())
    }
}

pub use crate::process::ContinuationMetadata;
/// The launcher-only resume payload the process passed this window
/// ([`crate::process::ProcessIdentity::continuity_resume`]).
pub fn continuation_restore_env() -> Option<ContinuationMetadata> {
    crate::process::environment()
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

/// Why a native launch's manager could not join its workspace.
#[cfg(feature = "native")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeLaunchError {
    /// The configured service could not be reached, or this device has no
    /// identity to join its workspace as.
    Operation(String),
}

/// What holds up a primary route's join, as the broker reports it while the
/// join is pending ([`CollaborationManager::join_hold`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinHold {
    /// The broker holds the join until the workspace is ready on this device:
    /// why, in the person's words, and what last held it up when something
    /// did.
    WorkspaceWaiting {
        reason: String,
        detail: Option<String>,
    },
    /// The broker could not be asked how the workspace stands: the connection to
    /// it is unreachable, and why.
    Unreachable(String),
}

/// Which connection a native launch's manager joins its workspace on.
#[cfg(feature = "native")]
pub enum NativeConnection {
    /// The one the launcher handed the process: its first window's.
    Launch,
    /// A window the shell opened in the running process, on the connection
    /// its request attached, if it attached one.
    Window(Option<runtime_portal::process_link::Descriptor>),
    /// The first window of a process that is its own launcher (a phone, which
    /// runs the device's daemon in the app's process): the connections that
    /// daemon made for the window, as a launcher makes them for a process it
    /// starts.
    Handed(HandedConnection),
}

/// What a process that is its own launcher hands its first window: the
/// window's collaboration connection, which the collaboration service attests
/// as that window from the record the daemon made of it, its identity-service
/// connection, which signs what it writes as the person, and the app and
/// workspace it joins.
#[cfg(feature = "native")]
pub struct HandedConnection {
    pub collaboration: runtime_portal::process_link::Descriptor,
    pub identity: runtime_portal::process_link::Descriptor,
    pub app: String,
    pub workspace: Uuid,
}

#[derive(Debug, Clone)]
pub enum CollabRequest {
    #[cfg(feature = "broker-transport")]
    ExternalModeration {
        command: runtime_collab::external_moderation::Request,
        on_behalf_of: tilo_actions::InvocationId,
        reply: crate::invocation_scope::ExternalModerationReply,
    },
    Hello {
        app_id: String,
        cluster_id: Uuid,
        /// Which **record** ([`collab_types::document`]) this connection carries, when
        /// adopted. The broker delivers payloads only to connections
        /// announcing the same record, so two people editing different
        /// files in one workspace don't merge each other's state; `None` means
        /// one document per `(app, workspace)`.
        record: Option<String>,
        /// The resource kind of `record`, named so a host that creates a
        /// missing record on the app's first write knows what it holds.
        resource_type: Option<String>,
        /// Name on the caret pill; `None` gets a synthetic `peer-<n>`.
        /// Informational only — never used for routing or grants.
        handle: Option<String>,
    },
    /// The app's own changes of the route's record: the record whole (every
    /// change it holds, for a record's first write) or the changes since what
    /// the store last took. The device's store takes them as the app's
    /// changes (`collab_types::store::wire::Command::RecordChange`) and signs
    /// them itself.
    Document {
        mutation: DocumentMutation,
        /// The action this mutation is one part of, when the caller made it
        /// as part of an action that writes more than one unit: every part
        /// carries the same commit, and every device applies them all
        /// together or not at all. `None` for a write of this unit alone.
        commit: Option<collab_types::WorkspaceCommit>,
    },
    /// Content this route's record references, which the app stored: the
    /// host keeps it where the record's readers are served from.
    OfferContent {
        hash: String,
        bytes: Vec<u8>,
    },
    /// Content this route's record references and the app lacks. Answered,
    /// from this device or the route's other devices, with
    /// [`CollabEvent::DocumentContent`].
    WantContent {
        hash: String,
    },
    /// Change this application's exported resource/action surface: the
    /// delta from what the broker holds (or, when it resets, a whole new
    /// surface). See [`SurfacePublication`].
    PublishActionSurface {
        delta: SurfaceDelta,
    },
    /// App-owned CRDT state projected for same-identity continuation. The
    /// local broker authenticates the route and stores this only for shell
    /// capture; it is never a workspace action or a restore command.
    ReportContinuation {
        schema_version: u16,
        metadata: Vec<u8>,
    },
    /// Remove the last continuation report after the app can no longer
    /// produce a safe projection.
    ClearContinuation,
    /// Answer a handoff request
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
    ResourceLocalEffect {
        address: tilo_actions::ActionAddress,
        update: tilo_actions::InvocationUpdate,
    },
    /// One remote-action request to the session's action host: list the
    /// actions offered to this connection, invoke one, observe or cancel an
    /// invocation. The answer arrives as [`CollabEvent::RemoteAction`].
    ///
    /// `press` is the input serial the person's press that ran the request
    /// travelled with, as the compositor issued it to this app's window: a
    /// request naming none followed no press, and one naming a serial the
    /// compositor did not issue to the window proves nothing.
    RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage,
        press: Option<u32>,
    },
    /// Write or read this app's resources in the device's one store, which answers with
    /// [`CollabEvent::ResourceCommandResult`]. The app holds no store of its own; the command is
    /// in the store's encoding (`collab_types::store::wire::Command`), which the connection
    /// carries as bytes.
    ResourceCommand {
        request_id: u64,
        command: Vec<u8>,
    },
    /// Ask the host what it says of the actions the window shows, replacing what was asked
    /// before: answered with [`CollabEvent::ActionVerdicts`] now and again whenever grants change
    /// one. Empty asks nothing more.
    DecideShownActions {
        actions: Vec<tilo_permissions::shown_actions::ShownAction>,
    },
}

impl CollabRequest {
    pub fn document_snapshot(data: Vec<u8>) -> Self {
        Self::Document {
            mutation: DocumentMutation::Snapshot(data),
            commit: None,
        }
    }

    pub fn document_incremental(data: Vec<u8>) -> Self {
        Self::Document {
            mutation: DocumentMutation::Incremental(data),
            commit: None,
        }
    }

    /// One part of the action `commit` names: the changes this unit made,
    /// carrying the action across every unit it wrote, so every device
    /// applies all of it or none.
    pub fn document_action(
        mutation: DocumentMutation,
        commit: collab_types::WorkspaceCommit,
    ) -> Self {
        Self::Document {
            mutation,
            commit: Some(commit),
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

pub use runtime_collab::route::{KeyAccessState, RouteSpec};

/// The ownership types only this toolkit's endpoint mints: the committed
/// captures its collaboration events carry.
#[derive(Clone, Copy, Debug)]
pub enum SdkAuthority {}

impl runtime_collab::event::EndpointOwnership for SdkAuthority {
    type CommittedRead = CommittedDocument;
}

/// One event this endpoint receives, carrying the toolkit's own ownership
/// types. The vocabulary is defined in [`runtime_collab::event`], which the
/// session's brokers share.
pub type CollabEvent = runtime_collab::event::CollabEvent<SdkAuthority>;

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
pub struct AcceptedRoute {
    #[cfg(not(any(test, feature = "test-support")))]
    route: RouteSpec,
    #[cfg(not(any(test, feature = "test-support")))]
    epoch: Uuid,
    // The toolkit's tests hold up observations of their own to an app.
    #[cfg(any(test, feature = "test-support"))]
    pub route: RouteSpec,
    #[cfg(any(test, feature = "test-support"))]
    pub epoch: Uuid,
}
impl AcceptedRoute {
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
    /// An observation the toolkit's tests hold up to a manager.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn for_test(route: RouteSpec, epoch: Uuid) -> Self {
        Self { route, epoch }
    }
}
#[derive(Default)]
struct PrimaryRouteState {
    requested: Option<RouteSpec>,
    accepted: Option<AcceptedRoute>,
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
        self.accepted = Some(AcceptedRoute {
            route,
            epoch: Uuid::new_v4(),
        });
        Ok(())
    }
    fn current(&self, healthy: bool) -> Option<&AcceptedRoute> {
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
    /// Whether document publication had failed at the last
    /// [`Self::take_document_publication_change`].
    publication_failed: bool,
}

impl CollaborationManager {
    /// The tid of the thread serving this connection's broker, for tests
    /// that hold the connection idle and count whether it wakes at all.
    #[cfg(all(test, unix, feature = "broker-transport"))]
    pub fn shovel_thread(&self) -> Option<i32> {
        let Backend::Transport(transport) = &self.backend else {
            return None;
        };
        transport
            .as_any()?
            .downcast_ref::<broker::BrokerTransport>()?
            .shovel_tid()
    }

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
            publication_failed: false,
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
    /// per `(app, workspace)`); use [`Self::for_resource`] for apps that have
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
    pub fn switch_native_resource(
        &mut self,
        _checkout: &collab_types::document::DocumentCheckout,
    ) -> Result<(), String> {
        Err("native document routes need the broker transport".into())
    }

    /// Without the broker transport a window's manager has no connection
    /// of its own to keep: the one for `checkout`.
    #[cfg(not(feature = "broker-transport"))]
    #[must_use]
    pub fn for_reopened(self, checkout: &collab_types::document::DocumentCheckout) -> Self {
        Self::for_resource(checkout)
    }

    pub fn has_native_committed_primary(&self) -> bool {
        match (&self.backend, &self.primary_route.accepted) {
            (Backend::Transport(transport), Some(route)) => {
                transport.committed_document_required(&route.route.channel_id())
            }
            _ => false,
        }
    }
}

/// Compatibility route marker for a launcher-issued app window. Native
/// document routes obtain their separate window binding from original broker
/// ownership; this environment value cannot authorize native private control.
fn continuation_instance_env() -> Option<String> {
    let environment = crate::process::environment();
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
    /// one channel — so two people editing different files in one workspace
    /// don't get handed each other's state. A process with several records
    /// open holds several managers; `tilo_ui::App` swaps its manager on checkout
    /// change.
    pub fn for_resource(checkout: &collab_types::document::DocumentCheckout) -> Self {
        Self::new_scoped(
            checkout.id.route_record(),
            Some(checkout.kind.as_str().to_string()),
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
            publication_failed: false,
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
    /// broker reports it: the workspace is not ready on this device yet, or the
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
    /// never had one (local, outside any workspace) is not offline, and one still
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
    pub fn retarget_join(
        &mut self,
        checkout: &collab_types::document::DocumentCheckout,
    ) -> Result<(), String> {
        let pending = self
            .requested_primary_route()
            .cloned()
            .ok_or("native primary route unavailable")?;
        let record = checkout.id.route_record();
        if pending.document == record {
            return Ok(());
        }
        if !self.send(CollabRequest::Hello {
            app_id: pending.app_id,
            cluster_id: pending.cluster_id,
            record,
            resource_type: Some(checkout.kind.as_str().to_owned()),
            handle: crate::local_identity::own_name(),
        }) {
            return Err("native route queue unavailable".into());
        }
        self.confirm_primary_route_in_background();
        Ok(())
    }

    /// Without a built-in transport there is nothing to discover.
    #[cfg(not(feature = "broker-transport"))]
    fn new_scoped(_record: Option<String>, _resource_type: Option<String>) -> Self {
        Self::disabled()
    }

    /// True when connected to a live broker — used by the
    /// event loop to keep ticking so the stream drains promptly.
    pub fn primary_route(&self) -> Option<&AcceptedRoute> {
        self.primary_route.current(self.is_connected())
    }

    pub fn private_window_instance(&self) -> Option<u64> {
        let observation = self.primary_route()?;
        let Backend::Transport(transport) = &self.backend else {
            return None;
        };
        if transport.committed_document_required(&observation.route.channel_id()) {
            return transport.private_window_binding(&observation.route);
        }
        let instance = observation
            .route
            .document
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
    /// waits for the transport's queue to have workspace, so a burst is
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
        if self.awaiting_primary && !matches!(req, CollabRequest::Hello { .. }) {
            // No acknowledged primary route carries anything but the Hello
            // yet; owners republish once the manager is ready.
            return Err(CollabSendError::Unavailable);
        }
        let primary_channel = self
            .primary_route
            .accepted
            .as_ref()
            .map(|observation| observation.route.channel_id());
        let unpublishable = matches!(req, CollabRequest::Document { .. })
            && match &self.backend {
                Backend::Transport(transport) => {
                    !transport.document_publishable(primary_channel.as_deref())
                }
                Backend::Disabled(_) => false,
            };
        if unpublishable {
            // Committed-document ownership: nothing is published on a route
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
            document: record.clone(),
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

    /// Report a capture on an explicitly retained document route, without
    /// querying or rebasing its directory revision. The route must own the
    /// captured input and published directory revision.
    /// Queueing and admission supply neither execution proof nor publication authority.
    /// Replies arrive as [`CollabEvent::LocalDocumentActionReported`].
    pub fn report_local_document_action_to_route(
        &mut self,
        route: RouteSpec,
        request_id: u64,
        capture: crate::local_document_action::CapturedAction,
    ) -> Result<(), runtime_collab::LocalDocumentActionError> {
        use runtime_collab::LocalDocumentActionError::{Rejected, Uncertain};
        let report = capture.into_report().map_err(Rejected)?;
        // Reads/no-ops retain their empty mutation sets. Never infer a published
        // action from the encoded Rust action type, or rebase a captured revision.
        report.encode().map_err(Rejected)?;
        self.try_send_to_route(
            route,
            CollabRequest::ReportLocalDocumentAction { request_id, report },
        )
        .map_err(|error| Uncertain(error.to_string()))
    }

    /// Take no more invocations: the broker hands over the ones it already
    /// addressed to this connection, then ends its streams, which
    /// [`Self::invocations_stopped`] reports once every one handed over was
    /// taken. What an app asks before it closes, so it answers every
    /// invocation it was handed.
    ///
    /// # Errors
    ///
    /// The transport cannot stop its invocations, or is closed.
    pub fn stop_invocations(&mut self) -> Result<(), TransportError> {
        match &mut self.backend {
            Backend::Disabled(_) => Err(TransportError::Closed),
            Backend::Transport(transport) => transport.stop_invocations(),
        }
    }

    /// Whether the invocations stopped after [`Self::stop_invocations`]:
    /// every one handed over has been taken.
    pub fn invocations_stopped(&self) -> bool {
        match &self.backend {
            Backend::Disabled(_) => true,
            Backend::Transport(transport) => transport.invocations_stopped(),
        }
    }

    /// Take invocations again after [`Self::stop_invocations`]: what earlier
    /// asked this window to stop and answered yes takes it back. Nothing
    /// reaches a connection without a transport either way.
    ///
    /// # Errors
    ///
    /// The transport is closed.
    pub fn resume_invocations(&mut self) -> Result<(), TransportError> {
        match &mut self.backend {
            Backend::Disabled(_) => Ok(()),
            Backend::Transport(transport) => transport.resume_invocations(),
        }
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
            Err(error) => {
                // The offer stays unoffered, so a later publish asks again;
                // until then the app's actions are not offered to anyone.
                tracing::warn!(%error, "collab: action directory offer failed");
            }
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
    /// Join `route`, whose record holds `resource_type`. The kind travels with
    /// the join so a host that must create the record when this device
    /// writes first knows what it holds; joining without it would let the
    /// host answer that the app named no kind.
    pub fn join_route(
        &mut self,
        route: RouteSpec,
        resource_type: collab_types::document::ResourceType,
    ) {
        let _ = self.submit(TransportCommand::Join(route, resource_type));
    }

    /// Install an already-provisioned MLS endpoint for `route`.
    ///
    /// From this point the content the route's record references is sealed
    /// under the group's keys.  The group must be the route's workspace's.  Group
    /// provisioning and durable secret storage intentionally remain outside
    /// the UI process; callers should obtain the group from the workspace
    /// identity service and must reinstall it after a process restart.
    pub fn install_document_encryption(
        &mut self,
        route: RouteSpec,
        group: key_group::KeyGroup,
    ) -> Result<(), String> {
        if group.workspace().to_string() != route.cluster_id.to_string() {
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
    /// This is the same transport check used before document publication. A
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
    /// workspace cannot create) says why here, rather than staying unready.
    #[must_use]
    pub fn route_join_failure(&self, route: &RouteSpec) -> Option<String> {
        match &self.backend {
            Backend::Transport(transport) => transport.route_join_failure(&route.channel_id()),
            Backend::Disabled(_) => None,
        }
    }

    /// Whether `route`'s join is still on its way: it has neither opened,
    /// nor found its record's history to read first, nor failed. The owner
    /// hears each of those as an event.
    #[must_use]
    pub fn document_route_joining(&self, route: &RouteSpec) -> bool {
        match &self.backend {
            Backend::Transport(transport) => transport.document_route_joining(&route.channel_id()),
            Backend::Disabled(_) => false,
        }
    }

    /// Whether changes queued for record `route` were let go since the last
    /// ask, because the route was not open to take them: the owner sends
    /// the record whole once it is.
    pub fn take_unpublished_route(&mut self, route: &RouteSpec) -> bool {
        match &self.backend {
            Backend::Transport(transport) => transport.take_unpublished_route(&route.channel_id()),
            Backend::Disabled(_) => false,
        }
    }

    /// Independently sample the existing transport read checks without admission.
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
        let unpublishable = matches!(req, CollabRequest::Document { .. })
            && match &self.backend {
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
            let resource_type = match &self.backend {
                Backend::Transport(transport) => {
                    transport.joined_resource_type(&route.channel_id())
                }
                Backend::Disabled(_) => None,
            };
            let _ = self.submit(TransportCommand::Join(
                route,
                resource_type.unwrap_or_else(|| collab_types::document::ResourceType::new("")),
            ));
            return Err(CollabSendError::Unavailable);
        }
        // A known committed route may be refreshing. Required original
        // ownership was checked above; retain this mutation in the bounded queue.
        // The worker still waits for this exact route before checking retained
        // ownership and applying the existing certification/commit policy.
        let result = self.submit(TransportCommand::ToRoute(route, req));
        if let Err(error) = &result {
            tracing::warn!(%error, cause = "transport_queue_refusal", "document route handoff refused");
        }
        Ok(result?)
    }

    pub fn document_publication_failed(&self) -> bool {
        matches!(&self.backend, Backend::Transport(transport) if transport.document_publication_failed())
    }

    /// Whether document publication failed, or recovered, since the last
    /// ask: an owner sends its records whole again once on each change. A
    /// refusal stands until a later change goes through, and the same
    /// changes sent again are refused again, so sending them again on every
    /// pass while it stands would only loop.
    pub fn take_document_publication_change(&mut self) -> bool {
        let failed = self.document_publication_failed();
        std::mem::replace(&mut self.publication_failed, failed) != failed
    }

    /// The kind of record `route` was joined with, as the transport holds it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn joined_resource_type(
        &self,
        route: &RouteSpec,
    ) -> Option<collab_types::document::ResourceType> {
        match &self.backend {
            Backend::Disabled(_) => None,
            Backend::Transport(transport) => transport.joined_resource_type(&route.channel_id()),
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
                && snapshot.workspace == route.cluster_id.to_string()
                && snapshot.document == route.document.as_deref().unwrap_or("")
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
/// undo can tell members apart (see
/// the toolkit's `State::undo_as`): the id must be **stable** across a
/// document's life and **distinct** between people, or one person's
/// Ctrl+Z reverts another's work.
///
/// The rungs, in the order a process knows who it is:
///
/// 1. The local identity this process resolves: its installed source (the
///    session's own identity records, for its in-process apps), else the
///    workspace identity service its launcher bound it to.
/// 2. The OS account name, then a constant.
///
/// Not memoised — see `auto_hello_env_uncached`: a process hosting
/// more than one identity must not be frozen into the first one.
pub fn local_user_id() -> collab_types::MemberId {
    let id = resolve_local_user_id();
    // An override that names no person (a prefixed or empty id) lands on the
    // last rung the ladder has: the local account, not a subject no roster
    // knows.
    let person = tilo_failure::ignored!(
        "an id that names no person falls back to the local account",
        collab_types::PersonId::parse(&id)
            .unwrap_or_else(|_| collab_types::PersonId::from(local_account()))
    );
    collab_types::MemberId::Person(person)
}

/// The OS account this process runs as, or `local` on a machine without one.
fn local_account() -> String {
    crate::process::environment()
        .identity
        .account_name
        .clone()
        .unwrap_or_else(|| "local".to_owned())
}

fn resolve_local_user_id() -> String {
    if let Some(id) = crate::local_identity::load() {
        return person_id_from_key(id.group_public_key);
    }
    crate::process::environment()
        .identity
        .account_name
        .clone()
        .unwrap_or_else(|| "local".to_string())
}

/// A public key rendered as a person, in the one canonical form — so this
/// string and the compositor's string for the same key are the same string.
fn person_id_from_key(key: [u8; 32]) -> String {
    hex::encode(key)
}

/// The application's authenticated durable route (falling back to its signed
/// package id, then `QUICK_APP_ID` for dev). Read from the same place the
/// channel key is built, so what an app publishes can't disagree with its
/// connection.
pub fn local_app_id() -> String {
    auto_hello_env().0
}

/// The workspace this process collaborates in. Exposed so presence scopes
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
    // workspace, since env is the routing truth (a property of the launch, not
    // the process image) and can differ per manager in one process (every
    // collaboration test does this).
    static FALLBACK_APP_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    let environment = crate::process::environment();
    let identity = &environment.identity;
    let app_id = identity
        .app_id
        .clone()
        .or_else(|| identity.development_app_id.clone())
        .unwrap_or_else(|| {
            FALLBACK_APP_ID
                .get_or_init(|| Uuid::new_v4().to_string())
                .clone()
        });

    // No workspace named is the nil workspace; one named that does not parse
    // is the same, and logged, since the app then reaches no workspace.
    let cluster_id = identity
        .workspace()
        .map(Option::unwrap_or_default)
        .unwrap_or_else(|error| {
            tracing::warn!(workspace = ?identity.workspace_id, %error, "collab: launch workspace id does not parse");
            Uuid::nil()
        });

    (app_id, cluster_id)
}

#[cfg(test)]
mod primary_route_state_tests {
    use super::{PrimaryRouteState, RouteSpec};
    #[test]
    fn a_primary_observation_requires_the_latest_success_and_current_health() {
        let a = RouteSpec::document("a", uuid::Uuid::nil(), "record-a");
        let b = RouteSpec::document("b", uuid::Uuid::nil(), "record-b");
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
            ownership: None,
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
                    label: Some("Private work".into()),
                    kind: "issue".into(),
                    declared_id: None,
                    session: None,
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

    tilo_test_env::scenario! {
        /// Reporting a captured local document action, each case on a manager of its own.
        fn a_local_document_action_is_reported_exactly_or_refused;

        phase exact_capture_reports_on_shared_and_local_routes_without_mutations;
        {
            let reports = Reports::default();
            let mut manager =
                CollaborationManager::with_transport(Box::new(CaptureTransport(reports.clone())));
            let captured = capture();
            let shared = RouteSpec::document("app", uuid::Uuid::from_bytes([3; 16]), "shared");
            let local = RouteSpec::document("app", uuid::Uuid::from_bytes([4; 16]), "local");
            for (request_id, route) in [(11, shared.clone()), (12, local.clone())] {
                manager
                    .report_local_document_action_to_route(route, request_id, captured.clone())
                    .unwrap();
            }
            let reports = reports.lock_recover();
            assert_eq!(reports.len(), 2);
            for ((route, request_id, bytes), (expected_route, expected_id)) in
                reports.iter().zip([(Some(shared), 11), (Some(local), 12)])
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

        phase unavailable_transport_and_missing_published_invocation_refuse;
        {
            let route = RouteSpec::document("app", uuid::Uuid::from_bytes([3; 16]), "shared");
            assert!(
                CollaborationManager::disabled()
                    .report_local_document_action_to_route(route.clone(), 1, capture())
                    .is_err()
            );
            let mut missing = capture();
            missing.published = None;
            let reports = Reports::default();
            let mut manager =
                CollaborationManager::with_transport(Box::new(CaptureTransport(reports.clone())));
            assert!(
                manager
                    .report_local_document_action_to_route(route, 2, missing)
                    .is_err()
            );
            assert!(reports.lock_recover().is_empty());
        }
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
    tilo_test_env::scenario! {
        /// A manager over a peer that drops every change, each case on a manager of its own.
        fn a_peer_that_fails_to_take_a_change_disconnects_the_manager;

        phase peer_enqueue_failure_disconnects_the_real_manager;
        {
            let mut manager = CollaborationManager::with_peer_transport(ClosedPeer);
            assert!(manager.is_connected());
            manager.send(CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Snapshot(vec![1]),
                commit: None,
            });
            assert!(!manager.is_connected());
            assert!(manager.poll().is_empty());
        }

        phase only_a_document_that_had_a_service_reports_sync_lost;
        {
            assert!(!CollaborationManager::disabled().sync_lost());
            let mut manager = CollaborationManager::with_peer_transport(ClosedPeer);
            assert!(!manager.sync_lost());
            manager.send(CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Snapshot(vec![1]),
                commit: None,
            });
            assert!(manager.sync_lost());
        }
    }
}

/// Original authority retained across display background work.
pub use runtime_collab::DisplayOrigin;

/// A refusal stands until a later change goes through: the owner sends its
/// records whole again once when publication fails and once when it
/// recovers, never on every pass in between.
#[cfg(test)]
mod publication_change_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Refusing(Arc<AtomicBool>);
    impl CollabTransport for Refusing {
        fn status(&self) -> TransportStatus {
            TransportStatus::Ready
        }
        fn submit(&mut self, _: TransportCommand) -> Result<(), TransportError> {
            Ok(())
        }
        fn drain(&mut self, _: &mut Vec<CollabEvent>) {}
        fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
            None
        }
        fn take_action_directory_requests(&mut self, _: &mut Vec<ActionDirectoryRequest>) {}
        fn document_publication_failed(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
    }

    #[test]
    fn a_lasting_refusal_is_one_change_and_its_end_another() {
        let failed = Arc::new(AtomicBool::new(false));
        let mut manager =
            CollaborationManager::with_transport(Box::new(Refusing(Arc::clone(&failed))));
        assert!(!manager.take_document_publication_change());
        failed.store(true, Ordering::Release);
        assert!(manager.take_document_publication_change(), "it failed");
        for _ in 0..3 {
            assert!(
                !manager.take_document_publication_change(),
                "the same refusal is no new change"
            );
        }
        failed.store(false, Ordering::Release);
        assert!(manager.take_document_publication_change(), "it recovered");
        assert!(!manager.take_document_publication_change());
    }
}

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
            document: None,
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
            resource_type: Some("notes.notebook".into()),
            handle: None,
        }));
        manager.confirm_primary_route_in_background();
        manager
    }

    tilo_test_env::scenario! {
        /// A manager joining in the background, each case on a manager and a transport of its own.
        fn a_late_primary_route_is_joined_held_up_or_refused_as_the_broker_says;

        phase a_late_primary_route_leaves_the_app_up_and_joins_when_it_answers;
        {
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
                    .map(|observation| observation.route().clone()),
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

        phase the_join_says_what_the_broker_says_holds_it_up;
        {
            let transport = Late::default();
            let mut manager = joining(&transport);
            let waiting = JoinHold::WorkspaceWaiting {
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

        phase a_refused_route_still_takes_the_app_offline;
        {
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
}
