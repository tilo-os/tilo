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

// This module's several `from_str(s: &str) -> Self` methods are deliberately
// infallible tag parsers with a default fallback (unknown/legacy tags map to
// a sensible variant rather than erroring) -- a different contract from
// `std::str::FromStr::from_str`, which returns `Result`. Implementing the
// trait would force a `Result` return and ripple `.parse()`/`?` handling
// through every call site for no behavioural benefit, so the name collision
// is accepted here rather than exempted call-by-call.
#![allow(clippy::should_implement_trait)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_ms;
use tilo_document::RichText;
use ui::tilo_resource::{self, Bytes, JsonSchema, Model, Refusal};
use uuid::Uuid;
use vello::peniko::Color;

// --- Book content ------------------------------------------------------------

/// Decoded, downscaled cover artwork extracted from an EPUB. Held behind an
/// `Arc` so a [`Book`] stays cheap to clone and the pixels are decoded
/// exactly once (at load), never per frame.
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
    /// BCP-47 language from EPUB metadata, used for TeX/Liang hyphenation.
    pub language: String,
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
        self.blocks
            .get(i)
            .map(Block::plain_text)
            .unwrap_or_default()
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
    /// A stanza or verse paragraph: a `<p>` (or other line container) inside
    /// z3998 `poem`/`verse`/`song`/`lyrics` semantics whose lines are marked
    /// up. Each run with [`Span::line_break`] starts a new line; lines are set
    /// ragged with a hanging indent. `quoted` verse sits in a `<blockquote>`.
    Verse { spans: Vec<Span>, quoted: bool },
    /// One `<li>` inside `<ul>`/`<ol>`. `ordered` picks bullet vs number;
    /// `index` is the 1-based ordinal for ordered lists.
    ListItem {
        spans: Vec<Span>,
        ordered: bool,
        index: usize,
    },
    /// Presentation `MathML` lowered into the toolkit's shared expression tree.
    Math {
        expr: ui::math::MathExpr,
        /// Linear fallback used for search, accessibility, and annotations.
        source: String,
        display: bool,
    },
    /// `<hr/>` — a section break.
    Separator,
}

impl Block {
    pub fn spans(&self) -> &[Span] {
        match self {
            Block::Heading { spans, .. }
            | Block::Paragraph { spans }
            | Block::Quote { spans }
            | Block::Verse { spans, .. }
            | Block::ListItem { spans, .. } => spans,
            Block::Math { .. } | Block::Separator => &[],
        }
    }

    pub fn plain_text(&self) -> String {
        // Spans are trimmed; a word break at a style boundary is carried by the
        // next span's `space_before` flag, so re-insert those spaces here.
        if let Block::Math { source, .. } = self {
            return source.clone();
        }
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
///
/// A hard line break (`<br/>`, a verse line) is laid out from `line_break`
/// but kept in the text as the single word space it always was, so a
/// block's plain text and every character offset into it are the same
/// whether or not its lines break.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "bold/italic/code/space_before/line_break are independent inline flags a span \
              can carry in any combination, not a state machine"
)]
pub struct Span {
    pub text: String,
    /// Structured inline `MathML`. `text` remains its accessible linear form.
    pub math: Option<ui::math::MathExpr>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Whether a word break separated this run from the previous one. Ignored
    /// for the first run of a block (it starts the line).
    pub space_before: bool,
    /// This run starts a new line. Always carries `space_before`.
    pub line_break: bool,
    /// Indentation in ems of the line this run starts (a verse line's `i1`,
    /// `i2`, … class); read on a block's first run and on `line_break` runs.
    pub line_indent: u8,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            math: None,
            bold: false,
            italic: false,
            code: false,
            space_before: false,
            line_break: false,
            line_indent: 0,
        }
    }
}

// --- Shared annotation model (reused by PDF reader + word processor) --------

/// What an annotation *is*. All three share one anchor so a unified
/// sidebar/list can present them together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum AnnoKind {
    /// A saved place. Anchors to a location; carries no range.
    Bookmark,
    /// A coloured marking over a passage. Anchors to a range; may carry a note.
    #[default]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HiColor {
    #[default]
    Yellow,
    Green,
    Blue,
    Pink,
    Purple,
}

impl HiColor {
    pub const ALL: [HiColor; 5] = [
        HiColor::Yellow,
        HiColor::Green,
        HiColor::Blue,
        HiColor::Pink,
        HiColor::Purple,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HiColor::Yellow => "yellow",
            HiColor::Green => "green",
            HiColor::Blue => "blue",
            HiColor::Pink => "pink",
            HiColor::Purple => "purple",
        }
    }
    /// The colour's name: its swatch's tooltip and accessible name.
    pub fn name(self) -> &'static str {
        match self {
            HiColor::Yellow => "Yellow",
            HiColor::Green => "Green",
            HiColor::Blue => "Blue",
            HiColor::Pink => "Pink",
            HiColor::Purple => "Purple",
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

/// The id an annotation is stored under in the library.
pub fn annotation_key(id: Uuid) -> tilo_resource::Id {
    tilo_resource::Id::from_raw(id.to_string())
}

/// Stable source coordinates for an annotation, independent of its identity,
/// kind, payload, and highlight-group membership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationAnchor {
    pub chapter: usize,
    pub block: usize,
    pub start: usize,
    pub end: usize,
    pub excerpt: String,
}

