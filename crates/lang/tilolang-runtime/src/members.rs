//! Who is in the workspace as code asks it: `members(r)`, `std.may_read`, and what an actor is
//! (`p.kind()`, `p.role()`, `p.app()`).
//!
//! Who is in the workspace is its `workspace/members` resource: the host reads it through the
//! store it reads every record through, keyed by the workspace it runs in, and hands the runtime
//! its reading ([`crate::Runtime::set_members_root`]). Their roles and who of them reads what are
//! the workspace's policy, which the host gives whenever it changes
//! ([`crate::Runtime::set_member_policy`]). A scenario's people join through the workspace's
//! own add-member action ([`crate::Runtime::set_member_adder`]). The runtime keeps no list of
//! its own beyond the last reading, and an app keeps none at all. A view that read them runs
//! again when either changes, as one that read the lenses on a resource does.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::policy::{MemberId, Role};

use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::ids::Actor;
use crate::meta::ReaderRule;
use crate::value::{ResourceRef, Value};
use crate::view_state::StateKey;

/// One member of the workspace, as the session's member list has them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: MemberId,
    pub role: Option<Role>,
}

impl Member {
    #[track_caller]
    #[must_use]
    pub fn person(id: impl Into<tilo_actions::policy::PersonId>, role: Role) -> Self {
        Self {
            id: MemberId::person(id),
            role: Some(role),
        }
    }

    /// The agent `id` of `person`.
    #[must_use]
    pub fn agent(id: impl Into<String>, person: impl Into<tilo_actions::policy::PersonId>) -> Self {
        Self {
            id: MemberId::agent(id, person),
            role: None,
        }
    }

    /// An installed bot: software the workspace made a member, with the grants it made it.
    #[must_use]
    pub fn bot(id: impl Into<String>) -> Self {
        Self {
            id: MemberId::bot(id),
            role: None,
        }
    }

    /// The actor the language names them by.
    #[must_use]
    pub fn who(&self) -> Actor {
        Actor::Member(self.id.clone())
    }
}

/// The case of the language's `ActorKind` `who` is: a person, else software.
fn kind_case(who: &Actor) -> i64 {
    i64::from(!who.member_id().is_some_and(MemberId::is_person))
}

/// Whether a member can read a resource, as the workspace's grants decide.
pub type Reads = dyn Fn(&Actor, &ResourceRef) -> bool;

/// Carry out the workspace's add-member action for `who`: what a scenario naming a person
/// (`person("Mia")`) does, as a seed writes only through declared actions.
pub type MemberAdder = dyn Fn(&Actor) -> Result<(), String>;

/// The workspace's policy as the host gave it last: the role of each member it names, the
/// software it made members by its grants, and how it decides who reads what.
#[derive(Default)]
pub struct Members {
    policy: Vec<Member>,
    reads: Option<Rc<Reads>>,
    /// The people of the members' reading last parsed, by the reading's generation: a reading
    /// is parsed once, however many views read it.
    people: RefCell<Option<(u64, Rc<Vec<Actor>>)>>,
    adder: Option<Rc<MemberAdder>>,
}

impl Members {
    /// The members the policy names, with their roles, in the order the host gave them.
    #[must_use]
    pub fn policy(&self) -> &[Member] {
        &self.policy
    }
}

/// The view state slot the members are kept under: a view that read them runs again when the
/// host gives new ones.
fn members_key() -> StateKey {
    StateKey::runtime(
        crate::platform::LENS_WINDOW,
        None,
        "$workspace-members".to_owned(),
    )
}

/// How many of a conversation's last items a rich input's drawer looks back over for its
/// people: who spoke longer ago than that is a member like any other, and the cost stays the
/// same however long the conversation grows.
const RECENT: usize = 50;

