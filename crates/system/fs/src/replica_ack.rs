//! Authenticated, exact-version durable replica receipts.
//!
//! A receipt is useful only after the transport has mutually authenticated
//! both certified devices and the server has rechecked live record access.
//! It names the complete signed content tuple and a caller nonce, is signed by
//! the serving device, and expires quickly. This module never deletes data.

use collab_types::{Ed25519PublicKey, Signature, canonical_bytes, sign_event, verify_event};
use fs_protocol::{ContentRevisionV1, FileActor};
use getrandom::{SysRng, rand_core::TryRng as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::availability::ReplicaObservation;
use crate::peer_auth::AuthenticatedFsPeer;

pub const REPLICA_ACK_PROTOCOL_VERSION: u16 = 1;
pub const MAX_REPLICA_ACK_LIFETIME_MS: u64 = 5 * 60 * 1000;
pub(crate) const SIGNATURE_DOMAIN: &str = "tilo.collaborative-fs.replica-ack.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaAckRequestV1 {
    pub protocol_version: u16,
    pub workspace_id: Uuid,
    pub revision: ContentRevisionV1,
    pub nonce: [u8; 32],
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ReceiptSigningView<'a> {
    domain: &'static str,
    protocol_version: u16,
    workspace_id: Uuid,
    revision: &'a ContentRevisionV1,
    nonce: [u8; 32],
    actor: FileActor,
    issued_at_ms: u64,
    expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaAckV1 {
    pub protocol_version: u16,
    pub workspace_id: Uuid,
    pub revision: ContentRevisionV1,
    pub nonce: [u8; 32],
    pub actor: FileActor,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub signature: Signature,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::large_enum_variant,
    reason = "Boxing would change the versioned wire schema."
)]
pub enum ReplicaAckResponseV1 {
    Acknowledged(ReplicaAckV1),
    Denied,
    Unavailable,
}

impl ReplicaAckRequestV1 {
    /// Construct a validated request with a fresh nonce.
    pub fn new(workspace_id: Uuid, revision: ContentRevisionV1) -> Result<Self, ReplicaAckError> {
        let mut nonce = [0_u8; 32];
        SysRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| ReplicaAckError::Randomness)?;
        let request = Self {
            protocol_version: REPLICA_ACK_PROTOCOL_VERSION,
            workspace_id,
            revision,
            nonce,
        };
        request.validate()?;
        Ok(request)
    }

    /// Validate this request's version, scope, nonce, and revision.
    ///
    /// # Errors
    ///
    /// Returns an error when any request invariant is invalid.
    pub fn validate(&self) -> Result<(), ReplicaAckError> {
        if self.protocol_version != REPLICA_ACK_PROTOCOL_VERSION {
            return Err(ReplicaAckError::UnsupportedVersion(self.protocol_version));
        }
        if self.workspace_id.is_nil() || self.nonce == [0; 32] {
            return Err(ReplicaAckError::InvalidScope);
        }
        self.revision
            .validate()
            .map_err(|_| ReplicaAckError::InvalidRevision)
    }
}

impl ReplicaAckV1 {
    /// Sign a receipt for an exact request and actor.
    pub fn signed(
        request: &ReplicaAckRequestV1,
        actor: FileActor,
        issued_at_ms: u64,
        signing_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, ReplicaAckError> {
        request.validate()?;
        if signing_key.verifying_key().to_bytes() != *actor.device.as_bytes() {
            return Err(ReplicaAckError::WrongSigningKey);
        }
        let mut receipt = Self {
            protocol_version: REPLICA_ACK_PROTOCOL_VERSION,
            workspace_id: request.workspace_id,
            revision: request.revision,
            nonce: request.nonce,
            actor,
            issued_at_ms,
            expires_at_ms: issued_at_ms.saturating_add(MAX_REPLICA_ACK_LIFETIME_MS),
            signature: Signature::from_bytes([0; 64]),
        };
        receipt.signature = sign_event(&receipt.signing_view(), signing_key)?;
        Ok(receipt)
    }

    fn signing_view(&self) -> ReceiptSigningView<'_> {
        ReceiptSigningView {
            domain: SIGNATURE_DOMAIN,
            protocol_version: self.protocol_version,
            workspace_id: self.workspace_id,
            revision: &self.revision,
            nonce: self.nonce,
            actor: self.actor,
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
        }
    }

    /// Verify this receipt against the exact request and authenticated peer.
    pub fn verify_for(
        &self,
        request: &ReplicaAckRequestV1,
        peer: &AuthenticatedFsPeer,
        local_device: Ed25519PublicKey,
        now_ms: u64,
    ) -> Result<ReplicaObservation, ReplicaAckError> {
        request.validate()?;
        if self.workspace_id != request.workspace_id
            || self.revision != request.revision
            || self.nonce != request.nonce
        {
            return Err(ReplicaAckError::RequestMismatch);
        }
        self.verify_claim(peer, local_device, now_ms)
    }

