#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

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
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_scratchpad"))
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
        let normal_dir = tempfile::tempdir().unwrap();
        let large_dir = tempfile::tempdir().unwrap();
        let mut normal = open(normal_dir.path());
        let mut large = open(large_dir.path());
        large
            .harness()
            .set_theme(ui::Theme::dark().with_text_scale(2.0));

        let normal_editor = normal.harness().targets()["note-editor"];
        let large_editor = large.harness().targets()["note-editor"];
        assert!(
            large_editor.y0 > normal_editor.y0,
            "large text must reflow retained content, not only change theme metadata: {normal_editor:?} -> {large_editor:?}"
        );
    }

    eprintln!("phase editor_uses_the_available_width_and_keeps_status_visible");
    {
        for viewport in [(480, 600), (1000, 700)] {
            let directory = tempfile::tempdir().unwrap();
            let mut app = open(directory.path());
            app.harness().resize(viewport).unwrap();
            let targets = app.harness().targets();
            let editor = targets.get("note-editor").unwrap();
            let status = targets.get("save-status").unwrap();
            assert!(editor.width() > f64::from(viewport.0) - 100.0, "{editor:?}");
            assert!(status.y1 < f64::from(viewport.1));
        }
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

    eprintln!("phase a_second_instance_cannot_overwrite_the_open_note");
    {
        let directory = tempfile::tempdir().unwrap();
        let first = open(directory.path());
        assert!(HeadlessApp::<Scratchpad>::open_data_dir(directory.path()).is_err());
        drop(first);
        assert!(HeadlessApp::<Scratchpad>::open_data_dir(directory.path()).is_ok());
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
