//! Turning "the user picked this file" into "the app can open it".
//!
//! ## Why a grant is not enough
//!
//! A capability recorded on a workspace is what the *launcher* reads.
//! It becomes `--ro-bind-try <p> <p>` (or `--bind-try`) plus a matching
//! Landlock rule, and both are decided once, when the app starts —
//! `crates/packaging/launcher/src/capabilities.rs` is explicit that the
//! two enforcement layers are derived from one plan so they cannot
//! disagree. That is the right shape, and it has one consequence: a
//! grant made *after* the app started changes nothing about the
//! namespace it is already living in.
//!
//! An app under `--unshare-all` has its own mount namespace. Nothing
//! outside can add a mount to it. `mount --bind` on the host does not
//! propagate; the app's `/` is not the host's `/`. So a portal that
//! hands back `/home/you/notes.md` hands back a path that resolves to
//! `ENOENT` inside the caller, and the app reports that the file it
//! just watched the user choose does not exist.
//!
//! ## The three candidate mechanisms
//!
//! **A launch-time grant** is what already exists, and it is what makes
//! `doc_open.rs` work: grant, *then* start the app. It cannot serve a
//! running app, which is exactly the case the portal is for.
//!
//! **A live re-bind** would mean entering the app's mount and user
//! namespaces and mounting from the inside. Something has to hold
//! `CAP_SYS_ADMIN` in that user namespace to do it, which means a
//! helper process living inside the sandbox whose whole job is to mount
//! host paths on request. That helper is then the most valuable target
//! in the sandbox: compromise its protocol and you have arbitrary host
//! mounts, from inside the thing the sandbox exists to contain. It
//! trades a confinement boundary for a convenience.
//!
//! **A document store** — this module — mounts one filesystem the
//! compositor owns, before any app starts, and binds a per-app subtree
//! of it into each sandbox. The mount topology is then fixed for the
//! app's whole life and never changes again. What changes is the
//! *contents* of that filesystem, which the compositor edits from
//! outside, and which appear inside immediately because it is the same
//! filesystem rather than a new mount. This is how `xdg-document-portal`
//! solves the same problem, and it is the mechanism used here.
//!
//! ## What it means for confinement
//!
//! The bind is unconditional: every sandbox gets `/run/qos/documents`,
//! whether or not the user has ever picked a file for it. That is a
//! widening of the launch profile, and it is worth being precise about
//! how much.
//!
//! At launch the app's subtree is **empty**. There is no ambient
//! authority in it: the app cannot enumerate documents belonging to
//! another app, because the bind is of its own subtree and not of the
//! store root. It cannot guess its way to one, because a handle it was
//! not given is not in its subtree at all — the confinement is the
//! mount topology, not a secret. And it cannot keep one, because the
//! entry is removed when the grant is.
//!
//! So what the app gains is: exactly the files a human chose for it in
//! a dialog the app does not draw, does not aim, and cannot read. That
//! is strictly narrower than the alternative the portal replaces, which
//! granted `Scope::FsPath("/")` on an *Always* click — the entire
//! filesystem, permanently, from one dialog.
//!
//! ## When the store cannot be mounted
//!
//! FUSE may be unavailable — no `/dev/fuse`, no permission, no
//! `fusermount3`. The honest failure is to say so: [`HostPathStore`]
//! serves callers that share the compositor's own namespace (built-in
//! apps, development runs, tests) by handing back the real path, which
//! for those callers is genuinely reachable. It does **not** pretend to
//! serve a confined caller. `documents_reach_confined_apps()` is what
//! the compositor logs at startup, so "file open silently doesn't work
//! for sandboxed apps" is a line in the journal rather than a mystery.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::PortalCaller;
use crate::shell::state::FsMode;

/// Where the store appears inside a sandbox.
pub const SANDBOX_DOC_DIR: &str = "/run/qos/documents";

/// Env var naming [`SANDBOX_DOC_DIR`] inside the sandbox.
pub const DOC_DIR_ENV: &str = "QOS_DOC_DIR";

/// The projected workspace tree. Not part of the store — named here
/// because the picker's default scope reaches it.
pub const WORKSPACE_ROOT: &str = "/var/workspace";

