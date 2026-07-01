//! Pure data model for Books — no CRDT, no rendering.
//!
//! Two families live here:
//!
//! 1. **Book content** ([`Book`] / [`Chapter`] / [`Block`] / [`Span`]) — the
//!    parsed, reflowable representation of an EPUB. Immutable once parsed;
//!    treated as a render cache, NOT stored in the CRDT (it's large and
//!    regenerable from the bundled `.epub`).
//!
//! 2. **The shared annotation model** ([`Annotation`] / [`AnnoKind`] /
//!    [`HiColor`]) — this is the part the PDF reader and word processor are
//!    meant to reuse, so it is deliberately document-agnostic. An annotation
//!    is an **anchor** (which document, where) plus a **payload** (what kind,
//!    what colour, what note). See [`Annotation`] for the anchoring contract.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ===========================================================================
// Book content
// ===========================================================================

/// Decoded, downscaled cover artwork extracted from an EPUB. Held behind an
/// `Arc` so a [`Book`] stays cheap to clone and the pixels are decoded exactly
/// once (at load), never per frame. `rgba` is tightly packed 8-bit RGBA of
/// `width * height` pixels.
#[derive(Clone, Debug)]
pub struct Cover {
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// One reflowable book: ordered chapters (the EPUB spine) plus metadata.
#[derive(Clone, Debug, Default)]
pub struct Book {
    /// Stable id used as the key into the CRDT library + annotation store.
    pub id: String,
    pub title: String,
    pub author: String,
    pub publisher: String,
    pub description: String,
    pub chapters: Vec<Chapter>,
    /// Index into the theme accent/semantic ramps used to tint the spine on
    /// the shelf. Purely cosmetic; resolved against the live `Theme`.
    pub cover_tint: usize,
    /// Short subject line shown on the cover, e.g. "Fable".
    pub subject: String,
    /// Real cover artwork decoded from the EPUB, when it carries one. `None`
    /// falls back to a generated spine tinted by [`Self::cover_tint`].
    pub cover: Option<Cover>,
}

impl Book {
    /// Total number of blocks across every chapter — a cheap proxy for length.
    pub fn block_count(&self) -> usize {
        self.chapters.iter().map(|c| c.blocks.len()).sum()
    }

    pub fn chapter(&self, i: usize) -> Option<&Chapter> {
        self.chapters.get(i)
    }
}

/// One spine document (an XHTML file in the EPUB), parsed into blocks.
#[derive(Clone, Debug, Default)]
pub struct Chapter {
    /// TOC label (from the nav/ncx), falling back to the first heading.
    pub title: String,
    /// The XHTML href this chapter came from (used to resolve TOC targets).
    pub href: String,
    pub blocks: Vec<Block>,
}

impl Chapter {
    /// Plain text of one block (spans concatenated) — used for annotation
    /// excerpts and word counts.
    pub fn block_text(&self, i: usize) -> String {
        self.blocks.get(i).map(|b| b.plain_text()).unwrap_or_default()
    }
}

/// A flow-level block. Kinds map to the structural HTML elements an EPUB
/// chapter actually uses; styling (size, weight, indent) is derived from the
/// kind at layout time, never stored here.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// `<h1>`..`<h6>` — `level` is 1-based and clamped to 1..=3 for layout.
    Heading { level: u8, spans: Vec<Span> },
    /// `<p>`
    Paragraph { spans: Vec<Span> },
    /// `<blockquote>` — rendered indented with a rule.
    Quote { spans: Vec<Span> },
    /// One `<li>` inside `<ul>`/`<ol>`. `ordered` picks bullet vs number;
    /// `index` is the 1-based ordinal for ordered lists.
    ListItem { spans: Vec<Span>, ordered: bool, index: usize },
    /// `<hr/>` — a section break.
    Separator,
}

impl Block {
    pub fn spans(&self) -> &[Span] {
        match self {
            Block::Heading { spans, .. }
            | Block::Paragraph { spans }
            | Block::Quote { spans }
            | Block::ListItem { spans, .. } => spans,
            Block::Separator => &[],
        }
    }

    pub fn plain_text(&self) -> String {
        // Spans are trimmed; a word break at a style boundary is carried by the
        // next span's `space_before` flag, so re-insert those spaces here.
        let mut out = String::new();
        for (i, s) in self.spans().iter().enumerate() {
            if i > 0 && s.space_before {
                out.push(' ');
            }
            out.push_str(&s.text);
        }
        out
    }

    pub fn is_separator(&self) -> bool {
        matches!(self, Block::Separator)
    }
}

