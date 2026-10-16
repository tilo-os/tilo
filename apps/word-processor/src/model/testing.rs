//! Documents built block by block for the crate's tests.

use automerge::AutoCommit;

/// A Writer document whose body holds the schema and nothing else, as a
/// tilo-doc [`tilo_doc::Document`] to build block by block: for tests.
///
/// # Errors
/// Returns the document's write error.
pub(crate) fn empty() -> Result<tilo_doc::Document, String> {
    let mut doc = AutoCommit::new();
    super::init(&mut doc)?;
    doc.commit();
    super::reopen(doc)
}
