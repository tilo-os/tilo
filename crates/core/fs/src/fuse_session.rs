//! How a FUSE session's end is read. Unmounting aborts the connection, and a
//! session thread woken inside that abort fails with `ECONNABORTED` even though
//! the session never negotiated `FUSE_ABORT_ERROR`; fuser 0.18 reports that as the
//! session's error, so either errno means the connection is gone.

use std::io;
use std::path::{Path, PathBuf};

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
/// ended and its mount detached. Returns whether it did: a mount this process does not find
/// is left to fuser's own unmount.
///
/// fuser's own unmount, where this process may not unmount, is `fusermount3 -u -z`, a lazy
/// detach: a folder still in use (a shell standing in it, a file held open, this daemon's own
/// watches) keeps its connection, so the session's thread never returns and whatever its
/// filesystem holds (the workspace's stores, its runtime's tasks) outlives the holder that
/// ended. The connection is the mounting user's to abort, at the FUSE control filesystem's
/// entry for the mount's device.
#[cfg(target_os = "linux")]
fn end_connection(mount_point: &Path) -> io::Result<bool> {
    use rustix::io::Errno;
    use rustix::mount::{UnmountFlags, unmount};

    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    let Some((listed, connection)) = fuse_connection_at(&mountinfo, mount_point) else {
        return Ok(false);
    };
    let control = Path::new("/sys/fs/fuse/connections").join(connection.to_string());
    match unmount(mount_point, UnmountFlags::empty()) {
        Ok(()) => {}
        Err(Errno::PERM) => {
            if !fusermount(&listed, false)? {
                trace(&format!("busy; aborting connection {connection}"));
                abort(&control, &listed)?;
                // Aborted, the mount answers ENOTCONN until it is detached from the tree.
                if !fusermount(&listed, true)? {
                    return Err(io::Error::other(format!(
                        "{FUSERMOUNT} could not detach the folder at {}",
                        listed.display()
                    )));
                }
            }
        }
        Err(Errno::INVAL) => return Ok(false),
        Err(Errno::BUSY) => {
            trace("busy; forcing a detach");
            // The mount point is never opened here: serving that open is what the session
            // being ended must do, and the thread that would wait for it is this one. Every
            // change this daemon answered is already journaled and its content committed, so
            // nothing of its own is left in the kernel to flush.
            //
            // `MNT_FORCE` makes FUSE abort the connection before the unmount, so its open files
            // fail with `ENOTCONN` and the workers' reads end.
            unmount(mount_point, UnmountFlags::FORCE | UnmountFlags::DETACH)?;
        }
        Err(error) => return Err(error.into()),
    }
    // Unmounted here, the filesystem stays mounted wherever its mount reached besides (another
    // mount namespace it propagated to), and its connection serves on there: the session's
    // thread would wait for it. A connection the unmount ended has no control entry any more.
    if control.exists() {
        trace(&format!(
            "unmounted; connection {connection} lingers, aborting it"
        ));
        abort(&control, &listed)?;
    }
    Ok(true)
}

/// Abort the FUSE connection whose control entry is `control`, the one of the folder at `listed`:
/// its requests fail with `ENOTCONN` and its session's thread returns. One already gone is ended.
#[cfg(target_os = "linux")]
fn abort(control: &Path, listed: &Path) -> io::Result<()> {
    let abort = control.join("abort");
    match std::fs::write(&abort, b"1") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!(
                "the FUSE connection of the folder at {} could not be ended at {}: {error}",
                listed.display(),
                abort.display()
            ),
        )),
    }
}

#[cfg(target_os = "linux")]
fn trace(what: &str) {
    if std::env::var_os("TILO_FS_BENCH_VERBOSE").is_some() {
        eprintln!("unmount: {what}");
    }
}

