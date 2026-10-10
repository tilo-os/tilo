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
use vello::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{
    Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as Image, ImageFormat as Format,
};

use crate::ColorCompat;
use crate::theme::Theme;
use crate::tokens::{Colors, Shadow};

/// Shared silhouette for lightweight desktop floats. The asymmetry is only
/// one or two logical pixels along each edge: enough to feel soft and held,
/// never enough to look like a novelty blob. Body, border, app-owned call /
/// navigation surfaces, and generic floating windows all use this exact path
/// so their corner radii cannot drift apart.
pub fn soft_outline(rect: Rect) -> BezPath {
    let mut path = BezPath::new();
    path.move_to((rect.x0 + 21.0, rect.y0 + 1.5));
    path.curve_to(
        (rect.x0 + 8.0, rect.y0),
        (rect.x0, rect.y0 + 9.0),
        (rect.x0 + 1.5, rect.y0 + 23.0),
    );
    path.line_to((rect.x0 - 1.0, rect.y1 - 20.0));
    path.curve_to(
        (rect.x0, rect.y1 - 7.0),
        (rect.x0 + 10.0, rect.y1 + 1.0),
        (rect.x0 + 24.0, rect.y1 + 1.5),
    );
    path.line_to((rect.x1 - 18.0, rect.y1 + 2.0));
    path.curve_to(
        (rect.x1 - 6.0, rect.y1),
        (rect.x1 + 1.0, rect.y1 - 9.0),
        (rect.x1, rect.y1 - 22.0),
    );
    path.line_to((rect.x1 + 2.0, rect.y0 + 27.0));
    path.curve_to(
        (rect.x1 + 1.0, rect.y0 + 11.0),
        (rect.x1 - 10.0, rect.y0 - 1.0),
        (rect.x1 - 27.0, rect.y0),
    );
    path.close_path();
    path
}

/// Shared quiet drag affordance for a detachable soft surface.
///
/// Call, navigation and generic floating windows use the same 36×3 logical
/// pixel pill. Keeping this out of app renderers makes the cross-boundary
/// gesture recognizable without imposing one shared title/control layout on
/// workflow-specific surfaces.
pub fn floating_drag_grip(scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
    let grip = Rect::new(
        rect.center().x - 18.0,
        rect.y0 + 6.0,
        rect.center().x + 18.0,
        rect.y0 + 9.0,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        color.with_alpha_factor(0.38),
        None,
        &RoundedRect::from_rect(grip, 2.0),
    );
}

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

/// Each tint owns a 256² corner plus a small edge image (~260 KiB total).
/// Theme surfaces normally use only a handful of tints; 32 keeps those hot
/// while bounding a dynamic-colour caller to roughly 8.1 MiB of cached pixels.
const SHADOW_SPRITE_CACHE_LIMIT: usize = 32;

fn cached_sprites(
    map: &mut HashMap<(u8, u8, u8), Arc<ShadowSprites>>,
    key: (u8, u8, u8),
) -> Arc<ShadowSprites> {
    if let Some(cached) = map.get(&key) {
        return Arc::clone(cached);
    }
    if map.len() >= SHADOW_SPRITE_CACHE_LIMIT
        && let Some(victim) = map.keys().next().copied()
    {
        map.remove(&victim);
    }
    let sprites = Arc::new(build_shadow_sprites(key.0, key.1, key.2));
    map.insert(key, Arc::clone(&sprites));
    sprites
}

