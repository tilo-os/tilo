//! A workspace: a unit of collaboration with its own strips, members and
//! permissions.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};
use collab_types::role::Role as CapRole;
use collab_types::{PermissionProfile, PermissionProfileError};

use crate::app::AppId;
use crate::capability::Capability;
use crate::collaborator::Collaborator;
use crate::ids::{RoomId, WorkspaceId};
use crate::layout::Strip;
use crate::shared_surfaces::SharedSurfaces;
use crate::task::{WorkspaceEffectReceipt, WorkspaceTask};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Durable identity of this workspace as a collaboration room — the
    /// value two hosts must agree on, unlike the local-only [`Self::id`].
    pub room: RoomId,
    pub name: String,
    pub strips: Vec<Strip>,
    /// Who is in this workspace *right now*, asserted present by a
    /// verified invite or presence frame — not durable membership, which
    /// is [`Workspace::owner`] / [`Workspace::capabilities`].
    /// `skip_deserializing` rather than `skip`: reading is the direction
    /// that can fabricate a stale person, but `Serialize` still feeds the
    /// control socket's state dump.
    #[serde(skip_deserializing)]
    pub collaborators: Vec<Collaborator>,
    /// Who owns this room — the one principal whose authority is not
    /// itself a capability. `None` until the local user adopts it on load.
    #[serde(default)]
    pub owner: Option<UserId>,
    /// One exact, owner-signed offer awaiting the named recipient's consent.
    /// Persisted with the workspace so restart cannot erase, extend, or retarget
    /// the ceremony.
    #[serde(default)]
    pub pending_ownership_transfer: Option<collab_types::OwnershipTransferProposalV1>,
    /// Completed transfers retaining the outgoing owner's signed authorization
    /// and the exact recipient's signed acceptance. This is append-only audit
    /// evidence, not a second source of current ownership.
    #[serde(default)]
    pub ownership_transfer_audit: Vec<collab_types::OwnershipTransferAuditV1>,
    /// Owner-signed cancellations retained so a cancelled proposal cannot be
    /// replayed after restart.
    #[serde(default)]
    pub ownership_transfer_cancellations: Vec<collab_types::OwnershipTransferCancellationV1>,
    /// **Every** permission in this room, as capabilities — one list, one
    /// vocabulary (room membership, file shares, portal grants), so it
    /// can't drift the way a second per-app map would.
    #[serde(default)]
    pub capabilities: Vec<CapOverride>,
    /// Reusable least-privilege bundles authored for this workspace. Profiles
    /// are policy source; assigned authority is still materialized into
    /// `capabilities`, the sole enforcement and replication vocabulary.
    #[serde(default)]
    pub permission_profiles: Vec<PermissionProfile>,
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
    #[serde(default)]
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

/// Stable identifier for a person. The compositor's collaborator table
/// keys on this; a peer's Ed25519 key hex is a valid value, and so is
/// the local user's account name.
pub type UserId = String;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PermissionProfileMutationError {
    #[error("only a workspace administrator can manage permission profiles")]
    NotAdministrator,
    #[error("permission profile is invalid: {0}")]
    Invalid(#[from] PermissionProfileError),
    #[error("permission profile was not found")]
    NotFound,
    #[error("permission profile actor has no assignable identity")]
    MissingPrincipal,
}

pub const WORKSPACE_GOVERNANCE_APP_ID: &str = "tilo.workspace";
pub const PROFILE_SAVE_ACTION_ID: &str = "profile.save";
pub const PROFILE_ASSIGN_ACTION_ID: &str = "profile.assign";
/// Governance action letting one application use another's action.
pub const APP_ACTION_GRANT_ACTION_ID: &str = "app-action.grant";
/// Governance action taking such a grant back.
pub const APP_ACTION_REVOKE_ACTION_ID: &str = "app-action.revoke";

/// A role's stable name, as a collaborator row records it
/// ([`crate::Collaborator::role_id`]). Surfaces translate it for display
/// through [`role_from_id`]; it is never shown as is.
pub fn role_id(role: CapRole) -> &'static str {
    match role {
        CapRole::Owner => "owner",
        CapRole::Editor => "editor",
        CapRole::Commenter => "commenter",
        CapRole::Viewer => "viewer",
        // Not "no role" — a blocked member is still in the room's
        // directory, and a row that said nothing would read as ordinary.
        CapRole::Denied => "blocked",
    }
}

