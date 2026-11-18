//! Waiting until the wall clock reaches an instant.
//!
//! A certificate that verifies but is "not yet valid" is intact; it verifies
//! again once the clock reads a time inside its validity interval. A machine
//! whose real-time clock came up weeks behind (a PMIC counter that restarted,
//! a drained clock battery) refuses its own certificate until network time
//! steps the clock. [`wait_for_wall_clock`] waits for that instead of
//! retrying on a timer.

use std::io;

/// What ended a [`wait_for_wall_clock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallClockWait {
    /// The wall clock reached the instant.
    Reached,
    /// The wall clock was set or stepped (or the machine resumed) before
    /// it reached the instant. Whatever depended on its reading may have
    /// changed either way.
    Changed,
}

/// Block until the wall clock reaches `unix_ns` (Unix nanoseconds), or
/// until it changes discontinuously, whichever comes first. Returns at once
/// when the clock already reads `unix_ns` or later.
///
/// On Linux this is one absolute `CLOCK_REALTIME` timerfd armed with
/// `TFD_TIMER_CANCEL_ON_SET`: the kernel ends the wait when the instant
/// arrives or when the clock is set, so nothing polls and no step is
/// missed, however far the clock jumps.
///
/// # Errors
///
/// Returns the system error when the timer cannot be created, armed or
/// read.
#[cfg(target_os = "linux")]
pub fn wait_for_wall_clock(unix_ns: u64) -> io::Result<WallClockWait> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    // SAFETY: a plain syscall; the descriptor is checked and owned once.
    let raw = unsafe { libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh descriptor nothing else owns.
    let timer = unsafe { OwnedFd::from_raw_fd(raw) };
    // Zero would disarm the timer: an instant at the epoch fires at its
    // first nanosecond instead.
    let at = unix_ns.max(1);
    let spec = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: libc::time_t::try_from(at / 1_000_000_000).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::try_from(at % 1_000_000_000).unwrap_or(0),
        },
    };
    // SAFETY: `timer` is a live timerfd and `spec` a valid value.
    let armed = unsafe {
        libc::timerfd_settime(
            timer.as_raw_fd(),
            libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
            &raw const spec,
            std::ptr::null_mut(),
        )
    };
    if armed != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut expirations = [0u8; 8];
    loop {
        // SAFETY: `expirations` is a writable buffer of the length passed.
        let read = unsafe {
            libc::read(
                timer.as_raw_fd(),
                expirations.as_mut_ptr().cast::<libc::c_void>(),
                expirations.len(),
            )
        };
        if read == 8 {
            return Ok(WallClockWait::Reached);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ECANCELED) => return Ok(WallClockWait::Changed),
            Some(libc::EINTR) => {}
            _ => return Err(error),
        }
    }
}

/// [`wait_for_wall_clock`] where the kernel cannot report clock changes:
/// one sleep until the instant, which a clock step does not shorten.
///
/// # Errors
///
/// Never; the signature matches the Linux implementation.
#[cfg(all(not(target_os = "linux"), not(target_arch = "wasm32")))]
pub fn wait_for_wall_clock(unix_ns: u64) -> io::Result<WallClockWait> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        });
    if let Some(remaining) = unix_ns.checked_sub(now).filter(|ns| *ns > 0) {
        std::thread::sleep(std::time::Duration::from_nanos(remaining));
    }
    Ok(WallClockWait::Reached)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn waiting_for_the_wall_clock_ends_when_it_reaches_the_instant() {
        let now = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| u64::try_from(since.as_nanos()).unwrap())
                .unwrap()
        };
        assert_eq!(wait_for_wall_clock(1).unwrap(), WallClockWait::Reached);
        let soon = now() + 50_000_000;
        assert_eq!(wait_for_wall_clock(soon).unwrap(), WallClockWait::Reached);
        assert!(now() >= soon);
    }
}
