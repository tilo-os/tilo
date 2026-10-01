//! Command implementations. Each returns an [`Outcome`]: what happened, as
//! text and as JSON, plus the likely next commands.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tilo_vm_protocol::manifest::{Component, Manifest};
use tilo_vm_protocol::{
    Action, AppInstall, AppInstallResult, ApplyResult, BuildJob, BuildTarget, Button, CiJob,
    Control, Gc, Input, InputAction, JobOutput, LayerImport, LayerSource, Logs, Owner, Policy,
    Rollback, Scope, SeatClaim, SeatRelease, Shot, SlotCreate, SlotInfo, SlotKind, SlotList,
    SlotRestart, SlotRm, SlotShow, SlotShowResult, Status, Stick, Swap, VM_SLOT, VmTools,
};

use crate::appsets;
use crate::build;
use crate::ci;
use crate::cli::{
    Builder, Command, CreateArgs, FlightCommand, GamepadCommand, ImageCommand, InputCommand,
    PenCommand, ScenarioCommand, SeatCommand, SlotAppsCommand, SlotCommand, TouchCommand,
    UpgradeArgs, VmCommand,
};
use crate::config::{self, DirectKernel, Registry, Source, VmEntry, WORKTREE_FILE};
use crate::context::Worktree;
use crate::disk;
use crate::doctor::{self, Check};
use crate::error::CliError;
use crate::guide;
use crate::host::{self, HostFacts};
use crate::hypervisor::{
    self, CommandLine, Hypervisor, Runner, SshEndpoint, SystemRunner, Vm, run_all,
};
use crate::image;
use crate::imageup;
use crate::receipt::Receipt;
use crate::remote::{self, LockOutcome, Remote};
use crate::sshguard;
use crate::sshkey;
use crate::vmbuild;

mod kernel;
mod scenario;

#[derive(Debug, Clone, Copy)]
struct ForeignRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl ForeignRect {
    fn center_x(self) -> f64 {
        self.x + self.w / 2.0
    }

    fn center_y(self) -> f64 {
        self.y + self.h / 2.0
    }
}

#[derive(Debug, Clone, Copy)]
struct ForeignFrame {
    surface_id: u64,
    frame_seq: u64,
    rect: Option<ForeignRect>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartConnectDisposition {
    Ready,
    NoDaemon,
    Retry,
    Fail,
}

#[cfg(test)]
fn start_connect_disposition(result: &Result<Remote, CliError>) -> StartConnectDisposition {
    match result {
        Ok(_) => StartConnectDisposition::Ready,
        Err(CliError::DaemonMissing { .. }) => StartConnectDisposition::NoDaemon,
        Err(CliError::Unreachable { .. }) => StartConnectDisposition::Retry,
        Err(_) => StartConnectDisposition::Fail,
    }
}

fn foreign_frames_args() -> &'static [String] {
    static ARGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    ARGS.get_or_init(|| {
        vec![
            "raw".into(),
            serde_json::json!({ "op": "foreign_frames" }).to_string(),
        ]
    })
}

fn control_json(
    remote: &mut Remote,
    slot: &str,
    args: &[String],
    worst: &mut Duration,
) -> Result<Value, CliError> {
    let started = Instant::now();
    let result = remote.call(Control {
        slot: slot.to_owned(),
        args: args.to_vec(),
    })?;
    *worst = (*worst).max(started.elapsed());
    if result.exit_code != 0 {
        return Err(CliError::CommandFailed {
            command: format!("tilo-control {}", args.join(" ")),
            status: format!("exit {}", result.exit_code),
            stderr: result.stderr.trim().to_owned(),
        });
    }
    serde_json::from_str(result.stdout.trim()).map_err(|error| CliError::CommandFailed {
        command: format!("tilo-control {}", args.join(" ")),
        status: "invalid JSON response".into(),
        stderr: error.to_string(),
    })
}

fn foreign_rows(value: &Value) -> impl Iterator<Item = &Value> {
    value
        .get("foreign_frames")
        .and_then(|frames| frames.get("frames"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn foreign_surface_ids(value: &Value) -> std::collections::HashSet<u64> {
    foreign_rows(value)
        .filter_map(|frame| frame.get("surface_id").and_then(Value::as_u64))
        .collect()
}

fn parse_foreign_frame(value: &Value) -> Option<ForeignFrame> {
    let rect = value.get("rect").and_then(|rect| {
        Some(ForeignRect {
            x: rect.get("x")?.as_f64()?,
            y: rect.get("y")?.as_f64()?,
            w: rect.get("w")?.as_f64()?,
            h: rect.get("h")?.as_f64()?,
        })
    });
    Some(ForeignFrame {
        surface_id: value.get("surface_id")?.as_u64()?,
        frame_seq: value.get("frame_seq")?.as_u64()?,
        rect,
    })
}

fn matching_presented_frame(
    value: &Value,
    app_id: &str,
    baseline_ids: &std::collections::HashSet<u64>,
) -> Option<ForeignFrame> {
    foreign_rows(value).find_map(|frame| {
        let parsed = parse_foreign_frame(frame)?;
        (frame.get("app_id").and_then(Value::as_str) == Some(app_id)
            && frame.get("presented").and_then(Value::as_bool) == Some(true)
            && parsed.frame_seq > 0
            && !baseline_ids.contains(&parsed.surface_id))
        .then_some(parsed)
    })
}

fn matching_surface(value: &Value, surface_id: u64) -> Option<ForeignFrame> {
    foreign_rows(value).find_map(|frame| {
        let parsed = parse_foreign_frame(frame)?;
        (parsed.surface_id == surface_id).then_some(parsed)
    })
}

fn ensure_before(deadline: Instant, message: &str) -> Result<(), CliError> {
    (Instant::now() < deadline)
        .then_some(())
        .ok_or_else(|| flight_failure(message))
}

fn flight_failure(message: impl Into<String>) -> CliError {
    CliError::CommandFailed {
        command: "tilo-vm flight app".into(),
        status: "acceptance invariant failed".into(),
        stderr: message.into(),
    }
}

#[cfg(test)]
mod flight_tests {
    use super::*;

    fn frames(rows: &Value) -> Value {
        json!({ "foreign_frames": { "frames": rows } })
    }

    #[test]
    fn presented_flight_surface_must_be_new_matching_and_real() {
        let baseline = frames(&json!([
            { "surface_id": 4, "app_id": "chatgpt", "frame_seq": 8, "presented": true }
        ]));
        let ids = foreign_surface_ids(&baseline);
        let current = frames(&json!([
            { "surface_id": 4, "app_id": "chatgpt", "frame_seq": 9, "presented": true },
            { "surface_id": 5, "app_id": "other", "frame_seq": 1, "presented": true },
            { "surface_id": 6, "app_id": "chatgpt", "frame_seq": 0, "presented": false },
            {
                "surface_id": 7,
                "app_id": "chatgpt",
                "frame_seq": 2,
                "presented": true,
                "rect": { "x": 10.0, "y": 20.0, "w": 300.0, "h": 200.0 }
            }
        ]));

        let selected = matching_presented_frame(&current, "chatgpt", &ids).expect("new frame");
        assert_eq!(selected.surface_id, 7);
        assert_eq!(selected.frame_seq, 2);
        let rect = selected.rect.expect("placement");
        assert_eq!((rect.center_x(), rect.center_y()), (160.0, 120.0));
    }

    #[test]
    fn frame_lookup_tracks_the_same_surface_after_input() {
        let value = frames(&json!([
            { "surface_id": 12, "app_id": "chatgpt", "frame_seq": 41, "presented": true },
            { "surface_id": 13, "app_id": "chatgpt", "frame_seq": 99, "presented": true }
        ]));

        assert_eq!(matching_surface(&value, 12).expect("surface").frame_seq, 41);
        assert!(matching_surface(&value, 14).is_none());
    }
}

pub struct Outcome {
    pub text: String,
    pub data: Value,
    pub next: Vec<String>,
    /// Set by checks that report rather than fail, like `doctor`.
    pub failed: bool,
}

impl Outcome {
    fn new(text: impl Into<String>, data: Value) -> Self {
        Self {
            text: text.into(),
            data,
            next: Vec::new(),
            failed: false,
        }
    }

    #[must_use]
    fn next(mut self, commands: &[&str]) -> Self {
        self.next.extend(commands.iter().map(|c| (*c).to_owned()));
        self
    }
}

pub struct Resolved {
    pub name: String,
    pub entry: VmEntry,
    pub source: Source,
}

impl Resolved {
    fn vm(&self) -> Vm<'_> {
        Vm {
            name: &self.name,
            entry: &self.entry,
        }
    }
}

/// A published dev disk, found but not yet downloaded.
struct Published {
    tag: String,
    disk: disk::Disk,
    dest: PathBuf,
    direct_boot: Option<disk::DirectBoot>,
    source_revision: Option<String>,
    auth: disk::AuthHeader,
}

/// What `vm create` will do, settled before it does anything.
struct Plan {
    choice: host::Choice,
    image: Option<PathBuf>,
    /// The format of `image`.
    image_format: Option<image::Format>,
    /// Where `image`, on a network path, is imported to first (WHPX).
    import: Option<PathBuf>,
    boot_esp: Option<PathBuf>,
    published: Option<Published>,
    disk_path: Option<PathBuf>,
}

impl Plan {
    fn describe(&self) -> String {
        let mut lines = vec![format!("  hypervisor: {}", self.choice.describe())];
        match (&self.image, &self.published) {
            (Some(image), _) => {
                let format = self.image_format.map_or("", image::Format::name);
                lines.push(format!("  image: {} ({format})", image.display()));
                if let Some(import) = &self.import {
                    lines.push(format!(
                        "  import: on a network path, so copied once to {} and compared byte for byte{}",
                        import.display(),
                        if import.is_file() { " (already imported)" } else { "" }
                    ));
                }
            }
            (None, Some(p)) => {
                lines.push(format!(
                    "  image: published dev disk {} ({} {}, {}{})",
                    p.tag,
                    p.disk.arch,
                    p.disk.format,
                    human_bytes(p.disk.size),
                    if disk::cached(&p.dest, &p.disk) {
                        ", already downloaded"
                    } else {
                        ", to download"
                    }
                ));
                if p.direct_boot.is_some() {
                    lines.push("  boot: verified CI direct kernel bundle (WHPX)".into());
                }
            }
            (None, None) => lines.push("  image: the UTM template".into()),
        }
        if let Some(disk) = &self.disk_path {
            lines.push(format!("  VM disk: {}", disk.display()));
        }
        if let Some(esp) = &self.boot_esp {
            lines.push(format!(
                "  boot ESP: {} (read-only, boots before the VM disk)",
                esp.display()
            ));
        }
        lines.join("\n")
    }
}

pub struct App {
    pub json: bool,
    pub worktree: Option<Worktree>,
    pub registry_path: PathBuf,
    pub runner: SystemRunner,
    pub receipt: Receipt,
    /// Non-fatal findings surfaced after the main output.
    pub warnings: Vec<String>,
}

pub fn human_bytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        let tenths = bytes.saturating_mul(10) >> 30;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else {
        format!("{} MB", bytes >> 20)
    }
}

/// Set `key` on a JSON object; no-op for anything else.
fn insert(value: &mut Value, key: &str, item: Value) {
    if let Value::Object(map) = value {
        map.insert(key.into(), item);
    }
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

/// What an apply did, in one sentence per consequence.
pub fn describe_apply(result: &ApplyResult) -> String {
    if !result.changed {
        return format!(
            "slot {} is already at generation {}; nothing to do",
            result.slot, result.generation
        );
    }
    let mut text = format!(
        "generation {} applied to slot {}",
        result.generation, result.slot
    );
    let restarted_sessions: Vec<&str> = result
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::StartUnit { unit } => unit
                .strip_prefix("tilo-vm-session@")?
                .strip_suffix(".service"),
            _ => None,
        })
        .collect();
    match result.policy {
        Policy::SessionRestart => {
            text.push_str("; session restarted (logout, user manager restart, login)");
        }
        Policy::NoRestart => {
            text.push_str("; nothing restarted: apps are live at their next launch");
        }
        Policy::VmRefresh => {
            let units: Vec<&str> = result
                .actions
                .iter()
                .filter_map(|a| match a {
                    Action::RestartUnit { unit } if !unit.starts_with("user@") => {
                        Some(unit.as_str())
                    }
                    _ => None,
                })
                .collect();
            text.push_str("; VM /usr refreshed with systemd-sysext");
            if !units.is_empty() {
                let _ = write!(text, "; restarted {}", units.join(", "));
            }
            if !restarted_sessions.is_empty() {
                let _ = write!(
                    text,
                    "; restarted the sessions of every slot so they see it: {}",
                    restarted_sessions.join(", ")
                );
            }
        }
        Policy::Reboot => {
            text.push_str("; VM /usr refreshed with systemd-sysext");
            match result.boot.as_ref() {
                Some(boot) => match (&boot.kver, &boot.entry) {
                    (Some(kver), Some(entry)) => {
                        let _ = write!(text, "; kernel {kver} is the next boot ({entry})");
                    }
                    (None, entry) => {
                        let _ = write!(
                            text,
                            "; the image's kernel is the default boot again{}",
                            entry
                                .as_ref()
                                .map(|e| format!(" ({e})"))
                                .unwrap_or_default()
                        );
                    }
                    (Some(kver), None) => {
                        let _ = write!(text, "; kernel {kver} is the next boot");
                    }
                },
                None => text.push_str("; a kernel changed"),
            }
            text.push_str("; it takes effect when the VM reboots");
        }
    }
    text
}

/// The registry that publishes images and CI layers. Anyone may pull from
/// it: no pull needs a login.
pub const REGISTRY: &str = "registry.tilo.so";

/// `path`, or its nearest ancestor that exists.
fn existing_ancestor(path: &Path) -> &Path {
    path.ancestors()
        .find(|p| p.exists())
        .unwrap_or_else(|| Path::new("/"))
}

/// Free bytes of the filesystem that holds (or will hold) `path`, where the
/// host can tell.
fn host_free(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[IO.DriveInfo]::new([IO.Path]::GetPathRoot($env:TILO_VM_DISK_DIR)).AvailableFreeSpace",
            ])
            .env("TILO_VM_DISK_DIR", existing_ancestor(path))
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
    #[cfg(unix)]
    {
        let output = std::process::Command::new("df")
            .arg("-Pk")
            .arg(existing_ancestor(path))
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let kib: u64 = text
            .lines()
            .nth(1)?
            .split_whitespace()
            .nth(3)?
            .parse()
            .ok()?;
        Some(kib.saturating_mul(1024))
    }
}

fn vm_free_floor(configured: u64, hypervisor: &str) -> u64 {
    configured.max(if hypervisor == "whpx" { 15 << 30 } else { 0 })
}

fn on_path(tool: &str) -> bool {
    if tool == "rsync" {
        return !crate::host::which_all(tool).is_empty();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&paths).any(|dir| {
        extensions.iter().any(|ext| {
            let candidate = dir.join(format!("{tool}{ext}"));
            candidate.is_file()
        }) || dir.join(tool).is_file()
    })
}

impl App {
    fn registry(&self) -> Result<Registry, CliError> {
        Registry::load(&self.registry_path)
    }

    fn base_dir(&self) -> PathBuf {
        self.worktree.as_ref().map_or_else(
            || std::env::current_dir().unwrap_or_default(),
            |w| w.root.clone(),
        )
    }

