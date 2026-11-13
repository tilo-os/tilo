//! Offline-first Places primitives for Photos.

use std::collections::BTreeMap;

use crate::model::{LibraryPhoto, PhotoLocation};

/// Minimum and maximum local map zoom. These are presentation values, never
/// shared state: a collaborator can open the same place without moving your
/// viewport.
pub const MIN_ZOOM: f64 = 1.0;
pub const MAX_ZOOM: f64 = 18.0;

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
}

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

    #[must_use]
    pub fn fitting(points: &[(f64, f64)], width: f64, height: f64, margin: f64) -> Self {
        let Some(&(first_lat, first_lon)) = points.first() else {
            return Self::default();
        };
        let (mut south, mut north, mut west, mut east) =
            (first_lat, first_lat, first_lon, first_lon);
        for &(latitude, longitude) in points {
            south = south.min(latitude);
            north = north.max(latitude);
            west = west.min(longitude);
            east = east.max(longitude);
        }
        let room_x = (width - 2.0 * margin).max(1.0);
        let room_y = (height - 2.0 * margin).max(1.0);
        let fit = (room_x / (east - west).max(f64::EPSILON))
            .min(room_y / (north - south).max(f64::EPSILON));
        let zoom = (fit * 360.0 / 256.0).log2().min(6.0);
        Self {
            latitude: f64::midpoint(south, north),
            longitude: f64::midpoint(west, east),
            zoom,
        }
        .clamped()
    }

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

    #[must_use]
    pub fn unproject(self, x: f64, y: f64, width: f64, height: f64) -> Option<(f64, f64)> {
        if !(x.is_finite() && y.is_finite() && width > 0.0 && height > 0.0) {
            return None;
        }
        let scale = 256.0 * 2.0_f64.powf(self.zoom) / 360.0;
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        Some((
            (self.latitude - (y - height / 2.0) / scale).clamp(-90.0, 90.0),
            wrap_longitude(self.longitude + (x - width / 2.0) / scale),
        ))
    }
}

#[must_use]
pub fn clusters<'a>(photos: impl IntoIterator<Item = &'a LibraryPhoto>) -> Vec<PlaceCluster> {
    let mut grouped = BTreeMap::<String, (String, f64, f64, Vec<String>)>::new();
    for photo in photos {
        let Some(location) = &photo.location else {
            continue;
        };
        if !location.is_valid() {
            continue;
        }
        let key = location.place_key();
        let entry = grouped
            .entry(key.clone())
            .or_insert_with(|| (location.display_name(), 0.0, 0.0, Vec::new()));
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
        let mut photo = LibraryPhoto::from_image(OpenImage::fixture(id, 10, 10));
        photo.location = Some(PhotoLocation {
            latitude,
            longitude,
            label: Some(label.to_owned()),
            provenance: LocationProvenance::default(),
        });
        photo
    }

    #[test]
    fn places_cluster_canonically_and_project_finitely_across_the_dateline() {
        eprintln!("phase clusters_are_canonical_collections_not_zoom_dependent_representatives");
        {
            let photos = vec![
                photo("a.png", 37.7749, -122.4194, "San Francisco"),
                photo("b.png", 37.7749, -122.4194, "San Francisco"),
                photo("c.png", 48.8566, 2.3522, "Paris"),
            ];
            let clusters = clusters(&photos);
            assert_eq!(clusters.iter().map(PlaceCluster::count).sum::<usize>(), 3);
            let san_francisco = clusters
                .iter()
                .find(|cluster| cluster.key == photos[0].location.as_ref().unwrap().place_key())
                .unwrap();
            assert_eq!(san_francisco.count(), 2);
            assert_eq!(
                san_francisco.photo_ids,
                vec![photos[0].id.clone(), photos[1].id.clone()]
            );
        }

        eprintln!("phase pan_zoom_and_projection_are_finite_and_wrap_the_dateline");
        {
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
            let (latitude, longitude) = viewport.unproject(x, y, 1000.0, 600.0).unwrap();
            assert!((latitude - location.latitude).abs() < 0.000_001);
            assert!((longitude - location.longitude).abs() < 0.000_001);
            let moved = viewport.panned(40.0, -20.0, 1000.0, 600.0);
            assert!((MIN_ZOOM..=MAX_ZOOM).contains(&moved.zoom));
            assert!((-180.0..180.0).contains(&moved.longitude));
        }
    }
}
