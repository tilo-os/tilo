//! The workspace's agents: who works in it, what each may do and how it was
//! admitted, read from the workspace's own grants.
use super::*;

/// A named starting point for an agent in the workspace: the grants it receives
/// when admitted. Deliberately not a person's role.
pub use tilo_action_policy::roles::AgentPreset;

/// The preset's name, for the Agents pane.
pub fn agent_preset_label(preset: AgentPreset) -> String {
    t(match preset {
        AgentPreset::Observer => "workspace-settings-preset-observer",
        AgentPreset::Contributor => "workspace-settings-preset-contributor",
        AgentPreset::Operator => "workspace-settings-preset-operator",
        AgentPreset::Custom => "workspace-settings-preset-custom",
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
    /// "Agent" or "Service".
    pub badge: String,
    /// What it was admitted with and what it holds beside.
    pub detail: String,
}

/// The workspace's agents, each as the Agents tab reads it.
#[must_use]
pub fn agent_summaries(ws: &Workspace) -> Vec<AgentSummary> {
    let mut members = ws.agent_members_at(tilo_clock::unix_now());
    members.compact_sort_by(|a, b| a.0.cmp(&b.0));
    members
        .into_iter()
        .map(|(agent, kinds)| {
            let (name, badge, detail) = agent_summary_parts(ws, &agent, kinds);
            AgentSummary {
                agent,
                name,
                badge,
                detail,
            }
        })
        .collect()
}

/// The id prefix of the grant that admits `agent`: a named admission, or a
/// service member's membership, which is its admission and not a permission
/// it holds beside one.
fn admission_prefix(agent: &str) -> String {
    match agent.strip_prefix("service:") {
        Some(device) if tilo_desktop_model::service_member::service_device(agent).is_some() => {
            format!("service-member:{device}:")
        }
        _ => format!("agent-member:{agent}:"),
    }
}

/// An agent's name, its badge and the line about what it may do: the one
/// place the tab reads them.
pub(super) fn agent_summary_parts(
    ws: &Workspace,
    agent: &str,
    kinds: tilo_action_policy::KindSet,
) -> (String, String, String) {
    let now = tilo_clock::unix_now();
    let service = service_purpose(ws, agent, now);
    let name = match &service {
        Some(service) if !service.purpose.is_empty() => service.purpose.clone(),
        _ => short_id(agent),
    };
    let badge = t(if service.is_some() {
        "workspace-settings-badge-service"
    } else {
        "workspace-settings-badge-agent"
    });
    let detail = if let Some(service) = &service {
        service_member_detail(service)
    } else {
        let admission = admission_prefix(agent);
        let subject = tilo_action_policy::Subject::agent(agent);
        let grant_count = ws
            .policy
            .grants
            .iter()
            .filter(|grant| grant.subject == subject && !grant.id.as_str().starts_with(&admission))
            .count();
        t_args(
            "workspace-settings-agent-detail",
            &[
                ("admission", agent_admission_label(kinds).into()),
                (
                    "profile",
                    agent_preset_label(agent_authority_preset(ws, agent)).into(),
                ),
                ("count", grant_count.into()),
            ],
        )
    };
    (name, badge, detail)
}
