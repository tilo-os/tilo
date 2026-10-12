//! Plain text laid out paragraph by paragraph, so an edit prepares and shapes
//! only the paragraphs it touched.
//!
//! A multiline field's text is a list of paragraphs, and every step that
//! turns a paragraph into a laid-out line here depends on that paragraph
//! alone: its break hints and hyphenation, its font plan (faces, emoji runs,
//! detected identifiers), its shape and its wrap. A block editor gets the
//! same by giving every block its own layout. An edit is therefore spliced
//! into the previous layout: the paragraphs between the first and the last
//! changed byte are prepared and shaped again, every other `BufferLine` keeps
//! its shape and wrap caches, and the offsets after the edit shift. A splice
//! equals building the new text from scratch by construction, because a
//! fresh build is the same per-paragraph step applied to every paragraph.
//!
//! The one thing this decides differently from planning the whole text at
//! once is font fallback across a line feed: digits and punctuation at a
//! paragraph's edge take their face from that paragraph's own words, never
//! from the neighbouring paragraph's.
//!
//! Text whose paragraphs are not independent takes the whole-text styled
//! path instead (see [`RichTextLayout::paragraphs_apply`]).

use super::{
    Attrs, AttrsList, Buffer, BufferLine, FontSystem, LineEnding, Metrics, OffsetMap,
    RichPreparation, RichSpan, RichTextLayout, Shaping, TextAlign, Wrap,
};
use crate::text_break::{HUGE_PARAGRAPH_BYTES, LineEdit, VirtualInsertion};
use crate::text_render::TextStyle;

#[cfg(test)]
thread_local! {
    static PREPARED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many paragraphs this thread has prepared and handed to cosmic-text
/// for shaping since the last call. Every other line keeps its shape.
#[cfg(test)]
pub(crate) fn take_prepared_paragraphs() -> usize {
    PREPARED.with(|count| count.replace(0))
}

/// Everything one paragraph's line is built from that is the same for every
/// paragraph of the text.
struct ParagraphCx<'s> {
    style: &'s TextStyle,
    /// The buffer's default attributes, as the styled path gives them.
    base: Attrs<'s>,
    /// The attributes of an unstyled run, as the styled path gives them.
    run: Attrs<'s>,
    metrics: Metrics,
    align: TextAlign,
    browser_emoji: bool,
}

impl<'s> ParagraphCx<'s> {
    fn new(style: &'s TextStyle, align: TextAlign, browser_emoji: bool) -> Self {
        let size = style.size;
        let base = style.cosmic_attrs().family(style.family.to_cosmic());
        let run = RichTextLayout::run_attrs(&RichSpan::new(""), style, size, style.line_height);
        Self {
            style,
            base,
            run,
            metrics: Metrics::new(size, style.line_height_px()),
            align,
            browser_emoji,
        }
    }

