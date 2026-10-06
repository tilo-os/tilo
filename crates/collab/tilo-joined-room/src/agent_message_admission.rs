//! Inert pinned-host attestations of original broker Message admission.
//! Only the broker's private `OriginalInvocationData` service signs these after
//! revalidating original caller/publisher/stream custody. No signing service is
//! exposed to an application or Agent. Signature verification alone is NOT the
//! final owner barrier: the actual typed mutation/row and live managed process
//! must also match inside the owner's serialized document/input publication.
use crate::{NativeCommittedView, agent_message::MessageSource};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use tilo_peer_state::owner_commit::{CommitBinding, OwnerCommitRequest};
use tilo_user_identity::DeviceCert;

const MAX_ACTION_BYTES: usize = collab_types::MAX_DOCUMENT_TRANSFER_BYTES;
const DOMAIN: &[u8] = b"tilo/native-agent-message-admission/v1\0";

/// Original published operation facts, attested by the private broker rather
/// than inferred from an app label or supplied as unsigned caller authority.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageOperationData {
    pub app: String,
    pub resource: String,
    pub action: String,
    pub contract: tilo_actions::ActionContract,
    pub foreign_restriction: bool,
}

/// Descriptive data, not a delivery capability or proof of a live process.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageAdmissionData {
    pub invocation: uuid::Uuid,
    pub caller: DeviceCert,
    /// The exact typed encoded `PostAgentMessage` action submitted by the
    /// application for this broker admission, including serialized extras.
    pub action: Vec<u8>,
    pub document: CommitBinding,
    /// Digest only: never preserve a live provider/stream/process token.
    pub publisher_digest: [u8; 32],
    pub operation: AgentMessageOperationData,
    pub executed_action: tilo_actions::ActionId,
    /// Actual immutable `ActionEnvelope` actor, never inferred from `ActionId`.
    pub executed_actor: Vec<u8>,
    /// Strictly sorted exact newly committed range of this executed action.
    pub changes: Vec<[u8; 32]>,
}

/// Untrusted data until checked against current native owner authority.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageAdmissionAttestation {
    pub data: AgentMessageAdmissionData,
    pub attestor: DeviceCert,
    pub signature: Vec<u8>,
}

impl std::fmt::Debug for AgentMessageAdmissionAttestation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AgentMessageAdmissionAttestation(..)")
    }
}

impl AgentMessageAdmissionAttestation {
    /// Stable signing input for the private pinned-host broker service. This
    /// computes bytes only; it cannot sign, authorize, admit or deliver input.
    ///
    /// # Errors
    /// Invalid/oversized correlation or an encoding failure.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        if self.data.invocation.is_nil()
            || self.data.action.is_empty()
            || self.data.executed_actor.is_empty()
            || self
                .data
                .action
                .len()
                .checked_add(self.data.executed_actor.len())
                .is_none_or(|bytes| bytes > MAX_ACTION_BYTES)
            || self.data.publisher_digest == [0; 32]
            || self.data.executed_action.0.is_nil()
            || self.data.changes.is_empty()
            || self.data.changes.len() > 4096
            || self.data.changes.contains(&[0; 32])
            || self
                .data
                .changes
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
            || self.data.operation.app != "so.tilo.chat"
            || [&self.data.operation.resource, &self.data.operation.action]
                .iter()
                .any(|value| {
                    value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
                })
        {
            return Err("invalid Agent Message admission attestation data".into());
        }
        let encoded = postcard::to_allocvec(&(&self.data, &self.attestor))
            .map_err(|_| "Agent Message attestation encoding failed")?;
        let mut hash = blake3::Hasher::new();
        hash.update(DOMAIN);
        hash.update(&encoded);
        Ok(hash.finalize().as_bytes().to_vec())
    }

    pub(crate) fn verify(
        &self,
        view: &NativeCommittedView,
        request: &OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<MessageSource, String> {
        let pinned = view.host().unwrap_or_else(|| view.governor());
        if self.attestor != *pinned
            || self.attestor != *view.device()
            || (view.host().is_some() && !self.attestor.capabilities.can_act_as_host)
            || !view.peer().certificates().contains(&self.attestor)
            || !view.peer().certificates().contains(&self.data.caller)
        {
            return Err(
                "Agent Message admission lacks exact current pinned host/person evidence".into(),
            );
        }
        view.peer()
            .permissions(self.attestor.device_public_key, now)?;
        view.peer()
            .permissions(self.data.caller.device_public_key, now)?;
        if self.data.document != request.binding()? {
            return Err("Agent Message admission attests a different document mutation".into());
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.attestor.device_public_key)
            .map_err(|_| "invalid Agent Message attestor key")?;
        let signature = ed25519_dalek::Signature::from_slice(&self.signature)
            .map_err(|_| "invalid Agent Message attestation signature")?;
        key.verify_strict(&self.signing_bytes()?, &signature)
            .map_err(|_| "Agent Message attestation signature mismatch")?;
        revalidate_operation(view, &self.data.caller, &self.data.operation, now)?;
        self.source()
    }

    fn source(&self) -> Result<MessageSource, String> {
        use tilo_actions::agent_message::{
            AgentIntent, AgentMessageMutation, SerializedAgentMessagePayload,
        };
        let action: AgentMessageMutation<SerializedAgentMessagePayload> =
            serde_json::from_slice(&self.data.action)
                .map_err(|_| "Agent Message admission is not the canonical typed action")?;
        let AgentMessageMutation::PostAgentMessage {
            workspace,
            agent,
            message,
        } = &action;
        if message.intent != AgentIntent::Message {
            return Err("Agent Message admission cannot deliver Queue or Steer".into());
        }
        // Do not accept unknown fields hidden outside the shared typed schema.
        // JSON object ordering is not authority; exact original bytes remain
        // included in the attestation and action digest independently.
        let mut normalized: serde_json::Value = serde_json::from_slice(&self.data.action)
            .map_err(|_| "Agent Message admission JSON is invalid")?;
        let payload = normalized
            .get_mut("PostAgentMessage")
            .and_then(|action| action.get_mut("message"))
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("Agent Message admission payload is invalid")?;
        for (field, value) in [
            ("id", serde_json::json!("")),
            ("attachment", serde_json::Value::Null),
            ("mentions", serde_json::json!([])),
            ("reference", serde_json::Value::Null),
        ] {
            payload.entry(field).or_insert(value);
        }
        if payload
            .get("body")
            .is_some_and(|body| body.as_array().is_some_and(Vec::is_empty))
        {
            payload.remove("body");
        }
        if normalized
            != serde_json::to_value(&action).map_err(|_| "Agent Message action encoding failed")?
        {
            return Err("Agent Message admission contains unrecognized typed fields".into());
        }
        let room = crate::RoomId::parse(workspace).ok_or("Agent Message action room is invalid")?;
        let source = MessageSource {
            room,
            invocation: self.data.invocation,
            request_row: format!("m_{}", self.data.invocation.simple()),
            channel: tilo_actions::agent_message::agent_conversation_channel_id(workspace, agent),
            caller: self.data.caller.clone(),
            host: self.attestor.clone(),
            agent: agent.clone(),
            payload: message.clone(),
            action: self.data.action.clone(),
            action_digest: *blake3::hash(&self.data.action).as_bytes(),
            publisher_digest: self.data.publisher_digest,
            operation: self.data.operation.clone(),
            executed_action: self.data.executed_action,
            executed_actor: self.data.executed_actor.clone(),
            changes: self.data.changes.clone(),
            document: self.data.document.clone(),
        };
        source.validate()?;
        Ok(source)
    }
}

