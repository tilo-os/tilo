//! Linux UAPI evidence for ext4, XFS and single-device Btrfs: native UUID,
//! generation-bearing export handles, hardware identity and health evidence.
//! No filesystem label, `dev_t`, boot ID or path is a persistent identity.
use super::super::model::{EndpointIdentity, NamePolicy};
use super::{Capabilities, Capability, EndpointError, NativeEvidence, digest, readable_directory};
use cap_fs_ext::OpenOptionsFollowExt;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

fn ioctl<const N: usize>(
    file: &File,
    request: libc::c_ulong,
    mut data: [u8; N],
) -> Result<[u8; N], EndpointError> {
    // SAFETY: live fd; each caller uses the exact Linux UAPI-sized byte buffer.
    if unsafe { libc::ioctl(file.as_raw_fd(), request, data.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(data)
}
fn slice(bytes: &[u8], start: usize, end: usize) -> Result<&[u8], EndpointError> {
    bytes.get(start..end).ok_or(EndpointError::Unsupported)
}
fn number(bytes: &[u8], start: usize) -> Result<u64, EndpointError> {
    Ok(u64::from_ne_bytes(
        slice(bytes, start, start + 8)?
            .try_into()
            .map_err(|_| EndpointError::Unsupported)?,
    ))
}
fn btrfs_uuid(file: &File) -> Result<([u8; 16], u64), EndpointError> {
    // BTRFS_IOC_FS_INFO (1024): max_id at 0, num_devices at 8, fsid at 16.
    let info = ioctl(file, 0x8400_941f, [0; 1024])?;
    let uuid: [u8; 16] = slice(&info, 16, 32)?
        .try_into()
        .map_err(|_| EndpointError::Unsupported)?;
    if uuid == [0; 16] {
        return Err(EndpointError::Unsupported);
    }
    // Multi-device membership and degraded/replacement topology need their own
    // native tests. Refuse rather than infer health from an accessible root.
    if number(&info, 8)? != 1 {
        return Err(EndpointError::Unsupported);
    }
    Ok((uuid, number(&info, 0)?))
}

// Clone identification deliberately does not admit filesystem health. A dirty
// duplicate still creates ambiguity, and an unreadable UUID is not evidence of
// a different volume. Health is checked only for the selected endpoint below.
fn clone_uuid(file: &File) -> Result<[u8; 16], EndpointError> {
    let bytes = if let Ok(fsuuid) = ioctl(file, 0x8011_1500, [0; 17]) {
        if fsuuid.first() != Some(&16) {
            return Err(EndpointError::Unsupported);
        }
        slice(&fsuuid, 1, 17)?.to_vec()
    } else if let Ok(info) = ioctl(file, 0x8400_941f, [0; 1024]) {
        slice(&info, 16, 32)?.to_vec()
    } else {
        let geometry = ioctl(file, 0x8100_587e, [0; 256])?;
        slice(&geometry, 64, 80)?.to_vec()
    };
    let uuid: [u8; 16] = bytes.try_into().map_err(|_| EndpointError::Unsupported)?;
    if uuid == [0; 16] {
        return Err(EndpointError::Unsupported);
    }
    Ok(uuid)
}
fn inspect_clone_candidate(mount: &Mount) -> Result<[u8; 16], EndpointError> {
    let pin = super::Pin::open(&mount.path)?;
    let file = readable_directory(&pin.file)?;
    if mount_id(&file)? != mount.id {
        return Err(EndpointError::IdentityChanged);
    }
    let uuid = clone_uuid(&file)?;
    pin.validate()?;
    Ok(uuid)
}

fn native_volume(file: &File) -> Result<([u8; 16], bool), EndpointError> {
    if let Ok((uuid, device)) = btrfs_uuid(file) {
        device_health(file, device)?;
        return Ok((uuid, true));
    }
    // XFS_IOC_FSGEOMETRY is 256 bytes, UUID offset64, flags92, sick112.
    // A successful geometry query supplies UUID and native health, independent
    // of the mount's human-readable filesystem name.
    if let Ok(geometry) = ioctl(file, 0x8100_587e, [0; 256]) {
        if slice(&geometry, 112, 116)?.iter().any(|b| *b != 0) {
            return Err(EndpointError::UnsafeVolume);
        }
        let flags = u32::from_ne_bytes(
            slice(&geometry, 92, 96)?
                .try_into()
                .map_err(|_| EndpointError::Unsupported)?,
        );
        if flags & (1 << 12) != 0 {
            return Err(EndpointError::Unsupported);
        }
        let uuid = slice(&geometry, 64, 80)?
            .try_into()
            .map_err(|_| EndpointError::Unsupported)?;
        if uuid == [0; 16] {
            return Err(EndpointError::Unsupported);
        }
        return Ok((uuid, false));
    }
    // Generic kernel UUID plus the kernel's own sysfs instance path. Merely
    // seeing an ext4 name does not suffice: errors_count must be readable/zero,
    // and export handles are required separately. Missing health is refusal.
    let fsuuid = ioctl(file, 0x8011_1500, [0; 17])?;
    if fsuuid.first() != Some(&16) {
        return Err(EndpointError::Unsupported);
    }
    let uuid = slice(&fsuuid, 1, 17)?
        .try_into()
        .map_err(|_| EndpointError::Unsupported)?;
    if uuid == [0; 16] {
        return Err(EndpointError::Unsupported);
    }
    let path = ioctl(file, 0x8081_1501, [0; 129])?;
    let len = usize::from(*path.first().ok_or(EndpointError::Unsupported)?);
    let name = std::str::from_utf8(slice(&path, 1, len + 1)?)
        .map_err(|_| EndpointError::Unsupported)?
        .trim_end_matches('\0');
    let parts: Vec<_> = name.split('/').collect();
    if parts.len() != 2
        || parts.first() != Some(&"ext4")
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == ".." || *part == ".")
    {
        return Err(EndpointError::Unsupported);
    }
    let mut errors = String::new();
    File::open(PathBuf::from("/sys/fs").join(name).join("errors_count"))?
        .take(64)
        .read_to_string(&mut errors)?;
    if errors.trim() != "0" {
        return Err(EndpointError::UnsafeVolume);
    }
    Ok((uuid, false))
}

fn mount_id(file: &File) -> Result<u64, EndpointError> {
    let mut stat = std::mem::MaybeUninit::<libc::statx>::uninit();
    // SAFETY: fd is live, empty C path uses AT_EMPTY_PATH, complete output.
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            stat.as_mut_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful statx initialized the native struct.
    let stat = unsafe { stat.assume_init() };
    if stat.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(EndpointError::Unsupported);
    }
    Ok(stat.stx_mnt_id)
}
#[repr(C)]
struct ExportHandle {
    bytes: u32,
    kind: i32,
    data: [u8; 128],
}
fn persistent_root(file: &File, btrfs: bool) -> Result<Vec<u8>, EndpointError> {
    let mut handle = ExportHandle {
        bytes: 128,
        kind: 0,
        data: [0; 128],
    };
    let mut mount = 0;
    // SAFETY: Linux flexible file_handle header followed by 128 writable bytes;
    // AT_EMPTY_PATH binds the query to the retained descriptor, not a pathname.
    let result = unsafe {
        libc::name_to_handle_at(
            file.as_raw_fd(),
            c"".as_ptr(),
            (&raw mut handle).cast(),
            &raw mut mount,
            libc::AT_EMPTY_PATH,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if handle.bytes == 0 || handle.bytes > 128 {
        return Err(EndpointError::Unsupported);
    }
    let mut bytes = handle.kind.to_le_bytes().to_vec();
    bytes.extend_from_slice(
        handle
            .data
            .get(..handle.bytes as usize)
            .ok_or(EndpointError::Unsupported)?,
    );
    // Btrfs snapshot trees can reuse inode numbers. Bind explicit subvolume UUID
    // and tree ID in addition to the kernel generation-bearing export handle.
    if !btrfs {
        return Ok(bytes);
    }
    let subvolume = ioctl(file, 0x81f8_943c, [0; 504])?;
    let tree = number(&subvolume, 0)?;
    let sub_uuid = slice(&subvolume, 296, 312)?;
    if tree == 0 || (tree != 5 && sub_uuid.iter().all(|b| *b == 0)) {
        return Err(EndpointError::Unsupported);
    }
    bytes.extend_from_slice(&tree.to_le_bytes());
    bytes.extend_from_slice(sub_uuid);
    Ok(bytes)
}
fn device_health(file: &File, device: u64) -> Result<(), EndpointError> {
    // BTRFS_IOC_GET_DEV_STATS: devid, nr_items, flags=0 (never reset), five
    // counters; UAPI structure padded to 1032 bytes. A nonzero counter refuses.
    let mut data = [0_u8; 1032];
    data.get_mut(..8)
        .ok_or(EndpointError::Unsupported)?
        .copy_from_slice(&device.to_ne_bytes());
    data.get_mut(8..16)
        .ok_or(EndpointError::Unsupported)?
        .copy_from_slice(&5_u64.to_ne_bytes());
    let result = ioctl(file, 0xc408_9434, data)?;
    if number(&result, 8)? != 5 || slice(&result, 24, 64)?.iter().any(|b| *b != 0) {
        return Err(EndpointError::UnsafeVolume);
    }
    Ok(())
}
#[derive(Debug)]
struct Mount {
    id: u64,
    device: String,
    path: PathBuf,
    readonly: bool,
    kernel_type: String,
    unsafe_options: bool,
}
fn unescape(field: &str) -> Result<PathBuf, EndpointError> {
    let mut output = Vec::new();
    let mut iter = field.as_bytes().iter().copied();
    while let Some(byte) = iter.next() {
        if byte != b'\\' {
            output.push(byte);
            continue;
        }
        let escaped = [iter.next(), iter.next(), iter.next()];
        match escaped {
            [Some(b'0'), Some(b'4'), Some(b'0')] => output.push(b' '),
            [Some(b'0'), Some(b'1'), Some(b'1')] => output.push(b'\t'),
            [Some(b'0'), Some(b'1'), Some(b'2')] => output.push(b'\n'),
            [Some(b'1'), Some(b'3'), Some(b'4')] => output.push(b'\\'),
            _ => return Err(EndpointError::Unsupported),
        }
    }
    Ok(std::ffi::OsString::from_vec(output).into())
}
fn mounts() -> Result<Vec<Mount>, EndpointError> {
    let mut bytes = Vec::new();
    File::open("/proc/self/mountinfo")?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(EndpointError::LimitExceeded);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| EndpointError::Unsupported)?;
    let mut result = Vec::new();
    for line in text.lines() {
        if result.len() >= 4096 {
            return Err(EndpointError::LimitExceeded);
        }
        let mut fields = line.split(' ');
        let id = fields
            .next()
            .and_then(|f| f.parse().ok())
            .ok_or(EndpointError::Unsupported)?;
        fields.next().ok_or(EndpointError::Unsupported)?;
        let device = fields.next().ok_or(EndpointError::Unsupported)?.to_owned();
        fields.next().ok_or(EndpointError::Unsupported)?;
        let path = unescape(fields.next().ok_or(EndpointError::Unsupported)?)?;
        let options = fields.next().ok_or(EndpointError::Unsupported)?;
        // A kernel mount type is only a dispatch filter for potential clones;
        // it never establishes capability or identity. Those require ioctls.
        let kernel_type = fields
            .by_ref()
            .find(|field| *field == "-")
            .and_then(|_| fields.next())
            .ok_or(EndpointError::Unsupported)?
            .to_owned();
        fields.next().ok_or(EndpointError::Unsupported)?; // mount source
        let super_options = fields.next().ok_or(EndpointError::Unsupported)?;
        let unsafe_options = options
            .split(',')
            .chain(super_options.split(','))
            .any(|option| {
                matches!(
                    option,
                    "noload" | "norecovery" | "nobarrier" | "barrier=0" | "degraded"
                ) || option.starts_with("rescue=")
            });
        result.push(Mount {
            id,
            device,
            path,
            readonly: options.split(',').any(|o| o == "ro"),
            kernel_type,
            unsafe_options,
        });
    }
    Ok(result)
}
fn bounded_text(path: &std::path::Path) -> Result<Vec<u8>, EndpointError> {
    let mut bytes = Vec::new();
    File::open(path)?.take(4097).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err(EndpointError::Unsupported);
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes.pop();
    }
    if bytes.is_empty() || bytes.contains(&0) {
        return Err(EndpointError::Unsupported);
    }
    Ok(bytes)
}
fn btrfs_device(uuid: &[u8; 16]) -> Result<u64, EndpointError> {
    use std::fmt::Write;
    let mut text = String::new();
    for (index, byte) in uuid.iter().enumerate() {
        if [4, 6, 8, 10].contains(&index) {
            text.push('-');
        }
        write!(text, "{byte:02x}").map_err(|_| EndpointError::Unsupported)?;
    }
    // Btrfs inode st_dev is a virtual subvolume device, not the block device.
    // FS_INFO's UUID identifies the kernel sysfs instance; require its one
    // device entry (the earlier FS_INFO query independently required one).
    let mut devices = std::fs::read_dir(PathBuf::from("/sys/fs/btrfs").join(text).join("devices"))?;
    let device = devices.next().ok_or(EndpointError::Offline)??;
    if devices.next().is_some() {
        return Err(EndpointError::Unsupported);
    }
    let dev = bounded_text(&device.path().join("dev"))?;
    let dev = std::str::from_utf8(&dev).map_err(|_| EndpointError::Unsupported)?;
    let (major, minor) = dev.split_once(':').ok_or(EndpointError::Unsupported)?;
    Ok(libc::makedev(
        major.parse().map_err(|_| EndpointError::Unsupported)?,
        minor.parse().map_err(|_| EndpointError::Unsupported)?,
    ))
}
fn device_identity(device: u64, depth: usize) -> Result<Vec<u8>, EndpointError> {
    if depth > 4 {
        return Err(EndpointError::LimitExceeded);
    }
    let sys = PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        libc::major(device),
        libc::minor(device)
    ))
    .canonicalize()?;
    // Device-mapper identity is meaningful only together with its one underlying
    // device. Never unlock a crypt mapping or accept unresolved/multi-device IO.
    if let Ok(uuid) = bounded_text(&sys.join("dm/uuid")) {
        let mut slaves = std::fs::read_dir(sys.join("slaves"))?;
        let slave = slaves.next().ok_or(EndpointError::Unsupported)??;
        if slaves.next().is_some() {
            return Err(EndpointError::Unsupported);
        }
        let dev = bounded_text(&slave.path().join("dev"))?;
        let text = std::str::from_utf8(&dev).map_err(|_| EndpointError::Unsupported)?;
        let (major, minor) = text.split_once(':').ok_or(EndpointError::Unsupported)?;
        let parent = libc::makedev(
            major.parse().map_err(|_| EndpointError::Unsupported)?,
            minor.parse().map_err(|_| EndpointError::Unsupported)?,
        );
        let mut result = device_identity(parent, depth + 1)?;
        result.extend_from_slice(&uuid);
        return Ok(result);
    }
    // A loop number changes across boots. Its backing file's export handle,
    // filesystem UUID and physical disk provide the persistent identity chain.
    if let Ok(backing) = bounded_text(&sys.join("loop/backing_file")) {
        let path = PathBuf::from(std::ffi::OsString::from_vec(backing));
        let parent = path.parent().ok_or(EndpointError::Unsupported)?;
        let name = path.file_name().ok_or(EndpointError::Unsupported)?;
        let pin = super::Pin::open(parent)?;
        let mut options = cap_primitives::fs::OpenOptions::new();
        options.read(true);
        options.follow(cap_primitives::fs::FollowSymlinks::No);
        let file = cap_primitives::fs::open(&pin.file, std::path::Path::new(name), &options)?;
        let (uuid, btrfs) = native_volume(&readable_directory(&pin.file)?)?;
        let underlying = if btrfs {
            btrfs_device(&uuid)?
        } else {
            file.metadata()?.dev()
        };
        let mut result = device_identity(underlying, depth + 1)?;
        result.extend_from_slice(&uuid);
        // Ordinary backing files don't support subvolume ioctl on all kernels;
        // generation-bearing kernel export handle already includes the tree.
        result.extend_from_slice(&persistent_root(&file, false)?);
        pin.validate()?;
        return Ok(result);
    }
    let mut ancestor = Some(sys.as_path());
    for _ in 0..8 {
        let Some(path) = ancestor else {
            break;
        };
        for attribute in ["wwid", "device/wwid", "serial", "device/serial"] {
            if let Ok(value) = bounded_text(&path.join(attribute)) {
                let mut identity = if attribute.ends_with("wwid") {
                    let mut identity = b"wwid\0".to_vec();
                    identity.extend_from_slice(&value);
                    identity
                } else {
                    let vendor = bounded_text(&path.join("vendor"))
                        .or_else(|_| bounded_text(&path.join("device/vendor")))
                        .ok();
                    let model = bounded_text(&path.join("model"))
                        .or_else(|_| bounded_text(&path.join("device/model")))
                        .ok();
                    super::serial_identity(&value, vendor.as_deref(), model.as_deref())?
                };
                if let Ok(start) = bounded_text(&sys.join("start")) {
                    identity.push(0);
                    identity.extend_from_slice(&start);
                }
                return Ok(identity);
            }
        }
        ancestor = path.parent();
    }
    Err(EndpointError::Unsupported)
}
pub(super) fn inspect(file: &File) -> Result<NativeEvidence, EndpointError> {
    let readable = readable_directory(file)?;
    let (volume, btrfs) = native_volume(&readable)?;
    let root = persistent_root(&readable, btrfs)?;
    let physical_device = if btrfs {
        btrfs_device(&volume)?
    } else {
        readable.metadata()?.dev()
    };
    let mut persistent_volume = device_identity(physical_device, 0)?;
    persistent_volume.extend_from_slice(&volume);
    let id = mount_id(file)?;
    let mounts = mounts()?;
    let current = mounts
        .iter()
        .find(|m| m.id == id)
        .ok_or(EndpointError::Offline)?;
    if current.unsafe_options {
        return Err(EndpointError::UnsafeVolume);
    }
    // Detect independently mounted copies of the UUID. Aliases on the same
    // kernel device are allowed; different devices with the UUID are ambiguous.
    for other in &mounts {
        if other.device == current.device || other.kernel_type != current.kernel_type {
            continue;
        }
        let candidate = inspect_clone_candidate(other);
        super::reject_clone_candidate(&volume, candidate)?;
    }
    let mut mount = id.to_le_bytes().to_vec();
    mount.extend_from_slice(current.device.as_bytes());
    // No native crash/restart matrix has yet admitted a writable configuration.
    let write = if current.readonly {
        EndpointError::ReadOnly
    } else {
        EndpointError::Unverified
    };
    Ok(NativeEvidence {
        identity: EndpointIdentity {
            volume: digest(&persistent_volume),
            root: digest(&root),
        },
        mount,
        capabilities: Capabilities {
            safe_read: Capability::Available,
            safe_write: Capability::Refused(write),
            durable_transactions: Capability::Refused(EndpointError::Unverified),
        },
        names: NamePolicy::Exact,
    })
}
pub(super) fn same_mount(a: &File, b: &File) -> Result<(), EndpointError> {
    if mount_id(a)? != mount_id(b)? {
        return Err(EndpointError::IdentityChanged);
    }
    Ok(())
}
pub(super) fn check_metadata(file: &File, directory: bool) -> Result<bool, EndpointError> {
    let meta = file.metadata()?;
    // Refuse ACLs, xattrs, security labels, encryption and case-folded dirs until
    // model/store fidelity can represent them. List only; never read file data.
    // SAFETY: null/zero output asks only for required xattr-list length.
    let count = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if count > 65536 {
        return Err(EndpointError::LimitExceeded);
    }
    if count != 0 {
        let mut names = vec![0_u8; count as usize];
        // SAFETY: live fd and a buffer exactly sized from the bounded first query.
        let size =
            unsafe { libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len()) };
        if size < 0 || size as usize != names.len() {
            return Err(EndpointError::Unstable);
        }
        // SELinux labels are local enforcement policy, not portable content.
        // The public probe discloses this policy; ACLs/other xattrs stay pending.
        if names
            .split(|b| *b == 0)
            .any(|name| !name.is_empty() && name != b"security.selinux")
        {
            return Err(EndpointError::Unsupported);
        }
    }
    if (!directory && meta.nlink() != 1) || meta.mode() & 0o7000 != 0 {
        return Err(EndpointError::Unsupported);
    }
    // FS_IOC_GETFLAGS uses native unsigned long-sized output on this ABI.
    let mut flags: libc::c_long = 0;
    let request = if std::mem::size_of::<libc::c_long>() == 8 {
        0x8008_6601
    } else {
        0x8004_6601
    };
    // SAFETY: live fd and correctly sized writable flags storage.
    if unsafe { libc::ioctl(file.as_raw_fd(), request, &raw mut flags) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // immutable, append-only, encrypted, casefold: unsupported semantics.
    if flags & (0x10 | 0x20 | 0x800 | 0x4000_0000) != 0 {
        return Err(EndpointError::Unsupported);
    }
    Ok(!directory && meta.mode() & 0o111 != 0)
}

