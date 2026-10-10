//! Notes' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Notes performs on its shared page tree is a
//! [`NotesAction`]: a small serializable value that deterministically mutates the
//! CRDT. Because the tree already lives in the shared document (`root["pages"]`,
//! keyed by page uuid — see [`crate::doc`]), an action replicates and replays for
//! free, and feeds straight back into the sidebar's page list. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the sidebar's "+" button,
//! an inline rename, a row's Delete, and semantic editor deltas) dispatch and
//! what the Ctrl+K palette dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: a notebook owns its pages, and a
//! page owns its child pages (the tree nests arbitrarily deep). Each resource
//! exports the [`ActionSpec`]s Ctrl+K offers for it (rename, delete, new child).
//!
//! Presentation — which page is selected, the scroll position, whether the
//! sidebar drawer is open, an expand/collapse toggle, the search query — stays
//! out of the document and never becomes an action.
//!
//! ## Determinism contract
//!
//! Page ids are minted at *authoring* time and carried in the payload;
//! [`Action::apply`] never touches the RNG or the wall-clock. A malformed payload
//! (an unparseable id) is a **no-op** rather than an error, so a bad replay can't
//! wedge a peer. Deletes are recomputed from the
//! document at the replay's base state, so two replicas at the same heads land on
//! the byte-identical result. Page *body* text is edited by splicing directly
//! into the per-block Automerge `Text` (the toolkit's CRDT-native text path); it
//! is represented by UUID-addressed block operations in an action. The shared
//! widget only computes those operations; it never writes the document itself.

use std::collections::{HashMap, HashSet};

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use tilo_actions::{
    Action, ActionAddress, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ParamType, Resource, ResourcePresentation,
    ResourceViewRangeSpec, ResourceViewSpec,
};

use crate::continuation::{self, BuilderDomain};
use crate::doc::NotesDoc;
use crate::effect::NotesEffect;
use crate::model::{PageId, PageMeta, TagId, TagMeta};
use tilo_document::RichText;
use tilo_document::RichTextAction;
use tilo_document::action::RegionKind;
use ui::tilo_resource::Effect as _;
use ui::widget::marks::{Mark, MarkSpan};
use ui::widget::{BlockContent, BlockKind};

/// Registry `type_id` for every Notes action.
pub const NOTES_ACTION_TYPE: &str = "notes.action";
pub const PAGE_EDITOR_VIEW_ID: &str = "editor";

pub fn page_resource_id(page: PageId) -> String {
    format!("notes.page:{page}")
}

pub fn block_resource_id(page: PageId, block_id: &str) -> String {
    format!("notes.block:{page}:{block_id}")
}

pub fn tag_resource_id(tag: TagId) -> String {
    format!("notes.tag:{tag}")
}

/// Parse an opaque stable block resource id without accepting a nearby page
/// or block after a collaborative deletion/reorder.
pub fn parse_block_resource_id(id: &str) -> Option<(PageId, &str)> {
    let (page, block) = id.strip_prefix("notes.block:")?.split_once(':')?;
    if block.is_empty() {
        return None;
    }
    Some((page.parse().ok()?, block))
}

pub fn open_page_address(page: PageId) -> ActionAddress {
    ActionAddress::new(page_resource_id(page), "open")
}

pub fn open_block_address(page: PageId, block_id: &str) -> ActionAddress {
    ActionAddress::new(block_resource_id(page, block_id), "open")
}

pub fn backlinks_resource_id(page: PageId) -> String {
    format!("notes.backlinks:{page}")
}

pub fn backlink_resource_id(target: PageId, source: PageId, block_id: &str) -> String {
    format!("notes.backlink:{target}:{source}:{block_id}")
}

pub fn parse_backlink_resource_id(id: &str) -> Option<(PageId, PageId, &str)> {
    let mut parts = id.strip_prefix("notes.backlink:")?.splitn(3, ':');
    let target = parts.next()?.parse().ok()?;
    let source = parts.next()?.parse().ok()?;
    let block = parts.next()?;
    (!block.is_empty()).then_some((target, source, block))
}

/// A serializable snapshot of a page's metadata, in exactly the representation
/// the document stores (ids as strings, an absent parent as `""`). Carrying the
/// document shape keeps [`Action::apply`] a pure write and makes
/// [`Self::from_meta`]/`Self::to_meta` lossless round-trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageData {
    /// Page uuid (string form), minted at authoring time.
    pub id: String,
    pub title: String,
    /// Parent page uuid (string form), or `""` for a root page.
    pub parent: String,
    /// Fractional order key among the page's siblings.
    pub order: String,
}

/// Wire-stable representation of a tag. Its UUID is minted before dispatch,
/// never inside `Action::apply`, so replay is absolute and deterministic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagData {
    pub id: String,
    pub name: String,
}

impl TagData {
    pub fn from_meta(tag: &TagMeta) -> Self {
        Self {
            id: tag.id.to_string(),
            name: tag.name.clone(),
        }
    }

    fn to_meta(&self) -> Option<TagMeta> {
        let name = self.name.trim();
        if name.is_empty() {
            return None;
        }
        Some(TagMeta {
            id: self.id.parse().ok()?,
            name: name.to_string(),
        })
    }
}

impl PageData {
    pub fn from_meta(m: &PageMeta) -> Self {
        Self {
            id: m.id.to_string(),
            title: m.title.clone(),
            parent: m.parent.map(|p| p.to_string()).unwrap_or_default(),
            order: m.order.clone(),
        }
    }

    /// Reconstruct the model meta, or `None` when the id or order key is
    /// malformed so `apply` can no-op instead of writing a garbage page.
    fn to_meta(&self) -> Option<PageMeta> {
        let id = self.id.parse::<PageId>().ok()?;
        if !ui::tilo_resource::order::is_key(&self.order) {
            return None;
        }
        let parent = if self.parent.is_empty() {
            None
        } else {
            self.parent.parse::<PageId>().ok()
        };
        Some(PageMeta {
            id,
            title: self.title.clone(),
            parent,
            order: self.order.clone(),
        })
    }
}

/// Every collaborative operation Notes exports on its shared page tree.
///
/// The set is deliberately small: a create, a rename, a re-parent/reorder, and
/// a single field edit are all one whole-page write ([`Self::UpsertPage`]); a
/// delete removes a page and its whole subtree; a body edit is one
/// [`RichTextAction`] on the page's rich text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NotesAction {
    /// Create or replace a page's metadata (title / parent / order). One variant
    /// covers create, rename, re-parent, and reorder — all are a metadata write.
    /// The title is diff-spliced so a rename doesn't tombstone the whole title.
    UpsertPage(PageData),
    /// Create a page after its parent's current children. The position is
    /// resolved from the document at the replay's base state, so a create
    /// authored without the current tree (an agent, a script, a second create
    /// before the tree republishes) never takes a sibling's place. Two
    /// replicas appending at once get the same key and order by page id.
    AppendPage {
        id: String,
        title: String,
        /// Parent page uuid, or `""` for a root page.
        parent: String,
    },
    /// Change an existing tree atomically against its complete metadata preimage.
    UpdatePageTree {
        before: Vec<PageData>,
        after: Vec<PageData>,
    },
    /// Delete a page and every descendant. The descendant set is recomputed
    /// from the document at the replay's base state, so replicas converge.
    /// Surviving siblings keep their fractional keys.
    DeletePage { id: String },
    /// Copy the exact source subtree using only these pre-minted identities.
    DuplicateSubtree {
        source: String,
        pages: Vec<DuplicatePage>,
    },
    /// One rich-text edit of a page's body. This is the only Notes path
    /// allowed to change a body after it is written: the editor, the palette
    /// and agents all edit through it.
    EditBody { page: String, edit: RichTextAction },
    /// Write a page's first content into its still-empty body: a page seeded
    /// from Markdown. Refused once the body holds text.
    WriteBody {
        page: String,
        blocks: Vec<PageBlock>,
    },
    /// Add one content-addressed image and the body edit referencing it in
    /// the same replayable mutation. The digest is verified before either
    /// write.
    AddImageAsset {
        page: String,
        digest: String,
        bytes: Vec<u8>,
        edit: RichTextAction,
    },
    /// Create or rename one independently keyed tag.
    UpsertTag(TagData),
    /// Tombstone a tag plus every keyed membership edge currently known for it.
    DeleteTag { id: String },
    /// Set one membership edge to an absolute state. This is deliberately not
    /// a toggle: retries and out-of-order replays stay idempotent.
    SetTagAssignment {
        page: String,
        tag: String,
        assigned: bool,
    },
    /// One atomic convenience action for the normal "add a new tag to this
    /// note" path. Both ids are authored up front, so it is as replayable as
    /// the lower-level upsert + absolute membership operations.
    CreateAndAssignTag { page: String, tag: TagData },
    /// Import one Markdown document as a new root note after the last one,
    /// in one executor transaction, so malformed content cannot leave behind
    /// a successful-looking empty page. Its `[[Title]]` links resolve
    /// against the notebook the import lands in.
    ImportMarkdown {
        page: String,
        title: String,
        markdown: String,
        assets: Vec<(String, Vec<u8>)>,
    },
}

/// One block of a page, as an import carries it: the editor's kind (its
/// stored spelling), text, marks, outline depth and code fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageBlock {
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub marks: Vec<InlineMarkSpan>,
    #[serde(default)]
    pub nesting: u8,
    #[serde(default)]
    pub code_group: Option<String>,
}

impl PageBlock {
    pub fn of(block: &BlockContent) -> Self {
        Self {
            kind: block.kind.stored(),
            text: block.text.clone(),
            marks: block.marks.iter().map(InlineMarkSpan::from_ui).collect(),
            nesting: block.nesting,
            code_group: block.code_group.clone(),
        }
    }

    /// The editor's block, when every part of it is valid.
    fn content(&self) -> Option<BlockContent> {
        let marks = self
            .marks
            .iter()
            .map(InlineMarkSpan::to_ui)
            .collect::<Option<Vec<_>>>()?;
        if marks.iter().any(|span| {
            span.range.start >= span.range.end
                || !self.text.is_char_boundary(span.range.start)
                || !self.text.is_char_boundary(span.range.end)
                || span.range.end > self.text.len()
        }) {
            return None;
        }
        Some(BlockContent {
            kind: BlockKind::from_stored(&self.kind)?,
            marks,
            nesting: self.nesting.min(crate::markdown::MAX_LIST_INDENT),
            code_group: self.code_group.clone(),
            ..BlockContent::paragraph("", self.text.clone())
        })
    }
}

fn update_page_tree(doc: &mut Doc, before: &[PageData], after: &[PageData]) -> Result<(), String> {
    let mut workspace = crate::notebook::project(doc, None)?;
    let mut current = workspace
        .pages()
        .iter()
        .map(PageData::from_meta)
        .collect::<Vec<_>>();
    current.sort_by(|a, b| a.id.cmp(&b.id));
    if current != before {
        return Err("note tree changed since the action was authored".into());
    }
    if before.len() != after.len() || before.iter().zip(after).any(|(a, b)| a.id != b.id) {
        return Err("note tree update must preserve every existing note identity".into());
    }
    let pages = after
        .iter()
        .map(|data| {
            let page = data.to_meta().ok_or("invalid note identity")?;
            if page.id.is_nil() || (!data.parent.is_empty() && page.parent.is_none()) {
                return Err("invalid note or parent identity");
            }
            Ok(page)
        })
        .collect::<Result<Vec<_>, _>>()?;
    workspace.data.pages = pages;
    workspace.check_invariants()?;
    for (old, page) in current.iter().zip(&workspace.data.pages) {
        if *old != PageData::from_meta(page) {
            NotesDoc::upsert_page_meta(doc, page);
        }
    }
    Ok(())
}

/// Serializable inline formatting stored in a Notes block's native Automerge
/// text marks. Keeping this small wire type in Notes (rather than serializing
/// a widget enum) makes it a durable, replayable action payload for peers and
/// agents alike.
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
}

/// Authored once before dispatch; replay never mints page identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicatePage {
    pub source: String,
    pub destination: String,
}

/// Build a complete deterministic copy mapping from the current source tree.
///
/// # Errors
/// Refuses a missing source or invalid notebook.
pub fn duplicate_subtree_action(doc: &AutoCommit, source: PageId) -> Result<NotesAction, String> {
    let workspace = crate::notebook::project(doc, None)?;
    duplicate_subtree_from_workspace(source, &workspace)
}

fn duplicate_subtree_from_workspace(
    source: PageId,
    workspace: &crate::Workspace,
) -> Result<NotesAction, String> {
    Ok(author_duplicate(
        source,
        &duplicate_sources(source, workspace)?,
    ))
}

// Projection captures only existing stable identities. Destination UUIDs are
// authored once by the final admitted builder, never by resource discovery.
fn duplicate_sources(source: PageId, workspace: &crate::Workspace) -> Result<Vec<PageId>, String> {
    let _profile = ui::tilo_resource::profiling::Phase::start("duplicate_sources");
    if workspace.get(source).is_none() {
        return Err("source note no longer exists".into());
    }
    let mut subtree = HashSet::new();
    let mut pending = vec![source];
    while let Some(page) = pending.pop() {
        if subtree.insert(page) {
            pending.extend(
                workspace
                    .pages()
                    .iter()
                    .filter(|p| p.parent == Some(page))
                    .map(|p| p.id),
            );
        }
    }
    let mut sources = subtree.into_iter().collect::<Vec<_>>();
    sources.sort();
    Ok(sources)
}

fn author_duplicate(source: PageId, sources: &[PageId]) -> NotesAction {
    NotesAction::DuplicateSubtree {
        source: source.to_string(),
        pages: sources
            .iter()
            .map(|page| DuplicatePage {
                source: page.to_string(),
                destination: Uuid::new_v4().to_string(),
            })
            .collect(),
    }
}

#[derive(Clone, Copy)]
enum PageTreeCommand {
    MoveUp,
    MoveDown,
    Indent,
    Outdent,
}

fn page_tree_action(
    workspace: &crate::Workspace,
    page: PageId,
    command: PageTreeCommand,
) -> Option<NotesAction> {
    let _profile = ui::tilo_resource::profiling::Phase::start("page_tree_action");
    let mut workspace = workspace.clone();
    let mut before = workspace
        .pages()
        .iter()
        .map(PageData::from_meta)
        .collect::<Vec<_>>();
    before.sort_by(|a, b| a.id.cmp(&b.id));
    let changed = move_page_tree(&mut workspace, page, command)?;
    if !changed {
        return None;
    }
    let mut after = workspace
        .pages()
        .iter()
        .map(PageData::from_meta)
        .collect::<Vec<_>>();
    after.sort_by(|a, b| a.id.cmp(&b.id));
    Some(NotesAction::UpdatePageTree { before, after })
}

