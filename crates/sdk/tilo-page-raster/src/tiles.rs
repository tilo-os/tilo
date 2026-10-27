//! Tiled rendering of zoomed pages.
//!
//! A page drawn larger than [`TILE_MIN_PAGE_PX`] device pixels wide is not
//! rendered as one bitmap: it is cut into a fixed grid of [`TILE_PX`] square
//! tiles at a discrete zoom *level* and only the tiles in or next to the
//! viewport are rendered. Every tile is bounded, the page is not, so any zoom
//! is sharp.
//!
//! This module holds the parts shared by the workers and the UI: zoom levels,
//! which tiles a viewport wants and in what order, a byte-bounded tile cache
//! that keeps the current level's tiles longest, and painting a page's tiles
//! over the tiles of other levels still held.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::ImageData as PenikoImage;

/// Pages whose on-screen device width exceeds this are drawn from tiles.
/// Below it the whole-page bitmap is cheap and exact.
pub const TILE_MIN_PAGE_PX: f64 = 2048.0; // bitmap device pixels, not UI spacing

/// Width of the whole-page bitmap requested as the placeholder under tiles
/// that have not arrived yet.
pub const PLACEHOLDER_PAGE_PX: u32 = 1024;

/// Thumbnails are rendered at most this wide; anything wider is a page.
pub const THUMB_MAX_PX: u32 = 600;

/// At most this many tiles are queued for one viewer at a time. A viewport
/// needs far fewer; the bound keeps a hostile page size from flooding the
/// worker.
pub const MAX_TILE_BATCH: usize = 64;

/// Byte budget of a tile cache (a worker and its UI may each keep one; the
/// pixels themselves are shared between them).
pub const MAX_TILE_CACHE_BYTES: usize = 128 * 1024 * 1024;

/// Edge length of a square tile in device pixels.
pub const TILE_PX: u32 = 512;

/// A zoom level: `scale = 2^(level / 4)` device pixels per page point, so a
/// pinch reuses tiles until the scale moves by about 19%. Integral so it can
/// key a cache.
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

/// The scale of `level`, in device pixels per page point.
#[allow(
    clippy::cast_precision_loss,
    reason = "levels are clamped to ±64, exact in f32"
)]
pub fn level_scale(level: Level) -> f32 {
    (level as f32 / 4.0).exp2()
}

/// A rectangle of page pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One grid cell of a page at a level, clipped to the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tile {
    pub col: u32,
    pub row: u32,
    pub region: PixelRect,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "rounded, positive and clamped to u32 first"
)]
fn px(v: f32) -> u32 {
    if v.is_finite() && v > 0.0 {
        v.round().min(u32::MAX as f32) as u32
    } else {
        0
    }
}

/// A page's pixel size at `level`, for its displayed size in points.
#[allow(
    clippy::cast_possible_truncation,
    reason = "page sizes are validated points, far inside f32's range"
)]
pub fn page_pixels((width, height): (f64, f64), level: Level) -> (u32, u32) {
    let scale = level_scale(level);
    (px(width as f32 * scale), px(height as f32 * scale))
}

/// The tiles of a page `page_px` pixels in size that meet `visible`,
/// row-major. Edge tiles are clipped to the page.
pub fn tiles_meeting((page_w, page_h): (u32, u32), visible: PixelRect) -> Vec<Tile> {
    let t = TILE_PX;
    if page_w == 0 || page_h == 0 {
        return Vec::new();
    }
    let x_end = visible.x.saturating_add(visible.width).min(page_w);
    let y_end = visible.y.saturating_add(visible.height).min(page_h);
    let mut out = Vec::new();
    let mut row = visible.y / t;
    while row.saturating_mul(t) < y_end {
        let mut col = visible.x / t;
        while col.saturating_mul(t) < x_end {
            let left = col.saturating_mul(t);
            let top = row.saturating_mul(t);
            out.push(Tile {
                col,
                row,
                region: PixelRect {
                    x: left,
                    y: top,
                    width: t.min(page_w.saturating_sub(left)),
                    height: t.min(page_h.saturating_sub(top)),
                },
            });
            col = col.saturating_add(1);
        }
        row = row.saturating_add(1);
    }
    out
}

