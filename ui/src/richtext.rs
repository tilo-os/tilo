//! `RichTextLayout` — attributed, word-wrapped, hit-testable text layout. The
//! toolkit's rich-text primitive (cosmic-text under the hood): per-run
//! **bold**/*italic*, underlines, wrapping to a width, caret geometry, selection
//! rectangles, and point↔offset hit-testing. Render it with
//! [`crate::text_render::TextRenderer::draw_rich`].

use cosmic_text::{Align, Attrs, Buffer, FontSystem, Metrics, Shaping, Style, Weight, Wrap};

/// A styled inline run.
#[derive(Clone, Debug)]
pub struct RichSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

impl RichSpan {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), bold: false, italic: false }
    }
    pub fn bold(mut self, b: bool) -> Self {
        self.bold = b;
        self
    }
    pub fn italic(mut self, i: bool) -> Self {
        self.italic = i;
        self
    }
}

/// Horizontal alignment of a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    fn to_cosmic(self) -> Option<Align> {
        match self {
            TextAlign::Left => None,
            TextAlign::Center => Some(Align::Center),
            TextAlign::Right => Some(Align::Right),
        }
    }
}

/// A laid-out block of attributed text.
pub struct RichTextLayout {
    pub(crate) buffer: Buffer,
    pub(crate) underlines: Vec<(usize, usize)>,
    line_height: f32,
}

impl RichTextLayout {
    /// Shape `spans` into `width`. `base_bold` makes the whole block bold
    /// (headings); `underlines` are byte ranges to underline; `align` sets
    /// horizontal alignment.
    pub fn shape(
        fs: &mut FontSystem,
        spans: &[RichSpan],
        base_bold: bool,
        size: f32,
        width: f32,
        align: TextAlign,
        underlines: Vec<(usize, usize)>,
    ) -> Self {
        let line_height = size * 1.4;
        let mut buffer = Buffer::new(fs, Metrics::new(size, line_height));
        let base = Attrs::new().weight(if base_bold { Weight::BOLD } else { Weight::NORMAL });

        let built: Vec<(&str, Attrs)> = if spans.iter().all(|r| r.text.is_empty()) {
            vec![("", base.clone())]
        } else {
            spans
                .iter()
                .filter(|r| !r.text.is_empty())
                .map(|r| {
                    let mut a =
                        Attrs::new().weight(if r.bold || base_bold { Weight::BOLD } else { Weight::NORMAL });
                    if r.italic {
                        a = a.style(Style::Italic);
                    }
                    (r.text.as_str(), a)
                })
                .collect()
        };

        buffer.set_rich_text(built, &base, Shaping::Advanced, align.to_cosmic());
        buffer.set_size(Some(width), None);
        buffer.set_wrap(Wrap::Word);
        buffer.shape_until_scroll(fs, false);

        Self { buffer, underlines, line_height }
    }

    /// Total laid-out height (at least one line tall).
    pub fn height(&self) -> f32 {
        self.buffer
            .layout_runs()
            .map(|r| r.line_top + r.line_height)
            .fold(0.0f32, f32::max)
            .max(self.line_height)
    }

    /// Map a local point to a byte offset (click → caret).
    pub fn hit(&self, x: f32, y: f32) -> usize {
        self.buffer.hit(x, y).map(|c| c.index).unwrap_or(0)
    }

    /// Caret geometry for a byte offset: `(x, top_y, height)`.
    pub fn caret_geometry(&self, byte: usize) -> (f32, f32, f32) {
        let mut last: Option<(f32, f32, f32)> = None;
        for run in self.buffer.layout_runs() {
            for g in run.glyphs.iter() {
                if byte <= g.start {
                    return (g.x, run.line_top, run.line_height);
                }
                last = Some((g.x + g.w, run.line_top, run.line_height));
            }
            if run.glyphs.is_empty() && last.is_none() {
                last = Some((0.0, run.line_top, run.line_height));
            }
        }
        last.unwrap_or((0.0, 0.0, self.line_height))
    }

    /// Selection rectangles `(x0, y0, x1, y1)` for byte range `[start, end)`.
    pub fn selection_rects(&self, start: usize, end: usize) -> Vec<(f32, f32, f32, f32)> {
        let mut rects = Vec::new();
        if start >= end {
            return rects;
        }
        for run in self.buffer.layout_runs() {
            let mut x0: Option<f32> = None;
            let mut x1 = 0.0f32;
            for g in run.glyphs.iter() {
                if g.end > start && g.start < end {
                    if x0.is_none() {
                        x0 = Some(g.x);
                    }
                    x1 = g.x + g.w;
                }
            }
            if let Some(x0) = x0 {
                rects.push((x0, run.line_top, x1, run.line_top + run.line_height));
            }
        }
        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_caret_hit_and_selection() {
        let mut fs = FontSystem::new();
        let layout = RichTextLayout::shape(
            &mut fs,
            &[RichSpan::new("hello world")],
            false,
            16.0,
            400.0,
            TextAlign::Left,
            vec![],
        );
        assert!(layout.height() > 0.0);
        let (x0, _, h) = layout.caret_geometry(0);
        assert!(x0 < 2.0 && h > 0.0);
        let (xend, _, _) = layout.caret_geometry(11);
        assert!(xend > x0);
        assert!(layout.hit(xend - 1.0, 1.0) > 0);
        let rects = layout.selection_rects(0, 5);
        assert_eq!(rects.len(), 1);
        assert!(rects[0].2 > rects[0].0);
    }

    #[test]
    fn wrapping_produces_multiple_lines() {
        let mut fs = FontSystem::new();
        let long = "word ".repeat(60);
        let one = RichTextLayout::shape(&mut fs, &[RichSpan::new("word")], false, 16.0, 1000.0, TextAlign::Left, vec![]);
        let wrapped = RichTextLayout::shape(&mut fs, &[RichSpan::new(long)], false, 16.0, 120.0, TextAlign::Left, vec![]);
        assert!(wrapped.height() > one.height() * 3.0);
    }
}
