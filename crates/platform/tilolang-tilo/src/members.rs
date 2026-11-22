//! The workspace's members as tilolang reads them (`workspace.members`, `members(r)`): the
//! workspace's own policy, the one the API decides by, read with the person's access. Nothing
//! here keeps a list of its own: the members are the policy's owner and members with their
//! roles and the agents its grants name, and who reads a resource is what the policy answers
//! for each of them.
//!
//! The policy names people by their person id, as the API, presence and the member list do;
//! the runtime names the person at the window `me`.

use std::collections::HashMap;
use std::rc::Rc;

use tilo_action_policy::{
    ActionKind, Answer, Authority as Policy, Context, Grantee, MemberAccess, TreePath,
};
use tilo_action_policy::{MemberId, PersonId};
use tilolang_runtime::members::{Member, Reads};
use tilolang_runtime::{Actor, ResourceRef};

use crate::host::Host;
use crate::meta::MetaExt as _;

/// The runtime's actor the person `person` of `access`'s policy is: the person at the
/// window is the runtime's `me` (as the authority maps them back,
/// [`crate::authority::TiloAuthority`]), and anyone else their person id, as the workspace's
/// member list and presence name them.
fn actor(access: &MemberAccess, me: &PersonId, person: &str) -> MemberId {
    if Some(person) == access.person_id() {
        MemberId::Person(me.clone())
    } else {
        MemberId::person(person)
    }
}

/// The person of `access`'s policy the runtime's actor `who` is, where `me` is the person
/// at the window.
fn person_of<'a>(access: &'a MemberAccess, me: &PersonId, who: &'a PersonId) -> &'a str {
    if who == me {
        access.person_id().unwrap_or_default()
    } else {
        who.as_str()
    }
}

/// Everyone in the workspace by `access`'s policy: the owner, the members by their role
/// (none who are blocked: they hold nothing), then the agents its grants name.
#[must_use]
pub fn members_of(access: &MemberAccess, me: &PersonId) -> Vec<Member> {
    let policy = &access.policy;
    let mut out: Vec<Member> = Vec::new();
    let persons = policy
        .owner
        .iter()
        .chain(policy.members.keys())
        .filter(|person| !policy.is_blocked(person));
    for person in persons {
        let Some(role) = policy.role_of(person) else {
            continue;
        };
        let id = actor(access, me, person);
        if out.iter().any(|member| member.id == id) {
            continue;
        }
        out.push(Member {
            id,
            role: Some(role),
        });
    }
    let mut agents: Vec<&str> = policy
        .grants
        .iter()
        // An agent admitted to the workspace holds its preset's grants (`agent-member:…`); the
        // answers a person gave an agent working for them admit nobody.
        .filter(|grant| grant.id.0.starts_with("agent-member:"))
        .filter_map(|grant| match &grant.grantee {
            Grantee::Member(MemberId::Agent(agent)) => Some(agent.as_str()),
            _ => None,
        })
        .collect();
    agents.sort_unstable();
    agents.dedup();
    out.extend(agents.into_iter().map(Member::agent));
    out
}

/// Who of the members reads a resource, as `access`'s policy answers for each: the node of
/// the resource under its app and kind, read.
#[must_use]
pub fn reads_of(access: &MemberAccess, me: &PersonId, host: &Host) -> Rc<Reads> {
    let kinds: HashMap<String, (String, String)> = host
        .packages()
        .into_iter()
        .flat_map(|package| {
            let meta = &package.meta;
            meta.types
                .iter()
                .map(|ty| (ty.id.clone(), (meta.app.clone(), meta.kind_of(&ty.name))))
                .collect::<Vec<_>>()
        })
        .collect();
    let access = access.clone();
    let me = me.clone();
    Rc::new(move |who: &Actor, target: &ResourceRef| {
        let workspace = TreePath::workspace(access.workspace());
        let node = match kinds.get(&target.ty) {
            Some((app, kind)) => workspace.app(app).kind(kind).resource(target.id_string()),
            None => workspace,
        };
        reads_node(&access, &me, who, &node)
    })
}

/// Whether `who` may read `node` by `access`'s policy: a person by their own party, an
/// agent by the grants the workspace gave it.
fn reads_node(access: &MemberAccess, me: &PersonId, who: &Actor, node: &TreePath) -> bool {
    let policy = &access.policy;
    let party = match who.member_id() {
        Some(MemberId::Person(person)) => policy.party(person_of(access, me, person)),
        Some(_) => match who.grantee() {
            Some(grantee) => policy.party_of(grantee),
            None => return false,
        },
        // An automation or an app is no member: it reads as the member it runs for.
        None => return false,
    };
    let earlier = crate::schema::earlier_ids();
    let cx = Context::at_time(tilo_clock::wall()).with_earlier_ids(&earlier);
    Policy::person(party)
        .decide(node, ActionKind::Read, &cx)
        .answer
        == Answer::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::{Grant, KindSet, Role, WorkspacePolicy};

    fn access() -> MemberAccess {
        let mut policy = WorkspacePolicy::new("workspace", "ada");
        policy.members.insert("bo".to_owned(), Role::Editor);
        policy.members.insert("cy".to_owned(), Role::Viewer);
        policy.grants.push(Grant::new(
            "agent-member:planner",
            Grantee::agent("planner"),
            TreePath::workspace("workspace"),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Grantee::person("ada"),
        ));
        // Cy may not read the secret note.
        policy.grants.push(Grant::new(
            "hide-secret",
            Grantee::person("cy"),
            TreePath::workspace("workspace")
                .app("notes")
                .kind("tl.notes.Note")
                .resource("secret"),
            KindSet::ALL,
            Answer::Deny,
            Grantee::person("ada"),
        ));
        MemberAccess::person(policy, "ada")
    }

    tilo_test_env::scenario! {
        fn members_and_readers_are_what_the_policy_answers;

        phase the_members_are_the_policys_people_as_the_runtime_names_them_then_its_agents;
        {
            let members = members_of(&access(), &PersonId::from("me"));
            assert_eq!(
                members,
                [
                    Member::person("me", Role::Owner),
                    Member::person("bo", Role::Editor),
                    Member::person("cy", Role::Viewer),
                    Member::agent("planner"),
                ]
            );
        }

        phase who_reads_a_resource_is_what_the_policy_answers_for_each_person;
        {
            let access = access();
            let node = |id: &str| {
                TreePath::workspace("workspace")
                    .app("notes")
                    .kind("tl.notes.Note")
                    .resource(id)
            };
            let all: [Actor; 4] = [
                MemberId::person("me").into(),
                MemberId::person("bo").into(),
                MemberId::person("cy").into(),
                MemberId::agent("planner").into(),
            ];
            for who in &all {
                assert!(
                    reads_node(&access, &PersonId::from("me"), who, &node("open")),
                    "{who} reads what the workspace holds"
                );
            }
            let secret: Vec<&str> = all
                .iter()
                .filter(|who| reads_node(&access, &PersonId::from("me"), who, &node("secret")))
                .map(Actor::id)
                .collect();
            assert_eq!(
                secret,
                ["me", "bo", "planner"],
                "Cy, whom the grant keeps out, does not"
            );
        }
    }
}
