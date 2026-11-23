//! A Windows program's filtered network, in its own launch.
//!
//! A launch granted specific network destinations gets a sandbox with an empty network
//! namespace and one socket out, brokered by a SOCKS5/HTTP proxy ([`crate::egress_proxy`]). The
//! proxy lives in a process of the launch's own, started right before the sandbox as its child:
//! it stays while the desktop restarts in place, as the program does, and ends when the program
//! ends, is quit, or is ended with its group at logout. A proxy in the session would go with the
//! desktop and leave a kept program without a network, and nobody to restore or revoke it.
//!
//! The policy is the session's: it prepares the grants at the launch, and updates them while
//! the program lives through a control socket in the keeper's directory, which the desktop that
//! adopts the program finds again after a restart. Replacing the policy cuts every connection
//! that was open under the old one, so taking a grant back ends the traffic it allowed.
//!
//! The sandbox reaches only the proxy's socket, bound into it. The policy file and the control
//! socket live in directories the sandbox has no path to, under the program's keeper; only the
//! session's own user may connect to the control socket, as to the keeper's. A proxy that dies
//! leaves the program without a route, never with an unfiltered one.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::{
    FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::egress_proxy::{EgressPolicy, EgressProxy, ProxyError};

/// The file the launch's policy is prepared in, one grant spelling per line.
const POLICY: &str = "policy";
/// The proxy's socket in its directory: the sandbox's one route out.
const PROXY: &str = "proxy.sock";
/// The name of the control socket in a kept program's keeper directory, which every desktop that
/// shows the program finds again.
pub const CONTROL: &str = "network-proxy-control";
/// How long the proxy may take to listen before the launch refuses to start.
const START_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How long the session waits for the proxy's answer to a policy it sets: the desktop's loop
/// waits that long too, so it stays short.
const CONTROL_TIMEOUT: Duration = Duration::from_millis(500);
/// Largest JSON line exchanged over the network proxy's control socket, in either direction.
pub const MAX_CONTROL_LINE_BYTES: usize = 64 * 1024;

