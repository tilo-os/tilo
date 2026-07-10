//! Shared surface-drawing primitives — the reusable "how a card/panel
//! sits above the desktop" layer.
//!
//! The centrepiece is [`soft_shadow`]: a stretched radial-Gaussian
//! sprite that produces a genuinely soft drop shadow. Vello has no blur
//! primitive, so the historical fallback everywhere was a stack of
//! concentric rounded rects — which visibly stair-steps. The toolkit
//! solved this once inside `tile_host` but kept it private; promoting it
//! here lets the shell chrome (notifications, the wheel disc glow, the
//! top bar) drop their own stair-stepping approximations and share one
//! correct implementation.
//!
//! Sprites are baked once per tint and cached, so repeated shadows of
//! the same colour cost only an image stretch.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{
    Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as Image,
    ImageFormat as Format,
};

use crate::tokens::Shadow;

/// Baked sprite resolution. 256² is plenty — the sprite is always
/// stretched over a larger target, and the Gaussian is smooth so the
/// upscale stays soft.
const SHADOW_SPRITE_SIZE: u32 = 256;
/// Gaussian width of the falloff ring. Larger = softer, more spread-out.
const SHADOW_GAUSSIAN_K: f64 = 0.35;
/// Normalised radius (0..1 of the half-diagonal) where the shadow is
/// darkest. The centre is forced transparent so a card's own fill paints
/// over clear sprite, not a pre-darkened spot.
const SHADOW_PEAK_RADIUS: f64 = 0.30;

/// Bake a premultiplied-RGBA radial-Gaussian sprite tinted `(r,g,b)`.
///
/// The alpha envelope is `bump * inner_shoulder * outer_shoulder`:
/// - `bump` — the Gaussian ring peaking at [`SHADOW_PEAK_RADIUS`].
/// - `inner_shoulder` — clamps the centre to zero.
/// - `outer_shoulder` — clamps the sprite boundary to zero so the
///   stretched image can't leak a square-edged halo.
fn build_shadow_sprite(r: u8, g: u8, b: u8) -> Image {
    let size = SHADOW_SPRITE_SIZE;
    let s = size as f64;
    let cx = (s - 1.0) * 0.5;
    let cy = (s - 1.0) * 0.5;
    let max_r = cx.max(cy);
    let k = SHADOW_GAUSSIAN_K;
    let d_peak = SHADOW_PEAK_RADIUS;
    let mut data = Vec::<u8>::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = (x as f64 - cx) / max_r;
            let dy = (y as f64 - cy) / max_r;
            let d = (dx * dx + dy * dy).sqrt();
            let bump = (-((d - d_peak) / k).powi(2)).exp();
            let inner = (d / d_peak).min(1.0);
            let outer = (1.0 - d).max(0.0);
            let a = (bump * inner * outer).clamp(0.0, 1.0);
            let a8 = (a * 255.0).round() as u8;
            data.push((r as f64 * a).round() as u8);
            data.push((g as f64 * a).round() as u8);
            data.push((b as f64 * a).round() as u8);
            data.push(a8);
        }
    }
    let blob = Blob::new(Arc::new(data));
    Image {
        data: blob,
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: size,
        height: size,
    }
}

/// Per-tint sprite cache. Keyed by opaque `(r,g,b)`; the alpha is applied
/// at draw time via a layer, so one sprite serves every intensity.
fn sprite_for(r: u8, g: u8, b: u8) -> Arc<Image> {
    static CACHE: OnceLock<Mutex<HashMap<(u8, u8, u8), Arc<Image>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
    map.entry((r, g, b))
        .or_insert_with(|| Arc::new(build_shadow_sprite(r, g, b)))
        .clone()
}