/// An inline run with uniform styling. The smallest unit the renderer paints.
///
/// Span text is *trimmed* at style boundaries: the whitespace that separated
/// this run from the previous one is not stored in `text` but recorded in
/// `space_before`, so `"burning <em>for</em>."` yields a `for` run with a
/// leading space and a `.` run with none. The layout and `plain_text` both
/// consult `space_before` to decide whether to insert a gap.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Whether a word break separated this run from the previous one. Ignored
    /// for the first run of a block (it starts the line).
    pub space_before: bool,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), bold: false, italic: false, code: false, space_before: false }
    }
}

// ===========================================================================
// Shared annotation model  (reused by PDF reader + word processor)
// ===========================================================================

/// What an annotation *is*. All three share one anchor so a unified
/// sidebar/list can present them together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnoKind {
    /// A saved place. Anchors to a location; carries no range.
    Bookmark,
    /// A coloured marking over a passage. Anchors to a range; may carry a note.
    Highlight,
    /// A free-text comment attached to a passage.
    Note,
}

impl AnnoKind {
    pub fn label(self) -> &'static str {
        match self {
            AnnoKind::Bookmark => "Bookmark",
            AnnoKind::Highlight => "Highlight",
            AnnoKind::Note => "Note",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            AnnoKind::Bookmark => "bookmark",
            AnnoKind::Highlight => "highlight",
            AnnoKind::Note => "note",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "bookmark" => AnnoKind::Bookmark,
            "note" => AnnoKind::Note,
            _ => AnnoKind::Highlight,
        }
    }
}

/// Highlight palette. Kept as a small enum (rather than raw RGB) so the colour
/// stays semantic and the OS theme can map it to on-brand swatches. The reader
/// resolves each to a concrete `Color` at draw time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiColor {
    Yellow,
    Green,
    Blue,
    Pink,
    Purple,
}

impl HiColor {
    pub const ALL: [HiColor; 5] =
        [HiColor::Yellow, HiColor::Green, HiColor::Blue, HiColor::Pink, HiColor::Purple];

    pub fn as_str(self) -> &'static str {
        match self {
            HiColor::Yellow => "yellow",
            HiColor::Green => "green",
            HiColor::Blue => "blue",
            HiColor::Pink => "pink",
            HiColor::Purple => "purple",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "green" => HiColor::Green,
            "blue" => HiColor::Blue,
            "pink" => HiColor::Pink,
            "purple" => HiColor::Purple,
            _ => HiColor::Yellow,
        }
    }
    /// Base RGB for the swatch. The reader applies alpha for the text overlay.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            HiColor::Yellow => (250, 204, 21),  // amber-400
            HiColor::Green => (52, 211, 153),   // emerald-400
            HiColor::Blue => (96, 165, 250),    // blue-400
            HiColor::Pink => (244, 114, 182),   // pink-400
            HiColor::Purple => (167, 139, 250), // violet-400
        }
    }
}

/// One annotation. **The anchor is the reusable contract:**
///
/// - `doc_id` — which document (book id here; file path / doc uuid elsewhere).
/// - `chapter` — coarse location index (EPUB spine index; PDF page; WP section).
/// - `block` — the structural unit within that location (paragraph index).
/// - `start` / `end` — char offsets within the block's plain text. For a
///   [`AnnoKind::Bookmark`] both are 0 (it anchors to the block, not a range).
/// - `excerpt` — a snapshot of the anchored text. Lets the sidebar render
///   without re-resolving, and lets a reflow re-locate the anchor heuristically
///   if block indices ever drift.
///
/// The payload is `kind` + `color` + `note`. A highlight may also carry a
/// `note` (a commented highlight); a bare note uses a neutral colour.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Annotation {
    pub id: Uuid,
    pub doc_id: String,
    pub kind: AnnoKind,
    pub chapter: usize,
    pub block: usize,
    pub start: usize,
    pub end: usize,
    pub excerpt: String,
    pub note: String,
    pub color: HiColor,
    /// Creation time, ms since epoch. Used to sort the sidebar.
    pub created: u64,
}

impl Annotation {
    pub fn new(doc_id: &str, kind: AnnoKind, chapter: usize, block: usize) -> Self {
        Self {
            id: Uuid::new_v4(),
            doc_id: doc_id.to_string(),
            kind,
            chapter,
            block,
            start: 0,
            end: 0,
            excerpt: String::new(),
            note: String::new(),
            color: HiColor::Yellow,
            created: now_ms(),
        }
    }

