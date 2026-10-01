//! Fixed-verb privileged consumer for signed `BootPack` catalogue entries.
//!
//! This binary deliberately has no general installer interface. Its sole
//! mutating verb takes a catalogue id, resolves it under an image-owned path,
//! verifies its release signature and base-image compatibility, and asks bootc
//! to stage one digest-pinned deployment. It never consumes a path, URL, tag,
//! shell fragment, package name, or reboot request from a desktop client.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};
use system_extension::{ExtensionKind, HostImage, SignedExtensionManifest, TrustedKeyring};

const CATALOG_DIR: &str = "/usr/share/tilo/system-extensions/catalog.d";
const TRUST_DIR: &str = "/usr/share/tilo/system-extensions/trust.d";
const HARDWARE_CATALOG_PATH: &str =
    "/usr/share/tilo/system-extensions/hardware-catalog.d/pci-recommendations.json";
const HARDWARE_TRUST_PATH: &str =
    "/usr/share/tilo/system-extensions/hardware-trust.d/pci-recommendations.json";
const PCI_DEVICES_PATH: &str = "/sys/bus/pci/devices";
const STAGE_PATH: &str = "/var/lib/tilo/system-extensions/boot-pack-stage.json";
const REQUEST_PATH: &str = "/var/lib/tilo/system-extensions/boot-pack-request";
const STATUS_PATH: &str = "/run/tilo/system-extension-api/status.json";
const MAX_CATALOG_BYTES: u64 = 1024 * 1024;
const MAX_BOOTC_STATUS_BYTES: u64 = 1024 * 1024;
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// Helper diagnostics are discarded, but still bounded while they are drained.
const HELPER_STDERR_LIMIT: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicKeyDocument {
    schema_version: u16,
    key_id: String,
    public_key_hex: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootPackStage {
    schema_version: u16,
    generation: u64,
    extension_id: String,
    image: String,
    image_digest: String,
    reboot_required: bool,
}

/// A separately-signed, data-only hardware policy.  It deliberately cannot
/// contain an image reference: the `BootPack` catalogue remains the sole source
/// of a deployable image.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedPciRecommendationCatalog {
    schema_version: u16,
    key_id: String,
    catalog: PciRecommendationCatalog,
    signature: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PciRecommendationCatalog {
    schema_version: u16,
    recommendations: Vec<PciRecommendation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PciRecommendation {
    /// Stable release-owned rule id, useful when diagnosing a no-match.
    id: String,
    /// An exact id in the separately signed `BootPack` catalogue.
    boot_pack_id: String,
    pci: PciMatch,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the field names mirror the PCI sysfs ABI"
)]
struct PciMatch {
    vendor_id: u16,
    device_id: u16,
    subsystem_vendor_id: Option<u16>,
    subsystem_device_id: Option<u16>,
}

#[derive(Clone, Debug, Serialize)]
struct PciCatalogSigningPayload<'a> {
    domain: &'static str,
    schema_version: u16,
    key_id: &'a str,
    catalog: &'a PciRecommendationCatalog,
}

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_field_names,
    reason = "the field names mirror the PCI sysfs ABI"
)]
struct PciDevice {
    vendor_id: u16,
    device_id: u16,
    subsystem_vendor_id: Option<u16>,
    subsystem_device_id: Option<u16>,
}

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [verb, id] if verb == "stage-boot" => stage_boot_report(id),
        [verb] if verb == "stage-pending" => stage_pending(),
        [verb] if verb == "project-status" => project_status(),
        [verb] if verb == "recommend-pci" => recommend_pci(),
        _ => Err(
            "usage: tilo-system-extension-manager {stage-boot <catalog-id>|project-status}".into(),
        ),
    }
}

fn recommend_pci() -> Result<(), String> {
    let recommendations = recommended_boot_packs()?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": 1,
            "recommendations": recommendations,
        }))
        .map_err(|error| error.to_string())?
    );
    Ok(())
}

fn stage_pending() -> Result<(), String> {
    let bytes = safe_read(Path::new(REQUEST_PATH))?;
    let id = std::str::from_utf8(&bytes)
        .map_err(|_| "BootPack request is not UTF-8".to_string())?
        .strip_suffix('\n')
        .ok_or_else(|| "BootPack request has no terminating newline".to_string())?;
    if id.contains('\n') || id.contains('\r') {
        return Err("BootPack request has more than one field".into());
    }
    // A path unit also observes an already-existing request after boot.  Once
    // we have durably written either `staged` or `failed`, consume this queue
    // item so that a permanent signature/compatibility failure cannot retrigger
    // forever. A crash before the terminal projection leaves it for recovery.
    consume_terminal_request(Path::new(REQUEST_PATH), stage_boot_report(id))
}