/// Paint a soft drop shadow for `rect`.
///
/// - `color` is the shadow tint (its own alpha is ignored — pass the
///   opaque hue; use `intensity` for strength).
/// - `spread` extends the soft falloff this many logical px past every
///   edge of `rect`.
/// - `offset_y` shifts the shadow downward (ambient light from above).
/// - `intensity` in `0.0..=1.0` scales the whole shadow's opacity.
///
/// No-op for a degenerate rect or `intensity <= 0`.
pub fn soft_shadow(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    color: Color,
    spread: f64,
    offset_y: f64,
    intensity: f32,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || intensity <= 0.0 {
        return;
    }
    let target = Rect::new(
        rect.x0 - spread,
        rect.y0 - spread + offset_y,
        rect.x1 + spread,
        rect.y1 + spread + offset_y,
    );
    let sprite = sprite_for(color.to_rgba8().r, color.to_rgba8().g, color.to_rgba8().b);
    let sx = target.width() / sprite.width as f64;
    let sy = target.height() / sprite.height as f64;
    let brush_transform = transform
        * Affine::translate((target.x0, target.y0))
        * Affine::scale_non_uniform(sx, sy);
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        intensity.clamp(0.0, 1.0),
        transform,
        &target,
    );
    let sprite_brush = ImageBrush::new((*sprite).clone());
    scene.fill(
        Fill::NonZero,
        brush_transform,
        &sprite_brush,
        None,
        &Rect::new(0.0, 0.0, sprite.width as f64, sprite.height as f64),
    );
    scene.pop_layer();
}

/// Paint an elevation-token shadow under `rect`. Maps a [`Shadow`] token
/// (offset_y + blur + tinted-alpha color) onto [`soft_shadow`], so the
/// whole UI can express depth as `surface::elevation(.., &theme.tokens.elevation.e3)`
/// instead of hand-tuning spread/alpha at each call site.
pub fn elevation(scene: &mut Scene, transform: Affine, rect: Rect, shadow: &Shadow) {
    if shadow.blur <= 0.0 {
        return;
    }
    let rgba = shadow.color.to_rgba8();
    let opaque = Color::from_rgba8(rgba.r, rgba.g, rgba.b, 255);
    // The token `blur` is a CSS-style blur radius; the sprite needs a bit
    // more spread to render the same visual softness, plus a small floor
    // so tight shadows still read.
    let spread = (shadow.blur as f64) * 1.6 + 6.0;
    let intensity = rgba.a as f32 / 255.0;
    soft_shadow(
        scene,
        transform,
        rect,
        opaque,
        spread,
        shadow.offset_y as f64,
        intensity,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_is_cached_per_tint() {
        let a = sprite_for(0, 0, 0);
        let b = sprite_for(0, 0, 0);
        assert!(Arc::ptr_eq(&a, &b), "same tint must return the cached sprite");
        let c = sprite_for(99, 102, 241);
        assert!(!Arc::ptr_eq(&a, &c), "different tint must bake a new sprite");
    }

    #[test]
    fn sprite_centre_is_transparent_edges_are_zero() {
        let img = build_shadow_sprite(0, 0, 0);
        let read_a = |x: u32, y: u32| -> u8 {
            let idx = ((y * img.width + x) * 4 + 3) as usize;
            img.data.data()[idx]
        };
        let mid = img.width / 2;
        // The sprite is even-sized so there's no exact centre pixel; the
        // near-centre sample rounds to <=2/255. Corner is a true zero.
        assert!(read_a(mid, mid) <= 2, "centre alpha must be ~0");
        assert_eq!(read_a(0, 0), 0, "corner alpha must be 0");
        // A point on the peak ring should be non-zero.
        let peak_x = (mid as f64 + SHADOW_PEAK_RADIUS * (mid as f64)) as u32;
        assert!(read_a(peak_x, mid) > 0, "peak ring alpha must be > 0");
    }

    #[test]
    fn degenerate_inputs_are_noops() {
        let mut scene = Scene::new();
        // Zero-area rect, zero intensity — must not panic and must add
        // nothing detectable (we just assert it returns).
        soft_shadow(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 0.0, 0.0),
            Color::BLACK,
            10.0,
            0.0,
            0.5,
        );
        soft_shadow(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Color::BLACK,
            10.0,
            0.0,
            0.0,
        );
    }
}
