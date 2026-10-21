//! Reflowable text layout and pagination for every reader.
//!
//! Given a document's [`Block`]s, a content box and the [`Typesetting`],
//! this lays the text out as one continuous column with real inline styling
//! (bold, italic, mono, small capitals, raised and lowered runs, links) and
//! word-level wrapping composed a paragraph at a time, hyphenated by the
//! document's language when justified. Code keeps its lines and tables
//! their columns, scrolling sideways inside the measure rather than breaking
//! it; a picture reserves the box its size calls for, so nothing moves when
//! it arrives. A paginated reader then slices the column into fixed-height
//! pages at line boundaries (so a line is never cut in half); a scrolling
//! one shows it whole.
//!
//! Layout is measurement-bound, so it runs where a [`TextRenderer`] is at
//! hand and is cached per [`FlowKey`]; it only recomputes when the text,
//! the content width or a typographic setting changes.

use std::sync::Arc;

use unicode_bidi::{BidiInfo, Level};

mod pages;
mod selectable;

pub use selectable::selectable_text;

use super::document::{Baseline, Block, Image, Span, TableRow};
use crate::text_render::{FontChoice, NumericStyle, TextRenderer, TextStyle, TextWeight};
use crate::{
    ComposedLine, ParagraphBox, ParagraphOptions, compose_paragraph, discretionary_hyphen_breaks,
    line_break_opportunities,
};

/// How text is set: what a reader's preferences and the book or article
/// decide between them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Typesetting {
    /// Body text size in logical pixels, before the renderer's text scale.
    pub body_size: f32,
    /// Line spacing: a multiplier on top of the face's own 1.25.
    pub line_spacing: f32,
    /// Justified, hyphenated paragraphs instead of ragged ones.
    pub justify: bool,
    /// The reader's own size as a multiple of its base: at accessibility
    /// sizes lines keep natural glyph metrics and punctuation never hangs.
    pub reading_scale: f32,
}

impl Default for Typesetting {
    fn default() -> Self {
        Self {
            body_size: super::preferences::READING_BODY_PX,
            line_spacing: 1.2,
            justify: false,
            reading_scale: 1.0,
        }
    }
}

/// A positioned, single-style run within a line. `x` is relative to the line's
/// left edge (which is `content_x + indent`). A run here is one whitespace-
/// separated word; `char_start`/`char_len` locate it within the block's
/// canonical text so text selection can map a point to a character offset.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "bold/italic/mono/small caps are independent inline flags a run carries in any \
              combination"
)]
pub struct LineRun {
    pub x: f32,
    /// Vertical offset within the line box (inline math, raised and lowered
    /// runs).
    pub y: f32,
    /// Canonical source text covered by this run.
    pub text: String,
    /// Painted text; differs only when an exercised discretionary break adds
    /// a terminal hyphen that is absent from the source document.
    pub display_text: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub small_caps: bool,
    pub baseline: Baseline,
    /// The run's own size: its block's, smaller when raised or lowered.
    pub size: f32,
    /// Where the run links.
    pub link: Option<Arc<str>>,
    /// The footnote the run refers to.
    pub note: Option<u16>,
    pub math: Option<crate::math::Layout>,
    /// Char offset of this run's first character within the block's canonical
    /// text (see [`LaidBlock::text`]).
    pub char_start: usize,
    /// Number of chars in this run.
    pub char_len: usize,
}

/// One visual line of a block. `y` is absolute within the flow.
#[derive(Clone, Debug)]
pub struct LaidLine {
    pub y: f32,
    /// Microtypographic horizontal glyph scale chosen by the paragraph
    /// compositor. It remains within two percent of natural width.
    pub glyph_scale: f32,
    pub adjustment_ratio: f32,
    pub rtl: bool,
    pub runs: Vec<LineRun>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutKind {
    Heading,
    Paragraph,
    Quote,
    /// A stanza: one line per source line, ragged, wrapped lines hanging.
    Verse {
        quoted: bool,
    },
    List,
    Math,
    Separator,
    /// A quiet line about the document, in the interface face.
    Meta,
    /// Preformatted text in a well; scrolls sideways.
    Code,
    /// A picture with its caption under it.
    Figure,
    /// A table; scrolls sideways when wider than the measure.
    Table,
}

#[derive(Clone, Debug)]
pub struct LaidMath {
    pub layout: crate::math::Layout,
    pub x: f32,
    pub y_offset: f32,
}

/// Where a figure's picture sits.
#[derive(Clone, Debug, PartialEq)]
pub struct LaidFigure {
    /// Left edge relative to the content's left edge: negative when a large
    /// picture bleeds past the measure.
    pub x: f32,
    pub width: f32,
    pub height: f32,
    pub image: Image,
}

/// A table's grid.
#[derive(Clone, Debug, PartialEq)]
pub struct LaidTable {
    /// Each column's left edge (relative to the content's left edge) and
    /// width, its padding included.
    pub columns: Vec<(f32, f32)>,
    /// Each row's top (absolute in the flow), height and whether it is a
    /// heading row.
    pub rows: Vec<(f32, f32, bool)>,
}

/// A laid-out block, ready to paint.
#[derive(Clone, Debug)]
pub struct LaidBlock {
    pub index: usize,
    pub kind: LayoutKind,
    /// Absolute top of the block's first line.
    pub top: f32,
    /// Absolute bottom of the block's last line.
    pub bottom: f32,
    pub size: f32,
    pub line_height: f32,
    /// Left inset for text + decorations, relative to `content_x`.
    pub indent: f32,
    pub lines: Vec<LaidLine>,
    /// Bullet/number glyph for list items.
    pub bullet: Option<String>,
    /// Shared structured-math geometry for a `MathML` block.
    pub math: Option<LaidMath>,
    /// The block's canonical selectable text: its words joined by single
    /// spaces. Char offsets in [`LineRun::char_start`] index into this.
    pub text: String,
    /// Total chars in [`Self::text`].
    pub char_len: usize,
    /// Char offsets of the spaces in [`Self::text`] that stand for a hard
    /// line break (a verse line, a `<br/>`, a line of code), ascending.
    pub hard_breaks: Vec<usize>,
    /// The face this block is set in when it is not the reader's (the
    /// interface face of a line about the document).
    pub family: Option<FontChoice>,
    /// Set in the secondary ink (a line about the document, a caption).
    pub dim: bool,
    /// How wide the block's content runs (code, a table), when it may be
    /// wider than the measure and scrolls sideways; 0 otherwise.
    pub scroll_width: f32,
    pub figure: Option<LaidFigure>,
    pub table: Option<LaidTable>,
}

impl LaidBlock {
    /// Whether the block scrolls sideways inside a measure `width` wide.
    #[must_use]
    pub fn scrolls(&self, width: f32) -> bool {
        self.scroll_width > width + 0.5
    }
}

/// A fully laid-out flow plus its page breaks.
#[derive(Clone, Debug)]
pub struct FlowLayout {
    pub blocks: Vec<LaidBlock>,
    pub total_height: f32,
    /// Absolute y at which each page begins. `pages.len()` == page count.
    pub pages: Vec<f32>,
    pub key: FlowKey,
    /// Whether every block is set. A layout still arriving holds the flow's
    /// first blocks, and only page breaks already decided.
    pub complete: bool,
}

impl FlowLayout {
    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }

    /// The laid blocks that reach into `top..bottom` of the flow.
    pub fn blocks_in(&self, top: f32, bottom: f32) -> impl Iterator<Item = &LaidBlock> {
        // Blocks are laid top to bottom; skip those that end above.
        let first = self.blocks.partition_point(|block| block.bottom < top);
        self.blocks
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take_while(move |block| block.top <= bottom)
    }
}

