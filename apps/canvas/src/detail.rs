//! Painted layers and the ground at the detail a view or an export needs.
//!
//! A painted layer is simulated once over its whole grid at a modest scale
//! (see [`crate::render::cell_scale`]). Closer than that, the part being
//! shown is simulated again, finer, in tiles: each tile is a window of the
//! layer's grid at a finer level, simulated from the marks that reach it
//! (`tilo_wet_media::windowed`), so it paints what the whole grid would
//! paint there and neighbouring tiles meet without a seam. A tile is a
//! pure function of those marks, its level and its place, and is kept by
//! that key: adding a stroke re-simulates only the tiles it reaches.
//!
//! A live view asks for its tiles and draws what is ready, the coarse
//! picture standing in for the rest until worker threads finish them. An
//! export or a `look` waits for every tile it needs. Farther than the
//! coarse scale, the picture is drawn from halved copies of itself, so a
//! zoomed-out painting does not shimmer.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "tile arithmetic is on cell counts far below 2^23 and kurbo float geometry"
)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use ui::num::f32_of_f64;

use tilo_wet_media::{
    Canvas, Heightfield, LayerPainter, MediumEngine, Painted, Rgb, Settings, Texture, windowed,
};
use ui::vello::kurbo::{Affine, Rect};
use ui::vello::peniko::ImageData;

/// A tile's side, in cells of its level.
pub const TILE: usize = 256; // a simulation unit, not on-screen spacing
/// The finest a painted layer is simulated, in cells per artwork unit.
use crate::content_geometry::FINEST;
use crate::picture::image_of;
use ui::lock::MutexRecover as _;
/// Tiles kept between frames.
const KEEP: usize = 240;
/// How much coarser than a pixel the coarse picture may be before a finer
/// level is simulated: a little blur is cheaper than a level.
const SLACK: f64 = 1.3;

/// What part of the artwork is shown, and how closely.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detail {
    /// The part shown, in artwork units.
    pub visible: Rect,
    /// Device pixels per artwork unit.
    pub pixels_per_unit: f64,
    /// Simulate every tile now, rather than showing the coarse picture
    /// where a tile is not ready yet: an export or a `look`.
    pub wait: bool,
}

/// The finer level to simulate at for `pixels_per_unit` over a layer
/// simulated at `base` cells per unit: twice, four times ... `base`, the
/// level nearest a cell per pixel. `None` when `base` is fine enough.
pub fn level(base: f64, pixels_per_unit: f64) -> Option<f64> {
    if pixels_per_unit <= base * SLACK || base <= 0.0 {
        return None;
    }
    let mut scale = base;
    while scale * 2.0 <= FINEST && pixels_per_unit / scale > std::f64::consts::SQRT_2 {
        scale *= 2.0;
    }
    (scale > base).then_some(scale)
}

/// The halving of the coarse picture to draw at `pixels_per_unit`, where
/// it has `base` cells per unit: 0 for the picture itself, 1 for half ...
pub fn mip(base: f64, pixels_per_unit: f64) -> usize {
    let mut halvings = 0;
    let mut scale = base;
    while pixels_per_unit > 0.0 && scale / pixels_per_unit >= 2.0 && halvings < 8 {
        scale /= 2.0;
        halvings += 1;
    }
    halvings
}

/// `image` and its halvings, down to a pixel a side.
#[derive(Default)]
pub struct Mips {
    images: Vec<ImageData>,
}

impl Mips {
    /// `image` halved `halvings` times, building the halvings on demand;
    /// a picture already a pixel a side is not halved further.
    pub fn get(&mut self, image: &ImageData, halvings: usize) -> (ImageData, f64) {
        if self
            .images
            .first()
            .is_none_or(|first| first.data.id() != image.data.id())
        {
            self.images = vec![image.clone()];
        }
        while self.images.len() <= halvings {
            let Some(last) = self.images.last() else {
                break;
            };
            if last.width <= 1 && last.height <= 1 {
                break;
            }
            let half = halve(last);
            self.images.push(half);
        }
        let index = halvings.min(self.images.len().saturating_sub(1));
        let chosen = self
            .images
            .get(index)
            .cloned()
            .unwrap_or_else(|| image.clone());
        let factor = f64::from(image.width) / f64::from(chosen.width.max(1));
        (chosen, factor)
    }
}

