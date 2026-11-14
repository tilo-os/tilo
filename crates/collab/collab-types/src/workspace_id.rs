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

    /// The number the 0.1 releases named this workspace by on disk and on
    /// the wire: the first eight bytes, little-endian, of a BLAKE3 key
    /// derived from the id's lowercase text, never zero. Read only to find
    /// what those releases kept; nothing names a workspace by it now.
    #[must_use]
    pub fn released_number(&self) -> u64 {
        let mut text = [0_u8; uuid::fmt::Hyphenated::LENGTH];
        let hash = blake3::derive_key(
            "tilo-room-transport-v1",
            self.0.hyphenated().encode_lower(&mut text).as_bytes(),
        );
        let first = hash.first_chunk::<8>().copied().unwrap_or_default();
        match u64::from_le_bytes(first) {
            0 => 1,
            number => number,
        }
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

#[cfg(test)]
mod tests {
    use super::WorkspaceId;

    /// The numbers the release the live relay runs (v0.1.5) named two
    /// workspaces by, as its own `seed.json` and directories name them.
    #[test]
    fn a_workspace_is_the_number_the_releases_named_it_by() {
        for (id, number) in [
            (
                "abee87a1-91ca-46b7-9233-8ab47f9ec3e7",
                17_893_561_556_324_935_546,
            ),
            (
                "f8daa172-13b8-4dfb-9ba3-e60fa7b06333",
                16_906_116_704_135_141_798,
            ),
        ] {
            assert_eq!(WorkspaceId::parse(id).unwrap().released_number(), number);
        }
    }
}
