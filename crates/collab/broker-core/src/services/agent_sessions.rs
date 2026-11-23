//! The agents a workspace keeps, and the sessions they work in: an MCP client's connection.

/// The prefix of an agent session's id: the desktop names each session beneath it by an id of its
/// own.
pub const AGENT_SESSION_PREFIX: &str = "so.tilo.session/agent-session/";

/// Whether `id` names an agent session: [`AGENT_SESSION_PREFIX`] and one plain id.
#[must_use]
pub fn is_agent_session(id: &str) -> bool {
    id.strip_prefix(AGENT_SESSION_PREFIX).is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

/// The scope a session's question asks its person's answer under: the exact action where the step
/// shows something for good, the whole kind where it does not.
#[must_use]
pub fn session_answer_scope(
    app: &str,
    (kind, action): (&str, &str),
    consequential: bool,
) -> tilo_action_policy::agent_answers::AnswerScope {
    if consequential {
        tilo_action_policy::agent_answers::AnswerScope::action(app, kind, action)
    } else {
        tilo_action_policy::agent_answers::AnswerScope::kind(app, kind)
    }
}

/// The id of the grant keeping the person's answer for actions of `kind` at `scope` in `session`:
/// one per question, so answering it again replaces it.
#[must_use]
pub fn session_answer_id(
    session: &str,
    (scope, kind): (
        &tilo_action_policy::agent_answers::AnswerScope,
        tilo_action_policy::ActionKind,
    ),
) -> String {
    format!(
        "{}{session}:{}:{kind}",
        tilo_action_policy::SESSION_ANSWER_PREFIX,
        scope.key()
    )
}

/// One agent member as the workspace's record of its agents names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedAgent {
    pub id: String,
    pub name: String,
    /// The person the agent acts for.
    pub acts_for: String,
}

/// The agents a workspace keeps, as its own record of them names them (`chat.agent`): an agent
/// works here because the record names the person it acts for, and that person's session its
/// device joins on.
pub trait AgentRoster: Send + Sync {
    /// The person `agent` acts for in `workspace`, while the workspace's
    /// record names it active there; `None` for an agent it does not name.
    fn acts_for(&self, workspace: &str, agent: &str) -> Option<String> {
        self.listed(workspace)
            .into_iter()
            .find(|listed| listed.id == agent)
            .map(|listed| listed.acts_for)
    }
    /// Whether the workspace's record names `agent` active in `workspace`.
    fn lists(&self, workspace: &str, agent: &str) -> bool {
        self.acts_for(workspace, agent).is_some()
    }
    /// The active agents the workspace's record names: its agent members.
    fn listed(&self, workspace: &str) -> Vec<ListedAgent>;
    /// What `agent` may spend on the day its person counts: the limit its
    /// person set and what it spent of it. `None` where this host reads no
    /// such record, and then the agent works unmetered.
    fn spend(&self, workspace: &str, agent: &str) -> Option<runtime_collab::AgentSpend> {
        let _ = (workspace, agent);
        None
    }
}

/// The agent sessions a host serves: each MCP connection the person admitted, for as long as it
/// lasts.
pub trait AgentSessions: Send + Sync {
    /// The session `session` of `agent` in `workspace`, while it goes.
    fn going(&self, workspace: &str, agent: &str, session: &str) -> Option<GoingSession>;
    /// Keep `grant`, the person's "allow" for every later step like the one
    /// `session` asked about, for as long as the session lasts.
    fn keep_answer(&self, session: &str, grant: tilo_action_policy::Grant);
}

/// An agent session while it goes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GoingSession {
    /// The other device of the person's that the connection came from; `None`
    /// for a program on this device.
    pub origin: Option<tilo_actions::InvokerDevice>,
    /// What the person allowed it for every later step like the one it asked about.
    pub answers: Vec<tilo_action_policy::Grant>,
}
