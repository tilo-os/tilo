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
mod admission;
mod checkpoint;
mod checkpoint_commit;
mod commit;
mod compaction;
mod delivery;
mod dispatch;
mod fanout;
mod federation;
mod live;
mod replay;
mod session;
mod state;
mod subscribe;
pub mod sync_refresh;
mod test_hooks;

use crate::store;
pub(crate) use admission::{HostAdmission, enqueue_federated, enqueue_host_state};
use admission::{enqueue_state, enqueue_state_inner};
#[cfg(test)]
use checkpoint::{
    CHECKPOINT_PROGRESS, CheckpointProgress, buffered_retention_prefix_with_limits,
    checkpoint_mode_for, new_checkpoint_batch, step_checkpoint_batch, track_checkpoint_batch_modes,
};
use checkpoint::{
    CheckpointBatch, CheckpointBatchInput, CheckpointBatchMode, RETENTION_PREFIX_BYTE_CAP,
    RETENTION_PREFIX_FRAME_CAP, ReplayPendingTarget, ReplayPendingWork, buffered_retention_prefix,
    checkpoint_prefix_recovery_allowed, commit_and_materialize, eligible_checkpoint_prefix,
    enqueue_checkpoint_prefix, retention_publication_kind, state_commit_for, step_commit_replay,
    union_sessions,
};
use checkpoint_commit::{
    CheckpointCommitFence, ProjectedCheckpointCommit, ProjectedCheckpointStep,
    SerialCheckpointCommit, promote_checkpoint_input, publish_checkpoint_commit,
    publish_checkpoint_commit_fenced,
};
#[cfg(test)]
use delivery::write_state_delivery;
use delivery::{
    STATE_RUN, SessionCoding, delivery_room, encode_charged, write_presence, write_state_deliveries,
};
use fanout::{LiveFanout, LiveFrame, PublicationFrames, PublicationScope, live_budget};
use replay::{
    ReplayCatchup, ReplayWrite, ResumeLog, authorize_state_frame, authorize_state_recipient,
    finish_replay_write, resume_record, send_private_reply, write_private,
};
#[cfg(test)]
use replay::{authorize_replay, replay_direct_wait};
use session::inbound_bytes;
pub(crate) use session::{SessionProtocol, run_peer_version};
#[cfg(test)]
use session::{finish_peer_outcome, private_may_overtake, run_peer_stream};
pub use session::{run_peer, run_peer_until};
use state::{
    FanoutStamp, LiveEnvelope, PreparedRecipients, SelectedPublication, SessionLease, SessionLives,
    StateCommit, authorized_state_source, captured_live_current, gated_checkpoint_channel,
    governor_notice, handle_delta, handle_delta_prefix, live_audience, prepare_recipients,
    steward_audience, steward_only_delta, union_session_lives, unpublished,
};
pub(crate) use state::{Lag, StateDelivery, StateSession, StateSource};
#[cfg(feature = "test-support")]
pub use test_hooks::CheckpointPrefixPromotionFaultState;
use test_hooks::{checkpoint_prefix_post_first_promotion_fault, test_point};
#[cfg(test)]
use test_hooks::{observe_reader_backlog, stop_tracking_reader_backlog, track_reader_backlog};

use access::{
    action_peer_admitted, audit_subscriber_access, current_action_peer, current_session,
    session_peer_authorized, unavailable_target,
};
pub(crate) use actions::{ActionTarget, PrivateOutbound};
#[cfg(test)]
use actions::{PrivateDelivery, authorize_delivery};
use actions::{
    RemoteActionRoute, enable_actions, enable_broker_client, handle_remote_action, register_broker,
    route_private, route_signed_directed, write_private_delivery,
};
use commit::{handle_commit, over_change_limit};
use compaction::handle_compaction;
#[cfg(test)]
use dispatch::handle_inbound_frame;
use dispatch::{handle_envelope, handle_private};
use federation::{handle_peer_relay_hello, handle_sync_request};
use live::{
    handle_live, handle_live_join, handle_live_leave, handle_presence, handle_voice,
    route_record_content,
};
pub(crate) use subscribe::get_or_autobind_workspace;
use subscribe::{Subscription, handle_subscribe, handle_unsubscribe};

use crate::store::bounded::Invariant;
use access_audit::AccessAudit;
use std::collections::HashMap;
use std::sync::Arc;

