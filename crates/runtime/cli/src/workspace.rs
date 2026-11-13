//! Workspace paths for CLI apps: the workspace FUSE at `/var/workspace/` is a
//! regular filesystem to the app, and outside Tilo a state directory stands
//! in for it.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{ensure_dir, package_id, standalone_state_dir};

pub const TILO_WORKSPACE_ROOT_ENV: &str = "TILO_WORKSPACE_ROOT";

/// Default workspace root when inside Tilo ([`crate::is_tilo_native`]) but
/// the env var is unset.
pub const DEFAULT_WORKSPACE_ROOT: &str = "/var/workspace";

pub fn is_workspace_available() -> bool {
    crate::is_tilo_native() || std::env::var_os(TILO_WORKSPACE_ROOT_ENV).is_some()
}

pub fn workspace_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(TILO_WORKSPACE_ROOT_ENV) {
        return Some(PathBuf::from(root));
    }
    if crate::is_tilo_native() {
        return Some(PathBuf::from(DEFAULT_WORKSPACE_ROOT));
    }
    None
}

/// The path an app can `open()` for a workspace-relative path: under the
/// workspace root inside Tilo (the FUSE hydrates on first read), or under
/// `~/.local/share/<package id>/` standalone, its parent created so callers
/// can write at once.
///
/// Absolute paths and `..` are rejected, so a path safe in Tilo is safe
/// standalone too.
///
/// # Errors
///
/// `rel` is empty, absolute or holds `..`, or the standalone parent cannot be
/// created.
pub fn resolve_workspace_path(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        anyhow::bail!("workspace path is empty");
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        anyhow::bail!("workspace path must be relative, got {rel:?}");
    }
    for c in p.components() {
        if c == std::path::Component::ParentDir {
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

/// The resolved workspace root and the FS client, `None` standalone.
pub struct WorkspaceHandle {
    pub root: PathBuf,
    #[cfg(unix)]
    fs: Option<runtime_fs::FsClient>,
}

impl WorkspaceHandle {
    /// Whether this process got the launcher's filesystem capability. It
    /// answers on every host, though the transport is Unix-only.
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

/// Run a closure with a [`WorkspaceHandle`]: a live filesystem client in
/// Tilo, `None` standalone. Runs on the current tokio `LocalSet`, the client
/// session being `!Send`.
///
/// # Errors
///
/// The workspace (or standalone) root cannot be created, or `f` returns an
/// error.rror.
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
        {
            let mut env = without_tilo_env();
            env.set(TILO_WORKSPACE_ROOT_ENV, "/tmp/tilo-ws");
            let p = resolve_workspace_path("notes/inbox.md").unwrap();
            assert_eq!(p, PathBuf::from("/tmp/tilo-ws/notes/inbox.md"));
        }
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
        {
            let mut env = without_tilo_env();
            assert!(!is_workspace_available());
            env.set(TILO_WORKSPACE_ROOT_ENV, "/tmp/xxx");
            assert!(is_workspace_available());
        }
    }
}
