//! The record content a workspace's documents reference, on the workspace
//! service's links: the profile's content store, and the workspaces the
//! service holds.
//!
//! The routing itself is the workspace service's ([`tilo_workspace_service::content`]);
//! what a profile adds is where its content is kept — the profile's own
//! content store, reached through the scope a record's content lives under —
//! and the retry that asks again for content no device handed over.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use neighbour_wire::content::PIECE_PATIENCE_MS;
use tilo_workspace_service::content::{ContentScope, ContentShelf, RouteContent, routes_content};

use super::service::ServiceWorkspaces;

/// The profile's content store, read and written as the desktop session
/// reaches its own: through the scope a record's content lives under.
struct ProfileShelf(std::path::PathBuf);

impl ContentShelf for ProfileShelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        use runtime_fs::blobs::{BlobClient, ReadOutcome};

        let scoped = BlobClient::connect_session(&self.0)
            .and_then(|session| session.scope(&scope.workspace, &scope.app))
            .ok()?;
        if !scoped.has(hash).ok()? {
            return None;
        }
        match scoped.read_all(hash).ok()? {
            ReadOutcome::Ready { bytes, .. } => Some(bytes),
            ReadOutcome::Pending | ReadOutcome::Unavailable => None,
        }
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let kept = runtime_fs::blobs::BlobClient::connect_session(&self.0)
            .and_then(|session| session.scope(&scope.workspace, &scope.app))
            .and_then(|scoped| scoped.put(bytes, "application/octet-stream"))
            .map_err(|error| error.to_string())?;
        if kept.hash == hash {
            Ok(())
        } else {
            Err("the content store keeps these bytes under another address".into())
        }
    }
}

/// The profile's record-content service: its content store, served on
/// `blob_socket`, and the workspaces the workspace service holds.
pub(in crate::standalone) fn document_content(
    workspaces: &Arc<ServiceWorkspaces>,
    blob_socket: &Path,
) -> Arc<RouteContent> {
    let shelf: Arc<dyn ContentShelf> = Arc::new(ProfileShelf(blob_socket.to_path_buf()));
    record_content_on(workspaces, shelf)
}

/// The record-content service of `workspaces`' workspaces over `shelf`.
pub(super) fn record_content_on(
    workspaces: &Arc<ServiceWorkspaces>,
    shelf: Arc<dyn ContentShelf>,
) -> Arc<RouteContent> {
    let served = routes_content(workspaces.copies(), shelf);
    workspaces.install_content(&served.content);
    workspaces.install_keeper(Arc::clone(&served.keeper));
    // A want that got no content is asked again once the patience of its
    // last answer passed: a holder that said it keeps none may keep it a
    // moment later, and a large content crosses in windows of one answer.
    let ticking = Arc::clone(&served.content);
    tilo_rt::app().spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(PIECE_PATIENCE_MS)).await;
            ticking.retry(tilo_clock::unix_now_ms());
        }
    });
    served.content
}
