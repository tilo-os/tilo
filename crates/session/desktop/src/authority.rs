//! The model's authority questions answered at this machine's clock.
//!
//! [`Workspace`] takes `now` explicitly so the model stays clock-free; every
//! host asks through this one trait instead of reading the time itself.

use std::collections::HashMap;

use collab_types::caps::{Access, Actor, CapDecision, CapOverride, ResourceRef};
use collab_types::role::Role as CapRole;
use tilo_action_policy::{ActionKind, Answer, Grant, KindSet, TreePath, roles};
use tilo_clock::unix_now;
use tilo_desktop_model::{AppId, Capability, UserId, Workspace, WorkspaceDefaultError};

/// The model's authority questions answered at this machine's clock.
pub trait WorkspaceAuthority {
    /// See [`Workspace::set_default_app_policy_as_at`].
    ///
    /// # Errors
    ///
    /// As [`Workspace::set_default_app_policy_as_at`].
    fn set_default_app_policy_as(
        &mut self,
        actor: &UserId,
        resource: desktop_policy::ResourceKind,
        policy: desktop_policy::WorkspaceDefaultPolicy,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
    ) -> Result<(), WorkspaceDefaultError>;
    fn app_capabilities(&self, app_id: &str) -> Vec<&CapOverride>;
    fn app_grants_snapshot(&self) -> HashMap<AppId, Vec<Capability>>;
    fn has_app_grants(&self) -> bool;
    fn app_grant_ids(&self, app_id: &str) -> Vec<String>;
    /// Whether `actor` reaches the sandboxed `resource` for `access`.
    fn decide(&self, actor: &Actor, resource: &ResourceRef, access: Access) -> CapDecision;
    fn allows(&self, actor: &Actor, resource: &ResourceRef, access: Access) -> bool;
    /// Record the sandbox capability `cap`, as `grantor`, who must be
    /// allowed to grant in the room. The capability's id.
    fn grant_as(&mut self, grantor: &UserId, cap: CapOverride) -> Option<String>;
    /// Drop the sandbox capability or grant with id `id`, as `revoker`, who
    /// must be allowed to revoke in the room.
    fn revoke_as(&mut self, revoker: &UserId, id: &str) -> bool;
    /// Whether `user` may grant in the room.
    fn may_administer(&self, user: &UserId) -> bool;
    /// Whether `user` is allowed, without asking first, an action of `kind`
    /// at `relative`, a path below the room.
    fn person_allowed(&self, user: &str, relative: &TreePath, kind: ActionKind) -> bool;
    fn transfer_owner_as(&mut self, current_owner: &UserId, new_owner: &UserId) -> bool;
    fn role_of(&self, user: &UserId) -> Option<CapRole>;
    /// The sandbox actor a person is in this room.
    fn actor_for_person(&self, user: &UserId) -> Actor;
    /// The sandbox actor an app's code is in this room.
    fn actor_for_app(&self, app_id: &str) -> Actor;
    /// The sandbox actor an agent is in this room.
    fn actor_for_agent(&self, agent: &str) -> Actor;
    fn roster(&self) -> Vec<(UserId, CapRole)>;
    /// Agents admitted to the room, with the kinds of action their room
    /// grants allow.
    fn agent_members(&self) -> Vec<(String, KindSet)>;
    /// The grants naming one person each that reach `node`.
    fn shares_of(&self, node: &TreePath) -> Vec<&Grant>;
}

impl WorkspaceAuthority for Workspace {
    fn set_default_app_policy_as(
        &mut self,
        actor: &UserId,
        resource: desktop_policy::ResourceKind,
        policy: desktop_policy::WorkspaceDefaultPolicy,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
    ) -> Result<(), WorkspaceDefaultError> {
        self.set_default_app_policy_as_at(actor, resource, policy, resolver, unix_now())
    }
    fn app_capabilities(&self, app_id: &str) -> Vec<&CapOverride> {
        self.app_capabilities_at(app_id, unix_now())
    }
    fn app_grants_snapshot(&self) -> HashMap<AppId, Vec<Capability>> {
        self.app_grants_snapshot_at(unix_now())
    }
    fn has_app_grants(&self) -> bool {
        self.has_app_grants_at(unix_now())
    }
    fn app_grant_ids(&self, app_id: &str) -> Vec<String> {
        self.app_grant_ids_at(app_id, unix_now())
    }
    fn decide(&self, actor: &Actor, resource: &ResourceRef, access: Access) -> CapDecision {
        self.sandbox_decide_at(actor, resource, access, unix_now())
    }
    fn allows(&self, actor: &Actor, resource: &ResourceRef, access: Access) -> bool {
        self.decide(actor, resource, access) == CapDecision::Allow
    }
    fn grant_as(&mut self, grantor: &UserId, cap: CapOverride) -> Option<String> {
        self.grant_sandbox_as_at(grantor, cap, unix_now())
            .ok()?
            .sandbox_granted
            .pop()
    }
    fn revoke_as(&mut self, revoker: &UserId, id: &str) -> bool {
        !crate::workspace_settings::apply_revoke(self, revoker, id).is_empty()
    }
    fn may_administer(&self, user: &UserId) -> bool {
        self.may_govern_at(user, roles::governance::GRANT, unix_now())
    }
    fn person_allowed(&self, user: &str, relative: &TreePath, kind: ActionKind) -> bool {
        self.answer_at(user, &self.node().join(relative), kind, unix_now()) == Answer::Allow
    }
    fn transfer_owner_as(&mut self, current_owner: &UserId, new_owner: &UserId) -> bool {
        self.transfer_owner_as_at(current_owner, new_owner, unix_now())
    }
    fn role_of(&self, user: &UserId) -> Option<CapRole> {
        Workspace::role_of(self, user).filter(|_| !self.is_blocked(user))
    }
    fn actor_for_person(&self, user: &UserId) -> Actor {
        Actor::person(user.clone()).in_room(self.room.to_string())
    }
    fn actor_for_app(&self, app_id: &str) -> Actor {
        Actor::app(app_id).in_room(self.room.to_string())
    }
    fn actor_for_agent(&self, agent: &str) -> Actor {
        Actor::agent(agent).in_room(self.room.to_string())
    }
    fn roster(&self) -> Vec<(UserId, CapRole)> {
        Workspace::roster(self)
    }
    fn agent_members(&self) -> Vec<(String, KindSet)> {
        self.agent_members_at(unix_now())
    }
    fn shares_of(&self, node: &TreePath) -> Vec<&Grant> {
        self.shares_of_at(node, unix_now())
    }
}
