//! The transports to a VM's tilo-vmd: SSH, and for a device beyond the
//! LAN, the peer network through `tilo-device-admin dial`.

use std::io::{BufReader, Read as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use tilo_vm_protocol::{Call, Client, ClientError, HelloResult};

use crate::error::CliError;
use crate::hypervisor::{CommandLine, SshEndpoint};

mod keepawake;

pub const DEFAULT_DAEMON: &str = "tilo-vmd rpc";

/// The VM's own `known_hosts`, beside its key. Local VMs all answer on
/// 127.0.0.1 with forwarded ports, and a recreated VM has new host keys, so
/// one shared `~/.ssh/known_hosts` entry per port would refuse the next VM.
pub fn known_hosts_path(key: &str) -> String {
    format!("{key}.known_hosts")
}

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
        args.extend([
            "-o".into(),
            format!("UserKnownHostsFile={}", known_hosts_path(key)),
            "-i".into(),
            key.to_owned(),
        ]);
    }
    args.extend([
        "-p".into(),
        endpoint.port.to_string(),
        format!("{}@{}", endpoint.user, endpoint.host),
    ]);
    args
}

/// `TILO_VM_SSH` replaces the ssh program, e.g. with a wrapper that adds
/// options or a local stand-in for testing.
///
/// The remote command always runs as root: see [`as_root`].
pub fn ssh_command(
    endpoint: &SshEndpoint,
    key: Option<&str>,
    remote: &[String],
    interactive: bool,
) -> CommandLine {
    let mut command = ssh_transport(endpoint, key, interactive);
    command.args.extend(as_root(endpoint, remote));
    command
}

/// `ssh` to the endpoint without a remote command: the transport other
/// tools (rsync) put their own command after.
pub fn ssh_transport(endpoint: &SshEndpoint, key: Option<&str>, interactive: bool) -> CommandLine {
    let program = std::env::var("TILO_VM_SSH").unwrap_or_else(|_| "ssh".into());
    let mut command = CommandLine::new(&program, &[]);
    command.args = ssh_args(endpoint, key, interactive);
    command
}

/// Everything tilo-vm runs remotely runs as root (tilo-vmd manages users,
/// units, sysext and seats; bootc and the VM lock are root's). A VM is
/// reached as root with its own key. A device usually lets only its owner's
/// account in over SSH (the installer's dev SSH setup refuses root), so
/// there the command goes through `sudo -n`, which `vm add-device` checks.
/// ssh hands the remote words to the login shell joined by spaces, so the
/// same line runs as `sh -c` under sudo; with no command, a root login shell.
pub fn as_root(endpoint: &SshEndpoint, remote: &[String]) -> Vec<String> {
    if endpoint.user == "root" {
        return remote.to_vec();
    }
    if remote.is_empty() {
        return vec!["sudo".into(), "-i".into()];
    }
    vec![
        "sudo".into(),
        "-n".into(),
        "--".into(),
        "sh".into(),
        "-c".into(),
        crate::vmbuild::sh_quote(&remote.join(" ")),
    ]
}

/// rsync's `--rsync-path` (a shell line rsync appends its server arguments
/// to) as root, like [`as_root`].
pub fn rsync_path_as_root(endpoint: &SshEndpoint, line: &str) -> String {
    if endpoint.user == "root" {
        return line.to_owned();
    }
    // The appended arguments become "$@" of the rsync at the line's end.
    let line = line
        .strip_suffix("rsync")
        .map_or_else(|| line.to_owned(), |head| format!("{head}rsync \"$@\""));
    format!("sudo -n -- sh -c {} rsync", crate::vmbuild::sh_quote(&line))
}

/// Why sudo refused to run a command without a password, from its stderr.
pub fn sudo_refusal(stderr: &str) -> Option<String> {
    stderr.lines().map(str::trim).find_map(|line| {
        let rest = line.strip_prefix("sudo: ")?;
        (rest.contains("password is required")
            || rest.contains("a terminal is required")
            || rest.contains("not in the sudoers")
            || rest.contains("not allowed to execute"))
        .then(|| rest.to_owned())
    })
}

pub struct Remote {
    child: Child,
    /// `None` only while dropping: closing its stdin ends the session.
    client: Option<Client<BufReader<ChildStdout>, ChildStdin>>,
    pub hello: HelloResult,
    /// Over the peer network: the dial helper closes the connection itself
    /// once its stdin ends, so the device's session ends at once.
    peer: bool,
}

