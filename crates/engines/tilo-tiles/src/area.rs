//! An area of the map a person keeps for offline use: the tiles it covers, at the zoom levels
//! they chose, fetched and kept whole so a trip with no network still shows the area.
//!
//! What is kept is listed beside the pictures themselves (`offline/areas.json`), so the areas a
//! device holds survive a restart and can be listed and removed as such, not only as anonymous
//! pictures. Removing one gives up only the tiles no other kept area still needs.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "tile levels, columns and rows are small counts; a zoom is a whole number"
)]

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::tiles::Tiles;

/// The closest zoom any basemap's tiles go: past it a tile is not one the map asks for.
pub const MOST_ZOOM: i64 = 19;

/// What the kept areas are listed in, under the saved pictures' own directory.
const MANIFEST: &str = "areas.json";

/// About how many bytes one map picture takes, for quoting an area before its pictures are
/// kept: what the maps app itself estimated a tile at before this device had kept any.
pub const AVERAGE_TILE_BYTES: u64 = 18_000;

/// An area of the map kept on this device for offline use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Area {
    /// The area's stable key: its bounds and the zoom levels it keeps.
    pub id: String,
    /// What people call it.
    pub name: String,
    /// The latitude of its northern edge, in degrees.
    pub north: f64,
    /// The latitude of its southern edge, in degrees.
    pub south: f64,
    /// The longitude of its eastern edge, in degrees.
    pub east: f64,
    /// The longitude of its western edge, in degrees.
    pub west: f64,
    /// The furthest zoom level out whose tiles it keeps.
    pub least_zoom: i64,
    /// The closest zoom level in whose tiles it keeps.
    pub most_zoom: i64,
    /// The tile address the area's tiles come from, with `%z`, `%x` and `%y` standing for a
    /// tile's level, column and row.
    pub basemap: String,
}

/// How far keeping an area has come: the tiles kept so far, of how many.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// The tiles already on the device.
    pub saved: u64,
    /// Every tile of the area at the levels it keeps.
    pub total: u64,
}

/// The ground an area covers, and the zoom levels whose tiles it keeps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AreaBounds {
    /// The latitude of the area's northern edge, in degrees.
    pub north: f64,
    /// The latitude of the area's southern edge, in degrees.
    pub south: f64,
    /// The longitude of the area's eastern edge, in degrees.
    pub east: f64,
    /// The longitude of the area's western edge, in degrees.
    pub west: f64,
    /// The furthest zoom level out whose tiles it keeps.
    pub least_zoom: i64,
    /// The closest zoom level in whose tiles it keeps.
    pub most_zoom: i64,
}

impl Area {
    /// The area a view names, with the levels clamped to the ones a basemap has tiles for.
    #[must_use]
    pub fn new(id: String, name: String, bounds: AreaBounds, basemap: String) -> Self {
        Self {
            id,
            name,
            north: bounds.north,
            south: bounds.south,
            east: bounds.east,
            west: bounds.west,
            least_zoom: bounds.least_zoom.clamp(0, MOST_ZOOM),
            most_zoom: bounds.most_zoom.clamp(0, MOST_ZOOM),
            basemap,
        }
    }

    /// The tiles the area covers, as its level, column and row, coarsest level first.
    #[must_use]
    pub fn tiles(&self) -> Vec<(i64, i64, i64)> {
        let mut tiles = Vec::new();
        let (least, most) = (self.least_zoom, self.most_zoom.max(self.least_zoom));
        for z in least..=most {
            let columns = 1_i64 << z;
            let column = |longitude: f64| {
                ((longitude.clamp(-180.0, 180.0) + 180.0) / 360.0 * columns as f64).floor() as i64
            };
            let row = |latitude: f64| {
                let lat = latitude
                    .clamp(-85.051_128_779_806_6, 85.051_128_779_806_6)
                    .to_radians();
                let sine = lat.sin();
                let share = 0.5 - ((1.0 + sine) / (1.0 - sine)).ln() / (4.0 * std::f64::consts::PI);
                (share * columns as f64).floor() as i64
            };
            let edge = columns.saturating_sub(1);
            let first_x = column(self.west.min(self.east)).clamp(0, edge);
            let last_x = column(self.west.max(self.east)).clamp(first_x, edge);
            let first_y = row(self.north.max(self.south)).clamp(0, edge);
            let last_y = row(self.north.min(self.south)).clamp(first_y, edge);
            for y in first_y..=last_y {
                for x in first_x..=last_x {
                    tiles.push((z, x, y));
                }
            }
        }
        tiles
    }

