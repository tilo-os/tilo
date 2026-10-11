//! Certified peer authentication for collaborative-files transports.
//!
//! Iroh proves that a connection terminates at an endpoint key. That alone
//! says nothing about the human who owns the endpoint. This module adds the
//! missing link: a root-signed [`DeviceCert`] and a fresh challenge signed by
//! that certified device key. Admission code can then make policy decisions
//! against both the verified human and the verified device.
//!
//! Challenges are verifier-issued, short-lived, single-use, and bounded. A
//! proof is valid only on the connection whose observed Iroh endpoint equals
//! both the proof's endpoint and the device key in its certificate.

use std::collections::HashMap;

use ed25519_dalek::{Signer, Verifier};
use getrandom::{SysRng, rand_core::TryRng as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceCertError, RevocationList, verify_device_cert_at,
};

/// Frozen domain-separation tag for collaborative-FS peer proofs.
pub const FS_PEER_AUTH_TAG: &[u8] = b"tilo-fs-peer-auth-v1\0";

/// Default maximum lifetime of a transport authentication challenge.
pub const DEFAULT_CHALLENGE_TTL_NS: u64 = 30_000_000_000;

/// A challenge created by the accepting endpoint and sent to a peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerAuthChallenge {
    /// Iroh endpoint accepting the connection.
    pub verifier_endpoint: [u8; 32],
    /// Cryptographically random, verifier-issued nonce.
    pub nonce: [u8; 32],
    /// Absolute Unix-time deadline in nanoseconds.
    pub expires_at_ns: u64,
}

/// A certified device's response to a [`PeerAuthChallenge`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerAuthProof {
    /// Root-signed certificate binding the device to a human identity.
    pub cert: DeviceCert,
    /// Iroh endpoint claimed by the prover. It must match both the observed
    /// connection endpoint and `cert.device_public_key`.
    pub prover_endpoint: [u8; 32],
    /// The exact verifier-issued challenge being answered.
    pub challenge: PeerAuthChallenge,
    /// Device-key signature over the domain-separated proof payload.
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

    /// Answer a challenge with the certified device key.
    ///
    /// This refuses mismatched key/certificate/endpoint inputs rather than
    /// emitting a proof that every conforming verifier must reject.
    ///
    /// # Errors
    ///
    /// Returns an error when the certified key and endpoint disagree or the
    /// canonical proof cannot be encoded.
    pub fn respond(
        cert: DeviceCert,
        prover_endpoint: [u8; 32],
        challenge: PeerAuthChallenge,
        device_signing_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, PeerAuthError> {
        let device_public_key = device_signing_key.verifying_key().to_bytes();
        if cert.device_public_key != device_public_key || prover_endpoint != device_public_key {
            return Err(PeerAuthError::LocalKeyMismatch);
        }
        let mut proof = Self {
            cert,
            prover_endpoint,
            challenge,
            signature: [0; 64],
        };
        proof.signature = device_signing_key.sign(&proof_digest(&proof)?).to_bytes();
        Ok(proof)
    }
}

/// Identity established by a successfully consumed peer proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedFsPeer {
    /// Human root identity that certified the remote device.
    pub user_public_key: [u8; 32],
    /// Certified device key and observed Iroh endpoint identity.
    pub device_public_key: [u8; 32],
    /// Human-readable, root-certified device label.
    pub device_label: String,
    /// Root-certified device capabilities. Workspace ACL admission remains a
    /// separate, mandatory check by the caller.
    pub capabilities: DeviceCapabilities,
    /// The complete verified certificate for audit/UI use.
    pub cert: DeviceCert,
}

/// Certified-peer authentication failures. Every variant denies admission.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PeerAuthError {
    /// The peer's root-signed device certificate is invalid or expired.
    #[error("peer device certificate is invalid: {0}")]
    Certificate(#[from] DeviceCertError),
    /// A verified, same-user revocation is effective for this device.
    #[error("peer device has been revoked")]
    Revoked,
    /// The proof's certified device does not match the observed Iroh peer.
    #[error("certified device does not match the observed Iroh endpoint")]
    EndpointMismatch,
    /// The challenge was never issued locally or has already been consumed.
    #[error("challenge is unknown or has already been consumed")]
    UnknownOrReplayedChallenge,
    /// The returned challenge differs from the one issued for this nonce.
    #[error("proof does not answer the exact issued challenge")]
    ChallengeMismatch,
    /// The challenge deadline has passed.
    #[error("peer authentication challenge has expired")]
    ChallengeExpired,
    /// Too many authentication handshakes are pending.
    #[error("pending peer authentication challenge limit reached")]
    ChallengeCapacity,
    /// The caller attempted to register a nonce already pending.
    #[error("peer authentication nonce is already pending")]
    DuplicateChallenge,
    /// Secure random nonce generation failed.
    #[error("secure challenge nonce generation failed")]
    Randomness,
    /// Local proof construction used inconsistent key identities.
    #[error("local device key, certificate, and endpoint do not match")]
    LocalKeyMismatch,
    /// The device's proof-of-possession signature is invalid.
    #[error("peer challenge signature did not verify")]
    BadProofSignature,
    /// Canonical proof encoding failed.
    #[error("peer proof encoding failed")]
    Encoding,
}

/// Bounded, single-use verifier state for in-progress handshakes.
///
/// A store belongs to one accepting endpoint. Expired challenges are pruned
/// when new ones are issued. Verification removes a nonce before doing any
/// other work, so neither a successful proof nor an invalid attempt can be
/// replayed against the same challenge.
#[derive(Debug)]
pub struct PendingPeerChallenges {
    verifier_endpoint: [u8; 32],
    max_pending: usize,
    max_ttl_ns: u64,
    pending: HashMap<[u8; 32], PeerAuthChallenge>,
}

impl PendingPeerChallenges {
    /// Create a verifier store. `max_pending` is clamped to at least one;
    /// challenge lifetimes cannot exceed `max_ttl_ns`.
    pub fn new(verifier_endpoint: [u8; 32], max_pending: usize, max_ttl_ns: u64) -> Self {
        Self {
            verifier_endpoint,
            max_pending: max_pending.max(1),
            max_ttl_ns,
            pending: HashMap::new(),
        }
    }

    /// Issue a fresh random challenge using the default lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error when randomness fails or the pending store cannot
    /// accept another unique challenge.
    pub fn issue(&mut self, now_ns: u64) -> Result<PeerAuthChallenge, PeerAuthError> {
        self.issue_with_ttl(now_ns, DEFAULT_CHALLENGE_TTL_NS)
    }

    /// Issue a fresh random challenge with a caller-selected, bounded lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error when randomness fails or the pending store cannot
    /// accept another unique challenge.
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

    /// Register an explicit nonce. This is useful for deterministic protocol
    /// tests; production callers should use [`Self::issue`].
    ///
    /// # Errors
    ///
    /// Returns an error when the nonce is duplicated or capacity is exhausted.
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

    /// Consume and authenticate a proof from an observed Iroh endpoint.
    ///
    /// Returning [`AuthenticatedFsPeer`] proves certificate validity,
    /// same-user-scoped non-revocation, endpoint binding, possession of the
    /// certified device key, and freshness. It does not grant workspace
    /// access; sync admission must next evaluate the human/device against the
    /// workspace ACL.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown, replayed, expired, revoked,
    /// mismatched, incorrectly certified, or incorrectly signed proof.
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

    /// Number of currently pending challenges.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether no challenges are pending.
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
