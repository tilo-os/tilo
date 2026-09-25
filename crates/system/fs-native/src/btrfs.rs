//! Btrfs subvolumes and snapshots.
//!
//! Capture epochs are read-only snapshots of a room's subvolume: one atomic,
//! crash-consistent image of every file in the room, taken without pausing
//! writers.

use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::sys;

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
/// `BTRFS_SUBVOL_RDONLY`.
const SUBVOL_RDONLY: u64 = 0x2;
/// Inode number of every subvolume root (`BTRFS_FIRST_FREE_OBJECTID`).
const SUBVOL_ROOT_INO: u64 = 256;

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

/// Whether `path` lives on Btrfs.
pub fn is_btrfs(path: &Path) -> io::Result<bool> {
    Ok(sys::statfs_type(path)? == sys::BTRFS_SUPER_MAGIC)
}

/// Whether `path` is the root of a Btrfs subvolume.
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
