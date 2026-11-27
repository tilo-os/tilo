//! The Tilo collaboration broker's host side.
//!
//! Answers the collaboration wire (`runtime_collab::wire`) on the shell's
//! collaboration socket.
//!
//! A join uses the caller's channel id verbatim as the routing key.
//! Document traffic has one path: a certified signed op published into the
//! channel. A history read answers from the in-memory op log; an
//! exhausted-but-empty page triggers no fallback push, callers repeat the
//! read instead.

#![allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach the module through the parent glob"
)]
mod directory;
mod join_identity;
mod socket;
pub(super) use directory::*;
mod authority;
mod catalogue;
mod routing;
mod shown_actions;
use tilo_actions::remote::argument_digest;
mod invocation;
use authority::*;
#[cfg(test)]
pub(crate) use invocation::answered;
use invocation::*;
pub(crate) use invocation::{InvocationCompletion, InvocationEnvelope, InvocationJob};
mod confirmation;
mod connection_calls;
use confirmation::*;
mod journal_commit;
use journal_commit::*;
use routing::*;
mod app_asks;
mod session;
mod shell;
mod shell_served;
use session::*;
use socket::*;
mod lifecycle;
use crate::BrokerError;
use join_identity::*;
#[allow(
    unused_imports,
    reason = "the broker's tests and remaining code reach the thread lifecycle through this glob"
)]
use lifecycle::*;
pub(crate) use lifecycle::{BrokerWiring, spawn_configured};
mod execution_activity;
#[cfg(test)]
mod execution_activity_tests;
mod external_moderation;
mod local_document_action;
mod observed_launch;
mod presses;
mod subscriptions;
mod surface_publication;
pub use observed_launch::AppOpen;
mod offload_api;
use execution_activity::{ActivityBus, ActivityTarget, ExecutionScope};
pub use execution_activity::{ExecutionActivity, ExecutionStatus};
#[cfg(test)]
mod app_settings_tests;
#[cfg(test)]
mod authority_sample_tests;
mod canonical_route;
use tilo_workspace_service::workspaces::record_reads::{AppReach, app_reach};
mod committed_document;
mod content_put;
mod resource_read;
pub use content_put::CONTENT_PUT_REFUSED;
#[cfg(test)]
mod directory_disclosure_tests;
mod directory_pagination;
mod durable_local_invocation;
mod staged_admission;
pub use staged_admission::StagedJob;
use staged_admission::{Admitted, DefiniteOutcome, Delivery};
mod file_clipboard;
// The workspace's folder as its listing offers it.
#[cfg(feature = "files")]
mod files_listing;
mod host_workers;
mod invocations;
mod invoker;
mod item_use;
mod kept_document;
mod launcher_handoff;
use launcher_handoff::{LauncherHandoff, credentialed_peer, read_launcher_handoff};
mod app_process;
mod media;
mod native_checkout;
#[cfg(test)]
#[path = "collab_app_broker/native_effect_recovery_tests.rs"]
mod native_effect_recovery_tests;
#[cfg(test)]
mod policy_turn_tests;
#[cfg(target_os = "linux")]
mod process_binding;
mod published;
mod published_invocation;
mod record_changes;
mod resolved_invocation;
mod route_holds;
/// A desktop session's app socket, which only a Unix session has.
#[cfg(unix)]
pub mod seat;
#[cfg(test)]
mod shell_tests;
mod shortcut_approval;
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "some wire helpers are reached only from the broker's tests"
    )
)]
mod wire;
pub use crate::services::LaunchScope;
pub use committed_document::{
    AwaitingDocument, ChainHeadAnswer, DocumentBytes, HeldRevision, NativeDocumentRead,
    NativeDocumentReadContext, NativeDocumentReads, ReadAnswering, ResourceRead,
};
pub(crate) use tilo_invocation_journal::{DurableInvocation, admission_request_digest};
use tilo_invocation_journal::{InvocationJournal, InvocationKey, InvocationOwner};

pub use action_provider::delivering::LaunchOrigin;
pub use media::{
    AudioOutputConnection, AudioOutputFuture, AudioOutputServices, CallMediaConnection,
    CallMediaFuture, CallMediaServices, MediaRequestContext,
};
#[cfg(test)]
pub(crate) use published_invocation::OriginalLookup;
use published_invocation::{InvocationCaller, OriginalInvocationData};
pub use published_invocation::{InvocationRequest, OriginalInvocation};

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use tilo_lock::MutexRecover as _;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;

use collab_types::handoff::HandoffReport;
use collab_types::presence::PresenceKey;
use collab_types::presence::PresencePayload;
use runtime_collab::event::CollabEvent;
use runtime_collab::wire::ContextUpdate;
use tilo_action_policy::{ActionKind, Answer, TreePath, roles};
#[cfg(test)]
use tilo_desktop_model::Layout;

