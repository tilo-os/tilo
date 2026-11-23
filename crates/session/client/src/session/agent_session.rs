//! A session declaring itself one run of an agent: the person's MCP server
//! lists the run while the session lasts.
//!
//! A run is the host's own action now: a session that is one declares it to the
//! host's `tilo.host` actions, and this session carries no word of its own.

use super::Task;
use tilo_session_actions::NOT_CONNECTED;

/// Where whether the session is now the agent's run goes ([`crate::Cmd::AgentSession`]).
pub(super) type AgentSessionCompletion = crate::Answer<Result<(), String>>;

impl Task {
    /// Always `true`: the session takes it whatever the answer.
    pub(super) fn session_as_agent(&mut self, label: String, done: AgentSessionCompletion) -> bool {
        // A session is one agent's run: the first is the one kept.
        self.agent_session.get_or_insert(label);
        // Declaring the run is the host's action, which this session does not carry: the run is
        // recorded here for whoever asks the host on its behalf.
        let _ = done.send(Err(NOT_CONNECTED.into()));
        true
    }
}
