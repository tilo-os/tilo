//! `RichTextLayout` — attributed, word-wrapped, hit-testable text layout. The
//! toolkit's rich-text primitive (cosmic-text under the hood): per-run
//! **bold**/*italic*, underlines, wrapping to a width, caret geometry, selection
//! rectangles, and point↔offset hit-testing. Render it with
//! [`crate::text_render::TextRenderer::draw_rich`].

use cosmic_text::{
    Affinity, Align, Attrs, AttrsList, Buffer, BufferLine, Cursor, Family, FontSystem, LineEnding,
    Metrics, Shaping, Style, Weight, Wrap,
};
use tilo_sort::CompactSort as _;

use crate::text_break::OffsetMap;
use crate::text_selection::{TextAffinity, TextPosition};

pub mod inline_emoji;
#[path = "richtext_paragraphs.rs"]
mod paragraphs;
#[cfg(test)]
pub(crate) use paragraphs::take_prepared_paragraphs;

/// A generic font family the toolkit can resolve from system fonts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
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
    #[must_use]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
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
#[derive(Clone, Debug, PartialEq)]
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
    /// What the run is, when the run knows it: a mention is an
    /// [`Identifier`](crate::text_render::TextKind::Identifier), a link a
    /// [`Url`](crate::text_render::TextKind::Url). `None` takes the block
    /// style's kind, with the identifiers found in prose set apart
    /// ([`crate::text_detect`]).
    pub kind: Option<crate::text_render::TextKind>,
    /// The run is the `:name:` of an emoji of the window's workspace, drawn
    /// as its picture in a square of its own ([`inline_emoji`]); its bytes
    /// stay the text's.
    pub workspace_emoji: bool,
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
            kind: None,
            workspace_emoji: false,
        }
    }
    #[must_use]
    pub fn bold(mut self, b: bool) -> Self {
        self.bold = b;
        self
    }
    #[must_use]
    pub fn italic(mut self, i: bool) -> Self {
        self.italic = i;
        self
    }
    #[must_use]
    pub fn color(mut self, c: Option<[u8; 3]>) -> Self {
        self.color = c;
        self
    }
    #[must_use]
    pub fn background(mut self, c: Option<[u8; 3]>) -> Self {
        self.background = c;
        self
    }
    #[must_use]
    pub fn baseline(mut self, b: BaselineShift) -> Self {
        self.baseline = b;
        self
    }
    /// Say what the run is ([`crate::text_render::TextKind`]).
    #[must_use]
    pub fn kind(mut self, kind: crate::text_render::TextKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Whether the run carries no styling of its own: text in the block's
    /// style, as [`Self::new`] makes it.
    pub(crate) fn is_unstyled(&self) -> bool {
        !self.bold
            && !self.italic
            && self.size.is_none()
            && self.family.is_none()
            && self.color.is_none()
            && self.background.is_none()
            && self.baseline == BaselineShift::None
            && self.kind.is_none()
            && !self.workspace_emoji
    }
}

/// Horizontal alignment of a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justified,
}

impl TextAlign {
    fn to_cosmic(self) -> Option<Align> {
        match self {
            TextAlign::Left => None,
            TextAlign::Center => Some(Align::Center),
            TextAlign::Right => Some(Align::Right),
            TextAlign::Justified => Some(Align::Justified),
        }
    }
}

/// How far the ink of a layout's rows reaches past their own bands: above a
/// row's top and below its bottom, in the layout's units. Zero where every
/// glyph stays inside its row.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InkReach {
    pub above: f32,
    pub below: f32,
}

/// How far each logical line's rows' ink reaches past their bands, kept
/// as it is worked out the first time it is asked for
/// ([`crate::text_render::TextRenderer::rich_ink_reach`]), and the most of
/// them. Each line's reach is measured from its own rows, so an edit
/// forgets only the lines it replaced; a new width forgets them all.
#[derive(Default)]
pub(crate) struct InkReachCache {
    /// Each logical line's reach, once worked out; shorter than the lines
    /// where the ones past its end are not.
    pub(crate) lines: std::sync::Mutex<Vec<Option<InkReach>>>,
    /// The most any line's reaches, until a line changes.
    pub(crate) all: std::sync::OnceLock<InkReach>,
}

impl InkReachCache {
    /// Lines `first..=last` were replaced by `count` new ones.
    pub(crate) fn lines_replaced(&mut self, first: usize, last: usize, count: usize) {
        self.all = std::sync::OnceLock::new();
        let lines = self
            .lines
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last < lines.len() {
            lines.splice(first..=last, std::iter::repeat_n(None, count));
        } else {
            lines.truncate(first);
        }
    }

    /// Every line's rows changed.
    pub(crate) fn forget(&mut self) {
        *self = Self::default();
    }

    /// The same reaches, for a copy of the same rows.
    pub(crate) fn duplicate(&self) -> Self {
        Self {
            lines: std::sync::Mutex::new(
                self.lines
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            ),
            all: self.all.clone(),
        }
    }
}

/// A laid-out block of attributed text.
pub struct RichTextLayout {
    /// One exact source snapshot, scoped to this layout owner. It avoids
    /// reconstructing runs/offset maps/buffer lines for unchanged rich input.
    prepared: Option<RichPreparation>,
    pub(crate) buffer: Buffer,
    /// Global byte offset of every cosmic-text logical line. cosmic-text
    /// cursors are `(line, byte-within-line)`; the public toolkit API uses a
    /// single byte space over the complete paragraph/block.
    line_starts: Vec<usize>,
    /// Byte length of each logical line, excluding its trailing newline.
    line_lens: Vec<usize>,
    offset_map: OffsetMap,
    text_len: usize,
    pub(crate) underlines: Vec<(usize, usize)>,
    pub(crate) strikes: Vec<(usize, usize)>,
    /// Highlight bands: `(start_byte, end_byte, rgb)`. Painted behind glyphs.
    pub(crate) backgrounds: Vec<(usize, usize, [u8; 3])>,
    /// Baseline-shifted runs: `(start_byte, end_byte, shift)`. Glyphs in these
    /// ranges are already shaped at the reduced size; the draw path applies the
    /// vertical offset.
    pub(crate) baselines: Vec<(usize, usize, BaselineShift)>,
    /// The runs drawn as a workspace emoji's picture, in text order.
    pub(crate) inline_emoji: Vec<inline_emoji::InlineEmoji>,
    discretionary_hyphen_width: f32,
    line_height: f32,
    /// Block-level paint axes. The renderer replaces `wght` with each batch's
    /// shaped glyph weight so an explicit block axis cannot override span bold.
    pub(crate) font_variations: Vec<crate::text_render::FontVariation>,
    pub(crate) hanging_punctuation: crate::text_render::HangingPunctuation,
    fragmentation: crate::text_render::Fragmentation,
    /// Where each shaped logical line starts down the layout, and the
    /// tallest visual row: found once per shaped buffer, so a view of a
    /// long text finds its first visible row by bisection
    /// ([`Self::rows_within`]) instead of walking every row above it.
    row_tops: std::cell::OnceCell<RowTops>,
    /// The faces the runs asked for ([`requested_faces`]). The shaped
    /// buffer only records the face each glyph fell back to, so the web,
    /// where the monospace, serif and italic faces load on demand, reads
    /// these to fetch them.
    pub(crate) faces: Vec<crate::text_render::TextStyle>,
    /// How far the rows' ink reaches past their bands
    /// ([`crate::text_render::TextRenderer::rich_ink_reach`]).
    pub(crate) ink_reach: InkReachCache,
}

/// Where each shaped logical line of a buffer starts, as its rows are laid
/// out from the top, and the tallest row: up to the first line not shaped
/// yet, where the buffer's own rows stop too.
#[derive(Debug, Default)]
struct RowTops {
    starts: Vec<f32>,
    tallest: f32,
}

impl RowTops {
    fn of(buffer: &Buffer) -> Self {
        let default = buffer.metrics().line_height;
        let mut tops = Self {
            starts: Vec::with_capacity(buffer.lines.len()),
            tallest: default,
        };
        let mut top = 0.0;
        for line in &buffer.lines {
            let (Some(_), Some(layout)) = (line.shape_opt(), line.layout_opt()) else {
                break;
            };
            tops.starts.push(top);
            for row in layout {
                let height = row.line_height_opt.unwrap_or(default);
                tops.tallest = tops.tallest.max(height);
                top += height;
            }
        }
        tops
    }
}

