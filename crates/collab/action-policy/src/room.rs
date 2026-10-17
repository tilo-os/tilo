//! A room's authority: its owner, its members' roles and its grants, changed
//! only through the room's governance actions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ActionKind, Answer, Authority, Context, Grant, GrantId, Lifetime, Party, Role, Subject,
    TreePath,
    roles::{self, governance},
};

/// Everything that decides authority in one room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPolicy {
    pub room: String,
    /// The owner, by person id. Structural: holds everything and cannot be
    /// removed, demoted or denied by a grant.
    pub owner: Option<String>,
    /// Members other than the owner and the role preset each holds.
    pub members: BTreeMap<String, Role>,
    /// When the room admitted each member, Unix milliseconds, by person id.
    /// Recorded once, by whoever admitted them, and kept through role
    /// changes; a member admitted before rooms recorded it has none. People
    /// who share a name are numbered in this order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub joined: BTreeMap<String, u64>,
    /// Every grant in the room, role presets included.
    pub grants: Vec<Grant>,
}

/// A change to a room's authority. Each is one governance action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    AddMember {
        person: String,
        role: Role,
    },
    RemoveMember {
        person: String,
    },
    /// Remove `person` (when a member) and keep them out until unbanned.
    Ban {
        person: String,
    },
    /// Let a banned `person` be admitted again.
    Unban {
        person: String,
    },
    /// Stop `person` changing anything until `until_ms` (Unix ms). A new
    /// restriction replaces the one they hold.
    Restrict {
        person: String,
        until_ms: u64,
    },
    /// Lift `person`'s restriction before it lapses.
    Unrestrict {
        person: String,
    },
    SetRole {
        person: String,
        role: Role,
    },
    Grant(Grant),
    Revoke(GrantId),
}

impl Change {
    /// The governance action this change is.
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::AddMember { .. } => governance::ADD_MEMBER,
            Self::RemoveMember { .. } => governance::REMOVE_MEMBER,
            Self::Ban { .. } => governance::BAN,
            Self::Unban { .. } => governance::UNBAN,
            Self::Restrict { .. } => governance::RESTRICT,
            Self::Unrestrict { .. } => governance::UNRESTRICT,
            Self::SetRole { .. } => governance::SET_ROLE,
            Self::Grant(_) => governance::GRANT,
            Self::Revoke(_) => governance::REVOKE,
        }
    }
}

/// Why a governance action was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The actor's grants do not allow the action; carries the explanation.
    NotPermitted(String),
    /// The action asks first and no approval was witnessed.
    NeedsApproval(String),
    /// The owner cannot be removed, demoted or made a member.
    Owner,
    /// Ownership is not handed out through membership.
    OwnerRole,
    /// An invite or membership above the actor's own role.
    AboveActorRole {
        actor: Option<Role>,
        wanted: Role,
    },
    /// A grant giving more than the actor holds on its node.
    AboveActorGrant {
        kind: ActionKind,
        held: Answer,
        wanted: Answer,
    },
    /// A grant on a node outside this room.
    OutsideRoom(TreePath),
    /// A membership action on someone the actor does not outrank: a
    /// Moderator reaches only members below Moderator, and nobody bans
    /// themselves.
    NotBelowActor {
        actor: Option<Role>,
        target: Role,
    },
    /// The person is banned: unban them first.
    Banned(String),
    /// The person is restricted: they change nothing, the room's governance
    /// included, until the restriction lapses or is lifted.
    Restricted(String),
    /// The person is not banned.
    NotBanned(String),
    /// The person holds no live restriction.
    NotRestricted(String),
    /// A restriction ending at or before now restricts nothing.
    InThePast(u64),
    NotAMember(String),
    AlreadyAMember(String),
    UnknownGrant(GrantId),
    DuplicateGrant(GrantId),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPermitted(why) | Self::NeedsApproval(why) => f.write_str(why),
            Self::Owner => f.write_str("the room's owner cannot be removed or demoted"),
            Self::OwnerRole => f.write_str("ownership is transferred, not granted as a role"),
            Self::AboveActorRole { actor, wanted } => match actor {
                Some(actor) => write!(f, "a {actor} cannot hand out the {wanted} role"),
                None => write!(f, "only members can hand out the {wanted} role"),
            },
            Self::AboveActorGrant { kind, held, wanted } => write!(
                f,
                "cannot grant \"{wanted}\" for {} while holding \"{held}\"",
                kind.phrase()
            ),
            Self::OutsideRoom(node) => write!(f, "{node} is not in this room"),
            Self::NotBelowActor { actor, target } => match actor {
                Some(actor) => write!(f, "a {actor} cannot do this to a {target}"),
                None => write!(f, "only members ranked above a {target} can do this"),
            },
            Self::Banned(person) => write!(f, "{person} is banned from the room"),
            Self::Restricted(person) => write!(f, "{person} is restricted in the room"),
            Self::NotBanned(person) => write!(f, "{person} is not banned"),
            Self::NotRestricted(person) => write!(f, "{person} is not restricted"),
            Self::InThePast(at) => write!(f, "{at} ms is not in the future"),
            Self::NotAMember(person) => write!(f, "{person} is not a member"),
            Self::AlreadyAMember(person) => write!(f, "{person} is already a member"),
            Self::UnknownGrant(id) => write!(f, "no grant {id}"),
            Self::DuplicateGrant(id) => write!(f, "grant {id} already exists"),
        }
    }
}

