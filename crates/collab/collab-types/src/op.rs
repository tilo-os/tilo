//! The signed-op envelope: `SignedOp`.
//!
//! Every CRDT mutation crossing a process or network boundary is wrapped
//! in a `SignedOp`, binding: the opaque payload (an Automerge change
//! blob); the artifact id it applies to; the author's identity public
//! key; the attestation id underwriting "this author may apply ops of
//! this class to this artifact"; a self-declared op class (cross-checked
//! against the author's grants); a wall-clock timestamp (checked against the
//! attestation's validity window); a monotonic per-author sequence
//! number (replay defence); and a detached Ed25519 signature over all of it.
//!
//! Metadata fields stay cleartext even once MLS encryption is layered
//! in — only `payload` is encrypted — so the broker can route by
//! artifact id and filter by op class without decrypting.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use web_time::SystemTime;

use crate::artifact::ArtifactId;
use crate::canonical::{CanonicalError, canonical_bytes};
use crate::crypto::{Ed25519PublicKey, Signature};
use crate::identity::AttestationId;
use crate::role::OpKind;

pub(crate) const DOCUMENT_MUTATION_V2_PREFIX: &[u8] = b"tilo-document-mutation-v2\0";
pub(crate) const DOCUMENT_SNAPSHOT_TAG: u8 = 1;
pub(crate) const DOCUMENT_INCREMENTAL_TAG: u8 = 2;
/// One piece of a mutation too large for a single frame; see
/// [`crate::document_transfer`].
pub(crate) const DOCUMENT_SEGMENT_TAG: u8 = 3;
/// Maximum Automerge bytes carried by one signed frame. Larger mutations are
/// split into segments (see [`crate::document_transfer`]) up to
/// [`crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES`].
pub const MAX_DOCUMENT_MUTATION_BYTES: usize = 3 * 1024 * 1024;
const MAX_ENCODED_DOCUMENT_MUTATION_BYTES: usize =
    DOCUMENT_MUTATION_V2_PREFIX.len() + crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES + 16;
const DOCUMENT_ARTIFACT_DOMAIN: &[u8] = b"tilo-document-artifact-v1\0";
const DOCUMENT_ATTESTATION_DOMAIN: &[u8] = b"tilo-document-attestation-v1\0";
const LIVE_ARTIFACT_DOMAIN: &[u8] = b"tilo-live-artifact-v1\0";

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
    /// The clear, signed artifact/attestation/encryption metadata does not
    /// bind the envelope to the collaboration route that carried it.
    #[error("signed document payload does not match its collaboration route")]
    RouteMismatch,
    /// Payload predates the typed document-mutation envelope.
    #[error("signed payload is not a typed document mutation")]
    MissingPrefix,
    /// A versioned envelope used a mutation kind this build does not know.
    #[error("typed document mutation has unknown kind tag {0}")]
    UnknownKind(u8),
    /// A v2 envelope ended before its required kind tag.
    #[error("typed document mutation is truncated")]
    Truncated,
    /// The payload exceeds the collaboration transport's per-mutation budget.
    #[error("document mutation is {actual} bytes; maximum is {max}")]
    TooLarge { actual: usize, max: usize },
    /// The payload is one segment of a larger mutation; it must be
    /// reassembled before it can be decoded as a complete mutation.
    #[error("payload is one segment of a larger document mutation")]
    Segmented,
    /// A segment header is inconsistent with the bytes it carries.
    #[error("document segment is invalid: {0}")]
    InvalidSegment(&'static str),
    /// The action commit carried inside the payload is not readable.
    #[error("workspace commit is invalid: {0}")]
    InvalidCommit(String),
}

/// Failure to encode a typed document mutation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DocumentMutationEncodeError {
    #[error("document mutation is {actual} bytes; maximum is {max}")]
    TooLarge { actual: usize, max: usize },
}

impl DocumentMutation {
    /// Visit the exact snapshot encoding without allocating a payload buffer.
    /// The visitor owns any destination allocation or digest state.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentMutationEncodeError::TooLarge`] when `bytes` exceed
    /// [`MAX_DOCUMENT_MUTATION_BYTES`].
    pub fn visit_snapshot_encoding(
        bytes: &[u8],
        mut write: impl FnMut(&[u8]),
    ) -> Result<(), DocumentMutationEncodeError> {
        if bytes.len() > MAX_DOCUMENT_MUTATION_BYTES {
            return Err(DocumentMutationEncodeError::TooLarge {
                actual: bytes.len(),
                max: MAX_DOCUMENT_MUTATION_BYTES,
            });
        }
        write(DOCUMENT_MUTATION_V2_PREFIX);
        write(&[DOCUMENT_SNAPSHOT_TAG]);
        write(bytes);
        Ok(())
    }