/// `image` at half its size, each pixel the average of four, weighted by
/// their alpha so a transparent pixel does not darken its neighbours.
fn halve(image: &ImageData) -> ImageData {
    let (width, height) = (image.width as usize, image.height as usize);
    let (half_width, half_height) = (width.div_ceil(2).max(1), height.div_ceil(2).max(1));
    let source = image.data.data();
    let mut pixels = vec![0_u8; half_width * half_height * 4];
    for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        let (x, y) = (index % half_width * 2, index / half_width * 2);
        let mut sum = [0.0_f32; 4];
        let mut count = 0.0_f32;
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let (sx, sy) = ((x + dx).min(width - 1), (y + dy).min(height - 1));
            if let Some(&[r, g, b, a]) =
                source.get((sy * width + sx) * 4..(sy * width + sx) * 4 + 4)
            {
                let alpha = f32::from(a);
                sum[0] += f32::from(r) * alpha;
                sum[1] += f32::from(g) * alpha;
                sum[2] += f32::from(b) * alpha;
                sum[3] += alpha;
                count += 1.0;
            }
        }
        let alpha = sum[3];
        let colour = |value: f32| byte(if alpha > 0.0 { value / alpha } else { 0.0 });
        pixel.copy_from_slice(&[
            colour(sum[0]),
            colour(sum[1]),
            colour(sum[2]),
            byte(alpha / count.max(1.0)),
        ]);
    }
    image_of(pixels, half_width, half_height)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to a byte before the cast"
)]
fn byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// A painted layer's marks at a finer level, kept while its marks stay.
pub struct LayerLevel {
    pub key: blake3::Hash,
    pub scale: f64,
    pub marks: Arc<[Painted]>,
    /// The cells each mark can reach at this level.
    reach: Vec<Option<[f32; 4]>>,
    /// The room a tile is simulated with around it, so paint dragged or
    /// flowing in from beside it arrives as it does on the whole grid.
    pub margin: usize,
}

impl LayerLevel {
    pub fn new(key: blake3::Hash, scale: f64, marks: Vec<Painted>) -> Self {
        let reach: Vec<Option<[f32; 4]>> = marks.iter().map(|mark| mark.mark.reach()).collect();
        let widest = marks
            .iter()
            .map(|mark| match &mark.mark {
                tilo_wet_media::Mark::Stroke(stroke) => stroke.size,
                tilo_wet_media::Mark::Fill(fill) => fill.brush,
                tilo_wet_media::Mark::Erase(erase) => erase.size,
            })
            .fold(0.0_f32, f32::max);
        Self {
            key,
            scale,
            marks: marks.into(),
            reach,
            margin: cells(f64::from(widest) * 0.6 + 32.0).min(192),
        }
    }
}

/// How a tile is simulated.
#[derive(Clone)]
pub enum Work {
    /// A painted layer's tile.
    Paint {
        engine: Arc<dyn MediumEngine>,
        settings: Settings,
        texture: Texture,
        marks: Arc<[Painted]>,
        margin: usize,
    },
    /// The lit ground's tile.
    Ground { texture: Texture, colour: Rgb },
}

/// One tile to simulate.
#[derive(Clone)]
pub struct Job {
    key: blake3::Hash,
    work: Work,
    scale: f64,
    /// The tile, in cells of its level.
    tile: [usize; 4],
    /// The level's whole grid, in cells.
    grid: (usize, usize),
}