impl std::error::Error for Refusal {}

impl RoomPolicy {
    /// A new room: its owner and every role's preset grants.
    #[must_use]
    pub fn new(room: impl Into<String>, owner: impl Into<String>) -> Self {
        let room = room.into();
        let owner = owner.into();
        let grants = roles::room_role_grants(&room, &Subject::Person(owner.clone()));
        Self {
            room,
            owner: Some(owner),
            members: BTreeMap::new(),
            joined: BTreeMap::new(),
            grants,
        }
    }

    /// A room with no owner yet and the role presets in place.
    #[must_use]
    pub fn unowned(room: impl Into<String>, presets_by: &Subject) -> Self {
        let room = room.into();
        let grants = roles::room_role_grants(&room, presets_by);
        Self {
            room,
            owner: None,
            members: BTreeMap::new(),
            joined: BTreeMap::new(),
            grants,
        }
    }

    /// Move the policy to another room id: every grant on this room's nodes
    /// now names `room`. Grants on other rooms are untouched.
    pub fn rehome(&mut self, room: impl Into<String>) {
        let room = room.into();
        if room == self.room {
            return;
        }
        for grant in &mut self.grants {
            grant.node = grant.node.with_room(&self.room, &room);
        }
        self.room = room;
    }

    #[must_use]
    pub fn node(&self) -> TreePath {
        TreePath::room(&self.room)
    }

    #[must_use]
    pub fn is_owner(&self, person: &str) -> bool {
        self.owner.as_deref() == Some(person)
    }

    /// The role a person holds: `Owner` for the owner, `None` for a non-member.
    #[must_use]
    pub fn role_of(&self, person: &str) -> Option<Role> {
        if self.is_owner(person) {
            return Some(Role::Owner);
        }
        self.members.get(person).copied()
    }

    /// When the room admitted `person`, Unix milliseconds: as recorded, or
    /// for an owner the room has no time for, `0`, as they hold it since it
    /// began. `None` for a non-member or a member admitted unrecorded.
    #[must_use]
    pub fn joined_at(&self, person: &str) -> Option<u64> {
        self.joined
            .get(person)
            .copied()
            .or_else(|| self.is_owner(person).then_some(0))
    }

    /// Whether `person` is blocked: their own grants deny every kind on the
    /// whole room. A blocked member stays on record (history keeps naming
    /// them) but holds nothing; the owner is never blocked.
    #[must_use]
    pub fn is_blocked(&self, person: &str) -> bool {
        if self.is_owner(person) {
            return false;
        }
        let subject = Subject::Person(person.to_owned());
        let room = self.node();
        self.grants.iter().any(|grant| {
            grant.subject == subject
                && grant.node == room
                && grant.answer == Answer::Deny
                && grant.kinds == crate::KindSet::ALL
        })
    }

    /// The role `person` acts with: their role, unless they are blocked.
    #[must_use]
    pub fn active_role(&self, person: &str) -> Option<Role> {
        if self.is_blocked(person) {
            None
        } else {
            self.role_of(person)
        }
    }

    /// The grant that blocks `person`, given by `granted_by`.
    #[must_use]
    pub fn block(&self, person: &str, granted_by: Subject) -> Grant {
        roles::block_grant(&self.room, person, granted_by)
    }

    /// Whether `person` is banned: the room holds their `block:<person>`
    /// grant. A banned person is not admitted again until `unban`.
    #[must_use]
    pub fn is_banned(&self, person: &str) -> bool {
        let id = roles::block_grant_id(person);
        self.grants.iter().any(|grant| grant.id == id)
    }

