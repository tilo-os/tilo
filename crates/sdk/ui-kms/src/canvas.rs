//! The frame in ordinary memory, rasterized on the CPU where it changed.

use tilo_cpu_raster::{CpuRasterizer, Options, Region, Target};
use vello::Scene;
use vello::kurbo::Rect;

pub struct Canvas {
    rasterizer: CpuRasterizer,
    pixels: Vec<u8>,
    size: (u32, u32),
    /// The pixels hold a whole earlier frame, so damage can be painted over
    /// them.
    valid: bool,
}

impl Canvas {
    /// A canvas `size` pixels large, rasterized on up to `threads` cores.
    pub fn new(size: (u32, u32), threads: u16) -> Self {
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

    /// Premultiplied RGBA, `width * 4` bytes a row.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Rasterize `scene` where `damage` says it changed (`None`: all of it)
    /// and return the pixel regions written.
    pub fn draw(&mut self, scene: &Scene, damage: Option<&[Rect]>) -> Vec<Region> {
        let (width, height) = self.size;
        let whole = vec![Region::new(0, 0, width, height)];
        let regions = match damage {
            Some(rects) if self.valid => regions(rects, width, height),
            _ => whole,
        };
        if regions.is_empty() {
            return regions;
        }
        let stride = (width as usize).saturating_mul(4);
        let Ok(mut target) = Target::new(&mut self.pixels, width, height, stride) else {
            return Vec::new();
        };
        self.rasterizer.prepare(scene.encoding());
        self.rasterizer.render_prepared(
            &mut target,
            Some(&regions),
            tilo_cpu_raster::color::AlphaColor::new([0.0, 0.0, 0.0, 1.0]),
        );
        self.valid = true;
        regions
    }
}

/// `rects` in scene pixels as whole-pixel regions of a `width` by `height`
/// target: rounded out, clipped, and merged where they overlap so no pixel
/// is rasterized twice.
pub(crate) fn regions(rects: &[Rect], width: u32, height: u32) -> Vec<Region> {
    let bounds = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let mut out: Vec<Rect> = Vec::new();
    for rect in rects {
        let mut rect = rect.expand().intersect(bounds);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        // Fold in every region this one touches, until none does.
        while let Some(index) = out
            .iter()
            .position(|other| other.intersect(rect).area() > 0.0)
        {
            rect = rect.union(out.swap_remove(index));
        }
        out.push(rect);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded-out rectangles clipped to a target of u32 sides"
    )]
    out.into_iter()
        .map(|rect| {
            Region::new(
                rect.x0 as u32,
                rect.y0 as u32,
                rect.width() as u32,
                rect.height() as u32,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Affine;
    use vello::peniko::{Color, Fill};

    #[test]
    fn damage_rounds_out_clips_and_merges() {
        let regions = regions(
            &[
                Rect::new(1.5, 1.5, 4.2, 3.0),
                Rect::new(3.0, 2.0, 6.0, 5.0),
                Rect::new(90.0, 90.0, 120.0, 95.0),
                Rect::new(-5.0, -5.0, -1.0, -1.0),
            ],
            100,
            100,
        );
        assert_eq!(
            regions,
            [Region::new(1, 1, 5, 4), Region::new(90, 90, 10, 5)]
        );
    }

    fn red_square(at: f64) -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 255),
            None,
            &Rect::new(at, at, at + 4.0, at + 4.0),
        );
        scene
    }

    fn pixel(canvas: &Canvas, x: usize, y: usize) -> [u8; 4] {
        let offset = y
            .checked_mul(canvas.size.0 as usize)
            .and_then(|row| row.checked_add(x))
            .and_then(|at| at.checked_mul(4))
            .unwrap();
        canvas.pixels()[offset..][..4].try_into().unwrap()
    }

    #[test]
    fn the_first_frame_is_whole_and_later_ones_only_where_damaged() {
        let mut canvas = Canvas::new((16, 16), 0);
        assert_eq!(
            canvas.draw(&red_square(0.0), Some(&[Rect::new(0.0, 0.0, 1.0, 1.0)])),
            [Region::new(0, 0, 16, 16)],
            "nothing to paint over yet"
        );
        assert_eq!(pixel(&canvas, 1, 1), [255, 0, 0, 255]);
        // The square moved, but only its new place is said to be damaged:
        // the old one stays, as the damage asked.
        canvas.draw(&red_square(8.0), Some(&[Rect::new(8.0, 8.0, 12.0, 12.0)]));
        assert_eq!(pixel(&canvas, 1, 1), [255, 0, 0, 255]);
        assert_eq!(pixel(&canvas, 9, 9), [255, 0, 0, 255]);
        canvas.draw(&red_square(8.0), None);
        assert_eq!(pixel(&canvas, 1, 1), [0, 0, 0, 255]);
    }
}
