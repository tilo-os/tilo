use super::*;
use ui::prelude::TestApp;
use ui::tilo_resource::{Replica, Settled};

/// A new document holds the standard places, which the window publishes
/// as rows that open; the filesystem verbs ride the resource.
#[test]
fn a_new_document_publishes_the_standard_places() {
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
        "cloud-import",
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

fn sync(a: &mut TestApp<FileExplorerShell>, b: &mut TestApp<FileExplorerShell>) {
    let mut doc = b.peer_document();
    a.merge_peer(&mut doc).unwrap();
    b.merge_peer(&mut doc).unwrap();
    a.settle();
    b.settle();
}

fn pin(place: &str, at: Option<usize>) -> Invoke<FileExplorer> {
    FileExplorer::ADD_BOOKMARK.with(file_explorer::AddBookmark {
        place: place.into(),
        at,
    })
}

/// Bookmarks are edited place by place, so two people pinning at once
/// both keep their pin, and the same folder pinned twice is one pin.
#[test]
fn bookmark_edits_are_granular() {
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

/// Pinning a folder in the window is the resource's action: it lands in
/// the document, undo takes it back, and the sidebar follows.
#[test]
fn pinning_a_folder_is_a_bookmark_action() {
    let folder = tempfile::tempdir().unwrap();
    let mut app = TestApp::<FileExplorerShell>::new().unwrap();
    assert_eq!(app.state().bookmarks(), crate::places::DEFAULT_PLACES);
    app.with_surface(|files: &mut FileExplorerWidget| {
        files.apply_command(crate::FileCommand::AddFavorite(folder.path().to_path_buf()))
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
            files.favorites().iter().any(|f| f.path == folder.path())
        })
        .unwrap();
    assert!(!shown, "the sidebar shows the stored places");

    app.invoke(pin(&place, None)).unwrap();
    let shown = app
        .with_surface(|files: &mut FileExplorerWidget| {
            files.favorites().iter().any(|f| f.path == folder.path())
        })
        .unwrap();
    assert!(shown, "an agent's pin reaches the sidebar");
}

/// The view preferences are the person's settings, not the document.
#[test]
fn view_preferences_are_personal_settings() {
    let mut app = TestApp::<FileExplorerShell>::new().unwrap();
    let before = app.peer_document().get_heads();
    app.with_surface(|files: &mut FileExplorerWidget| {
        files.set_view_mode(crate::model::ViewMode::Grid);
        files.apply_command(crate::FileCommand::ZoomIn);
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

/// A capture browses the folders written for its scenario and reads
/// nothing of the machine it runs on; its view state is staged once laid
/// out.
#[test]
fn captures_browse_their_own_scene() {
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

#[test]
fn a_launch_opens_the_folder_it_names() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("launched.txt"), b"x").unwrap();
    let mut app =
        TestApp::<FileExplorerShell>::launched(&[directory.path().to_str().unwrap()]).unwrap();
    let frame = app.harness().inspection_json().unwrap();
    assert!(frame.contains("launched.txt"), "{frame}");
}

/// Invoke the action `id` of the published resource `on` with `args`
/// through the door, as an agent does, and let the device carry out
/// what it queued.
fn act_on(app: &mut TestApp<FileExplorerShell>, on: &str, id: &str, args: &serde_json::Value) {
    app.harness()
        .call_on_confirmed(on, id, args.clone())
        .unwrap();
    app.settle();
}

#[test]
fn published_batch_copy_is_one_undoable_operation_and_cut_requires_write() {
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
        path: destination.display().to_string(),
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
            path: root.path().display().to_string(),
        })
        .unwrap();
    viewer.effect(&FilesEffect::SelectAll).unwrap();
    viewer.effect(&FilesEffect::CopySelection).unwrap();
    assert!(viewer.effect(&FilesEffect::CutSelection).is_err());
    for name in ["a.txt", "b.txt", "c.txt"] {
        assert!(root.path().join(name).exists());
    }
}

#[test]
fn published_selection_capture_observes_the_real_selected_rows() {
    let guard = crate::model::fixture_io_guard::DenyAmbientIo::new();
    let mut app = TestApp::<FileExplorerShell>::capture(Some("selection-actions")).unwrap();
    assert_eq!(
        app.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
        Some(2)
    );
    let frame = app.harness().inspection_json().unwrap();
    assert!(frame.contains("Notes.txt"), "{frame}");
    assert!(frame.contains("Recipes.md"), "{frame}");
    assert_eq!(guard.attempts(), 0, "a capture read ambient state");
}

#[test]
fn published_selection_changes_stay_in_the_addressed_files_window() {
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
        source.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
        Some(2)
    );
    assert_eq!(
        other.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
        Some(1)
    );
    source.effect(&FilesEffect::ClearSelection).unwrap();
    assert_eq!(
        source.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
        Some(0)
    );
    assert_eq!(
        other.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
        Some(1)
    );
    assert!(source.state().ops.is_empty());
    assert!(other.state().ops.is_empty());
}

/// Copying and pasting are queued and undoable like every filesystem
/// change: undo moves a copy to Trash and a cut-and-paste back. A row
/// offers rename, move and trash on its entry; undoing the trash puts
/// the file back.
#[test]
fn copies_and_row_edits_are_undoable() {
    // Inside the home, so Trash is the home's and its entries are known.
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
        path: projects.clone(),
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
        path: folder.path().display().to_string(),
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
        path: projects.clone(),
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
        path: folder.path().display().to_string(),
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

/// Undo, as Ctrl+Z does, then run frames until the Files document
/// records the revert carried out: a keystroke has no caller to answer,
/// and the device reverts in a later frame, as a job.
fn undo_settled(app: &mut TestApp<FileExplorerShell>) {
    assert!(app.undo());
    settle_ops(app);
}

/// Run frames until every queued change settled: a copy, a move, an
/// extraction or a packing runs on a worker.
fn settle_ops(app: &mut TestApp<FileExplorerShell>) {
    let deadline = tilo_test_env::hang_deadline();
    app.settle();
    while app.state().ops.pending().next().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "a queued change never settled"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.settle();
    }
}

/// Invoke the resource's action `id` with `args` through the door, as
/// an agent does, and let the device carry out what it queued.
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

/// A rename, a move or a trash queued here reaches the disk, so grants
/// treat it as an outside effect, yet its undo queues the revert: the
/// contract says it can be taken back, bound to a folder or not, so a
/// reviewer is not told it sends anything away.
#[test]
fn queued_file_changes_declare_that_they_can_be_undone() {
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
        .declaration("rename-here", "Rename an entry here")
        .unwrap();
    assert_eq!(bound.kind, ActionKind::Outside);
    let spec = ui::tilo_resource::declaration_spec(&*bound);
    assert!(spec.contract.can_be_undone());
}

/// Creating, renaming and moving files are document actions: queued in
/// the Files document, carried out by this device's drain, settled with
/// what they changed, and undone by queueing their revert.
#[test]
fn filesystem_changes_are_undoable_document_actions() {
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

    // Undo takes the move back on the filesystem, then the rename.
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

/// The window's other work is its effects: copying a row, pasting,
/// showing a folder, pinning the folder shown.
#[test]
fn the_windows_work_is_its_effects() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir(folder.path().join("Projects")).unwrap();
    std::fs::write(folder.path().join("notes.txt"), b"n").unwrap();
    let mut app =
        TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
    // A row's effect, bound to it, as the palette offers it on the row.
    let row = crate::search_provider::entry_resource_id(&folder.path().join("notes.txt"));
    app.effect_on(&row, &FilesEffect::Copy { entry: row.clone() })
        .unwrap();
    app.effect(&FilesEffect::GoTo {
        path: folder.path().join("Projects").display().to_string(),
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

/// A caller with no screen reads what the files say, writes a new file
/// and compresses entries of the folder shown, through the same
/// published door, and each change undoes like any other.
#[test]
fn files_are_read_written_and_compressed_through_actions() {
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

    // Compress to tar.zst and extract it again, through the actions.
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

/// A change queued for another device waits for that device: only the
/// device whose disk it names carries it out, and both see what it did.
#[test]
fn only_the_device_a_change_names_carries_it_out() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().display().to_string();
    let mut laptop = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
    let mut phone = TestApp::<FileExplorerShell>::on_device("phone").unwrap();
    sync(&mut laptop, &mut phone);
    laptop
        .invoke(FileExplorer::NEW_FOLDER.with(file_explorer::NewFolder {
            device: "phone".into(),
            parent,
            name: "Inbox".into(),
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

fn import_destination(path: &std::path::Path) -> ImportDestination {
    let path = path.display().to_string();
    ImportDestination {
        identity: crate::cloud_transfer::pin_destination(&path, std::iter::empty()).unwrap(),
        path,
    }
}

// Only for requests whose path intentionally cannot be pinned. This is
// serializable evidence, not permission to read a private cloud source.
fn nonexistent_import_destination(path: &std::path::Path) -> ImportDestination {
    ImportDestination {
        path: path.display().to_string(),
        identity: serde_json::from_value(serde_json::json!({"device": 0, "inode": 0})).unwrap(),
    }
}

fn import_entry() -> tilo_cloud_files::cloud::ImportEntry {
    tilo_cloud_files::cloud::ImportEntry {
        id: "remote-document".into(),
        name: "document.txt".into(),
        kind: tilo_cloud_files::cloud::ImportKind::File,
        revision: Some("revision-1".into()),
        size: Some(12),
        content_writable: true,
    }
}

// Cloud import stages through descriptor-relative paths only Linux provides.
#[cfg(target_os = "linux")]
#[test]
fn cloud_import_queues_on_its_device_and_undo_reverts_its_record() {
    // Keep this real service socket private to the test: other concurrently
    // running Files tests can also browse the default cloud endpoint.
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
    let root = tempfile::tempdir().unwrap();
    let socket = tilo_cloud_files::cloud::default_files_socket().unwrap();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        use tilo_cloud_files::cloud::{
            CLOUD_FRAMING, CloudRequest, CloudResponse, ImportReadState, ImportReply, ImportRequest,
        };
        let (mut stream, _) = listener.accept().unwrap();
        while let Some(CloudRequest::Import { request, .. }) = CLOUD_FRAMING
            .read_value::<_, CloudRequest>(&mut stream)
            .unwrap()
        {
            let reply = match request {
                ImportRequest::StartRead { .. } => ImportReply::Preparing {
                    session: "session".into(),
                },
                ImportRequest::ReadStatus { .. } => {
                    ImportReply::ReadState(ImportReadState::Ready { size: 6 })
                }
                ImportRequest::ReadChunk { .. } => ImportReply::Chunk {
                    bytes: b"import".to_vec(),
                    eof: true,
                },
                ImportRequest::CloseRead { .. } => ImportReply::Closed,
                ImportRequest::Browse { .. } => panic!("unexpected request"),
            };
            CLOUD_FRAMING
                .write_value(&mut stream, &CloudResponse::Import(reply))
                .unwrap();
        }
        listener
    });
    let folder = root.path().display().to_string();
    let op = FileOp::CloudImport {
        job_id: uuid::Uuid::new_v4().to_string(),
        account: "connected-account".into(),
        entries: vec![import_entry()],
        destination: crate::cloud_transfer::pin_destination(&folder, std::iter::empty()).unwrap(),
        folder,
    };
    app.invoke(FileExplorer::queueing("laptop", op.clone()).unwrap())
        .unwrap();
    app.settle();
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while app.state().ops.pending().next().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "import did not settle"
        );
        app.settle();
        std::thread::yield_now();
    }
    let state = app.state();
    let (id, entry) = state.ops.entries().next().unwrap();
    assert_eq!(entry.value.device, "laptop");
    assert_eq!(entry.value.op, op);
    let outcome = entry.settled.as_ref().unwrap();
    assert!(outcome.is_done(), "{outcome:?}");
    assert!(matches!(
        outcome.result().as_slice(),
        [Change::CloudImported { .. }]
    ));
    assert_eq!(
        std::fs::read(root.path().join(import_entry().name)).unwrap(),
        b"import"
    );
    let id = id.as_str().to_owned();

    assert!(app.undo());
    app.settle();
    assert!(app.state().ops.entries().any(|(_, entry)| {
        entry.value.device == "laptop"
            && entry.value.op == FileOp::Revert { op: id.clone() }
            && entry.settled.as_ref().is_some_and(Settled::is_done)
    }));
    assert!(!root.path().join(import_entry().name).exists());

    assert_cloud_import_history_is_structural(&app, &id, &op, root.path());
    assert_forged_cloud_jobs_denied(&mut app, op, root.path(), &listener);
    std::fs::remove_file(socket).unwrap();
}

/// Exercise the recorded production verb, not a hand-built outbox edit.
/// A historical import describes what happened even when its destination
/// has moved and a different device/person is reading it.
fn assert_cloud_import_history_is_structural(
    app: &TestApp<FileExplorerShell>,
    entry: &str,
    op: &FileOp,
    folder: &std::path::Path,
) {
    use ui::tilo_actions::{ActionEnvelope, ActionId, ActionRegistry, ExecutionLocus};
    use ui::tilo_resource::Resource as _;

    assert!(ui::bound_dispatch::executing_host_scope().is_none());
    let mut document = app.peer_document();
    let recorded = app
        .versions()
        .into_iter()
        .find(|version| {
            version.context.as_ref().is_some_and(|context| {
                context.action.as_ref().is_some_and(|action| {
                    String::from_utf8_lossy(&action.payload).contains("cloud-import")
                })
            })
        })
        .expect("the admitted import records its production action");
    let metadata = document
        .get_change_meta_by_hash(&recorded.hash)
        .unwrap()
        .into_owned();
    let context = recorded.context.unwrap();
    let encoded = context.action.clone().expect("recorded replay payload");
    let registry = ActionRegistry::with(ui::tilo_resource::group::register::<FileExplorer>);
    let envelope = ActionEnvelope {
        id: ActionId::new(),
        action: encoded.clone(),
        base_heads: metadata.deps.iter().map(|head| head.0).collect(),
        timestamp: metadata.timestamp,
        locus: ExecutionLocus::ClientLocal,
        actor: Some(metadata.actor.to_bytes().to_vec()),
        context: Some(context),
    };
    let displaced = folder.with_extension("historical-destination");
    std::fs::rename(folder, &displaced).unwrap();
    assert!(
        !folder.exists(),
        "history must not need the old destination"
    );

    let mut base = document.fork_at(&metadata.deps).unwrap();
    base.set_actor(ui::automerge::ActorId::from(
        b"other-device-reader".to_vec(),
    ));
    let action = registry.decode(&encoded).unwrap();
    assert!(
        ui::tilo_actions::executor::inverse_as(
            &mut base,
            action.as_ref(),
            metadata.actor.clone().into_owned(),
        )
        .is_some(),
        "undo preview must evaluate the import without host authority or filesystem IO"
    );
    assert!(FileExplorer::read(&base).unwrap().ops.is_empty());
    let mut peer = Replica::<FileExplorer>::from_doc(base);
    peer.replay(&envelope).unwrap();
    assert_eq!(peer.doc_mut().get_heads(), vec![recorded.hash]);
    let state = peer.state().unwrap();
    let (replayed_id, task) = state.ops.entries().next().unwrap();
    assert_eq!(replayed_id.as_str(), entry);
    assert_eq!(task.value.device, "laptop");
    assert_eq!(
        &task.value.op, op,
        "replay preserves the exact queued task data"
    );
    assert_eq!(
        ui::tilo_actions::executor::verify_change(&mut document, recorded.hash, &registry).unwrap(),
        ui::tilo_actions::executor::Verified::Reproduced,
    );
    let preview = app.past_version(&[recorded.hash]).unwrap();
    let shown = preview.state();
    let (_, historical) = shown.ops.entries().next().unwrap();
    assert_eq!(historical.value, task.value);
    assert!(historical.settled.is_none());
    assert!(ui::bound_dispatch::executing_host_scope().is_none());
    std::fs::rename(displaced, folder).unwrap();
    // The caller now reopens the already consumed exact entry and checks
    // that neither replay nor either preview reminted its local approval.
}

fn assert_forged_cloud_jobs_denied(
    app: &mut TestApp<FileExplorerShell>,
    op: FileOp,
    root: &std::path::Path,
    listener: &std::os::unix::net::UnixListener,
) {
    // A collaborator can write perfectly valid outbox data without going
    // through cloud-import. Neither replaying the exact approved job under
    // another entry nor changing that job may spend the host's consent.
    let mut changed_account = op.clone();
    let FileOp::CloudImport { account, .. } = &mut changed_account else {
        panic!("the admitted job must be a cloud import")
    };
    *account = "another-private-account".into();
    let mut changed_entries = op.clone();
    let FileOp::CloudImport { entries, .. } = &mut changed_entries else {
        panic!("the admitted job must be a cloud import")
    };
    entries[0].id = "another-private-document".into();
    let mut changed_job = op.clone();
    let FileOp::CloudImport { job_id, .. } = &mut changed_job else {
        panic!("the admitted job must be a cloud import")
    };
    *job_id = uuid::Uuid::new_v4().to_string();
    let admitted_id = app
        .state()
        .ops
        .entries()
        .find(|(_, entry)| entry.value.op == op)
        .map(|(id, _)| id.clone())
        .expect("the original admitted import remains recorded");
    // Re-opening the original consumed entry must not re-approve it either.
    for (attempt, forged) in [
        op.clone(),
        op,
        changed_account,
        changed_entries,
        changed_job,
    ]
    .into_iter()
    .enumerate()
    {
        let forged_id: ui::tilo_resource::Id = if attempt == 0 {
            admitted_id.clone()
        } else {
            uuid::Uuid::new_v4().to_string().into()
        };
        let mut ops = app.state().ops;
        ops.push(
            forged_id.clone(),
            FileTask {
                device: "laptop".into(),
                op: forged,
            },
        );
        app.merge_peer_edit(|doc| {
            ui::tilo_resource::model::write_key(doc, &ui::automerge::ROOT, "ops", &ops).unwrap();
        });
        app.settle();
        let state = app.state();
        let outcome = state.ops.outcome(&forged_id).expect("forged job settled");
        assert!(
            matches!(outcome, Settled::Failed { reason, partial }
                if reason == "cloud import has no unused local approval" && partial.is_empty()),
            "unapproved outbox entry must fail without local changes: {outcome:?}"
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "a forged entry must not even connect to the cloud source"
        );
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
        assert!(!root.join(import_entry().name).exists());
    }
}

#[test]
fn mounted_import_requires_private_admission_and_copies_editable_outputs() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("Google.docx");
    let folder = root.path().join("Copies");
    std::fs::write(&source, b"native Google view").unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::create_dir(&folder).unwrap();
    let op = FileOp::ImportMounted {
        paths: vec![source.display().to_string()],
        folder: folder.display().to_string(),
    };
    let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
    let forged_id: Id = "forged-mounted".into();
    let mut ops = app.state().ops;
    ops.push(
        forged_id.clone(),
        FileTask {
            device: "laptop".into(),
            op: op.clone(),
        },
    );
    app.merge_peer_edit(|doc| {
        ui::tilo_resource::model::write_key(doc, &ui::automerge::ROOT, "ops", &ops).unwrap();
    });
    app.settle();
    assert!(
        matches!(app.state().ops.outcome(&forged_id), Some(Settled::Failed { partial, .. }) if partial.is_empty())
    );
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    assert!(
        matches!(crate::file_ops::execute(&op, None), Settled::Failed { partial, .. } if partial.is_empty())
    );
    app.invoke(FileExplorer::queueing("laptop", op).unwrap())
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while !folder.join("Google.docx").exists() || app.state().ops.pending().next().is_some() {
        assert!(std::time::Instant::now() < deadline);
        app.settle();
        std::thread::yield_now();
    }
    assert_eq!(
        std::fs::read(folder.join("Google.docx")).unwrap(),
        b"native Google view"
    );
    assert_ne!(
        std::fs::metadata(folder.join("Google.docx"))
            .unwrap()
            .permissions()
            .mode()
            & 0o200,
        0
    );
    assert_eq!(
        std::fs::metadata(source).unwrap().permissions().mode() & 0o222,
        0
    );
    assert!(app.undo());
    app.settle();
    assert!(!folder.join("Google.docx").exists());
}

#[test]
fn mounted_import_record_verifies_offline_without_host_or_source() {
    use ui::tilo_actions::ActionRegistry;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.docx");
    let destination = root.path().join("Copies");
    std::fs::write(&source, b"private mounted representation").unwrap();
    std::fs::create_dir(&destination).unwrap();
    let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
    let op = FileOp::ImportMounted {
        paths: vec![source.display().to_string()],
        folder: destination.display().to_string(),
    };
    app.invoke(FileExplorer::queueing("laptop", op).unwrap())
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while app.state().ops.pending().next().is_some() {
        assert!(std::time::Instant::now() < deadline);
        app.settle();
        std::thread::yield_now();
    }
    assert!(
        app.state()
            .ops
            .entries()
            .next()
            .unwrap()
            .1
            .settled
            .as_ref()
            .unwrap()
            .is_done()
    );
    let recorded = app
        .versions()
        .into_iter()
        .find(|version| {
            version
                .context
                .as_ref()
                .and_then(|context| context.action.as_ref())
                .is_some_and(|action| {
                    String::from_utf8_lossy(&action.payload).contains("import-mounted")
                })
        })
        .unwrap();
    root.close().unwrap();
    assert!(ui::bound_dispatch::executing_host_scope().is_none());
    let mut document = app.peer_document();
    document.set_actor(ui::automerge::ActorId::from(b"other-device".to_vec()));
    let registry = ActionRegistry::with(ui::tilo_resource::group::register::<FileExplorer>);
    assert_eq!(
        ui::tilo_actions::executor::verify_change(&mut document, recorded.hash, &registry).unwrap(),
        ui::tilo_actions::executor::Verified::Reproduced,
    );
    let preview = app.past_version(&[recorded.hash]).unwrap();
    let state = preview.state();
    assert_eq!(state.ops.entries().count(), 1);
    assert!(state.ops.entries().next().unwrap().1.settled.is_none());
}

fn invoke_confirmed_by(
    app: &mut TestApp<FileExplorerShell>,
    role: ui::permissions::Role,
    invoker: ui::tilo_actions::ActionInvoker,
    invoke: &ui::tilo_resource::Invoke<FileExplorer>,
) -> Result<(), ui::tilo_actions::DoorError> {
    if let Some(refusal) = invoke.refusal() {
        return Err(ui::tilo_actions::DoorError::InvalidArguments(
            refusal.to_owned(),
        ));
    }
    // Discover as the local host, then exercise admission under the
    // caller's role: discovery is not an authority grant.
    let address = app
        .actions()?
        .into_iter()
        .find(|action| action.address.action == invoke.id)
        .ok_or_else(|| {
            ui::tilo_actions::DoorError::Failed(format!("action {} is not published", invoke.id))
        })?
        .address;
    app.set_role(role);
    let instance_id = app.harness().component_instance_id().unwrap();
    app.harness().start_bound_job_for(
        Some(invoker),
        &ui::bound_dispatch::BoundActionAddress {
            instance_id,
            address,
        },
        &invoke.arguments,
        true,
    )?;
    Ok(())
}

#[test]
fn mounted_import_refuses_guest_public_actions() {
    use ui::permissions::Role;
    use ui::tilo_actions::ActionInvoker;
    for role in [Role::Editor, Role::Viewer] {
        let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
        let result = invoke_confirmed_by(
            &mut app,
            role,
            ActionInvoker::actor("guest"),
            &FileExplorer::IMPORT_MOUNTED.with(file_explorer::ImportMounted {
                device: "test-device".into(),
                paths: vec!["/private/Google.docx".into()],
                folder: "/private/Copies".into(),
            }),
        );
        assert!(result.is_err());
        app.settle();
        assert_eq!(app.state().ops.entries().count(), 0);
    }
}

#[test]
fn cloud_import_refuses_another_device_before_pinning_the_destination() {
    let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
    let root = tempfile::tempdir().unwrap();
    // This path does not exist on the sender: a destination lookup would
    // fail differently, proving the host-device check precedes pinning.
    let folder = root.path().join("receiving-device-folder");
    let error = app
        .invoke(FileExplorer::CLOUD_IMPORT.with(file_explorer::CloudImport {
            job_id: "remote-import-attempt".into(),
            device: "phone".into(),
            account: "connected-account".into(),
            entries: vec![serde_json::json!(import_entry())],
            destination: nonexistent_import_destination(&folder),
        }))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Invoke Files on the receiving device and choose the destination there")
    );
    app.settle();
    assert_eq!(app.state().ops.entries().count(), 0);
    assert!(!folder.exists());
}

#[test]
fn cloud_import_without_a_checked_host_binding_fails_closed() {
    let mut replica = Replica::<FileExplorer>::new();
    let error = replica
        .apply(FileExplorer::CLOUD_IMPORT.with(file_explorer::CloudImport {
            device: "laptop".into(),
            job_id: "unbound-import".into(),
            account: "private-account".into(),
            entries: vec![serde_json::json!(import_entry())],
            destination: nonexistent_import_destination(std::path::Path::new(
                "not-an-absolute-destination",
            )),
        }))
        .unwrap_err();
    assert!(error.to_string().contains("host's person"), "{error}");
    assert_eq!(replica.state().unwrap().ops.entries().count(), 0);
}

#[test]
fn cloud_guests_cannot_queue_imports_even_with_workspace_ownership() {
    use ui::permissions::Role;
    use ui::tilo_actions::ActionInvoker;

    for (actor, role) in [
        ("guest-owner", Role::Owner),
        ("guest-editor", Role::Editor),
        ("guest-viewer", Role::Viewer),
    ] {
        let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("must-not-be-pinned");
        let error = invoke_confirmed_by(
            &mut app,
            role,
            ActionInvoker::actor(actor),
            &FileExplorer::CLOUD_IMPORT.with(file_explorer::CloudImport {
                device: "test-device".into(),
                job_id: format!("{actor}-attempt"),
                account: "private-account".into(),
                entries: vec![serde_json::json!(import_entry())],
                destination: nonexistent_import_destination(&folder),
            }),
        )
        .unwrap_err();
        // Editors and owners pass workspace permission admission, but
        // neither owns this host's account. Viewers may be denied earlier.
        if role != Role::Viewer {
            assert!(
                error.to_string().contains("host's person"),
                "{actor}: {error}"
            );
        }
        app.settle();
        assert_eq!(app.state().ops.entries().count(), 0, "{actor}");
        assert!(!folder.exists(), "{actor}");
    }
}

#[test]
fn a_valid_destination_witness_does_not_authorize_a_fresh_guest_import() {
    let root = tempfile::tempdir().unwrap();
    let destination = import_destination(root.path());
    let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
    app.set_role(ui::permissions::Role::Owner);
    let error = app
        .call_by(
            ui::tilo_actions::ActionInvoker::actor("guest-owner"),
            <FileExplorer as ui::tilo_resource::Resource>::KIND,
            "cloud-import",
            serde_json::json!({
                "device": "test-device",
                "job_id": uuid::Uuid::new_v4().to_string(),
                "account": "private-account",
                "entries": [import_entry()],
                "destination": destination,
            }),
        )
        .unwrap_err();
    assert!(error.to_string().contains("host's person"), "{error}");
    app.settle();
    assert!(app.state().ops.is_empty());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn cloud_effects_refuse_guest_invocations_before_accessing_accounts() {
    use ui::permissions::Role;
    use ui::tilo_actions::ActionInvoker;

    for (actor, role) in [
        ("guest-owner", Role::Owner),
        ("guest-editor", Role::Editor),
        ("guest-viewer", Role::Viewer),
    ] {
        let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
        app.set_role(role);
        for effect in [
            FilesEffect::BrowseCloud {
                account: "private-account".into(),
                folder: None,
            },
            FilesEffect::MoreCloud,
            FilesEffect::SelectCloud {
                entry: "private-entry".into(),
            },
            FilesEffect::ImportCloud { folder: None },
            FilesEffect::CloudProgress,
            FilesEffect::CancelCloud,
            FilesEffect::RetryCloud,
            FilesEffect::RevealCloud,
        ] {
            use ui::tilo_resource::Effect as _;
            let error = app
                .call_by(
                    ActionInvoker::actor(actor),
                    <FileExplorer as ui::tilo_resource::Resource>::KIND,
                    effect.id(),
                    serde_json::Value::Object(effect.arguments()),
                )
                .unwrap_err();
            if matches!(effect, FilesEffect::CloudProgress) {
                assert!(
                    error.to_string().contains("host's person"),
                    "{actor}: {effect:?}: {error}"
                );
            } else {
                // Private view admission rejects a guest before the
                // cloud handler can inspect any account or transfer.
                assert!(
                    matches!(&error, ui::tilo_actions::DoorError::Unavailable { code, .. }
                        if code == "private_view"),
                    "{actor}: {effect:?}: {error}"
                );
            }
            assert_eq!(app.state().ops.entries().count(), 0, "{actor}");
        }
    }
}

#[test]
fn cloud_attribution_and_mutable_authority_do_not_grant_personal_account_access() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    use ui::tilo_resource::Effect as _;

    for invoker in [
        ActionInvoker::actor("guest-owner").acting_for("local-person"),
        ActionInvoker::actor("guest-agent")
            .with_kind(ActorKind::Agent)
            .acting_for("local-person"),
    ] {
        let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
        // Mutable workspace authority may identify a different owner. It
        // must not replace the immutable person who owns this host.
        app.set_permissions(ui::permissions::PermissionContext::member(
            invoker.actor.clone(),
            ui::permissions::Role::Owner,
        ));
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("must-not-be-pinned");
        let error = app
            .call_by(
                invoker.clone(),
                <FileExplorer as ui::tilo_resource::Resource>::KIND,
                "cloud-import",
                serde_json::json!({
                    "device": "test-device",
                    "job_id": uuid::Uuid::new_v4().to_string(),
                    "account": "private-account",
                    "entries": [import_entry()],
                    "destination": nonexistent_import_destination(&folder),
                }),
            )
            .unwrap_err();
        assert!(error.to_string().contains("host's person"), "{error}");
        // A read effect reaches the account guard; a private View
        // effect would instead be refused earlier by the SDK.
        let effect = FilesEffect::CloudProgress;
        let error = app
            .call_by(
                invoker,
                <FileExplorer as ui::tilo_resource::Resource>::KIND,
                effect.id(),
                serde_json::Value::Object(effect.arguments()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("host's person"), "{error}");
        app.settle();
        assert_eq!(app.state().ops.entries().count(), 0);
        assert!(!folder.exists());
    }
}

#[test]
fn workspace_file_actions_remain_available_to_guests() {
    let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
    let folder = tempfile::tempdir().unwrap();
    invoke_confirmed_by(
        &mut app,
        ui::permissions::Role::Editor,
        ui::tilo_actions::ActionInvoker::actor("guest-editor"),
        &FileExplorer::NEW_FOLDER.with(file_explorer::NewFolder {
            device: "test-device".into(),
            parent: folder.path().to_string_lossy().into_owned(),
            name: "Shared folder".into(),
        }),
    )
    .unwrap();
    app.settle();
    assert!(folder.path().join("Shared folder").is_dir());
    assert_eq!(app.state().ops.entries().count(), 1);
}

#[test]
fn cloud_import_destination_rejects_sync_mounts_and_aliases() {
    let root = tempfile::tempdir().unwrap();
    let mount = root.path().join("synced");
    std::fs::create_dir(&mount).unwrap();
    let child = mount.join("child");
    std::fs::create_dir(&child).unwrap();
    for destination in [&mount, &child] {
        assert!(
            independent_destination(
                destination.to_str().unwrap(),
                std::iter::once(mount.as_path()),
            )
            .is_err()
        );
    }
    #[cfg(unix)]
    {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&child, &alias).unwrap();
        assert!(
            independent_destination(alias.to_str().unwrap(), std::iter::once(mount.as_path()),)
                .is_err()
        );
    }
    assert!(
        independent_destination(
            root.path().to_str().unwrap(),
            std::iter::once(mount.as_path()),
        )
        .is_ok()
    );
}

#[test]
fn cloud_import_refuses_invalid_requests_before_queueing() {
    let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
    let root = tempfile::tempdir().unwrap();
    let valid = file_explorer::CloudImport {
        job_id: "import-attempt".into(),
        device: "laptop".into(),
        account: "connected-account".into(),
        entries: vec![serde_json::json!(import_entry())],
        destination: import_destination(root.path()),
    };
    let mut invalid = vec![
        file_explorer::CloudImport {
            job_id: String::new(),
            ..valid.clone()
        },
        file_explorer::CloudImport {
            entries: vec![serde_json::json!({"not": "entries"})],
            ..valid.clone()
        },
        file_explorer::CloudImport {
            device: String::new(),
            ..valid.clone()
        },
        file_explorer::CloudImport {
            account: String::new(),
            ..valid.clone()
        },
        file_explorer::CloudImport {
            entries: vec![],
            ..valid.clone()
        },
        file_explorer::CloudImport {
            destination: nonexistent_import_destination(std::path::Path::new("relative")),
            ..valid.clone()
        },
        file_explorer::CloudImport {
            destination: nonexistent_import_destination(root.path()),
            ..valid.clone()
        },
        file_explorer::CloudImport {
            entries: vec![serde_json::json!(import_entry()); MAX_MOVE_ENTRIES + 1],
            ..valid.clone()
        },
        file_explorer::CloudImport {
            entries: vec![
                serde_json::json!(import_entry()),
                serde_json::json!(import_entry()),
            ],
            ..valid.clone()
        },
    ];
    for (id, name, kind) in [
        ("", "file.txt", tilo_cloud_files::cloud::ImportKind::File),
        (
            "remote",
            "../escape",
            tilo_cloud_files::cloud::ImportKind::File,
        ),
        ("remote", "..", tilo_cloud_files::cloud::ImportKind::File),
        (
            "remote",
            "bad\0name",
            tilo_cloud_files::cloud::ImportKind::File,
        ),
        (
            "remote",
            "file",
            tilo_cloud_files::cloud::ImportKind::Unsupported,
        ),
    ] {
        let mut request = valid.clone();
        let mut entry = import_entry();
        entry.id = id.into();
        entry.name = name.into();
        entry.kind = kind;
        request.entries = vec![serde_json::json!(entry)];
        invalid.push(request);
    }
    for request in invalid {
        assert!(
            app.invoke(FileExplorer::CLOUD_IMPORT.with(request))
                .is_err()
        );
        assert_eq!(app.state().ops.entries().count(), 0);
    }
}

/// What the person did to Files reads as the app's sentences.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("draft.txt"), b"x").unwrap();
    let folder = root.path().display().to_string();
    let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
    let before = app.versions().len();
    let kind = <FileExplorer as ui::tilo_resource::Resource>::KIND;
    let agent = ui::tilo_actions::ActionInvoker::actor("agent:claude")
        .with_kind(ui::tilo_actions::ActorKind::Agent)
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
        // A test host names the app after the action type's stem
        // (`files`), not the app that hosts the record; read the entry
        // as the app's own host records it.
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
        ]
    );
}
