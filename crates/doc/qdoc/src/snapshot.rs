//! Read-only materialized view of a `Document` for rendering/export.

use crate::document::MarkSpan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A scalar block/style attribute value (heading level, list ordering, style id, font size, …). `Float` carries fractional values that don't fit an integer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttrVal {
    Int(i64),
    Bool(bool),
    Str(String),
    Float(f64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocSnapshot {
    pub schema: String,
    pub schema_version: i64,
    pub blocks: Vec<BlockView>,
    /// Content-addressed binary store (blake3 hex → bytes). `image` blocks carry
    /// their `blob` attr as a key here so a renderer can fetch the pixels.
    #[serde(default)]
    pub blobs: BTreeMap<String, Vec<u8>>,
    /// CRDT-native named styles: `style_id → (property → value)`. Built-ins (body/heading1…/quote) plus any user- or import-defined styles.
    #[serde(default)]
    pub styles: BTreeMap<String, BTreeMap<String, AttrVal>>,
    /// Document properties / metadata (`title`/`author`/`subject`/`keywords`/`created`/`modified`/`generator` → value). Round-trips through `.odt` `meta.xml` and `.docx` `docProps/*.xml`.
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    /// Page-header block sequence (rendered at the top of every page). A `field` mark valued
    /// `page-number` over a placeholder character marks a page-number field. Round-trips through the `.odt` master-page header and `.docx` `header1.xml`.
    #[serde(default)]
    pub header: Vec<BlockView>,
    /// Page-footer block sequence (rendered at the bottom of every page); mirrors `header`.
    #[serde(default)]
    pub footer: Vec<BlockView>,
    /// Footnotes / endnotes, each a small block sequence keyed by note id, referenced from body text
    /// by a `note-ref` mark over a placeholder character carrying the note id. Round-trips through inline `text:note` in `.odt` and `word/footnotes.xml`/`endnotes.xml` in `.docx`.
    #[serde(default)]
    pub notes: Vec<NoteView>,
    /// Comments / annotations, each anchored to a body text range (recovered from the `comment` mark)
    /// carrying the comment id, with author/date/content held in the `comments` container. Round-trips
    /// through `office:annotation`(+`-end`) in `.odt` and `word/comments.xml` + `commentRangeStart`/`-End`/`commentReference` in `.docx`.
    #[serde(default)]
    pub comments: Vec<CommentView>,
    /// Tracked changes / revision marks, each anchored to a body text range (recovered from the
    /// `ins`/`del` mark) carrying the change id, with author/date/kind held in the `changes`
    /// container. An insertion flags real inserted text; a deletion flags retained-but-struck text.
    /// Round-trips through `text:tracked-changes` + inline change markers in `.odt` and `<w:ins>`/`<w:del>` run wrappers in `.docx`.
    #[serde(default)]
    pub changes: Vec<ChangeView>,
    /// Bookmarks (named anchors), each a `(name, range)` derived from the body `bookmark` marks — the
    /// mark is the source of truth, this is a convenience registry. Round-trips through
    /// `text:bookmark-start`/`-end` in `.odt` and `w:bookmarkStart`/`-End` in `.docx`. A cross-reference
    /// is an internal `link` mark valued `#<name>`, surfaced as an ordinary `link` mark rather than in a separate list.
    #[serde(default)]
    pub bookmarks: Vec<BookmarkView>,
    /// Document-wide multi-column section layout: the number of newspaper-style columns the body text
    /// flows across. `1` (the default) for a single-column document. Round-trips through the `.odt` `style:columns` and `.docx` `w:sectPr`→`w:cols`.
    #[serde(default = "default_columns")]
    pub columns: u32,
    /// The gutter between columns, in px, or `None` when unspecified (only meaningful when `columns > 1`); maps onto `.odt` `style:column-gap` and `.docx` `w:cols w:space`.
    #[serde(default)]
    pub column_gap: Option<f32>,
    /// The document default language as a BCP-47 tag (`en-US`, `de-DE`, `fr`), or `None` when the
    /// document declares none. Per-run overrides ride as a `lang` mark. Round-trips through the `.odt`
    /// `fo:language`/`fo:country` and `.docx` `docDefaults` `w:lang`; supports spell-check locale and screen-reader language.
    #[serde(default)]
    pub language: Option<String>,
}

/// The default column count for a [`DocSnapshot`] — a single column. Used both as the serde
/// fallback for older snapshots and as the `Default` value.
fn default_columns() -> u32 {
    crate::schema::DEFAULT_COLUMNS
}

impl Default for DocSnapshot {
    fn default() -> Self {
        // Hand-rolled (rather than `#[derive(Default)]`) so `columns` defaults to a single column, not the integer default of zero.
        DocSnapshot {
            schema: String::default(),
            schema_version: i64::default(),
            blocks: Vec::default(),
            blobs: BTreeMap::default(),
            styles: BTreeMap::default(),
            properties: BTreeMap::default(),
            header: Vec::default(),
            footer: Vec::default(),
            notes: Vec::default(),
            comments: Vec::default(),
            changes: Vec::default(),
            bookmarks: Vec::default(),
            columns: default_columns(),
            column_gap: None,
            language: None,
        }
    }
}

/// One bookmark (named anchor): its `name` and the anchored body range `[start, end)` it covers
/// (recovered from the `bookmark` mark). A cross-reference targets it by a `link` mark valued `#<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkView {
    pub name: String,
    /// `(block_id, start, end)` of the anchored range in the body.
    pub range: (String, usize, usize),
}

