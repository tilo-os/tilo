//! The daemon in a workspace's VM: one device of the person that reaches the workspace alone, and
//! runs the VM's Browser and Linux apps sessions as its shells (`docs/hosted-browser.md`). The host
//! starts it through the guest agent with the ticket the placement carries ([`DAEMON`],
//! [`TICKET`]), learns the device it logged in as, and is done: the sessions are launched and read
//! on that device through the workspace's dispatch
//! ([`tilo_action_services::environment_window`]). The daemon ends once its last session ended,
//! and the VM sleeps.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use linux_vm_protocol::client::{self, LaunchIo, LaunchSignals};
use linux_vm_protocol::link::{AgentLink, AgentStream};
use linux_vm_protocol::{AGENT_PORT, Display, Launch, ProtocolError, Signal, Stdio};
use thiserror::Error;
use tilo_server_protocol::hosted::Material;

use super::network::Endpoint;

/// The VM daemon's entry, as the guest image ships it: started with [`TICKET`] it runs the daemon;
/// `--device` prints the device it is, and `--browser-store PATH` names where Browser keeps its
/// profile.
pub const DAEMON: &str = "/usr/libexec/tilo/vm-daemon";

/// Names, in the daemon's environment, the ticket it logs in with.
pub const TICKET: &str = "TILO_VM_TICKET";

/// The log the daemon and its sessions write, in the VM's state directory.
pub const LOG: &str = "vm-daemon.log";

/// How long the guest agent is given to start the daemon.
pub const READY_PATIENCE: Duration = Duration::from_secs(120);

/// How long the daemon is given to log in with its ticket.
pub const UP_PATIENCE: Duration = Duration::from_secs(60);

/// How often the device the daemon logged in as is looked for while it logs in.
const LOGIN_LOOK: Duration = Duration::from_millis(100);

/// The placement as the VM's daemon is handed it: the host's material, with the address its ticket
/// names the issuing host at replaced by the one this VM reaches it at. The host's endpoint id
/// stays, so the daemon logs in at the same host at the one address this VM dials.
///
/// # Errors
///
/// The material is not the host's.
pub fn hand_off(material: &str, server: Endpoint) -> Result<Material, String> {
    let mut material: Material = serde_json::from_str(material.trim())
        .map_err(|error| format!("the material is not the server's: {error}"))?;
    material.ticket.issuer.relay = None;
    material.ticket.issuer.direct = vec![server.socket()];
    Ok(material)
}

/// The material a host writes for a workspace's VM, its ticket naming the host at `server`: what
/// the tests of a hand-off start from.
#[cfg(test)]
pub fn test_material(server: &str) -> String {
    let material = Material {
        workspace: "ws1".into(),
        workspace_id: tilo_server_protocol::WorkspaceId::from_u128(7),
        record: "browser-1".into(),
        ticket: tilo_device_identity::login::Ticket {
            issuer: tilo_device_identity::login::Reached {
                endpoint: test_server_id(),
                relay: Some("https://relay.example".into()),
                direct: vec![server.parse().unwrap()],
            },
            direction: tilo_device_identity::login::Direction::Login,
            secret: [3; 32],
        },
        user_public_key: [1; 32],
        account: "acct-1".into(),
    };
    serde_json::to_string(&material).unwrap_or_default()
}

