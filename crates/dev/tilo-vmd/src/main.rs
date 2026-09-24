use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use tilo_vm_protocol::{BuildJob, CiJob, Manifest};
use tilo_vmd::apply::{Apply, RecordingApply};
use tilo_vmd::systemd::SystemdApply;
use tilo_vmd::{Daemon, input, jobs, server};

/// Guest daemon for Tilo dev VMs. Humans and agents use `tilo-vm` on the host.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve JSON-RPC on stdin/stdout (what `tilo-vm` runs over SSH).
    Rpc {
        #[command(flatten)]
        state: State,
        /// `SELinux` domain of a greetd session; slot sessions must match it.
        #[arg(long, default_value = tilo_vmd::daemon::DEFAULT_DOMAIN)]
        expect_domain: String,
    },
    /// Build a component (or package and install an app) from a synced
    /// worktree; the job is JSON on stdin, output streams on stderr.
    Build {
        #[command(flatten)]
        state: State,
    },
    /// Turn one component of a published image into a layer; the job is
    /// JSON on stdin, output streams on stderr.
    CiLayer {
        #[command(flatten)]
        state: State,
    },
    /// Bring every slot back after boot: re-create headful seats and start
    /// each slot's session (run once by tilo-vm-restore.service).
    Restore {
        #[command(flatten)]
        state: State,
    },
    /// Run one command as a slot sees the system: in its session's mount
    /// namespace, as its user; output streams, and the run is bounded.
    Exec {
        #[command(flatten)]
        state: State,
        #[arg(long)]
        slot: String,
        /// Seconds before the command is killed (exit 124).
        #[arg(long, default_value_t = tilo_vm_protocol::EXEC_TIMEOUT_SECONDS)]
        timeout: u64,
        /// Output bytes after which the command is killed (exit 125).
        #[arg(long, default_value_t = tilo_vm_protocol::EXEC_MAX_BYTES)]
        max_bytes: u64,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Own a seat's uinput keyboard and pointer and serve input requests
    /// (run by tilo-vmd itself as a transient unit).
    InputServe {
        #[arg(long)]
        seat: String,
        #[arg(long)]
        socket: PathBuf,
    },
}

#[derive(Args)]
struct State {
    /// State root holding layers/, slots/ and src/.
    #[arg(long, default_value = "/var/lib/tilo-vm")]
    root: PathBuf,
    /// Component manifest used when a swap does not carry the component's scope.
    #[arg(long, default_value = "/usr/share/tilo/components.toml")]
    manifest: PathBuf,
    /// What the VM's base image was built with, for layer ABI checks.
    #[arg(long, default_value = "/usr/share/tilo-vm/base.json")]
    base: PathBuf,
    /// `record` performs no side effects; for protocol tests only.
    #[arg(long, value_enum, default_value_t = Backend::Systemd)]
    apply: Backend,
}

#[derive(Clone, Copy, ValueEnum)]
enum Backend {
    Systemd,
    Record,
}

impl State {
    fn daemon(&self) -> Daemon {
        let base = std::fs::read(&self.base)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let manifest = match Manifest::load(&self.manifest) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("tilo-vmd: ignoring manifest: {error}");
                None
            }
        };
        let backend: Box<dyn Apply> = match self.apply {
            Backend::Systemd => Box::new(SystemdApply::new(&self.root)),
            Backend::Record => Box::new(RecordingApply::default()),
        };
        Daemon::new(&self.root, backend, manifest, base)
    }
}

fn exit(ok: bool) -> ExitCode {
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Rpc {
            state,
            expect_domain,
        } => {
            let mut daemon = state.daemon().with_expected_domain(expect_domain);
            match server::serve(
                &mut daemon,
                std::io::stdin().lock(),
                std::io::stdout().lock(),
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("tilo-vmd: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Build { state } => {
            let mut daemon = state.daemon();
            exit(jobs::run_job(
                std::io::stdin().lock(),
                std::io::stdout().lock(),
                |job: BuildJob| daemon.build_job(job),
            ))
        }
        Command::CiLayer { state } => {
            let mut daemon = state.daemon();
            exit(jobs::run_job(
                std::io::stdin().lock(),
                std::io::stdout().lock(),
                |job: CiJob| daemon.ci_job(&job),
            ))
        }
        Command::Restore { state } => {
            let issues = state.daemon().restore();
            for issue in &issues {
                eprintln!(
                    "tilo-vmd: slot {}: {} ({})",
                    issue.slot, issue.problem, issue.hint
                );
            }
            if issues.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Command::Exec {
            state,
            slot,
            timeout,
            max_bytes,
            command,
        } => {
            let limit = std::time::Duration::from_secs(timeout);
            match state.daemon().exec(&slot, &command, limit, max_bytes) {
                Ok(ended) => {
                    match ended {
                        tilo_vmd::exec::Ended::TimedOut => eprintln!(
                            "tilo-vmd: stopped `{}` after {timeout}s (--timeout)",
                            command.join(" ")
                        ),
                        tilo_vmd::exec::Ended::TooMuchOutput => eprintln!(
                            "\ntilo-vmd: stopped `{}` after {max_bytes} bytes of output; narrow it (| head, grep)",
                            command.join(" ")
                        ),
                        _ => {}
                    }
                    ExitCode::from(ended.code())
                }
                Err(error) => {
                    eprintln!("tilo-vmd: {error}\nhint: {}", error.hint());
                    ExitCode::from(126)
                }
            }
        }
        Command::InputServe { seat, socket } => match input::serve(&seat, &socket) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilo-vmd: {error}");
                ExitCode::FAILURE
            }
        },
    }
}
