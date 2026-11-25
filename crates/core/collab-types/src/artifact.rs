//! The id of the artifact a document is, derived from its route.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable id for an artifact. UUID v4 — globally unique, no coordination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(pub Uuid);

/// The root document kind of a workspace's Files tree.
pub const FILES_TREE_KIND: &str = "files.tree";

/// The verbs of a workspace's Files tree a `files#open` given content changes it by: a folder
/// made on the way, a file made, a file of the same name given the new content.
pub const FILES_OPEN_VERBS: [&str; 3] = ["new-folder", "new-file", "write"];

/// Whether the call `target` asks, as its own, for the step `verb` on a record of `kind` in the
/// records of `app`: Files' `open` given content puts files into the workspace's folder, which is
/// the tree's new-folder, new-file and write. The call is carried out as those verbs, so a change
/// that names them is inside the call, and the caller's own grants on them decide it.
#[must_use]
pub fn files_open_step(
    target: &tilo_actions::ActionAddress,
    (app, kind, verb): (&str, &str, &str),
) -> bool {
    app == tilo_app_id::apps::FILE_EXPLORER
        && kind == FILES_TREE_KIND
        && FILES_OPEN_VERBS.contains(&verb)
        && target.action == tilo_actions::remote::APPLICATION_OPEN_ACTION
        && target
            .resource
            .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
            == Some(tilo_app_id::apps::FILE_EXPLORER)
}

/// Whether `argument`, an `open`'s, names content to put: one file's `content`, or many as
/// `files`. An open naming neither starts the app and changes no file.
#[must_use]
pub fn files_open_puts(argument: &tilo_actions::remote::InvokeArgument) -> bool {
    matches!(
        argument,
        tilo_actions::remote::InvokeArgument::Json(arguments)
            if arguments.contains_key("content") || arguments.contains_key("files")
    )
}

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
