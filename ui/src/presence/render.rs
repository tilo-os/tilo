use super::PresenceCtx;
use crate::color_compat::ColorCompat;
use crate::permissions::UserId;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use vello::{
    Scene,
    kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke},
    peniko::{Color, Fill},
};

/// How much pigment a "just typed" marker lays down over the text.
/// [`MARKER_PIGMENT_L`] pins pigment to a fixed luminance first, so raising
/// this makes the marker easier to see without the text paying for it.
/// Measured: at 0.5 the band read 1.22:1 against paper; at 0.7, 1.79:1.
const MARKER_ALPHA: f32 = 0.7;

/// Luminance every marker pigment is normalised to. See [`marker_pigment`].
const MARKER_PIGMENT_L: f32 = 0.30;

/// Relative luminance (WCAG), from sRGB.
fn relative_luminance(c: Color) -> f32 {
    let c = c.to_rgba8();
    let f = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
}

/// A peer's colour re-tuned to a common marker luminance, keeping its hue.
///
/// A fixed alpha over [`super::PEER_PALETTE`] isn't even — the palette spans
/// a 4x luminance range, so a wash reads ~1.1:1..1.9:1 against paper
/// depending on peer. Scaling in linear light to a fixed luminance fixes
/// that (measured 1.39:1/1.09:1 -> 1.79:1/1.65:1) while hue survives
/// exactly. Only the marker uses this; carets/pointers/chips/selections
/// keep the peer's own colour.
fn marker_pigment(c: Color) -> Color {
    let l = relative_luminance(c);
    if l <= f32::EPSILON {
        return c;
    }
    let k = MARKER_PIGMENT_L / l;
    let rgba = c.to_rgba8();
    // `enc` is clamped to 0.0..=1.0 just above, so the scaled-and-rounded
    // result is always 0.0..=255.0 — always in `u8` range.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=1.0 above, so the rounded result is 0.0..=255.0"
    )]
    let ch = |v: u8| -> u8 {
        let v = f32::from(v) / 255.0;
        let lin = if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        };
        let scaled = (lin * k).clamp(0.0, 1.0);
        let enc = if scaled <= 0.003_130_8 {
            12.92 * scaled
        } else {
            1.055 * scaled.powf(1.0 / 2.4) - 0.055
        };
        (enc.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    Color::from_rgba8(ch(rgba.r), ch(rgba.g), ch(rgba.b), rgba.a)
}

/// Deterministic per-user colour via [`PresenceCtx::color_for`], falling
/// back to `theme.accent_primary` only on the all-zero (hash-lands-on-black)
/// case. Every collab affordance resolves through this so they all agree on
/// a peer's colour.
pub(crate) fn color_for(ctx: &PresenceCtx, user: &UserId, theme: &Theme) -> Color {
    let (r, g, b) = ctx.color_for(user);
    if r == 0 && g == 0 && b == 0 {
        return theme.accent_primary;
    }
    Color::from_rgb8(r, g, b)
}

impl PresenceCtx {
    /// Paint a peer's caret in their colour. `rect` is the widget's own
    /// caret geometry (`TextInput::caret_rect` / `RichTextLine::caret_rect`),
    /// so a peer caret is pixel-identical to the local one.
    pub fn draw_peer_caret(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        transform: Affine,
        user: &UserId,
        rect: Rect,
    ) {
        let color = color_for(self, user, theme);
        scene.fill(Fill::NonZero, transform, color, None, &rect);
    }

    pub fn draw_peer_selection(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        transform: Affine,
        user: &UserId,
        rect: Rect,
    ) {
        let color = color_for(self, user, theme).with_alpha_factor(0.35);
        scene.fill(Fill::NonZero, transform, color, None, &rect);
    }

    /// Draw an authorship "marker": translucent underlay in the author's
    /// colour behind the runs they wrote, at lower alpha than a selection.
    ///
    /// Not presence-fed — authorship must survive both peers closing the
    /// doc, so it's an Automerge mark (`local_author` /
    /// `RichTextLine::author_runs`). **Silently a no-op if
    /// `set_local_author` was never called** — looks exactly like a network
    /// fault. Set by `apps/notes`/`apps/chat`; `qdoc` has no per-run author
    /// field yet.
    pub fn draw_peer_authorship(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        rects: &[Rect],
    ) {
        let color = color_for(self, user, theme).with_alpha_factor(0.20);
        for r in rects {
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, r);
        }
    }

    /// `kurbo::Affine` composition below is plain f64 matrix math with no
    /// panic path; `arithmetic_side_effects` flags the `*` operator
    /// overload anyway.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
    )]
    pub fn draw_peer_pointer(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        transform: Affine,
        user: &UserId,
        pos: (f32, f32),
    ) {
        let color = color_for(self, user, theme);
        let t = transform * Affine::translate((f64::from(pos.0), f64::from(pos.1)));
        let halo = Circle::new(Point::ORIGIN, 9.0);
        scene.fill(Fill::NonZero, t, color.with_alpha_factor(0.22), None, &halo);
        let dot = Circle::new(Point::ORIGIN, 5.0);
        scene.fill(Fill::NonZero, t, color, None, &dot);
        scene.stroke(&Stroke::new(1.5), t, Color::WHITE, None, &dot);
    }

    /// Draw a soft "indicative mist" glow over `rect` in the peer's colour —
    /// wayfinding to where a peer currently is. Two translucent fills
    /// approximate a radial glow without an actual gradient.
    pub fn draw_peer_mist(&self, scene: &mut Scene, theme: &Theme, user: &UserId, rect: Rect) {
        let color = color_for(self, user, theme);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color.with_alpha_factor(0.16),
            None,
            &rect,
        );
        // Brighter core inset from the edges, so it reads as a glow.
        let pad = (rect.height() * 0.2).min(rect.width() * 0.45);
        let core = rect.inflate(-pad, -pad);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color.with_alpha_factor(0.22),
            None,
            &core,
        );
    }

    /// Draw a peer's caret with a name chip above the tip — mirrors the
    /// pointer's pill (same size/shadow) so caret and pointer read as one
    /// person.
    // Standard immediate-mode paint-helper signature for this vello
    // renderer (scene + transform + text_renderer + theme, plus this
    // draw's own content); every parameter is independently load-bearing.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_peer_caret_labeled(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        user: &UserId,
        name: &str,
        caret_rect: Rect,
    ) {
        let color = color_for(self, user, theme);
        scene.fill(Fill::NonZero, transform, color, None, &caret_rect);

        let (chip_w, chip_h) = peer_pill_size(text_renderer, theme, name);
        let cx = caret_rect.x0;
        let cy = caret_rect.y0 - chip_h - 2.0;
        let chip_rect = Rect::new(cx, cy, cx + chip_w, cy + chip_h);
        draw_peer_pill(
            scene,
            transform,
            text_renderer,
            theme,
            chip_rect,
            name,
            color,
        );
    }

    /// Outline `rect` in the peer's colour — "this peer has *this object*
    /// selected". The counterpart to [`Self::draw_peer_selection`] for
    /// content that is an object rather than a run of text: a clip on a
    /// timeline, a file in a grid, a shape on a canvas.
    ///
    /// A wash plus a ring rather than either alone. The wash on its own is
    /// too easily read as the app's own hover state; the ring on its own
    /// disappears against a busy fill — and a peer's selection is the one
    /// thing that has to stay legible *over* whatever the object is showing.
    /// Both are the peer's colour, the same colour their pointer and caret
    /// carry, so the three read as one person.
    ///
    /// `rect` is in the caller's local coordinates and `transform` maps it
    /// into scene space, so the outline is drawn from the very geometry the
    /// object itself was drawn from — never a re-derived approximation.
    pub fn draw_peer_outline(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        transform: Affine,
        user: &UserId,
        rect: Rect,
        radius: f64,
    ) {
        let color = color_for(self, user, theme);
        let shape = RoundedRect::from_rect(rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            color.with_alpha_factor(0.18),
            None,
            &shape,
        );
        scene.stroke(&Stroke::new(2.0), transform, color, None, &shape);
    }

    /// [`Self::draw_peer_outline`] with the peer's name chipped above the
    /// object's top-left corner, so a selection says *who* without needing
    /// a hover or a roster lookup. Mirrors
    /// [`Self::draw_peer_caret_labeled`] — one chip design for every "this
    /// is where that person is" affordance.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_peer_outline_labeled(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        user: &UserId,
        name: &str,
        rect: Rect,
        radius: f64,
    ) {
        self.draw_peer_outline(scene, theme, transform, user, rect, radius);
        let color = color_for(self, user, theme);
        draw_name_chip(
            scene,
            theme,
            text_renderer,
            transform,
            color,
            name,
            rect.x0,
            rect.y0,
        );
    }

    /// [`Self::draw_peer_outline_labeled`] for a surface that clips its own
    /// contents.
    ///
    /// A name chip normally hangs above the thing it names. On the top row of
    /// a clipped area there is nothing above to hang it in, so the chip is cut
    /// in half by the clip — which is how a collaborator ends up labelled with
    /// the bottom two pixels of their name. When `bounds` leaves no room above,
    /// the chip drops inside the object's top edge instead.
    ///
    /// Inside, it goes to the **right**, because an object that carries a
    /// label of its own puts it top-left: a chip there turns a clip named
    /// "Logo_Bug.png" into one named "Ada_Bug.png". On an object too narrow to
    /// hold both, the chip stays within the object rather than escaping it —
    /// covering a name is recoverable, pointing at the wrong clip is not.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_peer_outline_labeled_within(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        user: &UserId,
        name: &str,
        rect: Rect,
        radius: f64,
        bounds: Rect,
    ) {
        self.draw_peer_outline(scene, theme, transform, user, rect, radius);
        let color = color_for(self, user, theme);
        let (w, h) = chip_size(theme, text_renderer, name);
        let above = rect.y0 - h - CHIP_GAP;
        let chip = if above >= bounds.y0 {
            Rect::new(rect.x0, above, rect.x0 + w, above + h)
        } else {
            let x = (rect.x1 - w - CHIP_GAP).max(rect.x0 + CHIP_GAP);
            let y = rect.y0 + CHIP_GAP;
            Rect::new(x, y, x + w, y + h)
        };
        draw_name_chip_in(scene, theme, text_renderer, transform, color, name, chip);
    }

    /// Draw a peer's fading "just typed" highlighter behind the glyph runs
    /// they inserted. `spans` pairs each rect with its fade alpha.
    ///
    /// # Must be painted BEFORE the text, not after
    ///
    /// Painted over glyphs this drags ink and paper toward the same pigment:
    /// measured, a fresh mark over text leaves 2.4:1 (unmarked ~15:1, below
    /// WCAG AA); beneath the glyphs the same band leaves 7.5:1. Composite
    /// strength is identical either way, so getting the order right is
    /// free. Held by `text_stays_legible_under_a_marker_in_both_palettes`.
    pub fn draw_peer_typing(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        spans: &[(Rect, f32)],
    ) {
        let color = marker_pigment(color_for(self, user, theme));
        for (rect, alpha) in spans {
            // Reduce-motion holds full strength instead of fading; the span
            // still drops on schedule — only the animation is removed.
            let fade = if theme.reduce_motion { 1.0 } else { *alpha };
            let a = (fade * MARKER_ALPHA).clamp(0.0, 1.0);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color.with_alpha_factor(a),
                None,
                rect,
            );
        }
    }

    pub fn draw_peer_focus(&self, scene: &mut Scene, theme: &Theme, user: &UserId, rect: Rect) {
        let color = color_for(self, user, theme);
        let strip = Rect::new(rect.x0 - 4.0, rect.y0, rect.x0 - 1.0, rect.y1);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &strip);
    }
}

