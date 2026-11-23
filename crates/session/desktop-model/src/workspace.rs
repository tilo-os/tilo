//! A workspace: a unit of collaboration with its own strips, members and
//! permissions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use collab_types::caps::{
    Access, CapDecision, CapOverride, Effect, ResourceRef, decide_app, decide_capability,
};
use tilo_action_policy::{
    Acting, ActionKind, Answer, Authority, Change, Context, Grant, GrantId, Grantee, KindSet,
    Lifetime, Preset, PresetError, Refusal, Role, TreePath, WorkspacePolicy, roles,
};

use crate::ids::WorkspaceId;
use crate::layout::Strip;
use crate::layout::WorkspaceLayout;
use crate::present::Present;
use crate::receipt::WorkspaceEffectReceipt;
use crate::shared_surfaces::SharedSurfaces;
use collab_types::Member;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "StoredWorkspace")]
pub struct Workspace {
    id: WorkspaceId,
    /// The workspace's name, as its governance records it: projected onto the shell by the
    /// device showing it ([`Self::named_as`]), never stored or sent with the layout, so the
    /// workspace has one name, the one every device and list of it reads.
    #[serde(skip)]
    pub name: String,
    pub strips: Vec<Strip>,
    /// Whether the workspace shows its strips or its canvas, and where each
    /// window group stands on the canvas: the same windows, arranged on a
    /// 2D surface ([`crate::layout`]).
    #[serde(default, skip_serializing_if = "WorkspaceLayout::is_default")]
    pub layout: WorkspaceLayout,
    /// The workspace's members: its people with their roles, the agents
    /// admitted in it and its bots, each with the colour it wears everywhere.
    /// Placed from [`Workspace::policy`] and presence before a surface reads
    /// them; never read back from a stored workspace.
    /// `skip_deserializing` rather than `skip`: reading is the direction
    /// that can fabricate a stale member, but `Serialize` still feeds the
    /// control socket's state dump.
    #[serde(skip_deserializing, default)]
    pub members: Vec<Member>,
    /// The name each person goes by, as the workspace's governance keeps it, by their root: the
    /// name they gave themselves, which every surface showing someone who is not here reads
    /// ([`Self::person_name`]). Projected by the device showing the workspace from the governance
    /// it holds ([`Self::named_people`]); never stored or sent with the layout.
    #[serde(skip)]
    pub people_names: BTreeMap<UserId, String>,
    /// How each of them is here now: where they are, what they say they are
    /// doing, how available they are. Said again on every frame and never
    /// written: presence, not the workspace's document.
    #[serde(skip, default)]
    pub present: Vec<Present>,
    /// Who may do what in this workspace: its owner (structural, `None` until the
    /// local user adopts it on load), each member's role preset, and every
    /// grant on the workspace's tree. Changed only through governance actions.
    pub policy: WorkspacePolicy,
    /// What apps' code may reach outside their documents in this workspace:
    /// filesystem paths, file records, devices and network hosts granted
    /// through portals. Never what a person grants an agent.
    #[serde(default)]
    pub sandbox: Vec<CapOverride>,
    /// Named grant sets people assign instead of writing grants one by one.
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Default-application policy belongs to the workspace, while each
    /// person's ordinary defaults remain in their personal settings. All
    /// mutation goes through the authority-checked methods below.
    #[serde(default)]
    pub default_apps: desktop_policy::WorkspaceDefaults,
    /// Consequential effects are activity, but never pretend to be edits to
    /// an app's collaborative document.
    #[serde(default, skip_serializing_if = "crate::history::omitted")]
    pub effect_receipts: Vec<WorkspaceEffectReceipt>,
    /// Shortcuts saved in this workspace: a person's own in their personal
    /// workspace, shared ones in a collaborative one. Projected as a CRDT
    /// map keyed by shortcut id, so they reach every desktop the workspace
    /// reaches.
    #[serde(default)]
    pub shortcuts: Vec<tilo_automation::SavedShortcut>,
    /// The workspace's bots, projected from its governance on every change: the
    /// installed ones and those a member asked the workspace's host to run. Never
    /// read back from a stored workspace; the workspace's governance is their
    /// one source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bots: Vec<crate::bots::WorkspaceBot>,
    /// Surfaces from this workspace being streamed to peers. Keys are
    /// wayland `wl_surface` ids; values are the target Iroh peers
    /// (hashed to `u64`) — a `HashSet` for multi-recipient streaming.
    #[serde(default)]
    pub shared_surfaces: SharedSurfaces,
    /// The windows closed here and not reopened, newest last
    /// ([`crate::closed_tabs`]). This device's own history, so it is written
    /// to no document and projected into no workspace: a close is undone where
    /// the person closed it, with the app this device opens.
    #[serde(skip)]
    pub(crate) closed_tabs: crate::closed_tabs::ClosedTabs,
}

