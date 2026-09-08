//! Opening a document in the application that owns it.
//!
//! Two registries answer half of "who owns this file" each: [`qdoc`] knows
//! which extensions are editable documents, while app manifests declare
//! normalized MIME and extension claims. [`resolve`] reports
//! both, since they fail differently — no app at all vs. no editor.
//!
//! A path handed to an app as a launch argument is not in its sandbox
//! namespace by virtue of being named, so [`open_in_workspace`] records
//! a read-only, session-scoped capability for it before launching.
//!
//! Activating an already-open document must not start a second copy:
//! [`OPEN_DOCUMENTS`] tracks a file's identity
//! ([`ui::record::RecordId::for_file`]) against the window each launch
//! produced, and a record whose window has since closed is not "open".

use std::io::Read;
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
    App {
        // Boxed: `InstalledApp` carries four `String`s, two `PathBuf`s and
        // three `Vec<String>`s, dwarfing the other variants' payloads.
        app: Box<InstalledApp>,
        /// `false` for a type an app claims that `qdoc` has no codec
        /// for — still a good open, just not a document one.
        editable_document: bool,
    },
    /// An editable document with no installed owner, distinct from
    /// [`Self::Unclaimed`] because it's actionable: the store can name an editor.
    NoAppForDocument { ext: String },
    /// Nothing claims the extension. Callers fall back to the desktop's
    /// default handler.
    Unclaimed,
    /// A directory — the caller navigates into it.
    Directory,
}

/// Two outcomes, because reporting "opened" for both would be lying
/// about one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Launched {
        app: InstalledApp,
    },
    /// The document was already open; its window was focused instead of
    /// starting a second copy. `tab` is `None` when the first open's
    /// window hasn't arrived yet — still not a reason to launch again.
    Focused {
        app: InstalledApp,
        tab: Option<TabId>,
    },
}

impl Opened {
    pub fn app(&self) -> &InstalledApp {
        match self {
            Opened::Launched { app } | Opened::Focused { app, .. } => app,
        }
    }

