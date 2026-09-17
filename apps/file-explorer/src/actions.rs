//! Resource and operation boundary for Files.
//!
//! Files is a view over host-owned locations: local folders, remote DAV
//! locations, Trash, and the collaborative FUSE mount. The names, paths,
//! selection, focus, navigation history, transfer progress, credentials and
//! unfinished rename value are observations or device-local interaction state.
//! They must not be represented as fields in the shared Automerge document.
//!
//! The app instead publishes stable [`ui::tilo_actions::Resource`] identities
//! and operation contracts. A resource action crosses the host's operation
//! boundary, where it revalidates the current path/record identity before
//! touching the filesystem or a remote authority. This avoids turning a stale
//! path, a click flag, or a JSON blob in a document into an operation.
//! Collaborative metadata (record IDs, sharing, history, residency) remains
//! owned by the filesystem/authority projections; Files reads those bounded
//! projections and issues their typed control requests without duplicating a
//! second durable record store.
//!
//! Local preferences such as favourites and zoom live in the user's Files
//! config directory. They are deliberately not collaborative. Likewise,
//! inline rename uses a process-local [`ui::widget::TextInput`]; it is a
//! proposed external mutation until committed, not content that another peer
//! should see or undo.
//!
//! Consequently Files has no replayable CRDT actions. `take_committed_action`
//! stays empty; external work is surfaced through the canonical resource
//! operation lifecycle and its typed status/result updates.

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ROOT, ReadDoc};
    use ui::widget::Widget;

    use crate::FileExplorerWidget;
    use crate::command::FileCommand;
    use crate::favorites::UserDirs;

    #[test]
    fn local_interaction_state_never_materialises_a_shared_document_record() {
        let mut files = FileExplorerWidget::new();
        let mut document = AutoCommit::new();

        files.sync(&mut document, &ROOT);
        assert_eq!(document.keys(&ROOT).count(), 0, "initial UI state is local");

        files.set_rename_buffer("draft.txt".into());
        files.sync(&mut document, &ROOT);
        assert_eq!(
            document.keys(&ROOT).count(),
            0,
            "an unfinished filename must not become collaborative data"
        );
    }

    #[test]
    fn filesystem_operation_changes_disk_not_the_shared_document() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("old.txt"), b"x").unwrap();

        let mut files = FileExplorerWidget::new();
        files.navigate(directory.path().to_path_buf());
        let mut document = AutoCommit::new();
        files.sync(&mut document, &ROOT);

        let index = files
            .displayed_entries()
            .iter()
            .position(|entry| entry.name == "old.txt")
            .unwrap();
        files.select(Some(index));
        assert!(files.apply_command(FileCommand::Rename));
        files.set_rename_buffer("new.txt".into());
        assert!(files.apply_command(FileCommand::RenameCommit));

        assert!(directory.path().join("new.txt").exists());
        assert_eq!(
            document.keys(&ROOT).count(),
            0,
            "filesystem mutations are not fabricated as CRDT actions"
        );
    }

    #[test]
    fn preferences_stay_in_user_config_not_the_shared_document() {
        let config = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut files = FileExplorerWidget::with_persistence_dirs(
            Some(config.path().to_path_buf()),
            Some(home.path().to_path_buf()),
            UserDirs::default(),
        );
        let mut document = AutoCommit::new();
        files.sync(&mut document, &ROOT);

        assert!(files.apply_command(FileCommand::AddFavorite(home.path().join("Projects"))));
        assert!(files.apply_command(FileCommand::ZoomIn));

        assert!(config.path().join("favorites").exists());
        assert!(config.path().join("zoom").exists());
        assert_eq!(document.keys(&ROOT).count(), 0);
    }

    #[test]
    fn resources_expose_typed_local_operations_without_crdt_actions() {
        let mut files = FileExplorerWidget::new();
        let mut document = AutoCommit::new();
        files.sync(&mut document, &ROOT);

        let resources = Widget::resources(&files, &document);
        assert_eq!(resources.len(), 1, "one current-folder root");
        let folder = &resources[0];
        assert_eq!(folder.kind, "files.folder");
        let action_ids: Vec<_> = folder
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for expected in ["refresh", "new-folder", "paste"] {
            assert!(
                action_ids.contains(&expected),
                "missing {expected}: {action_ids:?}"
            );
        }
        assert!(
            !action_ids.contains(&"navigate"),
            "a raw local path is not a durable action identity"
        );
        assert!(files.take_committed_action().is_none());
    }
}
