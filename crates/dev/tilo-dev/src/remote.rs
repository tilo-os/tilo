//! The SSH transport to a VM's tilo-devd.

use std::io::{BufReader, Read as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use tilo_dev_protocol::{Call, Client, ClientError, HelloResult};

use crate::error::CliError;
use crate::hypervisor::{CommandLine, SshEndpoint};

pub const DEFAULT_DAEMON: &str = "tilo-devd rpc";

/// `ssh` arguments for a non-interactive, key-only connection.
pub fn ssh_args(endpoint: &SshEndpoint, key: Option<&str>, interactive: bool) -> Vec<String> {
    let mut args: Vec<String> = [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ServerAliveInterval=15",
    ]
    .map(str::to_owned)
    .to_vec();
    if interactive {
        args.push("-t".into());
    }
    if let Some(key) = key {
        args.extend(["-i".into(), key.to_owned()]);
    }
    args.extend([
        "-p".into(),
        endpoint.port.to_string(),
        format!("{}@{}", endpoint.user, endpoint.host),
    ]);
    args
}

/// `TILO_DEV_SSH` replaces the ssh program, e.g. with a wrapper that adds
/// options or a local stand-in for testing.
pub fn ssh_command(
    endpoint: &SshEndpoint,
    key: Option<&str>,
    remote: &[String],
    interactive: bool,
) -> CommandLine {
    let program = std::env::var("TILO_DEV_SSH").unwrap_or_else(|_| "ssh".into());
    let mut command = CommandLine::new(&program, &[]);
    command.args = ssh_args(endpoint, key, interactive);
    command.args.extend(remote.iter().cloned());
    command
}

pub struct Remote {
    child: Child,
    client: Client<BufReader<ChildStdout>, ChildStdin>,
    pub hello: HelloResult,
}

impl Remote {
    /// Open `ssh <vm> tilo-devd rpc` and complete the handshake.
    ///
    /// # Errors
    /// Unreachable VMs, a missing daemon and protocol mismatches, each with
    /// its own hint; `debug` lists the hypervisor's fallback commands.
    pub fn connect(
        vm: &str,
        endpoint: &SshEndpoint,
        key: Option<&str>,
        daemon: Option<&str>,
        debug: Vec<String>,
    ) -> Result<Self, CliError> {
        let remote: Vec<String> = daemon
            .unwrap_or(DEFAULT_DAEMON)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let line = ssh_command(endpoint, key, &remote, false);
        let mut child = Command::new(&line.program)
            .args(&line.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    CliError::ToolMissing {
                        tool: "ssh".into(),
                        purpose: "reach the VM".into(),
                    }
                } else {
                    CliError::io("start ssh")(e)
                }
            })?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            return Err(CliError::io("start ssh")(std::io::Error::other("no pipes")));
        };
        let mut client = Client::new(BufReader::new(stdout), stdin);
        match client.hello(env!("CARGO_PKG_VERSION")) {
            Ok(hello) => Ok(Self {
                child,
                client,
                hello,
            }),
            Err(ClientError::Io(_) | ClientError::Closed) => {
                let _ = child.kill();
                let status = child.wait().ok().and_then(|s| s.code());
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                let stderr = stderr.trim().to_owned();
                // 127: the remote shell found no such command.
                if status == Some(127)
                    || stderr.contains("command not found")
                    || stderr.contains("No such file")
                {
                    Err(CliError::DaemonMissing { vm: vm.into() })
                } else {
                    let detail = if stderr.is_empty() {
                        "connection closed".into()
                    } else {
                        stderr
                    };
                    Err(CliError::Unreachable {
                        vm: vm.into(),
                        detail,
                        debug,
                    })
                }
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error.into())
            }
        }
    }

    /// # Errors
    /// Daemon-reported and transport errors.
    pub fn call<C: Call>(&mut self, params: C) -> Result<C::Output, CliError> {
        self.client.call(params).map_err(CliError::from)
    }

    /// # Errors
    /// Read or transport failures.
    pub fn upload(&mut self, bytes: impl std::io::Read) -> Result<String, CliError> {
        self.client.upload(bytes).map_err(CliError::from)
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // Closing stdin ends the daemon's loop; reap ssh so no zombie remains.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_command_is_key_only_and_bounded() {
        let endpoint = SshEndpoint {
            user: "root".into(),
            host: "127.0.0.1".into(),
            port: 2223,
        };
        let line = ssh_command(
            &endpoint,
            Some("/k/id"),
            &["tilo-devd".into(), "rpc".into()],
            false,
        );
        assert_eq!(
            line.display(),
            "ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new -o ServerAliveInterval=15 \
             -i /k/id -p 2223 root@127.0.0.1 tilo-devd rpc"
        );
        let interactive = ssh_command(&endpoint, None, &[], true);
        assert!(interactive.args.contains(&"-t".to_string()));
    }
}
