//! CPU rasterization of recorded vello scenes.
//!
//! A [`vello::Scene`] is a [`vello_encoding::Encoding`]: tag and data streams
//! that vello's compute pipeline consumes on the GPU. Some GPUs cannot run that
//! pipeline at all, so [`CpuRasterizer`] walks the same streams on the CPU and
//! replays every draw into [`vello_cpu`]: paths (fills and strokes), solid,
//! gradient and image brushes, blurred rounded rectangles, clip and blend
//! layers, and glyph runs, whose outlines it resolves and caches itself.
//!
//! The output is premultiplied RGBA8 written into a caller-owned buffer with
//! an arbitrary row stride, either whole or limited to damaged regions. Draws
//! whose bounds miss a region are not recorded at all, so a small damage
//! region costs little regardless of how much the scene contains.
//!
//! [`vello::Scene`]: https://docs.rs/vello/latest/vello/struct.Scene.html
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::indexing_slicing,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::too_many_lines,
    reason = "pixel and geometry arithmetic over bounds this crate checks itself; exact float comparisons detect identical transforms and axis-aligned rectangles; the stream walkers follow the format's layout in one place"
)]

mod blur;
pub mod capture;
pub mod compare;
mod decode;
mod diff;
mod glyph;
mod image;
mod record;

use std::time::{Duration, Instant};

use vello_cpu::color::{AlphaColor, Srgb};
use vello_cpu::kurbo::Rect;
use vello_cpu::{Level, PixmapMut, RasterizerSettings, RenderContext, RenderMode, RenderSettings};
use vello_encoding::Encoding;

pub use vello_cpu::color;

/// Integer device-pixel rectangle: a damaged region of the target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn clamp_to(self, width: u32, height: u32) -> Option<Self> {
        let x0 = self.x.min(width);
        let y0 = self.y.min(height);
        let x1 = self.x.saturating_add(self.width).min(width);
        let y1 = self.y.saturating_add(self.height).min(height);
        (x1 > x0 && y1 > y0).then(|| Self::new(x0, y0, x1 - x0, y1 - y0))
    }

    fn to_rect(self) -> Rect {
        Rect::new(
            f64::from(self.x),
            f64::from(self.y),
            f64::from(self.x + self.width),
            f64::from(self.y + self.height),
        )
    }
}

/// A caller-owned premultiplied RGBA8 buffer, `stride` bytes per row.
#[derive(Debug)]
pub struct Target<'a> {
    data: &'a mut [u8],
    width: u32,
    height: u32,
    stride: usize,
}

/// Why a buffer cannot be rendered into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetError {
    /// The stride is shorter than a row, or not a whole number of pixels.
    Stride,
    /// The buffer is shorter than `stride * height`.
    Length,
    /// A side exceeds the 65535 pixels `vello_cpu` addresses.
    Size,
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stride => "row stride is shorter than a row or not a multiple of 4 bytes",
            Self::Length => "buffer is shorter than stride * height",
            Self::Size => "a side exceeds 65535 pixels",
        })
    }
}

impl std::error::Error for TargetError {}

impl<'a> Target<'a> {
    /// Wrap `data` as a `width` x `height` target with `stride` bytes per row.
    ///
    /// # Errors
    ///
    /// Returns an error when the stride or length cannot hold the image, or a
    /// side exceeds 65535 pixels.
    pub fn new(
        data: &'a mut [u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<Self, TargetError> {
        if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
            return Err(TargetError::Size);
        }
        if !stride.is_multiple_of(4)
            || stride < width as usize * 4
            || stride / 4 > usize::from(u16::MAX)
        {
            return Err(TargetError::Stride);
        }
        if data.len() < stride * height as usize {
            return Err(TargetError::Length);
        }
        Ok(Self {
            data,
            width,
            height,
            stride,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    fn clear(&mut self, region: Region) {
        let x0 = region.x as usize * 4;
        let x1 = x0 + region.width as usize * 4;
        for row in region.y..region.y + region.height {
            let start = row as usize * self.stride;
            self.data[start + x0..start + x1].fill(0);
        }
    }
}

/// What differs between two prepared scenes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneChanges {
    /// Every pixel that can differ.
    pub regions: Vec<Region>,
    /// The part of the change that moved, when every differing draw is its
    /// old self translated by one whole-pixel offset.
    pub motion: Option<PixelMotion>,
    /// Why the change is not a motion, when it is not, for diagnostics.
    pub not_a_motion: Option<String>,
}

/// Pixels the new scene shows where the old one showed them `(dx, dy)`
/// earlier: a pixel `p` inside `opaque` and, at both `p` and `p - (dx, dy)`,
/// outside every obstacle equals the old pixel at `p - (dx, dy)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PixelMotion {
    pub dx: i32,
    pub dy: i32,
    /// Where the moved draws paint opaquely, rounded in to whole pixels.
    pub opaque: Vec<Region>,
    /// Draws that stayed put over the moved ones, rounded out.
    pub obstacles: Vec<Region>,
}

