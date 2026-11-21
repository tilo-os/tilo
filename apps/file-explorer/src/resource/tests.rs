use super::*;
use ui::prelude::TestApp;
use ui::tilo_resource::{Replica, Settled};

tilo_test_env::scenario! {
    fn the_files_document_publishes_places_bookmarks_and_preferences;

    phase a_new_document_publishes_the_standard_places;
    {
        let state = Replica::<FileExplorer>::new().state().unwrap();
        assert_eq!(state.bookmarks(), crate::places::DEFAULT_PLACES);

        let published = ui::tilo_resource::publish::<FileExplorer>("files", "Folder", &state);
        assert_eq!(published.kind, "files.folder");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "add-bookmark",
            "remove-bookmark",
            "field-edit",
            "new-folder",
            "rename",
            "trash",
            "move",
            "copy-into",
            "revert",
        ] {
            assert!(ids.contains(&expected), "missing `{expected}`: {ids:?}");
        }
        let effects: Vec<String> = <FilesEffect as ui::tilo_resource::Effect>::specs()
            .into_iter()
            .map(|spec| spec.id.clone())
            .collect();
        for expected in [
            "refresh",
            "paste",
            "go-to",
            "open-folder",
            "list",
            "grid",
            "sort-name",
        ] {
            assert!(
                effects.iter().any(|id| id == expected),
                "the window's `{expected}` is an effect: {effects:?}"
            );
        }
        for gone in [
            "set-view",
            "set-sort",
            "set-zoom",
            "set-hidden",
            "set-bookmarks",
        ] {
            assert!(!ids.contains(&gone), "`{gone}` is a preference: {ids:?}");
        }

        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        let rows = app.harness().resources();
        let places: Vec<_> = rows
            .iter()
            .flat_map(|root| std::iter::once(root).chain(&root.children))
            .filter(|row| row.kind == "files.place")
            .collect();
        assert!(!places.is_empty(), "the window publishes its places");
        assert!(
            places
                .iter()
                .all(|place| place.actions.iter().any(|a| a.id == "open")),
            "each place opens"
        );
    }

    phase bookmarks_and_actions_are_named_as_a_person_reads_them;
    {
        crate::i18n::set_locale("en-US");
        let state = Replica::<FileExplorer>::new().state().unwrap();
        let published = ui::tilo_resource::publish::<FileExplorer>("files", "Folder", &state);
        let names: Vec<&str> = published
            .children
            .iter()
            .map(|child| child.label.as_str())
            .collect();
        for place in [
            "Home",
            "Documents",
            "Downloads",
            "Pictures",
            "Music",
            "Videos",
        ] {
            assert!(names.contains(&place), "{place} is published: {names:?}");
        }
        assert!(
            names.iter().all(|name| !name.contains("xdg:")),
            "no bookmark is published under its stored form: {names:?}"
        );
        let title = |id: &str| {
            published
                .actions
                .iter()
                .find(|action| action.id == id)
                .map(|action| action.label.as_str())
        };
        for (id, short) in [
            ("new-folder", "New folder"),
            ("rename", "Rename"),
            ("copy-into", "Copy entries to folder"),
            ("revert", "Take back change"),
        ] {
            assert_eq!(title(id), Some(short), "`{id}` has a short title");
        }
    }

    phase bookmark_edits_are_granular;
    {
        let mut laptop = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let mut phone = TestApp::<FileExplorerShell>::on_device("phone").unwrap();
        sync(&mut laptop, &mut phone);
        laptop.invoke(pin("~/Projects", None)).unwrap();
        laptop.invoke(pin("~/Shared", None)).unwrap();
        phone.invoke(pin("/mnt/data", Some(1))).unwrap();
        phone.invoke(pin("~/Shared", None)).unwrap();
        sync(&mut laptop, &mut phone);
        let places = laptop.state().bookmarks();
        assert!(places.contains(&"~/Projects".to_owned()), "{places:?}");
        assert_eq!(places[1], "/mnt/data");
        assert_eq!(
            places.iter().filter(|place| *place == "~/Shared").count(),
            1,
            "{places:?}"
        );
        assert_eq!(phone.state().bookmarks(), places, "both converge");

        laptop
            .invoke(
                Invoke::edit(
                    &FileExplorer::paths()
                        .places
                        .move_to(&bookmark_id("~/Projects"), 0),
                )
                .unwrap(),
            )
            .unwrap();
        laptop
            .invoke(FileExplorer::REMOVE_BOOKMARK.with("xdg:music".to_owned()))
            .unwrap();
        let places = laptop.state().bookmarks();
        assert_eq!(places[0], "~/Projects");
        assert!(!places.contains(&"xdg:music".to_owned()));
        assert!(
            laptop
                .invoke(FileExplorer::REMOVE_BOOKMARK.with("xdg:music".to_owned()))
                .is_err(),
            "a folder that is not a favourite is refused"
        );
    }

    phase pinning_a_folder_is_a_bookmark_action;
    {
        let folder = tempfile::tempdir().unwrap();
        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        assert_eq!(app.state().bookmarks(), crate::places::DEFAULT_PLACES);
        app.with_surface(|files: &mut FileExplorerWidget| {
            files
                .state()
                .apply_command(crate::FileCommand::AddFavorite(folder.path().to_path_buf()))
        })
        .unwrap();
        app.settle();

        let place = folder.path().display().to_string();
        assert!(
            app.state().bookmarks().contains(&place),
            "{:?}",
            app.state()
        );
        assert!(app.undo());
        assert!(!app.state().bookmarks().contains(&place));
        let shown = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files
                    .state()
                    .favorites()
                    .iter()
                    .any(|f| f.path == folder.path())
            })
            .unwrap();
        assert!(!shown, "the sidebar shows the stored places");

        app.invoke(pin(&place, None)).unwrap();
        let shown = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files
                    .state()
                    .favorites()
                    .iter()
                    .any(|f| f.path == folder.path())
            })
            .unwrap();
        assert!(shown, "an agent's pin reaches the sidebar");
    }

    phase view_preferences_are_personal_settings;
    {
        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        let before = app.peer_document().get_heads();
        app.with_surface(|files: &mut FileExplorerWidget| {
            files.state().set_view_mode(crate::model::ViewMode::Grid);
            files.state().apply_command(crate::FileCommand::ZoomIn);
        })
        .unwrap();
        app.settle();
        assert_eq!(
            app.peer_document().get_heads(),
            before,
            "no document change"
        );
        let stored = app.settings::<FilesPreferences>().get();
        assert_eq!(stored.view, "grid");
        assert_eq!(stored.zoom, 1);
    }

    phase captures_browse_their_own_scene;
    {
        let guard = crate::model::fixture_io_guard::DenyAmbientIo::new();
        let mut select = TestApp::<FileExplorerShell>::capture(Some("select")).unwrap();
        assert_eq!(select.harness().inspection().viewport, [1200, 720]);
        let frame = select.harness().inspection_json().unwrap();
        assert!(frame.contains("Holiday plan.md"), "{frame}");
        assert!(frame.contains("Development"), "{frame}");
        assert_eq!(guard.attempts(), 0, "a capture read ambient state");
        drop(guard);

        let mut menu = TestApp::<FileExplorerShell>::capture(Some("context-menu")).unwrap();
        let frame = menu.harness().inspection_json().unwrap();
        assert!(
            frame.contains("Meeting notes"),
            "the scene's templates: {frame}"
        );
    }

    phase a_launch_opens_the_folder_it_names;
    {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("launched.txt"), b"x").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[directory.path().to_str().unwrap()]).unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("launched.txt"), "{frame}");
    }
}