    pub fn with_excerpt(mut self, s: impl Into<String>) -> Self {
        self.excerpt = s.into();
        self
    }
    pub fn with_color(mut self, c: HiColor) -> Self {
        self.color = c;
        self
    }
    pub fn with_note(mut self, n: impl Into<String>) -> Self {
        self.note = n.into();
        self
    }
}

/// Milliseconds since the Unix epoch; monotone enough for sort order.
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ===========================================================================
// Reading settings + per-book progress
// ===========================================================================

/// One of the three classic e-reader page tints. These tint the *content
/// surface* (the page), layered on top of the OS theme that drives all chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadTheme {
    Light,
    Sepia,
    Dark,
}

impl ReadTheme {
    pub const ALL: [ReadTheme; 3] = [ReadTheme::Light, ReadTheme::Sepia, ReadTheme::Dark];
    pub fn label(self) -> &'static str {
        match self {
            ReadTheme::Light => "Light",
            ReadTheme::Sepia => "Sepia",
            ReadTheme::Dark => "Dark",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ReadTheme::Light => "light",
            ReadTheme::Sepia => "sepia",
            ReadTheme::Dark => "dark",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "sepia" => ReadTheme::Sepia,
            "dark" => ReadTheme::Dark,
            _ => ReadTheme::Light,
        }
    }
}

/// Reading face. The text backend exposes the system proportional family and a
/// monospace family, so we offer those two honestly: a proportional "Sans" for
/// normal reading and a fixed-width "Mono" some readers prefer for alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadFont {
    Sans,
    Mono,
}

impl ReadFont {
    pub const ALL: [ReadFont; 2] = [ReadFont::Sans, ReadFont::Mono];
    pub fn label(self) -> &'static str {
        match self {
            ReadFont::Sans => "Sans",
            ReadFont::Mono => "Mono",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ReadFont::Sans => "sans",
            ReadFont::Mono => "mono",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "mono" => ReadFont::Mono,
            _ => ReadFont::Sans,
        }
    }
    /// Whether body text should render in the monospace family.
    pub fn is_mono(self) -> bool {
        matches!(self, ReadFont::Mono)
    }
}

/// Global reading preferences, shared across the library and persisted in the
/// CRDT so collaborators see a consistent layout when reading together.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Body font size multiplier; 1.0 == 19px base. Clamped to [0.8, 1.8].
    pub font_scale: f32,
    /// Line-height multiplier on top of the font's own 1.25. [0.95, 1.6].
    pub line_spacing: f32,
    pub font: ReadFont,
    pub theme: ReadTheme,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            line_spacing: 1.2,
            font: ReadFont::Sans,
            theme: ReadTheme::Sepia,
        }
    }
}

impl Settings {
    pub const FONT_MIN: f32 = 0.8;
    pub const FONT_MAX: f32 = 1.8;
    pub const SPACING_MIN: f32 = 0.95;
    pub const SPACING_MAX: f32 = 1.6;

    /// Base body size in logical px before the scale multiplier.
    pub const BASE_SIZE: f32 = 19.0;

    pub fn body_size(&self) -> f32 {
        Self::BASE_SIZE * self.font_scale
    }
}

/// Where the reader left off in one book.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub chapter: usize,
    pub page: usize,
    /// 0.0..=1.0 fraction through the whole book, for the shelf bar.
    pub fraction: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hicolor_round_trips() {
        for c in HiColor::ALL {
            assert_eq!(HiColor::from_str(c.as_str()), c);
        }
    }

    #[test]
    fn annokind_round_trips() {
        for k in [AnnoKind::Bookmark, AnnoKind::Highlight, AnnoKind::Note] {
            assert_eq!(AnnoKind::from_str(k.as_str()), k);
        }
    }

    #[test]
    fn read_theme_and_font_round_trip() {
        for t in ReadTheme::ALL {
            assert_eq!(ReadTheme::from_str(t.as_str()), t);
        }
        for f in ReadFont::ALL {
            assert_eq!(ReadFont::from_str(f.as_str()), f);
        }
    }

    #[test]
    fn block_plain_text_concats_spans() {
        let b = Block::Paragraph {
            spans: vec![
                Span::plain("Hello "),
                Span { text: "world".into(), bold: true, italic: false, code: false, space_before: false },
            ],
        };
        assert_eq!(b.plain_text(), "Hello world");
    }

    #[test]
    fn settings_body_size_scales() {
        let mut s = Settings::default();
        s.font_scale = 1.5;
        assert!((s.body_size() - Settings::BASE_SIZE * 1.5).abs() < 0.01);
    }
}
