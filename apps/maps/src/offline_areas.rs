//! The areas this device downloaded for offline use.
//!
//! The tiles of an offline area live in this device's tile cache, so the list
//! of areas is this device's too: a device-local setting, never part of the
//! shared map. A collaborator's device has its own cache and its own list, a
//! deletion here frees tiles here, and nothing an undo restores can name tiles
//! that are gone.

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

use crate::model::DownloadedRegion;

/// The offline areas downloaded on this device, newest last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OfflineAreas {
    #[serde(default)]
    pub offline_areas: Vec<DownloadedRegion>,
}

impl SettingsSchema for OfflineAreas {
    const SCOPE: Scope = Scope::DeviceLocal;
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