/// One annotation as the library stores it. The reusable anchor contract:
/// `doc_id` (which document), `chapter` (coarse location: spine index /
/// page / section), `block` (paragraph index), `start`/`end` (char offsets,
/// both 0 for a [`AnnoKind::Bookmark`]), and `excerpt` (a text snapshot so
/// the sidebar renders without re-resolving and a reflow can re-locate the
/// anchor). Payload is `kind` + `color` + `text`. What the reader draws and
/// searches is its [`Annotation`].
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = Mark::valid)]
pub struct Mark {
    /// The annotation's id: the key it is stored under.
    #[model(readonly)]
    pub id: Uuid,
    /// The book it is in
    #[model(readonly)]
    pub doc_id: String,
    /// Bookmark, highlight or note
    #[model(readonly)]
    pub kind: AnnoKind,
    /// Chapter
    pub chapter: usize,
    /// Paragraph
    pub block: usize,
    /// Where in the paragraph it starts
    pub start: usize,
    /// Where in the paragraph it ends
    pub end: usize,
    /// The passage
    #[model(label)]
    pub excerpt: String,
    /// The note or comment as written.
    #[model(whole)]
    pub text: RichText,
    /// Colour
    pub color: HiColor,
    /// When it was made, ms since epoch. Used to sort the sidebar.
    pub created: u64,
    /// The logical annotation this row belongs to. A multi-paragraph
    /// highlight is one [`AnnoKind::Highlight`] row per spanned block,
    /// sharing one `group` (one click, one delete); comments on it are
    /// [`AnnoKind::Note`] rows carrying the same `group`. A standalone
    /// bookmark or note is its own group (`group == id`).
    #[model(readonly)]
    pub group: Uuid,
}

/// The longest passage an annotation keeps as its excerpt, in bytes.
pub const MAX_EXCERPT_BYTES: usize = 16 * 1024;
/// The longest note or comment, in bytes.
pub const MAX_NOTE_BYTES: usize = 64 * 1024;

impl Mark {
    /// The id the annotation is stored under in the library.
    pub fn key(&self) -> tilo_resource::Id {
        annotation_key(self.id)
    }

    /// A range that ends before it starts, or a passage or note too long
    /// to keep, is refused however it arrives.
    fn valid(&self) -> Result<(), Refusal> {
        if self.start > self.end {
            return Err(Refusal::new("an annotation ends where it starts or later"));
        }
        if self.excerpt.len() > MAX_EXCERPT_BYTES {
            return Err(Refusal::new("the annotated passage is too long"));
        }
        if self.text.text().len() > MAX_NOTE_BYTES {
            return Err(Refusal::new("the note is too long"));
        }
        Ok(())
    }

    /// What the reader draws and searches of it.
    pub fn annotation(&self) -> Annotation {
        let blocks = self.text.state().map_or_else(Vec::new, |state| {
            state
                .body()
                .all()
                .filter_map(|id| state.block(id))
                .map(|block| tilo_rich_text::content_of(block))
                .collect()
        });
        Annotation {
            id: self.id,
            doc_id: self.doc_id.clone(),
            kind: self.kind,
            chapter: self.chapter,
            block: self.block,
            start: self.start,
            end: self.end,
            excerpt: self.excerpt.clone(),
            note: self.text.text(),
            blocks,
            color: self.color,
            created: self.created,
            group: self.group,
        }
    }

    /// Stored as `annotation` says, its note written as plain text.
    pub fn of(annotation: &Annotation) -> Self {
        Self {
            id: annotation.id,
            doc_id: annotation.doc_id.clone(),
            kind: annotation.kind,
            chapter: annotation.chapter,
            block: annotation.block,
            start: annotation.start,
            end: annotation.end,
            excerpt: annotation.excerpt.clone(),
            text: RichText::from_markdown(annotation.note.clone()),
            color: annotation.color,
            created: annotation.created,
            group: annotation.group,
        }
    }
}

/// One annotation as the reader draws and searches it: a [`Mark`] with its
/// note read as text and as the editor's blocks.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: Uuid,
    pub doc_id: String,
    pub kind: AnnoKind,
    pub chapter: usize,
    pub block: usize,
    pub start: usize,
    pub end: usize,
    pub excerpt: String,
    /// The note or comment's plain text: what search, labels and the
    /// palette read.
    pub note: String,
    /// The note or comment as written, as the editor shows it.
    pub blocks: Vec<ui::widget::BlockContent>,
    pub color: HiColor,
    /// Creation time, ms since epoch. Used to sort the sidebar.
    pub created: u64,
    /// See [`Mark::group`].
    pub group: Uuid,
}

impl Annotation {
    pub fn new(doc_id: &str, kind: AnnoKind, chapter: usize, block: usize) -> Self {
        let id = Uuid::new_v4();
        Self {
            id,
            doc_id: doc_id.to_string(),
            kind,
            chapter,
            block,
            start: 0,
            end: 0,
            excerpt: String::new(),
            note: String::new(),
            blocks: Vec::new(),
            color: HiColor::Yellow,
            created: unix_now_ms(),
            group: id,
        }
    }

