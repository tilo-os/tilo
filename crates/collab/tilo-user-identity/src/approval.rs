//! Certified-device approvals for an exact privileged operation.
//!
//! The transport is deliberately outside this module. These pure wire types
//! bind a request and response to two root-certified device keys, one purpose,
//! one challenge, and a short time window. The requester consumes an approval
//! once through [`ApprovalLedger`]; approval never grants ambient remote
//! control or an open-ended authentication session.

use std::collections::BTreeMap;

use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};
use crate::{DeviceCert, verify_device_cert_at};

/// Frozen domain tag for request signatures.
pub const APPROVAL_REQUEST_TAG: &[u8] = b"tilo-device-approval-request-v1\0";
/// Frozen domain tag for response signatures.
pub const APPROVAL_RESPONSE_TAG: &[u8] = b"tilo-device-approval-response-v1\0";
/// Wire schema understood by this build.
pub const APPROVAL_SCHEMA_VERSION: u16 = 1;
/// Longest approval lifetime accepted by either peer.
pub const MAX_APPROVAL_LIFETIME_NS: u64 = 5 * 60 * 1_000_000_000;
const MAX_SPENT_APPROVALS: usize = 1_024;

/// A bounded, typed privileged operation. Display copy is derived locally
/// from this value; no requester-controlled prompt text is trusted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPurpose {
    /// Unlock one exact requester-owned login/session challenge.
    SessionUnlock {
        /// Opaque identifier for the exact session to unlock.
        session_id: [u8; 16],
    },
    /// Approve one WebAuthn/passkey challenge for a relying party.
    PasskeyAssertion {
        /// SHA-256 digest of the normalized relying-party identifier.
        rp_id_hash: [u8; 32],
        /// Digest of the authenticator challenge bytes.
        challenge_hash: [u8; 32],
    },
    /// Confirm one catalogued privileged action and exact argument digest.
    PrivilegedAction {
        /// Stable identifier from the local privileged-action catalogue.
        action_id: String,
        /// Digest of the canonical action arguments.
        arguments_hash: [u8; 32],
    },
}

impl ApprovalPurpose {
    fn validate(&self) -> Result<(), ApprovalError> {
        match self {
            Self::SessionUnlock { session_id } if *session_id == [0; 16] => {
                Err(ApprovalError::InvalidPurpose)
            }
            Self::PasskeyAssertion {
                rp_id_hash,
                challenge_hash,
            } if *rp_id_hash == [0; 32] || *challenge_hash == [0; 32] => {
                Err(ApprovalError::InvalidPurpose)
            }
            Self::PrivilegedAction {
                action_id,
                arguments_hash,
            } if action_id.is_empty()
                || action_id.len() > 128
                || *arguments_hash == [0; 32]
                || !action_id.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':')
                }) =>
            {
                Err(ApprovalError::InvalidPurpose)
            }
            _ => Ok(()),
        }
    }
}

/// Request signed by the device on which the privileged operation will run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    /// Wire schema version.
    pub version: u16,
    /// Random single-use request identifier.
    pub request_id: [u8; 16],
    /// Certified device key requesting approval.
    pub requester_device: [u8; 32],
    /// Exact certified device key allowed to answer.
    pub approver_device: [u8; 32],
    /// Optional workspace authority context. `None` means personal/device
    /// authority rather than silently matching every workspace.
    pub workspace_id: Option<u64>,
    /// Typed operation whose trusted local description is shown.
    pub purpose: ApprovalPurpose,
    /// Random challenge preventing precomputed responses.
    pub challenge: [u8; 32],
    /// Earliest valid decision time in Unix-epoch nanoseconds.
    pub issued_at_ns: u64,
    /// Exclusive expiry in Unix-epoch nanoseconds.
    pub expires_at_ns: u64,
    /// Requester-device Ed25519 signature over the canonical request.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct ApprovalRequestView<'a> {
    version: u16,
    request_id: &'a [u8; 16],
    requester_device: &'a [u8; 32],
    approver_device: &'a [u8; 32],
    workspace_id: Option<u64>,
    purpose: &'a ApprovalPurpose,
    challenge: &'a [u8; 32],
    issued_at_ns: u64,
    expires_at_ns: u64,
}

