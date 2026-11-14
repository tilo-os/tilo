//! A session declaring itself one run of an agent: the person's agent door
//! lists the run while the session lasts.

use tilo_relay_protocol::Envelope;
use tilo_relay_session::RelayClient;

use super::Task;

/// Where whether the session is now the agent's run goes ([`crate::Cmd::AgentRun`]).
pub(super) type AgentRunCompletion = crate::Answer<Result<(), String>>;

impl Task {
    /// Always `true`: the session takes it whatever the answer.
    pub(super) fn run_as_agent(
        &mut self,
        label: String,
        done: AgentRunCompletion,
        client: Option<&RelayClient>,
    ) -> bool {
        // A session is one agent's run: the host refuses another, so the first is the one kept.
        self.agent_run.get_or_insert_with(|| label.clone());
        let Some(client) = client else {
            let _ = done.send(Err("not connected to the host".into()));
            return true;
        };
        let request_id = self.agent_runs_next;
        self.agent_runs_next = self.agent_runs_next.wrapping_add(1);
        self.agent_runs_pending.insert(request_id, done);
        if client
            .send(Envelope::AgentRun { request_id, label })
            .is_err()
            && let Some(done) = self.agent_runs_pending.remove(&request_id)
        {
            let _ = done.send(Err("the host's session closed".into()));
        }
        true
    }

    /// Say again on a new connection that this session is the run of the
    /// agent it declared: the host knows it per connection, and what a session
    /// that came back asked without it would be the person's. Its answer is
    /// nobody's to wait for. `false` when the session could not take it.
    pub(super) fn declare_agent_run_again(&mut self, client: &RelayClient) -> bool {
        let Some(label) = self.agent_run.clone() else {
            return true;
        };
        let request_id = self.agent_runs_next;
        self.agent_runs_next = self.agent_runs_next.wrapping_add(1);
        client
            .send(Envelope::AgentRun { request_id, label })
            .is_ok()
    }

    /// The host answered an agent run this session declared.
    pub(super) fn agent_run_answered(&mut self, answer: Envelope) {
        let Envelope::AgentRunAnswer {
            request_id,
            refusal,
        } = answer
        else {
            return;
        };
        if let Some(done) = self.agent_runs_pending.remove(&request_id) {
            let _ = done.send(refusal.map_or(Ok(()), Err));
        }
    }
}
