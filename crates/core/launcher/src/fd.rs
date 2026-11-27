//! The small kernel-descriptor observations the launcher needs.
use std::{io, os::fd::AsRawFd};

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