    /// The id the annotation is stored under in the library.
    pub fn key(&self) -> tilo_resource::Id {
        annotation_key(self.id)
    }

    /// Whether this row is a comment on a highlight.
    pub fn is_comment(&self) -> bool {
        self.kind == AnnoKind::Note && self.group != self.id
    }

    pub fn anchor(&self) -> AnnotationAnchor {
        AnnotationAnchor {
            chapter: self.chapter,
            block: self.block,
            start: self.start,
            end: self.end,
            excerpt: self.excerpt.clone(),
        }
    }

    #[must_use]
    pub fn with_excerpt(mut self, s: impl Into<String>) -> Self {
        self.excerpt = s.into();
        self
    }
    #[must_use]
    pub fn with_color(mut self, c: HiColor) -> Self {
        self.color = c;
        self
    }
    #[must_use]
    pub fn with_note(mut self, n: impl Into<String>) -> Self {
        self.note = n.into();
        self
    }
    /// Bind this row to a logical annotation group (a multi-block highlight and
    /// its comments all share one group).
    #[must_use]
    pub fn with_group(mut self, g: Uuid) -> Self {
        self.group = g;
        self
    }
}

// --- Reading settings + per-book progress ------------------------------------

/// One of the three classic e-reader page tints. These tint the *content
/// surface* (the page), layered on top of the OS theme that drives all chrome.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum ReadTheme {
    #[default]
    Light,
    Sepia,
    Dark,
    /// Maximum-contrast reading: a true-black page with white ink and a vivid
    /// accent, for low-vision reading. A first-class tint alongside the classics.
    HighContrast,
}

impl ReadTheme {
    pub const ALL: [ReadTheme; 4] = [
        ReadTheme::Light,
        ReadTheme::Sepia,
        ReadTheme::Dark,
        ReadTheme::HighContrast,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ReadTheme::Light => "Light",
            ReadTheme::Sepia => "Sepia",
            ReadTheme::Dark => "Dark",
            ReadTheme::HighContrast => "High contrast",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ReadTheme::Light => "light",
            ReadTheme::Sepia => "sepia",
            ReadTheme::Dark => "dark",
            ReadTheme::HighContrast => "high-contrast",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "sepia" => ReadTheme::Sepia,
            "dark" => ReadTheme::Dark,
            "high-contrast" | "high_contrast" | "hc" => ReadTheme::HighContrast,
            _ => ReadTheme::Light,
        }
    }
}

/// How the reader lays the book out: the three modes share one paginated
/// layout ([`crate::paginate::ChapterLayout`]), differing only in which
/// slice of the flow is shown. `Paged` turns whole pages; `Continuous`
/// scrolls by pixels and rolls into the next/previous chapter at the ends;
/// `TwoUp` shows two facing pages and turns them together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    #[default]
    Paged,
    Continuous,
    TwoUp,
}

/// Paragraph-composition policy used by the reader. `Book` enables the full
/// typesetting pipeline; `Ragged` keeps natural spacing and disables automatic
/// hyphenation/micro-expansion for readers who prefer an uneven right edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum CompositionStyle {
    #[default]
    Book,
    Ragged,
}

impl CompositionStyle {
    pub const ALL: [Self; 2] = [Self::Book, Self::Ragged];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Book => "Justified",
            Self::Ragged => "Ragged",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Book => "book",
            Self::Ragged => "ragged",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "ragged" => Self::Ragged,
            _ => Self::Book,
        }
    }

    pub const fn justified(self) -> bool {
        matches!(self, Self::Book)
    }
}

impl ReadMode {
    pub const ALL: [ReadMode; 3] = [ReadMode::Paged, ReadMode::Continuous, ReadMode::TwoUp];

    pub fn label(self) -> &'static str {
        match self {
            ReadMode::Paged => "Pages",
            ReadMode::Continuous => "Scroll",
            ReadMode::TwoUp => "Two pages",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ReadMode::Paged => "paged",
            ReadMode::Continuous => "continuous",
            ReadMode::TwoUp => "two_up",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "continuous" | "scroll" => ReadMode::Continuous,
            "two_up" | "two-up" | "spread" => ReadMode::TwoUp,
            _ => ReadMode::Paged,
        }
    }
    /// Whether this mode scrolls the flow continuously (no page snapping).
    pub fn is_continuous(self) -> bool {
        matches!(self, ReadMode::Continuous)
    }
    /// How many pages advance per page-turn in this mode.
    pub fn page_stride(self) -> usize {
        match self {
            ReadMode::TwoUp => 2,
            _ => 1,
        }
    }
}

/// Which font *class* a registered face resolves to. `Default` is the reader's
/// proportional face (also used for EPUB-specified text); `Serif` is the
/// system's reading face and `Mono` the monospace class; `Reading` is one of
/// the other reading faces the system ships, by its name
/// ([`ui::ReadingFace::name`]); `Named` targets a specific installed family
/// the reader picked from the system font list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontKind {
    #[default]
    Default,
    Serif,
    Mono,
    Reading(String),
    Named(String),
}

tilo_resource::json_model!(FontKind);