    /// # Errors
    /// No VM selected, or a name the registry does not know.
    pub fn resolve_vm(&self, explicit: Option<&str>) -> Result<Resolved, CliError> {
        let registry = self.registry()?;
        let (name, source) = if let Some(name) = explicit {
            (name.to_owned(), Source::Argument)
        } else {
            let file = self.worktree.as_ref().map(|w| w.root.join(WORKTREE_FILE));
            let text = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok());
            let worktree_file = file.as_deref().zip(text.as_deref());
            let env = std::env::var("TILO_VM").ok();
            config::resolve(
                env.as_deref(),
                worktree_file,
                &registry,
                &self.registry_path,
            )
            .ok_or_else(|| CliError::NoVm {
                registry: self.registry_path.display().to_string(),
            })?
        };
        let entry = registry
            .vms
            .get(&name)
            .cloned()
            .ok_or_else(|| CliError::UnknownVm {
                name: name.clone(),
                registry: self.registry_path.display().to_string(),
                known: registry.vms.keys().cloned().collect(),
            })?;
        Ok(Resolved {
            name,
            entry,
            source,
        })
    }

    fn adapter(vm: &Resolved) -> Result<Box<dyn Hypervisor>, CliError> {
        hypervisor::adapter(&vm.entry.hypervisor)
    }

    fn debug_commands(adapter: &dyn Hypervisor, vm: &str) -> Vec<String> {
        let caps = adapter.capabilities();
        let mut commands = Vec::new();
        if caps.screenshot {
            commands.push(format!("tilo-vm vm shot {vm}"));
        }
        if caps.serial {
            commands.push(format!("tilo-vm vm serial {vm}"));
        }
        commands
    }

    fn connect(&mut self, vm: &Resolved) -> Result<Remote, CliError> {
        let adapter = Self::adapter(vm)?;
        let endpoint = adapter.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let debug = Self::debug_commands(adapter.as_ref(), &vm.name);
        let remote = match Remote::connect(
            &vm.name,
            &endpoint,
            vm.entry.key.as_deref(),
            vm.entry.daemon.as_deref(),
            debug,
        ) {
            Ok(remote) => remote,
            // SSH works; whether vm upgrade can fix it depends on bootc.
            Err(CliError::ProtocolMismatch { client, daemon, .. }) => {
                return Err(CliError::ProtocolMismatch {
                    vm: vm.name.clone(),
                    client,
                    daemon,
                    bootc: self.probe_bootc(vm, &endpoint),
                });
            }
            Err(CliError::DaemonMissing { .. }) => {
                return Err(CliError::DaemonMissing {
                    vm: vm.name.clone(),
                    bootc: self.probe_bootc(vm, &endpoint),
                });
            }
            Err(error) => return Err(key_refusal(vm, adapter.as_ref(), &endpoint, error)),
        };
        for issue in &remote.hello.issues {
            self.warnings.push(format!(
                "slot {}: {}; {}",
                issue.slot, issue.problem, issue.hint
            ));
        }
        self.receipt.set("vm", &vm.name);
        Ok(remote)
    }

    /// Does the VM have bootc? `None` when SSH did not say.
    fn probe_bootc(&mut self, vm: &Resolved, endpoint: &SshEndpoint) -> Option<bool> {
        let output = self.vm_sh(vm, endpoint, imageup::BOOTC_PROBE).ok()?;
        imageup::parse_bootc_probe(&output.stdout)
    }

    /// Why `connect` failed, as `status` and `vm status` report it; asks the
    /// hypervisor whether the VM is running at all.
    fn connect_failure(
        &mut self,
        vm: &Resolved,
        adapter: &dyn Hypervisor,
        error: &CliError,
    ) -> ConnectFailure {
        let running = adapter.running(&vm.vm(), &mut self.runner);
        let debug = Self::debug_commands(adapter, &vm.name);
        connect_failure(&vm.name, running, &debug, error)
    }

    /// The slot commands use when none is named: `TILO_VM_SLOT`, then
    /// `slot = "<name>"` in the worktree's `.tilo-vm`, then the branch's.
    pub fn default_slot(&self) -> Option<String> {
        if let Ok(name) = std::env::var("TILO_VM_SLOT")
            && !name.trim().is_empty()
        {
            return Some(name.trim().to_owned());
        }
        let worktree = self.worktree.as_ref()?;
        std::fs::read_to_string(worktree.root.join(WORKTREE_FILE))
            .ok()
            .and_then(|text| config::parse_worktree_slot(&text))
            .or_else(|| worktree.default_slot())
    }

    /// # Errors
    /// Outside a worktree, or a branch that makes no usable name.
    pub fn slot_name(&self, explicit: Option<String>) -> Result<String, CliError> {
        if let Some(name) = explicit.or_else(|| self.default_slot()) {
            return Ok(name);
        }
        let worktree = self.worktree.as_ref().ok_or(CliError::NotInWorktree)?;
        worktree.default_slot().ok_or_else(|| CliError::SlotName {
            branch: worktree
                .branch
                .clone()
                .unwrap_or_else(|| worktree.root.display().to_string()),
        })
    }

    fn manifest(&self) -> Result<Option<Manifest>, CliError> {
        let Some(worktree) = &self.worktree else {
            return Ok(None);
        };
        let path = worktree.root.join("os/components.toml");
        Manifest::load(&path).map_err(|e| CliError::Config {
            path: path.display().to_string(),
            detail: e.to_string(),
        })
    }

    /// # Errors
    /// Whatever the command reports.
    pub fn run(&mut self, command: Option<Command>) -> Result<Outcome, CliError> {
        let named = command.as_ref().and_then(named_slot);
        let mut outcome = self.dispatch(command)?;
        // Hints follow the slot in use when it is not the default one.
        if let Some(slot) = named
            && self.default_slot().as_deref() != Some(slot.as_str())
        {
            outcome.next = outcome.next.iter().map(|n| with_slot(n, &slot)).collect();
        }
        Ok(outcome)
    }

    fn dispatch(&mut self, command: Option<Command>) -> Result<Outcome, CliError> {
        match command {
            None => self.status(None),
            Some(Command::Status { slot }) => self.status(slot),
            Some(Command::Doctor) => Ok(self.doctor()),
            Some(Command::Guide { topic }) => Self::guide(topic.as_deref()),
            Some(Command::Vm(command)) => self.vm(command),
            Some(Command::Slot(SlotCommand::Apps(SlotAppsCommand::List))) => Ok(apps_list()),
            Some(Command::Slot(command)) => self.slot(command),
            Some(Command::Swap {
                component,
                slot,
                from,
                builder,
                profile,
                direct,
                jobs,
                cmdline,
            }) => self.swap(
                slot,
                component,
                &from,
                builder,
                profile.as_deref(),
                KernelFlags {
                    direct,
                    jobs,
                    cmdline,
                },
            ),
            Some(Command::Input { slot, action }) => self.input(slot, action),
            Some(Command::Rollback {
                slot, to, forward, ..
            }) => {
                let slot = self.slot_name(slot)?;
                let vm = self.resolve_vm(None)?;
                let to = if forward {
                    let info = self
                        .connect(&vm)?
                        .call(Status {
                            slot: Some(slot.clone()),
                        })?
                        .slots
                        .into_iter()
                        .next();
                    Some(forward_target(&slot, info.as_ref())?)
                } else {
                    to
                };
                if slot == VM_SLOT {
                    return self.rollback_vm(to);
                }
                let mut remote = self.connect(&vm)?;
                let result = self.receipt.phase("apply", || {
                    remote.call(Rollback {
                        slot: slot.clone(),
                        to,
                    })
                })?;
                Ok(Outcome::new(describe_apply(&result), json!(result)).next(&[
                    "tilo-vm shot",
                    "tilo-vm logs",
                    "tilo-vm status",
                ]))
            }
            Some(Command::Logs { slot, lines, .. }) => {
                let slot = self.slot_name(slot)?;
                let vm = self.resolve_vm(None)?;
                let result = self.connect(&vm)?.call(Logs { slot, lines })?;
                Ok(Outcome::new(result.text.clone(), json!(result)))
            }
            Some(Command::Seat(command)) => self.seat(command),
            Some(Command::Shot { slot, output, .. }) => self.shot(slot, &output),
            Some(Command::Control { slot, args }) => self.control(slot, &args),
            Some(Command::Flight(command)) => self.flight(command),
            Some(Command::Scenario(ScenarioCommand::Install(args))) => self.scenario_install(&args),
            Some(Command::Image(ImageCommand::Build { disk, out })) => self.image_build(disk, &out),
            Some(Command::Image(ImageCommand::Disk { image })) => self.image_disk(&image),
            Some(Command::Gc {
                dry_run,
                keep,
                keep_targets,
                min_age,
            }) => self.gc(dry_run, keep, keep_targets, min_age),
        }
    }

    fn guide(topic: Option<&str>) -> Result<Outcome, CliError> {
        let text = match topic {
            None => guide::index(),
            Some(name) => {
                guide::topic(name)
                    .map(str::to_owned)
                    .ok_or_else(|| CliError::Unsupported {
                        what: format!("guide topic '{name}'"),
                        alternative: format!(
                            "pick one of: {}",
                            guide::TOPICS
                                .iter()
                                .map(|(n, _, _)| *n)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    })?
            }
        };
        Ok(Outcome::new(
            text.clone(),
            json!({ "topic": topic, "text": text }),
        ))
    }

    // ---- status ---------------------------------------------------------------

    #[expect(
        clippy::too_many_lines,
        reason = "one linear report, section by section"
    )]
    fn status(&mut self, slot: Option<String>) -> Result<Outcome, CliError> {
        let goals =
            "goals: warm component build < 60s, swap + session restart < 10s, cached boot < 30s";
        let vm = match self.resolve_vm(None) {
            Err(error @ CliError::NoVm { .. }) => {
                return Ok(Outcome::new(
                    format!(
                        "VM: none selected\n  new here? read tilo-vm guide quickstart\n  {}\n{goals}",
                        error.hint()
                    ),
                    json!({ "vm": null, "registry": self.registry_path, "hint": error.hint() }),
                )
                .next(&[
                    "tilo-vm guide quickstart",
                    "tilo-vm vm create <name>",
                    "tilo-vm vm list",
                ]));
            }
            other => other?,
        };
        let slot = self.slot_name(slot).ok();
        let mut lines = vec![format!(
            "VM: {} ({}, {})",
            vm.name,
            vm.entry.hypervisor,
            vm.source.describe()
        )];
        let mut vm_data =
            json!({ "name": vm.name, "hypervisor": vm.entry.hypervisor, "chosen": vm.source });
        let mut remote = match self.connect(&vm) {
            Ok(remote) => remote,
            Err(error) => {
                let adapter = Self::adapter(&vm)?;
                let failure = self.connect_failure(&vm, adapter.as_ref(), &error);
                lines.push(failure.text(&error));
                insert(&mut vm_data, "state", json!(failure.state));
                insert(&mut vm_data, "reachable", json!(failure.ssh));
                let mut data = json!({ "vm": vm_data });
                merge_error(&mut data, &error);
                let mut outcome = Outcome::new(lines.join("\n"), data);
                outcome.next = failure.next;
                outcome.failed = true;
                self.warnings.clear();
                return Ok(outcome);
            }
        };
        let hello = remote.hello.clone();
        let status = remote.call(Status { slot: None })?;
        lines.push(format!(
            "  tilo-vmd {} (protocol {}), {}",
            hello.daemon_version, hello.protocol, hello.arch
        ));
        let registry = self.registry()?;
        if let Some(free) = status.storage.free_bytes {
            let low = free < registry.min_free_bytes();
            lines.push(format!(
                "  disk: {} free, layers use {}, build dirs {}{}",
                human_bytes(free),
                human_bytes(status.storage.layers_bytes),
                human_bytes(status.storage.targets_bytes),
                if low { "  LOW: tilo-vm gc" } else { "" }
            ));
        }
        lines.push(format!("  builds: {}", describe_tools(&hello.tools)));
        if let Some(lease) = &status.seat0 {
            lines.push(format!(
                "  seat0: held by slot {} since {}",
                lease.slot, lease.since
            ));
        }
        let mine = slot
            .as_ref()
            .and_then(|name| status.slots.iter().find(|s| &s.name == name));
        let mut next: Vec<String> = Vec::new();
        match (&slot, mine) {
            (Some(_), Some(info)) => {
                lines.push(slot_line(info));
                for (component, layer) in &info.components {
                    lines.push(format!("    {component}: {}", short(layer)));
                }
                next.extend(
                    [
                        "tilo-vm swap <component>",
                        "tilo-vm shot",
                        "tilo-vm control actions list",
                        "tilo-vm logs",
                    ]
                    .map(str::to_owned),
                );
            }
            (Some(name), None) => {
                let (text, hints) = missing_slot(name, &status.slots);
                lines.extend(text);
                next.extend(hints);
            }
            (None, _) => {
                lines.push("slot: none (not in a worktree; pass a slot name)".into());
                next.push("tilo-vm slot list".into());
                next.push("tilo-vm guide quickstart".into());
            }
        }
        for issue in &status.issues {
            lines.push(format!(
                "  ! slot {}: {} -> {}",
                issue.slot, issue.problem, issue.hint
            ));
        }
        self.warnings.clear();
        lines.push(goals.into());
        insert(&mut vm_data, "state", json!("ready"));
        insert(&mut vm_data, "reachable", json!(true));
        let data = json!({
            "vm": vm_data,
            "daemon": hello,
            "slot": mine,
            "slot_name": slot,
            "status": status,
        });
        let mut outcome = Outcome::new(lines.join("\n"), data);
        outcome.next = next;
        Ok(outcome)
    }

    // ---- doctor ---------------------------------------------------------------

    fn doctor(&mut self) -> Outcome {
        let facts = HostFacts::probe(&mut self.runner);
        let mut checks = doctor::host_checks(&facts);
        let token = disk::find_token();
        checks.extend(doctor::credential_checks(token.as_ref()));
        let root = self.base_dir();
        match self.manifest() {
            Ok(Some(_)) => checks.push(Check::ok("manifest", "os/components.toml found")),
            Ok(None) => checks.push(Check::warn(
                "manifest",
                "no os/components.toml here",
                "run from a Tilo worktree",
            )),
            Err(e) => checks.push(Check::fail("manifest", e.to_string(), e.hint())),
        }
        let listing = disk::AuthHeader::new(token.as_ref())
            .and_then(|auth| disk::get(&mut self.runner, disk::VERSIONS, &auth));
        checks.push(doctor::disk_check(
            listing,
            token.as_ref(),
            doctor::local_build(&root).as_deref(),
        ));
        let vm = match self.resolve_vm(None) {
            Ok(vm) => vm,
            Err(e) => {
                checks.push(Check::fail("vm", e.to_string(), e.hint()));
                return doctor_outcome(&checks);
            }
        };
        checks.push(Check::ok(
            "vm",
            format!("{} ({})", vm.name, vm.source.describe()),
        ));
        let adapter = match Self::adapter(&vm) {
            Ok(adapter) => adapter,
            Err(e) => {
                checks.push(Check::fail("vm hypervisor", e.to_string(), e.hint()));
                return doctor_outcome(&checks);
            }
        };
        checks.extend(doctor::vm_adapter_checks(
            &vm.entry,
            host::evaluate(&facts, &vm.entry.hypervisor),
            root.join("os/dev/vm.sh").is_file(),
        ));
        let caps = adapter.capabilities();
        checks.push(Check::ok(
            "debug capabilities",
            format!(
                "serial {}, screenshot {}, reset {}, control {}",
                yes(caps.serial),
                yes(caps.screenshot),
                yes(caps.reset),
                caps.control.unwrap_or("no")
            ),
        ));
        if let Some(disk) = vm.entry.disk.as_deref()
            && let Ok(meta) = std::fs::metadata(disk)
        {
            let limit = self.registry().map_or(u64::MAX, |r| r.warn_disk_bytes());
            let detail = format!("{disk} is {}", human_bytes(meta.len()));
            checks.push(if meta.len() > limit {
                Check::warn("vm disk", detail, "tilo-vm gc")
            } else {
                Check::ok("vm disk", detail)
            });
        }
        match self.connect(&vm) {
            Ok(mut remote) => self.vm_checks(&mut remote, &vm, &mut checks),
            Err(e) => checks.push(Check::fail("daemon", e.to_string(), e.hint())),
        }
        self.warnings.clear();
        doctor_outcome(&checks)
    }

    /// The checks that need the VM's daemon.
    fn vm_checks(&mut self, remote: &mut Remote, vm: &Resolved, checks: &mut Vec<Check>) {
        let hello = remote.hello.clone();
        checks.push(Check::ok(
            "daemon",
            format!(
                "tilo-vmd {} speaks protocol {}",
                hello.daemon_version, hello.protocol
            ),
        ));
        let tools = &hello.tools;
        let builds = describe_tools(tools);
        let upgrade = if tools.can_build() || self.vm_has_bootc(vm) == Some(true) {
            "tilo-vm vm upgrade   (a dev image with cargo, rsync and python3)"
        } else {
            "create a VM from a current dev image (tilo-vm vm create <name>); this one has no bootc, so vm upgrade cannot move it"
        };
        checks.push(if tools.can_build() {
            Check::ok("vm builder", builds)
        } else if cfg!(target_os = "linux") {
            Check::warn("vm builder", builds, upgrade)
        } else {
            Check::fail("vm builder", builds, upgrade)
        });
        if hello.arch == std::env::consts::ARCH || vm.entry.emulated {
            checks.push(Check::ok("arch", format!("VM is {}", hello.arch)));
        } else {
            checks.push(Check::warn(
                "arch",
                format!(
                    "VM is {} but this host is {}",
                    hello.arch,
                    std::env::consts::ARCH
                ),
                "host builds will not run there; build in the VM: --builder vm",
            ));
        }
        let Ok(status) = remote.call(Status { slot: None }) else {
            return;
        };
        let floor = self.registry().map_or(0, |r| r.min_free_bytes());
        if let Some(free) = status.storage.free_bytes {
            let detail = format!("{} free, floor {}", human_bytes(free), human_bytes(floor));
            checks.push(if free < floor {
                Check::fail("vm free space", detail, "tilo-vm gc")
            } else {
                Check::ok("vm free space", detail)
            });
        }
        for slot in status.slots.iter().filter(|s| s.user.is_some()) {
            match (&slot.parity, slot.session.as_str()) {
                (Some(problem), _) => checks.push(Check::fail(
                    "session parity",
                    format!("slot {}: {problem}", slot.name),
                    "slot session capabilities or SELinux domain differ from a greetd session; see tilo-vm guide troubleshooting",
                )),
                (None, "active") => checks.push(Check::ok(
                    "session parity",
                    format!(
                        "slot {} matches a greetd session (no capabilities, user domain)",
                        slot.name
                    ),
                )),
                _ => {}
            }
        }
        for issue in status
            .issues
            .into_iter()
            .filter(|i| !i.problem.contains("greetd session"))
        {
            checks.push(Check::warn(
                "vm state",
                format!("slot {}: {}", issue.slot, issue.problem),
                issue.hint,
            ));
        }
    }

    // ---- vm -------------------------------------------------------------------

    fn vm(&mut self, command: VmCommand) -> Result<Outcome, CliError> {
        match command {
            VmCommand::List => self.vm_list(),
            VmCommand::Use { name, global } => self.vm_use(&name, global),
            VmCommand::Ssh {
                raw,
                name,
                vm,
                command,
            } => self.vm_ssh(name.or(vm).as_deref(), raw, &command),
            VmCommand::Status { name } => self.vm_status(name.as_deref()),
            VmCommand::Create(args) => self.vm_create(args),
            VmCommand::Start { name } => self.vm_start(name.as_deref()),
            VmCommand::Restart { name } => {
                let stopped = self.vm_stop(name.as_deref(), false)?;
                if stopped.data.get("down") == Some(&json!(false)) {
                    return Ok(stopped);
                }
                let mut started = self.vm_start(name.as_deref())?;
                started.text = format!("{}\n{}", stopped.text, started.text);
                insert(&mut started.data, "stop", stopped.data);
                Ok(started)
            }
            VmCommand::Stop { name, force } => self.vm_stop(name.as_deref(), force),
            VmCommand::Rm { name, force } => self.vm_rm(&name, force),
            VmCommand::Upgrade(args) => self.vm_upgrade(&args),
            VmCommand::RollbackImage { name, no_reboot } => {
                self.vm_rollback_image(name.as_deref(), no_reboot)
            }
            VmCommand::Serial { name } => {
                let vm = self.resolve_vm(name.as_deref())?;
                let adapter = Self::adapter(&vm)?;
                let command = adapter
                    .serial(&vm.vm())
                    .ok_or_else(|| unsupported(adapter.as_ref(), "a serial console"))?;
                self.runner.interactive(&command)?;
                Ok(Outcome::new(
                    "",
                    json!({ "vm": vm.name, "command": command }),
                ))
            }
            VmCommand::Shot { name, output } => {
                let vm = self.resolve_vm(name.as_deref())?;
                let adapter = Self::adapter(&vm)?;
                let output = absolute(&output);
                ensure_parent(&output)?;
                let command = adapter
                    .screenshot(&vm.vm(), &output)
                    .ok_or_else(|| unsupported(adapter.as_ref(), "screenshots"))?;
                run_all(&mut self.runner, std::slice::from_ref(&command))?;
                Ok(Outcome::new(
                    format!("saved {}", output.display()),
                    json!({ "vm": vm.name, "path": output }),
                ))
            }
            VmCommand::Reset { name } => {
                let vm = self.resolve_vm(name.as_deref())?;
                let adapter = Self::adapter(&vm)?;
                let command = adapter
                    .reset(&vm.vm())
                    .ok_or_else(|| unsupported(adapter.as_ref(), "reset"))?;
                run_all(&mut self.runner, std::slice::from_ref(&command))?;
                Ok(Outcome::new(
                    format!("VM {} reset; every slot session restarts with it", vm.name),
                    json!({ "vm": vm.name }),
                )
                .next(&["tilo-vm vm status"]))
            }
            VmCommand::Control { vm, args } => {
                let vm = self.resolve_vm(vm.as_deref())?;
                let adapter = Self::adapter(&vm)?;
                let command = adapter
                    .control(&vm.vm(), &args)
                    .ok_or_else(|| unsupported(adapter.as_ref(), "native control"))?;
                let output = self.runner.output(&command)?;
                if !output.success {
                    return Err(CliError::CommandFailed {
                        command: command.display(),
                        status: output.status,
                        stderr: output.stderr,
                    });
                }
                Ok(Outcome::new(
                    output.stdout.trim_end(),
                    json!({ "stdout": output.stdout, "stderr": output.stderr }),
                ))
            }
        }
    }

    /// Shut down under the guest's VM lock, so no swap, gc or upgrade is cut
    /// off halfway; the lock is held until the VM is down.
    fn vm_stop(&mut self, name: Option<&str>, force: bool) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let adapter = Self::adapter(&vm)?;
        let mut lock = None;
        if !force {
            let endpoint = adapter.ssh_endpoint(&vm.vm(), &mut self.runner)?;
            match remote::hold_vm_lock(&endpoint, vm.entry.key.as_deref()) {
                LockOutcome::Held(held) => lock = Some(held),
                LockOutcome::Busy => {
                    return Err(CliError::Busy {
                        what: format!("VM {}", vm.name),
                    });
                }
                LockOutcome::Unreachable(detail) => self.warnings.push(format!(
                    "could not take the VM lock ({detail}); stopping without it"
                )),
            }
        }
        // A session holding logind's power-key lock (on seat0 it owns the
        // key) turns the ACPI request into its own power-key action, so the
        // guest is asked directly next, and powered off as a last resort.
        let poweroff = adapter
            .ssh_endpoint(&vm.vm(), &mut self.runner)
            .ok()
            .map(|endpoint| {
                remote::ssh_command(
                    &endpoint,
                    vm.entry.key.as_deref(),
                    &["systemctl".into(), "poweroff".into()],
                    false,
                )
            });
        let (down, step) = stop_escalating(
            &mut self.runner,
            adapter.as_ref(),
            &vm.vm(),
            poweroff.as_ref(),
            lock.as_mut(),
            StopGraces::default(),
        )?;
        match step {
            StopStep::Acpi => {}
            StopStep::GuestPoweroff => self.warnings.push(format!(
                "VM {} ignored the ACPI shutdown request (a session holding the power key?); \
                 it powered off on `systemctl poweroff` over SSH",
                vm.name
            )),
            StopStep::HardPowerOff => self.warnings.push(format!(
                "VM {} ignored both the ACPI shutdown request and `systemctl poweroff`; \
                 it was powered off hard",
                vm.name
            )),
        }
        let text = match down {
            Some(true) if lock.is_some() => format!(
                "VM {} shut down (it held the VM lock until it was down)",
                vm.name
            ),
            Some(true) => format!("VM {} shut down", vm.name),
            Some(false) => format!(
                "VM {} was asked to shut down but is still up; tilo-vm vm status",
                vm.name
            ),
            None => format!("VM {} is shutting down", vm.name),
        };
        drop(lock);
        Ok(Outcome::new(
            text,
            json!({ "vm": vm.name, "down": down, "step": step.as_str() }),
        )
        .next(&["tilo-vm vm start"]))
    }

    /// Delete a VM: power it off (with `force`), unregister it from the
    /// hypervisor, remove the files `vm create` made for it, and drop it
    /// from the registry. The image it was created from stays.
    fn vm_rm(&mut self, name: &str, force: bool) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(Some(name))?;
        let adapter = Self::adapter(&vm)?;
        // vm create gives every VM it makes its own key in keys/. A VM
        // registered by hand (a shared VM, say) is only forgotten: its
        // hypervisor, disk and key are not ours to delete.
        let own_key = sshkey::key_path(&self.registry_path, &vm.name);
        let owned = vm.entry.key.as_deref().map(Path::new) == Some(own_key.as_path());
        if owned && adapter.running(&vm.vm(), &mut self.runner) == Some(true) {
            if !force {
                return Err(CliError::VmRunning { vm: vm.name });
            }
            if let Some(off) = adapter.power_off(&vm.vm()) {
                run_all(&mut self.runner, &[off])?;
            }
            let started = Instant::now();
            while adapter.running(&vm.vm(), &mut self.runner) == Some(true) {
                if started.elapsed() > Duration::from_secs(60) {
                    return Err(CliError::VmRunning { vm: vm.name });
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        } else if owned
            && force
            && let Some(off) = adapter.power_off(&vm.vm())
        {
            // The adapter cannot tell whether it runs; a hard power-off of a
            // VM that is already off is harmless.
            let _ = self.runner.output(&off);
        }
        let mut removed = Vec::new();
        if owned {
            run_all(&mut self.runner, &adapter.destroy(&vm.vm()))?;
            let mut files = vm
                .entry
                .disk
                .as_deref()
                .map(hypervisor::vm_files)
                .unwrap_or_default();
            let key = own_key.display().to_string();
            files.push(remote::known_hosts_path(&key));
            files.push(sshkey::public_path(&own_key).display().to_string());
            files.push(key);
            for file in files {
                if remove_if_present(Path::new(&file))? {
                    removed.push(file);
                }
            }
        }
        let mut registry = self.registry()?;
        registry.vms.remove(&vm.name);
        if registry.default.as_deref() == Some(vm.name.as_str()) {
            registry.default = None;
        }
        registry.save(&self.registry_path)?;
        let mut text = if owned {
            format!("VM {} deleted and unregistered", vm.name)
        } else {
            format!(
                "VM {} unregistered; tilo-vm did not create it, so the hypervisor and its files are untouched",
                vm.name
            )
        };
        for file in &removed {
            let _ = write!(text, "\n  removed {file}");
        }
        if let Some(image) = &vm.entry.image {
            let _ = write!(text, "\n  kept its image {image}");
        }
        Ok(Outcome::new(
            text,
            json!({ "vm": vm.name, "deleted": owned, "removed": removed, "image": vm.entry.image }),
        )
        .next(&["tilo-vm vm list"]))
    }

    fn vm_list(&self) -> Result<Outcome, CliError> {
        let registry = self.registry()?;
        let current = self.resolve_vm(None).ok().map(|vm| vm.name);
        let mut lines = vec![format!("registry: {}", self.registry_path.display())];
        if registry.vms.is_empty() {
            lines.push(format!(
                "no VMs registered yet. Get one:\n  tilo-vm vm create <name>   downloads the published dev disk, creates and registers the VM\n  or ask a human which shared VM to use; they register it in {} (tilo-vm vm use <name> --global)",
                self.registry_path.display()
            ));
        }
        for (name, entry) in &registry.vms {
            let mut marks = Vec::new();
            if registry.default.as_ref() == Some(name) {
                marks.push("default");
            }
            if current.as_ref() == Some(name) {
                marks.push("in use here");
            }
            lines.push(format!(
                "  {name:<16} {:<8} {}",
                entry.hypervisor,
                marks.join(", ")
            ));
        }
        let mut outcome = Outcome::new(
            lines.join("\n"),
            json!({ "registry": self.registry_path, "vms": registry.vms, "default": registry.default, "current": current }),
        );
        if registry.vms.is_empty() {
            outcome = outcome.next(&["tilo-vm vm create <name>", "tilo-vm guide quickstart"]);
        }
        Ok(outcome)
    }

    fn vm_use(&self, name: &str, global: bool) -> Result<Outcome, CliError> {
        let mut registry = self.registry()?;
        if !registry.vms.contains_key(name) {
            return Err(CliError::UnknownVm {
                name: name.into(),
                registry: self.registry_path.display().to_string(),
                known: registry.vms.keys().cloned().collect(),
            });
        }
        let place = if global {
            registry.default = Some(name.into());
            registry.save(&self.registry_path)?;
            self.registry_path.clone()
        } else {
            let worktree = self.worktree.as_ref().ok_or(CliError::NotInWorktree)?;
            let file = worktree.root.join(WORKTREE_FILE);
            let old = std::fs::read_to_string(&file).unwrap_or_default();
            std::fs::write(&file, config::with_worktree_vm(&old, name))
                .map_err(CliError::io(format!("write {}", file.display())))?;
            file
        };
        Ok(Outcome::new(
            format!("using VM {name} (recorded in {})", place.display()),
            json!({ "vm": name, "file": place }),
        )
        .next(&["tilo-vm status"]))
    }

    fn vm_ssh(
        &mut self,
        name: Option<&str>,
        raw: bool,
        command: &[String],
    ) -> Result<Outcome, CliError> {
        if !raw && let Some(change) = sshguard::guarded_write(command) {
            return Err(CliError::RawWrite {
                command: command.join(" "),
                change,
            });
        }
        let vm = self.resolve_vm(name)?;
        let adapter = Self::adapter(&vm)?;
        let endpoint = adapter.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let line = remote::ssh_command(
            &endpoint,
            vm.entry.key.as_deref(),
            command,
            command.is_empty(),
        );
        let ok = self.runner.interactive(&line)?;
        // A remote command that fails has said why itself: pass its failure
        // on as the exit status, without a hint of ours.
        let mut outcome = Outcome::new("", json!({ "vm": vm.name, "command": command, "ok": ok }));
        outcome.failed = !ok && !command.is_empty();
        Ok(outcome)
    }

    fn vm_status(&mut self, name: Option<&str>) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let adapter = Self::adapter(&vm)?;
        let mut lines = vec![format!(
            "VM {} ({}, {})",
            vm.name,
            adapter.name(),
            vm.source.describe()
        )];
        let mut data = json!({ "vm": vm.name, "hypervisor": adapter.name(), "capabilities": adapter.capabilities() });
        match self.connect(&vm) {
            Ok(remote) => {
                lines.push(format!(
                    "  running and ready; tilo-vmd {} (protocol {}, {})",
                    remote.hello.daemon_version, remote.hello.protocol, remote.hello.arch
                ));
                insert(&mut data, "state", json!("ready"));
                insert(&mut data, "reachable", json!(true));
                insert(&mut data, "daemon", json!(remote.hello));
                Ok(Outcome::new(lines.join("\n"), data).next(&["tilo-vm status"]))
            }
            Err(error) => {
                let failure = self.connect_failure(&vm, adapter.as_ref(), &error);
                lines.push(failure.text(&error));
                insert(&mut data, "state", json!(failure.state));
                insert(&mut data, "reachable", json!(failure.ssh));
                insert(&mut data, "daemon", Value::Null);
                merge_error(&mut data, &error);
                let mut outcome = Outcome::new(lines.join("\n"), data);
                outcome.next = failure.next;
                outcome.failed = true;
                self.warnings.clear();
                Ok(outcome)
            }
        }
    }

    /// Everything `vm create` needs, checked before anything is downloaded
    /// or created: the hypervisor and its tools, a local `--image`, the
    /// published disk for the tag, the VM disk path, and free space.
    #[expect(
        clippy::too_many_lines,
        reason = "one preflight sequence for all VM creation inputs"
    )]
    fn preflight_create(
        &mut self,
        facts: &HostFacts,
        args: &CreateArgs,
        arch: &str,
    ) -> Result<Plan, CliError> {
        let choice = match args.hypervisor.as_deref() {
            Some(name) => {
                hypervisor::adapter(name)?;
                host::evaluate(facts, name)
            }
            None => host::auto(facts),
        }
        .map_err(CliError::NoHypervisor)?;
        if choice.adapter == "libvirt" {
            host::session_domain(facts, arch, args.emulated).map_err(CliError::NoHypervisor)?;
        }
        if choice.adapter == "qemu" && !self.base_dir().join("os/dev/vm.sh").is_file() {
            return Err(CliError::Unsupported {
                what: "the qemu adapter outside a Tilo worktree".into(),
                alternative: "run tilo-vm vm create from a Tilo worktree: the qemu adapter drives os/dev/vm.sh".into(),
            });
        }
        Self::validate_create_options(choice.adapter, args)?;
        if !sshkey::key_path(&self.registry_path, &args.name).exists() && !on_path("ssh-keygen") {
            return Err(CliError::ToolMissing {
                tool: "ssh-keygen".into(),
                purpose: "make the VM's root SSH key".into(),
            });
        }
        let format = disk::format_for(choice.adapter);
        let LocalDisks {
            image,
            image_format,
            import,
            boot_esp,
        } = check_local_disks(
            choice.adapter,
            args.image.as_deref(),
            args.boot_esp.as_deref(),
        )?;
        let published = if image.is_none() && args.template.is_none() {
            Some(self.resolve_published(
                args.tag.as_deref(),
                arch,
                format,
                choice.adapter == "whpx" && args.direct_boot_bundle.is_none(),
            )?)
        } else {
            None
        };
        let floor = vm_free_floor(self.registry()?.min_free_bytes(), choice.adapter);
        if let Some(published) = &published
            && !disk::cached(&published.dest, &published.disk)
        {
            let part = published.dest.with_extension("part");
            let have = std::fs::metadata(&part).map_or(0, |m| m.len());
            let needed = published.disk.size.saturating_sub(have);
            let dir = published.dest.parent().unwrap_or_else(|| Path::new("."));
            if let Some(free) = host_free(dir)
                && free < needed.saturating_add(floor)
            {
                return Err(CliError::NoSpace {
                    place: existing_ancestor(dir).display().to_string(),
                    free: human_bytes(free),
                    what: format!("the {} dev disk download", published.tag),
                    needed: human_bytes(needed.saturating_add(floor)),
                });
            }
        }
        let disk_path = match (&args.disk, image.is_some() || published.is_some()) {
            (Some(path), _) => Some(absolute(Path::new(path))),
            (None, true) => Some(
                xdg_paths::data_local_dir()
                    .unwrap_or_else(std::env::temp_dir)
                    .join("tilo-vm")
                    .join("vms")
                    .join(format!("{}.{format}", args.name)),
            ),
            (None, false) => None,
        };
        if let Some(path) = &disk_path {
            if path.exists() && choice.adapter != "utm" {
                return Err(CliError::DiskExists {
                    path: path.display().to_string(),
                });
            }
            let dir = path.parent().unwrap_or_else(|| Path::new("."));
            if let Some(free) = host_free(dir)
                && free < floor
            {
                return Err(CliError::LowDisk {
                    place: existing_ancestor(dir).display().to_string(),
                    free: human_bytes(free),
                    floor: human_bytes(floor),
                });
            }
        }
        Ok(Plan {
            choice,
            image,
            image_format,
            import,
            boot_esp,
            published,
            disk_path,
        })
    }

    fn validate_create_options(adapter: &str, args: &CreateArgs) -> Result<(), CliError> {
        if args.headless && adapter != "whpx" {
            return Err(CliError::Unsupported {
                what: format!("--headless with the {adapter} adapter"),
                alternative: "use --hypervisor whpx on Windows ARM64, or omit --headless".into(),
            });
        }
        if (args.heads.is_some() || args.resolution.is_some() || args.gl) && adapter != "libvirt" {
            return Err(CliError::Unsupported {
                what: format!("--heads, --resolution or --gl with the {adapter} adapter"),
                alternative: "use --hypervisor libvirt, or omit them".into(),
            });
        }
        if args.direct_boot_bundle.is_some() && adapter != "whpx" {
            return Err(CliError::Unsupported {
                what: format!("--direct-boot-bundle with the {adapter} adapter"),
                alternative: "use --hypervisor whpx on Windows ARM64".into(),
            });
        }
        if args.direct_boot_bundle.is_some() && args.image.is_none() {
            return Err(CliError::Unsupported {
                what: "--direct-boot-bundle with a published disk".into(),
                alternative: "omit --direct-boot-bundle: vm create downloads and verifies the matching CI bundle automatically; use --image for a local disk".into(),
            });
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one linear sequence: preflight, download, adapter, registry"
    )]
    fn vm_create(&mut self, args: CreateArgs) -> Result<Outcome, CliError> {
        let host = std::env::consts::ARCH;
        let arch = args.arch.clone().unwrap_or_else(|| host.to_owned());
        if arch != host && !args.emulated {
            return Err(CliError::ArchMismatch {
                host: host.into(),
                requested: arch,
            });
        }
        if arch != host {
            self.warnings.push(format!(
                "{arch} on a {host} host is emulated: expect everything to be many times slower"
            ));
        }
        let mut registry = self.registry()?;
        if let Some(existing) = registry.vms.get(&args.name) {
            return Ok(Outcome::new(
                format!(
                    "VM {} is already registered ({}); nothing to do",
                    args.name, existing.hypervisor
                ),
                json!({ "vm": args.name, "created": false }),
            )
            .next(&["tilo-vm vm start", "tilo-vm vm status"]));
        }
        let facts = HostFacts::probe(&mut self.runner);
        let plan = self.preflight_create(&facts, &args, &arch)?;
        let mut direct_kernel = args
            .direct_boot_bundle
            .as_deref()
            .map(|path| read_direct_boot_bundle(Path::new(path)))
            .transpose()?;
        let summary = plan.describe();
        if !self.json {
            eprintln!("tilo-vm: preflight passed\n{summary}");
        }
        self.receipt.set("preflight", &summary);
        let mut image = plan.image.as_ref().map(|p| p.display().to_string());
        let mut image_format = plan.image_format;
        if let (Some(source), Some(dest)) = (&image, &plan.import) {
            if !dest.is_file() {
                let qemu_img = plan.choice.tool("qemu-img").unwrap_or("qemu-img");
                let format = image_format.unwrap_or(image::Format::Raw);
                let runner = &mut self.runner;
                self.receipt.phase("import", || {
                    import_image(runner, qemu_img, source, format, dest)
                })?;
            }
            self.receipt.set(
                "import",
                json!({ "source": source, "copy": dest.display().to_string() }),
            );
            image = Some(dest.display().to_string());
            image_format = Some(image::Format::Qcow2);
        }
        let tag = plan.published.as_ref().map(|p| p.tag.clone());
        if let Some(published) = &plan.published {
            let runner = &mut self.runner;
            self.receipt.phase("download", || {
                disk::fetch(runner, &published.disk, &published.dest, &published.auth)
            })?;
            if let Some(bundle) = &published.direct_boot {
                let path = self.receipt.phase("direct-boot", || {
                    disk::fetch_direct_boot(
                        &mut self.runner,
                        published.dest.parent().unwrap_or_else(|| Path::new(".")),
                        bundle,
                        &published.auth,
                    )
                })?;
                direct_kernel = Some(read_published_direct_boot(
                    &path,
                    &published.tag,
                    &bundle.arch,
                )?);
                self.receipt.set(
                    "direct-boot",
                    json!({
                        "tag": published.tag,
                        "source_revision": published.source_revision,
                        "manifest": path,
                    }),
                );
            }
            self.receipt.set(
                "disk",
                json!({ "tag": published.tag, "file": published.disk.file, "sha256": published.disk.sha256 }),
            );
            image = Some(published.dest.display().to_string());
        }
        if plan.choice.adapter == "utm" && args.template.is_none() {
            return Err(CliError::Unsupported {
                what: "creating a UTM VM from a disk on the command line".into(),
                alternative: format!(
                    "import {} in UTM once (Create > Virtualize > Linux, use the existing disk), name it, then: tilo-vm vm create {} --template '<that name>'",
                    image.as_deref().unwrap_or("the dev disk"),
                    args.name
                ),
            });
        }
        if let Some(dir) = plan.disk_path.as_deref().and_then(Path::parent) {
            std::fs::create_dir_all(dir)
                .map_err(CliError::io(format!("create {}", dir.display())))?;
        }
        let key = sshkey::key_path(&self.registry_path, &args.name);
        let root_key = sshkey::ensure(&mut self.runner, &key, &args.name)?;
        // A new VM has new host keys: forget those of any earlier VM of this name.
        let known_hosts = remote::known_hosts_path(&key.display().to_string());
        match std::fs::remove_file(&known_hosts) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(CliError::io(format!("remove {known_hosts}"))(e));
            }
            _ => {}
        }
        let entry = VmEntry {
            hypervisor: plan.choice.adapter.to_owned(),
            ssh: args.ssh,
            key: Some(key.display().to_string()),
            image,
            disk: plan.disk_path.as_ref().map(|p| p.display().to_string()),
            arch: (arch != host).then_some(arch),
            emulated: args.emulated,
            memory_mb: args.memory_mb,
            headless: args.headless,
            heads: args.heads,
            gl: args.gl,
            resolution: args.resolution,
            cpus: args.cpus,
            ssh_port: args.ssh_port,
            template: args.template,
            image_format: image_format
                .filter(|f| *f == image::Format::Raw)
                .map(|f| f.name().to_owned()),
            boot_esp: plan.boot_esp.as_ref().map(|p| p.display().to_string()),
            qemu_img: plan.choice.tool("qemu-img").map(str::to_owned),
            qemu_system: plan.choice.tool("qemu-system-aarch64").map(str::to_owned),
            firmware_code: plan.choice.tool("firmware-code").map(str::to_owned),
            firmware_vars: plan.choice.tool("firmware-vars").map(str::to_owned),
            direct_kernel,
            ..VmEntry::default()
        };
        let adapter = hypervisor::adapter(&entry.hypervisor)?;
        let commands = adapter.create(
            &Vm {
                name: &args.name,
                entry: &entry,
            },
            &root_key,
        )?;
        run_all(&mut self.runner, &commands)?;
        registry.vms.insert(args.name.clone(), entry);
        if registry.default.is_none() {
            registry.default = Some(args.name.clone());
        }
        registry.save(&self.registry_path)?;
        let mut text = format!(
            "VM {} created with {} and registered in {}",
            args.name,
            plan.choice.describe(),
            self.registry_path.display()
        );
        if let (Some(tag), Some(image)) = (&tag, &entry_image(&registry, &args.name)) {
            let _ = write!(
                text,
                "\n  from the published dev disk {tag}, cached at {image} (the VM's disk is a copy-on-write child of it: keep that file)"
            );
        }
        let credentials = adapter.capabilities().credentials;
        if let Some(channel) = credentials {
            let _ = write!(
                text,
                "\n  root SSH key {} (passed to the VM as the systemd credential {}, in an {channel})",
                key.display(),
                sshkey::CREDENTIAL
            );
        } else {
            let _ = write!(
                text,
                "\n  root SSH key {}: {} has no credential channel, so install it once after the first boot; tilo-vm vm start prints the exact command (tilo-vm guide platforms)",
                key.display(),
                plan.choice.adapter
            );
        }
        if plan.choice.adapter == "libvirt" {
            let _ = write!(text, "\n  {}", host::VIRT_MANAGER_TIP);
        }
        let start = format!("tilo-vm vm start {}", args.name);
        let slot = "tilo-vm slot create --headless".to_owned();
        let mut outcome = Outcome::new(
            text,
            json!({ "vm": args.name, "created": true, "tag": tag, "hypervisor": plan.choice, "key": key, "key_as_credential": credentials, "commands": commands }),
        );
        outcome.next = vec![start, slot];
        Ok(outcome)
    }

    /// Find the published dev disk for `arch`/`format` (the newest tag when
    /// none is given) without downloading it.
    fn resolve_published(
        &mut self,
        tag: Option<&str>,
        arch: &str,
        format: &str,
        auto_direct_boot: bool,
    ) -> Result<Published, CliError> {
        let token = disk::find_token();
        let auth = disk::AuthHeader::new(token.as_ref())?;
        let local_build = doctor::local_build(&self.base_dir());
        let refused = |url: &str, status: u16| CliError::ForgeAuth {
            url: url.to_owned(),
            status,
            token: token.as_ref().map(|t| t.source.clone()),
        };
        let unexpected = |url: &str, status: u16, body: &str| CliError::CommandFailed {
            command: format!("curl {url}"),
            status: format!("HTTP {status}"),
            stderr: body.chars().take(300).collect(),
        };
        // Each architecture's lane publishes its own manifest, so the newest
        // tag may not have this host's yet: without --tag, take the newest
        // that does.
        let tags: Vec<String> = if let Some(tag) = tag {
            vec![tag.to_owned()]
        } else {
            let (status, body) = disk::get(&mut self.runner, disk::VERSIONS, &auth)?;
            match status {
                200 => {}
                401 | 403 => return Err(refused(disk::VERSIONS, status)),
                _ => return Err(unexpected(disk::VERSIONS, status, &body)),
            }
            let mut tags = disk::candidate_tags(&body);
            if tags.is_empty() {
                if token.is_none() {
                    self.warnings.push(
                        "no forge token was found, and the forge hides private packages from anonymous requests: export FORGEJO_TOKEN=<token with package read access>".into(),
                    );
                }
                return Err(CliError::NoDiskPublished {
                    tag: "(any)".into(),
                    url: disk::VERSIONS.into(),
                    local_build,
                });
            }
            tags.truncate(disk::TAGS_SEARCHED);
            tags
        };
        for tag in &tags {
            let url = disk::manifest_url(tag, arch);
            let (status, body) = disk::get(&mut self.runner, &url, &auth)?;
            match status {
                200 => {}
                401 | 403 => return Err(refused(&url, status)),
                404 => continue,
                _ => return Err(unexpected(&url, status, &body)),
            }
            let manifest: disk::DiskManifest =
                serde_json::from_str(&body).map_err(|e| CliError::CommandFailed {
                    command: format!("read {url}"),
                    status: "not a disk manifest".into(),
                    stderr: e.to_string(),
                })?;
            let chosen =
                manifest
                    .pick(arch, format)
                    .cloned()
                    .ok_or_else(|| CliError::NoDiskForHost {
                        tag: tag.clone(),
                        arch: arch.to_owned(),
                        format: format.to_owned(),
                        available: manifest.offered(),
                        local_build: local_build.clone(),
                    })?;
            let dest = disk::cache_path(tag, &chosen.file);
            let direct_boot = if auto_direct_boot {
                Some(manifest.direct_boot_for(tag, arch, &chosen)?)
            } else {
                None
            };
            return Ok(Published {
                tag: tag.clone(),
                disk: chosen,
                dest,
                direct_boot,
                source_revision: manifest.source_revision,
                auth,
            });
        }
        Err(self.no_disk_for_host(
            &tags,
            tag.is_some(),
            (arch, format),
            &auth,
            token.is_none(),
            local_build,
        ))
    }

    /// No tag in `tags` has `arch`: say what the newest offers instead.
    fn no_disk_for_host(
        &mut self,
        tags: &[String],
        explicit: bool,
        (arch, format): (&str, &str),
        auth: &disk::AuthHeader,
        anonymous: bool,
        local_build: Option<String>,
    ) -> CliError {
        let newest = tags.first().cloned().unwrap_or_default();
        let available = match disk::get(&mut self.runner, &disk::files_url(&newest), auth) {
            Ok((200, body)) => disk::offered_in_files(&newest, &body),
            _ => Vec::new(),
        };
        if explicit && available.is_empty() {
            if anonymous {
                self.warnings.push(
                    "no forge token was found, and the forge answers 404 for private packages: export FORGEJO_TOKEN=<token with package read access>".into(),
                );
            }
            return CliError::NoDiskPublished {
                url: disk::manifest_url(&newest, arch),
                tag: newest,
                local_build,
            };
        }
        if tags.len() > 1 {
            self.warnings.push(format!(
                "none of the {} newest dev disk tags ({}) has a {arch} disk yet",
                tags.len(),
                tags.join(", ")
            ));
        }
        CliError::NoDiskForHost {
            tag: newest,
            arch: arch.to_owned(),
            format: format.to_owned(),
            available,
            local_build,
        }
    }

    fn ensure_whpx_running(
        &mut self,
        vm: &Resolved,
        adapter: &dyn Hypervisor,
    ) -> Result<(), CliError> {
        if adapter.running(&vm.vm(), &mut self.runner) == Some(false) {
            return Err(CliError::Unreachable {
                vm: vm.name.clone(),
                detail: format!(
                    "QEMU exited before SSH answered; inspect {}.stderr.log and Windows Application errors",
                    vm.entry.disk.as_deref().unwrap_or("the VM disk")
                ),
                debug: Self::debug_commands(adapter, &vm.name),
            });
        }
        Ok(())
    }

    fn vm_start(&mut self, name: Option<&str>) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let floor = vm_free_floor(self.registry()?.min_free_bytes(), &vm.entry.hypervisor);
        if let Some(dir) = vm
            .entry
            .disk
            .as_deref()
            .map(Path::new)
            .and_then(Path::parent)
            && let Some(free) = host_free(dir)
            && free < floor
        {
            return Err(CliError::LowDisk {
                place: dir.display().to_string(),
                free: human_bytes(free),
                floor: human_bytes(floor),
            });
        }
        let adapter = Self::adapter(&vm)?;
        if adapter.running(&vm.vm(), &mut self.runner) != Some(true) {
            run_all(&mut self.runner, &adapter.start(&vm.vm()))?;
        }
        let started = Instant::now();
        let mut reachable = false;
        let mut no_daemon = None;
        let mut remote = None;
        let mut last_whpx_running_check = started;
        while started.elapsed() < Duration::from_secs(180) {
            match self.connect(&vm) {
                Ok(connected) => {
                    reachable = true;
                    remote = Some(connected);
                    break;
                }
                // SSH works; the image just has no daemon, and never will.
                Err(error @ CliError::DaemonMissing { .. }) => {
                    reachable = true;
                    no_daemon = Some(error);
                    break;
                }
                // Connection transport failures can resolve while the VM boots.
                Err(CliError::Unreachable { .. }) => {
                    if vm.entry.hypervisor == "whpx"
                        && last_whpx_running_check.elapsed() >= Duration::from_secs(2)
                    {
                        self.ensure_whpx_running(&vm, adapter.as_ref())?;
                        last_whpx_running_check = Instant::now();
                    }
                    // WHPX boots quickly enough that a two-second SSH poll
                    // accounts for a noticeable part of the restart time.
                    let interval = if vm.entry.hypervisor == "whpx" {
                        Duration::from_millis(500)
                    } else {
                        Duration::from_secs(2)
                    };
                    std::thread::sleep(interval);
                }
                // SSH and tilo-vmd already answered. Waiting cannot fix a refused
                // key, incompatible protocol, or any other semantic failure.
                Err(error) => return Err(error),
            }
        }
        self.receipt.record("boot", started.elapsed().as_secs_f64());
        if !reachable {
            return Err(CliError::Unreachable {
                vm: vm.name.clone(),
                detail: "no SSH answer within 180s of starting".into(),
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            });
        }
        self.warnings.extend(self.receipt.slow());
        let seconds = started.elapsed().as_secs_f64();
        if let Some(error) = no_daemon {
            return Ok(Outcome {
                next: vec![error.hint(), format!("tilo-vm vm ssh {}", vm.name)],
                ..Outcome::new(
                    format!(
                        "VM {} is up; SSH answered after {seconds:.0}s, but it runs no tilo-vmd (not a Tilo dev image), so slots and swaps cannot use it",
                        vm.name
                    ),
                    json!({ "vm": vm.name, "boot_seconds": seconds, "daemon": false }),
                )
            });
        }
        // Slots survive restarts, so only suggest creating one when none came back.
        let slots = remote
            .and_then(|mut remote| remote.call(Status { slot: None }).ok())
            .map(|status| status.slots)
            .unwrap_or_default();
        let next = match slots.first() {
            Some(slot) => vec![
                "tilo-vm status".to_owned(),
                format!("tilo-vm slot show --slot {}", slot.name),
            ],
            None => vec![
                "tilo-vm status".to_owned(),
                "tilo-vm slot create --headless".to_owned(),
            ],
        };
        Ok(Outcome {
            next,
            ..Outcome::new(
                format!("VM {} is up; SSH answered after {seconds:.0}s", vm.name),
                json!({ "vm": vm.name, "boot_seconds": seconds, "slots": slots.len() }),
            )
        })
    }

    /// Run one shell line in the VM as root, capturing its output.
    fn vm_sh(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        script: &str,
    ) -> Result<hypervisor::Output, CliError> {
        let line = remote::ssh_command(
            endpoint,
            vm.entry.key.as_deref(),
            &[script.to_owned()],
            false,
        );
        self.runner.output(&line)
    }

    /// Boot id, base.json, free space and bootc status, in one round trip.
    /// `command` is what needs them, named when the VM has no bootc.
    fn vm_facts(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        command: &str,
    ) -> Result<imageup::VmFacts, CliError> {
        let output = self.vm_sh(vm, endpoint, &imageup::vm_facts_script())?;
        if !output.stdout.contains("@@base") {
            let adapter = Self::adapter(vm)?;
            let error = CliError::Unreachable {
                vm: vm.name.clone(),
                detail: output.stderr.trim().to_owned(),
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            };
            return Err(key_refusal(vm, adapter.as_ref(), endpoint, error));
        }
        imageup::parse_vm_facts(&output.stdout).map_err(|error| match error {
            imageup::FactsError::NoBootc => CliError::NotBootc {
                vm: vm.name.clone(),
                command: command.to_owned(),
            },
            imageup::FactsError::Unreadable(detail) => CliError::CommandFailed {
                command: "bootc status --json".into(),
                status: format!("VM {} did not report a bootc status ({detail})", vm.name),
                stderr: output.stderr.trim().to_owned(),
            },
        })
    }

    fn vm_upgrade(&mut self, args: &UpgradeArgs) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(args.name.as_deref())?;
        let adapter = Self::adapter(&vm)?;
        let endpoint = adapter.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let Some(from) = args.from.as_deref() else {
            if let Some(layout) = args.image.as_deref().map(Path::new)
                && imageup::is_layout(layout)
            {
                return self.vm_upgrade_layout(&vm, &endpoint, args, layout);
            }
            return self.vm_upgrade_registry(&vm, &endpoint, args, args.image.as_deref());
        };
        let (image, build, base) = match self.upgrade_source(&vm, &endpoint, args, from)? {
            UpgradeSource::Local { image, build, base } => (image, build, base),
            UpgradeSource::Published(image) => {
                return self.vm_upgrade_registry(&vm, &endpoint, args, Some(&image));
            }
        };
        self.receipt.set("image", &image);
        if !imageup::is_dev_name(&image) {
            // Refused by name alone, before anything is built or inspected.
            imageup::check_image(&image, &imageup::ImageFacts::default())?;
        }
        // Fail on an unreachable or non-bootc VM before a long build.
        self.vm_facts(&vm, &endpoint, "vm upgrade")?;
        if build {
            self.build_dev_image(&image, base.as_deref(), args.rebuild_base)?;
        }
        let runner = &mut self.runner;
        let image_facts = self
            .receipt
            .phase("check", || local_image_facts(runner, &image))?;
        let layout = imageup::host_layout(&vm.name);
        let runner = &mut self.runner;
        let digest = self
            .receipt
            .phase("export", || export_layout(runner, &image, &layout))?;
        self.move_onto_layout(&vm, &endpoint, args, &image, &image_facts, &layout, digest)
    }

    /// `image build`: the dev image `--from local` builds, written as an OCI
    /// layout with its facts and digest (and with `disk`, the dev disk) into
    /// `out`, for `vm upgrade --image` and `vm create --image`. It needs no VM,
    /// so it runs anywhere the checkout is, including a copy without git.
    /// The checkout `image` commands build from: the worktree, or (without git)
    /// the tree around the working directory.
    fn checkout(&self) -> Result<PathBuf, CliError> {
        match &self.worktree {
            Some(worktree) => Ok(worktree.root.clone()),
            None => std::env::current_dir()
                .ok()
                .and_then(|cwd| checkout_root(&cwd))
                .ok_or(CliError::NotInWorktree),
        }
    }

    /// `image disk`: the disk step of the build, for an image `image build`
    /// wrote (here or on another machine): the layout into podman's storage,
    /// then `os/dev/build-image.sh` with `TILO_SKIP_BUILD=1`, as
    /// `build-dev-image.sh` runs it.
    fn image_disk(&mut self, layout: &Path) -> Result<Outcome, CliError> {
        let started = Instant::now();
        let root = self.checkout()?;
        let local_image = imageup::local_dev_image()?;
        let layout = if layout.is_absolute() {
            layout.to_path_buf()
        } else {
            root.join(layout)
        };
        if !imageup::is_layout(&layout) {
            return Err(CliError::ImageRefused {
                image: layout.display().to_string(),
                problem: "is not an OCI layout".into(),
                fix:
                    "build one: tilo-vm image build (tilo-vm image build --help to build elsewhere)"
                        .into(),
            });
        }
        let facts_file = layout.with_extension("facts");
        let facts_text = std::fs::read_to_string(&facts_file)
            .map_err(CliError::io(format!("read {}", facts_file.display())))?;
        let (image, facts) =
            imageup::parse_layout_facts(&facts_text).map_err(|problem| CliError::ImageRefused {
                image: layout.display().to_string(),
                problem,
                fix: "rebuild it: tilo-vm image build".into(),
            })?;
        imageup::check_image(&image, &facts)?;
        let out = layout
            .parent()
            .map_or_else(|| root.clone(), Path::to_path_buf);
        let scratch = build_scratch(&out);
        let import = imageup::import_layout_command(&layout, &local_image);
        let runner = &mut self.runner;
        let imported = self.receipt.phase("import", || runner.output(&import))?;
        if !imported.success {
            return Err(CliError::CommandFailed {
                command: import.display(),
                status: imported.status,
                stderr: imported.stderr.trim().to_owned(),
            });
        }
        let command = image_disk_command(&root.join(imageup::DISK_SCRIPT), &local_image, &scratch);
        self.runner.cwd = Some(root);
        let runner = &mut self.runner;
        let written = self
            .receipt
            .phase("disk", || runner.interactive(&command))?;
        // Root podman's copy of the image is root's; only sudo removes it.
        let drop_store = CommandLine::new("sudo", &["-n", "rm", "-rf", "--"])
            .arg(root_podman_store(&scratch).display().to_string());
        if !self.runner.output(&drop_store)?.success {
            self.warnings.push(format!(
                "could not remove {}; remove it with: {}",
                root_podman_store(&scratch).display(),
                drop_store.display()
            ));
        }
        if !written {
            return Err(CliError::CommandFailed {
                command: command.display(),
                status: "disk step failed".into(),
                stderr: "the output above names the cause".into(),
            });
        }
        let dest = move_built_disk(&scratch, &out)?;
        Ok(Outcome::new(
            format!(
                "wrote {} from {image} in {}s\n  {}",
                dest.display(),
                started.elapsed().as_secs(),
                self.receipt.summary()
            ),
            json!({ "disk": dest.display().to_string(), "image": image }),
        )
        .next(&[&format!(
            "tilo-vm vm create <name> --image {}",
            dest.display()
        )]))
    }

    fn image_build(&mut self, disk: bool, out: &Path) -> Result<Outcome, CliError> {
        let started = Instant::now();
        let root = self.checkout()?;
        let script = root.join(imageup::BUILD_SCRIPT);
        if !script.is_file() {
            return Err(CliError::Unsupported {
                what: format!("image build without {}", imageup::BUILD_SCRIPT),
                alternative: "run it from a Tilo checkout".into(),
            });
        }
        let out = if out.is_absolute() {
            out.to_path_buf()
        } else {
            root.join(out)
        };
        let scratch = build_scratch(&out);
        std::fs::create_dir_all(&out).map_err(CliError::io(format!("create {}", out.display())))?;
        if self.worktree.is_none() && std::env::var_os("SOURCE_DATE_EPOCH").is_none() {
            self.warnings.push("no git checkout here and no SOURCE_DATE_EPOCH: the image is dated 1970, so a VM on a newer base refuses it without --allow-older-base; pass the commit time (SOURCE_DATE_EPOCH=$(git log -1 --format=%ct))".into());
        }
        let local_image = imageup::local_dev_image()?;
        let command = image_build_command(&script, &local_image, disk, &scratch);
        eprintln!(
            "tilo-vm: building {local_image} ({}); cached steps are skipped",
            if disk { "and its disk" } else { "no disk" }
        );
        self.runner.cwd = Some(root);
        let runner = &mut self.runner;
        let built = self
            .receipt
            .phase("image-build", || runner.interactive(&command))?;
        if !built {
            return Err(CliError::CommandFailed {
                command: command.display(),
                status: "build failed".into(),
                stderr: "the build output above names the failed step; rerunning resumes there"
                    .into(),
            });
        }
        let layout = out.join(imageup::BUILT_LAYOUT);
        let runner = &mut self.runner;
        let digest = self
            .receipt
            .phase("export", || export_layout(runner, &local_image, &layout))?;
        let runner = &mut self.runner;
        let facts_text = self
            .receipt
            .phase("check", || image_facts_text(runner, &local_image))?;
        let (image, facts) =
            imageup::parse_layout_facts(&facts_text).map_err(|detail| CliError::CommandFailed {
                command: imageup::inspect_command(&local_image).display(),
                status: "unreadable output".into(),
                stderr: detail,
            })?;
        imageup::check_image(&image, &facts)?;
        let facts_file = layout.with_extension("facts");
        std::fs::write(&facts_file, &facts_text)
            .map_err(CliError::io(format!("write {}", facts_file.display())))?;
        let mut next = vec![format!("tilo-vm vm upgrade --image {}", layout.display())];
        let mut disk_path = None;
        if disk {
            let dest = move_built_disk(&scratch, &out)?;
            next.insert(
                0,
                format!("tilo-vm vm create <name> --image {}", dest.display()),
            );
            disk_path = Some(dest.display().to_string());
        }
        let data = json!({
            "image": image,
            "digest": digest,
            "layout": layout.display().to_string(),
            "layout_bytes": imageup::layout_bytes(&layout),
            "disk": disk_path,
            "seconds": started.elapsed().as_secs(),
        });
        self.receipt.set("digest", &digest);
        let text = format!(
            "built {image} ({}) in {}s\n  layout: {} ({}){}\n  {}",
            imageup::short_digest(&digest),
            started.elapsed().as_secs(),
            layout.display(),
            human_bytes(imageup::layout_bytes(&layout)),
            disk_path
                .as_deref()
                .map(|d| format!("\n  disk:   {d}"))
                .unwrap_or_default(),
            self.receipt.summary()
        );
        let next: Vec<&str> = next.iter().map(String::as_str).collect();
        Ok(Outcome::new(text, data).next(&next))
    }

    /// `--image <dir>`: an OCI layout `tilo-vm image build` wrote, here or on
    /// another machine, with its facts and digest beside it.
    fn vm_upgrade_layout(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        args: &UpgradeArgs,
        layout: &Path,
    ) -> Result<Outcome, CliError> {
        let read = |path: PathBuf| {
            std::fs::read_to_string(&path).map_err(|error| CliError::ImageRefused {
                image: layout.display().to_string(),
                problem: format!("has no {} ({error})", path.display()),
                fix: "write the layout with tilo-vm image build (it saves the image's facts and digest beside it)".into(),
            })
        };
        let (image, image_facts) = imageup::parse_layout_facts(&read(
            layout.with_extension("facts"),
        )?)
        .map_err(|problem| CliError::ImageRefused {
            image: layout.display().to_string(),
            problem,
            fix: "rebuild it: tilo-vm image build".into(),
        })?;
        let digest = read(layout.with_extension("digest"))?.trim().to_owned();
        self.receipt.set("image", &image);
        self.receipt.set("layout", layout.display().to_string());
        self.move_onto_layout(vm, endpoint, args, &image, &image_facts, layout, digest)
    }

    /// The image is in `layout` with `digest`: check it, send the VM its new
    /// blobs, and boot it.
    fn move_onto_layout(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        args: &UpgradeArgs,
        image: &str,
        image_facts: &imageup::ImageFacts,
        layout: &Path,
        digest: String,
    ) -> Result<Outcome, CliError> {
        imageup::check_image(image, image_facts)?;
        let facts = self.vm_facts(vm, endpoint, "vm upgrade")?;
        match imageup::check_base_age(image, facts.base.as_ref(), image_facts.base.as_ref()) {
            Err(error) if !args.allow_older_base => return Err(error),
            Err(error) => self
                .warnings
                .push(format!("{error}; going ahead (--allow-older-base)")),
            Ok(Some(note)) => self.warnings.push(note),
            Ok(None) => {}
        }
        let floor = self.registry()?.min_free_bytes();
        if let Some(free) = facts.free_bytes
            && free < floor
        {
            return Err(CliError::LowDisk {
                place: format!("VM {}", vm.name),
                free: human_bytes(free),
                floor: human_bytes(floor),
            });
        }
        self.receipt.set("digest", &digest);
        let layout_bytes = imageup::layout_bytes(layout);
        if facts.bootc.staged.is_none()
            && facts
                .bootc
                .booted
                .as_ref()
                .is_some_and(|b| b.digest == digest)
        {
            return Ok(Outcome::new(
                format!(
                    "VM {} already runs {image} ({}); nothing to do\n  {}",
                    vm.name,
                    imageup::short_digest(&digest),
                    self.receipt.summary()
                ),
                json!({ "vm": vm.name, "image": image, "digest": digest, "changed": false }),
            )
            .next(&["tilo-vm status"]));
        }
        let transferred = self.send_layout(vm, endpoint, layout)?;
        self.receipt.set("transferred_bytes", transferred);
        self.receipt.set("image_bytes", layout_bytes);
        if !self.json {
            eprintln!(
                "tilo-vm: sent {} of the image's {} to VM {} ({})",
                transferred.map_or_else(|| "?".into(), human_bytes),
                human_bytes(layout_bytes),
                vm.name,
                imageup::VM_LAYOUT
            );
        }
        let stage = Stage {
            transport: "oci",
            imgref: imageup::layout_imgref(),
            expected: Some(digest),
            no_reboot: args.no_reboot,
        };
        self.stage_and_boot(vm, endpoint, &facts, &stage)
    }

    /// What `--from` resolves to.
    fn upgrade_source(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        args: &UpgradeArgs,
        from: &str,
    ) -> Result<UpgradeSource, CliError> {
        match imageup::parse_from(from, self.worktree.as_ref())? {
            imageup::UpgradeFrom::Local { image, build } => {
                if !build && (args.base.is_some() || args.rebuild_base) {
                    return Err(CliError::Unsupported {
                        what: "--base or --rebuild-base with --from local:<image>".into(),
                        alternative: "those choose how --from local builds; an image you name is used as it is: tilo-vm vm upgrade --from local --rebuild-base".into(),
                    });
                }
                Ok(UpgradeSource::Local {
                    image,
                    build,
                    base: args.base.clone(),
                })
            }
            imageup::UpgradeFrom::Ci { tag } => {
                if args.base.is_some() || args.rebuild_base {
                    return Err(CliError::Unsupported {
                        what: "--base or --rebuild-base with --from ci:<tag>".into(),
                        alternative: "the CI build is the base: tilo-vm vm upgrade --from ci:<tag>   |   or build one here: tilo-vm vm upgrade --from local --rebuild-base".into(),
                    });
                }
                // Fail on an unreachable or non-bootc VM before looking for images.
                self.vm_facts(vm, endpoint, "vm upgrade")?;
                // WHPX on Windows needs no host-side image tools. Main dev
                // tags are deterministic, and bootc pulls inside the VM.
                if cfg!(windows)
                    && tag.starts_with("main-")
                    && let Some(image) = imageup::ci_candidates(&tag, std::env::consts::ARCH)
                        .dev_images
                        .into_iter()
                        .next()
                {
                    return Ok(UpgradeSource::Published(image));
                }
                Ok(match self.resolve_ci_upgrade(&tag)? {
                    CiUpgrade::Published(image) => UpgradeSource::Published(image),
                    CiUpgrade::Base(base) => UpgradeSource::Local {
                        image: imageup::ci_dev_image(&base),
                        build: true,
                        base: Some(base),
                    },
                })
            }
        }
    }

    /// `--from ci:<tag>`: the dev image dev-disk.yml published for that build,
    /// which the VM pulls itself; else the production image it was built FROM,
    /// pulled here so a dev image can be built on top of it.
    fn resolve_ci_upgrade(&mut self, tag: &str) -> Result<CiUpgrade, CliError> {
        let candidates = imageup::ci_candidates(tag, std::env::consts::ARCH);
        let runner = &mut self.runner;
        let found = self.receipt.phase("resolve", || -> Result<_, CliError> {
            for image in &candidates.dev_images {
                if runner.output(&imageup::exists_command(image))?.success {
                    return Ok(Some(CiUpgrade::Published(image.clone())));
                }
            }
            for base in &candidates.bases {
                if runner.output(&imageup::exists_command(base))?.success {
                    return Ok(Some(CiUpgrade::Base(base.clone())));
                }
            }
            // Not a main build: a branch or pull request build of the commit.
            let Some(sha) = imageup::ci_commit(tag) else {
                return Ok(None);
            };
            let mut listing = |repository| -> Result<String, CliError> {
                let output = runner.output(&imageup::list_tags_command(repository))?;
                Ok(if output.success {
                    output.stdout
                } else {
                    String::new()
                })
            };
            let vm_tags = listing("registry.tilo.so/tilo/tilo-vm")?;
            let ci_tags = listing("registry.tilo.so/tilo/tilo-ci")?;
            let refs = imageup::ci_ref_candidates(sha, std::env::consts::ARCH, &vm_tags, &ci_tags);
            if let Some(image) = refs.dev_images.first() {
                return Ok(Some(CiUpgrade::Published(image.clone())));
            }
            Ok(refs.bases.first().cloned().map(CiUpgrade::Base))
        })?;
        let Some(found) = found else {
            let tried = candidates
                .dev_images
                .iter()
                .chain(&candidates.bases)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CliError::ImageRefused {
                image: format!("ci:{tag}"),
                problem: format!("names no CI build this host can pull (tried {tried})"),
                fix: "list the builds CI pushed: skopeo list-tags docker://registry.tilo.so/tilo/tilo-ci   |   main and pull request builds are tagged <ref>-<sha7>: tilo-vm vm upgrade --from sha:<commit>   |   or build this worktree: tilo-vm vm upgrade --from local".into(),
            });
        };
        if let CiUpgrade::Base(base) = &found {
            eprintln!(
                "tilo-vm: no dev image is published for ci:{tag}; pulling its base {base} to build one here"
            );
            let pull = CommandLine::new(
                "podman",
                &["pull", "--arch", ci::podman_arch(std::env::consts::ARCH)],
            )
            .arg(base.clone());
            let runner = &mut self.runner;
            let pulled = self.receipt.phase("pull", || runner.interactive(&pull))?;
            if !pulled {
                return Err(CliError::CommandFailed {
                    command: pull.display(),
                    status: "pull failed".into(),
                    stderr: "the podman output above names the cause".into(),
                });
            }
        }
        Ok(found)
    }

    /// `vm upgrade`, `vm upgrade --image` and a published `--from ci:`: bootc
    /// pulls from a registry.
    fn vm_upgrade_registry(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        args: &UpgradeArgs,
        image: Option<&str>,
    ) -> Result<Outcome, CliError> {
        if let Some(image) = image
            && !imageup::is_dev_name(image)
        {
            imageup::check_image(image, &imageup::ImageFacts::default())?;
        }
        let facts = self.vm_facts(vm, endpoint, "vm upgrade")?;
        let (transport, imgref) = match image {
            Some(image) => ("registry", image.to_owned()),
            None => match facts.bootc.spec.clone() {
                Some((image, transport)) if transport == "oci" => ("oci", image),
                Some((image, _)) => ("registry", image),
                None => ("registry", String::new()),
            },
        };
        self.receipt.set("image", &imgref);
        let stage = Stage {
            transport,
            imgref,
            expected: None,
            no_reboot: args.no_reboot,
        };
        self.stage_and_boot(vm, endpoint, &facts, &stage)
    }

    /// rsync the layout into the VM; the bytes that crossed, when rsync says.
    fn send_layout(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        layout: &Path,
    ) -> Result<Option<u64>, CliError> {
        if !on_path("rsync") {
            return Err(CliError::ToolMissing {
                tool: "rsync".into(),
                purpose: "send the image's new layers to the VM".into(),
            });
        }
        let rsync = imageup::rsync_command(layout, endpoint, vm.entry.key.as_deref())?;
        let runner = &mut self.runner;
        let sent = self.receipt.phase("transfer", || runner.output(&rsync))?;
        if sent.success {
            return Ok(imageup::rsync_sent_bytes(&sent.stdout));
        }
        let mut stderr = sent.stderr.trim().to_owned();
        if stderr.contains("connection unexpectedly closed") {
            stderr.push_str(" (the SSH transport or remote rsync command closed before transferring data; check the output above)");
        }
        Err(CliError::CommandFailed {
            command: rsync.display(),
            status: sent.status,
            stderr,
        })
    }

    /// `--from local`: the dev image build, which skips every cached step.
    fn build_dev_image(
        &mut self,
        image: &str,
        base: Option<&str>,
        rebuild_base: bool,
    ) -> Result<(), CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let script = worktree.root.join(imageup::BUILD_SCRIPT);
        if !script.is_file() {
            return Err(CliError::Unsupported {
                what: format!("--from local without {}", imageup::BUILD_SCRIPT),
                alternative: "run from a Tilo worktree, or name an image already built: tilo-vm vm upgrade --from local:localhost/tilo-vm:<tag>".into(),
            });
        }
        let base_build = if rebuild_base {
            "1"
        } else if base.is_some() {
            "0"
        } else {
            "auto"
        };
        let mut command = CommandLine::new(&script.display().to_string(), &[])
            .env("TILO_DEV_DISK", "0")
            .env("TILO_DEV_IMAGE", image)
            .env("TILO_DEV_BASE_BUILD", base_build);
        if let Some(base) = base {
            command = command.env("TILO_DEV_BASE", base);
        }
        let base_name = match base {
            Some(base) => base.to_owned(),
            None => imageup::local_base()?,
        };
        eprintln!(
            "tilo-vm: building {image} FROM {base_name} ({}); podman's layer cache skips unchanged steps",
            match base_build {
                "1" => "rebuilding the base first",
                "0" => "the base as it is",
                _ => "the base is rebuilt only if its inputs changed",
            }
        );
        self.runner.cwd = Some(worktree.root);
        let runner = &mut self.runner;
        let built = self
            .receipt
            .phase("image-build", || runner.interactive(&command))?;
        if built {
            Ok(())
        } else {
            Err(CliError::CommandFailed {
                command: command.display(),
                status: "build failed".into(),
                stderr: "the build output above names the cause".into(),
            })
        }
    }

    /// Run `words` under the VM lock; exit 75 means another command holds it.
    fn vm_locked(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        words: &[String],
    ) -> Result<hypervisor::Output, CliError> {
        let script = imageup::under_vm_lock(words);
        let runner = &mut self.runner;
        let output = self.receipt.phase("stage", || {
            runner.output(&remote::ssh_command(
                endpoint,
                vm.entry.key.as_deref(),
                &[script],
                false,
            ))
        })?;
        if !output.success && output.status.contains("75") {
            return Err(CliError::Busy {
                what: format!("VM {}", vm.name),
            });
        }
        Ok(output)
    }

    /// Stage an image with bootc under the VM lock, reboot into it, wait for
    /// SSH and tilo-vmd, and check the VM booted what was staged.
    #[expect(
        clippy::too_many_lines,
        reason = "one checked bootc staging transaction"
    )]
    fn stage_and_boot(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        facts: &imageup::VmFacts,
        stage: &Stage,
    ) -> Result<Outcome, CliError> {
        // The image is the VM's rollback deployment already (after rollback-image):
        // bootc will not stage it again, but rolling back boots exactly it.
        let mut reuse = facts.bootc.staged.is_none()
            && stage.expected.is_some()
            && facts.bootc.rollback.as_ref().map(|r| &r.digest) == stage.expected.as_ref();
        let words = if reuse {
            vec!["bootc".to_owned(), "rollback".to_owned()]
        } else {
            imageup::stage_command(&facts.bootc, stage.transport, &stage.imgref)
        };
        let mut staged = self.vm_locked(vm, endpoint, &words)?;
        if !staged.success && imageup::is_already_rollback(&staged.stderr) {
            // bootc resolved the requested tag to its existing rollback image.
            staged = self.vm_locked(vm, endpoint, &["bootc".into(), "rollback".into()])?;
            reuse = staged.success;
        }
        if !staged.success && imageup::is_already_booted(&staged.stderr) {
            // The same image under another manifest digest (a disk installs
            // from a recompressed registry copy of the layout's image).
            return Ok(Outcome::new(
                format!(
                    "VM {} already runs this image (same composefs digest); nothing to do\n  {}",
                    vm.name,
                    self.receipt.summary()
                ),
                json!({ "vm": vm.name, "image": stage.imgref, "changed": false }),
            )
            .next(&["tilo-vm status"]));
        }
        if !staged.success {
            return Err(CliError::CommandFailed {
                command: words.join(" "),
                status: staged.status,
                stderr: staged.stderr.trim().to_owned(),
            });
        }
        let status = self.bootc_status(vm, endpoint)?;
        let previous = status.booted;
        let previous_text = previous
            .as_ref()
            .map_or_else(|| "(unknown)".into(), imageup::Deployment::describe);
        let queued = if reuse {
            facts.bootc.rollback.clone()
        } else {
            None
        };
        let Some(next) = status.staged.or(queued) else {
            if stage.expected.is_none()
                || previous.as_ref().map(|b| &b.digest) == stage.expected.as_ref()
            {
                return Ok(Outcome::new(
                    format!(
                        "VM {} already runs the newest {previous_text}; nothing to do",
                        vm.name
                    ),
                    json!({ "vm": vm.name, "changed": false, "booted": previous.map(|p| p.digest) }),
                )
                .next(&["tilo-vm status"]));
            }
            return Err(CliError::CommandFailed {
                command: words.join(" "),
                status: "staged nothing".into(),
                stderr: format!(
                    "bootc reported no staged deployment; the VM still runs {previous_text}"
                ),
            });
        };
        if let Some(want) = &stage.expected
            && &next.digest != want
        {
            return Err(CliError::CommandFailed {
                command: words.join(" "),
                status: "staged another image".into(),
                stderr: format!("expected {want}, bootc staged {}", next.digest),
            });
        }
        self.keep_esp_writable(vm, endpoint)?;
        if stage.no_reboot {
            return Ok(Outcome::new(
                format!(
                    "staged {} on VM {}; it boots at the next restart (now running {previous_text})\n  {}",
                    next.describe(),
                    vm.name,
                    self.receipt.summary()
                ),
                json!({ "vm": vm.name, "changed": true, "staged": next.digest, "booted": previous.map(|p| p.digest) }),
            )
            .next(&["tilo-vm vm stop", "tilo-vm vm start"]));
        }
        let booted = self.reboot_into(vm, endpoint, &facts.boot_id, &next.digest)?;
        self.warnings.extend(self.receipt.slow());
        Ok(Outcome::new(
            format!(
                "VM {} now runs {}\n  before: {previous_text}, kept for tilo-vm vm rollback-image\n  slots and layers are unchanged (they live in /var)\n  {}",
                vm.name,
                booted.describe(),
                self.receipt.summary()
            ),
            json!({
                "vm": vm.name,
                "changed": true,
                "image": stage.imgref,
                "transport": stage.transport,
                "digest": booted.digest,
                "previous": previous.map(|p| p.digest),
            }),
        )
        .next(&["tilo-vm status", "tilo-vm vm rollback-image"]))
    }

    /// So the shutdown can apply the staged deployment (see
    /// `imageup::ESP_WRITABLE_UNTIL_BOOT`).
    fn keep_esp_writable(&mut self, vm: &Resolved, endpoint: &SshEndpoint) -> Result<(), CliError> {
        let writable = self.vm_sh(vm, endpoint, imageup::ESP_WRITABLE_UNTIL_BOOT)?;
        if !writable.success {
            self.warnings.push(format!(
                "could not make /boot writable in VM {} ({}); bootc may fail to apply the new image at shutdown",
                vm.name,
                writable.stderr.trim()
            ));
        }
        Ok(())
    }

    fn bootc_status(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
    ) -> Result<imageup::BootcStatus, CliError> {
        let output = self.vm_sh(vm, endpoint, &imageup::bootc_line(&["status", "--json"]))?;
        imageup::parse_bootc_status(&output.stdout).map_err(|detail| CliError::CommandFailed {
            command: "bootc status --json".into(),
            status: output.status,
            stderr: format!("{detail}; {}", output.stderr.trim()),
        })
    }

    /// WHPX boots a host-side kernel to avoid the broken QEMU UEFI reset path.
    /// Before a bootc image switch, copy only that deployment's kernel and
    /// initrd from the VM and point the next QEMU start at its BLS options.
    fn prepare_whpx_direct_boot(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        digest: &str,
    ) -> Result<VmEntry, CliError> {
        let status = self.vm_sh(vm, endpoint, "bootc status --json")?;
        let value: Value = serde_json::from_str(&status.stdout).map_err(|e| CliError::Config {
            path: "bootc status --json".into(),
            detail: e.to_string(),
        })?;
        let deployment = ["staged", "rollback"]
            .iter()
            .filter_map(|name| value.pointer(&format!("/status/{name}")))
            .find(|entry| {
                entry.pointer("/image/imageDigest").and_then(Value::as_str) == Some(digest)
            })
            .ok_or_else(|| CliError::Config {
                path: "bootc status --json".into(),
                detail: format!("target {digest} is neither staged nor rollback"),
            })?;
        let verity = deployment
            .pointer("/composefs/verity")
            .and_then(Value::as_str)
            .filter(|s| s.len() == 128 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| CliError::Config {
                path: "bootc status --json".into(),
                detail: "target deployment has no valid composefs verity".into(),
            })?;
        let bls = self.vm_sh(
            vm,
            endpoint,
            &format!(
                "grep -l -F 'composefs={verity}' /boot/loader/entries.staged/*.conf /boot/loader/entries/*.conf 2>/dev/null"
            ),
        )?;
        let mut paths = bls.stdout.lines();
        let path = paths
            .next()
            .filter(|path| {
                bls.success
                    && paths.next().is_none()
                    && (path.starts_with("/boot/loader/entries.staged/")
                        || path.starts_with("/boot/loader/entries/"))
                    && path
                        .rsplit_once('.')
                        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("conf"))
                    && !path.contains("..")
            })
            .ok_or_else(|| CliError::Config {
                path: "/boot/loader/entries".into(),
                detail: format!("expected one BLS entry for composefs={verity}"),
            })?;
        let entry = self.vm_sh(vm, endpoint, &format!("cat '{path}'"))?;
        let field = |prefix: &str| {
            entry
                .stdout
                .lines()
                .find_map(|line| line.strip_prefix(prefix).map(str::trim))
        };
        // bootc stages the new BLS options before shutdown, in
        // entries.staged. If the kernel did not change, that entry correctly
        // reuses the previous deployment's kernel and initrd directory.
        let base = field("linux ")
            .and_then(|path| path.strip_suffix("vmlinuz"))
            .ok_or_else(|| CliError::Config {
                path: path.into(),
                detail: "BLS kernel and initrd do not match the staged deployment".into(),
            })?;
        let kernel_verity = base
            .strip_prefix("/EFI/Linux/bootc_composefs-")
            .and_then(|hash| hash.strip_suffix('/'));
        let valid_kernel = kernel_verity.is_some_and(|hash| {
            hash.len() == 128 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        if !valid_kernel || field("initrd ") != Some(format!("{base}initrd").as_str()) {
            return Err(CliError::Config {
                path: path.into(),
                detail: "BLS kernel and initrd do not match the staged deployment".into(),
            });
        }
        let options = field("options ")
            .filter(|s| {
                s.split_whitespace().any(|arg| arg == "root=tilo-auto")
                    && s.split_whitespace()
                        .any(|arg| arg == format!("composefs={verity}"))
            })
            .ok_or_else(|| CliError::Config {
                path: path.into(),
                detail: "BLS options do not name the staged Tilo root".into(),
            })?;
        let disk = vm.entry.disk.as_deref().ok_or_else(|| CliError::Config {
            path: self.registry_path.display().to_string(),
            detail: "WHPX VM has no disk path".into(),
        })?;
        let dest = Path::new(disk)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("direct-boot")
            .join(&vm.name)
            .join(verity);
        std::fs::create_dir_all(&dest)
            .map_err(CliError::io("create WHPX direct boot directory"))?;
        let key = vm.entry.key.as_deref().ok_or_else(|| CliError::Config {
            path: self.registry_path.display().to_string(),
            detail: "WHPX VM has no SSH key".into(),
        })?;
        for (source, name) in [("vmlinuz", "Image"), ("initrd", "initrd")] {
            let target = dest.join(name);
            if !target.is_file() {
                let command = CommandLine::new("scp", &["-q", "-P"])
                    .arg(endpoint.port.to_string())
                    .arg("-o")
                    .arg("BatchMode=yes")
                    .arg("-o")
                    .arg("StrictHostKeyChecking=accept-new")
                    .arg("-o")
                    .arg(format!(
                        "UserKnownHostsFile={}",
                        remote::known_hosts_path(key)
                    ))
                    .arg("-i")
                    .arg(key)
                    .arg(format!(
                        "{}@{}:/boot{base}{source}",
                        endpoint.user, endpoint.host
                    ))
                    .arg(target.display().to_string());
                run_all(&mut self.runner, &[command])?;
            }
            let size = std::fs::metadata(&target)
                .map_err(CliError::io("inspect WHPX direct boot file"))?
                .len();
            if size < 1_000_000 {
                return Err(CliError::Config {
                    path: target.display().to_string(),
                    detail: "copied boot file is unexpectedly small".into(),
                });
            }
        }
        let mut updated = vm.entry.clone();
        let prior = updated
            .direct_kernel
            .as_ref()
            .ok_or_else(|| CliError::Config {
                path: self.registry_path.display().to_string(),
                detail: "WHPX VM has no direct boot bundle".into(),
            })?;
        updated.direct_kernel = Some(DirectKernel {
            kernel: dest.join("Image").display().to_string(),
            initrd: dest.join("initrd").display().to_string(),
            cmdline: options.to_owned(),
            loader_partuuid: prior.loader_partuuid.clone(),
            kver: prior.kver.clone(),
        });
        let mut registry = self.registry()?;
        registry.vms.insert(vm.name.clone(), updated.clone());
        registry.save(&self.registry_path)?;
        Ok(updated)
    }

    /// Reboot, wait for a new boot id over SSH, then for tilo-vmd, and check
    /// that the booted image has `digest`.
    #[expect(
        clippy::too_many_lines,
        reason = "one checked reboot and image verification"
    )]
    fn reboot_into(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        old_boot: &str,
        digest: &str,
    ) -> Result<imageup::Deployment, CliError> {
        let boot_entry = if vm.entry.hypervisor == "whpx" && vm.entry.direct_kernel.is_some() {
            Some(self.prepare_whpx_direct_boot(vm, endpoint, digest)?)
        } else {
            None
        };
        if !self.json {
            eprintln!(
                "tilo-vm: rebooting VM {} into {}",
                vm.name,
                imageup::short_digest(digest)
            );
        }
        // The connection drops as the VM goes down; its exit status says nothing.
        let reboot = format!("{}; systemctl reboot", imageup::ESP_WRITABLE_UNTIL_BOOT);
        let _ = self.vm_sh(vm, endpoint, &reboot);
        let started = Instant::now();
        let mut up = false;
        let adapter = Self::adapter(vm)?;
        let mut whpx_restarted = false;
        while started.elapsed() < Duration::from_secs(600) {
            // WHPX QEMU uses -no-reboot because its in-process reset is not
            // reliable. A guest reboot exits QEMU; launch the same VM again
            // once its old process has released the disk and ports.
            if vm.entry.hypervisor == "whpx"
                && !whpx_restarted
                && adapter.running(&vm.vm(), &mut self.runner) == Some(false)
            {
                let restart_vm = Vm {
                    name: &vm.name,
                    entry: boot_entry.as_ref().unwrap_or(&vm.entry),
                };
                run_all(&mut self.runner, &adapter.start(&restart_vm))?;
                whpx_restarted = true;
            }
            std::thread::sleep(if vm.entry.hypervisor == "whpx" {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(2)
            });
            if let Ok(output) = self.vm_sh(vm, endpoint, "cat /proc/sys/kernel/random/boot_id")
                && output.success
                && !output.stdout.trim().is_empty()
                && output.stdout.trim() != old_boot
            {
                up = true;
                break;
            }
        }
        self.receipt
            .record("reboot", started.elapsed().as_secs_f64());
        if !up {
            return Err(CliError::Unreachable {
                vm: vm.name.clone(),
                detail: "no SSH answer from a new boot within 600s of the reboot; if it is still booting, tilo-vm vm status tells when it answers".into(),
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            });
        }
        let started = Instant::now();
        let mut daemon = None;
        let mut missing = false;
        while started.elapsed() < Duration::from_secs(60) {
            match self.connect(vm) {
                Ok(remote) => {
                    daemon = Some(remote.hello.daemon_version.clone());
                    break;
                }
                Err(CliError::DaemonMissing { .. }) => {
                    missing = true;
                    break;
                }
                Err(error @ CliError::ProtocolMismatch { .. }) => {
                    self.warnings.push(format!("{error}; {}", error.hint()));
                    daemon = Some(String::from("incompatible"));
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_secs(1)),
            }
        }
        self.receipt
            .record("ready", started.elapsed().as_secs_f64());
        if missing {
            self.warnings.push(format!(
                "the new image has no tilo-vmd, so VM {} cannot host slots; tilo-vm vm rollback-image",
                vm.name
            ));
        } else if daemon.is_none() {
            self.warnings
                .push("tilo-vmd did not answer within 60s of SSH; tilo-vm vm status".into());
        }
        self.receipt.set("daemon", &daemon);
        let status = self.bootc_status(vm, endpoint)?;
        let booted = status.booted.ok_or_else(|| CliError::CommandFailed {
            command: "bootc status --json".into(),
            status: "no booted deployment".into(),
            stderr: String::new(),
        })?;
        if booted.digest != digest {
            return Err(CliError::CommandFailed {
                command: "bootc status --json".into(),
                status: "booted another image".into(),
                stderr: format!(
                    "the VM booted {} instead of {digest}: the boot entry fell back; tilo-vm vm serial shows why",
                    booted.describe()
                ),
            });
        }
        Ok(booted)
    }

    fn vm_rollback_image(
        &mut self,
        name: Option<&str>,
        no_reboot: bool,
    ) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let adapter = Self::adapter(&vm)?;
        let endpoint = adapter.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let facts = self.vm_facts(&vm, &endpoint, "vm rollback-image")?;
        let Some(target) = facts.bootc.rollback.clone() else {
            return Err(CliError::Unsupported {
                what: format!("rolling back VM {}: it has no previous image", vm.name),
                alternative: "bootc keeps one previous image, the one before the last upgrade, and there is none yet; tilo-vm vm upgrade makes one".into(),
            });
        };
        let words = ["bootc".to_owned(), "rollback".to_owned()];
        let output = self.vm_locked(&vm, &endpoint, &words)?;
        if !output.success {
            return Err(CliError::CommandFailed {
                command: words.join(" "),
                status: output.status,
                stderr: output.stderr.trim().to_owned(),
            });
        }
        let current = facts
            .bootc
            .booted
            .as_ref()
            .map_or_else(|| "(unknown)".into(), imageup::Deployment::describe);
        if no_reboot {
            return Ok(Outcome::new(
                format!(
                    "VM {} boots {} at its next restart (now running {current})",
                    vm.name,
                    target.describe()
                ),
                json!({ "vm": vm.name, "next": target.digest }),
            )
            .next(&["tilo-vm vm stop", "tilo-vm vm start"]));
        }
        let booted = self.reboot_into(&vm, &endpoint, &facts.boot_id, &target.digest)?;
        Ok(Outcome::new(
            format!(
                "VM {} rolled back to {}\n  {current} is now the previous image: rollback-image again returns to it\n  {}",
                vm.name,
                booted.describe(),
                self.receipt.summary()
            ),
            json!({ "vm": vm.name, "digest": booted.digest, "previous": facts.bootc.booted.map(|b| b.digest) }),
        )
        .next(&["tilo-vm status"]))
    }

    // ---- slots ----------------------------------------------------------------

    fn slot(&mut self, command: SlotCommand) -> Result<Outcome, CliError> {
        // A typo in the set name needs no VM to be reported.
        if let SlotCommand::Apps(SlotAppsCommand::Install { set, .. }) = &command {
            app_set(set)?;
        }
        let vm = self.resolve_vm(None)?;
        match command {
            SlotCommand::Create {
                name,
                headless: _,
                headful,
                outputs,
                identity,
                ..
            } => {
                let name = self.slot_name(name)?;
                let kind = if headful {
                    SlotKind::Headful
                } else {
                    SlotKind::Headless
                };
                let owner = self
                    .worktree
                    .as_ref()
                    .map(Worktree::owner)
                    .unwrap_or_default();
                let mut remote = self.connect(&vm)?;
                let result = remote.call(SlotCreate {
                    name: name.clone(),
                    kind,
                    owner,
                    outputs,
                    identity,
                })?;
                if identity && result.identity.is_none() {
                    return Err(CliError::Unsupported {
                        what: "slot create --identity by this VM's tilo-vmd".into(),
                        alternative:
                            "tilo-vm vm upgrade --from local, or tilo-vm swap vmd --slot vm".into(),
                    });
                }
                let text = slot_created_text(&name, kind, &result);
                self.receipt.set("slot", &name);
                Ok(Outcome::new(text, json!(result)).next(&[
                    "tilo-vm swap <component>",
                    "tilo-vm shot",
                    "tilo-vm control actions list",
                ]))
            }
            SlotCommand::List => {
                let slots = self.connect(&vm)?.call(SlotList {})?;
                let text = if slots.is_empty() {
                    "no slots yet".to_owned()
                } else {
                    slots.iter().map(slot_line).collect::<Vec<_>>().join("\n")
                };
                let mut outcome = Outcome::new(text, json!(slots));
                if slots.is_empty() {
                    outcome = outcome.next(&["tilo-vm slot create --headless"]);
                }
                Ok(outcome)
            }
            SlotCommand::Show { name, .. } => {
                let name = self.slot_name(name)?;
                let shown = self.connect(&vm)?.call(SlotShow { name })?;
                Ok(Outcome::new(describe_show(&shown), json!(shown))
                    .next(&["tilo-vm slot exec -- sha256sum <path>"]))
            }
            SlotCommand::Exec {
                name,
                timeout,
                command,
                ..
            } => self.slot_exec(&vm, name, timeout, &command),
            SlotCommand::Apps(SlotAppsCommand::Install {
                name, set, timeout, ..
            }) => self.apps_install(&vm, name, &set, timeout),
            SlotCommand::Apps(SlotAppsCommand::List) => Ok(apps_list()),
            SlotCommand::Restart { name, .. } => {
                let name = self.slot_name(name)?;
                let mut remote = self.connect(&vm)?;
                let result = self
                    .receipt
                    .phase("apply", || remote.call(SlotRestart { name }))?;
                Ok(Outcome::new(describe_apply(&result), json!(result))
                    .next(&["tilo-vm logs", "tilo-vm shot"]))
            }
            SlotCommand::Rm { name, .. } => {
                let name = self.slot_name(name)?;
                let result = self.connect(&vm)?.call(SlotRm { name: name.clone() })?;
                let text = if result.removed {
                    format!(
                        "slot {name} removed: session stopped, user and generations deleted; its layers go at the next gc"
                    )
                } else {
                    format!("slot {name} does not exist; nothing to do")
                };
                Ok(Outcome::new(text, json!(result)).next(&["tilo-vm gc --dry-run"]))
            }
        }
    }

    /// `slot apps install`: the set's packages through `tilo-vmd
    /// install-packages` over SSH (output streams), then a session restart
    /// so the slot sees the VM's new `/usr`.
    fn apps_install(
        &mut self,
        vm: &Resolved,
        name: Option<String>,
        set: &str,
        timeout: u64,
    ) -> Result<Outcome, CliError> {
        let apps = app_set(set)?;
        let slot = self.slot_name(name)?;
        // Fail on a missing slot before downloading anything.
        self.connect(vm)?.call(SlotShow { name: slot.clone() })?;
        let packages: Vec<String> = apps.apps.iter().map(|a| a.package.clone()).collect();
        let endpoint = Self::adapter(vm)?.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let mut words = vmbuild::daemon_command(vm.entry.daemon.as_deref(), "install-packages");
        words.extend(["--timeout".to_owned(), timeout.to_string()]);
        words.extend(packages.iter().map(|p| vmbuild::sh_quote(p)));
        let line = remote::ssh_command(&endpoint, vm.entry.key.as_deref(), &words, false);
        if !self
            .receipt
            .phase("install", || self.runner.interactive(&line))?
        {
            return Err(CliError::PackagesFailed {
                set: set.to_owned(),
                vm: vm.name.clone(),
            });
        }
        let mut remote = self.connect(vm)?;
        let restarted = self
            .receipt
            .phase("apply", || remote.call(SlotRestart { name: slot.clone() }))?;
        self.receipt.set("slot", &slot);
        let mut text = vec![format!(
            "installed the {set} apps into VM {} (a transient /usr overlay: gone at the next VM reboot) and restarted slot {slot}'s session",
            vm.name
        )];
        text.extend(
            apps.apps
                .iter()
                .map(|a| format!("  {}: {}  (try: {})", a.package, a.checks, a.try_command)),
        );
        text.push(
            "Start one from the launcher (system apps are listed with Tilo's), or open xterm and type the try commands into it."
                .into(),
        );
        Ok(Outcome::new(
            text.join("\n"),
            json!({ "vm": vm.name, "slot": slot, "set": set, "packages": packages, "restart": restarted }),
        )
        .next(&[
            "tilo-vm input key super",
            "tilo-vm input type xterm",
            "tilo-vm input key Return",
            "tilo-vm shot",
        ]))
    }

    /// `slot exec`: `tilo-vmd exec` over SSH, output streaming straight
    /// through; its exit status is the command's.
    fn slot_exec(
        &mut self,
        vm: &Resolved,
        name: Option<String>,
        timeout: u64,
        command: &[String],
    ) -> Result<Outcome, CliError> {
        let slot = self.slot_name(name)?;
        let endpoint = Self::adapter(vm)?.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let line = remote::ssh_command(
            &endpoint,
            vm.entry.key.as_deref(),
            &exec_words(vm.entry.daemon.as_deref(), &slot, timeout, command),
            false,
        );
        let ok = self.runner.interactive(&line)?;
        let mut outcome = Outcome::new(
            "",
            json!({ "vm": vm.name, "slot": slot, "command": command, "ok": ok }),
        );
        outcome.failed = !ok;
        Ok(outcome)
    }

    /// Does the VM have bootc (so vm upgrade can move it)? `None` when
    /// SSH does not say.
    fn vm_has_bootc(&mut self, vm: &Resolved) -> Option<bool> {
        let endpoint = Self::adapter(vm)
            .ok()?
            .ssh_endpoint(&vm.vm(), &mut self.runner)
            .ok()?;
        self.probe_bootc(vm, &endpoint)
    }

    /// Say where a build runs before it starts, when it is not the VM.
    fn announce_builder(&mut self, vm: &Resolved, note: Option<String>) {
        let Some(note) = note else { return };
        let fix = if self.vm_has_bootc(vm) == Some(true) {
            "tilo-vm vm upgrade gets an image that builds in the VM"
        } else {
            "a VM created from a current dev image builds in the VM (tilo-vm vm create <name>)"
        };
        let note = format!("{note}; {fix}");
        if self.json {
            self.warnings.push(note);
        } else {
            eprintln!("warning: {note}");
        }
    }

    /// `choose_builder`'s refusal, with the fix that fits this VM.
    fn cannot_build(&mut self, vm: &Resolved, error: CliError) -> CliError {
        match error {
            CliError::VmCannotBuild { missing, .. } => CliError::VmCannotBuild {
                missing,
                bootc: self.vm_has_bootc(vm),
            },
            other => other,
        }
    }

    // ---- swap -----------------------------------------------------------------

    fn swap(
        &mut self,
        slot: Option<String>,
        component: String,
        from: &str,
        builder: Option<Builder>,
        profile: Option<&str>,
        kernel: KernelFlags,
    ) -> Result<Outcome, CliError> {
        // Boot components first: the kernel swaps VM-wide (its slot is `vm`
        // unless named), the others go to their scenario.
        if !component.starts_with("app:")
            && let Some(entry) = self
                .manifest()?
                .as_ref()
                .and_then(|m| m.get(&component))
                .filter(|c| c.boot)
                .cloned()
        {
            let other_flags = builder.is_some() || profile.is_some();
            return self.swap_boot(slot.as_deref(), &entry, from, other_flags, kernel);
        }
        reject_kernel_flags(&component, &kernel)?;
        let slot = self.slot_name(slot)?;
        self.receipt.set("slot", &slot);
        self.receipt.set("component", &component);
        if let Some(profile) = profile {
            check_profile(profile, &component, from)?;
            self.receipt.set("profile", profile);
        }
        if let Some(app) = component.strip_prefix("app:") {
            return self.swap_app(&slot, app, from, builder);
        }
        let manifest = self.manifest()?;
        let entry = manifest.as_ref().and_then(|m| m.get(&component)).cloned();
        if let Some(c) = &entry {
            let fits = if slot == VM_SLOT {
                c.info.scope == Scope::Vm
            } else {
                c.info.scope != Scope::Vm
            };
            if !fits {
                return Err(CliError::ScopeMismatch {
                    component,
                    slot,
                    scope: c.info.scope.as_str().into(),
                });
            }
        }
        let source = ci::parse(from, self.worktree.as_ref())?;
        if matches!(source, ci::Source::Worktree)
            && let Some(explicit @ Builder::Host) = builder
        {
            // Fail before connecting: no host build is possible off Linux.
            choose_builder(
                Some(explicit),
                &VmTools::default(),
                cfg!(target_os = "linux"),
                std::env::consts::OS,
            )?;
        }
        let vm = self.resolve_vm(None)?;
        let mut remote = self.connect(&vm)?;
        self.check_vm_space(&mut remote, &vm)?;
        let layer = if let ci::Source::Layer(hash) = source {
            hash
        } else {
            let manifest_path = self.base_dir().join("os/components.toml");
            let manifest = manifest.ok_or_else(|| CliError::NoManifest {
                path: manifest_path.display().to_string(),
            })?;
            let entry = entry.as_ref().ok_or_else(|| CliError::UnknownComponent {
                name: component.clone(),
                known: manifest.components.keys().cloned().collect(),
            })?;
            if let ci::Source::Ci { tag, commit } = source {
                self.ci_layer(&mut remote, &vm, entry, &tag, commit)?
            } else {
                worktree_buildable(entry)?;
                let entry = entry.clone().with_profile(profile);
                self.build_worktree_layer(&mut remote, &vm, &entry, builder)?
            }
        };
        self.receipt.set("layer", &layer);
        let info = entry.map(|c| c.info);
        let result = self.receipt.phase("apply", || {
            remote.call(Swap {
                slot: slot.clone(),
                component: component.clone(),
                layer: layer.clone(),
                info,
            })
        })?;
        if result.changed && result.policy == Policy::SessionRestart {
            self.receipt
                .phase("session-ready", || wait_for_session(&mut remote, &slot));
        }
        self.warnings.extend(result.warnings.iter().cloned());
        self.warnings.extend(self.receipt.slow());
        let text = format!(
            "{} ({component} {})",
            describe_apply(&result),
            short(&layer)
        );
        Ok(Outcome::new(text, json!(result)).next(&[
            "tilo-vm shot",
            "tilo-vm control state-dump --pretty",
            "tilo-vm logs",
            "tilo-vm rollback",
        ]))
    }

    /// A boot component: the kernel swaps, the rest are refused with their
    /// scenario driver.
    fn swap_boot(
        &mut self,
        slot: Option<&str>,
        entry: &Component,
        from: &str,
        other_flags: bool,
        kernel: KernelFlags,
    ) -> Result<Outcome, CliError> {
        let name = entry.name.clone();
        if entry.scenario_only() {
            return Err(CliError::BootComponent {
                name,
                driver: scenario_driver(entry),
            });
        }
        if other_flags {
            return Err(CliError::Unsupported {
                what: format!("--builder and --profile for '{name}'"),
                alternative: format!(
                    "the {name} builds with its own script on this host: tilo-vm swap {name} [--jobs N] [--direct]"
                ),
            });
        }
        self.swap_kernel(slot, entry, from, kernel)
    }

    /// `--from worktree` with the chosen builder.
    fn build_worktree_layer(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: Option<Builder>,
    ) -> Result<String, CliError> {
        let (chosen, note) = choose_builder(
            builder,
            &remote.hello.tools,
            cfg!(target_os = "linux"),
            std::env::consts::OS,
        )
        .map_err(|error| self.cannot_build(vm, error))?;
        self.announce_builder(vm, note);
        self.receipt
            .set("builder", if chosen == Builder::Vm { "vm" } else { "host" });
        match chosen {
            Builder::Vm => self.build_layer_vm(vm, component),
            Builder::Host => self.build_layer(remote, component),
        }
    }

    /// Refuse to add layers to a VM below its free-space floor.
    fn check_vm_space(&self, remote: &mut Remote, vm: &Resolved) -> Result<(), CliError> {
        let status = remote.call(Status { slot: None })?;
        let floor = self.registry()?.min_free_bytes();
        match status.storage.free_bytes {
            Some(free) if free < floor => Err(CliError::LowDisk {
                place: format!("VM {}", vm.name),
                free: human_bytes(free),
                floor: human_bytes(floor),
            }),
            _ => Ok(()),
        }
    }

    /// rsync this worktree into the VM; returns its id there.
    fn sync_worktree(&mut self, vm: &Resolved) -> Result<(Worktree, String), CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        if !on_path("rsync") {
            return Err(CliError::ToolMissing {
                tool: "rsync".into(),
                purpose: "sync this worktree into the VM for an in-VM build".into(),
            });
        }
        let endpoint = Self::adapter(vm)?.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let id = vmbuild::worktree_id(&worktree, crate::context::hostname().as_deref());
        let rsync =
            vmbuild::rsync_command(&worktree.root, &endpoint, vm.entry.key.as_deref(), &id)?;
        eprintln!(
            "tilo-vm: syncing {} to {}:{}/{id}",
            worktree.root.display(),
            vm.name,
            tilo_vm_protocol::SRC_DIR
        );
        let runner = &mut self.runner;
        let synced = self.receipt.phase("sync", || runner.interactive(&rsync))?;
        if !synced {
            return Err(CliError::CommandFailed {
                command: rsync.display(),
                status: "sync failed".into(),
                stderr:
                    "rsync's output above names the failed SSH transport, remote command or lock"
                        .into(),
            });
        }
        self.receipt.set("worktree_id", &id);
        Ok((worktree, id))
    }

    /// Run a streaming job in the VM and record its phases.
    fn vm_job(
        &mut self,
        vm: &Resolved,
        subcommand: &str,
        job: &impl serde::Serialize,
    ) -> Result<JobOutput, CliError> {
        let endpoint = Self::adapter(vm)?.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let command = vmbuild::daemon_command(vm.entry.daemon.as_deref(), subcommand);
        let output = vmbuild::run_job(&vm.name, &endpoint, vm.entry.key.as_deref(), &command, job)?;
        let (JobOutput::Layer { phases, .. } | JobOutput::App { phases, .. }) = &output;
        for (name, seconds) in phases {
            self.receipt.record(name, *seconds);
        }
        if let JobOutput::Layer { cache, .. } = &output {
            self.receipt.set("cache", cache);
        }
        Ok(output)
    }

    /// `--builder vm`: sync, build in the VM, import there; no bytes return.
    fn build_layer_vm(&mut self, vm: &Resolved, component: &Component) -> Result<String, CliError> {
        let (worktree, id) = self.sync_worktree(vm)?;
        let job = BuildJob {
            worktree: id,
            target: BuildTarget::Component {
                name: component.name.clone(),
                profile: Some(component.profile.clone()),
            },
            provenance: build::source_provenance(&worktree, &component.name, "vm"),
            owner: worktree.owner(),
            color: std::io::IsTerminal::is_terminal(&std::io::stderr()),
        };
        match self.vm_job(vm, "build", &job)? {
            JobOutput::Layer { hash, .. } => Ok(hash),
            JobOutput::App { .. } => Err(CliError::CommandFailed {
                command: "tilo-vmd build".into(),
                status: "unexpected result".into(),
                stderr: "an app install instead of a layer".into(),
            }),
        }
    }

    /// `--from sha:` / `--from ci:`: the component's manifest paths out of
    /// the published CI image, as a layer with `authority = "ci"`.
    fn ci_layer(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        tag: &str,
        commit: Option<String>,
    ) -> Result<String, CliError> {
        let image = ci::image(tag);
        self.receipt.set("image", &image);
        if component.paths.is_empty() {
            return Err(CliError::Unsupported {
                what: format!("--from ci for '{}', which lists no files", component.name),
                alternative: "build it: --from worktree".into(),
            });
        }
        let mut provenance = tilo_vm_protocol::Provenance::local(&component.name);
        provenance.source_commit = commit;
        provenance.cargo_profile = Some(component.profile.clone());
        provenance.features.clone_from(&component.features);
        let job = CiJob {
            image: image.clone(),
            component: component.name.clone(),
            paths: component.paths.clone(),
            provenance,
        };
        if remote.hello.tools.podman {
            eprintln!("tilo-vm: pulling {image} in VM {}", vm.name);
            return match self.vm_job(vm, "ci-layer", &job) {
                Ok(JobOutput::Layer { hash, .. }) => Ok(hash),
                Ok(JobOutput::App { .. }) => Err(CliError::CommandFailed {
                    command: "tilo-vmd ci-layer".into(),
                    status: "unexpected result".into(),
                    stderr: "an app install instead of a layer".into(),
                }),
                Err(error) => Err(error),
            };
        }
        // No podman in the VM: pull here, upload the layer.
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let staging = build::Staging::new(&worktree)?;
        let arch = remote.hello.arch.clone();
        eprintln!("tilo-vm: pulling {image} on this host (the VM has no podman)");
        let runner = &mut self.runner;
        let digest = self.receipt.phase("pull", || {
            ci::extract_on_host(runner, &image, &arch, &job.paths, &staging.tree())
        })?;
        build::pack(&staging.tree(), &staging.archive())?;
        let mut provenance = job.provenance;
        provenance.authority = "ci".into();
        provenance.builder = Some("ci".into());
        provenance.base_image_digest = digest;
        provenance.target = Some(format!("{arch}-unknown-linux-gnu"));
        let archive = staging.archive();
        let upload = self
            .receipt
            .phase("transfer", || -> Result<String, CliError> {
                let file = std::fs::File::open(&archive)
                    .map_err(CliError::io(format!("open {}", archive.display())))?;
                remote.upload(std::io::BufReader::new(file))
            })?;
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source: LayerSource::Upload(upload),
                provenance: Box::new(provenance),
            })
        })?;
        Ok(imported.hash)
    }

    /// `swap app:<dir>`: package the app as a self-signed `.tapp` and
    /// install it into the slot user's store. Not a generation.
    fn swap_app(
        &mut self,
        slot: &str,
        dir: &str,
        from: &str,
        builder: Option<Builder>,
    ) -> Result<Outcome, CliError> {
        if from != "worktree" {
            return Err(CliError::Unsupported {
                what: format!("--from {from} for an app"),
                alternative: format!("apps build from this worktree: tilo-vm swap app:{dir}"),
            });
        }
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let known = build::apps_in(&worktree.root);
        if !known.iter().any(|a| a == dir) {
            return Err(CliError::UnknownApp {
                name: dir.to_owned(),
                known,
            });
        }
        let vm = self.resolve_vm(None)?;
        let mut remote = self.connect(&vm)?;
        let (chosen, note) = choose_builder(
            builder,
            &remote.hello.tools,
            cfg!(target_os = "linux"),
            std::env::consts::OS,
        )
        .map_err(|error| self.cannot_build(&vm, error))?;
        self.announce_builder(&vm, note);
        let result: AppInstallResult = match chosen {
            Builder::Vm => {
                let (worktree, id) = self.sync_worktree(&vm)?;
                let job = BuildJob {
                    worktree: id,
                    target: BuildTarget::App {
                        dir: dir.to_owned(),
                        slot: slot.to_owned(),
                    },
                    provenance: build::source_provenance(&worktree, &format!("app:{dir}"), "vm"),
                    owner: worktree.owner(),
                    color: std::io::IsTerminal::is_terminal(&std::io::stderr()),
                };
                match self.vm_job(&vm, "build", &job)? {
                    JobOutput::App { result, .. } => result,
                    JobOutput::Layer { .. } => {
                        return Err(CliError::CommandFailed {
                            command: "tilo-vmd build".into(),
                            status: "unexpected result".into(),
                            stderr: "a layer instead of an app install".into(),
                        });
                    }
                }
            }
            Builder::Host => self.install_app_from_host(&mut remote, &worktree, slot, dir)?,
        };
        let what = match (&result.id, &result.version) {
            (Some(id), Some(version)) => format!("{id} {version}"),
            (Some(id), None) => id.clone(),
            _ => format!("apps/{dir}"),
        };
        let text = format!(
            "installed {what} (self-signed) into slot {slot}'s app store; nothing restarted: it is live the next time the app launches\n  app installs are not generations: tilo-vm rollback does not undo them"
        );
        Ok(Outcome::new(text, json!(result)).next(&[
            "tilo-vm control actions list",
            "tilo-vm shot",
            "tilo-vm logs",
        ]))
    }

    fn install_app_from_host(
        &mut self,
        remote: &mut Remote,
        worktree: &Worktree,
        slot: &str,
        dir: &str,
    ) -> Result<AppInstallResult, CliError> {
        self.runner.cwd = Some(worktree.root.clone());
        let target = build::target_dir(&self.cargo_metadata());
        let staging = build::Staging::new(worktree)?;
        let tapp = staging.tree().with_extension("tapp");
        let [tools, package] = build::app_package(&target, dir, &tapp);
        let runner = &mut self.runner;
        let built = self.receipt.phase("build", || -> Result<(), CliError> {
            for command in [&tools, &package] {
                if !runner.interactive(command)? {
                    return Err(CliError::CommandFailed {
                        command: command.display(),
                        status: "failed".into(),
                        stderr: String::new(),
                    });
                }
            }
            Ok(())
        });
        built?;
        let upload = self
            .receipt
            .phase("transfer", || -> Result<String, CliError> {
                let file = std::fs::File::open(&tapp)
                    .map_err(CliError::io(format!("open {}", tapp.display())))?;
                remote.upload(std::io::BufReader::new(file))
            })?;
        self.receipt.phase("install", || {
            remote.call(AppInstall {
                slot: slot.to_owned(),
                source: LayerSource::Upload(upload),
            })
        })
    }

    fn cargo_metadata(&mut self) -> Value {
        self.runner
            .output(&CommandLine::new(
                "cargo",
                &["metadata", "--no-deps", "--format-version", "1"],
            ))
            .ok()
            .and_then(|o| serde_json::from_str(&o.stdout).ok())
            .unwrap_or(Value::Null)
    }

    /// `--builder host`: cargo on this Linux host, layer uploaded.
    fn build_layer(
        &mut self,
        remote: &mut Remote,
        component: &Component,
    ) -> Result<String, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self.cargo_metadata();
        let target = build::target_dir(&metadata);
        let extra = build::dev_features(&metadata, &component.packages);
        let cargo = build::cargo_build(component, &extra);
        let before = build::sccache_stats(&mut self.runner);
        let runner = &mut self.runner;
        let built = self.receipt.phase("build", || runner.interactive(&cargo))?;
        if !built {
            return Err(CliError::CommandFailed {
                command: cargo.display(),
                status: "build failed".into(),
                stderr: String::new(),
            });
        }
        let after = build::sccache_stats(&mut self.runner);
        self.receipt
            .set("cache", build::cache_summary(before, after));
        let staging = build::Staging::new(&worktree)?;
        let install = build::install(component, &target, &staging.tree());
        let runner = &mut self.runner;
        let assembled = self.receipt.phase("assemble", || -> Result<(), CliError> {
            if !runner.interactive(&install)? {
                return Err(CliError::CommandFailed {
                    command: install.display(),
                    status: "failed".into(),
                    stderr: String::new(),
                });
            }
            build::pack(&staging.tree(), &staging.archive())
        });
        assembled?;
        let features = component.build_features(&extra);
        let provenance = build::provenance(&mut self.runner, &worktree, component, features);
        let archive = staging.archive();
        let upload = self
            .receipt
            .phase("transfer", || -> Result<String, CliError> {
                let file = std::fs::File::open(&archive)
                    .map_err(CliError::io(format!("open {}", archive.display())))?;
                remote.upload(std::io::BufReader::new(file))
            })?;
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source: LayerSource::Upload(upload),
                provenance: Box::new(provenance),
            })
        })?;
        Ok(imported.hash)
    }

    // ---- driving --------------------------------------------------------------

    fn seat(&mut self, command: SeatCommand) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(None)?;
        match command {
            SeatCommand::Claim { slot, steal, .. } => {
                let slot = self.slot_name(slot)?;
                let owner = self
                    .worktree
                    .as_ref()
                    .map(Worktree::owner)
                    .unwrap_or_default();
                let result = self.connect(&vm)?.call(SeatClaim {
                    slot: slot.clone(),
                    owner,
                    steal,
                })?;
                let text = if result.changed {
                    format!(
                        "slot {slot} now runs on seat0 (the hypervisor window); greetd is stopped; session restarted"
                    )
                } else {
                    format!("slot {slot} already holds seat0; nothing to do")
                };
                Ok(Outcome::new(text, json!(result))
                    .next(&["tilo-vm vm shot", "tilo-vm seat release"]))
            }
            SeatCommand::Release { slot, .. } => {
                let slot = self.slot_name(slot)?;
                let result = self
                    .connect(&vm)?
                    .call(SeatRelease { slot: slot.clone() })?;
                let text = if result.changed {
                    format!(
                        "seat0 returned to greetd; slot {slot} restarted on {}",
                        result.seat
                    )
                } else {
                    format!("slot {slot} does not hold seat0; nothing to do")
                };
                Ok(Outcome::new(text, json!(result)))
            }
        }
    }

    fn shot(&mut self, slot: Option<String>, output: &Path) -> Result<Outcome, CliError> {
        let slot = self.slot_name(slot)?;
        let vm = self.resolve_vm(None)?;
        let result = self.connect(&vm)?.call(Shot { slot: slot.clone() })?;
        let png = STANDARD
            .decode(result.png)
            .map_err(|e| CliError::CommandFailed {
                command: "shot".into(),
                status: "bad image data".into(),
                stderr: e.to_string(),
            })?;
        let output = absolute(output);
        ensure_parent(&output)?;
        std::fs::write(&output, &png)
            .map_err(CliError::io(format!("write {}", output.display())))?;
        Ok(Outcome::new(
            format!(
                "saved {} ({} bytes) from slot {slot}",
                output.display(),
                png.len()
            ),
            json!({ "slot": slot, "path": output, "bytes": png.len() }),
        ))
    }

    fn control(&mut self, slot: Option<String>, args: &[String]) -> Result<Outcome, CliError> {
        let slot = self.slot_name(slot)?;
        let vm = self.resolve_vm(None)?;
        let result = self.connect(&vm)?.call(Control {
            slot,
            args: args.to_vec(),
        })?;
        if result.exit_code != 0 {
            return Err(CliError::CommandFailed {
                command: format!("tilo-control {}", args.join(" ")),
                status: format!("exit {}", result.exit_code),
                stderr: result.stderr.trim().to_owned(),
            });
        }
        if !result.stderr.is_empty() && !self.json {
            eprint!("{}", result.stderr);
        }
        Ok(Outcome::new(result.stdout.trim_end(), json!(result)))
    }

    fn flight(&mut self, command: FlightCommand) -> Result<Outcome, CliError> {
        match command {
            FlightCommand::App {
                desktop_id,
                app_id,
                slot,
                type_text,
                maximum_launch_ms,
                maximum_control_ms,
                timeout_seconds,
                ..
            } => self.app_flight(
                slot,
                &desktop_id,
                &app_id,
                &type_text,
                FlightBudget {
                    maximum_launch: Duration::from_millis(maximum_launch_ms),
                    maximum_control: Duration::from_millis(maximum_control_ms),
                    timeout: Duration::from_secs(timeout_seconds),
                },
            ),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one linear acceptance flight keeps every measured phase and failure invariant visible"
    )]
    fn app_flight(
        &mut self,
        slot: Option<String>,
        desktop_id: &str,
        app_id: &str,
        type_text: &str,
        budget: FlightBudget,
    ) -> Result<Outcome, CliError> {
        let FlightBudget {
            maximum_launch,
            maximum_control,
            timeout,
        } = budget;
        let slot = self.slot_name(slot)?;
        let vm = self.resolve_vm(None)?;
        let mut remote = self.connect(&vm)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| flight_failure("flight timeout is too large"))?;
        let mut worst_control = Duration::ZERO;
        let baseline = control_json(
            &mut remote,
            &slot,
            foreign_frames_args(),
            &mut worst_control,
        )?;
        let baseline_ids = foreign_surface_ids(&baseline);

        let launched_at = Instant::now();
        control_json(
            &mut remote,
            &slot,
            &[
                "actions".into(),
                "invoke".into(),
                "host-applications".into(),
                desktop_id.into(),
            ],
            &mut worst_control,
        )?;

        let (surface_id, initial_frame_seq) = loop {
            ensure_before(deadline, "app did not present before the flight deadline")?;
            let value = control_json(
                &mut remote,
                &slot,
                foreign_frames_args(),
                &mut worst_control,
            )?;
            if let Some(frame) = matching_presented_frame(&value, app_id, &baseline_ids) {
                break (frame.surface_id, frame.frame_seq);
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let launch_time = launched_at.elapsed();
        if launch_time > maximum_launch {
            return Err(flight_failure(format!(
                "{desktop_id} presented after {:.1} ms, over the {:.1} ms launch ceiling",
                launch_time.as_secs_f64() * 1000.0,
                maximum_launch.as_secs_f64() * 1000.0
            )));
        }

        let value = control_json(
            &mut remote,
            &slot,
            foreign_frames_args(),
            &mut worst_control,
        )?;
        let rect = matching_surface(&value, surface_id)
            .and_then(|frame| frame.rect)
            .ok_or_else(|| flight_failure(format!("surface {surface_id} has no focusable rect")))?;
        control_json(
            &mut remote,
            &slot,
            &[
                "click-at".into(),
                rect.center_x().to_string(),
                rect.center_y().to_string(),
            ],
            &mut worst_control,
        )?;
        control_json(
            &mut remote,
            &slot,
            &["type".into(), type_text.into()],
            &mut worst_control,
        )?;

        let final_frame_seq = loop {
            ensure_before(
                deadline,
                "focused app did not present a frame after injected input",
            )?;
            let value = control_json(
                &mut remote,
                &slot,
                foreign_frames_args(),
                &mut worst_control,
            )?;
            if let Some(frame) = matching_surface(&value, surface_id)
                && frame.frame_seq > initial_frame_seq
            {
                break frame.frame_seq;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        if worst_control > maximum_control {
            return Err(flight_failure(format!(
                "worst compositor control round trip was {:.1} ms, over the {:.1} ms ceiling",
                worst_control.as_secs_f64() * 1000.0,
                maximum_control.as_secs_f64() * 1000.0
            )));
        }

        let data = json!({
            "slot": slot,
            "desktop_id": desktop_id,
            "app_id": app_id,
            "surface_id": surface_id,
            "initial_frame_seq": initial_frame_seq,
            "final_frame_seq": final_frame_seq,
            "request_to_presented_ms": launch_time.as_secs_f64() * 1000.0,
            "worst_control_ms": worst_control.as_secs_f64() * 1000.0,
            "typed": type_text,
        });
        Ok(Outcome::new(
            format!(
                "PASS {desktop_id}: surface {surface_id} presented in {:.1} ms, accepted input (frame {initial_frame_seq} -> {final_frame_seq}); worst control round trip {:.1} ms",
                launch_time.as_secs_f64() * 1000.0,
                worst_control.as_secs_f64() * 1000.0
            ),
            data,
        )
        .next(&["tilo-vm shot", "tilo-vm logs"]))
    }

    fn input(&mut self, slot: Option<String>, command: InputCommand) -> Result<Outcome, CliError> {
        let actions = input_actions(command)?;
        let slot = self.slot_name(slot)?;
        let vm = self.resolve_vm(None)?;
        let result = self.connect(&vm)?.call(Input {
            slot: slot.clone(),
            actions: actions.clone(),
        })?;
        let what = actions
            .iter()
            .map(describe_input)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(Outcome::new(
            format!(
                "{what} on slot {slot} (seat {}, {} input events through its uinput devices)",
                result.seat, result.events
            ),
            json!(result),
        )
        .next(&["tilo-vm shot", "tilo-vm control state-dump --pretty"]))
    }

    fn gc(
        &mut self,
        dry_run: bool,
        keep: Option<u32>,
        keep_targets: Option<u32>,
        min_age: Option<u64>,
    ) -> Result<Outcome, CliError> {
        let registry = self.registry()?;
        let keep = keep.unwrap_or_else(|| registry.keep_generations());
        let keep_targets = keep_targets.or(registry.keep_targets);
        let vm = self.resolve_vm(None)?;
        let result = self.connect(&vm)?.call(Gc {
            keep,
            dry_run,
            keep_targets,
            free_goal_bytes: Some(registry.min_free_bytes()),
            min_age_seconds: min_age,
        })?;
        let verb = if dry_run { "would remove" } else { "removed" };
        let mut text = format!(
            "{verb} {} layers ({}) and {} generations; keeping the current and newest {keep} per slot",
            result.removed_layers.len(),
            human_bytes(result.freed_bytes),
            result.removed_generations.len()
        );
        let _ = write!(
            text,
            "\n{verb} {} in-VM build dirs ({}), least recently used first{}",
            result.removed_targets.len(),
            human_bytes(result.freed_target_bytes),
            if result.removed_targets.is_empty() {
                String::new()
            } else {
                format!(": {}", result.removed_targets.join(", "))
            }
        );
        if let Some(kept) = describe_kept(&result) {
            text.push('\n');
            text.push_str(&kept);
        }
        let mut outcome = Outcome::new(text, json!(result));
        if dry_run {
            outcome = outcome.next(&["tilo-vm gc"]);
        }
        if !result.kept_young.is_empty() {
            outcome =
                outcome.next(&["tilo-vm gc --min-age 0   (only for layers you know are yours)"]);
        }
        Ok(outcome)
    }
}

/// How `stage_and_boot` stages: bootc's transport and reference, and the
/// manifest digest it must stage when that is known in advance.
struct Stage {
    transport: &'static str,
    imgref: String,
    expected: Option<String>,
    no_reboot: bool,
}

/// What `vm upgrade --from` moves the VM onto.
enum UpgradeSource {
    /// An image in local storage, built first when `build` (FROM `base`, or
    /// the default base).
    Local {
        image: String,
        build: bool,
        base: Option<String>,
    },
    /// A dev image in the registry, which the VM pulls itself.
    Published(String),
}

/// Where `vm upgrade --from ci:<tag>` found its image.
enum CiUpgrade {
    /// A dev image in the registry: the VM switches to it directly.
    Published(String),
    /// A production image, now in local storage: build the dev image FROM it.
    Base(String),
}

/// `podman image inspect` plus a look inside: the facts `check_image` judges.
fn local_image_facts(
    runner: &mut SystemRunner,
    image: &str,
) -> Result<imageup::ImageFacts, CliError> {
    let inspect = runner.output(&imageup::inspect_command(image))?;
    if !inspect.success {
        return Err(CliError::ImageRefused {
            image: image.to_owned(),
            problem: "is not in this host's podman storage".into(),
            fix: "build it: tilo-vm vm upgrade --from local   |   or see what is there: podman images"
                .into(),
        });
    }
    let (id, flavour) =
        imageup::parse_inspect(&inspect.stdout).map_err(|detail| CliError::CommandFailed {
            command: imageup::inspect_command(image).display(),
            status: "unreadable output".into(),
            stderr: detail,
        })?;
    let mut facts = imageup::ImageFacts {
        id,
        flavour,
        ..imageup::ImageFacts::default()
    };
    if facts.flavour.as_deref() == Some("dev") {
        let probe = imageup::probe_command(&facts.id);
        let output = runner.output(&probe)?;
        if !output.success {
            return Err(CliError::CommandFailed {
                command: probe.display(),
                status: output.status,
                stderr: output.stderr.trim().to_owned(),
            });
        }
        imageup::parse_probe(&output.stdout, &mut facts);
    }
    Ok(facts)
}

/// The Tilo checkout `dir` is in, found by its dev image build script (for a
/// copy of the tree without git, such as a synced copy on a build machine).
fn checkout_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| d.join(imageup::BUILD_SCRIPT).is_file())
        .map(Path::to_path_buf)
}

