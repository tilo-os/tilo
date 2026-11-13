//! Read-only materialized view of a `Document` for rendering/export.

use crate::document::MarkSpan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub schema_version: u64,
    pub blocks: Vec<BlockView>,
    /// Named styles: `style_id → (property → value)`. Built-ins (body/heading1…/quote) plus any
    /// user- or import-defined styles.
    #[serde(default)]
    pub styles: BTreeMap<String, BTreeMap<String, AttrVal>>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    /// Page-header blocks, rendered at the top of every page; a `field` mark valued `page-number`
    /// over a placeholder marks a page-number field. Round-trips through the `.odt` master-page
    /// header and `.docx` `header1.xml`.
    #[serde(default)]
    pub header: Vec<BlockView>,
    #[serde(default)]
    pub footer: Vec<BlockView>,
    /// Footnotes and endnotes, each a block sequence keyed by note id and referenced from body text
    /// by a `note-ref` mark. Round-trips through `text:note` in `.odt` and `word/footnotes.xml`.
    #[serde(default)]
    pub notes: Vec<NoteView>,
    /// Comments, each anchored to a body range by a `comment` mark, with author, date and content
    /// in the `comments` container. Round-trips through `office:annotation` and
    /// `word/comments.xml`.
    #[serde(default)]
    pub comments: Vec<CommentView>,
    /// Tracked changes, each anchored by an `ins`/`del` mark: an insertion marks inserted text, a
    /// deletion retained-but-struck text. Round-trips through `text:tracked-changes` and
    /// `<w:ins>`/`<w:del>`.
    #[serde(default)]
    pub changes: Vec<ChangeView>,
    /// Bookmarks, each a `(name, range)` derived from the body `bookmark` marks, which are the
    /// source of truth. A cross-reference is an ordinary internal `link` mark valued `#<name>`.
    #[serde(default)]
    pub bookmarks: Vec<BookmarkView>,
    /// The number of newspaper-style columns the body flows across; `1` for a single-column
    /// document. Round-trips through the `.odt` `style:columns` and `.docx` `w:sectPr`→`w:cols`.
    #[serde(default = "default_columns")]
    pub columns: u32,
    #[serde(default)]
    pub column_gap: Option<f32>,
    /// The document default language as a BCP-47 tag, or `None` when it declares none; per-run
    /// overrides ride as a `lang` mark.
    #[serde(default)]
    pub language: Option<String>,
}

impl DocSnapshot {
    /// Every content address an `image` block anywhere in the snapshot (body, header, footer,
    /// notes, comments and replies, nested children) names in its `blob` attr: the binary content a
    /// renderer or exporter has to fetch.
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

fn default_columns() -> u32 {
    crate::schema::DEFAULT_COLUMNS
}

impl Default for DocSnapshot {
    fn default() -> Self {
        // Hand-rolled rather than derived so `columns` defaults to a single column, not the integer
        // default of zero.
        DocSnapshot {
            schema: String::default(),
            schema_version: u64::default(),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkView {
    pub name: String,
    pub range: (String, usize, usize),
}

/// A tracked change is an insertion (proposed-added text) or a deletion (retained-but-struck text);
/// stored as the change's `kind` scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    Ins,
    Del,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Ins => "ins",
            ChangeKind::Del => "del",
        }
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeView {
    pub id: String,
    pub kind: ChangeKind,
    pub author: String,
    pub date: String,
    #[serde(default)]
    pub range: Option<(String, usize, usize)>,
}

/// One comment: its id, author, date, content blocks, and the anchored body range `[start, end)`,
/// `None` if absent because the commented text was deleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentView {
    pub id: String,
    pub author: String,
    pub date: String,
    pub blocks: Vec<BlockView>,
    /// Ordered, durable thread replies; older documents have no replies map and materialize as an
    /// empty list.
    #[serde(default)]
    pub replies: Vec<CommentReplyView>,
    #[serde(default)]
    pub range: Option<(String, usize, usize)>,
}

/// One reply in a durable thread. Reply ids are stable CRDT ids rather than list positions, so
/// simultaneous replies merge without one replacing the other; consumers display them in id order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentReplyView {
    pub id: String,
    pub author: String,
    pub date: String,
    pub blocks: Vec<BlockView>,
}

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
    pub fn str_attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).and_then(AttrVal::as_str)
    }

    pub fn int_attr(&self, key: &str) -> Option<i64> {
        self.attrs.get(key).and_then(AttrVal::as_int)
    }

    pub fn bool_attr(&self, key: &str) -> Option<bool> {
        self.attrs.get(key).and_then(AttrVal::as_bool)
    }

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

/// The document as of position `version` of [`tilo_resource::history::versions`] (time travel).
///
/// # Errors
///
/// Returns [`DocError`](crate::DocError) if Automerge rejects the replayed prefix, or the rebuilt
/// replica has no readable document root.
pub fn at(doc: &mut automerge::AutoCommit, version: usize) -> Result<DocSnapshot, crate::DocError> {
    let rebuilt = tilo_resource::history::replay_prefix(doc, version)
        .map_err(|e| crate::DocError::Invalid(format!("apply_changes: {e}")))?;
    let c = crate::model::open(&rebuilt, &automerge::ROOT)?;
    Ok(crate::model::materialize(&rebuilt, &c))
}

pub fn of(doc: &automerge::AutoCommit) -> Option<DocSnapshot> {
    let c = crate::model::open(doc, &automerge::ROOT).ok()?;
    Some(crate::model::materialize(doc, &c))
}

/// The document as of each of `versions`, from one replay of the change log. A version Automerge
/// rejects, and every later one, is `None`.
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

        // Some intermediate version has exactly one block (after the first insert, before the
        // second).
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