    pub fn launched(&self) -> bool {
        matches!(self, Opened::Launched { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    NoHandler {
        path: PathBuf,
        ext: Option<String>,
    },
    Launch(LaunchError),
    /// No active workspace, so no sandbox and no room to open into.
    NoWorkspace,
}

impl OpenError {
    pub fn user_message(&self) -> String {
        match self {
            OpenError::NoHandler { path, ext } => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().to_string(),
                );
                match ext {
                    Some(ext) => format!("no app on this desktop opens .{ext} files ({name})"),
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

pub fn extension_of(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.trim();
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Pure — touches neither the workspace nor the process table, so a
/// context menu can call it just to build an "Open with…" label.
pub fn resolve(path: &Path) -> DocumentTarget {
    resolve_for(path, "", None)
}

/// Resolve with the active workspace's policy and the authenticated person's
/// identity. Context-menu previews and launch use this same function.
pub fn resolve_for(
    path: &Path,
    user_id: &str,
    workspace_defaults: Option<&desktop_policy::WorkspaceDefaults>,
) -> DocumentTarget {
    if path.is_dir() {
        return DocumentTarget::Directory;
    }
    let ext = extension_of(path);
    let editable_document = ext.as_deref().is_some_and(qdoc::is_supported_ext);
    let mime = mime_guess::from_path(path).first_raw();
    let app = mime
        .and_then(|mime| desktop_policy::ResourceKind::mime(mime).ok())
        .and_then(|resource| {
            launcher::app_for_resource_with_policy(&resource, user_id, workspace_defaults)
        })
        .or_else(|| {
            ext.as_deref().and_then(|ext| {
                launcher::app_for_extension_with_policy(ext, user_id, workspace_defaults)
            })
        });
    match app {
        Some(app) => DocumentTarget::App {
            app: Box::new(app),
            editable_document,
        },
        None if editable_document => DocumentTarget::NoAppForDocument {
            ext: ext.unwrap_or_default(),
        },
        None => DocumentTarget::Unclaimed,
    }
}

/// The one open path — palette, file explorer and drop-on-group all
/// come through here, so a document lands in the same sandbox with the
/// same identity regardless of which surface activated it.
///
/// # Errors
///
/// Returns [`OpenError::NoHandler`] if no app claims `path`'s
/// extension, [`OpenError::NoWorkspace`] if there is no active
/// workspace to open into, or [`OpenError::Launch`] if starting the
/// app failed.
pub fn open_in_workspace(path: &Path, chrome: &Chrome) -> Result<Opened, OpenError> {
    open(path, chrome, None)
}

/// [`open_in_workspace`] for a caller that already knows the target
/// group — a drop target is the user's own instruction rather than a
/// placement rule. Every other open asks [`placement_for_document_open`].
///
/// # Errors
///
/// Same conditions as [`open_in_workspace`].
pub fn open_in_group(path: &Path, chrome: &Chrome, group: GroupId) -> Result<Opened, OpenError> {
    open(path, chrome, Some(Placement::TabIn(group)))
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "guard's only use is the borrow passed into resolve_for, released \
              immediately after; nothing to tighten without cloning workspace policy"
)]
fn open(path: &Path, chrome: &Chrome, placement: Option<Placement>) -> Result<Opened, OpenError> {
    // The guard feeds `resolve_for`'s `workspace_defaults` borrow directly
    // and the block ends the moment that call returns; cloning the policy
    // map out just to drop the guard a statement earlier would cost more
    // than the (synchronous, non-blocking) call it's guarding against.
    let target = {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.active_workspace();
        resolve_for(
            path,
            &state.local_user_id,
            workspace.map(|ws| &ws.default_apps),
        )
    };
    let app = match target {
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

    let claim_is_current = mime_guess::from_path(path)
        .first_raw()
        .and_then(|mime| desktop_policy::ResourceKind::mime(mime).ok())
        .is_some_and(|resource| launcher::revalidate_resource_claim(&app.package_id, &resource))
        || extension_of(path)
            .and_then(|ext| desktop_policy::ResourceKind::extension(&ext).ok())
            .is_some_and(|resource| {
                launcher::revalidate_resource_claim(&app.package_id, &resource)
            });
    if !claim_is_current {
        return Err(OpenError::NoHandler {
            path: path.to_path_buf(),
            ext: extension_of(path),
        });
    }

    // Asked before anything is granted or launched: a second activation
    // of an open document must change nothing but which window is in front.
    let record = RecordId::for_file(path);
    match existing_window_for(&record, chrome) {
        Some(Existing::Window(tab)) => {
            focus(chrome, tab);
            return Ok(Opened::Focused {
                app: *app,
                tab: Some(tab.tab),
            });
        }
        Some(Existing::Starting) => {
            return Ok(Opened::Focused {
                app: *app,
                tab: None,
            });
        }
        None => {}
    }

    // Must precede launch: `launch_document_in_workspace` publishes the
    // capability list as it stands at call time.
    grant_read_for_open(&app.package_id, path, chrome)?;

    if let Some(p) = placement.or_else(|| placement_for_document_open(chrome)) {
        launcher::set_pending_placement(&app.package_id, p);
    }

    // Before the launch: the window can arrive and claim the note as
    // soon as the launch thread starts.
    note_opening(&record, path, &app.package_id);

    let launched = {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.active_workspace()
            .map(|ws| launcher::launch_document_in_workspace(&app.package_id, ws, Some(path)))
    };
    match launched {
        Some(Ok(app)) => Ok(Opened::Launched { app }),
        // Otherwise the stale note would make the next attempt believe
        // this document is already opening and refuse to try again.
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

/// A document opened elsewhere belongs *with* whatever asked for it —
/// a tab in the focused group rather than a new column.
fn placement_for_document_open(chrome: &Chrome) -> Option<Placement> {
    focused_group(chrome).map(Placement::TabIn)
}

/// `None` when there's no strip or group to be "beside" — adoption
/// bootstraps its own in that case.
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

/// Every surface that can open a file reports through here — silent
/// failure is indistinguishable from a broken desktop.
pub fn report_open_failure(chrome: &Chrome, path: &Path, detail: &str, surface: Option<&str>) {
    use crate::shell::notification::{Notification, NotificationSeverity};
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    );
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

#[derive(Debug, Clone)]
struct OpenDocument {
    record: RecordId,
    /// Kept only in the local process so continuation can read the FUSE
    /// record projection. It is never serialized into a checkpoint.
    path: PathBuf,
    /// Correlates the launch with the window it produces.
    package_id: String,
    /// The wayland surface that arrived, once one has. `None` while starting.
    surface: Option<u64>,
}

/// Process-wide: a document is open in *this session*, not in one
/// workspace or one widget tree.
static OPEN_DOCUMENTS: Mutex<Vec<OpenDocument>> = Mutex::new(Vec::new());

/// Lock [`OPEN_DOCUMENTS`], recovering from poisoning instead of
/// panicking: a panic while holding this lock doesn't corrupt the open
/// document list it protects, so a poisoned lock is treated the same
/// as a healthy one rather than wedging every future caller in this
/// process.
fn lock_open_documents() -> std::sync::MutexGuard<'static, Vec<OpenDocument>> {
    OPEN_DOCUMENTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Focus needs all four coordinates — a subset leaves the user looking
/// at a different workspace than the tab they asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabAddress {
    workspace: WorkspaceId,
    strip: StripId,
    group: GroupId,
    tab: TabId,
}

#[derive(Debug, Clone, Copy)]
enum Existing {
    Window(TabAddress),
    /// A launch for it is in flight and its window has not arrived.
    Starting,
}

/// Keyed by record, so re-noting the same document replaces rather
/// than duplicates.
fn note_opening(record: &RecordId, path: &Path, package_id: &str) {
    let mut open = lock_open_documents();
    open.retain(|d| &d.record != record);
    open.push(OpenDocument {
        record: record.clone(),
        path: path.to_path_buf(),
        package_id: package_id.to_string(),
        surface: None,
    });
}

fn forget_document(record: &RecordId) {
    lock_open_documents().retain(|d| &d.record != record);
}

pub fn clear_open_documents() {
    lock_open_documents().clear();
}

/// Return a privacy-safe continuation reference for a document surface.  The
/// ordinary `RecordId::for_file` embeds an absolute local path and must never
/// leave this device; continuity therefore carries only a stable hash-derived
/// reference (and the same hash as its content/reference fingerprint).
pub fn continuation_file_reference_for_surface(
    surface_id: u64,
) -> Option<crate::shell::personal_continuity::LaunchIntent> {
    let open = lock_open_documents();
    let path = open
        .iter()
        .find(|document| document.surface == Some(surface_id))?
        .path
        .clone();
    drop(open);
    let record_id = xattr::get(&path, "user.qos.record-id")
        .ok()
        .flatten()
        .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
        .or_else(|| {
            // Older/local mounts may expose the bounded root catalogue before
            // the per-file identity xattr. Preserve continuation there without
            // walking the tree: the legacy catalogue is capped and validated
            // by Files, while current large catalogues use the exact file xattr.
            file_explorer::model::read_workspace_record_index(
                &file_explorer::model::workspace_record_authority_roots(),
            )
            .records
            .into_iter()
            .find(|record| record.path == *path)
            .map(|record| *record.record_id.as_bytes())
        })?;
    let content_hash = xattr::get(&path, "user.qos.content-hash")
        .ok()
        .flatten()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
    // FUSE exposes preview bytes, never a path. Persist only their hash so
    // the receiver can request/cache its own projected thumbnail.
    let thumbnail_hash = xattr::get(&path, "user.qos.thumb")
        .ok()
        .flatten()
        .map(|bytes| *blake3::hash(&bytes).as_bytes());
    crate::shell::personal_continuity::LaunchIntent::file_reference(
        format!("file-record-{}", hex::encode(record_id)),
        content_hash,
        thumbnail_hash,
    )
    .ok()
}

/// Resolve a same-identity file continuation through the mounted Files record
/// catalog. The incoming reference names a collaborative `FileRecordId`, not
/// a source pathname. Opening one byte is deliberate: on a projected FUSE
/// record it begins normal on-demand hydration, while local files are cheap.
pub fn open_continuity_file_reference(
    reference: &crate::shell::personal_continuity::LaunchIntent,
) -> Option<PathBuf> {
    let path = resolve_continuity_file_reference_in_roots(
        reference,
        &file_explorer::model::workspace_record_authority_roots(),
    )?;
    let mut file = std::fs::File::open(&path).ok()?;
    let mut first_byte = [0_u8; 1];
    let _ = file.read(&mut first_byte).ok()?;
    Some(path)
}

/// A human label for a continuation file card. The record id is deliberately
/// never user-facing: it is a transport identity, not a filename.
pub fn continuity_file_reference_label(
    reference: &crate::shell::personal_continuity::LaunchIntent,
) -> String {
    resolve_continuity_file_reference_in_roots(
        reference,
        &file_explorer::model::workspace_record_authority_roots(),
    )
    .and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
    .filter(|name| !name.trim().is_empty())
    .unwrap_or_else(|| "File".to_string())
}

/// Production activation for a synchronized file-reference tab.  Resolution
/// stays record-id based, then the ordinary document launcher grants the
/// projected path and chooses the registered app exactly as for a local open.
pub fn activate_continuity_file_tab(chrome: &Chrome, tab: TabId) -> bool {
    let reference = chrome
        .borrow()
        .tree()
        .continuation_file_references
        .get(&tab)
        .cloned();
    let Some(reference) = reference else {
        return false;
    };
    let Some(path) = open_continuity_file_reference(&reference) else {
        return false;
    };
    open_in_workspace(&path, chrome).is_ok()
}

fn resolve_continuity_file_reference_in_roots(
    reference: &crate::shell::personal_continuity::LaunchIntent,
    roots: &[PathBuf],
) -> Option<PathBuf> {
    let crate::shell::personal_continuity::LaunchIntent::FileReference { record_id, .. } =
        reference
    else {
        return None;
    };
    let encoded = record_id.strip_prefix("file-record-")?;
    let bytes = hex::decode(encoded).ok()?;
    let record_id: [u8; 16] = bytes.try_into().ok()?;
    let entry =
        file_explorer::model::resolve_workspace_record(roots, uuid::Uuid::from_bytes(record_id))?;
    // Hashes describe the capture-time content/preview and may seed a cache,
    // but they must never turn a normal edit into a failed reopen. The stable
    // collaborative FileRecordId above is the sole routing authority.
    Some(entry.path)
}

/// Matches by wayland app-id; the oldest unclaimed note wins (launch
/// order) when one app opens two documents at once. Idempotent per
/// surface — one that already holds a note must not steal a second.
pub fn claim_surface_for_document(app_id: &str, surface_id: u64) {
    let mut open = lock_open_documents();
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

/// A note whose window has since closed is not "open": dropped here, so
/// the caller launches for real rather than focusing nothing.
fn existing_window_for(record: &RecordId, chrome: &Chrome) -> Option<Existing> {
    let surface = {
        let open = lock_open_documents();
        let surface_opt = open.iter().find(|d| &d.record == record)?.surface;
        drop(open);
        match surface_opt {
            Some(s) => s,
            None => return Some(Existing::Starting),
        }
    };
    let found = {
        let st = chrome.borrow();
        let tree = st.tree();
        tab_showing_surface(&tree, surface)
    };
    if let Some(addr) = found {
        Some(Existing::Window(addr))
    } else {
        forget_document(record);
        None
    }
}

/// Anywhere in the tree, not just the active workspace — focusing it
/// may mean switching to where it is.
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

/// Read-only, session-scoped, the same shape a file drop records.
/// Skipped when a live grant already covers the path — reopening must
/// not pile up duplicate capabilities.
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
        &Capability::Filesystem {
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
    use std::fmt::Write as _;
    use std::sync::Arc;

    /// A registry + manifest pair so `launcher::installed_apps()`
    /// enumerates a package declaring `[providers].ext`.
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
        write!(
            existing,
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        )
        .unwrap();
        std::fs::write(&registry, existing).unwrap();
    }

    fn chrome_with_workspace() -> Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::bootstrap_demo()));
        ChromeState::new_with_handle(handle)
    }

    /// A stand-in `qos-launcher` that records its argv, wired in via
    /// `QOS_LAUNCHER_BIN` so the log holds what production really ran.
    fn install_recording_launcher(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("launched.log");
        let bin = dir.join("qos-launcher");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
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

    /// Waits for at least `want` — a launch runs on its own thread, not
    /// observable the instant `open` returns.
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
            std::env::remove_var("QOS_WORKSPACE_ROOTS");
        }
        launcher::set_wayland_socket_path(None);
        reset_session_state();
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn continuation_uses_root_catalog_when_file_xattr_is_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let document = root.path().join("deep/report.pdf");
        std::fs::create_dir_all(document.parent().unwrap()).unwrap();
        std::fs::write(&document, b"report").unwrap();
        let record_id = uuid::Uuid::from_u128(0x5151);
        let projection = serde_json::json!({
            "schema_version": 1,
            "truncated": false,
            "records": [{
                "record_id": record_id,
                "path": "/deep/report.pdf",
                "is_dir": false
            }]
        });
        xattr::set(
            root.path(),
            file_explorer::model::RECORD_INDEX_XATTR,
            &serde_json::to_vec(&projection).unwrap(),
        )
        .unwrap();
        // SAFETY: guarded by the process_env serial group.
        unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", root.path()) };
        clear_open_documents();
        note_opening(
            &RecordId::for_file(&document),
            &document,
            "dev.qos.pdf-viewer",
        );
        claim_surface_for_document("dev.qos.pdf-viewer", 77);

        let reference = continuation_file_reference_for_surface(77)
            .expect("the root catalog supplies stable identity without a file xattr");
        assert!(matches!(
            reference,
            crate::shell::personal_continuity::LaunchIntent::FileReference {
                record_id: ref id,
                content_hash: None,
                ..
            } if id == &format!("file-record-{}", hex::encode(record_id.as_bytes()))
        ));
        clear_launch_env();
    }

    /// Three formats, three owning packages. Asserted against the
    /// command the compositor executed and the events it queued, not
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

    /// One instance, one launch — asserted against the tree the
    /// compositor paints and the commands it ran.
    #[test]
    #[serial_test::serial(process_env)]
    fn opening_an_open_document_focuses_it_instead_of_launching_again() {
        const SURFACE: u64 = 4242;
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

        let adopted = {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            crate::compositor::tab_binding::adopt_toplevel(
                &mut tree,
                SURFACE,
                Some("word-processor".to_string()),
            )
        };
        // Focus something else first, so "it is focused" is a claim about this open.
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
                    DocumentTarget::App { app, .. } => *app,
                    other => panic!("expected an owning app, got {other:?}"),
                },
                tab: Some(adopted),
            },
            "the second open focuses the window the first one produced"
        );

        // Read out of the tree, not out of the return value.
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

    /// Driven through the explorer's own `open_entry` and the
    /// production route ([`crate::shell::install_document_opener`]) —
    /// the wiring is under test, not a direct call.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_file_explorer_opens_a_document_in_the_app_that_owns_it() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        install(&apps, "dev.qos.word-processor", "Word Processor", &["odt"]);
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
        explorer.navigate(files);
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

        {
            use crate::shell::state::{Effect, Scope, Subject};
            let st = chrome.borrow();
            let tree = st.tree();
            let ws = tree.active_workspace().unwrap();
            let want = Subject::app("dev.qos.word-processor");
            let scopes: Vec<Scope> = ws
                .capabilities
                .iter()
                .filter(|c| c.subject == want && c.effect == Effect::Grant)
                .map(|c| c.scope.clone())
                .collect();
            drop(tree);
            assert_eq!(scopes, vec![Scope::FsPath(doc.display().to_string())]);
        }

        file_explorer::files_app::clear_document_opener();
        clear_launch_env();
    }

    /// A type nothing claims is refused in words the explorer shows.
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

    /// A closed window means the next activation launches for real.
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
        let commands = launched_commands(&log, 2);
        assert_eq!(
            commands.len(),
            2,
            "one launch per activation: {commands:#?}"
        );

        clear_launch_env();
    }

