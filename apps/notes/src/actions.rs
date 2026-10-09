//! What the window and tests build to change the notebook, and what the
//! notebook's notes say in the editor's terms.
//!
//! Every change is the [`Notebook`]'s action: a field edit at a path (a
//! body's rich-text op, a tag on a note, a tag's name, a note added under an
//! id the window chose) or one of its verbs. The builders here name the
//! common ones so the window, the scenarios and the tests say the same
//! thing the same way.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use tilo_actions::EncodedAction;
use tilo_document::RichTextAction;
use ui::tilo_resource::{Atom, Resource, Stamped, items};
use ui::widget::marks::{Mark, MarkSpan};
use ui::widget::{BlockContent, BlockKind};

use crate::model::{PageId, PageMeta, TagId};
use crate::notebook::{self, Notebook, NotebookAction, Page, Tag, page_id};

/// The notebook's action: a field edit or a verb, stamped with when it was
/// authored.
pub type Action = <Notebook as Resource>::Action;

/// The resource a note is published as: its item of the notebook.
#[must_use]
pub fn page_resource_id(page: PageId) -> String {
    items::item_resource_id::<Notebook>("pages", &page_id(page))
}

/// The note a published resource id names.
#[must_use]
pub fn page_of_resource(id: &str) -> Option<PageId> {
    match items::item_of_resource::<Notebook>(id) {
        Some(("pages", item)) => notebook::page_of(&item),
        _ => None,
    }
}

/// The resource a tag is published as.
#[must_use]
pub fn tag_resource_id(tag: TagId) -> String {
    items::item_resource_id::<Notebook>("tags", &page_id(tag))
}

fn stamped(operation: NotebookAction) -> Action {
    Stamped {
        operation,
        now_ms: 0,
    }
}

fn edit(edit: ui::tilo_resource::Edit<Notebook>) -> Action {
    stamped(NotebookAction::FieldEdit(edit))
}

/// The wire form of `action`, for applying it to a document directly.
///
/// # Panics
/// Never: a notebook action is plain data that always serializes.
#[allow(
    clippy::expect_used,
    reason = "the notebook's actions serialize with serde_json over owned plain data"
)]
#[must_use]
pub fn encode(action: &Action) -> EncodedAction {
    ui::tilo_resource::encode::<Notebook>(action).expect("a notebook action serializes")
}

/// Add note `id` titled `title` under `parent`, after its siblings: the
/// window's New, which knows the id it opens next.
#[must_use]
pub fn create_page_action(id: PageId, parent: Option<PageId>, title: &str) -> Action {
    edit(Notebook::paths().pages.insert_as(
        &page_id(id),
        usize::MAX,
        &Page::titled(title, parent.map(page_id)),
    ))
}

/// One rich-text op on `page`'s body.
///
/// # Panics
/// Never: a rich-text op is plain data that always serializes.
#[allow(
    clippy::expect_used,
    reason = "a rich-text op is plain data that always serializes"
)]
#[must_use]
pub fn edit_body_action(page: PageId, op: impl std::borrow::Borrow<RichTextAction>) -> Action {
    edit(
        Notebook::paths()
            .pages
            .item(&page_id(page))
            .body
            .perform(op.borrow())
            .expect("a rich-text op serializes"),
    )
}

/// Mark the characters `range` of `block` with `mark`: a format, a page link.
#[must_use]
pub fn set_mark_action(
    page: PageId,
    block: &str,
    range: std::ops::Range<usize>,
    mark: &Mark,
) -> Action {
    edit_body_action(page, notebook::mark_op(block, range, mark))
}

/// Replace `page`'s body with `blocks`.
#[must_use]
pub fn write_body_action(page: PageId, blocks: &[BlockContent]) -> Action {
    edit_body_action(page, notebook::clips_op(blocks))
}

/// Rename note `page`; links to it read the new title.
#[must_use]
pub fn rename_page_action(page: PageId, title: &str) -> Action {
    stamped(NotebookAction::Rename(notebook::notebook::Rename {
        page: page_id(page),
        title: title.to_owned(),
    }))
}

/// Delete note `page` and every note under it.
#[must_use]
pub fn delete_page_action(page: PageId) -> Action {
    stamped(NotebookAction::Delete(notebook::notebook::Delete {
        page: page_id(page),
    }))
}

/// Put note `page` under `parent`, before `before` (last when none).
#[must_use]
pub fn move_page_action(page: PageId, parent: Option<PageId>, before: Option<PageId>) -> Action {
    stamped(NotebookAction::MoveNote(notebook::notebook::MoveNote {
        page: page_id(page),
        parent: parent.map(page_id),
        before: before.map(page_id),
    }))
}

