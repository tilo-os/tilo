//! Persistent native session shared by independently launched GUI apps and MCP.
use crate::device::presence::Presence;
use crate::device::{local::Frames, services::Daemon};
use crate::seats::{AppAt, Seated, Seats};
use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_api::daemon::admission::{Admitted, ROOT};
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_mcp::lines::{Answer, Frame, mcp_answer, read_frame, serve_lines, spawn_reader};
use tilo_mcp::session_api::{Asking, bridge_stdio};
use tilo_socket::{Listener, Stream};

use crate::daemon_args::invalid;
pub use crate::daemon_args::{Mode, default_profile, parse_args, validate_profile};

const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Every socket the daemon serving a profile binds, named in the one directory its clients
/// derive from [`sockets_dir`]: what it listens on and what an app it starts connects to are
/// this one value, never a path each side works out for itself.
#[derive(Clone, Debug)]
pub(crate) struct Sockets {
    /// The daemon's native endpoint, which a program it started calls as itself.
    pub(crate) native: PathBuf,
    pub(crate) collab: PathBuf,
    pub(crate) identity: PathBuf,
    pub(crate) blobs: PathBuf,
}

impl Sockets {
    /// The names a daemon's sockets have in the directory serving a profile keeps them in.
    pub(crate) fn in_dir(dir: &Path) -> Self {
        Self {
            native: dir.join(tilo_paths::machine::DAEMON_NATIVE),
            collab: dir.join(COLLAB_SOCKET),
            identity: dir.join(IDENTITY_SOCKET),
            blobs: dir.join(BLOB_SOCKET),
        }
    }

    fn all(&self) -> [&Path; 4] {
        [&self.native, &self.collab, &self.identity, &self.blobs]
    }
}

/// The sockets the daemon serving `profile` binds.
///
/// # Errors
/// The directory cannot be made, or a socket's path is longer than a socket's address holds.
pub(crate) fn sockets(profile: &Path) -> io::Result<Sockets> {
    Ok(Sockets::in_dir(&sockets_dir(profile)?))
}

/// The directory the daemon serving `profile` keeps its sockets in: this computer's own listens
/// where every client finds it ([`tilo_api::daemon::this_computer`]), never inside a profile whose
/// path the home folder lengthens; a profile named apart (a test's, a VM host's, a stack's) keeps
/// its own directory beside that one ([`tilo_api::daemon::profile_sockets`]), named by its path,
/// so no profile's path ever becomes a socket's path. Either way a socket whose path its address
/// cannot hold is refused, by name.
fn sockets_dir(profile: &Path) -> io::Result<PathBuf> {
    let dir = if crate::daemon_args::own_profile().is_ok_and(|own| own == profile) {
        tilo_api::daemon::this_computer()
            .map_err(io::Error::other)?
            .sockets()
            .to_owned()
    } else {
        tilo_api::daemon::profile_sockets(profile).map_err(io::Error::other)?
    };
    for socket in Sockets::in_dir(&dir).all() {
        tilo_api::daemon::socket_fits(socket).map_err(io::Error::other)?;
    }
    Ok(dir)
}

/// The native endpoint of the daemon serving `profile`, in its [`sockets`].
fn native_socket(profile: &Path) -> io::Result<PathBuf> {
    Ok(sockets(profile)?.native)
}

/// Name the endpoint `native` in `profile`, as `<profile>/native.sock`: a link, so a host that
/// starts the daemon over a profile finds it where it looks whatever the sockets' own directory
/// is, and the profile's path never has to fit a socket's address.
#[cfg(unix)]
fn publish_native(profile: &Path, native: &Path) -> io::Result<()> {
    let named = profile.join(tilo_paths::machine::DAEMON_NATIVE);
    if named == native {
        return Ok(());
    }
    match std::fs::symlink_metadata(&named) {
        Ok(_) => std::fs::remove_file(&named)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::os::unix::fs::symlink(native, &named)
}

/// [`sockets`] with the services' own in [`service_sockets`] instead: a daemon of the computer's
/// keeps those where only it may enter, and its native endpoint where every client finds it.
pub(crate) fn sockets_served(profile: &Path) -> io::Result<Sockets> {
    let mut sockets = sockets(profile)?;
    let services = service_sockets(profile)?;
    sockets.collab = services.join(COLLAB_SOCKET);
    sockets.identity = services.join(IDENTITY_SOCKET);
    sockets.blobs = services.join(BLOB_SOCKET);
    Ok(sockets)
}

/// The directory the profile's services bind `collab.sock`, `identity.sock` and `blobs.sock` in:
/// the daemon's own [`sockets_dir`], or, for a daemon of the computer's that serves every account,
/// the account's own below it, which only the daemon may enter. A broker socket refuses a
/// directory other accounts can reach, while the native endpoint every account calls must stay in
/// one they reach: the two are kept apart here.
///
/// # Errors
/// The directory cannot be made, or a socket's path is longer than a socket's address holds.
pub(crate) fn service_sockets(profile: &Path) -> io::Result<PathBuf> {
    let own = sockets_dir(profile)?;
    let account = tilo_api::daemon::admission::installed()
        .and_then(tilo_api::daemon::admission::Admission::device_person_uid);
    service_sockets_at(&own, tilo_api::daemon::system_daemon(), account)
}

/// [`service_sockets`] for a daemon that is the computer's (`system`) or one account's, serving
/// the person of `account`.
fn service_sockets_at(own: &Path, system: bool, account: Option<u32>) -> io::Result<PathBuf> {
    let Some(account) = account.filter(|_| system) else {
        return Ok(own.to_owned());
    };
    let dir = own.join("accounts").join(account.to_string());
    for name in [COLLAB_SOCKET, IDENTITY_SOCKET, BLOB_SOCKET] {
        tilo_api::daemon::socket_fits(&dir.join(name)).map_err(io::Error::other)?;
    }
    tilo_files::native::private::directory(&dir)?;
    Ok(dir)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    version: u32,
    pid: u32,
    /// The workspace the daemon serves now: empty until the person's workspace is founded, or
    /// comes in from their desktop. Asking founds none, so a desktop about to hand its
    /// workspaces over may ask. The probe answers it as `workspace`, the name launchers and
    /// scripts outside this crate read.
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
    // The endpoint answers from boot, before the person's workspace is founded, so a status
    // naming none is the daemon's own answer, not another's.
    if status.version != 1 || status.pid == 0 || status.workspace.len() > 4096 {
        return Err(invalid("invalid native runtime status"));
    }
    Ok(status)
}

/// How long a client waits for the daemon the service started to serve its person.
const ANSWERING: Duration = Duration::from_secs(120);

/// The daemon serving `profile`, once its native endpoint answers: asked at once, and, when it
/// does not answer, after the one start ([`tilo_api::daemon::service::start`]) for a profile that
/// is this computer's. A profile named apart (a test's, a VM host's) is served by whoever started
/// its daemon, and is only asked. Its status names the person's workspace once there is one: at
/// boot, before anything founded it or their desktop handed it over, it names none.
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
                        .and_then(|place| place.found(&tilo_api::daemon::Founding::default()))
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
    // Heard from the daemon's first moment: before there is a person, waiting for one is all it
    // does, and a signal the caller had ignored is inherited across exec.
    let stop = crate::device::serve::Stop::default();
    let native = native_socket(profile)?;
    crate::device::serve::hear_stops(stop.clone(), crate::device::serve::STOPPED_WITHIN, native)
        .map_err(|error| io::Error::other(format!("{error:#}")))?;
    Running::start_serving(profile, dev, stop)?.wait()
}

