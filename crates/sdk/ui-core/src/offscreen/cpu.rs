//! Drawing a picture where no GPU adapter is: on the CPU, with
//! `tilo-cpu-raster`. The native toolkit has it; a browser page has no CPU
//! rasterizer and uses `no_cpu.rs` instead, which draws nothing.

use std::sync::Mutex;

use tilo_lock::MutexRecover as _;
use vello::Scene;
use vello::peniko::Color;

/// What `scene` draws over `bg` at `width` x `height` without a GPU: always
/// a picture here, or why the CPU rasterizer could not draw one.
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

/// What `scene` draws over `bg` at `width` x `height`, rasterised on the
/// CPU by `tilo-cpu-raster`: straight-alpha RGBA8, as the GPU renderer
/// returns it. For when no [`OffscreenRenderer`](super::OffscreenRenderer) can be built.
///
/// # Errors
/// Returns why the CPU rasterizer has no target of this size.
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

/// The CPU rasterizer [`render_on_cpu`] draws with, kept for the glyph
/// outlines and images it has converted.
static CPU_RASTERIZER: Mutex<Option<tilo_cpu_raster::CpuRasterizer>> = Mutex::new(None);

/// Premultiplied RGBA8 to straight alpha, in place.
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
    use vello::Scene;
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        let mut out = [0; 4];
        out.copy_from_slice(&rgba[at..at + 4]);
        out
    }

    /// Without an adapter a picture is still drawn, on the CPU, at the
    /// asked size and in straight alpha like the GPU path.
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

    /// The backdrop fills what the scene leaves uncovered, and over an
    /// opaque one every pixel is opaque.
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
}
