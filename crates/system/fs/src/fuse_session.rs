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
    let trace = |what: &str| {
        if std::env::var_os("TILO_FS_BENCH_VERBOSE").is_some() {
            eprintln!("unmount: {what}");
        }
    };
    trace("ending the connection");
    if end_connection(mount_point)? {
        trace("connection ended; joining the session");
        let result = ended(session.join());
        trace("session joined");
        result
    } else {
        trace("fuser unmounts; joining");
        let result = ended(session.umount_and_join());
        trace("fuser joined");
        result
    }
}

/// Unmounts the filesystem at `mount_point`; one still in use has its connection
/// ended and its mount detached. Returns whether it did: where this process may
/// not unmount, fuser's own unmount does it.
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
    if std::env::var_os("TILO_FS_BENCH_VERBOSE").is_some() {
        eprintln!("unmount: busy; forcing a detach");
    }
    // The mount point is never opened here: serving that open is what the session being ended
    // must do, and the thread that would wait for it is this one. Every change this daemon
    // answered is already journaled and its content committed, so nothing of its own is left in
    // the kernel to flush.
    //
    // `MNT_FORCE` makes FUSE abort the connection before the unmount, so
    // its open files fail with `ENOTCONN` and the workers' reads end.
    unmount(mount_point, UnmountFlags::FORCE | UnmountFlags::DETACH)?;
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
fn end_connection(_mount_point: &Path) -> io::Result<bool> {
    Ok(false)
}

/// The helper an unprivileged process mounts FUSE through.
pub const FUSERMOUNT: &str = "fusermount3";

/// Whether this user can mount FUSE: the device `dev_fuse` exists and
/// [`FUSERMOUNT`] is an executable on `path`.
#[must_use]
pub fn mountable(dev_fuse: &Path, path: Option<&std::ffi::OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    dev_fuse.exists()
        && path.is_some_and(|path| {
            std::env::split_paths(path).any(|dir| {
                std::fs::metadata(dir.join(FUSERMOUNT))
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
        })
}

/// A mount a crashed daemon left behind answers `ENOTCONN`; unmount it so
/// its path can be mounted again.
///
/// The mount is found in `/proc/self/mounts`, never by touching it: a stat of a mount whose
/// daemon has just died can wait for an answer that will not come.
///
/// # Errors
/// [`FUSERMOUNT`] could not remove it, and it is still there.
pub fn clear_stale_mount(at: &Path) -> io::Result<()> {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return Ok(());
    };
    let escaped = at.to_string_lossy().replace(' ', "\\040");
    let listed = mounts.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let _source = fields.next();
        let point = fields.next();
        let kind = fields.next();
        point == Some(escaped.as_str()) && kind.is_some_and(|kind| kind.starts_with("fuse"))
    });
    if !listed {
        return Ok(());
    }
    let status = std::process::Command::new(FUSERMOUNT)
        .args(["-u", "-z"])
        .arg(at)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{FUSERMOUNT} could not remove the stale mount at {}",
            at.display()
        )))
    }
}
