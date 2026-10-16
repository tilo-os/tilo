use crate::num::f32_of_f64;
use cosmic_text::fontdb::Weight;
use cosmic_text::{
    Align, Attrs, Buffer, Family, FontFeatures as CosmicFontFeatures, FontSystem, Metrics, Shaping,
    Style as FontStyle, SwashCache, Wrap,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::{Color, Fill, FontData as Font, StyleRef};

use crate::text_break::{LineBreakKind, OffsetMap, line_break_opportunities};

mod cache;
pub use cache::PreparedText;
use cache::{CacheKey, CacheQuery, TextCache};
use tilo_lock::RwLockRecover as _;

/// Product-wide primary font families. Tilo ships these exact faces, and the
/// renderer also assigns them to the CSS-like generic families below. Keeping
/// the names here makes plain text, rich text, and theme tokens agree instead
/// of depending on each machine's fontconfig defaults.
///
/// The UI face covers Latin, Vietnamese and Greek. Cyrillic falls back to a
/// face matched to it, sized to its x-height (see [`crate::font_catalog`]);
/// every other script falls back to Noto.
pub const UI_SANS_FAMILY: &str = "Tilo Sans";
/// The built-in "Hyperlegible" UI face a theme or the accessibility layer
/// can choose instead of [`UI_SANS_FAMILY`].
pub const UI_HYPERLEGIBLE_FAMILY: &str = "Atkinson Hyperlegible Next";
/// The default reading face, New Computer Modern. Set in this family, text
/// takes the cut its size calls for: the 8 pt design for footnotes, Book for
/// body text and Regular for titles ([`crate::tokens::ReadingFace`]).
pub const UI_SERIF_FAMILY: &str = "New Computer Modern";
pub const UI_MONOSPACE_FAMILY: &str = "Noto Sans Mono";
/// Dedicated OpenType MATH face used by the shared formula compositor.
pub const UI_MATH_FAMILY: &str = "Noto Sans Math";

fn browser_assets_for_missing(character: char) -> &'static [&'static str] {
    use unicode_script::{Script, UnicodeScript};
    match character.script() {
        Script::Arabic => &["NotoSansArabic[wght].ttf"],
        Script::Hebrew => &["NotoSansHebrew[wght].ttf"],
        Script::Devanagari => &["NotoSansDevanagari[wght].ttf"],
        Script::Bengali => &["NotoSansBengali[wght].ttf"],
        Script::Thai => &["NotoSansThai[wght].ttf"],
        Script::Han | Script::Bopomofo | Script::Hangul | Script::Hiragana | Script::Katakana => {
            &["NotoSansCJK-VF.ttc"]
        }
        Script::Common | Script::Inherited => {
            let code = u32::from(character);
            if matches!(code, 0x1F000..=0x1FAFF | 0x1FC00..=0x1FFFF) {
                &["Noto-COLRv1.ttf"]
            } else if matches!(
                code,
                0x2190..=0x22FF | 0x27C0..=0x27EF | 0x2980..=0x2AFF | 0x1D400..=0x1D7FF
            ) {
                &[
                    "NotoSansMath-Regular.ttf",
                    "NotoSansSymbols[wght].ttf",
                    "NotoSansSymbols2-Regular.ttf",
                ]
            } else if matches!(
                code,
                0x2300..=0x26FF | 0x2700..=0x27BF | 0x2B00..=0x2BFF | 0x1F000..=0x1F02F
            ) {
                &["NotoSansSymbols[wght].ttf", "NotoSansSymbols2-Regular.ttf"]
            } else {
                &[]
            }
        }
        _ => &[],
    }
}

pub(crate) fn browser_text_demands_emoji(text: &str) -> bool {
    crate::text_break::graphemes(text).any(browser_grapheme_demands_emoji)
}

fn browser_grapheme_demands_emoji(grapheme: &str) -> bool {
    use unicode_properties::{EmojiStatus, UnicodeEmoji};
    if grapheme.contains('\u{fe0e}') {
        return false;
    }
    let emoji_sequence = grapheme.contains('\u{fe0f}')
        || grapheme.contains('\u{20e3}')
        || (grapheme.contains('\u{200d}') && grapheme.chars().any(char::is_emoji_char));
    emoji_sequence
        || grapheme.chars().any(|character| {
            matches!(
                character.emoji_status(),
                EmojiStatus::EmojiPresentation
                    | EmojiStatus::EmojiPresentationAndModifierBase
                    | EmojiStatus::EmojiPresentationAndEmojiComponent
                    | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
            )
        })
}

pub(crate) fn browser_emoji_runs<'text, 'attrs>(
    text: &'text str,
    attrs: &Attrs<'attrs>,
    enabled: bool,
) -> Vec<(&'text str, Attrs<'attrs>)> {
    if !enabled || !browser_text_demands_emoji(text) {
        return vec![(text, attrs.clone())];
    }
    let mut runs = Vec::new();
    let mut start = 0;
    let mut emoji = None;
    for (offset, grapheme) in crate::text_break::grapheme_indices(text) {
        let next = browser_grapheme_demands_emoji(grapheme);
        if emoji.is_some_and(|current| current != next) {
            let current = emoji.unwrap_or(false);
            let run_attrs = if current {
                attrs.clone().family(Family::Name("Noto Color Emoji"))
            } else {
                attrs.clone()
            };
            if let Some(run) = text.get(start..offset) {
                runs.push((run, run_attrs));
            }
            start = offset;
        }
        emoji = Some(next);
    }
    let run_attrs = if emoji.unwrap_or(false) {
        attrs.clone().family(Family::Name("Noto Color Emoji"))
    } else {
        attrs.clone()
    };
    if let Some(run) = text.get(start..) {
        runs.push((run, run_attrs));
    }
    runs
}

#[cfg(test)]
thread_local! {
    pub(crate) static FONT_SYSTEM_CONSTRUCTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// This crate's unit tests get the checked-in controlled catalog instead of
/// the host's fonts, so their shaping and pixels never depend on which fonts
/// the machine running them has installed.
#[cfg(test)]
pub fn configured_font_system() -> FontSystem {
    FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(count.get() + 1));
    crate::font_catalog::fixture_font_system()
}

