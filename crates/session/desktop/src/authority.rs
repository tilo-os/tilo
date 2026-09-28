//! The model's authority questions answered at this machine's clock.
//!
//! [`Workspace`] takes `now` explicitly so the model stays clock-free; every
//! host asks through this one trait instead of reading the time itself.

use std::collections::HashMap;

use collab_types::PermissionProfile;
use collab_types::caps::{Actor, CapDecision, CapOverride, Expiry, ResourceRef, Subject, Verb};
use collab_types::role::Role as CapRole;
use tilo_clock::unix_now;
use tilo_desktop_model::{
    AppId, Capability, PermissionProfileMutationError, UserId, Workspace, WorkspaceDefaultError,
};

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
    fn decide(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> CapDecision;
    fn allows(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> bool;
    fn grant_as(&mut self, grantor: &UserId, cap: CapOverride) -> Option<String>;
    /// See [`Workspace::save_permission_profile_as_at`].
    ///
    /// # Errors
    ///
    /// As [`Workspace::save_permission_profile_as_at`].
    fn save_permission_profile_as(
        &mut self,
        actor: &UserId,
        profile: PermissionProfile,
    ) -> Result<(), PermissionProfileMutationError>;
    /// See [`Workspace::save_permission_profile_as_actor_at`].
    ///
    /// # Errors
    ///
    /// As [`Workspace::save_permission_profile_as_actor_at`].
    fn save_permission_profile_as_actor(
        &mut self,
        actor: &Actor,
        profile: PermissionProfile,
    ) -> Result<(), PermissionProfileMutationError>;
    /// See [`Workspace::assign_permission_profile_as_at`].
    ///
    /// # Errors
    ///
    /// As [`Workspace::assign_permission_profile_as_at`].
    fn assign_permission_profile_as(
        &mut self,
        actor: &UserId,
        profile_name: &str,
        subject: &Subject,
        expiry: Expiry,
    ) -> Result<Vec<String>, PermissionProfileMutationError>;
    /// See [`Workspace::assign_permission_profile_as_actor_at`].
    ///
    /// # Errors
    ///
    /// As [`Workspace::assign_permission_profile_as_actor_at`].
    fn assign_permission_profile_as_actor(
        &mut self,
        actor: &Actor,
        profile_name: &str,
        subject: &Subject,
        expiry: Expiry,
    ) -> Result<Vec<String>, PermissionProfileMutationError>;
    fn revoke_as(&mut self, revoker: &UserId, id: &str) -> bool;
    fn may_administer(&self, user: &UserId) -> bool;
    fn transfer_owner_as(&mut self, current_owner: &UserId, new_owner: &UserId) -> bool;
    fn role_of(&self, user: &UserId) -> Option<CapRole>;
    fn actor_for_person(&self, user: &UserId) -> Actor;
    fn roster(&self) -> Vec<(UserId, CapRole)>;
    fn role_of_agent(&self, agent: &str) -> Option<CapRole>;
    fn agent_members(&self) -> Vec<(String, CapRole)>;
    fn shares_of(&self, resource: &ResourceRef) -> Vec<&CapOverride>;
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
    fn decide(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> CapDecision {
        self.decide_at(actor, resource, verb, unix_now())
    }
    fn allows(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> bool {
        self.allows_at(actor, resource, verb, unix_now())
    }
    fn grant_as(&mut self, grantor: &UserId, cap: CapOverride) -> Option<String> {
        self.grant_as_at(grantor, cap, unix_now())
    }
    fn save_permission_profile_as(
        &mut self,
        actor: &UserId,
        profile: PermissionProfile,
    ) -> Result<(), PermissionProfileMutationError> {
        self.save_permission_profile_as_at(actor, profile, unix_now())
    }
    fn save_permission_profile_as_actor(
        &mut self,
        actor: &Actor,
        profile: PermissionProfile,
    ) -> Result<(), PermissionProfileMutationError> {
        self.save_permission_profile_as_actor_at(actor, profile, unix_now())
    }
    fn assign_permission_profile_as(
        &mut self,
        actor: &UserId,
        profile_name: &str,
        subject: &Subject,
        expiry: Expiry,
    ) -> Result<Vec<String>, PermissionProfileMutationError> {
        self.assign_permission_profile_as_at(actor, profile_name, subject, expiry, unix_now())
    }
    fn assign_permission_profile_as_actor(
        &mut self,
        actor: &Actor,
        profile_name: &str,
        subject: &Subject,
        expiry: Expiry,
    ) -> Result<Vec<String>, PermissionProfileMutationError> {
        self.assign_permission_profile_as_actor_at(actor, profile_name, subject, expiry, unix_now())
    }
    fn revoke_as(&mut self, revoker: &UserId, id: &str) -> bool {
        self.revoke_as_at(revoker, id, unix_now())
    }
    fn may_administer(&self, user: &UserId) -> bool {
        self.may_administer_at(user, unix_now())
    }
    fn transfer_owner_as(&mut self, current_owner: &UserId, new_owner: &UserId) -> bool {
        self.transfer_owner_as_at(current_owner, new_owner, unix_now())
    }
    fn role_of(&self, user: &UserId) -> Option<CapRole> {
        self.role_of_at(user, unix_now())
    }
    fn actor_for_person(&self, user: &UserId) -> Actor {
        self.actor_for_person_at(user, unix_now())
    }
    fn roster(&self) -> Vec<(UserId, CapRole)> {
        self.roster_at(unix_now())
    }
    fn role_of_agent(&self, agent: &str) -> Option<CapRole> {
        self.role_of_agent_at(agent, unix_now())
    }
    fn agent_members(&self) -> Vec<(String, CapRole)> {
        self.agent_members_at(unix_now())
    }
    fn shares_of(&self, resource: &ResourceRef) -> Vec<&CapOverride> {
        self.shares_of_at(resource, unix_now())
    }
}
