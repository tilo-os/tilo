//! The Tilo mark ([`super::Icon::Tilo`]): the brand's own shapes, two bars and the block between them,
//! in the brand's colours rather than an ink.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

/// Paint the mark centred in `rect`, its grid the largest square that fits: its two bars (the source's
/// first two shapes) and the block between them (the third), each in the brand's tone for the ground.
pub fn paint_tilo_mark(scene: &mut Scene, transform: Affine, rect: Rect, dark: bool) {
    let tones = if dark {
        crate::tokens::brand::MARK_ON_DARK
    } else {
        crate::tokens::brand::MARK_ON_LIGHT
    };
    let edge = rect.width().min(rect.height());
    let center = rect.center();
    let view = transform
        .pre_translate((center.x - edge / 2.0, center.y - edge / 2.0).into())
        .pre_scale(edge / super::LUCIDE_VIEWBOX);
    let outlines = super::icon_subpaths(super::Icon::Tilo);
    for (outline, tone) in outlines.iter().zip([tones.bars, tones.bars, tones.block]) {
        scene.fill(Fill::NonZero, view, tone, None, outline);
    }
}

/// Whether a glyph drawn in `ink` stands on a dark ground: a light ink is read on one.
pub(super) fn on_dark(ink: Color) -> bool {
    let [r, g, b, _] = ink.components;
    0.2126 * r + 0.7152 * g + 0.0722 * b > 0.5
}
