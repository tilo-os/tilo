use vello::{Scene, kurbo::{Rect, Affine, Stroke}, peniko::{Color, Fill}};
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

    /// Draw a small 8-px circle for the peer's pointer.
    pub fn draw_peer_pointer(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        pos: (f32, f32),
    ) {
        let color = color_for(self, user, theme);
        let x = pos.0 as f64;
        let y = pos.1 as f64;
        let rect = Rect::new(x - 4.0, y - 4.0, x + 4.0, y + 4.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
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
