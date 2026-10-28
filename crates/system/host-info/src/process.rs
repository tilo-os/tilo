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
