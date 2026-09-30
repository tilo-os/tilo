//! Chromium helper transport, independent of native CEF build dependencies.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the Chromium helper process is supervised by threads"
)]
mod protocol;

use crate::time::Duration;
use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Condvar, Mutex, Weak};

use super::{FrameSlot, KeyInput, MouseButton, WebViewCommand, WebViewEngine, WebViewEvent};
use serde_json::{Value, json};

#[derive(Default)]
struct Shared {
    open_tabs: Mutex<VecDeque<(String, bool)>>,
    frames: FrameSlot,
    events: Mutex<VecDeque<WebViewEvent>>,
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    uri: Mutex<String>,
    stopping: AtomicBool,
    failed: AtomicBool,
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

    fn event(&self, event: WebViewEvent) {
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
                    )
                })
            {
                events.remove(index);
            }
            events.push_back(event);
        }
        self.wake();
    }

    fn fail(&self, message: String) {
        if !self.stopping.load(Ordering::Acquire) && !self.failed.swap(true, Ordering::AcqRel) {
            let uri = self.uri.lock().map(|uri| uri.clone()).unwrap_or_default();
            self.event(WebViewEvent::LoadFailed {
                uri,
                error: message,
            });
        }
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
const PRIVATE_ROOT_PREFIX: &str = "tilo-browser-private-";
const PRIVATE_ROOT_MARKER: &str = ".tilo-private-root-v1";
const PRIVATE_ROOT_MARKER_CONTENT: &str = "tilo-browser-owned-private-root-v1\n";
const PRIVATE_ROOT_HELPER: &str = ".tilo-private-helper-pid";
#[cfg(unix)]
const PRIVATE_ROOT_LOCK: &str = ".tilo-private-root.lock";

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

fn owned_private_root(root: &Path) -> bool {
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
    if owned_private_root(root)
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
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.process.stopping.store(true, Ordering::Release);
        let _ = self.tx.push(json!({"op":"shutdown"}));
        self.tx.close();
        if let Some(supervisor) = self
            .supervisor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            && supervisor.thread().id() != std::thread::current().id()
        {
            let _ = supervisor.join();
        }
    }
}
fn queue_download_event(queue: &Mutex<VecDeque<WebViewEvent>>, event: WebViewEvent) {
    let mut events = queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// Error-only surface used when a real engine cannot start. It never renders a
/// page and repeats the diagnostic on navigation so the browser's normal error
/// presentation remains actionable.
pub struct UnavailableEngine {
    message: String,
    shared: Shared,
    _private_root: PrivateRootCleanup,
}

impl UnavailableEngine {
    pub fn new(message: String) -> Arc<Self> {
        Self::build(message, None)
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the private root is not an owned Browser directory.
    pub fn new_with_private_root(message: String, root: &Path) -> Result<Arc<Self>, String> {
        if !owned_private_root(root) {
            return Err("Private Chromium root is not owned by Browser".into());
        }
        Ok(Self::build(message, Some(root.to_owned())))
    }

    fn build(message: String, private_root: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            message,
            shared: Shared {
                open_tabs: Mutex::default(),
                frames: Arc::new(Mutex::new(None)),
                events: Mutex::new(VecDeque::new()),
                waker: Mutex::new(None),
                uri: Mutex::new(String::new()),
                stopping: AtomicBool::new(false),
                failed: AtomicBool::new(false),
            },
            _private_root: PrivateRootCleanup(private_root),
        })
    }
}

