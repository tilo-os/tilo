//! Read-only startup preflight: never repairs a tree, mints identity or contacts the network.
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceStatus {
    schema_version: u8,
    /// This device's workspace service serves the trees of the workspaces it belongs to.
    workspace_service: bool,
    can_resume_existing: bool,
}

impl WorkspaceStatus {
    pub(super) fn require_existing(&self) -> Result<()> {
        anyhow::ensure!(
            self.can_resume_existing,
            "no workspace to sync this computer's files from; sign this computer in to a workspace first"
        );
        Ok(())
    }
}

/// What `cache` and the workspace service at `tree_socket` offer a mount that must not start
/// empty.
pub(super) fn inspect(cache: &Path, tree_socket: &Path) -> Result<WorkspaceStatus> {
    let _ = cache;
    let workspace_service = std::fs::symlink_metadata(tree_socket).is_ok_and(|metadata| {
        use std::os::unix::fs::FileTypeExt as _;
        metadata.file_type().is_socket()
    });
    Ok(WorkspaceStatus {
        schema_version: 3,
        workspace_service,
        can_resume_existing: workspace_service,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_workspace_service_resumes_a_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("files-tree.sock");
        let cache = dir.path().join("cache");
        assert!(
            inspect(&cache, &socket)
                .unwrap()
                .require_existing()
                .is_err()
        );

        // Kept documents of an earlier build resume nothing: the store is the tree.
        std::fs::create_dir_all(cache.join("namespace")).unwrap();
        std::fs::write(cache.join("namespace/0.automerge"), b"kept").unwrap();
        assert!(
            inspect(&cache, &socket)
                .unwrap()
                .require_existing()
                .is_err()
        );

        std::fs::write(&socket, b"not a socket").unwrap();
        assert!(
            inspect(&cache, &socket)
                .unwrap()
                .require_existing()
                .is_err()
        );
        std::fs::remove_file(&socket).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        inspect(&cache, &socket)
            .unwrap()
            .require_existing()
            .unwrap();
    }
}
