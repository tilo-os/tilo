//! The durable file id behind [`super::RecordId::identify_file`]: the
//! `user.tilo.record-id` extended attribute, kept by `tilo-files`.

use std::path::Path;

/// Raw 16-byte UUID of a file's record, the one the collaborative filesystem
/// serves its `FileRecordId` in and the one written for files elsewhere.
/// Unlike a path, this identity survives rename and move and is the scope
/// used by sharing.
pub const RECORD_ID_XATTR: &str = "user.tilo.record-id";

/// The file's id, minting and storing one for a regular file that has
/// none. `None` when the file can't carry an id.
pub(super) fn read_or_mint(path: &Path) -> Option<uuid::Uuid> {
    let fresh = uuid::Uuid::new_v4().into_bytes();
    match tilo_files::native::identity::read_or_mint(path, RECORD_ID_XATTR, fresh) {
        Ok(id) => id.map(uuid::Uuid::from_bytes),
        Err(error) => {
            tilo_failure::ignored!(
                "the file is identified by its path instead",
                tracing::debug!(path = %path.display(), %error, "file cannot carry a record id; identifying it by path")
            );
            None
        }
    }
}
