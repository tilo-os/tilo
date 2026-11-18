//! The notebook's notes: a collection the framework keeps beside the
//! notebook, which decides where each note is stored, so that an edit of one
//! note of thousands costs that note. The notebook's own resource
//! ([`Notebook`]) keeps the tags above them.
//!
//! - **Order.** Each note carries its place among the notes beside it
//!   ([`Page::rank`]), set where it is made or moved from every note held
//!   there ([`prepare`]), so the tree reads across them all.
//! - **What reads across the collection.** A note whose parent, or any note
//!   above it, is gone is not shown, wherever notes are read ([`Pages`]); it
//!   comes back when the delete is undone. A tag a note carries that the
//!   notebook no longer names is not shown either. Neither is written
//!   anywhere else.
//! - **What a change means elsewhere.** Renaming a note relabels the links to
//!   it wherever one is kept ([`spread`]), each such change its own step.
//! - **Copies and appends** are kept beside the note they copy or add to
//!   ([`Chained::beside`](ui::framework::Chained::beside)).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use tilo_document::action::RegionKind;
use tilo_document::{ClipBlock, RichText, RichTextAction};
use ui::rich_edit::BlockContent;
use ui::tilo_resource::{
    self, ActionCx, Bytes, Collection, Id, Map, MemberId, Model, NonBlank, Origin, Reactions,
    Refusal, resource,
};
use ui::widget::marks::Mark;

use crate::model::PageId;
use crate::notebook::{
    MarkdownFile, Notebook, Page, check_markdown_import, check_picture, heading_href, mark_op,
    page_id, page_of,
};

/// The notebook's notes the engine keeps in one of its parts: every
/// note made while it was the newest, and everything done to them since.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct NoteList {
    /// Notes
    ///
    /// Each note is a storage unit of its own.
    #[model(unit)]
    pub pages: Collection<Page>,
    /// Images
    ///
    /// The pictures this part's notes show, by the BLAKE3 digest their
    /// bodies name them by. Only placing or keeping a picture writes one; a
    /// note shows a picture any part keeps.
    #[model(readonly)]
    pub images: Map<Bytes>,
}

/// Where a note goes, worked out where its move is invoked: under which
/// note, and its place among the notes there.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct Placement {
    /// The note it goes under; none for the top level.
    #[serde(default)]
    pub parent: Option<Id>,
    /// Its place among the notes there.
    pub rank: String,
}

/// The notes the links of some Markdown lead to, looked up where the action
/// taking it is invoked, across every part.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Links {
    /// The note each `[[Title]]` the Markdown names leads to.
    #[serde(default)]
    pub titles: BTreeMap<String, Id>,
    /// The note each file the Markdown links to by its path became.
    #[serde(default)]
    pub paths: BTreeMap<String, Id>,
}

/// What a Markdown import reads of the notes kept in every part, where it is
/// invoked: its place among the top-level notes, the notes its links lead
/// to, and whether a note of the id it names is kept already.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct ImportReads {
    /// Its place among the top-level notes.
    pub rank: String,
    /// The notes its links lead to.
    #[serde(default)]
    pub links: Links,
    /// Whether a note of the id it names is kept already, in any part.
    #[serde(default)]
    pub imported: bool,
}

/// One note of a copied subtree, read where the copy is invoked: the copy's
/// id, its place and what it says, its links already leading to the copies.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct PageCopy {
    /// The id the copy is kept under.
    pub id: Id,
    /// Its title.
    pub title: String,
    /// The note it goes under: the original's parent for the copied note,
    /// a copy for the notes under it.
    #[serde(default)]
    pub parent: Option<Id>,
    /// Its place among the notes there.
    pub rank: String,
    /// Its body.
    pub blocks: Vec<ClipBlock>,
    /// The tags it carries.
    #[serde(default)]
    pub tags: Vec<Id>,
}

/// One page of a set brought from another tool in one step (a Notion
/// export): its title, its Markdown and the pictures it shows.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct ImportedNote {
    /// The note's title.
    pub title: String,
    /// Its text, as Markdown.
    pub markdown: String,
    /// The pictures it shows.
    #[serde(default)]
    pub images: Vec<Bytes>,
}

impl NoteList {
    fn page_mut(&mut self, page: &Id) -> Result<&mut Page, Refusal> {
        self.pages.get_mut(page).ok_or_else(no_page)
    }

    /// What system search finds of the part: each note by its title, and
    /// each block by its words.
    #[must_use]
    pub fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        crate::search::entries(self.pages.iter())
    }

    /// The note `page` and the notes under it that this part keeps, parents
    /// before their children.
    fn subtree(&self, page: &Id) -> Vec<Id> {
        let mut under: HashMap<&Id, Vec<&Id>> = HashMap::new();
        for (id, note) in self.pages.iter() {
            if let Some(parent) = &note.parent {
                under.entry(parent).or_default().push(id);
            }
        }
        let mut ordered = vec![page.clone()];
        let mut at = 0;
        while let Some(parent) = ordered.get(at).cloned() {
            for child in under.get(&parent).into_iter().flatten() {
                if !ordered.contains(child) {
                    ordered.push((*child).clone());
                }
            }
            at += 1;
        }
        ordered
    }

    /// Name `picture`, which the side placing it kept, by its digest; the
    /// digest.
    fn store_image(&mut self, picture: &Bytes) -> Result<String, Refusal> {
        match picture.inline() {
            // Data too large for the part is kept by whoever places it.
            Some(bytes) if Bytes::of(bytes).inline().is_none() => {
                return Err(Refusal::new("a picture is kept where it is placed"));
            }
            Some(bytes) => check_picture(bytes)?,
            None if usize::try_from(picture.len()).unwrap_or(usize::MAX)
                > crate::notebook::MAX_ASSET_BYTES =>
            {
                return Err(Refusal::new("a picture is between 1 byte and 1 MiB"));
            }
            None => {}
        }
        let digest = blake3::Hash::from_bytes(picture.hash())
            .to_hex()
            .to_string();
        self.images.insert(digest.clone(), picture.clone());
        Ok(digest)
    }

    /// Relabel every link and embed of note `target` in this part's notes
    /// to read `title`.
    fn relabel(&mut self, target: PageId, title: &str) {
        let mut edits = Vec::new();
        for (id, page) in self.pages.iter() {
            let ops = relabel_ops(page, target, title);
            if !ops.is_empty() {
                edits.push((id.clone(), ops));
            }
        }
        for (id, ops) in edits {
            if let Some(page) = self.pages.get_mut(&id) {
                for op in ops {
                    page.body.perform(op);
                }
            }
        }
    }

    /// Whether a note of this part links to or embeds note `target`.
    fn links_to(&self, target: PageId) -> bool {
        self.pages
            .values()
            .any(|page| !relabel_ops(page, target, "").is_empty())
    }
}

/// The edits relabelling every link and embed of note `target` in `page`'s
/// body to read `title`; with an empty `title`, every one there is.
fn relabel_ops(page: &Page, target: PageId, title: &str) -> Vec<RichTextAction> {
    let Some(state) = page.body.state() else {
        return Vec::new();
    };
    let mut ops = Vec::new();
    // Read the shared paragraphs' marks, without copying every page's prose
    // and formatting into editor blocks.
    for block in state.blocks(0..usize::MAX) {
        let mut ranges: Vec<_> = block
            .marks
            .iter()
            .filter_map(|span| {
                let mark = tilo_rich_text::editor_mark(&span.name, &span.value)?;
                if !matches!(&mark, Mark::Link(linked) | Mark::Transclusion(linked) if *linked == target)
                {
                    return None;
                }
                let byte = |at| {
                    block
                        .text
                        .char_indices()
                        .nth(at)
                        .map_or(block.text.len(), |(byte, _)| byte)
                };
                let start = byte(span.start);
                let end = byte(span.end);
                let old = block.text.get(start..end)?;
                (start < end && old != title).then(|| (span.start, old.chars().count(), mark))
            })
            .collect();
        // Last first, so each edit leaves the earlier ranges where they are.
        // Stored marks already use character offsets.
        ranges.sort_unstable_by_key(|(at, _, _)| std::cmp::Reverse(*at));
        for (at, del, mark) in ranges {
            ops.push(RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: block.id.clone(),
                at,
                del,
                ins: title.to_owned(),
            });
            ops.push(mark_op(
                &block.id,
                at..at.saturating_add(title.chars().count()),
                &mark,
            ));
        }
    }
    ops
}

