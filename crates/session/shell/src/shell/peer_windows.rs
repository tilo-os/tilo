//! Giving a peer's window a real window here.
//!
//! This OS replicates *documents*, not screens: a room carries state and
//! every member runs their own app against it. So a
//! [`TileContent::PeerWindow`] tab arriving in the room document is news
//! that the room has one open, and the honest local answer is to open
//! one too — with the app id and peer name it carries, checked against
//! the room owner's `Write` grant on `ResourceRef::App(id)` via
//! `admits`, and launched through [`crate::shell::launcher`], binding
//! into the existing tab. `PeerWindow::note` labels the card and marks
//! the decision made, so a refusal or launch is not re-run on every sync.

use crate::shell::launcher;
use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    GroupId, ResourceRef, RoomId, StripId, TabId, TileContent, Verb, Workspace, WorkspaceTree,
};
use crate::shell::tab_binding::Placement;
use tilo_desktop_model::MissingApp;

/// Shown while the local instance is starting.
pub(crate) const OPENING: &str = "Opening here…";
/// Said of a window whose app this machine does not have.
pub(crate) const NOT_INSTALLED: &str = "Not installed on this machine";

/// Why a window could not be opened here.
enum Refusal {
    /// The sentence the card shows.
    Said(String),
    /// The app is not installed here; the card says whether it can be.
    NotInstalled,
}

impl From<String> for Refusal {
    fn from(sentence: String) -> Self {
        Self::Said(sentence)
    }
}

/// Tabs whose card's install button was pressed, drained by [`examine`].
#[cfg(not(test))]
static INSTALL_REQUESTS: std::sync::Mutex<Vec<TabId>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    // Each test owns its card requests, just like the retry queue below.
    static TEST_INSTALL_REQUESTS: std::cell::RefCell<Vec<TabId>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn with_install_requests<R>(apply: impl FnOnce(&mut Vec<TabId>) -> R) -> R {
    #[cfg(test)]
    {
        TEST_INSTALL_REQUESTS.with(|requests| apply(&mut requests.borrow_mut()))
    }
    #[cfg(not(test))]
    {
        apply(
            &mut INSTALL_REQUESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

#[derive(PartialEq, Eq)]
struct RetryRequest {
    tab: TabId,
    app_id: String,
}

/// Explicit recovery requests from retained cards, drained on reconciliation.
#[cfg(not(test))]
static RETRY_REQUESTS: std::sync::Mutex<Vec<RetryRequest>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    // Each test owns a desktop session, like the launcher's placement registry.
    static TEST_RETRY_REQUESTS: std::cell::RefCell<Vec<RetryRequest>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn with_retry_requests<R>(apply: impl FnOnce(&mut Vec<RetryRequest>) -> R) -> R {
    #[cfg(test)]
    {
        TEST_RETRY_REQUESTS.with(|requests| apply(&mut requests.borrow_mut()))
    }
    #[cfg(not(test))]
    {
        apply(
            &mut RETRY_REQUESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

pub(crate) fn request_retry(tab: TabId, app_id: &str) {
    let request = RetryRequest {
        tab,
        app_id: app_id.to_owned(),
    };
    with_retry_requests(|requests| {
        if !requests.contains(&request) {
            requests.push(request);
        }
    });
}

/// Admit an explicit retry of this exact saved window. Reuse its tab and
/// record; a stale card, a changed app or an already accepted retry is inert.
pub(crate) fn retry_local_restore(
    tree: &mut WorkspaceTree,
    tab_id: TabId,
    expected_app: &str,
) -> bool {
    reset_local_restore(tree, tab_id, expected_app).unwrap_or(false)
}

/// Normal app activation also acknowledges an already queued/opening saved
/// window. It must not launch another window while this exact one is pending.
pub(crate) fn reopen_local_restore(
    tree: &mut WorkspaceTree,
    tab_id: TabId,
    expected_app: &str,
) -> bool {
    reset_local_restore(tree, tab_id, expected_app).is_some()
}

/// None is a stale/nonlocal target; Some says whether this exact local card
/// changed. Queued and opening cards are accepted without another reset.
fn reset_local_restore(
    tree: &mut WorkspaceTree,
    tab_id: TabId,
    expected_app: &str,
) -> Option<bool> {
    let changed = tree
        .desktop
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .flat_map(|group| &mut group.tabs)
        .find(|tab| tab.id == tab_id)
        .and_then(|tab| match &mut tab.tile {
            TileContent::LocalApp { app_id, note, .. } if app_id == expected_app => {
                let changed = !note.is_empty() && note != OPENING;
                if changed {
                    note.clear();
                }
                Some(changed)
            }
            _ => None,
        });
    if changed == Some(true) {
        tree.queue_save();
    }
    changed
}

fn start_requested_retries(tree: &mut WorkspaceTree) -> bool {
    let requests = with_retry_requests(std::mem::take);
    let mut changed = false;
    for request in requests {
        changed |= retry_local_restore(tree, request.tab, &request.app_id);
    }
    changed
}

/// An opening message belongs to this saved tab's launch, not to any window
/// of the same app. A retired/missing claim leaves a recoverable card rather
/// than a permanent promise or an automatic process loop.
fn settle_orphaned_local_restores(tree: &mut WorkspaceTree) -> bool {
    let live = launcher::pending_restore_tabs();
    let detail = crate::shell::i18n::t("shell-window-restore-interrupted");
    let mut changed = false;
    for workspace in &mut tree.desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    if let TileContent::LocalApp { note, .. } = &mut tab.tile
                        && note == OPENING
                        && !live.contains(&tab.id)
                    {
                        note.clone_from(&detail);
                        changed = true;
                    }
                }
            }
        }
    }
    if changed {
        tree.queue_save();
    }
    changed
}

/// The catalogue generation the cards' [`MissingApp`] answers were read at.
static MISSING_READ_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The person pressed "Install" on `tab`'s card: install its app and open it
/// in that tab, on the next [`examine`].
pub(crate) fn request_install(tab: TabId) {
    with_install_requests(|requests| {
        if !requests.contains(&tab) {
            requests.push(tab);
        }
    });
}

/// Whether a card's Install press waits for the next [`examine`]. Queued
/// outside the tree, so no tree revision announces it.
pub(crate) fn install_requested() -> bool {
    with_install_requests(|requests| !requests.is_empty())
}

/// Whether `catalogue` offers `app_id` to this device.
fn missing_app(app_id: &str, catalogue: &[crate::shell::catalogue::CatalogueApp]) -> MissingApp {
    catalogue
        .iter()
        .find(|offered| launcher::app_id_matches_package(app_id, &offered.package_id))
        .map_or(MissingApp::Unavailable, |offered| MissingApp::Installable {
            display_name: offered.display_name.clone(),
        })
}

/// One symbolic application window, lifted out of the tree so the decision
/// can be made without holding a borrow on it. `local` distinguishes a window
/// this machine persisted for session restore from a peer's room projection.
struct Pending {
    tab: TabId,
    app_id: String,
    /// Index of the owning workspace in `tree.workspaces`.
    workspace: usize,
    local: bool,
}

/// Examine every peer window this machine has not yet answered for, and
/// start a local instance of each one it may. Returns whether anything
/// was decided, so the caller can repaint only when needed.
///
/// At most one unanswered launch per app id, across reconciliation passes:
/// the launcher correlates each arriving window with its app's FIFO placement.
/// Wait for that claim, failure or expiry before starting a saved sibling.
pub fn examine(tree: &mut WorkspaceTree) -> bool {
    #[cfg(test)]
    EXAMINE_PASSES.with(|passes| passes.set(passes.get().saturating_add(1)));
    let released = release_brought_over(tree);
    let installing = start_requested_installs(tree);
    let reread = reread_missing_apps(tree);
    let retried = start_requested_retries(tree);
    let interrupted = settle_orphaned_local_restores(tree);
    let todo = pending(tree);
    if todo.is_empty() {
        return released || installing || reread || retried || interrupted;
    }
    let mut started: Vec<String> = Vec::new();
    let mut decided = false;
    for item in todo {
        if started.iter().any(|a| a.eq_ignore_ascii_case(&item.app_id))
            || launcher::has_unclaimed_launch(&item.app_id)
        {
            continue;
        }
        let Some(workspace) = tree.desktop.workspaces.get(item.workspace) else {
            continue;
        };
        // A same-identity continuation can discover the room before its
        // signed membership document arrives. Keep it pending so the next
        // reconciliation pass opens it as soon as authority is available.
        if !item.local && workspace.owner.is_none() {
            continue;
        }
        // A governed room's window opens the record its owner created for
        // it; wait until this device has admitted it.
        if !item.local && tree.requires_native_owner(workspace.room) {
            match prepare_hosted_record(tree, &item) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::debug!(tab = ?item.tab, app = %item.app_id, "governed window waits for its record");
                    continue;
                }
                Err(refusal) => {
                    tracing::debug!(tab = ?item.tab, app = %item.app_id, %refusal, "governed window cannot open");
                    if set_note(tree, item.tab, refusal) {
                        decided = true;
                    }
                    continue;
                }
            }
        }
        let (note, missing) = match open_for(tree, &item) {
            Ok(()) => {
                started.push(item.app_id.clone());
                (OPENING.to_string(), None)
            }
            Err(Refusal::Said(refusal)) => (refusal, None),
            Err(Refusal::NotInstalled) => (
                NOT_INSTALLED.to_string(),
                (!item.local)
                    .then(|| missing_app(&item.app_id, &crate::shell::catalogue::available_apps())),
            ),
        };
        if set_state(tree, item.tab, note, missing) {
            decided = true;
        }
    }
    decided || released || installing || reread || retried || interrupted
}