impl PixelMotion {
    /// Tiles of a `width` x `height` target, `tile` pixels square, whose
    /// pixels can be moved from `(dx, dy)` earlier instead of drawn, as runs
    /// along tile rows. Partial tiles at the target's edge count whole.
    pub fn movable(&self, width: u32, height: u32, tile: u32) -> Vec<Region> {
        let tile = tile.max(1);
        let (tiles_x, tiles_y) = (width.div_ceil(tile), height.div_ceil(tile));
        let mut ok = vec![false; tiles_x as usize * tiles_y as usize];
        let tile_rect = |tx: u32, ty: u32| {
            let x = tx * tile;
            let y = ty * tile;
            (x, y, (x + tile).min(width), (y + tile).min(height))
        };
        for region in &self.opaque {
            let (rx1, ry1) = (region.x + region.width, region.y + region.height);
            for ty in region.y / tile..ry1.div_ceil(tile).min(tiles_y) {
                for tx in region.x / tile..rx1.div_ceil(tile).min(tiles_x) {
                    let (x0, y0, x1, y1) = tile_rect(tx, ty);
                    if x0 >= region.x && y0 >= region.y && x1 <= rx1 && y1 <= ry1 {
                        ok[(ty * tiles_x + tx) as usize] = true;
                    }
                }
            }
        }
        let (dx, dy) = (i64::from(self.dx), i64::from(self.dy));
        for region in &self.obstacles {
            // The obstacle itself, and every pixel whose source it covers.
            for (ox, oy) in [(0, 0), (dx, dy)] {
                let x0 = (i64::from(region.x) + ox).clamp(0, i64::from(width));
                let y0 = (i64::from(region.y) + oy).clamp(0, i64::from(height));
                let x1 = (i64::from(region.x + region.width) + ox).clamp(0, i64::from(width));
                let y1 = (i64::from(region.y + region.height) + oy).clamp(0, i64::from(height));
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let tile = i64::from(tile);
                for ty in y0 / tile..(y1 + tile - 1) / tile {
                    for tx in x0 / tile..(x1 + tile - 1) / tile {
                        if let Some(cell) = ok.get_mut((ty * i64::from(tiles_x) + tx) as usize) {
                            *cell = false;
                        }
                    }
                }
            }
        }
        // The source of every moved pixel must lie inside the target.
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let (x0, y0, x1, y1) = tile_rect(tx, ty);
                let inside = i64::from(x0) - dx >= 0
                    && i64::from(y0) - dy >= 0
                    && i64::from(x1) - dx <= i64::from(width)
                    && i64::from(y1) - dy <= i64::from(height);
                if !inside {
                    ok[(ty * tiles_x + tx) as usize] = false;
                }
            }
        }
        let mut out = Vec::new();
        for ty in 0..tiles_y {
            let mut tx = 0;
            while tx < tiles_x {
                if !ok[(ty * tiles_x + tx) as usize] {
                    tx += 1;
                    continue;
                }
                let start = tx;
                while tx < tiles_x && ok[(ty * tiles_x + tx) as usize] {
                    tx += 1;
                }
                let (x0, y0, _, y1) = tile_rect(start, ty);
                let (_, _, x1, _) = tile_rect(tx - 1, ty);
                out.push(Region::new(x0, y0, x1 - x0, y1 - y0));
            }
        }
        out
    }
}

