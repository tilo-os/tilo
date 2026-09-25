//! Linux UAPI evidence for ext4, XFS and single-device Btrfs: native UUID,
//! generation-bearing export handles, hardware identity and health evidence.
//! No filesystem label, `dev_t`, boot ID or path is a persistent identity.
use super::super::model::{EndpointIdentity, NamePolicy};
use super::{Capabilities, Capability, EndpointError, NativeEvidence, digest, readable_directory};
use cap_fs_ext::OpenOptionsFollowExt;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;

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
struct CloneCandidate {
    uuid: Option<[u8; 16]>,
    pin: Arc<super::Pin>,
    path: PathBuf,
    mount_id: u64,
    mount_device: u64,
    device: u64,
}

impl CloneCandidate {
    fn validate(&self) -> Result<(), EndpointError> {
        // Inode ancestry alone cannot detect a replacement bind mount of the
        // same inode. Reopen the path and check its mount ID as well, retaining
        // the original pin so its kernel device cannot be recycled meanwhile.
        self.pin.validate()?;
        validate_clone_descriptor(&self.pin.file, self.mount_id, self.device)?;
        let current = super::Pin::open(&self.path)?;
        validate_clone_descriptor(&current.file, self.mount_id, self.device)?;
        if !super::same_object(&self.pin.metadata, &current.metadata) {
            return Err(EndpointError::IdentityChanged);
        }
        Ok(())
    }
}

fn validate_clone_descriptor(
    file: &File,
    expected_mount: u64,
    expected_device: u64,
) -> Result<(), EndpointError> {
    if mount_id(file)? != expected_mount || file.metadata()?.dev() != expected_device {
        return Err(EndpointError::IdentityChanged);
    }
    Ok(())
}

fn kernel_device(mount: &Mount) -> Result<u64, EndpointError> {
    let (major, minor) = mount
        .device
        .split_once(':')
        .ok_or(EndpointError::IdentityChanged)?;
    if mount.id == 0
        || [major, minor]
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(EndpointError::IdentityChanged);
    }
    Ok(libc::makedev(
        major.parse().map_err(|_| EndpointError::IdentityChanged)?,
        minor.parse().map_err(|_| EndpointError::IdentityChanged)?,
    ))
}

fn inspect_clone_candidate(
    mount: &Mount,
    query_uuid: impl FnOnce(&File) -> Result<[u8; 16], EndpointError>,
) -> Result<CloneCandidate, EndpointError> {
    let mount_device = kernel_device(mount)?;
    let pin = super::Pin::open(&mount.path)?;
    let device = pin.file.metadata()?.dev();
    let mut candidate = CloneCandidate {
        uuid: None,
        pin,
        path: mount.path.clone(),
        mount_id: mount.id,
        mount_device,
        device,
    };
    // Once a pin is available, its mount/path identity must be checked before
    // a readable-open or UUID-query denial can be classified as deferrable.
    candidate
        .validate()
        .map_err(|_| EndpointError::IdentityChanged)?;
    let file = match readable_directory(&candidate.pin.file) {
        Ok(file) => file,
        Err(EndpointError::PermissionDenied) => {
            candidate
                .validate()
                .map_err(|_| EndpointError::IdentityChanged)?;
            return Ok(candidate);
        }
        Err(error) => return Err(error),
    };
    validate_clone_descriptor(&file, mount.id, device)?;
    if !super::same_object(&candidate.pin.metadata, &super::metadata(&file)?) {
        return Err(EndpointError::IdentityChanged);
    }
    let uuid = query_uuid(&file);
    // Validate the available evidence even when the query was denied. A race
    // is IdentityChanged, never a denial another readable alias may cover.
    validate_clone_descriptor(&file, mount.id, device)
        .and_then(|()| candidate.validate())
        .map_err(|_| EndpointError::IdentityChanged)?;
    candidate.uuid = match uuid {
        Ok(uuid) => Some(uuid),
        Err(EndpointError::PermissionDenied) => None,
        Err(error) => return Err(error),
    };
    Ok(candidate)
}