/// What `image build` runs: `--from local`'s build, the base rebuilt only
/// when its inputs changed, with or without the disk step.
fn image_build_command(script: &Path, image: &str, disk: bool, scratch: &Path) -> CommandLine {
    CommandLine::new(&script.display().to_string(), &[])
        .env("TILO_DEV_DISK", if disk { "1" } else { "0" })
        .env("TILO_DEV_IMAGE", image)
        .env("TILO_DEV_BASE_BUILD", "auto")
        .env("TILO_OUTPUT", scratch.display().to_string())
}

/// What `image disk` runs: the build's disk step for an image already in
/// podman's storage.
fn image_disk_command(script: &Path, image: &str, scratch: &Path) -> CommandLine {
    CommandLine::new(&script.display().to_string(), &[])
        .env("TILO_IMAGE", image)
        .env("TILO_SKIP_BUILD", "1")
        .env("TILO_OUTPUT", scratch.display().to_string())
        // The size os/dev/build-dev-image.sh and the published dev disks use.
        .env("TILO_DISK_SIZE", "64G")
        .env(
            "TILO_ROOT_PODMAN_STORE",
            root_podman_store(scratch).display().to_string(),
        )
}

/// Where the disk step's root podman keeps its copy of the image: in the
/// scratch directory, removed once the disk is written, not in
/// /var/lib/containers.
fn root_podman_store(scratch: &Path) -> PathBuf {
    scratch.join("root-podman")
}

