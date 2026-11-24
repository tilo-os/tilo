//! `tilo-vm scenario four-devices` and `laptop-phone-server`: boot a fleet of devices
//! on tilo-vm's private network with `os/dev/vm.sh` (the same cycle machinery
//! the install scenario uses) and drive the flow's steps through the guests'
//! own logins.

use std::fmt::Write as _;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::scenario::write_json;
use super::{App, Outcome};
use crate::cli::FlowScenarioArgs;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};
use crate::scenario::flows::{self, Fleet, Flow, Guest, Status, StepReport};

/// The disks a flow boots, when they are supplied. A guest whose disk is
/// absent is reported by name instead of being waited for.
struct GuestVm {
    guest: Guest,
    arch: &'static str,
    disk: Option<PathBuf>,
    ssh_port: u16,
    cycle: String,
    run: PathBuf,
    mac: String,
    address: std::net::Ipv4Addr,
    user: String,
    password: String,
}

impl GuestVm {
    fn running(&self) -> bool {
        self.disk.is_some()
    }

    fn env(&self, plan: &FlowPlan) -> Vec<(String, String)> {
        let mut env = vec![
            ("TILO_RUN".into(), self.run.display().to_string()),
            ("TILO_SSH_PORT".into(), self.ssh_port.to_string()),
            ("TILO_VM_ARCH".into(), self.arch.into()),
            ("TILO_USER".into(), self.user.clone()),
            ("TILO_PASS".into(), self.password.clone()),
            ("TILO_SSH_BOUND".into(), "60".into()),
        ];
        if let Some(disk) = &self.disk {
            env.push(("TILO_DISK".into(), disk.display().to_string()));
        }
        env.push((
            "TILO_VM_LAN_GROUP".into(),
            format!("{}:{}", plan.lan_group, self.mac),
        ));
        if self.arch != host_arch() {
            // A guest of another architecture runs on the software emulator;
            // the host's KVM cannot accelerate it.
            env.push(("TILO_ACCEL".into(), "tcg".into()));
        }
        env
    }

    fn vm(&self, plan: &FlowPlan, args: &[&str]) -> CommandLine {
        let mut line = CommandLine::new("bash", &["os/dev/vm.sh"]);
        for arg in args {
            line = line.arg(*arg);
        }
        for (key, value) in self.env(plan) {
            line = line.env(&key, value);
        }
        line
    }
}

struct FlowPlan {
    flow: Flow,
    out: PathBuf,
    runtime: PathBuf,
    lan_group: String,
    vms: Vec<GuestVm>,
    timeout: Duration,
    keep: bool,
}

impl FlowPlan {
    fn new(flow: Flow, root: &std::path::Path, args: &FlowScenarioArgs) -> Result<Self, CliError> {
        let default_desktop = root.join("os/output/qcow2/disk.qcow2");
        let desktop = match args.desktop_disk.clone() {
            Some(path) => Some(disk(&path, "the x86-64 desktop disk")?),
            // A dry run reports the plan even before anything is built; a run
            // refuses a missing disk up front.
            None if args.dry_run => default_desktop
                .is_file()
                .then(|| disk(&default_desktop, "the x86-64 desktop disk"))
                .transpose()?,
            None => Some(disk(&default_desktop, "the x86-64 desktop disk")?),
        };
        let arm = args
            .arm_disk
            .clone()
            .map(|path| disk(&path, "the aarch64 laptop disk"))
            .transpose()?;
        let server = args
            .server_disk
            .clone()
            .map(|path| disk(&path, "the server image disk"))
            .transpose()?;
        let user = args
            .user
            .clone()
            .or_else(|| std::env::var("TILO_USER").ok())
            .unwrap_or_else(|| "tester".into());
        let password = std::env::var("TILO_PASS").unwrap_or_else(|_| "tilo".into());
        let lan_group = args
            .lan
            .clone()
            .unwrap_or_else(|| format!("{}:{}", crate::lan::GROUP, crate::lan::port()));
        let timeout = Duration::from_secs(args.timeout.max(60));
        let cycle = format!(
            "scenario-{}-{}-{}",
            flow.name(),
            jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ"),
            std::process::id()
        );
        let out = args
            .out
            .clone()
            .unwrap_or_else(|| root.join("target/tilo-vm-scenarios").join(&cycle));
        let runtime = tilo_paths::xdg::runtime_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let mut taken = Vec::new();
        let mut guest_vm = |guest: Guest,
                            arch: &'static str,
                            disk: Option<PathBuf>,
                            index: usize|
         -> Result<GuestVm, CliError> {
            let mac = crate::lan::new_mac(|mac| taken.iter().any(|seen| seen == mac));
            taken.push(mac.clone());
            let address = crate::lan::address(&mac).ok_or_else(|| CliError::Unsupported {
                what: "scenario network address".into(),
                alternative: "the network MAC could not name an address".into(),
            })?;
            let ssh_port = free_port()?;
            Ok(GuestVm {
                guest,
                arch,
                disk,
                ssh_port,
                cycle: format!("{cycle}-{index}"),
                run: runtime.join(format!("tilo-vm-{cycle}-{index}")),
                mac,
                address,
                user: user.clone(),
                password: password.clone(),
            })
        };
        let mut vms = vec![guest_vm(Guest::DesktopX86, "x86_64", desktop, 0)?];
        if flow == Flow::FourDevices {
            vms.push(guest_vm(Guest::LaptopArm, "aarch64", arm, 1)?);
        }
        vms.push(guest_vm(Guest::Server, "x86_64", server, 2)?);
        Ok(Self {
            flow,
            out,
            runtime,
            lan_group,
            vms,
            timeout,
            keep: args.keep,
        })
    }

