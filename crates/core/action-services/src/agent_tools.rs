//! The tools an agent answering a turn works through, asked of the device that serves its session.
//!
//! An agent whose turn is worked in a process of its own (the Agent app on a desktop) holds no
//! tool server: it asks the device's session for the tools ([`LIST`]) and has each call the model
//! makes taken there ([`CALL`]), as two steps of its agent session under [`PROVIDER`]. The session
//! answers both with the same MCP tools every other agent works through, taken as the agent in its
//! agent session, so what a call does is decided, asked about first and attributed in History
//! exactly as the agent's own actions are. A call that takes long is an invocation like any other: the agent follows it until
//! it ends, and its other calls go on meanwhile.

use serde::{Deserialize, Serialize};

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

/// The application whose steps these are: the agent a turn works as.
pub use tilo_app_id::apps::AGENT as APP;
/// The provider a device's session answers an agent's tools under.
pub const PROVIDER: &str = "tilo.agent-tools";
/// The resource both steps are actions of.
pub const RESOURCE: &str = "agent.tools";
/// List the tools the agent may call; answers a [`ToolList`].
pub const LIST: &str = "list-tools";
/// Call one tool ([`ToolCall`]); answers a [`ToolAnswer`].
pub const CALL: &str = "call-tool";

/// The target of step `action`.
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// The argument a step carrying `value` sends.
#[must_use]
pub fn argument(value: &impl Serialize) -> InvokeArgument {
    tilo_actions::remote::workspace_activity::argument(value)
}

/// One tool, as a model is told it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The JSON schema of its arguments.
    pub input_schema: serde_json::Value,
}

/// What [`LIST`] answers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolList {
    pub tools: Vec<Tool>,
}

/// What [`CALL`] is asked: the tool by its name, with the arguments the model gave it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// A picture a tool answered with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Picture {
    pub media_type: String,
    pub base64: String,
}

/// What [`CALL`] answers: what the tool said, as the model reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAnswer {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pictures: Vec<Picture>,
    /// It failed or was refused; `text` says why.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}
