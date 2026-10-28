//! The durable file id behind [`super::RecordId::identify_file`]: the
//! `user.tilo.record-id` extended attribute, kept by `tilo-files`.

use std::path::Path;

/// The attribute the collaborative filesystem serves its `FileRecordId`
/// in, and the one written for files elsewhere.
pub(super) use fs_protocol::RECORD_ID_XATTR as ATTRIBUTE;

/// The file's id, minting and storing one for a regular file that has
/// none. `None` when the file can't carry an id.
pub(super) fn read_or_mint(path: &Path) -> Option<uuid::Uuid> {
    let fresh = uuid::Uuid::new_v4().into_bytes();
    match tilo_files::native::identity::read_or_mint(path, ATTRIBUTE, fresh) {
        Ok(id) => id.map(uuid::Uuid::from_bytes),
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "file cannot carry a record id; identifying it by path");
            None
        }
    }
}