use crate::collab_broker::{BrokerChannels, CrdtRoute, FanoutState};
use crate::collab_outbound::PeerPublisher;
use crate::services::{ProcessServices, WorkspaceAuthorityHandle as WorkspaceTreeHandle};
use collab_types::Role as CapRole;
#[cfg(test)]
use tilo_action_policy::Grantee;

mod agent_cert;
use agent_cert::admit_listed_agent;

/// The node of opening the application `app` in `workspace`: launching it is
/// an action of the app itself.
pub(crate) fn app_node(workspace: &tilo_desktop_model::Workspace, app: &str) -> TreePath {
    workspace.policy.node().app(app)
}

/// What an invocation of `descriptor` would run with, as the person reviews it: the draft the
/// sheet shows them, `None` when it takes no field.
fn rendered_draft(
    descriptor: &tilo_actions::ActionDescriptor,
    argument: &tilo_actions::remote::InvokeArgument,
) -> Option<tilo_actions::Draft> {
    let tilo_actions::remote::InvokeArgument::Json(arguments) = argument else {
        return None;
    };
    Some(tilo_actions::Draft::render(descriptor, arguments))
        .filter(|draft| !draft.fields.is_empty())
}

/// One inert operation published by a live application connection: the one
/// exported-action shape `runtime-collab` decodes off the wire, typed.
/// The broker's actor, its published actions and their admission: every host's.
pub use tilo_broker_core::actor::{BrokerActor, person_id_for_root};
pub use tilo_broker_core::published::{PublishedAction, PublishedInput, PublishedSurface};
pub(crate) use tilo_broker_core::published::{
    validate_published_surface, validate_resource_view_descriptors,
};

#[derive(Clone)]
struct PendingInvocation {
    /// Who it was admitted for, kept so an edit its effect submits is
    /// admitted for them too.
    caller: Option<ContinuingCaller>,
    /// How far it was admitted to reach: what its effect asks for goes no
    /// further without being asked for.
    reach: tilo_action_policy::Reach,
    activity: Option<ActivityTarget>,
    owner: InvocationOwner,
    original: Option<Arc<OriginalInvocationData>>,
    policy_time: Option<crate::policy_clock::PolicyTime>,
    journal_key: Option<InvocationKey>,
    peer_id: u64,
    tx: std::sync::mpsc::Sender<InvocationCompletion>,
    update: tilo_actions::InvocationUpdate,
    prepared_effect: Arc<Mutex<Option<PreparedEffectCommit>>>,
    last_sequence: u64,
    actor: tilo_member_types::Actor,
    cancellable: bool,
    retryable_on_disconnect: bool,
    execution: tilo_actions::ExecutionMode,
    result: tilo_actions::ResultSpec,
    operation_kind: tilo_actions::OperationKind,
    cluster_id: String,
    context: tilo_actions::ActionContext,
    /// Admission/progress heartbeat. A silent application must not leave a
    /// broker-owned job in `Accepted` forever.
    last_activity: std::time::Instant,
    /// Starts the shorter acknowledgement deadline for cooperative cancel.
    cancellation_requested_at: Option<std::time::Instant>,
    /// A retained continuation must observe cancellation after registry removal.
    cancellation_fence: Arc<AtomicBool>,
}

/// A host step that declared itself cancellable and has not ended.
struct HostStepStop {
    /// The flag that stops it when its caller cancels it.
    stop: Arc<AtomicBool>,
    /// Who asked for it: only they, or whoever may cancel what they run, stop it.
    owner: InvocationOwner,
    /// The step itself, by its full key.
    key: JobKey,
    /// The invocation it is a step of, as the dispatch delivered that invocation to the app that
    /// made the call: that invocation waits on it, and its cancel stops it.
    continues: Option<JobKey>,
}

/// A job by its full key: the app it is an action of, the workspace it runs in and its
/// invocation id. A caller chooses the id, so a job is never found by the id alone.
#[derive(Clone, PartialEq, Eq, Hash)]
struct JobKey {
    app: String,
    workspace: String,
    invocation: tilo_actions::InvocationId,
}

impl JobKey {
    /// The invocation `serving` delivered to its app's verified connection, never what a call
    /// named.
    fn of(serving: &dyn tilo_actions::remote::Serving) -> Self {
        Self {
            app: serving.app().to_owned(),
            workspace: serving.workspace().to_owned(),
            invocation: serving.invocation(),
        }
    }

    /// Whether `job` is the job this key names.
    fn is(&self, job: &PendingInvocation) -> bool {
        job.key().as_ref() == Some(self)
    }
}

impl PendingInvocation {
    fn completion_owner(&self) -> InvocationOwner {
        self.owner.clone()
    }

