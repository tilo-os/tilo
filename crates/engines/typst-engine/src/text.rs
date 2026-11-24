//! The selectable text of a laid-out page: one cell per glyph, in reading order, in page points,
//! extracted from the compiled frame so positions line up with the picture.

use typst::layout::{Frame, FrameItem, Point as TypstPoint, Transform};
use typst::text::TextItem;

/// One selectable glyph, in page points: its box, the line box it stands on, and what it draws.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Cell {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
    pub top: f64,
    pub bottom: f64,
    /// The line box it stands on, at half a point, so cells of one line share it.
    pub line: i64,
}

/// One page's glyph cells, in reading order.
#[derive(Clone, Debug, Default)]
pub struct PageText {
    cells: Vec<Cell>,
}

impl PageText {
    pub fn of_frame(frame: &Frame) -> Self {
        let mut text = Self::default();
        text.walk(frame, Affine::IDENTITY);
        text
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Add a run of glyph cells (`x0, x1, characters`) on the line box `top..bottom`.
    fn push_run<'a>(
        &mut self,
        top: f64,
        bottom: f64,
        cells: impl IntoIterator<Item = (f64, f64, &'a str)>,
    ) {
        let line = (top * 2.0).round() as i64;
        for (x0, x1, text) in cells {
            self.cells.push(Cell {
                text: text.to_owned(),
                x0,
                x1,
                top,
                bottom,
                line,
            });
        }
    }

    /// Follow nested groups with their transforms composed in, so text placed inside `#align`,
    /// `#box`, tables and the like lands at the right spot.
    fn walk(&mut self, frame: &Frame, cur: Affine) {
        for (pos, item) in frame.items() {
            match item {
                FrameItem::Group(group) => {
                    let child = cur
                        .then(Affine::translate(pos.x.to_pt(), pos.y.to_pt()))
                        .then(transform_to_affine(&group.transform));
                    self.walk(&group.frame, child);
                }
                FrameItem::Text(text) => self.push_item(*pos, text, cur),
                _ => {}
            }
        }
    }

    /// One shaped `TextItem` placed at `pos` inside a frame whose local-to-page transform is
    /// `cur`. Glyph advances accumulate along the baseline; the line box comes from the font's
    /// ascender and descender so cells on adjacent lines do not overlap. Rotation and shear are
    /// not modelled (documents lay text out axis-aligned); the x and y scale is honoured.
    fn push_item(&mut self, pos: TypstPoint, item: &TextItem, cur: Affine) {
        if item.glyphs.is_empty() {
            return;
        }
        let size = item.size;
        let metrics = item.font.metrics();
        let ascent = metrics.ascender.at(size).to_pt();
        let descent = metrics.descender.at(size).to_pt(); // negative (below the baseline)

        let (sx, sy) = cur.scale();
        let (origin_x, origin_y) = cur.point(pos.x.to_pt(), pos.y.to_pt());

        let mut cum = 0.0f64;
        let cells = item.glyphs.iter().map(|g| {
            let advance = g.x_advance.at(size).to_pt();
            let x0 = origin_x + cum * sx;
            let x1 = origin_x + (cum + advance) * sx;
            cum += advance;
            let range = usize::from(g.range.start)..usize::from(g.range.end);
            (x0, x1, item.text.get(range).unwrap_or(""))
        });
        self.push_run(origin_y - ascent * sy, origin_y - descent * sy, cells);
    }
}

/// A 2D affine, laid out as the six coefficients row-major (`sx, ky, kx, sy, tx, ty`), the same
/// order Typst's own transform states and its renderer reads.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine([f64; 6]);

impl Affine {
    const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    fn translate(x: f64, y: f64) -> Self {
        Self([1.0, 0.0, 0.0, 1.0, x, y])
    }

    fn new(coeffs: [f64; 6]) -> Self {
        Self(coeffs)
    }

    /// `self` after `then`: the transform that applies `then` first and then `self`.
    fn then(self, then: Self) -> Self {
        let a = self.0;
        let b = then.0;
        Self([
            a[0] * b[0] + a[2] * b[1],
            a[1] * b[0] + a[3] * b[1],
            a[0] * b[2] + a[2] * b[3],
            a[1] * b[2] + a[3] * b[3],
            a[0] * b[4] + a[2] * b[5] + a[4],
            a[1] * b[4] + a[3] * b[5] + a[5],
        ])
    }

    fn point(self, x: f64, y: f64) -> (f64, f64) {
        let a = self.0;
        (a[0] * x + a[2] * y + a[4], a[1] * x + a[3] * y + a[5])
    }

    /// The x and y scale of the transform, as its basis vectors' lengths.
    fn scale(self) -> (f64, f64) {
        let a = self.0;
        (a[0].hypot(a[1]).max(1e-6), a[2].hypot(a[3]).max(1e-6))
    }
}

/// A Typst affine as the renderer reads it.
fn transform_to_affine(t: &Transform) -> Affine {
    Affine::new([
        t.sx.get(),
        t.ky.get(),
        t.kx.get(),
        t.sy.get(),
        t.tx.to_pt(),
        t.ty.to_pt(),
    ])
}
