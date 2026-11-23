//! Only a genuine committed token publishes the fixed completed selector.
use super::boot_review::Selection;
use super::*;
use crate::setup::migration::{changed, check_cancel};
use std::ffi::OsStr;
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
pub(super) fn publish(
    origin: &Origin,
    expected: &crate::imager_model::PreparedSetupExpectation,
    cancel: &AtomicBool,
) -> io::Result<()> {
    check_cancel(cancel)?;
    match origin {
        Origin::Home(v) => v.with_committed_selection_directory(|private, facts| {
            let selected = Selection::home(facts)?;
            publish_selected(private, &selected, expected, cancel)
        }),
        Origin::Applications(v) => v.with_committed_selection_directory(|private, facts| {
            let selected = Selection::applications(facts)?;
            publish_selected(private, &selected, expected, cancel)
        }),
    }
}
fn publish_selected(
    private: &Arc<PinnedDirectory>,
    selected: &Selection,
    expected: &crate::imager_model::PreparedSetupExpectation,
    cancel: &AtomicBool,
) -> io::Result<()> {
    if &selected.completed_setup != expected {
        return Err(changed("Completed selector differs from committed setup"));
    }
    let bytes = serde_json::to_vec(selected)?;
    if bytes.len() > 16 * 1024 {
        return Err(invalid());
    }
    let storage =
        StorageDirectory::from_file(migration_durability::readable_directory(&private.file)?)?;
    storage.validate_privacy(Privacy::OwnerOnly)?;
    match bounded_file(private, "completed-selection.json", 16 * 1024) {
        Ok(existing) if existing == bytes => {}
        Ok(_) => {
            return Err(changed(
                "Completed selector conflicts with protected committed output",
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            storage
                .publish(
                    OsStr::new("completed-selection.json"),
                    &bytes,
                    PublishMode::IfAbsent,
                    Privacy::OwnerOnly,
                )
                .map_err(|e| e.into_io_error())?;
        }
        Err(e) => return Err(e),
    }
    if bounded_file(private, "completed-selection.json", 16 * 1024)? != bytes {
        return Err(invalid());
    }
    private.validate_destination()?;
    check_cancel(cancel)
}
