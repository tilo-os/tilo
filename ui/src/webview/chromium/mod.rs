//! Chromium helper transport, independent of native CEF build dependencies.
mod protocol;

use std::collections::VecDeque;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::{FrameSlot, KeyInput, MouseButton, WebViewCommand, WebViewEngine, WebViewEvent};
use serde_json::{Value, json};

struct Shared {
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
                        | WebViewEvent::LoadProgress(_)
                        | WebViewEvent::HistoryChanged { .. }
                )
            };
            if state(&event) {
                events.retain(|old| std::mem::discriminant(old) != std::mem::discriminant(&event));
            } else if matches!(
                &event,
                WebViewEvent::LoadFinished | WebViewEvent::LoadFailed { .. }
            ) {
                events.retain(|old| {
                    !matches!(
                        old,
                        WebViewEvent::LoadFinished | WebViewEvent::LoadFailed { .. }
                    )
                });
            }
            if events.len() >= 512 {
                if let Some(index) = events.iter().position(|old| {
                    !matches!(
                        old,
                        WebViewEvent::LoadFinished | WebViewEvent::LoadFailed { .. }
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
}

/// Error-only surface used when a real engine cannot start. It never renders a
/// page and repeats the diagnostic on navigation so the browser's normal error
/// presentation remains actionable.
pub struct UnavailableEngine {
    message: String,
    shared: Shared,
}

impl UnavailableEngine {
    pub fn new(message: String) -> Arc<Self> {
        Arc::new(Self {
            message,
            shared: Shared {
                frames: Arc::new(Mutex::new(None)),
                events: Mutex::new(VecDeque::new()),
                waker: Mutex::new(None),
                uri: Mutex::new(String::new()),
                stopping: AtomicBool::new(false),
                failed: AtomicBool::new(false),
            },
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
        let profile = std::env::var_os("QOS_CHROMIUM_PROFILE")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|path| path.join("qos-browser/chromium")))
            .ok_or("Cannot locate Chromium profile directory")?;
        Self::with_host(&host_path()?, &profile)
    }

    /// Explicit runtime/profile paths for packaging and isolated live tests.
    pub fn with_host(host: &Path, profile: &Path) -> Result<Arc<Self>, String> {
        std::fs::create_dir_all(profile).map_err(|error| format!("Chromium profile: {error}"))?;
        let profile = profile.canonicalize().map_err(|error| error.to_string())?;
        let mut child = Command::new(host)
            .arg(format!("--profile={}", profile.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("Could not start Chromium at {}: {error}", host.display()))?;
        let stdin = child.stdin.take().ok_or("Chromium stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("Chromium stdout unavailable")?;
        let shared = Arc::new(Shared {
            frames: Arc::new(Mutex::new(None)),
            events: Mutex::new(VecDeque::new()),
            waker: Mutex::new(None),
            uri: Mutex::new(String::new()),
            stopping: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        });
        let tx = Arc::new(CommandQueue::default());
        let rx = tx.clone();
        let (ready_tx, ready_rx) = sync_channel(1);
        let writer_shared = shared.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("chromium-commands".into())
            .spawn(move || {
                let mut writer = stdin;
                while let Some(command) = rx.pop() {
                    if let Err(error) = protocol::write_json(&mut writer, &command) {
                        writer_shared.fail(format!("Chromium command channel closed: {error}"));
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
        if let Err(error) = std::thread::Builder::new().name("chromium-frames".into()).spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match protocol::read_packet(&mut reader) {
                    Ok(Some(protocol::Packet::Frame(frame))) => {
                        if let Ok(mut slot) = reader_shared.frames.lock() { *slot = Some(frame); }
                        reader_shared.wake();
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
                        if let Some(event) = decode_event(&value) { reader_shared.event(event); }
                    }
                    Ok(None) => { reader_shared.fail("Chromium engine disconnected".into()); break; }
                    Err(error) => { reader_shared.fail(format!("Invalid Chromium output: {error}")); break; }
                }
            }
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
        if let Err(error) = std::thread::Builder::new()
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
            })
        {
            if let Some(mut child) = child.lock().unwrap().take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            tx.close();
            return Err(format!("Chromium supervisor: {error}"));
        }
        Ok(Arc::new(Self { tx, shared }))
    }
}

impl WebViewEngine for ChromiumEngine {
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
        if let Some(command) = encode_command(command) {
            if let Err(error) = self.tx.push(command) {
                if self.shared.failed.load(Ordering::Acquire) {
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

    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut slot) = self.shared.waker.lock() {
            *slot = Some(Arc::from(waker));
        }
        self.shared.wake();
    }
}

impl Drop for ChromiumEngine {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        let _ = self.tx.push(json!({"op":"shutdown"}));
        self.tx.close();
    }
}

fn validate_event(value: &Value) -> Result<(), &'static str> {
    let event = value
        .get("event")
        .and_then(Value::as_str)
        .ok_or("missing event name")?;
    let text = |key: &str| value.get(key).is_some_and(Value::is_string);
    let valid = match event {
        "ready" => text("cef_version") && text("chromium_version"),
        "uri" => text("url"),
        "title" => text("title"),
        "progress" => value["progress"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n)),
        "loaded" => text("url") && value["status"].as_i64().is_some(),
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
        WebViewCommand::Navigate(url) => url.len() <= 65536,
        WebViewCommand::Key {
            key: KeyInput::Character(text),
            ..
        } => text.len() <= 65536,
        WebViewCommand::SetUserScripts(scripts) => {
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
            if state.0.back().is_some_and(|(last, _)| last["op"] == op) {
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
        "uri" => WebViewEvent::UriChanged(value["url"].as_str()?.into()),
        "title" => WebViewEvent::TitleChanged(value["title"].as_str()?.into()),
        "progress" => WebViewEvent::LoadProgress(value["progress"].as_f64()?.clamp(0.0, 1.0)),
        "loaded" => WebViewEvent::LoadFinished,
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

fn encode_command(command: WebViewCommand) -> Option<Value> {
    Some(match command {
        WebViewCommand::Navigate(url) => json!({"op":"navigate","url":url}),
        WebViewCommand::GoBack => json!({"op":"back"}),
        WebViewCommand::GoForward => json!({"op":"forward"}),
        WebViewCommand::Reload => json!({"op":"reload"}),
        WebViewCommand::StopLoading => json!({"op":"stop"}),
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
        WebViewCommand::SetUserScripts(scripts) => json!({"op":"scripts","scripts": &*scripts}),
        WebViewCommand::ClearWebsiteData => json!({"op":"clear_data"}),
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