/// One published document: the path the app should use, and the host
/// path it resolves to.
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
    /// Publish `paths` for `caller` and return the path each one should
    /// be addressed by. Must return one entry per input, in order.
    fn publish(&self, caller: &PortalCaller, paths: &[PathBuf], mode: FsMode) -> Vec<PublishedDoc>;

    /// Whether a caller confined to its own mount namespace can reach
    /// what this store publishes.
    fn reaches_confined_apps(&self) -> bool;

    /// Human-readable name, for the startup log.
    fn describe(&self) -> String;
}

/// Cheap clone wrapper, matching the shape of the other
/// [`super::PortalContext`] handles.
#[derive(Clone)]
pub struct DocumentStoreHandle {
    inner: Arc<dyn DocumentStore>,
}

impl DocumentStoreHandle {
    pub fn new(inner: Arc<dyn DocumentStore>) -> Self {
        Self { inner }
    }

    /// A store that hands back real host paths. Correct for callers
    /// that share the compositor's mount namespace; see the module
    /// docs for what it does *not* claim.
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
/// This is the truthful answer for every caller that shares the
/// compositor's mount namespace — a built-in app, a development run, a
/// test. It is the *wrong* answer for a bwrap'd app, which is why it
/// reports [`DocumentStore::reaches_confined_apps`] as `false` rather
/// than quietly succeeding.
pub struct HostPathStore;

impl DocumentStore for HostPathStore {
    fn publish(&self, _caller: &PortalCaller, paths: &[PathBuf], mode: FsMode) -> Vec<PublishedDoc> {
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

/// The per-app subdirectory name inside the store root.
///
/// Keyed by workspace and app, matching how the launcher already
/// partitions an app's writable directory by room — two rooms are two
/// directories, and a document published into one is not nameable from
/// the other.
pub fn caller_slug(caller: &PortalCaller) -> String {
    format!(
        "{}/{}",
        caller.workspace_id.0,
        sanitise_component(&caller.app_id)
    )
}

/// An app id is untrusted input, so it may not become more than one
/// path component.
fn sanitise_component(raw: &str) -> String {
    let mapped: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.is_empty() || mapped.chars().all(|c| c == '.') {
        return "unknown".to_string();
    }
    mapped
}

/// The file name a published document keeps.
///
/// The app sees the name the user picked, so "Open" followed by "Save"
/// round-trips a sensible default filename. A path with no final
/// component (which `read_dir` cannot produce, but a caller could
/// construct) falls back to a fixed name rather than an empty one.
pub fn doc_file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "document".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceId;

    fn caller() -> PortalCaller {
        PortalCaller::new(WorkspaceId(7), "dev.qos.notes")
    }

    #[test]
    fn host_paths_publish_unchanged() {
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

    #[test]
    fn host_paths_does_not_claim_to_serve_confined_callers() {
        // The whole point of the flag: a passthrough store is honest
        // that a bwrap'd app cannot reach what it returns. If this ever
        // reports true, the compositor's startup log starts lying about
        // whether sandboxed file-open works.
        assert!(!DocumentStoreHandle::host_paths().reaches_confined_apps());
    }

    #[test]
    fn publish_returns_one_entry_per_input_in_order() {
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

    #[test]
    fn a_caller_slug_is_two_components_and_cannot_escape() {
        let c = PortalCaller::new(WorkspaceId(3), "../../etc/passwd");
        let slug = caller_slug(&c);
        assert_eq!(
            Path::new(&slug).components().count(),
            2,
            "an app id must not add path components: {slug}"
        );
        assert!(!slug.contains(".."), "slug must not contain a parent hop: {slug}");
    }

    #[test]
    fn an_empty_app_id_still_yields_a_usable_component() {
        let c = PortalCaller::new(WorkspaceId(1), "");
        let slug = caller_slug(&c);
        assert!(slug.ends_with("/unknown"), "{slug}");
    }

    #[test]
    fn a_published_document_keeps_the_name_the_user_picked() {
        assert_eq!(doc_file_name(Path::new("/home/q/Report.odt")), "Report.odt");
        assert_eq!(doc_file_name(Path::new("/")), "document");
    }
}