impl Job {
    /// The tile's pixels: the window around it simulated, then cut to it.
    fn run(&self) -> ImageData {
        let [x0, y0, x1, y1] = self.tile;
        let (width, height) = (x1 - x0, y1 - y0);
        let scale = f32_of_f64(self.scale);
        match &self.work {
            Work::Ground { texture, colour } => {
                // One cell more above and to the left: the light falls
                // from there, so the tile's first row is lit as on the
                // whole ground.
                let (left, top) = (x0.min(1), y0.min(1));
                let field = Heightfield::window(
                    (x0 - left, y0 - top),
                    width + left,
                    height + top,
                    *texture,
                    scale,
                    1,
                );
                let lit = field.shade(*colour);
                image_of(
                    crop(&lit, width + left, (left, top), (width, height)),
                    width,
                    height,
                )
            }
            Work::Paint {
                engine,
                settings,
                texture,
                marks,
                margin,
            } => {
                let origin = (x0.saturating_sub(*margin), y0.saturating_sub(*margin));
                let end = (
                    (x1 + margin).min(self.grid.0),
                    (y1 + margin).min(self.grid.1),
                );
                let (span, rows) = (end.0 - origin.0, end.1 - origin.1);
                let ground = Arc::new(Heightfield::window(origin, span, rows, *texture, scale, 1));
                let canvas = Canvas::window(scale, ground, origin);
                let mut painter = LayerPainter::new(Arc::clone(engine), canvas, settings.clone());
                let pixels = painter.paint(&windowed(marks, origin, span, rows));
                image_of(
                    crop(
                        pixels,
                        span,
                        (x0 - origin.0, y0 - origin.1),
                        (width, height),
                    ),
                    width,
                    height,
                )
            }
        }
    }
}

fn crop(
    pixels: &[u8],
    span: usize,
    (left, top): (usize, usize),
    (width, height): (usize, usize),
) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 4);
    for row in top..top + height {
        let start = (row * span + left) * 4;
        out.extend_from_slice(pixels.get(start..start + width * 4).unwrap_or(&[]));
    }
    out.resize(width * height * 4, 0);
    out
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a non-negative cell count, clamped before the cast"
)]
fn cells(value: f64) -> usize {
    value.round().clamp(0.0, 1e7) as usize
}

/// How many tiles are simulated at once in the background.
const WORKERS: usize = 4;

/// The tiles being simulated in the background, and those finished.
#[derive(Default)]
struct Shared {
    running: HashSet<blake3::Hash>,
    done: Vec<(blake3::Hash, ImageData)>,
}

/// Tiles simulated so far, and the work simulating more.
#[derive(Default)]
pub struct Tiles {
    ready: HashMap<blake3::Hash, (ImageData, u64)>,
    frame: u64,
    shared: Arc<Mutex<Shared>>,
    /// Jobs wanted this frame, nearest the middle of the view first.
    wanted: Vec<Job>,
    /// Whether some tile shown is still being simulated.
    pending: bool,
    mosaics: HashMap<String, (blake3::Hash, ImageData)>,
}

/// A picture of part of a layer at a level, built from its tiles.
pub struct Mosaic {
    pub image: ImageData,
    /// Where it lies, in artwork units: its pixels map onto this.
    pub to_units: Affine,
    pub rect: Rect,
    /// The tiles not simulated yet, in artwork units: the coarse picture
    /// shows there meanwhile.
    pub missing: Vec<Rect>,
}

/// One layer at one level, as a request for tiles.
pub struct Request<'a> {
    /// What the mosaic is kept under between frames.
    pub slot: &'a str,
    pub work: Work,
    /// The layer at the level: its key, scale and marks.
    pub level: &'a LayerLevel,
    /// The level's whole grid, in cells.
    pub grid: (usize, usize),
}

