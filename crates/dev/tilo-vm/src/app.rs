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
    SlotRestart, SlotRm, SlotShow, SlotShowResult, Status, Swap, VM_SLOT, VmTools,
};

use crate::build;
use crate::ci;
use crate::cli::{
    Builder, Command, CreateArgs, InputCommand, ScenarioCommand, SeatCommand, SlotCommand,
    UpgradeArgs, VmCommand,
};
use crate::config::{self, Registry, Source, VmEntry, WORKTREE_FILE};
use crate::context::Worktree;
use crate::disk;
use crate::doctor::{self, Check};
use crate::error::CliError;
use crate::guide;
use crate::host::{self, HostFacts};
use crate::hypervisor::{
    self, CommandLine, Hypervisor, Runner, SshEndpoint, SystemRunner, Vm, run_all,
};
use crate::imageup;
use crate::receipt::Receipt;
use crate::remote::{self, LockOutcome, Remote};
use crate::sshguard;
use crate::sshkey;
use crate::vmbuild;

mod kernel;
mod scenario;

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
    auth: disk::AuthHeader,
}

/// What `vm create` will do, settled before it does anything.
struct Plan {
    choice: host::Choice,
    image: Option<PathBuf>,
    published: Option<Published>,
    disk_path: Option<PathBuf>,
}

