//! Chromium helper transport, independent of native CEF build dependencies.
mod protocol;

use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

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
        if let Some(waker) = waker {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker())).is_err() {
                tracing::error!("Chromium UI wake callback panicked");
            }
        }
    }

    fn event(&self, event: WebViewEvent) {
        if let WebViewEvent::UriChanged(uri) = &event {
            if let Ok(mut current) = self.uri.lock() {
                current.clone_from(uri);
            }
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
            if events.len() >= 512 {
                if let Some(index) = events.iter().position(|old| {
                    !matches!(
                        old,
                        WebViewEvent::LoadFinished
                            | WebViewEvent::LoadFinishedAt { .. }
                            | WebViewEvent::LoadFailed { .. }
                    )
                }) {
                    events.remove(index);
                }
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
const PRIVATE_ROOT_PREFIX: &str = "qos-browser-private-";
const PRIVATE_ROOT_MARKER: &str = ".qos-private-root-v1";
const PRIVATE_ROOT_MARKER_CONTENT: &str = "qos-browser-owned-private-root-v1\n";
const PRIVATE_ROOT_HELPER: &str = ".qos-private-helper-pid";
const PRIVATE_ROOT_LOCK: &str = ".qos-private-root.lock";

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
        use std::os::fd::AsRawFd;
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(PRIVATE_ROOT_LOCK))
        {
            Ok(file) => {
                // SAFETY: flock only observes the valid descriptor owned by `file`.
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                    tracing::warn!(path = %root.display(), "Could not lock private browser root for cleanup");
                    return;
                }
                Some(file)
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

fn fail_routes(routes: &Routes, message: String) {
    let endpoints: Vec<_> = routes
        .lock()
        .unwrap()
        .values()
        .filter_map(Weak::upgrade)
        .collect();
    for shared in endpoints {
        shared.fail(message.clone());
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
        if let WebViewCommand::Navigate(uri) = command {
            self.shared.event(WebViewEvent::LoadFailed {
                uri,
                error: self.message.clone(),
            });
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
    if let Some(path) = std::env::var_os("QOS_CHROMIUM_HOST") {
        return Ok(path.into());
    }
    #[cfg(test)]
    let executable = std::env::var_os("QOS_TEST_CHROMIUM_DISCOVERY_EXE")
        .map(PathBuf::from)
        .map_or_else(std::env::current_exe, Ok)
        .map_err(|error| error.to_string())?;
    #[cfg(not(test))]
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let binary_name = if cfg!(target_os = "macos") {
        "Browser Engine.app/Contents/MacOS/qos-chromium-host"
    } else {
        "qos-chromium-host"
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
    Err("Chromium runtime is missing. Run python3 scripts/browser/build-host.py, or set QOS_CHROMIUM_HOST to its executable.".into())
}

impl ChromiumEngine {
    /// Start the packaged runtime and persistent local profile.
    pub fn new() -> Result<Arc<Self>, String> {
        Self::new_with_extensions(&[])
    }

    pub fn new_with_extensions(extensions: &[PathBuf]) -> Result<Arc<Self>, String> {
        let profile = std::env::var_os("QOS_CHROMIUM_PROFILE")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|path| path.join("qos-browser/chromium")))
            .ok_or("Cannot locate Chromium profile directory")?;
        Self::with_host_extensions(&host_path()?, &profile, extensions, None)
    }

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
        Self::with_host_extensions(&host_path()?, profile, extensions, None)
    }

    /// Explicit runtime/profile paths for packaging and isolated live tests.
    pub fn with_host(host: &Path, profile: &Path) -> Result<Arc<Self>, String> {
        Self::with_host_extensions(host, profile, &[], None)
    }

    /// Start an incognito helper whose complete installation root is removed
    /// by the process supervisor only after the native child has exited.
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
        if extensions.len() > 100 {
            return Err("Too many approved extensions".into());
        }
        let extensions = extensions
            .iter()
            .map(|path| {
                let path = path.canonicalize().map_err(|error| error.to_string())?;
                let value = path.to_str().ok_or("Invalid extension path")?;
                if value.contains(',') || value.len() > 4096 {
                    return Err("Unsupported extension path".into());
                }
                Ok(value.to_owned())
            })
            .collect::<Result<Vec<String>, String>>()?;
        let mut cleanup_on_error = PrivateRootCleanup(private_root.clone());
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
                dirs::download_dir()
                    .unwrap_or_else(|| profile.join("downloads"))
                    .display()
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("Could not start Chromium at {}: {error}", host.display()))?;
        if let Some(root) = private_root.as_deref()
            && let Err(error) = record_private_helper(root, child.id())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Could not record private Chromium process: {error}"
            ));
        }
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
        if let Err(error) = std::thread::Builder::new()
            .name("chromium-commands".into())
            .spawn(move || {
                let mut writer = stdin;
                while let Some(command) = rx.pop() {
                    if let Err(error) = protocol::write_json(&mut writer, &command) {
                        writer_shared.fail(format!("Chromium command channel closed: {error}"));
                        fail_routes(
                            &writer_routes,
                            format!("Chromium command channel closed: {error}"),
                        );
                        break;
                    }
                }
            })
        {
            tx.close();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        let reader_shared = shared.clone();
        let reader_routes = routes.clone();
        if let Err(error) = std::thread::Builder::new().name("chromium-frames".into()).spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match protocol::read_packet(&mut reader) {
                    Ok(Some(protocol::Packet::Frame(frame))) => {
                        if let Some(shared) = route(&reader_routes, 1) {
                            if let Ok(mut slot) = shared.frames.lock() { *slot = Some(frame); }
                            shared.wake();
                        }
                    }
                    Ok(Some(protocol::Packet::TabFrame(tab, frame))) => {
                        if let Some(shared) = route(&reader_routes, tab) {
                            if let Ok(mut slot) = shared.frames.lock() { *slot = Some(frame); }
                            shared.wake();
                        }
                    }
                    Ok(Some(protocol::Packet::Json(value))) => {
                        if let Err(error) = validate_event(&value) {
                            reader_shared.fail(format!("Invalid Chromium event: {error}"));
                            break;
                        }
                        if value["event"] == "ready" {
                            tracing::info!(cef = ?value["cef_version"], chromium = ?value["chromium_version"], "Chromium engine ready (CPU frame transport)");
                            let _ = ready_tx.try_send(());
                        }
                        if matches!(
                            value["event"].as_str(),
                            Some("download_prompt" | "download" | "download_runtime")
                        ) {
                            if let Some(event) = decode_event(&value) {
                                queue_download_event(&download_events, event);
                                let endpoints: Vec<_> = reader_routes.lock().unwrap().values().filter_map(Weak::upgrade).collect();
                                for endpoint in endpoints { endpoint.wake(); }
                            }
                            continue;
                        }
                        let tab = value["tab"].as_u64().unwrap_or(1) as u32;
                        let retires_route = matches!(
                            value["event"].as_str(),
                            Some("closed" | "tab_detached" | "tab_create_failed")
                        );
                        let endpoint = if retires_route {
                            reader_routes
                                .lock()
                                .unwrap()
                                .remove(&tab)
                                .and_then(|route| route.upgrade())
                        } else {
                            route(&reader_routes, tab)
                        };
                        if let Some(shared) = endpoint {
                            if value["event"] == "open_tab" {
                                if let Some(url) = value["url"].as_str() {
                                    let mut requests = shared.open_tabs.lock().unwrap();
                                    if requests.len() < 32 { requests.push_back((url.into(), value["background"].as_bool().unwrap_or(false))); }
                                    drop(requests); shared.wake();
                                }
                            }
                            if let Some(event) = decode_event(&value) { shared.event(event); }
                        }
                    }
                    Ok(None) => { reader_shared.fail("Chromium engine disconnected".into()); break; }
                    Err(error) => { reader_shared.fail(format!("Invalid Chromium output: {error}")); break; }
                }
            }
            queue_download_event(&download_events, WebViewEvent::DownloadRuntime {
                admitted: 0,
                detached: 0,
                accepting: false,
                error: "Chromium engine disconnected".into(),
            });
            fail_routes(&reader_routes, "Chromium engine disconnected".into());
        }) {
            tx.close();
            let _ = child.kill(); let _ = child.wait();
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
                let mut child = supervisor_child.lock().unwrap().take().unwrap();
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
                        let began = shutdown_started.get_or_insert_with(std::time::Instant::now);
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
                    cleanup_private_root(&root);
                }
            });
        let supervisor = match supervisor {
            Ok(supervisor) => supervisor,
            Err(error) => {
                if let Some(mut child) = child.lock().unwrap().take() {
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
        let shared = Arc::new(Shared::default());
        *shared.waker.lock().unwrap() = self.shared.waker.lock().unwrap().clone();
        routes.insert(tab, Arc::downgrade(&shared));
        if let Err(error) = self.tx.push(json!({"op":"create_tab","tab":tab})) {
            routes.remove(&tab);
            return Err(error.into());
        }
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
        self.shared.open_tabs.lock().unwrap().drain(..).collect()
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
            tracing::error!("Rejected oversized Chromium command");
            if let Some(uri) = navigation {
                self.shared.event(WebViewEvent::LoadFailed {
                    uri,
                    error: "Navigation URL exceeds the supported size".into(),
                });
            }
            return;
        }
        if let Some(mut command) = encode_command(command) {
            command["tab"] = json!(self.tab);
            if let Err(error) = self.tx.push(command) {
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
            } else if let Some(uri) = navigation {
                if let Ok(mut current) = self.shared.uri.lock() {
                    *current = uri;
                }
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
        let mut routes = self.runtime.routes.lock().unwrap();
        if let Some(route) = routes.get_mut(&self.tab) {
            *route = Weak::new();
            let _ = self.tx.push(json!({"op":"close_tab","tab":self.tab}));
        }
    }
}

fn valid_id(value: &Value) -> bool {
    value["id"]
        .as_u64()
        .is_some_and(|id| id > 0 && id <= i32::MAX as u64)
}

fn validate_event(value: &Value) -> Result<(), &'static str> {
    if value.get("tab").is_some_and(|tab| {
        !tab.as_u64()
            .is_some_and(|id| id > 0 && id <= i32::MAX as u64)
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
        "site_status" | "site_result" => {
            let header = value["request_id"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= i32::MAX as u64)
                && value["origin"].as_str().is_some_and(|s| s.len() <= 8192)
                && value["generation"].as_f64().is_some_and(|n| {
                    n.is_finite() && (0.0..=9007199254740991.0).contains(&n) && n.fract() == 0.0
                });
            header
                && if event == "site_result" {
                    value["success"].is_boolean()
                        && value["action"].as_str().is_some_and(|s| {
                            matches!(s, "status" | "clear" | "revoke" | "enable" | "disable")
                        })
                        && value["message"].as_str().is_some_and(|s| s.len() <= 1024)
                } else {
                    ["global_enabled", "excepted", "effective"]
                        .iter()
                        .all(|k| value[*k].is_boolean())
                        && value["blocked"].as_f64().is_some_and(|n| {
                            n.is_finite()
                                && (0.0..=9007199254740991.0).contains(&n)
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
                            .all(|k| value[*k].as_u64().is_some_and(|n| n <= u32::MAX as u64))
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
        "context_closed" => valid_id(value),
        "context_menu" => {
            valid_id(value)
                && value["link"].as_str().is_some_and(|s| s.len() <= 8192)
                && ["x", "y"].iter().all(|k| {
                    value[*k]
                        .as_f64()
                        .is_some_and(|v| v.is_finite() && v.abs() < 100000.0)
                })
                && value["items"].as_array().is_some_and(|items| {
                    items.len() <= 24
                        && items.iter().all(|item| {
                            valid_id(item)
                                && item["label"].as_str().is_some_and(|s| s.len() <= 4096)
                        })
                })
        }
        "find_result" => {
            valid_id(value)
                && ["count", "active"].iter().all(|key| {
                    value[*key]
                        .as_i64()
                        .is_some_and(|n| (-1..=i32::MAX as i64).contains(&n))
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
        "permission_closed" => valid_id(value),
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
                && value["received"]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n >= 0.0)
                && value["total"]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n >= 0.0)
        }
        "download_runtime" => {
            ["admitted", "detached"].iter().all(|key| {
                value[*key].as_u64().is_some_and(|count| count <= 32)
            }) && value["accepting"].is_boolean()
                && value["error"].as_str().is_some_and(|error| error.len() <= 1024)
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
                .is_some_and(|id| id > 0 && id <= i32::MAX as u64)
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
                && *request_id <= i32::MAX as u32
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
                && *request_id <= i32::MAX as u32
                && !origin.is_empty()
                && origin.len() <= 8192
                && *generation > 0
                && *generation <= 9007199254740991
                && matches!(action.as_str(), "clear" | "revoke" | "enable" | "disable")
        }
        WebViewCommand::ContextMenuAction { id, command } => {
            *id > 0 && *id <= i32::MAX as u32 && *command <= i32::MAX as u32
        }
        WebViewCommand::Find { id, text, .. } => {
            *id > 0 && *id <= i32::MAX as u32 && text.len() <= 4096
        }
        WebViewCommand::SetZoomLevel(level) => level.is_finite() && (-5.0..=9.0).contains(level),
        WebViewCommand::RequestNavigationHistory { request_id, .. } => {
            *request_id > 0 && *request_id <= i32::MAX as u32
        }
        WebViewCommand::NavigateToHistoryEntry { request_id, id } => {
            *request_id > 0 && *request_id <= i32::MAX as u32 && *id > 0 && *id <= i32::MAX as u32
        }
        WebViewCommand::RequestMediaElementById { element_id, .. } => element_id.len() <= 256,
        WebViewCommand::PermissionDecision { id, .. }
        | WebViewCommand::DownloadDecision { id, .. }
        | WebViewCommand::CancelDownload { id }
        | WebViewCommand::RetryDownload { id } => *id > 0 && *id <= i32::MAX as u32,
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
                self.0 += data.len();
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
        let mut state = self.state.lock().unwrap();
        if state.2 {
            return Err("command channel closed");
        }
        let op = value["op"].as_str().unwrap_or("");
        // These operations are explicitly replaceable. Keep ordering barriers:
        // mouse motion before a click and resize before navigation stay ordered.
        if matches!(op, "mouse_move" | "resize" | "focus") {
            if state
                .0
                .back()
                .is_some_and(|(last, _)| last["op"] == op && last["tab"] == value["tab"])
            {
                let (_, bytes) = state.0.pop_back().unwrap();
                state.1 -= bytes;
            }
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
        state.1 += count.0;
        state.0.push_back((value, count.0));
        self.ready.notify_one();
        Ok(())
    }
    fn pop(&self) -> Option<Value> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some((value, bytes)) = state.0.pop_front() {
                state.1 -= bytes;
                return Some(value);
            }
            if state.2 {
                return None;
            }
            state = self.ready.wait(state).unwrap();
        }
    }
    fn close(&self) {
        self.state.lock().unwrap().2 = true;
        self.ready.notify_all();
    }
}

fn decode_event(value: &Value) -> Option<WebViewEvent> {
    Some(match value["event"].as_str()? {
        "credential_result" => WebViewEvent::CredentialResult(super::CredentialReply::decode(value)?),
        "closed" => WebViewEvent::TabClosed,
        "tab_detached" => WebViewEvent::TabDetached,
        "close_blocked" => WebViewEvent::TabCloseBlocked,
        "site_status" => WebViewEvent::SiteStatus(super::SiteStatus {
            request_id: value["request_id"].as_u64()? as u32,
            origin: value["origin"].as_str()?.into(),
            generation: value["generation"].as_f64()? as u64,
            blocked: value["blocked"].as_f64()? as u64,
            global_enabled: value["global_enabled"].as_bool()?,
            excepted: value["excepted"].as_bool()?,
            effective: value["effective"].as_bool()?,
            security: value["security"].as_str()?.into(),
            certificate_status: value["certificate_status"].as_u64()? as u32,
            tls_version: value["tls_version"].as_u64()? as u32,
            content_status: value["content_status"].as_u64()? as u32,
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
            request_id: value["request_id"].as_u64()? as u32,
            origin: value["origin"].as_str()?.into(),
            generation: value["generation"].as_f64()? as u64,
            action: value["action"].as_str()?.into(),
            success: value["success"].as_bool()?,
            message: value["message"].as_str()?.into(),
        },
        "context_closed" => WebViewEvent::ContextMenuClosed {
            id: value["id"].as_u64()? as u32,
        },
        "context_menu" => WebViewEvent::ContextMenu {
            id: value["id"].as_u64()? as u32,
            x: value["x"].as_f64()?,
            y: value["y"].as_f64()?,
            link: value["link"].as_str()?.into(),
            items: value["items"]
                .as_array()?
                .iter()
                .map(|item| Some((item["id"].as_u64()? as u32, item["label"].as_str()?.into())))
                .collect::<Option<Vec<_>>>()?,
        },
        "find_result" => WebViewEvent::FindResult {
            id: value["id"].as_u64()? as u32,
            count: value["count"].as_i64()? as i32,
            active: value["active"].as_i64()? as i32,
            final_update: value["final"].as_bool()?,
        },
        "zoom" => WebViewEvent::ZoomChanged(value["level"].as_f64()?),
        "fullscreen" => WebViewEvent::FullscreenChanged(value["fullscreen"].as_bool()?),
        "renderer_terminated" => WebViewEvent::RendererTerminated,
        "uri" => WebViewEvent::UriChanged(value["url"].as_str()?.into()),
        "title" => WebViewEvent::TitleChanged(value["title"].as_str()?.into()),
        "favicon" => {
            let width = value["width"].as_u64()? as u32;
            let height = value["height"].as_u64()? as u32;
            let png = decode_base64_bounded(value["png"].as_str()?, 256 * 1024)?;
            if width == 0 || height == 0 || width > 64 || height > 64 || png.len() > 256 * 1024 {
                return None;
            }
            WebViewEvent::FaviconChanged {
                page_url: value["page_url"].as_str()?.into(),
                width,
                height,
                png: Arc::new(png),
            }
        }
        "audio_muted" => WebViewEvent::AudioMutedChanged(value["muted"].as_bool()?),
        "media_state" => WebViewEvent::MediaPlayingChanged {
            page_url: value["page_url"].as_str()?.into(),
            document_generation: value["document_generation"].as_u64()?,
            playing: value["playing"].as_bool()?,
        },
        "progress" => WebViewEvent::LoadProgress(value["progress"].as_f64()?.clamp(0.0, 1.0)),
        "permission_prompt" => WebViewEvent::PermissionRequested {
            id: value["id"].as_u64()? as u32,
            origin: value["origin"].as_str()?.into(),
            permissions: value["permissions"].as_str()?.into(),
        },
        "permission_closed" => WebViewEvent::PermissionClosed {
            id: value["id"].as_u64()? as u32,
        },
        "download_prompt" => WebViewEvent::DownloadRequested {
            id: value["id"].as_u64()? as u32,
            url: value["url"].as_str()?.into(),
            name: value["name"].as_str()?.into(),
        },
        "download" => WebViewEvent::DownloadUpdated {
            id: value["id"].as_u64()? as u32,
            state: value["state"].as_str()?.into(),
            path: value["path"].as_str()?.into(),
            name: value["name"].as_str()?.into(),
            url: value["url"].as_str()?.into(),
            retryable: value["retryable"].as_bool()?,
            received: value["received"].as_f64()? as u64,
            total: value["total"].as_f64()? as u64,
            error: value["error"].as_str()?.into(),
        },
        "download_runtime" => WebViewEvent::DownloadRuntime {
            admitted: value["admitted"].as_u64()? as u32,
            detached: value["detached"].as_u64()? as u32,
            accepting: value["accepting"].as_bool()?,
            error: value["error"].as_str()?.into(),
        },
        "navigation_history" => WebViewEvent::NavigationHistory {
            request_id: value["request_id"].as_u64()? as u32,
            direction: super::NavigationDirection::from_wire(value["direction"].as_str())?,
            entries: value["entries"]
                .as_array()?
                .iter()
                .map(|entry| {
                    Some(super::NavigationEntry {
                        id: entry["id"].as_u64()? as u32,
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
        "error" => WebViewEvent::LoadFailed {
            uri: value["url"].as_str().unwrap_or_default().into(),
            error: value["message"].as_str()?.into(),
        },
        _ => return None,
    })
}

fn decode_base64_bounded(input: &str, limit: usize) -> Option<Vec<u8>> {
    if input.len() > (limit + 2) / 3 * 4 + 4 {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u8);
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            if out.len() > limit {
                return None;
            }
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
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
        WebViewCommand::CancelDownload { id } => json!({"op":"download_cancel","id":id}),
        WebViewCommand::RetryDownload { id } => json!({"op":"download_retry","id":id}),
        WebViewCommand::CancelAllDownloads => json!({"op":"download_cancel_all"}),
        WebViewCommand::SetNativeFilterRules { rules, enabled } => {
            json!({"op":"filter_rules","rules":&*rules,"enabled":enabled})
        }
        WebViewCommand::SetContentFilter(_) | WebViewCommand::SetContentFilters { .. } => {
            return None;
        }
        WebViewCommand::RequestMediaElement { .. }
        | WebViewCommand::RequestMediaElementById { .. } => return None,
    })
}

#[cfg(test)]
mod tests;
