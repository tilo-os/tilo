//! Giving someone, or everyone, a role over one part of a workspace this server serves
//! ([`tilo_action_policy::Change::SetScopedRole`], `grant.set`). A change records its signer as
//! the giver, so this server signs only its own person's shares, and the windows of a device it
//! placed in the workspace: every other member shares from their own device.

use tilo_action_policy::{Change, Context, Grantee, Role, TreePath};
use tilo_peer_state::join_log::request_digest;
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, Workspaces, system_time};

/// A request id of its own for each change: a share ended and made again
/// is a new change, which a repeated id would answer as done.
fn fresh_request() -> Result<[u8; 32], CollaborationError> {
    let mut id = [0; 32];
    getrandom::fill(&mut id).map_err(|error| CollaborationError::new(error.to_string()))?;
    Ok(id)
}

impl Workspaces {
    /// Give `grantee` `role` over `scope` of `workspace`, or take this server's share of it back
    /// (`None`), as `device`'s person asks. Answers whether the workspace changed.
    ///
    /// # Errors
    /// A device that is no current member's, a share its person may not give, a share this
    /// server does not sign for them, or a change that could not commit.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from the decision through the committed change"
    )]
    pub async fn set_scoped_role(
        &self,
        workspace: &str,
        device: &DeviceCert,
        (grantee, scope): (Grantee, &TreePath),
        role: Option<Role>,
    ) -> Result<bool, CollaborationError> {
        tilo_peer_state::workspace::share::window_is_the_writers(
            &hex::encode(device.device_public_key),
            scope,
        )
        .map_err(CollaborationError::new)?;
        // A device this server placed in the workspace holds none of its keys, and nothing yet
        // carries a change it signed itself: this server signs that device's own windows.
        let placed_window = tilo_desktop_model::window_share::window_of_node(scope).is_some()
            && crate::environment::environment_of(
                &self.inner.host.environments,
                &device.device_public_key,
            )
            .and_then(crate::environment::Environment::placed)
            .is_some_and(|placed| placed.workspace.to_string() == workspace);
        let now = tilo_clock::unix_now_ns();
        let cx = Context::at_time(system_time(now));
        let state = self.workspace(workspace).await?;
        let mut hosted = state.lock().await;
        let member = hosted
            .peer()
            .permissions(device.device_public_key, system_time(now))?;
        let person = member
            .person_id()
            .ok_or_else(|| CollaborationError::new("the caller is no person's device"))?;
        let change = Change::SetScopedRole {
            grantee,
            scope: scope.clone(),
            role,
        };
        let refused =
            |refusal: tilo_action_policy::Refusal| CollaborationError::new(refusal.to_string());
        if role.is_some() {
            (*member.policy)
                .clone()
                .apply(person, &member.authority(), change.clone(), &cx, false)
                .map_err(refused)?;
        }
        let host = hex::encode(self.inner.host.cert.user_public_key);
        if person != host && !placed_window {
            return Err(CollaborationError::new(
                "a member shares from their own device, which signs the share as theirs",
            ));
        }
        let mut after = (*member.policy).clone();
        after
            .apply(
                &host,
                &member.policy.person_on_device(&host, false),
                change.clone(),
                &cx,
                false,
            )
            .map_err(refused)?;
        if after == *member.policy {
            return Ok(false);
        }
        let digest = request_digest(&("resource.share", &change))?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut hosted,
            fresh_request()?,
            digest,
            |peer| {
                peer.governance_mut()
                    .change_member_list(change, &key, system_time(now))
            },
            now,
        )
        .await?;
        Ok(true)
    }

    /// Let `grantee` read `node` of `workspace` (`reads`) or keep them from it, as
    /// `device`'s person asks: who belongs to a resource is who reads it, and this is the one
    /// change that moves them ([`tilo_action_policy::reading`]). Answers whether the
    /// workspace's grants changed; a person already standing as asked changes nothing.
    ///
    /// # Errors
    /// A device that is no current member's, a person the workspace does not let grant (or
    /// revoke), a grant above their own reach, or a change that could not commit.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from the decision through the committed change"
    )]
    pub async fn set_resource_reading(
        &self,
        workspace: &str,
        device: &DeviceCert,
        node: &TreePath,
        grantee: tilo_action_policy::Grantee,
        reads: bool,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let at = system_time(now);
        let state = self.workspace(workspace).await?;
        let mut hosted = state.lock().await;
        let member = hosted.peer().permissions(device.device_public_key, at)?;
        let cx = Context::at_time(at);
        let person = member
            .person_id()
            .ok_or_else(|| CollaborationError::new("the caller is no person's device"))?;
        let change = member
            .policy
            .reading_change(
                &member.authority(),
                grantee,
                node,
                reads,
                tilo_action_policy::Grantee::person(person),
                &cx,
            )
            .map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
        let Some(change) = change else {
            return Ok(false);
        };
        let digest = request_digest(&("resource.reading", &change))?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut hosted,
            fresh_request()?,
            digest,
            |peer| match change {
                tilo_action_policy::Change::Grant(grant) => {
                    peer.governance_mut().put_member_grants(&[grant], &key, at)
                }
                tilo_action_policy::Change::Revoke(id) => {
                    peer.governance_mut().remove_member_grants(&[id], &key, at)
                }
                _ => Ok(false),
            },
            now,
        )
        .await?;
        Ok(true)
    }
}
