//! Chromium helper transport, independent of native CEF build dependencies.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the Chromium helper process is supervised by threads"
)]
mod audio;
mod damage;
mod extension_notification;
mod extension_omnibox;
mod extension_permissions;
mod extension_sign_in;
pub mod install;
mod menus;
mod protocol;
pub mod runtime;
mod service_session_probe;
mod shared;
pub use service_session_probe::service_session_capability;
pub use shared::serve_shared_until_idle;

use crate::time::Duration;
use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Condvar, Mutex, Weak};

use super::{
    FrameSlot, KeyInput, MouseButton, NavigationCommand, WebViewCommand, WebViewEngine,
    WebViewEvent,
};
use menus::{MENU_ROWS, decode_context_items, decode_extension_menu};
use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;

const MAX_OWNED_NAVIGATIONS: usize = 32;
/// The host numbers a page's popups from here, apart from the tabs the app
/// opens.
const FIRST_POPUP: u32 = 1 << 30;
/// Longest address a page can have: Chromium's own limit
/// (`url::kMaxURLChars`), in bytes.
const MAX_PAGE_URL: usize = 2 * 1024 * 1024;
/// Longest download address kept with its download, in bytes.
const MAX_DOWNLOAD_URL: usize = 8192;

#[derive(Default)]
struct NavigationRequests {
    last_id: u32,
    pending: HashMap<u32, String>,
}

/// Whether the host reports the document's size, and what this tab asked
/// and told it: told once the host said it can, so an older host never sees
/// the command.
#[derive(Default)]
struct DocumentSizeReports {
    can: AtomicBool,
    /// 0 never asked, 1 off, 2 on.
    asked: AtomicU8,
    told: AtomicU8,
}

#[derive(Default)]
struct Shared {
    audio: audio::PageAudio,
    document_size: DocumentSizeReports,
    /// Whether the host opens page popups as popups, whether this tab asked
    /// for them, and whether the host was told.
    popups_can: AtomicBool,
    popups_asked: AtomicBool,
    popups_told: AtomicBool,
    /// Whether the host sends the page's own questions, whether this tab
    /// asked for them, and whether the host was told.
    dialogs_can: AtomicBool,
    dialogs_asked: AtomicBool,
    dialogs_told: AtomicBool,
    /// Popups the page opened, not yet taken.
    popups: Mutex<VecDeque<super::Popup>>,
    /// The endpoints of popups the page opened, not yet adopted: each is
    /// routed from the moment it opens, so its first frames reach it.
    unadopted: Mutex<HashMap<u32, Arc<Shared>>>,
    /// The host reads a page's selection as text (`selection_text`).
    selection_text: AtomicBool,
    text_input: Mutex<super::text_input::Transactions>,
    native_edit_history: AtomicBool,
    guarded_edit_history: AtomicBool,
    native_caret_text_input: AtomicBool,
    open_tabs: Mutex<VecDeque<(String, bool)>>,
    /// The pointer shape the page last asked for.
    cursor: Mutex<Option<winit::window::CursorIcon>>,
    frames: FrameSlot,
    /// The tab's frame whole, which the host's damage packets change.
    frame_store: Mutex<damage::FrameStore>,
    /// The host was asked to send the tab's frame whole and has not yet.
    frame_asked: AtomicBool,
    events: Mutex<VecDeque<WebViewEvent>>,
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    uri: Mutex<String>,
    navigations: Mutex<NavigationRequests>,
    navigation_commands: Mutex<()>,
    stopping: AtomicBool,
    failed: AtomicBool,
    /// Set once the failure `failed` marks has queued its
    /// [`WebViewEvent::LoadFailed`]. The supervisor waits for it before it
    /// reaps a failed host, so once the host is reaped its failure is there
    /// for whoever drains the events.
    failure_reported: (Mutex<bool>, std::sync::Condvar),
    /// Wakes the host's supervisor when `failed` or `stopping` is set.
    supervisor_bell: SupervisorBell,
    /// How often the host's supervisor has looked at it: once at start and
    /// once per change, never on a timer while the host runs.
    supervisor_checks: AtomicU32,
    /// How often the supervisor has blocked with no deadline, until the host
    /// exits or its bell rings.
    #[cfg(test)]
    supervisor_untimed_waits: AtomicU32,
    /// The kernel's id for the supervisor's thread, so a test watches this
    /// host's supervisor and not another engine's of the same name.
    #[cfg(test)]
    supervisor_thread: std::sync::OnceLock<u32>,
    /// Set on the owner for another process's tab: its host events and
    /// frames go to that process instead of this endpoint.
    remote: Option<Arc<shared::Outbox>>,
}

impl Shared {
    fn wake(&self) {
        let waker = self.waker.lock().ok().and_then(|slot| slot.clone());
        if let Some(waker) = waker
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker())).is_err()
        {
            tracing::error!("Chromium UI wake callback panicked");
        }
    }

    fn admit_navigation(&self, request_id: u32, uri: String) -> Result<(), String> {
        let mut owners = self
            .navigations
            .lock()
            .map_err(|_| "Navigation owners unavailable")?;
        let events = self
            .events
            .lock()
            .map_err(|_| "Navigation outcomes unavailable")?;
        if self.failed.load(Ordering::Acquire) || self.stopping.load(Ordering::Acquire) {
            return Err("Chromium is unavailable".into());
        }
        if request_id <= owners.last_id {
            return Err("Navigation request IDs must increase without reuse".into());
        }
        let completed = events
            .iter()
            .filter(|event| matches!(event, WebViewEvent::NavigationOutcome { .. }))
            .count();
        if owners.pending.len() + completed >= MAX_OWNED_NAVIGATIONS {
            return Err("Navigation outcome capacity reached".into());
        }
        owners.last_id = request_id;
        owners.pending.insert(request_id, uri);
        Ok(())
    }

    fn cancel_navigation(&self, request_id: u32) {
        let mut owners = self.navigations.lock_recover();
        owners.pending.remove(&request_id);
        let mut events = self.events.lock_recover();
        events.retain(|event| !matches!(event, WebViewEvent::NavigationOutcome { request_id: id, .. } if *id == request_id));
    }

    fn fail_navigations(&self, message: &str) {
        let pending: Vec<_> = self
            .navigations
            .lock_recover()
            .pending
            .iter()
            .map(|(&id, uri)| (id, uri.clone()))
            .collect();
        for (request_id, uri) in pending {
            self.event(WebViewEvent::NavigationOutcome {
                request_id,
                uri,
                error: Some(message.into()),
            });
        }
    }

    fn complete_navigation(&self, event: WebViewEvent) {
        let WebViewEvent::NavigationOutcome { request_id, .. } = &event else {
            return;
        };
        // Reserve the queue slot before send; hold ownership through enqueue
        // so an admission cannot race a completing request's reservation.
        let mut owners = self.navigations.lock_recover();
        if owners.pending.remove(request_id).is_none() {
            return;
        }
        let mut events = self.events.lock_recover();
        if events.len() >= 512 {
            if let Some(index) = events.iter().position(|event| {
                !matches!(
                    event,
                    WebViewEvent::NavigationOutcome { .. }
                        | WebViewEvent::LoadFinished
                        | WebViewEvent::LoadFinishedAt { .. }
                        | WebViewEvent::LoadFailed { .. }
                )
            }) {
                events.remove(index);
            }
        }
        events.push_back(event);
        drop(events);
        drop(owners);
        self.wake();
    }

    fn event(&self, event: WebViewEvent) {
        if matches!(event, WebViewEvent::NavigationOutcome { .. }) {
            self.complete_navigation(event);
            return;
        }
        if matches!(event, WebViewEvent::TabClosed | WebViewEvent::TabDetached) {
            self.stop();
        }
        if matches!(
            event,
            WebViewEvent::RendererTerminated | WebViewEvent::TabClosed | WebViewEvent::TabDetached
        ) {
            self.text_input.lock_recover().unavailable();
        }
        // Renderer termination is visual: the host names each affected owner
        // explicitly. Its delayed generic event cannot invalidate a recovery
        // command admitted after the corresponding owned failure.
        if matches!(event, WebViewEvent::TabClosed | WebViewEvent::TabDetached) {
            self.fail_navigations("The native browser document became unavailable");
        }

        if let WebViewEvent::UriChanged(uri) = &event
            && let Ok(mut current) = self.uri.lock()
        {
            current.clone_from(uri);
        }
        if let Ok(mut events) = self.events.lock() {
            // State updates supersede older values without evicting load outcomes.
            let state = |value: &WebViewEvent| {
                matches!(
                    value,
                    WebViewEvent::UriChanged(_)
                        | WebViewEvent::TitleChanged(_)
                        | WebViewEvent::FaviconChanged { .. }
                        | WebViewEvent::AudioMutedChanged(_)
                        | WebViewEvent::MediaPlayingChanged { .. }
                        | WebViewEvent::FullscreenChanged(_)
                        | WebViewEvent::LoadProgress(_)
                        | WebViewEvent::HistoryChanged { .. }
                )
            };
            if state(&event) {
                events.retain(|old| std::mem::discriminant(old) != std::mem::discriminant(&event));
            } else if matches!(
                &event,
                WebViewEvent::LoadFinished
                    | WebViewEvent::LoadFinishedAt { .. }
                    | WebViewEvent::LoadFailed { .. }
            ) {
                events.retain(|old| {
                    !matches!(
                        old,
                        WebViewEvent::LoadFinished
                            | WebViewEvent::LoadFinishedAt { .. }
                            | WebViewEvent::LoadFailed { .. }
                    )
                });
            }
            if events.len() >= 512
                && let Some(index) = events.iter().position(|old| {
                    !matches!(
                        old,
                        WebViewEvent::LoadFinished
                            | WebViewEvent::LoadFinishedAt { .. }
                            | WebViewEvent::LoadFailed { .. }
                            | WebViewEvent::NavigationOutcome { .. }
                    )
                })
            {
                events.remove(index);
            }
            events.push_back(event);
        }
        self.wake();
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        self.supervisor_bell.ring();
    }

    fn fail(&self, message: String) {
        if !self.stopping.load(Ordering::Acquire) && !self.failed.swap(true, Ordering::AcqRel) {
            self.fail_navigations(&message);
            self.text_input.lock_recover().unavailable();
            let uri = self.uri.lock().map(|uri| uri.clone()).unwrap_or_default();
            self.event(WebViewEvent::LoadFailed {
                uri,
                error: message,
            });
            let (reported, queued) = &self.failure_reported;
            *reported.lock_recover() = true;
            queued.notify_all();
            self.supervisor_bell.ring();
        }
    }

    /// Whether the host failed, waiting for the failure's report to be
    /// queued when another thread is still making it: the reader (at EOF or
    /// bad output) and the supervisor (at the exit) can both see the same
    /// failure, and the supervisor must not reap the host before its
    /// failure is reported. The report never waits on the supervisor, so
    /// this wait is bounded by the reporting thread's own work.
    fn failure_reported(&self) -> bool {
        if !self.failed.load(Ordering::Acquire) {
            return false;
        }
        let (reported, queued) = &self.failure_reported;
        let mut done = reported.lock_recover();
        while !*done {
            done = queued
                .wait(done)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        true
    }
}

/// A real Chromium process. Initialization errors are returned to the caller;
/// this type never creates a simulated engine or weakens Chromium's sandbox.
pub struct ChromiumEngine {
    tx: Arc<CommandQueue>,
    shared: Arc<Shared>,
    runtime: Arc<Runtime>,
    tab: u32,
}

type Routes = Arc<Mutex<HashMap<u32, Weak<Shared>>>>;
/// A private window's disposable root is a temp directory named with this
/// prefix that holds [`PRIVATE_ROOT_MARKER`] reading
/// [`PRIVATE_ROOT_MARKER_CONTENT`]; only such a root is ever removed.
pub const PRIVATE_ROOT_PREFIX: &str = "tilo-browser-private-";
pub const PRIVATE_ROOT_MARKER: &str = ".tilo-private-root-v1";
pub const PRIVATE_ROOT_MARKER_CONTENT: &str = "tilo-browser-owned-private-root-v1\n";
/// The engine helper's pid, written once it runs inside the root.
pub const PRIVATE_ROOT_HELPER: &str = ".tilo-private-helper-pid";
/// Writers hold this file shared and cleanup holds it exclusive.
pub const PRIVATE_ROOT_LOCK: &str = ".tilo-private-root.lock";

