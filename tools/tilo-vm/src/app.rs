//! Command implementations. Each returns an [`Outcome`]: what happened, as
//! text and as JSON, plus the likely next commands.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tilo_device_protocol::image::{
    BootFacts, BootcStatus, Deployment, ImageSource, Reboot, Upgrade, UpgradeResult, short_digest,
};
use tilo_device_protocol::manifest::{Component, Manifest};
use tilo_device_protocol::{
    AccountCreate, AccountInfo, AccountKind, AccountList, AccountRestart, AccountRm, AccountShow,
    AccountShowResult, Action, AppInstall, AppInstallResult, ApplyResult, Build, BuildTarget,
    Button, CiLayer, Control, Gc, Input, InputAction, JobOutput, LayerImport, LayerList,
    LayerSource, Logs, Owner, Policy, Rollback, Scope, SeatClaim, SeatRelease, Shot, Status, Stick,
    Swap, VM_ACCOUNT, VmTools,
};

use crate::appsets;
use crate::build;
use crate::ci;
use crate::cicache;
use crate::cli::{
    AccountAppsCommand, AccountCommand, Builder, Command, CreateArgs, DeviceCommand, FlightCommand,
    GamepadCommand, ImageBuildArgs, ImageCommand, InputCommand, PenCommand, QemuCommand,
    ScenarioCommand, SeatCommand, TouchCommand, TouchpadCommand, UpgradeArgs, VmCommand,
};
use crate::config::{self, DirectKernel, Registry, Source, VmEntry, WORKTREE_FILE};
use crate::context::Worktree;
use crate::disk;
use crate::doctor::{self, Check};
use crate::error::CliError;
use crate::guide;
use crate::host::{self, HostFacts};
use crate::hypervisor::{self, CommandLine, Hypervisor, Runner, SystemRunner, Vm, run_all};
use crate::image;
use crate::imageup;
use crate::receipt::Receipt;
use crate::remote::{Machine, Remote};
use crate::sshguard;
use crate::sshkey;
use crate::vmbuild;

use space::{host_space, measured, room};

mod artifact;
mod branch;
mod branch_admit;
mod builder;
mod device;
mod flows;
#[cfg(test)]
mod forwarded_port_tests;
mod joining;
mod kernel;
mod local_layer;
mod reach;
mod scenario;
mod space;
mod userspace;

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

fn windows_args() -> &'static [String] {
    static ARGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    ARGS.get_or_init(|| {
        vec![
            "raw".into(),
            serde_json::json!({ "op": "windows" }).to_string(),
        ]
    })
}

/// The action frame that opens `app` as the desktop's own open-app does, answered once it is
/// admitted: what a person's request to open an app sends the session.
fn open_app_frame(app: &str) -> String {
    json!({
        "version": 2,
        "message": {"Invoke": {
            "invocation": uuid::Uuid::new_v4().to_string(),
            "target": {
                "provider": "so.tilo.session",
                "address": {"resource": "desktop:chrome", "action": "open-app"},
            },
            "argument": {"Json": {"app": app}},
        }},
    })
    .to_string()
}