/// Cache key: layout is invalid if any of these change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlowKey {
    /// Which part of the document (a book's chapter; 0 for an article).
    pub section: usize,
    pub width_q: i32,
    pub height_q: i32,
    pub body_q: i32,
    pub scale_q: i32,
    /// System accessibility text scale. `TextRenderer` applies this to glyph
    /// metrics, so the flow cache and vertical rhythm must invalidate with it.
    pub text_scale_q: i32,
    pub spacing_q: i32,
    pub composition: bool,
    /// Signature of the selected font and of the text, so a change relays out.
    pub font_sig: u64,
}

impl FlowKey {
    pub fn new(
        section: usize,
        content_w: f32,
        content_h: f32,
        typesetting: &Typesetting,
        font_sig: u64,
        text_scale: f32,
    ) -> Self {
        Self {
            section,
            width_q: content_w.round() as i32,
            height_q: content_h.round() as i32,
            body_q: (typesetting.body_size * 100.0).round() as i32,
            scale_q: (typesetting.reading_scale * 100.0).round() as i32,
            text_scale_q: (text_scale * 100.0).round() as i32,
            spacing_q: (typesetting.line_spacing * 100.0).round() as i32,
            composition: typesetting.justify,
            font_sig,
        }
    }
}

/// Per-block style derived from kind + settings.
#[derive(Clone)]
struct BlockStyle {
    kind: LayoutKind,
    size: f32,
    /// Base inline style applied to every run (quotes are italic; mono forces).
    force_italic: bool,
    force_bold: bool,
    indent: f32,
    space_before: f32,
    /// Fixed total height for blocks with no flowed text (separators).
    fixed_height: Option<f32>,
    /// Line height as a multiple of the size; `None` for the typesetting's.
    leading: Option<f32>,
    family: Option<FontChoice>,
    dim: bool,
}

impl BlockStyle {
    fn text(kind: LayoutKind, size: f32, space_before: f32) -> Self {
        Self {
            kind,
            size,
            force_italic: false,
            force_bold: false,
            indent: 0.0,
            space_before,
            fixed_height: None,
            leading: None,
            family: None,
            dim: false,
        }
    }
}

fn style_for(block: &Block, body: f32, text_scale: f32) -> BlockStyle {
    match block {
        Block::Heading { level: 0, .. } => BlockStyle {
            force_bold: true,
            leading: Some(1.18),
            ..BlockStyle::text(LayoutKind::Heading, body * 1.9, body * 0.3 * text_scale)
        },
        Block::Heading { level, .. } => {
            let (mult, sb) = match level {
                1 => (1.62, 1.6),
                2 => (1.32, 1.3),
                _ => (1.14, 1.0),
            };
            BlockStyle {
                force_bold: true,
                ..BlockStyle::text(LayoutKind::Heading, body * mult, body * sb * text_scale)
            }
        }
        Block::Paragraph { .. } => {
            BlockStyle::text(LayoutKind::Paragraph, body, body * 0.75 * text_scale)
        }
        Block::Quote { .. } => BlockStyle {
            force_italic: true,
            indent: 30.0 * text_scale,
            ..BlockStyle::text(LayoutKind::Quote, body * 1.02, body * 0.9 * text_scale)
        },
        Block::Verse { quoted, .. } => BlockStyle {
            indent: if *quoted { 30.0 * text_scale } else { 0.0 },
            ..BlockStyle::text(
                LayoutKind::Verse { quoted: *quoted },
                body,
                body * 0.75 * text_scale,
            )
        },
        Block::ListItem { depth, .. } => BlockStyle {
            indent: (28.0 + 24.0 * f32::from(*depth)) * text_scale,
            ..BlockStyle::text(LayoutKind::List, body, body * 0.4 * text_scale)
        },
        Block::Math { .. } => BlockStyle::text(LayoutKind::Math, body * 1.08, body * text_scale),
        Block::Separator => BlockStyle {
            fixed_height: Some(body * 1.6 * text_scale),
            ..BlockStyle::text(LayoutKind::Separator, body, body * 1.2 * text_scale)
        },
        Block::Meta { .. } => BlockStyle {
            leading: Some(1.35),
            family: Some(FontChoice::SansSerif),
            dim: true,
            ..BlockStyle::text(LayoutKind::Meta, body * 0.8, body * 0.45 * text_scale)
        },
        Block::Code { .. } => BlockStyle {
            family: Some(FontChoice::Monospace),
            ..BlockStyle::text(LayoutKind::Code, body * 0.82, body * 0.9 * text_scale)
        },
        Block::Figure { .. } => BlockStyle {
            dim: true,
            leading: Some(1.4),
            ..BlockStyle::text(LayoutKind::Figure, body * 0.84, body * 1.1 * text_scale)
        },
        Block::Table { .. } => {
            BlockStyle::text(LayoutKind::Table, body * 0.88, body * 0.9 * text_scale)
        }
    }
}

/// A word carrying its inline style.
#[allow(
    clippy::struct_excessive_bools,
    reason = "bold/italic/mono/small caps/space_before/hyphen_break_before/natural_break_before \
              are independent per-word layout facts, not a state machine"
)]
struct Word {
    text: String,
    bold: bool,
    italic: bool,
    mono: bool,
    small_caps: bool,
    baseline: Baseline,
    size: f32,
    link: Option<Arc<str>>,
    note: Option<u16>,
    math: Option<crate::math::Layout>,
    w: f32,
    /// Whether a space separates this word from the previous one. `false` lets
    /// punctuation and clitics hug the preceding word across a style boundary.
    space_before: bool,
    /// Char offset of this word's first char within the block's canonical text.
    char_start: usize,
    char_len: usize,
    source_byte_start: usize,
    /// This fragment continues the preceding source word and may begin a line
    /// only by exercising a discretionary hyphen.
    hyphen_break_before: bool,
    hyphen_width_before: f32,
    natural_break_before: bool,
    stretch_before: f32,
    shrink_before: f32,
    start_protrusion: f32,
    end_protrusion: f32,
    /// This word starts a line (the block's first, or one after a hard
    /// break), indented this many ems.
    line_start: Option<u8>,
}

fn weight(bold: bool) -> TextWeight {
    if bold {
        TextWeight::Bold
    } else {
        TextWeight::Regular
    }
}

/// The style one run of reading text is measured and painted in: its size,
/// weight, slant and face, with the old-style proportional figures running
/// text takes, and small capitals where the source asks for them. Measuring
/// and painting through this one function keeps them from drifting apart.
#[must_use]
pub fn run_style(
    size: f32,
    bold: bool,
    italic: bool,
    small_caps: bool,
    family: &FontChoice,
) -> TextStyle {
    let mut style = TextStyle::new(size)
        .with_weight(weight(bold))
        .with_italic(italic)
        .with_family(family.clone());
    if !matches!(family, FontChoice::Monospace) {
        style = style.with_numeric_style(NumericStyle::ProportionalOldstyle);
    }
    if small_caps {
        style = style.with_small_caps(true);
    }
    style
}

/// Reader font scale from which layout keeps natural glyph metrics inside
/// the measured page box: no glyph expansion and no hanging punctuation.
const ACCESSIBLE_READING_SCALE: f32 = 1.35;

/// Whether punctuation may hang into the margins: book composition at
/// ordinary sizes. At accessibility sizes a hanging mark is a whole glyph
/// width outside the measure on a narrow page.
fn hangs_punctuation(typesetting: &Typesetting) -> bool {
    typesetting.justify && typesetting.reading_scale < ACCESSIBLE_READING_SCALE
}