fn sprites_for(r: u8, g: u8, b: u8) -> Arc<ShadowSprites> {
    static CACHE: OnceLock<ShadowSpriteCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // A panic elsewhere while the lock was held can't corrupt this map (a
    // plain sprite cache, not an invariant-bearing structure), so recover
    // the data instead of taking the whole render path down with it.
    let mut map = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cached_sprites(&mut map, (r, g, b))
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

/// Every pixel [`shadow`] can paint under `rect`, antialiasing included, in
/// `rect`'s coordinates; `None` when it paints nothing.
pub fn shadow_bounds(rect: Rect, token: &Shadow) -> Option<Rect> {
    if token.blur <= 0.0
        || rect.width() <= 0.0
        || rect.height() <= 0.0
        || token.color.components[3] <= 0.0
    {
        return None;
    }
    // Vello paints the blur inside the rect grown by its kernel, 2.5
    // standard deviations; one more pixel covers the shape's antialiasing.
    let reach = 2.5 * std_dev_for(token.blur) + 1.0;
    let offset = f64::from(token.offset_y);
    let shadow_rect = Rect::new(rect.x0, rect.y0 + offset, rect.x1, rect.y1 + offset);
    Some(shadow_rect.inflate(reach, reach))
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

/// The regions a window is made of (HIG §9.2). Each has one ground, and
/// the framework paints it: an app declares which region an area is (a
/// `NavShell` slot, or [`region`] in a painted app) and never fills it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    /// The window ground, `surfaces.content`: every app's background.
    Content,
    /// The navigation column at the leading edge, `surfaces.sidebar`.
    Sidebar,
    /// The detail column at the trailing edge, `surfaces.inspector`.
    Inspector,
}

/// Paint `region`'s ground over `rect` and return it, for the region's
/// contents to measure against ([`Theme::on_ground`],
/// [`Colors::lift_on`], [`edge_on`]).
///
/// A region is told apart from the content beside it by its tone. Where a
/// material gives the two the same ground (high contrast), the region draws
/// the hairline instead, on the edge it shares with content: the trailing
/// edge of a sidebar, the leading edge of an inspector.
pub fn region(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    region: Region,
    theme: &Theme,
) -> Color {
    let ground = theme.region(region);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return ground;
    }
    scene.fill(Fill::NonZero, transform, ground, None, &rect);
    let content = theme.tokens.colors.surfaces.content;
    let shared_edge = match region {
        Region::Content => None,
        Region::Sidebar => Some(rect.x1.round() - 1.0),
        Region::Inspector => Some(rect.x0.round()),
    };
    if let Some(x) = shared_edge
        && region_needs_edge(ground, content)
    {
        let rule = Rect::new(x, rect.y0, x + 1.0, rect.y1);
        scene.fill(Fill::NonZero, transform, edge_on(ground), None, &rule);
    }
    ground
}

/// Whether a region tier is too close to the content ground to separate
/// from it by tone alone.
#[must_use]
pub fn region_needs_edge(ground: Color, content: Color) -> bool {
    let l = |c: Color| f64::from(crate::color::Oklch::from_color(c).l);
    (l(ground) - l(content)).abs() < crate::tokens::colors::REGION_MIN_STEP
}

/// Alphas [`edge_on`] tries, ascending, until the hairline visibly moves the
/// surface it is drawn on (HIG §9.5). Step 0 is the control hairline
/// (`chrome::shell::HAIRLINE_ALPHA`); the ladder only climbs on grounds so
/// dark or so light that a 26-alpha wash lands within rounding distance of
/// the surface — relative luminance compresses at the dark end, so pure
/// black needs more alpha for the same visible step.
pub const EDGE_ALPHA_STEPS: &[u8] = &[26, 40, 56, 72, 92];

/// How far `edge` moves `surface` once painted on it, in relative-luminance
/// units — the measurement HIG §9.5 asks every chrome border to pass. Uses
/// the same compositor as `Colors::hairline_on` / `lift_target` /
/// `window_edge`, so one rounding model decides "visible" everywhere.
#[must_use]
pub fn edge_delta(edge: Color, surface: Color) -> f64 {
    use crate::a11y::relative_luminance;
    let painted = crate::tokens::colors::composite(edge, surface);
    (relative_luminance(painted) - relative_luminance(surface)).abs()
}

/// The derived 1 px rest edge of a control, card or row: a hairline
/// measured against the surface it is painted on, never a declared
/// `border_standard` (which is one fixed colour per theme and disappears on
/// a raised dark surface). Walks [`EDGE_ALPHA_STEPS`] until the edge
/// visibly registers (`tokens::colors::HAIRLINE_MIN_DELTA`).
#[must_use]
pub fn edge_on(surface: Color) -> Color {
    let key = u32::from_be_bytes([surface.r(), surface.g(), surface.b(), surface.a()]);
    if let Some(edge) = EDGE_MEMO.with(|memo| {
        memo.borrow()
            .entries
            .iter()
            .flatten()
            .find(|(k, _)| *k == key)
            .map(|(_, edge)| *edge)
    }) {
        return edge;
    }
    let edge = derive_edge_on(surface);
    EDGE_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        let slot = memo.next;
        memo.entries[slot] = Some((key, edge));
        memo.next = (slot + 1) % EDGE_MEMO_SLOTS;
    });
    edge
}

