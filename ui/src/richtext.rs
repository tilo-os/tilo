//! `RichTextLayout` — attributed, word-wrapped, hit-testable text layout. The
//! toolkit's rich-text primitive (cosmic-text under the hood): per-run
//! **bold**/*italic*, underlines, wrapping to a width, caret geometry, selection
//! rectangles, and point↔offset hit-testing. Render it with
//! [`crate::text_render::TextRenderer::draw_rich`].

use cosmic_text::{
    Align, Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight, Wrap,
};

/// A generic font family the toolkit can resolve from system fonts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FontFamily {
    #[default]
    Sans,
    Serif,
    Mono,
}

impl FontFamily {
    fn to_cosmic(self) -> Family<'static> {
        match self {
            FontFamily::Sans => Family::SansSerif,
            FontFamily::Serif => Family::Serif,
            FontFamily::Mono => Family::Monospace,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            FontFamily::Sans => "Sans",
            FontFamily::Serif => "Serif",
            FontFamily::Mono => "Mono",
        }
    }
    /// Cycle to the next family (for a toolbar toggle).
    pub fn next(self) -> Self {
        match self {
            FontFamily::Sans => FontFamily::Serif,
            FontFamily::Serif => FontFamily::Mono,
            FontFamily::Mono => FontFamily::Sans,
        }
    }
}

/// Per-run baseline shift, for superscript / subscript runs.
///
/// `Super`/`Sub` cause the run's glyphs to be shaped at a reduced size
/// ([`BaselineShift::SCALE`]) and drawn with a vertical offset
/// ([`BaselineShift::offset`]) — up for super, down for sub — relative to the
/// surrounding text's baseline. `None` leaves the run on the normal baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BaselineShift {
    #[default]
    None,
    Super,
    Sub,
}

impl BaselineShift {
    /// Font-size multiplier applied to super/subscript runs (~0.7×).
    pub const SCALE: f32 = 0.7;

    pub fn is_shifted(self) -> bool {
        !matches!(self, BaselineShift::None)
    }

    /// Vertical draw-time offset (in px) for a run shaped at `font_size`.
    ///
    /// Returns the signed delta to add to the glyph baseline: **negative**
    /// (upward) for `Super`, **positive** (downward) for `Sub`, `0.0` for
    /// `None`. Tuned to ~0.33× / ~0.18× of the surrounding size so the shifted
    /// run reads clearly above/below the baseline without leaving the line box.
    pub fn offset(self, font_size: f32) -> f32 {
        match self {
            BaselineShift::None => 0.0,
            BaselineShift::Super => -font_size * 0.33,
            BaselineShift::Sub => font_size * 0.18,
        }
    }
}

/// A styled inline run. `size`/`family` override the block defaults per run.
#[derive(Clone, Debug)]
pub struct RichSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub size: Option<f32>,
    pub family: Option<FontFamily>,
    /// Per-run text colour (RGB). `None` uses the block's default ink.
    pub color: Option<[u8; 3]>,
    /// Per-run highlight / background colour (RGB). `None` paints no band.
    pub background: Option<[u8; 3]>,
    /// Per-run baseline shift for super/subscript. `None` is the baseline.
    pub baseline: BaselineShift,
}

impl RichSpan {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            size: None,
            family: None,
            color: None,
            background: None,
            baseline: BaselineShift::None,
        }
    }
    pub fn bold(mut self, b: bool) -> Self {
        self.bold = b;
        self
    }
    pub fn italic(mut self, i: bool) -> Self {
        self.italic = i;
        self
    }
    pub fn color(mut self, c: Option<[u8; 3]>) -> Self {
        self.color = c;
        self
    }
    pub fn background(mut self, c: Option<[u8; 3]>) -> Self {
        self.background = c;
        self
    }
    pub fn baseline(mut self, b: BaselineShift) -> Self {
        self.baseline = b;
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
    pub(crate) strikes: Vec<(usize, usize)>,
    /// Highlight bands: `(start_byte, end_byte, rgb)`. Painted behind glyphs.
    pub(crate) backgrounds: Vec<(usize, usize, [u8; 3])>,
    /// Baseline-shifted runs: `(start_byte, end_byte, shift)`. Glyphs in these
    /// ranges are already shaped at the reduced size; the draw path applies the
    /// vertical offset.
    pub(crate) baselines: Vec<(usize, usize, BaselineShift)>,
    line_height: f32,
}

