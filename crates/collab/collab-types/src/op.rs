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
use web_time::SystemTime;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::channel::{ArtifactId, ChannelId};
use crate::crypto::{Ed25519PublicKey, Signature};
use crate::identity::AttestationId;
use crate::role::OpClass;

pub(crate) const DOCUMENT_MUTATION_V1_PREFIX: &[u8] = b"tilo-document-mutation-v1\0";
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
const QOS_DOCUMENT_MUTATION_V1_PREFIX: &[u8] = b"qos-document-mutation-v1\0";
const QOS_DOCUMENT_MUTATION_V2_PREFIX: &[u8] = b"qos-document-mutation-v2\0";
const QOS_DOCUMENT_ARTIFACT_DOMAIN: &[u8] = b"qos-document-artifact-v1\0";
const QOS_DOCUMENT_ATTESTATION_DOMAIN: &[u8] = b"qos-document-attestation-v1\0";
const QOS_DOCUMENT_CHANNEL_DOMAIN: &[u8] = b"qos-document-channel-v1\0";
const DOCUMENT_ARTIFACT_DOMAIN: &[u8] = b"tilo-document-artifact-v1\0";
const DOCUMENT_ATTESTATION_DOMAIN: &[u8] = b"tilo-document-attestation-v1\0";
const DOCUMENT_CHANNEL_DOMAIN: &[u8] = b"tilo-document-channel-v1\0";

/// Closed set of document mutation encodings that have shipped on durable
/// media. New mutations use [`Self::TiloV2`]; the other variants exist only so
/// an authenticated existing route can be read and continued byte-for-byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentWireVersion {
    QosV1,
    QosV2,
    TiloV1,
    TiloV2,
}

impl DocumentWireVersion {
    pub const CURRENT: Self = Self::TiloV2;
    pub const ALL: [Self; 4] = [Self::QosV1, Self::QosV2, Self::TiloV1, Self::TiloV2];

    fn prefix(self) -> &'static [u8] {
        match self {
            Self::QosV1 => QOS_DOCUMENT_MUTATION_V1_PREFIX,
            Self::QosV2 => QOS_DOCUMENT_MUTATION_V2_PREFIX,
            Self::TiloV1 => DOCUMENT_MUTATION_V1_PREFIX,
            Self::TiloV2 => DOCUMENT_MUTATION_V2_PREFIX,
        }
    }

    fn is_v1(self) -> bool {
        matches!(self, Self::QosV1 | Self::TiloV1)
    }

    fn domain(self) -> DocumentRouteDomain {
        match self {
            Self::QosV1 | Self::QosV2 => DocumentRouteDomain::QosV1,
            Self::TiloV1 | Self::TiloV2 => DocumentRouteDomain::TiloV1,
        }
    }

    #[must_use]
    pub fn shares_route_namespace(self, other: Self) -> bool {
        self.domain() == other.domain()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentRouteDomain {
    QosV1,
    TiloV1,
}

impl DocumentRouteDomain {
    fn artifact_domain(self) -> &'static [u8] {
        match self {
            Self::QosV1 => QOS_DOCUMENT_ARTIFACT_DOMAIN,
            Self::TiloV1 => DOCUMENT_ARTIFACT_DOMAIN,
        }
    }

    fn attestation_domain(self) -> &'static [u8] {
        match self {
            Self::QosV1 => QOS_DOCUMENT_ATTESTATION_DOMAIN,
            Self::TiloV1 => DOCUMENT_ATTESTATION_DOMAIN,
        }
    }

    fn channel_domain(self) -> &'static [u8] {
        match self {
            Self::QosV1 => QOS_DOCUMENT_CHANNEL_DOMAIN,
            Self::TiloV1 => DOCUMENT_CHANNEL_DOMAIN,
        }
    }
}

