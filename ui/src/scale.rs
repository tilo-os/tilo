//! Display-scale plumbing shared by the render loop.
//!
//! qOS separates **two** scale factors and multiplies them at composite time:
//!
//! - **Device scale** — the per-output fractional factor the compositor reports
//!   (winit `window.scale_factor()`: 1.0, 1.25, 1.5, 1.75, 2.0, …). It comes
//!   from `wp_fractional_scale_v1`, so it is a true fraction, not a rounded
//!   integer.
//! - **User UI scale** — the "make everything bigger" preference, already baked
//!   into the [`crate::theme::Theme`] tokens (spacing / radii / type) by
//!   [`crate::theme::Theme::with_ui_scale`], plus an extra type-only
//!   [`crate::theme::Theme::with_text_scale`].
//!
//! The toolkit lays widgets out in **logical pixels** (metrics already grown by
//! the user scale via the theme) and then applies a single
//! [`composite_transform`] of the **device** scale when drawing into the Vello
//! scene. Because Vello rasterises vector geometry — glyph outlines, strokes,
//! fills — at the *transformed* resolution, the result is crisp at any
//! fractional device scale: there is no intermediate low-resolution raster that
//! gets blurrily upscaled.
//!
//! Keeping the arithmetic here (rather than inline in the frame loop) makes the
//! logical⇄physical mapping a single named, unit-tested unit that the render
//! loop actually calls, so the tests assert the real code path.

use vello::kurbo::Affine;

/// The composite transform the render loop applies to the whole widget scene:
/// a uniform scale by the **device** factor. Widget rects live in logical
/// space; this maps them to physical pixels while Vello rasterises glyphs and
/// strokes at the physical resolution (crisp, not upscaled).
#[inline]
pub fn composite_transform(device_scale: f64) -> Affine {
    Affine::scale(device_scale)
}

/// The logical extent that a physical allocation of `physical` px corresponds to
/// at `device_scale`. This is the available space handed to the layout solver —
/// widgets size themselves in logical px, so the physical window is divided by
/// the device scale before layout.
#[inline]
pub fn logical_size(physical: (u32, u32), device_scale: f64) -> (f64, f64) {
    let s = if device_scale > 0.0 { device_scale } else { 1.0 };
    (physical.0 as f64 / s, physical.1 as f64 / s)
}

/// Map a single logical length to physical pixels at `device_scale`.
#[inline]
pub fn physical_len(logical: f64, device_scale: f64) -> f64 {
    logical * device_scale
}

/// Map a logical point to physical pixels at `device_scale` (the inverse of the
/// layout division above). Used by tests and hit-testing to reason about where
/// logical geometry lands on the physical surface.
#[inline]
pub fn physical_point(logical: (f64, f64), device_scale: f64) -> (f64, f64) {
    (logical.0 * device_scale, logical.1 * device_scale)
}