/// A workspace as stored: its policy names the workspace it was stored for, or is
/// absent for a workspace that names no authority yet.
#[derive(Deserialize)]
struct StoredWorkspace {
    id: WorkspaceId,
    strips: Vec<Strip>,
    #[serde(default)]
    layout: WorkspaceLayout,
    #[serde(skip_deserializing, default)]
    members: Vec<Member>,
    #[serde(default)]
    policy: Option<WorkspacePolicy>,
    #[serde(default)]
    sandbox: Vec<CapOverride>,
    #[serde(default)]
    presets: Vec<Preset>,
    #[serde(default)]
    default_apps: desktop_policy::WorkspaceDefaults,
    #[serde(default)]
    effect_receipts: Vec<WorkspaceEffectReceipt>,
    #[serde(default)]
    shortcuts: Vec<tilo_automation::SavedShortcut>,
    #[serde(default)]
    shared_surfaces: SharedSurfaces,
}

impl From<StoredWorkspace> for Workspace {
    fn from(stored: StoredWorkspace) -> Self {
        let identity = stored.id;
        let policy = match stored.policy {
            Some(mut policy) => {
                policy.rehome(identity.to_string());
                policy
            }
            None => WorkspacePolicy::unowned(identity.to_string(), &Grantee::role(Role::Owner)),
        };
        Self {
            id: identity,
            name: String::new(),
            strips: stored.strips,
            layout: stored.layout,
            members: stored.members,
            present: Vec::new(),
            policy,
            sandbox: stored.sandbox,
            presets: stored.presets,
            default_apps: stored.default_apps,
            effect_receipts: stored.effect_receipts,
            shortcuts: stored.shortcuts,
            bots: Vec::new(),
            shared_surfaces: stored.shared_surfaces,
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
            people_names: BTreeMap::new(),
        }
    }
}

