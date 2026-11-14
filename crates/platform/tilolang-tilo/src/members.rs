//! The workspace's members as tilolang reads them (`workspace.members`, `members(r)`): the
//! workspace's own policy, the one the door decides by, read with the person's standing. Nothing
//! here keeps a list of its own: the members are the policy's owner and members with their
//! roles and the agents its grants name, and who reads a resource is what the policy answers
//! for each of them.
//!
//! The policy names people by their person id, as the door, presence and the member list do;
//! the runtime names the person at the window `me`.

use std::collections::HashMap;
use std::rc::Rc;

use tilo_action_policy::MemberId;
use tilo_action_policy::{
    ActionKind, Answer, Authority as Policy, Context, Grantee, Standing, TreePath,
};
use tilolang_runtime::members::{Member, Reads};
use tilolang_runtime::{Principal, ResourceRef};

use crate::host::Host;
use crate::meta::MetaExt as _;

/// The runtime's principal the person `person` of `standing`'s policy is: the person at the
/// window is the runtime's `me` (as the authority maps them back,
/// [`crate::authority::TiloAuthority`]), and anyone else their person id, as the workspace's
/// member list and presence name them.
fn principal(standing: &Standing, me: &str, person: &str) -> String {
    if Some(person) == standing.person_id() {
        me.to_owned()
    } else {
        person.to_owned()
    }
}

/// The person of `standing`'s policy the runtime's principal `who` is, where `me` is the person
/// at the window.
fn person_of<'a>(standing: &'a Standing, me: &str, who: &'a str) -> &'a str {
    if who == me {
        standing.person_id().unwrap_or_default()
    } else {
        who
    }
}

/// Everyone in the workspace by `standing`'s policy: the owner, the members by their role
/// (none who are blocked: they hold nothing), then the agents its grants name.
#[must_use]
pub fn members_of(standing: &Standing, me: &str) -> Vec<Member> {
    let policy = &standing.policy;
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
        let id = MemberId::person(principal(standing, me, person));
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

/// Who of the members reads a resource, as `standing`'s policy answers for each: the node of
/// the resource under its app and kind, read. A resource of an app whose records are each
/// member's own is an item of the record of the person at the window, which only they read.
#[must_use]
pub fn reads_of(standing: &Standing, me: &str, host: &Host) -> Rc<Reads> {
    let kinds: HashMap<String, (String, String, Option<String>)> = host
        .packages()
        .into_iter()
        .flat_map(|package| {
            let meta = &package.meta;
            let own = crate::own_record(meta, standing.person_id().unwrap_or_default());
            meta.types
                .iter()
                .map(|ty| {
                    let at = (meta.app.clone(), meta.kind_of(&ty.name), own.clone());
                    (ty.id.clone(), at)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let standing = standing.clone();
    let me = me.to_owned();
    Rc::new(move |who: &Principal, target: &ResourceRef| {
        let workspace = TreePath::workspace(standing.workspace());
        let node = match kinds.get(&target.ty) {
            Some((app, kind, Some(own))) => workspace
                .app(app)
                .kind(kind)
                .resource(own)
                .item(target.id_string()),
            Some((app, kind, None)) => workspace.app(app).kind(kind).resource(target.id_string()),
            None => workspace,
        };
        reads_node(&standing, &me, who, &node)
    })
}

/// Whether `who` may read `node` by `standing`'s policy: a person by their own party, an
/// agent by the grants the workspace gave it.
fn reads_node(standing: &Standing, me: &str, who: &Principal, node: &TreePath) -> bool {
    let policy = &standing.policy;
    let party = match who.0.strip_prefix("agent:") {
        Some(agent) => policy.party_of(Grantee::agent(agent)),
        None => policy.party(person_of(standing, me, &who.0)),
    };
    let earlier = crate::schema::earlier_ids();
    let cx = Context::at_time(std::time::SystemTime::now()).with_earlier_ids(&earlier);
    Policy::person(party)
        .decide(node, ActionKind::Read, &cx)
        .answer
        == Answer::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::{Grant, KindSet, Role, WorkspacePolicy};

    fn standing() -> Standing {
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
        Standing::person(policy, "ada")
    }

    tilo_test_env::scenario! {
        fn members_and_readers_are_what_the_policy_answers;

        phase the_members_are_the_policys_people_as_the_runtime_names_them_then_its_agents;
        {
            let members = members_of(&standing(), "me");
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
            let standing = standing();
            let node = |id: &str| {
                TreePath::workspace("workspace")
                    .app("notes")
                    .kind("tl.notes.Note")
                    .resource(id)
            };
            let all = ["me", "bo", "cy", "agent:planner"];
            for who in all {
                assert!(
                    reads_node(&standing, "me", &Principal::new(who), &node("open")),
                    "{who} reads what the workspace holds"
                );
            }
            let secret: Vec<&str> = all
                .into_iter()
                .filter(|who| reads_node(&standing, "me", &Principal::new(*who), &node("secret")))
                .collect();
            assert_eq!(
                secret,
                ["me", "bo", "agent:planner"],
                "Cy, whom the grant keeps out, does not"
            );
        }
    }
}
