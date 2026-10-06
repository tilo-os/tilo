//! Host-private signing at the actual native owner installation door.
use super::NativeOwnerCommitService;
use ed25519_dalek::Signer as _;
use std::sync::Arc;
use tilo_action_broker::services::{
    AgentMessagePublisherInstallation, NativeMessageHostAttestations, NativeMessagePublication,
    WorkspaceMessages,
};

fn original_attestations(
    request: &NativeMessagePublication,
) -> Result<Arc<NativeMessageHostAttestations>, String> {
    if let Some(proofs) = request.original_host_attestations()? {
        return Ok(proofs);
    }
    let (key, attestor) =
        tilo_identity_host::broker::trusted_current_device(&request.request().room)
            .map_err(|_| "native Message signing custody unavailable")?;
    let mut proofs = Vec::with_capacity(request.admissions().len());
    for data in request.admissions() {
        let mut proof = tilo_joined_room::AgentMessageAdmissionAttestation {
            data: data.clone(),
            attestor: attestor.clone(),
            signature: Vec::new(),
        };
        proof.signature = key.sign(&proof.signing_bytes()?).to_bytes().to_vec();
        proofs.push(proof);
    }
    request.retain_original_host_attestations(proofs)
}

impl WorkspaceMessages for NativeOwnerCommitService {
    fn open_agent_execution(
        &self,
        process: launcher::managed::VerifiedManagedProcess,
    ) -> Result<std::sync::Arc<dyn tilo_action_broker::services::NativeAgentMessageSession>, String>
    {
        super::agent_message_execution::install(self, process)
    }

    fn commit_message(
        &self,
        request: Arc<NativeMessagePublication>,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let owner = owner.ok_or("native Message owner unavailable")?;
            let room = tilo_desktop_model::RoomId::parse(&request.request().room)
                .ok_or("invalid Message room")?;
            if let Some(network) = network {
                network.check_native_custody(&owner, room)?;
                if network.governed_elsewhere(room) {
                    return Err("Message admission requires the governing native owner".into());
                }
            }
            let attestations = original_attestations(&request)?;
            let receipt = owner.commit_agent_messages(
                request.request().clone(),
                attestations.proofs(),
                request.publisher(),
                std::time::SystemTime::now(),
            )?;
            receipt.validate_for(request.request())?;
            Ok(receipt)
        })
    }
    fn reconcile_message(
        &self,
        request: Arc<NativeMessagePublication>,
    ) -> tilo_action_broker::services::NativeMessageReconcileFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let owner = owner.ok_or("native Message owner unavailable")?;
            let room = tilo_desktop_model::RoomId::parse(&request.request().room)
                .ok_or("invalid Message room")?;
            if let Some(network) = network {
                network.check_native_custody(&owner, room)?;
            }
            // Observation uses the original proof, never a fresh signature.
            let proofs = request
                .original_host_attestations()?
                .ok_or("native Message has no retained original host proof")?;
            owner.reconcile_agent_message_publication(
                request.request(),
                proofs.proofs(),
                request.publisher(),
                std::time::SystemTime::now(),
            )
        })
    }
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