/// One tile a viewport wants, on displayed page `page` (0-based).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WantedTile {
    pub page: usize,
    pub level: Level,
    pub col: u32,
    pub row: u32,
    /// The tile's pixel region within the page at `level`.
    pub region: PixelRect,
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
    pub fn screen_rect(&self, region: PixelRect) -> (f64, f64, f64, f64) {
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
    let tile = TILE_PX;
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
        let grown = PixelRect {
            x: x0.saturating_sub(tile),
            y: y0.saturating_sub(tile),
            width: x1
                .saturating_add(tile)
                .min(pw)
                .saturating_sub(x0.saturating_sub(tile)),
            height: y1
                .saturating_add(tile)
                .min(ph)
                .saturating_sub(y0.saturating_sub(tile)),
        };
        for t in tiles_meeting((pw, ph), grown) {
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
pub fn tile_region(size: (f64, f64), level: Level, col: u32, row: u32) -> Option<PixelRect> {
    let probe = PixelRect {
        x: col.checked_mul(TILE_PX)?,
        y: row.checked_mul(TILE_PX)?,
        width: 1,
        height: 1,
    };
    tiles_meeting(page_pixels(size, level), probe)
        .into_iter()
        .find(|tile| tile.col == col && tile.row == row)
        .map(|tile| tile.region)
}

/// A cache key that names the zoom level it belongs to.
pub trait LevelKey: Copy + Eq + Hash {
    fn level(&self) -> Level;
}

/// A tile a viewer holds: displayed page (0-based), zoom level, column, row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: usize,
    pub level: Level,
    pub col: u32,
    pub row: u32,
}

impl LevelKey for TileKey {
    fn level(&self) -> Level {
        self.level
    }
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

    /// Keep the tiles `map` gives a new key, under it, in their LRU order;
    /// drop the rest.
    pub fn carry(&mut self, map: impl Fn(&K) -> Option<K>) {
        let order = std::mem::take(&mut self.order);
        let mut images = std::mem::take(&mut self.images);
        self.bytes = 0;
        for key in order {
            let (Some(to), Some(entry)) = (map(&key), images.remove(&key)) else {
                continue;
            };
            if self.images.contains_key(&to) {
                continue;
            }
            self.bytes = self.bytes.saturating_add(entry.1);
            self.images.insert(to, entry);
            self.order.push_back(to);
        }
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

/// The tiles available for a zoomed page this frame, and where the page's
/// pixels at their zoom level land on screen.
#[derive(Clone, Debug)]
pub struct PageTiles {
    pub placement: PagePlacement,
    /// Tiles of other zoom levels still held, each with its own screen
    /// pixels per tile pixel, painted first (farthest level first) where the
    /// current level's tiles have not arrived.
    pub under: Vec<(f64, PixelRect, PenikoImage)>,
    pub tiles: Vec<(PixelRect, PenikoImage)>,
}

impl PageTiles {
    pub fn new(placement: PagePlacement) -> Self {
        Self {
            placement,
            under: Vec::new(),
            tiles: Vec::new(),
        }
    }

    /// Paint the tiles, each at its own pixel region, over whatever
    /// placeholder is already drawn.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "affine composition, not integer arithmetic"
    )]
    pub fn paint(&self, scene: &mut Scene, base: Affine) {
        for (k, region, image) in &self.under {
            let placement = PagePlacement {
                screen_per_px: *k,
                ..self.placement
            };
            let (x0, y0, _, _) = placement.screen_rect(*region);
            scene.draw_image(
                image,
                base * Affine::translate((x0, y0)) * Affine::scale_non_uniform(*k, *k),
            );
        }
        let k = self.placement.screen_per_px;
        for (region, image) in &self.tiles {
            let (x0, y0, _, _) = self.placement.screen_rect(*region);
            scene.draw_image(
                image,
                base * Affine::translate((x0, y0)) * Affine::scale_non_uniform(k, k),
            );
        }
    }
}

/// What a frame's tiled pages still need: the tiles to ask the worker for,
/// most important first, and whether a visible one is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TilePlan {
    pub batch: Vec<TileKey>,
    pub missing: bool,
}

