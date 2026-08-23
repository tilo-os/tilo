use vello::{Scene, kurbo::{Circle, Point, Rect, Affine, RoundedRect, Stroke}, peniko::{Color, Fill}};
use crate::color_compat::ColorCompat;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::permissions::UserId;
use super::PresenceCtx;

/// Deterministic per-user color — maps a UserId string to a hue with stable lightness.
///
/// Consults the [`PresenceCtx::color_for`] override map first (seeded by
/// the shell from [`Collaborator::cursor_color`]) and only falls through
/// to the palette-pinned [`super::user_color`] when no override exists.
/// This is how every collab affordance — tab presence dots, remote
/// cursors, peer caret, selection highlight, focus ring — picks the
/// **same** colour for a given peer. `theme` is retained on the signature
/// for the all-zero fallback, which only fires if a hash happens to land
/// exactly on black.
pub(crate) fn color_for(ctx: &PresenceCtx, user: &UserId, theme: &Theme) -> Color {
    let (r, g, b) = ctx.color_for(user);
    if r == 0 && g == 0 && b == 0 {
        return theme.accent_primary;
    }
    Color::from_rgb8(r, g, b)
}

impl PresenceCtx {
    /// Paint a peer's caret in their colour. `rect` is the caret geometry
    /// the widget itself produced (`TextInput::caret_rect` /
    /// `RichTextLine::caret_rect`) in its local coordinates, and
    /// `transform` maps that into scene space — so a peer caret is
    /// pixel-identical to the widget's own focused caret, never a
    /// re-derived approximation.
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

    /// Paint a peer's selection highlight in their colour at read-through
    /// alpha. `rect` comes from the widget's own `selection_rect`, mapped
    /// to scene space by `transform`.
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

    /// Draw an authorship "marker" highlight — a translucent underlay in
    /// the author's color behind the runs they wrote, like a highlighter
    /// pen. Lower alpha than a selection so the text stays readable.
    ///
    /// # This one is not fed by presence
    ///
    /// Every other method on this type draws something a peer *is doing* —
    /// where their caret is, what they have selected — and that arrives as
    /// a presence frame. Authorship is different: it is who wrote what,
    /// and it has to survive both people closing the document, so it lives
    /// **in the document** rather than on the presence channel.
    ///
    /// In the [`crate::widget::TextInput`] stack it is an Automerge mark:
    /// every insert stamps `AUTHOR_MARK` with the widget's `local_author`,
    /// and `RichTextLine::author_runs` reads the marks back on sync. Two
    /// consequences worth knowing before you debug this:
    ///
    /// - It crosses machines wherever document deltas already cross. There
    ///   is no presence payload to bridge and no wire format to add — if
    ///   the text replicates, the authorship replicates with it.
    /// - **It only exists if the app called `set_local_author`.** With no
    ///   local author, inserts stamp no mark, `author_runs` stays empty,
    ///   and this method is never reached — so the tint is missing while
    ///   the caret and selection work perfectly. That looks exactly like a
    ///   network fault and is not one. `apps/notes` and `apps/chat` set it;
    ///   an app that forgets will spend a day in the wrong subsystem.
    ///
    /// A document model that records no per-run author cannot feed this at
    /// all. The word processor is the live example: it is built on `qdoc`,
    /// whose only author field is `CHANGE_AUTHOR` on a tracked `ins`/`del`
    /// change, so ambient "she wrote this paragraph" tinting needs a new
    /// per-run mark in that schema before there is anything to draw.
    ///
    /// The fading "just typed" swipe is the separate, genuinely ephemeral
    /// half, and that one *is* presence — see
    /// [`crate::presence::PresenceCtx::record_peer_typing`] and
    /// [`Self::draw_peer_typing`].
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