pub(crate) fn revalidate_operation(
    view: &NativeCommittedView,
    caller: &DeviceCert,
    operation: &AgentMessageOperationData,
    now: SystemTime,
) -> Result<(), String> {
    use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
    let (role, caps, person) = view.peer().permissions(caller.device_public_key, now)?;
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "Agent Message clock precedes Unix epoch")?
        .as_secs();
    let mut actor = Actor::person(Some(role), &person).in_room(view.record.room.to_string());
    if operation.foreign_restriction {
        actor = collab_types::foreign_device::on_foreign_device(actor, &caps, seconds);
    }
    let verb = match operation.contract.permission {
        tilo_actions::PermissionRequirement::None => None,
        tilo_actions::PermissionRequirement::Read => Some(Verb::Read),
        tilo_actions::PermissionRequirement::Comment => Some(Verb::Comment),
        tilo_actions::PermissionRequirement::Write => Some(Verb::Write),
        tilo_actions::PermissionRequirement::Invite => Some(Verb::Invite),
        tilo_actions::PermissionRequirement::Admin => Some(Verb::Admin),
    };
    if verb.is_some_and(|verb| {
        decide_capability(
            &actor,
            &ResourceRef::Action {
                app: operation.app.clone(),
                action: operation.action.clone(),
            },
            verb,
            &caps,
            Some(seconds),
        ) != CapDecision::Allow
    }) {
        return Err("original Agent Message operation is no longer authorized".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application_record::tests::{Fixture, fixture};
    use ed25519_dalek::Signer as _;

    fn signed_request(fixture: &Fixture) -> OwnerCommitRequest {
        let room = crate::RoomId::parse(crate::testing::ROOM).unwrap();
        let message = fixture
            .owner
            .with_current(room, |view| {
                let mut document = automerge::AutoCommit::new();
                view.peer().clone().publish_document(
                    "so.tilo.chat",
                    "",
                    document.save(),
                    &fixture.key,
                    fixture.now,
                )
            })
            .unwrap();
        crate::publication::request(&message).unwrap()
    }

    fn attestation(
        fixture: &Fixture,
        request: &OwnerCommitRequest,
    ) -> AgentMessageAdmissionAttestation {
        let action = serde_json::to_vec(&serde_json::json!({"PostAgentMessage": {
            "workspace": crate::testing::ROOM, "agent": "scribe", "message": {
                "id": "", "ts": 7, "text": "Explicit input", "intent": "message",
                "attachment": null, "mentions": [], "reference": null
            }
        }}))
        .unwrap();
        let mut proof = AgentMessageAdmissionAttestation {
            data: AgentMessageAdmissionData {
                invocation: uuid::Uuid::from_u128(500),
                caller: fixture.certificate.clone(),
                action,
                document: request.binding().unwrap(),
                publisher_digest: [7; 32],
                operation: AgentMessageOperationData {
                    app: "so.tilo.chat".into(),
                    resource: "opaque-agent-resource".into(),
                    action: "message".into(),
                    contract: tilo_actions::ActionContract::default(),
                    foreign_restriction: false,
                },
                executed_action: tilo_actions::ActionId(uuid::Uuid::from_u128(501)),
                executed_actor: tilo_actions::ActionId(uuid::Uuid::from_u128(501))
                    .actor_bytes()
                    .to_vec(),
                changes: vec![[6; 32]],
            },
            attestor: fixture.certificate.clone(),
            signature: Vec::new(),
        };
        resign(fixture, &mut proof);
        proof
    }

    fn resign(fixture: &Fixture, proof: &mut AgentMessageAdmissionAttestation) {
        proof.signature = fixture
            .key
            .sign(&proof.signing_bytes().unwrap())
            .to_bytes()
            .to_vec();
    }

    fn verify(
        fixture: &Fixture,
        proof: &AgentMessageAdmissionAttestation,
        request: &OwnerCommitRequest,
    ) -> Result<MessageSource, String> {
        fixture.owner.with_current(
            crate::RoomId::parse(crate::testing::ROOM).unwrap(),
            |view| proof.verify(view, request, fixture.now),
        )
    }

    #[test]
    fn actual_host_signature_checks_do_not_create_an_input_without_typed_document_barrier() {
        let fixture = fixture();
        let request = signed_request(&fixture);
        let proof = attestation(&fixture, &request);
        let source = verify(&fixture, &proof, &request).unwrap();
        assert_eq!(source.payload.text, "Explicit input");
        fixture
            .owner
            .with_current(source.room, |view| {
                assert!(
                    view.record.agent_messages.is_empty(),
                    "an attestation alone must never mint input"
                );
                Ok(())
            })
            .unwrap();
        let debug = format!("{proof:?}");
        assert!(!debug.contains(&source.payload.text));
        assert!(!debug.contains(&proof.attestor.device_label));
    }

    #[test]
    fn attestation_refuses_bad_signature_changed_body_provider_or_document_envelope() {
        let fixture = fixture();
        let request = signed_request(&fixture);
        let proof = attestation(&fixture, &request);
        let mut changed = proof.clone();
        changed.signature[0] ^= 1;
        assert!(verify(&fixture, &changed, &request).is_err());
        let mut changed = proof.clone();
        let mut action: serde_json::Value = serde_json::from_slice(&changed.data.action).unwrap();
        action["PostAgentMessage"]["message"]["text"] = "Changed input".into();
        changed.data.action = serde_json::to_vec(&action).unwrap();
        assert!(verify(&fixture, &changed, &request).is_err());
        let mut changed = proof.clone();
        changed.data.publisher_digest = [9; 32];
        assert!(verify(&fixture, &changed, &request).is_err());
        let mut changed = proof.clone();
        changed.data.executed_action = tilo_actions::ActionId(uuid::Uuid::from_u128(999));
        assert!(verify(&fixture, &changed, &request).is_err());
        let mut changed = proof.clone();
        changed.data.executed_actor = b"asserted-other-writer".to_vec();
        assert!(verify(&fixture, &changed, &request).is_err());
        let mut changed_request = request;
        if let tilo_peer_state::owner_commit::CommitOperation::Document { record, .. } =
            &mut changed_request.operation
        {
            *record = "foreign".into();
        }
        assert!(verify(&fixture, &proof, &changed_request).is_err());
    }

    #[test]
    fn even_signed_admission_refuses_wrong_host_or_non_message_typed_action() {
        let fixture = fixture();
        let request = signed_request(&fixture);
        let proof = attestation(&fixture, &request);
        let mut changed = proof.clone();
        changed.attestor.device_label = "another pinned host".into();
        resign(&fixture, &mut changed);
        assert!(verify(&fixture, &changed, &request).is_err());
        let mut changed = proof;
        let mut action: serde_json::Value = serde_json::from_slice(&changed.data.action).unwrap();
        action["PostAgentMessage"]["message"]["intent"] = "queue".into();
        changed.data.action = serde_json::to_vec(&action).unwrap();
        resign(&fixture, &mut changed);
        assert!(verify(&fixture, &changed, &request).is_err());
        action["PostAgentMessage"]["message"]["intent"] = "message".into();
        action["PostAgentMessage"]["asserted_author"] = "another person".into();
        changed.data.action = serde_json::to_vec(&action).unwrap();
        resign(&fixture, &mut changed);
        assert!(verify(&fixture, &changed, &request).is_err());
    }
}