    /// The laid-out line of one paragraph (`paragraph` holds no line feed),
    /// and its virtual break insertions relative to the paragraph's start.
    /// Lines and attributes are those cosmic-text's rich setter gives the
    /// same paragraph.
    fn line(&self, fs: &FontSystem, paragraph: &str) -> (BufferLine, Vec<VirtualInsertion>) {
        #[cfg(test)]
        PREPARED.with(|count| count.set(count.get().saturating_add(1)));
        let map = self.style.hyphenation.offset_map(paragraph);
        let shaping = map.shape_text(paragraph);
        let runs: Vec<(&str, Attrs<'s>, Option<crate::text_render::TextKind>)> =
            if shaping.is_empty() {
                vec![("", self.run.clone(), None)]
            } else {
                crate::text_render::browser_emoji_runs(&shaping, &self.run, self.browser_emoji)
                    .into_iter()
                    .map(|(text, attrs)| (text, attrs, None))
                    .collect()
            };
        let planned = crate::font_catalog::matched::plan_runs(
            fs.db(),
            runs,
            self.metrics,
            self.style.kind,
            self.style.optical(),
            self.style.distinguish || crate::font_catalog::matched::distinguish_everywhere(),
        );
        let mut attrs_list = AttrsList::new(&self.base);
        let mut at = 0usize;
        for (text, attrs) in &planned {
            let end = at.saturating_add(text.len());
            if at < end {
                if *attrs != attrs_list.defaults() {
                    attrs_list.add_span(at..end, attrs);
                }
            } else if at == 0 && attrs.metrics_opt.is_some() {
                // An empty line takes its run's metrics, as the rich setter
                // gives it, so its height is the run's line height.
                attrs_list = AttrsList::new(attrs);
            }
            at = end;
        }
        let mut line = BufferLine::new(
            shaping.into_owned(),
            LineEnding::default(),
            attrs_list,
            Shaping::Advanced,
        );
        line.set_align(self.align.to_cosmic());
        (line, map.insertions)
    }

    /// The lines of `region` (whole paragraphs separated by line feeds) and
    /// their insertions, offset by `source_start`.
    fn lines(
        &self,
        fs: &FontSystem,
        region: &str,
        source_start: usize,
    ) -> (Vec<BufferLine>, Vec<VirtualInsertion>) {
        let mut lines = Vec::new();
        let mut insertions = Vec::new();
        let mut start = source_start;
        for paragraph in region.split('\n') {
            let (line, local) = self.line(fs, paragraph);
            insertions.extend(local.into_iter().map(|insertion| VirtualInsertion {
                source: insertion.source.saturating_add(start),
                ch: insertion.ch,
            }));
            lines.push(line);
            start = start.saturating_add(paragraph.len()).saturating_add(1);
        }
        (lines, insertions)
    }
}

/// Whether `text`'s paragraphs are cosmic-text's lines and are never split:
/// no paragraph separator but a line feed (cosmic-text also breaks lines at
/// CR, U+001C–U+001E, NEL and U+2029) and no paragraph long enough to be
/// shaped in chunks.
fn independent_paragraphs(text: &str) -> bool {
    let bytes = text.as_bytes();
    let separator = bytes.iter().enumerate().any(|(index, &byte)| match byte {
        b'\r' | 0x1C..=0x1E => true,
        // NEL is C2 85; U+2029 is E2 80 A9.
        0x85 => index > 0 && bytes.get(index - 1) == Some(&0xC2),
        0xA9 => index > 1 && bytes.get(index - 2..index) == Some(&[0xE2, 0x80][..]),
        _ => false,
    });
    !separator
        && text
            .split('\n')
            .all(|paragraph| paragraph.len() <= HUGE_PARAGRAPH_BYTES)
}

/// The width cosmic-text wraps to: the styled path reserves a left-aligned
/// block's discretionary hyphen.
fn layout_width(width: f32, discretionary_hyphen_width: f32, align: TextAlign) -> f32 {
    let reserved = if align == TextAlign::Left {
        discretionary_hyphen_width
    } else {
        0.0
    };
    (width - reserved).max(0.0)
}

impl RichTextLayout {
    /// Lay out plain `text` in `style`, wrapped to `max_width`, paragraph by
    /// paragraph, reusing `previous`.
    ///
    /// When `previous` was laid out this way in the same style, alignment and
    /// emoji font, the edit from its text to `text` is spliced into it: only
    /// the changed paragraphs are prepared and shaped, and only a changed
    /// width wraps the others again. Unchanged inputs return `previous`. `sibling`, a layout of the same
    /// text at another width, lends the changed paragraphs its shapes, so
    /// measuring at several widths shapes an edited paragraph once.
    /// Text whose paragraphs are not independent falls back to
    /// [`Self::shape_styled_reusing_browser`] over one unstyled span.
    pub(crate) fn shape_paragraphs_reusing_browser(
        fs: &mut FontSystem,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        align: TextAlign,
        previous: Option<Self>,
        browser_emoji: bool,
        sibling: Option<&Self>,
    ) -> Self {
        let width = max_width.filter(|width| width.is_finite());
        let Some(width) = width.filter(|_| Self::paragraphs_apply(style, max_width)) else {
            return Self::shape_styled_reusing_browser(
                fs,
                &[RichSpan::new(text)],
                style,
                max_width,
                align,
                previous,
                browser_emoji,
            );
        };
        let mut previous = previous;
        if let Some(mut retained) = previous.take() {
            if retained.paragraphs_match(style, align, browser_emoji)
                && retained.splice_paragraphs(fs, text, width, sibling)
            {
                return retained;
            }
            previous = Some(retained);
        }
        Self::shape_paragraphs_fresh(fs, text, style, width, align, browser_emoji, &mut previous)
            .unwrap_or_else(|| {
                Self::shape_styled_reusing_browser(
                    fs,
                    &[RichSpan::new(text)],
                    style,
                    max_width,
                    align,
                    previous,
                    browser_emoji,
                )
            })
    }