fn consume_terminal_request(path: &Path, outcome: Result<(), String>) -> Result<(), String> {
    fs::remove_file(path)
        .map_err(|error| format!("could not clear terminal BootPack request: {error}"))?;
    outcome
}

fn stage_boot_report(id: &str) -> Result<(), String> {
    match stage_boot(id) {
        Ok(()) => Ok(()),
        Err(error) => {
            project_status_value(id, "failed", true, Some(&error))?;
            Err(error)
        }
    }
}

fn stage_boot(id: &str) -> Result<(), String> {
    validate_id(id)?;
    project_status_value(id, "staging", true, None)?;
    if !recommended_boot_packs()?.contains(id) {
        return Err(
            "BootPack id is not recommended by the signed PCI hardware catalogue for this machine"
                .into(),
        );
    }
    let signed = read_catalog(id)?;
    let keyring = read_keyring(&signed.key_id)?;
    signed
        .verify(&keyring)
        .map_err(|error| format!("catalogue signature is not trusted: {error}"))?;
    let manifest = &signed.manifest;
    if !matches!(
        manifest.kind,
        ExtensionKind::KernelModule | ExtensionKind::GpuDriverStack
    ) || !manifest.requires_reboot
    {
        return Err("catalogue entry is not a reboot-bound BootPack".into());
    }
    let host = current_host_image()?;
    manifest
        .preflight(&host)
        .map_err(|error| format!("BootPack is incompatible with this deployment: {error}"))?;
    let image = bootc_image_reference(&manifest.artifact.source, &manifest.artifact.sha256)?;

    bootc_switch(&image)?;

    let previous = load_stage().unwrap_or(None);
    let generation = previous.as_ref().map_or(Ok(1), |stage| {
        stage
            .generation
            .checked_add(1)
            .ok_or_else(|| "BootPack stage generation overflowed".to_string())
    })?;
    let stage = BootPackStage {
        schema_version: 1,
        generation,
        extension_id: id.to_string(),
        image_digest: manifest.artifact.sha256.clone(),
        image,
        reboot_required: true,
    };
    save_json(Path::new(STAGE_PATH), &stage, 0o600)?;
    project_stage(&stage)
}

fn project_status() -> Result<(), String> {
    let Some(stage) = load_stage()? else {
        return Ok(());
    };
    project_stage(&stage)
}

fn project_stage(stage: &BootPackStage) -> Result<(), String> {
    // This projection carries no path, registry credentials, or catalogue
    // bytes. It is intentionally safe for Settings to snapshot unprivileged.
    let reboot_required = current_host_image().map_or(true, |host| {
        host.digest != format!("sha256:{}", stage.image_digest)
    });
    // A broken status query must never falsely claim the new driver booted.
    let activation_state = if reboot_required { "staged" } else { "active" };
    project_status_value(&stage.extension_id, activation_state, reboot_required, None)
}