#[cfg(test)]
fn test_server_id() -> [u8; 32] {
    *iroh_base::SecretKey::from_bytes(&[7; 32])
        .public()
        .as_bytes()
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Agent(#[from] ProtocolError),
    #[error("the daemon did not start within {0:?}")]
    Silent(Duration),
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The daemon's process in the VM, driven through the guest agent.
pub struct Session {
    signals: Option<LaunchSignals<AgentStream>>,
    ended: Arc<AtomicBool>,
}

impl Session {
    /// Start the daemon in the guest `link` reaches, logging in with `ticket`, logging to `log`.
    pub fn start(link: &AgentLink, ticket: &str, log: PathBuf) -> Result<Self, SessionError> {
        let stream = link.connect(AGENT_PORT)?;
        let launch = Launch {
            argv: vec![DAEMON.to_owned()],
            cwd: None,
            env: vec![(TICKET.to_owned(), ticket.to_owned())],
            stdio: Stdio::Pump,
            display: Display::None,
        };
        let (telling, told) = mpsc::channel::<LaunchSignals<AgentStream>>();
        let ended = Arc::new(AtomicBool::new(false));
        let ending = Arc::clone(&ended);
        std::thread::spawn(move || {
            let (mut out, mut err) = (Log::new(&log), Log::new(&log));
            let result = client::launch(
                stream,
                &launch,
                LaunchIo {
                    stdin: None,
                    stdout: &mut out,
                    stderr: &mut err,
                    started: &mut |_| {},
                },
                move |signals| {
                    let _ = telling.send(signals);
                },
            );
            ending.store(true, Ordering::SeqCst);
            let ended = match result {
                Ok(status) => format!("the VM's daemon exited ({})\n", status.shell_code()),
                Err(error) => format!("the VM's daemon ended: {error}\n"),
            };
            let _ = err.write_all(ended.as_bytes());
        });
        let signals = told
            .recv_timeout(READY_PATIENCE)
            .map_err(|_| SessionError::Silent(READY_PATIENCE))?;
        Ok(Self {
            signals: Some(signals),
            ended,
        })
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

/// Run the daemon's entry with `args` in the guest `link` reaches, as a command that ends: what it
/// printed, once it exited with success.
fn run_entry(link: &AgentLink, args: &[&str], log: &Path) -> Result<Vec<u8>, String> {
    let stream = link
        .connect(AGENT_PORT)
        .map_err(|error| error.to_string())?;
    let launch = Launch {
        argv: std::iter::once(DAEMON)
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect(),
        cwd: None,
        env: Vec::new(),
        stdio: Stdio::Pump,
        display: Display::None,
    };
    let (mut out, mut err) = (Vec::new(), Log::new(log));
    let status = client::launch(
        stream,
        &launch,
        LaunchIo {
            stdin: None,
            stdout: &mut out,
            stderr: &mut err,
            started: &mut |_| {},
        },
        |_| {},
    )
    .map_err(|error| error.to_string())?;
    match status.shell_code() {
        0 => Ok(out),
        code => Err(format!("{DAEMON} {} exited ({code})", args.join(" "))),
    }
}

/// The device a `--device` answer names, once a person's certificate names it: the daemon has
/// logged in.
fn logged_in(answer: &[u8]) -> Option<[u8; 32]> {
    #[derive(serde::Deserialize)]
    struct Device {
        device: String,
        #[serde(default)]
        people: Vec<String>,
    }
    let device: Device = serde_json::from_slice(answer).ok()?;
    if device.people.is_empty() {
        return None;
    }
    hex::decode(device.device).ok()?.try_into().ok()
}

struct Log {
    file: Option<std::fs::File>,
}

impl Log {
    fn new(path: &Path) -> Self {
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

/// What starts and asks the daemon in a workspace's VM.
pub trait Starts: Send + Sync {
    /// Start the daemon, logging in with `ticket`.
    ///
    /// # Errors
    ///
    /// Why it did not start: the guest agent refused it, or did not answer.
    fn start(&self, ticket: &str) -> Result<Session, SessionError>;

    /// The device the daemon logged in as, once it has.
    ///
    /// # Errors
    ///
    /// The guest could not be asked.
    fn device(&self) -> Result<Option<[u8; 32]>, String>;

    /// Name `store` as where Browser keeps its profile between runs.
    ///
    /// # Errors
    ///
    /// The guest could not be told.
    fn keep_store(&self, store: &Path) -> Result<(), String>;
}

pub struct Launcher {
    link: AgentLink,
    log: PathBuf,
}

impl Launcher {
    #[must_use]
    pub const fn new(link: AgentLink, log: PathBuf) -> Self {
        Self { link, log }
    }
}

impl Starts for Launcher {
    fn start(&self, ticket: &str) -> Result<Session, SessionError> {
        Session::start(&self.link, ticket, self.log.clone())
    }

    fn device(&self) -> Result<Option<[u8; 32]>, String> {
        // Before the daemon made its key there is no device to read yet.
        Ok(run_entry(&self.link, &["--device"], &self.log)
            .ok()
            .and_then(|answer| logged_in(&answer)))
    }

    fn keep_store(&self, store: &Path) -> Result<(), String> {
        let store = store.to_string_lossy();
        run_entry(&self.link, &["--browser-store", &store], &self.log).map(drop)
    }
}

/// A workspace VM's daemon as the host holds it: what starts it, the process once it was started,
/// and the device it logged in as.
pub struct Daemon {
    launcher: Option<Box<dyn Starts>>,
    session: Mutex<Option<Session>>,
    device: Mutex<Option<[u8; 32]>>,
    /// Held while one open starts it, so a second waits for that one.
    opening: Mutex<()>,
}

impl fmt::Debug for Daemon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Daemon")
            .field("alive", &self.alive())
            .field("device", &lock(&self.device).map(hex::encode))
            .finish_non_exhaustive()
    }
}

impl Daemon {
    /// A daemon that cannot start: its VM's agent was never reached.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            launcher: None,
            session: Mutex::new(None),
            device: Mutex::new(None),
            opening: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn booted(starts: Box<dyn Starts>) -> Self {
        Self {
            launcher: Some(starts),
            ..Self::new()
        }
    }

    /// Whether its process runs: what keeps its VM awake between the commands run in it.
    #[must_use]
    pub fn alive(&self) -> bool {
        lock(&self.session)
            .as_ref()
            .is_some_and(|session| !session.ended())
    }

    /// The daemon logged in with `material`'s ticket, started here when it does not run, with
    /// `store` named as Browser's: the device it logged in as, and how long starting it took
    /// when this call started it.
    ///
    /// # Errors
    ///
    /// It could not be started, did not log in within `patience`, or the store was not named.
    pub fn open(
        &self,
        material: &Material,
        store: Option<&Path>,
        patience: Duration,
    ) -> Result<(Option<u64>, [u8; 32]), String> {
        let _opening = lock(&self.opening);
        let starts = self
            .launcher
            .as_ref()
            .ok_or_else(|| "the workspace's VM has no daemon".to_owned())?;
        let running = self.alive().then(|| *lock(&self.device)).flatten();
        let (started, device) = if let Some(device) = running {
            (None, device)
        } else {
            let began = Instant::now();
            let session = starts
                .start(&material.ticket.encode())
                .map_err(|error| error.to_string())?;
            *lock(&self.session) = Some(session);
            let device = loop {
                match starts.device() {
                    Ok(Some(device)) => break device,
                    Ok(None) if self.alive() && began.elapsed() < patience => {
                        std::thread::sleep(LOGIN_LOOK);
                    }
                    Ok(None) if self.alive() => {
                        self.kill();
                        return Err(format!(
                            "the VM's daemon did not log in within {patience:?}"
                        ));
                    }
                    Ok(None) => return Err("the VM's daemon ended before it logged in".into()),
                    Err(error) => {
                        self.kill();
                        return Err(error);
                    }
                }
            };
            *lock(&self.device) = Some(device);
            let took = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
            (Some(took), device)
        };
        if let Some(store) = store {
            starts.keep_store(store)?;
        }
        Ok((started, device))
    }

    fn kill(&self) {
        let session = lock(&self.session).take();
        if let Some(session) = session {
            session.kill();
        }
        *lock(&self.device) = None;
    }

    /// End the daemon in the guest: asked with `SIGTERM`, so its sessions end and Browser writes its
    /// profile to the VM's disk, and killed when it has not ended within `grace`.
    pub fn end(&self, grace: Duration) {
        let session = lock(&self.session).take();
        *lock(&self.device) = None;
        let Some(session) = session else {
            return;
        };
        session.signal(Signal::Terminate);
        let started = Instant::now();
        while !session.ended() && started.elapsed() < grace {
            std::thread::sleep(Duration::from_millis(20));
        }
        session.kill();
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn the_ticket_handed_to_a_daemon_names_the_host_at_its_dialable_address() {
        let handed = hand_off(
            &test_material("203.0.113.9:7704"),
            Endpoint {
                address: "10.0.2.4".parse().unwrap(),
                port: 7704,
            },
        )
        .unwrap();
        assert_eq!(
            handed.ticket.issuer.endpoint,
            test_server_id(),
            "the same host"
        );
        assert_eq!(
            handed.ticket.issuer.direct,
            ["10.0.2.4:7704".parse().unwrap()],
            "at the one address this VM dials"
        );
        assert_eq!(handed.ticket.issuer.relay, None);
        assert_eq!(handed.ticket.secret, [3; 32], "the ticket's own secret");
        assert_eq!(handed.workspace, "ws1");
        assert_eq!(handed.record, "browser-1");
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
    fn a_daemon_has_logged_in_once_a_persons_certificate_names_its_device() {
        let device = hex::encode([9_u8; 32]);
        assert_eq!(
            logged_in(format!(r#"{{"device":"{device}","people":["ab"]}}"#).as_bytes()),
            Some([9; 32])
        );
        for not_yet in [
            format!(r#"{{"device":"{device}","people":[]}}"#),
            format!(r#"{{"device":"{device}"}}"#),
            r#"{"device":"nothex","people":["ab"]}"#.to_owned(),
            String::new(),
        ] {
            assert_eq!(logged_in(not_yet.as_bytes()), None, "{not_yet}");
        }
    }

    /// A guest whose daemon logs in after `logs_in_after` looks.
    struct FakeGuest {
        tickets: Arc<Mutex<Vec<String>>>,
        looks: AtomicUsize,
        logs_in_after: usize,
        stores: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl Starts for FakeGuest {
        fn start(&self, ticket: &str) -> Result<Session, SessionError> {
            lock(&self.tickets).push(ticket.to_owned());
            Ok(Session {
                signals: None,
                ended: Arc::new(AtomicBool::new(false)),
            })
        }

        fn device(&self) -> Result<Option<[u8; 32]>, String> {
            let looked = self.looks.fetch_add(1, Ordering::SeqCst);
            Ok((looked >= self.logs_in_after).then_some([5; 32]))
        }

        fn keep_store(&self, store: &Path) -> Result<(), String> {
            lock(&self.stores).push(store.to_owned());
            Ok(())
        }
    }

    /// What the fake guest was handed: the tickets it started with, and the stores it was named.
    type Handed<T> = Arc<Mutex<Vec<T>>>;

    fn guest(logs_in_after: usize) -> (Daemon, Handed<String>, Handed<PathBuf>) {
        let tickets: Arc<Mutex<Vec<String>>> = Arc::default();
        let stores: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let daemon = Daemon::booted(Box::new(FakeGuest {
            tickets: Arc::clone(&tickets),
            looks: AtomicUsize::new(0),
            logs_in_after,
            stores: Arc::clone(&stores),
        }));
        (daemon, tickets, stores)
    }

    /// The first open starts the daemon with the ticket and waits for it to log in; the next finds
    /// it running and names Browser's store, and the ticket is handed once.
    #[test]
    fn one_open_starts_the_daemon_with_its_ticket_and_the_next_finds_it() {
        let (daemon, tickets, stores) = guest(2);
        let material: Material = serde_json::from_str(&test_material("10.0.2.2:7704")).unwrap();
        assert!(!daemon.alive());
        let (started, device) = daemon.open(&material, None, UP_PATIENCE).unwrap();
        assert!(started.is_some(), "this open started it");
        assert_eq!(device, [5; 32], "the device it logged in as");
        assert!(daemon.alive(), "a daemon that runs keeps its VM awake");
        assert_eq!(*lock(&tickets), [material.ticket.encode()]);
        let store = Path::new("/workspace/.browser");
        assert_eq!(
            daemon.open(&material, Some(store), UP_PATIENCE).unwrap(),
            (None, [5; 32]),
            "the second finds it running"
        );
        assert_eq!(lock(&tickets).len(), 1, "the ticket is spent once");
        assert_eq!(*lock(&stores), [store.to_owned()]);
        daemon.end(Duration::from_millis(1));
        assert!(!daemon.alive(), "an ended daemon is no reason to keep a VM");
    }

    /// A daemon that does not log in in time is ended, so the next open starts a fresh one.
    #[test]
    fn a_daemon_that_never_logs_in_is_ended() {
        let (daemon, tickets, _) = guest(usize::MAX);
        let material: Material = serde_json::from_str(&test_material("10.0.2.2:7704")).unwrap();
        let error = daemon
            .open(&material, None, Duration::from_millis(20))
            .expect_err("it never logged in");
        assert!(error.contains("log in"), "{error}");
        assert!(!daemon.alive());
        assert!(
            daemon
                .open(&material, None, Duration::from_millis(1))
                .is_err()
        );
        assert_eq!(lock(&tickets).len(), 2, "each open started a fresh one");
    }

    #[test]
    fn a_vm_whose_agent_was_never_reached_starts_no_daemon() {
        let material: Material = serde_json::from_str(&test_material("10.0.2.2:7704")).unwrap();
        assert_eq!(
            Daemon::new()
                .open(&material, None, Duration::from_millis(1))
                .err()
                .as_deref(),
            Some("the workspace's VM has no daemon")
        );
    }
}
