//! Marks a surface draws over the things on it while a person picks them:
//! the band a drag sweeps out, and the ring around what is selected. One
//! look for every surface that selects by dragging (the workspace canvas,
//! a board, a slide), from the theme's selection colours.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use crate::theme::Theme;

/// The selection band a drag sweeps out over a surface: the selection tint
/// with an accent edge, `line` wide (the theme's drag line).
pub fn paint_selection_band(scene: &mut Scene, transform: Affine, band: Rect, theme: &Theme) {
    let colors = &theme.tokens.colors;
    let line = drag_line(theme);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.selection_tint,
        None,
        &band,
    );
    scene.stroke(
        &Stroke::new(line),
        transform,
        colors.intent.accent,
        None,
        &band,
    );
}

/// The ring around a selected object of `radius` on a surface: the accent,
/// twice the drag line wide, just outside the object so it never covers it.
pub fn paint_selected_ring(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &Theme,
) {
    let width = drag_line(theme) * 2.0;
    scene.stroke(
        &Stroke::new(width),
        transform,
        theme.tokens.colors.intent.accent,
        None,
        &RoundedRect::from_rect(rect.inflate(width, width), radius + width),
    );
}

fn drag_line(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.drag_line_px).max(1.0)
}
