//! The id of the artifact a document is, derived from its route.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable id for an artifact. UUID v4 — globally unique, no coordination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(pub Uuid);

/// The root document kind of a workspace's Files tree.
pub const FILES_TREE_KIND: &str = "files.tree";

/// The authenticated data address for the bytes of the workspace's Files tree.
#[must_use]
pub fn file_content_artifact(workspace: crate::WorkspaceId) -> ArtifactId {
    crate::document_artifact_id(
        tilo_app_id::apps::FILE_EXPLORER,
        &workspace.to_string(),
        "file-content",
    )
}

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