/// Stable identifier for a person. The compositor's member table
/// keys on this; a peer's Ed25519 key hex is a valid value, and so is
/// the local user's account name.
pub type UserId = String;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PresetMutationError {
    #[error("{0}")]
    Refused(#[from] Refusal),
    #[error("grant set is invalid: {0}")]
    Invalid(#[from] PresetError),
    #[error("grant set was not found")]
    NotFound,
}

/// The app id the workspace's own resources are published under.
pub const WORKSPACE_GOVERNANCE_APP_ID: &str = tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER;
/// Letting one application use another's action, published on the workspace's
/// governance with the app, the other app, its resource and the action.
pub const APP_ACTION_GRANT_ACTION_ID: &str = "app-action.grant";
/// Taking such a grant back, named the same way.
pub const APP_ACTION_REVOKE_ACTION_ID: &str = "app-action.revoke";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkspaceDefaultError {
    #[error("only a workspace administrator may change workspace policy")]
    NotAdministrator,
    #[error("a person may only change their own workspace default")]
    WrongIdentity,
    #[error("invalid default application: {0}")]
    InvalidSelection(desktop_policy::DefaultAppError),
}

/// The decision context at `now`, Unix seconds.
#[must_use]
pub fn context_at(now: u64) -> Context<'static> {
    Context::at_secs(now)
}

/// Every authority question takes the caller's clock as `now`, Unix
/// seconds: grants expire, and the model does not read a clock of its own.
impl Workspace {
    /// Set the administrator layer of default-app policy. The shell supplies
    /// the authenticated local identity; a caller cannot widen policy by
    /// merely painting an enabled control.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceDefaultError::NotAdministrator`] if `actor` may not
    /// change the workspace's settings, or
    /// [`WorkspaceDefaultError::InvalidSelection`] if `policy` names an app
    /// package the resolver rejects for `resource`.
    pub fn set_default_app_policy_as_at(
        &mut self,
        actor: &UserId,
        resource: desktop_policy::ResourceKind,
        policy: desktop_policy::WorkspaceDefaultPolicy,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
        now: u64,
    ) -> Result<(), WorkspaceDefaultError> {
        if !self.may_govern_at(actor, roles::governance::SETTINGS, now) {
            return Err(WorkspaceDefaultError::NotAdministrator);
        }
        if let desktop_policy::WorkspaceDefaultPolicy::App(package) = &policy {
            resolver
                .validate_app(&resource, package)
                .map_err(WorkspaceDefaultError::InvalidSelection)?;
        }
        self.default_apps.policy.insert(resource, policy);
        Ok(())
    }

    /// Set a person's own workspace-level choice. Even a workspace owner may
    /// not silently rewrite another person's preference; administrative
    /// policy is the separate method above and remains visibly attributable.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceDefaultError::WrongIdentity`] if `actor` differs
    /// from `subject` — a person may only set their own choice — or
    /// [`WorkspaceDefaultError::InvalidSelection`] if `choice` names an app
    /// package the resolver rejects for `resource`.
    pub fn set_default_app_choice_as(
        &mut self,
        actor: &UserId,
        subject: &UserId,
        resource: desktop_policy::ResourceKind,
        choice: desktop_policy::UserWorkspaceChoice,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
    ) -> Result<(), WorkspaceDefaultError> {
        if actor != subject {
            return Err(WorkspaceDefaultError::WrongIdentity);
        }
        if let desktop_policy::UserWorkspaceChoice::App(package) = &choice {
            resolver
                .validate_app(&resource, package)
                .map_err(WorkspaceDefaultError::InvalidSelection)?;
        }
        self.default_apps
            .user_choices
            .entry(subject.clone())
            .or_default()
            .insert(resource, choice);
        Ok(())
    }

    // The sandbox: what apps' code may reach, granted through portals.

    pub fn app_capabilities_at(&self, app_id: &str, now: u64) -> Vec<&CapOverride> {
        let live_at = Some(now.saturating_mul(1000));
        self.sandbox
            .iter()
            .filter(|c| c.names_app(app_id) && c.effect == Effect::Grant && c.is_live_at(live_at))
            .collect()
    }

    /// The grant letting app `app_id` use the desktop's actions beyond its
    /// own workspace (`tilo action` typed in a terminal), given by `by`: an
    /// action grant naming the app, on the desktop's own node.
    #[must_use]
    pub fn desktop_control_grant(&self, app_id: &str, by: Grantee) -> Grant {
        Grant::new(
            format!("desktop-control:{app_id}"),
            Grantee::app(app_id),
            self.node().app(crate::commands::APP_ID),
            KindSet::ALL,
            Answer::Allow,
            by,
        )
    }

    /// Whether app `app_id` may act on this desktop beyond its own window at
    /// `now`: what the workspace's grants naming the app answer for the
    /// desktop's own actions.
    pub fn holds_desktop_control_at(&self, app_id: &str, now: u64) -> bool {
        self.policy
            .party_of(Grantee::app(app_id))
            .decide(
                &self.node().app(crate::commands::APP_ID),
                ActionKind::Undoable,
                &context_at(now),
            )
            .answer
            == Answer::Allow
    }

    /// Take back app `app_id`'s desktop control: every grant naming it on the
    /// desktop's actions, whatever its lifetime. Whether there was one.
    pub fn stop_desktop_control(&mut self, app_id: &str) -> bool {
        let node = self.node().app(crate::commands::APP_ID);
        let app = Grantee::app(app_id);
        !self
            .revoke_grants_matching(|grant| grant.grantee == app && grant.node == node)
            .is_empty()
    }

    /// May the app `app` reach `resource` with `access` under the workspace's sandbox?
    pub fn sandbox_decide_at(
        &self,
        app: &str,
        resource: &ResourceRef,
        access: Access,
        now: u64,
    ) -> CapDecision {
        decide_app(
            app,
            resource,
            access,
            &self.sandbox,
            Some(now.saturating_mul(1000)),
        )
    }

    /// May `acting` reach `resource` with `access` under the workspace's sandbox?
    pub fn sandbox_decide_as_at(
        &self,
        acting: &Acting,
        resource: &ResourceRef,
        access: Access,
        now: u64,
    ) -> CapDecision {
        decide_capability(
            acting,
            resource,
            access,
            &self.sandbox,
            Some(now.saturating_mul(1000)),
        )
    }

    /// Record a sandbox capability, minting an id if the caller didn't supply
    /// one. A re-grant of the same (grantee, app, scope, effect, access) merges,
    /// keeping the longer lifetime.
    pub fn grant_sandbox(&mut self, mut cap: CapOverride) -> String {
        if let Some(held) = self.sandbox.iter_mut().find(|c| {
            c.grantee == cap.grantee
                && c.app == cap.app
                && c.scope == cap.scope
                && c.effect == cap.effect
                && c.access == cap.access
        }) {
            held.lifetime = held.lifetime.max(cap.lifetime);
            if !cap.id.is_empty() {
                held.id = cap.id;
            }
            if cap.granted_by.is_some() {
                held.granted_by = cap.granted_by;
            }
            return held.id.clone();
        }
        if cap.id.is_empty() {
            cap.id = mint_capability_id();
        }
        let id = cap.id.clone();
        self.sandbox.push(cap);
        id
    }

    /// Drop the sandbox capability with this id. Returns whether anything was
    /// removed.
    pub fn revoke_sandbox(&mut self, id: &str) -> bool {
        let before = self.sandbox.len();
        self.sandbox.retain(|c| c.id != id);
        self.sandbox.len() != before
    }

    /// Drop every sandbox capability matching `pred`. Returns the ids removed.
    pub fn revoke_sandbox_matching(&mut self, pred: impl Fn(&CapOverride) -> bool) -> Vec<String> {
        let mut removed = Vec::new();
        self.sandbox.retain(|c| {
            if pred(c) {
                removed.push(c.id.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    // Authority over the workspace's tree.

    /// The workspace's node in the published tree.
    #[must_use]
    pub fn node(&self) -> TreePath {
        self.policy.node()
    }

    /// The owner, the one person whose authority is structural.
    #[must_use]
    pub fn owner(&self) -> Option<&UserId> {
        self.policy.owner.as_ref()
    }

    #[must_use]
    pub fn is_owner(&self, user: &str) -> bool {
        self.policy.is_owner(user)
    }

    /// The workspace this workspace is, given when it was made and never changed.
    #[must_use]
    pub fn id(&self) -> WorkspaceId {
        self.id
    }

    /// Project the name the workspace's governance records, `name`, onto this shell: the
    /// workspace's one name, which the layout keeps none of, so the device showing the workspace
    /// projects it here from the governance it holds. Naming it what it is already called changes
    /// nothing. Whether the shell changed.
    pub fn named_as(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.name == name {
            return false;
        }
        name.clone_into(&mut self.name);
        true
    }

    /// Project the names the workspace's governance gives its people, `names` (person root, hex,
    /// to the name they go by), onto this shell. A blank name names nobody. Whether the shell
    /// changed.
    pub fn named_people<'a>(
        &mut self,
        names: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> bool {
        let names: BTreeMap<UserId, String> = names
            .into_iter()
            .map(|(person, name)| (person.to_owned(), name.trim().to_owned()))
            .filter(|(_, name)| !name.is_empty())
            .collect();
        if self.people_names == names {
            return false;
        }
        self.people_names = names;
        true
    }

    /// The name the person `person` (root, hex) goes by here: the one they show by while here,
    /// else the one the workspace's governance keeps for them. `None` for someone nobody named,
    /// never their key.
    #[must_use]
    pub fn person_name(&self, person: &str) -> Option<&str> {
        fn shown(name: &str) -> Option<&str> {
            let name = name.trim();
            let key = name.len() >= 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
            (!name.is_empty() && !key).then_some(name)
        }
        self.members
            .iter()
            .find(|member| member.id.person_id() == Some(person))
            .and_then(|member| shown(&member.name))
            .or_else(|| self.people_names.get(person).and_then(|name| shown(name)))
    }

    /// A person's own authority in this workspace.
    #[must_use]
    pub fn person_authority(&self, user: &str) -> Authority<'_> {
        Authority::person(self.policy.party(user))
    }

    /// The authority of `subject` acting on its own (an app, or an agent no
    /// person delegated): only what the workspace grants it.
    #[must_use]
    pub fn subject_authority(&self, subject: Grantee) -> Authority<'_> {
        Authority::person(self.policy.party_of(subject))
    }

    #[must_use]
    pub fn answer_at(&self, user: &str, node: &TreePath, kind: ActionKind, now: u64) -> Answer {
        self.person_authority(user)
            .decide(node, kind, &context_at(now))
            .answer
    }

    /// Whether `user` may perform the workspace's governance action `action` at
    /// `now`.
    #[must_use]
    pub fn may_govern_at(&self, user: &str, action: &str, now: u64) -> bool {
        self.answer_at(
            user,
            &roles::governance_action(&self.policy.workspace, action),
            roles::GOVERNANCE_KIND,
            now,
        ) == Answer::Allow
    }

    /// Apply a governance change made by `actor` as themselves.
    ///
    /// # Errors
    /// See [`WorkspacePolicy::apply`].
    pub fn govern_as_at(&mut self, actor: &str, change: Change, now: u64) -> Result<(), Refusal> {
        let policy = self.policy.clone();
        let authority = Authority::person(policy.party(actor));
        self.policy
            .apply(actor, &authority, change, &context_at(now), false)
    }

    /// The first instant after `now` (Unix seconds) at which one of this
    /// workspace's grants or sandbox capabilities stops being live: until then
    /// every decision answers the same as at `now` unless the workspace is
    /// written. `None` while nothing expires later.
    pub fn next_grant_expiry_after(&self, now: u64) -> Option<u64> {
        let lifetimes = self.policy.grants.iter().map(|grant| grant.lifetime);
        lifetimes
            .chain(self.sandbox.iter().map(|cap| cap.lifetime))
            .filter_map(|lifetime| match lifetime {
                // Live while the clock is before `at` milliseconds.
                Lifetime::Until(at) => Some(at.div_ceil(1000)),
                _ => None,
            })
            .filter(|deadline| *deadline > now)
            .min()
    }

    /// Record a grant without a governance check, for a host installing the
    /// workspace's own state (an accepted invite, a role preset, a consent the
    /// person gave on their device). Returns its id; a grant with the same id
    /// replaces the earlier one.
    pub fn grant(&mut self, grant: Grant) -> GrantId {
        let id = grant.id.clone();
        self.policy.grants.retain(|held| held.id != id);
        self.policy.grants.push(grant);
        id
    }

    /// Drop the grant with this id. Returns whether anything was removed.
    pub fn revoke_grant(&mut self, id: &GrantId) -> bool {
        let before = self.policy.grants.len();
        self.policy.grants.retain(|grant| &grant.id != id);
        self.policy.grants.len() != before
    }

    /// Drop every grant matching `pred`. Returns the ids removed.
    pub fn revoke_grants_matching(&mut self, pred: impl Fn(&Grant) -> bool) -> Vec<GrantId> {
        let mut removed = Vec::new();
        self.policy.grants.retain(|grant| {
            if pred(grant) {
                removed.push(grant.id.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    /// Admit `user` with `role`, without a governance check (the host
    /// installing an accepted invite or the workspace's genesis).
    pub fn admit(&mut self, user: impl Into<UserId>, role: Role) {
        let user = user.into();
        if self.is_owner(&user) {
            return;
        }
        self.policy.members.insert(user, role);
    }

    /// Remove `user`'s membership and personal grants, without a governance
    /// check. The owner stays.
    pub fn dismiss(&mut self, user: &str) -> bool {
        if self.is_owner(user) {
            return false;
        }
        let person = Grantee::person(user.to_owned());
        self.policy.grants.retain(|grant| grant.grantee != person);
        self.policy.joined.remove(user);
        self.policy.members.remove(user).is_some()
    }

    /// Install `user` as owner; the previous owner, if any, holds nothing more.
    pub fn set_owner(&mut self, user: impl Into<UserId>) {
        let user = user.into();
        self.policy.members.remove(&user);
        self.policy.owner = Some(user);
    }

    /// The role `user` holds: `Owner` for the owner, `None` for a non-member.
    #[must_use]
    pub fn role_of(&self, user: &str) -> Option<Role> {
        self.policy.role_of(user)
    }

    /// Whether `user` is blocked: a member whose own grants deny everything
    /// on the workspace.
    #[must_use]
    pub fn is_blocked(&self, user: &str) -> bool {
        let person = Grantee::person(user.to_owned());
        let workspace_node = self.node();
        self.policy.grants.iter().any(|grant| {
            grant.grantee == person
                && grant.node == workspace_node
                && grant.answer == Answer::Deny
                && grant.kinds == KindSet::ALL
        })
    }

    /// `member` joins this workspace's member list: what a host places from its governance and
    /// presence before a surface reads who is in the workspace. A member already listed keeps
    /// their place.
    pub fn list_member(&mut self, member: Member) {
        match self
            .members
            .iter_mut()
            .find(|listed| listed.id == member.id)
        {
            Some(listed) => *listed = member,
            None => self.members.push(member),
        }
    }

    /// `here` is `member`'s presence now, replacing what was said of them before.
    pub fn place_presence(&mut self, here: Present) {
        match self
            .present
            .iter_mut()
            .find(|placed| placed.member == here.member)
        {
            Some(placed) => *placed = here,
            None => self.present.push(here),
        }
    }

    /// `members` join this workspace's member list, each of them here now saying nothing of
    /// themselves.
    pub fn members_here(&mut self, members: impl IntoIterator<Item = Member>) {
        for member in members {
            self.member_here(member);
        }
    }

    /// `member` is one of this workspace's members and is here now, saying nothing of themselves
    /// yet: what a host places for someone presence named before the roster listed them.
    pub fn member_here(&mut self, member: Member) {
        self.place_presence(Present::of(member.id.clone()));
        self.list_member(member);
    }

    /// The workspace roster: the owner first, then the members by id.
    #[must_use]
    pub fn roster(&self) -> Vec<(UserId, Role)> {
        let mut roster: Vec<(UserId, Role)> = self
            .policy
            .owner
            .iter()
            .map(|owner| (owner.clone(), Role::Owner))
            .collect();
        roster.extend(
            self.policy
                .members
                .iter()
                .map(|(user, role)| (user.clone(), *role)),
        );
        roster
    }

    /// The people who may read the whole workspace at `now`, each with when that
    /// lapses (Unix seconds, exclusive): `None` for its members and anyone
    /// granted until revoked, the end of a time-limited grant otherwise. A
    /// grant that ends with the session is no access reach and is left out.
    #[must_use]
    pub fn workspace_readers_at(&self, now: u64) -> Vec<(UserId, Option<u64>)> {
        let workspace_node = self.node();
        let cx = context_at(now);
        let mut readers: BTreeMap<UserId, Option<u64>> = self
            .roster()
            .into_iter()
            .map(|(person, _)| (person, None))
            .collect();
        for grant in &self.policy.grants {
            let Some(person) = grant.grantee.person_id() else {
                continue;
            };
            if grant.answer != Answer::Allow
                || !grant.kinds.contains(ActionKind::Read)
                || !grant.node.covers(&workspace_node)
            {
                continue;
            }
            let until = match grant.lifetime {
                Lifetime::Until(ms) => Some(ms / 1000),
                Lifetime::UntilRevoked => None,
                Lifetime::Once | Lifetime::Session => continue,
            };
            let held = readers.entry(person.to_owned()).or_insert(until);
            *held = match (*held, until) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
        }
        readers
            .into_iter()
            .filter(|(person, _)| {
                Authority::person(self.policy.party(person))
                    .decide(&workspace_node, ActionKind::Read, &cx)
                    .answer
                    == Answer::Allow
            })
            .collect()
    }

    /// The agents the workspace admits: those holding a live grant on the workspace
    /// itself, with the kinds it allows them.
    #[must_use]
    pub fn agent_members_at(&self, now: u64) -> Vec<(String, KindSet)> {
        let cx = context_at(now);
        let workspace_node = self.node();
        let mut agents: Vec<(String, KindSet)> = Vec::new();
        for grant in &self.policy.grants {
            let Some(agent) = grant.grantee.agent_id() else {
                continue;
            };
            if grant.node != workspace_node || grant.answer == Answer::Deny || !grant.is_live(&cx) {
                continue;
            }
            match agents.iter_mut().find(|(held, _)| held == agent) {
                Some((_, kinds)) => *kinds = kinds.union(grant.kinds),
                None => agents.push((agent.to_owned(), grant.kinds)),
            }
        }
        agents
    }

    /// What a launch into this workspace hands its apps: the workspace's
    /// sandbox.
    #[must_use]
    pub fn launch_grants(&self) -> collab_types::caps::LaunchGrants {
        collab_types::caps::LaunchGrants {
            sandbox: self.sandbox.clone(),
        }
    }

    /// The grants naming a person on one node below the workspace — the share list
    /// of a resource, apart from the workspace's roster.
    #[must_use]
    pub fn shares_of_at(&self, node: &TreePath, now: u64) -> Vec<&Grant> {
        let cx = context_at(now);
        let workspace_node = self.node();
        self.policy
            .grants
            .iter()
            .filter(|grant| {
                grant.grantee.person_id().is_some()
                    && grant.node != workspace_node
                    && grant.answer != Answer::Deny
                    && grant.is_live(&cx)
                    && grant.node.covers(node)
            })
            .collect()
    }

    /// Create or replace a named grant set. The set itself grants nothing;
    /// assigning it does.
    ///
    /// # Errors
    ///
    /// [`PresetMutationError::Refused`] when `actor` may not change the workspace's
    /// settings, or [`PresetMutationError::Invalid`] when `preset` does not
    /// validate.
    pub fn save_preset_with_at(
        &mut self,
        authority: &Authority<'_>,
        mut preset: Preset,
        now: u64,
    ) -> Result<(), PresetMutationError> {
        self.policy.check_action(
            authority,
            roles::governance::SETTINGS,
            &context_at(now),
            false,
        )?;
        preset.validate()?;
        // A grant set is kept below no workspace in particular: assigning it
        // places it in this one.
        for rule in &mut preset.rules {
            rule.node = rule.node.within_workspace();
        }
        if let Some(existing) = self
            .presets
            .iter_mut()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(preset.name.trim()))
        {
            *existing = preset;
        } else {
            self.presets.push(preset);
            crate::grants::sort_presets(&mut self.presets);
        }
        Ok(())
    }

    /// Assign a saved grant set to `subject`, as `actor` acting through
    /// `authority`. Assigning is granting, so the workspace's grant action covers
    /// every set; a grant on one set's assign node covers that set only.
    /// Each grant written is never above the actor's own. Returns the ids
    /// written.
    ///
    /// # Errors
    ///
    /// [`PresetMutationError::NotFound`] for an unknown set, or
    /// [`PresetMutationError::Refused`] when the assignment is refused.
    pub fn assign_preset_with_at(
        &mut self,
        actor: &str,
        authority: &Authority<'_>,
        name: &str,
        subject: &Grantee,
        lifetime: &Lifetime,
        now: u64,
    ) -> Result<Vec<GrantId>, PresetMutationError> {
        let cx = context_at(now);
        let preset = self
            .presets
            .iter()
            .find(|preset| preset.name.eq_ignore_ascii_case(name.trim()))
            .cloned();
        let workspace = self.policy.workspace.clone();
        let verdict = preset.as_ref().map_or_else(
            || {
                authority.decide(
                    &roles::governance_action(&workspace, roles::governance::GRANT),
                    roles::GOVERNANCE_KIND,
                    &cx,
                )
            },
            |preset| roles::assign_preset_verdict(authority, &workspace, &preset.name, &cx),
        );
        if verdict.answer != Answer::Allow {
            return Err(Refusal::NotPermitted(verdict.explain()).into());
        }
        let preset = preset.ok_or(PresetMutationError::NotFound)?;
        let prefix = format!(
            "preset:{}:{}",
            mint_capability_id(),
            preset.name.trim().to_lowercase()
        );
        let workspace_node = self.node();
        let grants: Vec<Grant> = preset
            .assign(
                &prefix,
                subject,
                &Grantee::person(actor.to_owned()),
                lifetime,
            )
            .into_iter()
            .map(|grant| Grant {
                node: workspace_node.join(&grant.node),
                ..grant
            })
            .collect();
        for grant in &grants {
            if grant.node.workspace_id() != Some(workspace.as_str()) {
                return Err(Refusal::OutsideWorkspace(grant.node.clone()).into());
            }
            WorkspacePolicy::check_within(authority, grant, &cx)?;
        }
        let ids = grants.iter().map(|grant| grant.id.clone()).collect();
        self.policy.grants.extend(grants);
        Ok(ids)
    }
}

/// The node of assigning the named grant set `name` in `workspace`: a grant on it
/// lets someone assign that one set.
pub use tilo_action_policy::roles::preset_assign_node;

impl Workspace {
    /// The workspace `id`, named `name`, with nothing in it yet.
    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            strips: Vec::new(),
            layout: WorkspaceLayout::default(),
            members: Vec::new(),
            present: Vec::new(),
            policy: WorkspacePolicy::unowned(id.to_string(), &Grantee::role(Role::Owner)),
            sandbox: Vec::new(),
            presets: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            bots: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
            people_names: BTreeMap::new(),
        }
    }

    /// Every window of this workspace, in its order: strip by strip, group by group.
    pub fn tabs(&self) -> impl Iterator<Item = &crate::Tab> {
        self.strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
    }

    /// A copy holding what authority decisions read — identity, the workspace's
    /// policy, sandbox, presets, defaults — and none of the layout or
    /// history: no strips, effect receipts or shortcuts. For
    /// a caller that must not hold the tree while it asks [`Self::answer_at`]
    /// and its kin, so the copy costs the policy, not the workspace's past.
    #[must_use]
    pub fn policy_view(&self) -> Self {
        // Destructured, so a new field has to be sorted into one side.
        let Self {
            id,
            name,
            strips: _,
            layout: _,
            members,
            people_names,
            present,
            policy,
            sandbox,
            presets,
            default_apps,
            effect_receipts: _,
            shortcuts: _,
            bots,
            shared_surfaces,
            closed_tabs: _,
        } = self;
        Self {
            id: *id,
            name: name.clone(),
            strips: Vec::new(),
            layout: WorkspaceLayout::default(),
            members: members.clone(),
            present: present.clone(),
            policy: policy.clone(),
            sandbox: sandbox.clone(),
            presets: presets.clone(),
            default_apps: default_apps.clone(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            bots: bots.clone(),
            shared_surfaces: shared_surfaces.clone(),
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
            people_names: people_names.clone(),
        }
    }

    /// Mark `surface_id` as being streamed to `peer_id`. Additive — two
    /// calls record both peers in the per-surface set.
    pub fn share_surface(&mut self, surface_id: u64, peer_id: u64) {
        self.shared_surfaces.share(surface_id, peer_id);
    }

    /// Drop the share entry for `surface_id` (no-op if unshared).
    /// Returns the peers it was shared with, so callers can fire
    /// `StopShare` for each without a second lookup.
    pub fn unshare_surface(&mut self, surface_id: u64) -> Option<std::collections::HashSet<u64>> {
        self.shared_surfaces.unshare(surface_id)
    }

    /// Stop streaming `surface_id` to one peer, leaving others intact.
    /// Drops the surface key entirely once the last peer is removed, so
    /// [`Self::is_surface_shared`] reports `false` again.
    pub fn unshare_surface_from_peer(&mut self, surface_id: u64, peer_id: u64) -> bool {
        self.shared_surfaces.unshare_peer(surface_id, peer_id)
    }

    /// Whether `surface_id` is currently streamed somewhere. Used by
    /// the compositor's commit hook to short-circuit the readback for
    /// unshared surfaces (~all of them in steady state).
    pub fn is_surface_shared(&self, surface_id: u64) -> bool {
        self.shared_surfaces.contains(surface_id)
    }
}

impl Default for Workspace {
    /// A workspace with a fresh id: two made this way are never the same workspace.
    fn default() -> Self {
        Self::new(WorkspaceId::new(), String::new())
    }
}

/// A fresh grant or capability id. Random rather than drawn from the window
/// id space: ids key replicated maps, so they must not collide across peers,
/// and nothing orders or parses them.
fn mint_capability_id() -> String {
    format!("cap-{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspaceId;

    /// Someone is called what they show by while here, else what the workspace's governance
    /// names them: never their key, and nobody named stays unnamed.
    #[test]
    fn a_person_goes_by_their_given_name_here_or_not() {
        let ada = "ad".repeat(32);
        let bo = "b0".repeat(32);
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Lounge");
        assert!(workspace.named_people([(ada.as_str(), " Ada "), (bo.as_str(), bo.as_str())]));
        assert!(
            !workspace.named_people([(ada.as_str(), "Ada"), (bo.as_str(), bo.as_str())]),
            "the same names change nothing"
        );
        assert_eq!(
            workspace.person_name(&ada),
            Some("Ada"),
            "governance names someone not here"
        );
        assert_eq!(workspace.person_name(&bo), None, "a key is never a name");
        assert_eq!(workspace.person_name("someone"), None);
        workspace.member_here(Member::named(
            collab_types::MemberId::person(ada.clone()),
            "Ada Lovelace",
        ));
        assert_eq!(
            workspace.person_name(&ada),
            Some("Ada Lovelace"),
            "who is here says their own"
        );
        assert!(
            workspace.named_people([]),
            "names leave with the people the workspace names"
        );
        assert_eq!(workspace.person_name(&bo), None);
    }

    /// A shell takes the name its workspace's governance records, whether it
    /// landed before anything named the workspace or its copy is older.
    #[test]
    fn a_shell_takes_the_name_its_governance_records() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "");
        assert!(workspace.named_as("Tilo"), "an unnamed shell takes it");
        assert_eq!(workspace.name, "Tilo");
        assert!(!workspace.named_as("Tilo"), "its own name changes nothing");
        assert!(workspace.named_as("Tilo Coffee"), "a rename reaches it");
        assert_eq!(workspace.name, "Tilo Coffee");
        assert!(
            !workspace.named_as("  "),
            "a governance that names nothing leaves the name it has"
        );
        assert_eq!(workspace.name, "Tilo Coffee");
        assert!(
            workspace.named_as(" Tilo "),
            "the name as written is trimmed"
        );
        assert_eq!(workspace.name, "Tilo");
    }
}
