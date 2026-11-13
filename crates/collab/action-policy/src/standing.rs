//! One acting's standing in one workspace, as owned data a host hands to a door
//! or an app: who acted, the workspace's policy, and the grants the host made
//! for the acting's software.

use serde::{Deserialize, Serialize};

use crate::{
    Acting, ActionKind, Answer, Authority, Context, Grant, TreePath, Verdict, WorkspacePolicy,
    roles,
};

/// Who made a change and what they may do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub acting: Acting,
    /// Shared, not copied: a host keeps every member device's standing in a
    /// workspace, and each holds the workspace's whole policy, so a copy each
    /// made one more member cost a copy of every member's policy. Encoded as
    /// the policy itself.
    pub policy: std::sync::Arc<WorkspacePolicy>,
    /// The grants the host made for the acting's software nodes: an agent's
    /// turn, an automation's steps.
    #[serde(default)]
    pub granted: Vec<Grant>,
    /// The resource kinds the app in `acting` declares: what it may reach in
    /// its own node. `None` where the host does not hold its manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_kinds: Option<Vec<String>>,
}

impl Standing {
    /// `acting` in the workspace `policy` governs.
    #[must_use]
    pub fn new(policy: impl Into<std::sync::Arc<WorkspacePolicy>>, acting: Acting) -> Self {
        Self {
            acting,
            policy: policy.into(),
            granted: Vec::new(),
            app_kinds: None,
        }
    }

    /// A person acting as themselves.
    #[must_use]
    pub fn person(
        policy: impl Into<std::sync::Arc<WorkspacePolicy>>,
        person: impl Into<String>,
    ) -> Self {
        Self::new(policy, Acting::person(person))
    }

    /// The same person, acting as `actor` for them, carrying `grants`.
    #[must_use]
    pub fn acting_as(
        mut self,
        actor: crate::Actor,
        grants: impl IntoIterator<Item = Grant>,
    ) -> Self {
        let person = self.person_id().map(str::to_owned);
        let foreign_device = self.acting.foreign_device;
        self.acting = match (&actor, person) {
            (crate::Actor::Member(crate::MemberId::Person(_)) | _, Some(person)) => {
                Acting::from(actor).acting_for(Acting::person(person))
            }
            (crate::Actor::Member(crate::MemberId::Person(_)) | _, None) => actor.into(),
        };
        self.acting.foreign_device = foreign_device;
        self.granted.extend(grants);
        self
    }

    /// This standing's policy, with `acting` as who acts: `None` when the
    /// acting is not for the person this standing is. The delegation a
    /// person's own device gives its agent comes with it.
    #[must_use]
    pub fn as_acting(mut self, acting: Acting) -> Option<Self> {
        let person = self.person_id()?.to_owned();
        if acting.people().first().copied() != Some(person.as_str()) {
            return None;
        }
        for node in acting.nodes() {
            if let crate::Actor::Member(crate::MemberId::Agent(agent)) = &node.by {
                let grants = roles::agent_grants(&self.policy.workspace, agent, &person, &[]);
                self.granted.extend(grants);
            }
        }
        self.acting = acting;
        Some(self)
    }

    /// The same standing, with the resource kinds the app in it declares.
    #[must_use]
    pub fn in_app(mut self, kinds: impl IntoIterator<Item = String>) -> Self {
        self.app_kinds.get_or_insert_with(Vec::new).extend(kinds);
        self
    }

    /// The same standing, with `grants` for its software.
    #[must_use]
    pub fn granted(mut self, grants: impl IntoIterator<Item = Grant>) -> Self {
        self.granted.extend(grants);
        self
    }

    /// The same standing, from a device that is not the person's: what such a
    /// device may do applies.
    #[must_use]
    pub fn on_foreign_device(mut self) -> Self {
        self.acting.foreign_device = true;
        self.granted
            .extend(roles::foreign_device_bound().iter().cloned());
        self
    }

    /// Who an invocation of this standing asks
    /// ([`crate::Acting::asker`]).
    #[must_use]
    pub fn asker(&self, host_reaches_person: bool) -> crate::Asker {
        self.acting.asker(host_reaches_person)
    }

    /// The people this acting is for, in tree order.
    #[must_use]
    pub fn people(&self) -> Vec<&str> {
        self.acting.people()
    }

    /// The person it acted for, when it is for one.
    #[must_use]
    pub fn person_id(&self) -> Option<&str> {
        self.acting.people().first().copied()
    }

    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.policy.workspace
    }

    /// `relative` (a path below the workspace) as a node of this workspace.
    #[must_use]
    pub fn node(&self, relative: &TreePath) -> TreePath {
        self.policy.node().joined(relative)
    }

    /// The authority this standing confers.
    #[must_use]
    pub fn authority(&self) -> Authority<'_> {
        let authority = Authority::of_acting(&self.acting, &self.policy, &self.granted);
        match (self.acting.app.as_ref(), self.app_kinds.as_deref()) {
            (Some(_), Some(kinds)) => authority.in_app(kinds),
            _ => authority,
        }
    }

    #[must_use]
    pub fn verdict(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Verdict<'_> {
        self.authority().decide(node, kind, cx)
    }

    #[must_use]
    pub fn answer(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Answer {
        self.verdict(node, kind, cx).answer
    }
}
