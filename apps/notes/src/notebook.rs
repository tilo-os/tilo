//! The notebook: what Notes stores, and the operations on it that are more
//! than editing one field.
//!
//! [`Notebook`] is the record: its notes ([`Page`]) in one [`Collection`],
//! whose order is the order of every group of sibling notes, the tags a note
//! may carry, and the images the notes show. A note's body is rich text,
//! edited by field edits at its path
//! (`Notebook::paths().pages.item(&id).body.perform(&op)`); assigning or
//! removing a tag is a field edit of the note's `tags`, renaming a tag one of
//! the tag's `name`. The methods below are what is more than that: a note's
//! place in the tree (a parent must exist and never be the note's own
//! descendant), its title (renaming relabels the links to it), creating,
//! deleting and copying notes with everything under them.

use std::collections::{HashMap, HashSet};

use tilo_document::action::{RegionKind, Span};
use tilo_document::{ClipBlock, RichText, RichTextAction};
use ui::tilo_resource::{
    self, ActionCx, Atom, Bytes, Collection, Id, Map, Model, Refusal, Set, resource,
};
use ui::widget::BlockContent;
use ui::widget::marks::Mark;

use crate::model::PageId;

/// Largest image a note accepts. The notebook records a reference and the
/// bytes live in the content store, but the action adding it carries the
/// bytes once, so this stays well inside one action payload.
pub const MAX_ASSET_BYTES: usize = 1024 * 1024;

/// The title a note gets when none is given.
pub const UNTITLED: &str = "Untitled";

/// The notebook.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Notebook {
    /// Notes
    pub pages: Collection<Page>,
    /// Tags
    pub tags: Collection<Tag>,
    /// Images
    ///
    /// The pictures the notes show, by the BLAKE3 digest their bodies name
    /// them by. Only placing a picture writes one.
    #[model(readonly)]
    pub images: Map<Bytes>,
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
    /// Body
    ///
    /// Read whole: a note is short, and search, links and copies read every
    /// word of it. Published as a rich-text resource of its own beneath the
    /// note (`notes.notebook/pages/<page>/body`), whose block actions apply
    /// as field edits of this field.
    #[model(whole)]
    #[model(resource)]
    pub body: RichText,
    /// Tags
    pub tags: Set<Id>,
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
    /// A new note titled `title` (the untitled title when blank) under
    /// `parent`.
    #[must_use]
    pub fn titled(title: &str, parent: Option<Id>) -> Self {
        let title = title.trim();
        Self {
            title: if title.is_empty() { UNTITLED } else { title }.to_owned(),
            parent,
            ..Self::default()
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
            .map(|view| tilo_rich_text::content_of(view))
            .collect()
    }
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

#[resource(
    kind = "notes.notebook",
    label = "Notebook",
    version = 2,
    migrate = crate::legacy::migrate,
    search = crate::search::entries,
    read_content_in_doc = Notebook::read_note,
    document_child = Notebook::document_projection_child,
    answer = Notebook::created_page,
    effects = crate::effect::NotesEffect
)]
impl Notebook {
    /// New note
    ///
    /// A note titled `title`, after the notes already under `parent` (a
    /// top-level note when none).
    #[action(answers)]
    pub fn new_note(
        &mut self,
        cx: &mut ActionCx,
        #[param("Title")] title: String,
        #[param("The note it goes under; none for a top-level note")] parent: Option<Id>,
    ) -> Result<(), Refusal> {
        if let Some(parent) = &parent {
            self.pages
                .get(parent)
                .ok_or("the note it goes under is gone")?;
        }
        let id = Id::from(cx.uuid().to_string());
        self.pages.push(id, Page::titled(&title, parent));
        Ok(())
    }

    /// New note inside
    ///
    /// A note titled `title`, after the notes already under this one.
    #[action(item = pages, answers)]
    pub fn new_child(
        &mut self,
        cx: &mut ActionCx,
        page: &Id,
        #[param("Title")] title: String,
    ) -> Result<(), Refusal> {
        self.pages.get(page).ok_or("no such note")?;
        let id = Id::from(cx.uuid().to_string());
        self.pages
            .push(id, Page::titled(&title, Some(page.clone())));
        Ok(())
    }