fn project_status_value(
    extension_id: &str,
    state: &str,
    reboot_required: bool,
    error: Option<&str>,
) -> Result<(), String> {
    let previous = load_stage().unwrap_or(None);
    let generation = previous.as_ref().map_or(0, |stage| stage.generation);
    let error = error.map(|error| error.chars().take(240).collect::<String>());
    let value = serde_json::json!({
        "schema_version": 1,
        "activation_class": "boot_pack",
        "state": state,
        "extension_id": extension_id,
        "stage_generation": generation,
        "reboot_required": reboot_required,
        "error": error,
    });
    save_json(Path::new(STATUS_PATH), &value, 0o644)?;
    println!(
        "{}",
        serde_json::to_string(&value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn read_catalog(id: &str) -> Result<SignedExtensionManifest, String> {
    let path = Path::new(CATALOG_DIR).join(format!("{id}.json"));
    let bytes = safe_read(&path)?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid signed catalog entry: {error}"))
}

fn read_keyring(key_id: &str) -> Result<TrustedKeyring, String> {
    validate_id(key_id)?;
    let path = Path::new(TRUST_DIR).join(format!("{key_id}.json"));
    let bytes = safe_read(&path)?;
    let document: PublicKeyDocument = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid pinned public key: {error}"))?;
    if document.schema_version != 1 || document.key_id != key_id {
        return Err("pinned public key does not match requested key id".into());
    }
    let bytes = hex::decode(&document.public_key_hex)
        .map_err(|_| "pinned public key is not hexadecimal".to_string())?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(
        &bytes
            .as_slice()
            .try_into()
            .map_err(|_| "pinned public key has the wrong length".to_string())?,
    )
    .map_err(|_| "pinned public key is malformed".to_string())?;
    let mut keyring = TrustedKeyring::default();
    keyring
        .insert(key_id, key)
        .map_err(|error| error.to_string())?;
    Ok(keyring)
}

fn recommended_boot_packs() -> Result<BTreeSet<String>, String> {
    let signed = read_pci_catalog()?;
    verify_pci_catalog(&signed)?;
    let devices = read_pci_devices()?;
    let mut packs = BTreeSet::new();
    for device in devices {
        let specificity = signed
            .catalog
            .recommendations
            .iter()
            .filter(|recommendation| recommendation.pci.matches(&device))
            .map(|recommendation| recommendation.pci.specificity())
            .max();
        if let Some(specificity) = specificity {
            packs.extend(
                signed
                    .catalog
                    .recommendations
                    .iter()
                    .filter(|recommendation| {
                        recommendation.pci.matches(&device)
                            && recommendation.pci.specificity() == specificity
                    })
                    .map(|recommendation| recommendation.boot_pack_id.clone()),
            );
        }
    }
    Ok(packs)
}

fn read_pci_catalog() -> Result<SignedPciRecommendationCatalog, String> {
    let bytes = safe_read(Path::new(HARDWARE_CATALOG_PATH))?;
    let catalog: SignedPciRecommendationCatalog = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid signed PCI hardware catalogue: {error}"))?;
    validate_pci_catalog(&catalog)?;
    Ok(catalog)
}

fn validate_pci_catalog(signed: &SignedPciRecommendationCatalog) -> Result<(), String> {
    if signed.schema_version != 1 || signed.catalog.schema_version != 1 {
        return Err("unsupported PCI hardware catalogue schema".into());
    }
    validate_id(&signed.key_id)?;
    let mut rule_ids = BTreeSet::new();
    let mut selectors = BTreeSet::new();
    for recommendation in &signed.catalog.recommendations {
        validate_id(&recommendation.id)?;
        validate_id(&recommendation.boot_pack_id)?;
        if !rule_ids.insert(&recommendation.id) {
            return Err("PCI hardware catalogue has duplicate rule ids".into());
        }
        if recommendation.pci.subsystem_vendor_id.is_some()
            != recommendation.pci.subsystem_device_id.is_some()
        {
            return Err("PCI hardware catalogue must specify both subsystem ids or neither".into());
        }
        let selector = (
            recommendation.pci.vendor_id,
            recommendation.pci.device_id,
            recommendation.pci.subsystem_vendor_id,
            recommendation.pci.subsystem_device_id,
        );
        if !selectors.insert(selector) {
            return Err("PCI hardware catalogue has duplicate match selectors".into());
        }
    }
    Ok(())
}

fn verify_pci_catalog(signed: &SignedPciRecommendationCatalog) -> Result<(), String> {
    let bytes = safe_read(Path::new(HARDWARE_TRUST_PATH))?;
    let document: PublicKeyDocument = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid pinned PCI hardware catalogue key: {error}"))?;
    verify_pci_catalog_against(signed, &document)
}

fn verify_pci_catalog_against(
    signed: &SignedPciRecommendationCatalog,
    document: &PublicKeyDocument,
) -> Result<(), String> {
    if document.schema_version != 1 || document.key_id != signed.key_id {
        return Err("pinned PCI hardware catalogue key does not match catalogue".into());
    }
    let public_key = decode_public_key(&document.public_key_hex)?;
    let payload = PciCatalogSigningPayload {
        // This is a signed wire-format identifier, independent of product branding.
        // Changing it invalidates the already published v1 catalogue signature.
        domain: "tilo.system-extension.pci-recommendations.v1",
        schema_version: signed.schema_version,
        key_id: &signed.key_id,
        catalog: &signed.catalog,
    };
    let bytes = serde_json::to_vec(&payload)
        .map_err(|error| format!("cannot canonicalize PCI hardware catalogue: {error}"))?;
    let signature = ed25519_dalek::Signature::from_slice(&signed.signature)
        .map_err(|_| "PCI hardware catalogue signature is malformed".to_string())?;
    public_key
        .verify(&bytes, &signature)
        .map_err(|_| "PCI hardware catalogue signature is not trusted".to_string())
}

fn decode_public_key(public_key_hex: &str) -> Result<ed25519_dalek::VerifyingKey, String> {
    let bytes = hex::decode(public_key_hex)
        .map_err(|_| "pinned public key is not hexadecimal".to_string())?;
    ed25519_dalek::VerifyingKey::from_bytes(
        &bytes
            .as_slice()
            .try_into()
            .map_err(|_| "pinned public key has the wrong length".to_string())?,
    )
    .map_err(|_| "pinned public key is malformed".to_string())
}

impl PciMatch {
    fn matches(&self, device: &PciDevice) -> bool {
        self.vendor_id == device.vendor_id
            && self.device_id == device.device_id
            && self
                .subsystem_vendor_id
                .is_none_or(|value| device.subsystem_vendor_id == Some(value))
            && self
                .subsystem_device_id
                .is_none_or(|value| device.subsystem_device_id == Some(value))
    }

    fn specificity(&self) -> u8 {
        2_u8.saturating_add(u8::from(self.subsystem_vendor_id.is_some()))
            .saturating_add(u8::from(self.subsystem_device_id.is_some()))
    }
}

fn read_pci_devices() -> Result<Vec<PciDevice>, String> {
    let entries = fs::read_dir(PCI_DEVICES_PATH)
        .map_err(|error| format!("cannot inspect PCI hardware: {error}"))?;
    let mut devices = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot enumerate PCI hardware: {error}"))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !valid_pci_address(name) {
            continue;
        }
        let vendor_id = read_pci_hex(&path.join("vendor"))?;
        let device_id = read_pci_hex(&path.join("device"))?;
        let subsystem_vendor_id = read_optional_pci_hex(&path.join("subsystem_vendor"))?;
        let subsystem_device_id = read_optional_pci_hex(&path.join("subsystem_device"))?;
        devices.push(PciDevice {
            vendor_id,
            device_id,
            subsystem_vendor_id,
            subsystem_device_id,
        });
    }
    Ok(devices)
}

fn valid_pci_address(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 12
        && bytes.get(4) == Some(&b':')
        && bytes.get(7) == Some(&b':')
        && bytes.get(10) == Some(&b'.')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10) || byte.is_ascii_hexdigit())
}