impl FontKind {
    /// Encode as a single scalar string.
    pub fn as_code(&self) -> String {
        match self {
            FontKind::Default => "default".to_string(),
            FontKind::Serif => "serif".to_string(),
            FontKind::Mono => "mono".to_string(),
            FontKind::Reading(name) => format!("reading:{name}"),
            FontKind::Named(n) => format!("named:{n}"),
        }
    }
    pub fn from_code(s: &str) -> Self {
        if let Some(n) = s.strip_prefix("named:") {
            return FontKind::Named(n.to_string());
        }
        if let Some(name) = s.strip_prefix("reading:") {
            return FontKind::Reading(name.to_string());
        }
        match s {
            "serif" => FontKind::Serif,
            "mono" => FontKind::Mono,
            _ => FontKind::Default,
        }
    }
    /// Whether body text should render in the monospace family.
    pub fn is_mono(&self) -> bool {
        matches!(self, FontKind::Mono)
    }
}

/// A registered reading font. Fonts carry a display name and a per-face size
/// multiplier (installed faces differ in x-height, so the same nominal size
/// reads larger or smaller). `shown` controls whether it appears in the quick
/// display-settings font row. Unlike colour schemes, fonts are **not**
/// duplicable — a face is a face; there is no per-instance colour to vary.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Model)]
#[model(check = FontEntry::valid)]
pub struct FontEntry {
    /// The font's id: the key it is stored under.
    #[model(readonly)]
    pub id: String,
    /// The face it resolves to
    pub kind: FontKind,
    /// Name
    #[model(label)]
    pub name: String,
    /// Multiplier on top of the global text-size setting, within
    /// [`Self::MULT_MIN`]..=[`Self::MULT_MAX`].
    pub size_mult: f32,
    /// Whether this font appears in the quick display-settings picker.
    pub shown: bool,
}

impl FontEntry {
    pub const MULT_MIN: f32 = 0.7;
    pub const MULT_MAX: f32 = 1.4;

    fn valid(&self) -> Result<(), Refusal> {
        if !self.size_mult.is_finite()
            || !(Self::MULT_MIN..=Self::MULT_MAX).contains(&self.size_mult)
        {
            return Err(Refusal::new(
                "a font's size is between 0.7 and 1.4 times the text size",
            ));
        }
        if self.name.trim().is_empty() {
            return Err(Refusal::new("a font has a name"));
        }
        Ok(())
    }

    pub fn new(id: &str, kind: FontKind, name: &str, size_mult: f32, shown: bool) -> Self {
        Self {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            size_mult: size_mult.clamp(Self::MULT_MIN, Self::MULT_MAX),
            shown,
        }
    }
}

/// The built-in font registry: the reader's default proportional face, the
/// system's reading face (New Computer Modern), the other reading faces the
/// system ships, and a monospace face. A fresh library starts here; the font
/// manager can add installed families, rename, retune size, or hide any of
/// them.
pub fn default_fonts() -> Vec<FontEntry> {
    let mut fonts = vec![
        FontEntry::new("default", FontKind::Default, "Default", 1.0, true),
        FontEntry::new(
            "serif",
            FontKind::Serif,
            ui::ReadingFace::Default.name(),
            1.05,
            true,
        ),
    ];
    fonts.extend(
        ui::ReadingFace::ALL
            .into_iter()
            .filter(|face| *face != ui::ReadingFace::Default)
            .map(|face| {
                let name = face.name();
                FontEntry::new(
                    &format!("reading-{}", name.to_lowercase().replace(' ', "-")),
                    FontKind::Reading(name.to_owned()),
                    name,
                    reading_size_mult(face),
                    true,
                )
            }),
    );
    fonts.push(FontEntry::new("mono", FontKind::Mono, "Mono", 0.96, true));
    fonts
}

/// The size multiplier that sets a reading face at New Computer Modern's
/// apparent size: its x-height over theirs, so switching faces does not
/// change how large the page reads.
fn reading_size_mult(face: ui::ReadingFace) -> f32 {
    match face {
        // x-heights in em: New Computer Modern Book 0.431, Libertinus 0.429.
        ui::ReadingFace::Default | ui::ReadingFace::Libertinus => 1.05,
        // 0.507
        ui::ReadingFace::Literata => 0.9,
        // 0.400
        ui::ReadingFace::Garamond => 1.12,
        // 0.475
        ui::ReadingFace::SourceSerif => 0.96,
    }
}

// ===========================================================================
// Colour schemes
// ===========================================================================

/// Relative luminance (0..1) of an sRGB colour, for light/dark decisions.
pub fn luminance(c: [u8; 3]) -> f32 {
    (0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2])) / 255.0
}

/// Linear blend of two colours; `t` in 0..1 moves from `a` to `b`.
pub fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (f32::from(a[0]) * (1.0 - t) + f32::from(b[0]) * t).round() as u8,
        (f32::from(a[1]) * (1.0 - t) + f32::from(b[1]) * t).round() as u8,
        (f32::from(a[2]) * (1.0 - t) + f32::from(b[2]) * t).round() as u8,
    ]
}