    /// Rename note
    ///
    /// Give the note a new title; every link to it reads the new title.
    #[action(item = pages)]
    pub fn rename(
        &mut self,
        page: &Id,
        #[param("New title")] title: String,
    ) -> Result<(), Refusal> {
        let title = title.trim().to_owned();
        let note = self.pages.get_mut(page).ok_or("no such note")?;
        if note.title == title {
            return Ok(());
        }
        note.title.clone_from(&title);
        if let Some(target) = page_of(page) {
            self.relabel_links(target, &title);
        }
        Ok(())
    }

    /// Delete note
    ///
    /// Delete the note and every note under it.
    #[action(item = pages, destructive)]
    pub fn delete(&mut self, page: &Id) -> Result<(), Refusal> {
        self.pages.get(page).ok_or("no such note")?;
        for id in self.subtree(page) {
            self.pages.remove(&id);
        }
        Ok(())
    }

    /// Move note
    ///
    /// Put the note under `parent` (top level when none), before the note
    /// `before` there, or after the notes there when none.
    #[action(item = pages)]
    pub fn move_note(
        &mut self,
        page: &Id,
        #[param("The note it goes under; none for the top level")] parent: Option<Id>,
        #[param("The note it goes before; none to go last")] before: Option<Id>,
    ) -> Result<(), Refusal> {
        self.place(page, parent, before.as_ref())
    }

    /// Move note up
    #[action(item = pages)]
    pub fn move_up(&mut self, page: &Id) -> Result<(), Refusal> {
        let (parent, siblings, at) = self.siblings_of(page)?;
        let above = at
            .checked_sub(1)
            .and_then(|at| siblings.get(at))
            .ok_or("the note is first already")?
            .clone();
        self.place(page, parent, Some(&above))
    }

    /// Move note down
    #[action(item = pages)]
    pub fn move_down(&mut self, page: &Id) -> Result<(), Refusal> {
        let (parent, siblings, at) = self.siblings_of(page)?;
        if at.saturating_add(1) >= siblings.len() {
            return Err(Refusal::new("the note is last already"));
        }
        let after_next = siblings.get(at.saturating_add(2)).cloned();
        self.place(page, parent, after_next.as_ref())
    }

    /// Indent note
    ///
    /// Put the note under the note above it.
    #[action(item = pages)]
    pub fn indent(&mut self, page: &Id) -> Result<(), Refusal> {
        let (_, siblings, at) = self.siblings_of(page)?;
        let above = at
            .checked_sub(1)
            .and_then(|at| siblings.get(at))
            .ok_or("no note above to go under")?
            .clone();
        self.place(page, Some(above), None)
    }

    /// Outdent note
    ///
    /// Put the note beside the note it is under, right after it.
    #[action(item = pages)]
    pub fn outdent(&mut self, page: &Id) -> Result<(), Refusal> {
        let parent = self
            .pages
            .get(page)
            .ok_or("no such note")?
            .parent
            .clone()
            .ok_or("the note is at the top level already")?;
        let (grandparent, siblings, at) = self.siblings_of(&parent)?;
        let next = siblings.get(at.saturating_add(1)).cloned();
        self.place(page, grandparent, next.as_ref())
    }