/// Answer every card whose app is missing again once the catalogue has
/// changed: a registry that came online may now offer it, or stop to.
fn reread_missing_apps(tree: &mut WorkspaceTree) -> bool {
    use std::sync::atomic::Ordering;
    let cards: Vec<(TabId, String, MissingApp)> = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|ws| &ws.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            TileContent::PeerWindow {
                app_id,
                missing: Some(missing),
                ..
            } => Some((tab.id, app_id.clone(), missing.clone())),
            _ => None,
        })
        .collect();
    if cards.is_empty() {
        return false;
    }
    // Reading also schedules a refresh of a cold or stale catalogue.
    let catalogue = crate::shell::catalogue::available_apps();
    let generation = crate::shell::catalogue::available_apps_generation();
    if MISSING_READ_AT.swap(generation, Ordering::Relaxed) == generation {
        return false;
    }
    let mut changed = false;
    for (tab, app_id, before) in cards {
        let now = missing_app(&app_id, &catalogue);
        if now != before {
            changed |= set_state(tree, tab, NOT_INSTALLED.to_string(), Some(now));
        }
    }
    changed
}

/// Install and open the app of every card whose install button was
/// pressed. The click is the consent; nothing installs without one.
fn start_requested_installs(tree: &mut WorkspaceTree) -> bool {
    let requests = with_install_requests(std::mem::take);
    let mut changed = false;
    for tab in requests {
        changed |= install_for(tree, tab);
    }
    changed
}

/// Install `tab`'s app, when its card offers that, so the new instance opens
/// in that exact tab the way a launch for a peer's window does.
fn install_for(tree: &mut WorkspaceTree, tab: TabId) -> bool {
    let found = tree
        .desktop
        .workspaces
        .iter()
        .enumerate()
        .find_map(|(workspace, ws)| {
            ws.strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|candidate| candidate.id == tab)
                .and_then(|candidate| match &candidate.tile {
                    TileContent::PeerWindow {
                        app_id,
                        missing: Some(MissingApp::Installable { display_name }),
                        ..
                    } => Some((workspace, app_id.clone(), display_name.clone())),
                    _ => None,
                })
        });
    let Some((workspace, app_id, display_name)) = found else {
        return false;
    };
    let item = Pending {
        tab,
        app_id,
        workspace,
        local: false,
    };
    let Some(ws) = tree.desktop.workspaces.get(workspace) else {
        return false;
    };
    if let Err(refusal) = admits(ws, &item.app_id) {
        return set_state(tree, tab, refusal, None);
    }
    let (room, capabilities) = (ws.room, ws.capabilities.clone());
    // Placement and any continued record are staged before the install
    // starts, so the window that finally maps lands in this tab.
    stage_continuation_for_tab(tree, &item);
    let changed = set_state(
        tree,
        tab,
        crate::shell::i18n::t_args(
            "peer-window-installing",
            &[("app", display_name.clone().into())],
        ),
        None,
    );
    crate::shell::catalogue::install_and_launch(item.app_id, display_name, room, capabilities);
    changed
}

/// Reads a governed room's committed record: (room, app, record) → bytes.
type HostedRecordReader =
    dyn Fn(RoomId, &str, &ui::record::RecordId) -> Result<Vec<u8>, String> + Send + Sync;
static HOSTED_RECORDS: std::sync::OnceLock<Box<HostedRecordReader>> = std::sync::OnceLock::new();

/// Install how this session reads the records of rooms another device governs.
pub(crate) fn set_hosted_record_reader(reader: Box<HostedRecordReader>) {
    let _ = HOSTED_RECORDS.set(reader);
}

/// Point `item`'s window at the record its governed room's owner created for
/// the tab: keep that record in this device's profile of the app and name it
/// in the saved view of the instance the tab launches as. `Ok(false)`
/// while this device has not admitted the record yet.
fn prepare_hosted_record(tree: &WorkspaceTree, item: &Pending) -> Result<bool, String> {
    use crate::shell::record_hydration::{RecordAddress, keep, reopen_on};
    let Some(ws) = tree.desktop.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string());
    };
    let room = ws.room;
    let Some(record) = tree
        .room_tab_record(room, item.tab)
        .and_then(|record| ui::record::RecordId::parse(&record))
    else {
        return Ok(false);
    };
    let (Some(reader), Some(data_home)) =
        (HOSTED_RECORDS.get(), ::launcher::appdir::host_data_home())
    else {
        return Err("This workspace's records are not reachable here".to_string());
    };
    let Ok(bytes) = reader(room, &item.app_id, &record) else {
        return Ok(false);
    };
    let address = RecordAddress {
        room,
        app_id: item.app_id.clone(),
        record,
    };
    let kept = keep(&address, &data_home, &bytes)?;
    reopen_on(&address, &data_home, item.tab, Some(&kept));
    Ok(true)
}

/// Whether any peer window is still waiting to be examined: an unanswered
/// window, a saved window still opening, or a queued retry. One pass over the
/// tabs that stops at the first hit and allocates nothing; the frame loop
/// calls it only after the tree may have changed.
pub fn any_pending(tree: &WorkspaceTree) -> bool {
    #[cfg(test)]
    PENDING_WALKS.with(|walks| walks.set(walks.get().saturating_add(1)));
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .any(|tab| {
            awaits_examination(&tab.tile)
                || matches!(&tab.tile, TileContent::LocalApp { note, .. } if note == OPENING)
        })
        || retry_requested()
}

/// Whether a card's explicit retry waits for the next [`examine`]. Queued
/// outside the tree, so no tree revision announces it.
pub(crate) fn retry_requested() -> bool {
    with_retry_requests(|requests| !requests.is_empty())
}

/// A window no decision has been made for yet: [`examine`]'s work list.
fn awaits_examination(tile: &TileContent) -> bool {
    matches!(
        tile,
        TileContent::PeerWindow { app_id, note, .. } | TileContent::LocalApp { app_id, note, .. }
            if note.is_empty() && !app_id.trim().is_empty()
    )
}

