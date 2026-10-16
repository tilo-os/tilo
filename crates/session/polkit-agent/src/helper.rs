//! Starting `polkit-agent-helper-1`, the privileged program that runs PAM
//! for an agent.
//!
//! Current polkit runs it socket-activated behind
//! `/run/polkit/agent-helper.socket` (user name, then cookie, one per
//! line); older installs ship it setuid and take the user name as its
//! argument and the cookie on stdin. Either way the conversation that
//! follows is the same line protocol ([`super::HelperEvent`]).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncWriteExt as _, BufReader};

use super::{HelperIo, HelperSpawner};

pub const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";
/// Where distributions install the setuid helper.
pub const HELPER_PATHS: &[&str] = &[
    "/usr/lib/polkit-1/polkit-agent-helper-1",
    "/usr/libexec/polkit-agent-helper-1",
    "/usr/lib/policykit-1/polkit-agent-helper-1",
];

fn valid_line(value: &str) -> bool {
    !value.is_empty() && !value.contains(['\n', '\r', '\0'])
}

/// The system's helper, by socket when polkit provides one.
#[derive(Debug, Clone)]
pub struct SystemHelper {
    socket: PathBuf,
    executables: Vec<PathBuf>,
}

impl Default for SystemHelper {
    fn default() -> Self {
        Self {
            socket: PathBuf::from(HELPER_SOCKET),
            executables: HELPER_PATHS.iter().map(PathBuf::from).collect(),
        }
    }
}

impl SystemHelper {
    async fn over_socket(socket: &Path, user: &str, cookie: &str) -> std::io::Result<HelperIo> {
        let stream = tokio::net::UnixStream::connect(socket).await?;
        let (read, mut write) = stream.into_split();
        write
            .write_all(format!("{user}\n{cookie}\n").as_bytes())
            .await?;
        write.flush().await?;
        Ok(HelperIo {
            output: Box::new(BufReader::new(read)),
            input: Box::new(write),
        })
    }

    async fn executable(path: &Path, user: &str, cookie: &str) -> std::io::Result<HelperIo> {
        let mut child = tokio::process::Command::new(path)
            .arg(user)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("helper has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("helper has no stdout"))?;
        stdin.write_all(format!("{cookie}\n").as_bytes()).await?;
        stdin.flush().await?;
        // The child is reaped by tokio once it exits; dropping the pipes
        // (a cancelled request) makes it exit.
        drop(tokio::spawn(async move {
            let _ = child.wait().await;
        }));
        Ok(HelperIo {
            output: Box::new(BufReader::new(stdout)),
            input: Box::new(stdin),
        })
    }
}

impl HelperSpawner for SystemHelper {
    async fn spawn(&self, user: &str, cookie: &str) -> std::io::Result<HelperIo> {
        if !valid_line(user) || !valid_line(cookie) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "user and cookie must be single lines",
            ));
        }
        if self.socket.exists() {
            match Self::over_socket(&self.socket, user, cookie).await {
                Ok(io) => return Ok(io),
                Err(error) => tracing::debug!(
                    target: "shell::polkit",
                    %error,
                    "helper socket refused; trying the setuid helper"
                ),
            }
        }
        let path = self
            .executables
            .iter()
            .find(|path| path.is_file())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "polkit-agent-helper-1 is not installed",
                )
            })?;
        Self::executable(path, user, cookie).await
    }
}
