//! Remote's engine work, as a package reaches it: an SSH shell, a VNC desktop, the input a
//! person sends one, and the files that cross an SSH session.
//!
//! Streams (`engine.stream`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `ssh` | `{host, port, user, method, secret_key, key_path, expected, columns, rows}` | `{job, session}`: the grid's session, kept by the toolkit |
//! | `vnc` | `{host, port, password_key}` | `{job}`: the newest frame at `tilo-live://<job>` |
//!
//! Calls (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `ssh-send` | `{job, text}` | `{}` once the remote shell took the text |
//! | `keep-secret` | `{key, secret}` | `{}` once the workspace's vault keeps it under `key` |
//!
//! Jobs (`engine.start`, `engine.cancel`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `put` | `{job, local_path, remote_path}` | the bytes moved |
//! | `fetch` | `{job, remote_path, local_path}` | the bytes moved |
//!
//! A password or key passphrase is kept once (`keep-secret`) in the store this process keeps the
//! app's secrets in, which is the workspace's vault for the Remote app's own process, as the
//! app's own item under the key the app names. A stream's ask names that key, and the secret is
//! read from the vault into this process for the one session it signs in.
//!
//! What a stream says of itself is JSON a view decodes: `{"phase":"connecting"}`,
//! `{"phase":"connected"}`, `{"phase":"need-trust","algorithm":…,"fingerprint":…}` after a first
//! contact, `{"phase":"key-changed","expected":…,"observed":…}`, `{"phase":"closed"}`, or
//! `{"phase":"failed","why":…}`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use serde::Deserialize;
use serde_json::json;
use tilo_engine_loader::{Device, EngineApi, Frames, JobOp, JobSink, Needs};
use tilo_remote::asked::{self, KeepAsk, SshAsk, VncAsk, ssh_word, vnc_word};
use tilo_remote::{
    Moved, SshSession, SshStatus, Transfer, TransferDirection, VncInput, VncSession, VncStatus,
    keysym, vnc_wheel_clicks,
};
use tilo_terminal::FarSide;
use tilo_terminal::live::Live;

use runtime_dylib::tilo_ui_base::key_input::{
    self, Button, Held, Key as UiKey, Keys, Named as UiNamed,
};
use runtime_dylib::ui;

/// The one app this device answers remote work for.
pub const APP: &str = "so.tilo.remote";

/// The jobs this engine runs: files moving over an SSH session, under the app's own network.
fn job_ops() -> &'static [JobOp] {
    const JOBS: &[JobOp] = &[
        JobOp::new("put", Needs::NETWORK),
        JobOp::new("fetch", Needs::NETWORK),
    ];
    JOBS
}

// ------------------------------------------------------------------ the sessions this process keeps

/// A session this engine started for the app, by the job the app names it by.
struct Entry {
    live: LiveKind,
    /// What the stream last said of itself, as JSON.
    last: Mutex<String>,
    /// The person stopped the session or the work it runs.
    stopped: Arc<AtomicBool>,
}

enum LiveKind {
    /// An SSH shell: the session the shared terminal draws, and the connection behind it.
    Shell {
        tool: Arc<Live>,
        ssh: Arc<SshSession>,
    },
    /// A VNC desktop: its frames reach the app's window as they arrive.
    Desktop {
        /// Held while the job is: the session ends when its last handle goes.
        _session: Arc<VncSession>,
    },
}

fn held() -> MutexGuard<'static, BTreeMap<String, Arc<Entry>>> {
    static SESSIONS: OnceLock<Mutex<BTreeMap<String, Arc<Entry>>>> = OnceLock::new();
    SESSIONS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The entry `job` names, when this engine keeps it.
fn entry(job: &str) -> Option<Arc<Entry>> {
    held().get(job).cloned()
}

/// The transfers this engine started, by their job's path: what a cancel stops.
fn transfers() -> MutexGuard<'static, BTreeMap<String, Arc<AtomicBool>>> {
    static TRANSFERS: OnceLock<Mutex<BTreeMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    TRANSFERS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Remember a stream's newest word, and hand it to every view reading it.
fn say(frames: Frames, job: &str, last: &Mutex<String>, word: String) {
    frames.state(job, &word);
    *last.lock().unwrap_or_else(PoisonError::into_inner) = word;
}

