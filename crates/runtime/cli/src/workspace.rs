//! Workspace-path resolution for CLI apps.
//!
//! Tilo apps read and write user data through the workspace FUSE at
//! `/var/workspace/` — from the app's view, a regular filesystem, no capnp
//! gymnastics for the common "write this file" case. This module gives CLI
//! apps a one-line way to compute the right path, plus a graceful
//! standalone fallback.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{ensure_dir, package_id, standalone_state_dir};

/// Env var the launcher sets pointing at the workspace root; absent outside Tilo.
pub const TILO_WORKSPACE_ROOT_ENV: &str = "TILO_WORKSPACE_ROOT";

/// Default workspace root when inside Tilo ([`crate::is_tilo_native`]) but
/// the env var is unset.
pub const DEFAULT_WORKSPACE_ROOT: &str = "/var/workspace";

/// Is the Tilo workspace FUSE mounted and reachable? Checks
/// `TILO_WORKSPACE_ID` and the workspace-root env var.
pub fn is_workspace_available() -> bool {
    crate::is_tilo_native() || std::env::var_os(TILO_WORKSPACE_ROOT_ENV).is_some()
}

/// The workspace root the launcher points at, if any: falls back to
/// [`DEFAULT_WORKSPACE_ROOT`] inside Tilo, `None` standalone.
pub fn workspace_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(TILO_WORKSPACE_ROOT_ENV) {
        return Some(PathBuf::from(root));
    }
    if crate::is_tilo_native() {
        return Some(PathBuf::from(DEFAULT_WORKSPACE_ROOT));
    }
    None
}

/// Resolve a workspace-relative path to something an app can `open()`. In
/// Tilo, joins onto the workspace root (the FUSE hydrates on first read);
/// standalone, falls back to `~/.local/share/<pkg-id>/` and creates the
/// directory tree so callers can write immediately.
///
/// Rejects absolute paths and `..` traversal, so a path safe in workspace
/// is also safe standalone.
///
/// # Errors
///
/// Returns an error if `rel` is empty, absolute, contains a `..` component,
/// or — standalone only — if the parent directory can't be created.
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
    // Standalone fallback: materialise the parent so callers can write without
    // pre-mkdir'ing.
    let root = standalone_state_dir(&package_id());
    let target = root.join(p);
    if let Some(parent) = target.parent() {
        ensure_dir(parent)?;
    }
    Ok(target)
}

/// Handle passed into [`act`] closures: the resolved workspace root plus
/// the FS client (`None` standalone) for direct RPC vs. plain `std::fs`.
pub struct WorkspaceHandle {
    pub root: PathBuf,
    #[cfg(unix)]
    fs: Option<runtime_fs::FsClient>,
}

impl WorkspaceHandle {
    /// Whether this process received the launcher's optional filesystem
    /// capability. This stays available on every host even though the
    /// capability transport itself is Unix-only.
    pub fn has_filesystem_service(&self) -> bool {
        #[cfg(unix)]
        {
            self.fs.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

impl std::fmt::Debug for WorkspaceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("WorkspaceHandle");
        debug.field("root", &self.root);
        debug.field("fs", &self.has_filesystem_service());
        debug.finish()
    }
}

/// Run a closure with a resolved [`WorkspaceHandle`]: a live capnp FS
/// client in Tilo, `None` standalone. Runs on the current tokio `LocalSet`
/// (capnp-rpc futures are `!Send`); skip this and call
/// [`resolve_workspace_path`] directly if you don't need the FS client.
///
/// # Errors
///
/// Returns an error if the workspace (or standalone fallback) root can't be
/// created, or if `f` itself returns an error.
pub async fn act<T, F, Fut>(f: F) -> Result<T>
where
    F: FnOnce(WorkspaceHandle) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let root = workspace_root().unwrap_or_else(|| standalone_state_dir(&package_id()));
    ensure_dir(&root)?;
    #[cfg(unix)]
    let handle = WorkspaceHandle {
        root,
        fs: runtime_fs::try_connect().ok(),
    };
    #[cfg(not(unix))]
    let handle = WorkspaceHandle { root };
    f(handle).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scope with every `TILO_*` variable removed, restored on drop.
    fn without_tilo_env() -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("TILO_") {
                env.remove(&key);
            }
        }
        env
    }

    #[test]
    fn resolve_rejects_absolute_paths_and_parent_traversal() {
        for (path, want) in [
            ("/etc/passwd", "relative"),
            ("notes/../../etc/passwd", ".."),
        ] {
            let _env = without_tilo_env();
            let e = resolve_workspace_path(path).unwrap_err();
            assert!(format!("{e}").contains(want), "case {path}: {e}");
        }
    }

    #[test]
    fn the_workspace_root_env_drives_resolution_and_availability() {
        // With the root set, paths resolve under it.
        {
            let mut env = without_tilo_env();
            env.set(TILO_WORKSPACE_ROOT_ENV, "/tmp/tilo-ws");
            let p = resolve_workspace_path("notes/inbox.md").unwrap();
            assert_eq!(p, PathBuf::from("/tmp/tilo-ws/notes/inbox.md"));
        }
        // Without it, paths fall back to the standalone state dir.
        {
            let mut env = without_tilo_env();
            let tmp = tempfile::tempdir().unwrap();
            env.set("XDG_DATA_HOME", tmp.path())
                .set("TILO_PACKAGE_ID", "dev.example.notes");
            let p = resolve_workspace_path("inbox.md").unwrap();
            assert!(p.starts_with(tmp.path().join("dev.example.notes")));
            assert!(
                p.parent().unwrap().exists(),
                "parent dir should be pre-created"
            );
        }
        // Availability reflects the env.
        {
            let mut env = without_tilo_env();
            assert!(!is_workspace_available());
            env.set(TILO_WORKSPACE_ROOT_ENV, "/tmp/xxx");
            assert!(is_workspace_available());
        }
    }
}