/// Durable, browseable history metadata for a Notes mutation.
pub fn history_context(action: &NotesAction) -> tilo_actions::ActionContext {
    let mut context = tilo_actions::ActionContext::for_action("so.tilo.notes", "Notes", action);
    let (label, resource_id, kind, title) = match action {
        NotesAction::UpdatePageTree { .. } => (
            "Move note",
            "notes.notebook".into(),
            "notes.notebook",
            "Notes".into(),
        ),
        NotesAction::UpsertPage(page) => (
            "Update note",
            format!("notes.page:{}", page.id),
            "notes.page",
            page.title.clone(),
        ),
        NotesAction::AppendPage { id, title, .. } => (
            "New note",
            format!("notes.page:{id}"),
            "notes.page",
            title.clone(),
        ),
        NotesAction::ImportMarkdown { page, title, .. } => (
            "Import Markdown",
            format!("notes.page:{page}"),
            "notes.page",
            title.clone(),
        ),
        NotesAction::DuplicateSubtree { source, .. } => (
            "Duplicate note",
            format!("notes.page:{source}"),
            "notes.page",
            "Note".into(),
        ),
        NotesAction::DeletePage { id } => (
            "Delete note",
            format!("notes.page:{id}"),
            "notes.page",
            "Note".to_string(),
        ),
        NotesAction::EditBody { page, .. } | NotesAction::WriteBody { page, .. } => (
            "Edit note",
            format!("notes.page:{page}"),
            "notes.page",
            "Note".to_string(),
        ),
        NotesAction::AddImageAsset { page, .. } => (
            "Insert image",
            format!("notes.page:{page}"),
            "notes.page",
            "Note".to_string(),
        ),
        NotesAction::UpsertTag(tag) => (
            "Update tag",
            format!("notes.tag:{}", tag.id),
            "notes.tag",
            tag.name.clone(),
        ),
        NotesAction::DeleteTag { id } => (
            "Delete tag",
            format!("notes.tag:{id}"),
            "notes.tag",
            "Tag".to_string(),
        ),
        NotesAction::SetTagAssignment {
            page,
            tag,
            assigned,
        } => (
            if *assigned {
                "Assign tag"
            } else {
                "Unassign tag"
            },
            format!("notes.page:{page}"),
            "notes.page",
            format!("Tag {tag}"),
        ),
        NotesAction::CreateAndAssignTag { page, tag } => (
            "Create and assign tag",
            format!("notes.page:{page}"),
            "notes.page",
            tag.name.clone(),
        ),
    };
    context.action_label = label.to_string();
    context.resource = Some(tilo_actions::ActionResource {
        id: resource_id,
        kind: kind.to_string(),
        label: title,
    });
    context
}

fn move_page_tree(
    workspace: &mut crate::Workspace,
    page: PageId,
    command: PageTreeCommand,
) -> Option<bool> {
    let plan = page_tree_plan(workspace, page, command)?;
    workspace.apply_page_move_plan(plan);
    Some(true)
}

fn page_tree_plan(
    workspace: &crate::Workspace,
    page: PageId,
    command: PageTreeCommand,
) -> Option<Vec<PageMeta>> {
    match command {
        PageTreeCommand::MoveUp => workspace.plan_move_sibling(page, -1),
        PageTreeCommand::MoveDown => workspace.plan_move_sibling(page, 1),
        PageTreeCommand::Indent => {
            let current = workspace.get(page)?;
            let siblings = workspace.children_of(current.parent);
            let index = siblings.iter().position(|candidate| candidate.id == page)?;
            let previous = siblings.get(index.checked_sub(1)?)?.id;
            workspace.plan_move_to(page, Some(previous), None)
        }
        PageTreeCommand::Outdent => {
            let current = workspace.get(page)?;
            let parent = workspace.get(current.parent?)?;
            let before = workspace
                .children_of(parent.parent)
                .iter()
                .skip_while(|candidate| candidate.id != parent.id)
                .nth(1)
                .map(|candidate| candidate.id);
            workspace.plan_move_to(page, parent.parent, before)
        }
    }
}

/// Rewrite the display text of every UUID-backed reference after its target is
/// renamed. Identity remains the UUID; this only prevents stale labels.
fn rewrite_page_reference_labels(
    doc: &mut AutoCommit,
    target: PageId,
    new_title: &str,
) -> Result<(), String> {
    let chars = |text: &str, byte: usize| text.get(..byte).map_or(0, |head| head.chars().count());
    for page in NotesDoc::list_pages(doc) {
        for block in NotesDoc::page_blocks(doc, page.id) {
            let mut ranges = block
                .marks
                .iter()
                .filter_map(|span| {
                    let (Mark::Link(linked) | Mark::Transclusion(linked)) = &span.mark else {
                        return None;
                    };
                    (*linked == target).then(|| (span.range.clone(), span.mark.clone()))
                })
                .filter(|(range, _)| block.text.get(range.clone()) != Some(new_title))
                .collect::<Vec<_>>();
            // Last first, so each edit leaves the earlier ranges where they are.
            ranges.sort_unstable_by_key(|(range, _)| std::cmp::Reverse(range.start));
            for (range, mark) in ranges {
                let at = chars(&block.text, range.start);
                let del = block
                    .text
                    .get(range.clone())
                    .map_or(0, |old| old.chars().count());
                NotesDoc::edit_page_body(
                    doc,
                    page.id,
                    &RichTextAction::SpliceText {
                        region: RegionKind::Body,
                        block: block.id.clone(),
                        at,
                        del,
                        ins: new_title.to_string(),
                    },
                )?;
                let NotesAction::EditBody { edit, .. } = set_mark_action(
                    page.id,
                    &block.id,
                    at..at.saturating_add(new_title.chars().count()),
                    &mark,
                ) else {
                    continue;
                };
                NotesDoc::edit_page_body(doc, page.id, &edit)?;
            }
        }
    }
    Ok(())
}

/// The most Markdown one import takes: 1 MiB of text in at most 10,000
/// blocks.
pub const MAX_MARKDOWN_IMPORT_BYTES: usize = 1024 * 1024;
pub const MAX_MARKDOWN_IMPORT_BLOCKS: usize = 10_000;

/// Whether `markdown` is small enough for one import.
///
/// # Errors
/// Says which limit it exceeds.
pub fn check_markdown_import(markdown: &str) -> Result<(), String> {
    if markdown.len() > MAX_MARKDOWN_IMPORT_BYTES {
        return Err("Markdown content is larger than 1 MiB".into());
    }
    if crate::markdown::parse(markdown).len() > MAX_MARKDOWN_IMPORT_BLOCKS {
        return Err("Markdown content contains more than 10,000 blocks".into());
    }
    Ok(())
}

fn import_markdown(
    doc: &mut Doc,
    page: &str,
    title: &str,
    markdown: &str,
    assets: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let id = page
        .parse::<PageId>()
        .map_err(|_| "invalid imported page identity")?;
    let pages = NotesDoc::list_pages(doc);
    if id.is_nil() || pages.iter().any(|existing| existing.id == id) {
        return Err("import requires a new root page".into());
    }
    check_markdown_import(markdown)?;
    let title = match title.trim() {
        "" => "Imported note",
        title => title,
    };
    let order = crate::model::key_after(pages.iter().filter(|existing| existing.parent.is_none()));
    for (digest, bytes) in assets {
        crate::notebook::put_asset(doc, digest, bytes)?;
    }
    let blocks = crate::MainPanel::seed_blocks(markdown, |target| {
        if target == title {
            return Some(id);
        }
        pages
            .iter()
            .find(|candidate| candidate.title == target)
            .map(|candidate| candidate.id)
    });
    NotesDoc::upsert_page_meta(
        doc,
        &PageMeta {
            id,
            title: title.to_owned(),
            parent: None,
            order,
        },
    );
    NotesDoc::fill_page_body(doc, id, &blocks)
}

/// Import `markdown` as a new note titled `title`.
pub fn import_markdown_action(
    page: PageId,
    title: &str,
    markdown: &str,
    assets: Vec<(String, Vec<u8>)>,
) -> NotesAction {
    NotesAction::ImportMarkdown {
        page: page.to_string(),
        title: title.to_owned(),
        markdown: markdown.to_owned(),
        assets,
    }
}

