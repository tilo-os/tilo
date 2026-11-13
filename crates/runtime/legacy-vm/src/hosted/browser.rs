//! Browser in a workspace's VM: the headless Tilo session that holds the
//! workspace's Browser window, run in the same VM as its Linux programs. The host
//! starts it through the guest agent and relays its stdio: argv is the guest
//! image's own entry ([`SESSION`]), stdout reports one line each (`ready`, `up
//! <provider>`, `used`), and stdin is handed one line of material. The session ends
//! after [`Limits::idle_stop`] without page activity, or at [`Limits::lifetime`].

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

use super::network::Endpoint;

/// The browser session's entry, as the guest image ships it.
pub const SESSION: &str = "/usr/libexec/tilo/browser-session";

/// The Linux apps session's entry, as the guest image ships it: the same
/// headless session, run as the workspace's tenant with Xwayland, holding the
/// windows of the Linux apps the workspace opened
/// ([`tilo_actions::remote::linux_apps`]).
pub const APPS_SESSION: &str = "/usr/libexec/tilo/apps-session";

/// Which session of a workspace's VM: the one holding its Browser, or the one holding its Linux
/// apps' windows. Both join the workspace as a device, run as different accounts and are never the
/// same process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// Browser, as the image's unprivileged browser user.
    Browser,
    /// The workspace's Linux apps, as its tenant: each later line of the
    /// session's input is one app to start.
    Apps,
}

impl Kind {
    #[must_use]
    pub const fn entry(self) -> &'static str {
        match self {
            Self::Browser => SESSION,
            Self::Apps => APPS_SESSION,
        }
    }

    #[must_use]
    pub const fn mark(self) -> &'static str {
        match self {
            Self::Browser => MARK,
            Self::Apps => tilo_actions::remote::linux_apps::MARK,
        }
    }

    #[must_use]
    pub const fn log(self) -> &'static str {
        match self {
            Self::Browser => "browser.log",
            Self::Apps => "apps.log",
        }
    }
}

/// Names, in the session's environment, the folder it keeps its profile in.
pub const STORE: &str = "TILO_BROWSER_STORE";

/// How long the guest agent is given to start a session it was asked for.
pub const READY_PATIENCE: Duration = Duration::from_secs(120);

/// How long a session is given to report `up` after the material was handed
/// to it.
pub const UP_PATIENCE: Duration = Duration::from_secs(60);

/// The line a session is handed at hand-off: the relay's material, with the address the session
/// dials replaced by `relay`. The endpoint's id stays, so it joins the same relay at the one
/// address this VM dials.
///
/// # Errors
///
/// The material is not the relay's, or names no endpoint.
pub fn hand_off(material: &str, relay: Endpoint) -> Result<String, String> {
    let mut material: Material = serde_json::from_str(material.trim())
        .map_err(|error| format!("the material is not the relay's: {error}"))?;
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
        workspace_id: WorkspaceId::from_u128(7),
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
fn test_relay_id() -> iroh_base::EndpointId {
    iroh_base::SecretKey::from_bytes(&[7; 32]).public()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub idle_stop: Duration,
    pub lifetime: Duration,
}

impl Default for Limits {
    /// Ten minutes idle, two hours alive.
    fn default() -> Self {
        Self {
            idle_stop: Duration::from_secs(600),
            lifetime: Duration::from_secs(2 * 60 * 60),
        }
    }
}

/// Why a browser session ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum End {
    Idle,
    Lifetime,
    Session,
}

