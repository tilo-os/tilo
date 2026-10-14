//! The transports to a VM's tilo-vmd: SSH, and for a device beyond the
//! LAN, the peer network through `tilo-device-admin dial`.

use std::io::{BufReader, Read as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use tilo_vm_protocol::{Call, Client, ClientError, HelloResult};

use crate::error::CliError;
use crate::hypervisor::{CommandLine, SshEndpoint};

mod keepawake;

pub const DEFAULT_DAEMON: &str = "tilo-vmd rpc";

/// The already-imported daemon must apply its own replacement: the old RPC
/// process keeps executing its old mount-composition code after a sysext swap.
/// Preserve the configured RPC flags and the authenticated daemon's state root.
pub(crate) fn imported_daemon_command(
    configured: Option<&str>,
    root: &str,
    layer: &str,
) -> Result<Vec<String>, CliError> {
    let invalid = |detail| {
        CliError::io("prepare imported daemon handoff")(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            detail,
        ))
    };
    let path = std::path::Path::new(root);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || root.chars().any(char::is_control)
        || layer.len() != 64
        || !layer
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid(
            "expected an absolute state root and an imported SHA-256 layer",
        ));
    }
    let mut words: Vec<String> = configured
        .unwrap_or(DEFAULT_DAEMON)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let executable = words
        .iter()
        .position(|word| {
            std::path::Path::new(word)
                .file_name()
                .is_some_and(|name| name == "tilo-vmd")
        })
        .ok_or_else(|| {
            invalid("configured daemon must invoke tilo-vmd rpc for a daemon upgrade")
        })?;
    if words.get(executable.saturating_add(1)).map(String::as_str) != Some("rpc") {
        return Err(invalid(
            "configured daemon must invoke tilo-vmd rpc for a daemon upgrade",
        ));
    }
    *words
        .get_mut(executable)
        .ok_or_else(|| invalid("configured daemon has no executable"))? = path
        .join("layers")
        .join(layer)
        .join("usr/libexec/tilo-vmd")
        .to_string_lossy()
        .into_owned();
    let mut command = Vec::new();
    let mut arguments = words.into_iter();
    while let Some(word) = arguments.next() {
        if word == "--root" {
            arguments
                .next()
                .ok_or_else(|| invalid("configured --root has no value"))?;
        } else if !word.starts_with("--root=") {
            command.push(word);
        }
    }
    command.extend(["--root".into(), root.into()]);
    Ok(command)
}

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

// Forward a typed call after inspecting its request, without a second serializer.
struct Forward<T>(tilo_vm_protocol::Request, std::marker::PhantomData<T>);

impl<T> From<Forward<T>> for tilo_vm_protocol::Request {
    fn from(call: Forward<T>) -> Self {
        call.0
    }
}

impl<T: serde::de::DeserializeOwned + serde::Serialize> Call for Forward<T> {
    type Output = T;
}

fn bootstrap_request(request: &tilo_vm_protocol::Request) -> bool {
    use tilo_vm_protocol::{LayerSource, Request};
    match request {
        Request::Status(status) => status.slot.is_none(),
        Request::LayerList(_) => true,
        Request::LayerImport(import) => {
            import.provenance.component == "vmd" && matches!(import.source, LayerSource::Upload(_))
        }
        _ => false,
    }
}

