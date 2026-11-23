//! A session declaring itself one run of an agent: the person's MCP server
//! lists the run while the session lasts.
//!
//! The run is the link the session holds to its host: the host mints one
//! execution for each link it answers, so every request the session makes is
//! that one run, and the run ends when the link does. The name the session
//! declares is kept here, for whatever asks the session what run it is; the
//! host needs no word of its own for it, and there is none to send.

use super::Task;

/// Where whether the session is now the agent's run goes ([`crate::Cmd::AgentSession`]).
pub(super) type AgentSessionCompletion = crate::Answer<Result<(), String>>;

impl Task {
    /// Always `true`: the session takes it whatever the answer.
    pub(super) fn session_as_agent(&mut self, label: String, done: AgentSessionCompletion) -> bool {
        // A session is one agent's run: the first is the one kept.
        self.agent_session.get_or_insert(label);
        // The run is the link this session holds, and the host mints its execution there: no
        // other session can be the same run, so the host has nothing to refuse.
        let _ = done.send(Ok(()));
        true
    }
}
