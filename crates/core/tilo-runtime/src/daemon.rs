//! Persistent native session shared by independently launched GUI apps and MCP.
use crate::device::presence::{Presence, Visit};
use crate::device::{local::Frames, services::Daemon};
use crate::seats::{Seated, Seats};
use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_api::daemon::admission::{Admitted, ROOT};
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_mcp::lines::{Answer, Frame, mcp_answer, read_frame, serve_lines, spawn_reader};
use tilo_mcp::session_api::{Asking, bridge_stdio};

use crate::daemon_args::invalid;
pub use crate::daemon_args::{Mode, default_profile, parse_args, validate_profile};

const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// The directory the daemon serving `profile` keeps its sockets in: this computer's own listens
/// where every client finds it ([`tilo_api::daemon::this_computer`]), never inside a profile whose
/// path the home folder lengthens; a profile named apart (a test's, a VM host's) keeps its sockets
/// inside it. Either way a socket whose path its address cannot hold is refused, by name.
pub(crate) fn sockets(profile: &Path) -> io::Result<PathBuf> {
    let dir = if crate::daemon_args::own_profile().is_ok_and(|own| own == profile) {
        tilo_api::daemon::this_computer()
            .map_err(io::Error::other)?
            .sockets()
            .to_owned()
    } else {
        profile.to_owned()
    };
    for name in [
        tilo_paths::machine::DAEMON_NATIVE,
        COLLAB_SOCKET,
        IDENTITY_SOCKET,
        BLOB_SOCKET,
    ] {
        tilo_api::daemon::socket_fits(&dir.join(name)).map_err(io::Error::other)?;
    }
    Ok(dir)
}

/// The native endpoint of the daemon serving `profile`, in its [`sockets`].
fn native_socket(profile: &Path) -> io::Result<PathBuf> {
    Ok(sockets(profile)?.join(tilo_paths::machine::DAEMON_NATIVE))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    version: u32,
    pid: u32,
    /// The workspace the daemon serves. The probe answers it as `workspace`, the
    /// name launchers and scripts outside this crate read; `workspace` is
    /// read as the same.
    #[serde(rename = "workspace", alias = "workspace")]
    workspace: String,
}

fn probe(socket: &Path) -> io::Result<Status> {
    let mut stream = tilo_files::native::socket::connect(socket)?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    stream.write_all(b"{\"native\":\"status\"}\n")?;
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take(16_385)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > 16_384 || bytes.last() != Some(&b'\n') {
        return Err(invalid("invalid native runtime status frame"));
    }
    let status: Status = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if status.version != 1
        || status.pid == 0
        || status.workspace.is_empty()
        || status.workspace.len() > 4096
    {
        return Err(invalid("invalid native runtime status"));
    }
    Ok(status)
}

/// How long a client waits for the daemon the service started to serve its person.
const ANSWERING: Duration = Duration::from_secs(120);

