//! An app's frame in ordinary memory, rasterized on the CPU where it
//! changed.

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Buffer, Rectangle};
use tilo_cpu_raster::color::AlphaColor;
use tilo_cpu_raster::{CpuRasterizer, Options, Region, Target};
use vello::Scene;
use vello::kurbo::Rect;

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

    pub(crate) const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The alpha of one pixel, if it lies inside.
    pub(crate) fn alpha(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.size.0 || y >= self.size.1 {
            return None;
        }
        let index = (y as usize)
            .checked_mul(self.size.0 as usize)?
            .checked_add(x as usize)?
            .checked_mul(4)?
            .checked_add(3)?;
        self.pixels.get(index).copied()
    }

    /// Rasterize `scene` where `damage` says it changed (`None`: all of it)
    /// and copy what was written into `buffer`, telling it what changed.
    pub(crate) fn present(
        &mut self,
        scene: &Scene,
        damage: Option<&[Rect]>,
        buffer: &mut MemoryRenderBuffer,
    ) {
        let regions = self.draw(scene, damage);
        if regions.is_empty() {
            return;
        }
        let pixels = self.pixels();
        let width = self.size().0 as usize;
        let drawn = buffer.render().draw(|memory: &mut [u8]| {
            for region in &regions {
                for row in region.y..region.y.saturating_add(region.height) {
                    let start = (row as usize)
                        .saturating_mul(width)
                        .saturating_add(region.x as usize)
                        .saturating_mul(4);
                    let end = start.saturating_add((region.width as usize).saturating_mul(4));
                    if let (Some(to), Some(from)) =
                        (memory.get_mut(start..end), pixels.get(start..end))
                    {
                        to.copy_from_slice(from);
                    }
                }
            }
            Ok::<_, std::convert::Infallible>(
                regions
                    .iter()
                    .filter_map(|region| {
                        Some(Rectangle::<i32, Buffer>::new(
                            (i32::try_from(region.x).ok()?, i32::try_from(region.y).ok()?).into(),
                            (
                                i32::try_from(region.width).ok()?,
                                i32::try_from(region.height).ok()?,
                            )
                                .into(),
                        ))
                    })
                    .collect(),
            )
        });
        if let Err(never) = drawn {
            match never {}
        }
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
        let Ok(mut target) = Target::new(&mut self.pixels, width, height, stride) else {
            return Vec::new();
        };
        self.rasterizer.prepare(scene.encoding());
        // Where the app draws nothing the layer stays transparent.
        self.rasterizer.render_prepared(
            &mut target,
            Some(&regions),
            AlphaColor::new([0.0, 0.0, 0.0, 0.0]),
        );
        self.valid = true;
        regions
    }
}

/// `rects` as whole-pixel regions of a `width` by `height` target: rounded
/// out, clipped, and merged where they overlap so no pixel is drawn twice.
fn regions(rects: &[Rect], width: u32, height: u32) -> Vec<Region> {
    let bounds = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let mut merged: Vec<Rect> = Vec::new();
    for rect in rects {
        let mut rect = rect.expand().intersect(bounds);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        while let Some(index) = merged
            .iter()
            .position(|other| other.intersect(rect).area() > 0.0)
        {
            rect = rect.union(merged.swap_remove(index));
        }
        merged.push(rect);
    }
    merged
        .into_iter()
        .map(|rect| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "whole-pixel rectangles clipped to the target"
            )]
            let region = Region::new(
                rect.x0 as u32,
                rect.y0 as u32,
                rect.width() as u32,
                rect.height() as u32,
            );
            region
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_is_rounded_out_clipped_and_merged() {
        let rects = [
            Rect::new(1.5, 1.5, 4.2, 4.2),
            Rect::new(3.0, 3.0, 6.0, 6.0),
            Rect::new(90.0, 90.0, 120.0, 120.0),
            Rect::new(-5.0, -5.0, -1.0, -1.0),
        ];
        let mut out = regions(&rects, 100, 100);
        out.sort_by_key(|region| (region.x, region.y));
        assert_eq!(
            out,
            vec![Region::new(1, 1, 5, 5), Region::new(90, 90, 10, 10)]
        );
    }
}
