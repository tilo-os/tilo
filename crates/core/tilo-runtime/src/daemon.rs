//! Persistent native session shared by independently launched GUI apps and MCP.
use crate::device::presence::Presence;
use crate::device::{
    local::Frames,
    services::{Daemon, UNNAMED_AGENT},
};
#[cfg(unix)]
use crate::host;
use crate::seats::{AppAt, Seated, Seats};
use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use std::process::Command;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_api::daemon::admission::{Admitted, ROOT};
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_mcp::lines::{Answer, mcp_answer, serve_lines, spawn_reader};
#[cfg(unix)]
use tilo_mcp::lines::{Frame, read_frame};
use tilo_mcp::session_api::{Asking, bridge_stdio};
use tilo_socket::{Listener, Stream};

use crate::daemon_args::invalid;
pub use crate::daemon_args::{Mode, default_profile, parse_args, validate_profile};

const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// How long a client looks for the daemon of a profile it names apart before saying none serves
/// it. The daemon answers its device's actions from boot and takes a connection as its person
/// once their services start, which it bounds itself at [`WAITED_WITHIN`]: a probe that missed in
/// its short read says nothing about a machine busy with other work, so it is asked again for as
/// long as the daemon gives a caller.
const LOOKING: Duration = WAITED_WITHIN;

/// How long the client waits between looks for its profile's daemon.
const LOOKING_AGAIN: Duration = Duration::from_millis(50);

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
            native: dir.join(tilo_machine::DAEMON_NATIVE),
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
    let named = profile.join(tilo_machine::DAEMON_NATIVE);
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
    service_sockets_at(&own, serves_the_computer(profile), account)
}

/// Whether the daemon serving `profile` is the computer's: Tilo OS's system unit, serving every
/// account of it. A daemon started over any other profile, there too, is one account's.
fn serves_the_computer(profile: &Path) -> bool {
    tilo_machine::serves_the_computer(tilo_api::daemon::system_daemon(), Some(profile))
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

/// The account a workspace VM's tenant runs as: what the VM's keeper names this daemon's tenant
/// socket with.
pub const TENANT_UID_ENV: &str = "TILO_VM_TENANT_UID";
/// The directory the daemon serves its tenant's socket in: made by the VM's keeper for the
/// tenant's group, which is the first gate, and what a client of the daemon whose sockets are in
/// a named profile looks in ([`tilo_machine::DAEMON_PROFILE_VAR`]).
pub const TENANT_SOCKETS_ENV: &str = "TILO_VM_TENANT_SOCKETS";

/// The tenant a workspace VM's keeper hands this daemon: the account its socket serves, and the
/// directory the socket is in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Tenant {
    pub(crate) uid: u32,
    pub(crate) sockets: PathBuf,
}

impl Tenant {
    /// The tenant's native endpoint, in its directory.
    fn native(&self) -> PathBuf {
        self.sockets.join(tilo_machine::DAEMON_NATIVE)
    }
}

/// The tenant this daemon serves a socket for, as its VM's keeper named it; `None` when it named
/// none, and on a computer that runs no workspace VM.
#[cfg(unix)]
fn tenant_of_environment() -> io::Result<Option<Tenant>> {
    let Some(sockets) = std::env::var_os(TENANT_SOCKETS_ENV) else {
        return Ok(None);
    };
    let named = |error: std::env::VarError| invalid(format!("{TENANT_UID_ENV}: {error}"));
    let uid = std::env::var(TENANT_UID_ENV)
        .map_err(named)?
        .trim()
        .parse()
        .map_err(|_| invalid(format!("{TENANT_UID_ENV} is not an account's number")))?;
    let sockets = PathBuf::from(sockets);
    if !sockets.is_absolute() {
        return Err(invalid(format!(
            "{TENANT_SOCKETS_ENV} is not an absolute path"
        )));
    }
    tilo_api::daemon::socket_fits(&sockets.join(tilo_machine::DAEMON_NATIVE))
        .map_err(io::Error::other)?;
    Ok(Some(Tenant { uid, sockets }))
}

/// A workspace VM's tenant socket is a Unix account's: a computer with none serves no tenant.
#[cfg(not(unix))]
fn tenant_of_environment() -> io::Result<Option<Tenant>> {
    Ok(None)
}

/// Bind the tenant's native endpoint in its directory, which the VM's keeper made for the
/// tenant's group: the socket is that group's and `0660`, so only the account the kernel names
/// on each connection ever reaches a place where the daemon can refuse it.
#[cfg(unix)]
fn bind_tenant(tenant: &Tenant) -> io::Result<Listener> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
    let path = tenant.native();
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Ok(_) => match std::fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let group = std::fs::metadata(&tenant.sockets)?.gid();
    let listener = tilo_socket::bind(&path)?;
    std::os::unix::fs::chown(&path, None, Some(group))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

/// A workspace VM's tenant is a Unix account's: a computer with none serves no tenant socket.
#[cfg(not(unix))]
fn bind_tenant(_: &Tenant) -> io::Result<Listener> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a tenant's socket is a Unix account's",
    ))
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

