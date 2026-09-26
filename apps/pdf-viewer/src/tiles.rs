//! Tiled rendering of zoomed pages.
//!
//! A page drawn larger than [`TILE_MIN_PAGE_PX`] device pixels wide is not
//! rendered as one bitmap: it is cut into the engine's fixed
//! [`tilo_pdf::TileGrid`] at a discrete zoom *level* (the scale snapped by
//! [`tilo_pdf::TileGrid::snap_scale`]) and only the tiles in or next to the
//! viewport are rendered. Every tile is bounded, the page is not, so any zoom
//! is sharp.
//!
//! This module holds the pure parts shared by the owner worker and the UI:
//! zoom levels, which tiles a viewport wants and in what order, and a
//! byte-bounded tile cache that keeps the current level's tiles longest.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

use vello::peniko::ImageData as PenikoImage;

/// Pages whose on-screen device width exceeds this are drawn from tiles.
/// Below it the whole-page bitmap is cheap and exact.
pub const TILE_MIN_PAGE_PX: f64 = 2048.0; // bitmap device pixels, not UI spacing

/// Width of the whole-page bitmap requested as the placeholder under tiles
/// that have not arrived yet.
pub const PLACEHOLDER_PAGE_PX: u32 = 1024;

/// At most this many tiles are queued for one viewer at a time. A viewport
/// needs far fewer; the bound keeps a hostile page size from flooding the
/// owner worker.
pub const MAX_TILE_BATCH: usize = 64;

/// Byte budget of a tile cache (owner worker and UI each keep one; the pixels
/// themselves are shared between them).
pub const MAX_TILE_CACHE_BYTES: usize = 128 * 1024 * 1024;

/// A zoom level: `scale = 2^(level / 4)` device pixels per PDF point — the
/// steps [`tilo_pdf::TileGrid::snap_scale`] snaps to. Integral so it can key
/// a cache.
pub type Level = i32;

/// The level a continuous scale snaps to.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is rounded and clamped to a small range first"
)]
pub fn level_for(scale: f64) -> Level {
    if !(scale.is_finite() && scale > 0.0) {
        return 0;
    }
    (scale.log2() * 4.0).round().clamp(-64.0, 64.0) as Level
}

/// The scale of `level`; equal to `TileGrid::snap_scale` of any scale that
/// snaps to it.
pub fn level_scale(level: Level) -> f32 {
    (level as f32 / 4.0).exp2()
}

/// The grid tiles are cut from.
pub fn grid() -> tilo_pdf::TileGrid {
    tilo_pdf::TileGrid::default()
}

/// A page's pixel size at `level`, for its displayed size in points (its
/// recorded rotation applied). Matches what the engine renders.
pub fn page_pixels((width, height): (f64, f64), level: Level) -> (u32, u32) {
    tilo_pdf::RenderOptions {
        scale: level_scale(level),
        ..tilo_pdf::RenderOptions::default()
    }
    .page_pixels(page_size((width, height)))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "page sizes are validated points, far inside f32's range"
)]
fn page_size((width, height): (f64, f64)) -> tilo_pdf::PageSize {
    tilo_pdf::PageSize {
        width: width as f32,
        height: height as f32,
    }
}

/// One tile a viewport wants, on displayed page `page` (0-based).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WantedTile {
    pub page: usize,
    pub level: Level,
    pub col: u32,
    pub row: u32,
    /// The tile's pixel region within the page at `level`.
    pub region: tilo_pdf::PixelRect,
    /// Whether it intersects the viewport (otherwise it is prefetch margin).
    pub visible: bool,
    /// Squared distance of the tile's centre from the viewport's centre, in
    /// screen pixels; the priority within visible and within margin tiles.
    pub distance: f64,
}

/// Where a page's pixels at a level land on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PagePlacement {
    pub page: usize,
    /// Displayed page size in points.
    pub size: (f64, f64),
    pub level: Level,
    /// Screen position of the page's top-left pixel.
    pub origin: (f64, f64),
    /// Screen pixels per page pixel.
    pub screen_per_px: f64,
}

impl PagePlacement {
    /// The screen rectangle `(x0, y0, x1, y1)` of a page pixel region.
    pub fn screen_rect(&self, region: tilo_pdf::PixelRect) -> (f64, f64, f64, f64) {
        let k = self.screen_per_px;
        let x0 = self.origin.0 + f64::from(region.x) * k;
        let y0 = self.origin.1 + f64::from(region.y) * k;
        (
            x0,
            y0,
            x0 + f64::from(region.width) * k,
            y0 + f64::from(region.height) * k,
        )
    }
}

