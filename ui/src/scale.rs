//! Display-scale plumbing shared by the render loop.
//!
//! qOS separates **two** scale factors, multiplied at composite time:
//! **device scale** — the per-output fractional factor the compositor
//! reports via `wp_fractional_scale_v1` (a true fraction, not rounded to an
//! integer) — and **user UI scale**, the "make everything bigger"
//! preference already baked into [`crate::theme::Theme`]'s tokens.
//!
//! Widgets lay out in **logical pixels** (already grown by the user scale
//! via the theme); [`composite_transform`] applies only the **device**
//! scale when drawing into the Vello scene. Because Vello rasterises vector
//! geometry at the *transformed* resolution, the result is crisp at any
//! fractional device scale — no intermediate low-res raster gets blurrily
//! upscaled.

use vello::kurbo::Affine;

/// Uniform scale by the **device** factor only — widget rects live in
/// logical space, and Vello rasterises at the transformed (physical)
/// resolution.
#[inline]
pub fn composite_transform(device_scale: f64) -> Affine {
    Affine::scale(device_scale)
}

/// The available space handed to the layout solver: widgets size
/// themselves in logical px, so the physical window is divided by the
/// device scale before layout.
#[inline]
pub fn logical_size(physical: (u32, u32), device_scale: f64) -> (f64, f64) {
    let s = if device_scale > 0.0 {
        device_scale
    } else {
        1.0
    };
    (f64::from(physical.0) / s, f64::from(physical.1) / s)
}

#[inline]
pub fn physical_len(logical: f64, device_scale: f64) -> f64 {
    logical * device_scale
}

/// Inverse of the layout division in [`logical_size`]. Used by tests and
/// hit-testing to reason about where logical geometry lands physically.
#[inline]
pub fn physical_point(logical: (f64, f64), device_scale: f64) -> (f64, f64) {
    (logical.0 * device_scale, logical.1 * device_scale)
}