/// [`probe`] of `socket` asked again until it answers or `within` passes: an answer a busy
/// machine has not written yet is asked for again rather than taken for no daemon.
fn answered_within(socket: &Path, within: Duration) -> io::Result<Status> {
    let deadline = Instant::now()
        .checked_add(within)
        .unwrap_or_else(Instant::now);
    loop {
        match probe(socket) {
            Ok(status) => return Ok(status),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(LOOKING_AGAIN),
        }
    }
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

/// The daemon serving `profile`, once its native endpoint answers: looked for as long as the
/// daemon itself gives a caller ([`LOOKING`]), and, when it does not answer, after the one start
/// ([`tilo_api::daemon::service::start`]) for a profile that is this computer's. A profile named
/// apart (a test's, a VM host's) is served by whoever started its daemon, and is only asked. Its
/// status names the person's workspace once there is one: at boot, before anything founded it or
/// their desktop handed it over, it names none.
fn running(profile: &Path) -> io::Result<Status> {
    validate_profile(profile)?;
    let socket = native_socket(profile)?;
    if let Ok(status) = answered_within(&socket, LOOKING) {
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
    /// The lock on the profile, held for as long as this daemon serves it, which says who serves
    /// a profile named apart while the daemon keeps itself undumpable.
    _profile: advisory_lock::FileLock,
    stop: crate::device::serve::Stop,
    /// The native endpoint, which takes no connection once it is closing.
    endpoint: Arc<Native>,
    accepting: Option<std::thread::JoinHandle<()>>,
    /// A workspace VM's tenant socket, and the thread taking its connections.
    tenant: Option<PathBuf>,
    tenant_accepting: Option<std::thread::JoinHandle<()>>,
    serving: Option<std::thread::JoinHandle<io::Result<()>>>,
    /// The person's services while they run: what starts their shell.
    person: Arc<Person>,
}

/// The person's services, while they run, for whoever waits for them to.
#[derive(Default)]
struct Person {
    services: Mutex<Starting>,
    started: Condvar,
}

/// The person's services while they run, or why they will not.
#[derive(Default)]
struct Starting {
    services: std::sync::Weak<Daemon>,
    failed: Option<String>,
}

impl Person {
    /// `services` run from now on, or (`None`) no longer.
    fn set(&self, services: Option<&Arc<Daemon>>) {
        *self.services.lock_recover() = Starting {
            services: services.map_or_else(std::sync::Weak::new, Arc::downgrade),
            failed: None,
        };
        self.started.notify_all();
    }

    /// The person's services did not start, as `why` says: whoever waits for them is told so
    /// now, rather than when its wait runs out.
    fn failed(&self, why: String) {
        *self.services.lock_recover() = Starting {
            services: std::sync::Weak::new(),
            failed: Some(why),
        };
        self.started.notify_all();
    }

    /// The person's services once they run, waiting up to `within` for them to start.
    fn within(&self, within: Duration) -> io::Result<Arc<Daemon>> {
        let deadline = Instant::now()
            .checked_add(within)
            .unwrap_or_else(Instant::now);
        let mut services = self.services.lock_recover();
        loop {
            if let Some(services) = services.services.upgrade() {
                return Ok(services);
            }
            if let Some(why) = &services.failed {
                return Err(io::Error::other(why.clone()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("they were still starting after {} s", within.as_secs()),
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
        Self::start_serving_tenant(profile, dev, stop, tenant_of_environment()?.as_ref())
    }

    /// [`Self::start_serving`] with the tenant socket its VM's keeper named, if it named one.
    fn start_serving_tenant(
        profile: &Path,
        dev: bool,
        stop: crate::device::serve::Stop,
        tenant: Option<&Tenant>,
    ) -> io::Result<Self> {
        validate_profile(profile)?;
        // A profile is one daemon's: this holds it for as long as the daemon serves it, and the
        // pid it records there is how a launcher of the daemon finds it, which a daemon keeping
        // this computer's device key says no other way of itself.
        let profile_lock = crate::profile::lock(profile)?;
        let device = crate::device::unclaimed::ThisDevice::lock(profile.to_path_buf())?;
        // Who each connection is: root and this daemon's own account act as the device, and
        // under the system unit the account that made the device's person, kept in the profile,
        // and the people signed in act as themselves.
        tilo_api::daemon::admission::install(
            tilo_api::daemon::admission::Admission::new(
                crate::seats::own_account(),
                serves_the_computer(profile),
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
        if serves_the_computer(profile) {
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
        // The socket a workspace VM's tenant reaches: its keeper made the directory for the
        // tenant's group, and every connection is admitted by the account the kernel names.
        let (tenant_socket, tenant_accepting) = match tenant {
            Some(tenant) => {
                let listener = bind_tenant(tenant)?;
                let serving = TenantServing::new(tenant.uid);
                native.serve_tenant_socket(Arc::clone(&serving));
                let accepting = {
                    let serving = Arc::clone(&serving);
                    std::thread::Builder::new()
                        .name("tilo-tenant".into())
                        .spawn(move || {
                            if let Err(error) = serving.accept(&listener) {
                                tracing::debug!(%error, "the tenant's endpoint stopped taking connections");
                            }
                        })?
                };
                tracing::info!("answering the workspace's tenant at its own endpoint");
                (Some(tenant.native()), Some(accepting))
            }
            None => (None, None),
        };
        let person = Arc::new(Person::default());
        let serving = {
            let (profile, stop, person) = (profile.to_owned(), stop.clone(), Arc::clone(&person));
            std::thread::Builder::new()
                .name("tilo-daemon".into())
                .spawn(move || {
                    let served = serve_device(&profile, dev, &device, &native, &stop, &person);
                    // A daemon that ended this way serves no person: whoever waits for their
                    // services is told why at once.
                    if let Err(error) = &served {
                        tracing::error!(%error, "this computer's daemon ended");
                        person.failed(error.to_string());
                    }
                    served
                })?
        };
        Ok(Self {
            native: path,
            _profile: profile_lock,
            stop,
            endpoint,
            accepting: Some(accepting),
            tenant: tenant_socket,
            tenant_accepting,
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

    /// A link to the daemon's dispatch as its person, on no seat, once the person's services run
    /// (within `within`): what a test's own program reads and runs the person's calls over beside
    /// the session's link ([`Self::link_shell`]), which holds this process's seat. It hands the
    /// device no process, as only a seat's shell does.
    ///
    /// # Errors
    /// The person's services did not start in time, or the link could not be made.
    #[cfg(feature = "test-support")]
    pub fn link_person(
        &self,
        within: Duration,
    ) -> io::Result<runtime_portal::process_link::Descriptor> {
        let services = self.person.within(within)?;
        let secret = services.shell_token();
        Ok(runtime_portal::packet::descriptor_of(
            services.link_with(&secret)?,
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
        // The tenant's socket closes with the daemon's, and no connection it takes serves the
        // device's own actions in the person's place.
        if let Some(tenant) = self.endpoint.tenant.get() {
            tenant.closing.store(true, Ordering::Relaxed);
        }
        if let Some(socket) = &self.tenant {
            drop(tilo_socket::connect(socket));
        }
        if let Some(accepting) = self.tenant_accepting.take() {
            let _ = accepting.join();
        }
        if let Some(socket) = &self.tenant {
            let _ = std::fs::remove_file(socket);
        }
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
#[expect(
    clippy::too_many_lines,
    reason = "one daemon's life read top to bottom: each part, its end, and the endpoint handed on"
)]
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
    // A deployment may hand this daemon its owner's root instead of a ticket: the root is kept
    // here as on any of its person's own devices, which certifies this device's key and makes
    // this computer one its person's services start as, founding the workspaces it keeps.
    crate::device::services::adopt_owner_root(profile)?;
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
    // The endpoint the part that ends hands to the part that starts: this device is reached at
    // one port for as long as the daemon runs, whichever server its person's parts enter through.
    let mut handed: Option<iroh::Endpoint> = None;
    let _lock = device.device_lock();
    loop {
        // A stop heard while this computer waited for its person ends the daemon without
        // starting their services.
        if stop.told() {
            return Ok(());
        }
        stop.waiting_on("the wait between its person's parts");
        hold_between_parts(stop);
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
            &founding,
            handed.take(),
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
            Err(error) => {
                // The person's services did not start, and this daemon ends: a shell that asked
                // for its link is answered why, rather than waiting for a part that will not
                // come, so the screen that says Tilo did not start says what did it.
                native.seats.did_not_start(format!(
                    "this computer's person's services did not start: {error}"
                ));
                return Err(error);
            }
        };
        drop(watched);
        tracing::info!("this daemon's person's part started");
        // A login the device's own actions answer while this part runs is taken in by it, rather
        // than at its next start.
        device.serving(&session);
        serve_the_started(&session, native, person, stop)?;
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
        device.stopped_serving();
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
        let held = held.map_err(|error| io::Error::other(format!("{error:#}")))?;
        // The endpoint outlives the part that served on it: the part that starts next takes it
        // as it is, so the port this device is reached at is never bound a second time, and no
        // cleanup behind the part that ended can keep it.
        let restarting = matches!(held, crate::device::Ended::LoggedIn(_));
        if restarting {
            handed = Some(session.serving().endpoint());
        } else {
            // Ending for good: its endpoint goes first, and is awaited here, so the address it
            // bound is free before what the part still has to let go of runs.
            let endpoint = Arc::clone(session.serving());
            crate::device::serve::ending_step(
                Some(stop),
                "the person's part's endpoint",
                CLOSING_ENDPOINT,
                move || {
                    crate::device::workspaces::block_on_owned(async move {
                        endpoint.close_endpoint().await;
                    });
                },
            );
        }
        // Its workspace service stops answering before the next part opens the same stores and
        // enters as the same device: what it held is already written through, every change kept
        // before the call that made it was answered. The step is bounded from inside, so it
        // always ends and lets go of the service it ran.
        let workspaces = Arc::clone(session.serving());
        let left = Arc::downgrade(&session);
        crate::device::serve::ending_step(
            Some(stop),
            "the person's part's workspace service",
            CLOSING_WORKSPACES,
            move || {
                crate::device::workspaces::block_on_owned(async move {
                    let busy = closing_busy();
                    let closing = async move {
                        if let Some(busy) = busy {
                            tokio::time::sleep(busy).await;
                        }
                        workspaces.shutdown_service().await;
                    };
                    if tokio::time::timeout(CLOSING_WORKSPACES, closing)
                        .await
                        .is_err()
                    {
                        tracing::error!(
                            waited_ms = CLOSING_WORKSPACES.as_millis(),
                            "the person's part's workspace service did not close in time; it is \
                             left behind"
                        );
                    }
                });
            },
        );
        // What the part kept lets go here, on this thread. A stop is heard on a thread of its
        // own, so a step that will not return cannot keep the daemon running.
        drop(session);
        match held {
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

/// In a build that is not a release, a test can hold the daemon between its person's parts, where
/// a step of the part that ended does not return: what a folder that will not unmount does. The
/// stop must still end the daemon there, so it is the stop's own wait, never a part's.
#[cfg(debug_assertions)]
fn hold_between_parts(stop: &crate::device::serve::Stop) {
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

/// A release build holds nothing between its person's parts.
#[cfg(not(debug_assertions))]
fn hold_between_parts(_: &crate::device::serve::Stop) {}

/// In a build that is not a release, a test keeps the closing work of the person's part busy
/// past any bound the ending waits on, as a folder that will not unmount does: the part that
/// starts next must still take the port the part that ended bound.
#[cfg(debug_assertions)]
fn closing_busy() -> Option<Duration> {
    let ms = std::env::var_os("TILO_TEST_BUSY_CLEANUP_MS")
        .and_then(|ms| ms.to_str().and_then(|ms| ms.parse::<u64>().ok()))?;
    tracing::info!(ms, "a test keeps the person's part cleanup busy");
    Some(Duration::from_millis(ms))
}

/// A release build closes with nothing kept busy.
#[cfg(not(debug_assertions))]
fn closing_busy() -> Option<Duration> {
    None
}

/// Serve the part that started, from this daemon's own native endpoint: what its apps run and kept
/// from the move, the apps themselves, the person's shells, their services and their keys, and the
/// search over their workspace once the device is held.
fn serve_the_started(
    session: &Arc<Daemon>,
    native: &Native,
    person: &Person,
    stop: &crate::device::serve::Stop,
) -> io::Result<()> {
    // The origin connectors this device starts reach the one dispatch on the connection
    // handed them, admitted as the app the launch record says they run.
    session.hand_actions_to_connectors();
    // The person's own programs reach Tilo's API at their login's socket, through their shell.
    session.serve_programs_api();
    // Migrations run on the device that holds the data they read.
    session.serve_migrations();
    // The secrets the apps the move carried over kept in this device's old keeper are the
    // person's vault items from now on: walked once per start, as the person, and taken out
    // of the keeper in the same step.
    session.adopt_kept_secrets();
    // A computer that just logged in takes its workspace in while the device is held: nothing is
    // admitted to the workspace before then. The endpoint admits each connection as it comes, and
    // host search starts once the workspace is here, rather than the daemon ending before it could
    // take the workspace in.
    // The person's and their agents' task programs run here, in this daemon's processes.
    #[cfg(target_os = "linux")]
    session.serve_task_programs();
    let search = Arc::new(OnceLock::new());
    search_once_held(session, &search)?;
    // A person who signs in from now on has their workspace here (the one their desktop hands
    // over, or one founded for them now) and a link their shell is admitted by.
    native.seats.follow_launches(app_of(session));
    native
        .seats
        .follow_shells(record_shell(session), shell_lines(session));
    native.serve_apps(app_take(session));
    #[cfg(unix)]
    session.hand_apps_their_lines();
    native
        .seats
        .serve(Arc::new(PersonShells(Arc::downgrade(session))));
    // A workspace VM's tenant reaches the same workspace as the person's unnamed agent, at the
    // socket its keeper made for it alone.
    if let Some(tenant) = native.tenant.get() {
        tenant.serve(the_tenant(session));
    }
    native.serve_the_person(the_persons(session, search));
    native.serve_keys(key_endpoint(session));
    // A workspace VM's daemon runs the VM's sessions as its shells.
    #[cfg(target_os = "linux")]
    crate::device::vm_environment::serve(session, stop);
    #[cfg(not(target_os = "linux"))]
    let _ = stop;
    person.set(Some(session));
    Ok(())
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
    fn token(&self, _importing: bool) -> io::Result<[u8; 32]> {
        // The person's workspace was founded with their root, or comes from a device of theirs
        // that has it: a sign-in founds nothing.
        Ok(self.daemon()?.shell_token())
    }

    fn token_holder(&self, token: &[u8; 32]) -> Option<[u8; 32]> {
        self.daemon().ok()?.shell_token_holder(token)
    }

    fn redeem(&self, secret: [u8; 32], seat: u64) -> io::Result<Stream> {
        self.daemon()?.shell_redeem(secret, seat)
    }

    fn link_for(&self, person: [u8; 32], seat: u64) -> io::Result<Stream> {
        let daemon = self.daemon()?;
        let secret = daemon.shell_token_for(person, seat)?;
        daemon.shell_link_of(secret)
    }

    fn live(&self, seat: u64) -> bool {
        self.daemon().is_ok_and(|daemon| daemon.shell_live(seat))
    }

    fn serves(&self, person: [u8; 32]) -> bool {
        self.daemon().is_ok_and(|daemon| daemon.serves(person))
    }

    fn hand_processes(
        &self,
        uid: u32,
        secret: &[u8; 32],
        packets: runtime_portal::process_link::Descriptor,
    ) -> Result<(), String> {
        let daemon = self.daemon().map_err(|error| error.to_string())?;
        daemon.hand_processes(uid, secret, packets)
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
        daemon.start_shell(command, window.as_ref())
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
                && name != runtime_collab::wire::shell::SHELL_TOKEN_ENV)
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
        launch: Some(launch_take(session, Asks::Person)),
        #[cfg(not(unix))]
        launch: None,
        connections: Some(Arc::clone(session.connections())),
        acting: None,
    }
}

/// How long the endpoint of a person's part that ended may take to close before the next part
/// starts without waiting for it: the port it bound is the one the next part binds.
const CLOSING_ENDPOINT: Duration = Duration::from_secs(5);

/// How long the workspace service of a person's part that ended may take to save what it took
/// in and close before the next part starts without waiting for it.
const CLOSING_WORKSPACES: Duration = Duration::from_secs(10);

/// What records a shell the device's shell runtime started for an app, as that app's.
fn record_shell(session: &Arc<Daemon>) -> crate::seats::RecordShell {
    let session = Arc::downgrade(session);
    Arc::new(move |pid, started, app, workspace| {
        session
            .upgrade()
            .ok_or_else(|| "this computer's daemon is stopping".to_owned())?
            .record_shell(pid, started, app, workspace)
    })
}

/// What makes a connection of an app's shell, served as that app in its workspace exactly as the
/// native endpoint serves a process the daemon started for it ([`Frames::of_an_app`]): whatever
/// process holds it acts as the app, so a shell's programs reach the daemon as Terminal even where
/// the daemon cannot see their processes. It stands for no process the daemon started: nothing is
/// released to it that is kept for one ([`crate::device::vault_release`]).
fn shell_lines(session: &Arc<Daemon>) -> crate::seats::ShellLines {
    let session = Arc::downgrade(session);
    Arc::new(move |app: &str, workspace: &str| {
        let session = session
            .upgrade()
            .ok_or_else(|| "this computer's daemon is stopping".to_owned())?;
        let workspace = session.shell_workspace(workspace)?;
        let failed = |error: io::Error| format!("no connection was made for the shell: {error}");
        let (ours, theirs) = tilo_socket::pair().map_err(failed)?;
        let at = AppAt {
            app: app.to_owned(),
            workspace: workspace.clone(),
            pid: 0,
        };
        // Closed when the person's part ends, as every connection it serves is.
        let held = session.connections().hold(&ours);
        serve_as_app(ours, Frames::of_an_app(&session, &at), None, held).map_err(failed)?;
        Ok((workspace, theirs))
    })
}

/// What finds the app a process the daemon started runs, from the launch records `session`
/// keeps, as the process connects: the app has connected once it reaches the native endpoint.
fn app_of(session: &Arc<Daemon>) -> crate::seats::AppOf {
    let session = Arc::downgrade(session);
    Arc::new(move |pid| session.upgrade()?.app_connected(pid))
}

/// What answers a connection from a process the daemon started, as the app it runs.
fn app_take(session: &Arc<Daemon>) -> AppTake {
    // The launch an app asks on this connection: it reaches the same launch a person's program
    // asks of the daemon, which starts the installed package and records the instance.
    #[cfg(unix)]
    let launch = Some(launch_take(session, Asks::App));
    #[cfg(not(unix))]
    let launch = None;
    let session = Arc::downgrade(session);
    Box::new(move |stream, slot, seated, app| {
        let session = session
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        serve_app(stream, slot, seated, &session, &app, launch.clone())
    })
}

/// Answer one connection from an app the daemon started, as that app in its workspace: the
/// action wire, decided by the workspace's broker with the app's grants, and the launch it asks
/// for another instance of itself. Anything else an app asks, its MCP projection above all, is
/// refused before its first call.
fn serve_app(
    stream: Stream,
    slot: ConnectionSlot,
    _seated: Seated,
    session: &Arc<Daemon>,
    app: &AppAt,
    launch: Option<Arc<LaunchTake>>,
) -> io::Result<()> {
    // Closed when the person's part ends, as every connection it serves is.
    let held = session.connections().hold(&stream);
    serve_as_app(
        stream,
        Frames::of_an_app(session, app),
        launch,
        (slot, held),
    )
}

/// Answer `stream` with `frames`, an app's own connection, on a thread of its own while it lasts,
/// holding `held` until it ends: the action wire, its first line a frame, or the launch `launch`
/// makes for the app.
fn serve_as_app(
    stream: Stream,
    frames: Frames,
    launch: Option<Arc<LaunchTake>>,
    held: impl Send + 'static,
) -> io::Result<()> {
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
            if let Some(launch) =
                launch.filter(|_| runtime_portal::native::LaunchRequest::is_launch(&line))
            {
                launch(stream, &line, Vec::new());
                return;
            }
            if !crate::device::local::is_frame(&line) {
                // An app reaches the device's actions alone, never its MCP projection.
                crate::device::local::refuse(
                    stream,
                    "an app's connection speaks the action wire alone",
                );
                return;
            }
            frames.serve(stream, &line, reader, None);
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
        serve_as_app(ours, Frames::of_an_app(&session, &at), None, held)?;
        Ok(runtime_portal::packet::descriptor_of(theirs))
    })
}

#[cfg(unix)]
impl Daemon {
    /// Hand each app's own tilolang process started from now on its line to this daemon: the
    /// one path its Rust host has here, which carries the shells it opens, the secrets it keeps
    /// and the key the device holds for it. Every daemon that starts the person's apps does this
    /// once, as soon as it is shared.
    pub fn hand_apps_their_lines(self: &Arc<Self>) {
        self.apps().serve_own_lines(own_lines(self));
        let daemon = Arc::downgrade(self);
        self.workspaces()
            .start_apps_with(Arc::new(move |app, workspace, record| {
                use tilo_app_host::services::LauncherServices as _;
                let started = daemon
                    .upgrade()
                    .map(|daemon| daemon.apps().windowless(app, workspace, Some(record)));
                let app = app.to_owned();
                Box::pin(async move {
                    match started {
                        None => Err("this computer's daemon is stopping".to_owned()),
                        Some(Err(error)) => Err(error),
                        Some(Ok(None)) => Err(format!("{app} does not start with no window here")),
                        Some(Ok(Some(started))) => started
                            .await
                            .map_err(|_| format!("{app} stopped before it started"))?,
                    }
                })
            }));
    }
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
    native
        .seats
        .follow_shells(record_shell(session), shell_lines(session));
    native.serve_apps(app_take(session));
    native.serve_the_person(the_persons(session, Arc::new(OnceLock::new())));
    native.serve_keys(key_endpoint(session));
    let listener = tilo_files::native::private::bind_socket(socket)?;
    std::thread::Builder::new()
        .name("tilo-native-test".into())
        .spawn(move || native.accept(&listener))?;
    Ok(())
}

/// Serve `session`'s tenant endpoint at the socket `tenant` names, admitting only its account, as
/// the daemon does for the socket its VM's keeper names: what a test of the tenant's connection
/// reaches.
#[cfg(all(test, unix))]
pub(crate) fn serve_tenant_for_test(session: &Arc<Daemon>, tenant: &Tenant) -> io::Result<PathBuf> {
    let serving = TenantServing::new(tenant.uid);
    serving.serve(the_tenant(session));
    let listener = bind_tenant(tenant)?;
    std::thread::Builder::new()
        .name("tilo-native-tenant-test".into())
        .spawn(move || serving.accept(&listener))?;
    Ok(tenant.native())
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
pub(crate) struct Native {
    answers: crate::device::unclaimed::DeviceOnly,
    /// What this device's own answers keep of a founding: told when the endpoint serves the
    /// person below, which is when their services run.
    founding: Arc<crate::device::unclaimed::Founding>,
    /// Who each connection is, and the device's answers about it.
    pub(crate) seats: Arc<Seats>,
    /// The person's endpoint, while their services run.
    person: Mutex<Option<Arc<Take>>>,
    /// What answers a connection from an app the daemon started, while the person's services
    /// run: the app's action frames alone, never the person's MCP projection.
    apps: Mutex<Option<Arc<AppTake>>>,
    /// The endpoint of the other people signed in on the computer, each as their own key, while
    /// the person's services run.
    keys: Mutex<Option<Arc<KeyTake>>>,
    /// The socket a workspace VM's tenant reaches this daemon at, when its keeper named one.
    tenant: OnceLock<Arc<TenantServing>>,
    /// Told when [`Self::person`] is set.
    started: (Mutex<()>, Condvar),
    /// Set as the daemon stops: the endpoint takes no more connections.
    closing: AtomicBool,
}

impl Native {
    /// The endpoint of `device`, reading each connection's account with `peer_of` and admitting
    /// it by `admission`.
    pub(crate) fn new(
        device: crate::device::unclaimed::DeviceOnly,
        peer_of: crate::seats::PeerOf,
        admission: &'static tilo_api::daemon::admission::Admission,
    ) -> Self {
        let seats = Arc::new(Seats::new(admission, device.clone(), peer_of));
        let founding = device.founding_record();
        Self {
            answers: device,
            founding,
            seats,
            started: (Mutex::new(()), Condvar::new()),
            person: Mutex::new(None),
            apps: Mutex::new(None),
            keys: Mutex::new(None),
            tenant: OnceLock::new(),
            closing: AtomicBool::new(false),
        }
    }

    /// Serve the tenant's socket with `serving` from now on, when this daemon has one.
    fn serve_tenant_socket(&self, serving: Arc<TenantServing>) {
        let _ = self.tenant.set(serving);
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
            acting: None,
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
        // The person's services run from here: a founding call answered once they do, and the
        // seats' link that waited for them, are answered now.
        self.founding.person_serves();
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
        if let Some(tenant) = self.tenant.get() {
            tenant.forget();
        }
    }

    /// The person's endpoint, while their services run.
    fn person(&self) -> Option<Arc<Take>> {
        self.person.lock_recover().clone()
    }

    /// Serve each connection to `listener` as whatever answers when it comes ([`accept`]): an
    /// account the daemon does not serve is answered the device's own actions alone.
    pub(crate) fn accept(self: &Arc<Self>, listener: &Listener) -> io::Result<()> {
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
        // The session a sign-in answered a founding link reaches the device's own actions about
        // the person it makes or joins, and nothing of a person's.
        if !seated.seat.admitted.served()
            || matches!(seated.seat.admitted, Admitted::Founding { .. })
            || (seated.seat.uid == ROOT && self.seats.system())
        {
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
        // The session a sign-in answered a founding link reaches the device's setup actions
        // about its person, whatever it acts as now: making one, joining one, what the device is.
        if seated.seat.founding && asks_the_founding(&stream) {
            return self.device_only(stream, slot, seated);
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
            // What the device key does is answered by the device's own actions, from the key the
            // daemon took: the person's services do not answer it. The first line is looked at
            // on a thread of the connection's own, so the listener never waits on it.
            Some(person) => {
                let native = Arc::clone(self);
                std::thread::Builder::new()
                    .name("tilo-native-open".into())
                    .spawn(move || native.open_with_person(&person, stream, slot, seated))
                    .map(drop)
            }
            // A founding answered at once makes the person after its answer: what comes
            // meanwhile waits for their part as it would once they are made.
            None if !self.answers.claimed() && !self.answers.founding() => {
                self.device_only(stream, slot, seated)
            }
            None => {
                let native = Arc::clone(self);
                std::thread::Builder::new()
                    .name("tilo-native-wait".into())
                    .spawn(move || native.wait_for_the_person(stream, slot, seated))
                    .map(drop)
            }
        }
    }

    /// Serve `stream` as the person's, unless its first line asks what the device key does: the
    /// device's own actions answer that. Runs on a thread of the connection's own, so the listener
    /// never waits on its first line.
    fn open_with_person(
        self: &Arc<Self>,
        person: &Arc<Take>,
        stream: Stream,
        slot: ConnectionSlot,
        seated: Seated,
    ) {
        let served = if asks_the_device_key(&stream)
            || (seated.seat.founding && asks_the_founding(&stream))
        {
            self.device_only(stream, slot, seated)
        } else {
            person(stream, slot, seated)
        };
        if let Err(error) = served {
            tracing::debug!(%error, "a native connection was not served");
        }
    }

    /// Serve `stream`, which came while the person's services start: at once when it asks the
    /// seats, follows a call, asks the device key, or asks the device's own package cache, and
    /// otherwise once the person's part takes it, within [`WAITED_WITHIN`], or with why it is not
    /// served.
    fn wait_for_the_person(self: &Arc<Self>, stream: Stream, slot: ConnectionSlot, seated: Seated) {
        // A caller following a call this device already took reads its stage
        // where the device keeps it: answered here, not once the person's part
        // starts. The call's progress is what a founding waits on.
        // What the device key does is answered at once too, from the key the
        // daemon took: never after the person's services start or a workspace is
        // admitted, which a signature does not depend on.
        // The device's own package cache answers its actions the same way on the account's own
        // daemon: an install of a development build, a runtime bundle or an image is the
        // device's, and waits for no workspace of the person's. A system daemon serves every
        // account's person, and admits those changes only as the person they are.
        let personal = !tilo_api::daemon::system_daemon();
        // A connection that asks the seats is answered by them, never held behind the person's
        // part: `account#who` is the device's own answer, and the link is the one request the
        // seats hold themselves until their services run. The part's start waits out what the
        // network answers — a relay's net report among it — so a `who` that waited for it was
        // answered far later than the session asking it could wait.
        if asks_the_seats(&stream) {
            serve_a_seat(&stream, &seated);
            return;
        }
        if follows_a_call(&stream)
            || asks_the_device_key(&stream)
            || (personal && asks_the_devices_own_actions(&stream))
        {
            if let Err(error) = self.device_only(stream, slot, seated) {
                tracing::debug!(
                    %error,
                    "a native connection following a call was not served"
                );
            }
            return;
        }
        let deadline = Instant::now()
            .checked_add(WAITED_WITHIN)
            .unwrap_or_else(Instant::now);
        let mut told = self.started.0.lock_recover();
        let person = loop {
            if let Some(person) = self.person() {
                break Some(person);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break None;
            }
            told = self
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
            // The request is read before the connection is answered: closing it
            // with the request unread sends the client a reset, and it reads
            // that instead of why it is not served.
            let (first, rest, descriptors) = match first_line(&stream, Vec::new()) {
                Ok(read) => read,
                Err(error) => {
                    tracing::debug!(%error, "a native connection was not read");
                    return;
                }
            };
            let line = first.strip_suffix(b"\n").unwrap_or(&first);
            // A session asking for its shell is served by the seats, which wait
            // for the person's services themselves: its shell starts once they
            // run, rather than the start being refused a few seconds in.
            if serde_json::from_slice::<Value>(line).is_ok_and(|request| Seats::asks(&request)) {
                serve_the_read_seat(&stream, &seated, first, rest, descriptors);
                return;
            }
            crate::device::serve::refuse(
                &stream,
                "Tilo on this computer isn't ready for its person yet: run `tilo status` \
                 to see how it stands, then try again",
            );
            return;
        };
        if let Err(error) = person(stream, slot, seated) {
            tracing::debug!(%error, "a native connection that waited while the person's services started was not served");
        }
    }
}

/// How long a connection waits for the person's part to take it before it is told why it is not
/// served. The agents' endpoint bounds its own handoff by the same name. A caller is told within
/// seconds: a person's part that is not up is said, never waited on for as long as it is broken.
/// A session asking for its shell is the seats' to answer, and its shell starts once the person's
/// services run.
pub(crate) const WAITED_WITHIN: Duration = Duration::from_secs(30);

/// How long a connection's session is made within before it is told why it was not: a daemon whose
/// person's part cannot be made answers a caller at once, never holding the connection.
const ADMIT_WITHIN: Duration = Duration::from_secs(30);

/// What serves a workspace VM's tenant at the socket its keeper made: one account, the person's
/// unnamed agent, and every other account refused by name.
struct TenantServing {
    /// The account the socket serves: the VM's tenant, whose group is the first gate and whose
    /// uid the kernel names on each connection.
    uid: u32,
    /// The endpoint the tenant is served with, while the person's services run.
    endpoint: Mutex<Option<Endpoint<<Daemon as RuntimeServices>::ActionProvider>>>,
    /// Told when [`Self::endpoint`] is set.
    started: (Mutex<()>, Condvar),
    /// Set as the daemon stops: the socket takes no more connections.
    closing: AtomicBool,
}

impl TenantServing {
    fn new(uid: u32) -> Arc<Self> {
        Arc::new(Self {
            uid,
            endpoint: Mutex::new(None),
            started: (Mutex::new(()), Condvar::new()),
            closing: AtomicBool::new(false),
        })
    }

    /// Serve every connection from now on with `endpoint`, the person's unnamed agent.
    fn serve(&self, endpoint: Endpoint<<Daemon as RuntimeServices>::ActionProvider>) {
        *self.endpoint.lock_recover() = Some(endpoint);
        let _told = self.started.0.lock_recover();
        self.started.1.notify_all();
    }

    /// Serve no connection from now on, while the person's services start again.
    fn forget(&self) {
        self.endpoint.lock_recover().take();
    }

    fn endpoint(&self) -> Option<Endpoint<<Daemon as RuntimeServices>::ActionProvider>> {
        self.endpoint.lock_recover().clone()
    }

    /// Take each connection: the tenant's account is served once the person's services run, as
    /// the person's unnamed agent, and every other account is told this socket is not theirs.
    pub(crate) fn accept(self: &Arc<Self>, listener: &Listener) -> io::Result<()> {
        accept(listener, |stream, slot| {
            if self.closing.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "the tenant's endpoint closed",
                ));
            }
            let peer = match crate::seats::Peer::of(&stream) {
                Ok(peer) => peer.uid,
                Err(error) => {
                    tracing::debug!(%error, "a tenant connection's account is not known");
                    refuse_tenant(&stream, self.uid, None);
                    return Ok(());
                }
            };
            if peer != self.uid {
                refuse_tenant(&stream, self.uid, Some(peer));
                return Ok(());
            }
            self.take(stream, slot);
            Ok(())
        })
    }

    /// Serve `stream` once the person's services run: one that comes while they start waits for
    /// them, as the native endpoint's own connections do.
    fn take(self: &Arc<Self>, stream: Stream, slot: ConnectionSlot) {
        let serving = Arc::clone(self);
        let served = std::thread::Builder::new()
            .name("tilo-native-tenant".into())
            .spawn(move || {
                let deadline = Instant::now()
                    .checked_add(WAITED_WITHIN)
                    .unwrap_or_else(Instant::now);
                let mut told = serving.started.0.lock_recover();
                let endpoint = loop {
                    if let Some(endpoint) = serving.endpoint() {
                        break Some(endpoint);
                    }
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break None;
                    }
                    told = serving
                        .started
                        .1
                        .wait_timeout(told, left)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                };
                drop(told);
                let Some(endpoint) = endpoint else {
                    crate::device::serve::refuse(
                        &stream,
                        "this computer's Tilo has not started its person's part yet: run `tilo \
                         status` to see how it stands, then try again",
                    );
                    return;
                };
                if let Err(error) = endpoint.take(stream, slot, None) {
                    tracing::debug!(%error, "a tenant connection was not served");
                }
            });
        if let Err(error) = served {
            tracing::warn!(%error, "a connection to the tenant's socket could not be served");
        }
    }
}

/// Tell the account at `stream` why the tenant's socket serves it nothing, then close it.
fn refuse_tenant(stream: &Stream, tenant: u32, peer: Option<u32>) {
    let why = match peer {
        Some(uid) => format!(
            "this socket is the workspace's tenant's alone: uid {uid} is not the account it \
             serves (uid {tenant})"
        ),
        None => "this socket is the workspace's tenant's alone: the kernel does not name the \
                 account connecting"
            .to_owned(),
    };
    crate::device::serve::refuse(stream, &why);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// What a workspace VM's tenant is served with: the person's unnamed agent, over the action wire
/// and MCP alike, so an asked action still asks and nothing here is the person.
fn the_tenant(session: &Arc<Daemon>) -> Endpoint<<Daemon as RuntimeServices>::ActionProvider> {
    Endpoint {
        session_factory: session.agent_sessions(UNNAMED_AGENT),
        status: status_of(session),
        search: Arc::new(OnceLock::new()),
        media: session.media(),
        other_devices: None,
        agents: None,
        frames: Frames::of_the_tenant(session),
        to_agents: None,
        // The tenant starts no app of the daemon's: what it runs, it runs in its own account.
        launch: None,
        // The tenant is served for the person's part, and closed with it.
        connections: Some(Arc::clone(session.connections())),
        acting: None,
    }
}

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
    /// The person's root key and the key every connection of this endpoint acts as, for one
    /// that serves another person's key: each connection ends once their root revokes it
    /// ([`Connections::end_key`]).
    acting: Option<([u8; 32], [u8; 32])>,
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
        acting: None,
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
        let acting = Some((cert.user_public_key, cert.device_public_key));
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
            acting,
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
                        match crate::os_search::start(host, &workspace, daemon.actor(), &daemon) {
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

/// [`ADMIT_WITHIN`] as a connection's session is made: the answer when it was made in time, or
/// why one was not.
fn admit_within<H: tilo_actions::remote::ActionProvider + Send + Sync + 'static>(
    make: impl FnOnce() -> io::Result<Arc<McpServer<H>>> + Send + 'static,
) -> io::Result<Arc<McpServer<H>>> {
    let (said, answer) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("tilo-native-admit".into())
        .spawn(move || {
            let _ = said.send(make());
        })
        .map_err(io::Error::other)?;
    match answer.recv_timeout(ADMIT_WITHIN) {
        Ok(admitted) => admitted,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Tilo on this computer didn't answer in time: run `tilo status` to see how it \
             stands, then try again",
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("this computer's daemon is stopping"))
        }
    }
}

/// The longest first line a native connection is read for before it is served: a forward to
/// another device, or the first request of any other. An action frame is the first line of a
/// connection that speaks the action wire, so the line is read up to the largest frame the wire
/// carries: one cut short reads as no frame, and its caller would wait on an answer never sent.
pub(crate) const FIRST_LINE: u64 = tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES as u64 + 2;

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
            acting: self.acting,
        }
    }
}

impl<H: tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static> Endpoint<H> {
    /// This endpoint with its sessions made by `sessions`: a test's connection of another agent
    /// than the one the login was made for.
    #[cfg(test)]
    pub(crate) fn with_sessions(mut self, sessions: tilo_mcp::SessionFactory<H>) -> Self {
        self.session_factory = sessions;
        self
    }

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
        let held = self
            .connections
            .as_ref()
            .and_then(|connections| connections.hold(&stream));
        // A connection of another person's key acts as that key from its first line: it is shut
        // once their root revokes the key, until a session it admits says how it is told.
        if let (Some(held), Some((user, device))) = (&held, self.acting) {
            held.acts_as(user, device, None);
        }
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
        // A first line that names another of the person's devices asks for the connection to be
        // carried there: its action frames go to that device from this daemon's one endpoint.
        if let Some(carry) = tilo_action_line::carry_request(line) {
            let input = BufReader::new(io::Cursor::new(rest).chain(follow));
            return self.frames.carry(stream, &carry, input);
        }
        if crate::device::local::is_frame(line) {
            let input = BufReader::new(io::Cursor::new(rest).chain(follow));
            let ending = self
                .connections
                .as_deref()
                .map(|connections| connections as &dyn tilo_action_line_serve::Ending);
            return self.frames.serve(stream, line, input, ending);
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
            self.forward(stream, &forward, rest.is_empty());
            return;
        }
        self.serve_session(
            stream,
            first,
            (rest, follow),
            seated,
            descriptors,
            held.as_ref(),
        );
    }

    /// Answer `stream` as a session, `first` and then `rest` and what `follow` reads being its
    /// input: by the seats when it asks them, as a launcher's start, or as the person's own
    /// connection.
    fn serve_session(
        &self,
        stream: Stream,
        first: Vec<u8>,
        (rest, follow): (Vec<u8>, Stream),
        seated: Option<&Seated>,
        descriptors: Vec<runtime_portal::process_link::Descriptor>,
        held: Option<&HeldConnection>,
    ) {
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
        let factory = Arc::clone(&self.session_factory);
        let media = self.media.clone();
        let admit = move || {
            let (hand, factory, media) = (hand.clone(), Arc::clone(&factory), media.clone());
            admit_within(move || {
                let made = crate::seats::making_for(hand.clone(), || factory())?;
                Ok::<_, io::Error>(Arc::new(made.with_media(media.clone())))
            })
        };
        if let Err(error) = serve_connection(
            &stream,
            input,
            &admit,
            &(self.status)(),
            &self.search,
            seated,
            self.agents.as_ref(),
            held,
        ) {
            tracing::debug!(%error, "native runtime client disconnected");
        }
    }

    /// Carry `stream` to another of the person's devices' MCP server, as its first line asked,
    /// when that line is all it sent (`alone`): a client asks for nothing more before it is told
    /// whether the connection opened.
    fn forward(&self, stream: Stream, forward: &tilo_mcp::devices::Forward, alone: bool) {
        match &self.other_devices {
            Some(other_devices) if alone => other_devices.serve_forward(stream, forward),
            Some(_) => tilo_mcp::devices::refuse_forward(
                stream,
                "a forward is the only line before its answer".into(),
            ),
            None => tilo_mcp::devices::refuse_forward(
                stream,
                "this runtime reaches none of your other devices".into(),
            ),
        }
    }
}

/// The connections a part of the daemon serves for its person: each is closed when that part
/// ends ([`Self::end`]), once every call it took has its answer ([`Self::finish`]). One left
/// open would be answered by the services of a part that ended, refusing every call, and would
/// hold those services' stores and links open under the part that starts next; closed, its
/// client connects again and reaches the new part. No connection of one part is ever served by
/// the next: that part may be another person's.
#[derive(Default)]
pub(crate) struct Connections {
    open: Mutex<std::collections::BTreeMap<u64, Open>>,
    next: std::sync::atomic::AtomicU64,
    /// The requests this part is answering now, each a call some program waits on.
    answering: Answering,
    /// Why the part takes no new call, once it ends: what each new call is answered with.
    ending: Mutex<Option<tilo_actions::remote::ActionCallError>>,
}

/// One connection a part of the daemon serves.
struct Open {
    stream: Stream,
    /// The key it acts as, once that is known ([`HeldConnection::acts_as`]).
    acting: Option<Acting>,
}

/// The key a connection acts as, and how it ends once that key's person revokes it
/// ([`Connections::end_key`]).
struct Acting {
    /// The person's root key, and the key itself.
    key: ([u8; 32], [u8; 32]),
    /// What the connection tells its client as it ends, for a connection whose own loop reads
    /// its ending ([`HeldConnection::ended`]); `None` for one shut at once.
    told: Option<String>,
    /// Set once the key was revoked: why the connection ends.
    ended: Option<String>,
}

/// The calls a part of the daemon is answering now, counted while each answer runs: a part that
/// ends lets the calls it serves hear their answer first, rather than cutting the one that made
/// the change it ends for.
#[derive(Clone)]
pub(crate) struct Answering(Arc<tokio::sync::watch::Sender<usize>>);

impl Default for Answering {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::watch::channel(0).0))
    }
}

impl Answering {
    /// Count one call being answered until the returned guard is dropped.
    fn began(&self) -> AnsweringCall {
        self.0
            .send_modify(|answering| *answering = answering.saturating_add(1));
        AnsweringCall(Arc::clone(&self.0))
    }

    /// Wait for every call being answered to have its answer, however long that takes: how
    /// many are still waited on is said every [`ANSWERING_SAID`].
    async fn answered(&self) {
        let mut answering = self.0.subscribe();
        loop {
            let answered = tokio::time::timeout(
                ANSWERING_SAID,
                answering.wait_for(|answering| *answering == 0),
            )
            .await
            .is_ok();
            if answered {
                return;
            }
            let still = *answering.borrow();
            tracing::info!(
                answering = still,
                "the person's part ending still waits for calls it took to be answered"
            );
        }
    }
}

/// The count of one call being answered, for as long as the answer runs.
pub(crate) struct AnsweringCall(Arc<tokio::sync::watch::Sender<usize>>);

impl Drop for AnsweringCall {
    fn drop(&mut self) {
        self.0
            .send_modify(|answering| *answering = answering.saturating_sub(1));
    }
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
        self.open.lock_recover().insert(
            number,
            Open {
                stream: kept,
                acting: None,
            },
        );
        Some(HeldConnection(Arc::downgrade(self), number))
    }

    /// Close every connection the part serves: what each is reading ends and what it writes
    /// fails, so its serving ends and lets go of the part's services. The number closed.
    pub(crate) fn end(&self) -> usize {
        let open = std::mem::take(&mut *self.open.lock_recover());
        for open in open.values() {
            // A shutdown wakes the reader and never waits: a connection already gone is
            // closed already.
            let _ = open.stream.shutdown(std::net::Shutdown::Both);
        }
        open.len()
    }

    /// End every connection that acts as the key `device` of the person `user`, as its person
    /// revoked it: one whose loop reads its ending tells its client why and closes within that
    /// loop's next look ([`HeldConnection::ended`]), and any other is shut now. What the
    /// workspace writes from then on reaches the key over none of them, and its calls in flight
    /// are dropped with them.
    pub(crate) fn end_key(&self, user: &[u8; 32], device: &[u8; 32]) {
        let mut open = self.open.lock_recover();
        for open in open.values_mut() {
            let Some(acting) = open
                .acting
                .as_mut()
                .filter(|acting| acting.key == (*user, *device))
            else {
                continue;
            };
            match &acting.told {
                Some(told) => acting.ended = Some(told.clone()),
                None => {
                    let _ = open.stream.shutdown(std::net::Shutdown::Both);
                }
            }
        }
    }

    /// Have every call the part took answered before it ends, however long they run: from now
    /// on each new call is answered with `why`, and this returns once every call counted has its
    /// answer. The connections stay open until [`Self::end`].
    pub(crate) async fn finish(&self, why: tilo_actions::remote::ActionCallError) {
        *self.ending.lock_recover() = Some(why);
        self.answering.answered().await;
    }

    /// `answer`, counted among the calls this part serves until it answered.
    fn counting<'a>(&self, answer: Answer<'a>) -> Answer<'a> {
        match answer {
            Answer::InOrder(answer) => {
                let answering = self.answering.began();
                Answer::InOrder(Box::pin(async move {
                    let _answering = answering;
                    answer.await
                }))
            }
            Answer::Overlapping(answer) => {
                let answering = self.answering.began();
                Answer::Overlapping(Box::pin(async move {
                    let _answering = answering;
                    answer.await
                }))
            }
        }
    }
}

