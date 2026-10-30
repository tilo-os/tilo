//! The desk as it was left: which workspaces there are, what their strips
//! are called and which workspace and strip are in front. Kept in this
//! device's state directory, so the next session starts there instead of
//! on one empty desk.
//!
//! Only the shape is kept, not the windows: a window comes back when its
//! application runs again, and a tab restored without one would be a
//! window that never arrives.

use std::path::Path;

use tilo_desktop_model::{DesktopTree, STATE_SCHEMA_VERSION};

/// The file beside the rest of this device's state.
const FILE: &str = "desk.json";

/// Where the desk lives under `state`, this device's state directory.
fn path(state: &Path) -> std::path::PathBuf {
    state.join("tilo").join(FILE)
}

/// The desk as it was saved under `state`, if there is one this build
/// understands.
pub(crate) fn load_at(state: &Path) -> Option<DesktopTree> {
    let text = std::fs::read_to_string(path(state)).ok()?;
    let tree: DesktopTree = serde_json::from_str(&text)
        .map_err(|error| tracing::debug!(%error, "the saved desk is not this build's"))
        .ok()?;
    // A file a newer build wrote describes a desk this one cannot place.
    if tree.schema_version != STATE_SCHEMA_VERSION {
        tracing::debug!(
            saved = tree.schema_version,
            this = STATE_SCHEMA_VERSION,
            "the saved desk is another build's"
        );
        return None;
    }
    if tree.workspaces.is_empty() {
        return None;
    }
    Some(shape(tree))
}

/// The desk `tree` describes, as it is kept: what a save writes and a
/// reader compares.
#[must_use]
pub(crate) fn text(tree: &DesktopTree) -> Option<String> {
    serde_json::to_string(&shape(tree.clone())).ok()
}

/// Write `text`, the desk as [`text`] made it, under `state`, replacing the
/// one before.
///
/// # Errors
/// The state directory could not be written.
pub(crate) fn save_at(state: &Path, text: &str) -> std::io::Result<()> {
    let path = path(state);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written beside and moved into place: a session that dies mid-write
    // leaves the desk it had.
    let beside = path.with_extension("json.tmp");
    std::fs::write(&beside, text)?;
    std::fs::rename(&beside, &path)
}

/// `tree` without its windows: what is saved and what a restore brings
/// back.
fn shape(mut tree: DesktopTree) -> DesktopTree {
    tree.schema_version = STATE_SCHEMA_VERSION;
    for workspace in &mut tree.workspaces {
        for strip in &mut workspace.strips {
            strip.window_groups.clear();
        }
    }
    tree.active_group.clear();
    tree.outputs = tilo_desktop_model::OutputPartition::default();
    tree
}
