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
    StateKey {
        window: crate::platform::LENS_WINDOW.to_owned(),
        principal: None,
        state: "$workspace-members".to_owned(),
    }
}

/// How many of those who spoke last a rich input's drawer offers first (chat.md).
const SPEAKERS: usize = 5;

/// How many of a document's last changes the drawer looks back over for them: who spoke
/// longer ago than that is a member like any other, and the cost stays the same however long
/// the conversation's history grows.
const RECENT: usize = 200;

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

    /// The members, noted as read by the running view.
    fn read_members(&self) -> HResult<Vec<Member>> {
        let key = members_key();
        let (value, generation) = {
            let states = self.view_states.borrow();
            (
                states.get(&key).cloned().unwrap_or(Value::List(Vec::new())),
                states.generation(&key),
            )
        };
        self.note_state_read(key, generation, &value)?;
        Ok(self.members.borrow().list.clone())
    }

    fn member(&self, who: &Principal) -> Option<Member> {
        self.members
            .borrow()
            .list
            .iter()
            .find(|member| member.who == *who)
            .cloned()
    }

    pub(crate) fn members_import(
        &self,
        imp: crate::abi::Imp,
        a: &Args<'_>,
    ) -> HResult<Option<Wasm>> {
        use crate::abi::Imp;
        match imp {
            Imp::WorkspaceMembers => {
                let list = self.read_members()?;
                self.ret_h(principals(list.iter()))
            }
            Imp::Members => {
                let target = self.ref_arg(a.h(0)?)?;
                let list = self.read_members()?;
                let reads = self.members.borrow().reads.clone();
                let readers = list.iter().filter(|member| {
                    reads
                        .as_ref()
                        .is_none_or(|reads| reads(&member.who, &target.target))
                });
                self.ret_h(principals(readers))
            }
            Imp::Speakers => {
                let target = self.ref_arg(a.h(0)?)?;
                let said = self
                    .store
                    .borrow()
                    .recent_authors(&target.target, SPEAKERS, RECENT)
                    .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
                self.read_versions_of(&target.target)?;
                self.ret_h(Value::List(
                    said.into_iter()
                        .map(|who| Value::Principal(Principal(who)))
                        .collect(),
                ))
            }
            Imp::PrincipalKind => {
                let who = self.principal_arg(a.h(0)?)?;
                let _ = self.read_members()?;
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
                let _ = self.read_members()?;
                let role = self.member(&who).and_then(|member| member.role);
                self.ret_h(role.map_or_else(Value::none, |role| Value::some(Value::Text(role))))
            }
            _ => trap("not a members import"),
        }
    }
}

fn principals<'a>(members: impl Iterator<Item = &'a Member>) -> Value {
    Value::List(
        members
            .map(|member| Value::Principal(member.who.clone()))
            .collect(),
    )
}