/// Why a network proxy could not be prepared, started, or reached.
#[derive(Debug, thiserror::Error)]
pub enum NetworkProxyError {
    #[error("the network proxy's file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not a private directory of this user")]
    UnsafePath { path: PathBuf },
    #[error("the network proxy did not start: {0}")]
    NotStarted(String),
    #[error("the network proxy's control socket {path}: {source}")]
    Control {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the program did not start: {0}")]
    Exec(#[source] io::Error),
    #[error("the network proxy refused the policy: {0}")]
    Refused(String),
}

impl NetworkProxyError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// What the session sends the proxy: the grant spellings the program may reach now. None is a
/// policy of no destinations: the proxy keeps going, and every connection is refused.
#[derive(Debug, Serialize, Deserialize)]
struct Update {
    allowed: Vec<String>,
}

/// The proxy's answer to an [`Update`].
#[derive(Debug, Serialize, Deserialize)]
struct Answer {
    allowed: Option<usize>,
    error: Option<String>,
}

/// Prepare `dir` with `policy`: the grants the proxy enforces first, one spelling per line. The
/// directory is private to the session's user, as is the file.
///
/// # Errors
/// The directory could not be made private, or the policy could not be written.
pub fn prepare(dir: &Path, policy: &EgressPolicy) -> Result<(), NetworkProxyError> {
    private_dir(dir)?;
    let path = dir.join(POLICY);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|source| NetworkProxyError::io(&path, source))?;
    for name in policy.describe() {
        writeln!(file, "{name}").map_err(|source| NetworkProxyError::io(&path, source))?;
    }
    file.sync_all()
        .map_err(|source| NetworkProxyError::io(&path, source))
}

/// The control socket in `dir`, for a launch whose program is not kept and has no desktop to
/// adopt it.
#[must_use]
pub fn control_beside(dir: &Path) -> PathBuf {
    dir.join("control")
}

/// Start the proxy `dir` names and become `command`, whose process it ends with: a Windows
/// program's sandbox. The proxy listens before the sandbox starts, so the socket it binds into
/// itself is there.
///
/// # Errors
/// The proxy did not start (the launch is refused rather than started unfiltered), or `command`
/// could not be executed.
pub fn exec_with(
    dir: &Path,
    control: &Path,
    command: &[std::ffi::OsString],
) -> Result<std::convert::Infallible, NetworkProxyError> {
    let Some((program, args)) = command.split_first() else {
        return Err(NetworkProxyError::NotStarted("no command".to_owned()));
    };
    let exe = std::env::current_exe().map_err(|source| {
        NetworkProxyError::NotStarted(format!("this launcher's own program: {source}"))
    })?;
    let mut proxy = Command::new(exe);
    proxy
        .args(["windows-network-serve", "--dir"])
        .arg(dir)
        .arg("--control")
        .arg(control)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: prctl and close_range are async-signal-safe and the closure captures nothing.
    unsafe {
        proxy.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            // The sandbox's own descriptors stay with the sandbox.
            if libc::syscall(
                libc::SYS_close_range,
                3,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = proxy
        .spawn()
        .map_err(|source| NetworkProxyError::NotStarted(source.to_string()))?;
    if let Err(error) = wait_for_proxy(dir, control, &mut child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    // Left going: it ends with this process, which goes on as the program it serves.
    drop(child);
    Err(NetworkProxyError::Exec(
        Command::new(program).args(args).exec(),
    ))
}

/// Serve one launch's network proxy in `dir`, answering the session on `control`, until the
/// process is ended. The proxy enforces its policy whatever the desktop is doing; a session that
/// is gone changes nothing, and a policy it set stays in force.
///
/// # Errors
/// The policy could not be read, or the sockets could not be bound. The proxy never starts to
/// enforce something other than the prepared policy.
pub fn serve(dir: &Path, control: &Path) -> Result<std::convert::Infallible, NetworkProxyError> {
    let policy = read_policy(dir)?;
    let listener = bind_control(control)?;
    let proxy = EgressProxy::start(&dir.join(PROXY), policy)?;
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(NetworkProxyError::Control {
                    path: control.to_path_buf(),
                    source,
                });
            }
        };
        admit(&stream, &proxy);
    }
}

/// Set the policy the proxy at `control` enforces from now on, cutting every connection that was
/// open under the old one.
///
/// # Errors
/// The proxy could not be reached, or refused the policy.
pub fn set_policy(control: &Path, policy: &EgressPolicy) -> Result<(), NetworkProxyError> {
    let stream = tilo_files::native::socket::connect(control).map_err(|source| {
        NetworkProxyError::Control {
            path: control.to_path_buf(),
            source,
        }
    })?;
    stream
        .set_read_timeout(Some(CONTROL_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(CONTROL_TIMEOUT)))
        .map_err(|source| NetworkProxyError::Control {
            path: control.to_path_buf(),
            source,
        })?;
    let update = Update {
        allowed: policy.describe(),
    };
    let mut line = serde_json::to_vec(&update)
        .map_err(|source| NetworkProxyError::Refused(source.to_string()))?;
    line.push(b'\n');
    (&stream)
        .write_all(&line)
        .map_err(|source| NetworkProxyError::Control {
            path: control.to_path_buf(),
            source,
        })?;
    let mut reader = BufReader::new(&stream);
    let answer = wire_frame::read_line(&mut reader, MAX_CONTROL_LINE_BYTES)
        .map_err(|source| NetworkProxyError::Control {
            path: control.to_path_buf(),
            source: source.into(),
        })?
        .ok_or_else(|| NetworkProxyError::Control {
            path: control.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the network proxy closed its control socket without answering",
            ),
        })?;
    let answer: Answer = serde_json::from_slice(&answer)
        .map_err(|source| NetworkProxyError::Refused(source.to_string()))?;
    match (answer.error, answer.allowed) {
        (Some(error), _) => Err(NetworkProxyError::Refused(error)),
        (None, allowed) => {
            tracing::info!(
                allowed = allowed.unwrap_or_default(),
                "a kept program's network policy was set"
            );
            Ok(())
        }
    }
}

