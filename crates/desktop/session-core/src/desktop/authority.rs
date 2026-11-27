//! The model's authority questions answered at this machine's clock.
//! [`Workspace`] takes `now` explicitly so the model stays clock-free; every host asks through this
//! one trait instead of reading the time itself.

use collab_types::caps::{
    Access, CapDecision, CapOverride, ResourceRef, decide_capability, sandbox,
};
use collab_types::role::Role as CapRole;
use tilo_action_policy::{ActionKind, Answer, Grant, TreePath, roles};
use tilo_clock::unix_now;
use tilo_desktop_model::{Workspace, WorkspaceDefaultError};
use tilo_member_types::Acting;

/// The model's authority questions answered at this machine's clock.
pub trait WorkspaceAuthority {
    /// See [`Workspace::set_default_app_policy_as_at`].
    ///
    /// # Errors
    ///
    /// As [`Workspace::set_default_app_policy_as_at`].
    fn set_default_app_policy_as(
        &mut self,
        actor: &str,
        resource: desktop_policy::ResourceKind,
        policy: desktop_policy::WorkspaceDefaultPolicy,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
    ) -> Result<(), WorkspaceDefaultError>;
    fn app_capabilities(&self, app_id: &str) -> Vec<&CapOverride>;
    /// Whether `acting` reaches the sandboxed `resource` for `access` .
    fn decide(&self, acting: &Acting, resource: &ResourceRef, access: Access) -> CapDecision;
    fn allows(&self, acting: &Acting, resource: &ResourceRef, access: Access) -> bool;
    /// Record the sandbox capability `cap` , as `grantor` , who must be allowed to grant in the
    /// workspace. The capability's id.
    fn grant_as(&mut self, grantor: &str, cap: CapOverride) -> Option<String>;
    /// Drop the sandbox capability or grant with id `id` , as `revoker` , who must be allowed to
    /// revoke in the workspace.
    fn revoke_as(&mut self, revoker: &str, id: &str) -> bool;
    /// Whether `user` may grant in the workspace.
    fn may_administer(&self, user: &str) -> bool;
    /// Whether `user` is allowed, without asking first, an action of `kind` at `relative` , a path
    /// below the workspace.
    fn person_allowed(&self, user: &str, relative: &TreePath, kind: ActionKind) -> bool;
    fn role_of(&self, user: &str) -> Option<CapRole>;
    /// The acting a person is in this workspace.
    fn acting_for_person(&self, user: &str) -> Acting;
    fn roster(&self) -> Vec<(String, CapRole)>;
    /// The grants naming one person each that reach `node` .
    fn shares_of(&self, node: &TreePath) -> Vec<&Grant>;
}

impl WorkspaceAuthority for Workspace {
    fn set_default_app_policy_as(
        &mut self,
        actor: &str,
        resource: desktop_policy::ResourceKind,
        policy: desktop_policy::WorkspaceDefaultPolicy,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
    ) -> Result<(), WorkspaceDefaultError> {
        self.set_default_app_policy_as_at(actor, resource, policy, resolver, unix_now())
    }
    fn app_capabilities(&self, app_id: &str) -> Vec<&CapOverride> {
        sandbox::of_app_at(&self.sandbox, app_id, unix_now())
    }
    fn decide(&self, acting: &Acting, resource: &ResourceRef, access: Access) -> CapDecision {
        decide_capability(
            acting,
            resource,
            access,
            &self.sandbox,
            Some(unix_now().saturating_mul(1000)),
        )
    }
    fn allows(&self, acting: &Acting, resource: &ResourceRef, access: Access) -> bool {
        self.decide(acting, resource, access) == CapDecision::Allow
    }
    fn grant_as(&mut self, grantor: &str, cap: CapOverride) -> Option<String> {
        sandbox::grant_as_at(&self.policy, &mut self.sandbox, grantor, cap, unix_now())
            .ok()?
            .sandbox_granted
            .pop()
    }
    fn revoke_as(&mut self, revoker: &str, id: &str) -> bool {
        crate::desktop::workspace_settings::apply_revoke(self, revoker, id)
            .is_ok_and(|edit| !edit.is_empty())
    }
    fn may_administer(&self, user: &str) -> bool {
        self.policy
            .may_change_at(user, roles::governance::GRANT, unix_now())
    }
    fn person_allowed(&self, user: &str, relative: &TreePath, kind: ActionKind) -> bool {
        self.policy
            .answer_at(user, &self.policy.node().join(relative), kind, unix_now())
            == Answer::Allow
    }
    fn role_of(&self, user: &str) -> Option<CapRole> {
        self.policy.active_role(user)
    }
    fn acting_for_person(&self, user: &str) -> Acting {
        Acting::person(user.to_owned())
    }
    fn roster(&self) -> Vec<(String, CapRole)> {
        self.policy.people()
    }
    fn shares_of(&self, node: &TreePath) -> Vec<&Grant> {
        self.policy.shares_at(node, unix_now())
    }
}

