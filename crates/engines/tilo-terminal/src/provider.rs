//! Where a session runs: the pty its shell process is on, and the route to it.
//!
//! A session crosses the sandbox boundary as a terminal, never as a namespace: the workspace's
//! host has a supervisor (`ush`) that builds the environment and hands back a pty descriptor, so
//! this process only reads and writes a terminal. A device with no supervisor builds the guest
//! itself when it can, and a person may open the device's own login shell in place of either
//! (docs/app-model/environments.md).

use std::io::{Read, Write};
use std::path::PathBuf;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::commands::new_key;
use crate::session::Session;
use crate::target::SessionTarget;

/// Who caused a session to open. The device's own shell is never opened for an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Requester {
    #[default]
    Person,
    Agent,
}

impl Requester {
    fn wire(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Agent => "agent",
        }
    }
}

/// Why a session did not open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The workspace's host would open its device's shell only if the person allows it.
    NeedsAllow(String),
    /// The supervisor answered and refused.
    Refused(String),
    /// This device has no way to reach that target.
    Unavailable(String),
    /// No environment can be built on this device: no host runtime is installed. The device's
    /// own shell may still be opened instead.
    NoRuntime(String),
    Failed(String),
    /// The supervisor's control connection is already held by another session.
    ServiceBusy,
    /// The supervisor speaks a protocol this client does not.
    ServiceProtocol,
    /// The supervisor's session declaration was malformed or did not match the request.
    InvalidSession,
    /// The supervisor handed back no session lease.
    MissingLease,
    /// The supervisor handed back no terminal descriptor.
    MissingTerminal,
    /// The session lease could not be adopted.
    InvalidLease,
}

impl OpenError {
    /// Whether the supervisor answered and refused, leaving the connection usable.
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            Self::NeedsAllow(_) | Self::Refused(_) | Self::NoRuntime(_)
        )
    }

    /// A sentence for a person, in words the app can localise by variant.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::NeedsAllow(reason)
            | Self::Refused(reason)
            | Self::Unavailable(reason)
            | Self::NoRuntime(reason)
            | Self::Failed(reason) => reason,
            Self::ServiceBusy => "The terminal service isn't answering. Try again.",
            Self::ServiceProtocol => {
                "This window needs a newer Tilo on this computer. Update, then try again."
            }
            Self::InvalidSession | Self::MissingLease | Self::MissingTerminal => {
                "The terminal didn't start. Try again."
            }
            Self::InvalidLease => "The terminal's connection couldn't be checked. Try again.",
        }
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for OpenError {}

/// What a caller knows about the session it asks for.
#[derive(Clone, Copy, Default)]
pub struct OpenOptions<'a> {
    /// The program the launcher asked for instead of a login shell; the launcher's context
    /// channel when `None`.
    pub program: Option<&'a [String]>,
    /// The session on another device's host this one relays, when it was carried.
    pub relay_session: Option<&'a str>,
    /// The workspace the run belongs to, when the window knows it.
    pub workspace: Option<&'a str>,
    /// The app's record of the session whose shell the workspace's host runs.
    pub record: Option<&'a str>,
    /// The supervisor connection the launcher handed this process, when it did.
    pub service: Option<&'a tilo_process::local_socket::ServiceSocket>,
    /// Raised when the workspace's host is asked for the session's shell, which may wait for
    /// the person to allow it on another device.
    pub waiting: Option<&'a std::sync::atomic::AtomicBool>,
}

/// Which way the terminal reaches a target, decided by a capability probe.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfineRoute {
    /// The supervisor is reachable: the production path for every target, and a refusal is final.
    Service,
    /// This unconfined host can build a guest itself and has the runtime at the given path.
    SelfHosted(PathBuf),
    Host,
    Unavailable(String),
}

/// Choose how to reach `target` from a capability probe, security first: the supervisor wins
/// unconditionally, the device's own shell is reached only by naming it, and an environment that
/// cannot run is unavailable rather than the host shell.
#[must_use]
pub fn decide_route(
    target: &SessionTarget,
    service_available: bool,
    can_self_host: bool,
    runtime: Option<PathBuf>,
) -> ConfineRoute {
    if let SessionTarget::LinuxVm { .. } = target {
        return ConfineRoute::Unavailable("The Linux VM service is not running".into());
    }
    if service_available {
        return ConfineRoute::Service;
    }
    if target.is_host() {
        return ConfineRoute::Host;
    }
    if can_self_host && let Some(bin) = runtime {
        return ConfineRoute::SelfHosted(bin);
    }
    ConfineRoute::Unavailable(
        "Environments need Tilo's shell runtime, which is not available here".into(),
    )
}

