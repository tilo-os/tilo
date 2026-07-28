//! Opening a document in the application that owns it.
//!
//! Activating a file — from the file explorer, from a search result, or
//! from anywhere else — is a launch like any other: the app starts as a
//! sandboxed process in the active workspace, with that workspace's
//! capabilities and its collaboration room. The only thing that makes it
//! a *document* open is which app is chosen and what argument it gets.
//!
//! ## Who owns `.odt`
//!
//! Two registries answer half the question each, and this module is the
//! join.
//!
//! [`qdoc`] is the format registry. It knows which extensions are
//! editable documents and which codec parses them, and it deliberately
//! knows nothing about installed packages — it is a library, and the
//! same `.odt` is the same document whether or not anything is installed
//! to edit it.
//!
//! The **app manifests** name the owner. `[providers].ext` in
//! `qos-app.toml` is a package declaring "I open these file types"; it
//! travels with the package, so installing an app is what teaches this
//! desktop to open its types and uninstalling it is what un-teaches
//! that. There is deliberately no table of extensions in the compositor:
//! a hard-coded one would be wrong the moment anybody installs anything.
//!
//! [`resolve`] reports both halves, because they fail differently. An
//! extension nothing claims is "no app for this"; a *document* extension
//! nothing claims is "you have no editor installed", which is a
//! different sentence to show a user and a different thing to fix.
//!
//! ## The path and the permission are two decisions
//!
//! `qos-launcher` binds what the manifest declares plus what the
//! capability file allows. A path handed to an app as an argument is not
//! in its namespace by virtue of being named — the app starts, opens the
//! path, and gets `ENOENT` or `EACCES`.
//!
//! So opening a document has to grant it too. [`open_in_workspace`]
//! records a read capability for exactly that path, addressed to exactly
//! that package, before it launches — the same `GrantStoreHandle` the
//! permission prompt and the file-drop router write through, so it
//! reaches the sandbox by the same route as any other grant and shows up
//! in the permissions UI like any other grant.
//!
//! The grant is read-only and session-scoped, matching what dropping a
//! file onto a workspace already does
//! (`ChromeState::grant_dropped_file_access`): the least privilege that
//! satisfies "let this app open this file now". Opening a document is
//! not a promise to let the app write it — an editor that saves will
//! raise a write prompt, which is the correct place for the user to be
//! asked.

use std::path::{Path, PathBuf};

use crate::shell::chrome_state::Chrome;
use crate::shell::launcher::{self, InstalledApp, LaunchError};

/// What opening `path` would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentTarget {
    /// An installed GUI application declares this extension.
    App {
        app: InstalledApp,
        /// Whether the format registry also recognises the extension as
        /// an editable document. `false` for a type an app claims that
        /// `qdoc` has no codec for (images, video, archives) — still a
        /// perfectly good open, just not a document one.
        editable_document: bool,
    },
    /// The format registry calls this an editable document, but no
    /// installed package declares it. Distinct from [`Self::Unclaimed`]
    /// because it is actionable: the user needs an editor, and the store
    /// can say which one.
    NoAppForDocument { ext: String },
    /// Nothing claims the extension and it is not a known document
    /// format. Callers fall back to the desktop's default handler.
    Unclaimed,
    /// A directory. Not something an app is launched for — the caller
    /// navigates into it.
    Directory,
}

/// Why an open could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// No installed application declares this file's extension.
    NoHandler { path: PathBuf, ext: Option<String> },
    /// A handler was resolved but the launch itself was refused.
    Launch(LaunchError),
    /// There is no active workspace, so there is no sandbox to open
    /// into and no room to open into.
    NoWorkspace,
}