/// The disk the build left in `scratch`, moved to `<out>/disk.qcow2`.
fn move_built_disk(scratch: &Path, out: &Path) -> Result<PathBuf, CliError> {
    let built = scratch.join("qcow2").join("disk.qcow2");
    let dest = out.join(imageup::BUILT_DISK);
    std::fs::rename(&built, &dest).map_err(CliError::io(format!(
        "move {} to {}",
        built.display(),
        dest.display()
    )))?;
    Ok(dest)
}

/// `image build`'s scratch directory: next to its output, never inside it, so
/// fetching the output does not fetch the build's temporary files.
fn build_scratch(out: &Path) -> PathBuf {
    let name = out
        .file_name()
        .map_or_else(|| "tilo-vm".into(), |n| n.to_string_lossy().into_owned());
    out.with_file_name(format!("{name}-build"))
}

/// [`imageup::layout_facts_text`] for a local image: its inspect output and a
/// look inside.
fn image_facts_text(runner: &mut SystemRunner, image: &str) -> Result<String, CliError> {
    let mut run = |command: CommandLine| -> Result<String, CliError> {
        let output = runner.output(&command)?;
        if output.success {
            Ok(output.stdout)
        } else {
            Err(CliError::CommandFailed {
                command: command.display(),
                status: output.status,
                stderr: output.stderr.trim().to_owned(),
            })
        }
    };
    let inspect = run(imageup::inspect_command(image))?;
    let probe = run(imageup::probe_command(image))?;
    Ok(imageup::layout_facts_text(&inspect, &probe))
}

