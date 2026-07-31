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
//!
//! ## Opening what is already open
//!
//! Activating the same document twice must not start a second copy of
//! its app, and answering "is this already open?" needs the document to
//! have an identity. It has one: a file *is* a record whose identity is
//! its path ([`ui::record::RecordId::for_file`]), so two spellings of
//! one path are one record and the question is a lookup rather than a
//! guess from window titles.
//!
//! [`OPEN_DOCUMENTS`] is the answer's other half — which window arrived
//! for which record. A launch and the window it produces are correlated
//! the same way the placement note is: the launch leaves a note against
//! the package, and the first toplevel that package adopts claims it
//! ([`claim_surface_for_document`], called from
//! [`crate::compositor::tab_binding::adopt_toplevel`]). A record whose
//! window has since closed is *not* open — the note is dropped and the
//! next open launches for real.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ui::record::RecordId;

use crate::compositor::tab_binding::Placement;
use crate::shell::chrome_state::Chrome;
use crate::shell::launcher::{self, InstalledApp, LaunchError};
use crate::shell::state::{GroupId, StripId, TabId, TileContent, WorkspaceId, WorkspaceTree};

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

/// What an open actually did.
///
/// Two outcomes, because they are two different things to have happened
/// to the user's desktop and a caller reporting "opened" for both would
/// be lying about one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// A new instance of the app was started with the document.
    Launched { app: InstalledApp },
    /// The document was already open here, so its window was focused
    /// instead of a second copy being started.
    ///
    /// `tab` is the tab now in front of the user, or `None` when the
    /// first open's window has not arrived yet — still not a reason to
    /// launch again, and still not a launch to report.
    Focused {
        app: InstalledApp,
        tab: Option<TabId>,
    },
}

impl Opened {
    /// The application the document belongs to, either way.
    pub fn app(&self) -> &InstalledApp {
        match self {
            Opened::Launched { app } | Opened::Focused { app, .. } => app,
        }
    }

