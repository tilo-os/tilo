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
    #[test]
    fn host_record_edits_meet_launch_participation_and_fresh_authority() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let record = TreePath::root()
            .app("so.tilo.store")
            .kind("host.probe")
            .resource("fixture")
            .action("edit");
        let outside = TreePath::root().app("so.tilo.store").action("buy");
        for launch_role in [Role::Owner, Role::Editor, Role::Viewer] {
            for host_role in [Role::Owner, Role::Editor, Role::Viewer] {
                let samples = Arc::new(AtomicUsize::new(0));
                let counter = samples.clone();
                let permissions = PermissionContext::member_of("room-a", "person-a", launch_role)
                    .with_action_authority(
                        "so.tilo.store".into(),
                        Arc::new(move || {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok(PermissionContext::member_of(
                                "room-a", "person-a", host_role,
                            ))
                        }),
                    );
                let expected = if launch_role == Role::Viewer || host_role == Role::Viewer {
                    Answer::Deny
                } else {
                    Answer::Allow
                };
                assert_eq!(permissions.answer(&record, ActionKind::Undoable), expected);
                assert_eq!(
                    samples.load(Ordering::SeqCst),
                    1,
                    "even a launch refusal samples current authority"
                );
                if host_role == Role::Owner {
                    assert_eq!(
                        permissions.answer(&outside, ActionKind::Outside),
                        Answer::Allow
                    );
                }
                if launch_role == Role::Viewer {
                    let other = TreePath::root().app("so.tilo.other").action("edit");
                    assert_eq!(
                        permissions.answer(&other, ActionKind::Undoable),
                        Answer::Deny
                    );
                }
            }
        }
    }

    #[test]
    fn record_ceiling_preserves_fresh_revocation_and_unavailable_authority_refusal() {
        use std::sync::Mutex;
        let record = TreePath::root().app("so.tilo.store").action("edit");
        for launch_role in [Role::Owner, Role::Editor] {
            let current = Arc::new(Mutex::new(Some(Role::Editor)));
            let source = current.clone();
            let permissions = PermissionContext::member_of("room-a", "person-a", launch_role)
                .with_action_authority(
                    "so.tilo.store".into(),
                    Arc::new(move || {
                        let role = source.lock().unwrap().ok_or("host authority unavailable")?;
                        Ok(PermissionContext::member_of("room-a", "person-a", role))
                    }),
                );
            assert_eq!(
                permissions.answer(&record, ActionKind::Undoable),
                Answer::Allow
            );
            *current.lock().unwrap() = Some(Role::Viewer);
            assert_eq!(
                permissions.answer(&record, ActionKind::Undoable),
                Answer::Deny
            );
            *current.lock().unwrap() = Some(Role::Editor);
            assert_eq!(
                permissions.answer(&record, ActionKind::Undoable),
                Answer::Allow
            );
            *current.lock().unwrap() = None;
            assert_eq!(
                permissions.answer(&record, ActionKind::Undoable),
                Answer::Deny
            );
        }
    }
    #[test]
    fn record_ceiling_retains_launch_asks_and_privileged_refusal() {
        let record = TreePath::root().app("so.tilo.store").action("edit");
        let mut launch = PermissionContext::member_of("room-a", "person-a", Role::Editor);
        let mut standing = launch.standing().clone();
        standing.policy.grants.push(policy::Grant::new(
            "ask-record-edit",
            policy::Subject::person("person-a"),
            TreePath::room("room-a").joined(&record),
            policy::KindSet::UNDOABLE,
            Answer::Ask,
            policy::Subject::person("owner"),
        ));
        launch = launch.with_standing(standing).with_action_authority(
            "so.tilo.store".into(),
            Arc::new(|| {
                Ok(PermissionContext::member_of(
                    "room-a",
                    "person-a",
                    Role::Owner,
                ))
            }),
        );
        assert_eq!(launch.answer(&record, ActionKind::Undoable), Answer::Ask);
        assert_eq!(
            launch
                .verdict_for(&record, ActionKind::Undoable, true, false, |v| v.answer)
                .unwrap(),
            Answer::Deny
        );
    }

    #[test]
    fn shared_once_grant_is_spent_once_and_cannot_authorize_a_replay() {
        use std::sync::Mutex;
        let resource = tilo_actions::Resource::new("fixture", "host.probe", "Fixture");
        let action = tilo_actions::ActionSpec::immediate_encoded(
            "edit",
            "Edit",
            tilo_actions::ExecutionLocus::ClientLocal,
            tilo_actions::EncodedAction::written("fixture.edit".into(), Vec::new()),
        );
        let node = TreePath::room("room-a")
            .app("so.tilo.store")
            .kind("host.probe")
            .resource("fixture")
            .action("edit");
        let base = PermissionContext::member_of("room-a", "person-a", Role::Viewer);
        let mut standing = base.standing().clone();
        standing.policy.grants.push(
            policy::Grant::new(
                "edit-once",
                policy::Subject::person("person-a"),
                node,
                policy::KindSet::UNDOABLE,
                Answer::Allow,
                policy::Subject::person("owner"),
            )
            .with_lifetime(policy::Lifetime::Once),
        );
        let launch = base.with_standing(standing);
        let sampled = launch.clone();
        let consumed = Arc::new(Mutex::new(Vec::new()));
        let receipt = consumed.clone();
        let mut permissions = launch
            .with_action_authority(
                "so.tilo.store".into(),
                Arc::new(move || Ok(sampled.clone())),
            )
            .with_once_spender(Arc::new(move |ids| {
                receipt.lock().unwrap().extend_from_slice(ids);
                Ok(())
            }));
        assert_eq!(
            permissions.answer(
                &TreePath::root()
                    .app("so.tilo.store")
                    .kind("host.probe")
                    .resource("fixture")
                    .action("edit"),
                ActionKind::Undoable
            ),
            Answer::Deny
        );
        let (answer, spent) =
            permissions.invocation_verdict("so.tilo.store", &resource, &action, None, None);
        assert_eq!(answer, Answer::Allow);
        assert_eq!(spent, vec![policy::GrantId::new("edit-once")]);
        permissions.spend_once(&spent).unwrap();
        assert_eq!(*consumed.lock().unwrap(), spent);
        let (answer, spent) =
            permissions.invocation_verdict("so.tilo.store", &resource, &action, None, None);
        assert_eq!(answer, Answer::Deny);
        assert!(spent.is_empty());
    }
}