const DOT_R: f64 = 6.5;
/// Outer edge including the contrast ring — the clearance pill placement
/// uses.
const RING_R: f64 = 8.5;
/// Gap between the dot and its pill, and between two pills.
const PILL_GAP: f64 = 4.0;
/// Distance from its pointer past which a pill gets a tether line.
const TETHER_AFTER: f64 = 4.0;

/// Padding inside a peer's name pill. One pair of numbers because there is
/// one pill: the pointer's and the caret's chip must size identically.
pub(crate) const PILL_PAD_X: f64 = 7.0;
pub(crate) const PILL_PAD_Y: f64 = 3.0;

/// Peer name type size: `scale.sm`, not `scale.xs` — xs measured ~34x15px
/// with ~8px of glyph, present but not readable for a whole-session label.
pub(crate) fn pill_label_size(theme: &Theme) -> f32 {
    theme.tokens.typography.scale.sm
}

/// Pill size for `label`, padding included — shared so callers size their
/// box exactly like the painter.
pub(crate) fn peer_pill_size(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    label: &str,
) -> (f64, f64) {
    let size = pill_label_size(theme);
    let (tw, _th) = text_renderer.measure(label, size, None);
    (
        f64::from(tw) + PILL_PAD_X * 2.0,
        f64::from(size) + PILL_PAD_Y * 2.0,
    )
}

#[derive(Debug, Clone)]
pub struct PeerCursor {
    pub pos: (f64, f64),
    pub label: String,
    pub color: Color,
}

#[derive(Debug, Clone, Copy)]
pub struct PillRequest {
    pub at: (f64, f64),
    pub size: (f64, f64),
}

/// Where a pill ended up; `tethered` means the caller should draw a line
/// back to its pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PillPlacement {
    pub rect: Rect,
    pub tethered: bool,
}

/// Place one name pill per peer so none overlap and none leaves `viewport`.
/// Each pill tries the four corners around its pointer, taking the first
/// clear of the viewport and every already-placed pill; only when all four
/// are taken does it slide along the vertical and get marked `tethered`.
/// Placement order is by position, not arrival, so a peer reconnecting
/// doesn't reshuffle everyone else's names.
pub fn layout_peer_pills(viewport: Rect, pills: &[PillRequest]) -> Vec<PillPlacement> {
    // Normalise — an inverted `viewport` would otherwise clamp every pill
    // to nothing.
    let vp = Rect::new(
        viewport.x0.min(viewport.x1),
        viewport.y0.min(viewport.y1),
        viewport.x0.max(viewport.x1),
        viewport.y0.max(viewport.y1),
    );

    // Carries each request's original index alongside itself, so the sort
    // below never needs to index back into `pills`.
    let mut order: Vec<(usize, PillRequest)> = pills.iter().copied().enumerate().collect();
    order.sort_by(|&(a, pa), &(b, pb)| {
        pa.at
            .1
            .total_cmp(&pb.at.1)
            .then(pa.at.0.total_cmp(&pb.at.0))
            .then(a.cmp(&b))
    });

    let mut out = vec![
        PillPlacement {
            rect: Rect::ZERO,
            tethered: false,
        };
        pills.len()
    ];
    let mut placed: Vec<Rect> = Vec::with_capacity(pills.len());

    let inside =
        |r: &Rect, vp: &Rect| r.x0 >= vp.x0 && r.y0 >= vp.y0 && r.x1 <= vp.x1 && r.y1 <= vp.y1;
    let clear = |r: &Rect, placed: &[Rect]| {
        placed.iter().all(|p| {
            p.inflate(PILL_GAP / 2.0, PILL_GAP / 2.0)
                .intersect(*r)
                .is_zero_area()
        })
    };

    for (i, request) in order {
        let PillRequest {
            at: (px, py),
            size: (w, h),
        } = request;
        let right = px + RING_R + PILL_GAP;
        let left = px - RING_R - PILL_GAP - w;
        let below = py + RING_R + PILL_GAP;
        let above = py - RING_R - PILL_GAP - h;
        let corners = [(right, below), (right, above), (left, below), (left, above)];

        let mut chosen = None;
        for (n, (x, y)) in corners.into_iter().enumerate() {
            let r = Rect::new(x, y, x + w, y + h);
            if inside(&r, &vp) && clear(&r, &placed) {
                // Any corner but the first means the pill moved — no longer
                // obviously its own pointer.
                chosen = Some((r, n > 0));
                break;
            }
        }

        let (rect, tethered) = if let Some((r, moved)) = chosen {
            (r, moved)
        } else {
            // Every corner taken or off-window: keep to whichever side
            // fits and walk down the vertical until clear.
            let x = if right + w <= vp.x1 {
                right
            } else {
                left.max(vp.x0)
            };
            let x = x.clamp(vp.x0, (vp.x1 - w).max(vp.x0));
            let mut r = Rect::new(x, below, x + w, below + h);
            let step = h + PILL_GAP;
            let mut guard: u32 = 0;
            while (!inside(&r, &vp) || !clear(&r, &placed)) && guard < 64 {
                let next_y = if r.y1 + step <= vp.y1 {
                    r.y0 + step
                } else {
                    // Out of room going down — restart above and walk up.
                    let up = if r.y0 > py { above } else { r.y0 - step };
                    if up < vp.y0 {
                        break;
                    }
                    up
                };
                r = Rect::new(x, next_y, x + w, next_y + h);
                // Bounded loop guard, not a meaningful count — saturating
                // is fine, the `< 64` condition stops the loop first.
                guard = guard.saturating_add(1);
            }
            let r = Rect::new(
                r.x0,
                r.y0.clamp(vp.y0, (vp.y1 - h).max(vp.y0)),
                r.x0 + w,
                r.y0.clamp(vp.y0, (vp.y1 - h).max(vp.y0)) + h,
            );
            (r, true)
        };

        // Further from its pointer than a corner placement gets: draw the
        // line, however it got there.
        let dx = (rect.x0 - px).abs().min((rect.x1 - px).abs());
        let dy = (rect.y0 - py).abs().min((rect.y1 - py).abs());
        let far = dx.max(dy) > RING_R + PILL_GAP + TETHER_AFTER;
        // `i` is always < `out.len()`: `order` is built from `0..pills.len()`
        // and `out` is sized to `pills.len()` above.
        if let Some(slot) = out.get_mut(i) {
            *slot = PillPlacement {
                rect,
                tethered: tethered || far,
            };
        }
        placed.push(rect);
    }
    out
}

