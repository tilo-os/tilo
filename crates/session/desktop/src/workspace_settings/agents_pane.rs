//! The Agents tab's rows: who works in the workspace, what it may do and
//! how it was admitted, read from the workspace's own grants.
use super::*;

/// One explicitly non-human row in the Agents pane. Keeping this a
/// separate type makes it hard for a future renderer to accidentally
/// present agents as human roster entries.
#[derive(Clone, Debug)]
pub struct AgentRow {
    pub agent: String,
    /// The kinds of action its room grants allow.
    pub kinds: tilo_action_policy::KindSet,
    pub grant_count: usize,
    /// A human-readable summary of the *scoped* authority, not a role.
    /// Agents never inherit their admission capability as ambient access.
    pub preset: AgentPreset,
    /// Set when the agent is a service member: a cloud or relay identity
    /// admitted to the room's encrypted channels with a stated purpose.
    pub service: Option<ServiceMember>,
    pub row_rect: Rect,
    pub role_rect: Rect,
    pub review_rect: Rect,
    pub remove_rect: Rect,
}

/// A named starting point for an agent in the room: the grants it receives
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

/// The preset `agent` was admitted with, from the ids of its room grants.
/// Anything broad or sensitive it holds beside them (the whole room, an
/// outside effect, a device, the network) makes it custom: that needs a
/// conscious review, never a reassuring preset.
pub fn agent_authority_preset(ws: &Workspace, agent: &str) -> AgentPreset {
    use tilo_action_policy::ActionKind;
    let prefix = format!("agent-member:{agent}:");
    let subject = tilo_action_policy::Subject::agent(agent);
    let room = ws.node();
    let broad_grant = ws.policy.grants.iter().any(|grant| {
        // An answer that asks first or denies never widens the agent.
        let narrowing = tilo_action_policy::agent_answers::is_answer(grant)
            && grant.answer != tilo_action_policy::Answer::Allow;
        grant.subject == subject
            && !grant.id.as_str().starts_with(&prefix)
            && !narrowing
            && grant.answer != tilo_action_policy::Answer::Deny
            && (grant.node.covers(&room) || grant.kinds.contains(ActionKind::Outside))
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

/// Admit `device` as a service member of this workspace: a cloud or relay
/// identity that reads and acts in the room only through `kinds`, with the
/// purpose `purpose` and, if it has one, the deadline `expires_at` (Unix
/// seconds). Returns the grant to revoke it by when the job ends, or `None`
/// when `actor` may not admit in this room.
pub fn admit_service_member(
    ws: &mut Workspace,
    actor: &UserId,
    device: &collab_types::Ed25519PublicKey,
    purpose: &str,
    kinds: tilo_action_policy::KindSet,
    expires_at: Option<u64>,
) -> Option<tilo_action_policy::GrantId> {
    let grant = tilo_desktop_model::service_member::service_membership(
        &ws.policy.room,
        device,
        purpose,
        kinds,
        expires_at,
        tilo_action_policy::Subject::person(actor.clone()),
    );
    let id = grant.id.clone();
    ws.govern_all_at(
        actor,
        vec![tilo_action_policy::Change::Grant(grant)],
        tilo_clock::unix_now(),
    )
    .ok()
    .map(|_| id)
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
                &ws.policy.room,
                &agent,
                &tilo_action_policy::Subject::Person(actor.clone()),
            )
            .into_iter()
            .map(tilo_action_policy::Change::Grant),
    );
    ws.govern_all_at(actor, changes, tilo_clock::unix_now())
        .unwrap_or_default()
}

/// One agent as the Agents tab lists it, in the person's words: without the
/// geometry the pane lays it out with.
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
/// place both the pane and the tab read them.
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

/// Geometry for the always-visible entry point into the guided setup.
pub fn agents_add_rect(pane_rect: Rect) -> Rect {
    Rect::new(
        pane_rect.x0 + PANE_PAD_X,
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
        pane_rect.x0 + PANE_PAD_X + 132.0,
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP + AGENT_ONBOARDING_HEIGHT,
    )
}

/// Stable, policy-backed rows for the Agents pane.
pub fn agents_pane_rows(ws: &Workspace, pane_rect: Rect) -> Vec<AgentRow> {
    let mut agents = ws.agent_members_at(tilo_clock::unix_now());
    agents.compact_sort_by(|a, b| a.0.cmp(&b.0));
    let mut rows = Vec::new();
    let mut cursor_y = pane_rect.y0
        + PANE_PAD_TOP
        + PANE_INTRO_HEIGHT
        + PANE_INTRO_GAP
        + AGENT_ONBOARDING_HEIGHT
        + SECTION_HEADER_HEIGHT;
    for (agent, kinds) in agents {
        let row_rect = Rect::new(
            pane_rect.x0 + PANE_PAD_X,
            cursor_y,
            pane_rect.x1 - PANE_PAD_X,
            cursor_y + ROW_HEIGHT,
        );
        if row_rect.y1 > pane_rect.y1 - PANE_PAD_BOTTOM {
            break;
        }
        let remove_rect = Rect::new(
            row_rect.x1 - 12.0 - REVOKE_WIDTH,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            row_rect.x1 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let role_rect = Rect::new(
            remove_rect.x0 - 12.0 - AGENT_REVIEW_WIDTH - 12.0 - 96.0,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            remove_rect.x0 - 12.0 - AGENT_REVIEW_WIDTH - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let review_rect = Rect::new(
            remove_rect.x0 - 12.0 - AGENT_REVIEW_WIDTH,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            remove_rect.x0 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let subject = tilo_action_policy::Subject::agent(agent.clone());
        let admission = admission_prefix(&agent);
        let grant_count = ws
            .policy
            .grants
            .iter()
            .filter(|grant| grant.subject == subject && !grant.id.as_str().starts_with(&admission))
            .count();
        let preset = agent_authority_preset(ws, &agent);
        let service = service_purpose(ws, &agent, tilo_clock::unix_now());
        rows.push(AgentRow {
            agent,
            kinds,
            grant_count,
            preset,
            service,
            row_rect,
            role_rect,
            review_rect,
            remove_rect,
        });
        cursor_y += ROW_HEIGHT + ROW_GAP;
    }
    rows
}