/// The uncached ladder walk behind [`edge_on`].
fn derive_edge_on(surface: Color) -> Color {
    use crate::a11y::relative_luminance;
    let ground = relative_luminance(surface);
    let mut last = Color::TRANSPARENT;
    for alpha in EDGE_ALPHA_STEPS {
        last = Colors::hairline_on(surface, *alpha);
        let painted = crate::tokens::colors::composite(last, surface);
        if (relative_luminance(painted) - ground).abs() >= crate::tokens::colors::HAIRLINE_MIN_DELTA
        {
            return last;
        }
    }
    last
}

/// A frame asks for the edge of the same three or four theme constants
/// hundreds of times (every row, card and rule), and each derivation is a
/// handful of `powf` calls per ladder step. `edge_on` is a pure function of
/// its 8-bit input, so the last few answers are kept per thread.
struct EdgeMemo {
    entries: [Option<(u32, Color)>; EDGE_MEMO_SLOTS],
    next: usize,
}

const EDGE_MEMO_SLOTS: usize = 8;

thread_local! {
    static EDGE_MEMO: std::cell::RefCell<EdgeMemo> = const {
        std::cell::RefCell::new(EdgeMemo {
            entries: [None; EDGE_MEMO_SLOTS],
            next: 0,
        })
    };
}

/// [`edge_on`] at a fixed alpha, for callers that want the control hairline
/// (`HAIRLINE_ALPHA`) or the panel edge (`PANEL_BORDER_ALPHA`) exactly.
#[must_use]
pub fn edge_on_with(surface: Color, alpha: u8) -> Color {
    Colors::hairline_on(surface, alpha)
}

/// The outline a crisp 1 px edge is stroked on: half a pixel inside `rect`,
/// so a 1 px stroke covers exactly the outermost pixel row of the surface
/// and never bleeds onto the ground beside it.
#[must_use]
pub fn edge_outline(rect: Rect, radius: f64) -> RoundedRect {
    RoundedRect::from_rect(rect.inset(-0.5), (radius - 0.5).max(0.0))
}

/// Whole logical pixels for an edged surface ([`crate::paint::snap_rect`]).
fn snap(rect: Rect) -> Rect {
    crate::paint::snap_rect(rect)
}

/// Fill a card / row / tile and give it its derived rest edge in one call —
/// the one way an app or shell surface paints an edged surface. The edge
/// derives from `fill` itself: a stroke that sits entirely on the surface
/// needs no knowledge of what is behind it.
pub fn card(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, fill: Color) {
    let rect = snap(rect);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    scene.fill(
        Fill::NonZero,
        transform,
        fill,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        edge_on(fill),
        None,
        &edge_outline(rect, radius),
    );
}

/// [`card`] for a translucent `fill` (an accent wash, a hover tint): the
/// edge derives from what the eye sees — `fill` composited over `ground` —
/// not from the wash's own RGB. A 16 % accent wash on a light panel is
/// still a light surface and wants a dark hairline; judged by the accent
/// alone it would get a white one and vanish.
pub fn card_on(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    fill: Color,
    ground: Color,
) {
    let rect = snap(rect);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let seen = crate::tokens::colors::composite(fill, ground);
    scene.fill(
        Fill::NonZero,
        transform,
        fill,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        edge_on(seen),
        None,
        &edge_outline(rect, radius),
    );
}