    /// Encode the kind and Automerge bytes into the payload carried by one
    /// [`SignedOp`]. Use [`Self::encode_frames`] for mutations that may
    /// exceed a single frame.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentMutationEncodeError::TooLarge`] when the Automerge
    /// bytes exceed [`MAX_DOCUMENT_MUTATION_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, DocumentMutationEncodeError> {
        let actual = self.data().len();
        if actual > MAX_DOCUMENT_MUTATION_BYTES {
            return Err(DocumentMutationEncodeError::TooLarge {
                actual,
                max: MAX_DOCUMENT_MUTATION_BYTES,
            });
        }
        let mut encoded = Vec::with_capacity(
            DOCUMENT_MUTATION_V2_PREFIX
                .len()
                .saturating_add(1)
                .saturating_add(actual),
        );
        encoded.extend_from_slice(DOCUMENT_MUTATION_V2_PREFIX);
        encoded.push(match self {
            Self::Snapshot(_) => DOCUMENT_SNAPSHOT_TAG,
            Self::Incremental(_) => DOCUMENT_INCREMENTAL_TAG,
        });
        encoded.extend_from_slice(self.data());
        Ok(encoded)
    }

    /// Decode a complete versioned document mutation, either a single frame
    /// or a payload reassembled from segments. Unprefixed payloads fail
    /// explicitly rather than being guessed to be an incremental update.
    /// Wire frames are checked with the tighter
    /// [`crate::document_transfer::DocumentFrame::decode`].
    ///
    /// # Errors
    ///
    /// Returns [`DocumentMutationError::MissingPrefix`] for unprefixed bytes,
    /// [`DocumentMutationError::Segmented`] for one segment of a larger
    /// mutation, or [`DocumentMutationError::UnknownKind`] /
    /// [`DocumentMutationError::Truncated`] for a corrupt typed body.
    pub fn decode(payload: &[u8]) -> Result<Self, DocumentMutationError> {
        // A frame that carries its action's commit is read as the frame
        // inside it: the commit is the envelope's business, not the
        // mutation's.
        let payload = match crate::WorkspaceCommit::split(payload) {
            Ok((inner, _)) => inner,
            Err(error) => return Err(DocumentMutationError::InvalidCommit(error.to_string())),
        };
        if payload.len() > MAX_ENCODED_DOCUMENT_MUTATION_BYTES {
            return Err(DocumentMutationError::TooLarge {
                actual: payload.len(),
                max: MAX_ENCODED_DOCUMENT_MUTATION_BYTES,
            });
        }
        let body = payload
            .strip_prefix(DOCUMENT_MUTATION_V2_PREFIX)
            .ok_or(DocumentMutationError::MissingPrefix)?;
        let (&kind, data) = body.split_first().ok_or(DocumentMutationError::Truncated)?;
        let mutation = match kind {
            DOCUMENT_SNAPSHOT_TAG => Self::Snapshot(data.to_vec()),
            DOCUMENT_INCREMENTAL_TAG => Self::Incremental(data.to_vec()),
            DOCUMENT_SEGMENT_TAG => return Err(DocumentMutationError::Segmented),
            other => return Err(DocumentMutationError::UnknownKind(other)),
        };
        let actual = mutation.data().len();
        if actual > crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES {
            return Err(DocumentMutationError::TooLarge {
                actual,
                max: crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES,
            });
        }
        Ok(mutation)
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

/// Artifact a sealed live frame is bound to: its workspace's content
/// key seals it for this kind, sequence and record only, so a frame cannot
/// be replayed under another sequence, record or kind.
#[must_use]
pub fn live_artifact_id(record_digest: &[u8; 32], kind: u8, seq: u64) -> ArtifactId {
    let mut hash = Sha256::new();
    hash.update(LIVE_ARTIFACT_DOMAIN);
    hash.update(record_digest);
    hash.update([kind]);
    hash.update(seq.to_le_bytes());
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    ArtifactId(uuid::Uuid::from_bytes(bytes))
}

static LAST_UNORDERED_DOCUMENT_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Sequence for a document write signed outside its record owner's
/// serialized turn: never below wall-clock microseconds and strictly
/// increasing within the process, so the owner's own dense `floor + 1`
/// allocations cannot take it first.
///
/// This is the one process-wide allocator for caller-signed document
/// writes; every SDK document signer and every out-of-turn test signer
/// draws from it.
#[must_use]
pub fn next_unordered_document_sequence(now: SystemTime) -> u64 {
    use std::sync::atomic::Ordering;

    let wall = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
        });
    let mut observed = LAST_UNORDERED_DOCUMENT_SEQUENCE.load(Ordering::Relaxed);
    loop {
        let candidate = wall.max(observed.saturating_add(1));
        match LAST_UNORDERED_DOCUMENT_SEQUENCE.compare_exchange_weak(
            observed,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return candidate,
            Err(actual) => observed = actual,
        }
    }
}

/// The route tag of one document: derived from its app, workspace and
/// record, so it stays stable across edits and scrollback or audit entries
/// never mint arbitrary ids per op.
#[must_use]
pub fn document_attestation_id(app_id: &str, workspace_id: &str, record_id: &str) -> AttestationId {
    AttestationId(derived_document_uuid(
        DOCUMENT_ATTESTATION_DOMAIN,
        app_id,
        workspace_id,
        record_id,
    ))
}

