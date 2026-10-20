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
    discretionary_hyphen_width: f32,
    line_height: f32,
    /// Block-level paint axes. The renderer replaces `wght` with each batch's
    /// shaped glyph weight so an explicit block axis cannot override span bold.
    pub(crate) font_variations: Vec<crate::text_render::FontVariation>,
    pub(crate) hanging_punctuation: crate::text_render::HangingPunctuation,
    fragmentation: crate::text_render::Fragmentation,
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
            buffer,
            line_starts,
            line_lens,
            offset_map,
            text_len: text.len(),
            underlines: Vec::new(),
            strikes: Vec::new(),
            backgrounds: Vec::new(),
            baselines: Vec::new(),
            discretionary_hyphen_width,
            line_height,
            font_variations: style.resolved_variations(),
            hanging_punctuation: style.hanging_punctuation,
            fragmentation: style.fragmentation,
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

        let (built, backgrounds, baselines) =
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
            buffer,
            line_starts,
            line_lens,
            offset_map,
            text_len: full_text.len(),
            underlines: Vec::new(),
            strikes: Vec::new(),
            backgrounds,
            baselines,
            discretionary_hyphen_width,
            line_height,
            font_variations: style.resolved_variations(),
            hanging_punctuation: style.hanging_punctuation,
            fragmentation: style.fragmentation,
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
                    let a = Self::run_attrs(r, style, size, line_height_mult);
                    (offset_map.shape_fragment(&r.text, start), a, r.kind)
                })
                .collect()
        };
        (built, backgrounds, baselines)
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
        for run in self.buffer.layout_runs() {
            // A missing affinity may require a byte-only fallback. Do not
            // construct and sort every paragraph's grapheme stops while
            // looking for an exact match that cannot exist there. Use the
            // same monotone source mapping as caret_stops, keeping both ends
            // inclusive for bidi and soft-wrap affinity boundaries.
            let contains_byte = if run.glyphs.is_empty() {
                self.global_byte(Cursor::new(run.line_i, 0)) == position.byte
            } else {
                let first = self.glyph_global_byte(run.line_i, 0);
                let last = self.glyph_global_byte(run.line_i, run.text.len());
                first <= position.byte && position.byte <= last
            };
            if !contains_byte {
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
        self.buffer
            .layout_runs()
            .filter(|run| run.line_i == line)
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

    /// Move one shaped caret stop toward the visual left or right. Stops are
    /// derived from grapheme boundaries inside each glyph cluster, so this
    /// respects ligatures, combining sequences, and `BiDi` reordering.
    pub fn visual_motion_position(&self, position: TextPosition, right: bool) -> TextPosition {
        let position = TextPosition {
            byte: position.byte.min(self.text_len),
            affinity: position.affinity,
        };
        for run in self.buffer.layout_runs() {
            let stops = self.caret_stops(&run);
            let Some(index) = stops.iter().position(|(_, stop)| *stop == position) else {
                continue;
            };
            return stops
                .get(Self::visual_step(&stops, index, right))
                .map_or(position, |(_, stop)| *stop);
        }

        // Byte-only callers conventionally enter with Before affinity. At a
        // boundary where only the other side exists, choose that stop and
        // continue in the requested visual direction.
        for run in self.buffer.layout_runs() {
            let stops = self.caret_stops(&run);
            let Some(index) = stops
                .iter()
                .position(|(_, stop)| stop.byte == position.byte)
            else {
                continue;
            };
            return stops
                .get(Self::visual_step(&stops, index, right))
                .map_or(position, |(_, stop)| *stop);
        }
        position
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

    #[allow(
        clippy::cast_precision_loss,
        reason = "visual/segments are grapheme counts within one glyph cluster, always far below f32's 24-bit exact-integer range"
    )]
    fn caret_stops(&self, run: &cosmic_text::LayoutRun<'_>) -> Vec<(f32, TextPosition)> {
        #[cfg(test)]
        tests::CARET_STOP_RUNS.with(|count| count.set(count.get() + 1));
        let mut stops = Vec::new();
        let line_start = self.line_starts.get(run.line_i).copied().unwrap_or(0);
        for glyph in run.glyphs {
            // A wrapped row's hanging indent is not text: no caret stands in it.
            if self
                .offset_map
                .is_virtual(line_start.saturating_add(glyph.start), WRAP_INDENT)
            {
                continue;
            }
            let cluster = run.text.get(glyph.start..glyph.end).unwrap_or("");
            let boundaries = caret_cluster_boundaries(cluster);
            let segments = boundaries.len().saturating_sub(1).max(1);
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
                    crate::text_render::hanging_glyph_x(run, glyph, self.hanging_punctuation)
                        + glyph.w * visual as f32 / segments as f32,
                    TextPosition::with_affinity(
                        self.glyph_global_byte(run.line_i, shaped),
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
        stops.compact_sort_by(|a, b| a.0.total_cmp(&b.0));
        stops.dedup_by(|a, b| (a.0 - b.0).abs() < 0.01 && a.1 == b.1);
        stops
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
        for run in self.buffer.layout_runs() {
            if run.line_i < cursor_start.line || run.line_i > cursor_end.line {
                continue;
            }
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
mod tests {
    use super::*;

    #[test]
    fn indented_wrap_hangs_continuation_rows_under_the_line_indent() {
        let mut fs = crate::font_catalog::fixture_font_system();
        let style = crate::text_render::TextStyle::new(14.0)
            .with_family(crate::text_render::FontChoice::Monospace)
            .with_wrap_mode(crate::text_render::TextWrapMode::Indented);
        let text = "    let total = first_value + second_value + third_value + fourth_value;\nend";
        let plain = RichTextLayout::shape_plain(
            &mut fs,
            text,
            &crate::text_render::TextStyle::new(14.0)
                .with_family(crate::text_render::FontChoice::Monospace),
            Some(220.0),
        );
        let indented = RichTextLayout::shape_plain(&mut fs, text, &style, Some(220.0));
        let indent_x = indented.caret_geometry(4).0;
        assert!(indent_x > 20.0, "the line's own indent: {indent_x}");
        let rows: Vec<(usize, f32)> = indented
            .buffer
            .layout_runs()
            .filter(|run| run.line_i == 0)
            .skip(1)
            .map(|run| {
                let start = run.glyphs.iter().map(|glyph| glyph.start).min().unwrap();
                let byte = indented.glyph_global_byte(0, start);
                (byte, run.line_top)
            })
            .collect();
        assert!(!rows.is_empty(), "the first line wraps");
        for (byte, top) in rows {
            let (x, caret_top, _) =
                indented.caret_geometry_at(TextPosition::with_affinity(byte, TextAffinity::After));
            assert!(
                (x - indent_x).abs() < 0.5,
                "a wrapped row starts under the indent: {x} vs {indent_x}"
            );
            assert!((caret_top - top).abs() < 0.5);
            assert_eq!(
                indented.hit(1.0, top + 1.0),
                byte,
                "a click in the hanging indent lands at the row's start"
            );
        }
        // The text itself is unchanged: the same bytes, end to end.
        assert_eq!(indented.hit(1.0e4, 1.0e4), text.len());
        assert_eq!(plain.caret_geometry(4).0, indent_x);
    }

    #[test]
    fn huge_paragraph_chunks_keep_source_offsets_and_reuse_shaping() {
        let mut fs = crate::font_catalog::fixture_font_system();
        let style = crate::text_render::TextStyle::new(16.0);
        let text = (0..7000)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.len() > crate::text_break::HUGE_PARAGRAPH_BYTES);
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(&text)],
            &style,
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        assert!(layout.buffer.lines.len() > 3, "shaped in chunks");
        let mut last_y = f32::MIN;
        for byte in (0..=text.len()).step_by(211) {
            let (x, y, h) = layout.caret_geometry(byte);
            assert!(y >= last_y && h > 0.0, "caret order at {byte}");
            last_y = y;
            // Hitting the caret position lands on the same source byte.
            let hit = layout.hit(x + 0.01, y + h / 2.0);
            assert!(hit.abs_diff(byte) <= 12, "hit {hit} for {byte}");
        }
        assert_eq!(
            layout.caret_geometry(text.len()).1,
            last_y.max(layout.caret_geometry(text.len()).1)
        );
        // An edit near the end retains the shaped allocation of the first chunk.
        let retained = layout.buffer.lines[0].layout_opt().unwrap().as_ptr();
        let edited = format!("{text} tail");
        let reused = RichTextLayout::shape_styled_reusing(
            &mut fs,
            &[RichSpan::new(&edited)],
            &style,
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
            Some(layout),
        );
        assert_eq!(
            retained,
            reused.buffer.lines[0].layout_opt().unwrap().as_ptr()
        );
    }

    #[test]
    fn offscreen_visual_lines_are_culled_only_by_vertical_extent() {
        use vello::kurbo::{Affine, Rect};
        let mut fs = crate::font_catalog::fixture_font_system();
        let style = crate::text_render::TextStyle::new(16.0);
        let text = "word ".repeat(200);
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(&text)],
            &style,
            Some(100.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let runs: Vec<_> = layout.buffer.layout_runs().collect();
        assert!(runs.len() > 10);
        let clip = Rect::new(0.0, 0.0, 100.0, 40.0);
        let outside = crate::text_render::rich_run_outside_clip;
        assert!(!outside(&runs[0], Affine::IDENTITY, clip));
        let last = runs.last().unwrap();
        assert!(outside(last, Affine::IDENTITY, clip));
        assert!(!outside(
            last,
            Affine::translate((0.0, -f64::from(last.line_top))),
            clip
        ));
        assert!(
            !outside(last, Affine::rotate(0.1), clip),
            "rotation is never culled"
        );
        assert!(!outside(
            last,
            Affine::IDENTITY,
            Rect::new(0.0, f64::NAN, 1.0, 1.0)
        ));
    }

    #[test]
    fn batched_selection_rects_match_per_range_geometry() {
        let mut fs = crate::font_catalog::fixture_font_system();
        let style = crate::text_render::TextStyle::new(16.0);
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau\nשלום mixed עברית text wraps here too\nemoji 👩‍💻 é end";
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(text)],
            &style,
            Some(120.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let bytes: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        let mut ranges = vec![(5, 5), (text.len(), 0), (0, text.len())];
        for (i, &a) in bytes.iter().enumerate().step_by(3) {
            for &b in bytes.iter().skip(i).step_by(7) {
                ranges.push((a, b));
            }
        }
        let batched = layout.selection_rects_many(&ranges);
        assert_eq!(batched.len(), ranges.len());
        for (range, rects) in ranges.iter().zip(&batched) {
            assert_eq!(
                rects,
                &layout.selection_rects(range.0, range.1),
                "range {range:?}"
            );
        }
        // The band's source range covers every range that has a rectangle in it.
        let (lo, hi) = layout.source_range_within(0.0, 20.0).unwrap();
        for (range, rects) in ranges
            .iter()
            .zip(&layout.selection_rects_many_within(&ranges, 0.0, 20.0))
        {
            if !rects.is_empty() {
                assert!(range.1 > lo && range.0 < hi, "{range:?} outside {lo}..{hi}");
            }
        }
        assert_eq!(layout.source_range_within(1.0e6, 2.0e6), None);
        // A band keeps exactly the unrestricted rectangles of the lines it overlaps.
        let bottom = 20.0;
        let banded = layout.selection_rects_many_within(&ranges, 0.0, bottom);
        for (all, within) in batched.iter().zip(&banded) {
            let expected: Vec<_> = all
                .iter()
                .copied()
                .filter(|r| r.1 <= bottom && r.3 >= 0.0)
                .collect();
            assert_eq!(within, &expected);
        }
        assert!(banded.iter().any(|rects| !rects.is_empty()));
        assert!(
            batched.iter().map(Vec::len).sum::<usize>()
                > banded.iter().map(Vec::len).sum::<usize>()
        );
    }

    #[test]
    fn reused_paragraphs_keep_allocations_and_match_fresh_geometry_through_edits() {
        // Case `reused_paragraphs_keep_allocations_and_match_fresh_geometry`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            let style = crate::text_render::TextStyle::new(16.0);
            let initial = "first paragraph\nunchanged שלום world\nlast line\n";
            let old = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new(initial)],
                &style,
                Some(180.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            let retained = old.buffer.lines[1].layout_opt().unwrap().as_ptr();
            let edited = "xfirst paragraph\nunchanged שלום world\nlast line\n";
            let reused = RichTextLayout::shape_styled_reusing(
                &mut fs,
                &[RichSpan::new(edited)],
                &style,
                Some(180.0),
                TextAlign::Left,
                vec![],
                vec![],
                Some(old),
            );
            assert_eq!(
                retained,
                reused.buffer.lines[1].layout_opt().unwrap().as_ptr(),
                "unchanged paragraph must retain its shaped layout allocation"
            );
            let fresh = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new(edited)],
                &style,
                Some(180.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            assert_eq!(reused.width(), fresh.width());
            assert_eq!(reused.height(), fresh.height());
            for byte in edited.char_indices().map(|(i, _)| i).chain([edited.len()]) {
                assert_eq!(reused.caret_geometry(byte), fresh.caret_geometry(byte));
                assert_eq!(
                    reused.selection_rects(0, byte),
                    fresh.selection_rects(0, byte)
                );
            }
        }

        // Case `reused_paragraphs_match_fresh_after_edit_and_style_transitions`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            let base = crate::text_render::TextStyle::new(16.0);
            let texts = [
                "alpha beta gamma\nשלום mixed text\nemoji 👩‍💻 é\n",
                "new paragraph\nalpha beta gamma\nשלום mixed text\nemoji 👩‍💻 é\n",
                "new paragraph\nalpha beta gamma\nשלום changed text\nemoji 👩‍💻 é\n",
                "alpha beta gamma\nשלום changed text\nemoji 👩‍💻 é",
                "alpha beta gamma\nשלום changed text\nemoji 👩‍💻 é\n\n",
                "",
                "one\r\ntwo\nthree",
                "one\nthree",
            ];
            let mut previous = None;
            for (index, text) in texts.into_iter().cycle().take(32).enumerate() {
                let mut style = base.clone();
                if index >= 8 {
                    style.size = 20.0;
                }
                if index >= 16 {
                    style.letter_spacing = 0.6;
                    style.italic = true;
                }
                if index >= 24 {
                    style.wrap_mode = crate::text_render::TextWrapMode::Balance;
                }
                let width = if index % 3 == 0 { 90.0 } else { 180.0 };
                let align = if index % 5 == 0 {
                    TextAlign::Right
                } else {
                    TextAlign::Left
                };
                let split = text.find('\n').unwrap_or(text.len());
                let spans = [
                    RichSpan::new(&text[..split])
                        .bold(index % 2 == 0)
                        .background(Some([20, 30, 40])),
                    RichSpan::new(&text[split..]),
                ];
                let reused = RichTextLayout::shape_styled_reusing(
                    &mut fs,
                    &spans,
                    &style,
                    Some(width),
                    align,
                    vec![(0, split)],
                    vec![],
                    previous,
                );
                let fresh = RichTextLayout::shape_styled(
                    &mut fs,
                    &spans,
                    &style,
                    Some(width),
                    align,
                    vec![(0, split)],
                    vec![],
                );
                assert_eq!(reused.width(), fresh.width(), "case {index}");
                assert_eq!(reused.height(), fresh.height(), "case {index}");
                assert_eq!(reused.backgrounds, fresh.backgrounds);
                assert_eq!(reused.logical_line_origins(), fresh.logical_line_origins());
                // Compare full glyph layouts, not only overall dimensions.
                for (a, b) in reused.buffer.lines.iter().zip(&fresh.buffer.lines) {
                    assert_eq!(
                        format!("{:?}", a.layout_opt()),
                        format!("{:?}", b.layout_opt())
                    );
                }
                for byte in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
                    for affinity in [TextAffinity::Before, TextAffinity::After] {
                        let position = TextPosition::with_affinity(byte, affinity);
                        let actual = reused.caret_geometry_at(position);
                        assert_eq!(
                            actual,
                            fresh.caret_geometry_at(position),
                            "case {index} byte {byte}"
                        );
                        assert_eq!(
                            reused.hit_position(actual.0, actual.1 + actual.2 / 2.0),
                            fresh.hit_position(actual.0, actual.1 + actual.2 / 2.0)
                        );
                        assert_eq!(
                            reused.visual_motion_position(position, true),
                            fresh.visual_motion_position(position, true)
                        );
                    }
                    assert_eq!(
                        reused.selection_rects(0, byte),
                        fresh.selection_rects(0, byte)
                    );
                }
                previous = Some(reused);
            }
        }
    }

    thread_local! {
        pub(super) static CARET_STOP_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "sweeping integer pixel positions across a fixed 400.0-wide test layout: width().ceil() is always small and non-negative, so the round trip through usize is exact"
    )]
    #[test]
    fn caret_hit_and_selection_geometry_follow_visual_direction() {
        // Case `caret_geometry_does_not_segment_unrelated_paragraphs`.
        {
            let text = "ordinary paragraph\n".repeat(1000);
            let shaped = layout(&text, 300.0);
            CARET_STOP_RUNS.with(|count| count.set(0));
            let geometry = shaped.caret_geometry(0);
            assert_eq!(geometry.0, 0.0);
            let visited = CARET_STOP_RUNS.with(std::cell::Cell::get);
            assert!(
                visited <= 2,
                "caret search segmented {visited} unrelated paragraphs"
            );
        }

        // Case `caret_geometry_matches_full_scan_for_all_byte_affinity_boundaries`.
        {
            for text in [
                "",
                "alpha beta gamma\nnext\n",
                "a\r\nb\rc",
                "שלום mixed العربية\n👩‍💻 é\n",
                "ภาษาไทยทดสอบ\nខ្មែរ",
                "soft\u{ad}hyphen word\n\n",
            ] {
                for width in [30.0, 180.0] {
                    let shaped = layout(text, width);
                    for byte in 0..=text.len() + 1 {
                        for affinity in [TextAffinity::Before, TextAffinity::After] {
                            let position =
                                TextPosition::with_affinity(byte.min(text.len()), affinity);
                            let mut fallback = None;
                            let mut exact = None;
                            'scan: for run in shaped.buffer.layout_runs() {
                                for (x, stop) in shaped.caret_stops(&run) {
                                    let geometry = (x, run.line_top, run.line_height);
                                    if stop == position {
                                        exact = Some(geometry);
                                        break 'scan;
                                    }
                                    if stop.byte == position.byte && fallback.is_none() {
                                        fallback = Some(geometry);
                                    }
                                }
                            }
                            let expected =
                                exact.or(fallback).unwrap_or((0.0, 0.0, shaped.line_height));
                            assert_eq!(
                                shaped
                                    .caret_geometry_at(TextPosition::with_affinity(byte, affinity)),
                                expected,
                                "text {text:?}, width {width}, byte {byte}, affinity {affinity:?}"
                            );
                        }
                    }
                }
            }
        }

        // Case `shapes_caret_hit_and_selection`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            let layout = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new("hello world")],
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(400.0),
                TextAlign::Left,
                vec![],
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

        // Case `multiline_hit_and_caret_use_global_byte_offsets`.
        {
            let text = "first\nשני";
            let second_line = "first\n".len();
            let layout = layout(text, 400.0);
            let (_, first_top, first_h) = layout.caret_geometry(0);
            let (second_x, second_top, second_h) = layout.caret_geometry(second_line);

            assert!(second_top >= first_top + first_h * 0.9);
            assert_eq!(
                layout.hit(second_x + 1.0, second_top + second_h * 0.5),
                second_line,
                "a hit on line two must include the preceding line and newline bytes"
            );
            let rects = layout.selection_rects(0, text.len());
            assert!(rects.len() >= 2, "a cross-line selection paints each line");
        }

        // Case `rtl_caret_geometry_follows_visual_direction`.
        {
            let text = "שלום";
            let layout = layout(text, 400.0);
            let (start_x, top, height) = layout.caret_geometry(0);
            let (end_x, _, _) = layout.caret_geometry(text.len());

            assert!(
                start_x > end_x,
                "the logical start of an RTL paragraph is its visual right edge: start={start_x}, end={end_x}"
            );
            assert_eq!(layout.hit(start_x + 1.0, top + height * 0.5), 0);
            assert_eq!(
                layout.hit((end_x - 1.0).max(0.0), top + height * 0.5),
                text.len()
            );
        }

        // Case `mixed_bidi_selection_can_produce_disjoint_visual_bands`.
        {
            let text = "abc אבג xyz";
            // Logically select the Latin prefix plus the first Hebrew character.
            // Visually that Hebrew character sits at the far side of its RTL run,
            // so an unselected gap separates it from the Latin prefix.
            let selected_end = "abc א".len();
            let layout = layout(text, 400.0);
            let rects = layout.selection_rects(0, selected_end);

            assert_eq!(
                rects.len(),
                2,
                "mixed-direction selection must preserve disjoint visual ranges: {rects:?}"
            );
            assert!(rects.iter().all(|(x0, y0, x1, y1)| x1 > x0 && y1 > y0));
        }

        // Case `mixed_bidi_boundary_preserves_both_visual_affinities`.
        {
            let layout = layout("abc אבג xyz", 400.0);
            let (_, top, height) = layout.caret_geometry(0);
            let y = top + height * 0.5;
            let mut positions = Vec::new();
            for x in 0..=layout.width().ceil() as usize {
                let position = layout.hit_position(x as f32, y);
                if positions.last() != Some(&position) {
                    positions.push(position);
                }
            }
            let pair = positions.iter().find_map(|before| {
                let before_x = layout.caret_geometry_at(*before).0;
                positions
                    .iter()
                    .find(|after| {
                        before.byte == after.byte
                            && before.affinity != after.affinity
                            && (before_x - layout.caret_geometry_at(**after).0).abs() > 1.0
                    })
                    .map(|after| (*before, *after))
            });
            let (a, b) = pair.expect("mixed BiDi exposes two sides of one logical byte boundary");
            let ax = layout.caret_geometry_at(a).0;
            let bx = layout.caret_geometry_at(b).0;
            assert!(
                (ax - bx).abs() > 1.0,
                "the two affinities occupy distinct visual positions: {a:?}={ax}, {b:?}={bx}"
            );

            let (left, right) = if ax < bx { (a, b) } else { (b, a) };
            let mut walked = left;
            for _ in 0..16 {
                if walked == right {
                    break;
                }
                let next = layout.visual_motion_position(walked, true);
                assert_ne!(
                    next, walked,
                    "visual motion must not stall inside a BiDi run"
                );
                walked = next;
            }
            assert_eq!(
                walked, right,
                "walking across the intervening visual run reaches the other side of the same byte"
            );
        }
    }

    fn assert_near(actual: f32, expected: f32, message: &str) {
        assert!(
            (actual - expected).abs() < 0.01,
            "{message}: expected {expected}, got {actual}"
        );
    }

    fn layout(text: &str, width: f32) -> RichTextLayout {
        let mut fs = crate::font_catalog::fixture_font_system();
        RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(text)],
            &RichTextLayout::block_style(false, 18.0, FontFamily::Sans, 1.0),
            Some(width),
            TextAlign::Left,
            vec![],
            vec![],
        )
    }

    #[test]
    fn bounded_selection_geometry_refuses_overflow_without_partial_rectangles() {
        let text = "first line\nsecond line\nthird line";
        let shaped = layout(text, 400.0);
        let expected = shaped.selection_rects(0, text.len());
        assert_eq!(expected.len(), 3, "fixture has three real shaped lines");
        assert_eq!(
            shaped.selection_rects_bounded(0..text.len(), f32::NEG_INFINITY..f32::INFINITY, 3),
            Some(expected.clone())
        );
        assert!(
            shaped
                .selection_rects_bounded(0..text.len(), f32::NEG_INFINITY..f32::INFINITY, 2)
                .is_none()
        );
        assert!(
            shaped
                .selection_rects_bounded(0..text.len(), f32::NEG_INFINITY..f32::INFINITY, 0)
                .is_none()
        );
        let middle = expected[1];
        assert_eq!(
            shaped.selection_rects_bounded(0..text.len(), (middle.1 + 0.1)..(middle.3 - 0.1), 1),
            Some(vec![middle])
        );
        assert_eq!(
            shaped.selection_rects_bounded(0..text.len(), -100.0..-1.0, 0),
            Some(vec![])
        );
        assert!(
            shaped
                .selection_rects_bounded(0..text.len(), f32::NAN..1.0, 3)
                .is_none()
        );
    }

    #[test]
    fn bounded_caret_geometry_matches_real_stops_without_origin_fallback() {
        let shaped = layout("a😀éZ", 400.0);
        for byte in [0, 1, 5, 7, 8] {
            assert_eq!(
                shaped.caret_geometry_bounded(byte),
                Some(shaped.caret_geometry(byte))
            );
        }
        assert!(shaped.caret_geometry_bounded(9).is_none());
        assert!(
            shaped.caret_geometry_bounded(2).is_none(),
            "a byte inside the emoji is not an observed caret stop"
        );
        let empty = layout("", 400.0);
        assert_eq!(
            empty.caret_geometry_bounded(0),
            Some(empty.caret_geometry(0))
        );
    }

    #[test]
    fn bounded_geometry_refuses_oversized_shaped_runs_before_caret_allocation() {
        let text = "a".repeat(65_537);
        let mut fs = crate::font_catalog::fixture_font_system();
        let shaped = RichTextLayout::shape_plain(
            &mut fs,
            &text,
            &crate::text_render::TextStyle::new(18.0),
            None,
        );
        let runs: Vec<_> = shaped.buffer.layout_runs().collect();
        assert_eq!(runs.len(), 1, "fixture is one actual unwrapped run");
        assert_eq!(runs[0].glyphs.len(), text.len());
        CARET_STOP_RUNS.with(|count| count.set(0));
        assert!(shaped.caret_geometry_bounded(0).is_none());
        assert_eq!(CARET_STOP_RUNS.with(std::cell::Cell::get), 0);
        assert!(
            shaped
                .selection_rects_bounded(0..text.len(), 0.0..100.0, 4096)
                .is_none()
        );
        assert_eq!(
            shaped.selection_rects_bounded(0..text.len(), -100.0..-1.0, 0),
            Some(vec![]),
            "offscreen runs do not allocate highlight geometry"
        );
    }

    #[test]
    fn paragraph_wrapping_alignment_and_final_rows_follow_controls() {
        // Case `center_alignment_offsets_text`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            let left = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new("hi")],
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(400.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            let center = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new("hi")],
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(400.0),
                TextAlign::Center,
                vec![],
                vec![],
            );
            let lx = left.caret_geometry(0).0;
            let cx = center.caret_geometry(0).0;
            assert!(
                cx > lx + 50.0,
                "centered text should start further right: left={lx} center={cx}"
            );
        }

        // Case `wrapping_produces_multiple_lines`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            let long = "word ".repeat(60);
            let one = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new("word")],
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(1000.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            let wrapped = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new(long)],
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(120.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            assert!(wrapped.height() > one.height() * 3.0);
        }

        // Case `paragraph_controls_shape_and_fragment_the_authoritative_layout`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            let text = "Good typography makes a paragraph feel even before it is read. Spacing supports the argument without calling attention to itself.";
            let justified = RichTextLayout::shape_plain(
                &mut fs,
                text,
                &crate::TextStyle::new(16.0)
                    .with_align(crate::ParagraphAlign::Justified)
                    .with_fragmentation(2, 2),
                Some(260.0),
            );
            let widths: Vec<_> = justified
                .buffer
                .layout_runs()
                .map(|run| run.line_w)
                .collect();
            assert!(widths.len() >= 3);
            assert!(
                widths[..widths.len() - 1]
                    .iter()
                    .all(|width| (*width - 260.0).abs() < 1.0),
                "every eligible line should fill the justified measure: {widths:?}"
            );
            assert_eq!(justified.fragment_line_count(1), 0);
            assert_eq!(justified.fragment_line_count(2), 2);
            assert_eq!(justified.fragment_line_count(widths.len()), widths.len());
            let widow_safe = widths.len().saturating_sub(2);
            assert_eq!(
                justified.fragment_line_count(widths.len() - 1),
                if widow_safe >= 2 { widow_safe } else { 0 }
            );
            assert_eq!(
                justified.fragment_line_count_from(2, widths.len()),
                widths.len() - 2
            );
        }

        // Case `balanced_wrap_avoids_a_short_final_line_without_adding_lines`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            let text =
                "A balanced heading wraps its thought into lines of comparable visual weight.";
            let ordinary = RichTextLayout::shape_plain(
                &mut fs,
                text,
                &crate::TextStyle::new(21.0),
                Some(520.0),
            );
            let balanced = RichTextLayout::shape_plain(
                &mut fs,
                text,
                &crate::TextStyle::new(21.0).with_wrap_mode(crate::TextWrapMode::Balance),
                Some(520.0),
            );
            let ordinary_widths: Vec<_> = ordinary
                .buffer
                .layout_runs()
                .map(|run| run.line_w)
                .collect();
            let balanced_widths: Vec<_> = balanced
                .buffer
                .layout_runs()
                .map(|run| run.line_w)
                .collect();
            assert_eq!(balanced_widths.len(), ordinary_widths.len());
            let ordinary_range = ordinary_widths.iter().copied().fold(0.0, f32::max)
                - ordinary_widths.iter().copied().fold(f32::MAX, f32::min);
            let balanced_range = balanced_widths.iter().copied().fold(0.0, f32::max)
                - balanced_widths.iter().copied().fold(f32::MAX, f32::min);
            assert!(
                balanced_range < ordinary_range,
                "balanced widths {balanced_widths:?} should improve {ordinary_widths:?}"
            );
        }

        // Case `attributed_terminal_lf_preserves_the_plain_layout_final_row`.
        {
            use crate::text_render::TextStyle;

            for scale in [1.0, 2.0] {
                let style = TextStyle::new(16.0 * scale).with_line_height(1.25);
                let fixtures = [
                    ("x", vec![RichSpan::new("x")], 1usize),
                    ("x\ny", vec![RichSpan::new("x\ny")], 2),
                    ("x\n", vec![RichSpan::new("x\n")], 2),
                    ("\n", vec![RichSpan::new("\n")], 2),
                    ("x\n\n", vec![RichSpan::new("x\n\n")], 3),
                    (
                        "x\n",
                        vec![
                            RichSpan::new("x").bold(true),
                            RichSpan::new("\n").italic(true),
                        ],
                        2,
                    ),
                ];

                for (text, spans, expected_rows) in fixtures {
                    let mut fs = crate::font_catalog::fixture_font_system();
                    let plain = RichTextLayout::shape_plain(&mut fs, text, &style, Some(300.0));
                    let styled = RichTextLayout::shape_styled(
                        &mut fs,
                        &spans,
                        &style,
                        Some(300.0),
                        TextAlign::Left,
                        vec![],
                        vec![],
                    );

                    assert_near(
                        styled.height(),
                        plain.height(),
                        &format!("{text:?} styled and plain heights at {scale}x"),
                    );
                    let plain_eof = plain.caret_geometry(text.len());
                    let styled_eof = styled.caret_geometry(text.len());
                    assert_near(
                        styled_eof.1,
                        plain_eof.1,
                        &format!("{text:?} EOF caret top at {scale}x"),
                    );
                    assert_near(
                        styled_eof.2,
                        plain_eof.2,
                        &format!("{text:?} EOF caret height at {scale}x"),
                    );
                    assert_eq!(
                        styled.visual_line_index(text.len()),
                        expected_rows - 1,
                        "{text:?} EOF visual row at {scale}x"
                    );
                    assert_eq!(
                        styled.logical_line_bands(text.len()).len(),
                        1,
                        "{text:?} EOF logical line has one shaped band at {scale}x"
                    );
                    assert_eq!(
                        styled.hit(0.0, styled.height() + style.line_height_px()),
                        text.len(),
                        "{text:?} hit below content resolves to EOF at {scale}x"
                    );
                }
            }
        }

        // Case `terminal_empty_row_inherits_the_final_run_metrics_and_alignment`.
        {
            use crate::text_render::TextStyle;

            let style = TextStyle::new(16.0).with_line_height(1.25);
            let mut final_run = RichSpan::new("\n").bold(true).italic(true);
            final_run.size = Some(24.0);
            let mut fs = crate::font_catalog::fixture_font_system();
            let layout = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new("x"), final_run],
                &style,
                Some(300.0),
                TextAlign::Center,
                vec![],
                vec![],
            );

            let final_line = layout.buffer.lines.last().expect("terminal empty line");
            let attrs = final_line.attrs_list().defaults();
            assert_eq!(final_line.text(), "");
            assert_eq!(final_line.ending(), cosmic_text::LineEnding::None);
            assert_eq!(final_line.align(), Some(Align::Center));
            assert_eq!(attrs.weight, Weight::BOLD);
            assert_eq!(attrs.style, Style::Italic);
            assert_eq!(
                attrs.metrics_opt,
                Some(Metrics::new(24.0, 30.0).into()),
                "the final empty row keeps the last effective run's metrics"
            );
        }
    }

    #[test]
    fn script_aware_breaks_keep_document_offsets() {
        // Case `offset_map_round_trips_document_boundaries_around_virtual_breaks`.
        {
            let text = "ทุกสองสัปดาห์";
            let map = OffsetMap::for_text_with_hyphenation(text, None);
            assert_eq!(
                map.insertions
                    .iter()
                    .map(|insertion| insertion.source)
                    .collect::<Vec<_>>(),
                vec![9, 18]
            );

            for source in text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
            {
                assert_eq!(map.shaped_to_source(map.source_to_shaped(source)), source);
            }
            for insertion in &map.insertions {
                let source = insertion.source;
                let len = insertion.ch.len_utf8();
                let before = map.source_to_shaped(source) - len;
                assert_eq!(map.shaped_to_source(before), source);
                assert_eq!(map.shaped_to_source(before + 1), source);
                assert_eq!(map.shaped_to_source(before + len), source);
            }
        }

        // Case `attributed_thai_wraps_at_dictionary_boundaries_in_document_byte_space`.
        {
            let text = "ทุกสองสัปดาห์";
            let first = &text[..9];
            let second = &text[9..18];
            let third = &text[18..];
            let width = [first, second, third]
                .into_iter()
                .map(|word| layout(word, 400.0).width())
                .fold(0.0f32, f32::max)
                + 0.5;

            let mut fs = crate::font_catalog::fixture_font_system();
            let layout = RichTextLayout::shape_styled(
                &mut fs,
                &[
                    RichSpan::new(first).background(Some([240, 220, 80])),
                    RichSpan::new(format!("{second}{third}")).bold(true),
                ],
                &RichTextLayout::block_style(false, 18.0, FontFamily::Sans, 1.0),
                Some(width),
                TextAlign::Left,
                vec![(9, 18)],
                vec![],
            );

            assert_eq!(layout.visual_line_index(0), 0);
            assert_eq!(layout.visual_line_index(9), 1);
            assert_eq!(layout.visual_line_index(18), 2);
            assert_eq!(layout.visual_line_index(text.len()), 2);
            assert_eq!(layout.backgrounds[0].0..layout.backgrounds[0].1, 0..9);
            assert_eq!(layout.selection_rects(9, 18).len(), 1);

            for source in [0, 9, 18, text.len()] {
                let (x, top, height) = layout.caret_geometry(source);
                assert_eq!(
                    layout.hit(x, top + height * 0.5),
                    source,
                    "hit testing must never expose a virtual-break byte"
                );
            }
        }

        // Case `plain_thai_layout_uses_the_same_dictionary_wrapping`.
        {
            use crate::text_render::TextStyle;

            let text = "ทุกสองสัปดาห์";
            let style = TextStyle::new(18.0);
            let mut fs = crate::font_catalog::fixture_font_system();
            let probe = RichTextLayout::shape_plain(&mut fs, &text[..9], &style, None);
            let width = probe.width() + 0.5;
            let layout = RichTextLayout::shape_plain(&mut fs, text, &style, Some(width));

            assert_eq!(layout.visual_line_index(0), 0);
            assert_eq!(layout.visual_line_index(9), 1);
            assert_eq!(layout.visual_line_index(18), 2);
            assert_eq!(
                layout.hit(layout.caret_geometry(18).0, layout.caret_geometry(18).1),
                18
            );
        }

        // Case `virtual_dictionary_hints_have_no_visible_advance`.
        {
            use crate::text_render::TextStyle;

            let text = "ทุกสองสัปดาห์";
            let style = TextStyle::new(18.0);
            let mut fs = crate::font_catalog::fixture_font_system();
            let source = RichTextLayout::shape_plain(&mut fs, text, &style, None);
            let hinted = RichTextLayout::shape_plain(&mut fs, text, &style, Some(10_000.0));

            assert!(
                (source.width() - hinted.width()).abs() < 0.01,
                "zero-width break hints must not alter visible text metrics"
            );
        }

        // Case `real_layout_wraps_cjk_but_preserves_nbsp_and_emoji_zwj_clusters`.
        {
            use crate::text_render::TextStyle;

            let style = TextStyle::new(18.0);
            let mut fs = crate::font_catalog::fixture_font_system();

            let cjk = "東京京都";
            let cjk_probe = RichTextLayout::shape_plain(&mut fs, "東京", &style, None);
            let cjk_layout =
                RichTextLayout::shape_plain(&mut fs, cjk, &style, Some(cjk_probe.width() + 0.5));
            assert_eq!(cjk_layout.visual_line_index(0), 0);
            assert!(cjk_layout.visual_line_index("東京".len()) >= 1);

            let nbsp = "10\u{a0}km away";
            let narrow = RichTextLayout::shape_plain(&mut fs, "10", &style, None).width() + 0.5;
            let nbsp_layout = RichTextLayout::shape_plain(&mut fs, nbsp, &style, Some(narrow));
            let protected_end = "10\u{a0}km".len();
            assert_eq!(nbsp_layout.visual_line_index(0), 0);
            assert_eq!(nbsp_layout.visual_line_index(protected_end), 0);
            assert!(nbsp_layout.visual_line_index(protected_end + 1) >= 1);

            let family = "👨‍👩‍👧‍👦";
            let emoji = format!("{family} {family}");
            let emoji_width =
                RichTextLayout::shape_plain(&mut fs, family, &style, None).width() + 0.5;
            let emoji_layout =
                RichTextLayout::shape_plain(&mut fs, &emoji, &style, Some(emoji_width));
            for (byte, _) in family.char_indices() {
                assert_eq!(emoji_layout.visual_line_index(byte), 0);
            }
            assert!(emoji_layout.visual_line_index(family.len() + 1) >= 1);
        }

        // Case `hyphenated_attributed_layout_keeps_document_offsets_and_decorations`.
        {
            use crate::text_render::TextStyle;

            let text = "extensive";
            let style = TextStyle::new(20.0).with_auto_hyphenation("en-US");
            let mut fs = crate::font_catalog::fixture_font_system();
            let probe = RichTextLayout::shape_plain(&mut fs, "exten-", &TextStyle::new(20.0), None);
            let layout = RichTextLayout::shape_styled(
                &mut fs,
                &[
                    RichSpan::new("exten").background(Some([240, 220, 80])),
                    RichSpan::new("sive").bold(true),
                ],
                &style,
                Some(probe.width() + 0.5),
                TextAlign::Left,
                vec![(2, 5)],
                vec![],
            );

            assert!(layout.visual_line_index(text.len()) >= 1);
            assert!(layout.width() <= probe.width() + 0.51);
            assert_eq!(layout.backgrounds[0].0..layout.backgrounds[0].1, 0..5);
            assert!(!layout.selection_rects(2, 5).is_empty());
            for source in 0..=text.len() {
                let (x, top, height) = layout.caret_geometry(source);
                assert_eq!(
                    layout.hit(x, top + height * 0.5),
                    source,
                    "virtual soft-hyphen bytes must never escape into editor state"
                );
            }
        }
    }

    #[test]
    fn run_styles_paint_backgrounds_superscripts_and_line_heights() {
        // Case `background_span_records_a_paintable_band`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            // "AAA" plain, "BBB" highlighted, "CCC" plain.
            let spans = [
                RichSpan::new("AAA"),
                RichSpan::new("BBB").background(Some([255, 230, 0])),
                RichSpan::new("CCC"),
            ];
            let layout = RichTextLayout::shape_styled(
                &mut fs,
                &spans,
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(400.0),
                TextAlign::Left,
                vec![],
                vec![],
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
            let plain = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new("AAA")],
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(400.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            assert!(plain.backgrounds.is_empty());
        }

        // Case `super_run_shapes_smaller_and_offsets_up`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            let base_size = 20.0f32;
            // "x" normal, "2" superscript.
            let spans = [
                RichSpan::new("x"),
                RichSpan::new("2").baseline(BaselineShift::Super),
            ];
            let layout = RichTextLayout::shape_styled(
                &mut fs,
                &spans,
                &RichTextLayout::block_style(false, base_size, FontFamily::Sans, 1.0),
                Some(400.0),
                TextAlign::Left,
                vec![],
                vec![],
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
                for g in run.glyphs {
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

        // Case `line_height_mult_scales_line_advance`.
        {
            let mut fs = crate::font_catalog::fixture_font_system();
            // Force two lines so the gap between line tops reflects the line advance.
            let long = "word ".repeat(40);
            let single = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new(long.clone())],
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(120.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            let doubled = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new(long)],
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 2.0),
                Some(120.0),
                TextAlign::Left,
                vec![],
                vec![],
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
    #[test]
    fn batched_carets_match_existing_geometry_and_visit_each_row_once() {
        let mut fs = crate::font_catalog::fixture_font_system();
        let style = crate::text_render::TextStyle::new(16.0);
        for text in [
            "",
            "ordinary text and ffi ligatures",
            "שלום mixed עברית words and English",
            "emoji 👩‍💻 and é é
second line",
            "  indented wrapped text has spaces and several visual rows",
        ] {
            for width in [None, Some(90.0)] {
                let layout = RichTextLayout::shape_styled(
                    &mut fs,
                    &[RichSpan::new(text)],
                    &style,
                    width,
                    TextAlign::Left,
                    vec![],
                    vec![],
                );
                let positions: Vec<_> = text
                    .char_indices()
                    .map(|(byte, _)| byte)
                    .chain([text.len(), text.len() + 1])
                    .flat_map(|byte| {
                        [
                            TextPosition::with_affinity(byte, TextAffinity::Before),
                            TextPosition::with_affinity(byte, TextAffinity::After),
                        ]
                    })
                    .collect();
                let expected: Vec<_> = positions
                    .iter()
                    .map(|position| layout.caret_geometry_at(*position))
                    .collect();
                CARET_STOP_RUNS.with(|count| count.set(0));
                let actual = layout.caret_geometries(&positions);
                assert_eq!(actual, expected, "{text:?} width={width:?}");
                assert_eq!(
                    CARET_STOP_RUNS.with(std::cell::Cell::get),
                    layout.buffer.layout_runs().count(),
                    "one stop construction per visual row, not per requested caret"
                );
            }
        }
    }
    #[test]
    fn caret_cluster_fast_path_matches_unicode_segmenter() {
        let mut clusters: Vec<String> = (0u8..=127)
            .map(|byte| char::from(byte).to_string())
            .collect();
        clusters.extend(["", "ffi", "\r\n", "é", "é", "👩‍💻", "שלום"].map(str::to_owned));
        for cluster in clusters {
            let mut expected: Vec<_> = crate::text_break::grapheme_indices(&cluster)
                .map(|(at, _)| at)
                .collect();
            expected.push(cluster.len());
            assert_eq!(
                caret_cluster_boundaries(&cluster).as_ref(),
                expected.as_slice(),
                "{cluster:?}"
            );
        }
    }

    /// Skipping the visual rows a range cannot touch returns exactly the
    /// rectangles a scan of every row does, for every range of wrapped,
    /// multi-line, mixed-direction and clustered text.
    #[test]
    fn selection_rects_match_a_scan_of_every_row() {
        let scan = |shaped: &RichTextLayout, start: usize, end: usize| {
            let mut rects = Vec::new();
            if start >= end {
                return rects;
            }
            let (cursor_start, cursor_end) = (shaped.cursor_at(start), shaped.cursor_at(end));
            for run in shaped.buffer.layout_runs() {
                if run.line_i < cursor_start.line || run.line_i > cursor_end.line {
                    continue;
                }
                for (x, width) in run.highlight(cursor_start, cursor_end) {
                    rects.push((x, run.line_top, x + width, run.line_top + run.line_height));
                }
            }
            rects
        };
        for text in [
            "one long paragraph that wraps across several visual rows of the layout",
            "first line\nsecond line wraps once it is long enough\n\nfourth",
            "שלום mixed العربية words wrapping 👩‍💻 é ffi",
        ] {
            for width in [60.0, 140.0, 400.0] {
                let shaped = layout(text, width);
                let boundaries: Vec<usize> = text
                    .char_indices()
                    .map(|(at, _)| at)
                    .chain([text.len()])
                    .collect();
                for &start in &boundaries {
                    for &end in &boundaries {
                        assert_eq!(
                            shaped.selection_rects(start, end),
                            scan(&shaped, start, end),
                            "{text:?} at {width}: {start}..{end}"
                        );
                    }
                }
            }
        }
    }
}
