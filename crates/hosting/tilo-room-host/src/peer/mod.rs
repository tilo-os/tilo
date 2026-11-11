//! Per-connection peer task — reads envelopes from the wire,
//! dispatches to the workspace runtime, drains the outbound queue
//! back to the wire.
//!
//! The connection carries **one** bidirectional stream. Reads and
//! writes run concurrently: a `tokio::select!` between the wire
//! reader and the outbound mpsc receiver. The receiver stops when
//! the connection closes; the reader stops on EOF / decode error;
//! either termination cancels the other via drop.

mod access;
mod access_audit;
mod actions;
mod admin;
mod admission;
mod agent_run;
mod checkpoint;
mod commit;
mod compaction;
mod delivery;
mod dispatch;
mod fanout;
mod fetch;
mod hello;
mod live;
mod records;
mod replay;
mod session;
mod state;
mod subscribe;
mod sync;
mod test_hooks;

pub use admission::enqueue_from_host;
pub(crate) use admission::{HostAdmission, enqueue_host_state};
use admission::{enqueue_state, enqueue_state_inner};
#[cfg(test)]
use checkpoint::buffered_retention_prefix_with_limits;
use checkpoint::{
    RETENTION_PREFIX_BYTE_CAP, RETENTION_PREFIX_FRAME_CAP, ReplayPendingTarget, ReplayPendingWork,
    buffered_retention_prefix, eligible_checkpoint_prefix,
};
#[cfg(test)]
use delivery::write_state_delivery;
use delivery::{
    STATE_RUN, SessionCoding, delivery_room, encode_charged, write_presence, write_state_deliveries,
};
use fanout::{LiveFanout, LiveFrame, live_budget};
use replay::{
    ReplayCatchup, ReplayWrite, ResumeLog, authorize_state_frame, authorize_state_recipient,
    finish_replay_write, resume_record,
};
use session::inbound_bytes;
#[cfg(test)]
use session::{finish_peer_outcome, run_peer_stream};
pub use session::{run_peer, run_peer_until};
use state::{
    FanoutStamp, PreparedRecipients, StateCommit, authorized_state_source, desktop_refusal,
    file_refusal_of, gated_checkpoint_channel, governor_notice, governor_refusal, handle_delta,
    handle_delta_prefix, live_audience, unpublished,
};
pub use state::{Lag, StateSource};
pub(crate) use state::{StateDelivery, StateSession, tell_governor};
use test_hooks::test_point;

use access::{
    action_peer_admitted, audit_subscriber_access, current_session, lost_session,
    session_peer_authorized, unavailable_target,
};
use actions::{RemoteActionRoute, handle_host_action, handle_remote_action};
use admin::handle_workspaces;
use agent_run::handle_agent_run;
use commit::{
    change_standing, commit_run_key, handle_commit, handle_commit_run, over_change_limit,
    over_storage,
};
use compaction::handle_compaction;
#[cfg(test)]
use dispatch::handle_inbound_frame;
use dispatch::{handle_envelope, unserved_private};
pub use fetch::FetchGrants;
use hello::handle_host_hello;
use live::{
    handle_content_want, handle_live_join, handle_presence, handle_voice, route_record_content,
};
use records::handle_open_record;
pub use subscribe::get_or_autobind_workspace;
use subscribe::{Subscribed, Subscription, handle_subscribe, handle_unsubscribe, subscribe};
use sync::handle_sync_request;

use crate::store::bounded::Invariant;
use access_audit::AccessAudit;
use std::collections::HashMap;
use std::sync::Arc;

use tilo_clock::unix_now_ns;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::AuthenticatedPeer;
use crate::persistence::{ReplayAdvance, ReplayHandle, ReplayTicket, WrittenBatch};
use crate::store::StoreError;
use crate::store::bounded::WorkLimit;
use crate::store::replay::Step;
use crate::transport::Envelope;
use crate::workspace::{SharedWorkspace, Subscriber};
use crate::workspace_access::{self, Decision, RelayTrust, WorkspaceOperation};
use tilo_delivery_protocol::refusal::UnavailableReason;
use tilo_delivery_protocol::transport_v2::{EnvelopeV2, SessionReader};

/// Maximum live fan-out backlog per connected peer. Deltas are durable before
/// queueing; saturation disconnects the recipient for replay on reconnect.
const PEER_OUTBOUND_CAPACITY: usize = 32;

