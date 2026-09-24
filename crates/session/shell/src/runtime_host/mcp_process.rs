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

use super::{Session, mcp::McpServer};

pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

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
pub enum Frame {
    Line(Vec<u8>),
    TooLong,
    Eof,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub profile: PathBuf,
    pub apps: PathBuf,
}

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<Args> {
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

pub fn load_manifest(path: &Path) -> io::Result<ValidatedManifest> {
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
pub fn prepare_profile_environment(profile: &Path) -> io::Result<[(&'static str, PathBuf); 6]> {
    if !profile.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile path must be absolute",
        ));
    }
    super::unix::private_directory(profile)?;
    let paths = [
        ("XDG_CONFIG_HOME", profile.join("config")),
        ("XDG_DATA_HOME", profile.join("data")),
        ("XDG_CACHE_HOME", profile.join("cache")),
        ("XDG_STATE_HOME", profile.join("state")),
        ("TILO_IDENTITY_ROOT", profile.join("identity")),
        ("TILO_BLOB_STORE", profile.join("blobs")),
    ];
    for (_, path) in &paths {
        super::unix::private_directory(path)?;
    }
    Ok(paths)
}

pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Frame> {
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
        let consumed = newline.map_or(available.len(), |position| position + 1);
        let content_len = newline.unwrap_or(available.len());
        if !too_long {
            if line.len().saturating_add(content_len) > MAX_REQUEST_BYTES {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..content_len]);
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

pub fn write_reply(writer: &mut impl Write, reply: &Value) -> io::Result<()> {
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
    super::unix::private_directory(&logs)?;
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

fn launch_apps(
    session: &Session,
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
        .name("qos-runtime-mcp-stdin".into())
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
pub fn serve<R, W>(
    profile: &Path,
    manifest: ValidatedManifest,
    reader: R,
    mut writer: W,
) -> io::Result<()>
where
    R: Read + Send + 'static,
    W: Write,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;
    let session = Session::start(profile)?;
    let broker = session
        .broker
        .app_action_broker()
        .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))?;
    let mut server = McpServer::new(broker, session.room.clone(), session.actor.clone());
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
                    if let Some(reply) = runtime.block_on(server.handle(request)) {
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
pub fn run(profile: &Path, manifest_path: &Path) -> io::Result<()> {
    // The complete startup configuration is parsed and validated before
    // profile services or any application process is started.
    let manifest = load_manifest(manifest_path)?;
    serve(profile, manifest, std::io::stdin(), std::io::stdout())
}

#[cfg(test)]
#[path = "mcp_process_tests.rs"]
mod tests;
