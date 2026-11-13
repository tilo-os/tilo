//! Drawing a picture where no GPU adapter is: on the CPU, with `tilo-cpu-raster`. The native toolkit
//! has it; a browser page has no CPU rasterizer and uses `no_cpu.rs` instead, which draws nothing.

use std::sync::Mutex;

use tilo_lock::MutexRecover as _;
use vello::Scene;
use vello::peniko::Color;

/// What `scene` draws over `bg` at `width` x `height` without a GPU: always a picture here, or why the
/// CPU rasterizer could not draw one.
pub(super) fn render(
    scene: &Scene,
    width: u32,
    height: u32,
    bg: Color,
) -> Option<Result<Vec<u8>, String>> {
    tracing::debug!(
        width,
        height,
        "no GPU adapter: drawing a picture on the CPU"
    );
    Some(render_on_cpu(scene, width, height, bg))
}

/// What `scene` draws over `bg` at `width` x `height`, rasterised on the CPU by `tilo-cpu-raster`:
/// straight-alpha RGBA8, as the GPU renderer returns it.
fn render_on_cpu(scene: &Scene, width: u32, height: u32, bg: Color) -> Result<Vec<u8>, String> {
    let stride = (width as usize).saturating_mul(4);
    let mut rgba = vec![0; stride.saturating_mul(height as usize)];
    let mut target = tilo_cpu_raster::Target::new(&mut rgba, width, height, stride)
        .map_err(|error| format!("no CPU renderer for a {width}x{height} picture: {error}"))?;
    let base = tilo_cpu_raster::color::AlphaColor::new(bg.components);
    let mut rasterizer = CPU_RASTERIZER.lock_recover();
    rasterizer
        .get_or_insert_with(tilo_cpu_raster::CpuRasterizer::default)
        .render(scene.encoding(), &mut target, None, base);
    drop(rasterizer);
    if bg.components[3] >= 1.0 {
        // Everything drawn over an opaque backdrop is opaque; the 8-bit
        // pipeline's rounding leaves a few pixels at 254, which the GPU path
        // does not. Premultiplied and straight colour agree at full alpha.
        for pixel in rgba.chunks_exact_mut(4) {
            if let Some(alpha) = pixel.last_mut() {
                *alpha = u8::MAX;
            }
        }
    } else {
        unpremultiply(&mut rgba);
    }
    Ok(rgba)
}

/// A surface's frames kept in memory and drawn on the CPU: what a host shows on a machine where the
/// toolkit has no GPU adapter to draw them on, exactly as it draws a normal app's frame there.
pub struct CpuSurface {
    rasterizer: tilo_cpu_raster::CpuRasterizer,
    /// Premultiplied RGBA, `width * 4` bytes a row.
    pixels: Vec<u8>,
    size: (u32, u32),
    /// The pixels hold a whole earlier frame, so damage can be drawn over them.
    valid: bool,
}

impl CpuSurface {
    /// A surface of `width` x `height` pixels, rasterized with up to `threads` cores, or `None` where
    /// this build has no CPU rasterizer.
    #[must_use]
    pub fn new(width: u32, height: u32, threads: u16) -> Option<Self> {
        let length = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        Some(Self {
            rasterizer: tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options {
                threads,
                ..tilo_cpu_raster::Options::default()
            }),
            pixels: vec![0; length],
            size: (width, height),
            valid: false,
        })
    }

    /// Its size in pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The pixels drawn last, premultiplied RGBA, `width * 4` bytes a row.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Draw `scene` (physical pixels) over the pixels already here, in the `damage` regions of the
    /// surface as `(x, y, width, height)`.
    pub fn draw(
        &mut self,
        scene: &vello::Scene,
        damage: Option<&[(u32, u32, u32, u32)]>,
    ) -> Option<Vec<(u32, u32, u32, u32)>> {
        let (width, height) = self.size;
        self.rasterizer.prepare(scene.encoding());
        let whole = tilo_cpu_raster::Region::new(0, 0, width, height);
        let inside = |&(x, y, w, h): &(u32, u32, u32, u32)| {
            let x = x.min(width);
            let y = y.min(height);
            let w = w.min(width.saturating_sub(x));
            let h = h.min(height.saturating_sub(y));
            (w > 0 && h > 0).then(|| tilo_cpu_raster::Region::new(x, y, w, h))
        };
        let mut moved = Vec::new();
        let mut shift = None;
        let drawn: Vec<tilo_cpu_raster::Region> = if !self.valid {
            vec![whole]
        } else if let Some(changes) = self.rasterizer.changes(width, height) {
            let work = changes.work(width, height, MOVED_TILE);
            moved = work.moved;
            shift = changes.motion.map(|motion| (motion.dx, motion.dy));
            work.drawn
        } else {
            match damage {
                Some(regions) => regions.iter().filter_map(inside).collect(),
                None => vec![whole],
            }
        };
        if drawn.is_empty() && moved.is_empty() {
            return None;
        }
        // The rasterizer walks its scene once per region, so a frame that
        // changed in many places is drawn in their bounds instead: the walk
        // is what it pays for, and a hundred small regions cost more than
        // the picture they cover.
        let drawn = if drawn.len() > MAX_REGIONS {
            vec![bounds(&drawn, width, height)]
        } else {
            drawn
        };
        let stride = (width as usize).saturating_mul(4);
        let mut target =
            tilo_cpu_raster::Target::new(&mut self.pixels, width, height, stride).ok()?;
        if let Some((dx, dy)) = shift.filter(|_| !moved.is_empty()) {
            tilo_cpu_raster::move_pixels(&mut target, &moved, dx, dy);
        }
        // Where the app draws nothing the surface stays transparent, so the
        // desktop shows through.
        if !drawn.is_empty() {
            self.rasterizer.render_prepared(
                &mut target,
                Some(&drawn),
                tilo_cpu_raster::color::AlphaColor::new([0.0, 0.0, 0.0, 0.0]),
            );
        }
        self.valid = true;
        let mut changed = drawn;
        changed.extend(moved);
        Some(
            changed
                .into_iter()
                .map(|region| (region.x, region.y, region.width, region.height))
                .collect(),
        )
    }
}

