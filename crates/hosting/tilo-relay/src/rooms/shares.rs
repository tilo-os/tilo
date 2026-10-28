//! A member sharing one record, or one item of it, with a room this relay
//! governs ([`tilo_action_policy::share`]).
//!
//! A share is grants on the shared resource's node, written into the room's
//! governance by this host as it writes any governed grant, so the room
//! decides the members' reads and writes there with them, at every door and
//! on receipt. The sharer asks it themselves: it gives only what they hold
//! on that node, and only they (or whoever may govern `revoke`) end it. Two
//! doors reach the same methods: the member actions
//! (`super::actions::member`) and a page's `/rooms` requests
//! (`super::pages`).

use tilo_action_policy::share::{self, ShareAccess};
use tilo_action_policy::{ActionKind, Context, GrantId, Subject, TreePath, roles::governance};
use tilo_peer_state::join_ledger::request_digest;
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, Rooms, system_time};

/// What a member may do at a node of the room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Permits {
    pub read: bool,
    pub undoable: bool,
}

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
        let member = governed
            .peer()
            .permissions(device.device_public_key, at)
            .map_err(CollaborationError::new)?;
        let cx = Context::at_time(at);
        let person = member.person.clone();
        if member.policy.is_blocked(&person)
            || member.policy.restricted_until(&person, &cx).is_some()
        {
            return Err(CollaborationError::new(
                "you can't share in this room right now",
            ));
        }
        let grants = share::share_grants(governed.room(), &person, share, node, access)
            .map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
        let bound = tilo_action_policy::roles::foreign_device_bound();
        share::check_held(&member.authority(&bound), &grants, &cx)
            .map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
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
        let member = governed
            .peer()
            .permissions(device.device_public_key, at)
            .map_err(CollaborationError::new)?;
        let cx = Context::at_time(at);
        let bound = tilo_action_policy::roles::foreign_device_bound();
        let governs = member
            .policy
            .check_action(&member.authority(&bound), governance::REVOKE, &cx, false)
            .is_ok();
        let sharer = Subject::Person(member.person.clone());
        let ids: Vec<GrantId> = member
            .policy
            .grants
            .iter()
            .filter(|grant| share::is_of_share(grant, share))
            .filter(|grant| governs || grant.granted_by == sharer)
            .map(|grant| grant.id.clone())
            .collect();
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

    /// What `device`'s person may do at `node` of `room`, decided as the
    /// room decides their reads and writes there. The room's spectator
    /// person writes nothing.
    ///
    /// # Errors
    /// A node of another room, or a device that is no current member's.
    pub async fn resource_permits(
        &self,
        room: &str,
        device: &DeviceCert,
        node: &TreePath,
    ) -> Result<Permits, CollaborationError> {
        let at = system_time(tilo_clock::unix_now_ns());
        let state = self.room(room).await?;
        let governed = state.lock().await;
        if node.room_id() != Some(governed.room()) {
            return Err(CollaborationError::new("the node is not in this room"));
        }
        let workspace_id = governed.workspace_id();
        let member = governed
            .peer()
            .permissions(device.device_public_key, at)
            .map_err(CollaborationError::new)?;
        drop(governed);
        let watches = self.watches_only(workspace_id, device).await;
        Ok(Permits {
            read: member.allows(node, ActionKind::Read, at),
            undoable: !watches && member.allows(node, ActionKind::Undoable, at),
        })
    }
}
