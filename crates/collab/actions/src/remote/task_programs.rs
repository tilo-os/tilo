//! An agent task's tilolang programs (`docs/designs/app-language/agents.md`, "How a program
//! runs"), served by the session to the Tasks agent's loop.
//!
//! The session's door runs every task's programs: an MCP client's through its `tilo` tool, the
//! Tasks agent's through [`RUN`] under this provider. Both are the same step of the same task
//! session: the program runs in the task's own tilolang host, what it reads and changes is done
//! as the agent working on the task, one call is one step that `undo` takes back, and the
//! answer is tilolang text whose references later programs of the task take.

use super::RemoteActionTarget;
use crate::door::ActionAddress;

/// The provider the session answers a task's programs under.
pub const PROVIDER: &str = "tilo.task-programs";
/// The resource the step is an action of.
pub const RESOURCE: &str = "task";
/// Run one program as one step of the caller's task: `program`, `confirm` (send again a
/// previewed program), and `this` (what the person pointed at, as a resource literal). Answers
/// a [`ProgramAnswer`].
pub const RUN: &str = "run-program";

#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, RUN),
    }
}

/// What a program answered.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProgramAnswer {
    /// What the agent reads: tilolang, each resource named by a short reference.
    pub text: String,
    pub failed: bool,
    /// The task host's answer (`{state, output, detail, steps, changed, apps}`), when the
    /// program ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<serde_json::Value>,
    /// Each reference the text shows (`t3`), with the resource literal it names
    /// (`@tasks.Task:root/items:x`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<(String, String)>,
}