/// Paint pointers, then tethers, then pills — in that layer order so a
/// later peer's halo can't wash over an earlier peer's name. `viewport`
/// bounds the pills; positions are assumed already filtered to it.
pub fn draw_peer_cursors(
    scene: &mut Scene,
    transform: Affine,
    viewport: Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    peers: &[PeerCursor],
) {
    if peers.is_empty() {
        return;
    }
    let requests: Vec<PillRequest> = peers
        .iter()
        .map(|p| PillRequest {
            at: p.pos,
            size: peer_pill_size(text_renderer, theme, &p.label),
        })
        .collect();
    let placements = layout_peer_pills(viewport, &requests);

    for p in peers {
        draw_peer_dot(scene, transform, p.pos, p.color);
    }

    // Tethers — under the pills so a line never crosses a name.
    for (p, place) in peers.iter().zip(&placements) {
        if !place.tethered {
            continue;
        }
        let from = Point::new(p.pos.0, p.pos.1);
        let to = nearest_point_on(place.rect, from);
        let mut line = vello::kurbo::BezPath::new();
        line.move_to(from);
        line.line_to(to);
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            p.color.with_alpha_factor(0.75),
            None,
            &line,
        );
    }

    for (p, place) in peers.iter().zip(&placements) {
        draw_peer_pill(
            scene,
            transform,
            text_renderer,
            theme,
            place.rect,
            &p.label,
            p.color,
        );
    }
}

/// A peer's pointer: halo, shadow edge, solid dot, white ring. Shadow and
/// ring together work on both light and dark backgrounds — ring separates
/// dark-on-dark, shadow separates light-on-light; whichever isn't needed
/// stays invisible.
/// `kurbo::Affine` composition below is plain f64 matrix math with no
/// panic path; `arithmetic_side_effects` flags the `*` operator overload
/// anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
)]
fn draw_peer_dot(scene: &mut Scene, transform: Affine, pos: (f64, f64), peer: Color) {
    let t = transform * Affine::translate((pos.0, pos.1));
    scene.fill(
        Fill::NonZero,
        t,
        peer.with_alpha_factor(0.22),
        None,
        &Circle::new(Point::ORIGIN, RING_R + 3.0),
    );
    scene.stroke(
        &Stroke::new(2.0),
        t,
        Color::BLACK.with_alpha_factor(0.18),
        None,
        &Circle::new(Point::ORIGIN, RING_R - 0.5),
    );
    let dot = Circle::new(Point::ORIGIN, DOT_R);
    scene.fill(Fill::NonZero, t, peer, None, &dot);
    scene.stroke(&Stroke::new(2.0), t, Color::WHITE, None, &dot);
}

/// A peer's name pill, filled in their own colour (not a surface token) so
/// the pill *is* the swatch — name and cursor match by colour, not by
/// proximity.
/// `kurbo::Rect`'s `Add<Vec2>` and `kurbo::Affine`'s `Mul` below are plain
/// f64 arithmetic with no panic path; `arithmetic_side_effects` flags the
/// operator overloads anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo Rect/Affine arithmetic is f64 math; it cannot panic or overflow, unlike integer +/-/*"
)]
pub(crate) fn draw_peer_pill(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    peer: Color,
) {
    let label_size = pill_label_size(theme);
    let radius = f64::from(theme.tokens.radii.sm);
    let shadow = RoundedRect::from_rect(
        rect.inflate(0.5, 0.5) + vello::kurbo::Vec2::new(0.0, 1.0),
        radius,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        Color::BLACK.with_alpha_factor(0.20),
        None,
        &shadow,
    );
    let pill = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, transform, peer, None, &pill);
    text_renderer.draw_text(
        scene,
        label,
        label_size,
        contrast_ink(peer),
        transform * Affine::translate((rect.x0 + PILL_PAD_X, rect.y0 + PILL_PAD_Y)),
        None,
    );
}

/// Closest point on `rect` to `p` — where a tether meets the pill it points
/// at.
fn nearest_point_on(rect: Rect, p: Point) -> Point {
    Point::new(p.x.clamp(rect.x0, rect.x1), p.y.clamp(rect.y0, rect.y1))
}

/// Paint every entry in `presence.remote_cursors` as a dot + name pill, for
/// non-shell apps drawing cross-process ghost cursors without reimplementing
/// the geometry. No viewport, so pills are de-overlapped but not clamped —
/// prefer [`draw_remote_presence`], which keeps names on screen.
pub fn draw_remote_cursors(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    // Same ownership rule as draw_remote_presence: skip pointers the
    // compositor overlay already paints.
    let peers: Vec<PeerCursor> = presence
        .remote_cursors
        .iter()
        .filter(|c| c.painted_by_window())
        .map(peer_of)
        .collect();
    if peers.is_empty() {
        return;
    }
    let unbounded = Rect::new(
        f64::MIN / 4.0,
        f64::MIN / 4.0,
        f64::MAX / 4.0,
        f64::MAX / 4.0,
    );
    draw_peer_cursors(scene, transform, unbounded, text_renderer, theme, &peers);
}

/// A [`super::RemoteCursor`] as the renderer wants it.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "each channel is clamped to 0.0..=1.0 above, so the scaled result is 0.0..=255.0"
)]
fn peer_of(cursor: &super::RemoteCursor) -> PeerCursor {
    PeerCursor {
        pos: (f64::from(cursor.position[0]), f64::from(cursor.position[1])),
        label: cursor.label.clone(),
        color: Color::from_rgba8(
            (cursor.color[0].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[1].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[2].clamp(0.0, 1.0) * 255.0) as u8,
            255,
        ),
    }
}

/// Pick black or white ink for text drawn on top of `bg`, by relative
/// luminance — so a name chip label stays readable on any peer colour.
/// Paint a peer's name in a small rounded chip in their colour, anchored so
/// its bottom-left corner sits just above `(anchor_x, anchor_top)` — the
/// top-left of whatever the chip is naming.
///
/// One implementation for every "this is where that person is" affordance
/// (a caret, a selected object), because two chip designs on one screen
/// read as two different kinds of thing when they are the same kind of
/// thing. Drawn through `transform` so the chip rides with what it labels.
/// Padding inside a name chip, and the gap it keeps from what it names.
const CHIP_PAD_X: f64 = 5.0;
const CHIP_PAD_Y: f64 = 1.5;
const CHIP_GAP: f64 = 2.0;

/// How big a name chip will be. A caller that has to fit one somewhere — a
/// lane area that clips its contents, a viewport edge — needs this before it
/// can decide where the chip goes.
fn chip_size(theme: &Theme, text_renderer: &mut TextRenderer, name: &str) -> (f64, f64) {
    let label_size = theme.tokens.typography.scale.xs;
    let (tw, _th) = text_renderer.measure(name, label_size, None);
    (
        tw as f64 + CHIP_PAD_X * 2.0,
        label_size as f64 + CHIP_PAD_Y * 2.0,
    )
}