fn control_json(
    remote: &mut Remote,
    account: &str,
    args: &[String],
    worst: &mut Duration,
) -> Result<Value, CliError> {
    let started = Instant::now();
    let result = remote.call(Control {
        account: account.to_owned(),
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

fn window_rows(value: &Value) -> impl Iterator<Item = &Value> {
    value
        .get("answer")
        .and_then(|answer| answer.get("windows"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn window_ids(value: &Value) -> std::collections::HashSet<u64> {
    window_rows(value)
        .filter_map(|row| row.get("window").and_then(Value::as_u64))
        .collect()
}

fn parse_window(value: &Value) -> Option<ForeignFrame> {
    let rect = value.get("rect").and_then(Value::as_array).and_then(|at| {
        Some(ForeignRect {
            x: at.first()?.as_f64()?,
            y: at.get(1)?.as_f64()?,
            w: at.get(2)?.as_f64()?,
            h: at.get(3)?.as_f64()?,
        })
    });
    Some(ForeignFrame {
        surface_id: value.get("window")?.as_u64()?,
        frame_seq: value.get("frames")?.as_u64()?,
        rect,
    })
}

fn matching_presented_window(
    value: &Value,
    app_id: &str,
    baseline_ids: &std::collections::HashSet<u64>,
) -> Option<ForeignFrame> {
    window_rows(value).find_map(|row| {
        let parsed = parse_window(row)?;
        (row.get("app_id").and_then(Value::as_str) == Some(app_id)
            && parsed.rect.is_some()
            && parsed.frame_seq > 0
            && !baseline_ids.contains(&parsed.surface_id))
        .then_some(parsed)
    })
}

fn matching_window(value: &Value, surface_id: u64) -> Option<ForeignFrame> {
    window_rows(value).find_map(|row| {
        let parsed = parse_window(row)?;
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

    fn windows(rows: &Value) -> Value {
        json!({ "answer": { "windows": rows } })
    }

    tilo_test_env::scenario! {
        fn flight_windows_are_found_and_followed;

        phase presented_flight_window_must_be_new_matching_and_mapped;
        {
            let baseline = windows(&json!([
                { "window": 4, "app_id": "chatgpt", "frames": 8, "rect": [0, 0, 100, 100] }
            ]));
            let ids = window_ids(&baseline);
            let current = windows(&json!([
                { "window": 4, "app_id": "chatgpt", "frames": 9, "rect": [0, 0, 100, 100] },
                { "window": 5, "app_id": "other", "frames": 1, "rect": [0, 0, 100, 100] },
                { "window": 6, "app_id": "chatgpt", "frames": 0, "rect": null },
                {
                    "window": 7,
                    "app_id": "chatgpt",
                    "frames": 2,
                    "rect": [10, 20, 300, 200]
                }
            ]));

            let selected = matching_presented_window(&current, "chatgpt", &ids).expect("new window");
            assert_eq!(selected.surface_id, 7);
            assert_eq!(selected.frame_seq, 2);
            let rect = selected.rect.expect("placement");
            assert_eq!((rect.center_x(), rect.center_y()), (160.0, 120.0));
        }

        phase window_lookup_tracks_the_same_window_after_input;
        {
            let value = windows(&json!([
                { "window": 12, "app_id": "chatgpt", "frames": 41, "rect": [0, 0, 10, 10] },
                { "window": 13, "app_id": "chatgpt", "frames": 99, "rect": [0, 0, 10, 10] }
            ]));

            assert_eq!(matching_window(&value, 12).expect("window").frame_seq, 41);
            assert!(matching_window(&value, 14).is_none());
        }
    }
}

pub struct Outcome {
    pub text: String,
    pub data: Value,
    pub next: Vec<String>,
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

fn qemu_command(command: &QemuCommand) -> Result<Outcome, CliError> {
    match command {
        QemuCommand::Boot {
            machine,
            start_timeout,
        } => crate::qemu::boot_command(machine, *start_timeout),
        QemuCommand::Run { machine, wait } => crate::qemu::run_command(machine, wait),
        QemuCommand::Firmware { machine } => crate::qemu::firmware_command(machine),
        QemuCommand::Args { machine } => crate::qemu::args_command(machine),
        QemuCommand::Stop { run, tpm_dir } => {
            Ok(crate::qemu::stop_command(run, tpm_dir.as_deref()))
        }
        QemuCommand::Status { run } => Ok(crate::qemu::status_command(run)),
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

struct Published {
    tag: String,
    disk: disk::Disk,
    dest: PathBuf,
    direct_boot: Option<disk::DirectBoot>,
    source_revision: Option<String>,
    auth: disk::AuthHeader,
}

struct Plan {
    choice: host::Choice,
    ssh_port: Option<u16>,
    image: Option<PathBuf>,
    image_format: Option<image::Format>,
    import: Option<PathBuf>,
    boot_esp: Option<PathBuf>,
    published: Option<Published>,
    disk_path: Option<PathBuf>,
}

impl Plan {
    fn describe(&self) -> String {
        let mut lines = vec![format!("  hypervisor: {}", self.choice.describe())];
        if let Some(port) = self.ssh_port
            && matches!(self.choice.adapter, "libvirt" | "qemu" | "whpx")
        {
            lines.push(format!(
                "  SSH forwarding: host TCP port {port} (availability checked, not reserved)"
            ));
        }
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
    /// `--local`: this machine's own device, reached on loopback like any other, with no dev-VM
    /// registry.
    pub local: bool,
    pub target: Option<String>,
    pub worktree: Option<Worktree>,
    pub registry_path: PathBuf,
    pub runner: SystemRunner,
    pub receipt: Receipt,
    pub warnings: Vec<String>,
    /// Who devices are asked as; `None` reads this machine's login when a device is first asked.
    pub machine: Option<Machine>,
}

#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    tilo_formats::ByteFormat::binary()
        .min_unit(tilo_formats::ByteUnit::Mega)
        .decimal_from(tilo_formats::ByteUnit::Giga)
        .format(bytes)
}

fn insert(value: &mut Value, key: &str, item: Value) {
    if let Value::Object(map) = value {
        map.insert(key.into(), item);
    }
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn describe_login(result: &ApplyResult, desktops: &[&str], text: &mut String) {
    text.push_str("; merged into the machine's /usr with systemd-sysext");
    let services: Vec<&str> = result
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::RestartUserUnit { unit, .. } => Some(unit.as_str()),
            _ => None,
        })
        .collect();
    if !services.is_empty() {
        let _ = write!(text, "; restarted {}", services.join(", "));
    }
    for user in desktops {
        let _ = write!(
            text,
            "; {user}'s desktop restarted in place (still logged in; apps reopen)"
        );
    }
    for action in &result.actions {
        if let Action::Deferred { reason } = action {
            let _ = write!(text, "; {reason}");
        }
    }
}

pub fn describe_apply(result: &ApplyResult) -> String {
    if !result.changed {
        return format!(
            "account {} is already at generation {}; nothing to do",
            result.account, result.generation
        );
    }
    let mut text = format!(
        "generation {} applied to account {}",
        result.generation, result.account
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
    let desktops: Vec<&str> = result
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::RestartDesktop { user } => Some(user.as_str()),
            _ => None,
        })
        .collect();
    let login = !desktops.is_empty()
        || (result.account == tilo_device_protocol::DEVICE_ACCOUNT
            && result.actions.contains(&Action::SysextRefresh));
    match result.policy {
        Policy::SessionRestart if login => describe_login(result, &desktops, &mut text),
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
                    "; restarted the sessions of every account so they see it: {}",
                    restarted_sessions.join(", ")
                );
            }
            for user in &desktops {
                let _ = write!(
                    text,
                    "; {user}'s desktop restarted in place (still logged in)"
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

pub const REGISTRY: &str = "registry.tilo.so";

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

    pub fn resolve_vm(&self, explicit: Option<&str>) -> Result<Resolved, CliError> {
        if let (Some(target), Some(name)) = (self.target.as_deref(), explicit)
            && target != name
        {
            return Err(CliError::TargetConflict {
                target: target.to_owned(),
                name: name.to_owned(),
            });
        }
        let registry = self.registry()?;
        let (name, source) = if let Some(name) = explicit.or(self.target.as_deref()) {
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
        self.connect_with_wake(vm, false)
    }

    fn connect_for_swap(&mut self, vm: &Resolved) -> Result<Remote, CliError> {
        self.connect_with_wake(vm, true)
    }

    /// This machine's own daemon, for `--local`: asked through its native endpoint like any
    /// other device, as this machine's login and under the same grants, with no dev-VM registry
    /// and no hypervisor.
    pub(crate) fn connect_here(&mut self) -> Result<Remote, CliError> {
        let machine = self.machine()?;
        let route = config::DeviceRoute {
            device: hex::encode(machine.device),
            workspace: machine.workspace.clone(),
            addresses: Vec::new(),
            relay: None,
        };
        let remote = Remote::connect("this machine", &machine, &route)?;
        self.note_issues(&remote);
        self.receipt.set("route", json!({ "local": true }));
        Ok(remote)
    }

    /// The identity this machine drives its devices as: the person's own `tilo login`.
    pub(crate) fn machine(&self) -> Result<Machine, CliError> {
        if let Some(machine) = &self.machine {
            return Ok(machine.clone());
        }
        Machine::here()?.ok_or_else(|| CliError::NotLoggedIn {
            detail: "this computer has no Tilo login".into(),
        })
    }

    fn note_issues(&mut self, remote: &Remote) {
        for issue in &remote.status.issues {
            self.warnings.push(format!(
                "account {}: {}; {}",
                issue.account, issue.problem, issue.hint
            ));
        }
    }

    /// `--local swap <component> --from layer:<hash>`: put a layer this machine
    /// already imported into one of its accounts. Building, CI sources and boot
    /// components need the development routes.
    fn swap_local(
        &mut self,
        account: Option<String>,
        component: &str,
        from: &str,
    ) -> Result<Outcome, CliError> {
        let Some(layer) = from.strip_prefix("layer:") else {
            return Err(CliError::InvalidInput(format!(
                "--local applies a layer this machine already imported; pass --from layer:<hash>, not '{from}'"
            )));
        };
        if layer.len() != 64
            || !layer
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(CliError::InvalidInput(
                "--from layer: takes a lowercase SHA-256 layer hash".into(),
            ));
        }
        if component.starts_with("app:") {
            return Err(CliError::Unsupported {
                what: "--local with an app".into(),
                alternative: "install apps through the app store; --local swaps components".into(),
            });
        }
        let account = account.ok_or_else(|| {
            CliError::InvalidInput(
                "--local swaps into an account this machine runs; name it, e.g. --account vm or --account device"
                    .into(),
            )
        })?;
        self.receipt.set("account", &account);
        self.receipt.set("component", component);
        self.receipt.set("layer", layer);
        let mut remote = self.connect_here()?;
        let result = self.receipt.phase("apply", || {
            remote.call(Swap {
                account: account.clone(),
                component: component.to_owned(),
                layer: layer.to_owned(),
                info: None,
                partner: None,
            })
        })?;
        self.warnings.extend(result.warnings.iter().cloned());
        Ok(Outcome::new(
            format!("{} ({component} {})", describe_apply(&result), short(layer)),
            json!(result),
        ))
    }

    fn prepare_swap_connection(&mut self, vm: &Resolved) -> Result<Remote, CliError> {
        let mut remote = self.connect_with_wake(vm, true)?;
        self.check_vm_space(&mut remote, vm)?;
        Ok(remote)
    }

    fn connect_with_wake(&mut self, vm: &Resolved, wake: bool) -> Result<Remote, CliError> {
        let mut route = vm.entry.device.clone().ok_or_else(|| CliError::NotJoined {
            vm: vm.name.clone(),
        })?;
        for address in self.daemon_addresses(vm) {
            if !route.addresses.contains(&address) {
                route.addresses.push(address);
            }
        }
        let machine = self.machine()?;
        let adapter = Self::adapter(vm)?;
        let transport = || Remote::connect(&vm.name, &machine, &route);
        let connected = if wake {
            swap_transport(adapter.as_ref(), &vm.vm(), &mut self.runner, transport)
        } else {
            transport()
        };
        let remote = connected.map_err(|error| match error {
            CliError::Unreachable { detail, .. } if adapter.name() == hypervisor::DEVICE => {
                CliError::DeviceAsleep {
                    vm: vm.name.clone(),
                    address: route.addresses.join(", "),
                    detail,
                    lookup: match &vm.entry.hostname {
                        Some(hostname) => {
                            format!(", nor as {}", reach::local_name(hostname))
                        }
                        None => ", and no hostname is recorded to look it up by".into(),
                    },
                    wakeable: vm.entry.mac.is_some(),
                }
            }
            CliError::Unreachable { detail, .. } => CliError::Unreachable {
                vm: vm.name.clone(),
                detail,
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            },
            other => other,
        })?;
        self.note_issues(&remote);
        self.receipt.set("vm", &vm.name);
        Ok(remote)
    }

    fn connect_failure(
        &mut self,
        vm: &Resolved,
        adapter: &dyn Hypervisor,
        error: &CliError,
    ) -> ConnectFailure {
        if adapter.suspended(&vm.vm(), &mut self.runner) == Some(true) {
            return ConnectFailure {
                state: "asleep",
                summary: "suspended (the hypervisor reports it is asleep)".into(),
                ssh: false,
                next: vec![format!("tilo-vm vm wake {}", vm.name)],
            };
        }
        let running = adapter.running(&vm.vm(), &mut self.runner);
        let debug = Self::debug_commands(adapter, &vm.name);
        connect_failure(&vm.name, running, &debug, error)
    }

    pub fn default_account(&self) -> Option<String> {
        if let Ok(name) = std::env::var("TILO_VM_ACCOUNT")
            && !name.trim().is_empty()
        {
            return Some(name.trim().to_owned());
        }
        let worktree = self.worktree.as_ref()?;
        std::fs::read_to_string(worktree.root.join(WORKTREE_FILE))
            .ok()
            .and_then(|text| config::parse_worktree_account(&text))
            .or_else(|| worktree.default_account())
    }

    pub fn account_name(&self, explicit: Option<String>) -> Result<String, CliError> {
        if let Some(name) = explicit.or_else(|| self.default_account()) {
            return Ok(name);
        }
        let worktree = self.worktree.as_ref().ok_or(CliError::NotInWorktree)?;
        worktree
            .default_account()
            .ok_or_else(|| CliError::AccountName {
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

    pub fn run(&mut self, command: Option<Command>) -> Result<Outcome, CliError> {
        let named = command.as_ref().and_then(named_account);
        let mut outcome = self.dispatch(command)?;
        if let Some(account) = named
            && self.default_account().as_deref() != Some(account.as_str())
        {
            outcome.next = outcome
                .next
                .iter()
                .map(|n| with_account(n, &account))
                .collect();
        }
        Ok(outcome)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per command, each a couple of lines"
    )]
    fn dispatch(&mut self, command: Option<Command>) -> Result<Outcome, CliError> {
        if self.local
            && !matches!(
                command.as_ref(),
                Some(Command::ImportLayer(_) | Command::Swap { .. })
            )
        {
            return Err(CliError::Unsupported {
                what: "--local here".into(),
                alternative: "use it with import-layer, or swap --from layer:<hash>".into(),
            });
        }
        match command {
            None => self.status(None),
            Some(Command::Status { account }) => self.status(account),
            Some(Command::Doctor) => Ok(self.doctor()),
            Some(Command::Guide { topic }) => Self::guide(topic.as_deref()),
            Some(Command::Vm(command)) => self.vm(command),
            Some(Command::Account(AccountCommand::Apps(AccountAppsCommand::List))) => {
                Ok(apps_list())
            }
            Some(Command::Account(command)) => self.account(command),
            Some(Command::SwapArtifact(request)) => self.swap_artifact(&request),
            Some(Command::ImportLayer(request)) => self.import_local_layer(&request),
            Some(Command::Swap {
                component,
                account,
                from,
                builder,
                profile,
                direct,
                jobs,
                cmdline,
                frame_cpu_trace,
            }) => self.swap(
                account,
                component,
                &from,
                builder,
                profile.as_deref(),
                frame_cpu_trace,
                KernelFlags {
                    direct,
                    jobs,
                    cmdline,
                },
            ),
            Some(Command::Input { account, action }) => self.input(account, action),
            Some(Command::Rollback {
                account,
                to,
                forward,
                ..
            }) => {
                let account = self.account_name(account)?;
                let vm = self.resolve_vm(None)?;
                let to = if forward {
                    let info = self
                        .connect(&vm)?
                        .call(Status {
                            account: Some(account.clone()),
                        })?
                        .accounts
                        .into_iter()
                        .next();
                    Some(forward_target(&account, info.as_ref())?)
                } else {
                    to
                };
                if account == VM_ACCOUNT {
                    return self.rollback_vm(to);
                }
                let mut remote = self.connect(&vm)?;
                let result = self.receipt.phase("apply", || {
                    remote.call(Rollback {
                        account: account.clone(),
                        to,
                    })
                })?;
                Ok(Outcome::new(describe_apply(&result), json!(result)).next(&[
                    "tilo-vm shot",
                    "tilo-vm logs",
                    "tilo-vm status",
                ]))
            }
            Some(Command::Logs { account, lines, .. }) => {
                let account = self.account_name(account)?;
                let vm = self.resolve_vm(None)?;
                let result = self.connect(&vm)?.call(Logs { account, lines })?;
                Ok(Outcome::new(result.text.clone(), json!(result)))
            }
            Some(Command::Seat(command)) => self.seat(command),
            Some(Command::Shot {
                account, output, ..
            }) => self.shot(account, &output),
            Some(Command::Control { account, args }) => self.control(account, &args),
            Some(Command::Flight(command)) => self.flight(command),
            Some(Command::Scenario(ScenarioCommand::Install(args))) => self.scenario_install(&args),
            Some(Command::Scenario(ScenarioCommand::FourDevices(args))) => {
                self.scenario_four_devices(&args)
            }
            Some(Command::Scenario(ScenarioCommand::LaptopPhoneServer(args))) => {
                self.scenario_laptop_phone_server(&args)
            }
            Some(command @ (Command::LayerPack { .. } | Command::AppPack { .. })) => {
                Self::pack_command(command)
            }
            Some(Command::Image(ImageCommand::Build(args))) => self.image_build(&args),
            Some(Command::Qemu(command)) => qemu_command(&command),
            Some(Command::Gc {
                dry_run,
                keep,
                keep_targets,
                min_age,
            }) => self.gc(dry_run, keep, keep_targets, min_age),
        }
    }

    fn pack_command(command: Command) -> Result<Outcome, CliError> {
        match command {
            Command::LayerPack {
                component,
                profile,
                features,
                source_inventory,
                cached_layer,
                incoming,
            } => Self::packed(crate::builderlayer::pack_with_source(
                &component,
                &profile,
                &features,
                source_inventory.as_deref(),
                &cached_layer,
                incoming.as_deref(),
            )),
            Command::AppPack {
                app,
                source_inventory,
            } => Self::packed(crate::builderapp::pack(&app, &source_inventory)),
            _ => Err(CliError::InvalidInput("not a remote pack command".into())),
        }
    }

    fn packed(result: Result<(), CliError>) -> Result<Outcome, CliError> {
        result?;
        Ok(Outcome::new("", json!({})))
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

    #[expect(
        clippy::too_many_lines,
        reason = "one linear report, section by section"
    )]
    fn status(&mut self, account: Option<String>) -> Result<Outcome, CliError> {
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
        let account = self.account_name(account).ok();
        let mut lines = vec![format!(
            "VM: {} ({}, {})",
            vm.name,
            vm.entry.hypervisor,
            vm.source.describe()
        )];
        let mut vm_data =
            json!({ "name": vm.name, "hypervisor": vm.entry.hypervisor, "chosen": vm.source });
        let remote = match self.connect(&vm) {
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
        let status = remote.status;
        let hello = &status;
        lines.push(format!(
            "  device steps {}, {}",
            hello.daemon_version, hello.arch
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
        if let Some(change) = &status.image_change {
            lines.extend(
                change
                    .describe()
                    .into_iter()
                    .map(|line| format!("  {line}")),
            );
            insert(&mut vm_data, "image_change", json!(change));
        }
        let ci_from_host = Self::adapter(&vm)?.ci_layers_from_host();
        lines.push(format!(
            "  builds: {}",
            describe_tools(&hello.tools, ci_from_host)
        ));
        if let Some(lease) = &status.seat0 {
            lines.push(format!(
                "  seat0: held by account {} since {}",
                lease.account, lease.since
            ));
        }
        let mine = account
            .as_ref()
            .and_then(|name| status.accounts.iter().find(|s| &s.name == name));
        let mut next: Vec<String> = Vec::new();
        match (&account, mine) {
            (Some(_), Some(info)) => {
                lines.push(account_line(info));
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
                let (text, hints) = missing_account(name, &status.accounts);
                lines.extend(text);
                next.extend(hints);
            }
            (None, _) => {
                lines.push("account: none (not in a worktree; pass an account name)".into());
                next.push("tilo-vm account list".into());
                next.push("tilo-vm guide quickstart".into());
            }
        }
        for issue in &status.issues {
            lines.push(format!(
                "  ! account {}: {} -> {}",
                issue.account, issue.problem, issue.hint
            ));
        }
        self.warnings.clear();
        lines.push(goals.into());
        insert(&mut vm_data, "state", json!("ready"));
        insert(&mut vm_data, "reachable", json!(true));
        let data = json!({
            "vm": vm_data,
            "daemon": hello,
            "account": mine,
            "account_name": account,
            "status": status,
        });
        let mut outcome = Outcome::new(lines.join("\n"), data);
        outcome.next = next;
        Ok(outcome)
    }

    fn doctor(&mut self) -> Outcome {
        let facts = HostFacts::probe(&mut self.runner);
        let mut checks = doctor::host_checks(&facts);
        checks.push(match self.machine() {
            Ok(machine) => Check::ok(
                "login",
                format!(
                    "asks devices as {} in workspace {}",
                    hex::encode(machine.device),
                    machine.workspace,
                ),
            ),
            Err(error) => Check::warn("login", error.to_string(), error.hint()),
        });
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
        checks.push(doctor::ownership_check(
            &doctor::root_owned_build_dirs(&build_dirs(&root), 3),
            &std::env::var("USER").unwrap_or_else(|_| "$USER".into()),
        ));
        checks.push(doctor::contended_build_check(
            &doctor::contended_build_dirs(&root.join("target"), &build_dirs(&root)),
        ));
        let listing = disk::AuthHeader::new(token.as_ref())
            .and_then(|auth| disk::get(&mut self.runner, disk::VERSIONS, &auth));
        checks.push(doctor::disk_check(listing, token.as_ref()));
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
            host::evaluate_vm(&facts, &vm.entry.hypervisor, vm.entry.emulated),
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

    fn vm_checks(&mut self, remote: &mut Remote, vm: &Resolved, checks: &mut Vec<Check>) {
        let hello = remote.status.clone();
        checks.push(Check::ok(
            "daemon",
            format!("the device answers its steps ({})", hello.daemon_version),
        ));
        let tools = &hello.tools;
        let builds = describe_tools(
            tools,
            Self::adapter(vm).is_ok_and(|adapter| adapter.ci_layers_from_host()),
        );
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
        let Ok(status) = remote.call(Status { account: None }) else {
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
        for account in status.accounts.iter().filter(|s| s.user.is_some()) {
            match (&account.parity, account.session.as_str()) {
                (Some(problem), _) => checks.push(Check::fail(
                    "session parity",
                    format!("account {}: {problem}", account.name),
                    "account session capabilities or SELinux domain differ from a greetd session; see tilo-vm guide troubleshooting",
                )),
                (None, "active") => checks.push(Check::ok(
                    "session parity",
                    format!(
                        "account {} matches a greetd session (no capabilities, user domain)",
                        account.name
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
                format!("account {}: {}", issue.account, issue.problem),
                issue.hint,
            ));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per VM command, each a report of its own"
    )]
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
            VmCommand::Branch(args) => self.vm_branch(&args),
            VmCommand::Add(args) => self.vm_add(&args),
            VmCommand::Start { name } => self.vm_start(name.as_deref()),
            VmCommand::Wake { name } => self.vm_wake(name.as_deref()),
            VmCommand::NetworkWake {
                name,
                profile,
                enabled,
            } => self.vm_network_wake(name.as_deref(), &profile, enabled),
            VmCommand::Awake { name, hold, off } => self.vm_awake(name.as_deref(), hold, off),
            VmCommand::Restart { name } => self.vm_restart(name.as_deref()),
            VmCommand::Stop { name, force } => self.vm_stop(name.as_deref(), force),
            VmCommand::Rm { name, force } => {
                let mut runner = SystemRunner {
                    cwd: self.runner.cwd.clone(),
                };
                self.vm_rm(&mut runner, &name, force)
            }
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
            VmCommand::Sound { name, window } => {
                let vm = self.resolve_vm(name.as_deref())?;
                if !vm.entry.sound {
                    return Err(CliError::Unsupported {
                        what: format!("measuring {}'s sound: it has no sound card", vm.name),
                        alternative:
                            "create a VM with one: tilo-vm vm create <name> --sound (libvirt or qemu)"
                                .into(),
                    });
                }
                let disk = vm.entry.disk.as_deref().unwrap_or_default();
                let path = std::path::PathBuf::from(crate::sound::wav_path(disk));
                let measured = crate::sound::measure_file(&vm.name, &path, window)?;
                Ok(Outcome::new(
                    crate::sound::describe(&path, &measured),
                    json!({ "vm": vm.name, "path": path, "sound": measured }),
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
                    format!(
                        "VM {} reset; every account session restarts with it",
                        vm.name
                    ),
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

    fn vm_restart(&mut self, name: Option<&str>) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        if !Self::adapter(&vm)?.powers_on() {
            return self.vm_reboot(&vm);
        }
        let stopped = self.vm_stop(name, false)?;
        if stopped.data.get("down") == Some(&json!(false)) {
            return Ok(stopped);
        }
        let mut started = self.vm_start(name)?;
        started.text = format!("{}\n{}", stopped.text, started.text);
        insert(&mut started.data, "stop", stopped.data);
        Ok(started)
    }

    fn vm_stop(&mut self, name: Option<&str>, force: bool) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let adapter = Self::adapter(&vm)?;
        // The device's poweroff takes its VM lock and keeps it until it is down, so a stop never
        // cuts a swap, gc or upgrade short; --force skips it.
        let asked = !force
            && match self
                .connect(&vm)
                .and_then(|mut remote| remote.call(tilo_device_protocol::image::Poweroff {}))
            {
                Ok(_) => true,
                Err(CliError::Remote(error))
                    if error.code == tilo_device_protocol::code::CONFLICT =>
                {
                    return Err(CliError::Busy {
                        what: format!("VM {}", vm.name),
                    });
                }
                Err(error) => {
                    self.warnings.push(format!(
                        "could not ask {} to power off ({error}); stopping it through the hypervisor",
                        vm.name
                    ));
                    false
                }
            };
        let (down, step) = stop_escalating(
            &mut self.runner,
            adapter.as_ref(),
            &vm.vm(),
            asked,
            StopGraces::default(),
        )?;
        match step {
            StopStep::Acpi | StopStep::GuestPoweroff => {}
            StopStep::HardPowerOff => self.warnings.push(format!(
                "VM {} ignored both its poweroff and the ACPI shutdown request; it was powered off hard",
                vm.name
            )),
        }
        if !adapter.powers_on() {
            self.warnings.push(format!(
                "tilo-vm cannot power {} on: it stays off until someone does (tilo-vm vm restart reboots it instead)",
                vm.name
            ));
        }
        let text = match down {
            Some(true) if asked => format!(
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
        Ok(Outcome::new(
            text,
            json!({ "vm": vm.name, "down": down, "step": step.as_str() }),
        )
        .next(&["tilo-vm vm start"]))
    }

    fn vm_rm(
        &mut self,
        runner: &mut dyn Runner,
        name: &str,
        force: bool,
    ) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(Some(name))?;
        let adapter = Self::adapter(&vm)?;
        // vm create gives every VM it makes its own key in keys/. A VM
        // registered by hand (a shared VM, say) is only forgotten: its
        // hypervisor, disk and key are not ours to delete.
        let own_key = sshkey::key_path(&self.registry_path, &vm.name);
        let owned = vm.entry.key.as_deref().map(Path::new) == Some(own_key.as_path());
        if owned && adapter.running(&vm.vm(), runner) == Some(true) {
            if !force {
                return Err(CliError::VmRunning { vm: vm.name });
            }
            if let Some(off) = adapter.power_off(&vm.vm()) {
                run_all(runner, &[off])?;
            }
            let started = Instant::now();
            while adapter.running(&vm.vm(), runner) == Some(true) {
                if started.elapsed() > Duration::from_secs(60) {
                    return Err(CliError::VmRunning { vm: vm.name });
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        } else if owned
            && force
            && let Some(off) = adapter.power_off(&vm.vm())
        {
            let _ = runner.output(&off);
        }
        let mut removed = Vec::new();
        if owned {
            run_all(runner, &adapter.destroy(&vm.vm()))?;
            let mut files = vm
                .entry
                .disk
                .as_deref()
                .map(hypervisor::vm_files)
                .unwrap_or_default();
            let key = own_key.display().to_string();
            files.push(sshkey::known_hosts_path(&key));
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
        let bases = branch::release_bases(&mut registry, &mut self.warnings);
        registry.save(&self.registry_path)?;
        let mut text = if !adapter.powers_on() {
            format!(
                "device {} unregistered{}; the device itself is untouched",
                vm.name,
                if owned { " and its key removed" } else { "" }
            )
        } else if owned {
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
        for base in &bases {
            let _ = write!(text, "\n  removed base {base}: no VM uses it any more");
        }
        if let Some(image) = vm.entry.image.as_ref().filter(|i| !bases.contains(i)) {
            let users = registry.base_users(image);
            if users.is_empty() {
                let _ = write!(text, "\n  kept its image {image}");
            } else {
                let _ = write!(
                    text,
                    "\n  kept its base {image}, which {} still use",
                    users.join(", ")
                );
            }
        }
        Ok(Outcome::new(
            text,
            json!({ "vm": vm.name, "deleted": owned, "removed": removed, "image": vm.entry.image, "bases_removed": bases }),
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
            let from = entry
                .branched_from
                .as_ref()
                .map(|from| format!("branch of {from}"));
            marks.extend(from.as_deref());
            lines.push(format!(
                "  {name:<16} {:<8} {}",
                entry.hypervisor,
                marks.join(", ")
            ));
        }
        if !registry.bases.is_empty() {
            lines.push(
                "bases (frozen disks branches share; each is deleted once nothing uses it):".into(),
            );
        }
        let mut bases = serde_json::Map::new();
        for (id, base) in &registry.bases {
            let users = registry.base_users(&base.disk);
            lines.push(format!(
                "  {id}: {}'s disk on {}, used by {}\n    {}",
                base.from,
                base.frozen,
                users.join(", "),
                base.disk
            ));
            bases.insert(id.clone(), json!({ "base": base, "users": users }));
        }
        let mut outcome = Outcome::new(
            lines.join("\n"),
            json!({ "registry": self.registry_path, "vms": registry.vms, "bases": bases, "default": registry.default, "current": current }),
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
        let endpoint = self.endpoint(&vm)?;
        let line = sshkey::ssh_command(
            &endpoint,
            vm.entry.key.as_deref(),
            command,
            command.is_empty(),
        );
        let ok = self.runner.interactive(&line)?;
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
            Ok(mut remote) => {
                lines.push(format!(
                    "  running and ready; device steps {} ({})",
                    remote.status.daemon_version, remote.status.arch
                ));
                let hello = remote.status.clone();
                if let Some(wake) = self.refresh_net(&mut remote, &vm) {
                    lines.push(format!("  wake: {wake}"));
                    let entry = self.resolve_vm(Some(&vm.name))?.entry;
                    insert(&mut data, "address", json!(entry.ssh));
                    insert(&mut data, "hostname", json!(entry.hostname));
                    insert(&mut data, "mac", json!(entry.mac));
                    insert(&mut data, "wake", json!(entry.wake));
                }
                insert(&mut data, "device", json!(vm.entry.device));
                insert(&mut data, "state", json!("ready"));
                insert(&mut data, "reachable", json!(true));
                insert(&mut data, "daemon", json!(hello));
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
                host::evaluate_vm(facts, name, args.emulated)
            }
            None if args.emulated && facts.os == "linux" => {
                host::evaluate_vm(facts, "libvirt", true)
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
        let ssh_port =
            crate::forwarded_port::select(&self.registry()?, choice.adapter, args.ssh_port)?;
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
        let registry = self.registry()?;
        let image_text = image.as_ref().map(|i| i.display().to_string());
        if let Some((owner, _)) = registry
            .vms
            .iter()
            .find(|(_, vm)| vm.disk.is_some() && vm.disk == image_text)
        {
            return Err(CliError::Unsupported {
                what: format!("--image with the disk of VM {owner}"),
                alternative: format!(
                    "a child of that disk breaks once {owner} boots or is removed; branch {owner} instead: tilo-vm vm branch {owner} {}",
                    args.name
                ),
            });
        }
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
            if let Some(space) = host_space(dir)
                && space.free < needed.saturating_add(floor)
            {
                return Err(CliError::NoSpace {
                    place: measured(dir, &space),
                    free: room(&space),
                    what: format!("the {} dev disk download", published.tag),
                    needed: human_bytes(needed.saturating_add(floor)),
                });
            }
        }
        let disk_path = match (&args.disk, image.is_some() || published.is_some()) {
            (Some(path), _) => Some(absolute(Path::new(path))),
            (None, true) => Some(
                tilo_paths::data_local_dir()
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
            if let Some(space) = host_space(dir)
                && space.free < floor
            {
                return Err(CliError::LowDisk {
                    place: measured(dir, &space),
                    free: room(&space),
                    floor: human_bytes(floor),
                });
            }
        }
        Ok(Plan {
            choice,
            ssh_port,
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
        if args.heads.is_some() && adapter != "libvirt" {
            return Err(CliError::Unsupported {
                what: format!("--heads with the {adapter} adapter"),
                alternative: "use --hypervisor libvirt, or omit --heads".into(),
            });
        }
        if (args.resolution.is_some() || args.gl) && !matches!(adapter, "libvirt" | "qemu") {
            return Err(CliError::Unsupported {
                what: format!("--resolution or --gl with the {adapter} adapter"),
                alternative: "use --hypervisor libvirt or qemu, or omit them".into(),
            });
        }
        if (args.sound || args.vulkan) && !matches!(adapter, "libvirt" | "qemu") {
            return Err(CliError::Unsupported {
                what: format!("--sound or --vulkan with the {adapter} adapter"),
                alternative: "use --hypervisor libvirt or qemu, or omit them".into(),
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
        if args.hypervisor.as_deref() == Some(hypervisor::DEVICE) {
            return Err(CliError::Unsupported {
                what: "creating a device".into(),
                alternative: format!(
                    "register one that runs a Tilo dev image: tilo-vm vm add {} --ssh root@<address>",
                    args.name
                ),
            });
        }
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
        let template = match (
            plan.choice.adapter == "utm",
            args.template.clone(),
            image.as_deref(),
        ) {
            (true, None, Some(disk)) => {
                let name = format!("Tilo Dev ({})", args.name);
                crate::hypervisor::utm_import_template(
                    &mut self.runner,
                    (&name, disk),
                    (
                        args.memory_mb.unwrap_or(crate::hypervisor::UTM_MEMORY_MB),
                        args.cpus.unwrap_or(crate::hypervisor::UTM_CPUS),
                    ),
                )?;
                self.receipt
                    .set("utm-template", json!({ "name": name, "disk": disk }));
                Some(name)
            }
            (true, None, None) => {
                return Err(CliError::Unsupported {
                    what: "a UTM VM without a disk or a template".into(),
                    alternative: format!(
                        "tilo-vm vm create {} --image <disk.qcow2>, or --template '<an imported UTM VM>'",
                        args.name
                    ),
                });
            }
            (_, template, _) => template,
        };
        if let Some(dir) = plan.disk_path.as_deref().and_then(Path::parent) {
            std::fs::create_dir_all(dir)
                .map_err(CliError::io(format!("create {}", dir.display())))?;
        }
        let key = sshkey::key_path(&self.registry_path, &args.name);
        let root_key = sshkey::ensure(&mut self.runner, &key, &args.name)?;
        let known_hosts = sshkey::known_hosts_path(&key.display().to_string());
        match std::fs::remove_file(&known_hosts) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(CliError::io(format!("remove {known_hosts}"))(e));
            }
            _ => {}
        }
        let (uuid, nic_mac) = hypervisor::new_identity();
        let identified = matches!(plan.choice.adapter, "libvirt" | "whpx");
        let lan = hypervisor::adapter(plan.choice.adapter)?
            .joins_lan()
            .then(|| {
                if args.no_lan {
                    crate::lan::OFF.to_owned()
                } else {
                    crate::lan::new_mac(|mac| lan_taken(&registry, mac))
                }
            });
        let render_node = if args.gl {
            Some(crate::qemu::resolve_rendernode(
                args.render_node.as_deref(),
            )?)
        } else {
            None
        };
        let entry = VmEntry {
            lan,
            hypervisor: plan.choice.adapter.to_owned(),
            uuid: identified.then_some(uuid),
            nic_mac: identified.then_some(nic_mac),
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
            render_node: render_node.as_ref().map(|node| node.path.clone()),
            vulkan: args.vulkan,
            sound: args.sound,
            resolution: args.resolution,
            cpus: args.cpus,
            ssh_port: plan.ssh_port,
            template,
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
        let (vulkan, sound_wav) = (
            entry.vulkan,
            entry
                .disk
                .as_deref()
                .filter(|_| entry.sound)
                .map(crate::sound::wav_path),
        );
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
        if let Some(node) = &render_node {
            let api = if vulkan {
                "GL and Vulkan render"
            } else {
                "GL renders"
            };
            let _ = write!(text, "\n  {api} on {} ({})", node.path, node.reason);
        }
        if let Some(wav) = &sound_wav {
            let _ = write!(
                text,
                "\n  its sound card plays into {wav} (tilo-vm vm sound {} measures it)",
                args.name
            );
        }
        if plan.choice.adapter == "libvirt" {
            let _ = write!(text, "\n  {}", host::VIRT_MANAGER_TIP);
        }
        let start = format!("tilo-vm vm start {}", args.name);
        let account = "tilo-vm account create --headless".to_owned();
        let mut outcome = Outcome::new(
            text,
            json!({ "vm": args.name, "created": true, "tag": tag, "hypervisor": plan.choice, "ssh_port": plan.ssh_port, "key": key, "key_as_credential": credentials, "commands": commands, "render_node": render_node.as_ref().map(|n| &n.path) }),
        );
        outcome.next = vec![start, account];
        Ok(outcome)
    }

    fn resolve_published(
        &mut self,
        tag: Option<&str>,
        arch: &str,
        format: &str,
        auto_direct_boot: bool,
    ) -> Result<Published, CliError> {
        let token = disk::find_token();
        let auth = disk::AuthHeader::new(token.as_ref())?;
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
        Err(self.no_disk_for_host(&tags, tag.is_some(), (arch, format), &auth, token.is_none()))
    }

    fn no_disk_for_host(
        &mut self,
        tags: &[String],
        explicit: bool,
        (arch, format): (&str, &str),
        auth: &disk::AuthHeader,
        anonymous: bool,
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

    /// Restart a machine tilo-vm cannot power on: its device restarts itself, and the new boot
    /// is the one that answers.
    fn vm_reboot(&mut self, vm: &Resolved) -> Result<Outcome, CliError> {
        let mut remote = self.connect(vm)?;
        let old_boot = remote.status.boot.boot_id.clone();
        if !self.json {
            eprintln!("tilo-vm: rebooting {}", vm.name);
        }
        remote.call(Reboot {}).map_err(|error| busy_vm(vm, error))?;
        drop(remote);
        let started = Instant::now();
        let mut answered = None;
        while started.elapsed() < Duration::from_secs(600) {
            std::thread::sleep(Duration::from_secs(2));
            if let Ok(remote) = self.connect(vm)
                && remote.status.boot.boot_id != old_boot
            {
                answered = Some(remote);
                break;
            }
        }
        self.receipt
            .record("reboot", started.elapsed().as_secs_f64());
        let Some(remote) = answered else {
            return Err(CliError::Unreachable {
                vm: vm.name.clone(),
                detail: "its device did not answer from a new boot within 600s of the reboot; a device whose disk asks for a passphrase waits for it at its own keyboard, and tilo-vm vm status tells when it answers".into(),
                debug: Vec::new(),
            });
        };
        let seconds = started.elapsed().as_secs_f64();
        Ok(Outcome::new(
            format!(
                "{} rebooted; its device answered after {seconds:.0}s ({})",
                vm.name, remote.status.daemon_version
            ),
            json!({ "vm": vm.name, "boot_seconds": seconds, "daemon": remote.status.daemon_version }),
        )
        .next(&["tilo-vm status"]))
    }

    fn power_up(&mut self, adapter: &dyn Hypervisor, vm: &Resolved) -> Result<(), CliError> {
        if adapter.suspended(&vm.vm(), &mut self.runner) == Some(true)
            && let Some(wake) = adapter.wake(&vm.vm())
        {
            run_all(&mut self.runner, &wake)
        } else if adapter.running(&vm.vm(), &mut self.runner) != Some(true) {
            run_all(&mut self.runner, &adapter.start(&vm.vm()))
        } else {
            Ok(())
        }
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
            && let Some(space) = host_space(dir)
            && space.free < floor
        {
            return Err(CliError::LowDisk {
                place: measured(dir, &space),
                free: room(&space),
                floor: human_bytes(floor),
            });
        }
        let adapter = Self::adapter(&vm)?;
        let mut vm = self.join_lan_now(adapter.as_ref(), vm)?;
        let joining = self.begin_joining(&vm)?;
        if joining.is_some() {
            vm = self.resolve_vm(Some(&vm.name))?;
        }
        self.power_up(adapter.as_ref(), &vm)?;
        let started = Instant::now();
        if let Some(joining) = &joining {
            vm = self.finish_joining(&vm, joining)?;
        }
        drop(joining);
        let mut remote = None;
        let mut last_whpx_running_check = started;
        while started.elapsed() < Duration::from_secs(180) {
            match self.connect(&vm) {
                Ok(connected) => {
                    remote = Some(connected);
                    break;
                }
                Err(CliError::Unreachable { .. }) => {
                    if vm.entry.hypervisor == "whpx"
                        && last_whpx_running_check.elapsed() >= Duration::from_secs(2)
                    {
                        self.ensure_whpx_running(&vm, adapter.as_ref())?;
                        last_whpx_running_check = Instant::now();
                    }
                    let interval = if vm.entry.hypervisor == "whpx" {
                        Duration::from_millis(500)
                    } else {
                        Duration::from_secs(2)
                    };
                    std::thread::sleep(interval);
                }
                // The device answered. Waiting cannot fix a missing grant, an image without
                // the step, or any other semantic failure.
                Err(error) => return Err(error),
            }
        }
        self.receipt.record("boot", started.elapsed().as_secs_f64());
        let Some(remote) = remote else {
            return Err(CliError::Unreachable {
                vm: vm.name.clone(),
                detail: "its device did not answer within 180s of starting".into(),
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            });
        };
        self.warnings.extend(self.receipt.slow());
        let seconds = started.elapsed().as_secs_f64();
        // Accounts survive restarts, so only suggest creating one when none came back.
        let accounts = remote.status.accounts;
        let next = match accounts.first() {
            Some(account) => vec![
                "tilo-vm status".to_owned(),
                format!("tilo-vm account show --account {}", account.name),
            ],
            None => vec![
                "tilo-vm status".to_owned(),
                "tilo-vm account create --headless".to_owned(),
            ],
        };
        Ok(Outcome {
            next,
            ..Outcome::new(
                format!(
                    "VM {} is up; its device answered after {seconds:.0}s",
                    vm.name
                ),
                json!({ "vm": vm.name, "boot_seconds": seconds, "accounts": accounts.len() }),
            )
        })
    }

    /// The device's boot and bootc status, for a command that moves it onto another image.
    fn boot_facts(
        &mut self,
        vm: &Resolved,
        command: &str,
    ) -> Result<(Remote, BootFacts, BootcStatus), CliError> {
        if vm.entry.hypervisor == "whpx" && vm.entry.direct_kernel.is_some() {
            // Such a VM boots a kernel kept on this host, which would have to be copied out of
            // the deployment bootc stages.
            return Err(CliError::Unsupported {
                what: format!("{command} on a WHPX VM that boots its kernel from this host"),
                alternative: format!(
                    "create it again from the image it should run: tilo-vm vm rm {name} --force; tilo-vm vm create {name}",
                    name = vm.name
                ),
            });
        }
        let remote = self.connect(vm)?;
        let facts = remote.status.boot.clone();
        let bootc = facts.bootc.clone().ok_or_else(|| CliError::NotBootc {
            vm: vm.name.clone(),
            command: command.to_owned(),
        })?;
        Ok((remote, facts, bootc))
    }

    fn vm_upgrade(&mut self, args: &UpgradeArgs) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(args.name.as_deref())?;
        let (mut remote, facts, bootc) = self.boot_facts(&vm, "vm upgrade")?;
        let Some(from) = args.from.as_deref() else {
            if let Some(built) = args.image.as_deref().map(Path::new)
                && imageup::is_built_image(built)
            {
                return self.vm_upgrade_built(&vm, &mut remote, &facts, args, built);
            }
            return self.vm_upgrade_registry(
                &vm,
                &mut remote,
                (&facts, &bootc),
                args,
                args.image.as_deref(),
            );
        };
        match imageup::parse_from(from, self.worktree.as_ref())? {
            imageup::UpgradeFrom::Local => {
                let root = self.checkout()?;
                let arch = facts
                    .arch()
                    .unwrap_or_else(|| std::env::consts::ARCH.to_owned());
                let out = root.join(imageup::BUILD_OUT);
                self.t_outputs(&root, &imageup::image_check(&arch, false), &out)?;
                let built = imageup::built_image(&out, &arch);
                self.vm_upgrade_built(&vm, &mut remote, &facts, args, &built)
            }
            imageup::UpgradeFrom::Ci { tag } => {
                let image = self.resolve_ci_upgrade(&facts, &tag)?;
                self.vm_upgrade_registry(&vm, &mut remote, (&facts, &bootc), args, Some(&image))
            }
        }
    }

    /// The worktree t builds from: the one this command runs in.
    fn checkout(&self) -> Result<PathBuf, CliError> {
        match &self.worktree {
            Some(worktree) => Ok(worktree.root.clone()),
            None => std::env::current_dir()
                .ok()
                .and_then(|cwd| checkout_root(&cwd))
                .ok_or(CliError::NotInWorktree),
        }
    }

    /// Build `check` on t from this worktree's state and download its outputs under `out`,
    /// with t's own progress on this terminal.
    fn t_outputs(&mut self, root: &Path, check: &str, out: &Path) -> Result<(), CliError> {
        let command = imageup::outputs_command(check, root, out);
        if !self.json {
            eprintln!(
                "tilo-vm: building {check} from this worktree on t; t reuses its answer when nothing it reads changed"
            );
        }
        let runner = &mut self.runner;
        let built = self
            .receipt
            .phase(check, || runner.interactive(&command))
            .map_err(|error| match error {
                CliError::ToolMissing { .. } => CliError::ToolMissing {
                    tool: "t".into(),
                    purpose: "build the dev image and disk on a build server (CONTRIBUTING.md says how to set it up)".into(),
                },
                other => other,
            })?;
        if built {
            Ok(())
        } else {
            Err(CliError::CommandFailed {
                command: command.display(),
                status: "build failed".into(),
                stderr: format!(
                    "t's output above names the failed step; its full log: t log {check}"
                ),
            })
        }
    }

    fn image_build(&mut self, args: &ImageBuildArgs) -> Result<Outcome, CliError> {
        let started = Instant::now();
        let root = self.checkout()?;
        let arch = args
            .arch
            .clone()
            .unwrap_or_else(|| std::env::consts::ARCH.to_owned());
        let out = if args.out.is_absolute() {
            args.out.clone()
        } else {
            root.join(&args.out)
        };
        self.t_outputs(&root, &imageup::image_check(&arch, args.lean), &out)?;
        let built = imageup::built_image(&out, &arch);
        let facts = imageup::layout_facts(&built.join("oci")).map_err(|problem| {
            CliError::ImageRefused {
                image: built.display().to_string(),
                problem,
                fix: format!(
                    "t outputs left no dev image there; its log: t log {}",
                    imageup::image_check(&arch, args.lean)
                ),
            }
        })?;
        let image = imageup::built_name(&built);
        imageup::check_image(&image, &facts)?;
        let mut next = vec![format!("tilo-vm vm upgrade --image {}", built.display())];
        let disk = if args.disk {
            self.t_outputs(&root, &imageup::disk_check(&arch, args.lean), &out)?;
            let disk = imageup::built_disk(&out, &arch);
            next.insert(
                0,
                format!("tilo-vm vm create <name> --image {}", disk.display()),
            );
            Some(disk.display().to_string())
        } else {
            None
        };
        let layout = built.join("oci");
        let digest = std::fs::read_to_string(built.join("digest"))
            .map_err(CliError::io(format!(
                "read {}",
                built.join("digest").display()
            )))?
            .trim()
            .to_owned();
        self.receipt.set("digest", &digest);
        let data = json!({
            "image": image,
            "arch": arch,
            "digest": digest,
            "layout": built.display().to_string(),
            "layout_bytes": imageup::layout_bytes(&layout),
            "disk": disk,
            "seconds": started.elapsed().as_secs(),
        });
        let text = format!(
            "built {image} for {arch} ({}) in {}s\n  image: {} ({}){}\n  {}",
            short_digest(&digest),
            started.elapsed().as_secs(),
            built.display(),
            human_bytes(imageup::layout_bytes(&layout)),
            disk.as_deref()
                .map(|d| format!("\n  disk:  {d}"))
                .unwrap_or_default(),
            self.receipt.summary()
        );
        let next: Vec<&str> = next.iter().map(String::as_str).collect();
        Ok(Outcome::new(text, data).next(&next))
    }

    fn vm_upgrade_built(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        facts: &BootFacts,
        args: &UpgradeArgs,
        built: &Path,
    ) -> Result<Outcome, CliError> {
        let image = imageup::built_name(built);
        let refused = |problem: String| {
            CliError::ImageRefused {
            image: built.display().to_string(),
            problem,
            fix: "build it again: tilo-vm image build   (or move onto this worktree's at once: tilo-vm vm upgrade --from local)".into(),
        }
        };
        let image_facts = imageup::layout_facts(&built.join("oci")).map_err(refused)?;
        imageup::check_image(&image, &image_facts)?;
        let layout = imageup::host_layout(&vm.name);
        let digest = self
            .receipt
            .phase("stage", || imageup::stage_built(built, &layout))
            .map_err(refused)?;
        self.receipt.set("image", &image);
        self.receipt.set("layout", built.display().to_string());
        let onto = Onto {
            image: &image,
            facts: &image_facts,
            layout: &layout,
            digest,
        };
        self.move_onto_layout(vm, remote, facts, args, onto)
    }

    fn move_onto_layout(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        facts: &BootFacts,
        args: &UpgradeArgs,
        onto: Onto<'_>,
    ) -> Result<Outcome, CliError> {
        let Onto {
            image,
            facts: image_facts,
            layout,
            digest,
        } = onto;
        imageup::check_image(image, image_facts)?;
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
        let bootc = facts.bootc.clone().unwrap_or_default();
        if bootc.staged.is_none() && bootc.booted.as_ref().is_some_and(|b| b.digest == digest) {
            return Ok(Outcome::new(
                format!(
                    "VM {} already runs {image} ({}); nothing to do\n  {}",
                    vm.name,
                    short_digest(&digest),
                    self.receipt.summary()
                ),
                json!({ "vm": vm.name, "image": image, "digest": digest, "changed": false }),
            )
            .next(&["tilo-vm status"]));
        }
        let files = layout_files(layout)?;
        let (manifest, transferred) = self
            .receipt
            .phase("transfer", || remote.put_tree(layout, &files))?;
        self.receipt.set("transferred_bytes", transferred);
        self.receipt.set("image_bytes", layout_bytes);
        if !self.json {
            eprintln!(
                "tilo-vm: sent {} of the image's {} to VM {}",
                human_bytes(transferred),
                human_bytes(layout_bytes),
                vm.name,
            );
        }
        let upgrade = Upgrade {
            source: ImageSource::Layout { manifest },
            expected: Some(digest),
        };
        self.stage_and_boot(vm, remote, facts, &upgrade, image, args.no_reboot)
    }

    fn resolve_ci_upgrade(&mut self, facts: &BootFacts, tag: &str) -> Result<String, CliError> {
        let arch = facts
            .arch()
            .unwrap_or_else(|| std::env::consts::ARCH.to_owned());
        let image = match &facts.board {
            Some(board) => imageup::board_image(tag, board),
            None => imageup::ci_image(tag, &arch),
        };
        if cfg!(windows) && tag.starts_with("main-") {
            return Ok(image);
        }
        let runner = &mut self.runner;
        let exists = self
            .receipt
            .phase("resolve", || {
                runner.output(&imageup::exists_command(&image))
            })?
            .success;
        if exists {
            return Ok(image);
        }
        Err(CliError::ImageRefused {
            image: format!("ci:{tag}"),
            problem: match &facts.board {
                Some(board) => format!(
                    "names no {board} board image this host can pull (tried {image}); main publishes one with each aarch64 build"
                ),
                None => format!("names no published dev image (tried {image}); main publishes one once its dev image checks pass"),
            },
            fix: "list what is published: skopeo list-tags docker://registry.tilo.so/tilo/tilo-vm   |   or build that commit on t: check it out, then tilo-vm vm upgrade --from local".into(),
        })
    }

    fn vm_upgrade_registry(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        (facts, bootc): (&BootFacts, &BootcStatus),
        args: &UpgradeArgs,
        image: Option<&str>,
    ) -> Result<Outcome, CliError> {
        if let Some(image) = image
            && !imageup::is_dev_name(image)
        {
            imageup::check_image(image, &imageup::ImageFacts::default())?;
        }
        let imgref = image.map_or_else(
            || {
                bootc
                    .spec
                    .clone()
                    .map(|(image, _)| image)
                    .unwrap_or_default()
            },
            str::to_owned,
        );
        self.receipt.set("image", &imgref);
        let upgrade = Upgrade {
            source: ImageSource::Registry {
                image: image.map(str::to_owned),
            },
            expected: None,
        };
        match self.stage_and_boot(vm, remote, facts, &upgrade, &imgref, args.no_reboot) {
            Err(CliError::Remote(error))
                if error.code == tilo_device_protocol::code::AUTH && image.is_some() =>
            {
                if !self.json {
                    eprintln!(
                        "tilo-vm: VM {} may not pull {imgref} without a login; copying it through this host's registry login instead",
                        vm.name
                    );
                }
                let arch = facts
                    .arch()
                    .unwrap_or_else(|| std::env::consts::ARCH.to_owned());
                self.relay_private(vm, remote, facts, args, &imgref, &arch)
            }
            other => other,
        }
    }

    fn relay_private(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        facts: &BootFacts,
        args: &UpgradeArgs,
        image: &str,
        arch: &str,
    ) -> Result<Outcome, CliError> {
        let run = |runner: &mut SystemRunner, command: CommandLine| -> Result<String, CliError> {
            let output = runner.output(&command)?;
            if output.success {
                Ok(output.stdout)
            } else {
                Err(CliError::CommandFailed {
                    command: command.display(),
                    status: output.status,
                    stderr: format!(
                        "{}   (this host needs a login too: podman login registry.tilo.so)",
                        output.stderr.trim()
                    ),
                })
            }
        };
        let inspect = run(
            &mut self.runner,
            imageup::registry_inspect_command(image, arch),
        )?;
        // A board dev image is sealed by design (its UKI carries the board's
        // device tree) and the device already boots one, so its seal is no
        // refusal here, as for any image the VM pulls itself.
        let image_facts = imageup::ImageFacts {
            sealed: false,
            ..imageup::registry_facts(&inspect)
        };
        let layout = imageup::host_layout(&vm.name);
        if let Some(parent) = layout.parent() {
            std::fs::create_dir_all(parent)
                .map_err(CliError::io(format!("create {}", parent.display())))?;
        }
        let digest_file = layout.with_extension("digest");
        let copy = imageup::registry_export_command(image, arch, &layout, &digest_file);
        let runner = &mut self.runner;
        self.receipt.phase("export", || run(runner, copy))?;
        let digest = std::fs::read_to_string(&digest_file)
            .map_err(CliError::io(format!("read {}", digest_file.display())))?
            .trim()
            .to_owned();
        imageup::prune_layout(&layout)
            .map_err(CliError::io(format!("prune {}", layout.display())))?;
        let onto = Onto {
            image,
            facts: &image_facts,
            layout: &layout,
            digest,
        };
        self.move_onto_layout(vm, remote, facts, args, onto)
    }

    fn stage_and_boot(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        facts: &BootFacts,
        upgrade: &Upgrade,
        imgref: &str,
        no_reboot: bool,
    ) -> Result<Outcome, CliError> {
        let staged = self
            .receipt
            .phase("stage", || remote.call(upgrade.clone()))
            .map_err(|error| busy_vm(vm, error))?;
        let (previous, next) = match staged {
            UpgradeResult::AlreadyBooted => {
                return Ok(Outcome::new(
                    format!(
                        "VM {} already runs this image (same composefs digest); nothing to do\n  {}",
                        vm.name,
                        self.receipt.summary()
                    ),
                    json!({ "vm": vm.name, "image": imgref, "changed": false }),
                )
                .next(&["tilo-vm status"]));
            }
            UpgradeResult::Current { booted } => {
                return Ok(Outcome::new(
                    format!(
                        "VM {} already runs the newest {}; nothing to do",
                        vm.name,
                        booted
                            .as_ref()
                            .map_or_else(|| "(unknown)".into(), Deployment::describe)
                    ),
                    json!({ "vm": vm.name, "changed": false, "booted": booted.map(|p| p.digest) }),
                )
                .next(&["tilo-vm status"]));
            }
            UpgradeResult::Staged {
                previous,
                next,
                warnings,
            } => {
                self.warnings.extend(
                    warnings
                        .into_iter()
                        .map(|warning| format!("VM {}: {warning}", vm.name)),
                );
                (previous, next)
            }
        };
        let previous_text = previous
            .as_ref()
            .map_or_else(|| "(unknown)".into(), Deployment::describe);
        if no_reboot {
            return Ok(Outcome::new(
                format!(
                    "staged {} on VM {}; it boots at the next restart (now running {previous_text})\n  accounts keep their layers across it; tilo-vm status then names those built on this image\n  {}",
                    next.describe(),
                    vm.name,
                    self.receipt.summary()
                ),
                json!({ "vm": vm.name, "changed": true, "staged": next.digest, "booted": previous.map(|p| p.digest) }),
            )
            .next(&["tilo-vm vm stop", "tilo-vm vm start"]));
        }
        let (booted, mut remote) = self.reboot_into(vm, remote, &facts.boot_id, &next.digest)?;
        self.warnings.extend(self.receipt.slow());
        let change = image_change_of_this_boot(&mut remote);
        let accounts = match &change {
            Some(change) if !change.kept.is_empty() || !change.held.is_empty() => {
                change.describe().join("\n  ")
            }
            Some(_) => "no account runs layers: every account runs the new image as installed".into(),
            None => "accounts: the device did not report which accounts keep layers built on the previous image (tilo-vm status)".into(),
        };
        Ok(Outcome::new(
            format!(
                "VM {} now runs {}\n  before: {previous_text}, kept for tilo-vm vm rollback-image\n  {accounts}\n  {}",
                vm.name,
                booted.describe(),
                self.receipt.summary()
            ),
            json!({
                "vm": vm.name,
                "changed": true,
                "image": imgref,
                "digest": booted.digest,
                "previous": previous.map(|p| p.digest),
                "image_change": change,
            }),
        )
        .next(&["tilo-vm status", "tilo-vm vm rollback-image"]))
    }

    /// Restart the device into the staged `digest` and wait until it answers from that boot.
    fn reboot_into(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        old_boot: &str,
        digest: &str,
    ) -> Result<(Deployment, Remote), CliError> {
        if !self.json {
            eprintln!(
                "tilo-vm: rebooting VM {} into {}",
                vm.name,
                short_digest(digest)
            );
        }
        remote.call(Reboot {})?;
        let started = Instant::now();
        let adapter = Self::adapter(vm)?;
        let mut whpx_restarted = false;
        let mut answered = None;
        while started.elapsed() < Duration::from_secs(600) {
            // WHPX QEMU uses -no-reboot because its in-process reset is not
            // reliable. A guest reboot exits QEMU; launch the same VM again
            // once its old process has released the disk and ports.
            if vm.entry.hypervisor == "whpx"
                && !whpx_restarted
                && adapter.running(&vm.vm(), &mut self.runner) == Some(false)
            {
                run_all(&mut self.runner, &adapter.start(&vm.vm()))?;
                whpx_restarted = true;
            }
            std::thread::sleep(if vm.entry.hypervisor == "whpx" {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(2)
            });
            if let Ok(remote) = self.connect(vm)
                && !remote.status.boot.boot_id.is_empty()
                && remote.status.boot.boot_id != old_boot
            {
                answered = Some(remote);
                break;
            }
        }
        self.receipt
            .record("reboot", started.elapsed().as_secs_f64());
        let Some(remote) = answered else {
            return Err(CliError::Unreachable {
                vm: vm.name.clone(),
                detail: if vm.entry.hypervisor == hypervisor::DEVICE {
                    "its device did not answer from a new boot within 600s of the reboot; a device whose disk asks for a passphrase waits for it at its own keyboard, and tilo-vm vm status tells when it answers".into()
                } else {
                    "its device did not answer from a new boot within 600s of the reboot; if it is still booting, tilo-vm vm status tells when it answers".into()
                },
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            });
        };
        self.receipt.set("daemon", &remote.status.daemon_version);
        let booted = remote
            .status
            .boot
            .bootc
            .as_ref()
            .and_then(|bootc| bootc.booted.clone())
            .ok_or_else(|| CliError::CommandFailed {
                command: "status".into(),
                status: "no booted deployment".into(),
                stderr: String::new(),
            })?;
        if booted.digest != digest {
            return Err(CliError::CommandFailed {
                command: "status".into(),
                status: "booted another image".into(),
                stderr: format!(
                    "the VM booted {} instead of {digest}: the boot entry fell back; tilo-vm vm serial shows why",
                    booted.describe()
                ),
            });
        }
        Ok((booted, remote))
    }

    fn vm_rollback_image(
        &mut self,
        name: Option<&str>,
        no_reboot: bool,
    ) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let (mut remote, facts, bootc) = self.boot_facts(&vm, "vm rollback-image")?;
        let Some(target) = bootc.rollback.clone() else {
            return Err(CliError::Unsupported {
                what: format!("rolling back VM {}: it has no previous image", vm.name),
                alternative: "bootc keeps one previous image, the one before the last upgrade, and there is none yet; tilo-vm vm upgrade makes one".into(),
            });
        };
        let staged = remote
            .call(Upgrade {
                source: ImageSource::Rollback,
                expected: Some(target.digest.clone()),
            })
            .map_err(|error| busy_vm(&vm, error))?;
        if let UpgradeResult::Staged { warnings, .. } = staged {
            self.warnings.extend(warnings);
        }
        let current = bootc
            .booted
            .as_ref()
            .map_or_else(|| "(unknown)".into(), Deployment::describe);
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
        let (booted, _) = self.reboot_into(&vm, &mut remote, &facts.boot_id, &target.digest)?;
        Ok(Outcome::new(
            format!(
                "VM {} rolled back to {}\n  {current} is now the previous image: rollback-image again returns to it\n  {}",
                vm.name,
                booted.describe(),
                self.receipt.summary()
            ),
            json!({ "vm": vm.name, "digest": booted.digest, "previous": bootc.booted.map(|b| b.digest) }),
        )
        .next(&["tilo-vm status"]))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per account command, each a report of its own"
    )]
    fn account(&mut self, command: AccountCommand) -> Result<Outcome, CliError> {
        if let AccountCommand::Apps(AccountAppsCommand::Install { set, .. }) = &command {
            app_set(set)?;
        }
        let vm = self.resolve_vm(None)?;
        match command {
            AccountCommand::Create {
                name,
                headless: _,
                headful,
                outputs,
                identity,
                ..
            } => {
                let name = self.account_name(name)?;
                let kind = if headful {
                    AccountKind::Headful
                } else {
                    AccountKind::Headless
                };
                let owner = self
                    .worktree
                    .as_ref()
                    .map(Worktree::owner)
                    .unwrap_or_default();
                let mut remote = self.connect(&vm)?;
                let result = remote.call(AccountCreate {
                    name: name.clone(),
                    kind,
                    owner,
                    outputs,
                    identity: identity == Some(crate::cli::AccountIdentity::Throwaway),
                    device_identity: identity == Some(crate::cli::AccountIdentity::Device),
                })?;
                if identity.is_some() && result.identity.is_none() {
                    return Err(CliError::Unsupported {
                        what: "account create --identity by this VM's device steps".into(),
                        alternative: "tilo-vm vm upgrade --from local".into(),
                    });
                }
                let text = account_created_text(&name, kind, &result);
                self.receipt.set("account", &name);
                Ok(Outcome::new(text, json!(result)).next(&[
                    "tilo-vm swap <component>",
                    "tilo-vm shot",
                    "tilo-vm control actions list",
                ]))
            }
            AccountCommand::List => {
                let accounts = self.connect(&vm)?.call(AccountList {})?;
                let text = if accounts.is_empty() {
                    "no accounts yet".to_owned()
                } else {
                    accounts
                        .iter()
                        .map(account_line)
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let mut outcome = Outcome::new(text, json!(accounts));
                if accounts.is_empty() {
                    outcome = outcome.next(&["tilo-vm account create --headless"]);
                }
                Ok(outcome)
            }
            AccountCommand::Show { name, .. } => {
                let name = self.account_name(name)?;
                let mut remote = self.connect(&vm)?;
                let shown = remote.call(AccountShow { name })?;
                let layers = remote.call(LayerList {})?;
                Ok(installed_account_outcome(&shown, &layers))
            }
            AccountCommand::Exec {
                name,
                timeout,
                command,
                ..
            } => self.account_exec(&vm, name, timeout, &command),
            AccountCommand::Apps(AccountAppsCommand::Install {
                name, set, timeout, ..
            }) => self.apps_install(&vm, name, &set, timeout),
            AccountCommand::Apps(AccountAppsCommand::List) => Ok(apps_list()),
            AccountCommand::Restart { name, .. } => {
                let name = self.account_name(name)?;
                let mut remote = self.connect(&vm)?;
                let result = self
                    .receipt
                    .phase("apply", || remote.call(AccountRestart { name }))?;
                Ok(Outcome::new(describe_apply(&result), json!(result))
                    .next(&["tilo-vm logs", "tilo-vm shot"]))
            }
            AccountCommand::Rm { name, .. } => {
                let name = self.account_name(name)?;
                let result = self.connect(&vm)?.call(AccountRm { name: name.clone() })?;
                let text = if result.removed {
                    format!(
                        "account {name} removed: session stopped, user and generations deleted; its layers go at the next gc"
                    )
                } else {
                    format!("account {name} does not exist; nothing to do")
                };
                Ok(Outcome::new(text, json!(result)).next(&["tilo-vm gc --dry-run"]))
            }
        }
    }

    fn apps_install(
        &mut self,
        vm: &Resolved,
        name: Option<String>,
        set: &str,
        timeout: u64,
    ) -> Result<Outcome, CliError> {
        let apps = app_set(set)?;
        let account = self.account_name(name)?;
        self.connect(vm)?.call(AccountShow {
            name: account.clone(),
        })?;
        let packages: Vec<String> = apps.apps.iter().map(|a| a.package.clone()).collect();
        let mut remote = self.connect(vm)?;
        let installed = self.receipt.phase("install", || {
            remote.call(tilo_device_protocol::InstallPackages {
                packages: packages.clone(),
                timeout,
            })
        })?;
        if installed.code != 0 {
            return Err(CliError::PackagesFailed {
                set: set.to_owned(),
                vm: vm.name.clone(),
            });
        }
        let restarted = self.receipt.phase("apply", || {
            remote.call(AccountRestart {
                name: account.clone(),
            })
        })?;
        self.receipt.set("account", &account);
        let mut text = vec![format!(
            "installed the {set} apps into VM {} (a transient /usr overlay: gone at the next VM reboot) and restarted account {account}'s session",
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
            json!({ "vm": vm.name, "account": account, "set": set, "packages": packages, "restart": restarted }),
        )
        .next(&[
            "tilo-vm input key super",
            "tilo-vm input type xterm",
            "tilo-vm input key Return",
            "tilo-vm shot",
        ]))
    }

    fn account_exec(
        &mut self,
        vm: &Resolved,
        name: Option<String>,
        timeout: u64,
        command: &[String],
    ) -> Result<Outcome, CliError> {
        let account = self.account_name(name)?;
        let ran = self.connect(vm)?.call(tilo_device_protocol::Exec {
            account: account.clone(),
            command: command.to_vec(),
            timeout,
            max_bytes: tilo_device_protocol::EXEC_MAX_BYTES,
        })?;
        let stdout = STANDARD
            .decode(&ran.stdout)
            .map_err(|error| CliError::CommandFailed {
                command: "exec".into(),
                status: "unreadable output".into(),
                stderr: error.to_string(),
            })?;
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let ok = ran.code == 0;
        let mut outcome = Outcome::new(
            stdout.trim_end(),
            json!({ "vm": vm.name, "account": account, "command": command, "ok": ok, "code": ran.code, "stdout": stdout }),
        );
        outcome.failed = !ok;
        Ok(outcome)
    }

    fn vm_has_bootc(&mut self, vm: &Resolved) -> Option<bool> {
        let remote = self.connect(vm).ok()?;
        Some(remote.status.boot.bootc.is_some())
    }

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

    fn cannot_build(&mut self, vm: &Resolved, error: CliError) -> CliError {
        match error {
            CliError::VmCannotBuild { missing, .. } => CliError::VmCannotBuild {
                missing,
                bootc: self.vm_has_bootc(vm),
            },
            other => other,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one swap sequence: resolve, build, deliver and receipt stay in order"
    )]
    fn swap(
        &mut self,
        account: Option<String>,
        component: String,
        from: &str,
        builder: Option<Builder>,
        profile: Option<&str>,
        frame_cpu_trace: bool,
        kernel: KernelFlags,
    ) -> Result<Outcome, CliError> {
        if self.local {
            if builder.is_some()
                || profile.is_some()
                || frame_cpu_trace
                || kernel.direct
                || kernel.jobs.is_some()
                || kernel.cmdline.is_some()
            {
                return Err(CliError::Unsupported {
                    what: "--local with a build, profile or kernel flag".into(),
                    alternative:
                        "import-layer imports a prebuilt layer; --local swap only swaps an already imported layer"
                            .into(),
                });
            }
            return self.swap_local(account, &component, from);
        }
        validate_frame_cpu_component(&component, frame_cpu_trace)?;
        // Boot components first: the kernel swaps VM-wide (its account is `vm`
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
            return self.swap_boot(account.as_deref(), &entry, from, other_flags, kernel);
        }
        reject_kernel_flags(&component, &kernel)?;
        let account = self.account_name(account)?;
        self.receipt.set("account", &account);
        self.receipt.set("component", &component);
        if component == "userspace" {
            return self.swap_userspace(&account, from, builder, profile);
        }
        self.record_swap_profile(profile, &component, from)?;
        if let Some(app) = component.strip_prefix("app:") {
            return self.swap_app(&account, app, from, builder);
        }
        let manifest = self.manifest()?;
        let entry = manifest.as_ref().and_then(|m| m.get(&component)).cloned();
        if let Some(c) = &entry
            && (account == VM_ACCOUNT) != (c.info.scope == Scope::Vm)
        {
            return Err(CliError::ScopeMismatch {
                component,
                account,
                scope: c.info.scope.as_str().into(),
            });
        }
        let source = ci::parse(from, self.worktree.as_ref())?;
        if matches!(source, ci::Source::Worktree)
            && let Some(explicit @ Builder::Host) = builder
        {
            choose_builder(
                Some(explicit),
                &VmTools::default(),
                cfg!(target_os = "linux"),
                std::env::consts::OS,
            )?;
        }
        let cohort_inputs = self.cohort_inputs(&component, &source)?;
        let vm = self.resolve_vm(None)?;
        let mut remote = self.prepare_swap_connection(&vm)?;
        let apps_build = if cohort_inputs.is_some() {
            let configured = self
                .registry()?
                .remote_builders
                .get(&remote.status.arch)
                .is_some_and(|line| !line.is_empty());
            let plan = plan_apps_runtime_build(
                builder,
                &remote.status.arch,
                std::env::consts::ARCH,
                cfg!(target_os = "linux"),
                configured,
            )?;
            self.receipt.set("apps_runtime_build", plan.as_str());
            Some(plan)
        } else {
            None
        };
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
                self.worktree_component_layer(
                    &mut remote,
                    &vm,
                    entry,
                    (builder, apps_build),
                    profile,
                )?
            }
        };
        if let Some(inputs) = &cohort_inputs {
            self.cohort_prerequisites(&account, &component, inputs, apps_build)?;
        }
        self.receipt.set("layer", &layer);
        remote = self.connect_with_wake(&vm, true)?;
        let info = frame_cpu_info(entry, frame_cpu_trace)?;
        let result = self.receipt.phase("apply", || {
            remote.call(Swap {
                account: account.clone(),
                component: component.clone(),
                layer: layer.clone(),
                info,
                partner: None,
            })
        })?;
        if result.changed && result.policy == Policy::SessionRestart {
            self.receipt
                .phase("session-ready", || wait_for_session(&mut remote, &account));
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

    fn record_swap_profile(
        &mut self,
        profile: Option<&str>,
        component: &str,
        from: &str,
    ) -> Result<(), CliError> {
        if let Some(profile) = profile {
            check_profile(profile, component, from)?;
            self.receipt.set("profile", profile);
        }
        Ok(())
    }

    fn cohort_inputs(
        &self,
        component: &str,
        source: &ci::Source,
    ) -> Result<Option<crate::source_inputs::Snapshot>, CliError> {
        if component == "apps+runtime" && matches!(source, ci::Source::Worktree) {
            crate::source_inputs::Snapshot::create(
                self.worktree.as_ref().ok_or(CliError::NotInWorktree)?,
                component,
            )
            .map(Some)
        } else {
            Ok(None)
        }
    }

    fn worktree_component_layer(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        entry: &Component,
        (builder, apps_build): (Option<Builder>, Option<AppsRuntimeBuild>),
        profile: Option<&str>,
    ) -> Result<String, CliError> {
        let selected = if entry.image_assembled && entry.name == "apps+runtime" {
            if apps_build == Some(AppsRuntimeBuild::Local) {
                self.receipt.set("builder", "host");
                return self.build_layer_local_pack(
                    remote,
                    vm,
                    &entry.clone().with_profile(profile),
                );
            }
            Some(Builder::Remote)
        } else {
            worktree_buildable(entry)?;
            builder
        };
        self.build_worktree_layer(remote, vm, &entry.clone().with_profile(profile), selected)
    }

    fn cohort_prerequisites(
        &mut self,
        account: &str,
        component: &str,
        inputs: &crate::source_inputs::Snapshot,
        apps_build: Option<AppsRuntimeBuild>,
    ) -> Result<(), CliError> {
        let prerequisite_builder = if apps_build == Some(AppsRuntimeBuild::Local) {
            Builder::Host
        } else {
            Builder::Remote
        };
        let root = self
            .worktree
            .as_ref()
            .ok_or(CliError::NotInWorktree)?
            .root
            .clone();
        self.receipt.set(
            "cohort_source",
            build::source_provenance(
                self.worktree.as_ref().ok_or(CliError::NotInWorktree)?,
                component,
                if prerequisite_builder == Builder::Host {
                    "host"
                } else {
                    "remote"
                },
            ),
        );
        let mut outcomes = Vec::new();
        for component in ["launcher", "install"] {
            inputs.unchanged(&root)?;
            let outcome = self.swap(
                Some(account.into()),
                component.into(),
                "worktree",
                Some(prerequisite_builder),
                None,
                false,
                KernelFlags::default(),
            )?;
            inputs.unchanged(&root)?;
            outcomes.push(json!({"component": component, "result": outcome.data}));
            self.receipt.set("cohort_prerequisites", &outcomes);
        }
        self.receipt.set("component", component);
        Ok(())
    }

    fn swap_boot(
        &mut self,
        account: Option<&str>,
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
        self.swap_kernel(account, entry, from, kernel)
    }

    fn build_worktree_layer(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: Option<Builder>,
    ) -> Result<String, CliError> {
        let arch = remote.status.arch.clone();
        let remote_builder = self.registry()?.remote_builders.get(&arch).cloned();
        let builder = match builder {
            None if vm.entry.hypervisor == hypervisor::DEVICE && remote_builder.is_some() => {
                Some(Builder::Remote)
            }
            other => other,
        };
        if builder == Some(Builder::Remote) {
            let command = remote_builder
                .filter(|command| !command.is_empty())
                .ok_or_else(|| self.no_remote_builder(&arch))?;
            self.receipt.set("builder", "remote");
            return self.build_layer_remote(remote, vm, component, &command);
        }
        let (chosen, note) = choose_builder(
            builder,
            &remote.status.tools,
            cfg!(target_os = "linux"),
            std::env::consts::OS,
        )
        .map_err(|error| self.cannot_build(vm, error))?;
        self.announce_builder(vm, note);
        self.receipt
            .set("builder", if chosen == Builder::Vm { "vm" } else { "host" });
        match chosen {
            Builder::Vm => self.build_layer_vm(vm, component),
            Builder::Host => self.build_layer(remote, vm, component),
            Builder::Remote => Err(self.no_remote_builder(&arch)),
        }
    }

    fn check_vm_space(&self, remote: &mut Remote, vm: &Resolved) -> Result<(), CliError> {
        let status = remote.call(Status { account: None })?;
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

    /// Lay this worktree out in the VM: only the files it does not hold yet go.
    fn sync_worktree(&mut self, vm: &Resolved) -> Result<(Worktree, String), CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let id = vmbuild::worktree_id(&worktree, crate::context::hostname().as_deref());
        let files = vmbuild::worktree_files(&worktree.root)?;
        eprintln!(
            "tilo-vm: syncing {} to {}:{}/{id}",
            worktree.root.display(),
            vm.name,
            tilo_device_protocol::SRC_DIR
        );
        let mut remote = self.connect(vm)?;
        let root = worktree.root.clone();
        self.receipt
            .phase("sync", || remote.sync_tree(&id, &root, &files))?;
        self.receipt.set("worktree_id", &id);
        Ok((worktree, id))
    }

    /// Run a build step on the VM; its output shows here as it goes.
    fn vm_job<C>(&mut self, vm: &Resolved, job: C) -> Result<JobOutput, CliError>
    where
        C: tilo_device_protocol::Call<Output = JobOutput>,
    {
        let output = self.connect(vm)?.call(job)?;
        let (JobOutput::Layer { phases, .. } | JobOutput::App { phases, .. }) = &output;
        for (name, seconds) in phases {
            self.receipt.record(name, *seconds);
        }
        if let JobOutput::Layer { cache, .. } = &output {
            self.receipt.set("cache", cache);
        }
        Ok(output)
    }

    fn build_layer_vm(&mut self, vm: &Resolved, component: &Component) -> Result<String, CliError> {
        let (worktree, id) = self.sync_worktree(vm)?;
        let job = Build {
            worktree: id,
            target: BuildTarget::Component {
                name: component.name.clone(),
                profile: Some(component.profile.clone()),
            },
            provenance: build::source_provenance(&worktree, &component.name, "vm"),
            owner: worktree.owner(),
            color: std::io::IsTerminal::is_terminal(&std::io::stderr()),
        };
        match self.vm_job(vm, job)? {
            JobOutput::Layer { hash, .. } => Ok(hash),
            JobOutput::App { .. } => Err(CliError::CommandFailed {
                command: "build".into(),
                status: "unexpected result".into(),
                stderr: "an app install instead of a layer".into(),
            }),
        }
    }

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
        let mut provenance = tilo_device_protocol::Provenance::local(&component.name);
        provenance.source_commit = commit;
        provenance.cargo_profile = Some(component.profile.clone());
        provenance.features.clone_from(&component.features);
        let job = CiLayer {
            image: image.clone(),
            component: component.name.clone(),
            paths: component.paths.clone(),
            provenance,
        };
        let from_host = Self::adapter(vm)?.ci_layers_from_host();
        if remote.status.tools.podman && !from_host {
            eprintln!("tilo-vm: pulling {image} in VM {}", vm.name);
            return match self.vm_job(vm, job) {
                Ok(JobOutput::Layer { hash, .. }) => Ok(hash),
                Ok(JobOutput::App { .. }) => Err(CliError::CommandFailed {
                    command: "ci-layer".into(),
                    status: "unexpected result".into(),
                    stderr: "an app install instead of a layer".into(),
                }),
                Err(error) => Err(error),
            };
        }
        self.ci_layer_from_host(remote, vm, component, &image, &job, from_host)
    }

    fn ci_layer_from_host(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        image: &str,
        job: &CiLayer,
        from_host: bool,
    ) -> Result<String, CliError> {
        // A device, or a VM without podman: pull here, upload only the layer.
        // The layer stays cached here by the image's digest, so a repeat swap
        // of the same build only asks the registry which digest it is.
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let staging = build::Staging::new(&worktree)?;
        let arch = remote.status.arch.clone();
        let cache = cicache::CiCache::at_home();
        let runner = &mut self.runner;
        let digest = self
            .receipt
            .phase("lookup", || ci::registry_digest(runner, image, &arch));
        let cached = digest
            .as_deref()
            .and_then(|digest| cache.get(digest, &component.name));
        let (archive, pulled) = if let Some((archive, pulled)) = cached {
            eprintln!(
                "tilo-vm: {image} ({arch}) is {}, whose {} layer this host has kept; no pull",
                digest.as_deref().unwrap_or_default(),
                component.name
            );
            self.receipt.set("ci_cache", "hit");
            if let Some(hash) = digest
                .as_deref()
                .and_then(|digest| cache.layer_hash(digest, &component.name))
                && remote.call(LayerList {})?.iter().any(|l| l.hash == hash)
            {
                eprintln!(
                    "tilo-vm: {} already holds layer {}; no upload",
                    vm.name,
                    short(&hash)
                );
                self.receipt.set("upload", "skipped");
                return Ok(hash);
            }
            (archive, pulled)
        } else {
            let why = if from_host {
                "only the layer goes to the device"
            } else {
                "the VM has no podman"
            };
            eprintln!("tilo-vm: pulling {image} ({arch}) on this host; {why}");
            let runner = &mut self.runner;
            let pulled = self.receipt.phase("pull", || {
                ci::extract_on_host(runner, image, &arch, &job.paths, &staging.tree())
            })?;
            build::pack(&staging.tree(), &staging.archive())?;
            if let Err(error) = cache.put(&component.name, &staging.archive(), &pulled) {
                self.warnings.push(format!(
                    "could not keep the layer for the next swap: {error}"
                ));
            }
            self.receipt.set("ci_cache", "miss");
            (staging.archive(), pulled)
        };
        // The record the VM's own pull would have written.
        let provenance = tilo_device_protocol::Provenance::from_ci(
            &job.provenance,
            &arch,
            pulled.digest,
            pulled.revision,
        );
        let upload = self
            .receipt
            .phase("transfer", || -> Result<String, CliError> {
                let file = std::fs::File::open(&archive)
                    .map_err(CliError::io(format!("open {}", archive.display())))?;
                remote.put_reader(std::io::BufReader::new(file))
            })?;
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source: LayerSource::Content { hash: upload },
                provenance: Box::new(provenance),
            })
        })?;
        if let Some(digest) = &digest {
            let _ = cache.record_hash(digest, &component.name, &imported.hash);
        }
        Ok(imported.hash)
    }

    fn swap_app(
        &mut self,
        account: &str,
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
        let arch = remote.status.arch.clone();
        let lock = std::fs::read(worktree.root.join("Cargo.lock"))
            .ok()
            .map(|bytes| build::sha256_hex(&bytes));
        check_app_lock(
            lock.as_deref(),
            remote.status.base.cargo_lock_hash.as_deref(),
        )?;
        let configured = self.registry()?.remote_builders.get(&arch).cloned();
        let builder = if vm.entry.hypervisor == hypervisor::DEVICE {
            Some(plan_device_app_build(
                dir,
                builder,
                &arch,
                std::env::consts::ARCH,
                cfg!(target_os = "linux"),
                configured.as_ref().is_some_and(|line| !line.is_empty()),
            )?)
        } else {
            builder
        };
        let (chosen, note) = choose_builder(
            builder,
            &remote.status.tools,
            cfg!(target_os = "linux"),
            std::env::consts::OS,
        )
        .map_err(|error| self.cannot_build(&vm, error))?;
        self.announce_builder(&vm, note);
        let result: AppInstallResult = match chosen {
            Builder::Vm => {
                let (worktree, id) = self.sync_worktree(&vm)?;
                let job = Build {
                    worktree: id,
                    target: BuildTarget::App {
                        dir: dir.to_owned(),
                        account: account.to_owned(),
                    },
                    provenance: build::source_provenance(&worktree, &format!("app:{dir}"), "vm"),
                    owner: worktree.owner(),
                    color: std::io::IsTerminal::is_terminal(&std::io::stderr()),
                };
                match self.vm_job(&vm, job)? {
                    JobOutput::App { result, .. } => result,
                    JobOutput::Layer { .. } => {
                        return Err(CliError::CommandFailed {
                            command: "build".into(),
                            status: "unexpected result".into(),
                            stderr: "a layer instead of an app install".into(),
                        });
                    }
                }
            }
            Builder::Host => {
                self.install_app_from_host(&mut remote, &vm, &worktree, account, dir)?
            }
            Builder::Remote => {
                let command = configured
                    .filter(|line| !line.is_empty())
                    .ok_or_else(|| self.no_remote_builder(&arch))?;
                self.install_app_from_remote(&mut remote, &vm, account, dir, &command)?
            }
        };
        let what = match (&result.id, &result.version) {
            (Some(id), Some(version)) => format!("{id} {version}"),
            (Some(id), None) => id.clone(),
            _ => format!("apps/{dir}"),
        };
        let text = format!(
            "installed {what} (self-signed) into account {account}'s app store; nothing restarted: it is live the next time the app launches\n  app installs are not generations: tilo-vm rollback does not undo them"
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
        vm: &Resolved,
        worktree: &Worktree,
        account: &str,
        dir: &str,
    ) -> Result<AppInstallResult, CliError> {
        self.runner.cwd = Some(worktree.root.clone());
        let target = build::target_dir(&self.cargo_metadata()?);
        let staging = build::Staging::new(worktree)?;
        let tapp = staging.tree().with_extension("tapp");
        let package = build::app_package(&target, dir, &tapp);
        self.install_host_app_package(
            remote,
            account,
            |runner| {
                if !runner.interactive(&package)? {
                    return Err(CliError::CommandFailed {
                        command: package.display(),
                        status: "failed".into(),
                        stderr: String::new(),
                    });
                }
                Ok(tapp)
            },
            |app| app.connect(vm),
        )
    }

    fn install_host_app_package(
        &mut self,
        remote: &mut Remote,
        account: &str,
        build: impl FnOnce(&mut SystemRunner) -> Result<PathBuf, CliError>,
        reconnect: impl FnOnce(&mut Self) -> Result<Remote, CliError>,
    ) -> Result<AppInstallResult, CliError> {
        let runner = &mut self.runner;
        let tapp = self.receipt.phase("build", || build(runner))?;
        // Packaging may outlast the transport's idle lifetime. Open the usual
        // authenticated route again before any upload; never replay AppInstall.
        *remote = reconnect(self)?;
        let upload = self
            .receipt
            .phase("transfer", || -> Result<String, CliError> {
                let file = std::fs::File::open(&tapp)
                    .map_err(CliError::io(format!("open {}", tapp.display())))?;
                remote.put_reader(std::io::BufReader::new(file))
            })?;
        self.receipt.phase("install", || {
            remote.call(AppInstall {
                account: account.to_owned(),
                source: LayerSource::Content { hash: upload },
            })
        })
    }

    fn cargo_metadata(&mut self) -> Result<Value, CliError> {
        let runner = &mut self.runner;
        self.receipt
            .phase("metadata", || build::cargo_metadata(runner))
    }

    fn build_layer(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
    ) -> Result<String, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self.cargo_metadata()?;
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
        for native in build::native_builds(component, &target) {
            let runner = &mut self.runner;
            if !self
                .receipt
                .phase("native-build", || runner.interactive(&native))?
            {
                return Err(CliError::CommandFailed {
                    command: native.display(),
                    status: "build failed".into(),
                    stderr: String::new(),
                });
            }
        }
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
            Ok(())
        });
        assembled?;
        *remote = self.connect_with_wake(vm, false)?;
        let features = component.build_features(&extra);
        let provenance = build::provenance(
            &mut self.runner,
            &worktree,
            component,
            features,
            &staging.tree(),
        );
        self.import_host_layer(remote, &staging, provenance)
    }

    fn import_host_layer(
        &mut self,
        remote: &mut Remote,
        staging: &build::Staging,
        provenance: tilo_device_protocol::Provenance,
    ) -> Result<String, CliError> {
        let hash = self.receipt.phase("digest", || {
            tilo_device_protocol::layer_hash::tree_hash(&staging.tree())
                .map_err(CliError::io("hash assembled layer"))
        })?;
        let cached = self
            .receipt
            .phase("layer_lookup", || remote.call(LayerList {}))?;
        if cached
            .iter()
            .any(|layer| layer.hash == hash && layer.provenance.component == provenance.component)
        {
            self.receipt.set("upload", "skipped");
            self.receipt.set("layer_reused", &hash);
            eprintln!(
                "tilo-vm: device already holds layer {}; no pack or upload",
                short(&hash)
            );
            return Ok(hash);
        }
        self.receipt
            .phase("pack", || build::pack(&staging.tree(), &staging.archive()))?;
        let archive = staging.archive();
        let upload = self
            .receipt
            .phase("transfer", || -> Result<String, CliError> {
                let file = std::fs::File::open(&archive)
                    .map_err(CliError::io(format!("open {}", archive.display())))?;
                remote.put_reader(std::io::BufReader::new(file))
            })?;
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source: LayerSource::Content { hash: upload },
                provenance: Box::new(provenance),
            })
        })?;
        if imported.hash != hash {
            return Err(CliError::InvalidInput(format!(
                "imported layer {} does not match assembled layer {hash}",
                imported.hash
            )));
        }
        Ok(imported.hash)
    }

    fn seat(&mut self, command: SeatCommand) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(None)?;
        match command {
            SeatCommand::Claim { account, steal, .. } => {
                let account = self.account_name(account)?;
                let owner = self
                    .worktree
                    .as_ref()
                    .map(Worktree::owner)
                    .unwrap_or_default();
                let result = self.connect(&vm)?.call(SeatClaim {
                    account: account.clone(),
                    owner,
                    steal,
                })?;
                let text = crate::panel::describe_claim(&account, &result.actions, result.changed);
                Ok(Outcome::new(text, json!(result))
                    .next(&["tilo-vm vm shot", "tilo-vm seat release"]))
            }
            SeatCommand::Release { account, .. } => {
                let account = self.account_name(account)?;
                let result = self.connect(&vm)?.call(SeatRelease {
                    account: account.clone(),
                })?;
                let text =
                    crate::panel::describe_release(&account, &result.actions, result.changed);
                Ok(Outcome::new(text, json!(result)))
            }
        }
    }

    fn shot(&mut self, account: Option<String>, output: &Path) -> Result<Outcome, CliError> {
        let account = self.account_name(account)?;
        let vm = self.resolve_vm(None)?;
        let result = self.connect(&vm)?.call(Shot {
            account: account.clone(),
        })?;
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
                "saved {} ({} bytes) from account {account}",
                output.display(),
                png.len()
            ),
            json!({ "account": account, "path": output, "bytes": png.len() }),
        ))
    }

    fn control(&mut self, account: Option<String>, args: &[String]) -> Result<Outcome, CliError> {
        // What the desktop can do is `tilo action list`'s to say, in the account's session.
        if let [actions, list, filter @ ..] = args
            && actions == "actions"
            && list == "list"
        {
            let vm = self.resolve_vm(None)?;
            let command: Vec<String> = ["tilo", "action", "list"]
                .into_iter()
                .map(str::to_owned)
                .chain(filter.iter().cloned())
                .collect();
            return self.account_exec(
                &vm,
                account,
                tilo_device_protocol::EXEC_TIMEOUT_SECONDS,
                &command,
            );
        }
        let account = self.account_name(account)?;
        let vm = self.resolve_vm(None)?;
        let result = self.connect(&vm)?.call(Control {
            account,
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
                account,
                type_text,
                maximum_launch_ms,
                maximum_control_ms,
                timeout_seconds,
                ..
            } => self.app_flight(
                account,
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
        account: Option<String>,
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
        let account = self.account_name(account)?;
        let vm = self.resolve_vm(None)?;
        let mut remote = self.connect(&vm)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| flight_failure("flight timeout is too large"))?;
        let mut worst_control = Duration::ZERO;
        let baseline = control_json(&mut remote, &account, windows_args(), &mut worst_control)?;
        let baseline_ids = window_ids(&baseline);

        let launched_at = Instant::now();
        let opened = control_json(
            &mut remote,
            &account,
            &["raw".into(), open_app_frame(desktop_id)],
            &mut worst_control,
        )?;
        let status = opened
            .pointer("/message/Receipt/status")
            .ok_or_else(|| flight_failure(format!("opening {desktop_id} answered {opened}")))?;
        if status.get("Rejected").is_some() || status.get("Failed").is_some() {
            return Err(flight_failure(format!(
                "opening {desktop_id} was refused: {status}"
            )));
        }

        let (surface_id, initial_frame_seq) = loop {
            ensure_before(deadline, "app did not present before the flight deadline")?;
            let value = control_json(&mut remote, &account, windows_args(), &mut worst_control)?;
            if let Some(frame) = matching_presented_window(&value, app_id, &baseline_ids) {
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

        let value = control_json(&mut remote, &account, windows_args(), &mut worst_control)?;
        let rect = matching_window(&value, surface_id)
            .and_then(|frame| frame.rect)
            .ok_or_else(|| flight_failure(format!("surface {surface_id} has no focusable rect")))?;
        control_json(
            &mut remote,
            &account,
            &[
                "click-at".into(),
                rect.center_x().to_string(),
                rect.center_y().to_string(),
            ],
            &mut worst_control,
        )?;
        control_json(
            &mut remote,
            &account,
            &["type".into(), type_text.into()],
            &mut worst_control,
        )?;

        let final_frame_seq = loop {
            ensure_before(
                deadline,
                "focused app did not present a frame after injected input",
            )?;
            let value = control_json(&mut remote, &account, windows_args(), &mut worst_control)?;
            if let Some(frame) = matching_window(&value, surface_id)
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
            "account": account,
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

    fn input(
        &mut self,
        account: Option<String>,
        command: InputCommand,
    ) -> Result<Outcome, CliError> {
        let actions = input_actions(command)?;
        let account = self.account_name(account)?;
        let vm = self.resolve_vm(None)?;
        let result = self.connect(&vm)?.call(Input {
            account: account.clone(),
            actions: actions.clone(),
        })?;
        let what = actions
            .iter()
            .map(describe_input)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(Outcome::new(
            format!(
                "{what} on account {account} (seat {}, {} input events through its uinput devices)",
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
            "{verb} {} layers ({}) and {} generations; keeping the current and newest {keep} per account",
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

/// The image a VM moves onto: its reference, what was checked about it, its OCI layout on this
/// host and the manifest digest bootc must stage.
struct Onto<'a> {
    image: &'a str,
    facts: &'a imageup::ImageFacts,
    layout: &'a Path,
    digest: String,
}

/// Every file of an OCI layout, relative to it.
fn layout_files(layout: &Path) -> Result<Vec<PathBuf>, CliError> {
    fn walk(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(root, &path, files)?;
            } else if let Ok(relative) = path.strip_prefix(root) {
                files.push(relative.to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(layout, layout, &mut files).map_err(CliError::io(format!("read {}", layout.display())))?;
    files.sort();
    Ok(files)
}

/// A VM lock held by other work, as the person reads it.
fn busy_vm(vm: &Resolved, error: CliError) -> CliError {
    match error {
        CliError::Remote(rpc) if rpc.code == tilo_device_protocol::code::CONFLICT => {
            CliError::Busy {
                what: format!("VM {}", vm.name),
            }
        }
        other => other,
    }
}

/// Which accounts keep layers built on the image before, when the device recorded this boot's.
fn image_change_of_this_boot(remote: &mut Remote) -> Option<tilo_device_protocol::ImageChange> {
    let status = remote.refresh().ok()?.clone();
    let booted = status.boot.booted_image?;
    if status.accounts_image.as_deref() != Some(booted.as_str()) {
        return None;
    }
    match status.image_change {
        Some(change) if change.to == booted => Some(change),
        _ => Some(tilo_device_protocol::ImageChange {
            from: None,
            to: booted,
            at: String::new(),
            kept: Vec::new(),
            held: Vec::new(),
            dropped: Vec::new(),
            freed_bytes: 0,
        }),
    }
}

fn checkout_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| d.join("t.toml").is_file() && d.join("os").is_dir())
        .map(Path::to_path_buf)
}

#[derive(Debug, Clone, Default)]
pub struct KernelFlags {
    pub direct: bool,
    pub jobs: Option<u32>,
    pub cmdline: Option<String>,
}

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
    if !tilo_device_protocol::manifest::valid_profile(profile) {
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

impl App {
    /// [`App::join_lan`] by the host's own tools, saying so among the warnings.
    fn join_lan_now(
        &mut self,
        adapter: &dyn Hypervisor,
        vm: Resolved,
    ) -> Result<Resolved, CliError> {
        let mut runner = SystemRunner {
            cwd: self.runner.cwd.clone(),
        };
        let (vm, joined) = self.join_lan(&mut runner, adapter, vm)?;
        self.warnings.extend(joined);
        Ok(vm)
    }
}

/// Whether a VM in `registry` has `mac` on the host network ([`crate::lan`]).
pub(crate) fn lan_taken(registry: &Registry, mac: &str) -> bool {
    registry
        .vms
        .values()
        .any(|entry| entry.lan.as_deref() == Some(mac))
}

impl App {
    /// Put `vm` on the network this host's VMs share before it boots, when its adapter has one
    /// and the VM, made before it, has no NIC there yet: give it a MAC, record it, and redefine
    /// the VM with it. A VM tilo-vm did not make keeps its definition, and `lan = "off"` none.
    /// Says so when it joined.
    pub(crate) fn join_lan(
        &self,
        runner: &mut dyn Runner,
        adapter: &dyn Hypervisor,
        vm: Resolved,
    ) -> Result<(Resolved, Option<String>), CliError> {
        let own = sshkey::key_path(&self.registry_path, &vm.name);
        if !adapter.joins_lan()
            || vm.entry.lan.is_some()
            || vm.entry.key.as_deref().map(Path::new) != Some(own.as_path())
        {
            return Ok((vm, None));
        }
        let mut registry = self.registry()?;
        let mac = crate::lan::new_mac(|mac| lan_taken(&registry, mac));
        let mut entry = vm.entry.clone();
        entry.lan = Some(mac.clone());
        let root_key = sshkey::read_public(&own)?;
        let joined = Resolved { entry, ..vm };
        let define = adapter.redefine(&joined.vm(), &root_key, runner)?;
        run_all(runner, &define)?;
        registry
            .vms
            .insert(joined.name.clone(), joined.entry.clone());
        registry.save(&self.registry_path)?;
        let note = format!(
            "{} joined {} as {}: it was made before that network",
            joined.name,
            crate::lan::describe(),
            crate::lan::address(&mac).map_or_else(String::new, |ip| ip.to_string())
        );
        Ok((joined, Some(note)))
    }
}

fn entry_image(registry: &Registry, name: &str) -> Option<String> {
    registry.vms.get(name).and_then(|e| e.image.clone())
}

pub fn describe_tools(tools: &VmTools, ci_from_host: bool) -> String {
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
    let ci = if ci_from_host {
        "CI layers extracted on this host, only the layer sent"
    } else if tools.podman {
        "CI layers pulled in the VM"
    } else {
        "CI layers pulled on this host"
    };
    format!("{build}; {ci}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopStep {
    Acpi,
    GuestPoweroff,
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

/// Bring the VM down: wait for the clean poweroff its device was asked for (`asked`), then the
/// hypervisor's ACPI request, then a hard power-off.
pub fn stop_escalating(
    runner: &mut dyn Runner,
    adapter: &dyn Hypervisor,
    vm: &Vm<'_>,
    asked: bool,
    graces: StopGraces,
) -> Result<(Option<bool>, StopStep), CliError> {
    if asked {
        let down = await_down(runner, adapter, vm, graces.guest, graces.poll);
        if down != Some(false) {
            return Ok((down, StopStep::GuestPoweroff));
        }
    }
    run_all(runner, &adapter.stop(vm))?;
    let mut down = await_down(runner, adapter, vm, graces.acpi, graces.poll);
    let mut step = StopStep::Acpi;
    if down == Some(false)
        && let Some(off) = adapter.power_off(vm)
    {
        step = StopStep::HardPowerOff;
        run_all(runner, &[off])?;
        down = await_down(runner, adapter, vm, graces.hard, graces.poll);
    }
    Ok((down, step))
}

fn await_down(
    runner: &mut dyn Runner,
    adapter: &dyn Hypervisor,
    vm: &Vm<'_>,
    limit: Duration,
    poll: Duration,
) -> Option<bool> {
    let started = Instant::now();
    adapter.running(vm, runner)?;
    Some(loop {
        match adapter.running(vm, runner) {
            Some(true) if started.elapsed() < limit => std::thread::sleep(poll),
            Some(true) => break false,
            _ => break true,
        }
    })
}

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
            "one app from this worktree: tilo-vm swap app:<dir>; \
             apps with their matching runtime: tilo-vm swap apps+runtime; the published set: \
             tilo-vm swap {} --from ci:<tag>; this worktree's image: tilo-vm vm upgrade --from local",
            component.name
        ),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppsRuntimeBuild {
    Remote,
    Local,
}

impl AppsRuntimeBuild {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Local => "host",
        }
    }
}

pub fn plan_apps_runtime_build(
    builder: Option<Builder>,
    device_arch: &str,
    host_arch: &str,
    linux_host: bool,
    remote_configured: bool,
) -> Result<AppsRuntimeBuild, CliError> {
    if builder == Some(Builder::Vm) {
        return Err(CliError::Unsupported {
            what: "a VM-only cargo build for the paired apps+runtime assembly".into(),
            alternative: "tilo-vm swap apps+runtime --builder host (a Linux host of the device's architecture) or --builder remote".into(),
        });
    }
    plan_local_or_remote(
        "apps+runtime",
        builder,
        device_arch,
        host_arch,
        linux_host,
        remote_configured,
    )
}

pub fn plan_device_app_build(
    dir: &str,
    builder: Option<Builder>,
    device_arch: &str,
    host_arch: &str,
    linux_host: bool,
    remote_configured: bool,
) -> Result<Builder, CliError> {
    if builder == Some(Builder::Vm) {
        return Err(CliError::Unsupported {
            what: "building an app on the physical device itself".into(),
            alternative: format!(
                "tilo-vm swap app:{dir} --builder host (a Linux host of the device's architecture) or --builder remote"
            ),
        });
    }
    let what = format!("app:{dir}");
    Ok(
        match plan_local_or_remote(
            &what,
            builder,
            device_arch,
            host_arch,
            linux_host,
            remote_configured,
        )? {
            AppsRuntimeBuild::Local => Builder::Host,
            AppsRuntimeBuild::Remote => Builder::Remote,
        },
    )
}

fn plan_local_or_remote(
    what: &str,
    builder: Option<Builder>,
    device_arch: &str,
    host_arch: &str,
    linux_host: bool,
    remote_configured: bool,
) -> Result<AppsRuntimeBuild, CliError> {
    let local_possible = linux_host && host_arch == device_arch;
    match builder {
        Some(Builder::Host) if local_possible => Ok(AppsRuntimeBuild::Local),
        Some(Builder::Host) => Err(CliError::Unsupported {
            what: format!(
                "building {what} on this {} host for a {device_arch} device",
                if linux_host { host_arch } else { "non-Linux" }
            ),
            alternative: format!(
                "it must be built on a Linux machine of the device's architecture: tilo-vm swap {what} --builder remote"
            ),
        }),
        None if !remote_configured && local_possible => Ok(AppsRuntimeBuild::Local),
        Some(Builder::Remote | Builder::Vm) | None => Ok(AppsRuntimeBuild::Remote),
    }
}

pub fn check_app_lock(
    worktree_lock: Option<&str>,
    image_lock: Option<&str>,
) -> Result<(), CliError> {
    match (worktree_lock, image_lock) {
        (Some(mine), Some(image)) if mine != image => Err(CliError::Unsupported {
            what: format!(
                "app:<dir> from a worktree whose Cargo.lock ({}) is not the image's ({}): the app would link the image's libruntime_dylib.so, whose symbols carry crate hashes",
                short(mine),
                short(image)
            ),
            alternative: "build the apps with their matching runtime as one cohort: tilo-vm swap apps+runtime (it is a generation, so tilo-vm rollback undoes it); never regenerate Cargo.lock to match".into(),
        }),
        _ => Ok(()),
    }
}

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
        Some(Builder::Remote) => Ok((Builder::Remote, None)),
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

pub fn input_actions(command: InputCommand) -> Result<Vec<InputAction>, CliError> {
    let action = match command {
        InputCommand::Key { combos } => InputAction::Key { combos },
        InputCommand::Type { text, stdin } => {
            if stdin {
                return stdin_input_actions(std::io::stdin().lock());
            }
            InputAction::Type {
                text: text.ok_or_else(|| CliError::InvalidInput("no text source given".into()))?,
            }
        }
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
        InputCommand::Hold { combo } => InputAction::KeyDown { combo },
        InputCommand::Release { combo } => InputAction::KeyUp { combo },
        InputCommand::Device(DeviceCommand::Plug { profile }) => {
            InputAction::DevicePlug { profile }
        }
        InputCommand::Device(DeviceCommand::Unplug { profile }) => {
            InputAction::DeviceUnplug { profile }
        }
        InputCommand::Touchpad(TouchpadCommand::Tap { fingers }) => {
            InputAction::TouchpadTap { fingers }
        }
        InputCommand::Touchpad(TouchpadCommand::Swipe {
            dx,
            dy,
            fingers,
            duration,
            press,
        }) => InputAction::TouchpadSwipe {
            fingers,
            dx_mm: dx,
            dy_mm: dy,
            duration_ms: duration,
            press,
        },
        InputCommand::Touchpad(TouchpadCommand::Click) => InputAction::TouchpadClick,
    };
    let actions = vec![action];
    tilo_device_protocol::input::validate(&actions).map_err(CliError::InvalidInput)?;
    Ok(actions)
}

/// Stdin is deliberately bounded and its failures never quote supplied bytes.
fn stdin_input_actions(reader: impl std::io::Read) -> Result<Vec<InputAction>, CliError> {
    use std::io::Read as _;
    const LIMIT: u64 = 65_536;
    let mut bytes = Vec::new();
    reader
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CliError::InvalidInput("could not read input text from stdin".into()))?;
    if bytes.len() as u64 > LIMIT {
        return Err(CliError::InvalidInput(
            "stdin input exceeds 65536 bytes".into(),
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| CliError::InvalidInput("stdin input must be valid UTF-8".into()))?;
    let actions = vec![InputAction::Type { text }];
    tilo_device_protocol::input::validate(&actions).map_err(|_| {
        CliError::InvalidInput("stdin input cannot be typed on a US keyboard layout".into())
    })?;
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
        InputAction::KeyDown { combo } => format!("holding {combo}"),
        InputAction::KeyUp { combo } => format!("released {combo}"),
        InputAction::DevicePlug { profile } => format!("plugged in {profile}"),
        InputAction::DeviceUnplug { profile } => format!("unplugged {profile}"),
        InputAction::TouchpadTap { fingers } => {
            format!("tapped the touchpad with {fingers} finger(s)")
        }
        InputAction::TouchpadSwipe {
            fingers,
            dx_mm,
            dy_mm,
            press,
            ..
        } => format!(
            "{} {fingers} finger(s) {dx_mm},{dy_mm} mm on the touchpad",
            if *press { "dragged" } else { "moved" }
        ),
        InputAction::TouchpadClick => "clicked the touchpad".to_owned(),
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
    Outcome::new(text, json!(json)).next(&[&format!("tilo-vm account apps install --set {first}")])
}

fn account_created_text(
    name: &str,
    kind: AccountKind,
    result: &tilo_device_protocol::AccountCreateResult,
) -> String {
    let user = result.account.user.clone().unwrap_or_default();
    let set_outputs = result.actions.iter().find_map(|action| match action {
        tilo_device_protocol::Action::SetOutputs { outputs } => Some(outputs.clone()),
        _ => None,
    });
    let started = result
        .actions
        .iter()
        .take_while(|action| {
            !matches!(
                action,
                tilo_device_protocol::Action::ProvisionTestIdentity { .. }
                    | tilo_device_protocol::Action::CompleteOnboarding { .. }
            )
        })
        .any(|action| !matches!(action, tilo_device_protocol::Action::SetOutputs { .. }));
    let text = if result.created {
        format!(
            "account {name} created ({}); session started as {user} on the base image",
            kind.as_str()
        )
    } else if started {
        format!("account {name} already existed; its session was not running and has been started")
    } else if set_outputs.is_some() {
        format!("account {name} already exists and its session is running")
    } else {
        format!("account {name} already exists and its session is running; nothing to do")
    };
    let text = match &result.identity {
        Some(identity) if !identity.throwaway => format!(
            "{text}\n{}; fs runs on the machine's certified device key",
            if identity.created {
                "its user is this VM's person, made through onboarding; the session restarted with fresh desktop state"
            } else {
                "its user is this VM's person"
            }
        ),
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

fn account_line(info: &AccountInfo) -> String {
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
            tilo_device_protocol::display::format_outputs(&info.outputs)
        )
    };
    format!(
        "account: {} ({}) generation {}, session {}{seat}{outputs}{owner}",
        info.name,
        info.kind.as_str(),
        info.generation,
        info.session
    )
}

fn missing_account(name: &str, accounts: &[AccountInfo]) -> (Vec<String>, Vec<String>) {
    if name == tilo_device_protocol::DEVICE_ACCOUNT {
        return (
            vec![format!(
                "account: {name} (none here: no board session, and nobody is logged in to Tilo on the display)"
            )],
            vec![
                "log in at the machine, then: tilo-vm status --account device".into(),
                "tilo-vm guide devices".into(),
            ],
        );
    }
    let others: Vec<&AccountInfo> = accounts
        .iter()
        .filter(|s| s.kind != AccountKind::Vm)
        .collect();
    if others.is_empty() {
        return (
            vec![format!(
                "account: {name} (not created yet; new here? tilo-vm guide quickstart)"
            )],
            vec![
                "tilo-vm account create --headless".into(),
                "tilo-vm guide quickstart".into(),
            ],
        );
    }
    let mut lines = vec![format!(
        "account: {name} does not exist; the VM has these (use one with --account <name>, or set TILO_VM_ACCOUNT):"
    )];
    lines.extend(others.iter().map(|s| format!("  {}", account_line(s))));
    let mut next: Vec<String> = others
        .iter()
        .take(3)
        .map(|s| format!("tilo-vm status --account {}", s.name))
        .collect();
    next.push("tilo-vm account list".into());
    (lines, next)
}

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

pub fn scenario_driver(entry: &Component) -> String {
    match entry.name.as_str() {
        "installer" => {
            "tilo-vm scenario install   (a throwaway VM installs from your medium through the installer's actions and cold-boots the result; the release check is os/dev/test-installer-cycle.sh)".into()
        }
        _ => entry
            .scenario
            .clone()
            .unwrap_or_else(|| "os/dev/vm.sh boot".into()),
    }
}

fn named_account(command: &Command) -> Option<String> {
    match command {
        Command::SwapArtifact(request) => request.account.clone(),
        Command::Swap { account, .. }
        | Command::Input { account, .. }
        | Command::Control { account, .. }
        | Command::Flight(FlightCommand::App { account, .. })
        | Command::Rollback { account, .. }
        | Command::Logs { account, .. }
        | Command::Shot { account, .. }
        | Command::Status { account }
        | Command::Seat(
            SeatCommand::Claim { account, .. } | SeatCommand::Release { account, .. },
        )
        | Command::Account(
            AccountCommand::Create { name: account, .. }
            | AccountCommand::Restart { name: account, .. }
            | AccountCommand::Rm { name: account, .. }
            | AccountCommand::Show { name: account, .. }
            | AccountCommand::Exec { name: account, .. }
            | AccountCommand::Apps(AccountAppsCommand::Install { name: account, .. }),
        ) => account.clone(),
        _ => None,
    }
}

pub fn with_account(command: &str, account: &str) -> String {
    let words: Vec<&str> = command.split(' ').collect();
    if words.first() != Some(&"tilo-vm") || command.contains("--account") {
        return command.to_owned();
    }
    let at = match (words.get(1).copied(), words.get(2).copied()) {
        (Some("swap" | "shot" | "control" | "logs" | "rollback" | "input" | "status"), _) => 2,
        (Some("account"), Some("create" | "restart" | "rm" | "show" | "exec"))
        | (Some("seat"), Some("claim" | "release")) => 3,
        _ => return command.to_owned(),
    };
    let Some((head, tail)) = words.split_at_checked(at) else {
        return command.to_owned();
    };
    let flag = format!("--account {account}");
    let mut out: Vec<&str> = head.to_vec();
    out.push(&flag);
    out.extend_from_slice(tail);
    out.join(" ")
}

pub fn forward_target(account: &str, info: Option<&AccountInfo>) -> Result<u64, CliError> {
    let info = info.ok_or_else(|| CliError::Unsupported {
        what: format!("rolling account {account} forward: the VM has no such account"),
        alternative: "tilo-vm account list".into(),
    })?;
    info.generations
        .iter()
        .copied()
        .find(|n| *n > info.generation)
        .ok_or_else(|| CliError::Unsupported {
            what: format!(
                "rolling account {account} forward: generation {} is its newest",
                info.generation
            ),
            alternative: format!(
                "pick one by number: tilo-vm rollback --account {account} --to <n>   (tilo-vm account show --account {account} lists them)"
            ),
        })
}

#[derive(Clone, Copy)]
struct FlightBudget {
    maximum_launch: Duration,
    maximum_control: Duration,
    timeout: Duration,
}

fn validate_frame_cpu_component(component: &str, enabled: bool) -> Result<(), CliError> {
    if enabled && component != "shell" {
        return Err(CliError::InvalidInput(
            "--frame-cpu-trace is supported only for the shell component".into(),
        ));
    }
    Ok(())
}

fn frame_cpu_info(
    entry: Option<Component>,
    enabled: bool,
) -> Result<Option<tilo_device_protocol::ComponentInfo>, CliError> {
    let mut info = entry.map(|entry| entry.info);
    if enabled {
        let shell = info.as_mut().ok_or_else(|| {
            CliError::InvalidInput("--frame-cpu-trace requires the shell component manifest".into())
        })?;
        shell.frame_cpu_trace = true;
    }
    Ok(info)
}

#[derive(serde::Serialize)]
struct InstalledAccountReceipt<'a> {
    #[serde(flatten)]
    shown: &'a AccountShowResult,
    installed_builds: BTreeMap<String, Option<&'a tilo_device_protocol::LayerInfo>>,
    identity_scope: &'static str,
}

fn installed_account_outcome(
    shown: &AccountShowResult,
    layers: &[tilo_device_protocol::LayerInfo],
) -> Outcome {
    let installed_builds = installed_builds(shown, layers);
    let mut text = describe_show(shown);
    text.push_str("\n  build receipts: installed layers; running process image unverified");
    for (component, layer) in &installed_builds {
        let receipt = layer.map_or_else(
            || "unavailable".into(),
            |layer| {
                let p = &layer.provenance;
                format!(
                    "source={} toolchain={} profile={} builder={}",
                    p.source_commit.as_deref().unwrap_or("unavailable"),
                    p.toolchain.as_deref().unwrap_or("unavailable"),
                    p.cargo_profile.as_deref().unwrap_or("unavailable"),
                    p.builder.as_deref().unwrap_or("unavailable")
                )
            },
        );
        let _ = write!(text, "\n    {component}: {receipt}");
    }
    let data = json!(InstalledAccountReceipt {
        shown,
        installed_builds,
        identity_scope: "installed_layer_not_running_process",
    });
    Outcome::new(text, data).next(&["tilo-vm --json account show"])
}

/// Build receipts describe immutable installed layers, never process memory.
fn installed_builds<'a>(
    shown: &AccountShowResult,
    layers: &'a [tilo_device_protocol::LayerInfo],
) -> BTreeMap<String, Option<&'a tilo_device_protocol::LayerInfo>> {
    shown
        .account
        .components
        .iter()
        .map(|(component, hash)| {
            (
                component.clone(),
                layers.iter().find(|layer| &layer.hash == hash),
            )
        })
        .collect()
}

fn describe_show(shown: &AccountShowResult) -> String {
    let info = &shown.account;
    let mut lines = vec![
        account_line(info),
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
        lines.push("  no layers: the account sees the VM's image as it is".into());
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

fn describe_kept(result: &tilo_device_protocol::GcResult) -> Option<String> {
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

fn build_dirs(root: &Path) -> Vec<PathBuf> {
    let listing = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let mut dirs = worktree_targets(&listing);
    if dirs.is_empty() {
        dirs.push(root.join("target"));
    }
    let cache = std::env::var_os("SCCACHE_DIR").map_or_else(
        || PathBuf::from("/var/cache/tilo-vm/sccache"),
        PathBuf::from,
    );
    if cache.is_dir() {
        dirs.push(cache);
    }
    dirs.retain(|dir| dir.exists());
    dirs
}

fn worktree_targets(listing: &str) -> Vec<PathBuf> {
    listing
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| Path::new(path).join("target"))
        .collect()
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

fn merge_error(data: &mut Value, error: &CliError) {
    if let (Value::Object(map), Value::Object(fields)) = (data, error.to_json()) {
        map.extend(fields);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectFailure {
    pub state: &'static str,
    pub summary: String,
    pub ssh: bool,
    pub next: Vec<String>,
}

impl ConnectFailure {
    fn text(&self, error: &CliError) -> String {
        if self.state == "off"
            || (self.state == "asleep" && !matches!(error, CliError::DeviceAsleep { .. }))
        {
            return format!("  {}", self.summary);
        }
        format!(
            "  {}\n  error: {error}\n  hint: {}",
            self.summary,
            error.hint()
        )
    }
}

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
    let no_ssh = || {
        let mut next = debug.to_vec();
        next.push("tilo-vm doctor".into());
        next
    };
    match error {
        CliError::DeviceAsleep { wakeable, .. } => fail(
            "asleep",
            "asleep or gone: it does not answer at its address, nor by name".into(),
            false,
            if *wakeable {
                vec![format!("tilo-vm vm wake {vm}")]
            } else {
                vec![format!("tilo-vm vm status {vm}   (once woken by hand)")]
            },
        ),
        CliError::NotJoined { .. } => fail(
            "not-joined",
            "not known as a device yet: it joins this machine's person at its next start".into(),
            false,
            vec![start],
        ),
        CliError::NotLoggedIn { .. } => fail(
            "no-login",
            "this computer has no Tilo login to ask it as".into(),
            false,
            vec!["tilo-vm guide devices".into()],
        ),
        CliError::NotGranted { .. } => fail(
            "not-granted",
            "running, but it does not let this machine act on it".into(),
            true,
            vec!["tilo-vm guide devices".into()],
        ),
        CliError::DeviceTooOld { .. } => fail(
            "outdated",
            "running, but its image predates a step this tilo-vm asks for".into(),
            true,
            vec![format!("tilo-vm vm upgrade {vm}")],
        ),
        _ if running == Some(false) => fail(
            "off",
            "shut off (the hypervisor reports it is not running)".into(),
            false,
            vec![start],
        ),
        CliError::Unreachable { .. } if running == Some(true) => fail(
            "no-answer",
            "running, but its device does not answer (still booting, or stuck)".into(),
            false,
            no_ssh(),
        ),
        CliError::Unreachable { .. } => {
            let mut next = vec![format!("{start}   (if it is off)")];
            next.extend(no_ssh());
            fail(
                "no-answer",
                "its device does not answer (the hypervisor cannot say whether it is running)"
                    .into(),
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
    if adapter.name() == hypervisor::DEVICE {
        return CliError::Unsupported {
            what: format!("{what} on a device"),
            alternative: "a device has no hypervisor, only its own daemon: tilo-vm shot is an account compositor's own frame (on a laptop, tilo-vm seat claim first puts your account on its display), tilo-vm logs its journal, tilo-vm vm ssh a shell, tilo-vm vm restart a reboot (tilo-vm guide devices)".into(),
        };
    }
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
                let cache = tilo_paths::cache_dir().unwrap_or_else(std::env::temp_dir);
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

fn wait_for_session(remote: &mut Remote, account: &str) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        let active = remote
            .call(Status {
                account: Some(account.to_owned()),
            })
            .ok()
            .and_then(|s| s.accounts.into_iter().next())
            .is_some_and(|s| s.session == "active");
        if active {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn swap_transport<T>(
    adapter: &dyn Hypervisor,
    vm: &Vm<'_>,
    runner: &mut dyn Runner,
    transport: impl FnOnce() -> Result<T, CliError>,
) -> Result<T, CliError> {
    if adapter.name() != hypervisor::DEVICE && adapter.suspended(vm, runner) == Some(true) {
        let commands = adapter.wake(vm).ok_or_else(|| CliError::Unsupported {
            what: format!("waking suspended VM {} with {}", vm.name, adapter.name()),
            alternative: "resume the VM in the hypervisor's UI before swapping".into(),
        })?;
        run_all(runner, &commands)?;
    }
    transport()
}

#[cfg(test)]
mod swap_wake_tests;

#[cfg(test)]
mod branch_tests;

#[cfg(test)]
mod host_layer_tests;

#[cfg(test)]
mod tests {
    use super::*;

    mod input_commands;

    tilo_test_env::scenario! {
        fn machines_images_and_layers_are_checked_before_use;

        phase the_checkout_t_builds_from_is_found_above_the_working_directory;
        {
            let tree = tempfile::tempdir().unwrap();
            std::fs::write(tree.path().join("t.toml"), "").unwrap();
            std::fs::create_dir_all(tree.path().join("os")).unwrap();
            let deep = tree.path().join("tools/tilo-vm");
            std::fs::create_dir_all(&deep).unwrap();
            assert_eq!(checkout_root(&deep).as_deref(), Some(tree.path()));
            assert_eq!(checkout_root(Path::new("/")), None);
        }

        phase a_device_is_registered_by_its_device_key_once_it_answers;
        {
            let device = crate::remote::fixture::Device::serving(|action, _| {
                Err(tilo_device_protocol::RpcError {
                    code: tilo_device_protocol::code::JOB,
                    message: format!("this device serves no {action}"),
                    hint: String::new(),
                    see: None,
                })
            });
            let address = device.route.addresses[0].clone();
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            app.machine = Some(device.machine.clone());

            {
                let added = app.vm_add(&add("opi", &device.route.device, &address)).unwrap();
                assert!(!added.failed, "{}", added.text);
                let registry = app.registry().unwrap();
                let entry = &registry.vms["opi"];
                assert_eq!(entry.hypervisor, "device");
                let route = entry.device.as_ref().unwrap();
                assert_eq!(route.device, device.route.device);
                assert_eq!(route.workspace, device.machine.workspace);
                assert_eq!(route.addresses, std::slice::from_ref(&address));
                assert_eq!(entry.arch.as_deref(), Some(std::env::consts::ARCH));
                assert_eq!(registry.default.as_deref(), Some("opi"));
                assert!(added.next.contains(&"tilo-vm vm use opi".to_owned()));
            }
            {
                let again = app.vm_add(&add("opi", &device.route.device, &address)).unwrap();
                assert!(again.text.contains("already registered as opi"), "{}", again.text);
                let error = app
                    .vm_add(&crate::cli::AddArgs {
                        key: Some(dir.path().join("missing")),
                        ..add("pi2", &device.route.device, &address)
                    })
                    .err()
                    .unwrap();
                assert!(error.to_string().contains("no such file"), "{error}");
                let error = app.vm_add(&add("pi2", "3f2a", &address)).err().unwrap();
                assert!(error.to_string().contains("64 hex digits"), "{error}");
                assert!(
                    app.vm_add(&crate::cli::AddArgs {
                        ssh: Some("root@".into()),
                        ..add("pi2", &device.route.device, &address)
                    })
                    .is_err()
                );
                let error = app
                    .vm_create(CreateArgs {
                        hypervisor: Some("device".into()),
                        ..create_args("pi3")
                    })
                    .err()
                    .unwrap();
                assert!(
                    error.hint().contains("tilo-vm vm add pi3"),
                    "{}",
                    error.hint()
                );
            }
            {
                // Another device's key at that address: nothing answers as it, so nothing is
                // registered.
                let other = app.vm_add(&add("pi2", &"ab".repeat(32), &address)).unwrap();
                assert!(other.failed, "{}", other.text);
                assert!(other.text.contains("not registered"), "{}", other.text);
                assert!(!app.registry().unwrap().vms.contains_key("pi2"));
            }
            {
                let adapter = hypervisor::adapter("device").unwrap();
                assert!(!adapter.powers_on());
                assert!(hypervisor::adapter("libvirt").unwrap().powers_on());
                let vm = app.resolve_vm(Some("opi")).unwrap();
                assert!(adapter.start(&vm.vm()).is_empty());
            }
            {
                let mut runner = SystemRunner { cwd: None };
                let removed = app.vm_rm(&mut runner, "opi", false).unwrap();
                assert!(
                    removed.text.contains("the device itself is untouched"),
                    "{}",
                    removed.text
                );
                assert!(app.registry().unwrap().vms.is_empty());
            }
        }

        phase qemu_accepts_display_fixture_options_but_not_multiple_heads;
        {
            let mut args = create_args("dev");
            args.resolution = Some("1920x1080".into());
            args.gl = true;
            assert!(App::validate_create_options("qemu", &args).is_ok());
            assert!(App::validate_create_options("libvirt", &args).is_ok());
            for adapter in ["whpx", "utm", "hyperv"] {
                assert!(App::validate_create_options(adapter, &args).is_err());
            }
            args.sound = true;
            args.vulkan = true;
            assert!(App::validate_create_options("qemu", &args).is_ok());
            assert!(App::validate_create_options("libvirt", &args).is_ok());
            assert!(App::validate_create_options("utm", &args).is_err());
            assert!(
                <crate::cli::Cli as clap::Parser>::try_parse_from(["tilo-vm", "vm", "create", "v", "--vulkan"]).is_err(),
                "--vulkan without --gl has no GPU to render on"
            );
            args.sound = false;
            args.vulkan = false;
            args.heads = Some(2);
            assert!(App::validate_create_options("qemu", &args).is_err());
            assert!(App::validate_create_options("libvirt", &args).is_ok());
        }

        phase published_boot_json_cannot_redirect_to_an_unverified_kernel;
        {
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

        phase preflight_checks_hypervisor_image_disks_and_worktree_rules;
        {
            // Explicit TCG must work without KVM, whether libvirt is selected
            // directly or by the Linux default. Native creation still fails.
            {
                use crate::host::HostKind;
                use crate::host::tests::{LIBVIRT, linux};
                let dir = tempfile::tempdir().unwrap();
                let mut app = app_with_empty_registry(dir.path());
                std::fs::write(dir.path().join("vms.toml"), "min_free_gb = 0\n").unwrap();
                let mut facts = linux(HostKind::Dnf, LIBVIRT);
                facts.kvm = Some(false);
                facts.session = Some(Ok(vec![crate::host::Guest {
                    arch: "x86_64".into(),
                    domains: vec!["qemu".into()],
                }]));
                let image = dir.path().join("base.qcow2");
                std::fs::write(&image, b"QFI\xfb\0\0\0\x03").unwrap();
                let mut args = create_args("dev");
                args.image = Some(image.display().to_string());
                args.disk = Some(dir.path().join("child.qcow2").display().to_string());
                assert!(matches!(
                    app.preflight_create(&facts, &args, "x86_64"),
                    Err(CliError::NoHypervisor(_))
                ));
                args.emulated = true;
                for adapter in [None, Some("libvirt".to_owned())] {
                    args.hypervisor = adapter;
                    let plan = app.preflight_create(&facts, &args, "x86_64").unwrap();
                    assert_eq!(plan.choice.adapter, "libvirt");
                }
                facts.session = Some(Ok(vec![crate::host::Guest {
                    arch: "x86_64".into(),
                    domains: vec!["kvm".into()],
                }]));
                assert!(matches!(
                    app.preflight_create(&facts, &args, "x86_64"),
                    Err(CliError::NoHypervisor(_))
                ));
                assert!(!dir.path().join("child.qcow2").exists());
            }

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
                    PathBuf::from(sshkey::known_hosts_path(&key.display().to_string())),
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
    }

    fn result(policy: Policy, actions: Vec<Action>, changed: bool) -> ApplyResult {
        ApplyResult {
            account: "a".into(),
            generation: 4,
            changed,
            policy,
            actions,
            cohort_selection: Vec::new(),
            warnings: vec![],
            boot: None,
        }
    }

    tilo_test_env::scenario! {
        fn the_cli_explains_itself_and_its_errors;

        phase commands_describe_their_consequence_and_redirect_where_needed;
        {
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

            {
                let mut swapped = result(Policy::Reboot, vec![Action::SysextRefresh], true);
                swapped.boot = Some(tilo_device_protocol::BootChange {
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
                back.boot = Some(tilo_device_protocol::BootChange {
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

            {
                assert_eq!(
                    describe_apply(&result(Policy::SessionRestart, vec![], true)),
                    "generation 4 applied to account a; session restarted (logout, user manager restart, login)"
                );
                assert!(
                    describe_apply(&result(Policy::NoRestart, vec![], false)).contains("nothing to do")
                );
                let mut login = result(
                    Policy::SessionRestart,
                    vec![
                        Action::SysextRefresh,
                        Action::RestartDesktop { user: "me".into() },
                    ],
                    true,
                );
                login.account = "device".into();
                assert_eq!(
                    describe_apply(&login),
                    "generation 4 applied to account device; merged into the machine's /usr with \
                 systemd-sysext; me's desktop restarted in place (still logged in; apps reopen)"
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
                assert!(vm.contains("every account so they see it: b"), "{vm}");
            }

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
                for command in [
                    ssh(false, "systemctl status tilo-device-key-init --no-pager"),
                    ssh(false, "tilo-runtime --version"),
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

        phase status_and_errors_explain_what_is_wrong_and_what_to_do;
        {
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
                assert_eq!(silent.state, "no-answer");
                assert!(
                    !silent.next.iter().any(|n| n.contains("vm start")),
                    "{:?}",
                    silent.next
                );
                assert!(silent.next.contains(&"tilo-vm vm serial dev".to_owned()));
                assert!(silent.next.contains(&"tilo-vm doctor".to_owned()));
                let unknown = connect_failure("dev", None, &debug, &unreachable);
                assert!(unknown.next[0].starts_with("tilo-vm vm start dev"));

                let unjoined = CliError::NotJoined { vm: "dev".into() };
                let fresh = connect_failure("dev", Some(true), &debug, &unjoined);
                assert_eq!(fresh.state, "not-joined");
                assert_eq!(fresh.next, ["tilo-vm vm start dev"]);

                let refused = CliError::NotGranted {
                    vm: "dev".into(),
                    resource: "devices/ab/vm".into(),
                };
                let not_granted = connect_failure("dev", Some(true), &debug, &refused);
                assert_eq!(not_granted.state, "not-granted");
                assert!(
                    not_granted.text(&refused).contains("tilo grant devices/ab/vm"),
                    "{}",
                    not_granted.text(&refused)
                );

                let older_vm = CliError::DeviceTooOld {
                    vm: "dev".into(),
                    action: "tree-sync".into(),
                };
                let outdated = connect_failure("dev", Some(true), &debug, &older_vm);
                assert_eq!(outdated.state, "outdated");
                assert_eq!(outdated.next, ["tilo-vm vm upgrade dev"]);
                assert!(!outdated.next.iter().any(|n| n.contains("vm start")));

                let no_login = CliError::NotLoggedIn {
                    detail: "no identity".into(),
                };
                assert_eq!(
                    connect_failure("dev", Some(true), &debug, &no_login).state,
                    "no-login"
                );
            }

            {
                struct Guest {
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
                        if line.contains("Get-Process -Id") && !line.contains("TcpClient") {
                            return Ok(crate::hypervisor::Output {
                                success: true,
                                status: "exit status: 0".into(),
                                stdout: if self.down { "no" } else { "yes" }.into(),
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
                let stop = |obeys: &'static str, asked: bool| {
                    let mut guest = Guest {
                        obeys,
                        ran: Vec::new(),
                        down: false,
                    };
                    let result =
                        stop_escalating(&mut guest, &hypervisor::Libvirt, &vm, asked, graces)
                            .unwrap();
                    (result, guest.ran)
                };
                let acpi = "virsh --connect qemu:///session shutdown dev";
                let hard = "virsh --connect qemu:///session destroy dev";
                assert_eq!(
                    stop("shutdown dev", false),
                    ((Some(true), StopStep::Acpi), vec![acpi.to_owned()])
                );
                assert_eq!(
                    stop("destroy dev", false),
                    (
                        (Some(true), StopStep::HardPowerOff),
                        vec![acpi.to_owned(), hard.to_owned()]
                    )
                );
                assert_eq!(
                    stop("nothing brings it down", false).0,
                    (Some(false), StopStep::HardPowerOff)
                );
                // A guest that obeys the poweroff it was asked for is never sent the ACPI event:
                // that event can suspend a guest instead of stopping it.
                let mut obeying = Guest {
                    obeys: "",
                    ran: Vec::new(),
                    down: true,
                };
                assert_eq!(
                    stop_escalating(&mut obeying, &hypervisor::Libvirt, &vm, true, graces).unwrap(),
                    (Some(true), StopStep::GuestPoweroff)
                );
                assert!(obeying.ran.is_empty(), "{:?}", obeying.ran);
                assert_eq!(
                    stop("shutdown dev", true),
                    ((Some(true), StopStep::Acpi), vec![acpi.to_owned()]),
                    "a guest that ignores its poweroff still gets the ACPI event"
                );
            }

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

            {
                use AppsRuntimeBuild::{Local, Remote};
                let plan = |b, dev, host, linux, cfg| plan_apps_runtime_build(b, dev, host, linux, cfg);
                assert_eq!(
                    plan(Some(Builder::Host), "x86_64", "x86_64", true, false).unwrap(),
                    Local
                );
                assert_eq!(plan(None, "x86_64", "x86_64", true, false).unwrap(), Local);
                assert_eq!(plan(None, "x86_64", "x86_64", true, true).unwrap(), Remote);
                assert_eq!(
                    plan(Some(Builder::Remote), "x86_64", "x86_64", true, false).unwrap(),
                    Remote
                );
                assert_eq!(
                    plan(None, "aarch64", "x86_64", true, false).unwrap(),
                    Remote
                );
                for refused in [
                    plan(Some(Builder::Host), "aarch64", "x86_64", true, true),
                    plan(Some(Builder::Host), "x86_64", "x86_64", false, true),
                    plan(Some(Builder::Vm), "x86_64", "x86_64", true, true),
                ] {
                    assert!(
                        matches!(refused, Err(CliError::Unsupported { .. })),
                        "{refused:?}"
                    );
                }
            }

            {
                let plan =
                    |b, dev, host, linux, cfg| plan_device_app_build("notes", b, dev, host, linux, cfg);
                assert_eq!(
                    plan(None, "aarch64", "aarch64", true, false).unwrap(),
                    Builder::Host
                );
                assert_eq!(
                    plan(Some(Builder::Host), "aarch64", "aarch64", true, true).unwrap(),
                    Builder::Host
                );
                assert_eq!(
                    plan(None, "aarch64", "aarch64", true, true).unwrap(),
                    Builder::Remote
                );
                assert_eq!(
                    plan(None, "aarch64", "x86_64", true, false).unwrap(),
                    Builder::Remote
                );
                assert_eq!(
                    plan(None, "aarch64", "aarch64", false, false).unwrap(),
                    Builder::Remote
                );
                for refused in [
                    plan(Some(Builder::Host), "aarch64", "x86_64", true, false),
                    plan(Some(Builder::Host), "aarch64", "aarch64", false, false),
                    plan(Some(Builder::Vm), "aarch64", "aarch64", true, false),
                ] {
                    let Err(CliError::Unsupported { alternative, .. }) = &refused else {
                        panic!("{refused:?}");
                    };
                    assert!(alternative.contains("app:notes"), "{alternative}");
                }
            }

            {
                check_app_lock(Some("aaaa"), Some("aaaa")).unwrap();
                check_app_lock(None, Some("aaaa")).unwrap();
                check_app_lock(Some("aaaa"), None).unwrap();
                let error = check_app_lock(Some("aaaa"), Some("bbbb")).unwrap_err();
                let CliError::Unsupported { what, alternative } = &error else {
                    panic!("{error:?}");
                };
                assert!(what.contains("Cargo.lock"), "{what}");
                assert!(alternative.contains("swap apps+runtime"), "{alternative}");
                assert!(alternative.contains("rollback"), "{alternative}");
            }

            {
                let ready = VmTools {
                    cargo: true,
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
                assert!(describe_tools(&ready, false).starts_with("in the VM"));
                assert!(
                    describe_tools(&ready, true)
                        .ends_with("CI layers extracted on this host, only the layer sent")
                );
                assert!(hypervisor::adapter("device").unwrap().ci_layers_from_host());
                assert!(
                    !hypervisor::adapter("libvirt")
                        .unwrap()
                        .ci_layers_from_host()
                );
            }
        }

        phase json_errors_have_one_top_level_shape_everywhere;
        {
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
                app.run(Some(Command::Status { account: None })).unwrap(),
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

        phase first_contact_points_at_the_quickstart_and_both_ways_to_a_vm;
        {
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
                    account: Some("a".into()),
                    account_flag: None,
                    lines: 1,
                }))
                .err()
                .unwrap();
            assert!(matches!(error, CliError::NoVm { .. }));
            assert!(error.render().contains("see: tilo-vm guide quickstart"));
        }
    }

    fn app_with_empty_registry(dir: &Path) -> App {
        App {
            json: false,
            local: false,
            target: None,
            worktree: None,
            registry_path: dir.join("vms.toml"),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(dir, vec![]),
            warnings: Vec::new(),
            machine: None,
        }
    }

    tilo_test_env::scenario! {
        fn targets_accounts_and_generations_are_resolved_and_shown;

        phase explicit_target_refuses_unknown_and_conflicting_targets;
        {
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            std::fs::write(&app.registry_path, "default = 'default-vm'\n[vms.default-vm]\nhypervisor = 'libvirt'\n[vms.review]\nhypervisor = 'libvirt'\n").unwrap();
            app.target = Some("review".into());
            let selected = app.resolve_vm(None).unwrap();
            assert_eq!(selected.name, "review");
            assert!(matches!(selected.source, Source::Argument));
            assert!(matches!(
                app.resolve_vm(Some("default-vm")),
                Err(CliError::TargetConflict { .. })
            ));
            app.target = Some("unknown".into());
            assert!(matches!(
                app.resolve_vm(None),
                Err(CliError::UnknownVm { .. })
            ));
        }

        phase accounts_are_named_hinted_and_shown_with_their_owners;
        {
            {
                assert_eq!(with_account("tilo-vm shot", "ux1"), "tilo-vm shot --account ux1");
                assert_eq!(
                    with_account("tilo-vm control state-dump --pretty", "ux1"),
                    "tilo-vm control --account ux1 state-dump --pretty"
                );
                assert_eq!(
                    with_account("tilo-vm swap <component>", "ux1"),
                    "tilo-vm swap --account ux1 <component>"
                );
                assert_eq!(
                    with_account("tilo-vm seat release", "ux1"),
                    "tilo-vm seat release --account ux1"
                );
                assert_eq!(
                    with_account("tilo-vm account exec -- sha256sum <path>", "ux1"),
                    "tilo-vm account exec --account ux1 -- sha256sum <path>"
                );
                for untouched in [
                    "tilo-vm vm shot",
                    "tilo-vm gc",
                    "tilo-vm status --account other",
                    "tilo-vm account list",
                ] {
                    assert_eq!(with_account(untouched, "ux1"), untouched);
                }
                let swap = Command::Swap {
                    component: "shell".into(),
                    account: Some("ux1".into()),
                    from: "worktree".into(),
                    builder: None,
                    profile: None,
                    direct: false,
                    jobs: None,
                    cmdline: None,
                    frame_cpu_trace: false,
                };
                assert_eq!(named_account(&swap).as_deref(), Some("ux1"));
                assert_eq!(named_account(&Command::Doctor), None);
            }

            {
                let (lines, next) = missing_account("mine", &[account_info("vm", AccountKind::Vm, 0, &[0])]);
                assert!(lines[0].contains("not created yet"));
                assert_eq!(next[0], "tilo-vm account create --headless");
                let (lines, next) = missing_account(
                    "mine",
                    &[
                        account_info("vm", AccountKind::Vm, 0, &[0]),
                        account_info("ux1", AccountKind::Headless, 2, &[0, 1, 2]),
                    ],
                );
                assert!(lines[0].contains("does not exist"), "{lines:?}");
                assert!(lines[1].contains("ux1") && lines[1].contains("owner laptop:ux-pass"));
                assert!(!next.iter().any(|n| n.contains("account create")), "{next:?}");
                assert_eq!(next[0], "tilo-vm status --account ux1");
            }

            {
                let line = account_line(&account_info("ux1", AccountKind::Headful, 1, &[0, 1]));
                assert!(line.ends_with("owner laptop:ux-pass"), "{line}");
                assert_eq!(describe_owner(&Owner::default()), None);
            }

            {
                let shown = AccountShowResult {
                    provenance: std::collections::BTreeMap::new(),
                    account: account_info("a", AccountKind::Headless, 2, &[0, 1, 2]),
                    files: vec![
                        tilo_device_protocol::AccountFile {
                            component: "shell".into(),
                            layer: "ab".repeat(32),
                            path: "/usr/bin/shell".into(),
                            sha256: Some("cd".repeat(32)),
                            link: None,
                            executable: true,
                        },
                        tilo_device_protocol::AccountFile {
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

        phase installed_build_receipts_exclude_unselected_layers_and_keep_missing_explicit;
        {
            let mut account = account_info("a", AccountKind::Headless, 2, &[0, 1, 2]);
            account.components.insert("shell".into(), "current".into());
            account.components.insert("missing".into(), "unknown".into());
            let shown = AccountShowResult {
                account,
                files: vec![],
                provenance: BTreeMap::new(),
            };
            let layers = vec![
                tilo_device_protocol::LayerInfo {
                    hash: "previous".into(),
                    provenance: tilo_device_protocol::Provenance::local("shell"),
                },
                tilo_device_protocol::LayerInfo {
                    hash: "current".into(),
                    provenance: tilo_device_protocol::Provenance::local("shell"),
                },
            ];
            let builds = installed_builds(&shown, &layers);
            assert_eq!(builds.len(), 2);
            assert_eq!(builds["shell"].unwrap().hash, "current");
            assert!(builds["missing"].is_none());
            let outcome = installed_account_outcome(&shown, &layers);
            let receipt = outcome.data;
            assert!(receipt["installed_builds"]["missing"].is_null());
            assert_eq!(
                receipt["installed_builds"]["shell"]["provenance"]["component"],
                "shell"
            );
            assert_eq!(
                receipt["identity_scope"],
                "installed_layer_not_running_process"
            );
            assert_eq!(receipt["account"]["generation"], 2);
        }

        phase the_worktree_file_can_name_the_default_account;
        {
            if std::env::var_os("TILO_VM_ACCOUNT").is_some() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let mut app = app_with_empty_registry(dir.path());
            app.worktree = Some(Worktree {
                root: dir.path().to_owned(),
                branch: Some("feature/x".into()),
            });
            assert_eq!(app.account_name(None).unwrap(), app.default_account().unwrap());
            let branch_account = app.default_account().unwrap();
            std::fs::write(
                dir.path().join(WORKTREE_FILE),
                "vm = \"dev\"\naccount = \"ux1\"\n",
            )
            .unwrap();
            assert_eq!(app.account_name(None).unwrap(), "ux1");
            assert_ne!(branch_account, "ux1");
            assert_eq!(app.account_name(Some("other".into())).unwrap(), "other");
        }

        phase generations_roll_forward_and_gc_explains_itself;
        {
            {
                let info = account_info("a", AccountKind::Headless, 1, &[0, 1, 2, 4]);
                assert_eq!(forward_target("a", Some(&info)).unwrap(), 2);
                let newest = account_info("a", AccountKind::Headless, 4, &[0, 1, 2, 4]);
                let error = forward_target("a", Some(&newest)).unwrap_err();
                assert!(error.to_string().contains("newest"), "{error}");
                assert!(error.hint().contains("--to <n>"), "{}", error.hint());
                assert!(forward_target("a", None).is_err());
            }

            {
                let mut result = tilo_device_protocol::GcResult {
                    dry_run: true,
                    removed_layers: vec![],
                    removed_generations: vec![],
                    freed_bytes: 0,
                    removed_targets: vec![],
                    freed_target_bytes: 0,
                    storage: tilo_device_protocol::Storage::default(),
                    kept_young: vec![],
                    min_age_seconds: 3600,
                };
                assert_eq!(describe_kept(&result), None);
                result.kept_young.push(tilo_device_protocol::KeptLayer {
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

            {
                assert_eq!(human_bytes(5 << 30), "5.0 GiB");
                assert_eq!(human_bytes(300 << 20), "300 MiB");
            }
        }
    }

    fn add(name: &str, device: &str, address: &str) -> crate::cli::AddArgs {
        crate::cli::AddArgs {
            name: name.into(),
            device: device.into(),
            address: Some(address.into()),
            ssh: None,
            key: None,
        }
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
            render_node: None,
            vulkan: false,
            sound: false,
            resolution: None,
            cpus: None,
            ssh: None,
            ssh_port: None,
            template: None,
            no_lan: false,
        }
    }

    fn raw_disk(path: &Path) {
        let mut bytes = vec![0_u8; 1024];
        bytes[512..520].copy_from_slice(b"EFI PART");
        std::fs::write(path, bytes).unwrap();
    }

    fn account_info(
        name: &str,
        kind: AccountKind,
        generation: u64,
        generations: &[u64],
    ) -> AccountInfo {
        AccountInfo {
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
            session_identity: None,
        }
    }

    #[test]
    fn doctor_looks_at_the_target_of_every_worktree() {
        let listing = "worktree /w/main\nHEAD abc\nbranch refs/heads/main\n\nworktree /w/wt/bar\nHEAD def\ndetached\n";
        assert_eq!(
            worktree_targets(listing),
            vec![
                PathBuf::from("/w/main/target"),
                PathBuf::from("/w/wt/bar/target")
            ]
        );
    }

    #[test]
    fn human_bytes_formats_table() {
        let cases = [
            (0, "0 MiB"),
            (1, "0 MiB"),
            (999, "0 MiB"),
            (1_000, "0 MiB"),
            (1_023, "0 MiB"),
            (1_024, "0 MiB"),
            (1_500, "0 MiB"),
            (999_999, "0 MiB"),
            (1_000_000, "0 MiB"),
            (1_048_576, "1 MiB"),
            (1_500_000, "1 MiB"),
            (1_073_741_824, "1.0 GiB"),
            (5_000_000_000, "4.6 GiB"),
            (u64::MAX, "17179869183.9 GiB"),
        ];
        for (input, expected) in cases {
            assert_eq!(human_bytes(input), expected, "input={input}");
        }
    }
}
