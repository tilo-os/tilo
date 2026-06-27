//! `RichTextLayout` — attributed, word-wrapped, hit-testable text layout built on
//! cosmic-text. This is the missing toolkit primitive a real editor needs:
//! per-run **bold**/*italic*/underline, wrapping to a width, caret geometry,
//! selection rectangles, and point↔offset hit-testing. (Intended to graduate
//! into the `ui` toolkit once stable.)

use std::collections::HashMap;

use cosmic_text::fontdb;
use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, Style, Weight, Wrap};
use vello::kurbo::Affine;
use vello::peniko::{Color, Fill, FontData, StyleRef};
use vello::Scene;

/// A styled inline run within a block.
#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

/// A laid-out block of attributed text.
pub struct RichTextLayout {
    buffer: Buffer,
    line_height: f32,
    /// Underlined byte ranges (drawn as lines under the glyphs).
    underlines: Vec<(usize, usize)>,
}

impl RichTextLayout {
    /// Shape `runs` (each carrying bold/italic) into `width`. `base_bold` makes
    /// the whole block bold (headings). `underlines` are byte ranges to underline.
    pub fn shape(
        fs: &mut FontSystem,
        runs: &[Run],
        base_bold: bool,
        size: f32,
        width: f32,
        underlines: Vec<(usize, usize)>,
    ) -> Self {
        let line_height = size * 1.4;
        let mut buffer = Buffer::new(fs, Metrics::new(size, line_height));
        let base = Attrs::new().weight(if base_bold { Weight::BOLD } else { Weight::NORMAL });

        // cosmic-text needs at least one span; an empty block still lays out one
        // (zero-width) line so the caret + height are well-defined.
        let spans: Vec<(&str, Attrs)> = if runs.iter().all(|r| r.text.is_empty()) {
            vec![("", base.clone())]
        } else {
            runs.iter()
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

        buffer.set_rich_text(spans, &base, Shaping::Advanced, None);
        buffer.set_size(Some(width), None);
        buffer.set_wrap(Wrap::Word);
        buffer.shape_until_scroll(fs, false);

        Self { buffer, line_height, underlines }
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
            // empty line (no glyphs) but the caret may belong here
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

    /// Draw the glyphs (single `color`) + any underlines into `scene`.
    pub fn draw(
        &self,
        scene: &mut Scene,
        font_system: &FontSystem,
        font_cache: &mut HashMap<fontdb::ID, FontData>,
        transform: Affine,
        color: Color,
    ) {
        for run in self.buffer.layout_runs() {
            // Batch consecutive glyphs by font face.
            let mut face: Option<fontdb::ID> = None;
            let mut batch: Vec<vello::Glyph> = Vec::new();
            let mut size = self.line_height / 1.4;
            for g in run.glyphs.iter() {
                size = g.font_size;
                if face != Some(g.font_id) && !batch.is_empty() {
                    flush(scene, font_system, font_cache, face.unwrap(), &batch, size, color, transform);
                    batch.clear();
                }
                face = Some(g.font_id);
                batch.push(vello::Glyph { id: g.glyph_id as u32, x: g.x, y: run.line_y + g.y });
            }
            if let Some(f) = face {
                if !batch.is_empty() {
                    flush(scene, font_system, font_cache, f, &batch, size, color, transform);
                }
            }
        }

        // Underlines: a thin rect just below the baseline for each underlined run.
        for &(s, e) in &self.underlines {
            for (x0, y0, x1, y1) in self.selection_rects(s, e) {
                let _ = (y0,);
                let uy = y1 - (y1 - y0) * 0.12;
                let rect = vello::kurbo::Rect::new(x0 as f64, uy as f64, x1 as f64, (uy + 1.5) as f64);
                scene.fill(Fill::NonZero, transform, color, None, &rect);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn flush(
    scene: &mut Scene,
    font_system: &FontSystem,
    font_cache: &mut HashMap<fontdb::ID, FontData>,
    face_id: fontdb::ID,
    glyphs: &[vello::Glyph],
    size: f32,
    color: Color,
    transform: Affine,
) {
    let font = font_cache.entry(face_id).or_insert_with(|| {
        let data = font_system.db().face(face_id).and_then(|f| match &f.source {
            fontdb::Source::Binary(d) => Some(d.as_ref().as_ref().to_vec()),
            fontdb::Source::File(p) => std::fs::read(p).ok(),
            fontdb::Source::SharedFile(_, d) => Some(d.as_ref().as_ref().to_vec()),
        });
        FontData::new(data.unwrap_or_default().into(), 0)
    });

    scene
        .draw_glyphs(font)
        .font_size(size)
        .brush(color)
        .transform(transform)
        .draw(StyleRef::Fill(Fill::NonZero), glyphs.iter().copied());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Run {
        Run { text: text.into(), bold: false, italic: false }
    }

    #[test]
    fn shapes_caret_hit_and_selection() {
        let mut fs = FontSystem::new();
        let layout = RichTextLayout::shape(&mut fs, &[run("hello world")], false, 16.0, 400.0, vec![]);
        assert!(layout.height() > 0.0);

        let (x0, _, h) = layout.caret_geometry(0);
        assert!(x0 < 2.0, "caret at 0 is at the left, got {x0}");
        assert!(h > 0.0);

        let (xend, _, _) = layout.caret_geometry(11);
        assert!(xend > x0, "caret at end is right of caret at start");

        // hitting near the end yields a non-zero offset
        assert!(layout.hit(xend - 1.0, 1.0) > 0);

        // selecting "hello" yields one rect with positive width
        let rects = layout.selection_rects(0, 5);
        assert_eq!(rects.len(), 1);
        assert!(rects[0].2 > rects[0].0);
    }

    #[test]
    fn empty_block_has_one_line_and_zero_caret() {
        let mut fs = FontSystem::new();
        let layout = RichTextLayout::shape(&mut fs, &[run("")], false, 16.0, 400.0, vec![]);
        assert!(layout.height() > 0.0, "empty block still occupies a line");
        let (x, _, h) = layout.caret_geometry(0);
        assert!(x < 2.0 && h > 0.0);
    }

    #[test]
    fn wrapping_produces_multiple_lines() {
        let mut fs = FontSystem::new();
        let long = "word ".repeat(60);
        let one = RichTextLayout::shape(&mut fs, &[run("word")], false, 16.0, 1000.0, vec![]);
        let wrapped = RichTextLayout::shape(&mut fs, &[run(&long)], false, 16.0, 120.0, vec![]);
        assert!(wrapped.height() > one.height() * 3.0, "narrow width wraps onto many lines");
    }
}

