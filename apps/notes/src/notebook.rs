//! The notebook: what Notes stores, and the operations on it that are more
//! than editing one field.
//!
//! [`Notebook`] is the record a workspace's notes hang off: the tags a note
//! may carry. The notes themselves ([`Page`]) are kept in a chain of parts
//! beside it ([`crate::note_list::NoteList`]), so an edit of one note costs the
//! part holding it, not every note. A note's body is rich text, edited by
//! field edits at its path in its part
//! (`NoteList::paths().pages.item(&id).body.perform(&op)`); assigning or
//! removing a tag is a field edit of the note's `tags`, renaming a tag one of
//! the tag's `name`.

use crate::metadata::CATALOG;
use std::collections::HashMap;

use tilo_document::action::{RegionKind, Span};
use tilo_document::{ClipBlock, RichText, RichTextAction};
use ui::rich_edit::BlockContent;
use ui::tilo_resource::{
    self, ActionCx, Atom, Bytes, Collection, Id, Map, Model, Origin, Reactions, Refusal, Set,
    resource,
};
use ui::widget::marks::Mark;

use crate::model::PageId;

pub use crate::note_list::{body_object, page_object};

/// Largest image a note accepts. The side placing it keeps the bytes, sealed,
/// in the content store, and the action adding it carries the reference
/// ([`Bytes::to_keep`]).
pub const MAX_ASSET_BYTES: usize = 1024 * 1024;

/// The title a note nobody has named holds: none. A title is only ever what a person typed, so a
/// note that has none reads as the word for an unnamed note in whoever opens it, in their own
/// language.
pub const NO_TITLE: &str = "";

/// The notebook: the tags its notes carry. The notes are kept in parts
/// beside it ([`crate::note_list::NoteList`]).
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Notebook {
    /// Tags
    pub tags: Collection<Tag>,
}

/// One note.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Page {
    /// Title
    ///
    /// Renaming relabels every link to the note.
    #[model(label)]
    #[model(readonly)]
    pub title: String,
    /// Parent
    ///
    /// The note this one sits under; none for a top-level note. Only moving
    /// the note changes it.
    #[model(readonly)]
    pub parent: Option<Id>,
    /// Rank
    ///
    /// Its place among the notes beside it, under the same parent: the notes
    /// there are read in the order of their ranks, whatever part keeps each.
    /// Only making or moving the note sets it.
    #[model(readonly)]
    pub rank: String,
    /// Body
    ///
    /// Read whole: a note is short, and search, links and copies read every
    /// word of it. Published as a rich-text resource of its own beneath the
    /// note (`notes.pages/pages/<page>/body`), whose block actions apply
    /// as field edits of this field.
    #[model(whole)]
    #[model(resource)]
    pub body: RichText,
    /// Tags
    pub tags: Set<Id>,
    /// Made by
    ///
    /// The person or agent whose action made the note; none for a note
    /// made before notes kept it, or by a field edit. Only making the note
    /// sets it.
    #[model(readonly)]
    #[model(default = None)]
    pub creator: Option<ui::tilo_resource::MemberId>,
    /// The name its maker acted under, when software made it: an agent's
    /// name, shown where the workspace no longer names the agent.
    #[model(readonly)]
    pub creator_name: String,
    /// Origin
    ///
    /// Where the note was first written, when it was moved here from another
    /// tool: shown as its author and date, while `creator` keeps who moved
    /// it.
    #[model(readonly)]
    pub origin: Option<Origin>,
    /// Path
    ///
    /// The file it was imported from, by its path in the set it came with
    /// (`guide/setup.md`): links between the set's files by their paths lead
    /// to the notes they became.
    #[model(readonly)]
    pub path: Option<String>,
    /// Reactions
    ///
    /// What people answered the note with, shown under its title.
    pub reactions: Reactions,
}

/// A named tag notes carry.
#[derive(Model, Default, Clone, Debug, PartialEq)]
#[model(check = Tag::valid)]
pub struct Tag {
    /// Name
    ///
    /// Written whole: two people renaming a tag at once keep one of the
    /// names, never a mix.
    #[model(label)]
    pub name: Atom,
}

impl Tag {
    /// A tag is listed and found by its name: never blank, never padded.
    fn valid(&self) -> Result<(), Refusal> {
        let name = self.name.0.as_str();
        if name.trim().is_empty() || name.trim() != name {
            return Err(Refusal::new("a tag has a name without surrounding spaces"));
        }
        Ok(())
    }
}

