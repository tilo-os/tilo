//! Physical membership of an actual held Btrfs mount, including readonly mounts.
use super::{FS_INFO, FsInfoArgs};
use crate::sys;
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, Read},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
};

/// Kernel-observed single physical member of a held Btrfs filesystem.
/// UUIDs alone cannot distinguish a cloned filesystem. The device number is
/// read through sysfs links created from the mounted filesystem's actual bdev.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SingleDeviceIdentity {
    pub filesystem: [u8; 16],
    pub device_id: u64,
    pub rdev: u64,
}

#[derive(PartialEq, Eq)]
struct Filesystem {
    uuid: [u8; 16],
    device_id: u64,
}

fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn filesystem(directory: BorrowedFd<'_>) -> io::Result<Filesystem> {
    // SAFETY: zeroed FS_INFO is valid input; the exact 1024-byte UAPI layout is
    // asserted by the parent module and remains live throughout the ioctl.
    let mut args: FsInfoArgs = unsafe { std::mem::zeroed() };
    unsafe { sys::ioctl_ptr(directory, FS_INFO, &raw mut args) }?;
    if args.num_devices != 1 || args.max_id == 0 || args.fsid == [0; 16] {
        return Err(refused("Btrfs requires one nonmissing physical member"));
    }
    Ok(Filesystem {
        uuid: args.fsid,
        device_id: args.max_id,
    })
}

fn kernel_file(file: File) -> io::Result<File> {
    let metadata = file.metadata()?;
    let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor is held and the output has the native statfs size.
    sys::cvt(unsafe { libc::fstatfs(file.as_raw_fd(), status.as_mut_ptr()) })?;
    // SAFETY: successful fstatfs initialized the output.
    if unsafe { status.assume_init() }.f_type != libc::SYSFS_MAGIC || metadata.uid() != 0 {
        return Err(refused("Btrfs member observation requires kernel sysfs"));
    }
    Ok(file)
}

fn child(parent: &File, name: &std::ffi::OsStr, flags: libc::c_int) -> io::Result<File> {
    // Every name is either a fixed component, a derived UUID or an entry from
    // a held kernel directory; none is supplied by a source request.
    let path = sys::cstring(Path::new(name))?;
    // SAFETY: parent remains held, path is terminated, and no creation is used.
    let raw = sys::cvt(unsafe {
        libc::openat(parent.as_raw_fd(), path.as_ptr(), flags | libc::O_CLOEXEC)
    })?;
    // SAFETY: successful openat returned one newly owned descriptor.
    kernel_file(unsafe { File::from_raw_fd(raw) })
}

fn directory(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    child(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )
}

fn one_entry(directory: &File) -> io::Result<OsString> {
    // /proc routes to this still-held descriptor, never a cached kernel device
    // pathname. Only names are consumed; each object is opened relative to it.
    let mut entries = fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
    let name = entries
        .next()
        .ok_or_else(|| refused("Btrfs membership is empty"))??
        .file_name();
    if entries.next().transpose()?.is_some() {
        return Err(refused("Btrfs membership is not exactly one device"));
    }
    Ok(name)
}

fn attribute(directory: &File, name: &'static str) -> io::Result<String> {
    let file = child(directory, name.as_ref(), libc::O_RDONLY | libc::O_NOFOLLOW)?;
    let mut bytes = Vec::new();
    file.take(65).read_to_end(&mut bytes)?;
    if bytes.len() > 64 {
        return Err(refused("Btrfs kernel member attribute exceeds bound"));
    }
    let text = String::from_utf8(bytes).map_err(io::Error::other)?;
    Ok(text.strip_suffix('\n').unwrap_or(&text).to_owned())
}

fn decimal(text: &str) -> io::Result<u64> {
    if text.is_empty() || !text.bytes().all(|value| value.is_ascii_digit()) {
        return Err(refused("Btrfs kernel member number is malformed"));
    }
    text.parse().map_err(io::Error::other)
}

fn device_number(text: &str) -> io::Result<u64> {
    let (major, minor) = text
        .split_once(':')
        .ok_or_else(|| refused("Btrfs device number is malformed"))?;
    let major = u32::try_from(decimal(major)?).map_err(io::Error::other)?;
    let minor = u32::try_from(decimal(minor)?).map_err(io::Error::other)?;
    let device = libc::makedev(major, minor);
    if major == 0 || libc::major(device) != major || libc::minor(device) != minor {
        return Err(refused("Btrfs physical device number is invalid"));
    }
    Ok(device)
}

