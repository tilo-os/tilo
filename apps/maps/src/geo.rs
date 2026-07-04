//! Web-Mercator projection + geodesic helpers.
//!
//! The map uses the standard slippy-map projection: the world is a square
//! of `256 · 2^zoom` pixels at a given (fractional) zoom. We project a
//! `LatLon` to a screen `Point` relative to a viewport whose centre shows
//! `center`. All trig lives here so the canvas/draw code stays geometric.

use std::f64::consts::PI;

use vello::kurbo::Point;

use crate::model::LatLon;

const TILE_SIZE: f64 = 256.0;
const EARTH_RADIUS_M: f64 = 6_378_137.0;

/// World-pixel size at `zoom`.
fn world_size(zoom: f64) -> f64 {
    TILE_SIZE * 2f64.powf(zoom)
}

/// Clamp latitude to the Mercator-valid range.
fn clamp_lat(lat: f64) -> f64 {
    lat.clamp(-85.051_128_78, 85.051_128_78)
}

/// Project a coordinate to absolute world pixels at `zoom`.
pub fn lonlat_to_world(c: LatLon, zoom: f64) -> (f64, f64) {
    let size = world_size(zoom);
    let lat = clamp_lat(c.lat).to_radians();
    let x = (c.lon + 180.0) / 360.0 * size;
    let y = (0.5 - (lat.tan() + 1.0 / lat.cos()).ln() / (2.0 * PI)) * size;
    (x, y)
}

/// Inverse of [`lonlat_to_world`].
pub fn world_to_lonlat(x: f64, y: f64, zoom: f64) -> LatLon {
    let size = world_size(zoom);
    let lon = x / size * 360.0 - 180.0;
    let n = PI - 2.0 * PI * y / size;
    let lat = (0.5 * (n.exp() - (-n).exp())).atan().to_degrees();
    LatLon::new(lat, lon)
}

/// A reusable projection bound to a viewport rectangle + camera. Construct
/// once per frame and project many points cheaply.
#[derive(Clone, Copy, Debug)]
pub struct Projection {
    center_world: (f64, f64),
    view_center: Point,
    zoom: f64,
}