fn no_page() -> Refusal {
    Refusal::new("no such note")
}

fn placed(place: Option<Placement>) -> Result<Placement, Refusal> {
    place.ok_or_else(|| Refusal::new("where the note goes is worked out where it is moved"))
}

/// A body written from `blocks`.
fn body_of(blocks: Vec<ClipBlock>) -> RichText {
    let mut body = RichText::default();
    body.perform(RichTextAction::ReplaceClips { blocks });
    body
}

/// A new body holding `blocks`.
fn written(blocks: &[BlockContent]) -> RichText {
    crate::notebook::written(blocks)
}

/// The blocks `markdown` is, as the note `new` titled `title`, kept as the
/// file at `path`: `[[Title]]` links lead to the notes `links` names, a link
/// to a heading of the same file (`#install`) to the note's own heading, and
/// a link to another file by its path to the note that file became, among
/// `files` (path to note) imported with it or the notes `links` names.
fn seed(
    markdown: &str,
    new: &Id,
    title: &str,
    path: Option<&str>,
    files: &HashMap<String, Id>,
    links: &Links,
) -> Vec<BlockContent> {
    let mut specs = crate::markdown::parse(markdown);
    crate::markdown::resolve_link_ids(&mut specs, |target| {
        if target == title {
            return page_of(new);
        }
        links.titles.get(target).and_then(page_of)
    });
    crate::markdown::resolve_relative_links(&mut specs, |link| {
        let (file, anchor) = crate::markdown::relative_target(path, link)?;
        let note = match file {
            None => new.clone(),
            Some(file) if path == Some(file.as_str()) => new.clone(),
            Some(file) => files
                .get(&file)
                .cloned()
                .or_else(|| links.paths.get(&file).cloned())?,
        };
        match anchor {
            Some(anchor) => heading_href(&note, &anchor).map(Mark::ResourceLink),
            None => page_of(&note).map(Mark::Link),
        }
    });
    crate::markdown::specs_to_blocks(&specs)
        .iter()
        .map(ui::rich_edit::Block::content)
        .collect()
}

/// The note a page id names, from either form a reader meets it in: the
/// stored id, or the resource the notes are published as
/// (`notes.pages/pages/<id>`).
fn note_key(id: Id) -> Id {
    crate::actions::page_of_resource(id.as_str()).map_or(id, page_id)
}