/// Paint the chip into a rectangle the caller has already chosen.
fn draw_name_chip_in(
    scene: &mut Scene,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    transform: Affine,
    color: Color,
    name: &str,
    chip_rect: Rect,
) {
    let chip = RoundedRect::from_rect(chip_rect, theme.tokens.radii.sm as f64);
    // Solid fill in the peer's colour so the chip reads as "them".
    scene.fill(Fill::NonZero, transform, color, None, &chip);
    // Label ink: white or near-black for contrast against the colour.
    text_renderer.draw_text(
        scene,
        name,
        theme.tokens.typography.scale.xs,
        contrast_ink(color),
        transform * Affine::translate((chip_rect.x0 + CHIP_PAD_X, chip_rect.y0 + CHIP_PAD_Y)),
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_name_chip(
    scene: &mut Scene,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    transform: Affine,
    color: Color,
    name: &str,
    anchor_x: f64,
    anchor_top: f64,
) {
    let (w, h) = chip_size(theme, text_renderer, name);
    let cy = anchor_top - h - CHIP_GAP;
    draw_name_chip_in(
        scene,
        theme,
        text_renderer,
        transform,
        color,
        name,
        Rect::new(anchor_x, cy, anchor_x + w, cy + h),
    );
}

/// Black or white ink for text on `bg`, by luma — a fixed white label is
/// unreadable on over half the peer palette (11 of 24 entries under 2:1,
/// palest 1.24:1 against white).
pub fn contrast_ink(bg: Color) -> Color {
    let c = bg.to_rgba8();
    // Rec. 601 luma on the 0..255 channels.
    let luma = 0.299 * f32::from(c.r) + 0.587 * f32::from(c.g) + 0.114 * f32::from(c.b);
    if luma > 140.0 {
        Color::BLACK
    } else {
        Color::WHITE
    }
}

/// The clickable/paintable rect of an off-screen peer's mist band, centred
/// on `mist.along`, clamped to the viewport. Shared by
/// [`draw_remote_presence`] (paints) and
/// [`super::PresenceCtx::hit_test_offscreen_mist`] (hit-tests) so the two
/// never drift.
pub(super) fn mist_band_rect(viewport: vello::kurbo::Rect, mist: &super::geom::EdgeMist) -> Rect {
    use super::geom::MistEdge;
    let x0 = viewport.x0.min(viewport.x1);
    let x1 = viewport.x0.max(viewport.x1);
    let y0 = viewport.y0.min(viewport.y1);
    let y1 = viewport.y0.max(viewport.y1);
    let w = x1 - x0;
    let h = y1 - y0;

    let depth = 44.0_f64.min(w * 0.5).min(h * 0.5);
    let extent = 140.0_f64;

    match mist.edge {
        MistEdge::Left | MistEdge::Right => {
            let cy = y0 + mist.along * h;
            let top = (cy - extent / 2.0).clamp(y0, y1 - extent.min(h));
            let bot = (top + extent).min(y1);
            if matches!(mist.edge, MistEdge::Left) {
                Rect::new(x0, top, x0 + depth, bot)
            } else {
                Rect::new(x1 - depth, top, x1, bot)
            }
        }
        MistEdge::Top | MistEdge::Bottom => {
            let cx = x0 + mist.along * w;
            let left = (cx - extent / 2.0).clamp(x0, x1 - extent.min(w));
            let right = (left + extent).min(x1);
            if matches!(mist.edge, MistEdge::Top) {
                Rect::new(left, y0, right, y0 + depth)
            } else {
                Rect::new(left, y1 - depth, right, y1)
            }
        }
    }
}

/// Paint one off-screen peer's edge mist: a soft band fading from the
/// border (a few inset layers approximating a gradient), plus a name tag.
#[allow(
    clippy::cast_possible_truncation,
    reason = "f64 -> f32 for rendering alpha factors already bounded to ~0.0..=1.0; f32 precision is more than sufficient"
)]
fn draw_edge_mist(
    scene: &mut Scene,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    viewport: vello::kurbo::Rect,
    mist: &super::geom::EdgeMist,
    peer: Color,
    name: &str,
) {
    use super::geom::MistEdge;
    let band = mist_band_rect(viewport, mist);

    // Intensity tapers with how far off-screen the peer is.
    let far = (mist.distance / 600.0).clamp(0.0, 1.0) as f32;
    let base = 0.34 * (1.0 - 0.6 * far);

    // Three bands from the edge inward, each dimmer and shorter, so the
    // mist reads as a soft glow rather than a flat block.
    let layers = 3;
    for i in 0..layers {
        let f = f64::from(i) / f64::from(layers);
        let a = base * (1.0 - f as f32);
        let inset = match mist.edge {
            MistEdge::Left => Rect::new(band.x0, band.y0, band.x1 - (band.width() * f), band.y1),
            MistEdge::Right => Rect::new(band.x0 + (band.width() * f), band.y0, band.x1, band.y1),
            MistEdge::Top => Rect::new(band.x0, band.y0, band.x1, band.y1 - (band.height() * f)),
            MistEdge::Bottom => Rect::new(band.x0, band.y0 + (band.height() * f), band.x1, band.y1),
        };
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            peer.with_alpha_factor(a),
            None,
            &inset,
        );
    }

    let tokens = &theme.tokens;
    let label_size = tokens.typography.scale.xs;
    let (tw, _th) = text_renderer.measure(name, label_size, None);
    let pad = 4.0_f64;
    let tag_w = f64::from(tw) + pad * 2.0;
    let tag_h = f64::from(label_size) + pad;
    // Anchor the tag near the band centre, nudged inward from the edge.
    let (tx, ty) = match mist.edge {
        MistEdge::Left => (band.x0 + 4.0, band.center().y - tag_h / 2.0),
        MistEdge::Right => (band.x1 - tag_w - 4.0, band.center().y - tag_h / 2.0),
        MistEdge::Top => (band.center().x - tag_w / 2.0, band.y0 + 4.0),
        MistEdge::Bottom => (band.center().x - tag_w / 2.0, band.y1 - tag_h - 4.0),
    };
    let tag_rect = Rect::new(tx, ty, tx + tag_w, ty + tag_h);
    let tag = RoundedRect::from_rect(tag_rect, f64::from(tokens.radii.sm));
    scene.fill(Fill::NonZero, Affine::IDENTITY, peer, None, &tag);
    text_renderer.draw_text(
        scene,
        name,
        label_size,
        contrast_ink(peer),
        Affine::translate((tag_rect.x0 + pad, tag_rect.y0 + pad / 2.0)),
        None,
    );
}

/// Paint already-resolved peer cursors in one coordinate space. On-screen
/// peers get moving dots and collision-free name pills; off-screen peers
/// become directional edge mist. This is the compositor-facing sibling of
/// [`draw_remote_presence`], which resolves cursors from a [`PresenceCtx`].
pub fn draw_peer_presence(
    scene: &mut Scene,
    transform: Affine,
    viewport: vello::kurbo::Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    peers: &[PeerCursor],
) {
    let mut on_screen = Vec::new();
    for peer in peers {
        let target = Point::new(peer.pos.0, peer.pos.1);
        if let Some(mist) = super::geom::edge_mist_for(viewport, target) {
            draw_edge_mist(
                scene,
                text_renderer,
                theme,
                viewport,
                &mist,
                peer.color,
                &peer.label,
            );
        } else {
            on_screen.push(peer.clone());
        }
    }
    draw_peer_cursors(scene, transform, viewport, text_renderer, theme, &on_screen);
}

