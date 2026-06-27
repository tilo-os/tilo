use vello::{Scene, kurbo::{Rect, Affine, BezPath, Point, RoundedRect, Stroke}, peniko::{Color, Fill}};
use crate::color_compat::ColorCompat;
use crate::text_render::TextRenderer;
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
    let tokens = &theme.tokens;
    let caret_size = 10.0_f64;
    let chip_offset = 2.0_f64;
    let pad_x = 6.0_f64;
    let pad_y = 2.0_f64;
    let label_size = tokens.typography.scale.sm;

    for cursor in &presence.remote_cursors {
        let apex = Point::new(cursor.position[0] as f64, cursor.position[1] as f64);
        // Reconstruct the peer's color from the cached f32 RGBA so we
        // honour `color_for`'s override-aware path (the cursor was
        // populated via `upsert_remote_cursor`, which already routes
        // through `color_for`). Clamp + cast back to u8 for the chip
        // accent stroke.
        let peer = Color::from_rgba8(
            (cursor.color[0].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[1].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[2].clamp(0.0, 1.0) * 255.0) as u8,
            (cursor.color[3].clamp(0.0, 1.0) * 255.0) as u8,
        );

        // Triangle (caret arrow): apex at the cursor position, then
        // two legs forming a right-angle pointer.
        let mut caret = BezPath::new();
        caret.move_to(apex);
        caret.line_to((apex.x + caret_size, apex.y + caret_size * 0.4));
        caret.line_to((apex.x + caret_size * 0.4, apex.y + caret_size));
        caret.close_path();
        scene.fill(Fill::NonZero, transform, peer, None, &caret);

        // Name pill, anchored just below-right of the caret tip.
        let label = &cursor.label;
        let (tw, _th) = text_renderer.measure(label, label_size, None);
        let chip_w = tw as f64 + pad_x * 2.0;
        let chip_h = label_size as f64 + pad_y * 2.0;
        let chip_x = apex.x + caret_size + chip_offset;
        let chip_y = apex.y + caret_size + chip_offset;
        let chip_rect = Rect::new(chip_x, chip_y, chip_x + chip_w, chip_y + chip_h);
        let chip = RoundedRect::from_rect(chip_rect, tokens.radii.sm as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &chip,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &chip,
        );
        let underline = Rect::new(
            chip_rect.x0,
            chip_rect.y1 - 2.0,
            chip_rect.x1,
            chip_rect.y1,
        );
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
}
