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
    fn read_progress(
        &self,
        context: &tilo_action_broker::services::NativeDocumentReadContext,
        invocation: uuid::Uuid,
        request_row: &str,
    ) -> Result<Option<runtime_collab::AgentMessagePublicProgress>, String> {
        let route = context.route();
        if route.app_id != "so.tilo.chat"
            || !route.record_id.is_empty()
            || context.agent_id().is_some()
        {
            return Err("public Agent Message progress requires canonical Chat Read".into());
        }
        context.revalidate()?;
        let owner = self
            .owner
            .as_ref()
            .ok_or("native Message owner unavailable")?;
        let room =
            tilo_desktop_model::RoomId::parse(&route.cluster_id).ok_or("invalid Message room")?;
        if let Some(network) = self.network.as_ref() {
            network.check_native_custody(owner, room)?;
            if network.governed_elsewhere(room) {
                return Err("public Message progress requires the governing native owner".into());
            }
        }
        let result = owner.agent_message_public_progress(
            room,
            context.certificate(),
            invocation,
            request_row,
            std::time::SystemTime::now(),
        )?;
        context.revalidate()?;
        // Both vocabularies are closed and sanitized. Conversion does not
        // allocate/inspect private model data; parity is covered below.
        result
            .map(|progress| {
                let bytes = serde_json::to_vec(&progress)
                    .map_err(|_| "public Message progress encoding failed")?;
                if bytes.len() > runtime_collab::MAX_AGENT_MESSAGE_PROGRESS_BYTES {
                    return Err("public Message progress too large".into());
                }
                serde_json::from_slice(&bytes)
                    .map_err(|_| "public Message progress contract mismatch".into())
            })
            .transpose()
    }

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

#[cfg(test)]
mod progress_contract_tests {
    #[test]
    fn public_owner_wire_parity_contains_only_phase_generation_and_reply_row() {
        use tilo_actions::agent_message::{
            AgentMessagePublicPhase as Phase, AgentMessagePublicProgress,
            AgentMessagePublicReason as Reason,
        };
        let phases = [
            Phase::WaitingForReply,
            Phase::Publishing,
            Phase::NeedsAttention(Reason::MembershipChanged),
            Phase::NeedsAttention(Reason::AuthorRevoked),
            Phase::NeedsAttention(Reason::HostChanged),
            Phase::NeedsAttention(Reason::ExecutionRetired),
            Phase::NeedsAttention(Reason::RequestChanged),
            Phase::NeedsAttention(Reason::DeliveryUnavailable),
            Phase::NeedsAttention(Reason::Closed),
            Phase::Committed {
                reply_row: "m_00000000000000000000000000000001".into(),
            },
        ];
        for phase in phases {
            let owner = AgentMessagePublicProgress {
                generation: 7,
                phase,
            };
            let json = serde_json::to_vec(&owner).unwrap();
            assert!(json.len() <= runtime_collab::MAX_AGENT_MESSAGE_PROGRESS_BYTES);
            let wire: runtime_collab::AgentMessagePublicProgress =
                serde_json::from_slice(&json).unwrap();
            runtime_collab::validate_agent_message_public_progress(&wire).unwrap();
            assert_eq!(serde_json::to_vec(&wire).unwrap(), json);
            let object: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert_eq!(object.as_object().unwrap().len(), 2);
            assert!(object.get("generation").is_some());
            assert!(object.get("phase").is_some());
        }
    }
}
