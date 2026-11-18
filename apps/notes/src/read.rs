//! What a notebook's documents hold, read through its records: the
//! [`Notebook`] and the parts its notes are kept in ([`NoteList`]). For tools
//! and tests that look at documents rather than a window.

use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use ui::tilo_resource::Resource;
use ui::widget::BlockContent;

use crate::actions::{Action, InlineMarkSpan, StoredBlock};
use crate::model::{PageId, PageMeta, TagId, TagMeta, Workspace};
use crate::note_list::NoteList;
use crate::notebook::{Notebook, Page, page_id};

/// Start a new notebook in `doc`.
///
/// # Panics
/// When `doc` cannot be written.
#[allow(
    clippy::expect_used,
    reason = "a fresh document always takes a notebook"
)]
pub fn initialize(doc: &mut AutoCommit) {
    Notebook::initialize(doc).expect("a new notebook");
}

/// Start a new part of the notes in `doc`.
///
/// # Panics
/// When `doc` cannot be written.
#[allow(clippy::expect_used, reason = "a fresh document always takes a part")]
pub fn initialize_part(doc: &mut AutoCommit) {
    NoteList::initialize(doc).expect("a new part");
}

/// The notebook `doc` holds; an empty one when it holds none.
#[must_use]
pub fn notebook(doc: &AutoCommit) -> Notebook {
    Notebook::read(doc).unwrap_or_default()
}

/// The part of the notes `doc` holds; an empty one when it holds none.
#[must_use]
pub fn part(doc: &AutoCommit) -> NoteList {
    NoteList::read(doc).unwrap_or_default()
}

/// Whether `doc` reads as a part of the notes.
pub fn validate(doc: &AutoCommit) -> Result<(), String> {
    NoteList::read(doc)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The notebook and the notes its `parts` keep, as the window shows them.
#[must_use]
pub fn workspace(notebook: &Notebook, parts: &[(u64, Rc<NoteList>)]) -> Workspace {
    Workspace::of(notebook, parts, &Workspace::default())
}

/// The notes the part `doc` holds as the window shows them, with no tags,
/// or why it does not read.
pub fn project(doc: &AutoCommit) -> Result<Workspace, String> {
    let part = NoteList::read(doc).map_err(|error| error.to_string())?;
    Ok(workspace(&Notebook::default(), &[(0, Rc::new(part))]))
}

/// The notes the part `part` holds and the tags of the notebook `notebook`,
/// as the window shows them, or why either does not read.
pub fn project_in(notebook: &AutoCommit, part: &AutoCommit) -> Result<Workspace, String> {
    let tags = Notebook::read(notebook).map_err(|error| error.to_string())?;
    let held = NoteList::read(part).map_err(|error| error.to_string())?;
    Ok(workspace(&tags, &[(0, Rc::new(held))]))
}

/// A note's `action` as a window holding only the part `doc` carries it
/// out: what it reads of the notes filled in against that part and
/// `notebook`, or why it is refused.
pub fn prepared(doc: &AutoCommit, notebook: &Notebook, action: &Action) -> Result<Action, String> {
    let held = NoteList::read(doc).map_err(|error| error.to_string())?;
    let mut action = action.clone();
    crate::note_list::prepare(&mut action, notebook, &[(0, Rc::new(held))], None)?;
    Ok(action)
}

/// Carry out a note's `action` on the part `doc` as a window holding only
/// that part does ([`prepared`]), as the person's own change.
pub fn apply_in_part(
    doc: &mut AutoCommit,
    notebook: &Notebook,
    action: &Action,
) -> Result<(), String> {
    let action = prepared(doc, notebook, action)?;
    tilo_actions::apply_local(doc, &action, crate::actions::encode(&action))
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Every note's title and place the part `doc` holds, in notebook order.
#[must_use]
pub fn list_pages(doc: &AutoCommit) -> Vec<PageMeta> {
    workspace(&Notebook::default(), &[(0, Rc::new(part(doc)))])
        .data
        .pages
}

/// Every tag the notebook `doc` holds, by name.
#[must_use]
pub fn list_tags(doc: &AutoCommit) -> Vec<TagMeta> {
    workspace(&notebook(doc), &[]).data.tags
}

/// Every note↔tag edge of `workspace`.
#[must_use]
pub fn list_tag_assignments(workspace: &Workspace) -> Vec<(PageId, TagId)> {
    workspace
        .data
        .tag_assignments
        .iter()
        .map(|edge| (edge.page, edge.tag))
        .collect()
}

fn page(doc: &AutoCommit, id: PageId) -> Option<Page> {
    part(doc).pages.get(&page_id(id)).cloned()
}

/// The object note `id`'s body is kept in, in the part `doc`, when the part
/// keeps the note.
#[must_use]
pub fn page_body(doc: &AutoCommit, id: PageId) -> Option<ObjId> {
    crate::note_list::body_object(doc, id)
}

/// Note `id`'s body, kept in the part `doc`, as the editor shows it, a block
/// per paragraph.
#[must_use]
pub fn page_blocks(doc: &AutoCommit, id: PageId) -> Vec<BlockContent> {
    page(doc, id).map(|page| page.blocks()).unwrap_or_default()
}

/// The id and text of every block of note `id`, kept in the part `doc`, in
/// order.
#[must_use]
pub fn page_block_snapshots(doc: &AutoCommit, id: PageId) -> Vec<(String, String)> {
    page_blocks(doc, id)
        .into_iter()
        .map(|block| (block.id, block.text))
        .collect()
}

/// The formatting and page links of every block of note `id`, kept in the
/// part `doc`.
#[must_use]
pub fn page_block_marks(doc: &AutoCommit, id: PageId) -> Vec<(String, Vec<InlineMarkSpan>)> {
    page_stored_blocks(doc, id)
        .into_iter()
        .map(|block| (block.id, block.marks))
        .collect()
}

/// Every block of note `id`, kept in the part `doc`, with its formatting and
/// page links.
#[must_use]
pub fn page_stored_blocks(doc: &AutoCommit, id: PageId) -> Vec<StoredBlock> {
    page(doc, id)
        .map(|page| crate::actions::stored_blocks(&page))
        .unwrap_or_default()
}

/// The pictures the `parts` keep whose bytes are readable now, by the
/// `asset:` source the notes show them by.
#[must_use]
pub fn images(parts: &[(u64, Rc<NoteList>)]) -> std::collections::HashMap<String, Vec<u8>> {
    let content = crate::notebook::content();
    crate::notebook::image_snapshot(&crate::note_list::images(parts), &content)
}