/// A floating Panel-role surface (menu, popover, sheet, toast) in one call:
/// the floating material ([`crate::floating_surface::Material::Floating`],
/// the blurred backdrop under a panel-ground tint, opaque where blur is
/// unavailable) with the [`crate::chrome::panel_border`] edge on a crisp
/// inset outline — HIG §9.5's Panel row, so an app never spells the fill +
/// stroke pair itself. Rows inside it use [`separator`] with an `s3` inset
/// and [`crate::chrome::selection_tier_on`] against the panel ground.
pub fn panel(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, theme: &Theme) {
    let rect = snap(rect);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    crate::floating_surface::paint_material(
        scene,
        transform,
        &RoundedRect::from_rect(rect, radius),
        crate::floating_surface::Material::Floating,
        theme,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        crate::chrome::panel_border(theme),
        None,
        &edge_outline(rect, radius),
    );
    floating_edge(scene, transform, rect, radius, theme);
}

/// The 1 px light edge a floating surface carries just inside its outline
/// on a dark material ([`crate::tokens::Elevation::floating_edge`]); nothing
/// on a light one. Paint it after the surface's body and border, at the
/// surface's own radius: the edge's radius is concentric with it.
pub fn floating_edge(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, theme: &Theme) {
    let edge = theme.tokens.elevation.floating_edge;
    if edge.a() == 0 {
        return;
    }
    const INSET: f64 = 1.5;
    let inner = snap(rect).inset(-INSET);
    if inner.width() <= 0.0 || inner.height() <= 0.0 {
        return;
    }
    let radii = &theme.tokens.radii;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a corner radius in logical pixels; f32 holds it exactly at UI scales"
    )]
    let inner_radius = radii.concentric(radius as f32, INSET as f32);
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        edge,
        None,
        &RoundedRect::from_rect(inner, f64::from(inner_radius)),
    );
}

/// The boundary of authored content, independent of the pixels inside it.
#[derive(Clone, Copy, Debug)]
pub enum ContentFrame {
    /// A square sheet on the canvas ground.
    Sheet,
    /// A thumbnail whose selected edge sits outside the image.
    Thumbnail { selected: bool },
}

impl ContentFrame {
    /// Paint only the frame, preserving every pixel of the content itself.
    pub fn draw(self, scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px).max(1.0);
        let (radius, selected) = match self {
            Self::Sheet => (0.0, false),
            Self::Thumbnail { selected } => (f64::from(theme.tokens.radii.item), selected),
        };
        let (width, inset, color) = if selected {
            (rule * 2.0, rule, theme.tokens.colors.text_primary)
        } else {
            (rule, 0.0, edge_on(theme.tokens.colors.surfaces.content))
        };
        scene.stroke(
            &Stroke::new(width),
            transform,
            color,
            None,
            &rect.inflate(inset, inset).to_rounded_rect(radius + inset),
        );
    }
}

/// A 1 px separator between rows inside a surface: derived against
/// `surface`, inset `inset` from both ends (HIG §9.5: `spacing.s3`, never
/// full-bleed into a corner radius), centred on `y`.
pub fn separator(
    scene: &mut Scene,
    transform: Affine,
    x0: f64,
    x1: f64,
    y: f64,
    inset: f64,
    surface: Color,
) {
    let Some(rule) = separator_rect(x0, x1, y, inset) else {
        return;
    };
    scene.fill(Fill::NonZero, transform, edge_on(surface), None, &rule);
}

/// A 1 px vertical separator between two columns or panes, derived against
/// `surface` like [`separator`], inset `inset` from both ends and centred
/// on `x`. The column rule of a painted split, a gutter beside a list.
pub fn vertical_separator(
    scene: &mut Scene,
    transform: Affine,
    x: f64,
    y0: f64,
    y1: f64,
    inset: f64,
    surface: Color,
) {
    let (a, b) = (y0 + inset, y1 - inset);
    if b <= a {
        return;
    }
    let left = (x - 0.5).round();
    scene.fill(
        Fill::NonZero,
        transform,
        edge_on(surface),
        None,
        &Rect::new(left, a, left + 1.0, b),
    );
}

/// The edge of a viewport that content can scroll in from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentEdge {
    Top,
    Bottom,
    Leading,
    Trailing,
}

/// How far a [`content_edge_fade`] reaches into the viewport, at most a
/// third of it.
const CONTENT_FADE_PX: f64 = 24.0;

