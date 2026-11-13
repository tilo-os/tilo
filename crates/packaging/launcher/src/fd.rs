//! Descriptor helpers for callers that keep custody of a directory hierarchy by holding
//! descriptors rather than paths.
use rustix::fs::{AtFlags, Statx, StatxFlags};
use std::{
    ffi::CStr,
    fs::File,
    io,
    os::{
        fd::{AsFd, AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
};

/// Open the directory `name` directly beneath `parent`, refusing symlinks and anything but a directory,
/// and keeping the descriptor out of `exec`.
pub(crate) fn open_beneath(parent: &File, name: &CStr) -> io::Result<File> {
    // SAFETY: both descriptors and the NUL-terminated name stay live for the call.
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new descriptor whose ownership moves here once.
    Ok(unsafe { File::from_raw_fd(raw) })
}

/// The device and inode that identify `file`, to notice a path being swapped for another object between
/// two looks.
pub(crate) fn identity(file: &File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

/// Extended metadata of the held object itself: the empty path names `file`, so no name is resolved and
/// nothing can be swapped in between.
pub(crate) fn statx(file: &File, flags: AtFlags, mask: StatxFlags) -> io::Result<Statx> {
    rustix::fs::statx(file.as_fd(), c"", flags | AtFlags::EMPTY_PATH, mask).map_err(io::Error::from)
}

/// The mount the held object lives on.
pub(crate) fn mount_id(file: &File, flags: AtFlags, unavailable: &'static str) -> io::Result<u64> {
    let facts = statx(file, flags, StatxFlags::MNT_ID)?;
    if facts.stx_mask & StatxFlags::MNT_ID.bits() == 0 || facts.stx_mnt_id == 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, unavailable));
    }
    Ok(facts.stx_mnt_id)
}
