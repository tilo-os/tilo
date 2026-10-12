//! Legacy launches from an environment that runs on a relay worker.
//!
//! On a device, a legacy app's shim runs `tilo-legacy` on the host: the
//! user's own legacy VM. On a relay worker there is no such VM for the
//! member; the worker asks the relay, which routes the launch to a worker
//! with KVM and streams the app's windows to the member's devices. The
//! worker that started the member's supervisor names a socket in
//! [`LEGACY_LAUNCHES_ENV`]; the broker sends each launch there instead.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The socket a relay worker answers its member's legacy launches on.
pub const LEGACY_LAUNCHES_ENV: &str = "TILO_USH_LEGACY_LAUNCHES";

/// Longest request or reply line.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// The broker → worker request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub app: String,
    pub args: Vec<String>,
    pub cwd: String,
}

/// The worker's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchReply {
    /// Routed; the windows reach the member's devices.
    Launched { launch: String },
    /// Not launched, and why.
    Refused { detail: String },
}

/// The socket [`LEGACY_LAUNCHES_ENV`] names, when this runs on a relay.
#[must_use]
pub fn socket_from_env() -> Option<PathBuf> {
    std::env::var_os(LEGACY_LAUNCHES_ENV)
        .filter(|socket| !socket.is_empty())
        .map(PathBuf::from)
}

/// Ask the worker at `socket` to launch `request`.
///
/// # Errors
///
/// Returns an error when the worker cannot be reached or answers nonsense.
pub fn request(socket: &Path, request: &LaunchRequest) -> std::io::Result<LaunchReply> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream)
        .take(u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX))
        .read_line(&mut reply)?;
    serde_json::from_str(&reply).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;

    #[test]
    fn a_launch_round_trips_through_the_workers_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("legacy.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let asked: LaunchRequest = serde_json::from_str(&line).unwrap();
            let mut reply = serde_json::to_vec(&LaunchReply::Launched {
                launch: "00ff".into(),
            })
            .unwrap();
            reply.push(b'\n');
            (&stream).write_all(&reply).unwrap();
            asked
        });
        let asked = LaunchRequest {
            app: "code".into(),
            args: vec![".".into()],
            cwd: "/home/you/src".into(),
        };
        let reply = request(&socket, &asked).unwrap();
        assert_eq!(worker.join().unwrap(), asked);
        assert_eq!(
            reply,
            LaunchReply::Launched {
                launch: "00ff".into()
            }
        );
    }
}
