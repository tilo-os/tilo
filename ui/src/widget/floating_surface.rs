//! Shared chrome for floating menu/popover surfaces.
//!
//! Overlay primitives own different placement and interaction models, but
//! their elevation must not drift into slightly different shadows, fills, and
//! borders. Keep that visual language here.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};

use crate::color_compat::ColorCompat;
use crate::theme::Theme;

#[allow(
    dead_code,
    reason = "shared floating-surface painting is staged for widget adoption"
)]
pub(super) fn draw(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let radius = f64::from(theme.tokens.radii.md);
    for (spread, alpha) in [(2.0_f64, 0.20_f32), (5.0, 0.12), (9.0, 0.06), (14.0, 0.02)] {
        let shadow_rect = Rect::new(
            rect.x0 - spread,
            rect.y0 - spread + 6.0,
            rect.x1 + spread,
            rect.y1 + spread + 6.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            vello::peniko::Color::BLACK.with_alpha_factor(alpha),
            None,
            &RoundedRect::from_rect(shadow_rect, radius + spread),
        );
    }

    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.overlay,
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &shape,
    );
}
