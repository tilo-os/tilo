//! A person's signed statement that an agent on one of their devices acts
//! for them in a room — the session recorder, recording what they see there.
//!
//! Such an agent needs no room administrator: it reads no more than its
//! person can. But being recorded is something everyone in the room must be
//! able to see, so the delegation is not kept on the recording device alone.
//! The device signs a [`RecordingDelegationV1`] with its certified device
//! key, and every member verifies it against the person's device certificate
//! before listing the agent. A later record from the same device for the
//! same agent and room replaces an earlier one; stopping publishes one with
//! `active: false`, which takes the agent off every member's list.
//!
//! What a member checks beyond the signature — that the certificate is the
//! person's and unrevoked, that the person still holds a role in the room,
//! that the room allows recording delegates — is room state this crate does
//! not hold, and is left to the member.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::CanonicalError;
use crate::crypto::{Ed25519PublicKey, Signature, VerifyError, sign_event, verify_event};

/// Signing-domain tag mixed into every delegation's signed bytes.
pub const RECORDING_DELEGATION_DOMAIN_TAG: &str = "tilo-recording-delegation-v1";

/// Longest agent id a delegation may name.
pub const MAX_DELEGATE_AGENT_LEN: usize = 64;

/// One device's agent acting for its person in one room, or no longer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingDelegationV1 {
    /// The room, as its canonical uuid string.
    pub room: String,
    /// The room's workspace (topic) id.
    pub workspace: u64,
    /// The person the agent acts for: their user root key.
    pub person: Ed25519PublicKey,
    /// The device the agent runs on, certified by `person`. Signs this
    /// record.
    pub device: Ed25519PublicKey,
    /// The agent's id, the capability subject it is listed as.
    pub agent: String,
    /// `true` while the agent acts; `false` once its person stopped it.
    pub active: bool,
    /// When the device signed this, Unix seconds by its clock. Orders the
    /// records of one device, agent and room: the latest is the one that
    /// holds.
    pub issued_at_unix_seconds: u64,
    /// Signature by `device` over every other field.
    pub signature: Signature,
}

/// Everything a delegation says, before the device signs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingDelegationSpec {
    /// See [`RecordingDelegationV1::room`].
    pub room: String,
    /// See [`RecordingDelegationV1::workspace`].
    pub workspace: u64,
    /// See [`RecordingDelegationV1::person`].
    pub person: Ed25519PublicKey,
    /// See [`RecordingDelegationV1::agent`].
    pub agent: String,
    /// See [`RecordingDelegationV1::active`].
    pub active: bool,
    /// See [`RecordingDelegationV1::issued_at_unix_seconds`].
    pub issued_at_unix_seconds: u64,
}

/// Why a delegation is refused.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RecordingDelegationError {
    /// The agent id is empty, too long, or not a plain identifier.
    #[error("a delegation must name its agent by a short plain id")]
    BadAgent,
    /// The signature did not verify against `device`.
    #[error("delegation signature did not verify: {0}")]
    BadSignature(#[source] VerifyError),
    /// Canonical encoding failed. Effectively unreachable.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

#[derive(Serialize)]
struct DelegationView<'a> {
    tag: &'a str,
    room: &'a str,
    workspace: u64,
    person: &'a Ed25519PublicKey,
    device: &'a Ed25519PublicKey,
    agent: &'a str,
    active: bool,
    issued_at_unix_seconds: u64,
}

fn agent_is_plain(agent: &str) -> bool {
    !agent.is_empty()
        && agent.len() <= MAX_DELEGATE_AGENT_LEN
        && agent
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

impl RecordingDelegationV1 {
    fn view(&self) -> DelegationView<'_> {
        DelegationView {
            tag: RECORDING_DELEGATION_DOMAIN_TAG,
            room: &self.room,
            workspace: self.workspace,
            person: &self.person,
            device: &self.device,
            agent: &self.agent,
            active: self.active,
            issued_at_unix_seconds: self.issued_at_unix_seconds,
        }
    }

    /// Sign `spec` as the device holding `device_sk`.
    ///
    /// # Errors
    ///
    /// [`RecordingDelegationError::BadAgent`] for an agent id no member
    /// would accept, or a canonical encoding failure.
    pub fn sign(
        spec: RecordingDelegationSpec,
        device_sk: &ed25519_dalek::SigningKey,
    ) -> Result<Self, RecordingDelegationError> {
        if !agent_is_plain(&spec.agent) {
            return Err(RecordingDelegationError::BadAgent);
        }
        let mut delegation = Self {
            room: spec.room,
            workspace: spec.workspace,
            person: spec.person,
            device: Ed25519PublicKey::from_bytes(device_sk.verifying_key().to_bytes()),
            agent: spec.agent,
            active: spec.active,
            issued_at_unix_seconds: spec.issued_at_unix_seconds,
            signature: Signature::from_bytes([0; 64]),
        };
        delegation.signature = sign_event(&delegation.view(), device_sk)?;
        Ok(delegation)
    }

    /// Check the record's own shape and its device's signature. Whether the
    /// device is `person`'s is the caller's to check, against the device
    /// certificate delivered with it.
    ///
    /// # Errors
    ///
    /// [`RecordingDelegationError::BadAgent`] or
    /// [`RecordingDelegationError::BadSignature`].
    pub fn verify(&self) -> Result<(), RecordingDelegationError> {
        if !agent_is_plain(&self.agent) {
            return Err(RecordingDelegationError::BadAgent);
        }
        verify_event(&self.view(), &self.signature, &self.device)
            .map_err(RecordingDelegationError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(active: bool) -> RecordingDelegationSpec {
        RecordingDelegationSpec {
            room: "6b3f9a10-0000-4000-8000-00000000000c".into(),
            workspace: 7,
            person: Ed25519PublicKey::from_bytes([1; 32]),
            agent: "recorder".into(),
            active,
            issued_at_unix_seconds: 1_700_000_000,
        }
    }

    #[test]
    fn a_signed_delegation_verifies_and_any_change_breaks_it() {
        let device = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let delegation = RecordingDelegationV1::sign(spec(true), &device).unwrap();
        assert_eq!(delegation.verify(), Ok(()));

        let mut widened = delegation.clone();
        widened.room = "6b3f9a10-0000-4000-8000-00000000000d".into();
        assert!(matches!(
            widened.verify(),
            Err(RecordingDelegationError::BadSignature(_))
        ));
        let mut revived = RecordingDelegationV1::sign(spec(false), &device).unwrap();
        revived.active = true;
        assert!(matches!(
            revived.verify(),
            Err(RecordingDelegationError::BadSignature(_))
        ));
        let mut unsigned = delegation;
        unsigned.signature = Signature::from_bytes([0; 64]);
        assert!(unsigned.verify().is_err());
    }

    #[test]
    fn an_agent_id_must_be_plain() {
        let device = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut odd = spec(true);
        odd.agent = "recorder:observer".into();
        assert_eq!(
            RecordingDelegationV1::sign(odd, &device),
            Err(RecordingDelegationError::BadAgent)
        );
    }
}
