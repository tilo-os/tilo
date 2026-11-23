//! A workspace's authority: its owner, its members' roles and its grants,
//! changed only through the workspace's governance actions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ActionKind, Answer, Authority, Context, Grant, GrantId, Grantee, Lifetime, Party, PersonId,
    Role, TreePath, Verdict,
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
    /// The workspace this policy governs.
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
    /// Workspace-defined roles by their stable id, with their display name.
    #[serde(default)]
    pub custom_roles: BTreeMap<String, String>,
    /// The custom roles assigned to each member, in addition to their preset.
    #[serde(default)]
    pub assigned_roles: BTreeMap<String, BTreeSet<String>>,
    /// The agents the workspace holds as members, by agent id: each acts for
    /// one person, and never reaches past what that person may
    /// ([`Change::AddAgent`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, AgentMember>,
}

/// An agent's membership: whom it acts for. What it may do is the narrower of
/// its own grants and that person's. The entry is the agent's one place in the
/// workspace: what else its membership comes to hold, such as the key records
/// are sealed to it with, is kept here beside its person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMember {
    /// The person the agent acts for, by person id.
    pub for_person: String,
}

/// A [`WorkspacePolicy`] as written, before its built-in roles are brought to
/// their current definition.
#[derive(Deserialize)]
struct StoredWorkspacePolicy {
    workspace: String,
    owner: Option<String>,
    members: BTreeMap<String, Role>,
    #[serde(default)]
    joined: BTreeMap<String, u64>,
    grants: Vec<Grant>,
    #[serde(default)]
    custom_roles: BTreeMap<String, String>,
    #[serde(default)]
    assigned_roles: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    agents: BTreeMap<String, AgentMember>,
}

impl From<StoredWorkspacePolicy> for WorkspacePolicy {
    fn from(stored: StoredWorkspacePolicy) -> Self {
        Self {
            workspace: stored.workspace,
            owner: stored.owner,
            members: stored.members,
            joined: stored.joined,
            grants: stored.grants,
            custom_roles: stored.custom_roles,
            assigned_roles: stored.assigned_roles,
            agents: stored.agents,
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
    /// Create a custom role, or rename the role with this id.
    DefineRole {
        id: String,
        name: String,
    },
    /// Remove the role's rules and assignments together.
    RemoveRole {
        id: String,
    },
    /// The whole-set role assignment the builds since custom roles numbered
    /// here: kept in its place so every later change reads as the change those
    /// builds wrote, an ownership transfer above all. No build wrote one, and
    /// a workspace refuses it.
    #[serde(rename = "set_member_roles")]
    RetiredSetMemberRoles {
        person: String,
        roles: BTreeSet<String>,
    },
    /// Assign or remove one custom role without replacing other assignments.
    SetMemberCustomRole {
        person: String,
        role: String,
        assigned: bool,
    },
    /// Make `person` the workspace's owner, in the place its owner holds now.
    /// The owner alone makes it. Whoever owned the workspace stays in it as an
    /// Editor and one of its administrators, which the new owner may undo
    /// like any administrator's grant.
    TransferOwnership {
        person: String,
    },
    /// Make `agent` a member acting for `for_person`. A member adds an agent
    /// acting for themselves; one acting for someone else is an
    /// administrator's to add. `by` is the member the signing device writes
    /// for, when that is not its own person: the person, or an agent of the
    /// member list acting for them, whose right decides, narrowed by the
    /// signer's.
    AddAgent {
        agent: String,
        for_person: String,
        #[serde(default)]
        by: Option<crate::MemberId>,
    },
    /// The agent leaves the workspace: its person, or an administrator,
    /// removes it. `by` as [`Self::AddAgent`]'s.
    RemoveAgent {
        agent: String,
        #[serde(default)]
        by: Option<crate::MemberId>,
    },
}

impl Change {
    /// Whether this change manages a custom role or its assignment.
    #[must_use]
    pub const fn is_custom_role_change(&self) -> bool {
        matches!(
            self,
            Self::DefineRole { .. } | Self::RemoveRole { .. } | Self::SetMemberCustomRole { .. }
        )
    }

    /// The governance action this change is.
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::AddMember { .. } | Self::AddAgent { .. } => governance::ADD_MEMBER,
            Self::RemoveMember { .. } | Self::RemoveAgent { .. } => governance::REMOVE_MEMBER,
            Self::Ban { .. } => governance::BAN,
            Self::Unban { .. } => governance::UNBAN,
            Self::Restrict { .. } => governance::RESTRICT,
            Self::Unrestrict { .. } => governance::UNRESTRICT,
            Self::SetRole { .. } => governance::SET_ROLE,
            Self::TransferOwnership { .. } => governance::TRANSFER_OWNERSHIP,
            Self::Grant(_)
            | Self::DefineRole { .. }
            | Self::RetiredSetMemberRoles { .. }
            | Self::SetMemberCustomRole { assigned: true, .. } => governance::GRANT,
            Self::Revoke(_)
            | Self::RemoveRole { .. }
            | Self::SetMemberCustomRole {
                assigned: false, ..
            } => governance::REVOKE,
        }
    }