/// Answer one control connection, when it comes from the session's own user.
fn admit(stream: &std::os::unix::net::UnixStream, proxy: &EgressProxy) {
    match peer_uid(stream) {
        Ok(uid) if uid == rustix::process::geteuid().as_raw() => {}
        Ok(_) => {
            tracing::warn!("refused a network proxy control connection of another user");
            return;
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "refused a network proxy control connection whose peer is unknown"
            );
            return;
        }
    }
    let mut answer = Answer {
        allowed: None,
        error: None,
    };
    let mut line = String::new();
    // A session that stopped halfway must not hold the next one's update: one line, briefly.
    if stream.set_read_timeout(Some(CONTROL_TIMEOUT)).is_err() {
        return;
    }
    let mut reader =
        BufReader::new(stream).take(u64::try_from(MAX_CONTROL_LINE_BYTES).unwrap_or(u64::MAX));
    let outcome = reader
        .read_line(&mut line)
        .and_then(|_| serde_json::from_str::<Update>(&line).map_err(io::Error::other));
    match outcome {
        Ok(update) => {
            let policy = EgressPolicy::from_grants(update.allowed.iter().map(String::as_str));
            answer.allowed = Some(policy.describe().len());
            proxy.set_policy(policy);
        }
        Err(error) => answer.error = Some(error.to_string()),
    }
    if let Ok(mut encoded) = serde_json::to_vec(&answer) {
        encoded.push(b'\n');
        let mut out = stream;
        let _ = out.write_all(&encoded);
    }
}

fn peer_uid(stream: &std::os::unix::net::UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd as _;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    // SAFETY: getsockopt writes at most `len` bytes into `credentials`, a ucred it owns.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut len,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

/// Bind the control socket at `control`, in a directory only this user may enter.
fn bind_control(control: &Path) -> Result<std::os::unix::net::UnixListener, NetworkProxyError> {
    let Some(parent) = control.parent() else {
        return Err(NetworkProxyError::UnsafePath {
            path: control.to_path_buf(),
        });
    };
    let metadata = fs::symlink_metadata(parent).map_err(|source| NetworkProxyError::Control {
        path: control.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(NetworkProxyError::UnsafePath {
            path: control.to_path_buf(),
        });
    }
    // A socket left by a proxy that was killed would make bind() fail.
    let _ = fs::remove_file(control);
    let listener =
        tilo_files::native::socket::bind(control).map_err(|source| NetworkProxyError::Control {
            path: control.to_path_buf(),
            source,
        })?;
    fs::set_permissions(control, fs::Permissions::from_mode(0o600)).map_err(|source| {
        NetworkProxyError::Control {
            path: control.to_path_buf(),
            source,
        }
    })?;
    Ok(listener)
}

/// Read the policy prepared in `dir`: the grants the program may reach.
fn read_policy(dir: &Path) -> Result<EgressPolicy, NetworkProxyError> {
    let path = dir.join(POLICY);
    let text = fs::read_to_string(&path).map_err(|source| NetworkProxyError::io(&path, source))?;
    let names: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    Ok(EgressPolicy::from_grants(names))
}

/// Make `dir` a private directory of this user, or refuse one that is not.
fn private_dir(dir: &Path) -> Result<(), NetworkProxyError> {
    fs::create_dir_all(dir).map_err(|source| NetworkProxyError::io(dir, source))?;
    let metadata =
        fs::symlink_metadata(dir).map_err(|source| NetworkProxyError::io(dir, source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(NetworkProxyError::UnsafePath {
            path: dir.to_path_buf(),
        });
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|source| NetworkProxyError::io(dir, source))?;
    Ok(())
}

/// Wait until the proxy listens on both sockets, or ended without doing so.
fn wait_for_proxy(dir: &Path, control: &Path, child: &mut Child) -> Result<(), NetworkProxyError> {
    let socket = dir.join(PROXY);
    let deadline = Instant::now()
        .checked_add(START_TIMEOUT)
        .ok_or_else(|| NetworkProxyError::NotStarted("the proxy's clock".to_owned()))?;
    loop {
        let both = [&socket, control].into_iter().all(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
        });
        if both {
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(NetworkProxyError::NotStarted(format!(
                    "it ended with {status}"
                )));
            }
            Ok(None) => {}
            Err(source) => {
                return Err(NetworkProxyError::NotStarted(source.to_string()));
            }
        }
        if Instant::now() >= deadline {
            return Err(NetworkProxyError::NotStarted(format!(
                "no socket at {} or {}",
                socket.display(),
                control.display()
            )));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

impl From<ProxyError> for NetworkProxyError {
    fn from(error: ProxyError) -> Self {
        Self::NotStarted(error.to_string())
    }
}
