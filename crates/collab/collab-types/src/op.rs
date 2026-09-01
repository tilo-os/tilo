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
use sha2::{Digest, Sha256};
use std::time::SystemTime;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::channel::{ArtifactId, ChannelId};
use crate::crypto::{Ed25519PublicKey, Signature};
use crate::identity::AttestationId;
use crate::role::OpClass;

const DOCUMENT_MUTATION_PREFIX: &[u8] = b"qos-document-mutation-v1\0";
const DOCUMENT_ARTIFACT_DOMAIN: &[u8] = b"qos-document-artifact-v1\0";
const DOCUMENT_ATTESTATION_DOMAIN: &[u8] = b"qos-document-attestation-v1\0";

/// Meaning of a signed document payload. Keeping this tag inside
/// [`SignedOp::payload`] makes snapshot-vs-incremental part of the bytes the
/// device signs without changing the persisted `SignedOp` layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum DocumentMutation {
    /// A complete Automerge document used to seed a late joiner.
    Snapshot(#[serde(with = "serde_bytes")] Vec<u8>),
    /// One Automerge incremental change set.
    Incremental(#[serde(with = "serde_bytes")] Vec<u8>),
}

/// Failure to decode a typed document mutation from a signed payload.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DocumentMutationError {
    /// Payload predates the typed document-mutation envelope.
    #[error("signed payload is not a typed document mutation")]
    MissingPrefix,
    /// The versioned payload body is malformed.
    #[error("typed document mutation is malformed: {0}")]
    Malformed(postcard::Error),
}

impl DocumentMutation {
    /// Encode the kind and Automerge bytes into the payload carried by a
    /// [`SignedOp`].
    ///
    /// # Errors
    ///
    /// Returns postcard's serialisation error if the mutation cannot be
    /// encoded.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        let body = postcard::to_allocvec(self)?;
        let mut encoded =
            Vec::with_capacity(DOCUMENT_MUTATION_PREFIX.len().saturating_add(body.len()));
        encoded.extend_from_slice(DOCUMENT_MUTATION_PREFIX);
        encoded.extend_from_slice(&body);
        Ok(encoded)
    }

    /// Decode a versioned document mutation. Raw pre-migration payloads fail
    /// explicitly rather than being guessed to be an incremental update.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentMutationError::MissingPrefix`] for legacy raw bytes,
    /// or [`DocumentMutationError::Malformed`] for a corrupt typed body.
    pub fn decode(payload: &[u8]) -> Result<Self, DocumentMutationError> {
        let body = payload
            .strip_prefix(DOCUMENT_MUTATION_PREFIX)
            .ok_or(DocumentMutationError::MissingPrefix)?;
        postcard::from_bytes(body).map_err(DocumentMutationError::Malformed)
    }

    /// Borrow the underlying Automerge bytes independent of mutation kind.
    pub fn data(&self) -> &[u8] {
        match self {
            Self::Snapshot(data) | Self::Incremental(data) => data,
        }
    }
}

/// Deterministically derive the policy artifact for one collaboration route.
/// Length-prefixing prevents ambiguous concatenations; domain separation keeps
/// these ids distinct from user-minted UUIDs and other derived identifiers.
#[must_use]
pub fn document_artifact_id(app_id: &str, workspace_id: &str, record_id: &str) -> ArtifactId {
    ArtifactId(derived_document_uuid(
        DOCUMENT_ARTIFACT_DOMAIN,
        app_id,
        workspace_id,
        record_id,
    ))
}

/// Deterministic attestation slot used by certified workspace admission.
/// The admitted validator does not trust this id by itself; it remains stable
/// so scrollback and audit entries do not mint arbitrary ids per edit.
#[must_use]
pub fn document_attestation_id(app_id: &str, workspace_id: &str, record_id: &str) -> AttestationId {
    AttestationId(derived_document_uuid(
        DOCUMENT_ATTESTATION_DOMAIN,
        app_id,
        workspace_id,
        record_id,
    ))
}

fn derived_document_uuid(
    domain: &[u8],
    app_id: &str,
    workspace_id: &str,
    record_id: &str,
) -> uuid::Uuid {
    let mut hash = Sha256::new();
    hash.update(domain);
    for part in [
        app_id.as_bytes(),
        workspace_id.as_bytes(),
        record_id.as_bytes(),
    ] {
        hash.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hash.update(part);
    }
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    // RFC 9562 UUIDv8 reserves the payload bits for application-defined
    // derivations such as this domain-separated SHA-256 digest.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

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

    #[test]
    fn typed_document_mutations_preserve_kind_and_bytes() {
        for mutation in [
            DocumentMutation::Snapshot(vec![0, 1, 2]),
            DocumentMutation::Incremental(vec![3, 4, 5]),
        ] {
            let encoded = mutation.encode().unwrap();
            let decoded = DocumentMutation::decode(&encoded).unwrap();
            assert_eq!(decoded, mutation);
            assert_eq!(decoded.data(), mutation.data());
        }
        assert_eq!(
            DocumentMutation::decode(b"raw legacy automerge bytes"),
            Err(DocumentMutationError::MissingPrefix)
        );
    }

    #[test]
    fn document_route_ids_are_stable_and_unambiguous() {
        let first = document_artifact_id("dev.qos.notes", "room-a", "record-b");
        assert_eq!(
            first,
            document_artifact_id("dev.qos.notes", "room-a", "record-b")
        );
        assert_ne!(
            first,
            document_artifact_id("dev.qos.notes", "room-ar", "ecord-b")
        );
        assert_ne!(
            first.0,
            document_attestation_id("dev.qos.notes", "room-a", "record-b").0,
            "artifact and attestation domains must not alias"
        );
        assert_eq!(first.0.get_version_num(), 8);
    }
}
