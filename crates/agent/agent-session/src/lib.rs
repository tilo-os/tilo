//! `tilo-agent-session` — agents as first-class session members.
//!
//! An agent works in a workspace the way a person's device does, not through a
//! side channel:
//!
//! - **Identity.** Its own device key, a capability-less `DeviceCert`
//!   issued under the user's root by ordinary device certification (the
//!   approving device is where consent is given), and a root-signed
//!   `tilo_server_protocol::delegation::Delegation` naming its label, cursor
//!   colour, workspaces and apps. The host confines the device to those workspaces and
//!   treats it as its own actor ([`control::AgentIdentity`]).
//! - **Apps.** Each record the agent works in is a replica of the app's
//!   resource, looked up by the kind the record names and used only through
//!   the operations the resource publishes — the same API a person's window
//!   uses — with no per-app code and nothing rendered ([`apps`]).
//! - **Confinement.** On a host, each record lives in its own confined app
//!   worker process, so no app code runs beside the host's and agents'
//!   keys; the host admits what a worker returns before it signs it
//!   ([`worker`]).
//! - **Work.** Each task is one turn of the host's model over the tools every
//!   agent in Tilo works through (`tilo_ls`, `tilo_read`, `tilo_do` and the
//!   rest), asked of the host's actions through the agent's own session: the
//!   host runs each action as the agent and orders its change with everyone
//!   else's; browsers receive it live, attributed to the agent ([`member`]).
//! - **Presence.** The agent publishes a member of kind agent, saying what it
//!   is doing, on the live session; browsers show it there, and it sees
//!   theirs.
//! - **Steering.** Spawn, message, pause, resume, stop and status are
//!   remote actions the host answers for the delegating user's devices
//!   ([`control`]); a grant is requested from the page and approved with
//!   its delegation in one consent step.
//! - **Hosting.** [`AgentHost`] runs each agent on its own thread, keeps
//!   grants and agents under a state directory, resumes them when the host
//!   restarts and settles expired grants ([`host`]).
//! - **Memory.** There is none of its own. What an agent learns it reads
//!   from the workspace's records, and what it keeps it writes there through an
//!   app's operations; the only state this crate holds is, in the host's state
//!   directory, the agents' credentials and each workspace's content bytes
//!   ([`host`]).

#![deny(rust_2018_idioms)]

pub mod apps;
pub mod bot;
pub mod control;
pub mod facts;
pub mod host;
pub mod member;
pub mod model;
pub mod profile;
pub mod render;
pub mod search;
mod web_apps;
pub mod worker;
pub mod workspace_actions;
mod workspace_documents;

pub use apps::{
    AppCatalogue, AppError, PlacedApp, PlacedArgument, Replica, ReplicaActor, Seed, component_apps,
};
pub use control::AgentSessions;
pub use control::{
    AdmissionDone, AgentControl, AgentHostLink, AgentIdentity, AgentSupervisor, ApprovalDone,
    GrantRecord, Spawner, SupervisorHost, SupervisorSnapshot,
};
pub use host::{AgentHost, CatalogueFactory, HostEndpoint};
pub use member::actions::{RESOURCE_PROVIDER, record_provider};
pub use member::{AgentMember, MemberConfig, Next, Pumped};
pub use model::{ApiKey, CLOUD_MODEL, MakeTurns, ModelChoice};
pub use worker::{ConfinedReplica, PageAction};
pub use workspace_actions::{
    FeedbackReviewer, HostActions, WorkspaceActions, WorkspaceDocumentsLink,
};

// Unit tests admit peer bytes through the Automerge isolate, whose workers
// are this test binary started again.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