/// The tiles `pages` need to fill `viewport` (`x0, y0, x1, y1` in screen
/// pixels), with a one-tile margin around it for prefetch: every visible tile
/// first, nearest the viewport's centre first, then the margin tiles in the
/// same order.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel coordinates are clamped to the page's u32 extent first"
)]
pub fn wanted_tiles(pages: &[PagePlacement], viewport: (f64, f64, f64, f64)) -> Vec<WantedTile> {
    let grid = grid();
    let tile = f64::from(grid.tile.max(1));
    let centre = (
        f64::midpoint(viewport.0, viewport.2),
        f64::midpoint(viewport.1, viewport.3),
    );
    let mut out = Vec::new();
    for placement in pages {
        let k = placement.screen_per_px;
        if !(k.is_finite() && k > 0.0) {
            continue;
        }
        let (pw, ph) = page_pixels(placement.size, placement.level);
        if pw == 0 || ph == 0 {
            continue;
        }
        // The viewport in page pixels, grown by one tile for the margin.
        let to_px = |v: f64, origin: f64, limit: u32| -> u32 {
            ((v - origin) / k).clamp(0.0, f64::from(limit)) as u32
        };
        let x0 = to_px(viewport.0, placement.origin.0, pw);
        let y0 = to_px(viewport.1, placement.origin.1, ph);
        let x1 = to_px(viewport.2, placement.origin.0, pw);
        let y1 = to_px(viewport.3, placement.origin.1, ph);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let margin = |a: u32| a.saturating_sub(tile as u32);
        let grown = tilo_pdf::PixelRect {
            x: margin(x0),
            y: margin(y0),
            width: x1
                .saturating_add(tile as u32)
                .min(pw)
                .saturating_sub(margin(x0)),
            height: y1
                .saturating_add(tile as u32)
                .min(ph)
                .saturating_sub(margin(y0)),
        };
        let options = tilo_pdf::RenderOptions {
            scale: level_scale(placement.level),
            ..tilo_pdf::RenderOptions::default()
        };
        for t in grid.tiles(page_size(placement.size), &options, grown) {
            let r = t.region;
            let visible = r.x < x1
                && r.x.saturating_add(r.width) > x0
                && r.y < y1
                && r.y.saturating_add(r.height) > y0;
            let (sx0, sy0, sx1, sy1) = placement.screen_rect(r);
            let dx = f64::midpoint(sx0, sx1) - centre.0;
            let dy = f64::midpoint(sy0, sy1) - centre.1;
            out.push(WantedTile {
                page: placement.page,
                level: placement.level,
                col: t.col,
                row: t.row,
                region: r,
                visible,
                distance: dx * dx + dy * dy,
            });
        }
    }
    out.sort_by(|a, b| {
        b.visible
            .cmp(&a.visible)
            .then(a.distance.total_cmp(&b.distance))
    });
    out
}

/// The region of tile `(col, row)` on a page of displayed size `size` at
/// `level`, clipped to the page; `None` outside it.
pub fn tile_region(
    size: (f64, f64),
    level: Level,
    col: u32,
    row: u32,
) -> Option<tilo_pdf::PixelRect> {
    let grid = grid();
    let t = grid.tile.max(1);
    let probe = tilo_pdf::PixelRect {
        x: col.checked_mul(t)?,
        y: row.checked_mul(t)?,
        width: 1,
        height: 1,
    };
    let options = tilo_pdf::RenderOptions {
        scale: level_scale(level),
        ..tilo_pdf::RenderOptions::default()
    };
    grid.tiles(page_size(size), &options, probe)
        .into_iter()
        .find(|tile| tile.col == col && tile.row == row)
        .map(|tile| tile.region)
}

/// A cache key that names the zoom level it belongs to.
pub trait LevelKey: Copy + Eq + Hash {
    fn level(&self) -> Level;
}

/// A byte-bounded LRU of rendered tiles. When space is needed, tiles of
/// other zoom levels are evicted before any tile of the level being filled,
/// so zooming never evicts the tiles on screen for tiles of a level left
/// behind; returning to a level reuses whatever of it survived.
pub struct TileCache<K: LevelKey> {
    images: HashMap<K, (PenikoImage, usize)>,
    order: VecDeque<K>,
    bytes: usize,
    budget: usize,
}

impl<K: LevelKey> TileCache<K> {
    pub fn new(budget: usize) -> Self {
        Self {
            images: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget,
        }
    }

    pub fn get(&mut self, key: &K) -> Option<PenikoImage> {
        let image = self.images.get(key).map(|(image, _)| image.clone())?;
        self.touch(*key);
        Some(image)
    }

    pub fn contains(&self, key: &K) -> bool {
        self.images.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// Keys currently cached, least recently used first.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.order.iter()
    }

