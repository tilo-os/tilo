//! Bounded internal-installer data and read-only byte verification.
//! Consistency is not authenticated provenance, filesystem qualification, or boot proof.

use serde::Deserialize;
use std::{path::Path, sync::atomic::AtomicBool};

pub const MAX_MANIFEST_BYTES: usize = 65_536;
const MAX_IMAGE_BYTES: u64 = 1 << 40;
const ESP_GUID: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
const ROOT_GUID: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";

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
    root_locator: String,
    composefs_digest: String,
}

/// Validated data only. This value grants neither stable-byte nor installation authority.
#[derive(Debug)]
pub struct InternalBundle {
    manifest: Manifest,
}

impl InternalBundle {
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

fn efi_path(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("/EFI/") else {
        return false;
    };
    let mut parts = rest.split('/').peekable();
    if value.len() > 240 {
        return false;
    }
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            let Some(stem) = part.strip_suffix(".efi") else {
                return false;
            };
            return !stem.is_empty()
                && stem.as_bytes()[0] != b'.'
                && stem
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c));
        }
        if part.is_empty()
            || !part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        {
            return false;
        }
    }
    false
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
                    && consoles.len() <= 2
                    && matches!(value, "tty0" | "ttyS0,115200"),
                "unsupported console",
            )?;
            continue;
        }
        require(names.insert(name), "duplicate kernel argument")?;
        let allowed = match name {
            "rw"
            | "quiet"
            | "splash"
            | "qos.install"
            | "slab_nomerge"
            | "plymouth.ignore-serial-consoles" => *token == name,
            "root" => value == boot.root_locator,
            "composefs" => value == boot.composefs_digest,
            "qos.mode" => value == "normal",
            "qos.luks" => value == "0".repeat(43),
            "qos.unlock" => value == "0".repeat(74),
            "init_on_alloc" | "init_on_free" | "page_alloc.shuffle" => value == "1",
            "lockdown" => value == "integrity",
            "randomize_kstack_offset" => value == "on",
            "vsyscall" => value == "none",
            "qos.generation" => {
                !value.is_empty()
                    && value.len() <= 10
                    && value.as_bytes()[0] != b'0'
                    && value.bytes().all(|c| c.is_ascii_digit())
            }
            _ => false,
        };
        require(allowed, "unrecognized command-line binding")?;
    }
    require(
        ["root", "composefs", "qos.install", "rw"]
            .iter()
            .all(|name| names.contains(name)),
        "missing boot binding",
    )
}

