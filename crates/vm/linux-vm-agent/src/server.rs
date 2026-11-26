//! One agent: the connection loop and the processes it owns.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use linux_vm_protocol::{
    AgentHello, Display, Duplex, ExitStatus, GuestMessage, GuestOs, GuestShare, Health,
    HostMessage, Launch, MAX_CHUNK_BYTES, PROTOCOL_MAJOR, ProtocolError, RunningApp, recv, send,
};

use crate::launch::{self, GuestView, X_DISPLAY};
use crate::mounts;
use crate::system;
use crate::terminal::Pty;
use crate::user::GuestUser;
use tilo_lock::MutexRecover as _;

const DRAIN: Duration = Duration::from_secs(2);

pub struct Agent {
    user: GuestUser,
    boot_id: String,
    running: Mutex<BTreeMap<u32, RunningApp>>,
    shares: Mutex<Vec<GuestShare>>,
    /// Moves the guest's wall clock. The agent is root in the guest, which
    /// is what `CLOCK_REALTIME` takes; a program it launches for the tenant is
    /// theirs, not root's.
    set_clock: Box<dyn Fn(u64) -> Result<(), String> + Send + Sync>,
    #[cfg(target_os = "linux")]
    files: Mutex<crate::remote_fs::Mounts>,
    xwayland: Mutex<Option<Child>>,
    /// Where the launches that serve agents are served.
    #[cfg(target_os = "linux")]
    agents: Arc<crate::agent_socket::AgentSocket>,
}

type Writer<S> = Arc<Mutex<S>>;

impl Agent {
    #[must_use]
    pub fn new(user: GuestUser, boot_id: String) -> Self {
        Self {
            boot_id,
            running: Mutex::new(BTreeMap::new()),
            shares: Mutex::new(Vec::new()),
            set_clock: Box::new(system::set_realtime),
            #[cfg(target_os = "linux")]
            files: Mutex::new(crate::remote_fs::Mounts::default()),
            xwayland: Mutex::new(None),
            #[cfg(target_os = "linux")]
            agents: Arc::new(crate::agent_socket::AgentSocket::new(
                tilo_paths::login::GUEST_AGENT_SOCKET,
                user.uid,
            )),
            user,
        }
    }