use tilo_clock::unix_now_ns;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::AuthenticatedPeer;
use crate::persistence::{ReplayAdvance, ReplayHandle, ReplayTicket, WrittenBatch};
use crate::room_access::{self, Decision, RelayTrust, RoomOperation};
use crate::store::StoreError;
use crate::store::bounded::WorkLimit;
use crate::store::replay::Step;
use crate::transport::Envelope;
use crate::workspace::{SharedWorkspace, Subscriber};
use tilo_delivery_protocol::peer_actions::{
    ActionMessage, DirectedAction, SignedDirectedAction, UnavailableReason,
};
use tilo_delivery_protocol::transport_v2::{ActionCapability, EnvelopeV2, SessionReader};

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
    /// Postcard encode error.
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
    /// Relay state root under which a newly seen workspace is persisted.
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
    /// host capability classifies it as a peer relay.
    pub own_device_cert: tilo_user_identity::DeviceCert,
    /// Metrics — cheap counters.
    pub metrics: Arc<crate::metrics::Metrics>,
    /// This relay's own device key: the address remote-action frames use to
    /// reach the app-serving plane.
    pub own_device_public_key: [u8; 32],
    /// Answers remote-action frames addressed to this relay. `None` unless
    /// the host supplied one and advertises `app_serving`.
    pub app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
    /// Also hands validated live frames and devices' presence to the
    /// browser plane, when the host serves browsers.
    pub live_bridge: Option<LiveBridge>,
    /// Agent devices this host was told about, see [`AgentRegistry`].
    pub agents: AgentRegistry,
    /// Mints and checks resumable subscribers' positions; `None` refuses
    /// resumption, and a `SubscribeFrom` replays everything.
    pub positions: Option<crate::position::PositionIssuer>,
    /// Keeps retained document channels short; `None` never compacts and
    /// refuses compaction volunteers.
    pub compactor: Option<crate::compaction::Compactor>,
    /// Per-person limits of a public host; `None` limits nobody.
    pub member_limits: Option<Arc<crate::member_limits::MemberLimits>>,
}

/// The host's browser plane, as the relay's sessions feed it.
pub trait BrowserBridge: Send + Sync {
    /// A validated live frame: workspace, stamped sender, frame.
    fn live(&self, workspace_id: u64, from: [u8; 32], frame: &[u8]);
    /// The presence payload a subscribed device (`from`, of `user`) sent.
    fn presence(&self, workspace_id: u64, from: [u8; 32], user: [u8; 32], payload: &[u8]);
}

/// Receives what sessions hand the browser plane, see [`BrowserBridge`].
pub type LiveBridge = Arc<dyn BrowserBridge>;

/// Agent devices by device key: their verified delegation, or `None` once
/// the agent was withdrawn. An agent device is confined to its delegated
/// rooms, is its own principal for remote actions, and a withdrawn one is
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
    own_device_cert: &'a tilo_user_identity::DeviceCert,
    metrics: &'a Arc<crate::metrics::Metrics>,
    trust_policy: &'a Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    disconnect: &'a mpsc::Sender<()>,
    own_device_public_key: &'a [u8; 32],
    app_serving: Option<&'a Arc<dyn crate::app_serving::AppServingPlane>>,
    /// The state-lane framing the session's protocol fixed.
    framing: tilo_delivery_protocol::transport_v2::StateFraming,
    /// The session negotiated `ALPN_SIGNED_ACTIONS_V1`: it may receive
    /// `CertifiedSignedDelivered`. Older sessions never see that variant.
    signed_actions: bool,
    private_tx: &'a mpsc::Sender<PrivateOutbound>,
    revocations: &'a Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    live_bridge: Option<&'a LiveBridge>,
    agents: &'a AgentRegistry,
    positions: Option<crate::position::PositionIssuer>,
    compactor: Option<&'a crate::compaction::Compactor>,
    member_limits: Option<&'a crate::member_limits::MemberLimits>,
    /// This connection's resume log per resumed workspace.
    resumes: &'a std::sync::Mutex<HashMap<u64, ResumeLog>>,
    /// Rooms whose live deliveries this connection could not take.
    lag: &'a Arc<Lag>,
    /// Where this connection's rooms post the presence they fan out to it;
    /// `None` takes each presence on the frame queue.
    presence: Option<&'a Arc<crate::presence_mailbox::PresenceMailbox>>,
}

impl SessionCtx<'_> {
    /// This relay's certificate and agent trust, as the session sees it.
    fn trust(&self) -> RelayTrust<'_> {
        RelayTrust {
            revocations: self.revocations,
            policy: self.trust_policy,
            own_user: self.own_user_public_key,
            agents: self.agents,
        }
    }

    /// The room admission decision for `peer` on `op`.
    fn decide(
        &self,
        room: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        op: RoomOperation,
    ) -> Decision {
        room_access::decide(room, &self.trust(), peer, op)
    }

    /// Whether `peer` may act in `room` on `op` now, without enrolment.
    fn admits(
        &self,
        room: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        op: RoomOperation,
    ) -> bool {
        self.decide(room, peer, op).admitted()
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

#[cfg(test)]
mod state_delivery_tests;