impl tilo_action_line_serve::Ending for Connections {
    fn ending(&self) -> Option<tilo_actions::remote::ActionCallError> {
        self.ending.lock_recover().clone()
    }

    fn answering(&self) -> Box<dyn Send> {
        Box::new(self.answering.began())
    }
}

/// How often a part that ends says how many of its calls it still waits on.
const ANSWERING_SAID: Duration = Duration::from_secs(10);

/// A connection [`Connections::hold`] keeps, until its serving ends.
pub(crate) struct HeldConnection(std::sync::Weak<Connections>, u64);

impl HeldConnection {
    /// This connection acts as the key `device` of the person `user`: it ends once their person
    /// revokes that key ([`Connections::end_key`]). With `told`, its own loop reads that it ended
    /// and tells its client so ([`Self::ended`]); without, it is shut at once.
    pub(crate) fn acts_as(&self, user: [u8; 32], device: [u8; 32], told: Option<String>) {
        let Some(connections) = self.0.upgrade() else {
            return;
        };
        if let Some(open) = connections.open.lock_recover().get_mut(&self.1) {
            open.acting = Some(Acting {
                key: (user, device),
                told,
                ended: None,
            });
        }
    }

    /// Why this connection ends, once the key it acts as was revoked ([`Self::acts_as`]).
    pub(crate) fn ended(&self) -> Option<String> {
        let connections = self.0.upgrade()?;
        let open = connections.open.lock_recover();
        open.get(&self.1)?.acting.as_ref()?.ended.clone()
    }
}

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

