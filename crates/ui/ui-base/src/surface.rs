//! Shared surface-drawing primitives — how a card/panel casts a shadow. [`shadow`] is the
//! default: a true Gaussian blur of the surface's own rounded outline, so the shadow is
//! the same shape as the thing casting it and equally dense the whole way round.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill, ImageBrush, ImageData as Image};

use crate::ColorCompat;
use crate::theme::Theme;
use crate::tokens::{Colors, Shadow};
use tilo_lock::MutexRecover as _;

/// Shared silhouette for lightweight shell floats. The asymmetry is only one or two
/// logical pixels along each edge: enough to feel soft and held, never enough to look
/// like a novelty blob.
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

/// Resolution of the baked falloff along the profile axis: 256 samples across a band at
/// most a couple hundred logical px wide is plenty.
const SHADOW_SPRITE_SIZE: u32 = 256;
/// Row count of the edge sprite: every row is an identical profile, but keeping a few
/// (not one) leaves the image non-degenerate for the sampler, at a cost of 4 KB.
const SHADOW_EDGE_ROWS: u32 = 4;
const SHADOW_GAUSSIAN_K: f64 = 0.35;
/// Where the envelope peaks, as a fraction of the band it is baked across (the band runs
/// one `spread` inside the rect's edge to one `spread` outside).
const SHADOW_PEAK_RADIUS: f64 = 0.30;

/// The shadow's alpha as a function of the normalised signed distance `u` from the rect's
/// edge: `-1` is one band inside the rect, `0` is exactly on the edge, `+1` is one
/// `spread` outside.
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

/// Signed distance to the rect, in band units, at a corner — the rounded-box SDF with a
/// zero corner radius.
fn corner_u(ux: f64, uy: f64) -> f64 {
    let ox = ux.max(0.0);
    let oy = uy.max(0.0);
    (ox * ox + oy * oy).sqrt() + ux.max(uy).min(0.0)
}

/// Map a texel index onto its band coordinate in `-1..=1`, sampling at pixel centres so
/// the brush's half-texel offset doesn't skew the baked profile against the geometry it
/// is stretched over.
fn band_coord(i: u32, n: u32) -> f64 {
    2.0 * (f64::from(i) + 0.5) / f64::from(n) - 1.0
}

/// The pair of premultiplied-RGBA sprites one tint needs. The channels are written
/// premultiplied (`c * a`), so both [`Image`]s must declare
/// [`ImageAlphaType::AlphaPremultiplied`].
struct ShadowSprites {
    /// `SHADOW_SPRITE_SIZE × SHADOW_EDGE_ROWS`: the 1-D profile along x (inner end at x =
    /// 0), constant down every column. Stretched along one axis only, so an edge's
    /// falloff width never depends on that edge's length.
    edge: Image,
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
    crate::raster::rgba8_premultiplied(data, width, height)
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

type ShadowSpriteCache = Mutex<HashMap<(u8, u8, u8), Arc<ShadowSprites>>>;

/// Each tint owns a 256² corner plus a small edge image (~260 KiB total). Theme surfaces
/// normally use only a handful of tints; 32 keeps those hot while bounding a
/// dynamic-colour caller to roughly 8.1 MiB of cached pixels.
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
    // A panic elsewhere while the lock was held can't corrupt this map (a plain sprite
    // cache, not an invariant-bearing structure), so recover the data instead of taking
    // the whole render path down with it.
    let mut map = cache.lock_recover();
    cached_sprites(&mut map, (r, g, b))
}

/// Map a sprite's `sw × sh` pixel rect onto `dest`, optionally mirroring either axis.
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

fn slice_xform_rot(dest: Rect, sw: f64, sh: f64, mirror_y: bool) -> Affine {
    let (b, f) = if mirror_y {
        (-dest.height() / sw, dest.y1)
    } else {
        (dest.height() / sw, dest.y0)
    };
    Affine::new([0.0, b, dest.width() / sh, 0.0, dest.x0, f])
}

/// Paint the part of a slice that falls inside `clip`. The shape is derived in `local`
/// space (axis-aligned by construction), so the bbox is exact regardless of the caller's
/// own `transform`.
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
    // Clip boundaries meet at the rect's mid-lines when narrower than `2 · spread`, so
    // opposing slices divide the interior, not overlap.
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