thread_local! {
    static LAYOUT_RENDERERS: std::cell::RefCell<Vec<TextRenderer>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f` with a layout-only renderer at `text_scale`, for widgets that
/// must measure in `apply_layout`, which is handed no renderer. Building one
/// loads every installed font (tens of milliseconds) and layout runs every
/// frame an animation is live, so renderers are pooled per thread; a nested
/// call takes its own from the pool rather than borrowing the outer one.
pub fn with_layout_renderer<R>(text_scale: f32, f: impl FnOnce(&mut TextRenderer) -> R) -> R {
    let mut renderer = LAYOUT_RENDERERS
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_default();
    renderer.set_text_scale(text_scale);
    let out = f(&mut renderer);
    LAYOUT_RENDERERS.with(|pool| pool.borrow_mut().push(renderer));
    out
}

/// The font database a host built for this process, when its fonts do not
/// come from the system: a browser page has no system fonts, so a renderer
/// made without one would shape with none. Set by
/// [`TextRenderer::with_browser_font_system`] and each optional font it
/// installs.
static HOST_FONT_DATABASE: std::sync::RwLock<Option<(String, cosmic_text::fontdb::Database)>> =
    std::sync::RwLock::new(None);

fn share_host_font_database(font_system: &FontSystem) {
    *HOST_FONT_DATABASE.write_recover() =
        Some((font_system.locale().to_owned(), font_system.db().clone()));
}

/// Build the process's font system: the database a host built for it when
/// there is one, otherwise the product font catalog with the system's fonts
/// behind it and Tilo's script fallback (Android copies its catalog out of
/// the APK first). A binary run from a checkout, a screenshot and the Tilo
/// image therefore all set text in the same faces, whether or not the
/// machine has them installed. Only when no catalog loads does it fall back
/// to the system's fonts alone with the generic families pinned to the
/// product names.
#[cfg(not(test))]
pub fn configured_font_system() -> FontSystem {
    let host = HOST_FONT_DATABASE.read_recover().clone();
    if let Some((locale, database)) = host {
        return crate::font_catalog::system_over(locale, database);
    }
    #[cfg(all(target_os = "android", feature = "native"))]
    if let Some(font_system) =
        crate::font_catalog::android_font_system(crate::i18n::I18nManager::current_locale())
    {
        return font_system;
    }
    if let Some(font_system) =
        crate::font_catalog::bundled_font_system(crate::i18n::I18nManager::current_locale())
    {
        return font_system;
    }
    let (locale, database) = SYSTEM_FONT_DATABASE.get_or_init(|| {
        let mut font_system = FontSystem::new();
        let database = font_system.db_mut();
        crate::font_catalog::drop_duplicate_faces(database);
        database.set_sans_serif_family(UI_SANS_FAMILY);
        database.set_serif_family(UI_SERIF_FAMILY);
        database.set_monospace_family(UI_MONOSPACE_FAMILY);
        (font_system.locale().to_owned(), font_system.db().clone())
    });
    FontSystem::new_with_locale_and_db(locale.clone(), database.clone())
}

/// Read the product font catalog into memory and set every renderer this
/// process makes in it from now on: for a process about to lose the
/// filesystem (a confined app worker) whose views still lay out text.
/// Returns whether a catalog loaded.
#[cfg(not(test))]
pub fn hold_catalog_fonts_in_memory() -> bool {
    let Some(font_system) =
        crate::font_catalog::catalog_in_memory(&crate::i18n::I18nManager::current_locale())
    else {
        return false;
    };
    share_host_font_database(&font_system);
    true
}

/// Unit tests already set text in the fixture catalog, held in memory.
#[cfg(test)]
pub fn hold_catalog_fonts_in_memory() -> bool {
    true
}

/// Build the font database [`configured_font_system`] copies into every
/// renderer now, and throw the font system away: a native launch runs this
/// in the background from process start, so the main thread's first
/// renderer finds the catalog verified and every font file read instead of
/// doing both itself. The database is a process-wide `OnceLock`, so
/// a renderer asked for meanwhile waits for this one: nothing is loaded
/// twice, and it is the database the main thread would have built.
pub fn prepare_font_database() {
    drop(configured_font_system());
}

/// The system's fonts, scanned once per process. A scan parses every
/// installed font file, and each renderer would otherwise repeat it.
#[cfg(not(test))]
static SYSTEM_FONT_DATABASE: std::sync::OnceLock<(String, cosmic_text::fontdb::Database)> =
    std::sync::OnceLock::new();

pub(crate) fn balance_buffer(buffer: &mut Buffer, font_system: &mut FontSystem, max_width: f32) {
    if max_width <= 0.0 {
        return;
    }
    let target_lines = buffer.layout_runs().count();
    if target_lines <= 1 {
        return;
    }
    let mut low = 0.0f32;
    let mut high = max_width;
    for _ in 0..12 {
        let candidate = (low + high) * 0.5;
        buffer.set_size(Some(candidate), None);
        buffer.shape_until_scroll(font_system, false);
        if buffer.layout_runs().count() > target_lines {
            low = candidate;
        } else {
            high = candidate;
        }
    }
    buffer.set_size(Some(high.ceil().min(max_width)), None);
    buffer.shape_until_scroll(font_system, false);
}

fn is_opening_punctuation(text: &str) -> bool {
    text.chars().next().is_some_and(|character| {
        matches!(
            character,
            '“' | '‘' | '"' | '\'' | '(' | '[' | '{' | '«' | '‹' | '「' | '『'
        )
    })
}

fn is_closing_punctuation(text: &str) -> bool {
    text.chars().next().is_some_and(|character| {
        matches!(
            character,
            '”' | '’'
                | '"'
                | '\''
                | ')'
                | ']'
                | '}'
                | '»'
                | '›'
                | ','
                | '.'
                | ':'
                | ';'
                | '!'
                | '?'
                | '、'
                | '。'
                | '」'
                | '』'
        )
    })
}

/// The CSS width keyword nearest a `wdth` axis value: the width a run is
/// shaped at (cosmic-text takes widths as keywords).
pub(crate) fn stretch_for_width(width: f32) -> cosmic_text::Stretch {
    use cosmic_text::Stretch;
    const WIDTHS: [(f32, Stretch); 9] = [
        (50.0, Stretch::UltraCondensed),
        (62.5, Stretch::ExtraCondensed),
        (75.0, Stretch::Condensed),
        (87.5, Stretch::SemiCondensed),
        (100.0, Stretch::Normal),
        (112.5, Stretch::SemiExpanded),
        (125.0, Stretch::Expanded),
        (150.0, Stretch::ExtraExpanded),
        (200.0, Stretch::UltraExpanded),
    ];
    WIDTHS
        .iter()
        .min_by(|a, b| (a.0 - width).abs().total_cmp(&(b.0 - width).abs()))
        .map_or(Stretch::Normal, |&(_, stretch)| stretch)
}

/// Where a laid-out glyph is painted: its pen position (`pen_x`, already
/// moved for hanging punctuation) and baseline plus the shaper's GPOS offset.
///
/// cosmic-text keeps that offset apart from the pen position, in em units and
/// with y pointing up. Complex scripts position mark and conjunct glyphs with
/// it — Noto Sans Bengali moves parts of নমস্কার and বিশ্ব by more than half
/// an em — so painting the bare pen position breaks them apart. Carets and
/// hit-testing keep using the pen position.
pub(crate) fn glyph_paint_origin(
    glyph: &cosmic_text::LayoutGlyph,
    pen_x: f32,
    line_y: f32,
) -> (f32, f32) {
    (
        pen_x + glyph.font_size * glyph.x_offset,
        line_y + glyph.y - glyph.font_size * glyph.y_offset,
    )
}

pub(crate) fn hanging_glyph_x(
    run: &cosmic_text::LayoutRun<'_>,
    glyph: &cosmic_text::LayoutGlyph,
    hanging: HangingPunctuation,
) -> f32 {
    let text = run.text.get(glyph.start..glyph.end).unwrap_or_default();
    if hanging.first
        && run
            .glyphs
            .first()
            .is_some_and(|first| first.start == glyph.start && first.end == glyph.end)
        && is_opening_punctuation(text)
    {
        glyph.x - glyph.w * 0.5
    } else if hanging.last
        && run
            .glyphs
            .last()
            .is_some_and(|last| last.start == glyph.start && last.end == glyph.end)
        && is_closing_punctuation(text)
    {
        glyph.x + glyph.w * 0.5
    } else {
        glyph.x
    }
}

/// Advance reserved for a visible discretionary U+2010 in this exact style.
pub(crate) fn discretionary_hyphen_advance(font_system: &mut FontSystem, style: &TextStyle) -> f32 {
    let mut buffer = Buffer::new(
        font_system,
        Metrics::new(style.size, style.line_height_px()),
    );
    let attrs = style.cosmic_attrs();
    buffer.set_text("\u{2010}", &attrs, Shaping::Advanced, None);
    buffer.set_size(None, None);
    buffer.set_wrap(Wrap::None);
    buffer.shape_until_scroll(font_system, false);
    buffer
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0f32, f32::max)
}

/// Named weight tiers mapping 1:1 to [`cosmic_text::fontdb::Weight`]'s
/// numeric constants (Tailwind's font-medium/semibold/bold), so call sites
/// read declaratively instead of passing raw u16s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TextWeight {
    /// 400 — body / subtitle / explanatory text. Default.
    #[default]
    Regular,
    /// 500 — most chrome labels (buttons, secondary titles).
    Medium,
    /// 600 — active chrome titles (tab titles, workspace-name pill).
    Semibold,
    /// 700 — h1/h2, clock, numeric monospace values.
    Bold,
}

impl TextWeight {
    pub const fn to_u16(self) -> u16 {
        match self {
            TextWeight::Regular => 400,
            TextWeight::Medium => 500,
            TextWeight::Semibold => 600,
            TextWeight::Bold => 700,
        }
    }

    pub const fn to_cosmic_weight(self) -> Weight {
        Weight(self.to_u16())
    }
}

/// A font family to shape with. The three generic classes resolve to the
/// system's default face for that class; `Named` targets a specific
/// installed family by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum FontChoice {
    #[default]
    SansSerif,
    Serif,
    Monospace,
    Named(String),
}

/// Paragraph hyphenation policy. It is deliberately opt-in: short UI labels
/// and identifiers should never acquire language-dependent line endings, while
/// reading text can request TeX/Liang patterns with a BCP-47 language tag.
/// Application-supplied discretionary hyphenation. Returned offsets are in
/// the original UTF-8 byte space; invalid, edge, and non-character boundaries
/// are ignored by the renderer.
pub trait Hyphenator: Send + Sync {
    fn breaks(&self, text: &str) -> Vec<usize>;

    /// Stable identity for shape/measurement caches. Change this value when
    /// the algorithm, dictionary, or user settings change.
    fn cache_key(&self) -> u64;
}

#[derive(Clone, Default)]
pub enum Hyphenation {
    #[default]
    None,
    /// Automatically insert discretionary breaks for this BCP-47 language.
    Auto(String),
    /// Use an application-supplied algorithm without changing the source
    /// string or replacing the text widget.
    Custom(Arc<dyn Hyphenator>),
}

impl std::fmt::Debug for Hyphenation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Auto(language) => formatter.debug_tuple("Auto").field(language).finish(),
            Self::Custom(hyphenator) => formatter
                .debug_tuple("Custom")
                .field(&hyphenator.cache_key())
                .finish(),
        }
    }
}

impl PartialEq for Hyphenation {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) => true,
            (Self::Auto(left), Self::Auto(right)) => left == right,
            (Self::Custom(left), Self::Custom(right)) => left.cache_key() == right.cache_key(),
            _ => false,
        }
    }
}

impl Eq for Hyphenation {}

impl std::hash::Hash for Hyphenation {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::None => 0_u8.hash(state),
            Self::Auto(language) => {
                1_u8.hash(state);
                language.hash(state);
                // Layouts made before the patterns' code arrived (where it
                // loads on first use) have no hyphens: make them again.
                crate::text_break::hyphenation_generation().hash(state);
            }
            Self::Custom(hyphenator) => {
                2_u8.hash(state);
                hyphenator.cache_key().hash(state);
            }
        }
    }
}

/// A four-byte OpenType feature or variation tag (`kern`, `dlig`, `opsz`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpenTypeTag(pub [u8; 4]);

impl OpenTypeTag {
    pub const KERNING: Self = Self(*b"kern");
    pub const STANDARD_LIGATURES: Self = Self(*b"liga");
    pub const DISCRETIONARY_LIGATURES: Self = Self(*b"dlig");
    pub const SMALL_CAPS: Self = Self(*b"smcp");
    pub const ALL_SMALL_CAPS: Self = Self(*b"c2sc");
    pub const TABULAR_NUMERALS: Self = Self(*b"tnum");
    pub const PROPORTIONAL_NUMERALS: Self = Self(*b"pnum");
    pub const OLDSTYLE_NUMERALS: Self = Self(*b"onum");
    pub const LINING_NUMERALS: Self = Self(*b"lnum");
    pub const WEIGHT: Self = Self(*b"wght");
    pub const WIDTH: Self = Self(*b"wdth");
    pub const OPTICAL_SIZE: Self = Self(*b"opsz");

    pub const fn new(tag: [u8; 4]) -> Self {
        Self(tag)
    }
}

/// What a run of text *is*, so the toolkit can pick its OpenType features.
///
/// Applications never name a feature: they say what the text is (or the
/// widget knows it) and the features are resolved against the face the text
/// is actually set in, applying only what that face supports.
///
/// | Kind | Features, where the face has them |
/// |---|---|
/// | `Prose` | none |
/// | `Numeric` | tabular lining figures (`tnum`, `lnum`) |
/// | `Secret` | tabular figures, slashed or dotted zero (`zero`), distinct I and l |
/// | `Identifier` | distinct I and l |
/// | `Url` | slashed or dotted zero, distinct I and l |
/// | `Code` | none; set in the mono face |
///
/// Identifiers inside `Prose` (links, email addresses, mentions, room
/// aliases) are found by [`crate::text_detect`] and set as `Url` or
/// `Identifier` on their own. A [`TextStyle`] marked
/// [`TextStyle::on_security_surface`], and all text while the person's
/// "Always distinguish similar letters" setting is on, gets the distinct
/// forms whatever its kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextKind {
    /// Running text. Numbers stay proportional.
    #[default]
    Prose,
    /// Figures that line up in columns or change in place: times, dates,
    /// durations, counts, prices, sizes, progress, clocks.
    Numeric,
    /// Characters someone copies or compares one by one: verification and
    /// recovery codes, TOTP, API keys, hashes, revealed passwords, Wi-Fi and
    /// licence keys.
    Secret,
    /// Names that must not be mistaken for another: usernames, handles,
    /// email addresses, invite codes, room names, other people's file
    /// names.
    Identifier,
    /// Web and other links. An identifier that also sets the zero apart:
    /// a look-alike domain (`paypa1.com`, `g00gle.com`) is a phishing
    /// vector.
    Url,
    /// Source code and terminal text, in the mono face.
    Code,
}

/// The features that pick a figure style: spacing and form.
pub(crate) const FIGURE_TAGS: [OpenTypeTag; 4] = [
    OpenTypeTag::TABULAR_NUMERALS,
    OpenTypeTag::PROPORTIONAL_NUMERALS,
    OpenTypeTag::OLDSTYLE_NUMERALS,
    OpenTypeTag::LINING_NUMERALS,
];

/// One explicit OpenType substitution/positioning setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpenTypeFeature {
    pub tag: OpenTypeTag,
    pub value: u32,
}

/// Numeric glyph style requested from an OpenType face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NumericStyle {
    ProportionalLining,
    ProportionalOldstyle,
    TabularLining,
    TabularOldstyle,
}

/// One design-space coordinate for a variable font.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontVariation {
    pub tag: OpenTypeTag,
    pub value: f32,
}

/// Whether a variable font's `opsz` axis follows the rendered font size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum OpticalSizing {
    #[default]
    Auto,
    None,
    Value(f32),
}

/// The ground text is set on, as far as the face is concerned. Light ink on a
/// dark ground loses fine hairlines first, so a family drawn in separate
/// optical cuts keeps its sturdier text cut to larger sizes there
/// ([`OpticalSizing::Auto`]). A theme's roles carry its content ground; a
/// surface that paints its own ground (a book's page) says which it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextGround {
    #[default]
    Light,
    Dark,
}

impl TextGround {
    /// The ground `color` is: dark when light ink reads better on it than
    /// dark ink does.
    #[must_use]
    pub fn of(color: Color) -> Self {
        // Black and white contrast equally with a ground of this luminance.
        if crate::color::relative_luminance(color) < 0.179 {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

/// Paragraph alignment, including inter-word justification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ParagraphAlign {
    #[default]
    Start,
    Center,
    End,
    Justified,
}

impl ParagraphAlign {
    pub(crate) const fn to_cosmic(self) -> Option<Align> {
        match self {
            Self::Start => None,
            Self::Center => Some(Align::Center),
            Self::End => Some(Align::End),
            Self::Justified => Some(Align::Justified),
        }
    }
}

/// Line selection policy for a constrained paragraph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextWrapMode {
    #[default]
    Wrap,
    /// Prefer word boundaries and fall back to glyph boundaries when one word
    /// is wider than the available measure.
    WordOrGlyph,
    /// Choose a narrower measure that keeps the same number of lines while
    /// minimizing raggedness, like CSS `text-wrap: balance`.
    Balance,
    /// Source code's soft wrap: a wrapped row hangs under its line's own
    /// indentation, so a continuation never reads as a new, outdented line.
    Indented,
}

/// Optical punctuation protrusion at paragraph edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HangingPunctuation {
    pub first: bool,
    pub last: bool,
}

impl HangingPunctuation {
    pub const BOTH: Self = Self {
        first: true,
        last: true,
    };
}

/// Minimum line counts retained on either side of a page/column break.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fragmentation {
    pub orphans: u8,
    pub widows: u8,
}

impl Default for Fragmentation {
    fn default() -> Self {
        Self {
            orphans: 2,
            widows: 2,
        }
    }
}

impl Hyphenation {
    pub(crate) fn language(&self) -> Option<hypher::Lang> {
        let Self::Auto(tag) = self else {
            return None;
        };
        let parsed = tag.parse::<unic_langid::LanguageIdentifier>().ok()?;
        let code = parsed.language.as_str().as_bytes();
        let [first, second] = code else {
            return None;
        };
        hypher::Lang::from_iso([first.to_ascii_lowercase(), second.to_ascii_lowercase()])
    }

    pub(crate) fn offset_map(&self, text: &str) -> OffsetMap {
        match self {
            Self::Custom(hyphenator) => {
                OffsetMap::for_text_with_breaks(text, None, &hyphenator.breaks(text))
            }
            Self::None | Self::Auto(_) => {
                OffsetMap::for_text_with_hyphenation(text, self.language())
            }
        }
    }

    fn discretionary_breaks(&self, text: &str) -> Vec<usize> {
        match self {
            Self::None => Vec::new(),
            Self::Auto(language) => crate::text_break::discretionary_hyphen_breaks(text, language),
            Self::Custom(hyphenator) => hyphenator.breaks(text),
        }
    }

    fn cache_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

/// The ellipsis [`TextRenderer::elide_to_width`] and its variants append.
pub const ELLIPSIS: char = '\u{2026}';

/// The complete typographic style for a plain text run.
///
/// Keeping these values together is more than API convenience: measurement,
/// wrapping, glyph placement, and painting must all receive the exact same
/// metrics or text will clip, jump, or wrap differently between layout and
/// draw. `line_height` is a multiplier, matching CSS and the values in the
/// theme's typography tokens.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub line_height: f32,
    pub weight: TextWeight,
    pub italic: bool,
    pub family: FontChoice,
    pub hyphenation: Hyphenation,
    /// Additional advance in logical pixels between shaped clusters.
    pub letter_spacing: f32,
    pub font_features: Vec<OpenTypeFeature>,
    pub font_variations: Vec<FontVariation>,
    pub optical_sizing: OpticalSizing,
    pub align: ParagraphAlign,
    pub wrap_mode: TextWrapMode,
    pub hanging_punctuation: HangingPunctuation,
    pub fragmentation: Fragmentation,
    /// What `size` is relative to, and so whether the renderer's
    /// accessibility text scale still applies to it.
    pub scaling: TextScaling,
    /// What the text is; resolved to OpenType features against each face it
    /// is set in.
    pub kind: TextKind,
    /// The ground the text is set on, for the optical cut it is set in.
    pub ground: TextGround,
    /// Set every look-alike (I, l, 1 and 0, O) apart whatever the kind: text
    /// on a security surface ([`Self::on_security_surface`]). The renderer
    /// also sets it on all text while the person's "Always distinguish
    /// similar letters" setting is on.
    pub distinguish: bool,
}

/// What a [`TextStyle`]'s size (and pixel letter spacing) is measured in.
///
/// Text must be scaled exactly once. A theme role is already resolved; a
/// renderer-relative base size is scaled by the renderer; a document's own
/// type is never scaled by an accessibility preference at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextScaling {
    /// A 1× base size. The renderer multiplies it by its accessibility text
    /// scale. [`TextStyle::new`] and the size-based renderer APIs use this.
    #[default]
    Renderer,
    /// A final logical size that already includes the theme's UI and text
    /// scale: every [`crate::theme::Theme::text`] role. Painted as given.
    Resolved,
    /// A document-space size that belongs to the content, not the chrome: a
    /// zoomed canvas, a PDF stamp, a terminal grid. Painted as given and
    /// never grown by the text scale; the content's own zoom scales it.
    Content,
}

impl TextStyle {
    /// Construct regular sans-serif text at a renderer-relative base size
    /// ([`TextScaling::Renderer`]) with the renderer's legacy leading. UI
    /// text takes a role instead: [`crate::theme::Theme::text`].
    pub fn new(size: f32) -> Self {
        Self {
            size,
            line_height: 1.25,
            weight: TextWeight::Regular,
            italic: false,
            family: FontChoice::SansSerif,
            hyphenation: Hyphenation::None,
            letter_spacing: 0.0,
            font_features: Vec::new(),
            font_variations: Vec::new(),
            optical_sizing: OpticalSizing::Auto,
            align: ParagraphAlign::Start,
            wrap_mode: TextWrapMode::Wrap,
            hanging_punctuation: HangingPunctuation::default(),
            fragmentation: Fragmentation::default(),
            scaling: TextScaling::Renderer,
            kind: TextKind::Prose,
            ground: TextGround::Light,
            distinguish: false,
        }
    }

    /// Document-space text at exactly `size` ([`TextScaling::Content`]):
    /// zoomed canvases, PDF stamps, terminal grids. The accessibility text
    /// scale never applies; the content's zoom does, through the transform.
    pub fn content(size: f32) -> Self {
        Self {
            scaling: TextScaling::Content,
            ..Self::new(size)
        }
    }

    #[must_use]
    pub fn with_line_height(mut self, multiplier: f32) -> Self {
        self.line_height = multiplier.max(0.1);
        self
    }

    #[must_use]
    pub fn with_weight(mut self, weight: TextWeight) -> Self {
        self.weight = weight;
        self
    }

    #[must_use]
    pub fn with_italic(mut self, italic: bool) -> Self {
        self.italic = italic;
        self
    }

    #[must_use]
    pub fn with_family(mut self, family: FontChoice) -> Self {
        self.family = family;
        self
    }

    /// Set CSS-like letter spacing in logical pixels.
    #[must_use]
    pub fn with_letter_spacing(mut self, px: f32) -> Self {
        self.letter_spacing = if px.is_finite() { px } else { 0.0 };
        self
    }

    /// Set tracking in em units, converted using this style's font size.
    #[must_use]
    pub fn with_tracking(self, em: f32) -> Self {
        let size = self.size;
        self.with_letter_spacing(em * size)
    }

    /// Say what the text is ([`TextKind`]). `Code` also sets it in the mono
    /// face. `Numeric` and `Secret` decide the figures, so they replace any
    /// figure style set before them (the Reading role's old-style figures).
    #[must_use]
    pub fn with_kind(mut self, kind: TextKind) -> Self {
        self.kind = kind;
        if kind == TextKind::Code {
            self.family = FontChoice::Monospace;
        }
        if matches!(kind, TextKind::Numeric | TextKind::Secret) {
            self.font_features
                .retain(|feature| !FIGURE_TAGS.contains(&feature.tag));
        }
        self
    }

    /// Say what ground the text is set on ([`TextGround`]). Theme roles
    /// carry their theme's; a surface that paints its own ground sets it.
    #[must_use]
    pub fn with_ground(mut self, ground: TextGround) -> Self {
        self.ground = ground;
        self
    }

    /// Text on a security surface — a permission prompt, a link preview, an
    /// invite or join confirmation, identity verification — where every
    /// look-alike letter and figure is set apart, prose included. Toolkit
    /// surfaces of these kinds mark themselves; a custom-painted one marks
    /// its styles here.
    #[must_use]
    pub fn on_security_surface(mut self) -> Self {
        self.distinguish = true;
        self
    }

    #[must_use]
    pub fn with_font_feature(mut self, tag: OpenTypeTag, value: u32) -> Self {
        self.font_features.retain(|feature| feature.tag != tag);
        self.font_features.push(OpenTypeFeature { tag, value });
        self
    }

    #[must_use]
    pub fn with_kerning(self, enabled: bool) -> Self {
        self.with_font_feature(OpenTypeTag::KERNING, u32::from(enabled))
    }

    #[must_use]
    pub fn with_discretionary_ligatures(self, enabled: bool) -> Self {
        self.with_font_feature(OpenTypeTag::DISCRETIONARY_LIGATURES, u32::from(enabled))
    }

    #[must_use]
    pub fn with_small_caps(self, enabled: bool) -> Self {
        self.with_font_feature(OpenTypeTag::SMALL_CAPS, u32::from(enabled))
    }

    #[must_use]
    pub fn with_numeric_style(mut self, style: NumericStyle) -> Self {
        for tag in [
            OpenTypeTag::TABULAR_NUMERALS,
            OpenTypeTag::PROPORTIONAL_NUMERALS,
            OpenTypeTag::OLDSTYLE_NUMERALS,
            OpenTypeTag::LINING_NUMERALS,
        ] {
            self.font_features.retain(|feature| feature.tag != tag);
        }
        let (spacing, form) = match style {
            NumericStyle::ProportionalLining => (
                OpenTypeTag::PROPORTIONAL_NUMERALS,
                OpenTypeTag::LINING_NUMERALS,
            ),
            NumericStyle::ProportionalOldstyle => (
                OpenTypeTag::PROPORTIONAL_NUMERALS,
                OpenTypeTag::OLDSTYLE_NUMERALS,
            ),
            NumericStyle::TabularLining => {
                (OpenTypeTag::TABULAR_NUMERALS, OpenTypeTag::LINING_NUMERALS)
            }
            NumericStyle::TabularOldstyle => (
                OpenTypeTag::TABULAR_NUMERALS,
                OpenTypeTag::OLDSTYLE_NUMERALS,
            ),
        };
        self.font_features.push(OpenTypeFeature {
            tag: spacing,
            value: 1,
        });
        self.font_features.push(OpenTypeFeature {
            tag: form,
            value: 1,
        });
        self
    }

    #[must_use]
    pub fn with_font_variation(mut self, tag: OpenTypeTag, value: f32) -> Self {
        self.font_variations.retain(|axis| axis.tag != tag);
        if value.is_finite() {
            self.font_variations.push(FontVariation { tag, value });
        }
        self
    }

    #[must_use]
    pub fn with_optical_sizing(mut self, optical_sizing: OpticalSizing) -> Self {
        self.optical_sizing = optical_sizing;
        self
    }

    #[must_use]
    pub fn with_align(mut self, align: ParagraphAlign) -> Self {
        self.align = align;
        self
    }

    #[must_use]
    pub fn with_wrap_mode(mut self, wrap_mode: TextWrapMode) -> Self {
        self.wrap_mode = wrap_mode;
        self
    }

    #[must_use]
    pub fn with_hanging_punctuation(mut self, hanging: HangingPunctuation) -> Self {
        self.hanging_punctuation = hanging;
        self
    }

    #[must_use]
    pub fn with_fragmentation(mut self, orphans: u8, widows: u8) -> Self {
        self.fragmentation = Fragmentation {
            orphans: orphans.max(1),
            widows: widows.max(1),
        };
        self
    }

    /// Opt this paragraph into automatic language-aware hyphenation.
    ///
    /// `language` is a BCP-47 tag such as `"en-US"`, `"de"`, or `"pt-BR"`.
    /// Unknown or malformed tags fall back to normal word wrapping, so content
    /// remains readable when no compiled pattern matches.
    #[must_use]
    pub fn with_auto_hyphenation(mut self, language: impl Into<String>) -> Self {
        self.hyphenation = Hyphenation::Auto(language.into());
        self
    }

    /// Use a custom discretionary-break algorithm. The same policy is used
    /// for measurement and painting, and participates in cache identity.
    #[must_use]
    pub fn with_hyphenator(mut self, hyphenator: Arc<dyn Hyphenator>) -> Self {
        self.hyphenation = Hyphenation::Custom(hyphenator);
        self
    }

    pub fn line_height_px(&self) -> f32 {
        self.size * self.line_height
    }

    /// Resolve the integer weight accepted by cosmic-text: the last explicit
    /// `wght` overrides the named tier, rounds to nearest, then clamps to 1..1000.
    /// Painting uses each shaped glyph's resulting weight, including span bold.
    pub(crate) fn effective_weight(&self) -> Weight {
        let value = self
            .font_variations
            .iter()
            .rev()
            .find(|axis| axis.tag == OpenTypeTag::WEIGHT)
            .map_or_else(|| f32::from(self.weight.to_u16()), |axis| axis.value)
            .round()
            .clamp(1.0, 1000.0);
        // Clamped to an integer in 1.0..=1000.0 immediately above, so this
        // narrowing is exact; converting differently would risk drifting the
        // resolved font weight against the pixel-diff golden.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "value is clamped to an integer in 1.0..=1000.0 immediately above"
        )]
        let value = value as u16;
        Weight(value)
    }

    pub(crate) fn resolved_variations(&self) -> Vec<FontVariation> {
        let mut variations = self.font_variations.clone();
        if !variations
            .iter()
            .any(|axis| axis.tag == OpenTypeTag::OPTICAL_SIZE)
        {
            let value = match self.optical_sizing {
                OpticalSizing::Auto => Some(self.size),
                OpticalSizing::None => None,
                OpticalSizing::Value(value) => Some(value),
            };
            if let Some(value) = value {
                variations.push(FontVariation {
                    tag: OpenTypeTag::OPTICAL_SIZE,
                    value,
                });
            }
        }
        variations
    }

    pub(crate) fn cosmic_attrs(&self) -> Attrs<'_> {
        let mut features = CosmicFontFeatures::new();
        for feature in &self.font_features {
            features.set(cosmic_text::FeatureTag::new(&feature.tag.0), feature.value);
        }
        let weight = self.effective_weight();
        let family = match &self.family {
            FontChoice::Named(name) => Family::Name(
                crate::font_catalog::optical::cut(
                    name,
                    self.size,
                    self.optical(),
                    self.italic,
                    weight.0,
                )
                .unwrap_or(name),
            ),
            other => other.to_cosmic(),
        };
        let mut attrs = Attrs::new()
            .weight(weight)
            .family(family)
            .font_features(features);
        if self.letter_spacing != 0.0 {
            attrs = attrs.letter_spacing(self.letter_spacing / self.size.max(0.1));
        }
        if self.italic {
            attrs = attrs.style(FontStyle::Italic);
        }
        // Shaping, measuring and kerning follow the width the glyphs are
        // drawn at (the draw applies `wdth` exactly; shaping takes the
        // nearest CSS width keyword).
        if let Some(width) = self
            .font_variations
            .iter()
            .find(|axis| axis.tag == OpenTypeTag::WIDTH)
        {
            attrs = attrs.stretch(stretch_for_width(width.value));
        }
        attrs
    }

    /// What picks this style's optical cut besides the family, size, style
    /// and weight of each run.
    pub(crate) fn optical(&self) -> crate::font_catalog::optical::Optical {
        crate::font_catalog::optical::Optical {
            sizing: self.optical_sizing,
            ground: self.ground,
        }
    }

    pub(crate) fn advanced_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        self.letter_spacing.to_bits().hash(&mut h);
        self.font_features.hash(&mut h);
        for axis in &self.font_variations {
            axis.tag.hash(&mut h);
            axis.value.to_bits().hash(&mut h);
        }
        match self.optical_sizing {
            OpticalSizing::Auto => 0u8.hash(&mut h),
            OpticalSizing::None => 1u8.hash(&mut h),
            OpticalSizing::Value(value) => {
                2u8.hash(&mut h);
                value.to_bits().hash(&mut h);
            }
        }
        self.align.hash(&mut h);
        self.wrap_mode.hash(&mut h);
        self.hanging_punctuation.hash(&mut h);
        self.fragmentation.hash(&mut h);
        self.kind.hash(&mut h);
        self.ground.hash(&mut h);
        self.distinguish.hash(&mut h);
        h.finish()
    }
}

impl FontChoice {
    pub(crate) fn to_cosmic(&self) -> Family<'_> {
        match self {
            FontChoice::SansSerif => Family::SansSerif,
            FontChoice::Serif => Family::Serif,
            FontChoice::Monospace => Family::Monospace,
            FontChoice::Named(n) => Family::Name(n),
        }
    }

    /// A stable hash of the choice, used as part of the shape-cache key so two
    /// families of the same text/size/weight are cached separately.
    fn cache_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        match self {
            FontChoice::SansSerif => 0u8.hash(&mut h),
            FontChoice::Serif => 1u8.hash(&mut h),
            FontChoice::Monospace => 2u8.hash(&mut h),
            FontChoice::Named(n) => {
                3u8.hash(&mut h);
                n.hash(&mut h);
            }
        }
        h.finish()
    }
}

/// Width/height offered to a pluggable text layout engine.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextLayoutConstraints {
    pub max_width: Option<f32>,
    pub max_height: Option<f32>,
}

/// Replaceable measurement/painting boundary used by [`crate::widget::Text`].
/// Implementations keep localization, accessibility, Taffy integration, scene
/// caching, and inspection supplied by the standard widget. `style` is always
/// renderer-relative: implementations which delegate to [`TextRenderer`] pass
/// it through unchanged. For theme-resolved or fixed-size widgets, `Text`
/// compensates the font size for the renderer's current multiplier before this
/// boundary; renderer-relative letter spacing keeps its established behavior.
pub trait TextLayoutEngine {
    fn measure(
        &mut self,
        renderer: &mut TextRenderer,
        text: &str,
        constraints: TextLayoutConstraints,
        style: &TextStyle,
    ) -> (f32, f32);

    fn draw(
        &mut self,
        renderer: &mut TextRenderer,
        scene: &mut Scene,
        text: &str,
        constraints: TextLayoutConstraints,
        style: &TextStyle,
        color: Color,
        transform: Affine,
    );

    /// Change when internal layout-affecting configuration changes.
    fn revision(&self) -> u64 {
        0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextFlowRegionError {
    TooFewPoints,
    NonFinitePoint,
    EmptyBounds,
}

impl std::fmt::Display for TextFlowRegionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewPoints => {
                formatter.write_str("a text-flow polygon needs at least 3 points")
            }
            Self::NonFinitePoint => formatter.write_str("text-flow polygon points must be finite"),
            Self::EmptyBounds => formatter.write_str("text-flow polygon must have non-zero bounds"),
        }
    }
}

impl std::error::Error for TextFlowRegionError {}

/// Non-rectangular local-coordinate region used for both line fitting and
/// exact scene clipping. Concave polygons may produce several independent
/// spans on one baseline, allowing text to flow around an inset or cutout.
#[derive(Clone, Debug)]
pub struct TextFlowRegion {
    points: Vec<Point>,
    path: BezPath,
    bounds: Rect,
    cache_key: u64,
}

impl TextFlowRegion {
    /// Build a closed polygon. Points are local to the Text widget's origin.
    ///
    /// # Errors
    ///
    /// Returns an error for fewer than three points, non-finite coordinates,
    /// or a polygon whose bounding box has no area.
    pub fn polygon(points: impl IntoIterator<Item = Point>) -> Result<Self, TextFlowRegionError> {
        use std::hash::{Hash, Hasher};

        let points: Vec<_> = points.into_iter().collect();
        if points.len() < 3 {
            return Err(TextFlowRegionError::TooFewPoints);
        }
        if points
            .iter()
            .any(|point| !point.x.is_finite() || !point.y.is_finite())
        {
            return Err(TextFlowRegionError::NonFinitePoint);
        }
        let min_x = points
            .iter()
            .map(|point| point.x)
            .fold(f64::INFINITY, f64::min);
        let max_x = points
            .iter()
            .map(|point| point.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = points
            .iter()
            .map(|point| point.y)
            .fold(f64::INFINITY, f64::min);
        let max_y = points
            .iter()
            .map(|point| point.y)
            .fold(f64::NEG_INFINITY, f64::max);
        let bounds = Rect::new(min_x, min_y, max_x, max_y);
        if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            return Err(TextFlowRegionError::EmptyBounds);
        }
        let mut path = BezPath::new();
        let Some(first) = points.first().copied() else {
            return Err(TextFlowRegionError::TooFewPoints);
        };
        path.move_to(first);
        for point in points.iter().skip(1) {
            path.line_to(*point);
        }
        path.close_path();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for point in &points {
            point.x.to_bits().hash(&mut hasher);
            point.y.to_bits().hash(&mut hasher);
        }
        Ok(Self {
            points,
            path,
            bounds,
            cache_key: hasher.finish(),
        })
    }

    #[must_use]
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    #[must_use]
    pub fn cache_key(&self) -> u64 {
        self.cache_key
    }

    fn spans_at(&self, y: f64, max_width: f64) -> Vec<std::ops::Range<f32>> {
        let mut crossings = Vec::new();
        for (first, second) in self
            .points
            .iter()
            .copied()
            .zip(self.points.iter().copied().cycle().skip(1))
            .take(self.points.len())
        {
            if (first.y <= y && second.y > y) || (second.y <= y && first.y > y) {
                let t = (y - first.y) / (second.y - first.y);
                crossings.push(first.x + t * (second.x - first.x));
            }
        }
        crossings.sort_by(f64::total_cmp);
        crossings
            .chunks_exact(2)
            .filter_map(|pair| {
                let [start, end] = pair else {
                    return None;
                };
                let start = start.max(0.0).min(max_width);
                let end = end.max(0.0).min(max_width);
                (end > start).then(|| float_range(start, end))
            })
            .collect()
    }

    fn spans_for_band(&self, top: f32, height: f32, max_width: f32) -> Vec<std::ops::Range<f32>> {
        let epsilon = f64::from(height.max(1.0)) * 0.01;
        let samples = [
            f64::from(top) + epsilon,
            f64::from(top + height * 0.5),
            f64::from(top + height) - epsilon,
        ];
        let mut spans = self.spans_at(samples[0], f64::from(max_width));
        for y in samples.into_iter().skip(1) {
            spans = intersect_spans(&spans, &self.spans_at(y, f64::from(max_width)));
        }
        spans
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "validated finite flow-region coordinates are ordinary logical pixels"
)]
fn float_range(start: f64, end: f64) -> std::ops::Range<f32> {
    start as f32..end as f32
}

fn intersect_spans(
    left: &[std::ops::Range<f32>],
    right: &[std::ops::Range<f32>],
) -> Vec<std::ops::Range<f32>> {
    let mut intersections = Vec::new();
    for a in left {
        for b in right {
            let start = a.start.max(b.start);
            let end = a.end.min(b.end);
            if end > start {
                intersections.push(start..end);
            }
        }
    }
    intersections
}

#[derive(Clone, Debug)]
struct FlowLine {
    text: String,
    x: f32,
    y: f32,
}

#[derive(Clone, Debug, Default)]
struct FlowLayout {
    lines: Vec<FlowLine>,
    width: f32,
    height: f32,
}

/// Built-in layout engine that fits text into and clips it against a polygon.
#[derive(Clone, Debug)]
pub struct PolygonTextLayout {
    region: TextFlowRegion,
}

impl PolygonTextLayout {
    #[must_use]
    pub fn new(region: TextFlowRegion) -> Self {
        Self { region }
    }

    #[must_use]
    pub fn region(&self) -> &TextFlowRegion {
        &self.region
    }

    fn layout(
        &self,
        renderer: &mut TextRenderer,
        text: &str,
        constraints: TextLayoutConstraints,
        style: &TextStyle,
    ) -> FlowLayout {
        use std::collections::{BTreeMap, HashSet};

        if text.is_empty() {
            return FlowLayout::default();
        }
        let max_width = constraints
            .max_width
            .unwrap_or_else(|| f32_of_f64(self.region.bounds.x1))
            .max(0.0);
        let max_height = constraints
            .max_height
            .unwrap_or_else(|| f32_of_f64(self.region.bounds.y1))
            .max(0.0);
        let line_height = style.line_height_px().max(1.0);
        let mut opportunities = BTreeMap::new();
        for (byte, kind) in crate::text_break::line_break_opportunities(text) {
            opportunities.insert(byte, kind == LineBreakKind::Mandatory);
        }
        let hyphen_breaks: HashSet<_> = style
            .hyphenation
            .discretionary_breaks(text)
            .into_iter()
            .filter(|byte| *byte > 0 && *byte < text.len() && text.is_char_boundary(*byte))
            .collect();
        for byte in &hyphen_breaks {
            opportunities.entry(*byte).or_insert(false);
        }

        let mut layout = FlowLayout::default();
        let mut source = 0_usize;
        let mut y = f32_of_f64(self.region.bounds.y0.max(0.0));
        let bottom = max_height.min(f32_of_f64(self.region.bounds.y1));
        while source < text.len() && y + line_height <= bottom + f32::EPSILON {
            let spans = self.region.spans_for_band(y, line_height, max_width);
            let mut forced_next_line = false;
            for span in spans {
                source = skip_flow_whitespace(text, source);
                if source >= text.len() {
                    break;
                }
                let available = span.end - span.start;
                let mut best = None;
                for (&end, &mandatory) in opportunities.range(source.saturating_add(1)..) {
                    let raw = text.get(source..end).unwrap_or_default();
                    let visible = raw.trim_end_matches(char::is_whitespace);
                    let hyphen = hyphen_breaks.contains(&end) && !mandatory;
                    let mut candidate = visible.to_owned();
                    if hyphen && !candidate.is_empty() {
                        candidate.push('\u{2010}');
                    }
                    let width = renderer.measure_style(&candidate, None, style).0;
                    if width <= available || best.is_none() {
                        if width <= available {
                            best = Some((end, candidate, width, mandatory));
                        }
                        if mandatory {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                let chosen =
                    best.or_else(|| force_flow_fragment(renderer, text, source, available, style));
                let Some((end, fragment, width, mandatory)) = chosen else {
                    break;
                };
                if !fragment.is_empty() {
                    layout.width = layout.width.max(span.start + width);
                    layout.lines.push(FlowLine {
                        text: fragment,
                        x: span.start,
                        y,
                    });
                }
                source = end;
                if mandatory {
                    forced_next_line = true;
                    break;
                }
            }
            layout.height = (y + line_height).max(layout.height);
            y += line_height;
            if forced_next_line {
                source = skip_flow_whitespace(text, source);
            }
        }
        layout
    }
}

impl TextLayoutEngine for PolygonTextLayout {
    fn measure(
        &mut self,
        renderer: &mut TextRenderer,
        text: &str,
        constraints: TextLayoutConstraints,
        style: &TextStyle,
    ) -> (f32, f32) {
        let layout = self.layout(renderer, text, constraints, style);
        (layout.width, layout.height)
    }

    fn draw(
        &mut self,
        renderer: &mut TextRenderer,
        scene: &mut Scene,
        text: &str,
        constraints: TextLayoutConstraints,
        style: &TextStyle,
        color: Color,
        transform: Affine,
    ) {
        let layout = self.layout(renderer, text, constraints, style);
        scene.push_clip_layer(Fill::NonZero, transform, &self.region.path);
        for line in layout.lines {
            let line_transform =
                transform.pre_translate((f64::from(line.x), f64::from(line.y)).into());
            renderer.draw_style(scene, &line.text, color, line_transform, None, style);
        }
        scene.pop_layer();
    }

    fn revision(&self) -> u64 {
        self.region.cache_key()
    }
}

fn skip_flow_whitespace(text: &str, mut byte: usize) -> usize {
    while let Some(character) = text.get(byte..).and_then(|rest| rest.chars().next()) {
        if !character.is_whitespace() {
            break;
        }
        byte = byte.saturating_add(character.len_utf8());
    }
    byte
}

fn force_flow_fragment(
    renderer: &mut TextRenderer,
    text: &str,
    source: usize,
    available: f32,
    style: &TextStyle,
) -> Option<(usize, String, f32, bool)> {
    let tail = text.get(source..)?;
    let mut best = None;
    for (offset, character) in tail.char_indices() {
        let end = source
            .saturating_add(offset)
            .saturating_add(character.len_utf8());
        let fragment = text.get(source..end)?.to_owned();
        let width = renderer.measure_style(&fragment, None, style).0;
        if width <= available || best.is_none() {
            best = Some((end, fragment, width, false));
        } else {
            break;
        }
    }
    best
}

// Fixed metadata is bounded independently from variable retained payloads.
const MEASURE_CACHE_LIMIT: usize = 4096;
const MEASURE_CACHE_BYTES: usize = 2 * 1024 * 1024;
const SHAPE_CACHE_BYTES: usize = 16 * 1024 * 1024;

/// One contiguous span of glyphs sharing a single font face, already
/// positioned in the text block's local coordinate space. Painting is then
/// just `scene.draw_glyphs` with the caller's colour + transform.
#[derive(Clone)]
struct GlyphRun {
    face_id: cosmic_text::fontdb::ID,
    font_weight: Weight,
    /// The size the run was shaped at: the style's, or a matched fallback's
    /// x-height-matched size.
    size: f32,
    glyphs: Vec<vello::Glyph>,
    normalized_coords: Vec<vello::NormalizedCoord>,
}

fn glyph_run_bytes(runs: &Vec<GlyphRun>) -> usize {
    runs.iter().fold(
        std::mem::size_of::<Vec<GlyphRun>>()
            .saturating_add(2 * std::mem::size_of::<usize>())
            .saturating_add(
                runs.capacity()
                    .saturating_mul(std::mem::size_of::<GlyphRun>()),
            ),
        |bytes, run| {
            bytes
                .saturating_add(
                    run.glyphs
                        .capacity()
                        .saturating_mul(std::mem::size_of::<vello::Glyph>()),
                )
                .saturating_add(
                    run.normalized_coords
                        .capacity()
                        .saturating_mul(std::mem::size_of::<vello::NormalizedCoord>()),
                )
        },
    )
}

/// Consecutive rich-text glyphs that share a face, weight, size, colour and
/// baseline, flushed as one `draw_glyphs` call.
struct GlyphBatch<'a> {
    face_id: cosmic_text::fontdb::ID,
    glyphs: &'a [cosmic_text::LayoutGlyph],
    size: f32,
    color: Color,
    line_y: f32,
    variations: &'a [FontVariation],
}

/// Shaped glyph geometry plus the `(width, height)` measure derived from the
/// same cosmic-text buffer, so a layout miss can reuse a prior paint (and a
/// layout warm can satisfy the paint that follows) without reshaping.
struct ShapedText {
    runs: Arc<Vec<GlyphRun>>,
    size: (f32, f32),
}

struct PreparedBuffer {
    buffer: Buffer,
    hyphen_width: f32,
}

/// Upper bound on distinct shaped runs retained across frames. Static
/// chrome labels warm this once; every subsequent frame is a pure `Arc`
/// clone instead of a cosmic-text shape.
const SHAPE_CACHE_LIMIT: usize = 4096;

#[derive(Hash, PartialEq, Eq)]
struct InkFontKey {
    blob: u64,
    index: u32,
    face: cosmic_text::fontdb::ID,
    weight: u16,
    size: u32,
    coords: Vec<vello::NormalizedCoord>,
}

/// Control points enclose every unhinted Bézier segment, including overhangs.
#[derive(Default)]
struct InkBoundsPen {
    bounds: Option<Rect>,
    invalid: bool,
}

fn outline_ink_bounds(
    font: &Font,
    glyph_id: u16,
    size: f32,
    coords: &[vello::NormalizedCoord],
) -> Option<Rect> {
    use skrifa::instance::{NormalizedCoord, Size};
    use skrifa::outline::DrawSettings;
    use skrifa::{FontRef, GlyphId, MetadataProvider, Tag};
    let face = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    // Outline ink cannot bound layered color or bitmap glyphs.
    if [
        b"COLR", b"CPAL", b"CBDT", b"CBLC", b"sbix", b"SVG ", b"EBDT", b"EBLC",
    ]
    .iter()
    .any(|tag| face.table_data(Tag::new(tag)).is_some())
    {
        return None;
    }
    let location: Vec<_> = coords
        .iter()
        .map(|v| NormalizedCoord::from_bits(*v))
        .collect();
    let outline = face
        .outline_glyphs()
        .get(GlyphId::new(u32::from(glyph_id)))?;
    let mut pen = InkBoundsPen::default();
    outline
        .draw(
            DrawSettings::unhinted(Size::new(size), location.as_slice()),
            &mut pen,
        )
        .ok()?;
    if pen.invalid {
        None
    } else {
        Some(pen.bounds.unwrap_or(Rect::ZERO))
    }
}

/// One shaped glyph as geometry: see [`TextRenderer::glyph_outlines`].
#[derive(Clone, Debug)]
pub struct GlyphOutline {
    /// The outline around the glyph's origin, y down, in the style's units.
    pub path: BezPath,
    /// The glyph's origin on its baseline, where [`TextRenderer::draw_style`]
    /// paints it.
    pub origin: Point,
    /// How far the pen moves on to the next glyph.
    pub advance: f64,
    /// The bytes of the text the glyph shows.
    pub cluster: std::ops::Range<usize>,
}

/// A glyph's unhinted outline at `size`, y down; `None` for a colour or
/// bitmap glyph, which has no outline to draw.
fn outline_path(
    font: &Font,
    glyph_id: u16,
    size: f32,
    coords: &[vello::NormalizedCoord],
) -> Option<BezPath> {
    use skrifa::instance::{NormalizedCoord, Size};
    use skrifa::outline::DrawSettings;
    use skrifa::{FontRef, GlyphId, MetadataProvider, Tag};
    let face = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    if [b"COLR", b"CBDT", b"sbix", b"SVG "]
        .iter()
        .any(|tag| face.table_data(Tag::new(tag)).is_some())
    {
        return None;
    }
    let location: Vec<_> = coords
        .iter()
        .map(|v| NormalizedCoord::from_bits(*v))
        .collect();
    let outline = face
        .outline_glyphs()
        .get(GlyphId::new(u32::from(glyph_id)))?;
    let mut pen = PathPen(BezPath::new());
    outline
        .draw(
            DrawSettings::unhinted(Size::new(size), location.as_slice()),
            &mut pen,
        )
        .ok()?;
    Some(pen.0)
}

/// Draws a glyph outline into a path, flipping the font's y-up to y-down.
struct PathPen(BezPath);

impl skrifa::outline::OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), -f64::from(y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), -f64::from(y)));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.quad_to(
            (f64::from(cx), -f64::from(cy)),
            (f64::from(x), -f64::from(y)),
        );
    }
    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        self.0.curve_to(
            (f64::from(ax), -f64::from(ay)),
            (f64::from(bx), -f64::from(by)),
            (f64::from(x), -f64::from(y)),
        );
    }
    fn close(&mut self) {
        self.0.close_path();
    }
}

impl InkBoundsPen {
    fn point(&mut self, x: f32, y: f32) {
        if !x.is_finite() || !y.is_finite() {
            self.invalid = true;
            return;
        }
        let point = Point::new(f64::from(x), -f64::from(y));
        self.bounds = Some(
            self.bounds
                .map_or_else(|| Rect::from_points(point, point), |b| b.union_pt(point)),
        );
    }
}

impl skrifa::outline::OutlinePen for InkBoundsPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.point(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.point(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.point(cx, cy);
        self.point(x, y);
    }
    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        self.point(ax, ay);
        self.point(bx, by);
        self.point(x, y);
    }
    fn close(&mut self) {}
}

pub struct TextRenderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    /// `(text, size, max_width, weight)` → measured `(width, height)`.
    /// Uncached cosmic-text re-shape per frame measured ~7.5ms in debug
    /// across a 50-widget tree; most widgets re-measure the same static
    /// strings every frame, so caching drops that to a hash lookup.
    measure_cache: TextCache<(f32, f32)>,
    /// Bumped by [`Self::bump_generation`] when locale or fonts change.
    /// Generation participates in identity; bumping it also clears retained entries.
    generation: u64,
    /// Identity of widget-owned layouts, unique across renderers and font epochs.
    layout_epoch: Arc<()>,
    /// "Always distinguish similar letters" ([`Self::set_distinct_letters`]).
    /// Mirrored into the shaping thread's state whenever the renderer
    /// measures, paints or hands out its layout epoch, so layouts shaped
    /// outside it ([`crate::richtext::RichTextLayout`]) follow it too.
    distinct_letters: bool,
    hits: u64,
    misses: u64,
    /// `(text, size, max_width, weight, style)` → shaped + positioned glyph
    /// runs. An `Arc` value lets a lookup hand the runs back to the paint
    /// loop without cloning the glyph vecs or holding a borrow on the cache
    /// while the font system is accessed.
    shape_cache: TextCache<ShapedText>,
    /// Surfaced via [`Self::take_shape_stats`] so the frame profiler
    /// (`TILO_UI_PERF`) can attribute draw-phase time between cosmic-text
    /// and Vello encoding.
    shape_hits: u64,
    shape_misses: u64,
    shape_ns: u128,
    /// App-provided accessibility text multiplier. Kept on the renderer so
    /// custom canvas widgets that use the low-level text API can honour the
    /// same scale as token-based controls without duplicating every draw call.
    text_scale: f32,
    /// Bounded, generation-cleared cache; None means conservatively draw.
    // The inner key is a bounded font glyph ID; avoid SipHash on every glyph.
    // Font/variation identity retains the randomized outer map.
    ink_bounds: HashMap<InkFontKey, rustc_hash::FxHashMap<u16, Option<Rect>>>,
    ink_bounds_entries: usize,
    /// Missing canonical browser assets observed by actual cosmic-text
    /// shaping. Native/full-catalog renderers leave this disabled.
    browser_font_demands: Option<BTreeSet<String>>,
    /// Successfully installed optional assets. Candidate fallback fonts are
    /// requested one at a time so one glyph never downloads unused families.
    browser_font_assets: HashSet<String>,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve a per-glyph rich-text colour override, falling back to `default`.
fn resolve_rich_color(default: Color, color: Option<cosmic_text::Color>) -> Color {
    color.map_or(default, |c| Color::from_rgb8(c.r(), c.g(), c.b()))
}

/// Highlight bands paint behind glyphs so the ink reads on top.
fn draw_rich_backgrounds(
    scene: &mut Scene,
    layout: &crate::richtext::RichTextLayout,
    transform: Affine,
) {
    for &(s, e, [br, bg, bb]) in &layout.backgrounds {
        let band = Color::from_rgb8(br, bg, bb);
        for (x0, y0, x1, y1) in layout.selection_rects(s, e) {
            let rect =
                vello::kurbo::Rect::new(f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1));
            scene.fill(vello::peniko::Fill::NonZero, transform, band, None, &rect);
        }
    }
}

fn draw_rich_underlines(
    scene: &mut Scene,
    layout: &crate::richtext::RichTextLayout,
    transform: Affine,
    color: Color,
) {
    for &(s, e) in &layout.underlines {
        for (x0, _y0, x1, y1) in layout.selection_rects(s, e) {
            let uy = f64::from(y1 - 2.0);
            let rect = vello::kurbo::Rect::new(f64::from(x0), uy, f64::from(x1), uy + 1.5);
            scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &rect);
        }
    }
}

fn draw_rich_strikes(
    scene: &mut Scene,
    layout: &crate::richtext::RichTextLayout,
    transform: Affine,
    color: Color,
) {
    for &(s, e) in &layout.strikes {
        for (x0, y0, x1, y1) in layout.selection_rects(s, e) {
            let my = f64::from(f32::midpoint(y0, y1));
            let rect = vello::kurbo::Rect::new(f64::from(x0), my, f64::from(x1), my + 1.5);
            scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &rect);
        }
    }
}

impl TextRenderer {
    pub fn new() -> Self {
        tilo_startup_metrics::span("text-renderer-init", || {
            Self::with_font_system(configured_font_system())
        })
    }

    ///
    /// # Errors
    ///
    /// Returns the catalog's packaging error if its manifest or fonts cannot
    /// be loaded and validated.
    pub fn with_font_catalog(
        catalog: &crate::font_catalog::FontCatalog,
    ) -> Result<Self, crate::font_catalog::FontCatalogError> {
        Ok(Self::with_font_system(
            crate::font_catalog::configured_font_system_with_catalog(catalog)?,
        ))
    }

    pub fn with_font_system(font_system: FontSystem) -> Self {
        Self {
            font_system,
            swash_cache: SwashCache::new(),
            measure_cache: TextCache::new(MEASURE_CACHE_LIMIT, MEASURE_CACHE_BYTES),
            generation: 0,
            layout_epoch: Arc::new(()),
            distinct_letters: false,
            hits: 0,
            misses: 0,
            shape_cache: TextCache::new(SHAPE_CACHE_LIMIT, SHAPE_CACHE_BYTES),
            shape_hits: 0,
            shape_misses: 0,
            shape_ns: 0,
            text_scale: 1.0,
            ink_bounds: HashMap::new(),
            ink_bounds_entries: 0,
            browser_font_demands: None,
            browser_font_assets: HashSet::new(),
        }
    }

    /// Construct a renderer over the verified browser startup catalog.
    pub fn with_browser_font_system(font_system: FontSystem) -> Self {
        share_host_font_database(&font_system);
        let mut renderer = Self::with_font_system(font_system);
        renderer.browser_font_demands = Some(BTreeSet::new());
        renderer.browser_font_assets = crate::font_catalog::browser_startup_files()
            .into_iter()
            .map(str::to_owned)
            .collect();
        renderer
    }

    /// Drain optional asset names demanded by missing glyphs in authoritative
    /// shaping. The host may retry a failed name after later shaping reports it.
    pub fn take_browser_font_demands(&mut self) -> Vec<String> {
        self.browser_font_demands
            .as_mut()
            .map_or_else(Vec::new, |demands| {
                std::mem::take(demands).into_iter().collect()
            })
    }

    /// A host fetch failed. Clear cached fallback measurements so a later
    /// frame that still shapes the affected text can re-report demand.
    pub fn browser_font_failed(&mut self) {
        if self.browser_font_demands.is_some() {
            self.bump_generation();
        }
    }

    /// Install one verified canonical optional asset and invalidate every
    /// cached metric/layout identity before another frame can be drawn.
    /// # Errors
    /// Propagates canonical manifest, hash, size, and face validation errors.
    pub fn install_browser_font(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<bool, crate::font_catalog::FontCatalogError> {
        let catalog = crate::font_catalog::FontCatalog::product("", self.font_system.locale());
        let changed = catalog.install_browser_asset(&mut self.font_system, name, bytes)?;
        self.browser_font_assets.insert(name.to_owned());
        if changed {
            share_host_font_database(&self.font_system);
            self.bump_generation();
        }
        Ok(changed)
    }

    /// Set the accessibility text multiplier used by subsequent measurement
    /// and paint calls. Callers that custom-draw canvas text must set this from
    /// the current theme before layout/painting; the renderer is retained
    /// across frames and therefore keeps this value until it is replaced.
    /// Invalid values fall back to 1× rather than poisoning the shaping cache
    /// with NaN metrics.
    pub fn set_text_scale(&mut self, scale: f32) {
        self.text_scale = if scale.is_finite() {
            scale.clamp(0.5, 4.0)
        } else {
            1.0
        };
    }

    /// Set the family the generic sans-serif resolves to: the theme's UI face
    /// ([`crate::tokens::Typography::sans_family`]). Text that names a role
    /// already carries the family; this makes text built on the generic
    /// family (inputs, toolkit internals) follow the same face. A family the
    /// font system does not have is ignored.
    pub fn set_ui_face(&mut self, family: &str) {
        let db = self.font_system.db();
        if db.family_name(&Family::SansSerif) == family
            || !db
                .faces()
                .any(|face| face.families.iter().any(|(name, _)| name == family))
        {
            return;
        }
        self.font_system.db_mut().set_sans_serif_family(family);
        self.bump_generation();
    }

    /// Follow a theme's type: its UI face ([`Self::set_ui_face`]) and its
    /// "Always distinguish similar letters" ([`Self::set_distinct_letters`]).
    pub fn apply_typography(&mut self, typography: &crate::tokens::Typography) {
        self.set_ui_face(&typography.sans_family);
        self.set_distinct_letters(typography.distinct_letters);
    }

    /// Turn the "Always distinguish similar letters" setting on or off: with
    /// it on, every look-alike (I, l, 1 and 0, O) is set apart in all text,
    /// whatever its kind, where the face has the forms. The theme's
    /// resolved [`crate::tokens::Typography::distinct_letters`].
    pub fn set_distinct_letters(&mut self, on: bool) {
        self.sync_distinct_letters();
        if self.distinct_letters != on {
            self.distinct_letters = on;
            self.sync_distinct_letters();
            self.bump_generation();
        }
    }

    /// Whether every look-alike letter is set apart in text drawn now: the
    /// setting is on, or a security surface is being drawn.
    pub fn distinguishes_letters(&self) -> bool {
        crate::font_catalog::matched::distinguish_everywhere()
    }

    /// Lay out or paint a security surface — a permission prompt, a link
    /// preview, an invite or join confirmation, identity verification —
    /// with every look-alike letter and figure set apart in all its text,
    /// prose included, whatever the setting.
    pub fn security_surface<R>(&mut self, draw: impl FnOnce(&mut Self) -> R) -> R {
        struct Leave;
        impl Drop for Leave {
            fn drop(&mut self) {
                crate::font_catalog::matched::security_surface(false);
            }
        }
        crate::font_catalog::matched::security_surface(true);
        let _leave = Leave;
        draw(self)
    }

    /// The effective accessibility text scale used for both measurement and
    /// drawing. Custom-drawn app chrome uses this to make its surrounding
    /// geometry grow with the shared renderer rather than letting enlarged
    /// type collide with fixed-size controls.
    pub fn text_scale(&self) -> f32 {
        self.text_scale
    }

    /// The style the shaper sees: only a renderer-relative style
    /// ([`TextScaling::Renderer`]) is grown by the accessibility text scale;
    /// resolved theme roles and document content are already final.
    fn scaled_style<'a>(&self, style: &'a TextStyle) -> std::borrow::Cow<'a, TextStyle> {
        self.sync_distinct_letters();
        let distinguish =
            style.distinguish || crate::font_catalog::matched::distinguish_everywhere();
        let unscaled =
            style.scaling != TextScaling::Renderer || (self.text_scale - 1.0).abs() <= f32::EPSILON;
        if unscaled && distinguish == style.distinguish {
            return std::borrow::Cow::Borrowed(style);
        }
        let mut scaled = style.clone();
        scaled.distinguish = distinguish;
        if !unscaled {
            scaled.size *= self.text_scale;
            scaled.letter_spacing *= self.text_scale;
        }
        std::borrow::Cow::Owned(scaled)
    }

    /// Take-and-reset the per-frame shape-cache stats:
    /// `(hits, misses, nanoseconds_spent_shaping)`, so each call reports only
    /// that frame's shaping work.
    pub fn take_shape_stats(&mut self) -> (u64, u64, u128) {
        let out = (self.shape_hits, self.shape_misses, self.shape_ns);
        self.shape_hits = 0;
        self.shape_misses = 0;
        self.shape_ns = 0;
        out
    }

    /// Useful for benchmarks asserting "the static-label widgets warmed the
    /// cache and subsequent frames are pure hits".
    pub fn cache_stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    pub(crate) fn layout_epoch(&self) -> &Arc<()> {
        self.sync_distinct_letters();
        &self.layout_epoch
    }

    /// Put this renderer's setting on the thread that is shaping with it.
    fn sync_distinct_letters(&self) {
        crate::font_catalog::matched::set_distinguish_everywhere(self.distinct_letters);
    }

    /// Call this when the font set or active locale changes; cosmic-text
    /// will shape glyphs differently after either swap, so stale numbers
    /// would let widgets compute wrong layouts.
    pub fn bump_generation(&mut self) {
        self.layout_epoch = Arc::new(());
        self.generation = self.generation.wrapping_add(1);
        self.measure_cache.clear();
        // Entries key on `generation` so stale ones would never match
        // anyway; the explicit clear reclaims their memory immediately.
        self.shape_cache.clear();
        self.ink_bounds.clear();
        self.ink_bounds_entries = 0;
    }

    fn measure_insert(&mut self, key: CacheKey<'_>, value: (f32, f32)) {
        self.measure_cache.insert(key, value, 0);
    }

    pub fn measure(&mut self, text: &str, size: f32, max_width: Option<f32>) -> (f32, f32) {
        self.measure_weighted(text, size, max_width, TextWeight::Regular)
    }

    /// Same as [`TextRenderer::measure`] but respects a typographic weight.
    /// Heavier weights generally advance wider, so measurements must use
    /// the same weight the draw path will render with.
    pub fn measure_weighted(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
    ) -> (f32, f32) {
        self.measure_style(text, max_width, &TextStyle::new(size).with_weight(weight))
    }

    /// Measure using one authoritative style value. This is the preferred
    /// API for new widgets because the same value can be passed to
    /// [`Self::draw_style`], preventing measurement/paint metric drift.
    pub fn measure_style(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> (f32, f32) {
        self.measure_style_ex(text, max_width, style, true)
    }

    /// Like [`Self::measure_style`] but skips warming the paint-side shape
    /// cache. Elision probes many throwaway prefixes; keeping those out of
    /// `shape_cache` preserves room for labels the next frame will paint.
    fn measure_style_cold(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> (f32, f32) {
        self.measure_style_ex(text, max_width, style, false)
    }

    fn measure_style_ex(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
        warm_shape: bool,
    ) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let input = CacheQuery::new(text, style);
        self.measure_query(&input, max_width, warm_shape)
    }

    /// Measure retained immutable input without hashing its text/style again.
    pub fn measure_prepared(&mut self, text: &PreparedText, max_width: Option<f32>) -> (f32, f32) {
        if text.text().is_empty() {
            return (0.0, 0.0);
        }
        self.measure_query(&CacheQuery::prepared(text), max_width, true)
    }

    fn measure_query(
        &mut self,
        input: &CacheQuery<'_>,
        max_width: Option<f32>,
        warm_shape: bool,
    ) -> (f32, f32) {
        let scaled_style = self.scaled_style(input.style);
        let style = &scaled_style;
        self.note_browser_explicit_family(style);
        let max_width = self.collapse_unused_max_width(input, max_width, style, warm_shape);
        self.measure_style_cached(input, max_width, style, warm_shape)
    }

    /// When `max_width` is wider than the text's natural line, cosmic-text
    /// produces the same glyphs as an unconstrained shape. Collapse to `None`
    /// so layout (often unconstrained) and paint (widget box width) share one
    /// cache entry instead of reshaping the same label twice on first paint.
    fn collapse_unused_max_width(
        &mut self,
        input: &CacheQuery<'_>,
        max_width: Option<f32>,
        style: &TextStyle,
        warm_shape: bool,
    ) -> Option<f32> {
        let text = input.text;
        let width = max_width?;
        // Balance and hyphenation consult the wrap width even when the natural
        // line would fit; keep those keys distinct. So does drawing a centred
        // or end-aligned line: its glyphs sit inside the width.
        if style.wrap_mode == TextWrapMode::Balance
            || !matches!(style.hyphenation, Hyphenation::None)
            || (warm_shape && matches!(style.align, ParagraphAlign::Center | ParagraphAlign::End))
        {
            return Some(width);
        }

        let key = input.key(None, self.generation, self.text_scale);
        if let Some(natural) = self.measure_cache.get(&key).copied() {
            self.hits = self.hits.saturating_add(1);
            if natural.0 <= width {
                if warm_shape {
                    self.ensure_shaped(input, None, style, natural);
                }
                return None;
            }
            return Some(width);
        }

        // Shape once for the unconstrained probe. Warm paint geometry only when
        // the width collapses away — wrapping text must not leave a stale
        // unconstrained entry that the draw path will never hit.
        self.misses = self.misses.saturating_add(1);
        let prepared = self.shape_buffer(text, None, style);
        let natural = Self::buffer_metrics(&prepared, style.line_height_px());
        self.measure_insert(key, natural);
        if natural.0 <= width {
            if warm_shape {
                let shape_key = input.key(None, self.generation, self.text_scale);
                self.store_shaped(shape_key, &prepared, style, natural);
            }
            None
        } else {
            Some(width)
        }
    }

    fn measure_style_cached(
        &mut self,
        input: &CacheQuery<'_>,
        max_width: Option<f32>,
        style: &TextStyle,
        warm_shape: bool,
    ) -> (f32, f32) {
        let text = input.text;
        let key = input.key(max_width, self.generation, self.text_scale);
        if let Some(cached) = self.measure_cache.get(&key).copied() {
            self.hits = self.hits.saturating_add(1);
            if warm_shape {
                self.ensure_shaped(input, max_width, style, cached);
            }
            return cached;
        }
        // Paint may already have shaped this string under the same key.
        let shape_key = input.key(max_width, self.generation, self.text_scale);
        if let Some(shaped) = self
            .shape_cache
            .get(&shape_key)
            .or_else(|| shape_key.uncached_shape())
        {
            self.hits = self.hits.saturating_add(1);
            let result = shaped.size;
            self.measure_insert(key, result);
            return result;
        }
        self.misses = self.misses.saturating_add(1);

        let line_height = style.line_height_px();
        let prepared = self.shape_buffer(text, max_width, style);
        let result = Self::buffer_metrics(&prepared, line_height);
        self.measure_insert(key, result);
        if warm_shape {
            self.store_shaped(shape_key, &prepared, style, result);
        }
        result
    }

    fn ensure_shaped(
        &mut self,
        input: &CacheQuery<'_>,
        max_width: Option<f32>,
        style: &TextStyle,
        size: (f32, f32),
    ) {
        let text = input.text;
        let shape_key = input.key(max_width, self.generation, self.text_scale);
        if self.shape_cache.contains_key(&shape_key) || shape_key.uncached_shape().is_some() {
            return;
        }
        let prepared = self.shape_buffer(text, max_width, style);
        self.store_shaped(shape_key, &prepared, style, size);
    }

    fn buffer_metrics(prepared: &PreparedBuffer, line_height: f32) -> (f32, f32) {
        let mut width = 0.0f32;
        let mut lines: usize = 0;
        for run in prepared.buffer.layout_runs() {
            let exercised_hyphen = run
                .glyphs
                .last()
                .is_some_and(|glyph| run.text.get(glyph.start..glyph.end) == Some("\u{ad}"));
            width = width.max(
                run.line_w
                    + if exercised_hyphen {
                        prepared.hyphen_width
                    } else {
                        0.0
                    },
            );
            lines = lines.saturating_add(1);
        }
        // Line counts stay in the low hundreds; this cast is exact in that
        // range, and any other conversion path risks the pixel golden.
        #[allow(
            clippy::cast_precision_loss,
            reason = "line counts stay well within f32's exact integer range"
        )]
        let height = lines as f32 * line_height;
        (width, height)
    }

    /// Truncate `text` to fit within `max_width` on a single line, appending
    /// [`ELLIPSIS`] when anything was dropped; returns just the ellipsis when
    /// not even one character fits before it, and an empty string for empty
    /// text or a non-positive width. Measures with [`TextWeight::Regular`];
    /// pair with [`Self::elide_to_width_weighted`] or
    /// [`Self::elide_to_width_style`] when the draw style differs.
    pub fn elide_to_width(&mut self, text: &str, size: f32, max_width: f32) -> String {
        self.elide_to_width_weighted(text, size, max_width, TextWeight::Regular)
    }

    /// Weighted variant of [`Self::elide_to_width`] — measures with the
    /// supplied [`TextWeight`] so the fit matches a bold/medium draw call.
    pub fn elide_to_width_weighted(
        &mut self,
        text: &str,
        size: f32,
        max_width: f32,
        weight: TextWeight,
    ) -> String {
        self.elide_to_width_style(text, max_width, &TextStyle::new(size).with_weight(weight))
    }

    /// [`Self::elide_to_width`] measured with a full [`TextStyle`] — the
    /// family (monospace), italic and weight the text will be drawn with.
    ///
    /// Cuts only between grapheme clusters, so a combining mark, an emoji
    /// sequence or a flag is never split, and drops whitespace before the
    /// ellipsis so it never reads as `" …"`. The kept prefix is the longest
    /// whose *shaped* width including the ellipsis fits, found by binary
    /// search, so a long label costs a handful of measures rather than one
    /// per character.
    pub fn elide_to_width_style(
        &mut self,
        text: &str,
        max_width: f32,
        style: &TextStyle,
    ) -> String {
        if text.is_empty() || max_width <= 0.0 {
            return String::new();
        }
        if self.measure_style_cold(text, None, style).0 <= max_width {
            return text.to_string();
        }
        // Byte offsets where a cut may fall: the start of every grapheme
        // cluster after the first. Cutting at index `i` keeps `text[..cuts[i]]`.
        let cuts: Vec<usize> = crate::text_break::grapheme_indices(text)
            .map(|(start, _)| start)
            .filter(|&start| start > 0)
            .collect();
        let candidate = |end: usize| {
            let mut kept = text.get(..end).unwrap_or_default().trim_end().to_string();
            kept.push(ELLIPSIS);
            kept
        };
        // Invariant: every cut below `lo` fits, every cut at or past `hi`
        // does not.
        let (mut lo, mut hi) = (0usize, cuts.len());
        while lo < hi {
            let mid = lo.midpoint(hi);
            let end = cuts.get(mid).copied().unwrap_or_default();
            if self.measure_style_cold(&candidate(end), None, style).0 <= max_width {
                lo = mid.saturating_add(1);
            } else {
                hi = mid;
            }
        }
        match lo.checked_sub(1).and_then(|i| cuts.get(i)) {
            Some(&end) => candidate(end),
            None => ELLIPSIS.to_string(),
        }
    }

    /// Word-wraps `text` in `style` to `max_width` and clamps to at most
    /// `max_lines` lines, eliding the last kept line when content overflows.
    /// Draw each returned line in the same style, one
    /// [`TextStyle::line_height_px`] apart. `max_lines == 1` degenerates to
    /// [`Self::elide_to_width_style`].
    pub fn clamp_to_lines(
        &mut self,
        text: &str,
        style: &TextStyle,
        max_width: f32,
        max_lines: usize,
    ) -> Vec<String> {
        self.clamp_to_lines_style(text, max_width, max_lines, style)
    }

    /// Style-aware line clamping. Wrapping and the final truncation marker
    /// use the same font, weight and features as the text that will be drawn.
    pub fn clamp_to_lines_style(
        &mut self,
        text: &str,
        max_width: f32,
        max_lines: usize,
        style: &TextStyle,
    ) -> Vec<String> {
        if max_lines == 0 || text.is_empty() || max_width <= 0.0 {
            return Vec::new();
        }
        if max_lines == 1 {
            return vec![self.elide_to_width_style(text, max_width, style)];
        }
        let wrapped = self.wrap_lines(text, style, max_width);
        if wrapped.len() <= max_lines {
            return wrapped
                .into_iter()
                .map(|line| self.elide_to_width_style(&line, max_width, style))
                .collect();
        }
        // Keep the first `max_lines - 1` whole lines, then merge the rest
        // onto the final line and elide it as the truncation marker.
        let (head, tail) = wrapped
            .split_at_checked(max_lines.saturating_sub(1))
            .unwrap_or((&wrapped, &[]));
        let mut kept: Vec<String> = head
            .iter()
            .map(|line| self.elide_to_width_style(line, max_width, style))
            .collect();
        let remainder: String = tail.join(" ");
        kept.push(self.elide_to_width_style(&remainder, max_width, style));
        kept
    }

    /// Greedy UAX #14 wrap of `text` at `max_width`. This handles scripts that
    /// do not use ASCII spaces, keeps non-breaking spaces intact, and honors
    /// explicit mandatory breaks. An unbreakable segment wider than the budget
    /// stays on its own line; [`Self::clamp_to_lines`] elides it if necessary.
    pub(crate) fn wrap_lines(
        &mut self,
        text: &str,
        style: &TextStyle,
        max_width: f32,
    ) -> Vec<String> {
        fn collapsible_edge(character: char) -> bool {
            matches!(character, ' ' | '\t' | '\n' | '\r')
        }

        let opportunities: Vec<_> = line_break_opportunities(text).collect();
        let mut lines = Vec::new();
        let mut start = 0usize;
        while start < text.len() {
            while let Some(character) = text.get(start..).and_then(|rest| rest.chars().next()) {
                if !collapsible_edge(character) {
                    break;
                }
                start = start.saturating_add(character.len_utf8());
            }
            if start == text.len() {
                break;
            }

            let mut first = None;
            let mut best = None;
            for &(end, kind) in opportunities.iter().filter(|(end, _)| *end > start) {
                first.get_or_insert(end);
                let candidate = text
                    .get(start..end)
                    .unwrap_or_default()
                    .trim_end_matches(collapsible_edge);
                let fits = self.measure_style(candidate, None, style).0 <= max_width;
                if fits {
                    best = Some(end);
                    if kind == LineBreakKind::Mandatory {
                        break;
                    }
                } else {
                    break;
                }
            }
            let end = best.or(first).unwrap_or(text.len());
            let line = text
                .get(start..end)
                .unwrap_or_default()
                .trim_end_matches(collapsible_edge);
            if !line.is_empty() {
                lines.push(line.to_string());
            }
            start = end;
        }
        lines
    }

    pub fn draw_text(
        &mut self,
        scene: &mut Scene,
        text: &str,
        size: f32,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
    ) {
        self.draw_text_weighted(
            scene,
            text,
            size,
            color,
            transform,
            max_width,
            TextWeight::Regular,
        );
    }

    /// Measure `text` shaped in an explicit [`FontChoice`].
    pub fn measure_font(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
        italic: bool,
        family: &FontChoice,
    ) -> (f32, f32) {
        let style = TextStyle::new(size)
            .with_weight(weight)
            .with_italic(italic)
            .with_family(family.clone());
        self.measure_style(text, max_width, &style)
    }

    /// Distinct family names of installed faces, sorted and de-duplicated
    /// across weights/styles — the pool an app's font picker offers.
    pub fn system_families(&self) -> Vec<String> {
        use std::collections::BTreeSet;
        let mut set: BTreeSet<String> = BTreeSet::new();
        for face in self.font_system.db().faces() {
            if let Some((name, _)) = face.families.first() {
                // Skip internal/placeholder families with no real name. An
                // optical cut is listed as the family it is a cut of.
                if !name.trim().is_empty() {
                    set.insert(
                        crate::font_catalog::optical::family_name(name)
                            .map_or_else(|| name.clone(), str::to_owned),
                    );
                }
            }
        }
        set.into_iter().collect()
    }

    /// Measure `text` with style attributes and an optional monospace family.
    /// NOT cached — these calls are rare (one per segment per frame).
    pub fn measure_styled(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
        italic: bool,
        monospace: bool,
    ) -> (f32, f32) {
        let family = if monospace {
            FontChoice::Monospace
        } else {
            FontChoice::SansSerif
        };
        let style = TextStyle::new(size)
            .with_weight(weight)
            .with_italic(italic)
            .with_family(family);
        self.measure_style(text, max_width, &style)
    }

    /// Same as [`TextRenderer::draw_text`] but takes an explicit typographic
    /// [`TextWeight`], for chrome labels that need to lift off the body.
    pub fn draw_text_weighted(
        &mut self,
        scene: &mut Scene,
        text: &str,
        size: f32,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
        weight: TextWeight,
    ) {
        let style = TextStyle::new(size).with_weight(weight);
        self.draw_style(scene, text, color, transform, max_width, &style);
    }

    /// Shape and paint with one authoritative style value. Pair this with
    /// [`Self::measure_style`] so font metrics, wrapping, and glyph placement
    /// cannot silently diverge.
    pub fn draw_style(
        &mut self,
        scene: &mut Scene,
        text: &str,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
        style: &TextStyle,
    ) {
        if text.is_empty() {
            return;
        }
        self.draw_query(
            scene,
            &CacheQuery::new(text, style),
            color,
            transform,
            max_width,
        );
    }

    /// How far below the origin [`Self::draw_style`], given the same
    /// arguments, puts the first line's baseline: what places text by its
    /// baseline rather than its line box.
    pub fn first_baseline_style(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> f32 {
        let logical_style = style;
        let scaled_style = self.scaled_style(style);
        let style = &scaled_style;
        if text.is_empty() {
            return style.size * 0.8;
        }
        let input = CacheQuery::new(text, logical_style);
        let max_width = self.collapse_unused_max_width(&input, max_width, style, true);
        let runs = self.shaped_query(&input, max_width, style);
        runs.first()
            .and_then(|run| run.glyphs.first())
            .map_or(style.size * 0.8, |glyph| glyph.y)
    }

    /// Draw retained immutable input; colour and transform do not affect geometry identity.
    pub fn draw_prepared(
        &mut self,
        scene: &mut Scene,
        text: &PreparedText,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
    ) {
        if text.text().is_empty() {
            return;
        }
        self.draw_query(
            scene,
            &CacheQuery::prepared(text),
            color,
            transform,
            max_width,
        );
    }

    fn draw_query(
        &mut self,
        scene: &mut Scene,
        input: &CacheQuery<'_>,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
    ) {
        let scaled_style = self.scaled_style(input.style);
        let style = &scaled_style;
        let max_width = self.collapse_unused_max_width(input, max_width, style, true);
        let runs = self.shaped_query(input, max_width, style);
        self.paint_runs(scene, &runs, color, transform);
    }

    /// Shapes `text` into a laid-out cosmic-text [`Buffer`], applying the
    /// same hyphenation/balance handling measurement uses.
    fn shape_buffer(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> PreparedBuffer {
        self.note_browser_explicit_family(style);
        let line_height = style.line_height_px();
        let metrics = Metrics::new(style.size, line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let attrs = style.cosmic_attrs();
        let offset_map = max_width.map(|_| style.hyphenation.offset_map(text));
        let shaping_text = offset_map
            .as_ref()
            .map_or(std::borrow::Cow::Borrowed(text), |map| map.shape_text(text));
        let emoji = self.browser_font_assets.contains("Noto-COLRv1.ttf");
        let spans = crate::font_catalog::matched::plan(
            self.font_system.db(),
            browser_emoji_runs(&shaping_text, &attrs, emoji),
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
        let hyphen_width = if offset_map
            .as_ref()
            .is_some_and(OffsetMap::has_discretionary_hyphens)
        {
            discretionary_hyphen_advance(&mut self.font_system, style)
        } else {
            0.0
        };
        let layout_width = max_width.map(|width| (width - hyphen_width).max(0.0));
        buffer.set_size(layout_width, None);
        buffer.set_wrap(match style.wrap_mode {
            TextWrapMode::WordOrGlyph => Wrap::WordOrGlyph,
            TextWrapMode::Wrap | TextWrapMode::Balance | TextWrapMode::Indented => Wrap::Word,
        });
        buffer.shape_until_scroll(&mut self.font_system, false);
        if style.wrap_mode == TextWrapMode::Balance && max_width.is_some() {
            balance_buffer(
                &mut buffer,
                &mut self.font_system,
                layout_width.unwrap_or(0.0),
            );
        }
        self.note_browser_font_demands(&buffer);
        PreparedBuffer {
            buffer,
            hyphen_width,
        }
    }

    pub(crate) fn note_browser_font_demands(&mut self, buffer: &Buffer) {
        let Some(demands) = self.browser_font_demands.as_mut() else {
            return;
        };
        for run in buffer.layout_runs() {
            let emoji_presentation = browser_text_demands_emoji(run.text);
            if emoji_presentation && !self.browser_font_assets.contains("Noto-COLRv1.ttf") {
                demands.insert("Noto-COLRv1.ttf".to_owned());
            }
            for glyph in run.glyphs.iter().filter(|glyph| glyph.glyph_id == 0) {
                let Some(text) = run.text.get(glyph.start..glyph.end) else {
                    continue;
                };
                for character in text.chars() {
                    let candidates = if emoji_presentation
                        && unicode_properties::UnicodeEmoji::is_emoji_char(character)
                    {
                        &["Noto-COLRv1.ttf"][..]
                    } else {
                        browser_assets_for_missing(character)
                    };
                    if let Some(asset) = candidates
                        .iter()
                        .find(|asset| !self.browser_font_assets.contains(**asset))
                    {
                        demands.insert((*asset).to_owned());
                    }
                }
            }
        }
    }

    fn note_browser_explicit_family(&mut self, style: &TextStyle) {
        let Some(demands) = self.browser_font_demands.as_mut() else {
            return;
        };
        // A generic family demands the face it resolves to, so the page can
        // start with the regular UI face alone and fetch the serif, the
        // monospace and the italics when text first asks for them.
        let name = match &style.family {
            FontChoice::Named(name) => name.as_str(),
            FontChoice::SansSerif => UI_SANS_FAMILY,
            FontChoice::Serif => UI_SERIF_FAMILY,
            FontChoice::Monospace => UI_MONOSPACE_FAMILY,
        };
        // A family drawn in optical cuts needs the cut for this size and the
        // smaller ones its superscripts and footnotes take.
        let weight = style.effective_weight().0;
        let mut families = crate::font_catalog::optical::cuts_up_to(
            name,
            style.size,
            style.optical(),
            style.italic,
            weight,
        );
        if families.is_empty() {
            families.push(name);
        }
        let bold = weight > 500;
        // A face without a bold sets bold text in its regular.
        let mut files = crate::font_catalog::product_files_for(&families, style.italic, bold);
        if files.is_empty() && bold {
            files = crate::font_catalog::product_files_for(&families, style.italic, false);
        }
        for file in files {
            if !self.browser_font_assets.contains(file) {
                demands.insert(file.to_owned());
            }
        }
    }

    pub(crate) fn note_browser_rich_layout(&mut self, layout: &crate::richtext::RichTextLayout) {
        self.note_browser_font_demands(&layout.buffer);
    }

    pub(crate) fn browser_color_emoji_installed(&self) -> bool {
        self.browser_font_assets.contains("Noto-COLRv1.ttf")
    }

    /// Flatten a shaped [`Buffer`] into per-face contiguous runs, baking the
    /// baseline into each glyph so paint is a pure transform + colour, no
    /// arithmetic.
    fn flatten_glyph_runs(&mut self, buffer: &Buffer, style: &TextStyle) -> Vec<GlyphRun> {
        let variations = style.resolved_variations();
        let mut runs = Vec::new();
        for run in buffer.layout_runs() {
            let exercised_hyphen = run
                .glyphs
                .last()
                .filter(|glyph| run.text.get(glyph.start..glyph.end) == Some("\u{ad}"));
            let mut current: Option<(cosmic_text::fontdb::ID, Weight, u32)> = None;
            let mut glyphs: Vec<vello::Glyph> = Vec::new();
            for g in run.glyphs {
                if run.text.get(g.start..g.end) == Some("\u{ad}") {
                    continue;
                }
                // A matched script fallback is shaped at its own size.
                let key = (g.font_id, g.font_weight, g.font_size.to_bits());
                if let Some((face_id, font_weight, size)) = current
                    && current != Some(key)
                    && !glyphs.is_empty()
                {
                    runs.push(GlyphRun {
                        face_id,
                        font_weight,
                        size: f32::from_bits(size),
                        glyphs: std::mem::take(&mut glyphs),
                        normalized_coords: self.normalized_coords(
                            face_id,
                            &variations,
                            font_weight,
                        ),
                    });
                }
                current = Some(key);
                let (x, y) = glyph_paint_origin(
                    g,
                    hanging_glyph_x(&run, g, style.hanging_punctuation),
                    run.line_y,
                );
                glyphs.push(vello::Glyph {
                    id: u32::from(g.glyph_id),
                    x,
                    y,
                });
            }
            if let Some((face_id, font_weight, size)) = current
                && !glyphs.is_empty()
            {
                runs.push(GlyphRun {
                    face_id,
                    font_weight,
                    size: f32::from_bits(size),
                    glyphs,
                    normalized_coords: self.normalized_coords(face_id, &variations, font_weight),
                });
            }
            if let Some(marker) = exercised_hyphen
                && let Some(hyphen) = self.visible_discretionary_hyphen(marker)
            {
                runs.push(GlyphRun {
                    face_id: hyphen.font_id,
                    font_weight: hyphen.font_weight,
                    size: hyphen.font_size,
                    glyphs: vec![vello::Glyph {
                        id: u32::from(hyphen.glyph_id),
                        x: hyphen.x,
                        y: run.line_y + hyphen.y,
                    }],
                    normalized_coords: self.normalized_coords(
                        hyphen.font_id,
                        &variations,
                        hyphen.font_weight,
                    ),
                });
            }
        }
        runs
    }

    fn store_shaped(
        &mut self,
        key: CacheKey<'_>,
        prepared: &PreparedBuffer,
        style: &TextStyle,
        size: (f32, f32),
    ) {
        if self.shape_cache.contains_key(&key) || key.uncached_shape().is_some() {
            return;
        }
        let runs = Arc::new(self.flatten_glyph_runs(&prepared.buffer, style));
        // Count measure-time warms as shape misses so paint-side stats still
        // see the cosmic-text work that filled the entry.
        self.shape_misses = self.shape_misses.saturating_add(1);
        let bytes = glyph_run_bytes(&runs);
        if !self.shape_cache.insert(
            key,
            ShapedText {
                runs: runs.clone(),
                size,
            },
            bytes,
        ) {
            key.remember_uncached_shape(ShapedText { runs, size });
        }
    }

    /// Shape `text` into positioned [`GlyphRun`]s, caching the result. On a
    /// hit this is an `Arc` clone; on a miss it runs cosmic-text once.
    fn shaped_query(
        &mut self,
        input: &CacheQuery<'_>,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> Arc<Vec<GlyphRun>> {
        let text = input.text;
        self.note_browser_explicit_family(style);
        let key = input.key(max_width, self.generation, self.text_scale);
        if let Some(shaped) = self.shape_cache.get(&key).or_else(|| key.uncached_shape()) {
            self.shape_hits = self.shape_hits.saturating_add(1);
            let runs = shaped.runs.clone();
            let size = shaped.size;
            let measure_key = key;
            if !self.measure_cache.contains_key(&measure_key) {
                self.measure_insert(measure_key, size);
            }
            return runs;
        }
        self.shape_misses = self.shape_misses.saturating_add(1);

        let t0 = crate::time::now();
        let prepared = self.shape_buffer(text, max_width, style);
        let size = Self::buffer_metrics(&prepared, style.line_height_px());
        let runs = Arc::new(self.flatten_glyph_runs(&prepared.buffer, style));
        self.shape_ns = self.shape_ns.saturating_add(t0.elapsed().as_nanos());

        self.shape_cache.insert(
            key,
            ShapedText {
                runs: runs.clone(),
                size,
            },
            glyph_run_bytes(&runs),
        );
        let measure_key = key;
        if !self.measure_cache.contains_key(&measure_key) {
            self.measure_insert(measure_key, size);
        }
        runs
    }

    #[cfg(test)]
    fn shaped_runs(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> Arc<Vec<GlyphRun>> {
        self.shaped_query(&CacheQuery::new(text, style), max_width, style)
    }

    /// Resolve font handles through cosmic-text's existing face-and-weight cache.
    fn paint_runs(
        &mut self,
        scene: &mut Scene,
        runs: &[GlyphRun],
        color: Color,
        transform: Affine,
    ) {
        for run in runs {
            let Some(font) = self.vello_font(run.face_id, run.font_weight) else {
                continue;
            };
            scene
                .draw_glyphs(&font)
                .font_size(run.size)
                .normalized_coords(&run.normalized_coords)
                .brush(color)
                .transform(transform)
                .draw(
                    StyleRef::Fill(vello::peniko::Fill::NonZero),
                    run.glyphs.iter().copied(),
                );
        }
    }

    /// `text` shaped exactly as [`Self::draw_style`] paints it, one outline
    /// per glyph: what draws text as geometry (an outlined export, words set
    /// along a curve) and measures its ink. Colour and bitmap glyphs, which
    /// have no outline, are left out.
    pub fn glyph_outlines(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> Vec<GlyphOutline> {
        if text.is_empty() {
            return Vec::new();
        }
        let input = CacheQuery::new(text, style);
        let scaled_style = self.scaled_style(style);
        let style = &scaled_style;
        let max_width = self.collapse_unused_max_width(&input, max_width, style, false);
        let prepared = self.shape_buffer(text, max_width, style);
        let variations = style.resolved_variations();
        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at.saturating_add(1)))
            .collect();
        let mut placed = Vec::new();
        for run in prepared.buffer.layout_runs() {
            let line_start = line_starts.get(run.line_i).copied().unwrap_or(0);
            for glyph in run.glyphs {
                if run.text.get(glyph.start..glyph.end) == Some("\u{ad}") {
                    continue;
                }
                let (x, y) = glyph_paint_origin(
                    glyph,
                    hanging_glyph_x(&run, glyph, style.hanging_punctuation),
                    run.line_y,
                );
                placed.push((
                    glyph.font_id,
                    glyph.font_weight,
                    glyph.glyph_id,
                    glyph.font_size,
                    Point::new(f64::from(x), f64::from(y)),
                    f64::from(glyph.w),
                    line_start.saturating_add(glyph.start)..line_start.saturating_add(glyph.end),
                ));
            }
        }
        placed
            .into_iter()
            .filter_map(|(face, weight, glyph, size, origin, advance, cluster)| {
                let font = self.vello_font(face, weight)?;
                let coords = self.normalized_coords(face, &variations, weight);
                Some(GlyphOutline {
                    path: outline_path(&font, glyph, size, &coords)?,
                    origin,
                    advance,
                    cluster,
                })
            })
            .collect()
    }

    /// Retain the exact face and shared bytes already used by shaping.
    fn vello_font(&mut self, face_id: cosmic_text::fontdb::ID, weight: Weight) -> Option<Font> {
        self.font_system
            .get_font(face_id, weight)
            .map(|font| font.as_peniko())
    }

    fn normalized_coords(
        &self,
        face_id: cosmic_text::fontdb::ID,
        variations: &[FontVariation],
        font_weight: Weight,
    ) -> Vec<vello::NormalizedCoord> {
        use skrifa::{FontRef, MetadataProvider, Tag};

        self.font_system
            .db()
            .with_face_data(face_id, |data, index| {
                let font = FontRef::from_index(data, index).ok()?;
                let settings = variations
                    .iter()
                    .filter(|axis| axis.tag != OpenTypeTag::WEIGHT)
                    .map(|axis| (Tag::new(&axis.tag.0), axis.value))
                    .chain(std::iter::once((
                        Tag::new(b"wght"),
                        f32::from(font_weight.0),
                    )));
                let location = font.axes().location(settings);
                Some(
                    location
                        .coords()
                        .iter()
                        .map(|coord| coord.to_bits())
                        .collect(),
                )
            })
            .flatten()
            .unwrap_or_default()
    }

    /// Draw a [`crate::richtext::RichTextLayout`] (attributed, wrapped text)
    /// in a single `color`, including its underline decorations.
    pub fn draw_rich(
        &mut self,
        scene: &mut Scene,
        layout: &crate::richtext::RichTextLayout,
        transform: Affine,
        color: Color,
    ) {
        draw_rich_backgrounds(scene, layout, transform);

        // Glyphs batch by face, shaped weight, color, size and baseline shift.
        for run in layout.buffer.layout_runs() {
            self.draw_rich_run(scene, &run, layout, transform, color, None);
        }

        draw_rich_underlines(scene, layout, transform, color);
        draw_rich_strikes(scene, layout, transform, color);
    }

    /// Draw rich text while omitting only glyph batches proven outside `clip`.
    /// The clip is in final scene coordinates; callers must keep actual clip
    /// layers. Decorations remain unchanged. Unsupported ink draws normally.
    pub fn draw_rich_in_clip(
        &mut self,
        scene: &mut Scene,
        layout: &crate::richtext::RichTextLayout,
        transform: Affine,
        color: Color,
        clip: Rect,
    ) {
        draw_rich_backgrounds(scene, layout, transform);
        for run in layout.buffer.layout_runs() {
            // A long block repeats this for every page slice it spans; skip
            // whole visual lines before any per-glyph work or glyph encoding.
            if rich_run_outside_clip(&run, transform, clip) {
                continue;
            }
            self.draw_rich_run(scene, &run, layout, transform, color, Some(clip));
        }
        draw_rich_underlines(scene, layout, transform, color);
        draw_rich_strikes(scene, layout, transform, color);
    }

    /// Batch and flush one shaped [`cosmic_text::LayoutRun`] of `layout` for
    /// [`Self::draw_rich`].
    fn draw_rich_run(
        &mut self,
        scene: &mut Scene,
        run: &cosmic_text::LayoutRun<'_>,
        layout: &crate::richtext::RichTextLayout,
        transform: Affine,
        color: Color,
        clip: Option<Rect>,
    ) {
        if !self.draw_uniform_rich_run(scene, run, layout, transform, color, clip) {
            self.draw_rich_run_copied(scene, run, layout, transform, color, clip);
        }
    }

    // Uniform runs need no rewritten positions or temporary glyph copies.
    fn draw_uniform_rich_run(
        &mut self,
        scene: &mut Scene,
        run: &cosmic_text::LayoutRun<'_>,
        layout: &crate::richtext::RichTextLayout,
        transform: Affine,
        color: Color,
        clip: Option<Rect>,
    ) -> bool {
        if layout.hanging_punctuation.first || layout.hanging_punctuation.last {
            return false;
        }
        let Some(first) = run.glyphs.first() else {
            return false;
        };
        let shift = layout.baseline_at(layout.glyph_global_byte(run.line_i, first.start));
        #[allow(
            clippy::float_cmp,
            reason = "batching requires exactly equal glyph sizes, not approximately equal sizes"
        )]
        let uniform = run.glyphs.iter().all(|glyph| {
            glyph.font_id == first.font_id
                && glyph.font_weight == first.font_weight
                && glyph.color_opt == first.color_opt
                && glyph.font_size == first.font_size
                && layout.baseline_at(layout.glyph_global_byte(run.line_i, glyph.start)) == shift
                && run.text.get(glyph.start..glyph.end) != Some("\u{ad}")
        });
        if !uniform {
            return false;
        }
        let batch = GlyphBatch {
            face_id: first.font_id,
            glyphs: run.glyphs,
            size: first.font_size,
            color: resolve_rich_color(color, first.color_opt),
            line_y: run.line_y + shift.offset(first.font_size),
            variations: &layout.font_variations,
        };
        self.flush_glyphs(scene, &batch, transform, clip);
        true
    }

    fn draw_rich_run_copied(
        &mut self,
        scene: &mut Scene,
        run: &cosmic_text::LayoutRun<'_>,
        layout: &crate::richtext::RichTextLayout,
        transform: Affine,
        color: Color,
        clip: Option<Rect>,
    ) {
        use crate::richtext::BaselineShift;
        let exercised_hyphen = run
            .glyphs
            .last()
            .filter(|glyph| run.text.get(glyph.start..glyph.end) == Some("\u{ad}"));
        let mut face: Option<cosmic_text::fontdb::ID> = None;
        let mut cur_color: Option<cosmic_text::Color> = None;
        let mut cur_weight: Option<Weight> = None;
        let mut cur_shift = BaselineShift::None;
        // Avoid repeated growth for ordinary lines without reserving an
        // unbounded long run whose individual style batches may be short.
        let mut batch: Vec<cosmic_text::LayoutGlyph> =
            Vec::with_capacity(run.glyphs.len().min(128));
        let mut size = 16.0f32;
        let mut batch_size = 16.0f32;
        for g in run.glyphs {
            if run.text.get(g.start..g.end) == Some("\u{ad}") {
                continue;
            }
            let global_byte = layout.glyph_global_byte(run.line_i, g.start);
            let g_shift = layout.baseline_at(global_byte);
            // Glyphs batch by face, shaped weight, color, size and baseline shift.
            let changed = face != Some(g.font_id)
                || cur_weight != Some(g.font_weight)
                || cur_color != g.color_opt
                || (g.font_size - batch_size).abs() > f32::EPSILON
                || cur_shift != g_shift;
            if let Some(face_id) = face
                && changed
                && !batch.is_empty()
            {
                let dy = cur_shift.offset(batch_size);
                let glyphs = GlyphBatch {
                    face_id,
                    glyphs: &batch,
                    size: batch_size,
                    color: resolve_rich_color(color, cur_color),
                    line_y: run.line_y + dy,
                    variations: &layout.font_variations,
                };
                self.flush_glyphs(scene, &glyphs, transform, clip);
                batch.clear();
            }
            face = Some(g.font_id);
            cur_weight = Some(g.font_weight);
            cur_color = g.color_opt;
            cur_shift = g_shift;
            size = g.font_size;
            batch_size = g.font_size;
            let mut positioned = g.clone();
            positioned.x = hanging_glyph_x(run, g, layout.hanging_punctuation);
            batch.push(positioned);
        }
        if let Some(f) = face
            && !batch.is_empty()
        {
            let dy = cur_shift.offset(batch_size);
            let glyphs = GlyphBatch {
                face_id: f,
                glyphs: &batch,
                size,
                color: resolve_rich_color(color, cur_color),
                line_y: run.line_y + dy,
                variations: &layout.font_variations,
            };
            self.flush_glyphs(scene, &glyphs, transform, clip);
        }
        if let Some(marker) = exercised_hyphen
            && let Some(hyphen) = self.visible_discretionary_hyphen(marker)
        {
            let global_byte = layout.glyph_global_byte(run.line_i, marker.start);
            let shift = layout.baseline_at(global_byte);
            let glyphs = GlyphBatch {
                face_id: hyphen.font_id,
                glyphs: std::slice::from_ref(&hyphen),
                size: hyphen.font_size,
                color: resolve_rich_color(color, hyphen.color_opt),
                line_y: run.line_y + shift.offset(hyphen.font_size),
                variations: &layout.font_variations,
            };
            self.flush_glyphs(scene, &glyphs, transform, clip);
        }
    }

    fn flush_glyphs(
        &mut self,
        scene: &mut Scene,
        batch: &GlyphBatch<'_>,
        transform: Affine,
        clip: Option<Rect>,
    ) {
        let Some(first) = batch.glyphs.first() else {
            return;
        };
        let Some(vello_font) = self.vello_font(batch.face_id, first.font_weight) else {
            return;
        };
        debug_assert!(
            batch
                .glyphs
                .iter()
                .all(|glyph| glyph.font_weight == first.font_weight)
        );
        let normalized_coords =
            self.normalized_coords(batch.face_id, batch.variations, first.font_weight);

        if let Some(clip) = clip
            && self.glyph_batch_outside(&vello_font, batch, &normalized_coords, transform, clip)
        {
            return;
        }

        let line_y = batch.line_y;
        let vello_glyphs = batch.glyphs.iter().map(|g| {
            let (x, y) = glyph_paint_origin(g, g.x, line_y);
            vello::Glyph {
                id: u32::from(g.glyph_id),
                x,
                y,
            }
        });

        scene
            .draw_glyphs(&vello_font)
            .font_size(batch.size)
            .normalized_coords(&normalized_coords)
            .brush(batch.color)
            .transform(transform)
            .draw(StyleRef::Fill(vello::peniko::Fill::NonZero), vello_glyphs);
    }

    fn glyph_batch_outside(
        &mut self,
        font: &Font,
        batch: &GlyphBatch<'_>,
        coords: &[vello::NormalizedCoord],
        transform: Affine,
        clip: Rect,
    ) -> bool {
        let GlyphBatch {
            face_id: face,
            glyphs,
            size,
            line_y,
            ..
        } = *batch;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the narrowing is intentional to reject transforms that overflow f32"
        )]
        let transform_finite = transform
            .as_coeffs()
            .iter()
            .all(|v| v.is_finite() && (*v as f32).is_finite());
        if ![clip.x0, clip.y0, clip.x1, clip.y1]
            .iter()
            .all(|v| v.is_finite())
            || !transform_finite
            || !size.is_finite()
            || size <= 0.0
            || !line_y.is_finite()
            || clip.x0 > clip.x1
            || clip.y0 > clip.y1
        {
            return false;
        }
        let Some(first) = glyphs.first() else {
            return true;
        };
        // Font identity and variation coordinates are shared by this batch.
        // Hash them once, then resolve individual glyphs by their small IDs.
        if self.ink_bounds_entries >= SHAPE_CACHE_LIMIT {
            self.ink_bounds.clear();
            self.ink_bounds_entries = 0;
        }
        let key = InkFontKey {
            blob: font.data.id(),
            index: font.index,
            face,
            weight: first.font_weight.0,
            size: size.to_bits(),
            coords: coords.to_vec(),
        };
        let cached = self.ink_bounds.entry(key).or_default();
        let entry_count = &mut self.ink_bounds_entries;
        let coefficients = transform
            .as_coeffs()
            .iter()
            .fold(1.0_f64, |m, v| m.max(v.abs()));
        glyphs.iter().all(|glyph| {
            let bounds = if let Some(bounds) = cached.get(&glyph.glyph_id) {
                *bounds
            } else {
                let bounds = outline_ink_bounds(font, glyph.glyph_id, size, coords);
                if *entry_count < SHAPE_CACHE_LIMIT {
                    cached.insert(glyph.glyph_id, bounds);
                    *entry_count = entry_count.saturating_add(1);
                }
                bounds
            };
            let Some(bounds) = bounds else {
                return false;
            };
            // Match flush_glyphs' f32 origin before transforming it.
            let (x, y) = glyph_paint_origin(glyph, glyph.x, line_y);
            let (x, y) = (f64::from(x), f64::from(y));
            if !x.is_finite() || !y.is_finite() {
                return false;
            }
            #[allow(clippy::arithmetic_side_effects, reason = "rectangle translation is floating-point arithmetic; nonfinite bounds are rejected below")]
            let local = bounds + vello::kurbo::Vec2::new(x, y);
            let projected = transform.transform_rect_bbox(local);
            let magnitude = [
                local.x0,
                local.y0,
                local.x1,
                local.y1,
                projected.x0,
                projected.y0,
                projected.x1,
                projected.y1,
            ]
            .iter()
            .fold(1.0_f64, |m, v| m.max(v.abs()));
            // Include pixel coverage and accumulated f32 transform/position rounding.
            let padding = 2.0 + 32.0 * f64::from(f32::EPSILON) * magnitude * coefficients;
            let ink = projected.inflate(padding, padding);
            [ink.x0, ink.y0, ink.x1, ink.y1]
                .iter()
                .all(|v| v.is_finite())
                && (ink.x1 < clip.x0 || ink.x0 > clip.x1 || ink.y1 < clip.y0 || ink.y0 > clip.y1)
        })
    }

    fn visible_discretionary_hyphen(
        &mut self,
        marker: &cosmic_text::LayoutGlyph,
    ) -> Option<cosmic_text::LayoutGlyph> {
        let font = self
            .font_system
            .get_font(marker.font_id, marker.font_weight)?;
        let charmap = font.as_swash().charmap();
        let mut glyph_id = charmap.map('\u{2010}');
        if glyph_id == 0 {
            glyph_id = charmap.map('-');
        }
        if glyph_id == 0 {
            return None;
        }
        let mut hyphen = marker.clone();
        hyphen.glyph_id = glyph_id;
        // The marker's GPOS offset was shaped for U+00AD, not this glyph.
        hyphen.x_offset = 0.0;
        hyphen.y_offset = 0.0;
        Some(hyphen)
    }
}

/// Whether a visual line cannot put ink inside `clip` (final scene space),
/// judged on its vertical extent only (alignment moves runs horizontally).
/// The line box is widened by two line heights above and below, which covers
/// ascenders, descenders and stacked marks; non-finite geometry and
/// transforms that rotate or shear are never culled.
pub(crate) fn rich_run_outside_clip(
    run: &cosmic_text::LayoutRun<'_>,
    transform: Affine,
    clip: Rect,
) -> bool {
    let [_, shear_y, shear_x, _, _, _] = transform.as_coeffs();
    if shear_x != 0.0 || shear_y != 0.0 {
        return false;
    }
    let margin = f64::from(run.line_height.max(1.0)) * 2.0;
    let local = Rect::new(
        0.0,
        f64::from(run.line_top) - margin,
        0.0,
        f64::from(run.line_top + run.line_height) + margin,
    );
    let screen = transform.transform_rect_bbox(local);
    // An invalid clip draws everything, as the per-glyph cull does.
    if ![screen.y0, screen.y1, clip.x0, clip.y0, clip.x1, clip.y1]
        .iter()
        .all(|value| value.is_finite())
        || clip.x0 > clip.x1
        || clip.y0 > clip.y1
    {
        return false;
    }
    screen.y1 < clip.y0 || screen.y0 > clip.y1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short centred line drawn in a wide box sits in the middle of the
    /// box, and an end-aligned one at its end: the draw keeps the width even
    /// though the line fits without wrapping.
    #[test]
    fn a_line_that_fits_is_drawn_where_its_alignment_puts_it() {
        let mut renderer = crate::font_catalog::fixture_renderer();
        let first_x = |renderer: &mut TextRenderer, align: ParagraphAlign| {
            let style = TextStyle::new(16.0).with_align(align);
            let width = renderer.collapse_unused_max_width(
                &CacheQuery::new("New note", &style),
                Some(400.0),
                &style,
                true,
            );
            let runs = renderer.shaped_runs("New note", width, &style);
            runs.iter()
                .flat_map(|run| run.glyphs.iter())
                .map(|glyph| glyph.x)
                .fold(f32::INFINITY, f32::min)
        };
        let start = first_x(&mut renderer, ParagraphAlign::Start);
        let center = first_x(&mut renderer, ParagraphAlign::Center);
        let end = first_x(&mut renderer, ParagraphAlign::End);
        let (natural, _) = renderer.measure_style("New note", None, &TextStyle::new(16.0));
        assert!(start.abs() < 1.0, "{start}");
        assert!(
            (center - (400.0 - natural) / 2.0).abs() < 2.0,
            "{center} of {natural}"
        );
        assert!((end - (400.0 - natural)).abs() < 2.0, "{end} of {natural}");
    }

    #[test]
    fn renderer_font_system_pins_families_weights_and_fallbacks() {
        // Case `explicit_catalog_constructs_the_production_renderer_and_caches`.
        {
            let catalog = crate::font_catalog::FontCatalog::controlled_fixture(
                crate::font_catalog::checkout_root(),
                "en-US",
            );
            let mut renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
            let first = renderer.measure("Reading", 16.0, None);
            let second = renderer.measure("Reading", 16.0, None);
            assert_eq!(first, second);
            assert!(first.0 > 0.0 && first.1 > 0.0);
            assert_eq!(renderer.hits, 1);
            assert_eq!(renderer.misses, 1);
        }

        // Case `product_font_system_pins_every_generic_family`.
        {
            let font_system = crate::font_catalog::fixture_font_system();
            let database = font_system.db();
            assert_eq!(database.family_name(&Family::SansSerif), UI_SANS_FAMILY);
            assert_eq!(database.family_name(&Family::Serif), UI_SERIF_FAMILY);
            assert_eq!(
                database.family_name(&Family::Monospace),
                UI_MONOSPACE_FAMILY
            );
        }

        // Case `shipped_fallback_policy_shapes_representative_scripts`.
        {
            let mut renderer = crate::font_catalog::fixture_renderer();
            let style = TextStyle::new(18.0);
            let samples = [
                ("Arabic", "مرحبا"),
                ("Hebrew", "שלום"),
                ("Devanagari", "नमस्ते"),
                ("Bengali", "বাংলা"),
                ("Thai", "สวัสดี"),
                ("CJK", "日本語"),
                ("symbols", "∞→✓"),
                ("emoji", "🙂"),
            ];

            for (script, sample) in samples {
                let runs = renderer.shaped_runs(sample, None, &style);
                let glyphs: Vec<_> = runs.iter().flat_map(|run| run.glyphs.iter()).collect();
                assert!(!glyphs.is_empty(), "{script} produced no shaped glyphs");
                assert!(
                    glyphs.iter().all(|glyph| glyph.id != 0),
                    "{script} fell through to a .notdef glyph; install the Tilo Noto fallback package"
                );
            }
        }

        // Case `named_weights_resolve_to_tailwind_numeric_values`:
        // A drift here would silently collapse every chrome label onto the
        // fallback weight.
        {
            assert_eq!(TextWeight::Regular.to_u16(), 400);
            assert_eq!(TextWeight::Medium.to_u16(), 500);
            assert_eq!(TextWeight::Semibold.to_u16(), 600);
            assert_eq!(TextWeight::Bold.to_u16(), 700);
            assert_eq!(TextWeight::Bold.to_cosmic_weight(), Weight(700));
            assert_eq!(TextWeight::Regular.to_cosmic_weight(), Weight::NORMAL);
            // Default must be Regular so call sites through `draw_text` keep
            // their 400-weight behaviour.
            assert_eq!(TextWeight::default(), TextWeight::Regular);
        }

        // Case `font_choice_families_resolve_and_shape`:
        // Two different families must be distinct shape-cache entries (so
        // serif doesn't collide with sans).
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let fams = tr.system_families();
            assert!(!fams.is_empty(), "the system font db exposes some families");

            // Both generic families measure to a positive advance for real text.
            let (ws, _) = tr.measure_font(
                "Reading",
                20.0,
                None,
                TextWeight::Regular,
                false,
                &FontChoice::Serif,
            );
            let (wn, _) = tr.measure_font(
                "Reading",
                20.0,
                None,
                TextWeight::Regular,
                false,
                &FontChoice::SansSerif,
            );
            assert!(ws > 0.0, "serif shapes to a real width");
            assert!(wn > 0.0, "sans shapes to a real width");

            // A specific installed family shapes into glyph runs without panicking.
            let mut scene = Scene::new();
            let named = FontChoice::Named(fams[0].clone());
            tr.draw_style(
                &mut scene,
                "Reading",
                Color::WHITE,
                Affine::IDENTITY,
                None,
                &TextStyle::new(20.0).with_family(named.clone()),
            );

            // Serif and Sans of the same string are distinct shape-cache entries.
            let _ = tr.take_shape_stats();
            tr.draw_style(
                &mut scene,
                "Reading",
                Color::WHITE,
                Affine::IDENTITY,
                None,
                &TextStyle::new(20.0).with_family(FontChoice::Serif),
            );
            tr.draw_style(
                &mut scene,
                "Reading",
                Color::WHITE,
                Affine::IDENTITY,
                None,
                &TextStyle::new(20.0).with_family(FontChoice::SansSerif),
            );
            let (h, m, _ns) = tr.take_shape_stats();
            assert_eq!(m, 0, "measure_font warms paint geometry for each family");
            assert_eq!(h, 2, "serif and sans remain distinct shape-cache entries");
        }
    }

    #[test]
    fn browser_missing_glyphs_demand_exact_assets_then_install_reshapes() {
        let catalog = crate::font_catalog::FontCatalog::product("", "en-US");
        let root = crate::font_catalog::checkout_root();
        let fonts = catalog
            .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
            .unwrap();
        let mut renderer = TextRenderer::with_browser_font_system(fonts);
        let style = TextStyle::new(18.0);
        for (text, expected) in [
            ("مَرْحَبًا", "NotoSansArabic[wght].ttf"),
            ("東京", "NotoSansCJK-VF.ttc"),
            ("🙂", "Noto-COLRv1.ttf"),
        ] {
            let before = renderer.shape_buffer(text, None, &style).buffer;
            assert!(
                before
                    .layout_runs()
                    .flat_map(|run| run.glyphs)
                    .any(|glyph| glyph.glyph_id == 0),
                "{text} must expose missing canonical glyphs before demand"
            );
            assert_eq!(renderer.take_browser_font_demands(), [expected]);
            let old_generation = renderer.generation;
            assert!(
                renderer
                    .install_browser_font(expected, std::fs::read(root.join(expected)).unwrap())
                    .unwrap()
            );
            assert_ne!(
                renderer.generation, old_generation,
                "font install must invalidate shape/measure caches"
            );
            let after = renderer.shape_buffer(text, None, &style).buffer;
            assert!(
                after
                    .layout_runs()
                    .flat_map(|run| run.glyphs)
                    .all(|glyph| glyph.glyph_id != 0),
                "{text} must reshape with the installed canonical font"
            );
            assert!(renderer.take_browser_font_demands().is_empty());
        }
    }

    #[test]
    fn browser_explicit_math_family_demands_its_face_while_core_emoji_stays_loaded() {
        let catalog = crate::font_catalog::FontCatalog::product("", "en-US");
        let root = crate::font_catalog::checkout_root();
        let fonts = catalog
            .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
            .unwrap();
        let mut renderer = TextRenderer::with_browser_font_system(fonts);
        let math = TextStyle::new(18.0).with_family(FontChoice::Named(UI_MATH_FAMILY.to_owned()));
        let before = renderer.shape_buffer("x", None, &math).buffer;
        assert!(
            before
                .layout_runs()
                .flat_map(|run| run.glyphs)
                .all(|glyph| {
                    renderer
                        .font_system
                        .db()
                        .face(glyph.font_id)
                        .is_none_or(|face| {
                            !face
                                .families
                                .iter()
                                .any(|family| family.0 == UI_MATH_FAMILY)
                        })
                })
        );
        assert_eq!(
            renderer.take_browser_font_demands(),
            ["NotoSansMath-Regular.ttf"]
        );
        renderer
            .install_browser_font(
                "NotoSansMath-Regular.ttf",
                std::fs::read(root.join("NotoSansMath-Regular.ttf")).unwrap(),
            )
            .unwrap();
        let after = renderer.shape_buffer("x", None, &math).buffer;
        assert!(after.layout_runs().flat_map(|run| run.glyphs).all(|glyph| {
            renderer
                .font_system
                .db()
                .face(glyph.font_id)
                .is_some_and(|face| {
                    face.families
                        .iter()
                        .any(|family| family.0 == UI_MATH_FAMILY)
                })
        }));
        assert!(renderer.take_browser_font_demands().is_empty());

        let core_emoji = renderer
            .shape_buffer("©®™", None, &TextStyle::new(18.0))
            .buffer;
        assert!(
            core_emoji
                .layout_runs()
                .flat_map(|run| run.glyphs)
                .all(|glyph| glyph.glyph_id != 0)
        );
        assert!(renderer.take_browser_font_demands().is_empty());
        let core_emoji_presentation = renderer
            .shape_buffer("‼️", None, &TextStyle::new(18.0))
            .buffer;
        assert!(
            core_emoji_presentation
                .layout_runs()
                .flat_map(|run| run.glyphs)
                .any(|glyph| glyph.glyph_id != 0),
            "a core glyph must not hide its requested emoji presentation"
        );
        assert_eq!(renderer.take_browser_font_demands(), ["Noto-COLRv1.ttf"]);
        renderer.shape_buffer("✈️", None, &TextStyle::new(18.0));
        assert_eq!(renderer.take_browser_font_demands(), ["Noto-COLRv1.ttf"]);
        renderer.shape_buffer("⌚", None, &TextStyle::new(18.0));
        assert_eq!(renderer.take_browser_font_demands(), ["Noto-COLRv1.ttf"]);
        renderer
            .install_browser_font(
                "Noto-COLRv1.ttf",
                std::fs::read(root.join("Noto-COLRv1.ttf")).unwrap(),
            )
            .unwrap();
        for emoji in ["‼️", "✈️", "⌚"] {
            let shaped = renderer
                .shape_buffer(emoji, None, &TextStyle::new(18.0))
                .buffer;
            let glyphs: Vec<_> = shaped
                .layout_runs()
                .flat_map(|run| run.glyphs)
                .filter(|glyph| glyph.glyph_id != 0)
                .collect();
            assert!(!glyphs.is_empty(), "{emoji} must shape a visible glyph");
            assert!(
                glyphs.iter().all(|glyph| renderer
                    .font_system
                    .db()
                    .face(glyph.font_id)
                    .is_some_and(|face| face
                        .families
                        .iter()
                        .any(|family| family.0 == "Noto Color Emoji"))),
                "{emoji} must reshape through the installed emoji family"
            );
        }
        assert!(renderer.take_browser_font_demands().is_empty());
    }

    #[test]
    fn failed_browser_fetch_rearms_cached_text_for_a_later_relevant_shape() {
        let catalog = crate::font_catalog::FontCatalog::product("", "en-US");
        let root = crate::font_catalog::checkout_root();
        let fonts = catalog
            .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
            .unwrap();
        let mut renderer = TextRenderer::with_browser_font_system(fonts);
        let style = TextStyle::new(18.0);
        renderer.measure_style("مَرْحَبًا", None, &style);
        assert_eq!(
            renderer.take_browser_font_demands(),
            ["NotoSansArabic[wght].ttf"]
        );
        renderer.measure_style("مَرْحَبًا", None, &style);
        assert!(
            renderer.take_browser_font_demands().is_empty(),
            "the cached measure does not spin demand"
        );
        renderer.browser_font_failed();
        renderer.measure_style("مَرْحَبًا", None, &style);
        assert_eq!(
            renderer.take_browser_font_demands(),
            ["NotoSansArabic[wght].ttf"]
        );
    }

    struct EveryThirdByte;

    impl Hyphenator for EveryThirdByte {
        fn breaks(&self, text: &str) -> Vec<usize> {
            (3..text.len())
                .step_by(3)
                .filter(|byte| text.is_char_boundary(*byte))
                .collect()
        }

        fn cache_key(&self) -> u64 {
            0x4355_5354_4f4d
        }
    }

    fn notched_region() -> TextFlowRegion {
        TextFlowRegion::polygon([
            Point::new(0.0, 0.0),
            Point::new(200.0, 0.0),
            Point::new(200.0, 120.0),
            Point::new(120.0, 120.0),
            Point::new(120.0, 30.0),
            Point::new(80.0, 30.0),
            Point::new(80.0, 120.0),
            Point::new(0.0, 120.0),
        ])
        .expect("valid concave flow polygon")
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "buckets glyph y-positions into small row indices for a three-line test layout; round() already yields an integer-valued f32 far below i32::MAX"
    )]
    #[test]
    fn line_breaking_follows_script_rules_hyphenation_and_polygon_flow() {
        // Case `custom_hyphenator_inserts_source_mapped_discretionary_breaks`.
        {
            let style = TextStyle::new(18.0).with_hyphenator(Arc::new(EveryThirdByte));
            let map = style.hyphenation.offset_map("customizable");
            assert_eq!(
                map.shape_text("customizable"),
                "cus\u{ad}tom\u{ad}iza\u{ad}ble"
            );
            assert_ne!(
                style.hyphenation.cache_hash(),
                Hyphenation::None.cache_hash()
            );
            for source in 0..="customizable".len() {
                assert_eq!(map.shaped_to_source(map.source_to_shaped(source)), source);
            }
        }

        // Case `polygon_flow_computes_conservative_spans_and_clips_paint`.
        {
            use vello_encoding::DrawTag;

            let region = notched_region();
            assert_eq!(region.spans_for_band(0.0, 20.0, 200.0), [0.0..200.0]);
            assert_eq!(
                region.spans_for_band(50.0, 20.0, 200.0),
                [0.0..80.0, 120.0..200.0]
            );

            let mut renderer = crate::font_catalog::fixture_renderer();
            let style = TextStyle::new(16.0).with_line_height(1.25);
            let text = "one two three four five six seven eight nine ten eleven twelve thirteen";
            let engine = PolygonTextLayout::new(region);
            let layout = engine.layout(
                &mut renderer,
                text,
                TextLayoutConstraints {
                    max_width: Some(200.0),
                    max_height: Some(120.0),
                },
                &style,
            );
            assert!(layout.lines.iter().any(|line| line.x == 0.0));
            assert!(layout.lines.iter().any(|line| line.x >= 120.0));
            assert!(layout.width <= 200.0);
            assert!(layout.height <= 120.0);

            let mut engine = engine;
            let mut scene = Scene::new();
            engine.draw(
                &mut renderer,
                &mut scene,
                text,
                TextLayoutConstraints {
                    max_width: Some(200.0),
                    max_height: Some(120.0),
                },
                &style,
                Color::WHITE,
                Affine::IDENTITY,
            );
            let begin = scene
                .encoding()
                .draw_tags
                .iter()
                .position(|tag| *tag == DrawTag::BEGIN_CLIP);
            let end = scene
                .encoding()
                .draw_tags
                .iter()
                .rposition(|tag| *tag == DrawTag::END_CLIP);
            assert!(begin.zip(end).is_some_and(|(begin, end)| begin < end));
        }

        // Case `wrap_lines_uses_cjk_opportunities_without_spaces`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let text = "東京京都大阪名古屋";
            let two_glyphs = tr.measure("東京", 14.0, None).0 + 0.5;
            let lines = tr.wrap_lines(text, &TextStyle::new(14.0), two_glyphs);
            assert!(lines.len() > 1, "CJK text wraps without ASCII spaces");
            assert_eq!(lines.concat(), text);
            assert!(lines.iter().all(|line| !line.is_empty()));
        }

        // Case `wrap_lines_respects_non_breaking_space`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let text = "10\u{a0}km away";
            let narrow = tr.measure("10", 14.0, None).0 + 1.0;
            let lines = tr.wrap_lines(text, &TextStyle::new(14.0), narrow);
            assert_eq!(lines.first().map(String::as_str), Some("10\u{a0}km"));
            assert_eq!(lines.join(" "), text);
        }

        // Case `wrap_lines_uses_dictionary_breaks_for_thai`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let text = "ทุกสองสัปดาห์";
            let first_word = &text[..9];
            let budget = tr.measure(first_word, 14.0, None).0 + 0.5;
            let lines = tr.wrap_lines(text, &TextStyle::new(14.0), budget);
            assert_eq!(lines, vec![&text[..9], &text[9..18], &text[18..]]);
            assert_eq!(lines.concat(), text);
        }

        // Case `measure_and_draw_use_dictionary_breaks_for_thai`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let style = TextStyle::new(18.0);
            let text = "ทุกสองสัปดาห์";
            let width = [&text[..9], &text[9..18], &text[18..]]
                .into_iter()
                .map(|word| tr.measure_style(word, None, &style).0)
                .fold(0.0f32, f32::max)
                + 0.5;

            let (_, height) = tr.measure_style(text, Some(width), &style);
            assert!(
                (height - style.line_height_px() * 3.0).abs() < 0.01,
                "plain widget measurement must use the three ICU word rows"
            );

            let runs = tr.shaped_runs(text, Some(width), &style);
            let mut rows: Vec<i32> = runs
                .iter()
                .flat_map(|run| run.glyphs.iter())
                .map(|glyph| (glyph.y / style.line_height_px()).round() as i32)
                .collect();
            rows.sort_unstable();
            rows.dedup();
            assert_eq!(rows.len(), 3, "paint geometry must match measured wrapping");
        }

        // Case `automatic_hyphenation_wraps_and_paints_only_exercised_breaks`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let text = "extensive";
            let plain = TextStyle::new(20.0);
            let hyphenated = plain.clone().with_auto_hyphenation("en-US");
            let width = tr.measure_style("exten-", None, &plain).0 + 0.5;

            let (_, plain_height) = tr.measure_style(text, Some(width), &plain);
            let (hyphenated_width, hyphenated_height) =
                tr.measure_style(text, Some(width), &hyphenated);
            assert_eq!(plain_height, plain.line_height_px());
            assert!(hyphenated_height >= hyphenated.line_height_px() * 2.0);
            assert!(
                hyphenated_width <= width + 0.01,
                "the visible discretionary glyph is included in line fitting"
            );

            let narrow = tr.shaped_runs(text, Some(width), &hyphenated);
            let narrow_glyphs = narrow.iter().map(|run| run.glyphs.len()).sum::<usize>();
            assert!(
                narrow_glyphs > text.chars().count(),
                "an exercised discretionary break paints one additional hyphen glyph"
            );

            let wide = tr.shaped_runs(text, Some(10_000.0), &hyphenated);
            let wide_glyphs = wide.iter().map(|run| run.glyphs.len()).sum::<usize>();
            assert_eq!(
                wide_glyphs,
                text.chars().count(),
                "unused discretionary opportunities stay invisible"
            );
        }
    }

    #[test]
    fn measure_cache_hits_and_keys_on_size_policy_and_generation() {
        // Case `measure_cache_returns_hit_on_second_call`:
        // Measuring the same string twice should hit the cache the second time
        // — without this, every widget's `draw` pays a full cosmic-text shape.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let label = "Hello, gallery!";
            let (w1, h1) = tr.measure(label, 16.0, None);
            let (hits1, misses1) = tr.cache_stats();
            assert_eq!(misses1, 1);
            assert_eq!(hits1, 0);

            let (w2, h2) = tr.measure(label, 16.0, None);
            let (hits2, misses2) = tr.cache_stats();
            assert_eq!(hits2, 1, "second measure of same string must hit");
            assert_eq!(misses2, 1, "miss count unchanged after a hit");
            assert_eq!((w1, h1), (w2, h2), "cached value must equal recomputed");
        }

        // Case `measure_cache_keys_on_size`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let _ = tr.measure("abc", 12.0, None);
            let _ = tr.measure("abc", 16.0, None);
            let _ = tr.measure("abc", 12.0, None); // hit
            let (hits, misses) = tr.cache_stats();
            assert_eq!(hits, 1);
            assert_eq!(misses, 2);
        }

        // Case `hyphenation_policy_participates_in_measurement_cache_identity`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let text = "extensive";
            // Wide enough that plain text collapses to an unconstrained key;
            // hyphenation keeps the wrap width, so the two policies stay distinct.
            let width = 1000.0;
            let plain = TextStyle::new(20.0);
            let automatic = plain.clone().with_auto_hyphenation("en");
            let _ = tr.measure_style(text, Some(width), &plain);
            let _ = tr.measure_style(text, Some(width), &automatic);
            let (hits, misses) = tr.cache_stats();
            // Plain collapses to unconstrained (probe miss + re-lookup hit);
            // hyphenation keeps the wrap width as a second distinct miss.
            assert_eq!((hits, misses), (1, 2));
        }

        // Case `bump_generation_invalidates_cache`:
        // `bump_generation` blows away every cached entry — the next
        // measure of a previously-seen string registers as a miss.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let _ = tr.measure("abc", 12.0, None);
            let _ = tr.measure("abc", 12.0, None); // hit
            tr.bump_generation();
            let _ = tr.measure("abc", 12.0, None); // miss after bump
            let (hits, misses) = tr.cache_stats();
            assert_eq!(hits, 1);
            assert_eq!(misses, 2);
        }
    }

    #[test]
    fn style_is_shared_by_measurement_and_drawing_including_backgrounds() {
        // Case `style_line_height_controls_layout_and_cache_identity`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let compact = TextStyle::new(20.0).with_line_height(1.2);
            let relaxed = TextStyle::new(20.0).with_line_height(1.6);

            let (_, compact_h) = tr.measure_style("first\nsecond", None, &compact);
            let (_, relaxed_h) = tr.measure_style("first\nsecond", None, &relaxed);
            assert!((compact_h - 48.0).abs() < 0.01);
            assert!((relaxed_h - 64.0).abs() < 0.01);

            let (hits, misses) = tr.cache_stats();
            assert_eq!(hits, 0);
            assert_eq!(misses, 2, "different leading must not alias in the cache");
            let _ = tr.measure_style("first\nsecond", None, &relaxed);
            let (hits, misses) = tr.cache_stats();
            assert_eq!(hits, 1);
            assert_eq!(misses, 2);
        }

        // Case `style_is_shared_by_measurement_and_drawing`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let style = TextStyle::new(18.0)
                .with_line_height(1.4)
                .with_weight(TextWeight::Semibold)
                .with_italic(true)
                .with_family(FontChoice::Serif);
            let _ = tr.measure_style("Consistent typography", Some(180.0), &style);
            let _ = tr.take_shape_stats();

            let mut scene = Scene::new();
            tr.draw_style(
                &mut scene,
                "Consistent typography",
                Color::WHITE,
                Affine::IDENTITY,
                Some(180.0),
                &style,
            );
            let (hits, misses, _ns) = tr.take_shape_stats();
            assert_eq!(
                misses, 0,
                "measure warms shape so the matching draw does not reshape"
            );
            assert_eq!(hits, 1, "draw reuses the geometry measure produced");
            assert!(
                !scene.encoding().resources.glyph_runs.is_empty(),
                "the complete style produces drawable glyph geometry"
            );
        }

        // Case `background_span_emits_an_extra_fill_in_the_scene`:
        // A highlighted run must add a real fill: the highlighted scene must
        // encode strictly more paths than the plain one.
        {
            use crate::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
            let mut tr = crate::font_catalog::fixture_renderer();

            let plain_spans = [RichSpan::new("highlighted")];
            let plain = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &plain_spans,
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(400.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            let hl_spans = [RichSpan::new("highlighted").background(Some([255, 235, 59]))];
            let highlighted = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &hl_spans,
                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                Some(400.0),
                TextAlign::Left,
                vec![],
                vec![],
            );

            let mut plain_scene = Scene::new();
            tr.draw_rich(
                &mut plain_scene,
                &plain,
                Affine::IDENTITY,
                Color::from_rgb8(0, 0, 0),
            );
            let plain_paths = plain_scene.encoding().n_paths;

            let mut hl_scene = Scene::new();
            tr.draw_rich(
                &mut hl_scene,
                &highlighted,
                Affine::IDENTITY,
                Color::from_rgb8(0, 0, 0),
            );
            let hl_paths = hl_scene.encoding().n_paths;

            assert!(
                hl_paths > plain_paths,
                "highlight must add a fill path: plain={plain_paths} highlighted={hl_paths}",
            );
        }
    }

    #[test]
    fn styled_line_clamping_keeps_drawn_weight_within_the_budget() {
        for scale in [1.0, 2.0] {
            with_layout_renderer(scale, |renderer| {
                let title = "Review the launch announcement, compact navigation, translated labels, and final release checklist";
                let style = TextStyle::new(13.0).with_weight(TextWeight::Semibold);
                let width = 240.0;
                assert!(renderer.measure_style(title, None, &style).0 > width);
                let lines = renderer.clamp_to_lines_style(title, width, 2, &style);
                assert_eq!(lines.len(), 2);
                assert!(lines.last().is_some_and(|line| line.ends_with(ELLIPSIS)));
                for line in &lines {
                    let (actual_width, height) = renderer.measure_style(line, None, &style);
                    assert!(height > 0.0, "real glyph metrics must be observable");
                    assert!(actual_width <= width + 0.1, "{line:?} exceeds {width}");
                }
                assert_eq!(
                    renderer.clamp_to_lines_style("Review", width, 2, &style),
                    ["Review"]
                );
                assert!(
                    renderer
                        .clamp_to_lines_style(title, width, 0, &style)
                        .is_empty()
                );
                assert_eq!(
                    renderer.clamp_to_lines_style(title, width, 1, &style),
                    [renderer.elide_to_width_style(title, width, &style)]
                );
                let unbreakable = "abcdefghijklmnopqrstuvwxyzzabcdefghijklmnopqrstuvwxyzz";
                let lines = renderer.clamp_to_lines_style(unbreakable, width, 2, &style);
                assert_eq!(lines.len(), 1);
                assert!(lines[0].ends_with(ELLIPSIS));
                assert!(renderer.measure_style(&lines[0], None, &style).0 <= width + 0.1);
            });
        }
    }

    #[test]
    fn elide_and_clamp_truncate_at_grapheme_boundaries() {
        // Case `elide_returns_text_unchanged_when_it_fits`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let s = "short";
            let (w, _) = tr.measure(s, 14.0, None);
            let out = tr.elide_to_width(s, 14.0, w + 50.0);
            assert_eq!(out, s, "fitting text must be returned unchanged");
            assert!(!out.ends_with('\u{2026}'), "no ellipsis when it fits");
        }

        // Case `elide_truncates_and_appends_ellipsis`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let s = "the quick brown fox jumps over the lazy dog";
            let (full_w, _) = tr.measure(s, 14.0, None);
            let budget = full_w / 3.0; // force truncation
            let out = tr.elide_to_width(s, 14.0, budget);
            assert!(
                out.ends_with('\u{2026}'),
                "truncated text must end with …, got {out:?}"
            );
            assert!(
                out.chars().count() < s.chars().count(),
                "must drop characters"
            );
            let (out_w, _) = tr.measure(&out, 14.0, None);
            assert!(
                out_w <= budget + 0.5,
                "elided string ({out_w}) must fit the budget ({budget})",
            );
        }

        // Case `elide_never_splits_a_grapheme_and_drops_space_before_the_ellipsis`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            // "e" + combining acute, repeated: a char-wise cut could strand the
            // accent from its base letter.
            let s = "cafe\u{301} cafe\u{301} cafe\u{301} cafe\u{301}";
            for budget in [10.0_f32, 25.0, 40.0, 55.0, 70.0, 85.0] {
                let out = tr.elide_to_width(s, 14.0, budget);
                let kept = out.trim_end_matches(ELLIPSIS);
                assert!(s.starts_with(kept), "{out:?} is not a prefix of the text");
                let rest = s.get(kept.len()..).unwrap_or_default();
                assert!(
                    !rest.starts_with('\u{301}'),
                    "{out:?} strands a combining mark"
                );
                assert!(!kept.ends_with(' '), "space before the ellipsis in {out:?}");
                if out != ELLIPSIS.to_string() {
                    assert!(
                        tr.measure(&out, 14.0, None).0 <= budget,
                        "{out:?} overflows {budget}"
                    );
                }
            }
        }

        // Case `elide_keeps_the_longest_prefix_that_fits`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let s = "the quick brown fox jumps over the lazy dog";
            let (full_w, _) = tr.measure(s, 14.0, None);
            let budget = full_w / 2.0;
            let out = tr.elide_to_width(s, 14.0, budget);
            let kept = out.trim_end_matches(ELLIPSIS);
            // One more character (plus the ellipsis) must no longer fit.
            let rest = s.get(kept.len()..).unwrap_or_default();
            let next_end = kept.len() + rest.chars().next().map_or(0, char::len_utf8);
            let longer = format!(
                "{}{ELLIPSIS}",
                s.get(..next_end).unwrap_or_default().trim_end()
            );
            if longer != out {
                assert!(
                    tr.measure(&longer, 14.0, None).0 > budget,
                    "{longer:?} would still fit"
                );
            }
        }

        // Case `elide_measures_in_the_given_style`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let s = "fn render_scene(scene: &mut Scene, transform: Affine)";
            let mono = TextStyle::new(13.0).with_family(FontChoice::Monospace);
            let budget = tr.measure_style(s, None, &mono).0 / 2.0;
            let out = tr.elide_to_width_style(s, budget, &mono);
            assert!(out.ends_with(ELLIPSIS));
            assert!(tr.measure_style(&out, None, &mono).0 <= budget);
        }

        // Case `elide_edge_cases`:
        // A zero/empty budget yields empty output; so does empty input.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            assert_eq!(tr.elide_to_width("", 14.0, 100.0), "");
            assert_eq!(tr.elide_to_width("abc", 14.0, 0.0), "");
        }

        // Case `clamp_to_lines_caps_line_count_and_elides_last`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let paragraph = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor \
             incididunt ut labore et dolore magna aliqua ut enim ad minim veniam quis";
            // Narrow width forces several wrapped lines; cap to 2.
            let lines = tr.clamp_to_lines(paragraph, &TextStyle::new(14.0), 120.0, 2);
            assert!(
                lines.len() <= 2,
                "must not exceed max_lines, got {}",
                lines.len()
            );
            assert_eq!(lines.len(), 2, "this paragraph wraps to more than 2 lines");
            assert!(
                lines.last().unwrap().ends_with('\u{2026}'),
                "overflowing clamp must elide the final line: {:?}",
                lines.last(),
            );
        }

        // Case `clamp_to_lines_degenerate_cases`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            assert!(
                tr.clamp_to_lines("anything", &TextStyle::new(14.0), 80.0, 0)
                    .is_empty()
            );
            let one = tr.clamp_to_lines(
                "a fairly long single line of text here",
                &TextStyle::new(14.0),
                60.0,
                1,
            );
            assert_eq!(one.len(), 1);
            assert!(one[0].ends_with('\u{2026}'), "single-line clamp elides");
        }

        // Case `clamp_to_lines_no_overflow_keeps_all`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let lines = tr.clamp_to_lines("two words", &TextStyle::new(14.0), 1000.0, 3);
            assert_eq!(lines, vec!["two words".to_string()]);
            assert!(!lines[0].ends_with('\u{2026}'));
        }
    }

    #[test]
    fn shape_cache_reuses_geometry_and_keys_on_width_and_generation() {
        // Case `shape_cache_reshapes_once_for_repeated_draw`:
        // The DRAW path caches shaped glyph geometry: drawing the same string
        // twice shapes it once.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let mut scene = Scene::new();
            let t = Affine::IDENTITY;
            let c = Color::WHITE;
            tr.draw_text_weighted(&mut scene, "Equals =", 20.0, c, t, None, TextWeight::Bold);
            let (_h, m1, _ns1) = tr.take_shape_stats();
            assert_eq!(m1, 1, "first draw must shape once (miss)");

            // Every redraw must be a cache hit; zero reshapes.
            for _ in 0..5 {
                tr.draw_text_weighted(&mut scene, "Equals =", 20.0, c, t, None, TextWeight::Bold);
            }
            let (h, m, ns) = tr.take_shape_stats();
            assert_eq!(m, 0, "repeated identical draws must not reshape");
            assert_eq!(h, 5, "all five redraws must hit the shape cache");
            assert_eq!(ns, 0, "no shaping time on an all-hit frame");
        }

        // Case `measure_warms_shape_cache_for_matching_draw_width`:
        // Layout measures a label, then paint draws it inside a wider box.
        // Collapsing an unused wrap width plus warming shape from measure must
        // make that first paint a shape-cache hit (one cosmic-text reshape total).
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let style = TextStyle::new(16.0);
            let label = "Send message";
            let (natural_w, _) = tr.measure_style(label, None, &style);
            let _ = tr.take_shape_stats();

            let mut scene = Scene::new();
            tr.draw_style(
                &mut scene,
                label,
                Color::WHITE,
                Affine::IDENTITY,
                Some(natural_w + 40.0),
                &style,
            );
            let (h, m, ns) = tr.take_shape_stats();
            assert_eq!(m, 0, "draw inside a wider box must not reshape");
            assert_eq!(h, 1, "draw must hit the shape entry measure warmed");
            assert_eq!(ns, 0, "no shaping time when measure already warmed paint");
        }

        // Case `shape_cache_reuses_geometry_across_recolor`:
        // Colour is NOT part of the shape key — a theme recolour of the same
        // label reuses the cached glyph geometry, making theme switches free.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let mut scene = Scene::new();
            let t = Affine::IDENTITY;
            tr.draw_text_weighted(
                &mut scene,
                "Clear",
                16.0,
                Color::WHITE,
                t,
                None,
                TextWeight::Regular,
            );
            let _ = tr.take_shape_stats();
            // Same string + size + weight, different colour (dark→light theme).
            tr.draw_text_weighted(
                &mut scene,
                "Clear",
                16.0,
                Color::BLACK,
                t,
                None,
                TextWeight::Regular,
            );
            let (h, m, _ns) = tr.take_shape_stats();
            assert_eq!(m, 0, "recolour must not reshape");
            assert_eq!(h, 1, "recolour must hit the cached geometry");
        }

        // Case `shape_cache_keys_on_width`:
        // A different wrap width is a different shape — wrapping changes
        // glyph positions, so it must miss and reshape. Both widths must
        // actually wrap; an unused wider box collapses to unconstrained.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let mut scene = Scene::new();
            let t = Affine::IDENTITY;
            let s = "the quick brown fox jumps over the lazy dog";
            tr.draw_text_weighted(
                &mut scene,
                s,
                16.0,
                Color::WHITE,
                t,
                Some(120.0),
                TextWeight::Regular,
            );
            tr.draw_text_weighted(
                &mut scene,
                s,
                16.0,
                Color::WHITE,
                t,
                Some(60.0),
                TextWeight::Regular,
            );
            tr.draw_text_weighted(
                &mut scene,
                s,
                16.0,
                Color::WHITE,
                t,
                Some(120.0),
                TextWeight::Regular,
            ); // hit
            let (h, m, _ns) = tr.take_shape_stats();
            assert_eq!(m, 2, "each distinct wrapping width shapes once");
            assert_eq!(h, 1, "the repeated width hits");
        }

        // Case `bump_generation_invalidates_shape_cache`:
        // `bump_generation` (locale / font hot-swap) invalidates shaped
        // geometry too, so the next draw reshapes.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let mut scene = Scene::new();
            let t = Affine::IDENTITY;
            tr.draw_text_weighted(
                &mut scene,
                "Send",
                16.0,
                Color::WHITE,
                t,
                None,
                TextWeight::Regular,
            );
            let _ = tr.take_shape_stats();
            tr.draw_text_weighted(
                &mut scene,
                "Send",
                16.0,
                Color::WHITE,
                t,
                None,
                TextWeight::Regular,
            );
            let (h, _m, _ns) = tr.take_shape_stats();
            assert_eq!(h, 1, "second draw hits before bump");
            tr.bump_generation();
            tr.draw_text_weighted(
                &mut scene,
                "Send",
                16.0,
                Color::WHITE,
                t,
                None,
                TextWeight::Regular,
            );
            let (_h, m, _ns) = tr.take_shape_stats();
            assert_eq!(m, 1, "draw after bump must reshape");
        }
    }

    #[test]
    fn font_features_and_variation_axes_shape_and_measure() {
        // Case `advanced_features_are_part_of_shaping_and_measurement`.
        {
            let mut renderer = crate::font_catalog::fixture_renderer();
            let base = TextStyle::new(32.0).with_family(FontChoice::Serif);
            let kerned =
                renderer.measure_style("AVATAR To", None, &base.clone().with_kerning(true));
            let unkerned =
                renderer.measure_style("AVATAR To", None, &base.clone().with_kerning(false));
            assert!(
                (kerned.0 - unkerned.0).abs() > 0.1,
                "explicit kerning must alter positioned advances"
            );

            let normal = renderer.measure_style("TRACKING", None, &base);
            let tracked =
                renderer.measure_style("TRACKING", None, &base.clone().with_tracking(0.08));
            assert!(tracked.0 > normal.0, "positive tracking must widen the run");

            let mut scene = Scene::new();
            let _ = renderer.take_shape_stats();
            renderer.draw_style(
                &mut scene,
                "Features",
                Color::WHITE,
                Affine::IDENTITY,
                None,
                &base,
            );
            renderer.draw_style(
                &mut scene,
                "Features",
                Color::WHITE,
                Affine::IDENTITY,
                None,
                &base.clone().with_small_caps(true),
            );
            let (_, misses, _) = renderer.take_shape_stats();
            assert_eq!(misses, 2, "feature settings must not collide in the cache");
        }

        // Case `feature_numeric_and_variation_builders_replace_their_own_settings`.
        {
            let style = TextStyle::new(20.0)
                .with_font_feature(OpenTypeTag::KERNING, 0)
                .with_kerning(true)
                .with_numeric_style(NumericStyle::ProportionalLining)
                .with_numeric_style(NumericStyle::TabularOldstyle)
                .with_font_variation(OpenTypeTag::WEIGHT, 310.0)
                .with_font_variation(OpenTypeTag::WEIGHT, 675.0)
                .with_optical_sizing(OpticalSizing::Auto);
            assert_eq!(
                style
                    .font_features
                    .iter()
                    .find(|feature| feature.tag == OpenTypeTag::KERNING)
                    .map(|feature| feature.value),
                Some(1)
            );
            assert!(style.font_features.iter().any(|feature| {
                feature.tag == OpenTypeTag::TABULAR_NUMERALS && feature.value == 1
            }));
            assert!(style.font_features.iter().any(|feature| {
                feature.tag == OpenTypeTag::OLDSTYLE_NUMERALS && feature.value == 1
            }));
            assert_eq!(style.effective_weight().0, 675);
            assert_eq!(
                style
                    .resolved_variations()
                    .iter()
                    .find(|axis| axis.tag == OpenTypeTag::OPTICAL_SIZE)
                    .map(|axis| axis.value),
                Some(20.0)
            );
            assert_eq!(
                style
                    .font_variations
                    .iter()
                    .filter(|axis| axis.tag == OpenTypeTag::WEIGHT)
                    .count(),
                1
            );
        }

        // Case `variable_axes_are_normalized_in_the_faces_native_axis_order`.
        {
            let renderer = crate::font_catalog::fixture_renderer();
            let face = renderer
                .font_system
                .db()
                .query(&cosmic_text::fontdb::Query {
                    families: &[Family::Name("Adwaita Sans")],
                    ..cosmic_text::fontdb::Query::default()
                })
                .expect("the controlled fixture ships Adwaita Sans");
            let coords = renderer.normalized_coords(
                face,
                &[
                    FontVariation {
                        tag: OpenTypeTag::WEIGHT,
                        value: 700.0,
                    },
                    FontVariation {
                        tag: OpenTypeTag::OPTICAL_SIZE,
                        value: 32.0,
                    },
                ],
                Weight(700),
            );
            assert_eq!(coords.len(), 2, "Adwaita exposes opsz and wght");
            assert!(
                coords.iter().all(|coordinate| *coordinate > 0),
                "both requested values sit above the face defaults: {coords:?}"
            );
        }
    }

    #[test]
    fn text_and_ui_scale_apply_once_to_metrics_and_caches() {
        // Case `text_scale_one_is_metric_identity_and_invalid_values_are_safe`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let base = tr.measure("Accessible calendar", 18.0, None);
            tr.set_text_scale(1.0);
            assert_eq!(tr.measure("Accessible calendar", 18.0, None), base);

            tr.set_text_scale(f32::NAN);
            assert_eq!(tr.text_scale, 1.0, "NaN resets to the safe identity scale");
            tr.set_text_scale(100.0);
            assert_eq!(tr.text_scale, 4.0, "oversized scales are bounded");
            tr.set_text_scale(0.0);
            assert_eq!(tr.text_scale, 0.5, "undersized scales are bounded");
        }

        // Case `text_scale_changes_measure_and_shape_cache_together`.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let base = tr.measure("Calendar", 20.0, None);
            tr.set_text_scale(2.0);
            let doubled = tr.measure("Calendar", 20.0, None);
            assert!(
                (doubled.0 - base.0 * 2.0).abs() < 0.1 && (doubled.1 - base.1 * 2.0).abs() < 0.1,
                "measurement uses the same scaled metrics that drawing receives: {base:?} -> {doubled:?}"
            );

            // Measure already warmed both scaled geometries; drawing must hit
            // each once rather than reshaping.
            let mut scene = Scene::new();
            let _ = tr.take_shape_stats();
            tr.draw_text(
                &mut scene,
                "Calendar",
                20.0,
                Color::WHITE,
                Affine::IDENTITY,
                None,
            );
            tr.set_text_scale(1.0);
            tr.draw_text(
                &mut scene,
                "Calendar",
                20.0,
                Color::WHITE,
                Affine::IDENTITY,
                None,
            );
            let (hits, misses, _ns) = tr.take_shape_stats();
            assert_eq!(misses, 0, "scaled measures warm the paint-side cache");
            assert_eq!(
                hits, 2,
                "each scaled style owns a distinct shape-cache entry"
            );
        }

        // Case `custom_canvas_styles_remain_renderer_relative_at_supported_scales`.
        {
            for scale in [1.0, 1.25, 1.5, 2.0] {
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(scale);
                let style = TextStyle::new(14.0).with_letter_spacing(2.0);
                let (_, height) = renderer.measure_style("Canvas", None, &style);
                assert!(
                    (height - 14.0 * 1.25 * scale).abs() < 0.001,
                    "custom canvas measure at {scale}x: {height}"
                );

                let mut scene = Scene::new();
                renderer.draw_style(
                    &mut scene,
                    "Canvas",
                    Color::WHITE,
                    Affine::IDENTITY,
                    None,
                    &style,
                );
                let runs = &scene.encoding().resources.glyph_runs;
                assert!(!runs.is_empty(), "custom canvas draw emitted no glyphs");
                for run in runs {
                    assert!(
                        (run.font_size - 14.0 * scale).abs() < 0.001,
                        "custom canvas draw at {scale}x: {}",
                        run.font_size
                    );
                }
            }
        }

        // Case `resolved_styles_keep_metrics_and_glyph_geometry_across_renderer_scales`.
        {
            let text = "Tracked resolved text";
            let mut resolved = TextStyle::new(28.0).with_letter_spacing(3.0);
            resolved.scaling = TextScaling::Resolved;
            let mut renderer = crate::font_catalog::fixture_renderer();

            let legacy_at_one = renderer.measure_style(text, None, &TextStyle::new(14.0));
            let resolved_at_one = renderer.measure_style(text, None, &resolved);
            let mut scene_at_one = Scene::new();
            renderer.draw_style(
                &mut scene_at_one,
                text,
                Color::WHITE,
                Affine::IDENTITY,
                None,
                &resolved,
            );

            renderer.set_text_scale(2.0);
            let legacy_at_two = renderer.measure_style(text, None, &TextStyle::new(14.0));
            assert!((legacy_at_two.1 - legacy_at_one.1 * 2.0).abs() < 0.001);

            let resolved_at_two = renderer.measure_style(text, None, &resolved);
            assert_eq!(resolved_at_two, resolved_at_one);

            let mut scene_at_two = Scene::new();
            renderer.draw_style(
                &mut scene_at_two,
                text,
                Color::WHITE,
                Affine::IDENTITY,
                None,
                &resolved,
            );
            let first = scene_at_one.encoding();
            let second = scene_at_two.encoding();
            assert_eq!(first.resources.glyphs.len(), second.resources.glyphs.len());
            for (left, right) in first.resources.glyphs.iter().zip(&second.resources.glyphs) {
                assert_eq!(left.id, right.id);
                assert_eq!(left.x, right.x);
                assert_eq!(left.y, right.y);
            }
            assert_eq!(
                first.resources.glyph_runs.len(),
                second.resources.glyph_runs.len()
            );
            for (left, right) in first
                .resources
                .glyph_runs
                .iter()
                .zip(&second.resources.glyph_runs)
            {
                assert_eq!(left.font_size, right.font_size);
                assert_eq!(left.transform, right.transform);
                assert_eq!(left.glyphs, right.glyphs);
            }
        }

        // Case `resolved_theme_style_applies_ui_and_text_scale_once`.
        {
            let role = crate::tokens::TextRole::Caption;
            let base = crate::theme::Theme::dark().text(role).size;
            let theme = crate::theme::Theme::dark()
                .with_ui_scale(1.25)
                .with_text_scale(2.0);
            let resolved = theme.text(role);
            let mut renderer = crate::font_catalog::fixture_renderer();
            renderer.set_text_scale(2.0);
            let (_, height) = renderer.measure_style("Scale once", None, &resolved);
            assert!((height - base * 1.25 * 2.0 * resolved.line_height).abs() < 0.001);

            // Content text ignores the renderer's accessibility scale too.
            let content = TextStyle::content(19.0);
            let (_, content_height) = renderer.measure_style("Page", None, &content);
            assert!((content_height - 19.0 * content.line_height).abs() < 0.001);
        }
    }
}

#[cfg(test)]
mod renderer_regressions;

#[cfg(test)]
mod reading_faces;

#[cfg(test)]
mod layout_renderer_pool {
    use super::*;

    #[test]
    fn layout_passes_reuse_pooled_renderers_instead_of_reloading_fonts() {
        let constructions = || FONT_SYSTEM_CONSTRUCTIONS.with(std::cell::Cell::get);
        FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(0));
        for _ in 0..8 {
            with_layout_renderer(1.0, |tr| {
                tr.measure("Files", 14.0, None);
            });
        }
        assert_eq!(
            constructions(),
            1,
            "a repeated layout pass loads fonts once"
        );

        let scales = with_layout_renderer(2.0, |outer| {
            let inner = with_layout_renderer(1.5, |inner| inner.text_scale());
            (outer.text_scale(), inner)
        });
        assert_eq!(
            scales,
            (2.0, 1.5),
            "a nested pass has its own renderer and scale"
        );
        assert_eq!(constructions(), 2, "nesting adds one renderer to the pool");
        with_layout_renderer(1.0, |_| with_layout_renderer(1.0, |_| ()));
        assert_eq!(constructions(), 2, "and later nested passes reuse it");
    }
}

#[cfg(test)]
#[path = "text_render/main_compatibility_tests.rs"]
mod main_compatibility_tests;
