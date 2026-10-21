//! One retained premultiplied RGBA canvas that the CPU brings up to date
//! with a scene.
//!
//! The canvas keeps its pixels between frames, and which of its 16 pixel
//! tiles hold the scene last decoded is tracked per tile. A new scene is
//! decoded and compared with the one before
//! ([`tilo_cpu_raster::CpuRasterizer::changes`]): pixels a pan or scroll only
//! moved are moved within the canvas, the tiles under every draw that
//! differs are invalidated, and only invalid tiles are rasterized, within
//! the area that is shown. The screen renderer keeps one
//! ([`super::cpu_present::CpuRenderer`]); a layer compositor keeps one per
//! layer.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "pixel and tile arithmetic on a canvas whose sides fit u16 (vello_cpu's limit), checked when it is allocated"
)]

use std::time::Instant;

use tilo_cpu_raster::{CpuRasterizer, Options, Region, SceneChanges, Target};
use vello::FineDamageRect;
use vello::Scene;
use vello::peniko::Color;

use super::cpu_present::CpuFrameStats;

/// Canvas validity is tracked in tiles of this edge, the same 16 pixels the
/// shell snaps its damage to.
pub const TILE: u32 = 16;

/// What a frame says about where its scene changed.
#[derive(Clone, Copy, Debug)]
pub enum Damage<'a> {
    /// The scene is the one decoded last: nothing changed.
    None,
    /// The changes lie within these rectangles.
    Within(&'a [FineDamageRect]),
    /// Unknown: the decoded draws decide.
    Unknown,
}

/// A retained canvas and the rasterizer that keeps it current.
pub struct RetainedCanvas {
    raster: CpuRasterizer,
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    /// Bumped whenever the pixels are reallocated, so nothing that copied
    /// them claims pixels of an earlier canvas.
    generation: u64,
    tiles_x: u32,
    tiles_y: u32,
    valid: Vec<bool>,
    want: Vec<bool>,
    /// Per tile, whether any of its pixels is not transparent, as last drawn
    /// or moved there: a compositor skips the empty ones.
    inked: Vec<bool>,
    /// The rasterizer holds a decoded scene.
    prepared: bool,
}