/// Meaning of a signed document payload. Keeping this tag inside
/// [`SignedOp::payload`] makes snapshot-vs-incremental part of the bytes the
/// device signs without changing the persisted `SignedOp` layout.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum DocumentMutation {
    /// A complete Automerge document used to seed a late joiner.
    Snapshot(#[serde(with = "serde_bytes")] Vec<u8>),
    /// One Automerge incremental change set.
    Incremental(#[serde(with = "serde_bytes")] Vec<u8>),
}

// One serializer for both the owned wire DTO and borrowed streaming output.
// Keep the historical enum name, variant names/indices and serde_bytes body.
#[derive(Serialize)]
#[serde(rename = "DocumentMutation")]
enum MutationRef<'a> {
    Snapshot(#[serde(with = "serde_bytes")] &'a [u8]),
    Incremental(#[serde(with = "serde_bytes")] &'a [u8]),
}
impl Serialize for DocumentMutation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Snapshot(bytes) => MutationRef::Snapshot(bytes).serialize(serializer),
            Self::Incremental(bytes) => MutationRef::Incremental(bytes).serialize(serializer),
        }
    }
}
struct SnapshotSink<F>(F);
impl<F: FnMut(&[u8])> postcard::ser_flavors::Flavor for SnapshotSink<F> {
    type Output = ();
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        (self.0)(&[byte]);
        Ok(())
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        (self.0)(bytes);
        Ok(())
    }
    fn finalize(self) -> postcard::Result<()> {
        Ok(())
    }
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
    /// The versioned payload body is malformed.
    #[error("typed document mutation is malformed: {0}")]
    Malformed(postcard::Error),
    /// The body decoded only as a prefix or through a non-canonical encoding.
    #[error("typed document mutation is not canonically encoded")]
    NonCanonical,
}

/// Failure to encode a typed document mutation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DocumentMutationEncodeError {
    #[error("document mutation is {actual} bytes; maximum is {max}")]
    TooLarge { actual: usize, max: usize },
    #[error("document mutation could not be encoded")]
    Serialization,
}

/// A decoded mutation together with the exact durable format that carried it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedDocumentMutation {
    pub mutation: DocumentMutation,
    pub version: DocumentWireVersion,
}

impl DocumentMutation {
    /// Visit the exact snapshot encoding without allocating a payload buffer.
    /// The visitor owns any destination allocation or digest state.
    pub fn visit_snapshot_encoding(
        bytes: &[u8],
        version: DocumentWireVersion,
        mut write: impl FnMut(&[u8]),
    ) -> Result<(), DocumentMutationEncodeError> {
        if bytes.len() > MAX_DOCUMENT_MUTATION_BYTES {
            return Err(DocumentMutationEncodeError::TooLarge {
                actual: bytes.len(),
                max: MAX_DOCUMENT_MUTATION_BYTES,
            });
        }
        write(version.prefix());
        if version.is_v1() {
            postcard::serialize_with_flavor(&MutationRef::Snapshot(bytes), SnapshotSink(write))
                .map_err(|_| DocumentMutationEncodeError::Serialization)
        } else {
            write(&[DOCUMENT_SNAPSHOT_TAG]);
            write(bytes);
            Ok(())
        }
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
        self.encode_for(DocumentWireVersion::CURRENT)
    }

    /// Encode using an explicitly authenticated durable format. Callers must
    /// not choose a legacy value from an app id or an unverified payload.
    pub fn encode_for(
        &self,
        version: DocumentWireVersion,
    ) -> Result<Vec<u8>, DocumentMutationEncodeError> {
        let actual = self.data().len();
        if actual > MAX_DOCUMENT_MUTATION_BYTES {
            return Err(DocumentMutationEncodeError::TooLarge {
                actual,
                max: MAX_DOCUMENT_MUTATION_BYTES,
            });
        }
        let mut encoded = Vec::with_capacity(version.prefix().len().saturating_add(actual + 8));
        encoded.extend_from_slice(version.prefix());
        if version.is_v1() {
            encoded.extend(
                postcard::to_allocvec(self)
                    .map_err(|_| DocumentMutationEncodeError::Serialization)?,
            );
        } else {
            encoded.push(match self {
                Self::Snapshot(_) => DOCUMENT_SNAPSHOT_TAG,
                Self::Incremental(_) => DOCUMENT_INCREMENTAL_TAG,
            });
            encoded.extend_from_slice(self.data());
        }
        Ok(encoded)
    }