/// Fill `page_tiles` (keyed by page index plus `key_offset`) with the tiles
/// of `wanted` held in `cache`, and under any page still missing tiles at
/// its level, the tiles other levels left on it within `view` (`x0, y0, x1,
/// y1`), nearest level last (on top), so a zoom keeps showing the last sharp
/// pixels until the new level's arrive. The tiles not held make the batch.
pub fn plan_tiles<S: std::hash::BuildHasher>(
    wanted: &[WantedTile],
    cache: &mut TileCache<TileKey>,
    view: (f64, f64, f64, f64),
    page_tiles: &mut HashMap<usize, PageTiles, S>,
    key_offset: usize,
) -> TilePlan {
    let mut plan = TilePlan::default();
    let mut missing_on: HashSet<usize> = HashSet::new();
    for tile in wanted {
        let key = TileKey {
            page: tile.page,
            level: tile.level,
            col: tile.col,
            row: tile.row,
        };
        if let Some(image) = cache.get(&key) {
            if tile.visible
                && let Some(entry) = page_tiles.get_mut(&tile.page.saturating_add(key_offset))
            {
                entry.tiles.push((tile.region, image));
            }
        } else {
            if tile.visible {
                plan.missing = true;
                missing_on.insert(tile.page);
            }
            if plan.batch.len() < MAX_TILE_BATCH {
                plan.batch.push(key);
            }
        }
    }
    for page in missing_on {
        let Some(entry) = page_tiles.get_mut(&page.saturating_add(key_offset)) else {
            continue;
        };
        let current = entry.placement.level;
        let mut held: Vec<TileKey> = cache
            .keys()
            .filter(|key| key.page == page && key.level != current)
            .copied()
            .collect();
        held.sort_by_key(|key| std::cmp::Reverse(key.level.abs_diff(current)));
        for key in held {
            let Some(region) = tile_region(entry.placement.size, key.level, key.col, key.row)
            else {
                continue;
            };
            let placement = PagePlacement {
                level: key.level,
                screen_per_px: entry.placement.screen_per_px * f64::from(level_scale(current))
                    / f64::from(level_scale(key.level)),
                ..entry.placement
            };
            let (x0, y0, x1, y1) = placement.screen_rect(region);
            if x1 < view.0 || x0 > view.2 || y1 < view.1 || y0 > view.3 {
                continue;
            }
            if let Some(image) = cache.get(&key) {
                entry.under.push((placement.screen_per_px, region, image));
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct Key(Level, u32);
    impl LevelKey for Key {
        fn level(&self) -> Level {
            self.0
        }
    }

    fn image(side: u32) -> PenikoImage {
        PenikoImage {
            data: vec![0u8; usize::try_from(side.pow(2).saturating_mul(4)).unwrap_or(0)].into(),
            format: vello::peniko::ImageFormat::Rgba8,
            alpha_type: vello::peniko::ImageAlphaType::Alpha,
            width: side,
            height: side,
        }
    }

    #[test]
    fn tile_grid_regions_and_visible_order() {
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
        let covered: f64 = tiles[..first_margin]
            .iter()
            .map(|t| {
                let (x0, y0, x1, y1) = placement.screen_rect(t.region);
                let w = (x1.min(viewport.2) - x0.max(viewport.0)).max(0.0);
                let h = (y1.min(viewport.3) - y0.max(viewport.1)).max(0.0);
                w * h
            })
            .sum();
        assert!((covered - 800_000.0).abs() < 0.5);
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

        cache.carry(|key| (key.1 != 2).then_some(Key(key.0, key.1 + 10)));
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(&Key(8, 11)) && cache.contains(&Key(8, 13)));
        assert_eq!(cache.bytes(), 2 * 16 * 16 * 4);
    }

    #[test]
    fn a_page_missing_tiles_shows_other_levels_under_the_ones_held() {
        let placement = PagePlacement {
            page: 0,
            size: (612.0, 792.0),
            level: 12,
            origin: (0.0, 0.0),
            screen_per_px: 1.0,
        };
        let view = (0.0, 0.0, 1000.0, 800.0);
        let wanted = wanted_tiles(&[placement], view);
        let mut cache = TileCache::new(usize::MAX);
        let first = wanted[0];
        let held = TileKey {
            page: 0,
            level: first.level,
            col: first.col,
            row: first.row,
        };
        cache.insert(held, image(TILE_PX));
        let older = TileKey {
            page: 0,
            level: 8,
            col: 0,
            row: 0,
        };
        cache.insert(older, image(TILE_PX));
        let mut page_tiles = HashMap::from([(1, PageTiles::new(placement))]);
        let plan = plan_tiles(&wanted, &mut cache, view, &mut page_tiles, 1);
        assert!(plan.missing);
        assert!(!plan.batch.contains(&held));
        assert_eq!(plan.batch.len(), wanted.len() - 1);
        let entry = &page_tiles[&1];
        assert_eq!(entry.tiles.len(), 1);
        assert_eq!(entry.under.len(), 1);
        assert!(
            (entry.under[0].0 - 2.0).abs() < 1e-9,
            "level 8 is drawn at twice its size"
        );
    }
}