/// `ui_scale`/`text_scale` are accepted separately so a caller starting
/// from an *unscaled* base size can compute the whole chain in one
/// explicit, testable call.
#[inline]
pub fn physical_font_px(base_font: f32, ui_scale: f32, text_scale: f32, device_scale: f32) -> f32 {
    base_font * ui_scale * text_scale * device_scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offscreen::OffscreenRenderer;
    use crate::text_render::TextRenderer;
    use vello::Scene;
    use vello::kurbo::Rect;
    use vello::peniko::{Color, Fill};

    #[test]
    fn composite_transform_is_uniform_device_scale() {
        for scale in [1.0f64, 1.25, 1.5, 1.75, 2.0] {
            let t = composite_transform(scale);
            let p = t * vello::kurbo::Point::new(40.0, 30.0);
            assert!((p.x - 40.0 * scale).abs() < 1e-9);
            assert!((p.y - 30.0 * scale).abs() < 1e-9);
            // Matrix carries no translation/shear — pure scale.
            let c = t.as_coeffs();
            assert!((c[0] - scale).abs() < 1e-9 && (c[3] - scale).abs() < 1e-9);
            assert!(c[1].abs() < 1e-12 && c[2].abs() < 1e-12);
            assert!(c[4].abs() < 1e-12 && c[5].abs() < 1e-12);
        }
    }

    #[test]
    fn logical_available_space_divides_by_fractional_scale() {
        assert_eq!(logical_size((2560, 1600), 1.25), (2048.0, 1280.0));
        assert_eq!(logical_size((1920, 1080), 1.5), (1280.0, 720.0));
        let (lw, lh) = logical_size((3360, 1890), 1.75);
        assert!((lw - 1920.0).abs() < 1e-9 && (lh - 1080.0).abs() < 1e-9);
        // A zero/garbage scale degrades to 1.0 rather than dividing by zero.
        assert_eq!(logical_size((800, 600), 0.0), (800.0, 600.0));
    }

    #[test]
    fn physical_font_px_composes_device_and_user_scales() {
        let px = physical_font_px(14.0, 1.25, 1.2, 1.5);
        assert!((px - 14.0 * 1.25 * 1.2 * 1.5).abs() < 1e-4, "got {px}");
        // Neutral user scales leave only the device factor.
        assert!((physical_font_px(16.0, 1.0, 1.0, 1.75) - 28.0).abs() < 1e-4);
        // Neutral device leaves only the user factors.
        assert!((physical_font_px(10.0, 1.5, 1.0, 1.0) - 15.0).abs() < 1e-4);
    }

    #[test]
    fn fill_lands_at_physical_coordinates_at_fractional_scales() {
        for scale in [1.25f64, 1.5, 1.75] {
            let (lw, lh) = (100.0f64, 60.0f64);
            let pw = physical_len(lw, scale).round() as u32;
            let ph = physical_len(lh, scale).round() as u32;

            let mut scene = Scene::new();
            let rect = Rect::new(20.0, 15.0, 60.0, 45.0); // logical
            scene.fill(
                Fill::NonZero,
                composite_transform(scale),
                Color::WHITE,
                None,
                &rect,
            );

            let mut r = OffscreenRenderer::new(pw, ph);
            let buf = r.render(&scene, Color::BLACK);

            let (cx, cy) = physical_point((40.0, 30.0), scale);
            let (cr, _, _, _) = OffscreenRenderer::sample(&buf, pw, cx as u32, cy as u32);
            assert!(
                cr > 200,
                "rect centre inked at physical ({cx:.0},{cy:.0}) for scale {scale}, got r={cr}"
            );

            let (ox, oy) = physical_point((6.0, 6.0), scale);
            let (obr, _, _, _) = OffscreenRenderer::sample(&buf, pw, ox as u32, oy as u32);
            assert!(
                obr < 60,
                "background stays black outside the rect for scale {scale}, got r={obr}"
            );
        }
    }

    #[test]
    fn text_metrics_are_logical_and_physical_extent_scales() {
        let mut tr = TextRenderer::new();
        let (lw, lh) = tr.measure("Fractional scaling", 16.0, None);
        assert!(lw > 0.0 && lh > 0.0);
        for scale in [1.0f64, 1.25, 1.5, 1.75] {
            let (w2, h2) = tr.measure("Fractional scaling", 16.0, None);
            assert_eq!(
                (w2, h2),
                (lw, lh),
                "measure is scale-free (logical) at {scale}"
            );
            let pw = physical_len(lw as f64, scale);
            assert!(
                (pw - lw as f64 * scale).abs() < 1e-6,
                "physical width = logical × {scale}"
            );
        }
    }

    /// Models the "blurry 2× upscale" a non-scale-aware pipeline would produce.
    fn nearest_upscale(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
        let mut out = vec![0u8; (dw * dh * 4) as usize];
        for y in 0..dh {
            let sy = (y * sh / dh).min(sh - 1);
            for x in 0..dw {
                let sx = (x * sw / dw).min(sw - 1);
                let si = ((sy * sw + sx) * 4) as usize;
                let di = ((y * dw + x) * 4) as usize;
                out[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
        out
    }

    /// Distinct red-channel levels on a grey scene — a proxy for how many
    /// anti-aliased edge gradations a raster carries.
    fn distinct_luma_levels(buf: &[u8], w: u32, h: u32) -> usize {
        let mut seen = [false; 256];
        for i in 0..(w * h) as usize {
            seen[buf[i * 4] as usize] = true;
        }
        seen.iter().filter(|&&b| b).count()
    }

    /// Native render at device scale 2.0 must carry strictly MORE
    /// anti-aliased luminance levels than nearest-upscaling the scale-1
    /// raster to the same size (which invents no new detail).
    #[test]
    fn native_fractional_render_is_crisper_than_upscaling() {
        let (lw, lh) = (140u32, 44u32);
        let mut tr = TextRenderer::new();

        let draw = |scale: f64, tr: &mut TextRenderer| -> (Vec<u8>, u32, u32) {
            let pw = (lw as f64 * scale).round() as u32;
            let ph = (lh as f64 * scale).round() as u32;
            let mut scene = Scene::new();
            tr.draw_text(
                &mut scene,
                "Ashgraphy",
                26.0,
                Color::WHITE,
                composite_transform(scale),
                None,
            );
            let mut r = OffscreenRenderer::new(pw, ph);
            (r.render(&scene, Color::BLACK), pw, ph)
        };

        let (buf1, w1, h1) = draw(1.0, &mut tr);
        let (buf2, w2, h2) = draw(2.0, &mut tr);

        let upscaled = nearest_upscale(&buf1, w1, h1, w2, h2);
        let native_levels = distinct_luma_levels(&buf2, w2, h2);
        let upscaled_levels = distinct_luma_levels(&upscaled, w2, h2);

        assert!(
            native_levels > upscaled_levels,
            "native 2× render must carry more anti-alias detail than an upscale: \
             native={native_levels} upscaled={upscaled_levels}",
        );
        assert!(
            native_levels > 4,
            "the native render produced real anti-aliased text"
        );
    }
}