    /// The address of the tile at `z`, `x` and `y`, with the basemap's own template filled in.
    #[must_use]
    pub fn address(&self, z: i64, x: i64, y: i64) -> String {
        self.basemap
            .replace("%z", &z.to_string())
            .replace("%x", &x.to_string())
            .replace("%y", &y.to_string())
    }

    /// Every address the area's tiles are kept at.
    #[must_use]
    pub fn addresses(&self) -> BTreeSet<String> {
        self.tiles()
            .into_iter()
            .map(|(z, x, y)| self.address(z, x, y))
            .collect()
    }

    /// How many tiles the area covers.
    #[must_use]
    pub fn tile_count(&self) -> u64 {
        self.tiles().len() as u64
    }
}

impl Tiles {
    /// How many bytes the area's kept pictures take on this device now; a picture another kept
    /// area shares counts for both.
    #[must_use]
    pub fn area_bytes(&self, area: &Area) -> u64 {
        let dir = self.saved_dir();
        area.addresses()
            .into_iter()
            .filter_map(|address| std::fs::metadata(Self::path_in(&dir, &address)).ok())
            .map(|meta| if meta.is_file() { meta.len() } else { 0 })
            .sum()
    }

    /// About how many bytes keeping an area of `tiles` pictures takes: their number times the
    /// size this device's kept pictures average, or [`AVERAGE_TILE_BYTES`] before it kept any.
    #[must_use]
    pub fn estimated_bytes(&self, tiles: u64) -> u64 {
        tiles.saturating_mul(self.average_tile_bytes())
    }

    /// The bytes one kept picture takes on average on this device, as the pictures it keeps now
    /// measure; [`AVERAGE_TILE_BYTES`] when none was kept yet.
    #[must_use]
    pub fn average_tile_bytes(&self) -> u64 {
        let Ok(entries) = std::fs::read_dir(self.saved_dir()) else {
            return AVERAGE_TILE_BYTES;
        };
        let mut files = 0_u64;
        let mut bytes = 0_u64;
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|kind| kind == "img")
                && let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(meta.len());
            }
        }
        if files == 0 {
            AVERAGE_TILE_BYTES
        } else {
            bytes.checked_div(files).unwrap_or(AVERAGE_TILE_BYTES)
        }
    }

    /// Fetch and keep every tile of `area`, so it shows with no network. `stop` is asked between
    /// tiles, so the person can stop it and keep what it got; `progress` is told how far it has
    /// come every so often, and once at the end. A tile already kept is not fetched again, so
    /// continuing a stopped area only gets the rest.
    pub fn keep_area(
        &self,
        area: &Area,
        stop: &dyn Fn() -> bool,
        mut progress: impl FnMut(Kept),
    ) -> Kept {
        let tiles = area.tiles();
        let total = tiles.len() as u64;
        let mut kept = Kept { saved: 0, total };
        self.record_area(area);
        for (done, (z, x, y)) in tiles.into_iter().enumerate() {
            if stop() {
                break;
            }
            if self.save(&area.address(z, x, y)).is_some() {
                kept.saved = kept.saved.saturating_add(1);
            }
            let done = (done as u64).saturating_add(1);
            if done.is_multiple_of(16) {
                progress(kept);
            }
        }
        progress(kept);
        kept
    }

    /// The areas this device keeps for offline use, as it listed them.
    #[must_use]
    pub fn kept_areas(&self) -> Vec<Area> {
        read_manifest(&self.saved_dir())
    }

    /// Give up the area `id`, answering how many bytes that freed. Tiles another kept area still
    /// needs stay; an area this device does not hold frees nothing.
    pub fn forget_area(&self, id: &str) -> u64 {
        let dir = self.saved_dir();
        let mut areas = read_manifest(&dir);
        let Some(at) = areas.iter().position(|area| area.id == id) else {
            return 0;
        };
        let gone = areas.remove(at);
        let still_needed: BTreeSet<String> = areas.iter().flat_map(Area::addresses).collect();
        let mut freed = 0_u64;
        for address in gone.addresses() {
            if still_needed.contains(&address) {
                continue;
            }
            let path = Self::path_in(&dir, &address);
            if let Ok(meta) = std::fs::metadata(&path)
                && std::fs::remove_file(&path).is_ok()
            {
                freed = freed.saturating_add(meta.len());
            }
        }
        write_manifest(&dir, &areas);
        freed
    }

    /// List `area` among what this device keeps, so it is listed even while it downloads and
    /// after the process that fetched it ends.
    fn record_area(&self, area: &Area) {
        let dir = self.saved_dir();
        let mut areas = read_manifest(&dir);
        if let Some(held) = areas.iter_mut().find(|held| held.id == area.id) {
            *held = area.clone();
        } else {
            areas.push(area.clone());
        }
        write_manifest(&dir, &areas);
    }
}

