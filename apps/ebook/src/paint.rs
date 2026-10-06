//! The reader's page rules: the separators drawn on the page itself. They
//! are content drawing in the page's own palette; every colour is the
//! caller's. The margin marks beside a paragraph (a note, a bookmark) are
//! `ui::icons` glyphs.

use vello::Scene;
use vello::kurbo::{Affine, Cap, Join, Line, Point, Stroke};
use vello::peniko::Color;

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, color: Color, width: f64) {
    scene.stroke(&rstroke(width), t, color, None, &Line::new(a, b));
}

fn rstroke(w: f64) -> Stroke {
    Stroke::new(w).with_caps(Cap::Round).with_join(Join::Round)
}
