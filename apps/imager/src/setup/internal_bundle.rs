//! Bounded internal-installer manifest validation.
//! Consistency is not authenticated provenance, filesystem qualification, or boot proof.
//!
//! Schema 2 is the contract of `os/dev/internal-bundle.py`, which exports it;
//! both sides must accept exactly the same manifests. Both test suites
//! validate the shared fixture `os/tests/fixtures/internal-bundle-v2.json`,
//! and the Python suite derives that fixture's command line from the live
//! installer's real kargs (design spec 2026-09-22-usb-free-install §3).

use super::staging::STAGING_ROOT_TYPE;
use serde::Deserialize;

pub const MAX_MANIFEST_BYTES: usize = 65_536;
const MAX_IMAGE_BYTES: u64 = 1 << 40;
const ESP_GUID: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// The only manifest schema this build accepts.
pub const SCHEMA: u32 = 2;
/// Manifest purpose; the release catalog's internal entries carry the same tag.
pub const PURPOSE: &str = "tilo-internal-installer";
/// The staging root partition is never smaller than the live root floor, even
/// though its minimised image is; the live installer grows it at boot.
pub const LIVE_ROOT_FLOOR_BYTES: u64 = 40 << 30;
/// The one root locator every Tilo UKI carries; os/usr/libexec/tilo-root-discover
/// resolves it on the disk whose ESP booted the UKI.
pub const ROOT_LOCATOR: &str = "tilo-auto";
/// Bare flags the live command line carries (os/Containerfile.installer).
const LIVE_FLAGS: [&str; 6] = [
    "rw",
    "quiet",
    "splash",
    "slab_nomerge",
    "plymouth.ignore-serial-consoles",
    "tilo.install",
];

