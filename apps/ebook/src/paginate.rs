//! Reflowable text layout + pagination.
//!
//! Given a chapter's [`Block`]s, a content box, and the live [`Settings`], this
//! lays the chapter out as one continuous column with real inline styling
//! (bold / italic / mono) and word-level wrapping, then slices that column into
//! fixed-height pages at line boundaries (so a line is never cut in half).
//!
//! Layout is measurement-bound, so it runs inside `draw` (where a
//! [`TextRenderer`] is available) and is cached per [`LayoutKey`]; it only
//! recomputes when the chapter, content width, or a typography setting changes.

use ui::text_render::{FontChoice, TextRenderer, TextWeight};
use ui::{
    ComposedLine, ParagraphBox, ParagraphOptions, compose_paragraph, discretionary_hyphen_breaks,
    line_break_opportunities,
};
use unicode_bidi::{BidiInfo, Level};

use crate::model::{Block, FontKind, Settings, Span};

/// Map a reader [`FontKind`] to the toolkit [`FontChoice`] used for shaping.
pub fn font_choice(kind: &FontKind) -> FontChoice {
    match kind {
        FontKind::Default => FontChoice::SansSerif,
        FontKind::Serif => FontChoice::Serif,
        FontKind::Mono => FontChoice::Monospace,
        // A reading face by its name; the toolkit sets each run in the
        // face's optical cut for its size. A name this build does not ship
        // reads in the system's reading face.
        FontKind::Reading(name) => ui::ReadingFace::from_name(name)
            .map_or(FontChoice::Serif, |face| {
                FontChoice::Named(face.family().to_owned())
            }),
        FontKind::Named(n) => FontChoice::Named(n.clone()),
    }
}

/// A positioned, single-style run within a line. `x` is relative to the line's
/// left edge (which is `content_x + indent`). A run here is one whitespace-
/// separated word; `char_start`/`char_len` locate it within the block's
/// canonical text so text selection can map a point to a character offset.
#[derive(Clone, Debug)]
pub struct LineRun {
    pub x: f32,
    /// Vertical offset within the line box (used to baseline inline math).
    pub y: f32,
    /// Canonical source text covered by this run.
    pub text: String,
    /// Painted text; differs only when an exercised discretionary break adds
    /// a terminal hyphen that is absent from the source document.
    pub display_text: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub math: Option<ui::math::Layout>,
    /// Char offset of this run's first character within the block's canonical
    /// text (see [`LaidBlock::text`]).
    pub char_start: usize,
    /// Number of chars in this run.
    pub char_len: usize,
}

/// One visual line of a block. `y` is absolute within the chapter's flow.
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
}

#[derive(Clone, Debug)]
pub struct LaidMath {
    pub layout: ui::math::Layout,
    pub x: f32,
    pub y_offset: f32,
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
    /// line break (a verse line, a `<br/>`), ascending.
    pub hard_breaks: Vec<usize>,
}

/// A fully laid-out chapter plus its page breaks.
#[derive(Clone, Debug)]
pub struct ChapterLayout {
    pub blocks: Vec<LaidBlock>,
    pub total_height: f32,
    /// Absolute y at which each page begins. `pages.len()` == page count.
    pub pages: Vec<f32>,
    pub key: LayoutKey,
}

impl ChapterLayout {
    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }
}

/// Cache key: layout is invalid if any of these change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutKey {
    pub chapter: usize,
    pub width_q: i32,
    pub height_q: i32,
    pub scale_q: i32,
    /// System accessibility text scale. `TextRenderer` applies this to glyph
    /// metrics, so the flow cache and vertical rhythm must invalidate with it.
    pub text_scale_q: i32,
    pub spacing_q: i32,
    pub composition: bool,
    /// Signature of the selected font (family + per-face size multiplier), so a
    /// font change relays out.
    pub font_sig: u64,
}

impl LayoutKey {
    pub fn new(
        chapter: usize,
        content_w: f32,
        content_h: f32,
        s: &Settings,
        font_sig: u64,
        text_scale: f32,
    ) -> Self {
        Self {
            chapter,
            width_q: content_w.round() as i32,
            height_q: content_h.round() as i32,
            scale_q: (s.font_scale * 100.0).round() as i32,
            text_scale_q: (text_scale * 100.0).round() as i32,
            spacing_q: (s.line_spacing * 100.0).round() as i32,
            composition: s.composition.justified(),
            font_sig,
        }
    }
}

/// Per-block style derived from kind + settings.
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
}

