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
//! `OPEN_DOCUMENTS` tracks a file's identity
//! ([`ui::record::RecordId::for_file`]) against the window each launch
//! produced, and a record whose window has since closed is not "open".

use std::io::Read;
use std::path::{Path, PathBuf};

use ui::record::RecordId;

use crate::shell::chrome_state::Chrome;
use crate::shell::launcher::{self, InstalledApp, LaunchError};
use crate::shell::state::{GroupId, StripId, TabId, TileContent, WorkspaceId, WorkspaceTree};
use crate::shell::tab_binding::Placement;
use tilo_lock::MutexRecover as _;

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
    /// A host desktop entry opens it: nothing Tilo packaged claims the type,
    /// or the person chose this host application.
    HostApp {
        app: Box<crate::shell::host_apps::HostApp>,
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
    /// Handed to an unconfined host application, which owns its own windows
    /// and file access.
    HostLaunched {
        app: crate::shell::host_apps::HostApp,
    },
}

impl Opened {
    /// The Tilo package that holds the document; `None` for a host application.
    pub fn app(&self) -> Option<&InstalledApp> {
        match self {
            Opened::Launched { app } | Opened::Focused { app, .. } => Some(app),
            Opened::HostLaunched { .. } => None,
        }
    }

    pub fn launched(&self) -> bool {
        matches!(self, Opened::Launched { .. } | Opened::HostLaunched { .. })
    }

    /// The typed result an open action answers with: what happened and
    /// which application holds the document now. `tab` is the focused tab
    /// when the document was already open.
    pub fn to_value(&self, path: &Path) -> serde_json::Value {
        match self {
            Opened::Launched { app } => serde_json::json!({
                "opened": "launched",
                "path": path,
                "app": app.package_id,
            }),
            Opened::Focused { app, tab } => serde_json::json!({
                "opened": "focused",
                "path": path,
                "app": app.package_id,
                "tab": tab,
            }),
            Opened::HostLaunched { app } => serde_json::json!({
                "opened": "host-launched",
                "path": path,
                "app": app.desktop_id,
            }),
        }
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
                    Some(ext) => crate::shell::i18n::t_args(
                        "doc-open-no-handler",
                        &[("ext", ext.as_str().into()), ("name", name.into())],
                    ),
                    None => crate::shell::i18n::t_args(
                        "doc-open-no-file-type",
                        &[("name", name.into())],
                    ),
                }
            }
            OpenError::Launch(e) => e.user_message(),
            OpenError::NoWorkspace => crate::shell::i18n::t("doc-open-no-workspace"),
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
    let editable_document = ext
        .as_deref()
        .is_some_and(tilo_doc_codecs::is_supported_ext);
    let mime = mime_guess::from_path(path).first_raw();
    let by_mime = mime
        .and_then(|mime| desktop_policy::ResourceKind::mime(mime).ok())
        .and_then(|resource| {
            launcher::handler_for_resource_with_policy(&resource, user_id, workspace_defaults)
        });
    let by_extension = ext
        .as_deref()
        .and_then(|ext| desktop_policy::ResourceKind::extension(ext).ok())
        .and_then(|resource| {
            launcher::package_for_resource_with_policy(&resource, user_id, workspace_defaults)
        });
    let chosen = |source: desktop_policy::defaults::SelectionSource| {
        !matches!(
            source,
            desktop_policy::defaults::SelectionSource::SystemDefault
                | desktop_policy::defaults::SelectionSource::DeterministicFallback
        )
    };
    // A choice (personal or workspace) wins, by type before extension.
    // Without one, a host application only wins when no package claims the
    // extension; otherwise Tilo's own app opens it.
    let handler = match (by_mime, by_extension) {
        (Some((handler, source)), _) if chosen(source) => Some(handler),
        (_, Some((app, source))) if chosen(source) => Some(launcher::ResourceHandler::Package(app)),
        (Some((launcher::ResourceHandler::Host(host), _)), by_extension) => Some(
            by_extension.map_or(launcher::ResourceHandler::Host(host), |(app, _)| {
                launcher::ResourceHandler::Package(app)
            }),
        ),
        (Some((handler, _)), _) => Some(handler),
        (None, by_extension) => {
            by_extension.map(|(app, _)| launcher::ResourceHandler::Package(app))
        }
    };
    match handler {
        Some(launcher::ResourceHandler::Package(app)) => DocumentTarget::App {
            app: Box::new(app),
            editable_document,
        },
        Some(launcher::ResourceHandler::Host(app)) => {
            DocumentTarget::HostApp { app: Box::new(app) }
        }
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

/// Open a document at an explicit shell placement. Drag-and-drop callers use
/// this rather than smuggling a group id through [`open_in_group`], because a
/// drop can name an exact tab gap or an exact inter-window seam. The placement
/// is recorded before launch and consumed when the app's toplevel arrives.
///
/// # Errors
///
/// Same conditions as [`open_in_workspace`].
pub fn open_at(path: &Path, chrome: &Chrome, placement: Placement) -> Result<Opened, OpenError> {
    open(path, chrome, Some(placement))
}

/// [`open_in_workspace`] for a caller that already knows the target
/// group — a drop target is the user's own instruction rather than a
/// placement rule. Every other open asks `placement_for_document_open`.
///
/// # Errors
///
/// Same conditions as [`open_in_workspace`].
pub fn open_in_group(path: &Path, chrome: &Chrome, group: GroupId) -> Result<Opened, OpenError> {
    open_at(path, chrome, Placement::TabIn(group))
}

fn open(path: &Path, chrome: &Chrome, placement: Option<Placement>) -> Result<Opened, OpenError> {
    let opened = open_resolved(path, chrome, placement)?;
    // Host applications' file choosers list what the person opened here.
    crate::shell::recent_files::note_opened(path);
    Ok(opened)
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "guard's only use is the borrow passed into resolve_for, released \
              immediately after; nothing to tighten without cloning workspace policy"
)]
fn open_resolved(
    path: &Path,
    chrome: &Chrome,
    placement: Option<Placement>,
) -> Result<Opened, OpenError> {
    // The guard feeds `resolve_for`'s `workspace_defaults` borrow directly
    // and the block ends the moment that call returns; cloning the policy
    // map out just to drop the guard a statement earlier would cost more
    // than the (synchronous, non-blocking) call it's guarding against.
    let target = {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace();
        resolve_for(
            path,
            &state.local_user_id,
            workspace.map(|ws| &ws.default_apps),
        )
    };
    let app = match target {
        DocumentTarget::App { app, .. } => *app,
        DocumentTarget::HostApp { app } => return open_with_host(&app, path, chrome, placement),
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
    open_package(app, path, chrome, placement)
}

/// Open `path` in the Tilo package `app`, after re-checking that it still
/// claims the file's type: focus an existing window on the document, else
/// grant the read and launch.
fn open_package(
    app: InstalledApp,
    path: &Path,
    chrome: &Chrome,
    placement: Option<Placement>,
) -> Result<Opened, OpenError> {
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
            if let Some(placement) = placement {
                let moved = {
                    let state = chrome.borrow_mut();
                    let mut tree = state.tree_mut();
                    crate::shell::tab_binding::relocate_tab(&mut tree, tab.tab, placement)
                };
                if moved {
                    chrome.borrow_mut().bump();
                } else {
                    focus(chrome, tab);
                }
            } else {
                focus(chrome, tab);
            }
            return Ok(Opened::Focused {
                app,
                tab: Some(tab.tab),
            });
        }
        Some(Existing::Starting) => {
            return Ok(Opened::Focused { app, tab: None });
        }
        None => {}
    }

    // Must precede launch: `queue_document_in_workspace` publishes the
    // capability list as it stands at call time.
    grant_read_for_open(&app.package_id, path, chrome)?;

    if let Some(p) = placement.or_else(|| placement_for_document_open(chrome)) {
        launcher::set_pending_placement(&app.package_id, p);
    }

    // Before the launch: the window can arrive and claim the note as
    // soon as the launch thread starts.
    note_opening(&record, path, &app.package_id);

    // Authorship is part of opening: the capability file is published from
    // the workspace this launch sees, and a workspace with no author grant
    // starts the editor as a viewer.
    let workspace = {
        let st = chrome.borrow();
        let mut tree = st.tree_mut();
        let workspace = tree.workspace_after_opening_app(&app.package_id);
        // The document's new read grant must reach the accepted-policy
        // publication before the launch gate checks its immutable snapshot.
        // Queue here; the launch worker waits without blocking the frame.
        tree.queue_save();
        workspace
    };
    // The launch's file work (a synced capability file, the identity
    // registry, the authority binding) runs on the preflight worker: this is
    // the compositor thread, and a click must not wait on the disk. A refusal
    // arrives as a launch event, after the stale note below is dropped.
    let Some(workspace) = workspace else {
        forget_document(&record);
        return Err(OpenError::NoWorkspace);
    };
    let package_id = app.package_id.clone();
    let refused_record = record.clone();
    let launched = launcher::queue_document_in_workspace(
        app.clone(),
        &workspace,
        path.to_path_buf(),
        move || {
            // Otherwise the stale note would make the next attempt believe
            // this document is already opening and refuse to try again.
            forget_document(&refused_record);
            launcher::clear_pending_placement(&package_id);
        },
    );
    match launched {
        Ok(()) => Ok(Opened::Launched { app }),
        Err(e) => {
            forget_document(&record);
            Err(OpenError::Launch(e))
        }
    }
}