/// Whether `stream`'s first line follows a call by its invocation: the request a caller makes
/// while it waits on a call the device took. The line is looked at without being read, so the
/// endpoint that serves the connection reads it, descriptors and all, as it always has.
#[cfg(unix)]
fn follows_a_call(stream: &Stream) -> bool {
    use rustix::net::RecvFlags;
    // What follows an invocation id is written as the connection opens: a client that says
    // nothing by then is no follow, and waits as any other connection does.
    const SAYS_WITHIN: Duration = Duration::from_secs(1);
    const SAYS: &[u8] = b"\"invocation_id\"";
    let mut line = [0_u8; 512];
    if stream.set_read_timeout(Some(SAYS_WITHIN)).is_err() {
        return false;
    }
    let read = match rustix::net::recv(stream, &mut line[..], RecvFlags::PEEK) {
        Ok((read, _)) => read,
        Err(_) => 0,
    };
    let _ = stream.set_read_timeout(None);
    line.get(..read)
        .is_some_and(|read| read.windows(SAYS.len()).any(|window| window == SAYS))
}

/// No Unix socket to look at: a Windows pipe's first line is left to the endpoint that serves it.
#[cfg(not(unix))]
fn follows_a_call(_stream: &Stream) -> bool {
    false
}

/// Whether `stream`'s first line asks what the device key does ([`crate::device::key_use`]): one
/// the device's own actions answer. Looked at without being read, so whoever serves the
/// connection reads it, descriptors and all, as it always has.
/// Whether a connection from the session a sign-in answered a founding link asks one of the
/// device's actions that session makes or joins its person with, even once it is the person's:
/// the setup's way into a device whose services answer as them.
fn asks_the_founding(stream: &Stream) -> bool {
    /// How long a request has to say itself on the connection it opens.
    const SAYS_WITHIN: Duration = Duration::from_secs(1);
    let mut line = vec![0_u8; 64 * 1024];
    let read = tilo_socket::peek(stream, &mut line, SAYS_WITHIN).unwrap_or(0);
    let Some(line) = line
        .get(..read)
        .and_then(|read| read.split(|byte| *byte == b'\n').next())
    else {
        return false;
    };
    let Ok(request) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    let params = request.get("params");
    request.get("method").and_then(Value::as_str) == Some("tools/call")
        && params
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            == Some("tilo_do")
        && params
            .and_then(|params| params.get("arguments"))
            .and_then(|arguments| arguments.get("path"))
            .and_then(Value::as_str)
            .is_some_and(|path| {
                use tilo_device_identity::login::{FOUND, LOG_IN, RESTORE};
                let Some((resource, action)) = path.split_once('#') else {
                    return false;
                };
                match resource {
                    tilo_device_identity::login::RESOURCE => {
                        matches!(action, FOUND | RESTORE | LOG_IN)
                            || action == crate::device::ticket::INFO
                    }
                    // The session that made the person certifies the key its root made it here,
                    // with the device's own actions, until its key is certified.
                    crate::seats::SEATS => action == crate::seats::CERTIFIED,
                    _ => false,
                }
            })
}

