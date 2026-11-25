//! Stable, versioned wire records for collaborative files.

use serde::de;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

use collab_types::Ed25519PublicKey;

pub const FILE_PROPERTIES_VERSION: u16 = 1;
pub const FILE_BINDING_VERSION: u16 = 1;
pub const MAX_FILE_NAME_BYTES: usize = 255;
pub const MAX_METADATA_TOKEN_BYTES: usize = 64;
pub const MAX_IMAGE_DIMENSION: u32 = 1_000_000;
pub const MAX_MEDIA_DURATION_MS: u64 = 100 * 365 * 24 * 60 * 60 * 1_000;
pub const MAX_DOCUMENT_PAGES: u32 = 10_000_000;

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
pub struct FileDocumentId(pub Uuid);

impl FileDocumentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// A new record id for a node of a workspace's tree that lives in its chunk `chunk`
    /// ([`crate::tree::chunk_document_id`]): the chunk is part of the id, so every device that
    /// writes the node, or rebuilds it from its record, writes it into the same chunk.
    #[must_use]
    pub fn in_chunk(chunk: TreeChunk) -> Self {
        let mut bytes = *Uuid::new_v4().as_bytes();
        let (number, _) = bytes.split_at_mut(4);
        number.copy_from_slice(&chunk.to_be_bytes());
        // A version-8 UUID: its leading bits are this format's own.
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Self(Uuid::from_bytes(bytes))
    }

    /// The chunk of the tree a node with this record id lives in. An id no chunk was chosen
    /// for (one made with [`Self::new`], or derived) is in the first chunk.
    #[must_use]
    pub fn chunk(&self) -> TreeChunk {
        if self.0.get_version_num() != 8 {
            return FIRST_TREE_CHUNK;
        }
        let [a, b, c, d, ..] = *self.0.as_bytes();
        u32::from_be_bytes([a, b, c, d]).max(FIRST_TREE_CHUNK)
    }
}

/// The number of a chunk of a workspace's tree: the tree's own record is chunk 0 and holds
/// no node; every node lives in a chunk from [`FIRST_TREE_CHUNK`] on.
pub type TreeChunk = u32;

/// The first chunk that holds nodes.
pub const FIRST_TREE_CHUNK: TreeChunk = 1;

impl Default for FileDocumentId {
    fn default() -> Self {
        Self::new()
    }
}

/// A content's hash: the BLAKE3 digest of its manifest, which names the bytes wherever they
/// are kept. Readable formats carry it as hex, compact ones as its 32 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 32]);

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", hex::encode(self.0))
    }
}

impl Serialize for ContentHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex::encode(self.0))
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let text = String::deserialize(deserializer)?;
            let mut bytes = [0u8; 32];
            hex::decode_to_slice(&text, &mut bytes).map_err(de::Error::custom)?;
            Ok(Self(bytes))
        } else {
            <[u8; 32]>::deserialize(deserializer).map(Self)
        }
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
    pub document_id: FileDocumentId,
    /// `None` identifies the workspace root. Every other binding has a stable
    /// directory record as parent, including tombstones.
    pub parent: Option<FileDocumentId>,
    pub name: FileNameV1,
    pub kind: FileNodeKind,
    pub tombstoned: bool,
    pub actor: FileActor,
    pub observed_at_ms: u64,
}

impl FileBindingV1 {
    /// Validate the structural binding and its bounded exact-byte name.
    pub fn validate(&self) -> Result<(), FileDocumentError> {
        if self.schema_version != FILE_BINDING_VERSION {
            return Err(FileDocumentError::UnsupportedBindingVersion(
                self.schema_version,
            ));
        }
        if self.document_id.0.is_nil() {
            return Err(FileDocumentError::NilDocumentId);
        }
        if self.parent.is_some_and(|parent| parent.0.is_nil()) {
            return Err(FileDocumentError::NilParentDocumentId);
        }
        if self.parent == Some(self.document_id) {
            return Err(FileDocumentError::BindingSelfParent);
        }
        if self.parent.is_none() {
            if !self.name.is_empty() || self.kind != FileNodeKind::Directory || self.tombstoned {
                return Err(FileDocumentError::InvalidRootBinding);
            }
        } else if self.name.is_empty()
            || self.name.len() > MAX_FILE_NAME_BYTES
            || !valid_file_name(&self.name)
        {
            return Err(FileDocumentError::InvalidBindingName);
        }
        Ok(())
    }
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
    pub fn validate(&self) -> Result<(), FileDocumentError> {
        if self.schema_version != FILE_PROPERTIES_VERSION {
            return Err(FileDocumentError::UnsupportedPropertiesVersion(
                self.schema_version,
            ));
        }
        match (self.image_width, self.image_height) {
            (Some(w), Some(h))
                if w > 0 && h > 0 && w <= MAX_IMAGE_DIMENSION && h <= MAX_IMAGE_DIMENSION => {}
            (None, None) => {}
            _ => return Err(FileDocumentError::InvalidImageDimensions),
        }
        if self
            .duration_ms
            .is_some_and(|v| v == 0 || v > MAX_MEDIA_DURATION_MS)
        {
            return Err(FileDocumentError::InvalidMediaDuration);
        }
        if self
            .page_count
            .is_some_and(|v| v == 0 || v > MAX_DOCUMENT_PAGES)
        {
            return Err(FileDocumentError::InvalidDocumentPageCount);
        }
        for token in [&self.codec, &self.container].into_iter().flatten() {
            if token.is_empty()
                || token.len() > MAX_METADATA_TOKEN_BYTES
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
            {
                return Err(FileDocumentError::InvalidMetadataToken);
            }
        }
        if self.image_width.is_none()
            && self.duration_ms.is_none()
            && self.page_count.is_none()
            && self.codec.is_none()
            && self.container.is_none()
        {
            return Err(FileDocumentError::EmptyFileProperties);
        }
        Ok(())
    }
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

/// What wrote a change for a person: an agent, in one of its sessions or not, a Tilo app, or a
/// third-party program. Presentation only: it grants nothing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributedWriter {
    /// An agent acting through Tilo actions, in one of its sessions or not.
    Agent {
        agent: String,
        session: Option<String>,
    },
    /// A Tilo app acting as itself, by its id.
    App(String),
    /// A third-party desktop program, by its desktop id.
    Program(String),
}

