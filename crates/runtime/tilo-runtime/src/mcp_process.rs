//! Process lifecycle and newline-delimited stdio framing for the native MCP host.

use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use crate::{RuntimeServices, mcp::McpServer};

pub(crate) const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppManifest {
    pub apps: Vec<AppSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    pub id: String,
    pub executable: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug)]
pub struct ValidatedManifest {
    pub apps: Vec<AppSpec>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Line(Vec<u8>),
    TooLong,
    Eof,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Args {
    pub profile: PathBuf,
    pub apps: PathBuf,
}

/// Parse the `--profile PATH --apps MANIFEST` command line.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an unknown or repeated
/// argument, a flag without its path, or a missing required flag.
pub(crate) fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<Args> {
    let mut profile = None;
    let mut apps = None;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--profile") if profile.is_none() => {
                profile = Some(PathBuf::from(args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--profile requires a path")
                })?));
            }
            Some("--apps") if apps.is_none() => {
                apps = Some(PathBuf::from(args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--apps requires a path")
                })?));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unexpected argument: {}", argument.to_string_lossy()),
                ));
            }
        }
    }
    Ok(Args {
        profile: profile
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing --profile PATH"))?,
        apps: apps.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "missing --apps MANIFEST")
        })?,
    })
}

/// Check that every manifest entry names an existing executable by absolute path.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an empty app id, a relative
/// executable path or a path that is not an executable file, and the
/// underlying error kind when the executable cannot be inspected.
pub fn validate_manifest(manifest: AppManifest) -> io::Result<ValidatedManifest> {
    for (index, app) in manifest.apps.iter().enumerate() {
        if app.id.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("apps[{index}].id must not be empty"),
            ));
        }
        if !app.executable.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("apps[{index}].executable must be absolute"),
            ));
        }
        let metadata = std::fs::metadata(&app.executable).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("apps[{index}].executable is unavailable: {error}"),
            )
        })?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("apps[{index}].executable is not an executable file"),
            ));
        }
    }
    Ok(ValidatedManifest {
        apps: manifest.apps,
    })
}

/// Read, parse and validate the app manifest at `path`.
///
/// # Errors
///
/// Returns the read error, [`io::ErrorKind::InvalidInput`] for malformed
/// JSON, or any [`validate_manifest`] error.
pub(crate) fn load_manifest(path: &Path) -> io::Result<ValidatedManifest> {
    let bytes = std::fs::read(path)?;
    let manifest = serde_json::from_slice(bytes.as_slice()).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid app manifest: {error}"),
        )
    })?;
    validate_manifest(manifest)
}

/// Create private profile directories and return the startup environment.
///
/// This function does not mutate process-global state. The native executable
/// applies the returned values at its single-threaded startup boundary.
///
/// # Errors
///
/// Rejects a relative profile path and returns any error creating or
/// securing the private directories.
pub(crate) fn prepare_profile_environment(
    profile: &Path,
) -> io::Result<[(&'static str, PathBuf); 5]> {
    if !profile.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile path must be absolute",
        ));
    }
    crate::unix::private_directory(profile)?;
    let paths = [
        ("XDG_CONFIG_HOME", profile.join("config")),
        ("XDG_DATA_HOME", profile.join("data")),
        ("XDG_CACHE_HOME", profile.join("cache")),
        ("XDG_STATE_HOME", profile.join("state")),
        ("TILO_IDENTITY_ROOT", profile.join("identity")),
    ];
    for (_, path) in &paths {
        crate::unix::private_directory(path)?;
    }
    Ok(paths)
}