/// The rows of an unscrolled buffer of unbounded height from the start of
/// line `line_i`, whose top is `line_top`, placed as
/// [`cosmic_text::LayoutRunIter`] places them from the first: each row's
/// top is the sum of the heights above it, added in the same order, so it
/// is the same to the bit.
struct RowsFrom<'b> {
    lines: &'b [cosmic_text::BufferLine],
    line_i: usize,
    layout_i: usize,
    line_top: f32,
    line_height: f32,
}

impl<'b> Iterator for RowsFrom<'b> {
    type Item = cosmic_text::LayoutRun<'b>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(line) = self.lines.get(self.line_i) {
            let shape = line.shape_opt()?;
            let layout = line.layout_opt()?;
            while let Some(row) = layout.get(self.layout_i) {
                self.layout_i = self.layout_i.saturating_add(1);
                let line_height = row.line_height_opt.unwrap_or(self.line_height);
                let line_top = self.line_top;
                let centering = (line_height - (row.max_ascent + row.max_descent)) / 2.0;
                let line_y = line_top + centering + row.max_ascent;
                self.line_top += line_height;
                if line_y + row.max_descent < 0.0 {
                    continue;
                }
                return Some(cosmic_text::LayoutRun {
                    line_i: self.line_i,
                    text: line.text(),
                    rtl: shape.rtl,
                    glyphs: &row.glyphs,
                    decorations: &row.decorations,
                    line_y,
                    line_top,
                    line_height,
                    line_w: row.w,
                });
            }
            self.line_i = self.line_i.saturating_add(1);
            self.layout_i = 0;
        }
        None
    }
}

/// [`RowsFrom`] where it applies, else the buffer's own walk.
enum Rows<'b> {
    From(RowsFrom<'b>),
    Walk(cosmic_text::LayoutRunIter<'b>),
}

impl<'b> Iterator for Rows<'b> {
    type Item = cosmic_text::LayoutRun<'b>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::From(rows) => rows.next(),
            Self::Walk(rows) => rows.next(),
        }
    }
}

/// The block style and each span's family, weight, slant and size
/// override, without repeats.
pub(crate) fn requested_faces(
    spans: &[RichSpan],
    style: &crate::text_render::TextStyle,
) -> Vec<crate::text_render::TextStyle> {
    use crate::text_render::{FontChoice, TextWeight};
    let mut faces = vec![style.clone()];
    for span in spans.iter().filter(|span| !span.text.is_empty()) {
        if span.family.is_none() && !span.bold && !span.italic && span.size.is_none() {
            continue;
        }
        let mut face = style.clone();
        if let Some(family) = span.family {
            face.family = match family {
                FontFamily::Sans => FontChoice::SansSerif,
                FontFamily::Serif => FontChoice::Serif,
                FontFamily::Mono => FontChoice::Monospace,
            };
        }
        if span.bold {
            face.weight = TextWeight::Bold;
        }
        face.italic |= span.italic;
        if let Some(size) = span.size {
            face.size = size;
        }
        if !faces.contains(&face) {
            faces.push(face);
        }
    }
    faces
}

#[derive(Clone)]
struct RichPreparation {
    spans: Vec<RichSpan>,
    style: crate::text_render::TextStyle,
    width: Option<f32>,
    align: TextAlign,
    browser_emoji: bool,
    /// Built paragraph by paragraph ([`RichTextLayout::shape_paragraphs_reusing_browser`]),
    /// whose fonts are planned per paragraph, not over the whole text.
    paragraph_planned: bool,
}

impl RichPreparation {
    fn matches(
        &self,
        spans: &[RichSpan],
        style: &crate::text_render::TextStyle,
        width: Option<f32>,
        align: TextAlign,
        browser_emoji: bool,
    ) -> bool {
        // A custom hyphenator's Arc is not an immutable revision witness.
        !matches!(
            style.hyphenation,
            crate::text_render::Hyphenation::Custom(_)
        ) && !self.paragraph_planned
            && self.spans == spans
            && self.style == *style
            && self.width == width
            && self.align == align
            && self.browser_emoji == browser_emoji
    }
}

/// Cosmic-text runs plus the byte-ranged background/baseline decoration
/// tables built alongside them by `RichTextLayout::build_rich_runs`.
type RichRuns<'a, 's> = (
    Vec<(
        std::borrow::Cow<'a, str>,
        Attrs<'s>,
        Option<crate::text_render::TextKind>,
    )>,
    Vec<(usize, usize, [u8; 3])>,
    Vec<(usize, usize, BaselineShift)>,
    Vec<inline_emoji::InlineEmoji>,
);

impl RichTextLayout {
    /// Revealed secret controls need geometry without an additional retained
    /// plaintext source snapshot. They opt out before keeping the layout.
    pub(crate) fn discard_prepared_inputs(&mut self) {
        self.prepared = None;
    }

    fn line_map(text: &str) -> (Vec<usize>, Vec<usize>) {
        let mut line_starts = vec![0];
        let mut line_lens = Vec::new();
        let mut line_start = 0;
        for (byte, ch) in text.char_indices() {
            if ch == '\n' {
                line_lens.push(byte.saturating_sub(line_start));
                line_start = byte.saturating_add(ch.len_utf8());
                line_starts.push(line_start);
            }
        }
        line_lens.push(text.len().saturating_sub(line_start));
        (line_starts, line_lens)
    }

    /// Shape an un-attributed paragraph with the same complete style used by
    /// [`crate::text_render::TextRenderer`]. `max_width = None` produces one
    /// unwrapped line (form fields); a width enables Unicode word wrapping.
    pub fn shape_plain(
        fs: &mut FontSystem,
        text: &str,
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
    ) -> Self {
        Self::shape_plain_browser(fs, text, style, max_width, false)
    }

    pub(crate) fn shape_plain_browser(
        fs: &mut FontSystem,
        text: &str,
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
        browser_emoji: bool,
    ) -> Self {
        let prep = StyledPrep {
            browser_emoji,
            extra: &[],
        };
        let mut layout = Self::shape_plain_with(fs, text, style, max_width, prep);
        if Self::hangs_wrapped_rows(style, max_width) {
            let mut extra = Vec::new();
            for _ in 0..WRAP_INDENT_PASSES {
                let wanted = layout.wrap_indent_insertions(text);
                if wanted == extra {
                    break;
                }
                extra = wanted;
                let prep = StyledPrep {
                    browser_emoji,
                    extra: &extra,
                };
                layout = Self::shape_plain_with(fs, text, style, max_width, prep);
            }
        }
        layout
    }

    fn shape_plain_with(
        fs: &mut FontSystem,
        text: &str,
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
        prep: StyledPrep<'_>,
    ) -> Self {
        let browser_emoji = prep.browser_emoji;
        let line_height = style.line_height_px();
        let offset_map = if max_width.is_some() {
            let mut map = style.hyphenation.offset_map(text);
            if text.len() > crate::text_break::HUGE_PARAGRAPH_BYTES {
                map.add_paragraph_chunks(text);
            }
            map.add_insertions(prep.extra);
            map
        } else {
            OffsetMap::default()
        };
        let discretionary_hyphen_width = if offset_map.has_discretionary_hyphens() {
            crate::text_render::discretionary_hyphen_advance(fs, style)
        } else {
            0.0
        };
        let shaping_text = offset_map.shape_text(text);
        let metrics = Metrics::new(style.size, line_height);
        let mut buffer = Buffer::new(fs, metrics);
        let attrs = style.cosmic_attrs();
        let spans = crate::font_catalog::matched::plan(
            fs.db(),
            crate::text_render::browser_emoji_runs(&shaping_text, &attrs, browser_emoji),
            metrics,
            style.kind,
            style.optical(),
            style.distinguish || crate::font_catalog::matched::distinguish_everywhere(),
        );
        crate::font_catalog::matched::set_spans(
            &mut buffer,
            &spans,
            &attrs,
            style.align.to_cosmic(),
        );
        let layout_width = max_width.map(|width| (width - discretionary_hyphen_width).max(0.0));
        buffer.set_size(layout_width, None);
        buffer.set_wrap(if max_width.is_some() {
            Wrap::Word
        } else {
            Wrap::None
        });
        buffer.shape_until_scroll(fs, false);
        if style.wrap_mode == crate::text_render::TextWrapMode::Balance
            && let Some(layout_width) = layout_width
        {
            crate::text_render::balance_buffer(&mut buffer, fs, layout_width);
        }
        let (line_starts, line_lens) = Self::line_map(&shaping_text);
        Self {
            prepared: None,
            faces: vec![style.clone()],
            buffer,
            line_starts,
            line_lens,
            offset_map,
            text_len: text.len(),
            underlines: Vec::new(),
            strikes: Vec::new(),
            backgrounds: Vec::new(),
            baselines: Vec::new(),
            inline_emoji: Vec::new(),
            discretionary_hyphen_width,
            line_height,
            font_variations: style.resolved_variations(),
            hanging_punctuation: style.hanging_punctuation,
            fragmentation: style.fragmentation,
            row_tops: std::cell::OnceCell::new(),
            ink_reach: InkReachCache::default(),
        }
    }

