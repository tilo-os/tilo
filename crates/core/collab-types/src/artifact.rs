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

/// The verbs of a workspace's Files tree Files' `action` on the workspace's own folder is carried
/// out as: its `open` given content and its `write-file` put files ([`FILES_OPEN_VERBS`]); a
/// folder is made, a rename or move makes the folders on the way and moves, a copy makes folders
/// and files, and the trash and its restore are their own verbs. Any other action changes no file.
#[must_use]
pub fn files_call_verbs(action: &str) -> &'static [&'static str] {
    match action {
        tilo_actions::remote::APPLICATION_OPEN_ACTION | "write-file" => &FILES_OPEN_VERBS,
        "new-folder" => &["new-folder"],
        "rename" | "move" => &["new-folder", "move"],
        "copy-into" => &["new-folder", "new-file"],
        "trash" => &["trash"],
        "restore" => &["restore"],
        _ => &[],
    }
}

/// Whether `target` names Files' action on a workspace's own folder in the records of `app`: the
/// app's own call on it (`app.<app>`), or the device's answerer of the same action where the
/// tree is kept (`tilo.device-files` on `files.folder`). A caller that reaches the folder asks
/// one or the other, never a second action beside them.
fn names_files_folder(target: &tilo_actions::remote::RemoteActionTarget, app: &str) -> bool {
    app == tilo_app_id::apps::FILE_EXPLORER
        && (target
            .address
            .resource
            .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
            == Some(tilo_app_id::apps::FILE_EXPLORER)
            || (target.provider == tilo_actions::remote::DEVICE_FILES_PROVIDER
                && target.address.resource == tilo_actions::remote::DEVICE_FILES_FOLDER))
}

/// Whether the call `target` asks, as its own, for the step `verb` on a record of `kind` in the
/// records of `app`: Files' action on the workspace's own folder, carried out as the tree's verbs
/// [`files_call_verbs`] names, so a change that names them is inside the call, and the caller's
/// own grants on them decide it.
#[must_use]
pub fn files_open_step(
    target: &tilo_actions::remote::RemoteActionTarget,
    (app, kind, verb): (&str, &str, &str),
) -> bool {
    kind == FILES_TREE_KIND
        && names_files_folder(target, app)
        && files_call_verbs(&target.address.action).contains(&verb)
}

/// Whether the call `target` asks, as its own, for a step through the history of the records of
/// `app`: Files' `undo` on the workspace's own folder takes a call back as History takes back a
/// step, and the caller's own grants on the tree decide it.
#[must_use]
pub fn files_history_step(target: &tilo_actions::remote::RemoteActionTarget, app: &str) -> bool {
    target.address.action == "undo" && names_files_folder(target, app)
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
