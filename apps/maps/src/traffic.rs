//! Real-time road-traffic overlay — the data model.
//!
//! Tilo's traffic layer is, by design, a *crowdsourced, peer-shared* layer over
//! OSM (see `docs/architecture/product.md`): devices report the speed they observe on a road
//! and the network aggregates those into a live congestion picture. There is no
//! turnkey free real-time-traffic feed to wire, so the live app ships the
//! overlay and this data model with an **honest empty state**: with no peer
//! reports the map shows "No traffic data" rather than inventing colours.
//!
//! The rendering lives in [`crate::canvas::draw_traffic`]; sample congestion
//! (behind `--shots` / the bundled offline basemap) exists only to exercise
//! that rendering and is always labelled as sample data — never presented as a
//! real measurement.

use crate::model::LatLon;
use crate::sample::{Basemap, RoadClass};

/// Congestion on a road segment, from free flow to a standstill. This is the
/// signal a live report (or a routing provider's current-vs-typical speed
/// ratio) maps onto, so peer reports and provider data share one bucketing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Congestion {
    /// Traffic moving at (or near) the free-flow speed.
    Free = 0,
    /// Noticeably slower than free flow, still moving.
    Moderate = 1,
    /// Heavy — well below free flow.
    Heavy = 2,
    /// Standstill / stop-and-go.
    Severe = 3,
}

impl Congestion {
    /// Every level, in worsening order — for legends and iteration.
    pub const ALL: [Congestion; 4] = [
        Congestion::Free,
        Congestion::Moderate,
        Congestion::Heavy,
        Congestion::Severe,
    ];

    /// Map an observed-speed / free-flow-speed ratio (0..=1) to a level. A real
    /// feed reports current vs typical speed; this is the shared thresholding so
    /// a peer report and a provider agree on what "heavy" means.
    pub fn from_speed_ratio(ratio: f64) -> Self {
        let r = ratio.clamp(0.0, 1.0);
        if r >= 0.75 {
            Congestion::Free
        } else if r >= 0.5 {
            Congestion::Moderate
        } else if r >= 0.25 {
            Congestion::Heavy
        } else {
            Congestion::Severe
        }
    }

    /// Human-readable label for the legend / a tapped segment.
    pub fn label(self) -> &'static str {
        match self {
            Congestion::Free => "Free flowing",
            Congestion::Moderate => "Moderate",
            Congestion::Heavy => "Heavy",
            Congestion::Severe => "Standstill",
        }
    }
}

/// One coloured road segment: a geographic polyline + its congestion level.
#[derive(Clone, Debug)]
pub struct TrafficSegment {
    pub geometry: Vec<LatLon>,
    pub level: Congestion,
}

/// The traffic overlay's data. Empty until real reports arrive — the honest
/// live state. `source` records provenance for the UI (e.g. `"sample data"`);
/// `None` means there is nothing to show and the overlay must say so.
#[derive(Clone, Debug, Default)]
pub struct TrafficData {
    pub segments: Vec<TrafficSegment>,
    pub source: Option<String>,
}

impl TrafficData {
    /// No congestion to draw — the live "No traffic data" state.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Count of segments at each [`Congestion`] level, indexed by the level's
    /// discriminant — drives the legend's per-level tallies.
    pub fn counts(&self) -> [usize; 4] {
        let mut c = [0usize; 4];
        for s in &self.segments {
            if let Some(n) = c.get_mut(s.level as usize) {
                *n += 1;
            }
        }
        c
    }

    /// Build a **sample** congestion overlay from the bundled vector basemap.
    ///
    /// Every drivable road is split into short sub-segments, each given a
    /// deterministic congestion level (derived from its geometry, so the same
    /// map always colours the same — no RNG, no per-frame churn). Congestion
    /// varies *along* a road the way real traffic does, and the level
    /// distribution is weighted by road class: primary avenues run busy
    /// (heavy → standstill), secondaries mixed, and the residential grid mostly
    /// free-flowing — so the overlay reads like a real arterial network rather
    /// than a flat wash.
    ///
    /// SAMPLE ONLY: the live app never calls this; it waits for real peer
    /// reports. Marked `"sample data"` so the UI can label it honestly.
    pub fn sample(base: &Basemap) -> Self {
        // Points per sub-segment window (they overlap by one point so the
        // coloured ribbon stays continuous across a level change).
        const WINDOW: usize = 2;
        let mut segments = Vec::new();
        for road in &base.roads {
            if matches!(road.class, RoadClass::Path) || road.pts.len() < 2 {
                continue;
            }
            let mut i = 0;
            let mut sub = 0u64;
            while i + 1 < road.pts.len() {
                let end = (i + WINDOW).min(road.pts.len());
                let Some(geom) = road.pts.get(i..end).map(<[_]>::to_vec) else {
                    break;
                };
                let Some(&seed) = geom.first() else {
                    break;
                };
                // Deterministic per-sub-segment hash.
                let h = (seed.lat.abs() * 1000.0) as u64 * 73
                    + (seed.lon.abs() * 1000.0) as u64 * 19
                    + sub.wrapping_mul(101)
                    + road.pts.len() as u64;
                segments.push(TrafficSegment {
                    geometry: geom,
                    level: level_for(road.class, h),
                });
                i = end - 1;
                sub += 1;
            }
        }
        Self {
            segments,
            source: Some("sample data".to_string()),
        }
    }
}