/// Write `image` into the VM's host-side OCI layout and drop what it no
/// longer references; returns the manifest digest bootc will report.
fn export_layout(
    runner: &mut SystemRunner,
    image: &str,
    layout: &Path,
) -> Result<String, CliError> {
    if let Some(parent) = layout.parent() {
        std::fs::create_dir_all(parent)
            .map_err(CliError::io(format!("create {}", parent.display())))?;
    }
    let digest_file = layout.with_extension("digest");
    let export = imageup::export_command(image, layout, &digest_file);
    let output = runner.output(&export).map_err(|error| match error {
        CliError::ToolMissing { .. } => CliError::ToolMissing {
            tool: "skopeo".into(),
            purpose: "copy the image's layers out of podman's storage".into(),
        },
        other => other,
    })?;
    if !output.success {
        return Err(CliError::CommandFailed {
            command: export.display(),
            status: output.status,
            stderr: output.stderr.trim().to_owned(),
        });
    }
    let digest = std::fs::read_to_string(&digest_file)
        .map_err(CliError::io(format!("read {}", digest_file.display())))?
        .trim()
        .to_owned();
    imageup::prune_layout(layout).map_err(CliError::io(format!("prune {}", layout.display())))?;
    Ok(digest)
}

/// SSH answered but refused the VM's recorded key: name the one-time step
/// that installs it. Every other error passes through.
fn key_refusal(
    vm: &Resolved,
    adapter: &dyn Hypervisor,
    endpoint: &SshEndpoint,
    error: CliError,
) -> CliError {
    let (CliError::Unreachable { detail, .. }, Some(key)) = (&error, vm.entry.key.as_deref())
    else {
        return error;
    };
    if !detail.contains("Permission denied") {
        return error;
    }
    let install = sshkey::read_public(Path::new(key)).map_or_else(
        |_| format!("ssh-keygen -y -f {key} > {key}.pub, then rerun this command"),
        |public| sshkey::install_command(endpoint, &public),
    );
    CliError::RootKeyRefused {
        vm: vm.name.clone(),
        key: key.to_owned(),
        install,
        credentials: adapter.capabilities().credentials.is_some(),
    }
}

