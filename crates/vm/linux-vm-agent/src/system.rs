//! What the guest reports about itself, and what it accepts being told.

#[cfg(target_os = "linux")]
/// Set the guest's wall clock (`CLOCK_REALTIME`) to `unix_millis`, as the host's
/// clock reads. The agent is root in the guest (os/hosted-guest/hosted-init),
/// so the whole guest — the tenant's programs and the session alike — reads this
/// clock; a tenant's own program could not move it.
///
/// # Errors
///
/// The kernel refused the call, which is what an unprivileged agent gets.
pub fn set_realtime(unix_millis: u64) -> Result<(), String> {
    let (seconds, nanos) = realtime_parts(unix_millis);
    let timespec = libc::timespec {
        tv_sec: seconds,
        tv_nsec: nanos,
    };
    // SAFETY: `clock_settime` reads the timespec it is given and has no other
    // precondition.
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &raw const timespec) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

/// `unix_millis` as the `timespec` the kernel takes. Whole seconds never
/// overflow on a 64-bit `time_t`; one that would has a zero fallback the kernel
/// refuses rather than a wrapped year.
#[cfg(target_os = "linux")]
fn realtime_parts(unix_millis: u64) -> (libc::time_t, libc::c_long) {
    (
        libc::time_t::try_from(unix_millis / 1_000).unwrap_or(0),
        libc::c_long::try_from((unix_millis % 1_000).saturating_mul(1_000_000)).unwrap_or(0),
    )
}

#[cfg(not(target_os = "linux"))]
/// Only a Linux guest's agent moves its clock.
///
/// # Errors
///
/// Always: this guest is not Linux.
pub fn set_realtime(_unix_millis: u64) -> Result<(), String> {
    Err("only a Linux guest has a wall clock to set".to_string())
}

#[must_use]
pub fn meminfo(contents: &str) -> (u64, u64) {
    let field = |name: &str| {
        contents
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    };
    (field("MemTotal"), field("MemAvailable"))
}

#[must_use]
pub fn uptime_secs(contents: &str) -> u64 {
    contents
        .split_whitespace()
        .next()
        .and_then(|secs| secs.split('.').next()?.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_files_are_read_defensively() {
        let meminfo_text =
            "MemTotal:        2014340 kB\nMemFree: 1 kB\nMemAvailable:    1500000 kB\n";
        assert_eq!(meminfo(meminfo_text), (2_014_340, 1_500_000));
        assert_eq!(meminfo("garbage"), (0, 0));
        assert_eq!(uptime_secs("1234.56 99.00\n"), 1234);
        assert_eq!(uptime_secs(""), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_clock_is_set_with_millisecond_precision() {
        assert_eq!(
            realtime_parts(1_782_816_120_123),
            (1_782_816_120, 123_000_000)
        );
        assert_eq!(realtime_parts(0), (0, 0));
    }
}