struct PrivateRootCleanup(Option<PathBuf>);
impl PrivateRootCleanup {
    fn disarm(&mut self) {
        self.0 = None;
    }
}
impl Drop for PrivateRootCleanup {
    fn drop(&mut self) {
        if let Some(root) = self.0.take() {
            cleanup_private_root(&root);
        }
    }
}

/// Whether `root` is a private window root Browser allocated: a real
/// directory with the prefix and an intact marker, never a symlink.
pub fn is_owned_private_root(root: &Path) -> bool {
    std::fs::symlink_metadata(root)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        && root
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.starts_with(PRIVATE_ROOT_PREFIX))
        && std::fs::symlink_metadata(root.join(PRIVATE_ROOT_MARKER)).is_ok_and(|metadata| {
            metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 64
        })
        && std::fs::read_to_string(root.join(PRIVATE_ROOT_MARKER))
            .is_ok_and(|value| value == PRIVATE_ROOT_MARKER_CONTENT)
}

/// Remove a private window's disposable root that no engine will use, if it
/// is one Browser owns.
pub fn release_private_root(root: &Path) {
    if is_owned_private_root(root) {
        cleanup_private_root(root);
    }
}

fn cleanup_private_root(root: &Path) {
    #[cfg(unix)]
    let _lock = {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(PRIVATE_ROOT_LOCK))
        {
            Ok(file) => {
                let Ok(lock) = advisory_lock::FileLock::exclusive(file) else {
                    tracing::warn!(path = %root.display(), "Could not lock private browser root for cleanup");
                    return;
                };
                Some(lock)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::warn!(%error, path = %root.display(), "Could not open private browser root lock");
                return;
            }
        }
    };
    if is_owned_private_root(root)
        && let Err(error) = std::fs::remove_dir_all(root)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %root.display(), "Could not remove private browser root");
    }
}

fn record_private_helper(root: &Path, pid: u32) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(
        &mut options.open(root.join(PRIVATE_ROOT_HELPER))?,
        pid.to_string().as_bytes(),
    )
}

struct Runtime {
    tx: Arc<CommandQueue>,
    process: Arc<Shared>,
    supervisor: Mutex<Option<std::thread::JoinHandle<()>>>,
    routes: Routes,
    next_tab: AtomicU32,
    download_events: Arc<Mutex<VecDeque<WebViewEvent>>>,
    /// The profile socket this process serves its host on. Shared with the
    /// host's supervisor, which publishes it without holding the runtime:
    /// a strong reference on the supervisor could be the last one, and the
    /// supervisor cannot wait for itself.
    server: Arc<std::sync::OnceLock<Arc<shared::Server>>>,
    /// Set when another process's host runs this profile.
    link: Option<shared::ClientLink>,
    service_session: Option<String>,
    exited: Arc<AtomicBool>,
}
impl Runtime {
    /// Reserves a tab within the host's shared capacity and asks the host to
    /// create it. `announce` runs before any host event for the tab can be
    /// routed.
    fn admit_tab(&self, shared: &Arc<Shared>, announce: impl FnOnce(u32)) -> Result<u32, String> {
        let mut routes = self.routes.lock().map_err(|_| "Tab routes unavailable")?;
        if routes.len() >= 32 {
            return Err("At most 32 tabs can be open".into());
        }
        let tab = self.next_tab.fetch_add(1, Ordering::Relaxed);
        if tab > i32::MAX as u32 {
            return Err("Tab identity exhausted".into());
        }
        routes.insert(tab, Arc::downgrade(shared));
        if let Err(error) = self.tx.push(json!({"op":"create_tab","tab":tab})) {
            routes.remove(&tab);
            return Err(error.into());
        }
        announce(tab);
        drop(routes);
        Ok(tab)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.process.stop();
        if self.link.is_some() {
            // The command writer sends what is queued, then hangs up; the
            // owner closes whatever this process left open.
            self.tx.close();
            return;
        }
        if let Some(server) = self.server.get() {
            server.stop();
        }
        let _ = self.tx.push(json!({"op":"shutdown"}));
        self.tx.close();
        if let Some(supervisor) = self.supervisor.lock_recover().take()
            && supervisor.thread().id() != std::thread::current().id()
        {
            let _ = supervisor.join();
        }
    }
}
fn queue_download_event(queue: &Mutex<VecDeque<WebViewEvent>>, event: WebViewEvent) {
    let mut events = queue.lock_recover();
    if let WebViewEvent::DownloadUpdated { id, .. } = &event {
        events.retain(
            |old| !matches!(old, WebViewEvent::DownloadUpdated { id: old_id, .. } if old_id == id),
        );
    } else if matches!(event, WebViewEvent::DownloadRuntime { .. }) {
        events.retain(|old| !matches!(old, WebViewEvent::DownloadRuntime { .. }));
    }
    if events.len() >= 256 {
        events.pop_front();
    }
    events.push_back(event);
}

fn fail_routes(routes: &Routes, message: &str) {
    let endpoints: Vec<_> = routes
        .lock_recover()
        .values()
        .filter_map(Weak::upgrade)
        .collect();
    for shared in endpoints {
        shared.fail(message.to_owned());
    }
}
fn route(routes: &Routes, id: u32) -> Option<Arc<Shared>> {
    routes.lock().ok()?.get(&id)?.upgrade()
}
/// A client tab's frame goes on to its process as the host sent it; only a
/// frame drawn here is converted.
fn deliver_frame(routes: &Routes, tab: u32, frame: protocol::WireFrame) {
    if let Some(shared) = route(routes, tab) {
        shared.frame_asked.store(false, Ordering::Relaxed);
        if let Some(outbox) = &shared.remote {
            outbox.frame(tab, frame);
            return;
        }
        present_frame(&shared, tab, frame.into_frame(tab));
    }
}

/// What changed of a tab's frame goes on to a client tab's process as the
/// host sent it, and is applied to the tab's frame here otherwise. Where the
/// frame it changes is missing, the host is asked, once, for it whole.
fn deliver_damage(routes: &Routes, tab: u32, damage: damage::WireDamage, tx: &CommandQueue) {
    let Some(shared) = route(routes, tab) else {
        return;
    };
    let delivered = match &shared.remote {
        Some(outbox) => outbox.damage(tab, damage),
        None => present_damage(&shared, tab, damage),
    };
    if !delivered && !shared.frame_asked.swap(true, Ordering::Relaxed) {
        let _ = tx.push(json!({"op":"resend_frame","tab":tab}));
    }
}

/// Makes `frame` the tab's newest and wakes the view, unless it shows exactly
/// what the tab already has.
fn present_frame(shared: &Shared, tab: u32, mut frame: super::Frame) {
    // Comparing a full-resolution frame may scan tens of megabytes. The UI
    // takes the slot's lock while handling input and drawing; the store's
    // is this thread's alone.
    let changed = shared.frame_store.lock_recover().replace(&mut frame);
    if let Ok(mut slot) = shared.frames.lock() {
        if !changed && let Some(current) = slot.as_ref() {
            super::frame_trace::unchanged(tab, frame.trace_id, current.trace_id);
            return;
        }
        // A view whose frame was cleared needs this one, even unchanged.
        *slot = Some(frame);
    }
    shared.wake();
}

/// Applies `damage` to the tab's frame and shows the result, as
/// [`present_frame`] shows a whole frame. Returns `false` when there is no
/// frame for it to change.
fn present_damage(shared: &Shared, tab: u32, damage: damage::WireDamage) -> bool {
    let mut store = shared.frame_store.lock_recover();
    let frame = match store.apply(tab, damage) {
        damage::Applied::Missing => return false,
        damage::Applied::Changed(frame) => frame,
        damage::Applied::Unchanged => {
            let cleared = shared.frames.lock().is_ok_and(|slot| slot.is_none());
            match store.frame(None) {
                Some(frame) if cleared => frame,
                _ => return true,
            }
        }
    };
    drop(store);
    if let Ok(mut slot) = shared.frames.lock() {
        *slot = Some(frame);
    }
    shared.wake();
    true
}

/// Error-only surface used when the engine is not installed or cannot start.
/// It never renders a page and answers navigation with its message, which is
/// written for the person using the app.
pub struct UnavailableEngine {
    message: String,
    shared: Shared,
}

impl UnavailableEngine {
    pub fn new(message: String) -> Arc<Self> {
        Arc::new(Self {
            message,
            shared: Shared {
                audio: audio::PageAudio::default(),
                text_input: Mutex::default(),
                native_edit_history: AtomicBool::new(false),
                guarded_edit_history: AtomicBool::new(false),
                native_caret_text_input: AtomicBool::new(false),
                document_size: DocumentSizeReports::default(),
                selection_text: AtomicBool::new(false),
                open_tabs: Mutex::default(),
                popups_can: AtomicBool::default(),
                popups_asked: AtomicBool::default(),
                popups_told: AtomicBool::default(),
                dialogs_can: AtomicBool::default(),
                dialogs_asked: AtomicBool::default(),
                dialogs_told: AtomicBool::default(),
                popups: Mutex::default(),
                unadopted: Mutex::default(),
                cursor: Mutex::default(),
                frames: Arc::new(Mutex::new(None)),
                frame_store: Mutex::default(),
                frame_asked: AtomicBool::new(false),
                events: Mutex::new(VecDeque::new()),
                waker: Mutex::new(None),
                uri: Mutex::new(String::new()),
                navigations: Mutex::default(),
                navigation_commands: Mutex::default(),
                stopping: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                failure_reported: (Mutex::new(false), std::sync::Condvar::new()),
                supervisor_bell: SupervisorBell::default(),
                supervisor_checks: AtomicU32::new(0),
                #[cfg(test)]
                supervisor_untimed_waits: AtomicU32::new(0),
                #[cfg(test)]
                supervisor_thread: std::sync::OnceLock::new(),
                remote: None,
            },
        })
    }
}

impl WebViewEngine for UnavailableEngine {
    fn navigate_owned(&self, _request_id: u32, _command: NavigationCommand) -> Result<(), String> {
        Err(self.message.clone())
    }
    fn send(&self, command: WebViewCommand) {
        match command {
            WebViewCommand::Navigate(uri) => self.shared.event(WebViewEvent::LoadFailed {
                uri,
                error: self.message.clone(),
            }),
            WebViewCommand::Page { request_id, .. } => {
                self.shared.event(WebViewEvent::PageResult {
                    request_id,
                    result: Err(super::PageFailure::new("unavailable", self.message.clone())),
                })
            }
            WebViewCommand::PaintCapture { request_id } => {
                self.shared.event(WebViewEvent::PaintSnapshot {
                    request_id,
                    snapshot: None,
                });
            }
            WebViewCommand::PaintResource { request_id, .. } => {
                self.shared.event(WebViewEvent::PaintResource {
                    request_id,
                    bytes: None,
                });
            }
            _ => {}
        }
    }
    fn frames(&self) -> FrameSlot {
        self.shared.frames.clone()
    }
    fn drain_events(&self) -> Vec<WebViewEvent> {
        self.shared
            .events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }
    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut slot) = self.shared.waker.lock() {
            *slot = Some(Arc::from(waker));
        }
    }
}

fn host_path() -> Result<PathBuf, String> {
    runtime::locate().map_err(|missing| {
        tracing::warn!(hint = %missing.developer_hint(), "Chromium engine runtime missing");
        runtime::NOT_INSTALLED_MESSAGE.to_owned()
    })
}

/// Every thread the Chromium transport runs is named and spawned here.
fn spawn_named(
    name: &str,
    work: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(work)
}

/// The profile directory, created and canonicalized: the host requires an
/// absolute profile, and every process must name one profile identically.
fn prepare_profile(profile: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(profile).map_err(|error| format!("Chromium profile: {error}"))?;
    profile.canonicalize().map_err(|error| error.to_string())
}

fn spawn_command_writer(
    stdin: std::process::ChildStdin,
    rx: Arc<CommandQueue>,
    writer_shared: Arc<Shared>,
    writer_routes: Routes,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    spawn_named("chromium-commands", move || {
        let mut writer = stdin;
        while let Some(mut command) = rx.pop() {
            let result = protocol::write_json(&mut writer, &command);
            wipe_service_wire(&mut command);
            if let Err(error) = result {
                writer_shared.fail(format!("Chromium command channel closed: {error}"));
                fail_routes(
                    &writer_routes,
                    &format!("Chromium command channel closed: {error}"),
                );
                break;
            }
        }
    })
}

