//! A workspace's authority: its owner, its members' roles and its grants,
//! changed only through the workspace's governance actions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    Answer, Authority, Context, Grant, GrantId, Grantee, Level, Party, Role, Segment, TreePath,
    Verdict,
    roles::{self, governance},
    scoped::{self, RoleAssignment, ScopedRole},
};
use tilo_action_types::{ActionKind, Lifetime};
use tilo_member_types::PersonId;

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
    /// The workspace this policy names.
    pub workspace: String,
    /// The owner, by person id. Structural: holds everything and cannot be
    /// removed, demoted or denied by a grant.
    pub owner: Option<String>,
    /// Members other than the owner and the roles each holds, over the whole workspace or
    /// over parts of it ([`RoleAssignment`]).
    pub members: BTreeMap<String, RoleAssignment>,
    /// Parts of the workspace shared with everyone holding a role over the whole workspace, as
    /// they are and as they join later.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub everyone: Vec<ScopedRole>,
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
}

/// A [`WorkspacePolicy`] as written, before its built-in roles are brought to
/// their current definition.
#[derive(Deserialize)]
struct StoredWorkspacePolicy {
    workspace: String,
    owner: Option<String>,
    members: BTreeMap<String, StoredAssignment>,
    #[serde(default)]
    everyone: Vec<ScopedRole>,
    #[serde(default)]
    joined: BTreeMap<String, u64>,
    grants: Vec<Grant>,
    #[serde(default)]
    custom_roles: BTreeMap<String, String>,
    #[serde(default)]
    assigned_roles: BTreeMap<String, BTreeSet<String>>,
}

/// A member's roles as written: one role over the whole workspace, as policies were written
/// before roles took a scope, or the entries.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredAssignment {
    Role(Role),
    Assignment(RoleAssignment),
}

