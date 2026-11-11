//! Browser environments: one microVM per workspace that runs a headless Tilo
//! session with Browser, and the spares kept booted for the next one.
//!
//! A workspace hosted on a relay opens Browser like any app, and in a hosted
//! workspace the page has to run somewhere: not on the relay (it runs no
//! apps) and not in the workspace's Linux VM (which holds a compiler, not an
//! engine). So a workspace gets a **browser environment**: the same
//! Firecracker VM of this service, on the browser guest image
//! ([`Guest::Browser`](super::guest::Guest)), with these differences:
//!
//! - **Its disk is fresh from the image every boot**, it is never
//!   snapshotted, and when it stops its disk, meter and state are deleted:
//!   cookies, storage and downloads go with it.
//! - **It is started directly**, by the relay's request
//!   ([`BrowserOpen`](super::control::Request)), not by a shell run.
//! - **Its size is [`VmSize::Browser`](super::size::VmSize)** (2 vCPU, 2 GiB,
//!   an 8 GiB disk), so the placer and the host's memory budget weigh it like
//!   any other VM while no account's plan admits it.
//! - **It ends by itself**: ten minutes without page activity, two hours
//!   alive, or its session ending.
//!
//! # The session in the guest
//!
//! The host starts the guest's session through the guest agent
//! ([`legacy_agent_protocol::Launch`]), relaying its stdio; there is no other
//! channel, and no addition to the agent's protocol. What the guest image
//! must therefore ship, and what the joining code must do, is exactly this:
//!
//! - **argv** [`SESSION`], the guest image's own entry: the script that makes
//!   the runtime directory, points the session at the image's software Vulkan
//!   driver and starts it on its own bus, headless and with no X11
//!   (`os/hosted-browser-guest/browser-session`). [`MARK`] is set in its
//!   environment: a session that shows its window nowhere and knows it serves
//!   a hosted browser environment.
//! - **stdout, one line each, nothing else**
//!   ([`Report`](tilo_actions::remote::browser::Report)): `ready` once it has
//!   taken the identity it was handed, and before it is due to wait for
//!   anything; then `up <provider>` once it has joined the workspace and
//!   opened the workspace's Browser window, with the provider token the
//!   window publishes under; then `used` whenever a page action runs on it or
//!   somebody views the page. The host counts the idle limit from the last of
//!   them, and hands the provider to the relay, which routes page actions to
//!   that window.
//! - **stdin, one line** at hand-off, written before it is told anything else:
//!   the material the relay gives, opaque here — the JSON that names the
//!   workspace, the record to open and the device certificate the session
//!   joins with ([`tilo_relay_protocol::hosted::Material`]) — with the relay's
//!   address replaced by the one this VM dials ([`hand_off`]). One line, and
//!   well under a megabyte: the host relays it as the session's input, one
//!   line at a time. Its identity is read once, before it builds anything, so
//!   the session of an environment starts at that hand-off and never before.
//! - **stderr** is the environment's log, nothing else.
//!
//! A spare is a booted VM whose session has not started: opening Browser
//! starts it, with the material as the first line instead of a boot. A spare
//! keeps the name it booted with (`spare/browser-<n>`) when it serves a
//! workspace; the placer's and the relay's name for that environment is
//! `<workspace>/browser`, which is what places and routes it. An environment
//! serves one workspace, is never handed back, and never shares a disk, a
//! device or a process with another.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iroh_base::{EndpointAddr, TransportAddr};
use legacy_agent_protocol::client::{self, LaunchIo, LaunchSignals};
use legacy_agent_protocol::link::{AgentLink, AgentStream};
use legacy_agent_protocol::{AGENT_PORT, Display, Launch, ProtocolError, Signal, Stdio};
use thiserror::Error;
use tilo_actions::remote::browser::{MARK, Report, UP};
use tilo_relay_protocol::hosted::Material;

use super::firecracker::Machine;
use super::network::Endpoint;

/// The session a browser environment runs, as the guest image ships it: the
/// script that sets the environment up and starts the session on it.
pub const SESSION: &str = "/usr/libexec/tilo/browser-session";

/// How long the guest agent is given to start a session it was asked for.
pub const READY_PATIENCE: Duration = Duration::from_secs(120);

/// How long an environment's session is given to report `up` after the
/// material was handed to it.
pub const UP_PATIENCE: Duration = Duration::from_secs(60);

