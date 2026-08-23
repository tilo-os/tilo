//! Shared surface-drawing primitives — the reusable "how a card/panel
//! sits above the desktop" layer.
//!
//! [`shadow`] is how a surface casts one. It is a true Gaussian blur of
//! the surface's **own rounded outline**, via vello's
//! `draw_blurred_rounded_rect`, at the strength its elevation token
//! declares. A shadow is therefore the same shape as the thing casting
//! it, and is equally dense the whole way round.
//!
//! [`soft_shadow`] is the older painter: a baked falloff 9-sliced around
//! the rect, written when vello had no blur primitive. Vello 0.9 has one,
//! so nothing that owns a corner radius should use the sprite path any
//! more — its corner term is the signed distance to a **square** corner,
//! which puts a hard box corner around a rounded surface and composites
//! the corners far lighter than the flat edges. It survives for the
//! effects that are not drop shadows and have no outline to follow: the
//! wheel disc's bloom, an accent glow around a dragged card.
//!
//! Sprites are baked once per tint and cached, so repeated glows of
//! the same colour cost only a handful of image stretches.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{
    Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as Image,
    ImageFormat as Format,
};

use crate::theme::Theme;
use crate::tokens::Shadow;

/// Resolution of the baked falloff along the profile axis. 256 samples
/// across a band that is at most a couple of hundred logical px wide is
/// plenty, and the Gaussian is smooth so any rescale stays soft.
const SHADOW_SPRITE_SIZE: u32 = 256;
/// Row count of the edge sprite. The edge falloff depends only on the
/// axis perpendicular to the edge, so every row is identical; keeping a
/// few of them (rather than one) leaves the image non-degenerate for the
/// sampler at a cost of 4 KB.
const SHADOW_EDGE_ROWS: u32 = 4;
/// Gaussian width of the falloff ring. Larger = softer, more spread-out.
const SHADOW_GAUSSIAN_K: f64 = 0.35;
/// Where the envelope peaks, as a fraction of the band it is baked
/// across. The band runs from one `spread` *inside* the rect's edge to
/// one `spread` outside it, so `0.30` puts the darkest point
/// `(1 - 2·0.30) = 0.4` of a spread under the rect's own edge and leaves
/// the interior fully transparent — a card's fill paints over clear
/// sprite, not a pre-darkened spot, which is what keeps translucent
/// surfaces from self-darkening.
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
/// rounded-box SDF with a zero corner radius.
///
/// `ux`/`uy` are each already normalised to their own band (`+1` one
/// spread outside that edge, `-1` at the inner end). Outside both edges
/// this is `hypot`, so the iso-alpha contours are *circles* of radius
/// `spread` about the corner; the moment one coordinate goes inside, the
/// `max` term takes over and the field collapses to a pure function of
/// the other axis — which is exactly what the neighbouring edge slice
/// paints, so the two meet without a discontinuity.
fn corner_u(ux: f64, uy: f64) -> f64 {
    let ox = ux.max(0.0);
    let oy = uy.max(0.0);
    (ox * ox + oy * oy).sqrt() + ux.max(uy).min(0.0)
}

/// Map a texel index onto its band coordinate in `-1..=1`, sampling at
/// pixel centres so the brush's half-texel offset doesn't skew the
/// baked profile against the geometry it is stretched over.
fn band_coord(i: u32, n: u32) -> f64 {
    2.0 * (i as f64 + 0.5) / n as f64 - 1.0
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
    /// `SHADOW_SPRITE_SIZE × SHADOW_EDGE_ROWS`: the 1-D profile running
    /// along x (inner end at x = 0), constant down every column. Stretched
    /// along one axis only, so an edge's falloff width never depends on
    /// how long that edge is.
    edge: Image,
    /// `SHADOW_SPRITE_SIZE²`: the corner quadrant, oriented
    /// bottom-right (inner end at the origin). Mirrored into the other
    /// three corners, and never scaled by anything but the band, so
    /// corners stay `spread × spread` and circular on any aspect ratio.
    corner: Image,
}

