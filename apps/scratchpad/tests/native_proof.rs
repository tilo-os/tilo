#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

#[path = "support/native_fixture.rs"]
mod native_fixture;

use native_fixture::seed_native_fixture;
use scratchpad::Scratchpad;
use ui::prelude::*;

fn snapshot(app: &mut HeadlessApp<Scratchpad>) -> serde_json::Value {
    app.harness().native_input_snapshot().unwrap()
}

#[test]
fn native_fixture_reloads_production_storage_and_refuses_overwrite() {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("note");
    let manifest = seed_native_fixture(&path).unwrap();
    {
        let mut app = HeadlessApp::<Scratchpad>::open_data_dir(&path).unwrap();
        let body = app.state().body;
        assert_eq!(body.lines().count(), 1000);
        assert_eq!(manifest.body_bytes, body.len());
        assert_eq!(
            manifest.body_hash,
            blake3::hash(body.as_bytes()).to_hex().to_string()
        );
        let observed = snapshot(&mut app);
        assert_eq!(observed["body_hash"], manifest.body_hash);
        assert_eq!(observed["body_bytes"], manifest.body_bytes);
        assert_eq!(observed["editor_id"], "note-editor");
        assert_eq!(observed["ready"], true, "a laid-out root reports ready");
        assert_eq!(observed["offset_x"], 0.0);
        assert_eq!(observed["offset_y"], 0.0);
        assert!(observed.get("body").is_none());
        assert_eq!(snapshot(&mut app), observed);
    }
    assert!(seed_native_fixture(&path).is_err());
    let app = HeadlessApp::<Scratchpad>::open_data_dir(&path).unwrap();
    assert_eq!(app.state().body.lines().count(), 1000);
}

#[test]
fn native_editor_click_home_matches_fixture_hash_then_backspace_restores_it() {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("note");
    let manifest = seed_native_fixture(&path).unwrap();
    {
        let mut app = HeadlessApp::<Scratchpad>::open_data_dir(&path).unwrap();
        let editor = *app.harness().targets().get("note-editor").unwrap();
        app.harness().click_at(editor.x0 + 8.0, editor.y0 + 8.0);
        app.harness().key("Home");
        app.harness().type_text("x");
        let body = app.state().body;
        assert_eq!(body.len(), manifest.prefixed_x_bytes);
        assert_eq!(
            blake3::hash(body.as_bytes()).to_hex().to_string(),
            manifest.prefixed_x_hash
        );
        app.harness().key("Backspace");
    }
    let mut app = HeadlessApp::<Scratchpad>::open_data_dir(&path).unwrap();
    let observed = snapshot(&mut app);
    assert_eq!(observed["body_hash"], manifest.body_hash);
    assert_eq!(observed["body_bytes"], manifest.body_bytes);
}

#[test]
fn native_snapshot_observes_real_scroll_and_focused_edit_without_body_text() {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("note");
    let manifest = seed_native_fixture(&path).unwrap();
    let mut app = HeadlessApp::<Scratchpad>::open_data_dir(&path).unwrap();
    app.harness().profile_frame();
    let rect = snapshot(&mut app)["scroll_rect"]
        .as_array()
        .unwrap()
        .clone();
    let coord = |index: usize| rect.get(index).unwrap().as_f64().unwrap();
    app.harness()
        .send(&ui::winit::event::WindowEvent::CursorMoved {
            device_id: ui::winit::event::DeviceId::dummy(),
            position: ui::winit::dpi::PhysicalPosition::new(
                f64::midpoint(coord(0), coord(2)),
                f64::midpoint(coord(1), coord(3)),
            ),
        });
    let before = snapshot(&mut app)["offset_y"].as_f64().unwrap();
    app.harness().scroll(0.0, -3.0);
    let after = snapshot(&mut app)["offset_y"].as_f64().unwrap();
    assert!(
        after > before,
        "real content must scroll: {before} -> {after}"
    );
    assert_eq!(snapshot(&mut app)["body_hash"], manifest.body_hash);
    app.harness().scroll(0.0, 3.0);
    assert!(snapshot(&mut app)["offset_y"].as_f64().unwrap() < after);
    let editor = *app.harness().targets().get("note-editor").unwrap();
    app.harness().click_at(editor.x0 + 8.0, editor.y0 + 8.0);
    app.harness().key("Home");
    app.harness().type_text("x");
    {
        let observed = snapshot(&mut app);
        assert_eq!(observed["body_hash"], manifest.prefixed_x_hash);
        assert_eq!(observed["editor_focused"], true);
        assert_eq!(observed["ready"], true);
        assert!(observed.get("body").is_none());
    }
    app.harness().key("Backspace");
    assert_eq!(snapshot(&mut app)["body_hash"], manifest.body_hash);
}

#[test]
fn native_focus_setup_reports_initial_and_tab_focus() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = HeadlessApp::<Scratchpad>::open_data_dir(directory.path()).unwrap();
    app.harness().profile_frame();
    app.harness().key("Tab");
    app.harness().profile_frame();
    let editor = *app.harness().targets().get("note-editor").unwrap();
    app.harness().click_at(editor.x0 + 8.0, editor.y0 + 8.0);
    app.harness().profile_frame();
    assert_eq!(snapshot(&mut app)["editor_focused"], true);
}