#[resource(
    kind = "notes.pages",
    params = note_list,
    search = NoteList::search,
    read_content_in_doc = NoteList::read_note,
    answer = NoteList::created_page,
    effects = crate::effect::NotesEffect
)]
impl NoteList {
    /// New note
    ///
    /// A note titled `title`, after the notes already under `parent` (a
    /// top-level note when none).
    #[action(
        keywords = "new page, create note, add note, memo",
        answers,
        icon = "file-plus",
        step = "create",
        writes = pages
    )]
    pub fn new_note(
        &mut self,
        cx: &mut ActionCx,
        #[param("Title")] title: String,
        #[param("The note it goes under; none for a top-level note")] parent: Option<Id>,
        #[param("The id the note is kept under; a new one when none")] note: Option<Id>,
        #[param("Its place among the notes there: filled in where it is invoked")] rank: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let id = match note {
            Some(note) if page_of(&note).is_none() => {
                return Err(Refusal::new("a note's id is a UUID"));
            }
            Some(note) if self.pages.contains(&note) => {
                return Err(Refusal::new("a note of that id exists"));
            }
            Some(note) => note,
            None => Id::from(cx.uuid().to_string()),
        };
        let mut page = Page::titled(&title, parent.map(note_key)).made(cx);
        page.rank = rank.unwrap_or_default();
        self.pages.push(id, page);
        Ok(())
    }

    /// New note inside
    ///
    /// A note titled `title`, after the notes already under this one.
    #[action(keywords = "sub page, subpage, child note, nested note", item = pages, answers, icon = "list-tree", writes = pages)]
    pub fn new_child(
        &mut self,
        cx: &mut ActionCx,
        page: &Id,
        #[param("Title")] title: String,
        #[param("Its place among the notes there: filled in where it is invoked")] rank: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let id = Id::from(cx.uuid().to_string());
        let mut note = Page::titled(&title, Some(page.clone())).made(cx);
        note.rank = rank.unwrap_or_default();
        self.pages.push(id, note);
        Ok(())
    }

    /// Rename note
    ///
    /// Give the note a new title; every link to it reads the new title.
    #[action(keywords = "change title, retitle", item = pages, field = "title", step = "edit", did = "renamed", icon = "pencil", writes = pages)]
    pub fn rename(
        &mut self,
        page: &Id,
        #[param("New title")] title: String,
    ) -> Result<(), Refusal> {
        let title = title.trim().to_owned();
        let note = self.page_mut(page)?;
        if note.title == title {
            return Ok(());
        }
        note.title.clone_from(&title);
        if let Some(target) = page_of(page) {
            ui::tilo_resource::profiling::measure("notes.rename.relabel", || {
                self.relabel(target, &title);
            });
        }
        Ok(())
    }

    /// Relabel the links to a note in this part's notes to read its title:
    /// what renaming it writes into every other part holding a link to it.
    #[action(by_app, step = "edit", writes = pages)]
    pub fn relabel_links(
        &mut self,
        #[param("The title the links read")] title: String,
        #[param("The note they lead to")] target: Id,
    ) -> Result<(), Refusal> {
        let target = page_of(&target).ok_or_else(no_page)?;
        self.relabel(target, title.trim());
        Ok(())
    }

    /// Delete note
    ///
    /// Delete the note and every note under it.
    #[action(keywords = "remove note, trash note, delete page", item = pages, destructive, icon = "trash-2", writes = pages)]
    pub fn delete(&mut self, page: &Id) -> Result<(), Refusal> {
        self.pages.get(page).ok_or_else(no_page)?;
        // The notes under it kept in other parts are not shown while it is
        // gone, and come back with it.
        for id in self.subtree(page) {
            self.pages.remove(&id);
        }
        Ok(())
    }

    /// Move note
    ///
    /// Put the note under `parent` (top level when none), before the note
    /// `before` there, or after the notes there when none.
    #[action(item = pages, audience = "agents", writes = pages)]
    pub fn move_note(
        &mut self,
        page: &Id,
        #[param("The note it goes under; none for the top level")] parent: Option<Id>,
        #[param("The note it goes before; none to go last")] before: Option<Id>,
        #[param("Where it goes: filled in where it is invoked")] place: Option<Placement>,
    ) -> Result<(), Refusal> {
        let _ = (parent, before);
        self.put(page, place)
    }

    /// Move note up
    #[action(item = pages, icon = "arrow-up", step = "move", writes = pages)]
    pub fn move_up(
        &mut self,
        page: &Id,
        #[param("Where it goes: filled in where it is invoked")] place: Option<Placement>,
    ) -> Result<(), Refusal> {
        self.put(page, place)
    }

    /// Move note down
    #[action(item = pages, icon = "arrow-down", step = "move", writes = pages)]
    pub fn move_down(
        &mut self,
        page: &Id,
        #[param("Where it goes: filled in where it is invoked")] place: Option<Placement>,
    ) -> Result<(), Refusal> {
        self.put(page, place)
    }

    /// Indent note
    ///
    /// Put the note under the note above it.
    #[action(item = pages, icon = "arrow-right", writes = pages)]
    pub fn indent(
        &mut self,
        page: &Id,
        #[param("Where it goes: filled in where it is invoked")] place: Option<Placement>,
    ) -> Result<(), Refusal> {
        self.put(page, place)
    }

    /// Outdent note
    ///
    /// Put the note beside the note it is under, right after it.
    #[action(item = pages, icon = "arrow-left", writes = pages)]
    pub fn outdent(
        &mut self,
        page: &Id,
        #[param("Where it goes: filled in where it is invoked")] place: Option<Placement>,
    ) -> Result<(), Refusal> {
        self.put(page, place)
    }

    /// Duplicate note
    ///
    /// Copy the note and every note under it, right after it. Links between
    /// the copied notes lead to the copies.
    #[action(keywords = "copy note, clone note", item = pages, answers, icon = "copy", writes = pages)]
    pub fn duplicate(
        &mut self,
        cx: &mut ActionCx,
        page: &Id,
        #[param("The copies: read where it is invoked")] copies: Option<Vec<PageCopy>>,
    ) -> Result<(), Refusal> {
        let _ = page;
        let copies =
            copies.ok_or_else(|| Refusal::new("what is copied is read where the copy is made"))?;
        for copy in copies {
            if page_of(&copy.id).is_none() || self.pages.contains(&copy.id) {
                return Err(Refusal::new("a copy is kept under a new id"));
            }
            let mut tags = tilo_resource::Set::new();
            for tag in copy.tags {
                tags.insert(tag);
            }
            let note = Page {
                title: copy.title,
                parent: copy.parent,
                rank: copy.rank,
                body: body_of(copy.blocks),
                tags,
                creator: None,
                creator_name: String::new(),
                // A copy is written here, by whoever copies it.
                origin: None,
                path: None,
                // People reacted to the original; the copy starts unanswered.
                reactions: Reactions::new(),
            }
            .made(cx);
            self.pages.push(copy.id, note);
        }
        Ok(())
    }

    /// Tag note
    ///
    /// Give the note the tag named `name`. A tag the notebook has none of is
    /// made first with `new-tag`.
    #[action(keywords = "label, add tag, categorize", item = pages, icon = "tag", writes = pages)]
    pub fn tag(
        &mut self,
        page: &Id,
        #[param("Tag")] name: String,
        #[param("The tag of that name: filled in where it is invoked")] tag: Option<Id>,
    ) -> Result<(), Refusal> {
        let tag = tag.ok_or_else(|| {
            Refusal::new(format!(
                "the notebook has no tag named “{}”: make it with new-tag first",
                name.trim()
            ))
        })?;
        self.page_mut(page)?.tags.insert(tag);
        Ok(())
    }

    /// Keep image
    ///
    /// Keep a PNG, JPEG or WebP picture with the notebook, for a note's body
    /// to show by `![alt](asset:<digest>)`.
    #[action(audience = "agents")]
    pub fn keep_image(&mut self, #[param("The picture")] bytes: Bytes) -> Result<(), Refusal> {
        self.store_image(&bytes).map(drop)
    }

    /// Place image
    ///
    /// Keep a PNG, JPEG or WebP picture with the notebook and show it at the
    /// end of the note, described by `alt`.
    #[action(item = pages, audience = "agents", writes = pages)]
    pub fn place_image(
        &mut self,
        page: &Id,
        #[param("The picture")] bytes: Bytes,
        #[param("A short description of it")] alt: Option<String>,
    ) -> Result<(), Refusal> {
        self.pages.get(page).ok_or_else(no_page)?;
        let digest = self.store_image(&bytes)?;
        let alt = alt
            .filter(|alt| !alt.trim().is_empty())
            .unwrap_or_else(|| "Image".to_owned())
            .replace([']', '[', '(', ')'], " ");
        self.page_mut(page)?
            .body
            .perform(RichTextAction::AppendParagraph {
                text: format!("![{}](asset:{digest})", alt.trim()),
            });
        Ok(())
    }

    /// Keep a picture with the note and show it there by `edit`, the edit of
    /// its body naming it: a picture pasted or dropped into the note, one
    /// change and one undo step.
    #[action(by_app, step = "write", writes = pages)]
    pub fn show_image(
        &mut self,
        page: &Id,
        #[param("The picture")] bytes: Bytes,
        #[param("The edit of the body that shows it")] edit: RichTextAction,
    ) -> Result<(), Refusal> {
        self.pages.get(page).ok_or_else(no_page)?;
        self.store_image(&bytes)?;
        self.page_mut(page)?.body.perform(edit);
        Ok(())
    }

    /// Create note from Markdown
    ///
    /// A top-level note titled `title` whose body is the Markdown: headings,
    /// bullets, `- [ ]` checklists, quotes, code, tables and `[[Note title]]`
    /// links. A leading `# title` equal to the title is dropped. A link to a
    /// heading (`[Undo](#undo)`) leads to that heading of the note. Answers
    /// the note's id and resource.
    #[action(answers, audience = "agents", writes = pages)]
    pub fn create_page(
        &mut self,
        cx: &mut ActionCx,
        #[param("The note's title")] title: NonBlank,
        #[param("The note's body as Markdown")] markdown: Option<String>,
        #[param(
            "Where it was first written, when it is moved here from elsewhere: shown as its author and date, while its history keeps who moved it"
        )]
        origin: Option<Origin>,
        #[param("Its place among the top-level notes: filled in where it is invoked")] rank: Option<
            String,
        >,
        #[param("The notes its links lead to: filled in where it is invoked")] links: Option<Links>,
    ) -> Result<(), Refusal> {
        let title = title.as_str();
        let markdown =
            crate::markdown::strip_matching_title_heading(&markdown.unwrap_or_default(), title);
        check_markdown_import(&markdown).map_err(Refusal::new)?;
        let origin = Origin::checked_option(origin)?;
        let id = Id::from(cx.uuid().to_string());
        let page = Page::titled(title, None).made(cx);
        let blocks = seed(
            &markdown,
            &id,
            &page.title,
            None,
            &HashMap::new(),
            &links.unwrap_or_default(),
        );
        self.pages.push(
            id,
            Page {
                body: written(&blocks),
                origin,
                rank: rank.unwrap_or_default(),
                ..page
            },
        );
        Ok(())
    }

    /// Import Markdown
    ///
    /// Add Markdown text as a new top-level note titled `title`, with the
    /// pictures it shows. A `[[Title]]` link leads to the note of that
    /// title, a link to a heading (`#install`) to that heading, and a link
    /// to a file imported before by its `path` (`../guide/setup.md#install`)
    /// to that note. Several files that link to each other import together
    /// with `import-markdown-files`.
    #[action(audience = "agents", writes = pages)]
    pub fn import_markdown(
        &mut self,
        #[param("Title")] title: String,
        #[param("Markdown content")] markdown: String,
        #[param("Pictures the Markdown shows")] images: Vec<Bytes>,
        #[param("The id the note is kept under; a new one when none")] note: Option<Id>,
        #[param("The file's path in the set it comes from (guide/setup.md), for links by path")]
        path: Option<String>,
        #[param(
            "Where it was first written, when it is moved here from elsewhere: shown as its author and date, while its history keeps who moved it"
        )]
        origin: Option<Origin>,
        #[param(
            "Its place, the notes its links lead to and whether its id is taken: filled in where it is invoked"
        )]
        reads: Option<ImportReads>,
    ) -> Result<(), Refusal> {
        check_markdown_import(&markdown).map_err(Refusal::new)?;
        let origin = Origin::checked_option(origin)?;
        let reads = reads.unwrap_or_default();
        let id = match note {
            Some(note) if page_of(&note).is_none() => {
                return Err(Refusal::new("a note's id is a UUID"));
            }
            Some(note) if self.pages.contains(&note) || reads.imported => {
                return Err(Refusal::coded(
                    ui::tilo_actions::codes::NOTE_ALREADY_IMPORTED,
                    "that note is imported already",
                ));
            }
            Some(note) => note,
            None => Id::from(tilo_resource::fresh_uuid().to_string()),
        };
        for picture in &images {
            self.store_image(picture)?;
        }
        let page = Page::titled(&title, None).made_by_invoker();
        let path = path
            .map(|path| crate::markdown::normal_path(&path))
            .filter(|path| !path.is_empty());
        let blocks = seed(
            &markdown,
            &id,
            &page.title,
            path.as_deref(),
            &HashMap::new(),
            &reads.links,
        );
        self.pages.push(
            id,
            Page {
                body: written(&blocks),
                origin,
                path,
                rank: reads.rank,
                ..page
            },
        );
        Ok(())
    }

    /// Import Markdown files
    ///
    /// Add several Markdown files as top-level notes in one step, each
    /// titled by its `title`, its first `# heading` or its file name. Links
    /// between them by their paths (`setup.md`, `../guide/setup.md#install`)
    /// lead to the notes they become, and to the heading an anchor names; a
    /// link to a file outside the set keeps its words.
    #[action(audience = "agents", writes = pages)]
    pub fn import_markdown_files(
        &mut self,
        cx: &mut ActionCx,
        #[param(
            "The files: each its path in the set, its Markdown, and optionally a title and an origin"
        )]
        files: Vec<MarkdownFile>,
        #[param("Their places among the top-level notes, in order: filled in where it is invoked")]
        ranks: Option<Vec<String>>,
        #[param("The notes their links lead to: filled in where it is invoked")] links: Option<
            Links,
        >,
    ) -> Result<(), Refusal> {
        if files.is_empty() {
            return Err(Refusal::new("give the files to import"));
        }
        let links = links.unwrap_or_default();
        let ranks = ranks.unwrap_or_default();
        let mut ids = HashMap::new();
        let mut planned = Vec::with_capacity(files.len());
        for file in files {
            check_markdown_import(&file.markdown).map_err(Refusal::new)?;
            let path = crate::markdown::normal_path(&file.path);
            if path.is_empty() {
                return Err(Refusal::new("a file has a path in its set"));
            }
            if ids.contains_key(&path) {
                return Err(Refusal::new(format!("{path} is in the set twice")));
            }
            let origin = Origin::checked_option(file.origin.clone())?;
            let id = Id::from(cx.uuid().to_string());
            ids.insert(path.clone(), id.clone());
            planned.push((id, path, file.title(), file.markdown, origin));
        }
        for (at, (id, path, title, markdown, origin)) in planned.into_iter().enumerate() {
            let markdown = crate::markdown::strip_matching_title_heading(&markdown, &title);
            let page = Page::titled(&title, None).made(cx);
            let blocks = seed(&markdown, &id, &page.title, Some(&path), &ids, &links);
            self.pages.push(
                id,
                Page {
                    body: written(&blocks),
                    origin,
                    path: Some(path),
                    rank: ranks.get(at).cloned().unwrap_or_default(),
                    ..page
                },
            );
        }
        Ok(())
    }

    /// Add every page of a set brought from another tool as a top-level
    /// note, in one step: one undo takes the whole import back.
    #[action(by_app, step = "create", writes = pages)]
    pub fn import_notes(
        &mut self,
        cx: &mut ActionCx,
        #[param("The pages")] notes: Vec<ImportedNote>,
        #[param("Their places among the top-level notes, in order")] ranks: Option<Vec<String>>,
        #[param("The notes their links lead to")] links: Option<Links>,
    ) -> Result<(), Refusal> {
        let links = links.unwrap_or_default();
        let ranks = ranks.unwrap_or_default();
        for (at, note) in notes.into_iter().enumerate() {
            check_markdown_import(&note.markdown).map_err(Refusal::new)?;
            for picture in &note.images {
                self.store_image(picture)?;
            }
            let id = Id::from(cx.uuid().to_string());
            let page = Page::titled(&note.title, None).made(cx);
            let blocks = seed(
                &note.markdown,
                &id,
                &page.title,
                None,
                &HashMap::new(),
                &links,
            );
            self.pages.push(
                id,
                Page {
                    body: written(&blocks),
                    rank: ranks.get(at).cloned().unwrap_or_default(),
                    ..page
                },
            );
        }
        Ok(())
    }

    /// Append to note
    ///
    /// Add Markdown at the end of a note: headings, bullets, `- [ ]`
    /// checklists, quotes, code, and links to its headings and other notes. The note is the one
    /// titled `title` (case and spacing aside); with no title, the one note
    /// you made. An agent names notes by the titles it reads, so it need
    /// not look the note's id up first; answers the note's id and resource.
    #[action(answers, audience = "agents", step = "write", writes = pages)]
    pub fn append_to_note(
        &mut self,
        #[param("The note's title, case and spacing aside; the one note you made when omitted")]
        title: Option<String>,
        #[param("What to add, as Markdown")] markdown: String,
        #[param("The note it names: filled in where it is invoked")] note: Option<Id>,
        #[param("The notes its links lead to: filled in where it is invoked")] links: Option<Links>,
    ) -> Result<(), Refusal> {
        let _ = title;
        check_markdown_import(&markdown).map_err(Refusal::new)?;
        if markdown.trim().is_empty() {
            return Err(Refusal::new("give the Markdown to add"));
        }
        let id = note.ok_or_else(|| Refusal::new("name the note by its title"))?;
        let page = self.pages.get(&id).ok_or_else(no_page)?;
        let (title, path) = (page.title.clone(), page.path.clone());
        // Read as every other Markdown a note takes in: wrapped lines are
        // one paragraph, and links to its headings and to other notes lead
        // there.
        let blocks = seed(
            &markdown,
            &id,
            &title,
            path.as_deref(),
            &HashMap::new(),
            &links.unwrap_or_default(),
        );
        let clips: Vec<ClipBlock> = blocks.iter().map(tilo_rich_text::clip_of_content).collect();
        let views: Vec<_> = clips.iter().map(ClipBlock::to_view).collect();
        // Written as lines the text is the invoker's in, where every block
        // has one; a table or a picture is placed as it is.
        self.page_mut(&id)?
            .body
            .perform(match tilo_document::block_markdown::lines_of(&views) {
                Some(markdown) => RichTextAction::InsertMarkdown {
                    at: tilo_document::block_edit::Placement::End,
                    markdown,
                },
                None => RichTextAction::SyncClips {
                    blocks: clips,
                    replace: Some(Vec::new()),
                    at: usize::MAX,
                },
            });
        Ok(())
    }

    /// Write note
    ///
    /// Make the note read as `markdown`, as the fewest edits: a paragraph
    /// that already reads so stays as it is, a changed one is edited in
    /// place, and the rest are added or removed. What the note's file in the
    /// workspace's folder was written to is applied this way, as the change
    /// of whoever wrote the file.
    #[action(item = pages, answers, audience = "agents", step = "write", writes = pages)]
    pub fn write_note(
        &mut self,
        page: &Id,
        #[param("The note's whole body, as Markdown")] markdown: String,
        #[param("The notes its links lead to: filled in where it is invoked")] links: Option<Links>,
    ) -> Result<(), Refusal> {
        check_markdown_import(&markdown).map_err(Refusal::new)?;
        let note = self.pages.get(page).ok_or_else(no_page)?;
        let (title, path) = (note.title.clone(), note.path.clone());
        let markdown = crate::markdown::strip_matching_title_heading(&markdown, &title);
        let blocks = seed(
            &markdown,
            page,
            &title,
            path.as_deref(),
            &HashMap::new(),
            &links.unwrap_or_default(),
        );
        let clips: Vec<ClipBlock> = blocks.iter().map(tilo_rich_text::clip_of_content).collect();
        self.page_mut(page)?
            .body
            .perform(RichTextAction::SyncClips {
                blocks: clips,
                replace: None,
                at: 0,
            });
        Ok(())
    }
}

