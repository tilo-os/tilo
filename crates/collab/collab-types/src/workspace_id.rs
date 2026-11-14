//! A workspace's id: minted once as a random UUID and carried, never derived.

use serde::{Deserialize, Serialize};

/// Identifies a workspace everywhere: in wire messages, relay routing, stored
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

    #[must_use]
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    #[must_use]
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
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
