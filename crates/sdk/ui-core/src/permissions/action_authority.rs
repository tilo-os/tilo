//! Host action authority is separate from a record's participation.
use super::{Actor, PermissionContext, Principal, ResourceRef, UserId};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct ActionAuthoritySampler {
    pub(super) app_id: String,
    sample: Arc<dyn Fn() -> Result<PermissionContext, String> + Send + Sync>,
}

impl std::fmt::Debug for ActionAuthoritySampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionAuthoritySampler")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}
impl PartialEq for ActionAuthoritySampler {
    fn eq(&self, other: &Self) -> bool {
        self.app_id == other.app_id && Arc::ptr_eq(&self.sample, &other.sample)
    }
}
impl Eq for ActionAuthoritySampler {}

impl ActionAuthoritySampler {
    pub(super) fn matches(&self, resource: &ResourceRef) -> bool {
        match resource {
            ResourceRef::App(app) | ResourceRef::Action { app, .. } => app == &self.app_id,
            _ => false,
        }
    }

    pub(super) fn sample_for(
        &self,
        person: &UserId,
        actor: &Actor,
    ) -> Result<PermissionContext, String> {
        let current = (self.sample)()?;
        let bound_person = |value: &Actor| {
            value
                .principals
                .iter()
                .filter_map(|principal| match principal {
                    Principal::Person(id) => Some(id.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                == vec![person.0.as_str()]
        };
        if !bound_person(actor)
            || !bound_person(&current.actor)
            || current.user != *person
            || current.action_authority.is_some()
            || actor
                .principals
                .iter()
                .any(|principal| !matches!(principal, Principal::Person(_) | Principal::Room(_)))
            || current
                .actor
                .principals
                .iter()
                .any(|principal| !matches!(principal, Principal::Person(_) | Principal::Room(_)))
        {
            return Err("host action authority binding mismatch".into());
        }
        Ok(current)
    }
}

impl PermissionContext {
    /// Borrow host-admitted person authority for this app's Admin actions.
    /// Record admission and all other verbs keep their original participation.
    #[must_use]
    pub(crate) fn with_action_authority(
        mut self,
        app_id: String,
        sample: Arc<dyn Fn() -> Result<PermissionContext, String> + Send + Sync>,
    ) -> Self {
        self.action_authority = Some(ActionAuthoritySampler { app_id, sample });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{CapDecision, Role, Verb};

    #[test]
    fn host_action_authority_cannot_be_borrowed_by_apps_agents_or_foreign_people() {
        let own = ResourceRef::App("so.tilo.store".into());
        let authority = || {
            PermissionContext::from_capabilities(
                Actor::person(Some(Role::Owner), "person-a").in_room("room-a"),
                vec![],
            )
        };
        for actor in [
            Actor::app("so.tilo.store"),
            Actor::agent("agent-a"),
            Actor::person(Some(Role::Editor), "person-b"),
            Actor {
                principals: vec![
                    Principal::Person("person-a".into()),
                    Principal::Person("person-b".into()),
                ],
                role: Some(Role::Editor),
                key: None,
            },
        ] {
            let permissions = PermissionContext::from_capabilities(actor, vec![])
                .with_action_authority("so.tilo.store".into(), Arc::new(move || Ok(authority())));
            assert_ne!(permissions.decide(&own, Verb::Admin), CapDecision::Allow);
            assert!(permissions.directory_witness().is_err());
        }
        let permissions = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Editor), "person-a"),
            vec![],
        )
        .with_action_authority("so.tilo.store".into(), Arc::new(move || Ok(authority())))
        .with_policy_time_sampler(Arc::new(|| Err("clock unavailable".into())));
        assert_ne!(permissions.decide(&own, Verb::Admin), CapDecision::Allow);
        assert!(permissions.directory_witness().is_err());
    }
}