    /// The complete style of a legacy text block for [`Self::shape_styled`]:
    /// `base_bold` makes the whole block bold (headings) and
    /// `line_height_mult` scales the default 1.4 line advance.
    pub fn block_style(
        base_bold: bool,
        size: f32,
        family: FontFamily,
        line_height_mult: f32,
    ) -> crate::text_render::TextStyle {
        use crate::text_render::{FontChoice, TextStyle, TextWeight};
        let family = match family {
            FontFamily::Sans => FontChoice::SansSerif,
            FontFamily::Serif => FontChoice::Serif,
            FontFamily::Mono => FontChoice::Monospace,
        };
        TextStyle::new(size)
            .with_line_height(1.4 * line_height_mult)
            .with_weight(if base_bold {
                TextWeight::Bold
            } else {
                TextWeight::Regular
            })
            .with_family(family)
    }

    /// Shape attributed spans with the same complete block style used by
    /// plain application text. This is the preferred entry point for editors
    /// and custom widgets: font family, weight, size, and leading remain one
    /// value from measurement through painting.

    pub fn shape_styled(
        fs: &mut FontSystem,
        spans: &[RichSpan],
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
        align: TextAlign,
        underlines: Vec<(usize, usize)>,
        strikes: Vec<(usize, usize)>,
    ) -> Self {
        Self::shape_styled_reusing(
            fs, spans, style, max_width, align, underlines, strikes, None,
        )
    }

    pub(crate) fn shape_styled_browser(
        fs: &mut FontSystem,
        spans: &[RichSpan],
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
        align: TextAlign,
        underlines: Vec<(usize, usize)>,
        strikes: Vec<(usize, usize)>,
        browser_emoji: bool,
    ) -> Self {
        Self::shape_styled_reusing_browser(fs, spans, style, max_width, align, None, browser_emoji)
            .with_decorations(underlines, strikes)
    }

    /// Reuse unchanged paragraphs from this renderer's previous layout.
    /// The caller must enforce the same font-system identity and generation;
    /// font replacement or locale changes require `previous = None`.
    /// Exact retained inputs skip preparation; otherwise fresh paragraph
    /// construction remains authoritative for attributes and document offsets,
    /// and only compatible cosmic shape/layout caches move over.
    pub fn shape_styled_reusing(
        fs: &mut FontSystem,
        spans: &[RichSpan],
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
        align: TextAlign,
        underlines: Vec<(usize, usize)>,
        strikes: Vec<(usize, usize)>,
        previous: Option<Self>,
    ) -> Self {
        Self::shape_styled_reusing_browser(fs, spans, style, max_width, align, previous, false)
            .with_decorations(underlines, strikes)
    }

    pub(crate) fn shape_styled_reusing_browser(
        fs: &mut FontSystem,
        spans: &[RichSpan],
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
        align: TextAlign,
        previous: Option<Self>,
        browser_emoji: bool,
    ) -> Self {
        if let Some(mut retained) = previous {
            if retained.prepared.as_ref().is_some_and(|prepared| {
                prepared.matches(spans, style, max_width, align, browser_emoji)
            }) {
                // Decorations are supplied by the new call, not part of shape
                // identity. In particular, an old IME underline must not leak.
                retained.underlines.clear();
                retained.strikes.clear();
                return retained;
            }
            return Self::build_styled_indented(
                fs,
                spans,
                style,
                max_width,
                align,
                Some(retained),
                browser_emoji,
            );
        }
        Self::build_styled_indented(fs, spans, style, max_width, align, None, browser_emoji)
    }

    /// Build, then (for [`crate::text_render::TextWrapMode::Indented`]) hang
    /// each wrapped row under its line's indentation and build again until
    /// the rows stop moving.
    fn build_styled_indented(
        fs: &mut FontSystem,
        spans: &[RichSpan],
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
        align: TextAlign,
        previous: Option<Self>,
        browser_emoji: bool,
    ) -> Self {
        let prep = StyledPrep {
            browser_emoji,
            extra: &[],
        };
        let mut layout =
            Self::build_styled_reusing_browser(fs, spans, style, max_width, align, previous, prep);
        if Self::hangs_wrapped_rows(style, max_width) {
            let full_text: String = spans.iter().map(|span| span.text.as_str()).collect();
            let mut extra = Vec::new();
            for _ in 0..WRAP_INDENT_PASSES {
                let wanted = layout.wrap_indent_insertions(&full_text);
                if wanted == extra {
                    break;
                }
                extra = wanted;
                let prep = StyledPrep {
                    browser_emoji,
                    extra: &extra,
                };
                layout = Self::build_styled_reusing_browser(
                    fs,
                    spans,
                    style,
                    max_width,
                    align,
                    Some(layout),
                    prep,
                );
            }
        }
        layout
    }

    fn build_styled_reusing_browser(
        fs: &mut FontSystem,
        spans: &[RichSpan],
        style: &crate::text_render::TextStyle,
        max_width: Option<f32>,
        align: TextAlign,
        previous: Option<Self>,
        prep: StyledPrep<'_>,
    ) -> Self {
        let browser_emoji = prep.browser_emoji;
        let full_text: String = spans.iter().map(|span| span.text.as_str()).collect();
        let offset_map = if max_width.is_some() {
            let mut map = style.hyphenation.offset_map(&full_text);
            // Wrapped paragraphs beyond the huge-paragraph bound shape in
            // content-defined chunks, so an edit reshapes one chunk.
            if full_text.len() > crate::text_break::HUGE_PARAGRAPH_BYTES {
                map.add_paragraph_chunks(&full_text);
            }
            map.add_insertions(prep.extra);
            map
        } else {
            OffsetMap::default()
        };
        let discretionary_hyphen_width = if offset_map.has_discretionary_hyphens() {
            crate::text_render::discretionary_hyphen_advance(fs, style)
        } else {
            0.0
        };
        let shaping_text = offset_map.shape_text(&full_text);
        let (line_starts, line_lens) = Self::line_map(&shaping_text);

        let size = style.size;
        let line_height = style.line_height_px();
        let line_height_mult = style.line_height;
        let mut buffer = Buffer::new(fs, Metrics::new(size, line_height));
        let fam = style.family.to_cosmic();
        let base = style.cosmic_attrs().family(fam);

        let (built, backgrounds, baselines, inline_emoji) =
            Self::build_rich_runs(spans, style, &offset_map, &base, size, line_height_mult);

        let planned = crate::font_catalog::matched::plan_runs(
            fs.db(),
            built.iter().flat_map(|(text, attrs, kind)| {
                crate::text_render::browser_emoji_runs(text.as_ref(), attrs, browser_emoji)
                    .into_iter()
                    .map(move |(text, attrs)| (text, attrs, *kind))
            }),
            Metrics::new(size, line_height),
            style.kind,
            style.optical(),
            style.distinguish || crate::font_catalog::matched::distinguish_everywhere(),
        );
        buffer.set_rich_text(
            planned.iter().map(|(text, attrs)| (*text, attrs.clone())),
            &base,
            Shaping::Advanced,
            align.to_cosmic(),
        );
        // cosmic-text 0.19's rich setter omits the final empty paragraph after
        // an LF, unlike its plain setter. Restore that line at this boundary so
        // attributed and plain layouts expose the same bytes and geometry.
        if full_text.ends_with('\n') {
            let terminal_attrs = built
                .last()
                .map_or_else(|| base.clone(), |(_, attrs, _)| attrs.clone());
            let mut terminal_line = BufferLine::new(
                "",
                LineEnding::None,
                AttrsList::new(&terminal_attrs),
                Shaping::Advanced,
            );
            terminal_line.set_align(align.to_cosmic());
            buffer.lines.push(terminal_line);
        }
        let reserved_hyphen_width = if align == TextAlign::Left {
            discretionary_hyphen_width
        } else {
            0.0
        };
        let layout_width = max_width.map(|width| (width - reserved_hyphen_width).max(0.0));
        buffer.set_size(layout_width, None);
        buffer.set_wrap(if max_width.is_some() {
            Wrap::Word
        } else {
            Wrap::None
        });
        if let Some(previous) = previous {
            Self::reuse_unchanged_paragraphs(&mut buffer, previous.buffer);
        }
        buffer.shape_until_scroll(fs, false);
        if style.wrap_mode == crate::text_render::TextWrapMode::Balance
            && let Some(layout_width) = layout_width
        {
            crate::text_render::balance_buffer(&mut buffer, fs, layout_width);
        }

        Self {
            prepared: (!matches!(
                style.hyphenation,
                crate::text_render::Hyphenation::Custom(_)
            ))
            .then(|| RichPreparation {
                spans: spans.to_vec(),
                style: style.clone(),
                width: max_width,
                align,
                browser_emoji,
                paragraph_planned: false,
            }),
            faces: requested_faces(spans, style),
            buffer,
            line_starts,
            line_lens,
            offset_map,
            text_len: full_text.len(),
            underlines: Vec::new(),
            strikes: Vec::new(),
            backgrounds,
            baselines,
            inline_emoji,
            discretionary_hyphen_width,
            line_height,
            font_variations: style.resolved_variations(),
            hanging_punctuation: style.hanging_punctuation,
            fragmentation: style.fragmentation,
            row_tops: std::cell::OnceCell::new(),
            ink_reach: InkReachCache::default(),
        }
    }