/// Fade content out toward `edge` of the viewport `rect`: the `ground` the
/// content is drawn on, opaque at the edge and clear a short way in, eased
/// so it has no start line. Paint it over the content, and only while
/// more content can scroll in from that edge (a `Scrollable` with
/// `with_edge_fade` does both). The replacement for a media scrim, a
/// `DestOut` mask or a hand-made gradient at the end of a list.
pub fn content_edge_fade(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    edge: ContentEdge,
    ground: Color,
) {
    let along = match edge {
        ContentEdge::Top | ContentEdge::Bottom => rect.height(),
        ContentEdge::Leading | ContentEdge::Trailing => rect.width(),
    };
    let reach = CONTENT_FADE_PX.min(along / 3.0);
    if reach <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let (band, strong, clear) = match edge {
        ContentEdge::Top => (
            Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + reach),
            (rect.x0, rect.y0),
            (rect.x0, rect.y0 + reach),
        ),
        ContentEdge::Bottom => (
            Rect::new(rect.x0, rect.y1 - reach, rect.x1, rect.y1),
            (rect.x0, rect.y1),
            (rect.x0, rect.y1 - reach),
        ),
        ContentEdge::Leading => (
            Rect::new(rect.x0, rect.y0, rect.x0 + reach, rect.y1),
            (rect.x0, rect.y0),
            (rect.x0 + reach, rect.y0),
        ),
        ContentEdge::Trailing => (
            Rect::new(rect.x1 - reach, rect.y0, rect.x1, rect.y1),
            (rect.x1, rect.y0),
            (rect.x1 - reach, rect.y0),
        ),
    };
    const STOPS: usize = 7;
    let stops: [vello::peniko::ColorStop; STOPS] = std::array::from_fn(|i| {
        #[allow(clippy::cast_precision_loss, reason = "i < STOPS, a single digit")]
        let t = i as f32 / (STOPS - 1) as f32;
        vello::peniko::ColorStop {
            offset: t,
            color: ground
                .with_alpha(crate::overlay::scrim::fade_alpha(f64::from(t), 1.0))
                .into(),
        }
    });
    let gradient = vello::peniko::Gradient::new_linear(strong, clear).with_stops(stops.as_slice());
    scene.fill(Fill::NonZero, transform, &gradient, None, &band);
}