/// [`decide_route`] with the device's own probes.
#[must_use]
pub fn route(target: &SessionTarget, service_available: bool) -> ConfineRoute {
    decide_route(
        target,
        service_available,
        can_build_namespace(),
        ush_binary(),
    )
}

/// Whether any environment can run on this device, given whether its host answered.
#[must_use]
pub fn environments_available(service_available: bool) -> bool {
    !matches!(
        route(&SessionTarget::default_environment(), service_available),
        ConfineRoute::Unavailable(_)
    )
}

/// Whether this process could build the user and mount namespace a self-hosted guest needs:
/// false under a seccomp filter, without unprivileged user namespaces, or without `bwrap`.
fn can_build_namespace() -> bool {
    !tilo_process::confinement::seccomp_filtered()
        && tilo_process::confinement::unprivileged_userns_enabled()
        && bwrap_present()
}

fn bwrap_present() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("bwrap").exists()))
}

/// Where the runtime binary is: `$TILO_USH_BIN`, then the installed location, then `PATH`.
#[must_use]
pub fn ush_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("TILO_USH_BIN") {
        let path = PathBuf::from(explicit);
        if path.exists() {
            return Some(path);
        }
    }
    let installed = PathBuf::from("/usr/libexec/tilo-ush");
    if installed.exists() {
        return Some(installed);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("tilo-ush"))
        .find(|candidate| candidate.exists())
}

/// The program the desktop asked this terminal to run instead of a login shell, as
/// `{"terminal_command": [argv]}` in the launcher's context channel.
#[must_use]
pub fn launch_command_from_env() -> Vec<String> {
    std::env::var("TILO_TOOL_CONTEXT")
        .ok()
        .map(|context| launch_command_from_context(&context))
        .unwrap_or_default()
}

/// The argv inside a launcher context, refusing anything that is not a program name.
#[must_use]
pub fn launch_command_from_context(context: &str) -> Vec<String> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(context)
    else {
        return Vec::new();
    };
    let Some(serde_json::Value::Array(argv)) = map.get("terminal_command") else {
        return Vec::new();
    };
    let argv: Option<Vec<String>> = argv
        .iter()
        .map(|arg| {
            arg.as_str()
                .filter(|arg| !arg.contains('\0'))
                .map(str::to_string)
        })
        .collect();
    argv.filter(|argv| argv.first().is_some_and(|program| !program.is_empty()))
        .unwrap_or_default()
}

/// `argv` as one shell word list that replaces the shell, quoted so the shell reads it back.
#[must_use]
pub fn exec_script(argv: &[String]) -> String {
    let words: Vec<String> = argv
        .iter()
        .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
        .collect();
    format!("exec {}", words.join(" "))
}

/// Open a session on `target` on the route [`route`] chose.
///
/// `service` is the supervisor connection the launcher handed this process, when it did; the
/// caller owns that descriptor and passes it here only for the session it opens.
///
/// # Errors
///
/// Why the session could not open: the person's allow is missing, the supervisor refused it, or
/// nothing here can run it.
pub fn open_with(
    route: &ConfineRoute,
    target: &SessionTarget,
    requested_by: Requester,
    options: &OpenOptions<'_>,
) -> Result<Session, OpenError> {
    let program = options.program.unwrap_or_default();
    match route {
        ConfineRoute::Service => {
            let socket = options.service.ok_or_else(|| {
                OpenError::NoRuntime("the required ush supervisor is not running".into())
            })?;
            open_on_socket(socket, target, requested_by, options)
        }
        ConfineRoute::SelfHosted(bin) => open_self_hosted(bin, program),
        ConfineRoute::Host => open_host(program, requested_by),
        ConfineRoute::Unavailable(reason) => Err(OpenError::Unavailable(reason.clone())),
    }
}