fn sync(a: &mut TestApp<FileExplorerShell>, b: &mut TestApp<FileExplorerShell>) {
    let mut doc = b.peer_document();
    a.merge_peer(&mut doc).unwrap();
    b.merge_peer(&mut doc).unwrap();
    a.settle();
    b.settle();
}

fn pin(place: &str, at: Option<usize>) -> Invoke<FileExplorer> {
    FileExplorer::ADD_BOOKMARK.with(file_explorer::AddBookmark {
        place: place.parse().unwrap(),
        at,
    })
}

fn act_on(app: &mut TestApp<FileExplorerShell>, on: &str, id: &str, args: &serde_json::Value) {
    app.harness()
        .call_on_confirmed(on, id, args.clone())
        .unwrap();
    app.settle();
}

tilo_test_env::scenario! {
    fn selection_and_file_changes_are_undoable_actions_on_their_device;

    phase published_batch_copy_is_one_undoable_operation_and_cut_requires_write;
    {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap();
        let root = tempfile::tempdir_in(home).unwrap();
        let destination = root.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.path().join(name), name.as_bytes()).unwrap();
        }
        let mut app = TestApp::<FileExplorerShell>::launched(&[root.path().to_str().unwrap()]).unwrap();
        let entries = ["a.txt", "c.txt"]
            .map(|name| crate::search_provider::entry_resource_id(&root.path().join(name)))
            .to_vec();
        app.effect(&FilesEffect::SelectEntries { entries }).unwrap();
        app.effect(&FilesEffect::CopySelection).unwrap();
        app.effect(&FilesEffect::ClearSelection).unwrap();
        app.effect(&FilesEffect::GoTo {
            path: AbsolutePath::new(&destination).unwrap(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste { conflict: None }).unwrap();
        for name in ["a.txt", "c.txt"] {
            assert_eq!(
                std::fs::read(destination.join(name)).unwrap(),
                name.as_bytes()
            );
        }
        assert!(!destination.join("b.txt").exists());
        undo_settled(&mut app);
        for name in ["a.txt", "c.txt"] {
            assert!(!destination.join(name).exists());
            assert_eq!(
                std::fs::read(root.path().join(name)).unwrap(),
                name.as_bytes()
            );
        }

        let mut viewer =
            TestApp::<FileExplorerShell>::with_role(ui::permissions::Role::Viewer).unwrap();
        viewer
            .effect(&FilesEffect::GoTo {
                path: AbsolutePath::new(root.path()).unwrap(),
            })
            .unwrap();
        viewer.effect(&FilesEffect::SelectAll).unwrap();
        viewer.effect(&FilesEffect::CopySelection).unwrap();
        assert!(viewer.effect(&FilesEffect::CutSelection).is_err());
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert!(root.path().join(name).exists());
        }
    }

    phase published_selection_capture_observes_the_real_selected_rows;
    {
        let guard = crate::model::fixture_io_guard::DenyAmbientIo::new();
        let mut app = TestApp::<FileExplorerShell>::capture(Some("selection-actions")).unwrap();
        assert_eq!(
            app.with_surface(|files: &mut FileExplorerWidget| files.state().selected_indices().len()),
            Some(2)
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Notes.txt"), "{frame}");
        assert!(frame.contains("Recipes.md"), "{frame}");
        assert_eq!(guard.attempts(), 0, "a capture read ambient state");
    }

    phase the_edits_bound_to_a_selection_name_it_in_the_windows_language;
    {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Bericht.pdf"), b"x").unwrap();
        let launch = [root.path().to_str().unwrap()];
        let mut app = TestApp::<FileExplorerShell>::launched(&launch).unwrap();
        app.effect(&FilesEffect::SelectEntries {
            entries: vec![crate::search_provider::entry_resource_id(
                &root.path().join("Bericht.pdf"),
            )],
        })
        .unwrap();
        let labels = |app: &mut TestApp<FileExplorerShell>| -> Vec<String> {
            app.with_surface(|files: &mut FileExplorerWidget| {
                files
                    .state()
                    .bound_declarations()
                    .iter()
                    .filter(|decl| matches!(decl.id, "rename-selected" | "trash-selected"))
                    .map(|decl| decl.shown_label())
                    .collect()
            })
            .unwrap()
        };
        crate::i18n::set_locale("de-DE");
        let german = labels(&mut app);
        crate::i18n::set_locale("en-US");
        let english = labels(&mut app);
        assert_eq!(
            german,
            [
                "„Bericht.pdf“ umbenennen",
                "„Bericht.pdf“ in den Papierkorb verschieben"
            ]
        );
        assert_eq!(
            english,
            [
                "Rename \u{201c}Bericht.pdf\u{201d}",
                "Move \u{201c}Bericht.pdf\u{201d} to Trash"
            ]
        );
    }

    phase published_selection_changes_stay_in_the_addressed_files_window;
    {
        let root = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.path().join(name), name.as_bytes()).unwrap();
        }
        let launch = [root.path().to_str().unwrap()];
        let mut source = TestApp::<FileExplorerShell>::launched(&launch).unwrap();
        let mut other = TestApp::<FileExplorerShell>::launched(&launch).unwrap();
        source.effect(&FilesEffect::SelectAll).unwrap();
        other
            .effect(&FilesEffect::SelectEntries {
                entries: vec![crate::search_provider::entry_resource_id(
                    &root.path().join("b.txt"),
                )],
            })
            .unwrap();
        assert_eq!(
            source
                .with_surface(|files: &mut FileExplorerWidget| files.state().selected_indices().len()),
            Some(2)
        );
        assert_eq!(
            other.with_surface(|files: &mut FileExplorerWidget| files.state().selected_indices().len()),
            Some(1)
        );
        source.effect(&FilesEffect::ClearSelection).unwrap();
        assert_eq!(
            source
                .with_surface(|files: &mut FileExplorerWidget| files.state().selected_indices().len()),
            Some(0)
        );
        assert_eq!(
            other.with_surface(|files: &mut FileExplorerWidget| files.state().selected_indices().len()),
            Some(1)
        );
        assert!(source.state().ops.is_empty());
        assert!(other.state().ops.is_empty());
    }

    phase queued_file_changes_declare_that_they_can_be_undone;
    {
        use ui::tilo_actions::{ActionKind, COMPENSATED_UNDO_TAG};
        use ui::tilo_resource::Resource as _;
        let declarations = FileExplorer::declarations();
        let tagged = |id: &str| {
            declarations
                .iter()
                .find(|decl| decl.id == id)
                .unwrap()
                .tags
                .iter()
                .any(|tag| tag == COMPENSATED_UNDO_TAG)
        };
        for id in [
            "rename",
            "move",
            "trash",
            "new-folder",
            "copy-into",
            "compress",
            "extract",
        ] {
            assert!(tagged(id), "{id} is undone by its compensation");
        }
        assert!(!tagged("add-bookmark"), "a document edit needs no tag");
        let bound = FileExplorer::RENAME
            .bind(file_explorer::Rename::paths().device, "device".to_owned())
            .and(file_explorer::Rename::paths().folder, "/tmp".to_owned())
            .declaration(
                "rename-here",
                ui::tilo_resource::word!("files-folder-rename-here"),
            )
            .unwrap();
        assert_eq!(bound.kind, ActionKind::Outside);
        let spec = ui::tilo_resource::declaration_spec(&*bound);
        assert!(spec.contract.can_be_undone());
    }

    phase filesystem_changes_are_undoable_document_actions;
    {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("draft.txt"), b"x").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        act(
            &mut app,
            "new-folder-here",
            &serde_json::json!({"name": "Projects"}),
        );
        assert!(folder.path().join("Projects").is_dir());
        act(
            &mut app,
            "rename-here",
            &serde_json::json!({"entry": "draft.txt", "name": "final.txt"}),
        );
        assert!(folder.path().join("final.txt").exists());
        let projects = folder.path().join("Projects").display().to_string();
        act(
            &mut app,
            "move-here",
            &serde_json::json!({"entries": ["final.txt"], "to": projects}),
        );
        assert!(folder.path().join("Projects/final.txt").exists());
        let ops = app.state().ops;
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert!(
            ops.entries()
                .all(|(_, op)| op.settled.as_ref().is_some_and(Settled::is_done))
        );

        assert!(app.undo());
        app.settle();
        assert!(folder.path().join("final.txt").exists(), "moved back");
        assert!(app.undo());
        app.settle();
        assert!(folder.path().join("draft.txt").exists(), "named back");
        assert!(app.redo());
        app.settle();
        assert!(folder.path().join("final.txt").exists(), "renamed again");

        let refused = app.harness().call_on(
            <FileExplorer as ui::tilo_resource::Resource>::KIND,
            "trash-here",
            serde_json::json!({"entries": ["missing.txt"]}),
        );
        assert!(refused.is_err(), "a misspelt entry changes nothing");
    }

    phase the_windows_work_is_its_effects;
    {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir(folder.path().join("Projects")).unwrap();
        std::fs::write(folder.path().join("notes.txt"), b"n").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        let row = crate::search_provider::entry_resource_id(&folder.path().join("notes.txt"));
        app.effect_on(&row, &FilesEffect::Copy { entry: row.clone() })
            .unwrap();
        app.effect(&FilesEffect::GoTo {
            path: AbsolutePath::new(folder.path().join("Projects")).unwrap(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste { conflict: None }).unwrap();
        assert!(
            folder.path().join("Projects/notes.txt").exists(),
            "a finished paste has its copy in place"
        );
        assert!(
            app.effect(&FilesEffect::EmptyTrash).is_err(),
            "Trash is emptied from the Trash folder only"
        );
        act(&mut app, "add-current-bookmark", &serde_json::json!({}));
        let place = folder.path().join("Projects").display().to_string();
        assert!(
            app.state().bookmarks().contains(&place),
            "{:?}",
            app.state()
        );
    }

    phase files_are_read_written_and_compressed_through_actions;
    {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("notes.md"), "# Notes\n\nBuy milk.").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        let read = app
            .effect(&FilesEffect::ReadText {
                file: "notes.md".into(),
                from: None,
            })
            .unwrap()
            .value
            .unwrap();
        assert_eq!(read["format"], "markdown");
        assert_eq!(read["text"], "# Notes\n\nBuy milk.");
        assert_eq!(read["next"], serde_json::Value::Null);
        let handed = app
            .effect(&FilesEffect::HandOver {
                file: "notes.md".into(),
            })
            .unwrap();
        let hash = ui::tilo_actions::content_hash(&handed.resources[0])
            .expect("a content address")
            .to_owned();
        assert_eq!(
            app.content().read(&hash).as_deref(),
            Some(b"# Notes\n\nBuy milk.".as_slice()),
            "the next step reads the file's bytes by their address"
        );
        app.effect(&FilesEffect::SelectAll).unwrap();
        let selected = app
            .effect(&FilesEffect::SelectedEntries)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(
            selected["paths"],
            serde_json::json!([folder.path().join("notes.md").display().to_string()])
        );

        act(
            &mut app,
            "write-file-here",
            &serde_json::json!({"name": "todo.txt", "text": "milk\neggs\n"}),
        );
        assert_eq!(
            std::fs::read_to_string(folder.path().join("todo.txt")).unwrap(),
            "milk\neggs\n"
        );
        act(
            &mut app,
            "compress-here",
            &serde_json::json!({"entries": ["notes.md", "todo.txt"]}),
        );
        assert!(folder.path().join("Archive.zip").exists());

        assert!(app.undo());
        app.settle();
        assert!(
            !folder.path().join("Archive.zip").exists(),
            "undo trashes it"
        );
        assert!(folder.path().join("notes.md").exists());

        act(
            &mut app,
            "compress-here",
            &serde_json::json!({"entries": ["notes.md", "todo.txt"], "name": "Bundle", "format": "tar.zst"}),
        );
        let bundle = folder.path().join("Bundle.tar.zst");
        assert!(bundle.is_file());
        act(
            &mut app,
            "extract-here",
            &serde_json::json!({"archive": bundle.display().to_string()}),
        );
        assert_eq!(
            std::fs::read_to_string(folder.path().join("Bundle/todo.txt")).unwrap(),
            "milk\neggs\n",
            "two entries at the top extract into a folder named after the archive"
        );
        act(
            &mut app,
            "extract-here",
            &serde_json::json!({"archive": bundle.display().to_string(), "if_exists": "skip"}),
        );
        assert!(
            !folder.path().join("Bundle 2").exists(),
            "skip extracts nothing"
        );
        undo_settled(&mut app);
        assert!(
            folder.path().join("Bundle").exists(),
            "the skip changed nothing"
        );
        undo_settled(&mut app);
        assert!(
            !folder.path().join("Bundle").exists(),
            "undo trashes the extraction"
        );
    }

    phase only_the_device_a_change_names_carries_it_out;
    {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().display().to_string();
        let mut laptop = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let mut phone = TestApp::<FileExplorerShell>::on_device("phone").unwrap();
        sync(&mut laptop, &mut phone);
        laptop
            .invoke(FileExplorer::NEW_FOLDER.with(file_explorer::NewFolder {
                device: "phone".into(),
                parent: AbsolutePath::new(&parent).unwrap(),
                name: "Inbox".parse().unwrap(),
            }))
            .unwrap();
        laptop.settle();
        assert!(!root.path().join("Inbox").exists(), "the laptop waits");
        assert_eq!(laptop.state().ops.pending().count(), 1);

        sync(&mut laptop, &mut phone);
        assert!(
            root.path().join("Inbox").is_dir(),
            "the phone carried it out"
        );
        sync(&mut laptop, &mut phone);
        let state = laptop.state();
        let (id, _) = state.ops.entries().next().unwrap();
        assert_eq!(
            state.ops.outcome(id),
            Some(&Settled::Done(vec![Change::Created {
                path: root.path().join("Inbox").display().to_string()
            }])),
            "what it did reaches every replica"
        );
    }
}

