//! The selectable text of a laid-out page: one cell per glyph, in reading
//! order, in page points. Extracted from the compiled `typst` frame (the
//! layout the raster is rendered from) so positions line up exactly, once per
//! compile, below the UI thread.

use std::ops::Range;

use typst::layout::{Frame, FrameItem, Point as TypstPoint, Transform};
use typst::text::TextItem;
use vello::kurbo::{Affine, Point};

/// One page's text: its glyph cells and the characters they render, held in
/// one string so a page's text is one allocation however many glyphs it has.
#[derive(Clone, Debug, Default)]
pub struct PageText {
    text: String,
    cells: Vec<TextCell>,
}

/// One selectable glyph, in page points.
#[derive(Clone, Debug)]
pub struct TextCell {
    pub x0: f64,
    pub x1: f64,
    /// Top and bottom of the line box of the run it belongs to.
    pub top: f64,
    pub bottom: f64,
    /// The visual line it sits on (its run's quantised top): copying across
    /// lines inserts a newline between them.
    pub line: i64,
    /// The source characters it renders, in [`PageText`]'s string (empty for
    /// a ligature continuation, several for a combined glyph).
    text: Range<usize>,
}

impl PageText {
    pub const EMPTY: Self = Self {
        text: String::new(),
        cells: Vec::new(),
    };

    /// The text of the page laid out as `frame`.
    pub fn of_frame(frame: &Frame) -> Self {
        let mut text = Self::default();
        text.walk(frame, Affine::IDENTITY);
        text
    }

    /// Add a run of glyph cells (`x0, x1, characters`) on the line box
    /// `top..bottom`.
    pub fn push_run<'a>(
        &mut self,
        top: f64,
        bottom: f64,
        cells: impl IntoIterator<Item = (f64, f64, &'a str)>,
    ) {
        let line = (top * 2.0).round() as i64;
        for (x0, x1, chars) in cells {
            let start = self.text.len();
            self.text.push_str(chars);
            self.cells.push(TextCell {
                x0,
                x1,
                top,
                bottom,
                line,
                text: start..self.text.len(),
            });
        }
    }

    pub fn cells(&self) -> &[TextCell] {
        &self.cells
    }

    /// The characters `cell` (one of this page's) renders.
    pub fn text_of(&self, cell: &TextCell) -> &str {
        self.text.get(cell.text.clone()).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Follow nested groups with their transforms composed in, so text placed
    /// inside `#align`, `#box`, tables, etc. lands at the right spot.
    fn walk(&mut self, frame: &Frame, cur: Affine) {
        for (pos, item) in frame.items() {
            match item {
                FrameItem::Group(group) => {
                    let child = cur
                        * Affine::translate((pos.x.to_pt(), pos.y.to_pt()))
                        * transform_to_affine(&group.transform);
                    self.walk(&group.frame, child);
                }
                FrameItem::Text(text) => self.push_item(*pos, text, cur),
                _ => {}
            }
        }
    }

    /// One shaped `TextItem` placed at `pos` inside a frame whose local→page
    /// transform is `cur`. Glyph advances accumulate along the baseline; the
    /// line box comes from the font's ascender/descender so cells on adjacent
    /// lines don't vertically overlap. Rotation/shear is not modelled
    /// (documents lay text axis-aligned); the x/y scale of `cur` is honoured.
    fn push_item(&mut self, pos: TypstPoint, item: &TextItem, cur: Affine) {
        if item.glyphs.is_empty() {
            return;
        }
        let size = item.size;
        let metrics = item.font.metrics();
        let ascent = metrics.ascender.at(size).to_pt();
        let descent = metrics.descender.at(size).to_pt(); // negative (below baseline)

        let coeffs = cur.as_coeffs();
        let sx = coeffs[0].hypot(coeffs[1]).max(1e-6); // |x-basis| of `cur`
        let sy = coeffs[2].hypot(coeffs[3]).max(1e-6); // |y-basis| of `cur`
        let origin = cur * Point::new(pos.x.to_pt(), pos.y.to_pt());

        let mut cum = 0.0f64;
        let cells = item.glyphs.iter().map(|g| {
            let adv = g.x_advance.at(size).to_pt();
            let x0 = origin.x + cum * sx;
            let x1 = origin.x + (cum + adv) * sx;
            cum += adv;
            let range = g.range.start as usize..g.range.end as usize;
            (x0, x1, item.text.get(range).unwrap_or(""))
        });
        self.push_run(origin.y - ascent * sy, origin.y - descent * sy, cells);
    }
}

/// A typst affine transform → a kurbo one (same row-major layout typst-render
/// uses, so geometry matches the raster).
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