    /// Whether this open started a process.
    pub fn launched(&self) -> bool {
        matches!(self, Opened::Launched { .. })
    }
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
            OpenError::NoWorkspace => "there is no active workspace to open it in".to_string(),
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
/// The one open path. Every surface that activates a file — the search
/// palette's file rows, the file explorer, a file dropped onto a window
/// group — comes through here, so a document opened from any of them
/// lands in the same sandbox with the same capabilities and the same
/// identity as one opened from any other.
///
/// If the document is already open, its window is focused and nothing is
/// launched (see the module docs). Otherwise a session-scoped read
/// capability for the path is recorded, and then
/// [`launcher::launch_document_in_workspace`] starts the app — the same
/// launch the palette's Applications rows take, so the app gets the
/// workspace's grants and its collaboration room like any other launch.
///
/// `Err(OpenError::NoHandler)` means nothing installed claims the type;
/// callers decide whether to fall back to the desktop's default handler
/// or to say so.
pub fn open_in_workspace(path: &Path, chrome: &Chrome) -> Result<Opened, OpenError> {
    open(path, chrome, None)
}

/// [`open_in_workspace`] for a caller that already knows which window
/// group the document belongs to — the group a file was dropped onto.
///
/// A drop target is the user's own instruction rather than a placement
/// rule: the pointer named a group, so the document goes in that group.
/// Every other open has no such target and asks
/// [`placement_for_document_open`] instead.
pub fn open_in_group(path: &Path, chrome: &Chrome, group: GroupId) -> Result<Opened, OpenError> {
    open(path, chrome, Some(Placement::TabIn(group)))
}

fn open(
    path: &Path,
    chrome: &Chrome,
    placement: Option<Placement>,
) -> Result<Opened, OpenError> {
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

    // The document's identity, and with it the answer to "is this
    // already open". Asked before anything is granted or launched: a
    // second activation of an open document must change nothing except
    // which window is in front.
    let record = RecordId::for_file(path);
    match existing_window_for(&record, chrome) {
        Some(Existing::Window(tab)) => {
            focus(chrome, tab);
            return Ok(Opened::Focused {
                app,
                tab: Some(tab.tab),
            });
        }
        Some(Existing::Starting) => return Ok(Opened::Focused { app, tab: None }),
        None => {}
    }

    // Grant before launching: `launch_document_in_workspace` publishes
    // the workspace's capability list as it stands at call time, so a
    // grant recorded after this point would not reach this launch.
    grant_read_for_open(&app.package_id, path, chrome)?;

    if let Some(p) = placement.or_else(|| placement_for_document_open(chrome)) {
        launcher::set_pending_placement(&app.package_id, p);
    }

    // Note the document as opening *before* the launch: the window can
    // arrive (and try to claim the note) as soon as the launch thread
    // starts.
    note_opening(&record, &app.package_id);

    let launched = {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.active_workspace()
            .map(|ws| launcher::launch_document_in_workspace(&app.package_id, ws, Some(path)))
    };
    match launched {
        Some(Ok(app)) => Ok(Opened::Launched { app }),
        // A launch that never started leaves no window, so the note
        // would make the next attempt believe the document was already
        // opening and refuse to try again.
        Some(Err(e)) => {
            forget_document(&record);
            Err(OpenError::Launch(e))
        }
        None => {
            forget_document(&record);
            Err(OpenError::NoWorkspace)
        }
    }
}

/// Where the window of a document opened from another surface lands.
///
/// **This is a seam, not a rule.** Where a window goes is the window
/// manager's decision and it is being written down in one place
/// elsewhere (`shell::placement::note_placement_for_launch`, on the
/// `window-placement` branch). This module needs an answer today, so it
/// applies the rule already documented on
/// [`crate::compositor::tab_binding::Placement::TabIn`] — a document
/// opened from somewhere else belongs *with* whatever asked for it, same
/// task, so a tab in the focused group rather than a new column.
///
/// It is deliberately the only place this module decides placement, so
/// replacing it with the call to the shared rule is a one-function
/// change:
///
/// ```ignore
/// crate::shell::placement::note_placement_for_launch(&app.package_id, &tree, None);
/// ```
///
/// (in place of the `set_pending_placement` call in [`open`]).
fn placement_for_document_open(chrome: &Chrome) -> Option<Placement> {
    focused_group(chrome).map(Placement::TabIn)
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
    tree.active_group_for(strip_id).or_else(|| {
        ws.strips
            .iter()
            .find(|s| s.id == strip_id)
            .and_then(|s| s.window_groups.first())
            .map(|g| g.id)
    })
}

/// Tell the user, by name, that a file could not be opened.
///
/// Every surface that can open a file reports a failed open through here,
/// because the failure is the same fact wherever the open came from: this
/// file, and nothing installed opens it. Doing nothing is not an option —
/// a silent activation is indistinguishable from a broken desktop, and it
/// is the one outcome the user cannot act on.
///
/// `surface` is the part of the desktop the user was in when they asked
/// (`"Files"` for the palette's file rows), or `None` when the answer is
/// simply the shell's.
pub fn report_open_failure(chrome: &Chrome, path: &Path, detail: &str, surface: Option<&str>) {
    use crate::shell::notification::{Notification, NotificationSeverity};
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let mut n = Notification::new(format!("Couldn't open {name}"), detail.to_string())
        .with_severity(NotificationSeverity::Error);
    if let Some(surface) = surface {
        n = n.with_app_name(surface);
    }
    let mut st = chrome.borrow_mut();
    st.push_notification_full(n);
    st.bump();
}

// ---------------------------------------------------------------------
// Which documents are open, and in which window.
// ---------------------------------------------------------------------

/// One document this session opened.
#[derive(Debug, Clone)]
struct OpenDocument {
    /// The record the document *is* — `file:<absolute path>`.
    record: RecordId,
    /// The package the document was opened in. What correlates the
    /// launch with the window it produces.
    package_id: String,
    /// The wayland surface that arrived for the launch, once one has.
    /// `None` means the app is still starting.
    surface: Option<u64>,
}

/// Documents this session has opened, and the window each landed in.
///
/// Process-wide because the fact is process-wide: a document is open in
/// *this session*, not in one workspace or one widget tree. The same
/// reason [`launcher::set_pending_placement`] lives beside the launch
/// rather than in the tree.
static OPEN_DOCUMENTS: Mutex<Vec<OpenDocument>> = Mutex::new(Vec::new());

/// The four coordinates that address a tab. Focus needs all of them —
/// setting a subset leaves the user looking at a different workspace
/// than the tab they asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabAddress {
    workspace: WorkspaceId,
    strip: StripId,
    group: GroupId,
    tab: TabId,
}