fn bootstrap_hello<R: std::io::BufRead, W: std::io::Write>(
    client: &mut Client<R, W>,
    bootstrap: bool,
) -> Result<HelloResult, ClientError> {
    match client.hello(env!("CARGO_PKG_VERSION")) {
        Err(ClientError::VersionMismatch { daemon: 5, .. }) if bootstrap => {
            // v5 hello/import/upload/list shapes were audited at c4fe1799f7.
            // It has no capability extension: never assume newer optional methods.
            let hello = client.call(tilo_vm_protocol::Hello {
                protocol: 5,
                client_version: env!("CARGO_PKG_VERSION").into(),
            })?;
            if hello.protocol != 5
                || hello.root != "/var/lib/tilo-vm"
                || !matches!(hello.arch.as_str(), "x86_64" | "aarch64")
                || hello.daemon_version.is_empty()
                || !hello.capabilities.is_empty()
            {
                return Err(ClientError::Protocol(
                    "unsupported legacy bootstrap identity or capabilities".into(),
                ));
            }
            Ok(hello)
        }
        result => result,
    }
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
        if self.hello.protocol == 5 {
            return Ok((build(), Ok(())));
        }
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
        Self::connect_line(vm, &line, endpoint, debug)
    }

    /// Only the daemon replacement path may negotiate the audited v5 import vocabulary.
    pub(crate) fn connect_bootstrap(
        vm: &str,
        endpoint: &SshEndpoint,
        key: Option<&str>,
        daemon: Option<&str>,
        debug: Vec<String>,
    ) -> Result<Self, CliError> {
        let command = daemon
            .unwrap_or(DEFAULT_DAEMON)
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let line = ssh_command(endpoint, key, &command, false);
        Self::connect_line_mode(vm, &line, endpoint, debug, true)
    }

    fn connect_line(
        vm: &str,
        line: &CommandLine,
        endpoint: &SshEndpoint,
        debug: Vec<String>,
    ) -> Result<Self, CliError> {
        Self::connect_line_mode(vm, line, endpoint, debug, false)
    }

    fn connect_line_mode(
        vm: &str,
        line: &CommandLine,
        endpoint: &SshEndpoint,
        debug: Vec<String>,
        bootstrap: bool,
    ) -> Result<Self, CliError> {
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
        let handshake = bootstrap_hello(&mut client, bootstrap);
        match handshake {
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

    /// Hand off to a verified imported daemon before its normal swap/recovery.
    /// No registered command or persistent access policy is changed.
    pub(crate) fn handoff(
        &self,
        vm: &str,
        line: &CommandLine,
        endpoint: &SshEndpoint,
        debug: Vec<String>,
    ) -> Result<Self, CliError> {
        if self.peer {
            return Err(CliError::io("upgrade the guest daemon")(
                std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "daemon handoff requires the existing SSH route; reconnect on the LAN before swapping vmd",
                ),
            ));
        }
        let replacement = Self::connect_line(vm, line, endpoint, debug)?;
        if replacement.hello.root != self.hello.root || replacement.hello.arch != self.hello.arch {
            return Err(CliError::io("upgrade the guest daemon")(
                std::io::Error::other(
                    "imported daemon answered for a different state root or architecture",
                ),
            ));
        }
        Ok(replacement)
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
        let request = params.into();
        if self.hello.protocol == 5 && !bootstrap_request(&request) {
            return Err(CliError::InvalidInput("legacy daemon connection permits only vmd upload/import diagnostics; upgrade before ordinary operations".into()));
        }
        self.client
            .as_mut()
            .ok_or_else(|| CliError::from(ClientError::Closed))?
            .call(Forward::<C::Output>(request, std::marker::PhantomData))
            .map_err(CliError::from)
    }

    /// # Errors
    /// Read or transport failures.
    pub fn upload(&mut self, bytes: impl std::io::Read) -> Result<String, CliError> {
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| CliError::from(ClientError::Closed))?;
        if self.hello.protocol == 5 {
            client.upload_legacy(bytes).map_err(CliError::from)
        } else {
            client.upload(bytes).map_err(CliError::from)
        }
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

    #[test]
    fn legacy_bootstrap_is_negotiated_only_for_known_import_vocabulary() {
        use std::io::Cursor;
        use tilo_vm_protocol::{LayerImport, LayerSource, Provenance, Request, Status};
        let response = |id, version, root: &str, arch: &str| {
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"protocol\":{version},\"daemon_version\":\"0.1\",\"root\":\"{root}\",\"arch\":\"{arch}\"}}}}\n"
            )
        };
        let first = response(1, 5, "/var/lib/tilo-vm", "x86_64");
        let mut strict = Client::new(Cursor::new(first.clone()), Vec::new());
        assert!(matches!(
            bootstrap_hello(&mut strict, false),
            Err(ClientError::VersionMismatch { daemon: 5, .. })
        ));
        let mut legacy = Client::new(
            Cursor::new(format!(
                "{first}{}",
                response(2, 5, "/var/lib/tilo-vm", "x86_64")
            )),
            Vec::new(),
        );
        assert_eq!(bootstrap_hello(&mut legacy, true).unwrap().protocol, 5);
        for (version, root, arch) in [
            (6, "/var/lib/tilo-vm", "x86_64"),
            (5, "/other", "x86_64"),
            (5, "/var/lib/tilo-vm", "unknown"),
        ] {
            let input = if version == 6 {
                response(1, version, root, arch)
            } else {
                format!("{first}{}", response(2, version, root, arch))
            };
            assert!(
                bootstrap_hello(&mut Client::new(Cursor::new(input), Vec::new()), true).is_err()
            );
        }
        assert!(!bootstrap_request(&Request::Control(
            tilo_vm_protocol::Control {
                slot: "test".into(),
                args: vec![],
                private_args: None,
            }
        )));
        assert!(bootstrap_request(&Request::Status(Status { slot: None })));
        for component in ["shell", "vmd"] {
            let import = Request::LayerImport(LayerImport {
                source: LayerSource::Upload("receipt".into()),
                provenance: Box::new(Provenance::local(component)),
            });
            assert_eq!(bootstrap_request(&import), component == "vmd");
        }
        let import = Request::LayerImport(LayerImport {
            source: LayerSource::Path("/outside".into()),
            provenance: Box::new(Provenance::local("vmd")),
        });
        assert!(!bootstrap_request(&import));
        // No hello result (including an SSH authentication refusal) is not bootstrap permission.
        assert!(
            bootstrap_hello(
                &mut Client::new(Cursor::new(Vec::<u8>::new()), Vec::new()),
                true
            )
            .is_err()
        );
    }

    #[test]
    fn imported_daemon_command_preserves_rpc_policy_and_refuses_untrusted_paths() {
        let hash = "ab".repeat(32);
        let command = imported_daemon_command(
            Some("sudo -n /usr/libexec/tilo-vmd rpc --root /old --expect-domain greetd_t"),
            "/var/lib/tilo-vm",
            &hash,
        )
        .unwrap();
        assert_eq!(
            command,
            [
                "sudo".to_owned(),
                "-n".into(),
                format!("/var/lib/tilo-vm/layers/{hash}/usr/libexec/tilo-vmd"),
                "rpc".into(),
                "--expect-domain".into(),
                "greetd_t".into(),
                "--root".into(),
                "/var/lib/tilo-vm".into(),
            ]
        );
        for root in ["relative", "/var/lib/../etc", "/var/lib/a\nBAD"] {
            assert!(imported_daemon_command(None, root, &hash).is_err());
        }
        for layer in ["../outside", &"a".repeat(63), &"z".repeat(64)] {
            assert!(imported_daemon_command(None, "/var/lib/tilo-vm", layer).is_err());
        }
        assert!(
            imported_daemon_command(Some("other-daemon rpc"), "/var/lib/tilo-vm", &hash).is_err()
        );
        assert!(imported_daemon_command(Some("tilo-vmd exec"), "/var/lib/tilo-vm", &hash).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn imported_daemon_handoff_applies_through_new_rpc_and_leaves_old_process_intact() {
        use std::os::unix::fs::PermissionsExt as _;
        use tilo_vm_protocol::{Scope, Swap, VM_SLOT, manifest::ComponentInfo};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let hash = "ab".repeat(32);
        let binary = root.join("layers").join(&hash).join("usr/libexec/tilo-vmd");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        let script = r"#!/usr/bin/env python3
import json,os,sys
root=sys.argv[sys.argv.index('--root')+1]
source='imported' if '/layers/' in sys.argv[0] else 'old'
for line in sys.stdin:
 request=json.loads(line)
 method=request['method']
 with open(os.path.join(root,'calls'),'a') as log: log.write(source+' '+method+'\n')
 if method=='hello':
  result={'protocol':request['params']['protocol'],'daemon_version':source,'root':root,'arch':'aarch64' if '--wrong-arch' in sys.argv else 'x86_64'}
 elif method=='swap' and source=='old':
  print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32000,'message':'legacy ExtensionDirectories third overlay: /usr Invalid argument','hint':'upgrade applying daemon'}}),flush=True)
  continue
 elif method=='swap':
  result={'slot':'vm','generation':1,'changed':True,'policy':'vm_refresh','actions':[],'warnings':[]}
 else: raise AssertionError(method)
 print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