/// The line a session is handed at hand-off: `material`, the relay's, with
/// the address its session joins the relay at replaced by `relay` — the
/// endpoint's id stays, so the session joins the same relay, at the one
/// address this VM dials. Nothing else of the material changes.
///
/// # Errors
///
/// The material is not the relay's, or names no endpoint: the words to
/// refuse the hand-off with.
pub fn hand_off(material: &str, relay: Endpoint) -> Result<String, String> {
    let mut material: Material = serde_json::from_str(material.trim())
        .map_err(|error| format!("the material is not the relay's: {error}"))?;
    // An endpoint address is its id and where it is reached; the session
    // dials the address written here, and no other.
    material.relay =
        EndpointAddr::from_parts(material.relay.id, [TransportAddr::Ip(relay.socket())]);
    serde_json::to_string(&material).map_err(|error| error.to_string())
}

#[cfg(test)]
/// The material the relay writes a session, joining the workspace at
/// `relay`: what the tests of a hand-off start from.
pub fn test_material(relay: &str) -> String {
    let cert = tilo_user_identity::DeviceCert {
        user_public_key: [1; 32],
        device_public_key: [2; 32],
        device_label: "Browser".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1_000,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    let material = Material {
        workspace: "ws1".into(),
        workspace_id: 7,
        record: "browser-1".into(),
        device: cert,
        secret: [2; 32],
        relay: EndpointAddr::from_parts(
            test_relay_id(),
            [TransportAddr::Ip(relay.parse().unwrap())],
        ),
        user_public_key: [1; 32],
        account: "acct-1".into(),
    };
    serde_json::to_string(&material).unwrap_or_default()
}

#[cfg(test)]
/// The endpoint id of [`test_material`]'s relay.
fn test_relay_id() -> iroh_base::EndpointId {
    iroh_base::SecretKey::from_bytes(&[7; 32]).public()
}

/// The browser environments a host runs: what it boots them with, the relay
/// their sessions join the workspace through, how many spares it keeps, and
/// how long one lives.
#[derive(Clone, Debug)]
pub struct BrowserGuest {
    /// The browser guest image: its kernel and the disk every environment
    /// starts as a copy of.
    pub machine: Machine,
    /// Where the relay is, as the host reaches it: the one destination an
    /// environment's VM may reach besides the internet. A relay on the
    /// host's own loopback (a local stack) is reached through
    /// [`super::network::HOST_LOOPBACK`]; anywhere else (a cluster address),
    /// the VM dials it as it is.
    pub relay: Endpoint,
    /// Spares kept booted with a session waiting for a workspace.
    pub spares: usize,
    /// No page activity for this long ends an environment.
    pub idle_stop: Duration,
    /// Alive for this long ends an environment, whatever happens.
    pub lifetime: Duration,
}

impl BrowserGuest {
    /// The defaults of a host that runs browser environments: two spares,
    /// ten minutes idle, two hours alive.
    #[must_use]
    pub fn new(machine: Machine, relay: Endpoint) -> Self {
        Self {
            machine,
            relay,
            spares: 2,
            idle_stop: Duration::from_secs(600),
            lifetime: Duration::from_secs(2 * 60 * 60),
        }
    }

    #[must_use]
    pub const fn limits(&self) -> Limits {
        Limits {
            idle_stop: self.idle_stop,
            lifetime: self.lifetime,
        }
    }
}

/// How long a browser environment may live, and be idle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub idle_stop: Duration,
    pub lifetime: Duration,
}

/// Why a browser environment ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum End {
    /// No page activity for the idle limit.
    Idle,
    /// Alive for the lifetime limit.
    Lifetime,
    /// Its session ended.
    Session,
    /// A spare no workspace took, and no spare is wanted.
    Surplus,
}

