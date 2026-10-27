//! Stable, versioned wire records for collaborative files.
//!
//! Paths are mutable presentation metadata. Authority, versions, previews and
//! conflicts refer to [`FileRecordId`] so a rename or move cannot silently
//! change what was shared or which history belongs to a file.

mod history_read;
pub use history_read::*;
mod checked_history;
pub use checked_history::*;

use serde::de::{self, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

use collab_types::{
    CanonicalError, Ed25519PublicKey, Signature, VerifyError, canonical_bytes, sign_event,
    verify_event,
};
use sha2::{Digest, Sha256};

pub const FILE_METADATA_VERSION: u16 = 2;
pub const FILE_METADATA_LEGACY_VERSION: u16 = 1;
pub const FILE_PROPERTIES_VERSION: u16 = 1;
pub const FILE_BINDING_VERSION: u16 = 1;
pub const CONTENT_REVISION_VERSION: u16 = 1;
pub const MAX_MIME_BYTES: usize = 255;
pub const MAX_FILE_NAME_BYTES: usize = 255;
pub const MAX_PREVIEW_VARIANTS: usize = 8;
pub const MAX_METADATA_TOKEN_BYTES: usize = 64;
pub const MAX_IMAGE_DIMENSION: u32 = 1_000_000;
pub const MAX_MEDIA_DURATION_MS: u64 = 100 * 365 * 24 * 60 * 60 * 1_000;
pub const MAX_DOCUMENT_PAGES: u32 = 10_000_000;
pub const MAX_VERSION_PARENTS: usize = 8;
pub const MAX_CONFLICT_VERSIONS: usize = 8;
pub const FILE_MUTATION_TRANSACTION_VERSION: u16 = 1;
pub const FILE_MUTATION_TRANSACTION_VERSION_V2: u16 = 2;
pub const FILE_MUTATION_TRANSACTION_VERSION_V3: u16 = 3;
pub const FILE_MUTATION_TRANSACTION_VERSION_V4: u16 = 4;
pub const MAX_TRANSACTION_PARTICIPANTS: usize = 8;
pub const FILE_MUTATION_TRANSACTION_SIGNATURE_DOMAIN: &str =
    "tilo.collaborative-fs.file-mutation-transaction.v1";
pub const FILE_MUTATION_TRANSACTION_SIGNATURE_DOMAIN_V2: &str =
    "tilo.collaborative-fs.file-mutation-transaction.v2";
pub const FILE_MUTATION_TRANSACTION_SIGNATURE_DOMAIN_V4: &str =
    "tilo.collaborative-fs.file-mutation-transaction.v4";

/// A Unix directory-entry name, preserved as the exact bytes supplied by the
/// kernel.  The transparent byte-vector encoding is deliberately wire
/// compatible with the previous `String` field for every valid UTF-8 name:
/// postcard encodes both as a length followed by the same bytes.  New peers
/// can therefore read old signed records without migration or resigning.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileNameV1(pub Vec<u8>);

impl FileNameV1 {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Safe presentation only. Never use this value for lookup or identity.
    pub fn display_lossy(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

impl From<String> for FileNameV1 {
    fn from(value: String) -> Self {
        Self(value.into_bytes())
    }
}
impl From<&str> for FileNameV1 {
    fn from(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }
}
impl From<Vec<u8>> for FileNameV1 {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}
impl AsRef<[u8]> for FileNameV1 {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl std::ops::Deref for FileNameV1 {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        self.as_bytes()
    }
}
impl PartialEq<str> for FileNameV1 {
    fn eq(&self, other: &str) -> bool {
        self.0 == other.as_bytes()
    }
}
impl PartialEq<&str> for FileNameV1 {
    fn eq(&self, other: &&str) -> bool {
        self.0 == other.as_bytes()
    }
}
impl PartialEq<String> for FileNameV1 {
    fn eq(&self, other: &String) -> bool {
        self.0 == other.as_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileRecordId(pub Uuid);

impl FileRecordId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for FileRecordId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileVersionId(pub Uuid);

impl FileVersionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for FileVersionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(pub [u8; 32]);

/// Atomic namespace pointer to one immutable, signed record-history version.
///
/// The namespace stores the canonical encoding as one CRDT scalar so a merge
/// can never select a hash from one edit and a size or record identity from
/// another. The referenced [`FileVersionV1`] remains the authority; this is a
/// compact discovery pointer, not a replacement for signature verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentRevisionV1 {
    pub schema_version: u16,
    pub record_id: FileRecordId,
    pub version_id: FileVersionId,
    pub content_hash: Option<ContentHash>,
    pub byte_len: u64,
    pub metadata_hash: ContentHash,
}

impl ContentRevisionV1 {
    /// Validate version, identity, and immutable revision identifiers.
    pub fn validate(&self) -> Result<(), FileRecordError> {
        if self.schema_version != CONTENT_REVISION_VERSION {
            return Err(FileRecordError::UnsupportedContentRevisionVersion(
                self.schema_version,
            ));
        }
        if self.record_id.0.is_nil() {
            return Err(FileRecordError::NilRecordId);
        }
        if self.version_id.0.is_nil() {
            return Err(FileRecordError::NilVersionId);
        }
        Ok(())
    }
}

/// The certified human authority and the exact device that performed an op.
/// Both are security identifiers; a separately resolved label is display-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileActor {
    pub person: Ed25519PublicKey,
    pub device: Ed25519PublicKey,
}

/// Structural kind authenticated independently of the mutable namespace CRDT.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FileNodeKind {
    File,
    Directory,
}

/// Signed stable-record-to-directory binding.
///
/// The namespace CRDT remains a discovery/index projection. This record is the
/// authority for structural operations and is the prerequisite for safely
/// staging mutations which touch more than one stable record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBindingV1 {
    pub schema_version: u16,
    pub record_id: FileRecordId,
    /// `None` identifies the workspace root. Every other binding has a stable
    /// directory record as parent, including tombstones.
    pub parent: Option<FileRecordId>,
    pub name: FileNameV1,
    pub kind: FileNodeKind,
    pub tombstoned: bool,
    pub actor: FileActor,
    pub observed_at_ms: u64,
}

impl FileBindingV1 {
    /// Validate the structural binding and its bounded exact-byte name.
    pub fn validate(&self) -> Result<(), FileRecordError> {
        if self.schema_version != FILE_BINDING_VERSION {
            return Err(FileRecordError::UnsupportedBindingVersion(
                self.schema_version,
            ));
        }
        if self.record_id.0.is_nil() {
            return Err(FileRecordError::NilRecordId);
        }
        if self.parent.is_some_and(|parent| parent.0.is_nil()) {
            return Err(FileRecordError::NilParentRecordId);
        }
        if self.parent == Some(self.record_id) {
            return Err(FileRecordError::BindingSelfParent);
        }
        if self.parent.is_none() {
            if !self.name.is_empty() || self.kind != FileNodeKind::Directory || self.tombstoned {
                return Err(FileRecordError::InvalidRootBinding);
            }
        } else if self.name.is_empty()
            || self.name.len() > MAX_FILE_NAME_BYTES
            || !valid_file_name(&self.name)
        {
            return Err(FileRecordError::InvalidBindingName);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum PreviewStatus {
    Pending,
    Ready,
    Failed,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewVariant {
    pub max_edge: u32,
    pub hash: Option<ContentHash>,
    pub status: PreviewStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum IndexStatus {
    Pending,
    Indexed,
    Failed,
    Excluded,
}

/// Bounded metadata that can replicate and render without fetching file bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilePropertiesV1 {
    pub schema_version: u16,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub page_count: Option<u32>,
    pub codec: Option<String>,
    pub container: Option<String>,
}

impl FilePropertiesV1 {
    /// Validate all bounds in this replicated rich-properties record.
    pub fn validate(&self) -> Result<(), FileRecordError> {
        if self.schema_version != FILE_PROPERTIES_VERSION {
            return Err(FileRecordError::UnsupportedPropertiesVersion(
                self.schema_version,
            ));
        }
        match (self.image_width, self.image_height) {
            (Some(w), Some(h))
                if w > 0 && h > 0 && w <= MAX_IMAGE_DIMENSION && h <= MAX_IMAGE_DIMENSION => {}
            (None, None) => {}
            _ => return Err(FileRecordError::InvalidImageDimensions),
        }
        if self
            .duration_ms
            .is_some_and(|v| v == 0 || v > MAX_MEDIA_DURATION_MS)
        {
            return Err(FileRecordError::InvalidMediaDuration);
        }
        if self
            .page_count
            .is_some_and(|v| v == 0 || v > MAX_DOCUMENT_PAGES)
        {
            return Err(FileRecordError::InvalidDocumentPageCount);
        }
        for token in [&self.codec, &self.container].into_iter().flatten() {
            if token.is_empty()
                || token.len() > MAX_METADATA_TOKEN_BYTES
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
            {
                return Err(FileRecordError::InvalidMetadataToken);
            }
        }
        if self.image_width.is_none()
            && self.duration_ms.is_none()
            && self.page_count.is_none()
            && self.codec.is_none()
            && self.container.is_none()
        {
            return Err(FileRecordError::EmptyFileProperties);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMetadataV1 {
    pub schema_version: u16,
    pub record_id: FileRecordId,
    pub content_hash: Option<ContentHash>,
    pub byte_len: u64,
    pub mime: Option<String>,
    pub previews: Vec<PreviewVariant>,
    pub latest_actor: Option<FileActor>,
    pub observed_at_ms: u64,
    pub index_status: IndexStatus,
    pub properties: Option<FilePropertiesV1>,
}

impl Serialize for FileMetadataV1 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let legacy = self.schema_version == FILE_METADATA_LEGACY_VERSION;
        let mut state =
            serializer.serialize_struct("FileMetadataV1", if legacy { 9 } else { 10 })?;
        state.serialize_field("schema_version", &self.schema_version)?;
        state.serialize_field("record_id", &self.record_id)?;
        state.serialize_field("content_hash", &self.content_hash)?;
        state.serialize_field("byte_len", &self.byte_len)?;
        state.serialize_field("mime", &self.mime)?;
        state.serialize_field("previews", &self.previews)?;
        state.serialize_field("latest_actor", &self.latest_actor)?;
        state.serialize_field("observed_at_ms", &self.observed_at_ms)?;
        state.serialize_field("index_status", &self.index_status)?;
        if !legacy {
            state.serialize_field("properties", &self.properties)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for FileMetadataV1 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MetadataVisitor;
        impl<'de> Visitor<'de> for MetadataVisitor {
            type Value = FileMetadataV1;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("versioned file metadata")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                macro_rules! next {
                    ($name:literal) => {
                        seq.next_element()?
                            .ok_or_else(|| de::Error::missing_field($name))?
                    };
                }
                let schema_version: u16 = next!("schema_version");
                Ok(FileMetadataV1 {
                    schema_version,
                    record_id: next!("record_id"),
                    content_hash: next!("content_hash"),
                    byte_len: next!("byte_len"),
                    mime: next!("mime"),
                    previews: next!("previews"),
                    latest_actor: next!("latest_actor"),
                    observed_at_ms: next!("observed_at_ms"),
                    index_status: next!("index_status"),
                    properties: if schema_version == FILE_METADATA_LEGACY_VERSION {
                        None
                    } else {
                        next!("properties")
                    },
                })
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct NamedMetadata {
                    schema_version: u16,
                    record_id: FileRecordId,
                    content_hash: Option<ContentHash>,
                    byte_len: u64,
                    mime: Option<String>,
                    previews: Vec<PreviewVariant>,
                    latest_actor: Option<FileActor>,
                    observed_at_ms: u64,
                    index_status: IndexStatus,
                    #[serde(default)]
                    properties: Option<FilePropertiesV1>,
                }
                let value = NamedMetadata::deserialize(MapAccessDeserializer::new(map))?;
                Ok(FileMetadataV1 {
                    schema_version: value.schema_version,
                    record_id: value.record_id,
                    content_hash: value.content_hash,
                    byte_len: value.byte_len,
                    mime: value.mime,
                    previews: value.previews,
                    latest_actor: value.latest_actor,
                    observed_at_ms: value.observed_at_ms,
                    index_status: value.index_status,
                    properties: value.properties,
                })
            }
        }
        const FIELDS: &[&str] = &[
            "schema_version",
            "record_id",
            "content_hash",
            "byte_len",
            "mime",
            "previews",
            "latest_actor",
            "observed_at_ms",
            "index_status",
            "properties",
        ];
        deserializer.deserialize_struct("FileMetadataV1", FIELDS, MetadataVisitor)
    }
}

impl FileMetadataV1 {
    /// Validate the metadata schema and every bounded nested value.
    pub fn validate(&self) -> Result<(), FileRecordError> {
        if !matches!(
            self.schema_version,
            FILE_METADATA_LEGACY_VERSION | FILE_METADATA_VERSION
        ) {
            return Err(FileRecordError::UnsupportedVersion(self.schema_version));
        }
        if self.mime.as_ref().is_some_and(|s| s.len() > MAX_MIME_BYTES) {
            return Err(FileRecordError::MimeTooLong);
        }
        if self.previews.len() > MAX_PREVIEW_VARIANTS {
            return Err(FileRecordError::TooManyPreviews);
        }
        if self.schema_version == FILE_METADATA_LEGACY_VERSION && self.properties.is_some() {
            return Err(FileRecordError::UnsupportedVersion(self.schema_version));
        }
        if let Some(properties) = &self.properties {
            properties.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FileProvenance {
    Create,
    Edit,
    Rename,
    Move,
    Delete,
    Restore { from: FileVersionId },
    ConflictResolution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FileMutationParticipantRole {
    Source,
    Replaced,
    KeepBothSource,
    KeepBothSibling,
    MergeSource,
    RestoreContent,
    AtomicSaveDocument,
    AtomicSaveTemporary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ConflictMergeCodecV1 {
    TiloDocAutomergeV1,
    PlainUtf8ThreeWayV1,
}

/// Complete authenticated post-state proposed for one transaction participant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMutationParticipant {
    pub record_id: FileRecordId,
    pub role: FileMutationParticipantRole,
    pub expected_version: FileVersionId,
    pub proposed_binding: FileBindingV1,
    pub proposed_version: FileVersionV1,
    pub proposed_content_revision: Option<ContentRevisionV1>,
    pub projection_hash: ContentHash,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ParticipantProjectionView<'a> {
    record_id: FileRecordId,
    role: FileMutationParticipantRole,
    expected_version: FileVersionId,
    proposed_binding: &'a FileBindingV1,
    proposed_version: &'a FileVersionV1,
    proposed_content_revision: &'a Option<ContentRevisionV1>,
}

impl FileMutationParticipant {
    fn projection_view(&self) -> ParticipantProjectionView<'_> {
        ParticipantProjectionView {
            record_id: self.record_id,
            role: self.role,
            expected_version: self.expected_version,
            proposed_binding: &self.proposed_binding,
            proposed_version: &self.proposed_version,
            proposed_content_revision: &self.proposed_content_revision,
        }
    }

    /// Compute the canonical digest binding this participant's proposed projection.
    pub fn computed_projection_hash(&self) -> Result<ContentHash, CanonicalError> {
        let bytes = canonical_bytes(&self.projection_view())?;
        Ok(ContentHash(Sha256::digest(bytes).into()))
    }

    /// Validate participant identity, role, revision, and projection digest.
    ///
    /// # Errors
    ///
    /// Returns a [`FileRecordError`] when any signed participant invariant fails.
    pub fn validate(&self, actor: FileActor) -> Result<(), FileRecordError> {
        if self.record_id.0.is_nil() || self.expected_version.0.is_nil() {
            return Err(FileRecordError::InvalidTransactionParticipant);
        }
        self.proposed_binding.validate()?;
        self.proposed_version.validate()?;
        if let Some(revision) = &self.proposed_content_revision {
            revision.validate()?;
        }
        if self.proposed_binding.record_id != self.record_id
            || self.proposed_version.record_id != self.record_id
            || self.proposed_version.actor != actor
            || self.proposed_binding.actor != actor
        {
            return Err(FileRecordError::InvalidTransactionParticipant);
        }
        let valid_parents = match self.role {
            FileMutationParticipantRole::Source
            | FileMutationParticipantRole::Replaced
            | FileMutationParticipantRole::AtomicSaveDocument
            | FileMutationParticipantRole::AtomicSaveTemporary => {
                self.proposed_version.parents == [self.expected_version]
            }
            FileMutationParticipantRole::KeepBothSource
            | FileMutationParticipantRole::MergeSource
            | FileMutationParticipantRole::RestoreContent => self
                .proposed_version
                .parents
                .contains(&self.expected_version),
            FileMutationParticipantRole::KeepBothSibling => {
                self.proposed_version.parents.is_empty()
            }
        };
        if !valid_parents {
            return Err(FileRecordError::InvalidTransactionParticipant);
        }
        match &self.proposed_content_revision {
            Some(revision)
                if revision.record_id == self.record_id
                    && revision.version_id == self.proposed_version.id
                    && revision.content_hash == self.proposed_version.content_hash
                    && revision.metadata_hash == self.proposed_version.metadata_hash => {}
            None if self.proposed_binding.kind == FileNodeKind::Directory
                && self.proposed_version.content_hash.is_none() => {}
            _ => return Err(FileRecordError::InvalidTransactionParticipant),
        }
        let valid_role = match self.role {
            FileMutationParticipantRole::Source => {
                !self.proposed_binding.tombstoned
                    && matches!(
                        self.proposed_version.provenance,
                        FileProvenance::Rename | FileProvenance::Move
                    )
            }
            FileMutationParticipantRole::AtomicSaveDocument => {
                !self.proposed_binding.tombstoned
                    && self.proposed_binding.kind == FileNodeKind::File
                    && self.proposed_version.provenance == FileProvenance::Edit
            }
            FileMutationParticipantRole::AtomicSaveTemporary => {
                self.proposed_binding.tombstoned
                    && self.proposed_binding.kind == FileNodeKind::File
                    && self.proposed_version.provenance == FileProvenance::Delete
            }
            FileMutationParticipantRole::Replaced => {
                self.proposed_binding.tombstoned
                    && self.proposed_version.provenance == FileProvenance::Delete
            }
            FileMutationParticipantRole::KeepBothSource
            | FileMutationParticipantRole::MergeSource => {
                !self.proposed_binding.tombstoned
                    && self.proposed_binding.kind == FileNodeKind::File
                    && self.proposed_version.provenance == FileProvenance::ConflictResolution
            }
            FileMutationParticipantRole::RestoreContent => {
                !self.proposed_binding.tombstoned
                    && self.proposed_binding.kind == FileNodeKind::File
                    && matches!(
                        self.proposed_version.provenance,
                        FileProvenance::Restore { .. }
                    )
            }
            FileMutationParticipantRole::KeepBothSibling => {
                !self.proposed_binding.tombstoned
                    && self.proposed_binding.kind == FileNodeKind::File
                    && self.proposed_version.provenance == FileProvenance::Create
            }
        };
        if !valid_role || self.computed_projection_hash().ok() != Some(self.projection_hash) {
            return Err(FileRecordError::InvalidTransactionParticipant);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FileMutationOperation {
    RestoreContent {
        request: CheckedHistoryRequestV1,
        selected_version: FileVersionId,
        prior_revision: ContentRevisionV1,
        compensates: Option<Uuid>,
    },
    ReplacementRename {
        source_parent: FileRecordId,
        source_name: FileNameV1,
        destination_parent: FileRecordId,
        destination_name: FileNameV1,
        source_record: FileRecordId,
        replaced_record: FileRecordId,
    },
    KeepBothContentConflict {
        source_record: FileRecordId,
        sibling_record: FileRecordId,
        parent_record: FileRecordId,
        source_name: FileNameV1,
        sibling_name: FileNameV1,
        conflict_id: Uuid,
        conflict_versions: Vec<FileVersionId>,
        selected_version: FileVersionId,
    },
    MergeContentConflict {
        source_record: FileRecordId,
        parent_record: FileRecordId,
        source_name: FileNameV1,
        conflict_id: Uuid,
        conflict_versions: Vec<FileVersionId>,
        current_version: FileVersionId,
        base_version: Option<FileVersionId>,
        codec: ConflictMergeCodecV1,
    },
    AtomicSave {
        parent_record: FileRecordId,
        document_name: FileNameV1,
        temporary_name: FileNameV1,
        document_record: FileRecordId,
        temporary_record: FileRecordId,
        prior_document_version: FileVersionId,
    },
}

/// Canonical manifest signed once and copied unchanged into every prepare.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMutationTransactionManifest {
    pub schema_version: u16,
    pub transaction_id: Uuid,
    pub workspace_id: Uuid,
    pub actor: FileActor,
    pub timestamp_ms: u64,
    pub operation: FileMutationOperation,
    /// Strictly sorted by stable record ID, with no duplicates.
    pub participants: Vec<FileMutationParticipant>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct TransactionSigningView<'a> {
    domain: &'static str,
    manifest: &'a FileMutationTransactionManifest,
}

impl FileMutationTransactionManifest {
    /// Validate the complete, operation-specific atomic transaction manifest.
    #[allow(
        clippy::too_many_lines,
        reason = "keeping the mutually exclusive signed operation variants in one validator makes their complete invariants auditable"
    )]
    pub fn validate(&self) -> Result<(), FileRecordError> {
        if !matches!(
            self.schema_version,
            FILE_MUTATION_TRANSACTION_VERSION
                | FILE_MUTATION_TRANSACTION_VERSION_V2
                | FILE_MUTATION_TRANSACTION_VERSION_V3
                | FILE_MUTATION_TRANSACTION_VERSION_V4
        ) {
            return Err(FileRecordError::UnsupportedMutationTransactionVersion(
                self.schema_version,
            ));
        }
        if self.transaction_id.is_nil() || self.workspace_id.is_nil() {
            return Err(FileRecordError::InvalidMutationTransactionScope);
        }
        if self.participants.is_empty()
            || self.participants.len() > MAX_TRANSACTION_PARTICIPANTS
            || self
                .participants
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left.record_id >= right.record_id))
        {
            return Err(FileRecordError::InvalidTransactionParticipants);
        }
        for participant in &self.participants {
            participant.validate(self.actor)?;
        }
        match &self.operation {
            FileMutationOperation::RestoreContent {
                request,
                selected_version,
                prior_revision,
                compensates,
            } => {
                request
                    .validate()
                    .map_err(|_| FileRecordError::InvalidTransactionParticipant)?;
                prior_revision.validate()?;
                let participant = self
                    .participants
                    .first()
                    .ok_or(FileRecordError::InvalidTransactionParticipants)?;
                let action_valid = match request.operation {
                    CheckedHistoryOperationV1::Restore { version_id } => {
                        version_id == *selected_version && compensates.is_none()
                    }
                    CheckedHistoryOperationV1::Undo { restore_intent } => {
                        *compensates == Some(restore_intent)
                    }
                    CheckedHistoryOperationV1::Redo { undo_intent } => {
                        *compensates == Some(undo_intent)
                    }
                };
                if self.schema_version != FILE_MUTATION_TRANSACTION_VERSION_V3
                    || self.participants.len() != 1
                    || request.intent_id != self.transaction_id
                    || request.review.workspace_id != self.workspace_id
                    || request.review.record_id != participant.record_id
                    || prior_revision.record_id != participant.record_id
                    || prior_revision.version_id != participant.expected_version
                    || selected_version.0.is_nil()
                    || participant.role != FileMutationParticipantRole::RestoreContent
                    || participant.proposed_version.provenance
                        != (FileProvenance::Restore {
                            from: *selected_version,
                        })
                    || participant.proposed_version.timestamp_ms != self.timestamp_ms
                    || !action_valid
                {
                    return Err(FileRecordError::InvalidTransactionParticipant);
                }
                Ok(())
            }
            FileMutationOperation::AtomicSave {
                parent_record,
                document_name,
                temporary_name,
                document_record,
                temporary_record,
                prior_document_version,
            } => {
                if self.schema_version != FILE_MUTATION_TRANSACTION_VERSION_V4
                    || self.participants.len() != 2
                    || parent_record.0.is_nil()
                    || document_record.0.is_nil()
                    || temporary_record.0.is_nil()
                    || document_record == temporary_record
                    || parent_record == document_record
                    || parent_record == temporary_record
                    || document_name == temporary_name
                    || prior_document_version.0.is_nil()
                    || !valid_transaction_name(document_name)
                    || !valid_transaction_name(temporary_name)
                {
                    return Err(FileRecordError::InvalidReplacementRename);
                }
                let document = self
                    .participants
                    .iter()
                    .find(|p| p.role == FileMutationParticipantRole::AtomicSaveDocument)
                    .ok_or(FileRecordError::InvalidTransactionParticipants)?;
                let temporary = self
                    .participants
                    .iter()
                    .find(|p| p.role == FileMutationParticipantRole::AtomicSaveTemporary)
                    .ok_or(FileRecordError::InvalidTransactionParticipants)?;
                if document.record_id != *document_record
                    || temporary.record_id != *temporary_record
                    || document.expected_version != *prior_document_version
                    || document.proposed_binding.parent != Some(*parent_record)
                    || document.proposed_binding.name != *document_name
                    || temporary.proposed_binding.parent != Some(*parent_record)
                    || temporary.proposed_binding.name != *temporary_name
                    || document.proposed_version.timestamp_ms != self.timestamp_ms
                    || temporary.proposed_version.timestamp_ms != self.timestamp_ms
                {
                    return Err(FileRecordError::InvalidTransactionParticipant);
                }
                Ok(())
            }
            FileMutationOperation::ReplacementRename {
                source_parent,
                source_name,
                destination_parent,
                destination_name,
                source_record,
                replaced_record,
            } => {
                if self.schema_version != FILE_MUTATION_TRANSACTION_VERSION
                    || source_parent.0.is_nil()
                    || destination_parent.0.is_nil()
                    || source_record == replaced_record
                    || !valid_transaction_name(source_name)
                    || !valid_transaction_name(destination_name)
                {
                    return Err(FileRecordError::InvalidReplacementRename);
                }
                let source = self
                    .participants
                    .iter()
                    .find(|participant| participant.role == FileMutationParticipantRole::Source);
                let replaced = self
                    .participants
                    .iter()
                    .find(|participant| participant.role == FileMutationParticipantRole::Replaced);
                let (Some(source), Some(replaced)) = (source, replaced) else {
                    return Err(FileRecordError::InvalidTransactionParticipants);
                };
                if source.record_id != *source_record
                    || replaced.record_id != *replaced_record
                    || source.proposed_binding.parent != Some(*destination_parent)
                    || source.proposed_binding.name != *destination_name
                    || replaced.proposed_binding.parent != Some(*destination_parent)
                    || replaced.proposed_binding.name != *destination_name
                {
                    return Err(FileRecordError::InvalidReplacementRename);
                }
                Ok(())
            }
            FileMutationOperation::KeepBothContentConflict {
                source_record,
                sibling_record,
                parent_record,
                source_name,
                sibling_name,
                conflict_id,
                conflict_versions,
                selected_version,
            } => {
                if self.schema_version != FILE_MUTATION_TRANSACTION_VERSION_V2
                    || source_record == sibling_record
                    || parent_record.0.is_nil()
                    || conflict_id.is_nil()
                    || conflict_versions.len() != 2
                    || !conflict_versions.contains(selected_version)
                    || conflict_versions
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != conflict_versions.len()
                    || !valid_transaction_name(source_name)
                    || !valid_transaction_name(sibling_name)
                    || source_name == sibling_name
                {
                    return Err(FileRecordError::InvalidKeepBothTransaction);
                }
                let source = self.participants.iter().find(|participant| {
                    participant.role == FileMutationParticipantRole::KeepBothSource
                });
                let sibling = self.participants.iter().find(|participant| {
                    participant.role == FileMutationParticipantRole::KeepBothSibling
                });
                let (Some(source), Some(sibling)) = (source, sibling) else {
                    return Err(FileRecordError::InvalidTransactionParticipants);
                };
                if source.record_id != *source_record
                    || sibling.record_id != *sibling_record
                    || source.proposed_binding.parent != Some(*parent_record)
                    || source.proposed_binding.name != *source_name
                    || sibling.proposed_binding.parent != Some(*parent_record)
                    || sibling.proposed_binding.name != *sibling_name
                    || sibling.expected_version != *selected_version
                    || !conflict_versions.contains(&source.expected_version)
                    || *selected_version == source.expected_version
                    || !conflict_versions
                        .iter()
                        .all(|version| source.proposed_version.parents.contains(version))
                    || source.proposed_version.timestamp_ms != self.timestamp_ms
                    || sibling.proposed_version.timestamp_ms != self.timestamp_ms
                {
                    return Err(FileRecordError::InvalidKeepBothTransaction);
                }
                Ok(())
            }
            FileMutationOperation::MergeContentConflict {
                source_record,
                parent_record,
                source_name,
                conflict_id,
                conflict_versions,
                current_version,
                base_version,
                codec,
            } => {
                let unique = conflict_versions
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>();
                if self.schema_version != FILE_MUTATION_TRANSACTION_VERSION_V2
                    || self.participants.len() != 1
                    || parent_record.0.is_nil()
                    || conflict_id.is_nil()
                    || conflict_versions.len() != 2
                    || unique.len() != 2
                    || !conflict_versions.contains(current_version)
                    || base_version.is_some_and(|base| unique.contains(&base))
                    || !valid_transaction_name(source_name)
                    || matches!(codec, ConflictMergeCodecV1::PlainUtf8ThreeWayV1)
                        && base_version.is_none()
                {
                    return Err(FileRecordError::InvalidMergeTransaction);
                }
                let Some(source) = self.participants.first() else {
                    return Err(FileRecordError::InvalidMergeTransaction);
                };
                if source.role != FileMutationParticipantRole::MergeSource
                    || source.record_id != *source_record
                    || source.expected_version != *current_version
                    || source.proposed_binding.parent != Some(*parent_record)
                    || source.proposed_binding.name != *source_name
                    || !conflict_versions
                        .iter()
                        .all(|version| source.proposed_version.parents.contains(version))
                    || source.proposed_version.timestamp_ms != self.timestamp_ms
                {
                    return Err(FileRecordError::InvalidMergeTransaction);
                }
                Ok(())
            }
        }
    }
}

fn valid_transaction_name(name: &FileNameV1) -> bool {
    valid_file_name(name)
}

fn valid_file_name(name: &FileNameV1) -> bool {
    let bytes = name.as_bytes();
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_BYTES
        && !matches!(bytes, b"." | b"..")
        && !bytes.contains(&b'/')
        && !bytes.contains(&0)
        && match std::str::from_utf8(bytes) {
            Ok(text) => !text.chars().any(char::is_control),
            Err(_) => !bytes.iter().any(u8::is_ascii_control),
        }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMutationTransaction {
    pub manifest: FileMutationTransactionManifest,
    pub signature: Signature,
}

impl FileMutationTransaction {
    fn signing_view(&self) -> TransactionSigningView<'_> {
        TransactionSigningView {
            domain: if self.manifest.schema_version == FILE_MUTATION_TRANSACTION_VERSION_V4 {
                FILE_MUTATION_TRANSACTION_SIGNATURE_DOMAIN_V4
            } else if self.manifest.schema_version == FILE_MUTATION_TRANSACTION_VERSION_V3 {
                "tilo.collaborative-fs.file-mutation-transaction.v3"
            } else if self.manifest.schema_version == FILE_MUTATION_TRANSACTION_VERSION_V2 {
                FILE_MUTATION_TRANSACTION_SIGNATURE_DOMAIN_V2
            } else {
                FILE_MUTATION_TRANSACTION_SIGNATURE_DOMAIN
            },
            manifest: &self.manifest,
        }
    }

    /// Validate and sign an atomic file-mutation transaction.
    pub fn signed(
        manifest: FileMutationTransactionManifest,
        signing_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, FileMutationTransactionError> {
        manifest.validate()?;
        if signing_key.verifying_key().to_bytes() != *manifest.actor.device.as_bytes() {
            return Err(FileMutationTransactionError::WrongSigningKey);
        }
        let mut transaction = Self {
            manifest,
            signature: Signature::from_bytes([0; 64]),
        };
        transaction.signature = sign_event(&transaction.signing_view(), signing_key)?;
        Ok(transaction)
    }

    /// Validate the manifest and verify its device signature.
    pub fn verify(&self) -> Result<(), FileMutationTransactionError> {
        self.manifest.validate()?;
        verify_event(
            &self.signing_view(),
            &self.signature,
            &self.manifest.actor.device,
        )?;
        Ok(())
    }

    /// Return the canonical wire bytes for this signed transaction.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }
}

#[derive(Debug, Error)]
pub enum FileMutationTransactionError {
    #[error("transaction signing key does not match its actor device")]
    WrongSigningKey,
    #[error(transparent)]
    InvalidManifest(#[from] FileRecordError),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Signature(#[from] VerifyError),
}

/// An immutable history node. Restoring creates another node; history is never
/// rewritten in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVersionV1 {
    pub id: FileVersionId,
    pub record_id: FileRecordId,
    pub parents: Vec<FileVersionId>,
    pub actor: FileActor,
    pub timestamp_ms: u64,
    pub content_hash: Option<ContentHash>,
    pub metadata_hash: ContentHash,
    pub provenance: FileProvenance,
}

impl FileVersionV1 {
    /// Validate bounded, acyclic parent references.
    pub fn validate(&self) -> Result<(), FileRecordError> {
        if self.parents.len() > MAX_VERSION_PARENTS {
            return Err(FileRecordError::TooManyParents);
        }
        if self.parents.contains(&self.id) {
            return Err(FileRecordError::SelfParent);
        }
        Ok(())
    }
}

/// Most bytes an attribution's actor or label carries.
pub const MAX_ATTRIBUTION_TEXT_BYTES: usize = 128;

/// Who caused a version when it was not the signing person themselves: a
/// delegated agent that acted for that person through their session. The
/// person it acted for is always the version's own `actor.person`, so an
/// attribution can never name anybody else. The signing device's session
/// took the agent from its authenticated action context, never from an app,
/// and the device signature covers it ([`FileVersionAttributionV1`]).
/// Presentation only: it grants nothing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileAttributionV1 {
    /// The acting agent's principal, `agent:<id>`.
    pub actor: String,
    /// How the agent is shown.
    pub label: String,
    /// The agent's presence colour, sRGB.
    pub color: [u8; 3],
}

impl FileAttributionV1 {
    /// The agent's id: its principal without the `agent:` namespace.
    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        self.actor.strip_prefix("agent:")
    }

    /// Validate a bounded, presentable agent attribution.
    ///
    /// # Errors
    ///
    /// Returns [`FileRecordError::InvalidAttribution`] unless the actor is an
    /// `agent:<id>` principal and both it and the label are non-empty,
    /// bounded and free of control characters.
    pub fn validate(&self) -> Result<(), FileRecordError> {
        let presentable = |text: &str| {
            !text.trim().is_empty()
                && text.len() <= MAX_ATTRIBUTION_TEXT_BYTES
                && !text.chars().any(char::is_control)
        };
        if self
            .agent_id()
            .is_some_and(|id| presentable(id) && id.trim() == id)
            && presentable(&self.actor)
            && presentable(&self.label)
        {
            Ok(())
        } else {
            Err(FileRecordError::InvalidAttribution)
        }
    }
}

/// The attribution of one version of a record, as the version's own signing
/// device recorded it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVersionAttributionV1 {
    pub version_id: FileVersionId,
    pub attribution: FileAttributionV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ConflictStatus {
    Open,
    Resolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConflictV1 {
    pub id: Uuid,
    pub record_id: FileRecordId,
    pub versions: Vec<FileVersionId>,
    pub status: ConflictStatus,
    pub resolution: Option<ConflictResolution>,
}

impl FileConflictV1 {
    /// Validate the bounded conflict membership and status/resolution pairing.
    pub fn validate(&self) -> Result<(), FileRecordError> {
        if self.versions.len() < 2 {
            return Err(FileRecordError::ConflictNeedsTwoVersions);
        }
        if self.versions.len() > MAX_CONFLICT_VERSIONS {
            return Err(FileRecordError::TooManyConflictVersions);
        }
        let unique = self
            .versions
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if unique.len() != self.versions.len() {
            return Err(FileRecordError::DuplicateConflictVersion);
        }
        match (self.status, self.resolution.as_ref()) {
            (ConflictStatus::Open, None) | (ConflictStatus::Resolved, Some(_)) => Ok(()),
            (ConflictStatus::Open, Some(_)) => Err(FileRecordError::OpenConflictHasResolution),
            (ConflictStatus::Resolved, None) => {
                Err(FileRecordError::ResolvedConflictNeedsResolution)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictResolution {
    pub strategy: ConflictStrategy,
    pub result: FileVersionId,
    pub actor: FileActor,
    pub timestamp_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ConflictStrategy {
    KeepCurrent,
    UseOther,
    KeepBoth,
    Merge,
}

/// Deterministically selects between concurrently authored resolution
/// proposals. This is an arrival-order-independent compatibility rule for the
/// V1 single-resolution projection; a future event graph can preserve undo
/// history without changing which current proposal is shown.
#[must_use]
pub fn prefer_conflict_resolution(
    candidate: &ConflictResolution,
    current: &ConflictResolution,
) -> bool {
    fn strategy_rank(strategy: ConflictStrategy) -> u8 {
        match strategy {
            ConflictStrategy::KeepCurrent => 0,
            ConflictStrategy::UseOther => 1,
            ConflictStrategy::KeepBoth => 2,
            ConflictStrategy::Merge => 3,
        }
    }
    (
        candidate.timestamp_ms,
        candidate.actor.person.as_bytes(),
        candidate.actor.device.as_bytes(),
        candidate.result.0.as_bytes(),
        strategy_rank(candidate.strategy),
    ) > (
        current.timestamp_ms,
        current.actor.person.as_bytes(),
        current.actor.device.as_bytes(),
        current.result.0.as_bytes(),
        strategy_rank(current.strategy),
    )
}

/// Versioned, bounded request written to the collaborative filesystem's
/// history-control extended attribute. JSON is used at this local UI boundary
/// so requests remain inspectable; record replication continues to use the
/// canonical signed wire format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryControlRequestV1 {
    pub schema_version: u16,
    pub operation: HistoryControlOperationV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", deny_unknown_fields)]
pub enum HistoryControlOperationV1 {
    /// Resolve one open first-class conflict by appending a separately signed
    /// immutable result version. `version_id` is the exact conflict member
    /// whose bytes become the result; even `KeepCurrent` names it explicitly so
    /// the daemon can reject a stale UI decision rather than reinterpret it.
    ResolveConflict {
        conflict_id: Uuid,
        strategy: ConflictStrategy,
        version_id: FileVersionId,
    },
}

pub const HISTORY_CONTROL_VERSION: u16 = 1;
pub const MAX_HISTORY_CONTROL_BYTES: usize = 1_024;
/// Read-only dynamic xattr: append a version UUID to request immutable bytes.
pub const HISTORY_PREVIEW_XATTR_PREFIX: &str = "user.tilo.history-preview.";
/// Fits below Linux's 64 KiB xattr ceiling with room for transport overhead.
pub const MAX_HISTORY_PREVIEW_BYTES: usize = 48 * 1024;
/// Read-only dynamic xattr: append an open conflict UUID to request a
/// non-committing deterministic merge classification.
pub const MERGE_PREFLIGHT_XATTR_PREFIX: &str = "user.tilo.merge-preflight.";
pub const MERGE_PREFLIGHT_VERSION: u16 = 1;
pub const MAX_MERGE_PREFLIGHT_BYTES: usize = 4 * 1024;

/// Bounded daemon-authored explanation used by Files before it enables a
/// merge. The projection never contains mutable working-copy bytes; its
/// `diff_summary` only describes the exact immutable conflict members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePreflightProjectionV1 {
    pub schema_version: u16,
    pub conflict_id: Uuid,
    pub safe: bool,
    pub explanation: String,
    pub diff_summary: String,
}

impl MergePreflightProjectionV1 {
    /// Validates that this projection is bounded and addresses a real conflict.
    ///
    /// # Errors
    ///
    /// Returns [`FileRecordError::InvalidMergePreflight`] when the schema,
    /// conflict identifier, or user-facing explanation is invalid.
    pub fn validate(&self) -> Result<(), FileRecordError> {
        if self.schema_version != MERGE_PREFLIGHT_VERSION
            || self.conflict_id.is_nil()
            || self.explanation.is_empty()
            || self.explanation.len() > 512
            || self.diff_summary.len() > 1_024
        {
            return Err(FileRecordError::InvalidMergePreflight);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum FileRecordError {
    #[error("unsupported file metadata version {0}")]
    UnsupportedVersion(u16),
    #[error("unsupported file properties version {0}")]
    UnsupportedPropertiesVersion(u16),
    #[error("image dimensions are incomplete or outside the wire bound")]
    InvalidImageDimensions,
    #[error("media duration is outside the wire bound")]
    InvalidMediaDuration,
    #[error("document page count is outside the wire bound")]
    InvalidDocumentPageCount,
    #[error("codec or container token is invalid")]
    InvalidMetadataToken,
    #[error("file properties record is empty")]
    EmptyFileProperties,
    #[error("unsupported content revision version {0}")]
    UnsupportedContentRevisionVersion(u16),
    #[error("unsupported file binding version {0}")]
    UnsupportedBindingVersion(u16),
    #[error("unsupported file mutation transaction version {0}")]
    UnsupportedMutationTransactionVersion(u16),
    #[error("file mutation transaction has an invalid id or workspace")]
    InvalidMutationTransactionScope,
    #[error("file mutation transaction participants must be exact, unique, and sorted")]
    InvalidTransactionParticipants,
    #[error("file mutation transaction participant projection is inconsistent")]
    InvalidTransactionParticipant,
    #[error("replacement rename transaction structure is inconsistent")]
    InvalidReplacementRename,
    #[error("keep-both transaction structure is inconsistent")]
    InvalidKeepBothTransaction,
    #[error("invalid merge-conflict transaction")]
    InvalidMergeTransaction,
    #[error("invalid merge preflight projection")]
    InvalidMergePreflight,
    #[error("content revision has a nil record id")]
    NilRecordId,
    #[error("content revision has a nil version id")]
    NilVersionId,
    #[error("file binding has a nil parent record id")]
    NilParentRecordId,
    #[error("file binding cannot name itself as parent")]
    BindingSelfParent,
    #[error("workspace root binding is invalid")]
    InvalidRootBinding,
    #[error("file binding name is invalid")]
    InvalidBindingName,
    #[error("MIME value exceeds the wire bound")]
    MimeTooLong,
    #[error("too many preview variants")]
    TooManyPreviews,
    #[error("too many version parents")]
    TooManyParents,
    #[error("a version cannot be its own parent")]
    SelfParent,
    #[error("a conflict must name at least two versions")]
    ConflictNeedsTwoVersions,
    #[error("too many versions in one conflict")]
    TooManyConflictVersions,
    #[error("a conflict names the same version more than once")]
    DuplicateConflictVersion,
    #[error("an open conflict cannot have a resolution")]
    OpenConflictHasResolution,
    #[error("a resolved conflict requires a resolution")]
    ResolvedConflictNeedsResolution,
    #[error("a version attribution must name a presentable agent")]
    InvalidAttribution,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    /// Metadata v2 round-trips rich properties and migrates v1; rich properties enforce strict
    /// bounds; metadata is versioned, bounded and canonical; a content revision is one canonical
    /// bounded pointer.
    #[test]
    fn file_metadata_is_versioned_bounded_and_canonical() {
        // Phase: metadata_v2_round_trips_rich_properties_and_v1_migrates.
        {
            let mut metadata = FileMetadataV1 {
                schema_version: FILE_METADATA_VERSION,
                record_id: FileRecordId::new(),
                content_hash: None,
                byte_len: 12,
                mime: Some("image/png".into()),
                previews: Vec::new(),
                latest_actor: None,
                observed_at_ms: 1,
                index_status: IndexStatus::Indexed,
                properties: Some(FilePropertiesV1 {
                    schema_version: FILE_PROPERTIES_VERSION,
                    image_width: Some(640),
                    image_height: Some(480),
                    duration_ms: None,
                    page_count: None,
                    codec: Some("png".into()),
                    container: Some("png".into()),
                }),
            };
            let encoded = postcard::to_allocvec(&metadata).unwrap();
            assert_eq!(
                postcard::from_bytes::<FileMetadataV1>(&encoded).unwrap(),
                metadata
            );
            let json = serde_json::to_vec(&metadata).unwrap();
            assert_eq!(
                serde_json::from_slice::<FileMetadataV1>(&json).unwrap(),
                metadata
            );

            metadata.schema_version = FILE_METADATA_LEGACY_VERSION;
            metadata.properties = None;
            let legacy = postcard::to_allocvec(&metadata).unwrap();
            let migrated: FileMetadataV1 = postcard::from_bytes(&legacy).unwrap();
            assert_eq!(migrated.schema_version, FILE_METADATA_LEGACY_VERSION);
            assert_eq!(migrated.properties, None);
            migrated.validate().unwrap();
        }

        // Phase: rich_properties_enforce_strict_bounds.
        {
            let invalid = FilePropertiesV1 {
                schema_version: FILE_PROPERTIES_VERSION,
                image_width: Some(MAX_IMAGE_DIMENSION + 1),
                image_height: Some(1),
                duration_ms: None,
                page_count: None,
                codec: Some("png".into()),
                container: None,
            };
            assert_eq!(
                invalid.validate(),
                Err(FileRecordError::InvalidImageDimensions)
            );
        }

        // Phase: metadata_is_versioned_bounded_and_canonical.
        {
            let metadata = FileMetadataV1 {
                schema_version: FILE_METADATA_VERSION,
                record_id: FileRecordId::new(),
                content_hash: Some(ContentHash([3; 32])),
                byte_len: 42,
                mime: Some("image/png".into()),
                previews: vec![PreviewVariant {
                    max_edge: 256,
                    hash: Some(ContentHash([4; 32])),
                    status: PreviewStatus::Ready,
                }],
                latest_actor: Some(actor()),
                observed_at_ms: 9,
                index_status: IndexStatus::Indexed,
                properties: None,
            };
            metadata.validate().unwrap();
            let bytes = collab_types::canonical::canonical_bytes(&metadata).unwrap();
            let decoded: FileMetadataV1 =
                collab_types::canonical::from_canonical_bytes(&bytes).unwrap();
            assert_eq!(decoded, metadata);
        }

        // Phase: content_revision_is_one_canonical_bounded_pointer.
        {
            let revision = ContentRevisionV1 {
                schema_version: CONTENT_REVISION_VERSION,
                record_id: FileRecordId::new(),
                version_id: FileVersionId::new(),
                content_hash: Some(ContentHash([8; 32])),
                byte_len: 4096,
                metadata_hash: ContentHash([9; 32]),
            };
            revision.validate().unwrap();
            let bytes = collab_types::canonical::canonical_bytes(&revision).unwrap();
            let decoded: ContentRevisionV1 =
                collab_types::canonical::from_canonical_bytes(&bytes).unwrap();
            assert_eq!(decoded, revision);
        }
    }

    fn actor() -> FileActor {
        FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        }
    }

    /// Byte-name encoding migrates old UTF-8 bindings without changing bytes; bindings accept non-UTF-8
    /// but reject unix and control separators; structural bindings are bounded and cannot forge roots
    /// or self-parent; a record scope survives paths and matches only its record.
    #[test]
    fn file_bindings_keep_exact_bytes_and_cannot_forge_structure() {
        // Phase: byte_name_encoding_migrates_old_utf8_binding_without_changing_bytes.
        {
            #[derive(Serialize)]
            struct LegacyBinding {
                schema_version: u16,
                record_id: FileRecordId,
                parent: Option<FileRecordId>,
                name: String,
                kind: FileNodeKind,
                tombstoned: bool,
                actor: FileActor,
                observed_at_ms: u64,
            }
            let record_id = FileRecordId::new();
            let parent = FileRecordId::new();
            let legacy = LegacyBinding {
                schema_version: FILE_BINDING_VERSION,
                record_id,
                parent: Some(parent),
                name: "Grüße.png".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor: actor(),
                observed_at_ms: 7,
            };
            let old_bytes = canonical_bytes(&legacy).unwrap();
            let migrated: FileBindingV1 =
                collab_types::canonical::from_canonical_bytes(&old_bytes).unwrap();
            assert_eq!(migrated.name.as_bytes(), "Grüße.png".as_bytes());
            assert_eq!(canonical_bytes(&migrated).unwrap(), old_bytes);
        }

        // Phase: binding_accepts_non_utf8_but_rejects_unix_and_control_separators.
        {
            let binding = |name: Vec<u8>| FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id: FileRecordId::new(),
                parent: Some(FileRecordId::new()),
                name: name.into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor: actor(),
                observed_at_ms: 1,
            };
            assert!(binding(b"raw-\xFF".to_vec()).validate().is_ok());
            assert_eq!(
                binding(b"bad/name".to_vec()).validate(),
                Err(FileRecordError::InvalidBindingName)
            );
            assert_eq!(
                binding(b"bad\0name".to_vec()).validate(),
                Err(FileRecordError::InvalidBindingName)
            );
        }

        // Phase: structural_bindings_are_bounded_and_cannot_forge_roots_or_self_parent.
        {
            let record_id = FileRecordId::new();
            let parent = FileRecordId::new();
            let mut binding = FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id,
                parent: Some(parent),
                name: "report.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor: actor(),
                observed_at_ms: 7,
            };
            binding.validate().unwrap();
            let bytes = collab_types::canonical::canonical_bytes(&binding).unwrap();
            assert_eq!(
                collab_types::canonical::from_canonical_bytes::<FileBindingV1>(&bytes).unwrap(),
                binding
            );
            binding.parent = Some(record_id);
            assert_eq!(binding.validate(), Err(FileRecordError::BindingSelfParent));
            binding.parent = None;
            assert_eq!(binding.validate(), Err(FileRecordError::InvalidRootBinding));
            binding.parent = Some(parent);
            binding.name = "../escape".into();
            assert_eq!(binding.validate(), Err(FileRecordError::InvalidBindingName));
        }

        // Phase: record_scope_survives_paths_and_matches_only_its_record.
        {
            let record = FileRecordId::new();
            let other = FileRecordId::new();
            let scope = collab_types::Scope::FsRecord(record.0.to_string());
            assert!(
                scope.matches_resource(&collab_types::ResourceRef::FsRecord(record.0.to_string()))
            );
            assert!(
                !scope.matches_resource(&collab_types::ResourceRef::FsRecord(other.0.to_string()))
            );
            assert!(
                !scope.matches_resource(&collab_types::ResourceRef::FsPath("/renamed.txt".into()))
            );
        }
    }

    /// History rejects cycles and unbounded parent sets; conflicts are bounded, unique and have a
    /// consistent status.
    #[test]
    fn file_history_and_conflicts_are_acyclic_bounded_and_consistent() {
        // Phase: history_rejects_cycles_and_unbounded_parent_sets.
        {
            let id = FileVersionId::new();
            let mut version = FileVersionV1 {
                id,
                record_id: FileRecordId::new(),
                parents: vec![id],
                actor: actor(),
                timestamp_ms: 1,
                content_hash: None,
                metadata_hash: ContentHash([0; 32]),
                provenance: FileProvenance::Create,
            };
            assert_eq!(version.validate(), Err(FileRecordError::SelfParent));
            version.parents = (0..=MAX_VERSION_PARENTS)
                .map(|_| FileVersionId::new())
                .collect();
            assert_eq!(version.validate(), Err(FileRecordError::TooManyParents));
        }

        // Phase: conflicts_are_bounded_unique_and_have_consistent_status.
        {
            let first = FileVersionId::new();
            let second = FileVersionId::new();
            let mut conflict = FileConflictV1 {
                id: Uuid::new_v4(),
                record_id: FileRecordId::new(),
                versions: vec![first, second],
                status: ConflictStatus::Open,
                resolution: None,
            };
            conflict.validate().unwrap();

            conflict.versions = vec![first, first];
            assert_eq!(
                conflict.validate(),
                Err(FileRecordError::DuplicateConflictVersion)
            );
            conflict.versions = vec![first, second];
            conflict.status = ConflictStatus::Resolved;
            assert_eq!(
                conflict.validate(),
                Err(FileRecordError::ResolvedConflictNeedsResolution)
            );
        }
    }

    fn transaction_participant(
        record_id: FileRecordId,
        role: FileMutationParticipantRole,
        parent: FileRecordId,
        name: &str,
        actor: FileActor,
    ) -> FileMutationParticipant {
        let expected_version = FileVersionId::new();
        let proposed_version = FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents: vec![expected_version],
            actor,
            timestamp_ms: 44,
            content_hash: None,
            metadata_hash: ContentHash([record_id.0.as_bytes()[0]; 32]),
            provenance: match role {
                FileMutationParticipantRole::Source => FileProvenance::Rename,
                FileMutationParticipantRole::AtomicSaveDocument => FileProvenance::Edit,
                FileMutationParticipantRole::Replaced
                | FileMutationParticipantRole::AtomicSaveTemporary => FileProvenance::Delete,
                _ => unreachable!("replacement fixture role"),
            },
        };
        let mut participant = FileMutationParticipant {
            record_id,
            role,
            expected_version,
            proposed_binding: FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id,
                parent: Some(parent),
                name: name.into(),
                kind: FileNodeKind::File,
                tombstoned: matches!(
                    role,
                    FileMutationParticipantRole::Replaced
                        | FileMutationParticipantRole::AtomicSaveTemporary
                ),
                actor,
                observed_at_ms: 44,
            },
            proposed_content_revision: Some(ContentRevisionV1 {
                schema_version: CONTENT_REVISION_VERSION,
                record_id,
                version_id: proposed_version.id,
                content_hash: proposed_version.content_hash,
                byte_len: 0,
                metadata_hash: proposed_version.metadata_hash,
            }),
            proposed_version,
            projection_hash: ContentHash([0; 32]),
        };
        participant.projection_hash = participant.computed_projection_hash().unwrap();
        participant
    }

    #[test]
    fn atomic_save_signed_manifest_keeps_document_and_rejects_role_or_prior_substitution() {
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let actor = FileActor {
            person: Ed25519PublicKey([9; 32]),
            device: Ed25519PublicKey(key.verifying_key().to_bytes()),
        };
        let parent = FileRecordId::new();
        let document = FileRecordId::new();
        let temporary = FileRecordId::new();
        let doc = transaction_participant(
            document,
            FileMutationParticipantRole::AtomicSaveDocument,
            parent,
            "report.txt",
            actor,
        );
        let prior = doc.expected_version;
        let mut participants = vec![
            doc,
            transaction_participant(
                temporary,
                FileMutationParticipantRole::AtomicSaveTemporary,
                parent,
                "save-temp",
                actor,
            ),
        ];
        participants.sort_by_key(|p| p.record_id);
        let manifest = FileMutationTransactionManifest {
            schema_version: FILE_MUTATION_TRANSACTION_VERSION_V4,
            transaction_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            actor,
            timestamp_ms: 44,
            operation: FileMutationOperation::AtomicSave {
                parent_record: parent,
                document_name: "report.txt".into(),
                temporary_name: "save-temp".into(),
                document_record: document,
                temporary_record: temporary,
                prior_document_version: prior,
            },
            participants,
        };
        let signed = FileMutationTransaction::signed(manifest.clone(), &key).unwrap();
        signed.verify().unwrap();
        let doc = signed
            .manifest
            .participants
            .iter()
            .find(|p| p.record_id == document)
            .unwrap();
        assert_eq!(doc.proposed_version.parents, [prior]);
        assert_eq!(doc.proposed_version.provenance, FileProvenance::Edit);
        assert!(!doc.proposed_binding.tombstoned);
        let mut changed = manifest.clone();
        changed.schema_version = FILE_MUTATION_TRANSACTION_VERSION;
        assert!(changed.validate().is_err());
        let mut changed = manifest.clone();
        if let FileMutationOperation::AtomicSave {
            prior_document_version,
            ..
        } = &mut changed.operation
        {
            *prior_document_version = FileVersionId::new();
        }
        assert!(changed.validate().is_err());
        let mut changed = manifest.clone();
        if let FileMutationOperation::AtomicSave { parent_record, .. } = &mut changed.operation {
            *parent_record = document;
        }
        assert!(changed.validate().is_err());
        let mut changed = manifest.clone();
        let temporary = changed
            .participants
            .iter_mut()
            .find(|p| p.role == FileMutationParticipantRole::AtomicSaveTemporary)
            .unwrap();
        temporary.proposed_binding.kind = FileNodeKind::Directory;
        temporary.proposed_content_revision = None;
        temporary.projection_hash = temporary.computed_projection_hash().unwrap();
        assert!(
            changed.validate().is_err(),
            "atomic saves retire their own regular temporary file"
        );
        let mut changed = manifest;
        let doc = changed
            .participants
            .iter_mut()
            .find(|p| p.record_id == document)
            .unwrap();
        doc.role = FileMutationParticipantRole::Source;
        doc.proposed_version.provenance = FileProvenance::Rename;
        doc.projection_hash = doc.computed_projection_hash().unwrap();
        assert!(changed.validate().is_err());
    }

    /// Replacement transactions are signed, sorted and projection-bound; checked-restore manifests
    /// bind the review selection and exact result; merge manifests bind the exact conflict, current
    /// base and codec requirements.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn file_manifests_bind_exactly_what_was_reviewed() {
        // Phase: replacement_transaction_is_signed_sorted_and_projection_bound.
        {
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let actor = FileActor {
                person: Ed25519PublicKey([9; 32]),
                device: Ed25519PublicKey(key.verifying_key().to_bytes()),
            };
            let source_parent = FileRecordId::new();
            let destination_parent = FileRecordId::new();
            let source_record = FileRecordId::new();
            let replaced_record = FileRecordId::new();
            let mut participants = vec![
                transaction_participant(
                    source_record,
                    FileMutationParticipantRole::Source,
                    destination_parent,
                    "destination.txt",
                    actor,
                ),
                transaction_participant(
                    replaced_record,
                    FileMutationParticipantRole::Replaced,
                    destination_parent,
                    "destination.txt",
                    actor,
                ),
            ];
            participants.sort_by_key(|participant| participant.record_id);
            let manifest = FileMutationTransactionManifest {
                schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                transaction_id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
                actor,
                timestamp_ms: 44,
                operation: FileMutationOperation::ReplacementRename {
                    source_parent,
                    source_name: "source.txt".into(),
                    destination_parent,
                    destination_name: "destination.txt".into(),
                    source_record,
                    replaced_record,
                },
                participants,
            };
            let signed = FileMutationTransaction::signed(manifest, &key).unwrap();
            signed.verify().unwrap();

            let mut reordered = signed.clone();
            reordered.manifest.participants.reverse();
            assert!(matches!(
                reordered.verify(),
                Err(FileMutationTransactionError::InvalidManifest(
                    FileRecordError::InvalidTransactionParticipants
                ))
            ));

            let mut forged = signed;
            forged.manifest.participants[0].proposed_binding.name = "other.txt".into();
            assert!(forged.verify().is_err());
        }

        // Phase: checked_restore_manifest_binds_review_selection_and_exact_result.
        {
            let mut manifest = merge_manifest();
            manifest.schema_version = 3;
            let selected = FileVersionId::new();
            let participant = &mut manifest.participants[0];
            participant.role = FileMutationParticipantRole::RestoreContent;
            participant.proposed_version.provenance = FileProvenance::Restore { from: selected };
            participant.projection_hash = participant.computed_projection_hash().unwrap();
            let prior = ContentRevisionV1 {
                version_id: participant.expected_version,
                ..participant.proposed_content_revision.unwrap()
            };
            manifest.operation = FileMutationOperation::RestoreContent {
                request: CheckedHistoryRequestV1 {
                    schema_version: 1,
                    intent_id: manifest.transaction_id,
                    review: FileRestoreReviewV1 {
                        schema_version: 1,
                        workspace_id: manifest.workspace_id,
                        record_id: participant.record_id,
                        mount_lifetime: Uuid::new_v4(),
                        frontier: [9; 32],
                    },
                    operation: CheckedHistoryOperationV1::Restore {
                        version_id: selected,
                    },
                },
                selected_version: selected,
                prior_revision: prior,
                compensates: None,
            };
            let mut mislabeled_v1 = manifest.clone();
            mislabeled_v1.schema_version = FILE_MUTATION_TRANSACTION_VERSION;
            assert_eq!(
                mislabeled_v1.validate(),
                Err(FileRecordError::InvalidTransactionParticipant)
            );
            manifest.validate().unwrap();
            let mut forged = manifest.clone();
            if let FileMutationOperation::RestoreContent { request, .. } = &mut forged.operation {
                request.review.record_id = FileRecordId::new();
            }
            assert!(forged.validate().is_err());
            let key = SigningKey::from_bytes(&[42; 32]);
            let signed = FileMutationTransaction::signed(manifest, &key).unwrap();
            signed.verify().unwrap();
        }

        // Phase: merge_manifest_binds_exact_conflict_current_base_and_codec_requirements.
        {
            let valid = merge_manifest();
            valid.validate().unwrap();

            let mut tilo_doc = valid.clone();
            if let FileMutationOperation::MergeContentConflict {
                base_version,
                codec,
                ..
            } = &mut tilo_doc.operation
            {
                *base_version = None;
                *codec = ConflictMergeCodecV1::TiloDocAutomergeV1;
            }
            tilo_doc.validate().unwrap();

            let mut stale_current = valid.clone();
            if let FileMutationOperation::MergeContentConflict {
                current_version, ..
            } = &mut stale_current.operation
            {
                *current_version = FileVersionId::new();
            }
            assert_eq!(
                stale_current.validate(),
                Err(FileRecordError::InvalidMergeTransaction)
            );

            let mut missing_base = valid.clone();
            if let FileMutationOperation::MergeContentConflict { base_version, .. } =
                &mut missing_base.operation
            {
                *base_version = None;
            }
            assert_eq!(
                missing_base.validate(),
                Err(FileRecordError::InvalidMergeTransaction)
            );

            let mut member_as_base = valid;
            if let FileMutationOperation::MergeContentConflict {
                conflict_versions,
                base_version,
                ..
            } = &mut member_as_base.operation
            {
                *base_version = Some(conflict_versions[0]);
            }
            assert_eq!(
                member_as_base.validate(),
                Err(FileRecordError::InvalidMergeTransaction)
            );
        }
    }

    fn merge_manifest() -> FileMutationTransactionManifest {
        let key = SigningKey::from_bytes(&[42; 32]);
        let actor = FileActor {
            person: Ed25519PublicKey([7; 32]),
            device: Ed25519PublicKey(key.verifying_key().to_bytes()),
        };
        let record_id = FileRecordId::new();
        let parent = FileRecordId::new();
        let base = FileVersionId::new();
        let current = FileVersionId::new();
        let other = FileVersionId::new();
        let result = FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents: vec![current, other],
            actor,
            timestamp_ms: 50,
            content_hash: Some(ContentHash([8; 32])),
            metadata_hash: ContentHash([9; 32]),
            provenance: FileProvenance::ConflictResolution,
        };
        let mut participant = FileMutationParticipant {
            record_id,
            role: FileMutationParticipantRole::MergeSource,
            expected_version: current,
            proposed_binding: FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id,
                parent: Some(parent),
                name: "merge.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor,
                observed_at_ms: 50,
            },
            proposed_content_revision: Some(ContentRevisionV1 {
                schema_version: CONTENT_REVISION_VERSION,
                record_id,
                version_id: result.id,
                content_hash: result.content_hash,
                byte_len: 12,
                metadata_hash: result.metadata_hash,
            }),
            proposed_version: result,
            projection_hash: ContentHash([0; 32]),
        };
        participant.projection_hash = participant.computed_projection_hash().unwrap();
        FileMutationTransactionManifest {
            schema_version: FILE_MUTATION_TRANSACTION_VERSION_V2,
            transaction_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            actor,
            timestamp_ms: 50,
            operation: FileMutationOperation::MergeContentConflict {
                source_record: record_id,
                parent_record: parent,
                source_name: "merge.txt".into(),
                conflict_id: Uuid::new_v4(),
                conflict_versions: vec![current, other],
                current_version: current,
                base_version: Some(base),
                codec: ConflictMergeCodecV1::PlainUtf8ThreeWayV1,
            },
            participants: vec![participant],
        }
    }
}
