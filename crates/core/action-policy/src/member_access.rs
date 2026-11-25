//! One acting's access in one workspace, as owned data a host hands to an action API
//! or an app: who acted, the workspace's policy, and the grants the host made
//! for the acting's software.

use serde::{Deserialize, Serialize};

use crate::{
    Acting, ActionKind, Answer, Authority, Context, Grant, TreePath, Verdict, WorkspacePolicy,
};
use tilo_clock::time::SystemTime;

/// Who made a change and what they may do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAccess {
    pub acting: Acting,
    /// Shared, not copied: a host keeps every member device's access in a
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
    /// The one workspace the device this access is reaches, by its id, when its person's root
    /// confined it to one (a workspace's VM); `None` for a device that reaches every workspace
    /// its person's grants let it. Written always, so no reader takes an access that says it for
    /// one that does not.
    #[serde(default)]
    pub confined_to: Option<String>,
}

impl MemberAccess {
    /// `acting` in the workspace `policy` governs.
    #[must_use]
    pub fn new(policy: impl Into<std::sync::Arc<WorkspacePolicy>>, acting: Acting) -> Self {
        let policy: std::sync::Arc<WorkspacePolicy> = policy.into();
        Self {
            granted: acting.handed_preset(&policy.workspace),
            acting,
            policy,
            app_kinds: None,
            confined_to: None,
        }
    }

    /// A person acting as themselves.
    #[must_use]
    #[track_caller]
    pub fn person(
        policy: impl Into<std::sync::Arc<WorkspacePolicy>>,
        person: impl Into<crate::PersonId>,
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

    /// This access's policy, with `acting` as who acts: `None` when the
    /// acting is not bounded by the person this access is. Every person in
    /// the tree bounds what it may do, so an agent answering someone else for
    /// this access's person ([`Acting::agent_of`]) does no more than either.
    /// The grants the acting's software carries are the caller's to give
    /// ([`Self::granted`]).
    #[must_use]
    pub fn as_acting(mut self, acting: Acting) -> Option<Self> {
        let person = self.person_id()?;
        if !acting.people().contains(&person) {
            return None;
        }
        self.granted
            .extend(acting.handed_preset(&self.policy.workspace));
        self.acting = acting;
        Some(self)
    }

    /// This access as a change naming `invoker` (who made it) is decided,
    /// signed by the device this access is: the invoker's whole tree, bounded
    /// by the signer, every node allowing it ([`Acting::signed_by`]). `None`
    /// when this device may not write for the member `invoker` names
    /// ([`Self::may_write_for`]).
    #[must_use]
    pub fn invoked_by(&self, invoker: &Acting) -> Option<Self> {
        if !self.may_write_for(invoker) {
            return None;
        }
        let acting = invoker.clone().signed_by(&self.acting);
        let mut access = self.clone();
        access
            .granted
            .extend(acting.handed_preset(&self.policy.workspace));
        access.acting = acting;
        Some(access)
    }

    /// Whether a change this device signed may name `invoker` as having made
    /// it: the member is the device's own person, or an agent it runs. The
    /// device's certificate is what says whose it is ([`Self::acting`] is
    /// built from it); this decides the member against it, once for every
    /// receiver. A bot, an automation or an app acts for whoever runs it, and
    /// is bounded by the signer as every member's change is. A device confined
    /// to another workspace than this access's writes for no one here
    /// ([`Self::confined_to`]).
    #[must_use]
    pub fn may_write_for(&self, invoker: &Acting) -> bool {
        if !self.reaches_its_workspace() {
            return false;
        }
        let Some(person) = self.acting.people().first().copied() else {
            // A device acting as nobody (an app's own key) claims no member:
            // its grants decide, as they always did.
            return true;
        };
        match invoker.by.member_id() {
            Some(crate::MemberId::Person(named)) => {
                // An agent's own key never stands for the person it works for.
                !self.acting.is_agent()
                    && named.as_str() == person
                    && invoker.people().iter().all(|named| *named == person)
            }
            Some(crate::MemberId::Agent { id, person: own }) => {
                invoker.people().iter().all(|named| *named == person)
                    && self.runs_agent(id, own.as_str())
            }
            // A bot, an automation or an app: the signer bounds it.
            _ => true,
        }
    }

    /// Whether this access's person runs the agent `agent` acting for
    /// `own`: the agent whose key this device is; a key's agent only through
    /// the grant its person signed for it; or an agent of this device's person,
    /// whose certificate names them and whose grants bound what it may do.
    fn runs_agent(&self, agent: &str, own: &str) -> bool {
        let person = self.acting.people().first().copied().unwrap_or_default();
        if self
            .acting
            .member_id()
            .and_then(crate::MemberId::as_agent)
            .is_some_and(|(id, of)| id == agent && of.as_str() == own)
        {
            return true;
        }
        // A key's agent is its own key, or the grant its person signed, which
        // this device holds: no device claims it handed.
        if crate::key_of_agent(agent).is_some() {
            return self.granted.iter().any(|grant| {
                grant
                    .grantee
                    .as_agent()
                    .is_some_and(|(id, of)| id == agent && *of == own)
            });
        }
        own == person
    }

    /// The same access, from a device its person's root confined to `workspace` alone (its id),
    /// or to none.
    #[must_use]
    pub fn confined_to(mut self, workspace: Option<String>) -> Self {
        self.confined_to = workspace;
        self
    }

    /// Whether the device this access is reaches the workspace its policy governs: every device
    /// does but one confined to another ([`Self::confined_to`]).
    #[must_use]
    pub fn reaches_its_workspace(&self) -> bool {
        self.confined_to
            .as_deref()
            .is_none_or(|only| only.eq_ignore_ascii_case(&self.policy.workspace))
    }

    /// The same access, with the resource kinds the app in it declares.
    #[must_use]
    pub fn in_app(mut self, kinds: impl IntoIterator<Item = String>) -> Self {
        self.app_kinds.get_or_insert_with(Vec::new).extend(kinds);
        self
    }

    /// The same access, with `grants` for its software.
    #[must_use]
    pub fn granted(mut self, grants: impl IntoIterator<Item = Grant>) -> Self {
        self.granted.extend(grants);
        self
    }

    /// The same access, from a device that is not the person's: what such a
    /// device may do applies ([`crate::Cap::FOREIGN_DEVICE`]).
    #[must_use]
    pub fn on_foreign_device(mut self) -> Self {
        self.acting.foreign_device = true;
        self
    }

    /// Who an invocation of this lasting asks
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

    /// The authority this access confers.
    #[must_use]
    pub fn authority(&self) -> Authority<'_> {
        let authority = Authority::of_acting(&self.acting, &self.policy, &self.granted);
        match (self.acting.app.as_ref(), self.app_kinds.as_deref()) {
            (Some(_), Some(kinds)) => authority.in_app(kinds),
            _ => authority,
        }
    }