    /// Decode a complete versioned document mutation, either a single frame
    /// or a payload reassembled from segments. Raw pre-migration payloads
    /// fail explicitly rather than being guessed to be an incremental update.
    /// Wire frames are checked with the tighter
    /// [`crate::document_transfer::DocumentFrame::decode`].
    ///
    /// # Errors
    ///
    /// Returns [`DocumentMutationError::MissingPrefix`] for legacy raw bytes,
    /// [`DocumentMutationError::Segmented`] for one segment of a larger
    /// mutation, or [`DocumentMutationError::Malformed`] for a corrupt typed
    /// body.
    pub fn decode(payload: &[u8]) -> Result<Self, DocumentMutationError> {
        Ok(Self::decode_versioned(payload)?.mutation)
    }

    /// Decode one of the four explicitly supported durable encodings and
    /// retain its exact version for route binding and later re-emission.
    pub fn decode_versioned(
        payload: &[u8],
    ) -> Result<DecodedDocumentMutation, DocumentMutationError> {
        if payload.len() > MAX_ENCODED_DOCUMENT_MUTATION_BYTES {
            return Err(DocumentMutationError::TooLarge {
                actual: payload.len(),
                max: MAX_ENCODED_DOCUMENT_MUTATION_BYTES,
            });
        }
        let (version, body) = DocumentWireVersion::ALL
            .into_iter()
            .find_map(|version| {
                payload
                    .strip_prefix(version.prefix())
                    .map(|body| (version, body))
            })
            .ok_or(DocumentMutationError::MissingPrefix)?;
        let mutation = if version.is_v1() {
            let (mutation, rest) =
                postcard::take_from_bytes(body).map_err(DocumentMutationError::Malformed)?;
            if !rest.is_empty() {
                return Err(DocumentMutationError::NonCanonical);
            }
            mutation
        } else {
            let (&kind, data) = body.split_first().ok_or(DocumentMutationError::Truncated)?;
            match kind {
                DOCUMENT_SNAPSHOT_TAG => Self::Snapshot(data.to_vec()),
                DOCUMENT_INCREMENTAL_TAG => Self::Incremental(data.to_vec()),
                DOCUMENT_SEGMENT_TAG => return Err(DocumentMutationError::Segmented),
                other => return Err(DocumentMutationError::UnknownKind(other)),
            }
        };
        let actual = mutation.data().len();
        if actual > crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES {
            return Err(DocumentMutationError::TooLarge {
                actual,
                max: crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES,
            });
        }
        if version.is_v1()
            && mutation
                .encode_for(version)
                .map_err(|_| DocumentMutationError::NonCanonical)?
                != payload
        {
            return Err(DocumentMutationError::NonCanonical);
        }
        Ok(DecodedDocumentMutation { mutation, version })
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
    document_artifact_id_for_version(
        DocumentWireVersion::CURRENT,
        app_id,
        workspace_id,
        record_id,
    )
}

#[must_use]
pub fn document_artifact_id_for_version(
    version: DocumentWireVersion,
    app_id: &str,
    workspace_id: &str,
    record_id: &str,
) -> ArtifactId {
    ArtifactId(derived_document_uuid(
        version.domain().artifact_domain(),
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
    document_attestation_id_for_version(
        DocumentWireVersion::CURRENT,
        app_id,
        workspace_id,
        record_id,
    )
}

#[must_use]
pub fn document_attestation_id_for_version(
    version: DocumentWireVersion,
    app_id: &str,
    workspace_id: &str,
    record_id: &str,
) -> AttestationId {
    AttestationId(derived_document_uuid(
        version.domain().attestation_domain(),
        app_id,
        workspace_id,
        record_id,
    ))
}

/// Deterministically derive the MLS channel for one collaboration route.
///
/// This is deliberately independent from the artifact id even though both
/// currently have one-to-one cardinality.  Keeping a separate domain means a
/// broker can bind clear, signed [`EncryptionMetadata`] to the joined route
/// without learning or attempting to decode the ciphertext payload.
#[must_use]
pub fn document_channel_id(app_id: &str, workspace_id: &str, record_id: &str) -> ChannelId {
    document_channel_id_for_version(
        DocumentWireVersion::CURRENT,
        app_id,
        workspace_id,
        record_id,
    )
}

#[must_use]
pub fn document_channel_id_for_version(
    version: DocumentWireVersion,
    app_id: &str,
    workspace_id: &str,
    record_id: &str,
) -> ChannelId {
    ChannelId(derived_document_uuid(
        version.domain().channel_domain(),
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
        channel: ChannelId,
        epoch: u64,
    },
}

/// Version-preserving form used by durable admission and authoring state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionedRoutedDocumentPayload {
    Cleartext(DecodedDocumentMutation),
    Encrypted { channel: ChannelId, epoch: u64 },
}

/// Validate the fields a broker is allowed to inspect for a document route.
///
/// Encrypted payloads are intentionally not decoded.  Their signed channel id
/// must equal the deterministic channel for the route and their ciphertext
/// must be non-empty; authenticity of the inner MLS frame remains an endpoint
/// responsibility.  Cleartext payloads retain the versioned typed-mutation
/// check.
///
/// # Errors
///
/// Returns [`DocumentMutationError::RouteMismatch`] when the signed routing
/// fields or MLS channel do not match, and forwards the typed mutation decode
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
    match validate_document_payload_route_versioned(app_id, workspace_id, record_id, op)? {
        VersionedRoutedDocumentPayload::Cleartext(decoded) => {
            Ok(RoutedDocumentPayload::Cleartext(decoded.mutation))
        }
        VersionedRoutedDocumentPayload::Encrypted { channel, epoch } => {
            if channel != document_channel_id(app_id, workspace_id, record_id) {
                return Err(DocumentMutationError::RouteMismatch);
            }
            Ok(RoutedDocumentPayload::Encrypted { channel, epoch })
        }
    }
}

/// Validate and retain the exact durable document wire version. The artifact
/// and attestation must select one complete known namespace, then the signed
/// payload prefix or MLS channel must select that same namespace.
pub fn validate_document_payload_route_versioned(
    app_id: &str,
    workspace_id: &str,
    record_id: &str,
    op: &SignedOp,
) -> Result<VersionedRoutedDocumentPayload, DocumentMutationError> {
    let matches = |domain: DocumentRouteDomain| {
        op.artifact.0
            == derived_document_uuid(domain.artifact_domain(), app_id, workspace_id, record_id)
            && op.attestation_id.0
                == derived_document_uuid(
                    domain.attestation_domain(),
                    app_id,
                    workspace_id,
                    record_id,
                )
    };
    let domain = if matches(DocumentRouteDomain::TiloV1) {
        DocumentRouteDomain::TiloV1
    } else if matches(DocumentRouteDomain::QosV1) {
        DocumentRouteDomain::QosV1
    } else {
        return Err(DocumentMutationError::RouteMismatch);
    };
    match op.encryption {
        None => {
            let decoded = DocumentMutation::decode_versioned(&op.payload)?;
            // One signed frame carries at most one frame's worth of data;
            // larger mutations travel as segments (see `document_transfer`).
            let actual = decoded.mutation.data().len();
            if actual > MAX_DOCUMENT_MUTATION_BYTES {
                return Err(DocumentMutationError::TooLarge {
                    actual,
                    max: MAX_DOCUMENT_MUTATION_BYTES,
                });
            }
            if decoded.version.domain() != domain {
                return Err(DocumentMutationError::RouteMismatch);
            }
            Ok(VersionedRoutedDocumentPayload::Cleartext(decoded))
        }
        Some(meta) => {
            let expected = ChannelId(derived_document_uuid(
                domain.channel_domain(),
                app_id,
                workspace_id,
                record_id,
            ));
            if meta.channel != expected || op.payload.is_empty() {
                return Err(DocumentMutationError::RouteMismatch);
            }
            Ok(VersionedRoutedDocumentPayload::Encrypted {
                channel: meta.channel,
                epoch: meta.epoch,
            })
        }
    }
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
    #[allow(
        clippy::too_many_arguments,
        reason = "The signed envelope constructor binds payload, artifact, author, attestation, clocks and encryption in one explicit wire contract."
    )]
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