/// The areas listed under `dir`, an empty list where none were kept yet.
fn read_manifest(dir: &Path) -> Vec<Area> {
    std::fs::read_to_string(dir.join(MANIFEST))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write `areas` as the list under `dir`, beside and moved into place so a reader never sees
/// half a list.
fn write_manifest(dir: &Path, areas: &[Area]) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join(MANIFEST);
    let beside = path.with_extension("part");
    let Ok(text) = serde_json::to_string_pretty(areas) else {
        return;
    };
    if std::fs::write(&beside, text).is_err() || std::fs::rename(&beside, &path).is_err() {
        let _ = std::fs::remove_file(&beside);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(least: i64, most: i64) -> Area {
        Area::new(
            "region:48.10:11.55:48.18:11.62:z14-15".to_owned(),
            "Munich".to_owned(),
            AreaBounds {
                north: 48.18,
                south: 48.10,
                east: 11.62,
                west: 11.55,
                least_zoom: least,
                most_zoom: most,
            },
            "https://tiles.test/%z/%x/%y.png".to_owned(),
        )
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tilo-tiles-area-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// An area's tiles are the columns and rows its bounds and levels cover, each addressed by
    /// its own level, column and row.
    #[test]
    fn an_area_covers_the_tiles_of_its_bounds_and_levels() {
        let one = area(14, 14);
        let tiles = one.tiles();
        assert!(!tiles.is_empty());
        assert!(tiles.iter().all(|(z, _, _)| *z == 14));
        let two = area(14, 15);
        assert!(
            two.tile_count() > tiles.len() as u64,
            "a second level covers more tiles"
        );
        assert!(
            two.tiles().iter().any(|(z, _, _)| *z == 15),
            "the closer level is kept too"
        );
        let address = one.address(14, 8671, 5746);
        assert_eq!(address, "https://tiles.test/14/8671/5746.png");
        assert_eq!(one.addresses().len(), one.tiles().len());
    }

    /// A kept area is listed, survives a restart, and its pictures answer from the saved store
    /// once the cache is emptied: what a person saved shows with no network, because it is read
    /// before anything is asked for.
    #[test]
    fn a_kept_area_is_listed_and_shows_with_the_network_off() {
        let dir = scratch("keep");
        let mut unreachable = area(14, 14);
        // A port nothing listens on: if keeping an area asked the network, it would fail here.
        unreachable.basemap = "http://127.0.0.1:1/%z/%x/%y.png".to_owned();
        let tiles = Tiles::open(dir.clone());
        // What the map looked at before the person saved the area, as the window's cache holds it.
        let mut first = (0, 0, 0);
        for (z, x, y) in unreachable.tiles() {
            let address = unreachable.address(z, x, y);
            let cached = Tiles::path_in(&tiles.cache_dir(), &address);
            std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
            std::fs::write(&cached, format!("tile {z}/{x}/{y}")).unwrap();
            first = (z, x, y);
        }
        let kept = tiles.keep_area(&unreachable, &|| false, |_| {});
        assert_eq!(
            kept.saved, kept.total,
            "every tile came from the window's cache"
        );
        assert!(tiles.clear_cache() > 0);
        // The cache is gone and the tile host is not reachable: the saved picture still answers.
        let (z, x, y) = first;
        let shown = tiles
            .bytes(&unreachable.address(z, x, y))
            .expect("the saved picture shows with no network");
        assert_eq!(shown, format!("tile {z}/{x}/{y}").as_bytes());
        // The list is read from the disk, as a second process would read it.
        let listed = Tiles::open(dir.clone()).kept_areas();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Munich");
        assert_eq!(listed[0].least_zoom, 14);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Removing one area gives up only the tiles no other kept area still needs.
    #[test]
    fn forgetting_an_area_keeps_what_another_still_needs() {
        let dir = scratch("forget");
        let tiles = Tiles::open(dir.clone());
        let big = area(14, 14);
        let mut small = big.clone();
        small.id = "region:small".to_owned();
        small.name = "Small".to_owned();
        small.north = 48.14;
        small.south = 48.13;
        small.east = 11.58;
        small.west = 11.57;
        // Seed both areas' pictures as if they had been fetched.
        for address in big.addresses() {
            let path = Tiles::path_in(&tiles.saved_dir(), &address);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"tile").unwrap();
        }
        tiles.record_area(&big);
        tiles.record_area(&small);
        let shared = small.addresses();
        assert!(
            tiles.forget_area(&big.id) > 0,
            "the tiles only it used are given up"
        );
        assert_eq!(tiles.kept_areas(), vec![small.clone()]);
        for address in shared {
            assert!(
                Tiles::path_in(&tiles.saved_dir(), &address).exists(),
                "a tile the kept area still needs stays: {address}"
            );
        }
        assert!(tiles.forget_area(&small.id) > 0);
        assert!(tiles.kept_areas().is_empty());
        assert_eq!(
            tiles.saved_bytes(),
            0,
            "no picture of a forgotten area stays"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An area quotes its stored bytes, and about what it will take from the size this device's
    /// kept pictures average, before any of its own are kept.
    #[test]
    fn an_areas_size_is_quoted_before_and_after_its_pictures_are_kept() {
        let dir = scratch("sizes");
        let tiles = Tiles::open(dir.clone());
        let held = area(12, 12);
        assert_eq!(
            tiles.average_tile_bytes(),
            AVERAGE_TILE_BYTES,
            "with nothing kept, a picture is the map's own average"
        );
        assert_eq!(
            tiles.estimated_bytes(held.tile_count()),
            held.tile_count() * AVERAGE_TILE_BYTES,
            "an area quotes its tile count at that average"
        );
        // Two of its pictures are kept, at sizes the device can measure.
        let addresses: Vec<String> = held.addresses().into_iter().take(2).collect();
        for (at, address) in addresses.iter().enumerate() {
            let path = Tiles::path_in(&tiles.saved_dir(), address);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![0_u8; 100 + at * 200]).unwrap();
        }
        tiles.record_area(&held);
        assert_eq!(
            tiles.area_bytes(&held),
            400,
            "the area's kept pictures are counted, and only theirs"
        );
        assert_eq!(
            tiles.average_tile_bytes(),
            200,
            "the average is the pictures this device keeps"
        );
        assert_eq!(
            tiles.estimated_bytes(held.tile_count()),
            held.tile_count() * 200,
            "an estimate uses the device's own average once it has one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