/// Paint the drop shadow an elevation token describes, following `rect`'s own corner
/// `radius` — a true Gaussian blur of the surface's own rounded outline, unlike
/// [`soft_shadow`]'s square-corner-SDF approximation.
pub fn shadow(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, token: &Shadow) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    if let Some(outline) = token.outline
        && outline.width > 0.0
        && outline.color.components[3] > 0.0
    {
        let half = f64::from(outline.width) / 2.0;
        scene.stroke(
            &Stroke::new(f64::from(outline.width)),
            transform,
            outline.color,
            None,
            &RoundedRect::from_rect(
                rect.inflate(half, half),
                radius.clamp(0.0, rect.width().min(rect.height()) / 2.0) + half,
            ),
        );
    }
    if token.blur <= 0.0 {
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

/// Every pixel [`shadow`] can paint under `rect`, antialiasing included, in `rect`'s
/// coordinates; `None` when it paints nothing.
pub fn shadow_bounds(rect: Rect, token: &Shadow) -> Option<Rect> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let outline = token
        .outline
        .filter(|outline| outline.width > 0.0 && outline.color.components[3] > 0.0)
        .map(|outline| {
            let reach = f64::from(outline.width) + 1.0;
            rect.inflate(reach, reach)
        });
    if token.blur <= 0.0 || token.color.components[3] <= 0.0 {
        return outline;
    }
    // Vello paints the blur inside the rect grown by its kernel, 2.5 standard deviations;
    // one more pixel covers the shape's antialiasing.
    let reach = 2.5 * std_dev_for(token.blur) + 1.0;
    let offset = f64::from(token.offset_y);
    let shadow_rect = Rect::new(rect.x0, rect.y0 + offset, rect.x1, rect.y1 + offset);
    let blurred = shadow_rect.inflate(reach, reach);
    Some(outline.map_or(blurred, |outline| outline.union(blurred)))
}

/// The Gaussian standard deviation a CSS-style `blur` radius means.
pub fn std_dev_for(blur: f32) -> f64 {
    f64::from(blur) / 2.0
}

/// Paint an elevation-token shadow under `rect`, square-cornered. For callers with no
/// radius to follow — anything rounded must call [`shadow`] instead, or it gets a shadow
/// of a different shape than itself.
pub fn elevation(scene: &mut Scene, transform: Affine, rect: Rect, token: &Shadow) {
    shadow(scene, transform, rect, 0.0, token);
}

/// The regions a window is made of (HIG §9.2). Each has one ground, and the framework
/// paints it: an app declares which region an area is (a `NavShell` slot, or [`region`]
/// in a painted app) and never fills it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    Content,
    Sidebar,
    Inspector,
}

/// Paint `region`'s ground over `rect` and return it, for the region's contents to
/// measure against ([`Theme::on_ground`], [`Colors::lift_on`], [`edge_on`]).
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
    if region == Region::Content {
        crate::part_paint::backdrop::fill_role(
            scene,
            transform,
            &RoundedRect::from_rect(rect, 0.0),
            crate::tokens::SurfaceRole::Window,
            theme,
            ground,
        );
    } else {
        scene.fill(Fill::NonZero, transform, ground, None, &rect);
    }
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

/// Whether a region tier is too close to the content ground to separate from it by tone
/// alone.
#[must_use]
pub fn region_needs_edge(ground: Color, content: Color) -> bool {
    let l = |c: Color| f64::from(crate::color::Oklch::from_color(c).l);
    (l(ground) - l(content)).abs() < crate::tokens::colors::REGION_MIN_STEP
}

/// Alphas [`edge_on`] tries, ascending, until the hairline visibly moves the surface it
/// is drawn on (HIG §9.5).
pub const EDGE_ALPHA_STEPS: &[u8] = &[26, 40, 56, 72, 92];

/// How far `edge` moves `surface` once painted on it, in relative-luminance units — the
/// measurement HIG §9.5 asks every chrome border to pass.
#[must_use]
pub fn edge_delta(edge: Color, surface: Color) -> f64 {
    use crate::a11y::relative_luminance;
    let painted = crate::tokens::colors::composite(edge, surface);
    (relative_luminance(painted) - relative_luminance(surface)).abs()
}

/// The derived 1 px rest edge of a control, card or row: a hairline measured against the
/// surface it is painted on, never a declared `border_standard` (which is one fixed
/// colour per theme and disappears on a raised dark surface).
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

/// A frame asks for the edge of the same three or four theme constants hundreds of times
/// (every row, card and rule), and each derivation is a handful of `powf` calls per
/// ladder step. `edge_on` is a pure function of its 8-bit input, so the last few answers
/// are kept per thread.
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