impl Page {
    /// A new note titled `title` (no title at all when blank) under `parent`.
    #[must_use]
    pub fn titled(title: &str, parent: Option<Id>) -> Self {
        Self {
            title: title.trim().to_owned(),
            parent,
            ..Self::default()
        }
    }

    /// This note, made by whoever invoked the action applying now, for a
    /// verb that takes no context.
    #[must_use]
    pub fn made_by_invoker(self) -> Self {
        Self {
            creator: ui::tilo_resource::invoking_member(),
            creator_name: ui::tilo_resource::invoking_name(),
            ..self
        }
    }

    /// This note, made by whoever invoked the action `cx` applies.
    #[must_use]
    pub fn made(self, cx: &ActionCx) -> Self {
        Self {
            creator: cx.author().cloned(),
            creator_name: cx.acting_name(),
            ..self
        }
    }

    /// The body's paragraphs as the editor shows them, a block per
    /// paragraph, with their ids.
    #[must_use]
    pub fn blocks(&self) -> Vec<BlockContent> {
        let Some(state) = self.body.state() else {
            return Vec::new();
        };
        state
            .body()
            .all()
            .filter_map(|id| state.block(id))
            .map(materialize)
            .collect()
    }

    /// The paragraphs changed since the body's history stood at `cursor`,
    /// each with where it is in the body of `len` paragraphs: `None` when
    /// that cannot be told (the body was reordered, grew or shrank, or was
    /// read whole again), and every paragraph must be read.
    #[must_use]
    pub fn blocks_changed_since(
        &self,
        cursor: tilo_document::ChangeCursor,
        len: usize,
    ) -> Option<Vec<(usize, BlockContent)>> {
        let state = self.body.state()?;
        let order = state.body();
        if order.len() != len {
            return None;
        }
        state
            .changed_since(cursor)?
            .iter()
            .map(|id| Some((order.position(id)?, materialize(state.block(id)?))))
            .collect()
    }
}

/// A paragraph as the editor shows it.
fn materialize(view: &std::rc::Rc<tilo_doc::snapshot::BlockView>) -> BlockContent {
    #[cfg(test)]
    tests::MATERIALIZED_BLOCKS.with(|count| count.set(count.get() + 1));
    tilo_rich_text::content_of(view)
}

/// The id a note is stored under.
#[must_use]
pub fn page_id(page: PageId) -> Id {
    Id::from(page.to_string())
}

/// The note a stored id names, when it is one.
#[must_use]
pub fn page_of(id: &Id) -> Option<PageId> {
    id.as_str().parse().ok()
}

/// The kind of a note, an item of the notebook's pages, as links name it.
pub const PAGE_KIND: &str = "notes.pages.pages";

/// The address a link to the heading `anchor` of the note `page` opens:
/// the note, with the anchor after its id (`<note>#<anchor>`), as Markdown
/// names a heading. Opening it shows the note at the heading whose anchor it
/// is ([`crate::markdown::heading_anchor`]).
#[must_use]
pub fn heading_href(page: &Id, anchor: &str) -> Option<String> {
    use ui::tilo_reference::link::{
        AppId, Link, LinkSecrets, LinkTarget, Mode, ResourceAddress, ResourceSegment, Scope,
    };
    let mut target = LinkTarget::new(Scope::Own);
    target.app = Some(AppId::parse("so.tilo.notes").ok()?);
    target.record = Some(ResourceSegment::Primary);
    target.resource = Some(ResourceAddress {
        kind: PAGE_KIND.to_owned(),
        id: format!("{page}#{anchor}"),
        span: None,
    });
    Link {
        mode: Mode::Desktop,
        target,
        secrets: LinkSecrets::default(),
    }
    .to_native()
    .ok()
}

/// The note and the heading anchor a link's resource id names
/// ([`heading_href`]): `<note>` alone, or `<note>#<anchor>`.
#[must_use]
pub fn note_and_anchor(id: &str) -> Option<(PageId, Option<&str>)> {
    let (page, anchor) = match id.split_once('#') {
        Some((page, anchor)) => (page, Some(anchor).filter(|anchor| !anchor.is_empty())),
        None => (id, None),
    };
    Some((page.parse().ok()?, anchor))
}

