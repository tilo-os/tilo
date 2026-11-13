//! Web-Mercator projection and geodesic helpers: a [`LatLon`] projects to a screen `Point` around a
//! viewport centre, with the world `256 · 2^zoom` pixels across.

use std::f64::consts::PI;

use vello::kurbo::Point;

use crate::model::LatLon;

const TILE_SIZE: f64 = 256.0;
const EARTH_RADIUS_M: f64 = 6_378_137.0;

fn world_size(zoom: f64) -> f64 {
    TILE_SIZE * 2f64.powf(zoom)
}

/// Clamp latitude to the range Mercator can project.
fn clamp_lat(lat: f64) -> f64 {
    lat.clamp(-85.051_128_78, 85.051_128_78)
}

pub fn lonlat_to_world(c: LatLon, zoom: f64) -> (f64, f64) {
    let size = world_size(zoom);
    let lat = clamp_lat(c.lat).to_radians();
    let x = (c.lon + 180.0) / 360.0 * size;
    let y = (0.5 - (lat.tan() + 1.0 / lat.cos()).ln() / (2.0 * PI)) * size;
    (x, y)
}

pub fn world_to_lonlat(x: f64, y: f64, zoom: f64) -> LatLon {
    let size = world_size(zoom);
    let lon = x / size * 360.0 - 180.0;
    let n = PI - 2.0 * PI * y / size;
    let lat = (0.5 * (n.exp() - (-n).exp())).atan().to_degrees();
    LatLon::new(lat, lon)
}

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

    pub fn project(&self, c: LatLon) -> Point {
        let (wx, wy) = lonlat_to_world(c, self.zoom);
        Point::new(
            self.view_center.x + (wx - self.center_world.0),
            self.view_center.y + (wy - self.center_world.1),
        )
    }

    pub fn unproject(&self, p: Point) -> LatLon {
        let wx = p.x - self.view_center.x + self.center_world.0;
        let wy = p.y - self.view_center.y + self.center_world.1;
        world_to_lonlat(wx, wy, self.zoom)
    }
}

