//! A workspace: a unit of collaboration with its own strips, members and
//! permissions.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, Principal, ResourceRef,
    Subject as SandboxSubject, decide_capability,
};
use tilo_action_policy::{
    ActionKind, Answer, Authority, Change, Context, Grant, GrantId, KindSet, Lifetime, Preset,
    PresetError, Refusal, Role, RoomPolicy, Subject, TreePath, roles,
};

use crate::app::AppId;
use crate::capability::Capability;
use crate::collaborator::Collaborator;
use crate::ids::{RoomId, WorkspaceId};
use crate::layout::Strip;
use crate::shared_surfaces::SharedSurfaces;
use crate::task::{WorkspaceEffectReceipt, WorkspaceTask};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "StoredWorkspace")]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Durable identity of this workspace as a collaboration room — the
    /// value two hosts must agree on, unlike the local-only [`Self::id`].
    /// Change it with [`Self::set_room`], which moves the room's grants too.
    pub room: RoomId,
    pub name: String,
    pub strips: Vec<Strip>,
    /// Who is in this workspace *right now*, asserted present by a
    /// verified invite or presence frame — not durable membership, which
    /// is [`Workspace::policy`].
    /// `skip_deserializing` rather than `skip`: reading is the direction
    /// that can fabricate a stale person, but `Serialize` still feeds the
    /// control socket's state dump.
    #[serde(skip_deserializing)]
    pub collaborators: Vec<Collaborator>,
    /// One exact, owner-signed offer awaiting the named recipient's consent.
    /// Persisted with the workspace so restart cannot erase, extend, or retarget
    /// the ceremony.
    #[serde(default)]
    pub pending_ownership_transfer: Option<collab_types::OwnershipTransferProposalV1>,
    /// Completed transfers retaining the outgoing owner's signed authorization
    /// and the exact recipient's signed acceptance. This is append-only audit
    /// evidence, not a second source of current ownership.
    #[serde(default, skip_serializing_if = "crate::history::omitted")]
    pub ownership_transfer_audit: Vec<collab_types::OwnershipTransferAuditV1>,
    /// Owner-signed cancellations retained so a cancelled proposal cannot be
    /// replayed after restart.
    #[serde(default, skip_serializing_if = "crate::history::omitted")]
    pub ownership_transfer_cancellations: Vec<collab_types::OwnershipTransferCancellationV1>,
    /// Who may do what in this room: its owner (structural, `None` until the
    /// local user adopts it on load), each member's role preset, and every
    /// grant on the room's tree. Changed only through governance actions.
    pub policy: RoomPolicy,
    /// What apps' code may reach outside their documents in this room:
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
    /// Work shared by the room independently of any one chat thread or app.
    /// Values are projected as a CRDT map keyed by task id.
    #[serde(default)]
    pub tasks: Vec<WorkspaceTask>,
    /// Consequential effects are activity, but never pretend to be edits to
    /// an app's collaborative document.
    #[serde(default, skip_serializing_if = "crate::history::omitted")]
    pub effect_receipts: Vec<WorkspaceEffectReceipt>,
    /// Shortcuts saved in this workspace: a person's own in their personal
    /// workspace, shared ones in a collaborative one. Projected as a CRDT
    /// map keyed by shortcut id, so they reach every desktop the workspace
    /// reaches.
    #[serde(default)]
    pub shortcuts: Vec<tilo_automation::Shortcut>,
    /// Which of the saver's devices took the latest fire time of each
    /// scheduled shortcut, so a schedule runs once across them. Projected
    /// beside the shortcuts, one map key per schedule.
    #[serde(default)]
    pub schedule_claims: Vec<tilo_automation::ScheduleClaim>,
    /// Surfaces from this workspace being streamed to peers. Keys are
    /// wayland `wl_surface` ids; values are the target Iroh peers
    /// (hashed to `u64`) — a `HashSet` for multi-recipient streaming.
    #[serde(default)]
    pub shared_surfaces: SharedSurfaces,
}