/// One Markdown file of a set imported together
/// ([`crate::note_list::NoteList::import_markdown_files`]).
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct MarkdownFile {
    /// Its path in the set (`guide/setup.md`): what the set's links name it
    /// by.
    pub path: String,
    /// The note's title; the file's first `# heading`, else its name, without.
    #[serde(default)]
    pub title: Option<String>,
    /// Its text, as Markdown.
    pub markdown: String,
    /// Where it was first written, when it was moved here from another tool.
    #[serde(default)]
    pub origin: Option<Origin>,
}

impl MarkdownFile {
    /// The note's title: the one given, else the file's leading `# heading`,
    /// else its file name without the extension.
    pub(crate) fn title(&self) -> String {
        if let Some(title) = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            return title.to_owned();
        }
        if let Some(heading) = self
            .markdown
            .trim_start_matches('\u{feff}')
            .lines()
            .find(|line| !line.trim().is_empty())
            .and_then(|line| line.strip_prefix("# "))
            .map(str::trim)
            .filter(|heading| !heading.is_empty())
        {
            return heading.to_owned();
        }
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        name.rsplit_once('.')
            .map_or(name, |(stem, _)| stem)
            .to_owned()
    }
}

/// A notebook stored before version 3 kept its notes in the resource itself;
/// they are kept in parts beside it now ([`crate::note_list::NoteList`]), and an
/// older notebook is not carried over: it says so rather than opening
/// empty.
fn migrate_stored(_doc: &tilo_resource::Doc, from: u32) -> Result<(), tilo_resource::ModelError> {
    Err(tilo_resource::ModelError::invalid(format!(
        "this notebook was stored at version {from}, before notes were kept in parts, and is not migrated"
    )))
}

#[resource(
    kind = "notes.notebook",
    version = 3,
    migrate = migrate_stored,
    read_content = Notebook::read_tags,
    document_child = Notebook::document_projection_child,
    effects = crate::effect::NotesEffect,
    available = Notebook::available
)]
impl Notebook {
    /// New tag
    #[action(icon = "tag")]
    pub fn new_tag(
        &mut self,
        cx: &mut ActionCx,
        #[param("Name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a tag has a name"));
        }
        if self.tag_named(name).is_some() {
            return Err(Refusal::new("a tag of that name exists"));
        }
        let id = Id::from(cx.uuid().to_string());
        self.tags.push(
            id,
            Tag {
                name: Atom(name.to_owned()),
            },
        );
        Ok(())
    }

    /// Delete tag
    ///
    /// Delete the tag; the notes that carried it no longer show it.
    #[action(item = tags, destructive, icon = "trash-2")]
    pub fn delete_tag(&mut self, tag: &Id) -> Result<(), Refusal> {
        self.tags.remove(tag).ok_or("no such tag")?;
        Ok(())
    }

    /// A verb on a tag needs a tag to act on: until the notebook has one,
    /// "Delete tag" is not offered, rather than offered asking for a tag
    /// there is none of.
    fn available(&self, verb: notebook::Verb) -> ui::tilo_actions::Availability {
        match verb {
            notebook::Verb::DeleteTag if self.tags.is_empty() => {
                ui::tilo_actions::Availability::Unavailable {
                    code: "notes.no_tags".into(),
                    reason: "The notebook has no tags".into(),
                }
            }
            _ => ui::tilo_actions::Availability::Available,
        }
    }
}

impl Notebook {
    /// The tag named `name`.
    #[must_use]
    pub fn tag_named(&self, name: &str) -> Option<Id> {
        self.tags
            .iter()
            .find(|(_, tag)| tag.name.0 == name)
            .map(|(id, _)| id.clone())
    }

    /// Only tags are addressed by the notebook's read; custom view children
    /// do not inherit that disclosure.
    fn document_projection_child(&self, id: &str) -> bool {
        Self::tag_of_resource(id).is_some_and(|tag| self.tags.get(&tag).is_some())
    }

    /// The tag a published resource id names (`notes.notebook/tags/<id>`).
    fn tag_of_resource(id: &str) -> Option<Id> {
        match tilo_resource::items::item_of_resource::<Self>(id) {
            Some(("tags", tag)) => Some(tag),
            _ => None,
        }
    }