/// Whether a tracked change is an insertion (proposed-added text) or a deletion (retained-but-struck text); stored as the change's `kind` scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    Ins,
    Del,
}

impl ChangeKind {
    /// The canonical schema string (`"ins"`/`"del"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Ins => "ins",
            ChangeKind::Del => "del",
        }
    }
    /// Parse a schema string back into a [`ChangeKind`] (`del` → `Del`, anything else → `Ins`, the default).
    #[allow(
        clippy::should_implement_trait,
        reason = "returns Self infallibly by design, not std::str::FromStr's fallible contract"
    )]
    pub fn from_str(s: &str) -> Self {
        if s == "del" {
            ChangeKind::Del
        } else {
            ChangeKind::Ins
        }
    }
}

/// One tracked change / revision mark: its id, kind, author, date string, and the anchored body
/// range `[start, end)` (recovered from the `ins`/`del` mark; `None` if the mark is absent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeView {
    pub id: String,
    pub kind: ChangeKind,
    pub author: String,
    pub date: String,
    /// `(block_id, start, end)` of the anchored range in the body, or `None` if the anchoring mark is absent.
    #[serde(default)]
    pub range: Option<(String, usize, usize)>,
}

/// One comment / annotation: its id, author, date string, content blocks, and the anchored body
/// range `[start, end)` (recovered from the `comment` mark; `None` if absent, e.g. the commented text was deleted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentView {
    pub id: String,
    pub author: String,
    pub date: String,
    pub blocks: Vec<BlockView>,
    /// Ordered, durable thread replies. Older documents have no `replies`
    /// map and materialize this as an empty list.
    #[serde(default)]
    pub replies: Vec<CommentReplyView>,
    /// `(block_id, start, end)` of the anchored range in the body, or `None` if the anchoring mark is absent.
    #[serde(default)]
    pub range: Option<(String, usize, usize)>,
}

/// One reply in a durable comment thread. Reply ids are stable CRDT ids rather
/// than list positions, so simultaneous replies merge without one replacing the
/// other; consumers display them in canonical id order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentReplyView {
    pub id: String,
    pub author: String,
    pub date: String,
    pub blocks: Vec<BlockView>,
}

/// One footnote/endnote: its id, kind (`"footnote"`/`"endnote"`), and the block sequence forming its content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteView {
    pub id: String,
    pub kind: String,
    pub blocks: Vec<BlockView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockView {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub marks: Vec<MarkSpan>,
    pub attrs: BTreeMap<String, AttrVal>,
    pub children: Vec<BlockView>,
}