impl From<StoredWorkspacePolicy> for WorkspacePolicy {
    fn from(stored: StoredWorkspacePolicy) -> Self {
        Self {
            workspace: stored.workspace,
            owner: stored.owner,
            members: stored
                .members
                .into_iter()
                .map(|(person, stored)| match stored {
                    StoredAssignment::Role(role) => (person, role.into()),
                    StoredAssignment::Assignment(assigned) => (person, assigned),
                })
                .collect(),
            everyone: stored.everyone,
            joined: stored.joined,
            grants: stored.grants,
            custom_roles: stored.custom_roles,
            assigned_roles: stored.assigned_roles,
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
    /// Give `grantee` (a person, or everyone as `Grantee::Role(Role::Viewer)`) `role` over
    /// `scope`, one part of the workspace, as the signer's own entry beside any other giver's;
    /// or take the signer's entry back (`None`).
    SetScopedRole {
        grantee: Grantee,
        scope: TreePath,
        role: Option<Role>,
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
            Self::AddMember { .. } => governance::ADD_MEMBER,
            Self::RemoveMember { .. } => governance::REMOVE_MEMBER,
            Self::Ban { .. } => governance::BAN,
            Self::Unban { .. } => governance::UNBAN,
            Self::Restrict { .. } => governance::RESTRICT,
            Self::Unrestrict { .. } => governance::UNRESTRICT,
            Self::SetRole { .. } => governance::SET_ROLE,
            Self::TransferOwnership { .. } => governance::TRANSFER_OWNERSHIP,
            Self::Grant(_)
            | Self::DefineRole { .. }
            | Self::RetiredSetMemberRoles { .. }
            | Self::SetMemberCustomRole { assigned: true, .. }
            | Self::SetScopedRole { role: Some(_), .. } => governance::GRANT,
            Self::Revoke(_)
            | Self::RemoveRole { .. }
            | Self::SetScopedRole { role: None, .. }
            | Self::SetMemberCustomRole {
                assigned: false, ..
            } => governance::REVOKE,
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
            | Self::TransferOwnership { person } => Some(person),
            Self::SetScopedRole { grantee, .. } => grantee.person_id(),
            Self::Grant(_)
            | Self::Revoke(_)
            | Self::DefineRole { .. }
            | Self::RemoveRole { .. } => None,
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
    /// A share of the whole workspace: someone holds that as their role, given in People.
    WholeWorkspace,
    /// A part narrower than one app, given to someone who holds no role over the whole
    /// workspace: their devices hold only what has its own key, which is one app's data.
    PartOfApp(TreePath),
    /// A share names one person, or everyone.
    NotPersonOrEveryone,
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
            Self::WholeWorkspace => f.write_str(
                "the whole workspace is not shared: a member's role over all of it is set in People",
            ),
            Self::PartOfApp(scope) => write!(
                f,
                "{scope} is part of an app: someone who is not a member of the whole workspace is \
                 given whole apps, since each app's data is locked with its own key"
            ),
            Self::NotPersonOrEveryone => f.write_str("a part of the workspace is shared with one person or with everyone"),
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
            everyone: Vec::new(),
            joined: BTreeMap::new(),
            grants,
            custom_roles: BTreeMap::new(),
            assigned_roles: BTreeMap::new(),
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
            everyone: Vec::new(),
            joined: BTreeMap::new(),
            grants,
            custom_roles: BTreeMap::new(),
            assigned_roles: BTreeMap::new(),
        }
    }

    /// The policy with each built-in role's grants as the code defines them
    /// now ([`roles::current_role_grants`]). Whatever reads a stored policy
    /// calls it, so stored copies of a role never decide.
    #[must_use]
    pub fn with_current_roles(mut self) -> Self {
        self.grants = roles::current_role_grants(std::mem::take(&mut self.grants));
        self.derive_scoped_grants();
        self
    }

    /// The grants every scoped entry gives ([`scoped::grants`]), in place of those it gave
    /// before: whatever changes an entry calls it, so the grants are always the entries'.
    pub(crate) fn derive_scoped_grants(&mut self) {
        self.grants.retain(|grant| !scoped::is_scoped_grant(grant));
        let workspace = self.workspace.as_str();
        let everyone = Grantee::role(Role::Viewer);
        for (index, entry) in self.everyone.iter().enumerate() {
            self.grants.extend(scoped::grants(
                workspace,
                (scoped::EVERYONE, index),
                entry,
                &everyone,
            ));
        }
        for (person, assigned) in &self.members {
            let grantee = Grantee::person(person.clone());
            for (index, entry) in assigned.scopes.iter().enumerate() {
                if !scoped::is_whole(&entry.scope, workspace) {
                    self.grants
                        .extend(scoped::grants(workspace, (person, index), entry, &grantee));
                }
            }
        }
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
        let entries = self
            .members
            .values_mut()
            .flat_map(|assigned| assigned.scopes.iter_mut())
            .chain(&mut self.everyone);
        for entry in entries {
            entry.scope = entry.scope.with_workspace(&self.workspace, &workspace);
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
        self.members.get(person).map(RoleAssignment::role)
    }

    /// The role a person holds over the whole workspace: `Owner` for the owner, `None` for a
    /// non-member and for a member given only parts of it.
    #[must_use]
    pub fn whole_role_of(&self, person: &str) -> Option<Role> {
        if self.is_owner(person) {
            return Some(Role::Owner);
        }
        self.members.get(person)?.whole()
    }

    /// The parts of the workspace `person` holds a role over: `None` when it is all of it (the
    /// owner, or a member over the whole workspace), and nothing for a non-member. What their
    /// devices hold follows it; a part shared with everyone widens it not.
    #[must_use]
    pub fn member_scope(&self, person: &str) -> Option<Vec<&TreePath>> {
        if self.whole_role_of(person).is_some() {
            return None;
        }
        Some(
            self.members
                .get(person)
                .map(|assigned| assigned.scopes.iter().map(|entry| &entry.scope).collect())
                .unwrap_or_default(),
        )
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
                && grant.kinds == tilo_action_types::KindSet::ALL
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
    /// administers by being the owner, not by this grant.
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
                && !scoped::is_scoped_grant(grant)
            {
                let extra = grants.extra.entry(person).or_default();
                *extra = extra.saturating_add(1);
            }
        }
        grants
    }

    /// Who holds `role` or one that includes it, by person id: the owner and every member so,
    /// as a record its reader rule gives to that role is read.
    pub fn holders(&self, role: Role) -> impl Iterator<Item = &str> + '_ {
        self.owner
            .iter()
            .chain(self.members.keys())
            .map(String::as_str)
            .filter(move |person| self.role_of(person).is_some_and(|held| held.includes(role)))
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
        if let Some(role) = self.whole_role_of(person) {
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
        // A member given parts of the workspace reaches nothing else of it; someone who is no
        // member reaches what grants naming them say, as ever.
        if let Some(scope) = self.member_scope(person)
            && self.members.contains_key(person)
        {
            party = party.within(&self.workspace, scope.into_iter().cloned().collect());
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

    /// The workspace's grants naming a delegation grantee (a bot, an app, a
    /// visitor's key), for bounding a person's authority. An answer a person
    /// gave one agent session is not among them: it decides that session alone
    /// ([`crate::is_session_answer`]). An agent is the pair of its id and the
    /// person it acts for, and only the grants naming that pair reach it
    /// ([`Self::party_of_agent`]), so no grant naming an agent is among these.
    #[must_use]
    pub fn party_of(&self, grantee: Grantee) -> Party<'_> {
        Party::new(vec![grantee], &[]).with_grants(
            self.grants.iter().filter(|grant| {
                !crate::is_session_answer(grant) && grant.grantee.agent_id().is_none()
            }),
        )
    }

    /// The workspace's grants reaching the agent `agent` acting for `person`,
    /// for bounding that person's authority. An agent is the pair: only a grant
    /// naming the pair reaches it, so another person's grant or answer for
    /// their agent of the same id reaches this one not at all. An answer a
    /// person gave one agent session is not among them
    /// ([`crate::is_session_answer`]). An agent its person stopped
    /// ([`crate::agent_answers::stop_grant`]) is refused everything, as a
    /// banned person is: no more specific grant lifts the stop, not the rights
    /// its certificate carries nor an answer its person gave it before. A rule
    /// of the workspace's own that denies the agent (one governance admitted,
    /// never a person's answer for their agent,
    /// [`crate::agent_answers::is_answer`]) holds it off from what it names the
    /// same way, whoever it acts for: what the workspace keeps from an agent is
    /// refused, never put to its person.
    #[must_use]
    #[track_caller]
    pub fn party_of_agent(&self, agent: &str, person: &str) -> Party<'_> {
        let grantee = Grantee::agent(agent, person);
        let stop = crate::agent_answers::stop_grant_id(agent, person);
        let held_off: Vec<&Grant> = self
            .grants
            .iter()
            .filter(|grant| {
                grant.grantee == grantee
                    && ((grant.id == stop && grant.granted_by.person_id() == Some(person))
                        || (grant.answer == Answer::Deny
                            && !crate::agent_answers::is_answer(grant)))
            })
            .collect();
        let theirs: Vec<&Grant> = self
            .grants
            .iter()
            .filter(|grant| {
                !crate::is_session_answer(grant)
                    && (grant.grantee.agent_id().is_none()
                        || (grant.grantee == grantee
                            && grant.granted_by.person_id() == Some(person)))
            })
            .collect();
        Party::new(vec![grantee], &[])
            .with_grants(theirs)
            .held_off_by(held_off)
    }

    /// The agent `agent` of `person` as every decision about it weighs it: its own grants
    /// ([`Self::party_of_agent`]) and those a delegation hands it (`granted`: its certificate's,
    /// the preset its turn carries), which decide nothing once any answer its person gave it
    /// stands ([`Self::answers_narrow`]): then it takes only what their answers name. Admission
    /// and the Settings that show what an agent may do both read this one party.
    #[must_use]
    pub fn party_of_answered_agent<'a>(
        &'a self,
        agent: &str,
        person: &str,
        granted: &'a [Grant],
    ) -> Party<'a> {
        self.party_of_agent(agent, person).narrowed_by(
            granted,
            self.grants_of_agent(agent, person)
                .filter(|grant| crate::agent_answers::is_answer(grant)),
        )
    }

    /// The grants naming the agent `agent` of `person`: those `person` gave it, and the
    /// workspace's own denials of it, as [`Self::party_of_agent`] reads them.
    pub fn grants_of_agent<'a>(
        &'a self,
        agent: &str,
        person: &str,
    ) -> impl Iterator<Item = &'a Grant> {
        self.grants.iter().filter(move |grant| {
            grant
                .grantee
                .as_agent()
                .is_some_and(|(id, of)| id == agent && of.as_str() == person)
                && (grant.granted_by.person_id() == Some(person)
                    || (grant.answer == Answer::Deny && !crate::agent_answers::is_answer(grant)))
        })
    }

    /// Whether `person` narrowed their agent `agent` here: an answer they gave
    /// it stands at `cx`. It then takes only what those answers name, and is
    /// never asked for what they do not. A grant kept for one use of theirs
    /// (the "Always" of a vault item's leave) is no answer and narrows nothing.
    #[must_use]
    pub fn answers_narrow(&self, agent: &str, person: &str, cx: &Context<'_>) -> bool {
        self.grants_of_agent(agent, person)
            .any(|grant| crate::agent_answers::is_answer(grant) && grant.is_live(cx))
    }

    /// Whether `person` stopped their agent `agent` here
    /// ([`crate::agent_answers::stop_grant`]) and has not let it go on: it is
    /// refused every action in the workspace.
    #[must_use]
    pub fn stops(&self, agent: &str, person: &str, cx: &Context<'_>) -> bool {
        let id = crate::agent_answers::stop_grant_id(agent, person);
        self.grants_of_agent(agent, person)
            .any(|grant| grant.id == id && grant.is_live(cx))
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

    /// [`Self::admit_action`] for placing `grant`: a grant assigns one of
    /// the workspace's saved sets ([`roles::preset_assigned`]) when a grant on that
    /// set's assignment node allows it, and any other grant when the
    /// workspace's own grant action does.
    fn admit_grant_action(
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
    /// stops every change, changes to other members included. The owner is never
    /// held off.
    fn check_not_held_off(&self, actor: &Authority<'_>, cx: &Context<'_>) -> Result<(), Refusal> {
        for grantee in &actor.person.grantees {
            let Grantee::Member(tilo_member_types::MemberId::Person(person)) = grantee else {
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
    pub fn unmakes_administrator(&self, actor_person: &str, change: &Change) -> bool {
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
        // An administrator administers as the owner's equal in all but the
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
    /// none, and neither does a person's answer for their own agent, though a
    /// restriction holds that off too.
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
        // A person may always write what their own agent may do, and what it
        // may not, in a workspace they take part in: their say over their own
        // agent needs no governance there, so a stop
        // ([`crate::agent_answers::stop_grant`]) works in a workspace they do
        // not administer.
        if self.answers_for_own_agent(actor_person, change) {
            self.check_not_held_off(actor, cx)?;
            return Ok(Vec::new());
        }
        match change {
            Change::Grant(grant) => self.admit_grant(actor_person, actor, grant, cx, approved),
            Change::Revoke(id) => self.admit_revoke(actor_person, actor, id, cx, approved),
            Change::SetScopedRole {
                grantee,
                scope,
                role,
            } => {
                // A member shares what they hold as their own say, as they always could; letting
                // someone in is `add-member`'s, and taking back another's share `revoke`'s.
                self.check_not_held_off(actor, cx)?;
                match (role, grantee.person_id()) {
                    (None, _) if scoped::gave(self.entries_of(grantee), scope, actor_person) => {
                        Ok(Vec::new())
                    }
                    (None, _) => self.admit_action(actor, governance::REVOKE, cx, approved),
                    (Some(_), Some(person)) if !self.is_member(person) => {
                        self.admit_action(actor, governance::ADD_MEMBER, cx, approved)
                    }
                    (Some(_), _) => Ok(Vec::new()),
                }
            }
            change => self.admit_action(actor, change.action(), cx, approved),
        }
    }

    /// Whether `grant` is one of the workspace's people giving an app what they reach
    /// themselves: recorded and signed as them, on a node they can read, for kinds no wider
    /// than they hold there. A person's file is theirs to open in an app, and the app reaches
    /// exactly what they do on that node, so no one widens access this way.
    fn app_grant_of_member(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        grant: &Grant,
        cx: &Context<'_>,
    ) -> bool {
        matches!(grant.grantee, Grantee::App(_))
            && grant.answer == Answer::Allow
            && grant.granted_by.person_id() == Some(actor_person)
            && self.role_of(actor_person).is_some()
            && actor.decide(&grant.node, ActionKind::Read, cx).answer == Answer::Allow
            && Self::check_within(actor, grant, cx).is_ok()
            && self.check_not_held_off(actor, cx).is_ok()
    }

    /// The grant `change` places, `actor_person`'s own to write when it is an app's and they
    /// reach the node; every other grant is the workspace's `grant` action to allow.
    fn admit_grant(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        grant: &Grant,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<Vec<GrantId>, Refusal> {
        if self.app_grant_of_member(actor_person, actor, grant, cx) {
            return Ok(Vec::new());
        }
        self.admit_grant_action(actor, grant, cx, approved)
    }

    /// The revocation of a grant: its own person's to take back when they gave an app what they
    /// reached, and otherwise the workspace's `revoke` action to allow. Nobody else ends another
    /// person's app grant.
    fn admit_revoke(
        &self,
        actor_person: &str,
        actor: &Authority<'_>,
        id: &GrantId,
        cx: &Context<'_>,
        approved: bool,
    ) -> Result<Vec<GrantId>, Refusal> {
        let held = self.grants.iter().find(|grant| &grant.id == id);
        if held.is_some_and(|grant| {
            matches!(grant.grantee, Grantee::App(_))
                && grant.granted_by.person_id() == Some(actor_person)
                && self.check_not_held_off(actor, cx).is_ok()
        }) {
            return Ok(Vec::new());
        }
        self.admit_action(actor, governance::REVOKE, cx, approved)
    }

    /// Whether `change` is an answer `actor_person` writes, or takes back,
    /// about an agent acting for them: their own say about their agent, which
    /// needs no governance grant in the workspace. It names the agent as the
    /// pair of its id and `actor_person`, so it reaches only their own agent
    /// ([`Self::party_of_agent`]), which acts with no more than they may
    /// themselves. An answer for one agent session is not among these: it
    /// decides the whole tree that session caused, so it is the workspace's
    /// grants' to admit.
    fn answers_for_own_agent(&self, actor_person: &str, change: &Change) -> bool {
        let grant = match change {
            Change::Grant(grant) => grant,
            Change::Revoke(id) => {
                let Some(held) = self.grants.iter().find(|held| &held.id == id) else {
                    return false;
                };
                held
            }
            _ => return false,
        };
        let Some((agent, person)) = grant.grantee.as_agent() else {
            return false;
        };
        // Named as the pair's own answer, so it neither replaces nor takes back
        // another agent's answer.
        let own = format!(
            "{}{}:",
            crate::agent_answers::PREFIX,
            crate::agent_answers::agent_key(agent, actor_person)
        );
        person.as_str() == actor_person
            && grant.granted_by.person_id() == Some(actor_person)
            && !crate::is_session_answer(grant)
            && crate::agent_answers::is_answer(grant)
            && grant.id.as_str().starts_with(&own)
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
        if let Change::Grant(grant) = &change
            && self.grants.iter().any(|held| {
                held.id == grant.id
                    && held.granted_by == grant.granted_by
                    && !same_terms(held, grant)
            })
        {
            // Its giver changing their mind: the grant they gave under this id is taken back and
            // the new one takes its place, one change admitted as both.
            let mut replaced = self.clone();
            replaced.apply(
                actor_person,
                actor,
                Change::Revoke(grant.id.clone()),
                cx,
                approved,
            )?;
            replaced.apply(actor_person, actor, change, cx, approved)?;
            *self = replaced;
            return Ok(());
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
            change @ (Change::AddMember { .. }
            | Change::SetRole { .. }
            | Change::SetScopedRole { .. }) => self.set_roles(actor_person, change, cx),
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
                //
                // An administrator's grant from before it named every
                // governance action sits on the workspace's resource, where
                // the Editor's own Deny outranks it. Its person was given
                // every governance action then too, so the grant is read as
                // the one that says so: no administrator loses their
                // governance, and every device reads the change alike
                // ([`roles::current_administrator_grant`]).
                let grant = roles::current_administrator_grant(&self.workspace, grant);
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

    /// Admitting, a new role and a share set one assignment, each its giver's entry.
    fn set_roles(&mut self, actor_person: &str, change: Change, cx: &Context<'_>) {
        match change {
            Change::AddMember { person, role } | Change::SetRole { person, role } => {
                self.joined.entry(person.clone()).or_insert(cx.now_unix_ms);
                self.members
                    .entry(person)
                    .or_insert_with(|| RoleAssignment { scopes: Vec::new() })
                    .set_whole(role, actor_person);
                self.derive_scoped_grants();
            }
            Change::SetScopedRole {
                grantee,
                scope,
                role,
            } => self.set_scoped(&grantee, &scope, role, actor_person, cx),
            _ => {}
        }
    }

    /// Set `giver`'s entry for `grantee` over `scope`, or take it back; taking back an entry
    /// someone else gave takes every entry there. A person left with none leaves.
    fn set_scoped(
        &mut self,
        grantee: &Grantee,
        scope: &TreePath,
        role: Option<Role>,
        giver: &str,
        cx: &Context<'_>,
    ) {
        let any = role.is_none() && !scoped::gave(self.entries_of(grantee), scope, giver);
        match grantee.person_id() {
            Some(person) => {
                let person = person.to_owned();
                if role.is_some() {
                    self.joined.entry(person.clone()).or_insert(cx.now_unix_ms);
                }
                let assigned = self
                    .members
                    .entry(person.clone())
                    .or_insert_with(|| RoleAssignment { scopes: Vec::new() });
                scoped::set_entry(&mut assigned.scopes, scope, role, giver, any);
                if assigned.scopes.is_empty() {
                    self.drop_member(&person);
                }
            }
            None => scoped::set_entry(&mut self.everyone, scope, role, giver, any),
        }
        self.derive_scoped_grants();
    }

    /// The entries given to `grantee`: a person's, or everyone's.
    fn entries_of(&self, grantee: &Grantee) -> &[ScopedRole] {
        match grantee.person_id() {
            Some(person) => self
                .members
                .get(person)
                .map_or(&[], |assigned| assigned.scopes.as_slice()),
            None => &self.everyone,
        }
    }

    /// Make `person` the owner. They leave the members, as an owner is none,
    /// and hold no administrator grant, as the owner administers by being the
    /// owner. Whoever owned the workspace stays as an Editor and an
    /// administrator, granted by themselves as its owner when they handed it
    /// over.
    ///
    /// A change hands the workspace over through its grants
    /// ([`Change::TransferOwnership`]); an owner's invitation hands it over as its holder joins,
    /// through the workspace's membership replay. This is the effect both take.
    pub fn hand_over(&mut self, person: &str, cx: &Context<'_>) {
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
        self.members.insert(previous.clone(), Role::Editor.into());
        self.derive_scoped_grants();
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

    /// The member and every grant naming them leave the workspace.
    fn drop_member(&mut self, person: &str) {
        self.members.remove(person);
        self.joined.remove(person);
        self.assigned_roles.remove(person);
        self.grants
            .retain(|grant| grant.grantee.person_id() != Some(person));
        let gone = self.everyone.len();
        self.everyone.retain(|entry| entry.by != person);
        if self.everyone.len() != gone {
            self.derive_scoped_grants();
        }
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
            Change::SetScopedRole {
                grantee,
                scope,
                role,
            } => self.check_scoped_role(actor, grantee, scope, *role, cx),
        }
    }

    /// A share names one part of this workspace short of all of it, and one person (neither its
    /// owner nor banned) or everyone. It gives no more than the giver holds there, and someone
    /// who holds no role over the whole workspace is given whole apps.
    fn check_scoped_role(
        &self,
        actor: &Authority<'_>,
        grantee: &Grantee,
        scope: &TreePath,
        role: Option<Role>,
        cx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if scope.workspace_id() != Some(self.workspace.as_str()) {
            return Err(Refusal::OutsideWorkspace(scope.clone()));
        }
        if scoped::is_whole(scope, &self.workspace) {
            return Err(Refusal::WholeWorkspace);
        }
        let person = match grantee {
            Grantee::Role(Role::Viewer) => None,
            grantee => Some(grantee.person_id().ok_or(Refusal::NotPersonOrEveryone)?),
        };
        let Some(role) = role else {
            return Ok(());
        };
        if role == Role::Owner {
            return Err(Refusal::OwnerRole);
        }
        if let Some(person) = person {
            if self.is_owner(person) {
                return Err(Refusal::Owner);
            }
            if self.is_banned(person) {
                return Err(Refusal::Banned(person.to_owned()));
            }
            let whole_app = match scope.segments() {
                [_, Segment::Name(Level::App, _), rest @ ..] => {
                    rest.iter().all(|segment| *segment == Segment::Rest)
                }
                _ => false,
            };
            if self.whole_role_of(person).is_none() && !whole_app {
                return Err(Refusal::PartOfApp(scope.clone()));
            }
        }
        let entry = ScopedRole {
            scope: scope.clone(),
            role,
            by: String::new(),
        };
        let everyone = Grantee::role(Role::Viewer);
        for grant in scoped::grants(&self.workspace, ("", 0), &entry, &everyone) {
            Self::check_within(actor, &grant, cx)?;
        }
        Ok(())
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
            if roles::is_role_grant(grant) || scoped::is_scoped_grant(grant) {
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
        if roles::is_role_grant(grant) || scoped::is_scoped_grant(grant) {
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
            // leaves the workspace as it was instead of refusing. Its own giver replaces it
            // ([`Self::apply`]); nobody else writes over it.
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
