//! The durable file id behind [`super::DocumentId::identify_file`]: the
//! `user.tilo.document-id` extended attribute, kept by `tilo-files`.

use std::path::Path;

/// Raw 16-byte UUID of a file, the one the collaborative filesystem
/// serves its `FileDocumentId` in and the one written for files elsewhere.
/// Unlike a path, this identity survives rename and move and is the scope
/// used by sharing.
pub const DOCUMENT_ID_XATTR: &str = "user.tilo.document-id";

/// The file's id, minting and storing one for a regular file that has
/// none. `None` when the file can't carry an id.
pub(super) fn read_or_mint(path: &Path) -> Option<uuid::Uuid> {
    let fresh = uuid::Uuid::new_v4().into_bytes();
    match tilo_files::native::identity::read_or_mint(path, DOCUMENT_ID_XATTR, fresh) {
        Ok(id) => id.map(uuid::Uuid::from_bytes),
        Err(error) => {
            tilo_failure::ignored!(
                "the file is identified by its path instead",
                tracing::debug!(path = %path.display(), %error, "file cannot carry a document id; identifying it by path")
            );
            None
        }
    }
}