/// Reason a peer task ends.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    /// Reader error.
    #[error("frame: {0}")]
    Frame(#[from] crate::frame::FrameError),
    /// Writer error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] postcard::Error),
    /// Backing store error.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// The peer produced a message we did not expect (e.g. Delta
    /// for a workspace it never Subscribed to).
    #[error("peer protocol violation: {0}")]
    Protocol(String),
    /// A newer connection for the same device replaced this connection's
    /// subscriber slot while its replay capture was queued.
    #[error("superseded by a newer connection for this device")]
    Superseded,
}

/// Handle to the set of workspaces the daemon owns.
pub type WorkspaceRegistry = Arc<AsyncMutex<HashMap<u64, SharedWorkspace>>>;
pub(crate) const MAX_AUTOBOUND_WORKSPACES: usize = 4096;

#[cfg(test)]
const fn autobind_has_capacity(current: usize) -> bool {
    current < MAX_AUTOBOUND_WORKSPACES
}

/// Optional on-demand workspace creation used by a same-owner home host.
/// Shared/provider hosts leave this disabled and pre-bind their tenant
/// workspaces explicitly.
#[derive(Clone)]
pub struct WorkspaceAutobind {
    pub state_root: std::path::PathBuf,
    /// The adapter a newly seen workspace's store classifies co-edit bodies
    /// with: the one the relay was started with.
    pub shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
}

impl std::fmt::Debug for WorkspaceAutobind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceAutobind")
            .field("state_root", &self.state_root)
            .finish_non_exhaustive()
    }
}

/// Relay-wide state a peer session needs for its whole lifetime — the
/// workspace registry, autobind policy, live revocation list, own
/// user chain, and metrics. Bundled into one struct so [`run_peer`]'s
/// argument count stays reasonable; see `SessionCtx` for the subset
/// `handle_envelope` reads on every message.
pub struct PeerContext {
    /// Handle to the set of workspaces the daemon owns.
    pub workspaces: WorkspaceRegistry,
    /// Optional on-demand workspace creation (same-owner home host).
    pub autobind: Option<WorkspaceAutobind>,
    /// Live revocation list, refreshed independently of this session.
    pub revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    /// Live signed-workspace-derived collaborator admission policy.
    pub trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    /// The user pubkey this relay itself chains to.
    pub own_user_public_key: [u8; 32],
    /// Exact certified device running this relay. Private actions may use this
    /// identity for a local durable-broker self-connection even though its
    /// certificate says it may host.
    pub own_device_cert: tilo_user_identity::DeviceCert,
    /// Metrics — cheap counters.
    pub metrics: Arc<crate::metrics::Metrics>,
    /// This relay's own device key: the address remote-action frames use to
    /// reach the app-serving plane.
    pub own_device_public_key: [u8; 32],
    /// Answers remote-action frames addressed to this relay. `None` unless
    /// the host supplied one and advertises `app_serving`.
    pub app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
    /// Agent devices this host was told about, see [`AgentRegistry`].
    pub agents: AgentRegistry,
    /// The devices this host certified for its browser environments, and the
    /// one record each reaches ([`crate::environment`]).
    pub environments: crate::environment::EnvironmentRegistry,
    /// Mints and checks resumable subscribers' positions; `None` refuses
    /// resumption, and a `SubscribeFrom` replays everything.
    pub positions: Option<crate::position::PositionIssuer>,
    /// Keeps retained document channels short; `None` never compacts and
    /// refuses compaction volunteers.
    pub compactor: Option<crate::compaction::Compactor>,
    /// Per-person limits of a public host; `None` limits nobody.
    pub member_limits: Option<Arc<crate::member_limits::MemberLimits>>,
    /// The workspaces whose calls this host carries: a workspace a person owns
    /// (synced across their devices through this host) holds its calls
    /// there. `None` holds no carried call.
    pub carried_calls: Option<crate::workspaces::WeakWorkspaces>,
    /// The governance a device administers its workspaces through, set once
    /// when the host governs them; `None` administers nothing.
    pub admin: Option<Arc<crate::workspaces::Workspaces>>,
    /// The token a session takes for the host's fetch proxy, and gives back
    /// when its connection ends; `None` serves no proxy.
    pub fetch: Option<Arc<dyn FetchGrants>>,
}

/// Agent devices by device key: their verified delegation, or `None` once
/// the agent was withdrawn. An agent device is confined to its delegated
/// workspaces, is its own principal for remote actions, and a withdrawn one is
/// refused like a revoked device.
pub type AgentRegistry = Arc<
    std::sync::RwLock<
        std::collections::HashMap<[u8; 32], Option<tilo_relay_protocol::delegation::Delegation>>,
    >,
>;