    /// Verify an authenticated peer's unsolicited exact-record durability
    /// claim. Exact-share recipients submit these over the already
    /// capability-gated record protocol; the enclosing share session supplies
    /// the record scope, while the random nonce keeps independently generated
    /// receipts distinct.
    pub fn verify_claim(
        &self,
        peer: &AuthenticatedFsPeer,
        local_device: Ed25519PublicKey,
        now_ms: u64,
    ) -> Result<ReplicaObservation, ReplicaAckError> {
        if self.protocol_version != REPLICA_ACK_PROTOCOL_VERSION {
            return Err(ReplicaAckError::UnsupportedVersion(self.protocol_version));
        }
        if self.workspace_id.is_nil() || self.nonce == [0; 32] {
            return Err(ReplicaAckError::InvalidScope);
        }
        self.revision
            .validate()
            .map_err(|_| ReplicaAckError::InvalidRevision)?;
        let expected = FileActor {
            person: Ed25519PublicKey(peer.user_public_key),
            device: Ed25519PublicKey(peer.device_public_key),
        };
        if self.actor != expected {
            return Err(ReplicaAckError::PeerIdentityMismatch);
        }
        if self.actor.device == local_device {
            return Err(ReplicaAckError::SelfAcknowledgement);
        }
        if self.issued_at_ms > now_ms
            || self.expires_at_ms < now_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms) > MAX_REPLICA_ACK_LIFETIME_MS
        {
            return Err(ReplicaAckError::Stale);
        }
        verify_event(&self.signing_view(), &self.signature, &self.actor.device)?;
        Ok(ReplicaObservation {
            protocol_version: self.protocol_version,
            workspace_id: self.workspace_id,
            record_id: self.revision.record_id,
            person: self.actor.person,
            device: self.actor.device,
            version_id: self.revision.version_id,
            content_hash: self.revision.content_hash,
            byte_len: self.revision.byte_len,
            metadata_hash: self.revision.metadata_hash,
            nonce: self.nonce,
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
            observed_at_ms: now_ms,
            signature: self.signature,
        })
    }

    /// Encode this receipt canonically.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ReplicaAckError> {
        Ok(canonical_bytes(self)?)
    }
}

#[derive(Debug, Error)]
pub enum ReplicaAckError {
    #[error("unsupported replica acknowledgement version {0}")]
    UnsupportedVersion(u16),
    #[error("invalid workspace, nonce, or record scope")]
    InvalidScope,
    #[error("invalid content revision")]
    InvalidRevision,
    #[error("secure randomness unavailable")]
    Randomness,
    #[error("signing key does not match the certified device")]
    WrongSigningKey,
    #[error("receipt does not exactly answer this request")]
    RequestMismatch,
    #[error("receipt actor does not match the mutually authenticated peer")]
    PeerIdentityMismatch,
    #[error("the local device cannot acknowledge itself")]
    SelfAcknowledgement,
    #[error("receipt is expired, from the future, or has an excessive lifetime")]
    Stale,
    #[error(transparent)]
    Canonical(#[from] collab_types::CanonicalError),
    #[error(transparent)]
    Signature(#[from] collab_types::VerifyError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_protocol::{ContentHash, FileRecordId, FileVersionId};
    use getrandom::rand_core::UnwrapErr;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    fn revision() -> ContentRevisionV1 {
        ContentRevisionV1 {
            schema_version: 1,
            record_id: FileRecordId::new(),
            version_id: FileVersionId::new(),
            content_hash: Some(ContentHash([7; 32])),
            byte_len: 9,
            metadata_hash: ContentHash([8; 32]),
        }
    }

    #[test]
    fn exact_nonce_version_peer_and_freshness_are_bound() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let capabilities = DeviceCapabilities::member();
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "replica".into(),
            capabilities: capabilities.clone(),
            issued_at_ns: 0,
            expires_at_ns: Some(2_000_000_000_000),
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let peer = AuthenticatedFsPeer {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "replica".into(),
            capabilities,
            cert,
        };
        let request = ReplicaAckRequestV1::new(Uuid::new_v4(), revision()).unwrap();
        let receipt = ReplicaAckV1::signed(
            &request,
            FileActor {
                person: Ed25519PublicKey(peer.user_public_key),
                device: Ed25519PublicKey(peer.device_public_key),
            },
            100,
            &device,
        )
        .unwrap();
        assert!(
            receipt
                .verify_for(&request, &peer, Ed25519PublicKey([3; 32]), 101)
                .is_ok()
        );
        assert!(matches!(
            receipt.verify_for(&request, &peer, Ed25519PublicKey([3; 32]), 99),
            Err(ReplicaAckError::Stale)
        ));
        let mut wrong = request;
        wrong.revision.version_id = FileVersionId::new();
        assert!(matches!(
            receipt.verify_for(&wrong, &peer, Ed25519PublicKey([3; 32]), 101),
            Err(ReplicaAckError::RequestMismatch)
        ));
        assert!(matches!(
            receipt.verify_for(
                &request,
                &peer,
                Ed25519PublicKey(peer.device_public_key),
                101
            ),
            Err(ReplicaAckError::SelfAcknowledgement)
        ));
        assert!(matches!(
            receipt.verify_for(
                &request,
                &peer,
                Ed25519PublicKey([3; 32]),
                receipt.expires_at_ms + 1
            ),
            Err(ReplicaAckError::Stale)
        ));
    }
}