fn asks_the_device_key(stream: &Stream) -> bool {
    // A program asking the key writes its whole request as it connects: one that says nothing
    // within this is no such request, and is served as any other.
    const SAYS_WITHIN: Duration = Duration::from_secs(1);
    let mut line = vec![0_u8; 64 * 1024];
    let read = tilo_socket::peek(stream, &mut line, SAYS_WITHIN).unwrap_or(0);
    let Some(line) = line
        .get(..read)
        .and_then(|read| read.split(|byte| *byte == b'\n').next())
    else {
        return false;
    };
    let Ok(request) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    let params = request.get("params");
    request.get("method").and_then(Value::as_str) == Some("tools/call")
        && params
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            == Some("tilo_do")
        && params
            .and_then(|params| params.get("arguments"))
            .and_then(|arguments| arguments.get("path"))
            .and_then(Value::as_str)
            .is_some_and(|path| {
                path.strip_prefix(tilo_device_identity::login::RESOURCE)
                    .and_then(|action| action.strip_prefix('#'))
                    .is_some_and(crate::device::key_use::asked_of_the_key)
            })
}

/// Whether `stream`'s first line is the action wire asking what the device keeps itself: its
/// package cache ([`package_actions::PROVIDER`]) and the id a development build of an app runs
/// under ([`tilo_device_identity::login::DEV_ID`]). Both are the device's, answered by the part
/// that keeps its key, never by the person's part that is starting. Looked at without being
/// read, so whoever serves the connection reads it as it always has.
fn asks_the_devices_own_actions(stream: &Stream) -> bool {
    use tilo_actions::remote::RemoteActionMessage;
    /// How long a request has to say itself on the connection it opens.
    const SAYS_WITHIN: Duration = Duration::from_secs(1);
    let mut line = vec![0_u8; 64 * 1024];
    let read = tilo_socket::peek(stream, &mut line, SAYS_WITHIN).unwrap_or(0);
    let Some(line) = line
        .get(..read)
        .and_then(|read| read.split(|byte| *byte == b'\n').next())
    else {
        return false;
    };
    let Ok(frame) = tilo_actions::remote::RemoteActionFrame::decode(line) else {
        return false;
    };
    match &frame.message {
        RemoteActionMessage::CatalogueRequest { query, .. } => {
            query.provider.as_deref() == Some(package_actions::PROVIDER)
        }
        RemoteActionMessage::Invoke(request) => {
            request.target.provider == package_actions::PROVIDER
                || (request.target.provider == tilo_device_identity::login::PROVIDER
                    && request.target.address.resource == tilo_device_identity::login::RESOURCE
                    && request.target.address.action == tilo_device_identity::login::DEV_ID)
        }
        _ => false,
    }
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

/// Whether `stream`'s first line asks the seats ([`Seats::asks`]): who the connection is, a shell
/// started or waited for, or the link a shell is admitted by. Looked at without being read, so
/// whoever serves the connection reads it, descriptors and all, as it always has.
fn asks_the_seats(stream: &Stream) -> bool {
    /// How long a request has to say itself on the connection it opens.
    const SAYS_WITHIN: Duration = Duration::from_secs(1);
    let mut line = vec![0_u8; 64 * 1024];
    let read = tilo_socket::peek(stream, &mut line, SAYS_WITHIN).unwrap_or(0);
    let Some(line) = line
        .get(..read)
        .and_then(|read| read.split(|byte| *byte == b'\n').next())
    else {
        return false;
    };
    serde_json::from_slice::<Value>(line).is_ok_and(|request| Seats::asks(&request))
}

/// Answer `stream` as a client of the seats, reading the first line it sent.
fn serve_a_seat(stream: &Stream, seated: &Seated) {
    let (first, rest, descriptors) = match first_line(stream, Vec::new()) {
        Ok(read) => read,
        Err(error) => {
            tracing::debug!(%error, "a native connection was not read");
            return;
        }
    };
    serve_the_read_seat(stream, seated, first, rest, descriptors);
}

/// [`serve_a_seat`] for a first line already read, with what followed it and the descriptors it
/// carried.
fn serve_the_read_seat(
    stream: &Stream,
    seated: &Seated,
    first: Vec<u8>,
    rest: Vec<u8>,
    descriptors: Vec<runtime_portal::process_link::Descriptor>,
) {
    seated.receives(descriptors);
    let Ok(follow) = stream.try_clone() else {
        return;
    };
    let input = io::Cursor::new(first).chain(io::Cursor::new(rest).chain(follow));
    if let Err(error) = serve_seat(stream, input, seated) {
        tracing::debug!(%error, "native seat client disconnected");
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
    // Each answer is written whole, so the link `shells#link` answers goes with its line.
    let mut writer = io::BufWriter::new(crate::seats::Handing {
        stream,
        handed: Some(&*seated.handed),
    });
    let ended = serve_lines(runtime, input, &mut writer, answer, None);
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
    held: Option<&HeldConnection>,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    let connections = held.and_then(|held| held.0.upgrade());
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Every connection thread waits on the one app runtime rather than
    // building a runtime per connection.
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let input = spawn_reader(input)?;
    let status = serde_json::to_value(status).map_err(io::Error::other)?;
    // The session this connection reaches, admitted by the first request that needs one: what
    // this device is is answered without it, so asking founds no workspace and waits for none.
    let mut session: Option<Arc<McpServer<H>>> = None;
    let mut unanswered = |value: Value| {
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
                    // The connection acts as the session's key from here, and ends, told why, once
                    // that key's person revokes it.
                    if let (Some(held), Some((user, device))) = (held, admitted.key()) {
                        let told = if agent.is_some() {
                            tilo_app_host::agents::agent_keys::REVOKED.to_owned()
                        } else {
                            KEY_REVOKED.to_owned()
                        };
                        held.acts_as(user, device, Some(told));
                    }
                    session = Some(Arc::clone(&admitted));
                    admitted
                }
                Err(error) => {
                    // Told why, as an agent whose person revoked its key is.
                    tracing::debug!(%error, "native MCP session admission unavailable");
                    return refusing(&value, &error.to_string());
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
                return match &agent {
                    Some(agents) => agents
                        .answer_first(value)
                        .unwrap_or_else(|value| mcp_answer(&server, value)),
                    None => mcp_answer(&server, value),
                };
            }
        })
    };
    // Each call the part answers is counted until its answer: a part that ends waits for them,
    // so the call that made the change it ends for hears its answer.
    let answer = |value: Value| match &connections {
        Some(connections) => connections.counting(unanswered(value)),
        None => unanswered(value),
    };
    // A connection whose key was revoked ends at its loop's next look, whatever it waits on.
    let mut revoked = || match held.and_then(HeldConnection::ended) {
        Some(why) => Err(io::Error::new(io::ErrorKind::PermissionDenied, why)),
        None => Ok(()),
    };
    // Each answer is written whole, so a descriptor it carries (the link `seats#signed-in` hands
    // root) goes with its line.
    let mut writer = io::BufWriter::new(crate::seats::Handing {
        stream,
        handed: seated.map(|seated| &*seated.handed),
    });
    let ended = serve_lines(
        runtime,
        input,
        &mut writer,
        answer,
        held.map(|_| &mut revoked as &mut dyn FnMut() -> io::Result<()>),
    );
    // A connection ended for its revoked key tells its client why, naming no request.
    if let Some(why) = held.and_then(HeldConnection::ended) {
        let ending =
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": why}});
        let _ = tilo_mcp::lines::write_reply(&mut writer, &ending);
    }
    // The reader thread holds a clone of the socket: shut it, so the reader
    // sees its end and the client its connection close.
    let _ = stream.shutdown(std::net::Shutdown::Both);
    ended
}