/// Result of validating the clear routing layer of a document envelope.
/// Brokers use this to distinguish a typed cleartext mutation from opaque MLS
/// ciphertext while applying identical route binding in both cases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutedDocumentPayload {
    Cleartext(DocumentMutation),
    /// One cleartext segment of a larger mutation. Brokers route it like any
    /// other validated envelope; endpoints reassemble it.
    CleartextSegment(crate::document_transfer::DocumentSegment),
    Encrypted {
        epoch: u64,
    },
}

/// Single-frame form used by durable admission and authoring state, which
/// never accepts a segment in place of a whole mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WholeDocumentPayload {
    Cleartext(DocumentMutation),
    Encrypted { epoch: u64 },
}

/// Validate the fields a broker is allowed to inspect for a document route.
///
/// Encrypted payloads are intentionally not decoded.  Their ciphertext must be
/// non-empty; authenticity of the sealed content remains an endpoint
/// responsibility.  Cleartext payloads retain the versioned typed-mutation
/// check.
///
/// # Errors
///
/// Returns [`DocumentMutationError::RouteMismatch`] when the signed routing
/// fields do not match, and forwards the typed mutation decode
/// error for malformed cleartext payloads.
pub fn validate_document_payload_route(
    app_id: &str,
    workspace_id: &str,
    record_id: &str,
    op: &SignedOp,
) -> Result<RoutedDocumentPayload, DocumentMutationError> {
    if op.artifact != document_artifact_id(app_id, workspace_id, record_id)
        || op.attestation_id != document_attestation_id(app_id, workspace_id, record_id)
    {
        return Err(DocumentMutationError::RouteMismatch);
    }
    // Segments only exist in the current format; brokers route them like any
    // other validated envelope and endpoints reassemble them.
    if op.encryption.is_none()
        && op
            .payload
            .strip_prefix(DOCUMENT_MUTATION_V2_PREFIX)
            .and_then(<[u8]>::first)
            == Some(&DOCUMENT_SEGMENT_TAG)
        && let crate::document_transfer::DocumentFrame::Segment(segment) =
            crate::document_transfer::DocumentFrame::decode(&op.payload)?
    {
        return Ok(RoutedDocumentPayload::CleartextSegment(segment));
    }
    match validate_whole_document_payload_route(app_id, workspace_id, record_id, op)? {
        WholeDocumentPayload::Cleartext(mutation) => Ok(RoutedDocumentPayload::Cleartext(mutation)),
        WholeDocumentPayload::Encrypted { epoch } => Ok(RoutedDocumentPayload::Encrypted { epoch }),
    }
}

/// Validate the route binding of a single-frame payload; clear segments are
/// refused.
///
/// # Errors
///
/// Returns [`DocumentMutationError::RouteMismatch`] when the signed metadata
/// does not bind the envelope to this route, or the decode error of a
/// malformed clear payload.
pub fn validate_whole_document_payload_route(
    app_id: &str,
    workspace_id: &str,
    record_id: &str,
    op: &SignedOp,
) -> Result<WholeDocumentPayload, DocumentMutationError> {
    if op.artifact != document_artifact_id(app_id, workspace_id, record_id)
        || op.attestation_id != document_attestation_id(app_id, workspace_id, record_id)
    {
        return Err(DocumentMutationError::RouteMismatch);
    }
    match op.encryption {
        None => {
            let mutation = DocumentMutation::decode(&op.payload)?;
            // One signed frame carries at most one frame's worth of data;
            // larger mutations travel as segments (see `document_transfer`).
            let actual = mutation.data().len();
            if actual > MAX_DOCUMENT_MUTATION_BYTES {
                return Err(DocumentMutationError::TooLarge {
                    actual,
                    max: MAX_DOCUMENT_MUTATION_BYTES,
                });
            }
            Ok(WholeDocumentPayload::Cleartext(mutation))
        }
        Some(meta) => {
            if op.payload.is_empty() {
                return Err(DocumentMutationError::RouteMismatch);
            }
            Ok(WholeDocumentPayload::Encrypted { epoch: meta.epoch })
        }
    }
}

fn derived_document_uuid(
    domain: &[u8],
    app_id: &str,
    workspace_id: &str,
    record_id: &str,
) -> uuid::Uuid {
    derived_uuid(domain, &[app_id, workspace_id, record_id])
}

/// The domain-separated id derived from `parts`: length-prefixed, so no two
/// tuples of parts hash alike, and fixed to `UUIDv8` so it reads as an id.
pub(crate) fn derived_uuid(domain: &[u8], parts: &[&str]) -> uuid::Uuid {
    let mut hash = Sha256::new();
    hash.update(domain);
    for part in parts {
        let part = part.as_bytes();
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

/// How an encrypted `SignedOp.payload` was sealed. Versioned so members
/// keep reading ciphertexts produced by earlier schemes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EncryptionScheme {
    /// Version 1: an MLS application message at `epoch`
    /// (`key_group::KeyGroup::encrypt`). Readable only by members
    /// holding that epoch's ratchet state, and only once per receiver.
    MlsApplication,
    /// Version 2: an XChaCha20-Poly1305 `tilo-seal` envelope under the
    /// workspace content key of `epoch`. Content keys are derived from the MLS
    /// exporter of each epoch and handed to joiners as a key history, so
    /// every member (including one added later) can read every op, any
    /// number of times. `retention` tells durable hosts how to store the op
    /// without decrypting it.
    ContentKey {
        /// Whether the op is a state baseline or an incremental change.
        retention: ContentRetention,
    },
}

/// Clear storage class of a content-key ciphertext, so relays and brokers
/// can retain encrypted history (latest baseline plus the changes after it)
/// without reading it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentRetention {
    /// A complete state baseline: a snapshot, or the first segment of one.
    Checkpoint,
    /// An incremental change, or a later segment of a snapshot.
    Delta,
}

