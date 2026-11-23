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
mod agent_session;
mod delivery;
mod dispatch;
mod fetch;
mod live;
pub(crate) mod records;
mod session;
mod subscribe;

use delivery::{encode_charged, write_presence};
pub use session::{run_peer, run_peer_until};

use access::{
    action_peer_admitted, audit_subscriber_access, current_session, session_peer_authorized,
    unavailable_target,
};
use actions::{RemoteActionRoute, handle_host_action, handle_remote_action};
use agent_session::handle_agent_session;
use dispatch::handle_envelope;
pub use fetch::FetchGrants;
use live::{
    handle_content_want, handle_host_said, handle_live_join, handle_presence,
    route_document_content,
};
use records::handle_open_document;
use subscribe::{handle_subscribe, handle_unsubscribe};

use access_audit::AccessAudit;
use std::collections::HashMap;
use std::sync::Arc;

use tilo_clock::unix_now_ns;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::AuthenticatedPeer;
use crate::transport::Envelope;
use crate::workspace::{SharedWorkspace, Subscriber};
use crate::workspace_access::{self, Decision, ServerTrust, WorkspaceOperation};
use tilo_delivery_protocol::transport_v2::{EnvelopeV2, SessionReader};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::refusal::Refusal;

/// Maximum live fan-out backlog per connected peer: a peer that cannot keep
/// up is disconnected, and is sent what it needs again when it reconnects.
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
    /// The workspace's files beside the store could not be read or written.
    #[error("files: {0}")]
    Files(#[from] crate::workspace_files::FilesError),
    /// The peer produced a message we did not expect (e.g. presence
    /// for a workspace it never subscribed to).
    #[error("peer protocol violation: {0}")]
    Protocol(String),
    /// The server does not admit this device, which holds no workspace here:
    /// its refusal went out and the session ended with it. A refusal, not a
    /// protocol violation.
    #[error("not admitted")]
    NotAdmitted,
}

/// Handle to the set of workspaces the daemon owns.
pub type WorkspaceRegistry = Arc<AsyncMutex<HashMap<WorkspaceId, SharedWorkspace>>>;
/// The most workspaces a host holds at once, each with its own state on disk.
pub(crate) const MAX_HELD_WORKSPACES: usize = 4096;

/// Server-wide state a peer session needs for its whole lifetime — the
/// workspace registry, live revocation list, own user chain, and metrics.
/// Bundled into one struct so the peer entry point's argument count stays
/// reasonable; see `SessionCtx` for the subset `handle_envelope` reads on
/// every message.
pub struct PeerContext {
    /// Handle to the set of workspaces the daemon owns.
    pub workspaces: WorkspaceRegistry,
    /// Live revocation list, refreshed independently of this session.
    pub revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    /// Live signed-workspace-derived member admission policy.
    pub trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    /// The user pubkey this server itself chains to.
    pub own_user_public_key: [u8; 32],
    /// Exact certified device running this server. Private actions may use this
    /// identity for a local durable-broker self-connection even though its
    /// certificate says it may host.
    pub own_device_cert: tilo_user_identity::DeviceCert,
    /// Metrics — cheap counters.
    pub metrics: Arc<crate::metrics::Metrics>,
    /// This server's own device key: the address remote-action frames use to
    /// reach the app action server.
    pub own_device_public_key: [u8; 32],
    /// Answers remote-action frames addressed to this server. `None` unless
    /// the host supplied one and advertises `app_serving`.
    pub app_serving: Option<Arc<dyn crate::app_serving::AppActionServer>>,
    /// Agent devices this host was told about, see [`AgentRegistry`].
    pub agents: AgentRegistry,
    /// The devices this host certified for its browser environments, and the
    /// one record each reaches ([`crate::environment`]).
    pub environments: crate::environment::EnvironmentRegistry,
    /// The governance a device administers its workspaces through, set once
    /// when the host governs them; `None` administers nothing.
    pub admin: Option<Arc<crate::workspaces::Workspaces>>,
    /// The token a session takes for the host's fetch proxy, and gives back
    /// when its connection ends; `None` serves no proxy.
    pub fetch: Option<Arc<dyn FetchGrants>>,
}

/// Agent devices by device key: their verified delegation, or `None` once
/// the agent was withdrawn. An agent device is confined to its delegated
/// workspaces, is its own actor for remote actions, and a withdrawn one is
/// refused like a revoked device.
pub type AgentRegistry = Arc<
    std::sync::RwLock<
        std::collections::HashMap<[u8; 32], Option<tilo_server_protocol::delegation::Delegation>>,
    >,
>;

/// Read-mostly session state [`handle_envelope`] and its per-arm
/// helpers read on every inbound message for one connection. Built
/// once per [`run_peer`] call from a [`PeerContext`] plus the
/// session's own outbound / disconnect channels.
struct SessionCtx<'a> {
    workspaces: &'a WorkspaceRegistry,
    outbound: &'a mpsc::Sender<Envelope>,
    /// This connection's queue for content its outbound channel could not
    /// take yet, one queue per sender ([`crate::content_queue`]).
    content: &'a Arc<crate::content_queue::ContentQueue>,
    own_user_public_key: &'a [u8; 32],
    metrics: &'a Arc<crate::metrics::Metrics>,
    trust_policy: &'a Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    disconnect: &'a mpsc::Sender<()>,
    /// Ends this session at once, as the person's MCP server stops an
    /// agent's run; `None` for a session nothing stops.
    stop: Option<&'a Arc<tokio::sync::Notify>>,
    own_device_public_key: &'a [u8; 32],
    app_serving: Option<&'a Arc<dyn crate::app_serving::AppActionServer>>,
    revocations: &'a Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    agents: &'a AgentRegistry,
    environments: &'a crate::environment::EnvironmentRegistry,
    /// The agent this connection is a run of, once its device said so
    /// ([`Envelope::AgentSession`]): every request the
    /// connection makes from then on is that agent's, acting for the device's
    /// person, as the session's caller. Said once, it stays: a session
    /// handed to an agent is never handed back.
    agent: &'a std::sync::OnceLock<String>,
    /// The execution the host minted for this connection: an agent's or a
    /// key's requests on it are one run, whatever the client says
    /// ([`tilo_actions::remote::Caller::agent_session`]). Closed when the
    /// connection ends.
    execution: &'a tilo_actions::remote::ExecutionLease,
    /// Where this connection's workspaces post the presence they fan out to it;
    /// `None` takes each presence on the frame queue.
    presence: Option<&'a Arc<crate::presence_mailbox::PresenceMailbox>>,
    /// What a device administers its workspaces through; `None` administers
    /// nothing.
    admin: Option<&'a crate::workspaces::Workspaces>,
    /// The host's fetch proxy, which this session takes a token from; `None`
    /// serves no proxy.
    fetch: Option<&'a dyn FetchGrants>,
}

impl SessionCtx<'_> {
    /// This server's certificate and agent trust, as the session sees it.
    fn trust(&self) -> ServerTrust<'_> {
        ServerTrust {
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
        crate::environment::environment_of(self.environments, device)?.placed()
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

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod private_transport_tests;