    /// Whether `style` lays paragraphs out independently: wrapped rows that
    /// hang under an indent and balanced rows are planned over the whole
    /// text, and a custom hyphenator sees the whole text.
    fn paragraphs_apply(style: &TextStyle, max_width: Option<f32>) -> bool {
        !Self::hangs_wrapped_rows(style, max_width)
            && style.wrap_mode != crate::text_render::TextWrapMode::Balance
            && !matches!(
                style.hyphenation,
                crate::text_render::Hyphenation::Custom(_)
            )
    }

    fn paragraphs_match(&self, style: &TextStyle, align: TextAlign, browser_emoji: bool) -> bool {
        self.prepared.as_ref().is_some_and(|prepared| {
            prepared.paragraph_planned
                && prepared.style == *style
                && prepared.align == align
                && prepared.browser_emoji == browser_emoji
                && prepared.spans.len() == 1
        })
    }

    /// Build `text` paragraph by paragraph, moving in whatever shaping of
    /// `previous` still matches (another style or alignment; a colour-only
    /// theme change keeps all of it). `None`, with `previous` untouched, when
    /// its paragraphs are not independent.
    fn shape_paragraphs_fresh(
        fs: &mut FontSystem,
        text: &str,
        style: &TextStyle,
        width: f32,
        align: TextAlign,
        browser_emoji: bool,
        previous: &mut Option<Self>,
    ) -> Option<Self> {
        if !independent_paragraphs(text) {
            return None;
        }
        let cx = ParagraphCx::new(style, align, browser_emoji);
        let (lines, insertions) = cx.lines(fs, text, 0);
        let offset_map = OffsetMap { insertions };
        let discretionary_hyphen_width = if offset_map.has_discretionary_hyphens() {
            crate::text_render::discretionary_hyphen_advance(fs, style)
        } else {
            0.0
        };
        let mut buffer = Buffer::new_empty(cx.metrics);
        buffer.set_size(
            Some(layout_width(width, discretionary_hyphen_width, align)),
            None,
        );
        buffer.set_wrap(Wrap::Word);
        // Mark the lines as freshly set, so moving shaped lines in from a
        // previous layout keeps their caches.
        buffer.set_text("", &cx.base, Shaping::Advanced, None);
        buffer.lines = lines;
        if let Some(previous) = previous.take() {
            Self::reuse_unchanged_paragraphs(&mut buffer, previous.buffer);
        }
        buffer.shape_until_scroll(fs, false);
        let (line_starts, line_lens) = Self::line_map_of(&buffer);
        Some(Self {
            prepared: Some(RichPreparation {
                spans: vec![RichSpan::new(text)],
                style: style.clone(),
                width: Some(width),
                align,
                browser_emoji,
                paragraph_planned: true,
            }),
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
            line_height: style.line_height_px(),
            font_variations: style.resolved_variations(),
            hanging_punctuation: style.hanging_punctuation,
            fragmentation: style.fragmentation,
        })
    }

