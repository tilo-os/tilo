//! Status-marker primitives + a bridge to the shared `ui::icons::Icon` set.
//!
//! Every *glyph* Feeds draws — the refresh arrow, the star, the toolbar
//! actions, the sidebar chevrons, the responsive-chrome hamburger / back /
//! close — now comes from the one shared, themable Lucide set in
//! [`ui::icons::Icon`], so the whole OS renders the same 24×24, 1.5px-stroke
//! glyphs and a fix at the source reaches every app. The refresh glyph in
//! particular used to be hand-traced here and drew its arrowhead against the
//! sweep, reading as a counter-clockwise reload; `Icon::RotateCw` is the
//! canonical clockwise arrow.
//!
//! What remains here are the two *status markers* that aren't Lucide glyphs:
//! the solid unread `dot` and the hollow read `ring`. Both are theme-coloured
//! circle primitives the caller places directly, so they stay local.
//!
//! [`glyph_box`] is the small bridge that turns a hand-placed `(center, side)`
//! affordance into the [`Rect`] the `Icon::draw` helper expects (it centres a
//! 24-unit glyph inside the rect).

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

/// A square draw box of side `side`, centred at `center`, for handing a
/// hand-placed affordance to the shared [`ui::icons::Icon`] draw helper.
pub fn glyph_box(center: Point, side: f64) -> Rect {
    Rect::from_center_size(center, (side, side))
}

/// A solid dot — the "unread" marker.
pub fn dot(scene: &mut Scene, transform: Affine, center: Point, radius: f64, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, &Circle::new(center, radius));
}

/// A hollow ring — the "read" marker (subtle).
pub fn ring(scene: &mut Scene, transform: Affine, center: Point, radius: f64, color: Color) {
    scene.stroke(
        &Stroke::new(1.2),
        transform,
        color,
        None,
        &Circle::new(center, radius),
    );
}