impl Tiles {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock_recover()
    }

    /// Start a frame: take in the tiles finished in the background.
    pub fn begin(&mut self) {
        self.frame += 1;
        self.pending = false;
        self.wanted.clear();
        let done = std::mem::take(&mut self.lock().done);
        for (key, image) in done {
            self.ready.insert(key, (image, self.frame));
        }
    }

    /// Start simulating this frame's wanted tiles in the background, as
    /// many at once as there are workers, and forget the oldest tiles
    /// beyond what is kept.
    pub fn end(&mut self) {
        let wanted = std::mem::take(&mut self.wanted);
        let mut shared = self.shared.lock_recover();
        let mut starting = Vec::new();
        for job in wanted {
            if starting.len() + shared.running.len() >= WORKERS {
                break;
            }
            if shared.running.insert(job.key) {
                starting.push(job);
            }
        }
        let busy = !shared.running.is_empty();
        drop(shared);
        self.pending |= busy;
        for job in starting {
            let shared = Arc::clone(&self.shared);
            // Detached: a tile finished after the view moved on is kept
            // for when it comes back.
            drop(ui::rt::app().spawn_blocking(move || {
                let image = job.run();
                let mut shared = shared.lock_recover();
                shared.running.remove(&job.key);
                shared.done.push((job.key, image));
            }));
        }
        if self.ready.len() > KEEP {
            let mut ages: Vec<(u64, blake3::Hash)> = self
                .ready
                .iter()
                .map(|(key, (_, used))| (*used, *key))
                .collect();
            ages.sort_unstable_by_key(|(used, _)| *used);
            for (_, key) in ages.into_iter().take(self.ready.len() - KEEP) {
                self.ready.remove(&key);
            }
        }
    }

    /// Whether a tile being shown is still being simulated: the view should
    /// draw again soon.
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// The part of `request`'s layer that `detail` shows, at its level: the
    /// tiles ready, the coarse picture where one is not, every tile when
    /// `detail.wait`. `None` when nothing of it is shown.
    pub fn mosaic(&mut self, request: &Request<'_>, detail: &Detail) -> Option<Mosaic> {
        let level = request.level;
        let scale = level.scale;
        let (grid_width, grid_height) = request.grid;
        let span = |low: f64, high: f64, limit: usize| {
            let first = cells((low * scale / TILE as f64).floor().max(0.0));
            let last = cells((high * scale / TILE as f64).ceil()).min(limit.div_ceil(TILE));
            (first, last)
        };
        let (tx0, tx1) = span(detail.visible.x0, detail.visible.x1, grid_width);
        let (ty0, ty1) = span(detail.visible.y0, detail.visible.y1, grid_height);
        if tx0 >= tx1 || ty0 >= ty1 {
            return None;
        }
        let middle = ((tx0 + tx1) as f64 * 0.5, (ty0 + ty1) as f64 * 0.5);
        let mut tiles = Vec::new();
        for ty in ty0..ty1 {
            for tx in tx0..tx1 {
                let tile = [
                    tx * TILE,
                    ty * TILE,
                    ((tx + 1) * TILE).min(grid_width),
                    ((ty + 1) * TILE).min(grid_height),
                ];
                let job = Job {
                    key: Self::key(request, tile),
                    work: request.work.clone(),
                    scale,
                    tile,
                    grid: request.grid,
                };
                let distance = (tx as f64 + 0.5 - middle.0).hypot(ty as f64 + 0.5 - middle.1);
                tiles.push((distance, job));
            }
        }
        if detail.wait {
            self.run_now(tiles.iter().map(|(_, job)| job));
        }
        let frame = self.frame;
        let mut identity = blake3::Hasher::new();
        identity.update(request.level.key.as_bytes());
        let mut missing = Vec::new();
        for (distance, job) in &tiles {
            if let Some((_, used)) = self.ready.get_mut(&job.key) {
                *used = frame;
                identity.update(job.key.as_bytes());
            } else {
                identity.update(b"missing");
                missing.push((*distance, job.clone()));
            }
        }
        let holes: Vec<Rect> = missing
            .iter()
            .map(|(_, job)| {
                let [a, b, c, d] = job.tile.map(|value| value as f64 / scale);
                Rect::new(a, b, c, d)
            })
            .collect();
        if !missing.is_empty() {
            self.pending = true;
            missing.sort_by(|a, b| a.0.total_cmp(&b.0));
            self.wanted.extend(missing.into_iter().map(|(_, job)| job));
        }
        let (x0, y0) = (tx0 * TILE, ty0 * TILE);
        let (x1, y1) = ((tx1 * TILE).min(grid_width), (ty1 * TILE).min(grid_height));
        let (width, height) = (x1 - x0, y1 - y0);
        let identity = identity.finalize();
        let image = match self.mosaics.get(request.slot) {
            Some((have, image)) if *have == identity => image.clone(),
            _ => {
                let image = self.assemble(&tiles, (x0, y0), (width, height));
                self.mosaics
                    .insert(request.slot.to_owned(), (identity, image.clone()));
                image
            }
        };
        let to_units = Affine::scale(1.0 / scale) * Affine::translate((x0 as f64, y0 as f64));
        Some(Mosaic {
            image,
            to_units,
            rect: Rect::new(
                x0 as f64 / scale,
                y0 as f64 / scale,
                x1 as f64 / scale,
                y1 as f64 / scale,
            ),
            missing: holes,
        })
    }

    /// Simulate `jobs` not yet ready now, side by side where the platform
    /// can.
    fn run_now<'a>(&mut self, jobs: impl Iterator<Item = &'a Job>) {
        use rayon::prelude::*;
        let todo: Vec<&Job> = jobs
            .filter(|job| !self.ready.contains_key(&job.key))
            .collect();
        // On the CPU pool, as every app's parallel work: the pool sizes it
        // to the device, and in a browser page its one thread.
        let done: Vec<(blake3::Hash, ImageData)> = ui::rt::cpu_pool()
            .install(|| todo.par_iter().map(|job| (job.key, job.run())).collect());
        for (key, image) in done {
            self.ready.insert(key, (image, self.frame));
        }
    }

    /// The tile's key: its level, its place, and what reaches it.
    fn key(request: &Request<'_>, tile: [usize; 4]) -> blake3::Hash {
        let mut key = blake3::Hasher::new();
        key.update(request.level.key.as_bytes());
        for value in tile {
            key.update(&(value as u64).to_le_bytes());
        }
        if let Work::Paint { margin, .. } = &request.work {
            // Only the marks that reach the window change the tile; a mark
            // painted dry elsewhere dries what is wet, wherever it is.
            let level = request.level;
            let [x0, y0, x1, y1] = tile.map(|value| value as f32);
            let room = *margin as f32;
            let mut dried = false;
            for (mark, reach) in level.marks.iter().zip(&level.reach) {
                let reaches = reach.is_some_and(|[a, b, c, d]| {
                    c >= x0 - room && d >= y0 - room && a <= x1 + room && b <= y1 + room
                });
                if reaches {
                    key.update(&mark.fingerprint);
                    dried = false;
                } else if !mark.mark.wet()
                    && !matches!(mark.mark, tilo_wet_media::Mark::Erase(_))
                    && !dried
                {
                    key.update(b"dry");
                    dried = true;
                }
            }
        }
        key.finalize()
    }

    /// The ready tiles side by side; those not ready are left clear.
    fn assemble(
        &self,
        tiles: &[(f64, Job)],
        (x0, y0): (usize, usize),
        (width, height): (usize, usize),
    ) -> ImageData {
        let mut pixels = vec![0_u8; width * height * 4];
        for (_, job) in tiles {
            let [tx0, ty0, tx1, ty1] = job.tile;
            let tile_width = tx1 - tx0;
            let Some((tile, _)) = self.ready.get(&job.key) else {
                continue;
            };
            let tile = tile.data.data();
            for row in ty0..ty1 {
                let start = ((row - y0) * width + (tx0 - x0)) * 4;
                let from = (row - ty0) * tile_width * 4;
                if let (Some(out), Some(source)) = (
                    pixels.get_mut(start..start + tile_width * 4),
                    tile.get(from..from + tile_width * 4),
                ) {
                    out.copy_from_slice(source);
                }
            }
        }
        image_of(pixels, width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closer_view_simulates_a_finer_level_and_a_far_one_draws_a_halving() {
        assert_eq!(level(1.0, 1.0), None);
        assert_eq!(level(1.0, 1.2), None);
        assert_eq!(level(1.0, 2.0), Some(2.0));
        assert_eq!(level(1.0, 3.5), Some(4.0));
        assert_eq!(level(1.0, 100.0), Some(8.0));
        assert_eq!(mip(1.0, 1.0), 0);
        assert_eq!(mip(1.0, 0.4), 1);
        assert_eq!(mip(1.0, 0.2), 2);
    }

    #[test]
    fn a_halving_keeps_colour_where_paint_meets_bare_ground() {
        // Red paint beside transparency halves to red at half alpha, not to
        // a darkened red.
        let image = image_of(vec![255, 0, 0, 255, 0, 0, 0, 0], 2, 1);
        let half = halve(&image);
        assert_eq!(half.data.data(), &[255, 0, 0, 128]);
    }
}
