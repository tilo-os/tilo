//! Programs a hosted workspace's shell runs in the workspace's own VM
//! (tier 3 on a host that serves other people's workspaces).
//!
//! On such a host, a stranger's `cargo` or `python` never runs in a guest
//! sharing the host's kernel: [`HostedPrograms`] asks the host's hosted-VM
//! service ([`legacy_vm::hosted`]) for the VM of this workspace environment,
//! which boots it on first use and restores it from its snapshot after it
//! went idle, and runs each program there through the VM's agent, with the
//! shell's streams relayed and the environment's granted directories at the
//! same paths. The shell, its own commands and their run records are the
//! same as on a device; only where a program runs differs. When the
//! account's plan does not allow the VM, the program fails with the plan's
//! refusal, word for word, so the agent that ran it can act on it.
//!
//! A host turns this on for its sessions with [`SOCKET_ENV`], naming the
//! account billed with [`ACCOUNT_ENV`] and the VM's size with [`SIZE_ENV`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use legacy_agent_protocol::client::{self, LaunchIo};
use legacy_agent_protocol::{AGENT_PORT, Display, Launch, ProtocolError, Signal, Stdio as Relay};
use legacy_vm::hosted::control::{self, Reply, Request, ShareRequest, VmRequest};
use legacy_vm::hosted::size::VmSize;
use ush_shell::{Input, Output, Program, Std, Stdio};
use ush_tilo::Programs;

/// The hosted-VM service's socket. Set on a host that serves workspaces.
pub const SOCKET_ENV: &str = "TILO_HOSTED_VM_SOCKET";
/// The account the session's VM time is billed to.
pub const ACCOUNT_ENV: &str = "TILO_HOSTED_ACCOUNT";
/// The size of the session's VM (`small`, `default`, `large`, `xlarge`).
pub const SIZE_ENV: &str = "TILO_HOSTED_VM_SIZE";

/// How long a wake may take, a first boot included.
const WAKE_PATIENCE: Duration = Duration::from_secs(300);

/// Where a hosted session's programs run, and who pays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedTarget {
    pub socket: PathBuf,
    pub account: String,
    pub size: VmSize,
}

impl HostedTarget {
    /// The host's settings for its sessions, when it serves workspaces.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let socket = PathBuf::from(std::env::var_os(SOCKET_ENV)?);
        let account = std::env::var(ACCOUNT_ENV).ok()?;
        let size = std::env::var(SIZE_ENV)
            .ok()
            .and_then(|size| VmSize::parse(&size))
            .unwrap_or_default();
        Some(Self {
            socket,
            account,
            size,
        })
    }
}

type Shares = dyn Fn() -> Vec<ShareRequest> + Send + Sync;

/// A hosted workspace environment's VM, as the place its programs run.
pub struct HostedPrograms {
    target: HostedTarget,
    workspace: String,
    environment: String,
    /// The directories the environment grants now: writable ones read-write.
    shares: Box<Shares>,
}

impl HostedPrograms {
    #[must_use]
    pub fn new(
        target: HostedTarget,
        workspace: String,
        environment: String,
        shares: Box<Shares>,
    ) -> Self {
        Self {
            target,
            workspace,
            environment,
            shares,
        }
    }

    fn request(&self) -> Request {
        Request::Ensure {
            vm: VmRequest {
                workspace: self.workspace.clone(),
                environment: self.environment.clone(),
                account: self.target.account.clone(),
                size: self.target.size,
            },
            shares: (self.shares)(),
        }
    }
}

/// The shares a policy's grants make: writable directories read-write,
/// readable ones read-only.
#[must_use]
pub fn shares_of(writable: Vec<PathBuf>, readable: Vec<PathBuf>) -> Vec<ShareRequest> {
    let mut shares: Vec<ShareRequest> = writable
        .into_iter()
        .map(|path| ShareRequest {
            path,
            read_only: false,
        })
        .collect();
    for path in readable {
        if !shares.iter().any(|share| share.path == path) {
            shares.push(ShareRequest {
                path,
                read_only: true,
            });
        }
    }
    shares
}

struct InputReader(Input);

impl Read for InputReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

struct OutputWriter<'a>(&'a Output);

impl Write for OutputWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn failed(name: &str, io: &Stdio, reason: &str) -> Program {
    let _ = io.stderr.write_str(&format!("ush: {name}: {reason}\n"));
    Program::Exited(126)
}

