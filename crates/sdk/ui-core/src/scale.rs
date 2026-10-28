//! Display-scale plumbing shared by the render loop.
//!
//! Tilo separates **two** scale factors, multiplied at composite time:
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

/// The pixels a window's frame is drawn in, and the scale that draws its
/// logical layout into them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderExtent {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

/// A window of `physical` pixels at `device_scale`, drawn into a texture
/// no edge of which may exceed `limit` (the device's
/// `max_texture_dimension_2d`).
///
/// A window that fits is drawn as it is. A larger one (a kiosk stretched
/// over several displays: 2944 + 1920 logical pixels at scale 2 is 9728
/// wide, past the 8192 every device here is opened with) keeps its logical
/// size and is drawn at the lower scale that fits; the compositor stretches
/// the smaller buffer over the window, so it is softer, never missing or
/// failing. Layout divides the extent by its scale and lands on the same
/// logical size, within a pixel.
#[must_use]
pub fn fit_to_texture_limit(physical: (u32, u32), device_scale: f64, limit: u32) -> RenderExtent {
    let (width, height) = (physical.0.max(1), physical.1.max(1));
    let scale = if device_scale.is_finite() && device_scale > 0.0 {
        device_scale
    } else {
        1.0
    };
    let limit = limit.max(1);
    if width <= limit && height <= limit {
        return RenderExtent {
            width,
            height,
            scale,
        };
    }
    let factor = (f64::from(limit) / f64::from(width)).min(f64::from(limit) / f64::from(height));
    // Without fractional scaling a Wayland window keeps its integer buffer
    // scale, and a buffer whose size it does not divide is a protocol error.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "an integral scale of at least two, far below u32::MAX"
    )]
    let step = if scale.fract() == 0.0 && scale >= 2.0 && scale <= f64::from(limit) {
        scale as u32
    } else {
        1
    };
    let fitted = |edge: u32| -> u32 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a positive edge times a factor below one, clamped to the u32 limit"
        )]
        let scaled = (f64::from(edge) * factor).round() as u32;
        let scaled = scaled.clamp(1, limit);
        (scaled - scaled % step).max(step)
    };
    RenderExtent {
        width: fitted(width),
        height: fitted(height),
        scale: scale * factor,
    }
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