/// Metadata describing how a `SignedOp.payload` was encrypted.
///
/// The presence of this field (not a separate wire flag) tells a
/// receiver the payload is ciphertext: `None` means `payload` is a
/// cleartext document frame; `Some(meta)` means `payload` was sealed under
/// its workspace's keys at `epoch` under `scheme`.
///
/// `SignedOp.signature` covers (cipher)bytes either way, and covers
/// `encryption` itself too, so a malicious peer can't strip the metadata or
/// downgrade the scheme.
///
/// Wire form: one tagged value — `0` none, `3` MLS application message
/// `{epoch}`, `4` content key `{epoch, retention}`. Tags `1` (`{epoch,
/// channel}`) and `2` (`{epoch, channel, retention}`) are the forms the 0.1
/// releases signed, when each record sealed under a channel of its own: they
/// are read with that channel kept ([`Self::released_channel`]), so the op
/// re-encodes to the bytes its signature covers, and never written for a new
/// op.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EncryptionMetadata {
    /// MLS epoch the ciphertext belongs to: the ratchet epoch for
    /// [`EncryptionScheme::MlsApplication`], the content-key epoch for
    /// [`EncryptionScheme::ContentKey`].
    pub epoch: u64,
    pub scheme: EncryptionScheme,
    /// The channel a 0.1 release sealed the op for, as it signed it; `None`
    /// for every op this build seals.
    pub released_channel: Option<uuid::Uuid>,
}

impl EncryptionMetadata {
    /// Metadata for a version-1 MLS application message.
    pub const fn mls_application(epoch: u64) -> Self {
        Self {
            epoch,
            scheme: EncryptionScheme::MlsApplication,
            released_channel: None,
        }
    }

    /// Metadata for a version-2 content-key envelope.
    pub const fn content_key(epoch: u64, retention: ContentRetention) -> Self {
        Self {
            epoch,
            scheme: EncryptionScheme::ContentKey { retention },
            released_channel: None,
        }
    }

    /// Storage class of a content-key ciphertext; `None` for schemes that
    /// cannot be retained (an MLS application message is single-use).
    pub const fn retention(&self) -> Option<ContentRetention> {
        match self.scheme {
            EncryptionScheme::MlsApplication => None,
            EncryptionScheme::ContentKey { retention } => Some(retention),
        }
    }
}

/// Serde adapter for `Option<EncryptionMetadata>`: its tagged wire form
/// (see [`EncryptionMetadata`]).
mod encryption_wire {
    use super::{ContentRetention, EncryptionMetadata, EncryptionScheme};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// The variants in tag order: the first three are the 0.1 releases'
    /// layout, byte for byte.
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    enum Wire {
        None,
        ReleasedMlsApplication {
            epoch: u64,
            channel: uuid::Uuid,
        },
        ReleasedContentKey {
            epoch: u64,
            channel: uuid::Uuid,
            retention: ContentRetention,
        },
        MlsApplication {
            epoch: u64,
        },
        ContentKey {
            epoch: u64,
            retention: ContentRetention,
        },
    }

    fn wire(value: Option<&EncryptionMetadata>) -> Wire {
        let Some(&EncryptionMetadata {
            epoch,
            scheme,
            released_channel,
        }) = value
        else {
            return Wire::None;
        };
        match (scheme, released_channel) {
            (EncryptionScheme::MlsApplication, Some(channel)) => {
                Wire::ReleasedMlsApplication { epoch, channel }
            }
            (EncryptionScheme::ContentKey { retention }, Some(channel)) => {
                Wire::ReleasedContentKey {
                    epoch,
                    channel,
                    retention,
                }
            }
            (EncryptionScheme::MlsApplication, None) => Wire::MlsApplication { epoch },
            (EncryptionScheme::ContentKey { retention }, None) => {
                Wire::ContentKey { epoch, retention }
            }
        }
    }

