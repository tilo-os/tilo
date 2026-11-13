//! A workspace: a unit of collaboration with its own strips, members and
//! permissions.

use std::collections::{BTreeMap, HashMap};
use tilo_sort::CompactSort as _;

use serde::{Deserialize, Serialize};

use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, Principal, ResourceRef, Scope,
    Subject as SandboxSubject, decide_capability,
};
use tilo_action_policy::{
    ActionKind, Answer, Authority, Change, Context, Grant, GrantId, KindSet, Lifetime, Preset,
    PresetError, Refusal, Role, Subject, TreePath, WorkspacePolicy, roles,
};

use crate::app::AppId;
use crate::arrangement::Arrangement;
use crate::capability::Capability;
use crate::collaborator::Collaborator;
use crate::ids::{WorkspaceId, WorkspaceSlot};
use crate::layout::Strip;
use crate::receipt::WorkspaceEffectReceipt;
use crate::shared_surfaces::SharedSurfaces;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "StoredWorkspace")]
pub struct Workspace {
    /// The desk's own handle for this workspace, in the salted strip/window
    /// id space. Stored under `id`, the key it has always been written with.
    #[serde(rename = "id")]
    pub slot: WorkspaceSlot,
    /// Durable identity of this workspace: the same value on every device.
    /// Given once, when the workspace is made ([`Self::new`] mints one,
    /// [`Self::in_workspace`] takes the identity of the workspace created or
    /// joined), and never changed: read it with [`Self::id`]. Joining the
    /// same workspace again lands in the same workspace. Stored under `room`,
    /// the key it has always been written with.
    #[serde(rename = "room")]
    id: WorkspaceId,
    pub name: String,
    pub strips: Vec<Strip>,
    /// Whether the workspace shows its strips or its canvas, and where each
    /// window group stands on the canvas: the same windows, arranged on a
    /// 2D surface ([`crate::arrangement`]).
    #[serde(default, skip_serializing_if = "Arrangement::is_default")]
    pub arrangement: Arrangement,
    /// Who is in this workspace *right now*, asserted present by a
    /// verified invite or presence frame — not durable membership, which
    /// is [`Workspace::policy`].
    /// `skip_deserializing` rather than `skip`: reading is the direction
    /// that can fabricate a stale person, but `Serialize` still feeds the
    /// control socket's state dump.
    #[serde(skip_deserializing)]
    pub collaborators: Vec<Collaborator>,
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
    /// Which of the saver's devices took the latest fire time of each
    /// scheduled shortcut, so a schedule runs once across them. Projected
    /// beside the shortcuts, one map key per schedule.
    #[serde(default)]
    pub schedule_claims: Vec<tilo_automation::ScheduleClaim>,
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
    #[serde(rename = "id")]
    slot: WorkspaceSlot,
    #[serde(rename = "room")]
    id: WorkspaceId,
    name: String,
    strips: Vec<Strip>,
    #[serde(default)]
    arrangement: Arrangement,
    #[serde(skip_deserializing)]
    collaborators: Vec<Collaborator>,
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
    schedule_claims: Vec<tilo_automation::ScheduleClaim>,
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
            None => WorkspacePolicy::unowned(identity.to_string(), &Subject::Role(Role::Owner)),
        };
        Self {
            slot: stored.slot,
            id: identity,
            name: stored.name,
            strips: stored.strips,
            arrangement: stored.arrangement,
            collaborators: stored.collaborators,
            policy,
            sandbox: stored.sandbox,
            presets: stored.presets,
            default_apps: stored.default_apps,
            effect_receipts: stored.effect_receipts,
            shortcuts: stored.shortcuts,
            schedule_claims: stored.schedule_claims,
            bots: Vec::new(),
            shared_surfaces: stored.shared_surfaces,
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
        }
    }
}

/// Stable identifier for a person. The compositor's collaborator table
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

/// A role's stable name, as a collaborator row records it
/// ([`crate::Collaborator::role_id`]). Surfaces translate it for display
/// through [`role_from_id`]; it is never shown as is.
pub fn role_id(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Moderator => "moderator",
        Role::Editor => "editor",
        Role::Commenter => "commenter",
        Role::Viewer => "viewer",
    }
}

/// The role a [`role_id`] names, or `None` for a name this model does not
/// define.
pub fn role_from_id(id: &str) -> Option<Role> {
    Role::ALL.into_iter().find(|role| role_id(*role) == id)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkspaceDefaultError {
    #[error("only a workspace administrator may change workspace policy")]
    NotAdministrator,
    #[error("a person may only change their own workspace default")]
    WrongIdentity,
    #[error("invalid default application: {0}")]
    InvalidSelection(desktop_policy::DefaultAppError),
}

/// The decision context at `now`, Unix seconds, in this process's session:
/// a grant given "until log out" ([`Lifetime::Session`] of
/// [`session_id`]) is dead in any later one.
#[must_use]
pub fn context_at(now: u64) -> Context<'static> {
    Context::at_secs(now).in_session(session_id())
}

