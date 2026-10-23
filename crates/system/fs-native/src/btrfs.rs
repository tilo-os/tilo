//! Btrfs subvolumes and snapshots.
//!
//! Capture epochs are read-only snapshots of a room's subvolume: one atomic,
//! crash-consistent image of every file in the room, taken without pausing
//! writers.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::sys;

mod single_device;
pub use single_device::{SingleDeviceIdentity, single_device_identity};

/// `BTRFS_IOC_SNAP_CREATE_V2`: `_IOW(0x94, 23, struct btrfs_ioctl_vol_args_v2)`.
const SNAP_CREATE_V2: libc::Ioctl = 0x5000_9417;
/// `BTRFS_IOC_SUBVOL_CREATE_V2`: `_IOW(0x94, 24, struct btrfs_ioctl_vol_args_v2)`.
const SUBVOL_CREATE_V2: libc::Ioctl = 0x5000_9418;
/// `BTRFS_IOC_SNAP_DESTROY`: `_IOW(0x94, 15, struct btrfs_ioctl_vol_args)`.
const SNAP_DESTROY: libc::Ioctl = 0x5000_940f;
/// `BTRFS_IOC_FS_INFO`: `_IOR(0x94, 31, struct btrfs_ioctl_fs_info_args)`.
const FS_INFO: libc::Ioctl = 0x8400_941f;
/// `BTRFS_IOC_SUBVOL_GETFLAGS`: `_IOR(0x94, 25, __u64)`.
const SUBVOL_GETFLAGS: libc::Ioctl = 0x8008_9419;
/// `BTRFS_IOC_SUBVOL_SETFLAGS`: `_IOW(0x94, 26, __u64)`.
const SUBVOL_SETFLAGS: libc::Ioctl = 0x4008_941a;
const SUBVOL_RDONLY: u64 = 0x2;
/// `GET_SUBVOL_INFO` reports root-item flags, not the CREATE/GETFLAGS flags.
const ROOT_SUBVOL_RDONLY: u64 = 1;
/// Inode number of every subvolume root (`BTRFS_FIRST_FREE_OBJECTID`).
const SUBVOL_ROOT_INO: u64 = 256;
const GET_SUBVOL_INFO: libc::Ioctl = 0x81f8_943c;
const SNAP_DESTROY_V2: libc::Ioctl = 0x5000_943f;
const SUBVOL_SPEC_BY_ID: u64 = 1 << 4;

#[repr(C)]
struct SubvolInfoArgs {
    treeid: u64,
    name: [u8; 256],
    parent_id: u64,
    dirid: u64,
    generation: u64,
    flags: u64,
    uuid: [u8; 16],
    parent_uuid: [u8; 16],
    received_uuid: [u8; 16],
    transactions: [u64; 4],
    times: [[u64; 2]; 4],
    reserved: [u64; 8],
}

const _: () = assert!(std::mem::size_of::<SubvolInfoArgs>() == 504);

/// Kernel identity of a held Btrfs subvolume, including snapshot provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubvolumeIdentity {
    pub filesystem: [u8; 16],
    pub id: u64,
    pub uuid: [u8; 16],
    pub parent_uuid: [u8; 16],
    pub readonly: bool,
}

/// Attests the containing subvolume of a held directory without resolving a path.
pub fn subvolume_identity(directory: BorrowedFd<'_>) -> io::Result<SubvolumeIdentity> {
    // SAFETY: both argument structures are zero-initialized kernel output buffers.
    let mut filesystem: FsInfoArgs = unsafe { std::mem::zeroed() };
    let mut info: SubvolInfoArgs = unsafe { std::mem::zeroed() };
    // SAFETY: the requests accept these exact Linux UAPI layouts.
    unsafe {
        sys::ioctl_ptr(directory, FS_INFO, &raw mut filesystem)?;
        sys::ioctl_ptr(directory, GET_SUBVOL_INFO, &raw mut info)?;
    }
    Ok(SubvolumeIdentity {
        filesystem: filesystem.fsid,
        id: info.treeid,
        uuid: info.uuid,
        parent_uuid: info.parent_uuid,
        // Unlike GETFLAGS, GET_SUBVOL_INFO also supports a HOME directory
        // that is inside its containing subvolume instead of its root.
        readonly: info.flags & ROOT_SUBVOL_RDONLY != 0,
    })
}

fn entry_name(name: &OsStr) -> io::Result<&[u8]> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(invalid("subvolume name must be one normal entry"));
    }
    Ok(bytes)
}