fn punctuation_protrusion(character: char) -> f32 {
    match character {
        '.' | ',' | '、' | '。' => 0.7,
        ':' | ';' | '!' | '?' => 0.5,
        '”' | '’' | '"' | '\'' | ')' | ']' | '}' | '»' | '›' | '」' | '』' => 0.45,
        '-' | '‐' | '–' | '—' => 0.35,
        _ => 0.0,
    }
}

fn opening_punctuation_protrusion(character: char) -> f32 {
    match character {
        '“' | '‘' | '"' | '\'' | '(' | '[' | '{' | '«' | '‹' | '「' | '『' => 0.45,
        _ => 0.0,
    }
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x2E80..=0x2FFF
            | 0x3000..=0x30FF
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0x20000..=0x323AF
    )
}

fn is_southeast_asian(character: char) -> bool {
    matches!(
        character as u32,
        0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF
    )
}

fn script_stretch_before(text: &str, byte: usize, size: f32) -> f32 {
    let before = text.get(..byte).and_then(|s| s.chars().next_back());
    let after = text.get(byte..).and_then(|s| s.chars().next());
    match (before, after) {
        (Some(left), Some(right)) if is_cjk(left) && is_cjk(right) => size * 0.25,
        (Some(left), Some(right)) if is_southeast_asian(left) || is_southeast_asian(right) => {
            size * 0.08
        }
        _ => 0.0,
    }
}

fn visual_order(words: &[Word], start: usize, end: usize, bidi: &BidiInfo<'_>) -> Vec<usize> {
    let Some(slice) = words.get(start..end) else {
        return Vec::new(); // an invalid range from the composer: nothing to order
    };
    let levels: Vec<Level> = slice
        .iter()
        .map(|word| {
            bidi.levels
                .get(word.source_byte_start)
                .copied()
                .unwrap_or_else(Level::ltr)
        })
        .collect();
    BidiInfo::reorder_visual(&levels)
        .into_iter()
        .map(|logical| start + logical)
        .collect()
}

/// Append the `Word` for a math span to `words`, advancing `canon`/`char_pos`
/// and widening `line_height` to fit the formula.
fn push_math_word(
    built: &mut BuiltWords,
    expr: &crate::math::MathExpr,
    span: &Span,
    text: &mut TextRenderer,
    style: &BlockStyle,
    line_start: Option<u8>,
) {
    let space_before = span.space_before;
    if !built.canonical_text.is_empty() && space_before {
        if span.line_break {
            built.hard_breaks.push(built.char_count);
        }
        built.canonical_text.push(' ');
        built.char_count += 1;
    }
    let source_byte_start = built.canonical_text.len();
    let char_start = built.char_count;
    let char_len = span.text.chars().count();
    built.canonical_text.push_str(&span.text);
    built.char_count += char_len;
    let math_layout = crate::math::layout(&expr.root, text, style.size);
    built.line_height = built
        .line_height
        .max(math_layout.height as f32 + style.size * 0.18);
    built.words.push(Word {
        text: span.text.clone(),
        bold: false,
        italic: false,
        mono: false,
        small_caps: false,
        baseline: Baseline::Normal,
        size: style.size,
        link: span.link.clone(),
        note: span.note,
        w: math_layout.width as f32,
        math: Some(math_layout),
        space_before,
        char_start,
        char_len,
        source_byte_start,
        hyphen_break_before: false,
        hyphen_width_before: 0.0,
        natural_break_before: false,
        stretch_before: 0.0,
        shrink_before: 0.0,
        start_protrusion: 0.0,
        end_protrusion: 0.0,
        line_start,
    });
}

/// Hyphenation/line-break points for one whitespace-separated word, plus its
/// opening-punctuation protrusion fraction. `boundaries` is the sorted,
/// deduped union of `hyphen`/`unicode` (plus an opening-punctuation split and
/// a final `w.len()`) — the byte offsets `push_word_fragments` slices at.
struct WordBreaks {
    hyphen: Vec<usize>,
    unicode: Vec<usize>,
    boundaries: Vec<usize>,
    opening_fraction: f32,
}

fn is_em_dash(character: char) -> bool {
    matches!(character, '—' | '―' | '⸺' | '⸻')
}

/// Byte offsets just after an em dash and any word joiners (U+2060) or
/// zero-width no-break spaces (U+FEFF) that follow it. Producers such as
/// Standard Ebooks glue every em dash to its neighbours so it never begins a
/// line; UAX #14 then forbids any break in `gods⁠—⁠Deliberations`, but a
/// line may still end after the dash, as in print.
fn em_dash_breaks(w: &str) -> Vec<usize> {
    let mut breaks = Vec::new();
    let mut chars = w.char_indices().peekable();
    while let Some((_, character)) = chars.next() {
        if !is_em_dash(character) {
            continue;
        }
        while chars
            .next_if(|&(_, next)| matches!(next, '\u{2060}' | '\u{feff}'))
            .is_some()
        {}
        if let Some(&(byte, next)) = chars.peek()
            && !is_em_dash(next)
        {
            breaks.push(byte);
        }
    }
    breaks
}

fn word_break_boundaries(w: &str, hyphenate: bool, language: &str) -> WordBreaks {
    let hyphen = if hyphenate {
        discretionary_hyphen_breaks(w, language)
    } else {
        Vec::new()
    };
    let mut unicode: Vec<_> = line_break_opportunities(w)
        .map(|(byte, _)| byte)
        .filter(|&byte| byte > 0 && byte < w.len())
        .collect();
    unicode.extend(em_dash_breaks(w).into_iter().filter(|&byte| byte > 0));
    unicode.sort_unstable();
    unicode.dedup();
    let mut boundaries = hyphen.clone();
    boundaries.extend(unicode.iter().copied());
    let opening_fraction = w.chars().next().map_or(0.0, opening_punctuation_protrusion);
    if opening_fraction > 0.0
        && let Some(first_end) = w.char_indices().nth(1).map(|(byte, _)| byte)
    {
        boundaries.push(first_end);
    }
    boundaries.push(w.len());
    boundaries.sort_unstable();
    boundaries.dedup();
    WordBreaks {
        hyphen,
        unicode,
        boundaries,
        opening_fraction,
    }
}

/// Width of one protruding character scaled by `fraction` (0 skips the
/// measurement entirely — protrusion is the exception, not the rule).
fn protrusion_width(
    character: char,
    fraction: f32,
    size: f32,
    flags: WordFlags,
    tr: &mut TextRenderer,
    word_family: &FontChoice,
) -> f32 {
    if fraction == 0.0 {
        return 0.0;
    }
    tr.measure_style(
        &character.to_string(),
        None,
        &run_style(
            size,
            flags.bold,
            flags.italic,
            flags.small_caps,
            word_family,
        ),
    )
    .0 * fraction
}

/// Per-word style flags, bundled so the fragment-building functions below
/// don't take separate `bool` parameters.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent style toggle (space-before, bold, italic, mono, small \
              caps); this struct exists specifically to replace bool arguments"
)]
struct WordFlags {
    space_before: bool,
    bold: bool,
    italic: bool,
    mono: bool,
    small_caps: bool,
}

struct FragmentBuildCtx<'a> {
    words: &'a mut Vec<Word>,
    style: &'a BlockStyle,
    typesetting: &'a Typesetting,
    language: &'a str,
    text: &'a mut TextRenderer,
}

struct SourceWord<'a> {
    text: &'a str,
    char_start: usize,
    source_byte_start: usize,
    flags: WordFlags,
    span: &'a Span,
    family: &'a FontChoice,
    line_start: Option<u8>,
}

