//! One agent: the connection loop and the processes it owns.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use legacy_agent_protocol::{
    AgentHello, Display, Duplex, ExitStatus, GuestMessage, GuestOs, GuestShare, Health,
    HostMessage, Launch, MAX_CHUNK_BYTES, PROTOCOL_MAJOR, ProtocolError, RunningApp, recv, send,
};

use crate::launch::{self, GuestView, X_DISPLAY};
use crate::mounts;
use crate::system;
use crate::user::GuestUser;
use tilo_lock::MutexRecover as _;

/// How long a finished app's output pumps get to drain before the exit is
/// reported anyway (a daemon it spawned may hold the pipe open forever).
const DRAIN: Duration = Duration::from_secs(2);

pub struct Agent {
    user: GuestUser,
    boot_id: String,
    running: Mutex<BTreeMap<u32, RunningApp>>,
    shares: Mutex<Vec<GuestShare>>,
    xwayland: Mutex<Option<Child>>,
}

type Writer<S> = Arc<Mutex<S>>;

impl Agent {
    #[must_use]
    pub fn new(user: GuestUser, boot_id: String) -> Self {
        Self {
            user,
            boot_id,
            running: Mutex::new(BTreeMap::new()),
            shares: Mutex::new(Vec::new()),
            xwayland: Mutex::new(None),
        }
    }

