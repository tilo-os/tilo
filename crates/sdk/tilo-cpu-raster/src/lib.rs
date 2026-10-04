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
        let mut stats = Stats::default();
        let start = Instant::now();
        self.glyphs.begin_frame();
        self.images.begin_frame();
        self.scratch.blurs.begin_frame();
        self.list
            .decode(encoding, &mut self.glyphs, &mut self.images, &mut stats);
        stats.draws = self.list.draw_count();
        stats.decode = start.elapsed();

        let full = [Region::new(0, 0, target.width, target.height)];
        let regions = regions.unwrap_or(&full);
        for region in regions {
            let Some(region) = region.clamp_to(target.width, target.height) else {
                continue;
            };
            self.render_region(target, region, base, &mut stats);
        }
        self.glyphs.end_frame();
        self.images.end_frame();
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