    /// Duplicate note
    ///
    /// Copy the note and every note under it, right after it. Links between
    /// the copied notes lead to the copies.
    #[action(item = pages)]
    pub fn duplicate(&mut self, cx: &mut ActionCx, page: &Id) -> Result<(), Refusal> {
        let source = self.pages.get(page).ok_or("no such note")?.clone();
        let sources = self.subtree(page);
        let copies: HashMap<Id, Id> = sources
            .iter()
            .map(|id| (id.clone(), Id::from(cx.uuid().to_string())))
            .collect();
        let follow = |page: PageId| copies.get(&page_id(page)).and_then(page_of).unwrap_or(page);
        let mut at = self
            .pages
            .position(page)
            .map_or(0, |at| at.saturating_add(1));
        for id in &sources {
            let (Some(original), Some(copy)) = (self.pages.get(id), copies.get(id)) else {
                continue;
            };
            let mut blocks = original.blocks();
            for block in &mut blocks {
                // A copied fence is a fence of its own: its group follows
                // the copy, the same on every replica.
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
            let title = if id == page {
                format!("{} copy", original.title)
            } else {
                original.title.clone()
            };
            let parent = if id == page {
                source.parent.clone()
            } else {
                original
                    .parent
                    .as_ref()
                    .and_then(|parent| copies.get(parent).cloned())
            };
            let page_copy = Page {
                title,
                parent,
                body: written(&blocks),
                tags: original.tags.clone(),
            };
            self.pages.insert(at, copy.clone(), page_copy);
            at = at.saturating_add(1);
        }
        Ok(())
    }

    /// New tag
    #[action]
    pub fn new_tag(
        &mut self,
        cx: &mut ActionCx,
        #[param("Name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim();
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

    /// Tag note
    ///
    /// Give the note the tag named `name`, made when the notebook has none.
    #[action(item = pages)]
    pub fn tag(
        &mut self,
        cx: &mut ActionCx,
        page: &Id,
        #[param("Tag")] name: String,
    ) -> Result<(), Refusal> {
        self.pages.get(page).ok_or("no such note")?;
        let name = name.trim();
        let tag = if let Some(tag) = self.tag_named(name) {
            tag
        } else {
            let id = Id::from(cx.uuid().to_string());
            self.tags.push(
                id.clone(),
                Tag {
                    name: Atom(name.to_owned()),
                },
            );
            id
        };
        if let Some(note) = self.pages.get_mut(page) {
            note.tags.insert(tag);
        }
        Ok(())
    }

    /// Delete tag
    ///
    /// Delete the tag and take it off every note.
    #[action(item = tags, destructive)]
    pub fn delete_tag(&mut self, tag: &Id) -> Result<(), Refusal> {
        self.tags.remove(tag).ok_or("no such tag")?;
        for (_, page) in self.pages.iter_mut() {
            page.tags.remove(tag);
        }
        Ok(())
    }

    /// Keep image
    ///
    /// Keep a PNG, JPEG or WebP picture with the notebook, for a note's body
    /// to show by `![alt](asset:<digest>)`.
    #[action]
    pub fn keep_image(
        &mut self,
        cx: &mut ActionCx,
        #[param("The picture")] bytes: Vec<u8>,
    ) -> Result<(), Refusal> {
        self.store_image(cx, &bytes).map(drop)
    }

    /// Place image
    ///
    /// Keep a PNG, JPEG or WebP picture with the notebook and show it at the
    /// end of the note, described by `alt`.
    #[action(item = pages)]
    pub fn place_image(
        &mut self,
        cx: &mut ActionCx,
        page: &Id,
        #[param("The picture")] bytes: Vec<u8>,
        #[param("A short description of it")] alt: Option<String>,
    ) -> Result<(), Refusal> {
        self.pages.get(page).ok_or("no such note")?;
        let digest = self.store_image(cx, &bytes)?;
        let alt = alt
            .filter(|alt| !alt.trim().is_empty())
            .unwrap_or_else(|| "Image".to_owned())
            .replace([']', '[', '(', ')'], " ");
        if let Some(note) = self.pages.get_mut(page) {
            note.body.perform(RichTextAction::AppendParagraph {
                text: format!("![{}](asset:{digest})", alt.trim()),
            });
        }
        Ok(())
    }

    /// Create note from Markdown
    ///
    /// A top-level note titled `title` whose body is the Markdown: headings,
    /// bullets, `- [ ]` checklists, quotes, code, tables and `[[Note title]]`
    /// links. A leading `# title` equal to the title is dropped. Answers the
    /// note's id and resource.
    #[action(answers)]
    pub fn create_page(
        &mut self,
        cx: &mut ActionCx,
        #[param("The note's title")] title: String,
        #[param("The note's body as Markdown")] markdown: Option<String>,
    ) -> Result<(), Refusal> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Refusal::new("give the note a title"));
        }
        let markdown =
            crate::markdown::strip_matching_title_heading(&markdown.unwrap_or_default(), title);
        check_markdown_import(&markdown).map_err(Refusal::new)?;
        let id = Id::from(cx.uuid().to_string());
        let page = Page::titled(title, None);
        let new = page_of(&id);
        let blocks = crate::MainPanel::seed_blocks(&markdown, |target| {
            if target == page.title {
                return new;
            }
            self.pages
                .iter()
                .find(|(_, candidate)| candidate.title == target)
                .and_then(|(id, _)| page_of(id))
        });
        self.pages.push(
            id,
            Page {
                body: written(&blocks),
                ..page
            },
        );
        Ok(())
    }

    /// Import Markdown
    ///
    /// Add Markdown text as a new top-level note titled `title`, with the
    /// pictures it shows. A `[[Title]]` link leads to the note of that
    /// title.
    #[action]
    pub fn import_markdown(
        &mut self,
        cx: &mut ActionCx,
        #[param("Title")] title: String,
        #[param("Markdown content")] markdown: String,
        #[param("Pictures the Markdown shows")] images: Vec<Vec<u8>>,
        #[param("The id the note is kept under; a new one when none")] note: Option<Id>,
    ) -> Result<(), Refusal> {
        check_markdown_import(&markdown).map_err(Refusal::new)?;
        let id = match note {
            Some(note) if page_of(&note).is_none() => {
                return Err(Refusal::new("a note's id is a UUID"));
            }
            Some(note) if self.pages.contains(&note) => {
                return Err(Refusal::new("that note is imported already"));
            }
            Some(note) => note,
            None => Id::from(cx.uuid().to_string()),
        };
        for bytes in images {
            self.store_image(cx, &bytes)?;
        }
        let fallback = ui::i18n!("notes-imported-untitled");
        let page = Page::titled(
            if title.trim().is_empty() {
                &fallback
            } else {
                &title
            },
            None,
        );
        let new = page_of(&id);
        let blocks = crate::MainPanel::seed_blocks(&markdown, |target| {
            if target == page.title {
                return new;
            }
            self.pages
                .iter()
                .find(|(_, candidate)| candidate.title == target)
                .and_then(|(id, _)| page_of(id))
        });
        self.pages.push(
            id,
            Page {
                body: written(&blocks),
                ..page
            },
        );
        Ok(())
    }
}