    /// Resolution must land on the declaring package, not a name the
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
        assert!(matches!(
            resolve(Path::new("/w/NOTES.MD")),
            DocumentTarget::App { .. }
        ));

        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        launcher::invalidate_app_cache();
    }

    /// A format with nothing installed is its own answer — different
    /// from "this file is not a document".
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
    /// path, addressed to exactly that package — or the app launches
    /// into a sandbox the file is not in. The launch itself fails here
    /// (no Wayland socket in a unit test), which is the point: the
    /// grant is recorded *before* the launch, so it's observable either way.
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
            message.contains("no app on this desktop opens"),
            "the message must say nothing can open it: {message}"
        );
        // Same rule as the launcher's: the user installed nothing, so
        // the sentence may not tell them something is not installed.
        assert!(
            !message.contains("install"),
            "user-facing text must not talk about installing: {message}"
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

    #[test]
    #[serial_test::serial(process_env)]
    fn remote_file_reference_resolves_a_projected_record_without_source_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("hydrated-on-read.md");
        std::fs::write(&path, b"hydrated content").unwrap();
        let record = [0x71; 16];
        let content = [0x42; 32];
        let thumbnail = b"projected thumbnail";
        xattr::set(&path, "user.qos.record-id", &record).unwrap();
        xattr::set(&path, "user.qos.content-hash", &content).unwrap();
        xattr::set(&path, "user.qos.thumb", thumbnail).unwrap();
        let uuid = uuid::Uuid::from_bytes(record);
        let exact = serde_json::json!({
            "schema_version": 2,
            "revision": "a".repeat(64),
            "record": {
                "record_id": uuid,
                "path": "/hydrated-on-read.md",
                "is_dir": false,
            },
        });
        xattr::set(
            root.path(),
            format!(
                "{}{}",
                file_explorer::model::RECORD_INDEX_BY_ID_XATTR_PREFIX,
                uuid
            ),
            &serde_json::to_vec(&exact).unwrap(),
        )
        .unwrap();
        let reference = crate::shell::personal_continuity::LaunchIntent::file_reference(
            format!("file-record-{}", hex::encode(record)),
            Some(content),
            Some(*blake3::hash(thumbnail).as_bytes()),
        )
        .unwrap();

        // A change after capture must not make the record unopenable: the
        // FileRecordId, rather than stale content/preview hashes, routes it.
        xattr::set(&path, "user.qos.content-hash", &[0x43; 32]).unwrap();
        xattr::set(&path, "user.qos.thumb", b"fresh preview").unwrap();

        let previous = std::env::var_os("QOS_WORKSPACE_ROOTS");
        // SAFETY: guarded by the process_env serial group.
        unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", root.path()) };
        let resolved = open_continuity_file_reference(&reference)
            .expect("the mounted Files record catalog resolves and hydrates the remote reference");
        assert_eq!(resolved, path);
        assert_eq!(std::fs::read(resolved).unwrap(), b"hydrated content");
        assert!(
            !serde_json::to_string(&reference)
                .unwrap()
                .contains(root.path().to_str().unwrap())
        );
        install(root.path(), "dev.qos.text-editor", "Text Editor", &["md"]);
        // SAFETY: guarded by the process_env serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", root.path()) };
        let log = install_recording_launcher(root.path());
        reset_session_state();
        let chrome = chrome_with_workspace();
        {
            let state = chrome.borrow_mut();
            state
                .tree_mut()
                .continuation_file_references
                .insert(0x77, reference);
        }
        assert!(activate_continuity_file_tab(&chrome, 0x77));
        let launch = launched_commands(&log, 1);
        assert!(
            launch
                .iter()
                .any(|command| command.contains("hydrated-on-read.md")),
            "activation must use the normal document launcher: {launch:?}"
        );
        clear_launch_env();
        // SAFETY: restore the process-global environment under the same lock.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("QOS_WORKSPACE_ROOTS", value),
                None => std::env::remove_var("QOS_WORKSPACE_ROOTS"),
            }
        }
    }
}
