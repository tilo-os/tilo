//! Host-private signing at the actual native owner installation door.
use super::NativeOwnerCommitService;
use ed25519_dalek::Signer as _;
use tilo_action_broker::services::{AgentMessagePublisherInstallation, WorkspaceMessages};

impl WorkspaceMessages for NativeOwnerCommitService {
    fn requires_room(&self, room: &str) -> bool {
        tilo_peer_state::owner_commit::OwnerCommitService::requires_room(self, room)
    }

    fn install_publisher(
        &self,
        request: &AgentMessagePublisherInstallation,
    ) -> Result<tilo_joined_room::AgentMessagePublisherLease, String> {
        let owner = self
            .owner
            .as_ref()
            .ok_or("native Message owner unavailable")?;
        let data = request.data();
        let room = data.room;
        if let Some(network) = self.network.as_ref() {
            network.check_native_custody(owner, room)?;
            if network.governed_elsewhere(room) {
                return Err("Message publisher requires the governing native owner".into());
            }
        }
        let (key, attestor) = tilo_identity_host::broker::trusted_current_device(&room.to_string())
            .map_err(|_| "native Message publisher signing custody unavailable")?;
        let mut registration = tilo_joined_room::AgentMessagePublisherRegistration {
            data: data.clone(),
            attestor,
            signature: Vec::new(),
        };
        registration.signature = key.sign(&registration.signing_bytes()?).to_bytes().to_vec();
        owner.register_agent_message_publisher(&registration, std::time::SystemTime::now())
    }
}