impl Notebook {
    /// The note `page` and every note under it, parents before their
    /// children, in notebook order.
    #[must_use]
    pub fn subtree(&self, page: &Id) -> Vec<Id> {
        let mut inside: HashSet<Id> = HashSet::from([page.clone()]);
        // Notebook order puts no parent before its children, so grow the
        // set until a pass adds nothing.
        loop {
            let before = inside.len();
            for (id, note) in self.pages.iter() {
                if note
                    .parent
                    .as_ref()
                    .is_some_and(|parent| inside.contains(parent))
                {
                    inside.insert(id.clone());
                }
            }
            if inside.len() == before {
                break;
            }
        }
        let mut ordered = vec![page.clone()];
        let mut pending = vec![page.clone()];
        while let Some(parent) = pending.pop() {
            for (id, note) in self.pages.iter() {
                if note.parent.as_ref() == Some(&parent)
                    && inside.contains(id)
                    && !ordered.contains(id)
                {
                    ordered.push(id.clone());
                    pending.push(id.clone());
                }
            }
        }
        ordered
    }

    /// Keep picture `bytes` under its digest; the digest.
    fn store_image(&mut self, cx: &ActionCx, bytes: &[u8]) -> Result<String, Refusal> {
        let (digest, stored) = image(bytes)?;
        if stored.needs_store() {
            let store = cx.content().ok_or("no content store keeps pictures here")?;
            store.put(bytes).map_err(|error| {
                Refusal::new(format!("the content store refused the picture: {error}"))
            })?;
        }
        self.images.insert(digest.clone(), stored);
        Ok(digest)
    }

    /// The tag named `name`.
    #[must_use]
    pub fn tag_named(&self, name: &str) -> Option<Id> {
        self.tags
            .iter()
            .find(|(_, tag)| tag.name.0 == name)
            .map(|(id, _)| id.clone())
    }