/// The part of the window that system chrome drawn over it (status and
/// navigation bars, a display cutout, a soft keyboard) leaves visible, as
/// logical-pixel insets from each edge. Zero wherever the platform keeps its
/// chrome outside the window.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SafeArea {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl SafeArea {
    /// From `[left, top, right, bottom]` physical-pixel insets.
    pub fn from_physical(insets: [i32; 4], device_scale: f64) -> Self {
        let s = if device_scale > 0.0 {
            device_scale
        } else {
            1.0
        };
        let [left, top, right, bottom] = insets.map(|inset| f64::from(inset.max(0)) / s);
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Where content goes in a `viewport` of logical size: its origin and
    /// size. Insets wider than the viewport leave an empty box, not a negative
    /// one.
    pub fn content(&self, viewport: (f64, f64)) -> (vello::kurbo::Point, (f64, f64)) {
        let left = self.left.min(viewport.0);
        let top = self.top.min(viewport.1);
        (
            vello::kurbo::Point::new(left, top),
            (
                (viewport.0 - left - self.right).max(0.0),
                (viewport.1 - top - self.bottom).max(0.0),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offscreen::OffscreenRenderer;
    use crate::text_render::TextRenderer;
    use vello::Scene;
    use vello::kurbo::Rect;
    use vello::peniko::{Color, Fill};

    /// A login kiosk stretched over a 2944x1840 panel and a 1920x1080
    /// monitor side by side (4864x1840 logical) at scale 2 asked for a
    /// 9728-wide texture on a device that allows 8192, and the window exited
    /// instead of drawing.
    #[test]
    fn a_window_past_the_texture_limit_draws_at_the_scale_that_fits() {
        let limit = 8192;
        let extent = fit_to_texture_limit((9728, 3680), 2.0, limit);
        assert!(
            extent.width <= limit && extent.height <= limit,
            "{extent:?} exceeds {limit}"
        );
        assert_eq!(extent.width, limit, "the long edge uses the whole texture");
        let (logical_w, logical_h) = logical_size((extent.width, extent.height), extent.scale);
        assert!(
            (logical_w - 4864.0).abs() <= 1.0 && (logical_h - 1840.0).abs() <= 1.0,
            "the layout keeps the window's logical size: {logical_w}x{logical_h}"
        );
        assert!(extent.scale < 2.0 && extent.scale > 1.6, "{extent:?}");
        // The window keeps buffer scale 2 where the compositor has no
        // fractional scaling; the buffer must stay divisible by it.
        assert_eq!((extent.width % 2, extent.height % 2), (0, 0), "{extent:?}");

        let tall = fit_to_texture_limit((2000, 20000), 1.0, limit);
        assert_eq!(tall.height, limit);
        assert!(tall.width <= limit && tall.width >= 800);

        assert_eq!(
            fit_to_texture_limit((5888, 3680), 2.0, limit),
            RenderExtent {
                width: 5888,
                height: 3680,
                scale: 2.0
            }
        );
        // Degenerate input never yields an empty texture or a zero scale.
        let empty = fit_to_texture_limit((0, 0), 0.0, limit);
        assert_eq!((empty.width, empty.height), (1, 1));
        assert!(empty.scale > 0.0);
        let sliver = fit_to_texture_limit((1_000_000, 1), 1.0, limit);
        assert_eq!((sliver.width, sliver.height), (limit, 1));
    }

    #[test]
    fn scale_factors_compose_device_and_user_scale_uniformly() {
        {
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

        {
            assert_eq!(logical_size((2560, 1600), 1.25), (2048.0, 1280.0));
            assert_eq!(logical_size((1920, 1080), 1.5), (1280.0, 720.0));
            let (lw, lh) = logical_size((3360, 1890), 1.75);
            assert!((lw - 1920.0).abs() < 1e-9 && (lh - 1080.0).abs() < 1e-9);
            // A zero/garbage scale degrades to 1.0 rather than dividing by zero.
            assert_eq!(logical_size((800, 600), 0.0), (800.0, 600.0));
        }

        {
            let px = physical_font_px(14.0, 1.25, 1.2, 1.5);
            assert!((px - 14.0 * 1.25 * 1.2 * 1.5).abs() < 1e-4, "got {px}");
            // Neutral user scales leave only the device factor.
            assert!((physical_font_px(16.0, 1.0, 1.0, 1.75) - 28.0).abs() < 1e-4);
            // Neutral device leaves only the user factors.
            assert!((physical_font_px(10.0, 1.5, 1.0, 1.0) - 15.0).abs() < 1e-4);
        }

        {
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
                let pw = physical_len(f64::from(lw), scale);
                assert!(
                    (pw - f64::from(lw) * scale).abs() < 1e-6,
                    "physical width = logical × {scale}"
                );
            }
        }
    }

    #[test]
    fn safe_area_insets_the_content_box_in_logical_pixels() {
        // A phone in portrait: status bar on top, gesture bar at the bottom.
        let area = SafeArea::from_physical([0, 128, 0, 63], 2.625);
        assert!((area.top - 128.0 / 2.625).abs() < 1e-9);
        let (origin, size) = area.content((411.0, 914.0));
        assert_eq!(origin.x, 0.0);
        assert!((origin.y - area.top).abs() < 1e-9);
        assert!((size.0 - 411.0).abs() < 1e-9);
        assert!((size.1 - (914.0 - area.top - area.bottom)).abs() < 1e-9);

        let (origin, size) = SafeArea::from_physical([128, 0, 0, 63], 2.0).content((1000.0, 400.0));
        assert_eq!((origin.x, origin.y), (64.0, 0.0));
        assert_eq!(size, (936.0, 368.5));

        // No chrome over the window: content is the whole viewport.
        assert_eq!(
            SafeArea::default().content((800.0, 600.0)),
            (vello::kurbo::Point::ORIGIN, (800.0, 600.0))
        );
        // Negative reports and a zero scale are clamped, and insets larger
        // than the viewport never produce a negative box.
        let odd = SafeArea::from_physical([-4, 900, 0, 900], 0.0);
        assert_eq!(odd.left, 0.0);
        let (origin, size) = odd.content((300.0, 500.0));
        assert_eq!(origin.y, 500.0);
        assert_eq!(size, (300.0, 0.0));
    }

    #[test]
    fn fractional_scale_renders_natively_at_physical_coordinates() {
        {
            for scale in [1.25f64, 1.5, 1.75] {
                let (lw, lh) = (100.0f64, 60.0f64);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "lw/lh are small positive test dimensions and scale is a positive \
                          test factor, so the physical length stays well within u32 range"
                )]
                let pw = physical_len(lw, scale).round() as u32;
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "lw/lh are small positive test dimensions and scale is a positive \
                          test factor, so the physical length stays well within u32 range"
                )]
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
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "cx/cy are physical_point() of positive logical coordinates at a \
                          positive scale, so they are always non-negative"
                )]
                let (cr, _, _, _) = OffscreenRenderer::sample(&buf, pw, cx as u32, cy as u32);
                assert!(
                    cr > 200,
                    "rect centre inked at physical ({cx:.0},{cy:.0}) for scale {scale}, got r={cr}"
                );

                let (ox, oy) = physical_point((6.0, 6.0), scale);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "ox/oy are physical_point() of positive logical coordinates at a \
                          positive scale, so they are always non-negative"
                )]
                let (obr, _, _, _) = OffscreenRenderer::sample(&buf, pw, ox as u32, oy as u32);
                assert!(
                    obr < 60,
                    "background stays black outside the rect for scale {scale}, got r={obr}"
                );
            }
        }

        // Case `native_fractional_render_is_crisper_than_upscaling`:
        // Native render at device scale 2.0 must carry strictly MORE
        // anti-aliased luminance levels than nearest-upscaling the scale-1
        // raster to the same size (which invents no new detail).
        {
            let (lw, lh) = (140u32, 44u32);
            let mut tr = TextRenderer::new();

            let draw = |scale: f64, tr: &mut TextRenderer| -> (Vec<u8>, u32, u32) {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "lw/lh are small positive test dimensions and scale is a positive \
                          test factor, so the physical size stays well within u32 range"
                )]
                let pw = (f64::from(lw) * scale).round() as u32;
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "lw/lh are small positive test dimensions and scale is a positive \
                          test factor, so the physical size stays well within u32 range"
                )]
                let ph = (f64::from(lh) * scale).round() as u32;
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

    /// Models the "blurry 2× upscale" a non-scale-aware pipeline would produce.
    fn nearest_upscale(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
        let mut out = vec![0u8; dw.saturating_mul(dh).saturating_mul(4) as usize];
        for y in 0..dh {
            let sy = (y.saturating_mul(sh).checked_div(dh).unwrap_or(0)).min(sh.saturating_sub(1));
            for x in 0..dw {
                let sx =
                    (x.saturating_mul(sw).checked_div(dw).unwrap_or(0)).min(sw.saturating_sub(1));
                let si = sy.saturating_mul(sw).saturating_add(sx).saturating_mul(4) as usize;
                let di = y.saturating_mul(dw).saturating_add(x).saturating_mul(4) as usize;
                out[di..di.saturating_add(4)].copy_from_slice(&src[si..si.saturating_add(4)]);
            }
        }
        out
    }

    /// Distinct red-channel levels on a grey scene — a proxy for how many
    /// anti-aliased edge gradations a raster carries.
    fn distinct_luma_levels(buf: &[u8], w: u32, h: u32) -> usize {
        let mut seen = [false; 256];
        for i in 0..w.saturating_mul(h) as usize {
            seen[buf[i.saturating_mul(4)] as usize] = true;
        }
        seen.iter().filter(|&&b| b).count()
    }
}
