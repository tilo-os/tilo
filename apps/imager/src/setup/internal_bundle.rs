//! Bounded internal-installer data and read-only byte verification.
//! Consistency is not authenticated provenance, filesystem qualification, or boot proof.
//!
//! Schema 2 is the contract of `os/dev/internal-bundle.py`, which exports it;
//! both sides must accept exactly the same manifests. Both test suites
//! validate the shared fixture `os/tests/fixtures/internal-bundle-v2.json`,
//! and the Python suite derives that fixture's command line from the live
//! installer's real kargs (design spec 2026-09-22-usb-free-install §3).

use super::staging::STAGING_ROOT_TYPE;
use serde::Deserialize;
use std::{path::Path, sync::atomic::AtomicBool};

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
                "init_on_alloc" | "init_on_free" | "page_alloc.shuffle" => value == "1",
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

/// Verify ordinary component files for this invocation only. Future staging must
/// reopen pinned handles and reverify; success here is not retained byte authority.
/// # Errors
/// Rejects unsupported hosts, unsafe files, replacements, cancellation or mismatched bytes.
pub fn verify_internal_components(
    bundle: &InternalBundle,
    directory: &Path,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        unix::verify(bundle, directory, cancelled)
    }
    #[cfg(windows)]
    {
        windows::verify(bundle, directory, cancelled)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (bundle, directory, cancelled);
        Err("Internal bundle file verification is unsupported on this platform.".into())
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    require(
        !cancel.load(std::sync::atomic::Ordering::Acquire),
        "Internal bundle verification cancelled.",
    )
}

// Cancellation is cooperative between blocking calls; this does not promise
// bounded latency from an unresponsive filesystem, filter driver or oplock.
fn verify_bytes(
    file: &mut std::fs::File,
    component: &Component,
    cancel: &AtomicBool,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut remaining = component.byte_size;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        check_cancel(cancel)?;
        let size = remaining.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..size]).map_err(|e| e.to_string())?;
        check_cancel(cancel)?;
        require(count > 0, "truncated component")?;
        hash.update(&buffer[..count]);
        remaining -= count as u64;
        #[cfg(test)]
        tests::event("read", &component.filename, component.byte_size - remaining);
    }
    check_cancel(cancel)?;
    require(
        file.read(&mut buffer[..1]).map_err(|e| e.to_string())? == 0,
        "long component",
    )?;
    #[cfg(test)]
    tests::event("eof", &component.filename, component.byte_size);
    check_cancel(cancel)?;
    require(
        hex::encode(hash.finalize()) == component.sha256,
        "component digest mismatch",
    )
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        fs::{self, File, Metadata, OpenOptions},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{MetadataExt, OpenOptionsExt},
        },
    };

    fn fingerprint(meta: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64, u64, u32) {
        (
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
            meta.nlink(),
            meta.mode(),
        )
    }
    fn open(dir: &File, name: &str, cancel: &AtomicBool) -> Result<File, String> {
        check_cancel(cancel)?;
        let name = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
        // SAFETY: the directory descriptor and NUL-terminated leaf name are live.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned a fresh owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        check_cancel(cancel)?;
        Ok(file)
    }
    pub(super) fn verify(
        bundle: &InternalBundle,
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        check_cancel(cancel)?;
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| e.to_string())?;
        let dir_meta = dir.metadata().map_err(|e| e.to_string())?;
        let mut pinned = Vec::new();
        let mut identities = std::collections::HashSet::new();
        for component in &bundle.manifest.components {
            check_cancel(cancel)?;
            let mut file = open(&dir, &component.filename, cancel)?;
            let before = file.metadata().map_err(|e| e.to_string())?;
            require(
                before.is_file() && before.nlink() == 1 && before.len() == component.byte_size,
                "invalid component type, links or length",
            )?;
            require(
                identities.insert((before.dev(), before.ino())),
                "component names alias the same file",
            )?;
            #[cfg(test)]
            super::tests::event("opened", &component.filename, 0);
            verify_bytes(&mut file, component, cancel)?;
            pinned.push((component, file, before));
        }
        // Retain all handles until every by-name check finishes, including earlier
        // components that might have changed while a later component was hashed.
        for (component, file, before) in &pinned {
            check_cancel(cancel)?;
            let after = file.metadata().map_err(|e| e.to_string())?;
            check_cancel(cancel)?;
            let named = open(&dir, &component.filename, cancel)?
                .metadata()
                .map_err(|e| e.to_string())?;
            #[cfg(test)]
            super::tests::event("readback", &component.filename, component.byte_size);
            check_cancel(cancel)?;
            require(
                fingerprint(before) == fingerprint(&after)
                    && fingerprint(&after) == fingerprint(&named),
                "component changed during verification",
            )?;
        }
        check_cancel(cancel)?;
        let named_dir = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        require(
            named_dir.is_dir()
                && dir_meta.dev() == named_dir.dev()
                && dir_meta.ino() == named_dir.ino(),
            "bundle directory replaced",
        )?;
        check_cancel(cancel)
    }
}

