//! The grants a host without a daemon decides a window's actions with (a harness, a shot, a
//! test): the workspace's policy, decided by the policy's own function at the node the dispatch
//! decides at ([`ShownAction::node`]), as the daemon decides them.

use super::{
    ActionVerdict, Answer, KindSet, LOCAL_USER, LOCAL_WORKSPACE, MemberAccess, MemberId, Role,
    ShownAction, TreePath, policy,
};

/// What a person reads when a harness host refuses an action.
const REFUSED: &str = "You aren't allowed to change this.";
/// What a person reads when the workspace holding a record is not kept by the harness host.
const NOT_HERE: &str =
    "This is kept in a workspace this device doesn't have, so it can't be changed here.";

/// A person's member access in one workspace, deciding as the daemon's dispatch would.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostGrants {
    access: MemberAccess,
    /// The person's access in the other workspaces this host keeps, which keep records the
    /// window shows (a person's profile record is kept in the person's workspace).
    kept: Vec<MemberAccess>,
    /// The instant grants are judged at, Unix seconds; the clock's when `None`.
    now: Option<u64>,
}

impl HostGrants {
    /// Decide as `access` says.
    #[must_use]
    pub fn new(access: MemberAccess) -> Self {
        Self {
            access,
            kept: Vec::new(),
            now: None,
        }
    }

    /// The owner of [`LOCAL_WORKSPACE`], [`LOCAL_USER`].
    #[must_use]
    pub fn owner() -> Self {
        Self::member(&MemberId::person(LOCAL_USER), Role::Owner)
    }

    /// `person` holding `role` in [`LOCAL_WORKSPACE`] (owning it for [`Role::Owner`]).
    #[must_use]
    pub fn member(person: &MemberId, role: Role) -> Self {
        Self::member_of(LOCAL_WORKSPACE, person, role)
    }

    /// `person` holding `role` in `workspace` (owning it for [`Role::Owner`]).
    #[must_use]
    pub fn member_of(workspace: &str, person: &MemberId, role: Role) -> Self {
        let person = person.id().to_owned();
        let policy = if role == Role::Owner {
            policy::WorkspacePolicy::new(workspace, person.clone())
        } else {
            let mut policy =
                policy::WorkspacePolicy::unowned(workspace, &policy::Grantee::role(Role::Owner));
            policy.members.insert(person.clone(), role);
            policy
        };
        Self::new(MemberAccess::person(policy, person))
    }

    /// `person`, who is no member of [`LOCAL_WORKSPACE`]: nothing is allowed.
    #[must_use]
    pub fn stranger(person: &MemberId) -> Self {
        Self::new(MemberAccess::person(
            policy::WorkspacePolicy::unowned(LOCAL_WORKSPACE, &policy::Grantee::role(Role::Owner)),
            person.id().to_owned(),
        ))
    }

    /// `person`, no member of [`LOCAL_WORKSPACE`], granted exactly `kinds` at `relative` (a
    /// path below the workspace), until Unix second `until` if given.
    #[must_use]
    pub fn granted(
        person: &MemberId,
        relative: &TreePath,
        kinds: KindSet,
        until: Option<u64>,
    ) -> Self {
        let grant = policy::Grant::new(
            "local-grant",
            policy::Grantee::Member(person.clone()),
            TreePath::workspace(LOCAL_WORKSPACE).join(relative),
            kinds,
            Answer::Allow,
            policy::Grantee::person(LOCAL_USER),
        );
        let grant = match until {
            Some(at) => grant.with_lifetime(policy::Lifetime::Until(at.saturating_mul(1000))),
            None => grant,
        };
        Self::stranger(person).with_grant(grant)
    }

    /// The owner of [`LOCAL_WORKSPACE`] acting through the agent `agent`, granted exactly `kinds`
    /// at `relative` (a path below the workspace): the scope an invitation gave it.
    #[must_use]
    pub fn agent_granted(agent: &str, relative: &TreePath, kinds: KindSet) -> Self {
        let grant = policy::Grant::new(
            "local-grant",
            policy::Grantee::agent(agent),
            TreePath::workspace(LOCAL_WORKSPACE).join(relative),
            kinds,
            Answer::Allow,
            policy::Grantee::person(LOCAL_USER),
        );
        Self::owner().acting_as(policy::Actor::Member(MemberId::agent(agent)), vec![grant])
    }

    /// The same grants with `grant` added to the workspace's.
    #[must_use]
    pub fn with_grant(mut self, grant: policy::Grant) -> Self {
        std::sync::Arc::make_mut(&mut self.access.policy)
            .grants
            .push(grant);
        self
    }

    /// The same person acting through a delegation of `subject` carrying `grants`.
    #[must_use]
    pub fn acting_as(mut self, subject: policy::Actor, grants: Vec<policy::Grant>) -> Self {
        self.access = self.access.acting_as(subject, grants);
        self
    }

    /// The same host also holding the workspace `access` decides in, for the records the window
    /// shows that it keeps.
    #[must_use]
    pub fn holding(mut self, access: MemberAccess) -> Self {
        self.kept.push(access);
        self
    }

    /// Judge grants at Unix second `now` instead of the clock.
    #[must_use]
    pub fn at_unix_time(mut self, now: u64) -> Self {
        self.now = Some(now);
        self
    }

    /// The person deciding as.
    #[must_use]
    pub fn person(&self) -> &str {
        self.access.person_id().unwrap_or_default()
    }

    /// The member access deciding.
    #[must_use]
    pub fn access(&self) -> &MemberAccess {
        &self.access
    }

    /// What the dispatch would say of `action` now, in the workspace holding its record: refused
    /// for a workspace this host does not keep, and `None` without a clock.
    #[must_use]
    pub fn decide(&self, action: &ShownAction) -> Option<ActionVerdict> {
        let now = self.now.or_else(tilo_clock::try_unix_now)?;
        let access = match &action.workspace {
            None => &self.access,
            Some(workspace) => match self.kept.iter().find(|kept| kept.workspace() == workspace) {
                Some(kept) => kept,
                None => return Some(ActionVerdict::Refused(NOT_HERE.into())),
            },
        };
        let cx = policy::Context::at_secs(now);
        let node = action.node(&access.policy.node());
        let (answer, _) = policy::answer_at(
            &access.authority(),
            (&node, action.kind_needed, action.privileged),
            None,
            &cx,
        );
        Some(match answer {
            Answer::Allow => ActionVerdict::Allowed,
            Answer::Ask => ActionVerdict::AsksFirst,
            Answer::Deny => ActionVerdict::Refused(REFUSED.into()),
        })
    }
}