/// The derived palette parts returned by [`ColorScheme::palette_parts`]:
/// `(page, page_edge, ink, ink_dim, rule, code_bg)`.
pub type PaletteParts = ([u8; 3], [u8; 3], [u8; 3], [u8; 3], [u8; 3], [u8; 3]);

/// A reader colour scheme: a page (paper) colour, body ink, and an accent, plus
/// a dark flag. The rest of the palette (faint edges, dim ink, rules, code
/// chips) is **derived**, so a scheme stays coherent from just these three
/// colours — and so a whole scheme can be spun up from one background colour.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Model)]
#[model(check = ColorScheme::valid)]
pub struct ColorScheme {
    /// The scheme's id: the key it is stored under.
    #[model(readonly)]
    pub id: String,
    /// Name
    #[model(label)]
    pub name: String,
    /// Page colour
    pub page: [u8; 3],
    /// Text colour
    pub ink: [u8; 3],
    /// Accent colour
    pub accent: [u8; 3],
    /// Whether the page is dark
    pub dark: bool,
    /// Whether it is offered
    pub enabled: bool,
    /// Built-in presets can be disabled/duplicated but never deleted or edited.
    #[model(readonly)]
    pub builtin: bool,
}

impl ColorScheme {
    fn valid(&self) -> Result<(), Refusal> {
        if self.name.trim().is_empty() {
            Err(Refusal::new("a colour scheme has a name"))
        } else {
            Ok(())
        }
    }

    /// The three editable colours, labelled — drives the per-colour editor.
    pub fn slots(&self) -> [(&'static str, [u8; 3]); 3] {
        [
            ("Page", self.page),
            ("Text", self.ink),
            ("Accent", self.accent),
        ]
    }

    pub fn set_slot(&mut self, i: usize, c: [u8; 3]) {
        match i {
            0 => self.page = c,
            1 => self.ink = c,
            _ => self.accent = c,
        }
        // Keep the dark flag consistent with the paper so highlight alpha and
        // ink defaults stay sensible after an edit.
        self.dark = luminance(self.page) < 0.5;
    }

    /// Derive a full scheme from one background colour: pick a legible ink
    /// (warm off-white on a dark paper, near-black on a light one) and a vivid,
    /// hue-rotated accent that keeps contrast against the paper.
    pub fn from_bg(id: &str, name: &str, bg: [u8; 3]) -> Self {
        let dark = luminance(bg) < 0.5;
        let ink = if dark {
            [0xEA, 0xE4, 0xD8]
        } else {
            [0x22, 0x20, 0x1E]
        };
        let (h, _s, _l) = ui::color::rgb_to_hsl(Color::from_rgb8(bg[0], bg[1], bg[2]));
        let acc_l = if dark { 0.64 } else { 0.48 };
        let accent = ui::color::hsl_to_rgb(h + 150.0, 0.62, acc_l);
        let accent = ui::color::to_rgb8(accent);
        Self {
            id: id.to_string(),
            name: name.to_string(),
            page: bg,
            ink,
            accent,
            dark,
            enabled: true,
            builtin: false,
        }
    }

    /// The derived palette parts `(page, page_edge, ink, ink_dim, rule, code_bg)`.
    pub fn palette_parts(&self) -> PaletteParts {
        let page = self.page;
        let ink = self.ink;
        (
            page,
            mix(page, ink, 0.14), // page_edge
            ink,
            mix(ink, page, 0.42), // ink_dim
            mix(page, ink, 0.22), // rule
            mix(page, ink, 0.10), // code_bg
        )
    }
}

/// The built-in colour schemes (mirrors the three classic tints). They ship
/// enabled; the scheme manager can disable, duplicate, or clone them.
pub fn default_schemes() -> Vec<ColorScheme> {
    vec![
        ColorScheme {
            id: "light".into(),
            name: "Light".into(),
            page: [0xFC, 0xFC, 0xFA],
            ink: [0x1B, 0x1B, 0x1F],
            accent: [0x3B, 0x82, 0xF6],
            dark: false,
            enabled: true,
            builtin: true,
        },
        ColorScheme {
            id: "sepia".into(),
            name: "Sepia".into(),
            page: [0xF6, 0xEC, 0xD8],
            ink: [0x40, 0x33, 0x22],
            accent: [0xB4, 0x6A, 0x2E],
            dark: false,
            enabled: true,
            builtin: true,
        },
        ColorScheme {
            // A warm-*neutral* charcoal — deliberately close to grey so it never
            // reads as sepia/brown, but with a hair of warmth (R ≥ B) so it stays
            // comfortable for long reading rather than a clinical blue-black.
            id: "dark".into(),
            name: "Dark".into(),
            page: [0x1E, 0x1D, 0x1C],
            ink: [0xE7, 0xE5, 0xE1],
            accent: [0x8A, 0xB4, 0xF8],
            dark: true,
            enabled: true,
            builtin: true,
        },
        ColorScheme {
            // Maximum contrast: a true-black page, pure-white ink, and a vivid
            // amber accent that stays unmistakable against the black. For
            // low-vision reading; its reading palette is fixed in `reader_theme`.
            id: "high-contrast".into(),
            name: "High contrast".into(),
            page: [0x00, 0x00, 0x00],
            ink: [0xFF, 0xFF, 0xFF],
            accent: [0xFF, 0xD5, 0x2E],
            dark: true,
            enabled: true,
            builtin: true,
        },
    ]
}

/// Global reading preferences, shared across the library and persisted in the
/// CRDT so collaborators see a consistent layout when reading together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
#[model(check = Settings::valid)]
pub struct Settings {
    /// Text size: a multiplier of the 19px base, within 0.8 to 2.0.
    pub font_scale: f32,
    /// Line spacing: a multiplier on top of the font's own 1.25, within
    /// 0.95 to 1.6.
    pub line_spacing: f32,
    /// Id of the selected [`FontEntry`] in the registry. Empty means "use the
    /// registry default".
    pub font_id: String,
    /// Built-in page tint. Retained for the three classic presets; a custom
    /// colour scheme (when selected) is carried by `scheme_id`.
    pub theme: ReadTheme,
    /// Id of a selected custom colour scheme, or empty to use `theme`.
    pub scheme_id: String,
    /// How the page is laid out (single page / continuous scroll / two-up
    /// spread). A reading preference, shared across the library like the tint.
    pub mode: ReadMode,
    /// Paragraph fitting policy. Defaults to book-quality composition.
    pub composition: CompositionStyle,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            line_spacing: 1.2,
            // Empty means "the reader's default reading face" — which resolves to
            // the serif book font (see `BooksApp::active_font`), because books
            // read best in serif. The picker offers the sans "Default" + mono.
            font_id: String::new(),
            theme: ReadTheme::Sepia,
            scheme_id: String::new(),
            mode: ReadMode::Paged,
            composition: CompositionStyle::Book,
        }
    }
}