    /// Serve the agent socket at `path` instead of the guest's own place.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn with_agent_socket(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.agents = Arc::new(crate::agent_socket::AgentSocket::new(path, self.user.uid));
        self
    }

    pub fn serve<S: Duplex>(&self, mut stream: S) -> Result<(), ProtocolError> {
        match recv(&mut stream)?.ok_or(ProtocolError::Closed)? {
            HostMessage::Hello { protocol_major } if protocol_major == PROTOCOL_MAJOR => {}
            HostMessage::Hello { protocol_major } => {
                send(
                    &mut stream,
                    &GuestMessage::Error {
                        message: format!(
                            "agent speaks protocol {PROTOCOL_MAJOR}, host sent {protocol_major}"
                        ),
                    },
                )?;
                return Ok(());
            }
            _ => return Err(ProtocolError::Unexpected),
        }
        send(
            &mut stream,
            &GuestMessage::Hello(AgentHello {
                protocol_major: PROTOCOL_MAJOR,
                os: GuestOs::Linux,
                boot_id: self.boot_id.clone(),
                agent_version: env!("CARGO_PKG_VERSION").to_string(),
            }),
        )?;
        let Some(request) = recv::<_, HostMessage>(&mut stream)? else {
            return Ok(());
        };
        let answer = match request {
            HostMessage::Health => GuestMessage::Health(self.health()),
            HostMessage::ListRunning => {
                GuestMessage::Running(self.running.lock_recover().values().cloned().collect())
            }
            HostMessage::Mount { shares } => self.mount(&shares),
            HostMessage::MountFiles { shares } => self.mount_files(&shares),
            HostMessage::SetClock { unix_millis } => match (self.set_clock)(unix_millis) {
                Ok(()) => GuestMessage::ClockSet { unix_millis },
                Err(message) => GuestMessage::Error { message },
            },
            HostMessage::InstalledApps => GuestMessage::InstalledApps(crate::apps::installed(
                &crate::apps::export_dirs(&self.user.home),
            )),
            HostMessage::Launch(request) => return self.launch(stream, &request, false),
            HostMessage::LaunchServingAgents(request) => {
                return self.launch(stream, &request, true);
            }
            HostMessage::Hello { .. }
            | HostMessage::Stdin(_)
            | HostMessage::CloseStdin
            | HostMessage::Resize(_)
            | HostMessage::Signal(_)
            | HostMessage::AgentBytes { .. }
            | HostMessage::AgentClose { .. } => GuestMessage::Error {
                message: "unexpected message".to_string(),
            },
        };
        send(&mut stream, &answer)
    }

    fn health(&self) -> Health {
        let (mem_total_kib, mem_available_kib) =
            system::meminfo(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default());
        let running = self.running.lock_recover();
        Health {
            uptime_secs: system::uptime_secs(
                &std::fs::read_to_string("/proc/uptime").unwrap_or_default(),
            ),
            mem_total_kib,
            mem_available_kib,
            running: u32::try_from(running.len()).unwrap_or(u32::MAX),
            windowed: u32::try_from(running.values().filter(|app| app.windowed).count())
                .unwrap_or(u32::MAX),
        }
    }

    fn mount(&self, desired: &[GuestShare]) -> GuestMessage {
        if let Err(error) = GuestShare::validate_set(desired) {
            return GuestMessage::Error {
                message: error.to_string(),
            };
        }
        let mut shares = self.shares.lock_recover();
        let plan = mounts::plan(&shares, desired);
        let (mounted, failures) = mounts::apply(&shares, &plan);
        shares.clone_from(&mounted);
        drop(shares);
        if failures.is_empty() {
            GuestMessage::Mounted { shares: mounted }
        } else {
            GuestMessage::Error {
                message: failures.join("; "),
            }
        }
    }

    fn mount_files(&self, desired: &[linux_vm_protocol::files::FileShare]) -> GuestMessage {
        if let Err(error) = linux_vm_protocol::files::FileShare::validate_set(desired) {
            return GuestMessage::Error {
                message: error.to_string(),
            };
        }
        #[cfg(target_os = "linux")]
        {
            let connect = |port| {
                linux_vm_protocol::vsock::VsockStream::connect(linux_vm_protocol::HOST_CID, port)
            };
            let (paths, failures) =
                self.files
                    .lock_recover()
                    .apply(desired, connect, self.user.uid, self.user.gid);
            if failures.is_empty() {
                GuestMessage::FilesMounted { paths }
            } else {
                GuestMessage::Error {
                    message: failures.join("; "),
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        GuestMessage::Error {
            message: "shares over vsock need a Linux guest".to_string(),
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "held across the spawn so two launches start one Xwayland"
    )]
    fn ensure_xwayland(&self, port: u32) -> Option<&'static str> {
        let mut xwayland = self.xwayland.lock_recover();
        if let Some(child) = xwayland.as_mut()
            && matches!(child.try_wait(), Ok(None))
        {
            return Some(X_DISPLAY);
        }
        if !Path::new("/usr/bin/xwayland-satellite").exists() {
            return None;
        }
        let argv = launch::xwayland_argv(port);
        let view = self.view(None, &|_| true);
        let spec = launch::spec(
            &Launch {
                argv,
                cwd: None,
                env: Vec::new(),
                stdio: linux_vm_protocol::Stdio::Null,
                display: Display::None,
            },
            &view,
        );
        let child = self
            .command(&spec, true)
            .stdin(Stdio::null())
            .spawn()
            .ok()?;
        *xwayland = Some(child);
        Some(X_DISPLAY)
    }

    fn view<'a>(
        &'a self,
        x_display: Option<&'a str>,
        is_dir: &'a dyn Fn(&Path) -> bool,
    ) -> GuestView<'a> {
        GuestView {
            user: &self.user,
            x_display,
            session_bus: None,
            is_dir,
        }
    }

    /// The process for `spec`, in a process group of its own when
    /// `own_group` (a terminal launch gets its own session instead).
    fn command(&self, spec: &launch::Spec, own_group: bool) -> Command {
        let (program, args) = spec
            .argv
            .split_first()
            .map_or(("/bin/false", &[][..]), |(program, args)| {
                (program.as_str(), args)
            });
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&spec.cwd)
            .uid(self.user.uid)
            .gid(self.user.gid);
        if own_group {
            command.process_group(0);
        }
        command
    }

    /// Start `request`'s program and pump it until it exits; with
    /// `serving_agents`, the agent socket answers what it starts, over this
    /// connection ([`crate::agent_socket`]).
    fn launch<S: Duplex>(
        &self,
        stream: S,
        request: &Launch,
        serving_agents: bool,
    ) -> Result<(), ProtocolError> {
        let writer: Writer<S> = Arc::new(Mutex::new(stream.try_clone()?));
        let reply = |message: &GuestMessage| send(&mut *writer.lock_recover(), message);
        if let Err(error) = request.validate() {
            return reply(&GuestMessage::Error {
                message: error.to_string(),
            });
        }
        let mut request = request.clone();
        if serving_agents && let Err(message) = self.names_agent_socket(&mut request) {
            return reply(&GuestMessage::Error { message });
        }
        let request = &request;
        let (mut command, note) = self.prepare(request);
        if let Some(note) = note {
            reply(&GuestMessage::Stderr(note.into_bytes()))?;
        }
        let terminal = match request.stdio {
            linux_vm_protocol::Stdio::Terminal(size) => {
                match Pty::open(size).and_then(|pty| pty.attach(&mut command).map(|()| pty)) {
                    Ok(pty) => Some(pty),
                    Err(error) => {
                        return reply(&GuestMessage::Error {
                            message: format!("a terminal for {}: {error}", request.argv.join(" ")),
                        });
                    }
                }
            }
            _ => None,
        };
        let spawned = self.spawn(&mut command, serving_agents);
        // The app holds the terminal's slave now; the agent keeps only the
        // master (the command's copies of the slave go with it), so reading
        // it ends once the app and its children are gone.
        drop(command);
        let terminal = terminal.map(|pty| pty.master);
        let (mut child, serving) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                return reply(&GuestMessage::Error {
                    message: format!("{}: {error}", request.argv.join(" ")),
                });
            }
        };
        let pid = child.id();
        self.running.lock_recover().insert(
            pid,
            RunningApp {
                pid,
                argv: request.argv.clone(),
                windowed: request.display == Display::Forwarded,
                started_unix: tilo_clock::unix_now(),
            },
        );
        reply(&GuestMessage::Started { pid })?;
        let carried = Carried::default();
        #[cfg(target_os = "linux")]
        let serving = serving.map(|(serving, connections)| {
            carry(connections, &writer, &carried);
            serving
        });

        let windowed = request.display == Display::Forwarded;
        let (pumps, pumps_done) = pump_streams(
            &mut child,
            terminal,
            (stream, &writer),
            (pid, windowed),
            &carried,
        )?;

        let status = child.wait();
        // What the launch carried ends with it: no process of it is served
        // any more, and the host's ends read theirs.
        drop(serving);
        carried.end();
        for _ in 0..pumps {
            if pumps_done.recv_timeout(DRAIN).is_err() {
                break;
            }
        }
        self.running.lock_recover().remove(&pid);
        let status = status.map_or(
            ExitStatus {
                code: None,
                signal: None,
            },
            exit_status,
        );
        reply(&GuestMessage::Exited(status))
    }

    /// Start `command`; with `serving_agents`, the agent socket serves what
    /// it starts from before it starts, so a connection it makes at once is its
    /// own and not turned away.
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the socket is held across the spawn, until the program is served"
    )]
    fn spawn(
        &self,
        command: &mut Command,
        serving_agents: bool,
    ) -> std::io::Result<(Child, Option<Served>)> {
        let opening = if serving_agents {
            Some(self.agents.opening().map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("the agent socket did not open: {error}"),
                )
            })?)
        } else {
            None
        };
        let child = command.spawn()?;
        let served = opening.map(|opening| opening.serve(child.id()));
        Ok((child, served))
    }

    #[cfg(not(target_os = "linux"))]
    #[allow(
        clippy::unused_self,
        reason = "the Linux build serves the agent socket, and this stub keeps its receiver"
    )]
    fn spawn(
        &self,
        command: &mut Command,
        _serving_agents: bool,
    ) -> std::io::Result<(Child, Option<Served>)> {
        Ok((command.spawn()?, None))
    }

    /// Name the agent socket to `request`'s program, in place of any the
    /// host wrote.
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "a guest of another system cannot serve it, and says so"
    )]
    fn names_agent_socket(&self, request: &mut Launch) -> Result<(), String> {
        request
            .env
            .retain(|(name, _)| name != tilo_paths::login::AGENT_SOCKET_ENV);
        request.env.push((
            tilo_paths::login::AGENT_SOCKET_ENV.to_owned(),
            self.agents.path().display().to_string(),
        ));
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    #[allow(
        clippy::unused_self,
        reason = "the Linux build reads the agent socket, and this stub keeps its receiver"
    )]
    fn names_agent_socket(&self, _request: &mut Launch) -> Result<(), String> {
        Err("an agent socket needs a Linux guest".to_owned())
    }

    fn prepare(&self, request: &Launch) -> (Command, Option<String>) {
        let runtime_dir = self.user.runtime_dir();
        ensure_runtime_dir(&runtime_dir, &self.user);
        let bus = runtime_dir.join("bus");
        let x_display = match request.display {
            Display::Forwarded => self.ensure_xwayland(launch::waypipe_port(request)),
            Display::None => None,
        };
        let is_dir = |path: &Path| path.is_dir();
        let mut view = self.view(x_display, &is_dir);
        view.session_bus = bus.exists().then_some(bus.as_path());
        let spec = launch::spec(request, &view);
        let terminal = matches!(request.stdio, linux_vm_protocol::Stdio::Terminal(_));
        let mut command = self.command(&spec, !terminal);
        let stdio = || match request.stdio {
            linux_vm_protocol::Stdio::Pump => Stdio::piped(),
            // Attached to its terminal by the caller.
            linux_vm_protocol::Stdio::Null | linux_vm_protocol::Stdio::Terminal(_) => Stdio::null(),
        };
        command.stdin(stdio()).stdout(stdio()).stderr(stdio());
        (command, spec.note)
    }
}