impl Action for NotesAction {
    #[allow(
        clippy::too_many_lines,
        reason = "the canonical action dispatcher keeps every durable Notes mutation in one exhaustive replay boundary"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            NotesAction::ImportMarkdown {
                page,
                title,
                markdown,
                assets,
            } => {
                import_markdown(doc, page, title, markdown, assets)
                    .map_err(ActionError::Refused)?;
            }
            NotesAction::UpdatePageTree { before, after } => {
                update_page_tree(doc, before, after).map_err(ActionError::Refused)?;
            }
            NotesAction::UpsertPage(data) => {
                if let Some(meta) = data.to_meta() {
                    let previous = NotesDoc::list_pages(doc)
                        .into_iter()
                        .find(|page| page.id == meta.id);
                    NotesDoc::upsert_page_meta(doc, &meta);
                    if previous
                        .as_ref()
                        .is_some_and(|page| page.title != meta.title)
                    {
                        rewrite_page_reference_labels(doc, meta.id, &meta.title)
                            .map_err(ActionError::Refused)?;
                    }
                }
            }
            NotesAction::AppendPage { id, title, parent } => {
                let (Ok(id), Ok(parent)) = (
                    id.parse::<PageId>(),
                    (!parent.is_empty())
                        .then(|| parent.parse::<PageId>())
                        .transpose(),
                ) else {
                    return Ok(());
                };
                let pages = NotesDoc::list_pages(doc);
                if pages.iter().any(|page| page.id == id) {
                    return Err(ActionError::Refused("this note already exists".into()));
                }
                if parent.is_some_and(|parent| pages.iter().all(|page| page.id != parent)) {
                    return Err(ActionError::Refused("the parent note is gone".into()));
                }
                let order =
                    crate::model::key_after(pages.iter().filter(|page| page.parent == parent));
                NotesDoc::upsert_page_meta(
                    doc,
                    &PageMeta {
                        id,
                        title: title.clone(),
                        parent,
                        order,
                    },
                );
            }
            NotesAction::DuplicateSubtree { source, pages } => {
                let source = source
                    .parse::<PageId>()
                    .map_err(|_| ActionError::Refused("note id is invalid".into()))?;
                NotesDoc::duplicate_subtree(doc, source, pages).map_err(ActionError::Refused)?;
            }
            NotesAction::DeletePage { id } => {
                let Ok(root) = id.parse::<PageId>() else {
                    return Ok(());
                };
                let all = NotesDoc::list_pages(doc);
                // Collect `root` and every descendant into the removal set,
                // walking the document's parent links (its base state).
                let mut removed: HashSet<PageId> = HashSet::new();
                let mut stack = vec![root];
                while let Some(cur) = stack.pop() {
                    if !removed.insert(cur) {
                        continue;
                    }
                    for p in all.iter().filter(|p| p.parent == Some(cur)) {
                        stack.push(p.id);
                    }
                }
                // Delete in a deterministic order (by page id). A `HashSet`'s
                // iteration order is not stable across replicas, so emitting the
                // delete ops in set order would make the change diverge; sorting
                // pins one identical op sequence on every replica.
                let mut removed_sorted: Vec<PageId> = removed.iter().copied().collect();
                removed_sorted.sort();
                for id in &removed_sorted {
                    NotesDoc::remove_page(doc, *id);
                }
            }
            NotesAction::EditBody { page, edit } => {
                let page = page
                    .parse::<PageId>()
                    .map_err(|_| ActionError::Refused("note id is invalid".into()))?;
                NotesDoc::edit_page_body(doc, page, edit).map_err(ActionError::Refused)?;
            }
            NotesAction::WriteBody { page, blocks } => {
                let page = page
                    .parse::<PageId>()
                    .map_err(|_| ActionError::Refused("note id is invalid".into()))?;
                let blocks = blocks
                    .iter()
                    .map(PageBlock::content)
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| ActionError::Refused("a block is invalid".into()))?;
                NotesDoc::fill_page_body(doc, page, &blocks).map_err(ActionError::Refused)?;
            }
            NotesAction::AddImageAsset {
                page,
                digest,
                bytes,
                edit,
            } => {
                crate::notebook::validate_asset_payload(digest, bytes)
                    .map_err(ActionError::Refused)?;
                let page = page
                    .parse::<PageId>()
                    .map_err(|_| ActionError::Refused("note id is invalid".into()))?;
                NotesDoc::edit_page_body(doc, page, edit).map_err(ActionError::Refused)?;
                crate::notebook::put_asset(doc, digest, bytes).map_err(ActionError::Refused)?;
            }
            NotesAction::UpsertTag(data) => {
                if let Some(tag) = data.to_meta() {
                    NotesDoc::upsert_tag(doc, &tag);
                }
            }
            NotesAction::DeleteTag { id } => {
                if let Ok(id) = id.parse::<TagId>() {
                    NotesDoc::delete_tag(doc, id);
                }
            }
            NotesAction::SetTagAssignment {
                page,
                tag,
                assigned,
            } => {
                let (Ok(page), Ok(tag)) = (page.parse::<PageId>(), tag.parse::<TagId>()) else {
                    return Ok(());
                };
                // A deleted endpoint wins over a stale assignment. The action
                // remains a harmless no-op instead of making a dangling edge.
                if NotesDoc::list_pages(doc)
                    .iter()
                    .any(|candidate| candidate.id == page)
                    && NotesDoc::list_tags(doc)
                        .iter()
                        .any(|candidate| candidate.id == tag)
                {
                    NotesDoc::set_tag_assignment(doc, page, tag, *assigned);
                }
            }
            NotesAction::CreateAndAssignTag { page, tag } => {
                let Ok(page) = page.parse::<PageId>() else {
                    return Ok(());
                };
                let Some(tag) = tag.to_meta() else {
                    return Ok(());
                };
                if NotesDoc::list_pages(doc)
                    .iter()
                    .any(|candidate| candidate.id == page)
                {
                    NotesDoc::upsert_tag(doc, &tag);
                    NotesDoc::set_tag_assignment(doc, page, tag.id, true);
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Notes' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<NotesAction>(NOTES_ACTION_TYPE);
}

/// Encode any [`NotesAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Notes action into a
/// replayable envelope.
///
/// # Panics
///
/// Panics if `action` fails to serialize. [`NotesAction`] is a plain,
/// non-recursive enum of owned strings and integers, so this cannot happen
/// for any value this module constructs.
#[allow(
    clippy::expect_used,
    reason = "NotesAction serializes with plain serde_json over owned strings/ints; encoding cannot fail"
)]
pub fn encode(action: &NotesAction) -> EncodedAction {
    EncodedAction::encode(NOTES_ACTION_TYPE, action).expect("NotesAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Create or replace a page (a new page, a rename, a re-parent, or a reorder).
pub fn upsert_page_action(meta: &PageMeta) -> NotesAction {
    NotesAction::UpsertPage(PageData::from_meta(meta))
}

/// Rename `meta`'s page, carrying the rest of its metadata unchanged so the
/// write only diff-splices the title.
pub fn rename_page_action(meta: &PageMeta, new_title: &str) -> NotesAction {
    let mut d = PageData::from_meta(meta);
    d.title = new_title.to_string();
    NotesAction::UpsertPage(d)
}

/// Create a child page under `parent` (or a root page when `parent` is `None`)
/// with a freshly-minted `id`, at the fractional key `order`.
pub fn new_page_action(
    id: PageId,
    parent: Option<PageId>,
    order: String,
    title: &str,
) -> NotesAction {
    NotesAction::UpsertPage(PageData {
        id: id.to_string(),
        title: title.to_string(),
        parent: parent.map(|p| p.to_string()).unwrap_or_default(),
        order,
    })
}

/// Create a page after `parent`'s children as they are when it applies.
pub fn append_page_action(id: PageId, parent: Option<PageId>, title: &str) -> NotesAction {
    NotesAction::AppendPage {
        id: id.to_string(),
        title: title.to_string(),
        parent: parent.map(|p| p.to_string()).unwrap_or_default(),
    }
}

/// Delete a page and its whole subtree.
pub fn delete_page_action(id: PageId) -> NotesAction {
    NotesAction::DeletePage { id: id.to_string() }
}

/// One rich-text edit of a page's body. Block ids and offsets are carried in
/// the payload, so a stale edit refuses rather than silently changing a
/// neighbour after a concurrent insert or reorder.
pub fn edit_body_action(page: PageId, edit: RichTextAction) -> NotesAction {
    NotesAction::EditBody {
        page: page.to_string(),
        edit,
    }
}

/// Mark the characters `range` of `block` with `mark`: a format, a page link.
pub fn set_mark_action(
    page: PageId,
    block: &str,
    range: std::ops::Range<usize>,
    mark: &Mark,
) -> NotesAction {
    let (name, value) = tilo_rich_text::document_mark(mark);
    edit_body_action(
        page,
        RichTextAction::SetMark {
            region: RegionKind::Body,
            spans: vec![tilo_document::action::Span {
                block: block.to_owned(),
                start: range.start,
                end: range.end,
            }],
            name: name.to_owned(),
            value: Some(value),
            also_clear: Vec::new(),
        },
    )
}

/// Write a page's first content into its empty body.
pub fn write_body_action(page: PageId, blocks: &[BlockContent]) -> NotesAction {
    NotesAction::WriteBody {
        page: page.to_string(),
        blocks: blocks.iter().map(PageBlock::of).collect(),
    }
}

pub fn add_image_asset_action(page: PageId, bytes: Vec<u8>, edit: RichTextAction) -> NotesAction {
    let digest = blake3::hash(&bytes).to_hex().to_string();
    NotesAction::AddImageAsset {
        page: page.to_string(),
        digest,
        bytes,
        edit,
    }
}

pub fn upsert_tag_action(tag: &TagMeta) -> NotesAction {
    NotesAction::UpsertTag(TagData::from_meta(tag))
}

pub fn new_tag_action(id: TagId, name: &str) -> NotesAction {
    NotesAction::UpsertTag(TagData {
        id: id.to_string(),
        name: name.trim().to_string(),
    })
}

pub fn rename_tag_action(tag: &TagMeta, name: &str) -> NotesAction {
    NotesAction::UpsertTag(TagData {
        id: tag.id.to_string(),
        name: name.trim().to_string(),
    })
}

pub fn delete_tag_action(id: TagId) -> NotesAction {
    NotesAction::DeleteTag { id: id.to_string() }
}

pub fn set_tag_assignment_action(page: PageId, tag: TagId, assigned: bool) -> NotesAction {
    NotesAction::SetTagAssignment {
        page: page.to_string(),
        tag: tag.to_string(),
        assigned,
    }
}

pub fn create_and_assign_tag_action(page: PageId, tag: TagId, name: &str) -> NotesAction {
    NotesAction::CreateAndAssignTag {
        page: page.to_string(),
        tag: TagData {
            id: tag.to_string(),
            name: name.trim().to_string(),
        },
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context Notes hands [`provide_resources`]: the page tree it
/// currently has open. Everything here is derived from state the document
/// already holds (the workspace mirrors the shared `pages` map); nothing is
/// fabricated.
#[derive(Debug, Clone)]
pub struct NotesResourceCtx {
    pub pages: Vec<PageMeta>,
    pub blocks: std::collections::HashMap<PageId, Vec<NoteBlockResource>>,
    pub tags: Vec<TagMeta>,
    pub tag_assignments: Vec<(PageId, TagId)>,
}

impl NotesResourceCtx {
    /// The context of the notebook `workspace`.
    pub fn of(workspace: &crate::Workspace) -> Self {
        Self {
            pages: workspace.pages().to_vec(),
            blocks: HashMap::new(),
            tags: workspace.tags().to_vec(),
            tag_assignments: workspace
                .data
                .tag_assignments
                .iter()
                .map(|assignment| (assignment.page, assignment.tag))
                .collect(),
        }
    }

    /// The notebook this context describes, whose notes say `bodies`.
    fn workspace(&self, bodies: Bodies) -> crate::Workspace {
        crate::Workspace {
            data: crate::model::WorkspaceData {
                pages: self.pages.clone(),
                selected: None,
                tags: self.tags.clone(),
                tag_assignments: self
                    .tag_assignments
                    .iter()
                    .map(|(page, tag)| crate::model::TagAssignment {
                        page: *page,
                        tag: *tag,
                    })
                    .collect(),
            },
            bodies: std::sync::Arc::new(bodies),
            ..crate::Workspace::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteBlockResource {
    pub id: String,
    pub text: String,
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

/// What every note says, by page.
pub type Bodies = HashMap<PageId, Vec<StoredBlock>>;

/// The bodies of `pages`, read from the notebook's document.
pub fn read_bodies(doc: &AutoCommit, pages: &[PageMeta]) -> Bodies {
    pages
        .iter()
        .map(|page| (page.id, NotesDoc::page_stored_blocks(doc, page.id)))
        .collect()
}

/// Each note's body as rich text, read where the notebook keeps it.
pub fn read_body_states(
    doc: &AutoCommit,
    pages: &[PageMeta],
) -> HashMap<PageId, tilo_document::RichTextState> {
    pages
        .iter()
        .filter_map(|page| {
            let body = NotesDoc::page_body(doc, page.id)?;
            RichText::read_under(doc, &body)
                .ok()
                .map(|state| (page.id, state))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteBacklinkResource {
    pub target: PageId,
    pub source: PageId,
    pub block_id: String,
    pub source_title: String,
    pub excerpt: String,
}

/// A one-line label for a page in the palette.
fn page_label(m: &PageMeta) -> String {
    let t = m.title.trim();
    if t.is_empty() {
        "Untitled".to_string()
    } else {
        t.to_string()
    }
}

#[allow(
    clippy::expect_used,
    reason = "wraps encode(), which cannot fail for NotesAction — see its doc comment"
)]
fn immediate_spec(id: &str, label: &str, action: &NotesAction) -> ActionSpec {
    let spec = ActionSpec::immediate(id, label, NOTES_ACTION_TYPE, action)
        .expect("encode notes action")
        .reversible();
    if id.contains("delete") {
        spec.destructive()
    } else {
        spec
    }
}

fn page_editor_view() -> ResourceViewSpec {
    ResourceViewSpec::new(PAGE_EDITOR_VIEW_ID, ui::i18n!("notes-view-editor"), "open")
        .with_primary(true)
}

fn decorate_external_link_actions(resource: &mut Resource, text: &str, marks: &[InlineMarkSpan]) {
    for mark in marks {
        let InlineMark::ExternalLink { url } = &mark.mark else {
            continue;
        };
        if !crate::markdown::safe_external_url(url) {
            continue;
        }
        let label = text
            .get(mark.start..mark.end)
            .unwrap_or("external link")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let digest = blake3::hash(format!("{}:{}:{url}", mark.start, mark.end).as_bytes());
        let hex = digest.to_hex();
        let suffix = hex.as_str().get(..12).unwrap_or(hex.as_str());
        let id = format!("open-external-{suffix}");
        resource.actions.push(
            NotesEffect::OpenLink { url: url.clone() }.offered(id, format!("Open link: {label}")),
        );
    }
}

/// Resource snapshots are intentionally bounded: agents and the action palette
/// receive useful nearby context, then resolve a stable page id lazily through
/// [`ResourceProvider::resource`] when they need a deeper or later sibling.
const RESOURCE_PAGE_SNAPSHOT_LIMIT: usize = 128;
const RESOURCE_PAGE_SNAPSHOT_DEPTH: usize = 6;
/// Keep resource materialization finite even when a shared workspace has a
/// machine-generated tag vocabulary. Every omitted tag remains addressable by
/// stable id through `ResourceProvider::resource`.
const RESOURCE_TAG_SNAPSHOT_LIMIT: usize = 128;
/// A tag can be attached to an unbounded number of notes. Its contextual
/// children are only a preview; exact pages resolve independently by id.
const RESOURCE_TAG_PAGE_SNAPSHOT_LIMIT: usize = 64;
/// Page resources expose a useful nearby set of one-click membership actions,
/// while the tag resource remains the scalable path for larger vocabularies.
const RESOURCE_PAGE_TAG_ACTION_LIMIT: usize = 64;
const RESOURCE_BACKLINK_SNAPSHOT_LIMIT: usize = 64;

pub fn canonical_backlinks(
    bodies: &Bodies,
    pages: &[PageMeta],
    target: PageId,
) -> Vec<NoteBacklinkResource> {
    canonical_backlink_index(bodies, pages)
        .remove(&target)
        .unwrap_or_default()
}

fn canonical_backlink_index(
    bodies: &Bodies,
    pages: &[PageMeta],
) -> HashMap<PageId, Vec<NoteBacklinkResource>> {
    let existing = pages.iter().map(|page| page.id).collect::<HashSet<_>>();
    let mut by_target = HashMap::<PageId, Vec<NoteBacklinkResource>>::new();
    for source in pages {
        for block in bodies.get(&source.id).map_or(&[][..], Vec::as_slice) {
            let block_id = &block.id;
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
                by_target
                    .entry(target)
                    .or_default()
                    .push(NoteBacklinkResource {
                        target,
                        source: source.id,
                        block_id: block_id.clone(),
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

fn backlink_collection_resource(
    bodies: &Bodies,
    pages: &[PageMeta],
    target: PageId,
) -> Option<Resource> {
    let backlinks = canonical_backlinks(bodies, pages, target);
    if backlinks.is_empty() {
        return None;
    }
    let mut collection = Resource::new(
        backlinks_resource_id(target),
        "notes.backlinks",
        "Linked from",
    );
    for backlink in backlinks.iter().take(RESOURCE_BACKLINK_SNAPSHOT_LIMIT) {
        collection = collection.with_child(backlink_resource(backlink));
    }
    Some(collection)
}

fn backlink_resource(backlink: &NoteBacklinkResource) -> Resource {
    let label = if backlink.excerpt.is_empty() {
        backlink.source_title.clone()
    } else {
        format!("{} — {}", backlink.source_title, backlink.excerpt)
    };
    let resource = Resource::new(
        backlink_resource_id(backlink.target, backlink.source, &backlink.block_id),
        "notes.backlink",
        label,
    );
    let open = NotesEffect::Open {
        page: backlink.source,
        block: Some(backlink.block_id.clone()),
    }
    .offered("open", "Open source block");
    resource.with_action(open)
}

fn decorate_backlinks(resource: &mut Resource, targets: &HashSet<PageId>) {
    if let Some(page) = resource
        .id
        .as_str()
        .strip_prefix("notes.page:")
        .and_then(|id| id.parse::<PageId>().ok())
        && targets.contains(&page)
    {
        resource.children.push(Resource::new(
            backlinks_resource_id(page),
            "notes.backlinks",
            "Linked from",
        ));
    }
    for child in &mut resource.children {
        decorate_backlinks(child, targets);
    }
}

#[derive(Serialize)]
enum TreeAction<'a> {
    UpdatePageTree {
        before: &'a serde_json::value::RawValue,
        after: Vec<&'a serde_json::value::RawValue>,
    },
}

/// Immutable metadata encoding shared only within this resource publication.
/// Every command still carries the complete original preimage and postimage.
struct PageTreeSnapshot<'a> {
    pages: Vec<&'a PageMeta>,
    encoded: Vec<Box<serde_json::value::RawValue>>,
    before: Box<serde_json::value::RawValue>,
    positions: HashMap<PageId, usize>,
}

impl<'a> PageTreeSnapshot<'a> {
    fn new(pages: &'a [PageMeta]) -> Option<Self> {
        let mut pages = pages.iter().collect::<Vec<_>>();
        pages.sort_by_key(|page| page.id);
        let encoded = pages
            .iter()
            .map(|page| serde_json::value::to_raw_value(&PageData::from_meta(page)))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let before = serde_json::value::to_raw_value(&encoded).ok()?;
        let positions = pages
            .iter()
            .enumerate()
            .map(|(index, page)| (page.id, index))
            .collect();
        Some(Self {
            pages,
            encoded,
            before,
            positions,
        })
    }

    fn spec(
        &self,
        workspace: &crate::Workspace,
        page: PageId,
        command: PageTreeCommand,
        id: &str,
        label: &str,
    ) -> Option<ActionSpec> {
        if self.positions.len() != self.pages.len() {
            return self.dense_spec(workspace, page, command, id, label);
        }
        let _profile = ui::tilo_resource::profiling::Phase::start("page_tree_action");
        let updates = {
            let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_plan");
            page_tree_plan(workspace, page, command)?
        };
        let changed = {
            let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_delta");
            updates
                .iter()
                .filter_map(|after| {
                    let index = *self.positions.get(&after.id)?;
                    (self.pages.get(index).copied()? != after).then_some((index, after))
                })
                .map(|(index, after)| {
                    serde_json::value::to_raw_value(&PageData::from_meta(after))
                        .map(|value| (index, value))
                })
                .collect::<Result<HashMap<_, _>, _>>()
                .ok()?
        };
        let after = self
            .encoded
            .iter()
            .enumerate()
            .map(|(index, original)| changed.get(&index).unwrap_or(original).as_ref())
            .collect();
        let encoded = {
            let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_encode");
            EncodedAction::encode_operation(
                NOTES_ACTION_TYPE,
                &TreeAction::UpdatePageTree {
                    before: &self.before,
                    after,
                },
            )
            .ok()?
        };
        let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_spec");
        Some(
            ActionSpec::immediate_encoded(id, label, ExecutionLocus::ClientLocal, encoded)
                .reversible(),
        )
    }

    fn dense_spec(
        &self,
        workspace: &crate::Workspace,
        page: PageId,
        command: PageTreeCommand,
        id: &str,
        label: &str,
    ) -> Option<ActionSpec> {
        let _profile = ui::tilo_resource::profiling::Phase::start("page_tree_action");
        // Tree operations only read and mutate metadata. Rich-text bodies and
        // tag assignments never participate in authoring a tree operation.
        let mut moved = {
            let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_clone");
            let mut moved = crate::Workspace::default();
            moved.data.pages = workspace.pages().to_vec();
            moved
        };
        let changed = {
            let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_move");
            move_page_tree(&mut moved, page, command)?
        };
        if !changed {
            return None;
        }
        let moved_pages = {
            let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_index");
            moved
                .pages()
                .iter()
                .map(|page| (page.id, page))
                .collect::<HashMap<_, _>>()
        };
        let changed = {
            let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_delta");
            self.pages
                .iter()
                .enumerate()
                .filter_map(|(index, original)| {
                    let after = *moved_pages.get(&original.id)?;
                    (after != *original).then_some((index, PageData::from_meta(after)))
                })
                .map(|(index, page)| {
                    serde_json::value::to_raw_value(&page).map(|value| (index, value))
                })
                .collect::<Result<HashMap<_, _>, _>>()
                .ok()?
        };
        let after = self
            .encoded
            .iter()
            .enumerate()
            .map(|(index, before)| changed.get(&index).unwrap_or(before).as_ref())
            .collect::<Vec<_>>();
        let encoded = {
            let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_encode");
            EncodedAction::encode_operation(
                NOTES_ACTION_TYPE,
                &TreeAction::UpdatePageTree {
                    before: &self.before,
                    after,
                },
            )
            .ok()?
        };
        let _profile = ui::tilo_resource::profiling::Phase::start("tree_metadata_spec");
        Some(
            ActionSpec::immediate_encoded(id, label, ExecutionLocus::ClientLocal, encoded)
                .reversible(),
        )
    }
}

/// Index a durable page list once before building its contextual resource
/// snapshot. This avoids the old recursive `pages.iter().filter(...)` scan at
/// every node (quadratic for a wide notebook).
struct PageResourceIndex<'a> {
    children: HashMap<Option<PageId>, Vec<&'a PageMeta>>,
    tree: Option<PageTreeSnapshot<'a>>,
}

impl<'a> PageResourceIndex<'a> {
    fn new(pages: &'a [PageMeta], tree_pages: &'a [PageMeta]) -> Self {
        let mut children: HashMap<Option<PageId>, Vec<&PageMeta>> = HashMap::new();
        for page in pages {
            children.entry(page.parent).or_default().push(page);
        }
        for siblings in children.values_mut() {
            siblings.sort_by(|a, b| crate::model::sibling_order(a, b));
        }
        Self {
            children,
            tree: PageTreeSnapshot::new(tree_pages),
        }
    }

    fn children_of(&self, parent: Option<PageId>) -> &[&'a PageMeta] {
        self.children.get(&parent).map_or(&[], Vec::as_slice)
    }
}

/// Build one page resource and a bounded contextual slice of its descendants.
#[cfg(test)]
pub(crate) fn page_resource(m: &PageMeta, pages: &[PageMeta], doc: &AutoCommit) -> Resource {
    let workspace = crate::notebook::project(doc, None).ok();
    page_resource_in(m, pages, workspace.as_ref(), &read_bodies(doc, pages))
}

/// [`page_resource`] in the notebook `workspace`, whose notes say `bodies`.
pub(crate) fn page_resource_in(
    m: &PageMeta,
    pages: &[PageMeta],
    workspace: Option<&crate::Workspace>,
    bodies: &Bodies,
) -> Resource {
    let index = PageResourceIndex::new(pages, workspace.map_or(pages, crate::Workspace::pages));
    let mut remaining = RESOURCE_PAGE_SNAPSHOT_LIMIT;
    page_resource_from_index(m, &index, bodies, workspace, 0, &mut remaining)
}

/// Root page resources for the notebook — the tree [`Resource::children`] and
/// [`provide_resources`] both publish. Each page nests its rich-text body.
pub(crate) fn notebook_page_children(
    workspace: &crate::Workspace,
    bodies: &Bodies,
) -> Vec<Resource> {
    let index = PageResourceIndex::new(workspace.pages(), workspace.pages());
    let mut remaining = RESOURCE_PAGE_SNAPSHOT_LIMIT;
    index
        .children_of(None)
        .iter()
        .filter_map(|root| {
            if remaining == 0 {
                return None;
            }
            Some(page_resource_from_index(
                root,
                &index,
                bodies,
                Some(workspace),
                0,
                &mut remaining,
            ))
        })
        .collect()
}

/// The id a page's rich-text body is published and edited under, as a
/// child resource of the notebook.
pub fn page_body_id(page: PageId) -> String {
    ui::tilo_resource::child_resource_id::<crate::resource::Notebook>(&format!("page/{page}/body"))
}

/// A rich-text edit of `page`'s body as the notebook's action.
fn page_body_edit(page: PageId) -> impl Fn(RichTextAction) -> NotesAction + Send + Sync + 'static {
    move |edit| edit_body_action(page, edit)
}

/// The dispatcher `page`'s body is edited through: each edit applies as the
/// notebook's [`NotesAction::EditBody`].
pub(crate) fn page_body_dispatcher(
    notebook: &ui::framework::Dispatcher<crate::resource::Notebook>,
    page: PageId,
) -> ui::framework::Dispatcher<RichText> {
    notebook.child(&page_body_id(page), page_body_edit(page))
}

/// A page's body, published as the rich-text resource it is: its actions
/// apply as the notebook's [`NotesAction::EditBody`] on that page.
fn page_body_child(page: &PageMeta, state: Option<&tilo_document::RichTextState>) -> Resource {
    let _profile = ui::tilo_resource::profiling::Phase::start("page_body_resource");
    let id = page_body_id(page.id);
    let mut body = {
        let _profile = ui::tilo_resource::profiling::Phase::start("page_body_publish");
        ui::tilo_resource::publish_child::<RichText, crate::resource::Notebook>(
            &id,
            &page.title,
            state,
            page_body_edit(page.id),
        )
    };
    {
        let _profile = ui::tilo_resource::profiling::Phase::start("page_body_continuations");
        stamp_published_dynamic_continuations(&mut body, id.as_bytes());
    }
    body
}

#[expect(
    clippy::too_many_lines,
    reason = "the published page-resource action table: one independent spec per action"
)]
fn page_resource_from_index(
    m: &PageMeta,
    index: &PageResourceIndex<'_>,
    bodies: &Bodies,
    workspace: Option<&crate::Workspace>,
    depth: usize,
    remaining: &mut usize,
) -> Resource {
    *remaining = remaining.saturating_sub(1);
    let mut res = Resource::new(page_resource_id(m.id), "notes.page", page_label(m));
    if let Some(detail) = page_preview_detail(bodies, m.id) {
        res = res.with_presentation(ResourcePresentation {
            detail: Some("Note preview".into()),
            badge: None,
            preview: Some(detail),
        });
    }

    res = res
        .with_action(
            NotesEffect::Open {
                page: m.id,
                block: None,
            }
            .offered("open", "Open note"),
        )
        .with_view(page_editor_view())
        .with_action(NotesEffect::CopyLink { page: m.id }.bound());
    if let Some(workspace) = workspace
        && let Ok(sources) = duplicate_sources(m.id, workspace)
    {
        let source = m.id;
        let domain = BuilderDomain::Duplicate {
            source,
            pages: sources.clone(),
        };
        res = res.with_action(
            ActionSpec::deferred(
                "duplicate",
                "Duplicate note",
                ExecutionLocus::ClientLocal,
                NOTES_ACTION_TYPE,
                move || encode(&author_duplicate(source, &sources)),
            )
            .reversible()
            .with_continuation_identity(continuation::builder(domain)),
        );
    }
    {
        let _profile = ui::tilo_resource::profiling::Phase::start("page_tree_commands");
        for (id, label, command) in [
            ("move-up", "Move note up", PageTreeCommand::MoveUp),
            ("move-down", "Move note down", PageTreeCommand::MoveDown),
            ("indent", "Indent note", PageTreeCommand::Indent),
            ("outdent", "Outdent note", PageTreeCommand::Outdent),
        ] {
            if let Some(workspace) = workspace {
                let spec = match &index.tree {
                    Some(tree) => tree.spec(workspace, m.id, command, id, label),
                    None => page_tree_action(workspace, m.id, command)
                        .map(|action| immediate_spec(id, label, &action)),
                };
                if let Some(spec) = spec {
                    res = res.with_action(spec);
                }
            }
        }
    }
    res = res.with_action(NotesEffect::ExportMarkdown { page: m.id }.bound());
    res = res.with_action(NotesEffect::InsertImage { page: m.id }.bound());

    // Rename — a prompted action carrying the rest of the page's metadata
    // unchanged so only the title diff-splices.
    let meta = m.clone();
    res = res.with_action(
        ActionSpec::prompted(
            "rename",
            "Rename note",
            "Title",
            "note title…",
            ExecutionLocus::ClientLocal,
            move |title: &str| encode(&rename_page_action(&meta, title.trim())),
        )
        .with_continuation_identity(continuation::builder(BuilderDomain::RenamePage(m.clone()))),
    );

    // Delete the whole page (and its subtree).
    res = res.with_action(immediate_spec(
        "delete",
        "Delete note",
        &delete_page_action(m.id),
    ));

    // New child — prompted; the child id is minted once at commit time and the
    // append position is resolved from the current tree.
    let parent = m.id;
    res = res.with_action(
        ActionSpec::prompted(
            "new-child",
            "New child note",
            "Title",
            "note title…",
            ExecutionLocus::ClientLocal,
            move |title: &str| {
                let t = title.trim();
                let title = if t.is_empty() { "Untitled" } else { t };
                encode(&append_page_action(Uuid::new_v4(), Some(parent), title))
            },
        )
        .with_continuation_identity(continuation::builder(BuilderDomain::CreatePage {
            parent: Some(parent),
        })),
    );

    // Child pages are sorted in the index. Omitted siblings/deeper descendants
    // remain available by their durable id through `resource()`; we never
    // publish an unbounded eager tree.
    if depth < RESOURCE_PAGE_SNAPSHOT_DEPTH {
        for child in index.children_of(Some(m.id)) {
            if *remaining == 0 {
                break;
            }
            res = res.with_child(page_resource_from_index(
                child,
                index,
                bodies,
                workspace,
                depth.saturating_add(1),
                remaining,
            ));
        }
    }
    // Nested rich-text body — same action vocabulary as a root RichText.
    res = res.with_child(page_body_child(
        m,
        workspace.and_then(|workspace| workspace.body_states.get(&m.id)),
    ));
    res
}

fn page_preview_detail(bodies: &Bodies, page: PageId) -> Option<String> {
    let _profile = ui::tilo_resource::profiling::Phase::start("page_preview");
    let text = bodies
        .get(&page)?
        .iter()
        .map(|block| block.text.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|text| !text.is_empty())?;
    let mut end = text.len().min(ResourcePresentation::MAX_DETAIL_BYTES);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut detail = text.get(..end)?.to_string();
    if end < text.len() {
        while detail.len() + '…'.len_utf8() > ResourcePresentation::MAX_DETAIL_BYTES {
            detail.pop();
        }
        detail.push('…');
    }
    Some(detail)
}

#[cfg(test)]
pub(crate) fn block_resource(page: PageId, block: &NoteBlockResource) -> Option<Resource> {
    block_resource_in_page(page, block, std::slice::from_ref(block))
}

fn block_resource_in_page(
    page: PageId,
    block: &NoteBlockResource,
    siblings: &[NoteBlockResource],
) -> Option<Resource> {
    let label = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() {
        return None;
    }
    let open = NotesEffect::Open {
        page,
        block: Some(block.id.clone()),
    }
    .offered("open", "Reveal block");
    let resource_id = block_resource_id(page, &block.id);
    let original = block.text.clone();
    let block_id = block.id.clone();
    let domain = BuilderDomain::ReplaceBlock {
        page,
        block: block_id.clone(),
        original: original.clone(),
    };
    let mut resource = Resource::new(resource_id, "notes.block", label)
        .with_view(page_editor_view().with_range(ResourceViewRangeSpec::new("reveal-block-range")))
        .with_action(open)
        .with_action(
            NotesEffect::RevealBlockRange {
                page,
                block: block.id.clone(),
                start: 0,
                end: 0,
                unit: "Chars".into(),
            }
            .bound_asking(&["start", "end", "unit"]),
        )
        // Agents and the action palette get a truthful, replayable edit
        // affordance too. The expected old text is encoded as a bounded
        // splice; if a collaborator changed it first, apply refuses and
        // the caller reconciles rather than clobbering remote work.
        .with_action(
            ActionSpec::prompted(
                "replace-text",
                "Replace block text",
                "Text",
                "block text…",
                ExecutionLocus::ClientLocal,
                move |text: &str| {
                    encode(&edit_body_action(
                        page,
                        RichTextAction::SpliceText {
                            region: RegionKind::Body,
                            block: block_id.clone(),
                            at: 0,
                            del: original.chars().count(),
                            ins: text.to_string(),
                        },
                    ))
                },
            )
            .with_continuation_identity(continuation::builder(domain)),
        );
    if let Some(index) = siblings
        .iter()
        .position(|candidate| candidate.id == block.id)
    {
        if index > 0 {
            let after = index
                .checked_sub(2)
                .and_then(|predecessor| siblings.get(predecessor))
                .map(|predecessor| predecessor.id.clone());
            resource = resource.with_action(immediate_spec(
                "move-up",
                "Move block up",
                &edit_body_action(
                    page,
                    RichTextAction::MoveBlocks {
                        blocks: vec![block.id.clone()],
                        after,
                    },
                ),
            ));
        }
        if let Some(next) = siblings.get(index.saturating_add(1)) {
            resource = resource.with_action(immediate_spec(
                "move-down",
                "Move block down",
                &edit_body_action(
                    page,
                    RichTextAction::MoveBlocks {
                        blocks: vec![block.id.clone()],
                        after: Some(next.id.clone()),
                    },
                ),
            ));
        }
    }
    resource = resource.with_action(immediate_spec(
        "delete",
        "Delete block",
        &edit_body_action(
            page,
            RichTextAction::RemoveBlocks {
                blocks: vec![block.id.clone()],
            },
        ),
    ));
    Some(resource)
}

/// Build the resource tree Notes currently has open: the notebook (always
/// present, exporting a "New note" action) and its pages, nested by the page
/// tree, each exporting rename / delete / new-child. Every document-mutating
/// operation is exported as the same action the on-screen affordance dispatches.
pub fn provide_resources(ctx: &NotesResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut workspace = ctx.workspace(read_bodies(doc, &ctx.pages));
    workspace.body_states = std::rc::Rc::new(read_body_states(doc, &ctx.pages));
    vec![notebook_resource(&workspace)]
}

/// Attach Draft continuation domains to published dynamic doors that still
/// lack one (`apply` / `group` and rich-text catalogue builders). Fixed
/// encodings and already-stamped specs are left alone.
fn stamp_published_dynamic_continuations(resource: &mut Resource, scope: &[u8]) {
    let _profile = ui::tilo_resource::profiling::Phase::start("continuation_stamp");
    let actions = std::mem::take(&mut resource.actions);
    resource.actions = actions
        .into_iter()
        .map(|spec| {
            if spec.fixed_invocation_encoding().is_some() || spec.has_continuation_identity() {
                return spec;
            }
            let action = spec.id.clone();
            let scope = scope.to_vec();
            spec.with_continuation_identity(continuation::builder(BuilderDomain::PublishedDoor {
                action,
                scope,
            }))
        })
        .collect();
    for child in &mut resource.children {
        let child_scope = child.id.as_str().as_bytes().to_vec();
        stamp_published_dynamic_continuations(child, &child_scope);
    }
}

fn notebook_resource(workspace: &crate::Workspace) -> Resource {
    ui::tilo_resource::publish::<crate::resource::Notebook>("notes.notebook", "Notes", workspace)
}

/// The notebook's children: its notes, nested by the page tree, each with its
/// rich-text body, membership actions for every tag and the notes linking to
/// it; and the tag catalogue.
pub(crate) fn notebook_children(workspace: &crate::Workspace) -> Vec<Resource> {
    let _profile = ui::tilo_resource::profiling::Phase::start("notebook_children");
    let ctx = NotesResourceCtx::of(workspace);
    let mut children = notebook_page_children(workspace, &workspace.bodies);
    children.push(tags_resource(&ctx));
    let backlink_targets = {
        let _profile = ui::tilo_resource::profiling::Phase::start("publication_backlinks");
        canonical_backlink_index(&workspace.bodies, workspace.pages())
            .into_keys()
            .collect::<HashSet<_>>()
    };
    for child in &mut children {
        let _profile = ui::tilo_resource::profiling::Phase::start("page_decoration");
        decorate_tag_actions(child, &ctx);
        decorate_backlinks(child, &backlink_targets);
        let scope = child.id.as_str().as_bytes().to_vec();
        stamp_published_dynamic_continuations(child, &scope);
    }
    children
}

/// The notebook resource `id` beyond the published snapshot: a note deeper
/// or later than [`notebook_children`] carries, a tag, a note's backlinks or
/// one of its blocks. Resolved from the notebook, so a stale id refuses.
pub(crate) fn notebook_child(workspace: &crate::Workspace, id: &str) -> Option<Resource> {
    let ctx = NotesResourceCtx::of(workspace);
    let bodies = workspace.bodies.as_ref();
    if id == "notes.tags" {
        let mut tags = tags_resource(&ctx);
        stamp_published_dynamic_continuations(&mut tags, b"notes.tags");
        return Some(tags);
    }
    if let Some(tag_id) = id.strip_prefix("notes.tag:")
        && let Ok(tag_id) = tag_id.parse::<TagId>()
        && let Some(tag) = ctx.tags.iter().find(|tag| tag.id == tag_id)
    {
        let mut tag = tag_resource(tag, &ctx);
        stamp_published_dynamic_continuations(&mut tag, id.as_bytes());
        return Some(tag);
    }
    if let Some((tag_id, page_id)) = parse_tag_page_resource_id(id) {
        if !ctx.tag_assignments.contains(&(page_id, tag_id)) {
            return None;
        }
        return ctx
            .pages
            .iter()
            .find(|page| page.id == page_id)
            .map(|page| tag_page_resource(tag_id, page));
    }
    if let Some(page_id) = id.strip_prefix("notes.page:")
        && let Ok(page_id) = page_id.parse::<PageId>()
        && let Some(page) = ctx.pages.iter().find(|page| page.id == page_id)
    {
        if let Some(found) = notebook_children(workspace)
            .iter()
            .find_map(|child| find_note(child, id))
        {
            return Some(found);
        }
        let mut resource = page_resource_in(page, &ctx.pages, Some(workspace), bodies);
        decorate_tag_actions(&mut resource, &ctx);
        let backlink_targets = canonical_backlink_index(bodies, &ctx.pages)
            .into_keys()
            .collect::<HashSet<_>>();
        decorate_backlinks(&mut resource, &backlink_targets);
        stamp_published_dynamic_continuations(&mut resource, id.as_bytes());
        return Some(resource);
    }
    if let Some(target) = id.strip_prefix("notes.backlinks:")
        && let Ok(target) = target.parse::<PageId>()
    {
        return backlink_collection_resource(bodies, &ctx.pages, target);
    }
    if let Some((target, source, block_id)) = parse_backlink_resource_id(id) {
        return canonical_backlinks(bodies, &ctx.pages, target)
            .into_iter()
            .find(|backlink| backlink.source == source && backlink.block_id == block_id)
            .map(|backlink| backlink_resource(&backlink));
    }
    let (page, block_id) = parse_block_resource_id(id)?;
    // Blocks are published only when asked for, so the open resource stays
    // bounded even for notebooks with millions of blocks.
    let stored = bodies.get(&page)?;
    let blocks = stored
        .iter()
        .map(|block| NoteBlockResource {
            id: block.id.clone(),
            text: block.text.clone(),
        })
        .collect::<Vec<_>>();
    let block = stored.iter().find(|candidate| candidate.id == block_id)?;
    let mut resource = blocks
        .iter()
        .find(|candidate| candidate.id == block_id)
        .and_then(|candidate| block_resource_in_page(page, candidate, &blocks))?;
    decorate_external_link_actions(&mut resource, &block.text, &block.marks);
    stamp_published_dynamic_continuations(&mut resource, id.as_bytes());
    Some(resource)
}

/// The notebook's state-bound edits: a new root note after the last one, and
/// today's note when the notebook has none yet.
pub(crate) fn notebook_bound_declarations(
    workspace: &crate::Workspace,
) -> Vec<ui::tilo_resource::ActionDecl<crate::resource::Notebook>> {
    let mut declarations = vec![
        ui::tilo_resource::ActionDecl::new(
            "new-page",
            "New note",
            vec![ActionParam::required(
                tilo_actions::PROMPTED_PARAM,
                ParamType::Text,
                "Title",
            )],
            move |args, _| {
                let title = args
                    .text(tilo_actions::PROMPTED_PARAM)
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .unwrap_or("Untitled");
                Ok(append_page_action(Uuid::new_v4(), None, title))
            },
        )
        .describe("Create a new root note."),
    ];
    let title = today_title();
    if !workspace.pages().iter().any(|page| page.title == title) {
        let order = crate::model::key_after(
            workspace
                .pages()
                .iter()
                .filter(|page| page.parent.is_none()),
        );
        declarations.push(
            ui::tilo_resource::ActionDecl::new(
                "daily-note",
                "Create today's note",
                Vec::new(),
                move |_, _| Ok(new_page_action(Uuid::new_v4(), None, order.clone(), &title)),
            )
            .describe("Create today's note, titled with the date."),
        );
    }
    declarations
}

/// Today's note, when the notebook has it: opened from the notebook.
pub(crate) fn notebook_bound_actions(workspace: &crate::Workspace) -> Vec<ActionSpec> {
    let title = today_title();
    workspace
        .pages()
        .iter()
        .find(|page| page.title == title)
        .map(|page| {
            NotesEffect::Open {
                page: page.id,
                block: None,
            }
            .offered("daily-note", "Open today's note")
        })
        .into_iter()
        .collect()
}

/// The title of today's note: the local date.
pub(crate) fn today_title() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub(crate) fn tags_resource(ctx: &NotesResourceCtx) -> Resource {
    // Tags ride the notebook. Assignment edges are exposed as absolute
    // assign/unassign actions on each page, while tag records publish their
    // own stable open/rename/delete identities.
    let mut tags_root = Resource::new("notes.tags", "notes.tags", "Tags");
    tags_root = tags_root.with_action(
        ActionSpec::prompted(
            "new-tag",
            "New tag",
            "Name",
            "tag name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let name = if name.trim().is_empty() {
                    "Untitled tag"
                } else {
                    name
                };
                encode(&new_tag_action(Uuid::new_v4(), name))
            },
        )
        .with_continuation_identity(continuation::builder(BuilderDomain::CreateTag)),
    );
    for tag in ctx.tags.iter().take(RESOURCE_TAG_SNAPSHOT_LIMIT) {
        tags_root = tags_root.with_child(tag_resource(tag, ctx));
    }
    tags_root
}

/// Add membership controls to each published page resource. The action id
/// includes the immutable tag id, so a tag rename never changes an address an
/// agent, history item or accessibility node may already hold.
fn decorate_tag_actions(resource: &mut Resource, ctx: &NotesResourceCtx) {
    if let Some(raw) = resource.id.as_str().strip_prefix("notes.page:")
        && let Ok(page) = raw.parse::<PageId>()
    {
        let assigned = ctx
            .tag_assignments
            .iter()
            .filter(|(candidate, _)| *candidate == page)
            .map(|(_, tag)| *tag)
            .collect::<HashSet<_>>();
        for tag in ctx.tags.iter().take(RESOURCE_PAGE_TAG_ACTION_LIMIT) {
            let is_assigned = assigned.contains(&tag.id);
            let action_id = if is_assigned {
                format!("unassign-tag:{}", tag.id)
            } else {
                format!("assign-tag:{}", tag.id)
            };
            let label = if is_assigned {
                format!("Remove tag {}", tag.name)
            } else {
                format!("Add tag {}", tag.name)
            };
            resource.actions.push(immediate_spec(
                &action_id,
                &label,
                &set_tag_assignment_action(page, tag.id, !is_assigned),
            ));
        }
        resource.actions.push(
            ActionSpec::prompted(
                "new-and-assign-tag",
                "Create and add tag",
                "Name",
                "tag name…",
                ExecutionLocus::ClientLocal,
                move |name: &str| {
                    let name = if name.trim().is_empty() {
                        "Untitled tag"
                    } else {
                        name
                    };
                    encode(&create_and_assign_tag_action(page, Uuid::new_v4(), name))
                },
            )
            .with_continuation_identity(continuation::builder(
                BuilderDomain::CreateAssignedTag(page),
            )),
        );
    }
    for child in &mut resource.children {
        decorate_tag_actions(child, ctx);
    }
}

/// What a control that browses `tag` invokes: the published tag's "open".
#[must_use]
pub fn open_tag_binding(tag: TagId, name: &str) -> Option<tilo_actions::ActionBinding<()>> {
    let open = NotesEffect::OpenTag { tag }.offered("open", "Browse tagged notes");
    let resource = Resource::new(tag_resource_id(tag), "notes.tag", name).with_action(open);
    tilo_actions::ActionBinding::unit_for(&resource, "open").ok()
}

/// What a control that opens `page` invokes: the published note's "open".
#[must_use]
pub fn open_page_binding(page: PageId, title: &str) -> Option<tilo_actions::ActionBinding<()>> {
    let open = NotesEffect::Open { page, block: None }.offered("open", "Open note");
    let resource = Resource::new(page_resource_id(page), "notes.page", title).with_action(open);
    tilo_actions::ActionBinding::unit_for(&resource, "open").ok()
}

fn tag_resource(tag: &TagMeta, ctx: &NotesResourceCtx) -> Resource {
    let tag_id = tag.id;
    let tag_name = if tag.name.trim().is_empty() {
        "Untitled tag"
    } else {
        tag.name.trim()
    };
    let mut resource = Resource::new(tag_resource_id(tag_id), "notes.tag", tag_name);
    resource = resource
        .with_action(NotesEffect::OpenTag { tag: tag_id }.offered("open", "Browse tagged notes"));
    let original = tag.clone();
    resource = resource.with_action(
        ActionSpec::prompted(
            "rename",
            "Rename tag",
            "Name",
            "tag name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_tag_action(&original, name)),
        )
        .with_continuation_identity(continuation::builder(BuilderDomain::RenameTag(tag.clone()))),
    );
    resource = resource.with_action(immediate_spec(
        "delete",
        "Delete tag",
        &delete_tag_action(tag_id),
    ));
    // Large vocabularies remain fully operable without attaching every tag as
    // an action to every page resource. The endpoint check in Action::apply
    // makes malformed, deleted, or stale page ids harmless no-ops.
    resource = resource.with_action(
        ActionSpec::prompted(
            "assign-page",
            "Add tag to note",
            "Note ID",
            "note UUID…",
            ExecutionLocus::ClientLocal,
            move |page: &str| {
                let page = page
                    .trim()
                    .parse::<PageId>()
                    .unwrap_or_else(|_| Uuid::nil());
                encode(&set_tag_assignment_action(page, tag_id, true))
            },
        )
        .with_continuation_identity(continuation::builder(BuilderDomain::TagMembership {
            tag: tag_id,
            assigned: true,
        })),
    );
    resource = resource.with_action(
        ActionSpec::prompted(
            "unassign-page",
            "Remove tag from note",
            "Note ID",
            "note UUID…",
            ExecutionLocus::ClientLocal,
            move |page: &str| {
                let page = page
                    .trim()
                    .parse::<PageId>()
                    .unwrap_or_else(|_| Uuid::nil());
                encode(&set_tag_assignment_action(page, tag_id, false))
            },
        )
        .with_continuation_identity(continuation::builder(BuilderDomain::TagMembership {
            tag: tag_id,
            assigned: false,
        })),
    );

    let mut tagged_pages = ctx
        .tag_assignments
        .iter()
        .filter(|(_, assigned)| *assigned == tag_id)
        .filter_map(|(page, _)| ctx.pages.iter().find(|candidate| candidate.id == *page))
        .collect::<Vec<_>>();
    tagged_pages.sort_by_key(|page| page.id);
    for page in tagged_pages
        .into_iter()
        .take(RESOURCE_TAG_PAGE_SNAPSHOT_LIMIT)
    {
        resource = resource.with_child(tag_page_resource(tag_id, page));
    }
    resource
}

/// A tag's membership row for one note. It carries its own address: the note
/// itself is already published once in the notebook tree, and a resource id
/// must be unique across the whole surface.
pub fn tag_page_resource_id(tag: TagId, page: PageId) -> String {
    format!("notes.tag-page:{tag}:{page}")
}

fn parse_tag_page_resource_id(id: &str) -> Option<(TagId, PageId)> {
    let (tag, page) = id.strip_prefix("notes.tag-page:")?.split_once(':')?;
    Some((tag.parse().ok()?, page.parse().ok()?))
}

fn tag_page_resource(tag: TagId, page: &PageMeta) -> Resource {
    let page_ref = Resource::new(
        tag_page_resource_id(tag, page.id),
        "notes.tag-page",
        page_label(page),
    );
    page_ref
        .with_action(
            NotesEffect::Open {
                page: page.id,
                block: None,
            }
            .offered("open", "Open note"),
        )
        .with_view(page_editor_view())
}

/// The note `id` within `resource`, at any depth.
fn find_note(resource: &Resource, id: &str) -> Option<Resource> {
    if resource.id.as_str() == id {
        return Some(resource.clone());
    }
    for child in &resource.children {
        if let Some(found) = find_note(child, id) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Workspace;
    use automerge::transaction::Transactable;
    use tilo_actions::{ActionInput, ExecutionMode, LocalTransport, OperationKind, ResultSpec};
    use ui::tilo_resource::Resource as _;
    use uuid::Uuid;

    #[test]
    fn page_tree_snapshot_preserves_exact_commands_and_availability() {
        let mut workspace = Workspace::new();
        let a = workspace.create_page(None);
        let b = workspace.create_page(None);
        let child = workspace.create_page(Some(a));
        workspace.get_mut(a).unwrap().title = "Quoted \" note\n雪".into();
        // Concurrent equal ordering keys exercise the existing re-key branch.
        let order = workspace.get(a).unwrap().order.clone();
        workspace.get_mut(b).unwrap().order = order;
        let snapshot = PageTreeSnapshot::new(workspace.pages()).unwrap();
        for page in [a, b, child, PageId::new_v4()] {
            let published = workspace.get(page).map(|meta| {
                page_resource_in(meta, workspace.pages(), Some(&workspace), &workspace.bodies)
            });
            for (id, command) in [
                ("move-up", PageTreeCommand::MoveUp),
                ("move-down", PageTreeCommand::MoveDown),
                ("indent", PageTreeCommand::Indent),
                ("outdent", PageTreeCommand::Outdent),
            ] {
                let expected = page_tree_action(&workspace, page, command)
                    .map(|action| immediate_spec("tree", "Move note", &action));
                let actual = snapshot.spec(&workspace, page, command, "tree", "Move note");
                assert_eq!(actual.is_some(), expected.is_some());
                let published = published
                    .as_ref()
                    .and_then(|resource| resource.actions.iter().find(|action| action.id == id));
                assert_eq!(published.is_some(), expected.is_some());
                if let (Some(actual), Some(expected)) = (actual, expected) {
                    assert_eq!(actual.locus, expected.locus);
                    assert_eq!(actual.contract, expected.contract);
                    let actual = actual.build_json(&serde_json::Map::new()).unwrap();
                    let expected = expected.build_json(&serde_json::Map::new()).unwrap();
                    assert_eq!(actual, expected);
                    assert_eq!(
                        published
                            .unwrap()
                            .build_json(&serde_json::Map::new())
                            .unwrap(),
                        expected
                    );
                    let decoded: NotesAction = serde_json::from_slice(&actual.payload).unwrap();
                    assert_eq!(encode(&decoded), actual);
                }
            }
        }
        // A fresh publication observes later metadata, never a prior snapshot.
        workspace.get_mut(b).unwrap().title = "Renamed".into();
        let contextual = [workspace.get(child).unwrap().clone()];
        let index = PageResourceIndex::new(&contextual, workspace.pages());
        let fresh = index.tree.unwrap();
        let expected = page_tree_action(&workspace, child, PageTreeCommand::Outdent).unwrap();
        assert_eq!(
            fresh
                .spec(
                    &workspace,
                    child,
                    PageTreeCommand::Outdent,
                    "tree",
                    "Move note"
                )
                .unwrap()
                .build_json(&serde_json::Map::new())
                .unwrap(),
            encode(&expected)
        );
    }

    fn fixed_tree_command_projection() -> serde_json::Value {
        let mut rows = Vec::new();
        for tied in [false, true] {
            let keys = ui::tilo_resource::order::sequence(None, None, 3);
            let mut workspace = Workspace::default();
            for index in 0..5 {
                let id = PageId::from_u128(index + 1);
                let parent = match index {
                    3 => Some(PageId::from_u128(2)),
                    4 => Some(PageId::from_u128(4)),
                    _ => None,
                };
                workspace.data.pages.push(PageMeta {
                    id,
                    parent,
                    title: format!("Quoted \" {index}\n雪"),
                    order: keys.get(index as usize).unwrap_or(&keys[0]).clone(),
                });
            }
            if tied {
                workspace.data.pages[2].order = workspace.data.pages[1].order.clone();
            }
            workspace.check_invariants().unwrap();
            let snapshot = PageTreeSnapshot::new(workspace.pages()).unwrap();
            for page in workspace.pages() {
                for (id, command) in [
                    ("move-up", PageTreeCommand::MoveUp),
                    ("move-down", PageTreeCommand::MoveDown),
                    ("indent", PageTreeCommand::Indent),
                    ("outdent", PageTreeCommand::Outdent),
                ] {
                    let operation = snapshot.spec(&workspace, page.id, command, id, "Move").map(|spec| {
                        let encoded = spec.build_json(&serde_json::Map::new()).unwrap();
                        serde_json::json!({"type_id":encoded.type_id,"payload":String::from_utf8(encoded.payload).unwrap(),"contract":spec.contract,"locus":spec.locus})
                    });
                    rows.push(serde_json::json!({"tied":tied,"page":page.id,"command":id,"operation":operation}));
                }
            }
        }
        serde_json::Value::Array(rows)
    }

    #[test]
    fn fixed_tree_command_projection_matches_recorded_eager_baseline() {
        let actual = fixed_tree_command_projection();
        assert_eq!(actual.as_array().unwrap().len(), 40);
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/page-tree-eager.json")).unwrap();
        assert_eq!(actual, expected);
    }

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    fn block(kind: BlockKind, text: &str, marks: Vec<MarkSpan>) -> BlockContent {
        BlockContent {
            kind,
            marks,
            ..BlockContent::paragraph("", text)
        }
    }

    /// Write `blocks` into `page`'s empty body through the action, and return
    /// the ids the document gave them.
    fn write_page(doc: &mut AutoCommit, page: PageId, blocks: &[BlockContent]) -> Vec<String> {
        let action = write_body_action(page, blocks);
        tilo_actions::apply_local(doc, &action, encode(&action)).unwrap();
        NotesDoc::page_block_snapshots(doc, page)
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn canonical_page_actions_validate_blocks_roll_back_and_run_file_jobs() {
        eprintln!(
            "phase canonical_actions_accept_every_editor_block_kind_and_reject_invalid_table_modes"
        );
        {
            let mut doc = AutoCommit::new();
            crate::notebook::initialize_empty(&mut doc).unwrap();
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let action = upsert_page_action(workspace.get(page).unwrap());
            tilo_actions::apply_local(&mut doc, &action, encode(&action)).unwrap();
            let kinds = [
                BlockKind::Paragraph,
                BlockKind::Quote,
                BlockKind::Heading(1),
                BlockKind::Heading(2),
                BlockKind::Heading(3),
                BlockKind::Bullet { indent: 5 },
                BlockKind::Numbered { indent: 5 },
                BlockKind::Checklist {
                    indent: 5,
                    done: false,
                },
                BlockKind::Checklist {
                    indent: 5,
                    done: true,
                },
                BlockKind::CodeBlock {
                    lang: Some("rust".into()),
                },
                BlockKind::Table { visual: false },
                BlockKind::Table { visual: true },
                BlockKind::Divider,
            ];
            let blocks = kinds
                .iter()
                .map(|kind| block(kind.clone(), "Preserved content", Vec::new()))
                .collect::<Vec<_>>();
            let action = write_body_action(page, &blocks);
            let mut peer = doc.clone();
            let applied = tilo_actions::dispatch(
                &mut doc,
                &action,
                encode(&action),
                &mut LocalTransport::new(),
            )
            .unwrap();
            tilo_actions::replay(&mut peer, applied.envelope(), &registry()).unwrap();
            assert_eq!(doc.get_heads(), peer.get_heads());
            crate::notebook::validate(&doc).unwrap();
            let stored = NotesDoc::page_blocks(&doc, page);
            assert_eq!(
                stored
                    .iter()
                    .map(|block| block.kind.clone())
                    .collect::<Vec<_>>(),
                kinds
            );
            let mut other = Workspace::new();
            let empty = other.create_page(None);
            let action = upsert_page_action(other.get(empty).unwrap());
            tilo_actions::apply_local(&mut doc, &action, encode(&action)).unwrap();
            for kind in ["table", "table:2", "table:true", "table:01", "table:1:0"] {
                let before = doc.get_heads();
                let action = NotesAction::WriteBody {
                    page: empty.to_string(),
                    blocks: vec![PageBlock {
                        kind: kind.into(),
                        text: "x".into(),
                        marks: Vec::new(),
                        nesting: 0,
                        code_group: None,
                    }],
                };
                assert!(
                    tilo_actions::apply_local(&mut doc, &action, encode(&action)).is_err(),
                    "invalid kind accepted: {kind}"
                );
                assert_eq!(doc.get_heads(), before);
            }
        }

        eprintln!("phase markdown_import_refuses_without_writing_and_replays_complete_page");
        {
            let page = PageId::new_v4();
            let mut doc = AutoCommit::new();
            let before = doc.get_heads();
            let oversized = import_markdown_action(
                page,
                "Import",
                &"x".repeat(MAX_MARKDOWN_IMPORT_BYTES + 1),
                vec![],
            );
            assert!(
                tilo_actions::dispatch(
                    &mut doc,
                    &oversized,
                    encode(&oversized),
                    &mut LocalTransport::new()
                )
                .is_err()
            );
            assert_eq!(doc.get_heads(), before);
            assert!(
                NotesDoc::list_pages(&doc).is_empty(),
                "a refused import must not leave phantom metadata"
            );
            let mut peer = doc.clone();
            let valid = import_markdown_action(
                page,
                "Import",
                "# Import\n\n**Rich content** and [[Import]]",
                vec![],
            );
            let applied = tilo_actions::dispatch(
                &mut doc,
                &valid,
                encode(&valid),
                &mut LocalTransport::new(),
            )
            .unwrap();
            tilo_actions::replay(&mut peer, applied.envelope(), &registry()).unwrap();
            assert_eq!(doc.get_heads(), peer.get_heads());
            assert_eq!(NotesDoc::page_block_snapshots(&doc, page).len(), 2);
            let marks = NotesDoc::page_block_marks(&doc, page);
            assert!(
                marks
                    .iter()
                    .flat_map(|(_, marks)| marks)
                    .any(|mark| matches!(
                        &mark.mark,
                        InlineMark::Link { page: linked } if *linked == page.to_string()
                    )),
                "a link to the imported title reaches the imported note: {marks:?}"
            );
            // The same page cannot be imported twice.
            let again = import_markdown_action(page, "Again", "text", vec![]);
            let heads = doc.get_heads();
            assert!(
                tilo_actions::dispatch(
                    &mut doc,
                    &again,
                    encode(&again),
                    &mut LocalTransport::new()
                )
                .is_err()
            );
            assert_eq!(doc.get_heads(), heads);
        }

        eprintln!("phase markdown_files_go_through_the_chooser_as_jobs_at_this_device");
        {
            let import = NotesEffect::specs()
                .into_iter()
                .find(|spec| spec.id == "import-markdown")
                .unwrap();
            let export = NotesEffect::ExportMarkdown {
                page: Uuid::from_u128(42),
            }
            .bound();
            for spec in [&import, &export] {
                assert!(spec.params().is_empty());
                assert_eq!(
                    spec.contract.execution,
                    ExecutionMode::Background { cancellable: true }
                );
                assert_eq!(spec.contract.result, ResultSpec::Job);
                assert!(
                    spec.contract
                        .tags
                        .iter()
                        .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY),
                    "choosing a file is the person's at this device"
                );
            }
        }
    }

    #[test]
    fn chooser_and_link_effects_answer_when_their_work_settles() {
        let mut doc = AutoCommit::new();
        crate::notebook::initialize_empty(&mut doc).unwrap();
        let page = PageMeta {
            id: Uuid::from_u128(42),
            title: "Guide".into(),
            parent: None,
            order: "a".into(),
        };
        NotesDoc::upsert_page_meta(&mut doc, &page);
        let resource = page_resource(&page, std::slice::from_ref(&page), &doc);
        let image = resource
            .actions
            .iter()
            .find(|action| action.id == "insert-image")
            .unwrap();
        let mut block = Resource::new("notes.block:test", "notes.block", "Guide");
        decorate_external_link_actions(
            &mut block,
            "Guide",
            &[InlineMarkSpan {
                start: 0,
                end: 5,
                mark: InlineMark::ExternalLink {
                    url: "https://example.test/guide".into(),
                },
            }],
        );
        let link = block.actions.first().unwrap();
        for action in [image, link] {
            assert_eq!(action.kind, OperationKind::Effect);
            assert!(
                matches!(action.contract.execution, ExecutionMode::Background { .. }),
                "chooser and link work stays pending until its actual outcome"
            );
            assert_eq!(action.contract.result, ResultSpec::Job);
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn links_keep_safe_uris_backlinks_and_stable_targets_through_actions() {
        eprintln!("phase external_link_marks_keep_their_safe_uri_through_the_action_wire");
        {
            let span = MarkSpan::new(
                0..5,
                Mark::ExternalLink("https://example.test/notes".into()),
            );
            let wire = InlineMarkSpan::from_ui(&span);
            assert_eq!(wire.to_ui(), Some(span));

            let unsafe_wire = InlineMarkSpan {
                start: 0,
                end: 5,
                mark: InlineMark::ExternalLink {
                    url: "javascript:alert(1)".into(),
                },
            };
            assert_eq!(unsafe_wire.to_ui(), None);
        }

        eprintln!("phase block_resources_publish_external_links_as_typed_portal_operations");
        {
            let mut resource = Resource::new("notes.block:test", "notes.block", "Guide");
            decorate_external_link_actions(
                &mut resource,
                "Read guide",
                &[InlineMarkSpan {
                    start: 5,
                    end: 10,
                    mark: InlineMark::ExternalLink {
                        url: "https://example.test/guide".into(),
                    },
                }],
            );
            let action = resource.actions.first().expect("external link action");
            assert_eq!(action.kind, OperationKind::Effect);
            assert_eq!(action.label, "Open link: guide");
            assert_eq!(
                effect_of(action),
                NotesEffect::OpenLink {
                    url: "https://example.test/guide".into()
                }
            );
        }

        eprintln!("phase canonical_links_publish_block_addressed_backlink_resources");
        {
            let (mut doc, _projects, source, target, _inbox) = seeded();
            let block_id = write_page(
                &mut doc,
                source,
                &[block(
                    BlockKind::Paragraph,
                    "See the launch plan for context",
                    vec![MarkSpan::new(8..19, Mark::Link(target))],
                )],
            )
            .remove(0);

            let pages = NotesDoc::list_pages(&doc);
            let bodies = read_bodies(&doc, &pages);
            let backlinks = canonical_backlinks(&bodies, &pages, target);
            assert_eq!(backlinks.len(), 1);
            assert_eq!(backlinks[0].source, source);
            assert_eq!(backlinks[0].block_id, block_id);
            assert_eq!(backlinks[0].excerpt, "See the launch plan for context");
            let resource = backlink_collection_resource(&bodies, &pages, target)
                .expect("linked target publishes a collection");
            assert_eq!(resource.id.as_str(), backlinks_resource_id(target));
            assert_eq!(resource.children.len(), 1);
            assert_eq!(resource.children[0].actions[0].id, "open");
        }

        eprintln!("phase renaming_a_page_updates_link_and_embed_labels_without_changing_targets");
        {
            let mut doc = AutoCommit::new();
            let source = Uuid::from_u128(1);
            let target = Uuid::from_u128(2);
            for (id, title, order) in [(source, "Source", "a"), (target, "Old title", "b")] {
                NotesDoc::upsert_page_meta(
                    &mut doc,
                    &PageMeta {
                        id,
                        title: title.into(),
                        parent: None,
                        order: order.into(),
                    },
                );
            }
            NotesDoc::fill_page_body(
                &mut doc,
                source,
                &[block(
                    BlockKind::Paragraph,
                    "Old title + Old title",
                    vec![
                        MarkSpan::new(0..9, Mark::Link(target)),
                        MarkSpan::new(12..21, Mark::Transclusion(target)),
                    ],
                )],
            )
            .unwrap();

            rename_page_action(
                &PageMeta {
                    id: target,
                    title: "Old title".into(),
                    parent: None,
                    order: "b".into(),
                },
                "New",
            )
            .apply(&mut doc)
            .unwrap();

            assert_eq!(
                NotesDoc::page_block_snapshots(&doc, source)[0].1,
                "New + New"
            );
            let marks = &NotesDoc::page_block_marks(&doc, source)[0].1;
            assert!(
                matches!(marks[0].mark, InlineMark::Link { ref page } if page == &target.to_string())
            );
            assert!(
                matches!(marks[1].mark, InlineMark::Transclusion { ref page } if page == &target.to_string())
            );
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    fn resource_count(resource: &Resource) -> usize {
        1 + resource.children.iter().map(resource_count).sum::<usize>()
    }

    /// A document seeded with a small page tree: a root "Projects" with two
    /// children, plus a second root "Inbox".
    fn seeded() -> (AutoCommit, PageId, PageId, PageId, PageId) {
        let mut doc = AutoCommit::new();
        let projects = Uuid::from_u128(0x1111);
        let a = Uuid::from_u128(0x2222);
        let b = Uuid::from_u128(0x3333);
        let inbox = Uuid::from_u128(0x4444);
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: projects,
                title: "Projects".into(),
                parent: None,
                order: "a".into(),
            },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: a,
                title: "Launch".into(),
                parent: Some(projects),
                order: "a".into(),
            },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: b,
                title: "Notes".into(),
                parent: Some(projects),
                order: "b".into(),
            },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: inbox,
                title: "Inbox".into(),
                parent: None,
                order: "b".into(),
            },
        );
        (doc, projects, a, b, inbox)
    }

    const PROJECTS: PageId = Uuid::from_u128(0x1111);
    const LAUNCH: PageId = Uuid::from_u128(0x2222);
    const NOTES: PageId = Uuid::from_u128(0x3333);
    const INBOX: PageId = Uuid::from_u128(0x4444);
    const URGENT: TagId = Uuid::from_u128(0x5555);

    fn spec_encoding_of(spec: &ActionSpec) -> EncodedAction {
        ui::testing::action_contract::spec_encoding(spec, None)
    }

    /// The effect `spec` invokes.
    fn effect_of(spec: &ActionSpec) -> NotesEffect {
        let encoded = spec_encoding_of(spec);
        assert_eq!(encoded.type_id, NotesEffect::OPERATION);
        NotesEffect::from_payload(&encoded.payload).unwrap()
    }

    fn dispatch(doc: &mut AutoCommit, action: &NotesAction) {
        tilo_actions::dispatch(doc, action, encode(action), &mut LocalTransport::new()).unwrap();
    }

    /// A canonical notebook built through the actions: a Projects page with
    /// two children and a block of text, an Inbox root, and a tag on Launch.
    fn notebook_seed() -> AutoCommit {
        let mut doc = AutoCommit::new();
        crate::notebook::initialize_empty(&mut doc).unwrap();
        for (id, parent, order, title) in [
            (PROJECTS, None, "a", "Projects"),
            (LAUNCH, Some(PROJECTS), "a", "Launch"),
            (NOTES, Some(PROJECTS), "b", "Notes"),
            (INBOX, None, "b", "Inbox"),
        ] {
            dispatch(&mut doc, &new_page_action(id, parent, order.into(), title));
        }
        dispatch(
            &mut doc,
            &write_body_action(
                PROJECTS,
                &[block(BlockKind::Paragraph, "Kickoff agenda", Vec::new())],
            ),
        );
        dispatch(&mut doc, &new_tag_action(URGENT, "urgent"));
        dispatch(&mut doc, &set_tag_assignment_action(LAUNCH, URGENT, true));
        doc
    }

    fn notebook_resources(doc: &AutoCommit) -> Vec<Resource> {
        use ui::tilo_resource::Resource as _;
        let state = crate::resource::Notebook::read(doc).expect("a canonical notebook");
        vec![ui::tilo_resource::publish::<crate::resource::Notebook>(
            "notes.notebook",
            "Notes",
            &state,
        )]
    }

    fn page_meta(doc: &AutoCommit, id: PageId) -> PageMeta {
        NotesDoc::list_pages(doc)
            .into_iter()
            .find(|p| p.id == id)
            .expect("a seeded page")
    }

    ui::action_contract_tests! {
        seed: notebook_seed(),
        registry: registry(),
        resources: |doc| notebook_resources(doc),
        palette: ["New note", "Rename note", "Delete note"],
        replay: [
            rename_page => |doc| encode(&rename_page_action(
                &page_meta(doc, LAUNCH),
                "Renamed via action",
            )),
            delete_subtree => |_| encode(&delete_page_action(PROJECTS)),
            reparent_via_upsert => |doc| {
                // Move Launch out from under Projects to a root after Inbox,
                // the way a drag-reorder does.
                let mut w = Workspace::new();
                w.data.pages = NotesDoc::list_pages(doc);
                assert!(w.move_to(LAUNCH, None, None));
                encode(&upsert_page_action(w.get(LAUNCH).unwrap()))
            },
        ],
        undo: [
            new_page => |_| encode(&new_page_action(Uuid::from_u128(0x9999), None, "c".into(), "Fresh")),
        ],
        encodes: [
            delete_page => (&page_resource_id(PROJECTS), "delete", |_, _| {
                encode(&delete_page_action(PROJECTS))
            }),
            rename_page => (&page_resource_id(PROJECTS), "rename", arg: "Renamed", |doc, _| {
                encode(&rename_page_action(&page_meta(doc, PROJECTS), "Renamed"))
            }),
        ],
    }

    fn ctx_from(doc: &AutoCommit) -> NotesResourceCtx {
        NotesResourceCtx {
            pages: NotesDoc::list_pages(doc),
            blocks: std::collections::HashMap::new(),
            tags: NotesDoc::list_tags(doc),
            tag_assignments: NotesDoc::list_tag_assignments(doc),
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn page_resources_publish_the_tree_view_copy_link_and_valid_moves() {
        eprintln!("phase ctrl_k_publishes_the_nested_page_tree");
        {
            let (doc, projects_id, _a, _b, _inbox) = seeded();
            let resources = provide_resources(&ctx_from(&doc), &doc);
            assert_eq!(resources.len(), 1, "one notebook root");
            let notebook = &resources[0];
            assert_eq!(notebook.kind, "notes.notebook");
            let nb_actions: Vec<&str> = notebook.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(nb_actions.contains(&"new-page"), "{nb_actions:?}");
            assert!(nb_actions.contains(&"daily-note"), "{nb_actions:?}");

            // Two root pages, in order, plus the tag catalogue.
            assert_eq!(
                notebook
                    .children
                    .iter()
                    .filter(|child| child.kind == "notes.page")
                    .count(),
                2,
                "Projects + Inbox"
            );
            let projects = notebook
                .children
                .iter()
                .find(|r| r.label == "Projects")
                .expect("a Projects note resource");
            assert_eq!(projects.kind, "notes.page");
            let p_actions: Vec<&str> = projects.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(p_actions.contains(&"rename"), "{p_actions:?}");
            assert!(p_actions.contains(&"delete"), "{p_actions:?}");
            assert!(p_actions.contains(&"new-child"), "{p_actions:?}");

            // Its two child notes nest underneath, in order, plus the body.
            let page_children: Vec<_> = projects
                .children
                .iter()
                .filter(|r| r.kind == "notes.page")
                .collect();
            assert_eq!(page_children.len(), 2, "Launch + Notes nest under Projects");
            assert_eq!(page_children[0].label, "Launch");
            assert_eq!(page_children[1].label, "Notes");
            let body = projects
                .children
                .iter()
                .find(|r| r.kind == RichText::KIND)
                .expect("page nests a rich-text body child");
            assert_eq!(body.id.as_str(), page_body_id(projects_id));
        }

        eprintln!("phase page_resource_publishes_the_localized_editor_view");
        {
            std::thread::spawn(|| {
                ui::i18n::I18nManager::install(
                    "en-US",
                    "en-US",
                    &[("en-US", include_str!("../locales/en-US.ftl"))],
                )
                .unwrap();
                let (doc, projects, _a, _b, _inbox) = seeded();
                let resource = provide_resources(&ctx_from(&doc), &doc)[0]
                    .children
                    .iter()
                    .find(|resource| resource.id.as_str() == page_resource_id(projects))
                    .cloned()
                    .expect("Projects page resource");
                let [view] = resource.views.as_slice() else {
                    panic!("one explicit page view expected")
                };
                assert_eq!(view.id, PAGE_EDITOR_VIEW_ID);
                assert_eq!(view.label, "Editor");
                assert_eq!(view.open_action, "open");
                assert!(view.primary);
                assert!(view.mount.is_none());
                assert!(view.validate(&resource).is_empty());
            })
            .join()
            .unwrap();
        }

        eprintln!("phase page_resource_exposes_a_stable_copy_link_operation");
        {
            let projects = Uuid::from_u128(0x1111);
            let meta = PageMeta {
                id: projects,
                title: "Projects".into(),
                parent: None,
                order: "a".into(),
            };
            let mut doc = AutoCommit::new();
            crate::notebook::initialize_empty(&mut doc).unwrap();
            NotesDoc::upsert_page_meta(&mut doc, &meta);
            let resource = page_resource(&meta, std::slice::from_ref(&meta), &doc);
            let copy = resource
                .actions
                .iter()
                .find(|action| action.id == "copy-link")
                .expect("note publishes copy-link");
            assert_eq!(copy.kind, OperationKind::Effect);
            assert_eq!(effect_of(copy), NotesEffect::CopyLink { page: projects });
            let duplicate = resource
                .actions
                .iter()
                .find(|action| action.id == "duplicate")
                .expect("canonical note publishes duplicate");
            let encoded = duplicate.build_json(&serde_json::Map::new()).unwrap();
            let NotesAction::DuplicateSubtree { source, pages } =
                serde_json::from_slice(&encoded.payload).unwrap()
            else {
                panic!("duplicate resource encoded the wrong action");
            };
            assert_eq!(source, projects.to_string());
            assert!(!pages.is_empty());
            let insert_image = resource
                .actions
                .iter()
                .find(|action| action.id == "insert-image")
                .expect("note publishes image insertion");
            assert_eq!(
                effect_of(insert_image),
                NotesEffect::InsertImage { page: projects }
            );
        }

        eprintln!("phase page_resources_expose_only_valid_canonical_hierarchy_moves");
        {
            let (_legacy, projects, a, b, inbox) = seeded();
            let pages = vec![
                PageMeta {
                    id: projects,
                    title: "Projects".into(),
                    parent: None,
                    order: "a".into(),
                },
                PageMeta {
                    id: a,
                    title: "A".into(),
                    parent: Some(projects),
                    order: "a".into(),
                },
                PageMeta {
                    id: b,
                    title: "B".into(),
                    parent: Some(projects),
                    order: "b".into(),
                },
                PageMeta {
                    id: inbox,
                    title: "Inbox".into(),
                    parent: None,
                    order: "b".into(),
                },
            ];
            let mut doc = AutoCommit::new();
            crate::notebook::initialize_empty(&mut doc).unwrap();
            for page in &pages {
                NotesDoc::upsert_page_meta(&mut doc, page);
            }

            let resource = page_resource(&pages[2], &pages, &doc);
            let ids = resource
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>();
            assert!(ids.contains(&"move-up"));
            assert!(!ids.contains(&"move-down"), "last sibling cannot move down");
            assert!(ids.contains(&"indent"));
            assert!(ids.contains(&"outdent"));

            let action = resource
                .actions
                .iter()
                .find(|action| action.id == "move-up")
                .map(spec_encoding_of)
                .unwrap();
            tilo_actions::apply_local(
                &mut doc,
                &serde_json::from_slice::<NotesAction>(&action.payload).unwrap(),
                action,
            )
            .unwrap();
            let updated = NotesDoc::list_pages(&doc);
            let children = updated
                .iter()
                .filter(|page| page.parent == Some(projects))
                .map(|page| page.id)
                .collect::<Vec<_>>();
            assert_eq!(children, vec![b, a]);
            let moved = updated.iter().find(|page| page.id == b).unwrap();
            let refreshed = page_resource(moved, &updated, &doc);
            assert!(
                !refreshed
                    .actions
                    .iter()
                    .any(|action| action.id == "move-up")
            );
            assert!(
                refreshed
                    .actions
                    .iter()
                    .any(|action| action.id == "move-down")
            );
            assert!(
                refreshed
                    .actions
                    .iter()
                    .any(|action| action.id == "duplicate")
            );

            // Sharing validation is confined to one immutable publication. A new
            // refresh must refuse malformed data rather than reuse a past snapshot.
            doc.put(automerge::ROOT, "notes_schema_version", 999_u64)
                .unwrap();
            let invalid = page_resource(moved, &updated, &doc);
            assert!(!invalid.actions.iter().any(|action| {
                matches!(
                    action.id.as_str(),
                    "duplicate" | "move-up" | "move-down" | "indent" | "outdent"
                )
            }));
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn block_actions_edit_reorder_and_replay_unicode_against_live_targets() {
        eprintln!("phase block_actions_replay_unicode_splices_and_refuse_stale_targets");
        {
            let (seeded_doc, projects, _a, _b, _inbox) = seeded();
            let mut first = seeded_doc;
            first.commit();
            let mut second = AutoCommit::load(&first.save()).unwrap();
            let insert =
                write_body_action(projects, &[block(BlockKind::Paragraph, "hé", Vec::new())]);
            let envelope = tilo_actions::apply_local(&mut first, &insert, encode(&insert)).unwrap();
            tilo_actions::replay(&mut second, &envelope, &registry()).unwrap();
            let block = NotesDoc::page_block_snapshots(&first, projects)[0]
                .0
                .clone();

            let splice = edit_body_action(
                projects,
                RichTextAction::SpliceText {
                    region: RegionKind::Body,
                    block: block.clone(),
                    // Offsets are characters, so this replaces the one `é`.
                    at: 1,
                    del: 1,
                    ins: "x".into(),
                },
            );
            let envelope = tilo_actions::apply_local(&mut first, &splice, encode(&splice)).unwrap();
            tilo_actions::replay(&mut second, &envelope, &registry()).unwrap();
            assert_eq!(
                first.get_heads(),
                second.get_heads(),
                "rich text replay converges"
            );
            assert_eq!(NotesDoc::page_block_snapshots(&second, projects)[0].1, "hx");

            let before = second.save();
            let stale = edit_body_action(
                projects,
                RichTextAction::SpliceText {
                    region: RegionKind::Body,
                    block,
                    at: 99,
                    del: 0,
                    ins: "!".into(),
                },
            );
            let error = tilo_actions::apply_local(&mut second, &stale, encode(&stale))
                .expect_err("a stale byte range must refuse, never clamp");
            assert!(matches!(error, ActionError::Refused(_)));
            assert_eq!(second.save(), before, "refusal leaves no partial mutation");
        }

        eprintln!("phase block_resource_exposes_the_canonical_edit_action");
        {
            let (mut doc, projects, _a, _b, _inbox) = seeded();
            let block = write_page(
                &mut doc,
                projects,
                &[block(BlockKind::Paragraph, "Original", Vec::new())],
            )
            .remove(0);
            let resource = block_resource(
                projects,
                &NoteBlockResource {
                    id: block.clone(),
                    text: "Original".into(),
                },
            )
            .expect("non-empty block is a resource");
            let edit = resource
                .actions
                .iter()
                .find(|action| action.id == "replace-text")
                .expect("agent-editable block action");
            let encoded = prompted_encode(edit, "Rewritten");
            let decoded: NotesAction = serde_json::from_slice(&encoded.payload).unwrap();
            assert_eq!(
                decoded,
                edit_body_action(
                    projects,
                    RichTextAction::SpliceText {
                        region: RegionKind::Body,
                        block,
                        at: 0,
                        del: "Original".chars().count(),
                        ins: "Rewritten".into(),
                    },
                ),
            );
        }

        eprintln!("phase block_resources_expose_only_valid_reorder_actions_and_delete");
        {
            let page = Uuid::new_v4();
            let blocks = ["a", "b", "c"].map(|id| NoteBlockResource {
                id: id.into(),
                text: format!("Block {id}"),
            });
            let first = block_resource_in_page(page, &blocks[0], &blocks).unwrap();
            let middle = block_resource_in_page(page, &blocks[1], &blocks).unwrap();
            let last = block_resource_in_page(page, &blocks[2], &blocks).unwrap();
            let ids = |resource: &Resource| {
                resource
                    .actions
                    .iter()
                    .map(|action| action.id.clone())
                    .collect::<HashSet<_>>()
            };
            assert!(!ids(&first).contains("move-up"));
            assert!(ids(&first).contains("move-down"));
            assert!(ids(&middle).contains("move-up"));
            assert!(ids(&middle).contains("move-down"));
            assert!(ids(&last).contains("move-up"));
            assert!(!ids(&last).contains("move-down"));
            assert!(ids(&middle).contains("delete"));

            let move_up = middle
                .actions
                .iter()
                .find(|action| action.id == "move-up")
                .map(spec_encoding_of)
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<NotesAction>(&move_up.payload).unwrap(),
                edit_body_action(
                    page,
                    RichTextAction::MoveBlocks {
                        blocks: vec!["b".into()],
                        after: None,
                    }
                )
            );
        }

        eprintln!("phase block_resource_exports_the_same_stable_reveal_address_as_search");
        {
            let (doc, projects, _a, _b, _inbox) = seeded();
            let mut ctx = ctx_from(&doc);
            ctx.blocks.insert(
                projects,
                vec![
                    NoteBlockResource {
                        id: "obj-a".into(),
                        text: "Project overview".into(),
                    },
                    NoteBlockResource {
                        id: "obj-b".into(),
                        text: "Budget decisions".into(),
                    },
                ],
            );
            let resources = provide_resources(&ctx, &doc);
            assert!(
                tilo_actions::door::resolve(&resources, &open_block_address(projects, "obj-b"))
                    .is_none()
            );
            let block = block_resource(projects, &ctx.blocks[&projects][1])
                .expect("second block resource resolves lazily");
            let open = block
                .actions
                .iter()
                .find(|action| action.id == "open")
                .expect("block reveal action");
            assert_eq!(open.kind, OperationKind::View);
            assert_eq!(
                open_block_address(projects, "obj-b"),
                ActionAddress::new(&block.id.0, &open.id)
            );
            assert_eq!(
                effect_of(open),
                NotesEffect::Open {
                    page: projects,
                    block: Some("obj-b".into())
                }
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn resource_snapshots_stay_bounded_but_lazily_resolvable() {
        eprintln!("phase ten_thousand_blocks_stay_out_of_the_open_snapshot");
        {
            let (doc, projects, _a, _b, _inbox) = seeded();
            let mut ctx = ctx_from(&doc);
            ctx.blocks.insert(
                projects,
                (0..10_000)
                    .map(|id| NoteBlockResource {
                        id: format!("block-{id}"),
                        text: format!("Block {id}"),
                    })
                    .collect(),
            );
            let resources = provide_resources(&ctx, &doc);
            assert!(
                tilo_actions::door::resolve(
                    &resources,
                    &open_block_address(projects, "block-9999")
                )
                .is_none()
            );
            assert!(block_resource(projects, &ctx.blocks[&projects][9_999]).is_some());
        }

        eprintln!("phase wide_and_deep_page_resources_are_bounded_but_lazily_resolvable");
        {
            let mut pages = Vec::new();
            let root = Uuid::new_v4();
            pages.push(PageMeta {
                id: root,
                title: "root".into(),
                parent: None,
                order: "a".into(),
            });
            let mut parent = root;
            for depth in 0..(RESOURCE_PAGE_SNAPSHOT_DEPTH + 4) {
                let id = Uuid::new_v4();
                pages.push(PageMeta {
                    id,
                    title: format!("deep-{depth}"),
                    parent: Some(parent),
                    order: "a".into(),
                });
                parent = id;
            }
            for order in 0..(RESOURCE_PAGE_SNAPSHOT_LIMIT + 40) {
                pages.push(PageMeta {
                    id: Uuid::new_v4(),
                    title: format!("wide-{order}"),
                    parent: None,
                    order: ui::tilo_resource::order::sequence(None, None, order + 1)
                        .pop()
                        .unwrap(),
                });
            }
            let ctx = NotesResourceCtx {
                pages: pages.clone(),
                blocks: HashMap::new(),
                tags: Vec::new(),
                tag_assignments: Vec::new(),
            };
            let resources = provide_resources(&ctx, &AutoCommit::new());
            // The notebook, each bounded page, that page's rich-text body, and
            // the tag catalogue.
            assert!(
                resource_count(&resources[0]) <= RESOURCE_PAGE_SNAPSHOT_LIMIT * 2 + 2,
                "notebook context must remain bounded"
            );
            let late = pages.last().expect("late note");
            let lazy = page_resource(late, &pages, &AutoCommit::new());
            assert_eq!(lazy.id.as_str(), page_resource_id(late.id));
            assert!(
                lazy.actions.iter().any(|action| action.id == "open"),
                "an omitted note remains addressable by its stable id"
            );
        }

        eprintln!("phase large_tag_vocabularies_and_memberships_have_bounded_snapshots");
        {
            let page = Uuid::new_v4();
            let pages = vec![PageMeta {
                id: page,
                title: "Reference".into(),
                parent: None,
                order: "a".into(),
            }];
            let tags = (0..(RESOURCE_TAG_SNAPSHOT_LIMIT + 40))
                .map(|index| TagMeta {
                    id: Uuid::new_v4(),
                    name: format!("tag-{index}"),
                })
                .collect::<Vec<_>>();
            let late = tags.last().expect("late tag").clone();
            let ctx = NotesResourceCtx {
                pages,
                blocks: HashMap::new(),
                tags,
                tag_assignments: vec![(page, late.id); RESOURCE_TAG_PAGE_SNAPSHOT_LIMIT + 40],
            };

            let resources = provide_resources(&ctx, &AutoCommit::new());
            let tags = resources[0]
                .children
                .iter()
                .find(|child| child.id.as_str() == "notes.tags")
                .expect("tag catalogue");
            assert_eq!(
                tags.children.len(),
                RESOURCE_TAG_SNAPSHOT_LIMIT,
                "the open Tags resource must have a fixed materialization ceiling"
            );
            let page_resource = resources[0]
                .children
                .iter()
                .find(|child| child.kind == "notes.page")
                .expect("page");
            assert!(
                page_resource.actions.len() <= RESOURCE_PAGE_TAG_ACTION_LIMIT + 12,
                "per-note tag actions must not grow with the whole vocabulary"
            );

            let late_resource = tag_resource(&late, &ctx);
            assert_eq!(late_resource.id.as_str(), tag_resource_id(late.id));
            assert!(late_resource.children.len() <= RESOURCE_TAG_PAGE_SNAPSHOT_LIMIT);
            assert!(
                late_resource
                    .actions
                    .iter()
                    .any(|action| action.id == "assign-page"),
                "an omitted tag remains operable by stable id"
            );
            assert!(
                late_resource
                    .actions
                    .iter()
                    .any(|action| action.id == "unassign-page"),
                "absolute membership removal remains available too"
            );
        }

        eprintln!("phase stable_block_resource_ids_refuse_malformed_or_empty_targets");
        {
            let page = Uuid::new_v4();
            assert_eq!(
                parse_block_resource_id(&block_resource_id(page, "block-1")),
                Some((page, "block-1"))
            );
            assert!(parse_block_resource_id("notes.block:not-a-uuid:block").is_none());
            assert!(parse_block_resource_id(&format!("notes.block:{page}:")).is_none());
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn page_hierarchy_actions_delete_create_and_record_addressable_history() {
        eprintln!("phase delete_page_removes_the_subtree_and_keeps_survivor_keys");
        {
            let (mut doc, projects, _a, _b, inbox) = seeded();
            assert_eq!(NotesDoc::list_pages(&doc).len(), 4);

            let action = delete_page_action(projects);
            let encoded = encode(&action);
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

            let left = NotesDoc::list_pages(&doc);
            assert_eq!(
                left.len(),
                1,
                "only Inbox survives (Projects + 2 children gone)"
            );
            assert_eq!(left[0].id, inbox);
            assert_eq!(left[0].order, "b", "a survivor keeps its key");
        }

        eprintln!("phase new_child_prompt_appends_under_its_parent");
        {
            let (mut doc, projects, a, b, _inbox) = seeded();
            let resources = provide_resources(&ctx_from(&doc), &doc);
            let page = resources[0]
                .children
                .iter()
                .find(|r| r.label == "Projects")
                .unwrap();

            // New-child builds an AppendPage under the right parent; the position
            // is resolved when it applies (the minted id differs each call).
            let new_child_spec = page.actions.iter().find(|a| a.id == "new-child").unwrap();
            let encoded = prompted_encode(new_child_spec, "Child");
            let decoded: NotesAction =
                serde_json::from_slice(&encoded.payload).expect("payload decodes to NotesAction");
            let NotesAction::AppendPage { id, title, parent } = &decoded else {
                panic!("new-child built the wrong action: {decoded:?}");
            };
            assert_eq!(
                parent,
                &projects.to_string(),
                "child parented under Projects"
            );
            assert_eq!(title, "Child");
            let child = id.parse::<PageId>().unwrap();
            dispatch(&mut doc, &decoded);
            let children = NotesDoc::list_pages(&doc)
                .into_iter()
                .filter(|page| page.parent == Some(projects))
                .map(|page| page.id)
                .collect::<Vec<_>>();
            assert_eq!(
                children,
                vec![a, b, child],
                "appended after the existing children"
            );
        }

        eprintln!("phase a11y_tree_surfaces_page_delete_as_the_affordance_action");
        {
            let (doc, projects, _a, _b, _inbox) = seeded();
            let resources = provide_resources(&ctx_from(&doc), &doc);

            // An inert root; the operable nodes come from the published resources.
            // `HashMap::new()`'s type is inferred from `refresh`'s signature.
            let root = ui::widget::Column::new();
            let node_map = std::collections::HashMap::new();
            let mut tree = ui::SemanticTree::new();

            // First refresh populates the tree; a second, unchanged refresh is a
            // no-op with identical node identity (retained, not rebuilt each frame).
            let d1 = tree.refresh(&root, &node_map, None, &resources);
            assert!(!d1.added.is_empty(), "the first refresh populates the tree");
            let notebook = tree
                .node(&ui::SemanticId("r:notes.notebook".into()))
                .expect("notebook node present");
            assert_eq!(notebook.name.as_deref(), Some("Notes"));
            assert!(
                notebook.action("new-page").is_some(),
                "the notebook affords new-note"
            );
            let ids1: Vec<ui::SemanticId> = tree.iter().map(|n| n.id.clone()).collect();

            let d2 = tree.refresh(&root, &node_map, None, &resources);
            assert!(d2.is_empty(), "an unchanged frame yields no delta: {d2:?}");
            let ids2: Vec<ui::SemanticId> = tree.iter().map(|n| n.id.clone()).collect();
            assert_eq!(ids1, ids2, "node identity survives across frames");

            // The Projects page node exposes rename (prompted) + delete (immediate);
            // the delete affordance carries the on-screen control's exact action.
            let page = tree
                .node(&ui::SemanticId(format!("r:notes.page:{projects}")))
                .expect("projects note node");
            assert!(page.action("rename").is_some(), "note affords rename");
            let exposed = match &page.action("delete").expect("delete affordance").kind {
                ui::SemanticActionKind::App(enc) => enc.clone(),
                other => panic!("expected an app action, got {other:?}"),
            };
            assert_eq!(
                exposed,
                encode(&delete_page_action(projects)),
                "the a11y affordance and the on-screen builder are one action"
            );

            // Invoking the exposed action mutates the doc exactly as the on-screen
            // control does: two identical docs, deleted through each path, compared.
            let dispatch_encoded = |doc: &mut AutoCommit, enc: EncodedAction| {
                let decoded = registry().decode(&enc).unwrap();
                let mut tx = LocalTransport::new();
                tilo_actions::dispatch(doc, decoded.as_ref(), enc, &mut tx).unwrap();
            };
            let (mut via_a11y, ..) = seeded();
            let (mut via_affordance, ..) = seeded();
            dispatch_encoded(&mut via_a11y, exposed);
            dispatch_encoded(&mut via_affordance, encode(&delete_page_action(projects)));

            let a: Vec<_> = NotesDoc::list_pages(&via_a11y)
                .iter()
                .map(|p| p.id)
                .collect();
            let b: Vec<_> = NotesDoc::list_pages(&via_affordance)
                .iter()
                .map(|p| p.id)
                .collect();
            assert_eq!(
                a, b,
                "invoking the a11y affordance deletes the same note + subtree"
            );
            assert_eq!(
                NotesDoc::list_pages(&via_a11y).len(),
                1,
                "only Inbox survives"
            );
        }

        eprintln!("phase page_action_history_is_addressable");
        {
            let context = history_context(&NotesAction::UpsertPage(PageData {
                id: "page-7".into(),
                title: "Launch brief".into(),
                parent: String::new(),
                order: "a".into(),
            }));
            assert_eq!(context.action_label, "Update note");
            let resource = context.resource.expect("note resource");
            assert_eq!(resource.id, "notes.page:page-7");
            assert_eq!(resource.label, "Launch brief");
        }
    }

    #[test]
    fn new_note_appends_at_apply_while_daily_note_keeps_its_published_fractional_key() {
        let mut doc = notebook_seed();
        let resources = notebook_resources(&doc);
        let root = &resources[0];
        let new_spec = root
            .actions
            .iter()
            .find(|spec| spec.id == "new-page")
            .unwrap();
        let new_action: NotesAction =
            serde_json::from_slice(&prompted_encode(new_spec, "Later note").payload).unwrap();
        let NotesAction::AppendPage { id, parent, .. } = &new_action else {
            panic!("a new note resolves its sibling position when it applies");
        };
        assert!(parent.is_empty());
        let new_id = id.parse().unwrap();
        let daily_spec = root
            .actions
            .iter()
            .find(|spec| spec.id == "daily-note")
            .unwrap();
        let daily_action: NotesAction =
            serde_json::from_slice(&daily_spec.build_args(Vec::new()).unwrap().payload).unwrap();
        let NotesAction::UpsertPage(daily) = &daily_action else {
            panic!("today's note carries the position published with its action");
        };
        assert!(ui::tilo_resource::order::is_key(&daily.order));
        let daily_id = daily.id.parse().unwrap();
        let published_order = daily.order.clone();

        let intervening = Uuid::new_v4();
        dispatch(
            &mut doc,
            &append_page_action(intervening, None, "Arrived meanwhile"),
        );
        let intervening_order = page_meta(&doc, intervening).order;
        assert!(intervening_order.as_str() > "b");
        dispatch(&mut doc, &new_action);
        assert!(
            page_meta(&doc, new_id).order > intervening_order,
            "a stale new-note action still appends after the live siblings"
        );
        dispatch(&mut doc, &daily_action);
        assert_eq!(
            page_meta(&doc, daily_id).order,
            published_order,
            "today's action retains its published fractional position"
        );
        assert_eq!(page_meta(&doc, PROJECTS).order, "a");
        assert_eq!(page_meta(&doc, INBOX).order, "b");
        assert_eq!(page_meta(&doc, LAUNCH).order, "a");
        assert_eq!(page_meta(&doc, NOTES).order, "b");
        crate::notebook::validate(&doc).unwrap();

        let resources = notebook_resources(&doc);
        let daily_spec = resources[0]
            .actions
            .iter()
            .find(|spec| spec.id == "daily-note")
            .unwrap();
        assert!(
            matches!(effect_of(daily_spec), NotesEffect::Open { page, .. } if page == daily_id),
            "once today's note exists its public action opens that note"
        );
    }

    #[test]
    fn tag_actions_replay_identically_and_publish_browseable_resources() {
        eprintln!("phase tag_actions_replay_byte_identically_and_delete_tombstones_edges");
        {
            let (seeded_doc, projects, _a, _b, _inbox) = seeded();
            let mut first = seeded_doc;
            first.commit();
            let mut second = AutoCommit::load(&first.save()).unwrap();
            let tag = Uuid::from_u128(0x5555);
            for action in [
                new_tag_action(tag, "Release"),
                set_tag_assignment_action(projects, tag, true),
            ] {
                let envelope =
                    tilo_actions::apply_local(&mut first, &action, encode(&action)).unwrap();
                tilo_actions::replay(&mut second, &envelope, &registry()).unwrap();
            }
            assert_eq!(
                first.get_heads(),
                second.get_heads(),
                "keyed tag graph replays exactly"
            );
            assert_eq!(
                NotesDoc::list_tag_assignments(&second),
                vec![(projects, tag)]
            );

            let remove = delete_tag_action(tag);
            let envelope = tilo_actions::apply_local(&mut first, &remove, encode(&remove)).unwrap();
            tilo_actions::replay(&mut second, &envelope, &registry()).unwrap();
            assert_eq!(
                first.get_heads(),
                second.get_heads(),
                "tag tombstone replays exactly"
            );
            assert!(NotesDoc::list_tags(&second).is_empty());
            assert!(
                NotesDoc::list_tag_assignments(&second).is_empty(),
                "edge tombstones were removed too"
            );
        }

        eprintln!("phase tags_publish_stable_browseable_resources");
        {
            let (mut doc, projects, _a, _b, _inbox) = seeded();
            let tag = Uuid::from_u128(0x6666);
            NotesDoc::upsert_tag(
                &mut doc,
                &TagMeta {
                    id: tag,
                    name: "Research".into(),
                },
            );
            NotesDoc::set_tag_assignment(&mut doc, projects, tag, true);
            let resources = provide_resources(&ctx_from(&doc), &doc);
            let tags = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "notes.tags")
                .expect("tags ride the notebook");
            let resource = tags
                .children
                .iter()
                .find(|resource| resource.id.as_str() == tag_resource_id(tag))
                .expect("stable tag resource");
            assert!(resource.actions.iter().any(|action| action.id == "open"));
            assert!(resource.actions.iter().any(|action| action.id == "rename"));
            assert!(resource.actions.iter().any(|action| action.id == "delete"));
            assert_eq!(
                resource.children[0].id.as_str(),
                tag_page_resource_id(tag, projects),
                "the membership row has its own address, distinct from the note's"
            );

            let page = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.as_str() == page_resource_id(projects))
                .expect("tagged note resource");
            assert!(
                page.actions
                    .iter()
                    .any(|action| action.id == format!("unassign-tag:{tag}"))
            );
            let create = page
                .actions
                .iter()
                .find(|action| action.id == "new-and-assign-tag")
                .expect("normal create-and-assign workflow");
            assert!(matches!(
                serde_json::from_slice::<NotesAction>(&prompted_encode(create, "Planning").payload),
                Ok(NotesAction::CreateAndAssignTag { page, tag: TagData { name, .. } }) if page == projects.to_string() && name == "Planning"
            ));
        }
    }
}
