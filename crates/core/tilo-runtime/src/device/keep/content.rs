//! The host's content: one store per workspace it serves
//! ([`tilo_content::WorkspaceStores`]), under its state root.
//!
//! Everything this host keeps for a workspace goes there and is read from
//! there: a file a caller uploaded and what an app made in a record kept for
//! it, what the workspace's members pushed ([`crate::device::keep::kept_content`]),
//! and the files its members posted in
//! Chat. Members read any of it by asking
//! their workspace for the hash their record names
//! ([`tilo_workspace_service::workspaces::Workspaces::content_answer`]), and are answered from
//! that workspace's store alone: possessing a hash grants nothing in another
//! workspace. What no member holds any more is let go of
//! ([`crate::device::keep::kept_content`]).

pub mod branch;
mod named;

use std::path::Path;
use std::sync::Arc;

use tilo_content::{ContentLimits, WorkspaceStores};

use tilo_workspace_service::workspaces::WorkspaceContent;

use crate::device::keep::kept_content::KeptContent;

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

/// The one content store of a device that hosts workspaces: the app action server's when it
/// brings one (its uploads and app workers keep there), else the stores under `state_root`, so
/// everything the device keeps for a workspace is in one place.
#[must_use]
pub fn of_device(
    state_root: &Path,
    apps: Option<&Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
) -> Arc<WorkspaceStores> {
    apps.and_then(|server| server.content())
        .unwrap_or_else(|| stores(state_root))
}

/// What every device that hosts workspaces keeps for their members, a server and a person's own
/// computer alike: members read what it keeps for their workspace from `content`, the content a
/// member pushes is kept there ([`KeptContent`]) so a member that comes up after the writer left
/// reads the same bytes from this device alone, a branch takes the content its records name, and
/// a workspace its owner deleted takes its content and its app workers (`apps`) with it. What
/// runs it is the future answered, which lasts as long as the device hosts; the keeper is
/// answered beside it, for what reads what it keeps.
pub fn keep_for_members(
    hosted: &tilo_workspace_service::workspaces::Workspaces,
    content: &Arc<WorkspaceStores>,
    apps: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
) -> (
    Arc<KeptContent>,
    impl std::future::Future<Output = ()> + Send + 'static,
) {
    hosted.serve_content(Arc::new(StoresContent(Arc::clone(content))));
    hosted.branch_content(Arc::new(branch::StoresBranches(Arc::clone(content))));
    let kept = KeptContent::new(hosted.clone(), Arc::clone(content));
    let forgetting = forget_deleted(hosted.clone(), Arc::clone(content), apps);
    let running = {
        let kept = Arc::clone(&kept);
        async move {
            futures::join!(kept.keep_content(), forgetting);
        }
    };
    (kept, running)
}

/// The workspaces' stores, as the host answers its members from them.
pub struct StoresContent(pub Arc<WorkspaceStores>);

impl WorkspaceContent for StoresContent {
    fn read(&self, workspace: &str, _app: &str, hash: &str) -> Option<Vec<u8>> {
        self.0.get(workspace).ok()?.read(hash)
    }
}

/// Let go of each workspace `workspaces` deletes, as it goes: its content in
/// `stores`, and what the app action server `apps` holds of it.
pub async fn forget_deleted(
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    stores: Arc<WorkspaceStores>,
    apps: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
) {
    let mut deletions = workspaces.deletions();
    drop(workspaces);
    loop {
        let (workspace, workspace_id) = match deletions.recv().await {
            Ok(deleted) => deleted,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                tracing::error!(missed, "deleted workspaces' content was not let go of");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        if let Err(error) = stores.remove(&workspace) {
            tracing::warn!(%workspace, %error, "a deleted workspace's content stays");
        }
        if let Some(apps) = &apps {
            apps.forget_workspace(workspace_id);
        }
    }
}
