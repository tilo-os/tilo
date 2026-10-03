//! Shared chrome for floating menu/popover surfaces.
//!
//! Overlay primitives own different placement and interaction models, but
//! their elevation must not drift into slightly different shadows, fills, and
//! borders. Keep that visual language here.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};

use crate::theme::Theme;

#[allow(
    dead_code,
    reason = "shared floating-surface painting is staged for widget adoption"
)]
pub(super) fn draw(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let radius = f64::from(theme.tokens.radii.control);
    crate::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );

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