#[derive(Clone, Copy)]
struct FragmentSlice {
    byte_start: usize,
    byte_end: usize,
    index: usize,
    chars_before: usize,
}

struct WordBuildInput<'a> {
    spans: &'a [Span],
    style: &'a BlockStyle,
    typesetting: &'a Typesetting,
    language: &'a str,
    base_family: &'a FontChoice,
    base_mono: bool,
    line_height: f32,
}

struct BuiltWords {
    words: Vec<Word>,
    canonical_text: String,
    char_count: usize,
    hard_breaks: Vec<usize>,
    line_height: f32,
}

struct BlockComposition<'a> {
    words: &'a [Word],
    canonical_text: &'a str,
    style: &'a BlockStyle,
    typesetting: &'a Typesetting,
    content_width: f32,
    base_family: &'a FontChoice,
    block_top: f32,
    line_height: f32,
}

struct LineComposition<'a, 'text> {
    words: &'a [Word],
    bidi: &'a BidiInfo<'text>,
    base_family: &'a FontChoice,
    space_width: f32,
    line_height: f32,
    text: &'a mut TextRenderer,
}

#[derive(Clone, Copy)]
struct LinePlacement {
    paragraph_rtl: bool,
    text_width: f32,
    justify: bool,
    block_top: f32,
    /// End of the hard-broken line (word index) this visual line belongs to;
    /// its last visual line is set like a paragraph's last.
    segment_end: usize,
    /// Indentation of this visual line from the block's left edge.
    x_offset: f32,
}

/// Measure and push the single fragment `w[byte_start..byte_end]` as a
/// `Word`. Returns its char count, so the caller can advance `chars_before`.
fn push_one_fragment(
    ctx: &mut FragmentBuildCtx<'_>,
    source: &SourceWord<'_>,
    slice: FragmentSlice,
    breaks: &WordBreaks,
    hyphen_width: f32,
) -> usize {
    let flags = source.flags;
    let size = ctx.style.size * source.span.baseline.size_factor();
    let fragment = source
        .text
        .get(slice.byte_start..slice.byte_end)
        .unwrap_or_default();
    let hyphen_break_before =
        slice.index != 0 && breaks.hyphen.binary_search(&slice.byte_start).is_ok();
    let natural_break_before =
        slice.index != 0 && breaks.unicode.binary_search(&slice.byte_start).is_ok();
    let stretch_before = if natural_break_before && ctx.typesetting.justify {
        script_stretch_before(source.text, slice.byte_start, size)
    } else {
        0.0
    };
    let fragment_chars = fragment.chars().count();
    let fragment_width = ctx
        .text
        .measure_style(
            fragment,
            None,
            &run_style(
                size,
                flags.bold,
                flags.italic,
                flags.small_caps,
                source.family,
            ),
        )
        .0;
    let end_protrusion = if slice.byte_end == source.text.len() {
        fragment.chars().last().map_or(0.0, |character| {
            let fraction = if hangs_punctuation(ctx.typesetting) {
                punctuation_protrusion(character)
            } else {
                0.0
            };
            protrusion_width(character, fraction, size, flags, ctx.text, source.family)
        })
    } else {
        0.0
    };
    let start_protrusion = if hangs_punctuation(ctx.typesetting)
        && slice.index == 0
        && breaks.opening_fraction > 0.0
    {
        fragment.chars().next().map_or(0.0, |character| {
            protrusion_width(
                character,
                breaks.opening_fraction,
                size,
                flags,
                ctx.text,
                source.family,
            )
        })
    } else {
        0.0
    };
    ctx.words.push(Word {
        text: fragment.to_string(),
        bold: flags.bold,
        italic: flags.italic,
        mono: flags.mono,
        small_caps: flags.small_caps,
        baseline: source.span.baseline,
        size,
        link: source.span.link.clone(),
        note: source.span.note,
        math: None,
        w: fragment_width,
        space_before: slice.index == 0 && flags.space_before,
        char_start: source.char_start + slice.chars_before,
        char_len: fragment_chars,
        source_byte_start: source.source_byte_start + slice.byte_start,
        hyphen_break_before,
        hyphen_width_before: if hyphen_break_before {
            hyphen_width
        } else {
            0.0
        },
        natural_break_before,
        stretch_before,
        shrink_before: 0.0,
        start_protrusion,
        end_protrusion,
        line_start: source.line_start.filter(|_| slice.index == 0),
    });
    fragment_chars
}

/// Split one whitespace-separated source word `w` into hyphenation/line-break
/// fragments and push each as a `Word` (measured, with protrusion).
fn push_word_fragments(ctx: &mut FragmentBuildCtx<'_>, source: &SourceWord<'_>) {
    let hyphenate = ctx.typesetting.justify
        && !source.flags.mono
        && !matches!(ctx.style.kind, LayoutKind::Verse { .. });
    let breaks = word_break_boundaries(source.text, hyphenate, ctx.language);
    let flags = source.flags;
    let hyphen_width = ctx
        .text
        .measure_style(
            "-",
            None,
            &run_style(
                ctx.style.size * source.span.baseline.size_factor(),
                flags.bold,
                flags.italic,
                flags.small_caps,
                source.family,
            ),
        )
        .0;
    let mut byte_start = 0;
    let mut chars_before = 0;
    for (fragment_index, &byte_end) in breaks.boundaries.iter().enumerate() {
        let fragment_chars = push_one_fragment(
            ctx,
            source,
            FragmentSlice {
                byte_start,
                byte_end,
                index: fragment_index,
                chars_before,
            },
            &breaks,
            hyphen_width,
        );
        byte_start = byte_end;
        chars_before += fragment_chars;
    }
}

/// Build the measured word list for a text block, plus its canonical
/// selectable text (words joined by single spaces). The first word of each
/// span inherits the span's `space_before`; later words are
/// whitespace-separated by construction. Widens `*line_height` to fit any
/// inline math.
fn build_words(text: &mut TextRenderer, input: &WordBuildInput<'_>) -> BuiltWords {
    let mut built = BuiltWords {
        words: Vec::new(),
        canonical_text: String::new(),
        char_count: 0,
        hard_breaks: Vec::new(),
        line_height: input.line_height,
    };
    for span in input.spans {
        let bold = span.bold || input.style.force_bold;
        let italic = span.italic || input.style.force_italic;
        let mono = span.code || input.base_mono;
        let word_family = if mono {
            &FontChoice::Monospace
        } else {
            input.base_family
        };
        let line_start = (built.words.is_empty() || span.line_break).then_some(span.line_indent);
        if let Some(expr) = &span.math {
            push_math_word(&mut built, expr, span, text, input.style, line_start);
            continue;
        }
        for (wi, w) in span.text.split_whitespace().enumerate() {
            let space_before = if wi == 0 { span.space_before } else { true };
            // A separating space is one canonical char between words.
            if !built.canonical_text.is_empty() && space_before {
                if wi == 0 && span.line_break {
                    built.hard_breaks.push(built.char_count);
                }
                built.canonical_text.push(' ');
                built.char_count += 1;
            }
            let source_byte_start = built.canonical_text.len();
            let char_start = built.char_count;
            let char_len = w.chars().count();
            built.canonical_text.push_str(w);
            built.char_count += char_len;
            push_word_fragments(
                &mut FragmentBuildCtx {
                    words: &mut built.words,
                    style: input.style,
                    typesetting: input.typesetting,
                    language: input.language,
                    text,
                },
                &SourceWord {
                    text: w,
                    char_start,
                    source_byte_start,
                    flags: WordFlags {
                        space_before,
                        bold,
                        italic,
                        mono,
                        small_caps: span.small_caps,
                    },
                    span,
                    family: word_family,
                    line_start: line_start.filter(|_| wi == 0),
                },
            );
        }
    }
    built
}

