//! The app's record, which sessions Agents shows: its type and verb are `console-model`'s.

pub use console_model::Console;
use ui::tilo_actions::remote::workspace_activity as wire;

/// Whether `session` shows under `filter`: its own name picks it, and the name of the client it
/// comes from picks every session of that client.
pub(crate) fn shows(filter: &Console, session: &wire::AgentSession) -> bool {
    let named = filter.agent.is_empty()
        || filter.agent == session.agent.name
        || filter.agent == crate::client::of(&session.agent.name).name();
    named
        && (filter.workspace.is_empty()
            || Some(filter.workspace.as_str()) == session.workspace.as_deref())
}
