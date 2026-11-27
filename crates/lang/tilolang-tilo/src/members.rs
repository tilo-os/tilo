//! The workspace's members as tilolang reads them (`workspace.members`, `members(r)`): the
//! workspace's own policy, the one the API decides by, read with the person's access. Nothing
//! here keeps a list of its own: the members are the policy's owner and members with their
//! roles and the agents it admits, and who reads a resource is what the policy answers for each
//! of them. An agent is the pair of its id and the person it acts for: one named by its id alone
//! is the agent of the person whose record or view names it.
//!
//! The policy names people by their person id, as the API, presence and the member list do;
//! the runtime names the person at the window `me`.

use std::collections::HashMap;
use std::rc::Rc;
use tilo_action_policy::ActingPolicy as _;

use tilo_action_policy::{Answer, Authority as Policy, Context, Grantee, MemberAccess, TreePath};
use tilo_action_types::ActionKind;
use tilo_member_types::Acting;
use tilo_member_types::{MemberId, PersonId};
use tilolang_runtime::members::{Member, Reads};
use tilolang_runtime::{Actor, ResourceRef};

use crate::host::Host;
use crate::meta::MetaExt as _;

/// The runtime's actor the person `person` of `access`'s policy is: the person at the
/// window is the runtime's `me` (as the authority maps them back,
/// [`crate::authority::TiloAuthority`]), and anyone else their person id, as the workspace's
/// member list and presence name them.
fn actor(access: &MemberAccess, me: &PersonId, person: &str) -> MemberId {
    MemberId::Person(person_named(access, me, person))
}

/// The runtime's name for the person of `access`'s policy `person`, where `me` is the person
/// at the window.
fn person_named(access: &MemberAccess, me: &PersonId, person: &str) -> PersonId {
    if Some(person) == access.person_id() {
        me.clone()
    } else {
        PersonId::from(person)
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
/// (none who are blocked: they hold nothing), then the agents a grant admitted
/// (`agent-member:…`).
#[must_use]
/// The members a members' reading `content` (`{"people": …}`) names, as a host reads them: each
/// row's actor with what the workspace says of them. A workspace's governance answers each row
/// with the actor an action names ([`tilo_actions::Actor`]); a store held in this process keeps
/// the runtime's own encoding of one, which names the same member.
fn rows_of(content: &serde_json::Value, me: &MemberId) -> Vec<collab_types::Member> {
    let Some(rows) = content.get("people").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            if row.get("actor").is_some() {
                return row.clone();
            }
            match tilolang_runtime::value::from_json(row) {
                Ok(tilolang_runtime::Value::Actor(who)) => serde_json::json!({
                    "actor": serde_json::to_value(who).unwrap_or_default(),
                }),
                _ => row.clone(),
            }
        })
        .collect();
    collab_types::Member::roster_of(&serde_json::json!({ "people": rows }), me)
}

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
    let mut agents: Vec<(&str, &str)> = policy
        .grants
        .iter()
        // An agent admitted to the workspace holds its preset's grants (`agent-member:…`); the
        // answers a person gave an agent working for them admit nobody.
        .filter(|grant| grant.id.0.starts_with("agent-member:"))
        .filter_map(|grant| grant.grantee.as_agent())
        .map(|(agent, person)| (agent, person.as_str()))
        .collect();
    agents.sort_unstable();
    agents.dedup();
    // An agent of the person at the window is theirs as the runtime names them.
    out.extend(
        agents
            .into_iter()
            .map(|(agent, person)| Member::agent(agent, person_named(access, me, person))),
    );
    // The workspace's installed bots: the grants it made them name them, and a bot reads what
    // they allow it through the host that runs it.
    let mut bots: Vec<&str> = policy
        .grants
        .iter()
        .chain(&access.granted)
        .filter_map(|grant| match &grant.grantee {
            Grantee::Member(MemberId::Bot(bot)) => Some(bot.as_str()),
            _ => None,
        })
        .collect();
    bots.sort_unstable();
    bots.dedup();
    out.extend(bots.into_iter().map(Member::bot));
    out
}

