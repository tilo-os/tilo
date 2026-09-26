//! Direct host-clock bounds for opt-in native input diagnostics.
//!
//! Metal's `presentedTime` and `CACurrentMediaTime` use host seconds. Keep these
//! bounds separate from Instant offsets: no assumption about Instant's epoch
//! or cross-clock rate is needed. The caller serializes after all reads finish.
use crate::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HostClockBracket {
    pub lo_s: f64,
    pub hi_s: f64,
}

pub(crate) const CLOCK_NAME: &str = if cfg!(target_os = "macos") {
    "mach_absolute_seconds"
} else {
    "unavailable"
};

/// Called only for native scenario packets, never ordinary platform input.
pub(crate) fn sample() -> (Instant, Option<HostClockBracket>) {
    #[cfg(target_os = "macos")]
    {
        let (instant, bracket) = sample_with(host_seconds, crate::time::now);
        (instant, Some(bracket))
    }
    #[cfg(not(target_os = "macos"))]
    {
        (crate::time::now(), None)
    }
}

#[cfg(target_os = "macos")]
fn host_seconds() -> f64 {
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {
        fn CACurrentMediaTime() -> f64;
    }
    // SAFETY: Apple's parameterless Core Animation clock API has no ownership
    // or thread-affinity preconditions and returns CFTimeInterval (double).
    unsafe { CACurrentMediaTime() }
}

#[cfg(any(target_os = "macos", test))]
fn sample_with<T>(
    mut host: impl FnMut() -> f64,
    instant: impl FnOnce() -> T,
) -> (T, HostClockBracket) {
    let lo_s = host();
    let stamp = instant();
    let hi_s = host();
    (stamp, HostClockBracket { lo_s, hi_s })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn host_clock_reads_bracket_in_order_and_report_invalid_or_unavailable() {
        // Case `host_reads_bracket_the_original_instant_read_in_order`.
        {
            let calls = RefCell::new(Vec::new());
            let expected = crate::time::now();
            let mut times = [120.125, 120.25].into_iter();
            let (instant, bracket) = sample_with(
                || {
                    calls.borrow_mut().push("host");
                    times.next().unwrap()
                },
                || {
                    calls.borrow_mut().push("instant");
                    expected
                },
            );
            assert_eq!(*calls.borrow(), ["host", "instant", "host"]);
            assert_eq!(instant, expected);
            assert_eq!(
                bracket,
                HostClockBracket {
                    lo_s: 120.125,
                    hi_s: 120.25
                }
            );
            assert_eq!(times.next(), None);
        }

        // Case `reversed_and_nonfinite_reads_remain_invalid_evidence`.
        {
            for (lo, hi) in [(5.0, 4.0), (f64::INFINITY, 8.0), (0.0, 0.0)] {
                let mut times = [lo, hi].into_iter();
                let (stamp, bracket) = sample_with(|| times.next().unwrap(), || 123);
                assert_eq!(stamp, 123);
                assert_eq!(bracket.lo_s, lo);
                assert_eq!(bracket.hi_s, hi);
            }
        }

        // Case `platform_sample_explicitly_reports_clock_availability`.
        {
            let before = crate::time::now();
            let (instant, bracket) = sample();
            let after = crate::time::now();
            assert!(before <= instant && instant <= after);
            #[cfg(target_os = "macos")]
            {
                assert_eq!(CLOCK_NAME, "mach_absolute_seconds");
                let bracket = bracket.expect("macOS provides the Metal host clock");
                assert!(bracket.lo_s.is_finite() && bracket.lo_s > 0.0);
                assert!(bracket.hi_s.is_finite() && bracket.hi_s >= bracket.lo_s);
            }
            #[cfg(not(target_os = "macos"))]
            {
                assert_eq!(CLOCK_NAME, "unavailable");
                assert!(bracket.is_none());
            }
        }
    }
}
