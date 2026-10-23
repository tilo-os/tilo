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

impl AttrVal {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocSnapshot {
    pub schema: String,
    pub schema_version: i64,
    pub blocks: Vec<BlockView>,
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
    /// container. An insertion marks inserted text; a deletion marks retained-
    /// but-struck text.
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

impl DocSnapshot {
    /// Every content address an `image` block anywhere in the snapshot (body, header, footer,
    /// notes, comments and their replies, nested children) names in its `blob` attr: the binary
    /// content a renderer or exporter has to fetch to draw this version.
    pub fn image_content(&self) -> std::collections::BTreeSet<String> {
        fn walk(blocks: &[BlockView], out: &mut std::collections::BTreeSet<String>) {
            for b in blocks {
                if let Some(AttrVal::Str(hash)) = b.attrs.get("blob") {
                    out.insert(hash.clone());
                }
                walk(&b.children, out);
            }
        }
        let mut out = std::collections::BTreeSet::new();
        walk(&self.blocks, &mut out);
        walk(&self.header, &mut out);
        walk(&self.footer, &mut out);
        for n in &self.notes {
            walk(&n.blocks, &mut out);
        }
        for c in &self.comments {
            walk(&c.blocks, &mut out);
            for r in &c.replies {
                walk(&r.blocks, &mut out);
            }
        }
        out
    }
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

impl BlockView {
    /// The string attribute `key`; `None` when unset or not a string.
    pub fn str_attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).and_then(AttrVal::as_str)
    }

    /// The integer attribute `key`; `None` when unset or not an integer.
    pub fn int_attr(&self, key: &str) -> Option<i64> {
        self.attrs.get(key).and_then(AttrVal::as_int)
    }

    /// The boolean attribute `key`; `None` when unset or not a boolean.
    pub fn bool_attr(&self, key: &str) -> Option<bool> {
        self.attrs.get(key).and_then(AttrVal::as_bool)
    }

    /// Whether the boolean attribute `key` is set and true.
    pub fn flag(&self, key: &str) -> bool {
        self.bool_attr(key) == Some(true)
    }

    /// A table cell's `(row, col)`; a missing or negative index reads as 0.
    pub fn cell_position(&self) -> (usize, usize) {
        let index = |key| {
            self.int_attr(key)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0)
        };
        (index("row"), index("col"))
    }

    /// A table cell's `colspan` or `rowspan`: at least 1, capped at 1024.
    pub fn cell_span(&self, key: &str) -> usize {
        self.int_attr(key)
            .and_then(|n| usize::try_from(n).ok())
            .map_or(1, |n| n.clamp(1, 1024))
    }
}

/// The document as of position `version` of
/// [`tilo_resource::history::versions`] (time travel).
///
/// # Errors
/// Returns [`DocError`](crate::DocError) if Automerge rejects the replayed
/// prefix, or if the rebuilt replica has no readable document root.
pub fn at(doc: &mut automerge::AutoCommit, version: usize) -> Result<DocSnapshot, crate::DocError> {
    let rebuilt = tilo_resource::history::replay_prefix(doc, version)
        .map_err(|e| crate::DocError::Invalid(format!("apply_changes: {e}")))?;
    let c = crate::model::open(&rebuilt, &automerge::ROOT)?;
    Ok(crate::model::materialize(&rebuilt, &c))
}

/// The snapshot of `doc`, when it carries the tilo-doc schema.
pub fn of(doc: &automerge::AutoCommit) -> Option<DocSnapshot> {
    let c = crate::model::open(doc, &automerge::ROOT).ok()?;
    Some(crate::model::materialize(doc, &c))
}

/// The document as of each of `versions` (ascending positions), from one
/// replay of the change log. A version Automerge rejects, and every later
/// one, is `None`.
pub fn series(doc: &mut automerge::AutoCommit, versions: &[usize]) -> Vec<Option<DocSnapshot>> {
    tilo_resource::history::replay_series(doc, versions, of)
}

#[cfg(test)]
mod version_tests {
    use crate::Document;

    #[test]
    fn version_chain_and_time_travel() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        d.splice_text(&a, 0, 0, "first");
        d.splice_text(&a, 5, 0, " edit");
        let b = d.insert_block(Some(&a), "paragraph");
        d.splice_text(&b, 0, 0, "second");

        let vers = d.versions();
        // schema-init + 5 edits → several authored changes.
        assert!(
            vers.len() >= 4,
            "expected a multi-entry timeline, got {}",
            vers.len()
        );
        assert!(
            vers.iter().all(|v| !v.actor.is_empty()),
            "every change has an author"
        );

        let latest = d.materialize_at(vers.len() - 1).unwrap();
        assert_eq!(latest.blocks.len(), 2, "latest has both paragraphs");
        let v0 = d.materialize_at(0).unwrap();
        assert_eq!(v0.blocks.len(), 0, "version 0 is just the schema init");

        // Some intermediate version has exactly one block (after the first insert, before the second).
        let saw_one = (0..vers.len()).any(|i| d.materialize_at(i).unwrap().blocks.len() == 1);
        assert!(
            saw_one,
            "an intermediate version should have a single block"
        );
    }

    #[test]
    fn time_travel_shows_text_growth() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello");
        let mid = d.versions().len() - 1; // after "hello"
        d.splice_text(&p, 5, 0, " world");

        let at_mid = d.materialize_at(mid).unwrap();
        assert_eq!(at_mid.blocks[0].text, "hello");
        let last = d.versions().len() - 1;
        let now = d.materialize_at(last).unwrap();
        assert_eq!(now.blocks[0].text, "hello world");
    }

    #[test]
    fn one_replay_matches_materializing_each_version() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        d.splice_text(&a, 0, 0, "first");
        d.splice_text(&a, 5, 0, " edit");
        let b = d.insert_block(Some(&a), "paragraph");
        d.splice_text(&b, 0, 0, "second");
        let count = d.versions().len();
        let every: Vec<usize> = (0..count).collect();
        let series = super::series(&mut d.doc, &every);
        assert_eq!(series.len(), count);
        for (version, snapshot) in every.iter().zip(&series) {
            assert_eq!(
                snapshot.as_ref(),
                Some(&d.materialize_at(*version).unwrap()),
                "version {version}"
            );
        }
        let window = [1, count - 1, count + 3];
        let series = super::series(&mut d.doc, &window);
        assert_eq!(series[0].as_ref(), Some(&d.materialize_at(1).unwrap()));
        assert_eq!(
            series[2].as_ref(),
            Some(&d.materialize_at(count - 1).unwrap())
        );
    }
}