    /// Draw a peer's mouse pointer as a round, color-coded cursor: a
    /// soft translucent halo, a filled dot in the peer's color, and a
    /// thin white ring for contrast against any background. `transform`
    /// is the scene transform (HiDPI scale etc.); `pos` is the pointer
    /// position in logical coordinates.
    pub fn draw_peer_pointer(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        transform: Affine,
        user: &UserId,
        pos: (f32, f32),
    ) {
        let color = color_for(self, user, theme);
        let t = transform * Affine::translate((pos.0 as f64, pos.1 as f64));
        // Soft halo so the pointer reads over busy content.
        let halo = Circle::new(Point::ORIGIN, 9.0);
        scene.fill(Fill::NonZero, t, color.with_alpha_factor(0.22), None, &halo);
        // Solid dot in the peer's color.
        let dot = Circle::new(Point::ORIGIN, 5.0);
        scene.fill(Fill::NonZero, t, color, None, &dot);
        // White ring for contrast.
        scene.stroke(&Stroke::new(1.5), t, Color::WHITE, None, &dot);
    }

    /// Draw a soft "indicative mist" over `rect` in the peer's color —
    /// a wayfinding cue painted on the navigational element that leads to
    /// where a peer currently is (e.g. a sidebar row for the page they're
    /// editing). Layered translucent fills approximate a radial glow
    /// without a gradient: a faint wash over the whole element plus a
    /// brighter core.
    pub fn draw_peer_mist(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        rect: Rect,
    ) {
        let color = color_for(self, user, theme);
        // Faint full-element wash.
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color.with_alpha_factor(0.16),
            None,
            &rect,
        );
        // Brighter core inset from the edges so it reads as a glow
        // rather than a flat highlight.
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

    /// Draw a peer's caret **with a small name chip** floating just above
    /// the caret tip — so on a shared document you can tell not just where
    /// a peer's cursor is but *who* it is, without hovering. The caret bar
    /// is `caret_rect` (the widget's own geometry, mapped by `transform`,
    /// pixel-identical to a local caret); the chip is drawn in the peer's
    /// colour with the name in a contrasting ink. Mirrors the pointer name
    /// pill so caret and pointer read as the same person.
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
        // The caret bar itself.
        scene.fill(Fill::NonZero, transform, color, None, &caret_rect);
        // The chip sits just above the caret's top, left-aligned to it.
        draw_name_chip(
            scene,
            theme,
            text_renderer,
            transform,
            color,
            name,
            caret_rect.x0,
            caret_rect.y0,
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

    /// Draw a peer's fading "just typed" highlighter over the glyph runs
    /// they inserted. `spans` pairs each scene-space rect with its fade
    /// alpha (from [`PresenceCtx::active_typing`]); the tint is the peer's
    /// colour scaled by that alpha and a low base so the text stays legible
    /// as the swipe decays. Rects are already in scene space, so this uses
    /// `Affine::IDENTITY`.
    pub fn draw_peer_typing(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        spans: &[(Rect, f32)],
    ) {
        let color = color_for(self, user, theme);
        for (rect, alpha) in spans {
            // Base 0.5 so a fresh mark is a clear-but-readable marker; it
            // tapers to nothing as `alpha` → 0.
            let a = (alpha * 0.5).clamp(0.0, 1.0);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color.with_alpha_factor(a),
                None,
                rect,
            );
        }
    }

    /// Draw a thin gutter strip on the left edge of `rect` in `user`'s
    /// assigned color — a far subtler "this peer is present on me"
    /// signal than a heavy block outline. Mirrors the gutter-rule a
    /// code editor uses to mark a hunk of unsaved changes. `rect` is
    /// already in scene space (transformed by the caller); the strip
    /// itself uses `Affine::IDENTITY` to avoid double-transforming.
    pub fn draw_peer_focus(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        rect: Rect,
    ) {
        let color = color_for(self, user, theme);
        let strip = Rect::new(rect.x0 - 4.0, rect.y0, rect.x0 - 1.0, rect.y1);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &strip);
    }
}


// =====================================================================
// Several peers at once
// =====================================================================