    /// The notebook as a reader reads it: its tags; a tag by its name. The
    /// notes are read from the parts they are kept in.
    fn read_tags(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let Some(item) = item else {
            return Ok(serde_json::json!({
                "kind": <Self as tilo_resource::Resource>::KIND,
                "tags": self.tags.iter().map(|(id, tag)| serde_json::json!({
                    "id": id.as_str(),
                    "name": tag.name.0,
                })).collect::<Vec<_>>(),
            }));
        };
        let id = Self::tag_of_resource(item.as_str()).unwrap_or_else(|| item.clone());
        let tag = self
            .tags
            .get(&id)
            .ok_or_else(|| format!("no tag `{item}`; read the notebook for its tags"))?;
        Ok(serde_json::json!({
            "kind": "notes.tag",
            "item": item.as_str(),
            "name": tag.name.0,
        }))
    }
}

/// A new body holding `blocks`, written when the note is stored.
#[must_use]
pub fn written(blocks: &[BlockContent]) -> RichText {
    let mut body = RichText::default();
    body.perform(clips_op(blocks));
    body
}

/// The rich-text op replacing a body with `blocks`, formatted as they are.
#[must_use]
pub fn clips_op(blocks: &[BlockContent]) -> RichTextAction {
    RichTextAction::ReplaceClips {
        blocks: blocks
            .iter()
            .map(tilo_rich_text::clip_of_content)
            .collect::<Vec<ClipBlock>>(),
    }
}

/// The rich-text op marking characters `range` of `block` with `mark`: a
/// format, a page link.
#[must_use]
pub fn mark_op(block: &str, range: std::ops::Range<usize>, mark: &Mark) -> RichTextAction {
    let (name, value) = tilo_rich_text::document_mark(mark);
    RichTextAction::SetMark {
        region: RegionKind::Body,
        spans: vec![Span {
            block: block.to_owned(),
            start: range.start,
            end: range.end,
        }],
        name: name.to_owned(),
        value: Some(value),
        also_clear: Vec::new(),
    }
}

/// Whether `bytes` are a picture a note shows.
///
/// # Errors
/// Refuses anything but a PNG, JPEG or WebP picture of at most
/// [`MAX_ASSET_BYTES`].
pub fn check_picture(bytes: &[u8]) -> Result<(), Refusal> {
    if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
        return Err(Refusal::new("a picture is between 1 byte and 1 MiB"));
    }
    if image_mime(bytes).is_none() {
        return Err(Refusal::new("a picture is PNG, JPEG or WebP"));
    }
    Ok(())
}

/// The picture format of `bytes`, when it is one notes show.
#[must_use]
pub fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    tilo_content::image_format(bytes)
        .map(|format| format.mime)
        .filter(|mime| matches!(*mime, "image/png" | "image/jpeg" | "image/webp"))
}

/// The content store of the window whose code is running. Outside a window
/// (a test, a page without a store), an empty one in memory.
#[must_use]
pub fn content() -> tilo_content::ContentStore {
    ui::tilo_resource::content::current().unwrap_or_else(tilo_content::ContentStore::memory)
}

/// The pictures of a notebook's `images` whose bytes are readable now from
/// `content`, by the `asset:` source their bodies name them by. A picture a
/// member added may still be on its way from their device; it is
/// absent until it arrives.
#[must_use]
pub fn image_snapshot(
    images: &Map<Bytes>,
    content: &tilo_content::ContentStore,
) -> HashMap<String, Vec<u8>> {
    images
        .iter()
        .filter_map(|(digest, stored)| Some((format!("asset:{digest}"), stored.read(content)?)))
        .collect()
}

/// The most Markdown one import takes: 1 MiB of text in at most 10,000
/// blocks.
pub const MAX_MARKDOWN_IMPORT_BYTES: usize = 1024 * 1024;
pub const MAX_MARKDOWN_IMPORT_BLOCKS: usize = 10_000;

/// Whether `markdown` is small enough for one import.
pub fn check_markdown_import(markdown: &str) -> Result<(), String> {
    if markdown.len() > MAX_MARKDOWN_IMPORT_BYTES {
        return Err(CATALOG.t("notes-import-too-big"));
    }
    if crate::markdown::parse(markdown).len() > MAX_MARKDOWN_IMPORT_BLOCKS {
        return Err(CATALOG.t("notes-import-too-many-blocks"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "notebook/tests.rs"]
pub(crate) mod tests;