fn reject_mounted_clones(
    volume: &[u8; 16],
    current: &Mount,
    mounts: &[Mount],
    mut inspect_candidate: impl FnMut(&Mount) -> Result<[u8; 16], EndpointError>,
) -> Result<(), EndpointError> {
    let current_device = kernel_device(current).map_err(|_| EndpointError::AmbiguousIdentity)?;
    // dev_t groups aliases only during this bounded mount observation. Native
    // UUID evidence, never the filesystem name or device number, qualifies each
    // distinct device. Keep unresolved groups even when others were verified.
    let mut devices = BTreeMap::new();
    for other in mounts {
        if other.kernel_type != current.kernel_type {
            continue;
        }
        let device = kernel_device(other).map_err(|_| EndpointError::AmbiguousIdentity)?;
        if device == current_device {
            continue;
        }
        let verified = devices.entry(device).or_insert(None);
        match inspect_candidate(other) {
            Ok(uuid) => {
                super::reject_clone_candidate(volume, Ok(uuid))?;
                if verified.is_some_and(|previous| previous != uuid) {
                    return Err(EndpointError::AmbiguousIdentity);
                }
                *verified = Some(uuid);
            }
            Err(EndpointError::PermissionDenied) => {}
            Err(_) => return Err(EndpointError::AmbiguousIdentity),
        }
    }
    if devices.values().any(Option::is_none) {
        return Err(EndpointError::AmbiguousIdentity);
    }
    Ok(())
}

