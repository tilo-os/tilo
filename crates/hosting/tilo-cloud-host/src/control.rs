//! The operator command surface, served by the running daemon.
//!
//! While `serve` owns a state root it keeps that Store in memory and holds its
//! file lock, so an operator command (`enrollment`, `mesh`, `agent receipt`)
//! run as another process reaches the owner
//! through `owner.sock`: a Unix socket in the owner-only state directory,
//! bound only while the daemon holds `daemon.lock`. With no daemon running the
//! same command opens the Store directly. Either way one executor runs it
//! against the one Store, so a command's effect never depends on whether a
//! daemon happened to be up.
//!
//! One exchange per connection: a length-prefixed JSON request carrying the
//! command's arguments and, for `apply`, the request file's bytes read by the
//! calling process; then one length-prefixed JSON reply.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SOCKET: &str = "owner.sock";
/// Largest request file an operator command accepts.
pub const MAX_REQUEST_FILE_BYTES: usize = 64 * 1024;
/// Largest encoded request or reply on the socket.
const MAX_FRAME: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_file: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Reply {
    Output(serde_json::Value),
    Error(String),
}

/// Whether `args` names an operator command this module executes.
pub fn is_operator_command(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("enrollment" | "mesh" | "agent")
    )
}

/// Run one operator command: through the running daemon's socket when a
/// daemon owns `root`, otherwise directly against the Store.
pub fn run(root: &Path, machine: &str, args: &[String]) -> Result<serde_json::Value> {
    let request_file = request_file(args)?;
    if daemon_running(root)? {
        return forward(root, args, request_file);
    }
    execute(root, machine, args, request_file.as_deref())
}

/// The bytes of `--request-file PATH`, read by the calling process (whose
/// user named the file), bounded before they travel anywhere.
fn request_file(args: &[String]) -> Result<Option<Vec<u8>>> {
    let Some(index) = args.iter().position(|arg| arg == "--request-file") else {
        return Ok(None);
    };
    let path = args.get(index + 1).context("--request-file needs a path")?;
    let mut bytes = Vec::new();
    use std::io::Read;
    std::fs::File::open(path)?
        .take(MAX_REQUEST_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_REQUEST_FILE_BYTES,
        "operator request too large"
    );
    Ok(Some(bytes))
}

/// Whether a daemon currently holds this root's lease.
fn daemon_running(root: &Path) -> Result<bool> {
    let path = root.join("daemon.lock");
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(advisory_lock::FileLock::try_exclusive(file).is_err())
}

#[cfg(unix)]
fn forward(
    root: &Path,
    args: &[String],
    request_file: Option<Vec<u8>>,
) -> Result<serde_json::Value> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(root.join(SOCKET))
        .context("the running Cloud host does not answer operator commands")?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
    write_frame(
        &mut stream,
        &serde_json::to_vec(&Request {
            version: 1,
            args: args.to_vec(),
            request_file,
        })?,
    )?;
    match serde_json::from_slice(&read_frame(&mut stream)?)? {
        Reply::Output(value) => Ok(value),
        Reply::Error(error) => bail!("{error}"),
    }
}

#[cfg(not(unix))]
fn forward(_: &Path, _: &[String], _: Option<Vec<u8>>) -> Result<serde_json::Value> {
    bail!("operator commands to a running Cloud host need Unix sockets")
}

fn write_frame(stream: &mut impl std::io::Write, bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= MAX_FRAME, "operator frame too large");
    stream.write_all(&u32::try_from(bytes.len())?.to_le_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()?;
    Ok(())
}

fn read_frame(stream: &mut impl std::io::Read) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(length <= MAX_FRAME, "operator frame too large");
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Execute one operator command against `root`'s Store: the daemon's
/// long-lived Store when this process owns it, otherwise one opened here.
pub(crate) fn execute(
    root: &Path,
    machine: &str,
    args: &[String],
    request_file: Option<&[u8]>,
) -> Result<serde_json::Value> {
    let now = tilo_delivery_protocol::clock::now_ns();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["agent", "receipt", "--invocation-id", invocation] => Ok(serde_json::to_value(
            crate::operator::agent_receipt(root, machine, invocation)?,
        )?),
        ["enrollment", "inspect"] => Ok(serde_json::to_value(
            crate::Store::open(root, machine)?.inspect(now)?,
        )?),
        ["enrollment", "apply", "--request-file", _] => {
            let request = serde_json::from_slice(request_file.context("request file missing")?)?;
            crate::Store::open(root, machine)?.apply(&request, now)
        }
        ["mesh", "inspect"] => Ok(serde_json::to_value(
            crate::Store::open(root, machine)?.mesh_inspect(now)?,
        )?),
        ["mesh", "status"] => crate::Store::open(root, machine)?.mesh_status(now),
        ["mesh", "apply", "--request-file", _] => {
            let request = serde_json::from_slice(request_file.context("request file missing")?)?;
            crate::Store::open(root, machine)?.mesh_apply(&request, now)
        }
        _ => bail!(
            "usage: tilo-cloud-host serve [--https-config PATH --ready-file PATH] | agent receipt --invocation-id UUID | enrollment inspect | enrollment apply --request-file PATH | mesh inspect | mesh status | mesh apply --request-file PATH"
        ),
    }
}

/// The daemon's operator socket; dropping it stops answering and removes it.
pub(crate) struct Served {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Answer operator commands for `root` while the daemon holds its lease.
#[cfg(unix)]
pub(crate) fn serve(root: &Path, machine: &str) -> Result<Served> {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join(SOCKET);
    // The caller holds `daemon.lock`: any socket file left here is a previous
    // daemon's, and nothing else may bind this name.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let (root, machine) = (root.to_owned(), machine.to_owned());
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let (root, machine) = (root.clone(), machine.clone());
            tokio::spawn(async move {
                if let Err(error) = answer(stream, root, machine).await {
                    tracing::debug!(%error, "operator command exchange failed");
                }
            });
        }
    });
    Ok(Served { path, task })
}

#[cfg(unix)]
async fn answer(stream: tokio::net::UnixStream, root: PathBuf, machine: String) -> Result<()> {
    // Only the daemon's own user reaches an owner-only directory, but check
    // the peer anyway: the socket file mode alone is not the boundary.
    let peer = stream.peer_cred()?;
    ensure!(
        peer.uid() == unsafe { libc::geteuid() },
        "operator command from another user"
    );
    let mut stream = stream.into_std()?;
    stream.set_nonblocking(false)?;
    tokio::task::spawn_blocking(move || -> Result<()> {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        let request: Request = serde_json::from_slice(&read_frame(&mut stream)?)?;
        ensure!(request.version == 1, "unsupported operator request");
        let reply = match execute(
            &root,
            &machine,
            &request.args,
            request.request_file.as_deref(),
        ) {
            Ok(value) => Reply::Output(value),
            Err(error) => Reply::Error(format!("{error:#}")),
        };
        write_frame(&mut stream, &serde_json::to_vec(&reply)?)
    })
    .await?
}

#[cfg(not(unix))]
pub(crate) fn serve(_: &Path, _: &str) -> Result<Served> {
    bail!("the Cloud host operator socket needs Unix")
}
