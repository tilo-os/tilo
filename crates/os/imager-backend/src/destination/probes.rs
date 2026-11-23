//! What this computer proves about its boot chain and what its image can mount.
//!
//! Each probe only observes; the privileged backend repeats the real checks before writing.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

use install_profile_model::FILESYSTEMS;
use serde_json::Value;

const OMITTED_INITRAMFS_DRIVERS: &str = "/usr/lib/tilo/initramfs-omitted-drivers.txt";

/// From the image's list of omitted drivers.
pub fn filesystem_support_from_manifest(
    manifest: &str,
) -> Result<[bool; FILESYSTEMS.len()], String> {
    let mut supported = [true; FILESYSTEMS.len()];
    let mut seen = HashSet::new();
    for line in manifest.lines() {
        if !seen.insert(line) {
            return Err("The image has a duplicate initramfs driver entry".into());
        }
        if let Some(index) = FILESYSTEMS
            .iter()
            .position(|filesystem| *filesystem == line)
        {
            let Some(flag) = supported.get_mut(index) else {
                return Err("The image has an invalid initramfs driver manifest".into());
            };
            *flag = false;
        } else {
            // Every image also omits the large-firmware GPU drivers to keep its UKI small; those
            // entries do not change which filesystem the installer can offer.
            if !matches!(line, "amdgpu" | "nouveau" | "xe") {
                return Err("The image has an invalid initramfs driver manifest".into());
            }
        }
    }
    Ok(supported)
}

pub fn filesystem_support() -> Result<[bool; FILESYSTEMS.len()], String> {
    use std::os::unix::fs::MetadataExt as _;
    let path = std::path::Path::new(OMITTED_INITRAMFS_DRIVERS);
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "The image's initramfs driver manifest is unavailable".to_string())?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("The image's initramfs driver manifest is not a trusted regular file".into());
    }
    let manifest = std::fs::read_to_string(path)
        .map_err(|_| "The image's initramfs driver manifest is unreadable".to_string())?;
    filesystem_support_from_manifest(&manifest)
}
/// Encryption does not require a TPM, but the TPM decides whether the disk unlocks itself, so this
/// decides whether the secret below is convenience or the only way in.
///
/// The unprivileged wizard can only observe the device node; the backend repeats the real probe, and
/// a root process uses the canonical `tilo-rollback tpm-status` protocol instead. Sampled once per
/// process.
pub fn has_tpm2() -> bool {
    static PROBE: OnceLock<bool> = OnceLock::new();
    let present =
        std::path::Path::new("/dev/tpmrm0").exists() || std::path::Path::new("/dev/tpm0").exists();
    if !present {
        return false;
    }
    if rustix::process::geteuid().as_raw() != 0 {
        return true;
    }
    *PROBE.get_or_init(|| {
        bounded_command::run(
            "/usr/libexec/tilo/tilo-rollback",
            &["tpm-status", "--json"],
            None,
            64 * 1024,
            std::time::Duration::from_secs(10),
        )
        .is_ok_and(|output| {
            output.status.success()
                && serde_json::from_slice::<Value>(&output.stdout)
                    .is_ok_and(|value| value.get("max_tries").is_some())
        })
    })
}

pub fn secure_boot_enabled() -> bool {
    std::fs::read_dir("/sys/firmware/efi/efivars")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("SecureBoot-")
        })
        .and_then(|entry| std::fs::read(entry.path()).ok())
        .and_then(|bytes| bytes.get(4).copied())
        == Some(1)
}

pub fn kernel_lockdown_active() -> bool {
    std::fs::read_to_string("/sys/kernel/security/lockdown")
        .is_ok_and(|state| state.contains("[integrity]") || state.contains("[confidentiality]"))
}

/// Whether this image proves that the target it writes keeps the verified-boot boundary after
/// reboot. A future UKI signing integration owns this and must fail closed unless the target UKI,
/// initramfs, command line and firmware trust chain are all covered.
pub fn target_verified_boot_ready() -> bool {
    bounded_command::run(
        "/usr/libexec/tilo-verified-boot-install-preflight",
        &[],
        None,
        64 * 1024,
        std::time::Duration::from_secs(30),
    )
    .is_ok_and(|output| output.status.success())
}

const ROLLBACK_BIN: &str = "/usr/libexec/tilo/tilo-rollback";
const VERIFIED_BOOT_PREFLIGHT: &str = "/usr/libexec/tilo-verified-boot-install-preflight";
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent probe result, not a state combination"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectionReport {
    pub tpm2: bool,
    pub pin_defensible: bool,
    pub lockout_auth_set: bool,
    pub shared_with_windows: bool,
    pub target_verified_boot: bool,
}

/// Run as root by the unattended installation.
pub struct SystemProtection;

impl SystemProtection {
    /// What the destination's TPM2 and its installed target's measured-boot preflight establish.
    pub fn probe(&mut self) -> Result<ProtectionReport, String> {
        // A real TPM2 protocol read, not a device-node test: the dictionary-attack capability is
        // what makes a short PIN defensible, the first boot hardens it, and the value reaches the
        // operator.
        let rollback = bounded_command::run(
            ROLLBACK_BIN,
            &["tpm-status", "--json"],
            None,
            64 * 1024,
            PROBE_TIMEOUT,
        )
        .map_err(|error| format!("The destination TPM2 could not be checked: {error}"))?;
        if !rollback.status.success() {
            return Err(format!(
                "The destination does not answer the canonical TPM2 probe (this path needs TPM2                  access, normally by running the unattended entry point as root): {}",
                rollback.stderr_diagnostic()
            ));
        }
        let status: Value = serde_json::from_slice(&rollback.stdout)
            .map_err(|_| "The destination TPM2 probe returned an unreadable answer".to_string())?;
        let pin_defensible = status
            .get("pin_is_defensible")
            .and_then(Value::as_bool)
            .ok_or("The destination TPM2 probe did not report its lockout policy")?;
        let lockout_auth_set = status
            .get("reset_auth_set")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let shared_with_windows = status
            .get("shared_with_windows")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let preflight = bounded_command::run(
            VERIFIED_BOOT_PREFLIGHT,
            &[],
            None,
            64 * 1024,
            PROBE_TIMEOUT,
        )
        .map_err(|error| {
            format!("The installed target's measured-boot boundary could not be checked: {error}")
        })?;
        if !preflight.status.success() {
            return Err(format!(
                "The installed target does not prove the measured-boot boundary this PIN would \
                 be sealed to: {}",
                preflight.stderr_diagnostic()
            ));
        }
        Ok(ProtectionReport {
            tpm2: true,
            pin_defensible,
            lockout_auth_set,
            shared_with_windows,
            target_verified_boot: true,
        })
    }
}
