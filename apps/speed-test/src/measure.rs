//! Domain math used by the canonical effect worker: measured transfer rates
//! and median latency with variation between real samples.

use ui::time::Duration;

/// Megabits per second for `bytes` moved in `elapsed`.
#[allow(
    clippy::cast_precision_loss,
    reason = "byte counts of a few megabytes are exact in f64"
)]
pub(crate) fn mbps(bytes: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return 0.0;
    }
    bytes as f64 * 8.0 / seconds / 1_000_000.0
}

/// The median latency of `samples` and its jitter: the mean absolute
/// difference between consecutive samples.
#[allow(
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "a handful of samples; indices are bounded by the length checks"
)]
pub(crate) fn latency_stats(samples: &[f64]) -> Option<(f64, f64)> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) {
        f64::midpoint(sorted[mid - 1], sorted[mid])
    } else {
        sorted[mid]
    };
    let jitter = if samples.len() < 2 {
        0.0
    } else {
        samples
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .sum::<f64>()
            / (samples.len() - 1) as f64
    };
    Some((median, jitter))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_is_the_median_and_jitter_the_mean_step() {
        let (ping, jitter) = latency_stats(&[10.0, 14.0, 12.0, 12.0, 12.0]).unwrap();
        assert!((ping - 12.0).abs() < 1e-9);
        // Steps: 4, 2, 0, 0.
        assert!((jitter - 1.5).abs() < 1e-9);
    }

    #[test]
    fn a_single_sample_has_no_jitter() {
        assert_eq!(latency_stats(&[9.0]), Some((9.0, 0.0)));
        assert_eq!(latency_stats(&[]), None);
    }

    #[test]
    fn speed_is_megabits_per_second() {
        let speed = mbps(1_250_000, Duration::from_secs(1));
        assert!((speed - 10.0).abs() < 1e-9);
        assert!(mbps(10, Duration::ZERO).abs() < f64::EPSILON);
    }
}