/// How far a rule that carries a colour of its own moves the ground it lands on: clearly
/// more than the neutral hairline beside it
/// ([`crate::tokens::colors::HAIRLINE_MIN_DELTA`]) does. A neutral rule divides two
/// things of one kind and says nothing by itself; a rule in the accent says "this is
/// where something starts", so it reads as a mark at a glance, never as another division.
pub const COLOURED_RULE_MIN_DELTA: f64 = crate::tokens::colors::HAIRLINE_MIN_DELTA * 3.0;

/// The 1 px rule `ink` draws when it is painted on `ground`: the ink's own hue, at the
/// smallest alpha that moves that ground by [`COLOURED_RULE_MIN_DELTA`] — clearly more
/// than the ground's own hairline ([`edge_on`]) moves it — or the ink whole where even a
/// full wash moves it less. A rule meant to carry a colour is derived against the ground
/// it lands on, never against itself, and is drawn at a weight a person reads without
/// looking for it.
#[must_use]
pub fn rule_of(ink: Color, ground: Color) -> Color {
    let [r, g, b, _] = ink.to_rgba8().to_u8_array();
    for alpha in [150, 190, 230, 255] {
        let wash = Color::from_rgba8(r, g, b, alpha);
        if edge_delta(wash, ground) >= COLOURED_RULE_MIN_DELTA {
            return wash;
        }
    }
    ink
}

/// The outline a crisp 1 px edge is stroked on: half a pixel inside `rect`, so a 1 px
/// stroke covers exactly the outermost pixel row of the surface and never bleeds onto the
/// ground beside it.
#[must_use]
pub fn edge_outline(rect: Rect, radius: f64) -> RoundedRect {
    RoundedRect::from_rect(rect.inset(-0.5), (radius - 0.5).max(0.0))
}

/// Whole logical pixels for an edged surface ([`crate::paint::snap_rect`]).
fn snap(rect: Rect) -> Rect {
    crate::paint::snap_rect(rect)
}

/// Fill a card / row / tile and give it its derived rest edge in one call — the one way
/// an app or shell surface paints an edged surface.
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

/// Clip what is painted next to the inside of a [`card`] at `rect` with `radius`, until
/// the caller's `scene.pop_layer()`: content held past the card's edge (a dragged marker,
/// a scrolled drawing) is cut there instead of spilling over what surrounds the card.
pub fn push_card_clip(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64) {
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        transform,
        &RoundedRect::from_rect(snap(rect), radius),
    );
}

/// [`card`] for a translucent `fill` (an accent wash, a hover tint): the edge derives
/// from what the eye sees — `fill` composited over `ground` — not from the wash's own
/// RGB.
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

/// A floating Panel-role surface (menu, popover, sheet, toast) in one call: the floating
/// material ([`crate::part_paint::material::Material::Floating`], the blurred backdrop under
/// a panel-ground tint, opaque where blur is unavailable) with its 1 px inner hairline
/// ([`floating_edge`]) — HIG §9.5's Panel row, so an app never spells the fill + stroke
/// pair itself. The shadow is [`floating_shadow`], painted first by the caller.
pub fn panel(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, theme: &Theme) {
    panel_unsnapped(scene, transform, snap(rect), radius, theme);
}

/// [`panel`] exactly where it is laid out, without snapping to the pixel grid: for chrome
/// whose own place is fractional or moving — a window's controls in a bar a strip
/// carries, a surface arriving on a spring.
pub fn panel_unsnapped(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &Theme,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    crate::part_paint::material::paint_material(
        scene,
        transform,
        &RoundedRect::from_rect(rect, radius),
        crate::part_paint::material::Material::Floating,
        theme,
    );
    floating_edge_unsnapped(scene, transform, rect, radius, theme);
}

