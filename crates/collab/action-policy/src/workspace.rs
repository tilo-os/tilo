//! A workspace's authority: its owner, its members' roles and its grants,
//! changed only through the workspace's governance actions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ActionKind, Answer, Authority, Context, Grant, GrantId, Grantee, Lifetime, Party, Role,
    TreePath, Verdict,
    roles::{self, governance},
};

/// Everything that decides authority in one workspace.
///
/// The built-in roles' grants are the code's ([`roles::role_rules`]): a
/// stored policy loads with them as defined now
/// ([`Self::with_current_roles`]), so a workspace written before a role
/// changed gets the role as it is. Grants given in the workspace stay as
/// stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredWorkspacePolicy")]
pub struct WorkspacePolicy {
    /// The workspace this policy governs. Written under `room`, the name a
    /// stored policy names it by: a policy travels to devices and hosts on
    /// other builds, and their reader knows no other. The key it is read
    /// under is [`StoredWorkspacePolicy`]'s.
    #[serde(rename = "room")]
    pub workspace: String,
    /// The owner, by person id. Structural: holds everything and cannot be
    /// removed, demoted or denied by a grant.
    pub owner: Option<String>,
    /// Members other than the owner and the role preset each holds.
    pub members: BTreeMap<String, Role>,
    /// When the workspace admitted each member, Unix milliseconds, by person
    /// id. Recorded once, by whoever admitted them, and kept through role
    /// changes; a member admitted before workspaces recorded it has none.
    /// People who share a name are numbered in this order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub joined: BTreeMap<String, u64>,
    /// Every grant in the workspace, role presets included.
    pub grants: Vec<Grant>,
}

/// A [`WorkspacePolicy`] as written, before its built-in roles are brought to
/// their current definition.
#[derive(Deserialize)]
struct StoredWorkspacePolicy {
    /// Policies name their workspace under `room`, the key they were first
    /// written under; one written while the field was named for what it means
    /// carries `workspace`, and reads as the same policy.
    #[serde(rename = "room", alias = "workspace")]
    workspace: String,
    owner: Option<String>,
    members: BTreeMap<String, Role>,
    #[serde(default)]
    joined: BTreeMap<String, u64>,
    grants: Vec<Grant>,
}

impl From<StoredWorkspacePolicy> for WorkspacePolicy {
    fn from(stored: StoredWorkspacePolicy) -> Self {
        Self {
            workspace: stored.workspace,
            owner: stored.owner,
            members: stored.members,
            joined: stored.joined,
            grants: stored.grants,
        }
        .with_current_roles()
    }
}

/// A change to a workspace's authority. Each is one governance action.
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