impl Settings {
    pub const READING_SCALE_MIN: f32 = 0.8;
    pub const READING_SCALE_MAX: f32 = 2.0;
    pub const SPACING_MIN: f32 = 0.95;
    pub const SPACING_MAX: f32 = 1.6;

    fn valid(&self) -> Result<(), Refusal> {
        let within =
            |value: f32, min: f32, max: f32| value.is_finite() && (min..=max).contains(&value);
        if !within(
            self.font_scale,
            Self::READING_SCALE_MIN,
            Self::READING_SCALE_MAX,
        ) {
            return Err(Refusal::new(
                "the text size is between 0.8 and 2 times the base",
            ));
        }
        if !within(self.line_spacing, Self::SPACING_MIN, Self::SPACING_MAX) {
            return Err(Refusal::new("the line spacing is between 0.95 and 1.6"));
        }
        Ok(())
    }

    /// Base body size in logical px before the scale multiplier.
    pub const BODY_CONTENT_PX: f32 = 19.0;

    pub fn body_size(&self) -> f32 {
        Self::BODY_CONTENT_PX * self.font_scale
    }

    /// Body size including a selected font's per-face size multiplier.
    pub fn body_size_with(&self, font_mult: f32) -> f32 {
        Self::BODY_CONTENT_PX * self.font_scale * font_mult
    }
}

/// Where the reader left off in one book.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, Model)]
pub struct Progress {
    /// Chapter
    pub chapter: usize,
    /// The first block (paragraph, heading, …) on screen, counted from 0 in
    /// the chapter. A block, not a page number, so the reader lands on the
    /// same passage at any window or text size.
    pub block: usize,
    /// 0.0..=1.0 fraction through the whole book, for the shelf bar.
    pub fraction: f32,
    /// ms-since-epoch this book was last opened, for the "recently opened"
    /// shelf sort. 0 means never opened.
    pub opened: u64,
}

impl Progress {
    /// A book counts as "read" once the reader is essentially at the end — used
    /// by the shelf's "hide read books" filter.
    pub fn is_read(&self) -> bool {
        self.fraction >= 0.985
    }
}

// --- Library shelf preferences -----------------------------------------------

/// How the library shelf orders its books. Persisted so the shelf keeps the
/// reader's chosen ordering across sessions and collaborators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySort {
    #[default]
    Title,
    Author,
    /// Most-recently-opened first.
    Recent,
    /// Furthest-read first.
    Progress,
}

impl LibrarySort {
    pub const ALL: [LibrarySort; 4] = [
        LibrarySort::Title,
        LibrarySort::Author,
        LibrarySort::Recent,
        LibrarySort::Progress,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LibrarySort::Title => "Title",
            LibrarySort::Author => "Author",
            LibrarySort::Recent => "Recently opened",
            LibrarySort::Progress => "Progress",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            LibrarySort::Title => "title",
            LibrarySort::Author => "author",
            LibrarySort::Recent => "recent",
            LibrarySort::Progress => "progress",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "author" => LibrarySort::Author,
            "recent" => LibrarySort::Recent,
            "progress" => LibrarySort::Progress,
            _ => LibrarySort::Title,
        }
    }
}