/// Read one newline-terminated request line, capped at `MAX_REQUEST_BYTES`.
///
/// An oversized line is consumed through its newline and reported as
/// [`Frame::TooLong`]; end of input yields any partial line, then [`Frame::Eof`].
///
/// # Errors
///
/// Returns any error from the underlying reader.
pub(crate) fn read_frame(reader: &mut impl BufRead) -> io::Result<Frame> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if too_long {
                Ok(Frame::TooLong)
            } else if line.is_empty() {
                Ok(Frame::Eof)
            } else {
                Ok(Frame::Line(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        // `position` indexes into `available`, so `position + 1` is at most its length.
        let consumed = newline.map_or(available.len(), |position| position.saturating_add(1));
        // Everything before the first newline, or the whole buffer when there is none.
        let content = available
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        if !too_long {
            if line.len().saturating_add(content.len()) > MAX_REQUEST_BYTES {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(content);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            if too_long {
                return Ok(Frame::TooLong);
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Frame::Line(line));
        }
    }
}

/// Write `reply` as one JSON line and flush it.
///
/// # Errors
///
/// Returns any serialization or write error.
pub(crate) fn write_reply(writer: &mut impl Write, reply: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, reply).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

struct OwnedChild {
    id: String,
    child: Child,
}

#[derive(Default)]
struct OwnedChildren(Vec<OwnedChild>);

impl OwnedChildren {
    fn check(&mut self) -> io::Result<()> {
        for owned in &mut self.0 {
            if let Some(status) = owned.child.try_wait()? {
                return Err(io::Error::other(format!(
                    "application {} (pid {}) exited: {status}",
                    owned.id,
                    owned.child.id()
                )));
            }
        }
        Ok(())
    }
}

impl Drop for OwnedChildren {
    fn drop(&mut self) {
        for owned in &mut self.0 {
            let _ = owned.child.kill();
            let _ = owned.child.wait();
        }
    }
}

fn log_file(profile: &Path, index: usize, app_id: &str) -> io::Result<File> {
    let logs = profile.join("logs");
    crate::unix::private_directory(&logs)?;
    let safe_id: String = app_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(logs.join(format!("{}-{index:03}-{safe_id}.log", std::process::id())))
}

fn launch_apps<S: RuntimeServices>(
    session: &S,
    profile: &Path,
    manifest: ValidatedManifest,
) -> io::Result<OwnedChildren> {
    let mut children = OwnedChildren::default();
    for (index, app) in manifest.apps.into_iter().enumerate() {
        let log = log_file(profile, index, &app.id)?;
        let mut command = Command::new(&app.executable);
        command
            .args(&app.args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let child = session.launch(&app.id, command).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to launch apps[{index}] {}: {error}", app.id),
            )
        })?;
        children.0.push(OwnedChild { id: app.id, child });
    }
    Ok(children)
}

/// What a connection's reader thread hands its serving loop.
pub(crate) enum InputEvent {
    Frame(Frame),
    Error(String),
}

/// Read `reader`'s frames on a thread of their own, so a connection's
/// serving loop writes replies while the next request has yet to arrive.
pub(crate) fn spawn_reader(
    reader: impl Read + Send + 'static,
) -> io::Result<tokio::sync::mpsc::Receiver<InputEvent>> {
    // Bounds aggregate buffered input as well as each individual request.
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    std::thread::Builder::new()
        .name("tilo-runtime-mcp-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let event = match read_frame(&mut reader) {
                    Ok(frame) => InputEvent::Frame(frame),
                    Err(error) => InputEvent::Error(error.to_string()),
                };
                let terminal =
                    matches!(event, InputEvent::Frame(Frame::Eof) | InputEvent::Error(_));
                if sender.blocking_send(event).is_err() || terminal {
                    return;
                }
            }
        })
        .map_err(|error| io::Error::other(format!("failed to start MCP reader: {error}")))?;
    Ok(receiver)
}

fn protocol_error(code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":null,"error":{"code":code,"message":message}})
}

/// A line's reply, or `None` for a notification.
type Reply = io::Result<Option<Value>>;

/// How a line is answered, relative to the lines after it.
pub(crate) enum Answer<'a> {
    /// Before the next line is read, by the connection's own thread:
    /// `initialize`, whose run id and page detail every later call of the
    /// session reads, and a daemon's own native requests.
    InOrder(std::pin::Pin<Box<dyn std::future::Future<Output = Reply> + 'a>>),
    /// On the runtime, beside the session's other requests: a `tilo_cancel`
    /// or `tilo_status` answers while the `tilo_invoke` it is about waits.
    /// The session bounds how many of these work at once.
    Overlapping(std::pin::Pin<Box<dyn std::future::Future<Output = Reply> + Send>>),
}

/// `request` answered by `server`, as an MCP request on a line-framed
/// connection is.
pub(crate) fn mcp_answer<H>(
    server: &std::sync::Arc<McpServer<H>>,
    request: Value,
) -> Answer<'static>
where
    H: tilo_actions::remote::ActionHost + Send + Sync + 'static,
{
    let server = std::sync::Arc::clone(server);
    let initialize = request.get("method").and_then(Value::as_str) == Some("initialize");
    let reply = Box::pin(async move { Ok::<_, io::Error>(server.handle(request).await) });
    if initialize {
        Answer::InOrder(reply)
    } else {
        Answer::Overlapping(reply)
    }
}

/// How often the stdio host checks its app children.
const CHECK_EVERY: Duration = Duration::from_millis(25);