/// Run `fusermount3 -u` on `listed`, lazily when `detach`: whether it unmounted.
#[cfg(target_os = "linux")]
fn fusermount(listed: &Path, detach: bool) -> io::Result<bool> {
    let mut command = std::process::Command::new(FUSERMOUNT);
    command.arg("-u").arg("-q");
    if detach {
        command.arg("-z");
    }
    let output = command
        .arg("--")
        .arg(listed)
        .stdin(std::process::Stdio::null())
        .output()?;
    Ok(output.status.success())
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
    let Some(listed) = fuse_mount_at(&mounts, at) else {
        return Ok(());
    };
    let status = std::process::Command::new(FUSERMOUNT)
        .args(["-u", "-z"])
        .arg(&listed)
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

/// The FUSE mount point `mounts` (as `/proc/self/mounts` reads) lists for `at`.
///
/// The kernel lists a mount by its real path, with a space, tab, newline or backslash as an
/// octal escape: a folder in a home reached through a symlink (`/home` on an image-based
/// Fedora is `/var/home`) is listed under the real one. Only `at`'s parent is resolved; the
/// mount point itself is never touched.
pub(crate) fn fuse_mount_at(mounts: &str, at: &Path) -> Option<PathBuf> {
    let real = at
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .zip(at.file_name())
        .map(|(parent, name)| parent.join(name));
    mounts.lines().find_map(|line| {
        let mut fields = line.split(' ');
        let _source = fields.next();
        let point = PathBuf::from(unescape(fields.next()?));
        let kind = fields.next()?;
        (kind.starts_with("fuse") && (point == at || real.as_ref() == Some(&point)))
            .then_some(point)
    })
}

/// The FUSE mount `mountinfo` (as `/proc/self/mountinfo` reads) lists for `at`, and its
/// connection: the number its entry in the FUSE control filesystem is named by, the mount's
/// device (`major << 20 | minor`).
#[cfg(any(target_os = "linux", test))]
fn fuse_connection_at(mountinfo: &str, at: &Path) -> Option<(PathBuf, u32)> {
    let real = at
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .zip(at.file_name())
        .map(|(parent, name)| parent.join(name));
    mountinfo.lines().find_map(|line| {
        let (mount, filesystem) = line.split_once(" - ")?;
        let mut fields = mount.split(' ');
        let device = fields.nth(2)?;
        let point = PathBuf::from(unescape(fields.nth(1)?));
        let kind = filesystem.split(' ').next()?;
        if !(kind == "fuse" || kind.starts_with("fuse."))
            || !(point == at || real.as_ref() == Some(&point))
        {
            return None;
        }
        let (major, minor) = device.split_once(':')?;
        let connection = major.parse::<u32>().ok()? << 20 | minor.parse::<u32>().ok()?;
        Some((point, connection))
    })
}

/// A `/proc/self/mounts` field with its `\ooo` octal escapes decoded.
fn unescape(field: &str) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt as _;
    let mut out = Vec::with_capacity(field.len());
    let mut rest = field.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        let code = after
            .get(..3)
            .filter(|_| byte == b'\\')
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        if let Some(code) = code {
            out.push(code);
            rest = after.get(3..).unwrap_or_default();
        } else {
            out.push(byte);
            rest = after;
        }
    }
    std::ffi::OsString::from_vec(out)
}

#[cfg(test)]
mod tests {
    use super::{fuse_connection_at, fuse_mount_at};

    /// A FUSE mount's connection is named by its device, as the control filesystem names it;
    /// another filesystem at the same path, or a FUSE mount elsewhere, is not it.
    #[test]
    fn a_fuse_mount_s_connection_is_its_device() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let at = home.join("Pat Home");
        let escaped = at.to_str().unwrap().replace(' ', "\\040");
        let mountinfo = format!(
            "25 1 0:22 / {escaped} rw,relatime - tmpfs tmpfs rw\n\
             33 25 0:356 / {escaped} rw,nosuid,nodev - fuse tilo rw,user_id=1001\n\
             34 25 0:357 / {escaped}2 rw,nosuid,nodev - fuse tilo rw,user_id=1001\n"
        );
        assert_eq!(fuse_connection_at(&mountinfo, &at), Some((at.clone(), 356)));
        let typed = mountinfo.replace("- fuse tilo", "- fuse.tilo tilo");
        assert_eq!(fuse_connection_at(&typed, &at), Some((at.clone(), 356)));
        let major = mountinfo.replace("0:356", "1:2");
        assert_eq!(
            fuse_connection_at(&major, &at),
            Some((at.clone(), 1 << 20 | 2))
        );
        assert_eq!(fuse_connection_at(&mountinfo, &home.join("Other")), None);
    }

    #[test]
    fn a_mount_in_a_home_reached_through_a_symlink_is_found_under_its_real_path() {
        let dir = tempfile::tempdir().unwrap();
        let real_home = dir.path().join("var/home/me");
        std::fs::create_dir_all(&real_home).unwrap();
        let real_home = real_home.canonicalize().unwrap();
        std::os::unix::fs::symlink(dir.path().join("var/home"), dir.path().join("home")).unwrap();
        let at = dir.path().join("home/me/Tilo (Tilo)");
        let listed = real_home.join("Tilo (Tilo)");
        let escaped = listed.to_str().unwrap().replace(' ', "\\040");
        let mounts =
            format!("proc /proc proc rw 0 0\ntilo {escaped} fuse.tilo rw,nosuid,nodev 0 0\n");
        assert_eq!(fuse_mount_at(&mounts, &at), Some(listed));
        // The same path's other mounts, and other paths' FUSE mounts, are not it.
        let other = format!("tmpfs {escaped} tmpfs rw 0 0\ntilo {escaped}2 fuse.tilo rw 0 0\n");
        assert_eq!(fuse_mount_at(&other, &at), None);
        // A backslash and a tab in a name are escaped too.
        let odd = real_home.join("a\\b\tc");
        let line = format!(
            "tilo {} fuse.tilo rw 0 0",
            odd.to_str()
                .unwrap()
                .replace('\\', "\\134")
                .replace('\t', "\\011")
        );
        assert_eq!(
            fuse_mount_at(&line, &dir.path().join("home/me/a\\b\tc")),
            Some(odd)
        );
    }
}