impl fmt::Display for End {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Idle => "no page activity",
            Self::Lifetime => "alive for its lifetime",
            Self::Session => "its session ended",
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

/// One browser session's life, as it reports it: when it started, when it was last used, and
/// whether it is up or ended.
#[derive(Debug, Default)]
pub struct Life {
    state: Mutex<State>,
    moved: Condvar,
}

#[derive(Debug, Default)]
struct State {
    started_ms: u64,
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
    pub fn new(started_ms: u64) -> Self {
        Self {
            state: Mutex::new(State {
                started_ms,
                used_ms: started_ms,
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

    /// The session ended: it serves the workspace no more.
    pub fn ended(&self, why: String) {
        lock(&self.state).ended = Some(why);
        self.moved.notify_all();
    }

    #[must_use]
    pub fn ready(&self) -> bool {
        lock(&self.state).ready_ms.is_some()
    }

    /// Whether its session reported a page used since the report last counted one: `used` is one of
    /// its reports, and reads as such.
    #[must_use]
    pub const fn used() -> Report {
        Report::Used
    }

    #[must_use]
    pub fn up(&self) -> bool {
        lock(&self.state).up
    }

    #[must_use]
    pub fn provider(&self) -> Option<String> {
        lock(&self.state).provider.clone()
    }

    #[must_use]
    pub fn ended_because(&self) -> Option<String> {
        lock(&self.state).ended.clone()
    }

    /// Take the hand-off if nobody else has: whether this caller hands the
    /// session the workspace's material.
    fn begin_opening(&self) -> bool {
        let mut state = lock(&self.state);
        if state.up || state.opening {
            return false;
        }
        state.opening = true;
        true
    }

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
                    "the browser session did not report {what} within {patience:?}"
                ));
            };
            let (again, _) = self
                .moved
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner);
            state = again;
        }
    }

    pub fn wait_up(&self, patience: Duration) -> Result<(), String> {
        self.wait_for(patience, &|state| state.up, UP)
    }

    #[must_use]
    pub fn expired(&self, now_ms: u64, limits: Limits) -> Option<End> {
        let (ended, up, used_ms, started_ms) = {
            let state = lock(&self.state);
            (
                state.ended.is_some(),
                state.up,
                state.used_ms,
                state.started_ms,
            )
        };
        if ended {
            return Some(End::Session);
        }
        let millis = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        if up && now_ms.saturating_sub(used_ms) >= millis(limits.idle_stop) {
            return Some(End::Idle);
        }
        if now_ms.saturating_sub(started_ms) >= millis(limits.lifetime) {
            return Some(End::Lifetime);
        }
        None
    }
}

/// The session running in a workspace's VM, driven through the guest agent: its stdin is where the
/// material goes, its stdout what it reports, and its stderr its log.
pub struct Session {
    signals: Option<LaunchSignals<AgentStream>>,
    /// Its standard input, a line at a time: the material went first.
    input: Option<mpsc::Sender<String>>,
    ended: Arc<AtomicBool>,
}

impl Session {
    /// Start the session in the guest `link` reaches for the workspace `material` names, reporting
    /// into `life`, logging to `log`, timed by `clock`: the material is the first thing its input
    /// is told.
    pub fn start(
        link: &AgentLink,
        kind: Kind,
        material: &str,
        store: Option<&std::path::Path>,
        life: Arc<Life>,
        log: PathBuf,
        clock: super::vm::Clock,
    ) -> Result<Self, SessionError> {
        let stream = link.connect(AGENT_PORT)?;
        let mut env = vec![(kind.mark().to_owned(), "1".to_owned())];
        if let Some(store) = store {
            env.push((STORE.to_owned(), store.display().to_string()));
        }
        let launch = Launch {
            argv: vec![kind.entry().to_owned()],
            cwd: None,
            env,
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
            input: Some(input),
            ended,
        })
    }

    fn tell(&self, line: &str) -> Result<(), String> {
        self.input
            .as_ref()
            .and_then(|input| input.send(line.trim_end().to_owned()).ok())
            .ok_or_else(|| "the session reads no more input".to_owned())
    }

    fn signal(&self, signal: Signal) {
        if let Some(signals) = &self.signals {
            let _ = signals.send(signal);
        }
    }

    pub fn kill(&self) {
        self.signal(Signal::Kill);
    }

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