/// One book the reader has added to their library. Stored under the
/// content-hash [`id`](Book::id): re-adding the same file updates the same
/// entry, and a workspace-shared book keeps one identity everywhere. Its
/// device path is deliberately *not* part of the library: a peer can resolve
/// a shared object from its content id, while local source locations remain
/// private to the device that granted them.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = LibraryEntry::valid)]
pub struct LibraryEntry {
    /// Content-hash id, shared with the book's annotations + progress.
    #[model(readonly)]
    pub id: String,
    /// Whether a content-addressed copy is available in the collaborative
    /// workspace. Its filename is deterministically derived from [`Self::id`],
    /// so no host-specific path needs replication.
    #[model(readonly)]
    pub shared: bool,
    /// Title
    #[model(label)]
    pub title: String,
    /// Author
    pub author: String,
    /// Subject
    pub subject: String,
    /// When the book was first added (ms since epoch), for a "recently added"
    /// reading of the shelf.
    #[model(readonly)]
    pub added: u64,
    /// The EPUB's bytes, by reference to a host content store
    /// (`tilo_content`) where a device without the file finds them; empty
    /// when only the devices holding the file read it.
    #[model(readonly)]
    pub content: Bytes,
}

/// The longest title, author or subject a shelf row keeps, in bytes.
const MAX_LABEL_BYTES: usize = 4 * 1024;

impl LibraryEntry {
    /// The id the book is stored under in the library.
    pub fn key(&self) -> tilo_resource::Id {
        tilo_resource::Id::from_raw(self.id.clone())
    }

    /// The content-store hash of the book's bytes, when the row names them.
    pub fn content_hash(&self) -> Option<String> {
        match &self.content {
            Bytes::Empty => None,
            content => Some(
                blake3::Hash::from_bytes(content.hash())
                    .to_hex()
                    .to_string(),
            ),
        }
    }

    /// Bytes the content store keeps as `hash` (its hex form), `len` long.
    pub fn stored(hash: &str, len: u64) -> Option<Bytes> {
        blake3::Hash::from_hex(hash).ok().map(|hash| Bytes::Blob {
            hash: *hash.as_bytes(),
            len,
        })
    }

    /// A row names its book by `book:` and the start of its content hash,
    /// and the content it names is that book's.
    fn valid(&self) -> Result<(), Refusal> {
        let tail = self
            .id
            .strip_prefix("book:")
            .filter(|tail| !tail.is_empty() && !tail.chars().any(char::is_control))
            .ok_or_else(|| Refusal::new("a book's id is book: and its content hash"))?;
        if [&self.title, &self.author, &self.subject]
            .iter()
            .any(|text| text.len() > MAX_LABEL_BYTES)
        {
            return Err(Refusal::new(
                "a book's title, author or subject is too long",
            ));
        }
        if let Some(hash) = self.content_hash()
            && hash.get(..32).is_none_or(|prefix| prefix != tail)
        {
            return Err(Refusal::new("the content named is not this book's"));
        }
        Ok(())
    }
}

/// Persisted shelf preferences: the sort order and whether finished books are
/// hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct LibraryPrefs {
    /// How the shelf is sorted
    pub sort: LibrarySort,
    /// Whether finished books are hidden
    pub hide_read: bool,
}