// Generation-bearing kernel file handle, scoped by the retained endpoint's
// independently attested volume and live mount session. No inode-only fallback.
pub(super) fn object_generation(file: &File) -> Result<Vec<u8>, EndpointError> {
    persistent_root(file, false)
}

#[cfg(test)]
mod clone_tests {
    use super::*;
    #[test]
    fn native_unresolved_clone_queries_and_paths_require_reselection() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("target");
        std::fs::create_dir(&selected).unwrap();
        std::os::unix::fs::symlink(&selected, root.path().join("link")).unwrap();
        for name in ["missing", "link"] {
            let mount = Mount {
                id: 0,
                device: "0:0".to_owned(),
                path: root.path().join(name),
                readonly: false,
                kernel_type: "ext4".to_owned(),
                unsafe_options: false,
            };
            let candidate = inspect_clone_candidate(&mount);
            assert!(candidate.is_err());
            assert_eq!(
                super::super::reject_clone_candidate(&[1; 16], candidate),
                Err(EndpointError::AmbiguousIdentity)
            );
        }
        // Native ioctl failure on a real unsupported descriptor must not count
        // as evidence of a different UUID. This is no substitute for the
        // separately coordinated same-filesystem mounted-clone fixture.
        let device = File::open("/dev/null").unwrap();
        let candidate = clone_uuid(&device);
        assert!(candidate.is_err());
        assert_eq!(
            super::super::reject_clone_candidate(&[1; 16], candidate),
            Err(EndpointError::AmbiguousIdentity)
        );
    }
}
