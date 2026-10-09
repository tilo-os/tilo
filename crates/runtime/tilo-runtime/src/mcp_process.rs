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
    sync::mpsc,
    time::Duration,
};

use crate::RuntimeServices;

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

enum InputEvent {
    Frame(Frame),
    Error(String),
}

fn spawn_reader(reader: impl Read + Send + 'static) -> io::Result<mpsc::Receiver<InputEvent>> {
    // Bounds aggregate buffered input as well as each individual request.
    let (sender, receiver) = mpsc::sync_channel(16);
    std::thread::Builder::new()
        .name("tilo-runtime-mcp-stdin".into())
        .spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let event = match read_frame(&mut reader) {
                    Ok(frame) => InputEvent::Frame(frame),
                    Err(error) => InputEvent::Error(error.to_string()),
                };
                let terminal =
                    matches!(event, InputEvent::Frame(Frame::Eof) | InputEvent::Error(_));
                if sender.send(event).is_err() || terminal {
                    return;
                }
            }
        })
        .map_err(|error| io::Error::other(format!("failed to start stdin reader: {error}")))?;
    Ok(receiver)
}

fn protocol_error(code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":null,"error":{"code":code,"message":message}})
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
    loop {
        children.check()?;
        let event = match input.recv_timeout(Duration::from_millis(25)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "stdin reader ended without EOF",
                ));
            }
        };
        match event {
            InputEvent::Frame(Frame::Eof) => return Ok(()),
            InputEvent::Frame(Frame::TooLong) => {
                write_reply(
                    &mut writer,
                    &protocol_error(-32700, "request line exceeds 1 MiB"),
                )?;
            }
            InputEvent::Frame(Frame::Line(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(request) => {
                    if let Some(reply) = runtime
                        .block_on(server.handle(request))
                        .map_err(io::Error::other)?
                    {
                        write_reply(&mut writer, &reply)?;
                    }
                }
                Err(_) => write_reply(&mut writer, &protocol_error(-32700, "Parse error"))?,
            },
            InputEvent::Error(message) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, message));
            }
        }
    }
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