    /// The notes beside `page` (its parent's children, in order), and where
    /// it is among them.
    fn siblings_of(&self, page: &Id) -> Result<(Option<Id>, Vec<Id>, usize), Refusal> {
        let parent = self.pages.get(page).ok_or("no such note")?.parent.clone();
        let siblings: Vec<Id> = self
            .pages
            .iter()
            .filter(|(_, note)| note.parent == parent)
            .map(|(id, _)| id.clone())
            .collect();
        let at = siblings
            .iter()
            .position(|id| id == page)
            .ok_or("no such note")?;
        Ok((parent, siblings, at))
    }

    /// Put `page` under `parent`, before its child `before` (last when
    /// none). Refused when the parent is gone or would be the note itself or
    /// a note under it.
    fn place(&mut self, page: &Id, parent: Option<Id>, before: Option<&Id>) -> Result<(), Refusal> {
        self.pages.get(page).ok_or("no such note")?;
        if let Some(parent) = &parent {
            self.pages
                .get(parent)
                .ok_or("the note it goes under is gone")?;
            if self.subtree(page).contains(parent) {
                return Err(Refusal::new("a note cannot go under itself"));
            }
        }
        let order: Vec<Id> = self.pages.ids().filter(|id| *id != page).cloned().collect();
        let siblings: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|(_, id)| self.pages.get(id).is_some_and(|note| note.parent == parent))
            .map(|(at, _)| at)
            .collect();
        let target = before
            .and_then(|before| order.iter().position(|id| id == before))
            .filter(|at| siblings.contains(at))
            .or_else(|| siblings.last().map(|last| last.saturating_add(1)));
        if let Some(note) = self.pages.get_mut(page) {
            note.parent = parent;
        }
        if let Some(target) = target {
            self.pages.move_to(page, target);
        }
        Ok(())
    }

    /// Relabel every link and embed of note `target` to read `title`.
    fn relabel_links(&mut self, target: PageId, title: &str) {
        let chars =
            |text: &str, byte: usize| text.get(..byte).map_or(0, |head| head.chars().count());
        for (_, page) in self.pages.iter_mut() {
            for block in page.blocks() {
                let mut ranges: Vec<_> = block
                    .marks
                    .iter()
                    .filter(|span| {
                        matches!(&span.mark, Mark::Link(linked) | Mark::Transclusion(linked) if *linked == target)
                    })
                    .filter(|span| block.text.get(span.range.clone()) != Some(title))
                    .map(|span| (span.range.clone(), span.mark.clone()))
                    .collect();
                // Last first, so each edit leaves the earlier ranges where
                // they are.
                ranges.sort_unstable_by_key(|(range, _)| std::cmp::Reverse(range.start));
                for (range, mark) in ranges {
                    let at = chars(&block.text, range.start);
                    let del = block
                        .text
                        .get(range.clone())
                        .map_or(0, |old| old.chars().count());
                    page.body.perform(RichTextAction::SpliceText {
                        region: RegionKind::Body,
                        block: block.id.clone(),
                        at,
                        del,
                        ins: title.to_owned(),
                    });
                    page.body.perform(mark_op(
                        &block.id,
                        at..at.saturating_add(title.chars().count()),
                        &mark,
                    ));
                }
            }
        }
    }

    /// What a note-creating action answers with: the id and resource address
    /// of the note it made, so its caller can act on it without a second
    /// read. A new note is a new entry of the note collection, keyed by its
    /// id; edits only touch fields of notes that exist, and a new tag is an
    /// entry of the tag collection, which is no note.
    fn created_page(
        doc: &tilo_resource::Doc,
        action: &str,
        changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        use tilo_resource::Resource as _;
        let notebook = Notebook::read(doc).ok()?;
        // A block action of a note's body answers the blocks it touched.
        if tilo_document::block_actions::ANSWERING.contains(&action) {
            return notebook.pages.iter().find_map(|(id, _)| {
                let body = body_object(doc, page_of(id)?)?;
                let answer =
                    tilo_document::block_actions::action_result_under(doc, &body, action, changes)?;
                answer
                    .as_object()
                    .is_some_and(|touched| !touched.is_empty())
                    .then_some(answer)
            });
        }
        changes.iter().find_map(|patch| {
            let automerge::PatchAction::PutMap {
                key,
                value: (automerge::Value::Object(_), _),
                ..
            } = &patch.action
            else {
                return None;
            };
            let page = page_of(&Id::from(key.as_str()))?;
            notebook.pages.contains(&page_id(page)).then(|| {
                serde_json::json!({
                    "page": page.to_string(),
                    "resource": crate::actions::page_resource_id(page),
                })
            })
        })
    }

    /// Only notes and their bodies are addressed by the notebook's read.
    /// Tags and custom view children do not inherit that disclosure.
    fn document_projection_child(&self, id: &str) -> bool {
        crate::actions::page_of_resource(id)
            .or_else(|| crate::actions::page_of_body_resource(id))
            .is_some_and(|page| self.pages.get(&page_id(page)).is_some())
    }

    /// One note, or the notebook, as a reader reads it: the notebook as its
    /// notes and tags; a note (by its id) as its title and blocks, each with
    /// the id edits of its body name it by; a note's body resource as those
    /// blocks.
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
                "tags": self.tags.iter().map(|(id, tag)| serde_json::json!({
                    "id": id.as_str(),
                    "name": tag.name.0,
                })).collect::<Vec<_>>(),
            }));
        };
        if let Some(page) = crate::actions::page_of_body_resource(item.as_str()) {
            let body = body_object(doc, page).ok_or_else(|| {
                format!("no note `{item}`; read the notebook for the notes it holds")
            })?;
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
            .ok_or_else(|| format!("no note `{item}`; read the notebook for the notes it holds"))?;
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

/// The digest an image is kept under, and how it is stored.
///
/// # Errors
/// Refuses anything but a PNG, JPEG or WebP picture of at most
/// [`MAX_ASSET_BYTES`].
pub fn image(bytes: &[u8]) -> Result<(String, Bytes), Refusal> {
    if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
        return Err(Refusal::new("a picture is between 1 byte and 1 MiB"));
    }
    if image_mime(bytes).is_none() {
        return Err(Refusal::new("a picture is PNG, JPEG or WebP"));
    }
    Ok((blake3::hash(bytes).to_hex().to_string(), Bytes::of(bytes)))
}