impl Core {
    /// Take the workspace's policy now: each member's role, the software it made members, and
    /// how its grants decide who reads what (`None`: every member reads every resource of the
    /// workspace). Who is in it is read from the members' resource ([`Self::read_members`]).
    pub(crate) fn set_member_policy(&self, policy: Vec<Member>, reads: Option<Rc<Reads>>) {
        let value = Value::List(
            policy
                .iter()
                .map(|member| Value::Actor(member.who()))
                .collect(),
        );
        {
            let mut members = self.members.borrow_mut();
            members.policy = policy;
            members.reads = reads;
        }
        // Grants may have moved even where the roles did not: every reader runs again.
        self.view_states.borrow_mut().set(members_key(), value);
    }

    /// Read who is in the workspace from `root`, its members' resource, whose reading the host
    /// hands in as it hands in any other ([`crate::Runtime::set_reading`]).
    pub(crate) fn set_members_root(&self, root: &ResourceRef) {
        *self.members_root.borrow_mut() = Some(root.clone());
    }

    /// Carry out the workspace's add-member action with `adder` from now on.
    pub(crate) fn set_member_adder(&self, adder: Rc<MemberAdder>) {
        self.members.borrow_mut().adder = Some(adder);
    }

    /// A scenario named a person (`person("Mia")`): they are in the workspace it stages, so a
    /// view that offers the members offers them too. Their id is the name the scenario gave:
    /// a person who is not a root key is a sample one, and reads as that name.
    pub(crate) fn named_person(&self, name: &str) -> HResult<Actor> {
        match tilo_actions::policy::PersonId::parse(name) {
            Ok(id) if !name.is_empty() => self.named_member(MemberId::Person(id)),
            _ => trap(format!("person: a name without `:`, got {name:?}")),
        }
    }

    /// A scenario named software (`agent("Scout")`): it is a member of the workspace it stages,
    /// as an agent of it is, and views offer it beside the people and mark it as software.
    pub(crate) fn named_agent(&self, name: &str) -> HResult<Actor> {
        let Actor::Member(MemberId::Person(person) | MemberId::Agent { person, .. }) =
            self.looking()?
        else {
            return trap("agent: a scenario names an agent of the person it runs as");
        };
        self.named_member(MemberId::agent(format!("scenario:{name}"), person))
    }

    /// Adds the member `id` to the workspace through its add-member action, unless it is one
    /// already, and names them. A runtime deciding by a real workspace's access adds no one: its
    /// members are the workspace's, and a scenario run there only names people.
    fn named_member(&self, id: MemberId) -> HResult<Actor> {
        let who = Actor::Member(id.clone());
        if self.members_decided.get() || self.member_list().iter().any(|member| member.id == id) {
            return Ok(who);
        }
        let adder = self.members.borrow().adder.clone();
        let Some(adder) = adder else {
            return trap(format!(
                "{}: this host adds nobody to its workspace",
                crate::ids::actor_text(&who)
            ));
        };
        match adder(&who) {
            Ok(()) => Ok(who),
            Err(why) => trap(format!(
                "{} could not be added to the workspace: {why}",
                crate::ids::actor_text(&who)
            )),
        }
    }