/// Parse the exact schema-1 exporter contract, without authenticating its origin.
/// # Errors
/// Rejects ambiguous JSON, unsupported profiles and inconsistent or unbounded facts.
pub fn validate_internal_bundle(manifest: &[u8]) -> Result<InternalBundle, String> {
    require(
        !manifest.is_empty() && manifest.len() <= MAX_MANIFEST_BYTES,
        "manifest exceeds size limit",
    )?;
    let data: Manifest = serde_json::from_slice(manifest).map_err(|error| error.to_string())?;
    require(
        data.schema == 1 && data.installer_protocol == 1,
        "unsupported schema or installer protocol",
    )?;
    require(
        data.purpose == "qos-internal-installer",
        "wrong bundle purpose",
    )?;
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
            let arch_guid = if data.architecture == "x86_64" {
                "4f68bce3-e8cd-4db1-96e7-fbcaf984b709"
            } else {
                "b921b045-1df0-41c3-af44-4c6f280d3fae"
            };
            require(
                (c.partition_type_guid == ROOT_GUID || c.partition_type_guid == arch_guid)
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
    let root = data
        .components
        .iter()
        .find(|c| c.role == "staging-root")
        .ok_or("missing root")?;
    require(
        boot.uki_architecture == data.architecture
            && efi_path(&boot.efi_path)
            && canonical_digest(&boot.uki_sha256, 64)
            && canonical_digest(&boot.composefs_digest, 128)
            && boot.root_locator == format!("UUID={}", root.filesystem_uuid),
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
        format!("{:x}", hash.finalize()) == component.sha256,
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
fn windows_volume_root(value: &str) -> bool {
    value
        .strip_prefix(r"\\?\Volume{")
        .and_then(|v| v.strip_suffix(r"}\"))
        .is_some_and(|id| guid(&id.to_ascii_lowercase()))
}

/// Windows is deliberately restricted to ordinary absolute drive-letter paths
/// with ASCII leaf names. UNC, verbatim/device namespaces, relative paths,
/// alternate streams, DOS aliases and every reparse tag fail closed.
#[cfg(windows)]
mod windows {
    use super::*;
    use cap_fs_ext::OpenOptionsFollowExt;
    use cap_primitives::fs::FollowSymlinks;
    use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
    use std::{
        fs::File,
        os::windows::{
            fs::OpenOptionsExt as StdOpenOptionsExt,
            io::{AsRawHandle, RawHandle},
        },
        path::{Component as PathComponent, Prefix},
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
        name: &str,
        directory: bool,
        cancel: &AtomicBool,
    ) -> Result<File, String> {
        check_cancel(cancel)?;
        require(
            windows_leaf_supported(name),
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
        root_path: String,
        // Each handle remains live, preventing compatible delete/write opens.
        entries: Vec<(String, Dir, Snapshot)>,
    }
    fn open_drive(root: &str, cancel: &AtomicBool) -> Result<File, String> {
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
            let raw = path
                .to_str()
                .ok_or("unsupported non-Unicode Windows path")?;
            // Parse raw separators ourselves: Path::components normalizes dot
            // segments; those aliases are deliberately unsupported here.
            require(
                !raw.contains('/')
                    && raw.len() >= 3
                    && raw.as_bytes()[0].is_ascii_alphabetic()
                    && &raw.as_bytes()[1..3] == b":\\",
                "require an ordinary absolute Windows drive path",
            )?;
            let mut components = path.components();
            require(
                matches!(components.next(),Some(PathComponent::Prefix(prefix)) if matches!(prefix.kind(),Prefix::Disk(_)))
                    && matches!(components.next(), Some(PathComponent::RootDir)),
                "unsupported Windows namespace",
            )?;
            let root_path = raw[..3].to_owned();
            let names: Vec<_> = if raw.len() == 3 {
                Vec::new()
            } else {
                raw[3..].split('\\').collect()
            };
            require(
                raw.len() <= 4096
                    && names.len() <= 64
                    && names.iter().all(|name| windows_leaf_supported(name)),
                "unsupported Windows directory component",
            )?;
            let root = open_drive(&root_path, cancel)?;
            let root_snapshot = snapshot(root.as_raw_handle(), true, cancel)?;
            let mut chain = Self {
                root_path,
                entries: vec![(String::new(), Dir::from_std_file(root), root_snapshot)],
            };
            for name in names {
                let file = open_leaf(
                    &chain.entries.last().ok_or("missing root")?.1,
                    name,
                    true,
                    cancel,
                )?;
                let before = snapshot(file.as_raw_handle(), true, cancel)?;
                chain
                    .entries
                    .push((name.into(), Dir::from_std_file(file), before));
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
            let mut file = open_leaf(dir, &component.filename, false, cancel)?;
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
            let named = open_leaf(dir, &component.filename, false, cancel)?;
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

    // Same sizes, offsets, identities and payloads as Python valid_fixture().
    // These are ordinary synthetic bytes, not filesystems or a bootable UKI.
    pub(crate) fn fixture() -> Value {
        let root = "12345678-1234-4234-8234-123456789abc";
        json!({"schema":1,"purpose":"qos-internal-installer","release_id":"fixture-only",
            "architecture":"x86_64","firmware":"uefi","secure_boot":"disabled",
            "installer_protocol":1,"source_image_bytes":65536,
            "components":[
                {"role":"staging-esp","filename":"staging-esp.img","byte_size":4096,
                 "sha256":format!("{:x}",Sha256::digest(vec![b'E';4096])),
                 "partition_type_guid":ESP_GUID,"filesystem_type":"vfat","filesystem_uuid":"1234-ABCD",
                 "minimum_partition_bytes":4096,"source_offset":17408},
                {"role":"staging-root","filename":"staging-root.img","byte_size":8192,
                 "sha256":format!("{:x}",Sha256::digest(vec![b'R';8192])),
                 "partition_type_guid":ROOT_GUID,"filesystem_type":"btrfs","filesystem_uuid":root,
                 "minimum_partition_bytes":8192,"source_offset":21504}],
            "boot_binding":{"efi_path":"/EFI/Linux/fixture.efi","uki_architecture":"x86_64",
                "uki_sha256":"a".repeat(64),"composefs_digest":"b".repeat(128),
                "root_locator":format!("UUID={root}"),
                "uki_cmdline":format!("root=UUID={root} rw qos.install composefs={}","b".repeat(128))}})
    }
    pub(crate) fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }
    #[test]
    fn python_compatible_positive() {
        assert!(validate_internal_bundle(&bytes(&fixture())).is_ok());
    }
    #[test]
    fn rejects_single_field_schema_corruptions() {
        let cases = [
            ("/schema", json!(2)),
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
                "/components/1/filesystem_uuid",
                json!("00000000-0000-0000-0000-000000000000"),
            ),
            ("/components/0/filesystem_uuid", json!("1234-abcd")),
            ("/components/0/filesystem_type", json!("btrfs")),
            (
                "/components/1/partition_type_guid",
                json!("b921b045-1df0-41c3-af44-4c6f280d3fae"),
            ),
            ("/boot_binding/uki_architecture", json!("aarch64")),
            ("/boot_binding/efi_path", json!("/EFI/../evil.efi")),
            ("/boot_binding/root_locator", json!("UUID=other")),
            ("/boot_binding/composefs_digest", json!("c".repeat(128))),
            ("/boot_binding/uki_sha256", json!("A".repeat(64))),
        ];
        for (path, value) in cases {
            let mut data = fixture();
            *data.pointer_mut(path).unwrap() = value;
            assert!(
                validate_internal_bundle(&bytes(&data)).is_err(),
                "accepted {path}"
            );
        }
    }
    #[test]
    fn rejects_ambiguous_or_unbounded_json() {
        let raw = String::from_utf8(bytes(&fixture())).unwrap();
        assert!(
            validate_internal_bundle(
                raw.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1)
                    .as_bytes()
            )
            .is_err()
        );
        for value in [json!(true), json!(1.0), json!("1"), json!(-1)] {
            let mut data = fixture();
            data["schema"] = value;
            assert!(validate_internal_bundle(&bytes(&data)).is_err());
        }
        let mut data = fixture();
        data["components"][0]["source_attributes"] = json!(0);
        assert!(validate_internal_bundle(&bytes(&data)).is_err());
        let mut raw = bytes(&fixture());
        raw.resize(MAX_MANIFEST_BYTES + 1, b' ');
        assert!(validate_internal_bundle(&raw).is_err());
    }
    #[test]
    fn cmdline_profile_is_closed() {
        let base = fixture()["boot_binding"]["uki_cmdline"]
            .as_str()
            .unwrap()
            .to_owned();
        for tail in [
            " ro",
            " rw",
            " init=/bin/sh",
            " console=tty0 console=tty0",
            " qos.generation=0",
            " qos.mode=rescue",
            " unknown",
            " console=tty1",
        ] {
            let mut data = fixture();
            data["boot_binding"]["uki_cmdline"] = json!(format!("{base}{tail}"));
            assert!(
                validate_internal_bundle(&bytes(&data)).is_err(),
                "accepted {tail}"
            );
        }
        let mut data = fixture();
        data["boot_binding"]["uki_cmdline"] = json!(format!(
            "{base} console=tty0 console=ttyS0,115200 qos.generation=12"
        ));
        assert!(validate_internal_bundle(&bytes(&data)).is_ok());
    }
    #[test]
    fn missing_components_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = validate_internal_bundle(&bytes(&fixture())).unwrap();
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn component_bytes_must_match_exact_length_and_digest() {
        let (dir, bundle) = ordinary_bundle();
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_ok());
        for data in [vec![b'R'; 8191], vec![b'R'; 8193], vec![b'X'; 8192]] {
            std::fs::write(dir.path().join("staging-root.img"), data).unwrap();
            assert!(
                verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err()
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn rejects_links_directories_and_fifos_without_blocking() {
        use std::os::unix::fs::symlink;
        let (dir, bundle) = ordinary_bundle();
        let root = dir.path().join("staging-root.img");
        std::fs::rename(&root, dir.path().join("saved.img")).unwrap();
        symlink("saved.img", &root).unwrap();
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err());
        std::fs::remove_file(&root).unwrap();
        std::fs::hard_link(dir.path().join("saved.img"), &root).unwrap();
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err());
        std::fs::remove_file(&root).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err());
        std::fs::remove_dir(&root).unwrap();
        let name = std::ffi::CString::new(root.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: the temporary pathname is NUL-terminated and remains live.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_err());
        let alias = dir.path().with_extension("link");
        symlink(dir.path(), &alias).unwrap();
        assert!(verify_internal_components(&bundle, &alias, &AtomicBool::new(false)).is_err());
        std::fs::remove_file(alias).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn rejects_replacement_after_open_and_of_an_earlier_component() {
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
                        verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false))
                            .is_err()
                    )
                },
            );
        }
    }
    #[test]
    fn observes_cancellation_before_open_first_middle_last_read_and_eof() {
        use std::sync::{Arc, atomic::Ordering};
        let (dir, _) = ordinary_bundle();
        // Three reads, with the final read shorter than the fixed buffer.
        let payload = vec![b'R'; 131584];
        std::fs::write(dir.path().join("staging-root.img"), &payload).unwrap();
        let mut manifest = fixture();
        manifest["components"][1]["byte_size"] = json!(payload.len());
        manifest["components"][1]["minimum_partition_bytes"] = json!(payload.len());
        manifest["components"][1]["sha256"] = json!(format!("{:x}", Sha256::digest(&payload)));
        manifest["source_image_bytes"] = json!(21504 + payload.len());
        let bundle = validate_internal_bundle(&bytes(&manifest)).unwrap();
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(false)).is_ok());
        assert!(verify_internal_components(&bundle, dir.path(), &AtomicBool::new(true)).is_err());
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

    #[test]
    fn windows_names_cannot_enter_device_stream_or_alias_namespaces() {
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

    #[test]
    fn windows_drive_root_cannot_hide_a_subdirectory_mapping() {
        assert!(windows_volume_root(
            r"\\?\Volume{12345678-1234-4234-8234-123456789abc}\"
        ));
        assert!(!windows_volume_root(
            r"\\?\Volume{12345678-1234-4234-8234-123456789abc}\mapped"
        ));
        assert!(!windows_volume_root(r"\\?\UNC\server\share\"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_directory_replacement_even_when_pinned_bytes_match() {
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
                    verify_internal_components(&bundle, &path, &AtomicBool::new(false)).is_err()
                )
            },
        );
    }
    #[test]
    fn accepts_both_architectures_and_rejects_nested_unknown_or_duplicate_keys() {
        let mut value = fixture();
        value["architecture"] = json!("aarch64");
        value["boot_binding"]["uki_architecture"] = json!("aarch64");
        value["components"][1]["partition_type_guid"] =
            json!("b921b045-1df0-41c3-af44-4c6f280d3fae");
        assert!(validate_internal_bundle(&bytes(&value)).is_ok());
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
}
