//! Namespace registration through the existing authenticated room authority.
use super::{CollaborationError, GovernanceCompletion, Rooms, system_time};
use tilo_user_identity::DeviceCert;

impl Rooms {
    /// Durably register an already admitted writer's own topology namespace.
    /// Endpoint-bound caller evidence comes from `RoomAdminProtocol`; membership
    /// and current workspace Write are checked again under the room lock.
    ///
    /// # Errors
    /// Refuses invalid/revoked certificates, stale membership, denied Write,
    /// conflicting namespaces and failed persistence or signed publication.
    pub async fn claim_topology_namespace(
        &self,
        room: &str,
        caller: &DeviceCert,
        salt: u16,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        tilo_user_identity::verify_device_cert_at(caller, now)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if self.revocations().is_revoked(&caller.device_public_key) {
            return Err(CollaborationError::new("device revoked"));
        }
        let state = self.room(room).await?;
        let mut room = state.lock().await;
        let mut candidate = room.peer().clone();
        let changed = candidate.claim_topology_namespace(
            caller,
            salt,
            &self.inner.host.key,
            system_time(now),
        )?;
        let collaboration = room.collaboration().clone();
        let generation = if changed {
            room.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
                Ok(collaboration)
            })?
        } else {
            room.record.generation
        };
        self.settle(&mut room).await?;
        Ok(GovernanceCompletion { generation })
    }
}