/// Creates a readonly snapshot through held source and private broker-parent handles.
/// The caller retains custody of the parent; this does not grant an app access to it.
pub fn snapshot_readonly_at(
    source: BorrowedFd<'_>,
    parent: BorrowedFd<'_>,
    name: &OsStr,
) -> io::Result<()> {
    if subvolume_identity(source)?.filesystem != subvolume_identity(parent)?.filesystem {
        return Err(invalid("snapshot parent must share the source filesystem"));
    }
    // SAFETY: zero is valid input; only the declared fields are populated.
    let mut args: VolArgsV2 = unsafe { std::mem::zeroed() };
    args.fd = i64::from(source.as_raw_fd());
    args.flags = SUBVOL_RDONLY;
    copy_name(&mut args.name, entry_name(name)?)?;
    // SAFETY: the request takes this exact Linux UAPI argument structure.
    unsafe { sys::ioctl_ptr(parent, SNAP_CREATE_V2, &raw mut args) }?;
    Ok(())
}

/// Deletes exactly the attested snapshot id. Never clears readonly flags or
/// recursively removes paths. Requires the broker's native deletion authority.
pub fn destroy_snapshot_id(
    parent: BorrowedFd<'_>,
    snapshot: BorrowedFd<'_>,
    expected: &SubvolumeIdentity,
) -> io::Result<()> {
    if !expected.readonly
        || expected.parent_uuid == [0; 16]
        || subvolume_identity(snapshot)? != *expected
        || subvolume_identity(parent)?.filesystem != expected.filesystem
    {
        return Err(invalid("snapshot identity or custody changed"));
    }
    // SAFETY: zero is valid input; SPEC_BY_ID selects the union's first u64.
    let mut args: VolArgsV2 = unsafe { std::mem::zeroed() };
    args.flags = SUBVOL_SPEC_BY_ID;
    args.name
        .get_mut(..8)
        .ok_or_else(|| invalid("missing subvolume id slot"))?
        .copy_from_slice(&expected.id.to_ne_bytes());
    // SAFETY: SNAP_DESTROY_V2 takes this exact Linux UAPI structure.
    unsafe { sys::ioctl_ptr(parent, SNAP_DESTROY_V2, &raw mut args) }?;
    Ok(())
}

#[repr(C)]
struct VolArgsV2 {
    fd: i64,
    transid: u64,
    flags: u64,
    unused: [u64; 4],
    name: [u8; 4040],
}

#[repr(C)]
struct VolArgs {
    fd: i64,
    name: [u8; 4088],
}

#[repr(C)]
struct FsInfoArgs {
    max_id: u64,
    num_devices: u64,
    fsid: [u8; 16],
    nodesize: u32,
    sectorsize: u32,
    clone_alignment: u32,
    csum_type: u16,
    csum_size: u16,
    flags: u64,
    generation: u64,
    metadata_uuid: [u8; 16],
    reserved: [u8; 944],
}

const _: () = assert!(std::mem::size_of::<VolArgsV2>() == 4096);
const _: () = assert!(std::mem::size_of::<VolArgs>() == 4096);
const _: () = assert!(std::mem::size_of::<FsInfoArgs>() == 1024);

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// Splits `path` into its parent directory and a single, valid entry name.
fn parent_and_name(path: &Path) -> io::Result<(&Path, &[u8])> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("subvolume path has no parent"))?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let name = path
        .file_name()
        .ok_or_else(|| invalid("subvolume path has no name"))?
        .as_bytes();
    Ok((parent, name))
}

fn copy_name(dst: &mut [u8], name: &[u8]) -> io::Result<()> {
    // Leave room for the terminating NUL.
    if name.len() >= dst.len() {
        return Err(invalid("subvolume name too long"));
    }
    dst.get_mut(..name.len())
        .ok_or_else(|| invalid("subvolume name too long"))?
        .copy_from_slice(name);
    Ok(())
}

pub fn is_btrfs(path: &Path) -> io::Result<bool> {
    Ok(sys::statfs_type(path)? == sys::BTRFS_SUPER_MAGIC)
}

pub fn is_subvolume(path: &Path) -> io::Result<bool> {
    Ok(is_btrfs(path)? && fs::symlink_metadata(path)?.ino() == SUBVOL_ROOT_INO)
}

/// The filesystem UUID of the Btrfs volume holding `path`. Every subvolume
/// of one filesystem reports the same UUID, which is how a dedicated store
/// volume is told apart from a subvolume of the root filesystem.
pub fn filesystem_uuid(path: &Path) -> io::Result<[u8; 16]> {
    let dir = sys::open_dir(path)?;
    // SAFETY: an all-zero `FsInfoArgs` is valid input for BTRFS_IOC_FS_INFO.
    let mut args: FsInfoArgs = unsafe { std::mem::zeroed() };
    // SAFETY: FS_INFO takes a pointer to `struct btrfs_ioctl_fs_info_args`.
    unsafe { sys::ioctl_ptr(dir.as_fd(), FS_INFO, &raw mut args) }?;
    Ok(args.fsid)
}

