//! The model an agent works with: which one a host runs ([`ModelChoice`]),
//! and what carries its turns, holding the credential ([`PlanTurns`]).
//!
//! [`GatewayTurns`] asks through `tilo-model-gateway`, which holds the
//! provider credential: the agent never sees a key. An endpoint the trusted
//! host was given is asked with the host's own client
//! ([`ModelChoice::turns`]). [`ScriptedTurns`] calls no network at all and
//! plays what a task writes out, for tests and a host started with
//! `--agent-model scripted`.

use std::sync::Arc;

/// Trusted-host CLI credential source; sandboxed agents use the gateway capability.
pub use tilo_model_gateway::host_backend::ApiKey;

mod openai;
mod scripted;
mod turns;

pub use openai::{CLOUD_MODEL, ModelChoice};
pub use scripted::ScriptedTurns;
pub use turns::{Answered, GatewayTurns, PlanTurns};

/// Makes the turns of the model a host's agents work with, on the thread
/// that asks it; `None` when the host has no model they can work with.
pub type MakeTurns = Arc<dyn Fn() -> Option<Box<dyn PlanTurns>> + Send + Sync>;