fn read_optional_pci_hex(path: &Path) -> Result<Option<u16>, String> {
    match fs::read_to_string(path) {
        Ok(value) => parse_pci_hex(&value).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect PCI hardware {}: {error}",
            path.display()
        )),
    }
}

fn read_pci_hex(path: &Path) -> Result<u16, String> {
    let value = fs::read_to_string(path)
        .map_err(|error| format!("cannot inspect PCI hardware {}: {error}", path.display()))?;
    parse_pci_hex(&value)
}

fn parse_pci_hex(value: &str) -> Result<u16, String> {
    let value = value.trim();
    let Some(value) = value.strip_prefix("0x") else {
        return Err("PCI hardware id is not hexadecimal".into());
    };
    if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("PCI hardware id is malformed".into());
    }
    u16::from_str_radix(value, 16).map_err(|_| "PCI hardware id is malformed".into())
}

fn safe_read(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot read image-owned input {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_CATALOG_BYTES
    {
        return Err(format!("unsafe image-owned input {}", path.display()));
    }
    fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn bounded_stdout(
    program: &Path,
    args: &[&str],
    limit: u64,
    timeout: Duration,
    label: &str,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(program);
    command.args(args);
    let output = bounded_command::run_prepared_with_limits(
        command,
        None,
        usize::try_from(limit).unwrap_or(usize::MAX),
        HELPER_STDERR_LIMIT,
        timeout,
    )
    .map_err(|error| format!("{label}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{label} failed with {}", output.status));
    }
    Ok(output.stdout)
}

fn current_host_image() -> Result<HostImage, String> {
    let output = bounded_stdout(
        Path::new("/usr/bin/bootc"),
        &["status", "--format", "json"],
        MAX_BOOTC_STATUS_BYTES,
        HELPER_TIMEOUT,
        "bootc status",
    )?;
    let status: serde_json::Value = serde_json::from_slice(&output)
        .map_err(|_| "bootc returned malformed deployment status".to_string())?;
    let image = status
        .pointer("/status/booted/image/image/image")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "bootc status has no booted image reference".to_string())?;
    let digest = image_digest(image)
        .ok_or_else(|| "bootc status has no digest-pinned booted image".to_string())?;
    let architecture = uname("-m", "architecture")?;
    let kernel_abi = uname("-r", "kernel ABI")?;
    Ok(HostImage {
        digest,
        architecture,
        kernel_abi,
    })
}

