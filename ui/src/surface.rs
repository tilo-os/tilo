//! Shared surface-drawing primitives — how a card/panel casts a shadow.
//!
//! [`shadow`] is the default: a true Gaussian blur of the surface's own
//! rounded outline, so the shadow is the same shape as the thing casting
//! it and equally dense the whole way round.
//!
//! [`soft_shadow`] is a baked-sprite falloff 9-sliced around the rect,
//! kept only for effects with no outline to follow (the wheel disc's
//! bloom, a dragged card's accent glow) — anything with a corner radius
//! should use [`shadow`] instead, since the sprite's corner term is a
//! square-corner SDF that reads as a box with the corners rubbed out.
//! Sprites are baked once per tint and cached.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{
    Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as Image, ImageFormat as Format,
};

use crate::theme::Theme;
use crate::tokens::Shadow;

/// Resolution of the baked falloff along the profile axis: 256 samples
/// across a band at most a couple hundred logical px wide is plenty.
const SHADOW_SPRITE_SIZE: u32 = 256;
/// Row count of the edge sprite: every row is an identical profile, but
/// keeping a few (not one) leaves the image non-degenerate for the
/// sampler, at a cost of 4 KB.
const SHADOW_EDGE_ROWS: u32 = 4;
/// Gaussian width of the falloff ring. Larger = softer, more spread-out.
const SHADOW_GAUSSIAN_K: f64 = 0.35;
/// Where the envelope peaks, as a fraction of the band it is baked
/// across (the band runs one `spread` inside the rect's edge to one
/// `spread` outside). `0.30` puts the peak `(1 - 2·0.30) = 0.4` of a
/// spread under the edge and leaves the interior fully transparent, so
/// translucent surfaces don't self-darken under their own fill.
const SHADOW_PEAK_RADIUS: f64 = 0.30;

/// The shadow's alpha as a function of the normalised signed distance
/// `u` from the rect's edge: `-1` is one band inside the rect, `0` is
/// exactly on the edge, `+1` is one `spread` outside.
///
/// The envelope is `bump · inner_shoulder · outer_shoulder`:
/// - `bump` — the Gaussian ring peaking at [`SHADOW_PEAK_RADIUS`].
/// - `inner_shoulder` — clamps the inner end of the band to zero.
/// - `outer_shoulder` — clamps the outer end to zero so no slice can
///   leak a hard-edged halo at the shadow's boundary.
fn shadow_profile(u: f64) -> f64 {
    let d = (u + 1.0) * 0.5;
    if d <= 0.0 || d >= 1.0 {
        return 0.0;
    }
    let bump = (-((d - SHADOW_PEAK_RADIUS) / SHADOW_GAUSSIAN_K).powi(2)).exp();
    let inner = (d / SHADOW_PEAK_RADIUS).min(1.0);
    let outer = 1.0 - d;
    (bump * inner * outer).clamp(0.0, 1.0)
}

/// Signed distance to the rect, in band units, at a corner — the
/// rounded-box SDF with a zero corner radius. `ux`/`uy` are each already
/// normalised to their own band (`+1` one spread outside, `-1` at the
/// inner end). Outside both edges this is `hypot`, so the iso-alpha
/// contours are circles about the corner; once one coordinate goes
/// inside, the `max` term collapses the field to a function of the
/// other axis only — exactly what the neighbouring edge slice paints,
/// so the two meet without a discontinuity.
fn corner_u(ux: f64, uy: f64) -> f64 {
    let ox = ux.max(0.0);
    let oy = uy.max(0.0);
    (ox * ox + oy * oy).sqrt() + ux.max(uy).min(0.0)
}

/// Map a texel index onto its band coordinate in `-1..=1`, sampling at
/// pixel centres so the brush's half-texel offset doesn't skew the
/// baked profile against the geometry it is stretched over.
fn band_coord(i: u32, n: u32) -> f64 {
    2.0 * (f64::from(i) + 0.5) / f64::from(n) - 1.0
}