/// Whether two adjacent words paint as one run.
fn same_run(previous: &LineRun, word: &Word) -> bool {
    previous.bold == word.bold
        && previous.italic == word.italic
        && previous.mono == word.mono
        && previous.small_caps == word.small_caps
        && previous.baseline == word.baseline
        && previous.link == word.link
        && previous.note == word.note
        && previous.math.is_none()
        && word.math.is_none()
}

/// Build the `LineRun`s for one composed line, in visual order, ligating
/// adjacent fragments that rejoin their source word. Returns the runs and the
/// final horizontal cursor position (used by the caller for RTL alignment).
fn line_runs(ctx: &mut LineComposition<'_, '_>, line: &ComposedLine) -> (Vec<LineRun>, f32) {
    let mut runs: Vec<LineRun> = Vec::with_capacity(line.end - line.start);
    let mut x = 0.0;
    let order = visual_order(ctx.words, line.start, line.end, ctx.bidi);
    let mut previous_index = None;
    for (offset, word_index) in order.into_iter().enumerate() {
        let Some(word) = ctx.words.get(word_index) else {
            continue; // an out-of-range order index: skip (should not happen)
        };
        if offset == 0 {
            x -= word.start_protrusion * line.glyph_scale;
        } else if let Some(previous) = previous_index
            && let Some(boundary) = ctx.words.get(word_index.max(previous))
        {
            // Glue belongs to the logical boundary between visually adjacent
            // items. In an RTL run that boundary is carried by the item with
            // the greater logical index.
            if boundary.space_before {
                x += ctx.space_width * line.glue_scale;
            } else if boundary.natural_break_before {
                let adjustment = if line.adjustment_ratio >= 0.0 {
                    boundary.stretch_before * line.adjustment_ratio
                } else {
                    boundary.shrink_before * line.adjustment_ratio
                };
                x += adjustment;
            }
        }
        let rejoins_source_word =
            word.hyphen_break_before || (word.natural_break_before && word.stretch_before == 0.0);
        if rejoins_source_word
            && previous_index.is_some_and(|previous| previous + 1 == word_index)
            && let Some(previous) = runs.last_mut()
            && same_run(previous, word)
            && previous.char_start + previous.char_len == word.char_start
        {
            previous.text.push_str(&word.text);
            previous.display_text.push_str(&word.text);
            previous.char_len += word.char_len;
            let family = if word.mono {
                &FontChoice::Monospace
            } else {
                ctx.base_family
            };
            x = previous.x
                + ctx
                    .text
                    .measure_style(
                        &previous.text,
                        None,
                        &run_style(
                            previous.size,
                            word.bold,
                            word.italic,
                            word.small_caps,
                            family,
                        ),
                    )
                    .0
                    * line.glyph_scale;
            previous_index = Some(word_index);
            continue;
        }
        let y = match &word.math {
            Some(math) => (ctx.line_height - math.height as f32) * 0.5,
            None => word.baseline.shift() * (word.size / word.baseline.size_factor()),
        };
        runs.push(LineRun {
            x,
            y,
            text: word.text.clone(),
            display_text: word.text.clone(),
            bold: word.bold,
            italic: word.italic,
            mono: word.mono,
            small_caps: word.small_caps,
            baseline: word.baseline,
            size: word.size,
            link: word.link.clone(),
            note: word.note,
            math: word.math.clone(),
            char_start: word.char_start,
            char_len: word.char_len,
        });
        x += word.w * line.glyph_scale;
        previous_index = Some(word_index);
    }
    (runs, x)
}

/// Build one visually-ordered `LaidLine` from a composed line's word range: its
/// runs, a trailing discretionary hyphen when the next word continues past
/// this line, and right-alignment for a fully-consumed RTL line.
fn compose_one_line(
    ctx: &mut LineComposition<'_, '_>,
    line: &ComposedLine,
    line_idx: usize,
    placement: LinePlacement,
) -> LaidLine {
    let (mut runs, x) = line_runs(ctx, line);

    if let Some(next_word) = ctx.words.get(line.end)
        && next_word.hyphen_break_before
        && let Some(source) = ctx.words.get(line.end - 1)
        && let Some(last) = runs.iter_mut().find(|run| {
            run.char_start <= source.char_start
                && run.char_start + run.char_len == source.char_start + source.char_len
        })
    {
        last.display_text.push('-');
    }
    let mut offset = placement.x_offset;
    if placement.paragraph_rtl && (line.end == placement.segment_end || !placement.justify) {
        offset += (placement.text_width - x).max(0.0);
    }
    for run in &mut runs {
        run.x += offset;
    }
    LaidLine {
        y: placement.block_top + line_idx as f32 * ctx.line_height,
        glyph_scale: line.glyph_scale,
        adjustment_ratio: line.adjustment_ratio,
        rtl: placement.paragraph_rtl,
        runs,
    }
}

/// The word ranges between hard line breaks: each starts at a word with
/// [`Word::line_start`].
fn hard_lines(words: &[Word]) -> Vec<std::ops::Range<usize>> {
    let starts: Vec<usize> = words
        .iter()
        .enumerate()
        .filter(|(index, word)| *index == 0 || word.line_start.is_some())
        .map(|(index, _)| index)
        .collect();
    let ends = starts.iter().skip(1).copied().chain([words.len()]);
    starts
        .iter()
        .zip(ends)
        .map(|(&start, end)| start..end)
        .collect()
}

/// The compositor's boxes for a block's measured words.
fn paragraph_boxes(words: &[Word], space_width: f32) -> Vec<ParagraphBox> {
    words
        .iter()
        .map(|word| ParagraphBox {
            width: word.w,
            glue_before: if word.space_before { space_width } else { 0.0 },
            stretch_before: word.stretch_before,
            shrink_before: word.shrink_before,
            break_before: word.space_before
                || word.natural_break_before
                || word.hyphen_break_before,
            break_penalty: if word.hyphen_break_before { 50 } else { 0 },
            flagged_break: word.hyphen_break_before,
            break_width: word.hyphen_width_before,
            end_protrusion: word.end_protrusion,
        })
        .collect()
}

