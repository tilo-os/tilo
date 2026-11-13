//! Certified peer authentication for collaborative-files transports. Iroh proves
//! only the endpoint key, so a root-signed [`DeviceCert`] plus a fresh challenge
//! signed by that certified device key prove the human and the device behind a
//! connection.
//!
//! Challenges are verifier-issued, short-lived, single-use and bounded; a proof is
//! valid only on the connection whose observed endpoint equals both the proof's
//! endpoint and the certificate's device key.

use std::collections::HashMap;

use ed25519_dalek::Verifier;
use getrandom::{SysRng, rand_core::TryRng as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceCertError, RevocationList, verify_device_cert_at,
};

/// Frozen domain-separation tag for collaborative-FS peer proofs.
pub const FS_PEER_AUTH_TAG: &[u8] = b"tilo-fs-peer-auth-v1\0";

pub const DEFAULT_CHALLENGE_TTL_NS: u64 = 30_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerAuthChallenge {
    pub verifier_endpoint: [u8; 32],
    pub nonce: [u8; 32],
    /// Absolute Unix time in nanoseconds.
    pub expires_at_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerAuthProof {
    /// Root-signed certificate binding the device to a human identity.
    pub cert: DeviceCert,
    /// Iroh endpoint claimed by the prover. It must match both the observed
    /// connection endpoint and `cert.device_public_key`.
    pub prover_endpoint: [u8; 32],
    pub challenge: PeerAuthChallenge,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct ProofView<'a> {
    cert: &'a DeviceCert,
    prover_endpoint: &'a [u8; 32],
    challenge: &'a PeerAuthChallenge,
}

impl PeerAuthProof {
    fn view(&self) -> ProofView<'_> {
        ProofView {
            cert: &self.cert,
            prover_endpoint: &self.prover_endpoint,
            challenge: &self.challenge,
        }
    }

    /// Answer a challenge with the certified device key, refusing mismatched key,
    /// certificate or endpoint inputs rather than emitting a proof every verifier
    /// must reject.
    pub fn respond(
        cert: DeviceCert,
        prover_endpoint: [u8; 32],
        challenge: PeerAuthChallenge,
        device: &dyn collab_types::signer::DeviceSigner,
    ) -> Result<Self, PeerAuthError> {
        let device_public_key = device.device_public_key();
        if cert.device_public_key != device_public_key || prover_endpoint != device_public_key {
            return Err(PeerAuthError::LocalKeyMismatch);
        }
        let mut proof = Self {
            cert,
            prover_endpoint,
            challenge,
            signature: [0; 64],
        };
        proof.signature = collab_types::signer::sign_now(device, proof_digest(&proof)?.to_vec())
            .map_err(|_| PeerAuthError::LocalKeyMismatch)?;
        Ok(proof)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedFsPeer {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub device_label: String,
    /// Root-certified device capabilities; workspace ACL admission is a separate
    /// check by the caller.
    pub capabilities: DeviceCapabilities,
    pub cert: DeviceCert,
}

/// Certified-peer authentication failures. Every variant denies admission.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PeerAuthError {
    #[error("peer device certificate is invalid: {0}")]
    Certificate(#[from] DeviceCertError),
    #[error("peer device has been revoked")]
    Revoked,
    #[error("certified device does not match the observed Iroh endpoint")]
    EndpointMismatch,
    #[error("challenge is unknown or has already been consumed")]
    UnknownOrReplayedChallenge,
    #[error("proof does not answer the exact issued challenge")]
    ChallengeMismatch,
    #[error("peer authentication challenge has expired")]
    ChallengeExpired,
    #[error("pending peer authentication challenge limit reached")]
    ChallengeCapacity,
    #[error("peer authentication nonce is already pending")]
    DuplicateChallenge,
    #[error("secure challenge nonce generation failed")]
    Randomness,
    #[error("local device key, certificate, and endpoint do not match")]
    LocalKeyMismatch,
    #[error("peer challenge signature did not verify")]
    BadProofSignature,
    #[error("peer proof encoding failed")]
    Encoding,
}

/// Bounded, single-use verifier state for in-progress handshakes: verification
/// removes a nonce before any other work, so neither a successful proof nor an
/// invalid attempt can be replayed.
#[derive(Debug)]
pub struct PendingPeerChallenges {
    verifier_endpoint: [u8; 32],
    max_pending: usize,
    max_ttl_ns: u64,
    pending: HashMap<[u8; 32], PeerAuthChallenge>,
}

impl PendingPeerChallenges {
    /// `max_pending` is clamped to at least one; lifetimes cannot exceed `max_ttl_ns`.
    pub fn new(verifier_endpoint: [u8; 32], max_pending: usize, max_ttl_ns: u64) -> Self {
        Self {
            verifier_endpoint,
            max_pending: max_pending.max(1),
            max_ttl_ns,
            pending: HashMap::new(),
        }
    }

    /// Issue a fresh random challenge using the default lifetime.
    pub fn issue(&mut self, now_ns: u64) -> Result<PeerAuthChallenge, PeerAuthError> {
        self.issue_with_ttl(now_ns, DEFAULT_CHALLENGE_TTL_NS)
    }

    /// Issue a fresh random challenge with a caller-chosen, bounded lifetime.
    pub fn issue_with_ttl(
        &mut self,
        now_ns: u64,
        ttl_ns: u64,
    ) -> Result<PeerAuthChallenge, PeerAuthError> {
        let mut nonce = [0u8; 32];
        SysRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| PeerAuthError::Randomness)?;
        self.register(nonce, now_ns, ttl_ns)
    }

    /// Register an explicit nonce, for deterministic protocol tests; production
    /// callers use [`Self::issue`].
    pub fn register(
        &mut self,
        nonce: [u8; 32],
        now_ns: u64,
        ttl_ns: u64,
    ) -> Result<PeerAuthChallenge, PeerAuthError> {
        self.pending
            .retain(|_, challenge| challenge.expires_at_ns > now_ns);
        if self.pending.len() >= self.max_pending {
            return Err(PeerAuthError::ChallengeCapacity);
        }
        if self.pending.contains_key(&nonce) {
            return Err(PeerAuthError::DuplicateChallenge);
        }
        let effective_ttl = ttl_ns.min(self.max_ttl_ns);
        let challenge = PeerAuthChallenge {
            verifier_endpoint: self.verifier_endpoint,
            nonce,
            expires_at_ns: now_ns.saturating_add(effective_ttl),
        };
        self.pending.insert(nonce, challenge.clone());
        Ok(challenge)
    }

    /// Consume and authenticate a proof from an observed Iroh endpoint: certificate
    /// validity, same-user-scoped non-revocation, endpoint binding, possession of
    /// the certified device key, and freshness. Workspace access is not granted;
    /// sync admission evaluates the human and device against the workspace ACL next.
    pub fn verify(
        &mut self,
        proof: &PeerAuthProof,
        observed_remote_endpoint: [u8; 32],
        revocations: &RevocationList,
        now_ns: u64,
    ) -> Result<AuthenticatedFsPeer, PeerAuthError> {
        // Consume first. Invalid attempts also burn their challenge, which is
        // fail-closed and prevents signature-oracle retries/replays.
        let issued = self
            .pending
            .remove(&proof.challenge.nonce)
            .ok_or(PeerAuthError::UnknownOrReplayedChallenge)?;
        if issued != proof.challenge {
            return Err(PeerAuthError::ChallengeMismatch);
        }
        if now_ns >= issued.expires_at_ns {
            return Err(PeerAuthError::ChallengeExpired);
        }
        verify_device_cert_at(&proof.cert, now_ns)?;

        // A RevocationList can aggregate several humans. Only the root that
        // certified this device may revoke this particular certification.
        let revoked = revocations.iter().any(|entry| {
            entry.user_public_key == proof.cert.user_public_key
                && entry.revoked_device_public_key == proof.cert.device_public_key
                && entry.revoked_at_ns <= now_ns
        });
        if revoked {
            return Err(PeerAuthError::Revoked);
        }
        if proof.prover_endpoint != observed_remote_endpoint
            || proof.prover_endpoint != proof.cert.device_public_key
        {
            return Err(PeerAuthError::EndpointMismatch);
        }
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&proof.cert.device_public_key)
            .map_err(|_| PeerAuthError::BadProofSignature)?;
        let signature = ed25519_dalek::Signature::from_bytes(&proof.signature);
        verifying_key
            .verify(&proof_digest(proof)?, &signature)
            .map_err(|_| PeerAuthError::BadProofSignature)?;

        Ok(AuthenticatedFsPeer {
            user_public_key: proof.cert.user_public_key,
            device_public_key: proof.cert.device_public_key,
            device_label: proof.cert.device_label.clone(),
            capabilities: proof.cert.capabilities.clone(),
            cert: proof.cert.clone(),
        })
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

fn proof_digest(proof: &PeerAuthProof) -> Result<[u8; 32], PeerAuthError> {
    let encoded = postcard::to_stdvec(&proof.view()).map_err(|_| PeerAuthError::Encoding)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(FS_PEER_AUTH_TAG);
    hasher.update(&encoded);
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests;