/// Open a session in an environment on the runtime at `bin`: this device builds the guest, which
/// it may only do unconfined.
pub fn open_self_hosted(bin: &std::path::Path, program: &[String]) -> Result<Session, OpenError> {
    let mut cmd = CommandBuilder::new(bin);
    cmd.arg("run");
    // The runtime's interactive shell marks its runs with this session's key, as it does for
    // the supervisor's sessions.
    let key = program.is_empty().then(new_key);
    if let Some(key) = &key {
        cmd.env("TILO_USH_MARKS", key);
    } else {
        cmd.arg("-c");
        cmd.arg(exec_script(program));
    }
    let mut backend = Session::with_command(cmd).map_err(|e| OpenError::Failed(e.to_string()))?;
    backend.commands = key;
    Ok(backend)
}

/// Open the device's own login shell. An agent never gets this: the device's shell is the
/// person's.
pub fn open_host(program: &[String], requested_by: Requester) -> Result<Session, OpenError> {
    if requested_by == Requester::Agent {
        return Err(OpenError::Refused(
            "agents may not open the device's own shell".into(),
        ));
    }
    let cmd = if let Some((first, rest)) = program.split_first() {
        let mut cmd = CommandBuilder::new(first);
        cmd.args(rest);
        cmd
    } else {
        // The login shell this account starts, with the process environment inherited.
        CommandBuilder::new(tilo_process::login_shell::login_shell())
    };
    Session::with_command(cmd).map_err(|error| OpenError::Failed(error.to_string()))
}

/// Open a session on an environment over a supervisor connection.
///
/// # Errors
///
/// Why the session could not open; see [`OpenError`].
pub fn open_on_socket(
    stream: &tilo_process::local_socket::ServiceSocket,
    target: &SessionTarget,
    requested_by: Requester,
    options: &OpenOptions<'_>,
) -> Result<Session, OpenError> {
    open_supervisor_session(stream, target, requested_by, options)
}

fn open_supervisor_session(
    stream: &tilo_process::local_socket::ServiceSocket,
    target: &SessionTarget,
    requested_by: Requester,
    options: &OpenOptions<'_>,
) -> Result<Session, OpenError> {
    // How long the answer may take. Before it answers, the supervisor checks
    // that the environment can be built, which on a device's first open reads
    // the system store's images: ~400 ms on the CI VM, past the 250 ms a
    // hosted connection is given to connect. Never shorten a longer
    // bound, nor add one where the connection has none.
    const USH_OPEN_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
    let program = options.program.unwrap_or_default();
    let failed = |error: &dyn std::fmt::Display| OpenError::Failed(error.to_string());
    {
        let mut writer = stream;
        writer
            .write_all(b"{\"op\":\"hello\",\"protocol\":2}\n")
            .map_err(|e| failed(&e))?;
        writer.flush().map_err(|e| failed(&e))?;
    }
    let (hello, rights) =
        runtime_portal::fd_transport::recv_with_fds(stream).map_err(|e| failed(&e))?;
    let ready = serde_json::from_slice::<serde_json::Value>(&hello).ok();
    if !rights.is_empty()
        || ready
            .as_ref()
            .is_none_or(|value| value["result"] != "ready" || value["protocol"] != 2)
    {
        return Err(OpenError::ServiceProtocol);
    }
    let key = new_key();
    let request = ush_request(target, requested_by, options, Some(&key), (80, 24))?;
    {
        let mut w = stream;
        w.write_all(&request).map_err(|e| failed(&e))?;
        w.flush().map_err(|e| failed(&e))?;
    }
    if stream
        .read_timeout()
        .map_err(|e| failed(&e))?
        .is_some_and(|bound| bound < USH_OPEN_REPLY_TIMEOUT)
    {
        stream
            .set_read_timeout(Some(USH_OPEN_REPLY_TIMEOUT))
            .map_err(|e| failed(&e))?;
    }

    let (mut payload, mut descriptors) =
        runtime_portal::fd_transport::recv_with_fds(stream).map_err(|e| failed(&e))?;
    // A workspace a server hosts: its host is asked for the session's shell, which waits as long
    // as the person takes to allow it on another device. The answer follows.
    if let Some(rest) = after_waiting(&payload) {
        if let Some(waiting) = options.waiting {
            waiting.store(true, std::sync::atomic::Ordering::Release);
        }
        stream.set_read_timeout(None).map_err(|e| failed(&e))?;
        if rest.is_empty() {
            (payload, descriptors) =
                runtime_portal::fd_transport::recv_with_fds(stream).map_err(|e| failed(&e))?;
        } else {
            // The answer arrived with it, in the same read.
            payload = rest.to_vec();
        }
    }
    if descriptors.is_empty() {
        return Err(refusal(&payload));
    }
    let response: UshSessionReply = serde_json::from_slice(&payload)
        .map_err(|_| OpenError::Failed("Malformed ush session descriptor declaration".into()))?;
    if !response.is_valid(descriptors.len(), program, target) {
        return Err(OpenError::InvalidSession);
    }
    let editor = if response.editor_protocol.is_some() {
        let fd = descriptors.pop().ok_or(OpenError::MissingTerminal)?;
        Some(tilo_process::editor::Socket::adopt(fd).map_err(|e| failed(&e))?)
    } else {
        None
    };
    let lifetime = descriptors.pop().ok_or(OpenError::MissingLease)?;
    let fd = descriptors.pop().ok_or(OpenError::MissingTerminal)?;

    // Every descriptor arrived over `SCM_RIGHTS` and this process now owns it.
    // The supervisor declares PTY first, then the private session lease, and
    // only after negotiation the editor capability last.
    let master = std::fs::File::from(fd);
    let lease = runtime_portal::pending_connection::SessionLifetime::adopt(lifetime)
        .map_err(|_| OpenError::InvalidLease)?;
    tilo_process::editor::validate_master(&master).map_err(|e| failed(&e))?;
    let reader = Box::new(master.try_clone().map_err(|e| failed(&e))?) as Box<dyn Read + Send>;
    let writer = Box::new(master.try_clone().map_err(|e| failed(&e))?) as Box<dyn Write + Send>;
    Ok(Session::from_backend(
        writer,
        reader,
        Box::new(FdMaster {
            master: tilo_process::pty::Master::new(master),
        }),
        if target.is_host() { "host" } else { "ush" }.into(),
        response.host_session,
        Some(response.session),
        response.commands.then_some(key),
        editor,
        Some(lease),
    ))
}