/// `swap --profile` names a cargo profile and builds a component from the worktree.
/// `swap` options that only the kernel takes.
#[derive(Debug, Clone, Default)]
pub struct KernelFlags {
    /// Boot the host build directly (libvirt) instead of installing it.
    pub direct: bool,
    /// Parallel compile jobs.
    pub jobs: Option<u32>,
    /// The direct boot's kernel command line.
    pub cmdline: Option<String>,
}

/// `--direct` and `--jobs` belong to the kernel's own build.
fn reject_kernel_flags(component: &str, kernel: &KernelFlags) -> Result<(), CliError> {
    if kernel.direct || kernel.jobs.is_some() {
        return Err(CliError::Unsupported {
            what: format!("--direct and --jobs for '{component}'"),
            alternative: "they apply to the kernel only: tilo-vm swap kernel --jobs 4, tilo-vm swap kernel --direct".into(),
        });
    }
    Ok(())
}

fn check_profile(profile: &str, component: &str, from: &str) -> Result<(), CliError> {
    if !tilo_vm_protocol::manifest::valid_profile(profile) {
        return Err(CliError::InvalidInput(format!(
            "'{profile}' is not a cargo profile name"
        )));
    }
    if from != "worktree" || component.starts_with("app:") {
        return Err(CliError::Unsupported {
            what: format!(
                "--profile with {}",
                if from == "worktree" { "an app" } else { from }
            ),
            alternative: "--profile applies to components built from this worktree: tilo-vm swap <component> --profile dev".into(),
        });
    }
    Ok(())
}

fn yes(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn entry_image(registry: &Registry, name: &str) -> Option<String> {
    registry.vms.get(name).and_then(|e| e.image.clone())
}

/// Where builds and CI extraction run for a VM with these tools.
pub fn describe_tools(tools: &VmTools) -> String {
    let build = if tools.can_build() {
        format!(
            "in the VM (sccache {})",
            if tools.sccache { "shared" } else { "missing" }
        )
    } else if cfg!(target_os = "linux") {
        format!(
            "on this host (the VM lacks {})",
            tools.missing_for_build().join(", ")
        )
    } else {
        format!(
            "unavailable (the VM lacks {}; tilo-vm doctor says how to fix it)",
            tools.missing_for_build().join(", ")
        )
    };
    let ci = if tools.podman {
        "CI layers pulled in the VM"
    } else {
        "CI layers pulled on this host"
    };
    format!("{build}; {ci}")
}

/// The step of `vm stop` that got the VM down (or the last one tried).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopStep {
    /// The hypervisor's ACPI shutdown request.
    Acpi,
    /// `systemctl poweroff` in the guest, over SSH.
    GuestPoweroff,
    /// The hypervisor's hard power-off.
    HardPowerOff,
}

impl StopStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Acpi => "acpi",
            Self::GuestPoweroff => "guest-poweroff",
            Self::HardPowerOff => "hard-power-off",
        }
    }
}

/// How long each [`StopStep`] may take before the next one.
#[derive(Debug, Clone, Copy)]
pub struct StopGraces {
    pub acpi: Duration,
    pub guest: Duration,
    pub hard: Duration,
    pub poll: Duration,
}

impl Default for StopGraces {
    fn default() -> Self {
        Self {
            acpi: Duration::from_secs(30),
            guest: Duration::from_secs(90),
            hard: Duration::from_secs(30),
            poll: Duration::from_millis(500),
        }
    }
}

/// Stop `vm`: ask over ACPI, then run `poweroff` (the guest's
/// `systemctl poweroff` over SSH) where there is one, then power it off
/// hard where the hypervisor can. Each step waits its grace for the VM to be
/// down; only a VM the hypervisor reports still running moves on. Returns
/// whether it is down (`None`: nothing can tell) and the last step taken.
///
/// # Errors
/// A hypervisor command could not run.
pub fn stop_escalating(
    runner: &mut dyn Runner,
    adapter: &dyn Hypervisor,
    vm: &Vm<'_>,
    poweroff: Option<&CommandLine>,
    mut lock: Option<&mut remote::VmLock>,
    graces: StopGraces,
) -> Result<(Option<bool>, StopStep), CliError> {
    run_all(runner, &adapter.stop(vm))?;
    let mut down = await_down(
        runner,
        adapter,
        vm,
        lock.as_deref_mut(),
        graces.acpi,
        graces.poll,
    );
    if down != Some(false) {
        return Ok((down, StopStep::Acpi));
    }
    let mut step = StopStep::Acpi;
    if let Some(poweroff) = poweroff {
        step = StopStep::GuestPoweroff;
        // ssh fails as the guest drops the connection; the wait below judges.
        let _ = runner.output(poweroff);
        down = await_down(runner, adapter, vm, lock, graces.guest, graces.poll);
        if down != Some(false) {
            return Ok((down, step));
        }
    }
    if let Some(off) = adapter.power_off(vm) {
        step = StopStep::HardPowerOff;
        run_all(runner, &[off])?;
        down = await_down(runner, adapter, vm, None, graces.hard, graces.poll);
    }
    Ok((down, step))
}

/// Wait up to `limit` for `vm` to be down: the VM lock's SSH session closing
/// and, where the hypervisor can say, the VM no longer running.
fn await_down(
    runner: &mut dyn Runner,
    adapter: &dyn Hypervisor,
    vm: &Vm<'_>,
    lock: Option<&mut remote::VmLock>,
    limit: Duration,
    poll: Duration,
) -> Option<bool> {
    let started = Instant::now();
    let mut down = lock.map(|lock| lock.wait_for_shutdown(limit));
    if down != Some(false) && adapter.running(vm, runner).is_some() {
        down = Some(loop {
            match adapter.running(vm, runner) {
                Some(true) if started.elapsed() < limit => std::thread::sleep(poll),
                Some(true) => break false,
                _ => break true,
            }
        });
    }
    down
}

/// Refuse `swap --from worktree` for a component only the image build
/// assembles: either builder would run a cargo build that cannot produce it.
///
/// # Errors
/// [`CliError::Unsupported`], naming the swaps that do work.
pub fn worktree_buildable(component: &Component) -> Result<(), CliError> {
    if !component.image_assembled {
        return Ok(());
    }
    Err(CliError::Unsupported {
        what: format!(
            "--from worktree for '{}', which the image build assembles and signs",
            component.name
        ),
        alternative: format!(
            "one app from this worktree: tilo-vm swap app:<dir>; the published set: \
             tilo-vm swap {} --from ci:<tag>; this worktree's image: tilo-vm vm upgrade --from local",
            component.name
        ),
    })
}

/// Which builder `swap --from worktree` uses.
///
/// # Errors
/// `--builder host` off Linux, and `--builder vm` (or no choice off Linux)
/// when the VM's image cannot build.
pub fn choose_builder(
    explicit: Option<Builder>,
    tools: &VmTools,
    linux_host: bool,
    os: &str,
) -> Result<(Builder, Option<String>), CliError> {
    let cannot = || CliError::VmCannotBuild {
        bootc: None,
        missing: tools
            .missing_for_build()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };
    match explicit {
        Some(Builder::Host) if !linux_host => {
            Err(CliError::HostBuildUnsupported { os: os.to_owned() })
        }
        Some(Builder::Host) => Ok((Builder::Host, None)),
        Some(Builder::Vm) | None if tools.can_build() => Ok((Builder::Vm, None)),
        None if linux_host => Ok((
            Builder::Host,
            Some(format!(
                "the VM's image has no {}, so this build runs on this host (--builder host)",
                tools.missing_for_build().join(", ")
            )),
        )),
        Some(Builder::Vm) | None => Err(cannot()),
    }
}

/// `tilo-vm input` arguments as protocol actions.
///
/// # Errors
/// Unknown buttons, and keys or characters that cannot be sent.
pub fn input_actions(command: InputCommand) -> Result<Vec<InputAction>, CliError> {
    let action = match command {
        InputCommand::Key { combos } => InputAction::Key { combos },
        InputCommand::Type { text } => InputAction::Type { text },
        InputCommand::Move { x, y } => InputAction::Move { x, y },
        InputCommand::Click { button, at } => InputAction::Click {
            button: Button::parse(&button).ok_or_else(|| {
                CliError::InvalidInput(format!("'{button}' is not a button (left, right, middle)"))
            })?,
            at: match at.as_deref() {
                Some([x, y]) => Some((*x, *y)),
                _ => None,
            },
        },
        InputCommand::Touch(gesture) => touch_action(gesture),
        InputCommand::Pen(PenCommand::Tap { x, y, pressure }) => {
            InputAction::PenTap { x, y, pressure }
        }
        InputCommand::Pen(PenCommand::Drag {
            x1,
            y1,
            x2,
            y2,
            duration,
            pressure,
        }) => InputAction::PenDrag {
            from: (x1, y1),
            to: (x2, y2),
            duration_ms: duration,
            pressure,
        },
        InputCommand::Gamepad(command) => gamepad_action(command)?,
    };
    let actions = vec![action];
    tilo_vm_protocol::input::validate(&actions).map_err(CliError::InvalidInput)?;
    Ok(actions)
}

fn touch_action(gesture: TouchCommand) -> InputAction {
    match gesture {
        TouchCommand::Tap { x, y } => InputAction::TouchTap { x, y },
        TouchCommand::Hold { x, y, duration } => InputAction::TouchHold {
            x,
            y,
            duration_ms: duration,
        },
        TouchCommand::Drag {
            x1,
            y1,
            x2,
            y2,
            duration,
        } => InputAction::TouchDrag {
            from: (x1, y1),
            to: (x2, y2),
            duration_ms: duration,
        },
        TouchCommand::Pinch {
            x,
            y,
            from,
            to,
            duration,
        } => InputAction::TouchPinch {
            center: (x, y),
            from_distance: from,
            to_distance: to,
            duration_ms: duration,
        },
    }
}

fn gamepad_action(command: GamepadCommand) -> Result<InputAction, CliError> {
    Ok(match command {
        GamepadCommand::Press { combos } => InputAction::GamepadPress { combos },
        GamepadCommand::Dpad { directions } => {
            if let Some(bad) = directions.iter().find(|d| {
                !matches!(
                    d.to_ascii_lowercase().as_str(),
                    "up" | "down" | "left" | "right"
                )
            }) {
                return Err(CliError::InvalidInput(format!(
                    "'{bad}' is not a d-pad direction (up, down, left, right)"
                )));
            }
            InputAction::GamepadPress { combos: directions }
        }
        GamepadCommand::Stick {
            stick,
            x,
            y,
            duration,
        } => InputAction::GamepadStick {
            stick: Stick::parse(&stick).ok_or_else(|| {
                CliError::InvalidInput(format!("'{stick}' is not a stick (left, right)"))
            })?,
            x,
            y,
            duration_ms: duration,
        },
    })
}

fn describe_input(action: &InputAction) -> String {
    match action {
        InputAction::Key { combos } => format!("pressed {}", combos.join(" ")),
        InputAction::Type { text } => format!("typed {} characters", text.chars().count()),
        InputAction::Move { x, y } => format!("moved the pointer to {x},{y}"),
        InputAction::Click { button, at } => {
            let at = at.map(|(x, y)| format!(" at {x},{y}")).unwrap_or_default();
            format!("clicked {button:?}{at}").to_lowercase()
        }
        InputAction::TouchTap { x, y } => format!("tapped {x},{y}"),
        InputAction::TouchHold { x, y, duration_ms } => {
            format!("held a finger on {x},{y} for {duration_ms} ms")
        }
        InputAction::TouchDrag { from, to, .. } => {
            format!(
                "dragged a finger from {},{} to {},{}",
                from.0, from.1, to.0, to.1
            )
        }
        InputAction::TouchPinch {
            center,
            from_distance,
            to_distance,
            ..
        } => format!(
            "pinched at {},{} from {from_distance} to {to_distance} px apart",
            center.0, center.1
        ),
        InputAction::PenTap { x, y, .. } => format!("tapped the pen at {x},{y}"),
        InputAction::PenDrag { from, to, .. } => {
            format!(
                "drew with the pen from {},{} to {},{}",
                from.0, from.1, to.0, to.1
            )
        }
        InputAction::GamepadPress { combos } => format!("pressed gamepad {}", combos.join(" ")),
        InputAction::GamepadStick { stick, x, y, .. } => {
            format!("pushed the {stick:?} stick to {x},{y}").to_lowercase()
        }
    }
}

fn app_set(set: &str) -> Result<appsets::AppSet, CliError> {
    appsets::find(set).map_err(|known| CliError::UnknownAppSet {
        set: set.to_owned(),
        known,
    })
}

