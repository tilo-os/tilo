//! Adaptive bitrate (ABR) stream selection.
//!
//! YouTube exposes a video at several qualities at once (yt-dlp / rustypipe hand
//! back the whole `formats` ladder, each with a height and a bitrate). This
//! module is the pure decision layer that picks *which* of those streams to play:
//!
//! * **Manual override** — when the user pins a resolution ("1080p" / "720p" /
//!   "480p"), honour it: play the highest variant that does not exceed the pinned
//!   height (never a taller one), falling back to the smallest if every variant
//!   is taller.
//! * **Automatic (bandwidth-driven)** — when the user leaves it on "auto", pick
//!   the highest-bitrate variant the measured bandwidth can sustain (with a
//!   safety margin), and re-pick as the estimate moves so playback rides the
//!   connection up and down. [`BandwidthEstimator`] keeps a smoothed estimate
//!   from real throughput samples.
//!
//! Kept toolkit-free and network-free so it can be unit-tested exhaustively and
//! run on the fetch-worker thread.

use std::time::Duration;

/// One selectable rendition of a video: enough to choose between qualities and
/// then hand the chosen URL to the media engine.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamVariant {
    /// Directly-playable media URL for this rendition.
    pub url: String,
    /// Vertical resolution in pixels (e.g. 720). 0 when unknown.
    pub height: u32,
    /// Total bitrate in bits per second (video + audio). The bandwidth an
    /// uninterrupted stream of this rendition needs.
    pub bandwidth_bps: u64,
    /// True when the rendition carries audio as well as video.
    pub has_audio: bool,
    /// Short quality label for the UI / status log (e.g. "720p").
    pub quality: String,
}

/// Fraction of the estimated bandwidth an auto-selected stream is allowed to use,
/// leaving headroom so a rendition right at the estimate doesn't immediately
/// rebuffer when throughput dips.
const SAFETY: f64 = 0.85;

/// The height "auto" aims for when there is no bandwidth estimate yet (first
/// play, before any throughput sample) — a sensible default that isn't the most
/// expensive rendition.
const DEFAULT_AUTO_HEIGHT: u32 = 720;

/// Pick the index of the best variant for the current conditions.
///
/// `manual_max_height` pins an upper bound (the user's resolution choice); `None`
/// means "auto", in which case `estimate_bps` drives the pick. Returns `None`
/// only when `variants` is empty.
pub fn select_variant(
    variants: &[StreamVariant],
    estimate_bps: Option<u64>,
    manual_max_height: Option<u32>,
) -> Option<usize> {
    if variants.is_empty() {
        return None;
    }
    // The smallest rendition is the universal fallback (always playable).
    let smallest = min_by_height(variants);

    if let Some(cap) = manual_max_height {
        // Manual: the tallest rendition at or under the cap; else the smallest.
        return Some(
            variants
                .iter()
                .enumerate()
                .filter(|(_, v)| v.height <= cap)
                .max_by_key(|(_, v)| v.height)
                .map(|(i, _)| i)
                .unwrap_or(smallest),
        );
    }

    match estimate_bps {
        Some(bps) => {
            // Auto: the richest rendition whose bitrate fits the budget; if none
            // fits (a very slow link), the smallest so playback still starts.
            let budget = (bps as f64 * SAFETY) as u64;
            Some(
                variants
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.bandwidth_bps <= budget)
                    .max_by_key(|(_, v)| v.bandwidth_bps)
                    .map(|(i, _)| i)
                    .unwrap_or(smallest),
            )
        }
        None => {
            // No estimate yet: aim for a sensible default height (tallest at or
            // under it), else the smallest.
            Some(
                variants
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.height <= DEFAULT_AUTO_HEIGHT)
                    .max_by_key(|(_, v)| v.height)
                    .map(|(i, _)| i)
                    .unwrap_or(smallest),
            )
        }
    }
}

