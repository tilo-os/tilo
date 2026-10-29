//! The workspace's members as tilolang reads them (`workspace.members`, `members(r)`): the
//! room's own policy, the one the door decides by, read with the person's standing. Nothing
//! here keeps a list of its own: the members are the policy's owner and members with their
//! roles and the agents its grants name, and who reads a resource is what the policy answers
//! for each of them.

use std::collections::HashMap;
use std::rc::Rc;

use tilo_action_policy::{
    ActionKind, Answer, Authority as Policy, Context, Standing, Subject, TreePath,
};
use tilolang_runtime::members::{Member, MemberKind, Reads};
use tilolang_runtime::{Principal, ResourceRef};

use crate::host::Host;
use crate::meta::MetaExt as _;

/// The principal a person of the policy is to the runtime: the person at the window is the
/// runtime's `me`, as the authority maps them back ([`crate::authority::TiloAuthority`]).
fn principal_of(standing: &Standing, me: &str, person: &str) -> String {
    if person == standing.person {
        me.to_owned()
    } else {
        person.to_owned()
    }
}

/// Everyone in the workspace by `standing`'s policy: the owner, the members by their role
/// (none who are blocked: they hold nothing), then the agents its grants name.
#[must_use]
pub fn members_of(standing: &Standing, me: &str) -> Vec<Member> {
    let policy = &standing.policy;
    let mut out: Vec<Member> = Vec::new();
    let people = policy
        .owner
        .iter()
        .chain(policy.members.keys())
        .filter(|person| !policy.is_blocked(person));
    for person in people {
        let Some(role) = policy.role_of(person) else {
            continue;
        };
        let who = principal_of(standing, me, person);
        if out.iter().any(|member| member.who.0 == who) {
            continue;
        }
        out.push(Member {
            who: Principal::new(who),
            kind: MemberKind::Person,
            role: Some(role.name().to_lowercase()),
        });
    }
    let mut agents: Vec<&str> = policy
        .grants
        .iter()
        // An agent admitted to the room holds its preset's grants (`agent-member:…`); the
        // answers a person gave an agent working for them admit nobody.
        .filter(|grant| grant.id.0.starts_with("agent-member:"))
        .filter_map(|grant| match &grant.subject {
            Subject::Agent(agent) => Some(agent.as_str()),
            _ => None,
        })
        .collect();
    agents.sort_unstable();
    agents.dedup();
    out.extend(agents.into_iter().map(|agent| Member {
        who: Principal::new(format!("agent:{agent}")),
        kind: MemberKind::Agent,
        role: None,
    }));
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
            let meta = package.meta;
            let own = crate::own_record(&meta, &standing.person);
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
        let room = TreePath::room(standing.room());
        let node = match kinds.get(&target.ty) {
            Some((app, kind, Some(own))) => room
                .app(app)
                .kind(kind)
                .resource(own)
                .item(target.id_string()),
            Some((app, kind, None)) => room.app(app).kind(kind).resource(target.id_string()),
            None => room,
        };
        reads_node(&standing, &me, who, &node)
    })
}

/// Whether `who` may read `node` by `standing`'s policy: a person by their own party, an
/// agent by the grants the room gave it.
fn reads_node(standing: &Standing, me: &str, who: &Principal, node: &TreePath) -> bool {
    let policy = &standing.policy;
    let party = match who.0.strip_prefix("agent:") {
        Some(agent) => policy.party_of(Subject::agent(agent)),
        None if who.0 == me => policy.party(&standing.person),
        None => policy.party(&who.0),
    };
    let cx = Context::at_time(std::time::SystemTime::now());
    Policy::person(party)
        .decide(node, ActionKind::Read, &cx)
        .answer
        == Answer::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::roles::AgentPreset;
    use tilo_action_policy::{Grant, KindSet, Role, RoomPolicy};

    fn standing() -> Standing {
        let mut policy = RoomPolicy::new("room", "ada");
        policy.members.insert("bo".to_owned(), Role::Editor);
        policy.members.insert("cy".to_owned(), Role::Viewer);
        policy.grants.extend(AgentPreset::Contributor.grants(
            "room",
            "planner",
            &Subject::person("ada"),
        ));
        // Cy may not read the secret note.
        policy.grants.push(Grant::new(
            "hide-secret",
            Subject::person("cy"),
            TreePath::room("room")
                .app("notes")
                .kind("tl.notes.Note")
                .resource("secret"),
            KindSet::ALL,
            Answer::Deny,
            Subject::person("ada"),
        ));
        Standing::person(policy, "ada")
    }

    #[test]
    fn the_members_are_the_policys_people_by_role_then_its_admitted_agents() {
        let members = members_of(&standing(), "me");
        let shown: Vec<(&str, MemberKind, Option<&str>)> = members
            .iter()
            .map(|member| (member.who.0.as_str(), member.kind, member.role.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                ("me", MemberKind::Person, Some("owner")),
                ("bo", MemberKind::Person, Some("editor")),
                ("cy", MemberKind::Person, Some("viewer")),
                ("agent:planner", MemberKind::Agent, None),
            ]
        );
    }

    #[test]
    fn who_reads_a_resource_is_what_the_policy_answers_for_each() {
        let standing = standing();
        let node = |id: &str| {
            TreePath::room("room")
                .app("notes")
                .kind("tl.notes.Note")
                .resource(id)
        };
        for who in ["me", "bo", "cy", "agent:planner"] {
            assert!(
                reads_node(&standing, "me", &Principal::new(who), &node("open")),
                "{who} reads what the room holds"
            );
        }
        let secret: Vec<&str> = ["me", "bo", "cy", "agent:planner"]
            .into_iter()
            .filter(|who| reads_node(&standing, "me", &Principal::new(*who), &node("secret")))
            .collect();
        assert_eq!(secret, ["me", "bo", "agent:planner"]);
    }
}