/// What a connection of a key that is no agent's is told as it ends, its person having revoked
/// the key it acts as.
const KEY_REVOKED: &str = "key_revoked: this key was revoked, so its connection ended: approve this device again to \
     connect once more";

/// The answer to `request` its session could not be admitted for, saying `why`: a call's own
/// refusal for a tool call, an error for any other request.
fn refusing(request: &Value, why: &str) -> Answer<'static> {
    let reply = request.get("id").cloned().map(|id| {
        if request.get("method").and_then(Value::as_str) == Some("tools/call") {
            json!({"jsonrpc": "2.0", "id": id, "result": {
                "content": [{"type": "text", "text": why}], "isError": true}})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": why}})
        }
    });
    Answer::InOrder(Box::pin(async move { Ok(reply) }))
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
const LAUNCH: &str = runtime_portal::native::LAUNCH;

/// How long a second start of an app's default presentation waits for the first to show its
/// window before it is refused.
#[cfg(unix)]
const PRESENTATION_GRACE: Duration = Duration::from_secs(3);

/// Whether `request` asks this computer to start an app.
#[cfg(unix)]
fn is_launch_request(request: &[u8]) -> bool {
    runtime_portal::native::LaunchRequest::is_launch(request)
}

/// Who a launch request comes from, by the endpoint it reached: the person's own native
/// endpoint, or the connection of an app the daemon started.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asks {
    Person,
    App,
}

