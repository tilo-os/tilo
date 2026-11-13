//! What Files is as a resource: the actions it declares and the rows it publishes.

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ROOT, ReadDoc};
    use ui::widget::Widget;

    use crate::FileExplorerWidget;
    use crate::command::FileCommand;
    use crate::resource::FileExplorer;

    #[test]
    fn local_state_and_operations_stay_out_of_the_shared_document() {
        {
            let mut files = FileExplorerWidget::new();
            let document = AutoCommit::new();

            files.sync(&document, &ROOT);
            assert_eq!(document.keys(&ROOT).count(), 0, "initial UI state is local");

            files.state().set_rename_buffer("draft.txt".into());
            files.sync(&document, &ROOT);
            assert_eq!(
                document.keys(&ROOT).count(),
                0,
                "an unfinished filename must not become collaborative data"
            );
        }

        {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("old.txt"), b"x").unwrap();

            let mut files = FileExplorerWidget::new();
            files.state().navigate(directory.path().to_path_buf());
            let document = AutoCommit::new();
            files.sync(&document, &ROOT);

            let index = files
                .state()
                .displayed_entries()
                .iter()
                .position(|entry| entry.name == "old.txt")
                .unwrap();
            files.state().select(Some(index));
            assert!(files.state().apply_command(FileCommand::Rename));
            files.state().set_rename_buffer("new.txt".into());
            assert!(files.state().apply_command(FileCommand::RenameCommit));

            assert!(directory.path().join("new.txt").exists());
            assert_eq!(
                document.keys(&ROOT).count(),
                0,
                "filesystem mutations are not fabricated as CRDT actions"
            );
        }

        {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("notes.txt"), b"x").unwrap();
            let mut files = FileExplorerWidget::new();
            files.state().navigate(directory.path().to_path_buf());
            let document = AutoCommit::new();
            files.sync(&document, &ROOT);

            let resources = files.state().published_rows();
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

    fn folder_resources(document: &AutoCommit) -> Vec<ui::tilo_actions::ResourceSpec> {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("notes.txt"), b"x").unwrap();
        let mut files = FileExplorerWidget::new();
        files.state().navigate(directory.path().to_path_buf());
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

    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |document| folder_resources(document),
        palette: ["Refresh", "Add this folder to Favorites", "Sort by name"],
    }
}