";
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let old = tmp.path().join("tilo-vmd");
        std::fs::write(&old, script).unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        let old_line = CommandLine::new(
            old.to_str().unwrap(),
            &["rpc", "--root", root.to_str().unwrap()],
        );
        let endpoint = SshEndpoint {
            user: "root".into(),
            host: "fixture".into(),
            port: 22,
        };
        let mut old_remote = Remote::connect_line("fixture", &old_line, &endpoint, vec![]).unwrap();
        let swap = || Swap {
            slot: VM_SLOT.into(),
            component: "vmd".into(),
            layer: hash.clone(),
            info: Some(ComponentInfo::new(Scope::Vm, vec![])),
        };
        let error = old_remote.call(swap()).unwrap_err();
        assert!(error.to_string().contains("Invalid argument"));
        let command = imported_daemon_command(None, &old_remote.hello.root, &hash).unwrap();
        let line = CommandLine {
            program: command[0].clone(),
            args: command[1..].to_vec(),
            env: vec![],
        };
        let mut imported = old_remote
            .handoff("fixture", &line, &endpoint, vec![])
            .unwrap();
        assert_eq!(imported.call(swap()).unwrap().generation, 1);
        // The old process still reports its own code: replacing a path cannot
        // fix an already-running RPC implementation.
        old_remote.client.as_mut().unwrap().hello("test").unwrap();
        assert_eq!(old_remote.hello.daemon_version, "old");
        assert_eq!(
            std::fs::read_to_string(root.join("calls")).unwrap(),
            "old hello\nold swap\nimported hello\nimported swap\nold hello\n"
        );
        let wrong_arch = CommandLine::new(
            binary.to_str().unwrap(),
            &["rpc", "--root", root.to_str().unwrap(), "--wrong-arch"],
        );
        assert!(
            old_remote
                .handoff("fixture", &wrong_arch, &endpoint, vec![])
                .is_err()
        );
        assert!(
            std::fs::read_to_string(root.join("calls"))
                .unwrap()
                .ends_with("old hello\nimported hello\n")
        );
        // A replacement for a different state root must never reach apply.
        let wrong_root = tmp.path().join("wrong");
        std::fs::create_dir(&wrong_root).unwrap();
        let wrong = CommandLine::new(
            binary.to_str().unwrap(),
            &["rpc", "--root", wrong_root.to_str().unwrap()],
        );
        assert!(
            old_remote
                .handoff("fixture", &wrong, &endpoint, vec![])
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(wrong_root.join("calls")).unwrap(),
            "imported hello\n"
        );
    }

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
