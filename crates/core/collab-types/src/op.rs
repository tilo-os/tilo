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
/// An incremental change set, as its writer saved the changes. A session's
/// delta coding carries the bytes it repeats from one keystroke to the next,
/// so the set travels as it was written.
pub(crate) const DOCUMENT_INCREMENTAL_TAG: u8 = 4;
/// One piece of a mutation too large for a single frame; see
/// [`crate::document_transfer`].
pub(crate) const DOCUMENT_SEGMENT_TAG: u8 = 3;
/// Maximum Automerge bytes carried by one signed frame. Larger mutations are
/// split into segments (see [`crate::document_transfer`]) up to
/// [`crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES`].
pub const MAX_DOCUMENT_MUTATION_BYTES: usize = 3 * 1024 * 1024;
/// A body is its mutation's bytes behind the prefix and tag, so the largest
/// transfer's frame stays under this.
const MAX_ENCODED_DOCUMENT_MUTATION_BYTES: usize = DOCUMENT_MUTATION_V2_PREFIX.len()
    + crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES
    + crate::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES / 1024
    + 16;
const DOCUMENT_ATTESTATION_DOMAIN: &[u8] = b"tilo-document-attestation-v1\0";
/// The domain of [`artifact_before_reader_sets`].
const ARTIFACT_BEFORE_READER_SETS_DOMAIN: &[u8] = b"tilo-document-artifact-v1\0";

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
        let body = self.data();
        let mut encoded = Vec::with_capacity(
            DOCUMENT_MUTATION_V2_PREFIX
                .len()
                .saturating_add(1)
                .saturating_add(body.len()),
        );
        encoded.extend_from_slice(DOCUMENT_MUTATION_V2_PREFIX);
        encoded.push(match self {
            Self::Snapshot(_) => DOCUMENT_SNAPSHOT_TAG,
            Self::Incremental(_) => DOCUMENT_INCREMENTAL_TAG,
        });
        encoded.extend_from_slice(body);
        Ok(encoded)
    }

    /// The mutation a frame body of kind `tag` carries.
    pub(crate) fn of_wire_body(tag: u8, body: &[u8]) -> Result<Self, DocumentMutationError> {
        match tag {
            DOCUMENT_SNAPSHOT_TAG => Ok(Self::Snapshot(body.to_vec())),
            DOCUMENT_INCREMENTAL_TAG => Ok(Self::Incremental(body.to_vec())),
            DOCUMENT_SEGMENT_TAG => Err(DocumentMutationError::Segmented),
            other => Err(DocumentMutationError::UnknownKind(other)),
        }
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
        let mutation = Self::of_wire_body(kind, data)?;
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

/// The artifact of `document_id`, a document of `app_id` in `workspace_id`: what its writes name
/// and its content is sealed for, under the keys of its reader set
/// ([`crate::readers::content_artifact_id`]): a store document's, as its id names it, and every
/// member's for an app's own document.
#[must_use]
pub fn document_artifact_id(app_id: &str, workspace_id: &str, document_id: &str) -> ArtifactId {
    // A document of the workspace's resource store is sealed for its reader set, which its id
    // names; any other is an app's own, which every member reads.
    match crate::readers::store_document_readers(document_id) {
        Some(readers) => crate::readers::content_artifact_id(workspace_id, readers, document_id),
        None => crate::readers::content_artifact_id(
            workspace_id,
            crate::readers::EVERY_MEMBER,
            &crate::readers::app_document_id(app_id, document_id),
        ),
    }
}

/// The artifact a write of `document_id` of `app_id` in `workspace_id` named before reader sets
/// ([`crate::readers`]), when the workspace's one key group sealed every document's content
/// for this id.
///
/// It names no write made since. A build from before reader sets still names every write of
/// its documents so, sealed or in the clear, and what a workspace's members wrote then is signed
/// over it: a replica takes such a write as one of its document ([`names_document`]), and the
/// first device holding the workspace's keys to open a document sealed so opens it under this id
/// and seals its content again under [`document_artifact_id`] as its own write, keeping the
/// original beside the document as its author signed it.
#[must_use]
pub fn artifact_before_reader_sets(
    app_id: &str,
    workspace_id: &str,
    document_id: &str,
) -> ArtifactId {
    ArtifactId(derived_document_uuid(
        ARTIFACT_BEFORE_READER_SETS_DOMAIN,
        app_id,
        workspace_id,
        document_id,
    ))
}

/// Whether `op` is signed for `document_id` of `app_id` in `workspace_id`: its attestation is
/// the document's, and its artifact the document's of today ([`document_artifact_id`]) or of
/// before reader sets ([`artifact_before_reader_sets`]). Both derive from the whole route, so
/// neither names any other document.
#[must_use]
pub fn names_document(app_id: &str, workspace_id: &str, document_id: &str, op: &SignedOp) -> bool {
    op.attestation_id == document_attestation_id(app_id, workspace_id, document_id)
        && (op.artifact == document_artifact_id(app_id, workspace_id, document_id)
            || op.artifact == artifact_before_reader_sets(app_id, workspace_id, document_id))
}

/// Whether `op` is a sealed write of `document_id` of `app_id` in `workspace_id` made before
/// reader sets ([`artifact_before_reader_sets`]): its content sealed under the artifact of then,
/// which a key holder seals again under today's.
#[must_use]
pub fn sealed_before_reader_sets(
    app_id: &str,
    workspace_id: &str,
    document_id: &str,
    op: &SignedOp,
) -> bool {
    op.encryption.is_some()
        && op.artifact != document_artifact_id(app_id, workspace_id, document_id)
        && op.artifact == artifact_before_reader_sets(app_id, workspace_id, document_id)
        && op.attestation_id == document_attestation_id(app_id, workspace_id, document_id)
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
pub fn document_attestation_id(
    app_id: &str,
    workspace_id: &str,
    document_id: &str,
) -> AttestationId {
    AttestationId(derived_document_uuid(
        DOCUMENT_ATTESTATION_DOMAIN,
        app_id,
        workspace_id,
        document_id,
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
    document_id: &str,
    op: &SignedOp,
) -> Result<RoutedDocumentPayload, DocumentMutationError> {
    if !names_document(app_id, workspace_id, document_id, op) {
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
    match validate_whole_document_payload_route(app_id, workspace_id, document_id, op)? {
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
    document_id: &str,
    op: &SignedOp,
) -> Result<WholeDocumentPayload, DocumentMutationError> {
    if !names_document(app_id, workspace_id, document_id, op) {
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
    document_id: &str,
) -> uuid::Uuid {
    derived_uuid(domain, &[app_id, workspace_id, document_id])
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
    /// A content-key envelope under the document's own MLS group.
    DocumentContentKey {
        group: crate::WorkspaceId,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptionMetadata {
    /// MLS epoch the ciphertext belongs to: the ratchet epoch for
    /// [`EncryptionScheme::MlsApplication`], the content-key epoch for
    /// [`EncryptionScheme::ContentKey`].
    pub epoch: u64,
    pub scheme: EncryptionScheme,
}

impl EncryptionMetadata {
    /// Metadata for a version-1 MLS application message.
    pub const fn mls_application(epoch: u64) -> Self {
        Self {
            epoch,
            scheme: EncryptionScheme::MlsApplication,
        }
    }

    /// Metadata for a version-2 content-key envelope.
    pub const fn content_key(epoch: u64, retention: ContentRetention) -> Self {
        Self {
            epoch,
            scheme: EncryptionScheme::ContentKey { retention },
        }
    }

    /// Metadata for content sealed under the selected workspace or document group.
    pub fn content_group(
        workspace: crate::WorkspaceId,
        group: crate::WorkspaceId,
        epoch: u64,
        retention: ContentRetention,
    ) -> Self {
        if group == workspace {
            Self::content_key(epoch, retention)
        } else {
            Self {
                epoch,
                scheme: EncryptionScheme::DocumentContentKey { group, retention },
            }
        }
    }

    /// Storage class of a content-key ciphertext; `None` for schemes that
    /// cannot be retained (an MLS application message is single-use).
    pub const fn retention(&self) -> Option<ContentRetention> {
        match self.scheme {
            EncryptionScheme::MlsApplication => None,
            EncryptionScheme::ContentKey { retention }
            | EncryptionScheme::DocumentContentKey { retention, .. } => Some(retention),
        }
    }

    /// Which group encrypted this payload. Older ciphertext belongs to the
    /// workspace group.
    pub const fn key_group(self, workspace: crate::WorkspaceId) -> crate::WorkspaceId {
        match self.scheme {
            EncryptionScheme::DocumentContentKey { group, .. } => group,
            _ => workspace,
        }
    }
}

/// Serde adapter for `Option<EncryptionMetadata>`: its tagged wire form
/// (see [`EncryptionMetadata`]).
/// The claims of an op as runs: the keystrokes of one word each claim the
/// same edit of the same units, which the op states once for all of their
/// changes rather than once per change.
mod claims_wire {
    use crate::claim::{ChangeClaim, ClaimedAction, UnitPath};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Run {
        changes: Vec<[u8; 32]>,
        action: ClaimedAction,
        units: Vec<UnitPath>,
        #[serde(with = "crate::claim::invoker_text")]
        invoker: Option<crate::claim::Invoker>,
        /// The call the run's changes were made for, when a host ran them for
        /// a caller elsewhere: the caller's own signed invocation, which every
        /// change of one invocation shares.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<Box<tilo_actions::remote::SignedInvocation>>,
    }

    #[derive(Serialize)]
    struct RunRef<'a> {
        changes: Vec<[u8; 32]>,
        action: &'a ClaimedAction,
        units: &'a [UnitPath],
        invoker: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        call: Option<&'a tilo_actions::remote::SignedInvocation>,
    }

    pub(super) fn serialize<S: Serializer>(
        claims: &[ChangeClaim],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut grouped: Vec<RunRef<'_>> = Vec::new();
        for claim in claims {
            let invoker = crate::claim::invoker_text::text(claim.invoker.as_ref())
                .map_err(serde::ser::Error::custom)?;
            let call = claim.call.as_deref();
            match grouped.last_mut() {
                Some(last)
                    if *last.action == claim.action
                        && last.units == claim.units
                        && last.invoker == invoker
                        && last.call == call =>
                {
                    last.changes.push(claim.change);
                }
                _ => grouped.push(RunRef {
                    changes: vec![claim.change],
                    action: &claim.action,
                    units: &claim.units,
                    invoker,
                    call,
                }),
            }
        }
        grouped.serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<ChangeClaim>, D::Error> {
        let runs = Vec::<Run>::deserialize(deserializer)?;
        let mut claims = Vec::new();
        for Run {
            changes,
            action,
            units,
            invoker,
            call,
        } in runs
        {
            if changes.is_empty() {
                return Err(serde::de::Error::custom("a claim run names no change"));
            }
            claims.extend(changes.into_iter().map(|change| ChangeClaim {
                change,
                action: action.clone(),
                units: units.clone(),
                invoker: invoker.clone(),
                call: call.clone(),
            }));
        }
        Ok(claims)
    }
}

/// How an op names the governance change whose hash is `hash`
/// ([`SignedOp::governance`]): by the first four bytes of the hash, as a short
/// commit id names a commit. Every op a member writes carries its governance
/// heads, so each costs what tells one workspace's governance changes apart,
/// not a whole 32-byte hash on every keystroke. Two changes sharing these
/// bytes are both taken as named: the op is then judged by a governance that
/// holds a change more than its writer did.
#[must_use]
pub fn governance_head(hash: &[u8; 32]) -> [u8; 4] {
    hash.first_chunk::<4>().copied().unwrap_or_default()
}

/// Why a write that waited for the governance it was written under is
/// refused when that governance did not come in time.
pub const GOVERNANCE_NEVER_ARRIVED: &str = "the role change it depends on never arrived";

/// One signed CRDT mutation. `Clone` because the broker fans an op to
/// multiple peers; `PartialEq` for envelope-identity assertions in tests.
/// Deliberately not `Hash` — use the signature bytes as a map key if needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// peer can't rewrite the metadata to downgrade a ciphertext.
    #[serde(default)]
    pub encryption: Option<EncryptionMetadata>,
    /// What each change in `payload` claims to be: its action and the
    /// storage units it writes, one per change, signed with the op so a
    /// receiver (a server that cannot read a sealed payload included) decides
    /// every change against the author's grants before merging it. On the
    /// wire, consecutive changes that claim the same action and units share
    /// one statement of them ([`claims_wire`]).
    #[serde(with = "claims_wire")]
    pub claims: Vec<crate::claim::ChangeClaim>,
    /// The content hash (BLAKE3) of the app surface the op's changes are
    /// decided against: the version's `surface.toml` in the form it is kept
    /// by. A receiver fetches it by hash and holds the op until it has it.
    pub surface: Option<[u8; 32]>,
    /// The heads of the workspace's governance its writer held when it wrote
    /// the op: the roles and grants it wrote under, and so what every receiver
    /// judges it by, whatever order changes reach it in. A receiver holds the
    /// op until it holds these heads. A change since that took its writer's
    /// standing away judges it too, unless the change kept the op as written
    /// before it. Each is named as [`governance_head`] names it.
    pub governance: Vec<[u8; 4]>,
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
            governance: Vec::new(),
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

    /// The op written under the governance whose heads are `heads`
    /// ([`Self::governance`]).
    #[must_use]
    pub fn with_governance(mut self, heads: &[[u8; 32]]) -> Self {
        self.name_governance(heads);
        self
    }

    /// Name `heads` as the governance this op was written under, as
    /// [`Self::with_governance`] does.
    pub fn name_governance(&mut self, heads: &[[u8; 32]]) {
        self.governance = heads.iter().map(governance_head).collect();
    }

    /// Whether this op is the whole record rather than a change to it: a
    /// compaction, which the key holder folded the record's sealed history
    /// into, or a checkpoint, the record as its writer saved it. It names
    /// the record on its own, so a replica holding none of the route founds
    /// it with this op, and a writer that founds a record sealed publishes
    /// one instead of a cleartext genesis a member, who reads only what the
    /// workspace's keys seal, could never account for.
    #[must_use]
    pub fn is_whole_resource(&self) -> bool {
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
            encryption: &'a Option<EncryptionMetadata>,
            claims: &'a [crate::claim::ChangeClaim],
            surface: &'a Option<[u8; 32]>,
            governance: &'a [[u8; 4]],
        }
        canonical_bytes(&View {
            payload: &self.payload,
            artifact: &self.artifact,
            author: &self.author,
            attestation_id: &self.attestation_id,
            kind: &self.kind,
            timestamp: &self.timestamp,
            seq: self.seq,
            encryption: &self.encryption,
            claims: &self.claims,
            surface: &self.surface,
            governance: &self.governance,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::from_canonical_bytes;

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
        /// The letters of a typed word claim one edit, which the op states once.
        fn an_ops_claims_state_a_shared_action_once;

        phase a_words_claims_travel_as_one_run_and_read_back_per_change;
        {
            use crate::claim::{ChangeClaim, ClaimedAction};
            use tilo_actions::PathStep;
            let edit = |field: &str| ClaimedAction::Edit {
                kind: "Document".into(),
                path: vec![
                    PathStep::Field("pages".into()),
                    PathStep::Item("12980f87-fbbb-446e-8620-ee255ff8498f".into()),
                    PathStep::Field(field.into()),
                ],
            };
            let claim = |change: u8, field: &str| ChangeClaim {
                change: [change; 32],
                action: edit(field),
                units: vec![vec![PathStep::Field("pages".into())]],
                invoker: None,
                call: None,
            };
            let op = |claims: Vec<ChangeClaim>| {
                SignedOp::new_unsigned(
                    vec![1],
                    ArtifactId::new(),
                    Ed25519PublicKey::from_bytes([7; 32]),
                    AttestationId::default(),
                    OpKind::Change,
                    SystemTime::UNIX_EPOCH,
                    1,
                )
                .with_claims(claims)
            };
            let one = postcard::to_stdvec(&op(vec![claim(0, "body")])).unwrap().len();
            let word = op((1..=6).map(|change| claim(change, "body")).collect());
            let bytes = postcard::to_stdvec(&word).unwrap();
            assert_eq!(
                bytes.len().saturating_sub(one),
                5 * 32,
                "each further letter adds its change's hash alone"
            );
            assert_eq!(postcard::from_bytes::<SignedOp>(&bytes).unwrap(), word);

            let mixed = op(vec![claim(1, "body"), claim(2, "title"), claim(3, "body")]);
            let bytes = postcard::to_stdvec(&mixed).unwrap();
            assert_eq!(
                postcard::from_bytes::<SignedOp>(&bytes).unwrap(),
                mixed,
                "claims keep their order across runs"
            );
        }
    }

    /// A run's claims do not cross runtime version 40 in either direction: the
    /// `call` field was appended to each run, and postcard writes a struct's
    /// fields in order with no names or count, so a build made at 39 and a
    /// build made at 40 cannot read each other's runs. The runtime interface's
    /// oldest served version is 40 for that reason, and this test guards it.
    #[test]
    fn claims_wire_does_not_cross_runtime_version_40() {
        use crate::claim::{ChangeClaim, ClaimedAction, UnitPath};
        use tilo_actions::PathStep;

        // A run as version 39 wrote it: the 40 run's fields without the call.
        #[derive(serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Run39 {
            changes: Vec<[u8; 32]>,
            action: ClaimedAction,
            units: Vec<UnitPath>,
            #[serde(with = "crate::claim::invoker_text")]
            invoker: Option<crate::claim::Invoker>,
        }
        // The claims a 40 build writes and reads: the real adapter either way.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct ClaimsAt40 {
            #[serde(with = "super::claims_wire")]
            claims: Vec<ChangeClaim>,
        }

        let action = || ClaimedAction::Edit {
            kind: "Document".into(),
            path: vec![PathStep::Field("body".into())],
        };
        let units = || vec![vec![PathStep::Field("body".into())]];
        let claim = |call| ChangeClaim {
            change: [1; 32],
            action: action(),
            units: units(),
            invoker: None,
            call,
        };

        let at_39 = postcard::to_stdvec(&vec![Run39 {
            changes: vec![[1; 32]],
            action: action(),
            units: units(),
            invoker: None,
        }])
        .unwrap();
        assert!(
            postcard::from_bytes::<ClaimsAt40>(&at_39).is_err(),
            "a run's frame written at 39 is read by 40 as a run with a missing call"
        );

        let statement = tilo_actions::remote::signed_invocation::InvocationStatement {
            workspace: "w1".into(),
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget::workspace_governance(
                "w1",
                "member.invite",
            ),
            arguments: [0; 32],
            on_behalf_of: None,
            issued_at_ns: 1,
            signer: [0; 32],
        };
        let signed = tilo_actions::remote::SignedInvocation::new(statement, [0; 64], vec![1]);
        let at_40 = postcard::to_stdvec(&ClaimsAt40 {
            claims: vec![claim(Some(Box::new(signed)))],
        })
        .unwrap();
        assert!(
            postcard::from_bytes::<Vec<Run39>>(&at_40).is_err(),
            "a 40 run with a call is read as more than the run a 39 build knows"
        );
    }

    tilo_test_env::scenario! {
        /// The first phase pins the field layout of the wire bytes by their digest.
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
                document_artifact_id("app", "workspace", "record"),
                Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                document_attestation_id("app", "workspace", "record"),
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
                invoker: None,
                call: None,
            }])
            .with_surface(Some([5; 32]))
            .with_governance(&[[6; 32]]);
            crate::crypto::sign_op(&mut op, &key).unwrap();
            let bytes = canonical_bytes(&op).unwrap();
            assert_eq!(from_canonical_bytes::<SignedOp>(&bytes).unwrap(), op);
            assert_eq!(
                postcard::from_bytes::<SignedOp>(&postcard::to_allocvec(&op).unwrap()).unwrap(),
                op
            );
            assert_eq!(
                hex::encode(sha2::Sha256::digest(&bytes)),
                "207ec79f4e1a1309ca6523fd281533ee8b08bfaa50b41c1f530402abbc02cbbf",
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
            let first = document_artifact_id("so.tilo.notes", "workspace-a", "record-b");
            assert_eq!(
                first,
                document_artifact_id("so.tilo.notes", "workspace-a", "record-b")
            );
            assert_ne!(
                first,
                document_artifact_id("so.tilo.notes", "workspace-ar", "ecord-b")
            );
            assert_ne!(
                first.0,
                document_attestation_id("so.tilo.notes", "workspace-a", "record-b").0,
                "artifact and attestation domains must not alias"
            );
            assert_eq!(first.0.get_version_num(), 8);
            assert_eq!(first.0.to_string(), "3b09fe70-3936-80a2-b81e-463232e016b9");
            assert_eq!(
                document_artifact_id("dev.tilo.notes", "workspace-a", "record-b")
                    .0
                    .to_string(),
                "2ceca59c-1004-854e-9067-9a09c2b5128a"
            );
            assert_eq!(
                document_attestation_id("dev.tilo.notes", "workspace-a", "record-b")
                    .0
                    .to_string(),
                "693e44d3-c0bb-81ef-a888-2aa67a0f1e4c"
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

        phase a_write_named_before_reader_sets_is_known_by_the_artifact_it_named;
        {
            // The ids that build's writes named, as it derived them.
            assert_eq!(
                artifact_before_reader_sets("so.tilo.notes", "workspace-a", "note-b")
                    .0
                    .to_string(),
                "83dfd05d-57e8-8b27-a8da-ee7f8a9278d4"
            );
            assert_eq!(
                artifact_before_reader_sets("dev.tilo.notes", "workspace-a", "note-b")
                    .0
                    .to_string(),
                "71ee3ff5-69ab-8d1a-9dc8-ef0c63e0eacc"
            );
            let (app, workspace, note) = ("notes", "workspace", "meeting");
            let sealed = |artifact| {
                SignedOp::new_unsigned_with_encryption(
                    vec![0xde, 0xad],
                    artifact,
                    Ed25519PublicKey::from_bytes([7; 32]),
                    document_attestation_id(app, workspace, note),
                    OpKind::Change,
                    SystemTime::UNIX_EPOCH,
                    1,
                    EncryptionMetadata::content_key(3, ContentRetention::Delta),
                )
            };
            let former = sealed(artifact_before_reader_sets(app, workspace, note));
            assert!(sealed_before_reader_sets(app, workspace, note, &former));
            // It is a write of its document, as one of today's is, and of no other document.
            assert_eq!(
                validate_whole_document_payload_route(app, workspace, note, &former),
                Ok(WholeDocumentPayload::Encrypted { epoch: 3 })
            );
            assert_eq!(
                validate_document_payload_route(app, workspace, note, &former),
                Ok(RoutedDocumentPayload::Encrypted { epoch: 3 })
            );
            assert_eq!(
                validate_whole_document_payload_route(app, workspace, "other", &former),
                Err(DocumentMutationError::RouteMismatch)
            );
            assert!(!sealed_before_reader_sets(app, workspace, "other", &former));
            let current = sealed(document_artifact_id(app, workspace, note));
            assert!(!sealed_before_reader_sets(app, workspace, note, &current));
            // A write in the clear that build named so is its document's too, and was never
            // sealed.
            let mut clear = former;
            clear.encryption = None;
            clear.payload = DocumentMutation::Incremental(vec![1, 2, 3]).encode().unwrap();
            assert!(!sealed_before_reader_sets(app, workspace, note, &clear));
            assert_eq!(
                validate_whole_document_payload_route(app, workspace, note, &clear),
                Ok(WholeDocumentPayload::Cleartext(DocumentMutation::Incremental(vec![1, 2, 3])))
            );
            assert_eq!(
                validate_whole_document_payload_route(app, workspace, "other", &clear),
                Err(DocumentMutationError::RouteMismatch)
            );
        }

        phase encrypted_document_route_rejects_empty_ciphertext;
        {
            let (app, workspace, note) = ("notes", "workspace", "note");
            let build = |payload| {
                SignedOp::new_unsigned_with_encryption(
                    payload,
                    document_artifact_id(app, workspace, note),
                    Ed25519PublicKey::from_bytes([8; 32]),
                    document_attestation_id(app, workspace, note),
                    OpKind::Change,
                    SystemTime::UNIX_EPOCH,
                    1,
                    EncryptionMetadata::mls_application(0),
                )
            };
            let empty = build(Vec::new());
            assert_eq!(
                validate_document_payload_route(app, workspace, note, &empty),
                Err(DocumentMutationError::RouteMismatch)
            );
        }
    }
}