    /// Serve one host connection to completion.
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
            HostMessage::InstalledApps => GuestMessage::InstalledApps(crate::apps::installed(
                &crate::apps::export_dirs(&self.user.home),
            )),
            HostMessage::Launch(request) => return self.launch(stream, &request),
            HostMessage::Hello { .. }
            | HostMessage::Stdin(_)
            | HostMessage::CloseStdin
            | HostMessage::Signal(_) => GuestMessage::Error {
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

    /// Start the forwarded Xwayland once; later launches reuse it.
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
                stdio: legacy_agent_protocol::Stdio::Null,
                display: Display::None,
            },
            &view,
        );
        let child = self.command(&spec).stdin(Stdio::null()).spawn().ok()?;
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

    fn command(&self, spec: &launch::Spec) -> Command {
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
            .gid(self.user.gid)
            .process_group(0);
        command
    }

    fn launch<S: Duplex>(&self, stream: S, request: &Launch) -> Result<(), ProtocolError> {
        let writer: Writer<S> = Arc::new(Mutex::new(stream.try_clone()?));
        let reply = |message: &GuestMessage| send(&mut *writer.lock_recover(), message);
        if let Err(error) = request.validate() {
            return reply(&GuestMessage::Error {
                message: error.to_string(),
            });
        }
        let (mut command, note) = self.prepare(request);
        if let Some(note) = note {
            reply(&GuestMessage::Stderr(note.into_bytes()))?;
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
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

        let (drained, pumps_done) = mpsc::channel();
        let mut pumps = 0_u8;
        if let Some(stdout) = child.stdout.take() {
            pump(
                stdout,
                Arc::clone(&writer),
                GuestMessage::Stdout,
                drained.clone(),
            );
            pumps = pumps.saturating_add(1);
        }
        if let Some(stderr) = child.stderr.take() {
            pump(
                stderr,
                Arc::clone(&writer),
                GuestMessage::Stderr,
                drained.clone(),
            );
            pumps = pumps.saturating_add(1);
        }
        drop(drained);
        let windowed = request.display == Display::Forwarded;
        host_input(stream, child.stdin.take(), pid, windowed);

        let status = child.wait();
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

    /// The guest process for `request`, and what to tell its stderr first.
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
        let mut command = self.command(&spec);
        let stdio = || match request.stdio {
            legacy_agent_protocol::Stdio::Relay => Stdio::piped(),
            legacy_agent_protocol::Stdio::Null => Stdio::null(),
        };
        command.stdin(stdio()).stdout(stdio()).stderr(stdio());
        (command, spec.note)
    }
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

/// Feed the host's stdin, close and signals to the app. When the host goes
/// away, a tool is hung up on like a closed terminal would; a windowed app
/// keeps running, the way a desktop app outlives the terminal that opened it.
fn host_input<S: Duplex>(mut stream: S, stdin: Option<ChildStdin>, pid: u32, windowed: bool) {
    std::thread::spawn(move || {
        let mut stdin = stdin;
        let group = 0_i32.saturating_sub(i32::try_from(pid).unwrap_or(i32::MAX));
        loop {
            match recv::<_, HostMessage>(&mut stream) {
                Ok(Some(HostMessage::Stdin(bytes))) => {
                    if let Some(input) = stdin.as_mut()
                        && input.write_all(&bytes).is_err()
                    {
                        stdin = None;
                    }
                }
                Ok(Some(HostMessage::CloseStdin)) => stdin = None,
                Ok(Some(HostMessage::Signal(signal))) => {
                    // SAFETY: `kill` has no memory preconditions; the group
                    // is the one this launch created with `process_group(0)`.
                    unsafe { libc::kill(group, signal.number()) };
                }
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

/// The guest's boot id, which changes on every boot.
#[must_use]
pub fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|id| id.trim().to_string())
        .unwrap_or_default()
}

/// `/etc/passwd`'s entry for the guest user.
#[must_use]
pub fn guest_user(name: &str) -> Option<GuestUser> {
    GuestUser::from_passwd(&std::fs::read_to_string("/etc/passwd").ok()?, name)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;
    use legacy_agent_protocol::client;

    fn me() -> GuestUser {
        let (uid, gid) = (
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw(),
        );
        GuestUser {
            name: "me".into(),
            uid,
            gid,
            home: std::env::temp_dir(),
            shell: "/bin/sh".into(),
        }
    }

    #[test]
    fn the_agent_runs_a_tool_and_reports_health() {
        let agent = Arc::new(Agent::new(me(), "boot-1".into()));
        // phase: a launch relays stdin to stdout and returns the exit code
        {
            let (host, guest) = UnixStream::pair().unwrap();
            let serving = Arc::clone(&agent);
            let server = std::thread::spawn(move || serving.serve(guest));
            let request = Launch {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "cat; echo err >&2; exit 3".into(),
                ],
                cwd: Some("/".into()),
                env: Vec::new(),
                stdio: legacy_agent_protocol::Stdio::Relay,
                display: Display::None,
            };
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let status = client::launch(
                host,
                &request,
                client::LaunchIo {
                    stdin: Some(Box::new(std::io::Cursor::new(b"piped".to_vec()))),
                    stdout: &mut out,
                    stderr: &mut err,
                    started: &mut |_| {},
                },
                |_| {},
            )
            .unwrap();
            server.join().unwrap().unwrap();
            assert_eq!(status.code, Some(3));
            assert_eq!(out, b"piped");
            assert_eq!(err, b"err\n");
        }
        // phase: health reports nothing running afterwards
        {
            let (mut host, guest) = UnixStream::pair().unwrap();
            let serving = Arc::clone(&agent);
            let server = std::thread::spawn(move || serving.serve(guest));
            let GuestMessage::Health(health) =
                client::request(&mut host, &HostMessage::Health).unwrap()
            else {
                panic!("expected health");
            };
            server.join().unwrap().unwrap();
            assert_eq!((health.running, health.windowed), (0, 0));
        }
        // phase: an invalid share set is refused without touching mounts
        {
            let (mut host, guest) = UnixStream::pair().unwrap();
            let serving = Arc::clone(&agent);
            let server = std::thread::spawn(move || serving.serve(guest));
            let refused = client::request(
                &mut host,
                &HostMessage::Mount {
                    shares: vec![GuestShare {
                        tag: "fs-1".into(),
                        path: "/usr".into(),
                        read_only: true,
                    }],
                },
            );
            server.join().unwrap().unwrap();
            assert!(matches!(refused, Err(ProtocolError::Refused(_))));
        }
    }
}