/// Move the pixels of `regions` (destinations) from `(dx, dy)` earlier in
/// the same target. Every source must lie inside the target. Rows are moved
/// in place, ordered so that no source is overwritten before it is read.
pub fn move_pixels(target: &mut Target<'_>, regions: &[Region], dx: i32, dy: i32) {
    let stride = target.stride;
    // (destination row, first column, width) for every row of every region.
    let mut rows: Vec<(u32, u32, u32)> = regions
        .iter()
        .flat_map(|r| (r.y..r.y + r.height).map(move |y| (y, r.x, r.width)))
        .collect();
    // Moving down, the lowest rows go first; moving right, the rightmost.
    rows.sort_unstable_by(|a, b| {
        let by_row = if dy > 0 { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) };
        let by_column = if dx > 0 { b.1.cmp(&a.1) } else { a.1.cmp(&b.1) };
        by_row.then(by_column)
    });
    for (y, x, width) in rows {
        let sx = (i64::from(x) - i64::from(dx)) as usize;
        let sy = (i64::from(y) - i64::from(dy)) as usize;
        let src = sy * stride + sx * 4;
        let dst = y as usize * stride + x as usize * 4;
        let bytes = width as usize * 4;
        if src + bytes <= target.data.len() && dst + bytes <= target.data.len() {
            target.data.copy_within(src..src + bytes, dst);
        }
    }
}

/// The pixels `rect` touches, inside a `width` x `height` target. Bounds
/// that are not finite cover the whole target.
fn region_out(rect: Rect, width: u32, height: u32) -> Option<Region> {
    if !(rect.x0.is_finite() && rect.y0.is_finite() && rect.x1.is_finite() && rect.y1.is_finite()) {
        return Some(Region::new(0, 0, width, height));
    }
    let x0 = rect.x0.floor().clamp(0.0, f64::from(width)) as u32;
    let y0 = rect.y0.floor().clamp(0.0, f64::from(height)) as u32;
    let x1 = rect.x1.ceil().clamp(0.0, f64::from(width)) as u32;
    let y1 = rect.y1.ceil().clamp(0.0, f64::from(height)) as u32;
    (x1 > x0 && y1 > y0).then(|| Region::new(x0, y0, x1 - x0, y1 - y0))
}

/// The whole pixels inside `rect`, inside the target.
fn region_in(rect: Rect, width: u32, height: u32) -> Option<Region> {
    let x0 = rect.x0.ceil().clamp(0.0, f64::from(width)) as u32;
    let y0 = rect.y0.ceil().clamp(0.0, f64::from(height)) as u32;
    let x1 = rect.x1.floor().clamp(0.0, f64::from(width)) as u32;
    let y1 = rect.y1.floor().clamp(0.0, f64::from(height)) as u32;
    (x1 > x0 && y1 > y0).then(|| Region::new(x0, y0, x1 - x0, y1 - y0))
}

/// Whether `encoding` blends a layer with what is under it other than by
/// source over. Such a scene drawn onto a transparent target and composited
/// later would not match the scene drawn in place. Walks the draw tags only.
pub fn reads_backdrop(encoding: &Encoding) -> bool {
    decode::encoding_reads_backdrop(encoding)
}

/// Whether every layer `encoding` leaves open after its first `draws` draw
/// tags is a plain clip or a source-over group at full opacity: a draw there
/// composites onto the target as it would with no layer open, clipped.
/// `false` when `encoding` has fewer draw tags. Walks the draw tags only.
pub fn plain_layers_open_at(encoding: &Encoding, draws: usize) -> bool {
    decode::plain_layers_open_at(encoding, draws)
}

/// Device-space bounds of everything a scene paints, for a caller that must
/// know what lies over a region without rasterising the scene. Keep one: it
/// retains glyph outlines across calls.
#[derive(Default)]
pub struct DrawBounds {
    list: decode::DisplayList,
    glyphs: glyph::GlyphCache,
    images: image::ImageCache,
}

impl std::fmt::Debug for DrawBounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DrawBounds").finish_non_exhaustive()
    }
}

impl DrawBounds {
    /// Every pixel `encoding` can paint, as device-space rectangles: each
    /// draw's path (stroke reach included) or glyph outlines under its
    /// transform, and the clip of every layer that blends other than by
    /// source over. A layer's clip is not applied, so the bounds hold
    /// whatever it clips. `None` when some draw cannot be bounded.
    pub fn of(&mut self, encoding: &Encoding) -> Option<Vec<Rect>> {
        let mut stats = Stats::default();
        self.glyphs.begin_frame();
        let bounds = self
            .list
            .bounds(encoding, &mut self.glyphs, &mut self.images, &mut stats);
        self.glyphs.end_frame();
        bounds
    }
}

