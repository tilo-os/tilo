//! Offline-first Places primitives for Photos.
//!
//! The map UI consumes this module rather than a network SDK: every marker is
//! made from explicitly shared [`PhotoLocation`] records, and the projection
//! works with a calm reference grid when no locally cached basemap exists.
//! That is both more useful than an empty error page on a train and more
//! honest than quietly sending a person's photo coordinates to a tile host.

use std::collections::BTreeMap;

use crate::model::{LibraryPhoto, PhotoLocation};

/// Minimum and maximum local map zoom. These are presentation values, never
/// shared state: a collaborator can open the same place without moving your
/// viewport.
pub const MIN_ZOOM: f64 = 1.0;
pub const MAX_ZOOM: f64 = 18.0;

/// One deterministic marker cluster. It retains photo ids rather than image
/// bytes, allowing a marker or a screen reader to open the exact canonical
/// photo resource without materialising every thumbnail.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceCluster {
    pub key: String,
    pub label: String,
    pub latitude: f64,
    pub longitude: f64,
    pub photo_ids: Vec<String>,
}

impl PlaceCluster {
    #[must_use]
    pub fn count(&self) -> usize {
        self.photo_ids.len()
    }

    /// A safe, concise screen-reader name. Exact coordinates are deliberately
    /// not announced by default; a person may choose to inspect them in the
    /// location editor when that precision is actually needed.
    #[must_use]
    pub fn accessibility_label(&self) -> String {
        format!("{}: {} photographs", self.label, self.count())
    }
}

/// Local map camera using WGS-84 degrees. The conversion is Web-Mercator-ish
/// for familiar panning behaviour, but it does not promise navigational
/// accuracy; Photos uses it for photo browsing only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapViewport {
    pub latitude: f64,
    pub longitude: f64,
    pub zoom: f64,
}

impl Default for MapViewport {
    fn default() -> Self {
        Self {
            latitude: 20.0,
            longitude: 0.0,
            zoom: 2.0,
        }
    }
}

impl MapViewport {
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            latitude: self.latitude.clamp(-85.0, 85.0),
            longitude: wrap_longitude(self.longitude),
            zoom: self.zoom.clamp(MIN_ZOOM, MAX_ZOOM),
        }
    }

    /// Pan by CSS/logical pixels in a viewport of `width × height`. This is
    /// intentionally pure so pointer drag, touch drag and keyboard arrows all
    /// lead to identical camera state.
    #[must_use]
    pub fn panned(self, dx: f64, dy: f64, width: f64, height: f64) -> Self {
        if width <= 0.0 || height <= 0.0 {
            return self.clamped();
        }
        let scale = 360.0 / (256.0 * 2.0_f64.powf(self.zoom));
        let latitude = self.latitude + dy * scale;
        let longitude = self.longitude - dx * scale;
        Self {
            latitude,
            longitude,
            ..self
        }
        .clamped()
    }

    /// Zoom around a focus point. The caller can pass the keyboard focus or
    /// pinch centre, keeping the item under it stable rather than jumping to
    /// the map centre.
    #[must_use]
    pub fn zoomed_at(
        self,
        delta: f64,
        focus_x: f64,
        focus_y: f64,
        width: f64,
        height: f64,
    ) -> Self {
        if width <= 0.0 || height <= 0.0 || !delta.is_finite() {
            return self.clamped();
        }
        let next = Self {
            zoom: (self.zoom + delta).clamp(MIN_ZOOM, MAX_ZOOM),
            ..self
        };
        let old_scale = 360.0 / (256.0 * 2.0_f64.powf(self.zoom));
        let next_scale = 360.0 / (256.0 * 2.0_f64.powf(next.zoom));
        let fx = focus_x - width / 2.0;
        let fy = focus_y - height / 2.0;
        Self {
            latitude: self.latitude + fy * (old_scale - next_scale),
            longitude: self.longitude - fx * (old_scale - next_scale),
            ..next
        }
        .clamped()
    }

    /// Project one location into local viewport pixels. `None` means invalid
    /// geometry, never a hidden fallback coordinate.
    #[must_use]
    pub fn project(self, location: &PhotoLocation, width: f64, height: f64) -> Option<(f64, f64)> {
        if !(location.is_valid() && width.is_finite() && height.is_finite()) {
            return None;
        }
        let scale = 256.0 * 2.0_f64.powf(self.zoom) / 360.0;
        let lon_delta = shortest_longitude_delta(location.longitude, self.longitude);
        let x = width / 2.0 + lon_delta * scale;
        let y = height / 2.0 - (location.latitude - self.latitude) * scale;
        Some((x, y))
    }
}

