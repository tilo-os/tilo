//! Read-only startup preflight: never repairs a tree, mints identity or contacts the network.
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceStatus {
    schema_version: u8,
    /// The tree was kept here by an earlier mount.
    saved_namespace: bool,
    /// This device's workspace service serves the trees of the workspaces it belongs to.
    workspace_service: bool,
    can_resume_existing: bool,
}

impl WorkspaceStatus {
    pub(super) fn require_existing(&self) -> Result<()> {
        anyhow::ensure!(
            self.can_resume_existing,
            "no saved Files tree and no workspace to sync one from; sign this computer in to a workspace first"
        );
        Ok(())
    }
}

/// Whether the tree's kept documents are there: the directory exists and holds a document or a
/// journal of one.
fn kept_documents(dir: &Path) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("inspect the kept workspace tree"),
    };
    anyhow::ensure!(
        metadata.is_dir(),
        "the kept workspace tree must be a directory"
    );
    let mut entries = std::fs::read_dir(dir).context("inspect the kept workspace tree")?;
    match entries.next() {
        Some(entry) => {
            entry.context("inspect the kept workspace tree")?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// What `cache` and the workspace service at `tree_socket` offer a mount that must not start
/// empty.
pub(super) fn inspect(cache: &Path, tree_socket: &Path) -> Result<WorkspaceStatus> {
    let saved_namespace = kept_documents(&cache.join("namespace"))?;
    let workspace_service = std::fs::symlink_metadata(tree_socket).is_ok_and(|metadata| {
        use std::os::unix::fs::FileTypeExt as _;
        metadata.file_type().is_socket()
    });
    Ok(WorkspaceStatus {
        schema_version: 2,
        saved_namespace,
        workspace_service,
        can_resume_existing: saved_namespace || workspace_service,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_tree_or_the_workspace_service_resumes_a_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("files-tree.sock");
        let missing = dir.path().join("absent-cache");
        assert!(
            inspect(&missing, &socket)
                .unwrap()
                .require_existing()
                .is_err()
        );
        assert!(!missing.exists());

        std::fs::create_dir_all(dir.path().join("namespace")).unwrap();
        assert!(
            inspect(dir.path(), &socket)
                .unwrap()
                .require_existing()
                .is_err()
        );
        std::fs::write(dir.path().join("namespace/0.automerge"), b"kept").unwrap();
        inspect(dir.path(), &socket)
            .unwrap()
            .require_existing()
            .unwrap();

        let fresh = dir.path().join("fresh");
        std::fs::write(&socket, b"not a socket").unwrap();
        assert!(
            inspect(&fresh, &socket)
                .unwrap()
                .require_existing()
                .is_err()
        );
        std::fs::remove_file(&socket).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        inspect(&fresh, &socket)
            .unwrap()
            .require_existing()
            .unwrap();
        assert!(!fresh.exists());
    }

    #[test]
    fn a_kept_tree_that_is_not_a_directory_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("elsewhere"), b"tree").unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join("namespace"))
            .unwrap();
        assert!(inspect(dir.path(), &dir.path().join("absent.sock")).is_err());
    }
}