fn spawn_frame_reader(
    stdout: std::process::ChildStdout,
    reader_shared: Arc<Shared>,
    reader_routes: Routes,
    download_events: Arc<Mutex<VecDeque<WebViewEvent>>>,
    ready_tx: std::sync::mpsc::SyncSender<()>,
    hub: Arc<shared::Hub>,
    tx: Arc<CommandQueue>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    spawn_named("chromium-frames", move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match protocol::read_packet(&mut reader) {
                Ok(Some(protocol::Packet::Frame(frame))) => {
                    deliver_frame(&reader_routes, 1, frame);
                }
                Ok(Some(protocol::Packet::TabFrame(tab, frame))) => {
                    deliver_frame(&reader_routes, tab, frame);
                }
                Ok(Some(protocol::Packet::TabDamage(tab, damage))) => {
                    deliver_damage(&reader_routes, tab, damage, &tx);
                }
                Ok(Some(protocol::Packet::Json(value))) => {
                    if let Err(error) =
                        route_native_event(&value, &reader_routes, &download_events, &ready_tx)
                    {
                        reader_shared.fail(format!("Invalid Chromium event: {error}"));
                        break;
                    }
                    if matches!(
                        value.get("event").and_then(Value::as_str),
                        Some("download_prompt" | "download" | "download_runtime" | "ready")
                    ) {
                        hub.broadcast(&value);
                    }
                }
                Ok(None) => {
                    reader_shared.fail("Chromium engine disconnected".into());
                    break;
                }
                Err(error) => {
                    reader_shared.fail(format!("Invalid Chromium output: {error}"));
                    break;
                }
            }
        }
        queue_download_event(
            &download_events,
            WebViewEvent::DownloadRuntime {
                admitted: 0,
                detached: 0,
                accepting: false,
                error: "Chromium engine disconnected".into(),
            },
        );
        fail_routes(&reader_routes, "Chromium engine disconnected");
        hub.close_all();
    })
}

fn take_supervisor_child(slot: &Mutex<Option<std::process::Child>>) -> Option<std::process::Child> {
    slot.lock_recover().take()
}

fn supervise_child(
    mut child: std::process::Child,
    supervisor_shared: &Shared,
    supervisor_queue: &CommandQueue,
    supervisor_private_root: Option<&Path>,
) -> bool {
    #[cfg(test)]
    if let Ok(tid) = tilo_test_env::idle::current_thread_id() {
        let _ = supervisor_shared.supervisor_thread.set(tid);
    }
    // The bell hangs before the first look, so a change after it rings.
    let (watch, bell) = tilo_process::child::ExitWatch::new(&child);
    supervisor_shared.supervisor_bell.hang(bell);
    let mut shutdown_started = None;
    let exited = loop {
        supervisor_shared
            .supervisor_checks
            .fetch_add(1, Ordering::Relaxed);
        match child.try_wait() {
            Ok(Some(_)) => {
                supervisor_shared.fail("Chromium engine exited".into());
                supervisor_shared.failure_reported();
                break true;
            }
            Err(error) => {
                supervisor_shared.fail(format!("Chromium process status: {error}"));
                let _ = child.kill();
                break child.wait().is_ok();
            }
            Ok(None) => {}
        }
        if supervisor_shared.failure_reported() {
            supervisor_queue.close();
            let _ = child.kill();
            break child.wait().is_ok();
        }
        let mut deadline = None;
        if supervisor_shared.stopping.load(Ordering::Acquire) {
            let began = shutdown_started.get_or_insert_with(crate::time::now);
            let Some(left) = HOST_SHUTDOWN_GRACE.checked_sub(began.elapsed()) else {
                let _ = child.kill();
                break child.wait().is_ok();
            };
            deadline = Some(left);
        }
        #[cfg(test)]
        if deadline.is_none() {
            supervisor_shared
                .supervisor_untimed_waits
                .fetch_add(1, Ordering::Release);
        }
        watch.wait(deadline);
    };
    supervisor_queue.close();
    if exited && let Some(root) = supervisor_private_root {
        cleanup_private_root(root);
    }
    exited
}

/// How long a host asked to shut down has to exit before it is killed.
const HOST_SHUTDOWN_GRACE: Duration = Duration::from_secs(35);

/// Wakes a host's supervisor when what it acts on changes: the host failing
/// or a shutdown starting. With the host's own exit, which the supervisor's
/// [`ExitWatch`](tilo_process::child::ExitWatch) sees, that is everything it
/// waits for, so a running Browser costs no timer wakeups.
#[derive(Default)]
struct SupervisorBell(Mutex<Option<tilo_process::child::Bell>>);

impl SupervisorBell {
    fn ring(&self) {
        if let Some(bell) = self.0.lock_recover().as_ref() {
            bell.ring();
        }
    }

    fn hang(&self, bell: tilo_process::child::Bell) {
        *self.0.lock_recover() = Some(bell);
    }
}

fn validated_extension_paths(extensions: &[PathBuf]) -> Result<Vec<String>, String> {
    if extensions.len() > 100 {
        return Err("Too many approved extensions".into());
    }
    extensions
        .iter()
        .map(|path| {
            let path = path.canonicalize().map_err(|error| error.to_string())?;
            let value = path.to_str().ok_or("Invalid extension path")?;
            if value.contains(',') || value.len() > 4096 {
                return Err("Unsupported extension path".into());
            }
            Ok(value.to_owned())
        })
        .collect::<Result<Vec<String>, String>>()
}

/// The engine host process. It is not one of the toolkit's thin binaries: it
/// loads CEF from beside itself and the rest from the system, so the library
/// path the launcher points at the app's own runtime bundle does not apply to
/// it. Nor does the hardened allocator the launcher preloads into the app:
/// Chromium brings its own (PartitionAlloc) and frees through glibc's
/// internals, so a preloaded allocator aborts the host at start.
use browser_continuity_core::engine_host::command as host_command;

#[cfg(all(test, target_os = "linux"))]
mod engine_exclusion_tests;

/// Engine audio switches.
///
/// A governed (sandboxed) app is given no PipeWire/Pulse socket and no
/// `/dev/snd`; its sound goes through the host's output service, which
/// Chromium cannot speak. `--tilo-audio-bridge` makes the host take every
/// page's PCM itself (CEF's audio handler) and send it here, to play through
/// that service (see `audio`). Chromium then opens no device, so it does not
/// probe the PulseAudio and ALSA endpoints the sandbox withholds; while the
/// service is unavailable the PCM is dropped, which keeps media
/// playing silently as the fake audio manager would. An app that is not
/// governed keeps Chromium's own audio.
fn host_audio_switches(governed: bool) -> &'static [&'static str] {
    if governed {
        &["--tilo-audio-bridge"]
    } else {
        &[]
    }
}

/// What a host is started for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostMode<'a> {
    /// Browsing: the Browser's profile, shared by its windows.
    Browsing,
    /// One consented service session's dedicated profile.
    ServiceSession(&'a str),
    /// Sealed documents (a mail): pages never navigate; a followed link is
    /// a new-tab request for the embedder.
    SealedDocuments,
}

impl HostMode<'_> {
    /// The host runs its profile alone: no other process joins it.
    fn alone(self) -> bool {
        !matches!(self, Self::Browsing)
    }

    fn service_session(self) -> Option<String> {
        match self {
            Self::ServiceSession(session) => Some(session.to_owned()),
            Self::Browsing | Self::SealedDocuments => None,
        }
    }
}

fn launch_host(
    host: &Path,
    profile: &Path,
    extensions: &[String],
    private_root: Option<&Path>,
    mode: HostMode<'_>,
) -> Result<std::process::Child, String> {
    let mut command =
        host_command(host).map_err(|error| format!("Could not confine Chromium: {error}"))?;
    command.args(host_audio_switches(media::audio_output::is_governed()));
    // A frame's changed rects, not the frame again (`damage`).
    command.arg("--tilo-frame-damage");
    if private_root.is_some() {
        command.arg("--private");
    }
    match mode {
        HostMode::ServiceSession(session) => {
            command.arg(format!("--service-session={session}"));
        }
        HostMode::SealedDocuments => {
            command.arg("--tilo-sealed-documents");
        }
        HostMode::Browsing => {}
    }
    if !extensions.is_empty() {
        command.arg(format!("--load-extension={}", extensions.join(",")));
    }
    let mut child = command
        .arg(format!("--profile={}", profile.display()))
        .arg(format!(
            "--downloads={}",
            tilo_paths::download_dir()
                .unwrap_or_else(|| profile.join("downloads"))
                .display()
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("Could not start Chromium at {}: {error}", host.display()))?;
    if let Some(root) = private_root
        && let Err(error) = record_private_helper(root, child.id())
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "Could not record private Chromium process: {error}"
        ));
    }
    Ok(child)
}

/// What a starting host's supervisor needs: it serves the profile to other
/// Browser processes, waits for the host to report ready and watches it until
/// it exits.
struct Starter {
    child: Arc<Mutex<Option<std::process::Child>>>,
    ready: std::sync::mpsc::Receiver<()>,
    claim: Option<shared::Claim>,
    runtime: Weak<Runtime>,
    published: Arc<std::sync::OnceLock<Arc<shared::Server>>>,
    hub: Arc<shared::Hub>,
    shared: Arc<Shared>,
    queue: Arc<CommandQueue>,
    private_root: Option<PathBuf>,
    exited: Arc<AtomicBool>,
}

impl Starter {
    fn run(self) {
        let Some(mut child) = take_supervisor_child(&self.child) else {
            self.shared
                .fail("Chromium supervisor lost process ownership".into());
            self.queue.close();
            return;
        };
        // Other Browser windows of the profile join at once rather than
        // waiting for the host: their tabs queue behind this one's.
        let served = self
            .claim
            .as_ref()
            .map(|claim| claim.serve(self.runtime.clone(), &self.published, &self.hub))
            .transpose();
        let server = served.as_ref().ok().cloned().flatten();
        let started = served.and_then(|_| {
            self.ready
                .recv_timeout(Duration::from_secs(20))
                .map_err(|error| format!("Chromium did not initialize: {error}"))
        });
        if let Err(error) = started {
            // A host that exited first has already said why.
            self.shared.fail(error);
            self.queue.close();
            let _ = child.kill();
            let exited = child.wait().is_ok();
            self.exited.store(exited, Ordering::Release);
            if exited && let Some(root) = &self.private_root {
                cleanup_private_root(root);
            }
        } else {
            let exited = supervise_child(
                child,
                &self.shared,
                &self.queue,
                self.private_root.as_deref(),
            );
            self.exited.store(exited, Ordering::Release);
        }
        if self.exited.load(Ordering::Acquire)
            && let Some(claim) = self.claim
        {
            claim.release_after_exit(server.as_ref());
        }
    }
}

fn valid_context_menu(value: &Value) -> bool {
    valid_id(value)
        && value["link"].as_str().is_some_and(|s| s.len() <= 8192)
        && ["x", "y"].iter().all(|k| {
            value[*k]
                .as_f64()
                .is_some_and(|v| v.is_finite() && v.abs() < 100_000.0)
        })
        && decode_context_items(&value["items"], 1, &mut { MENU_ROWS }).is_some()
}

impl ChromiumEngine {
    /// Start the packaged runtime and persistent local profile.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime/profile cannot be located or prepared,
    /// or the host cannot be started. A host that starts but never becomes
    /// ready fails the page later, as a [`WebViewEvent::LoadFailed`].
    pub fn new() -> Result<Arc<Self>, String> {
        Self::new_with_extensions(&[])
    }

