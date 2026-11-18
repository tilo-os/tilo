//! The notebook as the window shows it: every note's title and place in the
//! tree, the tags, and what each note says, projected from the [`Notebook`]
//! record and the parts its notes are kept in ([`NoteList`]), as the
//! framework reads them. Pure logic, no IO, no rendering.
//!
//! Page and tag ids are the UUIDs their items are stored under, so a link,
//! a route and a search hit name a note the same way every replica does.

use std::collections::HashMap;
use std::rc::Rc;

use tilo_document::{ChangeCursor, RichTextState};
use ui::tilo_resource::{Bytes, Map};
use uuid::Uuid;

use crate::note_list::{NoteList, Pages};
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
///
/// It keeps what it read of the notebook, never the notebook itself: the
/// framework updates a state no one else holds in place, and copies every
/// note of one a window still holds before each change.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub data: WorkspaceData,
    /// The pictures the notes show, by the digest their bodies name them by.
    pub images: Map<Bytes>,
    /// What every note says.
    pub bodies: std::sync::Arc<crate::actions::Bodies>,
    /// Where each note's body history stood when `bodies` read it; `None`
    /// for a note whose body is not stored yet.
    pub body_cursors: Rc<HashMap<PageId, Option<ChangeCursor>>>,
    /// The notes whose bodies changed since the workspace this one was made
    /// after ([`Self::of`]); every note for a first projection.
    pub changed: Vec<PageId>,
    /// The part each note is kept in.
    pub parts: Rc<HashMap<PageId, u64>>,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// The workspace showing `notebook` and the notes its `parts` keep,
    /// after `previous`: a note whose body did not change keeps what
    /// `previous` read of it. A note under one that is gone is not shown.
    #[must_use]
    pub fn of(notebook: &Notebook, parts: &[(u64, Rc<NoteList>)], previous: &Self) -> Self {
        let shown = Pages::of(parts);
        let mut known = std::collections::HashSet::with_capacity(shown.len());
        let mut pages = Vec::with_capacity(shown.len());
        let mut kept = HashMap::with_capacity(shown.len());
        let mut notes = Vec::with_capacity(shown.len());
        for (id, page) in shown.iter() {
            let Some(page_id) = page_of(id).filter(|id| known.insert(*id)) else {
                continue;
            };
            pages.push(PageMeta {
                id: page_id,
                title: page.title.clone(),
                parent: shown.parent_of(id).and_then(page_of),
            });
            kept.insert(page_id, shown.part_of(id).unwrap_or_default());
            notes.push((page_id, page));
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
        // A tag the notebook no longer names is not shown on its notes.
        let tag_ids: std::collections::HashSet<TagId> = tags.iter().map(|tag| tag.id).collect();
        let mut tag_assignments: Vec<TagAssignment> = notes
            .iter()
            .flat_map(|(page, note)| {
                note.tags
                    .iter()
                    .filter_map(page_of)
                    .filter(|tag| tag_ids.contains(tag))
                    .map(move |tag| TagAssignment { page: *page, tag })
            })
            .collect();
        tag_assignments.sort_by_key(|edge| (edge.page, edge.tag));
        tag_assignments.dedup();
        let (bodies, body_cursors, changed) = project_bodies(&notes, previous);
        let images = crate::note_list::images(parts);
        Self {
            data: WorkspaceData {
                pages,
                selected: previous.data.selected.filter(|page| known.contains(page)),
                tags,
                tag_assignments,
            },
            images,
            bodies: std::sync::Arc::new(bodies),
            body_cursors: Rc::new(body_cursors),
            changed,
            parts: Rc::new(kept),
        }
    }

    /// The part note `page` is kept in, while it is shown.
    #[must_use]
    pub fn part_of(&self, page: PageId) -> Option<u64> {
        self.parts.get(&page).copied()
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

    /// Whether note `page` has its body stored in the resource yet.
    pub fn has_stored_body(&self, page: PageId) -> bool {
        self.body_cursors.get(&page).is_some_and(Option::is_some)
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

    /// What `page` is called ([`crate::metadata::note_name`]).
    #[must_use]
    pub fn name_of(&self, page: &PageMeta) -> String {
        self.named(&page.title, page.id)
    }

    /// What note `id` is called when titled `title`: with no title, by its first line.
    #[must_use]
    pub fn named(&self, title: &str, id: PageId) -> String {
        crate::metadata::note_name(
            title,
            self.bodies
                .get(&id)
                .into_iter()
                .flat_map(|blocks| blocks.iter().map(|block| block.text.as_str())),
        )
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
            title: crate::notebook::NO_TITLE.to_string(),
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

/// What every note of `notes` says, read after `previous`, with where each
/// body's history stood and the notes whose blocks changed.
fn project_bodies(
    notes: &[(PageId, &crate::notebook::Page)],
    previous: &Workspace,
) -> (
    crate::actions::Bodies,
    HashMap<PageId, Option<ChangeCursor>>,
    Vec<PageId>,
) {
    let mut changed = Vec::new();
    let mut cursors = HashMap::with_capacity(notes.len());
    let bodies = notes
        .iter()
        .map(|(id, page)| {
            let cursor = page.body.state().map(RichTextState::change_cursor);
            cursors.insert(*id, cursor);
            let before = previous.bodies.get(id);
            // A body whose change cursor stands where it stood is the
            // body `previous` read: no paragraph of it changed, came,
            // went or moved. Any other is read, and kept as before when
            // it reads the same.
            let blocks = match before {
                Some(blocks)
                    if cursor.is_some() && previous.body_cursors.get(id) == Some(&cursor) =>
                {
                    std::sync::Arc::clone(blocks)
                }
                _ => {
                    let read = before
                        .zip(previous.body_cursors.get(id).copied().flatten())
                        .and_then(|(blocks, at)| changed_blocks(page, blocks, at))
                        .unwrap_or_else(|| {
                            crate::actions::stored_blocks(page)
                                .into_iter()
                                .map(std::sync::Arc::new)
                                .collect()
                        });
                    match before {
                        Some(blocks) if **blocks == *read => std::sync::Arc::clone(blocks),
                        _ => {
                            changed.push(*id);
                            read.into()
                        }
                    }
                }
            };
            (*id, blocks)
        })
        .collect();
    (bodies, cursors, changed)
}

/// `blocks`, what `page` said when its body's history stood at `at`, with
/// the paragraphs changed since read again and the others shared: `None`
/// when what changed cannot be told and the body is read whole.
fn changed_blocks(
    page: &crate::notebook::Page,
    blocks: &[std::sync::Arc<crate::actions::StoredBlock>],
    at: ChangeCursor,
) -> Option<Vec<std::sync::Arc<crate::actions::StoredBlock>>> {
    let changed = page.blocks_changed_since(at, blocks.len())?;
    let mut read = blocks.to_vec();
    for (index, block) in changed {
        *read.get_mut(index)? = std::sync::Arc::new(crate::actions::stored_block(block));
    }
    Some(read)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use tilo_document::RichTextAction;
    use tilo_document::action::RegionKind;
    use ui::rich_edit::BlockContent;
    use ui::tilo_resource::{Invoke, Replica};

    use super::{PageId, Workspace};
    use crate::actions;
    use crate::note_list::NoteList;
    use crate::notebook::Notebook;

    fn note(part: &mut Replica<NoteList>, title: &str, text: &str) -> PageId {
        let page = PageId::new_v4();
        part.apply_action(&actions::create_page_action(page, None, title))
            .unwrap();
        let blocks = [BlockContent::paragraph("", text)];
        part.apply(Invoke::apply(&actions::write_body_action(page, &blocks)).unwrap())
            .unwrap();
        page
    }

    fn of(part: NoteList, previous: &Workspace) -> Workspace {
        Workspace::of(&Notebook::default(), &[(0, Rc::new(part))], previous)
    }

    /// A projection after an edit re-reads only the edited note: the others
    /// keep the blocks the last projection read, shared rather than copied,
    /// whether the part was kept current in place or read afresh.
    #[test]
    fn projection_rereads_only_the_note_that_changed() {
        let mut part = Replica::<NoteList>::new();
        let kept = note(&mut part, "Kept", "Unchanged words");
        let edited = note(&mut part, "Edited", "Before");
        let first = of(part.live().unwrap().clone(), &Workspace::default());
        assert_eq!(
            first.changed.len(),
            2,
            "a first projection reads every note"
        );

        let splice = RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: first.bodies[&edited][0].id.clone(),
            at: 0,
            del: 0,
            ins: "Now ".into(),
        };
        part.apply(Invoke::apply(&actions::edit_body_action(edited, &splice)).unwrap())
            .unwrap();
        let live = of(part.live().unwrap().clone(), &first);
        assert_eq!(live.changed, vec![edited]);
        assert_eq!(live.bodies[&edited][0].text, "Now Before");
        assert!(Arc::ptr_eq(&live.bodies[&kept], &first.bodies[&kept]));

        let fresh = of(part.state().unwrap(), &live);
        assert!(
            fresh.changed.is_empty(),
            "a fresh read of the same notes changes none"
        );
        for page in [kept, edited] {
            assert!(Arc::ptr_eq(&fresh.bodies[&page], &live.bodies[&page]));
        }
        assert!(fresh.has_stored_body(edited));
    }
}