#[test]
fn copies_and_row_edits_are_undoable() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let folder = tempfile::tempdir_in(&home).unwrap();
    std::fs::create_dir(folder.path().join("Projects")).unwrap();
    std::fs::write(folder.path().join("notes.txt"), b"n").unwrap();
    std::fs::write(folder.path().join("plan.txt"), b"p").unwrap();
    let mut app =
        TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
    let row = |name: &str| crate::search_provider::entry_resource_id(&folder.path().join(name));
    let projects = folder.path().join("Projects").display().to_string();

    app.effect_on(
        &row("notes.txt"),
        &FilesEffect::Copy {
            entry: row("notes.txt"),
        },
    )
    .unwrap();
    app.effect(&FilesEffect::GoTo {
        path: AbsolutePath::new(&projects).unwrap(),
    })
    .unwrap();
    app.effect(&FilesEffect::Paste { conflict: None }).unwrap();
    assert!(
        folder.path().join("Projects/notes.txt").exists(),
        "a finished paste has its copy in place"
    );
    undo_settled(&mut app);
    assert!(
        !folder.path().join("Projects/notes.txt").exists(),
        "the copy goes"
    );
    assert!(
        folder.path().join("notes.txt").exists(),
        "the original stays"
    );

    app.effect(&FilesEffect::GoTo {
        path: AbsolutePath::new(folder.path()).unwrap(),
    })
    .unwrap();
    app.effect_on(
        &row("plan.txt"),
        &FilesEffect::Cut {
            entry: row("plan.txt"),
        },
    )
    .unwrap();
    app.effect(&FilesEffect::GoTo {
        path: AbsolutePath::new(&projects).unwrap(),
    })
    .unwrap();
    app.effect(&FilesEffect::Paste { conflict: None }).unwrap();
    assert!(
        folder.path().join("Projects/plan.txt").exists(),
        "a finished paste after Cut has moved the file"
    );
    undo_settled(&mut app);
    assert!(
        folder.path().join("plan.txt").exists(),
        "cut and paste moves back"
    );

    app.effect(&FilesEffect::GoTo {
        path: AbsolutePath::new(folder.path()).unwrap(),
    })
    .unwrap();
    act_on(
        &mut app,
        &row("plan.txt"),
        "rename",
        &serde_json::json!({"name": "agenda.txt"}),
    );
    assert!(folder.path().join("agenda.txt").exists());
    act_on(
        &mut app,
        &row("agenda.txt"),
        "move-to",
        &serde_json::json!({"to": projects}),
    );
    assert!(folder.path().join("Projects/agenda.txt").exists());
    act_on(&mut app, &row("notes.txt"), "trash", &serde_json::json!({}));
    assert!(!folder.path().join("notes.txt").exists(), "trashed");
    let undone = app.undo();
    app.settle();
    assert!(undone);
    assert!(
        folder.path().join("notes.txt").exists(),
        "undo restores it from Trash"
    );
}