impl OpenError {
    /// One-line summary for a notification body.
    pub fn user_message(&self) -> String {
        match self {
            OpenError::NoHandler { path, ext } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                match ext {
                    Some(ext) => format!("no installed app opens .{ext} files ({name})"),
                    None => format!("{name} has no file type to open it by"),
                }
            }
            OpenError::Launch(e) => e.user_message(),
            OpenError::NoWorkspace => {
                "there is no active workspace to open it in".to_string()
            }
        }
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

/// The lowercase extension of `path`, without the dot. `None` for a path
/// with no extension at all.
pub fn extension_of(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.trim();
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// What would happen if the user activated `path`.
///
/// Pure — reads the installed-package cache and the format registry, and
/// touches neither the workspace nor the process table. Callers that
/// only want to know whether a row is openable (a context menu building
/// its "Open with…" label) use this without side effects.
pub fn resolve(path: &Path) -> DocumentTarget {
    if path.is_dir() {
        return DocumentTarget::Directory;
    }
    let Some(ext) = extension_of(path) else {
        return DocumentTarget::Unclaimed;
    };
    // The format registry is the authority on "is this an editable
    // document"; the manifests are the authority on "who opens it".
    let editable_document = qdoc::is_supported_ext(&ext);
    match launcher::app_for_extension(&ext) {
        Some(app) => DocumentTarget::App {
            app,
            editable_document,
        },
        None if editable_document => DocumentTarget::NoAppForDocument { ext },
        None => DocumentTarget::Unclaimed,
    }
}

/// Open `path` in the application that declares its extension, in the
/// chrome's active workspace.
///
/// Records a session-scoped read capability for the path first, then
/// launches through [`launcher::launch_document_in_workspace`] — the
/// same path the palette's Applications rows take, so the app gets the
/// workspace's grants and its collaboration room like any other launch.
///
/// Returns the resolved app on success. `Err(OpenError::NoHandler)`
/// means nothing is installed that claims the type; callers decide
/// whether to fall back to the desktop's default handler or to say so.
pub fn open_in_workspace(path: &Path, chrome: &Chrome) -> Result<InstalledApp, OpenError> {
    let app = match resolve(path) {
        DocumentTarget::App { app, .. } => app,
        DocumentTarget::NoAppForDocument { ext } => {
            return Err(OpenError::NoHandler {
                path: path.to_path_buf(),
                ext: Some(ext),
            });
        }
        DocumentTarget::Unclaimed | DocumentTarget::Directory => {
            return Err(OpenError::NoHandler {
                path: path.to_path_buf(),
                ext: extension_of(path),
            });
        }
    };

    // Grant before launching: `launch_document_in_workspace` publishes
    // the workspace's capability list as it stands at call time, so a
    // grant recorded after this point would not reach this launch.
    grant_read_for_open(&app.package_id, path, chrome)?;

    // A document opened from somewhere else belongs *with* whatever
    // asked for it — same task, so a tab in the focused group rather
    // than a new column. A launch from the applications list gets the
    // default (its own group); see
    // [`crate::compositor::tab_binding::Placement`].
    if let Some(group) = focused_group(chrome) {
        launcher::set_pending_placement(
            &app.package_id,
            crate::compositor::tab_binding::Placement::TabIn(group),
        );
    }

    let launched = {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.active_workspace().map(|ws| {
            launcher::launch_document_in_workspace(&app.package_id, ws, Some(path))
        })
    };
    match launched {
        Some(Ok(app)) => Ok(app),
        Some(Err(e)) => Err(OpenError::Launch(e)),
        None => Err(OpenError::NoWorkspace),
    }
}

/// The window group the user is looking at, if any. `None` when the
/// active workspace has no strips or the strip has no groups — a state
/// in which "beside the focused group" has no meaning and the adoption
/// bootstraps its own.
fn focused_group(chrome: &Chrome) -> Option<crate::shell::state::GroupId> {
    let st = chrome.borrow();
    let tree = st.tree();
    let ws = tree.active_workspace()?;
    let strip_id = tree
        .active_strip
        .get(&ws.id)
        .copied()
        .or_else(|| ws.strips.first().map(|s| s.id))?;
    tree.active_group_for(strip_id)
        .or_else(|| {
            ws.strips
                .iter()
                .find(|s| s.id == strip_id)
                .and_then(|s| s.window_groups.first())
                .map(|g| g.id)
        })
}

/// Record "this app may read this file" in the active workspace.
///
/// Read-only and session-scoped, the same shape a file drop records.
/// Skipped when a live grant already covers the path — reopening the
/// same document must not pile up duplicate capabilities.
fn grant_read_for_open(
    package_id: &str,
    path: &Path,
    chrome: &Chrome,
) -> Result<(), OpenError> {
    use crate::shell::state::{Capability, FsMode, GRANT_SESSION};

    let ws_id = {
        let st = chrome.borrow();
        let tree = st.tree();
        if tree.active_workspace().is_none() {
            return Err(OpenError::NoWorkspace);
        }
        tree.active
    };
    let store = {
        let st = chrome.borrow();
        crate::portal::GrantStoreHandle::new(st.tree_handle())
    };
    let caller = crate::portal::PortalCaller::new(ws_id, package_id.to_string());
    if store.filesystem_path_granted(&caller, path) {
        return Ok(());
    }
    store.insert(
        &caller,
        Capability::Filesystem {
            path: path.to_path_buf(),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        },
    );
    chrome.borrow_mut().bump();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use parking_lot::RwLock;
    use std::sync::Arc;

    /// Write a registry + manifest pair so the real
    /// `launcher::installed_apps()` enumerates a package declaring
    /// `[providers].ext`.
    fn install(dir: &Path, package_id: &str, name: &str, exts: &[&str]) {
        let payload = dir.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        let manifest_path = payload.join("manifest.toml");
        let ext_list = exts
            .iter()
            .map(|e| format!("\"{e}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            &manifest_path,
            format!(
                "[package]\nid = \"{package_id}\"\nname = \"{name}\"\n\
                 version = \"1.0.0\"\n[entrypoint]\nbin = \"bin/app\"\n\
                 type = \"gui\"\n[providers]\nmime = []\next = [{ext_list}]\n"
            ),
        )
        .unwrap();
        let registry = dir.join("registry.toml");
        let mut existing = std::fs::read_to_string(&registry).unwrap_or_default();
        existing.push_str(&format!(
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        ));
        std::fs::write(&registry, existing).unwrap();
    }

    fn chrome_with_workspace() -> Chrome {
        let handle: WorkspaceTreeHandle =
            Arc::new(RwLock::new(WorkspaceTree::bootstrap_default()));
        ChromeState::new_with_handle(handle)
    }

    /// `.odt` is a `qdoc` document type. With a package declaring it,
    /// resolution must land on that package — not on a name the
    /// compositor made up.
    #[test]
    #[serial_test::serial(process_env)]
    fn declared_extension_resolves_to_the_declaring_package() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "dev.qos.word-processor", "Word Processor", &["odt", "md"]);
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        launcher::invalidate_app_cache();

        let target = resolve(Path::new("/w/report.odt"));
        match target {
            DocumentTarget::App {
                app,
                editable_document,
            } => {
                assert_eq!(app.package_id, "dev.qos.word-processor");
                assert!(
                    editable_document,
                    "the format registry knows .odt is an editable document"
                );
            }
            other => panic!("expected the declaring package, got {other:?}"),
        }
        // Case and a leading dot must not change the answer.
        assert!(matches!(
            resolve(Path::new("/w/NOTES.MD")),
            DocumentTarget::App { .. }
        ));

        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        launcher::invalidate_app_cache();
    }

    /// A document format with nothing installed to open it is its own
    /// answer — the user needs an editor, which is different from "this
    /// file is not a document".
    #[test]
    #[serial_test::serial(process_env)]
    fn document_with_no_installed_handler_is_distinguishable() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "dev.qos.calculator", "Calculator", &[]);
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        launcher::invalidate_app_cache();