/// Whether `change` is `actor_person` removing themselves: leaving, which the
/// workspace's grants never gate.
fn is_leaving(actor_person: &str, change: &Change) -> bool {
    matches!(change, Change::RemoveMember { person } if person == actor_person)
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
    /// Ownership is structural: it is set when the workspace is made, and
    /// membership neither hands it out nor takes it away.
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
    /// A grant on a node outside this workspace.
    OutsideWorkspace(TreePath),
    /// A membership action on someone the actor does not outrank: a
    /// Moderator reaches only members below Moderator, and nobody bans
    /// themselves.
    NotBelowActor {
        actor: Option<Role>,
        target: Role,
    },
    /// The person is banned: unban them first.
    Banned(String),
    /// The person is restricted: they change nothing, the workspace's
    /// governance included, until the restriction lapses or is lifted.
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
    /// A built-in role's grant: the role's rules are the code's, so a
    /// workspace neither revokes nor gives one; it adjusts a role with grants
    /// of its own.
    BuiltInRole(GrantId),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPermitted(why) | Self::NeedsApproval(why) => f.write_str(why),
            Self::Owner => f.write_str("the workspace's owner cannot be removed or demoted"),
            Self::OwnerRole => {
                f.write_str("ownership is set when the workspace is made, not granted as a role")
            }
            Self::AboveActorRole { actor, wanted } => match actor {
                Some(actor) => write!(f, "a {actor} cannot hand out the {wanted} role"),
                None => write!(f, "only members can hand out the {wanted} role"),
            },
            Self::AboveActorGrant { kind, held, wanted } => write!(
                f,
                "cannot grant \"{wanted}\" for {} while holding \"{held}\"",
                kind.phrase()
            ),
            Self::OutsideWorkspace(node) => {
                write!(f, "{} is not in this workspace", node.readable())
            }
            Self::NotBelowActor { actor, target } => match actor {
                Some(actor) => write!(f, "a {actor} cannot do this to a {target}"),
                None => write!(f, "only members ranked above a {target} can do this"),
            },
            Self::Banned(person) => write!(f, "{person} is banned from the workspace"),
            Self::Restricted(person) => write!(f, "{person} is restricted in the workspace"),
            Self::NotBanned(person) => write!(f, "{person} is not banned"),
            Self::NotRestricted(person) => write!(f, "{person} is not restricted"),
            Self::InThePast(at) => write!(f, "{at} ms is not in the future"),
            Self::NotAMember(person) => write!(f, "{person} is not a member"),
            Self::AlreadyAMember(person) => write!(f, "{person} is already a member"),
            Self::UnknownGrant(id) => write!(f, "no grant {id}"),
            Self::DuplicateGrant(id) => write!(f, "grant {id} already exists"),
            Self::BuiltInRole(id) => write!(f, "{id} is part of a built-in role"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Whether two grants say the same thing: the same id and terms, whoever the
/// workspace records as having given it.
fn same_terms(held: &Grant, wanted: &Grant) -> bool {
    held.id == wanted.id
        && held.grantee == wanted.grantee
        && held.node == wanted.node
        && held.kinds == wanted.kinds
        && held.answer == wanted.answer
        && held.lifetime == wanted.lifetime
}

impl WorkspacePolicy {
    /// A new workspace: its owner and every role's preset grants.
    #[must_use]
    pub fn new(workspace: impl Into<String>, owner: impl Into<String>) -> Self {
        let workspace = workspace.into();
        let owner = owner.into();
        let grants = roles::workspace_role_grants(&workspace, &Grantee::person(owner.clone()));
        Self {
            workspace,
            owner: Some(owner),
            members: BTreeMap::new(),
            joined: BTreeMap::new(),
            grants,
        }
    }

    /// Whether anyone besides `person` belongs to the workspace, its owner or
    /// a member: what the workspace holds is shared with them whether or not
    /// they are here right now.
    #[must_use]
    pub fn has_others(&self, person: &str) -> bool {
        self.owner.as_deref().is_some_and(|owner| owner != person)
            || self.members.keys().any(|member| member != person)
    }

    /// A workspace with no owner yet and the role presets in place.
    #[must_use]
    pub fn unowned(workspace: impl Into<String>, presets_by: &Grantee) -> Self {
        let workspace = workspace.into();
        let grants = roles::workspace_role_grants(&workspace, presets_by);
        Self {
            workspace,
            owner: None,
            members: BTreeMap::new(),
            joined: BTreeMap::new(),
            grants,
        }
    }

    /// The policy with each built-in role's grants as the code defines them
    /// now ([`roles::current_role_grants`]). Whatever reads a stored policy
    /// calls it, so stored copies of a role never decide.
    #[must_use]
    pub fn with_current_roles(mut self) -> Self {
        self.grants = roles::current_role_grants(std::mem::take(&mut self.grants));
        self
    }

    /// Move the policy to another workspace id: every grant on this
    /// workspace's nodes now names `workspace`. Grants on other workspaces are
    /// untouched.
    pub fn rehome(&mut self, workspace: impl Into<String>) {
        let workspace = workspace.into();
        if workspace == self.workspace {
            return;
        }
        for grant in &mut self.grants {
            grant.node = grant.node.with_workspace(&self.workspace, &workspace);
        }
        self.workspace = workspace;
    }

    #[must_use]
    pub fn node(&self) -> TreePath {
        TreePath::workspace(&self.workspace)
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

    /// When the workspace admitted `person`, Unix milliseconds: as recorded,
    /// or for an owner the workspace has no time for, `0`, as they hold it
    /// since it began. `None` for a non-member or a member admitted
    /// unrecorded.
    #[must_use]
    pub fn joined_at(&self, person: &str) -> Option<u64> {
        self.joined
            .get(person)
            .copied()
            .or_else(|| self.is_owner(person).then_some(0))
    }

    /// Whether `person` is blocked: their own grants deny every kind on the
    /// whole workspace. A blocked member stays on record (history keeps naming
    /// them) but holds nothing; the owner is never blocked.
    #[must_use]
    pub fn is_blocked(&self, person: &str) -> bool {
        if self.is_owner(person) {
            return false;
        }
        let grantee = Grantee::person(person.to_owned());
        let workspace = self.node();
        self.grants.iter().any(|grant| {
            grant.grantee == grantee
                && grant.node == workspace
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

    #[must_use]
    pub fn block(&self, person: &str, granted_by: Grantee) -> Grant {
        roles::block_grant(&self.workspace, person, granted_by)
    }

    /// Whether `person` is banned: the workspace holds their
    /// `block:<person>` grant. A banned person is not admitted again until
    /// `unban`.
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

    /// The grantees a person is known by here: themselves and their role. A
    /// grant to a lower role names them too ([`crate::decide::names`]).
    #[must_use]
    pub fn grantees_of(&self, person: &str) -> Vec<Grantee> {
        let mut grantees = vec![Grantee::person(person.to_owned())];
        if let Some(role) = self.role_of(person) {
            grantees.push(Grantee::role(role));
        }
        grantees
    }

    #[must_use]
    pub fn party(&self, person: &str) -> Party<'_> {
        // The workspace's ban or restriction of the person is a decision
        // about them: no grant to their role lifts it, however specific (an
        // item shared with the workspace to edit included), as nobody but
        // whoever may govern `unban` or `unrestrict` may.
        let moderation = [
            roles::block_grant_id(person),
            roles::restriction_grant_id(person),
        ];
        let grantee = Grantee::person(person.to_owned());
        let held_off = self
            .grants
            .iter()
            .filter(|grant| grant.grantee == grantee && moderation.contains(&grant.id));
        let party = Party::new(self.grantees_of(person), &self.grants).held_off_by(held_off);
        if self.is_owner(person) {
            party.owning(self.node())
        } else {
            party
        }
    }

    /// `person`'s authority from one of their devices: their own, and from a
    /// device that is not theirs (`foreign`), held to what such a device may
    /// do at most.
    #[must_use]
    pub fn person_on_device(&self, person: &str, foreign: bool) -> Authority<'_> {
        let party = self.party(person);
        if foreign {
            Authority::person(party.capped(crate::Cap::FOREIGN_DEVICE))
        } else {
            Authority::person(party)
        }
    }

    /// The workspace's grants naming a delegation grantee (an agent, an
    /// issue, a Shortcut), for bounding a person's authority.
    #[must_use]
    pub fn party_of(&self, grantee: Grantee) -> Party<'_> {
        Party::new(vec![grantee], &self.grants)
    }

    /// The grants naming `grantee`.
    pub fn grants_of<'a>(&'a self, grantee: &'a Grantee) -> impl Iterator<Item = &'a Grant> + 'a {
        self.grants
            .iter()
            .filter(move |grant| &grant.grantee == grantee)
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
        let node = roles::governance_action(&self.workspace, action);
        let verdict = actor.decide(&node, roles::GOVERNANCE_KIND, cx);
        let verdict = if roles::governance::is_privileged(action) {
            verdict.privileged()
        } else {
            verdict
        };
        Self::resolved(&verdict, approved)
    }

    /// [`Self::admit_action`] for placing `grant`: a grant assigns one of the
    /// workspace's saved sets ([`roles::preset_assigned`]) when a grant on that
    /// set's assignment node allows it, and any other grant when the
    /// workspace's own grant action does.
    fn admit_grant(
        &self,
        actor: &Authority<'_>,
        grant: &Grant,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<Vec<GrantId>, Refusal> {
        let Some((set, _)) = roles::preset_assigned(&grant.id) else {
            return self.admit_action(actor, governance::GRANT, cx, approved);
        };
        self.check_not_held_off(actor, cx)?;
        let verdict = roles::assign_preset_verdict(actor, &self.workspace, set, cx);
        Self::resolved(&verdict, approved)
    }

    fn resolved(verdict: &Verdict, approved: bool) -> Result<Vec<GrantId>, Refusal> {
        match verdict.answer {
            Answer::Allow => Ok(verdict.spent()),
            Answer::Ask if approved => Ok(verdict.spent()),
            Answer::Ask => Err(Refusal::NeedsApproval(verdict.explain())),
            Answer::Deny => Err(Refusal::NotPermitted(verdict.explain())),
        }
    }

    /// Refuses governance to a person the workspace bans or restricts,
    /// whatever else they hold. Their Deny sits on the whole workspace, so a
    /// grant on a
    /// more specific node (an administrator's, a Moderator's actions) would
    /// otherwise decide a governance action past it: a ban or a restriction
    /// stops every change, governing others included. The owner is never
    /// held off.
    fn check_not_held_off(&self, actor: &Authority<'_>, cx: &Context<'_>) -> Result<(), Refusal> {
        for grantee in &actor.person.grantees {
            let Grantee::Member(crate::MemberId::Person(person)) = grantee else {
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
        if !is_leaving(actor_person, change) {
            match change {
                Change::Grant(grant) => {
                    self.admit_grant(actor, grant, cx, approved)?;
                }
                change => {
                    self.admit_action(actor, change.action(), cx, approved)?;
                }
            }
        }
        self.check_change(actor_person, actor, change, cx)
    }

    /// Applies a governance change made by `actor_person` through `actor`.
    ///
    /// Leaving needs no grant: whoever removes themselves from a workspace does
    /// it by being who they are, and a ban or a restriction stops it no more
    /// than the `remove` action would (`check_reach`).
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
        let spent = if is_leaving(actor_person, &change) {
            Vec::new()
        } else {
            match &change {
                Change::Grant(grant) => self.admit_grant(actor, grant, cx, approved)?,
                change => self.admit_action(actor, change.action(), cx, approved)?,
            }
        };
        self.check_change(actor_person, actor, &change, cx)?;
        if let Change::Grant(grant) = &change
            && self.grants.iter().any(|held| same_terms(held, grant))
        {
            return Ok(());
        }
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
                let block = self.block(&person, Grantee::person(actor_person.to_owned()));
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
                    &self.workspace,
                    &person,
                    until_ms,
                    Grantee::person(actor_person.to_owned()),
                ));
            }
            Change::Unrestrict { person } => {
                let id = roles::restriction_grant_id(&person);
                self.grants.retain(|grant| grant.id != id);
            }
            Change::Grant(grant) => {
                // The change names its grantor, and every device keeps that
                // name: one who may grant records a grant for the member who
                // asked through them (a page asks its workspace's host to
                // share), and the share stays whose it is. Nothing here reads
                // this device's own state, so every device reads the same
                // change the same way.
                self.grants.push(grant);
            }
            Change::Revoke(id) => self.grants.retain(|grant| grant.id != id),
        }
        Ok(())
    }

    /// The member and every grant naming them leave the workspace.
    fn drop_member(&mut self, person: &str) {
        self.members.remove(person);
        self.joined.remove(person);
        let grantee = Grantee::person(person.to_owned());
        self.grants.retain(|grant| grant.grantee != grantee);
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
                // A member's removal takes their membership and every grant
                // naming them. Someone the membership no longer holds, because
                // the admission an invitation receipt gave them was taken back
                // first ([`super::super::members`] replay), has only those
                // grants left to take: removing them is the same change, and
                // removing them twice changes nothing.
                if self.members.contains_key(person) {
                    self.check_reach(actor_person, person, false)?;
                }
                Ok(())
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
                if roles::is_role_grant(grant) {
                    return Err(Refusal::BuiltInRole(grant.id.clone()));
                }
                if grant.node.workspace_id() != Some(self.workspace.as_str()) {
                    return Err(Refusal::OutsideWorkspace(grant.node.clone()));
                }
                if let Some(held) = self.grants.iter().find(|held| held.id == grant.id) {
                    // The same grant again converges: a replayed command
                    // leaves the workspace as it was instead of refusing.
                    if same_terms(held, grant) {
                        return Ok(());
                    }
                    return Err(Refusal::DuplicateGrant(grant.id.clone()));
                }
                Self::check_within(actor, grant, cx)
            }
            Change::Revoke(id) => {
                if let Some(grant) = self.grants.iter().find(|grant| &grant.id == id) {
                    if roles::is_role_grant(grant) {
                        return Err(Refusal::BuiltInRole(id.clone()));
                    }
                    Ok(())
                } else {
                    Err(Refusal::UnknownGrant(id.clone()))
                }
            }
        }
    }
}
