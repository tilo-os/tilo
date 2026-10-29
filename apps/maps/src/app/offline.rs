//! The offline areas a person saved: the regions their tiles cover, naming them, and recording, deleting or following the set the resource holds.

use ui::tilo_resource::Invoke;

use super::{GeoBounds, MapsApp, TileId, bounds_contains, failure, format_bytes};
use crate::offline_areas::OfflineAreas;

impl MapsApp {
    /// The tile ids a saved region covers — used for delete/keep set arithmetic
    /// so removing one area doesn't drop imagery another still needs.
    pub(super) fn region_tile_ids(region: &crate::model::DownloadedRegion) -> Vec<TileId> {
        GeoBounds::new(region.min, region.max).tiles_in_range(region.z0, region.z1)
    }

    /// A human label for a downloaded area: a known place inside it when there
    /// is one (reads nicely, e.g. "Marienplatz"), else its centre coordinate.
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

    /// Record the just-requested download as one of this device's offline
    /// areas; the tiles themselves were fetched by the caller. `name`
    /// overrides the derived label (the searched place's name).
    pub(super) fn record_region(
        &mut self,
        bounds: GeoBounds,
        z0: u8,
        z1: u8,
        tiles: u32,
        name: Option<String>,
    ) {
        // The footprint itself is the area's identity, so a second download
        // of the same area refreshes it instead of listing it twice.
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
        let areas = crate::offline_areas::recorded(&self.model.regions, region.clone());
        self.edit_offline_areas(|device| OfflineAreas::RECORD.with((device, region)), areas);
    }

    /// Remove an offline area and delete the tiles no *other* area still
    /// needs. Shared imagery is kept — the tile arithmetic reads the area set
    /// before the area is dropped.
    pub(super) fn delete_region(&mut self, id: &str) -> Result<(), String> {
        let Some(region) = self.model.regions.iter().find(|r| r.id == id).cloned() else {
            return Err(failure::t("maps-offline-area-missing"));
        };
        // Tiles any *other* saved region still needs must survive.
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
        let areas = self
            .model
            .regions
            .iter()
            .filter(|r| r.id != id)
            .cloned()
            .collect();
        let area = id.to_owned();
        self.edit_offline_areas(|device| OfflineAreas::FORGET.with((device, area)), areas);
        self.model.status = failure::t_args(
            "maps-offline-removed",
            &[
                ("name", region.name.into()),
                ("size", format_bytes(freed).into()),
            ],
        );
        Ok(())
    }

    /// Delete every offline tile on this device, and the areas they made up.
    pub(super) fn clear_offline_areas(&mut self) {
        let freed = self.tiles.clear_cache();
        self.offline_panel.set_cache_bytes(0);
        self.edit_offline_areas(|device| OfflineAreas::CLEAR.with(device), Vec::new());
        self.model.status = failure::t_args(
            "maps-offline-cleared",
            &[("size", format_bytes(freed).into())],
        );
    }

    /// Show `areas`, and keep them as this device's offline areas through
    /// the action `edit` makes for the device.
    pub(super) fn edit_offline_areas(
        &mut self,
        edit: impl FnOnce(String) -> Invoke<OfflineAreas>,
        areas: Vec<crate::model::DownloadedRegion>,
    ) {
        if let Some((record, device)) = &self.offline_areas
            && let Err(error) = record.invoke(&edit(device.clone()))
        {
            tracing::warn!("maps: the offline areas could not be stored: {error}");
        }
        self.model.regions = areas;
    }

    /// Show the offline areas another window, another device's sync or a
    /// restart left for this device.
    pub(super) fn follow_offline_areas(&mut self) {
        if let Some((record, device)) = &self.offline_areas {
            let stored = record.with(|areas| areas.on(device));
            if stored != self.model.regions {
                self.model.regions = stored;
            }
        }
    }
}
