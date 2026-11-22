//! Persistent native session shared by independently launched GUI apps and MCP.
use crate::device::services::Daemon;
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
use tilo_api::daemon::admission::ROOT;
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
            Ok(status) => return Ok(status),
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

/// Start this computer's daemon over `profile` and serve it: the native endpoint the apps and
/// `--connect` reach, and the device as [`crate::device::hold`] serves it. The executable
/// prepares its profile environment before entering this function; reusable serving code never
/// mutates it.
pub fn serve(profile: &Path, dev: bool) -> io::Result<()> {
    validate_profile(profile)?;
    let device = crate::device::unclaimed::ThisDevice::lock()?;
    // Who each connection is: root and this daemon's own account act as the device, and under
    // the system unit the people signed in act as themselves.
    tilo_api::daemon::admission::install(tilo_api::daemon::admission::Admission::new(
        rustix::process::geteuid().as_raw(),
        tilo_api::daemon::system_daemon(),
    ));
    // The native endpoint answers from boot, before anyone's certificate names this device and
    // while its person's services start: the device's own actions first, the person's once they
    // run. This process has the device's lock, so only it removes a stale endpoint.
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
    {
        let native = Arc::clone(&native);
        std::thread::Builder::new()
            .name("tilo-native".into())
            .spawn(move || {
                if let Err(error) = native.accept(&listener) {
                    tracing::error!(%error, "the native endpoint stopped taking connections");
                }
            })?;
    }
    tracing::info!("answering this device's actions on its native endpoint");
    // Before a person's certificate names this computer's key, a computer of one account's
    // founds its person; Tilo OS's device, and one handed a ticket, are served with the key alone,
    // the ticket spent on the way, and the person's services start once a certificate names it.
    if !device.claimed() {
        if crate::device::services::founds_a_person() {
            crate::device::services::found_a_person()?;
        } else {
            device.spend_handed_ticket();
            device.until_claimed();
        }
    }
    let locked = device.into_lock();
    let mut stage = |stage: &str| tracing::debug!(stage, "starting the person's services");
    let session = Arc::new(Daemon::start_locked(profile, &mut stage, dev, locked)?);
    // A computer that just logged in takes its workspace in while the device is held (below):
    // nothing is admitted to the workspace before then. The endpoint admits each connection as
    // it comes, and host search starts once the workspace is here, rather than the daemon ending
    // before it could take the workspace in.
    let search = Arc::new(OnceLock::new());
    search_once_held(&session, &search)?;
    let endpoint = Endpoint {
        session_factory: admitted_per_connection(&session),
        status: Status {
            version: 1,
            pid: std::process::id(),
            workspace: session.workspace().to_owned(),
        },
        search,
        media: session.media(),
    };
    native.serve_the_person(endpoint);
    // The device is served until the daemon is told to stop, or another takes the device over:
    // the process ends then, its apps' connections with it.
    let held = crate::device::hold(&session);
    // Removed while the daemon still holds the device's lock: a daemon starting after it binds
    // its own endpoint there.
    let _ = std::fs::remove_file(&path);
    drop(session);
    held.map_err(|error| io::Error::other(format!("{error:#}")))?;
    Ok(())
}

/// A connection the native endpoint takes, with the slot it counts against and who it is.
type Take = Box<dyn Fn(UnixStream, ConnectionSlot, Seated) -> io::Result<()> + Send + Sync>;

/// The native endpoint over the daemon's whole life: the device's own actions from boot, and the
/// person's endpoint once their services run. A connection on a device no certificate names yet
/// is answered with the device's own actions at once; one on a set-up device while its person's
/// services start waits on its own thread until they run, so an app is never served the device
/// alone for its life. The listener itself never waits.
struct Native {
    answers: crate::device::unclaimed::DeviceOnly,
    /// Who each connection is, and the device's answers about it.
    seats: Arc<Seats>,
    person: OnceLock<Take>,
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
            person: OnceLock::new(),
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
        Endpoint {
            session_factory: Arc::new(move || {
                Ok(McpServer::new(
                    answers.clone(),
                    String::new(),
                    String::new(),
                ))
            }),
            // No workspace is served on it.
            status: Status {
                version: 1,
                pid: std::process::id(),
                workspace: String::new(),
            },
            search: Arc::new(OnceLock::new()),
            media: None,
            other_devices: None,
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
        assert!(
            self.person.set(take).is_ok(),
            "one daemon starts its person's services once"
        );
        let _told = self.started.0.lock_recover();
        self.started.1.notify_all();
    }