fn bootc_switch(image: &str) -> Result<(), String> {
    bootc_switch_with(Path::new("/usr/bin/bootc"), image)
}

fn bootc_switch_with(program: &Path, image: &str) -> Result<(), String> {
    let status = Command::new(program)
        .args(["switch", "--transport=registry", image])
        .status()
        .map_err(|error| format!("could not start bootc switch: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "bootc refused the digest-pinned BootPack deployment ({status})"
        ))
    }
}

fn uname(flag: &str, label: &str) -> Result<String, String> {
    let output = bounded_stdout(
        Path::new("/usr/bin/uname"),
        &[flag],
        256,
        HELPER_TIMEOUT,
        &format!("uname {label}"),
    )?;
    let value = String::from_utf8(output)
        .map_err(|_| format!("uname returned invalid {label}"))?
        .trim()
        .to_string();
    if value.is_empty() || value.len() > 256 || value.contains(['\n', '\r', '\0']) {
        return Err(format!("uname returned invalid {label}"));
    }
    Ok(value)
}

fn image_digest(image: &str) -> Option<String> {
    let (_, digest) = image.rsplit_once("@sha256:")?;
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Some(format!("sha256:{digest}"))
    } else {
        None
    }
}

fn bootc_image_reference(source: &str, sha256: &str) -> Result<String, String> {
    let image = source
        .strip_prefix("bootc://")
        .ok_or_else(|| "BootPack source must be a bootc:// digest reference".to_string())?;
    let Some((name, digest)) = image.rsplit_once("@sha256:") else {
        return Err("BootPack image reference must be pinned by sha256".into());
    };
    if name.is_empty() || name.contains([' ', '\n', '\r', '\0']) || digest != sha256 {
        return Err("BootPack image reference does not match its signed digest".into());
    }
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("BootPack image digest is malformed".into());
    }
    Ok(format!("{name}@sha256:{digest}"))
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
        || id.starts_with(['.', '-', '_'])
        || id.ends_with(['.', '-', '_'])
    {
        return Err("invalid catalog id".into());
    }
    Ok(())
}

fn load_stage() -> Result<Option<BootPackStage>, String> {
    let path = Path::new(STAGE_PATH);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = safe_read(path)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("invalid BootPack stage state: {error}"))
}