#[cfg(test)]
thread_local! {
    /// How many times this thread walked the tree for [`any_pending`].
    static PENDING_WALKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// How many [`examine`] passes this thread ran.
    static EXAMINE_PASSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Walks and examination passes this thread has made so far.
#[cfg(test)]
pub(crate) fn work_counts() -> (u64, u64) {
    (
        PENDING_WALKS.with(std::cell::Cell::get),
        EXAMINE_PASSES.with(std::cell::Cell::get),
    )
}

/// Settle a persisted app card whose process started but never produced
/// a surface. This is not automatically retryable: retrying every frame would
/// create an unbounded process loop.
pub(crate) fn mark_timed_out_restore(tree: &mut WorkspaceTree, package_id: &str) -> bool {
    settle_local_restore(
        tree,
        package_id,
        &crate::shell::i18n::t("host-app-did-not-open-body"),
    )
}

/// A terminal launch outcome answers only still-opening local cards. Keep the
/// saved tab and app record association, and never retry an answered card on
/// each frame. A mapped Foreign window or an unrelated refusal is untouched.
pub(crate) fn settle_local_restore(
    tree: &mut WorkspaceTree,
    package_id: &str,
    detail: &str,
) -> bool {
    let host_app = crate::shell::host_apps::find(package_id);
    let mut changed = false;
    for ws in &mut tree.desktop.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    let TileContent::LocalApp { app_id, note, .. } = &mut tab.tile else {
                        continue;
                    };
                    let matches = launcher::app_id_matches_package(app_id, package_id)
                        || host_app.as_ref().is_some_and(|app| {
                            app.desktop_id.eq_ignore_ascii_case(app_id)
                                || app
                                    .window_identities
                                    .iter()
                                    .any(|id| id.eq_ignore_ascii_case(app_id))
                        });
                    if note == OPENING && matches {
                        *note = detail.to_string();
                        changed = true;
                    }
                }
            }
        }
    }
    if changed {
        tree.queue_save();
    }
    changed
}

/// Every peer window with no note on it yet.
fn pending(tree: &WorkspaceTree) -> Vec<Pending> {
    let mut out = Vec::new();
    for (idx, ws) in tree.desktop.workspaces.iter().enumerate() {
        for strip in &ws.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if !awaits_examination(&tab.tile) {
                        continue;
                    }
                    if let TileContent::PeerWindow { app_id, .. }
                    | TileContent::LocalApp { app_id, .. } = &tab.tile
                    {
                        out.push(Pending {
                            tab: tab.id,
                            app_id: app_id.clone(),
                            workspace: idx,
                            local: matches!(&tab.tile, TileContent::LocalApp { .. }),
                        });
                    }
                }
            }
        }
    }
    out
}

/// Older builds persisted a host window's toolkit identity and then marked
/// the card permanently missing when that identity was not a package id. If
/// the current desktop-entry catalogue can now resolve that identity, the
/// error is stale rather than user state: clear it so the normal restore path
/// can migrate and relaunch the app. A genuinely absent application remains
/// an answered card and is not retried on every tick.
pub fn retry_resolvable_host_app_errors(tree: &mut WorkspaceTree) -> bool {
    let mut changed = false;
    for workspace in &mut tree.desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    let TileContent::LocalApp { app_id, note, .. } = &mut tab.tile else {
                        continue;
                    };
                    if note == NOT_INSTALLED && crate::shell::host_apps::resolve(app_id).is_some() {
                        note.clear();
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

/// Start a local instance of `item`'s app, bound to `item`'s tab. `Err`
/// carries the sentence to put on the card — what is actually the case,
/// never a promise ("not installed", not "coming soon").
fn open_for(tree: &mut WorkspaceTree, item: &Pending) -> Result<(), Refusal> {
    let Some(ws) = tree.desktop.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string().into());
    };
    let ws_id = ws.id;
    if !item.local {
        admits(ws, &item.app_id)?;
    }
    // After `admits`, so nothing is granted for a launch the room refused.
    if !item.local {
        crate::shell::search::providers::applications::grant_file_manager_home(
            tree,
            ws_id,
            &item.app_id,
        );
    }
    let Some(ws) = tree.desktop.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string().into());
    };
    // Profile-backed windows need their exact record before launch. Native
    // committed rooms read through their owner: an app profile is neither
    // required nor an authority fallback for that path.
    if !item.local && !tree.commits_native_documents(ws.room) {
        bring_over_continued_record(item, ws.room)?;
    }
    stage_continuation_for_tab(tree, item);
    // The instance runs as the tab, whose saved view names the record the
    // room's owner created for it (`prepare_hosted_record`).
    if !item.local
        && tree.requires_native_owner(ws.room)
        && tree.room_tab_record(ws.room, item.tab).is_some()
    {
        launcher::stage_continuation_record(ws.room, item.tab, item.tab);
    }
    let launched = launcher::launch_in_workspace(&item.app_id, ws)
        .map(|_| ())
        .or_else(|err| {
            if item.local
                && matches!(err, launcher::LaunchError::NotInstalled { .. })
                && let Some(app) = crate::shell::host_apps::resolve(&item.app_id)
            {
                crate::shell::host_apps::note_placement(&app, Placement::IntoTab(item.tab));
                // The restored window belongs to this workspace, so an X11
                // client joins that workspace's private X server.
                return crate::shell::host_apps::launch(&app.desktop_id, Some(ws.room.to_string()))
                    .map(|_| ());
            }
            Err(err)
        });
    match launched {
        Ok(()) => Ok(()),
        Err(err) => {
            launcher::clear_pending_placement(&item.app_id);
            Err(match err {
                launcher::LaunchError::NotInstalled { .. }
                | launcher::LaunchError::HostAppNotFound { .. } => Refusal::NotInstalled,
                launcher::LaunchError::NotAGuiApp { .. } => {
                    Refusal::Said("This app has no window to open".to_string())
                }
                other => Refusal::Said(format!("Could not open it here: {other}")),
            })
        }
    }
}

/// Stage the exact continuation selected for a pending tab before launching.
fn stage_continuation_for_tab(tree: &WorkspaceTree, item: &Pending) {
    let Some(ws) = tree.desktop.workspaces.get(item.workspace) else {
        return;
    };
    let matches_current_tab = ws
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .any(|tab| {
            tab.id == item.tab
                && matches!(
                    (&tab.tile, item.local),
                    (TileContent::LocalApp { app_id, .. }, true)
                        | (TileContent::PeerWindow { app_id, .. }, false)
                        if app_id == &item.app_id
                )
        });
    if !matches_current_tab {
        return;
    }
    // Before the launch, or a note written after races the window's toplevel.
    launcher::set_pending_placement(&item.app_id, Placement::IntoTab(item.tab));
    launcher::clear_staged_continuation(ws.room, item.tab);
    let shared = crate::shell::personal_continuity_registry::global_registry();
    let Ok(mut registry) = shared.lock() else {
        return;
    };
    let tab = crate::shell::personal_continuity::AppInstanceId(item.tab);
    // The process cache is tab-keyed. Never let a stale local association
    // shadow an exact remote offer after a tab changes kind/room/app.
    registry.forget_local_binding(tab);
    if item.local {
        let Some(record) = tree.local_app_record(item.tab, ws.room, &item.app_id) else {
            return;
        };
        registry.bind_tab(
            tab,
            crate::shell::personal_continuity::AppInstanceId(record),
        );
    }
    let Ok(launch) = crate::shell::personal_continuity::LaunchIntent::app(item.app_id.clone())
    else {
        return;
    };
    let Some(record_instance) = registry.record_instance_for_tab(ws.room, tab, &launch) else {
        return;
    };
    if let Some(continuation) = registry.continuation_for_tab(ws.room, tab, &launch) {
        launcher::stage_continuation_resume(
            ws.room,
            item.tab,
            continuation.resume.schema_version(),
            continuation.resume.bytes().to_vec(),
            record_instance.0,
        );
    } else {
        launcher::stage_continuation_record(ws.room, item.tab, record_instance.0);
    }
}

/// The record a window continuing from another device showed there, and
/// the continuity instance it runs as, when its offer names one.
fn continued_record(
    item: &Pending,
    room: crate::shell::state::RoomId,
) -> Option<(crate::shell::record_hydration::RecordAddress, u64)> {
    let launch = crate::shell::personal_continuity::LaunchIntent::app(item.app_id.clone()).ok()?;
    let tab = crate::shell::personal_continuity::AppInstanceId(item.tab);
    let continuation = crate::shell::personal_continuity_registry::global_registry()
        .lock()
        .ok()?
        .continuation_for_tab(room, tab, &launch)?;
    let record = ui::app::structural_continuation_record(continuation.resume.bytes())?;
    Some((
        crate::shell::record_hydration::RecordAddress {
            room,
            app_id: item.app_id.clone(),
            record,
        },
        continuation.record_instance.0,
    ))
}

