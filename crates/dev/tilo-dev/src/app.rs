//! Command implementations. Each returns an [`Outcome`]: what happened, as
//! text and as JSON, plus the likely next commands.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tilo_dev_protocol::manifest::{Component, Manifest};
use tilo_dev_protocol::{
    Action, ApplyResult, Control, Gc, LayerImport, LayerSource, Logs, Policy, Rollback, Scope,
    SeatClaim, SeatRelease, Shot, SlotCreate, SlotInfo, SlotKind, SlotList, SlotRestart, SlotRm,
    Status, Swap, VM_SLOT,
};

use crate::build;
use crate::cli::{Command, CreateArgs, SeatCommand, SlotCommand, VmCommand};
use crate::config::{self, Registry, Source, VmEntry, WORKTREE_FILE};
use crate::context::Worktree;
use crate::error::CliError;
use crate::guide;
use crate::hypervisor::{self, CommandLine, Hypervisor, Runner, SystemRunner, Vm, run_all};
use crate::receipt::Receipt;
use crate::remote::{self, Remote};

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
                .strip_prefix("tilo-dev-session@")?
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

/// Commands that change the VM's `/usr` or tilo-dev's state behind its back.
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
            "/var/lib/tilo-dev",
            "/var/lib/extensions",
            "bootc usr-overlay",
            "ostree admin unlock",
            "tilo-devd",
        ]
        .iter()
        .any(|pattern| line.contains(pattern))
}

