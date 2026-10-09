//! Current canonical workspace policy shared by all broker hosts.
use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};
use collab_types::role::Role as CapRole;
use tilo_desktop_model::{UserId, Workspace, WorkspaceEffectReceipt};
pub trait WorkspacePolicy {
    fn grant_capability(&mut self, cap: CapOverride) -> String;
    fn revoke_capability(&mut self, id: &str) -> bool;
    fn set_owner(&mut self, user: impl Into<UserId>);
    fn decide(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> CapDecision;
    fn decide_at(&self, actor: &Actor, resource: &ResourceRef, verb: Verb, now: u64)
    -> CapDecision;
    fn allows(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> bool;
    fn may_administer(&self, user: &UserId) -> bool;
    fn role_of(&self, user: &UserId) -> Option<CapRole>;
    fn role_of_at(&self, user: &UserId, now: u64) -> Option<CapRole>;
    fn actor_for_person(&self, user: &UserId) -> Actor;
    fn actor_for_person_at(&self, user: &UserId, now: u64) -> Actor;
    /// [`Self::actor_for_person_at`], held to the foreign-device cap when
    /// `capped`.
    fn actor_for_person_on_device_at(&self, user: &UserId, capped: bool, now: u64) -> Actor;
    fn actor_for_app(&self, app_id: &str) -> Actor;
    fn actor_for_agent(&self, agent_id: &str) -> Actor;
    fn roster(&self) -> Vec<(UserId, CapRole)>;
    fn role_of_agent(&self, agent: &str) -> Option<CapRole>;
    fn role_of_agent_at(&self, agent: &str, now: u64) -> Option<CapRole>;
    fn agent_members(&self) -> Vec<(String, CapRole)>;
    fn shares_of(&self, resource: &ResourceRef) -> Vec<&CapOverride>;
}

impl WorkspacePolicy for Workspace {
    fn grant_capability(&mut self, cap: CapOverride) -> String {
        Workspace::grant_capability(self, cap)
    }
    fn revoke_capability(&mut self, id: &str) -> bool {
        let before = self.capabilities.len();
        self.capabilities.retain(|c| c.id != id);
        self.capabilities.len() != before
    }
    fn set_owner(&mut self, user: impl Into<UserId>) {
        let user = user.into();
        let owner_capability_id = format!("owner-{}", self.id.raw());
        // The owner capability has a stable id because the replicated map is
        // keyed by capability id.  Remove its previous in-memory value before
        // changing the subject: `grant_capability` deliberately merges only
        // identical subjects, and would otherwise leave the former owner with
        // a second local Admin grant until the next document rebuild.
        self.revoke_capability(&owner_capability_id);
        self.owner = Some(user.clone());
        self.grant_capability(
            CapOverride::membership(
                Subject::Principal(Principal::Person(user)),
                self.room.to_string(),
                Verb::Admin,
            )
            .with_id(owner_capability_id),
        );
    }
    fn decide(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> CapDecision {
        self.decide_at(actor, resource, verb, tilo_clock::unix_now())
    }
    fn decide_at(
        &self,
        actor: &Actor,
        resource: &ResourceRef,
        verb: Verb,
        now: u64,
    ) -> CapDecision {
        decide_capability(actor, resource, verb, &self.capabilities, Some(now))
    }
    fn allows(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> bool {
        self.decide(actor, resource, verb) == CapDecision::Allow
    }
    fn may_administer(&self, user: &UserId) -> bool {
        if self.owner.as_deref() == Some(user.as_str()) {
            return true;
        }
        self.allows(
            &self.actor_for_person(user),
            &ResourceRef::Workspace,
            Verb::Admin,
        )
    }
    fn role_of(&self, user: &UserId) -> Option<CapRole> {
        self.role_of_at(user, tilo_clock::unix_now())
    }
    fn role_of_at(&self, user: &UserId, now: u64) -> Option<CapRole> {
        tilo_peer_state::room::role_of(self.owner.as_deref(), &self.capabilities, user, now)
    }
    fn actor_for_person(&self, user: &UserId) -> Actor {
        self.actor_for_person_at(user, tilo_clock::unix_now())
    }
    fn actor_for_person_at(&self, user: &UserId, now: u64) -> Actor {
        Actor::person(self.role_of_at(user, now), user.clone()).in_room(self.room.to_string())
    }
    fn actor_for_person_on_device_at(&self, user: &UserId, capped: bool, now: u64) -> Actor {
        let actor = WorkspacePolicy::actor_for_person_at(self, user, now);
        if capped {
            collab_types::foreign_device::on_foreign_device(actor, &self.capabilities, now)
        } else {
            actor
        }
    }
    fn actor_for_app(&self, app_id: &str) -> Actor {
        Actor::app(app_id).in_room(self.room.to_string())
    }
    fn actor_for_agent(&self, agent_id: &str) -> Actor {
        Actor::agent(agent_id).in_room(self.room.to_string())
    }
    fn roster(&self) -> Vec<(UserId, CapRole)> {
        let now = Some(tilo_clock::unix_now());
        let mut seen: Vec<UserId> = Vec::new();
        for c in &self.capabilities {
            if !c.is_live_at(now) || c.effect != Effect::Grant {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            if let Subject::Principal(Principal::Person(p)) = &c.subject
                && !seen.contains(p)
            {
                seen.push(p.clone());
            }
        }
        if let Some(owner) = &self.owner
            && !seen.contains(owner)
        {
            seen.insert(0, owner.clone());
        }
        seen.into_iter()
            .filter_map(|u| self.role_of(&u).map(|r| (u, r)))
            .collect()
    }
    fn role_of_agent(&self, agent: &str) -> Option<CapRole> {
        self.role_of_agent_at(agent, tilo_clock::unix_now())
    }
    fn role_of_agent_at(&self, agent: &str, now: u64) -> Option<CapRole> {
        let now = Some(now);
        let mut best: Option<Verb> = None;
        let mut blocked = false;
        for c in &self.capabilities {
            let subject_matches = matches!(
                &c.subject,
                Subject::Principal(Principal::Agent(a)) if a == agent
            );
            if !subject_matches || !c.is_live_at(now) {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            match c.effect {
                Effect::Grant => {
                    best = Some(match best {
                        Some(prev) if prev.implies(c.verb) => prev,
                        _ => c.verb,
                    });
                }
                Effect::Restrict if !c.verb.implies(Verb::Read) => blocked = true,
                Effect::Restrict => {}
            }
        }
        if blocked {
            return Some(CapRole::Denied);
        }
        best.map(|v| match v {
            Verb::Admin => CapRole::Owner,
            Verb::Write => CapRole::Editor,
            Verb::Comment => CapRole::Commenter,
            Verb::Read | Verb::Invite => CapRole::Viewer,
        })
    }
    fn agent_members(&self) -> Vec<(String, CapRole)> {
        let now = Some(tilo_clock::unix_now());
        let mut seen: Vec<String> = Vec::new();
        for c in &self.capabilities {
            if !c.is_live_at(now) || c.effect != Effect::Grant {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            if let Subject::Principal(Principal::Agent(a)) = &c.subject
                && !seen.contains(a)
            {
                seen.push(a.clone());
            }
        }
        seen.into_iter()
            .filter_map(|a| self.role_of_agent(&a).map(|r| (a, r)))
            .collect()
    }
    fn shares_of(&self, resource: &ResourceRef) -> Vec<&CapOverride> {
        let now = Some(tilo_clock::unix_now());
        self.capabilities
            .iter()
            .filter(|c| {
                c.is_live_at(now)
                    && c.effect == Effect::Grant
                    && c.subject_is_person()
                    && !c.is_room_membership()
                    && c.scope.matches_resource(resource)
            })
            .collect()
    }
}
fn bounded_receipt_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

fn redact_receipt_context(context: &mut tilo_actions::ActionContext) {
    if let Some(item) = &mut context.item {
        item.id = bounded_receipt_text(&item.id, 512);
        item.label = bounded_receipt_text(&item.label, 200);
    }
    // What the effect was is public; what it was given is not.
    if let Some(what) = &mut context.what {
        what.forget_args();
    }
    if let Some(app) = &mut context.app {
        app.id = bounded_receipt_text(&app.id, 160);
        app.label = bounded_receipt_text(&app.label, 160);
    }
    if let Some(resource) = &mut context.resource {
        resource.id = bounded_receipt_text(&resource.id, 512);
        resource.kind = bounded_receipt_text(&resource.kind, 160);
        resource.label = bounded_receipt_text(&resource.label, 200);
    }
    context.purpose = context
        .purpose
        .as_deref()
        .map(|value| bounded_receipt_text(value, 512))
        .filter(|value| !value.is_empty());
    if let Some(group) = &mut context.group {
        group.id = bounded_receipt_text(&group.id, 160);
        group.label = bounded_receipt_text(&group.label, 200);
        group.kind = bounded_receipt_text(&group.kind, 160);
    }
    context.facets.truncate(32);
    for facet in &mut context.facets {
        facet.key = bounded_receipt_text(&facet.key, 96);
        facet.value = bounded_receipt_text(&facet.value, 160);
    }
}

/// Construct the same bounded public receipt for native and standalone owners.
/// No invocation arguments, output, streamed items or private failure text enter it.
pub fn redacted_effect_receipt(
    invocation: tilo_actions::InvocationId,
    actor: &str,
    mut context: tilo_actions::ActionContext,
    status: tilo_actions::EffectReceiptStatus,
) -> WorkspaceEffectReceipt {
    let completed_at = tilo_clock::unix_now_i64();
    // Arbitrary application facets may contain private argument summaries.
    // Only broker-owned terminal identifiers enter this public audit.
    context.facets.clear();
    context
        .facets
        .push(tilo_actions::ActionFacet::new("tilo.receipt", "effect"));
    context.facets.push(tilo_actions::ActionFacet::new(
        "tilo.invocation",
        invocation.0.to_string(),
    ));
    redact_receipt_context(&mut context);
    WorkspaceEffectReceipt {
        invocation,
        actor: bounded_receipt_text(actor, 160),
        context,
        status,
        completed_at,
    }
}

#[cfg(test)]
mod retained_clock_tests {
    use super::*;
    #[test]
    fn retained_policy_clock_samples_membership_and_grant_at_same_time() {
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(2), "Timed room");
        let user = "member".to_string();
        workspace.grant_capability(
            CapOverride::membership(
                Subject::person(&user),
                workspace.room.to_string(),
                Verb::Read,
            )
            .with_expiry(100),
        );
        workspace.grant_capability(
            CapOverride::grant(
                Subject::person(&user),
                Scope::App("app".into()),
                Verb::Write,
            )
            .with_expiry(100),
        );
        let before = workspace.actor_for_person_at(&user, 50);
        assert_eq!(workspace.role_of_at(&user, 50), Some(CapRole::Viewer));
        assert_eq!(
            workspace.decide_at(&before, &ResourceRef::App("app".into()), Verb::Write, 50),
            CapDecision::Allow
        );
        assert_eq!(workspace.role_of_at(&user, 101), None);
        assert_ne!(
            workspace.decide_at(
                &workspace.actor_for_person_at(&user, 101),
                &ResourceRef::App("app".into()),
                Verb::Write,
                101
            ),
            CapDecision::Allow
        );
    }
}

#[cfg(test)]
mod effect_receipt_redaction_tests {
    #[test]
    fn only_broker_owned_terminal_facets_enter_public_receipts() {
        let invocation = tilo_actions::InvocationId::new();
        let context = tilo_actions::ActionContext {
            facets: vec![
                tilo_actions::ActionFacet::new("private.argument", "secret text"),
                tilo_actions::ActionFacet::new("tilo.invocation", "forged"),
                tilo_actions::ActionFacet::new("tilo.receipt", "forged"),
            ],
            ..Default::default()
        };
        let receipt = super::redacted_effect_receipt(
            invocation,
            "person",
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
        );
        assert_eq!(
            receipt.context.facets,
            vec![
                tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
                tilo_actions::ActionFacet::new("tilo.invocation", invocation.0.to_string()),
            ]
        );
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains("secret text")
        );
    }
}