fn filesystem_name(uuid: [u8; 16]) -> String {
    let mut name = String::with_capacity(36);
    for (index, byte) in uuid.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            name.push('-');
        }
        name.push_str(&hex::encode([byte]));
    }
    name
}

#[derive(PartialEq, Eq)]
struct Member {
    info_identity: (u64, u64),
    block_identity: (u64, u64),
    rdev: u64,
}

fn member(root: &File, filesystem: &Filesystem) -> io::Result<Member> {
    let info = directory(root, "devinfo".as_ref())?;
    let name = one_entry(&info)?;
    if decimal(
        name.to_str()
            .ok_or_else(|| refused("Btrfs member ID is not decimal"))?,
    )? != filesystem.device_id
    {
        return Err(refused("Btrfs member ID differs from held filesystem"));
    }
    let info = directory(&info, &name)?;
    if attribute(&info, "missing")? != "0"
        || attribute(&info, "replace_target")? != "0"
        || attribute(&info, "in_fs_metadata")? != "1"
        || attribute(&info, "fsid")? != filesystem_name(filesystem.uuid)
    {
        return Err(refused(
            "Btrfs member is missing, seeded, replaced or ambiguous",
        ));
    }
    let devices = directory(root, "devices".as_ref())?;
    let name = one_entry(&devices)?;
    // This one kernel sysfs link is intentionally followed: Btrfs creates it
    // from device->bdev's kobject, rather than the cached DEV_INFO.path string.
    let block = child(&devices, &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
    let rdev = device_number(&attribute(&block, "dev")?)?;
    let info = info.metadata()?;
    let block = block.metadata()?;
    Ok(Member {
        info_identity: (info.dev(), info.ino()),
        block_identity: (block.dev(), block.ino()),
        rdev,
    })
}

/// Observe the sole actual block member of a readable held Btrfs directory.
///
/// This supports readonly mounts without requiring a writable member. It does
/// not grant source authority: callers must match rdev to their held block FD,
/// filesystem UUID to reviewed volume facts, and retain the actual mount ID and
/// namespace. Full-root callers separately require `subvolume_identity().id == 5`.
/// A missing or hidden kernel sysfs observation is an error, never a fallback to
/// a UUID or `DEV_INFO` pathname. The membership and `FS_INFO` are read twice.
pub fn single_device_identity(directory: BorrowedFd<'_>) -> io::Result<SingleDeviceIdentity> {
    let held = File::from(directory.try_clone_to_owned()?);
    if !held.metadata()?.is_dir() {
        return Err(refused(
            "Held Btrfs member observation requires a directory",
        ));
    }
    let directory = held.as_fd();
    let before = filesystem(directory)?;
    let sysfs = kernel_file(File::from(sys::open(
        Path::new("/sys"),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )?))?;
    let fs = self::directory(&sysfs, "fs".as_ref())?;
    let btrfs = self::directory(&fs, "btrfs".as_ref())?;
    let root = self::directory(&btrfs, filesystem_name(before.uuid).as_ref())?;
    let first = member(&root, &before)?;
    let after = filesystem(directory)?;
    let second = member(&root, &after)?;
    let final_info = filesystem(directory)?;
    if before != after || after != final_info || first != second {
        return Err(refused(
            "Btrfs physical membership changed during observation",
        ));
    }
    Ok(SingleDeviceIdentity {
        filesystem: before.uuid,
        device_id: before.device_id,
        rdev: first.rdev,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_name_matches_kernel_uuid_format() {
        assert_eq!(
            filesystem_name([0; 16]),
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            filesystem_name([
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
                0x32, 0x10,
            ]),
            "01234567-89ab-cdef-fedc-ba9876543210"
        );
    }

    #[test]
    fn kernel_numbers_reject_signs_spaces_overflow_and_anonymous_devices() {
        for text in ["", "-1", "+1", " 1", "1 ", "1\n", "18446744073709551616"] {
            assert!(decimal(text).is_err(), "{text:?}");
        }
        for text in [
            "",
            "1",
            "1:2:3",
            "0:35",
            "1:-1",
            "4294967296:0",
            "1:4294967296",
        ] {
            assert!(device_number(text).is_err(), "{text:?}");
        }
        let actual = device_number("259:7").unwrap();
        assert_eq!((libc::major(actual), libc::minor(actual)), (259, 7));
    }
}