    fn reuse_unchanged_paragraphs(buffer: &mut Buffer, mut previous: Buffer) {
        // BufferLine has separate shaping and line-wrapping caches. Width
        // changes only invalidate wrapping; retain shaping for paragraphs
        // whose text and attributes still match. Keep all other compatibility
        // checks conservative, including metrics and tab expansion.
        if buffer.metrics() != previous.metrics()
            || buffer.wrap() != previous.wrap()
            || buffer.ellipsize() != previous.ellipsize()
            || buffer.monospace_width() != previous.monospace_width()
            || buffer.tab_width() != previous.tab_width()
            || buffer.hinting() != previous.hinting()
        {
            return;
        }
        if buffer.size() != previous.size() {
            // The new buffer's TEXT_SET dirty flag assumes its lines are fresh,
            // so cosmic-text will not invalidate transferred layout caches.
            // Explicitly discard wrapping while preserving each ShapeLine.
            for line in &mut previous.lines {
                line.reset_layout();
            }
        }
        let same = |a: &BufferLine, b: &BufferLine| {
            a.text() == b.text()
                && a.ending() == b.ending()
                && a.attrs_list() == b.attrs_list()
                && a.align() == b.align()
        };
        let prefix = buffer
            .lines
            .iter()
            .zip(&previous.lines)
            .take_while(|(a, b)| same(a, b))
            .count();
        let suffix = buffer
            .lines
            .iter()
            .skip(prefix)
            .rev()
            .zip(previous.lines.iter().skip(prefix).rev())
            .take_while(|(a, b)| same(a, b))
            .count();
        let mut old_lines = previous.lines.into_iter();
        for (new, old) in buffer.lines.iter_mut().take(prefix).zip(old_lines.by_ref()) {
            *new = old;
        }
        for (new, old) in buffer
            .lines
            .iter_mut()
            .rev()
            .take(suffix)
            .zip(old_lines.by_ref().rev())
        {
            *new = old;
        }
        let end = buffer.lines.len() - suffix;
        if prefix == end || old_lines.len() == 0 {
            return;
        }
        // Prefix/suffix alone miss paragraphs between separate edits or moved
        // intact blocks. Index the remaining candidates, then verify exact text,
        // ending, attributes and alignment before moving a cache. Consume each
        // old paragraph once. A hash collision conservatively reshapes instead
        // of trusting the hash or scanning an unbounded collision chain.
        let mut candidates = std::collections::HashMap::<u64, Vec<BufferLine>>::new();
        for old in old_lines {
            candidates
                .entry(Self::paragraph_fingerprint(&old))
                .or_default()
                .push(old);
        }
        for new in &mut buffer.lines[prefix..end] {
            if let Some(bucket) = candidates.get_mut(&Self::paragraph_fingerprint(new))
                && bucket.last().is_some_and(|old| same(new, old))
                && let Some(old) = bucket.pop()
            {
                *new = old;
            }
        }
    }