fn inspect_mounted_clones(
    volume: &[u8; 16],
    current: &Mount,
    observed_mounts: &[Mount],
    mut query_uuid: impl FnMut(&Mount, &File) -> Result<[u8; 16], EndpointError>,
) -> Result<(), EndpointError> {
    let mut candidates = Vec::new();
    reject_mounted_clones(volume, current, observed_mounts, |mount| {
        let candidate = inspect_clone_candidate(mount, |file| query_uuid(mount, file))?;
        let uuid = candidate.uuid.ok_or(EndpointError::PermissionDenied);
        candidates.push(candidate);
        uuid
    })?;
    if candidates
        .iter()
        .any(|candidate| candidate.device != candidate.mount_device)
    {
        // Inode st_dev need not equal the superblock device in mountinfo (for
        // example, Btrfs subvolumes). While every candidate mount remains
        // pinned, bind its original row to one fresh, bounded kernel snapshot.
        // The native UUID still supplies identity; no filesystem-name exception
        // qualifies a candidate. Inspect the whole relevant row set so a newly
        // observed device cannot silently inherit another device's evidence.
        let refreshed = mounts().map_err(|_| EndpointError::AmbiguousIdentity)?;
        let rows = |mounts: &[Mount]| {
            mounts
                .iter()
                .filter(|mount| mount.kernel_type == current.kernel_type)
                .map(|mount| {
                    (
                        mount.id,
                        (
                            mount.device.clone(),
                            mount.path.clone(),
                            mount.kernel_type.clone(),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        if rows(observed_mounts) != rows(&refreshed) {
            return Err(EndpointError::AmbiguousIdentity);
        }
    }
    // Keep every available mount pin, including denied aliases without UUID
    // evidence, and revalidate after later aliases have been observed.
    for candidate in &candidates {
        candidate
            .validate()
            .map_err(|_| EndpointError::AmbiguousIdentity)?;
    }
    Ok(())
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
    inspect_mounted_clones(&volume, current, &mounts, |_, file| clone_uuid(file))?;
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

    fn mount(id: u64, device: &str) -> Mount {
        Mount {
            id,
            device: device.to_owned(),
            path: PathBuf::from(format!("/candidate-{id}")),
            readonly: false,
            kernel_type: "ext4".to_owned(),
            unsafe_options: false,
        }
    }

    fn check_candidates(
        candidates: &[(&str, Result<[u8; 16], EndpointError>)],
    ) -> Result<(), EndpointError> {
        let current = mount(1, "7:0");
        let mounts: Vec<_> = candidates
            .iter()
            .enumerate()
            .map(|(index, (device, _))| mount(index as u64 + 2, device))
            .collect();
        // Only the native observation boundary is substituted: the production
        // selector and aggregation decide which independently observed devices
        // are safe. Each mount receives its own result, not a device default.
        reject_mounted_clones(&[1; 16], &current, &mounts, |mount| {
            candidates[(mount.id - 2) as usize].1
        })
    }

    #[test]
    fn clone_alias_readable_device_allows_inaccessible_later_alias() {
        assert_eq!(
            check_candidates(&[
                ("8:17", Ok([2; 16])),
                ("8:17", Err(EndpointError::PermissionDenied)),
            ]),
            Ok(())
        );
    }

    #[test]
    fn clone_alias_inaccessible_first_alias_allows_readable_device() {
        assert_eq!(
            check_candidates(&[
                ("8:17", Err(EndpointError::PermissionDenied)),
                ("8:17", Ok([2; 16])),
            ]),
            Ok(())
        );
    }

    #[test]
    fn clone_alias_every_alias_inaccessible_remains_ambiguous() {
        assert_eq!(
            check_candidates(&[
                ("8:17", Err(EndpointError::PermissionDenied)),
                ("8:17", Err(EndpointError::PermissionDenied)),
            ]),
            Err(EndpointError::AmbiguousIdentity)
        );
    }

    #[test]
    fn clone_alias_verified_device_cannot_cover_an_unobserved_device() {
        assert_eq!(
            check_candidates(&[
                ("8:17", Ok([2; 16])),
                ("8:18", Err(EndpointError::PermissionDenied)),
                ("8:17", Ok([2; 16])),
            ]),
            Err(EndpointError::AmbiguousIdentity)
        );
    }

    #[test]
    fn clone_alias_identical_uuid_on_other_device_remains_ambiguous() {
        assert_eq!(
            check_candidates(&[("8:17", Ok([2; 16])), ("8:18", Ok([1; 16])),]),
            Err(EndpointError::AmbiguousIdentity)
        );
    }

    #[test]
    fn clone_alias_every_independent_device_requires_its_own_uuid() {
        assert_eq!(
            check_candidates(&[
                ("8:17", Ok([2; 16])),
                ("8:18", Ok([3; 16])),
                ("8:19", Ok([4; 16])),
            ]),
            Ok(())
        );
    }

    #[test]
    fn clone_alias_raced_or_unqualified_alias_cannot_be_masked() {
        for error in [
            EndpointError::IdentityChanged,
            EndpointError::Offline,
            EndpointError::Unsupported,
            EndpointError::Unstable,
            EndpointError::LimitExceeded,
        ] {
            assert_eq!(
                check_candidates(&[("8:17", Ok([2; 16])), ("8:17", Err(error))]),
                Err(EndpointError::AmbiguousIdentity),
                "verified alias must not mask {error:?}"
            );
        }
    }

    #[test]
    fn clone_alias_changed_uuid_within_one_device_is_ambiguous() {
        assert_eq!(
            check_candidates(&[("8:17", Ok([2; 16])), ("8:17", Ok([3; 16]))]),
            Err(EndpointError::AmbiguousIdentity)
        );
    }

    #[test]
    fn clone_alias_malformed_device_cannot_supply_identity() {
        for device in [
            "not-a-device",
            "8:",
            ":17",
            "+8:17",
            "8:17:1",
            "8:4294967296",
        ] {
            assert_eq!(
                check_candidates(&[(device, Ok([2; 16]))]),
                Err(EndpointError::AmbiguousIdentity),
                "malformed kernel device: {device}"
            );
        }
    }

    #[test]
    fn clone_alias_changed_descriptor_device_is_not_verified() {
        let root = tempfile::tempdir().unwrap();
        let file = File::open(root.path()).unwrap();
        let mut candidate = mount(mount_id(&file).unwrap(), "4294967295:4294967295");
        candidate.path = root.path().to_path_buf();
        assert_eq!(
            inspect_mounted_clones(&[1; 16], &mount(1, "7:0"), &[candidate], |_, _| Ok([2; 16])),
            Err(EndpointError::AmbiguousIdentity)
        );
    }

    #[test]
    fn clone_alias_changed_mount_id_is_not_verified() {
        let root = tempfile::tempdir().unwrap();
        let file = File::open(root.path()).unwrap();
        let device = file.metadata().unwrap().dev();
        let mut candidate = mount(
            mount_id(&file).unwrap() + 1,
            &format!("{}:{}", libc::major(device), libc::minor(device)),
        );
        candidate.path = root.path().to_path_buf();
        assert_eq!(
            inspect_clone_candidate(&candidate, |_| Ok([2; 16])).map(|candidate| candidate.uuid),
            Err(EndpointError::IdentityChanged)
        );
    }

    #[test]
    fn clone_alias_changed_inode_pin_is_not_verified() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("candidate");
        std::fs::create_dir(&path).unwrap();
        let file = File::open(&path).unwrap();
        let device = file.metadata().unwrap().dev();
        let mut candidate = mount(
            mount_id(&file).unwrap(),
            &format!("{}:{}", libc::major(device), libc::minor(device)),
        );
        candidate.path = path.clone();
        assert_eq!(
            inspect_clone_candidate(&candidate, |_| {
                std::fs::rename(&path, root.path().join("old-candidate")).unwrap();
                std::fs::create_dir(&path).unwrap();
                Ok([2; 16])
            })
            .map(|candidate| candidate.uuid),
            Err(EndpointError::IdentityChanged)
        );
    }

    #[test]
    fn clone_alias_earlier_pin_is_revalidated_after_later_query() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let file = File::open(&first).unwrap();
        let device = file.metadata().unwrap().dev();
        let device_text = format!("{}:{}", libc::major(device), libc::minor(device));
        let mut first_mount = mount(mount_id(&file).unwrap(), &device_text);
        first_mount.path = first.clone();
        let mut second_mount = mount(mount_id(&file).unwrap(), &device_text);
        second_mount.path = second.clone();
        // The selected device is deliberately independent of these real temp
        // directories; the candidate pin/mount checks still use actual statx.
        let current = mount(1, "4294967295:4294967295");
        let candidates = [first_mount, second_mount];
        assert_eq!(
            inspect_mounted_clones(&[1; 16], &current, &candidates, |mount, _| {
                if mount.path == second {
                    std::fs::rename(&first, root.path().join("old-first")).unwrap();
                    std::fs::create_dir(&first).unwrap();
                }
                Ok([2; 16])
            }),
            Err(EndpointError::AmbiguousIdentity)
        );
    }

    fn pinned_aliases() -> (tempfile::TempDir, Mount, [Mount; 2]) {
        let root = tempfile::tempdir().unwrap();
        let paths = [root.path().join("first"), root.path().join("second")];
        let candidates = paths.map(|path| {
            std::fs::create_dir(&path).unwrap();
            let file = File::open(&path).unwrap();
            let device = file.metadata().unwrap().dev();
            let mut candidate = mount(
                mount_id(&file).unwrap(),
                &format!("{}:{}", libc::major(device), libc::minor(device)),
            );
            candidate.path = path;
            candidate
        });
        (root, mount(1, "4294967295:4294967295"), candidates)
    }

    #[test]
    fn clone_alias_permission_denial_cannot_mask_a_race_during_query() {
        let (root, current, candidates) = pinned_aliases();
        let second = &candidates[1].path;
        assert_eq!(
            inspect_mounted_clones(&[1; 16], &current, &candidates, |mount, _| {
                if mount.path == *second {
                    std::fs::rename(second, root.path().join("old-second")).unwrap();
                    std::fs::create_dir(second).unwrap();
                    return Err(EndpointError::PermissionDenied);
                }
                Ok([2; 16])
            }),
            Err(EndpointError::AmbiguousIdentity)
        );
    }

    #[test]
    fn clone_alias_permission_denial_cannot_drop_a_pin_before_later_queries() {
        let (root, current, candidates) = pinned_aliases();
        let first = &candidates[0].path;
        assert_eq!(
            inspect_mounted_clones(&[1; 16], &current, &candidates, |mount, _| {
                if mount.path == *first {
                    return Err(EndpointError::PermissionDenied);
                }
                std::fs::rename(first, root.path().join("old-first")).unwrap();
                std::fs::create_dir(first).unwrap();
                Ok([2; 16])
            }),
            Err(EndpointError::AmbiguousIdentity)
        );
    }

    #[test]
    fn clone_alias_permission_denial_with_stable_pins_allows_an_observed_uuid() {
        for denied in [0, 1] {
            let (_root, current, candidates) = pinned_aliases();
            assert_eq!(
                inspect_mounted_clones(&[1; 16], &current, &candidates, |mount, _| {
                    if mount.path == candidates[denied].path {
                        Err(EndpointError::PermissionDenied)
                    } else {
                        Ok([2; 16])
                    }
                }),
                Ok(())
            );
        }
    }

    #[test]
    fn native_unresolved_clone_queries_and_paths_require_reselection() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("target");
        std::fs::create_dir(&selected).unwrap();
        std::os::unix::fs::symlink(&selected, root.path().join("link")).unwrap();
        let file = File::open(root.path()).unwrap();
        let device = file.metadata().unwrap().dev();
        for name in ["missing", "link"] {
            let mount = Mount {
                id: mount_id(&file).unwrap(),
                device: format!("{}:{}", libc::major(device), libc::minor(device)),
                path: root.path().join(name),
                readonly: false,
                kernel_type: "ext4".to_owned(),
                unsafe_options: false,
            };
            let candidate = inspect_clone_candidate(&mount, clone_uuid)
                .and_then(|candidate| candidate.uuid.ok_or(EndpointError::PermissionDenied));
            assert!(candidate.is_err());
            assert_eq!(
                super::super::reject_clone_candidate(&[1; 16], candidate),
                Err(EndpointError::AmbiguousIdentity)
            );
        }
        // Native ioctl failure on a real unsupported descriptor must not count
        // as evidence of a different UUID. This is no substitute for the
        // separately coordinated same-filesystem mounted-clone fixture.
        // /dev/null can expose its devtmpfs UUID through generic FS_IOC_GETFSUUID
        // on newer kernels. An anonymous pipe has no native filesystem UUID.
        let (reader, _writer) = std::io::pipe().unwrap();
        let device = File::from(std::os::fd::OwnedFd::from(reader));
        let candidate = clone_uuid(&device);
        assert!(candidate.is_err());
        assert_eq!(
            super::super::reject_clone_candidate(&[1; 16], candidate),
            Err(EndpointError::AmbiguousIdentity)
        );
    }
}

#[cfg(test)]
mod clone_native_tests {
    use super::*;

    #[test]
    fn native_permission_denial_does_not_mask_mismatched_mount() {
        use std::os::unix::fs::DirBuilderExt;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("execute-only");
        std::fs::DirBuilder::new()
            .mode(0o100)
            .create(&path)
            .unwrap();
        let pin = super::super::Pin::open(&path)
            .expect("the native fixture must permit an O_PATH directory pin");
        let readable = readable_directory(&pin.file).map(|_| ());
        let device = pin.file.metadata().unwrap().dev();
        let candidate = Mount {
            id: mount_id(&pin.file).unwrap().checked_add(1).unwrap(),
            device: format!("{}:{}", libc::major(device), libc::minor(device)),
            path: path.clone(),
            readonly: false,
            kernel_type: "ext4".to_owned(),
            unsafe_options: false,
        };
        let result =
            inspect_clone_candidate(&candidate, |_| Ok([2; 16])).map(|candidate| candidate.uuid);
        // The fixture is empty; unlink it through its writable parent before
        // assertions, including the intentional RED assertion, can unwind.
        std::fs::remove_dir(path).unwrap();
        assert_eq!(
            readable,
            Err(EndpointError::PermissionDenied),
            "run as an unprivileged user: the native fixture must deny O_RDONLY"
        );
        assert_eq!(result, Err(EndpointError::IdentityChanged));
    }

    #[test]
    fn native_btrfs_subvolume_alias_uses_pinned_mountinfo_device() {
        let path =
            PathBuf::from(std::env::var_os("QOS_ENDPOINT_BTRFS_ALIAS_FIXTURE").expect(
                "QOS_ENDPOINT_BTRFS_ALIAS_FIXTURE must name a mounted Btrfs subvolume alias",
            ));
        let pin = super::super::Pin::open(&path).unwrap();
        let file = readable_directory(&pin.file).unwrap();
        btrfs_uuid(&file).expect("fixture must provide native single-device Btrfs identity");
        let id = mount_id(&file).unwrap();
        let observed_mounts = mounts().unwrap();
        let candidate = observed_mounts
            .iter()
            .find(|mount| mount.id == id)
            .expect("fixture mount must be present in the observed namespace");
        assert_eq!(candidate.path, path, "fixture must be the exact mount root");
        assert_ne!(
            file.metadata().unwrap().dev(),
            kernel_device(candidate).unwrap(),
            "fixture must observe distinct inode and superblock device numbers"
        );
        let current = Mount {
            id: u64::MAX,
            device: "4294967295:4294967295".to_owned(),
            path: PathBuf::from("/independent-selected-device"),
            readonly: false,
            kernel_type: candidate.kernel_type.clone(),
            unsafe_options: false,
        };
        // A nonzero native UUID must qualify this independently pinned
        // candidate. The selected UUID/device are deliberately disjoint.
        assert_eq!(
            inspect_mounted_clones(&[0; 16], &current, &observed_mounts, |_, file| clone_uuid(
                file
            )),
            Ok(())
        );
    }
}
