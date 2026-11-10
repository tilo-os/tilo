//! Who is in the workspace: `workspace.members`, `members(r)`, and what a principal is
//! (`p.kind()`, `p.role()`).
//!
//! The members are the platform's: the host reads them from the workspace's membership (its
//! members, their roles and grants) and gives them to the runtime whenever they change
//! ([`crate::Runtime::set_members`]). The runtime keeps no copy of its own beyond the last
//! answer, and an app keeps none at all. A view that read them runs again when they change, as
//! one that read the lenses on a resource does.

use std::rc::Rc;

use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::ids::Principal;
use crate::value::{ResourceRef, Value};
use crate::view_state::StateKey;

/// What a member is: a person, or software acting in the workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MemberKind {
    #[default]
    Person,
    Agent,
}

impl MemberKind {
    /// The case of the language's `PrincipalKind` it is, by its place.
    const fn case(self) -> i64 {
        match self {
            Self::Person => 0,
            Self::Agent => 1,
        }
    }
}

/// One member of the workspace, as its membership records them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub who: Principal,
    pub kind: MemberKind,
    /// Their role, as the workspace's policy names it (`owner`, `editor`), when they hold one.
    pub role: Option<String>,
}

impl Member {
    #[must_use]
    pub fn person(who: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            who: Principal::new(who),
            kind: MemberKind::Person,
            role: Some(role.into()),
        }
    }

    #[must_use]
    pub fn agent(who: impl Into<String>) -> Self {
        Self {
            who: Principal::new(who),
            kind: MemberKind::Agent,
            role: None,
        }
    }
}

/// Whether a member can read a resource, as the workspace's grants decide.
pub type Reads = dyn Fn(&Principal, &ResourceRef) -> bool;

/// The members the host gave last, and how it decides who reads what.
#[derive(Default)]
pub struct Members {
    list: Vec<Member>,
    reads: Option<Rc<Reads>>,
}

impl Members {
    /// The members, in the order the host gave them.
    #[must_use]
    pub fn list(&self) -> &[Member] {
        &self.list
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
    /// Take the workspace's members now, and how its grants decide who reads what (`None`:
    /// every member reads every resource of the workspace).
    pub(crate) fn set_members(&self, list: Vec<Member>, reads: Option<Rc<Reads>>) {
        let value = Value::List(
            list.iter()
                .map(|member| Value::Principal(member.who.clone()))
                .collect(),
        );
        *self.members.borrow_mut() = Members { list, reads };
        // Grants may have moved even where the list did not: every reader runs again.
        self.view_states.borrow_mut().set(members_key(), value);
    }

    /// A scenario named a person (`person("Mia")`): they are in the workspace it stages, so a
    /// view that offers the members offers them too, under the name the scenario gave.
    pub(crate) fn named_person(&self, who: crate::ids::Principal) {
        self.named_member(who, MemberKind::Person);
    }

    /// A scenario named software (`agent("Scout")`): it is a member of the workspace it stages,
    /// as an agent of it is, and views offer it beside the people and mark it as software.
    pub(crate) fn named_agent(&self, who: crate::ids::Principal) {
        self.named_member(who, MemberKind::Agent);
    }

    /// Adds the member `who` of `kind` to the workspace, unless it is one already.
    fn named_member(&self, who: crate::ids::Principal, kind: MemberKind) {
        if self
            .members
            .borrow()
            .list
            .iter()
            .any(|member| member.who == who)
        {
            return;
        }
        let (mut list, reads) = {
            let members = self.members.borrow();
            (members.list.clone(), members.reads.clone())
        };
        list.push(Member {
            who,
            kind,
            role: None,
        });
        self.set_members(list, reads);
    }

    /// The members, noted as read by the running view.
    pub(crate) fn read_members(&self) -> Vec<Member> {
        let key = members_key();
        let (value, generation) = {
            let states = self.view_states.borrow();
            (
                states.get(&key).cloned().unwrap_or(Value::List(Vec::new())),
                states.generation(&key),
            )
        };
        self.note_state_read(key, generation, &value);
        self.members.borrow().list.clone()
    }

    fn member(&self, who: &Principal) -> Option<Member> {
        self.members
            .borrow()
            .list
            .iter()
            .find(|member| member.who == *who)
            .cloned()
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
            let who = Value::Principal(Principal::new(created.who));
            if !said.contains(&who) {
                said.push(who);
            }
        }
        Ok(said)
    }

    pub(crate) fn members_import(
        &self,
        imp: crate::abi::Imp,
        a: &Args<'_>,
    ) -> HResult<Option<Wasm>> {
        use crate::abi::Imp;
        match imp {
            Imp::WorkspaceMembers => {
                let list = self.read_members();
                self.ret_h(principals(list.iter()))
            }
            Imp::Members => {
                let target = self.ref_arg(a.h(0)?)?;
                let list = self.read_members();
                let reads = self.members.borrow().reads.clone();
                let readers = list.iter().filter(|member| {
                    reads
                        .as_ref()
                        .is_none_or(|reads| reads(&member.who, &target.target))
                });
                self.ret_h(principals(readers))
            }
            Imp::Speakers => {
                let max = usize::try_from(a.long(1)?).unwrap_or(0);
                self.ret_h(Value::List(self.said_by(a.h(0)?, max)?))
            }
            Imp::PrincipalKind => {
                let who = self.principal_arg(a.h(0)?)?;
                let _ = self.read_members();
                let kind = self.member(&who).map_or_else(
                    // Someone no longer a member keeps what their id says they are.
                    || {
                        if who.0.starts_with("agent:") {
                            MemberKind::Agent
                        } else {
                            MemberKind::Person
                        }
                    },
                    |member| member.kind,
                );
                crate::core::ret_i64(kind.case())
            }
            Imp::PrincipalRole => {
                let who = self.principal_arg(a.h(0)?)?;
                let _ = self.read_members();
                let role = self.member(&who).and_then(|member| member.role);
                self.ret_h(role.map_or_else(Value::none, |role| Value::some(Value::Text(role))))
            }
            _ => trap("not a members import"),
        }
    }
}

pub(crate) fn principals<'a>(members: impl Iterator<Item = &'a Member>) -> Value {
    Value::List(
        members
            .map(|member| Value::Principal(member.who.clone()))
            .collect(),
    )
}