    fn paragraph_fingerprint(line: &BufferLine) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        line.text().hash(&mut hash);
        (line.ending() as u8).hash(&mut hash);
        line.align().map(|align| align as u8).hash(&mut hash);
        line.attrs_list().defaults().hash(&mut hash);
        for (range, attrs) in line.attrs_list().spans_iter() {
            range.hash(&mut hash);
            attrs.hash(&mut hash);
        }
        hash.finish()
    }

    /// Build cosmic-text runs (with their `Attrs`) for `spans`, plus the
    /// byte-ranged background/baseline decoration tables that
    /// [`Self::shape_styled`] stores alongside the shaped buffer.
    ///
    /// Byte offsets accumulate over the *non-empty* spans (matching what gets
    /// fed to cosmic-text), so backgrounds/baselines align with glyph
    /// start/end indices reported in `layout_runs`.
    fn build_rich_runs<'a, 's>(
        spans: &'a [RichSpan],
        style: &'s crate::text_render::TextStyle,
        offset_map: &OffsetMap,
        base: &Attrs<'s>,
        size: f32,
        line_height_mult: f32,
    ) -> RichRuns<'a, 's> {
        let mut backgrounds: Vec<(usize, usize, [u8; 3])> = Vec::new();
        let mut baselines: Vec<(usize, usize, BaselineShift)> = Vec::new();
        let mut pictures: Vec<inline_emoji::InlineEmoji> = Vec::new();
        let mut cursor: usize = 0;

        let built = if spans.iter().all(|r| r.text.is_empty()) {
            vec![(std::borrow::Cow::Borrowed(""), base.clone(), None)]
        } else {
            spans
                .iter()
                .filter(|r| !r.text.is_empty())
                .map(|r| {
                    let start = cursor;
                    let end = cursor.saturating_add(r.text.len());
                    cursor = end;
                    if let Some(bg) = r.background {
                        backgrounds.push((start, end, bg));
                    }
                    if r.baseline.is_shifted() {
                        baselines.push((start, end, r.baseline));
                    }
                    let mut a = Self::run_attrs(r, style, size, line_height_mult);
                    let fragment = offset_map.shape_fragment(&r.text, start);
                    if r.workspace_emoji
                        && let Some(name) = tilo_resource::custom_emoji_name(&r.text)
                    {
                        let run_size = r.size.unwrap_or(size);
                        let glyphs = fragment.chars().count();
                        let line_height = run_size * line_height_mult;
                        a = inline_emoji::shaped_attrs(a, run_size, line_height, glyphs);
                        pictures.push(inline_emoji::InlineEmoji {
                            range: start..end,
                            name: name.to_owned(),
                            side: run_size * inline_emoji::SIDE_EM,
                        });
                    }
                    (fragment, a, r.kind)
                })
                .collect()
        };
        (built, backgrounds, baselines, pictures)
    }

    /// The cosmic-text attributes of one span's run in `style`.
    fn run_attrs<'s>(
        r: &RichSpan,
        style: &'s crate::text_render::TextStyle,
        size: f32,
        line_height_mult: f32,
    ) -> Attrs<'s> {
        let run_fam = match r.family {
            Some(family) => family.to_cosmic(),
            None => style.family.to_cosmic(),
        };
        let base_run_size = r.size.unwrap_or(size);
        // Super/subscript runs shape smaller so they advance narrower.
        let run_size = if r.baseline.is_shifted() {
            base_run_size * BaselineShift::SCALE
        } else {
            base_run_size
        };
        let mut a = style
            .cosmic_attrs()
            .family(run_fam)
            .weight(if r.bold {
                Weight::BOLD
            } else {
                style.effective_weight()
            })
            .metrics(Metrics::new(run_size, run_size * line_height_mult));
        if style.letter_spacing != 0.0 {
            a = a.letter_spacing(style.letter_spacing / run_size.max(0.1));
        }
        if r.italic || style.italic {
            a = a.style(Style::Italic);
        }
        if let Some([cr, cg, cb]) = r.color {
            a.color_opt = Some(cosmic_text::Color::rgb(cr, cg, cb));
        }
        a
    }

    /// Total laid-out height (at least one line tall).
    pub fn height(&self) -> f32 {
        self.buffer
            .layout_runs()
            .map(|r| r.line_top + r.line_height)
            .fold(0.0f32, f32::max)
            .max(self.line_height)
    }

    /// Return the number of lines that may be placed in the current fragment
    /// without stranding fewer than the configured orphan/widow counts.
    pub fn fragment_line_count(&self, available_lines: usize) -> usize {
        self.fragment_line_count_from(0, available_lines)
    }

    /// As [`Self::fragment_line_count`], starting at `first_line` for a
    /// paragraph that already spans one or more earlier fragments.
    pub fn fragment_line_count_from(&self, first_line: usize, available_lines: usize) -> usize {
        let total = self.buffer.layout_runs().count();
        let remaining = total.saturating_sub(first_line);
        if remaining <= available_lines {
            return remaining;
        }
        let orphans = usize::from(self.fragmentation.orphans);
        let widows = usize::from(self.fragmentation.widows);
        let take = available_lines.min(remaining.saturating_sub(widows));
        if take < orphans { 0 } else { take }
    }

    /// Widest laid-out visual line.
    pub fn width(&self) -> f32 {
        self.buffer
            .layout_runs()
            .map(|run| {
                let exercised_hyphen = run
                    .glyphs
                    .last()
                    .is_some_and(|glyph| run.text.get(glyph.start..glyph.end) == Some("\u{ad}"));
                run.line_w
                    + if exercised_hyphen {
                        self.discretionary_hyphen_width
                    } else {
                        0.0
                    }
            })
            .fold(0.0f32, f32::max)
    }

    /// Underline and strike through byte ranges of the shaped text; neither
    /// affects shaping.
    #[must_use]
    pub(crate) fn with_decorations(
        mut self,
        underlines: Vec<(usize, usize)>,
        strikes: Vec<(usize, usize)>,
    ) -> Self {
        self.underlines = underlines;
        self.strikes = strikes;
        self
    }

    /// Add an underline decoration in this layout's global byte space. This
    /// is used for IME preedit, where the composition is shaped and painted
    /// but has not yet entered the document model.
    #[allow(
        clippy::suspicious_operation_groupings,
        reason = "not a mirrored typo: range.start < range.end checks well-ordering, range.end <= text_len is the separate bounds check"
    )]
    pub fn add_underline(&mut self, range: std::ops::Range<usize>) {
        if range.start < range.end && range.end <= self.text_len {
            self.underlines.push((range.start, range.end));
        }
    }

    /// Map a local point to a byte offset and its visual boundary affinity.
    pub fn hit_position(&self, x: f32, y: f32) -> TextPosition {
        self.buffer.hit(x, y).map_or_else(
            || TextPosition::new(0),
            |cursor| self.global_position(cursor),
        )
    }

    /// Map a local point to a byte offset (click → caret).
    ///
    /// Prefer [`Self::hit_position`] in an editor so a mixed-BiDi boundary
    /// keeps the side that was clicked.
    pub fn hit(&self, x: f32, y: f32) -> usize {
        self.hit_position(x, y).byte
    }

    /// Caret geometry for a byte offset and affinity: `(x, top_y, height)`.
    pub fn caret_geometry_at(&self, position: TextPosition) -> (f32, f32, f32) {
        let position = TextPosition {
            byte: position.byte.min(self.text_len),
            affinity: position.affinity,
        };
        self.resolved_caret_geometry(position, false)
            .unwrap_or((0.0, 0.0, self.line_height))
    }

    /// Observer caret geometry with the same per-run work limits as
    /// [`Self::selection_rects_bounded`]. Missing stops and invalid positions
    /// are unavailable, never the ordinary editor's origin fallback.
    pub fn caret_geometry_bounded(&self, byte: usize) -> Option<(f32, f32, f32)> {
        if byte > self.text_len {
            return None;
        }
        self.resolved_caret_geometry(TextPosition::new(byte), true)
    }

    fn resolved_caret_geometry(
        &self,
        position: TextPosition,
        bounded: bool,
    ) -> Option<(f32, f32, f32)> {
        let mut byte_fallback = None;
        for run in self.rows_of_lines(self.lines_holding_stops(position.byte)) {
            // A missing affinity may require a byte-only fallback. Do not
            // construct and sort the grapheme stops of a row while looking
            // for an exact match that cannot exist there.
            if !self.row_may_hold_stop(&run, position.byte) {
                continue;
            }
            if bounded && !geometry_run_within_budget(&run) {
                return None;
            }
            for (x, stop) in self.caret_stops(&run) {
                let geometry = (x, run.line_top, run.line_height);
                if stop == position {
                    return Some(geometry);
                }
                if stop.byte == position.byte && byte_fallback.is_none() {
                    byte_fallback = Some(geometry);
                }
            }
        }
        byte_fallback
    }

    /// Resolve a set of carets from one traversal of the shaped visual rows.
    /// Exact affinity wins over the first byte-only fallback, as in
    /// `caret_geometry_at`. Geometry is local to this call and never survives
    /// an edit, reshape, font change or owner replacement.
    pub(crate) fn caret_geometries(&self, positions: &[TextPosition]) -> Vec<(f32, f32, f32)> {
        use std::collections::HashMap;
        let mut exact = HashMap::new();
        let mut fallback = HashMap::new();
        for run in self.buffer.layout_runs() {
            for (x, stop) in self.caret_stops(&run) {
                let geometry = (x, run.line_top, run.line_height);
                exact.entry(stop).or_insert(geometry);
                fallback.entry(stop.byte).or_insert(geometry);
            }
        }
        positions
            .iter()
            .map(|position| {
                let position = TextPosition {
                    byte: position.byte.min(self.text_len),
                    affinity: position.affinity,
                };
                exact
                    .get(&position)
                    .or_else(|| fallback.get(&position.byte))
                    .copied()
                    .unwrap_or((0.0, 0.0, self.line_height))
            })
            .collect()
    }

    /// Caret geometry for a conventional byte-only position.
    pub fn caret_geometry(&self, byte: usize) -> (f32, f32, f32) {
        self.caret_geometry_at(TextPosition::new(byte))
    }

    /// Logical byte positions reached by Home and End for the visual line
    /// containing `byte`. Paragraph direction is respected: Home is the
    /// right edge for an RTL line and the left edge for an LTR line.
    pub fn visual_line_edges(&self, byte: usize) -> (usize, usize) {
        let cursor = self.cursor_at(byte);
        let mut fallback = None;
        for run in self.buffer.layout_runs() {
            if run.line_i != cursor.line || run.cursor_position(&cursor).is_none() {
                continue;
            }
            let Some((first, last)) = run.glyphs.first().zip(run.glyphs.last()) else {
                let at = self.global_byte(Cursor::new(run.line_i, 0));
                return (at, at);
            };
            let left = run.cursor_from_glyph_left(first);
            let right = run.cursor_from_glyph_right(last);
            let edges = if run.rtl {
                (self.global_byte(right), self.global_byte(left))
            } else {
                (self.global_byte(left), self.global_byte(right))
            };
            if run.glyphs.iter().any(|glyph| glyph.start == cursor.index) {
                return edges;
            }
            fallback = Some(edges);
        }
        fallback.unwrap_or_else(|| (byte.min(self.text_len), byte.min(self.text_len)))
    }

    /// Zero-based visual row containing `byte`, including soft-wrapped rows.
    pub fn visual_line_index(&self, byte: usize) -> usize {
        let cursor = self.cursor_at(byte);
        let mut fallback = 0;
        for (visual, run) in self.buffer.layout_runs().enumerate() {
            if run.line_i != cursor.line || run.cursor_position(&cursor).is_none() {
                continue;
            }
            if run.glyphs.iter().any(|glyph| glyph.start == cursor.index) {
                return visual;
            }
            fallback = visual;
        }
        fallback
    }

    /// Vertical bands for every soft-wrapped visual row belonging to the
    /// logical line that contains `byte`. Useful for editor chrome such as a
    /// current-line background without rebuilding a second line model.
    pub fn logical_line_bands(&self, byte: usize) -> Vec<(f32, f32)> {
        let line = self.cursor_at(byte).line;
        self.rows_of_lines(line..line.saturating_add(1))
            .map(|run| (run.line_top, run.line_height))
            .collect()
    }

    /// Zero-based logical (hard-broken) line containing `byte`.
    pub fn logical_line_index(&self, byte: usize) -> usize {
        self.cursor_at(byte).line
    }

    /// The first visual row for each logical line as
    /// `(zero_based_line, top, height)`. Line-number gutters consume this so
    /// their baselines come from the authoritative shaped paragraph too.
    pub fn logical_line_origins(&self) -> Vec<(usize, f32, f32)> {
        let mut origins = Vec::new();
        for run in self.buffer.layout_runs() {
            if origins
                .last()
                .is_none_or(|(line, _, _)| *line != run.line_i)
            {
                origins.push((run.line_i, run.line_top, run.line_height));
            }
        }
        origins
    }

    /// [`Self::logical_line_origins`] of the lines whose first row meets
    /// `top..=bottom`, in the layout's coordinates: a gutter numbers only
    /// the lines it shows.
    pub fn logical_line_origins_within(&self, top: f32, bottom: f32) -> Vec<(usize, f32, f32)> {
        let mut origins = Vec::new();
        let mut last_line = None;
        for run in self.rows_within(top, bottom) {
            if run.line_top > bottom {
                break;
            }
            if last_line == Some(run.line_i) {
                continue;
            }
            last_line = Some(run.line_i);
            if run.line_top + run.line_height >= top {
                origins.push((run.line_i, run.line_top, run.line_height));
            }
        }
        origins
    }

    /// Move one shaped caret stop toward the visual left or right. Stops are
    /// derived from grapheme boundaries inside each glyph cluster, so this
    /// respects ligatures, combining sequences, and `BiDi` reordering. At
    /// the edge of a row the caret moves on to the row it reads into: the
    /// next row's start, or the previous row's end, in the paragraph's
    /// direction, so an arrow key crosses a soft wrap or a line break.
    pub fn visual_motion_position(&self, position: TextPosition, right: bool) -> TextPosition {
        let position = TextPosition {
            byte: position.byte.min(self.text_len),
            affinity: position.affinity,
        };
        // Every stop of a byte lies in the rows of its own paragraph, as in
        // `resolved_caret_geometry`: building and sorting the stops of every
        // row above it made one arrow key cost the whole document.
        let mut rows = Vec::new();
        let mut found = None;
        for run in self.rows_of_lines(self.lines_holding_stops(position.byte)) {
            if !self.row_may_hold_stop(&run, position.byte) {
                continue;
            }
            let row = (run.line_i, run.line_top, run.rtl);
            let stops = self.caret_stops(&run);
            if let Some(index) = stops.iter().position(|(_, stop)| *stop == position) {
                found = Some((row, stops, index));
                break;
            }
            rows.push((row, stops));
        }

        // Byte-only callers conventionally enter with Before affinity. At a
        // boundary where only the other side exists, choose that stop and
        // continue in the requested visual direction.
        let found = found.or_else(|| {
            rows.into_iter().find_map(|(row, stops)| {
                let index = stops
                    .iter()
                    .position(|(_, stop)| stop.byte == position.byte)?;
                Some((row, stops, index))
            })
        });
        let Some(((line, top, rtl), stops, index)) = found else {
            return position;
        };
        let target = Self::visual_step(&stops, index, right);
        let (Some((from, _)), Some((to, stop))) = (stops.get(index), stops.get(target)) else {
            return position;
        };
        if (to - from).abs() > 0.5 {
            return *stop;
        }
        // The row's edge: no stop on it lies further this way.
        self.adjacent_row_stop(line, top, right != rtl, right)
            .filter(|next| *next != position)
            .unwrap_or(*stop)
    }

    /// The stop a caret enters on the row after (`forward`) or before the
    /// row of line `line` at `top`: its left edge when moving right, its
    /// right edge when moving left. `None` at either end of the text.
    fn adjacent_row_stop(
        &self,
        line: usize,
        top: f32,
        forward: bool,
        right: bool,
    ) -> Option<TextPosition> {
        let is_current =
            |run: &cosmic_text::LayoutRun<'_>| run.line_i == line && run.line_top >= top;
        let run = if forward {
            self.rows_of_lines(line..usize::MAX)
                .find(|run| run.line_i > line || run.line_top > top)
        } else {
            self.rows_of_lines(line.saturating_sub(1)..line.saturating_add(1))
                .take_while(|run| !is_current(run))
                .last()
        }?;
        let stops = self.caret_stops(&run);
        let entry = if right { stops.first() } else { stops.last() };
        entry.map(|(_, stop)| *stop)
    }

    /// The stop one visual step from `index`: the next one at another place
    /// on the line. The end of one glyph and the start of the next are two
    /// stops at the same place (after "H", before "i"); stepping onto the
    /// other would leave the caret where it was.
    fn visual_step(stops: &[(f32, TextPosition)], index: usize, right: bool) -> usize {
        let Some((x, _)) = stops.get(index) else {
            return index;
        };
        let mut target = index;
        loop {
            let next = if right {
                target.checked_add(1).filter(|next| *next < stops.len())
            } else {
                target.checked_sub(1)
            };
            let Some(next) = next else {
                return target;
            };
            target = next;
            if stops
                .get(target)
                .is_some_and(|(at, _)| (at - x).abs() > 0.5)
            {
                return target;
            }
        }
    }

    /// Byte-only compatibility wrapper for visual caret motion.
    pub fn visual_motion(&self, byte: usize, right: bool) -> usize {
        self.visual_motion_position(TextPosition::new(byte), right)
            .byte
    }

    /// The shaped lines whose rows can hold a caret stop at source byte
    /// `byte`: its own paragraph's line and, where a huge paragraph was
    /// shaped in chunks, the chunk ending there. A chunk boundary is a soft
    /// wrap whose two sides are two shaped lines; the end of the earlier one
    /// maps to the same source byte as the start of the later one, and the
    /// caret must find it there as it finds the end of a wrapped row.
    fn lines_holding_stops(&self, byte: usize) -> std::ops::Range<usize> {
        let line = self.cursor_at(byte).line;
        let end = line.saturating_add(1);
        match line.checked_sub(1) {
            Some(previous)
                if self.global_byte(Cursor::new(
                    previous,
                    self.line_lens.get(previous).copied().unwrap_or(0),
                )) == byte =>
            {
                previous..end
            }
            _ => line..end,
        }
    }

    /// Whether [`Self::caret_stops`] of `run` can hold a stop at source
    /// byte `byte`, without building them. Every stop maps a shaped byte
    /// inside one of the row's glyphs through the same monotone source
    /// mapping, keeping both ends inclusive for bidi and soft-wrap affinity
    /// boundaries; a row with no stop of its own stands at its line's start.
    fn row_may_hold_stop(&self, run: &cosmic_text::LayoutRun<'_>, byte: usize) -> bool {
        if self.global_byte(Cursor::new(run.line_i, 0)) == byte {
            return true;
        }
        let (mut low, mut high) = (usize::MAX, 0);
        for glyph in run.glyphs {
            low = low.min(glyph.start);
            high = high.max(glyph.end);
        }
        low != usize::MAX
            && self.glyph_global_byte(run.line_i, low) <= byte
            && byte <= self.glyph_global_byte(run.line_i, high)
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "visual/segments are grapheme counts within one glyph cluster, always far below f32's 24-bit exact-integer range"
    )]
    fn caret_stops(&self, run: &cosmic_text::LayoutRun<'_>) -> Vec<(f32, TextPosition)> {
        #[cfg(test)]
        tests::CARET_STOP_RUNS.with(|count| count.set(count.get() + 1));
        // Every caret query and arrow key builds the stops of its row, so a
        // plain row (no virtual text, stops already in visual order) skips
        // the lookups and the sort that only other rows need.
        let mut stops = Vec::with_capacity(run.glyphs.len().saturating_mul(2).saturating_add(1));
        let line_start = self.line_starts.get(run.line_i).copied().unwrap_or(0);
        let virtual_text = !self.offset_map.is_identity();
        // `glyph_global_byte` of this row, read once when no virtual text
        // shifts the source bytes.
        let row_base = self
            .line_starts
            .get(run.line_i)
            .copied()
            .unwrap_or(self.text_len);
        let hanging = self.hanging_punctuation != crate::text_render::HangingPunctuation::default();
        for glyph in run.glyphs {
            // A wrapped row's hanging indent is not text: no caret stands in it.
            if virtual_text
                && self
                    .offset_map
                    .is_virtual(line_start.saturating_add(glyph.start), WRAP_INDENT)
            {
                continue;
            }
            let cluster = run.text.get(glyph.start..glyph.end).unwrap_or("");
            let boundaries = caret_cluster_boundaries(cluster);
            let segments = boundaries.len().saturating_sub(1).max(1);
            let x = if hanging {
                crate::text_render::hanging_glyph_x(run, glyph, self.hanging_punctuation)
            } else {
                glyph.x
            };
            for visual in 0..=segments {
                let logical = if glyph.level.is_rtl() {
                    boundaries
                        .get(segments.saturating_sub(visual))
                        .copied()
                        .unwrap_or(cluster.len())
                } else {
                    boundaries.get(visual).copied().unwrap_or(cluster.len())
                };
                let shaped = glyph.start.saturating_add(logical);
                let affinity = if shaped == glyph.end {
                    TextAffinity::Before
                } else {
                    TextAffinity::After
                };
                stops.push((
                    x + glyph.w * visual as f32 / segments as f32,
                    TextPosition::with_affinity(
                        if virtual_text {
                            self.glyph_global_byte(run.line_i, shaped)
                        } else {
                            row_base.saturating_add(shaped).min(self.text_len)
                        },
                        affinity,
                    ),
                ));
            }
        }
        if stops.is_empty() {
            stops.push((
                0.0,
                TextPosition::with_affinity(
                    self.global_byte(Cursor::new(run.line_i, 0)),
                    TextAffinity::Before,
                ),
            ));
        }
        // The sort is stable, so stops already in order stay as they are.
        if !stops.is_sorted_by(|a, b| a.0.total_cmp(&b.0).is_le()) {
            stops.compact_sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        stops.dedup_by(|a, b| (a.0 - b.0).abs() < 0.01 && a.1 == b.1);
        stops
    }

    /// The shaped rows of the logical lines `lines`, as
    /// [`Buffer::layout_runs`] places them.
    pub(crate) fn rows_of_lines(
        &self,
        lines: std::ops::Range<usize>,
    ) -> impl Iterator<Item = cosmic_text::LayoutRun<'_>> {
        self.buffer
            .layout_runs()
            .skip_to_line(lines.start)
            .take_while(move |run| run.line_i < lines.end)
    }

    /// The shaped rows whose band, widened by twice its height for ink that
    /// overhangs it, meets `top..=bottom` (in the layout's coordinates), and
    /// any between them.
    pub fn rows_within(
        &self,
        top: f32,
        bottom: f32,
    ) -> impl Iterator<Item = cosmic_text::LayoutRun<'_>> {
        // No row of a line above the first can reach within three rows of
        // `top`: each ends where the next line starts, at most the tallest
        // row above `top - 3 × tallest`.
        self.rows_from(top, 3.0)
            .skip_while(move |run| run.line_top + run.line_height * 3.0 < top)
            .take_while(move |run| run.line_top - run.line_height * 2.0 <= bottom)
    }

    /// The shaped rows whose band, widened by how far any row's ink reaches
    /// past its own (`reach`), meets `top..bottom` (in the layout's
    /// coordinates), and any between them: every row whose ink may show
    /// there, for a caller that checks each row's own ink.
    pub fn rows_reaching(
        &self,
        top: f32,
        bottom: f32,
        reach: InkReach,
    ) -> impl Iterator<Item = cosmic_text::LayoutRun<'_>> {
        // No row of a line above the first ends below `top - reach.below`.
        self.rows_from(top - reach.below, 1.0)
            .skip_while(move |run| run.line_top + run.line_height + reach.below <= top)
            .take_while(move |run| run.line_top - reach.above < bottom)
    }

    /// The rows from the start of the line `rows` tallest rows above `y`.
    /// The bisection reads rows from the top of an unscrolled buffer of
    /// unbounded height, as layouts here are shaped; any other walks.
    fn rows_from(&self, y: f32, rows: f32) -> Rows<'_> {
        if self.buffer.scroll() == cosmic_text::Scroll::default() && self.buffer.size().1.is_none()
        {
            let tops = self.row_tops.get_or_init(|| RowTops::of(&self.buffer));
            let start = tops
                .starts
                .partition_point(|line_top| *line_top <= y - rows * tops.tallest)
                .saturating_sub(1);
            Rows::From(RowsFrom {
                lines: &self.buffer.lines,
                line_i: start,
                layout_i: 0,
                line_top: tops.starts.get(start).copied().unwrap_or(0.0),
                line_height: self.buffer.metrics().line_height,
            })
        } else {
            Rows::Walk(cosmic_text::LayoutRunIter::new(&self.buffer))
        }
    }

    fn cursor_at(&self, byte: usize) -> Cursor {
        let byte = self.offset_map.source_to_shaped(byte.min(self.text_len));
        let line = match self.line_starts.binary_search(&byte) {
            Ok(line) => line,
            Err(next) => next.saturating_sub(1),
        }
        .min(self.line_starts.len().saturating_sub(1));
        let line_start = self.line_starts.get(line).copied().unwrap_or(0);
        let line_len = self.line_lens.get(line).copied().unwrap_or(0);
        let index = byte.saturating_sub(line_start).min(line_len);
        Cursor::new(line, index)
    }

    fn global_byte(&self, cursor: Cursor) -> usize {
        let line = cursor.line.min(self.line_starts.len().saturating_sub(1));
        let line_start = self.line_starts.get(line).copied().unwrap_or(0);
        let line_len = self.line_lens.get(line).copied().unwrap_or(0);
        self.offset_map
            .shaped_to_source(line_start.saturating_add(cursor.index.min(line_len)))
            .min(self.text_len)
    }

    fn global_position(&self, cursor: Cursor) -> TextPosition {
        let affinity = match cursor.affinity {
            Affinity::Before => TextAffinity::Before,
            Affinity::After => TextAffinity::After,
        };
        TextPosition::with_affinity(self.global_byte(cursor), affinity)
    }

    pub(crate) fn glyph_global_byte(&self, line: usize, byte: usize) -> usize {
        let shaped = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or_else(|| self.offset_map.source_to_shaped(self.text_len))
            .saturating_add(byte);
        self.offset_map.shaped_to_source(shaped).min(self.text_len)
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
        let cursor_start = self.cursor_at(start);
        let cursor_end = self.cursor_at(end);
        for run in self.rows_of_lines(cursor_start.line..cursor_end.line.saturating_add(1)) {
            // A wrapped paragraph is one logical line of many visual rows, and
            // `highlight` segments every grapheme of a row it is given. A row
            // whose glyphs all end before the range or start after it selects
            // nothing (as in `selection_rects_many_within`): skip it.
            let (mut low, mut high) = (usize::MAX, 0);
            for glyph in run.glyphs {
                low = low.min(glyph.start);
                high = high.max(glyph.end);
            }
            if low == usize::MAX
                || (run.line_i == cursor_start.line && high <= cursor_start.index)
                || (run.line_i == cursor_end.line && low >= cursor_end.index)
            {
                continue;
            }
            for (x, width) in run.highlight(cursor_start, cursor_end) {
                rects.push((x, run.line_top, x + width, run.line_top + run.line_height));
            }
        }
        rects
    }

    /// Selection rectangles in a vertical layout band, refusing overflow
    /// instead of returning a partial selection. Caller byte validation is
    /// unchanged. This bounded observer path also refuses a visible run with
    /// more than 65,536 glyphs or summed glyph-source bytes: the shaper's
    /// `highlight` iterator internally collects its per-run spans first.
    /// Ordinary editing continues to use the unrestricted APIs below/above.
    pub fn selection_rects_bounded(
        &self,
        range: std::ops::Range<usize>,
        band: std::ops::Range<f32>,
        maximum: usize,
    ) -> Option<Vec<(f32, f32, f32, f32)>> {
        if range.start > range.end
            || band.start.is_nan()
            || band.end.is_nan()
            || band.start > band.end
        {
            return None;
        }
        let mut rects = Vec::new();
        if range.is_empty() {
            return Some(rects);
        }
        let start = self.cursor_at(range.start);
        let end = self.cursor_at(range.end);
        for run in self.buffer.layout_runs() {
            if run.line_i < start.line
                || run.line_i > end.line
                || run.line_top + run.line_height < band.start
                || run.line_top > band.end
            {
                continue;
            }
            if !geometry_run_within_budget(&run) {
                return None;
            }
            for (x, width) in run.highlight(start, end) {
                if rects.len() == maximum {
                    return None;
                }
                rects.push((x, run.line_top, x + width, run.line_top + run.line_height));
            }
        }
        Some(rects)
    }

    /// [`Self::selection_rects`] for several byte ranges in one pass over the
    /// layout runs, in input order. A long paragraph is one logical line with
    /// many visual runs; the per-range form walks every run and every glyph
    /// of it once per range, so decorating every span of a pasted page
    /// (spell-check squiggles, find matches) was quadratic in the paragraph
    /// length. Here each run looks up only the ranges that can touch its byte
    /// extent (sorted starts plus a running maximum of ends), which yields
    /// exactly the same rectangles.
    pub fn selection_rects_many(
        &self,
        ranges: &[(usize, usize)],
    ) -> Vec<Vec<(f32, f32, f32, f32)>> {
        self.selection_rects_many_within(ranges, f32::NEG_INFINITY, f32::INFINITY)
    }

    /// [`Self::selection_rects_many`] restricted to visual lines that overlap
    /// the vertical band `[top, bottom]` (layout coordinates). Rectangles of
    /// lines entirely outside the band are omitted; a caller passing its clip
    /// paints exactly what the unrestricted form would have shown.
    pub fn selection_rects_many_within(
        &self,
        ranges: &[(usize, usize)],
        top: f32,
        bottom: f32,
    ) -> Vec<Vec<(f32, f32, f32, f32)>> {
        let mut out = vec![Vec::new(); ranges.len()];
        // (global shaped start, global shaped end, start cursor, end cursor, input index)
        let mut index: Vec<(usize, usize, Cursor, Cursor, usize)> = ranges
            .iter()
            .enumerate()
            .filter(|(_, (start, end))| start < end)
            .map(|(i, &(start, end))| {
                let (cursor_start, cursor_end) = (self.cursor_at(start), self.cursor_at(end));
                (
                    self.shaped_global(cursor_start),
                    self.shaped_global(cursor_end),
                    cursor_start,
                    cursor_end,
                    i,
                )
            })
            .collect();
        if index.is_empty() {
            return out;
        }
        index.compact_sort_by_key(|entry| entry.0);
        let mut running_end = 0;
        let running: Vec<usize> = index
            .iter()
            .map(|entry| {
                running_end = running_end.max(entry.1);
                running_end
            })
            .collect();
        for run in self.buffer.layout_runs() {
            if run.line_top + run.line_height < top || run.line_top > bottom {
                continue;
            }
            let (mut low, mut high) = (usize::MAX, 0);
            for glyph in run.glyphs {
                low = low.min(glyph.start);
                high = high.max(glyph.end);
            }
            if low == usize::MAX {
                continue;
            }
            let line_start = self.line_starts.get(run.line_i).copied().unwrap_or(0);
            let (global_low, global_high) = (line_start + low, line_start + high);
            // Every range touching this run ends after `global_low`; ranges are
            // sorted by start, and the running maximum of ends is monotonic.
            let first = running.partition_point(|&end| end <= global_low);
            for &(global_start, global_end, cursor_start, cursor_end, i) in &index[first..] {
                if global_start >= global_high {
                    break;
                }
                if global_end <= global_low
                    || !(cursor_start.line..=cursor_end.line).contains(&run.line_i)
                {
                    continue;
                }
                // `highlight` selects a grapheme only when it ends after the
                // start cursor and begins before the end cursor on their lines.
                if (run.line_i == cursor_start.line && high <= cursor_start.index)
                    || (run.line_i == cursor_end.line && low >= cursor_end.index)
                {
                    continue;
                }
                for (x, width) in run.highlight(cursor_start, cursor_end) {
                    out[i].push((x, run.line_top, x + width, run.line_top + run.line_height));
                }
            }
        }
        out
    }

    /// Source byte range `[start, end)` covered by the visual lines that
    /// overlap the vertical band `[top, bottom]`, or `None` when no line does.
    /// Callers with many decorated spans use it to skip spans that cannot
    /// reach the band before asking for their geometry.
    pub fn source_range_within(&self, top: f32, bottom: f32) -> Option<(usize, usize)> {
        let mut range: Option<(usize, usize)> = None;
        for run in self.buffer.layout_runs() {
            if run.line_top + run.line_height < top || run.line_top > bottom {
                continue;
            }
            for glyph in run.glyphs {
                let start = self.glyph_global_byte(run.line_i, glyph.start);
                let end = self.glyph_global_byte(run.line_i, glyph.end);
                range = Some(range.map_or((start, end), |(a, b)| (a.min(start), b.max(end))));
            }
        }
        range
    }

    /// Position of a cursor in the paragraph-global shaped byte space (the
    /// space of `line_starts` and of cosmic-text glyph offsets within a line).
    fn shaped_global(&self, cursor: Cursor) -> usize {
        self.line_starts.get(cursor.line).copied().unwrap_or(0) + cursor.index
    }

    fn hangs_wrapped_rows(style: &crate::text_render::TextStyle, max_width: Option<f32>) -> bool {
        style.wrap_mode == crate::text_render::TextWrapMode::Indented && max_width.is_some()
    }

    /// The virtual indent each wrapped row of this layout hangs under: one
    /// [`WRAP_INDENT`] per column of its line's leading whitespace (a tab to
    /// the next multiple of the buffer's tab width), at the row's start.
    fn wrap_indent_insertions(&self, source: &str) -> Vec<crate::text_break::VirtualInsertion> {
        let tab = usize::from(self.buffer.tab_width()).max(1);
        let room = self.buffer.size().0.unwrap_or(f32::INFINITY);
        let mut extra = Vec::new();
        let mut previous_line = None;
        for run in self.buffer.layout_runs() {
            let continuation = previous_line == Some(run.line_i);
            previous_line = Some(run.line_i);
            if !continuation {
                continue;
            }
            let Some(start) = run.glyphs.iter().map(|glyph| glyph.start).min() else {
                continue;
            };
            let at = self.glyph_global_byte(run.line_i, start);
            let line_start = source
                .get(..at)
                .and_then(|head| head.rfind('\n'))
                .map_or(0, |newline| newline + 1);
            let mut columns = 0usize;
            let mut indent_end = line_start;
            for ch in source.get(line_start..).unwrap_or("").chars() {
                match ch {
                    ' ' => columns += 1,
                    '\t' => columns += tab - columns % tab,
                    _ => break,
                }
                indent_end += ch.len_utf8();
            }
            // A row that starts inside the indentation itself keeps the edge,
            // and so does a line indented past half the measure.
            let indent_w = run
                .glyphs
                .iter()
                .find(|glyph| run.text.get(glyph.start..glyph.end) == Some(" "))
                .map_or(self.line_height / 2.0, |glyph| glyph.w);
            #[allow(
                clippy::cast_precision_loss,
                reason = "a column count of one line's indentation"
            )]
            let too_deep = columns as f32 * indent_w > room / 2.0;
            if at <= indent_end || columns == 0 || too_deep {
                continue;
            }
            extra.extend(std::iter::repeat_n(
                crate::text_break::VirtualInsertion {
                    source: at,
                    ch: WRAP_INDENT,
                },
                columns,
            ));
        }
        extra
    }
}

