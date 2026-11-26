//! Validating the internal installer's manifest, and the views over what it bound.

use super::staging::STAGING_ROOT_TYPE;
use serde::Deserialize;

pub const MAX_MANIFEST_BYTES: usize = 65_536;
const MAX_IMAGE_BYTES: u64 = 1 << 40;
const ESP_GUID: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
pub const SCHEMA: u32 = 2;
pub const PURPOSE: &str = "tilo-internal-installer";
pub const LIVE_ROOT_FLOOR_BYTES: u64 = 40 << 30;
pub const ROOT_LOCATOR: &str = "tilo-auto";
const LIVE_FLAGS: [&str; 6] = [
    "rw",
    "quiet",
    "splash",
    "slab_nomerge",
    "plymouth.ignore-serial-consoles",
    "tilo.install",
];

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
    pub minimum_partition_bytes: u64,
    pub partition_type_guid: &'a str,
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

    pub fn component_by_role(&self, role: &str) -> Option<ComponentView<'_>> {
        let index = self
            .manifest
            .components
            .iter()
            .position(|c| c.role == role)?;
        self.component(index)
    }

    pub fn efi_path(&self) -> &str {
        &self.manifest.boot_binding.efi_path
    }
    pub fn uki_sha256(&self) -> &str {
        &self.manifest.boot_binding.uki_sha256
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

pub fn validate_internal_bundle(manifest: &[u8]) -> Result<InternalBundle, String> {
    require(
        !manifest.is_empty() && manifest.len() <= MAX_MANIFEST_BYTES,
        "manifest exceeds size limit",
    )?;
    {
        let shape: serde_json::Value =
            serde_json::from_slice(manifest).map_err(|error| error.to_string())?;
        require(
            shape.is_object()
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
            && matches!(stem[3], 0x30..=0x39 | 0xb9 | 0xb2 | 0xb3))
}

#[cfg(any(windows, test))]
pub(in crate::setup) fn windows_directory_path(
    raw: impl IntoIterator<Item = u16>,
) -> Result<WindowsDirectoryPath, String> {
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
    use serde_json::Value;

    const SHARED_FIXTURE: &str =
        include_str!("../../../../os/tests/fixtures/internal-bundle-v2.json");
    pub(crate) fn fixture() -> Value {
        serde_json::from_str(SHARED_FIXTURE).unwrap()
    }
    pub(crate) fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }
    fn windows_path(text: &str) -> Result<WindowsDirectoryPath, String> {
        windows_directory_path(text.encode_utf16())
    }

    #[test]
    fn windows_unicode_ancestors_are_exact_bounded_and_lossless() {
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
            assert!(windows_path(&(path + "a")).is_err());
            assert!(windows_path(&format!("C:\\{}", vec!["é"; 64].join("\\"))).is_ok());
            assert!(windows_path(&format!("C:\\{}", vec!["é"; 65].join("\\"))).is_err());
        }

        {
            let mut consumed = 0;
            let raw = std::iter::repeat(b'a' as u16).inspect(|_| consumed += 1);
            assert!(windows_directory_path(raw).is_err());
            assert_eq!(consumed, 4097);
        }

        {
            let name = vec![0xd800, b'a' as u16, 0xdc00, 0xd83d, 0xde00];
            let mut raw: Vec<_> = "C:\\".encode_utf16().collect();
            raw.extend_from_slice(&name);
            assert_eq!(windows_directory_path(raw).unwrap().names, vec![name]);
        }

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
}