    ///
    /// # Errors
    ///
    /// Returns an error for invalid extensions, unavailable runtime/profile, or a helper that cannot be started.
    pub fn new_with_extensions(extensions: &[PathBuf]) -> Result<Arc<Self>, String> {
        let profile = std::env::var_os("TILO_CHROMIUM_PROFILE")
            .map(PathBuf::from)
            .or_else(|| tilo_paths::data_dir().map(|path| path.join("tilo-browser/chromium")))
            .ok_or("Cannot locate Chromium profile directory")?;
        Self::with_host_extensions(
            &host_path()?,
            &profile,
            extensions,
            None,
            HostMode::Browsing,
        )
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the private root is not owned, the runtime is missing, or the helper cannot be started.
    pub fn new_private(root: &Path) -> Result<Arc<Self>, String> {
        match host_path() {
            Ok(host) => Self::with_private_host(&host, root),
            Err(error) => {
                cleanup_private_root(root);
                Err(error)
            }
        }
    }

    /// Explicit profile with the packaged helper and approved extensions.
    pub fn with_profile_extensions(
        profile: &Path,
        extensions: &[PathBuf],
    ) -> Result<Arc<Self>, String> {
        Self::with_host_extensions(&host_path()?, profile, extensions, None, HostMode::Browsing)
    }

    /// Explicit runtime/profile paths for packaging and isolated live tests.
    ///
    /// # Errors
    ///
    /// Returns an error if the profile cannot be prepared or the helper cannot start.
    pub fn with_host(host: &Path, profile: &Path) -> Result<Arc<Self>, String> {
        Self::with_host_extensions(host, profile, &[], None, HostMode::Browsing)
    }

    /// Start an incognito helper whose complete installation root is removed
    /// by the process supervisor only after the native child has exited.
    ///
    /// # Errors
    ///
    /// Returns an error if the private root is not owned, the profile cannot be prepared, or the helper cannot start.
    pub fn with_private_host(host: &Path, root: &Path) -> Result<Arc<Self>, String> {
        if !is_owned_private_root(root) {
            return Err("Private Chromium root is not owned by Browser".into());
        }
        Self::with_host_extensions(
            host,
            &root.join("chromium"),
            &[],
            Some(root.to_owned()),
            HostMode::Browsing,
        )
    }

    /// A dedicated persistent profile for one explicitly consented service session.
    /// The caller owns a private, identity-bound profile directory; this never
    /// changes the normal Browser profile or accepts an unbound import.
    pub fn with_service_session(profile: &Path, session_id: &str) -> Result<Arc<Self>, String> {
        if !browser_continuity_core::sessions::valid_id(session_id) {
            return Err("Selected website session is unavailable".into());
        }
        Self::with_host_extensions(
            &host_path()?,
            profile,
            &[],
            None,
            HostMode::ServiceSession(session_id),
        )
    }

    /// A host for sealed documents over `profile`, its own: a page never
    /// navigates, a followed link comes back as a new-tab request
    /// ([`super::WebViewEngine::take_new_tab_requests`]), and the address
    /// under the pointer as [`super::WebViewEvent::LinkHovered`]. For a mail.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime is missing, the profile cannot be
    /// prepared, or the host cannot start.
    pub fn sealed_documents(profile: &Path) -> Result<Arc<Self>, String> {
        Self::with_host_extensions(&host_path()?, profile, &[], None, HostMode::SealedDocuments)
    }

    /// Admit private credentials only to the exact dedicated session host.
    pub fn import_service_session(
        &self,
        request: super::ServiceSessionRequest,
    ) -> Result<(), String> {
        if self.runtime.service_session.as_deref() != Some(request.session.session_id.as_str())
            || self.runtime.link.is_some()
            || self.tab != 1
            || !request.valid()
        {
            return Err("Selected website session cannot be imported into this profile".into());
        }
        let mut command = request
            .encode()
            .ok_or("Selected website session is unavailable")?;
        command["tab"] = json!(self.tab);
        self.tx.push(command).map_err(str::to_owned)
    }

    /// Stop this dedicated service host and confirm its actual child was reaped.
    /// Unrelated shared/default Browser hosts are never eligible for this operation.
    pub fn shutdown_service_session(&self) -> Result<(), String> {
        if self.runtime.service_session.is_none() || self.runtime.link.is_some() {
            return Err("This engine does not own a selected service profile".into());
        }
        self.runtime.process.stop();
        let _ = self.tx.push(json!({"op":"shutdown"}));
        self.tx.close();
        if let Some(supervisor) = self
            .runtime
            .supervisor
            .lock()
            .map_err(|_| "Engine process ownership unavailable")?
            .take()
        {
            supervisor
                .join()
                .map_err(|_| "Engine shutdown could not be confirmed")?;
        }
        if !self.runtime.exited.load(Ordering::Acquire) {
            return Err("Engine shutdown could not be confirmed".into());
        }
        Ok(())
    }

    fn with_host_extensions(
        host: &Path,
        profile: &Path,
        extensions: &[PathBuf],
        private_root: Option<PathBuf>,
        mode: HostMode<'_>,
    ) -> Result<Arc<Self>, String> {
        let extensions = validated_extension_paths(extensions)?;
        let mut cleanup_on_error = PrivateRootCleanup(private_root.clone());
        // A host needs its profile to itself: join the host already running
        // it, or become the process that runs it.
        let profile = &prepare_profile(profile)?;
        let claim = if mode.alone() {
            None
        } else {
            match shared::claim(profile)? {
                shared::Role::Owner(claim) => Some(claim),
                shared::Role::Alone => None,
                shared::Role::Client(stream) => {
                    cleanup_on_error.disarm();
                    return shared::connect(stream);
                }
                shared::Role::Busy(error) => {
                    // Another process may still be using the private root.
                    cleanup_on_error.disarm();
                    return Err(error);
                }
            }
        };
        let mut child = launch_host(host, profile, &extensions, private_root.as_deref(), mode)?;
        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Chromium stdin unavailable".into());
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Chromium stdout unavailable".into());
        };
        let shared = Arc::new(Shared::default());
        let tx = Arc::new(CommandQueue::default());
        let routes: Routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
        let download_events = Arc::new(Mutex::new(VecDeque::new()));
        let runtime = Arc::new(Runtime {
            tx: tx.clone(),
            process: shared.clone(),
            supervisor: Mutex::new(None),
            routes: routes.clone(),
            next_tab: AtomicU32::new(2),
            download_events: download_events.clone(),
            server: Arc::default(),
            link: None,
            service_session: mode.service_session(),
            exited: Arc::new(AtomicBool::new(false)),
        });
        let hub = Arc::new(shared::Hub::default());
        let rx = tx.clone();
        let (ready_tx, ready_rx) = sync_channel(1);
        let writer_shared = shared.clone();
        let writer_routes = routes.clone();
        if let Err(error) = spawn_command_writer(stdin, rx, writer_shared, writer_routes) {
            tx.close();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        let reader_shared = shared.clone();
        let reader_routes = routes;
        if let Err(error) = spawn_frame_reader(
            stdout,
            reader_shared,
            reader_routes,
            download_events,
            ready_tx,
            hub.clone(),
            tx.clone(),
        ) {
            tx.close();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        // The host takes a second or more to initialize, longer on a loaded
        // machine. Its window does not wait for it: commands queue until the
        // host reads them, and the profile socket, the host's readiness and
        // its supervision follow on a thread of their own. A host that never
        // becomes ready fails the page as one that dies later does.
        let child = Arc::new(Mutex::new(Some(child)));
        let wait_for_private_cleanup =
            private_root.is_some() || matches!(mode, HostMode::ServiceSession(_));
        let starter = Starter {
            child: child.clone(),
            ready: ready_rx,
            claim,
            runtime: Arc::downgrade(&runtime),
            published: runtime.server.clone(),
            hub,
            shared: shared.clone(),
            queue: tx.clone(),
            private_root: private_root.clone(),
            exited: runtime.exited.clone(),
        };
        let supervisor = match spawn_named("chromium-supervisor", move || starter.run()) {
            Ok(supervisor) => supervisor,
            Err(error) => {
                if let Some(mut child) = take_supervisor_child(&child) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                tx.close();
                return Err(format!("Chromium supervisor: {error}"));
            }
        };
        if wait_for_private_cleanup {
            *runtime.supervisor.lock_recover() = Some(supervisor);
        }
        cleanup_on_error.disarm();
        Ok(Arc::new(Self {
            tx,
            shared,
            runtime,
            tab: 1,
        }))
    }
}

impl ChromiumEngine {
    /// Tell the host what this tab last asked of document size reports,
    /// once the host said it can measure.
    /// Tell the host this tab takes its popups, once it asked and the host
    /// said it can.
    fn tell_popups(&self) {
        let shared = &self.shared;
        if !shared.popups_asked.load(Ordering::Acquire)
            || !shared.popups_can.load(Ordering::Acquire)
            || shared.popups_told.swap(true, Ordering::AcqRel)
        {
            return;
        }
        if self
            .tx
            .push(json!({"op":"popups","tab":self.tab,"enabled":true}))
            .is_err()
        {
            shared.popups_told.store(false, Ordering::Release);
        }
    }

    /// Tell the host this tab answers the page's questions, once it asked
    /// and the host said it can.
    fn tell_dialogs(&self) {
        let shared = &self.shared;
        if !shared.dialogs_asked.load(Ordering::Acquire)
            || !shared.dialogs_can.load(Ordering::Acquire)
            || shared.dialogs_told.swap(true, Ordering::AcqRel)
        {
            return;
        }
        if self
            .tx
            .push(json!({"op":"js_dialogs","tab":self.tab,"enabled":true}))
            .is_err()
        {
            shared.dialogs_told.store(false, Ordering::Release);
        }
    }

    fn tell_document_size(&self) {
        let reports = &self.shared.document_size;
        let asked = reports.asked.load(Ordering::Acquire);
        if asked == 0
            || !reports.can.load(Ordering::Acquire)
            || reports.told.swap(asked, Ordering::AcqRel) == asked
        {
            return;
        }
        if self
            .tx
            .push(json!({"op":"document_size","tab":self.tab,"enabled":asked == 2}))
            .is_err()
        {
            reports.told.store(0, Ordering::Release);
        }
    }
}

impl WebViewEngine for ChromiumEngine {
    fn cursor(&self) -> Option<winit::window::CursorIcon> {
        *self.shared.cursor.lock_recover()
    }

    fn handle_undo_request(&self, request: crate::undo::UndoRequest) -> bool {
        if !self.supports_text_input() || !self.shared.native_edit_history.load(Ordering::Acquire) {
            return false;
        }
        let direction = match request {
            crate::undo::UndoRequest::Undo => "undo",
            crate::undo::UndoRequest::Redo => "redo",
        };
        self.tx
            .push(json!({"op":"edit_history","tab":self.tab,"direction":direction}))
            .is_ok()
    }

    fn measures_document(&self) -> bool {
        self.shared.document_size.can.load(Ordering::Acquire)
    }

    fn supports_guarded_edit_history(&self) -> bool {
        self.supports_text_input() && self.shared.guarded_edit_history.load(Ordering::Acquire)
    }

    fn supports_text_input(&self) -> bool {
        !self.shared.failed.load(Ordering::Acquire) && !self.shared.stopping.load(Ordering::Acquire)
    }

    fn text_input(
        &self,
        request: super::text_input::Request,
    ) -> Option<super::text_input::Receipt> {
        if self.shared.failed.load(Ordering::Acquire)
            || self.shared.stopping.load(Ordering::Acquire)
        {
            return None;
        }
        let (receipt, mut value) = self.shared.text_input.lock().ok()?.admit(request)?;
        value["tab"] = json!(self.tab);
        if value["action"] == "prepare_text_input"
            && self.shared.native_caret_text_input.load(Ordering::Acquire)
        {
            value["native_caret"] = json!(true);
        }
        if self.tx.push(value).is_err() {
            receipt.complete(Err(super::text_input::Failure::Rejected));
        }
        Some(receipt)
    }

    fn navigate_owned(&self, request_id: u32, command: NavigationCommand) -> Result<(), String> {
        if !super::navigation_request_id(request_id) || !command.valid() {
            return Err("Invalid owned navigation request".into());
        }
        // Serialize reservation+enqueue with cancellation; neither waits for
        // CEF work, only the bounded command queue insertion.
        let _ordering = self
            .shared
            .navigation_commands
            .lock()
            .map_err(|_| "Navigation command ordering unavailable")?;
        let uri = match &command {
            NavigationCommand::Navigate { url } => url.clone(),
            _ => self
                .shared
                .uri
                .lock()
                .map(|uri| uri.clone())
                .unwrap_or_default(),
        };
        self.shared.admit_navigation(request_id, uri)?;
        let Some(mut wire) = encode_command(WebViewCommand::ScopedNavigation {
            request_id,
            command,
        }) else {
            self.shared.cancel_navigation(request_id);
            return Err("Invalid internal owned navigation command".into());
        };
        wire["tab"] = json!(self.tab);
        if let Err(error) = self.tx.push(wire) {
            self.shared.cancel_navigation(request_id);
            return Err(format!("Navigation was not queued: {error}"));
        }
        Ok(())
    }
    fn create_tab(&self) -> Result<super::SharedEngine, String> {
        let waker = self
            .shared
            .waker
            .lock()
            .map_err(|_| "Tab wake callback unavailable")?
            .clone();
        let shared = Arc::new(Shared {
            waker: Mutex::new(waker),
            ..Shared::default()
        });
        let tab = match &self.runtime.link {
            Some(link) => link.create_tab(&self.tx, shared.clone())?,
            None => self.runtime.admit_tab(&shared, |_| {})?,
        };
        Ok(Arc::new(Self {
            tx: self.tx.clone(),
            shared,
            runtime: self.runtime.clone(),
            tab,
        }))
    }
    fn set_modifiers(&self, modifiers: super::KeyModifiers) {
        let _ = self.tx.push(json!({"op":"modifiers","tab":self.tab,"ctrl":modifiers.ctrl,"shift":modifiers.shift,"alt":modifiers.alt,"meta":modifiers.meta}));
    }
    fn set_visible(&self, visible: bool) {
        let _ = self
            .tx
            .push(json!({"op":"visibility","tab":self.tab,"visible":visible}));
        // A newly composed WebView needs the retained page pixels immediately.
        // Keep this one frame until CEF asynchronously repaints on activation.
        // The old composition already retained the same Arc-backed pixels.
    }
    fn take_new_tab_requests(&self) -> Vec<(String, bool)> {
        self.shared.open_tabs.lock_recover().drain(..).collect()
    }

    fn accept_popups(&self) {
        self.shared.popups_asked.store(true, Ordering::Release);
        self.tell_popups();
    }

    fn accept_page_dialogs(&self) {
        self.shared.dialogs_asked.store(true, Ordering::Release);
        self.tell_dialogs();
    }

    fn take_popups(&self) -> Vec<super::Popup> {
        self.shared.popups.lock_recover().drain(..).collect()
    }

    fn adopt_popup(&self, popup: u32) -> Result<super::SharedEngine, String> {
        let shared = self
            .shared
            .unadopted
            .lock_recover()
            .remove(&popup)
            .ok_or("The popup is no longer open")?;
        Ok(Arc::new(Self {
            tx: self.tx.clone(),
            shared,
            runtime: self.runtime.clone(),
            tab: popup,
        }))
    }

    fn supports_native_filtering(&self) -> bool {
        true
    }

    fn send(&self, command: WebViewCommand) {
        match command {
            WebViewCommand::ImportServiceSession(request) => {
                let reply = super::ServiceSessionReply {
                    request_id: request.request_id,
                    session_id: request.session.session_id.clone(),
                    service: request.session.service,
                    imported: false,
                    cookie_count: 0,
                    origin_storage_count: 0,
                };
                if self.import_service_session(request).is_err() {
                    self.shared.event(WebViewEvent::ServiceSessionResult(reply));
                }
                return;
            }
            WebViewCommand::ScopedNavigation {
                request_id,
                command,
            } => {
                if let Err(error) = self.navigate_owned(request_id, command) {
                    tracing::error!(request_id, %error, "Owned navigation was not admitted; use navigate_owned to observe refusal");
                }
                return;
            }
            WebViewCommand::CopySelection { request_id } => {
                // An older host cannot read the selection: nothing is copied.
                if self.shared.selection_text.load(Ordering::Acquire) {
                    let _ = self.tx.push(
                        json!({"op":"selection_text","tab":self.tab,"request_id":request_id}),
                    );
                }
                return;
            }
            WebViewCommand::ReportDocumentSize(enabled) => {
                self.shared
                    .document_size
                    .asked
                    .store(if enabled { 2 } else { 1 }, Ordering::Release);
                self.tell_document_size();
                return;
            }
            WebViewCommand::CancelScopedNavigation { request_id } => {
                let _ordering = self.shared.navigation_commands.lock_recover();
                self.shared.cancel_navigation(request_id);
                let error = if super::navigation_request_id(request_id) {
                    self.tx.push(json!({"op":"cancel_navigation","tab":self.tab,"request_id":request_id})).err()
                } else {
                    None
                };
                // Failure wakes arbitrary callers, which may enqueue another
                // command. Preserve ordering without running callbacks under it.
                drop(_ordering);
                if let Some(error) = error {
                    self.shared
                        .fail(format!("Navigation cancellation was not queued: {error}"));
                }
                return;
            }
            _ => {}
        }
        let navigation = match &command {
            WebViewCommand::Navigate(uri) => Some(uri.clone()),
            _ => None,
        };
        if let WebViewCommand::RequestMediaElement { request_id }
        | WebViewCommand::RequestMediaElementById { request_id, .. } = command
        {
            self.shared
                .event(WebViewEvent::MediaElementUnavailable { request_id });
            return;
        }
        if !command_within_limits(&command) {
            if let WebViewCommand::Page { request_id, .. } = command {
                self.shared.event(WebViewEvent::PageResult {
                    request_id,
                    result: Err(super::PageFailure::new(
                        "invalid_request",
                        "The page request is malformed or exceeds its limits.",
                    )),
                });
                return;
            }
            tracing::error!("Rejected oversized Chromium command");
            if let Some(uri) = navigation {
                self.shared.event(WebViewEvent::LoadFailed {
                    uri,
                    error: "Navigation URL exceeds the supported size".into(),
                });
            }
            return;
        }
        let page_request = match &command {
            WebViewCommand::Page { request_id, .. } => Some(*request_id),
            _ => None,
        };
        if let Some(mut command) = encode_command(command) {
            let Some(fields) = command.as_object_mut() else {
                self.shared.fail("Invalid internal Chromium command".into());
                return;
            };
            fields.insert("tab".into(), json!(self.tab));
            if let Err(error) = self.tx.push(command) {
                if let Some(request_id) = page_request {
                    self.shared.event(WebViewEvent::PageResult {
                        request_id,
                        result: Err(super::PageFailure::new(
                            "unavailable",
                            format!("The page request was not queued: {error}"),
                        )),
                    });
                    return;
                }
                // A prior helper error describes its previous URL. Each new
                // rejected navigation still needs an error for the requested URL.
                if navigation.is_none() && self.shared.failed.load(Ordering::Acquire) {
                    return;
                }
                tracing::error!(%error, "Chromium command rejected");
                if let Some(uri) = navigation {
                    self.shared.event(WebViewEvent::LoadFailed {
                        uri,
                        error: format!("Navigation was not queued: {error}"),
                    });
                }
            } else if let Some(uri) = navigation
                && let Ok(mut current) = self.shared.uri.lock()
            {
                *current = uri;
            }
        }
    }

    fn frames(&self) -> FrameSlot {
        self.shared.frames.clone()
    }

    fn drain_events(&self) -> Vec<WebViewEvent> {
        self.tell_document_size();
        self.tell_popups();
        self.tell_dialogs();
        self.shared
            .events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    fn request_close(&self) -> bool {
        if self
            .tx
            .push(json!({"op":"try_close_tab","tab":self.tab}))
            .is_err()
        {
            self.shared.event(WebViewEvent::TabCloseBlocked);
        }
        false
    }

    fn drain_download_events(&self) -> Vec<WebViewEvent> {
        self.runtime
            .download_events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut slot) = self.shared.waker.lock() {
            *slot = Some(Arc::from(waker));
        }
        self.shared.wake();
    }
}

impl Drop for ChromiumEngine {
    fn drop(&mut self) {
        // Reserve native capacity until OnBeforeClose acknowledges destruction.
        // A dead route cannot receive events, but still counts against the bound.
        let retire = {
            let mut routes = self.runtime.routes.lock_recover();
            if let Some(route) = routes.get_mut(&self.tab) {
                *route = Weak::new();
                true
            } else {
                false
            }
        };
        if retire {
            let _ = self.tx.push(json!({"op":"close_tab","tab":self.tab}));
        }
    }
}

fn route_native_event(
    value: &Value,
    routes: &Routes,
    download_events: &Mutex<VecDeque<WebViewEvent>>,
    ready_tx: &std::sync::mpsc::SyncSender<()>,
) -> Result<(), &'static str> {
    validate_event(value)?;
    let event_name = value
        .get("event")
        .and_then(Value::as_str)
        .ok_or("missing event name")?;
    if event_name == "ready" {
        tracing::info!(cef = ?value.get("cef_version"), chromium = ?value.get("chromium_version"), "Chromium engine ready (CPU frame transport)");
        let _ = ready_tx.try_send(());
    }
    if matches!(
        event_name,
        "download_prompt" | "download" | "download_runtime"
    ) {
        if let Some(event) = decode_event(value) {
            let endpoints: Vec<_> = routes
                .lock()
                .map_err(|_| "Tab routes unavailable")?
                .values()
                .filter_map(Weak::upgrade)
                .collect();
            queue_download_event(download_events, event);
            for endpoint in endpoints {
                endpoint.wake();
            }
        }
        return Ok(());
    }
    let tab = u32::try_from(value.get("tab").and_then(Value::as_u64).unwrap_or(1))
        .map_err(|_| "Invalid tab ID")?;
    let retires_route = matches!(event_name, "closed" | "tab_detached" | "tab_create_failed");
    let endpoint = {
        let mut routes = routes.lock().map_err(|_| "Tab routes unavailable")?;
        if retires_route {
            routes.remove(&tab).and_then(|route| route.upgrade())
        } else {
            routes.get(&tab).and_then(Weak::upgrade)
        }
    };
    if let Some(shared) = endpoint {
        if matches!(event_name, "ready" | "tab_ready") {
            shared.native_caret_text_input.store(
                value["native_caret_text_input"].as_bool() == Some(true),
                Ordering::Release,
            );
            shared.guarded_edit_history.store(
                value["guarded_edit_history"].as_bool() == Some(true),
                Ordering::Release,
            );
            shared.native_edit_history.store(
                value["native_edit_history"].as_bool() == Some(true),
                Ordering::Release,
            );
            shared.document_size.can.store(
                value["document_size"].as_bool() == Some(true),
                Ordering::Release,
            );
            shared
                .popups_can
                .store(value["popups"].as_bool() == Some(true), Ordering::Release);
            shared.dialogs_can.store(
                value["js_dialogs"].as_bool() == Some(true),
                Ordering::Release,
            );
            shared.selection_text.store(
                value["selection_text"].as_bool() == Some(true),
                Ordering::Release,
            );
        }
        if event_name == "popup"
            && let Some(popup) = value["popup"]
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
        {
            if shared.remote.is_some() {
                // A popup of another process's tab is that process's: its
                // traffic goes where the tab's goes, until it closes.
                routes
                    .lock()
                    .map_err(|_| "Tab routes unavailable")?
                    .insert(popup, Arc::downgrade(&shared));
            } else {
                open_popup(&shared, popup, value, routes)?;
                return Ok(());
            }
        }
        if let Some(outbox) = &shared.remote {
            outbox.event(value.clone());
            return Ok(());
        }
        if event_name == "open_tab"
            && let Some(url) = value.get("url").and_then(Value::as_str)
        {
            let mut requests = shared
                .open_tabs
                .lock()
                .map_err(|_| "New tab requests unavailable")?;
            if requests.len() < 32 {
                requests.push_back((
                    url.into(),
                    value
                        .get("background")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ));
            }
            drop(requests);
            shared.wake();
        }
        if matches!(event_name, "audio_start" | "audio" | "audio_stop") {
            shared.audio.event(event_name, value);
            return Ok(());
        }
        if event_name == "cursor" {
            // A shape this toolkit has no name for is the arrow.
            *shared.cursor.lock_recover() = value["cursor"]
                .as_str()
                .and_then(|name| name.parse().ok())
                .filter(|icon| *icon != winit::window::CursorIcon::Default);
            return Ok(());
        }
        if matches!(event_name, "closed" | "tab_detached") {
            shared.audio.stop();
        }
        if event_name == "text_input_result" {
            shared.text_input.lock_recover().reply(value);
            shared.wake();
            return Ok(());
        }
        if let Some(event) = decode_event(value) {
            shared.event(event);
        }
    }

    Ok(())
}

// CEF emits byte counts and document counters as JSON doubles. Accept only
// whole, nonnegative values that fit, while preserving exact integer JSON.
fn wire_u64(value: &Value) -> Option<u64> {
    if let Some(integer) = value.as_u64() {
        return Some(integer);
    }
    let number = value.as_f64()?;
    // u64::MAX rounds up to 2^64 in f64; the upper bound must be exclusive.
    if !number.is_finite()
        || !(0.0..18_446_744_073_709_551_616.0).contains(&number)
        || number.fract() != 0.0
    {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "finite integral value is checked in the exclusive u64 range above"
    )]
    let integer = number as u64;
    Some(integer)
}

