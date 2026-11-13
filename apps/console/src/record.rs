//! The app's record, which runs Agents shows: its type and verb are `console-model`'s.

pub use console_model::Console;
use ui::tilo_actions::remote::workspace_activity as wire;

/// Whether `run` shows under `filter`: its own name picks it, and the name of the client it
/// comes from picks every run of that client.
pub(crate) fn shows(filter: &Console, run: &wire::AgentRun) -> bool {
    let named = filter.agent.is_empty()
        || filter.agent == run.agent.name
        || filter.agent == crate::client::of(&run.agent.name).name();
    named
        && (filter.workspace.is_empty()
            || Some(filter.workspace.as_str()) == run.workspace.as_deref())
}
