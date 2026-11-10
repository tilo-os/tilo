//! The host's content: one store per workspace it serves
//! ([`tilo_content::WorkspaceStores`]), under its state root.
//!
//! Everything this host keeps for a workspace goes there and is read from
//! there: a file a caller uploaded and what an app made in a record the host
//! runs, the files the workspace's members wrote ([`crate::files_content`]),
//! what its bots fetched from outside and the files its members posted in
//! Chat ([`crate::room_bots::attachments`]). Members read any of it by asking
//! their workspace for the hash their record names
//! ([`crate::workspaces::Workspaces::content_answer`]), and are answered from
//! that workspace's store alone: possessing a hash grants nothing in another
//! workspace.

use std::path::Path;
use std::sync::Arc;

use tilo_content::{ContentLimits, WorkspaceStores};

use crate::workspaces::WorkspaceContent;

/// The directory, under a host's state root, its workspaces' content is
/// kept in, one directory per workspace.
pub const DIRECTORY: &str = "content";

/// What one workspace may keep on its host: no bound of its own. A host
/// serves hundreds of workspaces, each as large as its people make it; what
/// bounds them is the disk, which refuses new content with its reason while
/// it runs low ([`tilo_content::ContentError::DiskLow`]) and drops nothing.
pub const WORKSPACE_LIMITS: ContentLimits = ContentLimits {
    max_item_bytes: usize::MAX,
    max_total_bytes: usize::MAX,
    max_entries: usize::MAX,
    max_scan_entries: usize::MAX,
};

/// The content stores of the workspaces a host keeps state under
/// `state_root`.
#[must_use]
pub fn stores(state_root: &Path) -> Arc<WorkspaceStores> {
    Arc::new(WorkspaceStores::in_dir(
        state_root.join(DIRECTORY),
        WORKSPACE_LIMITS,
    ))
}

/// The workspaces' stores, as the host answers its members from them.
pub struct StoresContent(pub Arc<WorkspaceStores>);

impl WorkspaceContent for StoresContent {
    fn read(&self, workspace: &str, _app: &str, hash: &str) -> Option<Vec<u8>> {
        self.0.get(workspace).ok()?.read(hash)
    }
}
