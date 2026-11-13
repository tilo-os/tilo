//! The traffic overlay's data model. Tilo's layer is crowdsourced and peer-shared: devices report
//! the speeds they see and the network aggregates them. No live feed is wired yet, so the live app
//! shows "No traffic data" rather than inventing colours, and sample congestion exists only on the
//! bundled basemap.

use crate::basemap::{Basemap, RoadClass};
use crate::failure;
use crate::model::LatLon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Congestion {
    Free = 0,
    Moderate = 1,
    Heavy = 2,
    Severe = 3,
}

impl Congestion {
    /// Every level, in worsening order.
    pub const ALL: [Congestion; 4] = [
        Congestion::Free,
        Congestion::Moderate,
        Congestion::Heavy,
        Congestion::Severe,
    ];

    pub fn label(self) -> String {
        failure::t(match self {
            Congestion::Free => "maps-congestion-free",
            Congestion::Moderate => "maps-congestion-moderate",
            Congestion::Heavy => "maps-congestion-heavy",
            Congestion::Severe => "maps-congestion-standstill",
        })
    }
}

#[derive(Clone, Debug)]
pub struct TrafficSegment {
    pub geometry: Vec<LatLon>,
    pub level: Congestion,
}

#[derive(Clone, Debug, Default)]
pub struct TrafficData {
    pub segments: Vec<TrafficSegment>,
}

impl TrafficData {
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn counts(&self) -> [usize; 4] {
        let mut c = [0usize; 4];
        for s in &self.segments {
            if let Some(n) = c.get_mut(s.level as usize) {
                *n += 1;
            }
        }
        c
    }

    /// A sample overlay from the bundled basemap: each drivable road split into short segments with
    /// a congestion level derived from its geometry, so the same map colours the same every time.
    /// Levels are weighted by road class, so primary avenues carry the most traffic and the
    /// residential grid
    /// flows freely. The live app never calls this.
    pub fn sample(base: &Basemap) -> Self {
        /// Points per sub-segment window (they overlap by one point so the coloured ribbon stays
        /// continuous across a level change).
        const WINDOW: usize = 2;
        let mut segments = Vec::new();
        for road in &base.roads {
            if road.pts.len() < 2 {
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
        Self { segments }
    }
}

/// Class-weighted congestion for the sample overlay: primary avenues never free, the
/// residential grid mostly does.
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traffic_levels_start_empty_and_sample_overlay_is_realistic() {
        {
            let t = TrafficData::default();
            assert!(t.is_empty(), "live traffic starts with no reports");
            assert_eq!(t.counts(), [0, 0, 0, 0]);
        }

        {
            let base = crate::sample::basemap();
            let t = TrafficData::sample(&base);
            assert!(!t.is_empty(), "the sample basemap has roads to colour");
            for s in &t.segments {
                assert!(s.geometry.len() >= 2, "a segment is a real polyline");
            }
            let counts = t.counts();
            assert_eq!(
                counts.iter().sum::<usize>(),
                t.segments.len(),
                "every segment is counted in exactly one level bucket"
            );
            let drivable = base.roads.iter().filter(|r| r.pts.len() >= 2).count();
            assert!(
                t.segments.len() > drivable,
                "roads are split into sub-segments: {} segments from {drivable} roads",
                t.segments.len()
            );
            let again = TrafficData::sample(&base);
            assert_eq!(
                again.counts(),
                counts,
                "sample overlay is stable across runs"
            );
        }

        {
            let base = crate::sample::basemap();
            let counts = TrafficData::sample(&base).counts();
            let levels_present = counts.iter().filter(|&&n| n > 0).count();
            assert!(
                levels_present >= 3,
                "at least three congestion levels are represented: {counts:?}"
            );
            assert!(
                counts[Congestion::Free as usize] > 0,
                "some roads flow freely"
            );
            assert!(
                counts[Congestion::Heavy as usize] + counts[Congestion::Severe as usize] > 0,
                "some arterials are congested"
            );
        }

        {
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
