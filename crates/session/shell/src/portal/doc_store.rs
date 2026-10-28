//! Turning "the user picked this file" into "the app can open it".
//!
//! A capability recorded on a workspace is what the *launcher* reads,
//! decided once when the app starts. A grant made *after* the app
//! started changes nothing about the mount namespace it's already
//! living in — a host `mount --bind` doesn't propagate into an
//! `--unshare-all` sandbox, so handing back `/home/you/notes.md` hands
//! back a path that resolves to `ENOENT` inside the caller.
//!
//! Three candidate fixes: a **launch-time grant** (what `doc_open.rs`
//! already does) can't serve a running app — exactly the case this
//! portal is for. A **live re-bind** would need a `CAP_SYS_ADMIN` helper
//! inside the sandbox to mount from within — the most valuable target in
//! the sandbox, trading confinement for convenience. A **document
//! store** — this module — mounts one compositor-owned filesystem before
//! any app starts and binds a per-app subtree into each sandbox; the
//! mount topology never changes, only the contents, edited from outside
//! and visible inside immediately. This is how `xdg-document-portal` solves the same problem.
//!
//! That store is `tilo_documents`, which the shell's own chooser gives
//! through (`compositor::chooser_service`). The bind is unconditional: every
//! sandbox launched into a room gets `/run/tilo/documents` whether or not a
//! file has been picked yet, empty at launch — no ambient authority. An app can't enumerate another app's documents
//! (the bind is of its own subtree, not the store root), can't guess its
//! way to one, and can't keep one after the grant is removed — strictly
//! narrower than the portal it replaces, which granted
//! `Scope::FsPath("/")` permanently on one *Always* click.
//!
//! The backend side of the standard portal protocol returns host paths to
//! `xdg-desktop-portal`. The trusted frontend publishes them through
//! `xdg-document-portal` before returning a URI to a confined application.
//! [`HostPathStore`] therefore deliberately preserves host paths here; it is
//! also useful directly for built-in apps, development runs, and tests. The
//! `reaches_confined_apps` flag describes *direct* reachability only and must
//! not be confused with the frontend's document export step.

use std::path::PathBuf;
use std::sync::Arc;

use super::PortalCaller;
use crate::shell::state::FsMode;

/// The projected workspace tree — not part of the store, named here because the picker's default scope reaches it.
pub const WORKSPACE_ROOT: &str = "/var/workspace";

/// One published document: the path the app should use, and the host path it resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedDoc {
    /// What goes in the portal's `uris` result.
    pub app_path: PathBuf,
    /// The real file on the host.
    pub host_path: PathBuf,
    /// What the app may do with it.
    pub mode: FsMode,
}

/// How picked paths are made reachable by the caller.
pub trait DocumentStore: Send + Sync {
    /// Publish `paths` for `caller` and return the path each one should be addressed by; one entry per input, in order.
    fn publish(&self, caller: &PortalCaller, paths: &[PathBuf], mode: FsMode) -> Vec<PublishedDoc>;

    /// Whether a caller confined to its own mount namespace can reach what this store publishes.
    fn reaches_confined_apps(&self) -> bool;

    /// Human-readable name, for the startup log.
    fn describe(&self) -> String;
}

/// Cheap clone wrapper, matching the shape of the other [`super::PortalContext`] handles.
#[derive(Clone)]
pub struct DocumentStoreHandle {
    inner: Arc<dyn DocumentStore>,
}

impl DocumentStoreHandle {
    pub fn new(inner: Arc<dyn DocumentStore>) -> Self {
        Self { inner }
    }

    /// A store that hands back real host paths — correct for callers that
    /// share the compositor's mount namespace; see the module docs for what it does *not* claim.
    pub fn host_paths() -> Self {
        Self::new(Arc::new(HostPathStore))
    }

    /// Publish, returning just the paths the app should be handed.
    pub fn publish(&self, caller: &PortalCaller, paths: &[PathBuf], mode: FsMode) -> Vec<PathBuf> {
        self.inner
            .publish(caller, paths, mode)
            .into_iter()
            .map(|d| d.app_path)
            .collect()
    }

    /// Publish, keeping the full record.
    pub fn publish_detailed(
        &self,
        caller: &PortalCaller,
        paths: &[PathBuf],
        mode: FsMode,
    ) -> Vec<PublishedDoc> {
        self.inner.publish(caller, paths, mode)
    }

    pub fn reaches_confined_apps(&self) -> bool {
        self.inner.reaches_confined_apps()
    }

    pub fn describe(&self) -> String {
        self.inner.describe()
    }
}

impl Default for DocumentStoreHandle {
    fn default() -> Self {
        Self::host_paths()
    }
}

/// The identity store: a picked path is published as itself.
///
/// This is also the required backend-side representation for the standard
/// portal frontend: `xdg-desktop-portal` consumes the host path and exports it
/// through `xdg-document-portal` for a confined caller. It reports direct
/// sandbox reachability as `false`, because that export is a separate step.
pub struct HostPathStore;

impl DocumentStore for HostPathStore {
    fn publish(
        &self,
        _caller: &PortalCaller,
        paths: &[PathBuf],
        mode: FsMode,
    ) -> Vec<PublishedDoc> {
        paths
            .iter()
            .map(|p| PublishedDoc {
                app_path: p.clone(),
                host_path: p.clone(),
                mode: mode.clone(),
            })
            .collect()
    }

    fn reaches_confined_apps(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        "host paths (unconfined callers only)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceId;

    fn caller() -> PortalCaller {
        PortalCaller::new(WorkspaceId(7), "so.tilo.notes")
    }

    #[test]
    fn host_path_doc_store_publishes_inputs_unchanged_in_order() {
        {
            let store = DocumentStoreHandle::host_paths();
            let out = store.publish(
                &caller(),
                &[PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")],
                FsMode::Read,
            );
            assert_eq!(
                out,
                vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]
            );
        }
        // Phase `host_paths_does_not_claim_to_serve_confined_callers`.
        // The whole point of the flag: a passthrough store is honest that a
        // bwrap'd app can't reach what it returns; reporting true here would make the compositor's startup log lie about sandboxed file-open.
        assert!(!DocumentStoreHandle::host_paths().reaches_confined_apps());
        {
            let store = DocumentStoreHandle::host_paths();
            let inputs = [
                PathBuf::from("/tmp/1"),
                PathBuf::from("/tmp/2"),
                PathBuf::from("/tmp/3"),
            ];
            let out = store.publish_detailed(&caller(), &inputs, FsMode::ReadWrite);
            assert_eq!(out.len(), inputs.len());
            for (got, want) in out.iter().zip(inputs.iter()) {
                assert_eq!(&got.host_path, want);
            }
        }
    }
}