impl fmt::Display for End {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Idle => "no page activity",
            Self::Lifetime => "alive for its lifetime",
            Self::Session => "its session ended",
            Self::Surplus => "a spare nobody took",
        })
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Agent(#[from] ProtocolError),
    #[error("the session did not start within {0:?}")]
    Silent(Duration),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One browser environment's life, as its session reports it: when it booted,
/// when it was last used, whether it is up, whether it is being handed a
/// workspace, and whether its session ended.
#[derive(Debug, Default)]
pub struct Life {
    state: Mutex<State>,
    /// Woken whenever the state moves.
    moved: Condvar,
}

#[derive(Debug, Default)]
struct State {
    booted_ms: u64,
    ready_ms: Option<u64>,
    used_ms: u64,
    up: bool,
    /// The provider token of the window its session opened, once it is up.
    provider: Option<String>,
    opening: bool,
    ended: Option<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Life {
    #[must_use]
    pub fn new(booted_ms: u64) -> Self {
        Self {
            state: Mutex::new(State {
                booted_ms,
                used_ms: booted_ms,
                ..State::default()
            }),
            moved: Condvar::new(),
        }
    }

    pub fn report(&self, report: Report, now_ms: u64) {
        {
            let mut state = lock(&self.state);
            match report {
                Report::Ready => {
                    state.ready_ms.get_or_insert(now_ms);
                    state.used_ms = state.used_ms.max(now_ms);
                }
                Report::Up { provider } => {
                    state.up = true;
                    state.opening = false;
                    state.provider = Some(provider);
                    state.used_ms = state.used_ms.max(now_ms);
                }
                Report::Used => {
                    if state.up {
                        state.used_ms = state.used_ms.max(now_ms);
                    }
                }
            }
        }
        self.moved.notify_all();
    }

    /// The session in this environment ended: it can serve no workspace.
    pub fn ended(&self, why: String) {
        lock(&self.state).ended = Some(why);
        self.moved.notify_all();
    }

    #[must_use]
    pub fn ready(&self) -> bool {
        lock(&self.state).ready_ms.is_some()
    }

    /// Whether its session reported a page used since [`Self::report`] last
    /// counted one: `used` is one of its reports, and reads as such.
    #[must_use]
    pub const fn used() -> Report {
        Report::Used
    }

    #[must_use]
    pub fn up(&self) -> bool {
        lock(&self.state).up
    }

    /// The provider token of the window this environment holds, once its
    /// session opened it: what page actions on it name.
    #[must_use]
    pub fn provider(&self) -> Option<String> {
        lock(&self.state).provider.clone()
    }

    #[must_use]
    pub fn ended_because(&self) -> Option<String> {
        lock(&self.state).ended.clone()
    }

    /// Take the hand-off if nobody else has: whether this caller hands this
    /// environment the workspace's material.
    fn begin_opening(&self) -> bool {
        let mut state = lock(&self.state);
        if state.up || state.opening {
            return false;
        }
        state.opening = true;
        true
    }

    /// Wait until the session prints `report`, or `patience` passes.
    fn wait_for(
        &self,
        patience: Duration,
        report: &dyn Fn(&State) -> bool,
        what: &str,
    ) -> Result<(), String> {
        let started = Instant::now();
        let mut state = lock(&self.state);
        loop {
            if report(&state) {
                return Ok(());
            }
            if let Some(why) = &state.ended {
                return Err(why.clone());
            }
            let Some(left) = patience
                .checked_sub(started.elapsed())
                .filter(|left| !left.is_zero())
            else {
                return Err(format!(
                    "the browser environment's session did not report {what} within {patience:?}"
                ));
            };
            let (again, _) = self
                .moved
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner);
            state = again;
        }
    }

    /// Wait until the session has joined its workspace.
    pub fn wait_up(&self, patience: Duration) -> Result<(), String> {
        self.wait_for(patience, &|state| state.up, UP)
    }

    /// Whether this environment is at its end by now, and why.
    #[must_use]
    pub fn expired(&self, now_ms: u64, limits: Limits) -> Option<End> {
        let (ended, up, used_ms, booted_ms) = {
            let state = lock(&self.state);
            (
                state.ended.is_some(),
                state.up,
                state.used_ms,
                state.booted_ms,
            )
        };
        if ended {
            return Some(End::Session);
        }
        let millis = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        if up && now_ms.saturating_sub(used_ms) >= millis(limits.idle_stop) {
            return Some(End::Idle);
        }
        if now_ms.saturating_sub(booted_ms) >= millis(limits.lifetime) {
            return Some(End::Lifetime);
        }
        None
    }
}

/// The session running in one browser environment, driven through the guest
/// agent: its stdin is where the workspace's material goes, its stdout is
/// what it reports ([`Report`]), and its stderr is the environment's log.
pub struct Session {
    /// Its process, to end it.
    signals: Option<LaunchSignals<AgentStream>>,
    ended: Arc<AtomicBool>,
}

impl Session {
    /// Start the session in the guest `link` reaches for the workspace
    /// `material` names, reporting into `life`, logging to `log`, timed by
    /// `clock`: the material is the first thing its standard input is told.
    pub fn start(
        link: &AgentLink,
        material: &str,
        life: Arc<Life>,
        log: PathBuf,
        clock: super::vm::Clock,
    ) -> Result<Self, SessionError> {
        let stream = link.connect(AGENT_PORT)?;
        let launch = Launch {
            argv: vec![SESSION.to_owned()],
            cwd: None,
            env: vec![(MARK.to_owned(), "1".to_owned())],
            stdio: Stdio::Relay,
            display: Display::None,
        };
        let (input, lines) = mpsc::channel::<String>();
        let _ = input.send(material.trim_end().to_owned());
        let (telling, told) = mpsc::channel::<LaunchSignals<AgentStream>>();
        let ended = Arc::new(AtomicBool::new(false));
        let ending = Arc::clone(&ended);
        std::thread::spawn(move || {
            let mut out = Reporter::new(&log, Arc::clone(&life), clock);
            let mut err = Log::new(&log);
            let result = client::launch(
                stream,
                &launch,
                LaunchIo {
                    stdin: Some(Box::new(Lines {
                        lines,
                        pending: Vec::new(),
                    })),
                    stdout: &mut out,
                    stderr: &mut err,
                    started: &mut |_| {},
                },
                move |signals| {
                    let _ = telling.send(signals);
                },
            );
            ending.store(true, Ordering::SeqCst);
            life.ended(match result {
                Ok(status) => format!("its session exited ({})", status.shell_code()),
                Err(error) => format!("its session ended: {error}"),
            });
        });
        let signals = told
            .recv_timeout(READY_PATIENCE)
            .map_err(|_| SessionError::Silent(READY_PATIENCE))?;
        Ok(Self {
            signals: Some(signals),
            ended,
        })
    }

    /// End the session in the guest.
    pub fn kill(&self) {
        if let Some(signals) = &self.signals {
            let _ = signals.send(Signal::Kill);
        }
    }

    /// Whether the session's process ended.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A session's stdin, fed a line at a time.
struct Lines {
    lines: mpsc::Receiver<String>,
    /// What is left of the last line.
    pending: Vec<u8>,
}

impl Read for Lines {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pending.is_empty() {
            match self.lines.recv() {
                Ok(line) => {
                    self.pending = line.into_bytes();
                    self.pending.push(b'\n');
                }
                // Nothing more to hand it: its stdin closes.
                Err(_) => return Ok(0),
            }
        }
        let read = buf.len().min(self.pending.len());
        if let (Some(into), Some(from)) = (buf.get_mut(..read), self.pending.get(..read)) {
            into.copy_from_slice(from);
        }
        self.pending.drain(..read);
        Ok(read)
    }
}

/// A session's stdout: the log, and what it reports.
struct Reporter {
    log: Log,
    line: Vec<u8>,
    life: Arc<Life>,
    now: super::vm::Clock,
}

impl Reporter {
    fn new(path: &std::path::Path, life: Arc<Life>, now: super::vm::Clock) -> Self {
        Self {
            log: Log::new(path),
            line: Vec::new(),
            life,
            now,
        }
    }

    fn take_line(&mut self) {
        let line = String::from_utf8_lossy(&self.line).into_owned();
        self.line.clear();
        if let Some(report) = Report::parse(&line) {
            self.life.report(report, (self.now)());
        }
    }
}

impl Write for Reporter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.log.write_all(buf)?;
        for byte in buf {
            if *byte == b'\n' {
                self.take_line();
            } else if self.line.len() < 4096 {
                self.line.push(*byte);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.log.flush()
    }
}

/// A session's stderr: the environment's log.
struct Log {
    file: Option<std::fs::File>,
}

impl Log {
    fn new(path: &std::path::Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        Self { file }
    }
}

impl Write for Log {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(file) = &mut self.file {
            file.write_all(buf)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }
}

/// What starts the session in a booted environment: a spare's guest agent is
/// answered here, and its session starts when a workspace is handed to it.
pub trait Starts: Send + Sync {
    /// Start the session with `material` as the first line of its input.
    ///
    /// # Errors
    ///
    /// Why no session started: the guest agent refused it, or did not answer.
    fn start(&self, material: &str) -> Result<Session, SessionError>;
}

/// The session of one booted environment, started through the guest agent of
/// the VM it runs in.
pub struct Launcher {
    link: AgentLink,
    life: Arc<Life>,
    log: PathBuf,
    clock: super::vm::Clock,
}

impl Launcher {
    #[must_use]
    pub fn new(link: AgentLink, life: Arc<Life>, log: PathBuf, clock: super::vm::Clock) -> Self {
        Self {
            link,
            life,
            log,
            clock,
        }
    }
}

impl Starts for Launcher {
    fn start(&self, material: &str) -> Result<Session, SessionError> {
        Session::start(
            &self.link,
            material,
            Arc::clone(&self.life),
            self.log.clone(),
            Arc::clone(&self.clock),
        )
    }
}

/// One browser environment as the host holds it: its life, what starts its
/// session, and the session once a workspace was handed to it.
pub struct Browser {
    life: Arc<Life>,
    launcher: Mutex<Option<Box<dyn Starts>>>,
    session: Mutex<Option<Session>>,
}

impl fmt::Debug for Browser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Browser")
            .field("up", &self.life.up())
            .field("session", &lock(&self.session).is_some())
            .finish_non_exhaustive()
    }
}