    /// Its full key: the app it was delivered to, its workspace and its invocation id.
    fn key(&self) -> Option<JobKey> {
        let original = self.original.as_ref()?;
        Some(JobKey {
            app: original.acts_on().0.to_owned(),
            workspace: self.cluster_id.clone(),
            invocation: self.update.invocation(),
        })
    }
}

// Remote-action provider: the broker as a `tilo_actions::remote::ActionProvider`.
pub(super) mod action_provider;
mod actions_registry;
mod admission_memo;
mod agent_connection;
#[cfg(test)]
#[path = "collab_app_broker/agent_tests.rs"]
mod agent_tests;
mod app_consent;
mod app_open_api;
mod approval;
#[cfg(test)]
pub(crate) use approval::person_approved;
#[cfg(test)]
mod cancel_tests;
mod host_action_admission;
mod invocation_update;
pub use jobs::{JobEffects, JobHandlers};
mod jobs;
mod launchable;
mod live_views;
mod local_action_provider;
mod person_ask;
mod provided;
mod remaining_workspace_actions;
#[cfg(all(test, feature = "agents"))]
mod web_search_tests;
mod window_provider;
mod windowless_instances;
use approval::requires_store_approval;
mod workspace_open;
pub use local_action_provider::{CertifiedActionLifecycle, CertifiedPersonContext};

/// The host the person's other devices reach this device's actions through:
/// a request is admitted for the caller its own device's signature named,
/// under no connection's ownership and no step of a run's, so what it may do is
/// decided by that caller alone.
pub(crate) fn device_host(
    state: Arc<AppBrokerState>,
) -> Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> {
    Arc::new(action_provider::BrokerApi::new(state))
}

pub use launchable::launchable_provider;
#[path = "policy_context.rs"]
mod policy_context;

