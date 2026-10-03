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
use crate::scenario::{Credentials, Layout, Unlock, scrub, scrub_with};

/// Where `os/dev/build-custom-installer.sh` writes the medium.
pub const DEFAULT_MEDIUM: &str = "os/output/installer/qcow2/disk.qcow2";
/// How long a booting installer gets to answer its console.
const INSTALLER_BOOT: Duration = Duration::from_secs(20 * 60);
/// How long the installer gets to power the VM off after Finish.
const POWER_OFF: Duration = Duration::from_secs(180);
/// How long a cold-booted target gets to reach the greeter.
const TARGET_BOOT: Duration = Duration::from_secs(15 * 60);
/// Bound retries even when the encrypted-root prompt remains visible.
const UNLOCK_ATTEMPTS: u32 = 2;

enum FirstBootSurface {
    Greeter(Value),
    IdentitySetup { screenshot: PathBuf, text: String },
}

fn is_identity_setup(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("tilo identity setup")
        && (text.contains("create your local")
            || text.contains("connect an existing identity")
            || text.contains("back up your tilo identity")
            || text.contains("your identity is ready"))
}

fn is_disk_unlock_prompt(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("unlock tilo") && text.contains("enter your disk passphrase or recovery key")
}

/// Everything one run needs to know, settled before anything boots.
struct Plan {
    root: PathBuf,
    medium: PathBuf,
    external_target: Option<PathBuf>,
    skip_target_boot: bool,
    layout: Layout,
    filesystem: Option<String>,
    resume: bool,
    unlock: Unlock,
    username: String,
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
        let mut commands = if self.resume {
            vec!["check retained install record, target layout and guest image proof".to_owned()]
        } else {
            vec![
                self.vm(&["init-cycle"]).display(),
                self.vm(&["boot-installer", self.graphics.flag(), self.tpm_flag()])
                    .display(),
                format!(
                    "drive the installer through its actions on {}",
                    self.console().display()
                ),
            ]
        };
        if !self.skip_target_boot {
            commands.push(
                self.vm(&["boot-target", self.graphics.flag(), self.tpm_flag()])
                    .display(),
            );
            commands.push(format!(
                "verify sign-in or first-use identity setup on {}",
                self.console().display()
            ));
        }
        json!({
            "scenario": "install",
            "medium": self.medium,
            "layout": self.layout.name(),
            "filesystem": self.filesystem,
            "encrypt": self.unlock.name(),
            "username": self.username,
            "graphics": self.graphics,
            "cycle": self.cycle,
            "artifacts": self.out,
            "console": self.console(),
            "target": self.target(),
            "external_target": self.external_target.is_some(),
            "skip_target_boot": self.skip_target_boot,
            "install_timeout_seconds": self.install_timeout.as_secs(),
            "keep": self.keep,
            "resume": self.resume,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    copied_manifest_digest: Option<String>,
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
    verifier_key_sha256: Option<String>,
}

#[cfg(windows)]
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct VmSelectedProvenance {
    source_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    firmware_manifest_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    firmware_candidate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<u64>,
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
    let (version, rest) = if let Some(rest) = line.strip_prefix("TILO_VM_INSTALL_PROOF_V4=") {
        (4, rest)
    } else if let Some(rest) = line.strip_prefix("TILO_VM_INSTALL_PROOF_V3=") {
        (3, rest)
    } else if let Some(rest) = line.strip_prefix("TILO_VM_INSTALL_PROOF_V2=") {
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
            || proof
                .booted_composefs_digest
                .as_ref()
                .is_some_and(|value| value != &proof.composefs_digest)
            || proof
                .installed_composefs_digest
                .as_ref()
                .is_some_and(|value| value != &proof.composefs_digest)
            || proof.selected_target.is_some()
            || proof.copied_manifest_digest.is_some()
        {
            return Err("VM v1 install proof has inconsistent image identities".into());
        }
    } else if version == 3 {
        if proof.schema != "tilo.vm-install-image-proof.v3"
            || proof.booted_image_digest == proof.installed_image_digest
            || proof.copied_manifest_digest.as_deref()
                != Some(proof.installed_image_digest.as_str())
            || proof.booted_composefs_digest.as_deref() != Some(proof.composefs_digest.as_str())
            || proof.installed_composefs_digest.as_deref() != Some(proof.composefs_digest.as_str())
            || proof.selected_target.is_some()
        {
            return Err("VM v3 development copy proof has inconsistent identities".into());
        }
    } else if version == 4 {
        let selected = proof
            .selected_target
            .as_ref()
            .ok_or("VM development target proof has no selected target")?;
        let source = &selected.provenance.source_revision;
        let tag = selected
            .update_origin
            .strip_prefix("registry.tilo.so/tilo/tilo-vm:");
        if proof.schema != "tilo.vm-install-image-proof.v4"
            || selected.schema != "tilo.installer-dev-target.v1"
            || selected.target_ref
                != format!("registry.tilo.so/tilo/tilo-vm@{}", selected.target_digest)
            || selected.target_digest != proof.installed_image_digest
            || selected.architecture != "aarch64"
            || !matches!(
                selected.hardware_profile.as_str(),
                "generic" | "lenovo-83ed"
            )
            || selected.verifier_key_sha256.is_some()
            || !lowercase_hex(source, 40)
            || selected.provenance.invocation_id.is_empty()
            || !selected
                .provenance
                .invocation_id
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            || !tag.is_some_and(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                    && value
                        .as_bytes()
                        .first()
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            })
            || selected.provenance.firmware_candidate.is_some()
            || selected.provenance.firmware_manifest_sha256.is_some()
            || selected.provenance.generation.is_some()
            || proof.installed_composefs_digest.as_deref() != Some(proof.composefs_digest.as_str())
            || !proof
                .booted_composefs_digest
                .as_deref()
                .is_some_and(|value| lowercase_hex(value, 128))
            || proof.copied_manifest_digest.is_some()
            || proof.installed_update_origin.as_deref()
                != Some(
                    format!(
                        "ostree-unverified-image:docker://{}",
                        selected.update_origin
                    )
                    .as_str(),
                )
        {
            return Err("VM development target proof has inconsistent image identities".into());
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
        let profile_matches = match selected.hardware_profile.as_str() {
            "lenovo-83ed" => {
                selected
                    .provenance
                    .firmware_manifest_sha256
                    .as_deref()
                    .is_some_and(|value| lowercase_hex(value, 64))
                    && selected
                        .provenance
                        .firmware_candidate
                        .as_deref()
                        .is_some_and(|value| {
                            value
                                .strip_prefix("code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:")
                                .is_some_and(|hex| lowercase_hex(hex, 64))
                        })
                    && selected
                        .provenance
                        .generation
                        .is_none_or(|generation| generation == 1)
                    && selected.update_origin
                        == format!("code.tilo.so/tilo/tilo:lenovo-83ed-{source}")
            }
            "generic" => {
                selected.provenance.firmware_candidate.is_none()
                    && selected.provenance.firmware_manifest_sha256.is_none()
                    && selected
                        .provenance
                        .generation
                        .is_some_and(|generation| generation > 0)
                    && selected.update_origin
                        == format!(
                            "registry.tilo.so/tilo/tilo:gen{}",
                            selected.provenance.generation.unwrap_or_default()
                        )
            }
            _ => false,
        };
        // Board images carry private firmware and stay on the forge's private
        // registry; every other image is public.
        let repository = if selected.hardware_profile == "lenovo-83ed" {
            "code.tilo.so/tilo/tilo"
        } else {
            "registry.tilo.so/tilo/tilo"
        };
        if proof.schema != "tilo.vm-install-image-proof.v2"
            || proof.copied_manifest_digest.is_some()
            || !lowercase_hex(booted_composefs, 128)
            || !lowercase_hex(installed_composefs, 128)
            || proof.composefs_digest != installed_composefs
            || selected.schema != "tilo.installer-offline-target.v1"
            || selected.target_ref != format!("{repository}@{}", selected.target_digest)
            || selected.target_digest != proof.installed_image_digest
            || selected.architecture != "aarch64"
            || !profile_matches
            || !lowercase_hex(source, 40)
            || selected.provenance.invocation_id.is_empty()
            || !selected
                .provenance
                .invocation_id
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            || !selected
                .verifier_key_sha256
                .as_deref()
                .is_some_and(|value| lowercase_hex(value, 64))
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
        "schema": if proof.schema == "tilo.vm-install-image-proof.v4" {
            "tilo.windows-vm-install-proof-check.v4"
        } else if proof.schema == "tilo.vm-install-image-proof.v2" {
            "tilo.windows-vm-install-proof-check.v2"
        } else if proof.schema == "tilo.vm-install-image-proof.v3" {
            "tilo.windows-vm-install-proof-check.v3"
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
        let username = args.username.as_deref().unwrap_or("scenario");
        if !valid_username(username) {
            return Err(CliError::Unsupported {
                what: "scenario account username".into(),
                alternative:
                    "use 1–32 lowercase letters, digits, _ or -, starting with a letter or _".into(),
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
        let medium_metadata = std::fs::symlink_metadata(&medium);
        if !medium_metadata
            .as_ref()
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        {
            return Err(CliError::ScenarioMedium {
                path: medium.display().to_string(),
            });
        }
        let medium = medium
            .canonicalize()
            .map_err(CliError::io("resolving installer medium"))?;
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
        let mut cycle = format!(
            "scenario-install-{}-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
            std::process::id()
        );
        let out = args
            .out
            .clone()
            .unwrap_or_else(|| root.join("target/tilo-vm-scenarios").join(&cycle));
        if args.resume {
            if !cfg!(windows) {
                return Err(CliError::Unsupported {
                    what: "install scenario resume".into(),
                    alternative: "resume currently requires a retained Windows raw target".into(),
                });
            }
            let saved: Value = serde_json::from_slice(
                &std::fs::read(out.join("scenario.json"))
                    .map_err(CliError::io("reading retained scenario"))?,
            )
            .map_err(|error| CliError::Unsupported {
                what: "retained scenario".into(),
                alternative: error.to_string(),
            })?;
            let old_cycle = saved.get("cycle").and_then(Value::as_str).unwrap_or("");
            if !old_cycle
                .strip_prefix("scenario-install-")
                .is_some_and(|tail| {
                    !tail.is_empty()
                        && tail
                            .chars()
                            .all(|c| c.is_ascii_digit() || matches!(c, 'T' | 'Z' | '-'))
                })
            {
                return Err(CliError::Unsupported {
                    what: "retained scenario cycle".into(),
                    alternative: "use the original scenario output".into(),
                });
            }
            old_cycle.clone_into(&mut cycle);
        }
        let storage = out.join("storage");
        // Exactly the runtime directory vm.sh derives for a named cycle;
        // init-cycle refuses to clear any other.
        #[cfg(not(windows))]
        let runtime = xdg_paths::xdg::runtime_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        #[cfg(windows)]
        let runtime = out.clone();
        let plan = Plan {
            root,
            medium,
            external_target,
            skip_target_boot: args.skip_target_boot,
            layout: args.layout,
            filesystem: args.filesystem.clone(),
            resume: args.resume,
            unlock: args.encrypt,
            username: username.to_owned(),
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
        if plan.resume {
            let saved: Value = serde_json::from_slice(
                &std::fs::read(plan.out.join("scenario.json"))
                    .map_err(CliError::io("reading retained scenario"))?,
            )
            .map_err(|error| failed("resume", error.to_string(), &plan))?;
            let current = plan.describe();
            for key in [
                "medium",
                "target",
                "layout",
                "filesystem",
                "encrypt",
                "username",
                "graphics",
            ] {
                if saved.get(key) != current.get(key) {
                    return Err(failed(
                        "resume",
                        format!("retained scenario {key} differs; use its original inputs"),
                        &plan,
                    ));
                }
            }
        }
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
        let credentials = if args.credentials_stdin {
            crate::scenario::credentials_from_stdin().map_err(|message| CliError::Unsupported {
                what: "scenario credentials stdin".into(),
                alternative: message.into(),
            })?
        } else {
            Credentials::disposable(plan.unlock)
        };
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
        let result = self.run_install_scenario(&plan, &credentials);
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
        let scrubbed = scrub_scenario_logs(
            &plan.out,
            &plan.run,
            &[
                credentials.account.expose(),
                credentials.unlock_secret().expose(),
            ],
        );
        if scrubbed.is_err() && plan.run.is_dir() {
            discard_unsanitized_runtime(&plan, &mut self.runner)?;
        }
        let withheld = scrubbed.map_err(CliError::io("scrubbing scenario artifacts and logs"))?;
        if !plan.keep && !preserve_failed_external {
            let _ = std::fs::remove_dir_all(&plan.cycle_dir);
            let _ = std::fs::remove_dir_all(&plan.run);
        }
        let mut report = result?;
        if !withheld.is_empty() {
            return Err(failed(
                "artifacts",
                format!(
                    "{} artifact(s) contained a supplied secret or recovery material and were emptied: {}",
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
            checked["installer_medium_sha256"] = json!(crate::disk::file_sha256(&plan.medium)?);
            write_json(&plan.out, "vm-install-proof-checked.json", &checked)?;
        }
        #[cfg(not(windows))]
        let _ = checked_install_proof;
        let completion = if plan.skip_target_boot {
            "target boot deferred; guest image proof and source GPT checked"
        } else {
            "first boot reached Tilo's sign-in or identity setup surface"
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
        credentials: &Credentials,
    ) -> Result<Value, CliError> {
        say(&format!(
            "install scenario {} ({} layout, {} unlock); artifacts in {}",
            plan.cycle,
            plan.layout.name(),
            plan.unlock.name(),
            plan.out.display()
        ));
        let mut preserved: Vec<Preserved> = Vec::new();
        let installed = if plan.resume {
            let installed: drive::Installed = serde_json::from_slice(
                &std::fs::read(plan.out.join("install.json"))
                    .map_err(CliError::io("reading retained install result"))?,
            )
            .map_err(|error| failed("resume", error.to_string(), plan))?;
            if installed.steps.last().map(String::as_str) != Some("power-off")
                || !self.wait_stopped(plan, Duration::from_secs(2))
            {
                return Err(failed(
                    "resume",
                    "retained install is incomplete or its VM is running",
                    plan,
                ));
            }
            say("resuming completed install; checking target and guest proof before cold boot");
            installed
        } else {
            let started = Instant::now();
            let init = self.runner.output(&plan.vm(&["init-cycle"]))?;
            if !init.success {
                return Err(failed("prepare", init.stderr.trim(), plan));
            }
            #[cfg(windows)]
            verify_external_target(plan, 0, "external-target-before.json")?;
            if plan.layout != Layout::Erase && plan.external_target.is_none() {
                let scratch = fixture::scratch_dir(&plan.cycle_dir);
                std::fs::create_dir_all(&scratch)
                    .map_err(CliError::io("creating fixture scratch"))?;
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
            let first =
                wait_for_app(&mut console, "installer", INSTALLER_BOOT).ok_or_else(|| {
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
                filesystem: plan.filesystem.clone(),
                unlock: plan.unlock,
                username: plan.username.clone(),
                full_name: if plan.username == "scenario" {
                    "Scenario User".into()
                } else {
                    plan.username.clone()
                },
                hostname: "scenario-tilo".into(),
                account_secret: crate::scenario::Secret::new(credentials.account.expose()),
                disk_secret: credentials
                    .disk
                    .as_ref()
                    .map(|secret| crate::scenario::Secret::new(secret.expose())),
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
            let installed =
                installed.map_err(|error| failed("install", error.to_string(), plan))?;
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
            installed
        };
        let mut console = SocketConsole::new(&plan.console());

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
            if !matches!(
                checked.get("schema").and_then(Value::as_str),
                Some(
                    "tilo.windows-vm-install-proof-check.v2"
                        | "tilo.windows-vm-install-proof-check.v4"
                )
            ) {
                return Err(failed(
                    "verify-install-proof",
                    "board install requires backend-derived signed target proof v2 or explicit dev target proof v4",
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

        #[cfg(windows)]
        if plan.external_target.is_some() {
            let discarded = self.runner.output(&plan.vm(&["discard-installer"]))?;
            if !discarded.success {
                return Err(failed("discard-installer", discarded.stderr.trim(), plan));
            }
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
        console.timeout = Duration::from_secs(2);
        let first_boot =
            self.unlock_to_first_boot(plan, &mut console, credentials.unlock_secret())?;
        let serial = plan.run.join("serial.log");
        if serial.is_file() {
            let _ = std::fs::copy(&serial, plan.out.join("target-serial.log"));
        }
        self.receipt
            .record("first-boot", target_boot.elapsed().as_secs_f64());
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
        let Some(live) = expected_composefs else {
            return Err(failed(
                "first-boot",
                "the installer did not report the composefs image it wrote",
                plan,
            ));
        };
        let (booted_image, observation) = match first_boot {
            FirstBootSurface::Greeter(greeter) => {
                write_json(&plan.out, "greeter-state.json", &greeter)?;
                let booted_image = greeter
                    .pointer("/image/composefs")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        failed(
                            "first-boot",
                            "the greeter did not report its composefs image",
                            plan,
                        )
                    })?;
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
                (
                    booted_image.to_owned(),
                    json!({ "kind": "greeter", "state": greeter }),
                )
            }
            FirstBootSurface::IdentitySetup { screenshot, text } => {
                write_json(
                    &plan.out,
                    "identity-setup-observation.json",
                    &json!({
                        "screenshot": screenshot,
                        "ocr": text,
                        "image_identity_source": "installer proof; the cold-booted VM has only the installed target disk",
                    }),
                )?;
                (
                    live.to_owned(),
                    json!({ "kind": "identity-setup", "screenshot": screenshot,
                    "image_identity_observed_directly": false }),
                )
            }
        };
        let report = json!({
            "scenario": "install",
            "layout": plan.layout.name(),
            "encrypt": plan.unlock.name(),
            "target": installed.target,
            "installer_steps": installed.steps,
            "installer_image": live,
            "installed_image": booted_image,
            "first_boot_verified": true,
            "first_boot": observation,
            "preserved": preserved,
            "artifacts": plan.out,
        });
        #[cfg(windows)]
        let report = {
            let mut report = report;
            if let Some(checked) = checked_install_proof {
                report["_checked_install_proof"] = checked;
            }
            report
        };
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

    /// Recognize either sign-in or first-use identity setup. Type the disk
    /// secret only after observing the encrypted-root prompt on this poll.
    fn unlock_to_first_boot(
        &mut self,
        plan: &Plan,
        console: &mut SocketConsole,
        secret: &crate::scenario::Secret,
    ) -> Result<FirstBootSurface, CliError> {
        let deadline = after(TARGET_BOOT);
        let mut typed = 0_u32;
        let mut next_unlock = Instant::now();
        while Instant::now() < deadline {
            if let Ok(state) = console.state()
                && state.get("app").and_then(Value::as_str) == Some("greeter")
            {
                self.receipt.set("unlock_attempts", typed);
                return Ok(FirstBootSurface::Greeter(state));
            }
            let mut unlock_prompt = false;
            if let Ok(shot) = self
                .runner
                .output(&plan.vm(&["shot", "first-boot-observe"]))
                && shot.success
            {
                let source = PathBuf::from(shot.stdout.trim());
                if source.is_file()
                    && let Ok(ocr) = std::process::Command::new("tesseract")
                        .arg(&source)
                        .arg("stdout")
                        .output()
                    && ocr.status.success()
                {
                    let text = String::from_utf8_lossy(&ocr.stdout).into_owned();
                    unlock_prompt = is_disk_unlock_prompt(&text);
                    if is_identity_setup(&text) {
                        let screenshot = plan.out.join("first-boot-identity-setup.png");
                        std::fs::copy(&source, &screenshot)
                            .map_err(CliError::io("saving first-boot identity setup screenshot"))?;
                        self.receipt.set("unlock_attempts", typed);
                        return Ok(FirstBootSurface::IdentitySetup { screenshot, text });
                    }
                }
            }
            let running = self
                .runner
                .output(&plan.vm(&["status"]))
                .is_ok_and(|status| status.stdout.trim_start().starts_with("running"));
            if !running {
                return Err(failed(
                    "first-boot",
                    "the installed target stopped before reaching sign-in or identity setup",
                    plan,
                ));
            }
            if unlock_prompt && typed < UNLOCK_ATTEMPTS && Instant::now() >= next_unlock {
                piped(plan, &["type", "-"], secret.expose())?;
                let _ = self.runner.output(&plan.vm(&["keys", "ret"]));
                typed = typed.saturating_add(1);
                next_unlock = after(Duration::from_secs(15));
            }
            // Keep observing startup while the retry delay elapses: a fast
            // successful unlock should not incur a fixed fifteen-second wait.
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(failed(
            "first-boot",
            format!(
                "no sign-in or identity setup appeared within {}s after {typed} unlock attempt(s)",
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

fn valid_username(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z' | '_'))
        && name.len() <= 32
        && chars.all(|character| matches!(character, 'a'..='z' | '0'..='9' | '_' | '-'))
}

/// The VM runtime holds raw serial and UART streams, including material the
/// installer deliberately withholds from scenario artifacts. Scrub it before
/// retention or cleanup, even when the install failed.
fn scrub_scenario_logs(out: &Path, run: &Path, secrets: &[&str]) -> std::io::Result<Vec<PathBuf>> {
    scrub_scenario_logs_with(out, run, secrets, |path, secrets| {
        if path == out {
            // VM disk images live here and can be tens of GiB. They are never
            // text logs; reading them can exhaust host memory, and a false
            // positive must never truncate an installed target.
            let storage = out.join("storage");
            scrub_with(path, secrets, |directory| directory != storage)
        } else {
            scrub(path, secrets)
        }
    })
}

fn scrub_scenario_logs_with(
    out: &Path,
    run: &Path,
    secrets: &[&str],
    mut inspect: impl FnMut(&Path, &[&str]) -> std::io::Result<Vec<PathBuf>>,
) -> std::io::Result<Vec<PathBuf>> {
    // Both paths are attempted even if the first fails. The caller must stop
    // the VM before deleting a runtime directory after either failure: the
    // pid file must survive until the stop command has used it.
    let artifacts = inspect(out, secrets);
    let runtime = if run.is_dir() {
        inspect(run, secrets)
    } else {
        Ok(Vec::new())
    };
    let mut artifacts = artifacts?;
    artifacts.extend(runtime?);
    Ok(artifacts)
}

fn discard_unsanitized_runtime(plan: &Plan, runner: &mut dyn Runner) -> Result<(), CliError> {
    // A failed first stop may leave QEMU holding serial.log open. Never delete
    // qemu.pid before a second stop and a confirmed stopped status.
    let _ = runner.output(&plan.vm(&["stop"]));
    let stopped = runner
        .output(&plan.vm(&["status"]))
        .is_ok_and(|status| status.success && status.stdout.trim_start().starts_with("stopped"));
    if !stopped {
        return Err(CliError::Unsupported {
            what: "unsanitized VM runtime logs".into(),
            alternative: "VM is still running; stop it and remove its runtime logs before retaining artifacts".into(),
        });
    }
    std::fs::remove_dir_all(&plan.run).map_err(CliError::io("removing unsanitized VM runtime logs"))
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
    #[test]
    fn disk_secret_requires_the_visible_encrypted_root_prompt() {
        assert!(super::is_disk_unlock_prompt(
            "Unlock Tilo\n\nEnter your disk passphrase or recovery key"
        ));
        assert!(!super::is_disk_unlock_prompt("Tilo\nStarting services"));
        assert!(!super::is_disk_unlock_prompt(
            "Welcome back to Tilo\nbob\nEnter your passcode to open your desktop."
        ));
        assert!(!super::is_disk_unlock_prompt(
            "Tilo identity setup\nBack up your Tilo identity\nRecovery key"
        ));
        assert!(!super::is_disk_unlock_prompt("Unlock Tilo"));
    }

    #[test]
    fn first_boot_ocr_accepts_identity_setup_but_not_a_login_prompt() {
        assert!(super::is_identity_setup(
            "Tilo identity setup Step 2 of 5 - Restore\nConnect an existing identity"
        ));
        assert!(super::is_identity_setup(
            "Tilo identity setup\nCreate your local identity"
        ));
        assert!(!super::is_identity_setup(
            "Fedora Linux 44\nscenario-tilo login:"
        ));
    }

    /// A VM install proof requires one valid guest line and matching source; a v2 proof binds a distinct
    /// medium and signed target.
    #[cfg(windows)]
    #[test]
    fn vm_install_proofs_bind_the_guest_line_medium_and_signed_target() {
        // Phase: vm_install_proof_requires_one_valid_guest_line_and_matching_source.
        {
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
            let mut extended = payload;
            extended["booted_composefs_digest"] = json!("b".repeat(128));
            extended["installed_composefs_digest"] = json!("b".repeat(128));
            extended["installed_update_origin"] =
                json!("ostree-unverified-image:docker://tilo:gen1");
            assert!(
                super::parse_install_proof(
                    format!("TILO_VM_INSTALL_PROOF_V1={extended}\n").as_bytes()
                )
                .is_ok()
            );
        }

        // A development VM may copy an identical composefs tree through a
        // different OCI manifest. The copy digest is explicit and v2 remains
        // the only proof accepted for a board install without first boot.
        {
            let installed = format!("sha256:{}", "a".repeat(64));
            let mut payload = json!({
                "schema": "tilo.vm-install-image-proof.v3", "architecture": "arm64",
                "booted_image_digest": format!("sha256:{}", "b".repeat(64)),
                "installed_image_digest": installed,
                "copied_manifest_digest": installed,
                "composefs_digest": "c".repeat(128),
                "booted_composefs_digest": "c".repeat(128),
                "installed_composefs_digest": "c".repeat(128),
                "target_uki_sha256": "d".repeat(64),
                "disk_guid": "44444444-4444-4444-8444-444444444444",
                "partuuids": {
                    "efi": "11111111-1111-4111-8111-111111111111",
                    "boot": "22222222-2222-4222-8222-222222222222",
                    "root": "33333333-3333-4333-8333-333333333333",
                },
            });
            let line = format!("TILO_VM_INSTALL_PROOF_V3={payload}\n");
            let proof = super::parse_install_proof(line.as_bytes()).expect("valid dev copy proof");
            let source = json!({"disk_guid": "44444444-4444-4444-8444-444444444444",
            "final_partuuids": {
                "efi": "11111111-1111-4111-8111-111111111111",
                "boot": "22222222-2222-4222-8222-222222222222",
                "root": "33333333-3333-4333-8333-333333333333",
            }});
            let checked = super::check_install_proof(&proof, &source, Some(&"c".repeat(128)))
                .expect("matching development copy");
            assert_eq!(checked["schema"], "tilo.windows-vm-install-proof-check.v3");
            payload["copied_manifest_digest"] = json!(format!("sha256:{}", "e".repeat(64)));
            assert!(
                super::parse_install_proof(
                    format!("TILO_VM_INSTALL_PROOF_V3={payload}\n").as_bytes()
                )
                .is_err()
            );
        }

        // Phase: vm_v2_proof_binds_distinct_medium_and_signed_target.
        {
            let revision = "a".repeat(40);
            let target = format!("sha256:{}", "b".repeat(64));
            let origin = format!("code.tilo.so/tilo/tilo:lenovo-83ed-{revision}");
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
                    "target_ref": format!("code.tilo.so/tilo/tilo@{target}"),
                    "target_digest": target, "architecture": "aarch64",
                    "hardware_profile": "lenovo-83ed", "update_origin": origin,
                    "provenance": {"source_revision": revision,
                        "firmware_manifest_sha256": "d".repeat(64),
                        "firmware_candidate": format!("code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:{}", "e".repeat(64)),
                        "invocation_id": "123"},
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
            payload["selected_target"]["target_digest"] =
                json!(format!("sha256:{}", "a".repeat(64)));
            assert!(
                super::parse_install_proof(
                    format!("TILO_VM_INSTALL_PROOF_V2={payload}\n").as_bytes()
                )
                .is_err()
            );
            payload["selected_target"]["target_digest"] = json!(target);
            // A board target on the public repository is refused.
            payload["selected_target"]["target_ref"] =
                json!(format!("registry.tilo.so/tilo/tilo@{target}"));
            assert!(
                super::parse_install_proof(
                    format!("TILO_VM_INSTALL_PROOF_V2={payload}\n").as_bytes()
                )
                .is_err()
            );
            payload["selected_target"]["hardware_profile"] = json!("generic");
            payload["selected_target"]["update_origin"] = json!("registry.tilo.so/tilo/tilo:gen7");
            payload["selected_target"]["provenance"]["generation"] = json!(7);
            payload["selected_target"]["provenance"]["firmware_candidate"] = Value::Null;
            payload["selected_target"]["provenance"]["firmware_manifest_sha256"] = Value::Null;
            payload["installed_update_origin"] =
                json!("ostree-unverified-image:docker://registry.tilo.so/tilo/tilo:gen7");
            assert!(
                super::parse_install_proof(
                    format!("TILO_VM_INSTALL_PROOF_V2={payload}\n").as_bytes()
                )
                .is_ok()
            );
            // Explicit development targets remain a separate proof protocol.
            let dev_origin = "registry.tilo.so/tilo/tilo-vm:fixture-lenovo-83ed-ci";
            payload["schema"] = json!("tilo.vm-install-image-proof.v4");
            payload["selected_target"]["schema"] = json!("tilo.installer-dev-target.v1");
            payload["selected_target"]["target_ref"] =
                json!(format!("registry.tilo.so/tilo/tilo-vm@{target}"));
            payload["selected_target"]["hardware_profile"] = json!("lenovo-83ed");
            payload["selected_target"]["update_origin"] = json!(dev_origin);
            payload["selected_target"]["verifier_key_sha256"] = Value::Null;
            payload["selected_target"]["provenance"]["generation"] = Value::Null;
            payload["installed_update_origin"] =
                json!(format!("ostree-unverified-image:docker://{dev_origin}"));
            let dev_proof = super::parse_install_proof(
                format!("TILO_VM_INSTALL_PROOF_V4={payload}\n").as_bytes(),
            )
            .expect("explicit development target");
            assert_eq!(
                super::check_install_proof(&dev_proof, &source, Some(&"f".repeat(128)))
                    .expect("development target readback binding")["schema"],
                "tilo.windows-vm-install-proof-check.v4"
            );
            assert!(
                super::parse_install_proof(
                    format!("TILO_VM_INSTALL_PROOF_V2={payload}\n").as_bytes()
                )
                .is_err()
            );
            payload["selected_target"]["verifier_key_sha256"] = json!("e".repeat(64));
            assert!(
                super::parse_install_proof(
                    format!("TILO_VM_INSTALL_PROOF_V4={payload}\n").as_bytes()
                )
                .is_err()
            );
        }
    }

    use super::*;

    fn plan(layout: Layout) -> Plan {
        Plan {
            root: PathBuf::from("/w"),
            medium: PathBuf::from("/w/os/output/installer/qcow2/disk.qcow2"),
            external_target: None,
            skip_target_boot: false,
            layout,
            filesystem: None,
            resume: false,
            unlock: Unlock::Passphrase,
            username: "scenario".into(),
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

    /// Retained runtime logs are scrubbed alongside artifacts; a scrub failure still checks the runtime and
    /// preserves the pid for stop; a runtime scrub failure preserves the pid for a stop retry; a failed stop
    /// keeps the pid and a successful retry removes the runtime.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn runtime_scrubbing_and_stop_keep_the_pid_until_cleanup_succeeds() {
        // Phase: retained_runtime_logs_are_scrubbed_alongside_artifacts.
        {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("artifacts");
            let run = dir.path().join("runtime");
            std::fs::create_dir(&out).unwrap();
            std::fs::create_dir(&run).unwrap();
            std::fs::create_dir(out.join("storage")).unwrap();
            std::fs::write(out.join("state.json"), "ready").unwrap();
            let disk = out.join("storage/target.qcow2");
            std::fs::write(&disk, "typed accountsecret123 in VM disk bytes").unwrap();
            std::fs::write(run.join("serial.log"), "typed accountsecret123").unwrap();
            std::fs::write(run.join("uart.log"), "typed disksecret456").unwrap();
            let withheld =
                scrub_scenario_logs(&out, &run, &["accountsecret123", "disksecret456"]).unwrap();
            assert_eq!(withheld.len(), 2);
            assert!(std::fs::read(run.join("serial.log")).unwrap().is_empty());
            assert!(std::fs::read(run.join("uart.log")).unwrap().is_empty());
            assert_eq!(std::fs::read(out.join("state.json")).unwrap(), b"ready");
            assert!(!std::fs::read(disk).unwrap().is_empty());
        }

        // Phase: scrub_failure_still_checks_runtime_and_preserves_pid_for_stop.
        {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("artifacts");
            let run = dir.path().join("runtime");
            std::fs::create_dir(&out).unwrap();
            std::fs::create_dir(&run).unwrap();
            std::fs::write(run.join("serial.log"), "typed disksecret456").unwrap();
            let mut inspected = Vec::new();
            let error =
                scrub_scenario_logs_with(&out, &run, &["disksecret456"], |path, secrets| {
                    inspected.push(path.to_path_buf());
                    if path == out {
                        Err(std::io::Error::other("injected artifact read failure"))
                    } else {
                        scrub(path, secrets)
                    }
                })
                .unwrap_err();
            assert_eq!(error.to_string(), "injected artifact read failure");
            assert_eq!(inspected, [out, run.clone()]);
            assert!(run.is_dir(), "the pid file must survive until stop retries");
            assert!(std::fs::read(run.join("serial.log")).unwrap().is_empty());
        }

        // Phase: runtime_scrub_failure_preserves_pid_for_stop_retry.
        {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("artifacts");
            let run = dir.path().join("runtime");
            std::fs::create_dir(&out).unwrap();
            std::fs::create_dir(&run).unwrap();
            std::fs::write(run.join("serial.log"), "typed disksecret456").unwrap();
            std::fs::write(run.join("qemu.pid"), "123").unwrap();
            let error =
                scrub_scenario_logs_with(&out, &run, &["disksecret456"], |path, secrets| {
                    if path == run {
                        Err(std::io::Error::other("injected runtime read failure"))
                    } else {
                        scrub(path, secrets)
                    }
                })
                .unwrap_err();
            assert_eq!(error.to_string(), "injected runtime read failure");
            assert!(run.join("qemu.pid").exists());
        }

        // Phase: failed_stop_keeps_pid_and_successful_retry_removes_runtime.
        {
            struct StopRunner {
                status: &'static str,
                calls: Vec<String>,
            }
            impl Runner for StopRunner {
                fn output(
                    &mut self,
                    command: &CommandLine,
                ) -> Result<crate::hypervisor::Output, CliError> {
                    let action = command.args.last().cloned().unwrap_or_default();
                    self.calls.push(action.clone());
                    Ok(crate::hypervisor::Output {
                        success: true,
                        status: "0".into(),
                        stdout: if action == "status" {
                            self.status.into()
                        } else {
                            String::new()
                        },
                        stderr: String::new(),
                    })
                }
                fn interactive(&mut self, _: &CommandLine) -> Result<bool, CliError> {
                    Ok(false)
                }
            }
            let dir = tempfile::tempdir().unwrap();
            let mut plan = plan(Layout::Erase);
            plan.run = dir.path().join("runtime");
            std::fs::create_dir(&plan.run).unwrap();
            std::fs::write(plan.run.join("qemu.pid"), "123").unwrap();
            std::fs::write(plan.run.join("serial.log"), "sensitive").unwrap();
            let mut running = StopRunner {
                status: "running",
                calls: Vec::new(),
            };
            assert!(discard_unsanitized_runtime(&plan, &mut running).is_err());
            assert_eq!(running.calls, ["stop", "status"]);
            assert!(plan.run.join("qemu.pid").exists());
            let mut stopped = StopRunner {
                status: "stopped",
                calls: Vec::new(),
            };
            discard_unsanitized_runtime(&plan, &mut stopped).unwrap();
            assert_eq!(stopped.calls, ["stop", "status"]);
            assert!(!plan.run.exists());
        }
    }

    /// Every VM call shares one named cycle and the plan names its console; the Windows plan uses WHPX
    /// transport and a TCP console; an external raw target is retained and passed to WHPX transport.
    #[test]
    fn scenario_plans_share_one_cycle_and_pick_the_platform_transport() {
        // Phase: every_vm_call_shares_one_named_cycle_and_the_plan_names_its_console.
        #[cfg(unix)]
        {
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

        // Phase: windows_plan_uses_whpx_transport_and_tcp_console.
        #[cfg(windows)]
        {
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

        // Phase: external_raw_target_is_retained_and_passed_to_whpx_transport.
        #[cfg(windows)]
        {
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
}