/// Hand `path` to a host application. It runs unconfined with the person's
/// own files, so there is no read grant to record and no sandbox to open in;
/// the workspace only attributes its portal requests.
fn open_with_host(
    app: &crate::shell::host_apps::HostApp,
    path: &Path,
    chrome: &Chrome,
    placement: Option<Placement>,
) -> Result<Opened, OpenError> {
    // Re-read the catalogue: the resolution may be seconds old.
    let Some(app) = crate::shell::host_apps::find(&app.desktop_id) else {
        return Err(OpenError::NoHandler {
            path: path.to_path_buf(),
            ext: extension_of(path),
        });
    };
    if let Some(placement) = placement.or_else(|| placement_for_document_open(chrome)) {
        crate::shell::host_apps::note_placement(&app, placement);
    }
    let portal_workspace_id = {
        let state = chrome.borrow();
        let tree = state.tree();
        tree.desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string())
    };
    crate::shell::host_apps::launch_resolved_with_activation(
        app,
        Some(path),
        portal_workspace_id,
        None,
    )
    .map(|app| Opened::HostLaunched { app })
    .map_err(OpenError::Launch)
}

/// One application offered under "Open With" for a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenWithChoice {
    /// A Tilo package id, or a host entry as `desktop_policy::host_app_id`.
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// The resource kinds a file is resolved by: its MIME type, then its extension.
fn resource_kinds(path: &Path) -> Vec<desktop_policy::ResourceKind> {
    mime_guess::from_path(path)
        .first_raw()
        .and_then(|mime| desktop_policy::ResourceKind::mime(mime).ok())
        .into_iter()
        .chain(
            extension_of(path).and_then(|ext| desktop_policy::ResourceKind::extension(&ext).ok()),
        )
        .collect()
}

/// How a file's type reads in "Always use for …".
pub fn type_label(path: &Path) -> String {
    resource_kinds(path)
        .iter()
        .rev()
        .map(desktop_policy::ResourceKind::display_label)
        .next()
        .unwrap_or_else(|| "this file type".to_string())
}

/// Every Tilo package and host application that claims `path`'s type,
/// the one activation would use first, then packages, then by name.
pub fn open_with_choices(
    path: &Path,
    user_id: &str,
    workspace_defaults: Option<&desktop_policy::WorkspaceDefaults>,
) -> Vec<OpenWithChoice> {
    if path.is_dir() {
        return Vec::new();
    }
    let default_id = match resolve_for(path, user_id, workspace_defaults) {
        DocumentTarget::App { app, .. } => Some(app.package_id),
        DocumentTarget::HostApp { app } => Some(desktop_policy::host_app_id(&app.desktop_id)),
        _ => None,
    };
    let kinds = resource_kinds(path);
    let mut choices: Vec<(bool, OpenWithChoice)> = launcher::resource_candidates()
        .into_iter()
        .filter(|candidate| kinds.iter().any(|kind| candidate.handles(kind)))
        .map(|candidate| {
            let host = candidate.is_host();
            (
                host,
                OpenWithChoice {
                    is_default: default_id.as_deref() == Some(candidate.package_id.as_str()),
                    id: candidate.package_id,
                    name: candidate.display_name,
                },
            )
        })
        .collect();
    choices.sort_by(|(a_host, a), (b_host, b)| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a_host.cmp(b_host))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    choices.dedup_by(|(_, a), (_, b)| a.id == b.id);
    choices.into_iter().map(|(_, choice)| choice).collect()
}