    /// Until when `person` is restricted, if a restriction of theirs is
    /// live in `cx`.
    #[must_use]
    pub fn restricted_until(&self, person: &str, cx: &Context<'_>) -> Option<u64> {
        let id = roles::restriction_grant_id(person);
        self.grants
            .iter()
            .find(|grant| grant.id == id && grant.is_live(cx))
            .and_then(|grant| match grant.lifetime {
                Lifetime::Until(at) => Some(at),
                _ => None,
            })
    }

    #[must_use]
    pub fn is_member(&self, person: &str) -> bool {
        self.role_of(person).is_some()
    }

    /// The subjects a person is known by here: themselves and their role.
    #[must_use]
    pub fn subjects_of(&self, person: &str) -> Vec<Subject> {
        let mut subjects = vec![Subject::Person(person.to_owned())];
        if let Some(role) = self.role_of(person) {
            subjects.push(Subject::Role(role));
        }
        subjects
    }

    /// A person's own authority in the room.
    #[must_use]
    pub fn party(&self, person: &str) -> Party<'_> {
        let party = Party::new(self.subjects_of(person), &self.grants);
        if self.is_owner(person) {
            party.owning(self.node())
        } else {
            party
        }
    }

    /// `person`'s authority from one of their devices: their own, and from a
    /// device that is not theirs (`foreign`), held to `foreign_bound` and to
    /// the room's grants for such devices too.
    #[must_use]
    pub fn person_on_device<'a>(
        &'a self,
        person: &str,
        foreign: bool,
        foreign_bound: &'a [Grant],
    ) -> Authority<'a> {
        let authority = Authority::person(self.party(person));
        if foreign {
            authority.bounded_by(
                Party::new(vec![Subject::ForeignDevice], foreign_bound)
                    .with_grants(self.grants_of(&Subject::ForeignDevice)),
            )
        } else {
            authority
        }
    }

    /// The answer task `task`'s own grant set gives for an action of `kind`
    /// at `node`, or `None` when the task carries no grant set.
    #[must_use]
    pub fn task_answer(
        &self,
        task: &str,
        node: &TreePath,
        kind: ActionKind,
        cx: &Context<'_>,
    ) -> Option<Answer> {
        let subject = Subject::Task(task.to_owned());
        self.grants
            .iter()
            .any(|grant| grant.subject == subject)
            .then(|| {
                Authority::person(self.party_of(subject))
                    .decide(node, kind, cx)
                    .answer
            })
    }

    /// The room's grants naming a delegation subject (an agent, an app, a
    /// task), for bounding a person's authority.
    #[must_use]
    pub fn party_of(&self, subject: Subject) -> Party<'_> {
        Party::new(vec![subject], &self.grants)
    }

    /// Whether application `app` is an author of the room: its grants let it
    /// change the room's content ([`roles::app_authorship`]).
    #[must_use]
    pub fn app_authors(&self, app: &str, cx: &Context<'_>) -> bool {
        Authority::person(self.party_of(Subject::App(app.to_owned())))
            .decide(&self.node(), ActionKind::Undoable, cx)
            .answer
            == Answer::Allow
    }

    /// The grants naming `subject`.
    pub fn grants_of<'a>(&'a self, subject: &'a Subject) -> impl Iterator<Item = &'a Grant> + 'a {
        self.grants
            .iter()
            .filter(move |grant| &grant.subject == subject)
    }

    /// Whether the actor may perform a governance action. `approved` says the
    /// host witnessed the person's approval when the answer is Ask.
    ///
    /// # Errors
    /// [`Refusal::NotPermitted`] or [`Refusal::NeedsApproval`].
    pub fn check_action(
        &self,
        actor: &Authority<'_>,
        action: &str,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<(), Refusal> {
        self.admit_action(actor, action, cx, approved).map(drop)
    }

    /// [`Self::check_action`], with the `Once` grants performing the action
    /// spends ([`crate::Verdict::spent`]).
    fn admit_action(
        &self,
        actor: &Authority<'_>,
        action: &str,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<Vec<GrantId>, Refusal> {
        self.check_not_held_off(actor, cx)?;
        let node = roles::governance_action(&self.room, action);
        let verdict = actor.decide(&node, roles::GOVERNANCE_KIND, cx);
        let verdict = if roles::governance::is_privileged(action) {
            verdict.privileged()
        } else {
            verdict
        };
        match verdict.answer {
            Answer::Allow => Ok(verdict.spent()),
            Answer::Ask if approved => Ok(verdict.spent()),
            Answer::Ask => Err(Refusal::NeedsApproval(verdict.explain())),
            Answer::Deny => Err(Refusal::NotPermitted(verdict.explain())),
        }
    }

    /// Refuses governance to a person the room bans or restricts, whatever
    /// else they hold. Their Deny sits on the whole room, so a grant on a
    /// more specific node (an administrator's, a Moderator's actions) would
    /// otherwise decide a governance action past it: a ban or a restriction
    /// stops every change, governing others included. The owner is never
    /// held off.
    fn check_not_held_off(&self, actor: &Authority<'_>, cx: &Context<'_>) -> Result<(), Refusal> {
        for subject in &actor.person.subjects {
            let Subject::Person(person) = subject else {
                continue;
            };
            if self.is_owner(person) {
                continue;
            }
            if self.is_blocked(person) {
                return Err(Refusal::Banned(person.clone()));
            }
            if self.restricted_until(person, cx).is_some() {
                return Err(Refusal::Restricted(person.clone()));
            }
        }
        Ok(())
    }

    /// Drop the grants that lapsed with their task
    /// ([`Grant::lapsed_with_tasks`]). The ids dropped.
    pub fn lapse_tasks(&mut self, open: impl Fn(&str) -> bool) -> Vec<GrantId> {
        let mut lapsed = Vec::new();
        self.grants.retain(|grant| {
            let keep = !grant.lapsed_with_tasks(&open);
            if !keep {
                lapsed.push(grant.id.clone());
            }
            keep
        });
        lapsed
    }

    /// Drop the `Once` grants among `spent`: an action ran on them. Any other
    /// grant named stays. Returns whether anything was dropped.
    pub fn spend(&mut self, spent: &[GrantId]) -> bool {
        let before = self.grants.len();
        self.grants
            .retain(|grant| grant.lifetime != Lifetime::Once || !spent.contains(&grant.id));
        self.grants.len() != before
    }

    /// Whether `actor_person` (acting through `actor`) may invite someone with
    /// `role`. An invite never carries more than the inviter's role.
    ///
    /// # Errors
    /// When the invite action is not granted or `role` exceeds the inviter's.
    pub fn check_invite(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        role: Role,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<(), Refusal> {
        self.check_action(actor, governance::INVITE, cx, approved)?;
        self.check_role_handout(actor_person, role)
    }

    fn check_role_handout(&self, actor_person: &str, role: Role) -> Result<(), Refusal> {
        if role == Role::Owner {
            return Err(Refusal::OwnerRole);
        }
        let held = self.role_of(actor_person);
        if !held.is_some_and(|held| held.includes(role)) {
            return Err(Refusal::AboveActorRole {
                actor: held,
                wanted: role,
            });
        }
        Ok(())
    }

    /// Whether `actor_person` (acting through `actor`) may make `change`,
    /// without making it: what a door asks before it commits the change by
    /// its own means.
    ///
    /// # Errors
    /// As [`Self::apply`].
    pub fn check(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        change: &Change,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<(), Refusal> {
        self.admit_action(actor, change.action(), cx, approved)?;
        self.check_change(actor_person, actor, change, cx)
    }

    /// Applies a governance change made by `actor_person` through `actor`.
    ///
    /// # Errors
    /// When the action is not granted, or the change breaks a structural rule:
    /// the owner stays, roles and grants never exceed the actor's own.
    pub fn apply(
        &mut self,
        actor_person: &str,
        actor: &Authority<'_>,
        change: Change,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<(), Refusal> {
        let spent = self.admit_action(actor, change.action(), cx, approved)?;
        self.check_change(actor_person, actor, &change, cx)?;
        self.spend(&spent);
        match change {
            Change::AddMember { person, role } => {
                self.joined.entry(person.clone()).or_insert(cx.now_unix_ms);
                self.members.insert(person, role);
            }
            Change::SetRole { person, role } => {
                self.members.insert(person, role);
            }
            Change::RemoveMember { person } => self.drop_member(&person),
            Change::Ban { person } => {
                self.drop_member(&person);
                let block = self.block(&person, Subject::Person(actor_person.to_owned()));
                self.grants.push(block);
            }
            Change::Unban { person } => {
                let id = roles::block_grant_id(&person);
                self.grants.retain(|grant| grant.id != id);
            }
            Change::Restrict { person, until_ms } => {
                let id = roles::restriction_grant_id(&person);
                self.grants.retain(|grant| grant.id != id);
                self.grants.push(roles::restriction_grant(
                    &self.room,
                    &person,
                    until_ms,
                    Subject::Person(actor_person.to_owned()),
                ));
            }
            Change::Unrestrict { person } => {
                let id = roles::restriction_grant_id(&person);
                self.grants.retain(|grant| grant.id != id);
            }
            Change::Grant(mut grant) => {
                grant.granted_by = Subject::Person(actor_person.to_owned());
                self.grants.push(grant);
            }
            Change::Revoke(id) => self.grants.retain(|grant| grant.id != id),
        }
        Ok(())
    }

    /// The member and every grant naming them leave the room.
    fn drop_member(&mut self, person: &str) {
        self.members.remove(person);
        self.joined.remove(person);
        let subject = Subject::Person(person.to_owned());
        self.grants.retain(|grant| grant.subject != subject);
    }

    /// Whether `actor_person` reaches `person` with a membership action: the
    /// owner reaches everyone else, anyone else only those they outrank (a
    /// non-member target ranks lowest). Leaving, removing oneself, is always
    /// in reach.
    fn check_reach(&self, actor_person: &str, person: &str, ban: bool) -> Result<(), Refusal> {
        if self.is_owner(person) {
            return Err(Refusal::Owner);
        }
        let actor = self.role_of(actor_person);
        if actor_person == person {
            return if ban {
                Err(Refusal::NotBelowActor {
                    actor,
                    target: actor.unwrap_or(Role::Viewer),
                })
            } else {
                Ok(())
            };
        }
        if self.is_owner(actor_person) {
            return Ok(());
        }
        match self.role_of(person) {
            Some(target) if !actor.is_some_and(|actor| actor.outranks(target)) => {
                Err(Refusal::NotBelowActor { actor, target })
            }
            _ => Ok(()),
        }
    }

    /// Whether `grant` gives no more than `actor` holds on its node, for each
    /// kind it covers. A Deny never exceeds.
    ///
    /// # Errors
    /// [`Refusal::AboveActorGrant`] for the first kind it exceeds.
    pub fn check_within(
        actor: &Authority<'_>,
        grant: &Grant,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if grant.answer == Answer::Deny {
            return Ok(());
        }
        for kind in grant.kinds.iter() {
            let held = actor.decide(&grant.node, kind, cx).answer;
            if held < grant.answer {
                return Err(Refusal::AboveActorGrant {
                    kind,
                    held,
                    wanted: grant.answer,
                });
            }
        }
        Ok(())
    }

    fn check_change(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        change: &Change,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        match change {
            Change::AddMember { person, role } => {
                if self.is_member(person) {
                    return Err(Refusal::AlreadyAMember(person.clone()));
                }
                if self.is_banned(person) {
                    return Err(Refusal::Banned(person.clone()));
                }
                self.check_role_handout(actor_person, *role)
            }
            Change::SetRole { person, role } => {
                if self.is_owner(person) {
                    return Err(Refusal::Owner);
                }
                if !self.members.contains_key(person) {
                    return Err(Refusal::NotAMember(person.clone()));
                }
                self.check_role_handout(actor_person, *role)
            }
            Change::RemoveMember { person } => {
                if self.is_owner(person) {
                    return Err(Refusal::Owner);
                }
                if !self.members.contains_key(person) {
                    return Err(Refusal::NotAMember(person.clone()));
                }
                self.check_reach(actor_person, person, false)
            }
            Change::Ban { person } => {
                if self.is_banned(person) {
                    return Err(Refusal::Banned(person.clone()));
                }
                self.check_reach(actor_person, person, true)
            }
            Change::Unban { person } => {
                if !self.is_banned(person) {
                    return Err(Refusal::NotBanned(person.clone()));
                }
                Ok(())
            }
            Change::Restrict { person, until_ms } => {
                if *until_ms <= cx.now_unix_ms {
                    return Err(Refusal::InThePast(*until_ms));
                }
                self.check_reach(actor_person, person, true)
            }
            Change::Unrestrict { person } => {
                if self.restricted_until(person, cx).is_none() {
                    return Err(Refusal::NotRestricted(person.clone()));
                }
                self.check_reach(actor_person, person, true)
            }
            Change::Grant(grant) => {
                if grant.node.room_id() != Some(self.room.as_str()) {
                    return Err(Refusal::OutsideRoom(grant.node.clone()));
                }
                if self.grants.iter().any(|held| held.id == grant.id) {
                    return Err(Refusal::DuplicateGrant(grant.id.clone()));
                }
                Self::check_within(actor, grant, cx)
            }
            Change::Revoke(id) => {
                if self.grants.iter().any(|grant| &grant.id == id) {
                    Ok(())
                } else {
                    Err(Refusal::UnknownGrant(id.clone()))
                }
            }
        }
    }
}