    /// Serve each connection to `listener` as whatever answers when it comes ([`accept`]): an
    /// account the daemon does not serve is answered the device's own actions alone.
    fn accept(self: &Arc<Self>, listener: &UnixListener) -> io::Result<()> {
        accept(listener, |stream, slot| {
            let seated = Seated {
                seat: self.seats.seat(&stream),
                seats: Arc::clone(&self.seats),
            };
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
        match self.person.get() {
            Some(person) => person(stream, slot, seated),
            None if !self.answers.claimed() => self.device_only(stream, slot, seated),
            None => {
                let native = Arc::clone(self);
                std::thread::Builder::new()
                    .name("tilo-native-wait".into())
                    .spawn(move || {
                        let mut told = native.started.0.lock_recover();
                        let person = loop {
                            if let Some(person) = native.person.get() {
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
struct Endpoint<H> {
    session_factory: tilo_mcp::SessionFactory<H>,
    status: Status,
    /// Opens the host search results another process was handed, once host search started.
    search: Arc<OnceLock<Arc<tilo_os_search::OsSearch>>>,
    /// Where answers read the content actions produced.
    media: Option<Arc<dyn tilo_mcp::MediaReader>>,
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
        let factory = daemon.mcp_session_factory(daemon.workspace())?;
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
                        match crate::os_search::start(host, daemon.workspace(), daemon.actor()) {
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

impl<H: tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static> Endpoint<H> {
    /// Serve `stream` on its own thread, counted in `slot` while it lasts, as `seated` says
    /// who it is.
    fn take(
        &self,
        stream: UnixStream,
        slot: ConnectionSlot,
        seated: Option<Seated>,
    ) -> io::Result<()> {
        let factory = Arc::clone(&self.session_factory);
        let status = self.status.clone();
        let search = Arc::clone(&self.search);
        let media = self.media.clone();
        std::thread::Builder::new()
            .name("tilo-native-mcp".into())
            .spawn(move || {
                let _slot = slot;
                let input = match stream.try_clone() {
                    Ok(clone) => clone,
                    Err(error) => {
                        tracing::debug!(%error, "native runtime client socket");
                        return;
                    }
                };
                let server = match factory() {
                    Ok(server) => Arc::new(server.with_media(media)),
                    Err(error) => {
                        tracing::debug!(%error, "native MCP session admission unavailable");
                        return;
                    }
                };
                if let Err(error) =
                    serve_connection(stream, input, &server, &status, &search, seated.as_ref())
                {
                    tracing::debug!(%error, "native runtime client disconnected");
                }
            })?;
        Ok(())
    }
}
struct ConnectionSlot(Arc<AtomicUsize>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Answer one native connection: the daemon's own requests in order, and
/// MCP requests as [`serve_lines`] does, overlapping one another.
fn serve_connection<H: tilo_actions::remote::ActionProvider + Send + Sync + 'static>(
    mut stream: UnixStream,
    input: impl Read + Send + 'static,
    server: &Arc<McpServer<H>>,
    status: &Status,
    search: &OnceLock<Arc<tilo_os_search::OsSearch>>,
    seated: Option<&Seated>,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Every connection thread waits on the one app runtime rather than
    // building a runtime per connection.
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let input = spawn_reader(input)?;
    let status = serde_json::to_value(status).map_err(io::Error::other)?;
    let answer = |value: Value| {
        if let Some(answer) = seated.and_then(|seated| seated.seats.answer(seated.seat, &value)) {
            return Answer::InOrder(Box::pin(async move { Ok(Some(answer)) }));
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
            _ => return mcp_answer(server, value),
        })
    };
    let ended = serve_lines(runtime, input, &mut stream, answer, None);
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
pub fn connect(profile: &Path, standalone: bool, workspace: Option<&str>) -> io::Result<()> {
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
    let stream = match route {
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
