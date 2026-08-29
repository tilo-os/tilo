//! A themed status bar: a thin strip with left- and right-aligned text (counts,
//! document info, cursor position, …).

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

/// Standard status-bar height.
pub const STATUS_BAR_HEIGHT: f64 = 28.0;

/// Draw a themed status bar in `rect` with `left`/`right`-aligned text.
pub fn draw(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    left: &str,
    right: &str,
) {
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised,
        None,
        &rect,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.border_standard,
        None,
        &Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + 1.0),
    );

    let muted = Color::from_rgb8(0x9a, 0x9c, 0xa4);
    let ty = rect.y0 + (rect.height() - 15.0) / 2.0;
    tr.draw_text(
        scene,
        left,
        13.0,
        muted,
        transform * Affine::translate((rect.x0 + 18.0, ty)),
        None,
    );
    let (rw, _) = tr.measure(right, 13.0, None);
    tr.draw_text(
        scene,
        right,
        13.0,
        muted,
        transform * Affine::translate((rect.x1 - 18.0 - f64::from(rw), ty)),
        None,
    );
}