/// What a styled build adds to its spans: whether colour emoji come from the
/// browser's font, and the virtual insertions of a wrapped row's indent.
#[derive(Clone, Copy)]
struct StyledPrep<'a> {
    browser_emoji: bool,
    extra: &'a [crate::text_break::VirtualInsertion],
}

/// The virtual character a wrapped row's hanging indent is set in: a no-break
/// space, a space's width that binds to the text after it, so the row breaks
/// before it, not after.
const WRAP_INDENT: char = '\u{a0}';

/// How many times an [`crate::text_render::TextWrapMode::Indented`] layout is
/// built again while its rows still move; indenting a row can only push text
/// onto later rows, which settles in a pass or two.
const WRAP_INDENT_PASSES: usize = 3;

#[cfg(test)]
#[path = "richtext/retained_layout_tests.rs"]
mod retained_layout_tests;

fn geometry_run_within_budget(run: &cosmic_text::LayoutRun<'_>) -> bool {
    const MAX_RUN_WORK: usize = 65_536;
    if run.glyphs.len() > MAX_RUN_WORK {
        return false;
    }
    let mut bytes = 0usize;
    for glyph in run.glyphs {
        let Some(total) = glyph
            .end
            .checked_sub(glyph.start)
            .and_then(|len| bytes.checked_add(len))
        else {
            return false;
        };
        bytes = total;
        if bytes > MAX_RUN_WORK {
            return false;
        }
    }
    true
}

/// One-byte ASCII clusters contain one grapheme. Keep the full segmenter for
/// ligatures, CRLF and every non-ASCII cluster.
fn caret_cluster_boundaries(cluster: &str) -> std::borrow::Cow<'_, [usize]> {
    if cluster.len() == 1 && cluster.is_ascii() {
        return std::borrow::Cow::Borrowed(&[0, 1]);
    }
    let mut boundaries: Vec<_> = crate::text_break::grapheme_indices(cluster)
        .map(|(at, _)| at)
        .collect();
    boundaries.push(cluster.len());
    std::borrow::Cow::Owned(boundaries)
}

#[cfg(test)]
mod tests;