impl RichTextLayout {
    /// Shape `spans` into `width`. `base_bold` makes the whole block bold
    /// (headings); `underlines` are byte ranges to underline; `align` sets
    /// horizontal alignment. `line_height_mult` scales the line advance
    /// (1.0 = default spacing).
    #[allow(clippy::too_many_arguments)]
    pub fn shape(
        fs: &mut FontSystem,
        spans: &[RichSpan],
        base_bold: bool,
        size: f32,
        width: f32,
        align: TextAlign,
        family: FontFamily,
        underlines: Vec<(usize, usize)>,
        strikes: Vec<(usize, usize)>,
        line_height_mult: f32,
    ) -> Self {
        let line_height = size * 1.4 * line_height_mult;
        let mut buffer = Buffer::new(fs, Metrics::new(size, line_height));
        let fam = family.to_cosmic();
        let base = Attrs::new().family(fam).weight(if base_bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        });

        // Byte offsets accumulate over the *non-empty* spans (matching what gets
        // fed to cosmic-text), so backgrounds/baselines align with glyph
        // start/end indices reported in `layout_runs`.
        let mut backgrounds: Vec<(usize, usize, [u8; 3])> = Vec::new();
        let mut baselines: Vec<(usize, usize, BaselineShift)> = Vec::new();
        let mut cursor = 0usize;

        let built: Vec<(&str, Attrs)> = if spans.iter().all(|r| r.text.is_empty()) {
            vec![("", base.clone())]
        } else {
            spans
                .iter()
                .filter(|r| !r.text.is_empty())
                .map(|r| {
                    let run_fam = r.family.map(|f| f.to_cosmic()).unwrap_or(fam);
                    let base_run_size = r.size.unwrap_or(size);
                    // Super/subscript runs shape smaller so they advance narrower.
                    let run_size = if r.baseline.is_shifted() {
                        base_run_size * BaselineShift::SCALE
                    } else {
                        base_run_size
                    };
                    let start = cursor;
                    let end = cursor + r.text.len();
                    cursor = end;
                    if let Some(bg) = r.background {
                        backgrounds.push((start, end, bg));
                    }
                    if r.baseline.is_shifted() {
                        baselines.push((start, end, r.baseline));
                    }
                    let mut a = Attrs::new()
                        .family(run_fam)
                        .weight(if r.bold || base_bold {
                            Weight::BOLD
                        } else {
                            Weight::NORMAL
                        })
                        .metrics(Metrics::new(run_size, run_size * 1.4 * line_height_mult));
                    if r.italic {
                        a = a.style(Style::Italic);
                    }
                    if let Some([cr, cg, cb]) = r.color {
                        a.color_opt = Some(cosmic_text::Color::rgb(cr, cg, cb));
                    }
                    (r.text.as_str(), a)
                })
                .collect()
        };

        buffer.set_rich_text(built, &base, Shaping::Advanced, align.to_cosmic());
        buffer.set_size(Some(width), None);
        buffer.set_wrap(Wrap::Word);
        buffer.shape_until_scroll(fs, false);

        Self {
            buffer,
            underlines,
            strikes,
            backgrounds,
            baselines,
            line_height,
        }
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