impl NoteList {
    /// Put `page` where `place` says.
    fn put(&mut self, page: &Id, place: Option<Placement>) -> Result<(), Refusal> {
        let place = placed(place)?;
        if place.parent.as_ref() == Some(page) {
            return Err(Refusal::new("a note cannot go under itself"));
        }
        let note = self.page_mut(page)?;
        note.parent = place.parent;
        note.rank = place.rank;
        Ok(())
    }

    /// What a note-creating action answers with: the id and resource address
    /// of the note it made, so its caller can act on it without a second
    /// read. A new note is a new entry of the note collection, keyed by its
    /// id; edits only touch fields of notes that exist.
    fn created_page(
        doc: &tilo_resource::Doc,
        action: &str,
        changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        use automerge::ReadDoc as _;
        // Only the notes the change names are looked up, never the whole
        // part read: a note-creating action costs what it does in a part of
        // ten notes in one of hundreds.
        let exists = |page: PageId| {
            NoteList::paths()
                .pages
                .item_path(&page_id(page))
                .object(doc)
                .is_some()
        };
        let answer = |page: PageId| {
            serde_json::json!({
                "page": page.to_string(),
                "resource": crate::actions::page_resource_id(page),
            })
        };
        // Appending names its note by title: answer the note it found.
        if action == "append-to-note" {
            return changes.iter().find_map(|patch| {
                patch.path.iter().find_map(|(_, prop)| {
                    let automerge::Prop::Map(key) = prop else {
                        return None;
                    };
                    let page = page_of(&Id::from(key.as_str()))?;
                    exists(page).then(|| answer(page))
                })
            });
        }
        // A block action of a note's body answers the blocks it touched.
        if tilo_document::block_actions::ANSWERING.contains(&action) {
            let pages = NoteList::paths().pages.object(doc)?;
            return doc.keys(&pages).find_map(|key| {
                let body = body_object(doc, page_of(&Id::from(key.as_str()))?)?;
                let answer =
                    tilo_document::block_actions::action_result_under(doc, &body, action, changes)?;
                answer
                    .as_object()
                    .is_some_and(|touched| !touched.is_empty())
                    .then_some(answer)
            });
        }
        let made: Vec<PageId> = changes
            .iter()
            .filter_map(|patch| {
                let automerge::PatchAction::PutMap {
                    key,
                    value: (automerge::Value::Object(_), _),
                    ..
                } = &patch.action
                else {
                    return None;
                };
                page_of(&Id::from(key.as_str())).filter(|page| exists(*page))
            })
            .collect();
        // A copy of a note and those under it answers the copy of the note:
        // the one made whose parent was not made with it.
        if action == "duplicate" {
            let part = <NoteList as tilo_resource::Resource>::read(doc).ok()?;
            return made
                .iter()
                .copied()
                .find(|page| {
                    part.pages
                        .get(&page_id(*page))
                        .and_then(|note| note.parent.as_ref())
                        .and_then(page_of)
                        .is_none_or(|parent| !made.contains(&parent))
                })
                .map(answer);
        }
        made.first().copied().map(answer)
    }

