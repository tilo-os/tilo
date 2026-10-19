use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use tilo_vm_protocol::{BuildJob, CiJob, Manifest};
use tilo_vmd::apply::{Apply, RecordingApply};
use tilo_vmd::systemd::SystemdApply;
use tilo_vmd::{Daemon, display, input, jobs, server};

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
    /// Bring every slot back after boot: record the booted image, merge the
    /// vm and device slots' layers, re-create headful seats and start each
    /// slot's session (run once by tilo-vm-restore.service).
    Restore {
        #[command(flatten)]
        state: State,
        /// The booted image (default: from the kernel command line).
        #[arg(long, hide = true)]
        booted: Option<String>,
    },
    /// Record the booted OS image and report the slots that keep layers
    /// built on the image before (run at boot by
    /// tilo-vm-reconcile-image.service). Never changes a slot.
    ReconcileImage {
        #[command(flatten)]
        state: State,
        /// The booted image (default: from the kernel command line).
        #[arg(long, hide = true)]
        booted: Option<String>,
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
        /// Absolute pointer range, e.g. 3200x1080: the slot's logical
        /// desktop (default: the seat's first connected output's mode).
        #[arg(long, value_parser = parse_extent, requires = "touch_extent")]
        extent: Option<(u32, u32)>,
        /// Touchscreen and pen range: the slot's first output's mode.
        #[arg(long, value_parser = parse_extent, requires = "extent")]
        touch_extent: Option<(u32, u32)>,
    },
    /// Hold a logind sleep lock until the keep-awake lease in --lease ends
    /// (run by tilo-vmd itself as tilo-vm-awake.service).
    AwakeHold {
        #[arg(long)]
        lease: PathBuf,
    },
    /// Install distribution packages into the VM's /usr with its package
    /// manager (a transient overlay on bootc images); output streams.
    InstallPackages {
        /// Seconds before the install is stopped (exit 124).
        #[arg(long, default_value_t = tilo_vm_protocol::INSTALL_TIMEOUT_SECONDS)]
        timeout: u64,
        /// Package names, e.g. xterm zenity.
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Give the slot's outputs these modes and scales through the display
    /// settings, as the Settings app does (run by tilo-vmd as the slot user).
    DisplayApply {
        /// Outputs left to right, e.g. 2560x1600@2,1920x1080@1.
        #[arg(long, value_delimiter = ',', required = true)]
        outputs: Vec<tilo_vm_protocol::OutputSpec>,
        /// Seconds to wait for the compositor at each step.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
}

fn parse_extent(text: &str) -> Result<(u32, u32), String> {
    let (width, height) = text
        .split_once('x')
        .ok_or_else(|| format!("'{text}' is not WIDTHxHEIGHT"))?;
    let number = |v: &str| v.parse::<u32>().map_err(|e| format!("'{text}': {e}"));
    Ok((number(width)?, number(height)?))
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
    /// `record` performs no side effects, `record-device` none on a
    /// machine whose own session runs as tester, and `record-login` none
    /// on a laptop tester is logged in to through greetd; for protocol
    /// tests only.
    #[arg(long, value_enum, default_value_t = Backend::Systemd)]
    apply: Backend,
}

#[derive(Clone, Copy, ValueEnum)]
enum Backend {
    Systemd,
    Record,
    RecordDevice,
    RecordLogin,
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
            Backend::RecordDevice => Box::new(RecordingApply::with_device("tester")),
            Backend::RecordLogin => Box::new(RecordingApply::with_login("tester")),
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

fn install_packages(packages: &[String], timeout: u64) -> ExitCode {
    let argv = match tilo_vmd::packages::install_argv(packages, std::path::Path::exists) {
        Ok(argv) => argv,
        Err(error) => {
            eprintln!("tilo-vmd: {error}");
            return ExitCode::from(126);
        }
    };
    let Some((program, flags)) = argv.split_first() else {
        return ExitCode::from(126);
    };
    eprintln!("tilo-vmd: {}", argv.join(" "));
    let mut command = std::process::Command::new(program);
    command.args(flags);
    match tilo_vmd::exec::run_bounded(
        command,
        std::time::Duration::from_secs(timeout),
        16 << 20,
        std::io::stdout(),
        std::io::stderr(),
    ) {
        Ok(ended) => ExitCode::from(ended.code()),
        Err(error) => {
            eprintln!("tilo-vmd: could not run {program}: {error}");
            ExitCode::from(126)
        }
    }
}

fn display_apply(outputs: &[tilo_vm_protocol::OutputSpec], timeout: u64) -> ExitCode {
    match display::apply(outputs, std::time::Duration::from_secs(timeout)) {
        Ok(done) => {
            println!("{done}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn awake_hold(lease: &std::path::Path) -> ExitCode {
    match tilo_vmd::awake::hold(lease) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-vmd: could not hold the machine awake: {error}");
            ExitCode::FAILURE
        }
    }
}

fn reconcile_image(state: &State, booted: Option<String>) -> ExitCode {
    let Some(booted) = booted else {
        eprintln!(
            "tilo-vmd: no composefs= or ostree= on the kernel command line; nothing to record"
        );
        return ExitCode::SUCCESS;
    };
    match state.daemon().reconcile_image(&booted) {
        Ok(Some(change)) => {
            for line in change.describe() {
                eprintln!("tilo-vmd: {line}");
            }
            if change.kept.is_empty() {
                eprintln!("tilo-vmd: the image changed to {booted}; no slot runs layers");
            }
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-vmd: could not record the booted image {booted}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn restore(state: &State, booted: Option<String>) -> ExitCode {
    let issues = state
        .daemon()
        .restore(booted.or_else(tilo_vmd::image::running_image).as_deref());
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
        Command::ReconcileImage { state, booted } => {
            reconcile_image(&state, booted.or_else(tilo_vmd::image::running_image))
        }
        Command::Restore { state, booted } => restore(&state, booted),
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
        Command::InputServe {
            seat,
            socket,
            extent,
            touch_extent,
        } => match input::serve(
            &seat,
            &socket,
            extent
                .zip(touch_extent)
                .map(|(desktop, touch)| input::Geometry { desktop, touch }),
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilo-vmd: {error}");
                ExitCode::FAILURE
            }
        },
        Command::AwakeHold { lease } => awake_hold(&lease),
        Command::DisplayApply { outputs, timeout } => display_apply(&outputs, timeout),
        Command::InstallPackages { timeout, packages } => install_packages(&packages, timeout),
    }
}
