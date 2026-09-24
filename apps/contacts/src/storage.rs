//! Reading the address book an older release of this app left on disk.
//!
//! This used to be where Contacts persisted. It saved by replaying the
//! working [`ContactStore`](crate::model::ContactStore) into a **brand-new**
//! `AutoCommit` on every write, which threw away the document's history and
//! actor identity — so despite storing Automerge bytes, the file could never
//! merge with a peer that had diverged, and each save produced a document
//! unrelated to the last. It was using Automerge as a serialisation format.
//!
//! The address book now lives in the shared `ui::State` document, which
//! `ui::persist` saves whole. What is left here is the reader for the old
//! file, so `ui::persist::open_migrating` can import an existing user's book
//! exactly once.
//!
//! Location (first match wins):
//!   * `$TILO_CONTACTS_DATA` — explicit override.
//!   * `$XDG_DATA_HOME/tilo-contacts/addressbook.automerge`.
//!   * `$HOME/.local/share/tilo-contacts/addressbook.automerge`.

use std::path::{Path, PathBuf};

use automerge::AutoCommit;

use crate::doc::ContactsDoc;
use crate::model::Contact;

/// Resolve the address-book file path for the live app, honouring
/// `TILO_CONTACTS_DATA`. Returns `None` only when no home directory can be
/// located (an extremely minimal container), in which case the app runs
/// in-memory for the session rather than crashing.
pub fn default_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TILO_CONTACTS_DATA")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    Some(
        xdg_paths::xdg::data_home()?
            .join("tilo-contacts")
            .join("addressbook.automerge"),
    )
}

/// Load the address book from `path`. Returns an empty vec when the file
/// doesn't exist yet (first launch) or can't be parsed — a missing/corrupt
/// book is a clean-empty start, never a crash.
pub fn load(path: &Path) -> Vec<Contact> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(doc) = AutoCommit::load(&bytes) else {
        tracing::warn!(path = %path.display(), "contacts: address book file is unreadable; starting empty");
        return Vec::new();
    };
    ContactsDoc::list_contacts(&doc)
}

/// Write a file in the shape the old release produced.
///
/// Test-only, and deliberately still wrong in the way the old code was wrong
/// — a fresh `AutoCommit` per call — because its only job now is to produce
/// the artefact the migration has to be able to read.
///
/// # Errors
///
/// Returns the underlying I/O error if the parent directory can't be created
/// or the file can't be written.
#[cfg(test)]
pub fn save(path: &Path, contacts: &[Contact]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut doc = AutoCommit::new();
    for c in contacts {
        ContactsDoc::upsert_contact(&mut doc, c);
    }
    let bytes = doc.save();
    atomic_file::write(path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_contacts;

    fn scratch_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "contacts-storage-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    /// The migration reader must recover every field an old file carried,
    /// or upgrading would silently drop half of each contact.
    #[test]
    fn the_legacy_reader_recovers_every_field() {
        let dir = scratch_dir();
        let path = dir.join("addressbook.automerge");
        let book = sample_contacts();
        save(&path, &book).expect("save");

        let loaded = load(&path);
        assert_eq!(loaded.len(), book.len());
        // Match by id (order is not significant on read).
        for original in &book {
            let back = loaded
                .iter()
                .find(|c| c.id == original.id)
                .expect("every contact loads back by its stable id");
            assert_eq!(back.first, original.first);
            assert_eq!(back.favorite, original.favorite, "favorite flag survives");
            assert_eq!(back.phones, original.phones);
            assert_eq!(back.groups, original.groups);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_empty_not_error() {
        let path = scratch_dir().join("nope.automerge");
        assert!(load(&path).is_empty());
    }
}
