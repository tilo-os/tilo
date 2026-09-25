//! `tilo-vm scenario install`: the glue between the scenario driver and the
//! host — `os/dev/vm.sh` for the VM, the console socket for the guest, the
//! receipt for timings, and the artifact directory.

#[cfg(windows)]
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[cfg(windows)]
use serde::Deserialize;
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
    external_target: Option<PathBuf>,
    skip_target_boot: bool,
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
    #[cfg(windows)]
    ports: WindowsPorts,
}

#[cfg(windows)]
#[derive(Clone, Copy)]
struct WindowsPorts {
    console: u16,
    qmp: u16,
}

#[cfg(windows)]
impl WindowsPorts {
    fn reserve() -> Result<Self, CliError> {
        let console = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(CliError::io("reserving installer console port"))?;
        let qmp = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(CliError::io("reserving installer QMP port"))?;
        Ok(Self {
            console: console
                .local_addr()
                .map_err(CliError::io("console port"))?
                .port(),
            qmp: qmp.local_addr().map_err(CliError::io("QMP port"))?.port(),
        })
    }
}

impl Plan {
    fn console(&self) -> PathBuf {
        #[cfg(windows)]
        return PathBuf::from(format!("tcp://127.0.0.1:{}", self.ports.console));
        #[cfg(not(windows))]
        self.run.join("console.sock")
    }

    fn target(&self) -> PathBuf {
        self.external_target
            .clone()
            .unwrap_or_else(|| self.cycle_dir.join("target.qcow2"))
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
        let env = vec![
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
        ];
        #[cfg(windows)]
        let env = {
            let mut env = env;
            env.push(("TILO_CONSOLE_PORT".into(), self.ports.console.to_string()));
            env.push(("TILO_QMP_PORT".into(), self.ports.qmp.to_string()));
            if let Some(target) = &self.external_target {
                env.push((
                    "TILO_INSTALL_TARGET_DISK".into(),
                    target.display().to_string(),
                ));
            }
            env
        };
        env
    }