fn valid_id(value: &Value) -> bool {
    value["id"]
        .as_u64()
        .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
}

fn decode_favicon(value: &Value) -> Option<WebViewEvent> {
    let width = u32::try_from(value["width"].as_u64()?).ok()?;
    let height = u32::try_from(value["height"].as_u64()?).ok()?;
    let png = decode_base64_bounded(value["png"].as_str()?, 256 * 1024)?;
    if width == 0 || height == 0 || width > 64 || height > 64 || png.len() > 256 * 1024 {
        return None;
    }
    Some(WebViewEvent::FaviconChanged {
        page_url: value["page_url"].as_str()?.into(),
        width,
        height,
        png: Arc::new(png),
    })
}

/// A popup `opener`'s page opened: routed to an endpoint of its own at
/// once, which waits to be adopted. One the app never adopts closes with
/// its opener.
fn open_popup(
    opener: &Arc<Shared>,
    popup: u32,
    value: &Value,
    routes: &Routes,
) -> Result<(), &'static str> {
    let mut routes = routes.lock().map_err(|_| "Tab routes unavailable")?;
    let mut unadopted = opener.unadopted.lock_recover();
    // Popups closed before they were adopted no longer count.
    unadopted.retain(|id, shared| {
        routes
            .get(id)
            .and_then(Weak::upgrade)
            .is_some_and(|route| Arc::ptr_eq(&route, shared))
    });
    if unadopted.len() >= 8 {
        return Ok(());
    }
    let shared = Arc::new(Shared {
        waker: Mutex::new(opener.waker.lock_recover().clone()),
        ..Shared::default()
    });
    routes.insert(popup, Arc::downgrade(&shared));
    drop(routes);
    unadopted.insert(popup, shared);
    drop(unadopted);
    let size = |key: &str| u32::try_from(value[key].as_u64().unwrap_or(0)).unwrap_or(0);
    opener.popups.lock_recover().push_back(super::Popup {
        id: popup,
        url: value["url"].as_str().unwrap_or_default().into(),
        width: size("width"),
        height: size("height"),
    });
    opener.wake();
    Ok(())
}