    /// Splice the edit from this layout's text to `text`, wrapped to `width`.
    /// `false`, with this layout unchanged, when the new paragraphs are not
    /// independent.
    fn splice_paragraphs(
        &mut self,
        fs: &mut FontSystem,
        text: &str,
        width: f32,
        sibling: Option<&Self>,
    ) -> bool {
        let Some(prepared) = self.prepared.take() else {
            return false;
        };
        let Some(old) = prepared.spans.first().map(|span| span.text.as_str()) else {
            self.prepared = Some(prepared);
            return false;
        };
        let mut spliced = 0..0;
        if old != text {
            let edit = LineEdit::between(old, text);
            let region = text.get(edit.start..edit.new_end).unwrap_or_default();
            let first = edit.first_line(text);
            let last = first.saturating_add(edit.old_line_breaks(old));
            if !independent_paragraphs(region)
                || text.get(edit.start..edit.new_end).is_none()
                || last >= self.buffer.lines.len()
            {
                self.prepared = Some(prepared);
                return false;
            }
            let copied = sibling
                .filter(|sibling| {
                    sibling.paragraphs_match(
                        &prepared.style,
                        prepared.align,
                        prepared.browser_emoji,
                    ) && sibling.paragraph_text() == Some(text)
                })
                .and_then(|sibling| sibling.copy_lines(&edit, first, region));
            let (lines, insertions) = copied.unwrap_or_else(|| {
                ParagraphCx::new(&prepared.style, prepared.align, prepared.browser_emoji)
                    .lines(fs, region, edit.start)
            });
            spliced = first..first.saturating_add(lines.len());
            self.buffer.lines.splice(first..=last, lines);
            self.offset_map.insertions = splice_insertions(
                &edit,
                std::mem::take(&mut self.offset_map.insertions),
                insertions,
                old.len(),
                text.len(),
            );
            (self.line_starts, self.line_lens) = Self::line_map_of(&self.buffer);
            self.text_len = text.len();
        }
        // Decorations are supplied by the caller, not part of shape identity.
        self.underlines.clear();
        self.strikes.clear();
        self.discretionary_hyphen_width = if self.offset_map.has_discretionary_hyphens() {
            crate::text_render::discretionary_hyphen_advance(fs, &prepared.style)
        } else {
            0.0
        };
        // A changed width (or reserved hyphen) wraps every line again, keeping
        // its shape; spliced lines are shaped and wrapped once.
        self.buffer.set_size(
            Some(layout_width(
                width,
                self.discretionary_hyphen_width,
                prepared.align,
            )),
            None,
        );
        self.buffer.shape_until_scroll(fs, false);
        // Fresh lines are not invalidated ones: cosmic-text shapes them on
        // request, which a clean buffer never makes.
        for line in spliced {
            let _ = self.buffer.line_layout(fs, line);
        }
        let mut prepared = prepared;
        if let Some(span) = prepared.spans.first_mut()
            && span.text != text
        {
            span.text.clear();
            span.text.push_str(text);
        }
        prepared.width = Some(width);
        self.prepared = Some(prepared);
        true
    }