    /// Baseline shift in effect at byte offset `byte`, if any range covers it.
    /// Used by the draw path to nudge super/subscript glyphs off the baseline.
    pub(crate) fn baseline_at(&self, byte: usize) -> BaselineShift {
        for &(s, e, shift) in &self.baselines {
            if byte >= s && byte < e {
                return shift;
            }
        }
        BaselineShift::None
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
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
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
    fn center_alignment_offsets_text() {
        let mut fs = FontSystem::new();
        let left = RichTextLayout::shape(
            &mut fs,
            &[RichSpan::new("hi")],
            false,
            16.0,
            400.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        let center = RichTextLayout::shape(
            &mut fs,
            &[RichSpan::new("hi")],
            false,
            16.0,
            400.0,
            TextAlign::Center,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        let lx = left.caret_geometry(0).0;
        let cx = center.caret_geometry(0).0;
        assert!(
            cx > lx + 50.0,
            "centered text should start further right: left={lx} center={cx}"
        );
    }

    #[test]
    fn wrapping_produces_multiple_lines() {
        let mut fs = FontSystem::new();
        let long = "word ".repeat(60);
        let one = RichTextLayout::shape(
            &mut fs,
            &[RichSpan::new("word")],
            false,
            16.0,
            1000.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        let wrapped = RichTextLayout::shape(
            &mut fs,
            &[RichSpan::new(long)],
            false,
            16.0,
            120.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        assert!(wrapped.height() > one.height() * 3.0);
    }

    #[test]
    fn background_span_records_a_paintable_band() {
        let mut fs = FontSystem::new();
        // "AAA" plain, "BBB" highlighted, "CCC" plain.
        let spans = [
            RichSpan::new("AAA"),
            RichSpan::new("BBB").background(Some([255, 230, 0])),
            RichSpan::new("CCC"),
        ];
        let layout = RichTextLayout::shape(
            &mut fs,
            &spans,
            false,
            16.0,
            400.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        // Exactly one background band, covering the "BBB" byte range [3, 6).
        assert_eq!(layout.backgrounds.len(), 1);
        let (s, e, rgb) = layout.backgrounds[0];
        assert_eq!((s, e), (3, 6));
        assert_eq!(rgb, [255, 230, 0]);
        // The draw path fills exactly `selection_rects(s, e)`; that band must be
        // a real, non-degenerate rectangle (x1 > x0, y1 > y0).
        let rects = layout.selection_rects(s, e);
        assert_eq!(rects.len(), 1, "highlight is a single contiguous band");
        let (x0, y0, x1, y1) = rects[0];
        assert!(x1 > x0, "highlight band must have positive width");
        assert!(y1 > y0, "highlight band must have positive height");
        // A span without a background contributes no band.
        let plain = RichTextLayout::shape(
            &mut fs,
            &[RichSpan::new("AAA")],
            false,
            16.0,
            400.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        assert!(plain.backgrounds.is_empty());
    }

    #[test]
    fn super_run_shapes_smaller_and_offsets_up() {
        let mut fs = FontSystem::new();
        let base_size = 20.0f32;
        // "x" normal, "2" superscript.
        let spans = [
            RichSpan::new("x"),
            RichSpan::new("2").baseline(BaselineShift::Super),
        ];
        let layout = RichTextLayout::shape(
            &mut fs,
            &spans,
            false,
            base_size,
            400.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        // Recorded as a baseline range over the "2" byte [1, 2).
        assert_eq!(layout.baselines.len(), 1);
        assert_eq!((layout.baselines[0].0, layout.baselines[0].1), (1, 2));
        assert_eq!(layout.baselines[0].2, BaselineShift::Super);
        assert_eq!(layout.baseline_at(1), BaselineShift::Super);
        assert_eq!(layout.baseline_at(0), BaselineShift::None);

        // The superscript glyph is shaped at the reduced size; the base glyph
        // keeps the full size.
        let mut base_glyph_size = None;
        let mut super_glyph_size = None;
        for run in layout.buffer.layout_runs() {
            for g in run.glyphs.iter() {
                if g.start == 0 {
                    base_glyph_size = Some(g.font_size);
                } else if g.start == 1 {
                    super_glyph_size = Some(g.font_size);
                }
            }
        }
        let bg = base_glyph_size.expect("base glyph present");
        let sg = super_glyph_size.expect("super glyph present");
        assert!(
            (bg - base_size).abs() < 0.01,
            "base run keeps full size: {bg}"
        );
        assert!(sg < bg, "super run shapes smaller: super={sg} base={bg}");
        assert!(
            (sg - base_size * BaselineShift::SCALE).abs() < 0.5,
            "super ~0.7x: {sg}"
        );

        // The draw-time vertical offset moves super UP (negative screen-y) and
        // sub DOWN (positive screen-y); None never moves.
        assert!(
            BaselineShift::Super.offset(sg) < 0.0,
            "super lifts above baseline"
        );
        assert!(
            BaselineShift::Sub.offset(sg) > 0.0,
            "sub drops below baseline"
        );
        assert_eq!(BaselineShift::None.offset(sg), 0.0);
    }

    #[test]
    fn line_height_mult_scales_line_advance() {
        let mut fs = FontSystem::new();
        // Force two lines so the gap between line tops reflects the line advance.
        let long = "word ".repeat(40);
        let single = RichTextLayout::shape(
            &mut fs,
            &[RichSpan::new(long.clone())],
            false,
            16.0,
            120.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        let doubled = RichTextLayout::shape(
            &mut fs,
            &[RichSpan::new(long)],
            false,
            16.0,
            120.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            2.0,
        );
        let advance = |layout: &RichTextLayout| -> f32 {
            let tops: Vec<f32> = layout.buffer.layout_runs().map(|r| r.line_top).collect();
            assert!(
                tops.len() >= 2,
                "need at least two lines to measure advance"
            );
            tops[1] - tops[0]
        };
        let a1 = advance(&single);
        let a2 = advance(&doubled);
        assert!(
            (a2 - 2.0 * a1).abs() < a1 * 0.1,
            "2.0 multiple should ~double the per-line advance: 1.0={a1} 2.0={a2}",
        );
        // And the reported block height scales with it too.
        assert!(doubled.height() > single.height() * 1.8);
    }
}
