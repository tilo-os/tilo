//! The workspace's agents: who works in it, what each may do and how it was
//! admitted, read from the workspace's own grants.
use super::*;

/// A named starting point for an agent in the workspace: the grants it receives
/// when admitted. Deliberately not a person's role.
pub use tilo_action_policy::roles::AgentPreset;

/// What an agent may do, in one line a person reads without knowing the
/// presets: the most it does on its own, and what waits for the person.
pub fn agent_status(ws: &Workspace, agent: &str) -> String {
    t(match agent_authority_preset(ws, agent) {
        AgentPreset::Observer => "workspace-settings-agent-status-observer",
        AgentPreset::Contributor => "workspace-settings-agent-status-contributor",
        AgentPreset::Operator => "workspace-settings-agent-status-operator",
        AgentPreset::Custom => "workspace-settings-agent-status-custom",
    })
}

/// The preset `agent` was admitted with, from the ids of its workspace grants.
/// Anything broad or sensitive it holds beside them (the whole workspace, an
/// outside effect, a device, the network) makes it custom: that needs a
/// conscious review, never a reassuring preset.
pub fn agent_authority_preset(ws: &Workspace, agent: &str) -> AgentPreset {
    use tilo_action_policy::ActionKind;
    let prefix = format!("agent-member:{agent}:");
    let subject = tilo_action_policy::Subject::agent(agent);
    let node = ws.node();
    let broad_grant = ws.policy.grants.iter().any(|grant| {
        // An answer that asks first or denies never widens the agent.
        let narrowing = tilo_action_policy::agent_answers::is_answer(grant)
            && grant.answer != tilo_action_policy::Answer::Allow;
        grant.subject == subject
            && !grant.id.as_str().starts_with(&prefix)
            && !narrowing
            && grant.answer != tilo_action_policy::Answer::Deny
            && (grant.node.covers(&node) || grant.kinds.contains(ActionKind::Outside))
    });
    let broad_sandbox = ws.sandbox.iter().any(|cap| {
        cap.effect == Effect::Grant
            && matches!(&cap.subject, Subject::Principal(Principal::Agent(id)) if id == agent)
            && matches!(cap.scope, Scope::Device(_) | Scope::NetHost(_))
    });
    if broad_grant || broad_sandbox {
        return AgentPreset::Custom;
    }
    ws.policy
        .grants
        .iter()
        .filter_map(|grant| grant.id.as_str().strip_prefix(&prefix))
        .map(|rest| rest.split(':').next().unwrap_or_default())
        .find_map(AgentPreset::parse)
        .unwrap_or(AgentPreset::Custom)
}

/// Admit a named agent with the grants of `preset`, replacing the ones it
/// was admitted with before, as `actor`.
pub fn admit_agent(
    ws: &mut Workspace,
    actor: &UserId,
    agent: impl AsRef<str>,
    preset: AgentPreset,
) -> PolicyEdit {
    let agent = agent.as_ref().trim().to_ascii_lowercase();
    if agent.is_empty() {
        return PolicyEdit::default();
    }
    let prefix = format!("agent-member:{agent}:");
    let mut changes: Vec<tilo_action_policy::Change> = ws
        .policy
        .grants
        .iter()
        .filter(|grant| grant.id.as_str().starts_with(&prefix))
        .map(|grant| tilo_action_policy::Change::Revoke(grant.id.clone()))
        .collect();
    changes.extend(
        preset
            .grants(
                &ws.policy.workspace,
                &agent,
                &tilo_action_policy::Subject::Person(actor.clone()),
            )
            .into_iter()
            .map(tilo_action_policy::Change::Grant),
    );
    ws.govern_all_at(actor, changes, tilo_clock::unix_now())
        .unwrap_or_default()
}

/// One agent as the Agents tab lists it, in the person's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSummary {
    /// What it is asked for by.
    pub agent: String,
    /// What the person reads: the service's purpose, or the agent's name.
    pub name: String,
    /// Whether the workspace admitted it as a service, which is no agent.
    pub service: bool,
    /// What it may do, as one plain line: for a service, what it reads and
    /// until when.
    pub detail: String,
}

/// The workspace's agents, each as the Agents tab reads it.
#[must_use]
pub fn agent_summaries(ws: &Workspace) -> Vec<AgentSummary> {
    let mut members = ws.agent_members_at(tilo_clock::unix_now());
    members.compact_sort_by(|a, b| a.0.cmp(&b.0));
    members
        .into_iter()
        .map(|(agent, _)| {
            let (name, service, detail) = agent_summary_parts(ws, &agent);
            AgentSummary {
                agent,
                name,
                service,
                detail,
            }
        })
        .collect()
}

/// An agent's name, whether it is a service, and the line about what it may
/// do: the one place the tab reads them.
pub(super) fn agent_summary_parts(ws: &Workspace, agent: &str) -> (String, bool, String) {
    let now = tilo_clock::unix_now();
    let service = service_purpose(ws, agent, now);
    let name = match &service {
        Some(service) if !service.purpose.is_empty() => service.purpose.clone(),
        _ => short_id(agent),
    };
    let detail = service
        .as_ref()
        .map_or_else(|| agent_status(ws, agent), service_member_detail);
    (name, service.is_some(), detail)
}