#[cfg(test)]
mod tests {
    //! What a host asks about a person is answered for the person as they stand now: a blocked
    //! member has no role, and who may not govern neither grants nor revokes.

    use super::*;
    use collab_types::caps::Scope;
    use tilo_action_policy::Grantee;
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner-user";
    const PEER: &str = "peer-user";

    /// The owner's workspace with one member admitted as `role`, as a host installs an accepted
    /// invite.
    fn ws_with_member(role: CapRole) -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from_u128(23), "authority-test");
        ws.policy.set_owner(OWNER);
        ws.policy.add_member(PEER, role);
        ws
    }

    /// Write access to the workspace's files, for the member.
    fn files_for_the_member() -> CapOverride {
        CapOverride::grant(
            Grantee::person(PEER),
            Scope::FsPath("/var/workspace".into()),
            Access::Write,
        )
    }

    tilo_test_env::scenario! {
        fn a_host_asks_authority_for_the_person_as_they_stand_now;

        phase a_member_blocked_while_still_on_record_has_no_role;
        // A blocked member stays on record, so the model still holds the role they had. What a host
        // asks here is the role they act with, and that is none.
        {
            let mut ws = ws_with_member(CapRole::Editor);
            let peer = PEER.to_string();
            assert_eq!(
                WorkspaceAuthority::role_of(&ws, &peer),
                Some(CapRole::Editor),
                "a member has the role they were admitted with"
            );

            let block = ws.policy.block(PEER, Grantee::person(OWNER));
            ws.policy.grant(block);
            assert!(ws.policy.is_blocked(PEER), "the block is on the whole workspace");
            assert_eq!(
                ws.policy.role_of(PEER),
                Some(CapRole::Editor),
                "the record still names the member"
            );
            assert_eq!(
                WorkspaceAuthority::role_of(&ws, &peer),
                None,
                "a blocked member has no role to act with"
            );
            assert_eq!(
                WorkspaceAuthority::role_of(&ws, OWNER),
                Some(CapRole::Owner),
                "blocking a member takes nothing from the owner"
            );
        }

        phase who_may_not_govern_neither_grants_nor_revokes;
        // Granting and revoking are the workspace's to govern: a viewer and a stranger record nothing
        // and take nothing back, and nobody takes back what is not there.
        {
            let mut ws = ws_with_member(CapRole::Viewer);
            let owner = OWNER.to_string();
            for who in [PEER.to_string(), "stranger".to_string()] {
                assert!(!ws.may_administer(&who), "{who} may not grant");
                assert_eq!(
                    ws.grant_as(&who, files_for_the_member()),
                    None,
                    "{who} grants nothing"
                );
                assert!(ws.sandbox.is_empty(), "{who} recorded nothing");
            }

            assert!(ws.may_administer(&owner), "the owner may grant");
            let id = ws
                .grant_as(&owner, files_for_the_member())
                .expect("the owner grants");
            assert_eq!(ws.sandbox.len(), 1, "the owner's grant is recorded");
            for who in [PEER.to_string(), "stranger".to_string()] {
                assert!(!ws.revoke_as(&who, &id), "{who} takes nothing back");
                assert_eq!(ws.sandbox.len(), 1, "the grant stands after {who} asked");
            }
            assert!(
                !ws.revoke_as(&owner, "no-such-capability"),
                "there is nothing of that id to take back"
            );
            assert_eq!(ws.sandbox.len(), 1, "the grant stands");
            assert!(
                ws.revoke_as(&owner, &id),
                "the owner takes back what they gave"
            );
            assert!(ws.sandbox.is_empty(), "and it is gone");
        }
    }
}