fn new_job(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

// ------------------------------------------------------------------ asks

/// The secret the app kept under `key`, read from the store this process keeps the app's
/// secrets in: the workspace's vault, for the app's own process.
fn kept(key: &str) -> Result<Option<String>, String> {
    let secret = ui::storage::secrets(APP)
        .get_blocking(key)
        .map_err(|error| error.to_string())?;
    Ok(secret.map(|secret| String::from_utf8_lossy(secret.expose()).into_owned()))
}

/// Keep `secret` under `key` in the store this process keeps the app's secrets in.
fn keep(ask: &KeepAsk) -> Result<(), String> {
    ui::storage::secrets(APP)
        .put_blocking(&ask.key, ask.secret.as_bytes(), Default::default())
        .map_err(|error| format!("The workspace's vault did not keep the password: {error}"))
}

#[derive(Deserialize)]
struct SendAsk {
    job: String,
    text: String,
}

#[derive(Deserialize)]
struct TransferAsk {
    /// The SSH stream's job: the session the file moves over.
    job: String,
    #[serde(default)]
    local_path: Option<String>,
    #[serde(default)]
    remote_path: Option<String>,
}

// ------------------------------------------------------------------ streams

/// One `engine.stream`: an SSH shell or a VNC desktop, answered with its job.
///
/// # Errors
///
/// An op this engine runs no stream for, an ask that does not read, a saved password the vault
/// did not hand over, or a session that could not be kept.
fn stream(device: &Device, op: &str, input: &str, frames: &Frames) -> Result<String, String> {
    match op {
        "ssh" => ssh_stream(device, input, *frames),
        "vnc" => vnc_stream(input, *frames),
        other => Err(format!("`{other}` is no stream this device runs for {APP}")),
    }
}

fn ssh_stream(device: &Device, input: &str, frames: Frames) -> Result<String, String> {
    let ask: SshAsk = reads(input)?;
    let (columns, rows) = (ask.columns.max(2), ask.rows.max(2));
    let config = asked::ssh_config(ask, &kept)?;
    let _ = device;
    let job = new_job("ssh");
    let last = Arc::new(Mutex::new(String::new()));
    let said_job = job.clone();
    let said_frames = frames;
    let said_last = Arc::clone(&last);
    let said = Box::new(move |status: SshStatus| {
        let word = ssh_word(&status);
        say(said_frames, &said_job, &said_last, word);
    });
    let ssh = SshSession::start(config, said)?;
    let output = ssh
        .take_output()
        .ok_or_else(|| "the SSH session gave no output".to_owned())?;
    let ssh = Arc::new(ssh);
    let far = FarSide {
        writer: Box::new(ShellInput(Arc::clone(&ssh))),
        reader: Box::new(ShellOutput(output)),
        resize: Box::new({
            let ssh = Arc::clone(&ssh);
            move |columns, rows| ssh.resize(columns, rows)
        }),
        close: Box::new({
            let ssh = Arc::clone(&ssh);
            move || ssh.close()
        }),
        shell_name: "ssh".to_owned(),
        ended: Arc::default(),
        relay_session: None,
        commands: None,
    };
    let tool = Live::carrying(
        far,
        tilo_content::ContentStore::memory(),
        columns,
        rows,
        None,
    )
    .map_err(|error| format!("the SSH shell could not be shown: {error}"))?;
    let session = tool.id().to_owned();
    held().insert(
        job.clone(),
        Arc::new(Entry {
            live: LiveKind::Shell {
                tool,
                ssh: Arc::clone(&ssh),
            },
            last: Mutex::new(String::new()),
            stopped: Arc::new(AtomicBool::new(false)),
        }),
    );
    // The first word is said once the app holds the job, so it is never read before it exists.
    frames.state(&job, &ssh_word(&SshStatus::Connecting));
    Ok(json!({ "job": job, "session": session }).to_string())
}

fn vnc_stream(input: &str, frames: Frames) -> Result<String, String> {
    let ask: VncAsk = reads(input)?;
    let config = asked::vnc_config(ask, &kept)?;
    let job = new_job("vnc");
    let last = Arc::new(Mutex::new(String::new()));
    let said_job = job.clone();
    let said_frames = frames;
    let said_last = Arc::clone(&last);
    let said = Box::new(move |status: VncStatus| {
        let word = vnc_word(&status);
        say(said_frames, &said_job, &said_last, word);
    });
    let saw_job = job.clone();
    let saw_frames = frames;
    let size = Arc::new(Mutex::new((0_u16, 0_u16)));
    let saw_size = Arc::clone(&size);
    let saw = Box::new(move |frame: tilo_remote::Framebuffer| {
        *saw_size.lock().unwrap_or_else(PoisonError::into_inner) = (frame.width, frame.height);
        saw_frames.frame(
            &saw_job,
            frame.generation,
            u32::from(frame.width),
            u32::from(frame.height),
            &frame.data,
        );
    });
    let desktop = VncSession::start(config, said, saw)?;
    let desktop = Arc::new(desktop);
    key_input::provide(
        &job,
        Arc::new(DesktopInput {
            desktop: Arc::clone(&desktop),
            size,
            pointer: Mutex::new((0.0, 0.0)),
            wheel: Mutex::new((0.0, 0.0)),
            held: Mutex::new(tilo_remote::Held::default()),
        }),
    );
    held().insert(
        job.clone(),
        Arc::new(Entry {
            live: LiveKind::Desktop { _session: desktop },
            last: Mutex::new(String::new()),
            stopped: Arc::new(AtomicBool::new(false)),
        }),
    );
    frames.state(&job, &vnc_word(&VncStatus::Connecting));
    Ok(json!({ "job": job }).to_string())
}

// ------------------------------------------------------------------ calls and jobs

/// One `engine.call`: text sent into a connected SSH shell, or a password kept for a connection.
///
/// # Errors
///
/// An op this engine answers none of, an ask that does not read, no such session, or a shell that
/// did not take the text.
fn call(_device: &Device, op: &str, input: &str) -> Result<String, String> {
    match op {
        "ssh-send" => {
            let ask: SendAsk = reads(input)?;
            let entry = entry(&ask.job)
                .ok_or_else(|| "that remote session is no longer open".to_owned())?;
            let LiveKind::Shell { ssh, .. } = &entry.live else {
                return Err("text input requires a connected SSH terminal".to_owned());
            };
            if ssh.status() != SshStatus::Connected {
                return Err("text input requires a connected SSH terminal".to_owned());
            }
            ssh.send_input_ack(ask.text.into_bytes())?;
            Ok(json!({}).to_string())
        }
        "keep-secret" => {
            keep(&reads::<KeepAsk>(input)?)?;
            Ok(json!({}).to_string())
        }
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

/// One `engine.start`: a file moved over an SSH session, run on this process's thread.
///
/// # Errors
///
/// An op this engine starts none of, an ask that does not read, a local path that is not
/// absolute, or no such session.
fn start(
    _device: &Device,
    job: &str,
    op: &str,
    input: &str,
    moved: &JobSink,
) -> Result<(), String> {
    match op {
        "put" | "fetch" => start_transfer(job, op, input, *moved),
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

fn start_transfer(job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
    let ask: TransferAsk = reads(input)?;
    let entry =
        entry(&ask.job).ok_or_else(|| "that remote session is no longer open".to_owned())?;
    let LiveKind::Shell { ssh, .. } = &entry.live else {
        return Err(
            "file transfer needs a connected SSH session; this session is a remote desktop"
                .to_owned(),
        );
    };
    if ssh.status() != SshStatus::Connected {
        return Err("file transfer needs a connected SSH session".to_owned());
    }
    let ssh = Arc::clone(ssh);
    let remote = ask
        .remote_path
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| "the remote path is empty".to_owned())?;
    let local = absolute(ask.local_path)?;
    let direction = if op == "put" {
        TransferDirection::Put {
            local_path: local,
            remote_path: remote,
        }
    } else {
        TransferDirection::Fetch {
            remote_path: remote,
            local_path: local,
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    transfers().insert(job.to_owned(), Arc::clone(&stop));
    let named = job.to_owned();
    std::thread::Builder::new()
        .name("remote-transfer".to_owned())
        .spawn(move || {
            let progress = Arc::new(move |moved_by: Moved| {
                let detail = match moved_by {
                    Moved::Measured { done, total } => {
                        json!({ "done": done, "total": total })
                    }
                    Moved::Seen { seen } => json!({ "seen": seen }),
                };
                moved.moved("running", &json!({ "moved": detail }).to_string());
            });
            let result = ssh.transfer(
                direction,
                Transfer {
                    progress,
                    stopped: Arc::clone(&stop),
                },
            );
            let (state, detail) = match result {
                Ok(bytes) => ("done", json!({ "bytes": bytes })),
                Err(why) if stop.load(Ordering::Acquire) => ("cancelled", json!({ "why": why })),
                Err(why) => ("failed", json!({ "why": why })),
            };
            moved.moved(state, &detail.to_string());
            transfers().remove(&named);
        })
        .map_err(|error| format!("the transfer did not start: {error}"))?;
    Ok(())
}

/// A path an action named, which must be whole: a relative one would land wherever this runs.
fn absolute(path: Option<String>) -> Result<std::path::PathBuf, String> {
    let path = path.ok_or_else(|| "the local path is empty".to_owned())?;
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() {
        return Err(format!("`{}` is not an absolute path", path.display()));
    }
    Ok(path)
}

/// How a job a loaded engine runs stands, for `engine.job` and `engine.status`.
///
/// # Errors
///
/// Nothing this engine knows started `job`.
fn status(_device: &Device, job: &str) -> Result<String, String> {
    if transfers().contains_key(job) {
        return Ok(json!({ "state": "running" }).to_string());
    }
    let entry = entry(job).ok_or_else(|| format!("no job `{job}` runs on this device"))?;
    let last = entry
        .last
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let phase: serde_json::Value = serde_json::from_str(&last).unwrap_or(serde_json::Value::Null);
    let state = match phase.get("phase").and_then(|phase| phase.as_str()) {
        Some("failed" | "key-changed") => "failed",
        Some("closed") => "done",
        _ => "running",
    };
    Ok(json!({ "state": state, "detail": phase }).to_string())
}

/// Stop the job `job`: a transfer stops between its reads, and a session ends.
fn cancel(_device: &Device, job: &str) -> bool {
    let transfer = transfers().remove(job);
    if let Some(stop) = transfer {
        stop.store(true, Ordering::Release);
        return true;
    }
    let Some(entry) = held().remove(job) else {
        return false;
    };
    entry.stopped.store(true, Ordering::Release);
    match &entry.live {
        LiveKind::Shell { tool, ssh } => {
            ssh.close();
            tool.close();
        }
        LiveKind::Desktop { .. } => {
            key_input::withdraw(job);
            ui::live_pictures::forget(job);
        }
    }
    true
}

// ------------------------------------------------------------------ what a shell reads and writes

/// The terminal's input for an SSH shell: its grid's keys reach the connection.
struct ShellInput(Arc<SshSession>);

impl Write for ShellInput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .send_input(bytes.to_vec())
            .map_err(std::io::Error::other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What the shell printed, as the grid's reader reads it.
struct ShellOutput(std::sync::mpsc::Receiver<Vec<u8>>);

impl Read for ShellOutput {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self.0.recv() {
            Ok(chunk) => {
                let taken = chunk.len().min(buffer.len());
                for (into, byte) in buffer.iter_mut().zip(&chunk) {
                    *into = *byte;
                }
                Ok(taken)
            }
            Err(_) => Ok(0),
        }
    }
}

// ------------------------------------------------------------------ a desktop's pointer and keys

/// The destination a VNC desktop's pointer, buttons, wheel and keys go to. The app's picture
/// names the stream's job; this keeps where the pointer is over the picture and what the person
/// holds down ([`tilo_remote::Held`]), so losing the keyboard lets go of it on the desktop too.
struct DesktopInput {
    desktop: Arc<VncSession>,
    /// The remote framebuffer's size, as the newest frame said it.
    size: Arc<Mutex<(u16, u16)>>,
    /// The pointer's last place over the picture, as fractions of it.
    pointer: Mutex<(f64, f64)>,
    /// Wheel movement not yet sent, in whole clicks.
    wheel: Mutex<(f64, f64)>,
    /// The keys, modifiers and buttons held, and where the pointer is in the desktop's pixels.
    held: Mutex<tilo_remote::Held>,
}

impl DesktopInput {
    /// Where the pointer is, in the remote framebuffer's pixels.
    fn target(&self) -> Option<(u16, u16)> {
        let (width, height) = *self.size.lock().unwrap_or_else(PoisonError::into_inner);
        if width == 0 || height == 0 {
            return None;
        }
        let (x, y) = *self.pointer.lock().unwrap_or_else(PoisonError::into_inner);
        // A fraction in 0..=1 of a pixel count that fits a u16 is a pixel that fits one too.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=1 of a u16 extent"
        )]
        let to = |fraction: f64, end: u16| {
            (fraction.clamp(0.0, 1.0) * f64::from(end.saturating_sub(1))).round() as u16
        };
        Some((to(x, width), to(y, height)))
    }

    /// Change what is held with `change`, and tell the desktop what it answers.
    fn hold(&self, change: impl FnOnce(&mut tilo_remote::Held) -> Vec<VncInput>) {
        let said = change(&mut self.held.lock().unwrap_or_else(PoisonError::into_inner));
        for input in said {
            let _ = self.desktop.input(input);
        }
    }

    /// The modifiers `held` says are down, as the keysyms the desktop reads.
    fn modifiers(held: Held) -> Vec<u32> {
        [
            (held.shift, tilo_remote::input::modifier::SHIFT),
            (held.ctrl, tilo_remote::input::modifier::CONTROL),
            (held.alt, tilo_remote::input::modifier::ALT),
            (held.sup, tilo_remote::input::modifier::SUPER),
        ]
        .into_iter()
        .filter_map(|(down, key)| down.then_some(key))
        .collect()
    }
}

impl Keys for DesktopInput {
    fn key(&self, key: UiKey, _text: Option<&str>, held: Held) {
        let down = Self::modifiers(held);
        self.hold(|desk| {
            let mut said = desk.modifiers(&down);
            if let Some(keysym) = keysym(ui_key(key)) {
                said.extend(desk.press(keysym));
            }
            said
        });
    }

    fn key_up(&self, key: UiKey) {
        if let Some(keysym) = keysym(ui_key(key)) {
            self.hold(|desk| desk.release(keysym));
        }
    }

    fn text(&self, text: &str) {
        for character in text.chars() {
            if let Some(keysym) = keysym(tilo_remote::Key::Char(character)) {
                let _ = self.desktop.input(VncInput::Key {
                    down: true,
                    key: keysym,
                });
                let _ = self.desktop.input(VncInput::Key {
                    down: false,
                    key: keysym,
                });
            }
        }
    }

    fn paste(&self, text: &str) {
        self.text(text);
    }

    fn mouse(
        &self,
        _button: Button,
        _pressed: bool,
        _held: Held,
        _column: usize,
        _row: usize,
    ) -> bool {
        false
    }

    fn pointer(&self, x: f64, y: f64, _held: Held) {
        *self.pointer.lock().unwrap_or_else(PoisonError::into_inner) = (x, y);
        if let Some(at) = self.target() {
            self.hold(|desk| desk.point(at));
        }
    }

    fn button(&self, button: Button, pressed: bool, _held: Held) {
        let bit = match button {
            Button::Left => 0b001,
            Button::Middle => 0b010,
            Button::Right => 0b100,
        };
        self.hold(|desk| desk.button(bit, pressed));
    }

    fn wheel(&self, across: f64, down: f64) {
        let clicks = {
            let mut pending = self.wheel.lock().unwrap_or_else(PoisonError::into_inner);
            vnc_wheel_clicks(&mut pending, across, down)
        };
        self.hold(|desk| desk.wheel(&clicks));
    }

    fn focus(&self, focused: bool) {
        if !focused {
            self.hold(tilo_remote::Held::let_go);
        }
    }
}

fn ui_key(key: UiKey) -> tilo_remote::Key {
    match key {
        UiKey::Char(character) => tilo_remote::Key::Char(character),
        UiKey::Named(named) => tilo_remote::Key::Named(match named {
            UiNamed::Enter => tilo_remote::Named::Enter,
            UiNamed::Space => tilo_remote::Named::Space,
            UiNamed::Backspace => tilo_remote::Named::Backspace,
            UiNamed::Tab => tilo_remote::Named::Tab,
            UiNamed::Escape => tilo_remote::Named::Escape,
            UiNamed::ArrowUp => tilo_remote::Named::ArrowUp,
            UiNamed::ArrowDown => tilo_remote::Named::ArrowDown,
            UiNamed::ArrowLeft => tilo_remote::Named::ArrowLeft,
            UiNamed::ArrowRight => tilo_remote::Named::ArrowRight,
            UiNamed::Home => tilo_remote::Named::Home,
            UiNamed::End => tilo_remote::Named::End,
            UiNamed::PageUp => tilo_remote::Named::PageUp,
            UiNamed::PageDown => tilo_remote::Named::PageDown,
            UiNamed::Insert => tilo_remote::Named::Insert,
            UiNamed::Delete => tilo_remote::Named::Delete,
            UiNamed::Function(number) => tilo_remote::Named::Function(number),
        }),
    }
}

// ------------------------------------------------------------------ the entry

fn reads<'de, T: Deserialize<'de>>(input: &'de str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))
}

/// The one engine this library answers with.
static API: EngineApi = EngineApi {
    app: APP,
    jobs: job_ops,
    call,
    start,
    status,
    stream,
    cancel,
};

/// The entry the process opens this library for.
#[expect(
    clippy::no_mangle_with_rust_abi,
    reason = "the loader reads it from a Rust dylib built by the same compiler, as a Rust fn"
)]
#[unsafe(no_mangle)]
pub fn tilo_engine_entry() -> &'static EngineApi {
    &API
}

/// Proves this engine loads and runs against the runtime it is built with: the words a stream
/// says and the keysym a key carries.
pub fn self_check() -> Result<String, String> {
    let said = ssh_word(&SshStatus::Connected);
    if !said.contains("connected") {
        return Err(format!("a session's word does not read: {said}"));
    }
    let key = keysym(tilo_remote::Key::Char('a')).ok_or("a character carries no keysym")?;
    Ok(format!("a session says {said}; a key is {key:#x}"))
}