    /// One note as a reader reads it: its title and blocks, one Markdown
    /// line per block in order, each with the id edits of its body name it
    /// by; a note's body resource as those blocks. The part itself reads as
    /// its notes.
    fn read_note(
        &self,
        doc: &tilo_resource::Doc,
        item: Option<&Id>,
    ) -> Result<serde_json::Value, String> {
        let note =
            |id: &Id| page_of(id).map_or_else(|| id.to_string(), crate::actions::page_resource_id);
        let Some(item) = item else {
            return Ok(serde_json::json!({
                "kind": <Self as tilo_resource::Resource>::KIND,
                "pages": self.pages.iter().map(|(id, page)| serde_json::json!({
                    "item": note(id),
                    "title": page.title,
                    "parent": page.parent.as_ref().map(note),
                })).collect::<Vec<_>>(),
            }));
        };
        if let Some(page) = crate::actions::page_of_body_resource(item.as_str()) {
            let body = body_object(doc, page).ok_or_else(|| format!("no note `{item}`"))?;
            let mut read = tilo_document::block_actions::read_under(doc, &body, None)?;
            if let Some(map) = read.as_object_mut() {
                map.insert(
                    "kind".into(),
                    <RichText as tilo_resource::Resource>::KIND.into(),
                );
            }
            return Ok(read);
        }
        let id =
            crate::actions::page_of_resource(item.as_str()).map_or_else(|| item.clone(), page_id);
        let page = self
            .pages
            .get(&id)
            .ok_or_else(|| format!("no note `{item}`"))?;
        let body = page_of(&id)
            .and_then(|page| body_object(doc, page))
            .ok_or_else(|| format!("note `{item}` has no body"))?;
        let read = tilo_document::block_actions::read_under(doc, &body, None)?;
        Ok(serde_json::json!({
            "kind": "notes.page",
            "item": note(&id),
            "title": page.title,
            "parent": page.parent.as_ref().map(note),
            "body": page_of(&id).map(crate::actions::body_resource_id),
            "markdown": read.get("markdown"),
            "blocks": read.get("blocks"),
        }))
    }
}

/// The object note `page` is kept in, in a part's `doc`, when the part
/// keeps it: its entry in the part's notes.
#[must_use]
pub fn page_object(doc: &tilo_resource::Doc, page: PageId) -> Option<automerge::ObjId> {
    use automerge::ReadDoc as _;
    let pages = NoteList::paths().pages.object(doc)?;
    match doc.get(&pages, page_id(page).as_str()) {
        Ok(Some((automerge::Value::Object(_), entry))) => Some(entry),
        _ => None,
    }
}

/// The object note `page`'s body is kept in, in a part's `doc`.
#[must_use]
pub fn body_object(doc: &tilo_resource::Doc, page: PageId) -> Option<automerge::ObjId> {
    NoteList::paths()
        .pages
        .item(&page_id(page))
        .body
        .object(doc)
}

/// Every note the held parts keep, read as one notebook: the notes shown
/// (none whose parent, or a note above it, is gone), each found by its id
/// and the part keeping it, the notes beside each other in their order.
///
/// Built from the parts once per change of any of them, and read by the
/// sidebar, search, an agent's read, the folder the notes are files in and
/// the checks an action makes where it is invoked.
pub struct Pages<'a> {
    /// Every note shown, in the notebook's order: each before the notes
    /// under it, the notes beside each other by rank, then id.
    ordered: Vec<(&'a Id, &'a Page)>,
    /// Where each note shown is in `ordered`, and the part that keeps it.
    at: HashMap<&'a Id, (usize, u64)>,
    /// The note each note shown stands under; none at the top.
    parents: HashMap<&'a Id, &'a Id>,
    /// The notes at the top, and those under each note, by where they are
    /// in `ordered`.
    top: Vec<usize>,
    under: HashMap<&'a Id, Vec<usize>>,
}