/// Radius of the solid dot at a peer's pointer. Larger than the single
/// -peer pointer this grew out of: with a name pill beside it, a 5px
/// dot reads as a speck next to its own label, and a person's cursor
/// should be the thing you find first.
const DOT_R: f64 = 6.5;
/// Outer edge of the dot including its contrast ring — what a name pill
/// has to clear.
const RING_R: f64 = 8.5;
/// Breathing room between the dot and its pill, and between two pills.
const PILL_GAP: f64 = 4.0;
/// How far a pill is allowed to sit from its own pointer before it stops
/// being self-evidently that pointer's and gets a tether drawn to it.
const TETHER_AFTER: f64 = 4.0;

/// One peer to paint: where their pointer is, what to call them, and the
/// colour that is theirs on every screen.
#[derive(Debug, Clone)]
pub struct PeerCursor {
    pub pos: (f64, f64),
    pub label: String,
    pub color: Color,
}

/// A pill wanting a home: the pointer it belongs to and how big it is.
#[derive(Debug, Clone, Copy)]
pub struct PillRequest {
    pub at: (f64, f64),
    pub size: (f64, f64),
}

/// Where a pill ended up, and whether it had to leave its pointer to get
/// there (in which case the caller draws a tether so it stays legible
/// *whose* it is).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PillPlacement {
    pub rect: Rect,
    pub tethered: bool,
}

