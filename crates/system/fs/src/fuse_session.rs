//! How a FUSE session's end is read.
//!
//! Unmounting aborts the kernel connection, and a session thread blocked in
//! a read of the FUSE device can wake inside that abort: the kernel has
//! already disconnected the device's processing queue while a request (such
//! as the asynchronous `RELEASE` of a just-closed file) still sits in the
//! input queue. That read fails with `ECONNABORTED` even though the session
//! never negotiated `FUSE_ABORT_ERROR` (Linux `fuse_dev_do_read`), where the
//! usual unmount answer is `ENODEV`. fuser 0.18 ends the loop cleanly only
//! on `ENODEV` and hands the `ECONNABORTED` back as the session's error, so
//! an ordinary unmount intermittently reported failure. Either errno means
//! the connection is gone, which is how every session ends.

use std::io;
use std::path::Path;

/// The session's result with an aborted connection read as a clean end.
pub fn ended(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.raw_os_error() == Some(libc::ECONNABORTED) => Ok(()),
        other => other,
    }
}

/// Unmounts `session`, mounted at `mount_point`, and waits for its thread,
/// reading an aborted connection as the end it is.
///
/// fuser cannot be left to unmount by itself. A session mounted with
/// `AutoUnmount` only closes its socket to `fusermount3`, and `fusermount3`
/// unmounts only a filesystem whose daemon has died: this one is still
/// serving, so the mount stays, the session's workers keep reading the
/// device and the join never returns (a stopping `fs.service` hung until
/// systemd aborted it). And a mount that is still in use (an open file, a
/// shell's working directory) cannot be unmounted at all; detaching it
/// leaves the connection serving its open files, and the join waits for
/// every one of them to close.
pub fn unmount_and_join(session: fuser::BackgroundSession, mount_point: &Path) -> io::Result<()> {
    if end_connection(mount_point)? {
        ended(session.join())
    } else {
        ended(session.umount_and_join())
    }
}

/// Unmounts the filesystem at `mount_point`; one still in use has its
/// writes flushed, then its connection ended and its mount detached, as
/// the daemon's exit would. Returns whether it did: where this process may
/// not unmount (an unprivileged mount, which `fusermount3 -u` ends) or
/// nothing is mounted any more, fuser's own unmount is left to do it.
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