impl Browser {
    /// An environment that booted at `booted_ms`, whose session has not
    /// started yet.
    #[must_use]
    pub fn new(booted_ms: u64) -> Self {
        Self {
            life: Arc::new(Life::new(booted_ms)),
            launcher: Mutex::new(None),
            session: Mutex::new(None),
        }
    }

    /// An environment whose life is `life`, whose guest agent answers, and
    /// whose session `starts` when a workspace is handed to it.
    #[must_use]
    pub fn booted(life: Arc<Life>, starts: Box<dyn Starts>) -> Self {
        Self {
            life,
            launcher: Mutex::new(Some(starts)),
            session: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn life(&self) -> &Arc<Life> {
        &self.life
    }

    /// The session in this environment started.
    pub fn attach(&self, session: Session) {
        *lock(&self.session) = Some(session);
    }

    /// Hand this environment `material` and wait until its session reports it
    /// is up: the session starts here, with the material as the first line of
    /// its input. `None` when it was already up: this request attached to the
    /// environment a person already opened.
    pub fn open(&self, material: &str, patience: Duration) -> Result<Option<u64>, String> {
        if !self.life.begin_opening() {
            // Someone else handed it over: follow that one in.
            self.life.wait_up(patience)?;
            return Ok(None);
        }
        let started = Instant::now();
        let handed = match lock(&self.launcher).as_ref() {
            Some(starts) => starts.start(material).map_err(|error| error.to_string()),
            None => Err("the browser environment has no session".to_owned()),
        };
        match handed {
            Ok(session) => *lock(&self.session) = Some(session),
            Err(error) => {
                // It cannot serve a workspace without a session: it is at its
                // end, and the workspace opens a fresh one.
                self.life.ended(error.clone());
                return Err(error);
            }
        }
        if let Err(error) = self.life.wait_up(patience) {
            self.life.ended(error.clone());
            return Err(error);
        }
        Ok(Some(
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        ))
    }

    /// End its session in the guest.
    pub fn kill_session(&self) {
        if let Some(session) = lock(&self.session).as_ref() {
            session.kill();
        } else {
            self.life.ended("the environment stopped".to_owned());
        }
    }

    /// Whether this environment is at its end by now, and why.
    #[must_use]
    pub fn expired(&self, now_ms: u64, limits: Limits) -> Option<End> {
        self.life.expired(now_ms, limits)
    }
}

/// One environment the pool holds, with the name its VM has on the host.
#[derive(Clone)]
pub struct Named {
    /// `<workspace>/<environment>`, as the service names its VMs.
    pub environment: String,
    pub browser: Arc<Browser>,
}

/// The browser environments of one host: the spares booted for the next
/// workspace, and which environment serves which workspace.
#[derive(Default)]
pub struct Pool {
    spares: Vec<Named>,
    open: std::collections::BTreeMap<String, Named>,
    /// Spares booting now, so no more are started than wanted.
    booting: usize,
    /// The number the next spare's name takes.
    next: u64,
}

/// What the host has to do for its browser environments now.
#[derive(Debug, Default)]
pub struct Work {
    /// Spares to boot.
    pub boot: usize,
    /// Environments to end and delete: their names, and why.
    pub end: Vec<(String, End)>,
}

/// The environment name a workspace's browser environment has, the name the
/// placer and the relay use for it.
#[must_use]
pub fn environment(workspace: &str) -> String {
    format!("{workspace}/{BROWSER}")
}

/// What a workspace's browser environment is called after the workspace.
pub const BROWSER: &str = "browser";

impl Pool {
    /// The environment serving `workspace`, when it has one.
    #[must_use]
    pub fn serving(&self, workspace: &str) -> Option<Named> {
        self.open.get(workspace).cloned()
    }

    /// The environment that serves `workspace` from now on: the one already
    /// serving it, or a spare it takes. `None` when no spare is free, and the
    /// workspace boots its own.
    pub fn claim(&mut self, workspace: &str) -> Option<Named> {
        if let Some(serving) = self.open.get(workspace) {
            return Some(serving.clone());
        }
        let spare = self.spares.pop()?;
        self.open.insert(workspace.to_owned(), spare.clone());
        Some(spare)
    }

    /// `named` serves `workspace`, unless another request got there first:
    /// whether it did.
    pub fn serve(&mut self, workspace: &str, named: Named) -> bool {
        if self.open.contains_key(workspace) {
            return false;
        }
        self.open.insert(workspace.to_owned(), named);
        true
    }

    /// The name the next spare booted gets: `spare/browser-<n>`, counted as
    /// booting until [`Self::spare_booted`] or [`Self::spare_failed`].
    pub fn spare_opening(&mut self) -> String {
        self.booting = self.booting.saturating_add(1);
        self.next = self.next.saturating_add(1);
        format!("spare/{BROWSER}-{}", self.next)
    }

    /// A spare booted, waiting for a workspace.
    pub fn spare_booted(&mut self, named: Named) {
        self.booting = self.booting.saturating_sub(1);
        self.spares.push(named);
    }

    /// A spare that could not be booted.
    pub fn spare_failed(&mut self) {
        self.booting = self.booting.saturating_sub(1);
    }

    /// Stop serving `workspace`: its environment, which the host ends.
    pub fn close(&mut self, workspace: &str) -> Option<Named> {
        self.open.remove(workspace)
    }

    /// Every open environment, for a host that is being taken back.
    #[must_use]
    pub fn opened(&self) -> Vec<Named> {
        self.open.values().cloned().collect()
    }

    #[must_use]
    pub fn spares(&self) -> usize {
        self.spares.len()
    }

    /// End what is at its end, drop spares nobody should have, and boot the
    /// spares that are wanted and missing.
    pub fn maintain(&mut self, now_ms: u64, limits: Limits, wanted: usize) -> Work {
        let mut work = Work::default();
        let mut ending = Vec::new();
        for (workspace, named) in &self.open {
            if let Some(end) = named.browser.expired(now_ms, limits) {
                ending.push((workspace.clone(), named.environment.clone(), end));
            }
        }
        for (workspace, environment, end) in ending {
            self.open.remove(&workspace);
            work.end.push((environment, end));
        }
        let mut kept = Vec::new();
        for named in self.spares.drain(..) {
            if let Some(end) = named.browser.expired(now_ms, limits) {
                work.end.push((named.environment, end));
            } else if kept.len() < wanted {
                kept.push(named);
            } else {
                work.end.push((named.environment, End::Surplus));
            }
        }
        self.spares = kept;
        work.boot = wanted.saturating_sub(self.spares.len().saturating_add(self.booting));
        work
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        idle_stop: Duration::from_secs(600),
        lifetime: Duration::from_secs(7_200),
    };

    fn environment(name: &str) -> Named {
        Named {
            environment: name.to_owned(),
            browser: Arc::new(Browser::new(1_000_000)),
        }
    }

    #[test]
    fn the_line_handed_to_a_session_keeps_the_relays_id_and_its_dialable_address() {
        let handed = hand_off(
            &test_material("203.0.113.9:7704"),
            Endpoint {
                address: "10.0.2.4".parse().unwrap(),
                port: 7704,
            },
        )
        .unwrap();
        let line: Material = serde_json::from_str(&handed).unwrap();
        assert_eq!(line.relay.id, test_relay_id(), "the same relay");
        assert_eq!(
            line.relay.addrs,
            [TransportAddr::Ip("10.0.2.4:7704".parse().unwrap())]
                .into_iter()
                .collect(),
            "at the one address this VM dials"
        );
        assert_eq!(line.workspace, "ws1");
        assert_eq!(line.record, "browser-1");
        assert_eq!(line.device.device_label, "Browser");
        assert_eq!(line.account, "acct-1");
        // phase: a material that is not the relay's is refused, not handed on
        for wrong in ["{}", r#"{"relay": "an address"}"#, "not json"] {
            assert!(
                hand_off(
                    wrong,
                    Endpoint {
                        address: [10, 0, 2, 4].into(),
                        port: 1
                    }
                )
                .is_err(),
                "{wrong}"
            );
        }
    }

    #[test]
    fn a_session_reports_ready_up_and_use() {
        let life = Life::new(1_000);
        assert!(!life.ready() && life.expired(1_000, LIMITS).is_none());
        life.report(Report::Ready, 2_000);
        life.report(Report::Used, 3_000);
        assert!(life.ready());
        assert!(!life.up(), "a spare is not up: it serves no workspace");
        assert_eq!(
            life.expired(8_000_000, LIMITS),
            Some(End::Lifetime),
            "a spare that was never taken still ends when its lifetime is up"
        );
        life.report(
            Report::Up {
                provider: "42".into(),
            },
            5_000,
        );
        assert!(life.up());
        assert_eq!(life.provider().as_deref(), Some("42"));
        // phase: the idle limit counts from the last use, not from the boot
        assert_eq!(life.expired(4_000, LIMITS), None);
        assert_eq!(life.expired(604_999, LIMITS), None);
        assert_eq!(life.expired(605_000, LIMITS), Some(End::Idle));
        life.report(Report::Used, 700_000);
        assert_eq!(life.expired(1_200_000, LIMITS), None, "use resets it");
        // phase: its session ending ends it at once
        life.ended("its session exited (1)".into());
        assert_eq!(life.expired(1_200_000, LIMITS), Some(End::Session));
        assert_eq!(
            life.ended_because().as_deref(),
            Some("its session exited (1)")
        );
    }

    /// A guest agent a test drives: it hands what it was told to start to the
    /// test, and answers with a session that reports `up` when the test says
    /// so.
    struct FakeGuest {
        told: Mutex<Option<mpsc::Sender<String>>>,
        life: Arc<Life>,
        up: bool,
    }

    impl Starts for FakeGuest {
        fn start(&self, material: &str) -> Result<Session, SessionError> {
            let told = lock(&self.told).take();
            if let Some(told) = told {
                let _ = told.send(material.to_owned());
            }
            if self.up {
                let life = Arc::clone(&self.life);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(30));
                    life.report(Report::Ready, 2_500);
                    life.report(
                        Report::Up {
                            provider: "42".into(),
                        },
                        3_000,
                    );
                });
            }
            Ok(Session {
                signals: None,
                ended: Arc::new(AtomicBool::new(false)),
            })
        }
    }

    #[test]
    fn one_hand_off_starts_the_session_with_the_material_and_the_second_attaches() {
        let life = Arc::new(Life::new(1_000));
        let (told, handed) = mpsc::channel();
        let browser = Arc::new(Browser::booted(
            Arc::clone(&life),
            Box::new(FakeGuest {
                told: Mutex::new(Some(told)),
                life: Arc::clone(&life),
                up: true,
            }),
        ));
        // phase: the hand-off starts the session with the material and waits
        // for `up`
        let opening = {
            let browser = Arc::clone(&browser);
            std::thread::spawn(move || browser.open("{\"workspace\":\"ws1\"}", UP_PATIENCE))
        };
        assert_eq!(
            handed.recv_timeout(Duration::from_secs(10)).unwrap(),
            "{\"workspace\":\"ws1\"}",
            "the session starts with the material as its first line"
        );
        // phase: a second request for the same workspace attaches to it
        let attaching = {
            let browser = Arc::clone(&browser);
            std::thread::spawn(move || browser.open("{\"workspace\":\"ws1\"}", UP_PATIENCE))
        };
        let opened = opening.join().unwrap().unwrap();
        assert!(
            opened.is_some(),
            "this request started the session and handed it the workspace"
        );
        assert_eq!(
            life.provider().as_deref(),
            Some("42"),
            "the window it holds is what the provider names"
        );
        assert_eq!(attaching.join().unwrap(), Ok(None), "this one attached");
        assert!(
            handed.try_recv().is_err(),
            "the material is handed once, never again"
        );
        // phase: a request after it is up attaches without waiting
        assert_eq!(
            browser.open("{\"workspace\":\"ws1\"}", Duration::from_millis(1)),
            Ok(None)
        );
        assert_eq!(browser.life().expired(3_100, LIMITS), None);
    }

    /// A session that never comes up takes the environment with it, so the
    /// workspace opens a fresh one instead of waiting for a wedged spare.
    #[test]
    fn a_session_that_never_reports_up_ends_the_environment() {
        let life = Arc::new(Life::new(1_000));
        let browser = Browser::booted(
            Arc::clone(&life),
            Box::new(FakeGuest {
                told: Mutex::new(None),
                life: Arc::clone(&life),
                up: false,
            }),
        );
        let error = browser
            .open("{}", Duration::from_millis(20))
            .expect_err("nothing came up");
        assert!(error.contains("up"), "{error}");
        assert_eq!(life.expired(1_100, LIMITS), Some(End::Session));
    }

    #[test]
    fn an_environment_whose_session_is_gone_never_serves_a_workspace() {
        let browser = Browser::new(1_000);
        assert_eq!(
            browser
                .open("{}", Duration::from_millis(1))
                .err()
                .as_deref(),
            Some("the browser environment has no session")
        );
        assert_eq!(
            browser.life().expired(1_100, LIMITS),
            Some(End::Session),
            "and it is ended, so the workspace opens a fresh one"
        );
    }

    #[test]
    fn a_spare_is_taken_once_and_replaced() {
        let mut pool = Pool::default();
        let spare = pool.spare_opening();
        assert_eq!(spare, "spare/browser-1");
        pool.spare_booted(environment(&spare));
        assert_eq!(pool.spares(), 1);
        assert!(pool.claim("ws1").is_some(), "the spare is handed over");
        assert_eq!(pool.spares(), 0);
        assert_eq!(
            pool.claim("ws1").map(|named| named.environment),
            Some(spare),
            "the workspace keeps the environment it was handed"
        );
        assert!(pool.claim("ws2").is_none(), "a taken spare is never shared");
        assert!(pool.serving("ws2").is_none());
        // phase: a replacement spare is wanted, and no more than wanted
        let work = pool.maintain(1_000_000, LIMITS, 2);
        assert_eq!(work.boot, 2);
        assert!(work.end.is_empty());
        pool.spare_booted(environment("spare/browser-2"));
        pool.spare_booted(environment("spare/browser-3"));
        let work = pool.maintain(1_000_000, LIMITS, 1);
        assert_eq!(
            work.end,
            [("spare/browser-3".to_owned(), End::Surplus)],
            "one too many"
        );
        assert_eq!(pool.spares(), 1);
        assert_eq!(
            pool.maintain(1_000_000, LIMITS, 1).boot,
            0,
            "it has its spare"
        );
    }

    #[test]
    fn an_environment_ends_by_itself_and_the_workspace_may_open_again() {
        let mut pool = Pool::default();
        assert!(pool.serve("ws1", environment("ws1/browser")));
        assert!(
            !pool.serve("ws1", environment("ws1/browser")),
            "one per workspace"
        );
        pool.spare_booted(environment("spare/browser-1"));
        let browser = pool.serving("ws1").unwrap().browser;
        browser.life().report(Report::Ready, 1_000_000);
        browser.life().report(
            Report::Up {
                provider: "42".into(),
            },
            1_000_000,
        );
        assert_eq!(pool.maintain(1_000_100, LIMITS, 1).end, Vec::new());
        let work = pool.maintain(1_600_000, LIMITS, 1);
        assert_eq!(
            work.end,
            [("ws1/browser".to_owned(), End::Idle)],
            "ten minutes without a page ends it, and its disk with it"
        );
        assert!(
            pool.serving("ws1").is_none(),
            "the workspace opens a new one"
        );
        assert_eq!(pool.spares(), 1, "the spare is untouched");
    }

    #[test]
    fn a_session_reports_on_the_lines_it_prints() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("session.log");
        let life = Arc::new(Life::new(0));
        let clock: crate::hosted::vm::Clock = Arc::new(|| 7_000);
        let mut reporter = Reporter::new(&log, Arc::clone(&life), clock);
        reporter.write_all(b"booting\nrea").unwrap();
        assert!(!life.ready());
        reporter.write_all(b"dy\nup 42\nsomething else\n").unwrap();
        reporter.flush().unwrap();
        assert!(life.ready() && life.up());
        assert_eq!(life.provider().as_deref(), Some("42"));
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("booting\nready\nup 42\n"),
            "everything it printed is the environment's log"
        );
        // phase: only whole lines that are reports count
        let life = Arc::new(Life::new(0));
        let mut reporter = Reporter::new(&log, life, Arc::new(|| 7_000));
        reporter.write_all(b"notup\nused\n").unwrap();
        assert!(!reporter.life.up(), "`notup` is not `up`");
        assert_eq!(reporter.life.provider(), None, "and names no window");
        assert_eq!(reporter.life.expired(0, LIMITS), None);
    }
}