/// Group nearby photos for a readable marker layer. The bucket becomes finer
/// as the user zooms in; at high zoom the key is the exact durable place key
/// so named places remain independently addressable.
#[must_use]
pub fn clusters(photos: &[LibraryPhoto], zoom: f64) -> Vec<PlaceCluster> {
    let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    // At z=2 clusters span ~5.6°, at z=18 they preserve roughly 10m labels.
    let bucket_degrees = (22.5 / 2.0_f64.powf((zoom - 1.0) / 2.0)).clamp(0.000_1, 22.5);
    let mut grouped = BTreeMap::<String, (String, f64, f64, Vec<String>)>::new();
    for photo in photos {
        let Some(location) = &photo.location else {
            continue;
        };
        if !location.is_valid() {
            continue;
        }
        let lat_bucket = (location.latitude / bucket_degrees).floor() as i64;
        let lon_bucket = (location.longitude / bucket_degrees).floor() as i64;
        let key = if zoom >= 13.0 {
            location.place_key()
        } else {
            format!("{lat_bucket}:{lon_bucket}")
        };
        let entry = grouped
            .entry(key.clone())
            .or_insert_with(|| (location.display_name(), 0.0, 0.0, Vec::new()));
        // Prefer the first explicit label: collection order is stable and that
        // avoids random marker titles when several camera records merge.
        entry.1 += location.latitude;
        entry.2 += location.longitude;
        entry.3.push(photo.id.clone());
    }
    grouped
        .into_iter()
        .map(|(key, (label, lat, lon, photo_ids))| {
            let count = photo_ids.len().max(1) as f64;
            PlaceCluster {
                key,
                label,
                latitude: lat / count,
                longitude: lon / count,
                photo_ids,
            }
        })
        .collect()
}

fn wrap_longitude(longitude: f64) -> f64 {
    (longitude + 180.0).rem_euclid(360.0) - 180.0
}

fn shortest_longitude_delta(target: f64, origin: f64) -> f64 {
    wrap_longitude(target - origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LibraryPhoto, LocationProvenance, OpenImage};

    fn photo(id: &str, latitude: f64, longitude: f64, label: &str) -> LibraryPhoto {
        let mut photo = LibraryPhoto::from_image(OpenImage::from_path(id, 10, 10));
        photo.location = Some(PhotoLocation {
            latitude,
            longitude,
            label: Some(label.to_owned()),
            provenance: LocationProvenance::default(),
        });
        photo
    }

    #[test]
    fn clusters_are_deterministic_and_keep_canonical_photo_ids() {
        let photos = vec![
            photo("a.png", 37.7749, -122.4194, "San Francisco"),
            photo("b.png", 37.7750, -122.4195, "San Francisco"),
            photo("c.png", 48.8566, 2.3522, "Paris"),
        ];
        let clusters = clusters(&photos, 14.0);
        assert_eq!(clusters.iter().map(PlaceCluster::count).sum::<usize>(), 3);
        assert!(
            clusters
                .iter()
                .any(|cluster| cluster.photo_ids.contains(&photos[0].id))
        );
        assert!(
            clusters
                .iter()
                .any(|cluster| cluster.accessibility_label().contains("photographs"))
        );
    }

    #[test]
    fn pan_zoom_and_projection_are_finite_and_wrap_the_dateline() {
        let viewport = MapViewport {
            latitude: 0.0,
            longitude: 179.0,
            zoom: 4.0,
        };
        let location = PhotoLocation {
            latitude: 1.0,
            longitude: -179.0,
            label: Some("Dateline".to_owned()),
            provenance: LocationProvenance::default(),
        };
        let (x, y) = viewport.project(&location, 1000.0, 600.0).unwrap();
        assert!(
            x > 450.0 && x < 550.0,
            "dateline should project nearby, got {x}"
        );
        assert!(y.is_finite());
        let moved = viewport
            .panned(40.0, -20.0, 1000.0, 600.0)
            .zoomed_at(1.0, x, y, 1000.0, 600.0);
        assert!((MIN_ZOOM..=MAX_ZOOM).contains(&moved.zoom));
        assert!((-180.0..180.0).contains(&moved.longitude));
    }
}