/// This computer's daemon running in this process: the native endpoint the apps and `--connect`
/// reach, and the device as [`crate::device::hold`] serves it. The `tilo-runtime` executable runs
/// one for its life; an app that is its own computer's daemon (Android's) runs one beside its
/// windows, and may stop it and start it again.
pub struct Running {
    native: PathBuf,
    stop: crate::device::serve::Stop,
    /// The native endpoint, which takes no connection once it is closing.
    endpoint: Arc<Native>,
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
        let deadline = Instant::now()
            .checked_add(within)
            .unwrap_or_else(Instant::now);
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

impl Running {
    /// Start the daemon over `profile`, returning once its native endpoint is bound: it answers
    /// the device's own actions at once, and its person's once their services run, which they
    /// start on a thread of the daemon's.
    ///
    /// # Errors
    /// The profile is not a private absolute directory, another daemon has this device, or the
    /// native endpoint cannot be bound.
    pub fn start(profile: &Path) -> io::Result<Self> {
        Self::start_serving(profile, false, crate::device::serve::Stop::default())
    }

    /// [`Self::start`], serving builds under development too when `dev`, and told to stop by
    /// `stop`.
    fn start_serving(
        profile: &Path,
        dev: bool,
        stop: crate::device::serve::Stop,
    ) -> io::Result<Self> {
        validate_profile(profile)?;
        let device = crate::device::unclaimed::ThisDevice::lock(profile.to_path_buf())?;
        // Who each connection is: root and this daemon's own account act as the device, and
        // under the system unit the account that made the device's person, kept in the profile,
        // and the people signed in act as themselves.
        tilo_api::daemon::admission::install(
            tilo_api::daemon::admission::Admission::new(
                crate::seats::own_account(),
                tilo_api::daemon::system_daemon(),
            )
            .keeping_accounts_in(profile.to_path_buf())?,
        );
        // The native endpoint answers from boot, before anyone's certificate names this device
        // and while its person's services start: the device's own actions first, the person's
        // once they run. This process has the device's lock, so only it removes a stale endpoint.
        let path = native_socket(profile)?;
        let listener = tilo_files::native::private::bind_socket(&path)?;
        #[cfg(unix)]
        publish_native(profile, &path)?;
        #[cfg(unix)]
        if tilo_api::daemon::system_daemon() {
            use std::os::unix::fs::PermissionsExt as _;
            // Every account reaches it; the admission decides what each is answered.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))?;
        }
        let native = Arc::new(Native::new(
            device.answers(),
            crate::seats::peer_of,
            tilo_api::daemon::admission::here(crate::seats::own_account),
        ));
        let endpoint = Arc::clone(&native);
        let accepting = {
            let native = Arc::clone(&native);
            std::thread::Builder::new()
                .name("tilo-native".into())
                .spawn(move || {
                    if let Err(error) = native.accept(&listener) {
                        tracing::debug!(%error, "the native endpoint stopped taking connections");
                    }
                })?
        };
        tracing::info!("answering this device's actions on its native endpoint");
        let person = Arc::new(Person::default());
        let serving = {
            let (profile, stop, person) = (profile.to_owned(), stop.clone(), Arc::clone(&person));
            std::thread::Builder::new()
                .name("tilo-daemon".into())
                .spawn(move || serve_device(&profile, dev, &device, &native, &stop, &person))?
        };
        Ok(Self {
            native: path,
            stop,
            endpoint,
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
        self.person.within(within)?.start_shell(command, None)
    }

    /// The shell's end of a link the daemon made, once the person's services run (within
    /// `within`): made and admitted exactly as [`Self::start_shell`] hands one to the shell it
    /// starts, for a test's session that runs in this process to take instead
    /// (`tilo_session::Options::shell_link`).
    ///
    /// # Errors
    /// The person's services did not start in time, or the link could not be made.
    #[cfg(feature = "test-support")]
    pub fn link_shell(
        &self,
        within: Duration,
    ) -> io::Result<runtime_portal::process_link::Descriptor> {
        Ok(runtime_portal::packet::descriptor_of(
            self.person.within(within)?.shell_link()?,
        ))
    }

    /// The person the daemon serves, by their id (the hex of their root), once their services
    /// run (within `within`): who a test's session in this process is the shell of
    /// ([`Self::link_shell`]), and whose own workspace the device's resources are read in.
    ///
    /// # Errors
    /// The person's services did not start in time.
    #[cfg(feature = "test-support")]
    pub fn person(&self, within: Duration) -> io::Result<String> {
        use crate::RuntimeServices as _;
        Ok(self.person.within(within)?.actor().to_owned())
    }

    /// Take `delta`, an encoded search index change, as the app `app` publishes it on its
    /// connection here from its record `record` of `workspace`, once the person's services run
    /// (within `within`): what an app a test runs in this process holds reaches the device's one
    /// index exactly as a running app's does, and every caller of the daemon's search finds it.
    ///
    /// # Errors
    /// The person's services did not start in time, the daemon keeps no index yet, or the index
    /// refused the change.
    #[cfg(feature = "test-support")]
    pub fn publish_search_entries(
        &self,
        within: Duration,
        (workspace, app, record): (&str, &str, &str),
        delta: &[u8],
    ) -> io::Result<()> {
        let workspace = tilo_desktop_model::WorkspaceId::parse(workspace)
            .ok_or_else(|| invalid(format!("{workspace} names no workspace")))?;
        self.person
            .within(within)?
            .search_index_services()
            .ok_or_else(|| io::Error::other("the daemon keeps no search index yet"))?
            .publish(workspace, app, record, delta)
            .map_err(io::Error::other)
    }

    /// Where the daemon's native endpoint listens.
    #[must_use]
    pub fn native_socket(&self) -> &Path {
        &self.native
    }

    /// The person's services once they run, waiting up to `within` for them to start: the
    /// workspaces this daemon holds and the packages it installed from them, which a view of the
    /// device in this process reads.
    pub(crate) fn services(
        &self,
        within: Duration,
    ) -> io::Result<Arc<crate::device::services::Daemon>> {
        self.person.within(within)
    }

    /// Wait up to `within` for the person's services to run: a test's proof that the daemon
    /// serves them, and did not end.
    #[cfg(test)]
    pub(crate) fn wait_for_person(&self, within: Duration) -> io::Result<()> {
        self.person.within(within).map(drop)
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

    /// Close the native endpoint and wait for its thread: it takes no connection once it is
    /// closing, and the one made here wakes its wait for the next.
    fn close(&mut self) {
        self.endpoint.closing.store(true, Ordering::Relaxed);
        if let Some(accepting) = self.accepting.take() {
            drop(tilo_socket::connect(&self.native));
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

    /// The shell's process, while the session runs.
    pub fn shell(&mut self) -> Option<&mut std::process::Child> {
        self.shell.as_mut()
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
    device: &crate::device::unclaimed::ThisDevice,
    native: &Native,
    stop: &crate::device::serve::Stop,
    person: &Person,
) -> io::Result<()> {
    // Before this computer's identity is one its person's services can start as, it is served
    // with the key alone: a ticket it was handed is spent on the way, and the person's services
    // start once founding, a login or that ticket makes the identity, from `devices#found`. A
    // certificate that no identity names, or one this device cannot act as, leaves the daemon
    // answering its own actions rather than ending.
    if !device.claimed() {
        device.spend_handed_ticket();
        if !device.until_ready(stop) {
            return Ok(());
        }
    }
    // A device a ticket made enters through the server the ticket named until its copy of the
    // person's own workspace names it.
    let mut entered = device.entered();
    // What a founding call waits on for the first tickets it asked for.
    let founding = device.founding();
    let device_key = device.device_key();
    let _lock = device.device_lock();
    loop {
        // A stop heard while this computer waited for its person ends the daemon without
        // starting their services.
        if stop.told() {
            return Ok(());
        }
        stop.waiting_on("the wait between its person's parts");
        // In a build that is not a release, a test can hold the daemon between its parts, where
        // a step of the part that ended does not return: what a folder that will not unmount
        // does. The stop must still end the daemon there.
        #[cfg(debug_assertions)]
        {
            if !stop.told()
                && let Some(block) = std::env::var_os("TILO_TEST_HOLD_BETWEEN_PARTS_MS")
                    .and_then(|ms| ms.to_str().and_then(|ms| ms.parse::<u64>().ok()))
            {
                tracing::info!(
                    ms = block,
                    "a test holds the daemon between its person's parts"
                );
                std::thread::sleep(std::time::Duration::from_millis(block));
            }
        }
        // Each stage is the work under way until the next begins: a start that hangs is named
        // by the stage it hangs in, in the stall's report.
        let mut watched = None;
        let mut stage = |stage: &str| {
            tracing::debug!(stage, "starting the person's services");
            let what = format!("starting the person's services: {stage}");
            stop.waiting_on(what.clone());
            watched = Some(tilo_rt::stall::doing(what));
        };
        let session = match Daemon::start_locked(
            profile,
            &mut stage,
            dev,
            (device_key, entered.take()),
            Arc::clone(&founding),
        ) {
            Ok(session) => Arc::new(session),
            // The identity went away before its services could start: keep answering the
            // device's own actions, which is where founding and logging in are, until it is one
            // its person's services can start as again.
            Err(error) if !device.claimed() => {
                tracing::info!(
                    %error,
                    "this computer has no identity its person's services can start as yet: \
                    answering the device's own actions"
                );
                if !device.until_ready(stop) {
                    return Ok(());
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        drop(watched);
        tracing::info!("this daemon's person's part started");
        // The origin connectors this device starts reach the one dispatch on the connection
        // handed them, admitted as the app the launch record says they run.
        session.hand_actions_to_connectors();
        // Migrations run on the device that holds the data they read.
        session.serve_migrations();
        // The secrets the apps the move carried over kept in this device's old keeper are the
        // person's vault items from now on: walked once per start, as the person, and taken out
        // of the keeper in the same step.
        session.adopt_kept_secrets();
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
        native.seats.follow_launches(app_of(&session));
        native.serve_apps(app_take(&session));
        #[cfg(unix)]
        session.apps().serve_own_lines(own_lines(&session));
        native
            .seats
            .serve(Arc::new(PersonShells(Arc::downgrade(&session))));
        native.serve_the_person(the_persons(&session, search));
        native.serve_keys(key_endpoint(&session));
        person.set(Some(&session));
        // A part serves from here: a stop told now is the part's to take up, with its folders to
        // unmount and its trees to keep.
        stop.serving(true);
        // The device is served until the daemon is told to stop, another takes the device over,
        // or this computer logs in with a ticket: its person's part starts again as that device,
        // the device's own part (the native endpoint, the device's lock) staying up meanwhile.
        let held = crate::device::hold(&session, stop);
        // Between parts from here: a stop told now has nothing of a part's to let go of, and
        // ends the daemon rather than wait for the start that follows to take it up.
        stop.serving(false);
        person.set(None);
        native.forget_the_person();
        if stop.told() {
            // The part ended because it was told to stop: what it served is let go by the
            // process ending, as a kill would let go of it, rather than closed for a start that
            // will not come.
            drop(session);
            return Ok(());
        }
        let closed = session.connections().end();
        tracing::info!(closed, "the person's part closed the connections it served");
        // Its workspace service stops answering and saves what it took in, and its endpoint
        // closes, before the next part opens the same stores and enters as the same device.
        let workspaces = Arc::clone(session.serving());
        let left = Arc::downgrade(&session);
        crate::device::serve::ending_step(
            Some(stop),
            "the person's part's workspace service",
            CLOSING_WORKSPACES,
            move || {
                crate::device::workspaces::block_on_owned(async move {
                    workspaces.shutdown().await;
                });
                // What the part kept of its folders lets go here, on this step's own thread: a
                // mount that will not unmount blocks this step alone, never the daemon that
                // has to hear its stop.
                drop(session);
            },
        );
        match held.map_err(|error| io::Error::other(format!("{error:#}")))? {
            crate::device::Ended::Stopped => return Ok(()),
            crate::device::Ended::LoggedIn(device) => entered = Some(*device),
        }
        // What still holds the part that ended keeps its workspaces' stores and links open,
        // which the part that starts now opens again: said, so a start that waits on them
        // names why.
        if left.strong_count() > 0 {
            tracing::error!(
                held = left.strong_count(),
                "the person's part that ended is still held; its services were not let go"
            );
        }
        tracing::info!("this daemon's person's part starts again");
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
    fn arrived(&self, importing: bool) -> io::Result<Stream> {
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
        window: Option<runtime_portal::process_link::Descriptor>,
    ) -> io::Result<std::process::Child> {
        let daemon = self.daemon()?;
        let mut command = std::process::Command::new(shell_program()?);
        command.args(args).stdin(std::process::Stdio::null());
        the_shells_environment(&mut command, env);
        daemon.start_shell(command, window)
    }

    #[cfg(not(target_os = "linux"))]
    fn start(
        &self,
        _: Vec<std::ffi::OsString>,
        _: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        _: Option<runtime_portal::process_link::Descriptor>,
    ) -> io::Result<std::process::Child> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the shell starts on its own on this computer",
        ))
    }
}

/// The environment a shell the daemon starts runs with: the account's, but for what would load
/// code into it ([`shell_reads`]), and what the daemon's own sets for the display it draws with
/// ([`shell_keeps_from_the_daemon`]).
#[cfg(target_os = "linux")]
fn the_shells_environment(
    command: &mut Command,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) {
    command
        .env_clear()
        .envs(std::env::vars_os().filter(|(name, _)| shell_keeps_from_the_daemon(name)))
        .envs(env.into_iter().filter(|(name, _)| shell_reads(name)));
}

/// Names the session program a test's daemon starts as its person's shell, in the daemon's own
/// environment, where a test build of the session is not installed beside the daemon's.
#[cfg(feature = "test-support")]
pub const TEST_SHELL_ENV: &str = "TILO_TEST_SHELL";

/// The shell the daemon starts: the session installed beside it.
#[cfg(target_os = "linux")]
fn shell_program() -> io::Result<PathBuf> {
    /// The shell, installed beside this daemon.
    const SHELL: &str = "tilo-session";
    #[cfg(feature = "test-support")]
    if let Some(program) = std::env::var_os(TEST_SHELL_ENV) {
        return Ok(PathBuf::from(program));
    }
    std::env::current_exe()?
        .parent()
        .map(|dir| dir.join(SHELL))
        .filter(|program| program.is_file())
        .ok_or_else(|| io::Error::other(format!("{SHELL} is not installed beside tilo-runtime")))
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
        #[cfg(unix)]
        launch: Some(launch_take(session)),
        #[cfg(not(unix))]
        launch: None,
        connections: Some(Arc::clone(session.connections())),
    }
}

/// How long the workspace service of a person's part that ended may take to save what it took
/// in and close its endpoint before the next part starts without waiting for it.
const CLOSING_WORKSPACES: Duration = Duration::from_secs(10);

/// What finds the app a process the daemon started runs, from the launch records `session`
/// keeps.
fn app_of(session: &Arc<Daemon>) -> crate::seats::AppOf {
    let session = Arc::downgrade(session);
    Arc::new(move |pid| session.upgrade()?.app_of(pid))
}

/// What answers a connection from a process the daemon started, as the app it runs.
fn app_take(session: &Arc<Daemon>) -> AppTake {
    let session = Arc::downgrade(session);
    Box::new(move |stream, slot, seated, app| {
        let session = session
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        serve_app(stream, slot, seated, &session, &app)
    })
}

/// Answer one connection from an app the daemon started, as that app in its workspace: the
/// action wire alone, decided by the workspace's broker with the app's grants. Anything else an
/// app asks, its MCP projection above all, is refused before its first call.
fn serve_app(
    stream: Stream,
    slot: ConnectionSlot,
    _seated: Seated,
    session: &Arc<Daemon>,
    app: &AppAt,
) -> io::Result<()> {
    // Closed when the person's part ends, as every connection it serves is.
    let held = session.connections().hold(&stream);
    serve_as_app(stream, Frames::of_an_app(session, app), (slot, held))
}

/// Answer `stream` with `frames`, an app's own connection, on a thread of its own while it lasts,
/// holding `held` until it ends: the action wire alone, its first line a frame.
fn serve_as_app(stream: Stream, frames: Frames, held: impl Send + 'static) -> io::Result<()> {
    std::thread::Builder::new()
        .name("tilo-native-app".into())
        .spawn(move || {
            let _held = held;
            let reader = match stream.try_clone() {
                Ok(reader) => reader,
                Err(error) => {
                    tracing::debug!(%error, "an app's native connection");
                    return;
                }
            };
            let mut reader = BufReader::new(reader);
            let mut first = Vec::new();
            if let Err(error) = (&mut reader).take(FIRST_LINE).read_until(b'\n', &mut first) {
                tracing::debug!(%error, "an app's native connection");
                return;
            }
            let line = first.strip_suffix(b"\n").unwrap_or(&first).to_vec();
            if !crate::device::local::is_frame(&line) {
                // An app reaches the device's actions alone, never its MCP projection.
                crate::device::local::refuse(
                    stream,
                    "an app's connection speaks the action wire alone",
                );
                return;
            }
            frames.serve(stream, &line, reader);
        })?;
    Ok(())
}

/// What makes the daemon line of the tilolang process the daemon started to run one app's own
/// instances ([`crate::device::processes::OwnLines`]).
///
/// That process's sandbox holds its process link and nothing else: no socket of the daemon's
/// ([`launcher::run::tilolang_process`]). Its Rust host still has to reach the calls a sandboxed
/// app makes on its held connection for itself (keeping and reading back its own secrets, the key
/// this device holds for it), so the daemon makes a connected pair, serves its end exactly as the
/// native endpoint serves a process it started for an app ([`Frames::of_an_app`]), and hands the
/// other end over the process link. It is the one path from a tilolang app's process to the
/// daemon. The line is bound to the process by its pid, app and workspace as the daemon recorded
/// them at its start: whatever writes on it is answered as that process, for as long as that
/// process runs, and as nothing once it ended.
#[cfg(unix)]
pub(crate) fn own_lines(session: &Arc<Daemon>) -> crate::device::processes::OwnLines {
    let session = Arc::downgrade(session);
    Arc::new(move |pid, app: &str, workspace: &str| {
        let session = session
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        let (ours, theirs) = tilo_socket::pair()?;
        let at = AppAt {
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            pid,
        };
        // Closed when the person's part ends, as every connection it serves is.
        let held = session.connections().hold(&ours);
        serve_as_app(ours, Frames::of_an_app(&session, &at), held)?;
        Ok(runtime_portal::packet::descriptor_of(theirs))
    })
}

/// Serve `session`'s native endpoint at `socket`, as the daemon does once the person's services
/// run, each connection's peer read by `peer_of` and admitted by `admission`: what a test of
/// the endpoint reaches.
#[cfg(test)]
pub(crate) fn serve_for_test(
    session: &Arc<Daemon>,
    device: crate::device::unclaimed::DeviceOnly,
    peer_of: crate::seats::PeerOf,
    admission: &'static tilo_api::daemon::admission::Admission,
    socket: &Path,
) -> io::Result<()> {
    let native = Arc::new(Native::new(device, peer_of, admission));
    native.seats.follow_launches(app_of(session));
    native.serve_apps(app_take(session));
    native.serve_the_person(the_persons(session, Arc::new(OnceLock::new())));
    native.serve_keys(key_endpoint(session));
    let listener = tilo_files::native::private::bind_socket(socket)?;
    std::thread::Builder::new()
        .name("tilo-native-test".into())
        .spawn(move || native.accept(&listener))?;
    Ok(())
}

/// A connection the native endpoint takes, with the slot it counts against and who it is.
type Take = Box<dyn Fn(Stream, ConnectionSlot, Seated) -> io::Result<()> + Send + Sync>;

/// A connection from an app the daemon started: the app it runs and the workspace it runs in,
/// with the slot it counts against and who it is.
type AppTake = Box<dyn Fn(Stream, ConnectionSlot, Seated, AppAt) -> io::Result<()> + Send + Sync>;

/// A connection of an account signed in as another person than the device's, taken as the key
/// of theirs it acts as, with that key's certificate.
type KeyTake = Box<
    dyn Fn(
            Stream,
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
    /// What answers a connection from an app the daemon started, while the person's services
    /// run: the app's action frames alone, never the person's MCP projection.
    apps: Mutex<Option<Arc<AppTake>>>,
    /// The endpoint of the other people signed in on the computer, each as their own key, while
    /// the person's services run.
    keys: Mutex<Option<Arc<KeyTake>>>,
    /// Told when [`Self::person`] is set.
    started: (Mutex<()>, Condvar),
    /// Set as the daemon stops: the endpoint takes no more connections.
    closing: AtomicBool,
}

impl Native {
    /// The endpoint of `device`, reading each connection's account with `peer_of` and admitting
    /// it by `admission`.
    fn new(
        device: crate::device::unclaimed::DeviceOnly,
        peer_of: crate::seats::PeerOf,
        admission: &'static tilo_api::daemon::admission::Admission,
    ) -> Self {
        let seats = Arc::new(Seats::new(admission, device.clone(), peer_of));
        Self {
            answers: device,
            seats,
            started: (Mutex::new(()), Condvar::new()),
            person: Mutex::new(None),
            apps: Mutex::new(None),
            keys: Mutex::new(None),
            closing: AtomicBool::new(false),
        }
    }

    /// Serve `stream` the device's own actions, as the account `seated` asks them.
    fn device_only(&self, stream: Stream, slot: ConnectionSlot, seated: Seated) -> io::Result<()> {
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
            launch: None,
            connections: None,
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

    /// Answer a connection from a process the daemon started with `take` from now on, as the app
    /// the launch record names.
    fn serve_apps(&self, take: AppTake) {
        *self.apps.lock_recover() = Some(Arc::new(take));
    }

    /// What answers a connection from an app the daemon started, while the person's services run.
    fn app(&self) -> Option<Arc<AppTake>> {
        self.apps.lock_recover().clone()
    }

    /// Serve no connection the person's from now on, while their services start again: one that
    /// comes meanwhile waits for them, and another person's is answered the device's own actions.
    fn forget_the_person(&self) {
        self.person.lock_recover().take();
        self.apps.lock_recover().take();
        self.keys.lock_recover().take();
    }

    /// The person's endpoint, while their services run.
    fn person(&self) -> Option<Arc<Take>> {
        self.person.lock_recover().clone()
    }

    /// Serve each connection to `listener` as whatever answers when it comes ([`accept`]): an
    /// account the daemon does not serve is answered the device's own actions alone.
    fn accept(self: &Arc<Self>, listener: &Listener) -> io::Result<()> {
        accept(listener, |stream, slot| {
            if self.closing.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "the native endpoint closed",
                ));
            }
            let seated = Seated::new(self.seats.seat(&stream), Arc::clone(&self.seats));
            self.take(stream, slot, seated)
        })
    }

    fn take(
        self: &Arc<Self>,
        stream: Stream,
        slot: ConnectionSlot,
        seated: Seated,
    ) -> io::Result<()> {
        // Under the system unit root acts as the device, saying who signed in and who left:
        // answered at once, never held while the person's services start. A person's own daemon
        // that runs as root (a container's, a sandbox's) serves root as its own account.
        if !seated.seat.admitted.served() || (seated.seat.uid == ROOT && self.seats.system()) {
            return self.device_only(stream, slot, seated);
        }
        // A process the daemon started is the app it runs, and reaches that app's own actions
        // alone: never the person's MCP projection or their device's actions.
        if let Some(app) = seated.seat.app.clone() {
            let Some(take) = self.app() else {
                // The app's services are not up: it reaches nothing until they are, never the
                // device's own actions in their place.
                crate::device::local::refuse(
                    stream,
                    "this computer is not serving its apps' workspaces yet",
                );
                return Ok(());
            };
            return take(stream, slot, seated, app);
        }
        // Another person than the device's acts as their own key, and is never served the
        // person's endpoint: until it runs, the device's own actions answer them.
        if matches!(seated.seat.admitted, Admitted::Key { .. }) {
            let take = self.keys.lock_recover().clone();
            return match (take, self.seats.key(&seated.seat)) {
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
                        let deadline = Instant::now()
                            .checked_add(WAITED_WITHIN)
                            .unwrap_or_else(Instant::now);
                        let mut told = native.started.0.lock_recover();
                        let person = loop {
                            if let Some(person) = native.person() {
                                break Some(person);
                            }
                            let left = deadline.saturating_duration_since(Instant::now());
                            if left.is_zero() {
                                break None;
                            }
                            told = native
                                .started
                                .1
                                .wait_timeout(told, left)
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .0;
                        };
                        drop(told);
                        // A person's part that has not started in time is said, never waited on
                        // for as long as it is broken: the caller would otherwise wait forever.
                        let Some(person) = person else {
                            crate::device::serve::refuse(
                                &stream,
                                "this computer's Tilo has not started its person's part yet: run \
                                 `tilo status` to see how it stands, then try again",
                            );
                            return;
                        };
                        if let Err(error) = person(stream, slot, seated) {
                            tracing::debug!(%error, "a native connection that waited while the person's services started was not served");
                        }
                    })
                    .map(drop)
            }
        }
    }
}

/// How long a connection waits for the person's part to take it before it is told why it is not
/// served. The agents' endpoint bounds its own handoff by the same name.
const WAITED_WITHIN: Duration = Duration::from_secs(120);

/// Hand each connection to `listener` to `take`, at most [`MAX_CONNECTIONS`] at once. One more
/// is told there is no room and closed ([`refuse_busy`]).
fn accept(
    listener: &Listener,
    take: impl Fn(Stream, ConnectionSlot) -> io::Result<()>,
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
    /// What starts an app this computer's person asks it to run (`--run-app`): the daemon makes
    /// the launch, so the app is attested as one. `None` for an endpoint that starts nothing.
    launch: Option<Arc<LaunchTake>>,
    /// The connections of the person's part this endpoint serves for, each closed when that part
    /// ends; `None` for an endpoint that serves the device alone, which outlives every part.
    connections: Option<Arc<Connections>>,
}

/// A launcher's request to start an app on this computer, with what the connection sent with it.
type LaunchTake =
    dyn Fn(Stream, &[u8], Vec<runtime_portal::process_link::Descriptor>) + Send + Sync;

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
        // An agent's connection is its own run, never the person's own act: an action frame it
        // sends acts as its person and what a workspace asks of them is put to them.
        frames: Frames::of_a_persons_program(daemon),
        to_agents: None,
        launch: None,
        connections: Some(Arc::clone(daemon.connections())),
    }
}

/// How the native endpoint serves a connection of another person than the device's: each as a
/// run of the key of theirs it acts as, in `session`'s workspace, over MCP and action frames
/// alike.
fn key_endpoint(session: &Arc<Daemon>) -> KeyTake {
    let session = Arc::downgrade(session);
    Box::new(move |stream, slot, seated, key, cert| {
        let daemon = session
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        Endpoint {
            frames: Frames::of_a_key(&daemon, key.clone(), cert.clone()),
            session_factory: daemon.key_sessions(key, cert),
            status: status_of(&daemon),
            search: Arc::new(OnceLock::new()),
            media: None,
            other_devices: None,
            agents: None,
            to_agents: None,
            launch: None,
            connections: Some(Arc::clone(daemon.connections())),
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
fn refuse_busy(mut stream: Stream) {
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
            launch: self.launch.clone(),
            connections: self.connections.clone(),
        }
    }
}

impl<H: tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static> Endpoint<H> {
    /// Serve `stream` on its own thread, counted in `slot` while it lasts, as `seated` says
    /// who it is.
    fn take(&self, stream: Stream, slot: ConnectionSlot, seated: Option<Seated>) -> io::Result<()> {
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
    pub(crate) fn serve(&self, stream: Stream, read: Vec<u8>, seated: Option<&Seated>) {
        let _held = self
            .connections
            .as_ref()
            .and_then(|connections| connections.hold(&stream));
        // The first line says whether the client asks to be carried to another device's MCP
        // server, or to have a shell started; anything else is served here, that line first. A
        // descriptor sent with it (a session's window connection, on its start) comes along.
        let (mut first, rest, descriptors) = match first_line(&stream, read) {
            Ok(read) => read,
            Err(error) => {
                tracing::debug!(%error, "native runtime client socket");
                return;
            }
        };
        let follow = match stream.try_clone() {
            Ok(clone) => clone,
            Err(error) => {
                tracing::debug!(%error, "native runtime client socket");
                return;
            }
        };
        // A first line that is an action frame says the connection speaks the action wire,
        // answered as whoever its account is admitted as.
        let line = first.strip_suffix(b"\n").unwrap_or(&first);
        if crate::device::local::is_frame(line) {
            let input = BufReader::new(io::Cursor::new(rest).chain(follow));
            return self.frames.serve(stream, line, input);
        }
        if line == tilo_api::daemon::AGENT_LINE.as_bytes() {
            if let Some(agents) = &self.to_agents {
                return agents.take(stream, rest);
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
                Some(other_devices) if rest.is_empty() => {
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
        if let Some(seated) = seated.filter(|_| asks_the_seats) {
            seated.receives(descriptors);
            let input = io::Cursor::new(first).chain(io::Cursor::new(rest).chain(follow));
            if let Err(error) = serve_seat(&stream, input, seated) {
                tracing::debug!(%error, "native seat client disconnected");
            }
            return;
        }
        // A launcher asks this computer to start an app: the daemon makes the launch, as it
        // makes every launch, so the app is attested as the one it runs. The request is the
        // whole connection, and the request's standard input and output ride it.
        #[cfg(unix)]
        if let Some(launch) = self.launch.clone().filter(|_| is_launch_request(&first)) {
            launch(stream, &first, descriptors);
            return;
        }
        let input = io::Cursor::new(first).chain(io::Cursor::new(rest).chain(follow));
        // The session is admitted by the first request that needs one, never by a connection's
        // coming: a connection that only asks what this device is (a desktop about to hand its
        // workspaces over, a computer whose person's part is still starting) must be answered
        // without founding a workspace or waiting for one, and goes on asking as it likes.
        let hand = seated.map(|seated| Arc::clone(&seated.handed));
        let admit = || {
            let made = crate::seats::making_for(hand.clone(), || (self.session_factory)())?;
            Ok::<_, io::Error>(Arc::new(made.with_media(self.media.clone())))
        };
        if let Err(error) = serve_connection(
            &stream,
            input,
            &admit,
            &(self.status)(),
            &self.search,
            seated,
            self.agents.as_ref(),
        ) {
            tracing::debug!(%error, "native runtime client disconnected");
        }
    }
}

/// The connections a part of the daemon serves for its person: each is closed when that part
/// ends ([`Self::end`]). One left open would be answered by the services of a part that ended,
/// refusing every call, and would hold those services' stores and links open under the part
/// that starts next; closed, its client connects again and reaches the new part.
#[derive(Default)]
pub(crate) struct Connections {
    open: Mutex<std::collections::BTreeMap<u64, Stream>>,
    next: std::sync::atomic::AtomicU64,
}

impl Connections {
    /// Keep `stream` to be closed when the part ends, while the answer is held: the connection
    /// is served until then. `None` when it cannot be kept, which is said.
    pub(crate) fn hold(self: &Arc<Self>, stream: &Stream) -> Option<HeldConnection> {
        let kept = match stream.try_clone() {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "a connection to the person's part cannot be closed when the part ends"
                );
                return None;
            }
        };
        let number = self.next.fetch_add(1, Ordering::Relaxed);
        self.open.lock_recover().insert(number, kept);
        Some(HeldConnection(Arc::downgrade(self), number))
    }

    /// Close every connection the part serves: what each is reading ends and what it writes
    /// fails, so its serving ends and lets go of the part's services. The number closed.
    pub(crate) fn end(&self) -> usize {
        let open = std::mem::take(&mut *self.open.lock_recover());
        for stream in open.values() {
            // A shutdown wakes the reader and never waits: a connection already gone is
            // closed already.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        open.len()
    }
}

/// A connection [`Connections::hold`] keeps, until its serving ends.
pub(crate) struct HeldConnection(std::sync::Weak<Connections>, u64);

impl Drop for HeldConnection {
    fn drop(&mut self) {
        if let Some(connections) = self.0.upgrade() {
            connections.open.lock_recover().remove(&self.1);
        }
    }
}

struct ConnectionSlot(Arc<AtomicUsize>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The first line of a native connection, what followed it in the same message, and the
/// descriptors the client sent with it. The first message is received rather than read, so a
/// descriptor that rides it (the window connection a session in a window sends with its
/// `shells#start`) is owned here rather than dropped by the reader.
fn first_line(
    stream: &Stream,
    read: Vec<u8>,
) -> io::Result<(
    Vec<u8>,
    Vec<u8>,
    Vec<runtime_portal::process_link::Descriptor>,
)> {
    let mut line = read;
    let mut descriptors = Vec::new();
    let mut received = false;
    let first_line = usize::try_from(FIRST_LINE).unwrap_or(usize::MAX);
    while !line.contains(&b'\n') && line.len() < first_line {
        let (chunk, fds) = if received {
            (read_some(stream)?, Vec::new())
        } else {
            received = true;
            first_message(stream)?
        };
        if chunk.is_empty() {
            break;
        }
        line.extend_from_slice(&chunk);
        descriptors.extend(fds);
    }
    let rest = match line.iter().position(|byte| *byte == b'\n') {
        Some(at) => line.split_off(at.saturating_add(1)),
        None => Vec::new(),
    };
    Ok((line, rest, descriptors))
}

/// What `stream` holds now, up to a chunk of it.
fn read_some(stream: &Stream) -> io::Result<Vec<u8>> {
    let mut chunk = [0_u8; 16 * 1024];
    let taken = Read::read(&mut &*stream, &mut chunk)?;
    Ok(chunk.get(..taken).unwrap_or_default().to_vec())
}

/// The first message on `stream`, with the descriptors it carries: a Windows pipe carries bytes
/// alone.
fn first_message(
    stream: &Stream,
) -> io::Result<(Vec<u8>, Vec<runtime_portal::process_link::Descriptor>)> {
    #[cfg(unix)]
    {
        runtime_portal::fd_transport::recv_with_fds(stream)
    }
    #[cfg(windows)]
    {
        Ok((read_some(stream)?, Vec::new()))
    }
}

/// Answer one native connection that asks the seats alone ([`Seats::asks`]).
fn serve_seat(
    stream: &Stream,
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
/// MCP requests as [`serve_lines`] does, overlapping one another. The session is admitted by
/// `admit` the first time a request needs one, so a connection that only asks what this device
/// is is answered without one. An `agent`'s connection hears first what [`Presence::answer_first`]
/// answers.
fn serve_connection<H: tilo_actions::remote::ActionProvider + Send + Sync + 'static>(
    stream: &Stream,
    input: impl Read + Send + 'static,
    admit: &dyn Fn() -> io::Result<Arc<McpServer<H>>>,
    status: &Status,
    search: &OnceLock<Arc<tilo_os_search::OsSearch>>,
    seated: Option<&Seated>,
    agent: Option<&Arc<Presence>>,
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
    // The session this connection reaches, admitted by the first request that needs one: what
    // this device is is answered without it, so asking founds no workspace and waits for none.
    let mut session: Option<Arc<McpServer<H>>> = None;
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
        if native == Some("status") && value == json!({"native":"status"}) {
            let status = status.clone();
            return Answer::InOrder(Box::pin(async move { Ok(Some(status)) }));
        }
        let server = if let Some(server) = session.clone() {
            server
        } else {
            match admit() {
                Ok(admitted) => {
                    if let Some(agents) = &agent {
                        agents.visit();
                    }
                    session = Some(Arc::clone(&admitted));
                    admitted
                }
                Err(error) => {
                    // Told why, as an agent whose person revoked its key is.
                    tracing::debug!(%error, "native MCP session admission unavailable");
                    return ending.refusing(&value, error.to_string());
                }
            }
        };
        Answer::InOrder(match native {
            Some("activate") => {
                Box::pin(async move { Ok(Some(answer_activate(&server, &value).await)) })
            }
            Some(tilo_host_search::OPEN) => Box::pin(async move {
                Ok(Some(match search.get() {
                    Some(search) => answer_open(search, &value).await,
                    None => json!({"error": SEARCH_NOT_STARTED}),
                }))
            }),
            _ => {
                if let Some(refused) = ending.answer(&server, &value) {
                    return refused;
                }
                return match &agent {
                    Some(agents) => agents
                        .answer_first(value)
                        .unwrap_or_else(|value| mcp_answer(&server, value)),
                    None => mcp_answer(&server, value),
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

/// What a launcher asks this computer's daemon to do: start an app.
#[cfg(unix)]
const LAUNCH: &str = "launch";

/// How long a second start of an app's default presentation waits for the first to show its
/// window before it is refused.
#[cfg(unix)]
const PRESENTATION_GRACE: Duration = Duration::from_secs(3);

/// `{"native":"launch", ...}`: a person's program asking this computer to start an app.
///
/// It names only the app, the workspace and whether it is the app's default presentation. What
/// runs — the program, its sandbox, its grants and the records it reaches — is the installed
/// package this computer resolves the name to: a program is never one the caller names.
#[cfg(unix)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchRequest {
    native: String,
    app_id: String,
    /// The workspace the caller read from this daemon a moment before.
    workspace: String,
    /// Whether this starts the app's default presentation, which one already shown answers.
    presentation: bool,
}

/// Whether `request` asks this computer to start an app.
#[cfg(unix)]
fn is_launch_request(request: &[u8]) -> bool {
    serde_json::from_slice::<Value>(request)
        .is_ok_and(|request| request.get("native").and_then(Value::as_str) == Some(LAUNCH))
}

/// What answers a launcher's request: it starts the app this computer has installed, on the
/// launch path a window takes, so the launch record exists before the app's connection does, and
/// tells the caller it started.
#[cfg(unix)]
fn launch_take(session: &Arc<Daemon>) -> Arc<LaunchTake> {
    let session = Arc::downgrade(session);
    Arc::new(move |mut stream, request, descriptors| {
        let reply = match start_requested(&session, request, &descriptors) {
            Ok(reply) => reply,
            Err(error) => json!({"error": error.to_string()}),
        };
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        if let Err(error) = writeln!(stream, "{reply}") {
            tracing::debug!(%error, "a launch request's answer");
        }
    })
}

/// Start the app `request` names, as this computer's person asks: the installed package decides
/// what runs, and the launch path a window takes starts it and records the process.
#[cfg(unix)]
fn start_requested(
    session: &std::sync::Weak<Daemon>,
    request: &[u8],
    descriptors: &[runtime_portal::process_link::Descriptor],
) -> io::Result<Value> {
    let request: LaunchRequest = serde_json::from_slice(request)
        .map_err(|error| invalid(format!("the launch request: {error}")))?;
    if request.native != LAUNCH {
        return Err(invalid("this is not a launch request"));
    }
    if request.app_id.trim().is_empty() || request.app_id.len() > 4096 {
        return Err(invalid("invalid application identity"));
    }
    if request.workspace.trim().is_empty() || request.workspace.len() > 4096 {
        return Err(invalid("a launch names a workspace"));
    }
    if !descriptors.is_empty() {
        return Err(invalid(
            "a launch of an app this computer has installed carries no streams",
        ));
    }
    let daemon = session
        .upgrade()
        .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
    // The workspace the app runs in is the one this daemon serves now: a caller that read
    // another a moment ago is told, rather than starting an app into a workspace gone.
    let serving = daemon
        .workspace()
        .map_err(|error| io::Error::other(error.to_string()))?;
    if serving.is_empty() {
        return Err(io::Error::other(
            "this computer serves no workspace to start an app in yet",
        ));
    }
    if serving != request.workspace {
        return Err(io::Error::other(format!(
            "this computer serves {serving} now, not {}; start the app again",
            request.workspace
        )));
    }
    let apps = daemon.apps();
    // What runs is the package this computer has installed: the request's name is a name, never
    // a program.
    let app = apps
        .installed_here(&request.app_id)
        .map_err(io::Error::other)?;
    if !app.has_gui {
        return Err(io::Error::other(format!(
            "{} has no window to open",
            app.app_id
        )));
    }
    // The app's default presentation is one: a second start shows the one running, and while the
    // first has shown nothing the launch's own lock waits for it, as a launch takes it.
    let guard = if request.presentation {
        match default_launch_guard(apps.root(), &app.app_id, PRESENTATION_GRACE)? {
            Some(guard) => Some(guard),
            None => return Ok(json!({"activated": true})),
        }
    } else {
        None
    };
    apps.start_watched_window(&app.app_id, &serving, guard)
        .map_err(io::Error::other)?;
    Ok(json!({"started": true}))
}

/// Ask the daemon at `profile` to start an app, and answer what it said.
#[cfg(unix)]
fn ask_launch(profile: &Path, request: &Value) -> io::Result<Value> {
    let stream = std::os::unix::net::UnixStream::connect(native_socket(profile)?)?;
    stream.set_read_timeout(Some(ANSWERING))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    (&stream).write_all(format!("{request}\n").as_bytes())?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    serde_json::from_str(&answer)
        .map_err(|error| invalid(format!("the launch request's answer, {answer:?}: {error}")))
}

/// Start an app on this computer, through the daemon serving `profile`: the app this computer has
/// installed, started the way a window is. The caller names an app and nothing else: what runs,
/// with which program, sandbox and grants, is the installed package.
///
/// # Errors
/// The daemon does not run, the app is not installed, or the launcher refuses it.
#[cfg(unix)]
pub fn run_app(profile: &Path, app_id: &str, workspace: Option<&str>) -> io::Result<()> {
    let status = running(profile)?;
    let workspace = workspace.unwrap_or(&status.workspace).to_owned();
    if workspace.is_empty() {
        return Err(io::Error::other(
            "this computer serves no workspace to start an app in yet",
        ));
    }
    // Starting the app of the workspace this computer serves is its default presentation: a
    // second start shows the window already running.
    let presentation = workspace == status.workspace;
    let request = json!({
        "native": LAUNCH,
        "app_id": app_id,
        "workspace": workspace,
        "presentation": presentation,
    });
    let answer = ask_launch(profile, &request)?;
    match answer.get("error").and_then(Value::as_str) {
        Some(error) => Err(io::Error::other(error.to_owned())),
        None => Ok(()),
    }
}

/// An app on Windows is handed its services on its process link, by the daemon that starts it;
/// nothing there attaches one by replacing a host process with it.
///
/// # Errors
/// Always [`io::ErrorKind::Unsupported`].
#[cfg(windows)]
pub fn run_app(_profile: &Path, _app_id: &str, _workspace: Option<&str>) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "--run-app attaches a Unix app to the daemon; a Windows app is handed its services by \
         the daemon that starts it",
    ))
}

#[cfg(unix)]
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
#[cfg(unix)]
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