#[cfg(any(windows, test))]
fn windows_leaf_supported(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name.ends_with(['.', ' '])
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b" ._-".contains(&c))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct WindowsDirectoryPath {
    root: [u16; 3],
    names: Vec<Vec<u16>>,
}

#[cfg(any(windows, test))]
fn windows_directory_leaf_supported(name: &[u16]) -> bool {
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
fn windows_directory_path(
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

#[cfg(any(windows, test))]
fn windows_volume_root(value: &str) -> bool {
    value
        .strip_prefix(r"\\?\Volume{")
        .and_then(|v| v.strip_suffix(r"}\"))
        .is_some_and(|id| guid(&id.to_ascii_lowercase()))
}

/// Windows is deliberately restricted to ordinary absolute drive-letter paths
/// with lossless UTF-16 directory names and ASCII payload names. UNC,
/// verbatim/device namespaces, relative paths,
/// alternate streams, DOS aliases and every reparse tag fail closed.
#[cfg(windows)]
mod windows {
    use super::*;
    use cap_fs_ext::OpenOptionsFollowExt;
    use cap_primitives::fs::FollowSymlinks;
    use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
    use std::{
        ffi::{OsStr, OsString},
        fs::File,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::OpenOptionsExt as StdOpenOptionsExt,
            io::{AsRawHandle, RawHandle},
        },
        path::PathBuf,
    };
    use windows_sys::Win32::Storage::FileSystem::*;

    #[derive(Debug, PartialEq, Eq)]
    struct Snapshot {
        volume: u64,
        id: [u8; 16],
        length: i64,
        links: u32,
        attributes: u32,
        tag: u32,
        created: i64,
        written: i64,
        changed: i64,
    }

    fn snapshot(
        handle: RawHandle,
        directory: bool,
        cancel: &AtomicBool,
    ) -> Result<Snapshot, String> {
        check_cancel(cancel)?;
        // SAFETY: caller retains the owning file/directory throughout these
        // synchronous queries. The raw handle is borrowed, never re-owned.
        require(
            unsafe { GetFileType(handle) } == FILE_TYPE_DISK,
            "unsupported Windows file type",
        )?;
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        let mut basic = FILE_BASIC_INFO::default();
        let mut standard = FILE_STANDARD_INFO::default();
        let mut id = FILE_ID_INFO::default();
        macro_rules! query {
            ($class:expr,$value:expr) => {{
                check_cancel(cancel)?;
                // SAFETY: class and corresponding initialized output type are
                // paired below; buffer is writable and has the exact type size.
                let success = unsafe {
                    GetFileInformationByHandleEx(
                        handle,
                        $class,
                        std::ptr::from_mut(&mut $value).cast(),
                        std::mem::size_of_val(&$value) as u32,
                    )
                };
                check_cancel(cancel)?;
                if success == 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }};
        }
        query!(FileAttributeTagInfo, tag);
        query!(FileBasicInfo, basic);
        query!(FileStandardInfo, standard);
        query!(FileIdInfo, id);
        require(
            tag.FileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE) == 0
                && tag.ReparseTag == 0
                && basic.FileAttributes == tag.FileAttributes
                && standard.Directory == directory
                && !standard.DeletePending
                && (directory || standard.NumberOfLinks == 1)
                && standard.EndOfFile >= 0
                && id.FileId.Identifier != [0; 16],
            "ambiguous Windows file metadata",
        )?;
        check_cancel(cancel)?;
        Ok(Snapshot {
            volume: id.VolumeSerialNumber,
            id: id.FileId.Identifier,
            length: standard.EndOfFile,
            links: standard.NumberOfLinks,
            attributes: tag.FileAttributes,
            tag: tag.ReparseTag,
            created: basic.CreationTime,
            written: basic.LastWriteTime,
            changed: basic.ChangeTime,
        })
    }

    fn open_leaf(
        dir: &Dir,
        name: &OsStr,
        directory: bool,
        cancel: &AtomicBool,
    ) -> Result<File, String> {
        check_cancel(cancel)?;
        require(
            if directory {
                windows_directory_leaf_supported(&name.encode_wide().take(256).collect::<Vec<_>>())
            } else {
                name.to_str().is_some_and(windows_leaf_supported)
            },
            "unsupported Windows leaf name",
        )?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .follow(FollowSymlinks::No)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(
                FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
            );
        let file = dir
            .open_with(name, &options)
            .map_err(|e| e.to_string())?
            .into_std();
        snapshot(file.as_raw_handle(), directory, cancel)?;
        Ok(file)
    }

    struct DirectoryChain {
        root_path: PathBuf,
        // Each handle remains live, preventing compatible delete/write opens.
        entries: Vec<(OsString, Dir, Snapshot)>,
    }
    fn open_drive(root: &Path, cancel: &AtomicBool) -> Result<File, String> {
        check_cancel(cancel)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(root)
            .map_err(|e| e.to_string())?;
        snapshot(file.as_raw_handle(), true, cancel)?;
        // Require an actual volume root, not a SUBST/mapped subdirectory whose
        // hidden ancestors would escape the explicit no-reparse walk.
        let mut final_path = [0_u16; 128];
        check_cancel(cancel)?;
        // SAFETY: the live borrowed handle and fixed writable UTF-16 buffer are
        // valid for this synchronous query; no returned pathname is reopened.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                final_path.as_mut_ptr(),
                final_path.len() as u32,
                VOLUME_NAME_GUID | FILE_NAME_NORMALIZED,
            )
        } as usize;
        check_cancel(cancel)?;
        require(
            length > 0 && length < final_path.len(),
            "Windows volume identity is unavailable",
        )?;
        let final_path = String::from_utf16(&final_path[..length]).map_err(|e| e.to_string())?;
        require(
            windows_volume_root(&final_path),
            "Windows drive maps outside an ordinary volume root",
        )?;
        Ok(file)
    }
    impl DirectoryChain {
        fn acquire(path: &Path, cancel: &AtomicBool) -> Result<Self, String> {
            check_cancel(cancel)?;
            let parsed = windows_directory_path(path.as_os_str().encode_wide())?;
            let root_path = PathBuf::from(OsString::from_wide(&parsed.root));
            let root = open_drive(&root_path, cancel)?;
            let root_snapshot = snapshot(root.as_raw_handle(), true, cancel)?;
            let mut chain = Self {
                root_path,
                entries: vec![(OsString::new(), Dir::from_std_file(root), root_snapshot)],
            };
            for raw_name in parsed.names {
                let name = OsString::from_wide(&raw_name);
                let file = open_leaf(
                    &chain.entries.last().ok_or("missing root")?.1,
                    &name,
                    true,
                    cancel,
                )?;
                let before = snapshot(file.as_raw_handle(), true, cancel)?;
                chain.entries.push((name, Dir::from_std_file(file), before));
            }
            Ok(chain)
        }
        fn directory(&self) -> Result<&Dir, String> {
            self.entries
                .last()
                .map(|entry| &entry.1)
                .ok_or_else(|| "missing bundle directory".into())
        }
        fn revalidate(&self, cancel: &AtomicBool) -> Result<(), String> {
            for (index, (name, dir, before)) in self.entries.iter().enumerate() {
                let named = if index == 0 {
                    open_drive(&self.root_path, cancel)?
                } else {
                    open_leaf(&self.entries[index - 1].1, name, true, cancel)?
                };
                require(
                    snapshot(dir.as_raw_handle(), true, cancel)? == *before
                        && snapshot(named.as_raw_handle(), true, cancel)? == *before,
                    "Windows directory changed during verification",
                )?;
            }
            check_cancel(cancel)
        }
    }
    pub(super) fn verify(
        bundle: &InternalBundle,
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let chain = DirectoryChain::acquire(path, cancel)?;
        let dir = chain.directory()?;
        let mut pinned = Vec::new();
        let mut identities = std::collections::HashSet::new();
        for component in &bundle.manifest.components {
            let mut file = open_leaf(dir, OsStr::new(&component.filename), false, cancel)?;
            let before = snapshot(file.as_raw_handle(), false, cancel)?;
            require(
                before.length as u64 == component.byte_size,
                "wrong Windows component length",
            )?;
            require(
                identities.insert((before.volume, before.id)),
                "Windows component names alias the same file",
            )?;
            #[cfg(test)]
            super::tests::event("opened", &component.filename, 0);
            verify_bytes(&mut file, component, cancel)?;
            pinned.push((component, file, before));
        }
        for (component, file, before) in &pinned {
            let named = open_leaf(dir, OsStr::new(&component.filename), false, cancel)?;
            require(
                snapshot(file.as_raw_handle(), false, cancel)? == *before
                    && snapshot(named.as_raw_handle(), false, cancel)? == *before,
                "Windows component changed during verification",
            )?;
            #[cfg(test)]
            super::tests::event("readback", &component.filename, component.byte_size);
            check_cancel(cancel)?;
        }
        chain.revalidate(cancel)?;
        check_cancel(cancel)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    type Hook = Box<dyn FnMut(&str, &str, u64)>;
    thread_local! { static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) }; }
    pub(super) fn event(stage: &str, name: &str, count: u64) {
        HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(stage, name, count);
            }
        });
    }
    fn with_hook(hook: Hook, test: impl FnOnce()) {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                HOOK.with(|hook| *hook.borrow_mut() = None);
            }
        }
        HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
        let _reset = Reset;
        test();
    }
    fn ordinary_bundle() -> (tempfile::TempDir, InternalBundle) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("staging-esp.img"), vec![b'E'; 4096]).unwrap();
        std::fs::write(dir.path().join("staging-root.img"), vec![b'R'; 8192]).unwrap();
        let bundle = validate_internal_bundle(&bytes(&fixture())).unwrap();
        (dir, bundle)
    }

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
    /// Missing components, exact bytes, special files, replacement and cancellation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn component_verification_requires_exact_stable_regular_files() {
        // Phase `missing_components_are_rejected`.
        {
            let dir = tempfile::tempdir().unwrap();
            let bundle = validate_internal_bundle(&bytes(&fixture())).unwrap();
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err()
            );
        }

        // Phase `component_bytes_must_match_exact_length_and_digest`.
        {
            let (dir, bundle) = ordinary_bundle();
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_ok()
            );
            for data in [vec![b'R'; 8191], vec![b'R'; 8193], vec![b'X'; 8192]] {
                std::fs::write(dir.path().join("staging-root.img"), data).unwrap();
                assert!(
                    verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false))
                        .is_err()
                );
            }
        }

        // Phase `rejects_links_directories_and_fifos_without_blocking`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let (dir, bundle) = ordinary_bundle();
            let root = dir.path().join("staging-root.img");
            std::fs::rename(&root, dir.path().join("saved.img")).unwrap();
            symlink("saved.img", &root).unwrap();
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err()
            );
            std::fs::remove_file(&root).unwrap();
            std::fs::hard_link(dir.path().join("saved.img"), &root).unwrap();
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err()
            );
            std::fs::remove_file(&root).unwrap();
            std::fs::create_dir(&root).unwrap();
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err()
            );
            std::fs::remove_dir(&root).unwrap();
            let name = std::ffi::CString::new(root.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: the temporary pathname is NUL-terminated and remains live.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err()
            );
            let alias = dir.path().with_extension("link");
            symlink(dir.path(), &alias).unwrap();
            assert!(verify_internal_components(&bundle, &alias, &AtomicBool::new(false)).is_err());
            std::fs::remove_file(alias).unwrap();
        }

        // Phase `rejects_replacement_after_open_and_of_an_earlier_component`.
        #[cfg(unix)]
        {
            for target in ["staging-root.img", "staging-esp.img"] {
                let (dir, bundle) = ordinary_bundle();
                let root = dir.path().to_owned();
                with_hook(
                    Box::new(move |stage, name, _| {
                        if stage == "opened" && name == "staging-root.img" {
                            std::fs::rename(root.join(target), root.join("retained.img")).unwrap();
                            std::fs::write(
                                root.join(target),
                                vec![
                                    if target == "staging-root.img" {
                                        b'R'
                                    } else {
                                        b'E'
                                    };
                                    if target == "staging-root.img" {
                                        8192
                                    } else {
                                        4096
                                    }
                                ],
                            )
                            .unwrap();
                        }
                    }),
                    || {
                        assert!(
                            verify_internal_components(
                                &bundle,
                                dir.path(),
                                &AtomicBool::new(false)
                            )
                            .is_err()
                        )
                    },
                );
            }
        }

        // Phase `rejects_directory_replacement_even_when_pinned_bytes_match`.
        #[cfg(unix)]
        {
            let parent = tempfile::tempdir().unwrap();
            let path = parent.path().join("bundle");
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("staging-esp.img"), vec![b'E'; 4096]).unwrap();
            std::fs::write(path.join("staging-root.img"), vec![b'R'; 8192]).unwrap();
            let bundle = validate_internal_bundle(&bytes(&fixture())).unwrap();
            let root = path.clone();
            let moved = parent.path().join("retained");
            with_hook(
                Box::new(move |stage, name, _| {
                    if stage == "opened" && name == "staging-esp.img" {
                        std::fs::rename(&root, &moved).unwrap();
                        std::fs::create_dir(&root).unwrap();
                    }
                }),
                || {
                    assert!(
                        verify_internal_components(&bundle, &path, &AtomicBool::new(false))
                            .is_err()
                    )
                },
            );
        }

        // Phase `observes_cancellation_before_open_first_middle_last_read_and_eof`.
        {
            use std::sync::{Arc, atomic::Ordering};
            let (dir, _) = ordinary_bundle();
            // Three reads, with the final read shorter than the fixed buffer.
            let payload = vec![b'R'; 131584];
            std::fs::write(dir.path().join("staging-root.img"), &payload).unwrap();
            let mut manifest = fixture();
            manifest["components"][1]["byte_size"] = json!(payload.len());
            manifest["components"][1]["sha256"] = json!(hex::encode(Sha256::digest(&payload)));
            manifest["source_image_bytes"] = json!(21504 + payload.len());
            let bundle = validate_internal_bundle(&bytes(&manifest)).unwrap();
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_ok()
            );
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(true)).is_err()
            );
            for (stage, count) in [
                ("opened", 0),
                ("read", 65536),
                ("read", 131072),
                ("read", 131584),
                ("eof", 131584),
                ("readback", 131584),
            ] {
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                with_hook(
                    Box::new(move |observed, name, bytes| {
                        if name == "staging-root.img" && observed == stage && bytes == count {
                            signal.store(true, Ordering::Release);
                        }
                    }),
                    || {
                        assert!(
                            verify_internal_components(&bundle, dir.path(), &cancel).is_err(),
                            "missed {stage}/{count}"
                        )
                    },
                );
            }
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

        // Phase `windows_unicode_directory_verifies_ordinary_payloads`.
        #[cfg(windows)]
        {
            let parent = tempfile::tempdir().unwrap();
            let path = parent.path().join("André-用户-e\u{301}-😀");
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("staging-esp.img"), vec![b'E'; 4096]).unwrap();
            std::fs::write(path.join("staging-root.img"), vec![b'R'; 8192]).unwrap();
            let bundle = validate_internal_bundle(&bytes(&fixture())).unwrap();
            assert!(verify_internal_components(&bundle, &path, &AtomicBool::new(false)).is_ok());
            assert!(verify_internal_components(&bundle, &path, &AtomicBool::new(true)).is_err());
            std::fs::write(path.join("staging-root.img"), vec![b'X'; 8192]).unwrap();
            assert!(verify_internal_components(&bundle, &path, &AtomicBool::new(false)).is_err());
        }
    }

    /// Namespaces, separators, aliases, devices, streams and drive roots.
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
                assert!(!windows_leaf_supported(name));
                let mut manifest = fixture();
                manifest["components"][0]["filename"] = json!(name);
                assert!(validate_internal_bundle(&bytes(&manifest)).is_err());
            }
        }

        // Phase `windows_names_cannot_enter_device_stream_or_alias_namespaces`.
        {
            for name in [
                "",
                ".",
                "..",
                "file.",
                "file ",
                "CON.img",
                "lpt1.img",
                "aux",
                "nul.txt",
                "COM9.img",
                "file:stream",
                "a/b",
                "a\\b",
            ] {
                assert!(!windows_leaf_supported(name), "accepted {name}");
            }
            assert!(windows_leaf_supported("staging-root.img"));
        }

        // Phase `windows_drive_root_cannot_hide_a_subdirectory_mapping`.
        {
            assert!(windows_volume_root(
                r"\\?\Volume{12345678-1234-4234-8234-123456789abc}\"
            ));
            assert!(!windows_volume_root(
                r"\\?\Volume{12345678-1234-4234-8234-123456789abc}\mapped"
            ));
            assert!(!windows_volume_root(r"\\?\UNC\server\share\"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn case_aliases_cannot_satisfy_both_component_roles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("payload.img"), vec![b'E'; 4096]).unwrap();
        // This regression applies to case-insensitive source filesystems only.
        if !dir.path().join("PAYLOAD.img").exists() {
            return;
        }
        let mut data = fixture();
        data["components"][0]["filename"] = json!("payload.img");
        data["components"][1]["filename"] = json!("PAYLOAD.img");
        data["components"][1]["byte_size"] = json!(4096);
        data["components"][1]["sha256"] = data["components"][0]["sha256"].clone();
        let bundle = validate_internal_bundle(&bytes(&data)).unwrap();
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err());
    }
    fn positional_record(value: &Value, fields: &str) -> Value {
        Value::Array(
            fields
                .split_whitespace()
                .map(|key| value[key].clone())
                .collect(),
        )
    }
}