/// An extension's id as Chromium makes it: 32 letters `a` to `p`.
fn extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
}

fn validate_event(value: &Value) -> Result<(), &'static str> {
    if value.get("tab").is_some_and(|tab| {
        !tab.as_u64()
            .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
    }) {
        return Err("Invalid tab ID");
    }
    let event = value
        .get("event")
        .and_then(Value::as_str)
        .ok_or("missing event name")?;
    let text = |key: &str| value.get(key).is_some_and(Value::is_string);
    let valid = match event {
        "credential_result" => super::CredentialReply::decode(value).is_some(),
        "service_session_result" => super::ServiceSessionReply::decode(value).is_some(),
        "site_status" | "site_result" => valid_site_event(value, event),
        "extension_action"
        | "extension_open_popup"
        | "extension_execute_action"
        | "extension_notification"
        | "extension_sign_in"
        | "extension_omnibox_suggestions"
        | "extension_omnibox_default"
        | "extension_side_panel" => decode_extension_event(value).is_some(),
        "extension_menu" => decode_extension_menu(value).is_some(),
        "extension_permission_request" | "extension_permissions" => {
            extension_permissions::decode(value).is_some()
        }
        "devtools_result" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                && value["ok"].is_boolean()
                && value["body"]
                    .as_str()
                    .is_some_and(|body| body.len() <= 1024 * 1024)
        }
        "context_closed" | "permission_closed" | "js_dialog_closed" => valid_id(value),
        "js_dialog" => decode_page_dialog(value).is_some(),
        "context_menu" => valid_context_menu(value),
        "find_result" => {
            valid_id(value)
                && ["count", "active"].iter().all(|key| {
                    value[*key]
                        .as_i64()
                        .is_some_and(|n| (-1..=i64::from(i32::MAX)).contains(&n))
                })
                && value["final"].is_boolean()
        }
        "zoom" => value["level"]
            .as_f64()
            .is_some_and(|level| level.is_finite() && (-5.0..=9.0).contains(&level)),
        "fullscreen" => value["fullscreen"].is_boolean(),
        "audio_start" | "audio" | "audio_stop" => audio::valid_event(event, value),
        "renderer_terminated" => value
            .as_object()
            .is_some_and(|object| object.len() == if value.get("tab").is_some() { 2 } else { 1 }),
        "permission_prompt" => valid_id(value) && text("origin") && text("permissions"),
        "download_prompt" => valid_id(value) && text("url") && text("name"),
        "download" => {
            valid_id(value)
                && text("path")
                && text("name")
                // Any length: a page's own export (a `data:` address) is
                // its whole file. What is kept of it is bounded in decoding.
                && text("url")
                && value["retryable"].is_boolean()
                && text("error")
                && matches!(
                    value["state"].as_str(),
                    Some("progress" | "complete" | "cancelled" | "failed")
                )
                && wire_u64(&value["received"]).is_some()
                && wire_u64(&value["total"]).is_some()
        }
        "download_runtime" => {
            ["admitted", "detached"]
                .iter()
                .all(|key| value[*key].as_u64().is_some_and(|count| count <= 32))
                && value["accepting"].is_boolean()
                && value["error"]
                    .as_str()
                    .is_some_and(|error| error.len() <= 1024)
        }
        "ready" => {
            value
                .get("native_caret_text_input")
                .is_none_or(Value::is_boolean)
                && text("cef_version")
                && text("chromium_version")
                && value
                    .get("guarded_edit_history")
                    .is_none_or(Value::is_boolean)
                && value
                    .get("native_edit_history")
                    .is_none_or(Value::is_boolean)
                && value.get("document_size").is_none_or(Value::is_boolean)
                && value.get("selection_text").is_none_or(Value::is_boolean)
                && value.get("sealed_documents").is_none_or(Value::is_boolean)
        }
        "tab_ready" => {
            value
                .get("native_caret_text_input")
                .is_none_or(Value::is_boolean)
                && value
                    .get("guarded_edit_history")
                    .is_none_or(Value::is_boolean)
                && value
                    .get("native_edit_history")
                    .is_none_or(Value::is_boolean)
                && value.get("document_size").is_none_or(Value::is_boolean)
        }
        "selection_text" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                && value["text"]
                    .as_str()
                    .is_some_and(|text| text.len() <= 1024 * 1024)
        }
        "link_status" => value["url"].as_str().is_some_and(|url| url.len() <= 8192),
        "extension_download_open" => valid_id(value) && value["reveal"].is_boolean(),
        "find_selection" => {
            valid_id(value)
                && ["x", "y", "width", "height"].iter().all(|key| {
                    value[*key]
                        .as_f64()
                        .is_some_and(|n| n.is_finite() && (-1e7..=1e7).contains(&n))
                })
        }
        "document_size" => ["width", "height", "scroll_y"].iter().all(|key| {
            value[*key]
                .as_f64()
                .is_some_and(|n| n.is_finite() && (0.0..=1e7).contains(&n))
        }),
        "open_tab" => {
            value["url"].as_str().is_some_and(|url| url.len() <= 8192)
                && value["background"].is_boolean()
        }
        "popup" => {
            value["popup"]
                .as_u64()
                .is_some_and(|id| id >= u64::from(FIRST_POPUP) && i32::try_from(id).is_ok())
                && value["url"].as_str().is_some_and(|url| url.len() <= 8192)
                && ["width", "height"]
                    .iter()
                    .all(|key| value[*key].as_u64().is_some_and(|n| n <= 10_000))
        }
        "cursor" => value["cursor"]
            .as_str()
            .is_some_and(|name| !name.is_empty() && name.len() <= 64),
        "uri" => text("url"),
        "title" => text("title"),
        "progress" => value["progress"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n)),
        "loaded" => text("url") && value["status"].as_i64().is_some(),
        "navigation_outcome" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                // The frame's address, as long as Chromium lets one be.
                && value["url"]
                    .as_str()
                    .is_some_and(|uri| uri.len() <= MAX_PAGE_URL)
                && value.get("error").is_none_or(|error| {
                    error
                        .as_str()
                        .is_some_and(|message| !message.is_empty() && message.len() <= 4096)
                })
        }
        "navigation_history" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                && super::NavigationDirection::from_wire(value["direction"].as_str()).is_some()
                && value["entries"].as_array().is_some_and(|entries| {
                    entries.len() <= 12
                        && entries.iter().all(|entry| {
                            valid_id(entry)
                                && entry["url"].as_str().is_some_and(|s| s.len() <= 8192)
                                && entry["title"].as_str().is_some_and(|s| s.len() <= 4096)
                        })
                })
        }
        "history" => value["back"].is_boolean() && value["forward"].is_boolean(),
        "page_result" | "text_input_result" => super::page::decode_result(value).is_some(),
        "paint_snapshot" | "paint_resource" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                && value["ok"].is_boolean()
        }
        // A load's failure names its address; the engine's own trouble (a
        // filter that could not check a resource) does not.
        "error" => value.get("url").is_none_or(Value::is_string) && text("message"),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err("malformed known event")
    }
}