/// Copy note `page` and every note under it, right after it.
#[must_use]
pub fn duplicate_action(page: PageId) -> Action {
    stamped(NotebookAction::Duplicate(notebook::notebook::Duplicate {
        page: page_id(page),
    }))
}

/// Keep the picture `bytes` with the notebook and show it in note `page`
/// by `show` (at the end of the note when none).
#[must_use]
pub fn place_image_action(page: PageId, bytes: Vec<u8>, show: Option<RichTextAction>) -> Action {
    stamped(NotebookAction::PlaceImage(notebook::notebook::PlaceImage {
        page: page_id(page),
        bytes,
        show,
    }))
}

/// Import `markdown` as a new top-level note titled `title` (kept under
/// `note` when given), with the pictures it shows.
#[must_use]
pub fn import_markdown_action(
    note: Option<PageId>,
    title: &str,
    markdown: &str,
    images: Vec<Vec<u8>>,
) -> Action {
    stamped(NotebookAction::ImportMarkdown(
        notebook::notebook::ImportMarkdown {
            title: title.to_owned(),
            markdown: markdown.to_owned(),
            images,
            note: note.map(page_id),
        },
    ))
}

/// Add tag `id` named `name`.
#[must_use]
pub fn new_tag_action(id: TagId, name: &str) -> Action {
    edit(Notebook::paths().tags.insert_as(
        &page_id(id),
        usize::MAX,
        &Tag {
            name: Atom(name.trim().to_owned()),
        },
    ))
}

/// Rename tag `id`.
#[must_use]
pub fn rename_tag_action(id: TagId, name: &str) -> Action {
    edit(
        Notebook::paths()
            .tags
            .item(&page_id(id))
            .name
            .set(Atom(name.trim().to_owned())),
    )
}

/// Delete tag `id` and take it off every note.
#[must_use]
pub fn delete_tag_action(id: TagId) -> Action {
    stamped(NotebookAction::DeleteTag(notebook::notebook::DeleteTag {
        tag: page_id(id),
    }))
}

/// Give note `page` tag `tag`, or take it off.
#[must_use]
pub fn set_tag_assignment_action(page: PageId, tag: TagId, assigned: bool) -> Action {
    let tags = Notebook::paths().pages.item(&page_id(page)).tags;
    edit(if assigned {
        tags.insert(&page_id(tag))
    } else {
        tags.remove(&page_id(tag))
    })
}

/// Give note `page` the tag named `name`, made when the notebook has none.
#[must_use]
pub fn tag_page_action(page: PageId, name: &str) -> Action {
    stamped(NotebookAction::Tag(notebook::notebook::Tag {
        page: page_id(page),
        name: name.to_owned(),
    }))
}

/// Serializable inline formatting of a Notes block, as tests and exports
/// compare it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InlineMark {
    Bold,
    Italic,
    Code,
    Underline,
    Strikethrough,
    Link {
        page: String,
    },
    /// A block-level live page preview, addressed by stable page UUID.
    Transclusion {
        page: String,
    },
    ExternalLink {
        url: String,
    },
    /// A reference to a participant, by id.
    Mention {
        id: String,
    },
}

/// A half-open UTF-8 byte range with one inline mark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineMarkSpan {
    pub start: usize,
    pub end: usize,
    pub mark: InlineMark,
}

impl InlineMarkSpan {
    pub fn from_ui(span: &MarkSpan) -> Self {
        let mark = match &span.mark {
            Mark::Bold => InlineMark::Bold,
            Mark::Italic => InlineMark::Italic,
            Mark::Code => InlineMark::Code,
            Mark::Underline => InlineMark::Underline,
            Mark::Strikethrough => InlineMark::Strikethrough,
            Mark::Link(page) => InlineMark::Link {
                page: page.to_string(),
            },
            Mark::Transclusion(page) => InlineMark::Transclusion {
                page: page.to_string(),
            },
            Mark::ExternalLink(url) => InlineMark::ExternalLink { url: url.clone() },
            Mark::Mention(id) => InlineMark::Mention { id: id.clone() },
        };
        Self {
            start: span.range.start,
            end: span.range.end,
            mark,
        }
    }

    pub fn to_ui(&self) -> Option<MarkSpan> {
        let mark = match &self.mark {
            InlineMark::Bold => Mark::Bold,
            InlineMark::Italic => Mark::Italic,
            InlineMark::Code => Mark::Code,
            InlineMark::Underline => Mark::Underline,
            InlineMark::Strikethrough => Mark::Strikethrough,
            InlineMark::Link { page } => Mark::Link(page.parse().ok()?),
            InlineMark::Transclusion { page } => Mark::Transclusion(page.parse().ok()?),
            InlineMark::ExternalLink { url } => {
                crate::markdown::safe_external_url(url).then(|| Mark::ExternalLink(url.clone()))?
            }
            InlineMark::Mention { id } => Mark::Mention(id.clone()),
        };
        Some(MarkSpan::new(self.start..self.end, mark))
    }