/// Regions smaller than this many pixels render on the calling thread.
pub const SINGLE_THREAD_PIXELS: u32 = 128 * 1024;

/// Construction options for [`CpuRasterizer`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Worker threads `vello_cpu` rasterizes large regions with; 0 renders
    /// everything on the calling thread. Regions under
    /// [`SINGLE_THREAD_PIXELS`] always render on the calling thread, where
    /// handing work to the pool costs more than it saves.
    pub threads: u16,
    /// Draw blurred rounded rectangles from cached coverage masks with a solid
    /// interior instead of evaluating the blur for every covered pixel.
    pub cached_blur: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threads: 0,
            cached_blur: true,
        }
    }
}

/// What one [`CpuRasterizer::render`] call did.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Walking the encoding into draws, including glyph and image lookups.
    pub decode: Duration,
    /// Handing the draws that touch a region to `vello_cpu`.
    pub record: Duration,
    /// `vello_cpu` rasterizing into the target.
    pub raster: Duration,
    /// Draw objects in the scene (fills, strokes, glyph runs, layers).
    pub draws: usize,
    /// Draws recorded, summed over regions.
    pub recorded: usize,
    /// Glyphs whose outline had to be built, rather than taken from the cache.
    pub glyph_misses: usize,
    /// Blur masks that had to be computed, rather than taken from the cache.
    pub blur_misses: usize,
    /// Draws using a feature this rasterizer skips (luminance masks).
    pub unsupported: usize,
}

/// Renders vello encodings into CPU buffers. Keep one per output: it retains
/// glyph outlines, converted images and blur masks across frames.
pub struct CpuRasterizer {
    options: Options,
    /// Rasterizes on the calling thread.
    ctx: Option<RenderContext>,
    /// Rasterizes on `options.threads` workers.
    threaded: Option<RenderContext>,
    resources: vello_cpu::Resources,
    list: decode::DisplayList,
    /// The scene decoded before `list`, for [`Self::changed_regions`].
    previous: decode::DisplayList,
    /// `list` holds a decoded scene that `previous` can be compared with.
    comparable: bool,
    glyphs: glyph::GlyphCache,
    images: image::ImageCache,
    scratch: record::Scratch,
}

impl std::fmt::Debug for CpuRasterizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuRasterizer")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl Default for CpuRasterizer {
    fn default() -> Self {
        Self::new(Options::default())
    }
}

impl CpuRasterizer {
    pub fn new(options: Options) -> Self {
        Self {
            options,
            ctx: None,
            threaded: None,
            resources: vello_cpu::Resources::new(),
            list: decode::DisplayList::default(),
            previous: decode::DisplayList::default(),
            comparable: false,
            glyphs: glyph::GlyphCache::default(),
            images: image::ImageCache::default(),
            scratch: record::Scratch::new(options.cached_blur),
        }
    }

    /// Render `encoding` over `base` into `target`.
    ///
    /// `regions: None` renders the whole target. Otherwise only the given
    /// regions are written; pixels outside them keep their contents, so a
    /// target retained from the previous frame stays complete.
    pub fn render(
        &mut self,
        encoding: &Encoding,
        target: &mut Target<'_>,
        regions: Option<&[Region]>,
        base: AlphaColor<Srgb>,
    ) -> Stats {
        let mut stats = self.prepare(encoding);
        let rendered = self.render_prepared(target, regions, base);
        stats.record = rendered.record;
        stats.raster = rendered.raster;
        stats.recorded = rendered.recorded;
        stats.blur_misses = rendered.blur_misses;
        stats
    }

    /// Decode `encoding` for [`Self::render_prepared`], keeping the scene
    /// decoded before it for [`Self::changed_regions`].
    pub fn prepare(&mut self, encoding: &Encoding) -> Stats {
        let mut stats = Stats::default();
        let start = Instant::now();
        std::mem::swap(&mut self.list, &mut self.previous);
        self.comparable = !self.previous.ops.is_empty();
        self.glyphs.begin_frame();
        self.images.begin_frame();
        self.list
            .decode(encoding, &mut self.glyphs, &mut self.images, &mut stats);
        self.glyphs.end_frame();
        self.images.end_frame();
        stats.draws = self.list.draw_count();
        stats.decode = start.elapsed();
        stats
    }