impl Projection {
    pub fn new(center: LatLon, zoom: f64, viewport_center: Point) -> Self {
        Self {
            center_world: lonlat_to_world(center, zoom),
            view_center: viewport_center,
            zoom,
        }
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// Project a geographic coordinate to a screen point.
    pub fn project(&self, c: LatLon) -> Point {
        let (wx, wy) = lonlat_to_world(c, self.zoom);
        Point::new(
            self.view_center.x + (wx - self.center_world.0),
            self.view_center.y + (wy - self.center_world.1),
        )
    }

    /// Inverse-project a screen point back to a coordinate.
    pub fn unproject(&self, p: Point) -> LatLon {
        let wx = p.x - self.view_center.x + self.center_world.0;
        let wy = p.y - self.view_center.y + self.center_world.1;
        world_to_lonlat(wx, wy, self.zoom)
    }
}

/// Great-circle distance between two coordinates, in metres (haversine).
pub fn haversine_m(a: LatLon, b: LatLon) -> f64 {
    let (lat1, lat2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = (b.lat - a.lat).to_radians();
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

/// Total length of a polyline in metres.
pub fn polyline_length_m(pts: &[LatLon]) -> f64 {
    pts.windows(2).map(|w| haversine_m(w[0], w[1])).sum()
}

/// Compute a centre + zoom that frames `pts` inside a `w × h` viewport with
/// `pad` pixels of margin. Falls back to a default zoom for degenerate input.
pub fn fit_bounds(pts: &[LatLon], w: f64, h: f64, pad: f64) -> Option<(LatLon, f64)> {
    if pts.is_empty() {
        return None;
    }
    let mut min_lat = f64::MAX;
    let mut max_lat = f64::MIN;
    let mut min_lon = f64::MAX;
    let mut max_lon = f64::MIN;
    for p in pts {
        min_lat = min_lat.min(p.lat);
        max_lat = max_lat.max(p.lat);
        min_lon = min_lon.min(p.lon);
        max_lon = max_lon.max(p.lon);
    }
    let center = LatLon::new((min_lat + max_lat) / 2.0, (min_lon + max_lon) / 2.0);
    let avail_w = (w - 2.0 * pad).max(64.0);
    let avail_h = (h - 2.0 * pad).max(64.0);

    // Find the largest zoom at which the bbox still fits.
    let mut best = 3.0_f64;
    let mut z = 3.0_f64;
    while z < 19.0 {
        let nw = lonlat_to_world(LatLon::new(max_lat, min_lon), z);
        let se = lonlat_to_world(LatLon::new(min_lat, max_lon), z);
        let span_x = (se.0 - nw.0).abs();
        let span_y = (se.1 - nw.1).abs();
        if span_x <= avail_w && span_y <= avail_h {
            best = z;
            z += 0.25;
        } else {
            break;
        }
    }
    Some((center, best))
}

/// Human-readable distance: metres under 1 km, else kilometres.
pub fn fmt_distance(m: f64) -> String {
    if m < 950.0 {
        format!("{} m", (m / 10.0).round() as i64 * 10)
    } else {
        format!("{:.1} km", m / 1000.0)
    }
}

/// Human-readable duration from seconds.
pub fn fmt_duration(secs: f64) -> String {
    let total_min = (secs / 60.0).round() as i64;
    if total_min < 60 {
        format!("{total_min} min")
    } else {
        let h = total_min / 60;
        let m = total_min % 60;
        if m == 0 {
            format!("{h} h")
        } else {
            format!("{h} h {m} min")
        }
    }
}

/// Pick a "nice" round distance (m) and its on-screen pixel width for the
/// scale bar at the given latitude/zoom. Returns `(label, pixels)`.
pub fn scale_bar(center: LatLon, zoom: f64, max_px: f64) -> (String, f64) {
    // Metres-per-pixel at this latitude.
    let mpp = (2.0 * PI * EARTH_RADIUS_M * center.lat.to_radians().cos())
        / (TILE_SIZE * 2f64.powf(zoom));
    let max_m = mpp * max_px;
    // Largest nice value ≤ max_m from the 1-2-5 sequence.
    let nice = [
        1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0,
        20000.0, 50000.0, 100000.0,
    ];
    let mut chosen = nice[0];
    for &n in &nice {
        if n <= max_m {
            chosen = n;
        }
    }
    let px = chosen / mpp;
    let label = if chosen >= 1000.0 {
        format!("{} km", (chosen / 1000.0) as i64)
    } else {
        format!("{} m", chosen as i64)
    };
    (label, px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_unproject_roundtrip() {
        let c = LatLon::new(48.1371, 11.5754);
        let proj = Projection::new(c, 14.0, Point::new(640.0, 400.0));
        let back = proj.unproject(proj.project(c));
        assert!((back.lat - c.lat).abs() < 1e-9);
        assert!((back.lon - c.lon).abs() < 1e-9);
        // Centre projects to the viewport centre.
        let p = proj.project(c);
        assert!((p.x - 640.0).abs() < 1e-6 && (p.y - 400.0).abs() < 1e-6);
    }

    #[test]
    fn haversine_known_distance() {
        // ~111 km between one degree of latitude.
        let d = haversine_m(LatLon::new(0.0, 0.0), LatLon::new(1.0, 0.0));
        assert!((d - 111_195.0).abs() < 500.0, "got {d}");
    }

    #[test]
    fn fmt_helpers() {
        assert_eq!(fmt_distance(120.0), "120 m");
        assert_eq!(fmt_distance(1500.0), "1.5 km");
        assert_eq!(fmt_duration(90.0), "2 min");
        assert_eq!(fmt_duration(3720.0), "1 h 2 min");
    }
}