/// What a card says while its record is being brought from another device.
fn bringing_note() -> String {
    crate::shell::i18n::t("continuity-bringing-record")
}

/// Make sure the record a continuing window showed is kept here, and that
/// its instance reopens on it. `Err` is the card's note while the record is
/// still on its way or cannot be read. A failed fetch must not open a
/// lookalike empty record in place of the selected resource.
fn bring_over_continued_record(
    item: &Pending,
    room: crate::shell::state::RoomId,
) -> Result<(), String> {
    use crate::shell::record_hydration::{Hydration, hydrate, kept_here};
    let Some((address, instance)) = continued_record(item, room) else {
        return Ok(());
    };
    let Some(data_home) = ::launcher::appdir::host_data_home() else {
        return Ok(());
    };
    let record = if kept_here(&address, &data_home) {
        None
    } else {
        match hydrate(&address, &data_home) {
            Hydration::Waiting => return Err(bringing_note()),
            Hydration::Kept(record) => Some(record),
            Hydration::Failed(reason) => {
                tracing::warn!(%reason, app = %item.app_id, "a continued record could not be brought over");
                return Err(reason);
            }
        }
    };
    crate::shell::record_hydration::reopen_on(&address, &data_home, instance, record.as_ref());
    Ok(())
}

/// Clear the note of every card whose record has stopped being on its way,
/// so the next pass opens it. Returns whether any was released.
fn release_brought_over(tree: &mut WorkspaceTree) -> bool {
    let note = &bringing_note();
    let waiting: Vec<(TabId, String, crate::shell::state::RoomId)> = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|ws| {
            ws.strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter_map(move |tab| match &tab.tile {
                    TileContent::PeerWindow {
                        app_id, note: slot, ..
                    } if slot == note => Some((tab.id, app_id.clone(), ws.room)),
                    _ => None,
                })
        })
        .collect();
    let Some(data_home) = ::launcher::appdir::host_data_home() else {
        return false;
    };
    let mut released = false;
    for (tab, app_id, room) in waiting {
        let item = Pending {
            tab,
            app_id,
            workspace: 0,
            local: false,
        };
        let still_waiting = continued_record(&item, room).is_some_and(|(address, _)| {
            crate::shell::record_hydration::hydrate(&address, &data_home)
                == crate::shell::record_hydration::Hydration::Waiting
        });
        if !still_waiting {
            released |= set_note(tree, tab, String::new());
        }
    }
    released
}

/// Whether `ws`'s room authorises starting `app_id` here. See the module
/// docs for why this is asked of the room's owner and with this verb.
fn admits(ws: &Workspace, app_id: &str) -> Result<(), String> {
    let Some(owner) = ws.owner.clone() else {
        return Err("This workspace has no owner yet".to_string());
    };
    let actor = ws.actor_for_person(&owner);
    if ws.allows(&actor, &ResourceRef::App(app_id.to_string()), Verb::Write) {
        return Ok(());
    }
    Err("This workspace does not permit opening it here".to_string())
}

/// Write `note` onto the peer window in `tab`. Returns whether it landed —
/// `false` if the tab went away, or stopped being a peer window, while the
/// decision was being made.
fn set_note(tree: &mut WorkspaceTree, tab: TabId, note: String) -> bool {
    set_state(tree, tab, note, None)
}

/// [`set_note`], also recording on a peer window whether its missing app can
/// be installed here.
fn set_state(
    tree: &mut WorkspaceTree,
    tab: TabId,
    note: String,
    missing: Option<MissingApp>,
) -> bool {
    for ws in &mut tree.desktop.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for t in &mut group.tabs {
                    if t.id != tab {
                        continue;
                    }
                    match &mut t.tile {
                        TileContent::PeerWindow {
                            note: slot,
                            missing: missing_slot,
                            ..
                        } => {
                            if slot == &note && missing_slot == &missing {
                                return false;
                            }
                            *slot = note;
                            *missing_slot = missing;
                            return true;
                        }
                        TileContent::LocalApp { note: slot, .. } => {
                            if slot == &note {
                                return false;
                            }
                            *slot = note;
                            return true;
                        }
                        _ => {}
                    }
                    return false;
                }
            }
        }
    }
    false
}

/// Where this machine already has the room's copy of `app_id` on screen
/// in `ws`, if it does. Without this, opening an app that already has a
/// peer-started instance gets a second process, and propagates into the
/// room so every member's [`examine`] starts one too.
///
/// Only counts a tab this machine does *not* own — two terminals the
/// user opened themselves are two windows they asked for. A tab still
/// showing a card counts too, as long as it's been answered for (`note`
/// non-empty).
pub fn opened_for_the_room(
    ws: &Workspace,
    app_id: &str,
    local_salt: u16,
) -> Option<(StripId, GroupId, TabId)> {
    for strip in &ws.strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                if crate::shell::state::peer_salt_of(tab.id) == local_salt {
                    continue;
                }
                let hosts = match &tab.tile {
                    TileContent::Foreign { app_id: a, .. } => a.eq_ignore_ascii_case(app_id),
                    TileContent::PeerWindow {
                        app_id: a, note, ..
                    } => !note.is_empty() && a.eq_ignore_ascii_case(app_id),
                    _ => false,
                };
                if hosts {
                    return Some((strip.id, group.id, tab.id));
                }
            }
        }
    }
    None
}

/// Where this machine already shows its own window of `app_id`, in any
/// workspace. The device's control panel has one window: opening it again
/// focuses that window. A second process could not open its record, which
/// the running one holds, and would exit with an error.
pub fn opened_here(
    tree: &WorkspaceTree,
    app_id: &str,
) -> Option<(crate::shell::state::WorkspaceId, StripId, GroupId, TabId)> {
    let local_salt = tree.local_salt();
    tree.desktop.workspaces.iter().find_map(|ws| {
        opened_locally_in_workspace(ws, app_id, local_salt)
            .map(|(strip, group, tab)| (ws.id, strip, group, tab))
    })
}