    fn vm(&self, guest: Guest) -> Option<&GuestVm> {
        self.vms.iter().find(|vm| vm.guest == guest)
    }

    fn unavailable(&self, guest: Guest) -> Option<String> {
        match (guest, self.vm(guest)) {
            (Guest::DesktopX86, None | Some(GuestVm { disk: None, .. })) => Some(
                "an x86-64 desktop disk (--desktop-disk; build one with os/build.sh --local)".into(),
            ),
            (Guest::LaptopArm, None | Some(GuestVm { disk: None, .. })) => Some(
                "an aarch64 laptop disk (--arm-disk; build one with TILO_VM_ARCH=aarch64 os/build.sh --local)"
                    .into(),
            ),
            (Guest::Server, None | Some(GuestVm { disk: None, .. })) => Some(
                "a bootable disk of the server image (--server-disk; image-host is an OCI archive, so install it onto a disk first)"
                    .into(),
            ),
            _ => None,
        }
    }

    fn describe(&self) -> Value {
        json!({
            "scenario": self.flow.name(),
            "artifacts": self.out,
            "lan": self.lan_group,
            "timeout_seconds": self.timeout.as_secs(),
            "keep": self.keep,
            "devices": self.vms.iter().map(|vm| json!({
                "device": vm.guest.name(),
                "arch": vm.arch,
                "mac": vm.mac,
                "disk": vm.disk,
                "address": vm.address.to_string(),
                "ssh_port": vm.ssh_port,
                "boot": vm.running().then_some(vm.vm(self, &["boot", &display_flag()]).display()),
            })).collect::<Vec<_>>(),
            "steps": self.flow.steps().iter().map(|step| json!({
                "step": step.id,
                "proves": step.proves,
                "needs": match step.requires {
                    flows::Requirement::Action { needle, missing, .. } =>
                        json!({"kind": "action", "action": needle, "missing": missing}),
                    flows::Requirement::Command(command) =>
                        json!({"kind": "command", "command": command}),
                    flows::Requirement::Feature(feature) =>
                        json!({"kind": "feature", "feature": feature}),
                },
            })).collect::<Vec<_>>(),
        })
    }
}

fn disk(path: &std::path::Path, what: &str) -> Result<PathBuf, CliError> {
    let metadata = std::fs::symlink_metadata(path);
    if !metadata.is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink()) {
        return Err(CliError::ScenarioInput {
            what: what.into(),
            path: path.display().to_string(),
        });
    }
    path.canonicalize()
        .map_err(CliError::io("resolving the scenario disk"))
}

/// The display flag every VM boots with: `TILO_VM_DISPLAY` (`2d` by default,
/// as the boot checks set it), so a scenario can be pointed at GL the same way.
fn display_flag() -> String {
    match std::env::var("TILO_VM_DISPLAY").as_deref() {
        Ok("gl") => "--gl".into(),
        Ok("window") => "--window".into(),
        _ => "--2d".into(),
    }
}

fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" | "arm64" => "aarch64",
        _ => "x86_64",
    }
}

fn free_port() -> Result<u16, CliError> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(CliError::io("finding a free SSH forward"))
}

struct VmFleet<'a> {
    plan: &'a FlowPlan,
    runner: &'a mut crate::hypervisor::SystemRunner,
}

impl VmFleet<'_> {
    fn output(&mut self, line: &CommandLine) -> Result<String, String> {
        let output = self
            .runner
            .output(line)
            .map_err(|error| error.to_string())?;
        if output.success {
            Ok(output.stdout)
        } else {
            Err(trimmed(&output.stderr, &output.stdout))
        }
    }
}

fn trimmed(stderr: &str, stdout: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        stdout.trim().to_owned()
    } else {
        stderr.to_owned()
    }
}