/// Class-weighted congestion pick for the sample overlay. Primary avenues carry
/// the most traffic (never free); the residential grid mostly flows freely.
fn level_for(class: RoadClass, h: u64) -> Congestion {
    let b = h % 100;
    match class {
        RoadClass::Primary => {
            if b < 20 {
                Congestion::Moderate
            } else if b < 60 {
                Congestion::Heavy
            } else {
                Congestion::Severe
            }
        }
        RoadClass::Secondary => {
            if b < 35 {
                Congestion::Moderate
            } else if b < 80 {
                Congestion::Heavy
            } else {
                Congestion::Severe
            }
        }
        RoadClass::Residential => {
            if b < 66 {
                Congestion::Free
            } else if b < 92 {
                Congestion::Moderate
            } else {
                Congestion::Heavy
            }
        }
        RoadClass::Path => Congestion::Free,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn traffic_levels_start_empty_and_sample_overlay_is_realistic() {
        // Phase `speed_ratio_buckets_into_levels`.
        {
            assert_eq!(Congestion::from_speed_ratio(1.0), Congestion::Free);
            assert_eq!(Congestion::from_speed_ratio(0.8), Congestion::Free);
            assert_eq!(Congestion::from_speed_ratio(0.6), Congestion::Moderate);
            assert_eq!(Congestion::from_speed_ratio(0.3), Congestion::Heavy);
            assert_eq!(Congestion::from_speed_ratio(0.1), Congestion::Severe);
            // Out-of-range ratios clamp rather than panic.
            assert_eq!(Congestion::from_speed_ratio(5.0), Congestion::Free);
            assert_eq!(Congestion::from_speed_ratio(-1.0), Congestion::Severe);
        }

        // Phase `default_traffic_is_empty_the_honest_live_state`.
        {
            let t = TrafficData::default();
            assert!(t.is_empty(), "live traffic starts with no reports");
            assert!(t.source.is_none(), "no provenance when there is no data");
            assert_eq!(t.counts(), [0, 0, 0, 0]);
        }

        // Phase `sample_overlay_colours_the_basemap_roads`.
        {
            let base = crate::sample::basemap();
            let t = TrafficData::sample(&base);
            assert!(!t.is_empty(), "the sample basemap has roads to colour");
            assert_eq!(t.source.as_deref(), Some("sample data"));
            // Every segment is a real polyline (>= 2 points) and is counted in
            // exactly one level bucket.
            for s in &t.segments {
                assert!(s.geometry.len() >= 2, "a segment is a real polyline");
            }
            let counts = t.counts();
            assert_eq!(
                counts.iter().sum::<usize>(),
                t.segments.len(),
                "every segment is counted in exactly one level bucket"
            );
            // Sub-segmentation makes the overlay dense: many more coloured segments
            // than there are roads, so congestion varies *along* a road.
            let drivable = base
                .roads
                .iter()
                .filter(|r| !matches!(r.class, RoadClass::Path) && r.pts.len() >= 2)
                .count();
            assert!(
                t.segments.len() > drivable,
                "roads are split into sub-segments: {} segments from {drivable} roads",
                t.segments.len()
            );
            // Deterministic: the same basemap yields the identical overlay each run.
            let again = TrafficData::sample(&base);
            assert_eq!(
                again.counts(),
                counts,
                "sample overlay is stable across runs"
            );
        }

        // Phase `sample_overlay_shows_a_realistic_congestion_spread`.
        {
            // A believable traffic picture uses the whole ramp, not one flat colour.
            let base = crate::sample::basemap();
            let counts = TrafficData::sample(&base).counts();
            let levels_present = counts.iter().filter(|&&n| n > 0).count();
            assert!(
                levels_present >= 3,
                "at least three congestion levels are represented: {counts:?}"
            );
            // The residential grid keeps some free-flowing streets, and the busy
            // arterials produce heavy/severe segments — both ends of the ramp show.
            assert!(
                counts[Congestion::Free as usize] > 0,
                "some roads flow freely"
            );
            assert!(
                counts[Congestion::Heavy as usize] + counts[Congestion::Severe as usize] > 0,
                "some arterials are congested"
            );
        }

        // Phase `primary_avenues_never_flow_free`.
        {
            // Primaries carry the most traffic — every sub-segment on a primary road
            // is at least Moderate, so the overlay reads like a real arterial net.
            for h in 0..500u64 {
                assert_ne!(
                    super::level_for(RoadClass::Primary, h),
                    Congestion::Free,
                    "a primary arterial is never fully free (h={h})"
                );
            }
        }
    }
}
