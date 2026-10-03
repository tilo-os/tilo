//! Operating-system process facts.

use std::io;

/// Whether [`is_alive`] can observe other processes on this platform.
pub const CAN_CHECK_LIVENESS: bool = cfg!(unix);

/// Whether process `pid` exists, including processes this one may not
/// signal. Always `false` where [`CAN_CHECK_LIVENESS`] is `false`.
#[inline]
#[must_use]
pub fn is_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 does not deliver a signal; it asks the kernel
        // whether the process exists and whether this process may signal it.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Number of files process `pid` has open, or `None` when the platform does
/// not expose it or it cannot be read (permission denied, the process just
/// exited) — never a fabricated zero.
#[inline]
#[must_use]
pub fn open_file_count(pid: u32) -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .ok()
            .map(|it| it.flatten().count())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Whether the process table itself is readable. A process listing that
/// comes back empty is genuine when this succeeds; an error explains why
/// nothing could be listed. Succeeds where the platform has no such probe.
///
/// # Errors
///
/// Returns the error reading the process table (for example
/// `PermissionDenied`).
#[inline]
pub fn probe_process_table() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc").map(|_| ())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}
