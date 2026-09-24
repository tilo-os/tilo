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
    Control, Gc, Input, InputAction, JobOutput, LayerImport, LayerSource, Logs, Policy, Rollback,
    Scope, SeatClaim, SeatRelease, Shot, SlotCreate, SlotInfo, SlotKind, SlotList, SlotRestart,
    SlotRm, Status, Swap, VM_SLOT, VmTools,
};

use crate::build;
use crate::ci;
use crate::cli::{Builder, Command, CreateArgs, InputCommand, SeatCommand, SlotCommand, VmCommand};
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
use crate::receipt::Receipt;
use crate::remote::{self, LockOutcome, Remote};
use crate::sshkey;
use crate::vmbuild;

mod kernel;

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
pub const REGISTRY: &str = "code.podesta.ai";

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

/// Commands that change the VM's `/usr` or tilo-vm's state behind its back.
pub fn looks_like_deploy(command: &[String]) -> bool {
    let line = command.join(" ");
    let writes_usr = ["cp ", "mv ", "install ", "rsync ", "tee ", "> ", "ln "]
        .iter()
        .any(|verb| line.contains(verb) && line.contains("/usr/"));
    writes_usr
        || [
            "systemd-sysext merge",
            "systemd-sysext refresh",
            "systemd-sysext unmerge",
            "/var/lib/tilo-vm",
            "/var/lib/extensions",
            "bootc usr-overlay",
            "ostree admin unlock",
            "tilo-vmd",
        ]
        .iter()
        .any(|pattern| line.contains(pattern))
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
        let remote = Remote::connect(
            &vm.name,
            &endpoint,
            vm.entry.key.as_deref(),
            vm.entry.daemon.as_deref(),
            debug,
        )
        .map_err(|error| key_refusal(vm, adapter.as_ref(), &endpoint, error))?;
        for issue in &remote.hello.issues {
            self.warnings.push(format!(
                "slot {}: {}; {}",
                issue.slot, issue.problem, issue.hint
            ));
        }
        self.receipt.set("vm", &vm.name);
        Ok(remote)
    }

    /// # Errors
    /// Outside a worktree, or a branch that makes no usable name.
    pub fn slot_name(&self, explicit: Option<String>) -> Result<String, CliError> {
        if let Some(name) = explicit {
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
        match command {
            None | Some(Command::Status) => self.status(),
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
            Some(Command::Rollback { slot, to, .. }) => {
                let slot = self.slot_name(slot)?;
                if slot == VM_SLOT {
                    return self.rollback_vm(to);
                }
                let vm = self.resolve_vm(None)?;
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
            Some(Command::Gc {
                dry_run,
                keep,
                keep_targets,
            }) => self.gc(dry_run, keep, keep_targets),
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
    fn status(&mut self) -> Result<Outcome, CliError> {
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
        let slot = self.slot_name(None).ok();
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
                lines.push(format!("  unreachable: {error}"));
                insert(&mut vm_data, "reachable", json!(false));
                let data = json!({ "vm": vm_data, "error": error.to_json() });
                let mut outcome = Outcome::new(lines.join("\n"), data);
                outcome.next = vec!["tilo-vm vm start".into(), "tilo-vm doctor".into()];
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
                lines.push(format!(
                    "slot: {name} (not created yet; new here? tilo-vm guide quickstart)"
                ));
                next.push("tilo-vm slot create --headless".into());
                next.push("tilo-vm guide quickstart".into());
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
    fn vm_checks(&self, remote: &mut Remote, vm: &Resolved, checks: &mut Vec<Check>) {
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
        let upgrade = "tilo-vm vm upgrade   (a dev image with cargo, rsync and python3)";
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
            VmCommand::Stop { name, force } => self.vm_stop(name.as_deref(), force),
            VmCommand::Upgrade { name, image } => {
                self.vm_upgrade(name.as_deref(), image.as_deref())
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
            std::fs::write(&file, format!("vm = \"{name}\"\n"))
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
        if !raw && looks_like_deploy(command) {
            return Err(CliError::RawDeploy {
                command: command.join(" "),
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
        if !ok && !command.is_empty() {
            return Err(CliError::CommandFailed {
                command: command.join(" "),
                status: "non-zero exit".into(),
                stderr: String::new(),
            });
        }
        Ok(Outcome::new(
            "",
            json!({ "vm": vm.name, "command": command }),
        ))
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
                    "  reachable; tilo-vmd {} (protocol {}, {})",
                    remote.hello.daemon_version, remote.hello.protocol, remote.hello.arch
                ));
                insert(&mut data, "reachable", json!(true));
                insert(&mut data, "daemon", json!(remote.hello));
                Ok(Outcome::new(lines.join("\n"), data).next(&["tilo-vm status"]))
            }
            Err(error @ CliError::DaemonMissing { .. }) => {
                lines.push(
                    "  reachable over SSH, but no tilo-vmd: it is not running a Tilo dev image"
                        .into(),
                );
                insert(&mut data, "reachable", json!(true));
                insert(&mut data, "daemon", Value::Null);
                insert(&mut data, "error", error.to_json());
                let mut outcome = Outcome::new(lines.join("\n"), data);
                outcome.next = vec![error.hint(), format!("tilo-vm vm ssh {}", vm.name)];
                Ok(outcome)
            }
            Err(error) => {
                lines.push(format!("  not reachable: {error}"));
                insert(&mut data, "reachable", json!(false));
                insert(&mut data, "error", error.to_json());
                let mut outcome = Outcome::new(lines.join("\n"), data).next(&["tilo-vm vm start"]);
                outcome
                    .next
                    .extend(Self::debug_commands(adapter.as_ref(), &vm.name));
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
        let floor = self.registry()?.min_free_bytes();
        if let Some(published) = &published
            && !disk::cached(&published.dest, &published.disk)
        {
            let part = published.dest.with_extension("part");
            let have = std::fs::metadata(&part).map_or(0, |m| m.len());
            let needed = published.disk.size.saturating_sub(have);
            let dir = published.dest.parent().unwrap_or_else(|| Path::new("."));
            if let Some(free) = host_free(dir)
                && free < needed
            {
                return Err(CliError::NoSpace {
                    place: existing_ancestor(dir).display().to_string(),
                    free: human_bytes(free),
                    what: format!("the {} dev disk download", published.tag),
                    needed: human_bytes(needed),
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
        let floor = self.registry()?.min_free_bytes();
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
        while started.elapsed() < Duration::from_secs(180) {
            match self.connect(&vm) {
                Ok(_) => {
                    reachable = true;
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
        Ok(Outcome::new(
            format!("VM {} is up; SSH answered after {seconds:.0}s", vm.name),
            json!({ "vm": vm.name, "boot_seconds": seconds }),
        )
        .next(&["tilo-vm status", "tilo-vm slot create --headless"]))
    }

    fn vm_upgrade(&mut self, name: Option<&str>, image: Option<&str>) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        registry_login(&mut self.runner)?;
        let adapter = Self::adapter(&vm)?;
        let endpoint = adapter.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        // Under the VM lock, so no swap or gc runs while the image changes.
        let mut command: Vec<String> = [
            "flock",
            "--nonblock",
            "/var/lib/tilo-vm/locks/vm.lock",
            "bootc",
        ]
        .map(str::to_owned)
        .to_vec();
        match image {
            Some(image) => command.extend(["switch".into(), image.to_owned()]),
            None => command.push("upgrade".into()),
        }
        let line = remote::ssh_command(&endpoint, vm.entry.key.as_deref(), &command, false);
        let output = self.runner.output(&line)?;
        if !output.success && is_auth_failure(&output.stderr) {
            return Err(CliError::RegistryLogin {
                place: format!("VM {}", vm.name),
                registry: REGISTRY.into(),
            });
        }
        if !output.success {
            return Err(CliError::CommandFailed {
                command: command.join(" "),
                status: output.status,
                stderr: output.stderr.trim().to_owned(),
            });
        }
        Ok(Outcome::new(
            format!(
                "new image staged on {}; it takes effect at the next boot (slots survive in /var)",
                vm.name
            ),
            json!({ "vm": vm.name, "image": image, "output": output.stdout }),
        )
        .next(&["tilo-vm vm stop", "tilo-vm vm start"]))
    }

    // ---- slots ----------------------------------------------------------------

    fn slot(&mut self, command: SlotCommand) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(None)?;
        match command {
            SlotCommand::Create {
                name,
                headless: _,
                headful,
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
            SlotCommand::Restart { name } => {
                let name = self.slot_name(name)?;
                let mut remote = self.connect(&vm)?;
                let result = self
                    .receipt
                    .phase("apply", || remote.call(SlotRestart { name }))?;
                Ok(Outcome::new(describe_apply(&result), json!(result))
                    .next(&["tilo-vm logs", "tilo-vm shot"]))
            }
            SlotCommand::Rm { name } => {
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
                driver: entry
                    .scenario
                    .clone()
                    .unwrap_or_else(|| "os/dev/vm.sh boot".into()),
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
        )?;
        self.warnings.extend(note);
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
        )?;
        self.warnings.extend(note);
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
        let mut outcome = Outcome::new(text, json!(result));
        if dry_run {
            outcome = outcome.next(&["tilo-vm gc"]);
        }
        Ok(outcome)
    }
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
            "unavailable (the VM lacks {}; tilo-vm vm upgrade)",
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
                "the VM's image has no {}, so this build runs on this host (--builder host); tilo-vm vm upgrade gets an image that builds",
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
    format!(
        "slot: {} ({}) generation {}, session {}{seat}",
        info.name,
        info.kind.as_str(),
        info.generation,
        info.session
    )
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
    fn deploy_attempts_over_raw_ssh_are_caught() {
        let cmd = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert!(looks_like_deploy(&cmd("sudo cp shell /usr/bin/shell")));
        assert!(looks_like_deploy(&cmd("systemd-sysext refresh")));
        assert!(looks_like_deploy(&cmd("rm -rf /var/lib/tilo-vm/layers/x")));
        assert!(!looks_like_deploy(&cmd("ls /usr/bin")));
        assert!(!looks_like_deploy(&cmd("journalctl -b -p warning")));
        assert!(!looks_like_deploy(&[]));
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

    #[test]
    fn bytes_read_well() {
        assert_eq!(human_bytes(5 << 30), "5.0 GB");
        assert_eq!(human_bytes(300 << 20), "300 MB");
    }
}
