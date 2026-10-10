//! Host action authority is separate from a record's participation.
use super::{Actor, PermissionContext, Principal, ResourceRef, UserId};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
pub(super) struct ActionAuthoritySampler {
    pub(super) app_id: String,
    sample: Arc<dyn Fn() -> Result<PermissionContext, String> + Send + Sync>,
    deadline_unix_ms: Arc<AtomicU64>,
    watches_native_projection: bool,
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
        self.app_id == other.app_id
            && Arc::ptr_eq(&self.sample, &other.sample)
            && Arc::ptr_eq(&self.deadline_unix_ms, &other.deadline_unix_ms)
            && self.watches_native_projection == other.watches_native_projection
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

    pub(super) fn deadline_unix_ms(&self) -> Option<u64> {
        match self.deadline_unix_ms.load(Ordering::Acquire) {
            0 => None,
            deadline => Some(deadline),
        }
    }
}

impl PermissionContext {
    /// Borrow host-admitted person authority for this app's Admin actions.
    /// Other verbs intersect current policy with original participation: a
    /// host projection can narrow their admission, but can never upgrade it.
    #[must_use]
    pub(crate) fn with_action_authority(
        mut self,
        app_id: String,
        sample: Arc<dyn Fn() -> Result<PermissionContext, String> + Send + Sync>,
    ) -> Self {
        self.action_authority = Some(ActionAuthoritySampler {
            app_id,
            sample,
            deadline_unix_ms: Arc::new(AtomicU64::new(0)),
            watches_native_projection: false,
        });
        self
    }

    pub(crate) fn with_live_action_authority(
        self,
        app_id: String,
        sample: Arc<dyn Fn() -> Result<PermissionContext, String> + Send + Sync>,
        deadline_unix_ms: Arc<AtomicU64>,
    ) -> Self {
        let mut current = self.with_action_authority(app_id, sample);
        if let Some(authority) = current.action_authority.as_mut() {
            authority.deadline_unix_ms = deadline_unix_ms;
            authority.watches_native_projection = true;
        }
        current
    }

    pub(crate) fn action_authority_deadline_unix_ms(&self) -> Option<u64> {
        self.action_authority
            .as_ref()
            .and_then(ActionAuthoritySampler::deadline_unix_ms)
    }

    pub(crate) fn has_live_action_authority(&self) -> bool {
        self.action_authority
            .as_ref()
            .is_some_and(|authority| authority.watches_native_projection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{CapDecision, Role, Verb};

    #[test]
    fn current_app_authority_narrows_but_never_upgrades_participation() {
        use crate::permissions::{CapOverride, Effect, Scope, Subject};
        use std::sync::Mutex;

        let app = "so.tilo.chat";
        let own = ResourceRef::App(app.into());
        let action = ResourceRef::Action {
            app: app.into(),
            resource: "conversation".into(),
            action: "send".into(),
        };
        let original = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Owner), "person-a").in_room("room-a"),
            vec![],
        );
        let current = Arc::new(Mutex::new(Ok(original.clone())));
        let sampler = {
            let current = current.clone();
            Arc::new(move || current.lock().unwrap().clone())
        };
        let author = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Editor), "person-a"),
            vec![],
        )
        .with_action_authority(app.into(), sampler.clone());
        let viewer = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Viewer), "person-a"),
            vec![],
        )
        .with_action_authority(app.into(), sampler.clone());
        assert_eq!(author.decide(&own, Verb::Write), CapDecision::Allow);
        assert_eq!(author.decide(&action, Verb::Write), CapDecision::Allow);
        assert_ne!(viewer.decide(&own, Verb::Write), CapDecision::Allow);
        assert_ne!(viewer.decide(&action, Verb::Write), CapDecision::Allow);

        let ceiling = CapOverride::new(
            Subject::person("person-a"),
            Scope::App(app.into()),
            Effect::Restrict,
            Verb::Read,
        );
        *current.lock().unwrap() = Ok(original.clone().granting(ceiling.clone()));
        assert_ne!(author.decide(&own, Verb::Write), CapDecision::Allow);
        assert_ne!(author.decide(&action, Verb::Write), CapDecision::Allow);
        assert_eq!(author.decide(&own, Verb::Read), CapDecision::Allow);
        assert_eq!(
            author.decide(&ResourceRef::Workspace, Verb::Write),
            CapDecision::Allow
        );
        assert_eq!(
            author.decide(&ResourceRef::App("so.tilo.desktop".into()), Verb::Write),
            CapDecision::Allow
        );

        // An accepted host owner policy cannot remove a launch-time ceiling.
        let capped = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Editor), "person-a"),
            vec![ceiling],
        )
        .with_action_authority(app.into(), sampler);
        *current.lock().unwrap() = Ok(original);
        assert_eq!(author.decide(&own, Verb::Write), CapDecision::Allow);
        assert_ne!(viewer.decide(&own, Verb::Write), CapDecision::Allow);
        assert_ne!(capped.decide(&own, Verb::Write), CapDecision::Allow);

        *current.lock().unwrap() = Err("projection unavailable".into());
        assert_ne!(author.decide(&own, Verb::Write), CapDecision::Allow);
        assert_ne!(author.decide(&action, Verb::Write), CapDecision::Allow);
        assert_ne!(author.decide(&own, Verb::Read), CapDecision::Allow);
        assert_eq!(
            author.decide(&ResourceRef::Workspace, Verb::Write),
            CapDecision::Allow
        );
    }

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
