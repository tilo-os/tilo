//! The Tilo mark ([`super::Icon::Tilo`]): the brand's own shapes, two bars and the block
//! between them, in the brand's colours rather than an ink.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect, RoundedRect, Shape as _};
use vello::peniko::{Color, Fill};

/// One bar of the mark in its 100-unit view box; the other is its mirror.
fn side() -> BezPath {
    let mut path = BezPath::new();
    path.move_to((10.98, 31.85));
    path.quad_to(
        (10.979_892_448_752_274, 27.966_487_822_512_84),
        (14.4, 26.12),
    );
    path.line_to((20.26, 22.97));
    path.quad_to(
        (24.843_373_493_975_903, 20.506_024_096_385_54),
        (24.84, 25.71),
    );
    path.line_to((24.84, 74.29));
    path.quad_to(
        (24.843_373_493_975_903, 79.493_975_903_614_46),
        (20.26, 77.03),
    );
    path.line_to((14.4, 73.88));
    path.quad_to(
        (10.979_892_448_752_274, 72.033_512_177_487_15),
        (10.98, 68.15),
    );
    path.close_path();
    path
}

/// The block between the bars, in the view box.
fn block() -> RoundedRect {
    RoundedRect::new(29.0, 16.0, 71.0, 84.0, 6.0)
}

/// The mark's outlines in the 24-unit box every glyph is drawn in: what an SVG of it or a
/// filled glyph traces.
pub(super) fn subpaths() -> Vec<BezPath> {
    let to_glyph = Affine::scale(24.0 / 100.0);
    let mirror = Affine::translate((100.0, 0.0)) * Affine::scale_non_uniform(-1.0, 1.0);
    vec![
        to_glyph * side(),
        to_glyph * (mirror * side()),
        to_glyph * block().to_path(0.1),
    ]
}

/// Paint the mark centred in `rect`, its view box the largest square that fits. A dark ground
/// (`dark`) takes the brand's dark variant (`mark-dark.svg`): the light one's pale bars glare
/// there.
pub fn paint_tilo_mark(scene: &mut Scene, transform: Affine, rect: Rect, dark: bool) {
    static SIDE: std::sync::LazyLock<BezPath> = std::sync::LazyLock::new(side);
    let tones = if dark {
        crate::tokens::brand::MARK_ON_DARK
    } else {
        crate::tokens::brand::MARK_ON_LIGHT
    };
    let (sides, middle) = (tones.bars, tones.block);
    let edge = rect.width().min(rect.height());
    let center = rect.center();
    let view = transform
        .pre_translate((center.x - edge / 2.0, center.y - edge / 2.0).into())
        .pre_scale(edge / 100.0);
    scene.fill(Fill::NonZero, view, sides, None, &*SIDE);
    let mirrored = view
        .pre_translate((100.0, 0.0).into())
        .pre_scale_non_uniform(-1.0, 1.0);
    scene.fill(Fill::NonZero, mirrored, sides, None, &*SIDE);
    scene.fill(Fill::NonZero, view, middle, None, &block());
}

/// Whether a glyph drawn in `ink` stands on a dark ground: a light ink is read on one.
pub(super) fn on_dark(ink: Color) -> bool {
    let [r, g, b, _] = ink.components;
    0.2126 * r + 0.7152 * g + 0.0722 * b > 0.5
}
