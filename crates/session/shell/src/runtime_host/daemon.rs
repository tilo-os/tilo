//! Persistent native session shared by independently launched GUI apps and MCP.
use super::{
    Session,
    mcp::McpServer,
    mcp_process::{Frame, read_frame, write_reply},
    unix,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Daemon,
    Connect,
    RunApp {
        app_id: String,
        executable: OsString,
        args: Vec<OsString>,
    },
}
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub profile: PathBuf,
    pub mode: Mode,
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// Validate the private profile before any startup side effects.
/// # Errors
/// Rejects public directories, symlinks, foreign ownership or creation errors.
pub fn validate_profile(profile: &Path) -> io::Result<()> {
    if !profile.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    unix::check_private_profile(profile)
}

/// The same persistent profile is used by daemon, app attachment and MCP proxy.
/// # Errors
/// Rejects a missing home directory or a relative profile override.
pub fn default_profile() -> io::Result<PathBuf> {
    let path = if let Some(value) = std::env::var_os("TILO_RUNTIME_PROFILE") {
        PathBuf::from(value)
    } else {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| invalid("HOME is missing; set TILO_RUNTIME_PROFILE"))?;
        #[cfg(target_os = "macos")]
        let suffix = "Library/Application Support/Tilo/runtime";
        #[cfg(not(target_os = "macos"))]
        let suffix = ".local/state/tilo/runtime";
        PathBuf::from(home).join(suffix)
    };
    if !path.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    Ok(path)
}

/// Parse native modes while preserving opaque application arguments.
/// # Errors
/// Rejects ambiguous modes, missing arguments and relative profiles.
pub fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> io::Result<Args> {
    let mut arguments = arguments.into_iter();
    let mut profile = None;
    let mut mode = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--profile") if profile.is_none() => {
                profile = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| invalid("--profile requires a path"))?,
                ));
            }
            Some("--daemon") if mode.is_none() => mode = Some(Mode::Daemon),
            Some("--connect") if mode.is_none() => mode = Some(Mode::Connect),
            Some("--run-app") if mode.is_none() => {
                let app_id = arguments
                    .next()
                    .and_then(|arg| arg.into_string().ok())
                    .ok_or_else(|| invalid("--run-app requires an application ID"))?;
                if app_id.trim().is_empty() || app_id.len() > 4096 {
                    return Err(invalid("invalid application ID"));
                }
                if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
                    return Err(invalid("--run-app APP_ID requires -- EXECUTABLE ARGS..."));
                }
                let executable = arguments
                    .next()
                    .ok_or_else(|| invalid("missing app executable"))?;
                mode = Some(Mode::RunApp {
                    app_id,
                    executable,
                    args: arguments.collect(),
                });
                break;
            }
            _ => {
                return Err(invalid(format!(
                    "unexpected argument: {}",
                    argument.to_string_lossy()
                )));
            }
        }
    }
    let profile = match profile {
        Some(path) => path,
        None => default_profile()?,
    };
    if !profile.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    Ok(Args { profile, mode: mode.ok_or_else(|| invalid("choose --daemon, --connect, --run-app APP_ID -- EXECUTABLE, or --profile PATH --apps MANIFEST"))? })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    version: u32,
    pid: u32,
    room: String,
}

fn probe(profile: &Path) -> io::Result<Status> {
    let mut stream = UnixStream::connect(profile.join("native.sock"))?;
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
    if status.version != 1 || status.pid == 0 || status.room.is_empty() || status.room.len() > 4096
    {
        return Err(invalid("invalid native runtime status"));
    }
    Ok(status)
}

/// How long a starting daemon may take. Startup is not bounded by a fixed
/// deadline: a loaded machine can take far longer than usual to start a
/// session that is still working. It keeps waiting while the process is
/// alive and making progress, and gives up once progress stops for `stall`
/// or the whole start exceeds `cap`.
#[derive(Clone, Copy, Debug)]
pub(super) struct StartupLimits {
    pub(super) stall: Duration,
    pub(super) cap: Duration,
    pub(super) poll: Duration,
}

pub(super) const STARTUP: StartupLimits = StartupLimits {
    stall: Duration::from_secs(15),
    cap: Duration::from_secs(120),
    poll: Duration::from_millis(25),
};

/// What a starting process looks like from outside at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Observation {
    /// What the process is doing, for errors: "while <stage>".
    pub(super) stage: &'static str,
    /// Changes whenever the process shows any sign of progress.
    pub(super) progress: u64,
}

/// A spawned process that has not reported ready yet.
pub(super) trait StartingProcess {
    type Ready;
    /// The ready answer, once the process gives one.
    fn ready(&mut self) -> Option<Self::Ready>;
    /// How the process ended, if it already has.
    /// # Errors
    /// Returns failures to query the process.
    fn exited(&mut self) -> io::Result<Option<String>>;
    fn observe(&mut self) -> Observation;
    /// End a process that will not become ready.
    fn stop(&mut self);
    /// Where to look next, with the process's most recent output.
    fn diagnostics(&self) -> String;
}