/// What the members were last given from: the person, the policy, and where each resource type
/// stood ([`kind_nodes`]).
pub(crate) type MembersFrom = (
    String,
    tilo_action_policy::WorkspacePolicy,
    HashMap<String, (String, String)>,
);

/// Where each resource type of `host` stands in the workspace's tree: its app and its kind.
#[must_use]
pub(crate) fn kind_nodes(host: &Host) -> HashMap<String, (String, String)> {
    host.packages()
        .into_iter()
        .flat_map(|package| {
            let meta = &package.meta;
            meta.types
                .iter()
                .map(|ty| (ty.id.clone(), (meta.app.clone(), meta.kind_of(&ty.name))))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Who of the members reads a resource, as `access`'s policy answers for each: the node of
/// the resource under its app and kind, read.
#[must_use]
pub fn reads_of(access: &MemberAccess, me: &PersonId, host: &Host) -> Rc<Reads> {
    let kinds = kind_nodes(host);
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

/// The store's reader decision for the workspace this host serves: who of the members a reader
/// field names reads a record, answered by the same check the runtime's member reads use
/// ([`reads_of`]), so a host makes one read decision and not two.
///
/// A field may name a person or an agent: a person reads as themselves, an agent as the person it
/// is of, decided by its preset narrowed to that person's reach. An agent whose grant does not
/// reach the record reads none of it.
#[derive(Default)]
pub struct MemberRecordReads {
    state: std::sync::Mutex<Option<MemberReadState>>,
}

struct MemberReadState {
    access: MemberAccess,
    me: PersonId,
    kinds: HashMap<String, (String, String)>,
}

/// The person the viewer acts as in `access`: the person the host decides as.
fn viewer_person(viewer: &Actor, access: &MemberAccess) -> PersonId {
    viewer
        .member_id()
        .and_then(MemberId::person_id)
        .map_or_else(
            || PersonId::from(access.person_id().unwrap_or_default()),
            PersonId::from,
        )
}

impl MemberRecordReads {
    /// Follow the access and packages the host has now. Until there is an access, no field is
    /// placed: the store refuses one.
    pub(crate) fn follow(&self, access: Option<MemberAccess>, viewer: &Actor) {
        self.follow_with_kinds(access, viewer, HashMap::new());
    }

    /// As [`Self::follow`], with the packages' kinds, so a record is decided at its own node.
    pub(crate) fn follow_with_kinds(
        &self,
        access: Option<MemberAccess>,
        viewer: &Actor,
        kinds: HashMap<String, (String, String)>,
    ) {
        let next = access.map(|access| MemberReadState {
            me: viewer_person(viewer, &access),
            access,
            kinds,
        });
        if let Ok(mut state) = self.state.lock() {
            *state = next;
        }
    }
}

impl tilo_resource_store::RecordReader for MemberRecordReads {
    fn reader_of(&self, actor: &Actor, kind: &str, id: &str) -> Option<PersonId> {
        let guard = self.state.lock().ok()?;
        let state = guard.as_ref()?;
        let workspace = TreePath::workspace(state.access.workspace());
        let node = match state.kinds.get(kind) {
            Some((app, named)) => workspace.app(app).kind(named).resource(id.to_owned()),
            None => workspace,
        };
        if !reads_node(&state.access, &state.me, actor, &node) {
            return None;
        }
        let reader = match actor.member_id()? {
            // An agent reads as the person it is of.
            MemberId::Person(person) | MemberId::Agent { person, .. } => Some(person.clone()),
            MemberId::Bot(_) => None,
        };
        drop(guard);
        reader
    }
}

impl Host {
    /// Hand the runtime the workspace's members as they stand: who is in it, read from the
    /// workspace's members' resource through this host's store, again whenever a change of the
    /// members or of its policy may have moved it ([`Self::read_workspace_members`]); and the
    /// policy the host decides by (its authority's access), when it changed: their roles, and
    /// who reads what. A window's instance follows them each frame, and a read follows them
    /// before it reads, so a host the session keeps decides who reads what as a window's does.
    /// Whether either moved.
    pub fn follow_members(&self) -> bool {
        let policy = self.follow_member_policy();
        self.read_workspace_members() || policy
    }

    /// Give the runtime the policy the host decides by, when it changed. Whether it did.
    fn follow_member_policy(&self) -> bool {
        let Some(access) = self.authority().access() else {
            // A host with no workspace policy yet decides by none: the people its members'
            // resource names are in it, with no role. It follows the policy once one arrives.
            // Until then it is in no workspace whose members it could name, so nothing waits on
            // a reading of them.
            self.inner.members_known.set(true);
            return false;
        };
        let person = access.person_id().unwrap_or_default();
        // Who reads a resource is decided at its app and kind, so a package loaded since the
        // policy was last given is a change too: its resources were decided at the workspace.
        let kinds = kind_nodes(self);
        let seen = self
            .inner
            .members_from
            .borrow()
            .as_ref()
            .is_some_and(|(was, policy, known)| {
                was == person && *policy == *access.policy && *known == kinds
            });
        if seen {
            return false;
        }
        // The store's reader decision follows the same access, and the packages' kinds as they
        // stand now, so a package just declared places its kinds at their own nodes.
        self.inner.record_reads.follow_with_kinds(
            Some(access.clone()),
            self.viewer(),
            kinds.clone(),
        );
        let me = PersonId::from(
            self.viewer()
                .member_id()
                .and_then(MemberId::person_id)
                .unwrap_or(person),
        );
        self.runtime()
            .set_member_policy(members_of(&access, &me), Some(reads_of(&access, &me, self)));
        *self.inner.members_from.borrow_mut() = Some((
            person.to_owned(),
            std::sync::Arc::unwrap_or_clone(access.policy),
            kinds,
        ));
        // A change of the members' policy is a change of the governance that names who is in
        // the workspace: its members' resource is read again with it.
        self.inner.members_stale.set(true);
        true
    }

    /// Read the workspace's members' resource again when anything that may have moved it did:
    /// the store moving as a whole or that record of it, the policy, or a member action of this
    /// host's. The reading is handed to the runtime as any other, so the same members read
    /// again move nothing. Whether the reading moved.
    pub fn read_workspace_members(&self) -> bool {
        // A host its embedder publishes the workspace's members to
        // ([`Self::serve_member_reading`]) has no store of that workspace to read them from.
        if self.inner.members_elsewhere.get() {
            return false;
        }
        let moved = self.inner.members_watch.take();
        let id = tilo_resource_store::WORKSPACE_MEMBERS_ID;
        if moved.everything
            || moved.docs.contains(id)
            || moved
                .fields
                .values()
                .any(|fields| fields.iter().any(|(resource, _)| resource == id))
        {
            self.inner.members_stale.set(true);
        }
        if !self.inner.members_stale.replace(false) {
            return false;
        }
        let me_id = self
            .viewer()
            .member_id()
            .cloned()
            .unwrap_or_else(|| MemberId::person(""));
        let rows = match self.member_rows_read(&me_id) {
            // The workspace's governance publishes its members' record; until it has, there is
            // no reading to hand in, and an empty list is not one: a trigger that may tell the
            // members waits for the reading rather than telling nobody. A view that reads it
            // shows what its type starts as meanwhile, rather than waiting on it every frame;
            // the reading handed in when the record arrives replaces this.
            Ok(None) => {
                let _ = self
                    .runtime()
                    .set_absent(&crate::platform::workspace_members());
                return false;
            }
            Ok(Some(rows)) => rows,
            Err(error) => {
                tracing::warn!(%error, "the workspace's members did not read");
                return false;
            }
        };
        self.serve_rows(&rows)
    }

    /// Whether the workspace's members come to this host as a reading its embedder publishes
    /// ([`Self::serve_member_reading`]), never read from this host's own store: a window whose
    /// store is not the workspace whose people it shows, as the shell's is (it keeps its record
    /// in the person's own workspace). A host that read its own store would name nobody of the
    /// workspace in front.
    pub fn let_members_be_read_elsewhere(&self) {
        self.inner.members_elsewhere.set(true);
        self.inner.members_known.set(true);
        self.inner.members_asked.set(true);
    }

    /// Whether the workspace's members come to this host as a reading its embedder publishes
    /// ([`Self::let_members_be_read_elsewhere`]), so a reading it publishes serves them.
    #[must_use]
    pub fn reads_members_elsewhere(&self) -> bool {
        self.inner.members_elsewhere.get()
    }

    /// The workspace's members' resource as its embedder publishes it in the workspace in front,
    /// `content` (`{"people": …}`): who is in it, with what the workspace says of each, named and
    /// drawn from here as from a reading of this host's own store. Whether the reading moved.
    pub fn serve_member_reading(&self, content: &serde_json::Value) -> bool {
        let me_id = self
            .viewer()
            .member_id()
            .cloned()
            .unwrap_or_else(|| MemberId::person(""));
        let rows = rows_of(content, &me_id);
        tracing::debug!(
            rows = rows.len(),
            "a host serves the members it was published"
        );
        self.serve_rows(&rows)
    }

    /// The members `rows` name, as the runtime's members and every view that names a member
    /// reads them: who is in the workspace, and the reading of them a view reads. Whether the
    /// reading moved.
    fn serve_rows(&self, rows: &[collab_types::Member]) -> bool {
        *self.inner.member_rows.borrow_mut() = std::rc::Rc::new(
            rows.iter()
                .map(|row| (row.id.clone(), row.clone()))
                .collect(),
        );
        let me = self.viewer().clone();
        let mine = self
            .authority()
            .access()
            .and_then(|access| access.person_id().map(PersonId::from));
        // The software the policy made members (agents its grants admitted, installed bots) is
        // in the workspace beside its people: the policy names them, not its member list.
        let people: Vec<Actor> = rows
            .iter()
            .filter(|row| !row.removed)
            .map(|row| Actor::Member(row.id.clone()))
            .collect();
        let software: Vec<Actor> = self
            .runtime()
            .member_policy()
            .into_iter()
            .filter(|member| !member.id.is_person())
            .map(|member| member.who())
            .filter(|who| !people.contains(who))
            .collect();
        let people: Vec<serde_json::Value> = people
            .into_iter()
            .chain(software)
            .map(|who| {
                // The person at the window is the runtime's `me`, as the policy names them.
                let who = match (&mine, who.member_id().and_then(MemberId::person_id)) {
                    (Some(mine), Some(person)) if person == mine.as_str() => me.clone(),
                    _ => who,
                };
                tilolang_runtime::value::to_json(&tilolang_runtime::Value::Actor(who))
                    .unwrap_or_default()
            })
            .collect();
        let reading = serde_json::json!({ "people": people });
        let took = self
            .runtime()
            .set_reading(&crate::platform::workspace_members(), &reading.to_string())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "the workspace's members were refused as a reading");
                false
            });
        self.inner.members_known.set(true);
        self.inner.members_asked.set(false);
        took
    }

    /// Who the workspace's members' resource names now, as this host's store reads it: the
    /// actors alone, as the add-member and remove-member actions write them back. A row that
    /// carries what the workspace says of a member names the member in its `actor`.
    fn workspace_people(&self) -> Result<Vec<Actor>, String> {
        let reading = self
            .store()
            .workspace_members()
            .map_err(|error| error.to_string())?;
        Ok(reading
            .as_ref()
            .and_then(|reading| reading.get("people"))
            .and_then(serde_json::Value::as_array)
            .map(|people| {
                people
                    .iter()
                    .filter_map(|row| {
                        let named = row.get("actor").unwrap_or(row);
                        match tilolang_runtime::value::from_json(named) {
                            Ok(tilolang_runtime::Value::Actor(who)) => Some(who),
                            _ => tilo_failure::ignored!(
                                "a row that names no actor is no member: the list holds the rows that do",
                                serde_json::from_value::<Actor>(named.clone())
                            )
                            .ok(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The members the workspace's members' resource names now, each with what the workspace
    /// says of them, as this host's store reads it: the one member list every surface showing
    /// people reads, here and on a replica that synced it.
    fn member_rows_read(&self, me: &MemberId) -> Result<Option<Vec<collab_types::Member>>, String> {
        let reading = self
            .store()
            .workspace_members()
            .map_err(|error| error.to_string())?;
        Ok(reading.map(|reading| rows_of(&reading, me)))
    }

    /// The members this host last read of the workspace's members' resource, by id, with what
    /// the workspace says of each: what a view calls people by and how it draws them.
    #[must_use]
    pub fn member_rows(
        &self,
    ) -> std::rc::Rc<std::collections::HashMap<MemberId, collab_types::Member>> {
        std::rc::Rc::clone(&self.inner.member_rows.borrow())
    }

    /// Carry out the workspace's add-member action for `who`, as this host's viewer: they are in
    /// its members' resource from now on, and the runtime reads them there at once.
    ///
    /// # Errors
    /// The store refused the change: a workspace's members change through its governance.
    pub fn add_workspace_member(&self, who: &Actor) -> Result<(), String> {
        self.change_workspace_members(tilo_action_policy::roles::governance::ADD_MEMBER, who)
    }

    /// Carry out the workspace's remove-member action for `who`, as this host's viewer.
    ///
    /// # Errors
    /// As [`Self::add_workspace_member`].
    pub fn remove_workspace_member(&self, who: &Actor) -> Result<(), String> {
        self.change_workspace_members(tilo_action_policy::roles::governance::REMOVE_MEMBER, who)
    }

    /// Decide by `access`, `me` the person it is read for, rather than by the host's own
    /// authority: the members' roles and who of them reads what (`members(r)`,
    /// `std.may_read`). Who is in the workspace is still its members' resource.
    pub fn give_member_policy(&self, access: &MemberAccess, me: &PersonId) {
        // The store's one read decision follows the same access: a reader field naming an agent
        // is placed as that agent's person where the policy lets it read, as a window's host
        // places it. A record host has no access of its own, so the one given here is the
        // store's too.
        self.inner.record_reads.follow_with_kinds(
            Some(access.clone()),
            self.viewer(),
            kind_nodes(self),
        );
        self.take_member_policy(members_of(access, me), Some(reads_of(access, me, self)));
        // A store kept by a workspace answers the members' resource from its governance; one
        // held in this process (a record served with no window, a test's) may hold no record of
        // it, and the policy the host was opened with names the workspace's people until one is
        // kept.
        if matches!(self.store().workspace_members(), Ok(None)) {
            let people: Vec<serde_json::Value> = members_of(access, me)
                .into_iter()
                .map(|member| {
                    tilolang_runtime::value::to_json(&tilolang_runtime::Value::Actor(member.who()))
                        .unwrap_or_default()
                })
                .collect();
            self.serve_member_reading(&serde_json::json!({ "people": people }));
        }
    }

    /// Decide by `policy`, the members' roles and the software made members, and by `reads`
    /// who of them reads what, rather than by the host's own authority; the workspace's
    /// members' reading names the software with its people from now on.
    pub fn take_member_policy(&self, policy: Vec<Member>, reads: Option<Rc<Reads>>) {
        self.runtime().set_member_policy(policy, reads);
        self.inner.members_stale.set(true);
        self.read_workspace_members();
    }

    fn change_workspace_members(&self, verb: &str, who: &Actor) -> Result<(), String> {
        let mut people = self.workspace_people()?;
        let held = people.contains(who);
        let adds = verb == tilo_action_policy::roles::governance::ADD_MEMBER;
        if held == adds {
            return Ok(());
        }
        if adds {
            people.push(who.clone());
        } else {
            people.retain(|person| person != who);
        }
        let people = people
            .into_iter()
            .map(|who| tilolang_runtime::value::to_json(&tilolang_runtime::Value::Actor(who)))
            .collect::<Result<Vec<_>, _>>()?;
        self.store()
            .change_workspace_members(verb, self.viewer(), serde_json::Value::Array(people))
            .map_err(|error| error.to_string())?;
        self.inner.members_stale.set(true);
        self.read_workspace_members();
        Ok(())
    }
}

/// Whether `who` may read `node` by `access`'s policy: a person by their own party; an agent
/// (its id and its person) as its turns for its person are decided, by its preset narrowed to
/// that person's reach; any other software by the grants the workspace gave it.
fn reads_node(access: &MemberAccess, me: &PersonId, who: &Actor, node: &TreePath) -> bool {
    let policy = &access.policy;
    let earlier = crate::schema::earlier_ids();
    let cx = Context::at_time(tilo_clock::wall()).with_earlier_ids(&earlier);
    if let Some(MemberId::Agent { id: agent, person }) = who.member_id() {
        let acting = Acting::handed_agent(agent.as_str(), person_of(access, me, person));
        let preset = acting.handed_preset(&policy.workspace);
        return Policy::of_acting(&acting, policy, &preset)
            .decide(node, ActionKind::Read, &cx)
            .answer
            == Answer::Allow;
    }
    let party = match who.member_id() {
        Some(MemberId::Person(person)) => policy.party(person_of(access, me, person)),
        // Software reads what the workspace grants it, and what the access it acts by carries
        // (a bot's own, made for its program).
        Some(_) => match tilo_action_policy::grantee_of(who) {
            Some(grantee) => policy.party_of(grantee).with_grants(&access.granted),
            None => return false,
        },
        // An automation or an app is no member: it reads as the member it runs for.
        None => return false,
    };
    Policy::person(party)
        .decide(node, ActionKind::Read, &cx)
        .answer
        == Answer::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::{Grant, Role, WorkspacePolicy};
    use tilo_action_types::KindSet;

    fn access() -> MemberAccess {
        let mut policy = WorkspacePolicy::new("workspace", "ada");
        policy.members.insert("bo".to_owned(), Role::Editor);
        policy.members.insert("cy".to_owned(), Role::Viewer);
        policy.grants.push(Grant::new(
            "agent-member:planner",
            Grantee::agent("planner", "ada"),
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
                    Member::agent("planner", "me"),
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
            // Each agent is the pair of its id and its person: `scout` of the person at the
            // window and `scout` of Cy are two agents.
            let all: [Actor; 6] = [
                MemberId::person("me").into(),
                MemberId::person("bo").into(),
                MemberId::person("cy").into(),
                MemberId::agent("planner", "me").into(),
                MemberId::agent("scout", "me").into(),
                MemberId::agent("scout", "cy").into(),
            ];
            for who in &all {
                assert!(
                    reads_node(&access, &PersonId::from("me"), who, &node("open")),
                    "{who} reads what the workspace holds"
                );
            }
            let secret: Vec<String> = all
                .iter()
                .filter(|who| reads_node(&access, &PersonId::from("me"), who, &node("secret")))
                .map(|who| match who.member_id().and_then(MemberId::as_agent) {
                    Some((agent, person)) => format!("{agent}@{person}"),
                    None => who.id().to_owned(),
                })
                .collect();
            assert_eq!(
                secret,
                ["me", "bo", "planner@me", "scout@me"],
                "Cy, whom the grant keeps out, does not, nor does his agent of the same id as Ada's"
            );
        }
    }
}