impl ApprovalRequest {
    fn view(&self) -> ApprovalRequestView<'_> {
        ApprovalRequestView {
            version: self.version,
            request_id: &self.request_id,
            requester_device: &self.requester_device,
            approver_device: &self.approver_device,
            workspace_id: self.workspace_id,
            purpose: &self.purpose,
            challenge: &self.challenge,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        }
    }

    /// Sign a new request with the requester's device key.
    pub fn sign(mut self, key: &ed25519_dalek::SigningKey) -> Result<Self, ApprovalError> {
        if key.verifying_key().to_bytes() != self.requester_device {
            return Err(ApprovalError::WrongDevice);
        }
        self.validate_shape()?;
        self.signature = key.sign(&self.digest()?).to_bytes();
        Ok(self)
    }

    /// Verify the requester's root-certified identity, signature, target, and
    /// validity window.
    pub fn verify_for(
        &self,
        requester_cert: &DeviceCert,
        expected_approver: [u8; 32],
        now_ns: u64,
    ) -> Result<(), ApprovalError> {
        self.validate_shape()?;
        verify_device_cert_at(requester_cert, now_ns).map_err(|_| ApprovalError::Certificate)?;
        if requester_cert.device_public_key != self.requester_device
            || self.approver_device != expected_approver
        {
            return Err(ApprovalError::WrongDevice);
        }
        if now_ns < self.issued_at_ns || now_ns >= self.expires_at_ns {
            return Err(ApprovalError::Expired);
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.requester_device)
            .map_err(|_| ApprovalError::Signature)?;
        key.verify(
            &self.digest()?,
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| ApprovalError::Signature)
    }

    fn validate_shape(&self) -> Result<(), ApprovalError> {
        if self.version != APPROVAL_SCHEMA_VERSION {
            return Err(ApprovalError::UnsupportedSchema(self.version));
        }
        if self.request_id == [0; 16]
            || self.challenge == [0; 32]
            || self.requester_device == self.approver_device
            || self.expires_at_ns <= self.issued_at_ns
            || self.expires_at_ns.saturating_sub(self.issued_at_ns) > MAX_APPROVAL_LIFETIME_NS
        {
            return Err(ApprovalError::InvalidRequest);
        }
        self.purpose.validate()
    }

    fn digest(&self) -> Result<[u8; 32], ApprovalError> {
        signing_digest(APPROVAL_REQUEST_TAG, &self.view()).map_err(Into::into)
    }
}

/// One approver-signed answer. A denial is signed too, preventing an
/// intermediary from keeping the requester indefinitely pending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceApproval {
    /// Wire schema version.
    pub version: u16,
    /// Digest of the exact signed request being answered.
    pub request_digest: [u8; 32],
    /// Certified device key that made the local decision.
    pub approver_device: [u8; 32],
    /// Whether trusted local UI approved or denied the request.
    pub approved: bool,
    /// Decision time in Unix-epoch nanoseconds.
    pub decided_at_ns: u64,
    /// Approver-device Ed25519 signature over the canonical response.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct DeviceApprovalView<'a> {
    version: u16,
    request_digest: &'a [u8; 32],
    approver_device: &'a [u8; 32],
    approved: bool,
    decided_at_ns: u64,
}

impl DeviceApproval {
    /// Produce an exact response after the approver has shown trusted local UI.
    pub fn sign_for(
        request: &ApprovalRequest,
        approved: bool,
        decided_at_ns: u64,
        key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, ApprovalError> {
        if key.verifying_key().to_bytes() != request.approver_device {
            return Err(ApprovalError::WrongDevice);
        }
        if decided_at_ns < request.issued_at_ns || decided_at_ns >= request.expires_at_ns {
            return Err(ApprovalError::Expired);
        }
        let mut answer = Self {
            version: APPROVAL_SCHEMA_VERSION,
            request_digest: request.digest()?,
            approver_device: request.approver_device,
            approved,
            decided_at_ns,
            signature: [0; 64],
        };
        answer.signature = key.sign(&answer.digest()?).to_bytes();
        Ok(answer)
    }

    fn view(&self) -> DeviceApprovalView<'_> {
        DeviceApprovalView {
            version: self.version,
            request_digest: &self.request_digest,
            approver_device: &self.approver_device,
            approved: self.approved,
            decided_at_ns: self.decided_at_ns,
        }
    }

    fn digest(&self) -> Result<[u8; 32], ApprovalError> {
        signing_digest(APPROVAL_RESPONSE_TAG, &self.view()).map_err(Into::into)
    }
}