        let legacy = DocumentMutation::Incremental(vec![9, 10]);
        let mut legacy_wire = DOCUMENT_MUTATION_V1_PREFIX.to_vec();
        legacy_wire.extend(postcard::to_allocvec(&legacy).unwrap());
        assert_eq!(DocumentMutation::decode(&legacy_wire).unwrap(), legacy);
    }

    #[test]
    fn legacy_document_mutation_decodes_exactly_without_changing_current_encoding() {
        let mutation = DocumentMutation::Snapshot(vec![0, 1, 2, 0xff]);
        let legacy = mutation.encode_for(DocumentWireVersion::QosV1).unwrap();
        let decoded = DocumentMutation::decode_versioned(&legacy).unwrap();
        assert_eq!(decoded.mutation, mutation);
        assert_eq!(decoded.version, DocumentWireVersion::QosV1);
        assert_eq!(
            decoded.mutation.encode_for(decoded.version).unwrap(),
            legacy
        );

        let current = mutation.encode().unwrap();
        assert_eq!(
            DocumentMutation::decode_versioned(&current)
                .unwrap()
                .version,
            DocumentWireVersion::TiloV2
        );
        assert_ne!(
            legacy, current,
            "new writes must retain the current Tilo format"
        );

        let mut trailing = legacy;
        trailing.push(0);
        assert_eq!(
            DocumentMutation::decode_versioned(&trailing),
            Err(DocumentMutationError::NonCanonical)
        );
    }

    #[test]
    fn legacy_route_requires_one_consistent_signed_namespace() {
        let (app, workspace, record) = ("dev.qos.notes", "room-a", "record-b");
        let mutation = DocumentMutation::Incremental(vec![3, 4, 5]);
        let legacy = DocumentWireVersion::QosV1;
        let mut op = SignedOp::new_unsigned(
            mutation.encode_for(legacy).unwrap(),
            document_artifact_id_for_version(legacy, app, workspace, record),
            Ed25519PublicKey::from_bytes([7; 32]),
            document_attestation_id_for_version(legacy, app, workspace, record),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            1,
        );
        assert!(matches!(
            validate_document_payload_route_versioned(app, workspace, record, &op),
            Ok(VersionedRoutedDocumentPayload::Cleartext(DecodedDocumentMutation {
                version: DocumentWireVersion::QosV1,
                mutation: DocumentMutation::Incremental(bytes),
            })) if bytes == vec![3, 4, 5]
        ));

        op.payload = mutation.encode_for(DocumentWireVersion::TiloV1).unwrap();
        assert_eq!(
            validate_document_payload_route_versioned(app, workspace, record, &op),
            Err(DocumentMutationError::RouteMismatch),
            "a current payload prefix cannot be spliced onto legacy route ids"
        );

        let encrypted = SignedOp::new_unsigned_with_encryption(
            vec![1],
            document_artifact_id_for_version(legacy, app, workspace, record),
            Ed25519PublicKey::from_bytes([7; 32]),
            document_attestation_id_for_version(legacy, app, workspace, record),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            2,
            EncryptionMetadata {
                epoch: 1,
                channel: document_channel_id(app, workspace, record),
            },
        );
        assert_eq!(
            validate_document_payload_route(app, workspace, record, &encrypted),
            Err(DocumentMutationError::RouteMismatch),
            "a current MLS channel cannot be spliced onto legacy route ids"
        );
    }

    #[test]
    fn typed_document_mutations_enforce_wire_budget_before_decode() {
        let oversized =
            DocumentMutation::Incremental(vec![0; MAX_DOCUMENT_MUTATION_BYTES.saturating_add(1)]);
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
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            1,
        );
        assert!(matches!(
            validate_document_payload_route(app, workspace, record, &op),
            Err(DocumentMutationError::TooLarge { .. })
        ));
    }

    #[test]
    fn document_route_ids_are_stable_and_unambiguous() {
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
        assert_ne!(
            first.0,
            document_channel_id("so.tilo.notes", "room-a", "record-b").0,
            "artifact and MLS channel domains must not alias"
        );
        assert_eq!(first.0.get_version_num(), 8);
        assert_eq!(first.0.to_string(), "fd9e9e68-54c3-8ae2-bfc6-cb06b0816e07");
        assert_eq!(
            document_attestation_id("dev.tilo.notes", "room-a", "record-b")
                .0
                .to_string(),
            "fe8b6b90-8390-8f45-923c-e78d2a15cb9c"
        );
        assert_eq!(
            document_channel_id("dev.tilo.notes", "room-a", "record-b")
                .0
                .to_string(),
            "f36e8478-33d6-8069-b327-1971d231a822"
        );
        let golden = DocumentMutation::Snapshot(vec![0, 1, 2, 0xff]);
        assert_eq!(
            golden.encode_for(DocumentWireVersion::TiloV1).unwrap(),
            b"tilo-document-mutation-v1\0\0\x04\0\x01\x02\xff"
        );
        assert_eq!(
            golden.encode_for(DocumentWireVersion::TiloV2).unwrap(),
            b"tilo-document-mutation-v2\0\x01\0\x01\x02\xff"
        );
        assert_eq!(
            document_artifact_id_for_version(
                DocumentWireVersion::QosV1,
                "dev.qos.notes",
                "room-a",
                "record-b"
            )
            .0
            .to_string(),
            "a380dcb2-de74-8a25-8f80-a25f4d19510f"
        );
    }

    #[test]
    fn encrypted_document_route_is_bound_without_decoding_ciphertext() {
        let (app, workspace, record) = ("notes", "workspace", "meeting");
        let op = SignedOp::new_unsigned_with_encryption(
            vec![0xde, 0xad, 0xbe, 0xef],
            document_artifact_id(app, workspace, record),
            Ed25519PublicKey::from_bytes([7; 32]),
            document_attestation_id(app, workspace, record),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            1,
            EncryptionMetadata {
                epoch: 4,
                channel: document_channel_id(app, workspace, record),
            },
        );
        assert_eq!(
            validate_document_payload_route(app, workspace, record, &op),
            Ok(RoutedDocumentPayload::Encrypted {
                channel: document_channel_id(app, workspace, record),
                epoch: 4,
            })
        );
        assert_eq!(
            validate_document_payload_route(app, workspace, "other", &op),
            Err(DocumentMutationError::RouteMismatch)
        );
    }

    #[test]
    fn encrypted_document_route_rejects_wrong_channel_and_empty_ciphertext() {
        let (app, workspace, record) = ("notes", "workspace", "record");
        let build = |payload, channel| {
            SignedOp::new_unsigned_with_encryption(
                payload,
                document_artifact_id(app, workspace, record),
                Ed25519PublicKey::from_bytes([8; 32]),
                document_attestation_id(app, workspace, record),
                OpClass::Splice,
                SystemTime::UNIX_EPOCH,
                1,
                EncryptionMetadata { epoch: 0, channel },
            )
        };
        let wrong = build(vec![1], ChannelId::new());
        let empty = build(Vec::new(), document_channel_id(app, workspace, record));
        assert_eq!(
            validate_document_payload_route(app, workspace, record, &wrong),
            Err(DocumentMutationError::RouteMismatch)
        );
        assert_eq!(
            validate_document_payload_route(app, workspace, record, &empty),
            Err(DocumentMutationError::RouteMismatch)
        );
    }
}