/// This process's session, which a grant given "until log out" names.
#[must_use]
pub fn session_id() -> &'static str {
    static SESSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SESSION.get_or_init(|| uuid::Uuid::new_v4().to_string())
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

    /// Record an app's sandbox grant. Returns the ids of the capabilities
    /// written (one per requested scope).
    pub fn grant_app(&mut self, app_id: &str, cap: &Capability) -> Vec<String> {
        let expiry = cap.requested_expiry();
        cap.requested_scopes()
            .into_iter()
            .map(|(scope, access)| {
                self.grant_sandbox(
                    CapOverride::grant(SandboxSubject::app(app_id), scope, access).expiring(expiry),
                )
            })
            .collect()
    }

    pub fn app_capabilities_at(&self, app_id: &str, now: u64) -> Vec<&CapOverride> {
        let want = SandboxSubject::app(app_id);
        let live_at = Some(now);
        self.sandbox
            .iter()
            .filter(|c| c.subject == want && c.effect == Effect::Grant && c.is_live_at(live_at))
            .collect()
    }

    /// Whether app `app_id` may act on this desktop beyond its own window at
    /// `now`: a live [`Capability::DesktopControl`] grant of this
    /// workspace's.
    pub fn holds_desktop_control_at(&self, app_id: &str, now: u64) -> bool {
        let want = SandboxSubject::app(app_id);
        self.sandbox.iter().any(|cap| {
            cap.subject == want
                && cap.effect == Effect::Grant
                && cap.is_live_at(Some(now))
                && matches!(
                    Capability::from_capability(cap),
                    Some(Capability::DesktopControl { .. })
                )
        })
    }

    /// Take back app `app_id`'s desktop control: every
    /// [`Capability::DesktopControl`] grant of its, whatever its lifetime.
    /// Whether there was one.
    pub fn stop_desktop_control(&mut self, app_id: &str) -> bool {
        let want = SandboxSubject::app(app_id);
        !self
            .revoke_sandbox_matching(|cap| {
                cap.subject == want
                    && matches!(
                        Capability::from_capability(cap),
                        Some(Capability::DesktopControl { .. })
                    )
            })
            .is_empty()
    }

    /// A **snapshot** of app grants in the portal's `Capability` shape.
    /// Rebuilt from [`Self::sandbox`] on every call — mutating the returned
    /// map edits a temporary and stores nothing. To grant, use
    /// [`Self::grant_app`].
    pub fn app_grants_snapshot_at(&self, now: u64) -> HashMap<AppId, Vec<Capability>> {
        let live_at = Some(now);
        let mut out: HashMap<AppId, Vec<Capability>> = HashMap::new();
        for c in &self.sandbox {
            if c.effect != Effect::Grant || !c.is_live_at(live_at) {
                continue;
            }
            let SandboxSubject::Principal(Principal::App(app)) = &c.subject else {
                continue;
            };
            if let Some(desc) = Capability::from_capability(c) {
                out.entry(app.clone()).or_default().push(desc);
            }
        }
        out
    }

    /// Whether app `app_id` still holds device `device` ("camera",
    /// "microphone", as [`Capability::device_scope`] spells it) at `now`.
    /// A revoke or an expiry answers `false` here at once, which is what
    /// ends a capture already running: nothing has to restart the app.
    #[must_use]
    pub fn grants_device_at(&self, app_id: &str, device: &str, now: u64) -> bool {
        let live_at = Some(now);
        self.sandbox.iter().any(|capability| {
            capability.effect == Effect::Grant
                && capability.is_live_at(live_at)
                && matches!(
                    &capability.subject,
                    SandboxSubject::Principal(Principal::App(app)) if app == app_id
                )
                && matches!(&capability.scope, Scope::Device(scope) if scope == device)
        })
    }

    /// Does any app hold a grant in this workspace? Drives the Permissions
    /// tab's visibility.
    pub fn has_app_grants_at(&self, now: u64) -> bool {
        let live_at = Some(now);
        self.sandbox.iter().any(|c| {
            c.effect == Effect::Grant
                && c.is_live_at(live_at)
                && matches!(&c.subject, SandboxSubject::Principal(Principal::App(_)))
                && Capability::from_capability(c).is_some()
        })
    }

    /// Ids of the capabilities backing app `app_id`'s grants, in the
    /// same order `Self::grants` renders them, so a Revoke button
    /// that knows a row index can name the capability it removes.
    pub fn app_grant_ids_at(&self, app_id: &str, now: u64) -> Vec<String> {
        let live_at = Some(now);
        let want = SandboxSubject::app(app_id);
        self.sandbox
            .iter()
            .filter(|c| {
                c.subject == want
                    && c.effect == Effect::Grant
                    && c.is_live_at(live_at)
                    && Capability::from_capability(c).is_some()
            })
            .map(|c| c.id.clone())
            .collect()
    }

    /// May `actor` reach `resource` with `access` under the workspace's sandbox?
    pub fn sandbox_decide_at(
        &self,
        actor: &Actor,
        resource: &ResourceRef,
        access: Access,
        now: u64,
    ) -> CapDecision {
        decide_capability(actor, resource, access, &self.sandbox, Some(now))
    }

    /// Record a sandbox capability, minting an id if the caller didn't supply
    /// one. A re-grant of the same (subject, scope, effect, access) merges,
    /// keeping the longer-lived expiry.
    pub fn grant_sandbox(&mut self, mut cap: CapOverride) -> String {
        if let Some(slot) = self.sandbox.iter_mut().find(|c| {
            c.subject == cap.subject
                && c.scope == cap.scope
                && c.effect == cap.effect
                && c.access == cap.access
        }) {
            slot.expiry = collab_types::caps::Expiry::longest(slot.expiry, cap.expiry);
            if !cap.id.is_empty() {
                slot.id = cap.id;
            }
            if cap.granted_by.is_some() {
                slot.granted_by = cap.granted_by;
            }
            return slot.id.clone();
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

    /// A person's own authority in this workspace.
    #[must_use]
    pub fn person_authority(&self, user: &str) -> Authority<'_> {
        Authority::person(self.policy.party(user))
    }

    /// A person's authority, held to the foreign-device bound when they work
    /// from a device that is not theirs, in a workspace this machine does not
    /// govern (the caller decides `foreign` accordingly).
    #[must_use]
    pub fn person_authority_on<'a>(
        &'a self,
        user: &str,
        foreign: bool,
        foreign_bound: &'a [Grant],
    ) -> Authority<'a> {
        self.policy.person_on_device(user, foreign, foreign_bound)
    }

    /// The authority of `subject` acting on its own (an app, or an agent no
    /// person delegated): only what the workspace grants it.
    #[must_use]
    pub fn subject_authority(&self, subject: Subject) -> Authority<'_> {
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
        let grants = self
            .policy
            .grants
            .iter()
            .filter_map(|grant| match grant.lifetime {
                // Live while the clock is before `at` milliseconds.
                Lifetime::Until(at) => Some(at.div_ceil(1000)),
                _ => None,
            });
        let sandbox = self.sandbox.iter().filter_map(|cap| match cap.expiry {
            collab_types::caps::Expiry::AtUnix(deadline) => Some(deadline),
            _ => None,
        });
        grants
            .chain(sandbox)
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
        let person = Subject::Person(user.to_owned());
        self.policy.grants.retain(|grant| grant.subject != person);
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
        let person = Subject::Person(user.to_owned());
        let workspace_node = self.node();
        self.policy.grants.iter().any(|grant| {
            grant.subject == person
                && grant.node == workspace_node
                && grant.answer == Answer::Deny
                && grant.kinds == KindSet::ALL
        })
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
    /// grant that ends with the session is no standing reach and is left out.
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
            let Subject::Person(person) = &grant.subject else {
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
                Lifetime::Once | Lifetime::Session(_) => continue,
            };
            let held = readers.entry(person.clone()).or_insert(until);
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
            let Subject::Agent(agent) = &grant.subject else {
                continue;
            };
            if grant.node != workspace_node || grant.answer == Answer::Deny || !grant.is_live(&cx) {
                continue;
            }
            match agents.iter_mut().find(|(held, _)| held == agent) {
                Some((_, kinds)) => *kinds = kinds.union(grant.kinds),
                None => agents.push((agent.clone(), grant.kinds)),
            }
        }
        agents
    }

    /// What a launch into this workspace hands its apps at `now`: the apps the
    /// workspace's grants make authors, and the workspace's sandbox.
    #[must_use]
    pub fn launch_grants(&self, now: u64) -> collab_types::caps::LaunchGrants {
        let mut authors: Vec<String> = self
            .policy
            .grants
            .iter()
            .filter_map(|grant| match &grant.subject {
                Subject::App(app) => Some(app.clone()),
                _ => None,
            })
            .filter(|app| self.app_is_author_at(app, now))
            .collect();
        authors.compact_sort();
        authors.dedup();
        collab_types::caps::LaunchGrants {
            authors,
            sandbox: self.sandbox.clone(),
        }
    }

    /// Whether application `app` is an author of the workspace at `now`: it may
    /// change the workspace's content ([`roles::app_authorship`]).
    #[must_use]
    pub fn app_is_author_at(&self, app: &str, now: u64) -> bool {
        self.policy.app_authors(app, &context_at(now))
    }

    /// Whether what `app`'s window changes in this workspace is told to that
    /// window as the person's own change: `person` may change the workspace
    /// through `app` at `now` and `app` is an author of it. `None` when
    /// `person` has no place in the workspace, so nothing is told to the window.
    #[must_use]
    pub fn app_writes_at(&self, person: &str, app: &str, now: u64) -> Option<bool> {
        self.role_of(person)?;
        let changes = self.answer_at(person, &self.node().app(app), ActionKind::Undoable, now)
            == Answer::Allow;
        Some(changes && self.app_is_author_at(app, now))
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
                matches!(grant.subject, Subject::Person(_))
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
        subject: &Subject,
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
                &Subject::Person(actor.to_owned()),
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
    /// A workspace whose durable identity is minted here: it exists from the
    /// moment it is made, not lazily.
    pub fn new(slot: WorkspaceSlot, name: impl Into<String>) -> Self {
        Self::in_workspace(slot, WorkspaceId::new(), name)
    }

    /// A new workspace named `name`, whose durable identity is minted here, in
    /// the slot every device derives from that identity ([`WorkspaceSlot::of_workspace`]).
    pub fn create(name: impl Into<String>) -> Self {
        let identity = WorkspaceId::new();
        Self::in_workspace(WorkspaceSlot::of_workspace(identity), identity, name)
    }

    /// Every window of this workspace, in its order: strip by strip, group by group.
    pub fn tabs(&self) -> impl Iterator<Item = &crate::Tab> {
        self.strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
    }

    /// The workspace that is `identity`, created or joined, in the local slot
    /// `slot`: new workspaces take [`WorkspaceSlot::of_workspace`], an existing
    /// one keeps the slot it was stored under.
    pub fn in_workspace(
        slot: WorkspaceSlot,
        identity: WorkspaceId,
        name: impl Into<String>,
    ) -> Self {
        Self {
            slot,
            id: identity,
            name: name.into(),
            strips: Vec::new(),
            arrangement: Arrangement::default(),
            collaborators: Vec::new(),
            policy: WorkspacePolicy::unowned(identity.to_string(), &Subject::Role(Role::Owner)),
            sandbox: Vec::new(),
            presets: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            schedule_claims: Vec::new(),
            bots: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
        }
    }

    /// A copy holding what authority decisions read — identity, the workspace's
    /// policy, sandbox, presets, defaults — and none of the layout or
    /// history: no strips, effect receipts, shortcuts or schedule claims. For
    /// a caller that must not hold the tree while it asks [`Self::answer_at`]
    /// and its kin, so the copy costs the policy, not the workspace's past.
    #[must_use]
    pub fn policy_view(&self) -> Self {
        // Destructured, so a new field has to be sorted into one side.
        let Self {
            slot,
            id,
            name,
            strips: _,
            arrangement: _,
            collaborators,
            policy,
            sandbox,
            presets,
            default_apps,
            effect_receipts: _,
            shortcuts: _,
            schedule_claims: _,
            bots,
            shared_surfaces,
            closed_tabs: _,
        } = self;
        Self {
            slot: *slot,
            id: *id,
            name: name.clone(),
            strips: Vec::new(),
            arrangement: Arrangement::default(),
            collaborators: collaborators.clone(),
            policy: policy.clone(),
            sandbox: sandbox.clone(),
            presets: presets.clone(),
            default_apps: default_apps.clone(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            schedule_claims: Vec::new(),
            bots: bots.clone(),
            shared_surfaces: shared_surfaces.clone(),
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
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
    /// Workspace is minted, not left at [`WorkspaceId::nil`] — two workspaces
    /// built this way must not silently become the same workspace.
    fn default() -> Self {
        let identity = WorkspaceId::new();
        Self {
            slot: WorkspaceSlot::default(),
            id: identity,
            name: String::new(),
            strips: Vec::new(),
            arrangement: Arrangement::default(),
            collaborators: Vec::new(),
            policy: WorkspacePolicy::unowned(identity.to_string(), &Subject::Role(Role::Owner)),
            sandbox: Vec::new(),
            presets: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            schedule_claims: Vec::new(),
            bots: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
        }
    }
}

/// A fresh grant or capability id. Random rather than drawn from the window
/// id space: ids key replicated maps, so they must not collide across peers,
/// and nothing orders or parses them.
fn mint_capability_id() -> String {
    format!("cap-{}", uuid::Uuid::new_v4().simple())
}