/// Pump the launched `child`'s output, and its `terminal`'s when it has one,
/// to the host on `writer`, and the host's frames on `stream` to it; how many
/// pumps carry its output, and where each says it is done.
fn pump_streams<S: Duplex>(
    child: &mut Child,
    terminal: Option<std::fs::File>,
    (stream, writer): (S, &Writer<S>),
    (pid, windowed): (u32, bool),
    carried: &Carried,
) -> std::io::Result<(u8, mpsc::Receiver<()>)> {
    let (drained, pumps_done) = mpsc::channel();
    let mut pumps = 0_u8;
    if let Some(stdout) = child.stdout.take() {
        pump(
            stdout,
            Arc::clone(writer),
            GuestMessage::Stdout,
            drained.clone(),
        );
        pumps = pumps.saturating_add(1);
    }
    if let Some(stderr) = child.stderr.take() {
        pump(
            stderr,
            Arc::clone(writer),
            GuestMessage::Stderr,
            drained.clone(),
        );
        pumps = pumps.saturating_add(1);
    }
    match terminal {
        Some(master) => {
            let output = master.try_clone()?;
            pump(
                output,
                Arc::clone(writer),
                GuestMessage::Stdout,
                drained.clone(),
            );
            pumps = pumps.saturating_add(1);
            host_input(stream, Input::Terminal(master), pid, windowed, carried);
        }
        None => host_input(
            stream,
            Input::Pipe(child.stdin.take()),
            pid,
            windowed,
            carried,
        ),
    }
    drop(drained);
    Ok((pumps, pumps_done))
}

