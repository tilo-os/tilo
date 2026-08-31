//! The signed-op envelope: `SignedOp`.
//!
//! Every CRDT mutation crossing a process or network boundary is wrapped
//! in a `SignedOp`, binding: the opaque payload (an Automerge change
//! blob); the artifact id it applies to; the author's identity public
//! key; the attestation id underwriting "this author may apply ops of
//! this class to this artifact"; a self-declared op class (cross-checked
//! against the role table); a wall-clock timestamp (checked against the
//! attestation's validity window); a monotonic per-author sequence
//! number (replay defence); and a detached Ed25519 signature over all of it.
//!
//! Metadata fields stay cleartext even once MLS encryption is layered
//! in — only `payload` is encrypted — so the broker can route by
//! artifact id and filter by op class without decrypting.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::channel::{ArtifactId, ChannelId};
use crate::crypto::{Ed25519PublicKey, Signature};
use crate::identity::AttestationId;
use crate::role::OpClass;

/// Metadata describing how a `SignedOp.payload` was encrypted.
///
/// The presence of this field (not a separate wire flag) tells a
/// receiver the payload is ciphertext: `None` means `payload` is a
/// cleartext Automerge change blob (legacy/unencrypted channels);
/// `Some(meta)` means `payload` is MLS ciphertext from
/// `channel_crypto::ChannelGroup::encrypt` — the receiver looks up the
/// `ChannelGroup` for `channel`, checks `epoch` is reachable, and decrypts.
///
/// `SignedOp.signature` covers (cipher)bytes either way, and covers
/// `encryption` itself too, so a malicious peer can't strip the metadata
/// and re-route the ciphertext to a different channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptionMetadata {
    /// MLS epoch counter the ciphertext was produced under
    /// (`MlsGroup::epoch().as_u64()` at encrypt time). A receiver at an
    /// earlier local epoch can't decrypt and drops the message until it
    /// catches up (or accepts it within openmls's out-of-order window).
    pub epoch: u64,
    /// Channel id the ciphertext belongs to. Redundant with the
    /// artifact→channel lookup the broker performs anyway, but cheap (a
    /// 16-byte UUID) and lets the receiver pick the `ChannelGroup` directly.
    pub channel: ChannelId,
}

/// One signed CRDT mutation. `Clone` because the broker fans an op to
/// multiple peers; `PartialEq` for envelope-identity assertions in tests.
/// Deliberately not `Hash` — use the signature bytes as a map key if needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedOp {
    /// The opaque payload bytes — an Automerge change blob, or (once
    /// encrypted) MLS ciphertext of the same. The validator does not
    /// interpret this field.
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    /// The artifact this op mutates.
    pub artifact: ArtifactId,
    /// The author's identity public key. Must equal `attestation.subject`
    /// (validator check 4).
    pub author: Ed25519PublicKey,
    /// Id of the attestation underwriting this op, resolved against the
    /// channel's attestation store by the validator.
    pub attestation_id: AttestationId,
    /// The author's self-declared op class, cross-checked against the role
    /// table (validator check 5). A lying client doesn't gain capability —
    /// `role_permits(attestation.role, artifact.kind, op_class)` must be
    /// true regardless of what the client claims.
    pub op_class: OpClass,
    /// Wall-clock time the op was created, checked to fall inside
    /// `[attestation.valid_from, attestation.valid_until]` (validator check
    /// 6). Clock skew tolerance is the validator's responsibility.
    pub timestamp: SystemTime,
    /// Per-(author, channel) monotonic sequence number. Replay defence: the
    /// validator rejects an inbound op whose `seq` isn't strictly greater
    /// than the last seen for that author (validator check 7). Independent
    /// per author.
    pub seq: u64,
    /// Encryption mode. `None` means `payload` is cleartext (the legacy
    /// form); `Some(meta)` means MLS ciphertext. Part of the signed bytes so
    /// a peer can't rewrite the metadata to mis-route a ciphertext.
    ///
    /// Default-`None` keeps canonical-bytes stability for ops built before
    /// encryption existed — `Option<T>` postcards as a discriminant byte
    /// plus the optional payload, so an unencrypted op's serialised tail now
    /// carries one extra `0x00` byte, accepted as a wire bump.
    #[serde(default)]
    pub encryption: Option<EncryptionMetadata>,
    /// Detached Ed25519 signature by `author` over the canonical-bytes view
    /// of every other field. See `SignedOp::signing_bytes`.
    pub signature: Signature,
}