/// Wait until `process` is ready, failing as soon as it exits, stalls or
/// exceeds the absolute cap.
/// # Errors
/// Names the stage the process reached and includes its diagnostics.
pub(super) fn wait_until_ready<P: StartingProcess>(
    process: &mut P,
    limits: StartupLimits,
) -> io::Result<P::Ready> {
    let started = Instant::now();
    let mut last = process.observe();
    let mut last_progress = started;
    loop {
        if let Some(ready) = process.ready() {
            return Ok(ready);
        }
        let now = process.observe();
        if now.progress != last.progress {
            last_progress = Instant::now();
        }
        last = now;
        if let Some(status) = process.exited()? {
            return Err(io::Error::other(format!(
                "native runtime exited while {} ({status}); {}",
                last.stage,
                process.diagnostics()
            )));
        }
        let failure = if last_progress.elapsed() >= limits.stall {
            format!(
                "native runtime stalled while {}: no progress for {} s",
                last.stage,
                limits.stall.as_secs_f32()
            )
        } else if started.elapsed() >= limits.cap {
            format!(
                "native runtime did not become ready within {} s (still {})",
                limits.cap.as_secs_f32(),
                last.stage
            )
        } else {
            std::thread::sleep(limits.poll);
            continue;
        };
        process.stop();
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{failure}; {}", process.diagnostics()),
        ));
    }
}

/// Files `Session::start` and `serve` create in order, each paired with what
/// the daemon does once it exists.
const MILESTONES: [(&str, &str); 6] = [
    ("runtime.lock", "provisioning the collaboration identity"),
    ("workspace.json", "starting the identity service"),
    ("identity.sock", "starting the collaboration broker"),
    ("collab.sock", "opening the content store"),
    ("blobs.sock", "binding the native endpoint"),
    ("native.sock", "answering the status probe"),
];
const OUTPUT_TAIL: u64 = 4096;

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::symlink_metadata(path).ok()?.modified().ok()
}

/// Total CPU time the process has used, in clock ticks.
#[cfg(target_os = "linux")]
fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces; the fields after it are fixed:
    // state is the first, user and system time the 12th and 13th.
    let mut fields = stat.get(stat.rfind(')')?.checked_add(2)?..)?.split(' ');
    let user = fields.nth(11)?.parse::<u64>().ok()?;
    let system = fields.next()?.parse::<u64>().ok()?;
    user.checked_add(system)
}
#[cfg(not(target_os = "linux"))]
fn cpu_ticks(_pid: u32) -> Option<u64> {
    None
}

/// The daemon child of this launcher. Progress is anything it visibly does:
/// reaching a startup milestone, writing output or using CPU time.
struct DaemonStart<'a> {
    profile: &'a Path,
    log: PathBuf,
    child: Child,
    /// Milestone modification times before the spawn, so files left by an
    /// earlier daemon do not count as this one's progress.
    before: [Option<SystemTime>; MILESTONES.len()],
}

impl StartingProcess for DaemonStart<'_> {
    type Ready = Status;
    fn ready(&mut self) -> Option<Status> {
        probe(self.profile).ok()
    }
    fn exited(&mut self) -> io::Result<Option<String>> {
        Ok(self.child.try_wait()?.map(|status| status.to_string()))
    }
    fn observe(&mut self) -> Observation {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        let mut stage = "loading the runtime";
        for ((name, next), before) in MILESTONES.iter().zip(&self.before) {
            let now = modified(&self.profile.join(name));
            if now.is_some() && now != *before {
                stage = next;
            }
            now.hash(&mut hasher);
        }
        std::fs::metadata(&self.log)
            .map(|metadata| metadata.len())
            .ok()
            .hash(&mut hasher);
        cpu_ticks(self.child.id()).hash(&mut hasher);
        Observation {
            stage,
            progress: hasher.finish(),
        }
    }
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn diagnostics(&self) -> String {
        let mut tail = Vec::new();
        if let Ok(mut file) = std::fs::File::open(&self.log) {
            use std::io::{Seek, SeekFrom};
            let length = file.metadata().map_or(0, |metadata| metadata.len());
            if file
                .seek(SeekFrom::Start(length.saturating_sub(OUTPUT_TAIL)))
                .is_ok()
            {
                let _ = file.take(OUTPUT_TAIL).read_to_end(&mut tail);
            }
        }
        let tail = String::from_utf8_lossy(&tail);
        let tail = tail.trim();
        if tail.is_empty() {
            format!("it wrote nothing to {}", self.log.display())
        } else {
            format!("last output from {}:\n{tail}", self.log.display())
        }
    }
}

fn ensure_running(profile: &Path) -> io::Result<Status> {
    validate_profile(profile)?;
    if let Ok(status) = probe(profile) {
        return Ok(status);
    }
    // A holder is another launcher starting the daemon, which gives up
    // within the startup cap, so waiting for it is bounded by that cap.
    let _start =
        advisory_lock::FileLock::exclusive(unix::private_file(&profile.join("start.lock"))?)?;
    if let Ok(status) = probe(profile) {
        return Ok(status);
    }
    let log_path = profile.join("daemon.log");
    let log = unix::private_file(&log_path)?;
    // Each start's log holds only its own daemon's output.
    log.set_len(0)?;
    let before = MILESTONES.map(|(name, _)| modified(&profile.join(name)));
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--profile")
        .arg(profile)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    // SAFETY: setsid is async-signal-safe and runs in the new child only. The
    // daemon must outlive the launcher/MCP terminal without holding its pipes.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    wait_until_ready(
        &mut DaemonStart {
            profile,
            log: log_path,
            child,
            before,
        },
        STARTUP,
    )
}

