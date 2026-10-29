//! The workspace's members as tilolang reads them (`workspace.members`, `members(r)`): the
//! room's own policy, the one the door decides by, read with the person's standing. Nothing
//! here keeps a list of its own: the members are the policy's owner and members with their
//! roles and the agents its grants name, and who reads a resource is what the policy answers
//! for each of them.
//!
//! The policy names people by their person id; the runtime names whoever acted by the
//! participant id the door gave the change, the one presence knows them by. [`People`] is how
//! one is the other.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use tilo_action_policy::{
    ActionKind, Answer, Authority as Policy, Context, Standing, Subject, TreePath,
};
use tilolang_runtime::members::{Member, MemberKind, Reads};
use tilolang_runtime::{Principal, ResourceRef};

use crate::host::Host;
use crate::meta::MetaExt as _;

/// How the people of the room's policy are the runtime's principals: the person at the window
/// is the runtime's `me` (as the authority maps them back,
/// [`crate::authority::TiloAuthority`]), a person the room's host paired with a participant is
/// that participant, and anyone else is their person id, as presence names them where it names
/// people.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct People {
    me: String,
    /// The participant each paired person is, by person id: the first of theirs, so every
    /// frame picks the same one.
    participant: BTreeMap<String, String>,
    /// The person each paired participant is a device of.
    person: BTreeMap<String, String>,
}

impl People {
    /// The people for the runtime whose own participant is `me`, with the `(participant,
    /// person)` pairs presence reports.
    #[must_use]
    pub fn new(me: &str, pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let person: BTreeMap<String, String> = pairs
            .into_iter()
            .filter(|(participant, person)| participant != person)
            .collect();
        let mut participant = BTreeMap::new();
        for (device, of) in &person {
            participant
                .entry(of.clone())
                .or_insert_with(|| device.clone());
        }
        Self {
            me: me.to_owned(),
            participant,
            person,
        }
    }

    /// The principal the person `person` of `standing`'s policy is.
    fn principal(&self, standing: &Standing, person: &str) -> String {
        if person == standing.person {
            return self.me.clone();
        }
        self.participant
            .get(person)
            .cloned()
            .unwrap_or_else(|| person.to_owned())
    }

    /// The person of `standing`'s policy the principal `who` is.
    fn person_of<'a>(&'a self, standing: &'a Standing, who: &'a str) -> &'a str {
        if who == self.me {
            return &standing.person;
        }
        self.person.get(who).map_or(who, String::as_str)
    }
}

/// Everyone in the workspace by `standing`'s policy: the owner, the members by their role
/// (none who are blocked: they hold nothing), then the agents its grants name.
#[must_use]
pub fn members_of(standing: &Standing, people: &People) -> Vec<Member> {
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
        let who = people.principal(standing, person);
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
pub fn reads_of(standing: &Standing, people: &People, host: &Host) -> Rc<Reads> {
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
    let people = people.clone();
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
        reads_node(&standing, &people, who, &node)
    })
}

/// Whether `who` may read `node` by `standing`'s policy: a person by their own party, an
/// agent by the grants the room gave it.
fn reads_node(standing: &Standing, people: &People, who: &Principal, node: &TreePath) -> bool {
    let policy = &standing.policy;
    let party = match who.0.strip_prefix("agent:") {
        Some(agent) => policy.party_of(Subject::agent(agent)),
        None => policy.party(people.person_of(standing, &who.0)),
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

    /// Cy works from a device the room's host paired with them; Bo is named as themselves.
    fn people() -> People {
        People::new(
            "me",
            [
                ("cy-laptop".to_owned(), "cy".to_owned()),
                ("cy-phone".to_owned(), "cy".to_owned()),
            ],
        )
    }

    #[test]
    fn the_members_are_the_policys_people_as_the_runtime_names_them_then_its_agents() {
        let members = members_of(&standing(), &people());
        let shown: Vec<(&str, MemberKind, Option<&str>)> = members
            .iter()
            .map(|member| (member.who.0.as_str(), member.kind, member.role.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                ("me", MemberKind::Person, Some("owner")),
                ("bo", MemberKind::Person, Some("editor")),
                ("cy-laptop", MemberKind::Person, Some("viewer")),
                ("agent:planner", MemberKind::Agent, None),
            ]
        );
    }

    #[test]
    fn who_reads_a_resource_is_what_the_policy_answers_for_each_person() {
        let (standing, people) = (standing(), people());
        let node = |id: &str| {
            TreePath::room("room")
                .app("notes")
                .kind("tl.notes.Note")
                .resource(id)
        };
        let all = ["me", "bo", "cy-laptop", "cy-phone", "agent:planner"];
        for who in all {
            assert!(
                reads_node(&standing, &people, &Principal::new(who), &node("open")),
                "{who} reads what the room holds"
            );
        }
        let secret: Vec<&str> = all
            .into_iter()
            .filter(|who| reads_node(&standing, &people, &Principal::new(*who), &node("secret")))
            .collect();
        assert_eq!(
            secret,
            ["me", "bo", "agent:planner"],
            "each of Cy's devices is Cy, whom the grant keeps out"
        );
    }
}
