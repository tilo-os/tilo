//! The id of the artifact a document is, derived from its route.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    fn artifact_ids_are_unique() {
        assert_ne!(ArtifactId::new(), ArtifactId::new());
    }
}