/// What answers a launcher's request: it starts the app this computer has installed, on the
/// launch path a window takes, so the launch record exists before the app's connection does, and
/// tells the caller it started.
#[cfg(unix)]
fn launch_take(session: &Arc<Daemon>, asks: Asks) -> Arc<LaunchTake> {
    let session = Arc::downgrade(session);
    Arc::new(move |mut stream, request, descriptors| {
        // The process at the other end, as the kernel names it: an app this computer started asks
        // for another instance of itself, and a person's own program for an app it names.
        let peer = crate::seats::Peer::of(&stream).ok();
        let reply = match start_requested(&session, request, &descriptors, peer, asks) {
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
/// what runs, and the launch path a window takes starts it and records the process. `peer` is the
/// process asking, when the kernel names it, and `asks` the endpoint it asked on.
#[cfg(unix)]
fn start_requested(
    session: &std::sync::Weak<Daemon>,
    request: &[u8],
    descriptors: &[runtime_portal::process_link::Descriptor],
    peer: Option<crate::seats::Peer>,
    asks: Asks,
) -> io::Result<Value> {
    let request: runtime_portal::native::LaunchRequest = serde_json::from_slice(request)
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
    // The app opens in the workspace the launch names: any this computer holds for its person,
    // whichever one its other windows show. Nothing of the computer's own workspace changes.
    let workspace = request.workspace.as_str();
    let apps = daemon.apps();
    if !apps.launches_into(workspace) {
        // The person's own workspace, as this computer holds it.
        let own = daemon.workspace().ok();
        if own.as_deref() != Some(workspace) {
            return Err(io::Error::other(format!(
                "this computer holds no workspace {workspace} of yours to start {} in; \
                 `tilo ls` lists the ones it does",
                request.app_id
            )));
        }
    }
    // An app this computer started asks for another instance of itself, in the workspace it
    // runs in: the launch a person's own program makes is not an app's to make for another app,
    // and an app acts in its own workspace alone.
    if let Some((started, started_in)) = peer
        .and_then(|peer| peer.pid)
        .and_then(|pid| daemon.app_of(pid))
    {
        if started != request.app_id {
            return Err(io::Error::other(format!(
                "this process runs {started}: an app asks for another instance of itself, not of {}",
                request.app_id
            )));
        }
        if started_in != workspace {
            return Err(io::Error::other(format!(
                "{started} runs in {started_in} and starts its instances there, not in {workspace}"
            )));
        }
    }
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
    if let Some(capture) = &request.capture {
        return start_capture(&daemon, &app, workspace, capture, peer, asks);
    }
    // The app's default presentation is one: a second start shows the one running, and while the
    // first has shown nothing the launch's own lock waits for it, as a launch takes it. A
    // tilolang app's window is the person's layout's, so its start shows that window there and a
    // second start brings the window already showing it forward; nothing waits on a process.
    #[cfg(target_os = "linux")]
    if request.presentation && crate::device::package_runs::runs_in_process(&app.app_id) {
        return start_in_layout(&daemon, &app.app_id, workspace);
    }
    let guard = if request.presentation {
        match default_launch_guard(apps.root(), &app.app_id, PRESENTATION_GRACE)? {
            Some(guard) => Some(guard),
            None => return Ok(json!({"activated": true})),
        }
    } else {
        None
    };
    // The start answers once the app is up: this thread is a launch's own, never the app
    // runtime's, so it waits on that runtime's own work.
    let app_id = app.app_id;
    let workspace = workspace.to_owned();
    crate::device::workspaces::block_on_owned(async move {
        daemon
            .apps()
            .start_watched_window(&app_id, &workspace, guard)
            .await
    })
    .map_err(io::Error::other)?;
    Ok(json!({"started": true}))
}

/// Run the capture `capture` asks for, of the app this computer has installed in `workspace`, and
/// answer once its process ended: what it wrote, and the directories it was granted.
#[cfg(target_os = "linux")]
fn start_capture(
    daemon: &Arc<Daemon>,
    app: &app_catalogue::apps::InstalledApp,
    workspace: &str,
    capture: &runtime_portal::native::Capture,
    peer: Option<crate::seats::Peer>,
    asks: Asks,
) -> io::Result<Value> {
    let args = capture.args()?;
    let asking_app = peer
        .and_then(|peer| peer.pid)
        .and_then(|pid| daemon.app_of(pid))
        .map(|(started, _)| started);
    let (writes, joins) = capture_grant(
        daemon,
        app,
        workspace,
        args,
        peer,
        asks,
        asking_app.as_deref(),
    )?;
    daemon
        .apps()
        .capture(&app.app_id, workspace, args, &writes, joins)
        .map_err(io::Error::other)?;
    // The directories the capture was granted, for its caller to see where it wrote.
    Ok(json!({"captured": true, "writes": writes}))
}

/// [`start_capture`] where no sandbox keeps a capture's directories: the capture runs with no
/// grant of this computer's, and writes nowhere of its own. A capture is a Unix launch, as
/// [`start_requested`] is.
#[cfg(all(unix, not(target_os = "linux")))]
fn start_capture(
    daemon: &Arc<Daemon>,
    app: &app_catalogue::apps::InstalledApp,
    workspace: &str,
    capture: &runtime_portal::native::Capture,
    _: Option<crate::seats::Peer>,
    _: Asks,
) -> io::Result<Value> {
    let args = capture.args()?;
    daemon
        .apps()
        .capture(&app.app_id, workspace, args, &[], false)
        .map_err(io::Error::other)?;
    Ok(json!({"captured": true, "writes": Vec::<std::path::PathBuf>::new()}))
}

/// The directories the capture `args` of `app` writes into, granted for its launch alone, and
/// whether its instance joins the workspace: both only when the person on this device's own
/// connection asks, for a development build this device made ([`crate::apps::capture`]).
#[cfg(target_os = "linux")]
fn capture_grant(
    daemon: &Arc<Daemon>,
    app: &app_catalogue::apps::InstalledApp,
    serving: &str,
    args: &[String],
    peer: Option<crate::seats::Peer>,
    asks: Asks,
    asking_app: Option<&str>,
) -> io::Result<(Vec<std::path::PathBuf>, bool)> {
    // Who asks: an app's own connection, or a process the daemon started for an app, is that
    // app; any other on the person's endpoint is who that endpoint admits.
    let caller = || match (asks, asking_app) {
        (Asks::App, None) => Ok(tilo_actions::remote::Caller::launched(app.app_id.as_str())),
        (_, Some(started)) => Ok(tilo_actions::remote::Caller::launched(started)),
        (Asks::Person, None) => daemon
            .connection_tree_in(Some(serving))
            .map(|(_, caller)| caller)
            .map_err(|error| error.to_string()),
    };
    let writer = crate::apps::capture::Writer {
        personal: !tilo_api::daemon::system_daemon(),
        app_id: app.app_id.as_str(),
        dev: app.dev.as_ref(),
        device: daemon.key().verifying_key().to_bytes(),
        uid: peer.map(|peer| peer.uid),
        protected: [
            tilo_paths::home(),
            tilo_paths::workspace_root(),
            tilo_paths::data_dir(),
            Some(daemon.apps().root().to_path_buf()),
        ]
        .into_iter()
        .flatten()
        .collect(),
    };
    let granted = crate::apps::capture::writes(args, caller, &writer)
        .map_err(|refused| io::Error::new(io::ErrorKind::PermissionDenied, refused))?;
    let joins = crate::apps::capture::joins(args, caller, &writer)
        .map_err(|refused| io::Error::new(io::ErrorKind::PermissionDenied, refused))?;
    Ok((granted, joins))
}

/// Ask the daemon at `profile` to start an app, and answer what it said: a capture answers once
/// the app it runs has ended, so the caller waits as long as the capture takes.
#[cfg(unix)]
fn ask_launch(profile: &Path, request: &Value, capture: bool) -> io::Result<Value> {
    let stream = tilo_files::native::socket::connect(&native_socket(profile)?)?;
    // A capture answers once the app it runs has ended, however long that takes.
    stream.set_read_timeout(if capture { None } else { Some(ANSWERING) })?;
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
pub fn run_app(
    profile: &Path,
    app_id: &str,
    workspace: Option<&str>,
    capture: Option<&[String]>,
) -> io::Result<()> {
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
    let mut request = runtime_portal::native::LaunchRequest::new(
        app_id,
        &workspace,
        presentation && capture.is_none(),
    );
    if let Some(args) = capture {
        request = request.capturing(args);
    }
    let answer = ask_launch(
        profile,
        &serde_json::to_value(request).map_err(io::Error::other)?,
        capture.is_some(),
    )?;
    if let Some(error) = answer.get("error").and_then(Value::as_str) {
        return Err(io::Error::other(error.to_owned()));
    }
    // Where the capture was let write outside its sandbox, said where its caller reads it.
    if let Some(writes) = answer
        .get("writes")
        .and_then(Value::as_array)
        .filter(|writes| !writes.is_empty())
    {
        let dirs: Vec<&str> = writes.iter().filter_map(Value::as_str).collect();
        eprintln!(
            "{app_id}'s capture wrote into {}, granted for its launch alone",
            dirs.join(", ")
        );
    }
    Ok(())
}

/// An app on Windows is handed its services on its process link, by the daemon that starts it;
/// nothing there attaches one by replacing a host process with it.
///
/// # Errors
/// Always [`io::ErrorKind::Unsupported`].
#[cfg(windows)]
pub fn run_app(
    _profile: &Path,
    _app_id: &str,
    _workspace: Option<&str>,
    _capture: Option<&[String]>,
) -> io::Result<()> {
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

/// Start the window of the tilolang app `app` in `workspace`, which the person's layout holds:
/// shown where the layout shows none, and brought in front where it shows one. What a start of
/// the app's default presentation answers.
#[cfg(target_os = "linux")]
fn start_in_layout(daemon: &Arc<Daemon>, app: &str, workspace: &str) -> io::Result<Value> {
    if daemon
        .show_app_window(app, workspace)
        .map_err(io::Error::other)?
    {
        return Ok(json!({"activated": true}));
    }
    let (app, workspace) = (app.to_owned(), workspace.to_owned());
    let starting = Arc::clone(daemon);
    crate::device::workspaces::block_on_owned(async move {
        starting
            .apps()
            .start_watched_window(&app, &workspace, None)
            .await
    })
    .map_err(io::Error::other)?;
    Ok(json!({"started": true}))
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "daemon_tenant_tests.rs"]
mod tenant_tests;

#[cfg(test)]
#[path = "daemon_limits_tests.rs"]
mod limits_tests;
