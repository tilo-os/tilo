//! An app's frame in ordinary memory, rasterized on the CPU where it
//! changed: what draws a surface's frames on a machine where the toolkit
//! draws every frame on the CPU, because it has no GPU to draw them on.

use tilo_compositor::Region;
use tilo_cpu_raster::color::AlphaColor;
use tilo_cpu_raster::{CpuRasterizer, Options};
use tilo_cpu_raster::{Region as RasterRegion, Target as RasterTarget};
use vello::Scene;
use vello::kurbo::Rect;

use crate::frames::regions;

/// Cores the rasterizer uses where it has them.
pub(crate) const RASTER_THREADS: u16 = 2;

pub(crate) struct Canvas {
    rasterizer: CpuRasterizer,
    /// Premultiplied RGBA, `width * 4` bytes a row.
    pixels: Vec<u8>,
    size: (u32, u32),
    /// The pixels hold a whole earlier frame, so damage can be drawn over
    /// them.
    valid: bool,
}

impl Canvas {
    pub(crate) fn new(size: (u32, u32), threads: u16) -> Self {
        let length = (size.0 as usize)
            .saturating_mul(size.1 as usize)
            .saturating_mul(4);
        Self {
            rasterizer: CpuRasterizer::new(Options {
                threads,
                ..Options::default()
            }),
            pixels: vec![0; length],
            size,
            valid: false,
        }
    }

    pub(crate) fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Rasterize `scene` where `damage` says it changed (`None`: all of it)
    /// and return the regions written.
    pub(crate) fn draw(&mut self, scene: &Scene, damage: Option<&[Rect]>) -> Vec<Region> {
        let (width, height) = self.size;
        let regions = match damage {
            Some(rects) if self.valid => regions(rects, width, height),
            _ => vec![Region::new(0, 0, width, height)],
        };
        if regions.is_empty() {
            return regions;
        }
        let stride = (width as usize).saturating_mul(4);
        let Ok(mut target) = RasterTarget::new(&mut self.pixels, width, height, stride) else {
            return Vec::new();
        };
        let drawn: Vec<RasterRegion> = regions
            .iter()
            .map(|region| RasterRegion::new(region.x, region.y, region.width, region.height))
            .collect();
        self.rasterizer.prepare(scene.encoding());
        // Where the app draws nothing the layer stays transparent.
        self.rasterizer.render_prepared(
            &mut target,
            Some(&drawn),
            AlphaColor::new([0.0, 0.0, 0.0, 0.0]),
        );
        self.valid = true;
        regions
    }
}