/// A unique actual private connection, independent of device role. A native
/// primary device and a delegated Cloud host retain their existing admission paths.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PrivateWorkerRegistration {
    identity: uuid::Uuid,
    route: CrdtRoute,
    certificate: tilo_user_identity::DeviceCert,
}
/// Host-owned admission for one private SDK descriptor. Never serialized on the wire.
#[derive(Clone)]
struct WorkerBinding {
    registration: Option<PrivateWorkerRegistration>,
    route: CrdtRoute,
    /// The certificate the host admitted the worker under: what its calls act as.
    certificate: tilo_user_identity::DeviceCert,
}
struct PendingWorker {
    stream: tilo_socket::Stream,
    binding: WorkerBinding,
}
/// What the caller brings to one invocation: the argument, the actor it runs
/// as, the one-use confirmation token it presents and its ambient context.
/// Admission checks every field; none of them grants authority by itself.
struct InvocationInputs<'a> {
    /// Exact remote request before resolving a provider alias for delivery.
    original_request: Option<&'a tilo_actions::remote::InvokeRequest>,
    /// The caller's lifecycle identity, independent of execution authority.
    requested: Option<tilo_actions::InvocationId>,
    /// Who the invocation is admitted for, when the API knows them as a
    /// caller: kept with the running invocation, so an edit its effect
    /// submits is admitted for them too.
    caller: Option<&'a tilo_actions::remote::Caller>,
    execution: Option<ExecutionScope>,
    argument: Option<&'a str>,
    actor: &'a BrokerActor,
    confirmation_token: Option<&'a str>,
    context: Option<tilo_actions::ActionContext>,
}
/// Who an admission is held for and where it came from: the directory revision the target was admitted at (the
/// one the caller chose it at or, when it named none, the one its owner
/// resolved it against; `None` only for an action no directory resolved), a
/// private owner and original caller, the original policy time a retained
/// invocation keeps, and a task-owned durable identity. Each call site names
/// every field, so no ownership input is filled implicitly.
struct AdmissionProvenance<'a> {
    revision: Option<&'a str>,
    origin: Option<InvocationCaller>,
    retained_time: Option<&'a crate::policy_clock::PolicyTime>,
}
/// What an admission's intent records besides the surface it resolves to:
/// who asks for it and the exact request the caller asked for.
/// Built once per admission, so a repeated invocation is compared with what
/// was recorded.
#[derive(Clone, Copy)]
struct IntentInputs<'a> {
    peer_id: u64,
    actor: &'a BrokerActor,
    argument: Option<&'a str>,
    revision: Option<&'a str>,
    original_request: Option<&'a tilo_actions::remote::InvokeRequest>,
    local_report: Option<&'a runtime_collab::LocalDocumentActionReport>,
    typed_local_arguments: Option<&'a serde_json::Map<String, serde_json::Value>>,
    receipt_context: &'a tilo_actions::ActionContext,
}
/// Shared typed broker state, attached to the transport-neutral fanout core.
pub struct AppBrokerState {
    /// Serializes the cloneable shell facade against final shutdown. Broker
    /// `LocalSet` tasks are dropped before shutdown takes the write side.
    lifecycle: RwLock<()>,
    accepting: AtomicBool,
    stopped: AtomicBool,
    /// Keyed by the record's channel key; inserted when a connection first names the record.
    channels: Mutex<HashMap<String, ChannelSubscribers>>,
    /// The held committed-read waits (`Await`) of each channel, by
    /// `channel_id`: a write fanned out on a channel's route changes what
    /// its read answers, and wakes them ([`Self::wake_committed_reads`]).
    committed_waits: Mutex<HashMap<String, Vec<committed_document::LandedWake>>>,
    /// Each window's shown actions, decided again on every policy change
    /// ([`shown_actions`]).
    shown_actions: Mutex<Vec<shown_actions::ShownSubscription>>,
    /// `Weak` so broker teardown isn't blocked by an outliving connection.
    fanout: Weak<FanoutState>,
    /// This broker while it runs: the registration an app's calls reach it through holds this,
    /// never the broker.
    itself: Weak<AppBrokerState>,
    /// Durable validation, audit, redaction, and scrollback state.
    broker_channels: Arc<Mutex<BrokerChannels>>,
    /// A publisher with no network behind it ([`PeerPublisher::disabled`]) is legitimate
    /// (unit tests, a shell started without peer sync).
    publisher: Arc<PeerPublisher>,
    durable_owner_required: bool,
    committed_read_budget: Arc<committed_document::Budget>,
    committed_read_id: AtomicU64,
    owner_commit_failure: Mutex<Option<String>>,
    owner_commit_order: AsyncMutex<()>,
    /// The shell's one replicated workspace owner/capability authority.
    /// `None` exists only for transitional test/diagnostic brokers; a
    /// certified join fails closed when they have no authority to consult.
    membership: Option<WorkspaceTreeHandle>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    certificate_authority:
        std::sync::OnceLock<Arc<dyn super::certificate_authority::CertificateAuthority>>,
    /// What each live connection publishes, and the invocation stream of each that takes
    /// invocations.
    publishers: tilo_broker_core::delivery::Publishers<InvocationEnvelope>,
    /// Each connection's surface as `publishActionSurface` deltas built it:
    /// the shapes and bindings the next delta applies to.
    surface_tables: Mutex<HashMap<u64, runtime_collab::SurfaceTable>>,
    /// Last published surface per app, workspace and record, kept across
    /// restarts. Catalogue state, never an executable capability by itself.
    launchable: launchable::LaunchableCatalogue,
    /// Workspace `open`s waiting for the instance each started to publish.
    workspace_opens: workspace_open::WorkspaceOpens,
    /// The window instance the shell launched each connected app for, by
    /// peer id: which tab's window the records it holds are shown in.
    window_instances: Mutex<HashMap<u64, u64>>,
    /// The saves the shell asked of windows' apps that have not answered, by the window's route
    /// (its channel key) and the request's number: the connection asked, and where its answer
    /// goes. The connection's going away drops what it was asked, which the asker hears as no
    /// answer ([`Self::ask_save`]).
    saves: Mutex<HashMap<(String, u64), SaveAnswer>>,
    /// The person's own presses, as the compositor attested them over the shell link: what a
    /// call an app's window makes proves it runs as the person
    /// ([`presses::Presses`]).
    presses: presses::Presses,
    /// Pull-only, revisioned directories owned by currently connected apps.
    /// This intentionally has no durable/archive counterpart: retaining a
    /// capability after disconnect would make a stale live-only row look
    /// executable.
    /// The broker's own end of the action directory bridge its askers reach,
    /// and the queue that reaches it: the fanout's live broker first, this
    /// one while the fanout holds none.
    action_directory_bridge: ActionDirectoryRequestQueue,
    action_directory_requests: ActionDirectoryRequests,
    action_directory_roots: Mutex<HashMap<u64, ActionDirectoryRoot>>,
    canonical_directories: Mutex<canonical_route::CanonicalDirectories>,
    native_checkouts: Mutex<HashMap<u64, native_checkout::NativeCheckout>>,
    action_directory_root_snapshot: Mutex<ActionDirectoryRootSnapshot>,
    pending_invocations: Mutex<HashMap<u64, PendingInvocation>>,
    /// The invocations each route was delivered and has answered, by peer, each with who invoked
    /// it: an app's save of what it wrote for one may reach the store after it answered, and is
    /// that invoker's change. An invocation leaves this once the route's changes for it have
    /// landed ([`Self::invocation_changes_landed`]), or with the route itself.
    served_invocations:
        Mutex<HashMap<u64, Vec<(tilo_actions::InvocationId, tilo_actions::Acting)>>>,
    external_moderation_jobs: Mutex<external_moderation::Registry>,
    /// Remote-action invocations admitted through the action host, keyed by
    /// caller and invocation id so a retried id is idempotent.
    remote_invocations: Mutex<tilo_action_calls::InvocationLog<u64>>,
    /// The calls each app connection made inside an invocation delivered to it, by connection
    /// and call: whose they are, so the connection follows and cancels them as that caller.
    child_calls: Mutex<HashMap<(u64, tilo_actions::InvocationId), tilo_actions::remote::Caller>>,
    /// The calls this device sent to the device that runs their app's connection, by their full
    /// key: who made each, and that device's actions, which follow it there
    /// ([`AppBrokerState::forwarded_call`]).
    forwarded_calls: Mutex<HashMap<JobKey, connection_calls::ForwardedCall>>,
    /// Shortcut runs the API finished, until history records them.
    ended_invocations: crate::automation::BrokerInvocations,
    /// What each application declared it may do on the action api under
    /// its own identity.
    app_action_access: RwLock<crate::app_actions::AppActionAccessResolver>,
    app_resource_kinds: RwLock<Option<crate::app_actions::AppResourceKindsResolver>>,
    /// Bumped whenever a pending or completed invocation changes, so hosts
    /// can refresh retained invocation projections exactly when needed and
    /// a waiter on a running invocation wakes when it may have moved.
    invocation_generation: tokio::sync::watch::Sender<u64>,
    /// Bumped whenever what the windows here hold changes — a window's app
    /// subscribes or lets go of a record, or the shell says which window a
    /// connection is — so the device's daemon publishes the change to the
    /// desktop at once ([`Self::window_changes`]).
    window_generation: tokio::sync::watch::Sender<u64>,
    execution_activity: ActivityBus,
    /// Admission serialization covers policy, one-use confirmation, the durable
    /// barrier and the single application delivery. Never held across awaits.
    admission_lock: Mutex<()>,
    journal: Mutex<Result<Option<InvocationJournal>, String>>,
    /// Invocations whose definite outcome is being written: expiry and a
    /// disconnect leave them alone until it is durable and heard.
    committing_invocations: Mutex<HashSet<u64>>,
    local_report_receipts: Mutex<local_document_action::ReceiptCache>,
    stable_invocation_subscribers: Mutex<std::collections::HashSet<u64>>,
    completed_invocations: Mutex<HashMap<u64, (InvocationOwner, tilo_actions::InvocationUpdate)>>,
    /// Host steps whose result stays current, while they are open.
    live_views: live_views::LiveViews,
    /// The host steps that declared themselves cancellable and have not ended, by job.
    host_step_stops: Mutex<HashMap<u64, HostStepStop>>,
    /// Jobs whose action's contract declares untrusted content, so every
    /// observation of them says so. They share the completed invocation
    /// cache's bounded lifetime.
    untrusted_jobs: Mutex<std::collections::HashSet<u64>>,
    confirmation_grants: Mutex<HashMap<String, ConfirmationGrant>>,
    cancellation_requests: Mutex<std::collections::HashSet<u64>>,
    /// The device's record of the invocations it was asked for, where its embedder
    /// shares it: what `tilo.invocations` reads and lists here.
    invocations: std::sync::OnceLock<Arc<tilo_invocations::Invocations>>,
    /// The device's one record reader, where its embedder serves it: what an app or agent here
    /// reads a record outside its app through ([`Self::serve_records`]).
    records: std::sync::OnceLock<Arc<dyn tilo_actions::remote::ResourceReads>>,
    /// Where a read the person is asked about waits for them, where its embedder serves it: the
    /// device's dispatch, which keeps it as its asked record ([`Self::serve_asked_reads`]).
    asked_reads: std::sync::OnceLock<Arc<dyn tilo_actions::remote::asked::AskedReads>>,
    /// The host that placed this device's window, where a host placed it: the only device whose
    /// word answers for the person at it ([`Self::placed_by`]).
    placement: std::sync::OnceLock<tilo_action_services::browser::Placement>,
    /// The apps' own calls that wait for their person's yes in an asked record
    /// ([`app_asks`]), once one was kept.
    app_asks: std::sync::OnceLock<Arc<app_asks::AppAsks>>,
    invocation_id: AtomicU64,
    services: ProcessServices,
    workers: Mutex<Vec<PendingWorker>>,
    /// Wakes the broker loop to adopt a newly admitted local worker at once
    /// rather than on its next housekeeping tick.
    worker_admitted: tokio::sync::Notify,
    /// Queued directory work, a test callback, or a new invocation. The
    /// accept loop parks until one of these; a timer used to notice them.
    local_turn: std::sync::Arc<tokio::sync::Notify>,
    /// Admitted offloadable invocations waiting for the turn to ask
    /// [`ProcessServices::action_offload`] where they run.
    offloads: Mutex<Vec<offload_api::QueuedOffload>>,
    /// The worker connections' admitted joins, by connection and route
    /// ([`host_workers`]); never reconstructed from surfaces.
    host_workers: Mutex<host_workers::HostWorkers>,
    /// The connection each further route hold belongs to ([`route_holds`]).
    route_holders: Mutex<route_holds::RouteHolders>,
    /// How often the broker thread unwound and started again.
    thread_restarts: AtomicU64,
    /// Work another thread hands the broker's turn (see [`TurnWork`]).
    turn_work: std::sync::Arc<TurnWork>,
    /// Told once this state is dropped and its journal's lock let go.
    /// The invocations that hold a live engine job on this device, by invocation id: never
    /// expired as idle while they do, and cancelling one cancels its jobs.
    job_invocations: Mutex<std::collections::HashSet<String>>,
    /// What cancels a held invocation's jobs, once the daemon runs them.
    job_canceller: Mutex<Option<JobCanceller>>,
    #[cfg(any(test, feature = "test-support"))]
    gone: BrokerGone,
}

