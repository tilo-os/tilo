//! The areas each device downloaded for offline use.
//!
//! The tiles of an offline area live in one device's tile cache, so each
//! device keeps its own list of areas, never part of the shared map: a
//! record Maps keeps beside the map in the workspace ([`OfflineAreas`]),
//! each device reading and writing only its own list. Another device has
//! its own cache and its own list, and a deletion here frees tiles here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::prelude::{Model, resource, tilo_resource};

use crate::model::DownloadedRegion;

/// The offline areas one device downloaded, newest last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceAreas {
    #[serde(default)]
    pub areas: Vec<DownloadedRegion>,
}

tilo_resource::json_model!(DeviceAreas);

/// The offline areas, by the device whose tile cache holds them.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct OfflineAreas {
    pub devices: BTreeMap<String, DeviceAreas>,
}

#[resource(kind = "maps.offline-areas", label = "Offline areas")]
impl OfflineAreas {
    /// `region` was downloaded on `device`: a download of the same footprint
    /// refreshes that area instead of listing it twice.
    #[action(housekeeping)]
    pub fn record(&mut self, device: String, region: DownloadedRegion) {
        let kept = self.devices.entry(device).or_default();
        kept.areas = recorded(&kept.areas, region);
    }

    /// The area `area` on `device` was deleted with its tiles.
    #[action(housekeeping)]
    pub fn forget(&mut self, device: String, area: String) {
        if let Some(kept) = self.devices.get_mut(&device) {
            kept.areas.retain(|region| region.id != area);
        }
    }

    /// Every offline tile on `device` was deleted, and with them its areas.
    #[action(housekeeping)]
    pub fn clear(&mut self, device: String) {
        self.devices.remove(&device);
    }
}

impl OfflineAreas {
    /// The areas `device` downloaded, newest last.
    #[must_use]
    pub fn on(&self, device: &str) -> Vec<DownloadedRegion> {
        self.devices
            .get(device)
            .map(|kept| kept.areas.clone())
            .unwrap_or_default()
    }
}

/// `areas` with `region` recorded: a download of the same footprint (bounds
/// and zoom range) refreshes that area instead of listing it twice.
pub(crate) fn recorded(
    areas: &[DownloadedRegion],
    region: DownloadedRegion,
) -> Vec<DownloadedRegion> {
    let mut areas: Vec<_> = areas
        .iter()
        .filter(|area| !same_footprint(area, &region))
        .cloned()
        .collect();
    areas.push(region);
    areas
}

/// Whether two areas cover the identical footprint (bounds + zoom range).
fn same_footprint(a: &DownloadedRegion, b: &DownloadedRegion) -> bool {
    const EPS: f64 = 1e-6;
    a.z0 == b.z0
        && a.z1 == b.z1
        && (a.min.lat - b.min.lat).abs() < EPS
        && (a.min.lon - b.min.lon).abs() < EPS
        && (a.max.lat - b.max.lat).abs() < EPS
        && (a.max.lon - b.max.lon).abs() < EPS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LatLon;

    fn area(id: &str, z1: u8) -> DownloadedRegion {
        DownloadedRegion {
            id: id.to_string(),
            name: id.to_string(),
            min: LatLon::new(48.10, 11.55),
            max: LatLon::new(48.18, 11.62),
            z0: 14,
            z1,
            tiles: 42,
        }
    }

    #[test]
    fn a_second_download_of_one_footprint_refreshes_its_area() {
        let areas = recorded(&[], area("first", 16));
        let areas = recorded(&areas, area("deeper", 17));
        assert_eq!(areas.len(), 2, "another zoom range is another area");
        let areas = recorded(&areas, area("again", 16));
        let ids: Vec<_> = areas.iter().map(|area| area.id.as_str()).collect();
        assert_eq!(ids, ["deeper", "again"]);
    }
}
