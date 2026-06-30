//! Read-only materialized view of a `Document` for rendering/export.

use crate::document::MarkSpan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A scalar block/style attribute value (heading level, list ordering, style id,
/// a style's font size, …). `Float` carries fractional values (style sizes /
/// spacing) that don't fit an integer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttrVal {
    Int(i64),
    Bool(bool),
    Str(String),
    Float(f64),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DocSnapshot {
    pub schema: String,
    pub schema_version: i64,
    pub blocks: Vec<BlockView>,
    /// Content-addressed binary store (blake3 hex → bytes). `image` blocks carry
    /// their `blob` attr as a key here so a renderer can fetch the pixels.
    #[serde(default)]
    pub blobs: BTreeMap<String, Vec<u8>>,
    /// CRDT-native named styles: `style_id → (property → value)`. Built-ins
    /// (body/heading1…/quote) plus any user- or import-defined styles. Empty for
    /// documents authored before the styles container was populated.
    #[serde(default)]
    pub styles: BTreeMap<String, BTreeMap<String, AttrVal>>,
    /// Document properties / metadata (`title`/`author`/`subject`/`keywords`/
    /// `created`/`modified`/`generator` → value). Round-trips through the `.odt`
    /// `meta.xml` and `.docx` `docProps/*.xml` parts. Empty for documents with no
    /// properties set (and absent in snapshots authored before this field existed).
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    /// Page-header block sequence (rendered at the top of every page). Empty for
    /// documents with no header. A `field` mark valued `page-number` (see
    /// `schema::MARK_FIELD`) over a placeholder character marks a page-number
    /// field. Round-trips through the `.odt` master-page header and the `.docx`
    /// `header1.xml` part.
    #[serde(default)]
    pub header: Vec<BlockView>,
    /// Page-footer block sequence (rendered at the bottom of every page). Mirrors
    /// `header`; round-trips through the `.odt` master-page footer and `.docx`
    /// `footer1.xml` part.
    #[serde(default)]
    pub footer: Vec<BlockView>,
    /// Footnotes / endnotes, each a small block sequence keyed by note id. The body
    /// text references a note via an inline `note-ref` mark (see
    /// `schema::MARK_NOTE_REF`) over a placeholder character carrying the note id.
    /// Round-trips through inline `text:note` elements in `.odt` and the
    /// `word/footnotes.xml` / `word/endnotes.xml` parts in `.docx`. Empty for
    /// documents with no notes (and absent in snapshots authored before this field).
    #[serde(default)]
    pub notes: Vec<NoteView>,
    /// Comments / annotations, each anchored to a body text range (recovered from
    /// the `comment` mark — see `schema::MARK_COMMENT`) carrying the comment id, with
    /// the author / date / content held in the `comments` container keyed by that id.
    /// Round-trips through `office:annotation`(+`office:annotation-end`) in `.odt`
    /// and `word/comments.xml` + `commentRangeStart`/`commentRangeEnd`/
    /// `commentReference` in `.docx`. Empty for documents with no comments (and absent
    /// in snapshots authored before this field existed).
    #[serde(default)]
    pub comments: Vec<CommentView>,
    /// Tracked changes / revision marks, each anchored to a body text range (recovered
    /// from the `ins`/`del` mark — see `schema::MARK_INS`/`schema::MARK_DEL`) carrying
    /// the change id, with the author / date / kind held in the `changes` container
    /// keyed by that id. A tracked **insertion** flags real inserted text; a tracked
    /// **deletion** flags retained-but-struck text. Round-trips through
    /// `text:tracked-changes`(+`text:changed-region`) + inline change markers in `.odt`
    /// and `<w:ins>`/`<w:del>` run wrappers in `.docx`. Empty for documents with no
    /// tracked changes (and absent in snapshots authored before this field existed).
    #[serde(default)]
    pub changes: Vec<ChangeView>,
}

/// Whether a tracked change is an insertion (proposed-added text) or a deletion
/// (retained-but-struck text). Stored as the change's `kind` scalar and round-tripped
/// through the codecs.
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
    /// Parse a schema string back into a [`ChangeKind`] (`del` → `Del`, anything else
    /// → `Ins`, the default).
    pub fn from_str(s: &str) -> Self {
        if s == "del" {
            ChangeKind::Del
        } else {
            ChangeKind::Ins
        }
    }
}

/// One tracked change / revision mark: its id, kind (insertion/deletion), author, date
/// string, and the anchored body range `[start, end)` it covers (recovered from the
/// `ins`/`del` mark; `None` if the anchoring mark is absent). The body text references
/// it by an `ins`/`del` mark valued with this `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeView {
    pub id: String,
    pub kind: ChangeKind,
    pub author: String,
    pub date: String,
    /// `(block_id, start, end)` of the anchored range in the body, or `None` if the
    /// anchoring mark is absent.
    #[serde(default)]
    pub range: Option<(String, usize, usize)>,
}

/// One comment / annotation: its id, author, date string, the block sequence forming
/// its content, and the anchored body range `[start, end)` it covers (recovered from
/// the `comment` mark; `None` if the anchoring mark is absent — e.g. the commented
/// text was deleted). The body text references it by a `comment` mark valued with
/// this `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentView {
    pub id: String,
    pub author: String,
    pub date: String,
    pub blocks: Vec<BlockView>,
    /// `(block_id, start, end)` of the anchored range in the body, or `None` if the
    /// anchoring mark is absent.
    #[serde(default)]
    pub range: Option<(String, usize, usize)>,
}

/// One footnote/endnote: its id, kind (`"footnote"`/`"endnote"`), and the block
/// sequence forming its content. The body text references it by a `note-ref` mark
/// valued with this `id`.
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