/// A save the shell asked of a window's app, still unanswered: the connection asked, and where
/// its answer goes.
type SaveAnswer = (u64, tilo_actions::remote::Completer<HandoffReport>);

/// What cancels a held invocation's jobs, once the daemon runs them.
type JobCanceller = Arc<dyn Fn(&tilo_actions::InvocationId) + Send + Sync>;

/// Work another thread handed the broker's turn, run with the state.
type TurnTask = Box<dyn FnOnce(&AppBrokerState) + Send>;

/// Work a thread hands the broker's turn instead of doing it there.
///
/// The journal resolves a write on a thread of its own; a callback that took
/// the broker there would drop the journal on it, and a writer cannot wait for
/// itself, so the store's lock would outlive the journal. Work that needs the
/// broker is handed here, with the state the turn owns and gives it.
pub(super) struct TurnWork {
    handed: Mutex<Vec<TurnTask>>,
    /// The turn that runs what is handed: the broker loop parks on it.
    turn: std::sync::Arc<tokio::sync::Notify>,
}

impl TurnWork {
    /// Queue `work` for the turn, which runs it with the state.
    pub(super) fn hand(&self, work: TurnTask) {
        self.handed.lock_recover().push(work);
        self.turn.notify_one();
    }
}

/// Told when a broker is dropped, after its journal let the store's lock go.
///
/// A state's strong count reaches zero before its fields are dropped, so a
/// count does not say the store is free: the journal's writer may still be
/// finishing the last write it was handed. Whoever opens the store again
/// waits here.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Default)]
pub struct BrokerGone(std::sync::Arc<BrokerGoneTold>);