fn save_json<T: Serialize>(path: &Path, value: &T, mode: u32) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    tilo_files::native::atomic::write_creating_parent_with_mode(path, &bytes, mode)
        .map_err(|error| format!("cannot atomically write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn boot_packs_and_catalogues_are_digest_pinned_signed_and_matched() {
        // Phase `boot_pack_image_must_be_digest_pinned_and_match_manifest`.
        {
            let digest = "a".repeat(64);
            assert_eq!(
                bootc_image_reference(
                    &format!("bootc://registry.example.test/drivers@sha256:{digest}"),
                    &digest
                )
                .unwrap(),
                format!("registry.example.test/drivers@sha256:{digest}")
            );
            assert!(
                bootc_image_reference("bootc://registry.example.test/drivers:latest", &digest)
                    .is_err()
            );
            assert!(
                bootc_image_reference(
                    &format!(
                        "bootc://registry.example.test/drivers@sha256:{}",
                        "b".repeat(64)
                    ),
                    &digest
                )
                .is_err()
            );
        }
        // Phase `image_digest_requires_an_exact_pin`.
        {
            let digest = "a".repeat(64);
            assert_eq!(
                image_digest(&format!("registry/tilo@sha256:{digest}")),
                Some(format!("sha256:{digest}"))
            );
            assert_eq!(image_digest("registry/tilo:latest"), None);
        }
        // Phase `shipped_pci_catalogue_has_a_valid_separate_signature`.
        {
            let signed: SignedPciRecommendationCatalog = serde_json::from_slice(include_bytes!(
                "../../../../../os/usr/share/tilo/system-extensions/hardware-catalog.d/pci-recommendations.json"
            ))
            .unwrap();
            let key: PublicKeyDocument = serde_json::from_slice(include_bytes!(
                "../../../../../os/usr/share/tilo/system-extensions/hardware-trust.d/pci-recommendations.json"
            ))
            .unwrap();
            validate_pci_catalog(&signed).unwrap();
            verify_pci_catalog_against(&signed, &key).unwrap();

            // Renaming the key in both documents cannot authorize unsigned changes.
            let mut renamed = signed;
            renamed.key_id = "tilo-pci-catalog-v2".into();
            let renamed_key = PublicKeyDocument {
                key_id: renamed.key_id.clone(),
                ..key
            };
            assert!(verify_pci_catalog_against(&renamed, &renamed_key).is_err());
        }
        // Phase `subsystem_rule_wins_over_generic_pci_rule`.
        {
            let device = PciDevice {
                vendor_id: 0x10de,
                device_id: 0x2684,
                subsystem_vendor_id: Some(0x1028),
                subsystem_device_id: Some(0x0c5f),
            };
            let generic = PciMatch {
                vendor_id: 0x10de,
                device_id: 0x2684,
                subsystem_vendor_id: None,
                subsystem_device_id: None,
            };
            let specific = PciMatch {
                subsystem_vendor_id: device.subsystem_vendor_id,
                subsystem_device_id: device.subsystem_device_id,
                ..generic
            };
            assert!(generic.matches(&device));
            assert!(specific.matches(&device));
            assert!(specific.specificity() > generic.specificity());
        }
    }

    #[test]
    fn catalog_ids_and_terminal_failures_are_bounded() {
        // Phase `catalog_id_cannot_be_a_path_or_command_fragment`.
        {
            for invalid in ["../nvidia", "nvidia open", "nvidia\nopen", "", "-driver"] {
                assert!(
                    validate_id(invalid).is_err(),
                    "{invalid:?} must be rejected"
                );
            }
            assert!(validate_id("nvidia-open.580").is_ok());
        }
        // Phase `terminal_failure_consumes_the_queue_and_cannot_retry_forever`.
        {
            let directory = tempfile::tempdir().unwrap();
            let request = directory.path().join("boot-pack-request");
            fs::write(&request, b"unsupported-pack\n").unwrap();
            let error =
                consume_terminal_request(&request, Err("not compatible".into())).unwrap_err();
            assert_eq!(error, "not compatible");
            assert!(!request.exists());
        }
    }

    #[test]
    #[cfg(unix)]
    fn privileged_helpers_run_fixed_argv_with_bounded_capture() {
        // Phase `bootc_switch_executes_only_its_fixed_argv`.
        {
            use std::os::unix::fs::PermissionsExt;

            let directory = tempfile::tempdir().unwrap();
            let command = directory.path().join("bootc");
            let capture = directory.path().join("argv");
            fs::write(
                &command,
                format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", capture.display()),
            )
            .unwrap();
            fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
            let image = format!("registry.example.test/boot-pack@sha256:{}", "a".repeat(64));
            bootc_switch_with(&command, &image).unwrap();
            assert_eq!(
                fs::read_to_string(capture).unwrap(),
                format!("switch\n--transport=registry\n{image}\n")
            );
        }
        // Phase `privileged_helper_capture_rejects_overflow_and_timeout`.
        {
            use std::os::unix::fs::PermissionsExt;

            let directory = tempfile::tempdir().unwrap();
            let overflow = directory.path().join("overflow");
            fs::write(&overflow, "#!/bin/sh\nhead -c 8192 /dev/zero\n").unwrap();
            fs::set_permissions(&overflow, fs::Permissions::from_mode(0o755)).unwrap();
            let error = bounded_stdout(
                &overflow,
                &[],
                4096,
                tilo_test_env::hang_bound(),
                "overflow probe",
            )
            .unwrap_err();
            assert!(error.contains("exceeds 4096 bytes"), "{error}");

            let hangs = directory.path().join("hangs");
            // The probe outlives the hang bound, so returning within it proves
            // the 25 ms deadline killed it.
            let sleep = tilo_test_env::hang_bound().as_secs() * 2;
            fs::write(&hangs, format!("#!/bin/sh\nsleep {sleep}\n")).unwrap();
            fs::set_permissions(&hangs, fs::Permissions::from_mode(0o755)).unwrap();
            let started = Instant::now();
            let error = bounded_stdout(
                &hangs,
                &[],
                4096,
                Duration::from_millis(25),
                "timeout probe",
            )
            .unwrap_err();
            assert!(error.contains("timed out"), "{error}");
            assert!(started.elapsed() < tilo_test_env::hang_bound());
        }
    }
}