/// Answer one newline-framed connection until its input ends.
///
/// `answer` answers each request; an overlapping one runs on `runtime`
/// beside the others in flight, and its reply is written, whole and on its
/// own line, as soon as it is ready. JSON-RPC ids pair replies with
/// requests, so they may come out of order. Only this loop writes, so lines
/// never interleave. At end of input the requests in flight are answered
/// before it returns. `check`, when given, runs every [`CHECK_EVERY`] and
/// ends the loop with its error.
///
/// # Errors
///
/// Returns a read, write or `check` error, or that a request's task failed.
pub(crate) fn serve_lines<'a>(
    runtime: &tilo_rt::AppRuntime,
    mut input: tokio::sync::mpsc::Receiver<InputEvent>,
    writer: &mut impl Write,
    mut answer: impl FnMut(Value) -> Answer<'a>,
    mut check: Option<&mut dyn FnMut() -> io::Result<()>>,
) -> io::Result<()> {
    let handle = runtime.handle().clone();
    runtime
        .block_on(async move {
            // Dropped with the connection, which aborts what still runs.
            let mut running = tokio::task::JoinSet::new();
            let mut open = true;
            let mut checks = tokio::time::interval(CHECK_EVERY);
            checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            while open || !running.is_empty() {
                tokio::select! {
                    event = input.recv(), if open => {
                        let request = match event {
                            Some(InputEvent::Frame(Frame::Line(bytes))) => {
                                serde_json::from_slice::<Value>(&bytes)
                            }
                            Some(InputEvent::Frame(Frame::Eof)) => {
                                open = false;
                                continue;
                            }
                            Some(InputEvent::Frame(Frame::TooLong)) => {
                                let reply = protocol_error(-32700, "request line exceeds 1 MiB");
                                write_reply(writer, &reply)?;
                                continue;
                            }
                            Some(InputEvent::Error(message)) => {
                                return Err(io::Error::new(io::ErrorKind::InvalidData, message));
                            }
                            None => {
                                return Err(io::Error::new(
                                    io::ErrorKind::BrokenPipe,
                                    "MCP reader ended without EOF",
                                ));
                            }
                        };
                        match request.map(&mut answer) {
                            Ok(Answer::InOrder(reply)) => {
                                if let Some(reply) = reply.await? {
                                    write_reply(writer, &reply)?;
                                }
                            }
                            Ok(Answer::Overlapping(reply)) => {
                                running.spawn_on(reply, &handle);
                            }
                            Err(_) => write_reply(writer, &protocol_error(-32700, "Parse error"))?,
                        }
                    }
                    Some(done) = running.join_next() => {
                        if let Some(reply) = done.map_err(io::Error::other)?? {
                            write_reply(writer, &reply)?;
                        }
                    }
                    _ = checks.tick(), if check.is_some() => {
                        if let Some(check) = check.as_mut() {
                            check()?;
                        }
                    }
                }
            }
            Ok(())
        })
        .map_err(io::Error::other)?
}

/// Own one MCP connection, one production service session and its app children.
///
/// The caller supplies the process environment before invoking this reusable
/// serving layer. This function never mutates process-global environment.
///
/// Returns `Ok(())` when the input reaches end of file.
///
/// # Errors
///
/// Returns an error when the runtime, session or action broker cannot start,
/// an app cannot be launched or exits, the input stream fails, or a reply
/// cannot be written.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the session holds the profile lock and services until the server loop ends"
)]
pub fn serve<S, R, W>(
    profile: &Path,
    manifest: ValidatedManifest,
    reader: R,
    mut writer: W,
) -> io::Result<()>
where
    S: RuntimeServices,
    R: Read + Send + 'static,
    W: Write,
{
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let session = S::start_reporting(profile, &mut |_| {})?;
    let server = session.mcp_session_factory(session.room())?()?;
    let mut children = launch_apps(&session, profile, manifest)?;
    let input = spawn_reader(reader)?;
    let server = std::sync::Arc::new(server);
    serve_lines(
        runtime,
        input,
        &mut writer,
        |request| mcp_answer(&server, request),
        Some(&mut || children.check()),
    )
}

/// Load a validated manifest and serve with the caller's configured profile environment.
///
/// # Errors
///
/// Returns any [`load_manifest`] or [`serve`] error.
pub(crate) fn run<S: RuntimeServices>(profile: &Path, manifest_path: &Path) -> io::Result<()> {
    // The complete startup configuration is parsed and validated before
    // profile services or any application process is started.
    let manifest = load_manifest(manifest_path)?;
    serve::<S, _, _>(profile, manifest, std::io::stdin(), std::io::stdout())
}

#[cfg(test)]
#[path = "mcp_process_tests.rs"]
mod tests;