/// The role a [`role_id`] names, or `None` for a name this model does not
/// define.
pub fn role_from_id(id: &str) -> Option<CapRole> {
    [
        CapRole::Owner,
        CapRole::Editor,
        CapRole::Commenter,
        CapRole::Viewer,
        CapRole::Denied,
    ]
    .into_iter()
    .find(|role| role_id(*role) == id)
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

/// Every authority question takes the caller's clock as `now`, Unix
/// seconds: capabilities expire, and the model does not read a clock of its
/// own. An unset clock reads as 0, making every `AtUnix` capability look
/// live — safe for a *grant*, unsafe for a *restriction*, so hosts write
/// restrictions without an expiry.
impl Workspace {
    /// Set the administrator layer of default-app policy. The shell supplies
    /// the authenticated local identity; a caller cannot widen policy by
    /// merely painting an enabled control.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceDefaultError::NotAdministrator`] if `actor` does
    /// not hold administrator authority in this workspace, or
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
        if !self.may_administer_at(actor, now) {
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

    // App grants — the portal's view of the one capability list. Not a
    // second store: they read/write `self.capabilities`, filtered to
    // subjects that are apps.

    /// Record an app's grant. Returns the ids of the capabilities
    /// written (one per requested scope).
    pub fn grant_app(&mut self, app_id: &str, cap: &Capability) -> Vec<String> {
        let expiry = cap.requested_expiry();
        cap.requested_scopes()
            .into_iter()
            .map(|(scope, verb)| {
                self.grant_capability(
                    CapOverride::grant(Subject::app(app_id), scope, verb).expiring(expiry),
                )
            })
            .collect()
    }

    pub fn app_capabilities_at(&self, app_id: &str, now: u64) -> Vec<&CapOverride> {
        let want = Subject::app(app_id);
        let live_at = Some(now);
        self.capabilities
            .iter()
            .filter(|c| c.subject == want && c.effect == Effect::Grant && c.is_live_at(live_at))
            .collect()
    }

    /// A **snapshot** of app grants in the portal's `Capability` shape.
    /// Rebuilt from [`Self::capabilities`] on every call — mutating the
    /// returned map edits a temporary and stores nothing. To grant, use
    /// [`Self::grant_app`] or [`Self::grant_as_at`].
    pub fn app_grants_snapshot_at(&self, now: u64) -> HashMap<AppId, Vec<Capability>> {
        let live_at = Some(now);
        let mut out: HashMap<AppId, Vec<Capability>> = HashMap::new();
        for c in &self.capabilities {
            if c.effect != Effect::Grant || !c.is_live_at(live_at) {
                continue;
            }
            let Subject::Principal(Principal::App(app)) = &c.subject else {
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
        self.capabilities.iter().any(|c| {
            c.effect == Effect::Grant
                && c.is_live_at(live_at)
                && matches!(&c.subject, Subject::Principal(Principal::App(_)))
                && Capability::from_capability(c).is_some()
        })
    }

    /// Ids of the capabilities backing app `app_id`'s grants, in the
    /// same order `Self::grants` renders them, so a Revoke button
    /// that knows a row index can name the capability it removes.
    pub fn app_grant_ids_at(&self, app_id: &str, now: u64) -> Vec<String> {
        let live_at = Some(now);
        let want = Subject::app(app_id);
        self.capabilities
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

    // The capability store — every permission question bottoms out in
    // `decide` / `allows` below.

    /// Decide whether `actor` may apply `verb` to `resource` in this
    /// room. Fail-closed: denies unless something explicitly permits.
    pub fn decide_at(
        &self,
        actor: &Actor,
        resource: &ResourceRef,
        verb: Verb,
        now: u64,
    ) -> CapDecision {
        decide_capability(actor, resource, verb, &self.capabilities, Some(now))
    }

    /// [`Self::decide_at`] reduced to a bool, for call sites that only
    /// gate an action rather than explaining a denial.
    pub fn allows_at(&self, actor: &Actor, resource: &ResourceRef, verb: Verb, now: u64) -> bool {
        self.decide_at(actor, resource, verb, now) == CapDecision::Allow
    }

    /// Record a capability, minting an id if the caller didn't supply
    /// one. Authority to *make* the grant is checked by the caller
    /// ([`Self::grant_as_at`]), not here.
    pub fn grant_capability(&mut self, mut cap: CapOverride) -> String {
        if cap.id.is_empty() {
            cap.id = mint_capability_id();
        }
        // Merges rather than stacks a re-grant of the same (subject,
        // scope, effect, verb), keeping the longer-lived expiry — a
        // grant is monotonic; reducing authority is revoke's job.
        if let Some(slot) = self.capabilities.iter_mut().find(|c| {
            c.subject == cap.subject
                && c.scope == cap.scope
                && c.effect == cap.effect
                && c.verb == cap.verb
        }) {
            slot.expiry = Expiry::longest(slot.expiry, cap.expiry);
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
        self.capabilities.push(cap);
        id
    }

    /// Grant a capability **on behalf of** `grantor`, enforcing that
    /// the grantor actually holds authority to delegate it — a peer
    /// without `Admin` cannot widen anyone's access, including their
    /// own. `None` when the grantor lacks that authority.
    pub fn grant_as_at(&mut self, grantor: &UserId, cap: CapOverride, now: u64) -> Option<String> {
        if !self.may_administer_at(grantor, now) {
            return None;
        }
        let cap = cap.granted_by(Principal::Person(grantor.clone()));
        Some(self.grant_capability(cap))
    }

    /// Create or replace a workspace permission profile. The profile itself
    /// grants nothing; assignment below materializes ordinary capabilities.
    ///
    /// # Errors
    ///
    /// See [`Self::save_permission_profile_as_actor_at`].
    pub fn save_permission_profile_as_at(
        &mut self,
        actor: &UserId,
        profile: PermissionProfile,
        now: u64,
    ) -> Result<(), PermissionProfileMutationError> {
        self.save_permission_profile_as_actor_at(
            &self.actor_for_person_at(actor, now),
            profile,
            now,
        )
    }

    /// Actor-general form used by the broker. Agents and apps have no ambient
    /// role; they can manage profiles only when explicitly granted this exact
    /// governance action (or the containing governance app/workspace).
    ///
    /// # Errors
    ///
    /// [`PermissionProfileMutationError::NotAdministrator`] when `actor` may
    /// not manage profiles here, or [`PermissionProfileMutationError::Invalid`]
    /// when `profile` does not validate.
    pub fn save_permission_profile_as_actor_at(
        &mut self,
        actor: &Actor,
        profile: PermissionProfile,
        now: u64,
    ) -> Result<(), PermissionProfileMutationError> {
        if !self.allows_at(
            actor,
            &ResourceRef::Action {
                app: WORKSPACE_GOVERNANCE_APP_ID.into(),
                action: PROFILE_SAVE_ACTION_ID.into(),
            },
            Verb::Admin,
            now,
        ) {
            return Err(PermissionProfileMutationError::NotAdministrator);
        }
        profile.validate()?;
        if let Some(existing) = self
            .permission_profiles
            .iter_mut()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(profile.name.trim()))
        {
            *existing = profile;
        } else {
            self.permission_profiles.push(profile);
            self.permission_profiles
                .sort_by_key(|candidate| candidate.name.to_lowercase());
        }
        Ok(())
    }

    /// Assign a saved profile to a person, app, or agent in one validated
    /// mutation. Returned ids are the exact revocation/audit handles written
    /// to the canonical capability store.
    ///
    /// # Errors
    ///
    /// See [`Self::assign_permission_profile_as_actor_at`].
    pub fn assign_permission_profile_as_at(
        &mut self,
        actor: &UserId,
        profile_name: &str,
        subject: &Subject,
        expiry: Expiry,
        now: u64,
    ) -> Result<Vec<String>, PermissionProfileMutationError> {
        self.assign_permission_profile_as_actor_at(
            &self.actor_for_person_at(actor, now),
            profile_name,
            subject,
            expiry,
            now,
        )
    }

    /// [`Self::assign_permission_profile_as_at`] for any actor, as the broker
    /// calls it.
    ///
    /// # Errors
    ///
    /// [`PermissionProfileMutationError::NotAdministrator`] when `actor` may
    /// not assign profiles, [`PermissionProfileMutationError::NotFound`] for
    /// an unknown profile, [`PermissionProfileMutationError::MissingPrincipal`]
    /// when `actor` carries no assignable identity, or
    /// [`PermissionProfileMutationError::Invalid`] when the assignment does
    /// not validate.
    pub fn assign_permission_profile_as_actor_at(
        &mut self,
        actor: &Actor,
        profile_name: &str,
        subject: &Subject,
        expiry: Expiry,
        now: u64,
    ) -> Result<Vec<String>, PermissionProfileMutationError> {
        if !self.allows_at(
            actor,
            &ResourceRef::Action {
                app: WORKSPACE_GOVERNANCE_APP_ID.into(),
                action: PROFILE_ASSIGN_ACTION_ID.into(),
            },
            Verb::Admin,
            now,
        ) {
            return Err(PermissionProfileMutationError::NotAdministrator);
        }
        let profile = self
            .permission_profiles
            .iter()
            .find(|profile| profile.name.eq_ignore_ascii_case(profile_name.trim()))
            .cloned()
            .ok_or(PermissionProfileMutationError::NotFound)?;
        let grantor = actor
            .principals
            .iter()
            .find(|principal| !matches!(principal, Principal::Room(_)))
            .cloned()
            .ok_or(PermissionProfileMutationError::MissingPrincipal)?;
        let capabilities = profile.assign_to(subject, &grantor, expiry)?;
        Ok(capabilities
            .into_iter()
            .map(|capability| self.grant_capability(capability))
            .collect())
    }

    /// Drop the capability with this id. Returns whether anything was
    /// removed.
    pub fn revoke_capability(&mut self, id: &str) -> bool {
        let before = self.capabilities.len();
        self.capabilities.retain(|c| c.id != id);
        self.capabilities.len() != before
    }

    /// Revoke on behalf of `revoker`, enforcing the same authority as
    /// [`Self::grant_as_at`].
    pub fn revoke_as_at(&mut self, revoker: &UserId, id: &str, now: u64) -> bool {
        self.may_administer_at(revoker, now) && self.revoke_capability(id)
    }

    /// Drop every capability matching `pred`. Returns the ids removed,
    /// so the caller can mirror each removal into the replicated doc.
    pub fn revoke_matching(&mut self, pred: impl Fn(&CapOverride) -> bool) -> Vec<String> {
        let mut removed = Vec::new();
        self.capabilities.retain(|c| {
            if pred(c) {
                removed.push(c.id.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    // Ownership and membership.

    /// Is `user` the owner, or a delegated co-owner (`Admin` over the
    /// room)? An ownerless workspace has no administrators, not
    /// everyone-is-an-administrator.
    pub fn may_administer_at(&self, user: &UserId, now: u64) -> bool {
        if self.owner.as_deref() == Some(user.as_str()) {
            return true;
        }
        self.allows_at(
            &self.actor_for_person_at(user, now),
            &ResourceRef::Workspace,
            Verb::Admin,
            now,
        )
    }

    /// Install `user` as owner, and record the membership capability
    /// that makes them `Admin` — `owner` survives an empty capability
    /// list, and the capability is what replicates and what `decide` reads.
    pub fn set_owner(&mut self, user: impl Into<UserId>) {
        let user = user.into();
        let owner_capability_id = format!("owner-{}", self.id.raw());
        // The owner capability has a stable id because the replicated map is
        // keyed by capability id.  Remove its previous in-memory value before
        // changing the subject: `grant_capability` deliberately merges only
        // identical subjects, and would otherwise leave the former owner with
        // a second local Admin grant until the next document rebuild.
        self.revoke_capability(&owner_capability_id);
        self.owner = Some(user.clone());
        self.grant_capability(
            CapOverride::membership(
                Subject::Principal(Principal::Person(user)),
                self.room.to_string(),
                Verb::Admin,
            )
            .with_id(owner_capability_id),
        );
    }

    /// Transfer the structural owner role.
    ///
    /// This is intentionally stricter than ordinary role administration:
    /// delegated administrators cannot transfer ownership, and the recipient
    /// must already be an administrator.  The latter makes transfer an
    /// explicit two-step ceremony (promote, then transfer) and avoids handing
    /// the workspace to a mistyped or stale identity.
    pub fn transfer_owner_as_at(
        &mut self,
        current_owner: &UserId,
        new_owner: &UserId,
        now: u64,
    ) -> bool {
        if current_owner == new_owner
            || self.owner.as_ref() != Some(current_owner)
            || self.role_of_at(new_owner, now) != Some(CapRole::Owner)
        {
            return false;
        }
        self.set_owner(new_owner.clone());
        true
    }

    /// Give up administration because this workspace joined a room it
    /// did not mint — drops both the recorded owner and every admin
    /// capability, or [`Self::may_administer_at`] would still answer yes
    /// for a room this machine doesn't own.
    pub fn surrender_governance(&mut self) -> bool {
        let had_owner = self.owner.take().is_some();
        let dropped = self.revoke_matching(|c| {
            let is_person = matches!(c.subject, Subject::Principal(Principal::Person(_)));
            // Capability vectors are already owned by one workspace. Accept
            // legacy local-id and current durable-room spellings alike so a
            // replicated membership cannot disappear on a peer whose local
            // workspace handle differs.
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            is_person && covers_room && c.verb.implies(Verb::Admin)
        });
        had_owner || !dropped.is_empty()
    }

    /// The role `user` holds. Not stored separately — it *is* the verb
    /// on their membership capability, so "in the room" and "allowed
    /// to do something" can't drift apart. `None` means not a member.
    pub fn role_of_at(&self, user: &UserId, now: u64) -> Option<CapRole> {
        if self.owner.as_deref() == Some(user.as_str()) {
            return Some(CapRole::Owner);
        }
        // A person with a signing key is named by its lowercase hex, so a
        // `Person` principal and the matching `User` key are the same member.
        let subject_is_user = |c: &CapOverride| match &c.subject {
            Subject::Principal(Principal::Person(p)) => p == user,
            Subject::User(key) => hex::encode(key.as_bytes()) == *user,
            _ => false,
        };
        let live_at = Some(now);
        let mut best: Option<Verb> = None;
        let mut blocked = false;
        for c in &self.capabilities {
            if !subject_is_user(c) || !c.is_live_at(live_at) {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            match c.effect {
                Effect::Grant => {
                    best = Some(match best {
                        Some(prev) if prev.implies(c.verb) => prev,
                        _ => c.verb,
                    });
                }
                // A room-wide Restrict below Read is how a member is
                // blocked without being erased from history.
                Effect::Restrict if !c.verb.implies(Verb::Read) => blocked = true,
                Effect::Restrict => {}
            }
        }
        if blocked {
            return Some(CapRole::Denied);
        }
        best.map(|v| match v {
            Verb::Admin => CapRole::Owner,
            Verb::Write => CapRole::Editor,
            Verb::Comment => CapRole::Commenter,
            Verb::Read | Verb::Invite => CapRole::Viewer,
        })
    }

    /// Build the [`Actor`] for a person acting in this room.
    pub fn actor_for_person_at(&self, user: &UserId, now: u64) -> Actor {
        Actor::person(self.role_of_at(user, now), user.clone()).in_room(self.room.to_string())
    }

    /// Build the [`Actor`] for an app running in this room. Carries the
    /// room as principal but **no role** — an app has no base authority,
    /// only what's granted to it by name.
    pub fn actor_for_app(&self, app_id: &str) -> Actor {
        Actor::app(app_id).in_room(self.room.to_string())
    }

    /// Build the [`Actor`] for an agent acting for the user. Same
    /// authority shape as an app: scoped, revocable, no role.
    pub fn actor_for_agent(&self, agent_id: &str) -> Actor {
        Actor::agent(agent_id).in_room(self.room.to_string())
    }

    /// The room roster: the people who are *members*. Built from
    /// room-scoped capabilities only — someone with a capability on one
    /// file and nothing else is **not** here. See [`Self::shares_of_at`].
    pub fn roster_at(&self, now: u64) -> Vec<(UserId, CapRole)> {
        let live_at = Some(now);
        let mut seen: Vec<UserId> = Vec::new();
        for c in &self.capabilities {
            if !c.is_live_at(live_at) || c.effect != Effect::Grant {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            if let Subject::Principal(Principal::Person(p)) = &c.subject
                && !seen.contains(p)
            {
                seen.push(p.clone());
            }
        }
        if let Some(owner) = &self.owner
            && !seen.contains(owner)
        {
            seen.insert(0, owner.clone());
        }
        seen.into_iter()
            .filter_map(|u| self.role_of_at(&u, now).map(|r| (u, r)))
            .collect()
    }

    /// The role an *agent* holds — same derivation as [`Self::role_of_at`]
    /// minus the owner short-circuit, since an agent is never the owner.
    pub fn role_of_agent_at(&self, agent: &str, now: u64) -> Option<CapRole> {
        let live_at = Some(now);
        let mut best: Option<Verb> = None;
        let mut blocked = false;
        for c in &self.capabilities {
            let subject_matches = matches!(
                &c.subject,
                Subject::Principal(Principal::Agent(a)) if a == agent
            );
            if !subject_matches || !c.is_live_at(live_at) {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            match c.effect {
                Effect::Grant => {
                    best = Some(match best {
                        Some(prev) if prev.implies(c.verb) => prev,
                        _ => c.verb,
                    });
                }
                Effect::Restrict if !c.verb.implies(Verb::Read) => blocked = true,
                Effect::Restrict => {}
            }
        }
        if blocked {
            return Some(CapRole::Denied);
        }
        best.map(|v| match v {
            Verb::Admin => CapRole::Owner,
            Verb::Write => CapRole::Editor,
            Verb::Comment => CapRole::Commenter,
            Verb::Read | Verb::Invite => CapRole::Viewer,
        })
    }

    /// The agents that hold membership in this room — a *second* list
    /// from [`Self::roster_at`], since an agent is not a person and
    /// rendering them undifferentiated would misrepresent who's present.
    pub fn agent_members_at(&self, now: u64) -> Vec<(String, CapRole)> {
        let live_at = Some(now);
        let mut seen: Vec<String> = Vec::new();
        for c in &self.capabilities {
            if !c.is_live_at(live_at) || c.effect != Effect::Grant {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            if let Subject::Principal(Principal::Agent(a)) = &c.subject
                && !seen.contains(a)
            {
                seen.push(a.clone());
            }
        }
        seen.into_iter()
            .filter_map(|a| self.role_of_agent_at(&a, now).map(|r| (a, r)))
            .collect()
    }

    /// The share list for one resource. Room-scoped capabilities are
    /// excluded (those belong in [`Self::roster_at`]) so the UI can never
    /// present "shared one file with Bob" as "Bob is a collaborator".
    pub fn shares_of_at(&self, resource: &ResourceRef, now: u64) -> Vec<&CapOverride> {
        let live_at = Some(now);
        self.capabilities
            .iter()
            .filter(|c| {
                c.is_live_at(live_at)
                    && c.effect == Effect::Grant
                    && c.subject_is_person()
                    && !c.is_room_membership()
                    && c.scope.matches_resource(resource)
            })
            .collect()
    }

    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            // A workspace that exists is a room that exists: minted
            // here rather than lazily.
            room: RoomId::new(),
            name: name.into(),
            strips: Vec::new(),
            collaborators: Vec::new(),
            owner: None,
            pending_ownership_transfer: None,
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            capabilities: Vec::new(),
            permission_profiles: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            schedule_claims: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
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
        Self {
            id: WorkspaceId::default(),
            room: RoomId::new(),
            name: String::new(),
            strips: Vec::new(),
            collaborators: Vec::new(),
            owner: None,
            pending_ownership_transfer: None,
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            capabilities: Vec::new(),
            permission_profiles: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            schedule_claims: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
        }
    }
}

/// A fresh capability id. Random rather than drawn from the window id
/// space: capability ids key the replicated capability map, so they must
/// not collide across peers, and nothing orders or parses them.
fn mint_capability_id() -> String {
    format!("cap-{}", uuid::Uuid::new_v4().simple())
}
