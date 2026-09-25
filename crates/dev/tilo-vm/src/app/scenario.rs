//! `tilo-vm scenario install`: the glue between the scenario driver and the
//! host — `os/dev/vm.sh` for the VM, the console socket for the guest, the
//! receipt for timings, and the artifact directory.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{App, Outcome};
use crate::cli::InstallScenarioArgs;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};
use crate::scenario::console::{Console, SocketConsole};
use crate::scenario::drive::{self, Choices, RealPace};
use crate::scenario::fixture::{self, Preserved, Tools};
use crate::scenario::{Layout, Unlock, scrub};

/// Where `os/dev/build-custom-installer.sh` writes the medium.
pub const DEFAULT_MEDIUM: &str = "os/output/installer/qcow2/disk.qcow2";
/// How long a booting installer gets to answer its console.
const INSTALLER_BOOT: Duration = Duration::from_secs(20 * 60);
/// How long the installer gets to power the VM off after Finish.
const POWER_OFF: Duration = Duration::from_secs(180);
/// How long a cold-booted target gets to reach the greeter.
const TARGET_BOOT: Duration = Duration::from_secs(15 * 60);
/// Unlock attempts typed at the target's boot prompt at most.
const UNLOCK_ATTEMPTS: u32 = 8;

/// Everything one run needs to know, settled before anything boots.
struct Plan {
    root: PathBuf,
    medium: PathBuf,
    layout: Layout,
    unlock: Unlock,
    graphics: crate::scenario::Graphics,
    cycle: String,
    out: PathBuf,
    storage: PathBuf,
    cycle_dir: PathBuf,
    run: PathBuf,
    install_timeout: Duration,
    keep: bool,
}

impl Plan {
    fn console(&self) -> PathBuf {
        self.run.join("console.sock")
    }

    fn target(&self) -> PathBuf {
        self.cycle_dir.join("target.qcow2")
    }

    fn tpm_flag(&self) -> &'static str {
        if self.unlock == Unlock::TpmPin {
            "--tpm"
        } else {
            "--no-tpm"
        }
    }

    /// The environment every `vm.sh` call of this run shares: one named
    /// cycle, so disk, TPM and firmware state persist from the installer
    /// boot to the target boot and nowhere else.
    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("TILO_VM_CYCLE".into(), self.cycle.clone()),
            (
                "TILO_INSTALLER_OUTPUT".into(),
                self.storage.display().to_string(),
            ),
            (
                "TILO_INSTALLER_DISK".into(),
                self.medium.display().to_string(),
            ),
            ("TILO_RUN".into(), self.run.display().to_string()),
        ]
    }

    fn vm(&self, args: &[&str]) -> CommandLine {
        let mut line = CommandLine::new("bash", &["os/dev/vm.sh"]);
        for arg in args {
            line = line.arg(*arg);
        }
        for (key, value) in self.env() {
            line = line.env(&key, value);
        }
        line
    }

    fn describe(&self) -> Value {
        json!({
            "scenario": "install",
            "medium": self.medium,
            "layout": self.layout.name(),
            "encrypt": self.unlock.name(),
            "graphics": self.graphics,
            "cycle": self.cycle,
            "artifacts": self.out,
            "console": self.console(),
            "target": self.target(),
            "install_timeout_seconds": self.install_timeout.as_secs(),
            "keep": self.keep,
            "commands": [
                self.vm(&["init-cycle"]).display(),
                self.vm(&["boot-installer", self.graphics.flag(), self.tpm_flag()]).display(),
                format!("drive the installer through its actions on {}", self.console().display()),
                self.vm(&["boot-target", self.graphics.flag(), self.tpm_flag()]).display(),
                format!("verify the greeter on {}", self.console().display()),
            ],
        })
    }
}

/// `vm.sh tool …`: host tools in the same environment `vm.sh` uses.
struct VmTools<'a> {
    plan: &'a Plan,
}

impl Tools for VmTools<'_> {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<(), String> {
        let mut all = vec!["tool", program];
        all.extend_from_slice(args);
        piped(self.plan, &all, stdin.unwrap_or_default()).map_err(|error| error.to_string())
    }
}