/// A workspace as stored: its policy names the room it was stored for, or is
/// absent for a workspace that names no authority yet.
#[derive(Deserialize)]
struct StoredWorkspace {
    id: WorkspaceId,
    room: RoomId,
    name: String,
    strips: Vec<Strip>,
    #[serde(skip_deserializing)]
    collaborators: Vec<Collaborator>,
    #[serde(default)]
    pending_ownership_transfer: Option<collab_types::OwnershipTransferProposalV1>,
    #[serde(default)]
    ownership_transfer_audit: Vec<collab_types::OwnershipTransferAuditV1>,
    #[serde(default)]
    ownership_transfer_cancellations: Vec<collab_types::OwnershipTransferCancellationV1>,
    #[serde(default)]
    policy: Option<RoomPolicy>,
    #[serde(default)]
    sandbox: Vec<CapOverride>,
    #[serde(default)]
    presets: Vec<Preset>,
    #[serde(default)]
    default_apps: desktop_policy::WorkspaceDefaults,
    #[serde(default)]
    tasks: Vec<WorkspaceTask>,
    #[serde(default)]
    effect_receipts: Vec<WorkspaceEffectReceipt>,
    #[serde(default)]
    shortcuts: Vec<tilo_automation::Shortcut>,
    #[serde(default)]
    schedule_claims: Vec<tilo_automation::ScheduleClaim>,
    #[serde(default)]
    shared_surfaces: SharedSurfaces,
}