/// Creates an empty subvolume at `path`.
pub fn create_subvolume(path: &Path) -> io::Result<()> {
    let (parent, name) = parent_and_name(path)?;
    let dir = sys::open_dir(parent)?;
    // SAFETY: an all-zero argument block is valid; the name is set below.
    let mut args: VolArgsV2 = unsafe { std::mem::zeroed() };
    copy_name(&mut args.name, name)?;
    // SAFETY: SUBVOL_CREATE_V2 takes a pointer to `btrfs_ioctl_vol_args_v2`.
    unsafe { sys::ioctl_ptr(dir.as_fd(), SUBVOL_CREATE_V2, &raw mut args) }?;
    Ok(())
}

/// Snapshots the subvolume at `source` to `dest` in one atomic step.
///
/// Btrfs flushes the source's delayed allocation first, so every write that
/// completed before the call is in the snapshot and none after it is. This
/// is the crash image that a capture epoch publishes.
pub fn snapshot(source: &Path, dest: &Path, readonly: bool) -> io::Result<()> {
    let src = sys::open_dir(source)?;
    let (parent, name) = parent_and_name(dest)?;
    let dir = sys::open_dir(parent)?;
    // SAFETY: an all-zero argument block is valid; fields are set below.
    let mut args: VolArgsV2 = unsafe { std::mem::zeroed() };
    args.fd = i64::from(std::os::fd::AsRawFd::as_raw_fd(&src));
    if readonly {
        args.flags = SUBVOL_RDONLY;
    }
    copy_name(&mut args.name, name)?;
    // SAFETY: SNAP_CREATE_V2 takes a pointer to `btrfs_ioctl_vol_args_v2`.
    unsafe { sys::ioctl_ptr(dir.as_fd(), SNAP_CREATE_V2, &raw mut args) }?;
    Ok(())
}

/// Sets or clears the read-only flag of a subvolume. Allowed for the owner.
pub fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let dir = sys::open_dir(path)?;
    let mut flags: u64 = 0;
    // SAFETY: GETFLAGS writes one u64.
    unsafe { sys::ioctl_ptr(dir.as_fd(), SUBVOL_GETFLAGS, &raw mut flags) }?;
    flags = if readonly {
        flags | SUBVOL_RDONLY
    } else {
        flags & !SUBVOL_RDONLY
    };
    // SAFETY: SETFLAGS reads one u64.
    unsafe { sys::ioctl_ptr(dir.as_fd(), SUBVOL_SETFLAGS, &raw mut flags) }?;
    Ok(())
}

fn destroy(path: &Path) -> io::Result<()> {
    let (parent, name) = parent_and_name(path)?;
    let dir = sys::open_dir(parent)?;
    // SAFETY: an all-zero argument block is valid; the name is set below.
    let mut args: VolArgs = unsafe { std::mem::zeroed() };
    copy_name(&mut args.name, name)?;
    // SAFETY: SNAP_DESTROY takes a pointer to `btrfs_ioctl_vol_args`.
    unsafe { sys::ioctl_ptr(dir.as_fd(), SNAP_DESTROY, &raw mut args) }?;
    Ok(())
}

/// Deletes the subvolume at `path`.
///
/// `BTRFS_IOC_SNAP_DESTROY` needs `CAP_SYS_ADMIN`, or a volume mounted with
/// `user_subvol_rm_allowed` and a writable subvolume (a read-only snapshot
/// gives `EROFS` until its flag is cleared). Without either, the owner can
/// still make the subvolume writable, empty it and `rmdir` it, which is
/// what the last fallback does. That is slow for large trees, so the store
/// volume is mounted with `user_subvol_rm_allowed`.
pub fn delete_subvolume(path: &Path) -> io::Result<()> {
    match destroy(path) {
        Ok(()) => Ok(()),
        Err(e) if matches!(e.raw_os_error(), Some(libc::EPERM | libc::EROFS)) => {
            set_readonly(path, false)?;
            if destroy(path).is_ok() {
                return Ok(());
            }
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let p = entry.path();
                if entry.file_type()?.is_dir() {
                    if is_subvolume(&p)? {
                        delete_subvolume(&p)?;
                    } else {
                        fs::remove_dir_all(&p)?;
                    }
                } else {
                    fs::remove_file(&p)?;
                }
            }
            fs::remove_dir(path)
        }
        Err(e) => Err(e),
    }
}
