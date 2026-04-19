use vello::{Scene, kurbo::{Rect, Affine, Stroke}, peniko::{Color, Fill}};
use crate::theme::Theme;
use crate::permissions::UserId;
use super::{PresenceCtx, SelectionRange, user_color};

/// Deterministic per-user color — maps a UserId string to a hue with stable lightness.
///
/// Delegates to [`user_color`] so every collab affordance (tab presence
/// dot, remote cursor, peer caret, selection highlight, focus ring) picks
/// the **same** colour for a given peer. The concept pins four canonical
/// ids — `me` / `alice` / `bob` / `charlie` — to fixed RGBs so the Rust
/// render matches the mockup pixel-for-pixel for those names; unknown ids
/// fall through to a stable FNV-1a → HSL hash. `theme` is retained on the
/// signature for the all-zero fallback, which only fires if the hash ever
/// lands exactly on black — vanishingly unlikely in practice.
pub(crate) fn color_for(user: &UserId, theme: &Theme) -> Color {
    let (r, g, b) = user_color(&user.0);
    if r == 0 && g == 0 && b == 0 {
        return theme.accent_primary;
    }
    Color::rgb8(r, g, b)
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
        let color = color_for(user, theme);
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
        let mut color = color_for(user, theme);
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
        let color = color_for(user, theme);
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
        let color = color_for(user, theme);
        // Inflate inward by 1px so the 2px stroke sits inside `rect`.
        let outline = rect.inflate(-1.0, -1.0);
        scene.stroke(&Stroke::new(2.0), Affine::IDENTITY, color, None, &outline);
    }
}
