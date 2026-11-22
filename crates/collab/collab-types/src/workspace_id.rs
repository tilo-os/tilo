//! A workspace's id: minted once as a random UUID and carried, never derived.

use serde::{Deserialize, Serialize};

/// Identifies a workspace everywhere: in wire messages, server routing, stored
/// keys and the desktop model. Every device that meets the workspace agrees on
/// it without having met the others.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct WorkspaceId(uuid::Uuid);

impl WorkspaceId {
    /// Mint a workspace's id, once, when the workspace is created.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// A workspace that has no id: a place the shell shows without holding a
    /// workspace.
    #[must_use]
    pub const fn nil() -> Self {
        Self(uuid::Uuid::nil())
    }

    /// The id a derivation of this crate's arrives at.
    pub(crate) const fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    #[must_use]
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    #[must_use]
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }

    /// The MLS wire uses the workspace-id field for document groups.
    /// These use a reserved UUID variant distinct from workspace UUIDs.
    #[must_use]
    pub const fn is_document_key_group(self) -> bool {
        self.0.as_bytes()[8] >> 5 == 7
    }

    /// The independent key group for a document in this workspace.
    #[must_use]
    pub fn document_key_group(self, app: &str, document: &str) -> Self {
        let mut hash = blake3::Hasher::new_derive_key("tilo document key group v1");
        hash.update(self.as_uuid().as_bytes());
        hash.update(&u64::try_from(app.len()).unwrap_or(u64::MAX).to_le_bytes());
        hash.update(app.as_bytes());
        hash.update(document.as_bytes());
        let mut id = [0; 16];
        hash.finalize_xof().fill(&mut id);
        // Reserve a non-RFC variant so this cannot alias a workspace or personal store.
        id[8] |= 0xe0;
        Self::from_u128(u128::from_be_bytes(id))
    }

    /// A workspace id built from a number, for tests that count workspaces.
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(uuid::Uuid::from_u128(value))
    }

    /// Parse a workspace id from its canonical text form.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        uuid::Uuid::parse_str(text).ok().map(Self)
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for WorkspaceId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(text).map(Self)
    }
}

impl From<uuid::Uuid> for WorkspaceId {
    fn from(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }
}

impl From<WorkspaceId> for uuid::Uuid {
    fn from(id: WorkspaceId) -> Self {
        id.0
    }
}

/// The id of the workspace of the person whose root is `person`: the same on each of their
/// devices, so none has to be told it.
///
/// Their own workspace holds what is theirs alone: their servers, their devices, the settings
/// that follow them and the records an app keeps of them among it. The domain names the
/// workspace's first use, the person's servers; the ids of workspaces founded before it follow
/// from it.
#[must_use]
pub fn person_workspace(person: &[u8; 32]) -> uuid::Uuid {
    let hash = blake3::Hasher::new()
        .update(b"tilo admin workspace v1\0")
        .update(person)
        .finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

/// Whether `workspace` is the own workspace of the person whose root is `person`.
#[must_use]
pub fn is_person_workspace(person: &[u8; 32], workspace: &uuid::Uuid) -> bool {
    person_workspace(person) == *workspace
}

/// [`person_workspace`] of the person a [`Person`](crate::Person) id names (the lowercase hex
/// of their root public key); `None` for anything else.
#[must_use]
pub fn person_workspace_of(person: &str) -> Option<uuid::Uuid> {
    let mut key = [0u8; 32];
    hex::decode_to_slice(person, &mut key).ok()?;
    Some(person_workspace(&key))
}