/// Run `vm.sh` with `input` on stdin. Used for what must never reach argv
/// (the unlock secret) and for tools that read a script from stdin.
fn piped(plan: &Plan, args: &[&str], input: &str) -> Result<(), CliError> {
    let line = plan.vm(args);
    let mut child = std::process::Command::new(&line.program)
        .args(&line.args)
        .envs(line.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&plan.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(CliError::io(format!("starting {}", line.display())))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(CliError::io("writing to vm.sh"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(CliError::io(format!("waiting for {}", line.display())))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CliError::CommandFailed {
            command: line.display(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn say(line: &str) {
    eprintln!("tilo-vm: {line}");
}

fn failed(phase: &str, detail: impl Into<String>, plan: &Plan) -> CliError {
    CliError::Scenario {
        phase: phase.into(),
        detail: detail.into(),
        artifacts: plan.out.display().to_string(),
        console: plan.console().display().to_string(),
    }
}

fn write_json(dir: &Path, name: &str, value: &Value) -> Result<(), CliError> {
    std::fs::write(dir.join(name), format!("{value:#}\n"))
        .map_err(CliError::io(format!("writing {name}")))
}

impl App {
    #[expect(
        clippy::too_many_lines,
        reason = "settles the whole plan before anything boots, then reports it"
    )]
    pub(super) fn scenario_install(
        &mut self,
        args: &InstallScenarioArgs,
    ) -> Result<Outcome, CliError> {
        let root = self
            .worktree
            .as_ref()
            .map(|worktree| worktree.root.clone())
            .ok_or_else(|| CliError::NeedsWorktree {
                what: "tilo-vm scenario install".into(),
            })?;
        let medium = args
            .medium
            .clone()
            .unwrap_or_else(|| root.join(DEFAULT_MEDIUM));
        let medium = if medium.is_absolute() {
            medium
        } else {
            std::env::current_dir().unwrap_or_default().join(medium)
        };
        if !medium.is_file() {
            return Err(CliError::ScenarioMedium {
                path: medium.display().to_string(),
            });
        }
        let cycle = format!(
            "scenario-install-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        );
        let out = args
            .out
            .clone()
            .unwrap_or_else(|| root.join("target/tilo-vm-scenarios").join(&cycle));
        let storage = out.join("storage");
        // Exactly the runtime directory vm.sh derives for a named cycle;
        // init-cycle refuses to clear any other.
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|path| !path.is_empty())
            .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        let plan = Plan {
            root,
            medium,
            layout: args.layout,
            unlock: args.encrypt,
            graphics: args.graphics,
            cycle_dir: storage.join("vm-cycles").join(&cycle),
            run: runtime.join(format!("tilo-vm-{cycle}")),
            cycle,
            storage,
            out,
            install_timeout: Duration::from_secs(args.install_timeout),
            keep: args.keep,
        };
        if args.dry_run {
            let plan_json = plan.describe();
            let text = format!(
                "install scenario (dry run; nothing booted)\n{}",
                plan_json
                    .get("commands")
                    .and_then(Value::as_array)
                    .map(|commands| commands
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|c| format!("  {c}"))
                        .collect::<Vec<_>>()
                        .join("\n"))
                    .unwrap_or_default()
            );
            return Ok(Outcome::new(text, json!({ "plan": plan_json }))
                .next(&["tilo-vm scenario install", "tilo-vm guide scenarios"]));
        }
        std::fs::create_dir_all(&plan.out)
            .map_err(CliError::io("creating the artifact directory"))?;
        write_json(&plan.out, "scenario.json", &plan.describe())?;
        self.receipt.set("scenario", plan.describe());
        let secret = plan.unlock.scenario_secret();
        let result = self.run_install_scenario(&plan, &secret);
        // Whatever happened, the VM stops and the evidence is scrubbed.
        let _ = self.runner.output(&plan.vm(&["stop"]));
        if !plan.keep {
            let _ = std::fs::remove_dir_all(&plan.cycle_dir);
            let _ = std::fs::remove_dir_all(&plan.run);
        }
        let withheld = scrub(&plan.out, secret.expose())
            .map_err(CliError::io("scrubbing the artifact directory"))?;
        let mut report = result?;
        if !withheld.is_empty() {
            return Err(failed(
                "artifacts",
                format!(
                    "{} artifact(s) contained the unlock secret or recovery material and were emptied: {}",
                    withheld.len(),
                    withheld
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                &plan,
            ));
        }
        if let Value::Object(map) = &mut report {
            map.insert("plan".into(), plan.describe());
            map.insert("phases".into(), json!(self.receipt.summary()));
        }
        write_json(&plan.out, "result.json", &report)?;
        let text = format!(
            "install scenario passed: {} install onto {} ({}), first boot reached the greeter on the installed image {}\nphases: {}\nartifacts: {}",
            plan.layout.name(),
            report.get("target").and_then(Value::as_str).unwrap_or("?"),
            plan.unlock.name(),
            report
                .get("installed_image")
                .and_then(Value::as_str)
                .map_or("?", |digest| digest.get(..16).unwrap_or(digest)),
            self.receipt.summary(),
            plan.out.display()
        );
        Ok(Outcome::new(text, report).next(&[
            "tilo-vm scenario install --layout guided-alongside",
            "tilo-vm guide scenarios",
        ]))
    }

    #[expect(clippy::too_many_lines, reason = "one linear scenario, phase by phase")]
    fn run_install_scenario(
        &mut self,
        plan: &Plan,
        secret: &crate::scenario::Secret,
    ) -> Result<Value, CliError> {
        say(&format!(
            "install scenario {} ({} layout, {} unlock); artifacts in {}",
            plan.cycle,
            plan.layout.name(),
            plan.unlock.name(),
            plan.out.display()
        ));
        let started = Instant::now();
        let init = self.runner.output(&plan.vm(&["init-cycle"]))?;
        if !init.success {
            return Err(failed("prepare", init.stderr.trim(), plan));
        }
        let mut preserved: Vec<Preserved> = Vec::new();
        if plan.layout != Layout::Erase {
            let scratch = fixture::scratch_dir(&plan.cycle_dir);
            std::fs::create_dir_all(&scratch).map_err(CliError::io("creating fixture scratch"))?;
            let raw = plan.cycle_dir.join("target.raw");
            preserved = fixture::build(plan.layout, &raw, &scratch, &mut VmTools { plan })
                .map_err(|detail| failed("prepare", detail, plan))?;
            let target = plan.target().display().to_string();
            VmTools { plan }
                .run(
                    "qemu-img",
                    &[
                        "create",
                        "-f",
                        "qcow2",
                        "-F",
                        "raw",
                        "-b",
                        &raw.display().to_string(),
                        &target,
                    ],
                    None,
                )
                .map_err(|detail| failed("prepare", detail, plan))?;
            write_json(&plan.out, "alongside-before.json", &json!(preserved))?;
        }
        self.receipt
            .record("prepare", started.elapsed().as_secs_f64());

        let boot = Instant::now();
        let booted = self.runner.output(&plan.vm(&[
            "boot-installer",
            plan.graphics.flag(),
            plan.tpm_flag(),
        ]))?;
        if !booted.success {
            return Err(failed("boot-installer", booted.stderr.trim(), plan));
        }
        say("installer medium booting; waiting for its console");
        let mut console = SocketConsole::new(&plan.console());
        let first = wait_for_app(&mut console, "installer", INSTALLER_BOOT).ok_or_else(|| {
            failed(
                "boot-installer",
                format!(
                    "the installer never answered on its console within {}s",
                    INSTALLER_BOOT.as_secs()
                ),
                plan,
            )
        })?;
        write_json(&plan.out, "installer-state-first.json", &first)?;
        self.receipt
            .record("boot-installer", boot.elapsed().as_secs_f64());

        let install = Instant::now();
        let choices = Choices {
            layout: plan.layout,
            unlock: plan.unlock,
            username: "scenario".into(),
            full_name: "Scenario User".into(),
            hostname: "scenario-tilo".into(),
            secret: crate::scenario::Secret::new(secret.expose()),
        };
        let installed = drive::install(
            &mut console,
            &choices,
            plan.install_timeout,
            &mut RealPace,
            &mut |line| say(line),
        );
        // The live installer's serial console is never kept: the install
        // backend's private output carries the one-time recovery key.
        std::fs::write(
            plan.out.join("installer-serial-withheld.txt"),
            "withheld: live-installer serial may contain the one-time recovery key\n",
        )
        .map_err(CliError::io("writing the withheld-serial notice"))?;
        if let Ok(state) = console.state() {
            write_json(&plan.out, "installer-state-last.json", &state)?;
        }
        let installed = installed.map_err(|error| failed("install", error.to_string(), plan))?;
        write_json(&plan.out, "install.json", &json!(installed))?;
        self.receipt
            .record("install", install.elapsed().as_secs_f64());

        let off = Instant::now();
        if !self.wait_stopped(plan, POWER_OFF) {
            return Err(failed(
                "power-off",
                "the installer's Power off action did not stop the VM",
                plan,
            ));
        }
        self.receipt
            .record("power-off", off.elapsed().as_secs_f64());

        if !preserved.is_empty() {
            let verify = Instant::now();
            let changed = fixture::changed_cow(
                &plan.target(),
                &plan.cycle_dir.join("verify-extents"),
                &preserved,
                &mut VmTools { plan },
            )
            .map_err(|detail| failed("verify-alongside", detail, plan))?;
            write_json(
                &plan.out,
                "alongside-after.json",
                &json!({ "preserved": preserved, "changed": changed }),
            )?;
            if !changed.is_empty() {
                return Err(failed(
                    "verify-alongside",
                    format!(
                        "the install changed preserved storage: {}",
                        changed.join(", ")
                    ),
                    plan,
                ));
            }
            self.receipt
                .record("verify-alongside", verify.elapsed().as_secs_f64());
        }

        let target_boot = Instant::now();
        let booted = self.runner.output(&plan.vm(&[
            "boot-target",
            plan.graphics.flag(),
            plan.tpm_flag(),
        ]))?;
        if !booted.success {
            return Err(failed("boot-target", booted.stderr.trim(), plan));
        }
        say("installed target booting; unlocking at its boot prompt");
        let greeter = self.unlock_to_greeter(plan, &mut console, secret)?;
        let serial = plan.run.join("serial.log");
        if serial.is_file() {
            let _ = std::fs::copy(&serial, plan.out.join("target-serial.log"));
        }
        write_json(&plan.out, "greeter-state.json", &greeter)?;
        self.receipt
            .record("first-boot", target_boot.elapsed().as_secs_f64());

        let installed_image = greeter
            .pointer("/image/composefs")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let (Some(live), Some(booted_image)) = (&installed.composefs, &installed_image) else {
            return Err(failed(
                "first-boot",
                "the installer or the greeter did not report the composefs image it booted, so the installed image cannot be verified",
                plan,
            ));
        };
        if live != booted_image {
            return Err(failed(
                "first-boot",
                format!(
                    "first boot runs composefs {booted_image}, not the {live} the installer wrote"
                ),
                plan,
            ));
        }
        if greeter.get("phase").and_then(Value::as_str) != Some("sign-in") {
            return Err(failed(
                "first-boot",
                format!("the greeter is not offering sign-in: {greeter}"),
                plan,
            ));
        }
        Ok(json!({
            "scenario": "install",
            "layout": plan.layout.name(),
            "encrypt": plan.unlock.name(),
            "target": installed.target,
            "installer_steps": installed.steps,
            "installer_image": live,
            "installed_image": booted_image,
            "greeter": greeter,
            "preserved": preserved,
            "artifacts": plan.out,
        }))
    }

    fn wait_stopped(&mut self, plan: &Plan, limit: Duration) -> bool {
        let deadline = after(limit);
        while Instant::now() < deadline {
            if self
                .runner
                .output(&plan.vm(&["status"]))
                .is_ok_and(|status| status.stdout.trim_start().starts_with("stopped"))
            {
                return true;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        false
    }

    /// Type the unlock secret at the target's boot prompt until the greeter
    /// answers its console. The prompt is before userspace, so the keyboard
    /// is the only door; the greeter answering is what stops the typing, so
    /// a late attempt cannot land in its username field.
    fn unlock_to_greeter(
        &mut self,
        plan: &Plan,
        console: &mut SocketConsole,
        secret: &crate::scenario::Secret,
    ) -> Result<Value, CliError> {
        let deadline = after(TARGET_BOOT);
        let first_attempt = after(Duration::from_secs(30));
        let mut typed = 0_u32;
        while Instant::now() < deadline {
            if let Ok(state) = console.state()
                && state.get("app").and_then(Value::as_str) == Some("greeter")
            {
                self.receipt.set("unlock_attempts", typed);
                return Ok(state);
            }
            let running = self
                .runner
                .output(&plan.vm(&["status"]))
                .is_ok_and(|status| status.stdout.trim_start().starts_with("running"));
            if !running {
                return Err(failed(
                    "first-boot",
                    "the installed target stopped before reaching the greeter",
                    plan,
                ));
            }
            if Instant::now() >= first_attempt && typed < UNLOCK_ATTEMPTS {
                piped(plan, &["type", "-"], secret.expose())?;
                let _ = self.runner.output(&plan.vm(&["keys", "ret"]));
                typed = typed.saturating_add(1);
                std::thread::sleep(Duration::from_secs(15));
            } else {
                std::thread::sleep(Duration::from_secs(5));
            }
        }
        Err(failed(
            "first-boot",
            format!(
                "no greeter answered within {}s after {typed} unlock attempt(s)",
                TARGET_BOOT.as_secs()
            ),
            plan,
        ))
    }
}

/// `limit` from now, saturating instead of overflowing.
fn after(limit: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(limit).unwrap_or(now)
}

/// Poll until the console answers as `app`, returning its first state.
fn wait_for_app(console: &mut SocketConsole, app: &str, limit: Duration) -> Option<Value> {
    let deadline = after(limit);
    while Instant::now() < deadline {
        if let Ok(state) = console.state()
            && state.get("app").and_then(Value::as_str) == Some(app)
        {
            return Some(state);
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(layout: Layout) -> Plan {
        Plan {
            root: PathBuf::from("/w"),
            medium: PathBuf::from("/w/os/output/installer/qcow2/disk.qcow2"),
            layout,
            unlock: Unlock::Passphrase,
            graphics: crate::scenario::Graphics::Software,
            cycle: "scenario-install-x".into(),
            out: PathBuf::from("/w/target/tilo-vm-scenarios/scenario-install-x"),
            storage: PathBuf::from("/w/target/tilo-vm-scenarios/scenario-install-x/storage"),
            cycle_dir: PathBuf::from(
                "/w/target/tilo-vm-scenarios/scenario-install-x/storage/vm-cycles/scenario-install-x",
            ),
            run: PathBuf::from("/run/user/1000/tilo-vm-scenario-install-x"),
            install_timeout: Duration::from_secs(3600),
            keep: false,
        }
    }

    #[test]
    fn every_vm_call_shares_one_named_cycle_and_the_plan_names_its_console() {
        let plan = plan(Layout::GuidedAlongside);
        let boot = plan.vm(&["boot-installer", "--2d", "--no-tpm"]);
        assert_eq!(
            boot.args,
            ["os/dev/vm.sh", "boot-installer", "--2d", "--no-tpm"]
        );
        for key in [
            "TILO_VM_CYCLE",
            "TILO_INSTALLER_OUTPUT",
            "TILO_INSTALLER_DISK",
            "TILO_RUN",
        ] {
            assert!(boot.env.iter().any(|(k, _)| k == key), "{key}");
        }
        let described = plan.describe();
        assert_eq!(described["layout"], "guided-alongside");
        assert_eq!(
            described["console"],
            "/run/user/1000/tilo-vm-scenario-install-x/console.sock"
        );
        // The cycle directory is where vm.sh itself derives it; init-cycle
        // refuses to clear anything else.
        assert_eq!(
            plan.cycle_dir,
            plan.storage.join("vm-cycles").join(&plan.cycle)
        );
        assert!(
            !described.to_string().contains("scenario-passphrase"),
            "the plan never carries the secret"
        );
    }
}