pub fn haversine_m(a: LatLon, b: LatLon) -> f64 {
    let (lat1, lat2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = (b.lat - a.lat).to_radians();
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

pub fn polyline_length_m(pts: &[LatLon]) -> f64 {
    pts.windows(2)
        .map(|w| {
            let [a, b] = w else { return 0.0 };
            haversine_m(*a, *b)
        })
        .sum()
}

/// Simplify a polyline with Ramer–Douglas–Peucker, keeping the vertices that deviate more than
/// `tolerance_m` and always the endpoints, so a route's pucks never move. OSRM and MOTIS emit dense
/// collinear shape points along straight sections; this runs on the fetch worker before the
/// geometry reaches the model. Iterative, so a long route cannot blow the stack.
pub fn simplify_polyline(pts: &[LatLon], tolerance_m: f64) -> Vec<LatLon> {
    let n = pts.len();
    if n <= 2 || tolerance_m <= 0.0 {
        return pts.to_vec();
    }
    let Some(origin) = pts.first() else {
        return pts.to_vec();
    };

    let lat0 = origin.lat.to_radians();
    let m_per_deg = EARTH_RADIUS_M * PI / 180.0;
    let cos_lat0 = lat0.cos();
    let xy = |p: LatLon| -> (f64, f64) {
        (
            (p.lon - origin.lon) * m_per_deg * cos_lat0,
            (p.lat - origin.lat) * m_per_deg,
        )
    };
    let plane: Vec<(f64, f64)> = pts.iter().map(|p| xy(*p)).collect();

    let mut keep = vec![false; n];
    if let Some(k) = keep.first_mut() {
        *k = true;
    }
    if let Some(k) = keep.last_mut() {
        *k = true;
    }
    let mut stack = vec![(0usize, n - 1)];
    let tol2 = tolerance_m * tolerance_m;
    while let Some((lo, hi)) = stack.pop() {
        if hi <= lo + 1 {
            continue;
        }
        let (Some(&p_lo), Some(&p_hi)) = (plane.get(lo), plane.get(hi)) else {
            continue;
        };
        let (mut far, mut far_d2) = (lo, -1.0f64);
        for i in (lo + 1)..hi {
            let Some(&p_cur) = plane.get(i) else { continue };
            let d2 = perp_dist2(p_cur, p_lo, p_hi);
            if d2 > far_d2 {
                far = i;
                far_d2 = d2;
            }
        }
        if far_d2 > tol2 {
            if let Some(k) = keep.get_mut(far) {
                *k = true;
            }
            stack.push((lo, far));
            stack.push((far, hi));
        }
    }

    pts.iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

fn perp_dist2(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 <= f64::EPSILON {
        let (px, py) = (p.0 - a.0, p.1 - a.1);
        return px * px + py * py;
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    let (ex, ey) = (p.0 - cx, p.1 - cy);
    ex * ex + ey * ey
}

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
    let center = LatLon::new(
        f64::midpoint(min_lat, max_lat),
        f64::midpoint(min_lon, max_lon),
    );
    let avail_w = (w - 2.0 * pad).max(64.0);
    let avail_h = (h - 2.0 * pad).max(64.0);

    let mut best = 3.0_f64;
    for step in 0..64 {
        let z = 3.0 + 0.25 * f64::from(step);
        if z >= 19.0 {
            break;
        }
        let nw = lonlat_to_world(LatLon::new(max_lat, min_lon), z);
        let se = lonlat_to_world(LatLon::new(min_lat, max_lon), z);
        let span_x = (se.0 - nw.0).abs();
        let span_y = (se.1 - nw.1).abs();
        if span_x <= avail_w && span_y <= avail_h {
            best = z;
        } else {
            break;
        }
    }
    Some((center, best))
}

/// Distance as a person reads it: metres under 1 km, one decimal to 100 km, whole kilometres
/// beyond, in their number format.
pub fn fmt_distance(m: f64) -> String {
    if m < 950.0 {
        format!("{} m", (m / 10.0).round() as i64 * 10)
    } else if m < 99_950.0 {
        format!("{} km", ui::format::decimal(m / 1000.0, 1))
    } else {
        format!("{} km", ui::format::decimal(m / 1000.0, 0))
    }
}

pub fn fmt_duration(secs: f64) -> String {
    let total_min = (secs / 60.0).round() as i64;
    if total_min < 60 {
        crate::failure::t_args("maps-duration-minutes", &[("minutes", total_min.into())])
    } else {
        let h = total_min / 60;
        let m = total_min % 60;
        if m == 0 {
            crate::failure::t_args("maps-duration-hours", &[("hours", h.into())])
        } else {
            crate::failure::t_args(
                "maps-duration-hours-minutes",
                &[("hours", h.into()), ("minutes", m.into())],
            )
        }
    }
}

pub fn scale_bar(center: LatLon, zoom: f64, max_px: f64) -> (String, f64) {
    let mpp =
        (2.0 * PI * EARTH_RADIUS_M * center.lat.to_radians().cos()) / (TILE_SIZE * 2f64.powf(zoom));
    let max_m = mpp * max_px;
    let nice = [
        1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0,
        20_000.0, 50_000.0, 100_000.0,
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
    fn projection_distance_and_formatting() {
        {
            let c = LatLon::new(48.1371, 11.5754);
            let proj = Projection::new(c, 14.0, Point::new(640.0, 400.0));
            let back = proj.unproject(proj.project(c));
            assert!((back.lat - c.lat).abs() < 1e-9);
            assert!((back.lon - c.lon).abs() < 1e-9);
            let p = proj.project(c);
            assert!((p.x - 640.0).abs() < 1e-6 && (p.y - 400.0).abs() < 1e-6);
        }

        {
            let d = haversine_m(LatLon::new(0.0, 0.0), LatLon::new(1.0, 0.0));
            assert!((d - 111_195.0).abs() < 500.0, "got {d}");
        }

        {
            assert_eq!(fmt_distance(120.0), "120 m");
            assert_eq!(fmt_distance(1500.0), "1.5 km");
            assert_eq!(fmt_distance(1_972_200.0), "1,972 km");
            assert_eq!(fmt_duration(90.0), "2 min");
            assert_eq!(fmt_duration(3720.0), "1 h 2 min");
        }
    }

    #[test]
    fn simplify_drops_collinear_points_keeps_corners_and_handles_edges() {
        {
            let pts: Vec<LatLon> = (0..100)
                .map(|i| LatLon::new(48.0, 11.0 + f64::from(i) * 0.001))
                .collect();
            let out = simplify_polyline(&pts, 3.0);
            assert_eq!(out.len(), 2, "a straight run reduces to its endpoints");
            assert_eq!(out[0], pts[0], "start preserved");
            assert_eq!(out[1], pts[99], "end preserved");
        }

        {
            let mut pts = Vec::new();
            for i in 0..20 {
                pts.push(LatLon::new(48.0, 11.0 + f64::from(i) * 0.001));
            }
            for i in 1..20 {
                pts.push(LatLon::new(48.0 + f64::from(i) * 0.001, 11.019));
            }
            let out = simplify_polyline(&pts, 3.0);
            assert!(
                out.len() >= 3,
                "the corner vertex is retained, got {}",
                out.len()
            );
            assert!(out.len() < pts.len(), "collinear jitter still dropped");
            assert_eq!(out.first(), pts.first());
            assert_eq!(out.last(), pts.last());
            for p in &pts {
                let d = min_dist_to_polyline(*p, &out);
                assert!(d <= 3.5, "deviation {d:.2} m exceeds tolerance");
            }
        }

        {
            let two = vec![LatLon::new(0.0, 0.0), LatLon::new(1.0, 1.0)];
            assert_eq!(
                simplify_polyline(&two, 3.0),
                two,
                "two points are already minimal"
            );
            let many: Vec<LatLon> = (0..10).map(|i| LatLon::new(f64::from(i), 0.0)).collect();
            assert_eq!(
                simplify_polyline(&many, 0.0),
                many,
                "zero tolerance keeps everything"
            );
        }
    }

    fn min_dist_to_polyline(p: LatLon, line: &[LatLon]) -> f64 {
        let m = EARTH_RADIUS_M * std::f64::consts::PI / 180.0;
        let c = line[0].lat.to_radians().cos();
        let xy = |q: LatLon| ((q.lon - line[0].lon) * m * c, (q.lat - line[0].lat) * m);
        let pp = xy(p);
        line.windows(2)
            .map(|w| super::perp_dist2(pp, xy(w[0]), xy(w[1])).sqrt())
            .fold(f64::MAX, f64::min)
    }
}