impl From<StoredWorkspace> for Workspace {
    fn from(stored: StoredWorkspace) -> Self {
        let room = stored.room;
        let policy = match stored.policy {
            Some(mut policy) => {
                policy.rehome(room.to_string());
                policy
            }
            None => RoomPolicy::unowned(room.to_string(), &Subject::Role(Role::Owner)),
        };
        Self {
            id: stored.id,
            room,
            name: stored.name,
            strips: stored.strips,
            collaborators: stored.collaborators,
            pending_ownership_transfer: stored.pending_ownership_transfer,
            ownership_transfer_audit: stored.ownership_transfer_audit,
            ownership_transfer_cancellations: stored.ownership_transfer_cancellations,
            policy,
            sandbox: stored.sandbox,
            presets: stored.presets,
            default_apps: stored.default_apps,
            tasks: stored.tasks,
            effect_receipts: stored.effect_receipts,
            shortcuts: stored.shortcuts,
            schedule_claims: stored.schedule_claims,
            shared_surfaces: stored.shared_surfaces,
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
pub const WORKSPACE_GOVERNANCE_APP_ID: &str = "tilo.workspace";
/// Granting, published on the room: a grant of the room's tree to someone.
pub const GRANT_ACTION_ID: &str = "grant";
/// Revoking one grant.
pub const REVOKE_ACTION_ID: &str = "revoke";
/// Saving a named grant set.
pub const PRESET_SAVE_ACTION_ID: &str = "preset.save";
/// Assigning a named grant set to someone.
pub const PRESET_ASSIGN_ACTION_ID: &str = "preset.assign";
/// Letting one application use another's action.
pub const APP_ACTION_GRANT_ACTION_ID: &str = "app-action.grant";
/// Taking such a grant back.
pub const APP_ACTION_REVOKE_ACTION_ID: &str = "app-action.revoke";

/// The room governance action a published governance action is, which
/// decides who may take it: granting (or assigning a grant set) is
/// granting, revoking is revoking, saving a grant set changes the room's
/// settings.
#[must_use]
pub fn governance_of(action: &str) -> Option<&'static str> {
    match action {
        GRANT_ACTION_ID | PRESET_ASSIGN_ACTION_ID | APP_ACTION_GRANT_ACTION_ID => {
            Some(roles::governance::GRANT)
        }
        REVOKE_ACTION_ID | APP_ACTION_REVOKE_ACTION_ID => Some(roles::governance::REVOKE),
        PRESET_SAVE_ACTION_ID => Some(roles::governance::SETTINGS),
        _ => None,
    }
}

/// The resource the workspace publishes its governance actions on, for the
/// room `room`: the room itself (resource kind [`roles::ROOM_KIND`]).
#[must_use]
pub fn governance_resource_id(room: &RoomId) -> String {
    format!("workspace.permissions:{room}")
}

/// A role's stable name, as a collaborator row records it
/// ([`crate::Collaborator::role_id`]). Surfaces translate it for display
/// through [`role_from_id`]; it is never shown as is.
pub fn role_id(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
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

/// The answer the grant sets of the tasks an invocation works in give for an
/// action of `kind` at `node`, among `tasks` under `policy`: the task the
/// invocation names (`named`), and every task `agent` (an agent's id, without
/// its `agent:` prefix) is running — so a run is held to its task whether or
/// not its calls say so. The least of them; `None` when none of those tasks
/// carries a grant set.
#[must_use]
pub fn task_bound(
    policy: &RoomPolicy,
    tasks: &[crate::WorkspaceTask],
    (agent, named): (Option<&str>, Option<&str>),
    (node, kind): (&TreePath, ActionKind),
    cx: &Context<'_>,
) -> Option<Answer> {
    let running = tasks.iter().filter(|task| {
        matches!(
            task.status,
            crate::WorkspaceTaskStatus::InProgress | crate::WorkspaceTaskStatus::Waiting
        ) && agent.is_some_and(|agent| task.assigned_agent.as_deref() == Some(agent))
    });
    named
        .into_iter()
        .chain(running.map(|task| task.id.as_str()))
        .filter_map(|task| policy.task_answer(task, node, kind, cx))
        .min()
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
    /// change the room's settings, or
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

    /// Does any app hold a grant in this room? Drives the Permissions
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

    /// May `actor` reach `resource` with `access` under the room's sandbox?
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

    // Authority over the room's tree.

    /// The room's node in the published tree.
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

    /// Give the workspace another durable room id; its grants move with it.
    pub fn set_room(&mut self, room: RoomId) {
        self.room = room;
        self.policy.rehome(room.to_string());
    }

    /// A person's own authority in this room.
    #[must_use]
    pub fn person_authority(&self, user: &str) -> Authority<'_> {
        Authority::person(self.policy.party(user))
    }

    /// A person's authority, held to the foreign-device bound when they work
    /// from a device that is not theirs, in a room this machine does not
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
    /// person delegated): only what the room grants it.
    #[must_use]
    pub fn subject_authority(&self, subject: Subject) -> Authority<'_> {
        Authority::person(self.policy.party_of(subject))
    }

    /// The authority of a delegation (an agent, a task, a Shortcut, an app)
    /// acting for `person`: the person's own, bounded by the grants the room
    /// holds for the delegation and by `extra` (grants it carries itself, such
    /// as an agent delegation's).
    #[must_use]
    pub fn delegated_authority<'a>(
        &'a self,
        person: &str,
        delegation: Subject,
        extra: &'a [Grant],
    ) -> Authority<'a> {
        let bound = self.policy.party_of(delegation).with_grants(extra);
        self.person_authority(person).bounded_by(bound)
    }

    /// What `user` may do at `node` for an action of `kind`, at `now`.
    #[must_use]
    pub fn answer_at(&self, user: &str, node: &TreePath, kind: ActionKind, now: u64) -> Answer {
        self.person_authority(user)
            .decide(node, kind, &context_at(now))
            .answer
    }

    /// Whether `user` may perform the room's governance action `action` at
    /// `now`.
    #[must_use]
    pub fn may_govern_at(&self, user: &str, action: &str, now: u64) -> bool {
        self.answer_at(
            user,
            &roles::governance_action(&self.policy.room, action),
            roles::GOVERNANCE_KIND,
            now,
        ) == Answer::Allow
    }

    /// Apply a governance change made by `actor` as themselves.
    ///
    /// # Errors
    /// See [`RoomPolicy::apply`].
    pub fn govern_as_at(&mut self, actor: &str, change: Change, now: u64) -> Result<(), Refusal> {
        let policy = self.policy.clone();
        let authority = Authority::person(policy.party(actor));
        self.policy
            .apply(actor, &authority, change, &context_at(now), false)
    }

    /// Apply a governance change made through `authority` (a person, or a
    /// delegation acting for `actor`). `approved` says the host witnessed
    /// the person's approval where the answer asks first.
    ///
    /// # Errors
    /// See [`RoomPolicy::apply`].
    pub fn govern_with_at(
        &mut self,
        actor: &str,
        authority: &Authority<'_>,
        change: Change,
        now: u64,
        approved: bool,
    ) -> Result<(), Refusal> {
        self.policy
            .apply(actor, authority, change, &context_at(now), approved)
    }

    /// Admit `agent` to the room with `preset`'s grants, given by
    /// `granted_by`, without a governance check, for a host installing the
    /// room's own state. The ids recorded.
    pub fn admit_agent_preset(
        &mut self,
        agent: &str,
        preset: roles::AgentPreset,
        granted_by: &Subject,
    ) -> Vec<GrantId> {
        preset
            .grants(&self.policy.room, agent, granted_by)
            .into_iter()
            .map(|grant| self.grant(grant))
            .collect()
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
    /// room's own state (an accepted invite, a role preset, a consent the
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
    /// installing an accepted invite or the room's genesis).
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
        self.policy.members.remove(user).is_some()
    }

    /// Install `user` as owner; the previous owner, if any, holds nothing more.
    pub fn set_owner(&mut self, user: impl Into<UserId>) {
        let user = user.into();
        self.policy.members.remove(&user);
        self.policy.owner = Some(user);
    }

    /// Transfer the structural owner role.
    ///
    /// Stricter than ordinary governance: only the current owner transfers,
    /// and the recipient must already be a member, so transfer is an
    /// explicit ceremony and never hands the room to a mistyped identity.
    pub fn transfer_owner_as_at(
        &mut self,
        current_owner: &UserId,
        new_owner: &UserId,
        _now: u64,
    ) -> bool {
        if current_owner == new_owner
            || self.owner() != Some(current_owner)
            || !self.policy.members.contains_key(new_owner)
        {
            return false;
        }
        self.set_owner(new_owner.clone());
        true
    }

    /// Give up governance because this workspace joined a room it did not
    /// mint: drops the recorded owner and every grant of governance actions,
    /// so this machine answers for nothing it does not own.
    pub fn surrender_governance(&mut self) -> bool {
        let had_owner = self.policy.owner.take().is_some();
        let governance = roles::room_resource(&self.policy.room);
        let dropped = self.revoke_grants_matching(|grant| {
            matches!(grant.subject, Subject::Person(_))
                && grant.answer != Answer::Deny
                && grant.node.is_within(&governance)
        });
        had_owner || !dropped.is_empty()
    }

    /// The role `user` holds: `Owner` for the owner, `None` for a non-member.
    #[must_use]
    pub fn role_of(&self, user: &str) -> Option<Role> {
        self.policy.role_of(user)
    }

    /// Whether `user` is blocked: a member whose own grants deny everything
    /// on the room.
    #[must_use]
    pub fn is_blocked(&self, user: &str) -> bool {
        let person = Subject::Person(user.to_owned());
        let room = self.node();
        self.policy.grants.iter().any(|grant| {
            grant.subject == person
                && grant.node == room
                && grant.answer == Answer::Deny
                && grant.kinds == KindSet::ALL
        })
    }

    /// The room roster: the owner first, then the members by id.
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

    /// The people who may read the whole room at `now`, each with when that
    /// lapses (Unix seconds, exclusive): `None` for its members and anyone
    /// granted until revoked, the end of a time-limited grant otherwise. A
    /// grant that ends with the session is no standing reach and is left out.
    #[must_use]
    pub fn room_readers_at(&self, now: u64) -> Vec<(UserId, Option<u64>)> {
        let room = self.node();
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
                || !grant.node.covers(&room)
            {
                continue;
            }
            let until = match grant.lifetime {
                Lifetime::Until(ms) => Some(ms / 1000),
                Lifetime::UntilRevoked => None,
                Lifetime::Once | Lifetime::Task(_) | Lifetime::Session(_) => continue,
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
                    .decide(&room, ActionKind::Read, &cx)
                    .answer
                    == Answer::Allow
            })
            .collect()
    }

    /// The agents the room admits: those holding a live grant on the room
    /// itself, with the kinds it allows them.
    #[must_use]
    pub fn agent_members_at(&self, now: u64) -> Vec<(String, KindSet)> {
        let cx = context_at(now);
        let room = self.node();
        let mut agents: Vec<(String, KindSet)> = Vec::new();
        for grant in &self.policy.grants {
            let Subject::Agent(agent) = &grant.subject else {
                continue;
            };
            if grant.node != room || grant.answer == Answer::Deny || !grant.is_live(&cx) {
                continue;
            }
            match agents.iter_mut().find(|(held, _)| held == agent) {
                Some((_, kinds)) => *kinds = kinds.union(grant.kinds),
                None => agents.push((agent.clone(), grant.kinds)),
            }
        }
        agents
    }

    /// The answer task `task`'s grant set gives for an action of `kind` at
    /// `node`; `None` when the task carries no grant set. Work done in the
    /// task is bounded by it, as an agent is bounded by its delegation.
    #[must_use]
    pub fn task_answer_at(
        &self,
        task: &str,
        node: &TreePath,
        kind: ActionKind,
        now: u64,
    ) -> Option<Answer> {
        self.policy.task_answer(task, node, kind, &context_at(now))
    }

    /// What a launch into this room hands its apps at `now`: the apps the
    /// room's grants make authors, and the room's sandbox.
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
        authors.sort();
        authors.dedup();
        collab_types::caps::LaunchGrants {
            authors,
            sandbox: self.sandbox.clone(),
        }
    }

    /// Whether application `app` is an author of the room at `now`: it may
    /// change the room's content ([`roles::app_authorship`]).
    #[must_use]
    pub fn app_is_author_at(&self, app: &str, now: u64) -> bool {
        self.policy.app_authors(app, &context_at(now))
    }

    /// The answer the grant sets of the tasks an invocation works in give
    /// for an action of `kind` at `node`: the task the invocation names, and
    /// every task `agent` (an agent's id, without its `agent:` prefix) is
    /// running here — so a run is held to its task whether or not its calls
    /// say so. The least of them; `None` when none of those tasks carries a
    /// grant set.
    #[must_use]
    pub fn task_bound_at(
        &self,
        agent: Option<&str>,
        named: Option<&str>,
        node: &TreePath,
        kind: ActionKind,
        now: u64,
    ) -> Option<Answer> {
        task_bound(
            &self.policy,
            &self.tasks,
            (agent, named),
            (node, kind),
            &context_at(now),
        )
    }

    /// The role a person with `agent`'s reach in the room would hold: what
    /// its grants let it change — the room, only its comments, or nothing.
    /// `None` when no live grant admits it.
    #[must_use]
    pub fn agent_role_at(&self, agent: &str, now: u64) -> Option<Role> {
        let cx = context_at(now);
        let room = self.node();
        let admitted = self.policy.grants.iter().any(|grant| {
            matches!(&grant.subject, Subject::Agent(held) if held == agent)
                && grant.node == room
                && grant.answer != Answer::Deny
                && grant.is_live(&cx)
        });
        if !admitted {
            return None;
        }
        let authority = self.subject_authority(Subject::agent(agent));
        let changes = |node: &TreePath| {
            authority.decide(node, ActionKind::Undoable, &cx).answer == Answer::Allow
        };
        Some(if changes(&room) {
            Role::Editor
        } else if changes(&roles::comments(&self.policy.room)) {
            Role::Commenter
        } else {
            Role::Viewer
        })
    }

    /// The grants naming a person on one node below the room — the share list
    /// of a resource, apart from the room's roster.
    #[must_use]
    pub fn shares_of_at(&self, node: &TreePath, now: u64) -> Vec<&Grant> {
        let cx = context_at(now);
        let room = self.node();
        self.policy
            .grants
            .iter()
            .filter(|grant| {
                matches!(grant.subject, Subject::Person(_))
                    && grant.node != room
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
    /// [`PresetMutationError::Refused`] when `actor` may not change the room's
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
        // A grant set is kept below no room in particular: assigning it
        // places it in this one.
        for rule in &mut preset.rules {
            rule.node = rule.node.within_room();
        }
        if let Some(existing) = self
            .presets
            .iter_mut()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(preset.name.trim()))
        {
            *existing = preset;
        } else {
            self.presets.push(preset);
            self.presets
                .sort_by_key(|candidate| candidate.name.to_lowercase());
        }
        Ok(())
    }

    /// [`Self::save_preset_with_at`] as `actor` acting as themselves.
    ///
    /// # Errors
    /// As [`Self::save_preset_with_at`].
    pub fn save_preset_as_at(
        &mut self,
        actor: &str,
        preset: Preset,
        now: u64,
    ) -> Result<(), PresetMutationError> {
        let policy = self.policy.clone();
        let authority = Authority::person(policy.party(actor));
        self.save_preset_with_at(&authority, preset, now)
    }

    /// [`Self::assign_preset_with_at`] as `actor` acting as themselves.
    ///
    /// # Errors
    /// As [`Self::assign_preset_with_at`].
    pub fn assign_preset_as_at(
        &mut self,
        actor: &str,
        name: &str,
        subject: &Subject,
        lifetime: &Lifetime,
        now: u64,
    ) -> Result<Vec<GrantId>, PresetMutationError> {
        let policy = self.policy.clone();
        let authority = Authority::person(policy.party(actor));
        self.assign_preset_with_at(actor, &authority, name, subject, lifetime, now)
    }

    /// Assign a saved grant set to `subject`, as `actor` acting through
    /// `authority`. Assigning is granting, so the room's grant action covers
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
        let room = self.policy.room.clone();
        let may = |node: &TreePath| {
            authority.decide(node, roles::GOVERNANCE_KIND, &cx).answer == Answer::Allow
        };
        let on_set = preset
            .as_ref()
            .is_some_and(|preset| may(&preset_assign_node(&room, &preset.name)));
        if !on_set && !may(&roles::governance_action(&room, roles::governance::GRANT)) {
            let verdict = authority.decide(
                &roles::governance_action(&room, roles::governance::GRANT),
                roles::GOVERNANCE_KIND,
                &cx,
            );
            return Err(Refusal::NotPermitted(verdict.explain()).into());
        }
        let preset = preset.ok_or(PresetMutationError::NotFound)?;
        let prefix = format!(
            "preset:{}:{}",
            mint_capability_id(),
            preset.name.trim().to_lowercase()
        );
        let room_node = self.node();
        let grants: Vec<Grant> = preset
            .assign(
                &prefix,
                subject,
                &Subject::Person(actor.to_owned()),
                lifetime,
            )
            .into_iter()
            .map(|grant| Grant {
                node: room_node.join(&grant.node),
                ..grant
            })
            .collect();
        for grant in &grants {
            if grant.node.room_id() != Some(room.as_str()) {
                return Err(Refusal::OutsideRoom(grant.node.clone()).into());
            }
            RoomPolicy::check_within(authority, grant, &cx)?;
        }
        let ids = grants.iter().map(|grant| grant.id.clone()).collect();
        self.policy.grants.extend(grants);
        Ok(ids)
    }
}

