#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use automerge::{ROOT, ReadDoc};
use ui::headless::Driver;

fn note_text(doc: &automerge::AutoCommit) -> String {
    let (value, id) = doc.get(ROOT, "body").unwrap().unwrap();
    assert_eq!(value, automerge::Value::Object(automerge::ObjType::Text));
    doc.text(id).unwrap()
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
        !directory.path().join("data/current").exists(),
        "a screenshot must not save an unsaved note"
    );
}

#[test]
fn large_text_theme_reflows_the_real_editor_layout() {
    let normal_dir = tempfile::tempdir().unwrap();
    let large_dir = tempfile::tempdir().unwrap();
    let (normal_state, normal_root) = scratchpad::open(normal_dir.path()).unwrap();
    let (large_state, large_root) = scratchpad::open(large_dir.path()).unwrap();
    let normal = Driver::new(normal_state, normal_root, (640, 480));
    let mut large = Driver::new(large_state, large_root, (640, 480));
    large.set_theme(ui::Theme::dark().with_text_scale(2.0));

    let normal_editor = normal.targets()["note-editor"];
    let large_editor = large.targets()["note-editor"];
    assert!(
        large_editor.y0 > normal_editor.y0,
        "large text must reflow retained content, not only change theme metadata: {normal_editor:?} -> {large_editor:?}"
    );
}

#[test]
fn editor_uses_the_available_width_and_keeps_save_visible() {
    for viewport in [(480, 600), (1000, 700)] {
        let directory = tempfile::tempdir().unwrap();
        let (state, root) = scratchpad::open(directory.path()).unwrap();
        let driver = Driver::new(state, root, viewport);
        let targets = driver.targets();
        let editor = targets.get("note-editor").unwrap();
        let save = targets.get("save-note").unwrap();
        assert!(editor.width() > f64::from(viewport.0) - 100.0, "{editor:?}");
        assert!(save.y1 < f64::from(viewport.1));
    }
}

#[test]
fn typed_note_survives_save_close_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    {
        let (state, root) = scratchpad::open(directory.path()).unwrap();
        let mut driver = Driver::new(state, root, (800, 600));
        scratchpad::actions::register(driver.action_registry_mut());
        driver.click_named("note-editor").unwrap();
        driver.type_text("Hello macOS — café 日本語");
        driver.click_named("save-note").unwrap();
        assert_eq!(
            note_text(&driver.state_mut().doc),
            "Hello macOS — café 日本語"
        );
    }
    let (state, _root) = scratchpad::open(directory.path()).unwrap();
    assert_eq!(note_text(&state.doc), "Hello macOS — café 日本語");
}

#[test]
fn edits_replay_to_identical_document_heads() {
    use tilo_actions::{ActionAddress, ActionRegistry, apply_local, replay};
    let directory = tempfile::tempdir().unwrap();
    let (mut state, root) = scratchpad::open(directory.path()).unwrap();
    let mut peer = state.doc.clone();
    let action = scratchpad::actions::EditNote {
        text: "Same note, on another replica.".into(),
    };
    let encoded = tilo_actions::door::encode(
        &root.resources(&state.doc),
        &ActionAddress::new("scratchpad", "edit"),
        Some(&action.text),
    )
    .unwrap();
    let envelope = apply_local(&mut state.doc, &action, encoded).unwrap();
    let mut registry = ActionRegistry::new();
    scratchpad::actions::register(&mut registry);
    replay(&mut peer, &envelope, &registry).unwrap();
    assert_eq!(state.doc.get_heads(), peer.get_heads());
    assert_eq!(note_text(&peer), "Same note, on another replica.");
}

#[test]
fn a_second_instance_cannot_overwrite_the_open_note() {
    let directory = tempfile::tempdir().unwrap();
    let first = scratchpad::open(directory.path()).unwrap();
    assert!(scratchpad::open(directory.path()).is_err());
    drop(first);
    assert!(scratchpad::open(directory.path()).is_ok());
}

#[test]
fn corrupt_or_missing_saved_content_is_not_replaced_with_an_empty_note() {
    let directory = tempfile::tempdir().unwrap();
    for reference in ["not-a-hash", &"0".repeat(64)] {
        std::fs::write(directory.path().join("current"), reference).unwrap();
        assert!(scratchpad::open(directory.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("current")).unwrap(),
            reference
        );
    }
}

#[test]
fn failed_save_reports_error_and_keeps_the_previous_reference() {
    let directory = tempfile::tempdir().unwrap();
    let (state, root) = scratchpad::open(directory.path()).unwrap();
    let mut driver = Driver::new(state, root, (800, 600));
    scratchpad::actions::register(driver.action_registry_mut());
    driver.click_named("note-editor").unwrap();
    driver.type_text("Keep this version");
    driver.click_named("save-note").unwrap();
    let previous = std::fs::read(directory.path().join("current")).unwrap();
    driver.click_named("note-editor").unwrap();
    driver.type_text(" and this unsaved edit");
    // Make the publication target a directory. The content blob may be
    // ingested, but atomic rename cannot replace a directory with a file.
    // Keep the previous reference aside and restore it after the assertion.
    let current = directory.path().join("current");
    let backup = directory.path().join("current.previous");
    std::fs::rename(&current, &backup).unwrap();
    std::fs::create_dir(&current).unwrap();
    let result = driver.click_named("save-note");
    result.unwrap();
    assert!(driver.inspection_json().unwrap().contains("Save failed:"));
    std::fs::remove_dir(&current).unwrap();
    std::fs::rename(&backup, &current).unwrap();
    assert_eq!(std::fs::read(&current).unwrap(), previous);
    drop(driver);
    let (state, _root) = scratchpad::open(directory.path()).unwrap();
    assert_eq!(note_text(&state.doc), "Keep this version");
}

#[test]
fn independent_edits_merge_without_replacing_the_text_object() {
    use tilo_actions::Action;
    let mut original = automerge::AutoCommit::new();
    scratchpad::actions::EditNote {
        text: "hello world".into(),
    }
    .apply(&mut original)
    .unwrap();
    let object = original.get(ROOT, "body").unwrap().unwrap().1;
    let mut left = original.fork();
    let mut right = original.fork();
    scratchpad::actions::EditNote {
        text: "hello brave world".into(),
    }
    .apply(&mut left)
    .unwrap();
    scratchpad::actions::EditNote {
        text: "hello world!".into(),
    }
    .apply(&mut right)
    .unwrap();
    left.merge(&mut right).unwrap();
    assert_eq!(left.get(ROOT, "body").unwrap().unwrap().1, object);
    assert_eq!(note_text(&left), "hello brave world!");
}

/// Two replicas of a freshly opened note each type first: the body must be
/// one shared `Text` from the start, not two lazily created ones of which a
/// merge shows only the winner.
#[test]
fn concurrent_first_edits_on_two_replicas_both_survive() {
    use tilo_actions::Action;
    for _ in 0..16 {
        let directory = tempfile::tempdir().unwrap();
        let (mut state, _root) = scratchpad::open(directory.path()).unwrap();
        let mut left = state.doc.fork();
        let mut right = state.doc.fork();
        scratchpad::actions::EditNote {
            text: "left".into(),
        }
        .apply(&mut left)
        .unwrap();
        scratchpad::actions::EditNote {
            text: "right".into(),
        }
        .apply(&mut right)
        .unwrap();
        left.merge(&mut right).unwrap();
        let text = note_text(&left);
        assert!(
            text.contains("left") && text.contains("right"),
            "both replicas' first edits survive; got {text:?}"
        );
    }
}