impl AttributedWriter {
    #[must_use]
    pub fn agent(agent: impl Into<String>) -> Self {
        Self::Agent {
            agent: agent.into(),
            session: None,
        }
    }

    /// One session of the agent `agent`.
    #[must_use]
    pub fn agent_session(agent: impl Into<String>, session: impl Into<String>) -> Self {
        Self::Agent {
            agent: agent.into(),
            session: Some(session.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum FileDocumentError {
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
    #[error("unsupported file binding version {0}")]
    UnsupportedBindingVersion(u16),
    #[error("the file has a nil document id")]
    NilDocumentId,
    #[error("file binding has a nil parent record id")]
    NilParentDocumentId,
    #[error("file binding cannot name itself as parent")]
    BindingSelfParent,
    #[error("workspace root binding is invalid")]
    InvalidRootBinding,
    #[error("file binding name is invalid")]
    InvalidBindingName,
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::canonical_bytes;

    /// Rich properties enforce strict bounds.
    #[test]
    fn rich_properties_enforce_strict_bounds() {
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
            Err(FileDocumentError::InvalidImageDimensions)
        );
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
        use FileDocumentError::{BindingSelfParent, InvalidBindingName, InvalidRootBinding};
        // Phase: byte_name_encoding_migrates_old_utf8_binding_without_changing_bytes.
        {
            #[derive(Serialize)]
            struct LegacyBinding {
                schema_version: u16,
                document_id: FileDocumentId,
                parent: Option<FileDocumentId>,
                name: String,
                kind: FileNodeKind,
                tombstoned: bool,
                actor: FileActor,
                observed_at_ms: u64,
            }
            let document_id = FileDocumentId::new();
            let parent = FileDocumentId::new();
            let legacy = LegacyBinding {
                schema_version: FILE_BINDING_VERSION,
                document_id,
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
                document_id: FileDocumentId::new(),
                parent: Some(FileDocumentId::new()),
                name: name.into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor: actor(),
                observed_at_ms: 1,
            };
            assert!(binding(b"raw-\xFF".to_vec()).validate().is_ok());
            assert_eq!(
                binding(b"bad/name".to_vec()).validate(),
                Err(FileDocumentError::InvalidBindingName)
            );
            assert_eq!(
                binding(b"bad\0name".to_vec()).validate(),
                Err(FileDocumentError::InvalidBindingName)
            );
        }

        // Phase: structural_bindings_are_bounded_and_cannot_forge_roots_or_self_parent.
        {
            let document_id = FileDocumentId::new();
            let parent = FileDocumentId::new();
            let mut binding = FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                document_id,
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
            binding.parent = Some(document_id);
            assert_eq!(binding.validate(), Err(BindingSelfParent));
            binding.parent = None;
            assert_eq!(binding.validate(), Err(InvalidRootBinding));
            binding.parent = Some(parent);
            binding.name = "../escape".into();
            assert_eq!(binding.validate(), Err(InvalidBindingName));
        }

        // Phase: record_scope_survives_paths_and_matches_only_its_record.
        {
            let record = FileDocumentId::new();
            let other = FileDocumentId::new();
            let scope = collab_types::Scope::FsResource(record.0.to_string());
            assert!(
                scope
                    .matches_resource(&collab_types::ResourceRef::FsResource(record.0.to_string()))
            );
            assert!(
                !scope
                    .matches_resource(&collab_types::ResourceRef::FsResource(other.0.to_string()))
            );
            assert!(
                !scope.matches_resource(&collab_types::ResourceRef::FsPath("/renamed.txt".into()))
            );
        }
    }
}