impl WebViewEngine for UnavailableEngine {
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
    if let Some(path) = std::env::var_os("TILO_CHROMIUM_HOST") {
        return Ok(path.into());
    }
    #[cfg(test)]
    let executable = std::env::var_os("TILO_TEST_CHROMIUM_DISCOVERY_EXE")
        .map(PathBuf::from)
        .map_or_else(std::env::current_exe, Ok)
        .map_err(|error| error.to_string())?;
    #[cfg(not(test))]
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let binary_name = if cfg!(target_os = "macos") {
        "Browser Engine.app/Contents/MacOS/tilo-chromium-host"
    } else {
        "tilo-chromium-host"
    };
    for parent in executable.ancestors().skip(1).take(4) {
        for candidate in [
            parent.join("chromium").join(binary_name),
            parent.join("Frameworks").join(binary_name),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err("Chromium runtime is missing. Run python3 scripts/browser/build-host.py, or set TILO_CHROMIUM_HOST to its executable.".into())
}

fn spawn_command_writer(
    stdin: std::process::ChildStdin,
    rx: Arc<CommandQueue>,
    writer_shared: Arc<Shared>,
    writer_routes: Routes,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("chromium-commands".into())
        .spawn(move || {
            let mut writer = stdin;
            while let Some(command) = rx.pop() {
                if let Err(error) = protocol::write_json(&mut writer, &command) {
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
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("chromium-frames".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match protocol::read_packet(&mut reader) {
                    Ok(Some(protocol::Packet::Frame(frame))) => {
                        if let Some(shared) = route(&reader_routes, 1) {
                            if let Ok(mut slot) = shared.frames.lock() {
                                *slot = Some(frame);
                            }
                            shared.wake();
                        }
                    }
                    Ok(Some(protocol::Packet::TabFrame(tab, frame))) => {
                        if let Some(shared) = route(&reader_routes, tab) {
                            if let Ok(mut slot) = shared.frames.lock() {
                                *slot = Some(frame);
                            }
                            shared.wake();
                        }
                    }
                    Ok(Some(protocol::Packet::Json(value))) => {
                        if let Err(error) =
                            route_native_event(&value, &reader_routes, &download_events, &ready_tx)
                        {
                            reader_shared.fail(format!("Invalid Chromium event: {error}"));
                            break;
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
        })
}

fn take_supervisor_child(slot: &Mutex<Option<std::process::Child>>) -> Option<std::process::Child> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

fn supervise_child(
    mut child: std::process::Child,
    supervisor_shared: &Shared,
    supervisor_queue: &CommandQueue,
    supervisor_private_root: Option<&Path>,
) {
    let mut shutdown_started = None;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                supervisor_shared.fail("Chromium engine exited".into());
                break;
            }
            Err(error) => {
                supervisor_shared.fail(format!("Chromium process status: {error}"));
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => {}
        }
        if supervisor_shared.failed.load(Ordering::Acquire) {
            supervisor_queue.close();
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        if supervisor_shared.stopping.load(Ordering::Acquire) {
            let began = shutdown_started.get_or_insert_with(crate::time::now);
            if began.elapsed() >= Duration::from_secs(35) {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    supervisor_queue.close();
    if let Some(root) = supervisor_private_root {
        cleanup_private_root(root);
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

fn launch_host(
    host: &Path,
    profile: &Path,
    extensions: &[String],
    private_root: Option<&Path>,
) -> Result<std::process::Child, String> {
    std::fs::create_dir_all(profile).map_err(|error| format!("Chromium profile: {error}"))?;
    let profile = profile.canonicalize().map_err(|error| error.to_string())?;
    let mut command = Command::new(host);
    if private_root.is_some() {
        command.arg("--private");
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

fn valid_context_menu(value: &Value) -> bool {
    valid_id(value)
        && value["link"].as_str().is_some_and(|s| s.len() <= 8192)
        && ["x", "y"].iter().all(|k| {
            value[*k]
                .as_f64()
                .is_some_and(|v| v.is_finite() && v.abs() < 100_000.0)
        })
        && value["items"].as_array().is_some_and(|items| {
            items.len() <= 24
                && items.iter().all(|item| {
                    valid_id(item) && item["label"].as_str().is_some_and(|s| s.len() <= 4096)
                })
        })
}

impl ChromiumEngine {
    /// Start the packaged runtime and persistent local profile.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime/profile cannot be located, prepared, or initialized.
    pub fn new() -> Result<Arc<Self>, String> {
        Self::new_with_extensions(&[])
    }

    ///
    /// # Errors
    ///
    /// Returns an error for invalid extensions, unavailable runtime/profile, or helper initialization failure.
    pub fn new_with_extensions(extensions: &[PathBuf]) -> Result<Arc<Self>, String> {
        let profile = std::env::var_os("TILO_CHROMIUM_PROFILE")
            .map(PathBuf::from)
            .or_else(|| tilo_paths::data_dir().map(|path| path.join("tilo-browser/chromium")))
            .ok_or("Cannot locate Chromium profile directory")?;
        Self::with_host_extensions(&host_path()?, &profile, extensions, None)
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the private root is not owned, the runtime is missing, or initialization fails.
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
    ///
    /// # Errors
    ///
    /// Returns an error for invalid extensions, profile preparation failure, or helper startup/initialization failure.
    pub fn with_profile_extensions(
        profile: &Path,
        extensions: &[PathBuf],
    ) -> Result<Arc<Self>, String> {
        Self::with_host_extensions(&host_path()?, profile, extensions, None)
    }

    /// Explicit runtime/profile paths for packaging and isolated live tests.
    ///
    /// # Errors
    ///
    /// Returns an error if the profile cannot be prepared or the helper cannot start and initialize.
    pub fn with_host(host: &Path, profile: &Path) -> Result<Arc<Self>, String> {
        Self::with_host_extensions(host, profile, &[], None)
    }

    /// Start an incognito helper whose complete installation root is removed
    /// by the process supervisor only after the native child has exited.
    ///
    /// # Errors
    ///
    /// Returns an error if the private root is not owned, the profile cannot be prepared, or the helper cannot initialize.
    pub fn with_private_host(host: &Path, root: &Path) -> Result<Arc<Self>, String> {
        if !owned_private_root(root) {
            return Err("Private Chromium root is not owned by Browser".into());
        }
        Self::with_host_extensions(host, &root.join("chromium"), &[], Some(root.to_owned()))
    }

    fn with_host_extensions(
        host: &Path,
        profile: &Path,
        extensions: &[PathBuf],
        private_root: Option<PathBuf>,
    ) -> Result<Arc<Self>, String> {
        let extensions = validated_extension_paths(extensions)?;
        let mut cleanup_on_error = PrivateRootCleanup(private_root.clone());
        let mut child = launch_host(host, profile, &extensions, private_root.as_deref())?;
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
        let shared = Arc::new(Shared {
            open_tabs: Mutex::default(),
            frames: Arc::new(Mutex::new(None)),
            events: Mutex::new(VecDeque::new()),
            waker: Mutex::new(None),
            uri: Mutex::new(String::new()),
            stopping: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        });
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
        });
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
        ) {
            tx.close();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        if let Err(error) = ready_rx.recv_timeout(Duration::from_secs(20)) {
            shared.stopping.store(true, Ordering::Release);
            tx.close();
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Chromium did not initialize: {error}"));
        }
        let child = Arc::new(Mutex::new(Some(child)));
        let supervisor_child = child.clone();
        let supervisor_shared = shared.clone();
        let supervisor_queue = tx.clone();
        let wait_for_private_cleanup = private_root.is_some();
        let supervisor_private_root = private_root;
        let supervisor = std::thread::Builder::new()
            .name("chromium-supervisor".into())
            .spawn(move || {
                let Some(child) = take_supervisor_child(&supervisor_child) else {
                    supervisor_shared.fail("Chromium supervisor lost process ownership".into());
                    supervisor_queue.close();
                    return;
                };
                supervise_child(
                    child,
                    &supervisor_shared,
                    &supervisor_queue,
                    supervisor_private_root.as_deref(),
                );
            });
        let supervisor = match supervisor {
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
            *runtime
                .supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(supervisor);
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

impl WebViewEngine for ChromiumEngine {
    fn create_tab(&self) -> Result<super::SharedEngine, String> {
        let waker = self
            .shared
            .waker
            .lock()
            .map_err(|_| "Tab wake callback unavailable")?
            .clone();
        let mut routes = self
            .runtime
            .routes
            .lock()
            .map_err(|_| "Tab routes unavailable")?;
        if routes.len() >= 32 {
            return Err("At most 32 tabs can be open".into());
        }
        let tab = self.runtime.next_tab.fetch_add(1, Ordering::Relaxed);
        if tab > i32::MAX as u32 {
            return Err("Tab identity exhausted".into());
        }
        let shared = Arc::new(Shared {
            waker: Mutex::new(waker),
            ..Shared::default()
        });
        routes.insert(tab, Arc::downgrade(&shared));
        if let Err(error) = self.tx.push(json!({"op":"create_tab","tab":tab})) {
            routes.remove(&tab);
            return Err(error.into());
        }
        drop(routes);
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
        self.shared
            .open_tabs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    fn supports_native_filtering(&self) -> bool {
        true
    }

    fn send(&self, command: WebViewCommand) {
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
            let mut routes = self
                .runtime
                .routes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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

fn valid_site_event(value: &Value, event: &str) -> bool {
    let header = value["request_id"]
        .as_u64()
        .is_some_and(|n| n > 0 && i32::try_from(n).is_ok())
        && value["origin"].as_str().is_some_and(|s| s.len() <= 8192)
        && value["generation"].as_f64().is_some_and(|n| {
            n.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&n) && n.fract() == 0.0
        });
    header
        && if event == "site_result" {
            value["success"].is_boolean()
                && value["action"].as_str().is_some_and(|s| {
                    matches!(
                        s,
                        "status" | "clear" | "revoke" | "enable" | "disable" | "reload"
                    )
                })
                && value["message"].as_str().is_some_and(|s| s.len() <= 1024)
        } else {
            ["global_enabled", "excepted", "effective"]
                .iter()
                .all(|k| value[*k].is_boolean())
                && value["blocked"].as_f64().is_some_and(|n| {
                    n.is_finite()
                        && (0.0..=9_007_199_254_740_991.0).contains(&n)
                        && n.fract() == 0.0
                })
                && matches!(
                    value["security"].as_str(),
                    Some("insecure" | "secure" | "unverified")
                )
                && ["subject", "issuer"]
                    .iter()
                    .all(|k| value[*k].as_str().is_some_and(|s| s.len() <= 512))
                && ["certificate_status", "tls_version", "content_status"]
                    .iter()
                    .all(|k| value[*k].as_u64().is_some_and(|n| u32::try_from(n).is_ok()))
                && value["expires"].as_f64().is_some_and(f64::is_finite)
                && value["permissions"].as_object().is_some_and(|p| {
                    p.len() == 4
                        && ["camera", "microphone", "location", "notifications"]
                            .iter()
                            .all(|k| {
                                matches!(
                                    p.get(*k).and_then(Value::as_str),
                                    Some("allowed" | "blocked" | "ask")
                                )
                            })
                })
        }
}

fn decode_site_event(value: &Value, event: &str) -> Option<WebViewEvent> {
    Some(match event {
        "site_status" => WebViewEvent::SiteStatus(super::SiteStatus {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            origin: value["origin"].as_str()?.into(),
            generation: wire_u64(&value["generation"])?,
            blocked: wire_u64(&value["blocked"])?,
            global_enabled: value["global_enabled"].as_bool()?,
            excepted: value["excepted"].as_bool()?,
            effective: value["effective"].as_bool()?,
            security: value["security"].as_str()?.into(),
            certificate_status: u32::try_from(value["certificate_status"].as_u64()?).ok()?,
            tls_version: u32::try_from(value["tls_version"].as_u64()?).ok()?,
            content_status: u32::try_from(value["content_status"].as_u64()?).ok()?,
            subject: value["subject"].as_str()?.into(),
            issuer: value["issuer"].as_str()?.into(),
            expires: value["expires"].as_f64()?,
            permissions: value["permissions"]
                .as_object()?
                .iter()
                .map(|(k, v)| Some((k.clone(), v.as_str()?.into())))
                .collect::<Option<Vec<_>>>()?,
        }),
        "site_result" => WebViewEvent::SiteResult {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            origin: value["origin"].as_str()?.into(),
            generation: wire_u64(&value["generation"])?,
            action: value["action"].as_str()?.into(),
            success: value["success"].as_bool()?,
            message: value["message"].as_str()?.into(),
        },
        _ => return None,
    })
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
        "site_status" | "site_result" => valid_site_event(value, event),
        "context_closed" | "permission_closed" => valid_id(value),
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
        "renderer_terminated" => value
            .as_object()
            .is_some_and(|object| object.len() == if value.get("tab").is_some() { 2 } else { 1 }),
        "permission_prompt" => valid_id(value) && text("origin") && text("permissions"),
        "download_prompt" => valid_id(value) && text("url") && text("name"),
        "download" => {
            valid_id(value)
                && text("path")
                && text("name")
                && value["url"].as_str().is_some_and(|url| url.len() <= 8192)
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
        "ready" => text("cef_version") && text("chromium_version"),
        "open_tab" => {
            value["url"].as_str().is_some_and(|url| url.len() <= 8192)
                && value["background"].is_boolean()
        }
        "uri" => text("url"),
        "title" => text("title"),
        "progress" => value["progress"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n)),
        "loaded" => text("url") && value["status"].as_i64().is_some(),
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
        "page_result" => super::page::decode_result(value).is_some(),
        "error" => text("url") && text("message"),
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
        WebViewCommand::Credentials(request) => request.valid(),
        WebViewCommand::SetSiteExceptions(origins) => {
            origins.len() <= 1024 && origins.iter().all(|s| !s.is_empty() && s.len() <= 8192)
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
                    "clear" | "revoke" | "enable" | "disable" | "reload"
                )
        }
        WebViewCommand::ContextMenuAction { id, command } => {
            *id > 0 && i32::try_from(*id).is_ok() && i32::try_from(*command).is_ok()
        }
        WebViewCommand::Find { id, text, .. } => {
            *id > 0 && i32::try_from(*id).is_ok() && text.len() <= 4096
        }
        WebViewCommand::SetZoomLevel(level) => level.is_finite() && (-5.0..=9.0).contains(level),
        WebViewCommand::RequestNavigationHistory { request_id, .. } => {
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
        WebViewCommand::PermissionDecision { id, .. }
        | WebViewCommand::DownloadDecision { id, .. }
        | WebViewCommand::CancelNativeDownload { id }
        | WebViewCommand::RetryDownload { id } => *id > 0 && i32::try_from(*id).is_ok(),
        WebViewCommand::Navigate(url) => url.len() <= 65536,
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

#[derive(Default)]
struct CommandQueue {
    state: Mutex<(VecDeque<(Value, usize)>, usize, bool)>,
    ready: Condvar,
}
impl CommandQueue {
    fn push(&self, value: Value) -> Result<(), &'static str> {
        // Count encoded bytes without allocating a second command buffer.
        struct Counter(usize);
        impl std::io::Write for Counter {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(data.len())
                    .ok_or_else(|| std::io::Error::other("command byte count overflow"))?;
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Counter(0);
        serde_json::to_writer(&mut count, &value).map_err(|_| "encoding failed")?;
        if count.0 >= protocol::MAX_PACKET as usize {
            return Err("command too large");
        }
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
        if state.0.len() >= items || state.1.saturating_add(count.0) > bytes {
            // Input saturation is not a navigation failure. The caller logs the
            // rejected operation; protocol/lifecycle errors have their own path.
            return Err("command queue capacity reached");
        }
        state.1 = state
            .1
            .checked_add(count.0)
            .ok_or("command byte count overflow")?;
        state.0.push_back((value, count.0));
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
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .2 = true;
        self.ready.notify_all();
    }
}

fn decode_event(value: &Value) -> Option<WebViewEvent> {
    Some(match value["event"].as_str()? {
        "credential_result" => {
            WebViewEvent::CredentialResult(super::CredentialReply::decode(value)?)
        }
        "closed" => WebViewEvent::TabClosed,
        "tab_detached" => WebViewEvent::TabDetached,
        "close_blocked" => WebViewEvent::TabCloseBlocked,
        "site_status" | "site_result" => decode_site_event(value, value["event"].as_str()?)?,
        "context_closed" => WebViewEvent::ContextMenuClosed {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
        },
        "context_menu" => WebViewEvent::ContextMenu {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
            x: value["x"].as_f64()?,
            y: value["y"].as_f64()?,
            link: value["link"].as_str()?.into(),
            items: value["items"]
                .as_array()?
                .iter()
                .map(|item| {
                    Some((
                        u32::try_from(item["id"].as_u64()?).ok()?,
                        item["label"].as_str()?.into(),
                    ))
                })
                .collect::<Option<Vec<_>>>()?,
        },
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
            url: value["url"].as_str()?.into(),
            retryable: value["retryable"].as_bool()?,
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
        "error" => WebViewEvent::LoadFailed {
            uri: value["url"].as_str().unwrap_or_default().into(),
            error: value["message"].as_str()?.into(),
        },
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
        WebViewCommand::Navigate(url) => json!({"op":"navigate","url":url}),
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
        WebViewCommand::StopFinding => json!({"op":"stop_find"}),
        WebViewCommand::SetZoomLevel(level) => json!({"op":"zoom","level":level}),
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
        WebViewCommand::StartDownload { .. }
        | WebViewCommand::SetDownloadDestination { .. }
        | WebViewCommand::CancelDownload { .. }
        | WebViewCommand::SetContentFilter(_)
        | WebViewCommand::SetContentFilters { .. } => {
            return None;
        }
        WebViewCommand::RequestMediaElement { .. }
        | WebViewCommand::RequestMediaElementById { .. } => return None,
    })
}

#[cfg(test)]
mod tests;