    fn vm(&self, args: &[&str]) -> CommandLine {
        #[cfg(windows)]
        let mut line = CommandLine::new("pwsh", &["-NoProfile", "-File", "os/dev/vm-windows.ps1"]);
        #[cfg(not(windows))]
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
        let mut commands = vec![
            self.vm(&["init-cycle"]).display(),
            self.vm(&["boot-installer", self.graphics.flag(), self.tpm_flag()])
                .display(),
            format!(
                "drive the installer through its actions on {}",
                self.console().display()
            ),
        ];
        if !self.skip_target_boot {
            commands.push(
                self.vm(&["boot-target", self.graphics.flag(), self.tpm_flag()])
                    .display(),
            );
            commands.push(format!(
                "verify the greeter on {}",
                self.console().display()
            ));
        }
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
            "external_target": self.external_target.is_some(),
            "skip_target_boot": self.skip_target_boot,
            "install_timeout_seconds": self.install_timeout.as_secs(),
            "keep": self.keep,
            "commands": commands,
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

#[cfg(windows)]
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct VmInstallProof {
    schema: String,
    architecture: String,
    booted_image_digest: String,
    installed_image_digest: String,
    composefs_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    booted_composefs_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_composefs_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_update_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_target: Option<VmSelectedTarget>,
    target_uki_sha256: String,
    disk_guid: String,
    partuuids: VmPartuuids,
}

#[cfg(windows)]
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct VmPartuuids {
    efi: String,
    boot: String,
    root: String,
}

#[cfg(windows)]
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct VmSelectedTarget {
    schema: String,
    target_ref: String,
    target_digest: String,
    architecture: String,
    hardware_profile: String,
    update_origin: String,
    provenance: VmSelectedProvenance,
    verifier_key_sha256: String,
}

#[cfg(windows)]
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct VmSelectedProvenance {
    source_revision: String,
    firmware_manifest_sha256: String,
    invocation_id: String,
}

#[cfg(windows)]
fn lowercase_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(windows)]
fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

#[cfg(windows)]
fn parse_install_proof(bytes: &[u8]) -> Result<VmInstallProof, String> {
    let line = std::str::from_utf8(bytes).map_err(|_| "VM install proof is not UTF-8")?;
    let (version, rest) = if let Some(rest) = line.strip_prefix("TILO_VM_INSTALL_PROOF_V2=") {
        (2, rest)
    } else if let Some(rest) = line.strip_prefix("TILO_VM_INSTALL_PROOF_V1=") {
        (1, rest)
    } else {
        return Err("VM install proof has an unknown prefix".into());
    };
    let payload = rest
        .strip_suffix('\n')
        .ok_or("VM install proof needs exactly one terminated proof line")?;
    if payload.contains('\n') || payload.contains('\r') {
        return Err("VM install proof contains another line or carriage return".into());
    }
    let proof: VmInstallProof = serde_json::from_str(payload)
        .map_err(|error| format!("invalid VM install proof: {error}"))?;
    if proof.architecture != "arm64"
        || !proof
            .booted_image_digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| lowercase_hex(hex, 64))
        || !proof
            .installed_image_digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| lowercase_hex(hex, 64))
        || !lowercase_hex(&proof.composefs_digest, 128)
        || !lowercase_hex(&proof.target_uki_sha256, 64)
        || !canonical_uuid(&proof.disk_guid)
        || !canonical_uuid(&proof.partuuids.efi)
        || !canonical_uuid(&proof.partuuids.boot)
        || !canonical_uuid(&proof.partuuids.root)
        || proof.partuuids.efi == proof.partuuids.boot
        || proof.partuuids.efi == proof.partuuids.root
        || proof.partuuids.boot == proof.partuuids.root
    {
        return Err("VM install proof has invalid or inconsistent identities".into());
    }
    if version == 1 {
        if proof.schema != "tilo.vm-install-image-proof.v1"
            || proof.booted_image_digest != proof.installed_image_digest
            || proof.booted_composefs_digest.is_some()
            || proof.installed_composefs_digest.is_some()
            || proof.installed_update_origin.is_some()
            || proof.selected_target.is_some()
        {
            return Err("VM v1 install proof has inconsistent image identities".into());
        }
    } else {
        let selected = proof
            .selected_target
            .as_ref()
            .ok_or("VM v2 install proof has no selected target")?;
        let booted_composefs = proof
            .booted_composefs_digest
            .as_deref()
            .ok_or("VM v2 install proof has no booted composefs")?;
        let installed_composefs = proof
            .installed_composefs_digest
            .as_deref()
            .ok_or("VM v2 install proof has no installed composefs")?;
        let update_origin = proof
            .installed_update_origin
            .as_deref()
            .ok_or("VM v2 install proof has no installed update origin")?;
        let source = &selected.provenance.source_revision;
        if proof.schema != "tilo.vm-install-image-proof.v2"
            || !lowercase_hex(booted_composefs, 128)
            || !lowercase_hex(installed_composefs, 128)
            || proof.composefs_digest != installed_composefs
            || selected.schema != "tilo.installer-offline-target.v1"
            || selected.target_ref != format!("code.podesta.ai/qos/tilo@{}", selected.target_digest)
            || selected.target_digest != proof.installed_image_digest
            || selected.architecture != "aarch64"
            || selected.hardware_profile != "lenovo-83ed"
            || !lowercase_hex(source, 40)
            || !lowercase_hex(&selected.provenance.firmware_manifest_sha256, 64)
            || selected.provenance.invocation_id.is_empty()
            || !selected
                .provenance
                .invocation_id
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            || !lowercase_hex(&selected.verifier_key_sha256, 64)
            || selected.update_origin != format!("code.podesta.ai/qos/tilo:lenovo-83ed-{source}")
            || update_origin
                != format!(
                    "ostree-unverified-image:docker://{}",
                    selected.update_origin
                )
        {
            return Err("VM v2 install proof has inconsistent signed target identities".into());
        }
    }
    Ok(proof)
}

#[cfg(windows)]
fn check_install_proof(
    proof: &VmInstallProof,
    source_check: &Value,
    installer_composefs: Option<&str>,
) -> Result<Value, String> {
    if source_check.get("disk_guid").and_then(Value::as_str) != Some(&proof.disk_guid)
        || source_check
            .pointer("/final_partuuids/efi")
            .and_then(Value::as_str)
            != Some(&proof.partuuids.efi)
        || source_check
            .pointer("/final_partuuids/boot")
            .and_then(Value::as_str)
            != Some(&proof.partuuids.boot)
        || source_check
            .pointer("/final_partuuids/root")
            .and_then(Value::as_str)
            != Some(&proof.partuuids.root)
        || installer_composefs
            != Some(
                proof
                    .booted_composefs_digest
                    .as_deref()
                    .unwrap_or(&proof.composefs_digest),
            )
    {
        return Err(
            "guest install proof differs from the verified source GPT or installer composefs"
                .into(),
        );
    }
    Ok(json!({
        "schema": if proof.schema == "tilo.vm-install-image-proof.v2" {
            "tilo.windows-vm-install-proof-check.v2"
        } else {
            "tilo.windows-vm-install-proof-check.v1"
        },
        "guest_proof": proof,
        "source_check": source_check,
        "installer_composefs": installer_composefs,
    }))
}