/// What starts the session in a workspace's VM, once the workspace's material
/// is handed to it.
pub trait Starts: Send + Sync {
    /// Start the session with `material` as the first line of its input.
    ///
    /// # Errors
    ///
    /// Why no session started: the guest agent refused it, or did not answer.
    fn start(&self, material: &str) -> Result<Session, SessionError>;
}

pub struct Launcher {
    link: AgentLink,
    kind: Kind,
    /// Where the session keeps its profile, when the workspace keeps one.
    store: Option<PathBuf>,
    life: Arc<Life>,
    log: PathBuf,
    clock: super::vm::Clock,
}

impl Launcher {
    #[must_use]
    pub fn new(
        link: AgentLink,
        kind: Kind,
        store: Option<PathBuf>,
        life: Arc<Life>,
        log: PathBuf,
        clock: super::vm::Clock,
    ) -> Self {
        Self {
            link,
            kind,
            store,
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
            self.kind,
            material,
            self.store.as_deref(),
            Arc::clone(&self.life),
            self.log.clone(),
            Arc::clone(&self.clock),
        )
    }
}

/// A workspace's browser session as the host holds it: its life, what starts
/// it, and the session once the workspace's material was handed to it.
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
    /// A session that cannot start: its VM's agent was never reached.
    #[must_use]
    pub fn new(started_ms: u64) -> Self {
        Self {
            life: Arc::new(Life::new(started_ms)),
            launcher: Mutex::new(None),
            session: Mutex::new(None),
        }
    }

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

    /// Hand the session `material` and wait until it reports it is up; the session starts here,
    /// with the material as the first line of its input. `None` when it was already up.
    pub fn open(&self, material: &str, patience: Duration) -> Result<Option<u64>, String> {
        if !self.life.begin_opening() {
            self.life.wait_up(patience)?;
            return Ok(None);
        }
        let started = Instant::now();
        let handed = match lock(&self.launcher).as_ref() {
            Some(starts) => starts.start(material).map_err(|error| error.to_string()),
            None => Err("the workspace's VM has no browser session".to_owned()),
        };
        match handed {
            Ok(session) => *lock(&self.session) = Some(session),
            Err(error) => {
                // It is at its end, and the next open starts a fresh one.
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

    /// Tell the running session `line` on its standard input: what an apps session is asked to
    /// start, after it was handed its workspace.
    ///
    /// # Errors
    ///
    /// No session runs, or it reads no more input.
    pub fn tell(&self, line: &str) -> Result<(), String> {
        lock(&self.session)
            .as_ref()
            .ok_or_else(|| "the workspace's VM runs no such session".to_owned())?
            .tell(line)
    }

    /// End the session in the guest: asked with `SIGTERM`, so Chromium writes its profile to the
    /// VM's disk, and killed when it has not ended within `grace`.
    pub fn end(&self, grace: Duration) {
        let session = lock(&self.session).take();
        let Some(session) = session else {
            self.life.ended("its VM stopped".to_owned());
            return;
        };
        session.signal(Signal::Terminate);
        let started = Instant::now();
        while !session.ended() && started.elapsed() < grace {
            std::thread::sleep(Duration::from_millis(20));
        }
        session.kill();
    }

    #[must_use]
    pub fn expired(&self, now_ms: u64, limits: Limits) -> Option<End> {
        self.life.expired(now_ms, limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        idle_stop: Duration::from_secs(600),
        lifetime: Duration::from_secs(7_200),
    };

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
        assert!(!life.up(), "not up until its window is");
        assert_eq!(
            life.expired(8_000_000, LIMITS),
            Some(End::Lifetime),
            "a session that never came up still ends when its lifetime is up"
        );
        life.report(
            Report::Up {
                provider: "42".into(),
            },
            5_000,
        );
        assert!(life.up());
        assert_eq!(life.provider().as_deref(), Some("42"));
        assert_eq!(life.expired(4_000, LIMITS), None);
        assert_eq!(life.expired(604_999, LIMITS), None);
        assert_eq!(life.expired(605_000, LIMITS), Some(End::Idle));
        life.report(Report::Used, 700_000);
        assert_eq!(life.expired(1_200_000, LIMITS), None, "use resets it");
        life.ended("its session exited (1)".into());
        assert_eq!(life.expired(1_200_000, LIMITS), Some(End::Session));
        assert_eq!(
            life.ended_because().as_deref(),
            Some("its session exited (1)")
        );
    }

    struct FakeGuest {
        told: Mutex<Option<mpsc::Sender<String>>>,
        input: Option<mpsc::Sender<String>>,
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
                input: self.input.clone(),
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
                input: None,
                life: Arc::clone(&life),
                up: true,
            }),
        ));
        let opening = {
            let browser = Arc::clone(&browser);
            std::thread::spawn(move || browser.open("{\"workspace\":\"ws1\"}", UP_PATIENCE))
        };
        assert_eq!(
            handed.recv_timeout(Duration::from_secs(10)).unwrap(),
            "{\"workspace\":\"ws1\"}",
            "the session starts with the material as its first line"
        );
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
        assert_eq!(
            browser.open("{\"workspace\":\"ws1\"}", Duration::from_millis(1)),
            Ok(None)
        );
        assert_eq!(browser.life().expired(3_100, LIMITS), None);
    }

    /// A Linux apps session is handed its workspace once, and each app after that is a line of its
    /// input, to the session already running.
    #[test]
    fn an_apps_session_is_told_each_app_after_its_material() {
        let life = Arc::new(Life::new(1_000));
        let (told, handed) = mpsc::channel();
        let (input, lines) = mpsc::channel();
        let apps = Browser::booted(
            Arc::clone(&life),
            Box::new(FakeGuest {
                told: Mutex::new(Some(told)),
                input: Some(input),
                life: Arc::clone(&life),
                up: true,
            }),
        );
        assert!(
            apps.tell("{}").is_err(),
            "nothing is told a session that was never started"
        );
        assert!(
            apps.open("{\"workspace\":\"ws1\"}", UP_PATIENCE)
                .unwrap()
                .is_some()
        );
        apps.tell("{\"desktop_id\":\"xterm\"}\n").unwrap();
        apps.tell("{\"desktop_id\":\"org.gnome.TextEditor\"}")
            .unwrap();
        assert_eq!(handed.try_recv().unwrap(), "{\"workspace\":\"ws1\"}");
        assert_eq!(
            lines.try_iter().collect::<Vec<_>>(),
            [
                "{\"desktop_id\":\"xterm\"}",
                "{\"desktop_id\":\"org.gnome.TextEditor\"}"
            ],
            "each app is one line, after the material"
        );
        assert_eq!(Kind::Apps.entry(), APPS_SESSION);
        assert_ne!(Kind::Apps.mark(), Kind::Browser.mark());
    }

    /// A session that never comes up is at its end, so the next open starts a fresh one instead of
    /// waiting for a wedged one.
    #[test]
    fn a_session_that_never_reports_up_is_at_its_end() {
        let life = Arc::new(Life::new(1_000));
        let browser = Browser::booted(
            Arc::clone(&life),
            Box::new(FakeGuest {
                told: Mutex::new(None),
                input: None,
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
    fn a_vm_whose_agent_was_never_reached_starts_no_session() {
        let browser = Browser::new(1_000);
        assert_eq!(
            browser
                .open("{}", Duration::from_millis(1))
                .err()
                .as_deref(),
            Some("the workspace's VM has no browser session")
        );
        assert_eq!(
            browser.life().expired(1_100, LIMITS),
            Some(End::Session),
            "and it is ended, so the next open starts a fresh one"
        );
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
            "everything it printed is its log"
        );
        let life = Arc::new(Life::new(0));
        let mut reporter = Reporter::new(&log, life, Arc::new(|| 7_000));
        reporter.write_all(b"notup\nused\n").unwrap();
        assert!(!reporter.life.up(), "`notup` is not `up`");
        assert_eq!(reporter.life.provider(), None, "and names no window");
        assert_eq!(reporter.life.expired(0, LIMITS), None);
    }
}