/// Bounded requester-side replay ledger.
#[derive(Clone, Debug, Default)]
pub struct ApprovalLedger {
    spent: BTreeMap<[u8; 32], u64>,
}

impl ApprovalLedger {
    /// Verify and consume one positive response exactly once.
    pub fn consume(
        &mut self,
        request: &ApprovalRequest,
        approval: &DeviceApproval,
        requester_cert: &DeviceCert,
        approver_cert: &DeviceCert,
        now_ns: u64,
    ) -> Result<(), ApprovalError> {
        self.spent.retain(|_, expiry| *expiry > now_ns);
        request.verify_for(requester_cert, approval.approver_device, now_ns)?;
        verify_device_cert_at(approver_cert, now_ns).map_err(|_| ApprovalError::Certificate)?;
        if requester_cert.user_public_key != approver_cert.user_public_key {
            return Err(ApprovalError::DifferentIdentity);
        }
        if approval.version != APPROVAL_SCHEMA_VERSION
            || approval.approver_device != request.approver_device
            || approver_cert.device_public_key != approval.approver_device
            || approval.request_digest != request.digest()?
        {
            return Err(ApprovalError::WrongRequest);
        }
        if approval.decided_at_ns < request.issued_at_ns
            || approval.decided_at_ns >= request.expires_at_ns
            || approval.decided_at_ns > now_ns
            || now_ns >= request.expires_at_ns
        {
            return Err(ApprovalError::Expired);
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&approval.approver_device)
            .map_err(|_| ApprovalError::Signature)?;
        key.verify(
            &approval.digest()?,
            &ed25519_dalek::Signature::from_bytes(&approval.signature),
        )
        .map_err(|_| ApprovalError::Signature)?;
        if !approval.approved {
            return Err(ApprovalError::Denied);
        }
        if self.spent.contains_key(&approval.request_digest) {
            return Err(ApprovalError::Replay);
        }
        if self.spent.len() >= MAX_SPENT_APPROVALS {
            return Err(ApprovalError::LedgerFull);
        }
        self.spent
            .insert(approval.request_digest, request.expires_at_ns);
        Ok(())
    }
}