/// The node of assigning the named grant set `name` in `room`: a grant on it
/// lets someone assign that one set.
#[must_use]
pub fn preset_assign_node(room: &str, name: &str) -> TreePath {
    roles::room_resource(room)
        .field("presets")
        .item(name.trim().to_lowercase())
        .action("assign")
}

impl Workspace {
    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        // A workspace that exists is a room that exists: minted here rather
        // than lazily.
        let room = RoomId::new();
        Self {
            id,
            room,
            name: name.into(),
            strips: Vec::new(),
            collaborators: Vec::new(),
            pending_ownership_transfer: None,
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            policy: RoomPolicy::unowned(room.to_string(), &Subject::Role(Role::Owner)),
            sandbox: Vec::new(),
            presets: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            schedule_claims: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
        }
    }

    /// A copy holding what authority decisions read — identity, the room's
    /// policy, sandbox, presets, defaults — and none of the layout or
    /// history: no strips, tasks, effect receipts, shortcuts, schedule
    /// claims or ownership-transfer records. For a caller that must not hold
    /// the tree while it asks [`Self::answer_at`] and its kin, so the copy
    /// costs the policy, not the workspace's past.
    #[must_use]
    pub fn policy_view(&self) -> Self {
        // Destructured, so a new field has to be sorted into one side.
        let Self {
            id,
            room,
            name,
            strips: _,
            collaborators,
            pending_ownership_transfer,
            ownership_transfer_audit: _,
            ownership_transfer_cancellations: _,
            policy,
            sandbox,
            presets,
            default_apps,
            tasks: _,
            effect_receipts: _,
            shortcuts: _,
            schedule_claims: _,
            shared_surfaces,
        } = self;
        Self {
            id: *id,
            room: *room,
            name: name.clone(),
            strips: Vec::new(),
            collaborators: collaborators.clone(),
            pending_ownership_transfer: pending_ownership_transfer.clone(),
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            policy: policy.clone(),
            sandbox: sandbox.clone(),
            presets: presets.clone(),
            default_apps: default_apps.clone(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            schedule_claims: Vec::new(),
            shared_surfaces: shared_surfaces.clone(),
        }
    }

    /// This workspace's collaboration cluster id — [`Self::room`], not
    /// the local [`Self::id`]. Unminted returns the nil uuid, which
    /// `CrdtRoute::delivers_to` refuses to deliver to.
    pub fn collab_cluster_id(&self) -> uuid::Uuid {
        self.room.as_uuid()
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
    /// Room is minted, not left at [`RoomId::nil`] — two workspaces
    /// built this way must not silently become the same room.
    fn default() -> Self {
        let room = RoomId::new();
        Self {
            id: WorkspaceId::default(),
            room,
            name: String::new(),
            strips: Vec::new(),
            collaborators: Vec::new(),
            pending_ownership_transfer: None,
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            policy: RoomPolicy::unowned(room.to_string(), &Subject::Role(Role::Owner)),
            sandbox: Vec::new(),
            presets: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            schedule_claims: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
        }
    }
}

/// A fresh grant or capability id. Random rather than drawn from the window
/// id space: ids key replicated maps, so they must not collide across peers,
/// and nothing orders or parses them.
fn mint_capability_id() -> String {
    format!("cap-{}", uuid::Uuid::new_v4().simple())
}
