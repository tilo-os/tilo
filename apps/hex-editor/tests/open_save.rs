//! The file lifecycle through the production paths: a file named on the
//! command line opens as its own buffer, keystrokes edit it through the
//! framework's door (undone per run of typing), the palette and agents edit
//! it through the same actions, and Ctrl+S writes it back to disk.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use hex_editor::HexEditor;
use hex_editor::HexEditorShell;
use hex_editor::actions::HexEffect;
use hex_editor::resource::hex_editor::DeleteBytes;
use ui::a11y::Action as A11yAction;
use ui::framework::testing::TestApp;
use ui::portal::PortalOutcome;
use ui::record::RecordId;

fn file(name: &str, bytes: &[u8]) -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    (directory, path.to_str().unwrap().to_owned())
}

fn launched(path: &str) -> TestApp<HexEditorShell> {
    let mut app = TestApp::<HexEditorShell>::launched(&[path]).unwrap();
    app.harness().relayout();
    app
}

fn ctrl(app: &mut TestApp<HexEditorShell>, key: &str) {
    app.harness().set_modifiers(&["Ctrl".to_owned()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
    app.settle();
}

#[test]
fn a_launched_file_is_its_own_buffer_typed_into_undone_and_saved_back() {
    let (_directory, path) = file("Bytes Grün with spaces.bin", &[0x00, 0x11, 0x22, 0x33]);
    let mut app = launched(&path);
    assert_eq!(
        app.record(),
        &RecordId::derived("hex", &RecordId::identify_file(&path))
    );
    let state = app.state();
    assert_eq!(state.name(), Some("Bytes Grün with spaces.bin"));
    assert_eq!(state.bytes, [0x00, 0x11, 0x22, 0x33]);

    // Two nibbles over the first byte, then one more byte: one run of
    // typing, one undo step.
    app.harness()
        .perform_named("hex_view", &A11yAction::Focus)
        .unwrap();
    app.harness().type_text("ab1");
    app.settle();
    assert_eq!(app.state().bytes, [0xAB, 0x11, 0x22, 0x33]);
    assert!(app.undo());
    assert_eq!(app.state().bytes, [0x00, 0x11, 0x22, 0x33]);
    assert!(app.redo());
    assert_eq!(app.state().bytes, [0xAB, 0x11, 0x22, 0x33]);

    // Moving the cursor ends the run: the next typing is a step of its own.
    app.harness().key("ArrowRight");
    app.harness().type_text("ff");
    app.settle();
    assert_eq!(app.state().bytes, [0xAB, 0x11, 0xFF, 0x33]);
    assert!(app.undo());
    assert_eq!(app.state().bytes, [0xAB, 0x11, 0x22, 0x33]);

    // Ctrl+S writes the buffer back to the file it was launched on.
    ctrl(&mut app, "s");
    assert_eq!(std::fs::read(&path).unwrap(), [0xAB, 0x11, 0x22, 0x33]);

    // Launching on the file again reads it afresh into the same buffer.
    std::fs::write(&path, [9, 8, 7]).unwrap();
    let again = app.relaunch(&[path.as_str()]).unwrap();
    assert_eq!(again.state().bytes, [9, 8, 7]);
}

#[test]
fn agents_edit_the_bytes_through_the_published_actions() {
    let (_directory, path) = file("agent.bin", &[0x00, 0x11, 0x22, 0x33]);
    let mut app = launched(&path);

    let surface = app.agent_surface();
    let published: Vec<(String, String)> = surface
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["action"].as_str().unwrap().to_owned(),
                row["operation"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    for (id, operation) in [
        ("insert-bytes", "Document"),
        ("overwrite-bytes", "Document"),
        ("delete-bytes", "Document"),
        ("insert-byte", "Document"),
        ("overwrite-byte", "Document"),
        ("delete-byte", "Document"),
        ("open-file", "Effect"),
        ("save", "Effect"),
        ("save-as", "Effect"),
        ("search", "View"),
        ("goto-offset", "View"),
    ] {
        assert!(
            published
                .iter()
                .any(|(action, kind)| action == id && kind == operation),
            "{id} ({operation}) in {published:?}"
        );
    }

    app.call(
        "overwrite-bytes",
        serde_json::json!({"offset": 1, "bytes": "ff ee"}),
    )
    .unwrap();
    assert_eq!(app.state().bytes, [0x00, 0xFF, 0xEE, 0x33]);
    // Bound to the cursor, which starts on the first byte.
    app.call("insert-byte", serde_json::json!({"bytes": "7f"}))
        .unwrap();
    assert_eq!(app.state().bytes, [0x7F, 0x00, 0xFF, 0xEE, 0x33]);
    assert!(
        app.call(
            "overwrite-bytes",
            serde_json::json!({"offset": 9, "bytes": "01"})
        )
        .is_err(),
        "an offset past the end is refused"
    );
    // An undoable delete runs at once and offers Undo.
    app.call("delete-byte", serde_json::json!({})).unwrap();
    assert_eq!(app.state().bytes.len(), 4, "one byte deleted at once");
    assert!(app.undo());
    assert_eq!(app.state().bytes, [0x7F, 0x00, 0xFF, 0xEE, 0x33]);
    // An agent deletes in one call, confirmed.
    app.invoke_confirmed(HexEditor::DELETE_BYTES.with(DeleteBytes { offset: 0, len: 2 }))
        .unwrap();
    assert_eq!(app.state().bytes, [0xFF, 0xEE, 0x33]);
    assert!(app.undo());
    assert_eq!(app.state().bytes, [0x7F, 0x00, 0xFF, 0xEE, 0x33]);

    // The views open on the editor without touching the bytes.
    app.call("search", serde_json::json!({})).unwrap();
    app.call("goto-offset", serde_json::json!({})).unwrap();
    assert_eq!(app.state().bytes, [0x7F, 0x00, 0xFF, 0xEE, 0x33]);

    // Saving to a path an agent names writes there.
    let (_out, target) = file("copy.bin", &[]);
    app.effect_confirmed(&HexEffect::SaveAs {
        path: target.clone(),
    })
    .unwrap();
    assert_eq!(
        std::fs::read(&target).unwrap(),
        [0x7F, 0x00, 0xFF, 0xEE, 0x33]
    );
}

#[test]
fn opening_a_file_replaces_the_buffer_and_the_chooser_answers_the_effect() {
    let mut app = TestApp::<HexEditorShell>::new().unwrap();
    app.harness().relayout();
    assert!(!app.state().has_file(), "starts in the honest empty state");
    assert!(app.effect(&HexEffect::Save).is_err(), "nothing to save yet");

    let (_directory, path) = file("named.bin", b"HELLO-HEX");
    app.effect(&HexEffect::OpenFile { path: Some(path) })
        .unwrap();
    app.settle();
    assert_eq!(app.state().name(), Some("named.bin"));
    assert_eq!(app.state().bytes, b"HELLO-HEX");
    assert!(
        !app.undo(),
        "a file's bytes come from outside: no undo takes them back"
    );

    // Ctrl+O opens the host's file chooser; its pick becomes the buffer.
    let (_chosen_directory, chosen) = file("chosen.bin", &[1, 2, 3]);
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick(PortalOutcome::Opened(chosen.clone().into()));
    ui::host::scoped(&services, || {
        app.harness()
            .perform_named("hex_view", &A11yAction::Focus)
            .unwrap();
        ctrl(&mut app, "o");
        app.settle();
    });
    assert_eq!(recorder.pick_requests().len(), 1);
    assert_eq!(app.state().name(), Some("chosen.bin"));
    assert_eq!(app.state().bytes, [1, 2, 3]);

    // The chosen file is where a save writes back to.
    app.harness().type_text("ff");
    ctrl(&mut app, "s");
    assert_eq!(std::fs::read(&chosen).unwrap(), [0xFF, 2, 3]);
}

/// A file larger than a host may hand the app is refused before it is read
/// whole, through the same door a person's chooser and an agent's path use,
/// and the open buffer stays as it was.
#[test]
fn a_file_too_large_to_hold_is_refused_and_the_buffer_kept() {
    let mut app = TestApp::<HexEditorShell>::new().unwrap();
    app.harness().relayout();
    let (_directory, small) = file("small.bin", b"KEEP");
    app.effect(&HexEffect::OpenFile { path: Some(small) })
        .unwrap();
    app.settle();

    // Sparse: its length is past the limit, its blocks are never written.
    let huge_directory = tempfile::tempdir().unwrap();
    let huge = huge_directory.path().join("disk.img");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(u64::try_from(ui::application::MAX_OPEN_FILE_BYTES).unwrap() + 1)
        .unwrap();
    let refused = app
        .effect(&HexEffect::OpenFile {
            path: Some(huge.to_str().unwrap().to_owned()),
        })
        .expect_err("a file past the limit does not open");
    assert!(
        format!("{refused:?}").contains("larger than"),
        "the refusal says why: {refused:?}"
    );
    app.settle();
    assert_eq!(app.state().name(), Some("small.bin"));
    assert_eq!(app.state().bytes, b"KEEP");
}

#[test]
fn captures_stage_the_sample_through_the_resource() {
    let mut sample = TestApp::<HexEditorShell>::capture(None).unwrap();
    let targets = sample.harness().targets();
    assert_eq!(
        targets["hex_view"], targets["BoundComponent[0]/ResourceApp[0]"],
        "the editor fills the app's area"
    );
    let state = sample.state();
    assert_eq!(state.name(), Some("sample.png"));
    assert_eq!(&state.bytes.to_vec()[..4], &[0x89, 0x50, 0x4E, 0x47]);

    for scenario in [
        "find",
        "goto",
        "edited",
        "error",
        "narrow",
        "narrow-inspector",
    ] {
        let app = TestApp::<HexEditorShell>::capture(Some(scenario)).unwrap();
        assert!(app.state().has_file(), "{scenario}");
        assert_eq!(
            app.state().bytes != state.bytes,
            scenario == "edited",
            "only the edited capture changes bytes, as the resource's edits: {scenario}"
        );
    }
    let empty = TestApp::<HexEditorShell>::capture(Some("empty")).unwrap();
    assert!(!empty.state().has_file());
}

#[test]
fn the_executable_opens_a_path_edits_and_saves_it() {
    let (directory, path) = file("Bytes Grün with spaces.bin", &[0x00, 0x11, 0x22, 0x33]);
    let spec = serde_json::json!({
        "actions": [
            {"perform": {"target": "hex_view", "action": "focus"}},
            {"type": "ab"},
            {"modifiers": ["Ctrl"]},
            {"key": "s"},
            {"modifiers": []}
        ],
    });
    let mut command = std::process::Command::new(tilo_test_env::package_binary!("hex-editor"));
    command
        .args(["--headless-drive", &spec.to_string()])
        .arg(&path)
        .env("XDG_STATE_HOME", directory.path().join("state"))
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .env("XDG_DATA_HOME", directory.path().join("data"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&path).unwrap(), [0xAB, 0x11, 0x22, 0x33]);
}

/// What an agent does to the bytes is history the person reads in their
/// language: the app's sentence for each edit and verb.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use hex_editor::model::BufferEdit;
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let (_directory, path) = file("history.bin", &[0x00, 0x11, 0x22, 0x33]);
    let mut app = launched(&path);
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <HexEditor as ui::tilo_resource::Resource>::KIND;
    let bytes = HexEditor::paths().bytes;
    for edit in [
        BufferEdit::Overwrite {
            offset: 1,
            bytes: hex_editor::buffer::Hex(vec![0xff]),
        },
        BufferEdit::Insert {
            offset: 0,
            bytes: hex_editor::buffer::Hex(vec![1, 2, 3]),
        },
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": bytes.perform(&edit).unwrap() }),
        )
        .unwrap();
    }
    app.call_by(
        agent.clone(),
        kind,
        "overwrite-bytes",
        serde_json::json!({"offset": 2, "bytes": "ab"}),
    )
    .unwrap();
    app.call_by(
        agent,
        kind,
        "load",
        serde_json::json!({"name": "other.bin", "bytes": "00"}),
    )
    .unwrap();

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
    assert_eq!(
        sentences,
        [
            "Overwrote one byte at offset 1",
            "Inserted 3 bytes at offset 0",
            "Overwrote bytes at offset 2",
            "Opened other.bin",
        ]
    );
}

/// Two replicas of one buffer converge after each edits a different page.
#[test]
fn two_replicas_converge() {
    let page = 4096;
    let (_directory, path) = file("shared.bin", &vec![0; 2 * page]);
    let mut app = launched(&path);
    let mut theirs = ui::tilo_resource::Replica::<HexEditor>::from_doc(app.peer_document());
    theirs
        .apply(
            HexEditor::OVERWRITE_BYTES.with(hex_editor::resource::hex_editor::OverwriteBytes {
                offset: page + 7,
                bytes: hex_editor::buffer::Hex(vec![7]),
            }),
        )
        .unwrap();
    app.call(
        "overwrite-bytes",
        serde_json::json!({"offset": 0, "bytes": "01"}),
    )
    .unwrap();
    let mut peer = theirs.doc().clone();
    app.merge_peer(&mut peer).unwrap();
    let mut expected = vec![0; 2 * page];
    expected[0] = 1;
    expected[page + 7] = 7;
    assert_eq!(app.state().bytes, expected);
}

/// With no file to open (a page whose chooser cannot answer, or bytes to
/// write from scratch), "New empty file" and Ctrl+N start an empty buffer
/// that typing fills.
#[test]
fn a_new_empty_file_starts_a_buffer_typing_fills() {
    let mut app = TestApp::<HexEditorShell>::new().unwrap();
    app.harness().relayout();
    assert!(!app.state().has_file());
    app.harness()
        .perform_named("hex.new", &A11yAction::Activate)
        .unwrap();
    app.settle();
    let state = app.state();
    assert_eq!(state.name(), Some("Untitled.bin"));
    assert!(state.bytes.is_empty());
    app.harness()
        .perform_named("hex_view", &A11yAction::Focus)
        .unwrap();
    app.harness().type_text("de");
    app.settle();
    assert_eq!(app.state().bytes, [0xDE]);

    let mut fresh = TestApp::<HexEditorShell>::new().unwrap();
    fresh.harness().relayout();
    fresh
        .harness()
        .perform_named("hex_view", &A11yAction::Focus)
        .unwrap();
    ctrl(&mut fresh, "n");
    assert_eq!(fresh.state().name(), Some("Untitled.bin"));
}

/// A guest's own "Open file" goes straight to their chooser: picking a file
/// on their own device is their decision, so the workspace asks no consent
/// first, and the pick opens.
#[test]
fn a_guests_open_file_asks_no_workspace_consent() {
    let mut app = TestApp::<HexEditorShell>::new().unwrap();
    app.harness().relayout();
    app.set_role(ui::permissions::Role::Editor);
    let (_chosen_directory, chosen) = file("guest.bin", &[7, 8]);
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick(PortalOutcome::Opened(chosen.into()));
    ui::host::scoped(&services, || {
        app.harness()
            .perform_named("hex.open", &A11yAction::Activate)
            .unwrap();
        app.settle();
    });
    assert!(
        app.asking().is_none(),
        "the chooser is the consent: {:?}",
        app.harness().asking_why()
    );
    assert_eq!(recorder.pick_requests().len(), 1);
    assert_eq!(app.state().name(), Some("guest.bin"));
}