fn style_for(block: &Block, body: f32, text_scale: f32) -> BlockStyle {
    match block {
        Block::Heading { level, .. } => {
            let (mult, sb) = match level {
                1 => (1.62, 1.6),
                2 => (1.32, 1.3),
                _ => (1.14, 1.0),
            };
            BlockStyle {
                kind: LayoutKind::Heading,
                size: body * mult,
                force_italic: false,
                force_bold: true,
                indent: 0.0,
                space_before: body * sb * text_scale,
                fixed_height: None,
            }
        }
        Block::Paragraph { .. } => BlockStyle {
            kind: LayoutKind::Paragraph,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 0.0,
            space_before: body * 0.75 * text_scale,
            fixed_height: None,
        },
        Block::Quote { .. } => BlockStyle {
            kind: LayoutKind::Quote,
            size: body * 1.02,
            force_italic: true,
            force_bold: false,
            indent: 30.0 * text_scale,
            space_before: body * 0.9 * text_scale,
            fixed_height: None,
        },
        Block::Verse { quoted, .. } => BlockStyle {
            kind: LayoutKind::Verse { quoted: *quoted },
            size: body,
            force_italic: false,
            force_bold: false,
            indent: if *quoted { 30.0 * text_scale } else { 0.0 },
            space_before: body * 0.75 * text_scale,
            fixed_height: None,
        },
        Block::ListItem { .. } => BlockStyle {
            kind: LayoutKind::List,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 28.0 * text_scale,
            space_before: body * 0.4 * text_scale,
            fixed_height: None,
        },
        Block::Math { .. } => BlockStyle {
            kind: LayoutKind::Math,
            size: body * 1.08,
            force_italic: false,
            force_bold: false,
            indent: 0.0,
            space_before: body * text_scale,
            fixed_height: None,
        },
        Block::Separator => BlockStyle {
            kind: LayoutKind::Separator,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 0.0,
            space_before: body * 1.2 * text_scale,
            fixed_height: Some(body * 1.6 * text_scale),
        },
    }
}

/// A word carrying its inline style.
#[allow(
    clippy::struct_excessive_bools,
    reason = "bold/italic/mono/space_before/hyphen_break_before/natural_break_before are \
              independent per-word layout facts, not a state machine"
)]
struct Word {
    text: String,
    bold: bool,
    italic: bool,
    mono: bool,
    math: Option<ui::math::Layout>,
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

/// Reader font scale from which layout keeps natural glyph metrics inside
/// the measured page box: no glyph expansion and no hanging punctuation.
const ACCESSIBLE_READING_SCALE: f32 = 1.35;

/// Whether punctuation may hang into the margins: book composition at
/// ordinary sizes. At accessibility sizes a hanging mark is a whole glyph
/// width outside the measure on a narrow page.
fn hangs_punctuation(settings: &Settings) -> bool {
    settings.composition.justified() && settings.font_scale < ACCESSIBLE_READING_SCALE
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
    expr: &ui::math::MathExpr,
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
    let math_layout = ui::math::layout(&expr.root, text, style.size);
    built.line_height = built
        .line_height
        .max(math_layout.height as f32 + style.size * 0.18);
    built.words.push(Word {
        text: span.text.clone(),
        bold: false,
        italic: false,
        mono: false,
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
    st: &BlockStyle,
    bold: bool,
    italic: bool,
    tr: &mut TextRenderer,
    word_family: &FontChoice,
) -> f32 {
    if fraction == 0.0 {
        return 0.0;
    }
    tr.measure_font(
        &character.to_string(),
        st.size,
        None,
        weight(bold),
        italic,
        word_family,
    )
    .0 * fraction
}

/// Per-word style flags, bundled so the fragment-building functions below
/// don't take four separate `bool` parameters.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent style toggle (space-before, bold, italic, mono); this struct exists specifically to replace four bool arguments, so un-bundling them would defeat its purpose"
)]
struct WordFlags {
    space_before: bool,
    bold: bool,
    italic: bool,
    mono: bool,
}

struct FragmentBuildCtx<'a> {
    words: &'a mut Vec<Word>,
    style: &'a BlockStyle,
    settings: &'a Settings,
    language: &'a str,
    text: &'a mut TextRenderer,
}

struct SourceWord<'a> {
    text: &'a str,
    char_start: usize,
    source_byte_start: usize,
    flags: WordFlags,
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
    block: &'a Block,
    style: &'a BlockStyle,
    settings: &'a Settings,
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
    settings: &'a Settings,
    content_width: f32,
    base_family: &'a FontChoice,
    block_top: f32,
    line_height: f32,
}