/// Whether this session already has a record open, and how far along.
#[derive(Debug, Clone, Copy)]
enum Existing {
    /// A live window shows it, in this tab.
    Window(TabAddress),
    /// A launch for it is in flight and its window has not arrived.
    Starting,
}

/// Note that `record` is being opened in `package_id`.
///
/// Keyed by record, so re-noting the same document replaces rather than
/// duplicates.
fn note_opening(record: &RecordId, package_id: &str) {
    let mut open = OPEN_DOCUMENTS.lock().expect("OPEN_DOCUMENTS poisoned");
    open.retain(|d| &d.record != record);
    open.push(OpenDocument {
        record: record.clone(),
        package_id: package_id.to_string(),
        surface: None,
    });
}

fn forget_document(record: &RecordId) {
    OPEN_DOCUMENTS
        .lock()
        .expect("OPEN_DOCUMENTS poisoned")
        .retain(|d| &d.record != record);
}

/// Drop every note. Used by tests, which share this process-wide state.
pub fn clear_open_documents() {
    OPEN_DOCUMENTS
        .lock()
        .expect("OPEN_DOCUMENTS poisoned")
        .clear();
}

/// One entry of [`open_documents`]: which record, opened in which
/// package, showing in which wayland surface (`None` while starting).
pub type OpenDocumentView = (String, String, Option<u64>);

/// What this session has open, for anything that has to report it.
///
/// The binding of a window to the document it holds is not derivable
/// from the workspace tree — a tab records its app, not its content — so
/// without this an observer can see that *an* app window appeared and
/// not that it is the one showing this file. `state_dump` publishes it
/// for that reason.
pub fn open_documents() -> Vec<OpenDocumentView> {
    OPEN_DOCUMENTS
        .lock()
        .expect("OPEN_DOCUMENTS poisoned")
        .iter()
        .map(|d| {
            (
                d.record.as_str().to_string(),
                d.package_id.clone(),
                d.surface,
            )
        })
        .collect()
}

/// Attribute a freshly-adopted toplevel to the document it was launched
/// for, if it was launched for one.
///
/// Called from [`crate::compositor::tab_binding::adopt_toplevel`], beside
/// the placement lookup, and correlated the same way: by the client's
/// wayland app-id against the package we launched. The oldest unclaimed
/// note for that package wins, which is launch order — the best available
/// answer when one app is opening two documents at once, and a
/// deterministic one rather than a race.
///
/// Idempotent per surface: adoption can run more than once for a window,
/// and a surface that already holds a note must not steal a second.
pub fn claim_surface_for_document(app_id: &str, surface_id: u64) {
    let mut open = OPEN_DOCUMENTS.lock().expect("OPEN_DOCUMENTS poisoned");
    if open.iter().any(|d| d.surface == Some(surface_id)) {
        return;
    }
    if let Some(d) = open
        .iter_mut()
        .find(|d| d.surface.is_none() && launcher::app_id_matches_package(app_id, &d.package_id))
    {
        d.surface = Some(surface_id);
    }
}

/// Whether `record` is already open, and where.
///
/// A note whose window has since closed is not "open": it is dropped
/// here, so the caller launches for real rather than focusing nothing.
fn existing_window_for(record: &RecordId, chrome: &Chrome) -> Option<Existing> {
    let surface = {
        let open = OPEN_DOCUMENTS.lock().expect("OPEN_DOCUMENTS poisoned");
        let d = open.iter().find(|d| &d.record == record)?;
        match d.surface {
            Some(s) => s,
            None => return Some(Existing::Starting),
        }
    };
    let found = {
        let st = chrome.borrow();
        let tree = st.tree();
        tab_showing_surface(&tree, surface)
    };
    match found {
        Some(addr) => Some(Existing::Window(addr)),
        None => {
            forget_document(record);
            None
        }
    }
}