/// A mapped local window within this workspace only. Ordinary Open Terminal
/// returns to its existing room record; explicit new-window actions remain
/// responsible for asking for another instance.
pub(crate) fn opened_locally_in_workspace(
    ws: &Workspace,
    app_id: &str,
    local_salt: u16,
) -> Option<(StripId, GroupId, TabId)> {
    ws.strips.iter().find_map(|strip| {
        strip.window_groups.iter().find_map(|group| {
            group.tabs.iter().find_map(|tab| {
                let ours = crate::shell::state::peer_salt_of(tab.id) == local_salt;
                let hosts = matches!(
                    &tab.tile,
                    TileContent::Foreign { app_id: a, .. } if a.eq_ignore_ascii_case(app_id)
                );
                (ours && hosts).then_some((strip.id, group.id, tab.id))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{
        CapOverride, Effect, Principal, Scope, Strip, StripId, Tab, TileContent, WindowGroup,
        WindowWidth, Workspace, WorkspaceId, WorkspaceTree, next_id,
    };

    const APP: &str = "so.tilo.word-processor";

    fn room_owned_by(owner: &str) -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from(next_id()), "shared");
        ws.owner = Some(owner.to_string());
        ws
    }

    const FILES: &str = "so.tilo.file-explorer";

    fn tree_with_local_app(app_id: &str) -> (WorkspaceTree, TabId) {
        let mut ws = room_owned_by("ada");
        let tab = Tab {
            id: next_id(),
            title: "Legacy App".into(),
            tile: TileContent::LocalApp {
                app_id: app_id.into(),
                label: "Legacy App".into(),
                note: String::new(),
            },
        };
        let tab_id = tab.id;
        let group = WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let mut strip = Strip::new(StripId::from(next_id()), "main");
        strip.window_groups = vec![group];
        ws.strips = vec![strip];
        let ws_id = ws.id;
        (
            WorkspaceTree {
                desktop: crate::shell::state::DesktopTree {
                    workspaces: vec![ws],
                    active: ws_id,
                    ..Default::default()
                },
                ..Default::default()
            },
            tab_id,
        )
    }

    fn local_app_note(tree: &WorkspaceTree, tab_id: TabId) -> &str {
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .find_map(|tab| {
                (tab.id == tab_id).then(|| match &tab.tile {
                    TileContent::LocalApp { note, .. } => note.as_str(),
                    other => panic!("expected LocalApp, got {other:?}"),
                })
            })
            .expect("local app tab remains in the tree")
    }

    #[test]
    fn orphaned_local_opening_card_settles_once_without_losing_its_saved_record() {
        let app = "dev.tilo.orphaned-saved-card";
        let (mut tree, orphan) = tree_with_local_app(app);
        set_note(&mut tree, orphan, OPENING.into());
        tree.remember_local_app_record(orphan, 8123);
        let live = next_id();
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(Tab {
                id: live,
                title: "Still launching".into(),
                tile: TileContent::LocalApp {
                    app_id: app.into(),
                    label: "Still launching".into(),
                    note: OPENING.into(),
                },
            });
        tree.remember_local_app_record(live, 8124);
        let records = tree.local_app_records.clone();
        launcher::set_pending_placement(app, Placement::IntoTab(live));
        assert!(
            examine(&mut tree),
            "an opening card with no exact launch must become recoverable"
        );
        assert_eq!(
            local_app_note(&tree, orphan),
            crate::shell::i18n::t("shell-window-restore-interrupted")
        );
        assert_eq!(
            local_app_note(&tree, live),
            OPENING,
            "the live sibling's ticket cannot keep the orphan opening"
        );
        assert_eq!(tree.local_app_records, records);
        assert!(
            !examine(&mut tree),
            "settlement must not relaunch or keep changing the saved card"
        );
        launcher::clear_pending_placements();
    }

    #[test]
    fn saved_local_card_exposes_an_exact_semantic_recovery_control() {
        let app = "dev.tilo.saved-card-recovery";
        let (mut tree, tab) = tree_with_local_app(app);
        set_note(&mut tree, tab, "A launch failed".into());
        tree.remember_local_app_record(tab, 8181);
        let records = tree.local_app_records.clone();
        let inputs = crate::shell::hosted_inputs::HostedTileInputs::fixture().unwrap();
        let tile = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0]
            .tile
            .clone();
        let ui::widget::TileContent::App(mut card) =
            crate::shell::to_ui_tile_for_tab_with_inputs(&tile, tab, &inputs)
        else {
            panic!("the saved failure card must offer recovery, not only static text");
        };
        assert!(
            !card.uses_host_record(),
            "a recovery control does not acquire scratch custody"
        );
        let id = format!("reopen-window:{tab}");
        assert!(
            ui::widget::controls::control_action_bindings(card.as_ref())
                .iter()
                .any(|binding| binding.control_id == id)
        );
        let mut doc = automerge::AutoCommit::new();
        assert!(!card.perform_semantic_action(
            "reopen-window:wrong",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(card.perform_semantic_action(
            &id,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(card.perform_semantic_action(
            &id,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(
            examine(&mut tree),
            "explicit recovery examines this saved tab"
        );
        assert_eq!(
            local_app_note(&tree, tab),
            NOT_INSTALLED,
            "the synthetic missing app is answered without spawning"
        );
        assert_eq!(
            tree.local_app_records, records,
            "recovery retains the selected saved record"
        );
        assert_eq!(
            tree.desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .len(),
            1
        );
        assert_eq!(
            tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id,
            tab
        );
        assert!(
            !examine(&mut tree),
            "a repeated activation cannot leave another retry queued"
        );
        launcher::clear_pending_placements();
    }

    #[test]
    fn every_distinct_saved_card_recovery_is_drained_and_stale_requests_are_inert() {
        let app = "dev.tilo.recovery-batch-fixture";
        let (mut tree, first) = tree_with_local_app(app);
        set_note(&mut tree, first, "Interrupted".into());
        let second = next_id();
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(Tab {
                id: second,
                title: "Second saved window".into(),
                tile: TileContent::LocalApp {
                    app_id: app.into(),
                    label: "Second".into(),
                    note: "Interrupted".into(),
                },
            });
        tree.remember_local_app_record(first, 8191);
        tree.remember_local_app_record(second, 8192);
        let records = tree.local_app_records.clone();
        request_retry(first, app);
        request_retry(first, app);
        request_retry(second, app);
        request_retry(second, "dev.tilo.wrong");
        request_retry(next_id(), app);
        assert!(examine(&mut tree));
        assert_eq!(local_app_note(&tree, first), NOT_INSTALLED);
        assert_eq!(
            local_app_note(&tree, second),
            NOT_INSTALLED,
            "do not short-circuit the request batch"
        );
        assert_eq!(tree.local_app_records, records);
        assert!(
            !examine(&mut tree),
            "requests including closed/mismatched cards are consumed"
        );
    }

    #[test]
    fn saved_card_keyboard_pointer_cancel_and_resize_use_the_shared_control() {
        for input in ["keyboard", "pointer", "semantic"] {
            let app = "dev.tilo.recovery-input-fixture";
            let (mut tree, tab) = tree_with_local_app(app);
            set_note(&mut tree, tab, "The launch was interrupted.".into());
            let card = crate::shell::app_notice_card::AppNoticeCard::retry(
                tab,
                app,
                "Saved window",
                "The launch was interrupted.",
            );
            let mut harness = ui::headless::FrameHarness::new_with_theme(
                ui::State::new(),
                Box::new(card),
                (456, 500),
                ui::launch::shot_theme_from_env().unwrap(),
            );
            harness.key("Tab");
            if input == "keyboard"
                && let Some(directory) = std::env::var_os("TILO_LOCALAPP_NOTICE_EVIDENCE")
            {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                harness.render_png(&directory.join("recovery-focused.png"));
            }
            harness.key("Escape");
            assert!(!examine(&mut tree), "Escape must not request recovery");
            harness.resize((620, 400)).unwrap();
            if input == "keyboard"
                && let Some(directory) = std::env::var_os("TILO_LOCALAPP_NOTICE_EVIDENCE")
            {
                harness
                    .render_png(&std::path::PathBuf::from(directory).join("recovery-resized.png"));
            }
            harness.resize((456, 500)).unwrap();
            if input == "keyboard" {
                harness.key("Enter");
            } else if input == "semantic" {
                let resource = harness
                    .resources()
                    .into_iter()
                    .find(|resource| {
                        resource.label == crate::shell::i18n::t("shell-window-retry")
                            && resource
                                .actions
                                .iter()
                                .any(|action| action.id == "activate")
                    })
                    .expect("recovery is published through the native resource door");
                harness
                    .invoke_action(
                        &tilo_actions::ActionAddress::new(resource.id.as_str(), "activate"),
                        &serde_json::Map::new(),
                        false,
                        None,
                    )
                    .expect("advertised saved-window recovery");
            } else {
                let button = harness
                    .root_widget()
                    .synthetic_accessibility_rect(&[0])
                    .expect("shared recovery button rectangle");
                harness.click_at(button.center().x, button.center().y);
            }
            assert!(
                examine(&mut tree),
                "{input} must reach the saved card action"
            );
            assert_eq!(local_app_note(&tree, tab), NOT_INSTALLED);
            assert!(!examine(&mut tree), "activation must be consumed once");
        }
    }

    #[test]
    fn restored_sibling_waits_across_frames_until_the_first_window_maps() {
        let app = "dev.tilo.restore-sibling-waits";
        let (mut tree, first) = tree_with_local_app(app);
        set_note(&mut tree, first, OPENING.into());
        let second = next_id();
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(Tab {
                id: second,
                title: "Second saved window".into(),
                tile: TileContent::LocalApp {
                    app_id: app.into(),
                    label: "Second saved window".into(),
                    note: String::new(),
                },
            });
        launcher::set_pending_placement(app, Placement::IntoTab(first));
        launcher::attach_continuity_instance_for_test(app, 42);

        for _ in 0..3 {
            assert!(
                !examine(&mut tree),
                "the next reconciliation frame must wait for the preceding saved window"
            );
            assert_eq!(local_app_note(&tree, second), "");
            assert!(any_pending(&tree), "the sibling still needs restoration");
        }
        assert_eq!(
            launcher::placement_and_instance_for_toplevel(app, 4711),
            Some((Placement::IntoTab(first), Some(42))),
            "the first native surface retains its exact saved tab and record"
        );
        assert!(examine(&mut tree), "mapping releases the next saved window");
        assert_eq!(local_app_note(&tree, second), NOT_INSTALLED);
        launcher::clear_pending_placements();
    }

    #[test]
    fn restored_window_waits_for_a_live_launch_but_not_a_failed_or_unrelated_one() {
        let app = "dev.tilo.restore-launch-lifetime";
        let (mut tree, tab) = tree_with_local_app(app);
        launcher::set_pending_placement(app, Placement::IntoTab(next_id()));
        assert!(
            !examine(&mut tree),
            "an unanswered launch must survive into the next reconciliation pass"
        );
        assert_eq!(local_app_note(&tree, tab), "");

        let other = "dev.tilo.unrelated-restore";
        let (mut other_tree, other_tab) = tree_with_local_app(other);
        assert!(
            examine(&mut other_tree),
            "other apps are examined independently"
        );
        assert_eq!(local_app_note(&other_tree, other_tab), NOT_INSTALLED);

        launcher::clear_pending_placement(app);
        assert!(
            examine(&mut tree),
            "a failed launch must release its sibling"
        );
        assert_eq!(local_app_note(&tree, tab), NOT_INSTALLED);
        launcher::clear_pending_placements();
    }

    #[test]
    fn peer_windows_any_pending_matches_pending_is_empty() {
        with_retry_requests(std::mem::take);
        let empty = WorkspaceTree::default();
        let (mut answered, answered_tab) = tree_with_local_app("dev.tilo.answered-card");
        set_note(&mut answered, answered_tab, NOT_INSTALLED.into());
        let (blank_app, _) = tree_with_local_app("  ");
        let (one_local, _) = tree_with_local_app("dev.tilo.one-local");
        let (one_peer, _) = tree_with_peer_window(room_owned_by("ada"));
        let (mut many, _) = tree_with_peer_window_for(room_owned_by("ada"), APP);
        for app in ["dev.tilo.second", "dev.tilo.third"] {
            let (other, _) = tree_with_local_app(app);
            many.desktop.workspaces.extend(other.desktop.workspaces);
        }
        let (mut late, _) = tree_with_local_app("dev.tilo.answered-first");
        let first = late.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
        set_note(&mut late, first, NOT_INSTALLED.into());
        let (pending_last, _) = tree_with_local_app("dev.tilo.pending-last");
        late.desktop
            .workspaces
            .extend(pending_last.desktop.workspaces);

        for (name, tree, expected) in [
            ("an empty desktop", &empty, 0),
            ("an answered card", &answered, 0),
            ("a card without an app id", &blank_app, 0),
            ("one saved window", &one_local, 1),
            ("one peer window", &one_peer, 1),
            ("several windows", &many, 3),
            ("a pending window after an answered one", &late, 1),
        ] {
            assert_eq!(pending(tree).len(), expected, "{name}: fixture shape");
            assert_eq!(
                any_pending(tree),
                !pending(tree).is_empty(),
                "{name}: the early-exit walk agrees with examine's work list"
            );
        }

        // A saved window still opening is owed another look, though it is
        // not on the work list: its launch may still fail.
        let (mut opening, tab) = tree_with_local_app("dev.tilo.still-opening");
        set_note(&mut opening, tab, OPENING.into());
        assert!(pending(&opening).is_empty());
        assert!(any_pending(&opening));
        // So is an explicit retry queued outside the tree.
        request_retry(tab, "dev.tilo.still-opening");
        assert!(any_pending(&empty), "a queued retry is owed a look");
        with_retry_requests(std::mem::take);
        assert!(!any_pending(&empty));
    }

    fn tree_with_peer_window(ws: Workspace) -> (WorkspaceTree, u64) {
        tree_with_peer_window_for(ws, APP)
    }

    fn tree_with_peer_window_for(ws: Workspace, app: &str) -> (WorkspaceTree, u64) {
        let mut ws = ws;
        let tab = Tab {
            id: peer_id(),
            title: "Word Processor".into(),
            tile: TileContent::PeerWindow {
                app_id: app.into(),
                label: "Word Processor".into(),
                note: String::new(),
                missing: None,
            },
        };
        let tab_id = tab.id;
        let group = WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let mut strip = Strip::new(StripId::from(next_id()), "main");
        strip.window_groups = vec![group];
        ws.strips = vec![strip];
        let ws_id = ws.id;
        let tree = WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                ..Default::default()
            },
            ..Default::default()
        };
        (tree, tab_id)
    }

    /// An id carrying a salt that isn't this process's, so tabs built
    /// with it look like a peer's.
    fn peer_id() -> u64 {
        const SOMEBODY_ELSE: u64 = 0xBEEF;
        (SOMEBODY_ELSE << crate::shell::state::PEER_ID_SALT_SHIFT) | (next_id() & 0xFFFF)
    }

    fn may_read(ws: &Workspace, app: &str, path: &std::path::Path) -> bool {
        ws.allows(
            &ws.actor_for_app(app),
            &ResourceRef::FsPath(path.to_string_lossy().into_owned()),
            Verb::Read,
        )
    }

    /// Settings already open in another workspace is the window a second
    /// open returns to; a peer's window of it is not this machine's.
    #[test]
    fn this_machines_own_window_of_an_app_is_found_in_any_workspace() {
        const SETTINGS: &str = "so.tilo.settings";
        let (mut tree, _) = tree_with_peer_window_for(room_owned_by("ada"), SETTINGS);
        assert!(
            opened_here(&tree, SETTINGS).is_none(),
            "a peer's window is not ours"
        );

        let mut other = Workspace::new(WorkspaceId::from(next_id()), "elsewhere");
        let tab = Tab {
            id: next_id(),
            title: "Personal Settings".into(),
            tile: TileContent::Foreign {
                app_id: SETTINGS.into(),
                surface_id: 7,
            },
        };
        let tab_id = tab.id;
        let group = WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let group_id = group.id;
        let mut strip = Strip::new(StripId::from(next_id()), "main");
        strip.window_groups = vec![group];
        let strip_id = strip.id;
        other.strips = vec![strip];
        let other_id = other.id;
        tree.desktop.workspaces.push(other);
        tree.set_salt_override(crate::shell::state::peer_salt_of(tab_id));

        assert_eq!(
            opened_here(&tree, SETTINGS),
            Some((other_id, strip_id, group_id, tab_id))
        );
        assert!(opened_here(&tree, "so.tilo.notes").is_none());
        assert_eq!(
            opened_locally_in_workspace(&tree.desktop.workspaces[1], SETTINGS, tree.local_salt()),
            Some((strip_id, group_id, tab_id)),
        );
        assert!(
            opened_locally_in_workspace(&tree.desktop.workspaces[0], SETTINGS, tree.local_salt())
                .is_none(),
            "an ordinary Open must stay in its workspace"
        );
        assert!(
            opened_locally_in_workspace(
                &tree.desktop.workspaces[1],
                "so.tilo.terminal",
                tree.local_salt()
            )
            .is_none()
        );
        assert!(
            opened_locally_in_workspace(
                &tree.desktop.workspaces[1],
                SETTINGS,
                tree.local_salt().wrapping_add(1)
            )
            .is_none(),
            "a foreign device's tab is not a local mapped instance"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn room_file_managers_alone_see_the_users_home() {
        fn a_file_manager_opened_for_the_room_can_see_the_users_home() {
            let home = tilo_paths::home().expect("the test machine has a home directory");
            let (mut tree, _tab) = tree_with_peer_window_for(room_owned_by("ada"), FILES);

            assert!(
                !may_read(&tree.desktop.workspaces[0], FILES, &home),
                "fixture sanity: the file manager must start with no grant on the home, \
                 or this test cannot tell a granted one from a pre-granted one"
            );

            let _ = examine(&mut tree);

            assert!(
                may_read(&tree.desktop.workspaces[0], FILES, &home),
                "a file manager started on the room's behalf must be able to read the \
                 user's home; without it the window opens on its sandbox directory and \
                 says the folder is empty"
            );
        }

        fn opening_another_app_for_the_room_grants_it_no_home() {
            let home = tilo_paths::home().expect("the test machine has a home directory");
            let (mut tree, _tab) = tree_with_peer_window_for(room_owned_by("ada"), APP);

            let _ = examine(&mut tree);

            assert!(
                !may_read(&tree.desktop.workspaces[0], APP, &home),
                "only the file manager gets the user's home; the word processor must not"
            );
        }

        fn a_refused_file_manager_is_granted_nothing() {
            let home = tilo_paths::home().expect("the test machine has a home directory");
            let mut ws = room_owned_by("ada");
            ws.owner = None;
            let (mut tree, _tab) = tree_with_peer_window_for(ws, FILES);

            let _ = examine(&mut tree);

            assert!(
                !may_read(&tree.desktop.workspaces[0], FILES, &home),
                "a room that authorises no launch must not hand out the home either"
            );
        }

        a_file_manager_opened_for_the_room_can_see_the_users_home();
        opening_another_app_for_the_room_grants_it_no_home();
        a_refused_file_manager_is_granted_nothing();
    }

    #[test]
    fn windows_to_return_to_are_this_machines_answered_openings() {
        fn a_window_this_machine_opened_for_the_room_is_found() {
            let mut ws = room_owned_by("ada");
            let tab = Tab {
                id: peer_id(),
                title: "Word Processor".into(),
                tile: TileContent::Foreign {
                    app_id: APP.into(),
                    surface_id: 4711,
                },
            };
            let tab_id = tab.id;
            let group = WindowGroup {
                id: peer_id(),
                tabs: vec![tab],
                active_tab: tab_id,
                width: WindowWidth::Normal,
                maximized: false,
            };
            let mut strip = Strip::new(StripId::from(peer_id()), "main");
            strip.window_groups = vec![group];
            ws.strips = vec![strip];

            let found = opened_for_the_room(&ws, APP, 0);
            assert_eq!(
                found.map(|(_, _, t)| t),
                Some(tab_id),
                "a peer's tab already hosting this machine's window of the app has to be \
                 found, or the launch opens a second one beside it"
            );
            assert!(
                opened_for_the_room(&ws, "so.tilo.terminal", 0).is_none(),
                "a different app is not this one"
            );
        }

        fn a_window_this_person_opened_is_not_one_to_return_to() {
            let mut ws = room_owned_by("ada");
            let tab = Tab {
                id: next_id(),
                title: "Terminal".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.terminal".into(),
                    surface_id: 99,
                },
            };
            let tab_id = tab.id;
            let group = WindowGroup {
                id: next_id(),
                tabs: vec![tab],
                active_tab: tab_id,
                width: WindowWidth::Normal,
                maximized: false,
            };
            let mut strip = Strip::new(StripId::from(next_id()), "main");
            strip.window_groups = vec![group];
            ws.strips = vec![strip];

            let salt = crate::shell::state::peer_salt_of(tab_id);
            assert!(
                opened_for_the_room(&ws, "so.tilo.terminal", salt).is_none(),
                "a second terminal is an ordinary thing to want; a window this person \
                 opened must not swallow the next launch of the same app"
            );
        }

        fn an_unanswered_card_is_not_a_window_to_return_to() {
            let (tree, _tab) = tree_with_peer_window(room_owned_by("ada"));
            assert!(
                opened_for_the_room(&tree.desktop.workspaces[0], APP, 0).is_none(),
                "an unexamined peer window has no instance behind it yet"
            );
        }

        fn a_note_is_what_marks_a_window_answered() {
            let (mut tree, tab) = tree_with_peer_window(room_owned_by("ada"));
            assert!(any_pending(&tree), "an unexamined peer window is pending");
            assert!(set_note(
                &mut tree,
                tab,
                "Not installed on this machine".into()
            ));
            assert!(
                !any_pending(&tree),
                "a peer window that has been answered for must not be examined again"
            );
        }

        a_window_this_machine_opened_for_the_room_is_found();
        a_window_this_person_opened_is_not_one_to_return_to();
        an_unanswered_card_is_not_a_window_to_return_to();
        a_note_is_what_marks_a_window_answered();
    }

    #[test]
    fn room_authority_admits_apps_only_with_an_owner_and_no_restriction() {
        fn a_room_with_an_owner_admits_its_apps() {
            let ws = room_owned_by("ada");
            assert_eq!(admits(&ws, APP), Ok(()));
        }

        /// Fail-closed: this is the state a room is in before its governance
        /// has replicated, exactly when a machine must not start processes
        /// on a peer's say-so.
        fn a_room_with_no_owner_admits_nothing() {
            let mut ws = room_owned_by("ada");
            ws.owner = None;
            let refusal = admits(&ws, APP).expect_err("an ownerless room must authorise nothing");
            assert_eq!(refusal, "This workspace has no owner yet");
        }

        /// Goes *through* the capability model rather than around it —
        /// asserted against `Workspace::decide`, the same call every other
        /// enforcement point bottoms out in.
        fn a_restriction_on_the_app_stops_it_opening_here() {
            let mut ws = room_owned_by("ada");
            let _ = ws.grant_capability(CapOverride {
                effect: Effect::Restrict,
                ..CapOverride::grant(
                    crate::shell::state::Subject::Principal(Principal::Person("ada".into())),
                    Scope::App(APP.to_string()),
                    Verb::Read,
                )
            });
            let refusal = admits(&ws, APP).expect_err("a restricted app must not be opened here");
            assert!(
                refusal.contains("does not permit"),
                "the card has to say why; got {refusal:?}"
            );
            // The restriction is about this app, not about the room: another
            // app in the same room is still admitted.
            assert_eq!(admits(&ws, "so.tilo.terminal"), Ok(()));
        }

        fn an_ownerless_window_stays_pending_until_room_authority_arrives() {
            let mut ws = room_owned_by("ada");
            ws.owner = None;
            let (mut tree, tab) = tree_with_peer_window(ws);
            assert!(
                !examine(&mut tree),
                "missing authority is transient and must not become a permanent refusal"
            );
            assert!(any_pending(&tree), "the next authority sync must retry it");
            let note = tree.desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .find(|t| t.id == tab)
                .map(|t| match &t.tile {
                    TileContent::PeerWindow { note, .. } => note.clone(),
                    other => panic!("expected a peer window, got {other:?}"),
                })
                .expect("the tab is still there");
            assert!(
                note.is_empty(),
                "a temporary authority race must not leave a stale error card; got {note:?}"
            );
        }

        a_room_with_an_owner_admits_its_apps();
        a_room_with_no_owner_admits_nothing();
        a_restriction_on_the_app_stops_it_opening_here();
        an_ownerless_window_stays_pending_until_room_authority_arrives();
    }

    fn offered(package_id: &str, display_name: &str) -> crate::shell::catalogue::CatalogueApp {
        crate::shell::catalogue::CatalogueApp {
            package_id: package_id.into(),
            display_name: display_name.into(),
            description: String::new(),
            category: None,
            version: "1.0.0".into(),
            registry: "main".into(),
            trust: "verified".into(),
        }
    }

    fn missing_of(tree: &WorkspaceTree, tab: TabId) -> Option<MissingApp> {
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .find(|candidate| candidate.id == tab)
            .and_then(|candidate| match &candidate.tile {
                TileContent::PeerWindow { missing, .. } => missing.clone(),
                _ => None,
            })
    }

    #[test]
    fn a_missing_app_is_installable_only_where_a_registry_offers_it() {
        assert_eq!(
            missing_app(APP, &[offered(APP, "Word Processor")]),
            MissingApp::Installable {
                display_name: "Word Processor".into()
            }
        );
        assert_eq!(
            missing_app(APP, &[offered("so.tilo.sheets", "Sheets")]),
            MissingApp::Unavailable
        );
        assert_eq!(missing_app(APP, &[]), MissingApp::Unavailable);

        // A card answered before the catalogue arrived is answered again
        // once it does, and only then offers the install.
        let (mut tree, tab) = tree_with_peer_window(room_owned_by("ada"));
        assert!(set_state(
            &mut tree,
            tab,
            NOT_INSTALLED.into(),
            Some(MissingApp::Unavailable)
        ));
        assert!(!any_pending(&tree), "an answered card is not relaunched");
        crate::shell::catalogue::replace_for_test(vec![offered(APP, "Word Processor")]);
        assert!(reread_missing_apps(&mut tree));
        assert_eq!(
            missing_of(&tree, tab),
            Some(MissingApp::Installable {
                display_name: "Word Processor".into()
            })
        );
        crate::shell::catalogue::replace_for_test(Vec::new());
        assert!(reread_missing_apps(&mut tree));
        assert_eq!(missing_of(&tree, tab), Some(MissingApp::Unavailable));
    }

    #[test]
    fn only_a_pressed_install_on_an_installable_card_installs() {
        let (mut tree, tab) = tree_with_peer_window(room_owned_by("ada"));
        assert!(set_state(
            &mut tree,
            tab,
            NOT_INSTALLED.into(),
            Some(MissingApp::Unavailable)
        ));
        // Nothing this device can install provides it: the request is
        // dropped and the card is left as it is.
        request_install(tab);
        assert!(!start_requested_installs(&mut tree));
        assert_eq!(missing_of(&tree, tab), Some(MissingApp::Unavailable));

        // Pressing the card's button queues exactly that tab.
        let mut card = crate::shell::app_notice_card::AppNoticeCard::install(
            tab,
            "Word Processor",
            "Word Processor",
        );
        card.press_for_test();
        let queued = with_install_requests(std::mem::take);
        assert_eq!(queued, vec![tab]);
    }

    #[test]
    fn native_local_restore_failures_settle_without_discarding_saved_tabs() {
        let (mut tree, tab) = tree_with_local_app(FILES);
        assert!(set_note(&mut tree, tab, OPENING.into()));
        tree.remember_local_app_record(tab, 8130);
        let saved = tree.local_app_records.clone();
        let original_id = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
        assert!(mark_timed_out_restore(&mut tree, FILES));
        assert_eq!(
            local_app_note(&tree, tab),
            crate::shell::i18n::t("host-app-did-not-open-body")
        );
        assert!(
            !any_pending(&tree),
            "answered timeout must not relaunch each frame"
        );
        assert!(!mark_timed_out_restore(&mut tree, FILES));
        assert_eq!(
            tree.local_app_records, saved,
            "timeout keeps the saved app record"
        );
        assert_eq!(
            tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id,
            original_id
        );

        assert!(set_note(&mut tree, tab, OPENING.into()));
        assert!(!settle_local_restore(
            &mut tree,
            APP,
            "different app failed"
        ));
        assert_eq!(local_app_note(&tree, tab), OPENING);
        let failure = launcher::LaunchEvent {
            package_id: FILES.into(),
            display_name: "Files".into(),
            managed_agent_id: None,
            managed_workspace_id: None,
            kind: launcher::LaunchEventKind::SpawnFailed {
                reason: launcher::LaunchFailure::LauncherUnavailable,
                detail: "worker refused".into(),
            },
        }
        .failure_notice()
        .unwrap();
        assert!(settle_local_restore(&mut tree, FILES, &failure.body));
        assert_eq!(local_app_note(&tree, tab), failure.body);
        assert!(!settle_local_restore(&mut tree, FILES, "later error"));
        assert!(!any_pending(&tree));

        // A mapped window has already completed restore: failure never rewrites it.
        tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile = TileContent::Foreign {
            surface_id: 99,
            app_id: FILES.into(),
        };
        assert!(!mark_timed_out_restore(&mut tree, FILES));
        assert!(matches!(
            tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile,
            TileContent::Foreign { surface_id: 99, .. }
        ));
    }

    #[test]
    fn a_window_hosted_on_another_device_is_never_opened_here() {
        let (mut tree, tab) = tree_with_peer_window(room_owned_by("ada"));
        tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile =
            TileContent::HostedWindow {
                app_id: "org.gimp.GIMP".into(),
                label: "Poster.xcf".into(),
                host: "ab".repeat(32),
                host_label: "Laptop".into(),
            };
        assert!(!any_pending(&tree));
        assert!(!set_note(&mut tree, tab, OPENING.into()));
        assert!(opened_for_the_room(&tree.desktop.workspaces[0], "org.gimp.GIMP", 0).is_none());
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn host_window_restores_retry_once_and_time_out_explicitly() {
        fn stale_missing_note_for_a_resolvable_host_app_is_retried_once() {
            let data = tempfile::tempdir().expect("temporary XDG data home");
            let applications = data.path().join("applications");
            std::fs::create_dir_all(&applications).expect("applications directory");
            std::fs::write(
                applications.join("rygel-preferences.desktop"),
                "[Desktop Entry]\nType=Application\nName=Rygel Preferences\nExec=/bin/true\nStartupWMClass=org.gnome.Rygel.Preferences\n",
            )
            .expect("desktop entry");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_DATA_HOME", data.path());
            crate::shell::host_apps::invalidate_cache();

            let (mut tree, tab) = tree_with_local_app("org.gnome.Rygel.Preferences");
            assert!(set_note(
                &mut tree,
                tab,
                "Not installed on this machine".into()
            ));
            assert!(retry_resolvable_host_app_errors(&mut tree));
            assert_eq!(local_app_note(&tree, tab), "");
            assert!(any_pending(&tree));

            assert!(set_note(
                &mut tree,
                tab,
                "Not installed on this machine".into()
            ));
            if let TileContent::LocalApp { app_id, .. } =
                &mut tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile
            {
                *app_id = "org.example.ActuallyMissing".into();
            }
            assert!(!retry_resolvable_host_app_errors(&mut tree));
            assert_eq!(local_app_note(&tree, tab), "Not installed on this machine");

            crate::shell::host_apps::invalidate_cache();
            drop(env);
        }

        fn timed_out_host_restore_becomes_an_explicit_non_retrying_error() {
            let data = tempfile::tempdir().expect("temporary XDG data home");
            let applications = data.path().join("applications");
            std::fs::create_dir_all(&applications).expect("applications directory");
            std::fs::write(
                applications.join("org.example.Windowless.desktop"),
                "[Desktop Entry]\nType=Application\nName=Windowless App\nExec=/bin/true\nStartupWMClass=Windowless\n",
            )
            .expect("desktop entry");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_DATA_HOME", data.path());
            crate::shell::host_apps::invalidate_cache();

            let (mut tree, tab) = tree_with_local_app("org.example.Windowless");
            assert!(set_note(&mut tree, tab, OPENING.to_string()));
            assert!(mark_timed_out_restore(&mut tree, "org.example.Windowless"));
            assert_eq!(
                local_app_note(&tree, tab),
                crate::shell::i18n::t("host-app-did-not-open-body")
            );
            assert!(
                !any_pending(&tree),
                "timeout errors must not relaunch every frame"
            );

            crate::shell::host_apps::invalidate_cache();
            drop(env);
        }

        stale_missing_note_for_a_resolvable_host_app_is_retried_once();
        timed_out_host_restore_becomes_an_explicit_non_retrying_error();
    }
}

#[cfg(test)]
#[path = "peer_windows_local_record_tests.rs"]
mod local_record_restart_tests;

#[cfg(test)]
#[path = "peer_windows_hydration_tests.rs"]
mod hydration_tests;
