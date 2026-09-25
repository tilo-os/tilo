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
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const START_TIMEOUT: Duration = Duration::from_secs(15);
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
    let path = if let Some(value) = std::env::var_os("QOS_RUNTIME_PROFILE") {
        PathBuf::from(value)
    } else {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| invalid("HOME is missing; set QOS_RUNTIME_PROFILE"))?;
        #[cfg(target_os = "macos")]
        let suffix = "Library/Application Support/qOS/runtime";
        #[cfg(not(target_os = "macos"))]
        let suffix = ".local/state/qos/runtime";
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

fn lock_until(file: &File, deadline: Instant) -> io::Result<()> {
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
            Err(std::fs::TryLockError::WouldBlock) => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native runtime startup lock timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn ensure_running(profile: &Path) -> io::Result<Status> {
    validate_profile(profile)?;
    if let Ok(status) = probe(profile) {
        return Ok(status);
    }
    let deadline = Instant::now()
        .checked_add(START_TIMEOUT)
        .ok_or_else(|| io::Error::other("startup deadline overflow"))?;
    let lock = unix::private_file(&profile.join("start.lock"))?;
    lock_until(&lock, deadline)?;
    if let Ok(status) = probe(profile) {
        return Ok(status);
    }
    let log = unix::private_file(&profile.join("daemon.log"))?;
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
    let mut child = command.spawn()?;
    loop {
        if let Ok(status) = probe(profile) {
            return Ok(status);
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "native runtime exited during startup ({status}); see {}",
                profile.join("daemon.log").display()
            )));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "native runtime did not become ready; see {}",
                    profile.join("daemon.log").display()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
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
            .name("qos-native-mcp".into())
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
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
                Ok(value) => runtime.block_on(server.handle(value)),
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
        .name("qos-mcp-proxy-stdin".into())
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
    let resume = std::env::var_os("QOS_RUNTIME_CONNECTION_OWNER_PID").as_deref()
        == Some(std::ffi::OsStr::new(&owner))
        && std::env::var_os("QOS_RUNTIME_RESUME_LIVE").as_deref()
            == Some(std::ffi::OsStr::new(app_id));
    let mut command = Command::new(executable);
    command
        .args(args)
        .env("TILO_BLOB_STORE", profile.join("blobs"));
    let connections = unix::Connections::prepare(
        &mut command,
        app_id,
        &status.room,
        &profile.join("collab.sock"),
        &profile.join("identity.sock"),
    )?;
    command
        .env("QOS_RUNTIME_CONNECTION_OWNER_PID", owner)
        .env("QOS_RUNTIME_PROFILE", profile);
    if resume {
        command.env("QOS_RUNTIME_RESUME_LIVE", app_id);
    }
    let error = command.exec();
    drop(connections);
    Err(error)
}