/// Place one name pill per peer so that no two overlap and none leaves
/// `viewport`.
///
/// Two people working next to each other put their pointers a few pixels
/// apart, and two pills at the same offset from two nearby pointers land
/// on top of each other — which is not one name obscuring another, it is
/// both names becoming unreadable. Someone near the right or bottom edge
/// has the same problem against the window: a pill anchored below-right
/// of them is half outside it.
///
/// So each pill is offered the four corners around its own pointer in
/// turn and takes the first that is fully inside the viewport and clear
/// of every pill already placed. Only when all four are taken does it
/// slide away along the vertical, and then it is marked `tethered` so
/// the caller can draw the line back.
///
/// Placement order is by position, not by arrival, so the same peers in
/// the same places always produce the same picture — a cursor that
/// reconnects does not reshuffle everyone else's names.
pub fn layout_peer_pills(viewport: Rect, pills: &[PillRequest]) -> Vec<PillPlacement> {
    // Normalise: a caller handing us an inverted rect would otherwise
    // clamp every pill to nothing.
    let vp = Rect::new(
        viewport.x0.min(viewport.x1),
        viewport.y0.min(viewport.y1),
        viewport.x0.max(viewport.x1),
        viewport.y0.max(viewport.y1),
    );

    let mut order: Vec<usize> = (0..pills.len()).collect();
    order.sort_by(|&a, &b| {
        let (pa, pb) = (pills[a].at, pills[b].at);
        pa.1.total_cmp(&pb.1)
            .then(pa.0.total_cmp(&pb.0))
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

    let inside = |r: &Rect, vp: &Rect| {
        r.x0 >= vp.x0 && r.y0 >= vp.y0 && r.x1 <= vp.x1 && r.y1 <= vp.y1
    };
    let clear = |r: &Rect, placed: &[Rect]| {
        placed
            .iter()
            .all(|p| p.inflate(PILL_GAP / 2.0, PILL_GAP / 2.0).intersect(*r).is_zero_area())
    };

    for i in order {
        let PillRequest {
            at: (px, py),
            size: (w, h),
        } = pills[i];
        let right = px + RING_R + PILL_GAP;
        let left = px - RING_R - PILL_GAP - w;
        let below = py + RING_R + PILL_GAP;
        let above = py - RING_R - PILL_GAP - h;
        let corners = [
            (right, below),
            (right, above),
            (left, below),
            (left, above),
        ];

        let mut chosen = None;
        for (n, (x, y)) in corners.into_iter().enumerate() {
            let r = Rect::new(x, y, x + w, y + h);
            if inside(&r, &vp) && clear(&r, &placed) {
                // Anything but the first corner means this pill moved to
                // get out of someone's way or off an edge, and in a
                // cluster of pointers a pill that is not below-right of
                // its own is no longer obviously its own. Draw the line.
                chosen = Some((r, n > 0));
                break;
            }
        }

        let (rect, tethered) = match chosen {
            Some((r, moved)) => (r, moved),
            None => {
                // Every corner is taken or off-window. Keep the pill on
                // the side of the pointer that fits and walk it down the
                // vertical until it is clear, then back up if the bottom
                // runs out.
                let x = if right + w <= vp.x1 { right } else { left.max(vp.x0) };
                let x = x.clamp(vp.x0, (vp.x1 - w).max(vp.x0));
                let mut r = Rect::new(x, below, x + w, below + h);
                let step = h + PILL_GAP;
                let mut guard = 0;
                while (!inside(&r, &vp) || !clear(&r, &placed)) && guard < 64 {
                    let next_y = if r.y1 + step <= vp.y1 {
                        r.y0 + step
                    } else {
                        // Out of room going down — restart above the
                        // pointer and walk up instead.
                        let up = if r.y0 > py { above } else { r.y0 - step };
                        if up < vp.y0 {
                            break;
                        }
                        up
                    };
                    r = Rect::new(x, next_y, x + w, next_y + h);
                    guard += 1;
                }
                let r = Rect::new(
                    r.x0,
                    r.y0.clamp(vp.y0, (vp.y1 - h).max(vp.y0)),
                    r.x0 + w,
                    r.y0.clamp(vp.y0, (vp.y1 - h).max(vp.y0)) + h,
                );
                (r, true)
            }
        };

        // A pill that ended up further from its pointer than a corner
        // placement would have needs the line drawn, however it got there.
        let dx = (rect.x0 - px).abs().min((rect.x1 - px).abs());
        let dy = (rect.y0 - py).abs().min((rect.y1 - py).abs());
        let far = dx.max(dy) > RING_R + PILL_GAP + TETHER_AFTER;
        out[i] = PillPlacement {
            rect,
            tethered: tethered || far,
        };
        placed.push(rect);
    }
    out
}

/// Paint a whole room's worth of pointers in one pass — every dot, then
/// every tether, then every pill.
///
/// The three layers are separate on purpose: pills carry the names, so
/// nothing is allowed on top of one. Drawing peer-by-peer would let a
/// later peer's halo wash over an earlier peer's name.
///
/// `viewport` bounds the pills. Pass the rect the peers are being drawn
/// into; positions themselves are assumed already filtered to it.
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
    let tokens = &theme.tokens;
    let label_size = tokens.typography.scale.sm;
    let pad_x = 7.0_f64;
    let pad_y = 3.0_f64;

    let requests: Vec<PillRequest> = peers
        .iter()
        .map(|p| {
            let (tw, _th) = text_renderer.measure(&p.label, label_size, None);
            PillRequest {
                at: p.pos,
                size: (
                    tw as f64 + pad_x * 2.0,
                    label_size as f64 + pad_y * 2.0,
                ),
            }
        })
        .collect();
    let placements = layout_peer_pills(viewport, &requests);

    // Layer 1 — the pointers themselves.
    for p in peers {
        draw_peer_dot(scene, transform, p.pos, p.color);
    }

    // Layer 2 — tethers, under the pills so a line never crosses a name.
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

    // Layer 3 — the names, on top of everything.
    for (p, place) in peers.iter().zip(&placements) {
        draw_peer_pill(
            scene,
            transform,
            text_renderer,
            theme,
            place.rect,
            &p.label,
            p.color,
            (pad_x, pad_y),
            label_size,
        );
    }
}

/// A peer's pointer: soft halo, a shadow edge, the solid dot in their
/// colour, and a white ring.
///
/// The shadow and the ring are what make one dot work over both the word
/// processor's white page and the desktop behind it — the white ring
/// separates a dark colour from a dark background, the shadow separates a
/// light colour from a light one, and whichever is doing nothing is
/// invisible rather than in the way.
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

/// A peer's name pill: their colour as the fill, a soft drop shadow, and
/// the label in whichever of black or white reads on that colour.
///
/// Filling in the peer's colour rather than in a surface token is what
/// makes several peers legible at once — the pill *is* the swatch, so a
/// name and a cursor are matched by colour and not by proximity.
#[allow(clippy::too_many_arguments)]
fn draw_peer_pill(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    peer: Color,
    pad: (f64, f64),
    label_size: f32,
) {
    let radius = theme.tokens.radii.sm as f64;
    let shadow = RoundedRect::from_rect(rect.inflate(0.5, 0.5) + vello::kurbo::Vec2::new(0.0, 1.0), radius);
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
        transform * Affine::translate((rect.x0 + pad.0, rect.y0 + pad.1)),
        None,
    );
}

