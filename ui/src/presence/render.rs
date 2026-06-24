use vello::{Scene, kurbo::{Circle, Point, Rect, Affine, Stroke}, peniko::{Color, Fill}};
use crate::color_compat::ColorCompat;
use crate::theme::Theme;
use crate::permissions::UserId;
use super::{PresenceCtx, SelectionRange};

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
    /// Draw a 2-px vertical caret at `pos`, 16px tall, in the user's assigned color.
    pub fn draw_peer_caret(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        pos: (f32, f32),
    ) {
        let color = color_for(self, user, theme);
        let x = pos.0 as f64;
        let y = pos.1 as f64;
        let rect = Rect::new(x, y, x + 2.0, y + 16.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
    }

    /// Draw a translucent selection highlight over `line_rects` lines.
    pub fn draw_peer_selection(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        _range: &SelectionRange,
        line_rects: &[Rect],
    ) {
        let mut color = color_for(self, user, theme);
        // 35% alpha.
        color = color.with_alpha_factor(0.35);
        for r in line_rects {
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, r);
        }
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

    /// Draw a 2-px focus outline around `rect` in `user`'s assigned color.
    /// Used by widgets that want to signal "this peer is present on me"
    /// without owning caret/selection geometry of their own.
    pub fn draw_peer_focus(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        rect: Rect,
    ) {
        let color = color_for(self, user, theme);
        // Inflate inward by 1px so the 2px stroke sits inside `rect`.
        let outline = rect.inflate(-1.0, -1.0);
        scene.stroke(&Stroke::new(2.0), Affine::IDENTITY, color, None, &outline);
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
        ctx.draw_peer_caret(&mut scene, &theme, &user, (20.0, 10.0));

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
}
