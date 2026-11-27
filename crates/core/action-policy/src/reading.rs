//! Who reads one resource, changed one person or role at a time.
//!
//! A resource keeps no membership of its own: who belongs to a channel, a shared folder or any
//! other resource is who the workspace's policy lets read it. Letting someone read it, or keeping
//! them from it, is a grant on the resource's own node, made by whoever the workspace lets grant
//! (to let someone in) or revoke (to keep someone out), and decided by [`crate::decide`] like
//! every other grant.
//!
//! The policy holds the least that says it. Each person or role has at most one reading grant on
//! a resource ([`reading_grant_id`]); a change that leaves them reading as they would without it
//! takes that grant away rather than writing one more. So letting back in someone a moderator
//! kept out removes the Deny, and keeping out someone a grant let in removes the Allow, while
//! keeping out someone their role lets read writes a Deny of their own.

use crate::{
    Authority, Change, Context, Grant, GrantId, Grantee, Level, Refusal, TreePath, WorkspacePolicy,
    roles::governance,
};
use tilo_action_types::{ActionKind, KindSet};

/// The node a published resource of a tilolang app (`tl:<app>:<id>`) stands at in `workspace`:
/// the resource under its app, of whatever kind it is. `None` for any other resource.
#[must_use]
pub fn published_node(workspace: &str, resource: &str) -> Option<TreePath> {
    let (app, id) = resource.strip_prefix("tl:")?.split_once(':')?;
    (!app.is_empty() && !id.is_empty()).then(|| {
        TreePath::workspace(workspace)
            .app(app)
            .any(Level::Kind)
            .resource(id)
    })
}

/// The id of the one grant that says whether `grantee` reads `node`. An agent is the pair of its
/// id and its person, so a reading grant for an agent is named by both.
#[must_use]
pub fn reading_grant_id(grantee: &Grantee, node: &TreePath) -> GrantId {
    let who = match grantee {
        Grantee::Member(tilo_member_types::MemberId::Person(person)) => {
            format!("person:{}", person.as_str())
        }
        Grantee::Member(tilo_member_types::MemberId::Agent { id, person }) => format!(
            "agent:{}",
            crate::agent_answers::agent_key(id, person.as_str())
        ),
        Grantee::Member(tilo_member_types::MemberId::Bot(bot)) => format!("bot:{bot}"),
        Grantee::Role(role) => format!("role:{}", role.key()),
        Grantee::CustomRole(role) => format!("custom-role:{role}"),
        Grantee::App(app) => format!("app:{app}"),
        Grantee::AnyKey => "any-key".to_owned(),
    };
    GrantId::new(format!("reads:{who}:{node}"))
}