/// Device-approval validation failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ApprovalError {
    /// The wire schema is not understood by this build.
    #[error("unsupported device-approval schema {0}")]
    UnsupportedSchema(u16),
    /// A request violates an identifier, challenge, or time-window invariant.
    #[error("device-approval request is malformed")]
    InvalidRequest,
    /// The typed purpose is empty, unbounded, or otherwise invalid.
    #[error("device-approval purpose is invalid")]
    InvalidPurpose,
    /// A signing key, certificate, or requested target does not match.
    #[error("device-approval is bound to another device")]
    WrongDevice,
    /// The response does not answer the exact supplied request.
    #[error("device-approval is bound to another request")]
    WrongRequest,
    /// A device certificate is invalid or expired.
    #[error("device certificate is invalid or expired")]
    Certificate,
    /// The two device certificates chain to different user roots.
    #[error("requester and approver are not certified by the same user identity")]
    DifferentIdentity,
    /// A request or response signature does not verify.
    #[error("device-approval signature is invalid")]
    Signature,
    /// The request, decision, or certificate is not currently valid.
    #[error("device-approval request or decision is expired")]
    Expired,
    /// The approver explicitly denied the operation.
    #[error("device denied the request")]
    Denied,
    /// This positive approval has already been consumed.
    #[error("device-approval was already consumed")]
    Replay,
    /// The bounded replay ledger has no capacity for another live request.
    #[error("device-approval replay ledger is full")]
    LedgerFull,
    /// Canonical wire encoding failed.
    #[error("device-approval canonical encoding failed: {0}")]
    Canonical(#[from] CanonicalError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceCapabilities, DeviceCert, sign_device_cert};
    use getrandom::{SysRng, rand_core::UnwrapErr};

    fn cert(root: &ed25519_dalek::SigningKey, device: &ed25519_dalek::SigningKey) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "device".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: Some(10_000),
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn request(
        requester: &ed25519_dalek::SigningKey,
        approver: &ed25519_dalek::SigningKey,
    ) -> ApprovalRequest {
        ApprovalRequest {
            version: APPROVAL_SCHEMA_VERSION,
            request_id: [1; 16],
            requester_device: requester.verifying_key().to_bytes(),
            approver_device: approver.verifying_key().to_bytes(),
            workspace_id: Some(7),
            purpose: ApprovalPurpose::PrivilegedAction {
                action_id: "settings.identity.rotate".into(),
                arguments_hash: [2; 32],
            },
            challenge: [3; 32],
            issued_at_ns: 100,
            expires_at_ns: 1_000,
            signature: [0; 64],
        }
        .sign(requester)
        .unwrap()
    }

    /// Exact certified devices approve once; a wrong target, expiry, denial or mutation fails
    /// closed; a foreign identity or a future decision fails closed.
    #[test]
    fn device_approvals_count_once_and_fail_closed_on_anything_unexpected() {
        // Phase: exact_certified_devices_approve_once.
        {
            let mut rng = UnwrapErr(SysRng);
            let root = ed25519_dalek::SigningKey::generate(&mut rng);
            let requester = ed25519_dalek::SigningKey::generate(&mut rng);
            let approver = ed25519_dalek::SigningKey::generate(&mut rng);
            let requester_cert = cert(&root, &requester);
            let approver_cert = cert(&root, &approver);
            let request = request(&requester, &approver);
            request
                .verify_for(&requester_cert, approver.verifying_key().to_bytes(), 200)
                .unwrap();
            let approval = DeviceApproval::sign_for(&request, true, 250, &approver).unwrap();
            let mut ledger = ApprovalLedger::default();
            ledger
                .consume(&request, &approval, &requester_cert, &approver_cert, 300)
                .unwrap();
            assert_eq!(
                ledger.consume(&request, &approval, &requester_cert, &approver_cert, 301),
                Err(ApprovalError::Replay)
            );
        }

        // Phase: wrong_target_expiry_denial_and_mutation_fail_closed.
        {
            let mut rng = UnwrapErr(SysRng);
            let root = ed25519_dalek::SigningKey::generate(&mut rng);
            let requester = ed25519_dalek::SigningKey::generate(&mut rng);
            let approver = ed25519_dalek::SigningKey::generate(&mut rng);
            let other = ed25519_dalek::SigningKey::generate(&mut rng);
            let requester_cert = cert(&root, &requester);
            let approver_cert = cert(&root, &approver);
            let request = request(&requester, &approver);
            assert_eq!(
                request.verify_for(&requester_cert, other.verifying_key().to_bytes(), 200),
                Err(ApprovalError::WrongDevice)
            );
            assert_eq!(
                request.verify_for(&requester_cert, approver.verifying_key().to_bytes(), 1_000),
                Err(ApprovalError::Expired)
            );

            let denial = DeviceApproval::sign_for(&request, false, 250, &approver).unwrap();
            assert_eq!(
                ApprovalLedger::default().consume(
                    &request,
                    &denial,
                    &requester_cert,
                    &approver_cert,
                    300
                ),
                Err(ApprovalError::Denied)
            );
            let mut mutated = DeviceApproval::sign_for(&request, true, 250, &approver).unwrap();
            mutated.approved = false;
            assert_eq!(
                ApprovalLedger::default().consume(
                    &request,
                    &mutated,
                    &requester_cert,
                    &approver_cert,
                    300
                ),
                Err(ApprovalError::Signature)
            );
        }

        // Phase: foreign_identity_and_future_decision_fail_closed.
        {
            let mut rng = UnwrapErr(SysRng);
            let requester_root = ed25519_dalek::SigningKey::generate(&mut rng);
            let approver_root = ed25519_dalek::SigningKey::generate(&mut rng);
            let requester = ed25519_dalek::SigningKey::generate(&mut rng);
            let approver = ed25519_dalek::SigningKey::generate(&mut rng);
            let requester_cert = cert(&requester_root, &requester);
            let foreign_approver_cert = cert(&approver_root, &approver);
            let request = request(&requester, &approver);
            let approval = DeviceApproval::sign_for(&request, true, 250, &approver).unwrap();
            assert_eq!(
                ApprovalLedger::default().consume(
                    &request,
                    &approval,
                    &requester_cert,
                    &foreign_approver_cert,
                    300,
                ),
                Err(ApprovalError::DifferentIdentity)
            );

            let future = DeviceApproval::sign_for(&request, true, 400, &approver).unwrap();
            let approver_cert = cert(&requester_root, &approver);
            assert_eq!(
                ApprovalLedger::default().consume(
                    &request,
                    &future,
                    &requester_cert,
                    &approver_cert,
                    300,
                ),
                Err(ApprovalError::Expired)
            );
        }
    }
}
