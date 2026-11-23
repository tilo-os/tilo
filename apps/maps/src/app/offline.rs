//! The names and footprints for map tiles downloaded to this device.

use std::collections::BTreeMap;

use settings_model::{Scope, SettingsSchema};

use super::{GeoBounds, MapsApp, TileId, bounds_contains, failure, format_bytes};
use crate::model::DownloadedRegion;
use crate::offline_areas::recorded;

#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DownloadedAreas {
    #[serde(default)]
    pub(crate) by_workspace: BTreeMap<String, Vec<DownloadedRegion>>,
}

impl SettingsSchema for DownloadedAreas {
    const SCOPE: Scope = Scope::DeviceLocal;
}

impl DownloadedAreas {
    pub(crate) fn in_workspace(&self, workspace: &str) -> &[DownloadedRegion] {
        self.by_workspace.get(workspace).map_or(&[], Vec::as_slice)
    }
}

impl MapsApp {
    /// The tile ids a region covers; set arithmetic on them keeps imagery another area still needs.
    pub(super) fn region_tile_ids(region: &crate::model::DownloadedRegion) -> Vec<TileId> {
        GeoBounds::new(region.min, region.max).tiles_in_range(region.z0, region.z1)
    }

    pub(super) fn region_name(&self, bounds: GeoBounds) -> String {
        if let Some(p) = self
            .model
            .places
            .iter()
            .find(|p| bounds_contains(bounds, GeoBounds::new(p.pos, p.pos)))
        {
            return p.name.clone();
        }
        let lat = f64::midpoint(bounds.min.lat, bounds.max.lat);
        let lon = f64::midpoint(bounds.min.lon, bounds.max.lon);
        format!("{lat:.3}, {lon:.3}")
    }

    pub(super) fn record_region(
        &mut self,
        bounds: GeoBounds,
        z0: u8,
        z1: u8,
        tiles: u32,
        name: Option<String>,
    ) {
        // The footprint is the area's identity, so a second download of the same area refreshes it
        // instead of listing it twice.
        let name = name.unwrap_or_else(|| self.region_name(bounds));
        let id = format!(
            "region:{:.6}:{:.6}:{:.6}:{:.6}:z{z0}-{z1}",
            bounds.min.lat, bounds.min.lon, bounds.max.lat, bounds.max.lon,
        );
        let region = crate::model::DownloadedRegion {
            id,
            name,
            min: bounds.min,
            max: bounds.max,
            z0,
            z1,
            tiles,
        };
        self.edit_offline_areas(|areas| *areas = recorded(areas, region.clone()));
    }

    pub(super) fn delete_region(&mut self, id: &str) -> Result<(), String> {
        let Some(region) = self.model.regions.iter().find(|r| r.id == id).cloned() else {
            return Err(failure::t("maps-offline-area-missing"));
        };
        // Tiles any other saved region still needs.
        let mut keep: std::collections::HashSet<TileId> = std::collections::HashSet::new();
        for r in self.model.regions.iter().filter(|r| r.id != id) {
            keep.extend(Self::region_tile_ids(r));
        }
        let doomed: Vec<TileId> = Self::region_tile_ids(&region)
            .into_iter()
            .filter(|t| !keep.contains(t))
            .collect();
        let freed = self.tiles.delete_tiles(&doomed);
        self.offline_panel.set_cache_bytes(self.tiles.cache_bytes());
        self.edit_offline_areas(|areas| areas.retain(|area| area.id != id));
        self.model.status = failure::t_args(
            "maps-offline-removed",
            &[
                ("name", region.name.into()),
                ("size", format_bytes(freed).into()),
            ],
        );
        Ok(())
    }

    pub(super) fn clear_offline_areas(&mut self) {
        let freed = self.tiles.clear_cache();
        self.offline_panel.set_cache_bytes(0);
        self.edit_offline_areas(Vec::clear);
        self.model.status = failure::t_args(
            "maps-offline-cleared",
            &[("size", format_bytes(freed).into())],
        );
    }

    fn edit_offline_areas(&mut self, edit: impl Fn(&mut Vec<DownloadedRegion>)) {
        if let Some((settings, workspace)) = &self.offline_areas {
            match settings.update(|state| {
                edit(state.by_workspace.entry(workspace.clone()).or_default());
            }) {
                Ok(()) => {
                    self.follow_offline_areas();
                    return;
                }
                Err(error) => tracing::warn!(%error, "could not store offline map areas"),
            }
        }
        edit(&mut self.model.regions);
    }

    pub(super) fn follow_offline_areas(&mut self) {
        if let Some((settings, workspace)) = &self.offline_areas {
            let stored = settings.get();
            let areas = stored.in_workspace(workspace);
            if areas != self.model.regions {
                self.model.regions = areas.to_vec();
            }
        }
    }
}
