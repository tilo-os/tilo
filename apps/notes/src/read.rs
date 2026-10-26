//! What a notebook document holds, read through the [`Notebook`] record: for
//! tools and tests that look at a document rather than a window.

use automerge::{AutoCommit, ObjId};
use ui::tilo_resource::Resource;
use ui::widget::BlockContent;

use crate::actions::{InlineMarkSpan, StoredBlock};
use crate::model::{PageId, PageMeta, TagId, TagMeta, Workspace};
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

/// The notebook `doc` holds, or why it does not read.
///
/// An initialized document holding no notes is a notebook with nothing in
/// it. A document that does not read is an error, never an empty notebook.
///
/// # Errors
/// Says why `doc` does not read as a notebook.
pub fn notebook(doc: &AutoCommit) -> Result<Notebook, String> {
    Notebook::read(doc).map_err(|error| error.to_string())
}

/// Whether `doc` reads as a notebook.
///
/// # Errors
/// Says why it does not.
pub fn validate(doc: &AutoCommit) -> Result<(), String> {
    Notebook::read(doc)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The notebook `doc` holds as the window shows it, or why it does not read.
///
/// # Errors
/// Says why `doc` does not read as a notebook.
pub fn project(doc: &AutoCommit) -> Result<Workspace, String> {
    let notebook = Notebook::read(doc).map_err(|error| error.to_string())?;
    Ok(Workspace::of(&notebook, &Workspace::default()))
}

/// The notebook `doc` holds, as the window shows it, or why it does not read.
///
/// # Errors
/// Says why `doc` does not read as a notebook.
pub fn workspace(doc: &AutoCommit) -> Result<Workspace, String> {
    notebook(doc).map(|notebook| Workspace::of(&notebook, &Workspace::default()))
}

/// The workspace of a document a tool already knows reads. Tests and exports
/// that only look at a checked record use it; anything that acts on a live
/// document calls [`project`] or [`workspace`] and handles the failure.
fn workspace_or_empty(doc: &AutoCommit) -> Workspace {
    workspace(doc).unwrap_or_default()
}

/// Every note's title and place, in notebook order.
#[must_use]
pub fn list_pages(doc: &AutoCommit) -> Vec<PageMeta> {
    workspace_or_empty(doc).data.pages
}

/// Every tag, by name.
#[must_use]
pub fn list_tags(doc: &AutoCommit) -> Vec<TagMeta> {
    workspace_or_empty(doc).data.tags
}

/// Every note↔tag edge.
#[must_use]
pub fn list_tag_assignments(doc: &AutoCommit) -> Vec<(PageId, TagId)> {
    workspace_or_empty(doc)
        .data
        .tag_assignments
        .into_iter()
        .map(|edge| (edge.page, edge.tag))
        .collect()
}

fn page(doc: &AutoCommit, id: PageId) -> Option<Page> {
    notebook(doc).ok()?.pages.get(&page_id(id)).cloned()
}

/// The object note `id`'s body is kept in, when the note exists.
#[must_use]
pub fn page_body(doc: &AutoCommit, id: PageId) -> Option<ObjId> {
    crate::notebook::body_object(doc, id)
}

/// Note `id`'s body as the editor shows it, a block per paragraph.
#[must_use]
pub fn page_blocks(doc: &AutoCommit, id: PageId) -> Vec<BlockContent> {
    page(doc, id).map(|page| page.blocks()).unwrap_or_default()
}

/// The id and text of every block of note `id`, in order.
#[must_use]
pub fn page_block_snapshots(doc: &AutoCommit, id: PageId) -> Vec<(String, String)> {
    page_blocks(doc, id)
        .into_iter()
        .map(|block| (block.id, block.text))
        .collect()
}

/// The formatting and page links of every block of note `id`.
#[must_use]
pub fn page_block_marks(doc: &AutoCommit, id: PageId) -> Vec<(String, Vec<InlineMarkSpan>)> {
    page_stored_blocks(doc, id)
        .into_iter()
        .map(|block| (block.id, block.marks))
        .collect()
}

/// Every block of note `id` with its formatting and page links.
#[must_use]
pub fn page_stored_blocks(doc: &AutoCommit, id: PageId) -> Vec<StoredBlock> {
    page(doc, id)
        .map(|page| crate::actions::stored_blocks(&page))
        .unwrap_or_default()
}

/// The pictures `doc` holds whose bytes are readable now, by the `asset:`
/// source the notes show them by.
#[must_use]
pub fn images(doc: &AutoCommit) -> std::collections::HashMap<String, Vec<u8>> {
    let content = crate::notebook::content();
    notebook(doc).map_or_else(
        |_| std::collections::HashMap::new(),
        |notebook| crate::notebook::image_snapshot(&notebook.images, |digest| content.read(digest)),
    )
}

#[cfg(test)]
mod tests {
    use super::{list_pages, project, validate};
    use automerge::transaction::Transactable as _;

    /// `doc` with its notes no longer a collection, as a document a newer or
    /// broken writer leaves behind.
    fn corrupt(doc: &mut automerge::AutoCommit) {
        doc.put(automerge::ROOT, "pages", "not a collection")
            .expect("the field is written");
    }

    #[test]
    fn a_document_that_does_not_read_is_not_an_empty_notebook() {
        let mut empty = automerge::AutoCommit::new();
        super::initialize(&mut empty);
        assert!(
            project(&empty)
                .expect("an initialized document is a valid empty notebook")
                .pages()
                .is_empty()
        );
        assert!(validate(&empty).is_ok());
        assert!(list_pages(&empty).is_empty());

        let mut broken = automerge::AutoCommit::new();
        super::initialize(&mut broken);
        corrupt(&mut broken);
        assert!(super::notebook(&empty).is_ok());
        assert!(super::workspace(&empty).is_ok());
        let failure = project(&broken).expect_err("the notebook does not read");
        assert!(
            failure.contains("pages"),
            "the failure names the field: {failure:?}"
        );
        assert!(super::notebook(&broken).is_err());
        assert!(super::workspace(&broken).is_err());
        assert!(validate(&broken).is_err());
        assert_eq!(
            list_pages(&broken),
            Vec::new(),
            "the test helper still answers nothing; `project` is how a caller tells why"
        );
        let failure = crate::actions::find_text(&broken, "anything", None)
            .expect_err("a search on a notebook that does not read fails");
        assert!(
            failure.contains("pages"),
            "the effect says why it could not search: {failure:?}"
        );
    }
}