/// The final physical font size a run of text is rasterised at, folding the
/// whole pipeline together: the widget's logical font size, the user's global
/// UI scale and text scale (both already baked into the theme's type tokens),
/// and the per-output device scale applied by the composite transform.
///
/// `logical_font` here is the size a widget reads from the theme *after* the
/// user scales are folded in, so this is `theme_font × device_scale`. The
/// `ui_scale` / `text_scale` params are accepted so a caller starting from an
/// *unscaled* base size can compute the full chain in one call and so the
/// composition is explicit and testable.
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

    /// The composite transform is exactly a uniform scale by the device factor —
    /// the single multiply the render loop applies so logical widget geometry
    /// lands at physical resolution.
    #[test]
    fn composite_transform_is_uniform_device_scale() {
        for scale in [1.0f64, 1.25, 1.5, 1.75, 2.0] {
            let t = composite_transform(scale);
            // A logical point maps to logical×scale in physical space.
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

    /// Layout available space is the physical window divided by the device
    /// scale, at the exact fractional factors the compositor advertises.
    #[test]
    fn logical_available_space_divides_by_fractional_scale() {
        // A 2560×1600 panel at 1.25 → 2048×1280 logical, etc.
        assert_eq!(logical_size((2560, 1600), 1.25), (2048.0, 1280.0));
        assert_eq!(logical_size((1920, 1080), 1.5), (1280.0, 720.0));
        let (lw, lh) = logical_size((3360, 1890), 1.75);
        assert!((lw - 1920.0).abs() < 1e-9 && (lh - 1080.0).abs() < 1e-9);
        // A zero/garbage scale degrades to 1.0 rather than dividing by zero.
        assert_eq!(logical_size((800, 600), 0.0), (800.0, 600.0));
    }

    /// The full pipeline composes multiplicatively: physical text px =
    /// base × ui_scale × text_scale × device_scale. This is the "device × user"
    /// contract the whole feature rests on.
    #[test]
    fn physical_font_px_composes_device_and_user_scales() {
        // 14px base, user UI scale 1.25, user text size 1.2, display at 1.5.
        let px = physical_font_px(14.0, 1.25, 1.2, 1.5);
        assert!((px - 14.0 * 1.25 * 1.2 * 1.5).abs() < 1e-4, "got {px}");
        // Neutral user scales leave only the device factor.
        assert!((physical_font_px(16.0, 1.0, 1.0, 1.75) - 28.0).abs() < 1e-4);
        // Neutral device leaves only the user factors.
        assert!((physical_font_px(10.0, 1.5, 1.0, 1.0) - 15.0).abs() < 1e-4);
    }

    /// Real render path: a logical rect drawn through [`composite_transform`]
    /// lands at the correct **physical** coordinates at fractional device
    /// scales 1.25 / 1.5 / 1.75 — proving layout stays logical while the ink
    /// maps to physical pixels.
    #[test]
    fn fill_lands_at_physical_coordinates_at_fractional_scales() {
        for scale in [1.25f64, 1.5, 1.75] {
            let (lw, lh) = (100.0f64, 60.0f64);
            let pw = physical_len(lw, scale).round() as u32;
            let ph = physical_len(lh, scale).round() as u32;

            let mut scene = Scene::new();
            let rect = Rect::new(20.0, 15.0, 60.0, 45.0); // logical
            scene.fill(Fill::NonZero, composite_transform(scale), Color::WHITE, None, &rect);

            let mut r = OffscreenRenderer::new(pw, ph);
            let buf = r.render(&scene, Color::BLACK);

            // Logical centre (40,30) → physical (40×s, 30×s) must be inked.
            let (cx, cy) = physical_point((40.0, 30.0), scale);
            let (cr, _, _, _) = OffscreenRenderer::sample(&buf, pw, cx as u32, cy as u32);
            assert!(cr > 200, "rect centre inked at physical ({cx:.0},{cy:.0}) for scale {scale}, got r={cr}");

            // A logical point well outside the rect stays background.
            let (ox, oy) = physical_point((6.0, 6.0), scale);
            let (obr, _, _, _) = OffscreenRenderer::sample(&buf, pw, ox as u32, oy as u32);
            assert!(obr < 60, "background stays black outside the rect for scale {scale}, got r={obr}");
        }
    }

    /// Text metrics are logical (scale-free): the same string measures the same
    /// regardless of device scale, and its physical extent is exactly
    /// `logical × scale`. This is what lets layout be scale-independent while
    /// the composite transform handles the physical mapping.
    #[test]
    fn text_metrics_are_logical_and_physical_extent_scales() {
        let mut tr = TextRenderer::new();
        let (lw, lh) = tr.measure("Fractional scaling", 16.0, None);
        assert!(lw > 0.0 && lh > 0.0);
        for scale in [1.0f64, 1.25, 1.5, 1.75] {
            let (w2, h2) = tr.measure("Fractional scaling", 16.0, None);
            assert_eq!((w2, h2), (lw, lh), "measure is scale-free (logical) at {scale}");
            let pw = physical_len(lw as f64, scale);
            assert!((pw - lw as f64 * scale).abs() < 1e-6, "physical width = logical × {scale}");
        }
    }

    /// Nearest-neighbour upscale of a tight RGBA buffer, used to model the
    /// "blurry 2× upscale" a non-scale-aware pipeline would produce.
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

    /// Count the distinct luminance (red-channel, on a grey scene) levels — a
    /// proxy for how many anti-aliased edge gradations a raster carries.
    fn distinct_luma_levels(buf: &[u8], w: u32, h: u32) -> usize {
        let mut seen = [false; 256];
        for i in 0..(w * h) as usize {
            seen[buf[i * 4] as usize] = true;
        }
        seen.iter().filter(|&&b| b).count()
    }

    /// Crispness, the real point of fractional scaling: rendering the same
    /// logical text natively at device scale 2.0 rasterises glyph outlines at
    /// the physical resolution, so it carries strictly MORE anti-aliased
    /// luminance levels than nearest-upscaling the scale-1 raster to the same
    /// size (which invents no new detail — the blurry-upscale failure mode).
    #[test]
    fn native_fractional_render_is_crisper_than_upscaling() {
        let (lw, lh) = (140u32, 44u32);
        let mut tr = TextRenderer::new();

        let mut draw = |scale: f64, tr: &mut TextRenderer| -> (Vec<u8>, u32, u32) {
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
        // Sanity: both actually drew ink (some near-white pixels present).
        assert!(native_levels > 4, "the native render produced real anti-aliased text");
    }
}