impl SignedOp {
    /// Build a fresh op envelope with a placeholder signature; pass to
    /// [`crate::crypto::sign_op`] to stamp the real one in-place. Defaults
    /// `encryption` to `None` — see
    /// [`SignedOp::new_unsigned_with_encryption`] for the encrypted form.
    pub fn new_unsigned(
        payload: Vec<u8>,
        artifact: ArtifactId,
        author: Ed25519PublicKey,
        attestation_id: AttestationId,
        op_class: OpClass,
        timestamp: SystemTime,
        seq: u64,
    ) -> Self {
        Self {
            payload,
            artifact,
            author,
            attestation_id,
            op_class,
            timestamp,
            seq,
            encryption: None,
            signature: Signature::from_bytes([0u8; 64]),
        }
    }

    /// Construct an unsigned op carrying an MLS ciphertext payload. The
    /// caller has already encrypted via `ChannelGroup::encrypt`; the
    /// ciphertext goes into `payload`, the matching `epoch`/`channel` into
    /// `encryption`. Sign with [`crate::crypto::sign_op`] as usual.
    #[allow(clippy::too_many_arguments)]
    pub fn new_unsigned_with_encryption(
        payload: Vec<u8>,
        artifact: ArtifactId,
        author: Ed25519PublicKey,
        attestation_id: AttestationId,
        op_class: OpClass,
        timestamp: SystemTime,
        seq: u64,
        encryption: EncryptionMetadata,
    ) -> Self {
        Self {
            payload,
            artifact,
            author,
            attestation_id,
            op_class,
            timestamp,
            seq,
            encryption: Some(encryption),
            signature: Signature::from_bytes([0u8; 64]),
        }
    }

    /// Canonical bytes the author signs to mint this envelope — the struct
    /// minus `signature`, stable regardless of when signing happens.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if canonicalising the view fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        #[derive(Serialize)]
        struct View<'a> {
            #[serde(with = "serde_bytes")]
            payload: &'a [u8],
            artifact: &'a ArtifactId,
            author: &'a Ed25519PublicKey,
            attestation_id: &'a AttestationId,
            op_class: &'a OpClass,
            timestamp: &'a SystemTime,
            seq: u64,
            encryption: &'a Option<EncryptionMetadata>,
        }
        canonical_bytes(&View {
            payload: &self.payload,
            artifact: &self.artifact,
            author: &self.author,
            attestation_id: &self.attestation_id,
            op_class: &self.op_class,
            timestamp: &self.timestamp,
            seq: self.seq,
            encryption: &self.encryption,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::from_canonical_bytes;
    use crate::crypto::generate_keypair;

    #[test]
    fn signed_op_round_trip() {
        let (_, pk) = generate_keypair();
        let op = SignedOp::new_unsigned(
            b"automerge-change-bytes".to_vec(),
            ArtifactId::new(),
            pk,
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            0,
        );
        let bytes = canonical_bytes(&op).unwrap();
        let back: SignedOp = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(op, back);
    }

    #[test]
    fn signed_op_with_encryption_round_trip() {
        let (_, pk) = generate_keypair();
        let mut op = SignedOp::new_unsigned_with_encryption(
            b"ciphertext-bytes".to_vec(),
            ArtifactId::new(),
            pk,
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            42,
            EncryptionMetadata {
                epoch: 3,
                channel: crate::channel::ChannelId::new(),
            },
        );
        op.signature = crate::crypto::Signature::from_bytes([1u8; 64]);
        let bytes = canonical_bytes(&op).unwrap();
        let back: SignedOp = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(op, back);
        assert!(back.encryption.is_some());
        assert_eq!(back.encryption.unwrap().epoch, 3);
    }

    #[test]
    fn encryption_metadata_changes_signing_bytes() {
        // Two ops differing only in `encryption` must NOT have matching
        // signing-bytes — otherwise a peer could strip the metadata to
        // redirect a ciphertext with the signature unchanged.
        let (_, pk) = generate_keypair();
        let cid = crate::channel::ChannelId::new();
        let a = SignedOp::new_unsigned(
            b"x".to_vec(),
            ArtifactId::new(),
            pk,
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            0,
        );
        let mut b = a.clone();
        b.encryption = Some(EncryptionMetadata {
            epoch: 0,
            channel: cid,
        });
        assert_ne!(a.signing_bytes().unwrap(), b.signing_bytes().unwrap());
    }

    #[test]
    fn signing_bytes_excludes_signature() {
        // Two ops differing only in signature must have identical
        // signing-bytes (so signing/verifying is well-defined).
        let (_, pk) = generate_keypair();
        let mut a = SignedOp::new_unsigned(
            b"x".to_vec(),
            ArtifactId::new(),
            pk,
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            7,
        );
        let mut b = a.clone();
        a.signature = Signature::from_bytes([0u8; 64]);
        b.signature = Signature::from_bytes([0xff; 64]);
        assert_eq!(a.signing_bytes().unwrap(), b.signing_bytes().unwrap());
    }
}
