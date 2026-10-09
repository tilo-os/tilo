//! `tilo-agent-participant` — agents as first-class session participants.
//!
//! An agent works in a room the way a person's device does, not through a
//! side channel:
//!
//! - **Identity.** Its own device key, a capability-less `DeviceCert`
//!   issued under the user's root by ordinary device certification (the
//!   approving device is where consent is given), and a root-signed
//!   `tilo_relay_protocol::agents::AgentDelegation` naming its label, cursor
//!   colour, rooms and apps. The host confines the device to those rooms and
//!   treats it as its own principal ([`control::AgentIdentity`]).
//! - **Apps.** Each record the agent works in is a replica of the app's
//!   resource, looked up by the kind the record names and used only through
//!   the operations the resource publishes — the same door a person's window
//!   uses — with no per-app code and nothing rendered ([`apps`]).
//! - **Edits.** Each action's Automerge change is committed through the
//!   agent's relay session and ordered by the host with everyone else's;
//!   browsers receive it live, attributed to the agent ([`participant`]).
//! - **Presence.** The agent publishes the record it focuses and a
//!   participant of kind agent on the live plane; browsers show it there,
//!   and it sees theirs.
//! - **Steering.** Spawn, message, pause, resume, stop and status are
//!   remote actions the host answers for the delegating user's devices
//!   ([`control`]); a grant is requested from the page and approved with
//!   its delegation in one consent step.
//! - **Hosting.** [`AgentHost`] runs each agent on its own thread, keeps
//!   grants and agents under a state directory, resumes them when the host
//!   restarts and settles expired grants ([`host`]).
//!
//! See `docs/architecture/browser-runtime.md`, "Agents".

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod apps;
pub mod control;
pub mod host;
pub mod model;
pub mod participant;
pub mod planner;
pub mod room_actions;
mod web_apps;

pub use apps::{AppCatalogue, AppError, Replica, Seed, component_apps};
pub use control::{
    AgentControl, AgentHostLink, AgentIdentity, AgentSupervisor, ApprovalDone, GrantRecord,
    Spawner, SupervisorHost, SupervisorSnapshot,
};
pub use host::{AgentHost, CatalogueFactory, HostEndpoint};
pub use model::{
    ApiKey, CLOUD_MODEL, GatewayModel, ModelChoice, ModelPlanner, OpenAiModel, PlanModel,
    ScriptedModel,
};
pub use participant::actions::{RECORD_PROVIDER, record_provider};
pub use participant::{AgentParticipant, ParticipantConfig};
pub use planner::{DirectPlanner, PlanContext, Planner, Step, StepAction};
pub use room_actions::{HostActions, RoomActions, RoomRecordsLink};