/// The pair of premultiplied-RGBA sprites one tint needs.
///
/// The channels are written premultiplied (`c * a`), so both [`Image`]s
/// must declare [`ImageAlphaType::AlphaPremultiplied`]. Declaring
/// straight alpha instead makes the renderer premultiply a second time,
/// which squares the tint and roughly halves every shadow in the
/// toolkit — the peak of a full-strength white shadow lands at
/// `255 · a²` instead of `255 · a`. `soft_shadow_peak_matches_sprite_
/// envelope` pins the composited strength so that cannot come back.
struct ShadowSprites {
    /// `SHADOW_SPRITE_SIZE × SHADOW_EDGE_ROWS`: the 1-D profile along x
    /// (inner end at x = 0), constant down every column. Stretched along
    /// one axis only, so an edge's falloff width never depends on that
    /// edge's length.
    edge: Image,
    /// `SHADOW_SPRITE_SIZE²`: the corner quadrant, oriented bottom-right
    /// (inner end at the origin), mirrored into the other three. Never
    /// scaled by anything but the band, so corners stay `spread × spread`
    /// and circular on any aspect ratio.
    corner: Image,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "alpha is clamped to 0.0..=1.0 and channel values are u8 (0..=255), so \
              each product rounds to an integer already within 0..=255"
)]
fn bake(
    red: u8,
    green: u8,
    blue: u8,
    width: u32,
    height: u32,
    profile: impl Fn(u32, u32) -> f64,
) -> Image {
    let mut data =
        Vec::<u8>::with_capacity(width.saturating_mul(height).saturating_mul(4) as usize);
    for row in 0..height {
        for col in 0..width {
            let alpha = profile(col, row).clamp(0.0, 1.0);
            data.push((f64::from(red) * alpha).round() as u8);
            data.push((f64::from(green) * alpha).round() as u8);
            data.push((f64::from(blue) * alpha).round() as u8);
            data.push((alpha * 255.0).round() as u8);
        }
    }
    Image {
        data: Blob::new(Arc::new(data)),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width,
        height,
    }
}

fn build_shadow_sprites(r: u8, g: u8, b: u8) -> ShadowSprites {
    let n = SHADOW_SPRITE_SIZE;
    ShadowSprites {
        edge: bake(r, g, b, n, SHADOW_EDGE_ROWS, |x, _| {
            shadow_profile(band_coord(x, n))
        }),
        corner: bake(r, g, b, n, n, |x, y| {
            shadow_profile(corner_u(band_coord(x, n), band_coord(y, n)))
        }),
    }
}

/// Per-tint sprite cache. Keyed by opaque `(r,g,b)`; the intensity is
/// applied to the brush at draw time, so one pair serves every intensity.
type ShadowSpriteCache = Mutex<HashMap<(u8, u8, u8), Arc<ShadowSprites>>>;

fn sprites_for(r: u8, g: u8, b: u8) -> Arc<ShadowSprites> {
    static CACHE: OnceLock<ShadowSpriteCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // A panic elsewhere while the lock was held can't corrupt this map (a
    // plain sprite cache, not an invariant-bearing structure), so recover
    // the data instead of taking the whole render path down with it.
    let mut map = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry((r, g, b))
        .or_insert_with(|| Arc::new(build_shadow_sprites(r, g, b)))
        .clone()
}

/// Map a sprite's `sw × sh` pixel rect onto `dest`, optionally mirroring
/// either axis. The sprite's x axis stays the destination's x axis.
fn slice_xform(dest: Rect, sw: f64, sh: f64, mirror_x: bool, mirror_y: bool) -> Affine {
    let (a, e) = if mirror_x {
        (-dest.width() / sw, dest.x1)
    } else {
        (dest.width() / sw, dest.x0)
    };
    let (d, f) = if mirror_y {
        (-dest.height() / sh, dest.y1)
    } else {
        (dest.height() / sh, dest.y0)
    };
    Affine::new([a, 0.0, 0.0, d, e, f])
}

/// [`slice_xform`] with the axes swapped: the sprite's x axis (its
/// profile) maps onto the destination's *y* axis, and its rows stretch
/// along the destination's x. Used for the top and bottom edges, where
/// the same baked profile has to run vertically.
fn slice_xform_rot(dest: Rect, sw: f64, sh: f64, mirror_y: bool) -> Affine {
    let (b, f) = if mirror_y {
        (-dest.height() / sw, dest.y1)
    } else {
        (dest.height() / sw, dest.y0)
    };
    Affine::new([0.0, b, dest.width() / sh, 0.0, dest.x0, f])
}

/// Paint the part of a slice that falls inside `clip`.
///
/// `local` maps the sprite onto the slice's **full** band geometry —
/// always `spread` on each side of the edge — and `clip` narrows that to
/// what this slice owns. Keeping the two separate is what lets a rect
/// shorter than `2 · spread` share its inner band between opposing
/// slices without squashing the profile or double-painting.
///
/// The shape is derived in `local` space (axis-aligned by construction),
/// so the bbox is exact regardless of the caller's own `transform`.
fn blit(scene: &mut Scene, transform: Affine, local: Affine, brush: &ImageBrush, clip: Rect) {
    if clip.width() <= 0.0 || clip.height() <= 0.0 {
        return;
    }
    let shape = local.inverse().transform_rect_bbox(clip);
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine composition is matrix multiplication on f64 fields; it cannot panic or overflow"
    )]
    let combined = transform * local;
    scene.fill(Fill::NonZero, combined, brush, None, &shape);
}