fn command_within_limits(command: &WebViewCommand) -> bool {
    match command {
        WebViewCommand::ScopedNavigation {
            request_id,
            command,
        } => super::navigation_request_id(*request_id) && command.valid(),
        WebViewCommand::CancelScopedNavigation { request_id } => {
            super::navigation_request_id(*request_id)
        }
        WebViewCommand::Credentials(request) => request.valid(),
        WebViewCommand::ImportServiceSession(request) => request.valid(),
        WebViewCommand::SetSiteExceptions(origins) => {
            origins.len() <= 1024 && origins.iter().all(|s| !s.is_empty() && s.len() <= 8192)
        }
        WebViewCommand::DevTools {
            request_id,
            method,
            params,
            session,
        } => {
            *request_id > 0
                && i32::try_from(*request_id).is_ok()
                && method.len() <= 128
                && params.is_object()
                && params.to_string().len() <= 64 * 1024
                && session.as_ref().is_none_or(|session| session.len() <= 128)
        }
        WebViewCommand::RequestSiteStatus { request_id, origin } => {
            *request_id > 0
                && i32::try_from(*request_id).is_ok()
                && !origin.is_empty()
                && origin.len() <= 8192
        }
        WebViewCommand::SiteAction {
            request_id,
            origin,
            generation,
            action,
        } => {
            *request_id > 0
                && i32::try_from(*request_id).is_ok()
                && !origin.is_empty()
                && origin.len() <= 8192
                && *generation > 0
                && *generation <= 9_007_199_254_740_991
                && matches!(
                    action.as_str(),
                    "clear" | "revoke" | "reset" | "enable" | "disable" | "reload"
                )
        }
        WebViewCommand::ContextMenuAction { id, command } => {
            *id > 0 && i32::try_from(*id).is_ok() && i32::try_from(*command).is_ok()
        }
        WebViewCommand::Find { id, text, .. } => {
            *id > 0 && i32::try_from(*id).is_ok() && text.len() <= 4096
        }
        WebViewCommand::AnswerPageDialog { id, text, .. } => {
            *id > 0
                && i32::try_from(*id).is_ok()
                && text
                    .as_ref()
                    .is_none_or(|text| text.len() <= super::MAX_PAGE_DIALOG_ANSWER)
        }
        WebViewCommand::SetZoomLevel(level) => level.is_finite() && (-5.0..=9.0).contains(level),
        WebViewCommand::RequestNavigationHistory { request_id, .. }
        | WebViewCommand::PaintCapture { request_id } => {
            *request_id > 0 && i32::try_from(*request_id).is_ok()
        }
        WebViewCommand::NavigateToHistoryEntry { request_id, id } => {
            *request_id > 0
                && i32::try_from(*request_id).is_ok()
                && *id > 0
                && i32::try_from(*id).is_ok()
        }
        WebViewCommand::RequestMediaElementById { element_id, .. } => element_id.len() <= 256,
        WebViewCommand::Page {
            request_id,
            request,
        } => *request_id > 0 && i32::try_from(*request_id).is_ok() && request.valid(),
        WebViewCommand::PaintResource { request_id, url } => {
            *request_id > 0
                && i32::try_from(*request_id).is_ok()
                && !url.is_empty()
                && url.len() <= 1 << 20
        }
        WebViewCommand::ExtensionAccess {
            id,
            permissions,
            hosts,
            scriptable,
            token,
            commands,
        } => {
            extension_id(id)
                && token
                    .as_ref()
                    .is_none_or(|token| (32..=128).contains(&token.len()))
                && [permissions, hosts, scriptable]
                    .iter()
                    .all(|list| list.len() <= 512 && list.iter().all(|entry| entry.len() <= 2048))
                && commands.as_ref().is_none_or(|commands| {
                    commands.is_object() && commands.to_string().len() <= 64 * 1024
                })
        }
        WebViewCommand::ExtensionGrants {
            id,
            optional,
            granted,
        } => {
            extension_id(id)
                && [optional, granted].iter().all(|set| {
                    [&set.permissions, &set.origins].iter().all(|list| {
                        list.len() <= 512 && list.iter().all(|entry| entry.len() <= 2048)
                    })
                })
        }
        WebViewCommand::ExtensionPermissionDecision { request, .. } => {
            *request > 0 && i32::try_from(*request).is_ok()
        }
        WebViewCommand::ExtensionForget { id }
        | WebViewCommand::ExtensionIdentity { id }
        | WebViewCommand::ExtensionClick { id }
        | WebViewCommand::ExtensionOpenSidePanel { id } => extension_id(id),
        WebViewCommand::ExtensionMenuChoice { id, key } => {
            extension_id(id) && !key.is_empty() && key.len() <= 1024
        }
        WebViewCommand::ExtensionPanel { action } => action.as_deref().is_none_or(extension_id),
        WebViewCommand::ExtensionNotificationChosen {
            id,
            notification,
            button,
        } => {
            extension_id(id) && notification.len() <= 500 && button.is_none_or(|button| button < 2)
        }
        WebViewCommand::ExtensionSignInPanel { id, flow } => {
            extension_id(id) && *flow > 0 && i32::try_from(*flow).is_ok()
        }
        WebViewCommand::ExtensionOmnibox { id, input } => {
            extension_id(id) && extension_omnibox::within_limits(input)
        }
        WebViewCommand::PermissionDecision { id, .. }
        | WebViewCommand::PermissionDismissed { id }
        | WebViewCommand::DownloadDecision { id, .. }
        | WebViewCommand::CancelNativeDownload { id }
        | WebViewCommand::RetryDownload { id } => *id > 0 && i32::try_from(*id).is_ok(),
        WebViewCommand::Navigate(url) => url.len() <= 65536,
        WebViewCommand::StartDownload { uri } => {
            uri.len() <= 8192
                && url::Url::parse(uri).ok().is_some_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host().is_some()
                })
        }
        WebViewCommand::Key {
            key: KeyInput::Character(text),
            ..
        } => text.len() <= 65536,
        WebViewCommand::SetUserScripts(scripts) | WebViewCommand::SetBlockingScripts(scripts) => {
            scripts.len() <= 1024
                && scripts
                    .iter()
                    .try_fold(0usize, |n, s| n.checked_add(s.len()))
                    .is_some_and(|n| n <= 4 * 1024 * 1024)
        }
        WebViewCommand::SetNativeFilterRules { rules, .. } => rules.len() <= 32 * 1024 * 1024,
        _ => true,
    }
}

fn wipe_service_wire(value: &mut Value) {
    if value.get("op").and_then(Value::as_str) != Some("service_session_import") {
        return;
    }
    for field in ["cookies", "origin_storage"] {
        if let Some(entries) = value.get_mut(field).and_then(Value::as_array_mut) {
            for entry in entries {
                if let Some(Value::String(secret)) = entry.get_mut("value") {
                    zeroize::Zeroize::zeroize(secret);
                }
            }
        }
    }
}

#[derive(Default)]
struct CommandQueue {
    state: Mutex<(VecDeque<(Value, usize)>, usize, bool)>,
    ready: Condvar,
}
impl CommandQueue {
    fn push(&self, mut value: Value) -> Result<(), &'static str> {
        let result = self.push_owned(&mut value);
        wipe_service_wire(&mut value);
        result
    }
    fn push_owned(&self, value: &mut Value) -> Result<(), &'static str> {
        let encoded = match runtime_collab::wire_json_len(&value) {
            usize::MAX => return Err("encoding failed"),
            bytes if bytes >= protocol::MAX_PACKET as usize => return Err("command too large"),
            bytes => bytes,
        };
        let mut state = self.state.lock().map_err(|_| {
            self.ready.notify_all();
            "command channel poisoned"
        })?;
        if state.2 {
            return Err("command channel closed");
        }
        let op = value.get("op").and_then(Value::as_str).unwrap_or("");
        // These operations are explicitly replaceable. Keep ordering barriers:
        // mouse motion before a click and resize before navigation stay ordered.
        if matches!(op, "mouse_move" | "resize" | "focus")
            && state.0.back().is_some_and(|(last, _)| {
                last.get("op").and_then(Value::as_str) == Some(op)
                    && last.get("tab") == value.get("tab")
            })
            && let Some((_, bytes)) = state.0.pop_back()
        {
            state.1 = state
                .1
                .checked_sub(bytes)
                .ok_or("invalid command byte count")?;
        }
        let input = matches!(
            op,
            "mouse_move" | "mouse_button" | "key" | "scroll" | "resize" | "focus"
        );
        let (items, bytes) = if input {
            (4096, 95 * 1024 * 1024)
        } else {
            (4160, 96 * 1024 * 1024)
        };
        if state.0.len() >= items || state.1.saturating_add(encoded) > bytes {
            // Input saturation is not a navigation failure. The caller logs the
            // rejected operation; protocol/lifecycle errors have their own path.
            return Err("command queue capacity reached");
        }
        state.1 = state
            .1
            .checked_add(encoded)
            .ok_or("command byte count overflow")?;
        state.0.push_back((std::mem::take(value), encoded));
        drop(state);
        self.ready.notify_one();
        Ok(())
    }
    fn pop(&self) -> Option<Value> {
        let mut state = self.state.lock().ok()?;
        loop {
            if let Some((value, bytes)) = state.0.pop_front() {
                state.1 = state.1.checked_sub(bytes)?;
                drop(state);
                return Some(value);
            }
            if state.2 {
                return None;
            }
            state = self.ready.wait(state).ok()?;
        }
    }
    fn close(&self) {
        self.state.lock_recover().2 = true;
        self.ready.notify_all();
    }
}

impl Drop for CommandQueue {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (command, _) in &mut state.0 {
            wipe_service_wire(command);
        }
    }
}

