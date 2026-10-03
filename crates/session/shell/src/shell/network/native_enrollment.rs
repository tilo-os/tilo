use super::*;
use crate::{
    network::invite::enrollment_driver,
    shell::{
        native_workspace_owner::NativeWorkspaceOwner, peer_sync_overlay::PeerSyncResponse,
        state::WorkspaceId,
    },
};
#[derive(Default)]
pub(super) struct State {
    owner: Mutex<Option<NativeWorkspaceOwner>>,
    pub(super) attempts: Mutex<std::collections::HashMap<u64, enrollment_driver::Attempt>>,
}
impl ShellNetwork {
    pub(crate) fn native_owner(&self) -> Result<NativeWorkspaceOwner, String> {
        let tree = self
            .replica
            .lock()
            .map_err(|_| "native replica unavailable")?
            .tree
            .clone();
        let mut retained = self
            .native_enrollment
            .owner
            .lock()
            .map_err(|_| "native owner retention unavailable")?;
        if let Some(owner) = retained.as_ref() {
            return Ok(owner.clone());
        }
        let rooms = tree.read().rooms();
        let owner = crate::shell::native_workspace_owner::open(tree)?;
        *retained = Some(owner.clone());
        drop(retained);
        for room in rooms {
            if owner.with_current(room, |_| Ok(())).is_ok()
                && let Err(error) = self.connect_native_room(owner.clone(), room)
            {
                tracing::warn!(%error, "native owner restored while data transport is unavailable");
            }
        }
        Ok(owner)
    }
    pub(crate) fn runtime_handle(&self) -> Result<tokio::runtime::Handle, String> {
        self.runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or_else(|| "native runtime unavailable".into())
    }
    /// Redeem a peer invitation into the local workspace `local`.
    ///
    /// # Errors
    ///
    /// Returns an error if the replica, device custody or native owner is
    /// unavailable, the target workspace disappeared, or the enrollment cannot
    /// be queued.
    pub fn accept_peer_invitation(
        &self,
        local: WorkspaceId,
        token: &str,
    ) -> Result<PeerSyncResponse, String> {
        if let Ok(legacy) = crate::network::invite::validate_invite_token(token) {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "native replica unavailable")?;
            let tree = replica.tree.read();
            let target = RoomId::parse(&legacy.room_id).ok_or("invalid legacy room")?;
            let previous = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id == local)
                .ok_or("target workspace disappeared")?
                .room;
            if tree.requires_native_owner(target) || tree.requires_native_owner(previous) {
                return Err("certified native rooms require canonical enrollment".into());
            }
            drop(tree);
            drop(replica);
            return self
                .accept_invite(local, token)
                .map(|room| PeerSyncResponse::Message(room.to_string()));
        }
        let owner = self.native_owner()?;
        let room = self
            .replica
            .lock()
            .map_err(|_| "native replica unavailable")?
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == local)
            .map(|workspace| workspace.room)
            .ok_or("target workspace disappeared")?;
        let topic = room_topic_id(room);
        let certificate = self
            .host_identity
            .as_ref()
            .ok_or("native device custody unavailable")?
            .current_native_material(topic)?
            .0;
        // A lost UI reply after the local barrier is reconciled from the actual
        // committed enrollment, never by another remote redemption.
        if owner
            .with_current(room, |view| {
                Ok(view.device() == &certificate
                    && tilo_peer_state::join::encode_invitation_text(view.enrollment_invitation())
                        == token)
            })
            .unwrap_or(false)
        {
            self.connect_native_room(owner, room)?;
            return Ok(PeerSyncResponse::Message(room.to_string()));
        }
        let mut attempts = self
            .native_enrollment
            .attempts
            .lock()
            .map_err(|_| "native enrollment registry unavailable")?;
        if attempts.len() >= 8 || attempts.values().any(|attempt| attempt.local() == local) {
            return Err("native enrollment already pending or quota reached".into());
        }
        let (command, attempt) = enrollment_driver::command(owner, local, topic, token.to_owned())?;
        let id = attempt.id();
        self.cmd_tx
            .try_send(NetCmd::NativeEnrollment(command))
            .map_err(|_| "native enrollment command queue unavailable")?;
        attempts.insert(id, attempt);
        drop(attempts);
        Ok(PeerSyncResponse::EnrollmentPending { id })
    }
    /// Finish the pending enrollment `id` once its remote exchange completed.
    ///
    /// # Errors
    ///
    /// Returns an error if the attempt is no longer active, or if installing the
    /// enrolled room fails.
    pub fn poll_enrollment(&self, id: u64) -> Result<PeerSyncResponse, String> {
        let attempt = self
            .native_enrollment
            .attempts
            .lock()
            .map_err(|_| "native enrollment registry unavailable")?
            .get(&id)
            .cloned()
            .ok_or("native enrollment attempt no longer active")?;
        let result = attempt.finish(|enrollment| {
            let identity = self
                .host_identity
                .as_ref()
                .ok_or("native custody unavailable")?;
            let (certificate, _) = identity.current_native_material(attempt.identity_topic())?;
            if &certificate != enrollment.local_device() {
                return Err("selected native custody changed before installation".into());
            }
            let room =
                RoomId::parse(&enrollment.bootstrap().room).ok_or("invalid enrolled room")?;
            // Preserve the explicitly selected identity when the target slot acquires
            // its new room. This is existing custody storage, not semantic authority.
            if identity.identity_paths.is_some() {
                let registry = crate::identity_registry::IdentityRegistryStore::from_env()
                    .map_err(|e| e.to_string())?;
                let profile = hex::encode(certificate.user_public_key);
                registry
                    .bind_workspace(&room.to_string(), &profile)
                    .map_err(|e| e.to_string())?;
                if identity.current_native_material(room_topic_id(room))?.0 != certificate {
                    return Err("native room custody did not retain selected certificate".into());
                }
            }
            attempt.owner().install(&enrollment, SystemTime::now())?;
            self.check_native_custody(attempt.owner(), room)?;
            if !self.join_topic(room_topic_id(room)) {
                return Err("native room committed; transport queue unavailable".into());
            }
            self.connect_native_room(attempt.owner().clone(), room)?;
            Ok(room)
        });
        let Some(result) = result else {
            return Ok(PeerSyncResponse::EnrollmentPending { id });
        };
        self.native_enrollment
            .attempts
            .lock()
            .map_err(|_| "native enrollment registry unavailable")?
            .remove(&id);
        result.map(|room| PeerSyncResponse::EnrollmentJoined {
            id,
            room: room.to_string(),
        })
    }
    /// Cancel the pending enrollment `id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the attempt is no longer active or cannot be cancelled.
    pub fn cancel_enrollment(&self, id: u64) -> Result<PeerSyncResponse, String> {
        let attempt = self
            .native_enrollment
            .attempts
            .lock()
            .map_err(|_| "native enrollment registry unavailable")?
            .remove(&id)
            .ok_or("native enrollment attempt no longer active")?;
        attempt.cancel()?;
        Ok(PeerSyncResponse::Message(
            "Enrollment cancellation requested".into(),
        ))
    }
}