fn min_by_height(variants: &[StreamVariant]) -> usize {
    variants
        .iter()
        .enumerate()
        .min_by_key(|(_, v)| (v.height, v.bandwidth_bps))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Map a UI resolution label to a maximum height, or `None` for automatic.
/// "1080p" → `Some(1080)`, "auto" (or anything unparseable) → `None`.
pub fn max_height_for_quality(quality: &str) -> Option<u32> {
    let q = quality.trim().to_ascii_lowercase();
    if q == "auto" {
        return None;
    }
    q.trim_end_matches('p').parse::<u32>().ok()
}

/// A smoothed bandwidth estimate built from real throughput samples.
///
/// Each sample is `(bytes, elapsed)`; the estimator keeps an exponentially
/// weighted moving average of the resulting bits-per-second so a single slow
/// chunk doesn't swing the auto-quality pick, while a sustained change moves it.
#[derive(Clone, Debug, Default)]
pub struct BandwidthEstimator {
    ewma_bps: Option<f64>,
}

impl BandwidthEstimator {
    /// Weight given to each new sample (0..1). Higher reacts faster; lower is
    /// smoother. 0.35 tracks sustained changes over a few samples without
    /// chasing per-chunk jitter.
    const ALPHA: f64 = 0.35;

    pub fn new() -> Self {
        Self { ewma_bps: None }
    }

    /// Fold in one throughput sample. Zero-length or zero-byte samples are
    /// ignored (they carry no rate information).
    pub fn record(&mut self, bytes: u64, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        if bytes == 0 || secs <= 0.0 {
            return;
        }
        let bps = (bytes as f64 * 8.0) / secs;
        self.ewma_bps = Some(match self.ewma_bps {
            Some(prev) => prev * (1.0 - Self::ALPHA) + bps * Self::ALPHA,
            None => bps,
        });
    }

    /// The current smoothed estimate in bits per second, or `None` until at least
    /// one sample has landed.
    pub fn estimate_bps(&self) -> Option<u64> {
        self.ewma_bps.map(|b| b as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder() -> Vec<StreamVariant> {
        vec![
            StreamVariant { url: "u360".into(), height: 360, bandwidth_bps: 700_000, has_audio: true, quality: "360p".into() },
            StreamVariant { url: "u480".into(), height: 480, bandwidth_bps: 1_400_000, has_audio: true, quality: "480p".into() },
            StreamVariant { url: "u720".into(), height: 720, bandwidth_bps: 2_800_000, has_audio: true, quality: "720p".into() },
            StreamVariant { url: "u1080".into(), height: 1080, bandwidth_bps: 5_000_000, has_audio: true, quality: "1080p".into() },
        ]
    }

    #[test]
    fn auto_picks_the_richest_stream_the_bandwidth_sustains() {
        let l = ladder();
        // ~1 Mbps: only 360p (700k) fits under the 85% budget (~850k).
        assert_eq!(select_variant(&l, Some(1_000_000), None).map(|i| l[i].height), Some(360));
        // ~3.5 Mbps budget ~2.97M: 720p (2.8M) fits, 1080p (5M) doesn't.
        assert_eq!(select_variant(&l, Some(3_500_000), None).map(|i| l[i].height), Some(720));
        // Plenty of headroom → the top rendition.
        assert_eq!(select_variant(&l, Some(8_000_000), None).map(|i| l[i].height), Some(1080));
        // A brutally slow link still starts, at the smallest rendition.
        assert_eq!(select_variant(&l, Some(100_000), None).map(|i| l[i].height), Some(360));
    }

    #[test]
    fn manual_override_is_honoured_regardless_of_bandwidth() {
        let l = ladder();
        // Pin 480p on a fat pipe: it must NOT jump to 1080p.
        assert_eq!(select_variant(&l, Some(50_000_000), Some(480)).map(|i| l[i].height), Some(480));
        // Pin 720p on a thin pipe: it must NOT drop below the pin.
        assert_eq!(select_variant(&l, Some(200_000), Some(720)).map(|i| l[i].height), Some(720));
        // A cap between rungs picks the tallest at or under it.
        assert_eq!(select_variant(&l, None, Some(600)).map(|i| l[i].height), Some(480));
        // A cap below every rung falls back to the smallest.
        assert_eq!(select_variant(&l, None, Some(240)).map(|i| l[i].height), Some(360));
    }

    #[test]
    fn no_estimate_aims_for_a_sensible_default() {
        let l = ladder();
        assert_eq!(select_variant(&l, None, None).map(|i| l[i].height), Some(720));
        assert_eq!(select_variant(&[], None, None), None);
    }

    #[test]
    fn quality_label_maps_to_a_height_cap() {
        assert_eq!(max_height_for_quality("1080p"), Some(1080));
        assert_eq!(max_height_for_quality("720p"), Some(720));
        assert_eq!(max_height_for_quality("480p"), Some(480));
        assert_eq!(max_height_for_quality("auto"), None);
    }

    #[test]
    fn estimator_smooths_and_tracks_sustained_change() {
        let mut e = BandwidthEstimator::new();
        assert_eq!(e.estimate_bps(), None, "no estimate before any sample");
        // 1 MB in 1s = 8 Mbps.
        e.record(1_000_000, Duration::from_secs(1));
        assert_eq!(e.estimate_bps(), Some(8_000_000));
        // A single slow chunk (1 Mbps) nudges but does not crash the estimate.
        e.record(125_000, Duration::from_secs(1));
        let mid = e.estimate_bps().unwrap();
        assert!(mid < 8_000_000 && mid > 1_000_000, "one slow sample is smoothed, not obeyed: {mid}");
        // Sustained slow throughput pulls the estimate down toward it.
        for _ in 0..8 {
            e.record(125_000, Duration::from_secs(1));
        }
        let low = e.estimate_bps().unwrap();
        assert!(low < 1_500_000, "a sustained slow link drives the estimate down: {low}");
    }

    #[test]
    fn zero_samples_are_ignored() {
        let mut e = BandwidthEstimator::new();
        e.record(0, Duration::from_secs(1));
        e.record(1000, Duration::from_secs(0));
        assert_eq!(e.estimate_bps(), None);
    }
}