impl std::fmt::Debug for RetainedCanvas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedCanvas")
            .field("size", &(self.width, self.height))
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl RetainedCanvas {
    pub fn new(threads: u16) -> Self {
        Self {
            raster: CpuRasterizer::new(Options {
                threads,
                ..Options::default()
            }),
            pixels: Vec::new(),
            width: 0,
            height: 0,
            generation: 0,
            tiles_x: 0,
            tiles_y: 0,
            valid: Vec::new(),
            want: Vec::new(),
            inked: Vec::new(),
            prepared: false,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Premultiplied RGBA, `width * 4` bytes per row.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }

    /// Reallocate the canvas at `width` x `height`, every tile invalid.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0; width as usize * height as usize * 4];
        self.tiles_x = width.div_ceil(TILE);
        self.tiles_y = height.div_ceil(TILE);
        let tiles = self.tiles_x as usize * self.tiles_y as usize;
        self.valid = vec![false; tiles];
        self.want = vec![false; tiles];
        self.inked = vec![true; tiles];
        self.generation = self.generation.wrapping_add(1);
    }

    /// Bring the canvas's tile validity up to date with `scene`: decode it
    /// unless `damage` says it is the one decoded last, move what a pan only
    /// moved and invalidate what changed. Returns the regions moved, which
    /// are valid now. Nothing is rasterized ([`Self::rasterize`]).
    pub fn apply(
        &mut self,
        scene: &Scene,
        damage: Damage<'_>,
        stats: &mut CpuFrameStats,
    ) -> Vec<Region> {
        if !matches!(damage, Damage::None) || !self.prepared {
            let started = Instant::now();
            self.raster.prepare(scene.encoding());
            stats.decode += started.elapsed();
            self.prepared = true;
        }
        let area = |w: u32, h: u32| u64::from(w) * u64::from(h);
        let diff_area = |changes: &SceneChanges| -> u64 {
            changes
                .regions
                .iter()
                .map(|r| area(r.width, r.height))
                .sum()
        };
        match damage {
            Damage::None => Vec::new(),
            // The rectangles bound a section; the changed draws inside it are
            // usually far smaller (a clock's digits in the top bar).
            Damage::Within(rects) => {
                let shell_area: u64 = rects.iter().map(|r| area(r.width, r.height)).sum();
                if let Some(changes) = self
                    .raster
                    .changes(self.width, self.height)
                    .filter(|changes| diff_area(changes) < shell_area)
                {
                    stats.fill = "damage-diff";
                    self.apply_changes(changes, stats)
                } else {
                    for rect in rects {
                        self.invalidate(*rect);
                    }
                    Vec::new()
                }
            }
            // The valid tiles hold the scene decoded before this one, and only
            // pixels under a draw that differs between the two can change.
            Damage::Unknown => {
                if let Some(changes) = self.raster.changes(self.width, self.height) {
                    stats.fill = "diff";
                    self.apply_changes(changes, stats)
                } else {
                    self.valid.fill(false);
                    Vec::new()
                }
            }
        }
    }

    /// Rasterize the invalid tiles inside `area` (the whole canvas with
    /// `None`) from the scene [`Self::apply`] decoded. Returns the regions
    /// drawn, which are valid now.
    pub fn rasterize(
        &mut self,
        area: Option<FineDamageRect>,
        stats: &mut CpuFrameStats,
    ) -> Vec<Region> {
        if !self.prepared {
            return Vec::new();
        }
        let regions = self.invalid_regions(area);
        if regions.is_empty() {
            return regions;
        }
        let started = Instant::now();
        let (width, height) = (self.width, self.height);
        let Ok(mut target) = Target::new(&mut self.pixels, width, height, width as usize * 4)
        else {
            return Vec::new();
        };
        // The scene owns its ground: invalid regions are cleared to
        // transparent before the scene is replayed over them.
        self.raster
            .render_prepared(&mut target, Some(&regions), Color::TRANSPARENT);
        stats.raster += started.elapsed();
        stats.raster_regions += regions.len();
        for region in &regions {
            stats.raster_px = stats
                .raster_px
                .saturating_add(u64::from(region.width) * u64::from(region.height));
            self.mark_valid(*region);
            self.note_ink(*region);
        }
        regions
    }

    /// Invalidate what changed, after moving the pixels a pan or scroll
    /// only moved. Returns the regions that were moved and are now valid.
    fn apply_changes(&mut self, changes: SceneChanges, stats: &mut CpuFrameStats) -> Vec<Region> {
        let mut moved = Vec::new();
        stats.motion = match (&changes.motion, &changes.not_a_motion) {
            (Some(motion), _) => format!(
                "{},{} opaque={} obstacles={}",
                motion.dx,
                motion.dy,
                motion.opaque.len(),
                motion.obstacles.len()
            ),
            (None, Some(reason)) => reason.clone(),
            (None, None) => String::new(),
        };
        if let Some(motion) = &changes.motion {
            let (dx, dy) = (i64::from(motion.dx), i64::from(motion.dy));
            for run in motion.movable(self.width, self.height, TILE) {
                // Tile by tile: the source pixels must all be valid.
                let mut x = run.x;
                while x < run.x + run.width {
                    let width = TILE.min(run.x + run.width - x);
                    let source = FineDamageRect {
                        x: u32::try_from(i64::from(x) - dx).unwrap_or(u32::MAX),
                        y: u32::try_from(i64::from(run.y) - dy).unwrap_or(u32::MAX),
                        width,
                        height: run.height,
                    };
                    if self.all_valid(source) {
                        match moved.last_mut() {
                            Some(Region {
                                x: px,
                                y,
                                width: pw,
                                height,
                            }) if *y == run.y && *height == run.height && *px + *pw == x => {
                                *pw += width;
                            }
                            _ => moved.push(Region::new(x, run.y, width, run.height)),
                        }
                    }
                    x += width;
                }
            }
            if !moved.is_empty() {
                let (width, height) = (self.width, self.height);
                if let Ok(mut target) =
                    Target::new(&mut self.pixels, width, height, width as usize * 4)
                {
                    let started = Instant::now();
                    tilo_cpu_raster::move_pixels(&mut target, &moved, motion.dx, motion.dy);
                    stats.move_time += started.elapsed();
                } else {
                    moved.clear();
                }
            }
        }
        for region in changes.regions {
            self.invalidate(region_rect(region));
        }
        for region in &moved {
            self.mark_valid(*region);
            self.note_ink(*region);
            stats.moved_px = stats
                .moved_px
                .saturating_add(u64::from(region.width) * u64::from(region.height));
        }
        moved
    }

    /// Every tile under `rect` holds the current canvas.
    fn all_valid(&self, rect: FineDamageRect) -> bool {
        if rect.x.saturating_add(rect.width) > self.width
            || rect.y.saturating_add(rect.height) > self.height
        {
            return false;
        }
        let (x0, y0, x1, y1) = self.tile_span(rect);
        (y0..y1).all(|ty| {
            let row = (ty * self.tiles_x) as usize;
            self.valid
                .get(row + x0 as usize..row + x1 as usize)
                .is_some_and(|tiles| tiles.iter().all(|valid| *valid))
        })
    }

    fn tile_span(&self, rect: FineDamageRect) -> (u32, u32, u32, u32) {
        let x0 = (rect.x / TILE).min(self.tiles_x);
        let y0 = (rect.y / TILE).min(self.tiles_y);
        let x1 = rect
            .x
            .saturating_add(rect.width)
            .div_ceil(TILE)
            .min(self.tiles_x);
        let y1 = rect
            .y
            .saturating_add(rect.height)
            .div_ceil(TILE)
            .min(self.tiles_y);
        (x0, y0, x1, y1)
    }

    pub fn invalidate(&mut self, rect: FineDamageRect) {
        let (x0, y0, x1, y1) = self.tile_span(rect);
        for ty in y0..y1 {
            let row = (ty * self.tiles_x) as usize;
            if let Some(tiles) = self.valid.get_mut(row + x0 as usize..row + x1 as usize) {
                tiles.fill(false);
            }
        }
    }

    /// Look again whether the tiles under `region` hold any ink.
    fn note_ink(&mut self, region: Region) {
        let (x0, y0, x1, y1) = self.tile_span(region_rect(region));
        let stride = self.width as usize * 4;
        for ty in y0..y1 {
            for tx in x0..x1 {
                let rows = ty * TILE..((ty + 1) * TILE).min(self.height);
                let columns =
                    (tx * TILE) as usize * 4..((tx + 1) * TILE).min(self.width) as usize * 4;
                let inked = rows.into_iter().any(|row| {
                    let start = row as usize * stride;
                    self.pixels
                        .get(start + columns.start..start + columns.end)
                        .is_some_and(|pixels| {
                            pixels
                                .chunks_exact(4)
                                .any(|pixel| pixel.get(3).is_some_and(|alpha| *alpha != 0))
                        })
                });
                if let Some(tile) = self.inked.get_mut((ty * self.tiles_x + tx) as usize) {
                    *tile = inked;
                }
            }
        }
    }

    /// The parts of `rect` (canvas pixels) whose tiles hold ink, as
    /// rectangles in whole tiles clipped to `rect`: what a compositor
    /// draws of it.
    pub fn inked_parts(&self, rect: FineDamageRect) -> Vec<FineDamageRect> {
        let (x0, y0, x1, y1) = self.tile_span(rect);
        let right = rect.x.saturating_add(rect.width);
        let bottom = rect.y.saturating_add(rect.height);
        let mut parts: Vec<FineDamageRect> = Vec::new();
        for ty in y0..y1 {
            let top = (ty * TILE).max(rect.y);
            let height = ((ty + 1) * TILE).min(bottom).saturating_sub(top);
            let mut tx = x0;
            while tx < x1 {
                let inked = |tx: u32| {
                    self.inked
                        .get((ty * self.tiles_x + tx) as usize)
                        .copied()
                        .unwrap_or(true)
                };
                if !inked(tx) {
                    tx += 1;
                    continue;
                }
                let start = tx;
                while tx < x1 && inked(tx) {
                    tx += 1;
                }
                let left = (start * TILE).max(rect.x);
                let width = (tx * TILE).min(right).saturating_sub(left);
                // A run the same as the one above it extends that one.
                match parts.iter_mut().rev().find(|part| {
                    part.x == left && part.width == width && part.y + part.height == top
                }) {
                    Some(part) => part.height += height,
                    None => parts.push(FineDamageRect {
                        x: left,
                        y: top,
                        width,
                        height,
                    }),
                }
            }
        }
        parts
    }

    fn mark_valid(&mut self, region: Region) {
        let (x0, y0, x1, y1) = self.tile_span(region_rect(region));
        for ty in y0..y1 {
            let row = (ty * self.tiles_x) as usize;
            if let Some(tiles) = self.valid.get_mut(row + x0 as usize..row + x1 as usize) {
                tiles.fill(true);
            }
        }
    }

    /// Invalid tiles inside `area`, merged into rectangles: runs along a
    /// tile row, joined with identical runs in the rows below.
    fn invalid_regions(&mut self, area: Option<FineDamageRect>) -> Vec<Region> {
        let (ax0, ay0, ax1, ay1) = match area {
            Some(area) => self.tile_span(area),
            None => (0, 0, self.tiles_x, self.tiles_y),
        };
        for (index, (want, valid)) in self.want.iter_mut().zip(&self.valid).enumerate() {
            let tx = index as u32 % self.tiles_x.max(1);
            let ty = index as u32 / self.tiles_x.max(1);
            *want = !*valid && (ax0..ax1).contains(&tx) && (ay0..ay1).contains(&ty);
        }
        // (x0, x1, y0, y1) in tiles.
        let mut open: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut closed: Vec<(u32, u32, u32, u32)> = Vec::new();
        for ty in 0..self.tiles_y {
            let row = (ty * self.tiles_x) as usize;
            let mut runs = Vec::new();
            let mut tx = 0;
            while tx < self.tiles_x {
                if self.want.get(row + tx as usize).copied().unwrap_or(false) {
                    let start = tx;
                    while tx < self.tiles_x
                        && self.want.get(row + tx as usize).copied().unwrap_or(false)
                    {
                        tx += 1;
                    }
                    runs.push((start, tx));
                } else {
                    tx += 1;
                }
            }
            let mut next_open = Vec::with_capacity(runs.len());
            for (start, end) in runs {
                if let Some(position) = open
                    .iter()
                    .position(|&(x0, x1, _, _)| x0 == start && x1 == end)
                {
                    let (x0, x1, y0, _) = open.swap_remove(position);
                    next_open.push((x0, x1, y0, ty + 1));
                } else {
                    next_open.push((start, end, ty, ty + 1));
                }
            }
            closed.append(&mut open);
            open = next_open;
        }
        closed.append(&mut open);
        closed
            .into_iter()
            .map(|(x0, x1, y0, y1)| {
                let x = x0 * TILE;
                let y = y0 * TILE;
                Region::new(
                    x,
                    y,
                    (x1 * TILE).min(self.width) - x,
                    (y1 * TILE).min(self.height) - y,
                )
            })
            .collect()
    }
}