#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct BrokerGoneTold {
    told: std::sync::atomic::AtomicBool,
    waiters: tokio::sync::Notify,
}

#[cfg(any(test, feature = "test-support"))]
impl BrokerGone {
    fn tell(&self) {
        self.0.told.store(true, Ordering::Release);
        self.0.waiters.notify_waiters();
    }

    /// Wait for the broker to be dropped and its store's lock let go.
    pub async fn wait(&self) {
        loop {
            let told = self.0.waiters.notified();
            tokio::pin!(told);
            told.as_mut().enable();
            if self.0.told.load(Ordering::Acquire) {
                return;
            }
            told.await;
        }
    }
}

/// A broker a caller never stopped is closed by its drop: the journal goes,
/// and its writer with it, so the store's lock is let go before [`BrokerGone`]
/// is told. The other fields drop after that tell.
#[cfg(any(test, feature = "test-support"))]
impl Drop for AppBrokerState {
    fn drop(&mut self) {
        self.let_go_of_journal();
        self.gone.tell();
    }
}

impl AppBrokerState {
    /// The device's sound output as an app's own connection to the daemon reaches it: the
    /// service that plays it, and the context of the app process the launcher placed the
    /// connection for, which `current` checks again before every command. `None` when this
    /// session serves no app sound.
    #[must_use]
    pub fn audio_output(
        &self,
        peer_id: u64,
        app: &str,
        workspace: &str,
        current: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Option<AudioOutputConnection> {
        let service = self.services.audio_output.clone()?;
        let context = MediaRequestContext::daemon_line(
            peer_id,
            app.to_owned(),
            workspace.to_owned(),
            current,
        );
        Some(AudioOutputConnection::new(service, context))
    }

    /// The device's call service as an app's own connection to the daemon reaches it: the
    /// service that holds the microphone, camera and screen, and the context of the app process
    /// the launcher placed the connection for, which `current` checks again before every
    /// command. `None` when this device serves no calls.
    #[must_use]
    pub fn call_media(
        &self,
        peer_id: u64,
        app: &str,
        workspace: &str,
        current: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Option<CallMediaConnection> {
        let service = self.services.call_media.clone()?;
        let context = MediaRequestContext::daemon_line(
            peer_id,
            app.to_owned(),
            workspace.to_owned(),
            current,
        );
        Some(CallMediaConnection::new(service, context))
    }

    /// Drop the journal now: its writer finishes what it was handed and the
    /// store's lock is released before this returns.
    fn let_go_of_journal(&self) {
        let journal = {
            let mut slot = match self.journal.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::replace(&mut *slot, Ok(None))
        };
        drop(journal);
    }

    /// A handle told once this broker is dropped and its journal's lock let
    /// go, whichever thread drops it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn gone(&self) -> BrokerGone {
        self.gone.clone()
    }

    /// `peer_id`'s invocation stream while it takes invocations: opened, not
    /// ended by its app, and still read.
    fn invocation_stream(&self, peer_id: u64) -> Option<mpsc::UnboundedSender<InvocationEnvelope>> {
        tilo_broker_core::delivery::open_stream(&*self.publishers.streams.lock().ok()?, peer_id)
            .cloned()
    }

    /// The invocations delivered to `peer_id` that it has not finished, and those it has
    /// answered whose changes may still land, each with who the workspace service admitted it
    /// for: what a write it makes for one is credited to.
    fn delivered_to(
        &self,
        peer_id: u64,
    ) -> Vec<(tilo_actions::InvocationId, tilo_actions::Acting)> {
        let mut delivered: Vec<(tilo_actions::InvocationId, tilo_actions::Acting)> = {
            let pending = match self.pending_invocations.lock() {
                Ok(pending) => pending,
                Err(poisoned) => poisoned.into_inner(),
            };
            pending
                .values()
                .filter(|pending| pending.peer_id == peer_id)
                .map(|pending| (pending.update.invocation(), acting_of(pending)))
                .collect()
        };
        if let Some(answered) = self
            .served_invocations
            .lock()
            .ok()
            .and_then(|served| served.get(&peer_id).cloned())
        {
            delivered.extend(answered);
        }
        delivered
    }

    /// Keep `invocation`, delivered to `peer_id` and answered, as one the route may still write
    /// changes for as `acting`: until those changes land, or the route closes.
    fn keep_served(
        &self,
        peer_id: u64,
        invocation: tilo_actions::InvocationId,
        acting: tilo_actions::Acting,
    ) {
        let Ok(mut served) = self.served_invocations.lock() else {
            return;
        };
        served
            .entry(peer_id)
            .or_default()
            .push((invocation, acting));
    }

    /// The changes of the invocations `named` by a write `peer_id`'s route made have landed: it
    /// answers for them no more.
    fn invocation_changes_landed(&self, peer_id: u64, named: &[String]) {
        if named.is_empty() {
            return;
        }
        let Ok(mut served) = self.served_invocations.lock() else {
            return;
        };
        let Some(answered) = served.get_mut(&peer_id) else {
            return;
        };
        answered.retain(|(invocation, _)| {
            !named.iter().any(|named| named == &invocation.0.to_string())
        });
        if answered.is_empty() {
            served.remove(&peer_id);
        }
    }
}

/// Who `pending` was invoked by, as a write it makes for it records: an agent its person's
/// device handed its session to, or one under a key of its own, else the actor it was admitted
/// as.
fn acting_of(pending: &PendingInvocation) -> tilo_actions::Acting {
    pending.context.invoker.as_ref().map_or_else(
        || tilo_actions::Acting::from(pending.actor.clone()),
        |invoker| invoker.acting.clone(),
    )
}

/// What ends one invocation stream once its app asks for no more: the broker
/// stops addressing invocations to it, and the stream hands over the ones it
/// already holds before it ends.
struct InvocationFeedEnd {
    state: Arc<AppBrokerState>,
    peer_id: u64,
    sender: mpsc::WeakUnboundedSender<InvocationEnvelope>,
}

impl InvocationFeedEnd {
    /// Stop addressing invocations to this stream, unless a later stream of
    /// the app has replaced it already.
    fn finish(self) {
        let Some(sender) = self.sender.upgrade() else {
            return;
        };
        let removed = self
            .state
            .publishers
            .streams
            .lock()
            .ok()
            .and_then(|mut subscribers| {
                subscribers
                    .get(&self.peer_id)
                    .is_some_and(|current| current.same_channel(&sender))
                    .then(|| subscribers.remove(&self.peer_id))
            })
            .is_some();
        drop(sender);
        if removed {
            self.state.publishers.streams_changed();
        }
    }
}

/// Bind `socket_path` and spawn the broker's accept loop. Attaches the
/// created [`AppBrokerState`] to `fanout` so every event flowing
/// through the fanout core lands on typed subscribers.
///
/// `publisher` is the other direction — attaching to the peer table lets
/// an app *receive*, `publisher` lets it *send*, same instance
/// [`crate::collab_broker`] uses for peer replication.
///
/// The accept thread runs a `current_thread` tokio runtime + `LocalSet`
/// (the broker's futures are `!Send`), polling `quit` on a 100 ms cadence.
///
/// # Errors
///
/// Returns an error if `socket_path` cannot be bound as a Unix listener
/// (e.g. a stale socket file blocks it, permissions are wrong, or the
/// parent directory does not exist) or if spawning the accept thread
/// fails.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the broker thread takes ownership of the exact shared handles it wires into its isolated LocalSet"
)]
pub fn spawn(
    socket_path: PathBuf,
    fanout: &Arc<FanoutState>,
    broker_channels: Arc<Mutex<BrokerChannels>>,
    publisher: Arc<PeerPublisher>,
    membership: Option<WorkspaceTreeHandle>,
    services: ProcessServices,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    spawn_configured(
        &socket_path,
        fanout,
        BrokerWiring {
            broker_channels,
            publisher,
            membership,
            trusted_devices: None,
            certificate_authority: None,
            services,
            config: None,
        },
        quit,
    )
}