/// Closest point on `rect`'s boundary box to `p` — where a tether meets
/// the pill it points at.
fn nearest_point_on(rect: Rect, p: Point) -> Point {
    Point::new(p.x.clamp(rect.x0, rect.x1), p.y.clamp(rect.y0, rect.y1))
}

/// Paint every entry in `presence.remote_cursors` as a dot + name pill,
/// at scene-space coordinates transformed by `transform`. Used by the
/// App's per-frame draw so non-shell apps (terminal, settings, file
/// explorer) get cross-process ghost cursors without each having to
/// re-implement the geometry.
///
/// No viewport is supplied here, so pills are only de-overlapped, not
/// clamped. [`draw_remote_presence`] is the one to prefer: it knows the
/// window and can keep a name on screen.
pub fn draw_remote_cursors(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    let peers: Vec<PeerCursor> = presence.remote_cursors.iter().map(peer_of).collect();
    if peers.is_empty() {
        return;
    }
    let unbounded = Rect::new(f64::MIN / 4.0, f64::MIN / 4.0, f64::MAX / 4.0, f64::MAX / 4.0);
    draw_peer_cursors(scene, transform, unbounded, text_renderer, theme, &peers);
}

/// A [`super::RemoteCursor`] as the renderer wants it.
fn peer_of(cursor: &super::RemoteCursor) -> PeerCursor {
    PeerCursor {
        pos: (cursor.position[0] as f64, cursor.position[1] as f64),
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
    let tokens = &theme.tokens;
    let label_size = tokens.typography.scale.xs;
    let pad_x = 5.0_f64;
    let pad_y = 1.5_f64;
    let (tw, _th) = text_renderer.measure(name, label_size, None);
    let chip_w = tw as f64 + pad_x * 2.0;
    let chip_h = label_size as f64 + pad_y * 2.0;
    // A 2px gap above the thing being named.
    let cy = anchor_top - chip_h - 2.0;
    let chip_rect = Rect::new(anchor_x, cy, anchor_x + chip_w, cy + chip_h);
    let chip = RoundedRect::from_rect(chip_rect, tokens.radii.sm as f64);
    // Solid fill in the peer's colour so the chip reads as "them".
    scene.fill(Fill::NonZero, transform, color, None, &chip);
    // Label ink: white or near-black for contrast against the colour.
    text_renderer.draw_text(
        scene,
        name,
        label_size,
        contrast_ink(color),
        transform * Affine::translate((chip_rect.x0 + pad_x, chip_rect.y0 + pad_y)),
        None,
    );
}

fn contrast_ink(bg: Color) -> Color {
    let c = bg.to_rgba8();
    // Rec. 601 luma on the 0..255 channels.
    let luma = 0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32;
    if luma > 140.0 { Color::BLACK } else { Color::WHITE }
}

/// The clickable / paintable rectangle of an off-screen peer's mist band
/// for `viewport`. A localised strip hugging the resolved edge, centred on
/// where the peer projects (`mist.along`), clamped inside the viewport.
/// Shared by [`draw_remote_presence`] (paints it) and
/// [`super::PresenceCtx::hit_test_offscreen_mist`] (hit-tests it) so the
/// two never drift.
pub(super) fn mist_band_rect(
    viewport: vello::kurbo::Rect,
    mist: &super::geom::EdgeMist,
) -> Rect {
    use super::geom::MistEdge;
    let x0 = viewport.x0.min(viewport.x1);
    let x1 = viewport.x0.max(viewport.x1);
    let y0 = viewport.y0.min(viewport.y1);
    let y1 = viewport.y0.max(viewport.y1);
    let w = x1 - x0;
    let h = y1 - y0;

    // How deep the band reaches into the screen, and how long it runs
    // along the edge.
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

/// Paint one off-screen peer's wayfinding mist: a soft colour band at the
/// resolved edge that fades away from the border (approximating a gradient
/// with a few inset layers), plus a tiny name tag so you know who drifted
/// off which way. `peer` is their colour; `name` the chip label.
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

    // Intensity tapers with how far off-screen the peer is: someone just
    // past the edge glows brighter than someone who scrolled far away.
    let far = (mist.distance / 600.0).clamp(0.0, 1.0) as f32;
    let base = 0.34 * (1.0 - 0.6 * far);

    // Layered falloff: three bands from the edge inward, each dimmer and
    // shorter, so the mist reads as a soft glow rather than a flat block.
    let layers = 3;
    for i in 0..layers {
        let f = i as f64 / layers as f64;
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

    // A small name tag riding the band, so the mist says *who*.
    let tokens = &theme.tokens;
    let label_size = tokens.typography.scale.xs;
    let (tw, _th) = text_renderer.measure(name, label_size, None);
    let pad = 4.0_f64;
    let tag_w = tw as f64 + pad * 2.0;
    let tag_h = label_size as f64 + pad;
    // Anchor the tag near the band centre, nudged inward from the edge.
    let (tx, ty) = match mist.edge {
        MistEdge::Left => (band.x0 + 4.0, band.center().y - tag_h / 2.0),
        MistEdge::Right => (band.x1 - tag_w - 4.0, band.center().y - tag_h / 2.0),
        MistEdge::Top => (band.center().x - tag_w / 2.0, band.y0 + 4.0),
        MistEdge::Bottom => (band.center().x - tag_w / 2.0, band.y1 - tag_h - 4.0),
    };
    let tag_rect = Rect::new(tx, ty, tx + tag_w, ty + tag_h);
    let tag = RoundedRect::from_rect(tag_rect, tokens.radii.sm as f64);
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

/// Viewport-aware peer overlay: the one call the App makes each frame to
/// render *all* cross-process peer presence. Peers whose cursor is inside
/// `viewport` get the round dot + name pill (identical to
/// [`draw_remote_cursors`]); peers whose cursor has drifted off-screen get
/// a colored edge mist pointing the way, so nobody vanishes. `viewport` is
/// the visible window rect in the same (logical) space as the cursor
/// positions; `transform` is the scene transform (HiDPI etc.).
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
    // so their names cannot land on top of each other, which is only
    // possible once the whole set is known.
    let mut on_screen: Vec<PeerCursor> = Vec::new();
    for cursor in &presence.remote_cursors {
        let target = Point::new(cursor.position[0] as f64, cursor.position[1] as f64);
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

    /// The round remote pointer paints the peer's color at its center.
    /// "alice" maps to the pinned pink (0xec, 0x48, 0x99), so the dot
    /// center should read red-high / green-low / blue-mid.
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

    /// The remote caret paints the peer's color as a thin vertical bar.
    /// "charlie" maps to emerald (0x10, 0xb9, 0x81): green dominant.
    #[test]
    fn peer_caret_paints_peer_color() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut scene = Scene::new();
        let user = UserId("charlie".to_string());
        // The widget supplies the caret rect; the helper just recolours it.
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

    /// Indicative mist tints a navigational element in the peer's color.
    /// "bob" maps to orange (0xf9, 0x73, 0x16): red-high, green-mid,
    /// blue-low. The mist is translucent, so over a dark background the
    /// channel ordering (R > G > B) still holds.
    #[test]
    fn peer_mist_tints_element_in_peer_color() {
        let ctx = PresenceCtx::empty();
        let theme = Theme::default();
        let mut scene = Scene::new();
        let user = UserId("bob".to_string());
        ctx.draw_peer_mist(
            &mut scene,
            &theme,
            &user,
            Rect::new(10.0, 10.0, 70.0, 30.0),
        );

        let mut r = OffscreenRenderer::new(80, 80);
        let rgba = r.render(&scene, Color::BLACK);
        let (red, green, blue, _a) = OffscreenRenderer::sample(&rgba, 80, 40, 20);
        assert!(
            red > green && green >= blue && red > 20,
            "expected bob's orange mist (R>G>=B), got ({red},{green},{blue})"
        );
    }

    /// A labeled caret paints the peer's color as the caret bar AND draws
    /// a name chip in the peer's color just above the caret tip. We sample
    /// both: the bar (mid-height of the caret) and the chip (above it).
    /// "charlie" is emerald (G dominant).
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

        // The chip fill, a few pixels above the caret top (y ~ 33), at the
        // caret's left x. It is drawn solid in the peer color, so an
        // emerald reading here proves the label chip positions above the
        // caret.
        let (cr, cg, cb, ca) = OffscreenRenderer::sample(&rgba, 120, 31, 33);
        assert!(ca > 0, "expected an opaque chip above the caret, got alpha {ca}");
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
        assert!(ca > 0, "expected an opaque chip above the object, got alpha {ca}");
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
        assert!(red2 < red, "lower alpha must paint a dimmer tint: {red2} !< {red}");
    }

    /// An off-screen peer produces an edge-mist band on the correct edge.
    /// A peer whose cursor is off to the RIGHT tints the right edge in
    /// their color and leaves the left edge clear.
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

    // -- several peers at once -----------------------------------------

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

    /// Four peers in a pile is the case that broke: four pills all at the
    /// same offset from four nearly-identical pointers.
    #[test]
    fn four_piled_peers_get_four_readable_pills() {
        let vp = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let pills: Vec<PillRequest> = (0..4)
            .map(|i| req(600.0 + i as f64 * 3.0, 400.0 + i as f64 * 2.0, 70.0, 22.0))
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

    /// A pill that had to leave its pointer says so, so the caller draws
    /// the line back to it. One that sits in its own corner does not — a
    /// tether on every pill would be noise.
    #[test]
    fn only_a_displaced_pill_asks_for_a_tether() {
        let vp = Rect::new(0.0, 0.0, 800.0, 600.0);
        let alone = layout_peer_pills(vp, &[req(400.0, 300.0, 60.0, 20.0)]);
        assert!(!alone[0].tethered, "a peer on its own needs no tether");

        // Six peers stacked within a few pixels: the corners run out and
        // the later ones must be walked away and tethered.
        let pile: Vec<PillRequest> = (0..6)
            .map(|i| req(400.0 + i as f64, 300.0 + i as f64, 60.0, 20.0))
            .collect();
        let out = layout_peer_pills(vp, &pile);
        assert!(
            out.iter().any(|p| p.tethered),
            "a six-deep pile placed every pill in a corner, which cannot be",
        );
    }

    /// The same peers in the same places must produce the same picture
    /// however the list is ordered — a peer reconnecting must not
    /// reshuffle everyone else's names.
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

    /// A peer's name is painted in their own colour, and the label on it
    /// is whichever of black or white can be read on that colour. Both
    /// samples are taken off a real render.
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

    /// A peer over a white page has to be as visible as one over the dark
    /// desktop. The dot carries both a white ring and a dark edge so that
    /// whichever background it lands on, one of the two separates it.
    #[test]
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
        for (name, bg) in [("a white page", Color::WHITE), ("the desktop", Color::BLACK)] {
            let rgba = render(bg);
            let bgc = bg.to_rgba8();
            // Somewhere on the ring around the dot must differ from the
            // background, or the cursor has no edge at all on it.
            let edge = (0..360).step_by(15).any(|deg| {
                let a = (deg as f64).to_radians();
                let x = (60.0 + 6.8 * a.cos()).round() as u32;
                let y = (60.0 + 6.8 * a.sin()).round() as u32;
                let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 120, x, y);
                let d = super::super::color_distance((r, g, b), (bgc.r, bgc.g, bgc.b));
                d > 60.0
            });
            assert!(edge, "a pale peer has no visible edge against {name}");
        }
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
}
