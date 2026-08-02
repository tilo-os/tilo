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

        // Chip anchored to the caret's top, mapped into scene space so it
        // rides with the caret under any transform.
        let tokens = &theme.tokens;
        let label_size = tokens.typography.scale.xs;
        let pad_x = 5.0_f64;
        let pad_y = 1.5_f64;
        let (tw, _th) = text_renderer.measure(name, label_size, None);
        let chip_w = tw as f64 + pad_x * 2.0;
        let chip_h = label_size as f64 + pad_y * 2.0;
        // Top-left corner of the caret in local space; the chip sits just
        // above it (a 2px gap) and left-aligns to the caret.
        let cx = caret_rect.x0;
        let cy = caret_rect.y0 - chip_h - 2.0;
        let chip_rect = Rect::new(cx, cy, cx + chip_w, cy + chip_h);
        let chip = RoundedRect::from_rect(chip_rect, tokens.radii.sm as f64);
        // Solid fill in the peer's colour so the chip reads as "them".
        scene.fill(Fill::NonZero, transform, color, None, &chip);
        // Label ink: white or near-black for contrast against the colour.
        let ink = contrast_ink(color);
        text_renderer.draw_text(
            scene,
            name,
            label_size,
            ink,
            transform * Affine::translate((chip_rect.x0 + pad_x, chip_rect.y0 + pad_y)),
            None,
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


/// Paint every entry in `presence.remote_cursors` as a triangle + name
/// pill, at scene-space coordinates transformed by `transform`. Used by
/// the App's per-frame draw so non-shell apps (terminal, settings, file
/// explorer) get cross-process ghost cursors without each having to
/// re-implement the geometry. The shell has a richer overlay that
/// clips to the active workspace's content rect; this helper covers
/// the "I'm just an app inside a hosted-mode window and want my peers
/// to be visible" case.
///
/// Mirrors the visual contract from
/// `shell::shell::overlay_host::OverlayHost::draw_real_peers` so the
/// two render paths look the same to a user comparing shell vs. app
/// windows side-by-side.
pub fn draw_remote_cursors(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    if presence.remote_cursors.is_empty() {
        return;
    }
    for cursor in &presence.remote_cursors {
        let peer = Color::from_rgba8(
            (cursor.color[0].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[1].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[2].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[3].clamp(0.0, 1.0) * 255.0) as u8,
        );
        draw_one_remote_cursor(scene, transform, text_renderer, theme, cursor, peer);
    }
}

/// Draw a single peer's round cursor dot + name pill at its scene position.
/// Extracted from [`draw_remote_cursors`] so the viewport-aware
/// [`draw_remote_presence`] paints on-screen peers identically.
fn draw_one_remote_cursor(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    cursor: &super::RemoteCursor,
    peer: Color,
) {
    let tokens = &theme.tokens;
    let caret_size = 10.0_f64;
    let chip_offset = 2.0_f64;
    let pad_x = 6.0_f64;
    let pad_y = 2.0_f64;
    let label_size = tokens.typography.scale.sm;

    let apex = Point::new(cursor.position[0] as f64, cursor.position[1] as f64);
    // Round, color-coded cursor dot at the pointer position: a soft halo,
    // a solid dot in the peer's colour, and a thin white ring for contrast.
    let t = transform * Affine::translate((apex.x, apex.y));
    scene.fill(
        Fill::NonZero,
        t,
        peer.with_alpha_factor(0.22),
        None,
        &Circle::new(Point::ORIGIN, 9.0),
    );
    let dot = Circle::new(Point::ORIGIN, 5.0);
    scene.fill(Fill::NonZero, t, peer, None, &dot);
    scene.stroke(&Stroke::new(1.5), t, Color::WHITE, None, &dot);

    // Name pill, anchored just below-right of the caret tip.
    let label = &cursor.label;
    let (tw, _th) = text_renderer.measure(label, label_size, None);
    let chip_w = tw as f64 + pad_x * 2.0;
    let chip_h = label_size as f64 + pad_y * 2.0;
    let chip_x = apex.x + caret_size + chip_offset;
    let chip_y = apex.y + caret_size + chip_offset;
    let chip_rect = Rect::new(chip_x, chip_y, chip_x + chip_w, chip_y + chip_h);
    let chip = RoundedRect::from_rect(chip_rect, tokens.radii.sm as f64);
    scene.fill(Fill::NonZero, transform, tokens.colors.surfaces.raised, None, &chip);
    scene.stroke(&Stroke::new(1.0), transform, tokens.colors.border_standard, None, &chip);
    let underline = Rect::new(chip_rect.x0, chip_rect.y1 - 2.0, chip_rect.x1, chip_rect.y1);
    scene.fill(Fill::NonZero, transform, peer, None, &underline);
    text_renderer.draw_text(
        scene,
        label,
        label_size,
        tokens.colors.text_primary,
        transform * Affine::translate((chip_rect.x0 + pad_x, chip_rect.y0 + pad_y)),
        None,
    );
}

/// Pick black or white ink for text drawn on top of `bg`, by relative
/// luminance — so a name chip label stays readable on any peer colour.
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
    for cursor in &presence.remote_cursors {
        let target = Point::new(cursor.position[0] as f64, cursor.position[1] as f64);
        let peer = Color::from_rgba8(
            (cursor.color[0].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[1].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[2].clamp(0.0, 1.0) * 255.0) as u8,
            255,
        );
        match super::geom::edge_mist_for(viewport, target) {
            Some(mist) => {
                draw_edge_mist(
                    scene,
                    text_renderer,
                    theme,
                    viewport,
                    &mist,
                    peer,
                    &cursor.label,
                );
            }
            None => {
                draw_one_remote_cursor(scene, transform, text_renderer, theme, cursor, peer);
            }
        }
    }
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