impl Programs for HostedPrograms {
    fn run(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: Option<&dyn Fn() -> bool>,
    ) -> Program {
        let Some(name) = argv.first() else {
            return Program::Exited(0);
        };
        let link = match control::ask(&self.target.socket, &self.request(), WAKE_PATIENCE) {
            Ok(Some(Reply::Ready { link, .. })) => link,
            Ok(Some(Reply::Refused(refusal))) => return failed(name, io, &refusal),
            Ok(Some(Reply::Error(error))) => return failed(name, io, &error),
            Ok(Some(other)) => {
                return failed(
                    name,
                    io,
                    &format!("the hosted-VM service answered {other:?}"),
                );
            }
            Ok(None) => {
                return failed(
                    name,
                    io,
                    "this workspace's Linux VM service is not running on its host",
                );
            }
            Err(error) => {
                return failed(name, io, &format!("this workspace's Linux VM: {error}"));
            }
        };
        let stream = match link.connect(AGENT_PORT) {
            Ok(stream) => stream,
            Err(error) => return failed(name, io, &format!("this workspace's Linux VM: {error}")),
        };
        let launch = Launch {
            argv: argv.to_vec(),
            cwd: Some(cwd.display().to_string()),
            env: env.to_vec(),
            stdio: Relay::Relay,
            display: Display::None,
        };
        // The VM has no terminal of the session's: a program reading the
        // terminal gets an empty input rather than taking the shell's keys.
        let stdin: Option<Box<dyn Read + Send>> = match io.stdin.host_stream() {
            Some(Std::In) => None,
            _ => Some(Box::new(InputReader(io.stdin.clone()))),
        };
        let (mut stdout, mut stderr) = (OutputWriter(&io.stdout), OutputWriter(&io.stderr));
        let (handing, handed) = std::sync::mpsc::channel();
        let result = std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                client::launch(
                    stream,
                    &launch,
                    LaunchIo {
                        stdin,
                        stdout: &mut stdout,
                        stderr: &mut stderr,
                        started: &mut |_| {},
                    },
                    move |signals| {
                        let _ = handing.send(signals);
                    },
                )
            });
            // Ended from outside (the job was killed): end it in the VM.
            if let Some(stop) = stop
                && let Ok(signals) = handed.recv()
            {
                while !running.is_finished() {
                    if stop() {
                        let _ = signals.send(Signal::Kill);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            running.join().unwrap_or(Err(ProtocolError::Closed))
        });
        match result {
            Ok(status) => Program::Exited(status.shell_code()),
            Err(ProtocolError::Refused(message))
                if message.ends_with("No such file or directory (os error 2)") =>
            {
                Program::NotFound
            }
            Err(error) => failed(name, io, &format!("this workspace's Linux VM: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_grants_win_over_readable_ones() {
        let shares = shares_of(
            vec!["/srv/ws/files".into()],
            vec!["/srv/ws/files".into(), "/srv/shared/docs".into()],
        );
        assert_eq!(
            shares,
            [
                ShareRequest {
                    path: "/srv/ws/files".into(),
                    read_only: false
                },
                ShareRequest {
                    path: "/srv/shared/docs".into(),
                    read_only: true
                }
            ]
        );
    }

    #[test]
    fn a_plans_refusal_reaches_the_program_word_for_word() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("hosted.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Request = control::FRAMING.read_value(&mut stream).unwrap().unwrap();
            let Request::Ensure { vm, shares } = request else {
                panic!("an ensure");
            };
            assert_eq!(
                (
                    vm.workspace.as_str(),
                    vm.environment.as_str(),
                    vm.account.as_str(),
                    vm.size
                ),
                ("ws1", "default", "acct-1", VmSize::Large)
            );
            assert_eq!(shares.len(), 1);
            control::FRAMING
                .write_value(
                    &mut stream,
                    &Reply::Refused("Your plan runs at most 1 Linux VM at a time.".into()),
                )
                .unwrap();
        });
        let programs = HostedPrograms::new(
            HostedTarget {
                socket,
                account: "acct-1".into(),
                size: VmSize::Large,
            },
            "ws1".into(),
            "default".into(),
            Box::new(|| shares_of(vec!["/srv/ws1/files".into()], Vec::new())),
        );
        let (stderr, err) = Output::buffer();
        let io = Stdio {
            stderr,
            ..Stdio::null()
        };
        let ran = programs.run(
            &["cargo".into()],
            &[],
            Path::new("/srv/ws1/files"),
            &io,
            None,
        );
        server.join().unwrap();
        assert_eq!(ran, Program::Exited(126));
        assert_eq!(
            String::from_utf8(err.take()).unwrap(),
            "ush: cargo: Your plan runs at most 1 Linux VM at a time.\n"
        );
    }
}