/// Read-mostly session state [`handle_envelope`] and its per-arm
/// helpers read on every inbound message for one connection. Built
/// once per [`run_peer`] call from a [`PeerContext`] plus the
/// session's own outbound / disconnect channels.
struct SessionCtx<'a> {
    workspaces: &'a WorkspaceRegistry,
    autobind: Option<&'a WorkspaceAutobind>,
    outbound: &'a mpsc::Sender<Envelope>,
    state_outbound: Option<&'a mpsc::Sender<StateDelivery>>,
    connection_live: Option<&'a Arc<std::sync::atomic::AtomicBool>>,
    own_user_public_key: &'a [u8; 32],
    /// The certificate of the device running this relay, so a session can
    /// recognise its own host's local self-connection.
    own_device_cert: &'a tilo_user_identity::DeviceCert,
    metrics: &'a Arc<crate::metrics::Metrics>,
    trust_policy: &'a Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    disconnect: &'a mpsc::Sender<()>,
    /// Ends this session at once, as the person's agent door stops an
    /// agent's run; `None` for a session nothing stops.
    stop: Option<&'a Arc<tokio::sync::Notify>>,
    own_device_public_key: &'a [u8; 32],
    app_serving: Option<&'a Arc<dyn crate::app_serving::AppServingPlane>>,
    revocations: &'a Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    agents: &'a AgentRegistry,
    environments: &'a crate::environment::EnvironmentRegistry,
    positions: Option<crate::position::PositionIssuer>,
    compactor: Option<&'a crate::compaction::Compactor>,
    member_limits: Option<&'a crate::member_limits::MemberLimits>,
    /// This connection's resume log per resumed workspace.
    resumes: &'a std::sync::Mutex<HashMap<u64, ResumeLog>>,
    /// The agent this connection is a run of, once its device said so
    /// ([`Envelope::AgentRun`]): every request the
    /// connection makes from then on is that agent's, acting for the device's
    /// person, as the session's caller. Said once, it stays: a session
    /// handed to an agent is never handed back.
    agent: &'a std::sync::OnceLock<String>,
    /// The execution the host minted for this connection: an agent's or a
    /// key's requests on it are one run, whatever the client says
    /// ([`tilo_actions::remote::Caller::agent_run`]). Closed when the
    /// connection ends.
    execution: &'a tilo_actions::remote::ExecutionLease,
    /// Workspaces whose live deliveries this connection could not take.
    lag: &'a Arc<Lag>,
    /// Where this connection's workspaces post the presence they fan out to it;
    /// `None` takes each presence on the frame queue.
    presence: Option<&'a Arc<crate::presence_mailbox::PresenceMailbox>>,
    /// The workspaces whose calls this host carries; `None` holds no carried
    /// call.
    carried_calls: Option<crate::workspaces::WeakWorkspaces>,
    /// What a device administers its workspaces through; `None` administers
    /// nothing.
    admin: Option<&'a crate::workspaces::Workspaces>,
    /// The host's fetch proxy, which this session takes a token from; `None`
    /// serves no proxy.
    fetch: Option<&'a dyn FetchGrants>,
}

impl SessionCtx<'_> {
    /// This relay's certificate and agent trust, as the session sees it.
    fn trust(&self) -> RelayTrust<'_> {
        RelayTrust {
            revocations: self.revocations,
            policy: self.trust_policy,
            own_user: self.own_user_public_key,
            agents: self.agents,
            environments: self.environments,
        }
    }

    /// What `device` reaches when it is one of this host's browser
    /// environments' devices ([`crate::environment`]): its one workspace and
    /// the one record there. `None` for every other device.
    fn environment(&self, device: &[u8; 32]) -> Option<crate::environment::EnvironmentScope> {
        crate::environment::scope_of(self.environments, device)
    }

    /// The workspace admission decision for `peer` on `op`.
    fn decide(
        &self,
        workspace: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        op: WorkspaceOperation,
    ) -> Decision {
        workspace_access::decide(workspace, &self.trust(), peer, op)
    }

    /// Whether `peer` may act in `workspace` on `op` now, without enrolment.
    fn admits(
        &self,
        workspace: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        op: WorkspaceOperation,
    ) -> bool {
        self.decide(workspace, peer, op).admitted()
    }
}

// Both ends trace the same publication hop; the client side lives with the
// session client.
pub(crate) use tilo_relay_session::trace_native_publication;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#[allow(
    clippy::similar_names,
    reason = "paired test fixtures are named after the roles they mirror"
)]
#[allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
mod private_transport_tests;

#[cfg(test)]
mod retention_diagnostic_tests;

#[cfg(test)]
mod state_tests;

#[cfg(all(test, feature = "agents"))]
mod refused_change_tests;

#[cfg(test)]
mod state_delivery_tests;
