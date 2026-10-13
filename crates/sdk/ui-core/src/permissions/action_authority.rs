//! The host's live view of a person's authority over one app's actions.
use super::{PermissionContext, TreePath, UserId};

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
    /// Whether `node` (below the room) is one of this app's actions.
    pub(super) fn matches(&self, node: &TreePath) -> bool {
        TreePath::root().app(self.app_id.clone()).covers(node)
    }

    /// The host's current view, for `asker` when it is exactly the person the
    /// view belongs to, acting as themselves: never borrowed by another person
    /// or by an agent acting for them.
    pub(super) fn sample_for(
        &self,
        asker: &PermissionContext,
    ) -> Result<PermissionContext, String> {
        if !asker.standing.bounds.is_empty() {
            return Err("host action authority is not a delegation's".into());
        }
        let person: &UserId = &asker.user;
        let current = (self.sample)()?;
        if current.user != *person
            || current.standing.person != person.0
            || !current.standing.bounds.is_empty()
            || current.action_authority.is_some()
        {
            return Err("host action authority binding mismatch".into());
        }
        Ok(current)
    }
}

impl PermissionContext {
    /// Decide this app's actions by the host's live view of the person's
    /// authority, resampled at every decision.
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
    use crate::permissions::{ActionKind, Answer, Role, policy};

    #[test]
    fn host_action_authority_cannot_be_borrowed_by_agents_or_other_people() {
        let own = TreePath::root().app("so.tilo.store").action("buy");
        let authority = || PermissionContext::member_of("room-a", "person-a", Role::Owner);
        let agent = PermissionContext::member_of("room-a", "person-a", Role::Editor);
        let bounded = agent.clone().with_standing(
            agent
                .standing()
                .clone()
                .bounded_by(policy::Subject::agent("agent-a"), Vec::new()),
        );
        for permissions in [
            bounded,
            PermissionContext::member_of("room-a", "person-b", Role::Editor),
        ] {
            let permissions = permissions
                .with_action_authority("so.tilo.store".into(), Arc::new(move || Ok(authority())));
            assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
            assert!(permissions.directory_witness().is_err());
        }
        let permissions = PermissionContext::member_of("room-a", "person-a", Role::Editor)
            .with_action_authority("so.tilo.store".into(), Arc::new(move || Ok(authority())));
        assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Allow);
        let permissions =
            permissions.with_policy_time_sampler(Arc::new(|| Err("clock unavailable".into())));
        assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
        assert!(permissions.directory_witness().is_err());
    }
}
