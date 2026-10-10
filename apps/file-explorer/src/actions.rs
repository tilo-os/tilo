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
//! What the Files document holds is the person's bookmarked places, changed
//! only through [`crate::resource::FilesDocAction`]; view preferences are the
//! person's settings. Inline rename uses a process-local
//! [`ui::widget::TextInput`]; it is a proposed external mutation until
//! committed, not content that another peer should see or undo.

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ROOT, ReadDoc};
    use ui::widget::Widget;

    use crate::FileExplorerWidget;
    use crate::command::FileCommand;
    use crate::resource::FileExplorer;

    /// Interaction state, filesystem operations and preferences never materialise in the shared document; resources expose typed local operations.
    #[test]
    fn local_state_and_operations_stay_out_of_the_shared_document() {
        // --- phase: local_interaction_state_never_materialises_a_shared_document_record ---
        {
            let mut files = FileExplorerWidget::new();
            let document = AutoCommit::new();

            files.sync(&document, &ROOT);
            assert_eq!(document.keys(&ROOT).count(), 0, "initial UI state is local");

            files.set_rename_buffer("draft.txt".into());
            files.sync(&document, &ROOT);
            assert_eq!(
                document.keys(&ROOT).count(),
                0,
                "an unfinished filename must not become collaborative data"
            );
        }

        // --- phase: filesystem_operation_changes_disk_not_the_shared_document ---
        {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("old.txt"), b"x").unwrap();

            let mut files = FileExplorerWidget::new();
            files.navigate(directory.path().to_path_buf());
            let document = AutoCommit::new();
            files.sync(&document, &ROOT);

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

        // --- phase: the_window_publishes_only_live_rows ---
        {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("notes.txt"), b"x").unwrap();
            let mut files = FileExplorerWidget::new();
            files.navigate(directory.path().to_path_buf());
            let document = AutoCommit::new();
            files.sync(&document, &ROOT);

            let resources = files.published_rows();
            assert!(
                resources
                    .iter()
                    .all(|resource| resource.kind != "files.folder"),
                "the folder is the resource's publication, not the window's"
            );
            assert!(
                resources
                    .iter()
                    .any(|resource| resource.label == "notes.txt"),
                "the listing's rows are the window's"
            );
            assert!(
                resources
                    .iter()
                    .flat_map(|resource| &resource.actions)
                    .all(|action| action.kind != ui::tilo_actions::OperationKind::Document),
                "a live row never changes the document"
            );
            assert!(ui::undo::take_committed_action(&mut files).is_none());
        }
    }

    /// What Files publishes while showing a real folder holding a file, synced
    /// against `document` exactly as the host drives it.
    fn folder_resources(document: &AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("notes.txt"), b"x").unwrap();
        let mut files = FileExplorerWidget::new();
        files.navigate(directory.path().to_path_buf());
        let synced = document.clone();
        files.sync(&synced, &ROOT);
        crate::files_app::published::tree(&files, &synced)
    }

    fn seeded() -> AutoCommit {
        use ui::tilo_resource::Resource as _;
        let mut document = AutoCommit::new();
        FileExplorer::initialize(&mut document).unwrap();
        document
    }

    fn registry() -> ui::tilo_actions::ActionRegistry {
        use ui::tilo_resource::Resource as _;
        ui::tilo_actions::ActionRegistry::with(FileExplorer::register)
    }

    // The resource's publication (its places and the folder-wide work) and
    // the window's live rows, as the palette sees them together.
    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |document| folder_resources(document),
        palette: ["Refresh folder", "Add to Favourites", "Sort by name"],
    }
}