/// The notes held, by id, with the part keeping each.
type Found<'a> = HashMap<&'a Id, (&'a Page, u64)>;

impl<'a> Pages<'a> {
    /// The notes `parts` keep, oldest part first. A note two parts both
    /// claim (never written so, but read safely) is the newer part's.
    #[must_use]
    pub fn of(parts: &'a [(u64, Rc<NoteList>)]) -> Self {
        let mut found: Found<'a> = HashMap::new();
        for (index, part) in parts {
            for (id, page) in part.pages.iter() {
                found.insert(id, (page, *index));
            }
        }
        // The notes under each note: a note whose parent is gone is under
        // nothing shown, and neither is anything under it.
        let mut children: HashMap<Option<&'a Id>, Vec<(&'a Id, &'a Page)>> = HashMap::new();
        for (id, (page, _)) in &found {
            match page.parent.as_ref() {
                None => children.entry(None).or_default().push((*id, *page)),
                Some(parent) => {
                    if let Some((parent, _)) = found.get_key_value(parent) {
                        children
                            .entry(Some(*parent))
                            .or_default()
                            .push((*id, *page));
                    }
                }
            }
        }
        for siblings in children.values_mut() {
            siblings.sort_by(|(a_id, a), (b_id, b)| {
                a.rank
                    .cmp(&b.rank)
                    .then_with(|| a_id.as_str().cmp(b_id.as_str()))
            });
        }
        let mut pages = Self {
            ordered: Vec::with_capacity(found.len()),
            at: HashMap::with_capacity(found.len()),
            parents: HashMap::new(),
            top: Vec::new(),
            under: HashMap::new(),
        };
        let roots = children.get(&None).cloned().unwrap_or_default();
        for root in roots {
            pages.walk(root, &children, &found);
        }
        // A cycle two writers' moves made stands at the top: each note of it
        // nothing above reaches, by id.
        let mut cyclic: Vec<&'a Id> = found
            .keys()
            .filter(|id| !pages.at.contains_key(*id) && in_cycle(&found, id))
            .copied()
            .collect();
        cyclic.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for id in cyclic {
            if let Some((page, _)) = found.get(id) {
                pages.walk((id, *page), &children, &found);
            }
        }
        pages
    }

    /// Show `root` at the top, and every note under it not shown yet.
    fn walk(
        &mut self,
        root: (&'a Id, &'a Page),
        children: &HashMap<Option<&'a Id>, Vec<(&'a Id, &'a Page)>>,
        found: &Found<'a>,
    ) {
        if self.at.contains_key(root.0) {
            return;
        }
        let part = |id: &Id| found.get(id).map_or(0, |(_, part)| *part);
        self.top.push(self.ordered.len());
        self.at.insert(root.0, (self.ordered.len(), part(root.0)));
        self.ordered.push(root);
        let mut stack: Vec<(&'a Id, usize)> = vec![(root.0, 0)];
        while let Some((parent, next)) = stack.pop() {
            let Some(child) = children
                .get(&Some(parent))
                .and_then(|list| list.get(next))
                .copied()
            else {
                continue;
            };
            stack.push((parent, next.saturating_add(1)));
            if self.at.contains_key(child.0) {
                continue;
            }
            self.under
                .entry(parent)
                .or_default()
                .push(self.ordered.len());
            self.parents.insert(child.0, parent);
            self.at.insert(child.0, (self.ordered.len(), part(child.0)));
            self.ordered.push(child);
            stack.push((child.0, 0));
        }
    }

    /// How many notes are shown.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    /// Whether none are.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    /// Every note shown, in the notebook's order: each before the notes
    /// under it.
    pub fn iter(&self) -> impl Iterator<Item = (&'a Id, &'a Page)> + '_ {
        self.ordered.iter().copied()
    }

    /// The note `id`, while it is shown.
    #[must_use]
    pub fn get(&self, id: &Id) -> Option<&'a Page> {
        self.at
            .get(id)
            .and_then(|(position, _)| self.ordered.get(*position))
            .map(|(_, page)| *page)
    }

    /// Whether `id` is a note shown.
    #[must_use]
    pub fn contains(&self, id: &Id) -> bool {
        self.at.contains_key(id)
    }

    /// The part that keeps note `id`, while it is shown.
    #[must_use]
    pub fn part_of(&self, id: &Id) -> Option<u64> {
        self.at.get(id).map(|(_, part)| *part)
    }

    /// The note `page` stands under, while both are shown; `None` at the
    /// top (a note of a cycle stands there too).
    #[must_use]
    pub fn parent_of(&self, page: &Id) -> Option<&'a Id> {
        self.parents.get(page).copied()
    }

    /// The notes directly under `parent` (the top-level notes for `None`),
    /// in their order.
    pub fn children<'s>(
        &'s self,
        parent: Option<&'s Id>,
    ) -> impl Iterator<Item = (&'a Id, &'a Page)> + 's {
        let positions = match parent {
            None => self.top.as_slice(),
            Some(parent) => self.under.get(parent).map_or(&[][..], Vec::as_slice),
        };
        positions
            .iter()
            .filter_map(|position| self.ordered.get(*position).copied())
    }

    /// Whether `page` is `ancestor` or sits somewhere under it.
    #[must_use]
    pub fn is_under(&self, page: &Id, ancestor: &Id) -> bool {
        let mut at = Some(page);
        for _ in 0..=self.ordered.len() {
            match at {
                Some(id) if id == ancestor => return true,
                Some(id) => at = self.parent_of(id),
                None => return false,
            }
        }
        false
    }

    /// The note `page` and every note shown under it, each before the notes
    /// under it, in the notebook's order.
    #[must_use]
    pub fn subtree(&self, page: &Id) -> Vec<&'a Id> {
        let Some((start, _)) = self.at.get(page) else {
            return Vec::new();
        };
        let Some((first, _)) = self.ordered.get(*start) else {
            return Vec::new();
        };
        let mut ordered = vec![*first];
        let mut at = 0;
        while let Some(parent) = ordered.get(at).copied() {
            ordered.extend(self.children(Some(parent)).map(|(id, _)| id));
            at += 1;
        }
        ordered
    }

    /// The rank after every note's directly under `parent`.
    #[must_use]
    pub fn last_rank(&self, parent: Option<&Id>) -> String {
        let last = self
            .children(parent)
            .map(|(_, page)| page.rank.as_str())
            .filter(|rank| !rank.is_empty())
            .max();
        tilo_resource::order::between(last, None)
    }

    /// The rank of a note placed under `parent` just before `before`,
    /// `moving` left out of where it stands now; after the notes there when
    /// `before` is none of them.
    #[must_use]
    pub fn rank_before(&self, parent: Option<&Id>, before: Option<&Id>, moving: &Id) -> String {
        let siblings: Vec<(&Id, &Page)> = self
            .children(parent)
            .filter(|(id, _)| *id != moving)
            .collect();
        let Some(at) = before.and_then(|before| siblings.iter().position(|(id, _)| *id == before))
        else {
            let last = siblings
                .iter()
                .map(|(_, page)| page.rank.as_str())
                .filter(|rank| !rank.is_empty())
                .max();
            return tilo_resource::order::between(last, None);
        };
        let high = siblings
            .get(at)
            .map(|(_, page)| page.rank.as_str())
            .filter(|rank| !rank.is_empty());
        let low = at
            .checked_sub(1)
            .and_then(|below| siblings.get(below))
            .map(|(_, page)| page.rank.as_str())
            .filter(|rank| !rank.is_empty());
        match (low, high) {
            // Equal ranks (notes made at once) leave no room between them:
            // it goes after the lower one.
            (Some(low), Some(high)) if low >= high => {
                tilo_resource::order::between(Some(low), None)
            }
            (low, high) => tilo_resource::order::between(low, high),
        }
    }

    /// The note titled `title`: an exact title first, else the one title
    /// equal with case and spacing aside; refused, naming the notes, when
    /// none or several are. Never another note in its place: an append
    /// that lands on the wrong note reads as done.
    ///
    /// # Errors
    /// Refuses a blank title, and a title no note or several notes have.
    pub fn titled(&self, title: &str) -> Result<&'a Id, String> {
        let wanted = title.trim();
        if wanted.is_empty() {
            return Err("name the note by its title".into());
        }
        if let Some((id, _)) = self.iter().find(|(_, page)| page.name() == wanted) {
            return Ok(id);
        }
        let wanted_folded = folded(wanted);
        let alike: Vec<(&Id, &Page)> = self
            .iter()
            .filter(|(_, page)| folded(&page.name()) == wanted_folded)
            .collect();
        match alike.as_slice() {
            [(id, _)] => Ok(id),
            [] => Err(format!(
                "no note is titled “{wanted}”; the notes are {}",
                titles(&self.iter().map(|(_, page)| page).collect::<Vec<_>>())
            )),
            several => Err(format!(
                "several notes are titled {}: give its exact title",
                titles(&several.iter().map(|(_, page)| *page).collect::<Vec<_>>())
            )),
        }
    }

    /// The one note `author` made, for an append that names none. The note
    /// open in a window, or the first, is someone's but not necessarily the
    /// caller's, so with none or several the caller names one.
    ///
    /// # Errors
    /// Refuses when `author` made no note or several.
    pub fn made_by(&self, author: Option<&MemberId>) -> Result<&'a Id, String> {
        let made: Vec<(&Id, &Page)> = self
            .iter()
            .filter(|(_, page)| author.is_some_and(|author| page.creator.as_ref() == Some(author)))
            .collect();
        match made.as_slice() {
            [(id, _)] => Ok(id),
            [] => Err(format!(
                "name the note by its title: you made none here; the notes are {}",
                titles(&self.iter().map(|(_, page)| page).collect::<Vec<_>>())
            )),
            several => Err(format!(
                "name the note by its title: you made {}",
                titles(&several.iter().map(|(_, page)| *page).collect::<Vec<_>>())
            )),
        }
    }

    /// The note `title` names, as a `[[Title]]` link finds it: the first
    /// note of exactly that title.
    #[must_use]
    pub fn linked(&self, title: &str) -> Option<&'a Id> {
        self.iter()
            .find(|(_, page)| page.name() == title)
            .map(|(id, _)| id)
    }

    /// The note imported from the file at `path`.
    #[must_use]
    pub fn at_path(&self, path: &str) -> Option<&'a Id> {
        self.iter()
            .find(|(_, page)| page.path.as_deref() == Some(path))
            .map(|(id, _)| id)
    }
}

/// Whether `page`'s chain of parents, among `found`, comes back to itself.
fn in_cycle(found: &Found<'_>, page: &Id) -> bool {
    let mut current = found.get(page).and_then(|(note, _)| note.parent.as_ref());
    for _ in 0..found.len() {
        match current {
            None => return false,
            Some(id) if id == page => return true,
            Some(id) => current = found.get(id).and_then(|(note, _)| note.parent.as_ref()),
        }
    }
    current.is_some()
}

/// The most titles a refusal names, so it stays one readable line.
const MOST_TITLES: usize = 20;