impl Fleet for VmFleet<'_> {
    fn ssh(&mut self, guest: Guest, command: &str) -> Result<String, String> {
        let Some(vm) = self.plan.vm(guest) else {
            return Err(format!("{} is not booted", guest.name()));
        };
        let line = vm.vm(self.plan, &["ssh", command]);
        self.output(&line)
    }

    fn has_action(&mut self, guest: Guest, needle: &str) -> Result<bool, String> {
        let output = self.ssh(guest, &format!("tilo action list {needle}"))?;
        Ok(output
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase()))
    }

    fn http(&mut self, guest: Guest, url: &str) -> Result<bool, String> {
        let command = format!(
            "if ! command -v curl >/dev/null; then echo no-curl; elif curl --fail --silent --show-error --max-time 5 --output /dev/null '{url}'; then echo reachable; else echo unreachable; fi"
        );
        let output = self.ssh(guest, &command)?;
        match output.trim() {
            "reachable" => Ok(true),
            "unreachable" => Ok(false),
            "no-curl" => Err(format!("{} has no curl", guest.name())),
            other => Err(format!(
                "unexpected curl answer on {}: {other}",
                guest.name()
            )),
        }
    }

    fn suspend(&mut self, guest: Guest) -> Result<(), String> {
        let Some(vm) = self.plan.vm(guest) else {
            return Err(format!("{} is not booted", guest.name()));
        };
        let line = vm.vm(self.plan, &["stop"]);
        self.output(&line).map(|_| ())
    }

    fn pause(&mut self, seconds: u64) {
        std::thread::sleep(Duration::from_secs(seconds));
    }

    fn unavailable(&self, guest: Guest) -> Option<String> {
        self.plan.unavailable(guest)
    }
}

impl App {
    pub(super) fn scenario_four_devices(
        &mut self,
        args: &FlowScenarioArgs,
    ) -> Result<Outcome, CliError> {
        self.scenario_flow(Flow::FourDevices, args)
    }

    pub(super) fn scenario_laptop_phone_server(
        &mut self,
        args: &FlowScenarioArgs,
    ) -> Result<Outcome, CliError> {
        self.scenario_flow(Flow::LaptopPhoneServer, args)
    }

