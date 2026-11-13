//! How a FUSE session's end is read. Unmounting aborts the connection, and a
//! session thread woken inside that abort fails with `ECONNABORTED` even though
//! the session never negotiated `FUSE_ABORT_ERROR`; fuser 0.18 reports that as the
//! session's error, so either errno means the connection is gone.

use std::io;
use std::path::Path;

pub fn ended(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.raw_os_error() == Some(libc::ECONNABORTED) => Ok(()),
        other => other,
    }
}

/// Unmounts `session`, mounted at `mount_point`, and waits for its thread, reading
/// an aborted connection as the end it is.
///
/// fuser cannot be left to unmount by itself: a session mounted with `AutoUnmount`
/// only closes its socket to `fusermount3`, which unmounts only a filesystem whose
/// daemon has died. A mount still in use cannot be unmounted at all; detaching it
/// leaves the connection serving its open files until each one closes.
pub fn unmount_and_join(session: fuser::BackgroundSession, mount_point: &Path) -> io::Result<()> {
    if end_connection(mount_point)? {
        ended(session.join())
    } else {
        ended(session.umount_and_join())
    }
}

/// Unmounts the filesystem at `mount_point`; one still in use has its writes
/// flushed, then its connection ended and its mount detached. Returns whether it
/// did: where this process may not unmount, fuser's own unmount does it.
#[cfg(target_os = "linux")]
fn end_connection(mount_point: &Path) -> io::Result<bool> {
    use rustix::io::Errno;
    use rustix::mount::{UnmountFlags, unmount};

    match unmount(mount_point, UnmountFlags::empty()) {
        Ok(()) => return Ok(true),
        Err(Errno::PERM | Errno::INVAL) => return Ok(false),
        Err(Errno::BUSY) => {}
        Err(error) => return Err(error.into()),
    }
    // Writes the kernel still holds (pages of a file mapped shared) reach
    // the daemon now; ending the connection would drop them.
    if let Ok(root) = std::fs::File::open(mount_point) {
        let _ = rustix::fs::syncfs(&root);
    }
    // `MNT_FORCE` makes FUSE abort the connection before the unmount, so
    // its open files fail with `ENOTCONN` and the workers' reads end.
    unmount(mount_point, UnmountFlags::FORCE | UnmountFlags::DETACH)?;
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
fn end_connection(_mount_point: &Path) -> io::Result<bool> {
    Ok(false)
}