/// `slot apps list`: every set, what each app checks and how to start it.
fn apps_list() -> Outcome {
    let sets = appsets::all();
    let text = sets
        .iter()
        .map(appsets::describe)
        .collect::<Vec<_>>()
        .join("\n\n");
    let json: Vec<_> = sets
        .iter()
        .map(|set| {
            json!({
                "name": set.name,
                "summary": set.summary,
                "apps": set.apps.iter().map(|a| json!({
                    "package": a.package, "checks": a.checks, "try": a.try_command,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let first = sets.first().map_or("verification", |s| s.name.as_str());
    Outcome::new(text, json!(json)).next(&[&format!("tilo-vm slot apps install --set {first}")])
}

/// What `slot create` did, in a sentence or two.
fn slot_created_text(
    name: &str,
    kind: SlotKind,
    result: &tilo_vm_protocol::SlotCreateResult,
) -> String {
    let user = result.slot.user.clone().unwrap_or_default();
    let set_outputs = result.actions.iter().find_map(|action| match action {
        tilo_vm_protocol::Action::SetOutputs { outputs } => Some(outputs.clone()),
        _ => None,
    });
    // The restart that gives a new identity fresh desktop state is not a
    // start of a stopped session.
    let started = result
        .actions
        .iter()
        .take_while(|action| {
            !matches!(
                action,
                tilo_vm_protocol::Action::ProvisionTestIdentity { .. }
            )
        })
        .any(|action| !matches!(action, tilo_vm_protocol::Action::SetOutputs { .. }));
    let text = if result.created {
        format!(
            "slot {name} created ({}); session started as {user} on the base image",
            kind.as_str()
        )
    } else if started {
        format!("slot {name} already existed; its session was not running and has been started")
    } else if set_outputs.is_some() {
        format!("slot {name} already exists and its session is running")
    } else {
        format!("slot {name} already exists and its session is running; nothing to do")
    };
    let text = match &result.identity {
        Some(identity) if identity.created => format!(
            "{text}\nthrowaway test identity \"{}\" minted; the session restarted with fresh desktop state",
            identity.display_name
        ),
        Some(identity) => format!(
            "{text}\nits user keeps throwaway test identity \"{}\"",
            identity.display_name
        ),
        None => text,
    };
    match set_outputs {
        Some(outputs) => format!(
            "{text}\noutputs {outputs} (left to right), set and confirmed through the display settings"
        ),
        None => text,
    }
}

fn slot_line(info: &SlotInfo) -> String {
    let seat = info
        .seat
        .as_deref()
        .map(|s| format!(", seat {s}"))
        .unwrap_or_default();
    let owner = describe_owner(&info.owner)
        .map(|o| format!(", owner {o}"))
        .unwrap_or_default();
    let outputs = if info.outputs.is_empty() {
        String::new()
    } else {
        format!(
            ", outputs {}",
            tilo_vm_protocol::display::format_outputs(&info.outputs)
        )
    };
    format!(
        "slot: {} ({}) generation {}, session {}{seat}{outputs}{owner}",
        info.name,
        info.kind.as_str(),
        info.generation,
        info.session
    )
}

/// `status` for a slot that does not exist: create it when the VM has no
/// slots, else show the ones it has rather than suggest yet another.
fn missing_slot(name: &str, slots: &[SlotInfo]) -> (Vec<String>, Vec<String>) {
    let others: Vec<&SlotInfo> = slots.iter().filter(|s| s.kind != SlotKind::Vm).collect();
    if others.is_empty() {
        return (
            vec![format!(
                "slot: {name} (not created yet; new here? tilo-vm guide quickstart)"
            )],
            vec![
                "tilo-vm slot create --headless".into(),
                "tilo-vm guide quickstart".into(),
            ],
        );
    }
    let mut lines = vec![format!(
        "slot: {name} does not exist; the VM has these (use one with --slot <name>, or set TILO_VM_SLOT):"
    )];
    lines.extend(others.iter().map(|s| format!("  {}", slot_line(s))));
    let mut next: Vec<String> = others
        .iter()
        .take(3)
        .map(|s| format!("tilo-vm status --slot {}", s.name))
        .collect();
    next.push("tilo-vm slot list".into());
    (lines, next)
}

/// `host:branch`, or whatever of host, branch and worktree is known.
fn describe_owner(owner: &Owner) -> Option<String> {
    let place = owner.branch.clone().or_else(|| {
        owner
            .worktree
            .as_deref()
            .and_then(|w| w.rsplit('/').next())
            .map(str::to_owned)
    });
    match (owner.host.clone(), place) {
        (Some(host), Some(place)) => Some(format!("{host}:{place}")),
        (Some(one), None) | (None, Some(one)) => Some(one),
        (None, None) => None,
    }
}

/// What exercises a scenario-only boot component. The installer has its
/// own `tilo-vm scenario`; the others name the os/dev driver the manifest
/// records.
pub fn scenario_driver(entry: &Component) -> String {
    match entry.name.as_str() {
        "installer" => {
            "tilo-vm scenario install   (a throwaway VM installs from your medium through the installer's actions and cold-boots the result; the release gate is os/dev/test-installer-cycle.sh)".into()
        }
        _ => entry
            .scenario
            .clone()
            .unwrap_or_else(|| "os/dev/vm.sh boot".into()),
    }
}

/// The slot a command names explicitly, after `--slot` was folded in.
fn named_slot(command: &Command) -> Option<String> {
    match command {
        Command::Swap { slot, .. }
        | Command::Input { slot, .. }
        | Command::Control { slot, .. }
        | Command::Flight(FlightCommand::App { slot, .. })
        | Command::Rollback { slot, .. }
        | Command::Logs { slot, .. }
        | Command::Shot { slot, .. }
        | Command::Status { slot }
        | Command::Seat(SeatCommand::Claim { slot, .. } | SeatCommand::Release { slot, .. })
        | Command::Slot(
            SlotCommand::Create { name: slot, .. }
            | SlotCommand::Restart { name: slot, .. }
            | SlotCommand::Rm { name: slot, .. }
            | SlotCommand::Show { name: slot, .. }
            | SlotCommand::Exec { name: slot, .. }
            | SlotCommand::Apps(SlotAppsCommand::Install { name: slot, .. }),
        ) => slot.clone(),
        _ => None,
    }
}

/// `command` aimed at `slot`: `--slot <slot>` right after the subcommand
/// (before any trailing arguments, which `control` would swallow).
pub fn with_slot(command: &str, slot: &str) -> String {
    let words: Vec<&str> = command.split(' ').collect();
    if words.first() != Some(&"tilo-vm") || command.contains("--slot") {
        return command.to_owned();
    }
    let at = match (words.get(1).copied(), words.get(2).copied()) {
        (Some("swap" | "shot" | "control" | "logs" | "rollback" | "input" | "status"), _) => 2,
        (Some("slot"), Some("create" | "restart" | "rm" | "show" | "exec"))
        | (Some("seat"), Some("claim" | "release")) => 3,
        _ => return command.to_owned(),
    };
    let Some((head, tail)) = words.split_at_checked(at) else {
        return command.to_owned();
    };
    let flag = format!("--slot {slot}");
    let mut out: Vec<&str> = head.to_vec();
    out.push(&flag);
    out.extend_from_slice(tail);
    out.join(" ")
}

/// The generation `rollback --forward` goes to: the next newer one.
///
/// # Errors
/// An unknown slot, or one already at its newest generation.
pub fn forward_target(slot: &str, info: Option<&SlotInfo>) -> Result<u64, CliError> {
    let info = info.ok_or_else(|| CliError::Unsupported {
        what: format!("rolling slot {slot} forward: the VM has no such slot"),
        alternative: "tilo-vm slot list".into(),
    })?;
    info.generations
        .iter()
        .copied()
        .find(|n| *n > info.generation)
        .ok_or_else(|| CliError::Unsupported {
            what: format!(
                "rolling slot {slot} forward: generation {} is its newest",
                info.generation
            ),
            alternative: format!(
                "pick one by number: tilo-vm rollback --slot {slot} --to <n>   (tilo-vm slot show --slot {slot} lists them)"
            ),
        })
}

/// The remote words of `slot exec`, quoted for the remote shell.
/// The limits an app flight must stay within: the launch, the slowest control
/// round trip, and the whole flight.
#[derive(Clone, Copy)]
struct FlightBudget {
    maximum_launch: Duration,
    maximum_control: Duration,
    timeout: Duration,
}

fn exec_words(daemon: Option<&str>, slot: &str, timeout: u64, command: &[String]) -> Vec<String> {
    let mut words = vmbuild::daemon_command(daemon, "exec");
    words.extend([
        "--slot".to_owned(),
        vmbuild::sh_quote(slot),
        "--timeout".to_owned(),
        timeout.to_string(),
        "--".to_owned(),
    ]);
    words.extend(command.iter().map(|w| vmbuild::sh_quote(w)));
    words
}

/// `slot show` as text: the slot, then each component's files.
fn describe_show(shown: &SlotShowResult) -> String {
    let info = &shown.slot;
    let mut lines = vec![
        slot_line(info),
        format!(
            "  generations: {} (current {}; tilo-vm rollback --to <n> switches to any of them)",
            info.generations
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            info.generation
        ),
    ];
    if shown.files.is_empty() {
        lines.push("  no layers: the slot sees the VM's image as it is".into());
    }
    let mut current = None;
    for file in &shown.files {
        if current != Some((&file.component, &file.layer)) {
            current = Some((&file.component, &file.layer));
            lines.push(format!(
                "  {} (layer {})",
                file.component,
                short(&file.layer)
            ));
        }
        let what = match (&file.sha256, &file.link) {
            (Some(sha), _) => format!("sha256:{}", short(sha)),
            (None, Some(link)) => format!("-> {link}"),
            (None, None) => String::new(),
        };
        lines.push(format!("    {}  {what}", file.path));
    }
    lines.join("\n")
}

/// What gc kept for its age, and when it goes.
fn describe_kept(result: &tilo_vm_protocol::GcResult) -> Option<String> {
    if result.kept_young.is_empty() {
        return None;
    }
    let age = |seconds: u64| {
        if seconds >= 3600 {
            format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60)
        } else {
            format!("{}m", seconds.div_ceil(60))
        }
    };
    let mut lines = vec![format!(
        "kept {} unreferenced layers younger than {} (an import may still be waiting for its swap):",
        result.kept_young.len(),
        age(result.min_age_seconds)
    )];
    for kept in &result.kept_young {
        lines.push(format!(
            "  {} {} (age {}, eligible in {})",
            kept.component,
            short(&kept.hash),
            age(kept.age_seconds),
            age(kept.eligible_in_seconds)
        ));
    }
    lines.push(
        "  your own orphans can go now: tilo-vm gc --min-age 0 (not while another agent's swap may be in flight)".into(),
    );
    Some(lines.join("\n"))
}

fn doctor_outcome(checks: &[Check]) -> Outcome {
    let failed = checks.iter().any(|c| c.state == "fail");
    let text = checks
        .iter()
        .map(|c| {
            let mark = match c.state {
                "ok" => "ok  ",
                "warn" => "warn",
                _ => "FAIL",
            };
            let mut line = format!("{mark} {:<18} {}", c.check, c.detail);
            if let Some(fix) = &c.fix {
                let _ = write!(line, "\n     fix: {fix}");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut outcome = Outcome::new(text, json!({ "ok": !failed, "checks": checks }));
    outcome.failed = failed;
    outcome
}

/// Put an error's `error`, `hint` and `see` at the top level of a report,
/// the one shape every `--json` error has.
fn merge_error(data: &mut Value, error: &CliError) {
    if let (Value::Object(map), Value::Object(fields)) = (data, error.to_json()) {
        map.extend(fields);
    }
}

/// Why a VM's daemon could not be reached, for `status` and `vm status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectFailure {
    /// `off`, `no-ssh`, `key-refused`, `no-daemon`, `incompatible` or `error`.
    pub state: &'static str,
    /// What the VM is doing, in one line.
    pub summary: String,
    /// Whether SSH answered.
    pub ssh: bool,
    pub next: Vec<String>,
}

impl ConnectFailure {
    fn text(&self, error: &CliError) -> String {
        // A VM that is off explains every connection error; repeating it is noise.
        if self.state == "off" {
            return format!("  {}", self.summary);
        }
        format!(
            "  {}\n  error: {error}\n  hint: {}",
            self.summary,
            error.hint()
        )
    }
}

/// Tell apart a VM that is off, one whose SSH does not answer, one without
/// tilo-vmd and one whose tilo-vmd is incompatible; each has its own fix.
pub fn connect_failure(
    vm: &str,
    running: Option<bool>,
    debug: &[String],
    error: &CliError,
) -> ConnectFailure {
    let fail = |state, summary: String, ssh, next: Vec<String>| ConnectFailure {
        state,
        summary,
        ssh,
        next,
    };
    let start = format!("tilo-vm vm start {vm}");
    let ssh = format!("tilo-vm vm ssh {vm}");
    let no_ssh = || {
        let mut next = debug.to_vec();
        next.push("tilo-vm doctor".into());
        next
    };
    match error {
        CliError::RootKeyRefused { .. } => fail(
            "key-refused",
            "running; SSH answers but refuses the root key".into(),
            true,
            vec!["tilo-vm guide platforms".into(), "tilo-vm doctor".into()],
        ),
        CliError::DaemonMissing { bootc, .. } => fail(
            "no-daemon",
            "running and reachable over SSH, but no tilo-vmd: it is not running a Tilo dev image"
                .into(),
            true,
            match bootc {
                Some(true) => vec![
                    format!("tilo-vm vm upgrade {vm} --image registry.tilo.so/tilo/tilo-vm:<tag>"),
                    ssh,
                ],
                Some(false) => vec!["tilo-vm vm create <name>".into(), ssh],
                None => vec![format!("tilo-vm vm status {vm}"), ssh],
            },
        ),
        CliError::ProtocolMismatch {
            client,
            daemon,
            bootc,
            ..
        } => fail(
            "incompatible",
            format!(
                "running and reachable over SSH, but its tilo-vmd speaks protocol {daemon} and this tilo-vm speaks {client}"
            ),
            true,
            match (client > daemon, bootc) {
                (false, _) => vec!["cargo build -p tilo-vm".into()],
                (true, Some(true)) => vec![format!("tilo-vm vm upgrade {vm}")],
                (true, Some(false)) => vec!["tilo-vm vm create <name>".into(), ssh],
                (true, None) => vec![format!("tilo-vm vm status {vm}")],
            },
        ),
        _ if running == Some(false) => fail(
            "off",
            "shut off (the hypervisor reports it is not running)".into(),
            false,
            vec![start],
        ),
        CliError::Unreachable { .. } if running == Some(true) => fail(
            "no-ssh",
            "running, but SSH does not answer (still booting, or stuck)".into(),
            false,
            no_ssh(),
        ),
        CliError::Unreachable { .. } => {
            let mut next = vec![format!("{start}   (if it is off)")];
            next.extend(no_ssh());
            fail(
                "no-ssh",
                "SSH does not answer (the hypervisor cannot say whether it is running)".into(),
                false,
                next,
            )
        }
        _ => fail(
            "error",
            "could not be checked".into(),
            false,
            vec!["tilo-vm doctor".into()],
        ),
    }
}

fn unsupported(adapter: &dyn Hypervisor, what: &str) -> CliError {
    let caps = adapter.capabilities();
    CliError::Unsupported {
        what: format!("{what} with the {} adapter", adapter.name()),
        alternative: format!(
            "this adapter supports: serial {}, screenshot {}, reset {}, control {}; open the VM in the hypervisor's own UI",
            yes(caps.serial),
            yes(caps.screenshot),
            yes(caps.reset),
            caps.control.unwrap_or("no")
        ),
    }
}

struct LocalDisks {
    image: Option<PathBuf>,
    image_format: Option<image::Format>,
    import: Option<PathBuf>,
    boot_esp: Option<PathBuf>,
}

/// `vm create`'s local files: `--image` and its format, where WHPX imports
/// one on a network path, and a `--boot-esp` the adapter can attach.
fn check_local_disks(
    adapter: &str,
    image: Option<&str>,
    boot_esp: Option<&str>,
) -> Result<LocalDisks, CliError> {
    let image = match image {
        Some(path) => {
            let path = absolute(Path::new(path));
            if !path.is_file() {
                return Err(CliError::ImageNotFound {
                    path: path.display().to_string(),
                });
            }
            Some(path)
        }
        None => None,
    };
    // Hyper-V copies its VHDX; the other adapters make a qcow2 child.
    let image_format =
        match &image {
            Some(path) if adapter != "hyperv" => Some(image::format_of(path).map_err(
                |detail| CliError::ImageFormat {
                    flag: "--image".into(),
                    path: path.display().to_string(),
                    detail,
                },
            )?),
            _ => None,
        };
    let import = match &image {
        Some(path) if adapter == "whpx" => {
            let text = path.display().to_string();
            if image::is_network_path(&text) {
                let meta = std::fs::metadata(path).map_err(CliError::io(format!("read {text}")))?;
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                let cache = xdg_paths::cache_dir().unwrap_or_else(std::env::temp_dir);
                Some(image::import_path(&cache, &text, meta.len(), modified))
            } else {
                None
            }
        }
        _ => None,
    };
    let esp = match boot_esp {
        Some(path) => {
            if !hypervisor::adapter(adapter)?.capabilities().boot_esp {
                return Err(CliError::Unsupported {
                    what: format!("--boot-esp with the {adapter} adapter"),
                    alternative: "use libvirt (Linux) or whpx (Windows ARM64) for a VM that boots a separate ESP".into(),
                });
            }
            let path = absolute(Path::new(path));
            let text = path.display().to_string();
            let refuse = |detail: String| CliError::ImageFormat {
                flag: "--boot-esp".into(),
                path: text.clone(),
                detail,
            };
            if image::is_network_path(&text) {
                return Err(refuse(
                    "is on a network path; QEMU does not read a disk there reliably".into(),
                ));
            }
            match image::format_of(&path).map_err(refuse)? {
                image::Format::Raw => Some(path),
                image::Format::Qcow2 => {
                    return Err(CliError::ImageFormat {
                        flag: "--boot-esp".into(),
                        path: path.display().to_string(),
                        detail: "is qcow2; the ESP is attached as a raw disk".into(),
                    });
                }
            }
        }
        None => None,
    };
    Ok(LocalDisks {
        image,
        image_format,
        import,
        boot_esp: esp,
    })
}

/// Copy a network image to `dest` through a verified partial file.
fn import_image(
    runner: &mut dyn Runner,
    qemu_img: &str,
    source: &str,
    format: image::Format,
    dest: &Path,
) -> Result<(), CliError> {
    let part = image::import_part(dest);
    ensure_parent(dest)?;
    remove_if_present(&part)?;
    let result = run_all(
        runner,
        &image::import_commands(qemu_img, source, format, &part),
    );
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result?;
    std::fs::rename(&part, dest).map_err(CliError::io(format!(
        "move {} to {}",
        part.display(),
        dest.display()
    )))
}

fn remove_if_present(path: &Path) -> Result<bool, CliError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(CliError::io(format!("remove {}", path.display()))(e)),
    }
}

/// Resolve a CI dev-disk companion bundle before downloading or creating a VM.
/// The kernel, initrd and command line must describe the same disk image.
fn read_direct_boot_bundle(path: &Path) -> Result<DirectKernel, CliError> {
    let path = absolute(path);
    let text =
        std::fs::read_to_string(&path).map_err(CliError::io(format!("read {}", path.display())))?;
    let mut direct: DirectKernel = serde_json::from_str(&text).map_err(|e| CliError::Config {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    let invalid = |detail: &str| CliError::Config {
        path: path.display().to_string(),
        detail: detail.to_owned(),
    };
    if !direct
        .cmdline
        .split_whitespace()
        .any(|arg| arg == "root=tilo-auto")
    {
        return Err(invalid(
            "direct boot needs the complete Tilo UKI command line with root=tilo-auto",
        ));
    }
    if direct.cmdline.contains('\0')
        || direct
            .cmdline
            .contains("systemd.setenv=TILO_ROOT_LOADER_PARTUUID=")
    {
        return Err(invalid(
            "the bundle command line must not include a loader UUID override",
        ));
    }
    let partuuid = direct
        .loader_partuuid
        .as_deref()
        .ok_or_else(|| invalid("the bundle needs loader_partuuid for its source disk's ESP"))?;
    direct.loader_partuuid = Some(
        uuid::Uuid::parse_str(partuuid)
            .map_err(|_| invalid("loader_partuuid is not a GPT partition UUID"))?
            .to_string(),
    );
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for file in [&mut direct.kernel, &mut direct.initrd] {
        let resolved = Path::new(file);
        let resolved = if resolved.is_absolute() {
            resolved.to_owned()
        } else {
            parent.join(resolved)
        };
        if !resolved.is_file() {
            return Err(invalid(&format!(
                "missing direct boot file {}",
                resolved.display()
            )));
        }
        *file = resolved.display().to_string();
    }
    direct.cmdline = direct.cmdline.trim().to_owned();
    Ok(direct)
}

fn read_published_direct_boot(
    path: &Path,
    tag: &str,
    arch: &str,
) -> Result<DirectKernel, CliError> {
    let direct = read_direct_boot_bundle(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let expected_kernel = parent.join(format!("tilo-vm-{tag}-{arch}-direct-Image"));
    let expected_initrd = parent.join(format!("tilo-vm-{tag}-{arch}-direct-initrd"));
    if direct.kernel != expected_kernel.display().to_string()
        || direct.initrd != expected_initrd.display().to_string()
    {
        return Err(CliError::Config {
            path: path.display().to_string(),
            detail: "kernel and initrd do not name the verified disk companions".into(),
        });
    }
    Ok(direct)
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

fn ensure_parent(path: &Path) -> Result<(), CliError> {
    match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent)
            .map_err(CliError::io(format!("create {}", parent.display()))),
        None => Ok(()),
    }
}

/// Poll until the slot's session is active again, at most 30 seconds.
fn wait_for_session(remote: &mut Remote, slot: &str) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        let active = remote
            .call(Status {
                slot: Some(slot.to_owned()),
            })
            .ok()
            .and_then(|s| s.slots.into_iter().next())
            .is_some_and(|s| s.session == "active");
        if active {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `image build` runs `--from local`'s build with or without the disk, keeps its scratch next
    /// to (never inside) the output it leaves to be fetched, and finds a checkout without git.
    #[test]
    fn image_build_runs_the_local_build_into_its_own_directory() {
        let command = image_build_command(
            Path::new("/w/os/dev/build-dev-image.sh"),
            "localhost/tilo-vm:mine",
            true,
            Path::new("/w/target/tilo-vm-build"),
        );
        assert_eq!(command.program, "/w/os/dev/build-dev-image.sh");
        let env = |key: &str| {
            command
                .env
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(env("TILO_DEV_DISK"), Some("1"));
        assert_eq!(env("TILO_DEV_IMAGE"), Some("localhost/tilo-vm:mine"));
        assert_eq!(env("TILO_DEV_BASE_BUILD"), Some("auto"));
        assert_eq!(env("TILO_OUTPUT"), Some("/w/target/tilo-vm-build"));
        let no_disk = image_build_command(
            Path::new("/s"),
            "localhost/tilo-vm:x",
            false,
            Path::new("/o"),
        );
        assert!(no_disk.env.contains(&("TILO_DEV_DISK".into(), "0".into())));

        assert_eq!(
            build_scratch(Path::new("/w/target/tilo-vm")),
            Path::new("/w/target/tilo-vm-build")
        );

        // image disk: the build's own disk step, on the layout imported into podman.
        let disk = image_disk_command(
            Path::new("/w/os/dev/build-image.sh"),
            "localhost/tilo-vm:mine",
            Path::new("/w/target/tilo-vm-build"),
        );
        assert!(disk.env.contains(&("TILO_SKIP_BUILD".into(), "1".into())));
        assert!(disk.env.contains(&("TILO_DISK_SIZE".into(), "64G".into())));
        // Root podman's copy stays in the scratch directory, not /var/lib/containers.
        assert_eq!(
            disk.env
                .iter()
                .find(|(key, _)| key == "TILO_ROOT_PODMAN_STORE")
                .map(|(_, value)| PathBuf::from(value)),
            Some(Path::new("/w/target/tilo-vm-build").join("root-podman"))
        );
        assert!(
            disk.env
                .contains(&("TILO_IMAGE".into(), "localhost/tilo-vm:mine".into()))
        );
        assert_eq!(
            imageup::import_layout_command(
                Path::new("/w/target/tilo-vm/image"),
                "localhost/tilo-vm:latest"
            )
            .display(),
            "skopeo copy --quiet oci:/w/target/tilo-vm/image:tilo-vm containers-storage:localhost/tilo-vm:latest"
        );

        let tree = tempfile::tempdir().unwrap();
        let script = tree.path().join(imageup::BUILD_SCRIPT);
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "").unwrap();
        let deep = tree.path().join("crates/dev/tilo-vm");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(checkout_root(&deep).as_deref(), Some(tree.path()));
        assert_eq!(checkout_root(Path::new("/")), None);
    }

    fn result(policy: Policy, actions: Vec<Action>, changed: bool) -> ApplyResult {
        ApplyResult {
            slot: "a".into(),
            generation: 4,
            changed,
            policy,
            actions,
            warnings: vec![],
            boot: None,
        }
    }

    /// Swapping the installer redirects to its scenario; kernel applies say the reboot is next; apply
    /// descriptions name the consequence; vm ssh refuses writes before it looks for a VM.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn commands_describe_their_consequence_and_redirect_where_needed() {
        // Phase: swapping_the_installer_redirects_to_its_scenario.
        {
            let manifest = Manifest::parse(
                "[[component]]\nname = \"installer\"\nkind = \"boot\"\nscope = \"vm\"\nscenario = \"os/dev/test-installer-cycle.sh\"\n\n[[component]]\nname = \"uki\"\nkind = \"boot\"\nscope = \"vm\"\nscenario = \"os/dev/test-measured-boot.sh\"\n",
            )
            .unwrap();
            let installer = scenario_driver(manifest.get("installer").unwrap());
            assert!(
                installer.starts_with("tilo-vm scenario install"),
                "{installer}"
            );
            let error = CliError::BootComponent {
                name: "installer".into(),
                driver: installer,
            };
            assert!(
                error.hint().contains("tilo-vm scenario install"),
                "{}",
                error.hint()
            );
            assert_eq!(
                scenario_driver(manifest.get("uki").unwrap()),
                "os/dev/test-measured-boot.sh"
            );
        }

        // Phase: kernel_applies_say_the_reboot_is_next.
        {
            let mut swapped = result(Policy::Reboot, vec![Action::SysextRefresh], true);
            swapped.boot = Some(tilo_vm_protocol::BootChange {
                kver: Some("7.2.5+dev".into()),
                entry: Some("tilo-vm-7.2.5+dev-0123456789ab.conf".into()),
                fallback: Some("fedora.conf".into()),
                loader: "systemd-boot".into(),
                initramfs: Some("dracut".into()),
                initramfs_seconds: 30.0,
            });
            let text = describe_apply(&swapped);
            assert!(
                text.contains(
                    "kernel 7.2.5+dev is the next boot (tilo-vm-7.2.5+dev-0123456789ab.conf)"
                ),
                "{text}"
            );
            assert!(
                text.ends_with("it takes effect when the VM reboots"),
                "{text}"
            );
            let mut back = result(Policy::Reboot, vec![], true);
            back.boot = Some(tilo_vm_protocol::BootChange {
                kver: None,
                entry: Some("fedora.conf".into()),
                fallback: None,
                loader: "systemd-boot".into(),
                initramfs: None,
                initramfs_seconds: 0.0,
            });
            assert!(
                describe_apply(&back)
                    .contains("the image's kernel is the default boot again (fedora.conf)")
            );
        }

        // Phase: apply_descriptions_name_the_consequence.
        {
            assert_eq!(
                describe_apply(&result(Policy::SessionRestart, vec![], true)),
                "generation 4 applied to slot a; session restarted (logout, user manager restart, login)"
            );
            assert!(
                describe_apply(&result(Policy::NoRestart, vec![], false)).contains("nothing to do")
            );
            let vm = describe_apply(&result(
                Policy::VmRefresh,
                vec![
                    Action::SysextRefresh,
                    Action::RestartUnit {
                        unit: "fs.service".into(),
                    },
                    Action::RestartUnit {
                        unit: "user@60001.service".into(),
                    },
                    Action::StartUnit {
                        unit: "tilo-vm-session@b.service".into(),
                    },
                ],
                true,
            ));
            assert!(vm.contains("restarted fs.service;"), "{vm}");
            assert!(vm.contains("every slot so they see it: b"), "{vm}");
        }

        // Phase: vm_ssh_refuses_writes_before_it_looks_for_a_vm.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            let ssh = |raw: bool, line: &str| {
                Command::Vm(VmCommand::Ssh {
                    raw,
                    name: None,
                    vm: None,
                    command: line.split(' ').map(str::to_owned).collect(),
                })
            };
            let error = app
                .run(Some(ssh(false, "cp shell /usr/bin/shell")))
                .err()
                .unwrap();
            assert!(matches!(error, CliError::RawWrite { .. }), "{error}");
            assert!(!error.to_string().contains("deploy"), "{error}");
            assert!(
                error.to_string().contains("writes /usr/bin/shell"),
                "{error}"
            );
            assert!(error.hint().contains("--raw"), "{}", error.hint());
            // Reads and --raw get past the guard, to the missing VM.
            for command in [
                ssh(false, "systemctl status tilo-vmd --no-pager"),
                ssh(false, "tilo-vmd --version"),
                ssh(true, "cp shell /usr/bin/shell"),
            ] {
                let error = app.run(Some(command)).err().unwrap();
                assert!(
                    matches!(error, CliError::NoVm { .. } | CliError::UnknownVm { .. }),
                    "{error}"
                );
            }
        }
    }

    /// Status tells off from silent from missing from incompatible; the builder choice prefers the
    /// VM and explains fallbacks; a refused key names the install step.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn status_and_errors_explain_what_is_wrong_and_what_to_do() {
        // Phase: vm_start_retries_only_transport_failures.
        {
            let unreachable = Err(CliError::Unreachable {
                vm: "dev".into(),
                detail: "connection refused".into(),
                debug: Vec::new(),
            });
            assert_eq!(
                start_connect_disposition(&unreachable),
                StartConnectDisposition::Retry
            );
            let missing = Err(CliError::DaemonMissing {
                vm: "dev".into(),
                bootc: Some(false),
            });
            assert_eq!(
                start_connect_disposition(&missing),
                StartConnectDisposition::NoDaemon
            );
            let mismatch = Err(CliError::ProtocolMismatch {
                vm: "dev".into(),
                client: 5,
                daemon: 3,
                bootc: Some(false),
            });
            assert_eq!(
                start_connect_disposition(&mismatch),
                StartConnectDisposition::Fail
            );
            let refused = Err(CliError::RootKeyRefused {
                vm: "dev".into(),
                key: "/tmp/key".into(),
                install: "install key".into(),
                credentials: false,
            });
            assert_eq!(
                start_connect_disposition(&refused),
                StartConnectDisposition::Fail
            );
        }

        // Phase: status_tells_off_from_silent_from_missing_from_incompatible.
        {
            let debug = [
                "tilo-vm vm shot dev".to_owned(),
                "tilo-vm vm serial dev".to_owned(),
            ];
            let unreachable = CliError::Unreachable {
                vm: "dev".into(),
                detail: "Connection refused".into(),
                debug: debug.to_vec(),
            };
            let off = connect_failure("dev", Some(false), &debug, &unreachable);
            assert_eq!(off.state, "off");
            assert!(off.summary.contains("shut off"), "{}", off.summary);
            assert_eq!(off.next, ["tilo-vm vm start dev"]);
            assert!(!off.text(&unreachable).contains("Connection refused"));

            let silent = connect_failure("dev", Some(true), &debug, &unreachable);
            assert_eq!(silent.state, "no-ssh");
            assert!(
                !silent.next.iter().any(|n| n.contains("vm start")),
                "{:?}",
                silent.next
            );
            assert!(silent.next.contains(&"tilo-vm vm serial dev".to_owned()));
            assert!(silent.next.contains(&"tilo-vm doctor".to_owned()));
            let unknown = connect_failure("dev", None, &debug, &unreachable);
            assert!(unknown.next[0].starts_with("tilo-vm vm start dev"));

            let missing = CliError::DaemonMissing {
                vm: "dev".into(),
                bootc: Some(false),
            };
            let no_daemon = connect_failure("dev", Some(true), &debug, &missing);
            assert_eq!(no_daemon.state, "no-daemon");
            assert!(no_daemon.ssh);
            assert!(!no_daemon.next.iter().any(|n| n.contains("vm upgrade")));

            let older_vm = |bootc| CliError::ProtocolMismatch {
                vm: "dev".into(),
                client: 3,
                daemon: 2,
                bootc,
            };
            let incompatible = connect_failure("dev", Some(true), &debug, &older_vm(Some(true)));
            assert_eq!(incompatible.state, "incompatible");
            assert!(incompatible.ssh);
            assert!(
                incompatible.summary.contains("protocol 2"),
                "{}",
                incompatible.summary
            );
            assert!(
                incompatible.summary.contains("speaks 3"),
                "{}",
                incompatible.summary
            );
            assert_eq!(incompatible.next, ["tilo-vm vm upgrade dev"]);
            assert!(!incompatible.next.iter().any(|n| n.contains("vm start")));
            let no_bootc = connect_failure("dev", None, &debug, &older_vm(Some(false)));
            assert!(
                !no_bootc.next.iter().any(|n| n.contains("vm upgrade")),
                "{:?}",
                no_bootc.next
            );
            let older_tool = CliError::ProtocolMismatch {
                vm: "dev".into(),
                client: 2,
                daemon: 3,
                bootc: Some(true),
            };
            assert_eq!(
                connect_failure("dev", Some(true), &debug, &older_tool).next,
                ["cargo build -p tilo-vm"]
            );
        }

        // Phase: vm_stop_escalates_past_a_guest_that_ignores_acpi.
        {
            struct Guest {
                /// The command whose run brings the VM down.
                obeys: &'static str,
                ran: Vec<String>,
                down: bool,
            }
            impl Runner for Guest {
                fn output(
                    &mut self,
                    command: &CommandLine,
                ) -> Result<crate::hypervisor::Output, CliError> {
                    let line = command.display();
                    if line.contains("--state-running") {
                        let stdout = if self.down { "" } else { "dev\n" };
                        return Ok(crate::hypervisor::Output {
                            success: true,
                            status: "exit status: 0".into(),
                            stdout: stdout.into(),
                            stderr: String::new(),
                        });
                    }
                    self.down |= line.contains(self.obeys);
                    self.ran.push(line);
                    Ok(crate::hypervisor::Output {
                        success: true,
                        status: "exit status: 0".into(),
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                }
                fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError> {
                    self.output(command).map(|o| o.success)
                }
            }
            let entry = VmEntry {
                hypervisor: "libvirt".into(),
                ..VmEntry::default()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            let graces = StopGraces {
                acpi: Duration::from_millis(30),
                guest: Duration::from_millis(30),
                hard: Duration::from_millis(30),
                poll: Duration::from_millis(1),
            };
            let poweroff = CommandLine::new("ssh", &["dev", "systemctl", "poweroff"]);
            let stop = |obeys: &'static str, poweroff: Option<&CommandLine>| {
                let mut guest = Guest {
                    obeys,
                    ran: Vec::new(),
                    down: false,
                };
                let result = stop_escalating(
                    &mut guest,
                    &hypervisor::Libvirt,
                    &vm,
                    poweroff,
                    None,
                    graces,
                )
                .unwrap();
                (result, guest.ran)
            };
            let acpi = "virsh --connect qemu:///session shutdown dev";
            let hard = "virsh --connect qemu:///session destroy dev";
            assert_eq!(
                stop("shutdown dev", Some(&poweroff)),
                ((Some(true), StopStep::Acpi), vec![acpi.to_owned()])
            );
            assert_eq!(
                stop("systemctl poweroff", Some(&poweroff)),
                (
                    (Some(true), StopStep::GuestPoweroff),
                    vec![acpi.to_owned(), poweroff.display()]
                )
            );
            assert_eq!(
                stop("destroy dev", Some(&poweroff)),
                (
                    (Some(true), StopStep::HardPowerOff),
                    vec![acpi.to_owned(), poweroff.display(), hard.to_owned()]
                )
            );
            assert_eq!(
                stop("destroy dev", None),
                (
                    (Some(true), StopStep::HardPowerOff),
                    vec![acpi.to_owned(), hard.to_owned()]
                )
            );
            assert_eq!(
                stop("nothing brings it down", Some(&poweroff)).0,
                (Some(false), StopStep::HardPowerOff)
            );
        }

        // Phase: image_assembled_components_refuse_a_worktree_build.
        {
            let manifest = Manifest::parse(
                "[[component]]\nname = \"apps+runtime\"\npackages = [\"runtime-dylib\"]\n\
                 features = [\"music/audio\"]\nlayer = \"direct\"\n\
                 [[component]]\nname = \"fs\"\npackages = [\"fs\"]\n",
            )
            .unwrap();
            let error = worktree_buildable(manifest.get("apps+runtime").unwrap()).unwrap_err();
            let CliError::Unsupported { what, alternative } = &error else {
                panic!("{error:?}");
            };
            assert!(what.contains("apps+runtime"), "{what}");
            assert!(alternative.contains("swap app:"), "{alternative}");
            assert!(alternative.contains("--from ci:"), "{alternative}");
            worktree_buildable(manifest.get("fs").unwrap()).unwrap();
        }

        // Phase: builder_choice_prefers_the_vm_and_explains_fallbacks.
        {
            let ready = VmTools {
                cargo: true,
                rsync: true,
                python3: true,
                sccache: true,
                podman: false,
            };
            let bare = VmTools::default();
            assert_eq!(
                choose_builder(None, &ready, true, "linux").unwrap(),
                (Builder::Vm, None)
            );
            let (builder, note) = choose_builder(None, &bare, true, "linux").unwrap();
            assert_eq!(builder, Builder::Host);
            assert!(note.unwrap().contains("--builder host"));
            assert!(matches!(
                choose_builder(None, &bare, false, "macos"),
                Err(CliError::VmCannotBuild { .. })
            ));
            let error = choose_builder(Some(Builder::Host), &ready, false, "macos").unwrap_err();
            assert!(matches!(error, CliError::HostBuildUnsupported { .. }));
            assert!(error.to_string().contains("macos"));
            assert!(error.hint().contains("--builder vm"));
            assert!(matches!(
                choose_builder(Some(Builder::Vm), &bare, true, "linux"),
                Err(CliError::VmCannotBuild { .. })
            ));
            assert_eq!(
                choose_builder(Some(Builder::Host), &bare, true, "linux").unwrap(),
                (Builder::Host, None)
            );
            assert!(describe_tools(&ready).starts_with("in the VM"));
        }

        // Phase: a_refused_key_names_the_install_step.
        {
            let dir = tempfile::tempdir().unwrap();
            let key = dir.path().join("keys/dev_ed25519");
            std::fs::create_dir_all(key.parent().unwrap()).unwrap();
            std::fs::write(
                sshkey::public_path(&key),
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample tilo-vm-dev\n",
            )
            .unwrap();
            let vm = Resolved {
                name: "dev".into(),
                entry: VmEntry {
                    hypervisor: "utm".into(),
                    key: Some(key.display().to_string()),
                    ..VmEntry::default()
                },
                source: Source::Argument,
            };
            let endpoint = SshEndpoint {
                user: "root".into(),
                host: "192.168.64.5".into(),
                port: 22,
            };
            let unreachable = |detail: &str| CliError::Unreachable {
                vm: "dev".into(),
                detail: detail.into(),
                debug: vec![],
            };
            let error = key_refusal(
                &vm,
                &hypervisor::Utm,
                &endpoint,
                unreachable("root@192.168.64.5: Permission denied (publickey)."),
            );
            assert!(
                matches!(
                    error,
                    CliError::RootKeyRefused {
                        credentials: false,
                        ..
                    }
                ),
                "{error:?}"
            );
            assert!(
                error.hint().contains("no credential channel"),
                "{}",
                error.hint()
            );
            assert!(
                error.hint().contains("quickuser@192.168.64.5"),
                "{}",
                error.hint()
            );
            assert_eq!(error.see().as_deref(), Some("tilo-vm guide platforms"));
            let libvirt = key_refusal(
                &vm,
                &hypervisor::Libvirt,
                &endpoint,
                unreachable("Permission denied (publickey)"),
            );
            assert!(
                libvirt.hint().contains("did not take it"),
                "{}",
                libvirt.hint()
            );
            // Anything else is still "unreachable".
            assert!(matches!(
                key_refusal(
                    &vm,
                    &hypervisor::Utm,
                    &endpoint,
                    unreachable("Connection refused")
                ),
                CliError::Unreachable { .. }
            ));
        }
    }

    #[test]
    fn json_errors_have_one_top_level_shape_everywhere() {
        if std::env::var_os("TILO_VM").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("vms.toml"),
            "default = \"probe\"\n[vms.probe]\nhypervisor = \"qemu\"\nssh = \"root@127.0.0.1:1\"\n",
        )
        .unwrap();
        let mut app = app_with_empty_registry(dir.path());
        let reports = [
            app.run(Some(Command::Status { slot: None })).unwrap(),
            app.run(Some(Command::Vm(VmCommand::Status { name: None })))
                .unwrap(),
        ];
        for report in reports {
            assert!(report.failed, "{}", report.text);
            for key in ["error", "hint", "see"] {
                assert!(
                    report.data[key].as_str().is_some_and(|v| !v.is_empty()),
                    "{key} is not a top-level string: {:#}",
                    report.data
                );
            }
            assert!(report.text.contains("hint: "), "{}", report.text);
            assert!(
                report.next.iter().all(|n| !n.ends_with("vm start")),
                "{:?}",
                report.next
            );
        }
        // The same keys as a failed command's own error.
        let failed = CliError::NoVm {
            registry: "r".into(),
        }
        .to_json();
        let mut merged = json!({ "vm": {} });
        merge_error(
            &mut merged,
            &CliError::NoVm {
                registry: "r".into(),
            },
        );
        for key in ["error", "hint", "see"] {
            assert_eq!(merged[key], failed[key]);
        }
    }

    fn app_with_empty_registry(dir: &Path) -> App {
        App {
            json: false,
            worktree: None,
            registry_path: dir.join("vms.toml"),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(dir, vec![]),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn first_contact_points_at_the_quickstart_and_both_ways_to_a_vm() {
        if std::env::var_os("TILO_VM").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with_empty_registry(dir.path());
        let status = app.run(None).unwrap();
        assert!(
            status.text.contains("tilo-vm guide quickstart"),
            "{}",
            status.text
        );
        assert!(status.text.contains("ask a human"), "{}", status.text);
        assert_eq!(status.next[0], "tilo-vm guide quickstart");
        assert!(status.next.contains(&"tilo-vm vm create <name>".to_owned()));

        let list = app.run(Some(Command::Vm(VmCommand::List))).unwrap();
        assert!(list.text.contains("no VMs registered yet"), "{}", list.text);
        assert!(
            list.text.contains("tilo-vm vm create <name>"),
            "{}",
            list.text
        );
        assert!(list.text.contains("ask a human"), "{}", list.text);
        assert!(list.next.contains(&"tilo-vm vm create <name>".to_owned()));

        let error = app
            .run(Some(Command::Logs {
                slot: Some("a".into()),
                slot_flag: None,
                lines: 1,
            }))
            .err()
            .unwrap();
        assert!(matches!(error, CliError::NoVm { .. }));
        assert!(error.render().contains("see: tilo-vm guide quickstart"));
    }

    /// Input arguments become validated actions and a direct-boot bundle resolves files and requires a
    /// root binding.
    #[test]
    fn inputs_and_boot_bundles_are_validated_before_use() {
        // Phase: input_arguments_become_validated_actions.
        {
            let click = input_actions(InputCommand::Click {
                button: "right".into(),
                at: Some(vec![4, 5]),
            })
            .unwrap();
            assert_eq!(
                click,
                [InputAction::Click {
                    button: Button::Right,
                    at: Some((4, 5))
                }]
            );
            assert_eq!(describe_input(&click[0]), "clicked right at 4,5");
            assert!(matches!(
                input_actions(InputCommand::Click {
                    button: "fourth".into(),
                    at: None
                }),
                Err(CliError::InvalidInput(_))
            ));
            let error = input_actions(InputCommand::Type {
                text: "naïve".into(),
            })
            .unwrap_err();
            assert!(error.to_string().contains('ï'));
            assert!(error.hint().contains("tilo-vm input key"));
        }

        // Phase: direct_boot_bundle_resolves_files_and_requires_root_binding.
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("Image"), b"kernel").unwrap();
            std::fs::write(dir.path().join("initrd"), b"initrd").unwrap();
            let bundle = dir.path().join("boot.json");
            std::fs::write(
                &bundle,
                r#"{"kernel":"Image","initrd":"initrd","cmdline":"root=tilo-auto composefs=abc","loader_partuuid":"FCDFC623-0563-4F91-9DB8-F2B87B98B3CE"}"#,
            )
            .unwrap();
            let direct = read_direct_boot_bundle(&bundle).unwrap();
            assert_eq!(
                direct.kernel,
                dir.path().join("Image").display().to_string()
            );
            assert_eq!(
                direct.initrd,
                dir.path().join("initrd").display().to_string()
            );
            assert_eq!(
                direct.loader_partuuid.as_deref(),
                Some("fcdfc623-0563-4f91-9db8-f2b87b98b3ce")
            );
            std::fs::write(
                &bundle,
                r#"{"kernel":"Image","initrd":"initrd","cmdline":"root=tilo-auto","loader_partuuid":"invalid"}"#,
            )
            .unwrap();
            assert!(read_direct_boot_bundle(&bundle).is_err());
        }
    }

    #[test]
    fn published_boot_json_cannot_redirect_to_an_unverified_kernel() {
        let dir = tempfile::tempdir().unwrap();
        let tag = "main-1234567";
        let arch = "aarch64";
        let kernel = format!("tilo-vm-{tag}-{arch}-direct-Image");
        let initrd = format!("tilo-vm-{tag}-{arch}-direct-initrd");
        std::fs::write(dir.path().join(&kernel), b"kernel").unwrap();
        std::fs::write(dir.path().join(&initrd), b"initrd").unwrap();
        std::fs::write(dir.path().join("other-Image"), b"other").unwrap();
        let path = dir
            .path()
            .join(format!("tilo-vm-{tag}-{arch}-direct-boot.json"));
        let json = |name: &str| {
            serde_json::json!({
                "kernel": name,
                "initrd": initrd,
                "cmdline": "root=tilo-auto console=ttyS0",
                "loader_partuuid": "fcdfc623-0563-4f91-9db8-f2b87b98b3ce",
            })
        };
        std::fs::write(&path, json(&kernel).to_string()).unwrap();
        assert!(read_published_direct_boot(&path, tag, arch).is_ok());
        std::fs::write(&path, json("other-Image").to_string()).unwrap();
        assert!(read_published_direct_boot(&path, tag, arch).is_err());

        let mut args = create_args("dev");
        args.direct_boot_bundle = Some(path.display().to_string());
        assert!(App::validate_create_options("whpx", &args).is_err());
        args.image = Some("local.qcow2".into());
        assert!(App::validate_create_options("whpx", &args).is_ok());
    }

    #[test]
    fn touch_pen_and_gamepad_commands_become_actions() {
        use clap::Parser as _;
        let parse = |args: &[&str]| {
            let cli = crate::cli::Cli::try_parse_from(args).unwrap();
            let Some(Command::Input { action, .. }) = cli.command else {
                panic!("{args:?} is not an input command");
            };
            input_actions(action)
        };
        let pinch = parse(&[
            "tilo-vm", "input", "touch", "pinch", "640", "400", "--from", "400", "--to", "100",
        ])
        .unwrap();
        assert_eq!(
            pinch,
            [InputAction::TouchPinch {
                center: (640, 400),
                from_distance: 400,
                to_distance: 100,
                duration_ms: 400,
            }]
        );
        assert_eq!(
            describe_input(&pinch[0]),
            "pinched at 640,400 from 400 to 100 px apart"
        );
        let stick = parse(&["tilo-vm", "input", "gamepad", "stick", "left", "0", "-100"]).unwrap();
        assert_eq!(
            stick,
            [InputAction::GamepadStick {
                stick: Stick::Left,
                x: 0,
                y: -100,
                duration_ms: 300,
            }]
        );
        let dpad = parse(&["tilo-vm", "input", "gamepad", "dpad", "down", "right"]).unwrap();
        assert_eq!(
            dpad,
            [InputAction::GamepadPress {
                combos: vec!["down".into(), "right".into()]
            }]
        );
        let bad = parse(&["tilo-vm", "input", "gamepad", "dpad", "a"]).unwrap_err();
        assert!(bad.to_string().contains("d-pad direction"), "{bad}");
        let bad = parse(&["tilo-vm", "input", "gamepad", "press", "guide+zz"]).unwrap_err();
        assert!(bad.to_string().contains("'zz'"), "{bad}");
        let pen = parse(&[
            "tilo-vm",
            "input",
            "pen",
            "tap",
            "1",
            "2",
            "--pressure",
            "0",
        ]);
        assert!(pen.unwrap_err().to_string().contains("pressure"));
    }

    fn create_args(name: &str) -> CreateArgs {
        CreateArgs {
            name: name.into(),
            hypervisor: None,
            image: None,
            boot_esp: None,
            direct_boot_bundle: None,
            tag: None,
            disk: None,
            arch: None,
            emulated: false,
            memory_mb: None,
            headless: false,
            heads: None,
            gl: false,
            resolution: None,
            cpus: None,
            ssh: None,
            ssh_port: None,
            template: None,
        }
    }

    /// Preflight names a missing hypervisor first, checks the image and VM disk, takes a raw disk and a
    /// boot ESP, and keeps raw QEMU to tilo worktrees; vm rm deletes what create made and keeps the image.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn preflight_checks_hypervisor_image_disks_and_worktree_rules() {
        // Phase: preflight_names_the_missing_hypervisor_before_anything_else.
        {
            use crate::host::HostKind;
            use crate::host::tests::linux;
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            // Only an Android SDK's qemu-img: refused, and nothing downloaded
            // (no --image, so the next step would have been the network).
            let facts = linux(
                HostKind::RpmOstree,
                &[("qemu-img", "/home/u/Android/Sdk/emulator/qemu-img")],
            );
            let error = app
                .preflight_create(&facts, &create_args("dev"), "x86_64")
                .err()
                .unwrap();
            assert!(matches!(error, CliError::NoHypervisor(_)), "{error}");
            assert!(error.to_string().contains("libvirt"), "{error}");
            assert!(
                error
                    .hint()
                    .contains("rpm-ostree install libvirt-daemon-kvm"),
                "{}",
                error.hint()
            );
            assert!(!dir.path().join("vms.toml").exists());
        }

        // Phase: preflight_checks_the_image_and_the_vm_disk.
        {
            use crate::host::HostKind;
            use crate::host::tests::{LIBVIRT, linux};
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            // The checks under test are the image and the disk path, not this
            // host's free space (a small /tmp would otherwise fail the floor).
            std::fs::write(dir.path().join("vms.toml"), "min_free_gb = 0\n").unwrap();
            let facts = linux(HostKind::Dnf, LIBVIRT);

            let mut args = create_args("dev");
            args.image = Some(dir.path().join("missing.qcow2").display().to_string());
            let error = app.preflight_create(&facts, &args, "x86_64").err().unwrap();
            assert!(matches!(error, CliError::ImageNotFound { .. }), "{error}");

            let image = dir.path().join("tilo-vm.qcow2");
            std::fs::write(&image, "").unwrap();
            args.image = Some(image.display().to_string());
            let error = app.preflight_create(&facts, &args, "x86_64").err().unwrap();
            assert!(matches!(error, CliError::ImageFormat { .. }), "{error}");
            std::fs::write(&image, b"QFI\xfb\0\0\0\x03").unwrap();
            let disk = dir.path().join("dev.qcow2");
            std::fs::write(&disk, "").unwrap();
            args.image = Some(image.display().to_string());
            args.disk = Some(disk.display().to_string());
            let error = app.preflight_create(&facts, &args, "x86_64").err().unwrap();
            assert!(matches!(error, CliError::DiskExists { .. }), "{error}");

            args.disk = Some(dir.path().join("vms/new.qcow2").display().to_string());
            let plan = app.preflight_create(&facts, &args, "x86_64").unwrap();
            assert_eq!(plan.choice.adapter, "libvirt");
            assert_eq!(plan.choice.tool("qemu-img"), Some("/usr/bin/qemu-img"));
            let text = plan.describe();
            assert!(text.contains("qemu:///session"), "{text}");
            assert!(text.contains("tilo-vm.qcow2"), "{text}");

            // An emulated aarch64 VM needs libvirt to list an aarch64 guest.
            args.arch = Some("aarch64".into());
            args.emulated = true;
            let error = app
                .preflight_create(&facts, &args, "aarch64")
                .err()
                .unwrap();
            assert!(matches!(error, CliError::NoHypervisor(_)), "{error}");
            assert!(error.to_string().contains("no aarch64 guest"), "{error}");
            assert!(
                error.hint().contains("qemu-system-aarch64"),
                "{}",
                error.hint()
            );
        }

        // Phase: preflight_takes_a_raw_disk_and_a_boot_esp.
        {
            use crate::host::HostKind;
            use crate::host::tests::{LIBVIRT, linux};
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            std::fs::write(dir.path().join("vms.toml"), "min_free_gb = 0\n").unwrap();
            let facts = linux(HostKind::Dnf, LIBVIRT);
            let image = dir.path().join("disk.raw");
            raw_disk(&image);
            let esp = dir.path().join("override.raw");
            let mut args = create_args("esp1");
            args.image = Some(image.display().to_string());
            args.disk = Some(dir.path().join("vms/esp1.qcow2").display().to_string());
            args.boot_esp = Some(esp.display().to_string());

            let error = app.preflight_create(&facts, &args, "x86_64").err().unwrap();
            assert!(
                matches!(&error, CliError::ImageFormat { flag, .. } if flag == "--boot-esp"),
                "{error}"
            );
            assert!(
                error.hint().contains("make-boot-esp.sh"),
                "{}",
                error.hint()
            );
            std::fs::write(&esp, b"QFI\xfb\0\0\0\x03").unwrap();
            let error = app.preflight_create(&facts, &args, "x86_64").err().unwrap();
            assert!(error.to_string().contains("is qcow2"), "{error}");

            raw_disk(&esp);
            let plan = app.preflight_create(&facts, &args, "x86_64").unwrap();
            assert_eq!(plan.image_format, Some(image::Format::Raw));
            assert!(plan.import.is_none(), "a local path is used in place");
            assert_eq!(plan.boot_esp.as_deref(), Some(esp.as_path()));
            let text = plan.describe();
            assert!(text.contains("(raw)"), "{text}");
            assert!(text.contains("boot ESP"), "{text}");
        }

        // Phase: preflight_keeps_raw_qemu_to_tilo_worktrees.
        {
            use crate::host::HostKind;
            use crate::host::tests::linux;
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            let facts = linux(
                HostKind::Dnf,
                &[
                    ("qemu-system-x86_64", "/usr/bin/qemu-system-x86_64"),
                    ("qemu-img", "/usr/bin/qemu-img"),
                    ("bash", "/usr/bin/bash"),
                ],
            );
            let mut args = create_args("ci");
            args.hypervisor = Some("qemu".into());
            let error = app.preflight_create(&facts, &args, "x86_64").err().unwrap();
            assert!(
                error.to_string().contains("outside a Tilo worktree"),
                "{error}"
            );
        }

        // Phase: vm_rm_deletes_what_create_made_and_keeps_the_image.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            let vms = dir.path().join("vms");
            std::fs::create_dir_all(&vms).unwrap();
            let disk = vms.join("try1.qcow2");
            let sidecars = hypervisor::vm_files(&disk.display().to_string());
            for file in &sidecars {
                std::fs::write(file, "x").unwrap();
            }
            let key = sshkey::key_path(&app.registry_path, "try1");
            std::fs::create_dir_all(key.parent().unwrap()).unwrap();
            for file in [
                key.clone(),
                sshkey::public_path(&key),
                PathBuf::from(remote::known_hosts_path(&key.display().to_string())),
            ] {
                std::fs::write(file, "k").unwrap();
            }
            let image = dir.path().join("base.raw");
            raw_disk(&image);
            let registry = format!(
                "default = \"try1\"\n[vms.try1]\nhypervisor = \"qemu\"\nimage = {image:?}\nimage_format = \"raw\"\ndisk = {disk:?}\nkey = {key:?}\n\n[vms.shared]\nhypervisor = \"libvirt\"\nssh = \"root@shared:22\"\nkey = \"/home/u/.ssh/id_ed25519\"\n",
                image = image.display().to_string(),
                disk = disk.display().to_string(),
                key = key.display().to_string(),
            );
            std::fs::write(&app.registry_path, registry).unwrap();

            let outcome = app
                .run(Some(Command::Vm(VmCommand::Rm {
                    name: "try1".into(),
                    force: false,
                })))
                .unwrap();
            assert!(outcome.text.contains("deleted"), "{}", outcome.text);
            assert!(outcome.text.contains("kept its image"), "{}", outcome.text);
            for file in &sidecars {
                assert!(!Path::new(file).exists(), "{file} survived");
            }
            assert!(!key.exists() && !sshkey::public_path(&key).exists());
            assert!(image.exists(), "the image is not the VM's to delete");
            let registry = app.registry().unwrap();
            assert!(!registry.vms.contains_key("try1"));
            assert_eq!(registry.default, None);

            // A hand-registered VM is only forgotten: no hypervisor command runs
            // (libvirt's would need virsh) and its key is the user's.
            let outcome = app
                .run(Some(Command::Vm(VmCommand::Rm {
                    name: "shared".into(),
                    force: false,
                })))
                .unwrap();
            assert!(outcome.text.contains("untouched"), "{}", outcome.text);
            assert!(app.registry().unwrap().vms.is_empty());

            let error = app
                .run(Some(Command::Vm(VmCommand::Rm {
                    name: "gone".into(),
                    force: false,
                })))
                .err()
                .unwrap();
            assert!(matches!(error, CliError::UnknownVm { .. }), "{error}");
        }
    }

    fn raw_disk(path: &Path) {
        let mut bytes = vec![0_u8; 1024];
        bytes[512..520].copy_from_slice(b"EFI PART");
        std::fs::write(path, bytes).unwrap();
    }

    fn slot_info(name: &str, kind: SlotKind, generation: u64, generations: &[u64]) -> SlotInfo {
        SlotInfo {
            name: name.into(),
            kind,
            owner: Owner {
                host: Some("laptop".into()),
                worktree: Some("/w/tilo".into()),
                branch: Some("ux-pass".into()),
            },
            user: None,
            generation,
            generations: generations.to_vec(),
            components: std::collections::BTreeMap::new(),
            session: "active".into(),
            seat: None,
            outputs: Vec::new(),
            parity: None,
        }
    }

    /// Hints follow a named slot; a missing slot shows the existing ones instead of create; slot lines name
    /// the owner; slot exec quotes for the remote shell; slot show lists files by component with digests.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn slots_are_named_hinted_and_shown_with_their_owners() {
        // Phase: hints_follow_a_named_slot.
        {
            assert_eq!(with_slot("tilo-vm shot", "ux1"), "tilo-vm shot --slot ux1");
            assert_eq!(
                with_slot("tilo-vm control state-dump --pretty", "ux1"),
                "tilo-vm control --slot ux1 state-dump --pretty"
            );
            assert_eq!(
                with_slot("tilo-vm swap <component>", "ux1"),
                "tilo-vm swap --slot ux1 <component>"
            );
            assert_eq!(
                with_slot("tilo-vm seat release", "ux1"),
                "tilo-vm seat release --slot ux1"
            );
            assert_eq!(
                with_slot("tilo-vm slot exec -- sha256sum <path>", "ux1"),
                "tilo-vm slot exec --slot ux1 -- sha256sum <path>"
            );
            for untouched in [
                "tilo-vm vm shot",
                "tilo-vm gc",
                "tilo-vm status --slot other",
                "tilo-vm slot list",
            ] {
                assert_eq!(with_slot(untouched, "ux1"), untouched);
            }
            let swap = Command::Swap {
                component: "shell".into(),
                slot: Some("ux1".into()),
                from: "worktree".into(),
                builder: None,
                profile: None,
                direct: false,
                jobs: None,
                cmdline: None,
            };
            assert_eq!(named_slot(&swap).as_deref(), Some("ux1"));
            assert_eq!(named_slot(&Command::Doctor), None);
        }

        // Phase: a_missing_slot_shows_the_existing_ones_instead_of_create.
        {
            let (lines, next) = missing_slot("mine", &[slot_info("vm", SlotKind::Vm, 0, &[0])]);
            assert!(lines[0].contains("not created yet"));
            assert_eq!(next[0], "tilo-vm slot create --headless");
            let (lines, next) = missing_slot(
                "mine",
                &[
                    slot_info("vm", SlotKind::Vm, 0, &[0]),
                    slot_info("ux1", SlotKind::Headless, 2, &[0, 1, 2]),
                ],
            );
            assert!(lines[0].contains("does not exist"), "{lines:?}");
            assert!(lines[1].contains("ux1") && lines[1].contains("owner laptop:ux-pass"));
            assert!(!next.iter().any(|n| n.contains("slot create")), "{next:?}");
            assert_eq!(next[0], "tilo-vm status --slot ux1");
        }

        // Phase: slot_lines_name_the_owner.
        {
            let line = slot_line(&slot_info("ux1", SlotKind::Headful, 1, &[0, 1]));
            assert!(line.ends_with("owner laptop:ux-pass"), "{line}");
            assert_eq!(describe_owner(&Owner::default()), None);
        }

        // Phase: slot_exec_quotes_the_command_for_the_remote_shell.
        {
            let words = exec_words(
                None,
                "a",
                60,
                &["sh".into(), "-c".into(), "ls /usr | head".into()],
            );
            assert_eq!(
                words.join(" "),
                "tilo-vmd exec --slot a --timeout 60 -- sh -c 'ls /usr | head'"
            );
            let sudo = exec_words(Some("sudo -n tilo-vmd rpc"), "a", 5, &["true".into()]);
            assert_eq!(sudo[..3], ["sudo", "-n", "tilo-vmd"]);
            assert_eq!(sudo[3], "exec");
        }

        // Phase: slot_show_lists_files_by_component_with_digests.
        {
            let shown = SlotShowResult {
                slot: slot_info("a", SlotKind::Headless, 2, &[0, 1, 2]),
                files: vec![
                    tilo_vm_protocol::SlotFile {
                        component: "shell".into(),
                        layer: "ab".repeat(32),
                        path: "/usr/bin/shell".into(),
                        sha256: Some("cd".repeat(32)),
                        link: None,
                        executable: true,
                    },
                    tilo_vm_protocol::SlotFile {
                        component: "shell".into(),
                        layer: "ab".repeat(32),
                        path: "/usr/bin/sh-alias".into(),
                        sha256: None,
                        link: Some("shell".into()),
                        executable: false,
                    },
                ],
            };
            let text = describe_show(&shown);
            assert!(text.contains("generations: 0, 1, 2 (current 2"), "{text}");
            assert!(text.contains("  shell (layer abababababab)"), "{text}");
            assert!(
                text.contains("/usr/bin/shell  sha256:cdcdcdcdcdcd"),
                "{text}"
            );
            assert!(text.contains("/usr/bin/sh-alias  -> shell"), "{text}");
        }
    }

    #[test]
    fn the_worktree_file_can_name_the_default_slot() {
        if std::env::var_os("TILO_VM_SLOT").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with_empty_registry(dir.path());
        app.worktree = Some(Worktree {
            root: dir.path().to_owned(),
            branch: Some("feature/x".into()),
        });
        assert_eq!(app.slot_name(None).unwrap(), app.default_slot().unwrap());
        let branch_slot = app.default_slot().unwrap();
        std::fs::write(
            dir.path().join(WORKTREE_FILE),
            "vm = \"dev\"\nslot = \"ux1\"\n",
        )
        .unwrap();
        assert_eq!(app.slot_name(None).unwrap(), "ux1");
        assert_ne!(branch_slot, "ux1");
        assert_eq!(app.slot_name(Some("other".into())).unwrap(), "other");
    }

    /// Rolling forward goes to the next newer generation; gc names what it kept and when it goes; bytes
    /// read well.
    #[test]
    fn generations_roll_forward_and_gc_explains_itself() {
        // Phase: rolling_forward_goes_to_the_next_newer_generation.
        {
            let info = slot_info("a", SlotKind::Headless, 1, &[0, 1, 2, 4]);
            assert_eq!(forward_target("a", Some(&info)).unwrap(), 2);
            let newest = slot_info("a", SlotKind::Headless, 4, &[0, 1, 2, 4]);
            let error = forward_target("a", Some(&newest)).unwrap_err();
            assert!(error.to_string().contains("newest"), "{error}");
            assert!(error.hint().contains("--to <n>"), "{}", error.hint());
            assert!(forward_target("a", None).is_err());
        }

        // Phase: gc_names_what_it_kept_and_when_it_goes.
        {
            let mut result = tilo_vm_protocol::GcResult {
                dry_run: true,
                removed_layers: vec![],
                removed_generations: vec![],
                freed_bytes: 0,
                removed_targets: vec![],
                freed_target_bytes: 0,
                storage: tilo_vm_protocol::Storage::default(),
                kept_young: vec![],
                min_age_seconds: 3600,
            };
            assert_eq!(describe_kept(&result), None);
            result.kept_young.push(tilo_vm_protocol::KeptLayer {
                hash: "ef".repeat(32),
                component: "shell".into(),
                age_seconds: 600,
                eligible_in_seconds: 3000,
            });
            let text = describe_kept(&result).unwrap();
            assert!(text.contains("younger than 1h00m"), "{text}");
            assert!(
                text.contains("shell efefefefefef (age 10m, eligible in 50m)"),
                "{text}"
            );
            assert!(text.contains("--min-age 0"), "{text}");
        }

        // Phase: bytes_read_well.
        {
            assert_eq!(human_bytes(5 << 30), "5.0 GB");
            assert_eq!(human_bytes(300 << 20), "300 MB");
        }
    }
}