/// Paint a soft drop shadow for `rect`.
///
/// - `color`'s own alpha is ignored — pass the opaque hue; use
///   `intensity` for strength.
/// - `spread` extends the soft falloff this many logical px past every
///   edge.
/// - `offset_y` shifts the shadow downward (ambient light from above).
/// - `intensity` in `0.0..=1.0` scales the whole shadow's opacity.
///
/// 9-sliced around the rect: corners get a fixed `spread × spread`
/// circular sprite, edges stretch the 1-D profile along their own
/// length. No-op for a degenerate rect, `spread <= 0`, or `intensity <= 0`.
pub fn soft_shadow(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    color: Color,
    spread: f64,
    offset_y: f64,
    intensity: f32,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || intensity <= 0.0 || spread <= 0.0 {
        return;
    }
    // Clip boundaries meet at the rect's mid-lines when narrower than
    // `2 · spread`, so opposing slices divide the interior, not overlap.
    let in_x = spread.min(rect.width() / 2.0);
    let in_y = spread.min(rect.height() / 2.0);
    let (ox0, ix0) = (rect.x0 - spread, rect.x0 + in_x);
    let (ix1, ox1) = (rect.x1 - in_x, rect.x1 + spread);
    let (oy0, iy0) = (rect.y0 - spread + offset_y, rect.y0 + in_y + offset_y);
    let (iy1, oy1) = (rect.y1 - in_y + offset_y, rect.y1 + spread + offset_y);
    // Full band extents, ignoring the clamp — what the sprites bake against.
    let (bx0, bx1) = (rect.x0 + spread, rect.x1 - spread);
    let (by0, by1) = (rect.y0 + spread + offset_y, rect.y1 - spread + offset_y);

    let rgba = color.to_rgba8();
    let sprites = sprites_for(rgba.r, rgba.g, rgba.b);
    let n = f64::from(SHADOW_SPRITE_SIZE);
    let rows = f64::from(SHADOW_EDGE_ROWS);

    // `intensity` rides on the brushes, not a group layer: the eight
    // slices are clipped to non-overlapping regions so they can't
    // double-darken, and a layer per shadow isn't free in vello.
    let alpha = intensity.clamp(0.0, 1.0);

    // Corners: 2·spread square centred on the corner, mirrored per quadrant.
    let corner = ImageBrush::new(sprites.corner.clone()).with_alpha(alpha);
    for (band, clip, mirror_x, mirror_y) in [
        (
            Rect::new(bx1, by1, ox1, oy1),
            Rect::new(ix1, iy1, ox1, oy1),
            false,
            false,
        ),
        (
            Rect::new(ox0, by1, bx0, oy1),
            Rect::new(ox0, iy1, ix0, oy1),
            true,
            false,
        ),
        (
            Rect::new(bx1, oy0, ox1, by0),
            Rect::new(ix1, oy0, ox1, iy0),
            false,
            true,
        ),
        (
            Rect::new(ox0, oy0, bx0, by0),
            Rect::new(ox0, oy0, ix0, iy0),
            true,
            true,
        ),
    ] {
        let local = slice_xform(band, n, n, mirror_x, mirror_y);
        blit(scene, transform, local, &corner, clip);
    }

    // Edges: centre slice skipped — the envelope is zero past one band in.
    let edge = ImageBrush::new(sprites.edge.clone()).with_alpha(alpha);
    for (band, clip, mirror_x) in [
        (
            Rect::new(bx1, iy0, ox1, iy1),
            Rect::new(ix1, iy0, ox1, iy1),
            false,
        ),
        (
            Rect::new(ox0, iy0, bx0, iy1),
            Rect::new(ox0, iy0, ix0, iy1),
            true,
        ),
    ] {
        let local = slice_xform(band, n, rows, mirror_x, false);
        blit(scene, transform, local, &edge, clip);
    }
    for (band, clip, mirror_y) in [
        (
            Rect::new(ix0, by1, ix1, oy1),
            Rect::new(ix0, iy1, ix1, oy1),
            false,
        ),
        (
            Rect::new(ix0, oy0, ix1, by0),
            Rect::new(ix0, oy0, ix1, iy0),
            true,
        ),
    ] {
        let local = slice_xform_rot(band, n, rows, mirror_y);
        blit(scene, transform, local, &edge, clip);
    }
}

/// Paint the drop shadow an elevation token describes, following `rect`'s
/// own corner `radius` — a true Gaussian blur of the surface's own
/// rounded outline, unlike [`soft_shadow`]'s square-corner-SDF
/// approximation.
///
/// The token's `color` alpha is the shadow's strength and is honoured
/// here; [`soft_shadow`] ignores it in favour of a separate `intensity`
/// argument. No-op for a degenerate rect or a token with no blur.
pub fn shadow(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, token: &Shadow) {
    if token.blur <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let alpha = token.color.components[3];
    if alpha <= 0.0 {
        return;
    }
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "translating a Rect by a Vec2 offset is vector addition on f64 fields; it cannot panic or overflow"
    )]
    let shadow_rect = rect + Vec2::new(0.0, f64::from(token.offset_y));
    scene.draw_blurred_rounded_rect(
        transform,
        shadow_rect,
        token.color,
        radius.clamp(0.0, rect.width().min(rect.height()) / 2.0),
        std_dev_for(token.blur),
    );
}

