//! The notebook as the window shows it: every note's title and place in the
//! tree, the tags, and what each note says, projected from the [`Notebook`]
//! record the framework reads. Pure logic, no IO, no rendering.
//!
//! Page and tag ids are the UUIDs their items are stored under, so a link,
//! a route and a search hit name a note the same way every replica does.

use std::collections::HashMap;
use std::rc::Rc;

use uuid::Uuid;

use crate::notebook::{Notebook, page_of};

/// Identifier for a single page. Stable across renames; every link and
/// route names a note by it.
pub type PageId = Uuid;
/// Stable identity for a reusable workspace tag. Like pages, ids are minted
/// once and never derived from a display name.
pub type TagId = Uuid;

/// Per-page metadata: the title and the place in the tree. Siblings are in
/// notebook order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageMeta {
    pub id: PageId,
    pub title: String,
    pub parent: Option<PageId>,
}

/// A named, reusable tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagMeta {
    pub id: TagId,
    pub name: String,
}

/// One edge in the note↔tag graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagAssignment {
    pub page: PageId,
    pub tag: TagId,
}

/// The notes, tags and edges of a workspace, and the note on screen.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceData {
    pub pages: Vec<PageMeta>,
    pub selected: Option<PageId>,
    pub tags: Vec<TagMeta>,
    pub tag_assignments: Vec<TagAssignment>,
}

