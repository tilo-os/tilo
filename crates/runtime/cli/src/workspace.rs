//! Workspace-path resolution for CLI apps.
//!
//! qOS apps read and write user data through the workspace FUSE
//! at `/var/workspace/`. From the app's point of view it's a
//! regular filesystem — no capnp gymnastics required for the
//! common "write this file" case. This module gives CLI apps a
//! one-line way to compute the right path plus a graceful
//! standalone fallback.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{ensure_dir, package_id, standalone_state_dir};

/// Env var the launcher sets pointing at the workspace root.
/// Absent when we're outside qOS.
pub const QOS_WORKSPACE_ROOT_ENV: &str = "QOS_WORKSPACE_ROOT";

/// Default workspace root when the env var is unset but we ARE
/// inside qOS (detected via [`crate::is_qos_native`]).
pub const DEFAULT_WORKSPACE_ROOT: &str = "/var/workspace";

/// Is the qOS workspace FUSE mounted and reachable? Cheap check:
/// we just look at `QOS_WORKSPACE_ID` (the launcher's canonical
/// "you're inside qOS" signal) and the workspace-root env var.
pub fn is_workspace_available() -> bool {
    crate::is_qos_native() || std::env::var_os(QOS_WORKSPACE_ROOT_ENV).is_some()
}

/// Return the workspace root path the launcher points at, if any.
/// Falls back to [`DEFAULT_WORKSPACE_ROOT`] when we're inside qOS
/// but the env var isn't set. Returns `None` when we're
/// standalone.
pub fn workspace_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(QOS_WORKSPACE_ROOT_ENV) {
        return Some(PathBuf::from(root));
    }
    if crate::is_qos_native() {
        return Some(PathBuf::from(DEFAULT_WORKSPACE_ROOT));
    }
    None
}

/// Resolve a workspace-relative path to something an app can
/// `open()`. Two behaviours:
///
/// - **In qOS** (workspace mounted): joins onto the workspace
///   root. The FUSE hydrates on first read.
/// - **Standalone**: falls back to `~/.local/share/<pkg-id>/`
///   under the current package id (from `QOS_PACKAGE_ID`, or
///   `qos-standalone` if unset). Creates the directory tree so
///   callers can write straight away.
///
/// Rejects absolute paths and `..` traversal to keep the two
/// modes symmetric — an app that writes into workspace must
/// also be safe to run standalone.
pub fn resolve_workspace_path(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        anyhow::bail!("workspace path is empty");
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        anyhow::bail!("workspace path must be relative, got {rel:?}");
    }
    for c in p.components() {
        if let std::path::Component::ParentDir = c {
            anyhow::bail!("workspace path {rel:?} must not contain `..`");
        }
    }

    if let Some(root) = workspace_root() {
        return Ok(root.join(p));
    }
    // Standalone fallback. Materialise the parent so callers can
    // write immediately without pre-mkdir'ing.
    let root = standalone_state_dir(&package_id());
    let target = root.join(p);
    if let Some(parent) = target.parent() {
        ensure_dir(parent)?;
    }
    Ok(target)
}

/// Handle passed into [`act`] closures — carries the resolved
/// workspace root plus the FS client (or `None` in standalone
/// mode). Async apps that want direct FS RPC use the client;
/// the more common "just call `std::fs`" path uses the root.
pub struct WorkspaceHandle {
    pub root: PathBuf,
    pub fs: Option<runtime_fs::FsClient>,
}

impl std::fmt::Debug for WorkspaceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceHandle")
            .field("root", &self.root)
            .field("fs", &self.fs.as_ref().map(|_| "<connected>"))
            .finish()
    }
}

/// Run a closure with a resolved [`WorkspaceHandle`]. In qOS the
/// FS client is a live capnp connection; in standalone mode it's
/// `None` and the closure operates through the plain filesystem.
///
/// The closure runs on the current tokio `LocalSet` (capnp-rpc
/// futures are `!Send`). Callers that don't need the FS client
/// can simply call the closure body directly against
/// [`resolve_workspace_path`].
pub async fn act<T, F, Fut>(f: F) -> Result<T>
where
    F: FnOnce(WorkspaceHandle) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let root = workspace_root().unwrap_or_else(|| standalone_state_dir(&package_id()));
    ensure_dir(&root)?;
    let fs = runtime_fs::try_connect().ok();
    let handle = WorkspaceHandle { root, fs };
    f(handle).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_qos_env() {
        for (k, _) in std::env::vars() {
            if k.starts_with("QOS_") {
                unsafe { std::env::remove_var(&k) };
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn resolve_rejects_absolute_paths() {
        clear_qos_env();
        let e = resolve_workspace_path("/etc/passwd").unwrap_err();
        assert!(format!("{e}").contains("relative"));
    }

    #[test]
    #[serial_test::serial]
    fn resolve_rejects_parent_traversal() {
        clear_qos_env();
        let e = resolve_workspace_path("notes/../../etc/passwd").unwrap_err();
        assert!(format!("{e}").contains(".."));
    }

    #[test]
    #[serial_test::serial]
    fn resolve_uses_workspace_root_when_env_set() {
        clear_qos_env();
        unsafe { std::env::set_var(QOS_WORKSPACE_ROOT_ENV, "/tmp/qos-ws") };
        let p = resolve_workspace_path("notes/inbox.md").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/qos-ws/notes/inbox.md"));
        unsafe { std::env::remove_var(QOS_WORKSPACE_ROOT_ENV) };
    }

    #[test]
    #[serial_test::serial]
    fn resolve_falls_back_to_standalone_state_dir() {
        clear_qos_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path()) };
        unsafe { std::env::set_var("QOS_PACKAGE_ID", "dev.example.notes") };
        let p = resolve_workspace_path("inbox.md").unwrap();
        assert!(p.starts_with(tmp.path().join("dev.example.notes")));
        assert!(p.parent().unwrap().exists(), "parent dir should be pre-created");
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("QOS_PACKAGE_ID");
        }
    }

    #[test]
    #[serial_test::serial]
    fn is_workspace_available_reflects_env() {
        clear_qos_env();
        assert!(!is_workspace_available());
        unsafe { std::env::set_var(QOS_WORKSPACE_ROOT_ENV, "/tmp/xxx") };
        assert!(is_workspace_available());
        unsafe { std::env::remove_var(QOS_WORKSPACE_ROOT_ENV) };
    }
}