/// The removable-media fallback loader a staging ESP boots through. The
/// exporter installs the unchanged live UKI itself at this path.
pub fn fallback_loader(architecture: &str) -> Option<&'static str> {
    match architecture {
        "x86_64" => Some("/EFI/BOOT/BOOTX64.EFI"),
        "aarch64" => Some("/EFI/BOOT/BOOTAA64.EFI"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    purpose: String,
    release_id: String,
    architecture: String,
    firmware: String,
    secure_boot: String,
    installer_protocol: u32,
    #[serde(default)]
    account_custody: Option<super::account_admission::ConsumerCapability>,
    #[serde(default)]
    home_archive: Option<super::home_admission::ConsumerCapability>,
    #[serde(default)]
    applications: Option<super::applications_admission::ConsumerCapability>,
    #[serde(default)]
    original_source: Option<super::original_admission::ConsumerCapability>,
    source_image_bytes: u64,
    components: Vec<Component>,
    boot_binding: BootBinding,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Component {
    role: String,
    filename: String,
    byte_size: u64,
    sha256: String,
    partition_type_guid: String,
    filesystem_type: String,
    filesystem_uuid: String,
    minimum_partition_bytes: u64,
    source_offset: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootBinding {
    efi_path: String,
    uki_architecture: String,
    uki_sha256: String,
    uki_cmdline: String,
    composefs_digest: String,
}

/// Validated data only. This value grants neither stable-byte nor installation authority.
#[derive(Debug)]
pub struct InternalBundle {
    manifest: Manifest,
}

/// Immutable validated metadata; this view grants no authentication or file authority.
#[derive(Clone, Copy, Debug)]
pub struct ComponentView<'a> {
    pub role: &'a str,
    pub filename: &'a str,
    pub byte_size: u64,
    pub sha256: &'a str,
    /// Size of the partition the staging writer creates; `>= byte_size`.
    pub minimum_partition_bytes: u64,
    pub partition_type_guid: &'a str,
    /// `XXXX-XXXX` for the ESP, the Btrfs fsid (lowercase UUID) for the root.
    pub filesystem_uuid: &'a str,
}

impl InternalBundle {
    pub fn component(&self, index: usize) -> Option<ComponentView<'_>> {
        self.manifest.components.get(index).map(|c| ComponentView {
            role: &c.role,
            filename: &c.filename,
            byte_size: c.byte_size,
            sha256: &c.sha256,
            minimum_partition_bytes: c.minimum_partition_bytes,
            partition_type_guid: &c.partition_type_guid,
            filesystem_uuid: &c.filesystem_uuid,
        })
    }

    /// The component with this role (`staging-esp` or `staging-root`).
    pub fn component_by_role(&self, role: &str) -> Option<ComponentView<'_>> {
        let index = self
            .manifest
            .components
            .iter()
            .position(|c| c.role == role)?;
        self.component(index)
    }

    /// Fallback loader path inside the staging ESP; it holds the live UKI.
    pub fn efi_path(&self) -> &str {
        &self.manifest.boot_binding.efi_path
    }
    /// SHA-256 of the live UKI at `efi_path`, as built and signed.
    pub fn uki_sha256(&self) -> &str {
        &self.manifest.boot_binding.uki_sha256
    }

    /// Check the prepared account requirement against this selected image's proof.
    /// # Errors
    /// Refuses protected custody when the authenticated target lacks its protocol.
    pub fn require_account_protocol(&self, required: Option<u32>) -> Result<(), String> {
        if required.is_some_and(|protocol| {
            self.manifest
                .account_custody
                .as_ref()
                .is_none_or(|capability| capability.protected_grant_protocol != protocol)
        }) {
            return Err("The selected system cannot use the saved connections".into());
        }
        Ok(())
    }

    /// Consumer proof is authenticated only when this bundle comes from the signed release selection.
    pub fn account_custody(&self) -> Option<&super::account_admission::ConsumerCapability> {
        self.manifest.account_custody.as_ref()
    }

    /// Check separately selected website sessions against the authenticated receiver.
    /// # Errors
    /// Refuses a target without its actual browser and engine consumer proof.
    pub fn require_service_session_protocol(&self, required: Option<u32>) -> Result<(), String> {
        if required.is_some_and(|protocol| {
            self.manifest.account_custody.as_ref().is_none_or(|proof| {
                !proof.supports_service_session_protocol(protocol)
                    && !super::account_admission::source_candidate(protocol, proof).unwrap_or(false)
            })
        }) {
            return Err("The selected system cannot use the saved connections".into());
        }
        Ok(())
    }

    /// Check the Home declaration against authenticated target consumers.
    /// # Errors
    /// Refuses images without the required archive protocol.
    /// # Errors
    /// Refuses selected app data without its separate authenticated consumer proof.
    pub fn require_applications_protocol(&self, required: Option<u32>) -> Result<(), String> {
        super::applications_admission::require(self.manifest.applications.as_ref(), required)
            .map_err(|e| e.to_string())
    }
    pub fn require_home_protocol(&self, required: Option<u32>) -> Result<(), String> {
        super::home_admission::require(self.manifest.home_archive.as_ref(), required)
            .map_err(|error| error.to_string())
    }

    /// This proof is authenticated by the enclosing selected signed image manifest.
    pub fn home_archive(&self) -> Option<&super::home_admission::ConsumerCapability> {
        self.manifest.home_archive.as_ref()
    }

    /// The enclosing selected manifest authenticates this declaration only;
    /// actual installed readiness must still be independently checked.
    pub fn original_source(&self) -> Option<&super::original_admission::ConsumerCapability> {
        self.manifest.original_source.as_ref()
    }
    pub fn require_original_protocol(&self, required: Option<u32>) -> Result<(), String> {
        super::original_admission::require(self.original_source(), required)
            .map_err(|error| error.to_string())
    }

    pub fn release_id(&self) -> &str {
        &self.manifest.release_id
    }
    pub fn architecture(&self) -> &str {
        &self.manifest.architecture
    }
    pub fn purpose(&self) -> &str {
        &self.manifest.purpose
    }
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

pub(crate) fn canonical_digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub(crate) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

fn guid(value: &str) -> bool {
    value.len() == 36
        && value != "00000000-0000-0000-0000-000000000000"
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}

/// Accept exactly the live installer command line, as built. Mirrors
/// `validate_cmdline` in os/dev/internal-bundle.py.
fn validate_cmdline(boot: &BootBinding) -> Result<(), String> {
    let text = &boot.uki_cmdline;
    require(
        !text.is_empty()
            && text.len() <= 4096
            && text
                .bytes()
                .all(|c| (32..=126).contains(&c) && !b"\\\"'".contains(&c)),
        "unsupported kernel command line",
    )?;
    let tokens: Vec<_> = text.split_ascii_whitespace().collect();
    require(tokens.len() <= 64, "too many kernel arguments")?;
    let mut names = std::collections::HashSet::new();
    let mut consoles = std::collections::HashSet::new();
    for token in &tokens {
        let (name, value) = token.split_once('=').unwrap_or((token, ""));
        if name == "console" {
            require(
                consoles.insert(*token)
                    && consoles.len() <= 3
                    && matches!(value, "tty0" | "ttyAMA0,115200" | "ttyS0,115200"),
                "unsupported console",
            )?;
            continue;
        }
        require(names.insert(name), "duplicate kernel argument")?;
        let allowed = if LIVE_FLAGS.contains(&name) {
            *token == name
        } else {
            match name {
                "root" => value == ROOT_LOCATOR,
                "rd.luks.options" => value == "tpm2-device=auto,fido2-device=auto",
                "composefs" => value == boot.composefs_digest,
                "tilo.mode" => value == "normal",
                "init_on_alloc" | "page_alloc.shuffle" => value == "1",
                "lockdown" => value == "integrity",
                "randomize_kstack_offset" => value == "on",
                "vsyscall" => value == "none",
                "preempt" => value == "full",
                "systemd.import_credentials" => value == "no",
                "tilo.generation" => {
                    !value.is_empty()
                        && value.len() <= 10
                        && value.as_bytes()[0] != b'0'
                        && value.bytes().all(|c| c.is_ascii_digit())
                }
                _ => false,
            }
        };
        require(allowed, "unrecognized command-line binding")?;
    }
    require(
        ["root", "composefs", "tilo.install", "rw"]
            .iter()
            .all(|name| names.contains(name)),
        "missing boot binding",
    )
}

/// Parse the exact schema-2 exporter contract, without authenticating its origin.
/// # Errors
/// Rejects ambiguous JSON, unsupported profiles and inconsistent or unbounded facts.
pub fn validate_internal_bundle(manifest: &[u8]) -> Result<InternalBundle, String> {
    require(
        !manifest.is_empty() && manifest.len() <= MAX_MANIFEST_BYTES,
        "manifest exceeds size limit",
    )?;
    // Serde's derived structs also accept positional arrays. Check the wire
    // shapes first, then strictly decode the ORIGINAL bytes below: never
    // serialize this Value, which would collapse duplicate object keys.
    {
        let shape: serde_json::Value =
            serde_json::from_slice(manifest).map_err(|error| error.to_string())?;
        require(
            shape.is_object()
                && shape
                    .get("account_custody")
                    .is_none_or(serde_json::Value::is_object)
                && shape
                    .get("applications")
                    .is_none_or(serde_json::Value::is_object)
                && shape
                    .get("home_archive")
                    .is_none_or(serde_json::Value::is_object)
                && shape
                    .get("original_source")
                    .is_none_or(super::original_admission::ConsumerCapability::wire_shape)
                && shape
                    .get("boot_binding")
                    .is_some_and(serde_json::Value::is_object)
                && shape
                    .get("components")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|entries| entries.iter().all(serde_json::Value::is_object)),
            "manifest, component and boot-binding records must be JSON objects",
        )?;
    }
    let data: Manifest = serde_json::from_slice(manifest).map_err(|error| error.to_string())?;
    require(
        data.schema == SCHEMA && data.installer_protocol == 1,
        "unsupported schema or installer protocol",
    )?;
    if let Some(capability) = &data.account_custody {
        capability.validate().map_err(|error| error.to_string())?;
    }
    if let Some(capability) = &data.applications {
        capability.validate().map_err(|e| e.to_string())?;
    }
    if let Some(capability) = &data.home_archive {
        capability.validate().map_err(|error| error.to_string())?;
    }
    if let Some(capability) = &data.original_source {
        capability.validate().map_err(|error| error.to_string())?;
        require(
            capability.capture_pins.arch == data.architecture,
            "original-source proof architecture differs from selected image",
        )?;
    }
    require(data.purpose == PURPOSE, "wrong bundle purpose")?;
    require(identifier(&data.release_id), "invalid release identity")?;
    require(
        matches!(data.architecture.as_str(), "x86_64" | "aarch64"),
        "unsupported architecture",
    )?;
    require(
        data.firmware == "uefi" && data.secure_boot == "disabled",
        "unsupported firmware/security mode",
    )?;
    require(
        (1..=MAX_IMAGE_BYTES).contains(&data.source_image_bytes),
        "invalid image size",
    )?;
    require(
        data.components.len() == 2,
        "exactly two components required",
    )?;
    let mut roles = std::collections::HashSet::new();
    let mut filenames = std::collections::HashSet::new();
    for c in &data.components {
        require(
            matches!(c.role.as_str(), "staging-root" | "staging-esp") && roles.insert(&c.role),
            "unsupported or duplicate role",
        )?;
        require(
            identifier(&c.filename)
                && c.filename
                    .strip_suffix(".img")
                    .is_some_and(|s| !s.is_empty())
                && filenames.insert(&c.filename),
            "invalid or duplicate filename",
        )?;
        require(
            (1..=MAX_IMAGE_BYTES).contains(&c.byte_size)
                && (1..=MAX_IMAGE_BYTES).contains(&c.source_offset)
                && c.byte_size % 512 == 0
                && c.source_offset % 512 == 0
                && (c.byte_size..=MAX_IMAGE_BYTES).contains(&c.minimum_partition_bytes),
            "invalid component extent",
        )?;
        // The staging root image is the minimised filesystem; its partition
        // keeps the live root floor and the live installer grows into it.
        require(
            c.role != "staging-root" || c.minimum_partition_bytes >= LIVE_ROOT_FLOOR_BYTES,
            "staging root partition below the live root floor",
        )?;
        require(
            canonical_digest(&c.sha256, 64) && guid(&c.partition_type_guid),
            "invalid digest or partition type",
        )?;
        if c.role == "staging-esp" {
            let id = &c.filesystem_uuid;
            require(
                c.partition_type_guid == ESP_GUID
                    && c.filesystem_type == "vfat"
                    && id.len() == 9
                    && id != "0000-0000"
                    && id.bytes().enumerate().all(|(i, c)| {
                        if i == 4 {
                            c == b'-'
                        } else {
                            c.is_ascii_digit() || (b'A'..=b'F').contains(&c)
                        }
                    }),
                "unsupported ESP",
            )?;
        } else {
            // The Tilo staging root type, never the Tilo root type: an
            // installed UKI must keep seeing exactly one Tilo root beside it.
            require(
                c.partition_type_guid == STAGING_ROOT_TYPE
                    && c.filesystem_type == "btrfs"
                    && guid(&c.filesystem_uuid),
                "unsupported root",
            )?;
        }
    }
    let mut ranges: Vec<_> = data
        .components
        .iter()
        .map(|c| (c.source_offset, c.byte_size))
        .collect();
    ranges.sort_unstable();
    let mut end = 0;
    for (offset, size) in ranges {
        require(
            offset >= end && offset + size <= data.source_image_bytes,
            "overlapping or out-of-bounds extents",
        )?;
        end = offset + size;
    }
    let boot = &data.boot_binding;
    require(
        boot.uki_architecture == data.architecture
            && fallback_loader(&data.architecture) == Some(boot.efi_path.as_str())
            && canonical_digest(&boot.uki_sha256, 64)
            && canonical_digest(&boot.composefs_digest, 128),
        "invalid boot binding",
    )?;
    validate_cmdline(boot)?;
    Ok(InternalBundle { manifest: data })
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub(in crate::setup) struct WindowsDirectoryPath {
    pub(in crate::setup) root: [u16; 3],
    pub(in crate::setup) names: Vec<Vec<u16>>,
}

#[cfg(any(windows, test))]
pub(in crate::setup) fn windows_directory_leaf_supported(name: &[u16]) -> bool {
    if name.is_empty()
        || name.len() > 255
        || matches!(name.last(), Some(0x20 | 0x2e))
        || name
            .iter()
            .any(|&unit| unit <= 31 || b"<>:\"/\\|?*~".iter().any(|&c| unit == u16::from(c)))
    {
        return false;
    }
    // Fold only ASCII for reserved-device comparison. Retain the original
    // code units, including unpaired surrogates, for every actual open.
    let mut stem: Vec<_> = name
        .iter()
        .copied()
        .take_while(|&unit| unit != u16::from(b'.'))
        .map(|unit| match unit {
            0x61..=0x7a => unit - 0x20,
            _ => unit,
        })
        .collect();
    while stem.last() == Some(&u16::from(b' ')) {
        stem.pop();
    }
    !["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|reserved| stem.iter().copied().eq(reserved.encode_utf16()))
        && !(stem.len() == 4
            && ["COM", "LPT"]
                .iter()
                .any(|prefix| stem[..3].iter().copied().eq(prefix.encode_utf16()))
            // Include DOS digit aliases and Windows superscript ¹, ² and ³.
            && matches!(stem[3], 0x30..=0x39 | 0xb9 | 0xb2 | 0xb3))
}

#[cfg(any(windows, test))]
pub(in crate::setup) fn windows_directory_path(
    raw: impl IntoIterator<Item = u16>,
) -> Result<WindowsDirectoryPath, String> {
    // Validate raw units before Path::components can normalize aliases. One
    // extra unit detects overflow without collecting an unbounded input.
    let raw: Vec<_> = raw.into_iter().take(4097).collect();
    require(
        (3..=4096).contains(&raw.len())
            && matches!(raw[0], 0x41..=0x5a | 0x61..=0x7a)
            && raw[1..3] == [u16::from(b':'), u16::from(b'\\')],
        "require a bounded ordinary absolute Windows drive path",
    )?;
    let mut names = Vec::new();
    if raw.len() > 3 {
        for name in raw[3..].split(|&unit| unit == u16::from(b'\\')) {
            require(
                names.len() < 64 && windows_directory_leaf_supported(name),
                "unsupported Windows directory component",
            )?;
            names.push(name.to_vec());
        }
    }
    Ok(WindowsDirectoryPath {
        root: [raw[0], raw[1], raw[2]],
        names,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// The one fixture both validators accept. os/tests/internal_bundle_test.py
    /// derives its command line from the live image's kargs.d and
    /// Containerfile.installer, so this also proves the Rust allow-list
    /// accepts the real live command line. Its payloads are
    /// ordinary synthetic bytes, not filesystems or a bootable UKI.
    const SHARED_FIXTURE: &str =
        include_str!("../../../../os/tests/fixtures/internal-bundle-v2.json");
    const ROOT_UUID: &str = "12345678-1234-4234-8234-123456789abc";

    pub(crate) fn fixture() -> Value {
        serde_json::from_str(SHARED_FIXTURE).unwrap()
    }
    pub(crate) fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }
    fn cmdline() -> String {
        fixture()["boot_binding"]["uki_cmdline"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn with_cmdline(cmdline: &str) -> Result<InternalBundle, String> {
        let mut data = fixture();
        data["boot_binding"]["uki_cmdline"] = json!(cmdline);
        validate_internal_bundle(&bytes(&data))
    }
    /// Python-compatible fixture, live cmdline, root floor and closed cmdline profile.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn live_cmdline_profile_and_root_floor_are_closed_contracts() {
        // Phase `python_compatible_positive`.
        {
            assert!(validate_internal_bundle(SHARED_FIXTURE.as_bytes()).is_ok());
            assert!(validate_internal_bundle(&bytes(&fixture())).is_ok());
            let bundle = validate_internal_bundle(SHARED_FIXTURE.as_bytes()).unwrap();
            assert_eq!(bundle.purpose(), PURPOSE);
            assert_eq!(bundle.purpose(), "tilo-internal-installer");
        }

        // Phase `shared_fixture_is_the_live_cmdline`.
        {
            let text = cmdline();
            let tokens: Vec<_> = text.split_ascii_whitespace().collect();
            for token in [
                "root=tilo-auto",
                "rd.luks.options=tpm2-device=auto,fido2-device=auto",
                "tilo.install",
                "tilo.mode=normal",
                "tilo.generation=7",
                "preempt=full",
                "rw",
            ] {
                assert!(tokens.contains(&token), "fixture lacks {token}");
            }
            assert!(!text.contains("tilo.root="));
            assert!(!text.contains("root=UUID="));
            assert!(!text.contains("tilo.luks="));
            assert!(!text.contains("tilo.unlock="));
        }

        // Phase `minimised_root_fits_its_live_floor_partition`.
        {
            let data = fixture();
            let root = &data["components"][1];
            assert!(root["byte_size"].as_u64() < root["minimum_partition_bytes"].as_u64());
            let bundle = validate_internal_bundle(&bytes(&data)).unwrap();
            let view = bundle.component_by_role("staging-root").unwrap();
            assert_eq!(view.minimum_partition_bytes, LIVE_ROOT_FLOOR_BYTES);
            assert_eq!(view.byte_size, 8192);
            assert_eq!(view.partition_type_guid, STAGING_ROOT_TYPE);
            assert_eq!(view.filesystem_uuid, ROOT_UUID);
            let esp = bundle.component_by_role("staging-esp").unwrap();
            assert_eq!((esp.byte_size, esp.minimum_partition_bytes), (4096, 4096));
            assert_eq!(esp.filesystem_uuid, "1234-ABCD");
            assert!(bundle.component_by_role("target-root").is_none());
            assert_eq!(bundle.efi_path(), "/EFI/BOOT/BOOTX64.EFI");
            assert_eq!(bundle.uki_sha256(), "a".repeat(64));
            // A root that fills its partition exactly is still a valid bundle.
            let mut full = fixture();
            full["components"][1]["byte_size"] = json!(LIVE_ROOT_FLOOR_BYTES);
            full["source_image_bytes"] = json!(21504 + LIVE_ROOT_FLOOR_BYTES);
            assert!(validate_internal_bundle(&bytes(&full)).is_ok());
            full["components"][1]["byte_size"] = json!(LIVE_ROOT_FLOOR_BYTES + 512);
            full["source_image_bytes"] = json!(21504 + LIVE_ROOT_FLOOR_BYTES + 512);
            assert!(validate_internal_bundle(&bytes(&full)).is_err());
        }

        // Phase `cmdline_profile_is_closed`.
        {
            let base = cmdline();
            for tail in [
                " ro",
                " rw",
                " init=/bin/sh",
                " console=tty0",
                " console=tty1",
                " tilo.generation=12",
                " unknown",
                " loglevel=3",
                " systemd.show_status=auto",
                " rd.udev.log_level=3",
                " vt.global_cursor_default=0",
                " root=tilo-auto",
                " rd.luks.options=tpm2-device=auto,fido2-device=auto",
                " tilo.luks=000000000000000000000000000000000000000000000",
                // Left out of the live hardening set (01-hardening.toml).
                " init_on_free=1",
            ] {
                assert!(
                    with_cmdline(&format!("{base}{tail}")).is_err(),
                    "accepted {tail}"
                );
            }
            for (old, new) in [
                ("tilo.mode=normal", "tilo.mode=repair"),
                ("preempt=full", "preempt=none"),
                ("tilo.generation=7", "tilo.generation=0"),
                ("tilo.generation=7", "tilo.generation=07"),
                ("tilo.generation=7", "tilo.generation=12345678901"),
                ("root=tilo-auto", "root=tilo-auto2"),
                ("fido2-device=auto", "fido2-device=none"),
                (" rw ", " ro "),
            ] {
                assert!(base.contains(old), "fixture lacks {old}");
                assert!(
                    with_cmdline(&base.replacen(old, new, 1)).is_err(),
                    "accepted {new}"
                );
            }
            for accepted in [
                base.clone(),
                base.replacen("tilo.generation=7", "tilo.generation=4294967295", 1),
                base.replacen(" tilo.generation=7", "", 1),
                base.replacen(
                    " console=tty0 console=ttyAMA0,115200 console=ttyS0,115200",
                    "",
                    1,
                ),
            ] {
                assert!(with_cmdline(&accepted).is_ok(), "rejected {accepted}");
            }
            for required in ["tilo.install", "rw", "root=tilo-auto"] {
                let removed = base
                    .split_ascii_whitespace()
                    .filter(|token| *token != required)
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(
                    with_cmdline(&removed).is_err(),
                    "accepted without {required}"
                );
            }
        }

        // Phase `device_bound_cmdlines_are_rejected`.
        {
            let base = cmdline();
            let auto = "root=tilo-auto";
            for cmdline in [
                base.replacen(auto, &format!("root=UUID={ROOT_UUID}"), 1),
                base.replacen(auto, &format!("root=PARTUUID={ROOT_UUID}"), 1),
                base.replacen(auto, &format!("tilo.root={}", "0".repeat(37)), 1),
            ] {
                assert!(with_cmdline(&cmdline).is_err(), "accepted {cmdline}");
            }
        }
    }
    /// Schema corruption, ambiguous JSON, architectures and object representation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn bundle_schema_rejects_corrupt_ambiguous_or_positional_records() {
        // Phase `rejects_single_field_schema_corruptions`.
        {
            let cases = [
                ("/schema", json!(1)),
                ("/schema", json!(3)),
                ("/purpose", json!("removable-media")),
                ("/firmware", json!("bios")),
                ("/secure_boot", json!("enabled")),
                ("/installer_protocol", json!(2)),
                ("/architecture", json!("arm")),
                ("/release_id", json!("../escape")),
                ("/source_image_bytes", json!(1_u64 << 41)),
                ("/components/1/role", json!("staging-esp")),
                ("/components/1/filename", json!("staging-esp.img")),
                ("/components/0/filename", json!("../escape.img")),
                ("/components/0/sha256", json!("A".repeat(64))),
                ("/components/0/byte_size", json!(4095)),
                ("/components/0/source_offset", json!(0)),
                ("/components/1/source_offset", json!(17408)),
                ("/components/1/minimum_partition_bytes", json!(4096)),
                (
                    "/components/1/minimum_partition_bytes",
                    json!(LIVE_ROOT_FLOOR_BYTES - 512),
                ),
                ("/components/0/minimum_partition_bytes", json!(2048)),
                (
                    "/components/1/filesystem_uuid",
                    json!("00000000-0000-0000-0000-000000000000"),
                ),
                ("/components/0/filesystem_uuid", json!("1234-abcd")),
                ("/components/0/filesystem_type", json!("btrfs")),
                (
                    "/components/1/partition_type_guid",
                    json!("b921b045-1df0-41c3-af44-4c6f280d3fae"),
                ),
                (
                    "/components/1/partition_type_guid",
                    json!("4f68bce3-e8cd-4db1-96e7-fbcaf984b709"),
                ),
                (
                    "/components/1/partition_type_guid",
                    json!("7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab"),
                ),
                (
                    "/components/1/partition_type_guid",
                    json!("0fc63daf-8483-4772-8e79-3d69d8477de4"),
                ),
                ("/boot_binding/uki_architecture", json!("aarch64")),
                ("/boot_binding/efi_path", json!("/EFI/../evil.efi")),
                ("/boot_binding/efi_path", json!("/EFI/Linux/fixture.efi")),
                ("/boot_binding/efi_path", json!("/EFI/BOOT/BOOTAA64.EFI")),
                ("/boot_binding/efi_path", json!("/EFI/BOOT/bootx64.efi")),
                ("/boot_binding/composefs_digest", json!("c".repeat(128))),
                ("/boot_binding/uki_sha256", json!("A".repeat(64))),
            ];
            for (path, value) in cases {
                let mut data = fixture();
                *data.pointer_mut(path).unwrap() = value.clone();
                assert!(
                    validate_internal_bundle(&bytes(&data)).is_err(),
                    "accepted {path} = {value}"
                );
            }
        }

        // Phase `rejects_ambiguous_or_unbounded_json`.
        {
            let raw = String::from_utf8(bytes(&fixture())).unwrap();
            assert!(raw.contains("\"schema\":2"));
            assert!(
                validate_internal_bundle(
                    raw.replacen("\"schema\":2", "\"schema\":2,\"schema\":2", 1)
                        .as_bytes()
                )
                .is_err()
            );
            for value in [json!(true), json!(2.0), json!("2"), json!(-2)] {
                let mut data = fixture();
                data["schema"] = value;
                assert!(validate_internal_bundle(&bytes(&data)).is_err());
            }
            let mut data = fixture();
            data["components"][0]["source_attributes"] = json!(0);
            assert!(validate_internal_bundle(&bytes(&data)).is_err());
            // The device-bound root locator of earlier exports is not a field.
            let mut data = fixture();
            data["boot_binding"]["root_locator"] = json!(format!("UUID={ROOT_UUID}"));
            assert!(validate_internal_bundle(&bytes(&data)).is_err());
            let mut raw = bytes(&fixture());
            raw.resize(MAX_MANIFEST_BYTES + 1, b' ');
            assert!(validate_internal_bundle(&raw).is_err());
        }

        // Phase `accepts_both_architectures_and_rejects_nested_unknown_or_duplicate_keys`.
        {
            let mut value = fixture();
            value["architecture"] = json!("aarch64");
            value["boot_binding"]["uki_architecture"] = json!("aarch64");
            // Each architecture boots through its own fallback loader path.
            assert!(validate_internal_bundle(&bytes(&value)).is_err());
            value["boot_binding"]["efi_path"] = json!("/EFI/BOOT/BOOTAA64.EFI");
            assert!(validate_internal_bundle(&bytes(&value)).is_ok());
            assert_eq!(fallback_loader("aarch64"), Some("/EFI/BOOT/BOOTAA64.EFI"));
            assert_eq!(fallback_loader("riscv64"), None);
            for path in ["/boot_binding", "/components/0"] {
                let mut value = fixture();
                value.pointer_mut(path).unwrap()["unknown"] = json!(true);
                assert!(validate_internal_bundle(&bytes(&value)).is_err());
            }
            let raw = String::from_utf8(bytes(&fixture())).unwrap();
            for field in ["filename", "uki_architecture"] {
                let marker = format!("\"{field}\":");
                let changed = raw.replacen(&marker, &format!("\"{field}\":\"bad\",{marker}"), 1);
                assert!(validate_internal_bundle(changed.as_bytes()).is_err());
            }
        }

        // Phase `representation_manifest_must_be_an_object`.
        {
            let value = fixture();
            assert!(validate_internal_bundle(&bytes(&value)).is_ok());
            let array = positional_record(
                &value,
                "schema purpose release_id architecture firmware secure_boot installer_protocol source_image_bytes components boot_binding",
            );
            assert!(validate_internal_bundle(&bytes(&array)).is_err());
        }

        // Phase `representation_component_must_be_an_object`.
        {
            let mut value = fixture();
            assert!(validate_internal_bundle(&bytes(&value)).is_ok());
            value["components"][0] = positional_record(
                &value["components"][0],
                "role filename byte_size sha256 partition_type_guid filesystem_type filesystem_uuid minimum_partition_bytes source_offset",
            );
            assert!(validate_internal_bundle(&bytes(&value)).is_err());
        }

        // Phase `representation_boot_binding_must_be_an_object`.
        {
            let mut value = fixture();
            assert!(validate_internal_bundle(&bytes(&value)).is_ok());
            value["boot_binding"] = positional_record(
                &value["boot_binding"],
                "efi_path uki_architecture uki_sha256 uki_cmdline composefs_digest",
            );
            assert!(validate_internal_bundle(&bytes(&value)).is_err());
        }
    }
    fn windows_path(text: &str) -> Result<WindowsDirectoryPath, String> {
        windows_directory_path(text.encode_utf16())
    }

    /// Windows Unicode ancestor names, bounds and bridges.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_unicode_ancestors_are_exact_bounded_and_lossless() {
        // Phase `windows_unicode_ancestors_retain_exact_names`.
        {
            for name in [
                "André",
                "用户",
                "e\u{301}",
                "cache-😀",
                "O'Neil",
                "$cache",
                "CONsole",
                "COM10",
                "COM¹safe",
            ] {
                let parsed = windows_path(&format!("C:\\Users\\{name}\\cache")).unwrap();
                assert_eq!(parsed.root, [b'C' as u16, b':' as u16, b'\\' as u16]);
                assert_eq!(parsed.names[1], name.encode_utf16().collect::<Vec<_>>());
            }
            assert!(windows_path("c:\\").unwrap().names.is_empty());
            let composed = windows_path("C:\\é").unwrap();
            let decomposed = windows_path("C:\\e\u{301}").unwrap();
            assert_eq!(composed.names, vec![vec![0xe9]]);
            assert_eq!(decomposed.names, vec![vec![b'e' as u16, 0x301]]);
            assert_ne!(composed, decomposed);
        }

        // Phase `windows_unicode_ancestor_bounds_count_utf16_units`.
        {
            let leaf = format!("{}a", "😀".repeat(127));
            assert_eq!(leaf.encode_utf16().count(), 255);
            assert!(windows_path(&format!("C:\\{leaf}")).is_ok());
            assert!(windows_path(&format!("C:\\{leaf}a")).is_err());
            let mut names = vec!["é".repeat(255); 15];
            names.push("é".repeat(253));
            let path = format!("C:\\{}", names.join("\\"));
            assert_eq!(path.encode_utf16().count(), 4096);
            assert!(windows_path(&path).is_ok());
            // The last leaf remains below 255; only the total path limit changes.
            assert!(windows_path(&(path + "a")).is_err());
            assert!(windows_path(&format!("C:\\{}", vec!["é"; 64].join("\\"))).is_ok());
            assert!(windows_path(&format!("C:\\{}", vec!["é"; 65].join("\\"))).is_err());
        }

        // Phase `windows_unicode_raw_collection_is_bounded`.
        {
            let mut consumed = 0;
            let raw = std::iter::repeat(b'a' as u16).inspect(|_| consumed += 1);
            assert!(windows_directory_path(raw).is_err());
            assert_eq!(consumed, 4097);
        }

        // Phase `windows_unicode_unpaired_surrogates_are_preserved`.
        {
            let name = vec![0xd800, b'a' as u16, 0xdc00, 0xd83d, 0xde00];
            let mut raw: Vec<_> = "C:\\".encode_utf16().collect();
            raw.extend_from_slice(&name);
            assert_eq!(windows_directory_path(raw).unwrap().names, vec![name]);
        }

        // Phase `windows_unicode_os_string_bridge_is_lossless`.
        #[cfg(windows)]
        {
            use std::{
                ffi::OsString,
                os::windows::ffi::{OsStrExt, OsStringExt},
            };
            let mut raw: Vec<_> = "C:\\用户\\".encode_utf16().collect();
            raw.extend([0xd800, b'a' as u16, 0xdc00, 0xd83d, 0xde00]);
            let path = std::path::PathBuf::from(OsString::from_wide(&raw));
            assert_eq!(path.as_os_str().encode_wide().collect::<Vec<_>>(), raw);
            let parsed = windows_directory_path(path.as_os_str().encode_wide()).unwrap();
            let mut rebuilt: Vec<_> = OsString::from_wide(&parsed.root).encode_wide().collect();
            for (index, name) in parsed.names.iter().enumerate() {
                if index != 0 {
                    rebuilt.push(b'\\' as u16);
                }
                rebuilt.extend(OsString::from_wide(name).encode_wide());
            }
            assert_eq!(rebuilt, raw);
        }
    }

    /// Namespaces, separators, aliases, devices and streams.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_names_cannot_alias_escape_or_broaden_authenticated_files() {
        // Phase `windows_unicode_raw_namespaces_and_separators_are_refused`.
        {
            assert!(windows_path("C:\\Users\\ordinary").is_ok());
            for path in [
                "",
                "C:",
                "C:ordinary",
                "\\ordinary",
                "ordinary",
                "1:\\ordinary",
                "\\\\server\\share",
                "\\\\?\\C:\\ordinary",
                "\\\\.\\C:\\ordinary",
                "\\??\\C:\\ordinary",
                "\\\\?\\GLOBALROOT\\Device\\HarddiskVolume1",
                "C:/ordinary",
                "C:\\Users/ordinary",
                "C:\\Users\\",
                "C:\\Users\\\\ordinary",
                "C:\\Users\\.\\ordinary",
                "C:\\Users\\..\\ordinary",
            ] {
                assert!(windows_path(path).is_err(), "accepted {path:?}");
            }
        }

        // Phase `windows_unicode_leaf_aliases_and_control_units_are_refused`.
        {
            assert!(windows_path("C:\\Users\\ordinary").is_ok());
            for name in [
                ".",
                "..",
                "file.",
                "file ",
                "CON",
                "prn.txt",
                "aux",
                "nul.cache",
                "CONIN$",
                "conout$.txt",
                "CoN .txt",
                "COM0",
                "com1.ext",
                "COM9",
                "LPT0",
                "lpt1.ext",
                "LPT9",
                "COM¹",
                "com².ext",
                "CoM³",
                "LPT¹.ext",
                "lpt²",
                "LPT³",
                "PROGRA~1",
                "file:stream",
            ] {
                assert!(
                    windows_path(&format!("C:\\Users\\{name}")).is_err(),
                    "accepted {name:?}"
                );
            }
            for unit in (0_u16..=31).chain("<>:\"/\\|?*~".encode_utf16()) {
                let mut raw: Vec<_> = "C:\\Users\\".encode_utf16().collect();
                raw.push(unit);
                assert!(windows_directory_path(raw).is_err(), "accepted {unit:x}");
            }
        }

        // Phase `windows_unicode_ancestors_do_not_broaden_authenticated_filenames`.
        {
            assert!(validate_internal_bundle(&bytes(&fixture())).is_ok());
            for name in ["é.img", "用户.img", "e\u{301}.img", "😀.img"] {
                let mut manifest = fixture();
                manifest["components"][0]["filename"] = json!(name);
                assert!(validate_internal_bundle(&bytes(&manifest)).is_err());
            }
        }
    }

    fn positional_record(value: &Value, fields: &str) -> Value {
        Value::Array(
            fields
                .split_whitespace()
                .map(|key| value[key].clone())
                .collect(),
        )
    }
    #[test]
    fn selected_bundle_account_admission_keeps_legacy_fresh_setup_and_refuses_missing_proof() {
        let mut data: Value = serde_json::from_str(SHARED_FIXTURE).unwrap();
        let legacy = validate_internal_bundle(&bytes(&data)).unwrap();
        assert!(legacy.require_account_protocol(None).is_ok());
        assert!(legacy.require_account_protocol(Some(1)).is_err());
        let capability = json!({"schema":1,"protected_grant_protocol":1,
            "credential_helper_sha256":"a".repeat(64),"runtime_root_hash":"b".repeat(64),
            "runtime_probe_sha256":"c".repeat(64),"runtime_library_sha256":"d".repeat(64),
            "runtime_version":"0.1.0+native.abc","runtime_library":"lib/libruntime_dylib.so"});
        data["account_custody"] = capability.clone();
        let updated = validate_internal_bundle(&bytes(&data)).unwrap();
        assert!(updated.require_account_protocol(Some(1)).is_ok());
        assert!(updated.require_service_session_protocol(None).is_ok());
        assert!(updated.require_service_session_protocol(Some(1)).is_err());
        assert!(updated.require_account_protocol(Some(2)).is_err());
        assert!(updated.require_account_protocol(None).is_ok());
        assert_eq!(updated.uki_sha256(), legacy.uki_sha256());
        for bad in [json!(null), json!([]), json!(true)] {
            data["account_custody"] = bad;
            assert!(validate_internal_bundle(&bytes(&data)).is_err());
        }
        data["account_custody"] = capability;
        data["account_custody"]["protected_grant_protocol"] = json!(2);
        assert!(validate_internal_bundle(&bytes(&data)).is_err());
    }
    #[test]
    fn selected_application_data_requires_distinct_authenticated_target_proof() {
        let mut data: Value = serde_json::from_str(SHARED_FIXTURE).unwrap();
        let legacy = validate_internal_bundle(&bytes(&data)).unwrap();
        assert!(legacy.require_applications_protocol(None).is_ok());
        assert!(legacy.require_applications_protocol(Some(2)).is_err());
        data["home_archive"] = json!({"schema":1,"archive_protocol":2,"seed_protocol":1,
            "firstboot_user_sha256":"c".repeat(64),"seed_helper_sha256":"d".repeat(64),
            "prepared_helper_sha256":"a".repeat(64),"tar_sha256":"b".repeat(64)});
        assert!(
            validate_internal_bundle(&bytes(&data))
                .unwrap()
                .require_applications_protocol(Some(2))
                .is_err()
        );
        let proof = json!({"schema":1,"subset_protocol":2,"prepared_helper_sha256":"a".repeat(64),
            "qualification_script_sha256":"b".repeat(64),"native_qualification_sha256":"c".repeat(64)});
        data["applications"] = proof.clone();
        let selected = validate_internal_bundle(&bytes(&data)).unwrap();
        assert!(selected.require_applications_protocol(Some(2)).is_ok());
        assert!(selected.require_applications_protocol(Some(1)).is_err());
        assert_eq!(selected.uki_sha256(), legacy.uki_sha256());
        for malformed in [json!(null), json!([]), json!(true)] {
            data["applications"] = malformed;
            assert!(validate_internal_bundle(&bytes(&data)).is_err());
        }
        for field in [
            "prepared_helper_sha256",
            "qualification_script_sha256",
            "native_qualification_sha256",
        ] {
            data["applications"] = proof.clone();
            data["applications"][field] = json!("x".repeat(64));
            assert!(validate_internal_bundle(&bytes(&data)).is_err());
        }
    }
    #[test]
    fn selected_bundle_home_admission_preserves_fresh_setup_and_requires_target_proof() {
        let mut data: Value = serde_json::from_str(SHARED_FIXTURE).unwrap();
        let legacy = validate_internal_bundle(&bytes(&data)).unwrap();
        assert!(legacy.require_home_protocol(None).is_ok());
        assert!(legacy.require_home_protocol(Some(2)).is_err());
        let proof = json!({"schema":1,"archive_protocol":2,"seed_protocol":1,
            "firstboot_user_sha256":"c".repeat(64),"seed_helper_sha256":"d".repeat(64),
            "prepared_helper_sha256":"a".repeat(64),"tar_sha256":"b".repeat(64)});
        data["home_archive"] = proof.clone();
        let updated = validate_internal_bundle(&bytes(&data)).unwrap();
        assert!(updated.require_home_protocol(Some(2)).is_ok());
        assert!(updated.require_home_protocol(Some(1)).is_err());
        assert_eq!(updated.uki_sha256(), legacy.uki_sha256());
        for bad in [
            json!(null),
            json!([]),
            json!(true),
            json!([
                1,
                2,
                1,
                "c".repeat(64),
                "d".repeat(64),
                "a".repeat(64),
                "b".repeat(64)
            ]),
        ] {
            data["home_archive"] = bad;
            assert!(validate_internal_bundle(&bytes(&data)).is_err());
        }
        for key in ["firstboot_user_sha256", "seed_helper_sha256"] {
            data["home_archive"] = proof.clone();
            data["home_archive"][key] = json!("X".repeat(64));
            assert!(validate_internal_bundle(&bytes(&data)).is_err());
        }
        data["home_archive"] = proof.clone();
        data["home_archive"]["seed_protocol"] = json!(2);
        assert!(validate_internal_bundle(&bytes(&data)).is_err());
        data["home_archive"] = proof;
        data["home_archive"]["archive_protocol"] = json!(1);
        assert!(validate_internal_bundle(&bytes(&data)).is_err());
    }
    #[test]
    fn selected_bundle_original_syntax_preserves_legacy_and_requires_matching_architecture() {
        let mut data: Value = serde_json::from_str(SHARED_FIXTURE).unwrap();
        let legacy = validate_internal_bundle(&bytes(&data)).unwrap();
        assert!(legacy.original_source().is_none());
        assert!(legacy.require_original_protocol(None).is_ok());
        assert!(legacy.require_original_protocol(Some(5)).is_err());
        let mut proof = super::super::original_admission::syntax_fixture();
        proof.capture_pins.arch = data["architecture"].as_str().unwrap().into();
        data["original_source"] = serde_json::to_value(&proof).unwrap();
        let selected = validate_internal_bundle(&bytes(&data)).unwrap();
        assert_eq!(selected.original_source(), Some(&proof));
        assert_eq!(selected.uki_sha256(), legacy.uki_sha256());
        for bad in [json!(null), json!([]), json!(true)] {
            let mut changed = data.clone();
            changed["original_source"] = bad;
            assert!(validate_internal_bundle(&bytes(&changed)).is_err());
        }
        data["original_source"]["capture_pins"]["arch"] =
            json!(if proof.capture_pins.arch == "x86_64" {
                "aarch64"
            } else {
                "x86_64"
            });
        assert!(validate_internal_bundle(&bytes(&data)).is_err());
    }
}