/// Start one production Session. The executable prepares its profile environment
/// before entering this function; reusable serving code never mutates it.
/// # Errors
/// Returns profile ownership, service startup, socket or worker-spawn errors.
pub fn serve(profile: &Path) -> io::Result<()> {
    validate_profile(profile)?;
    let session = Session::start(profile)?;
    let broker = session
        .broker
        .app_action_broker()
        .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))?;
    let path = profile.join("native.sock");
    // Session already owns runtime.lock. Only that owner may remove a stale
    // endpoint, and a second --daemon cannot reach this point.
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket() {
                return Err(invalid("native runtime endpoint is not a socket"));
            }
            std::fs::remove_file(&path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let status = Status {
        version: 1,
        pid: std::process::id(),
        room: session.room.clone(),
    };
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        if active.load(Ordering::Relaxed) >= 64 {
            drop(stream);
            continue;
        }
        let broker = broker.clone();
        let room = session.room.clone();
        let actor = session.actor.clone();
        let status = status.clone();
        let active = Arc::clone(&active);
        active.fetch_add(1, Ordering::Relaxed);
        let slot = ConnectionSlot(active);
        std::thread::Builder::new()
            .name("tilo-native-mcp".into())
            .spawn(move || {
                let _slot = slot;
                if let Err(error) =
                    serve_connection(stream, McpServer::new(broker, room, actor), &status)
                {
                    tracing::debug!(%error, "native runtime client disconnected");
                }
            })?;
    }
    Ok(())
}
struct ConnectionSlot(Arc<AtomicUsize>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn serve_connection(
    mut stream: UnixStream,
    mut server: McpServer,
    status: &Status,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Every connection thread waits on the one app runtime rather than
    // building a runtime per connection.
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    loop {
        let reply = match read_frame(&mut reader)? {
            Frame::Eof => return Ok(()),
            Frame::TooLong => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"request line exceeds 1 MiB"}}),
            ),
            Frame::Line(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) if value == json!({"native":"status"}) => {
                    Some(serde_json::to_value(status).map_err(io::Error::other)?)
                }
                Ok(value) => runtime
                    .block_on(server.handle(value))
                    .map_err(io::Error::other)?,
                Err(_) => Some(
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                ),
            },
        };
        if let Some(reply) = reply {
            write_reply(&mut stream, &reply)?;
        }
    }
}

/// Bridge this process's stdio to the persistent daemon. EOF closes only this
/// client's connection; neither applications nor the daemon are owned here.
/// # Errors
/// Returns bounded startup, transport, thread-spawn or stdio errors.
pub fn connect(profile: &Path) -> io::Result<()> {
    ensure_running(profile)?;
    let mut stream = UnixStream::connect(profile.join("native.sock"))?;
    let mut writer = stream.try_clone()?;
    std::thread::Builder::new()
        .name("tilo-mcp-proxy-stdin".into())
        .spawn(move || {
            let result = io::copy(&mut io::stdin().lock(), &mut writer);
            let _ = writer.shutdown(std::net::Shutdown::Write);
            result
        })?;
    io::copy(&mut stream, &mut io::stdout().lock())?;
    Ok(())
}

/// Attach connected service descriptors and replace this host with the app.
/// # Errors
/// Returns startup, handshake or exec errors without running disconnected.
pub fn run_app(
    profile: &Path,
    app_id: &str,
    executable: &OsString,
    args: &[OsString],
) -> io::Result<()> {
    let status = ensure_running(profile)?;
    let owner = std::process::id().to_string();
    // Only the bootstrap exec for this exact process can request resumption.
    // A same-app ordinary child must run its own command handler first.
    let resume = std::env::var_os("TILO_RUNTIME_CONNECTION_OWNER_PID").as_deref()
        == Some(std::ffi::OsStr::new(&owner))
        && std::env::var_os("TILO_RUNTIME_RESUME_LIVE").as_deref()
            == Some(std::ffi::OsStr::new(app_id));
    let mut command = Command::new(executable);
    command.args(args);
    let connections = unix::Connections::prepare(
        &mut command,
        app_id,
        &status.room,
        &unix::ServiceSockets {
            collab: &profile.join("collab.sock"),
            identity: &profile.join("identity.sock"),
            blobs: &profile.join("blobs.sock"),
        },
    )?;
    command
        .env("TILO_RUNTIME_CONNECTION_OWNER_PID", owner)
        .env("TILO_RUNTIME_PROFILE", profile);
    if resume {
        command.env("TILO_RUNTIME_RESUME_LIVE", app_id);
    }
    let error = command.exec();
    drop(connections);
    Err(error)
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;