impl Remote {
    /// Renew the existing automatic sleep lease while a builder is running.
    /// Explicit longer holds are preserved by the normal hello handshake.
    ///
    /// # Errors
    /// Worker startup, handshake, lease and transport failures. The inner
    /// result reports a renewal failure after the owned build has finished.
    pub(crate) fn keep_awake_while<T>(
        &mut self,
        build: impl FnOnce() -> T,
    ) -> Result<(T, Result<(), CliError>), CliError> {
        keepawake::during(
            std::time::Duration::from_secs(60),
            || self.renew_sleep_lease(std::time::Duration::from_secs(10)),
            build,
        )
    }

    fn renew_sleep_lease(&mut self, timeout: std::time::Duration) -> Result<(), CliError> {
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| CliError::from(ClientError::Closed))?;
        let hello = keepawake::hello(client, &mut self.child, timeout)?;
        let lease = hello.awake.as_ref().ok_or_else(|| {
            CliError::io("renew the device sleep lease")(std::io::Error::other(
                "the daemon did not report its sleep lease",
            ))
        })?;
        if !lease.held {
            return Err(CliError::io("renew the device sleep lease")(
                std::io::Error::other(
                    lease
                        .problem
                        .as_deref()
                        .unwrap_or("the sleep lock is not held"),
                ),
            ));
        }
        self.hello = hello;
        Ok(())
    }

    /// Open `ssh <vm> tilo-vmd rpc` and complete the handshake.
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
        // SSH stdio is byte-transparent; the audited peer bridge stays JSON-line oriented.
        client.enable_binary_uploads();
        match client.hello(env!("CARGO_PKG_VERSION")) {
            Ok(hello) => Ok(Self {
                child,
                client: Some(client),
                hello,
                peer: false,
            }),
            Err(ClientError::Io(_) | ClientError::Closed) => {
                let _ = child.kill();
                let status = child.wait().ok().and_then(|s| s.code());
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                let stderr = stderr.trim().to_owned();
                if let Some(detail) = sudo_refusal(&stderr) {
                    return Err(CliError::SudoRefused {
                        vm: vm.into(),
                        ssh: endpoint.ssh_spec(),
                        user: endpoint.user.clone(),
                        detail,
                    });
                }
                // 127: the remote shell found no such command.
                if status == Some(127)
                    || stderr.contains("command not found")
                    || stderr.contains("No such file")
                {
                    Err(CliError::DaemonMissing {
                        vm: vm.into(),
                        bootc: None,
                    })
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

    /// Open tilo-vmd's RPC over the peer network: `line` runs
    /// `tilo-device-admin dial`, whose first line names the device and the
    /// route before the request lines start.
    ///
    /// # Errors
    /// A missing dial helper, a dial that fails (with its exit status and
    /// what it said), or a protocol mismatch.
    pub fn over_peer(vm: &str, line: &CommandLine) -> Result<(Self, PeerSession), CliError> {
        let mut child = Command::new(&line.program)
            .args(&line.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    peer_tool_missing()
                } else {
                    CliError::io("start tilo-device-admin")(e)
                }
            })?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            return Err(CliError::io("start tilo-device-admin")(
                std::io::Error::other("no pipes"),
            ));
        };
        let mut stdout = BufReader::new(stdout);
        let mut first = String::new();
        let session = match std::io::BufRead::read_line(&mut stdout, &mut first) {
            Ok(n) if n > 0 => serde_json::from_str::<PeerPreamble>(&first)
                .map(|preamble| preamble.tilo_device_admin)
                .ok(),
            _ => None,
        };
        let Some(session) = session else {
            drop(stdin);
            return Err(peer_failure(vm, &mut child));
        };
        let mut client = Client::new(stdout, stdin);
        match client.hello(env!("CARGO_PKG_VERSION")) {
            Ok(hello) => Ok((
                Self {
                    child,
                    client: Some(client),
                    hello,
                    peer: true,
                },
                session,
            )),
            Err(ClientError::Io(_) | ClientError::Closed) => {
                drop(client);
                Err(peer_failure(vm, &mut child))
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
        self.client
            .as_mut()
            .ok_or_else(|| CliError::from(ClientError::Closed))?
            .call(params)
            .map_err(CliError::from)
    }

    /// # Errors
    /// Read or transport failures.
    pub fn upload(&mut self, bytes: impl std::io::Read) -> Result<String, CliError> {
        self.client
            .as_mut()
            .ok_or_else(|| CliError::from(ClientError::Closed))?
            .upload(bytes)
            .map_err(CliError::from)
    }
}

/// What a peer-network session said about itself before its first request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct PeerSession {
    /// The device id that proved itself, hex.
    pub device: String,
    pub label: String,
    /// `relay` or `direct`.
    pub route: String,
    /// The relay's URL or the device's address.
    pub via: String,
    /// The session's number in the device's journal.
    pub session: u64,
}