/// Compose the complete paragraph rather than accepting the first local break
/// that fits, then realize each chosen line into a `LaidLine`. Body prose and
/// quotations use adjustable inter-word glue; headings and lists retain
/// natural ragged spacing while still benefiting from paragraph-wide break
/// selection.
///
/// Each hard-broken line (after a `<br/>`, or a verse line) is composed on
/// its own, so it ends like a paragraph's last line. Verse is ragged, and
/// a verse line too long for the measure wraps with its continuation lines
/// hanging one em further in.
fn compose_block_lines(
    text: &mut TextRenderer,
    composition: &BlockComposition<'_>,
) -> Vec<LaidLine> {
    let text_width = (composition.content_width - composition.style.indent).max(40.0);
    let space_width = measure_space(text, composition.style.size, composition.base_family);
    let bidi = crate::text_break::bidi_info(composition.canonical_text);
    let paragraph_rtl = bidi
        .paragraphs
        .first()
        .is_some_and(|paragraph| paragraph.level.is_rtl());
    let verse = matches!(composition.style.kind, LayoutKind::Verse { .. });
    let boxes = paragraph_boxes(composition.words, space_width);
    let mut options = ParagraphOptions::new(text_width);
    // Justification spreads a line's slack over its spaces; below about
    // twenty ems a line has too few spaces to take it without rivers, so a
    // narrow measure sets ragged (still hyphenated) whatever the book asks.
    let wide_measure = text_width >= composition.style.size * text.text_scale() * 20.0;
    options.justify = composition.typesetting.justify
        && wide_measure
        && matches!(
            composition.style.kind,
            LayoutKind::Paragraph | LayoutKind::Quote
        );
    // At accessibility sizes, favour predictable natural glyph metrics over
    // the subtle expansion used for book-like justification. On a narrow
    // phone one expanded line can otherwise exceed its measured paint box,
    // leaving the final word visibly clipped instead of reflowing.
    if !options.justify
        || composition.typesetting.reading_scale >= ACCESSIBLE_READING_SCALE
        || verse
    {
        options.max_glyph_expansion = 0.0;
    }
    let em = composition.style.size;
    let hang = if verse { em } else { 0.0 };
    let mut lines: Vec<LaidLine> = Vec::new();
    let mut line_ctx = LineComposition {
        words: composition.words,
        bidi: &bidi,
        base_family: composition.base_family,
        space_width,
        line_height: composition.line_height,
        text,
    };
    for segment in hard_lines(composition.words) {
        let indent = composition
            .words
            .get(segment.start)
            .and_then(|word| word.line_start)
            .map_or(0.0, |ems| f32::from(ems) * em)
            .min(text_width * 0.5);
        let Some(segment_boxes) = boxes.get(segment.clone()) else {
            continue;
        };
        let width = (text_width - indent).max(40.0);
        let mut composed = compose_paragraph(
            segment_boxes,
            ParagraphOptions {
                line_width: width,
                ..options
            },
        );
        if hang > 0.0 && composed.len() > 1 {
            composed = compose_paragraph(
                segment_boxes,
                ParagraphOptions {
                    line_width: (width - hang).max(40.0),
                    ..options
                },
            );
        }
        for (visual, line) in composed.iter().enumerate() {
            let line = ComposedLine {
                start: line.start + segment.start,
                end: line.end + segment.start,
                ..*line
            };
            let hanging = if visual > 0 { hang } else { 0.0 };
            let placement = LinePlacement {
                paragraph_rtl,
                text_width: width - hanging,
                justify: options.justify,
                block_top: composition.block_top,
                segment_end: segment.end,
                // Right-to-left lines align to the right edge, so their
                // indentation comes off the measure instead.
                x_offset: if paragraph_rtl { 0.0 } else { indent + hanging },
            };
            let index = lines.len();
            lines.push(compose_one_line(&mut line_ctx, &line, index, placement));
        }
    }
    if lines.is_empty() {
        lines.push(LaidLine {
            y: composition.block_top,
            glyph_scale: 1.0,
            adjustment_ratio: 0.0,
            rtl: paragraph_rtl,
            runs: Vec::new(),
        });
    }
    lines
}

/// A block with no text of its own, `height` tall at `top`.
fn bare_block(index: usize, kind: LayoutKind, top: f32, height: f32, st: &BlockStyle) -> LaidBlock {
    LaidBlock {
        index,
        kind,
        top,
        bottom: top + height,
        size: st.size,
        line_height: height,
        indent: 0.0,
        lines: vec![LaidLine {
            y: top,
            glyph_scale: 1.0,
            adjustment_ratio: 0.0,
            rtl: false,
            runs: vec![],
        }],
        bullet: None,
        math: None,
        text: String::new(),
        char_len: 0,
        hard_breaks: Vec::new(),
        family: st.family.clone(),
        dim: st.dim,
        scroll_width: 0.0,
        figure: None,
        table: None,
    }
}

/// A laid-out `Block::Math` — the formula centered in the content width, with
/// vertical breathing room scaled off the body size.
struct MathBlockInput<'a> {
    index: usize,
    top: f32,
    expression: &'a crate::math::MathExpr,
    source: &'a str,
    style: &'a BlockStyle,
    body_size: f32,
    content_width: f32,
}

fn laid_math_block(text: &mut TextRenderer, input: &MathBlockInput<'_>) -> LaidBlock {
    let math_layout = crate::math::layout(&input.expression.root, text, input.style.size);
    let vertical_pad = input.body_size * 0.35;
    let height = math_layout.height as f32 + vertical_pad * 2.0;
    let math_x = ((input.content_width - math_layout.width as f32) * 0.5).max(0.0);
    LaidBlock {
        math: Some(LaidMath {
            layout: math_layout,
            x: math_x,
            y_offset: vertical_pad,
        }),
        text: input.source.to_string(),
        char_len: input.source.chars().count(),
        ..bare_block(
            input.index,
            LayoutKind::Math,
            input.top,
            height,
            input.style,
        )
    }
}

/// Lay out one flow into positioned lines + page breaks. `base_family` is
/// the reader's chosen body face; `font_signature` feeds the layout cache
/// key so a font or text change relays out.
pub struct FlowRequest<'a> {
    pub blocks: &'a [Block],
    /// The measure: how wide running text is set.
    pub content_width: f32,
    /// How wide a large picture may bleed, centred on the measure (at least
    /// `content_width`).
    pub bleed_width: f32,
    /// A page's height for a paginated reader; 0 for one that scrolls.
    pub content_height: f32,
    pub typesetting: &'a Typesetting,
    pub section: usize,
    pub language: &'a str,
    pub base_family: &'a FontChoice,
    pub font_signature: u64,
}

/// The marker set before a list item: its number, or a bullet; none for the
/// text an item continues with after a list nested in it.
fn list_marker(block: &Block) -> Option<String> {
    match block {
        Block::ListItem { index: 0, .. } => None,
        Block::ListItem {
            ordered: true,
            index: n,
            ..
        } => Some(format!("{n}.")),
        Block::ListItem {
            ordered: false,
            depth,
            ..
        } => Some(if depth % 2 == 0 { "•" } else { "◦" }.to_string()),
        _ => None,
    }
}

/// Whether `block` belongs to a document's header (its title and the lines
/// about it).
fn is_header(block: &Block) -> bool {
    matches!(block, Block::Heading { level: 0, .. } | Block::Meta { .. })
}

pub fn layout_flow(tr: &mut TextRenderer, request: &FlowRequest<'_>) -> FlowLayout {
    let mut layouter = FlowLayouter::new(request, tr.text_scale());
    let mut blocks = Vec::with_capacity(request.blocks.len());
    while let Some(block) = layouter.next_block(tr) {
        blocks.push(block);
    }
    FlowLayout {
        blocks,
        total_height: layouter.height(),
        pages: layouter.finish_pages(),
        key: layouter.key(),
        complete: true,
    }
}

/// The profiling scope of setting one block ([`FlowLayouter::next_block`]).
pub const LAY_BLOCK_PHASE: &str = "reading.lay_block";

/// Lays a flow out one block at a time, breaking pages as it goes, so a
/// caller can hand over each stretch as soon as it is set (a reader laying a
/// long chapter out off its UI thread). Every page break it has taken is
/// final: [`Self::take_pages`] returns only those.
pub struct FlowLayouter<'a> {
    request: &'a FlowRequest<'a>,
    key: FlowKey,
    text_scale: f32,
    base_mono: bool,
    next: usize,
    y: f32,
    /// Whether the block set last belongs to the document's header.
    after_header: bool,
    paginator: pages::Paginator,
}

impl<'a> FlowLayouter<'a> {
    #[must_use]
    pub fn new(request: &'a FlowRequest<'a>, text_scale: f32) -> Self {
        Self {
            request,
            key: FlowKey::new(
                request.section,
                request.content_width,
                request.content_height,
                request.typesetting,
                request.font_signature,
                text_scale,
            ),
            text_scale,
            base_mono: matches!(request.base_family, FontChoice::Monospace),
            next: 0,
            y: 0.0,
            after_header: false,
            paginator: pages::Paginator::new(request.content_height),
        }
    }