struct LineComposition<'a, 'text> {
    words: &'a [Word],
    bidi: &'a BidiInfo<'text>,
    style: &'a BlockStyle,
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
    let WordFlags {
        space_before,
        bold,
        italic,
        mono,
    } = source.flags;
    let fragment = source
        .text
        .get(slice.byte_start..slice.byte_end)
        .unwrap_or_default();
    let hyphen_break_before =
        slice.index != 0 && breaks.hyphen.binary_search(&slice.byte_start).is_ok();
    let natural_break_before =
        slice.index != 0 && breaks.unicode.binary_search(&slice.byte_start).is_ok();
    let stretch_before = if natural_break_before && ctx.settings.composition.justified() {
        script_stretch_before(source.text, slice.byte_start, ctx.style.size)
    } else {
        0.0
    };
    let fragment_chars = fragment.chars().count();
    let fragment_width = ctx
        .text
        .measure_font(
            fragment,
            ctx.style.size,
            None,
            weight(bold),
            italic,
            source.family,
        )
        .0;
    let end_protrusion = if slice.byte_end == source.text.len() {
        fragment.chars().last().map_or(0.0, |character| {
            let fraction = if hangs_punctuation(ctx.settings) {
                punctuation_protrusion(character)
            } else {
                0.0
            };
            protrusion_width(
                character,
                fraction,
                ctx.style,
                bold,
                italic,
                ctx.text,
                source.family,
            )
        })
    } else {
        0.0
    };
    let start_protrusion =
        if hangs_punctuation(ctx.settings) && slice.index == 0 && breaks.opening_fraction > 0.0 {
            fragment.chars().next().map_or(0.0, |character| {
                protrusion_width(
                    character,
                    breaks.opening_fraction,
                    ctx.style,
                    bold,
                    italic,
                    ctx.text,
                    source.family,
                )
            })
        } else {
            0.0
        };
    ctx.words.push(Word {
        text: fragment.to_string(),
        bold,
        italic,
        mono,
        math: None,
        w: fragment_width,
        space_before: slice.index == 0 && space_before,
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
    let hyphenate =
        ctx.settings.composition.justified() && !matches!(ctx.style.kind, LayoutKind::Verse { .. });
    let breaks = word_break_boundaries(source.text, hyphenate, ctx.language);
    let hyphen_width = ctx
        .text
        .measure_font(
            "-",
            ctx.style.size,
            None,
            weight(source.flags.bold),
            source.flags.italic,
            source.family,
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
    for span in input.block.spans() {
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
                    settings: input.settings,
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
                    },
                    family: word_family,
                    line_start: line_start.filter(|_| wi == 0),
                },
            );
        }
    }
    built
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
            && previous.bold == word.bold
            && previous.italic == word.italic
            && previous.mono == word.mono
            && previous.math.is_none()
            && word.math.is_none()
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
                    .measure_font(
                        &previous.text,
                        ctx.style.size,
                        None,
                        weight(word.bold),
                        word.italic,
                        family,
                    )
                    .0
                    * line.glyph_scale;
            previous_index = Some(word_index);
            continue;
        }
        runs.push(LineRun {
            x,
            y: word
                .math
                .as_ref()
                .map_or_else(|| 0.0, |math| (ctx.line_height - math.height as f32) * 0.5),
            text: word.text.clone(),
            display_text: word.text.clone(),
            bold: word.bold,
            italic: word.italic,
            mono: word.mono,
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
    let bidi = BidiInfo::new(composition.canonical_text, None);
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
    options.justify = composition.settings.composition.justified()
        && wide_measure
        && matches!(
            composition.style.kind,
            LayoutKind::Paragraph | LayoutKind::Quote
        );
    // At accessibility sizes, favour predictable natural glyph metrics over
    // the subtle expansion used for book-like justification. On a narrow
    // phone one expanded line can otherwise exceed its measured paint box,
    // leaving the final word visibly clipped instead of reflowing.
    if !options.justify || composition.settings.font_scale >= ACCESSIBLE_READING_SCALE || verse {
        options.max_glyph_expansion = 0.0;
    }
    let em = composition.style.size;
    let hang = if verse { em } else { 0.0 };
    let mut lines: Vec<LaidLine> = Vec::new();
    let mut line_ctx = LineComposition {
        words: composition.words,
        bidi: &bidi,
        style: composition.style,
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

/// A laid-out `Block::Math` — the formula centered in the content width, with
/// vertical breathing room scaled off the body size.
struct MathBlockInput<'a> {
    index: usize,
    top: f32,
    expression: &'a ui::math::MathExpr,
    source: &'a str,
    style: &'a BlockStyle,
    body_size: f32,
    content_width: f32,
}

fn laid_math_block(text: &mut TextRenderer, input: &MathBlockInput<'_>) -> LaidBlock {
    let math_layout = ui::math::layout(&input.expression.root, text, input.style.size);
    let vertical_pad = input.body_size * 0.35;
    let height = math_layout.height as f32 + vertical_pad * 2.0;
    let math_x = ((input.content_width - math_layout.width as f32) * 0.5).max(0.0);
    LaidBlock {
        index: input.index,
        kind: LayoutKind::Math,
        top: input.top,
        bottom: input.top + height,
        size: input.style.size,
        line_height: height,
        indent: 0.0,
        lines: vec![LaidLine {
            y: input.top,
            glyph_scale: 1.0,
            adjustment_ratio: 0.0,
            rtl: false,
            runs: vec![],
        }],
        bullet: None,
        math: Some(LaidMath {
            layout: math_layout,
            x: math_x,
            y_offset: vertical_pad,
        }),
        text: input.source.to_string(),
        char_len: input.source.chars().count(),
        hard_breaks: Vec::new(),
    }
}

/// A laid-out `Block::Separator` — a fixed-height rule with no text.
fn laid_separator_block(index: usize, y: f32, st: &BlockStyle) -> LaidBlock {
    let h = st.fixed_height.unwrap_or(st.size * 1.6);
    LaidBlock {
        index,
        kind: LayoutKind::Separator,
        top: y,
        bottom: y + h,
        size: st.size,
        line_height: h,
        indent: 0.0,
        lines: vec![LaidLine {
            y,
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
    }
}

/// Lay out one chapter into positioned lines + page breaks. `base_family` is
/// the reader's chosen body face (serif / a named installed family / sans);
/// `font_mult` scales the nominal body size for that face; `font_sig` feeds the
/// layout cache key so a font change relays out.
pub struct ChapterLayoutRequest<'a> {
    pub blocks: &'a [Block],
    pub content_width: f32,
    pub content_height: f32,
    pub settings: &'a Settings,
    pub chapter: usize,
    pub language: &'a str,
    pub base_family: &'a FontChoice,
    pub font_multiplier: f32,
    pub font_signature: u64,
}

/// The marker set before a list item: its number, or a bullet.
fn list_marker(block: &Block) -> Option<String> {
    match block {
        Block::ListItem {
            ordered: true,
            index: n,
            ..
        } => Some(format!("{n}.")),
        Block::ListItem { ordered: false, .. } => Some("•".to_string()),
        _ => None,
    }
}

pub fn layout_chapter(tr: &mut TextRenderer, request: &ChapterLayoutRequest<'_>) -> ChapterLayout {
    let text_scale = tr.text_scale();
    let key = LayoutKey::new(
        request.chapter,
        request.content_width,
        request.content_height,
        request.settings,
        request.font_signature,
        text_scale,
    );
    let body = request.settings.body_size_with(request.font_multiplier);
    let settings = request.settings;
    let base_mono = matches!(request.base_family, FontChoice::Monospace);
    let mut laid: Vec<LaidBlock> = Vec::with_capacity(request.blocks.len());
    let mut y = 0.0f32;

    for (index, block) in request.blocks.iter().enumerate() {
        let st = style_for(block, body, text_scale);
        y += st.space_before;

        if let Block::Math { expr, source, .. } = block {
            let laid_block = laid_math_block(
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
            );
            y = laid_block.bottom;
            laid.push(laid_block);
            continue;
        }

        if st.kind == LayoutKind::Separator {
            let laid_block = laid_separator_block(index, y, &st);
            y = laid_block.bottom;
            laid.push(laid_block);
            continue;
        }

        let built = build_words(
            tr,
            &WordBuildInput {
                block,
                style: &st,
                settings,
                language: request.language,
                base_family: request.base_family,
                base_mono,
                line_height: st.size * 1.25 * settings.line_spacing * text_scale,
            },
        );
        let line_height = built.line_height;

        let block_top = y;
        let lines = compose_block_lines(
            tr,
            &BlockComposition {
                words: &built.words,
                canonical_text: &built.canonical_text,
                style: &st,
                settings,
                content_width: request.content_width,
                base_family: request.base_family,
                block_top,
                line_height,
            },
        );

        let bullet = list_marker(block);
        let height = lines.len() as f32 * line_height;
        laid.push(LaidBlock {
            index,
            kind: st.kind,
            top: block_top,
            bottom: block_top + height,
            size: st.size,
            line_height,
            indent: st.indent,
            lines,
            bullet,
            math: None,
            char_len: built.char_count,
            text: built.canonical_text,
            hard_breaks: built.hard_breaks,
        });
        y += height;
    }

    let pages = paginate(&laid, request.content_height);

    ChapterLayout {
        blocks: laid,
        total_height: y,
        pages,
        key,
    }
}

/// Slice the flow into page-start offsets at line boundaries.
/// One row of the flattened page-breaking grid: either a whole `Separator`
/// block, or a single line of a text block (retaining enough paragraph
/// structure — `line`/`line_count` — to enforce TeX-like break constraints).
#[derive(Clone, Copy)]
struct Row {
    y: f32,
    height: f32,
    block: usize,
    line: usize,
    line_count: usize,
    kind: LayoutKind,
}

fn paginate(blocks: &[LaidBlock], page_h: f32) -> Vec<f32> {
    if blocks.is_empty() || page_h <= 1.0 {
        return vec![0.0];
    }

    // Flatten to row boxes while retaining enough paragraph structure to
    // enforce TeX-like page-break constraints.
    let mut rows = Vec::new();
    for (block_index, b) in blocks.iter().enumerate() {
        if b.kind == LayoutKind::Separator {
            rows.push(Row {
                y: b.top,
                height: b.bottom - b.top,
                block: block_index,
                line: 0,
                line_count: 1,
                kind: b.kind,
            });
        } else {
            for (line, l) in b.lines.iter().enumerate() {
                rows.push(Row {
                    y: l.y,
                    height: b.line_height,
                    block: block_index,
                    line,
                    line_count: b.lines.len(),
                    kind: b.kind,
                });
            }
        }
    }
    rows.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));

    let Some(&first) = rows.first() else {
        return vec![0.0]; // unreachable: a non-empty block always yields >=1 row
    };
    let mut pages = vec![first.y];
    let mut page_row = 0;
    while page_row < rows.len() {
        let Some(&start_row) = rows.get(page_row) else {
            break;
        };
        let page_start = start_row.y;
        let Some(overflow) = (page_row..rows.len()).find(|&index| {
            rows.get(index)
                .is_some_and(|r| r.y + r.height - page_start > page_h + 0.5)
        }) else {
            break;
        };
        let mut break_at = overflow;
        let Some(&row) = rows.get(break_at) else {
            break;
        };

        // Keep at least two lines of an ordinary text block on either side of
        // a page boundary whenever that can be achieved on this page.
        if matches!(
            row.kind,
            LayoutKind::Paragraph | LayoutKind::Quote | LayoutKind::Verse { .. } | LayoutKind::List
        ) && row.line_count >= 4
            && (row.line == 1 || row.line_count - row.line == 1)
        {
            break_at -= 1;
        }

        // A heading is semantically attached to the text following it. If it
        // would become the final item on a page, carry the complete heading to
        // the next page as long as it did not itself begin this page.
        if break_at > page_row
            && let Some(&prev) = rows.get(break_at - 1)
            && let Some(&cur) = rows.get(break_at)
            && prev.kind == LayoutKind::Heading
            && prev.block != cur.block
        {
            let heading_block = prev.block;
            let heading_start = (page_row..break_at)
                .find(|&index| rows.get(index).is_some_and(|r| r.block == heading_block))
                .unwrap_or(break_at - 1);
            if heading_start > page_row {
                break_at = heading_start;
            }
        }

        // An indivisible item taller than a page has no legal alternative.
        if break_at <= page_row {
            break_at = overflow.max(page_row + 1);
        }
        let Some(&brk) = rows.get(break_at) else {
            break;
        };
        pages.push(brk.y);
        page_row = break_at;
    }
    pages
}

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
    use crate::model::{Block, CompositionStyle, Span};

    fn para(text: &str) -> Block {
        Block::Paragraph {
            spans: vec![Span::plain(text)],
        }
    }

    fn test_layout(
        text: &mut TextRenderer,
        blocks: &[Block],
        content_width: f32,
        content_height: f32,
        settings: &Settings,
        language: &str,
        base_family: &FontChoice,
    ) -> ChapterLayout {
        layout_chapter(
            text,
            &ChapterLayoutRequest {
                blocks,
                content_width,
                content_height,
                settings,
                chapter: 0,
                language,
                base_family,
                font_multiplier: 1.0,
                font_signature: 0,
            },
        )
    }

    /// Layout produces pages and lines (including math and tall narrow content), keys on settings and font, and assigns selection offsets.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn layout_paginates_content_and_keys_on_settings() {
        // --- phase: layout_produces_pages_and_lines ---
        {
            let mut tr = TextRenderer::new();
            let blocks = vec![
                Block::Heading {
                    level: 1,
                    spans: vec![Span::plain("A Chapter")],
                },
                para(
                    "This is a reasonably long paragraph that should wrap across \
                  several lines when laid out into a narrow content column so \
                  that pagination has something real to slice into pages here.",
                ),
                Block::Separator,
                para(
                    "Another paragraph after the rule, also long enough to wrap a \
                  couple of times within the narrow measure we hand the engine.",
                ),
            ];
            let layout = test_layout(
                &mut tr,
                &blocks,
                360.0,
                220.0,
                &Settings::default(),
                "en-US",
                &FontChoice::SansSerif,
            );
            assert!(layout.total_height > 0.0);
            assert!(layout.page_count() >= 1);
            // The long paragraph wrapped into multiple lines.
            let para_block = &layout.blocks[1];
            assert!(para_block.lines.len() >= 2, "paragraph should wrap");
            // Lines are monotonically increasing in y.
            let ys: Vec<f32> = para_block.lines.iter().map(|l| l.y).collect();
            assert!(ys.windows(2).all(|w| w[1] > w[0]));
        }

        // --- phase: math_block_uses_shared_two_dimensional_layout ---
        {
            let mut tr = TextRenderer::new();
            let expr = ui::math::MathExpr {
                root: vec![ui::math::Node::Frac {
                    num: vec![
                        ui::math::Node::Char('x'),
                        ui::math::Node::Sup {
                            exp: vec![ui::math::Node::Char('2')],
                        },
                    ],
                    den: vec![ui::math::Node::Sqrt {
                        index: None,
                        radicand: vec![ui::math::Node::Char('y')],
                    }],
                }],
            };
            let layout = test_layout(
                &mut tr,
                &[Block::Math {
                    expr,
                    source: "(x^(2))/(sqrt(y))".into(),
                    display: true,
                }],
                420.0,
                500.0,
                &Settings::default(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            let math = block.math.as_ref().expect("shared math layout attached");
            assert_eq!(block.kind, LayoutKind::Math);
            assert!(math.layout.height > f64::from(block.size) * 2.0);
            assert!(math.x > 0.0, "display formula is centered in the measure");
            assert!(
                math.layout
                    .boxes
                    .iter()
                    .any(|math_box| matches!(math_box.kind, ui::math::BoxKind::FracBar))
            );
        }

        // --- phase: narrow_tall_content_paginates_into_several_pages ---
        {
            let mut tr = TextRenderer::new();
            let big = "word ".repeat(400);
            let blocks = vec![para(&big)];
            let layout = test_layout(
                &mut tr,
                &blocks,
                300.0,
                160.0,
                &Settings::default(),
                "en-US",
                &FontChoice::SansSerif,
            );
            assert!(layout.page_count() >= 3, "lots of text -> several pages");
            // Page starts strictly increase.
            assert!(layout.pages.windows(2).all(|w| w[1] > w[0]));
        }

        // --- phase: layout_key_changes_with_settings_and_font ---
        {
            let base = LayoutKey::new(0, 300.0, 200.0, &Settings::default(), 0, 1.0);
            let bigger_font = Settings {
                font_scale: 1.4,
                ..Default::default()
            };
            let scaled = LayoutKey::new(0, 300.0, 200.0, &bigger_font, 0, 1.0);
            assert_ne!(base, scaled, "font scale change relays out");
            // A different font signature (family/size-mult) also relays out.
            let font_changed = LayoutKey::new(0, 300.0, 200.0, &Settings::default(), 99, 1.0);
            assert_ne!(base, font_changed, "font change relays out");
            let ragged = Settings {
                composition: CompositionStyle::Ragged,
                ..Default::default()
            };
            let ragged_key = LayoutKey::new(0, 300.0, 200.0, &ragged, 0, 1.0);
            assert_ne!(base, ragged_key, "composition change relays out");
            let accessibility_scaled =
                LayoutKey::new(0, 300.0, 200.0, &Settings::default(), 0, 2.0);
            assert_ne!(
                base, accessibility_scaled,
                "host accessibility text scale relays out"
            );
        }

        // --- phase: selection_char_offsets_are_assigned_across_words ---
        {
            let mut tr = TextRenderer::new();
            let blocks = vec![para("alpha beta gamma")];
            let layout = test_layout(
                &mut tr,
                &blocks,
                600.0,
                400.0,
                &Settings::default(),
                "en-US",
                &FontChoice::SansSerif,
            );
            let b = &layout.blocks[0];
            assert_eq!(
                b.text, "alpha beta gamma",
                "canonical text joins words with spaces"
            );
            // Each run carries a char range that indexes into the canonical text.
            let runs: Vec<&LineRun> = b.lines.iter().flat_map(|l| l.runs.iter()).collect();
            assert_eq!(runs.len(), 3);
            for r in &runs {
                let slice: String = b.text.chars().skip(r.char_start).take(r.char_len).collect();
                assert_eq!(slice, r.text, "run char range slices back to its text");
            }
        }
    }

    /// Ragged composition, hyphenation, hanging punctuation, CJK breaking and RTL alignment follow language and composition.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn microtypography_follows_language_and_composition() {
        // --- phase: ragged_composition_disables_book_microtypography ---
        {
            let mut tr = TextRenderer::new();
            let settings = Settings {
                composition: CompositionStyle::Ragged,
                ..Default::default()
            };
            let layout = test_layout(
                &mut tr,
                &[para(
                    "“Characteristically typographic paragraphs demonstrate hyphenation and spacing.”",
                )],
                145.0,
                600.0,
                &settings,
                "en-US",
                &FontChoice::Serif,
            );
            let lines = &layout.blocks[0].lines;
            assert!(
                lines.len() >= 2,
                "fixture wraps across several ragged lines"
            );
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.runs)
                    .all(|run| !run.display_text.ends_with('-')),
                "ragged composition inserts no discretionary hyphens"
            );
            #[allow(
                clippy::float_cmp,
                reason = "ragged composition assigns glyph_scale exactly 1.0 (no computed \
                      expansion), so exact equality is the correct assertion"
            )]
            {
                assert!(
                    lines.iter().all(|line| line.glyph_scale == 1.0),
                    "ragged composition does not expand or condense glyphs"
                );
            }
            assert_eq!(lines[0].runs[0].x, 0.0, "opening punctuation does not hang");
        }

        // --- phase: language_aware_hyphenation_paints_only_the_exercised_break ---
        {
            let mut tr = TextRenderer::new();
            let blocks = vec![para("characteristically")];
            let layout = test_layout(
                &mut tr,
                &blocks,
                105.0,
                400.0,
                &Settings::default(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            assert!(block.lines.len() >= 2, "the long word should hyphenate");
            assert!(
                block.lines[..block.lines.len() - 1]
                    .iter()
                    .filter_map(|line| line.runs.last())
                    .any(|run| run.display_text.ends_with('-')),
                "a selected discretionary break paints its hyphen"
            );
            let source: String = block
                .lines
                .iter()
                .flat_map(|line| line.runs.iter())
                .map(|run| run.text.as_str())
                .collect();
            assert_eq!(
                source, block.text,
                "virtual hyphens never enter source text"
            );
        }

        // --- phase: opening_punctuation_hangs_into_the_margin_without_changing_source ---
        {
            let mut tr = TextRenderer::new();
            let blocks = vec![para("“Typography begins at the edge.”")];
            let layout = test_layout(
                &mut tr,
                &blocks,
                600.0,
                400.0,
                &Settings::default(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            let line = &block.lines[0];
            assert!(line.runs[0].x < 0.0, "opening quote protrudes left");
            for run in &line.runs {
                let source: String = block
                    .text
                    .chars()
                    .skip(run.char_start)
                    .take(run.char_len)
                    .collect();
                assert_eq!(source, run.text);
            }
        }

        // --- phase: unspaced_cjk_uses_unicode_breaks_and_inter_character_justification ---
        {
            let mut tr = TextRenderer::new();
            let text = "天地玄黃宇宙洪荒日月盈昃辰宿列張寒來暑往秋收冬藏";
            let layout = test_layout(
                &mut tr,
                &[para(text)],
                120.0,
                500.0,
                &Settings::default(),
                "zh-Hant",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            assert!(block.lines.len() >= 2, "CJK wraps without ASCII spaces");
            assert_eq!(block.text, text);
            assert!(
                block.lines[..block.lines.len() - 1]
                    .iter()
                    .any(|line| line.runs.len() > 1 && line.adjustment_ratio != 0.0),
                "non-final CJK lines participate in micro-justification"
            );
        }

        // --- phase: rtl_paragraphs_use_visual_bidi_order_and_right_align_the_final_line ---
        {
            let mut tr = TextRenderer::new();
            let layout = test_layout(
                &mut tr,
                &[para("مرحبا بالعالم 123")],
                500.0,
                400.0,
                &Settings::default(),
                "ar",
                &FontChoice::Serif,
            );
            let line = &layout.blocks[0].lines[0];
            assert!(line.rtl);
            assert!(line.runs.windows(2).all(|pair| pair[0].x < pair[1].x));
            let logical_first = line
                .runs
                .iter()
                .find(|run| run.char_start == 0)
                .expect("first Arabic word");
            assert!(
                logical_first.x > line.runs[0].x,
                "logical first word is visually rightmost"
            );
            assert!(line.runs[0].x > 100.0, "short RTL final line aligns right");
        }
    }

    /// A run of `text` that starts a new line indented `indent` ems.
    fn line(text: &str, indent: u8) -> Span {
        Span {
            space_before: true,
            line_break: true,
            line_indent: indent,
            ..Span::plain(text)
        }
    }

    fn line_text(line: &LaidLine) -> String {
        line.runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Verse sets one ragged line per source line, indents marked lines, hangs wrapped ones, and keeps the prose offsets.
    #[test]
    fn verse_lays_out_one_ragged_line_per_source_line() {
        // --- phase: verse_lines_are_ragged_indented_and_hang_when_wrapped ---
        {
            let mut tr = TextRenderer::new();
            let long =
                "The capitals of many nations, learned the customs of their dwellers, and endured";
            let verse = Block::Verse {
                spans: vec![
                    Span::plain("Tell me, O Muse,"),
                    line("Who, having overthrown", 0),
                    line("Of Ilium, wandered far", 2),
                    line(long, 0),
                ],
                quoted: false,
            };
            let layout = test_layout(
                &mut tr,
                std::slice::from_ref(&verse),
                360.0,
                2000.0,
                &Settings::default(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            assert_eq!(block.kind, LayoutKind::Verse { quoted: false });
            assert_eq!(block.text, verse.plain_text(), "offsets are the prose ones");
            assert_eq!(line_text(&block.lines[0]), "Tell me, O Muse,");
            assert_eq!(line_text(&block.lines[1]), "Who, having overthrown");
            assert_eq!(line_text(&block.lines[2]), "Of Ilium, wandered far");
            assert!(block.lines.len() >= 5, "the long line wraps");
            let x = |line: usize| block.lines[line].runs[0].x;
            let em = block.size;
            assert!((x(2) - x(1) - 2.0 * em).abs() < 0.5, "i2 indents two ems");
            assert!(
                (x(4) - x(3) - em).abs() < 0.5,
                "a wrapped line hangs one em"
            );
            #[allow(
                clippy::float_cmp,
                reason = "unjustified lines keep exactly the natural glyph scale 1.0"
            )]
            {
                assert!(block.lines.iter().all(|line| line.glyph_scale == 1.0));
            }
            assert!(
                block
                    .lines
                    .iter()
                    .flat_map(|line| &line.runs)
                    .all(|run| !run.display_text.ends_with('-')),
                "verse is not hyphenated"
            );
            for run in block.lines.iter().flat_map(|line| &line.runs) {
                let source: String = block
                    .text
                    .chars()
                    .skip(run.char_start)
                    .take(run.char_len)
                    .collect();
                assert_eq!(source, run.text);
            }
        }
    }

    /// A prose `<br/>` ends its line with natural spacing and no hanging indent.
    #[test]
    fn a_prose_break_ends_its_line_without_justifying_it() {
        {
            let mut tr = TextRenderer::new();
            let prose = Block::Paragraph {
                spans: vec![
                    Span::plain("Short line"),
                    line(
                        "and then a paragraph long enough to wrap and be justified across the measure",
                        0,
                    ),
                ],
            };
            let layout = test_layout(
                &mut tr,
                std::slice::from_ref(&prose),
                300.0,
                2000.0,
                &Settings::default(),
                "en-US",
                &FontChoice::Serif,
            );
            let natural = test_layout(
                &mut tr,
                &[para("Short line")],
                300.0,
                2000.0,
                &Settings::default(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            assert_eq!(line_text(&block.lines[0]), "Short line");
            let xs = |line: &LaidLine| line.runs.iter().map(|run| run.x).collect::<Vec<_>>();
            assert_eq!(
                xs(&block.lines[0]),
                xs(&natural.blocks[0].lines[0]),
                "the broken line keeps natural spacing"
            );
            assert!(block.lines[1].runs[0].x.abs() < 0.5, "prose does not hang");
            assert_eq!(
                block.text,
                "Short line and then a paragraph long enough to wrap and be justified across the measure"
            );
        }
    }

    /// Standard Ebooks' Odyssey, Book I: a zero-width no-break space glues
    /// every em dash to the word before it.
    const ODYSSEY_BOOK_ONE_SUMMARY: &str = "A council of the gods\u{feff}—Deliberations concerning Ulysses\u{feff}—Mercury despatched to Calypso, to bid her send Ulysses to Ithaca\u{feff}—Visit of Pallas, in the shape of Mentor, to Telemachus, advising him to repair to Pylos and Sparta in quest of his father, Ulysses\u{feff}—Revels of the suitors of Penelope\u{feff}—Phemius, the minstrel, and his song of the return of the Grecians\u{feff}—The suitors rebuked by Telemachus.";

    fn painted_line(line: &LaidLine) -> String {
        let mut text = String::new();
        for (index, run) in line.runs.iter().enumerate() {
            if index > 0 && !text.ends_with(['—', '\u{feff}', '\u{2060}']) {
                text.push(' ');
            }
            text.push_str(&run.display_text);
        }
        text
    }

    /// At phone measures, em-dash compounds glued by invisible joiners
    /// break after the dash, and no justified line is left holding a lone
    /// short word.
    #[test]
    fn em_dash_compounds_break_without_stranding_words_at_phone_widths() {
        let mut tr = TextRenderer::new();
        let word_joined = ODYSSEY_BOOK_ONE_SUMMARY.replace("\u{feff}—", "\u{2060}—\u{2060}");
        for text in [ODYSSEY_BOOK_ONE_SUMMARY, word_joined.as_str()] {
            for width in (300..=400).step_by(2) {
                let width = width as f32;
                let layout = test_layout(
                    &mut tr,
                    &[para(text)],
                    width,
                    4000.0,
                    &Settings::default(),
                    "en-US",
                    &FontChoice::Serif,
                );
                let block = &layout.blocks[0];
                let lines: Vec<String> = block.lines.iter().map(painted_line).collect();
                let size = block.size;
                for line in &lines[..lines.len() - 1] {
                    let natural = tr
                        .measure_font(
                            line,
                            size,
                            None,
                            TextWeight::Regular,
                            false,
                            &FontChoice::Serif,
                        )
                        .0;
                    assert!(
                        natural > width * 0.6,
                        "{width}px: underfull justified line {line:?} in {lines:#?}"
                    );
                }
                let source: String = block
                    .lines
                    .iter()
                    .flat_map(|line| &line.runs)
                    .map(|run| run.text.as_str())
                    .collect();
                assert_eq!(
                    source.replace(' ', ""),
                    block.text.replace(' ', ""),
                    "breaks never drop or duplicate source text"
                );
            }
        }
    }

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

    /// Word joiners and zero-width no-break spaces are invisible: they add
    /// no advance and paint no glyph in any reader face. A joiner does glue
    /// the dash to its words, which a kerned face (New Computer Modern) may
    /// then kern: the joined run is never wider than the plain one and at
    /// most a kern pair or two narrower.
    #[test]
    fn invisible_joiners_have_no_width() {
        const SIZE: f32 = 18.0;
        let mut tr = TextRenderer::new();
        for family in [FontChoice::Serif, FontChoice::SansSerif] {
            let mut width = |text: &str| {
                tr.measure_font(text, SIZE, None, TextWeight::Regular, false, &family)
                    .0
            };
            let plain = width("gods—Deliberations");
            for joined in [
                "gods\u{feff}—Deliberations",
                "gods\u{2060}—\u{2060}Deliberations",
            ] {
                let measured = width(joined);
                assert!(
                    measured < plain + 0.5 && plain - measured < 0.2 * SIZE,
                    "{joined:?} measures {measured} vs {plain}"
                );
            }
            assert!(width("\u{2060}\u{feff}") < 0.5);
        }
    }

    fn block_with_lines(kind: LayoutKind, top: f32, line_count: usize) -> LaidBlock {
        let line_height = 20.0;
        LaidBlock {
            index: 0,
            kind,
            top,
            bottom: top + line_count as f32 * line_height,
            size: 16.0,
            line_height,
            indent: 0.0,
            lines: (0..line_count)
                .map(|line| LaidLine {
                    y: top + line as f32 * line_height,
                    glyph_scale: 1.0,
                    adjustment_ratio: 0.0,
                    rtl: false,
                    runs: Vec::new(),
                })
                .collect(),
            bullet: None,
            math: None,
            text: String::new(),
            char_len: 0,
            hard_breaks: Vec::new(),
        }
    }

    /// Pagination avoids single-line widows and orphans and keeps headings with their text.
    #[test]
    fn pagination_avoids_widows_orphans_and_stranded_headings() {
        // --- phase: pagination_avoids_single_line_widows_and_orphans ---
        {
            let widow = block_with_lines(LayoutKind::Paragraph, 0.0, 5);
            assert_eq!(paginate(&[widow], 85.0), vec![0.0, 60.0]);

            let before = block_with_lines(LayoutKind::Separator, 0.0, 1);
            let paragraph = block_with_lines(LayoutKind::Paragraph, 40.0, 5);
            assert_eq!(paginate(&[before, paragraph], 65.0), vec![0.0, 40.0, 100.0]);
        }

        // --- phase: pagination_keeps_a_heading_with_following_text ---
        {
            let intro = block_with_lines(LayoutKind::Paragraph, 0.0, 1);
            let heading = block_with_lines(LayoutKind::Heading, 25.0, 1);
            let body = block_with_lines(LayoutKind::Paragraph, 45.0, 3);
            assert_eq!(
                paginate(&[intro, heading, body], 55.0),
                vec![0.0, 25.0, 65.0]
            );
        }
    }
}
