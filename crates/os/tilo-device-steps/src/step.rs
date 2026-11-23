//! One device step per process. The device's daemon stages a step's arguments in its own
//! directory and starts the step's oneshot unit, which runs `tilo-device-step <action>
//! <invocation>`: the step reads `args.json`, appends what it says to `progress` (its stderr),
//! and leaves `result.json`, `{"ok": <output>}` or `{"err": <RpcError>}`. Nothing here decides who
//! may run a step: the daemon admitted the caller before it started the unit.
//!
//! The same binary runs the helpers the steps start themselves (the input devices, the
//! keep-awake lock, a display layout, the device-owner check) and the boot units' work, and
//! prints the steps it runs (`tilo-device-step catalogue`) for the daemon to publish.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use tilo_device_protocol::Manifest;

use crate::apply::{Apply, RecordingApply};
use crate::systemd::SystemdApply;
use crate::{Daemon, display, input};

/// Where the daemon stages each invocation's directory.
pub const STEPS_DIR: &str = "/run/tilo/device-steps";

/// The arguments after the binary that run helper `name`.
pub(crate) fn helper(name: &'static str) -> [&'static str; 1] {
    [name]
}

/// Is `id` an invocation directory name, never a path?
pub fn valid_invocation(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `tilo-device-step ARGS...`: a step (`<action> <invocation>`), a helper, or `catalogue`.
pub fn main(args: Vec<OsString>) -> ExitCode {
    let action = args.first().and_then(|arg| arg.to_str());
    if action == Some("catalogue") {
        return match crate::catalogue::declared() {
            Ok(declared) => {
                println!("{declared}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("tilo-device-step catalogue: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if let Some(action) = action.filter(|a| tilo_device_protocol::ACTIONS.contains(a)) {
        let Some(invocation) = args
            .get(1)
            .and_then(|arg| arg.to_str())
            .filter(|id| valid_invocation(id))
        else {
            eprintln!("tilo-device-step {action} needs the invocation it runs");
            return ExitCode::from(2);
        };
        return run_step(action, &Path::new(STEPS_DIR).join(invocation));
    }
    helper_main(Helper::parse_from(
        std::iter::once(OsString::from("tilo-device-step")).chain(args),
    ))
}

/// Run step `action` with the invocation directory `dir`, as the step's unit does, on the
/// device's own daemon, built from its state on disk.
pub fn run_step(action: &str, dir: &Path) -> ExitCode {
    run_step_on(&mut State::default().daemon(), action, dir)
}

/// Run step `action` on `daemon` with the invocation directory `dir`. What the step writes to
/// stderr, its tools' output included, is appended to `dir/progress` while it runs.
pub fn run_step_on(daemon: &mut Daemon, action: &str, dir: &Path) -> ExitCode {
    let progress = Progress::to(dir);
    let outcome = fs::read(dir.join("args.json"))
        .map_err(crate::DaemonError::io("read the step's arguments"))
        .and_then(|args| daemon.step(action, &args));
    drop(progress);
    let ok = outcome.is_ok();
    let body = match outcome {
        Ok(output) => serde_json::json!({ "ok": output }),
        Err(error) => serde_json::json!({ "err": error.to_rpc() }),
    };
    let written = serde_json::to_vec(&body)
        .map_err(std::io::Error::other)
        .and_then(|bytes| tilo_files::native::atomic::write(&dir.join("result.json"), &bytes));
    match written {
        Ok(()) if ok => ExitCode::SUCCESS,
        Ok(()) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("could not write the step's result: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Stderr sent to an invocation's `progress` file until dropped. Tools the step runs inherit the
/// descriptor, so their output arrives there too without being copied.
struct Progress {
    saved: Option<rustix::fd::OwnedFd>,
}

impl Progress {
    fn to(dir: &Path) -> Self {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("progress"));
        let saved = file.ok().and_then(|file| {
            let saved = rustix::io::dup(std::io::stderr()).ok()?;
            rustix::stdio::dup2_stderr(&file).ok()?;
            Some(saved)
        });
        Self { saved }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            let _ = rustix::stdio::dup2_stderr(&saved);
        }
    }
}

#[derive(Parser)]
struct Helper {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bring every account back after boot: record the booted image, merge the vm and device
    /// accounts' layers, re-create headful seats and start each account's session.
    Restore {
        #[command(flatten)]
        state: State,
        /// The booted image (default: from the kernel command line).
        #[arg(long, hide = true)]
        booted: Option<String>,
    },
    /// Record the booted OS image and report the accounts that keep layers built on the image
    /// before. Never changes an account.
    ReconcileImage {
        #[command(flatten)]
        state: State,
        /// The booted image (default: from the kernel command line).
        #[arg(long, hide = true)]
        booted: Option<String>,
    },
    /// Import and swap in the component layers an installation left for this machine's first
    /// boot, removing each once it is in place.
    ApplyIncoming {
        #[command(flatten)]
        state: State,
        /// Where the installation left `<component>.tar` and its provenance.
        #[arg(long, default_value = crate::incoming::INCOMING_DIR)]
        dir: PathBuf,
    },
    /// Own a seat's uinput keyboard and pointer and serve input requests (a transient unit the
    /// `input` step starts).
    InputServe {
        #[arg(long)]
        seat: String,
        #[arg(long)]
        socket: PathBuf,
        /// Absolute pointer range, e.g. 3200x1080: the account's logical desktop (default: the
        /// seat's first connected output's mode).
        #[arg(long, value_parser = parse_extent, requires = "touch_extent")]
        extent: Option<(u32, u32)>,
        /// Touchscreen and pen range: the account's first output's mode.
        #[arg(long, value_parser = parse_extent, requires = "extent")]
        touch_extent: Option<(u32, u32)>,
    },
    /// Hold a logind sleep lock until the keep-awake lease in --lease ends (the unit the `awake`
    /// step starts).
    AwakeHold {
        #[arg(long)]
        lease: PathBuf,
    },
    /// Give the account's outputs these modes and scales through the display settings, as the
    /// Settings app does (run as the account user).
    DisplayApply {
        /// Outputs left to right, e.g. 2560x1600@2,1920x1080@1.
        #[arg(long, value_delimiter = ',', required = true)]
        outputs: Vec<tilo_device_protocol::OutputSpec>,
        /// Seconds to wait for the compositor at each step.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Exit 0 if the caller's active identity owns this device, 1 if not (run as the account
    /// user: identity records open only for the account that owns them).
    DeviceOwner {
        /// The account's identity directory.
        #[arg(long)]
        identity: PathBuf,
        /// Root of the device key and certificate.
        #[arg(long, default_value = "/var/lib/tilo")]
        device_root: PathBuf,
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
    /// State root holding layers/, accounts/ and src/.
    #[arg(long, default_value = "/var/lib/tilo-vm")]
    root: PathBuf,
    /// Component manifest used when a swap does not carry the component's scope.
    #[arg(long, default_value = "/usr/share/tilo/components.toml")]
    manifest: PathBuf,
    /// What the VM's base image was built with, for layer ABI checks.
    #[arg(long, default_value = "/usr/share/tilo-vm/base.json")]
    base: PathBuf,
    /// `record` performs no side effects; for tests only.
    #[arg(long, value_enum, default_value_t = Backend::Systemd)]
    apply: Backend,
}

impl Default for State {
    fn default() -> Self {
        Self {
            root: "/var/lib/tilo-vm".into(),
            manifest: "/usr/share/tilo/components.toml".into(),
            base: "/usr/share/tilo-vm/base.json".into(),
            apply: Backend::Systemd,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Backend {
    Systemd,
    Record,
}

impl State {
    fn daemon(&self) -> Daemon {
        let base = fs::read(&self.base)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let manifest = Manifest::load(&self.manifest).unwrap_or_else(|error| {
            eprintln!("ignoring the component manifest: {error}");
            None
        });
        let backend: Box<dyn Apply> = match self.apply {
            Backend::Systemd => Box::new(SystemdApply::new(&self.root)),
            Backend::Record => Box::new(RecordingApply::default()),
        };
        Daemon::new(&self.root, backend, manifest, base)
    }
}

fn exit(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn reconcile_image(state: &State, booted: Option<String>) -> ExitCode {
    let Some(booted) = booted else {
        eprintln!("no composefs= or ostree= on the kernel command line; nothing to record");
        return ExitCode::SUCCESS;
    };
    match state.daemon().reconcile_image(&booted) {
        Ok(Some(change)) => {
            for line in change.describe() {
                eprintln!("{line}");
            }
            if change.kept.is_empty() {
                eprintln!("the image changed to {booted}; no account runs layers");
            }
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("could not record the booted image {booted}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn helper_main(helper: Helper) -> ExitCode {
    match helper.command {
        Command::Restore { state, booted } => {
            let issues = state
                .daemon()
                .restore(booted.or_else(crate::image::running_image).as_deref());
            for issue in &issues {
                eprintln!(
                    "account {}: {} ({})",
                    issue.account, issue.problem, issue.hint
                );
            }
            if issues.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Command::ReconcileImage { state, booted } => {
            reconcile_image(&state, booted.or_else(crate::image::running_image))
        }
        Command::ApplyIncoming { state, dir } => {
            match crate::incoming::apply_incoming(&mut state.daemon(), &dir, &state.manifest) {
                Ok(_) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::InputServe {
            seat,
            socket,
            extent,
            touch_extent,
        } => exit(
            input::serve(
                &seat,
                &socket,
                extent
                    .zip(touch_extent)
                    .map(|(desktop, touch)| input::Geometry { desktop, touch }),
            )
            .map_err(|error| error.to_string()),
        ),
        Command::AwakeHold { lease } => exit(
            crate::awake::hold(&lease)
                .map_err(|error| format!("could not hold the machine awake: {error}")),
        ),
        Command::DisplayApply { outputs, timeout } => {
            match display::apply(&outputs, std::time::Duration::from_secs(timeout)) {
                Ok(done) => {
                    println!("{done}");
                    ExitCode::SUCCESS
                }
                Err(error) => exit(Err(error)),
            }
        }
        Command::DeviceOwner {
            identity,
            device_root,
        } => match tilo_identity::provisioning::active_identity_owns_device(
            &identity,
            &tilo_device_identity::IdentityPaths::under(&device_root),
        ) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("could not check the device owner: {error}");
                ExitCode::from(2)
            }
        },
    }
}