/// The `open-session` line for the supervisor; `workspace` is left out when the terminal does not
/// know it.
pub fn ush_request(
    target: &SessionTarget,
    requested_by: Requester,
    options: &OpenOptions<'_>,
    commands: Option<&str>,
    (cols, rows): (u16, u16),
) -> Result<Vec<u8>, OpenError> {
    let program = options.program.unwrap_or_default();
    let wire_target = target.to_supervisor().ok_or_else(|| {
        OpenError::Unavailable("This device's shell service does not serve that target".into())
    })?;
    let mut request = serde_json::json!({
        "op": "open-session",
        "cols": cols,
        "rows": rows,
        "term": "xterm-256color",
        "target": wire_target,
        "requested_by": requested_by.wire(),
    });
    if let Some(fields) = request.as_object_mut() {
        if tilo_process::editor::supported() && program.is_empty() && !target.is_host() {
            fields.insert(
                "editor_protocol".into(),
                serde_json::json!(runtime_portal::editor::VERSION),
            );
        }
        if !program.is_empty() {
            fields.insert("command".to_string(), serde_json::json!(program));
        }
        if let Some(session) = options.relay_session {
            fields.insert("relay_session".to_string(), serde_json::json!(session));
        }
        // The lease says how the session ended: only a shell that exited
        // closes the window; a lost connection to a remote shell reconnects,
        // and an environment that could not be set up says so on the tab.
        fields.insert("end_report".to_string(), serde_json::json!(true));
        if let Some(key) = commands {
            fields.insert("commands".to_string(), serde_json::json!(key));
        }
        // In a workspace a server hosts, the session whose shell runs there.
        if let Some(record) = options.record {
            fields.insert("record".to_string(), serde_json::json!(record));
        }
        with_workspace(&mut request, options.workspace);
    }
    let mut request =
        serde_json::to_vec(&request).map_err(|error| OpenError::Failed(error.to_string()))?;
    request.push(b'\n');
    Ok(request)
}

pub(crate) fn with_workspace(request: &mut serde_json::Value, workspace: Option<&str>) {
    if let (Some(fields), Some(workspace)) = (request.as_object_mut(), workspace) {
        fields.insert("workspace".into(), serde_json::json!(workspace));
    }
}

/// The supervisor's answer to `open-session`, field for field (`ServeResponse::Session` in
/// `crates/runtime/ush/src/supervisor.rs`). Unknown fields are refused, so every field the
/// supervisor sends is named here; `a_session_reply_keeps_its_wire_form` pins the form.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UshSessionReply {
    result: String,
    session: String,
    environment: String,
    /// The host's session, when the shell runs where a server hosts the workspace.
    #[serde(default)]
    host_session: Option<String>,
    /// The session's shell marks its runs with the key the request named.
    #[serde(default)]
    commands: bool,
    #[serde(default)]
    editor_protocol: Option<u8>,
    /// The control protocol that granted this session's private lease.
    #[serde(default)]
    protocol: u16,
}