    #[allow(
        clippy::ref_option,
        reason = "serde's `with` adapters receive a reference to the field"
    )]
    pub(super) fn serialize<S: Serializer>(
        value: &Option<EncryptionMetadata>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        wire(value.as_ref()).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<EncryptionMetadata>, D::Error> {
        Ok(match Wire::deserialize(deserializer)? {
            Wire::None => None,
            Wire::ReleasedMlsApplication { epoch, channel } => Some(EncryptionMetadata {
                released_channel: Some(channel),
                ..EncryptionMetadata::mls_application(epoch)
            }),
            Wire::ReleasedContentKey {
                epoch,
                channel,
                retention,
            } => Some(EncryptionMetadata {
                released_channel: Some(channel),
                ..EncryptionMetadata::content_key(epoch, retention)
            }),
            Wire::MlsApplication { epoch } => Some(EncryptionMetadata::mls_application(epoch)),
            Wire::ContentKey { epoch, retention } => {
                Some(EncryptionMetadata::content_key(epoch, retention))
            }
        })
    }

    /// Borrowing wrapper so signing views serialize exactly like the field.
    pub(super) struct View<'a>(pub(super) &'a Option<EncryptionMetadata>);

    impl Serialize for View<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serialize(self.0, serializer)
        }
    }
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
    /// The author's identity public key. The signature must verify under it.
    pub author: Ed25519PublicKey,
    /// The route tag this op belongs to, derived from its app, workspace and
    /// record ([`document_attestation_id`]).
    pub attestation_id: AttestationId,
    /// The author's self-declared op class. A lying client doesn't gain
    /// capability: the change claims below decide every change against the
    /// author's grants, whatever the client declares here.
    pub kind: OpKind,
    /// Wall-clock time the op was created. Live traffic must be fresh; clock
    /// skew tolerance is the validator's responsibility.
    pub timestamp: SystemTime,
    /// Per-(author, workspace) monotonic sequence number. Replay defence: the
    /// validator rejects an inbound op whose `seq` isn't strictly greater
    /// than the last seen for that author. Independent per author.
    pub seq: u64,
    /// Encryption mode. `None` means `payload` is cleartext; `Some(meta)`
    /// names the epoch and versioned scheme. Part of the signed bytes so a
    /// peer can't rewrite the metadata to downgrade a ciphertext. See
    /// [`EncryptionMetadata`] for the wire form, which keeps every op the
    /// 0.1 releases signed decodable.
    #[serde(default, with = "encryption_wire")]
    pub encryption: Option<EncryptionMetadata>,
    /// What each change in `payload` claims to be: its action and the
    /// storage units it writes, one per change, signed with the op so a
    /// receiver (a relay that cannot read a sealed payload included) decides
    /// every change against the author's grants before merging it.
    pub claims: Vec<crate::claim::ChangeClaim>,
    /// The content hash (BLAKE3) of the app surface the op's changes are
    /// decided against: the `surface.toml` of the package that made them,
    /// as the launcher handed it to the app. A receiver fetches it by hash
    /// and holds the op until it has it.
    pub surface: Option<[u8; 32]>,
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
        kind: OpKind,
        timestamp: SystemTime,
        seq: u64,
    ) -> Self {
        Self {
            payload,
            artifact,
            author,
            attestation_id,
            kind,
            timestamp,
            seq,
            encryption: None,
            claims: Vec::new(),
            surface: None,
            signature: Signature::from_bytes([0u8; 64]),
        }
    }

    /// Construct an unsigned op carrying an MLS ciphertext payload. The
    /// caller has already encrypted via `KeyGroup::encrypt`; the
    /// ciphertext goes into `payload`, the matching `epoch` into
    /// `encryption`. Sign with [`crate::crypto::sign_op`] as usual.
    pub fn new_unsigned_with_encryption(
        payload: Vec<u8>,
        artifact: ArtifactId,
        author: Ed25519PublicKey,
        attestation_id: AttestationId,
        kind: OpKind,
        timestamp: SystemTime,
        seq: u64,
        encryption: EncryptionMetadata,
    ) -> Self {
        Self {
            encryption: Some(encryption),
            ..Self::new_unsigned(
                payload,
                artifact,
                author,
                attestation_id,
                kind,
                timestamp,
                seq,
            )
        }
    }

    /// The op claiming `claims` for its changes. Sign after setting them.
    #[must_use]
    pub fn with_claims(mut self, claims: Vec<crate::claim::ChangeClaim>) -> Self {
        self.claims = claims;
        self
    }

    /// The op deciding its changes against the app surface `surface`.
    #[must_use]
    pub fn with_surface(mut self, surface: Option<[u8; 32]>) -> Self {
        self.surface = surface;
        self
    }

    /// Whether this op is the whole record rather than a change to it: a
    /// compaction, which the key holder folded the record's sealed history
    /// into, or a checkpoint, the record as its writer saved it. It names
    /// the record on its own, so a replica holding none of the route founds
    /// it with this op, and a writer that founds a record sealed publishes
    /// one instead of a cleartext genesis a member, who reads only what the
    /// workspace's keys seal, could never account for.
    #[must_use]
    pub fn is_whole_record(&self) -> bool {
        self.kind == OpKind::Compaction
            || self.encryption.and_then(|metadata| metadata.retention())
                == Some(ContentRetention::Checkpoint)
    }

    /// Canonical bytes the author signs to mint this envelope — the struct
    /// minus `signature`, stable regardless of when signing happens.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        #[derive(Serialize)]
        struct View<'a> {
            #[serde(with = "serde_bytes")]
            payload: &'a [u8],
            artifact: &'a ArtifactId,
            author: &'a Ed25519PublicKey,
            attestation_id: &'a AttestationId,
            kind: &'a OpKind,
            timestamp: &'a SystemTime,
            seq: u64,
            encryption: encryption_wire::View<'a>,
            claims: &'a [crate::claim::ChangeClaim],
            surface: &'a Option<[u8; 32]>,
        }
        canonical_bytes(&View {
            payload: &self.payload,
            artifact: &self.artifact,
            author: &self.author,
            attestation_id: &self.attestation_id,
            kind: &self.kind,
            timestamp: &self.timestamp,
            seq: self.seq,
            encryption: encryption_wire::View(&self.encryption),
            claims: &self.claims,
            surface: &self.surface,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::from_canonical_bytes;

    /// The metadata as a signed op's field carries it.
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Wrapped(#[serde(with = "encryption_wire")] Option<EncryptionMetadata>);
    use crate::crypto::generate_keypair;

    /// The unordered document sequence never falls below wall-clock
    /// microseconds and strictly increases, even when the clock stands still
    /// or steps backwards.
    #[test]
    fn next_unordered_document_sequence_is_monotonic_and_at_least_wall_clock() {
        let now = SystemTime::now();
        let micros = u64::try_from(
            now.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_micros(),
        )
        .unwrap();
        let first = next_unordered_document_sequence(now);
        assert!(first >= micros, "{first} < wall clock {micros}");
        let second = next_unordered_document_sequence(now);
        assert!(second > first, "same instant must still advance");
        let earlier = next_unordered_document_sequence(SystemTime::UNIX_EPOCH);
        assert!(earlier > second, "a backwards clock must still advance");
    }

    tilo_test_env::scenario! {
        /// The first phase pins the field layout of the wire bytes by their digest.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "fixture arithmetic in test bodies, which the lint skips in a plain test"
        )]
        fn signed_op_wire_and_signing_bytes_cover_exactly_the_signed_fields;

        phase signed_op_field_layout;
        // Signed ops round-trip with and without encryption; encryption metadata changes the signing
        // bytes, which exclude the signature; version-one metadata keeps the legacy wire and signing
        // bytes; content-key metadata round-trips and is signed.
        // The signed envelope's field layout, as the wire and the signature see
        // it: a change here is a wire change every peer must make together.
        {
            use sha2::Digest as _;
            let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let mut op = SignedOp::new_unsigned(
                b"change".to_vec(),
                document_artifact_id("app", "room", "record"),
                Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                document_attestation_id("app", "room", "record"),
                OpKind::Change,
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
                3,
            )
            .with_claims(vec![crate::claim::ChangeClaim {
                change: [9; 32],
                action: crate::claim::ClaimedAction::Group(vec![
                    crate::claim::ClaimedAction::Edit {
                        kind: "k".into(),
                        path: vec![tilo_actions::PathStep::Field("f".into())],
                    },
                    crate::claim::ClaimedAction::Verb {
                        action: tilo_actions::ActionKey::new("app", "k", "v"),
                        item: None,
                    },
                ]),
                units: vec![Vec::new()],
            }])
            .with_surface(Some([5; 32]));
            crate::crypto::sign_op(&mut op, &key).unwrap();
            let bytes = canonical_bytes(&op).unwrap();
            assert_eq!(from_canonical_bytes::<SignedOp>(&bytes).unwrap(), op);
            assert_eq!(
                postcard::from_bytes::<SignedOp>(&postcard::to_allocvec(&op).unwrap()).unwrap(),
                op
            );
            assert_eq!(
                hex::encode(sha2::Sha256::digest(&bytes)),
                "4d36ec4cbbb005c0eb17855ec94b0d7ff48f64bd0dcd31ff82e1063b3366176d",
                "{}",
                hex::encode(&bytes)
            );
        }

        phase signed_op_round_trip;
        {
            let (_, pk) = generate_keypair();
            let op = SignedOp::new_unsigned(
                b"automerge-change-bytes".to_vec(),
                ArtifactId::new(),
                pk,
                AttestationId::new(),
                OpKind::Change,
                SystemTime::UNIX_EPOCH,
                0,
            );
            let bytes = canonical_bytes(&op).unwrap();
            let back: SignedOp = from_canonical_bytes(&bytes).unwrap();
            assert_eq!(op, back);
        }

        phase signed_op_with_encryption_round_trip;
        {
            let (_, pk) = generate_keypair();
            let mut op = SignedOp::new_unsigned_with_encryption(
                b"ciphertext-bytes".to_vec(),
                ArtifactId::new(),
                pk,
                AttestationId::new(),
                OpKind::Change,
                SystemTime::UNIX_EPOCH,
                42,
                EncryptionMetadata::mls_application(3),
            );
            op.signature = crate::crypto::Signature::from_bytes([1u8; 64]);
            let bytes = canonical_bytes(&op).unwrap();
            let back: SignedOp = from_canonical_bytes(&bytes).unwrap();
            assert_eq!(op, back);
            assert!(back.encryption.is_some());
            assert_eq!(back.encryption.unwrap().epoch, 3);
        }

        phase encryption_metadata_changes_signing_bytes;
        {
            // Two ops differing only in `encryption` must NOT have matching
            // signing-bytes — otherwise a peer could strip the metadata to
            // redirect a ciphertext with the signature unchanged.
            let (_, pk) = generate_keypair();
            let a = SignedOp::new_unsigned(
                b"x".to_vec(),
                ArtifactId::new(),
                pk,
                AttestationId::new(),
                OpKind::Change,
                SystemTime::UNIX_EPOCH,
                0,
            );
            let mut b = a.clone();
            b.encryption = Some(EncryptionMetadata::mls_application(0));
            assert_ne!(a.signing_bytes().unwrap(), b.signing_bytes().unwrap());
        }

        phase signing_bytes_excludes_signature;
        {
            // Two ops differing only in signature must have identical
            // signing-bytes (so signing/verifying is well-defined).
            let (_, pk) = generate_keypair();
            let mut a = SignedOp::new_unsigned(
                b"x".to_vec(),
                ArtifactId::new(),
                pk,
                AttestationId::new(),
                OpKind::Change,
                SystemTime::UNIX_EPOCH,
                7,
            );
            let mut b = a.clone();
            a.signature = Signature::from_bytes([0u8; 64]);
            b.signature = Signature::from_bytes([0xff; 64]);
            assert_eq!(a.signing_bytes().unwrap(), b.signing_bytes().unwrap());
        }

        phase metadata_a_release_signed_keeps_its_bytes;
        {
            // The forms the 0.1 releases wrote: `Option<{epoch, channel}>`
            // for an MLS application message, the third tag for a content
            // key. Each reads with its channel and writes the same bytes.
            #[derive(Serialize)]
            struct Released {
                epoch: u64,
                channel: uuid::Uuid,
            }
            let channel = uuid::Uuid::from_u128(9);
            let mls = canonical_bytes(&Some(Released { epoch: 9, channel })).unwrap();
            let read: Wrapped = from_canonical_bytes(&mls).unwrap();
            assert_eq!(
                read.0,
                Some(EncryptionMetadata {
                    released_channel: Some(channel),
                    ..EncryptionMetadata::mls_application(9)
                })
            );
            assert_eq!(canonical_bytes(&encryption_wire::View(&read.0)).unwrap(), mls);
            let content = [&[2_u8, 9, 16][..], channel.as_bytes(), &[1]].concat();
            let read: Wrapped = from_canonical_bytes(&content).unwrap();
            assert_eq!(
                read.0,
                Some(EncryptionMetadata {
                    released_channel: Some(channel),
                    ..EncryptionMetadata::content_key(9, ContentRetention::Delta)
                })
            );
            assert_eq!(canonical_bytes(&encryption_wire::View(&read.0)).unwrap(), content);
            // This build's forms carry no channel, under tags of their own.
            let ours = canonical_bytes(&encryption_wire::View(&Some(
                EncryptionMetadata::content_key(9, ContentRetention::Delta),
            )))
            .unwrap();
            assert_eq!(ours, [4, 9, 1]);
        }

        phase content_key_metadata_round_trips_and_is_signed;
        {
            let (_, pk) = generate_keypair();
            let mut op = SignedOp::new_unsigned_with_encryption(
                b"sealed".to_vec(),
                ArtifactId::new(),
                pk,
                AttestationId::new(),
                OpKind::Change,
                SystemTime::UNIX_EPOCH,
                5,
                EncryptionMetadata::content_key(2, ContentRetention::Checkpoint),
            );
            op.signature = Signature::from_bytes([3; 64]);
            let back: SignedOp = from_canonical_bytes(&canonical_bytes(&op).unwrap()).unwrap();
            assert_eq!(back, op);
            assert_eq!(
                back.encryption.unwrap().retention(),
                Some(ContentRetention::Checkpoint)
            );
            let mut downgraded = op.clone();
            downgraded.encryption = Some(EncryptionMetadata::mls_application(2));
            assert_ne!(
                op.signing_bytes().unwrap(),
                downgraded.signing_bytes().unwrap()
            );
            let mut reclassified = op.clone();
            reclassified.encryption =
                Some(EncryptionMetadata::content_key(2, ContentRetention::Delta));
            assert_ne!(
                op.signing_bytes().unwrap(),
                reclassified.signing_bytes().unwrap()
            );
        }
    }

    tilo_test_env::scenario! {
        /// Typed document mutations preserve kind and bytes and enforce the wire budget before decode.
        fn typed_document_mutations_keep_kind_and_bytes_within_the_wire_budget;

        phase typed_document_mutations_preserve_kind_and_bytes;
        {
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

            let encoded = DocumentMutation::Snapshot(vec![7, 8]).encode().unwrap();
            assert_eq!(
                &encoded[..DOCUMENT_MUTATION_V2_PREFIX.len()],
                DOCUMENT_MUTATION_V2_PREFIX
            );
            assert_eq!(
                encoded[DOCUMENT_MUTATION_V2_PREFIX.len()],
                DOCUMENT_SNAPSHOT_TAG,
                "the signed wire kind is explicit rather than serde enum order"
            );
        }

        phase typed_document_mutations_enforce_wire_budget_before_decode;
        {
            let oversized = DocumentMutation::Incremental(vec![
                0;
                MAX_DOCUMENT_MUTATION_BYTES
                    .saturating_add(1)
            ]);
            assert!(matches!(
                oversized.encode(),
                Err(DocumentMutationEncodeError::TooLarge { actual, max })
                    if actual == MAX_DOCUMENT_MUTATION_BYTES.saturating_add(1)
                        && max == MAX_DOCUMENT_MUTATION_BYTES
            ));

            let oversized_wire = vec![0; MAX_ENCODED_DOCUMENT_MUTATION_BYTES.saturating_add(1)];
            assert!(matches!(
                DocumentMutation::decode(&oversized_wire),
                Err(DocumentMutationError::TooLarge { actual, max })
                    if actual == oversized_wire.len() && max == MAX_ENCODED_DOCUMENT_MUTATION_BYTES
            ));

            // A single signed frame above the frame limit is refused by the
            // routing layer even though a reassembled payload of that size is a
            // valid endpoint mutation.
            let mut frame = DOCUMENT_MUTATION_V2_PREFIX.to_vec();
            frame.push(DOCUMENT_INCREMENTAL_TAG);
            frame.extend(vec![0; MAX_DOCUMENT_MUTATION_BYTES.saturating_add(1)]);
            let (app, workspace, record) = ("notes", "workspace", "record");
            let op = SignedOp::new_unsigned(
                frame,
                document_artifact_id(app, workspace, record),
                Ed25519PublicKey::from_bytes([7; 32]),
                document_attestation_id(app, workspace, record),
                OpKind::Change,
                SystemTime::UNIX_EPOCH,
                1,
            );
            assert!(matches!(
                validate_document_payload_route(app, workspace, record, &op),
                Err(DocumentMutationError::TooLarge { .. })
            ));
        }
    }

    tilo_test_env::scenario! {
        /// Document route ids are stable and unambiguous; an encrypted route is bound without decoding
        /// ciphertext and rejects empty ciphertext.
        fn document_routes_are_stable_and_bound_without_decoding;

        phase document_route_ids_are_stable_and_unambiguous;
        {
            let first = document_artifact_id("so.tilo.notes", "room-a", "record-b");
            assert_eq!(
                first,
                document_artifact_id("so.tilo.notes", "room-a", "record-b")
            );
            assert_ne!(
                first,
                document_artifact_id("so.tilo.notes", "room-ar", "ecord-b")
            );
            assert_ne!(
                first.0,
                document_attestation_id("so.tilo.notes", "room-a", "record-b").0,
                "artifact and attestation domains must not alias"
            );
            assert_eq!(first.0.get_version_num(), 8);
            assert_eq!(first.0.to_string(), "0231a3fb-e6d4-89fc-9fc4-160d44d242bf");
            assert_eq!(
                document_artifact_id("dev.tilo.notes", "room-a", "record-b")
                    .0
                    .to_string(),
                "fd9e9e68-54c3-8ae2-bfc6-cb06b0816e07"
            );
            assert_eq!(
                document_attestation_id("dev.tilo.notes", "room-a", "record-b")
                    .0
                    .to_string(),
                "fe8b6b90-8390-8f45-923c-e78d2a15cb9c"
            );
            let golden = DocumentMutation::Snapshot(vec![0, 1, 2, 0xff]);
            assert_eq!(
                golden.encode().unwrap(),
                b"tilo-document-mutation-v2\0\x01\0\x01\x02\xff"
            );
        }

        phase encrypted_document_route_is_bound_without_decoding_ciphertext;
        {
            let (app, workspace, record) = ("notes", "workspace", "meeting");
            let op = SignedOp::new_unsigned_with_encryption(
                vec![0xde, 0xad, 0xbe, 0xef],
                document_artifact_id(app, workspace, record),
                Ed25519PublicKey::from_bytes([7; 32]),
                document_attestation_id(app, workspace, record),
                OpKind::Change,
                SystemTime::UNIX_EPOCH,
                1,
                EncryptionMetadata::mls_application(4),
            );
            assert_eq!(
                validate_document_payload_route(app, workspace, record, &op),
                Ok(RoutedDocumentPayload::Encrypted { epoch: 4 })
            );
            assert_eq!(
                validate_document_payload_route(app, workspace, "other", &op),
                Err(DocumentMutationError::RouteMismatch)
            );
        }

        phase encrypted_document_route_rejects_empty_ciphertext;
        {
            let (app, workspace, record) = ("notes", "workspace", "record");
            let build = |payload| {
                SignedOp::new_unsigned_with_encryption(
                    payload,
                    document_artifact_id(app, workspace, record),
                    Ed25519PublicKey::from_bytes([8; 32]),
                    document_attestation_id(app, workspace, record),
                    OpKind::Change,
                    SystemTime::UNIX_EPOCH,
                    1,
                    EncryptionMetadata::mls_application(0),
                )
            };
            let empty = build(Vec::new());
            assert_eq!(
                validate_document_payload_route(app, workspace, record, &empty),
                Err(DocumentMutationError::RouteMismatch)
            );
        }
    }
}
