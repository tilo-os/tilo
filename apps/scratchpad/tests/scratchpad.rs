#![allow(clippy::unwrap_used, reason = "test assertions")]

use scratchpad::{Note, Scratchpad};
use ui::prelude::*;

/// Replace the note's text, as the editor's field edit does.
fn write(text: impl Into<String>) -> Invoke<Note> {
    Invoke::edit(&Note::paths().body.replace(text)).unwrap()
}

fn open(directory: &std::path::Path) -> HeadlessApp<Scratchpad> {
    HeadlessApp::open_data_dir(directory).unwrap()
}

#[test]
fn cli_screenshot_accepts_shared_viewport_and_text_scale() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("note.png");
    let result = std::process::Command::new(tilo_test_env::package_binary!("scratchpad"))
        .arg("--data-dir")
        .arg(directory.path().join("data"))
        .arg("--shots")
        .arg(&output)
        .args(["--viewport", "640x480", "--text-scale", "2"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let png = std::fs::read(output).unwrap();
    assert_eq!(
        png.get(16..24),
        Some([0, 0, 2, 128, 0, 0, 1, 224].as_slice())
    );
    assert!(
        !directory.path().join("data").exists(),
        "a screenshot renders a private capture profile, never the user's note"
    );
}

#[test]
fn editor_layout_uses_available_width_and_reflows_for_large_text() {
    eprintln!("phase large_text_theme_reflows_the_real_editor_layout");
    {
        // The same words, set at twice the size, take more lines: the note
        // is laid out again rather than only styled again.
        let note = "Pick up the printed tickets before noon. Ask Maria whether the studio is \
                    free on the 14th, and send Tomas the revised floor plan. "
            .repeat(6);
        let normal_dir = tempfile::tempdir().unwrap();
        let large_dir = tempfile::tempdir().unwrap();
        let mut normal = open(normal_dir.path());
        let mut large = open(large_dir.path());
        normal.invoke(write(note.clone())).unwrap();
        large.invoke(write(note)).unwrap();
        large
            .harness()
            .set_theme(ui::Theme::dark().with_text_scale(2.0));

        let normal_editor = normal.harness().targets()["note-editor"];
        let large_editor = large.harness().targets()["note-editor"];
        assert!(
            large_editor.height() > normal_editor.height(),
            "large text must reflow retained content, not only change theme metadata: {normal_editor:?} -> {large_editor:?}"
        );
    }

    eprintln!("phase editor_keeps_margins_and_a_readable_measure_and_status_visible");
    {
        let readable = f64::from(ui::Theme::dark().tokens.metrics.readable_width);
        for viewport in [(480, 600), (1000, 700), (1708, 1400)] {
            let directory = tempfile::tempdir().unwrap();
            let mut app = open(directory.path());
            app.harness().resize(viewport).unwrap();
            let targets = app.harness().targets();
            let editor = targets.get("note-editor").unwrap();
            let status = targets.get("save-status").unwrap();
            let width = f64::from(viewport.0);
            assert!(editor.x0 > 0.0, "a margin before the text: {editor:?}");
            assert!(editor.x1 < width, "a margin after the text: {editor:?}");
            assert!(
                editor.width() > (width - 100.0).min(readable - 1.0),
                "a narrow window gives the note its width: {editor:?}"
            );
            assert!(
                editor.width() <= readable + 1.0,
                "a wide window keeps lines readable: {editor:?}"
            );
            assert!(
                (editor.center().x - width / 2.0).abs() < 2.0,
                "the column is centred: {editor:?}"
            );
            assert!(status.y1 < f64::from(viewport.1));
        }
    }

    eprintln!("phase the_note_is_set_in_the_ui_face_of_the_theme");
    {
        let theme = ui::Theme::dark();
        let note = theme.text(scratchpad::NOTE_ROLE).family;
        assert_eq!(
            note,
            theme.text(TextRole::Label).family,
            "the note shares the UI face the controls are set in"
        );
        assert_ne!(
            note,
            theme.text(TextRole::Reading).family,
            "never the book face, which not every visitor has loaded"
        );
    }
}

#[test]
fn saved_note_persists_and_is_protected_from_overwrite_and_corruption() {
    eprintln!("phase typed_note_survives_close_and_reopen_without_a_save_step");
    {
        let directory = tempfile::tempdir().unwrap();
        {
            let mut app = open(directory.path());
            app.harness().click_named("note-editor").unwrap();
            app.harness().type_text("Hello macOS — café 日本語");
            assert_eq!(app.state().body, "Hello macOS — café 日本語");
        }
        let app = open(directory.path());
        assert_eq!(app.state().body, "Hello macOS — café 日本語");
    }

    eprintln!("phase a_second_instance_shares_the_open_note_without_overwriting_it");
    {
        // The note is the workspace's one record: a second window opens the
        // same note as a writer of its own, so what each window keeps is
        // merged rather than one save replacing the other's.
        let directory = tempfile::tempdir().unwrap();
        let mut first = open(directory.path());
        first.invoke(write("first window")).unwrap();
        first.flush().unwrap();
        let mut second = open(directory.path());
        assert_eq!(second.record(), first.record());
        assert_eq!(second.state().body, "first window");
        second.invoke(write("second window")).unwrap();
        second.flush().unwrap();
        drop(second);
        drop(first);
        assert_eq!(open(directory.path()).state().body, "second window");
    }

    eprintln!("phase corrupt_saved_content_is_not_replaced_with_an_empty_note");
    {
        let directory = tempfile::tempdir().unwrap();
        {
            let mut app = open(directory.path());
            app.invoke(write("precious")).unwrap();
        }
        let documents = stored_documents(directory.path());
        assert_eq!(documents.len(), 1, "{documents:?}");
        let document = documents.first().unwrap();
        std::fs::write(document, b"not an automerge document").unwrap();
        assert!(HeadlessApp::<Scratchpad>::open_data_dir(directory.path()).is_err());
        assert_eq!(
            std::fs::read(document).unwrap(),
            b"not an automerge document"
        );
    }
}

#[test]
fn edits_are_undoable_permissioned_and_merge_to_identical_heads() {
    eprintln!("phase typing_is_undoable_and_recorded_in_history");
    {
        let directory = tempfile::tempdir().unwrap();
        let mut app = open(directory.path());
        app.invoke(write("first thought")).unwrap();
        app.invoke(write("second thought")).unwrap();
        let last = app.versions().pop().unwrap();
        let entry = last.context.expect("an entry");
        assert!(
            matches!(&entry.what, Some(ui::tilo_actions::Did::Edit(edit)) if edit.ty == "note" && edit.field == "body"),
            "{entry:?}"
        );
        assert!(app.undo());
        assert_eq!(app.state().body, "first thought");
        assert!(app.redo());
        assert_eq!(app.state().body, "second thought");
    }

    eprintln!("phase typed_words_undo_one_at_a_time_not_the_whole_note");
    {
        let directory = tempfile::tempdir().unwrap();
        let mut app = open(directory.path());
        app.harness().click_named("note-editor").unwrap();
        app.harness().type_text("pick up tickets");
        assert_eq!(app.state().body, "pick up tickets");
        assert!(app.undo());
        assert_eq!(
            app.state().body,
            "pick up ",
            "one undo takes back the last word, not the whole note"
        );
        assert!(app.undo());
        assert_eq!(app.state().body, "pick ");
        assert!(app.redo());
        assert_eq!(app.state().body, "pick up ");
    }

    eprintln!("phase a_viewer_cannot_edit_the_note");
    {
        let mut app = HeadlessApp::<Scratchpad>::with_role(ui::Role::Viewer).unwrap();
        assert!(app.invoke(write("not mine")).is_err());
        assert_eq!(app.state().body, "");
    }

    eprintln!("phase edits_replay_to_identical_document_heads");
    {
        let mut origin = Replica::<Note>::new();
        let mut peer = origin.fork();
        origin
            .apply(write("Same note, on another replica."))
            .unwrap();
        for envelope in origin.log().to_vec() {
            peer.replay(&envelope).unwrap();
        }
        assert_eq!(origin.heads(), peer.heads());
        assert_eq!(peer.state().unwrap().body, "Same note, on another replica.");
    }

    eprintln!("phase independent_edits_merge_without_replacing_the_text_object");
    {
        use ui::automerge::{ROOT, ReadDoc};
        let body = |replica: &Replica<Note>| replica.doc().get(ROOT, "body").unwrap().unwrap().1;
        let mut original = Replica::<Note>::new();
        original.apply(write("hello world")).unwrap();
        let object = body(&original);
        let mut left = original.fork();
        let mut right = original.fork();
        left.apply(write("hello brave world")).unwrap();
        right.apply(write("hello world!")).unwrap();
        left.sync_with(&mut right).unwrap();
        assert_eq!(body(&left), object);
        assert_eq!(left.state().unwrap().body, "hello brave world!");
        assert_eq!(right.state().unwrap().body, "hello brave world!");
    }
}

fn stored_documents(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap().path();
            if entry.is_dir() {
                pending.push(entry);
            } else if entry
                .file_name()
                .is_some_and(|name| name == "state.automerge")
            {
                found.push(entry);
            }
        }
    }
    found
}

/// The window is where the note is: the bar's location is the note's opening
/// line, not the app's name twice.
#[test]
fn the_window_bar_names_the_note_it_shows() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = open(directory.path());
    app.invoke(write("Notes for Thursday\n\nPick up the printed tickets."))
        .unwrap();
    app.harness().present_in_window_bar();
    let _ = app.harness().poll_frame();
    let bar = ui::window_toolbar::declared().expect("the note declares its slots");
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some("Notes for Thursday"),
        "the opening line is where the window is"
    );
    // Declaring the bar's slots is what takes the History control out of a
    // band above the app: the note now starts at its page's own inset, the
    // same one from the top as from the left.
    let editor = app.harness().targets()["note-editor"];
    assert!(
        (editor.y0 - editor.x0).abs() < 0.5,
        "nothing of the framework's stands above the note: {editor:?}"
    );

    // A note nobody has written yet is still a note.
    app.invoke(write("")).unwrap();
    let _ = app.harness().poll_frame();
    let bar = ui::window_toolbar::declared().expect("the note declares its slots");
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some("Note")
    );
}