/// Open `path` with exactly `app_id`, which must claim its type now. A Tilo
/// package goes through the same claim check, read grant and placement as
/// activation; a host application is launched with the file.
///
/// # Errors
///
/// [`OpenError::NoHandler`] when `app_id` is gone or does not claim the type,
/// else the same conditions as [`open_in_workspace`].
pub fn open_with_app(path: &Path, chrome: &Chrome, app_id: &str) -> Result<Opened, OpenError> {
    let no_handler = || OpenError::NoHandler {
        path: path.to_path_buf(),
        ext: extension_of(path),
    };
    let kinds = resource_kinds(path);
    let claims = launcher::resource_candidates().iter().any(|candidate| {
        candidate.package_id == app_id && kinds.iter().any(|kind| candidate.handles(kind))
    });
    if !claims {
        return Err(no_handler());
    }
    if let Some(desktop_id) = desktop_policy::host_desktop_id(app_id) {
        let app = crate::shell::host_apps::find(desktop_id).ok_or_else(no_handler)?;
        return open_with_host(&app, path, chrome, None);
    }
    let app = launcher::gui_apps()
        .into_iter()
        .find(|app| app.package_id == app_id)
        .ok_or_else(no_handler)?;
    open_package(app, path, chrome, None)
}

/// Make `app_id` the person's default for `path`'s type: by MIME type when
/// the app claims it, else by extension.
///
/// # Errors
///
/// A message naming the app when it claims neither, or the choice could not
/// be saved.
pub fn set_personal_default(path: &Path, app_id: &str) -> Result<(), String> {
    let candidates = launcher::resource_candidates();
    let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
    let mut personal = desktop_policy::PersonalDefaults::load();
    let mut last_error = None;
    for kind in resource_kinds(path) {
        match personal.set(kind, app_id.to_string(), &resolver) {
            Ok(()) => {
                personal.save().map_err(|error| error.to_string())?;
                launcher::publish_host_mimeapps();
                return Ok(());
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.map_or_else(
        || {
            format!(
                "{} has no file type to choose an application for",
                path.display()
            )
        },
        |error| error.to_string(),
    ))
}

/// A document opened elsewhere belongs *with* whatever asked for it —
/// a tab in the focused group rather than a new column.
fn placement_for_document_open(chrome: &Chrome) -> Option<Placement> {
    // The group whose "+" asked beats the focused one.
    let explicit = {
        let st = chrome.borrow();
        crate::shell::placement::explicit_group_placement(&st.tree(), st.open_target)
    };
    explicit.or_else(|| focused_group(chrome).map(Placement::TabIn))
}

/// `None` when there's no strip or group to be "beside" — adoption
/// bootstraps its own in that case.
fn focused_group(chrome: &Chrome) -> Option<crate::shell::state::GroupId> {
    let st = chrome.borrow();
    let tree = st.tree();
    let ws = tree.desktop.active_workspace()?;
    let strip_id = tree
        .desktop
        .active_strip
        .get(&ws.id)
        .copied()
        .or_else(|| ws.strips.first().map(|s| s.id))?;
    tree.desktop.active_group_for(strip_id).or_else(|| {
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
    use tilo_desktop::notifications::{Notification, NotificationSeverity};
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    );
    let title = crate::shell::i18n::t_args("doc-open-failed", &[("name", name.into())]);
    let mut n =
        Notification::new(title, detail.to_string()).with_severity(NotificationSeverity::Error);
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
pub(crate) struct OpenDocument {
    record: RecordId,
    /// Kept only in the local process so continuation can read the FUSE
    /// record projection. It is never serialized into a checkpoint.
    path: PathBuf,
    /// Correlates the launch with the window it produces.
    package_id: String,
    /// The wayland surface that arrived, once one has. `None` while starting.
    surface: Option<u64>,
}

/// The documents open in *this session* (not one workspace or one widget
/// tree): `session`'s list, `session` being [`launcher::LaunchSession::current`].
/// A poisoned lock is recovered: a panic while holding it doesn't corrupt the
/// list, so it is treated as healthy rather than wedging every later caller.
fn lock_open_documents(
    session: &launcher::LaunchSession,
) -> std::sync::MutexGuard<'_, Vec<OpenDocument>> {
    session.open_documents.lock_recover()
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
    let session = launcher::LaunchSession::current();
    let mut open = lock_open_documents(&session);
    open.retain(|d| &d.record != record);
    open.push(OpenDocument {
        record: record.clone(),
        path: path.to_path_buf(),
        package_id: package_id.to_string(),
        surface: None,
    });
}

fn forget_document(record: &RecordId) {
    lock_open_documents(&launcher::LaunchSession::current()).retain(|d| &d.record != record);
}

/// A launch of `package_id` ended, or ran out of time, without a window: the
/// documents it was opening are not open, so opening one again launches
/// again instead of waiting on a window that will never come.
pub(crate) fn forget_unpresented_documents(session: &launcher::LaunchSession, package_id: &str) {
    lock_open_documents(session).retain(|d| d.surface.is_some() || d.package_id != package_id);
}

pub fn clear_open_documents() {
    lock_open_documents(&launcher::LaunchSession::current()).clear();
}

/// Return a privacy-safe continuation reference for a document surface.  The
/// ordinary `RecordId::for_file` embeds an absolute local path and must never
/// leave this device; continuity therefore carries only a stable hash-derived
/// reference (and the same hash as its content/reference fingerprint).
pub fn continuation_file_reference_for_surface(
    surface_id: u64,
) -> Option<crate::shell::personal_continuity::LaunchIntent> {
    let session = launcher::LaunchSession::current();
    let open = lock_open_documents(&session);
    let path = open
        .iter()
        .find(|document| document.surface == Some(surface_id))?
        .path
        .clone();
    drop(open);
    let record_id = xattr::get(&path, fs_protocol::RECORD_ID_XATTR)
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
    let content_hash = xattr::get(&path, fs_protocol::CONTENT_HASH_XATTR)
        .ok()
        .flatten()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
    // FUSE exposes preview bytes, never a path. Persist only their hash so
    // the receiver can request/cache its own projected thumbnail.
    let thumbnail_hash = xattr::get(&path, fs_protocol::THUMB_XATTR)
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
    .unwrap_or_else(|| crate::shell::i18n::t("doc-open-unnamed-file"))
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
    let session = launcher::LaunchSession::current();
    let mut open = lock_open_documents(&session);
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
        let session = launcher::LaunchSession::current();
        let open = lock_open_documents(&session);
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
    for w in &tree.desktop.workspaces {
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
pub(crate) fn grant_read_for_open(
    package_id: &str,
    path: &Path,
    chrome: &Chrome,
) -> Result<(), OpenError> {
    let ws_id = {
        let st = chrome.borrow();
        let tree = st.tree();
        if tree.desktop.active_workspace().is_none() {
            return Err(OpenError::NoWorkspace);
        }
        tree.desktop.active
    };
    let store = {
        let st = chrome.borrow();
        crate::portal::GrantStoreHandle::new(st.tree_handle())
    };
    let caller = crate::portal::PortalCaller::new(ws_id, package_id.to_string());
    if store.filesystem_path_granted(&caller, path) {
        return Ok(());
    }
    let granted = grant_document_read(&mut chrome.borrow().tree_mut(), ws_id, package_id, path)
        .ok_or(OpenError::NoWorkspace)?;
    chrome.borrow_mut().bump();
    if granted {
        Ok(())
    } else {
        // Only this device's own identity can vouch for the read; without it
        // the app would start and lose the document a moment later.
        Err(OpenError::Launch(
            launcher::LaunchError::IdentityUnavailable {
                detail: "this device's identity cannot vouch for the document's read".into(),
            },
        ))
    }
}

/// Grant `package_id` a session read of `path` in workspace `ws_id`, with
/// this device's provenance for it. `None` when the workspace is gone;
/// `false` when the read cannot be vouched for and was not granted.
fn grant_document_read(
    tree: &mut crate::shell::state::WorkspaceTree,
    ws_id: crate::shell::state::WorkspaceId,
    package_id: &str,
    path: &Path,
) -> Option<bool> {
    use crate::shell::state::{Capability, FsMode, GRANT_SESSION};

    let ws = tree.desktop.workspaces.iter_mut().find(|w| w.id == ws_id)?;
    let ids = ws.grant_app(
        package_id,
        &Capability::Filesystem {
            path: path.to_path_buf(),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        },
    );
    // A room a native owner governs is re-projected from its signed state,
    // which never holds this device-local read: without its provenance the
    // next projection withdraws the grant and the sandbox of the app just
    // handed the document is interrupted.
    let remembered = tree
        .native_runtime
        .remember_document_read(ws, &ids, package_id);
    if !remembered && tree.native_authority.requires(ws.room) {
        ws.sandbox
            .retain(|capability| !ids.contains(&capability.id));
        return Some(false);
    }
    // Mirrored into the room document so a restart reloads it.
    tree.doc_write_policy(
        ws_id,
        &crate::shell::state::PolicyEdit {
            sandbox_granted: ids,
            ..crate::shell::state::PolicyEdit::default()
        },
    );
    Some(true)
}

#[cfg(test)]
mod tests {
    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use std::fmt::Write as _;

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
            crate::shell::test_support::canonical_manifest(
                &format!(
                    "id = \"{package_id}\"\nname = \"{name}\"\nversion = \"1.0.0\"\nkind = \"app\""
                ),
                &format!(
                    "[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n\
                     [providers]\nmime = []\next = [{ext_list}]\n"
                ),
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
        let handle: WorkspaceTreeHandle =
            crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::bootstrap_demo());
        // Match session startup: launch gates wait for the tree's accepted
        // policy revision before judging a document's newly granted read.
        launcher::action_authority::follow_tree(&handle);
        ChromeState::new_with_handle(handle)
    }

    /// A stand-in `tilo-launcher` that records its argv, wired in via
    /// `TILO_LAUNCHER_BIN` so the log holds what production really ran.
    fn install_recording_launcher(dir: &Path, env: &mut tilo_test_env::ScopedEnv) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("launched.log");
        let bin = dir.join("tilo-launcher");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        env.set("TILO_LAUNCHER_BIN", &bin)
            .set("XDG_RUNTIME_DIR", dir);
        launcher::set_test_execution_program(Some(bin));
        launcher::set_wayland_socket_path(Some(dir.join("wayland-0")));
        log
    }

    /// Waits for at least `want` — a launch runs on its own thread, not
    /// observable the instant `open` returns.
    fn launched_commands(log: &Path, want: usize) -> Vec<String> {
        let deadline = std::time::Instant::now()
            .checked_add(HANG_BOUND)
            .expect("the hang bound fits in an Instant");
        while std::time::Instant::now() < deadline {
            if let Ok(s) = std::fs::read_to_string(log) {
                let lines: Vec<String> = s.lines().map(str::to_string).collect();
                if lines.len() >= want {
                    return lines;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let got = std::fs::read_to_string(log).unwrap_or_default();
        let events = launcher::drain_events();
        panic!(
            "expected {want} launch(es); the launcher recorded:\n{got}\nlaunch events: {events:?}"
        );
    }

    /// Keep the machine's own desktop entries and `mimeapps.list` defaults
    /// out: a host with an office suite installed would otherwise answer for
    /// `.docx`, and a video player for `.mkv`.
    fn without_host_apps(dir: &Path, env: &mut tilo_test_env::ScopedEnv) {
        let data = dir.join("host-data");
        let config = dir.join("host-config");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        env.set("XDG_DATA_HOME", &data)
            .set("XDG_DATA_DIRS", &data)
            .set("XDG_CONFIG_HOME", &config)
            .set("XDG_CONFIG_DIRS", &config);
        crate::shell::host_apps::invalidate_cache();
    }

    fn reset_session_state() {
        launcher::invalidate_app_cache();
        launcher::clear_pending_placements();
        let _ = launcher::drain_events();
        clear_open_documents();
    }

    /// The environment itself is restored by each test's `ScopedEnv`.
    fn clear_launch_env() {
        launcher::set_test_execution_program(None);
        launcher::set_wayland_socket_path(None);
        reset_session_state();
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn continuations_and_remote_references_resolve_without_source_paths() {
        fn continuation_uses_root_catalog_when_file_xattr_is_unavailable() {
            let root = tempfile::tempdir().unwrap();
            let document = root.path().join("deep/report.pdf");
            std::fs::create_dir_all(document.parent().unwrap()).unwrap();
            std::fs::write(&document, b"report").unwrap();
            let record_id = uuid::Uuid::from_u128(0x5151);
            // The workspace root serves its record index as a mount without
            // an ACL does: a v2 manifest and its one page.
            let revision = "c".repeat(64);
            let v2 = fs_protocol::projection::RECORD_INDEX_V2_XATTR;
            xattr::set(
                root.path(),
                v2,
                &serde_json::to_vec(&serde_json::json!({
                    "schema_version": fs_protocol::projection::RECORD_INDEX_V2_VERSION,
                    "revision": revision,
                    "records": 1,
                    "page_entries": fs_protocol::projection::RECORD_INDEX_PAGE_ENTRIES,
                }))
                .unwrap(),
            )
            .unwrap();
            xattr::set(
                root.path(),
                format!("{v2}.{revision}.0"),
                &serde_json::to_vec(&serde_json::json!({
                    "schema_version": fs_protocol::projection::RECORD_INDEX_V2_VERSION,
                    "revision": revision,
                    "offset": 0,
                    "next_offset": null,
                    "records": [{
                        "record_id": record_id,
                        "path": "/deep/report.pdf",
                        "is_dir": false
                    }],
                }))
                .unwrap(),
            )
            .unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ROOTS", root.path());
            clear_open_documents();
            note_opening(
                &RecordId::for_file(&document),
                &document,
                "so.tilo.pdf-viewer",
            );
            claim_surface_for_document("so.tilo.pdf-viewer", 77);

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

        fn remote_file_reference_resolves_a_projected_record_without_source_path() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("hydrated-on-read.md");
            std::fs::write(&path, b"hydrated content").unwrap();
            let record = [0x71; 16];
            let content = [0x42; 32];
            let thumbnail = b"projected thumbnail";
            xattr::set(&path, fs_protocol::RECORD_ID_XATTR, &record).unwrap();
            xattr::set(&path, fs_protocol::CONTENT_HASH_XATTR, &content).unwrap();
            xattr::set(&path, fs_protocol::THUMB_XATTR, thumbnail).unwrap();
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
                    fs_protocol::projection::RECORD_INDEX_BY_ID_XATTR_PREFIX,
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
            xattr::set(&path, fs_protocol::CONTENT_HASH_XATTR, &[0x43; 32]).unwrap();
            xattr::set(&path, fs_protocol::THUMB_XATTR, b"fresh preview").unwrap();

            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ROOTS", root.path());
            let resolved = open_continuity_file_reference(&reference).expect(
                "the mounted Files record catalog resolves and hydrates the remote reference",
            );
            assert_eq!(resolved, path);
            assert_eq!(std::fs::read(resolved).unwrap(), b"hydrated content");
            assert!(
                !serde_json::to_string(&reference)
                    .unwrap()
                    .contains(root.path().to_str().unwrap())
            );
            install(root.path(), "so.tilo.text-editor", "Text Editor", &["md"]);
            env.set("TILO_APPS_DATA_DIR", root.path());
            let log = install_recording_launcher(root.path(), &mut env);
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
        }

        continuation_uses_root_catalog_when_file_xattr_is_unavailable();
        remote_file_reference_resolves_a_projected_record_without_source_path();
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn documents_open_in_the_declaring_package_or_refuse_with_a_reason() {
        /// Three formats, three owning packages. Asserted against the
        /// command the compositor executed and the events it queued, not
        /// what `open_in_workspace` handed back.
        fn each_format_opens_in_the_package_that_declares_it() {
            let tmp = tempfile::tempdir().unwrap();
            install(
                tmp.path(),
                "so.tilo.word-processor",
                "Word Processor",
                &["odt"],
            );
            install(tmp.path(), "so.tilo.pdf-viewer", "PDF Viewer", &["pdf"]);
            install(tmp.path(), "so.tilo.text-editor", "Text Editor", &["md"]);
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            let log = install_recording_launcher(tmp.path(), &mut env);
            reset_session_state();

            let chrome = chrome_with_workspace();
            let cases = [
                ("report.odt", "so.tilo.word-processor"),
                ("manual.pdf", "so.tilo.pdf-viewer"),
                ("notes.md", "so.tilo.text-editor"),
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

            // A child can write its recording log before the launch thread
            // publishes Started. Observe the production event queue separately;
            // seeing argv is not evidence that every event has arrived.
            let deadline = std::time::Instant::now() + HANG_BOUND;
            let mut started = Vec::new();
            loop {
                started.extend(
                    launcher::drain_events()
                        .into_iter()
                        .filter(|e| matches!(e.kind, launcher::LaunchEventKind::Started { .. }))
                        .map(|e| e.package_id),
                );
                if cases
                    .iter()
                    .all(|(_, package)| started.iter().any(|p| p == package))
                    || std::time::Instant::now() >= deadline
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            for (_, package) in cases {
                assert!(
                    started.iter().any(|p| p == package),
                    "the compositor did not record starting {package}: {started:?}"
                );
            }

            clear_launch_env();
        }

        /// Resolution must land on the declaring package, not a name the
        /// compositor made up.
        fn declared_extension_resolves_to_the_declaring_package() {
            let tmp = tempfile::tempdir().unwrap();
            install(
                tmp.path(),
                "so.tilo.word-processor",
                "Word Processor",
                &["odt", "md"],
            );
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            launcher::invalidate_app_cache();

            let target = resolve(Path::new("/w/report.odt"));
            match target {
                DocumentTarget::App {
                    app,
                    editable_document,
                } => {
                    assert_eq!(app.package_id, "so.tilo.word-processor");
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

            env.remove("TILO_APPS_DATA_DIR");
            launcher::invalidate_app_cache();
        }

        /// A format with nothing installed is its own answer — different
        /// from "this file is not a document".
        fn document_with_no_installed_handler_is_distinguishable() {
            let tmp = tempfile::tempdir().unwrap();
            install(tmp.path(), "so.tilo.calculator", "Calculator", &[]);
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            without_host_apps(tmp.path(), &mut env);
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

            env.remove("TILO_APPS_DATA_DIR");
            launcher::invalidate_app_cache();
        }

        /// Nothing installed claims `.zzz`, so the open is refused rather
        /// than silently launching something arbitrary.
        fn unclaimed_extension_refuses_with_a_reportable_reason() {
            let tmp = tempfile::tempdir().unwrap();
            install(tmp.path(), "so.tilo.calculator", "Calculator", &[]);
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            without_host_apps(tmp.path(), &mut env);
            launcher::invalidate_app_cache();

            let log = install_recording_launcher(tmp.path(), &mut env);
            reset_session_state();

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
                    .desktop
                    .active_workspace()
                    .unwrap()
                    .sandbox
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

        each_format_opens_in_the_package_that_declares_it();
        declared_extension_resolves_to_the_declaring_package();
        document_with_no_installed_handler_is_distinguishable();
        unclaimed_extension_refuses_with_a_reportable_reason();
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn open_documents_are_focused_not_relaunched_until_their_window_closes() {
        /// One instance, one launch — asserted against the tree the
        /// compositor paints and the commands it ran.
        fn opening_an_open_document_focuses_it_instead_of_launching_again() {
            const SURFACE: u64 = 4242;
            let tmp = tempfile::tempdir().unwrap();
            install(
                tmp.path(),
                "so.tilo.word-processor",
                "Word Processor",
                &["odt"],
            );
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            let log = install_recording_launcher(tmp.path(), &mut env);
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
                crate::shell::tab_binding::adopt_toplevel(
                    &mut tree,
                    SURFACE,
                    Some("word-processor".to_string()),
                )
            };
            // Focus something else first, so "it is focused" is a claim about this open.
            {
                let st = chrome.borrow();
                let mut tree = st.tree_mut();
                let strip = tree.desktop.workspaces[0].strips[0].id;
                let other = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
                let first_tab = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
                tree.desktop.set_active_group(strip, other);
                if let Some(g) = tree.desktop.workspaces[0].strips[0]
                    .window_groups
                    .first_mut()
                {
                    g.activate_tab(first_tab);
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
                .desktop
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
            assert_eq!(
                tree.desktop.active, addr.workspace,
                "its workspace is active"
            );
            assert_eq!(
                tree.desktop.active_strip.get(&addr.workspace).copied(),
                Some(addr.strip),
                "its strip is active"
            );
            assert_eq!(
                tree.desktop.active_group_for(addr.strip),
                Some(addr.group),
                "its group is active"
            );
            let group = tree.desktop.workspaces[0]
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

        /// A closed window means the next activation launches for real.
        fn a_closed_window_means_the_document_is_no_longer_open() {
            let tmp = tempfile::tempdir().unwrap();
            install(
                tmp.path(),
                "so.tilo.word-processor",
                "Word Processor",
                &["odt"],
            );
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            let log = install_recording_launcher(tmp.path(), &mut env);
            reset_session_state();

            let doc = tmp.path().join("report.odt");
            std::fs::write(&doc, b"x").unwrap();
            let chrome = chrome_with_workspace();
            open_in_workspace(&doc, &chrome).unwrap();
            launched_commands(&log, 1);
            {
                let st = chrome.borrow();
                let mut tree = st.tree_mut();
                crate::shell::tab_binding::adopt_toplevel(
                    &mut tree,
                    77,
                    Some("word-processor".to_string()),
                );
            }
            {
                let st = chrome.borrow();
                let mut tree = st.tree_mut();
                crate::shell::tab_binding::close_tab_for_destroyed_surface(&mut tree, 77);
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

        fn explicit_drop_placement_survives_launch_until_toplevel_adoption() {
            let tmp = tempfile::tempdir().unwrap();
            install(
                tmp.path(),
                "dev.tilo.word-processor",
                "Word Processor",
                &["odt"],
            );
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            let log = install_recording_launcher(tmp.path(), &mut env);
            reset_session_state();

            let chrome = chrome_with_workspace();
            let (strip_id, group_id, old_first) = {
                let state = chrome.borrow();
                let tree = state.tree();
                let strip = &tree.desktop.workspaces[0].strips[0];
                let ids = (
                    strip.id,
                    strip.window_groups[0].id,
                    strip.window_groups[0].tabs[0].id,
                );
                drop(tree);
                drop(state);
                ids
            };

            let first = tmp.path().join("left.odt");
            std::fs::write(&first, b"left").unwrap();
            assert!(
                open_at(&first, &chrome, Placement::TabAt(group_id, 0))
                    .unwrap()
                    .launched()
            );
            let _ = launched_commands(&log, 1);
            let left_tab = {
                let state = chrome.borrow();
                crate::shell::tab_binding::adopt_toplevel(
                    &mut state.tree_mut(),
                    71,
                    Some("word-processor".into()),
                )
            };
            {
                let state = chrome.borrow();
                let tree = state.tree();
                let group = &tree.desktop.workspaces[0].strips[0].window_groups[0];
                assert_eq!(group.tabs[0].id, left_tab);
                assert_eq!(group.tabs[1].id, old_first);
                drop(tree);
                drop(state);
            }

            let second = tmp.path().join("seam.odt");
            std::fs::write(&second, b"seam").unwrap();
            assert!(
                open_at(&second, &chrome, Placement::GroupAt(strip_id, 0))
                    .unwrap()
                    .launched()
            );
            let _ = launched_commands(&log, 2);
            let seam_tab = {
                let state = chrome.borrow();
                crate::shell::tab_binding::adopt_toplevel(
                    &mut state.tree_mut(),
                    72,
                    Some("word-processor".into()),
                )
            };
            {
                let state = chrome.borrow();
                let tree = state.tree();
                assert_eq!(
                    tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id,
                    seam_tab
                );
                drop(tree);
                drop(state);
            }

            clear_launch_env();
        }

        opening_an_open_document_focuses_it_instead_of_launching_again();
        a_closed_window_means_the_document_is_no_longer_open();
        explicit_drop_placement_survives_launch_until_toplevel_adoption();
    }

    /// Files as the shell mounts it on a session without startup inputs of
    /// its own (a Linux session): its own component, starting at `files`,
    /// opening documents through the shell's opener.
    fn mounted_files(
        profile: &std::path::Path,
        files: &std::path::Path,
    ) -> ui::headless::FrameHarness {
        let mut apps =
            crate::shell::in_process::InProcessApps::at(&profile.join("profile"), "fixture-person")
                .unwrap();
        let startup = file_explorer::StartupInputs {
            initial_dir: files.to_path_buf(),
            workspace_roots: Vec::new(),
            record_roots: Vec::new(),
            remote_accounts: Vec::new(),
            trusted_humans: file_explorer::identity_directory::TrustedHumanDirectory::default(),
        };
        let component = apps
            .mount(
                file_explorer::definition(),
                "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc",
                9,
                ui::record::RecordId::room_default(),
                |context| context.lend(startup),
            )
            .unwrap();
        let mut harness = ui::headless::FrameHarness::from_component(
            component,
            (1000, 700),
            ui::application::ComponentRunnerInputs::from_native_process(),
        )
        .unwrap();
        harness.relayout();
        harness
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn file_explorer_opens_documents_in_their_owning_app_or_reports_none() {
        /// Driven through the explorer's own `open_entry` and the
        /// production route ([`crate::shell::install_document_opener`]) —
        /// the wiring is under test, not a direct call.
        fn the_file_explorer_opens_a_document_in_the_app_that_owns_it() {
            let tmp = tempfile::tempdir().unwrap();
            let apps = tmp.path().join("apps");
            std::fs::create_dir_all(&apps).unwrap();
            install(&apps, "so.tilo.word-processor", "Word Processor", &["odt"]);
            let files = tmp.path().join("files");
            std::fs::create_dir_all(&files).unwrap();
            let doc = files.join("agenda.odt");
            std::fs::write(&doc, b"x").unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", &apps);
            let log = install_recording_launcher(tmp.path(), &mut env);
            reset_session_state();

            let chrome = chrome_with_workspace();
            crate::shell::install_document_opener(&chrome);

            let mut harness = mounted_files(tmp.path(), &files);
            let explorer =
                file_explorer::mounted_files_in(&mut harness).expect("the mounted Files");
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
                commands[0].contains("--app-id so.tilo.word-processor")
                    && commands[0].contains(&format!("--open {}", doc.display())),
                "the explorer's activation had to reach the owning app with the \
                 document: {}",
                commands[0]
            );

            {
                use crate::shell::state::{Effect, Scope, Subject};
                let st = chrome.borrow();
                let tree = st.tree();
                let ws = tree.desktop.active_workspace().unwrap();
                let want = Subject::app("so.tilo.word-processor");
                let scopes: Vec<Scope> = ws
                    .sandbox
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
        fn the_file_explorer_reports_a_type_nothing_opens() {
            let tmp = tempfile::tempdir().unwrap();
            let apps = tmp.path().join("apps");
            std::fs::create_dir_all(&apps).unwrap();
            install(&apps, "so.tilo.word-processor", "Word Processor", &["odt"]);
            let files = tmp.path().join("files");
            std::fs::create_dir_all(&files).unwrap();
            std::fs::write(files.join("archive.zzz"), b"x").unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", &apps);
            let log = install_recording_launcher(tmp.path(), &mut env);
            reset_session_state();

            let chrome = chrome_with_workspace();
            crate::shell::install_document_opener(&chrome);

            let mut harness = mounted_files(tmp.path(), &files);
            let explorer =
                file_explorer::mounted_files_in(&mut harness).expect("the mounted Files");
            explorer.navigate(files);
            let idx = explorer
                .displayed_entries()
                .iter()
                .position(|e| e.name == "archive.zzz")
                .expect("the explorer lists the file");
            let mut runner = file_explorer::files_app::SystemRunner;
            explorer.open_entry(idx, &mut runner).unwrap();

            // Files says what failed in its notice card (ef59750fad), not
            // the fading status bubble.
            let status = explorer
                .notice_line()
                .unwrap_or_else(|| panic!("the refusal must reach the user"));
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

        /// Opening a document must leave a read capability for exactly that
        /// path, addressed to exactly that package — or the app launches
        /// into a sandbox the file is not in. The launch itself fails here
        /// (no Wayland socket in a unit test), which is the point: the
        /// grant is recorded *before* the launch, so it's observable either way.
        fn opening_a_document_grants_read_for_that_path() {
            use crate::shell::state::{Effect, Scope, Subject};

            let tmp = tempfile::tempdir().unwrap();
            install(
                tmp.path(),
                "so.tilo.word-processor",
                "Word Processor",
                &["odt"],
            );
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            launcher::invalidate_app_cache();

            let doc_path = tmp.path().join("report.odt");
            std::fs::write(&doc_path, b"x").unwrap();
            let chrome = chrome_with_workspace();
            assert!(
                chrome
                    .borrow()
                    .tree()
                    .desktop
                    .active_workspace()
                    .unwrap()
                    .sandbox
                    .is_empty(),
                "fixture precondition: the workspace starts with no grants"
            );

            let _ = open_in_workspace(&doc_path, &chrome);

            let st = chrome.borrow();
            let tree = st.tree();
            let ws = tree.desktop.active_workspace().unwrap();
            let want_subject = Subject::app("so.tilo.word-processor");
            let granted: Vec<_> = ws
                .sandbox
                .iter()
                .filter(|c| c.subject == want_subject && c.effect == Effect::Grant)
                .collect();
            assert_eq!(
                granted.len(),
                1,
                "exactly one grant for the opening app: {:?}",
                ws.sandbox
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
                !ws.sandbox
                    .iter()
                    .any(|c| c.scope == Scope::FsPath(parent.clone())),
                "the open widened the grant to {parent}: {:?}",
                ws.sandbox
            );
            drop(tree);
            drop(st);

            env.remove("TILO_APPS_DATA_DIR");
            launcher::invalidate_app_cache();
        }

        the_file_explorer_opens_a_document_in_the_app_that_owns_it();
        the_file_explorer_reports_a_type_nothing_opens();
        opening_a_document_grants_read_for_that_path();
    }

    /// Files' "Open With" is the compositor's resolver: every package that
    /// claims the type, the activation default first; a pick launches exactly
    /// that package with the file; "Always" makes the next activation use it.
    #[test]
    #[serial_test::serial(process_env, launch_events)]
    fn open_with_lists_claiming_packages_launches_the_pick_and_remembers_always() {
        use file_explorer::open_with::OpenWithRoute as _;
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        install(&apps, "so.tilo.pdf-viewer", "PDF Viewer", &["pdf"]);
        install(&apps, "so.tilo.zine", "Zine", &["pdf"]);
        install(&apps, "so.tilo.text-editor", "Text Editor", &["md"]);
        let files = tmp.path().join("files");
        std::fs::create_dir_all(&files).unwrap();
        let doc = files.join("manual.pdf");
        std::fs::write(&doc, b"x").unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", &apps);
        env.set("XDG_CONFIG_HOME", &config);
        env.set("XDG_DATA_HOME", &data);
        env.set("XDG_DATA_DIRS", &data);
        env.set("XDG_CONFIG_DIRS", &config);
        let log = install_recording_launcher(tmp.path(), &mut env);
        reset_session_state();
        let chrome = chrome_with_workspace();

        let route = crate::shell::ShellOpenWithRoute { chrome };
        let candidates = route.candidates(&doc);
        let packages: Vec<(&str, bool)> = candidates
            .apps
            .iter()
            .filter(|app| desktop_policy::host_desktop_id(&app.id).is_none())
            .map(|app| (app.id.as_str(), app.is_default))
            .collect();
        assert_eq!(
            packages,
            vec![("so.tilo.pdf-viewer", true), ("so.tilo.zine", false)],
            "only claiming packages, the activation default first"
        );
        assert_eq!(candidates.apps[0].id, "so.tilo.pdf-viewer");
        assert_eq!(candidates.type_label, ".pdf files");

        route.open_with(&doc, "so.tilo.zine").unwrap();
        let commands = launched_commands(&log, 1);
        assert!(
            commands[0].contains("--app-id so.tilo.zine")
                && commands[0].contains(&format!("--open {}", doc.display())),
            "the pick launches with the file: {}",
            commands[0]
        );
        assert!(
            route.open_with(&doc, "so.tilo.text-editor").is_err(),
            "an app that does not claim the type is refused"
        );

        route.set_default(&doc, "so.tilo.zine").unwrap();
        assert!(matches!(
            resolve_for(&doc, "", None),
            DocumentTarget::App { ref app, .. } if app.package_id == "so.tilo.zine"
        ));
        assert!(
            route.candidates(&doc).apps[0].is_default
                && route.candidates(&doc).apps[0].id == "so.tilo.zine"
        );
        assert!(route.set_default(&doc, "so.tilo.text-editor").is_err());
        clear_launch_env();
    }

    /// Installs `app`'s own `apps/<app>/tilo-app.toml`, as an image does.
    fn install_shipped(dir: &Path, package_id: &str, app: &str) {
        let payload = dir.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        let manifest_path = payload.join("manifest.toml");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../apps")
                .join(app)
                .join("tilo-app.toml"),
            &manifest_path,
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

    #[test]
    #[serial_test::serial(process_env, launch_events)]
    fn a_video_opens_in_the_player_with_the_editor_offered_under_open_with() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        install_shipped(&apps, "so.tilo.video-editor", "video-editor");
        install_shipped(&apps, "so.tilo.video-player", "video-player");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", &apps);
        without_host_apps(tmp.path(), &mut env);
        let log = install_recording_launcher(tmp.path(), &mut env);
        reset_session_state();
        let clip = tmp.path().join("Holiday.mp4");
        std::fs::write(&clip, b"x").unwrap();
        let chrome = chrome_with_workspace();

        let opened = open_in_workspace(&clip, &chrome).unwrap();
        assert_eq!(
            opened.app().map(|app| app.package_id.as_str()),
            Some("so.tilo.video-player")
        );
        let commands = launched_commands(&log, 1);
        assert!(
            commands[0].contains("--app-id so.tilo.video-player")
                && commands[0].contains(&format!("--open {}", clip.display())),
            "the player is launched on the clip: {}",
            commands[0]
        );
        let choices: Vec<(String, bool)> = open_with_choices(&clip, "", None)
            .into_iter()
            .map(|choice| (choice.id, choice.is_default))
            .collect();
        assert_eq!(
            choices,
            [
                ("so.tilo.video-player".to_string(), true),
                ("so.tilo.video-editor".to_string(), false),
            ],
            "the editor stays one pick away"
        );
        clear_launch_env();
    }

    /// A source file opens in Code with the file handed to it, even where a
    /// host text editor claims its type (`.py` is `text/plain` to the type
    /// table); Markdown stays with Notes and plain text with the host editor.
    #[test]
    #[serial_test::serial(process_env, launch_events)]
    fn source_files_open_in_code_and_other_text_keeps_its_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        install_shipped(&apps, "so.tilo.code-editor", "code-editor");
        install_shipped(&apps, "so.tilo.notes", "notes");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", &apps);
        without_host_apps(tmp.path(), &mut env);
        let host_entries = tmp.path().join("host-data/applications");
        std::fs::create_dir_all(&host_entries).unwrap();
        std::fs::write(
            host_entries.join("org.gnome.TextEditor.desktop"),
            "[Desktop Entry]\nType=Application\nName=Text Editor\nExec=true %U\n\
             MimeType=text/plain;text/x-python;text/x-rust;application/x-sh;\n",
        )
        .unwrap();
        crate::shell::host_apps::invalidate_cache();
        let log = install_recording_launcher(tmp.path(), &mut env);
        reset_session_state();

        let file = |name: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, b"def greet():\n    pass\n").unwrap();
            path
        };
        for name in ["tool.py", "main.rs", "build.sh", "app.ts", "core.c"] {
            assert!(
                matches!(
                    resolve_for(&file(name), "", None),
                    DocumentTarget::App { ref app, .. } if app.package_id == "so.tilo.code-editor"
                ),
                "{name} opens in Code"
            );
        }
        assert!(matches!(
            resolve_for(&file("plan.md"), "", None),
            DocumentTarget::App { ref app, .. } if app.package_id == "so.tilo.notes"
        ));
        assert!(
            matches!(
                resolve_for(&file("readme.txt"), "", None),
                DocumentTarget::HostApp { ref app } if app.desktop_id.contains("TextEditor")
            ),
            "plain text stays with the person's text editor"
        );

        let script = file("tool.py");
        let chrome = chrome_with_workspace();
        let opened = open_in_workspace(&script, &chrome).unwrap();
        assert_eq!(
            opened.app().map(|app| app.package_id.as_str()),
            Some("so.tilo.code-editor")
        );
        let commands = launched_commands(&log, 1);
        assert!(
            commands[0].contains("--app-id so.tilo.code-editor")
                && commands[0].contains(&format!("--open {}", script.display())),
            "Code is launched with the file handed to it: {}",
            commands[0]
        );
        clear_launch_env();
    }

    /// An app that exits before showing a window (here: refusing its
    /// arguments) leaves nothing open, so the next open launches it again.
    #[test]
    #[serial_test::serial(process_env, launch_events)]
    fn a_launch_that_fails_before_its_window_does_not_block_the_next_open() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        install(
            tmp.path(),
            "so.tilo.word-processor",
            "Word Processor",
            &["odt"],
        );
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        let log = install_recording_launcher(tmp.path(), &mut env);
        let launcher_bin = tmp.path().join("tilo-launcher");
        std::fs::write(
            &launcher_bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
                 echo 'error: unknown argument' >&2\nexit 2\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&launcher_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        reset_session_state();
        let doc = tmp.path().join("report.odt");
        std::fs::write(&doc, b"x").unwrap();
        let chrome = chrome_with_workspace();

        assert!(open_in_workspace(&doc, &chrome).unwrap().launched());
        launched_commands(&log, 1);
        let deadline = std::time::Instant::now() + HANG_BOUND;
        while !launcher::drain_events().iter().any(|event| {
            event.package_id == "so.tilo.word-processor"
                && matches!(event.kind, launcher::LaunchEventKind::Failed { .. })
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "the failed launch was never reported"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let again = open_in_workspace(&doc, &chrome).unwrap();
        assert!(
            again.launched(),
            "the document is not open anywhere, so it launches again: {again:?}"
        );
        assert_eq!(launched_commands(&log, 2).len(), 2);
        clear_launch_env();
    }
}
