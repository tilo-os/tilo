//! Descriptor helpers for callers that keep custody of a directory hierarchy
//! by holding descriptors rather than paths.
use std::{
    ffi::CStr,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
};

/// Open the directory `name` directly beneath `parent`, refusing symlinks and
/// anything but a directory, and keeping the descriptor out of `exec`.
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

/// The device and inode that identify `file`, to notice a path being swapped
/// for another object between two looks.
pub(crate) fn identity(file: &File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