    /// The one change that makes `agent` a member acting for `person`: every
    /// place that makes an agent writes it, and it is the agent's whole
    /// membership. `by` names the member the signing device writes it for,
    /// when that is not the device's own person ([`Self::AddAgent`]).
    #[must_use]
    pub fn add_agent(
        agent: impl Into<String>,
        person: impl Into<String>,
        by: Option<crate::MemberId>,
    ) -> Self {
        Self::AddAgent {
            agent: agent.into(),
            for_person: person.into(),
            by,
        }
    }

    /// The person a membership change names.
    #[must_use]
    pub fn person(&self) -> Option<&str> {
        match self {
            Self::AddMember { person, .. }
            | Self::RemoveMember { person }
            | Self::Ban { person }
            | Self::Unban { person }
            | Self::Restrict { person, .. }
            | Self::Unrestrict { person }
            | Self::SetRole { person, .. }
            | Self::RetiredSetMemberRoles { person, .. }
            | Self::SetMemberCustomRole { person, .. }
            | Self::TransferOwnership { person }
            | Self::AddAgent {
                for_person: person, ..
            } => Some(person),
            Self::Grant(_)
            | Self::Revoke(_)
            | Self::DefineRole { .. }
            | Self::RemoveRole { .. }
            | Self::RemoveAgent { .. } => None,
        }
    }
}

/// Whether `change` is `actor_person` removing themselves: leaving, which the
/// workspace's grants never check.
fn is_leaving(actor_person: &str, change: &Change) -> bool {
    matches!(change, Change::RemoveMember { person } if person == actor_person)
}