/// A title as notes are found by it: case aside, any run of spaces as one.
fn folded(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `pages`' titles as a refusal names them.
fn titles(pages: &[&Page]) -> String {
    if pages.is_empty() {
        return "none yet".to_owned();
    }
    let more = pages.len().saturating_sub(MOST_TITLES);
    let mut named: Vec<String> = pages
        .iter()
        .take(MOST_TITLES)
        .map(|page| format!("“{}”", page.name()))
        .collect();
    if more > 0 {
        named.push(format!("and {more} more"));
    }
    named.join(", ")
}

/// The notes the links of `markdown`, kept as the file at `path`, lead to
/// among `pages`: each `[[Title]]` and each file linked by its path that a
/// note shown is.
fn links_of(markdown: &str, path: Option<&str>, pages: &Pages<'_>) -> Links {
    let mut links = Links::default();
    let mut specs = crate::markdown::parse(markdown);
    crate::markdown::resolve_link_ids(&mut specs, |target| {
        let id = pages.linked(target)?;
        links.titles.insert(target.to_owned(), id.clone());
        page_of(id)
    });
    crate::markdown::resolve_relative_links(&mut specs, |link| {
        let (file, _) = crate::markdown::relative_target(path, link)?;
        let file = file?;
        let id = pages.at_path(&file)?;
        links.paths.insert(file, id.clone());
        None
    });
    links
}

/// The copies of the note `page` and every note shown under it, read from
/// `pages`: each under a new id, its links to a copied note leading to the
/// copy, the copied note itself right after the original.
fn copies_of(page: &Id, pages: &Pages<'_>) -> Result<Vec<PageCopy>, String> {
    let source = pages.get(page).ok_or("no such note")?;
    let sources = pages.subtree(page);
    let copies: HashMap<&Id, Id> = sources
        .iter()
        .map(|id| (*id, Id::from(uuid::Uuid::new_v4().to_string())))
        .collect();
    let follow = |page: PageId| copies.get(&page_id(page)).and_then(page_of).unwrap_or(page);
    // Right after the original, before the note that follows it.
    let parent = pages.parent_of(page);
    let after = pages
        .children(parent)
        .map(|(id, _)| id)
        .skip_while(|id| *id != page)
        .nth(1);
    let first_rank = {
        let next = after
            .and_then(|next| pages.get(next))
            .map(|next| next.rank.as_str())
            .filter(|rank| !rank.is_empty());
        let own = Some(source.rank.as_str()).filter(|rank| !rank.is_empty());
        match (own, next) {
            (Some(low), Some(high)) if low >= high => {
                tilo_resource::order::between(Some(low), None)
            }
            (low, high) => tilo_resource::order::between(low, high),
        }
    };
    let mut made = Vec::with_capacity(sources.len());
    for id in sources {
        let (Some(original), Some(copy)) = (pages.get(id), copies.get(id)) else {
            continue;
        };
        let mut blocks = original.blocks();
        for block in &mut blocks {
            // A copied fence is a fence of its own: its group follows the
            // copy.
            if let Some(group) = &block.code_group {
                let digest = blake3::hash(format!("{copy}/{group}").as_bytes());
                let bytes = digest
                    .as_bytes()
                    .first_chunk::<16>()
                    .copied()
                    .unwrap_or_default();
                block.code_group = Some(uuid::Uuid::from_bytes(bytes).to_string());
            }
            for span in &mut block.marks {
                span.mark = match &span.mark {
                    Mark::Link(page) => Mark::Link(follow(*page)),
                    Mark::Transclusion(page) => Mark::Transclusion(follow(*page)),
                    other => other.clone(),
                };
            }
        }
        let is_root = id == page;
        made.push(PageCopy {
            id: copy.clone(),
            title: if is_root {
                format!("{} copy", original.title)
            } else {
                original.title.clone()
            },
            parent: if is_root {
                parent.cloned()
            } else {
                original
                    .parent
                    .as_ref()
                    .and_then(|parent| copies.get(parent).cloned())
            },
            rank: if is_root {
                first_rank.clone()
            } else {
                original.rank.clone()
            },
            blocks: blocks.iter().map(tilo_rich_text::clip_of_content).collect(),
            tags: original.tags.iter().cloned().collect(),
        });
    }
    Ok(made)
}

/// Fill in, where `actions` are invoked together, what each reads of the
/// notebook's tags and of every note held there, and refuse what they do not
/// allow: a parent that is not a note shown, a note moved under itself, a tag
/// the notebook has none of. The notes are read once for all of them.
///
/// # Errors
/// Refuses as the action's verb would, had it every part.
pub fn prepare(
    actions: &mut [<NoteList as tilo_resource::Resource>::Action],
    notebook: &Notebook,
    parts: &[(u64, Rc<NoteList>)],
    author: Option<&MemberId>,
) -> Result<(), String> {
    // A field edit reads nothing across parts: typing costs its part.
    if actions
        .iter()
        .all(|action| matches!(&action.operation, NoteListAction::FieldEdit(_)))
    {
        return Ok(());
    }
    let pages = Pages::of(parts);
    let mut ends = Ends::default();
    for action in actions {
        prepare_one(action, notebook, parts, (&pages, &mut ends), author)?;
    }
    Ok(())
}

/// The ranks after every note under each parent, one after the other: where
/// the notes that actions invoked together add under a parent go, each after
/// the one before it.
#[derive(Default)]
struct Ends(HashMap<Option<Id>, String>);

impl Ends {
    /// The rank after every note under `parent` and every rank given there
    /// before.
    fn next(&mut self, pages: &Pages<'_>, parent: Option<&Id>) -> String {
        let next = match self.0.get(&parent.cloned()) {
            Some(given) => tilo_resource::order::between(Some(given), None),
            None => pages.last_rank(parent),
        };
        self.0.insert(parent.cloned(), next.clone());
        next
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per verb that reads across parts"
)]
fn prepare_one(
    action: &mut <NoteList as tilo_resource::Resource>::Action,
    notebook: &Notebook,
    parts: &[(u64, Rc<NoteList>)],
    (pages, ends): (&Pages<'_>, &mut Ends),
    author: Option<&MemberId>,
) -> Result<(), String> {
    use note_list as verb;
    if matches!(&action.operation, NoteListAction::FieldEdit(_)) {
        return Ok(());
    }
    let shown_parent = |parent: Option<&Id>| -> Result<Option<Id>, String> {
        match parent {
            None => Ok(None),
            Some(parent) => {
                let parent = note_key(parent.clone());
                if pages.contains(&parent) {
                    Ok(Some(parent))
                } else {
                    Err("the note it goes under is gone".into())
                }
            }
        }
    };
    let shown = |page: &Id| -> Result<(), String> {
        if pages.contains(page) {
            Ok(())
        } else {
            Err("no such note".into())
        }
    };
    match &mut action.operation {
        NoteListAction::NewNote(verb::NewNote {
            parent, rank, note, ..
        }) => {
            // The note is named where it is made, so the step names the note it made.
            note.get_or_insert_with(|| crate::notebook::page_id(PageId::new_v4()));
            let under = shown_parent(parent.as_ref())?;
            *rank = Some(ends.next(pages, under.as_ref()));
            *parent = under;
        }
        NoteListAction::NewChild(verb::NewChild { page, rank, .. }) => {
            shown(page)?;
            *rank = Some(ends.next(pages, Some(page)));
        }
        NoteListAction::CreatePage(verb::CreatePage {
            markdown,
            rank,
            links,
            ..
        }) => {
            *rank = Some(ends.next(pages, None));
            *links = Some(links_of(
                markdown.as_deref().unwrap_or_default(),
                None,
                pages,
            ));
        }
        NoteListAction::ImportMarkdown(verb::ImportMarkdown {
            markdown,
            note,
            path,
            reads,
            ..
        }) => {
            let path = path
                .as_deref()
                .map(crate::markdown::normal_path)
                .filter(|path| !path.is_empty());
            *reads = Some(ImportReads {
                rank: ends.next(pages, None),
                links: links_of(markdown, path.as_deref(), pages),
                imported: note
                    .as_ref()
                    .is_some_and(|note| parts.iter().any(|(_, part)| part.pages.contains(note))),
            });
        }
        NoteListAction::ImportMarkdownFiles(verb::ImportMarkdownFiles {
            files,
            ranks,
            links,
        }) => {
            let mut placed = Vec::with_capacity(files.len());
            let mut found = Links::default();
            for file in files.iter() {
                placed.push(ends.next(pages, None));
                let path = crate::markdown::normal_path(&file.path);
                let of = links_of(&file.markdown, Some(&path), pages);
                found.titles.extend(of.titles);
                found.paths.extend(of.paths);
            }
            *ranks = Some(placed);
            *links = Some(found);
        }
        NoteListAction::ImportNotes(verb::ImportNotes {
            notes,
            ranks,
            links,
        }) => {
            let mut placed = Vec::with_capacity(notes.len());
            let mut found = Links::default();
            for note in notes.iter() {
                placed.push(ends.next(pages, None));
                found
                    .titles
                    .extend(links_of(&note.markdown, None, pages).titles);
            }
            *ranks = Some(placed);
            *links = Some(found);
        }
        NoteListAction::AppendToNote(verb::AppendToNote {
            title,
            markdown,
            note,
            links,
        }) => {
            let id = match title.as_deref().map(str::trim) {
                Some(title) if !title.is_empty() => pages.titled(title)?,
                _ => pages.made_by(author)?,
            };
            let page = pages.get(id).ok_or("no such note")?;
            *links = Some(links_of(markdown, page.path.as_deref(), pages));
            *note = Some(id.clone());
        }
        NoteListAction::WriteNote(verb::WriteNote {
            page,
            markdown,
            links,
        }) => {
            let path = pages.get(page).and_then(|page| page.path.clone());
            *links = Some(links_of(markdown, path.as_deref(), pages));
        }
        NoteListAction::MoveNote(verb::MoveNote {
            page,
            parent,
            before,
            place,
        }) => {
            shown(page)?;
            let parent = shown_parent(parent.as_ref())?;
            if let Some(parent) = &parent
                && pages.is_under(parent, page)
            {
                return Err("a note cannot go under itself".into());
            }
            let before = before.clone().map(note_key);
            *place = Some(Placement {
                rank: pages.rank_before(parent.as_ref(), before.as_ref(), page),
                parent,
            });
        }
        NoteListAction::MoveUp(verb::MoveUp { page, place }) => {
            let parent = pages.parent_of(page);
            let siblings: Vec<&Id> = pages.children(parent).map(|(id, _)| id).collect();
            let at = siblings
                .iter()
                .position(|id| *id == page)
                .ok_or("no such note")?;
            let above = at
                .checked_sub(1)
                .and_then(|at| siblings.get(at))
                .ok_or("the note is first already")?;
            *place = Some(Placement {
                rank: pages.rank_before(parent, Some(above), page),
                parent: parent.cloned(),
            });
        }
        NoteListAction::MoveDown(verb::MoveDown { page, place }) => {
            let parent = pages.parent_of(page);
            let siblings: Vec<&Id> = pages.children(parent).map(|(id, _)| id).collect();
            let at = siblings
                .iter()
                .position(|id| *id == page)
                .ok_or("no such note")?;
            if at.saturating_add(1) >= siblings.len() {
                return Err("the note is last already".into());
            }
            let after_next = siblings.get(at.saturating_add(2)).copied();
            *place = Some(Placement {
                rank: pages.rank_before(parent, after_next, page),
                parent: parent.cloned(),
            });
        }
        NoteListAction::Indent(verb::Indent { page, place }) => {
            let parent = pages.parent_of(page);
            let siblings: Vec<&Id> = pages.children(parent).map(|(id, _)| id).collect();
            let at = siblings
                .iter()
                .position(|id| *id == page)
                .ok_or("no such note")?;
            let above = at
                .checked_sub(1)
                .and_then(|at| siblings.get(at))
                .ok_or("no note above to go under")?;
            *place = Some(Placement {
                rank: ends.next(pages, Some(above)),
                parent: Some((*above).clone()),
            });
        }
        NoteListAction::Outdent(verb::Outdent { page, place }) => {
            shown(page)?;
            let parent = pages
                .parent_of(page)
                .ok_or("the note is at the top level already")?;
            let grandparent = pages.parent_of(parent);
            let next = pages
                .children(grandparent)
                .map(|(id, _)| id)
                .skip_while(|id| *id != parent)
                .nth(1);
            *place = Some(Placement {
                rank: pages.rank_before(grandparent, next, page),
                parent: grandparent.cloned(),
            });
        }
        NoteListAction::Duplicate(verb::Duplicate { page, copies }) => {
            *copies = Some(copies_of(page, pages)?);
        }
        NoteListAction::Tag(verb::Tag { page, name, tag }) => {
            shown(page)?;
            let name = name.trim();
            *tag = Some(notebook.tag_named(name).ok_or_else(|| {
                format!("the notebook has no tag named “{name}”: make it with new-tag first")
            })?);
        }
        _ => {}
    }
    Ok(())
}

/// What renaming a note writes into the other parts: the links to it
/// relabelled in each part holding one.
#[must_use]
pub fn spread(
    action: &<NoteList as tilo_resource::Resource>::Action,
    index: u64,
    parts: &[(u64, Rc<NoteList>)],
) -> Vec<(u64, <NoteList as tilo_resource::Resource>::Action)> {
    let NoteListAction::Rename(note_list::Rename { page, title }) = &action.operation else {
        return Vec::new();
    };
    let Some(target) = page_of(page) else {
        return Vec::new();
    };
    parts
        .iter()
        .filter(|(held, part)| *held != index && part.links_to(target))
        .map(|(held, _)| {
            (
                *held,
                tilo_resource::Stamped {
                    operation: NoteListAction::RelabelLinks(note_list::RelabelLinks {
                        title: title.trim().to_owned(),
                        target: page.clone(),
                    }),
                    now_ms: action.now_ms,
                },
            )
        })
        .collect()
}

/// The notes `parts` keep that are not shown, by the resources they are
/// published as: each whose parent, or a note above it, is gone.
#[must_use]
pub fn hidden(parts: &[(u64, Rc<NoteList>)]) -> HashSet<String> {
    let pages = Pages::of(parts);
    parts
        .iter()
        .flat_map(|(_, part)| part.pages.ids())
        .filter(|id| !pages.contains(id))
        .map(|id| tilo_resource::items::item_resource_id::<NoteList>("pages", id))
        .collect()
}

/// The pictures every held part keeps, by the digest the notes' bodies
/// name them by.
#[must_use]
pub fn images(parts: &[(u64, Rc<NoteList>)]) -> Map<Bytes> {
    let mut images = Map::new();
    for (_, part) in parts {
        for (digest, picture) in part.images.iter() {
            images.insert(digest.clone(), picture.clone());
        }
    }
    images
}

/// The chain the parts belong to, beside the notebook's own record.
#[must_use]
pub fn chain() -> ui::resource::DocumentId {
    ui::resource::DocumentId::derived("pages", &ui::resource::DocumentId::workspace_default())
}

/// The address the chain is published under: what an agent or a script
/// invokes the notes' actions on.
#[must_use]
pub fn chain_resource() -> String {
    ui::resource::chain_resource_id(&chain())
}

impl ui::framework::Chained for NoteList {
    type Base = Notebook;

    fn chain() -> ui::resource::DocumentId {
        chain()
    }

    /// An append is written beside the note it adds to, which it names
    /// once it is invoked: its parameter, not its first.
    fn beside(action: &Self::Action) -> Option<Id> {
        match &action.operation {
            NoteListAction::AppendToNote(note_list::AppendToNote { note, .. }) => note.clone(),
            _ => None,
        }
    }

    /// The pages as the tree they make: each under the note it sits under,
    /// by the resource each is published as.
    fn tree(parts: &[(u64, Rc<Self>)]) -> Vec<(Id, Option<Id>)> {
        let resource =
            |id: &Id| Id::from(tilo_resource::items::item_resource_id::<Self>("pages", id));
        Pages::of(parts)
            .iter()
            .map(|(id, page)| (resource(id), page.parent.as_ref().map(resource)))
            .collect()
    }

    fn prepare(
        actions: &mut [Self::Action],
        notebook: &Notebook,
        parts: &[(u64, Rc<Self>)],
        invoker: &ui::tilo_actions::ActionInvoker,
    ) -> Result<(), String> {
        let author = invoker
            .acting
            .members()
            .first()
            .map(|member| (*member).clone());
        prepare(actions, notebook, parts, author.as_ref())
    }

    fn spread(
        action: &Self::Action,
        index: u64,
        parts: &[(u64, Rc<Self>)],
    ) -> Vec<(u64, Self::Action)> {
        spread(action, index, parts)
    }

    fn hidden(parts: &[(u64, Rc<Self>)]) -> HashSet<String> {
        hidden(parts)
    }

    /// The sidebar lists every note, so a window holds every part; a part
    /// bounds what one edit costs, not what is held.
    const HELD_WHOLE: bool = true;
}

#[cfg(test)]
mod tests;