/// The one call the app makes each frame to render all cross-process peer
/// presence: on-screen peers get a dot + pill, off-screen peers get an edge
/// mist. `viewport` is the visible rect in the same (logical) space as
/// cursor positions.
pub fn draw_remote_presence(
    scene: &mut Scene,
    transform: Affine,
    viewport: vello::kurbo::Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    if presence.remote_cursors.is_empty() {
        return;
    }
    // Split first, draw after: the on-screen peers are laid out together
    // so their names cannot land on top of each other.
    let mut on_screen: Vec<PeerCursor> = Vec::new();
    for cursor in &presence.remote_cursors {
        // Nobody is a peer of themselves. Already enforced where state
        // arrives (apply_remote_presence, upsert_remote_cursor_owned), but
        // those depend on knowing our own id, which has several spellings —
        // checked again here as the one place that catches every door.
        if presence.is_own_identity(&cursor.user_id) {
            continue;
        }
        // A window only places pointers measured in its own coordinates. A
        // desktop-space pointer is the compositor's to resolve — testing it
        // against this window's rect and calling "outside" "off my edge"
        // would point a wayfinding band in a direction the peer isn't in.
        if !cursor.painted_by_window() {
            continue;
        }
        let target = Point::new(f64::from(cursor.position[0]), f64::from(cursor.position[1]));
        let peer = peer_of(cursor);
        match super::geom::edge_mist_for(viewport, target) {
            Some(mist) => {
                draw_edge_mist(
                    scene,
                    text_renderer,
                    theme,
                    viewport,
                    &mist,
                    peer.color,
                    &cursor.label,
                );
            }
            None => on_screen.push(peer),
        }
    }
    draw_peer_cursors(scene, transform, viewport, text_renderer, theme, &on_screen);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offscreen::OffscreenRenderer;
    use crate::permissions::UserId;

    /// "alice" maps to the pinned pink (0xec, 0x48, 0x99): the dot center
    /// should read red-high / green-low / blue-mid.
    #[test]
    fn peer_pointer_paints_peer_color_at_center() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut scene = Scene::new();
        let user = UserId("alice".to_string());
        ctx.draw_peer_pointer(&mut scene, &theme, Affine::IDENTITY, &user, (40.0, 40.0));

        let mut r = OffscreenRenderer::new(80, 80);
        let rgba = r.render(&scene, Color::BLACK);
        let (red, green, blue, _a) = OffscreenRenderer::sample(&rgba, 80, 40, 40);
        assert!(
            red > 150 && green < 150 && blue > 70,
            "expected alice's pink pointer at center, got ({red},{green},{blue})"
        );
    }

    /// "One person, one pointer": painted once when this layer owns it, not
    /// at all when the compositor overlay does. Both halves are asserted in
    /// one test — either alone would pass a filter that always/never draws.
    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "`at` is a fixed in-bounds test coordinate, always representable as u32"
    )]
    fn only_a_pointer_this_layer_owns_reaches_the_screen() {
        use super::super::PointerOwner;

        let theme = Theme::default();
        let viewport = Rect::new(0.0, 0.0, 120.0, 120.0);
        // Well inside the viewport, so `edge_mist_for` resolves to the
        // on-screen branch and this is testing the dot, not the mist.
        let at = [40.0_f32, 40.0_f32];

        let paint = |owner: PointerOwner| {
            let mut ctx = PresenceCtx::empty();
            ctx.upsert_remote_cursor_owned("alice".into(), at, Some("Alice"), 1.0, owner);
            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            draw_remote_presence(
                &mut scene,
                Affine::IDENTITY,
                viewport,
                &mut tr,
                &theme,
                &ctx,
            );
            let mut r = OffscreenRenderer::new(120, 120);
            let rgba = r.render(&scene, Color::BLACK);
            OffscreenRenderer::sample(&rgba, 120, at[0] as u32, at[1] as u32)
        };

        // Owned here: alice's pinned pink lands on the screen.
        let (r, g, b, _) = paint(PointerOwner::Window);
        assert!(
            r > 150 && g < 150 && b > 70,
            "a pointer this layer owns must be drawn; got ({r},{g},{b})"
        );

        // Owned by the compositor: the overlay already draws that person.
        let (r, g, b, _) = paint(PointerOwner::Desktop);
        assert_eq!(
            (r, g, b),
            (0, 0, 0),
            "the compositor paints this peer; painting them here too is one person twice",
        );
    }

    /// "charlie" maps to emerald (0x10, 0xb9, 0x81): green dominant.
    #[test]
    fn peer_caret_paints_peer_color() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut scene = Scene::new();
        let user = UserId("charlie".to_string());
        ctx.draw_peer_caret(
            &mut scene,
            &theme,
            Affine::IDENTITY,
            &user,
            Rect::new(20.0, 10.0, 22.0, 26.0),
        );

        let mut r = OffscreenRenderer::new(80, 80);
        let rgba = r.render(&scene, Color::BLACK);
        // Sample inside the 2px-wide, 16px-tall caret bar at (20, 18).
        let (red, green, blue, _a) = OffscreenRenderer::sample(&rgba, 80, 20, 18);
        assert!(
            green > red && green > blue && green > 100,
            "expected charlie's emerald caret (G dominant), got ({red},{green},{blue})"
        );
    }

    /// "bob" maps to orange (0xf9, 0x73, 0x16): red-high, green-mid,
    /// blue-low. Translucent over black, the channel ordering still holds.
    #[test]
    fn peer_mist_tints_element_in_peer_color() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut scene = Scene::new();
        let user = UserId("bob".to_string());
        ctx.draw_peer_mist(&mut scene, &theme, &user, Rect::new(10.0, 10.0, 70.0, 30.0));

        let mut r = OffscreenRenderer::new(80, 80);
        let rgba = r.render(&scene, Color::BLACK);
        let (red, green, blue, _a) = OffscreenRenderer::sample(&rgba, 80, 40, 20);
        assert!(
            red > green && green >= blue && red > 20,
            "expected bob's orange mist (R>G>=B), got ({red},{green},{blue})"
        );
    }

    /// Samples both the caret bar and the name chip above it. "charlie" is
    /// emerald (G dominant).
    #[test]
    fn labeled_caret_paints_bar_and_chip_above() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let user = UserId("charlie".to_string());
        // Caret bar from y=40..60 at x=30..32. The chip sits above y=40.
        ctx.draw_peer_caret_labeled(
            &mut scene,
            &theme,
            &mut tr,
            Affine::IDENTITY,
            &user,
            "Charlie",
            Rect::new(30.0, 40.0, 32.0, 60.0),
        );

        let mut r = OffscreenRenderer::new(120, 120);
        let rgba = r.render(&scene, Color::BLACK);

        // The caret bar itself, at (30, 50).
        let (br, bg, bb, _a) = OffscreenRenderer::sample(&rgba, 120, 30, 50);
        assert!(
            bg > br && bg > bb && bg > 100,
            "caret bar should be emerald, got ({br},{bg},{bb})"
        );

        // The chip fill, a few pixels above the caret top (y ~ 33).
        let (cr, cg, cb, ca) = OffscreenRenderer::sample(&rgba, 120, 31, 33);
        assert!(
            ca > 0,
            "expected an opaque chip above the caret, got alpha {ca}"
        );
        assert!(
            cg > cr && cg > cb,
            "chip should be emerald (peer color), got ({cr},{cg},{cb})"
        );
    }

    /// A peer's selected object gets both halves: a ring on the boundary
    /// and a wash across the middle. Either alone fails somewhere — a bare
    /// wash reads as the app's own hover, a bare ring vanishes over a busy
    /// fill — so both are asserted, and the ring is asserted to be the
    /// *stronger* of the two or it is not a boundary any more.
    ///
    /// "charlie" is emerald (0x10, 0xb9, 0x81), so both should read G-dominant.
    #[test]
    fn peer_outline_rings_the_object_and_washes_its_middle() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut scene = Scene::new();
        let user = UserId("charlie".to_string());
        ctx.draw_peer_outline(
            &mut scene,
            &theme,
            Affine::IDENTITY,
            &user,
            Rect::new(20.0, 20.0, 100.0, 80.0),
            4.0,
        );

        let mut r = OffscreenRenderer::new(120, 120);
        let rgba = r.render(&scene, Color::BLACK);

        // The ring, on the left edge at mid-height.
        let (er, eg, eb, _a) = OffscreenRenderer::sample(&rgba, 120, 20, 50);
        assert!(
            eg > er && eg > eb && eg > 100,
            "the outline edge should be charlie's emerald, got ({er},{eg},{eb})"
        );

        // The wash, in the middle, well away from any edge.
        let (wr, wg, wb, _a) = OffscreenRenderer::sample(&rgba, 120, 60, 50);
        assert!(
            wg > wr && wg > wb && wg > 0,
            "the interior should carry a wash of the same colour, got ({wr},{wg},{wb})"
        );
        assert!(
            eg > wg,
            "the edge must read stronger than the wash or it is not a boundary: \
             edge G={eg}, wash G={wg}"
        );
    }

    /// The named variant puts the same chip above the object that
    /// `draw_peer_caret_labeled` puts above a caret — one chip design, so a
    /// person's caret and their selected object read as the same person.
    #[test]
    fn a_labelled_outline_chips_the_peers_name_above_the_object() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let user = UserId("charlie".to_string());
        ctx.draw_peer_outline_labeled(
            &mut scene,
            &theme,
            &mut tr,
            Affine::IDENTITY,
            &user,
            "Charlie",
            Rect::new(30.0, 40.0, 100.0, 80.0),
            4.0,
        );

        let mut r = OffscreenRenderer::new(140, 140);
        let rgba = r.render(&scene, Color::BLACK);

        // The chip fill, a few pixels above the object's top edge, at its
        // left x — the same anchoring the labelled caret uses.
        let (cr, cg, cb, ca) = OffscreenRenderer::sample(&rgba, 140, 31, 33);
        assert!(
            ca > 0,
            "expected an opaque chip above the object, got alpha {ca}"
        );
        assert!(
            cg > cr && cg > cb,
            "the chip should be the peer's emerald, got ({cr},{cg},{cb})"
        );

        // And the object is still outlined underneath it.
        let (er, eg, eb, _a) = OffscreenRenderer::sample(&rgba, 140, 30, 60);
        assert!(
            eg > er && eg > eb && eg > 100,
            "the outline should still be drawn, got ({er},{eg},{eb})"
        );
    }

    /// On the top row of a surface that clips its contents there is no room
    /// above an object for its name chip, and hanging one there anyway gets
    /// it sliced by the clip — a collaborator labelled with the bottom two
    /// pixels of their name. The chip has to come inside instead.
    #[test]
    fn a_name_chip_with_no_room_above_it_comes_inside_the_object() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let user = UserId("charlie".to_string());
        // The lane area starts at y=20; the object sits 2px inside its top,
        // so a chip hung above would start above y=20 and be cut off.
        let bounds = Rect::new(0.0, 20.0, 200.0, 120.0);
        let object = Rect::new(10.0, 22.0, 160.0, 90.0);

        let mut scene = Scene::new();
        ctx.draw_peer_outline_labeled_within(
            &mut scene,
            &theme,
            &mut tr,
            Affine::IDENTITY,
            &user,
            "Charlie",
            object,
            4.0,
            bounds,
        );
        let mut r = OffscreenRenderer::new(200, 140);
        let rgba = r.render(&scene, Color::BLACK);

        // Nothing at all above the ceiling: that whole band would be clipped
        // away by the surface, so anything drawn there is invisible.
        for y in 0..20 {
            for x in 0..200 {
                let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, 200, x, y);
                assert!(
                    cr == 0 && cg == 0 && cb == 0,
                    "painted at ({x},{y}), above the ceiling at y=20, where the \
                     surface would clip it away",
                );
            }
        }

        // And the chip is inside the object, at its right — the left is where
        // an object keeps its own label.
        let inside_right = (155, 30);
        let (cr, cg, cb, ca) =
            OffscreenRenderer::sample(&rgba, 200, inside_right.0, inside_right.1);
        assert!(ca > 0, "no chip inside the object at {inside_right:?}");
        assert!(
            cg > cr && cg > cb,
            "the chip inside should be charlie's emerald, got ({cr},{cg},{cb})",
        );
    }

    /// With room above, the chip still hangs above — the flip is a fallback,
    /// not a new default, or every labelled object would start covering itself.
    #[test]
    fn a_name_chip_with_room_above_it_still_hangs_above() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let user = UserId("charlie".to_string());
        let bounds = Rect::new(0.0, 0.0, 200.0, 140.0);
        let object = Rect::new(10.0, 60.0, 160.0, 120.0);

        let mut scene = Scene::new();
        ctx.draw_peer_outline_labeled_within(
            &mut scene,
            &theme,
            &mut tr,
            Affine::IDENTITY,
            &user,
            "Charlie",
            object,
            4.0,
            bounds,
        );
        let mut r = OffscreenRenderer::new(200, 140);
        let rgba = r.render(&scene, Color::BLACK);

        // Above the object's top edge, at its left.
        let (cr, cg, cb, ca) = OffscreenRenderer::sample(&rgba, 200, 12, 52);
        assert!(ca > 0, "expected the chip above the object");
        assert!(
            cg > cr && cg > cb,
            "chip above should be charlie's emerald, got ({cr},{cg},{cb})",
        );
    }

    /// The typing highlighter paints the peer's color over the typed run.
    /// "bob" is orange (R>G>B). Fresh (alpha 1.0) over black still keeps
    /// that channel ordering.
    #[test]
    fn typing_tint_paints_peer_color_over_run() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut scene = Scene::new();
        let user = UserId("bob".to_string());
        ctx.draw_peer_typing(
            &mut scene,
            &theme,
            &user,
            &[(Rect::new(10.0, 10.0, 70.0, 30.0), 1.0)],
        );
        let mut r = OffscreenRenderer::new(80, 80);
        let rgba = r.render(&scene, Color::BLACK);
        let (red, green, blue, a) = OffscreenRenderer::sample(&rgba, 80, 40, 20);
        assert!(a > 0, "typing tint should be visible");
        assert!(
            red > green && green >= blue && red > 20,
            "expected bob's orange typing tint (R>G>=B), got ({red},{green},{blue})"
        );
        // A fainter alpha paints a dimmer tint — prove the fade is applied.
        let mut scene2 = Scene::new();
        ctx.draw_peer_typing(
            &mut scene2,
            &theme,
            &user,
            &[(Rect::new(10.0, 10.0, 70.0, 30.0), 0.2)],
        );
        let rgba2 = r.render(&scene2, Color::BLACK);
        let (red2, ..) = OffscreenRenderer::sample(&rgba2, 80, 40, 20);
        assert!(
            red2 < red,
            "lower alpha must paint a dimmer tint: {red2} !< {red}"
        );
    }

    /// A peer off to the RIGHT tints the right edge and leaves the left
    /// edge clear.
    #[test]
    fn offscreen_peer_mists_the_correct_edge() {
        let mut ctx = PresenceCtx::empty();
        // "bob" (orange) at x=180 — well past a 100-wide viewport's right
        // edge — at mid height.
        ctx.upsert_remote_cursor("bob".to_string(), [180.0, 50.0], None);
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let viewport = Rect::new(0.0, 0.0, 100.0, 100.0);
        draw_remote_presence(
            &mut scene,
            Affine::IDENTITY,
            viewport,
            &mut tr,
            &theme,
            &ctx,
        );

        let mut r = OffscreenRenderer::new(100, 100);
        let rgba = r.render(&scene, Color::BLACK);
        // Right edge, mid-height: expect orange mist.
        let (rr, rg, rb, ra) = OffscreenRenderer::sample(&rgba, 100, 97, 50);
        assert!(ra > 0, "right edge should carry the mist");
        assert!(
            rr > rg && rg >= rb && rr > 15,
            "right-edge mist should be bob's orange (R>G>=B), got ({rr},{rg},{rb})"
        );
        // Left edge, mid-height: the peer isn't that way — no mist.
        let (lr, lg, lb, _la) = OffscreenRenderer::sample(&rgba, 100, 3, 50);
        assert!(
            lr < 20 && lg < 20 && lb < 20,
            "left edge should stay clear, got ({lr},{lg},{lb})"
        );
    }

    fn req(x: f64, y: f64, w: f64, h: f64) -> PillRequest {
        PillRequest {
            at: (x, y),
            size: (w, h),
        }
    }

    /// Two people working next to each other put their pointers a few
    /// pixels apart. Their names must still be two names.
    #[test]
    fn pills_of_adjacent_peers_do_not_overlap() {
        let vp = Rect::new(0.0, 0.0, 800.0, 600.0);
        let out = layout_peer_pills(
            vp,
            &[req(400.0, 300.0, 60.0, 20.0), req(404.0, 302.0, 60.0, 20.0)],
        );
        assert!(
            out[0].rect.intersect(out[1].rect).is_zero_area(),
            "two pills landed on top of each other: {:?} and {:?}",
            out[0].rect,
            out[1].rect,
        );
    }

    /// Four pills from four nearly-identical pointers must still separate.
    #[test]
    fn four_piled_peers_get_four_readable_pills() {
        let vp = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let pills: Vec<PillRequest> = (0..4)
            .map(|i| {
                req(
                    600.0 + f64::from(i) * 3.0,
                    400.0 + f64::from(i) * 2.0,
                    70.0,
                    22.0,
                )
            })
            .collect();
        let out = layout_peer_pills(vp, &pills);
        for i in 0..out.len() {
            for j in i + 1..out.len() {
                assert!(
                    out[i].rect.intersect(out[j].rect).is_zero_area(),
                    "pills {i} and {j} overlap: {:?} vs {:?}",
                    out[i].rect,
                    out[j].rect,
                );
            }
        }
    }

    /// A peer in the bottom-right corner still gets a readable name: the
    /// pill flips to the other side of them rather than running off the
    /// window.
    #[test]
    fn a_pill_never_leaves_the_viewport() {
        let vp = Rect::new(0.0, 0.0, 800.0, 600.0);
        let corners = [
            (795.0, 595.0),
            (2.0, 2.0),
            (795.0, 2.0),
            (2.0, 595.0),
            (400.0, 598.0),
        ];
        for (x, y) in corners {
            let out = layout_peer_pills(vp, &[req(x, y, 90.0, 22.0)]);
            let r = out[0].rect;
            assert!(
                r.x0 >= vp.x0 && r.y0 >= vp.y0 && r.x1 <= vp.x1 && r.y1 <= vp.y1,
                "a peer at ({x}, {y}) put its name at {r:?}, outside {vp:?}",
            );
        }
    }

    /// A pill in its own corner needs no tether; a tether on every pill
    /// would be noise.
    #[test]
    fn only_a_displaced_pill_asks_for_a_tether() {
        let vp = Rect::new(0.0, 0.0, 800.0, 600.0);
        let alone = layout_peer_pills(vp, &[req(400.0, 300.0, 60.0, 20.0)]);
        assert!(!alone[0].tethered, "a peer on its own needs no tether");

        // Six peers stacked within a few pixels: the corners run out and
        // the later ones must be walked away and tethered.
        let pile: Vec<PillRequest> = (0..6)
            .map(|i| req(400.0 + f64::from(i), 300.0 + f64::from(i), 60.0, 20.0))
            .collect();
        let out = layout_peer_pills(vp, &pile);
        assert!(
            out.iter().any(|p| p.tethered),
            "a six-deep pile placed every pill in a corner, which cannot be",
        );
    }

    /// A peer reconnecting must not reshuffle everyone else's names.
    #[test]
    fn placement_does_not_depend_on_arrival_order() {
        let vp = Rect::new(0.0, 0.0, 800.0, 600.0);
        let a = req(400.0, 300.0, 60.0, 20.0);
        let b = req(404.0, 302.0, 70.0, 20.0);
        let c = req(410.0, 306.0, 50.0, 20.0);
        let forward = layout_peer_pills(vp, &[a, b, c]);
        let backward = layout_peer_pills(vp, &[c, b, a]);
        assert_eq!(forward[0], backward[2]);
        assert_eq!(forward[1], backward[1]);
        assert_eq!(forward[2], backward[0]);
    }

    #[test]
    fn a_peer_pill_is_the_peer_colour_and_its_label_contrasts() {
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        // Orange (bob's pin) is light enough that its label must be black.
        let peer = Color::from_rgb8(0xf9, 0x73, 0x16);
        draw_peer_cursors(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 200.0, 200.0),
            &mut tr,
            &theme,
            &[PeerCursor {
                pos: (60.0, 60.0),
                label: "bob".into(),
                color: peer,
            }],
        );
        let mut r = OffscreenRenderer::new(200, 200);
        let rgba = r.render(&scene, Color::WHITE);

        // The dot itself.
        let (dr, dg, db, _) = OffscreenRenderer::sample(&rgba, 200, 60, 60);
        assert!(
            dr > 180 && dg > 80 && dg < 160 && db < 90,
            "expected bob's orange at the pointer, got ({dr},{dg},{db})"
        );
        // The pill, below-right of the pointer.
        let (pr, pg, pb, pa) = OffscreenRenderer::sample(&rgba, 200, 80, 78);
        assert!(pa > 0, "expected an opaque pill below-right of the pointer");
        assert!(
            pr > pg && pg > pb,
            "the pill should be the peer's orange (R>G>B), got ({pr},{pg},{pb})"
        );
        assert_eq!(contrast_ink(peer), Color::BLACK, "orange needs black ink");
        assert_eq!(
            contrast_ink(Color::from_rgb8(0x63, 0x66, 0xf1)),
            Color::WHITE,
            "indigo needs white ink",
        );
    }

    #[test]
    #[allow(
        clippy::many_single_char_names,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "conventional short names for a color/angle sample; x/y are always in-bounds by construction"
    )]
    fn a_peer_dot_reads_on_a_white_page_and_on_a_dark_desktop() {
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let peer = Color::from_rgb8(0xf4, 0xf1, 0x71); // a pale ring member
        let mut render = |bg: Color| {
            let mut scene = Scene::new();
            draw_peer_cursors(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 120.0, 120.0),
                &mut tr,
                &theme,
                &[PeerCursor {
                    pos: (60.0, 60.0),
                    label: "p".into(),
                    color: peer,
                }],
            );
            let mut r = OffscreenRenderer::new(120, 120);
            r.render(&scene, bg)
        };
        for (name, bg) in [
            ("a white page", Color::WHITE),
            ("the desktop", Color::BLACK),
        ] {
            let rgba = render(bg);
            let bgc = bg.to_rgba8();
            // Somewhere on the ring around the dot must differ from the
            // background, or the cursor has no edge at all on it.
            //
            // `r`/`g`/`b`/`x`/`y`/`a`/`deg` are the conventional color and
            // polar-coordinate abbreviations; renaming them would only add
            // width. `x`/`y` are sampled on a fixed 6.8px ring around
            // (60, 60) on a 120x120 canvas, always in u32 range.
            let edge = (0..360).step_by(15).any(|deg| {
                let a = f64::from(deg).to_radians();
                let x = (60.0 + 6.8 * a.cos()).round() as u32;
                let y = (60.0 + 6.8 * a.sin()).round() as u32;
                let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 120, x, y);
                let d = super::super::color_distance((r, g, b), (bgc.r, bgc.g, bgc.b));
                d > 60.0
            });
            assert!(edge, "a pale peer has no visible edge against {name}");
        }
    }

    /// Asserted through `peer_pill_size`, shared by the pointer pill and
    /// the caret chip, so this fails if either drifts.
    #[test]
    fn a_peer_name_is_the_same_pill_everywhere() {
        let theme = Theme::default();
        let mut tr = crate::text_render::TextRenderer::new();

        // Control step, not caption — names are on screen for a whole
        // session.
        assert_eq!(
            pill_label_size(&theme),
            theme.tokens.typography.scale.sm,
            "a peer's name is a control label, not a caption",
        );

        let (w, h) = peer_pill_size(&mut tr, &theme, "ada");
        assert_eq!(
            (w, h),
            peer_pill_size(&mut tr, &theme, "ada"),
            "peer_pill_size is not stable for one label",
        );

        assert!(
            h >= 18.0,
            "a name pill {h}px tall is the unreadable one this replaced",
        );
        assert!(
            w > h,
            "a name pill should be wider than it is tall, got {w}x{h}"
        );

        // Wider pill, same height — height is the type, not the content.
        let (w2, h2) = peer_pill_size(&mut tr, &theme, "bob-with-a-long-name");
        assert!(w2 > w, "a longer name should get a wider pill");
        assert_eq!(h2, h, "pill height should not depend on the label");
    }

    /// Sampled just under the chip's bottom edge, where a shadow lands and
    /// a flat fill does not.
    #[test]
    fn a_caret_chip_carries_the_same_shadow_as_a_pointer_pill() {
        let theme = Theme::default();
        let user = UserId("alice".to_string());
        let ctx = PresenceCtx::empty();
        let mut tr = crate::text_render::TextRenderer::new();
        let (w, h) = peer_pill_size(&mut tr, &theme, "alice");

        let mut scene = Scene::new();
        let caret = Rect::new(20.0, 60.0, 22.0, 80.0);
        ctx.draw_peer_caret_labeled(
            &mut scene,
            &theme,
            &mut tr,
            Affine::IDENTITY,
            &user,
            "alice",
            caret,
        );
        let mut r = OffscreenRenderer::new(160, 160);
        let rgba = r.render(&scene, Color::WHITE);

        // The chip's own top edge is at caret.y0 - h - 2; a pixel just under
        // its bottom edge should be the shadow, darker than the white ground.
        let top = 60.0 - h - 2.0;
        // `top + h + 1.0` collapses to a fixed in-bounds sample row (59) on
        // the 160px-tall offscreen canvas, regardless of the chip height it
        // was derived from.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "top + h + 1.0 is a fixed, always-positive, in-bounds sample row"
        )]
        let (sr, sg, sb, _sa) = OffscreenRenderer::sample(&rgba, 160, 24, (top + h + 1.0) as u32);
        assert!(
            sr < 250 && sg < 250 && sb < 250,
            "expected a drop shadow under the caret's name chip, got \
             ({sr},{sg},{sb}) on a white ground — the chip is drawn flat \
             while the pointer's pill is not",
        );
        assert!(w > 0.0);
    }

    /// Renders the production `draw_peer_typing` for real — the band is read
    /// back off the framebuffer, not computed.
    fn marker_band(user: &str, paper: Color, theme: &Theme) -> (u8, u8, u8) {
        let ctx = PresenceCtx::empty();
        let mut scene = Scene::new();
        ctx.draw_peer_typing(
            &mut scene,
            theme,
            &UserId(user.to_string()),
            &[(Rect::new(0.0, 0.0, 40.0, 40.0), 1.0)],
        );
        let mut r = OffscreenRenderer::new(40, 40);
        let rgba = r.render(&scene, paper);
        let (red, green, blue, _) = OffscreenRenderer::sample(&rgba, 40, 20, 20);
        (red, green, blue)
    }

    /// [`marker_band`] for an explicit colour, so a test can walk every
    /// entry in [`super::super::PEER_PALETTE`], not just the ids that hash
    /// onto one. Goes in through `set_color_override`, the same door the
    /// shell uses, so this is still the production painter.
    fn marker_band_for(rgb: (u8, u8, u8), paper: Color, theme: &Theme) -> (u8, u8, u8) {
        let mut ctx = PresenceCtx::empty();
        let user = UserId("peer".to_string());
        ctx.set_color_override(
            user.clone(),
            &format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2),
        );
        let mut scene = Scene::new();
        ctx.draw_peer_typing(
            &mut scene,
            theme,
            &user,
            &[(Rect::new(0.0, 0.0, 40.0, 40.0), 1.0)],
        );
        let mut r = OffscreenRenderer::new(40, 40);
        let rgba = r.render(&scene, paper);
        let (red, green, blue, _) = OffscreenRenderer::sample(&rgba, 40, 20, 20);
        (red, green, blue)
    }

    fn lum(c: (u8, u8, u8)) -> f32 {
        relative_luminance(Color::from_rgb8(c.0, c.1, c.2))
    }

    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
        let (la, lb) = (lum(a), lum(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Every peer's marker lands at comparable strength against the page.
    /// See [`marker_pigment`].
    #[test]
    fn every_peers_marker_is_about_as_strong_as_every_others() {
        let theme = Theme::default();
        // Paper, as the word processor draws it in *either* palette.
        let paper = Color::from_rgb8(0xfa, 0xfa, 0xf8);
        let paper_rgb = (0xfa, 0xfa, 0xf8);

        let strengths: Vec<(f32, String)> = super::super::PEER_PALETTE
            .iter()
            .map(|rgb| {
                (
                    contrast(marker_band_for(*rgb, paper, &theme), paper_rgb),
                    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2),
                )
            })
            .collect();

        for (s, u) in &strengths {
            assert!(
                *s > 1.4,
                "{u}'s marker is invisible against the page ({s:.2}:1)"
            );
        }
        let lo = strengths.iter().map(|(s, _)| *s).fold(f32::MAX, f32::min);
        let hi = strengths.iter().map(|(s, _)| *s).fold(0.0f32, f32::max);
        assert!(
            hi / lo < 1.25,
            "markers differ too much between peers: {lo:.2}..{hi:.2} — {strengths:?}"
        );
    }

    /// Measures the band against *unmarked* ink, which is only right while
    /// the marker paints under the glyphs (see
    /// [`PresenceCtx::draw_peer_typing`]) — over them gets a number this
    /// test can't see.
    #[test]
    fn text_stays_legible_under_a_marker_in_both_palettes() {
        let paper = Color::from_rgb8(0xfa, 0xfa, 0xf8);
        let dark = Color::from_rgb8(0x18, 0x18, 0x1b);
        let ink = (0x1a, 0x1a, 0x1a);
        let dark_ink = (0xf5, 0xf5, 0xf5);

        // Every colour a peer can be drawn in, not just the ids that hash
        // onto one — an unhit palette entry is still a legibility promise.
        for rgb in super::super::PEER_PALETTE {
            for (theme, name) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
                let hex = format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2);
                let on_paper = contrast(marker_band_for(rgb, paper, &theme), ink);
                assert!(
                    on_paper >= 4.5,
                    "{hex}: body ink under the marker on paper ({name} theme) \
                     falls to {on_paper:.2}:1"
                );
                let on_dark = contrast(marker_band_for(rgb, dark, &theme), dark_ink);
                assert!(
                    on_dark >= 4.5,
                    "{hex}: ink under the marker on a dark surface ({name} theme) \
                     falls to {on_dark:.2}:1"
                );
            }
        }
    }

    /// Reduce-motion holds the marker at full strength instead of fading it
    /// — it still clears when the span expires; only the animation goes.
    #[test]
    fn reduce_motion_holds_the_marker_instead_of_dissolving_it() {
        let paper = Color::from_rgb8(0xfa, 0xfa, 0xf8);
        let paper_rgb = (0xfa, 0xfa, 0xf8);

        let half = |theme: &Theme| {
            let ctx = PresenceCtx::empty();
            let mut scene = Scene::new();
            ctx.draw_peer_typing(
                &mut scene,
                theme,
                &UserId("alice".to_string()),
                &[(Rect::new(0.0, 0.0, 40.0, 40.0), 0.25)],
            );
            let mut r = OffscreenRenderer::new(40, 40);
            let rgba = r.render(&scene, paper);
            let (red, green, blue, _) = OffscreenRenderer::sample(&rgba, 40, 20, 20);
            (red, green, blue)
        };

        let motion = Theme::default().with_reduce_motion(false);
        let still = Theme::default().with_reduce_motion(true);

        let faded = contrast(half(&motion), paper_rgb);
        let held = contrast(half(&still), paper_rgb);
        let fresh = contrast(marker_band("alice", paper, &motion), paper_rgb);

        assert!(
            faded < held,
            "with motion on, a quarter-aged mark should be weaker than a held one \
             ({faded:.2} vs {held:.2})"
        );
        assert!(
            (held - fresh).abs() < 0.02,
            "under reduce-motion an aged mark should read like a fresh one \
             ({held:.2} vs {fresh:.2})"
        );
    }

    /// The marker re-tunes lightness only; a peer's hue (what their caret is
    /// also coloured by) must survive it.
    #[test]
    fn marker_pigment_keeps_the_peers_hue() {
        for user in ["alice", "bob", "charlie", "me", "agent", "sara"] {
            let raw = super::super::user_color(user);
            let raw = Color::from_rgb8(raw.0, raw.1, raw.2);
            let pig = marker_pigment(raw);

            let a = raw.to_rgba8();
            let b = pig.to_rgba8();
            // Rank order of the channels is what carries hue; a pure lightness
            // change cannot reorder them.
            let order = |c: &vello::peniko::color::Rgba8| {
                let mut v = [(c.r, 0u8), (c.g, 1), (c.b, 2)];
                v.sort_by_key(|(x, _)| *x);
                [v[0].1, v[1].1, v[2].1]
            };
            assert_eq!(
                order(&a),
                order(&b),
                "{user}: marker pigment changed the hue ({a:?} -> {b:?})"
            );
        }
    }

    /// The edge band, too, is only for a pointer measured in this window's
    /// own coordinates — a desktop pointer is the compositor's to place.
    #[test]
    fn a_window_paints_no_band_for_a_pointer_it_cannot_place() {
        let theme = Theme::default();
        let mut tr = crate::text_render::TextRenderer::new();
        let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);

        let mut ctx = PresenceCtx::empty();
        ctx.set_own_identity(["me"]);
        ctx.set_display_name(UserId("peer@7".to_string()), "bob");
        // The frame a room really sends: a pointer keyed by the room, the
        // same key the compositor overlay and broker use.
        let _ = ctx.apply_remote_presence(
            "peer@7".to_string(),
            crate::presence::PresenceKey::Workspace {
                id: "room-1".to_string(),
            },
            crate::collaboration::PresencePayload::Pointer { x: 900.0, y: 512.0 },
        );
        assert_eq!(
            ctx.remote_cursors.len(),
            1,
            "the fixture published no pointer"
        );
        assert!(
            !ctx.remote_cursors[0].painted_by_window(),
            "fixture error: a room-keyed pointer is the compositor's to paint",
        );

        let mut scene = Scene::new();
        draw_remote_presence(
            &mut scene,
            Affine::IDENTITY,
            viewport,
            &mut tr,
            &theme,
            &ctx,
        );
        let mut r = OffscreenRenderer::new(320, 240);
        let painted = r.render(&scene, Color::WHITE);
        let mut b = OffscreenRenderer::new(320, 240);
        let bare = b.render(&Scene::new(), Color::WHITE);
        assert_eq!(
            OffscreenRenderer::pixels_differing_beyond_aa(&painted, &bare),
            0,
            "a window painted something for a pointer it cannot place",
        );

        // A click there goes to whatever's under it — hit-test must use the
        // same rule or pixels and clicks disagree.
        assert_eq!(
            ctx.hit_test_offscreen_mist(viewport, Point::new(318.0, 120.0)),
            None,
            "a band nobody can see still answered a click",
        );
    }

    /// The half that must keep working, or the rule above is satisfied by
    /// never drawing anything: a pointer this window does own still gets a
    /// band once it leaves the visible part of it.
    #[test]
    fn an_offscreen_peer_in_this_window_still_gets_a_band() {
        let theme = Theme::default();
        let mut tr = crate::text_render::TextRenderer::new();
        let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);

        let mut ctx = PresenceCtx::empty();
        ctx.set_own_identity(["me"]);
        ctx.set_display_name(UserId("peer@7".to_string()), "bob");
        // A widget scope is a position in this window's content, so this
        // window is the only layer that can place it.
        let _ = ctx.apply_remote_presence(
            "peer@7".to_string(),
            crate::presence::PresenceKey::Widget {
                path: vec![crate::presence::PathSegment::Index(0)],
            },
            crate::collaboration::PresencePayload::Pointer { x: 900.0, y: 120.0 },
        );
        assert!(
            ctx.remote_cursors[0].painted_by_window(),
            "fixture error: a widget-scoped pointer is this window's",
        );

        let mut scene = Scene::new();
        draw_remote_presence(
            &mut scene,
            Affine::IDENTITY,
            viewport,
            &mut tr,
            &theme,
            &ctx,
        );
        let mut r = OffscreenRenderer::new(320, 240);
        let painted = r.render(&scene, Color::WHITE);
        let mut b = OffscreenRenderer::new(320, 240);
        let bare = b.render(&Scene::new(), Color::WHITE);
        assert!(
            OffscreenRenderer::pixels_differing_beyond_aa(&painted, &bare) > 200,
            "a peer off the right edge of this window was not pointed at",
        );
        assert_eq!(
            ctx.hit_test_offscreen_mist(viewport, Point::new(318.0, 120.0)),
            Some("peer@7".to_string()),
            "the band that was painted does not answer a click on it",
        );
    }

    /// Clicking within an off-screen peer's mist band selects that peer to
    /// follow; a click in open space selects nobody.
    #[test]
    fn hit_test_offscreen_mist_selects_the_peer() {
        let mut ctx = PresenceCtx::empty();
        ctx.upsert_remote_cursor("bob".to_string(), [180.0, 50.0], None);
        let viewport = Rect::new(0.0, 0.0, 100.0, 100.0);
        // A point on the right-edge band.
        assert_eq!(
            ctx.hit_test_offscreen_mist(viewport, Point::new(98.0, 50.0)),
            Some("bob".to_string())
        );
        // A point in the middle of the canvas hits no band.
        assert_eq!(
            ctx.hit_test_offscreen_mist(viewport, Point::new(50.0, 50.0)),
            None
        );
    }
    #[test]
    fn resolved_desktop_peer_outside_viewport_becomes_edge_mist() {
        let theme = Theme::default();
        let mut text = crate::text_render::TextRenderer::new();
        let viewport = Rect::new(0.0, 0.0, 160.0, 120.0);
        let peers = [PeerCursor {
            pos: (300.0, 60.0),
            label: "Ada".to_string(),
            color: Color::from_rgb8(236, 72, 153),
        }];
        let mut scene = Scene::new();
        draw_peer_presence(
            &mut scene,
            Affine::IDENTITY,
            viewport,
            &mut text,
            &theme,
            &peers,
        );
        assert!(
            scene.encoding().draw_tags.len() >= 4,
            "an off-screen desktop peer produced no directional mist",
        );
    }
}