/// Why a governance action was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The actor's grants do not allow the action; carries the explanation.
    NotPermitted(String),
    /// The action asks first and no approval was approved.
    NeedsApproval(String),
    /// The owner cannot be removed, demoted or made a member.
    Owner,
    /// Ownership is structural: no role hands it out or takes it away; its
    /// owner hands it over ([`Change::TransferOwnership`]).
    OwnerRole,
    /// Handing the workspace over is its owner's alone.
    OwnershipByOwner,
    /// An invite or membership whose role grants more than the actor holds.
    AboveActorRole(Role),
    /// A grant giving more than the actor holds on its node.
    AboveActorGrant {
        kind: ActionKind,
        held: Answer,
        wanted: Answer,
    },
    /// A grant on a node outside this workspace.
    OutsideWorkspace(TreePath),
    /// Banning or restricting oneself: leaving is how someone stops being
    /// in a workspace.
    Themselves,
    /// Making or unmaking an administrator, or inviting one, by someone who
    /// may not: the owner and its administrators make administrators; only
    /// the owner unmakes or removes them.
    Administrators {
        actor: String,
        unmaking: bool,
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
    /// A grant to any key: what visitors may do follows the workspace's entry
    /// ([`crate::EntryPolicy::visitor_grants`]), and nobody writes one.
    EntryGrant(GrantId),
    /// An id that is no person's root key: a change naming software, or a
    /// spelling no device of ours writes, as a person.
    NotAPerson(String),
    InvalidRole,
    UnknownRole(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRole => f.write_str("invalid custom role id or name"),
            Self::UnknownRole(id) => write!(f, "unknown custom role: {id}"),
            Self::NotPermitted(why) | Self::NeedsApproval(why) => f.write_str(why),
            Self::Owner => f.write_str("the workspace's owner cannot be removed or demoted"),
            Self::OwnerRole => f.write_str(
                "ownership is handed over by the workspace's owner, not granted as a role",
            ),
            Self::OwnershipByOwner => {
                f.write_str("only the workspace's owner hands the workspace to someone else")
            }
            Self::AboveActorRole(wanted) => {
                write!(f, "the {wanted} role grants more than the giver holds")
            }
            Self::AboveActorGrant { kind, held, wanted } => write!(
                f,
                "cannot grant \"{wanted}\" for {} while holding \"{held}\"",
                kind.phrase()
            ),
            Self::OutsideWorkspace(node) => {
                write!(f, "{node} is not in this workspace")
            }
            Self::Themselves => f.write_str("nobody bans or restricts themselves"),
            Self::Administrators { actor, unmaking } => {
                if *unmaking {
                    write!(
                        f,
                        "{actor} may not unmake or remove an administrator: only the workspace's owner may"
                    )
                } else {
                    write!(
                        f,
                        "{actor} may not make administrators: only the workspace's owner and its administrators may"
                    )
                }
            }
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
            Self::EntryGrant(id) => write!(
                f,
                "{id} names any visitor: what visitors may do follows the workspace's entry"
            ),
            Self::NotAPerson(id) => write!(f, "{id} is not a person's id"),
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
            custom_roles: BTreeMap::new(),
            assigned_roles: BTreeMap::new(),
            agents: BTreeMap::new(),
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
            custom_roles: BTreeMap::new(),
            assigned_roles: BTreeMap::new(),
            agents: BTreeMap::new(),
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

    /// The owner, by person id: the root the workspace was made for.
    #[must_use]
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
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

    /// Everyone the workspace holds who it recorded a time for, by person id
    /// ([`Self::joined_at`]): the order people who share a name are numbered
    /// in. A member admitted unrecorded is not yielded, and is numbered last.
    pub fn admitted(&self) -> impl Iterator<Item = (&str, u64)> + '_ {
        self.members
            .keys()
            .map(String::as_str)
            .chain(self.owner.as_deref())
            .filter_map(|person| Some((person, self.joined_at(person)?)))
    }

    /// Whether `person` is blocked: their own grants deny every kind on the
    /// whole workspace. A blocked member stays listed (history keeps naming
    /// them) but holds nothing; the owner is never blocked.
    #[must_use]
    pub fn is_blocked(&self, person: &str) -> bool {
        if self.is_owner(person) {
            return false;
        }
        let workspace = self.node();
        self.grants.iter().any(|grant| {
            grant.grantee.person_id() == Some(person)
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

    /// Whether `person` is one of the workspace's administrators: they hold
    /// the administrator grant ([`roles::administrator_grant`]). The owner
    /// governs by being the owner, not by this grant.
    #[must_use]
    pub fn is_administrator(&self, person: &str) -> bool {
        let id = roles::administrator_grant_id(person);
        self.grants.iter().any(|grant| {
            grant.id == id
                && grant.grantee.person_id() == Some(person)
                && grant.answer == Answer::Allow
        })
    }

    /// How many permissions the workspace gives `person` beyond their role. Being an
    /// administrator is their role's to say, and is not counted.
    #[must_use]
    pub fn extra_grants(&self, person: &str) -> usize {
        self.grants_by_person().extra(person)
    }

    /// What the grants say of each person, read in one walk of them: whom they
    /// make an administrator, and how many permissions each person holds beyond
    /// their role ([`Self::extra_grants`]). Asking that of every member walks
    /// the grants once rather than once a member.
    #[must_use]
    pub fn grants_by_person(&self) -> PersonGrants<'_> {
        let mut grants = PersonGrants::default();
        for grant in &self.grants {
            if let Some(person) = roles::administrator_grant_person(&grant.id) {
                grants.administrators.insert(person);
            }
            if let Some(person) = grant.grantee.person_id()
                && roles::administrator_named(&grant.id).is_none()
            {
                let extra = grants.extra.entry(person).or_default();
                *extra = extra.saturating_add(1);
            }
        }
        grants
    }

    /// The workspace's administrators, by person id, in id order.
    pub fn administrators(&self) -> impl Iterator<Item = &str> + '_ {
        self.members
            .keys()
            .map(String::as_str)
            .filter(|person| self.is_administrator(person))
    }

    /// Whether `actor` may make administrators now: its grants allow
    /// [`roles::administrators_node`] on their own (the owner, an
    /// administrator), and the workspace
    /// neither bans nor restricts its person. A foreign device or an agent
    /// holds less, as for every governance action.
    #[must_use]
    pub fn may_make_administrators(&self, actor: &Authority<'_>, cx: &Context<'_>) -> bool {
        self.check_not_held_off(actor, cx).is_ok()
            && actor
                .decide(
                    &roles::administrators_node(&self.workspace),
                    roles::GOVERNANCE_KIND,
                    cx,
                )
                .privileged()
                .answer
                == Answer::Allow
    }

    /// Whether `actor_person`, acting through `actor`, may unmake or remove an
    /// administrator: the owner, while they may make administrators. One
    /// administrator never unmakes another.
    #[must_use]
    pub fn may_unmake_administrators(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        cx: &Context<'_>,
    ) -> bool {
        self.is_owner(actor_person) && self.may_make_administrators(actor, cx)
    }

    /// The grantees a person is known by here: themselves and their role. A
    /// grant to a lower role names them too ([`crate::decide::names`]).
    #[must_use]
    pub fn grantees_of(&self, person: &str) -> Vec<Grantee> {
        // An id that is no person's is known by nothing here.
        let Ok(id) = PersonId::parse(person) else {
            return Vec::new();
        };
        let mut grantees = vec![Grantee::person(id)];
        if let Some(role) = self.role_of(person) {
            grantees.push(Grantee::role(role));
            if let Some(roles) = self.assigned_roles.get(person) {
                grantees.extend(roles.iter().cloned().map(Grantee::CustomRole));
            }
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
        let held_off = self.grants.iter().filter(|grant| {
            grant.grantee.person_id() == Some(person) && moderation.contains(&grant.id)
        });
        let mut party = Party::new(self.grantees_of(person), &self.grants).held_off_by(held_off);
        if self.is_owner(person) || self.members.contains_key(person) {
            party = party.owning_records(roles::own_records(&self.workspace, person));
        }
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

    /// The workspace's grants naming a delegation grantee (an agent, a bot),
    /// for bounding a person's authority. An answer a person gave one agent
    /// session is not among them: it decides that session alone
    /// ([`crate::is_session_answer`]).
    #[must_use]
    pub fn party_of(&self, grantee: Grantee) -> Party<'_> {
        Party::new(vec![grantee], &[]).with_grants(
            self.grants
                .iter()
                .filter(|grant| !crate::is_session_answer(grant)),
        )
    }

    /// The grants naming `grantee`.
    pub fn grants_of<'a>(&'a self, grantee: &'a Grantee) -> impl Iterator<Item = &'a Grant> + 'a {
        self.grants
            .iter()
            .filter(move |grant| &grant.grantee == grantee)
    }

    /// Whether the actor may perform a governance action. `approved` says the
    /// host approved the person's approval when the answer is Ask.
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
                return Err(Refusal::Banned(person.as_str().to_owned()));
            }
            if self.restricted_until(person, cx).is_some() {
                return Err(Refusal::Restricted(person.as_str().to_owned()));
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

    /// Whether `actor` may let someone in at `role` (`add-member`): directly, by issuing an
    /// invitation, or when the invitation they issued is redeemed, which all decide the same.
    /// Never at more than they hold ([`Self::check_role_handout`]), whoever the person is.
    ///
    /// # Errors
    /// The action is not granted, or the role is above what `actor` hands out.
    pub fn check_admission(
        &self,
        actor: &Authority<'_>,
        role: Role,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<(), Refusal> {
        self.check_action(actor, governance::ADD_MEMBER, cx, approved)?;
        self.check_role_handout(actor, role, cx)
    }

    /// Whether `actor_person` (acting through `actor`) may invite someone as
    /// an administrator: an invitation at the Editor role that also makes its
    /// holder an administrator. Only someone who may make administrators
    /// directly ([`Self::may_make_administrators`]) invites one.
    ///
    /// # Errors
    /// As [`Self::check_admission`], and [`Refusal::Administrators`].
    pub fn check_administrator_invite(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<(), Refusal> {
        self.check_admission(actor, Role::Editor, cx, approved)?;
        self.check_makes_administrators(actor_person, actor, cx, false)
    }

    fn check_makes_administrators(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        cx: &Context<'_>,
        unmaking: bool,
    ) -> Result<(), Refusal> {
        let may = if unmaking {
            self.may_unmake_administrators(actor_person, actor, cx)
        } else {
            self.may_make_administrators(actor, cx)
        };
        if may {
            Ok(())
        } else {
            Err(Refusal::Administrators {
                actor: actor_person.to_owned(),
                unmaking,
            })
        }
    }

    /// Whether `change` unmakes or removes an administrator other than
    /// `actor_person`: the owner's alone to do. Leaving, or
    /// giving up one's own administration, is anyone's.
    fn unmakes_administrator(&self, actor_person: &str, change: &Change) -> bool {
        let target = match change {
            Change::RemoveMember { person }
            | Change::Ban { person }
            | Change::Restrict { person, .. }
            | Change::Unrestrict { person }
            | Change::SetRole { person, .. } => person.as_str(),
            Change::Revoke(id) => match roles::administrator_named(id) {
                Some(person) => {
                    return person != actor_person
                        && self.grants.iter().any(|grant| &grant.id == id);
                }
                None => return false,
            },
            _ => return false,
        };
        target != actor_person && self.is_administrator(target)
    }

    /// Whether `actor` may hand out `role`: what the role holds in this
    /// workspace, at each node its grants name, is within what the actor
    /// holds there, or the actor is an administrator. A workspace's own grant
    /// to the role binds whoever it is handed to as much as the giver, so an
    /// Editor of a workspace that denies its Editors outside effects still
    /// hands out Editor. Ownership is handed over, never handed out.
    fn check_role_handout(
        &self,
        actor: &Authority<'_>,
        role: Role,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if role == Role::Owner {
            return Err(Refusal::OwnerRole);
        }
        // An administrator governs as the owner's equal in all but the
        // owner's place: they hand out every role a workspace hands out.
        if self.may_make_administrators(actor, cx) {
            return Ok(());
        }
        let grantee = Grantee::role(role);
        let lifetime = Lifetime::UntilRevoked;
        let handed = Party::new(vec![grantee.clone()], &self.grants);
        let within = roles::role_preset(role, &self.workspace)
            .assign("handout", &grantee, &grantee, &lifetime)
            .iter()
            .all(|grant| {
                grant.kinds.iter().all(|kind| {
                    actor.decide(&grant.node, kind, cx).answer
                        >= handed.decide(&grant.node, kind, cx).answer
                })
            });
        if within {
            Ok(())
        } else {
            Err(Refusal::AboveActorRole(role))
        }
    }

    /// Whether `actor_person` (acting through `actor`) may make `change`,
    /// without making it: what an action API asks before it commits the change by
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
        self.admit(actor_person, actor, change, cx, approved)?;
        self.check_change(actor_person, actor, change, cx)
    }

    /// The grants `change` spends, when `actor` may make it. Leaving needs
    /// none, and neither does a member's managing an agent acting for
    /// themselves, though a restriction holds that off too.
    fn admit(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        change: &Change,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<Vec<GrantId>, Refusal> {
        if is_leaving(actor_person, change) {
            return Ok(Vec::new());
        }
        if let Some(own) = self.manages_own_agent(actor_person, change) {
            self.check_not_held_off(actor, cx)?;
            if self.restricted_until(own, cx).is_some() {
                return Err(Refusal::Restricted(own.to_owned()));
            }
            return Ok(Vec::new());
        }
        if let Change::AddAgent { by: Some(by), .. } | Change::RemoveAgent { by: Some(by), .. } =
            change
        {
            return Err(Refusal::NotPermitted(format!(
                "{by:?} manages only the agents acting for its own person"
            )));
        }
        match change {
            Change::Grant(grant) => self.admit_grant(actor, grant, cx, approved),
            change => self.admit_action(actor, change.action(), cx, approved),
        }
    }

    /// The member person `change`, an agent change, is made for when the
    /// device of `actor_person` signs it: the member it names as acting, a
    /// person or an agent of the member list acting for its person, else the
    /// signer's own person. `None` for one it names that acts for nobody.
    fn agents_actor<'a>(&'a self, actor_person: &'a str, change: &'a Change) -> Option<&'a str> {
        let (Change::AddAgent { by, .. } | Change::RemoveAgent { by, .. }) = change else {
            return Some(actor_person);
        };
        match by {
            None => Some(actor_person),
            Some(crate::MemberId::Person(person)) => Some(person.as_str()),
            Some(crate::MemberId::Agent(agent)) => self
                .agents
                .get(agent)
                .map(|member| member.for_person.as_str()),
            Some(crate::MemberId::Bot(_)) => None,
        }
    }

    /// The person `change` adds or removes an agent of, when it is the one the
    /// change is made for ([`Self::agents_actor`]): a member, written by a
    /// device of a member, so both rights hold it.
    fn manages_own_agent<'a>(&'a self, actor_person: &str, change: &'a Change) -> Option<&'a str> {
        let for_person = match change {
            Change::AddAgent { for_person, .. } => for_person.as_str(),
            Change::RemoveAgent { agent, .. } => self.agents.get(agent)?.for_person.as_str(),
            _ => return None,
        };
        let own = self.agents_actor(actor_person, change)?;
        (own == for_person && self.is_member(own) && self.is_member(actor_person))
            .then_some(for_person)
    }

    /// Applies a governance change made by `actor_person` through `actor`.
    ///
    /// Leaving needs no grant: whoever removes themselves from a workspace does
    /// it by being who they are, and a ban or a restriction stops it no more
    /// than the `remove` action would.
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
        // A change arrives from any device: one naming as a person an id
        // that is none is refused, never applied.
        for named in std::iter::once(actor_person).chain(change.person()) {
            PersonId::parse(named).map_err(|_| Refusal::NotAPerson(named.to_owned()))?;
        }
        let spent = self.admit(actor_person, actor, &change, cx, approved)?;
        self.check_change(actor_person, actor, &change, cx)?;
        if let Change::Grant(grant) = &change
            && self.grants.iter().any(|held| same_terms(held, grant))
        {
            return Ok(());
        }
        self.spend(&spent);
        match change {
            Change::TransferOwnership { person } => self.hand_over(&person, cx),
            Change::AddMember { person, role } => {
                self.joined.entry(person.clone()).or_insert(cx.now_unix_ms);
                self.members.insert(person, role);
            }
            Change::SetRole { person, role } => {
                self.members.insert(person, role);
            }
            Change::RemoveMember { person } => self.drop_member(&person),
            Change::AddAgent {
                agent, for_person, ..
            } => {
                self.agents.insert(agent, AgentMember { for_person });
            }
            Change::RemoveAgent { agent, .. } => {
                self.agents.remove(&agent);
            }
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
            Change::DefineRole { id, name } => {
                self.custom_roles.insert(id, name);
            }
            Change::RemoveRole { id } => {
                self.custom_roles.remove(&id);
                self.assigned_roles.retain(|_, roles| {
                    roles.remove(&id);
                    !roles.is_empty()
                });
                self.grants.retain(
                    |grant| !matches!(&grant.grantee, Grantee::CustomRole(role) if role == &id),
                );
            }
            // Refused by `check_change` before it is reached.
            Change::RetiredSetMemberRoles { .. } => {}
            Change::SetMemberCustomRole {
                person,
                role,
                assigned,
            } => {
                if assigned {
                    self.assigned_roles.entry(person).or_default().insert(role);
                } else if let std::collections::btree_map::Entry::Occupied(mut entry) =
                    self.assigned_roles.entry(person)
                {
                    entry.get_mut().remove(&role);
                    if entry.get().is_empty() {
                        entry.remove();
                    }
                }
            }
        }
        Ok(())
    }

    /// Make `person` the owner. They leave the members, as an owner is none,
    /// and hold no administrator grant, as the owner governs by being the
    /// owner. Whoever owned the workspace stays as an Editor and an
    /// administrator, granted by themselves as its owner when they handed it
    /// over.
    fn hand_over(&mut self, person: &str, cx: &Context<'_>) {
        let previous = self.owner.replace(person.to_owned());
        self.members.remove(person);
        self.joined.remove(person);
        self.assigned_roles.remove(person);
        let theirs = [
            roles::administrator_grant_id(person),
            roles::administrator_moderation_grant_id(person),
        ];
        self.grants.retain(|grant| !theirs.contains(&grant.id));
        let Some(previous) = previous else {
            return;
        };
        self.members.insert(previous.clone(), Role::Editor);
        self.joined
            .entry(previous.clone())
            .or_insert(cx.now_unix_ms);
        let by = Grantee::person(previous.clone());
        for grant in [
            roles::administrator_grant(&self.workspace, &previous, by.clone()),
            roles::administrator_moderation_grant(&self.workspace, &previous, by),
        ] {
            self.grants.retain(|held| held.id != grant.id);
            self.grants.push(grant);
        }
    }

    /// The member, the agents acting for them and every grant naming them
    /// leave the workspace.
    fn drop_member(&mut self, person: &str) {
        self.agents.retain(|_, agent| agent.for_person != person);
        self.members.remove(person);
        self.joined.remove(person);
        self.assigned_roles.remove(person);
        self.grants
            .retain(|grant| grant.grantee.person_id() != Some(person));
    }

    /// Whether `actor_person` may ban or restrict `person`, beyond the grant
    /// on the action: the workspace keeps its owner, and nobody bans or
    /// restricts themselves.
    fn check_bannable(&self, actor_person: &str, person: &str) -> Result<(), Refusal> {
        if self.is_owner(person) {
            return Err(Refusal::Owner);
        }
        if actor_person == person {
            return Err(Refusal::Themselves);
        }
        Ok(())
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
        self.check_administration_change(actor_person, actor, change, cx)?;
        match change {
            Change::TransferOwnership { person } => {
                self.check_ownership_transfer(actor_person, person)
            }
            Change::AddMember { person, role } => {
                self.check_member_addition(actor, person, *role, cx)
            }
            Change::SetRole { person, role } => self.check_member_role(actor, person, *role, cx),
            Change::RemoveMember { person } => self.check_member_removal(person),
            Change::Ban { person } => self.check_ban(actor_person, person),
            Change::Unban { person } => self.check_unban(person),
            Change::Restrict { person, until_ms } => {
                self.check_restriction(actor_person, person, *until_ms, cx)
            }
            Change::Unrestrict { person } => self.check_restriction_lift(actor_person, person, cx),
            Change::Grant(grant) => self.check_new_grant(actor, grant, cx),
            Change::Revoke(id) => self.check_revocation(id),
            Change::DefineRole { id, name } => Self::check_role_definition(id, name),
            Change::RemoveRole { id } => self.require_custom_role(id),
            Change::RetiredSetMemberRoles { .. } => Err(Refusal::NotPermitted(
                "roles are assigned one at a time".to_owned(),
            )),
            Change::SetMemberCustomRole {
                person,
                role,
                assigned,
            } => self.check_member_custom_role(actor, person, role, *assigned, cx),
            Change::AddAgent {
                agent, for_person, ..
            } => self.check_agent_addition(
                self.agents_actor(actor_person, change)
                    .unwrap_or(actor_person),
                agent,
                for_person,
            ),
            Change::RemoveAgent { agent, .. } => self.check_agent_removal(
                self.agents_actor(actor_person, change)
                    .unwrap_or(actor_person),
                agent,
            ),
        }
    }

    /// An agent joins once, acting for a member; one acting for someone else
    /// than the actor is added by the owner or an administrator.
    fn check_agent_addition(
        &self,
        actor_person: &str,
        agent: &str,
        for_person: &str,
    ) -> Result<(), Refusal> {
        if agent.trim().is_empty() {
            return Err(Refusal::NotAMember(agent.to_owned()));
        }
        if self.agents.contains_key(agent) {
            return Err(Refusal::AlreadyAMember(agent.to_owned()));
        }
        if !self.is_member(for_person) {
            return Err(Refusal::NotAMember(for_person.to_owned()));
        }
        self.check_agents_of(actor_person, for_person)
    }

    /// An agent leaves when its person or an administrator removes it.
    fn check_agent_removal(&self, actor_person: &str, agent: &str) -> Result<(), Refusal> {
        let member = self
            .agents
            .get(agent)
            .ok_or_else(|| Refusal::NotAMember(agent.to_owned()))?;
        self.check_agents_of(actor_person, &member.for_person)
    }

    fn check_agents_of(&self, actor_person: &str, for_person: &str) -> Result<(), Refusal> {
        if actor_person == for_person
            || self.is_owner(actor_person)
            || self.is_administrator(actor_person)
        {
            return Ok(());
        }
        Err(Refusal::NotPermitted(format!(
            "only {for_person} or an administrator manages the agents acting for {for_person}"
        )))
    }

    /// The owner alone hands the workspace to someone who is neither its owner
    /// nor banned.
    fn check_ownership_transfer(&self, actor_person: &str, person: &str) -> Result<(), Refusal> {
        if !self.is_owner(actor_person) {
            return Err(Refusal::OwnershipByOwner);
        }
        if self.is_owner(person) {
            return Err(Refusal::Owner);
        }
        if self.is_banned(person) {
            return Err(Refusal::Banned(person.to_owned()));
        }
        Ok(())
    }

    /// An admission takes someone the workspace holds neither as a member nor
    /// as banned, at a role the actor may hand out.
    fn check_member_addition(
        &self,
        actor: &Authority<'_>,
        person: &str,
        role: Role,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if self.is_member(person) {
            return Err(Refusal::AlreadyAMember(person.to_owned()));
        }
        if self.is_banned(person) {
            return Err(Refusal::Banned(person.to_owned()));
        }
        self.check_role_handout(actor, role, cx)
    }

    /// A role changes on someone the membership holds and nobody owns; who may
    /// hand the new role out is the handout's rule.
    fn check_member_role(
        &self,
        actor: &Authority<'_>,
        person: &str,
        role: Role,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if self.is_owner(person) {
            return Err(Refusal::Owner);
        }
        if !self.members.contains_key(person) {
            return Err(Refusal::NotAMember(person.to_owned()));
        }
        self.check_role_handout(actor, role, cx)
    }

    /// A removal takes the membership and every grant naming them. Someone the
    /// membership no longer holds, because the admission an invitation receipt
    /// gave them was taken back first ([`super::super::members`] replay), has
    /// only those grants left to take: removing them is the same change, and
    /// removing them twice changes nothing.
    fn check_member_removal(&self, person: &str) -> Result<(), Refusal> {
        if self.is_owner(person) {
            return Err(Refusal::Owner);
        }
        Ok(())
    }

    fn check_ban(&self, actor_person: &str, person: &str) -> Result<(), Refusal> {
        if self.is_banned(person) {
            return Err(Refusal::Banned(person.to_owned()));
        }
        self.check_bannable(actor_person, person)
    }

    fn check_unban(&self, person: &str) -> Result<(), Refusal> {
        if !self.is_banned(person) {
            return Err(Refusal::NotBanned(person.to_owned()));
        }
        Ok(())
    }

    fn check_restriction(
        &self,
        actor_person: &str,
        person: &str,
        until_ms: u64,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if until_ms <= cx.now_unix_ms {
            return Err(Refusal::InThePast(until_ms));
        }
        self.check_bannable(actor_person, person)
    }

    fn check_restriction_lift(
        &self,
        actor_person: &str,
        person: &str,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if self.restricted_until(person, cx).is_none() {
            return Err(Refusal::NotRestricted(person.to_owned()));
        }
        self.check_bannable(actor_person, person)
    }

    /// A grant is revoked by its own id, and a role's built-in grants are not
    /// the workspace's to take away.
    fn check_revocation(&self, id: &GrantId) -> Result<(), Refusal> {
        if let Some(grant) = self.grants.iter().find(|grant| &grant.id == id) {
            if roles::is_role_grant(grant) {
                return Err(Refusal::BuiltInRole(id.clone()));
            }
            Ok(())
        } else {
            Err(Refusal::UnknownGrant(id.clone()))
        }
    }

    fn check_member_custom_role(
        &self,
        actor: &Authority<'_>,
        person: &str,
        role: &str,
        assigned: bool,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if !self.members.contains_key(person) {
            return Err(Refusal::NotAMember(person.to_owned()));
        }
        self.require_custom_role(role)?;
        for grant in self.grants.iter().filter(|grant| {
            assigned && matches!(&grant.grantee, Grantee::CustomRole(id) if id == role)
        }) {
            Self::check_within(actor, grant, cx)?;
        }
        Ok(())
    }

    fn check_administration_change(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        change: &Change,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        // Administrators are made by whoever may make them, and unmade by the
        // owner alone.
        match change {
            Change::Grant(grant) if roles::administrator_named(&grant.id).is_some() => {
                self.check_makes_administrators(actor_person, actor, cx, false)?;
            }
            change if self.unmakes_administrator(actor_person, change) => {
                self.check_makes_administrators(actor_person, actor, cx, true)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn check_new_grant(
        &self,
        actor: &Authority<'_>,
        grant: &Grant,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        // What a visitor may do is the entry's to say ([`crate::EntryPolicy::visitor_grants`]),
        // never a grant someone writes.
        if roles::is_role_grant(grant) {
            return Err(Refusal::BuiltInRole(grant.id.clone()));
        }
        if grant.grantee == Grantee::AnyKey {
            return Err(Refusal::EntryGrant(grant.id.clone()));
        }
        if grant.node.workspace_id() != Some(self.workspace.as_str()) {
            return Err(Refusal::OutsideWorkspace(grant.node.clone()));
        }
        if let Grantee::CustomRole(id) = &grant.grantee {
            self.require_custom_role(id)?;
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

    fn check_role_definition(id: &str, name: &str) -> Result<(), Refusal> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || name.trim().is_empty()
            || name.trim() != name
            || name.chars().count() > 80
            || name.chars().any(char::is_control)
        {
            return Err(Refusal::InvalidRole);
        }
        Ok(())
    }

    fn require_custom_role(&self, id: &str) -> Result<(), Refusal> {
        if self.custom_roles.contains_key(id) {
            Ok(())
        } else {
            Err(Refusal::UnknownRole(id.to_owned()))
        }
    }
}

/// What a workspace's grants say of each person ([`WorkspacePolicy::grants_by_person`]).
#[derive(Debug, Default)]
pub struct PersonGrants<'p> {
    administrators: BTreeSet<&'p str>,
    extra: BTreeMap<&'p str, usize>,
}

impl PersonGrants<'_> {
    /// Whether a grant makes `person` an administrator: one holds the id
    /// [`roles::administrator_grant_id`] gives them.
    #[must_use]
    pub fn administers(&self, person: &str) -> bool {
        self.administrators.contains(person)
    }

    /// How many permissions the workspace gives `person` beyond their role.
    #[must_use]
    pub fn extra(&self, person: &str) -> usize {
        self.extra.get(person).copied().unwrap_or(0)
    }
}
