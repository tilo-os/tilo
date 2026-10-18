//! Historical MODEL delivery facts. These never confer dispatch authority.
use super::{NativeInvocationPermit, TaskAdmissionLedger, model::TypedNativeModelRequirement};
use crate::{NativeWorkspaceOwner, private_task_mailboxes::PrivateTaskMailboxStore};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_actions::task_messaging::{TaskMessage, TaskMessageStatus};
use tilo_model_gateway::native_delivery::NativeModelSuccess;

const MAIL: &str =
    "Untrusted task context. Sender and recipient below are attributed by the workspace owner,";

/// Signed by the physical host only upon protected-source HTTP success.
/// Deserialization alone is not admission: the owner checks signed dispatch
/// provenance and every full envelope under its private writer turn.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeModelDeliveryReceipt {
    requirement: TypedNativeModelRequirement,
    permit: NativeInvocationPermit,
    endpoint: String,
    messages: Vec<TaskMessage>,
    signature: [[u8; 32]; 2],
}
impl NativeModelDeliveryReceipt {
    /// Requires unforgeable evidence from the protected source, not an ACK.
    /// # Errors
    /// Refuses changed HTTP bytes, selection, invocation or host signing custody.
    pub fn capture(
        proof: &NativeModelSuccess,
        requirement: &TypedNativeModelRequirement,
        permit: &NativeInvocationPermit,
        key: &SigningKey,
    ) -> Result<Self, String> {
        if serde_json::to_value(requirement.invocation).map_err(|e| e.to_string())?
            != *proof.invocation()
            || *blake3::hash(proof.body()).as_bytes() != requirement.requestdigest
            || key.verifying_key().to_bytes() != requirement.host
            || requirement.modelselection.selection.model != proof.model()
            || requirement.modelselection.selection.contract.get("profile")
                != Some(&serde_json::to_value(proof.profile()).map_err(|e| e.to_string())?)
        {
            return Err("MODEL delivery source differs from prepared HTTP request".into());
        }
        let mut messages = Vec::new();
        for text in proof.user_texts() {
            for frame in text.split(MAIL).skip(1) {
                let Some((_, json)) = frame.split_once('\n') else {
                    continue;
                };
                // Protocol encoders may coalesce adjacent user frames. Decode
                // exactly one JSON value, without discarding following frames.
                if let Some(Ok(message)) = serde_json::Deserializer::from_str(json)
                    .into_iter::<TaskMessage>()
                    .next()
                    && message.recipient.task_id == requirement.run.task_id
                    && message.recipient.run_epoch == requirement.run.epoch
                    && message.recipient.agent_id == requirement.run.agent_id
                    && !messages
                        .iter()
                        .any(|m: &TaskMessage| m.message_id == message.message_id)
                {
                    messages.push(message);
                }
            }
        }
        let mut receipt = Self {
            requirement: requirement.clone(),
            permit: permit.clone(),
            endpoint: proof.endpoint().into(),
            messages,
            signature: [[0; 32]; 2],
        };
        let signature = super::sign("tilo-native-model-delivery-v1", &receipt.payload(), key)?;
        receipt.signature = signature;
        receipt.verify()?;
        Ok(receipt)
    }
    fn payload(&self) -> impl Serialize + '_ {
        (
            &self.requirement,
            &self.permit,
            &self.endpoint,
            &self.messages,
        )
    }
    fn verify(&self) -> Result<(), String> {
        self.permit.verify()?;
        if self.requirement.binding()? != self.permit.reservation.binding
            || self.endpoint.is_empty()
            || self.messages.len() > 4096
        {
            return Err("MODEL delivery receipt binding invalid".into());
        }
        super::verify(
            "tilo-native-model-delivery-v1",
            &self.payload(),
            self.requirement.host,
            self.signature,
        )
    }
    /// Only exact attributed owner envelopes can change delivery metadata.
    /// A successful HTTP response may contain forged context: retain the
    /// response fact without treating those purported envelopes as deliveries.
    /// # Errors
    /// Refuses an invalid signed HTTP success receipt.
    pub fn apply_to_messages(
        &self,
        messages: &mut std::collections::BTreeMap<String, TaskMessage>,
    ) -> Result<(), String> {
        self.verify()?;
        for seen in &self.messages {
            if let Some(actual) = messages.get_mut(&seen.message_id) {
                let mut expected = actual.clone();
                expected.status = seen.status;
                if expected == *seen {
                    actual.status = TaskMessageStatus::DeliveredToModel;
                }
            }
        }
        Ok(())
    }
    pub fn room(&self) -> crate::RoomId {
        self.requirement.room
    }
    pub fn host(&self) -> [u8; 32] {
        self.requirement.host
    }
}
impl TaskAdmissionLedger {
    /// Historical metadata must be backed by a retained signed success receipt.
    pub fn model_delivered_message(&self, message: &TaskMessage) -> bool {
        self.model_deliveries.values().any(|receipt| {
            receipt.messages.iter().any(|seen| {
                let mut expected = message.clone();
                expected.status = seen.status;
                expected == *seen
            })
        })
    }
    pub(super) fn validate_model_deliveries(&self, room: crate::RoomId) -> Result<(), String> {
        if self.model_deliveries.len() > super::MAX_RESERVATIONS {
            return Err("MODEL delivery capacity exceeded".into());
        }
        for (id, receipt) in &self.model_deliveries {
            receipt.verify()?;
            if receipt.room() != room || *id != receipt.requirement.invocation {
                return Err("MODEL delivery retained identity changed".into());
            }
            self.check_model_delivery(receipt)?;
        }
        Ok(())
    }
    fn check_model_delivery(&self, receipt: &NativeModelDeliveryReceipt) -> Result<(), String> {
        receipt.verify()?;
        let exact = self.dispatched_invocation(receipt.requirement.invocation, receipt.host())?;
        if *exact != receipt.requirement.binding()? {
            return Err("MODEL delivery lacks exact dispatched provenance".into());
        }
        let entry = self
            .reservations
            .get(&exact.invocation)
            .ok_or("MODEL delivery dispatch absent")?;
        if entry.permit.as_ref() != Some(&receipt.permit) {
            return Err("MODEL delivery permit changed".into());
        }
        Ok(())
    }
    /// Validate physical-host proof, not a model role or runner-supplied IDs.
    /// Stop does not erase delivery of an already dispatched HTTP request.
    /// # Errors
    /// Forged proof, different authenticated host or missing dispatch custody.
    pub fn accept_model_delivery(
        &mut self,
        receipt: &NativeModelDeliveryReceipt,
        host: [u8; 32],
    ) -> Result<(), String> {
        if host != receipt.host() {
            return Err("MODEL delivery authenticated host changed".into());
        }
        self.check_model_delivery(receipt)?;
        let id = receipt.requirement.invocation;
        if let Some(prior) = self.model_deliveries.get(&id) {
            if serde_json::to_vec(prior).map_err(|e| e.to_string())?
                != serde_json::to_vec(receipt).map_err(|e| e.to_string())?
            {
                return Err("MODEL delivery invocation reused".into());
            }
        } else {
            if self.model_deliveries.len() >= super::MAX_RESERVATIONS {
                return Err("MODEL delivery capacity exceeded".into());
            }
            self.model_deliveries.insert(id, receipt.clone());
        }
        Ok(())
    }
}
impl PrivateTaskMailboxStore {
    /// Full owner-envelope comparison; IDs alone cannot promote delivery.
    /// # Errors
    /// Invalid signed receipt or private mailbox integrity failure.
    pub fn apply_model_delivery(
        &mut self,
        receipt: &NativeModelDeliveryReceipt,
    ) -> Result<(), String> {
        receipt.apply_to_messages(&mut self.messages)?;
        self.validate()
    }
}
impl NativeWorkspaceOwner {
    /// Persist authenticated successful delivery together with the private inbox.
    /// No current lease/policy check here can turn this historical fact into
    /// authority; new actions still require the live applied frontier.
    /// # Errors
    /// Invalid signed source proof, missing dispatch or private storage failure.
    pub fn record_native_model_delivery(
        &self,
        receipt: &NativeModelDeliveryReceipt,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let room = receipt.room();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if candidate.device().device_public_key != receipt.host() {
            return Err("MODEL delivery physical host changed".into());
        }
        candidate
            .record
            .task_admission
            .accept_model_delivery(receipt, receipt.host())?;
        candidate
            .record
            .private_task_mailboxes
            .apply_model_delivery(receipt)?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("owner generation exhausted")?;
        self.publish_room(room, candidate, None)
    }
}