/// Tiles a change's motion is proven in, and the regions around it are drawn in: fine enough that
/// little is redrawn beside what moved.
const MOVED_TILE: u32 = 16;

/// Regions a frame is drawn in before it is drawn in their bounds: each one walks the prepared scene,
/// and past this many that walk costs more than drawing what lies between them.
const MAX_REGIONS: usize = 32;

fn bounds(regions: &[tilo_cpu_raster::Region], width: u32, height: u32) -> tilo_cpu_raster::Region {
    let left = regions.iter().map(|region| region.x).min().unwrap_or(0);
    let top = regions.iter().map(|region| region.y).min().unwrap_or(0);
    let right = regions
        .iter()
        .map(|region| region.x.saturating_add(region.width))
        .max()
        .unwrap_or(width)
        .min(width);
    let bottom = regions
        .iter()
        .map(|region| region.y.saturating_add(region.height))
        .max()
        .unwrap_or(height)
        .min(height);
    tilo_cpu_raster::Region::new(
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    )
}

/// The CPU rasterizer [`render_on_cpu`] draws with, kept for the glyph outlines and images it has
/// converted.
static CPU_RASTERIZER: Mutex<Option<tilo_cpu_raster::CpuRasterizer>> = Mutex::new(None);

fn unpremultiply(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let Some((&mut alpha, colour)) = pixel.split_last_mut() else {
            continue;
        };
        if alpha == 0 || alpha == u8::MAX {
            continue;
        }
        let alpha = u16::from(alpha);
        for channel in colour {
            // Rounded to nearest: (c * 255 + a / 2) / a, at most 255.
            let straight = u16::from(*channel)
                .saturating_mul(255)
                .saturating_add(alpha >> 1)
                .checked_div(alpha)
                .unwrap_or(u16::MAX);
            *channel = u8::try_from(straight).unwrap_or(u8::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "test assertions read fixed pixel offsets"
    )]

    use super::render_on_cpu;
    use crate::offscreen::CpuSurface;
    use vello::Scene;
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        let mut out = [0; 4];
        out.copy_from_slice(&rgba[at..at + 4]);
        out
    }

    /// Without an adapter a picture is still drawn, on the CPU, at the asked size and in straight alpha
    /// like the GPU path.
    #[test]
    fn a_picture_without_a_gpu_is_drawn_on_the_cpu() {
        let (width, height) = (40, 30);
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0xff, 0x00, 0x00),
            None,
            &Rect::new(0.0, 0.0, 20.0, 30.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0x00, 0x00, 0xff, 0x80),
            None,
            &Rect::new(20.0, 0.0, 40.0, 30.0),
        );
        let rgba = render_on_cpu(&scene, width, height, Color::TRANSPARENT)
            .expect("the CPU rasterizer draws it");
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        assert_eq!(pixel(&rgba, width, 5, 5), [0xff, 0x00, 0x00, 0xff]);
        let [r, g, b, a] = pixel(&rgba, width, 30, 15);
        assert_eq!((r, g), (0, 0));
        assert!(b >= 0xfe, "straight alpha keeps the full blue, got {b}");
        assert!(a.abs_diff(0x80) <= 1, "half coverage, got {a}");
    }

    #[test]
    fn the_backdrop_shows_where_nothing_is_drawn() {
        let grey = Color::from_rgb8(0x33, 0x44, 0x55);
        let rgba = render_on_cpu(&Scene::new(), 8, 8, grey).expect("drawn");
        assert!(
            rgba.chunks_exact(4)
                .all(|pixel| pixel == [0x33, 0x44, 0x55, 0xff]),
            "every pixel is the backdrop"
        );
    }

    /// A frame is drawn where this surface saw its own frames differ, more finely than a caller's
    /// damage says, and a frame that changed nothing costs nothing.
    #[test]
    fn a_frame_is_drawn_only_where_its_own_raster_saw_it_change() {
        let (width, height) = (64, 48);
        let mut surface = CpuSurface::new(width, height, 1).expect("the CPU rasterizer draws it");
        let chip = |x: f64| {
            let mut scene = Scene::new();
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(0x20, 0x60, 0xc0),
                None,
                &Rect::new(x, 8.0, x + 16.0, 24.0),
            );
            scene
        };
        let first = surface
            .draw(&chip(8.0), None)
            .expect("the first frame covers the surface");
        assert_eq!(first, vec![(0, 0, width, height)]);

        // The caller says nothing changed; the pixels this surface keeps say
        // the chip moved, and they are the ones drawn.
        let moved = surface
            .draw(&chip(40.0), Some(&[]))
            .expect("a moved chip changed pixels");
        let covered = |x: u32, y: u32| {
            moved
                .iter()
                .any(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
        };
        assert!(covered(8, 16) && covered(55, 16), "where it was and is");
        assert!(!covered(30, 40), "the pixels it never touched");
        let full = render_on_cpu(&chip(40.0), width, height, Color::TRANSPARENT).expect("drawn");
        assert_eq!(
            pixel(surface.pixels(), width, 44, 16),
            pixel(&full, width, 44, 16),
            "the chip is where a whole render of the frame puts it"
        );
        assert_eq!(
            pixel(surface.pixels(), width, 12, 16),
            [0, 0, 0, 0],
            "and the surface kept nothing of where it was"
        );

        assert!(
            surface.draw(&chip(40.0), None).is_none(),
            "an unchanged frame draws nothing"
        );
    }

    /// A frame that only moved, as a strip that slides does, is moved rather than drawn again, and what
    /// it leaves behind is a full render all the same: the pixels it left and the pixels it reached.
    #[test]
    fn a_frame_that_only_moves_leaves_a_full_render_behind() {
        let (width, height) = (200, 120);
        let mut surface = CpuSurface::new(width, height, 1).expect("the CPU rasterizer draws it");
        let block = |x: f64| {
            let mut scene = Scene::new();
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(0x20, 0x60, 0xc0),
                None,
                &Rect::new(x, 10.0, x + 120.0, 100.0),
            );
            scene
        };
        let first = surface
            .draw(&block(10.0), None)
            .expect("the first frame covers the surface");
        assert_eq!(first, vec![(0, 0, width, height)]);

        surface
            .draw(&block(50.0), Some(&[]))
            .expect("the block moved");
        let full = render_on_cpu(&block(50.0), width, height, Color::TRANSPARENT).expect("drawn");
        let differing: Vec<(u32, u32)> = surface
            .pixels()
            .chunks_exact(4)
            .zip(full.chunks_exact(4))
            .enumerate()
            .filter(|(_, (moved, drawn))| {
                moved
                    .iter()
                    .zip(drawn.iter())
                    .any(|(a, b)| a.abs_diff(*b) > 1)
            })
            .map(|(at, _)| {
                let at = at as u32;
                (at % width, at / width)
            })
            .collect();
        assert!(
            differing.is_empty(),
            "moved and drawn pixels equal a whole render: {differing:?}"
        );
        assert_eq!(
            pixel(surface.pixels(), width, 11, 12),
            [0, 0, 0, 0],
            "the surface kept nothing of where the block was"
        );
    }

    /// A frame that changed in more places than a scene walk is worth is drawn in one region covering
    /// them, and leaves the same picture behind.
    #[test]
    fn many_changed_places_are_drawn_in_their_bounds() {
        let (width, height) = (200, 100);
        let mut surface = CpuSurface::new(width, height, 1).expect("the CPU rasterizer draws it");
        let chips = |shift: f64| {
            let mut scene = Scene::new();
            for index in 0..40 {
                let x = 2.0 + f64::from(index) * 4.0 + shift;
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(0x20, 0x60, 0xc0),
                    None,
                    &Rect::new(x, 10.0, x + 2.0, 12.0),
                );
            }
            scene
        };
        let first = surface
            .draw(&chips(0.0), None)
            .expect("the first frame covers the surface");
        assert_eq!(first, vec![(0, 0, width, height)]);

        let moved = surface
            .draw(&chips(20.0), Some(&[]))
            .expect("every chip moved");
        assert_eq!(
            moved,
            vec![(2, 10, 178, 2)],
            "the chips and where they were are drawn as the one region they cover"
        );
        let full = render_on_cpu(&chips(20.0), width, height, Color::TRANSPARENT).expect("drawn");
        for index in [0_u32, 20, 39] {
            let x = 2 + index * 4 + 20;
            assert_eq!(
                pixel(surface.pixels(), width, x, 10),
                pixel(&full, width, x, 10),
                "the chip at {x} is where a whole render of the frame puts it"
            );
        }
    }
}
