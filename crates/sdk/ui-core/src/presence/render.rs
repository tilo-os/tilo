use super::PresenceCtx;
use crate::agent_identity::{AgentIdentityStyle, draw_agent_identity_mark};
use crate::color_compat::ColorCompat;
use crate::paint::PaintCtx;
use crate::permissions::UserId;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
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

/// A peer's colour as this theme paints it: their identity colour
/// ([`PresenceCtx::color_for`]) carried at the theme's presence lightness and
/// chroma, so every participant is equally loud on every material. Every
/// collab affordance resolves through this so they all agree on a peer's
/// colour.
pub(crate) fn color_for(ctx: &PresenceCtx, user: &UserId, theme: &Theme) -> Color {
    theme.tokens.colors.presence.of_rgb(ctx.color_for(user))
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
        if self.is_agent(user) {
            draw_agent_identity_mark(
                scene,
                transform,
                Rect::new(
                    rect.center().x - 4.5,
                    rect.y0 - 7.5,
                    rect.center().x + 4.5,
                    rect.y0 + 1.5,
                ),
                color,
                theme,
                AgentIdentityStyle::Solid,
            );
        }
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
    /// fault. Set by `apps/notes`/`apps/chat`; `tilo-doc` has no per-run author
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

        let is_agent = self.is_agent(user);
        let (chip_w, chip_h) = peer_pill_size_for(text_renderer, theme, name, is_agent);
        let cx = caret_rect.x0;
        let cy = caret_rect.y0 - chip_h - 2.0;
        let chip_rect = Rect::new(cx, cy, cx + chip_w, cy + chip_h);
        draw_peer_pill_kind(
            scene,
            transform,
            text_renderer,
            theme,
            chip_rect,
            name,
            color,
            is_agent,
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
    pub fn draw_peer_outline_labeled(
        &self,
        ctx: &mut PaintCtx<'_>,
        user: &UserId,
        name: &str,
        rect: Rect,
        radius: f64,
    ) {
        self.draw_peer_outline(ctx.scene, ctx.theme, ctx.transform, user, rect, radius);
        let color = color_for(self, user, ctx.theme);
        draw_name_chip(ctx, color, name, rect.origin());
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
    /// `Logo_Bug.png` into one named `Ada_Bug.png`. On an object too narrow to
    /// hold both, the chip stays within the object rather than escaping it —
    /// covering a name is recoverable, pointing at the wrong clip is not.
    pub fn draw_peer_outline_labeled_within(
        &self,
        ctx: &mut PaintCtx<'_>,
        user: &UserId,
        name: &str,
        rect: Rect,
        radius: f64,
        bounds: Rect,
    ) {
        let theme = ctx.theme;
        self.draw_peer_outline(ctx.scene, theme, ctx.transform, user, rect, radius);
        let color = color_for(self, user, theme);
        // Sideways the same rule applies: a chip that runs past the
        // surface's right edge is clipped to its first letters, so the name
        // is elided to what the surface can show and the chip slides left
        // to stay inside — at 2× a name is twice as wide, and objects near
        // the right edge lost their labels first.
        let (name, w, h) = chip_fitting(theme, ctx.text, name, bounds.width());
        let above = rect.y0 - h - CHIP_GAP;
        let chip = if above >= bounds.y0 {
            Rect::new(rect.x0, above, rect.x0 + w, above + h)
        } else {
            let x = (rect.x1 - w - CHIP_GAP).max(rect.x0 + CHIP_GAP);
            let y = rect.y0 + CHIP_GAP;
            Rect::new(x, y, x + w, y + h)
        };
        let x0 = chip.x0.min(bounds.x1 - w).max(bounds.x0);
        let chip = Rect::new(x0, chip.y0, x0 + w, chip.y1);
        draw_name_chip_in(ctx, color, &name, chip);
    }

    /// Semantic actions label the actual control beside it, not in the row
    /// above it: that row often contains a notice explaining whether the action
    /// is available. Ordinary peer selections retain their existing name chips.
    pub(crate) fn draw_action_outline_labeled_within(
        &self,
        ctx: &mut PaintCtx<'_>,
        user: &UserId,
        label: &str,
        rect: Rect,
        radius: f64,
        bounds: Rect,
    ) {
        self.draw_peer_outline(ctx.scene, ctx.theme, ctx.transform, user, rect, radius);
        let gap = f64::from(ctx.theme.tokens.spacing.s1);
        let right = (bounds.x1 - rect.x1 - gap).max(0.0);
        let left = (rect.x0 - bounds.x0 - gap).max(0.0);
        let (preferred_w, preferred_h) = chip_size(ctx.theme, ctx.text, label);
        let (budget, x, y) = if right >= preferred_w {
            (right, rect.x1 + gap, rect.y0)
        } else if left >= preferred_w {
            (left, rect.x0 - gap - preferred_w, rect.y0)
        } else if rect.y1 + gap + preferred_h <= bounds.y1 {
            (bounds.width(), rect.x0, rect.y1 + gap)
        } else if right >= left && right > 0.0 {
            (right, rect.x1 + gap, rect.y0)
        } else if left > 0.0 {
            (left, bounds.x0, rect.y0)
        } else {
            (bounds.width(), rect.x0, rect.y0)
        };
        let (label, w, h) = chip_fitting(ctx.theme, ctx.text, label, budget);
        if w <= 0.0 || h > bounds.height() {
            return;
        }
        let x = x.min(bounds.x1 - w).max(bounds.x0);
        let y = y.min(bounds.y1 - h).max(bounds.y0);
        draw_name_chip_in(
            ctx,
            color_for(self, user, ctx.theme),
            &label,
            Rect::new(x, y, x + w, y + h),
        );
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
///
/// The vertical pad wraps the *measured line box* (1.25 × the type size),
/// not the bare font size: at `scale.sm` that is the same 18 px pill as
/// before, but the label is centred in it instead of sitting flush to its
/// bottom edge, and the pill grows with the renderer's text scale instead
/// of the label spilling out of it.
pub(crate) const PILL_PAD_X: f64 = 7.0;
pub(crate) const PILL_PAD_Y: f64 = 1.5;
/// Room the agent's processor glyph takes ahead of the label, at 1×.
const AGENT_GLYPH_W: f64 = 14.0;

/// How much larger than the default the label paints: the theme's `sm`
/// type step over the default `sm`, times the renderer's text scale. Every
/// inset that frames a label is proportioned by this, so a 2× name gets 2×
/// breathing room instead of being pinned into 1× pads; at 1× it is
/// exactly 1, so no default metric moves. Reading the type step (not
/// `Theme::text_scale`) catches every way type grows — `with_text_scale`,
/// `with_ui_scale` (which doubles the type but leaves `text_scale` at 1)
/// and a theme with a larger base — and the renderer factor covers the
/// host-palette convention that scales the renderer instead.
pub(crate) fn label_magnification(theme: &Theme, _text_renderer: &TextRenderer) -> f64 {
    // The theme's resolved role already carries the text (and UI) scale, and
    // every label here is drawn in that resolved style, so the glyphs land
    // at exactly that size whether or not the renderer is scaled to match.
    // The magnification is therefore the role's ratio alone; multiplying by
    // the renderer's scale as well doubled the pads at 2x (X1).
    let stock = crate::tokens::Typography::default().size(TextRole::Caption);
    let type_ratio = theme.tokens.typography.size(TextRole::Caption) / stock;
    f64::from(type_ratio).max(0.5)
}

/// Peer name type: a caption, the smallest role, still readable for a
/// whole-session label.
pub(crate) fn pill_label_style(theme: &Theme) -> crate::text_render::TextStyle {
    theme.text(TextRole::Caption)
}

/// Pill size for `label`, padding included — shared so callers size their
/// box exactly like the painter.
#[cfg(test)]
pub(crate) fn peer_pill_size(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    label: &str,
) -> (f64, f64) {
    peer_pill_size_for(text_renderer, theme, label, false)
}

fn peer_pill_size_for(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    is_agent: bool,
) -> (f64, f64) {
    let style = pill_label_style(theme);
    // Both axes come from the same measurement the painter draws with, so
    // the pill fits its label at every renderer text scale; the horizontal
    // insets grow with the label's magnification so a 2× name is not pinned
    // into 1× pads.
    let k = label_magnification(theme, text_renderer);
    let (tw, th) = text_renderer.measure_style(label, None, &style);
    (
        f64::from(tw) + (PILL_PAD_X * 2.0 + if is_agent { AGENT_GLYPH_W } else { 0.0 }) * k,
        f64::from(th) + PILL_PAD_Y * 2.0,
    )
}

#[derive(Debug, Clone)]
pub struct PeerCursor {
    pub pos: (f64, f64),
    pub label: String,
    pub color: Color,
    /// Authenticated participant kind. Agents use diamond pointer geometry
    /// and an icon-bearing chip, so identity does not depend on reading text.
    pub is_agent: bool,
    /// `1.0` normally; falls towards `0.0` as a quiet peer's pointer fades
    /// out (see [`super::PresenceCtx::pointer_opacity`]).
    pub opacity: f32,
    /// How opaque the name beside the pointer is: whole while it moves,
    /// stepping aside once it stands still
    /// ([`super::PresenceCtx::pointer_name_opacity`]).
    pub name_opacity: f32,
    /// What they are saying beside their pointer (cursor chat). Shown in the
    /// name's place, under the name, whatever `name_opacity` is.
    pub chat: Option<super::ChatBubble>,
    /// A reaction floating up from the pointer.
    pub reaction: Option<super::ReactionFloat>,
}

impl PeerCursor {
    /// A pointer with its name shown and nothing said.
    #[must_use]
    pub fn new(pos: (f64, f64), label: impl Into<String>, color: Color) -> Self {
        Self {
            pos,
            label: label.into(),
            color,
            is_agent: false,
            opacity: 1.0,
            name_opacity: 1.0,
            chat: None,
            reaction: None,
        }
    }
}

/// Widest a cursor-chat bubble grows before its message wraps.
const CHAT_MAX_W: f64 = 240.0;
/// Inner padding of a cursor-chat bubble.
const CHAT_PAD_X: f64 = 10.0;
const CHAT_PAD_Y: f64 = 6.0;
/// Gap between the name and the message inside a bubble.
const CHAT_NAME_GAP: f64 = 1.0;
/// Room after a message for the caret being typed at its end.
const CARET_ROOM: f32 = 12.0;

/// The size of the name tag of `peer`, or of the bubble carrying its message
/// under its name while it talks.
fn peer_tag_size(text_renderer: &mut TextRenderer, theme: &Theme, peer: &PeerCursor) -> (f64, f64) {
    let Some(chat) = &peer.chat else {
        return peer_pill_size_for(text_renderer, theme, &peer.label, peer.is_agent);
    };
    let name = pill_label_style(theme);
    let body = chat_text_style(theme);
    let (nw, nh) = text_renderer.measure_style(&peer.label, None, &name);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a bubble a few hundred logical pixels wide"
    )]
    let wrap = (CHAT_MAX_W - CHAT_PAD_X * 2.0) as f32;
    let (bw, bh) = text_renderer.measure_style(&chat.text, Some(wrap), &body);
    (
        f64::from(nw.max(bw)) + CHAT_PAD_X * 2.0,
        f64::from(nh) + CHAT_NAME_GAP + f64::from(bh) + CHAT_PAD_Y * 2.0,
    )
}

/// Message type in a cursor-chat bubble: the body role, so it reads as
/// someone talking, not as a label.
fn chat_text_style(theme: &Theme) -> crate::text_render::TextStyle {
    theme.text(TextRole::Body)
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
                // the local pointer.
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
///
/// Returns the bounds, in scene coordinates, of everything painted: each
/// pointer's halo, each tether and each pill with its shadow.
pub fn draw_peer_cursors(
    scene: &mut Scene,
    transform: Affine,
    viewport: Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    peers: &[PeerCursor],
) -> Vec<Rect> {
    let mut painted = Vec::new();
    if peers.is_empty() {
        return painted;
    }
    // Only a shown name or a message takes room beside a pointer; a still
    // pointer whose name stepped aside leaves the place to others.
    let tagged: Vec<&PeerCursor> = peers
        .iter()
        .filter(|p| p.chat.is_some() || p.name_opacity > 0.0)
        .collect();
    let requests: Vec<PillRequest> = tagged
        .iter()
        .map(|p| PillRequest {
            at: p.pos,
            size: peer_tag_size(text_renderer, theme, p),
        })
        .collect();
    let placements = layout_peer_pills(viewport, &requests);

    for p in peers {
        let bounds = pointer_bounds(p.pos);
        painted.push(transform.transform_rect_bbox(bounds));
        faded(scene, transform, p.opacity, bounds, |scene| {
            draw_peer_pointer(scene, transform, theme, p.pos, p.color, p.is_agent);
        });
    }

    // Tethers — under the pills so a line never crosses a name.
    for (p, place) in tagged.iter().zip(&placements) {
        if !place.tethered {
            continue;
        }
        let from = Point::new(p.pos.0, p.pos.1);
        let to = nearest_point_on(place.rect, from);
        let mut line = vello::kurbo::BezPath::new();
        line.move_to(from);
        line.line_to(to);
        let bounds = Rect::from_points(from, to).inflate(FADE_EDGE, FADE_EDGE);
        painted.push(transform.transform_rect_bbox(bounds));
        faded(scene, transform, p.opacity, bounds, |scene| {
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                p.color.with_alpha_factor(0.75),
                None,
                &line,
            );
        });
    }

    for (p, place) in tagged.iter().zip(&placements) {
        let bounds = place.rect.inflate(FADE_EDGE, FADE_EDGE);
        painted.push(transform.transform_rect_bbox(bounds));
        if let Some(chat) = &p.chat {
            faded(
                scene,
                transform,
                p.opacity * chat.opacity,
                bounds,
                |scene| {
                    draw_chat_bubble(scene, transform, text_renderer, theme, place.rect, p, chat);
                },
            );
            continue;
        }
        faded(
            scene,
            transform,
            p.opacity * p.name_opacity,
            bounds,
            |scene| {
                draw_peer_pill_kind(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    place.rect,
                    &p.label,
                    p.color,
                    p.is_agent,
                );
            },
        );
    }

    // Reactions over everything: they are the moment's content.
    for p in peers {
        if let Some(reaction) = &p.reaction {
            painted.push(paint_reaction(
                scene,
                transform,
                text_renderer,
                theme,
                Point::new(p.pos.0, p.pos.1),
                reaction,
            ));
        }
    }
    painted
}

/// How far above the pointer's tip a reaction starts.
const REACTION_LIFT: f64 = 6.0;

/// A reaction's type: the emoji at a display size, from the emoji font.
fn reaction_style(theme: &Theme) -> crate::text_render::TextStyle {
    theme.text(TextRole::Display)
}

/// A cursor-chat bubble in the talker's colour: their name, then what they
/// say, wrapped. Contrast ink like the name pill; the name a step quieter.
fn draw_chat_bubble(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    peer: &PeerCursor,
    chat: &super::ChatBubble,
) {
    let text = paint_chat_bubble_frame(
        scene,
        transform,
        text_renderer,
        theme,
        rect,
        (&peer.label, peer.color),
    );
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a bubble a few hundred logical pixels wide"
    )]
    let wrap = text.width().max(1.0) as f32;
    text_renderer.draw_style(
        scene,
        &chat.text,
        contrast_ink(peer.color),
        transform * Affine::translate((text.x0, text.y0)),
        Some(wrap),
        &chat_text_style(theme),
    );
}