/// Free bytes of the filesystem holding `path`, where the host can tell.
fn host_free(path: &Path) -> Option<u64> {
    if !cfg!(unix) {
        return None;
    }
    let output = std::process::Command::new("df")
        .arg("-Pk")
        .arg(path)
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
            let env = std::env::var("TILO_DEV_VM").ok();
            config::resolve(
                env.as_deref(),
                worktree_file,
                &registry,
                &self.registry_path,
            )
            .ok_or(CliError::NoVm)?
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
            commands.push(format!("tilo-dev vm shot {vm}"));
        }
        if caps.serial {
            commands.push(format!("tilo-dev vm serial {vm}"));
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
        )?;
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
            Some(Command::Swap { target, from }) => self.swap(&target, &from),
            Some(Command::Rollback { slot, to }) => {
                let slot = self.slot_name(slot)?;
                let vm = self.resolve_vm(None)?;
                let mut remote = self.connect(&vm)?;
                let result = self.receipt.phase("apply", || {
                    remote.call(Rollback {
                        slot: slot.clone(),
                        to,
                    })
                })?;
                Ok(Outcome::new(describe_apply(&result), json!(result)).next(&[
                    "tilo-dev shot",
                    "tilo-dev logs",
                    "tilo-dev status",
                ]))
            }
            Some(Command::Logs { slot, lines }) => {
                let slot = self.slot_name(slot)?;
                let vm = self.resolve_vm(None)?;
                let result = self.connect(&vm)?.call(Logs { slot, lines })?;
                Ok(Outcome::new(result.text.clone(), json!(result)))
            }
            Some(Command::Seat(command)) => self.seat(command),
            Some(Command::Shot { slot, output }) => self.shot(slot, &output),
            Some(Command::Control { slot, args }) => self.control(slot, &args),
            Some(Command::Gc { dry_run, keep }) => self.gc(dry_run, keep),
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
            Err(CliError::NoVm) => {
                return Ok(Outcome::new(
                    format!("VM: none selected\n{goals}"),
                    json!({ "vm": null, "registry": self.registry_path }),
                )
                .next(&[
                    "tilo-dev vm list",
                    "tilo-dev vm use <name>",
                    "tilo-dev guide platforms",
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
                outcome.next = vec!["tilo-dev vm start".into(), "tilo-dev doctor".into()];
                return Ok(outcome);
            }
        };
        let hello = remote.hello.clone();
        let status = remote.call(Status { slot: None })?;
        lines.push(format!(
            "  tilo-devd {} (protocol {}), {}",
            hello.daemon_version, hello.protocol, hello.arch
        ));
        let registry = self.registry()?;
        if let Some(free) = status.storage.free_bytes {
            let low = free < registry.min_free_bytes();
            lines.push(format!(
                "  disk: {} free, layers use {}{}",
                human_bytes(free),
                human_bytes(status.storage.layers_bytes),
                if low { "  LOW: tilo-dev gc" } else { "" }
            ));
        }
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
                        "tilo-dev swap <component>",
                        "tilo-dev shot",
                        "tilo-dev control actions list",
                        "tilo-dev logs",
                    ]
                    .map(str::to_owned),
                );
            }
            (Some(name), None) => {
                lines.push(format!("slot: {name} (not created yet)"));
                next.push("tilo-dev slot create --headless".into());
            }
            (None, _) => {
                lines.push("slot: none (not in a worktree; pass a slot name)".into());
                next.push("tilo-dev slot list".into());
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

    #[expect(
        clippy::too_many_lines,
        reason = "a flat checklist reads best in one place"
    )]
    fn doctor(&mut self) -> Outcome {
        let mut checks: Vec<Value> = Vec::new();
        let mut check = |name: &str, state: &str, detail: String, fix: Option<String>| {
            checks.push(json!({ "check": name, "state": state, "detail": detail, "fix": fix }));
        };
        for (tool, purpose) in [
            ("ssh", "reach the VM"),
            ("git", "name your slot and record provenance"),
        ] {
            if on_path(tool) {
                check(tool, "ok", format!("{tool} found"), None);
            } else {
                check(
                    tool,
                    "fail",
                    format!("{tool} is not on PATH (needed to {purpose})"),
                    Some(format!("install {tool}")),
                );
            }
        }
        if cfg!(target_os = "linux") {
            for tool in ["cargo", "python3"] {
                if on_path(tool) {
                    check(
                        tool,
                        "ok",
                        format!("{tool} found (for swap --from worktree)"),
                        None,
                    );
                } else {
                    check(
                        tool,
                        "warn",
                        format!("{tool} missing: swap --from worktree cannot build"),
                        Some(format!("install {tool}")),
                    );
                }
            }
        } else {
            check(
                "host builds",
                "warn",
                "swap --from worktree needs a Linux host".into(),
                Some("swap prebuilt layers: --from layer:<hash>".into()),
            );
        }
        match self.manifest() {
            Ok(Some(_)) => check("manifest", "ok", "os/components.toml found".into(), None),
            Ok(None) => check(
                "manifest",
                "warn",
                "no os/components.toml here".into(),
                Some("run from a Tilo worktree".into()),
            ),
            Err(e) => check("manifest", "fail", e.to_string(), Some(e.hint())),
        }
        match registry_login(&mut self.runner) {
            Ok(user) => check(
                "registry",
                "ok",
                format!("logged in to {REGISTRY} as {user}"),
                None,
            ),
            Err(e) => check(
                "registry",
                "warn",
                format!("{e} (needed for vm create from {REGISTRY}, vm upgrade and CI layers)"),
                Some(e.hint()),
            ),
        }
        let vm = match self.resolve_vm(None) {
            Ok(vm) => vm,
            Err(e) => {
                check("vm", "fail", e.to_string(), Some(e.hint()));
                return doctor_outcome(&checks);
            }
        };
        check(
            "vm",
            "ok",
            format!("{} ({})", vm.name, vm.source.describe()),
            None,
        );
        let adapter = match Self::adapter(&vm) {
            Ok(adapter) => adapter,
            Err(e) => {
                check("hypervisor", "fail", e.to_string(), Some(e.hint()));
                return doctor_outcome(&checks);
            }
        };
        let missing: Vec<&str> = adapter
            .tools()
            .iter()
            .copied()
            .filter(|t| !on_path(t))
            .collect();
        if missing.is_empty() {
            check(
                "hypervisor",
                "ok",
                format!("{}: {} found", adapter.name(), adapter.tools().join(", ")),
                None,
            );
        } else {
            check(
                "hypervisor",
                "warn",
                format!("{}: missing {}", adapter.name(), missing.join(", ")),
                Some(format!("install {}", missing.join(", "))),
            );
        }
        let caps = adapter.capabilities();
        check(
            "debug capabilities",
            "ok",
            format!(
                "serial {}, screenshot {}, reset {}, control {}",
                yes(caps.serial),
                yes(caps.screenshot),
                yes(caps.reset),
                caps.control.unwrap_or("no")
            ),
            None,
        );
        if let Some(disk) = vm.entry.disk.as_deref()
            && let Ok(meta) = std::fs::metadata(disk)
        {
            let limit = self.registry().map_or(u64::MAX, |r| r.warn_disk_bytes());
            let state = if meta.len() > limit { "warn" } else { "ok" };
            check(
                "vm disk",
                state,
                format!("{disk} is {}", human_bytes(meta.len())),
                (state == "warn").then(|| "tilo-dev gc".into()),
            );
        }
        match self.connect(&vm) {
            Ok(mut remote) => {
                let hello = remote.hello.clone();
                check(
                    "daemon",
                    "ok",
                    format!(
                        "tilo-devd {} speaks protocol {}",
                        hello.daemon_version, hello.protocol
                    ),
                    None,
                );
                let emulated = vm.entry.emulated;
                if hello.arch == std::env::consts::ARCH || emulated {
                    check("arch", "ok", format!("VM is {}", hello.arch), None);
                } else {
                    check(
                        "arch",
                        "warn",
                        format!(
                            "VM is {} but this host is {}",
                            hello.arch,
                            std::env::consts::ARCH
                        ),
                        Some("host builds will not run there; build on a matching host".into()),
                    );
                }
                if let Ok(status) = remote.call(Status { slot: None }) {
                    let floor = self.registry().map_or(0, |r| r.min_free_bytes());
                    if let Some(free) = status.storage.free_bytes {
                        let state = if free < floor { "fail" } else { "ok" };
                        check(
                            "vm free space",
                            state,
                            format!("{} free, floor {}", human_bytes(free), human_bytes(floor)),
                            (state == "fail").then(|| "tilo-dev gc".into()),
                        );
                    }
                    for slot in status.slots.iter().filter(|s| s.user.is_some()) {
                        match (&slot.parity, slot.session.as_str()) {
                            (Some(problem), _) => check(
                                "session parity",
                                "fail",
                                format!("slot {}: {problem}", slot.name),
                                Some("slot session capabilities or SELinux domain differ from a greetd session; see tilo-dev guide troubleshooting".into()),
                            ),
                            (None, "active") => check(
                                "session parity",
                                "ok",
                                format!("slot {} matches a greetd session (no capabilities, user domain)", slot.name),
                                None,
                            ),
                            _ => {}
                        }
                    }
                    for issue in status
                        .issues
                        .into_iter()
                        .filter(|i| !i.problem.contains("greetd session"))
                    {
                        check(
                            "vm state",
                            "warn",
                            format!("slot {}: {}", issue.slot, issue.problem),
                            Some(issue.hint),
                        );
                    }
                }
            }
            Err(e) => check("daemon", "fail", e.to_string(), Some(e.hint())),
        }
        self.warnings.clear();
        doctor_outcome(&checks)
    }

    // ---- vm -------------------------------------------------------------------

    fn vm(&mut self, command: VmCommand) -> Result<Outcome, CliError> {
        match command {
            VmCommand::List => self.vm_list(),
            VmCommand::Use { name, global } => self.vm_use(&name, global),
            VmCommand::Ssh { raw, vm, command } => self.vm_ssh(vm.as_deref(), raw, &command),
            VmCommand::Status { name } => self.vm_status(name.as_deref()),
            VmCommand::Create(args) => self.vm_create(args),
            VmCommand::Start { name } => self.vm_start(name.as_deref()),
            VmCommand::Stop { name } => {
                let vm = self.resolve_vm(name.as_deref())?;
                run_all(&mut self.runner, &Self::adapter(&vm)?.stop(&vm.vm()))?;
                Ok(Outcome::new(
                    format!("VM {} is shutting down", vm.name),
                    json!({ "vm": vm.name }),
                )
                .next(&["tilo-dev vm start"]))
            }
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
                .next(&["tilo-dev vm status"]))
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

    fn vm_list(&self) -> Result<Outcome, CliError> {
        let registry = self.registry()?;
        let current = self.resolve_vm(None).ok().map(|vm| vm.name);
        let mut lines = vec![format!("registry: {}", self.registry_path.display())];
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
            outcome = outcome.next(&[
                "tilo-dev vm create <name> --image <dev image> --disk <path>",
                "tilo-dev guide platforms",
            ]);
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
        .next(&["tilo-dev status"]))
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
                    "  reachable; tilo-devd {} (protocol {}, {})",
                    remote.hello.daemon_version, remote.hello.protocol, remote.hello.arch
                ));
                insert(&mut data, "reachable", json!(true));
                insert(&mut data, "daemon", json!(remote.hello));
                Ok(Outcome::new(lines.join("\n"), data).next(&["tilo-dev status"]))
            }
            Err(error) => {
                lines.push(format!("  not reachable: {error}"));
                insert(&mut data, "reachable", json!(false));
                insert(&mut data, "error", error.to_json());
                let mut outcome = Outcome::new(lines.join("\n"), data).next(&["tilo-dev vm start"]);
                outcome
                    .next
                    .extend(Self::debug_commands(adapter.as_ref(), &vm.name));
                Ok(outcome)
            }
        }
    }

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
        let entry = VmEntry {
            hypervisor: args
                .hypervisor
                .unwrap_or_else(|| hypervisor::native().into()),
            ssh: args.ssh,
            image: args.image,
            disk: args.disk,
            arch: (arch != host).then_some(arch),
            emulated: args.emulated,
            memory_mb: args.memory_mb,
            cpus: args.cpus,
            ssh_port: args.ssh_port,
            template: args.template,
            ..VmEntry::default()
        };
        if let Some(existing) = registry.vms.get(&args.name) {
            return Ok(Outcome::new(
                format!(
                    "VM {} is already registered ({}); nothing to do",
                    args.name, existing.hypervisor
                ),
                json!({ "vm": args.name, "created": false }),
            )
            .next(&["tilo-dev vm start", "tilo-dev vm status"]));
        }
        if entry
            .image
            .as_deref()
            .is_some_and(|i| i.starts_with(REGISTRY))
        {
            registry_login(&mut self.runner)?;
        }
        let adapter = hypervisor::adapter(&entry.hypervisor)?;
        let commands = adapter.create(&Vm {
            name: &args.name,
            entry: &entry,
        })?;
        run_all(&mut self.runner, &commands)?;
        registry.vms.insert(args.name.clone(), entry);
        if registry.default.is_none() {
            registry.default = Some(args.name.clone());
        }
        registry.save(&self.registry_path)?;
        let text = format!(
            "VM {} created with {} and registered in {}",
            args.name,
            adapter.name(),
            self.registry_path.display()
        );
        Ok(Outcome::new(
            text,
            json!({ "vm": args.name, "created": true, "commands": commands }),
        )
        .next(&["tilo-dev vm start", "tilo-dev vm use <name>"]))
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
        while started.elapsed() < Duration::from_secs(180) {
            if self.connect(&vm).is_ok() {
                reachable = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
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
        Ok(Outcome::new(
            format!("VM {} is up; SSH answered after {seconds:.0}s", vm.name),
            json!({ "vm": vm.name, "boot_seconds": seconds }),
        )
        .next(&["tilo-dev status", "tilo-dev slot create --headless"]))
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
            "/var/lib/tilo-dev/locks/vm.lock",
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
        .next(&["tilo-dev vm stop", "tilo-dev vm start"]))
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
                    "tilo-dev swap <component>",
                    "tilo-dev shot",
                    "tilo-dev control actions list",
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
                    outcome = outcome.next(&["tilo-dev slot create --headless"]);
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
                    .next(&["tilo-dev logs", "tilo-dev shot"]))
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
                Ok(Outcome::new(text, json!(result)).next(&["tilo-dev gc --dry-run"]))
            }
        }
    }

    // ---- swap -----------------------------------------------------------------

    fn swap(&mut self, target: &[String], from: &str) -> Result<Outcome, CliError> {
        let (slot, component) = match target {
            [component] => (self.slot_name(None)?, component.clone()),
            [slot, component] => (slot.clone(), component.clone()),
            _ => return Err(CliError::InvalidSource(target.join(" "))),
        };
        self.receipt.set("slot", &slot);
        self.receipt.set("component", &component);
        let manifest = self.manifest()?;
        let entry = manifest.as_ref().and_then(|m| m.get(&component)).cloned();
        if let Some(c) = &entry {
            if c.boot {
                let driver = c
                    .scenario
                    .clone()
                    .unwrap_or_else(|| "os/dev/vm.sh boot".into());
                return Err(CliError::BootComponent {
                    name: component,
                    driver,
                });
            }
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
        let layer_ref = if from == "worktree" {
            None
        } else if let Some(hash) = from.strip_prefix("layer:") {
            Some(hash.to_owned())
        } else if from.starts_with("sha:") {
            return Err(CliError::Unsupported {
                what: "--from sha:<commit>".into(),
                alternative: "check the commit out in a worktree and use --from worktree, or swap a layer already in the VM: --from layer:<hash>".into(),
            });
        } else {
            return Err(CliError::InvalidSource(from.into()));
        };
        let vm = self.resolve_vm(None)?;
        let mut remote = self.connect(&vm)?;
        let status = remote.call(Status { slot: None })?;
        let floor = self.registry()?.min_free_bytes();
        if let Some(free) = status.storage.free_bytes
            && free < floor
        {
            return Err(CliError::LowDisk {
                place: format!("VM {}", vm.name),
                free: human_bytes(free),
                floor: human_bytes(floor),
            });
        }
        let layer = if let Some(hash) = layer_ref {
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
            self.build_layer(&mut remote, entry)?
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
            "tilo-dev shot",
            "tilo-dev control state-dump --pretty",
            "tilo-dev logs",
            "tilo-dev rollback",
        ]))
    }

    fn build_layer(
        &mut self,
        remote: &mut Remote,
        component: &Component,
    ) -> Result<String, CliError> {
        if !cfg!(target_os = "linux") {
            return Err(CliError::Unsupported {
                what: "building a component on this host".into(),
                alternative: "build on a Linux host, or swap a layer that is already in the VM: --from layer:<hash>".into(),
            });
        }
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self
            .runner
            .output(&CommandLine::new(
                "cargo",
                &["metadata", "--no-deps", "--format-version", "1"],
            ))
            .ok()
            .and_then(|o| serde_json::from_str(&o.stdout).ok())
            .unwrap_or(Value::Null);
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
        let install = build::install(component, &staging.tree());
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
        let mut features = component.features.clone();
        features.extend(extra);
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
            SeatCommand::Claim { slot, steal } => {
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
                    .next(&["tilo-dev vm shot", "tilo-dev seat release"]))
            }
            SeatCommand::Release { slot } => {
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

    fn gc(&mut self, dry_run: bool, keep: Option<u32>) -> Result<Outcome, CliError> {
        let keep = keep.unwrap_or(self.registry()?.keep_generations());
        let vm = self.resolve_vm(None)?;
        let result = self.connect(&vm)?.call(Gc { keep, dry_run })?;
        let verb = if dry_run { "would remove" } else { "removed" };
        let text = format!(
            "{verb} {} layers ({}) and {} generations; keeping the current and newest {keep} per slot",
            result.removed_layers.len(),
            human_bytes(result.freed_bytes),
            result.removed_generations.len()
        );
        let mut outcome = Outcome::new(text, json!(result));
        if dry_run {
            outcome = outcome.next(&["tilo-dev gc"]);
        }
        Ok(outcome)
    }
}

fn yes(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
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

fn doctor_outcome(checks: &[Value]) -> Outcome {
    let failed = checks.iter().any(|c| c["state"] == "fail");
    let text = checks
        .iter()
        .map(|c| {
            let mark = match c["state"].as_str() {
                Some("ok") => "ok  ",
                Some("warn") => "warn",
                _ => "FAIL",
            };
            let mut line = format!(
                "{mark} {:<18} {}",
                c["check"].as_str().unwrap_or_default(),
                c["detail"].as_str().unwrap_or_default()
            );
            if let Some(fix) = c["fix"].as_str() {
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
        }
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
                    unit: "tilo-dev-session@b.service".into(),
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
        assert!(looks_like_deploy(&cmd("rm -rf /var/lib/tilo-dev/layers/x")));
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

    #[test]
    fn bytes_read_well() {
        assert_eq!(human_bytes(5 << 30), "5.0 GB");
        assert_eq!(human_bytes(300 << 20), "300 MB");
    }
}