fn exit_status(status: std::process::ExitStatus) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt as _;
    ExitStatus {
        code: status.code(),
        signal: status.signal(),
    }
}

fn ensure_runtime_dir(dir: &Path, user: &GuestUser) {
    use std::os::unix::fs::PermissionsExt as _;
    if dir.is_dir() {
        return;
    }
    if std::fs::create_dir_all(dir).is_ok() {
        let _ = std::os::unix::fs::chown(dir, Some(user.uid), Some(user.gid));
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}

fn pump<S: Duplex, R: Read + Send + 'static>(
    mut source: R,
    writer: Writer<S>,
    frame: fn(Vec<u8>) -> GuestMessage,
    done: mpsc::Sender<()>,
) {
    std::thread::spawn(move || {
        let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
        loop {
            let read = match source.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let chunk = buffer.get(..read).unwrap_or_default().to_vec();
            if send(&mut *writer.lock_recover(), &frame(chunk)).is_err() {
                break;
            }
        }
        let _ = done.send(());
    });
}

/// A launch the agent socket serves, and the connections its processes make.
#[cfg(target_os = "linux")]
type Served = (
    crate::agent_socket::Serving,
    mpsc::Receiver<crate::agent_socket::Connected>,
);
#[cfg(not(target_os = "linux"))]
type Served = std::convert::Infallible;

/// The connections a launch carries from the agent socket, by their number
/// on the launch's connection.
#[derive(Clone, Default)]
struct Carried(Arc<Mutex<BTreeMap<u64, std::os::unix::net::UnixStream>>>);

impl Carried {
    /// Hand the host's bytes to connection `connection`.
    fn write(&self, connection: u64, bytes: &[u8]) {
        let stream = self
            .0
            .lock_recover()
            .get(&connection)
            .and_then(|stream| stream.try_clone().ok());
        if let Some(mut stream) = stream {
            let _ = stream.write_all(bytes);
        }
    }