/// The two-layer shadow of a floating surface: the tight contact layer that defines its
/// edge, under the wide layer that reads as distance
/// ([`crate::tokens::Elevation::contact`], [`crate::tokens::Elevation::floating`]), both
/// along the surface's own rounded outline.
pub fn floating_shadow(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    alpha: f32,
    theme: &Theme,
) {
    // A body tinted less than half shows what is below it nearly clear, so its own shadow
    // must not show through it: the shadow falls outside it only.
    let floating = theme.finish(crate::tokens::SurfaceRole::Floating);
    let clear = floating.translucent() && floating.tint() < CLEAR_TINT;
    let outside = clear.then(|| {
        let mut outside = BezPath::from_vec(
            vello::kurbo::Shape::path_elements(&rect.inflate(96.0, 96.0), 0.1).collect(),
        );
        outside.extend(vello::kurbo::Shape::path_elements(
            &RoundedRect::from_rect(rect, radius),
            0.1,
        ));
        outside
    });
    if let Some(outside) = &outside {
        scene.push_clip_layer(Fill::EvenOdd, transform, outside);
    }
    for layer in floating_lift(theme) {
        shadow(
            scene,
            transform,
            rect,
            radius,
            &layer.with_alpha_scaled(alpha),
        );
    }
    if outside.is_some() {
        scene.pop_layer();
    }
}

const CLEAR_TINT: f32 = 0.5;

/// The shadows [`floating_shadow`] paints, bottom first: the contact layer under the wide one.
#[must_use]
pub fn floating_lift(theme: &Theme) -> [crate::tokens::Shadow; 2] {
    let elevation = &theme.tokens.elevation;
    [elevation.contact, elevation.floating]
}

/// The 1 px inner hairline a floating surface carries on its outermost pixel row
/// ([`crate::chrome::panel_border`]): white at 7 % on a dark material, black at 8 % on a
/// light one, full strength on high contrast.
pub fn floating_edge(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, theme: &Theme) {
    floating_edge_unsnapped(scene, transform, snap(rect), radius, theme);
}

/// [`floating_edge`] exactly where the surface stands, without snapping it to the pixel
/// grid (see [`panel_unsnapped`]).
fn floating_edge_unsnapped(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &Theme,
) {
    if paint_border(
        scene,
        transform,
        &RoundedRect::from_rect(rect, radius),
        theme,
    ) {
        return;
    }
    let edge = crate::chrome::panel_border(theme);
    if edge.a() == 0 {
        return;
    }
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        edge,
        None,
        &edge_outline(rect, radius),
    );
}

/// How wide the selected edge is: twice the rest edge, so it reads as the same outline
/// made heavier.
#[must_use]
pub fn selected_edge_width(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.chrome_rule_px).max(1.0) * 2.0
}

/// The selected object's edge, in place of the rest edge [`card`] paints: at the same
/// position, a heavier line in the accent drawn wholly inside the surface's outline.
pub fn selected_edge(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, theme: &Theme) {
    let rect = snap(rect);
    let width = selected_edge_width(theme);
    if rect.width() <= width || rect.height() <= width {
        return;
    }
    let half = width / 2.0;
    scene.stroke(
        &Stroke::new(width),
        transform,
        theme.tokens.colors.intent.accent,
        None,
        &RoundedRect::from_rect(rect.inset(-half), (radius - half).max(0.0)),
    );
}

/// [`card`], or for the selected one its fill with the [`selected_edge`] in place of the
/// rest edge.
pub fn selectable_card(
    scene: &mut Scene,
    transform: Affine,
    (rect, radius): (Rect, f64),
    fill: Color,
    selected: bool,
    theme: &Theme,
) {
    if !selected {
        card(scene, transform, rect, radius, fill);
        return;
    }
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
    selected_edge(scene, transform, rect, radius, theme);
}

/// The boundary of authored content, independent of the pixels inside it.
#[derive(Clone, Copy, Debug)]
pub enum ContentFrame {
    Sheet,
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

/// A 1 px separator between rows inside a surface: derived against `surface`, inset
/// `inset` from both ends (HIG §9.5: `spacing.s3`, never full-bleed into a corner
/// radius), centred on `y`.
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

/// A 1 px vertical separator between two columns or panes, derived against `surface` like
/// [`separator`], inset `inset` from both ends and centred on `x`.
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

/// How far a [`content_edge_fade`] reaches into the viewport, at most a third of it.
pub(crate) const CONTENT_FADE_PX: f64 = 24.0;

/// Where [`content_edge_fade`] paints over the viewport `rect` toward `edge`: what a
/// scroller copies under it is not what the fade shows.
#[must_use]
pub fn content_edge_fade_band(rect: Rect, edge: ContentEdge) -> Rect {
    let along = match edge {
        ContentEdge::Top | ContentEdge::Bottom => rect.height(),
        ContentEdge::Leading | ContentEdge::Trailing => rect.width(),
    };
    let reach = CONTENT_FADE_PX.min(along / 3.0).max(0.0);
    match edge {
        ContentEdge::Top => Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + reach),
        ContentEdge::Bottom => Rect::new(rect.x0, rect.y1 - reach, rect.x1, rect.y1),
        ContentEdge::Leading => Rect::new(rect.x0, rect.y0, rect.x0 + reach, rect.y1),
        ContentEdge::Trailing => Rect::new(rect.x1 - reach, rect.y0, rect.x1, rect.y1),
    }
}