    fn scenario_flow(&mut self, flow: Flow, args: &FlowScenarioArgs) -> Result<Outcome, CliError> {
        let root = self
            .worktree
            .as_ref()
            .map(|worktree| worktree.root.clone())
            .ok_or_else(|| CliError::NeedsWorktree {
                what: format!("tilo-vm scenario {}", flow.name()),
            })?;
        let plan = FlowPlan::new(flow, &root, args)?;
        let described = plan.describe();
        if args.dry_run {
            let text = format!(
                "{} scenario (dry run; nothing booted)\n{}",
                flow.name(),
                plan.flow
                    .steps()
                    .iter()
                    .map(|step| format!("  {:<12} needs {}", step.id, need(step.requires)))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            return Ok(Outcome::new(text, json!({"plan": described}))
                .next(&["tilo-vm guide scenarios", "tilo-vm scenario --help"]));
        }
        std::fs::create_dir_all(&plan.out)
            .map_err(CliError::io("creating the scenario artifact directory"))?;
        std::fs::create_dir_all(&plan.runtime)
            .map_err(CliError::io("creating the scenario runtime directory"))?;
        write_json(&plan.out, "scenario.json", &described)?;
        let result = self.run_flow(&plan);
        self.stop_fleet(&plan);
        let reports = result?;
        let report = json!({
            "scenario": flow.name(),
            "devices": described.get("devices").cloned().unwrap_or(Value::Null),
            "steps": reports.iter().map(StepReport::to_json).collect::<Vec<_>>(),
            "artifacts": plan.out,
        });
        write_json(&plan.out, "result.json", &report)?;
        let red = reports.iter().find(|report| report.status.is_red());
        let mut text = format!("{} scenario\n", flow.name());
        for report in &reports {
            let line = match &report.status {
                Status::Passed => format!("[ OK ]   {}: {}", report.id, report.proves),
                Status::Missing(what) => format!("[MISS]   {}: needs {what}", report.id),
                Status::Failed(why) => format!("[FAIL]   {}: {why}", report.id),
                Status::Blocked(by) => format!("[ -- ]   {}: blocked by {by}", report.id),
            };
            text.push_str(&line);
            text.push('\n');
        }
        let Some(red) = red else {
            let _ = writeln!(text, "artifacts: {}", plan.out.display());
            return Ok(Outcome::new(text, report).next(&["tilo-vm guide scenarios"]));
        };
        let _ = write!(
            text,
            "\nstopped at {}; artifacts: {}\n",
            red.id,
            plan.out.display()
        );
        let mut outcome = Outcome::new(text, report);
        outcome.failed = true;
        Ok(outcome)
    }

    fn run_flow(&mut self, plan: &FlowPlan) -> Result<Vec<StepReport>, CliError> {
        for vm in &plan.vms {
            if !vm.running() {
                continue;
            }
            let display = display_flag();
            let booted = self.runner.output(&vm.vm(plan, &["boot", &display]))?;
            if !booted.success {
                return Err(CliError::CommandFailed {
                    command: vm.vm(plan, &["boot", &display]).display(),
                    status: booted.status,
                    stderr: booted.stderr.trim().to_owned(),
                });
            }
            self.wait_ssh(plan, vm)?;
            if plan.flow == Flow::LaptopPhoneServer && vm.guest == Guest::DesktopX86 {
                // The flow is a home with no tilo.sh and no internet: keep the
                // host forward for this scenario's own login and drop the
                // guest's route out. The offline step proves it.
                let command = format!(
                    "sudo -n ip route del default 2>/dev/null || printf '%s\\n' '{}' | sudo -S -p '' ip route del default 2>/dev/null || true",
                    vm.password
                );
                let _ = self.runner.output(&vm.vm(plan, &["ssh", &command]));
            }
            if vm.guest != Guest::Server {
                // A person signs in at the greeter. First use is a ceremony the
                // scenario cannot press without published actions; that is the
                // identity step's business, so a sign-in failure is not fatal.
                let _ = self.runner.output(&vm.vm(plan, &["wait"]));
                let _ = self
                    .runner
                    .output(&vm.vm(plan, &["login", "--passcode-only", &vm.password]));
            }
        }
        let mut fleet = VmFleet {
            plan,
            runner: &mut self.runner,
        };
        Ok(flows::run(plan.flow, &mut fleet))
    }

    fn wait_ssh(&mut self, plan: &FlowPlan, vm: &GuestVm) -> Result<(), CliError> {
        let deadline = Instant::now()
            .checked_add(plan.timeout)
            .unwrap_or_else(Instant::now);
        let mut last = String::new();
        while Instant::now() < deadline {
            let probe = vm.vm(plan, &["ssh", "true"]);
            match self.runner.output(&probe) {
                Ok(output) if output.success => return Ok(()),
                Ok(output) => last = trimmed(&output.stderr, &output.stdout),
                Err(error) => last = error.to_string(),
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        Err(CliError::Scenario {
            phase: format!("boot {}", vm.guest.name()),
            detail: format!("no SSH within {}s: {last}", plan.timeout.as_secs()),
            artifacts: plan.out.display().to_string(),
            console: String::new(),
        })
    }

    fn stop_fleet(&mut self, plan: &FlowPlan) {
        for vm in &plan.vms {
            if !vm.running() {
                continue;
            }
            let _ = self.runner.output(&vm.vm(plan, &["stop"]));
            if !plan.keep {
                let _ = std::fs::remove_dir_all(&vm.run);
                let _ = std::fs::remove_dir_all(&vm.cycle);
            }
        }
    }
}

fn need(requirement: flows::Requirement) -> String {
    match requirement {
        flows::Requirement::Action { needle, .. } => format!("the action '{needle}'"),
        flows::Requirement::Command(command) => format!("the command '{command}'"),
        flows::Requirement::Feature(feature) => feature.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_without_a_disk_is_named_rather_than_waited_for() {
        let plan = FlowPlan {
            flow: Flow::FourDevices,
            out: PathBuf::from("/out"),
            runtime: PathBuf::from("/run"),
            lan_group: "239.255.84.76:47123".into(),
            vms: Vec::new(),
            timeout: Duration::from_secs(60),
            keep: false,
        };
        assert!(
            plan.unavailable(Guest::Server)
                .is_some_and(|why| why.contains("--server-disk"))
        );
        assert!(
            plan.unavailable(Guest::LaptopArm)
                .is_some_and(|why| why.contains("--arm-disk"))
        );
        assert!(
            plan.unavailable(Guest::DesktopX86)
                .is_some_and(|why| why.contains("--desktop-disk"))
        );
        let described = plan.describe();
        assert_eq!(described["scenario"], "four-devices");
        assert_eq!(described["steps"].as_array().unwrap().len(), 7);
    }

    #[test]
    fn the_plan_names_each_step_and_its_requirement() {
        let steps = Flow::LaptopPhoneServer.steps();
        let text = steps
            .iter()
            .map(|step| format!("{} {}", step.id, need(step.requires)))
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "web-default the server serving its own web UI by default",
            "web-off the action 'web'",
            "offline the command 'curl'",
            "update-lease the action 'update'",
            "reboot the action 'reboot'",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }
}
