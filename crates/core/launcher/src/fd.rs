//! Descriptor helpers for callers that keep owning a directory hierarchy by holding
//! descriptors rather than paths, and the small kernel-descriptor observations they need.
use std::{
    ffi::CStr,
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
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

/// Whether a pidfd has signalled: `POLLIN`. A poll failure or `POLLERR`/`POLLNVAL` is refused
/// rather than read as a live process.
pub(crate) fn pidfd_exited(fd: &impl AsRawFd) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&raw mut descriptor, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(descriptor.revents & libc::POLLIN != 0)
}
