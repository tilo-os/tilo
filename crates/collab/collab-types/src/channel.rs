//! Ids carried by a document's ops: the artifact the document is, and the
//! channel its MLS group and content keys belong to. Both are derived from
//! the document's route.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable id for a channel. UUID v4 — globally unique, no coordination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelId(pub Uuid);

impl ChannelId {
    /// Mint a fresh channel id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ChannelId {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable id for an artifact. UUID v4 — globally unique, no coordination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(pub Uuid);

impl ArtifactId {
    /// Mint a fresh artifact id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ArtifactId {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        assert_ne!(ChannelId::new(), ChannelId::new());
        assert_ne!(ArtifactId::new(), ArtifactId::new());
    }
}