impl Plan {
    fn describe(&self) -> String {
        let mut lines = vec![format!("  hypervisor: {}", self.choice.describe())];
        match (&self.image, &self.published) {
            (Some(image), _) => lines.push(format!("  image: {}", image.display())),
            (None, Some(p)) => lines.push(format!(
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
            )),
            (None, None) => lines.push("  image: the UTM template".into()),
        }
        if let Some(disk) = &self.disk_path {
            lines.push(format!("  VM disk: {}", disk.display()));
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

/// The forge registry that publishes images and CI layers.
pub const REGISTRY: &str = "code.sandbox.example";

/// Output that means the registry refused our credentials.
pub fn is_auth_failure(text: &str) -> bool {
    text.contains("401") || text.contains("reqPackageAccess") || text.contains("unauthorized")
}

/// Preflight for anything that pulls from [`REGISTRY`] on this host.
fn registry_login(runner: &mut dyn Runner) -> Result<String, CliError> {
    let output = runner
        .output(&CommandLine::new(
            "podman",
            &["login", "--get-login", REGISTRY],
        ))
        .map_err(|_| CliError::ToolMissing {
            tool: "podman".into(),
            purpose: format!("pull from {REGISTRY}"),
        })?;
    if output.success {
        Ok(output.stdout.trim().to_owned())
    } else {
        Err(CliError::RegistryLogin {
            place: "this host".into(),
            registry: REGISTRY.into(),
        })
    }
}

/// `path`, or its nearest ancestor that exists.
fn existing_ancestor(path: &Path) -> &Path {
    path.ancestors()
        .find(|p| p.exists())
        .unwrap_or_else(|| Path::new("/"))
}

/// Free bytes of the filesystem that holds (or will hold) `path`, where the
/// host can tell.
fn host_free(path: &Path) -> Option<u64> {
    if !cfg!(unix) {
        return None;
    }
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

fn on_path(tool: &str) -> bool {
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
            Some(Command::Scenario(ScenarioCommand::Install(args))) => self.scenario_install(&args),
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
        checks.extend(doctor::credential_checks(
            token.as_ref(),
            registry_login(&mut self.runner),
        ));
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
        run_all(&mut self.runner, &adapter.stop(&vm.vm()))?;
        let started = Instant::now();
        let limit = Duration::from_secs(120);
        let mut down = lock.as_mut().map(|l| l.wait_for_shutdown(limit));
        // SSH closes early in a shutdown; where the hypervisor can say, "down"
        // means the VM is off, so a following vm start finds it stopped.
        if down != Some(false) && adapter.running(&vm.vm(), &mut self.runner).is_some() {
            down = Some(loop {
                match adapter.running(&vm.vm(), &mut self.runner) {
                    Some(true) if started.elapsed() < limit => {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    Some(true) => break false,
                    _ => break true,
                }
            });
        }
        let text = match down {
            Some(true) if lock.is_some() => format!(
                "VM {} shut down (it held the VM lock until it was down)",
                vm.name
            ),
            Some(true) => format!("VM {} shut down", vm.name),
            Some(false) => format!(
                "VM {} was asked to shut down but is still up after 120s; tilo-vm vm status",
                vm.name
            ),
            None => format!("VM {} is shutting down", vm.name),
        };
        drop(lock);
        Ok(Outcome::new(text, json!({ "vm": vm.name, "down": down })).next(&["tilo-vm vm start"]))
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
        if !sshkey::key_path(&self.registry_path, &args.name).exists() && !on_path("ssh-keygen") {
            return Err(CliError::ToolMissing {
                tool: "ssh-keygen".into(),
                purpose: "make the VM's root SSH key".into(),
            });
        }
        let format = disk::format_for(choice.adapter);
        let image = match &args.image {
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
        let published = if image.is_none() && args.template.is_none() {
            Some(self.resolve_published(args.tag.as_deref(), arch, format)?)
        } else {
            None
        };
        let floor = self
            .registry()?
            .min_free_bytes()
            .max(if choice.adapter == "whpx" {
                15 << 30
            } else {
                0
            });
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
                dirs::data_local_dir()
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
            published,
            disk_path,
        })
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
        let summary = plan.describe();
        if !self.json {
            eprintln!("tilo-vm: preflight passed\n{summary}");
        }
        self.receipt.set("preflight", &summary);
        let mut image = plan.image.as_ref().map(|p| p.display().to_string());
        let tag = plan.published.as_ref().map(|p| p.tag.clone());
        if let Some(published) = &plan.published {
            let runner = &mut self.runner;
            self.receipt.phase("download", || {
                disk::fetch(runner, &published.disk, &published.dest, &published.auth)
            })?;
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
            cpus: args.cpus,
            ssh_port: args.ssh_port,
            template: args.template,
            qemu_img: plan.choice.tool("qemu-img").map(str::to_owned),
            qemu_system: plan.choice.tool("qemu-system-aarch64").map(str::to_owned),
            firmware_code: plan.choice.tool("firmware-code").map(str::to_owned),
            firmware_vars: plan.choice.tool("firmware-vars").map(str::to_owned),
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
        let tag = if let Some(tag) = tag {
            tag.to_owned()
        } else {
            let (status, body) = disk::get(&mut self.runner, disk::VERSIONS, &auth)?;
            match status {
                200 => disk::newest_tag(&body).ok_or_else(|| {
                    if token.is_none() {
                        self.warnings.push(
                            "no forge token was found, and the forge hides private packages from anonymous requests: export FORGEJO_TOKEN=<token with package read access>".into(),
                        );
                    }
                    CliError::NoDiskPublished {
                        tag: "(any)".into(),
                        url: disk::VERSIONS.into(),
                        local_build: local_build.clone(),
                    }
                })?,
                401 | 403 => return Err(refused(disk::VERSIONS, status)),
                _ => return Err(unexpected(disk::VERSIONS, status, &body)),
            }
        };
        let url = disk::manifest_url(&tag);
        let (status, body) = disk::get(&mut self.runner, &url, &auth)?;
        match status {
            200 => {}
            401 | 403 => return Err(refused(&url, status)),
            404 => {
                if token.is_none() {
                    self.warnings.push(
                        "no forge token was found, and the forge answers 404 for private packages: export FORGEJO_TOKEN=<token with package read access>".into(),
                    );
                }
                return Err(CliError::NoDiskPublished {
                    tag,
                    url,
                    local_build,
                });
            }
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
                    local_build,
                })?;
        let dest = disk::cache_path(&tag, &chosen.file);
        Ok(Published {
            tag,
            disk: chosen,
            dest,
            auth,
        })
    }

    fn vm_start(&mut self, name: Option<&str>) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let floor = self
            .registry()?
            .min_free_bytes()
            .max(if vm.entry.hypervisor == "whpx" {
                15 << 30
            } else {
                0
            });
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
        run_all(&mut self.runner, &adapter.start(&vm.vm()))?;
        let started = Instant::now();
        let mut reachable = false;
        let mut no_daemon = None;
        let mut remote = None;
        while started.elapsed() < Duration::from_secs(180) {
            match self.connect(&vm) {
                Ok(connected) => {
                    reachable = true;
                    remote = Some(connected);
                    break;
                }
                // sshd is up and says no: waiting will not change that.
                Err(error @ CliError::RootKeyRefused { .. }) => return Err(error),
                // SSH works; the image just has no daemon, and never will.
                Err(error @ CliError::DaemonMissing { .. }) => {
                    reachable = true;
                    no_daemon = Some(error);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_secs(2)),
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
            return self.vm_upgrade_registry(&vm, &endpoint, args);
        };
        let (image, build) = imageup::parse_from(from)?;
        if !build && (args.base.is_some() || args.rebuild_base) {
            return Err(CliError::Unsupported {
                what: "--base or --rebuild-base with --from local:<image>".into(),
                alternative: "those choose how --from local builds; an image you name is used as it is: tilo-vm vm upgrade --from local --rebuild-base".into(),
            });
        }
        self.receipt.set("image", &image);
        if !imageup::is_dev_name(&image) {
            // Refused by name alone, before anything is built or inspected.
            imageup::check_image(&image, &imageup::ImageFacts::default())?;
        }
        // Fail on an unreachable or non-bootc VM before a long build.
        self.vm_facts(&vm, &endpoint, "vm upgrade")?;
        if build {
            self.build_dev_image(&image, args.base.as_deref(), args.rebuild_base)?;
        }
        let runner = &mut self.runner;
        let image_facts = self
            .receipt
            .phase("check", || local_image_facts(runner, &image))?;
        imageup::check_image(&image, &image_facts)?;
        let facts = self.vm_facts(&vm, &endpoint, "vm upgrade")?;
        match imageup::check_base_age(&image, facts.base.as_ref(), image_facts.base.as_ref()) {
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
        let layout = imageup::host_layout(&vm.name);
        let runner = &mut self.runner;
        let digest = self
            .receipt
            .phase("export", || export_layout(runner, &image, &layout))?;
        self.receipt.set("digest", &digest);
        let layout_bytes = imageup::layout_bytes(&layout);
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
        let transferred = self.send_layout(&vm, &endpoint, &layout)?;
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
        self.stage_and_boot(&vm, &endpoint, &facts, &stage)
    }

    /// `vm upgrade` and `vm upgrade --image`: bootc pulls from a registry.
    fn vm_upgrade_registry(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        args: &UpgradeArgs,
    ) -> Result<Outcome, CliError> {
        if let Some(image) = &args.image
            && !imageup::is_dev_name(image)
        {
            imageup::check_image(image, &imageup::ImageFacts::default())?;
        }
        let facts = self.vm_facts(vm, endpoint, "vm upgrade")?;
        let (transport, imgref) = match &args.image {
            Some(image) => ("registry", image.clone()),
            None => match facts.bootc.spec.clone() {
                Some((image, transport)) if transport == "oci" => ("oci", image),
                Some((image, _)) => ("registry", image),
                None => ("registry", String::new()),
            },
        };
        if transport == "registry" && imgref.starts_with(REGISTRY) {
            registry_login(&mut self.runner)?;
        }
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
        let rsync = imageup::rsync_command(layout, endpoint, vm.entry.key.as_deref());
        let runner = &mut self.runner;
        let sent = self.receipt.phase("transfer", || runner.output(&rsync))?;
        if sent.success {
            return Ok(imageup::rsync_sent_bytes(&sent.stdout));
        }
        // The remote side runs under flock; rsync reports a held lock only as
        // a closed connection, so say what it most likely was.
        let mut stderr = sent.stderr.trim().to_owned();
        if stderr.contains("connection unexpectedly closed") {
            stderr.push_str(
                " (another vm upgrade of this VM may hold /var/lib/tilo-vm/locks/image.lock)",
            );
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
        eprintln!(
            "tilo-vm: building {image} FROM {} ({}); podman's layer cache skips unchanged steps",
            base.unwrap_or(imageup::LOCAL_BASE),
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
    fn stage_and_boot(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        facts: &imageup::VmFacts,
        stage: &Stage,
    ) -> Result<Outcome, CliError> {
        // The image is the VM's rollback deployment already (after rollback-image):
        // bootc will not stage it again, but rolling back boots exactly it.
        let reuse = facts.bootc.staged.is_none()
            && stage.expected.is_some()
            && facts.bootc.rollback.as_ref().map(|r| &r.digest) == stage.expected.as_ref();
        let words = if reuse {
            vec!["bootc".to_owned(), "rollback".to_owned()]
        } else {
            imageup::stage_command(&facts.bootc, stage.transport, &stage.imgref)
        };
        let staged = self.vm_locked(vm, endpoint, &words)?;
        if !staged.success {
            if stage.transport == "registry" && is_auth_failure(&staged.stderr) {
                return Err(CliError::RegistryLogin {
                    place: format!("VM {}", vm.name),
                    registry: REGISTRY.into(),
                });
            }
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

    /// Reboot, wait for a new boot id over SSH, then for tilo-vmd, and check
    /// that the booted image has `digest`.
    fn reboot_into(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
        old_boot: &str,
        digest: &str,
    ) -> Result<imageup::Deployment, CliError> {
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
        while started.elapsed() < Duration::from_secs(600) {
            std::thread::sleep(Duration::from_secs(2));
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
            let adapter = Self::adapter(vm)?;
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
        let vm = self.resolve_vm(None)?;
        match command {
            SlotCommand::Create {
                name,
                headless: _,
                headful,
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
                })?;
                let user = result.slot.user.clone().unwrap_or_default();
                let text = if result.created {
                    format!(
                        "slot {name} created ({}); session started as {user} on the base image",
                        kind.as_str()
                    )
                } else if result.actions.is_empty() {
                    format!("slot {name} already exists and its session is running; nothing to do")
                } else {
                    format!(
                        "slot {name} already existed; its session was not running and has been started"
                    )
                };
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
        let rsync = vmbuild::rsync_command(&worktree.root, &endpoint, vm.entry.key.as_deref(), &id);
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
                stderr: "rsync's output above says why (exit 1 from flock: another build of this worktree is running)".into(),
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
                Err(CliError::Remote(error)) if error.code == tilo_vm_protocol::code::AUTH => {
                    Err(CliError::RegistryLogin {
                        place: format!("VM {}", vm.name),
                        registry: REGISTRY.into(),
                    })
                }
                Err(error) => Err(error),
            };
        }
        // No podman in the VM: pull here, upload the layer.
        registry_login(&mut self.runner)?;
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
    };
    let actions = vec![action];
    tilo_vm_protocol::input::validate(&actions).map_err(CliError::InvalidInput)?;
    Ok(actions)
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
    format!(
        "slot: {} ({}) generation {}, session {}{seat}{owner}",
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
            | SlotCommand::Exec { name: slot, .. },
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
                    format!("tilo-vm vm upgrade {vm} --image code.sandbox.example/qos/tilo-vm:<tag>"),
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

    #[test]
    fn swapping_the_installer_redirects_to_its_scenario() {
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

    #[test]
    fn kernel_applies_say_the_reboot_is_next() {
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

    #[test]
    fn apply_descriptions_name_the_consequence() {
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

    #[test]
    fn vm_ssh_refuses_writes_before_it_looks_for_a_vm() {
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

    #[test]
    fn status_tells_off_from_silent_from_missing_from_incompatible() {
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

    #[test]
    fn registry_auth_failures_are_recognised() {
        assert!(is_auth_failure(
            "Error: reading manifest: unauthorized: reqPackageAccess"
        ));
        assert!(is_auth_failure(
            "received unexpected HTTP status: 401 Unauthorized"
        ));
        assert!(!is_auth_failure("no space left on device"));
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

    #[test]
    fn builder_choice_prefers_the_vm_and_explains_fallbacks() {
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

    #[test]
    fn input_arguments_become_validated_actions() {
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

    fn create_args(name: &str) -> CreateArgs {
        CreateArgs {
            name: name.into(),
            hypervisor: None,
            image: None,
            tag: None,
            disk: None,
            arch: None,
            emulated: false,
            memory_mb: None,
            cpus: None,
            ssh: None,
            ssh_port: None,
            template: None,
        }
    }

    #[test]
    fn preflight_names_the_missing_hypervisor_before_anything_else() {
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

    #[test]
    fn preflight_checks_the_image_and_the_vm_disk() {
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

    #[test]
    fn preflight_keeps_raw_qemu_to_tilo_worktrees() {
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

    #[test]
    fn a_refused_key_names_the_install_step() {
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
            error.hint().contains("tester@192.168.64.5"),
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
            parity: None,
        }
    }

    #[test]
    fn hints_follow_a_named_slot() {
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

    #[test]
    fn a_missing_slot_shows_the_existing_ones_instead_of_create() {
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

    #[test]
    fn slot_lines_name_the_owner() {
        let line = slot_line(&slot_info("ux1", SlotKind::Headful, 1, &[0, 1]));
        assert!(line.ends_with("owner laptop:ux-pass"), "{line}");
        assert_eq!(describe_owner(&Owner::default()), None);
    }

    #[test]
    fn rolling_forward_goes_to_the_next_newer_generation() {
        let info = slot_info("a", SlotKind::Headless, 1, &[0, 1, 2, 4]);
        assert_eq!(forward_target("a", Some(&info)).unwrap(), 2);
        let newest = slot_info("a", SlotKind::Headless, 4, &[0, 1, 2, 4]);
        let error = forward_target("a", Some(&newest)).unwrap_err();
        assert!(error.to_string().contains("newest"), "{error}");
        assert!(error.hint().contains("--to <n>"), "{}", error.hint());
        assert!(forward_target("a", None).is_err());
    }

    #[test]
    fn slot_exec_quotes_the_command_for_the_remote_shell() {
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

    #[test]
    fn slot_show_lists_files_by_component_with_digests() {
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

    #[test]
    fn gc_names_what_it_kept_and_when_it_goes() {
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

    #[test]
    fn bytes_read_well() {
        assert_eq!(human_bytes(5 << 30), "5.0 GB");
        assert_eq!(human_bytes(300 << 20), "300 MB");
    }
}