    /// A copy that keeps every line's shape and wrap, for a caller that lays
    /// the same text out at a second width without shaping it twice.
    pub(crate) fn duplicate(&self, fs: &mut FontSystem) -> Self {
        let mut buffer = Buffer::new_empty(self.buffer.metrics());
        let (width, height) = self.buffer.size();
        buffer.set_size(width, height);
        buffer.set_wrap(self.buffer.wrap());
        buffer.set_ellipsize(self.buffer.ellipsize());
        buffer.set_monospace_width(self.buffer.monospace_width());
        buffer.set_tab_width(self.buffer.tab_width());
        buffer.set_hinting(self.buffer.hinting());
        // Freshly set lines: the copied caches stay valid.
        buffer.set_text("", &Attrs::new(), Shaping::Advanced, None);
        buffer.lines.clone_from(&self.buffer.lines);
        // Settle the dirty state now: a pending "lines set" would make
        // cosmic-text ignore the next width change and keep these wraps.
        buffer.shape_until_scroll(fs, false);
        Self {
            prepared: self.prepared.clone(),
            buffer,
            line_starts: self.line_starts.clone(),
            line_lens: self.line_lens.clone(),
            offset_map: self.offset_map.clone(),
            text_len: self.text_len,
            underlines: self.underlines.clone(),
            strikes: self.strikes.clone(),
            backgrounds: self.backgrounds.clone(),
            baselines: self.baselines.clone(),
            discretionary_hyphen_width: self.discretionary_hyphen_width,
            line_height: self.line_height,
            font_variations: self.font_variations.clone(),
            hanging_punctuation: self.hanging_punctuation,
            fragmentation: self.fragmentation,
        }
    }

    /// The text of a paragraph-planned layout.
    fn paragraph_text(&self) -> Option<&str> {
        self.prepared
            .as_ref()
            .filter(|prepared| prepared.paragraph_planned)
            .and_then(|prepared| prepared.spans.first())
            .map(|span| span.text.as_str())
    }

    /// The lines of `region` (starting at line `first`, source byte
    /// `edit.start`) and their insertions, copied with their shapes from this
    /// layout of the same text at another width, to be wrapped again.
    fn copy_lines(
        &self,
        edit: &LineEdit,
        first: usize,
        region: &str,
    ) -> Option<(Vec<BufferLine>, Vec<VirtualInsertion>)> {
        let count = crate::text_break::count_line_feeds(region.as_bytes()).saturating_add(1);
        let mut lines = self
            .buffer
            .lines
            .get(first..first.saturating_add(count))?
            .to_vec();
        for line in &mut lines {
            line.reset_layout();
        }
        let insertions = self
            .offset_map
            .insertions
            .iter()
            .filter(|insertion| (edit.start..=edit.new_end).contains(&insertion.source))
            .copied()
            .collect();
        Some((lines, insertions))
    }

    /// `line_starts`/`line_lens` read off the buffer's lines, which are the
    /// shaping text's paragraphs.
    fn line_map_of(buffer: &Buffer) -> (Vec<usize>, Vec<usize>) {
        let mut line_starts = Vec::with_capacity(buffer.lines.len());
        let mut line_lens = Vec::with_capacity(buffer.lines.len());
        let mut start = 0usize;
        for line in &buffer.lines {
            line_starts.push(start);
            line_lens.push(line.text().len());
            start = start.saturating_add(line.text().len()).saturating_add(1);
        }
        if line_starts.is_empty() {
            line_starts.push(0);
            line_lens.push(0);
        }
        (line_starts, line_lens)
    }
}

/// Insertions after an edit: those of the old paragraphs replaced by
/// `region`'s (already in new source bytes), later ones shifted.
fn splice_insertions(
    edit: &LineEdit,
    previous: Vec<VirtualInsertion>,
    region: Vec<VirtualInsertion>,
    old_len: usize,
    new_len: usize,
) -> Vec<VirtualInsertion> {
    let mut out = Vec::with_capacity(previous.len().saturating_add(region.len()));
    let mut previous = previous.into_iter().peekable();
    while let Some(insertion) = previous.next_if(|insertion| insertion.source < edit.start) {
        out.push(insertion);
    }
    out.extend(region);
    out.extend(
        previous
            .filter(|insertion| insertion.source > edit.old_end)
            .map(|insertion| VirtualInsertion {
                source: insertion
                    .source
                    .saturating_add(new_len)
                    .saturating_sub(old_len),
                ch: insertion.ch,
            }),
    );
    out
}

#[cfg(test)]
#[path = "richtext_paragraphs_tests.rs"]
mod tests;