#[cfg(windows)]
fn verify_external_target(
    plan: &Plan,
    expected_final_partitions: u64,
    artifact_name: &str,
) -> Result<Option<Value>, CliError> {
    let Some(target) = &plan.external_target else {
        return Ok(None);
    };
    let mut manifest_name = OsString::from(target.as_os_str());
    manifest_name.push(".manifest.json");
    let manifest = PathBuf::from(manifest_name);
    let script = plan.root.join("os/dev/prepare-windows-direct-source.py");
    let output = Command::new("python3")
        .arg(&script)
        .args(["verify-template", "--manifest"])
        .arg(&manifest)
        .arg("--source-raw")
        .arg(target)
        .current_dir(&plan.root)
        .output()
        .map_err(CliError::io("verifying the Windows direct-source template"))?;
    if !output.status.success() {
        return Err(failed(
            "verify-alongside",
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            plan,
        ));
    }
    let proof: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| failed("verify-alongside", error.to_string(), plan))?;
    if proof.get("preserved_gpt_verified").and_then(Value::as_bool) != Some(true)
        || proof
            .get("preserved_edge_samples_verified")
            .and_then(Value::as_bool)
            != Some(true)
        || proof.get("final_partition_count").and_then(Value::as_u64)
            != Some(expected_final_partitions)
    {
        return Err(failed(
            "verify-alongside",
            format!("unexpected Windows direct-source proof: {proof}"),
            plan,
        ));
    }
    write_json(&plan.out, artifact_name, &proof)?;
    Ok(Some(proof))
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
        if cfg!(windows) && args.layout != Layout::Erase && args.target_disk.is_none() {
            return Err(CliError::Unsupported {
                what: "alongside installer fixture on a Windows host".into(),
                alternative: "supply a reviewed sparse raw target with --target-disk; alongside fixture creation requires Linux sfdisk and mkfs tools".into(),
            });
        }
        if args.target_disk.is_some() && (!cfg!(windows) || args.layout != Layout::GuidedAlongside)
        {
            return Err(CliError::Unsupported {
                what: "external install target".into(),
                alternative: "use --target-disk only for a Windows WHPX guided-alongside cycle"
                    .into(),
            });
        }
        if args.skip_target_boot
            && (!cfg!(windows)
                || args.target_disk.is_none()
                || args.layout != Layout::GuidedAlongside)
        {
            return Err(CliError::Unsupported {
                what: "skip target boot".into(),
                alternative: "use --skip-target-boot only with a reviewed Windows guided-alongside --target-disk".into(),
            });
        }
        if cfg!(windows) && args.encrypt == Unlock::TpmPin {
            return Err(CliError::Unsupported {
                what: "TPM PIN installer scenario on a Windows host".into(),
                alternative:
                    "use --encrypt passphrase until the WHPX adapter has a persistent software TPM"
                        .into(),
            });
        }
        if cfg!(windows) && args.graphics == crate::scenario::Graphics::Gl {
            return Err(CliError::Unsupported {
                what: "GL installer scenario on a Windows host".into(),
                alternative:
                    "use --graphics software; Windows QEMU has no configured virgl headless backend"
                        .into(),
            });
        }
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
        let external_target = if let Some(target) = &args.target_disk {
            let metadata = std::fs::symlink_metadata(target)
                .map_err(CliError::io("inspecting external install target"))?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() < 40 * 1024 * 1024 * 1024
            {
                return Err(CliError::Unsupported {
                    what: "external install target".into(),
                    alternative: "use a regular, non-link sparse raw file of at least 40 GiB prepared by the Windows direct-source helper".into(),
                });
            }
            let target = target
                .canonicalize()
                .map_err(CliError::io("resolving external install target"))?;
            let medium_path = medium
                .canonicalize()
                .map_err(CliError::io("resolving installer medium"))?;
            if target == medium_path
                || target.extension().and_then(|extension| extension.to_str()) != Some("raw")
            {
                return Err(CliError::Unsupported {
                    what: "external install target".into(),
                    alternative: "use a distinct .raw target file, never the installer medium"
                        .into(),
                });
            }
            Some(target)
        } else {
            None
        };
        let cycle = format!(
            "scenario-install-{}-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
            std::process::id()
        );
        let out = args
            .out
            .clone()
            .unwrap_or_else(|| root.join("target/tilo-vm-scenarios").join(&cycle));
        let storage = out.join("storage");
        // Exactly the runtime directory vm.sh derives for a named cycle;
        // init-cycle refuses to clear any other.
        #[cfg(not(windows))]
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|path| !path.is_empty())
            .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        #[cfg(windows)]
        let runtime = out.clone();
        let plan = Plan {
            root,
            medium,
            external_target,
            skip_target_boot: args.skip_target_boot,
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
            #[cfg(windows)]
            ports: WindowsPorts::reserve()?,
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
        #[cfg(windows)]
        match std::fs::remove_file(plan.out.join("vm-install-proof-checked.json")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CliError::io("removing stale install proof")(error)),
        }
        if let Some(target) = &plan.external_target {
            let artifacts = plan
                .out
                .canonicalize()
                .map_err(CliError::io("resolving scenario artifacts"))?;
            if target.starts_with(artifacts) {
                return Err(CliError::Unsupported {
                    what: "external install target inside scenario artifacts".into(),
                    alternative: "place the reviewed raw target outside --out so scenario cleanup cannot remove it".into(),
                });
            }
        }
        write_json(&plan.out, "scenario.json", &plan.describe())?;
        self.receipt.set("scenario", plan.describe());
        let secret = plan.unlock.scenario_secret();
        let result = self.run_install_scenario(&plan, &secret);
        // Whatever happened, the VM stops and the evidence is scrubbed.
        let stop = self.runner.output(&plan.vm(&["stop"]));
        #[cfg(windows)]
        let stopped_cleanly = stop.as_ref().is_ok_and(|outcome| outcome.success);
        #[cfg(windows)]
        let result = result.and_then(|report| {
            if plan.external_target.is_some() {
                let stopped = stop.map_err(|error| failed("stop", error.to_string(), &plan))?;
                if !stopped.success {
                    return Err(failed("stop", stopped.stderr.trim(), &plan));
                }
            }
            let artifact = if plan.skip_target_boot {
                "external-target-after-install-stop.json"
            } else {
                "external-target-after-boot.json"
            };
            verify_external_target(&plan, 3, artifact)?;
            Ok(report)
        });
        #[cfg(not(windows))]
        let _ = stop;
        #[cfg(windows)]
        let preserve_failed_external = plan.external_target.is_some() && !stopped_cleanly;
        #[cfg(not(windows))]
        let preserve_failed_external = false;
        if !plan.keep && !preserve_failed_external {
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
        let checked_install_proof = report
            .as_object_mut()
            .and_then(|map| map.remove("_checked_install_proof"));
        if let Value::Object(map) = &mut report {
            map.insert("plan".into(), plan.describe());
            map.insert("phases".into(), json!(self.receipt.summary()));
        }
        write_json(&plan.out, "result.json", &report)?;
        #[cfg(windows)]
        if let Some(mut checked) = checked_install_proof {
            checked["first_boot_verified"] = json!(!plan.skip_target_boot);
            write_json(&plan.out, "vm-install-proof-checked.json", &checked)?;
        }
        #[cfg(not(windows))]
        let _ = checked_install_proof;
        let completion = if plan.skip_target_boot {
            "target boot deferred; guest image proof and source GPT checked"
        } else {
            "first boot reached the greeter on the installed image"
        };
        let text = format!(
            "install scenario passed: {} install onto {} ({}), {} {}\nphases: {}\nartifacts: {}",
            plan.layout.name(),
            report.get("target").and_then(Value::as_str).unwrap_or("?"),
            plan.unlock.name(),
            completion,
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
        #[cfg(windows)]
        verify_external_target(plan, 0, "external-target-before.json")?;
        let mut preserved: Vec<Preserved> = Vec::new();
        if plan.layout != Layout::Erase && plan.external_target.is_none() {
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
        #[cfg(windows)]
        let source_check = verify_external_target(plan, 3, "external-target-after-install.json")?;
        #[cfg(windows)]
        let checked_install_proof = if let Some(source_check) = &source_check {
            let path = plan.run.join("install-proof.log");
            let size = std::fs::metadata(&path)
                .map_err(|error| failed("verify-install-proof", error.to_string(), plan))?
                .len();
            if size == 0 || size > 8192 {
                return Err(failed(
                    "verify-install-proof",
                    "VM install proof is empty or oversized",
                    plan,
                ));
            }
            let bytes = std::fs::read(&path)
                .map_err(|error| failed("verify-install-proof", error.to_string(), plan))?;
            let proof = parse_install_proof(&bytes)
                .map_err(|error| failed("verify-install-proof", error, plan))?;
            let checked = check_install_proof(&proof, source_check, installed.composefs.as_deref())
                .map_err(|error| failed("verify-install-proof", error, plan))?;
            Some(checked)
        } else {
            None
        };

        #[cfg(windows)]
        if plan.skip_target_boot {
            let checked = checked_install_proof.ok_or_else(|| {
                failed(
                    "verify-install-proof",
                    "board install has no checked guest proof",
                    plan,
                )
            })?;
            if checked.get("schema").and_then(Value::as_str)
                != Some("tilo.windows-vm-install-proof-check.v2")
            {
                return Err(failed(
                    "verify-install-proof",
                    "board install requires backend-derived signed target proof v2",
                    plan,
                ));
            }
            return Ok(json!({
                "scenario": "install", "layout": plan.layout.name(),
                "encrypt": plan.unlock.name(), "target": installed.target,
                "installer_steps": installed.steps,
                "installer_image": installed.composefs,
                "installed_image": checked["guest_proof"]["installed_composefs_digest"],
                "first_boot_verified": false, "preserved": preserved,
                "artifacts": plan.out, "_checked_install_proof": checked,
            }));
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
        #[cfg(windows)]
        let expected_composefs = checked_install_proof
            .as_ref()
            .and_then(|proof| {
                proof
                    .pointer("/guest_proof/installed_composefs_digest")
                    .and_then(Value::as_str)
            })
            .or(installed.composefs.as_deref());
        #[cfg(not(windows))]
        let expected_composefs = installed.composefs.as_deref();
        let (Some(live), Some(booted_image)) = (expected_composefs, installed_image.as_deref())
        else {
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
        let mut report = json!({
            "scenario": "install",
            "layout": plan.layout.name(),
            "encrypt": plan.unlock.name(),
            "target": installed.target,
            "installer_steps": installed.steps,
            "installer_image": live,
            "installed_image": booted_image,
            "first_boot_verified": true,
            "greeter": greeter,
            "preserved": preserved,
            "artifacts": plan.out,
        });
        #[cfg(windows)]
        if let Some(checked) = checked_install_proof {
            report["_checked_install_proof"] = checked;
        }
        Ok(report)
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
    #[cfg(windows)]
    #[test]
    fn vm_install_proof_requires_one_valid_guest_line_and_matching_source() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let efi = "11111111-1111-4111-8111-111111111111";
        let boot = "22222222-2222-4222-8222-222222222222";
        let root = "33333333-3333-4333-8333-333333333333";
        let disk = "44444444-4444-4444-8444-444444444444";
        let payload = json!({
            "schema": "tilo.vm-install-image-proof.v1", "architecture": "arm64",
            "booted_image_digest": digest, "installed_image_digest": digest,
            "composefs_digest": "b".repeat(128), "target_uki_sha256": "c".repeat(64),
            "disk_guid": disk, "partuuids": {"efi": efi, "boot": boot, "root": root},
        });
        let line = format!("TILO_VM_INSTALL_PROOF_V1={payload}\n");
        let proof = super::parse_install_proof(line.as_bytes()).expect("valid guest proof");
        let source = json!({"disk_guid": disk,
            "final_partuuids": {"efi": efi, "boot": boot, "root": root}});
        let checked = super::check_install_proof(&proof, &source, Some(&"b".repeat(128)))
            .expect("matching source GPT");
        assert_eq!(checked["guest_proof"]["disk_guid"], disk);
        assert!(super::parse_install_proof(format!("{line}{line}").as_bytes()).is_err());
        assert!(super::parse_install_proof(line.trim_end().as_bytes()).is_err());
        let mut changed = source;
        changed["final_partuuids"]["root"] = json!(efi);
        assert!(super::check_install_proof(&proof, &changed, Some(&"b".repeat(128))).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn vm_v2_proof_binds_distinct_medium_and_signed_target() {
        let revision = "a".repeat(40);
        let target = format!("sha256:{}", "b".repeat(64));
        let origin = format!("code.podesta.ai/qos/tilo:lenovo-83ed-{revision}");
        let ids = json!({
            "efi": "11111111-1111-4111-8111-111111111111",
            "boot": "22222222-2222-4222-8222-222222222222",
            "root": "33333333-3333-4333-8333-333333333333",
        });
        let disk = "44444444-4444-4444-8444-444444444444";
        let mut payload = json!({
            "schema": "tilo.vm-install-image-proof.v2", "architecture": "arm64",
            "booted_image_digest": format!("sha256:{}", "e".repeat(64)),
            "installed_image_digest": target,
            "booted_composefs_digest": "f".repeat(128),
            "installed_composefs_digest": "c".repeat(128),
            "composefs_digest": "c".repeat(128),
            "installed_update_origin": format!("ostree-unverified-image:docker://{origin}"),
            "selected_target": {
                "schema": "tilo.installer-offline-target.v1",
                "target_ref": format!("code.podesta.ai/qos/tilo@{target}"),
                "target_digest": target, "architecture": "aarch64",
                "hardware_profile": "lenovo-83ed", "update_origin": origin,
                "provenance": {"source_revision": revision,
                    "firmware_manifest_sha256": "d".repeat(64), "invocation_id": "123"},
                "verifier_key_sha256": "e".repeat(64),
            },
            "target_uki_sha256": "d".repeat(64), "disk_guid": disk,
            "partuuids": ids,
        });
        let line = format!("TILO_VM_INSTALL_PROOF_V2={payload}\n");
        let proof = super::parse_install_proof(line.as_bytes()).expect("valid v2 guest proof");
        let source = json!({"disk_guid": disk, "final_partuuids": ids});
        let checked = super::check_install_proof(&proof, &source, Some(&"f".repeat(128)))
            .expect("matching installer composefs and source GPT");
        assert_eq!(checked["schema"], "tilo.windows-vm-install-proof-check.v2");
        assert_eq!(checked["guest_proof"]["installed_image_digest"], target);
        assert!(super::check_install_proof(&proof, &source, Some(&"c".repeat(128))).is_err());
        payload["selected_target"]["target_digest"] = json!(format!("sha256:{}", "a".repeat(64)));
        assert!(
            super::parse_install_proof(format!("TILO_VM_INSTALL_PROOF_V2={payload}\n").as_bytes())
                .is_err()
        );
    }
    use super::*;

    fn plan(layout: Layout) -> Plan {
        Plan {
            root: PathBuf::from("/w"),
            medium: PathBuf::from("/w/os/output/installer/qcow2/disk.qcow2"),
            external_target: None,
            skip_target_boot: false,
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
            #[cfg(windows)]
            ports: WindowsPorts {
                console: 30001,
                qmp: 30002,
            },
        }
    }

    #[cfg(unix)]
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

    #[cfg(windows)]
    #[test]
    fn windows_plan_uses_whpx_transport_and_tcp_console() {
        let plan = plan(Layout::Erase);
        let boot = plan.vm(&["boot-installer", "--2d", "--no-tpm"]);
        assert_eq!(boot.program, "pwsh");
        assert_eq!(
            boot.args,
            [
                "-NoProfile",
                "-File",
                "os/dev/vm-windows.ps1",
                "boot-installer",
                "--2d",
                "--no-tpm"
            ]
        );
        assert_eq!(plan.console(), PathBuf::from("tcp://127.0.0.1:30001"));
        assert!(
            boot.env
                .iter()
                .any(|(key, value)| key == "TILO_CONSOLE_PORT" && value == "30001")
        );
        assert!(
            boot.env
                .iter()
                .any(|(key, value)| key == "TILO_QMP_PORT" && value == "30002")
        );
        let described = plan.describe();
        let commands = described["commands"].to_string();
        assert!(commands.contains("boot-installer"));
        assert!(commands.contains("boot-target"));
        assert!(commands.contains("tcp://127.0.0.1:30001"));
    }

    #[cfg(windows)]
    #[test]
    fn external_raw_target_is_retained_and_passed_to_whpx_transport() {
        let mut plan = plan(Layout::GuidedAlongside);
        let target = PathBuf::from("C:/tilo-test/source.raw");
        plan.external_target = Some(target.clone());
        assert_eq!(plan.target(), target);
        let boot = plan.vm(&["boot-installer"]);
        assert!(boot.env.iter().any(|(key, value)| {
            key == "TILO_INSTALL_TARGET_DISK" && value == &target.display().to_string()
        }));
        assert_eq!(plan.describe()["external_target"], true);
        plan.skip_target_boot = true;
        let described = plan.describe();
        assert_eq!(described["skip_target_boot"], true);
        assert!(!described["commands"].to_string().contains("boot-target"));
    }
}