impl WorkspacePolicy {
    /// The change that makes `grantee` read `node` (`reads`) or not, asked by `actor` as the
    /// person `granted_by`: letting someone read is granting, keeping them out is revoking, each
    /// admitted by the workspace's own grant on it. `None` when they already stand as asked.
    ///
    /// # Errors
    /// The actor may not grant (or revoke) in the workspace, the node is outside it, or the
    /// actor would let someone read what they cannot read themselves.
    pub fn reading_change(
        &self,
        actor: &Authority<'_>,
        grantee: Grantee,
        node: &TreePath,
        reads: bool,
        granted_by: Grantee,
        cx: &Context<'_>,
    ) -> Result<Option<Change>, Refusal> {
        if node.workspace_id() != Some(self.workspace.as_str()) {
            return Err(Refusal::OutsideWorkspace(node.clone()));
        }
        let right = if reads {
            governance::GRANT
        } else {
            governance::REVOKE
        };
        self.check_action(actor, right, cx, false)?;
        let id = reading_grant_id(&grantee, node);
        let held = self.grants.iter().find(|grant| grant.id == id);
        // How they would stand with no reading grant of their own on it.
        let mut without = self.clone();
        without.grants.retain(|grant| grant.id != id);
        let party = match (grantee.person_id(), grantee.as_agent()) {
            (Some(person), _) => without.party(person),
            (None, Some((agent, person))) => without.party_of_agent(agent, person.as_str()),
            (None, None) => without.party_of(grantee.clone()),
        };
        let reads_without = Authority::person(party)
            .decide(node, ActionKind::Read, cx)
            .answer
            == crate::Answer::Allow;
        if reads_without == reads {
            return Ok(held.map(|grant| Change::Revoke(grant.id.clone())));
        }
        let grant = if reads {
            Grant::new(
                id.as_str(),
                grantee,
                node.clone(),
                KindSet::READ,
                crate::Answer::Allow,
                granted_by,
            )
        } else {
            // Kept out of reading is kept out of all of it: nobody writes into what they cannot
            // read.
            Grant::new(
                id.as_str(),
                grantee,
                node.clone(),
                KindSet::ALL,
                crate::Answer::Deny,
                granted_by,
            )
        };
        Self::check_within(actor, &grant, cx)?;
        if held.is_some_and(|kept| *kept == grant) {
            return Ok(None);
        }
        Ok(Some(Change::Grant(grant)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Answer, Role};

    const WORKSPACE: &str = "team";

    fn channel() -> TreePath {
        published_node(WORKSPACE, "tl:so.tilo.chat:root/general").expect("a published resource")
    }

    /// Whether `person` reads the channel, at the node a read of it is decided at: its kind
    /// named, as the reader's device names it.
    fn reads(policy: &WorkspacePolicy, person: &str) -> bool {
        let cx = Context::at_time(tilo_clock::wall());
        let read = TreePath::workspace(WORKSPACE)
            .app("so.tilo.chat")
            .kind("tl.chat.TextChannel")
            .resource("root/general");
        Authority::person(policy.party(person))
            .decide(&read, ActionKind::Read, &cx)
            .answer
            == Answer::Allow
    }

    fn apply(policy: &mut WorkspacePolicy, by: &str, change: Option<Change>) {
        let cx = Context::at_time(tilo_clock::wall());
        if let Some(change) = change {
            let actor = Authority::person(policy.party(by));
            let mut next = policy.clone();
            next.apply(by, &actor, change, &cx, false)
                .expect("the change applies");
            *policy = next;
        }
    }

    tilo_test_env::scenario! {
        fn who_reads_a_resource_is_changed_one_grant_at_a_time;

        phase keeping_out_a_reader_writes_their_deny_and_letting_them_back_removes_it;
        {
            let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
            policy.members.insert("ben".to_owned(), Role::Editor.into());
            let cx = Context::at_time(tilo_clock::wall());
            let node = channel();
            assert!(reads(&policy, "ben"), "a member reads what the workspace holds");
            let change = policy
                .reading_change(
                    &Authority::person(policy.party("ada")),
                    Grantee::person("ben"),
                    &node,
                    false,
                    Grantee::person("ada"),
                    &cx,
                )
                .expect("the owner may revoke");
            apply(&mut policy, "ada", change);
            assert!(!reads(&policy, "ben"), "Ben is kept out of the channel");
            let change = policy
                .reading_change(
                    &Authority::person(policy.party("ada")),
                    Grantee::person("ben"),
                    &node,
                    true,
                    Grantee::person("ada"),
                    &cx,
                )
                .expect("the owner may grant");
            assert!(
                matches!(change, Some(Change::Revoke(_))),
                "letting him back takes the Deny away: {change:?}"
            );
            apply(&mut policy, "ada", change);
            assert!(reads(&policy, "ben"));
            assert!(
                policy.grants.iter().all(|grant| !grant.id.as_str().starts_with("reads:")),
                "nothing is left of either change"
            );
        }

        phase a_member_the_workspace_does_not_let_grant_is_refused;
        {
            let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
            policy.members.insert("ben".to_owned(), Role::Editor.into());
            policy.members.insert("cy".to_owned(), Role::Editor.into());
            let cx = Context::at_time(tilo_clock::wall());
            let refused = policy.reading_change(
                &Authority::person(policy.party("cy")),
                Grantee::person("ben"),
                &channel(),
                false,
                Grantee::person("cy"),
                &cx,
            );
            assert!(
                matches!(refused, Err(Refusal::NotPermitted(_))),
                "{refused:?}"
            );
        }
    }
}