pub fn region_rect(region: Region) -> FineDamageRect {
    FineDamageRect {
        x: region.x,
        y: region.y,
        width: region.width,
        height: region.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::Fill;

    #[test]
    fn only_tiles_holding_ink_are_parts_to_draw() {
        let mut canvas = RetainedCanvas::new(0);
        canvas.resize(64, 64);
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(200, 10, 10),
            None,
            &Rect::new(20.0, 20.0, 40.0, 36.0),
        );
        let mut stats = CpuFrameStats::default();
        canvas.apply(&scene, Damage::Unknown, &mut stats);
        canvas.rasterize(None, &mut stats);
        let all = FineDamageRect {
            x: 0,
            y: 0,
            width: 64,
            height: 64,
        };
        // The fill covers tiles 1..3 across and the second tile row (and
        // the first pixels of the third one).
        assert_eq!(
            canvas.inked_parts(all),
            vec![FineDamageRect {
                x: 16,
                y: 16,
                width: 32,
                height: 32
            }]
        );
        // Clipped to what is asked for.
        let corner = FineDamageRect {
            x: 24,
            y: 0,
            width: 40,
            height: 24,
        };
        assert_eq!(
            canvas.inked_parts(corner),
            vec![FineDamageRect {
                x: 24,
                y: 16,
                width: 24,
                height: 8
            }]
        );
    }
}