    #[must_use]
    pub fn key(&self) -> FlowKey {
        self.key
    }

    /// The flow's height so far: the bottom of the last block set.
    #[must_use]
    pub fn height(&self) -> f32 {
        self.y
    }

    /// Set the next block, or `None` once the flow is done. The text
    /// renderer must be at the text scale this layouter was made for.
    pub fn next_block(&mut self, tr: &mut TextRenderer) -> Option<LaidBlock> {
        let index = self.next;
        let block = self.request.blocks.get(index)?;
        // Counted per thread: tests hold a UI thread to setting none.
        let _phase = tilo_resource::profiling::Phase::start(LAY_BLOCK_PHASE);
        self.next += 1;
        let laid = self.lay_block(tr, index, block);
        self.y = laid.bottom.max(self.y);
        self.paginator.push(&laid);
        Some(laid)
    }

    /// Page starts decided since the last call.
    pub fn take_pages(&mut self) -> Vec<f32> {
        self.paginator.take_decided()
    }

    /// Every page start not yet taken, once the flow is done; a flow with
    /// nothing to break has the one page at the top.
    pub fn finish_pages(&mut self) -> Vec<f32> {
        let pages = self.paginator.take_decided();
        if self.paginator.handed_none() && pages.is_empty() {
            return vec![0.0];
        }
        pages
    }

    fn lay_block(&mut self, tr: &mut TextRenderer, index: usize, block: &Block) -> LaidBlock {
        let request = self.request;
        let text_scale = self.text_scale;
        let body = request.typesetting.body_size;
        let st = style_for(block, body, text_scale);
        let mut y = self.y;
        if index > 0 || !is_header(block) {
            y += st.space_before;
        }
        // The body keeps a clear distance from the header above it.
        let header = is_header(block);
        if self.after_header && !header {
            y += body * 1.1 * text_scale;
        }
        self.after_header = header;
        match block {
            Block::Math { expr, source, .. } => laid_math_block(
                tr,
                &MathBlockInput {
                    index,
                    top: y,
                    expression: expr,
                    source,
                    style: &st,
                    body_size: body,
                    content_width: request.content_width,
                },
            ),
            Block::Separator => bare_block(
                index,
                LayoutKind::Separator,
                y,
                st.fixed_height.unwrap_or(st.size * 1.6),
                &st,
            ),
            Block::Code { text, .. } => laid_code_block(tr, index, y, text, &st, text_scale),
            Block::Figure { image, caption } => laid_figure_block(
                tr,
                request,
                &FigureInput {
                    index,
                    top: y,
                    image,
                    caption,
                    style: &st,
                    text_scale,
                },
            ),
            Block::Table { rows } => laid_table_block(tr, request, index, y, rows, &st, text_scale),
            _ => laid_text_block(
                tr,
                request,
                index,
                y,
                block,
                &st,
                text_scale,
                self.base_mono,
            ),
        }
    }
}

/// A block of running text: its words composed into lines.
fn laid_text_block(
    tr: &mut TextRenderer,
    request: &FlowRequest<'_>,
    index: usize,
    top: f32,
    block: &Block,
    st: &BlockStyle,
    text_scale: f32,
    base_mono: bool,
) -> LaidBlock {
    let family = st.family.as_ref().unwrap_or(request.base_family);
    let leading = st
        .leading
        .unwrap_or(1.25 * request.typesetting.line_spacing);
    let built = build_words(
        tr,
        &WordBuildInput {
            spans: block.spans(),
            style: st,
            typesetting: request.typesetting,
            language: request.language,
            base_family: family,
            base_mono: base_mono && st.family.is_none(),
            line_height: st.size * leading * text_scale,
        },
    );
    let line_height = built.line_height;
    let lines = compose_block_lines(
        tr,
        &BlockComposition {
            words: &built.words,
            canonical_text: &built.canonical_text,
            style: st,
            typesetting: request.typesetting,
            content_width: request.content_width,
            base_family: family,
            block_top: top,
            line_height,
        },
    );
    let height = lines.len() as f32 * line_height;
    LaidBlock {
        index,
        kind: st.kind,
        top,
        bottom: top + height,
        size: st.size,
        line_height,
        indent: st.indent,
        lines,
        bullet: list_marker(block),
        math: None,
        char_len: built.char_count,
        text: built.canonical_text,
        hard_breaks: built.hard_breaks,
        family: st.family.clone(),
        dim: st.dim,
        scroll_width: 0.0,
        figure: None,
        table: None,
    }
}

/// A `<pre>` block: each source line a line, in the mono face, never wrapped.
fn laid_code_block(
    tr: &mut TextRenderer,
    index: usize,
    top: f32,
    code: &str,
    st: &BlockStyle,
    text_scale: f32,
) -> LaidBlock {
    let size = st.size;
    let pad = size * 0.9;
    let line_height = size * 1.5 * text_scale;
    let style = run_style(size, false, false, false, &FontChoice::Monospace);
    let mut lines = Vec::new();
    let mut canonical = String::new();
    let mut chars = 0;
    let mut hard_breaks = Vec::new();
    let mut widest = 0.0f32;
    for (number, line) in code.split('\n').enumerate() {
        if number > 0 {
            hard_breaks.push(chars);
            canonical.push(' ');
            chars += 1;
        }
        let char_len = line.chars().count();
        let runs = if line.trim().is_empty() {
            Vec::new()
        } else {
            widest = widest.max(tr.measure_style(line, None, &style).0);
            vec![LineRun {
                x: 0.0,
                y: 0.0,
                text: line.to_owned(),
                display_text: line.to_owned(),
                bold: false,
                italic: false,
                mono: true,
                small_caps: false,
                baseline: Baseline::Normal,
                size,
                link: None,
                note: None,
                math: None,
                char_start: chars,
                char_len,
            }]
        };
        lines.push(LaidLine {
            y: top + pad + number as f32 * line_height,
            glyph_scale: 1.0,
            adjustment_ratio: 0.0,
            rtl: false,
            runs,
        });
        canonical.push_str(line);
        chars += char_len;
    }
    let height = pad * 2.0 + lines.len() as f32 * line_height;
    LaidBlock {
        indent: pad,
        line_height,
        lines,
        text: canonical,
        char_len: chars,
        hard_breaks,
        scroll_width: widest + pad * 2.0,
        ..bare_block(index, LayoutKind::Code, top, height, st)
    }
}

/// The aspect a picture of unknown size is given until it loads: the box is
/// kept, and the picture fits inside it.
const UNKNOWN_ASPECT: f32 = 2.0 / 3.0;

/// The box a picture takes in a measure `measure` wide that may bleed to
/// `bleed`: its own size where it fits, the measure where it is wider, the
/// whole bleed where it is much wider. The height follows from its aspect.
#[must_use]
pub fn figure_box(image: &Image, measure: f32, bleed: f32) -> (f32, f32) {
    let bleed = bleed.max(measure);
    let aspect = image.aspect().unwrap_or(UNKNOWN_ASPECT);
    let natural = image.width.map_or(measure, crate::num::f32_of_u32);
    let width = if natural > measure * 1.2 {
        natural.min(bleed)
    } else {
        natural.min(measure)
    }
    .max(1.0);
    (width, width * aspect)
}

struct FigureInput<'a> {
    index: usize,
    top: f32,
    image: &'a Image,
    caption: &'a [Span],
    style: &'a BlockStyle,
    text_scale: f32,
}