/// The workspace the window shows, with the runtime queries the UI needs.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub data: WorkspaceData,
    /// The record this workspace shows.
    pub notebook: Rc<Notebook>,
    /// What every note says.
    pub bodies: std::sync::Arc<crate::actions::Bodies>,
    /// The notes whose bodies changed since the workspace this one was made
    /// after ([`Self::of`]); every note for a first projection.
    pub changed: Vec<PageId>,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// The workspace showing `notebook`, after `previous`: a note whose body
    /// did not change keeps what `previous` read of it.
    #[must_use]
    pub fn of(notebook: Rc<Notebook>, previous: &Self) -> Self {
        let mut ids = std::collections::HashSet::new();
        let mut pages = Vec::with_capacity(notebook.pages.len());
        for (id, page) in notebook.pages.iter() {
            let Some(page_id) = page_of(id).filter(|id| ids.insert(*id)) else {
                continue;
            };
            pages.push(PageMeta {
                id: page_id,
                title: page.title.clone(),
                parent: page.parent.as_ref().and_then(page_of),
            });
        }
        let known: std::collections::HashSet<PageId> = ids;
        // A parent removed while a peer filed a note under it, or a cycle
        // two peers' moves made, leaves the note at the top level.
        let parents: HashMap<PageId, Option<PageId>> =
            pages.iter().map(|page| (page.id, page.parent)).collect();
        for page in &mut pages {
            if page.parent.is_some_and(|parent| !known.contains(&parent))
                || in_cycle(&parents, page.id)
            {
                page.parent = None;
            }
        }
        let mut tags: Vec<TagMeta> = notebook
            .tags
            .iter()
            .filter_map(|(id, tag)| {
                Some(TagMeta {
                    id: page_of(id)?,
                    name: tag.name.0.clone(),
                })
            })
            .filter(|tag| !tag.name.trim().is_empty())
            .collect();
        tags.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        let tag_ids: std::collections::HashSet<TagId> = tags.iter().map(|tag| tag.id).collect();
        let mut tag_assignments: Vec<TagAssignment> = notebook
            .pages
            .iter()
            .filter_map(|(id, page)| Some((page_of(id)?, page)))
            .flat_map(|(page, note)| {
                note.tags
                    .iter()
                    .filter_map(page_of)
                    .filter(|tag| tag_ids.contains(tag))
                    .map(move |tag| TagAssignment { page, tag })
            })
            .collect();
        tag_assignments.sort_by_key(|edge| (edge.page, edge.tag));
        tag_assignments.dedup();
        let mut changed = Vec::new();
        let bodies = pages
            .iter()
            .filter_map(|meta| {
                let page = notebook.pages.get(&crate::notebook::page_id(meta.id))?;
                let unchanged = previous
                    .notebook
                    .pages
                    .get(&crate::notebook::page_id(meta.id))
                    .is_some_and(|before| before.body == page.body);
                let blocks = match previous.bodies.get(&meta.id) {
                    Some(blocks) if unchanged => blocks.clone(),
                    _ => {
                        changed.push(meta.id);
                        crate::actions::stored_blocks(page)
                    }
                };
                Some((meta.id, blocks))
            })
            .collect();
        Self {
            data: WorkspaceData {
                pages,
                selected: previous.data.selected.filter(|page| known.contains(page)),
                tags,
                tag_assignments,
            },
            notebook,
            bodies: std::sync::Arc::new(bodies),
            changed,
        }
    }

    pub fn pages(&self) -> &[PageMeta] {
        &self.data.pages
    }

    pub fn selected(&self) -> Option<PageId> {
        self.data.selected
    }

    pub fn set_selected(&mut self, id: Option<PageId>) {
        self.data.selected = id;
    }

    pub fn tags(&self) -> &[TagMeta] {
        &self.data.tags
    }

    pub fn get_tag(&self, id: TagId) -> Option<&TagMeta> {
        self.data.tags.iter().find(|tag| tag.id == id)
    }

    /// The tag named `name`, ignoring case.
    pub fn tag_named(&self, name: &str) -> Option<&TagMeta> {
        let name = name.trim().to_lowercase();
        self.data
            .tags
            .iter()
            .find(|tag| tag.name.to_lowercase() == name)
    }

    /// Note `page`'s body as the rich text it is.
    pub fn body_state(&self, page: PageId) -> Option<&tilo_document::RichTextState> {
        self.notebook
            .pages
            .get(&crate::notebook::page_id(page))?
            .body
            .state()
    }

    pub fn tags_for_page(&self, page: PageId) -> Vec<&TagMeta> {
        let mut tags = self
            .data
            .tag_assignments
            .iter()
            .filter(|edge| edge.page == page)
            .filter_map(|edge| self.get_tag(edge.tag))
            .collect::<Vec<_>>();
        tags.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        tags
    }

    pub fn pages_for_tag(&self, tag: TagId) -> Vec<PageId> {
        let mut pages = self
            .data
            .tag_assignments
            .iter()
            .filter(|edge| edge.tag == tag)
            .map(|edge| edge.page)
            .collect::<Vec<_>>();
        pages.sort();
        pages.dedup();
        pages
    }

    /// Look a page up by id. None if unknown.
    pub fn get(&self, id: PageId) -> Option<&PageMeta> {
        self.data.pages.iter().find(|p| p.id == id)
    }

    /// The children of `parent` (or roots when `parent` is None), in
    /// notebook order.
    pub fn children_of(&self, parent: Option<PageId>) -> Vec<&PageMeta> {
        self.data
            .pages
            .iter()
            .filter(|p| p.parent == parent)
            .collect()
    }

    /// Add a page under `parent`, last among its siblings, for a workspace
    /// built by hand (a sidebar on its own). Returns the new id.
    pub fn create_page(&mut self, parent: Option<PageId>) -> PageId {
        let id = Uuid::new_v4();
        self.data.pages.push(PageMeta {
            id,
            title: crate::notebook::UNTITLED.to_string(),
            parent,
        });
        id
    }

    /// True when `maybe_ancestor` is `id` itself or one of its ancestors.
    pub fn is_ancestor(&self, maybe_ancestor: PageId, id: PageId) -> bool {
        let mut cur = Some(id);
        let mut hops = 0;
        while let Some(c) = cur {
            if c == maybe_ancestor {
                return true;
            }
            if hops > self.data.pages.len() {
                break;
            }
            cur = self.get(c).and_then(|p| p.parent);
            hops += 1;
        }
        false
    }
}

/// Whether `page`'s chain of parents comes back to itself.
fn in_cycle(parents: &HashMap<PageId, Option<PageId>>, page: PageId) -> bool {
    let mut current = parents.get(&page).copied().flatten();
    for _ in 0..parents.len() {
        match current {
            None => return false,
            Some(id) if id == page => return true,
            Some(id) => current = parents.get(&id).copied().flatten(),
        }
    }
    current.is_some()
}
