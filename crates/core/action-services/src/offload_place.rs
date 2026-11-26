use serde::{Deserialize, Serialize};

use tilo_actions::{ActionOutput, InvocationId, Offload};
use {tilo_actions::ActionAddress, tilo_actions::remote::RemoteActionTarget};

/// The provider that places offloadable invocations.
pub const PROVIDER: &str = "tilo.offload";
/// The resource the action acts on.
pub const RESOURCE: &str = "offload";
/// Place an admitted offloadable invocation on the workspace's devices.
pub const PLACE: &str = "place";

/// The action a session invokes on its device's daemon to place one.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, PLACE),
    }
}

/// What the session asks the daemon to place: the admitted offloadable
/// invocation, under the id the app invoked it with, so a retry at the
/// device that takes it is not a second run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    /// The workspace it was invoked in.
    pub workspace: String,
    /// The app whose action it is, whose content the inputs and outputs are.
    pub app_id: String,
    /// The resource the action is invoked on.
    pub resource: String,
    /// The action.
    pub action: String,
    /// The transform the action declares.
    pub offload: Offload,
    /// The content it reads, by digest.
    pub inputs: Vec<String>,
    /// The invocation the app made.
    pub invocation: InvocationId,
}

/// What became of the placement: another device ran it, or it should run
/// here, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placed {
    /// Another device ran it: its output, the device that ran it, and how
    /// long it took there.
    Ran {
        output: ActionOutput,
        device: String,
        elapsed_ms: u64,
    },
    /// Run it here, through the app's handler, and why it was not placed.
    Here { reason: String },
}