    /// Pixels of a `width` x `height` target that can differ between the
    /// scene prepared before and the one prepared last: the bounds of every
    /// draw between their first and last difference, rounded out to whole
    /// pixels: coverage never leaves the pixels a draw's bounds touch. `None`
    /// when there is no earlier scene or the difference is too large to list.
    pub fn changed_regions(&self, width: u32, height: u32) -> Option<Vec<Region>> {
        self.changes(width, height).map(|changes| changes.regions)
    }

    /// [`Self::changed_regions`], with the part of the change that is the old
    /// picture moved by whole pixels, when there is one.
    pub fn changes(&self, width: u32, height: u32) -> Option<SceneChanges> {
        if !self.comparable {
            return None;
        }
        let changes = diff::changes(&self.previous, &self.list)?;
        let outward = |rect: Rect| region_out(rect, width, height);
        Some(SceneChanges {
            regions: changes.bounds.into_iter().filter_map(outward).collect(),
            not_a_motion: changes.motion.as_ref().err().cloned(),
            motion: changes.motion.ok().map(|motion| PixelMotion {
                dx: motion.dx,
                dy: motion.dy,
                opaque: motion
                    .opaque
                    .into_iter()
                    .filter_map(|rect| region_in(rect, width, height))
                    .collect(),
                obstacles: motion.obstacles.into_iter().filter_map(outward).collect(),
            }),
        })
    }

    /// Render the scene last given to [`Self::prepare`], like
    /// [`Self::render`]. The decode fields of the returned stats are zero.
    pub fn render_prepared(
        &mut self,
        target: &mut Target<'_>,
        regions: Option<&[Region]>,
        base: AlphaColor<Srgb>,
    ) -> Stats {
        let mut stats = Stats::default();
        self.scratch.blurs.begin_frame();
        let full = [Region::new(0, 0, target.width, target.height)];
        let regions = regions.unwrap_or(&full);
        for region in regions {
            let Some(region) = region.clamp_to(target.width, target.height) else {
                continue;
            };
            self.render_region(target, region, base, &mut stats);
        }
        self.scratch.blurs.end_frame();
        stats
    }

    fn render_region(
        &mut self,
        target: &mut Target<'_>,
        region: Region,
        base: AlphaColor<Srgb>,
        stats: &mut Stats,
    ) {
        let (w, h) = (region.width as u16, region.height as u16);
        let threads = if region.width * region.height < SINGLE_THREAD_PIXELS {
            0
        } else {
            self.options.threads
        };
        let slot = if threads == 0 {
            &mut self.ctx
        } else {
            &mut self.threaded
        };
        let ctx = match slot {
            Some(ctx) => {
                if ctx.width() == w && ctx.height() == h {
                    ctx.reset();
                } else {
                    ctx.reset_and_resize(w, h);
                }
                ctx
            }
            None => slot.insert(RenderContext::new_with(
                w,
                h,
                RenderSettings {
                    level: Level::try_detect().unwrap_or(Level::baseline()),
                    num_threads: threads,
                },
            )),
        };

        let start = Instant::now();
        let recorded = record::record(
            ctx,
            &self.list,
            region.to_rect(),
            base,
            &mut self.scratch,
            stats,
        );
        ctx.flush();
        stats.recorded += recorded;
        stats.record += start.elapsed();

        let start = Instant::now();
        target.clear(region);
        let rows_start = region.y as usize * target.stride;
        let rows_end = rows_start + region.height as usize * target.stride;
        let pixmap_width = (target.stride / 4) as u16;
        if let Some(pixmap) =
            PixmapMut::new(pixmap_width, h, &mut target.data[rows_start..rows_end])
        {
            ctx.render_with(
                pixmap,
                &mut self.resources,
                RasterizerSettings {
                    render_mode: RenderMode::OptimizeSpeed,
                    target_init: vello_cpu::TargetInit::SrcOver,
                    offset: (region.x as u16, 0),
                    ..RasterizerSettings::default()
                },
            );
        }
        stats.raster += start.elapsed();
    }
}