impl UshSessionReply {
    /// Whether the session declaration is well formed and matches what was asked.
    fn is_valid(
        &self,
        descriptor_count: usize,
        program: &[String],
        target: &SessionTarget,
    ) -> bool {
        let hex_key = |key: &str| key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit());
        let descriptors = if self.editor_protocol.is_some() { 3 } else { 2 };
        self.result == "session"
            && hex_key(&self.session)
            && !self.environment.is_empty()
            && self.environment.len() <= 1024
            && self.host_session.as_deref().is_none_or(hex_key)
            && self
                .editor_protocol
                .is_none_or(|version| version == runtime_portal::editor::VERSION)
            && (self.editor_protocol.is_none() || (program.is_empty() && !target.is_host()))
            && self.protocol == 2
            && descriptor_count == descriptors
    }
}

/// What follows the supervisor's line saying the workspace's host is asked for the session, when
/// `payload` starts with it: empty when nothing arrived after it yet.
fn after_waiting(payload: &[u8]) -> Option<&[u8]> {
    let (line, rest) = match payload.iter().position(|byte| *byte == b'\n') {
        Some(end) => (payload.get(..end)?, payload.get(end.saturating_add(1)..)?),
        None => (payload, &[][..]),
    };
    serde_json::from_slice::<serde_json::Value>(line)
        .is_ok_and(|reply| {
            reply.get("result").and_then(serde_json::Value::as_str) == Some("waiting")
        })
        .then_some(rest)
}

pub fn refusal(payload: &[u8]) -> OpenError {
    #[derive(serde::Deserialize)]
    struct Reply {
        result: String,
        #[serde(default)]
        reason: String,
    }
    match serde_json::from_slice::<Reply>(payload) {
        Ok(reply) if reply.result == "host-not-allowed" => OpenError::NeedsAllow(reply.reason),
        Ok(reply) if reply.result == "no-runtime" => OpenError::NoRuntime(reply.reason),
        Ok(reply) if !reply.reason.is_empty() => OpenError::Refused(reply.reason),
        _ => OpenError::Refused(format!(
            "the ush service refused this session: {}",
            String::from_utf8_lossy(payload).trim()
        )),
    }
}

/// A [`MasterPty`] over a descriptor someone else opened. Resizing is a real `TIOCSWINSZ`: the
/// grid reflows on every window change.
struct FdMaster {
    master: tilo_process::pty::Master,
}

impl MasterPty for FdMaster {
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        let size = self.master.get_size()?;
        Ok(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        })
    }

    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        self.master.resize(tilo_process::pty::WinSize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        })?;
        Ok(())
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(self.master.try_clone()?))
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(self.master.try_clone()?))
    }

    fn process_group_leader(&self) -> Option<i32> {
        self.master.process_group_leader()
    }

    fn as_raw_fd(&self) -> Option<i32> {
        Some(self.master.raw_fd())
    }

    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

/// A pty master with no process behind it, for a widget that has no session yet.
pub struct NullMaster;

impl MasterPty for NullMaster {
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(PtySize::default())
    }

    fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
        Ok(())
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(std::io::sink()))
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(std::io::empty()))
    }

    fn process_group_leader(&self) -> Option<i32> {
        None
    }

    fn as_raw_fd(&self) -> Option<i32> {
        None
    }

    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

/// A pty this process opened: its writer, its reader, its master, and the program's own name.
pub(crate) type SpawnedPty = (
    Box<dyn Write + Send>,
    Box<dyn Read + Send>,
    Box<dyn MasterPty + Send>,
    String,
);

/// Open a pty on `cmd`, with `TERM` forced to `xterm-256color` so programs emit the SGR and DEC
/// sequences the emulator reads.
pub(crate) fn spawn_pty(cmd: CommandBuilder) -> Result<SpawnedPty, Box<dyn std::error::Error>> {
    let mut cmd = cmd;
    cmd.env("TERM", "xterm-256color");
    let shell_name = cmd
        .get_argv()
        .first()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    drop(child);
    Ok((writer, reader, pair.master, shell_name))
}