/// Fade content out toward `edge` of the viewport `rect`: the `ground` the content is
/// drawn on, opaque at the edge and clear a short way in, eased so it has no start line.
pub fn content_edge_fade(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    edge: ContentEdge,
    ground: Color,
) {
    let band = content_edge_fade_band(rect, edge);
    if band.width() <= 0.0 || band.height() <= 0.0 {
        return;
    }
    let (strong, clear) = match edge {
        ContentEdge::Top => ((rect.x0, rect.y0), (rect.x0, band.y1)),
        ContentEdge::Bottom => ((rect.x0, rect.y1), (rect.x0, band.y0)),
        ContentEdge::Leading => ((rect.x0, rect.y0), (band.x1, rect.y0)),
        ContentEdge::Trailing => ((rect.x1, rect.y0), (band.x0, rect.y0)),
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

/// The one device-pixel row a separator centred on `y` occupies: `y` is snapped so a
/// caller passing a whole-pixel coordinate gets a crisp rule, not a two-row smear at half
/// alpha (which would fall under `HAIRLINE_MIN_DELTA` even though the colour itself
/// registers).
#[must_use]
pub fn separator_rect(x0: f64, x1: f64, y: f64, inset: f64) -> Option<Rect> {
    let (a, b) = (x0 + inset, x1 - inset);
    if b <= a {
        return None;
    }
    let top = (y - 0.5).round();
    Some(Rect::new(a, top, b, top + 1.0))
}

/// The border every surface carries under the theme's shape role, as `(width, ink)`, or
/// `None` where the theme keeps the design language's own hairlines
/// ([`crate::tokens::BorderInk::Hairline`]).
#[must_use]
pub fn border(theme: &Theme) -> Option<(f64, Color)> {
    let ink = theme.tokens.border_color?;
    Some((f64::from(theme.tokens.border.width), ink))
}

/// Stroke the theme's border ([`border`]) wholly inside `body`. Returns whether one was
/// drawn, so a caller drops the hairline it stands in for.
pub fn paint_border(
    scene: &mut Scene,
    transform: Affine,
    body: &RoundedRect,
    theme: &Theme,
) -> bool {
    let Some((width, ink)) = border(theme) else {
        return false;
    };
    let rect = body.rect();
    if rect.width() <= width * 2.0 || rect.height() <= width * 2.0 {
        return false;
    }
    let half = width / 2.0;
    scene.stroke(
        &Stroke::new(width),
        transform,
        ink,
        None,
        &RoundedRect::from_rect(rect.inset(-half), (body.radii().top_left - half).max(0.0)),
    );
    true
}

/// Paint a control's face (a button, a field's well, a segmented track, a toggle's body)
/// with the control role's painter set (`crate::part_paint::Part::ControlFace`), then the
/// theme's border around it.
pub fn control_face(
    scene: &mut Scene,
    transform: Affine,
    body: &RoundedRect,
    fill: Color,
    theme: &Theme,
) -> bool {
    use crate::part_paint::{Moment, Part, States, builtin, paint_part, part_tokens};
    use crate::tokens::SurfaceRole;
    if fill.components[3] <= 0.0 {
        return false;
    }
    let rest = fill == theme.tokens.colors.surfaces.raised;
    let mut tokens = part_tokens(
        theme,
        SurfaceRole::Control,
        (fill, theme.tokens.colors.text_primary, Color::TRANSPARENT),
        theme.tokens.colors.surfaces.content,
    );
    tokens.radius = crate::num::f32_of_f64(body.radii().top_left);
    let moment = Moment::settled(if rest { States::REST } else { States::SELECTED });
    let part = (Part::ControlFace, SurfaceRole::Control);
    let rect = body.rect();
    let set = theme.painter(SurfaceRole::Control);
    if paint_part(
        scene,
        transform,
        rect,
        (set, part.0, part.1),
        moment,
        &tokens,
        theme,
    )
    .is_none()
    {
        paint_part(
            scene,
            transform,
            rect,
            (builtin::default_set(), part.0, part.1),
            moment,
            &tokens,
            theme,
        );
    }
    paint_border(scene, transform, body, theme)
}

