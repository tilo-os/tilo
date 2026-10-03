//! App-local observer tests; these do not claim native presentation coverage.
use super::*;

fn loaded_fixture() -> (
    tempfile::TempDir,
    CodeApp,
    AutoCommit,
    crate::native_fixture::Manifest,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.rs");
    let manifest = crate::native_fixture::seed(&path).unwrap();
    let mut app = CodeApp::open_path(&path);
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);
    app.editor.sync(&mut doc, &automerge::ROOT);
    (directory, app, doc, manifest)
}

#[test]
fn native_snapshot_reports_readiness_durable_edits_and_wheel_input() {
    // Each phase builds a fresh app/fixture: edits and scroll must not leak.
    native_snapshot_of_unopened_editor_is_not_ready();
    native_snapshot_tracks_durable_edits_and_caret_without_source_text();
    native_snapshot_observes_real_wheel_input_with_explicit_units();
}

fn native_snapshot_of_unopened_editor_is_not_ready() {
    let app = CodeApp::new();
    let doc = AutoCommit::new();
    let snapshot = app.native_input_snapshot(&doc, &automerge::ROOT).unwrap();
    assert_eq!(snapshot["ready"], false);
    assert!(snapshot["body_hash"].is_null());
    assert!(snapshot["body_bytes"].is_null());
    assert!(snapshot["buffer_id"].is_null());
}

fn native_snapshot_tracks_durable_edits_and_caret_without_source_text() {
    let (_directory, mut app, mut doc, manifest) = loaded_fixture();
    let before = app.native_input_snapshot(&doc, &automerge::ROOT).unwrap();
    assert_eq!(before["body_hash"], manifest.body_hash);
    assert_eq!(before["body_bytes"], manifest.body_bytes);
    assert_eq!(before["ready"], false, "unlaid-out app cannot be ready");
    app.editor.set_focus(true);
    app.editor.set_caret_for_test(0, 0);
    // Existing editor editing method emits the real action request; the app's
    // production dispatcher is what updates the durable Automerge text.
    app.editor.insert_for_test(&mut doc, "x");
    assert_eq!(
        app.native_input_snapshot(&doc, &automerge::ROOT).unwrap()["body_hash"],
        manifest.body_hash
    );
    app.dispatch_editor_text_edits(&mut doc);
    let changed = app.native_input_snapshot(&doc, &automerge::ROOT).unwrap();
    assert_eq!(changed["body_hash"], manifest.prefixed_x_hash);
    assert_eq!(changed["body_bytes"], manifest.prefixed_x_bytes);
    assert_eq!(changed["editor_focused"], true);
    assert_eq!(changed["caret_line"], 0);
    assert_eq!(changed["caret_column"], 1);
    assert_eq!(changed["buffer_id"], before["buffer_id"]);
    assert!(changed.get("body").is_none());
    app.editor.backspace_for_test(&mut doc);
    app.dispatch_editor_text_edits(&mut doc);
    assert_eq!(
        app.native_input_snapshot(&doc, &automerge::ROOT).unwrap()["body_hash"],
        manifest.body_hash
    );
}

fn native_snapshot_observes_real_wheel_input_with_explicit_units() {
    let (_directory, mut app, mut doc, manifest) = loaded_fixture();
    app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
    app.editor
        .force_geometry(Rect::new(200.0, 40.0, 800.0, 560.0), 8.0);
    let mut send = |event| {
        app.editor.handle_event(
            &event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut false,
                focus_target: &mut None,
                node_id: NodeId::new(0),
                taffy: &TaffyTree::new(),
            },
        );
    };
    send(WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(300.0, 100.0),
    });
    send(WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, -3.0),
        phase: winit::event::TouchPhase::Moved,
    });
    let snapshot = app.native_input_snapshot(&doc, &automerge::ROOT).unwrap();
    assert_eq!(snapshot["offset_y"], 9.0);
    assert_eq!(snapshot["offset_y_unit"], "lines");
    assert_eq!(snapshot["offset_x_unit"], "pixels");
    assert_eq!(snapshot["body_hash"], manifest.body_hash);
    assert_eq!(snapshot["ready"], true);
}
