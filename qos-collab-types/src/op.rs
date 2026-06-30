//! The signed-op envelope: `SignedOp`.
//!
//! Every CRDT mutation that crosses a process or network boundary
//! (local broker → other apps, broker → libp2p peers, device A →
//! device B of the same user) is wrapped in a `SignedOp`. The
//! envelope binds:
//!
//! - the opaque payload (an Automerge change blob);
//! - the artifact id the payload applies to;
//! - the author's identity public key;
//! - the attestation id underwriting "this author may apply ops of
//!   this class to this artifact";
//! - a self-declared op class (which the validator cross-checks
//!   against the role table);
//! - a wall-clock timestamp (checked against the attestation's
//!   validity window);
//! - a monotonic per-author sequence number (replay defence);
//! - a detached Ed25519 signature over all the above fields.
//!
//! The metadata fields stay cleartext even after MLS encryption is
//! layered in Phase 7; only the `payload` is encrypted. This is
//! deliberate — the broker needs to route by artifact id and
//! filter by op class without decrypting.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::channel::{ArtifactId, ChannelId};
use crate::crypto::{Ed25519PublicKey, Signature};
use crate::identity::AttestationId;
use crate::role::OpClass;

/// Metadata describing how a `SignedOp.payload` was encrypted.
///
/// Added in Phase 7 (group encryption with MLS). The presence of
/// this field, not a separate wire flag, is what tells a receiver
/// the payload is ciphertext rather than cleartext:
///
/// - `encryption == None` → `payload` is a cleartext Automerge change
///   blob, as in Phases 2-6. Legacy and unencrypted channels keep
///   working in this mode.
/// - `encryption == Some(_)` → `payload` is an MLS ciphertext from
///   `qos_channel_crypto::ChannelGroup::encrypt`. The receiver looks
///   up the `ChannelGroup` for `channel`, checks `epoch` is
///   reachable, and calls `decrypt`. The cleartext is then handed
///   to the existing pipeline as if it had arrived unencrypted.
///
/// The `SignedOp.signature` covers (cipher)bytes either way: tampering
/// with the payload after signing breaks the signature regardless of
/// the encryption mode. The `encryption` field is also covered by
/// the signature so a malicious peer cannot strip the metadata and
/// re-route the ciphertext to a different channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptionMetadata {
    /// MLS epoch counter the ciphertext was produced under. Mirrors
    /// `openmls::group::MlsGroup::epoch().as_u64()` at the moment
    /// `encrypt` was called. A receiver whose local `ChannelGroup`
    /// is at an earlier epoch than `epoch` cannot decrypt and drops
    /// the message until it catches up (or, with openmls's default
    /// out-of-order tolerance, it accepts older-than-current
    /// ciphertexts up to a window).
    pub epoch: u64,
    /// Channel id the ciphertext belongs to. Redundant with the
    /// artifact→channel lookup the broker performs anyway, but
    /// cheap (a 16-byte UUID) and lets the receiver pick the
    /// `ChannelGroup` to ask for decryption without consulting any
    /// other table.
    pub channel: ChannelId,
}

/// One signed CRDT mutation.
///
/// The struct is `Clone` because the broker fans an op to multiple
/// peers, and `PartialEq` so tests can assert envelope identity.
/// It deliberately does *not* implement `Hash` — we don't want
/// envelopes used as map keys; use the signature bytes if a stable
/// key is needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedOp {
    /// The opaque payload bytes. In Phase 2 this is an Automerge
    /// change blob; in Phase 7+ this is MLS-encrypted ciphertext
    /// of the same. The validator does not interpret this field —
    /// it is opaque from the envelope's perspective.
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    /// The artifact this op mutates.
    pub artifact: ArtifactId,
    /// The author's identity public key. Must equal
    /// `attestation.subject` (validator check 4).
    pub author: Ed25519PublicKey,
    /// Id of the attestation underwriting this op. Resolved
    /// against the channel's attestation store by the validator.
    pub attestation_id: AttestationId,
    /// The author's self-declared op class. Cross-checked against
    /// the role table (validator check 5). A lying client doesn't
    /// gain capability: the validator looks up
    /// `role_permits(attestation.role, artifact.kind, op_class)`
    /// and that has to be true regardless of what the client says.
    pub op_class: OpClass,
    /// Wall-clock time the op was created. Checked to fall inside
    /// `[attestation.valid_from, attestation.valid_until]`
    /// (validator check 6). Clock skew tolerance is the
    /// validator's responsibility.
    pub timestamp: SystemTime,
    /// Per-(author, channel) monotonic sequence number. Replay
    /// defence: the validator rejects an inbound op whose `seq`
    /// is not strictly greater than the last seen for the same
    /// author (validator check 7). Different authors maintain
    /// independent sequences.
    pub seq: u64,
    /// Encryption mode. `None` is the Phase 2-6 legacy form
    /// (`payload` is cleartext). `Some(meta)` is the Phase 7+ form
    /// (`payload` is `qos-channel-crypto` ciphertext). The field is
    /// part of the signed bytes so a peer cannot rewrite the
    /// metadata to mis-route a ciphertext.
    ///
    /// Default-`None` is intentional: existing tests built before
    /// Phase 7 keep their canonical-bytes stability because
    /// `Option<T>` postcards as a single discriminant byte + the
    /// optional payload. The serialised tail of an unencrypted op
    /// now carries one extra `0x00` byte compared to pre-Phase-7
    /// channels — accepted as a wire bump for this phase.
    #[serde(default)]
    pub encryption: Option<EncryptionMetadata>,
    /// Detached Ed25519 signature by `author` over the
    /// canonical-bytes view of every other field of this struct.
    /// See `SignedOp::signing_bytes`.
    pub signature: Signature,
}

impl SignedOp {
    /// Build a fresh op envelope with a placeholder signature.
    /// Pass the result to [`crate::crypto::sign_op`] to stamp in
    /// the real signature in-place before sending.
    ///
    /// Defaults the `encryption` field to `None`. For an encrypted
    /// op, mutate the field before signing — see
    /// [`SignedOp::new_unsigned_with_encryption`] for a one-shot
    /// helper.
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

    /// Construct an unsigned op carrying an MLS ciphertext payload.
    ///
    /// The caller has already encrypted the cleartext through a
    /// `qos_channel_crypto::ChannelGroup::encrypt` call; the
    /// ciphertext bytes go into `payload` and the matching `epoch`
    /// + `channel` metadata into `encryption`. After construction,
    /// stamp the signature with [`crate::crypto::sign_op`] as usual.
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

    /// Canonical bytes the author signs to mint this envelope.
    ///
    /// Mirrors the struct minus the `signature` field. Signing the
    /// envelope after construction stamps the signature into the
    /// last field; the `signing_bytes` view is stable regardless.
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
        // Two ops differing only in `encryption` must NOT have
        // matching signing-bytes — otherwise a peer could strip the
        // metadata to redirect a ciphertext, with the signature
        // unchanged.
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