/// The Gaussian standard deviation a CSS-style `blur` radius means.
///
/// Halving is the conventional mapping, and vello cuts its kernel at
/// `2.5 * std_dev`, so a token's visible falloff ends about `1.25 * blur`
/// past the surface. One function so every surface agrees.
pub fn std_dev_for(blur: f32) -> f64 {
    f64::from(blur) / 2.0
}

/// Paint an elevation-token shadow under `rect`, square-cornered. For
/// callers with no radius to follow — anything rounded must call
/// [`shadow`] instead, or it gets a shadow of a different shape than itself.
pub fn elevation(scene: &mut Scene, transform: Affine, rect: Rect, token: &Shadow) {
    shadow(scene, transform, rect, 0.0, token);
}

/// The one contract for "is this small selectable chrome element active"
/// — scope pills, filter chips, tab pills, toggle cards.
pub fn chip_colors(theme: &Theme, selected: bool) -> (Color, Option<Color>) {
    let c = &theme.tokens.colors;
    if selected {
        (c.accent[5], None)
    } else {
        (c.surfaces.raised, Some(c.border_standard))
    }
}

/// Draw a pill / chip: a fully-rounded (or `radius`-rounded) filled rect
/// with an optional 1px border. Pass `radius = None` for a fully-rounded
/// capsule (`height/2`).
pub fn pill(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    fill: Color,
    border: Option<Color>,
    radius: Option<f64>,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let r = radius.unwrap_or_else(|| rect.height() / 2.0);
    let shape = RoundedRect::new(rect.x0, rect.y0, rect.x1, rect.y1, r);
    scene.fill(Fill::NonZero, transform, fill, None, &shape);
    if let Some(bc) = border {
        scene.stroke(&Stroke::new(1.0), transform, bc, None, &shape);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_colors_selected_is_solid_accent_no_border() {
        let theme = Theme::dark();
        let (fill, border) = chip_colors(&theme, true);
        assert_eq!(fill, theme.tokens.colors.accent[5]);
        assert!(border.is_none(), "selected chip has no border");
        let (fill_idle, border_idle) = chip_colors(&theme, false);
        assert_eq!(fill_idle, theme.tokens.colors.surfaces.raised);
        assert!(border_idle.is_some(), "idle chip has a border");
    }

    #[test]
    fn sprite_is_cached_per_tint() {
        let a = sprites_for(0, 0, 0);
        let b = sprites_for(0, 0, 0);
        assert!(
            Arc::ptr_eq(&a, &b),
            "same tint must return the cached sprite"
        );
        let c = sprites_for(99, 102, 241);
        assert!(
            !Arc::ptr_eq(&a, &c),
            "different tint must bake a new sprite"
        );
    }

    fn alpha_at(img: &Image, x: u32, y: u32) -> u8 {
        let idx = y
            .saturating_mul(img.width)
            .saturating_add(x)
            .saturating_mul(4)
            .saturating_add(3);
        img.data.data()[idx as usize]
    }

    /// Ends of the band are zero, and the peak sits where
    /// [`SHADOW_PEAK_RADIUS`] says.
    #[test]
    fn edge_sprite_runs_from_zero_through_the_peak_back_to_zero() {
        let s = build_shadow_sprites(0, 0, 0);
        let n = SHADOW_SPRITE_SIZE;
        assert!(
            alpha_at(&s.edge, 0, 0) <= 2,
            "inner end of the band must be ~0"
        );
        assert!(
            alpha_at(&s.edge, n - 1, 0) <= 2,
            "outer end of the band must be ~0"
        );
        // Peak column: SHADOW_PEAK_RADIUS along the band.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "SHADOW_PEAK_RADIUS is a 0.0..=1.0 fraction of the band, so the \
                      product with n stays within 0..n"
        )]
        let peak_x = (SHADOW_PEAK_RADIUS * f64::from(n)).round() as u32;
        let peak = alpha_at(&s.edge, peak_x, 0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "255.0 * (1.0 - SHADOW_PEAK_RADIUS) is within 0.0..=255.0"
        )]
        let expected = (255.0 * (1.0 - SHADOW_PEAK_RADIUS)).round() as i32;
        assert!(
            (i32::from(peak) - expected).abs() <= 2,
            "the baked peak must be {expected}/255; got {peak}"
        );
        // Every row must match — that's what lets one sprite stretch to
        // any edge length.
        for y in 1..SHADOW_EDGE_ROWS {
            for x in [0, peak_x, n / 2, n - 1] {
                assert_eq!(
                    alpha_at(&s.edge, x, y),
                    alpha_at(&s.edge, x, 0),
                    "edge sprite row {y} differs from row 0 at x={x}"
                );
            }
        }
    }

    /// The corner sprite must equal the edge profile along the two
    /// rows/columns where it meets an edge slice — the seam is seamless
    /// by construction, not luck.
    #[test]
    fn corner_sprite_joins_the_edge_profile_where_the_slices_meet() {
        let s = build_shadow_sprites(0, 0, 0);
        let n = SHADOW_SPRITE_SIZE;
        for x in [0, 1, n / 4, n / 2, (3 * n) / 4, n - 1] {
            // Innermost row of the corner = the edge profile in x.
            assert_eq!(
                alpha_at(&s.corner, x, 0),
                alpha_at(&s.edge, x, 0),
                "corner row 0 must equal the edge profile at x={x}"
            );
            // …and by symmetry the innermost column equals it in y.
            assert_eq!(
                alpha_at(&s.corner, 0, x),
                alpha_at(&s.edge, x, 0),
                "corner column 0 must equal the edge profile at y={x}"
            );
        }
        // The outer corner of the sprite is a full spread × spread away
        // diagonally — outside the circle — so it is a true zero.
        assert_eq!(alpha_at(&s.corner, n - 1, n - 1), 0);
    }

    /// The corner's iso-alpha contours are circles about the corner
    /// point, not the elliptical contours a stretched radial sprite
    /// would produce.
    #[test]
    fn corner_sprite_contours_are_circular() {
        let s = build_shadow_sprites(0, 0, 0);
        let n = f64::from(SHADOW_SPRITE_SIZE);
        // Walking a quarter circle of radius `r` (in band units) out
        // from the corner must hold the alpha constant.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0.0..=n - 1.0 immediately above, so it's always within u32 range"
        )]
        let px = |u: f64| ((u + 1.0) * 0.5 * n).round().clamp(0.0, n - 1.0) as u32;
        for r in [0.25_f64, 0.5, 0.75] {
            let straight = i32::from(alpha_at(&s.corner, px(r), px(0.0)));
            for deg in [15.0_f64, 30.0, 45.0, 60.0, 75.0] {
                let (sin, cos) = deg.to_radians().sin_cos();
                let got = i32::from(alpha_at(&s.corner, px(r * cos), px(r * sin)));
                assert!(
                    (got - straight).abs() <= 3,
                    "at radius {r} the corner alpha must not depend on the \
                     angle: {deg}° gave {got}, straight out gave {straight}"
                );
            }
        }
    }

    /// The red channel of the render *is* the composited alpha, so a
    /// sample reads the production output of [`shadow`] directly.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test sample coordinates are always non-negative pixel offsets within the rendered canvas"
    )]
    fn render_token_shadow(
        w: u32,
        h: u32,
        rect: Rect,
        radius: f64,
        token: &Shadow,
    ) -> impl Fn(f64, f64) -> u8 {
        use crate::offscreen::OffscreenRenderer;
        let mut scene = Scene::new();
        shadow(&mut scene, Affine::IDENTITY, rect, radius, token);
        let mut r = OffscreenRenderer::new(w, h);
        let rgba = r.render(&scene, Color::BLACK);
        move |x: f64, y: f64| {
            OffscreenRenderer::sample(&rgba, w, x.floor() as u32, y.floor() as u32).0
        }
    }

    /// A shadow is as even around a rounded corner as along a flat edge,
    /// since it's a blur of the surface's own outline. Sampling is at
    /// equal *normal* distance from the silhouette — out from the flat
    /// edge, and out along the corner arc's diagonal.
    #[test]
    fn token_shadow_is_even_around_a_rounded_corner() {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let radius = 24.0;
        // offset_y 0 isolates shape from the deliberate downward shift.
        let token = Shadow {
            offset_y: 0.0,
            blur: 16.0,
            color: Color::WHITE,
        };
        let at = render_token_shadow(400, 400, rect, radius, &token);

        // Corner arc centre, and the point where the arc meets the
        // outward diagonal.
        let cc = 250.0 - radius;
        let k = std::f64::consts::FRAC_1_SQRT_2;
        for d in [2.0_f64, 5.0, 9.0, 14.0] {
            let edge = at(250.0 + d, 200.0);
            let corner = at(cc + (radius + d) * k, cc + (radius + d) * k);
            let (lo, hi) = (f64::from(edge.min(corner)), f64::from(edge.max(corner)));
            assert!(
                hi - lo <= 14.0,
                "at {d} px out the shadow must be as dense at the rounded \
                 corner as along the flat edge: edge {edge}, corner \
                 {corner}. A corner far lighter than the edge is a shadow \
                 whose outline is not the surface's outline."
            );
        }
    }

    /// The token's colour alpha is the shadow's strength; unlike
    /// [`soft_shadow`], [`shadow`] actually reads it.
    #[test]
    fn token_shadow_honours_the_token_alpha() {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let radius = 24.0;
        let peak = |a: u8| {
            let token = Shadow {
                offset_y: 0.0,
                blur: 16.0,
                color: Color::from_rgba8(255, 255, 255, a),
            };
            let at = render_token_shadow(400, 400, rect, radius, &token);
            // Just outside the flat right edge, where coverage is ~half.
            at(251.0, 200.0)
        };
        let full = peak(255);
        let half = peak(128);
        assert!(
            full > 60,
            "a full-strength shadow must actually paint (got {full})"
        );
        let ratio = f64::from(half) / f64::from(full);
        assert!(
            (ratio - 0.5).abs() <= 0.08,
            "halving the token's alpha must halve the shadow: {full} -> \
             {half} (ratio {ratio:.3}). A ratio near 1.0 means the token's \
             alpha is being discarded."
        );
    }

    /// A token's visible reach is set by its `blur`, through the one
    /// shared mapping ([`std_dev_for`]).
    #[test]
    fn token_shadow_reach_follows_blur() {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let token = Shadow {
            offset_y: 0.0,
            blur: 16.0,
            color: Color::WHITE,
        };
        let at = render_token_shadow(400, 400, rect, 24.0, &token);
        let reach = std_dev_for(token.blur) * 2.5;
        let mut last = 0.0_f64;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "reach is 2.5 * std_dev_for(a non-negative blur), so reach * 2.0 is \
                      always non-negative and far below u32::MAX"
        )]
        let steps = (reach * 2.0) as u32;
        for d in 1..=steps {
            if at(250.0 + f64::from(d), 200.0) > 2 {
                last = f64::from(d);
            }
        }
        assert!(
            last <= reach + 2.0,
            "the shadow must die out within 2.5 standard deviations \
             ({reach:.1} px) of the edge; it was still painting at {last} px"
        );
        assert!(
            last >= reach * 0.5,
            "the shadow must actually reach most of its blur \
             ({reach:.1} px); it stopped at {last} px"
        );
    }

    /// The red channel of the render *is* the composited alpha, so the
    /// sampler reads the production output of [`soft_shadow`] in the
    /// same units [`shadow_profile`] declares.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test sample coordinates are always non-negative pixel offsets within the rendered canvas"
    )]
    fn render_shadow(w: u32, h: u32, rect: Rect, spread: f64) -> impl Fn(f64, f64) -> u8 {
        use crate::offscreen::OffscreenRenderer;
        let mut scene = Scene::new();
        soft_shadow(
            &mut scene,
            Affine::IDENTITY,
            rect,
            Color::WHITE,
            spread,
            0.0,
            1.0,
        );
        let mut r = OffscreenRenderer::new(w, h);
        let rgba = r.render(&scene, Color::BLACK);
        move |x: f64, y: f64| {
            OffscreenRenderer::sample(&rgba, w, x.floor() as u32, y.floor() as u32).0
        }
    }

    /// The composited shadow has to reach the strength the sprite's
    /// alpha envelope describes. A full-intensity white shadow peaks at
    /// `bump · inner · outer = 1 · 1 · (1 - SHADOW_PEAK_RADIUS) = 0.7`,
    /// so over black the brightest pixel on the ring is `255 · 0.7 ≈
    /// 178`. If the premultiplied sprite were handed to the renderer as
    /// straight alpha, the tint would get premultiplied twice and that
    /// peak would collapse to `255 · 0.7² ≈ 125` — this pins against that.
    ///
    /// The ring's position is asserted too, measured from the rect's
    /// **edge**: the envelope peaks `(1 - 2·SHADOW_PEAK_RADIUS)` of a
    /// `spread` inside the edge and dies exactly one `spread` outside it,
    /// whatever the rect's size.
    #[test]
    fn soft_shadow_peak_matches_sprite_envelope() {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let spread = 30.0;
        let at = render_shadow(400, 400, rect, spread);

        // Walk outward along the horizontal centre line and keep the
        // brightest sample plus where it was found.
        let (cx, cy) = (200.0, 200.0);
        let half = rect.width() / 2.0 + spread; // 80 px
        let (mut peak, mut peak_dx) = (0u8, 0.0f64);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "half is rect.width() / 2.0 + spread, always non-negative for any rect this toolkit draws"
        )]
        let steps = half as u32;
        for dx in 0..=steps {
            let v = at(cx + f64::from(dx), cy);
            if v > peak {
                peak = v;
                peak_dx = f64::from(dx);
            }
        }

        #[allow(
            clippy::cast_possible_truncation,
            reason = "255.0 * (1.0 - SHADOW_PEAK_RADIUS) is within 0.0..=255.0"
        )]
        let expected = (255.0 * (1.0 - SHADOW_PEAK_RADIUS)).round() as i32;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "255.0 * (1.0 - SHADOW_PEAK_RADIUS)^2 is within 0.0..=255.0"
        )]
        let double_premultiplied =
            (255.0 * (1.0 - SHADOW_PEAK_RADIUS) * (1.0 - SHADOW_PEAK_RADIUS)).round() as i32;
        assert!(
            (i32::from(peak) - expected).abs() <= 6,
            "full-intensity white shadow must peak at ~{expected}/255 \
             (the sprite's own alpha envelope); got {peak}. A peak near \
             {double_premultiplied} means the premultiplied sprite is being premultiplied a \
             second time.",
        );

        let expected_dx = rect.width() / 2.0 - (1.0 - 2.0 * SHADOW_PEAK_RADIUS) * spread;
        assert!(
            (peak_dx - expected_dx).abs() <= 4.0,
            "the ring must peak {expected_dx} px out from the centre — \
             {} px inside the rect's own edge, independent of the rect's \
             size; peaked at {peak_dx}",
            (1.0 - 2.0 * SHADOW_PEAK_RADIUS) * spread,
        );

        // The centre stays clear so a card's own fill paints over an
        // unmodified backdrop, and the target's corner is fully
        // transparent so no square halo leaks out.
        assert!(at(cx, cy) <= 6, "sprite centre must stay clear");
        assert_eq!(
            at(cx + half, cy + half),
            0,
            "the stretched sprite must not leak a square-edged halo"
        );
    }

    /// Sample the shadow outward from the middle of `rect`'s right and
    /// bottom edges, one sample per pixel, starting half a pixel outside
    /// the edge.
    fn outward_profiles(rect: Rect, spread: f64, span: u32) -> (Vec<u8>, Vec<u8>) {
        let at = render_shadow(1400, 900, rect, spread);
        let right = (0..span)
            .map(|k| at(rect.x1 + f64::from(k), rect.center().y))
            .collect();
        let bottom = (0..span)
            .map(|k| at(rect.center().x, rect.y1 + f64::from(k)))
            .collect();
        (right, bottom)
    }

    /// The falloff outside an edge is a function of the distance to that
    /// edge and of `spread` — never of the rect's own dimensions. Wide,
    /// tall and small-square must all match [`shadow_profile`] pixel for
    /// pixel, on both axes.
    #[test]
    fn soft_shadow_falloff_width_is_the_spread_not_the_rects_size() {
        let spread = 40.0;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "spread is a small positive px constant in these tests, always well within u32 range"
        )]
        let span = spread as u32 + 6;
        // Wide (the overview's active row), tall, and a small square.
        let wide = outward_profiles(Rect::new(150.0, 330.0, 1172.0, 565.0), spread, span);
        let tall = outward_profiles(Rect::new(560.0, 100.0, 800.0, 800.0), spread, span);
        let square = outward_profiles(Rect::new(600.0, 400.0, 700.0, 500.0), spread, span);

        for (label, profile) in [
            ("wide/right", &wide.0),
            ("wide/bottom", &wide.1),
            ("tall/right", &tall.0),
            ("tall/bottom", &tall.1),
            ("square/right", &square.0),
            ("square/bottom", &square.1),
        ] {
            // 1. Every sample matches the declared envelope at its own
            //    distance from the edge, in units of `spread`.
            for (k, got) in profile.iter().enumerate() {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "k is a small profile-vector index (at most a few hundred), \
                              far below where f64 loses integer precision"
                )]
                let u = (k as f64 + 0.5) / spread;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "255.0 * shadow_profile(u) is within 0.0..=255.0"
                )]
                let want = (255.0 * shadow_profile(u)).round() as i32;
                assert!(
                    (i32::from(*got) - want).abs() <= 8,
                    "{label}: {k} px out from the edge must be {want}/255 \
                     (shadow_profile({u:.3})); got {got}. Whole profile: \
                     {profile:?}"
                );
            }
            // 2. …so the band is `spread` wide: still painting half way
            //    out, nothing at all past the spread.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "spread * 0.5 is a small positive px offset, always within the profile vector's length"
            )]
            let half_way = profile[(spread * 0.5) as usize];
            assert!(
                half_way >= 8,
                "{label}: half a spread ({} px) out the shadow must still \
                 be painting; got {half_way}/255: {profile:?}",
                spread * 0.5,
            );
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "spread is a small positive px constant, always within the profile vector's length"
            )]
            let spread_floor = spread as usize;
            assert!(
                profile[spread_floor..].iter().all(|v| *v == 0),
                "{label}: nothing may paint past {spread} px — the spread \
                 *is* the falloff width: {profile:?}"
            );
            // 3. And it is actually a shadow at the edge, not a hairline.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "255.0 * shadow_profile(0.0) is within 0.0..=255.0"
            )]
            let edge_alpha = (255.0 * shadow_profile(0.0)) as i32;
            assert!(
                i32::from(profile[0]) >= edge_alpha - 12,
                "{label}: the shadow at the edge must read ~{}/255; got {}",
                (255.0 * shadow_profile(0.0)).round(),
                profile[0]
            );
        }

        // 4. Stated directly: the three aspect ratios agree with each
        //    other, and each rect's own two axes agree.
        for (label, a, b) in [
            ("wide vs square", &wide.0, &square.0),
            ("tall vs square", &tall.0, &square.0),
            ("wide: x vs y", &wide.0, &wide.1),
            ("tall: x vs y", &tall.0, &tall.1),
        ] {
            for (k, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                assert!(
                    (i32::from(*x) - i32::from(*y)).abs() <= 6,
                    "{label}: the profiles diverge {k} px out from the \
                     edge ({x} vs {y}): {a:?} vs {b:?}"
                );
            }
        }
    }

    /// A corner's falloff is a `spread`-radius quarter circle: alpha a
    /// given distance diagonally out from the corner matches alpha the
    /// same distance straight out from an edge — on a wide rect too.
    #[test]
    fn soft_shadow_corners_stay_circular_on_a_wide_rect() {
        let spread = 40.0;
        let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
        let at = render_shadow(1400, 900, rect, spread);
        for r in [6.0_f64, 12.0, 20.0, 28.0, 36.0] {
            let straight = i32::from(at(rect.x1 + r, rect.center().y));
            for deg in [15.0_f64, 30.0, 45.0, 60.0, 75.0] {
                let (sin, cos) = deg.to_radians().sin_cos();
                let got = i32::from(at(rect.x1 + r * cos, rect.y1 + r * sin));
                assert!(
                    (got - straight).abs() <= 8,
                    "{r} px out from the bottom-right corner at {deg}° \
                     reads {got}/255, but {r} px straight out from the \
                     edge reads {straight}/255 — the corner is not a \
                     circle of radius {spread}"
                );
            }
        }
    }

    /// Along one edge the shadow is constant, including across the
    /// boundaries where the edge slice meets the two corner slices — any
    /// step there would show as a hairline running out of the corner.
    #[test]
    fn soft_shadow_is_uniform_along_an_edge_and_seamless_at_the_slice_joins() {
        let spread = 40.0;
        let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
        let at = render_shadow(1400, 900, rect, spread);
        // The corner slices own the first and last `spread` of the edge;
        // between them the field must not vary at all.
        for dy in [1.0_f64, 8.0, 20.0] {
            let y = rect.y1 + dy;
            let start = rect.x0 + spread;
            let end = rect.x1 - spread;
            let span = end - start;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "span is the sampled interior width in px (rect width minus \
                          2*spread); always non-negative here and far below u32::MAX \
                          for any rect this toolkit draws"
            )]
            let steps = if span >= 0.0 {
                (span as u32).saturating_add(1)
            } else {
                0
            };
            let samples: Vec<(f64, u8)> = (0..steps)
                .map(|i| {
                    let x = start + f64::from(i);
                    (x, at(x, y))
                })
                .collect();
            let lo = samples.iter().map(|(_, v)| *v).min().unwrap();
            let hi = samples.iter().map(|(_, v)| *v).max().unwrap();
            assert!(
                i32::from(hi) - i32::from(lo) <= 3,
                "{dy} px below the bottom edge the shadow varies from \
                 {lo} to {hi} across the edge's length; the worst \
                 columns were {:?}",
                samples
                    .iter()
                    .filter(|(_, v)| *v == lo || *v == hi)
                    .take(6)
                    .collect::<Vec<_>>()
            );
        }
    }

    /// A wide card's interior stays completely clear — the centre slice
    /// is skipped because the envelope is zero more than one band in.
    #[test]
    fn soft_shadow_leaves_a_wide_rects_interior_clear() {
        let spread = 40.0;
        let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
        let at = render_shadow(1400, 900, rect, spread);
        for fx in [0.25_f64, 0.5, 0.75] {
            let x = rect.x0 + rect.width() * fx;
            let y = rect.center().y;
            assert_eq!(at(x, y), 0, "the interior at ({x}, {y}) must be clear");
        }
    }

    #[test]
    fn degenerate_inputs_are_noops() {
        let mut scene = Scene::new();
        // Zero-area rect, zero intensity, zero spread — must not panic
        // and must add nothing detectable (we just assert it returns).
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
        soft_shadow(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Color::BLACK,
            0.0,
            0.0,
            0.5,
        );
    }

    /// A pill shorter than `2 · spread` still gets the full outward
    /// falloff: opposing slices share the interior instead of squashing
    /// the profile. The toolkit's tab/close/restore pills are this shape
    /// (26 px tall against a 10 px spread).
    #[test]
    fn a_pill_thinner_than_the_spread_keeps_the_full_falloff() {
        let spread = 20.0;
        // 24 px tall — narrower than 2 · spread — and 120 px wide.
        let pill = Rect::new(400.0, 400.0, 520.0, 424.0);
        let at = render_shadow(1400, 900, pill, spread);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "spread is a small positive px constant in these tests, always well within u32 range"
        )]
        let steps = spread as u32 + 4;
        for k in 0..steps {
            let u = (f64::from(k) + 0.5) / spread;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "255.0 * shadow_profile(u) is within 0.0..=255.0"
            )]
            let want = (255.0 * shadow_profile(u)).round() as i32;
            let got = i32::from(at(pill.center().x, pill.y1 + f64::from(k)));
            assert!(
                (got - want).abs() <= 8,
                "{k} px below a {}-px-tall pill must read {want}/255 \
                 (shadow_profile({u:.3})); got {got}",
                pill.height(),
            );
        }
    }
}