fn decode_event(value: &Value) -> Option<WebViewEvent> {
    Some(match value["event"].as_str()? {
        "credential_result" => {
            WebViewEvent::CredentialResult(super::CredentialReply::decode(value)?)
        }
        "service_session_result" => {
            WebViewEvent::ServiceSessionResult(super::ServiceSessionReply::decode(value)?)
        }
        "closed" => WebViewEvent::TabClosed,
        "tab_detached" => WebViewEvent::TabDetached,
        "close_blocked" => WebViewEvent::TabCloseBlocked,
        "site_status" | "site_result" => decode_site_event(value, value["event"].as_str()?)?,
        "extension_action"
        | "extension_open_popup"
        | "extension_execute_action"
        | "extension_notification"
        | "extension_sign_in"
        | "extension_omnibox_suggestions"
        | "extension_omnibox_default"
        | "extension_side_panel" => decode_extension_event(value)?,
        "extension_permission_request" | "extension_permissions" => {
            extension_permissions::decode(value)?
        }
        "extension_action_reset" => WebViewEvent::ExtensionActionReset,
        "extension_download_open" => WebViewEvent::ExtensionDownloadOpen {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
            reveal: value["reveal"].as_bool()?,
        },
        "devtools_result" => WebViewEvent::DevToolsResult {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            ok: value["ok"].as_bool()?,
            body: value["body"].as_str()?.into(),
        },
        "context_closed" => WebViewEvent::ContextMenuClosed {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
        },
        "js_dialog" => WebViewEvent::PageDialog(decode_page_dialog(value)?),
        "js_dialog_closed" => WebViewEvent::PageDialogClosed {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
        },
        "context_menu" => WebViewEvent::ContextMenu {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
            x: value["x"].as_f64()?,
            y: value["y"].as_f64()?,
            link: value["link"].as_str()?.into(),
            items: decode_context_items(&value["items"], 1, &mut { MENU_ROWS })?,
        },
        "extension_menu" => decode_extension_menu(value)?,
        "find_result" => WebViewEvent::FindResult {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
            count: i32::try_from(value["count"].as_i64()?).ok()?,
            active: i32::try_from(value["active"].as_i64()?).ok()?,
            final_update: value["final"].as_bool()?,
        },
        "zoom" => WebViewEvent::ZoomChanged(value["level"].as_f64()?),
        "fullscreen" => WebViewEvent::FullscreenChanged(value["fullscreen"].as_bool()?),
        "renderer_terminated" => WebViewEvent::RendererTerminated,
        "uri" => WebViewEvent::UriChanged(value["url"].as_str()?.into()),
        "title" => WebViewEvent::TitleChanged(value["title"].as_str()?.into()),
        "favicon" => decode_favicon(value)?,
        "audio_muted" => WebViewEvent::AudioMutedChanged(value["muted"].as_bool()?),
        "media_state" => WebViewEvent::MediaPlayingChanged {
            page_url: value["page_url"].as_str()?.into(),
            document_generation: value["document_generation"].as_u64()?,
            playing: value["playing"].as_bool()?,
        },
        "progress" => WebViewEvent::LoadProgress(value["progress"].as_f64()?.clamp(0.0, 1.0)),
        "permission_prompt" => WebViewEvent::PermissionRequested {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
            origin: value["origin"].as_str()?.into(),
            permissions: value["permissions"].as_str()?.into(),
        },
        "permission_closed" => WebViewEvent::PermissionClosed {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
        },
        "download_prompt" => WebViewEvent::NativeDownloadRequested {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
            url: value["url"].as_str()?.into(),
            name: value["name"].as_str()?.into(),
        },
        "download" => WebViewEvent::DownloadUpdated {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
            state: value["state"].as_str()?.into(),
            path: value["path"].as_str()?.into(),
            name: value["name"].as_str()?.into(),
            // Too long to keep (a page's `data:` export): kept as no address,
            // so it cannot be fetched again.
            url: value["url"]
                .as_str()
                .filter(|url| url.len() <= MAX_DOWNLOAD_URL)
                .unwrap_or_default()
                .into(),
            retryable: value["retryable"].as_bool()?
                && value["url"]
                    .as_str()
                    .is_some_and(|url| url.len() <= MAX_DOWNLOAD_URL),
            received: wire_u64(&value["received"])?,
            total: wire_u64(&value["total"])?,
            error: value["error"].as_str()?.into(),
        },
        "download_runtime" => WebViewEvent::DownloadRuntime {
            admitted: u32::try_from(value["admitted"].as_u64()?).ok()?,
            detached: u32::try_from(value["detached"].as_u64()?).ok()?,
            accepting: value["accepting"].as_bool()?,
            error: value["error"].as_str()?.into(),
        },
        "navigation_history" => WebViewEvent::NavigationHistory {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            direction: super::NavigationDirection::from_wire(value["direction"].as_str())?,
            entries: value["entries"]
                .as_array()?
                .iter()
                .map(|entry| {
                    Some(super::NavigationEntry {
                        id: u32::try_from(entry["id"].as_u64()?).ok()?,
                        url: entry["url"].as_str()?.into(),
                        title: entry["title"].as_str()?.into(),
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        },
        "navigation_outcome" => {
            validate_event(value).ok()?;
            WebViewEvent::NavigationOutcome {
                request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
                uri: value["url"].as_str()?.into(),
                error: value.get("error").and_then(Value::as_str).map(String::from),
            }
        }
        "loaded" => WebViewEvent::LoadFinishedAt {
            uri: value.get("url")?.as_str()?.to_owned(),
        },
        "history" => WebViewEvent::HistoryChanged {
            can_go_back: value["back"].as_bool()?,
            can_go_forward: value["forward"].as_bool()?,
        },
        "page_result" => {
            let (request_id, result) = super::page::decode_result(value)?;
            WebViewEvent::PageResult { request_id, result }
        }
        "selection_text" => WebViewEvent::SelectionText {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            text: value["text"].as_str()?.into(),
        },
        "link_status" => WebViewEvent::LinkHovered(value["url"].as_str()?.into()),
        "find_selection" => WebViewEvent::FindSelection {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
            x: value["x"].as_f64()?,
            y: value["y"].as_f64()?,
            width: value["width"].as_f64()?,
            height: value["height"].as_f64()?,
        },
        "document_size" => WebViewEvent::DocumentSize {
            width: value["width"].as_f64()?,
            height: value["height"].as_f64()?,
            scroll_y: value["scroll_y"].as_f64()?,
        },
        "paint_snapshot" => WebViewEvent::PaintSnapshot {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            snapshot: value["ok"]
                .as_bool()?
                .then(|| value["snapshot"].as_str().map(str::to_owned))
                .flatten(),
        },
        "paint_resource" => WebViewEvent::PaintResource {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            bytes: value["ok"]
                .as_bool()?
                .then(|| {
                    let content = value["content"].as_str()?;
                    if value["base64"].as_bool().unwrap_or(false) {
                        use base64::Engine as _;
                        base64::engine::general_purpose::STANDARD
                            .decode(content)
                            .ok()
                    } else {
                        Some(content.as_bytes().to_vec())
                    }
                })
                .flatten(),
        },
        "error" => {
            let message = value["message"].as_str()?;
            let Some(uri) = value["url"].as_str() else {
                tracing::warn!(message, "Chromium engine error");
                return None;
            };
            WebViewEvent::LoadFailed {
                uri: uri.into(),
                error: message.into(),
            }
        }
        _ => return None,
    })
}

fn decode_base64_bounded(input: &str, limit: usize) -> Option<Vec<u8>> {
    use base64::Engine as _;
    // Bound allocation before decoding; retain the exact decoded byte cap too.
    let encoded_limit = limit.div_ceil(3).checked_mul(4)?;
    if input.len() > encoded_limit {
        return None;
    }
    let output = base64::engine::general_purpose::STANDARD
        .decode(input)
        .ok()?;
    (output.len() <= limit).then_some(output)
}

fn encode_command(command: WebViewCommand) -> Option<Value> {
    Some(match command {
        WebViewCommand::Credentials(request) => request.encode()?,
        WebViewCommand::ImportServiceSession(request) => request.encode()?,
        WebViewCommand::Navigate(url) => json!({"op":"navigate","url":url}),
        WebViewCommand::ScopedNavigation {
            request_id,
            command,
        } => {
            let mut fields = match command {
                NavigationCommand::Navigate { url } => json!({"command":"navigate","url":url}),
                NavigationCommand::Back => json!({"command":"back"}),
                NavigationCommand::Forward => json!({"command":"forward"}),
                NavigationCommand::Reload => json!({"command":"reload"}),
                NavigationCommand::HistoryEntry {
                    history_request_id,
                    id,
                } => {
                    json!({"command":"history_entry","history_request_id":history_request_id,"id":id})
                }
            };
            fields["op"] = json!("scoped_navigation");
            fields["request_id"] = json!(request_id);
            fields
        }
        WebViewCommand::CancelScopedNavigation { request_id } => {
            json!({"op":"cancel_navigation","request_id":request_id})
        }
        WebViewCommand::RequestNavigationHistory {
            request_id,
            direction,
        } => {
            json!({"op":"navigation_history","request_id":request_id,"direction":direction.as_str()})
        }
        WebViewCommand::NavigateToHistoryEntry { request_id, id } => {
            json!({"op":"history_entry","request_id":request_id,"id":id})
        }
        WebViewCommand::GoBack => json!({"op":"back"}),
        WebViewCommand::GoForward => json!({"op":"forward"}),
        WebViewCommand::Reload => json!({"op":"reload"}),
        WebViewCommand::StopLoading => json!({"op":"stop"}),
        WebViewCommand::ExitFullscreen => json!({"op":"exit_fullscreen"}),
        WebViewCommand::SetAudioMuted(muted) => json!({"op":"audio_muted","muted":muted}),
        WebViewCommand::Resize {
            width,
            height,
            scale_factor,
        } => json!({"op":"resize","width":width,"height":height,"scale_factor":scale_factor}),
        WebViewCommand::SetFocused(focused) => json!({"op":"focus","focused":focused}),
        WebViewCommand::MouseMove { x, y } => json!({"op":"mouse_move","x":x,"y":y}),
        WebViewCommand::MouseButton { button, down, x, y } => {
            json!({"op":"mouse_button","button":match button {MouseButton::Left=>"left",MouseButton::Middle=>"middle",MouseButton::Right=>"right"},"down":down,"x":x,"y":y})
        }
        WebViewCommand::Scroll { dx, dy, x, y } => {
            json!({"op":"scroll","dx":dx,"dy":dy,"x":x,"y":y})
        }
        WebViewCommand::Key {
            key,
            down,
            modifiers,
        } => {
            let key = match key {
                KeyInput::Character(value) => value,
                KeyInput::Named(named) => format!("{named:?}"),
            };
            json!({"op":"key","key":key,"down":down,"ctrl":modifiers.ctrl,"shift":modifiers.shift,"alt":modifiers.alt,"meta":modifiers.meta})
        }
        WebViewCommand::SetBlockingScripts(scripts) => {
            json!({"op":"blocking_scripts","scripts": &*scripts})
        }
        WebViewCommand::SetUserScripts(scripts) => json!({"op":"scripts","scripts": &*scripts}),
        WebViewCommand::SetSiteExceptions(origins) => {
            json!({"op":"site_exceptions","origins":origins})
        }
        WebViewCommand::ExtensionAccess {
            id,
            permissions,
            hosts,
            scriptable,
            token,
            commands,
        } => {
            let mut command = json!({"op":"extension_access","id":id,
                "permissions":permissions,"hosts":hosts,"scriptable":scriptable});
            if let Some(token) = token {
                command["token"] = json!(token);
            }
            if let Some(commands) = commands {
                command["commands"] = commands;
            }
            command
        }
        WebViewCommand::ExtensionGrants {
            id,
            optional,
            granted,
        } => {
            let set = |set: &super::ExtensionPermissionSet| json!({"permissions":set.permissions,"hosts":set.origins});
            json!({"op":"extension_grants","id":id,"optional":set(&optional),"granted":set(&granted)})
        }
        WebViewCommand::ExtensionPermissionDecision { request, allow } => {
            json!({"op":"extension_permission_decision","request":request,"allow":allow})
        }
        WebViewCommand::ExtensionForget { id } => json!({"op":"extension_forget","id":id}),
        WebViewCommand::ExtensionIdentity { id } => json!({"op":"extension_identity","id":id}),
        WebViewCommand::ExtensionPanel { action } => match action {
            Some(id) => json!({"op":"extension_panel","action":id}),
            None => json!({"op":"extension_panel"}),
        },
        WebViewCommand::ExtensionClick { id } => json!({"op":"extension_click","id":id}),
        WebViewCommand::ExtensionMenuChoice { id, key } => {
            json!({"op":"extension_menu_action","id":id,"key":key})
        }
        WebViewCommand::ExtensionNotificationChosen {
            id,
            notification,
            button,
        } => match button {
            Some(button) => json!({"op":"extension_notification_chosen","id":id,
                "notification":notification,"button":button}),
            None => {
                json!({"op":"extension_notification_chosen","id":id,"notification":notification})
            }
        },
        WebViewCommand::ExtensionSignInPanel { id, flow } => {
            json!({"op":"extension_sign_in_panel","id":id,"flow":flow})
        }
        WebViewCommand::ExtensionOmnibox { id, input } => extension_omnibox::encode(&id, input),
        WebViewCommand::ExtensionOpenSidePanel { id } => {
            json!({"op":"extension_side_panel","id":id})
        }
        WebViewCommand::DevTools {
            request_id,
            method,
            params,
            session,
        } => {
            let mut command = json!({"op":"devtools_call","request_id":request_id,"method":method,"params":params});
            if let Some(session) = session {
                command["session"] = json!(session);
            }
            command
        }
        WebViewCommand::RequestSiteStatus { request_id, origin } => {
            json!({"op":"site_status","request_id":request_id,"origin":origin})
        }
        WebViewCommand::SiteAction {
            request_id,
            origin,
            generation,
            action,
        } => {
            json!({"op":"site_action","request_id":request_id,"origin":origin,"generation":generation,"action":action})
        }
        WebViewCommand::ClearWebsiteData => json!({"op":"clear_data"}),
        WebViewCommand::SetClearOnExit(enabled) => json!({"op":"clear_on_exit","enabled":enabled}),
        WebViewCommand::ShowDeveloperTools => json!({"op":"devtools"}),
        WebViewCommand::PermissionDecision { id, allow } => {
            json!({"op":"permission_decision","id":id,"allow":allow})
        }
        // A no to a host that knows no dismissal; a dismissal to one that does.
        WebViewCommand::PermissionDismissed { id } => {
            json!({"op":"permission_decision","id":id,"allow":false,"dismiss":true})
        }
        WebViewCommand::DownloadDecision { id, allow } => {
            json!({"op":"download_decision","id":id,"allow":allow})
        }
        WebViewCommand::Find {
            id,
            text,
            forward,
            next,
        } => json!({"op":"find","id":id,"text":text,"forward":forward,"next":next}),
        WebViewCommand::ContextMenuAction { id, command } => {
            json!({"op":"context_action","id":id,"command":command})
        }
        WebViewCommand::AnswerPageDialog { id, accept, text } => match text {
            Some(text) => json!({"op":"js_dialog_reply","id":id,"accept":accept,"text":text}),
            None => json!({"op":"js_dialog_reply","id":id,"accept":accept}),
        },
        WebViewCommand::StopFinding => json!({"op":"stop_find"}),
        WebViewCommand::SetZoomLevel(level) => json!({"op":"zoom","level":level}),
        WebViewCommand::ReportDocumentSize(enabled) => {
            json!({"op":"document_size","enabled":enabled})
        }
        WebViewCommand::CopySelection { request_id } => {
            json!({"op":"selection_text","request_id":request_id})
        }
        WebViewCommand::CancelNativeDownload { id } => json!({"op":"download_cancel","id":id}),
        WebViewCommand::RetryDownload { id } => json!({"op":"download_retry","id":id}),
        WebViewCommand::CancelAllDownloads => json!({"op":"download_cancel_all"}),
        WebViewCommand::SetNativeFilterRules { rules, enabled } => {
            json!({"op":"filter_rules","rules":&*rules,"enabled":enabled})
        }
        WebViewCommand::Page {
            request_id,
            request,
        } => super::page::encode_request(request_id, &request),
        WebViewCommand::PaintCapture { request_id } => json!({
            "op": "paint_capture",
            "request_id": request_id,
            "styles": tilo_paint_stream::snapshot::COMPUTED_STYLES,
        }),
        WebViewCommand::PaintResource { request_id, url } => {
            json!({"op":"paint_resource","request_id":request_id,"url":url})
        }
        WebViewCommand::StartDownload { uri } => json!({"op":"download_start","url":uri}),
        WebViewCommand::SetDownloadDestination { .. }
        | WebViewCommand::CancelDownload { .. }
        | WebViewCommand::SetContentFilter(_)
        | WebViewCommand::SetContentFilters { .. } => {
            return None;
        }
        WebViewCommand::RequestMediaElement { .. }
        | WebViewCommand::RequestMediaElementById { .. } => return None,
    })
}

mod event_decoding;
use event_decoding::{
    decode_extension_event, decode_page_dialog, decode_site_event, valid_site_event,
};
#[cfg(test)]
mod extension_tests;
#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod popup_tests;
#[cfg(test)]
mod tests;