    /// The host ended connection `connection`.
    fn close(&self, connection: u64) {
        let closed = self.0.lock_recover().remove(&connection);
        if let Some(stream) = closed {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// End every connection.
    fn end(&self) {
        let ended = std::mem::take(&mut *self.0.lock_recover());
        for stream in ended.into_values() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Carry each connection `connections` hands over to the host on `writer`,
/// numbered, until the launch is no longer served.
#[cfg(target_os = "linux")]
fn carry<S: Duplex>(
    connections: mpsc::Receiver<crate::agent_socket::Connected>,
    writer: &Writer<S>,
    carried: &Carried,
) {
    let (writer, carried) = (Arc::clone(writer), carried.clone());
    std::thread::spawn(move || {
        for (connection, connected) in (1_u64..).zip(connections) {
            let crate::agent_socket::Connected { stream, program } = connected;
            let Ok(reading) = stream.try_clone() else {
                continue;
            };
            carried.0.lock_recover().insert(connection, stream);
            let opened = GuestMessage::AgentOpened {
                connection,
                program,
            };
            if send(&mut *writer.lock_recover(), &opened).is_err() {
                carried.close(connection);
                return;
            }
            let (writer, carried) = (Arc::clone(&writer), carried.clone());
            std::thread::spawn(move || {
                carry_from(reading, connection, &writer);
                carried.0.lock_recover().remove(&connection);
            });
        }
    });
}

/// Carry what the process writes on connection `connection` to the host, and
/// its end.
fn carry_from<S: Duplex>(
    mut reading: std::os::unix::net::UnixStream,
    connection: u64,
    writer: &Writer<S>,
) {
    let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
    loop {
        let read = match reading.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let bytes = buffer.get(..read).unwrap_or_default().to_vec();
        if send(
            &mut *writer.lock_recover(),
            &GuestMessage::AgentBytes { connection, bytes },
        )
        .is_err()
        {
            return;
        }
    }
    let _ = send(
        &mut *writer.lock_recover(),
        &GuestMessage::AgentClosed { connection },
    );
}

enum Input {
    Pipe(Option<ChildStdin>),
    Terminal(std::fs::File),
}

fn host_input<S: Duplex>(mut stream: S, input: Input, pid: u32, windowed: bool, carried: &Carried) {
    let carried = carried.clone();
    std::thread::spawn(move || {
        let mut input = input;
        let group = 0_i32.saturating_sub(i32::try_from(pid).unwrap_or(i32::MAX));
        loop {
            match recv::<_, HostMessage>(&mut stream) {
                Ok(Some(HostMessage::Stdin(bytes))) => match &mut input {
                    Input::Pipe(stdin) => {
                        if let Some(pipe) = stdin.as_mut()
                            && pipe.write_all(&bytes).is_err()
                        {
                            *stdin = None;
                        }
                    }
                    Input::Terminal(master) => {
                        let _ = master.write_all(&bytes);
                    }
                },
                // A terminal's input never ends while it is open: the keys
                // say when (Ctrl-D).
                Ok(Some(HostMessage::CloseStdin)) => {
                    if let Input::Pipe(stdin) = &mut input {
                        *stdin = None;
                    }
                }
                Ok(Some(HostMessage::Resize(size))) => {
                    if let Input::Terminal(master) = &input {
                        let _ = crate::terminal::resize(master, size);
                    }
                }
                Ok(Some(HostMessage::Signal(signal))) => {
                    // SAFETY: `kill` has no memory preconditions; the group
                    // is the one this launch created (`process_group(0)`,
                    // or the session a terminal launch leads).
                    unsafe { libc::kill(group, signal.number()) };
                }
                Ok(Some(HostMessage::AgentBytes { connection, bytes })) => {
                    carried.write(connection, &bytes);
                }
                Ok(Some(HostMessage::AgentClose { connection })) => carried.close(connection),
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    if !windowed {
                        // SAFETY: as above.
                        unsafe { libc::kill(group, libc::SIGHUP) };
                    }
                    return;
                }
            }
        }
    });
}

#[must_use]
pub fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|id| id.trim().to_string())
        .unwrap_or_default()
}

#[must_use]
pub fn guest_user(name: &str) -> Option<GuestUser> {
    GuestUser::from_passwd(&std::fs::read_to_string("/etc/passwd").ok()?, name)
}