impl Default for LibraryPrefs {
    fn default() -> Self {
        Self {
            sort: LibrarySort::Title,
            hide_read: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Colour, annotation, theme, mode, font, sort and composition values round-trip with stable names and defaults.
    #[test]
    fn model_enums_round_trip_with_stable_defaults() {
        // --- phase: hicolor_round_trips ---
        {
            for c in HiColor::ALL {
                assert_eq!(HiColor::from_str(c.as_str()), c);
            }
        }

        // --- phase: annokind_round_trips ---
        {
            for k in [AnnoKind::Bookmark, AnnoKind::Highlight, AnnoKind::Note] {
                assert_eq!(AnnoKind::from_str(k.as_str()), k);
            }
        }

        // --- phase: read_theme_round_trips ---
        {
            for t in ReadTheme::ALL {
                assert_eq!(ReadTheme::from_str(t.as_str()), t);
            }
        }

        // --- phase: read_mode_round_trips_and_defaults_to_paged ---
        {
            for m in ReadMode::ALL {
                assert_eq!(ReadMode::from_str(m.as_str()), m);
            }
            assert_eq!(ReadMode::from_str("nonsense"), ReadMode::Paged);
            assert_eq!(
                Settings::default().mode,
                ReadMode::Paged,
                "readers open paged by default"
            );
            assert_eq!(ReadMode::TwoUp.page_stride(), 2, "a spread turns two pages");
            assert_eq!(ReadMode::Paged.page_stride(), 1);
            assert!(ReadMode::Continuous.is_continuous());
            assert!(!ReadMode::Paged.is_continuous());
        }

        // --- phase: font_kind_round_trips_including_named ---
        {
            for k in [
                FontKind::Default,
                FontKind::Serif,
                FontKind::Mono,
                FontKind::Reading(ui::ReadingFace::Libertinus.name().into()),
                FontKind::Named("Literata".into()),
            ] {
                assert_eq!(FontKind::from_code(&k.as_code()), k);
            }
        }

        // --- phase: default_fonts_are_shown_and_unique ---
        {
            let f = default_fonts();
            assert!(
                f.iter().any(|e| e.kind == FontKind::Serif),
                "a serif face ships by default"
            );
            assert!(
                f.iter().all(|e| e.shown),
                "the built-in fonts all show in quick settings"
            );
            let ids: std::collections::HashSet<_> = f.iter().map(|e| &e.id).collect();
            assert_eq!(ids.len(), f.len(), "font ids are unique");
        }

        // --- phase: composition_style_has_stable_storage_names ---
        {
            assert_eq!(CompositionStyle::Book.as_str(), "book");
            assert_eq!(
                CompositionStyle::from_str("ragged"),
                CompositionStyle::Ragged
            );
            assert_eq!(
                CompositionStyle::from_str("unknown"),
                CompositionStyle::Book,
                "unknown persisted values retain the book-quality default"
            );
        }

        // --- phase: library_sort_round_trips ---
        {
            for s in LibrarySort::ALL {
                assert_eq!(LibrarySort::from_str(s.as_str()), s);
            }
        }
    }

    /// Block plain text concatenates spans, body size scales with settings, and progress reads as finished at the end.
    #[test]
    fn reading_values_derive_from_content_and_settings() {
        // --- phase: block_plain_text_concats_spans ---
        {
            let b = Block::Paragraph {
                spans: vec![
                    Span::plain("Hello "),
                    Span {
                        text: "world".into(),
                        math: None,
                        bold: true,
                        italic: false,
                        code: false,
                        space_before: false,
                        line_break: false,
                        line_indent: 0,
                    },
                ],
            };
            assert_eq!(b.plain_text(), "Hello world");
        }

        // --- phase: settings_body_size_scales ---
        {
            let s = Settings {
                font_scale: 1.5,
                ..Default::default()
            };
            assert!((s.body_size() - Settings::BODY_CONTENT_PX * 1.5).abs() < 0.01);
        }

        // --- phase: progress_is_read_at_the_end ---
        {
            assert!(
                !Progress {
                    fraction: 0.5,
                    ..Default::default()
                }
                .is_read()
            );
            assert!(
                Progress {
                    fraction: 1.0,
                    ..Default::default()
                }
                .is_read()
            );
        }
    }

    /// Schemes derive contrasting ink and accents from their paper, stay neutral in dark, maximal in high contrast, and recompute on slot changes.
    #[test]
    fn scheme_palettes_derive_contrasting_parts() {
        // --- phase: scheme_from_dark_bg_derives_light_ink_and_contrasting_accent ---
        {
            let s = ColorScheme::from_bg("u1", "Midnight", [0x12, 0x14, 0x1C]);
            assert!(s.dark, "a dark background yields a dark scheme");
            assert!(
                luminance(s.ink) > 0.7,
                "ink is a light off-white on a dark paper"
            );
            assert_eq!(s.page, [0x12, 0x14, 0x1C], "paper is the given background");
            // The accent must stand clearly apart from the paper in luminance.
            assert!(
                (luminance(s.accent) - luminance(s.page)).abs() > 0.2,
                "accent contrasts with the paper",
            );
        }

        // --- phase: scheme_from_light_bg_derives_dark_ink ---
        {
            let s = ColorScheme::from_bg("u2", "Paper", [0xF3, 0xEE, 0xE2]);
            assert!(!s.dark, "a light background yields a light scheme");
            assert!(luminance(s.ink) < 0.3, "ink is near-black on a light paper");
        }

        // --- phase: scheme_derived_parts_sit_between_page_and_ink ---
        {
            let s = ColorScheme::from_bg("u3", "Warm", [0x20, 0x1C, 0x18]);
            let (page, edge, ink, dim, _rule, _code) = s.palette_parts();
            assert_eq!(page, s.page);
            assert_eq!(ink, s.ink);
            // Derived tones are genuine blends, never equal to an endpoint.
            assert_ne!(edge, page);
            assert_ne!(dim, ink);
        }

        // --- phase: dark_scheme_is_neutral_not_brown ---
        {
            let dark = default_schemes()
                .into_iter()
                .find(|s| s.id == "dark")
                .unwrap();
            // Warm (red ≥ blue) but only barely — a genuine charcoal, not a sepia.
            assert!(dark.page[0] >= dark.page[2], "kept faintly warm");
            assert!(dark.page[0] - dark.page[2] <= 6, "not a brown/sepia paper");
            assert!(luminance(dark.page) < 0.2, "still a dark page");
        }

        // --- phase: high_contrast_builtin_is_maximal_contrast ---
        {
            let hc = default_schemes()
                .into_iter()
                .find(|s| s.id == "high-contrast")
                .unwrap();
            assert!(
                hc.builtin && hc.dark,
                "high contrast ships as a dark built-in"
            );
            assert!(luminance(hc.page) < 0.02, "a true-black page");
            assert!(luminance(hc.ink) > 0.98, "white ink");
            // It is a first-class built-in tint: the ReadTheme enum round-trips its id.
            assert_eq!(ReadTheme::from_str(&hc.id), ReadTheme::HighContrast);
            assert_eq!(ReadTheme::HighContrast.as_str(), "high-contrast");
        }

        // --- phase: set_slot_recomputes_dark_from_paper ---
        {
            let mut s = default_schemes()[0].clone(); // Light
            assert!(!s.dark);
            s.set_slot(0, [0x10, 0x10, 0x12]); // paint the paper dark
            assert!(
                s.dark,
                "editing the page to a dark colour flips the scheme dark"
            );
        }
    }
}