    /// The people the members' reading names, parsed once per reading.
    fn people(&self) -> Rc<Vec<Actor>> {
        let Some(root) = self.members_root.borrow().clone() else {
            return Rc::default();
        };
        let foreign = self.foreign.borrow();
        let generation = foreign.generation(&root);
        let members = self.members.borrow();
        if let Some((held, people)) = members.people.borrow().as_ref()
            && *held == generation
        {
            return Rc::clone(people);
        }
        let people: Vec<Actor> = foreign
            .held(&root)
            .and_then(|reading| reading.get("people"))
            .and_then(serde_json::Value::as_array)
            .map(|people| {
                people
                    .iter()
                    .filter_map(|person| match crate::value::from_json(person) {
                        Ok(Value::Actor(who)) => Some(who),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let people = Rc::new(people);
        *members.people.borrow_mut() = Some((generation, Rc::clone(&people)));
        people
    }

    /// Everyone in the workspace now, not noted as read: the people its members' resource names,
    /// each with the role the policy gives them, then the software the policy made members.
    pub(crate) fn member_list(&self) -> Vec<Member> {
        let people = self.people();
        let members = self.members.borrow();
        let mut list: Vec<Member> = people
            .iter()
            .filter_map(|who| who.member_id().cloned())
            .map(|id| {
                let role = members
                    .policy
                    .iter()
                    .find(|member| member.id == id)
                    .and_then(|member| member.role);
                Member { id, role }
            })
            .collect();
        for member in &members.policy {
            if !member.id.is_person() && !list.iter().any(|held| held.id == member.id) {
                list.push(member.clone());
            }
        }
        list
    }

    /// Whether the host gave the workspace's policy.
    pub(crate) fn knows_policy(&self) -> bool {
        !self.members.borrow().policy.is_empty()
    }

    /// The members, noted as read by the running view: it runs again when the members'
    /// reading or the policy moves.
    pub(crate) fn read_members(&self) -> Vec<Member> {
        let mut keys = vec![members_key()];
        if let Some(root) = self.members_root.borrow().as_ref() {
            keys.push(crate::foreign::reading_key(root));
        }
        for key in keys {
            let (value, generation) = {
                let states = self.view_states.borrow();
                (
                    states.get(&key).cloned().unwrap_or(Value::List(Vec::new())),
                    states.generation(&key),
                )
            };
            self.note_state_read(key, generation, &value);
        }
        self.member_list()
    }

    pub(crate) fn member(&self, who: &Actor) -> Option<Member> {
        self.member_list()
            .into_iter()
            .find(|member| who.member_id() == Some(&member.id))
    }

    /// Who spoke in the conversation of `items`: who made them, most recent first, each once,
    /// at most `max` of them. It reads the last [`RECENT`] items and no more — the items
    /// themselves, never a history of what they are about — so a drawer's cost is bounded by
    /// the conversation and it runs again when one of those items' makers is made.
    fn said_by(&self, items: i32, max: usize) -> HResult<Vec<Value>> {
        let items = match self.val(items)? {
            Value::List(items) => items,
            Value::Coll(coll) => self.coll_take_end(&coll, RECENT)?,
            other => {
                return trap(format!(
                    "speakers: expected the items of a conversation, got {}",
                    other.kind_name()
                ));
            }
        };
        let mut said: Vec<Value> = Vec::new();
        for item in items.iter().rev().take(RECENT) {
            if said.len() >= max {
                break;
            }
            let Value::Ref(r) = item else {
                continue;
            };
            // Who made an item of an app this runtime does not run is in its history, kept
            // where that app runs: none of it is held here.
            if crate::foreign::is_foreign(&r.target) {
                continue;
            }
            let created = self
                .store
                .borrow()
                .version_end(&r.target, false)
                .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
            self.read_versions_of(&r.target);
            let Some(created) = created else {
                continue;
            };
            let who = Value::Actor(self.made_by(&created, &r.target));
            if !said.contains(&who) {
                said.push(who);
            }
        }
        Ok(said)
    }

    /// Whether `who` reads `target`: the answer `members(r)` gives for them, and none for an
    /// actor who is no member, an automation or an app among them (`std.may_read`).
    pub(crate) fn may_read(&self, who: &Actor, target: &ResourceRef) -> bool {
        let target = &self.typed(target);
        let list = self.read_members();
        let reads = self.members.borrow().reads.clone();
        let readers = self.readers_of(target);
        list.iter().any(|member| {
            who.member_id() == Some(&member.id)
                && member_reads(reads.as_deref(), &readers, member, target)
        })
    }

    /// `target` with the type the store keeps it as, when the reference names none: the
    /// workspace's grants decide a read at the resource's app and kind, so an untyped reference
    /// would be decided at the workspace and miss every grant on the resource itself.
    fn typed(&self, target: &ResourceRef) -> ResourceRef {
        let mut typed = target.clone();
        if typed.ty.is_empty() {
            typed.ty = self.stored_type(target);
        }
        typed
    }

    /// Whether every record holding `target` that says who reads it names `who`
    /// ([`Self::readers_of`]).
    pub(crate) fn readers_admit(&self, who: &Actor, target: &ResourceRef) -> bool {
        self.readers_of(target)
            .iter()
            .all(|people| people.contains(who))
    }

    /// Who the records holding `target` say read it: for each resource from `target` up to its
    /// document whose type names its readers (`type Direct (readers: people)`), the people that
    /// field holds. A member reads `target` only when every one of them names them, whatever the
    /// grants say. The fields are noted as read by the running view, so it runs again when they
    /// change.
    pub(crate) fn readers_of(&self, target: &ResourceRef) -> Vec<Vec<Actor>> {
        self.readers_made_by(target, None)
    }

    /// [`Self::readers_of`], `maker` the one making a record with no version yet: its author.
    fn readers_made_by(&self, target: &ResourceRef, maker: Option<&Actor>) -> Vec<Vec<Actor>> {
        let mut out = Vec::new();
        let mut at = Some(target.clone());
        while let Some(here) = at {
            if let Some(people) = self.readers_here(&here, maker) {
                out.push(people);
            }
            at = self
                .store
                .borrow()
                .container(&here)
                .ok()
                .flatten()
                .map(|(holder, _)| holder);
        }
        out
    }

    /// Who alone reads a change of the resources `touched` (as [`ResourceRef::id_string`] writes
    /// them): the members every record holding one of them names as its readers, when any does;
    /// `None` when none of them is held by a record that names its readers. What history lists the
    /// change to (`tilo_actions::READERS_FACET`). `maker` makes the change: the author of a record
    /// it makes, which has no version yet.
    pub(crate) fn change_readers(
        &self,
        touched: &[String],
        maker: Option<&Actor>,
    ) -> Option<Vec<MemberId>> {
        let mut out: Option<Vec<MemberId>> = None;
        for at in touched
            .iter()
            .filter_map(|id| ResourceRef::parse_id(id, String::new()))
        {
            for people in self.readers_made_by(&at, maker) {
                let named: Vec<MemberId> = people
                    .iter()
                    .filter_map(|who| who.member_id().cloned())
                    .collect();
                out = Some(match out {
                    None => named,
                    Some(held) => held.into_iter().filter(|m| named.contains(m)).collect(),
                });
            }
        }
        out
    }

    /// The people the resource `at` says read it, when its type names its readers. A record whose
    /// type names its author among its readers and that has no version yet is being made, by
    /// `maker`; with no maker its author is not known, and nobody reads it, never everybody.
    fn readers_here(&self, at: &ResourceRef, maker: Option<&Actor>) -> Option<Vec<Actor>> {
        let ty = if at.ty.is_empty() {
            self.stored_type(at)
        } else {
            at.ty.clone()
        };
        let (_, info) = self.type_of_ref(&ty).ok()?;
        match info.readers.clone()? {
            ReaderRule::Field { field } => {
                let field = info.field_by_id_or_name(&field)?.clone();
                if let Some(vs) = self.frames.borrow_mut().view_mut() {
                    vs.reads.push((at.id_string(), field.id.clone()));
                }
                // A record whose readers cannot be read is read by nobody: never by everyone.
                let Ok(Value::List(people)) = self.read_value(at, &field, None) else {
                    return Some(Vec::new());
                };
                Some(
                    people
                        .into_iter()
                        .filter_map(|who| match who {
                            Value::Actor(who) => Some(who),
                            _ => None,
                        })
                        .collect(),
                )
            }
            // `(readers: author)`: whoever made the record reads it; once the field `until`
            // names holds, it names no readers of its own and whoever reads its document reads
            // it, as the store hands it to the workspace's readers.
            ReaderRule::Author { until } => {
                if let Some(until) = until {
                    let field = info.field_by_id_or_name(&until)?.clone();
                    if let Some(vs) = self.frames.borrow_mut().view_mut() {
                        vs.reads.push((at.id_string(), field.id.clone()));
                    }
                    if self.read_value(at, &field, None) == Ok(Value::Bool(true)) {
                        return None;
                    }
                }
                Some(self.author_of(at, maker).into_iter().collect())
            }
            // `(readers: author and moderator)`: whoever made the record, and every member whose
            // role includes the one named, as the workspace has them now; the store seals it to
            // that role's group, which follows the same roles.
            ReaderRule::AuthorAnd { role } => {
                let mut readers: Vec<Actor> = self.author_of(at, maker).into_iter().collect();
                for member in self.read_members() {
                    let who = member.who();
                    if member.role.is_some_and(|held| held.includes(role))
                        && !readers.contains(&who)
                    {
                        readers.push(who);
                    }
                }
                Some(readers)
            }
        }
    }

    /// Who made the record `at`: whoever its first version names, else `maker`, who is making it
    /// now. `None` where neither is known.
    fn author_of(&self, at: &ResourceRef, maker: Option<&Actor>) -> Option<Actor> {
        let created = self.store.borrow().version_end(at, false).ok().flatten();
        match created {
            Some(created) => Some(self.whose(&created, at)),
            None => maker.cloned(),
        }
    }

    pub(crate) fn members_import(
        &self,
        imp: crate::abi::Imp,
        a: &Args<'_>,
    ) -> HResult<Option<Wasm>> {
        use crate::abi::Imp;
        match imp {
            Imp::Members => {
                let target = self.typed(&self.ref_arg(a.h(0)?)?.target);
                let list = self.read_members();
                let reads = self.members.borrow().reads.clone();
                let readers = self.readers_of(&target);
                let readers = list
                    .iter()
                    .filter(|member| member_reads(reads.as_deref(), &readers, member, &target));
                self.ret_h(actors(readers))
            }
            // Whether one actor reads a resource: the answer `members(r)` gives for them, and
            // none for an actor who is no member, an automation or an app among them.
            Imp::MayRead => {
                let who = self.actor_arg(a.h(0)?)?;
                let target = self.ref_arg(a.h(1)?)?;
                crate::core::ret_i32(i32::from(self.may_read(&who, &target.target)))
            }
            Imp::Speakers => {
                let max = usize::try_from(a.long(1)?).unwrap_or(0);
                self.ret_h(Value::List(self.said_by(a.h(0)?, max)?))
            }
            Imp::ActorKind => {
                let who = self.actor_arg(a.h(0)?)?;
                crate::core::ret_i64(kind_case(&who))
            }
            // What names the actor whatever they are called: the same on every device.
            Imp::ActorId => {
                let who = self.actor_arg(a.h(0)?)?;
                self.ret_h(Value::Text(crate::ids::actor_text(&who)))
            }
            // The app whose process the invoker calls through, as the action API admitted it.
            Imp::ActorApp => {
                let who = self.actor_arg(a.h(0)?)?;
                self.ret_h(
                    self.app_of_invoker(&who)
                        .map_or_else(Value::none, |app| Value::some(Value::Text(app))),
                )
            }
            Imp::ActorRole => {
                let who = self.actor_arg(a.h(0)?)?;
                let _ = self.read_members();
                let role = self.member(&who).and_then(|member| member.role);
                self.ret_h(role.map_or_else(Value::none, |role| {
                    Value::some(Value::Text(role.key().to_owned()))
                }))
            }
            _ => trap("not a members import"),
        }
    }
}

/// Whether `member` reads `target`: every one of `readers`, the records holding it that say who
/// reads them ([`Core::readers_of`]), names them, and `reads`, the host's grants, let them (none
/// given: every member reads every resource of the workspace).
fn member_reads(
    reads: Option<&Reads>,
    readers: &[Vec<Actor>],
    member: &Member,
    target: &ResourceRef,
) -> bool {
    let who = member.who();
    readers.iter().all(|people| people.contains(&who))
        && reads.is_none_or(|reads| reads(&who, target))
}

pub(crate) fn actors<'a>(members: impl Iterator<Item = &'a Member>) -> Value {
    Value::List(members.map(|member| Value::Actor(member.who())).collect())
}