/// The daemon serving `profile`, once it serves its person: asked at once, and, when it does
/// not answer, after the one start ([`tilo_api::daemon::service::start`]) for a profile that is
/// this computer's. A profile named apart (a test's, a VM host's) is served by whoever started
/// its daemon, and is only asked.
fn running(profile: &Path) -> io::Result<Status> {
    validate_profile(profile)?;
    let socket = native_socket(profile)?;
    if let Ok(status) = probe(&socket) {
        return Ok(status);
    }
    if std::env::var_os("TILO_RUNTIME_PROFILE").is_some() || default_profile()? != profile {
        return Err(io::Error::other(format!(
            "no Tilo daemon serves the profile {}",
            profile.display()
        )));
    }
    tilo_api::daemon::service::start().map_err(io::Error::other)?;
    let deadline = Instant::now()
        .checked_add(ANSWERING)
        .unwrap_or_else(Instant::now);
    loop {
        match probe(&socket) {
            // The computer's first command founds its person, as `tilo`'s does.
            Ok(status) => {
                if !tilo_api::daemon::system_daemon() {
                    tilo_api::daemon::this_computer()
                        .and_then(|place| place.found(None))
                        .map_err(io::Error::other)?;
                }
                return Ok(status);
            }
            Err(error) if Instant::now() >= deadline => {
                return Err(io::Error::other(format!(
                    "Tilo isn't answering at {} ({error}): {}",
                    socket.display(),
                    tilo_api::daemon::service::LOOK
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Start this computer's daemon over `profile` and serve it until it is told to stop
/// ([`Running`]). The executable prepares its profile environment before entering this function;
/// reusable serving code never mutates it.
///
/// # Errors
/// The daemon could not start, or it could not serve.
pub fn serve(profile: &Path, dev: bool) -> io::Result<()> {
    Running::start_serving(profile, dev)?.wait()
}

/// This computer's daemon running in this process: the native endpoint the apps and `--connect`
/// reach, and the device as [`crate::device::hold`] serves it. The `tilo-runtime` executable runs
/// one for its life; an app that is its own computer's daemon (Android's) runs one beside its
/// windows, and may stop it and start it again.
pub struct Running {
    native: PathBuf,
    stop: crate::device::serve::Stop,
    /// A duplicate of the native endpoint's listener: shutting it down ends its thread's accept.
    listener: UnixListener,
    accepting: Option<std::thread::JoinHandle<()>>,
    serving: Option<std::thread::JoinHandle<io::Result<()>>>,
    /// The person's services while they run: what starts their shell.
    person: Arc<Person>,
}

/// The person's services, while they run, for whoever waits for them to.
#[derive(Default)]
struct Person {
    services: Mutex<std::sync::Weak<Daemon>>,
    started: Condvar,
}

impl Person {
    /// `services` run from now on, or (`None`) no longer.
    fn set(&self, services: Option<&Arc<Daemon>>) {
        *self.services.lock_recover() = services.map_or_else(std::sync::Weak::new, Arc::downgrade);
        self.started.notify_all();
    }

    /// The person's services once they run, waiting up to `within` for them to start.
    fn within(&self, within: Duration) -> io::Result<Arc<Daemon>> {
        let deadline = Instant::now() + within;
        let mut services = self.services.lock_recover();
        loop {
            if let Some(services) = services.upgrade() {
                return Ok(services);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the person's services did not start",
                ));
            }
            services = self
                .started
                .wait_timeout(services, left)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(services, _)| services);
        }
    }
}

/// Where in the daemon's profile it keeps which account is the device's person.
const PERSON_ACCOUNTS: &str = "accounts/person.json";

impl Running {
    /// Start the daemon over `profile`, returning once its native endpoint is bound: it answers
    /// the device's own actions at once, and its person's once their services run, which they
    /// start on a thread of the daemon's.
    ///
    /// # Errors
    /// The profile is not a private absolute directory, another daemon has this device, or the
    /// native endpoint cannot be bound.
    pub fn start(profile: &Path) -> io::Result<Self> {
        Self::start_serving(profile, false)
    }

    /// [`Self::start`], serving builds under development too when `dev`.
    fn start_serving(profile: &Path, dev: bool) -> io::Result<Self> {
        validate_profile(profile)?;
        let device = crate::device::unclaimed::ThisDevice::lock()?;
        // Who each connection is: root and this daemon's own account act as the device, and
        // under the system unit the account that made the device's person, kept in the profile,
        // and the people signed in act as themselves.
        tilo_api::daemon::admission::install(
            tilo_api::daemon::admission::Admission::new(
                rustix::process::geteuid().as_raw(),
                tilo_api::daemon::system_daemon(),
            )
            .keeping_accounts_in(profile.join(PERSON_ACCOUNTS))?,
        );
        // The native endpoint answers from boot, before anyone's certificate names this device
        // and while its person's services start: the device's own actions first, the person's
        // once they run. This process has the device's lock, so only it removes a stale endpoint.
        let path = native_socket(profile)?;
        let listener = tilo_files::native::private::bind_socket(&path)?;
        if tilo_api::daemon::system_daemon() {
            use std::os::unix::fs::PermissionsExt as _;
            // Every account reaches it; the admission decides what each is answered.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))?;
        }
        let native = Arc::new(Native::new(
            device.answers(),
            crate::seats::peer_uid,
            tilo_api::daemon::admission::here(|| rustix::process::geteuid().as_raw()),
        ));
        let accepting = {
            let native = Arc::clone(&native);
            let listener = listener.try_clone()?;
            std::thread::Builder::new()
                .name("tilo-native".into())
                .spawn(move || {
                    if let Err(error) = native.accept(&listener) {
                        tracing::debug!(%error, "the native endpoint stopped taking connections");
                    }
                })?
        };
        tracing::info!("answering this device's actions on its native endpoint");
        let stop = crate::device::serve::Stop::default();
        let person = Arc::new(Person::default());
        let serving = {
            let (profile, stop, person) = (profile.to_owned(), stop.clone(), Arc::clone(&person));
            std::thread::Builder::new()
                .name("tilo-daemon".into())
                .spawn(move || serve_device(&profile, dev, device, &native, &stop, &person))?
        };
        Ok(Self {
            native: path,
            stop,
            listener,
            accepting: Some(accepting),
            serving: Some(serving),
            person,
        })
    }

    /// Start `command` as the person's shell, handed the link the daemon made for it, once the
    /// person's services run, waiting up to `within` for them to start
    /// ([`Daemon::start_shell`]).
    ///
    /// # Errors
    /// The person's services did not start in time, or the shell did not.
    #[cfg(target_os = "linux")]
    pub fn start_shell(
        &self,
        command: Command,
        within: Duration,
    ) -> io::Result<std::process::Child> {
        self.person.within(within)?.start_shell(command)
    }

    /// The shell's end of a link the daemon made, once the person's services run (within
    /// `within`): made and admitted exactly as [`Self::start_shell`] hands one to the shell it
    /// starts, for a test's session that runs in this process to take instead
    /// (`tilo_session::Options::shell_link`).
    ///
    /// # Errors
    /// The person's services did not start in time, or the link could not be made.
    #[cfg(feature = "test-support")]
    pub fn link_shell(&self, within: Duration) -> io::Result<std::os::fd::OwnedFd> {
        Ok(self.person.within(within)?.shell_link()?.into())
    }

    /// Where the daemon's native endpoint listens.
    #[must_use]
    pub fn native_socket(&self) -> &Path {
        &self.native
    }

    /// Stop serving, and return once the daemon let go of the device: its endpoints are closed
    /// and the device's lock is free, so a daemon may start on it again.
    ///
    /// # Errors
    /// The daemon could not serve.
    pub fn stop(self) -> io::Result<()> {
        self.stop.tell();
        self.wait()
    }

    /// Serve until the daemon is told to stop, by [`Self::stop`] or a signal, or another takes
    /// the device over.
    fn wait(mut self) -> io::Result<()> {
        let served = self.serving.take().map_or(Ok(()), |serving| {
            serving
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the daemon's thread panicked")))
        });
        self.close();
        served
    }

    /// Close the native endpoint and wait for its thread.
    fn close(&mut self) {
        let _ = rustix::net::shutdown(&self.listener, rustix::net::Shutdown::Both);
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
        let _ = std::fs::remove_file(&self.native);
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(serving) = self.serving.take() {
            self.stop.tell();
            let _ = serving.join();
        }
        self.close();
    }
}

/// This computer's daemon with the person's shell started over its link, as a sign-in starts
/// them: the one way a test starts a session that is the daemon's shell. The daemon runs in this
/// process ([`Running`]); the shell is the program `shell` starts, handed the link
/// ([`Daemon::start_shell`]). Dropping it stops both.
#[cfg(target_os = "linux")]
pub struct ShellSession {
    shell: Option<std::process::Child>,
    running: Option<Running>,
}

#[cfg(target_os = "linux")]
impl ShellSession {
    /// Start the daemon over `profile` and, once the person's services run (within `within`),
    /// `shell` as their shell. The profile's person is founded already, as a device a person
    /// signed in on is.
    ///
    /// # Errors
    /// The daemon did not start, its person's services did not within `within`, or the shell did
    /// not start.
    pub fn start(profile: &Path, shell: Command, within: Duration) -> io::Result<Self> {
        let running = Running::start(profile)?;
        let shell = running.start_shell(shell, within)?;
        Ok(Self {
            shell: Some(shell),
            running: Some(running),
        })
    }

    /// Where the daemon's native endpoint listens: what the person's programs, the CLI and the
    /// SDKs reach the session's actions at.
    #[must_use]
    pub fn native_socket(&self) -> &Path {
        self.running
            .as_ref()
            .map_or_else(|| Path::new(""), Running::native_socket)
    }

    /// The shell's process.
    pub fn shell(&mut self) -> &mut std::process::Child {
        self.shell
            .as_mut()
            .expect("the shell runs until the session stops")
    }

    /// Stop the shell, then the daemon, and return once both ended.
    ///
    /// # Errors
    /// The daemon could not serve.
    pub fn stop(mut self) -> io::Result<()> {
        self.end_shell();
        self.running.take().map_or(Ok(()), Running::stop)
    }

    /// Ask the shell to end, as a sign-out does, and wait for it.
    fn end_shell(&mut self) {
        if let Some(mut shell) = self.shell.take() {
            if let Ok(pid) = i32::try_from(shell.id())
                && let Some(pid) = rustix::process::Pid::from_raw(pid)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
            let _ = shell.wait();
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for ShellSession {
    fn drop(&mut self) {
        self.end_shell();
    }
}

/// Serve `device` over `profile` with `native` as its endpoint until `stop` is told: its person
/// found or awaited, their services started, and the device held.
fn serve_device(
    profile: &Path,
    dev: bool,
    device: crate::device::unclaimed::ThisDevice,
    native: &Native,
    stop: &crate::device::serve::Stop,
    person: &Person,
) -> io::Result<()> {
    // Before a person's certificate names this computer's key, it is served with the key alone:
    // a ticket it was handed is spent on the way, and the person's services start once a
    // certificate names it, from a login or a person its own account starts (`devices#found`).
    if !device.claimed() {
        device.spend_handed_ticket();
        if !device.until_claimed(stop) {
            return Ok(());
        }
    }
    // A device a ticket made enters through the server the ticket named until its copy of the
    // person's own workspace names it.
    let mut entered = device.entered();
    let locked = device.into_lock();
    loop {
        let mut stage = |stage: &str| tracing::debug!(stage, "starting the person's services");
        let session = Arc::new(Daemon::start_locked(
            profile,
            &mut stage,
            dev,
            (locked.0, entered.take()),
        )?);
        // A computer that just logged in takes its workspace in while the device is held (below):
        // nothing is admitted to the workspace before then. The endpoint admits each connection as
        // it comes, and host search starts once the workspace is here, rather than the daemon
        // ending before it could take the workspace in.
        // The person's and their agents' task programs run here, in this daemon's processes.
        #[cfg(target_os = "linux")]
        session.serve_task_programs();
        let search = Arc::new(OnceLock::new());
        search_once_held(&session, &search)?;
        // A person who signs in from now on has their workspace here (the one their desktop
        // hands over, or one founded for them now) and a link their shell is admitted by.
        native
            .seats
            .serve(Arc::new(PersonShells(Arc::downgrade(&session))));
        native.serve_the_person(the_persons(&session, search));
        native.serve_keys(key_endpoint(&session, native.answers.clone()));
        person.set(Some(&session));
        // The device is served until the daemon is told to stop, another takes the device over,
        // or this computer logs in with a ticket: its person's part starts again as that device,
        // the device's own part (the native endpoint, the device's lock) staying up meanwhile.
        let held = crate::device::hold(&session, stop);
        person.set(None);
        native.forget_the_person();
        drop(session);
        match held.map_err(|error| io::Error::other(format!("{error:#}")))? {
            crate::device::Ended::Stopped => return Ok(()),
            crate::device::Ended::LoggedIn(device) => entered = Some(*device),
        }
    }
}

/// What the native endpoint says `session` serves: asking founds nothing, so a desktop about to
/// hand its workspaces over may ask.
fn status_of(session: &Arc<Daemon>) -> Arc<dyn Fn() -> Status + Send + Sync> {
    let session = Arc::downgrade(session);
    Arc::new(move || Status {
        version: 1,
        pid: std::process::id(),
        workspace: session
            .upgrade()
            .and_then(|session| session.first_workspace().map(str::to_owned))
            .unwrap_or_default(),
    })
}

/// What the daemon does for its person's shells: the link a session root starts at their
/// sign-in is admitted by, and the shell it starts itself when it runs as their own account.
struct PersonShells(std::sync::Weak<Daemon>);

impl PersonShells {
    fn daemon(&self) -> io::Result<Arc<Daemon>> {
        self.0
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))
    }
}

impl crate::seats::Shells for PersonShells {
    fn arrived(&self, importing: bool) -> io::Result<UnixStream> {
        let daemon = self.daemon()?;
        if !importing && let Err(error) = daemon.workspace() {
            tracing::warn!(%error, "the person who signed in has no workspace here yet");
        }
        daemon.shell_link()
    }

    #[cfg(target_os = "linux")]
    fn start(
        &self,
        args: Vec<std::ffi::OsString>,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> io::Result<std::process::Child> {
        /// The shell, installed beside this daemon.
        const SHELL: &str = "tilo-session";
        let daemon = self.daemon()?;
        let program = std::env::current_exe()?
            .parent()
            .map(|dir| dir.join(SHELL))
            .filter(|program| program.is_file())
            .ok_or_else(|| {
                io::Error::other(format!("{SHELL} is not installed beside tilo-runtime"))
            })?;
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(std::env::vars_os().filter(|(name, _)| shell_keeps_from_the_daemon(name)))
            .envs(env.into_iter().filter(|(name, _)| shell_reads(name)))
            .stdin(std::process::Stdio::null());
        daemon.start_shell(command)
    }

    #[cfg(not(target_os = "linux"))]
    fn start(
        &self,
        _: Vec<std::ffi::OsString>,
        _: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> io::Result<std::process::Child> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the shell starts on its own on this computer",
        ))
    }
}

/// Whether the shell the daemon starts is handed the variable `name` the account asked it with:
/// what places it in the person's session (their display, bus, directories, language) and Tilo's
/// own settings, never one that loads code into it (`LD_PRELOAD`), which would hand its link to
/// whoever asked.
#[cfg(target_os = "linux")]
fn shell_reads(name: &std::ffi::OsStr) -> bool {
    const NAMED: [&str; 14] = [
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "PATH",
        "LANG",
        "LANGUAGE",
        "TZ",
        "TERM",
        "DISPLAY",
        "XAUTHORITY",
        "GLIBC_TUNABLES",
        "GNOME_KEYRING_CONTROL",
        "SSH_AUTH_SOCK",
    ];
    const PREFIXED: [&str; 5] = ["XDG_", "WAYLAND_", "DBUS_", "LC_", "TILO_"];
    name.to_str().is_some_and(|name| {
        NAMED.contains(&name)
            || (PREFIXED.iter().any(|prefix| name.starts_with(prefix))
                && name != runtime_collab::wire::shell::SHELL_FD_ENV)
    })
}

/// Whether the shell the daemon starts keeps the variable `name` of the daemon's own environment,
/// which whoever started the daemon set and no account that asks for a shell can: the graphics
/// driver and the libraries the daemon itself was started to load (a machine with no GPU draws with
/// Mesa's software drivers, and its shell must too).
#[cfg(target_os = "linux")]
fn shell_keeps_from_the_daemon(name: &std::ffi::OsStr) -> bool {
    const NAMED: [&str; 6] = [
        "VK_DRIVER_FILES",
        "VK_ICD_FILENAMES",
        "LIBGL_ALWAYS_SOFTWARE",
        "GALLIUM_DRIVER",
        "LD_LIBRARY_PATH",
        "RUST_LOG",
    ];
    const PREFIXED: [&str; 2] = ["WGPU_", "MESA_"];
    name.to_str().is_some_and(|name| {
        NAMED.contains(&name) || PREFIXED.iter().any(|prefix| name.starts_with(prefix))
    })
}

/// What the native endpoint serves the person with once `session`, their services, runs: the
/// MCP projection and the action wire over their workspace, and host search's results opened
/// through `search` once it started.
fn the_persons(
    session: &Arc<Daemon>,
    search: Arc<OnceLock<Arc<tilo_os_search::OsSearch>>>,
) -> Endpoint<<Daemon as RuntimeServices>::ActionProvider> {
    Endpoint {
        session_factory: admitted_per_connection(session),
        status: status_of(session),
        search,
        media: session.media(),
        other_devices: session.other_devices(),
        agents: None,
        frames: Frames::of_the_person(session),
        to_agents: Some(session.agents().clone()),
    }
}

/// Serve `session`'s native endpoint at `socket`, as the daemon does once the person's services
/// run, each connection's account read by `uid_of` and admitted by `admission`: what a test of
/// the endpoint reaches.
#[cfg(test)]
pub(crate) fn serve_for_test(
    session: &Arc<Daemon>,
    device: crate::device::unclaimed::DeviceOnly,
    uid_of: crate::seats::UidOf,
    admission: &'static tilo_api::daemon::admission::Admission,
    socket: &Path,
) -> io::Result<()> {
    let native = Arc::new(Native::new(device, uid_of, admission));
    native.serve_the_person(the_persons(session, Arc::new(OnceLock::new())));
    let listener = tilo_files::native::private::bind_socket(socket)?;
    std::thread::Builder::new()
        .name("tilo-native-test".into())
        .spawn(move || native.accept(&listener))?;
    Ok(())
}

/// A connection the native endpoint takes, with the slot it counts against and who it is.
type Take = Box<dyn Fn(UnixStream, ConnectionSlot, Seated) -> io::Result<()> + Send + Sync>;

/// A connection of an account signed in as another person than the device's, taken as the key
/// of theirs it acts as, with that key's certificate.
type KeyTake = Box<
    dyn Fn(
            UnixStream,
            ConnectionSlot,
            Seated,
            ed25519_dalek::SigningKey,
            tilo_user_identity::DeviceCert,
        ) -> io::Result<()>
        + Send
        + Sync,
>;

/// The native endpoint over the daemon's whole life: the device's own actions from boot, and the
/// person's endpoint once their services run. A connection on a device no certificate names yet
/// is answered with the device's own actions at once; one on a set-up device while its person's
/// services start waits on its own thread until they run, so an app is never served the device
/// alone for its life. The listener itself never waits.
struct Native {
    answers: crate::device::unclaimed::DeviceOnly,
    /// Who each connection is, and the device's answers about it.
    seats: Arc<Seats>,
    /// The person's endpoint, while their services run.
    person: Mutex<Option<Arc<Take>>>,
    /// The endpoint of the other people signed in on the computer, each as their own key, while
    /// the person's services run.
    keys: Mutex<Option<Arc<KeyTake>>>,
    /// Told when [`Self::person`] is set.
    started: (Mutex<()>, Condvar),
}

impl Native {
    /// The endpoint of `device`, reading each connection's account with `uid_of` and admitting
    /// it by `admission`.
    fn new(
        device: crate::device::unclaimed::DeviceOnly,
        uid_of: crate::seats::UidOf,
        admission: &'static tilo_api::daemon::admission::Admission,
    ) -> Self {
        let seats = Arc::new(Seats::new(admission, device.clone(), uid_of));
        Self {
            answers: device,
            seats,
            started: (Mutex::new(()), Condvar::new()),
            person: Mutex::new(None),
            keys: Mutex::new(None),
        }
    }

    /// Serve `stream` the device's own actions, as the account `seated` asks them.
    fn device_only(
        &self,
        stream: UnixStream,
        slot: ConnectionSlot,
        seated: Seated,
    ) -> io::Result<()> {
        let answers = self.answers.seated(seated.clone());
        let frames = Frames::of_the_device(answers.clone());
        Endpoint {
            session_factory: Arc::new(move || {
                Ok(McpServer::for_agent(
                    answers.clone(),
                    String::new(),
                    "agent",
                    "",
                ))
            }),
            // No workspace is served on it.
            status: Arc::new(|| Status {
                version: 1,
                pid: std::process::id(),
                workspace: String::new(),
            }),
            search: Arc::new(OnceLock::new()),
            media: None,
            other_devices: None,
            agents: None,
            frames,
            to_agents: None,
        }
        .take(stream, slot, Some(seated))
    }

    /// Serve every connection from now on with `endpoint`, the person's.
    fn serve_the_person<H: tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static>(
        &self,
        endpoint: Endpoint<H>,
    ) {
        let take: Take =
            Box::new(move |stream, slot, seated| endpoint.take(stream, slot, Some(seated)));
        *self.person.lock_recover() = Some(Arc::new(take));
        let _told = self.started.0.lock_recover();
        self.started.1.notify_all();
    }

    /// Serve the connections of the other people signed in on the computer with `take`, each
    /// as their own key, from now on.
    fn serve_keys(&self, take: KeyTake) {
        *self.keys.lock_recover() = Some(Arc::new(take));
    }

    /// Serve no connection the person's from now on, while their services start again: one that
    /// comes meanwhile waits for them, and another person's is answered the device's own actions.
    fn forget_the_person(&self) {
        self.person.lock_recover().take();
        self.keys.lock_recover().take();
    }

    /// The person's endpoint, while their services run.
    fn person(&self) -> Option<Arc<Take>> {
        self.person.lock_recover().clone()
    }

    /// Serve each connection to `listener` as whatever answers when it comes ([`accept`]): an
    /// account the daemon does not serve is answered the device's own actions alone.
    fn accept(self: &Arc<Self>, listener: &UnixListener) -> io::Result<()> {
        accept(listener, |stream, slot| {
            let seated = Seated::new(self.seats.seat(&stream), Arc::clone(&self.seats));
            self.take(stream, slot, seated)
        })
    }

    fn take(
        self: &Arc<Self>,
        stream: UnixStream,
        slot: ConnectionSlot,
        seated: Seated,
    ) -> io::Result<()> {
        // Root acts as the device, saying who signed in and who left: answered at once, never
        // held while the person's services start.
        if !seated.seat.admitted.served() || seated.seat.uid == ROOT {
            return self.device_only(stream, slot, seated);
        }
        // Another person than the device's acts as their own key, and is never served the
        // person's endpoint: until it runs, the device's own actions answer them.
        if matches!(seated.seat.admitted, Admitted::Key { .. }) {
            let take = self.keys.lock_recover().clone();
            return match (take, self.seats.key(seated.seat)) {
                (Some(take), Some((key, cert))) => take(stream, slot, seated, key, cert),
                _ => self.device_only(stream, slot, seated),
            };
        }
        match self.person() {
            Some(person) => person(stream, slot, seated),
            None if !self.answers.claimed() => self.device_only(stream, slot, seated),
            None => {
                let native = Arc::clone(self);
                std::thread::Builder::new()
                    .name("tilo-native-wait".into())
                    .spawn(move || {
                        let mut told = native.started.0.lock_recover();
                        let person = loop {
                            if let Some(person) = native.person() {
                                break person;
                            }
                            told = native
                                .started
                                .1
                                .wait(told)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        };
                        drop(told);
                        if let Err(error) = person(stream, slot, seated) {
                            tracing::debug!(%error, "a native connection that waited while the person's services started was not served");
                        }
                    })
                    .map(drop)
            }
        }
    }
}

/// Hand each connection to `listener` to `take`, at most [`MAX_CONNECTIONS`] at once. One more
/// is told there is no room and closed ([`refuse_busy`]).
fn accept(
    listener: &UnixListener,
    take: impl Fn(UnixStream, ConnectionSlot) -> io::Result<()>,
) -> io::Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            refuse_busy(stream);
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        take(stream, ConnectionSlot(Arc::clone(&active)))?;
    }
    Ok(())
}

/// What every native connection is served with.
pub(crate) struct Endpoint<H> {
    session_factory: tilo_mcp::SessionFactory<H>,
    /// What the endpoint says it serves, asked once a connection's session is made.
    status: Arc<dyn Fn() -> Status + Send + Sync>,
    /// Opens the host search results another process was handed, once host search started.
    search: Arc<OnceLock<Arc<tilo_os_search::OsSearch>>>,
    /// Where answers read the content actions produced.
    media: Option<Arc<dyn tilo_mcp::MediaReader>>,
    /// The person's other devices, which a `--connect --device` client is carried to; `None` for
    /// an endpoint that reaches none.
    other_devices: Option<crate::other_devices::OtherDevices>,
    /// For the connections of the agents this computer's login was made for: the device's
    /// presence in its workspace, which answers their account calls and says when the person
    /// stopped them.
    agents: Option<Arc<Presence>>,
    /// What a connection whose first line is an action frame is answered by: the wire `tilo`'s
    /// commands, `tilolang repl` and `tilo-app` call actions over.
    frames: Frames,
    /// Where an agent's `tilo mcp` connection is handed ([`tilo_api::daemon::AGENT_LINE`]): to
    /// the device's one session, which every agent of the computer shares. An endpoint without
    /// one serves an agent the device's own actions, as any other MCP client.
    to_agents: Option<crate::device::serve::Agents>,
}

/// The endpoint every agent of this computer's login reaches at its device socket: the
/// daemon's own workspace, as the native endpoint serves the person's apps, each connection a
/// run of the one agent the login was made for, as every computer of the person's names it.
pub(crate) fn agents_endpoint(
    daemon: &Arc<Daemon>,
    agents: Arc<Presence>,
) -> Endpoint<<Daemon as RuntimeServices>::ActionProvider> {
    let name = agents
        .home()
        .map_or_else(|| "agent".to_owned(), |device| device.agent);
    Endpoint {
        session_factory: daemon.agent_sessions(&name),
        status: status_of(daemon),
        search: Arc::new(OnceLock::new()),
        media: daemon.media(),
        other_devices: daemon.other_devices(),
        agents: Some(agents),
        frames: Frames::of_the_person(daemon),
        to_agents: None,
    }
}

/// How the native endpoint serves a connection of another person than the device's: each as a
/// run of the key of theirs it acts as, in `session`'s workspace. Its action frames reach the
/// device's own actions (`answers`) alone, never the person's tree.
fn key_endpoint(session: &Arc<Daemon>, answers: crate::device::unclaimed::DeviceOnly) -> KeyTake {
    let session = Arc::downgrade(session);
    Box::new(move |stream, slot, seated, key, cert| {
        let daemon = session
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        Endpoint {
            session_factory: daemon.key_sessions(key, cert),
            status: status_of(&daemon),
            search: Arc::new(OnceLock::new()),
            media: None,
            other_devices: None,
            agents: None,
            frames: Frames::of_the_device(answers.seated(seated.clone())),
            to_agents: None,
        }
        .take(stream, slot, Some(seated))
    })
}

/// The sessions of the native endpoint, each admitted to `session`'s workspace when its
/// connection comes: a connection before the workspace reached this computer is told why it is
/// not served, and one after it is served.
fn admitted_per_connection(
    session: &Arc<Daemon>,
) -> tilo_mcp::SessionFactory<<Daemon as RuntimeServices>::ActionProvider> {
    let session = Arc::downgrade(session);
    let made: OnceLock<tilo_mcp::SessionFactory<_>> = OnceLock::new();
    Arc::new(move || {
        if let Some(factory) = made.get() {
            return factory();
        }
        let daemon = session
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        let factory = daemon.mcp_session_factory(&daemon.workspace()?)?;
        made.get_or_init(|| factory)()
    })
}

/// Start host search over `session`'s workspace into `search` once the workspace can be admitted
/// to: at once on a computer that holds it, and when it arrives on one that just logged in.
fn search_once_held(
    session: &Arc<Daemon>,
    search: &Arc<OnceLock<Arc<tilo_os_search::OsSearch>>>,
) -> io::Result<()> {
    let entered = session.entered();
    let (session, search) = (Arc::downgrade(session), Arc::clone(search));
    let runtime = tilo_rt::try_app()
        .map_err(io::Error::other)?
        .handle()
        .clone();
    std::thread::Builder::new()
        .name("tilo-os-search".into())
        .spawn(move || {
            let mut said = false;
            loop {
                // Armed before the look, so a workspace held between the two is not missed.
                let mut held = std::pin::pin!(entered.notified());
                held.as_mut().enable();
                let Some(daemon) = session.upgrade() else {
                    return;
                };
                match daemon.action_provider() {
                    Ok(host) => {
                        let workspace = daemon.workspace().unwrap_or_default();
                        match crate::os_search::start(host, &workspace, daemon.actor()) {
                            Ok(started) => {
                                let _ = search.set(started);
                            }
                            Err(error) => {
                                tracing::warn!(%error, "host search does not search this computer's workspace");
                            }
                        }
                        return;
                    }
                    Err(error) if !said => {
                        said = true;
                        tracing::warn!(%error, "host search starts once this computer holds its workspace");
                    }
                    Err(_) => {}
                }
                drop(daemon);
                runtime.block_on(held);
            }
        })?;
    Ok(())
}

/// The longest first line a native connection is read for before it is served: a forward to
/// another device, or the first request of any other.
const FIRST_LINE: u64 = 64 * 1024;

/// Connections the native endpoint serves at once.
const MAX_CONNECTIONS: usize = 64;
/// What a connection past [`MAX_CONNECTIONS`] is told before it is closed.
const NO_ROOM: &str = "this runtime is serving 64 connections, as many as it serves at once, so \
     this one is closed and nothing it asked ran; connect again once one of them has closed";
const _: () = assert!(MAX_CONNECTIONS == 64, "NO_ROOM names the bound");

/// Tell a client the endpoint has no room for why it is not served, then
/// close: a JSON-RPC error that names no request, since none of its requests
/// is read. The line is written without waiting on the client, so one that
/// reads nothing cannot keep the endpoint from the next connection.
fn refuse_busy(mut stream: UnixStream) {
    let refusal = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":NO_ROOM}});
    let told = stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .and_then(|()| writeln!(stream, "{refusal}"));
    if let Err(error) = told {
        tracing::debug!(%error, "a native client past the bound was not told why it is closed");
    }
}

impl<H> Clone for Endpoint<H> {
    fn clone(&self) -> Self {
        Self {
            session_factory: Arc::clone(&self.session_factory),
            status: Arc::clone(&self.status),
            search: Arc::clone(&self.search),
            media: self.media.clone(),
            other_devices: self.other_devices.clone(),
            agents: self.agents.clone(),
            frames: self.frames.clone(),
            to_agents: self.to_agents.clone(),
        }
    }
}

impl<H: tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static> Endpoint<H> {
    /// Serve `stream` on its own thread, counted in `slot` while it lasts, as `seated` says
    /// who it is.
    fn take(
        &self,
        stream: UnixStream,
        slot: ConnectionSlot,
        seated: Option<Seated>,
    ) -> io::Result<()> {
        let endpoint = self.clone();
        std::thread::Builder::new()
            .name("tilo-native-mcp".into())
            .spawn(move || {
                let _slot = slot;
                endpoint.serve(stream, Vec::new(), seated.as_ref());
            })?;
        Ok(())
    }

    /// Serve `stream` on this thread until it ends, `read` being what was read of it already.
    pub(crate) fn serve(&self, stream: UnixStream, read: Vec<u8>, seated: Option<&Seated>) {
        // The first line says whether the client asks to be carried to another device's MCP
        // server; anything else is served here, that line first.
        let mut reader = match stream.try_clone() {
            Ok(clone) => BufReader::new(io::Cursor::new(read).chain(clone)),
            Err(error) => {
                tracing::debug!(%error, "native runtime client socket");
                return;
            }
        };
        let mut first = Vec::new();
        if let Err(error) = (&mut reader).take(FIRST_LINE).read_until(b'\n', &mut first) {
            tracing::debug!(%error, "native runtime client disconnected");
            return;
        }
        // A first line that is an action frame says the connection speaks the action wire,
        // answered as whoever its account is admitted as.
        let line = first.strip_suffix(b"\n").unwrap_or(&first);
        if crate::device::local::is_frame(line) {
            return self.frames.serve(stream, line, reader);
        }
        if line == tilo_api::daemon::AGENT_LINE.as_bytes() {
            if let Some(agents) = &self.to_agents {
                return agents.take(stream, reader.buffer().to_vec());
            }
            first.clear();
        }
        if let Some(forward) = first
            .strip_suffix(b"\n")
            .and_then(tilo_mcp::devices::forward_request)
        {
            match &self.other_devices {
                // A client asks for nothing more before it is told whether the connection
                // opened.
                Some(other_devices) if reader.buffer().is_empty() => {
                    other_devices.serve_forward(stream, &forward);
                }
                Some(_) => tilo_mcp::devices::refuse_forward(
                    stream,
                    "a forward is the only line before its answer".into(),
                ),
                None => tilo_mcp::devices::refuse_forward(
                    stream,
                    "this runtime reaches none of your other devices".into(),
                ),
            }
            return;
        }
        // A connection that asks the seats (who it is, a shell started or waited for) is
        // answered by them alone: asking founds the person no workspace, so a shell started for
        // a desktop about to hand its own over finds that one.
        let asks_the_seats =
            serde_json::from_slice::<Value>(&first).is_ok_and(|request| Seats::asks(&request));
        let input = io::Cursor::new(first).chain(reader);
        if let Some(seated) = seated.filter(|_| asks_the_seats) {
            if let Err(error) = serve_seat(&stream, input, seated) {
                tracing::debug!(%error, "native seat client disconnected");
            }
            return;
        }
        let server = match (self.session_factory)() {
            Ok(server) => Arc::new(server.with_media(self.media.clone()).with_devices(
                self.other_devices.clone().map(|other_devices| {
                    Arc::new(other_devices) as Arc<dyn tilo_mcp::devices::DeviceDirectory>
                }),
            )),
            Err(error) => {
                // Told why, as an agent whose person revoked its key is.
                tracing::debug!(%error, "native MCP session admission unavailable");
                crate::device::serve::refuse(&stream, &error.to_string());
                return;
            }
        };
        let visit = self.agents.as_ref().map(Presence::visit);
        let agent = self.agents.as_deref().zip(visit.as_ref());
        if let Err(error) = serve_connection(
            &stream,
            input,
            &server,
            &(self.status)(),
            &self.search,
            seated,
            agent,
        ) {
            tracing::debug!(%error, "native runtime client disconnected");
        }
    }
}

struct ConnectionSlot(Arc<AtomicUsize>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Answer one native connection that asks the seats alone ([`Seats::asks`]).
fn serve_seat(
    stream: &UnixStream,
    input: impl Read + Send + 'static,
    seated: &Seated,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let input = spawn_reader(input)?;
    let answer = |value: Value| {
        seated.answer(&value).unwrap_or_else(|| {
            Answer::InOrder(Box::pin(async {
                Ok(Some(
                    json!({"error": "a connection that asks the seats asks them alone"}),
                ))
            }))
        })
    };
    let ended = serve_lines(runtime, input, &mut &*stream, answer, None);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    ended
}

/// Answer one native connection: the daemon's own requests in order, and
/// MCP requests as [`serve_lines`] does, overlapping one another. An `agent`'s connection hears
/// first what [`Presence::answer_first`] answers.
fn serve_connection<H: tilo_actions::remote::ActionProvider + Send + Sync + 'static>(
    stream: &UnixStream,
    input: impl Read + Send + 'static,
    server: &Arc<McpServer<H>>,
    status: &Status,
    search: &OnceLock<Arc<tilo_os_search::OsSearch>>,
    seated: Option<&Seated>,
    agent: Option<(&Presence, &Visit)>,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Every connection thread waits on the one app runtime rather than
    // building a runtime per connection.
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let input = spawn_reader(input)?;
    let status = serde_json::to_value(status).map_err(io::Error::other)?;
    // An agent whose person revoked its key acts no further: its next request is answered why,
    // and the connection ends there.
    let ending = tilo_mcp::lines::Ending::default();
    let answer = |value: Value| {
        if let Some(answer) = seated.and_then(|seated| seated.answer(&value)) {
            return answer;
        }
        if value.get("method").and_then(Value::as_str) == Some(tilo_api::daemon::PEER_STATUS) {
            // Asked by `tilo status`: what this device holds, answered beside the MCP methods.
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            return Answer::InOrder(Box::pin(async move {
                Ok(Some(
                    json!({"jsonrpc": "2.0", "id": id, "result": crate::device::status().await}),
                ))
            }));
        }
        let native = value.get("native").and_then(Value::as_str);
        Answer::InOrder(match native {
            Some("status") if value == json!({"native":"status"}) => {
                let status = status.clone();
                Box::pin(async move { Ok(Some(status)) })
            }
            Some("activate") => {
                Box::pin(async move { Ok(Some(answer_activate(server, &value).await)) })
            }
            Some(tilo_host_search::OPEN) => Box::pin(async move {
                Ok(Some(match search.get() {
                    Some(search) => answer_open(search, &value).await,
                    None => json!({"error": SEARCH_NOT_STARTED}),
                }))
            }),
            _ => {
                if let Some(refused) = ending.answer(server, &value) {
                    return refused;
                }
                return match &agent {
                    Some((agents, visit)) => agents
                        .answer_first(visit, value)
                        .unwrap_or_else(|value| mcp_answer(server, value)),
                    None => mcp_answer(server, value),
                };
            }
        })
    };
    // Each answer is written whole, so a descriptor it carries (the link `seats#signed-in` hands
    // root) goes with its line.
    let mut writer = io::BufWriter::new(crate::seats::Handing {
        stream,
        handed: seated.map(|seated| &*seated.handed),
    });
    let mut ends = || ending.check();
    let ended = serve_lines(runtime, input, &mut writer, answer, Some(&mut ends));
    // The reader thread holds a clone of the socket: shut it, so the reader
    // sees its end and the client its connection close.
    let _ = stream.shutdown(std::net::Shutdown::Both);
    ended
}

/// A native activation request's answer: the newest live instance of the
/// named application brought forward, or that none runs.
async fn answer_activate<H: tilo_actions::remote::ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    request: &Value,
) -> Value {
    match request.get("app_id").and_then(Value::as_str) {
        Some(app) if !app.is_empty() && app.len() <= 4096 => {
            match server.activate_existing(app).await {
                Ok(Some(peer)) => json!({"activated": true, "peer_id": peer}),
                Ok(None) => json!({"activated": false}),
                Err(reason) => json!({"error": reason}),
            }
        }
        _ => json!({"error": "activation needs an application identity"}),
    }
}

/// What a host search result chosen before host search started is answered.
const SEARCH_NOT_STARTED: &str =
    "host search starts once this computer holds its workspace; nothing it found is open yet";

/// A native open request's answer: a host search result another process of
/// this runtime was handed (a Spotlight click reaches the bundle's app),
/// opened after the same re-check as any other.
pub(crate) async fn answer_open(search: &tilo_os_search::OsSearch, request: &Value) -> Value {
    let Some(entry) = request.get("entry").and_then(Value::as_str) else {
        return json!({"error": "an open request needs an entry"});
    };
    match search.open(entry).await {
        Ok(()) => json!({"opened": true}),
        Err(error) => json!({"error": format!("{error:?}")}),
    }
}

/// Bridge this process's stdio to the MCP host it belongs to: inside a
/// confined environment, the server the environment names
/// ([`tilo_paths::login::AGENT_SOCKET_ENV`]); inside a Tilo login, the
/// desktop's MCP server, which is never replaced by a standalone runtime;
/// elsewhere, or when `standalone`, the persistent daemon of `profile`,
/// its service started when none answers. EOF closes only this client's connection; neither
/// applications nor the daemon are owned here.
///
/// `workspace` is the workspace the client asks this desktop's MCP server for, written into
/// its `initialize`.
pub fn connect(
    profile: &Path,
    standalone: bool,
    forward: Option<&tilo_mcp::devices::Forward>,
    workspace: Option<&str>,
) -> io::Result<()> {
    let route = tilo_mcp::session_api::route(
        standalone,
        tilo_mcp::session_api::environment_server(std::env::var_os(
            tilo_paths::login::AGENT_SOCKET_ENV,
        )),
        tilo_paths::login::is_tilo_login(),
        tilo_mcp::session_api::agent_socket_path(
            tilo_paths::login::login_runtime_dir().map(Into::into),
        ),
        Path::exists,
    )?;
    let mut stream = match route {
        tilo_mcp::session_api::Route::Session(server) => {
            tilo_files::native::socket::connect(&server)
                .map_err(|error| tilo_mcp::session_api::unreachable_server(&server, &error))?
        }
        tilo_mcp::session_api::Route::Standalone if workspace.is_some() => {
            return Err(invalid(
                "--workspace asks a Tilo desktop's MCP server for a workspace, and this process \
                 is not in a Tilo login",
            ));
        }
        tilo_mcp::session_api::Route::Standalone => {
            running(profile)?;
            tilo_files::native::socket::connect(&native_socket(profile)?)?
        }
    };
    if let Some(forward) = forward {
        tilo_mcp::devices::ask_forward(&mut stream, forward)?;
    }
    bridge_stdio(
        stream,
        Asking {
            workspace: workspace.map(str::to_owned),
            key: tilo_mcp::session_api::agent_key()?,
            // A client started by an agent is that agent's session, shown by its name there when
            // the agent's harness gives it one.
            session_name: tilo_mcp::session_api::session_name(),
        },
    )
}

/// Attach connected service descriptors and replace this host with the app.
pub fn run_app(
    profile: &Path,
    app_id: &str,
    workspace: Option<&str>,
    executable: &OsString,
    args: &[OsString],
) -> io::Result<()> {
    let status = running(profile)?;
    // Only the argument-free default presentation is interchangeable here.
    // App-specific arguments must run their own parser and are never discarded.
    let launch_guard =
        if args.is_empty() && workspace.is_none_or(|workspace| workspace == status.workspace) {
            match default_launch_guard(profile, app_id, Duration::from_secs(3))? {
                Some(guard) => Some(guard),
                None => return Ok(()),
            }
        } else {
            None
        };
    let owner = std::process::id().to_string();
    // Only the bootstrap exec for this exact process can request resumption.
    // A same-app ordinary child must run its own command handler first.
    let resume = std::env::var_os("TILO_RUNTIME_CONNECTION_OWNER_PID").as_deref()
        == Some(std::ffi::OsStr::new(&owner))
        && std::env::var_os("TILO_RUNTIME_RESUME_LIVE").as_deref()
            == Some(std::ffi::OsStr::new(app_id));
    let mut command = Command::new(executable);
    command.args(args);
    // This process is introducing the local runtime workspace. A launcher that
    // already named a workspace keeps its participation, including fail-closed
    // viewer. A personal attach has no workspace yet, and the person who owns this
    // runtime may edit their own app.
    // A joined workspace's role is the one its governor admitted.
    if workspace.is_none()
        && std::env::var_os("TILO_WORKSPACE_ID").is_none()
        && std::env::var_os("TILO_PARTICIPATION").is_none()
    {
        command.env("TILO_PARTICIPATION", "author");
    }
    let sockets = sockets(profile)?;
    let connections = host::Connections::prepare(
        &mut command,
        app_id,
        workspace.unwrap_or(&status.workspace),
        &host::ServiceSockets {
            collab: &sockets.join(COLLAB_SOCKET),
            identity: &sockets.join(IDENTITY_SOCKET),
            blobs: &sockets.join(BLOB_SOCKET),
        },
    )?;
    command
        .env("TILO_RUNTIME_CONNECTION_OWNER_PID", owner)
        .env("TILO_RUNTIME_PROFILE", profile)
        .env(
            "TILO_RUNTIME_NATIVE",
            sockets.join(tilo_paths::machine::DAEMON_NATIVE),
        );
    if resume {
        command.env("TILO_RUNTIME_RESUME_LIVE", app_id);
    }
    if let Some(guard) = &launch_guard {
        use std::os::fd::AsRawFd;
        let fd = guard.file().as_raw_fd();
        command.env("TILO_RUNTIME_LAUNCH_GUARD_FD", fd.to_string());
        // SAFETY: the guard stays owned until exec and only fcntl runs here.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    } else {
        command.env_remove("TILO_RUNTIME_LAUNCH_GUARD_FD");
    }
    let error = command.exec();
    drop(connections);
    Err(error)
}

fn activate_existing(profile: &Path, app_id: &str) -> io::Result<bool> {
    let mut stream = tilo_files::native::socket::connect(&native_socket(profile)?)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = serde_json::to_vec(&json!({"native": "activate", "app_id": app_id}))
        .map_err(io::Error::other)?;
    stream.write_all(&request)?;
    stream.write_all(b"\n")?;
    let Frame::Line(bytes) = read_frame(&mut BufReader::new(stream))? else {
        return Err(invalid(
            "native activation endpoint returned no bounded reply",
        ));
    };
    let reply: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    match reply.get("activated").and_then(Value::as_bool) {
        Some(activated) => Ok(activated),
        None => Err(io::Error::other(
            reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("invalid native activation reply")
                .to_owned(),
        )),
    }
}

/// Kernel-held admission survives exec and is released on owner exit. File
/// contents and PID observations are diagnostics, never proof of a live owner.
fn default_launch_guard(
    profile: &Path,
    app_id: &str,
    grace: Duration,
) -> io::Result<Option<advisory_lock::FileLock>> {
    let directory = profile.join("launches");
    tilo_files::native::private::directory(&directory)?;
    let path = directory.join(format!("{}.lock", blake3::hash(app_id.as_bytes()).to_hex()));
    let deadline = Instant::now().checked_add(grace).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "native launch startup grace is too large",
        )
    })?;
    loop {
        if activate_existing(profile, app_id)? {
            return Ok(None);
        }
        match advisory_lock::FileLock::try_exclusive(host::private_file(&path)?) {
            Ok(guard) => {
                guard.resource_owner()?;
                return Ok(Some(guard));
            }
            Err(advisory_lock::TryLockError::Error(error)) => return Err(error),
            Err(advisory_lock::TryLockError::WouldBlock) => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "existing {app_id} launch has not published its window within the startup grace; its owner was retained"
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "daemon_limits_tests.rs"]
mod limits_tests;