/// Paint a bubble's shape and the talker's name in it; returns where its
/// message goes.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo Affine composition is f64 matrix math; it cannot panic or overflow"
)]
fn paint_chat_bubble_frame(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    (label, color): (&str, Color),
) -> Rect {
    // Rounded except where it hangs from the pointer, like the name pill
    // grown into a speech bubble.
    let radius = f64::from(theme.tokens.radii.control) + 4.0;
    let shadow = RoundedRect::from_rect(rect + vello::kurbo::Vec2::new(0.0, 1.0), radius);
    scene.fill(
        Fill::NonZero,
        transform,
        Color::BLACK.with_alpha_factor(0.22),
        None,
        &shadow,
    );
    let shape = RoundedRect::from_rect(rect, (2.0, radius, radius, radius));
    scene.fill(Fill::NonZero, transform, color, None, &shape);
    let name = pill_label_style(theme);
    let (_, nh) = text_renderer.measure_style(label, None, &name);
    text_renderer.draw_style(
        scene,
        label,
        contrast_ink(color).with_alpha_factor(0.78),
        transform * Affine::translate((rect.x0 + CHAT_PAD_X, rect.y0 + CHAT_PAD_Y)),
        None,
        &name,
    );
    let top = rect.y0 + CHAT_PAD_Y + f64::from(nh) + CHAT_NAME_GAP;
    Rect::new(
        rect.x0 + CHAT_PAD_X,
        top,
        rect.x1 - CHAT_PAD_X,
        rect.y1 - CHAT_PAD_Y,
    )
}