    fn touch(&mut self, key: K) {
        if let Some(at) = self.order.iter().position(|k| *k == key) {
            self.order.remove(at);
        }
        self.order.push_back(key);
    }

    fn remove(&mut self, key: &K) {
        if let Some((_, bytes)) = self.images.remove(key) {
            self.bytes = self.bytes.saturating_sub(bytes);
        }
        if let Some(at) = self.order.iter().position(|k| k == key) {
            self.order.remove(at);
        }
    }

    pub fn insert(&mut self, key: K, image: PenikoImage) {
        let bytes = (image.width as usize)
            .saturating_mul(image.height as usize)
            .saturating_mul(4);
        if bytes > self.budget {
            return;
        }
        self.remove(&key);
        while self.bytes.saturating_add(bytes) > self.budget {
            let victim = self
                .order
                .iter()
                .find(|k| k.level() != key.level())
                .or_else(|| self.order.front())
                .copied();
            let Some(victim) = victim else {
                break;
            };
            self.remove(&victim);
        }
        self.images.insert(key, (image, bytes));
        self.order.push_back(key);
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::peniko::{Blob, ImageAlphaType, ImageFormat};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct Key(Level, u32);
    impl LevelKey for Key {
        fn level(&self) -> Level {
            self.0
        }
    }

    fn image(side: u32) -> PenikoImage {
        PenikoImage {
            data: Blob::new(std::sync::Arc::new(vec![0u8; (side * side * 4) as usize])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: side,
            height: side,
        }
    }

    #[test]
    fn tile_grid_levels_regions_and_visible_order_match_the_engine() {
        for scale in [0.3_f64, 1.0, 1.7, 2.0, 5.5, 16.0] {
            let snapped = tilo_pdf::TileGrid::snap_scale(scale as f32);
            assert!((level_scale(level_for(scale)) - snapped).abs() < 1e-4);
        }

        let region = tile_region((612.0, 792.0), 12, 9, 0).expect("edge tile");
        let (pw, _) = page_pixels((612.0, 792.0), 12);
        assert_eq!(region.x + region.width, pw);
        assert!(tile_region((612.0, 792.0), 12, 10, 0).is_none());

        // A US-letter page at level 12 (8 px/pt): 4896 × 6336 px, drawn 1:1.
        let placement = PagePlacement {
            page: 0,
            size: (612.0, 792.0),
            level: 12,
            origin: (0.0, 0.0),
            screen_per_px: 1.0,
        };
        let viewport = (1000.0, 1000.0, 2000.0, 1800.0);
        let tiles = wanted_tiles(&[placement], viewport);
        let first_margin = tiles.iter().position(|t| !t.visible).expect("margin");
        assert!(tiles[..first_margin].iter().all(|t| t.visible));
        assert!(tiles[first_margin..].iter().all(|t| !t.visible));
        for pair in tiles[..first_margin].windows(2) {
            assert!(pair[0].distance <= pair[1].distance);
        }
        // The viewport's centre (1500, 1400) lies in tile (2, 2).
        assert_eq!((tiles[0].col, tiles[0].row), (2, 2));
        // Visible tiles cover the viewport.
        let covered: u64 = tiles[..first_margin]
            .iter()
            .map(|t| {
                let (x0, y0, x1, y1) = placement.screen_rect(t.region);
                let w = (x1.min(viewport.2) - x0.max(viewport.0)).max(0.0);
                let h = (y1.min(viewport.3) - y0.max(viewport.1)).max(0.0);
                (w * h) as u64
            })
            .sum();
        assert_eq!(covered, 1000 * 800);
        // Nothing far outside the margin is asked for.
        assert!(tiles.iter().all(|t| t.col <= 4 && t.row <= 4));
    }

    #[test]
    fn eviction_prefers_other_levels_and_keeps_the_budget() {
        // Room for three 16×16 tiles.
        let mut cache = TileCache::new(3 * 16 * 16 * 4);
        cache.insert(Key(4, 0), image(16));
        cache.insert(Key(8, 0), image(16));
        cache.insert(Key(8, 1), image(16));
        // Filling level 8 evicts the level-4 tile even though a level-8 tile
        // is older in LRU order after this touch.
        assert!(cache.get(&Key(4, 0)).is_some());
        cache.insert(Key(8, 2), image(16));
        assert!(!cache.contains(&Key(4, 0)));
        assert!(cache.contains(&Key(8, 0)) && cache.contains(&Key(8, 1)));
        assert!(cache.bytes() <= 3 * 16 * 16 * 4);
        // With only its own level left, plain LRU applies.
        cache.insert(Key(8, 3), image(16));
        assert!(!cache.contains(&Key(8, 0)));
        assert_eq!(cache.len(), 3);
    }
}