fn bake(r: u8, g: u8, b: u8, w: u32, h: u32, f: impl Fn(u32, u32) -> f64) -> Image {
    let mut data = Vec::<u8>::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = f(x, y).clamp(0.0, 1.0);
            data.push((r as f64 * a).round() as u8);
            data.push((g as f64 * a).round() as u8);
            data.push((b as f64 * a).round() as u8);
            data.push((a * 255.0).round() as u8);
        }
    }
    Image {
        data: Blob::new(Arc::new(data)),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width: w,
        height: h,
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
fn sprites_for(r: u8, g: u8, b: u8) -> Arc<ShadowSprites> {
    static CACHE: OnceLock<Mutex<HashMap<(u8, u8, u8), Arc<ShadowSprites>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
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
/// `local` maps the sprite's pixel rect onto the slice's **full** band
/// geometry — always `spread` on each side of the edge, whatever the
/// rect's size — and `clip` narrows that to the part this slice actually
/// owns. Keeping the two separate is what lets a rect shorter than
/// `2 · spread` share its inner band between the opposing slices without
/// either squashing the profile (which would shorten the outer falloff,
/// the very bug this function exists to fix) or double-painting the
/// overlap.
///
/// The shape is derived in `local` space, which is axis-aligned by
/// construction, so the bbox is exact no matter what the caller's own
/// `transform` does.
fn blit(scene: &mut Scene, transform: Affine, local: Affine, brush: &ImageBrush, clip: Rect) {
    if clip.width() <= 0.0 || clip.height() <= 0.0 {
        return;
    }
    let shape = local.inverse().transform_rect_bbox(clip);
    scene.fill(Fill::NonZero, transform * local, brush, None, &shape);
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
/// The falloff is **9-sliced** around the rect: the four corners get the
/// corner sprite at a fixed `spread × spread` (so they stay circular),
/// the four edges stretch the 1-D profile along their own length only
/// (so the falloff width is always `spread`), and the middle is skipped
/// because the envelope is zero there by construction.
///
/// The single stretched sprite this replaced was radially symmetric in
/// *sprite* space, so squashing it over `rect ± spread` scaled the
/// falloff with the rect's own dimensions: on the overview's 1022×235
/// active card at `spread` 40 the ring landed ~165 px from centre —
/// under the card — and the accent glow the concept asks for was
/// invisible at every aspect ratio but square. Every wide surface in the
/// toolkit (notifications, the palette, cards) had the same shadow.
///
/// No-op for a degenerate rect, `spread <= 0`, or `intensity <= 0`.
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
    // Band geometry. Every slice's profile runs `spread` outward from an
    // edge and `spread` inward; the *clip* boundaries meet at the rect's
    // mid-lines when it is narrower than `2 · spread`, so opposing slices
    // divide the interior between them rather than overlapping.
    let in_x = spread.min(rect.width() / 2.0);
    let in_y = spread.min(rect.height() / 2.0);
    let (ox0, ix0) = (rect.x0 - spread, rect.x0 + in_x);
    let (ix1, ox1) = (rect.x1 - in_x, rect.x1 + spread);
    let (oy0, iy0) = (rect.y0 - spread + offset_y, rect.y0 + in_y + offset_y);
    let (iy1, oy1) = (rect.y1 - in_y + offset_y, rect.y1 + spread + offset_y);
    // Full band extents, ignoring the clamp: this is what the sprites are
    // baked against.
    let (bx0, bx1) = (rect.x0 + spread, rect.x1 - spread);
    let (by0, by1) = (rect.y0 + spread + offset_y, rect.y1 - spread + offset_y);

    let rgba = color.to_rgba8();
    let sprites = sprites_for(rgba.r, rgba.g, rgba.b);
    let n = SHADOW_SPRITE_SIZE as f64;
    let rows = SHADOW_EDGE_ROWS as f64;

    // `intensity` rides on the brushes rather than on a group layer.
    // The eight slices are each clipped to the region they own, so they
    // never overlap and cannot double-darken — which is the only thing a
    // group would have bought. A layer per shadow is not free in vello,
    // and every card, toast and palette in the toolkit casts one.
    let alpha = intensity.clamp(0.0, 1.0);

    // Corners: the sprite covers a 2·spread square centred on the
    // corner, mirrored into each quadrant, and is never stretched by
    // anything but the band — so the corner falloff is a `spread`-radius
    // quarter circle on every aspect ratio.
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

    // Edges: the same 1-D profile, stretched along the edge's own length
    // only. The centre slice is skipped — the envelope is zero more than
    // one band inside the rect, which is also what keeps a translucent
    // card from darkening itself.
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
/// own corner `radius`.
///
/// **This is how a surface casts a shadow.** It is a true Gaussian blur of
/// the surface's own rounded outline, so the shadow is the same shape as
/// the thing casting it and is equally dense the whole way round.
///
/// It replaces a 9-sliced sprite approximation that existed because
/// "Vello has no blur primitive". Vello 0.9 has one, and the sprite's two
/// compromises had both become visible on the quick-settings panel:
///
/// - Its corner falloff was the signed distance to the rect with a **zero**
///   corner radius, so around a 24 px rounded corner the shadow kept a
///   hard square corner set back from the panel's own, and the corner
///   slice composited about a fifth of what the flat edges did. The eye
///   reads that as a box with its corners rubbed out.
/// - Its profile peaked *inside* the rect and was then clipped away, so
///   what escaped past the edge was the long shallow tail — a smear
///   rather than a falloff.
///
/// The token's `color` alpha is the shadow's strength and is honoured
/// here. [`soft_shadow`] deliberately ignores it in favour of a separate
/// `intensity` argument, which is a foot-gun a caller passing `1.0` steps
/// straight into: that is exactly how `e4`'s 110/255 became an opaque
/// black skirt on screen.
///
/// No-op for a degenerate rect or a token with no blur.
pub fn shadow(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    token: &Shadow,
) {
    if token.blur <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let alpha = token.color.components[3];
    if alpha <= 0.0 {
        return;
    }
    scene.draw_blurred_rounded_rect(
        transform,
        rect + Vec2::new(0.0, token.offset_y as f64),
        token.color,
        radius.clamp(0.0, rect.width().min(rect.height()) / 2.0),
        std_dev_for(token.blur),
    );
}

/// The Gaussian standard deviation a CSS-style `blur` radius means.
///
/// Halving is the conventional mapping, and vello cuts its kernel at
/// `2.5 * std_dev`, so a token's visible falloff ends about `1.25 * blur`
/// past the surface. One function so every surface agrees: the previous
/// two paths disagreed by 1.85x — `elevation()` inflated the token to
/// `blur * 1.6 + 6` while `chrome::shell::panel_shadow` used it raw, so
/// the same `e4` produced a 44 px shadow on the workspace-settings sheet
/// and a 24 px one on quick settings.
pub fn std_dev_for(blur: f32) -> f64 {
    (blur as f64) / 2.0
}

/// Paint an elevation-token shadow under `rect`, square-cornered.
///
/// Retained for callers that genuinely have no radius to follow. Anything
/// with rounded corners must call [`shadow`] and pass them, or it gets a
/// shadow of a different shape than itself.
pub fn elevation(scene: &mut Scene, transform: Affine, rect: Rect, token: &Shadow) {
    shadow(scene, transform, rect, 0.0, token);
}

/// The one contract for "is this small selectable chrome element
/// active" — scope pills, filter chips, tab pills, toggle cards.
///
/// Returns `(fill, border)` for a pill/chip given the theme and whether
/// it's the selected one. This replaces the four incompatible encodings
/// the chrome grew independently (white/10 overlay, solid indigo,
/// indigo/20 wash, red/20 wash): selected is a solid accent with no
/// border; idle is the raised surface with a standard 1px border.
pub fn chip_colors(theme: &Theme, selected: bool) -> (Color, Option<Color>) {
    let c = &theme.tokens.colors;
    if selected {
        (c.accent[5], None)
    } else {
        (c.surfaces.raised, Some(c.border_standard))
    }
}

/// Draw a pill / chip: a fully-rounded (or `radius`-rounded) filled
/// rect with an optional 1px border. Centralises the shape so every
/// pill in the UI shares one corner treatment instead of the chrome's
/// ad-hoc mix of 4/6/10/12/16/full radii. Pass `radius = None` for a
/// fully-rounded capsule (`height/2`).
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
    let r = radius.unwrap_or(rect.height() / 2.0);
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
        assert!(Arc::ptr_eq(&a, &b), "same tint must return the cached sprite");
        let c = sprites_for(99, 102, 241);
        assert!(!Arc::ptr_eq(&a, &c), "different tint must bake a new sprite");
    }

    fn alpha_at(img: &Image, x: u32, y: u32) -> u8 {
        img.data.data()[((y * img.width + x) * 4 + 3) as usize]
    }

    /// Both ends of the band are zero — the inner end so a card's own
    /// fill paints over clear sprite, the outer end so no slice can leak
    /// a hard-edged halo — and the envelope peaks where
    /// [`SHADOW_PEAK_RADIUS`] says it does.
    #[test]
    fn edge_sprite_runs_from_zero_through_the_peak_back_to_zero() {
        let s = build_shadow_sprites(0, 0, 0);
        let n = SHADOW_SPRITE_SIZE;
        assert!(alpha_at(&s.edge, 0, 0) <= 2, "inner end of the band must be ~0");
        assert!(
            alpha_at(&s.edge, n - 1, 0) <= 2,
            "outer end of the band must be ~0"
        );
        // Peak column: SHADOW_PEAK_RADIUS along the band.
        let peak_x = (SHADOW_PEAK_RADIUS * n as f64).round() as u32;
        let peak = alpha_at(&s.edge, peak_x, 0);
        let expected = (255.0 * (1.0 - SHADOW_PEAK_RADIUS)).round() as i32;
        assert!(
            (peak as i32 - expected).abs() <= 2,
            "the baked peak must be {expected}/255; got {peak}"
        );
        // Every row is the same profile — that is what lets one edge
        // sprite stretch along an edge of any length.
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

    /// The corner sprite has to *become* the edge profile along the two
    /// rows/columns where it meets an edge slice. That agreement is what
    /// makes the 9-slice seamless by construction rather than by luck.
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

    /// In the sprite itself the corner's iso-alpha contours are circles
    /// about the corner point, not the axis-aligned or elliptical
    /// contours a stretched radial sprite produces.
    #[test]
    fn corner_sprite_contours_are_circular() {
        let s = build_shadow_sprites(0, 0, 0);
        let n = SHADOW_SPRITE_SIZE as f64;
        // Band coordinate 0 is the rect's own edge — the sprite's
        // mid-point. Walking a quarter circle of radius `r` (in band
        // units) out from the corner must hold the alpha constant.
        let px = |u: f64| ((u + 1.0) * 0.5 * n).round().clamp(0.0, n - 1.0) as u32;
        for r in [0.25_f64, 0.5, 0.75] {
            let straight = alpha_at(&s.corner, px(r), px(0.0)) as i32;
            for deg in [15.0_f64, 30.0, 45.0, 60.0, 75.0] {
                let (sin, cos) = deg.to_radians().sin_cos();
                let got = alpha_at(&s.corner, px(r * cos), px(r * sin)) as i32;
                assert!(
                    (got - straight).abs() <= 3,
                    "at radius {r} the corner alpha must not depend on the \
                     angle: {deg}° gave {got}, straight out gave {straight}"
                );
            }
        }
    }


    /// Render the token painter over black. The red channel of the
    /// result *is* the composited alpha, so a sample reads the
    /// production output of [`shadow`] directly.
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

    /// A shadow is as even around a rounded corner as it is along a flat
    /// edge, because it is a blur of the surface's own outline.
    ///
    /// This is the defect that produced the 2026-08 "weirdly boxy and
    /// uneven" report. The sprite painter's corner term was the signed
    /// distance to the rect with a **zero** corner radius, so at a 24 px
    /// radius the corner composited roughly a fifth of what the flat
    /// edges did and kept a hard square corner of its own — read from
    /// the outside, a box with the corners rubbed out.
    ///
    /// Sampling is at equal *normal* distance from the silhouette: out
    /// from the flat right edge, and out along the corner arc's own
    /// diagonal. A blur of the outline gives those the same coverage.
    #[test]
    fn token_shadow_is_even_around_a_rounded_corner() {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let radius = 24.0;
        // offset_y 0 isolates shape from the deliberate downward shift.
        let token = Shadow { offset_y: 0.0, blur: 16.0, color: Color::WHITE };
        let at = render_token_shadow(400, 400, rect, radius, &token);

        // Corner arc centre, and the point where the arc meets the
        // outward diagonal.
        let cc = 250.0 - radius;
        let k = std::f64::consts::FRAC_1_SQRT_2;
        for d in [2.0_f64, 5.0, 9.0, 14.0] {
            let edge = at(250.0 + d, 200.0);
            let corner = at(cc + (radius + d) * k, cc + (radius + d) * k);
            let (lo, hi) = (edge.min(corner) as f64, edge.max(corner) as f64);
            assert!(
                hi - lo <= 14.0,
                "at {d} px out the shadow must be as dense at the rounded \
                 corner as along the flat edge: edge {edge}, corner \
                 {corner}. A corner far lighter than the edge is a shadow \
                 whose outline is not the surface's outline."
            );
        }
    }

    /// The token's colour alpha is the shadow's strength.
    ///
    /// [`soft_shadow`] ignores it in favour of a separate `intensity`
    /// argument, and every caller that passed `1.0` therefore rendered
    /// its token at full black — which is how `e4`'s 110/255 reached the
    /// screen as an opaque skirt. [`shadow`] reads the token.
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
        let ratio = half as f64 / full as f64;
        assert!(
            (ratio - 0.5).abs() <= 0.08,
            "halving the token's alpha must halve the shadow: {full} -> \
             {half} (ratio {ratio:.3}). A ratio near 1.0 means the token's \
             alpha is being discarded."
        );
    }

    /// A token's visible reach is set by its `blur`, through the one
    /// shared mapping.
    ///
    /// `elevation()` used to inflate the token to `blur * 1.6 + 6` while
    /// `chrome::shell::panel_shadow` used it raw, so the same `e4` was a
    /// 44 px shadow on one panel and a 24 px one on another.
    #[test]
    fn token_shadow_reach_follows_blur() {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let token = Shadow { offset_y: 0.0, blur: 16.0, color: Color::WHITE };
        let at = render_token_shadow(400, 400, rect, 24.0, &token);
        let reach = std_dev_for(token.blur) * 2.5;
        let mut last = 0.0_f64;
        for d in 1..=((reach * 2.0) as u32) {
            if at(250.0 + d as f64, 200.0) > 2 {
                last = d as f64;
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

    /// Render one full-intensity white shadow over black. The red
    /// channel of the result *is* the composited alpha, so the sampler
    /// reads the production output of [`soft_shadow`] in the same units
    /// [`shadow_profile`] declares.
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
    /// 178`. When the premultiplied sprite is handed to the renderer as
    /// straight alpha the tint gets premultiplied twice and that peak
    /// collapses to `255 · 0.7² ≈ 125` — a visibly washed-out shadow
    /// everywhere in the toolkit, which is what this pins against.
    ///
    /// The ring's position is asserted too, and it is measured from the
    /// rect's **edge**: the envelope peaks `(1 - 2·SHADOW_PEAK_RADIUS)`
    /// of a `spread` inside the edge and dies exactly one `spread`
    /// outside it, whatever the rect's size. It used to sit at
    /// `SHADOW_PEAK_RADIUS` of the *target's* half-extent, which is the
    /// aspect-dependent placement that hid every wide surface's shadow.
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
        for dx in 0..=(half as u32) {
            let v = at(cx + dx as f64, cy);
            if v > peak {
                peak = v;
                peak_dx = dx as f64;
            }
        }

        let expected = (255.0 * (1.0 - SHADOW_PEAK_RADIUS)).round() as i32;
        assert!(
            (peak as i32 - expected).abs() <= 6,
            "full-intensity white shadow must peak at ~{expected}/255 \
             (the sprite's own alpha envelope); got {peak}. A peak near \
             {} means the premultiplied sprite is being premultiplied a \
             second time.",
            (255.0 * (1.0 - SHADOW_PEAK_RADIUS) * (1.0 - SHADOW_PEAK_RADIUS)).round() as i32,
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
            .map(|k| at(rect.x1 + k as f64, rect.center().y))
            .collect();
        let bottom = (0..span)
            .map(|k| at(rect.center().x, rect.y1 + k as f64))
            .collect();
        (right, bottom)
    }

    /// The headline geometry contract: the falloff outside an edge is a
    /// function of the distance to that edge and of `spread` — never of
    /// the rect's own dimensions.
    ///
    /// The single stretched sprite this replaced was radially symmetric
    /// in sprite space, so squashing it over `rect ± spread` made the
    /// falloff width scale with the rect: the overview's 1022×235 active
    /// card at `spread` 40 put its ring ~165 px from centre, under the
    /// card, and the concept's accent glow was invisible. Here a wide
    /// card, a tall card and a small square must all produce the *same*
    /// outward profile, on both axes, and it must match
    /// [`shadow_profile`] pixel for pixel.
    #[test]
    fn soft_shadow_falloff_width_is_the_spread_not_the_rects_size() {
        let spread = 40.0;
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
                let u = (k as f64 + 0.5) / spread;
                let want = (255.0 * shadow_profile(u)).round() as i32;
                assert!(
                    (*got as i32 - want).abs() <= 8,
                    "{label}: {k} px out from the edge must be {want}/255 \
                     (shadow_profile({u:.3})); got {got}. Whole profile: \
                     {profile:?}"
                );
            }
            // 2. …so the band that carries the falloff is `spread` wide:
            //    still painting half way out, and nothing at all past the
            //    spread. Under the stretched sprite the wide card was
            //    already down to ~2/255 one pixel out and blank by ten.
            let half_way = profile[(spread * 0.5) as usize];
            assert!(
                half_way >= 8,
                "{label}: half a spread ({} px) out the shadow must still \
                 be painting; got {half_way}/255: {profile:?}",
                spread * 0.5,
            );
            assert!(
                profile[spread as usize..].iter().all(|v| *v == 0),
                "{label}: nothing may paint past {spread} px — the spread \
                 *is* the falloff width: {profile:?}"
            );
            // 3. And it is actually a shadow at the edge, not a hairline.
            assert!(
                profile[0] as i32 >= (255.0 * shadow_profile(0.0)) as i32 - 12,
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
                    (*x as i32 - *y as i32).abs() <= 6,
                    "{label}: the profiles diverge {k} px out from the \
                     edge ({x} vs {y}): {a:?} vs {b:?}"
                );
            }
        }
    }

    /// A corner's falloff is a `spread`-radius quarter circle about the
    /// corner point, so the alpha a given distance diagonally out from
    /// the corner matches the alpha the same distance straight out from
    /// an edge. The stretched sprite turned that circle into an ellipse
    /// as wide as the card.
    #[test]
    fn soft_shadow_corners_stay_circular_on_a_wide_rect() {
        let spread = 40.0;
        let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
        let at = render_shadow(1400, 900, rect, spread);
        for r in [6.0_f64, 12.0, 20.0, 28.0, 36.0] {
            let straight = at(rect.x1 + r, rect.center().y) as i32;
            for deg in [15.0_f64, 30.0, 45.0, 60.0, 75.0] {
                let (sin, cos) = deg.to_radians().sin_cos();
                let got = at(rect.x1 + r * cos, rect.y1 + r * sin) as i32;
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
    /// boundaries where the edge slice meets the two corner slices. Any
    /// step there would show as a hairline running out of the corner —
    /// so this is both the "edges stretch on one axis only" contract and
    /// the seam check for the 9-slice.
    #[test]
    fn soft_shadow_is_uniform_along_an_edge_and_seamless_at_the_slice_joins() {
        let spread = 40.0;
        let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
        let at = render_shadow(1400, 900, rect, spread);
        // The corner slices own the first and last `spread` of the edge;
        // between them the field must not vary at all. Sampling starts
        // exactly on those two joins.
        for dy in [1.0_f64, 8.0, 20.0] {
            let y = rect.y1 + dy;
            let mut samples: Vec<(f64, u8)> = Vec::new();
            let mut x = rect.x0 + spread;
            while x <= rect.x1 - spread {
                samples.push((x, at(x, y)));
                x += 1.0;
            }
            let lo = samples.iter().map(|(_, v)| *v).min().unwrap();
            let hi = samples.iter().map(|(_, v)| *v).max().unwrap();
            assert!(
                hi as i32 - lo as i32 <= 3,
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

    /// A wide card's interior stays completely clear, which is what lets
    /// translucent surfaces sit on a shadow without darkening
    /// themselves. The centre slice is skipped precisely because the
    /// envelope is zero more than one band in.
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
    /// falloff: the opposing slices share the interior instead of
    /// squashing the profile into whatever room the rect has. The
    /// toolkit's own tab / close / restore pills are exactly this shape
    /// (26 px tall against a 10 px spread), and the wheel disc's spoke
    /// card is the small near-square case that already looked right and
    /// must keep looking right.
    #[test]
    fn a_pill_thinner_than_the_spread_keeps_the_full_falloff() {
        let spread = 20.0;
        // 24 px tall — narrower than 2 · spread — and 120 px wide.
        let pill = Rect::new(400.0, 400.0, 520.0, 424.0);
        let at = render_shadow(1400, 900, pill, spread);
        for k in 0..(spread as u32 + 4) {
            let u = (k as f64 + 0.5) / spread;
            let want = (255.0 * shadow_profile(u)).round() as i32;
            let got = at(pill.center().x, pill.y1 + k as f64) as i32;
            assert!(
                (got - want).abs() <= 8,
                "{k} px below a {}-px-tall pill must read {want}/255 \
                 (shadow_profile({u:.3})); got {got}",
                pill.height(),
            );
        }
    }
}