/// This person's own cursor-chat bubble while they type, beside their
/// pointer at `at`: the same bubble everyone else sees, in their colour and
/// under their name, sized for `message` and at least `min_text_w` wide,
/// kept inside `bounds` (left of or above the pointer near an edge).
/// Paints its shape and name and returns the bubble and the one-line rect
/// the caller puts its text field in.
pub fn paint_own_chat_bubble(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    (at, label, color): (Point, &str, Color),
    (message, min_text_w, bounds): (&str, f64, Rect),
) -> (Rect, Rect) {
    let name = pill_label_style(theme);
    let body = chat_text_style(theme);
    let (nw, nh) = text_renderer.measure_style(label, None, &name);
    let (tw, _) = text_renderer.measure_style(message, None, &body);
    let line = f64::from(body.line_height_px());
    let text_w = f64::from(nw.max(tw + CARET_ROOM))
        .max(min_text_w)
        .min(CHAT_MAX_W - CHAT_PAD_X * 2.0);
    let width = text_w + CHAT_PAD_X * 2.0;
    let height = f64::from(nh) + CHAT_NAME_GAP + line + CHAT_PAD_Y * 2.0;
    let gap = RING_R + PILL_GAP;
    let x0 = if at.x + gap + width > bounds.x1 {
        at.x - gap - width
    } else {
        at.x + gap
    };
    let y0 = if at.y + gap + height > bounds.y1 {
        at.y - gap - height
    } else {
        at.y + gap
    };
    let rect = Rect::new(x0, y0, x0 + width, y0 + height);
    let text =
        paint_chat_bubble_frame(scene, transform, text_renderer, theme, rect, (label, color));
    (rect, text)
}

/// A reaction rising from a pointer at `at`; returns where it painted.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo Affine composition is f64 matrix math; it cannot panic or overflow"
)]
pub fn paint_reaction(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    at: Point,
    reaction: &super::ReactionFloat,
) -> Rect {
    let style = reaction_style(theme);
    let (w, h) = text_renderer.measure_style(&reaction.emoji, None, &style);
    let (w, h) = (f64::from(w), f64::from(h));
    let rect = Rect::new(
        at.x - w / 2.0,
        at.y - h - REACTION_LIFT - reaction.rise,
        at.x + w / 2.0,
        at.y - REACTION_LIFT - reaction.rise,
    );
    let bounds = rect.inflate(FADE_EDGE, FADE_EDGE);
    faded(scene, transform, reaction.opacity, bounds, |scene| {
        text_renderer.draw_style(
            scene,
            &reaction.emoji,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((rect.x0, rect.y0)),
            None,
            &style,
        );
    });
    transform.transform_rect_bbox(bounds)
}

/// Where a pointer's mark paints, its outline and shadow included, with two
/// pixels for antialiasing: where it repaints when it moves, or when a
/// scroll copies beneath it. The arrow hangs down and right of its tip; an
/// agent's mark is centred on it.
fn pointer_bounds(pos: (f64, f64)) -> Rect {
    let arrow = Rect::new(
        -ARROW_EDGE,
        -ARROW_EDGE,
        ARROW_W + ARROW_EDGE,
        ARROW_H + ARROW_EDGE,
    );
    let agent = Rect::from_center_size(Point::ORIGIN, (2.0 * RING_R + 6.0, 2.0 * RING_R + 6.0));
    arrow.union(agent).inflate(2.0, 2.0) + vello::kurbo::Vec2::new(pos.0, pos.1)
}

/// The arrow's extent from its tip, and the room its outline and shadow take.
const ARROW_W: f64 = 12.0;
const ARROW_H: f64 = 18.6;
const ARROW_EDGE: f64 = 2.5;
/// Room around a pill or tether for its stroke and shadow.
const FADE_EDGE: f64 = 8.0;

/// Paint `draw` at `opacity`: directly when opaque, not at all when fully
/// faded, else through a layer clipped to `bounds`, so overlapping parts of
/// one peer's mark fade as one rather than showing through each other.
fn faded(
    scene: &mut Scene,
    transform: Affine,
    opacity: f32,
    bounds: Rect,
    draw: impl FnOnce(&mut Scene),
) {
    if opacity >= 1.0 {
        draw(scene);
        return;
    }
    if opacity <= 0.0 {
        return;
    }
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        opacity,
        transform,
        &bounds,
    );
    draw(scene);
    scene.pop_layer();
}

/// The outline of a person's pointer: an arrow with its tip at the origin,
/// the shape everyone reads as "someone is pointing here".
fn arrow_path() -> vello::kurbo::BezPath {
    let mut path = vello::kurbo::BezPath::new();
    path.move_to((0.0, 0.0));
    path.line_to((0.0, 16.0));
    path.line_to((4.2, 12.2));
    path.line_to((7.0, ARROW_H));
    path.line_to((9.6, 17.5));
    path.line_to((6.9, 11.3));
    path.line_to((ARROW_W, 11.3));
    path.close_path();
    path
}

/// A peer's pointer: a person's is an arrow in their colour with a white
/// outline and a soft shadow, which together separate it from dark and
/// light ground alike; an agent's is the shared agent mark, so software is
/// never drawn as a person.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
)]
fn draw_peer_pointer(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    pos: (f64, f64),
    peer: Color,
    is_agent: bool,
) {
    if is_agent {
        draw_agent_pointer(scene, transform, theme, pos, peer);
        return;
    }
    let t = transform * Affine::translate((pos.0, pos.1));
    let arrow = arrow_path();
    scene.fill(
        Fill::NonZero,
        t * Affine::translate((0.0, 1.0)),
        Color::BLACK.with_alpha_factor(0.28),
        None,
        &arrow,
    );
    let outline = Stroke::new(2.5).with_join(vello::kurbo::Join::Round);
    scene.stroke(&outline, t, Color::WHITE, None, &arrow);
    scene.fill(Fill::NonZero, t, peer, None, &arrow);
}

