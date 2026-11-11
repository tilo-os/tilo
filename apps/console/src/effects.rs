//! What a person asks of an agent through Console's effects
//! ([`ConsoleEffect`], the record's vocabulary in `console-model`): which
//! agent an effect is about, and what it asks of it.

pub use console_model::ConsoleEffect;

/// What a person asked of an agent from outside its card, carried out once
/// the card has read the runs and the door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentCommand {
    Stop,
    UndoRun,
    TakeOver,
    Revoke,
}

/// The agent `effect` is about and what it asks of it: `None` for a run's
/// page.
pub(crate) fn agent(effect: &ConsoleEffect) -> Option<(&str, Option<AgentCommand>)> {
    match effect {
        ConsoleEffect::ViewRun { .. } => None,
        ConsoleEffect::ViewAgent { agent } => Some((agent, None)),
        ConsoleEffect::StopAgent { agent } => Some((agent, Some(AgentCommand::Stop))),
        ConsoleEffect::UndoAgentRun { agent } => Some((agent, Some(AgentCommand::UndoRun))),
        ConsoleEffect::TakeOverAgent { agent } => Some((agent, Some(AgentCommand::TakeOver))),
        ConsoleEffect::RevokeAgent { agent } => Some((agent, Some(AgentCommand::Revoke))),
    }
}