fn undo_settled(app: &mut TestApp<FileExplorerShell>) {
    assert!(app.undo());
    settle_ops(app);
}

fn settle_ops(app: &mut TestApp<FileExplorerShell>) {
    tilo_test_env::wait_until("every queued change to settle", || {
        app.settle();
        app.state().ops.pending().next().is_none()
    });
}

fn act(app: &mut TestApp<FileExplorerShell>, id: &str, args: &serde_json::Value) {
    app.harness()
        .call_on(
            <FileExplorer as ui::tilo_resource::Resource>::KIND,
            id,
            args.clone(),
        )
        .unwrap();
    settle_ops(app);
}

tilo_test_env::scenario! {
    fn history_and_reads_speak_the_apps_sentences_with_their_devices;

    phase history_entries_render_as_the_apps_sentences;
    {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("draft.txt"), b"x").unwrap();
        let folder = root.path().display().to_string();
        let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let before = app.versions().len();
        let kind = <FileExplorer as ui::tilo_resource::Resource>::KIND;
        let agent = ui::tilo_actions::ActionInvoker::agent("claude")
            .acting_for("ada");
        for (verb, args) in [
            ("add-bookmark", serde_json::json!({ "place": "~/Projects" })),
            (
                "new-folder",
                serde_json::json!({ "device": "laptop", "parent": folder, "name": "Inbox" }),
            ),
            (
                "rename",
                serde_json::json!({
                    "device": "laptop",
                    "folder": folder,
                    "entry": "draft.txt",
                    "name": "final.txt",
                }),
            ),
            (
                "write-file",
                serde_json::json!({
                    "device": "laptop",
                    "folder": folder,
                    "name": "agent-notes.md",
                    "text": "# Notes from your agent\n",
                }),
            ),
        ] {
            app.call_by(agent.clone(), kind, verb, args)
                .unwrap_or_else(|error| panic!("{verb}: {error:?}"));
        }
        app.settle();
        let english = include_str!("../../locales/en-US.ftl");
        let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .versions()
            .into_iter()
            .skip(before)
            .filter_map(|version| version.context)
            .filter(|entry| entry.change != ui::tilo_actions::ChangeKind::External)
            .map(|mut entry| {
                if let Some(ui::tilo_actions::entry::Did::Verb(verb)) = &mut entry.what {
                    FileExplorerShell::ID.clone_into(&mut verb.action.app);
                }
                reader.sentence(&entry.with_app(FileExplorerShell::ID, FileExplorerShell::TITLE))
            })
            .collect();
        assert_eq!(
            sentences,
            [
                "Added ~/Projects to Favorites",
                "Created the folder Inbox",
                "Renamed draft.txt to final.txt",
                "Wrote agent-notes.md",
            ]
        );
    }

    phase a_read_answers_the_places_and_each_change_with_its_device;
    {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().display().to_string();
        let mut laptop = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        act(
            &mut laptop,
            "new-folder",
            &serde_json::json!({"device": "laptop", "parent": parent, "name": "Inbox"}),
        );
        let read = laptop
            .harness()
            .call_on(
                <FileExplorer as ui::tilo_resource::Resource>::KIND,
                "read",
                serde_json::json!({}),
            )
            .unwrap()
            .completed()
            .and_then(|output| output.value)
            .expect("a read answers at once");
        assert_eq!(
            read["places"].as_array().map(Vec::len),
            Some(crate::places::DEFAULT_PLACES.len()),
            "{read}"
        );
        let change = &read["changes"][0];
        assert_eq!(change["device"], "laptop", "{read}");
        assert_eq!(change["settled"]["state"], "done", "{read}");
    }

    phase a_write_names_the_file_it_changes;
    {
        // The record holds no item for a file on a device: the change names
        // the file itself, so History says which file was written, not only
        // which folder.
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().display().to_string();
        let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let before = app.versions().len();
        let kind = <FileExplorer as ui::tilo_resource::Resource>::KIND;
        let agent = ui::tilo_actions::ActionInvoker::agent("claude").acting_for("ada");
        app.call_by(
            agent,
            kind,
            "write-file",
            serde_json::json!({
                "device": "laptop",
                "folder": folder,
                "name": "agent-notes.md",
                "text": "# Notes from your agent\n",
            }),
        )
        .unwrap_or_else(|error| panic!("write-file: {error:?}"));
        app.settle();
        let entry = app
            .versions()
            .into_iter()
            .skip(before)
            .filter_map(|version| version.context)
            .find(|entry| {
                matches!(
                    &entry.what,
                    Some(ui::tilo_actions::Did::Verb(verb)) if verb.action.action == "write-file"
                )
            })
            .expect("the write is one change");
        let item = entry
            .item
            .as_ref()
            .expect("the change names the file it writes");
        assert_eq!(item.label, "agent-notes.md");
    }
}