/// The tab hosting the foreign client on `surface_id`, anywhere in the
/// tree.
///
/// Searched across every workspace, not just the active one: a document
/// opened in one workspace and then looked for from another is still
/// open, and focusing it means switching to where it is.
fn tab_showing_surface(tree: &WorkspaceTree, surface_id: u64) -> Option<TabAddress> {
    for w in &tree.workspaces {
        for s in &w.strips {
            for g in &s.window_groups {
                for t in &g.tabs {
                    if matches!(&t.tile, TileContent::Foreign { surface_id: sid, .. } if *sid == surface_id)
                    {
                        return Some(TabAddress {
                            workspace: w.id,
                            strip: s.id,
                            group: g.id,
                            tab: t.id,
                        });
                    }
                }
            }
        }
    }
    None
}

fn focus(chrome: &Chrome, at: TabAddress) {
    chrome
        .borrow_mut()
        .focus_tab(at.workspace, at.strip, at.group, at.tab);
}

/// Record "this app may read this file" in the active workspace.
///
/// Read-only and session-scoped, the same shape a file drop records.
/// Skipped when a live grant already covers the path — reopening the
/// same document must not pile up duplicate capabilities.
fn grant_read_for_open(package_id: &str, path: &Path, chrome: &Chrome) -> Result<(), OpenError> {
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
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::bootstrap_default()));
        ChromeState::new_with_handle(handle)
    }

    /// Put a stand-in `qos-launcher` in `dir` that records the argv it is
    /// invoked with, and point the shell at it.
    ///
    /// `QOS_LAUNCHER_BIN` is the documented seam for exactly this (see
    /// `launcher::launcher_bin_candidates`), so what the log ends up
    /// holding is the command the production launch path really ran —
    /// not a value any code under test returned.
    ///
    /// Returns the log path.
    fn install_recording_launcher(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("launched.log");
        let bin = dir.join("qos-launcher");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: every caller is in the `process_env` serial group.
        unsafe {
            std::env::set_var("QOS_LAUNCHER_BIN", &bin);
            std::env::set_var("XDG_RUNTIME_DIR", dir);
        }
        launcher::set_wayland_socket_path(Some(dir.join("wayland-0")));
        log
    }

    /// Every launch the shell started, as the argv the launcher saw.
    /// Waits for at least `want` of them — a launch runs on its own
    /// thread, so it is not observable the instant `open` returns.
    fn launched_commands(log: &Path, want: usize) -> Vec<String> {
        for _ in 0..400 {
            if let Ok(s) = std::fs::read_to_string(log) {
                let lines: Vec<String> = s.lines().map(str::to_string).collect();
                if lines.len() >= want {
                    return lines;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let got = std::fs::read_to_string(log).unwrap_or_default();
        panic!("expected {want} launch(es); the launcher recorded:\n{got}");
    }

    /// Reset the process-wide state a launch and an open both write to.
    fn reset_session_state() {
        launcher::invalidate_app_cache();
        launcher::clear_pending_placements();
        let _ = launcher::drain_events();
        clear_open_documents();
    }

    fn clear_launch_env() {
        // SAFETY: every caller is in the `process_env` serial group.
        unsafe {
            std::env::remove_var("QOS_APPS_DATA_DIR");
            std::env::remove_var("QOS_LAUNCHER_BIN");
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        launcher::set_wayland_socket_path(None);
        reset_session_state();
    }

    /// Three formats, three different owning packages, one open path.
    ///
    /// The assertion is on the command the compositor actually executed
    /// and on the launch events the compositor itself queued — not on
    /// what `open_in_workspace` handed back.
    #[test]
    #[serial_test::serial(process_env)]
    fn each_format_opens_in_the_package_that_declares_it() {
        let tmp = tempfile::tempdir().unwrap();
        install(
            tmp.path(),
            "dev.qos.word-processor",
            "Word Processor",
            &["odt"],
        );
        install(tmp.path(), "dev.qos.pdf-viewer", "PDF Viewer", &["pdf"]);
        install(tmp.path(), "dev.qos.text-editor", "Text Editor", &["md"]);
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        let log = install_recording_launcher(tmp.path());
        reset_session_state();

        let chrome = chrome_with_workspace();
        let cases = [
            ("report.odt", "dev.qos.word-processor"),
            ("manual.pdf", "dev.qos.pdf-viewer"),
            ("notes.md", "dev.qos.text-editor"),
        ];
        for (name, _) in cases {
            let path = tmp.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            let opened = open_in_workspace(&path, &chrome)
                .unwrap_or_else(|e| panic!("{name} must open: {e}"));
            assert!(opened.launched(), "{name} had to start its app");
        }

        let commands = launched_commands(&log, 3);
        for (name, package) in cases {
            let want_path = tmp.path().join(name).display().to_string();
            let hit = commands
                .iter()
                .find(|c| c.contains(&format!("--app-id {package}")))
                .unwrap_or_else(|| {
                    panic!("no launch of {package}; the launcher ran:\n{commands:#?}")
                });
            assert!(
                hit.contains(&format!("--open {want_path}")),
                "{package} was launched without the document: {hit}"
            );
        }

        // The compositor's own record of what it started.
        let started: Vec<String> = launcher::drain_events()
            .into_iter()
            .filter(|e| matches!(e.kind, launcher::LaunchEventKind::Started { .. }))
            .map(|e| e.package_id)
            .collect();
        for (_, package) in cases {
            assert!(
                started.iter().any(|p| p == package),
                "the compositor did not record starting {package}: {started:?}"
            );
        }

        clear_launch_env();
    }

    /// Opening a document that is already open focuses its window. One
    /// instance, one launch — asserted against the tree the compositor
    /// paints and the commands it ran.
    #[test]
    #[serial_test::serial(process_env)]
    fn opening_an_open_document_focuses_it_instead_of_launching_again() {
        let tmp = tempfile::tempdir().unwrap();
        install(
            tmp.path(),
            "dev.qos.word-processor",
            "Word Processor",
            &["odt"],
        );
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        let log = install_recording_launcher(tmp.path());
        reset_session_state();

        let doc = tmp.path().join("report.odt");
        std::fs::write(&doc, b"x").unwrap();
        let chrome = chrome_with_workspace();

        assert!(
            open_in_workspace(&doc, &chrome).unwrap().launched(),
            "the first open starts the app"
        );
        let after_first = launched_commands(&log, 1);
        assert_eq!(after_first.len(), 1);

        // The app's window arrives, through the production adoption path.
        const SURFACE: u64 = 4242;
        let adopted = {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            crate::compositor::tab_binding::adopt_toplevel(
                &mut tree,
                SURFACE,
                Some("word-processor".to_string()),
            )
        };
        // Focus something else first, so "it is focused" is a claim about
        // this open rather than about where the window happened to land.
        {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            let strip = tree.workspaces[0].strips[0].id;
            let other = tree.workspaces[0].strips[0].window_groups[0].id;
            let first_tab = tree.workspaces[0].strips[0].window_groups[0].tabs[0].id;
            tree.set_active_group(strip, other);
            if let Some(g) = tree.workspaces[0].strips[0].window_groups.first_mut() {
                g.active_tab = first_tab;
            }
        }

        let again = open_in_workspace(&doc, &chrome).expect("the second open must succeed");
        assert_eq!(
            again,
            Opened::Focused {
                app: match resolve(&doc) {
                    DocumentTarget::App { app, .. } => app,
                    other => panic!("expected an owning app, got {other:?}"),
                },
                tab: Some(adopted),
            },
            "the second open focuses the window the first one produced"
        );

        // Exactly one window for the document, and it is the one in
        // front — read out of the tree, not out of the return value.
        let st = chrome.borrow();
        let tree = st.tree();
        let foreign: Vec<u64> = tree
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .flat_map(|g| g.tabs.iter())
            .filter_map(|t| match &t.tile {
                TileContent::Foreign { surface_id, .. } => Some(*surface_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            foreign,
            vec![SURFACE],
            "a second open must not add a second window"
        );

        let addr = tab_showing_surface(&tree, SURFACE).expect("the window is in the tree");
        assert_eq!(tree.active, addr.workspace, "its workspace is active");
        assert_eq!(
            tree.active_strip.get(&addr.workspace).copied(),
            Some(addr.strip),
            "its strip is active"
        );
        assert_eq!(
            tree.active_group_for(addr.strip),
            Some(addr.group),
            "its group is active"
        );
        let group = tree.workspaces[0]
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == addr.group)
            .expect("the group exists");
        assert_eq!(group.active_tab, adopted, "and its tab is the active one");
        drop(tree);
        drop(st);

        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "the second open must not have run the launcher again"
        );

        clear_launch_env();
    }

    /// Activating a row in the hosted file explorer opens the document in
    /// the app that declares its type.
    ///
    /// Driven through the explorer's own public activation
    /// (`open_entry`), with the route the shell installs in production
    /// ([`crate::shell::install_document_opener`]) — so what is under test
    /// is the wiring, not a call to `open_in_workspace` this test made
    /// itself.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_file_explorer_opens_a_document_in_the_app_that_owns_it() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        install(&apps, "dev.qos.word-processor", "Word Processor", &["odt"]);
        // The directory the explorer lists, kept apart from the fixture
        // files so the row indices are the documents.
        let files = tmp.path().join("files");
        std::fs::create_dir_all(&files).unwrap();
        let doc = files.join("agenda.odt");
        std::fs::write(&doc, b"x").unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", &apps) };
        let log = install_recording_launcher(tmp.path());
        reset_session_state();

        let chrome = chrome_with_workspace();
        crate::shell::install_document_opener(&chrome);

        let mut explorer = file_explorer::FileExplorerWidget::new();
        explorer.navigate(files.clone());
        let idx = explorer
            .displayed_entries()
            .iter()
            .position(|e| e.name == "agenda.odt")
            .expect("the explorer lists the document");
        let mut runner = file_explorer::files_app::SystemRunner;
        assert!(
            explorer.open_entry(idx, &mut runner).unwrap(),
            "activating a file row hands it off"
        );

        let commands = launched_commands(&log, 1);
        assert_eq!(commands.len(), 1, "exactly one launch: {commands:#?}");
        assert!(
            commands[0].contains("--app-id dev.qos.word-processor")
                && commands[0].contains(&format!("--open {}", doc.display())),
            "the explorer's activation had to reach the owning app with the \
             document: {}",
            commands[0]
        );

        // And the read grant came with it, scoped to the file.
        {
            use crate::shell::state::{Effect, Scope, Subject};
            let st = chrome.borrow();
            let tree = st.tree();
            let ws = tree.active_workspace().unwrap();
            let want = Subject::app("dev.qos.word-processor");
            let scopes: Vec<&Scope> = ws
                .capabilities
                .iter()
                .filter(|c| c.subject == want && c.effect == Effect::Grant)
                .map(|c| &c.scope)
                .collect();
            assert_eq!(scopes, vec![&Scope::FsPath(doc.display().to_string())]);
        }

        file_explorer::files_app::clear_document_opener();
        clear_launch_env();
    }

    /// A type nothing installed claims is refused in words the explorer
    /// shows, and nothing is launched.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_file_explorer_reports_a_type_nothing_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        install(&apps, "dev.qos.word-processor", "Word Processor", &["odt"]);
        let files = tmp.path().join("files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("archive.zzz"), b"x").unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", &apps) };
        let log = install_recording_launcher(tmp.path());
        reset_session_state();

        let chrome = chrome_with_workspace();
        crate::shell::install_document_opener(&chrome);

        let mut explorer = file_explorer::FileExplorerWidget::new();
        explorer.navigate(files);
        let idx = explorer
            .displayed_entries()
            .iter()
            .position(|e| e.name == "archive.zzz")
            .expect("the explorer lists the file");
        let mut runner = file_explorer::files_app::SystemRunner;
        explorer.open_entry(idx, &mut runner).unwrap();

        let status = explorer
            .status_line()
            .unwrap_or_else(|| panic!("the refusal must reach the user"))
            .to_string();
        assert!(
            status.contains("archive.zzz") && status.contains(".zzz"),
            "the message must name the file and its type: {status}"
        );
        assert!(
            !log.exists(),
            "nothing may be launched for a type nothing claims: {:?}",
            std::fs::read_to_string(&log)
        );

        file_explorer::files_app::clear_document_opener();
        clear_launch_env();
    }

    /// A document whose window has been closed is not open. The next
    /// activation launches for real rather than focusing nothing.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_closed_window_means_the_document_is_no_longer_open() {
        let tmp = tempfile::tempdir().unwrap();
        install(
            tmp.path(),
            "dev.qos.word-processor",
            "Word Processor",
            &["odt"],
        );
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        let log = install_recording_launcher(tmp.path());
        reset_session_state();

        let doc = tmp.path().join("report.odt");
        std::fs::write(&doc, b"x").unwrap();
        let chrome = chrome_with_workspace();
        open_in_workspace(&doc, &chrome).unwrap();
        launched_commands(&log, 1);
        {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            crate::compositor::tab_binding::adopt_toplevel(
                &mut tree,
                77,
                Some("word-processor".to_string()),
            );
        }
        {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            crate::compositor::tab_binding::close_tab_for_destroyed_surface(&mut tree, 77);
        }

        let reopened = open_in_workspace(&doc, &chrome).expect("reopening must work");
        assert!(
            reopened.launched(),
            "a document whose window is gone has to be launched again, got {reopened:?}"
        );
        // The second launch really ran, and there are exactly two.
        let commands = launched_commands(&log, 2);
        assert_eq!(commands.len(), 2, "one launch per activation: {commands:#?}");

        clear_launch_env();
    }

    /// `.odt` is a `qdoc` document type. With a package declaring it,
    /// resolution must land on that package — not on a name the
    /// compositor made up.
    #[test]
    #[serial_test::serial(process_env)]
    fn declared_extension_resolves_to_the_declaring_package() {
        let tmp = tempfile::tempdir().unwrap();
        install(
            tmp.path(),
            "dev.qos.word-processor",
            "Word Processor",
            &["odt", "md"],
        );
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
        install(
            tmp.path(),
            "dev.qos.word-processor",
            "Word Processor",
            &["odt"],
        );
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
        // Spelled out, because "scoped to the file" is the whole point:
        // no grant may name the directory the document sits in, which
        // would hand the app every other file beside it.
        let parent = doc_path.parent().unwrap().display().to_string();
        assert!(
            !ws.capabilities
                .iter()
                .any(|c| c.scope == Scope::FsPath(parent.clone())),
            "the open widened the grant to {parent}: {:?}",
            ws.capabilities
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

        let log = install_recording_launcher(tmp.path());
        reset_session_state();

        let chrome = chrome_with_workspace();
        let err =
            open_in_workspace(Path::new("/w/thing.zzz"), &chrome).expect_err("nothing claims .zzz");
        assert_eq!(
            err,
            OpenError::NoHandler {
                path: PathBuf::from("/w/thing.zzz"),
                ext: Some("zzz".to_string()),
            }
        );
        // The sentence a user is shown has to name the file and say that
        // nothing opens it — a bare "failed" is not actionable.
        let message = err.user_message();
        assert!(
            message.contains("thing.zzz") && message.contains(".zzz"),
            "the message must name the file and its type: {message}"
        );
        assert!(
            message.contains("no installed app opens"),
            "the message must say nothing can open it: {message}"
        );
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
        assert!(
            !log.exists(),
            "a refused open must not launch anything: {:?}",
            std::fs::read_to_string(&log)
        );

        clear_launch_env();
    }
}