/// The picture format of `bytes`, when it is one notes show.
#[must_use]
pub fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    tilo_content::image_format(bytes)
        .map(|format| format.mime)
        .filter(|mime| matches!(*mime, "image/png" | "image/jpeg" | "image/webp"))
}

/// The object note `page`'s body is kept in, in `doc`.
#[must_use]
pub fn body_object(doc: &tilo_resource::Doc, page: PageId) -> Option<automerge::ObjId> {
    Notebook::paths()
        .pages
        .item(&page_id(page))
        .body
        .object(doc)
}

/// The content store of the window whose code is running. Outside a window
/// (a test, a page without a store), an empty one in memory.
#[must_use]
pub fn content() -> tilo_content::ContentStore {
    ui::tilo_resource::content::current().unwrap_or_else(tilo_content::ContentStore::memory)
}

/// The pictures of a notebook's `images` whose bytes are readable now, by the `asset:`
/// source their bodies name them by. A picture a collaborator added may
/// still be on its way from their device; it is absent until it arrives.
#[must_use]
pub fn image_snapshot(
    images: &Map<Bytes>,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> HashMap<String, Vec<u8>> {
    images
        .iter()
        .filter_map(|(digest, stored)| {
            let bytes = match stored.inline() {
                Some(bytes) => bytes.to_vec(),
                None => read(digest).filter(|bytes| tilo_content::verify(digest, bytes))?,
            };
            Some((format!("asset:{digest}"), bytes))
        })
        .collect()
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
        return Err(ui::i18n!("notes-import-too-big"));
    }
    if crate::markdown::parse(markdown).len() > MAX_MARKDOWN_IMPORT_BLOCKS {
        return Err(ui::i18n!("notes-import-too-many-blocks"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "notebook/tests.rs"]
mod tests;