    fn rank(&self) -> u8 {
        match self.mark {
            InlineMark::Bold => 0,
            InlineMark::Italic => 1,
            InlineMark::Underline => 2,
            InlineMark::Strikethrough => 3,
            InlineMark::Code => 4,
            InlineMark::Link { .. } => 5,
            InlineMark::Transclusion { .. } => 6,
            InlineMark::ExternalLink { .. } => 7,
            InlineMark::Mention { .. } => 8,
        }
    }
}

/// One block of a note as the notebook stores it: its stable id, its kind,
/// its text and its Notes formatting and page links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlock {
    pub id: String,
    pub kind: BlockKind,
    pub text: String,
    pub marks: Vec<InlineMarkSpan>,
    /// Outline depth of a block that is not a list item.
    pub nesting: u8,
    /// The fence a code line belongs to.
    pub code_group: Option<String>,
}

/// What every note says, by page.
pub type Bodies = HashMap<PageId, Vec<StoredBlock>>;

/// Every block of `page` with its Notes formatting and page links, in order.
#[must_use]
pub fn stored_blocks(page: &Page) -> Vec<StoredBlock> {
    page.blocks()
        .into_iter()
        .map(|block| {
            let mut marks = block
                .marks
                .iter()
                .map(InlineMarkSpan::from_ui)
                .collect::<Vec<_>>();
            marks.sort_by_key(|span| (span.start, span.end, span.rank()));
            StoredBlock {
                id: block.id,
                kind: block.kind,
                text: block.text,
                marks,
                nesting: block.nesting,
                code_group: block.code_group,
            }
        })
        .collect()
}

/// Note `page` as Markdown, as the notebook holds it.
pub fn page_markdown(workspace: &crate::Workspace, page: PageId) -> String {
    let blocks = workspace
        .bodies
        .get(&page)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|block| {
            ui::widget::Block::from_content(BlockContent {
                id: block.id.clone(),
                kind: block.kind.clone(),
                text: block.text.clone(),
                marks: block
                    .marks
                    .iter()
                    .filter_map(InlineMarkSpan::to_ui)
                    .collect(),
                nesting: block.nesting,
                code_group: block.code_group.clone(),
                authors: Vec::new(),
            })
        })
        .collect::<Vec<_>>();
    crate::markdown::serialize(&crate::markdown::blocks_to_specs(&blocks))
}

/// A block of another note that links to a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    pub target: PageId,
    pub source: PageId,
    pub block_id: String,
    pub source_title: String,
    pub excerpt: String,
}

/// A one-line label for a page.
fn page_label(m: &PageMeta) -> String {
    let t = m.title.trim();
    if t.is_empty() {
        notebook::UNTITLED.to_string()
    } else {
        t.to_string()
    }
}

/// The blocks of other notes linking to `target`.
#[must_use]
pub fn backlinks(bodies: &Bodies, pages: &[PageMeta], target: PageId) -> Vec<Backlink> {
    backlink_index(bodies, pages)
        .remove(&target)
        .unwrap_or_default()
}

/// Every note's backlinks, by the note linked to.
#[must_use]
pub fn backlink_index(bodies: &Bodies, pages: &[PageMeta]) -> HashMap<PageId, Vec<Backlink>> {
    let existing = pages.iter().map(|page| page.id).collect::<HashSet<_>>();
    let mut by_target = HashMap::<PageId, Vec<Backlink>>::new();
    for source in pages {
        for block in bodies.get(&source.id).map_or(&[][..], Vec::as_slice) {
            let excerpt = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
            let targets = block
                .marks
                .iter()
                .filter_map(|span| match &span.mark {
                    InlineMark::Link { page } => page.parse::<PageId>().ok(),
                    _ => None,
                })
                .filter(|target| *target != source.id && existing.contains(target))
                .collect::<HashSet<_>>();
            for target in targets {
                by_target.entry(target).or_default().push(Backlink {
                    target,
                    source: source.id,
                    block_id: block.id.clone(),
                    source_title: page_label(source),
                    excerpt: excerpt.clone(),
                });
            }
        }
    }
    for backlinks in by_target.values_mut() {
        backlinks.sort_by(|a, b| {
            (a.source_title.to_lowercase(), a.source, a.block_id.as_str()).cmp(&(
                b.source_title.to_lowercase(),
                b.source,
                b.block_id.as_str(),
            ))
        });
    }
    by_target
}
