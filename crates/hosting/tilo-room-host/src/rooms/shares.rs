//! A member sharing one record, or one item of it, with a room this relay
//! governs ([`tilo_action_policy::share`]).
//!
//! A share is grants on the shared resource's node. A member's device
//! writes its own shares into the room; a page, which holds no copy of the
//! room, asks this host through its `/rooms` requests (`super::pages`), and
//! the host writes the same grants as the room's governor, decided as every
//! receiver decides a device's share ([`tilo_peer_state::room::share`]): it
//! gives only what the sharer holds on that node, and only they (or whoever
//! may govern `revoke`) end it.

use tilo_action_policy::share::ShareAccess;
use tilo_action_policy::{Context, TreePath};
use tilo_peer_state::join_ledger::request_digest;
use tilo_peer_state::room::share;
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, Rooms, system_time};

/// A request id of its own for each change: a share ended and made again
/// is a new change, which a repeated id would answer as done.
fn fresh_request() -> Result<[u8; 32], CollaborationError> {
    let mut id = [0; 32];
    getrandom::fill(&mut id).map_err(|error| CollaborationError::new(error.to_string()))?;
    Ok(id)
}

impl Rooms {
    /// Share `node` of `room` with its members as `access` gives, for
    /// `device`'s person, under the share id `share`. A repeat of the same
    /// share is no new change. Answers how many grants the share holds.
    ///
    /// # Errors
    /// A malformed share, a device that is no current member's, a member
    /// held off the room, a share above the sharer's own holdings, or a
    /// change that could not commit.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked from the decision through the committed change"
    )]
    pub async fn share_resource(
        &self,
        room: &str,
        device: &DeviceCert,
        share: &str,
        node: &TreePath,
        access: ShareAccess,
    ) -> Result<u32, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let at = system_time(now);
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        let member = governed.peer().permissions(device.device_public_key, at)?;
        let cx = Context::at_time(at);
        let bound = tilo_action_policy::roles::foreign_device_bound();
        let device_key = hex::encode(device.device_public_key);
        let grants = share::share_grants(
            (governed.peer().admitted_room(), &member.policy),
            (&member.person, &device_key, &member.authority(&bound), &cx),
            share,
            node,
            access,
        )
        .map_err(CollaborationError::new)?;
        let count = u32::try_from(grants.len()).unwrap_or(u32::MAX);
        if grants
            .iter()
            .all(|grant| member.policy.grants.contains(grant))
        {
            return Ok(count);
        }
        let digest = request_digest(&("resource.share", &grants))?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut governed,
            fresh_request()?,
            digest,
            |peer| peer.governance_mut().put_governed_grants(&grants, &key, at),
            now,
        )
        .await?;
        Ok(count)
    }

    /// End `device`'s person's share `share` in `room`: revoke its grants
    /// they gave, or any of its grants for a person who may govern
    /// `revoke`. Answers how many were revoked; none is no error.
    ///
    /// # Errors
    /// A device that is no current member's, or a change that could not
    /// commit.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked from the decision through the committed change"
    )]
    pub async fn unshare_resource(
        &self,
        room: &str,
        device: &DeviceCert,
        share: &str,
    ) -> Result<u32, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let at = system_time(now);
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        let member = governed.peer().permissions(device.device_public_key, at)?;
        let cx = Context::at_time(at);
        let bound = tilo_action_policy::roles::foreign_device_bound();
        let ids = share::ended_by(
            &member.policy,
            (&member.person, &member.authority(&bound), &cx),
            share,
        );
        if ids.is_empty() {
            return Ok(0);
        }
        let count = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        let digest = request_digest(&("resource.unshare", &ids))?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut governed,
            fresh_request()?,
            digest,
            |peer| peer.governance_mut().remove_governed_grants(&ids, &key, at),
            now,
        )
        .await?;
        Ok(count)
    }
}