impl PeerSession {
    /// `relay <url>` or `direct <address>`.
    pub fn describe(&self) -> String {
        format!("{} {}", self.route, self.via).trim().to_owned()
    }
}

#[derive(serde::Deserialize)]
struct PeerPreamble {
    tilo_device_admin: PeerSession,
}

/// `TILO_DEVICE_ADMIN` names the dial helper; else `tilo-device-admin`
/// beside this executable, else on PATH.
pub fn peer_tool() -> String {
    if let Some(tool) = std::env::var_os("TILO_DEVICE_ADMIN").filter(|tool| !tool.is_empty()) {
        return tool.to_string_lossy().into_owned();
    }
    let name = format!("tilo-device-admin{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .filter(|beside| beside.is_file())
        .map_or(name, |beside| beside.display().to_string())
}

fn peer_tool_missing() -> CliError {
    CliError::ToolMissing {
        tool: "tilo-device-admin".into(),
        purpose: "reach a device beyond this network over the peer network; build it beside tilo-vm (cargo build --release -p tilo-device-admin, then copy target/release/tilo-device-admin next to tilo-vm), or name it in TILO_DEVICE_ADMIN".into(),
    }
}

/// The dial helper's arguments for `route`, as this machine's identity at
/// `identity_root`.
pub fn peer_dial(route: &crate::config::PeerRoute, identity_root: &std::path::Path) -> CommandLine {
    let mut line = CommandLine::new(&peer_tool(), &["dial", "--identity-root"]);
    line.args.push(identity_root.display().to_string());
    line.args
        .extend(["--endpoint".into(), route.endpoint.clone()]);
    line.args.extend(["--device".into(), route.device.clone()]);
    if let Some(relay) = &route.relay {
        line.args.extend(["--relay".into(), relay.clone()]);
    }
    if let Some(home) = &route.home_relay {
        line.args.extend(["--home-relay".into(), home.clone()]);
    }
    line
}

/// Why the dial helper ended, from its exit status and what it said.
fn peer_failure(vm: &str, child: &mut Child) -> CliError {
    let status = child.wait().ok().and_then(|status| status.code());
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let detail = stderr
        .lines()
        .rev()
        .map(|line| line.trim().trim_start_matches("tilo-device-admin: "))
        .find(|line| !line.is_empty())
        .unwrap_or("the dial ended without a word")
        .to_owned();
    CliError::PeerRoute {
        vm: vm.into(),
        failure: crate::error::PeerFailure::from_exit(status),
        detail,
    }
}

/// The guest's VM lock, which VM-wide swaps, gc and upgrades take too.
pub const VM_LOCK: &str = "/var/lib/tilo-vm/locks/vm.lock";

/// The remote command that takes the VM lock, says so, and holds it until
/// the connection ends (the VM shutting down ends it).
pub fn vm_lock_command() -> Vec<String> {
    vec![format!(
        "mkdir -p /var/lib/tilo-vm/locks && flock --nonblock {VM_LOCK} sh -c 'echo locked; exec sleep 600'"
    )]
}

pub enum LockOutcome {
    Held(VmLock),
    /// Another command holds it.
    Busy,
    /// The lock could not be asked for (VM down, no SSH).
    Unreachable(String),
}

/// Holds the guest's VM lock while alive.
pub struct VmLock {
    child: Child,
}

impl VmLock {
    /// Wait up to `timeout` for the connection to end, which means the VM
    /// went down; `true` when it did.
    pub fn wait_for_shutdown(&mut self, timeout: std::time::Duration) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < timeout {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        false
    }
}

impl Drop for VmLock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Take the guest's VM lock without waiting.
pub fn hold_vm_lock(endpoint: &SshEndpoint, key: Option<&str>) -> LockOutcome {
    let line = ssh_command(endpoint, key, &vm_lock_command(), false);
    let spawned = Command::new(&line.program)
        .args(&line.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => return LockOutcome::Unreachable(error.to_string()),
    };
    let mut first = String::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = std::io::BufRead::read_line(&mut BufReader::new(stdout), &mut first);
    }
    if first.trim() == "locked" {
        return LockOutcome::Held(VmLock { child });
    }
    let status = child.wait().ok().and_then(|s| s.code());
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    match status {
        // flock --nonblock exits 1 when the lock is taken.
        Some(1) => LockOutcome::Busy,
        _ => LockOutcome::Unreachable(if stderr.trim().is_empty() {
            "no answer".into()
        } else {
            stderr.trim().to_owned()
        }),
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // Closing stdin ends the daemon's loop; reap ssh so no zombie remains.
        drop(self.client.take());
        if self.peer {
            // The dial helper closes its connection on its way out; killed,
            // it would leave the device's session to time out.
            let started = std::time::Instant::now();
            while started.elapsed() < std::time::Duration::from_secs(3) {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SSH command is key-only and bounded, and the VM lock is the one the daemon takes.
    #[test]
    fn remote_ssh_is_key_only_bounded_and_takes_the_daemons_lock() {
        // Phase: ssh_command_is_key_only_and_bounded.
        {
            let endpoint = SshEndpoint {
                user: "root".into(),
                host: "127.0.0.1".into(),
                port: 2223,
            };
            let line = ssh_command(
                &endpoint,
                Some("/k/id"),
                &["tilo-vmd".into(), "rpc".into()],
                false,
            );
            assert_eq!(
                line.display(),
                "ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new -o ServerAliveInterval=15 \
                 -o UserKnownHostsFile=/k/id.known_hosts -i /k/id -p 2223 root@127.0.0.1 tilo-vmd rpc"
            );
            let interactive = ssh_command(&endpoint, None, &[], true);
            assert!(interactive.args.contains(&"-t".to_string()));
        }

        // Phase: another_account_runs_everything_as_root_through_sudo.
        {
            let owner = SshEndpoint {
                user: "me".into(),
                host: "10.0.0.223".into(),
                port: 22,
            };
            let line = ssh_command(&owner, Some("/k/yoga"), &vm_lock_command(), false);
            let remote =
                &line.args[line.args.iter().position(|a| a == "me@10.0.0.223").unwrap() + 1..];
            assert_eq!(&remote[..5], ["sudo", "-n", "--", "sh", "-c"]);
            assert!(
                remote[5].starts_with("'mkdir -p /var/lib/tilo-vm/locks && flock"),
                "{}",
                remote[5]
            );
            // The daemon too, and a shell is a root login shell.
            let rpc = ssh_command(&owner, None, &["tilo-vmd".into(), "rpc".into()], false);
            assert_eq!(rpc.args.last().unwrap(), "'tilo-vmd rpc'");
            let shell = ssh_command(&owner, None, &[], true);
            assert_eq!(&shell.args[shell.args.len() - 2..], ["sudo", "-i"]);
            // rsync's server runs under sudo with the arguments rsync appends.
            assert_eq!(
                rsync_path_as_root(&owner, "mkdir -p /x && flock --nonblock /l rsync"),
                "sudo -n -- sh -c 'mkdir -p /x && flock --nonblock /l rsync \"$@\"' rsync"
            );
            let root = SshEndpoint {
                user: "root".into(),
                ..owner
            };
            assert_eq!(rsync_path_as_root(&root, "rsync"), "rsync");
            assert_eq!(as_root(&root, &["true".into()]), ["true"]);
            assert_eq!(
                sudo_refusal("motd\nsudo: a password is required\n").as_deref(),
                Some("a password is required")
            );
            assert_eq!(sudo_refusal("bash: tilo-vmd: command not found"), None);
        }

        // Phase: vm_lock_is_the_one_the_daemon_takes.
        {
            let command = vm_lock_command().join(" ");
            assert!(
                command.contains("flock --nonblock /var/lib/tilo-vm/locks/vm.lock"),
                "{command}"
            );
            assert!(command.contains("echo locked"));
        }
    }
}