#[cfg(any(test, feature = "test-support"))]
pub mod tests;

/// `handle` held by an app on its own wire connection to the broker.
/// Spawns on the current `LocalSet`.
#[cfg(any(test, feature = "test-support"))]
fn wire_channel(handle: JoinedChannel) -> runtime_collab::RecordHandle {
    wire_channel_rc(Rc::new(handle))
}

/// [`wire_channel`] for a handle the test also reads from the broker's side,
/// as its invocation feed: pulled, so whatever the broker dispatched and the
/// test did not take stays countable ([`unread_invocations`]).
#[cfg(any(test, feature = "test-support"))]
fn wire_channel_rc(handle: Rc<JoinedChannel>) -> runtime_collab::RecordHandle {
    let (connection, outgoing) = wire::WireConnection::new(None);
    let kind = handle.resource_type.clone().unwrap_or_default();
    let record = connection.open(wire::ChannelSession::new(handle));
    wire_client(connection, outgoing).record(&record, &kind)
}

/// An app's wire client served by `connection`. Spawns on the current
/// `LocalSet`.
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::unwrap_used,
    reason = "test support: a host without socket pairs cannot run the test at all"
)]
fn wire_client(
    connection: Rc<wire::WireConnection>,
    outgoing: tokio::sync::mpsc::UnboundedReceiver<runtime_collab::wire::HostFrame>,
) -> runtime_collab::CollabClient {
    let (host, app) = tokio::net::UnixStream::pair().unwrap();
    let (reader, writer) = host.into_split();
    tokio::task::spawn_local(wire::serve(connection, outgoing, reader, writer));
    let (reader, writer) = app.into_split();
    let (client, carry) = runtime_collab::CollabClient::over(reader, writer);
    tokio::task::spawn_local(carry);
    client
}