/// The one device-pixel row a separator centred on `y` occupies: `y` is
/// snapped so a caller passing a whole-pixel coordinate gets a crisp rule,
/// not a two-row smear at half alpha (which would fall under
/// `HAIRLINE_MIN_DELTA` even though the colour itself registers).
#[must_use]
pub fn separator_rect(x0: f64, x1: f64, y: f64, inset: f64) -> Option<Rect> {
    let (a, b) = (x0 + inset, x1 - inset);
    if b <= a {
        return None;
    }
    let top = (y - 0.5).round();
    Some(Rect::new(a, top, b, top + 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edge_fade_is_one_gradient_inside_its_edge_band() {
        let ground = Theme::dark().tokens.colors.surfaces.content;
        for edge in [
            ContentEdge::Top,
            ContentEdge::Bottom,
            ContentEdge::Leading,
            ContentEdge::Trailing,
        ] {
            let mut scene = Scene::new();
            content_edge_fade(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 300.0, 200.0),
                edge,
                ground,
            );
            let tags = &scene.encoding().draw_tags;
            assert_eq!(tags.len(), 1, "{edge:?}");
            assert!(
                tags[0] == vello_encoding::DrawTag::LINEAR_GRADIENT,
                "{edge:?}"
            );
        }
        // A viewport too small to fade paints nothing.
        let mut scene = Scene::new();
        content_edge_fade(
            &mut scene,
            Affine::IDENTITY,
            Rect::ZERO,
            ContentEdge::Top,
            ground,
        );
        assert!(scene.encoding().draw_tags.is_empty());
    }

    #[test]
    fn a_vertical_separator_is_one_crisp_column() {
        let mut scene = Scene::new();
        let surface = Theme::light().tokens.colors.surfaces.content;
        vertical_separator(&mut scene, Affine::IDENTITY, 100.3, 0.0, 50.0, 4.0, surface);
        assert_eq!(scene.encoding().draw_tags.len(), 1);
        let mut empty = Scene::new();
        vertical_separator(&mut empty, Affine::IDENTITY, 10.0, 0.0, 6.0, 4.0, surface);
        assert!(
            empty.encoding().draw_tags.is_empty(),
            "inset past its length"
        );
        assert_eq!(
            crate::paint::snap_rect(Rect::new(0.4, 1.6, 10.5, 9.49)),
            Rect::new(0.0, 2.0, 11.0, 9.0)
        );
    }

    #[test]
    fn edges_and_chips_register_on_every_preset_surface() {
        // Case `edge_on_registers_on_every_preset_surface`.
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let s = &theme.tokens.colors.surfaces;
                for (name, ground) in [
                    ("base", s.base),
                    ("raised", s.raised),
                    ("overlay", s.overlay),
                    ("panel_ground", crate::chrome::shell::panel_ground(&theme)),
                ] {
                    let edge = edge_on(ground);
                    let moved = edge_delta(edge, ground);
                    assert!(
                        moved >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
                        "{name} edge on {:?} moved only {moved:.4} ({edge:?})",
                        theme.name
                    );
                }
            }
            // The declared border is what vanished on the dark preset: derived
            // must register there where declared does not.
            let dark = Theme::dark();
            let raised = dark.tokens.colors.surfaces.raised;
            let declared = edge_delta(dark.tokens.colors.border_standard, raised);
            let derived = edge_delta(edge_on(raised), raised);
            assert!(
                declared < crate::tokens::colors::HAIRLINE_MIN_DELTA,
                "dark border_standard on raised was the invisible edge ({declared:.4}); if this \
             changed, the ratchet's rationale needs revisiting"
            );
            assert!(
                derived > declared,
                "derived {derived:.4} vs declared {declared:.4}"
            );
        }

        // Case `edge_on_starts_at_the_control_hairline_where_that_already_registers`.
        {
            // A mid grey moves plenty at alpha 26: the ladder must not over-darken it.
            let grey = Color::from_rgb8(0x80, 0x80, 0x80);
            assert_eq!(
                edge_on(grey),
                Colors::hairline_on(grey, EDGE_ALPHA_STEPS[0])
            );
        }

        // Case `a_translucent_wash_takes_its_edge_from_the_composited_surface`.
        {
            // A 16 % accent wash over the light panel is still a light surface.
            for theme in [Theme::light(), Theme::dark()] {
                let ground = theme.tokens.colors.surfaces.raised;
                let wash = theme.tokens.colors.text_primary.with_alpha(0.16);
                let seen = crate::tokens::colors::composite(wash, ground);
                let derived = edge_on(seen);
                assert!(
                    edge_delta(derived, seen) >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
                    "card_on edge must register on the seen surface"
                );
                // Judged by the wash's own RGB the hairline can flip polarity and
                // stop registering on the surface the eye sees.
                let naive = edge_on(wash);
                assert!(
                    edge_delta(derived, seen) >= edge_delta(naive, seen),
                    "composited derivation must never register worse than the naive one"
                );
            }
        }

        // Case `a_separator_occupies_one_whole_pixel_row`.
        {
            // A whole-pixel centre and a half-pixel centre both land on one row.
            let whole = separator_rect(10.0, 200.0, 100.0, 12.0).expect("rule");
            assert_eq!((whole.x0, whole.x1), (22.0, 188.0));
            assert_eq!((whole.y0, whole.y1), (100.0, 101.0));
            let half = separator_rect(0.0, 50.0, 99.5, 0.0).expect("rule");
            assert_eq!((half.y0, half.y1), (99.0, 100.0));
            assert!(
                separator_rect(0.0, 20.0, 5.0, 12.0).is_none(),
                "insets that meet paint nothing"
            );
        }

        // Case `edge_on_memo_returns_the_derived_answer`.
        {
            // The memo must be transparent: the same surface asked twice (and
            // many distinct surfaces cycling through the slots) always yields
            // exactly what the ladder derives.
            let theme = Theme::dark();
            let surfaces = [
                theme.tokens.colors.surfaces.base,
                theme.tokens.colors.surfaces.raised,
                theme.tokens.colors.surfaces.overlay,
                theme.tokens.colors.lift(0.35),
                theme.tokens.colors.lift(0.5),
                Theme::light().tokens.colors.surfaces.base,
                Theme::light().tokens.colors.surfaces.raised,
                Theme::high_contrast().tokens.colors.surfaces.base,
                Theme::high_contrast().tokens.colors.surfaces.raised,
                Color::from_rgb8(120, 60, 200),
                Color::from_rgb8(255, 255, 255),
                Color::from_rgb8(0, 0, 0),
            ];
            for _ in 0..3 {
                for surface in surfaces {
                    assert_eq!(edge_on(surface), derive_edge_on(surface));
                }
            }
        }

        // Case `panel_edge_registers_on_the_panel_ground_in_every_preset`.
        {
            for (name, theme) in [
                ("dark", Theme::dark()),
                ("light", Theme::light()),
                ("high-contrast", Theme::high_contrast()),
            ] {
                let ground = crate::chrome::panel_ground(&theme);
                let border = crate::chrome::panel_border(&theme);
                assert!(
                    edge_delta(border, ground) >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{name}: the panel border must register on the panel ground"
                );
            }
            // Degenerate rects paint nothing rather than a stray hairline.
            let mut scene = Scene::new();
            panel(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(10.0, 10.0, 10.0, 40.0),
                8.0,
                &Theme::dark(),
            );
            assert!(
                scene.encoding().is_empty(),
                "an empty panel encodes no draw ops"
            );
        }

        // Case `card_edge_is_one_pixel_inside_rect`.
        {
            let rect = Rect::new(10.0, 20.0, 110.0, 60.0);
            let outline = edge_outline(rect, 12.0);
            assert_eq!(outline.rect(), Rect::new(10.5, 20.5, 109.5, 59.5));
            assert!((outline.radii().top_left - 11.5).abs() < 1e-9);
            // A radius under half a pixel does not go negative.
            assert!(edge_outline(rect, 0.0).radii().top_left.abs() < 1e-9);
        }

        // Case `quiet_chip_edge_registers_on_every_preset`:
        // The quiet chip's edge is derived from its own fill, so it registers
        // on every preset — the declared `border_standard` it replaces measured
        // under the visibility floor on the dark preset's raised surface.
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let quiet = crate::widget::ChipStyle::Neutral.colors(
                    &theme,
                    theme.tokens.colors.surfaces.content,
                    false,
                    false,
                );
                let edge = quiet.edge.expect("quiet chip has an edge");
                assert_eq!(
                    edge,
                    edge_on(quiet.fill),
                    "{}: edge derives from the fill",
                    theme.name
                );
                assert!(
                    edge_delta(edge, quiet.fill) >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{}: quiet chip edge moved only {:.4}",
                    theme.name,
                    edge_delta(edge, quiet.fill)
                );
            }
        }
    }

    #[test]
    fn shadow_sprites_are_cached_bounded_and_circular() {
        // Case `sprite_is_cached_per_tint`.
        {
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

        // Case `sprite_cache_is_bounded_and_evicted_sprites_remain_alive`.
        {
            let mut cache = HashMap::new();
            let retained = cached_sprites(&mut cache, (0, 0, 0));
            for tint in 1..=SHADOW_SPRITE_CACHE_LIMIT {
                let channel = u8::try_from(tint).unwrap_or(u8::MAX);
                let _ = cached_sprites(&mut cache, (channel, channel, channel));
            }
            assert_eq!(cache.len(), SHADOW_SPRITE_CACHE_LIMIT);
            assert_eq!(retained.corner.width, SHADOW_SPRITE_SIZE);
            assert_eq!(retained.corner.height, SHADOW_SPRITE_SIZE);
        }

        // Case `edge_sprite_runs_from_zero_through_the_peak_back_to_zero`:
        // Ends of the band are zero, and the peak sits where
        // [`SHADOW_PEAK_RADIUS`] says.
        {
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

        // Case `corner_sprite_joins_the_edge_profile_where_the_slices_meet`:
        // The corner sprite must equal the edge profile along the two
        // rows/columns where it meets an edge slice — the join is continuous by
        // construction.
        {
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

        // Case `corner_sprite_contours_are_circular`:
        // The corner's iso-alpha contours are circles about the corner
        // point, not the elliptical contours a stretched radial sprite
        // would produce.
        {
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
    }

    fn alpha_at(img: &Image, x: u32, y: u32) -> u8 {
        let idx = y
            .saturating_mul(img.width)
            .saturating_add(x)
            .saturating_mul(4)
            .saturating_add(3);
        img.data.data()[idx as usize]
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

    #[test]
    fn token_shadow_is_even_and_follows_its_alpha_and_blur() {
        // Case `token_shadow_is_even_around_a_rounded_corner`:
        // A shadow is as even around a rounded corner as along a flat edge,
        // since it's a blur of the surface's own outline. Sampling is at
        // equal *normal* distance from the silhouette — out from the flat
        // edge, and out along the corner arc's diagonal.
        {
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

        // Case `token_shadow_honours_the_token_alpha`:
        // The token's colour alpha is the shadow's strength; unlike
        // [`soft_shadow`], [`shadow`] actually reads it.
        {
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

        // Case `token_shadow_reach_follows_blur`:
        // A token's visible reach is set by its `blur`, through the one
        // shared mapping ([`std_dev_for`]).
        {
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

    #[test]
    fn soft_shadow_falloff_is_seamless_circular_and_spread_sized() {
        // Case `soft_shadow_peak_matches_sprite_envelope`:
        // The composited shadow has to reach the strength the sprite's
        // alpha envelope describes. A full-intensity white shadow peaks at
        // `bump · inner · outer = 1 · 1 · (1 - SHADOW_PEAK_RADIUS) = 0.7`,
        // so over black the brightest pixel on the ring is `255 · 0.7 ≈
        // 178`. If the premultiplied sprite were handed to the renderer as
        // straight alpha, the tint would get premultiplied twice and that
        // peak would collapse to `255 · 0.7² ≈ 125` — this pins against that.
        //
        // The ring's position is asserted too, measured from the rect's
        // **edge**: the envelope peaks `(1 - 2·SHADOW_PEAK_RADIUS)` of a
        // `spread` inside the edge and dies exactly one `spread` outside it,
        // whatever the rect's size.
        {
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

        // Case `soft_shadow_falloff_width_is_the_spread_not_the_rects_size`:
        // The falloff outside an edge is a function of the distance to that
        // edge and of `spread` — never of the rect's own dimensions. Wide,
        // tall and small-square must all match [`shadow_profile`] pixel for
        // pixel, on both axes.
        {
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

        // Case `soft_shadow_corners_stay_circular_on_a_wide_rect`:
        // A corner's falloff is a `spread`-radius quarter circle: alpha a
        // given distance diagonally out from the corner matches alpha the
        // same distance straight out from an edge — on a wide rect too.
        {
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

        // Case `soft_shadow_is_uniform_along_an_edge_and_seamless_at_the_slice_joins`:
        // Along one edge the shadow is constant, including across the
        // boundaries where the edge slice meets the two corner slices — any
        // step there would show as a hairline running out of the corner.
        {
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

        // Case `soft_shadow_leaves_a_wide_rects_interior_clear`:
        // A wide card's interior stays completely clear — the centre slice
        // is skipped because the envelope is zero more than one band in.
        {
            let spread = 40.0;
            let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
            let at = render_shadow(1400, 900, rect, spread);
            for fx in [0.25_f64, 0.5, 0.75] {
                let x = rect.x0 + rect.width() * fx;
                let y = rect.center().y;
                assert_eq!(at(x, y), 0, "the interior at ({x}, {y}) must be clear");
            }
        }

        // Case `degenerate_inputs_are_noops`.
        {
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

        // Case `a_pill_thinner_than_the_spread_keeps_the_full_falloff`:
        // A pill shorter than `2 · spread` still gets the full outward
        // falloff: opposing slices share the interior instead of squashing
        // the profile. The toolkit's tab/close/restore pills are this shape
        // (26 px tall against a 10 px spread).
        {
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
}