fn laid_figure_block(
    tr: &mut TextRenderer,
    request: &FlowRequest<'_>,
    input: &FigureInput<'_>,
) -> LaidBlock {
    let st = input.style;
    let (width, height) = figure_box(input.image, request.content_width, request.bleed_width);
    let figure = LaidFigure {
        x: (request.content_width - width) / 2.0,
        width,
        height,
        image: input.image.clone(),
    };
    let mut block = bare_block(input.index, LayoutKind::Figure, input.top, height, st);
    block.figure = Some(figure);
    if input.caption.is_empty() {
        return block;
    }
    let caption_top = input.top + height + st.size * 0.6 * input.text_scale;
    let leading = st.leading.unwrap_or(1.4);
    let built = build_words(
        tr,
        &WordBuildInput {
            spans: input.caption,
            style: st,
            typesetting: request.typesetting,
            language: request.language,
            base_family: request.base_family,
            base_mono: false,
            line_height: st.size * leading * input.text_scale,
        },
    );
    let line_height = built.line_height;
    let lines = compose_block_lines(
        tr,
        &BlockComposition {
            words: &built.words,
            canonical_text: &built.canonical_text,
            style: st,
            typesetting: &Typesetting {
                justify: false,
                ..*request.typesetting
            },
            content_width: request.content_width,
            base_family: request.base_family,
            block_top: caption_top,
            line_height,
        },
    );
    block.bottom = caption_top + lines.len() as f32 * line_height;
    block.line_height = line_height;
    block.lines = lines;
    block.char_len = built.char_count;
    block.text = built.canonical_text;
    block.hard_breaks = built.hard_breaks;
    block
}

/// A table: columns as wide as their text wants within the measure, cells
/// wrapped inside them; a table whose words cannot fit scrolls sideways.
#[allow(
    clippy::too_many_lines,
    reason = "one pass: measure the columns, size them, then compose every cell"
)]
fn laid_table_block(
    tr: &mut TextRenderer,
    request: &FlowRequest<'_>,
    index: usize,
    top: f32,
    rows: &[TableRow],
    st: &BlockStyle,
    text_scale: f32,
) -> LaidBlock {
    let size = st.size;
    let pad_x = size * 0.6;
    let pad_y = size * 0.4 * text_scale;
    let line_height = size * 1.25 * request.typesetting.line_spacing * text_scale;
    let family = request.base_family;
    let columns = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
    let mut natural = vec![0.0f32; columns];
    let mut least = vec![0.0f32; columns];
    let widest_column = size * 22.0;
    for row in rows {
        for (column, cell) in row.cells.iter().enumerate() {
            let text = super::document::spans_text(&cell.spans);
            let style = run_style(size, cell.header, false, false, family);
            let whole = tr.measure_style(&text, None, &style).0;
            let longest = text
                .split_whitespace()
                .max_by_key(|word| word.chars().count())
                .map_or(0.0, |word| tr.measure_style(word, None, &style).0);
            if let Some(slot) = natural.get_mut(column) {
                *slot = slot.max(whole.min(widest_column));
            }
            if let Some(slot) = least.get_mut(column) {
                *slot = slot.max(longest);
            }
        }
    }
    for (natural, least) in natural.iter_mut().zip(&least) {
        *natural = natural.max(*least);
    }
    let pads = pad_x * 2.0 * columns as f32;
    let available = (request.content_width - pads).max(0.0);
    let sum_natural: f32 = natural.iter().sum();
    let sum_least: f32 = least.iter().sum();
    let widths: Vec<f32> = if sum_natural <= available {
        natural
    } else if sum_least >= available || sum_natural <= sum_least {
        least
    } else {
        let share = (available - sum_least) / (sum_natural - sum_least);
        natural
            .iter()
            .zip(&least)
            .map(|(natural, least)| least + (natural - least) * share)
            .collect()
    };
    let mut column_edges = Vec::with_capacity(columns);
    let mut x = 0.0f32;
    for width in &widths {
        column_edges.push((x, width + pad_x * 2.0));
        x += width + pad_x * 2.0;
    }
    let total_width = x;

    let mut lines = Vec::new();
    let mut canonical = String::new();
    let mut chars = 0usize;
    let mut row_boxes = Vec::with_capacity(rows.len());
    let mut y = top;
    let typesetting = Typesetting {
        justify: false,
        ..*request.typesetting
    };
    for row in rows {
        let mut row_height = line_height;
        let header = row.cells.iter().all(|cell| cell.header);
        for (column, cell) in row.cells.iter().enumerate() {
            let Some(&(cell_x, _)) = column_edges.get(column) else {
                continue;
            };
            let width = widths.get(column).copied().unwrap_or(0.0);
            let cell_style = BlockStyle {
                force_bold: cell.header,
                ..BlockStyle::text(LayoutKind::Table, size, 0.0)
            };
            let built = build_words(
                tr,
                &WordBuildInput {
                    spans: &cell.spans,
                    style: &cell_style,
                    typesetting: &typesetting,
                    language: request.language,
                    base_family: family,
                    base_mono: false,
                    line_height,
                },
            );
            if built.canonical_text.is_empty() {
                continue;
            }
            if !canonical.is_empty() {
                canonical.push(' ');
                chars += 1;
            }
            let offset = chars;
            let mut cell_lines = compose_block_lines(
                tr,
                &BlockComposition {
                    words: &built.words,
                    canonical_text: &built.canonical_text,
                    style: &cell_style,
                    typesetting: &typesetting,
                    content_width: width,
                    base_family: family,
                    block_top: y + pad_y,
                    line_height: built.line_height,
                },
            );
            for line in &mut cell_lines {
                for run in &mut line.runs {
                    run.x += cell_x + pad_x;
                    run.char_start += offset;
                }
            }
            row_height = row_height.max(cell_lines.len() as f32 * built.line_height);
            lines.extend(cell_lines);
            canonical.push_str(&built.canonical_text);
            chars += built.char_count;
        }
        let height = row_height + pad_y * 2.0;
        row_boxes.push((y, height, header));
        y += height;
    }
    LaidBlock {
        line_height,
        lines,
        text: canonical,
        char_len: chars,
        scroll_width: total_width,
        table: Some(LaidTable {
            columns: column_edges,
            rows: row_boxes,
        }),
        ..bare_block(index, LayoutKind::Table, top, y - top, st)
    }
}

/// Slice the flow into page-start offsets at line boundaries.
/// Width of a single space at `size` in `family`, derived by differencing.
fn measure_space(tr: &mut TextRenderer, size: f32, family: &FontChoice) -> f32 {
    let w2 = tr
        .measure_font("m m", size, None, TextWeight::Regular, false, family)
        .0;
    let w1 = tr
        .measure_font("mm", size, None, TextWeight::Regular, false, family)
        .0;
    (w2 - w1).max(size * 0.22)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line may end after an em dash even when joiners glue it to the next
    /// word, and never inside a dash run or before the dash.
    #[test]
    fn em_dash_breaks_follow_the_dash_and_its_joiners() {
        assert_eq!(
            em_dash_breaks("gods\u{feff}—Deliberations"),
            vec!["gods\u{feff}—".len()]
        );
        assert_eq!(
            em_dash_breaks("gods\u{2060}—\u{2060}Deliberations"),
            vec!["gods\u{2060}—\u{2060}".len()]
        );
        assert_eq!(em_dash_breaks("so——then"), vec!["so——".len()]);
        assert!(em_dash_breaks("father—").is_empty());
        assert!(em_dash_breaks("half-way").is_empty());
    }
}