/// Wait for a dropped broker to go: its channels let go of it once their
/// wire connections, closing with their clients, have wound down, and its
/// journal's writer has let the store's lock go.
#[cfg(test)]
async fn broker_gone(gone: BrokerGone) {
    tilo_test_env::within("the dropped broker lets its journal go", gone.wait()).await;
}

/// An app answering the host's asks of its published action directory from
/// `directory`. Spawns on the current `LocalSet`.
#[cfg(test)]
fn wire_answerer(
    directory: Rc<dyn wire::DirectorySource>,
) -> runtime_collab::wire::client::Answerer {
    use runtime_collab::wire::{Answer, Ask};
    type Asked = (Ask, tokio::sync::oneshot::Sender<Result<Answer, String>>);
    let (asks, mut asked) = tokio::sync::mpsc::unbounded_channel::<Asked>();
    tokio::task::spawn_local(async move {
        while let Some((ask, answer)) = asked.recv().await {
            let answered = match ask {
                Ask::Query { query, .. } => directory
                    .query(query, u32::MAX)
                    .await
                    .and_then(|page| serde_json::to_string(&page).map_err(|e| e.to_string()))
                    .map(Answer::Page),
                Ask::Resolve {
                    revision,
                    resource,
                    action,
                    local_document,
                    ..
                } => directory
                    .resolve((&revision, &resource, &action), local_document, u32::MAX)
                    .await
                    .and_then(|found| serde_json::to_string(&found).map_err(|e| e.to_string()))
                    .map(Answer::Resolution),
                Ask::Action { .. } => Err("an app's directory serves no provider".to_owned()),
            };
            let _ = answer.send(answered);
        }
    });
    Arc::new(move |ask| {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let _ = asks.send((ask, answer));
        Box::pin(async move {
            answered
                .await
                .map_err(|_| "the directory closed".to_owned())?
        })
    })
}

#[cfg(test)]
mod host_worker_capacity_tests;

#[cfg(test)]
#[path = "collab_app_broker/resource_view_tests.rs"]
mod resource_view_tests;
