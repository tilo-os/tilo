//! `tilo-agent-session` — agents as first-class session members.
//!
//! An agent works in a workspace the way a person's device does, not through a
//! side channel:
//!
//! - **Identity.** Its own device key, a capability-less `DeviceCert`
//!   issued under the user's root by ordinary device certification (the
//!   approving device is where consent is given), and a root-signed
//!   `tilo_server_protocol::delegation::Delegation` naming its label, cursor
//!   colour, workspace and apps. The host confines the device to that workspace and
//!   treats it as its own actor ([`grants::AgentIdentity`]).
//! - **Apps.** Each record the agent works in is a replica of the app's
//!   resource, looked up by the kind the record names and used only through
//!   the operations the resource publishes — the same API a person's window
//!   uses — with no per-app code and nothing rendered ([`apps`]).
//! - **Confinement.** On a host, each record lives in its own confined app
//!   worker process, so no app code runs beside the host's and agents'
//!   keys; the host admits what a worker returns before it signs it
//!   ([`worker`]).
//! - **Work.** The workspace's own agents (its `tilo.workspace.agent` roster) take
//!   their turns on the host that governs it, through the host's actions
//!   ([`workspace_actions`]); each signs its changes with its own key, through a
//!   signing session the host opens for its approved grant ([`grants`]).
//! - **Grants.** [`AgentService`] keeps each roster agent's grant under a state
//!   directory, opens its signing session again when the service restarts and
//!   settles expired grants.
//! - **Memory.** There is none of its own. What an agent learns it reads
//!   from the workspace's records, and what it keeps it writes there through an
//!   app's operations; the only state this crate holds is, in the host's state
//!   directory, the agents' credentials and each workspace's content bytes
//!   ([`service`]).

#![deny(rust_2018_idioms)]

pub mod apps;
pub mod facts;
pub mod grants;
pub mod model;
pub mod record_actions;
pub mod render;
pub mod search;
pub mod service;
pub mod transform;
mod web_apps;
pub mod worker;
pub mod workspace_actions;
mod workspace_documents;

pub use apps::{
    AppCatalogue, AppError, PlacedApp, PlacedArgument, Replica, ReplicaActor, Seed, component_apps,
};
pub use grants::{
    AgentHostLink, AgentIdentity, AgentSessions, AgentSupervisor, ApprovalDone, GrantRecord,
    SupervisorSnapshot,
};
pub use record_actions::{RESOURCE_PROVIDER, resource_provider};
pub use service::{AgentService, CatalogueFactory, ServerEndpoint};
pub use worker::{ConfinedReplica, PageAction};
pub use workspace_actions::{FeedbackReviewer, WorkspaceActions, WorkspaceDocumentsLink};

// Unit tests admit peer bytes through the Automerge isolate, whose workers
// are this test binary started again.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
