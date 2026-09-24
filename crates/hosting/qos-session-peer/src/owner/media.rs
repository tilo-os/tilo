//! Owner-key governed call actions and the publication invariant.
use super::*;
use qos_action_broker::collaboration_admin::{
    CollaborationAdminError as Error, CollaborationAdminOperation as Operation,
};
use qos_peer_state::room::media::MediaCallResultV1;

pub(super) fn prepare_media_governance(
    candidate: &mut StatePeer,
    key: &ed25519_dalek::SigningKey,
    now: SystemTime,
) -> Result<()> {
    crate::media_governance::prepare_media_governance(candidate, key, now)
}
impl SessionOwner {
    pub(crate) fn supports_media_governance(&self) -> bool {
        self.schema_read_only_reason().is_none()
            && self.peer.admission_v5_selected()
            && self.delivery_ledger().is_ok()
            && self.durability_healthy()
    }
    pub(super) fn prepare_media_governance_for_publication(
        &self,
        candidate: &mut StatePeer,
    ) -> Result<()> {
        prepare_media_governance(candidate, &self.identity.native, publication_time())
    }
    pub(super) fn validate_media_governance_before_commit(
        &self,
        candidate: &StatePeer,
    ) -> Result<()> {
        candidate
            .validate_media_governance_for_publication()
            .map_err(anyhow::Error::msg)
    }
    pub(crate) fn prepare_media_transition(
        &self,
        operation: &Operation,
        now: SystemTime,
    ) -> Result<(StatePeer, MediaCallResultV1), Error> {
        self.require_owner_resource_scope()
            .map_err(|error| Error::Rejected(error.to_string()))?;
        if !self.supports_media_governance() {
            return Err(Error::Rejected(
                "operational native V5 media writer required".into(),
            ));
        }
        crate::media_governance::prepare_media_transition(
            self.peer.clone(),
            &self.identity.native,
            operation,
            now,
        )
    }
}
