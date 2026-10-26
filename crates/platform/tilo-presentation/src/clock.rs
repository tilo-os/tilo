//! Explicit platform clock evidence, separate from relative Instant offsets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostClockBracket {
    pub lo_s: f64,
    pub hi_s: f64,
}

/// Whether ordinary Wayland frames can be joined to the kernel monotonic clock.
pub const SUPPORTS_WAYLAND_TIMING: bool = cfg!(target_os = "linux");

/// Whether this target's compositor can report presentation through Metal's
/// observer (a build that also links that observer measures it).
pub const SUPPORTS_METAL_TIMING: bool = cfg!(target_os = "macos");

pub const CLOCK_NAME: &str = if cfg!(target_os = "macos") {
    "mach_absolute_seconds"
} else if cfg!(target_os = "linux") {
    "clock_monotonic_seconds"
} else {
    "unavailable"
};

/// Called only under an explicit native diagnostic observer, not normal unprofiled input.
pub fn sample<T>(instant: impl FnOnce() -> T) -> (T, Option<HostClockBracket>) {
    sample_with(host_seconds, instant)
}

fn sample_with<T>(
    mut host: impl FnMut() -> Option<f64>,
    instant: impl FnOnce() -> T,
) -> (T, Option<HostClockBracket>) {
    let lo_s = host();
    let instant = instant();
    let hi_s = host();
    (
        instant,
        lo_s.zip(hi_s)
            .map(|(lo_s, hi_s)| HostClockBracket { lo_s, hi_s }),
    )
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "macOS and Linux both answer, so the Option is never empty here; \
              the arm for a host this build cannot time is why it is one"
)]
fn host_seconds() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        Some(mach_seconds())
    }
    #[cfg(target_os = "linux")]
    {
        monotonic_seconds()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn monotonic_seconds() -> Option<f64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the writable timespec outlives this parameterless clock read.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut time) } != 0 {
        return None;
    }
    let secs = u64::try_from(time.tv_sec).ok()?;
    let nanos = u32::try_from(time.tv_nsec).ok()?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    Some(std::time::Duration::new(secs, nanos).as_secs_f64())
}

#[cfg(target_os = "macos")]
fn mach_seconds() -> f64 {
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {
        fn CACurrentMediaTime() -> f64;
    }
    // SAFETY: Apple's parameterless Core Animation clock API has no ownership
    // or thread-affinity preconditions and returns CFTimeInterval (double).
    unsafe { CACurrentMediaTime() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn host_clock_reads_bracket_in_order_and_report_invalid_or_unavailable() {
        {
            let calls = RefCell::new(Vec::new());
            let expected = tilo_clock::now();
            let mut times = [120.125, 120.25].into_iter();
            let (instant, bracket) = sample_with(
                || {
                    calls.borrow_mut().push("host");
                    Some(times.next().unwrap())
                },
                || {
                    calls.borrow_mut().push("instant");
                    expected
                },
            );
            assert_eq!(*calls.borrow(), ["host", "instant", "host"]);
            assert_eq!(instant, expected);
            assert_eq!(
                bracket.expect("clock readings"),
                HostClockBracket {
                    lo_s: 120.125,
                    hi_s: 120.25
                }
            );
            assert_eq!(times.next(), None);
        }

        {
            for (lo, hi) in [(5.0, 4.0), (f64::INFINITY, 8.0), (0.0, 0.0)] {
                let mut times = [lo, hi].into_iter();
                let (stamp, bracket) = sample_with(|| Some(times.next().unwrap()), || 123);
                assert_eq!(stamp, 123);
                assert_eq!(
                    bracket.expect("clock readings").lo_s.to_bits(),
                    lo.to_bits()
                );
                assert_eq!(
                    bracket.expect("clock readings").hi_s.to_bits(),
                    hi.to_bits()
                );
            }
        }

        {
            let before = tilo_clock::now();
            let (instant, bracket) = sample(tilo_clock::now);
            let after = tilo_clock::now();
            assert!(before <= instant && instant <= after);
            #[cfg(target_os = "macos")]
            {
                assert_eq!(CLOCK_NAME, "mach_absolute_seconds");
                let bracket = bracket.expect("macOS provides the Metal host clock");
                assert!(bracket.lo_s.is_finite() && bracket.lo_s > 0.0);
                assert!(bracket.hi_s.is_finite() && bracket.hi_s >= bracket.lo_s);
            }
            #[cfg(target_os = "linux")]
            {
                assert_eq!(CLOCK_NAME, "clock_monotonic_seconds");
                const { assert!(SUPPORTS_WAYLAND_TIMING) };
                let bracket = bracket.expect("Linux CLOCK_MONOTONIC must be available");
                assert!(bracket.lo_s.is_finite() && bracket.lo_s > 0.0);
                assert!(bracket.hi_s.is_finite() && bracket.hi_s >= bracket.lo_s);
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                assert_eq!(CLOCK_NAME, "unavailable");
                assert!(bracket.is_none());
            }
        }
    }
}
