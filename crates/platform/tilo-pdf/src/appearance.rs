//! Appearance streams for the annotations pdfium does not draw itself.
//!
//! pdfium generates appearances for text markup, ink and notes, but a free
//! text annotation or a stamp without an `/AP` is invisible in most readers
//! (pdfium included). These are built here as form `XObject` content in the
//! page's user space — strokes as paths, text shaped and set in embedded
//! subset fonts ([`crate::font`]) — and attached by the export's patch pass.

use pdf_writer::types::{LineCapStyle, LineJoinStyle};
use pdf_writer::{Content, Name, Str};

use crate::edits::StampText;
use crate::font::{PlacedGlyph, ShapedLine, TextSetter};
use crate::geom::{Point, Rect};
use crate::model::Rgba;

/// A built appearance: content in user space, clipped to `bbox`, and the
/// fonts (by [`TextSetter`] index) its text uses.
pub(crate) struct Appearance {
    pub bbox: Rect,
    pub content: Vec<u8>,
    pub fonts: Vec<usize>,
}

fn unit(c: u8) -> f32 {
    f32::from(c) / 255.0
}

fn show_line(
    content: &mut Content,
    fonts: &mut Vec<usize>,
    line: &ShapedLine,
    origin: Point,
    size: f32,
) {
    let mut current: Option<usize> = None;
    for PlacedGlyph { font, cid, x, y } in &line.glyphs {
        if current != Some(*font) {
            let name = TextSetter::resource_name(*font);
            content.set_font(Name(name.as_bytes()), size);
            current = Some(*font);
            if !fonts.contains(font) {
                fonts.push(*font);
            }
        }
        content.set_text_matrix([1.0, 0.0, 0.0, 1.0, origin.x + x, origin.y + y]);
        content.show(Str(&cid.to_be_bytes()));
    }
}

/// A stamp: `strokes` drawn with round caps and joins, then each text line
/// from its baseline start, all in `color`, clipped to `rect`.
pub(crate) fn stamp(
    setter: &mut TextSetter,
    rect: Rect,
    color: Rgba,
    strokes: &[Vec<Point>],
    stroke_width: f32,
    text: &[StampText],
) -> Appearance {
    let mut content = Content::new();
    let mut fonts = Vec::new();
    content.save_state();
    content.rect(rect.x0, rect.y0, rect.width(), rect.height());
    content.clip_nonzero();
    content.end_path();
    let (r, g, b) = (unit(color.r), unit(color.g), unit(color.b));
    content.set_stroke_rgb(r, g, b);
    content.set_fill_rgb(r, g, b);
    content.set_line_width(stroke_width.max(0.1));
    content.set_line_cap(LineCapStyle::RoundCap);
    content.set_line_join(LineJoinStyle::RoundJoin);
    for stroke in strokes {
        let Some((start, others)) = stroke.split_first() else {
            continue;
        };
        content.move_to(start.x, start.y);
        for p in others {
            content.line_to(p.x, p.y);
        }
        content.stroke();
    }
    let lines: Vec<(ShapedLine, Point, f32)> = text
        .iter()
        .filter(|t| !t.text.trim().is_empty())
        .map(|t| {
            let size = t.size.max(1.0);
            (setter.shape_line(t.text.trim(), size), t.at, size)
        })
        .collect();
    if lines.iter().any(|(l, _, _)| !l.glyphs.is_empty()) {
        content.begin_text();
        for (line, at, size) in &lines {
            show_line(&mut content, &mut fonts, line, *at, *size);
        }
        content.end_text();
    }
    content.restore_state();
    Appearance {
        bbox: rect,
        content: content.finish().to_vec(),
        fonts,
    }
}

/// Inner padding between a free text box's edge and its text.
pub(crate) const FREE_TEXT_PADDING: f32 = 2.0;

/// Free text: `text` wrapped to `rect`'s width and set from its top-left in
/// `color` at `size`, clipped to `rect`.
pub(crate) fn free_text(
    setter: &mut TextSetter,
    rect: Rect,
    color: Rgba,
    size: f32,
    text: &str,
) -> Appearance {
    let size = size.max(1.0);
    let mut content = Content::new();
    let mut fonts = Vec::new();
    content.save_state();
    content.rect(rect.x0, rect.y0, rect.width(), rect.height());
    content.clip_nonzero();
    content.end_path();
    content.set_fill_rgb(unit(color.r), unit(color.g), unit(color.b));
    let width = (rect.width() - 2.0 * FREE_TEXT_PADDING).max(size);
    let lines = setter.wrap(text, size, width);
    let leading = size * 1.2;
    let mut baseline = rect.y1 - FREE_TEXT_PADDING - size * 0.9;
    if lines.iter().any(|l| !l.glyphs.is_empty()) {
        content.begin_text();
        for line in &lines {
            if baseline < rect.y0 - leading {
                break;
            }
            show_line(
                &mut content,
                &mut fonts,
                line,
                Point::new(rect.x0 + FREE_TEXT_PADDING, baseline),
                size,
            );
            baseline -= leading;
        }
        content.end_text();
    }
    content.restore_state();
    Appearance {
        bbox: rect,
        content: content.finish().to_vec(),
        fonts,
    }
}