/// An agent's pointer: halo, shadow edge and the agent mark. Shadow and ring
/// together work on both light and dark backgrounds — ring separates
/// dark-on-dark, shadow separates light-on-light; whichever isn't needed
/// stays invisible.
/// `kurbo::Affine` composition below is plain f64 matrix math with no
/// panic path; `arithmetic_side_effects` flags the `*` operator overload
/// anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
)]
fn draw_agent_pointer(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    pos: (f64, f64),
    peer: Color,
) {
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
    let radius = DOT_R + 1.0;
    draw_agent_identity_mark(
        scene,
        t,
        Rect::new(-radius, -radius, radius, radius),
        peer,
        theme,
        AgentIdentityStyle::Solid,
    );
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo Rect/Affine arithmetic is f64 math; it cannot panic or overflow, unlike integer +/-/*"
)]
fn draw_peer_pill_kind(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    peer: Color,
    is_agent: bool,
) {
    let label_style = pill_label_style(theme);
    let radius = f64::from(theme.tokens.radii.detail);
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
    // Insets and the agent glyph are proportioned to the label's
    // magnification (see `peer_pill_size_for`), so the pill the sizer
    // promised is the pill the painter fills.
    let k = label_magnification(theme, text_renderer);
    let text_x = rect.x0 + (PILL_PAD_X + if is_agent { AGENT_GLYPH_W } else { 0.0 }) * k;
    if is_agent {
        // The same processor mark used by every agent identity surface.
        // Unlike an "agent" suffix, it remains recognizable when elided.
        let badge = Rect::new(
            rect.x0 + 5.0 * k,
            rect.center().y - 4.5 * k,
            rect.x0 + 14.0 * k,
            rect.center().y + 4.5 * k,
        );
        draw_agent_identity_mark(
            scene,
            transform,
            badge,
            peer,
            theme,
            AgentIdentityStyle::Solid,
        );
    }
    // Centre the measured line box in whatever rect the placer handed us,
    // rather than pinning it to the top pad: a caller-supplied rect may be
    // taller than the pill asked for.
    let (_, th) = text_renderer.measure_style(label, None, &label_style);
    let text_y = rect.y0 + (rect.height() - f64::from(th)) / 2.0;
    text_renderer.draw_style(
        scene,
        label,
        contrast_ink(peer),
        transform * Affine::translate((text_x, text_y)),
        None,
        &label_style,
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
        .map(|cursor| peer_of(presence, cursor))
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
fn peer_of(ctx: &PresenceCtx, cursor: &super::RemoteCursor) -> PeerCursor {
    PeerCursor {
        pos: (f64::from(cursor.position[0]), f64::from(cursor.position[1])),
        label: cursor.label.clone(),
        color: Color::from_rgba8(
            (cursor.color[0].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[1].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[2].clamp(0.0, 1.0) * 255.0) as u8,
            255,
        ),
        is_agent: ctx.is_agent(&UserId(cursor.user_id.clone())),
        opacity: 1.0,
        name_opacity: 1.0,
        chat: None,
        reaction: None,
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
    let label_style = theme.text(TextRole::Caption);
    // Height from the measured line box, not the font size: the box is a
    // quarter taller than the size and carries the renderer's text scale,
    // so sizing from `label_style` clipped descenders at 1× and lost the
    // whole label below the chip at 2×. The side pads scale with the label
    // for the same reason (`label_magnification`).
    let k = label_magnification(theme, text_renderer);
    let (tw, th) = text_renderer.measure_style(name, None, &label_style);
    (
        f64::from(tw) + CHIP_PAD_X * 2.0 * k,
        f64::from(th) + CHIP_PAD_Y * 2.0,
    )
}

/// [`chip_size`] for a chip that has to fit inside `max_w`: the name is
/// elided to the room left after the pads, so the chip never has to leave
/// the surface to show its label. Returns the label to paint and the chip's
/// size; a name that already fits comes back untouched.
fn chip_fitting(
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    name: &str,
    max_w: f64,
) -> (String, f64, f64) {
    let (w, h) = chip_size(theme, text_renderer, name);
    if w <= max_w {
        return (name.to_string(), w, h);
    }
    let pad_x = CHIP_PAD_X * label_magnification(theme, text_renderer);
    let budget = (max_w - pad_x * 2.0).max(0.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pixel budget well inside f32 range"
    )]
    let label =
        text_renderer.elide_to_width_style(name, budget as f32, &theme.text(TextRole::Caption));
    let (w, h) = chip_size(theme, text_renderer, &label);
    (label, w.min(max_w), h)
}

/// Paint the chip into a rectangle the caller has already chosen.
fn draw_name_chip_in(ctx: &mut PaintCtx<'_>, color: Color, name: &str, chip_rect: Rect) {
    let theme = ctx.theme;
    // Solid fill in the peer's colour so the chip reads as "them".
    ctx.fill_round(chip_rect, f64::from(theme.tokens.radii.detail), color);
    // Label ink: white or near-black for contrast against the colour; the
    // measured line box is centred in the chip.
    let label_style = theme.text(TextRole::Caption);
    let pad_x = CHIP_PAD_X * label_magnification(theme, ctx.text);
    let (_, th) = ctx.text.measure_style(name, None, &label_style);
    let text_y = chip_rect.y0 + (chip_rect.height() - f64::from(th)) / 2.0;
    ctx.text(
        name,
        &label_style.clone(),
        contrast_ink(color),
        Point::new(chip_rect.x0 + pad_x, text_y),
        None,
    );
}

fn draw_name_chip(ctx: &mut PaintCtx<'_>, color: Color, name: &str, anchor: Point) {
    let (w, h) = chip_size(ctx.theme, ctx.text, name);
    let cy = anchor.y - h - CHIP_GAP;
    draw_name_chip_in(
        ctx,
        color,
        name,
        Rect::new(anchor.x, cy, anchor.x + w, cy + h),
    );
}

/// The ring a speaking participant's tile wears over its picture: their
/// colour, as strong as their voice is now (`level`, 0 to 1), a chrome rule
/// thick on each side of the tile's edge.
pub fn paint_speaking_ring(
    scene: &mut Scene,
    transform: Affine,
    tile: RoundedRect,
    colour: Color,
    level: f32,
    theme: &Theme,
) {
    if level <= 0.0 {
        return;
    }
    let ring = 2.0 * f64::from(theme.tokens.metrics.chrome_rule_px);
    let rect = tile.rect();
    let radius = tile.radii().top_left;
    scene.stroke(
        &Stroke::new(ring),
        transform,
        colour.multiply_alpha(level),
        None,
        &RoundedRect::from_rect(rect.inset(-ring / 2.0), (radius - ring / 2.0).max(0.0)),
    );
}

/// The frame round the work while following someone: their colour, a
/// chrome rule either side of `area`'s edge at the tile radius, as present
/// as the follow is (`openness`, 0 to 1).
pub fn paint_follow_frame(
    scene: &mut Scene,
    transform: Affine,
    area: Rect,
    colour: Color,
    openness: f32,
    theme: &Theme,
) {
    if openness <= 0.0 {
        return;
    }
    let rule = 2.0 * f64::from(theme.tokens.metrics.chrome_rule_px);
    scene.stroke(
        &Stroke::new(rule),
        transform,
        colour.multiply_alpha(openness),
        None,
        &RoundedRect::from_rect(area.inset(-rule / 2.0), f64::from(theme.tokens.radii.tile)),
    );
}

/// The ring round the pointer of the person being followed, in their
/// colour; it stays within [`FOLLOWED_POINTER_REACH`] of the pointer.
pub fn paint_followed_pointer(scene: &mut Scene, transform: Affine, point: Point, colour: Color) {
    scene.stroke(
        &Stroke::new(FOLLOWED_RING_STROKE),
        transform,
        colour,
        None,
        &vello::kurbo::Circle::new(point, FOLLOWED_RING_RADIUS),
    );
}

/// The followed pointer's ring: its radius and stroke.
const FOLLOWED_RING_RADIUS: f64 = 13.0;
const FOLLOWED_RING_STROKE: f64 = 2.5;
/// How far from the pointer its ring reaches, antialiased edge included.
pub const FOLLOWED_POINTER_REACH: f64 = FOLLOWED_RING_RADIUS + FOLLOWED_RING_STROKE;

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

/// The quieter ink on a person's colour (a placeholder, a hint): the
/// [`contrast_ink`] at the share a secondary line takes.
#[must_use]
pub fn contrast_ink_dim(bg: Color) -> Color {
    contrast_ink(bg).with_alpha(SECONDARY_INK_ON_COLOUR)
}

/// The share of the contrast ink a secondary line on a person's colour
/// shows.
const SECONDARY_INK_ON_COLOUR: f32 = 0.7;

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
    let alpha = 0.82 * (1.0 - 0.35 * far);

    // `band` remains the comfortably large pointer target used by
    // hit_test_offscreen_mist. Paint a much smaller edge rail inside it so the
    // cue reads as an intentional direction marker, not a clipped panel.
    let rail_depth = 7.0_f64.min(band.width()).min(band.height());
    let rail_extent = 46.0_f64;
    let rail = match mist.edge {
        MistEdge::Left => Rect::new(
            band.x0,
            band.center().y - rail_extent / 2.0,
            band.x0 + rail_depth,
            band.center().y + rail_extent / 2.0,
        ),
        MistEdge::Right => Rect::new(
            band.x1 - rail_depth,
            band.center().y - rail_extent / 2.0,
            band.x1,
            band.center().y + rail_extent / 2.0,
        ),
        MistEdge::Top => Rect::new(
            band.center().x - rail_extent / 2.0,
            band.y0,
            band.center().x + rail_extent / 2.0,
            band.y0 + rail_depth,
        ),
        MistEdge::Bottom => Rect::new(
            band.center().x - rail_extent / 2.0,
            band.y1 - rail_depth,
            band.center().x + rail_extent / 2.0,
            band.y1,
        ),
    };
    let rail_shape = RoundedRect::from_rect(rail, rail_depth / 2.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        peer.with_alpha_factor(alpha),
        None,
        &rail_shape,
    );

    let Some((tag_rect, label, pad)) = mist_tag(text_renderer, theme, band, mist.edge, name) else {
        return;
    };
    let tag = RoundedRect::from_rect(tag_rect, f64::from(theme.tokens.radii.detail));
    scene.fill(Fill::NonZero, Affine::IDENTITY, peer, None, &tag);
    text_renderer.draw_style(
        scene,
        &label,
        contrast_ink(peer),
        Affine::translate((tag_rect.x0 + pad, tag_rect.y0 + pad / 2.0)),
        None,
        &theme.text(TextRole::Caption),
    );
}

/// Inset of the name tag from the mist band's edges. A band inset, not a
/// label pad: the band is also the hit rect ([`mist_band_rect`]), so the
/// tag must never paint outside it.
const TAG_INSET: f64 = 4.0;

/// The edge tag's rect, its label (elided to what the band can hold) and
/// its label pad — or `None` when the band cannot show a single character.
///
/// The tag lives *inside* the band it decorates: on a left/right band that
/// is the band's 44 px depth, on a top/bottom band its 140 px extent. Until
/// this clamp the tag was as wide as the name, so a long name (or any name
/// at 2×) hung out over content the band's hit-test knew nothing about —
/// a painted rect the pointer could not reach.
fn mist_tag(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    band: Rect,
    edge: super::geom::MistEdge,
    name: &str,
) -> Option<(Rect, String, f64)> {
    use super::geom::MistEdge;
    let label_style = theme.text(TextRole::Caption);
    // The band is fixed chrome, so — unlike a pill's — the tag's pad does
    // not grow with the type: in a 44 px band a scaled pad would leave the
    // label no room at all.
    let pad = TAG_INSET;
    let max_tag_w = band.width() - TAG_INSET * 2.0;
    let budget = max_tag_w - pad * 2.0;
    if budget <= 0.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pixel budget well inside f32 range"
    )]
    let label = text_renderer.elide_to_width_style(name, budget as f32, &label_style);
    // A tag that keeps no letter of the name says nothing the mist's colour
    // does not already say; leave it out rather than paint a lone ellipsis.
    if label.trim_end_matches('…').is_empty() {
        return None;
    }
    // Sized from the measured line box so the tag holds its label at every
    // text scale (see `chip_size`).
    let (tw, th) = text_renderer.measure_style(&label, None, &label_style);
    let tag_w = (f64::from(tw) + pad * 2.0).min(max_tag_w);
    let tag_h = f64::from(th) + pad;
    // A top/bottom band is only as deep as a side band is wide; a line box
    // that no longer fits it (3× type and up) is not painted over content
    // the band does not hit-test — the mist alone still points.
    if tag_h > band.height() - TAG_INSET * 2.0 {
        return None;
    }
    // Anchor the tag near the band centre, nudged inward from the edge.
    let (tx, ty) = match edge {
        MistEdge::Left => (band.x0 + TAG_INSET, band.center().y - tag_h / 2.0),
        MistEdge::Right => (band.x1 - tag_w - TAG_INSET, band.center().y - tag_h / 2.0),
        MistEdge::Top => (band.center().x - tag_w / 2.0, band.y0 + TAG_INSET),
        MistEdge::Bottom => (band.center().x - tag_w / 2.0, band.y1 - tag_h - TAG_INSET),
    };
    Some((Rect::new(tx, ty, tx + tag_w, ty + tag_h), label, pad))
}

/// Paint already-resolved peer cursors in one coordinate space. On-screen
/// peers get moving dots and collision-free name pills; off-screen peers
/// become directional edge mist. This is the compositor-facing sibling of
/// [`draw_remote_presence`], which resolves cursors from a [`PresenceCtx`].
///
/// Returns the bounds, in scene coordinates, of everything painted (see
/// [`draw_peer_cursors`]).
pub fn draw_peer_presence(
    scene: &mut Scene,
    transform: Affine,
    viewport: vello::kurbo::Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    peers: &[PeerCursor],
) -> Vec<Rect> {
    let mut on_screen = Vec::new();
    let mut painted = Vec::new();
    for peer in peers {
        let target = Point::new(peer.pos.0, peer.pos.1);
        if let Some(mist) = super::geom::edge_mist_for(viewport, target) {
            // Drawn untransformed; the rail and the name tag stay inside
            // the band.
            painted.push(mist_band_rect(viewport, &mist).inflate(FADE_EDGE, FADE_EDGE));
            faded(scene, Affine::IDENTITY, peer.opacity, viewport, |scene| {
                draw_edge_mist(
                    scene,
                    text_renderer,
                    theme,
                    viewport,
                    &mist,
                    peer.color,
                    &peer.label,
                );
            });
        } else {
            on_screen.push(peer.clone());
        }
    }
    painted.extend(draw_peer_cursors(
        scene,
        transform,
        viewport,
        text_renderer,
        theme,
        &on_screen,
    ));
    painted
}

/// The one call the app makes each frame to render all cross-process peer
/// presence: on-screen peers get a dot + pill, off-screen peers get an edge
/// mist. `viewport` is the visible rect in the same (logical) space as
/// cursor positions.
///
/// Returns the bounds, in scene coordinates, of everything painted (see
/// [`draw_peer_cursors`]), so the frame damages only those.
pub fn draw_remote_presence(
    scene: &mut Scene,
    transform: Affine,
    viewport: vello::kurbo::Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) -> Vec<Rect> {
    let mut painted = Vec::new();
    if presence.remote_cursors.is_empty() {
        return painted;
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
        let peer = peer_of(presence, cursor);
        match super::geom::edge_mist_for(viewport, target) {
            Some(mist) => {
                // Drawn untransformed, inside the band (as in
                // `draw_peer_presence`).
                painted.push(mist_band_rect(viewport, &mist).inflate(FADE_EDGE, FADE_EDGE));
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
    painted.extend(draw_peer_cursors(
        scene,
        transform,
        viewport,
        text_renderer,
        theme,
        &on_screen,
    ));
    painted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offscreen::OffscreenRenderer;
    use crate::permissions::UserId;

    #[test]
    fn action_labels_preserve_the_notice_above_the_actual_control() {
        let presence = PresenceCtx::empty();
        let user = UserId("charlie".into());
        let viewport = Rect::new(20.0, 20.0, 480.0, 180.0);
        for theme in [Theme::default(), Theme::dark().with_text_scale(2.0)] {
            for control in [
                Rect::new(40.0, 60.0, 100.0, 90.0),
                Rect::new(400.0, 60.0, 460.0, 90.0),
                Rect::new(100.0, 60.0, 400.0, 90.0),
            ] {
                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                presence.draw_action_outline_labeled_within(
                    &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
                    &user,
                    "Rename (Applying) - Quill",
                    control,
                    f64::from(theme.tokens.radii.detail),
                    viewport,
                );
                let rgba = OffscreenRenderer::new(500, 200).render(&scene, Color::BLACK);
                let mut label_pixels = 0;
                for y in 0..200 {
                    for x in 0..500 {
                        let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 500, x, y);
                        if r == 0 && g == 0 && b == 0 {
                            continue;
                        }
                        let point = Point::new(f64::from(x), f64::from(y));
                        assert!(viewport.contains(point), "activity escaped its surface");
                        assert!(
                            y >= 59,
                            "activity painted over the notice above the control"
                        );
                        if !control.inflate(2.0, 2.0).contains(point) {
                            label_pixels += 1;
                        }
                    }
                }
                assert!(
                    label_pixels > 100,
                    "the action label must actually be painted"
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "`at` is a fixed in-bounds test coordinate, always representable as u32"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn pointers_and_carets_paint_in_the_peer_colour_with_their_marker() {
        // phase: only_a_pointer_this_layer_owns_reaches_the_screen
        // "One person, one pointer": painted once when this layer owns it, not
        // at all when the compositor overlay does. Both halves are asserted in
        // one test — either alone would pass a filter that always/never draws.
        {
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
                // Inside the arrow, just under its tip.
                OffscreenRenderer::sample(&rgba, 120, at[0] as u32 + 3, at[1] as u32 + 8)
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
        // phase: peer_caret_paints_peer_color
        // "charlie" maps to emerald (0x10, 0xb9, 0x81): green dominant.
        {
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
        // phase: agent_caret_has_the_shared_non_text_identity_marker
        {
            let theme = Theme::default();
            let agent = UserId("peer@agent".to_string());
            let mut agent_ctx = PresenceCtx::empty();
            agent_ctx.set_participant_kind(agent.clone(), super::super::ParticipantKind::Agent);
            let mut person_ctx = PresenceCtx::empty();
            person_ctx.set_participant_kind(agent.clone(), super::super::ParticipantKind::Person);
            let caret = Rect::new(30.0, 40.0, 32.0, 60.0);

            let render = |ctx: &PresenceCtx| {
                let mut scene = Scene::new();
                ctx.draw_peer_caret(&mut scene, &theme, Affine::IDENTITY, &agent, caret);
                let mut renderer = OffscreenRenderer::new(80, 80);
                renderer.render(&scene, Color::BLACK)
            };
            let agent_pixels = render(&agent_ctx);
            let person_pixels = render(&person_ctx);

            // Three pixels above the ordinary caret, only the agent's processor
            // marker occupies this point. This remains distinguishable without
            // colour perception and before any label is painted.
            let (ar, ag, ab, _) = OffscreenRenderer::sample(&agent_pixels, 80, 31, 37);
            let (pr, pg, pb, _) = OffscreenRenderer::sample(&person_pixels, 80, 31, 37);
            assert!(
                u16::from(ar) + u16::from(ag) + u16::from(ab)
                    > u16::from(pr) + u16::from(pg) + u16::from(pb)
            );
        }
        // phase: labeled_caret_paints_bar_and_chip_above
        // Samples both the caret bar and the name chip above it. "charlie" is
        // emerald (G dominant).
        {
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
        // phase: a_caret_chip_carries_the_same_shadow_as_a_pointer_pill
        // Sampled just under the chip's bottom edge, where a shadow lands and
        // a flat fill does not.
        {
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
            let (sr, sg, sb, _sa) =
                OffscreenRenderer::sample(&rgba, 160, 24, (top + h + 1.0) as u32);
            assert!(
                sr < 250 && sg < 250 && sb < 250,
                "expected a drop shadow under the caret's name chip, got \
             ({sr},{sg},{sb}) on a white ground — the chip is drawn flat \
             while the pointer's pill is not",
            );
            assert!(w > 0.0);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn objects_and_typing_are_tinted_outlined_and_chipped_in_the_peer_colour() {
        // phase: peer_mist_tints_element_in_peer_color
        // "bob" maps to orange (0xf9, 0x73, 0x16): red-high, green-mid,
        // blue-low. Translucent over black, the channel ordering still holds.
        {
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
        // phase: peer_outline_rings_the_object_and_washes_its_middle
        // A peer's selected object gets both halves: a ring on the boundary
        // and a wash across the middle. Either alone fails somewhere — a bare
        // wash reads as the app's own hover, a bare ring vanishes over a busy
        // fill — so both are asserted, and the ring is asserted to be the
        // *stronger* of the two or it is not a boundary any more.
        //
        // "charlie" is emerald (0x10, 0xb9, 0x81), so both should read G-dominant.
        {
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
        // phase: a_labelled_outline_chips_the_peers_name_above_the_object
        // The named variant puts the same chip above the object that
        // `draw_peer_caret_labeled` puts above a caret — one chip design, so a
        // person's caret and their selected object read as the same person.
        {
            let ctx = PresenceCtx::empty();
            let theme = Theme::default();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let user = UserId("charlie".to_string());
            ctx.draw_peer_outline_labeled(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
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
        // phase: a_name_chip_with_no_room_above_it_comes_inside_the_object
        // On the top row of a surface that clips its contents there is no room
        // above an object for its name chip, and hanging one there anyway gets
        // it sliced by the clip — a collaborator labelled with the bottom two
        // pixels of their name. The chip has to come inside instead.
        {
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
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
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
        // phase: a_name_chip_with_room_above_it_still_hangs_above
        // With room above, the chip still hangs above — the flip is a fallback,
        // not a new default, or every labelled object would start covering itself.
        {
            let ctx = PresenceCtx::empty();
            let theme = Theme::default();
            let mut tr = TextRenderer::new();
            let user = UserId("charlie".to_string());
            let bounds = Rect::new(0.0, 0.0, 200.0, 140.0);
            let object = Rect::new(10.0, 60.0, 160.0, 120.0);

            let mut scene = Scene::new();
            ctx.draw_peer_outline_labeled_within(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
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
        // phase: typing_tint_paints_peer_color_over_run
        // The typing highlighter paints the peer's color over the typed run.
        // "bob" is orange (R>G>B). Fresh (alpha 1.0) over black still keeps
        // that channel ordering.
        {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn offscreen_peers_get_a_band_only_where_the_window_can_place_them() {
        // phase: offscreen_peer_mists_the_correct_edge
        // A peer off to the RIGHT tints the right edge and leaves the left
        // edge clear.
        {
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
        // phase: a_window_paints_no_band_for_a_pointer_it_cannot_place
        // The edge band, too, is only for a pointer measured in this window's
        // own coordinates — a desktop pointer is the compositor's to place.
        {
            let theme = Theme::default();
            let mut tr = crate::text_render::TextRenderer::new();
            let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);

            let mut ctx = PresenceCtx::empty();
            ctx.set_own_identity(["me"]);
            ctx.set_display_name(UserId("peer@7".to_string()), "Xaver");
            // The frame a room sends: a pointer keyed by the room, the
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
        // phase: an_offscreen_peer_in_this_window_still_gets_a_band
        // The half that must keep working, or the rule above is satisfied by
        // never drawing anything: a pointer this window does own still gets a
        // band once it leaves the visible part of it.
        {
            let theme = Theme::default();
            let mut tr = crate::text_render::TextRenderer::new();
            let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);

            let mut ctx = PresenceCtx::empty();
            ctx.set_own_identity(["me"]);
            ctx.set_display_name(UserId("peer@7".to_string()), "Xaver");
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
        // phase: hit_test_offscreen_mist_selects_the_peer
        // Clicking within an off-screen peer's mist band selects that peer to
        // follow; a click in open space selects nobody.
        {
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
        // phase: resolved_desktop_peer_outside_viewport_becomes_edge_mist
        {
            let theme = Theme::default();
            let mut text = crate::text_render::TextRenderer::new();
            let viewport = Rect::new(0.0, 0.0, 160.0, 120.0);
            let peers = [PeerCursor {
                pos: (300.0, 60.0),
                label: "Ada".to_string(),
                color: Color::from_rgb8(236, 72, 153),
                is_agent: false,
                opacity: 1.0,
                name_opacity: 1.0,
                chat: None,
                reaction: None,
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
            // The rail and label background are the two geometry draws that
            // prove an off-screen cue was emitted. Glyph batching is deliberately
            // renderer-dependent, so counting the label's glyph draws made this
            // test vary between the local and CI render backends.
            assert!(
                scene.encoding().draw_tags.len() >= 2,
                "an off-screen desktop peer produced no directional mist",
            );
        }
    }

    fn req(x: f64, y: f64, w: f64, h: f64) -> PillRequest {
        PillRequest {
            at: (x, y),
            size: (w, h),
        }
    }

    #[test]
    fn pills_are_placed_without_overlap_inside_the_viewport_deterministically() {
        // phase: pills_of_adjacent_peers_do_not_overlap
        // Two people working next to each other put their pointers a few
        // pixels apart. Their names must still be two names.
        {
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
        // phase: four_piled_peers_get_four_readable_pills
        // Four pills from four nearly-identical pointers must still separate.
        {
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
        // phase: a_pill_never_leaves_the_viewport
        // A peer in the bottom-right corner still gets a readable name: the
        // pill flips to the other side of them rather than running off the
        // window.
        {
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
        // phase: only_a_displaced_pill_asks_for_a_tether
        // A pill in its own corner needs no tether; a tether on every pill
        // would be noise.
        {
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
        // phase: placement_does_not_depend_on_arrival_order
        // A peer reconnecting must not reshuffle everyone else's names.
        {
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
    }

    #[test]
    #[allow(
        clippy::many_single_char_names,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "conventional short names for a color/angle sample; x/y are always in-bounds by construction"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn markers_keep_the_peer_hue_equal_strength_and_legible_text() {
        // phase: a_peer_pill_is_the_peer_colour_and_its_label_contrasts
        {
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
                    is_agent: false,
                    opacity: 1.0,
                    name_opacity: 1.0,
                    chat: None,
                    reaction: None,
                }],
            );
            let mut r = OffscreenRenderer::new(200, 200);
            let rgba = r.render(&scene, Color::WHITE);

            // The arrow itself, just under its tip.
            let (dr, dg, db, _) = OffscreenRenderer::sample(&rgba, 200, 63, 68);
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
        // phase: a_peer_dot_reads_on_a_white_page_and_on_a_dark_desktop
        {
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
                        is_agent: false,
                        opacity: 1.0,
                        name_opacity: 1.0,
                        chat: None,
                        reaction: None,
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
        // phase: a_peer_name_is_the_same_pill_everywhere
        // Asserted through `peer_pill_size`, shared by the pointer pill and
        // the caret chip, so this fails if either drifts.
        {
            let theme = Theme::default();
            let mut tr = crate::text_render::TextRenderer::new();

            // Control step, not caption — names are on screen for a whole
            // session.
            assert_eq!(
                pill_label_style(&theme),
                theme.text(TextRole::Caption),
                "a peer's name is a caption in the theme's face",
            );

            let (w, h) = peer_pill_size(&mut tr, &theme, "quinten");
            assert_eq!(
                (w, h),
                peer_pill_size(&mut tr, &theme, "quinten"),
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
            let (w2, h2) = peer_pill_size(&mut tr, &theme, "xaver-with-a-long-name");
            assert!(w2 > w, "a longer name should get a wider pill");
            assert_eq!(h2, h, "pill height should not depend on the label");
        }
        // phase: every_peers_marker_is_about_as_strong_as_every_others
        // Every peer's marker lands at comparable strength against the page.
        // See [`marker_pigment`].
        {
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
        // phase: reduce_motion_holds_the_marker_instead_of_dissolving_it
        // Reduce-motion holds the marker at full strength instead of fading it
        // — it still clears when the span expires; only the animation goes.
        {
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
        // phase: marker_pigment_keeps_the_peers_hue
        // The marker re-tunes lightness only; a peer's hue (what their caret is
        // also coloured by) must survive it.
        {
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn pills_and_chips_scale_with_their_label_and_stay_inside_their_bounds() {
        // phase: pill_and_chip_heights_follow_the_measured_label_at_every_text_scale
        // The pill and chip are sized from the measured line box, so they hold
        // their label at every renderer text scale — the label used to spill
        // out of a font-size-derived box at 2×.
        {
            let theme = Theme::dark();
            for scale in [1.0_f32, 2.0] {
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let (_, pill_h) = peer_pill_size_for(&mut tr, &theme, "Alice", false);
                let (_, label_h) = tr.measure_style("Alice", None, &pill_label_style(&theme));
                assert!(
                    (pill_h - (f64::from(label_h) + PILL_PAD_Y * 2.0)).abs() < 1e-6,
                    "at {scale}x the pill is {pill_h} tall around a {label_h} label"
                );
                let (_, chip_h) = chip_size(&theme, &mut tr, "Alice");
                let (_, chip_label_h) =
                    tr.measure_style("Alice", None, &theme.text(TextRole::Caption));
                assert!(
                    (chip_h - (f64::from(chip_label_h) + CHIP_PAD_Y * 2.0)).abs() < 1e-6,
                    "at {scale}x the chip is {chip_h} tall around a {chip_label_h} label"
                );
            }
        }
        // phase: the_default_pill_is_one_caption_line_plus_its_pads
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let (_, pill_h) = peer_pill_size_for(&mut tr, &theme, "Alice", false);
            let line = f64::from(theme.text(TextRole::Caption).line_height_px());
            assert!(
                (pill_h - (line + PILL_PAD_Y * 2.0)).abs() < 1e-3,
                "pill height {pill_h}; one caption line is {line}"
            );
        }
        // phase: pill_and_chip_side_insets_scale_with_the_label
        // The side insets frame the label at every magnification: a 2× label
        // gets 2× pads (pill and chip alike), and the agent glyph's room grows
        // with it, while the 1× pill keeps its exact default width.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let (tw, _) = tr.measure_style("Alice", None, &pill_label_style(&theme));
            let (pw, _) = peer_pill_size_for(&mut tr, &theme, "Alice", false);
            assert!(
                (pw - (f64::from(tw) + PILL_PAD_X * 2.0)).abs() < 1e-6,
                "1x pill width {pw} is not label {tw} + two {PILL_PAD_X}px pads"
            );
            let (aw, _) = peer_pill_size_for(&mut tr, &theme, "Alice", true);
            assert!(
                (aw - pw - AGENT_GLYPH_W).abs() < 1e-6,
                "1x agent pill adds exactly the glyph room"
            );
            let (ctw, _) = tr.measure_style("Alice", None, &theme.text(TextRole::Caption));
            let (cw, _) = chip_size(&theme, &mut tr, "Alice");
            assert!((cw - (f64::from(ctw) + CHIP_PAD_X * 2.0)).abs() < 1e-6);

            // A matched 2x host: the theme's type steps carry the scale and the
            // renderer is set to the same scale, so the label is measured at the
            // renderer-relative size the painter uses (X1) and paints at 2x.
            let theme = Theme::dark().with_text_scale(2.0);
            tr.set_text_scale(2.0);
            let (tw2, _) = tr.measure_style("Alice", None, &pill_label_style(&theme));
            assert!(
                tw2 > tw * 1.5,
                "the 2x label ({tw2}) must be about twice the 1x label ({tw})"
            );
            let (pw2, _) = peer_pill_size_for(&mut tr, &theme, "Alice", false);
            assert!(
                (pw2 - (f64::from(tw2) + PILL_PAD_X * 4.0)).abs() < 1e-6,
                "2x pill width {pw2} should frame the {tw2} label with doubled pads"
            );
            let (aw2, _) = peer_pill_size_for(&mut tr, &theme, "Alice", true);
            assert!((aw2 - pw2 - AGENT_GLYPH_W * 2.0).abs() < 1e-6);
            let (ctw2, _) = tr.measure_style("Alice", None, &theme.text(TextRole::Caption));
            let (cw2, _) = chip_size(&theme, &mut tr, "Alice");
            assert!((cw2 - (f64::from(ctw2) + CHIP_PAD_X * 4.0)).abs() < 1e-6);
        }
        // phase: edge_tag_stays_inside_its_band_at_every_text_scale
        // The edge tag paints inside the band that is also its hit rect: a
        // long name, or any name at 2×, is elided rather than hung out over
        // content the pointer cannot reach it from. A short name at 1× is
        // untouched.
        {
            use super::super::geom::{EdgeMist, MistEdge};
            let theme = Theme::dark();
            let viewport = Rect::new(0.0, 0.0, 400.0, 300.0);
            for (scale, name) in [
                (1.0_f32, "Bob"),
                (1.0, "Charlotte Okonkwo"),
                (2.0, "Alice"),
                (3.0, "Alice"),
            ] {
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                for edge in [
                    MistEdge::Left,
                    MistEdge::Right,
                    MistEdge::Top,
                    MistEdge::Bottom,
                ] {
                    let mist = EdgeMist {
                        edge,
                        along: 0.5,
                        distance: 80.0,
                    };
                    let band = mist_band_rect(viewport, &mist);
                    let Some((tag, label, _)) = mist_tag(&mut tr, &theme, band, edge, name) else {
                        // Only a line box deeper than the band may withhold the
                        // tag; up to 2× every band shows one.
                        assert!(scale > 2.0, "{name:?} at {scale}x on {edge:?} shows no tag");
                        continue;
                    };
                    assert!(
                        tag.x0 >= band.x0 - 1e-6
                            && tag.x1 <= band.x1 + 1e-6
                            && tag.y0 >= band.y0 - 1e-6
                            && tag.y1 <= band.y1 + 1e-6,
                        "{name:?} at {scale}x on {edge:?}: tag {tag:?} leaves band {band:?}"
                    );
                    assert!(
                        !label.is_empty(),
                        "{name:?} at {scale}x on {edge:?} lost its label"
                    );
                    if name == "Bob" {
                        assert_eq!(label, "Bob", "a short 1x name must not be elided");
                    }
                }
            }
        }
        // phase: pill_insets_follow_ui_scaled_type_too
        // `with_ui_scale` doubles the type without touching `Theme::text_scale`;
        // the insets must follow the type, whichever knob grew it.
        {
            let theme = Theme::dark().with_ui_scale(2.0);
            let mut tr = TextRenderer::new();
            let (tw, _) = tr.measure_style("Alice", None, &pill_label_style(&theme));
            let (pw, _) = peer_pill_size_for(&mut tr, &theme, "Alice", false);
            assert!(
                (pw - (f64::from(tw) + PILL_PAD_X * 4.0)).abs() < 1e-6,
                "ui-scaled pill width {pw} should frame the {tw} label with doubled pads"
            );
        }
        // phase: a_name_chip_never_leaves_the_clipped_surface_sideways
        // Sideways too: an object near the right edge of a clipped surface
        // keeps its name chip inside the surface, elided if need be, instead of
        // showing the first two letters of a name that ran off the edge.
        {
            let ctx = PresenceCtx::empty();
            let theme = Theme::default();
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let user = UserId("charlie".to_string());
            // The surface ends at x=200; the canvas continues to 260 so paint
            // past the edge is observable.
            let bounds = Rect::new(0.0, 0.0, 200.0, 160.0);
            let object = Rect::new(150.0, 60.0, 196.0, 140.0);

            let mut scene = Scene::new();
            ctx.draw_peer_outline_labeled_within(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                &user,
                "Charlotte Okonkwo",
                object,
                4.0,
                bounds,
            );
            let mut r = OffscreenRenderer::new(260, 160);
            let rgba = r.render(&scene, Color::BLACK);
            let mut chip_seen = false;
            for y in 0..60 {
                for x in 0..260 {
                    let (cr, cg, cb, ca) = OffscreenRenderer::sample(&rgba, 260, x, y);
                    if ca == 0 || (cr == 0 && cg == 0 && cb == 0) {
                        continue;
                    }
                    assert!(
                        x < 200,
                        "chip paint at ({x},{y}) is past the surface edge at x=200"
                    );
                    chip_seen = true;
                }
            }
            assert!(
                chip_seen,
                "the chip was not painted above the object at all"
            );
        }
        // phase: pill_label_stays_inside_its_pill_at_two_x
        // At 2× the label ink of a pointer pill stays inside the pill it was
        // laid out for, top and bottom.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let (pw, ph) = peer_pill_size_for(&mut tr, &theme, "Alice", false);
            let (w, h) = (160_u32, 96_u32);
            let rect = Rect::new(20.0, 30.0, 20.0 + pw, 30.0 + ph);
            let mut scene = Scene::new();
            // A dark peer colour so the light label ink is unambiguous.
            let peer = Color::from_rgb8(0x1e, 0x29, 0x3b);
            draw_peer_pill_kind(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                rect,
                "Alice",
                peer,
                false,
            );
            let rgba = OffscreenRenderer::new(w, h).render(&scene, Color::BLACK);
            let mut ink_rows = Vec::new();
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let i = (y * w as usize + x) * 4;
                    // Label ink is white-ish; the pill and background are dark.
                    if rgba[i] > 160 && rgba[i + 1] > 160 && rgba[i + 2] > 160 {
                        ink_rows.push(y);
                        break;
                    }
                }
            }
            let (top, bottom) = (
                *ink_rows.first().expect("label painted some ink"),
                *ink_rows.last().expect("label painted some ink"),
            );
            assert!(
                f64::from(top as u32) >= rect.y0.floor()
                    && f64::from(bottom as u32) < rect.y1.ceil(),
                "label ink rows {top}..={bottom} leave the pill {rect:?} at 2x"
            );
        }
    }

    /// A still pointer's name steps aside while the arrow stays; someone
    /// talking gets a bubble in their colour, with their message, beside
    /// the pointer instead.
    #[test]
    fn a_still_pointer_drops_its_name_and_a_talking_one_shows_its_message() {
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let ada = Color::from_rgb8(0x25, 0x63, 0xeb);
        let mut render = |peer: PeerCursor| {
            let mut scene = Scene::new();
            let painted = draw_peer_cursors(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 400.0, 200.0),
                &mut tr,
                &theme,
                &[peer],
            );
            (
                OffscreenRenderer::new(400, 200).render(&scene, Color::WHITE),
                painted,
            )
        };
        let blue = |rgba: &[u8], x: u32, y: u32| {
            let (r, g, b, _) = OffscreenRenderer::sample(rgba, 400, x, y);
            b > 180 && r < 120 && g < 150
        };
        let mut moving = PeerCursor::new((60.0, 60.0), "Ada", ada);
        let (shown, _) = render(moving.clone());
        assert!(blue(&shown, 63, 68), "the arrow is in her colour");
        assert!(
            blue(&shown, 76, 76),
            "her name is beside it while she moves"
        );

        moving.name_opacity = 0.0;
        let (still, painted) = render(moving.clone());
        assert!(blue(&still, 63, 68), "the arrow stays while she is still");
        assert!(!blue(&still, 76, 76), "her name steps aside");
        assert!(
            painted.iter().all(|rect| rect.x1 < 90.0 && rect.y1 < 90.0),
            "a still pointer damages only the arrow: {painted:?}"
        );

        moving.chat = Some(super::super::ChatBubble {
            text: "over here, the second paragraph".into(),
            typing: false,
            opacity: 1.0,
        });
        let (talking, painted) = render(moving);
        assert!(
            blue(&talking, 76, 76),
            "a bubble in her colour, whatever the name did"
        );
        let bubble = painted.iter().copied().reduce(|a, b| a.union(b)).unwrap();
        assert!(bubble.width() > 150.0, "the message is in it: {bubble:?}");
    }
}