    /// What this access decides from ([`crate::Effective`]): two accesses with
    /// equal ones may do the same, so a host holding a view made under one keeps
    /// it under the other.
    #[must_use]
    pub fn effective(&self) -> crate::Effective {
        self.authority().effective()
    }

    #[must_use]
    pub fn verdict(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Verdict<'_> {
        self.authority().decide(node, kind, cx)
    }

    #[must_use]
    pub fn answer(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Answer {
        self.verdict(node, kind, cx).answer
    }

    /// Whether this access may take `kind` at `node` at `now`.
    #[must_use]
    pub fn allows(&self, node: &TreePath, kind: ActionKind, now: SystemTime) -> bool {
        self.answer(node, kind, &Context::at_time(now)) == Answer::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    /// A device its person confined to one workspace writes for them there, and for no one, not
    /// even them, in any other.
    #[test]
    fn a_confined_device_writes_for_no_one_outside_its_workspace() {
        let person = Acting::person("ada");
        let own = MemberAccess::person(WorkspacePolicy::new(OWN, "ada"), "ada")
            .confined_to(Some(OWN.to_owned()));
        assert!(own.may_write_for(&person));
        assert!(own.invoked_by(&person).is_some());
        let other = MemberAccess::person(WorkspacePolicy::new(OTHER, "ada"), "ada")
            .confined_to(Some(OWN.to_owned()));
        assert!(!other.reaches_its_workspace());
        assert!(!other.may_write_for(&person));
        assert!(other.invoked_by(&person).is_none());
        let unconfined = MemberAccess::person(WorkspacePolicy::new(OTHER, "ada"), "ada");
        assert!(unconfined.may_write_for(&person));
        // What it is confined to travels with the access, so a reader of it refuses the same.
        let read: MemberAccess =
            serde_json::from_str(&serde_json::to_string(&other).unwrap()).unwrap();
        assert!(!read.may_write_for(&person));
    }
}