        assert_eq!(
            resolve(Path::new("/w/report.docx")),
            DocumentTarget::NoAppForDocument {
                ext: "docx".to_string()
            },
            "docx is a qdoc format, so a missing handler is reportable"
        );
        assert_eq!(
            resolve(Path::new("/w/clip.mkv")),
            DocumentTarget::Unclaimed,
            "mkv is neither a qdoc format nor claimed by anything installed"
        );

        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        launcher::invalidate_app_cache();
    }

    /// Opening a document must leave a read capability for exactly that
    /// path, addressed to exactly that package, in the workspace — or
    /// the app launches into a sandbox the file is not in.
    ///
    /// The launch itself fails here (no Wayland socket in a unit test),
    /// which is the point: the grant is recorded *before* the launch, so
    /// it is observable either way and cannot be an afterthought.
    #[test]
    #[serial_test::serial(process_env)]
    fn opening_a_document_grants_read_for_that_path() {
        use crate::shell::state::{Effect, Scope, Subject};

        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "dev.qos.word-processor", "Word Processor", &["odt"]);
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        launcher::invalidate_app_cache();

        let doc_path = tmp.path().join("report.odt");
        std::fs::write(&doc_path, b"x").unwrap();
        let chrome = chrome_with_workspace();
        assert!(
            chrome
                .borrow()
                .tree()
                .active_workspace()
                .unwrap()
                .capabilities
                .is_empty(),
            "fixture precondition: the workspace starts with no grants"
        );

        let _ = open_in_workspace(&doc_path, &chrome);

        let st = chrome.borrow();
        let tree = st.tree();
        let ws = tree.active_workspace().unwrap();
        let want_subject = Subject::app("dev.qos.word-processor");
        let granted: Vec<_> = ws
            .capabilities
            .iter()
            .filter(|c| c.subject == want_subject && c.effect == Effect::Grant)
            .collect();
        assert_eq!(
            granted.len(),
            1,
            "exactly one grant for the opening app: {:?}",
            ws.capabilities
        );
        assert_eq!(
            granted[0].scope,
            Scope::FsPath(doc_path.display().to_string()),
            "the grant must name the document's own path, not a directory \
             above it"
        );
        drop(tree);
        drop(st);

        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        launcher::invalidate_app_cache();
    }

    /// Nothing installed claims `.zzz`, so the open is refused rather
    /// than silently launching something arbitrary.
    #[test]
    #[serial_test::serial(process_env)]
    fn unclaimed_extension_refuses_with_a_reportable_reason() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "dev.qos.calculator", "Calculator", &[]);
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        launcher::invalidate_app_cache();

        let chrome = chrome_with_workspace();
        let err = open_in_workspace(Path::new("/w/thing.zzz"), &chrome)
            .expect_err("nothing claims .zzz");
        assert_eq!(
            err,
            OpenError::NoHandler {
                path: PathBuf::from("/w/thing.zzz"),
                ext: Some("zzz".to_string()),
            }
        );
        assert!(err.user_message().contains(".zzz"));
        assert!(
            chrome
                .borrow()
                .tree()
                .active_workspace()
                .unwrap()
                .capabilities
                .is_empty(),
            "a refused open must not leave a grant behind"
        );

        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        launcher::invalidate_app_cache();
    }
}
