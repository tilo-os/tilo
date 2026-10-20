//! App-local observer tests; these do not claim native presentation coverage.
use super::*;

/// The editor launched on a fresh fixture file, as `code-editor FILE`.
fn loaded_fixture() -> (
    tempfile::TempDir,
    ui::framework::testing::TestApp<crate::CodeEditorShell>,
    crate::native_fixture::Manifest,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.rs");
    let manifest = crate::native_fixture::seed(&path).unwrap();
    let mut app = ui::framework::testing::TestApp::<crate::CodeEditorShell>::launched(&[path
        .to_str()
        .unwrap()])
    .unwrap();
    app.settle();
    (directory, app, manifest)
}

#[test]
fn dictation_ime_final_uses_native_code_actions_and_system_undo() {
    let (_directory, mut app, manifest) = loaded_fixture();
    app.harness()
        .perform_named("editor_view", &ui::a11y::Action::Focus)
        .unwrap();
    crate::test_support::editor(&mut app, |app| {
        app.editor.set_caret_for_test(0, 0);
    });
    assert_eq!(
        app.harness()
            .ime_window()
            .expect("focused native code field")
            .content
            .purpose,
        ui::ime::InputPurpose::Text
    );
    app.harness()
        .send(&WindowEvent::Ime(winit::event::Ime::Preedit(
            "temporary draft".into(),
            None,
        )));
    assert_eq!(
        cold_editor_snapshot(&mut app)["body_hash"],
        manifest.body_hash
    );
    app.harness()
        .send(&WindowEvent::Ime(winit::event::Ime::Commit(
            "日本語 café العربية".into(),
        )));
    let changed = cold_editor_snapshot(&mut app);
    assert_ne!(changed["body_hash"], manifest.body_hash);
    assert!(crate::test_support::editor(&mut app, |app| {
        app.editor
            .text_for_test()
            .starts_with("日本語 café العربية")
    }));
    assert!(app.undo(), "the final must be a system history action");
    assert_eq!(
        cold_editor_snapshot(&mut app)["body_hash"],
        manifest.body_hash
    );
    assert!(app.redo());
    assert_eq!(
        cold_editor_snapshot(&mut app)["body_hash"],
        changed["body_hash"]
    );
    crate::test_support::editor(&mut app, |app| app.editor.set_focus(false));
    app.harness()
        .send(&WindowEvent::Ime(winit::event::Ime::Commit(
            "must not reach the unfocused editor".into(),
        )));
    assert_eq!(
        cold_editor_snapshot(&mut app)["body_hash"],
        changed["body_hash"]
    );
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
    let (_directory, mut app, manifest) = loaded_fixture();
    let (before, unsent, changed) = crate::test_support::with_doc(&mut app, move |app, doc| {
        let before = app
            .native_input_snapshot(doc.doc(), &automerge::ROOT)
            .unwrap();
        app.editor.set_focus(true);
        app.editor.set_caret_for_test(0, 0);
        // Existing editor editing method emits the real action request; the
        // app's production dispatcher is what updates the durable text.
        app.editor.insert_for_test(doc, "x");
        let unsent = app
            .native_input_snapshot(doc.doc(), &automerge::ROOT)
            .unwrap();
        app.dispatch_editor_text_edits(doc);
        let changed = app
            .native_input_snapshot(doc.doc(), &automerge::ROOT)
            .unwrap();
        (before, unsent, changed)
    });
    assert_eq!(before["body_hash"], manifest.body_hash);
    assert_eq!(before["body_bytes"], manifest.body_bytes);
    assert_eq!(unsent["body_hash"], manifest.body_hash);
    assert_eq!(changed["body_hash"], manifest.prefixed_x_hash);
    assert_eq!(changed["body_bytes"], manifest.prefixed_x_bytes);
    assert_eq!(changed["editor_focused"], true);
    assert_eq!(changed["caret_line"], 0);
    assert_eq!(changed["caret_column"], 1);
    assert_eq!(changed["buffer_id"], before["buffer_id"]);
}

fn native_snapshot_observes_real_wheel_input_with_explicit_units() {
    let (_directory, mut app, manifest) = loaded_fixture();
    let snapshot = crate::test_support::with_doc(&mut app, |app, doc| {
        app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        app.editor
            .force_geometry(Rect::new(200.0, 40.0, 800.0, 560.0), 8.0);
        let mut send = |event| {
            app.editor.handle_event(
                &event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: doc.reborrow(),
                    path: &automerge::ROOT,
                    handled: &mut false,
                    focus_target: &mut None,
                    node_id: NodeId::new(0),
                    taffy: &TaffyTree::new(),
                },
            );
        };
        send(ui::synthetic_input::cursor_moved(300.0, 100.0));
        send(ui::synthetic_input::mouse_wheel(0.0, -3.0));
        app.native_input_snapshot(doc.doc(), &automerge::ROOT)
            .unwrap()
    });
    assert_eq!(snapshot["offset_y"], 9.0);
    assert_eq!(snapshot["offset_y_unit"], "lines");
    assert_eq!(snapshot["offset_x_unit"], "pixels");
    assert_eq!(snapshot["body_hash"], manifest.body_hash);
    assert_eq!(snapshot["ready"], true);
}

#[test]
fn bottom_panel_availability_tracks_its_real_host_mode() {
    let mut capture =
        ui::framework::testing::TestApp::<crate::CodeEditorShell>::capture(Some("selection"))
            .unwrap();
    crate::test_support::editor(&mut capture, |app| {
        assert!(!app.panel_toggle.is_enabled());
        assert!(
            ui::widget::controls::ActionControls::control_action_bindings(&app.panel_toggle)
                .is_empty(),
            "the captured toggle cannot advertise an effect without an executor"
        );
    });

    let (_directory, mut live, _manifest) = loaded_fixture();
    let before = crate::test_support::editor(&mut live, |app| {
        assert!(app.panel_toggle.is_enabled());
        let bindings =
            ui::widget::controls::ActionControls::control_action_bindings(&app.panel_toggle);
        assert!(bindings.iter().any(|binding| matches!(
            &binding.kind,
            ui::widget::ControlBindingKind::Action(address)
                if address.resource.as_str() == <crate::project::Project as ui::tilo_resource::Resource>::KIND
                    && address.action == "toggle-bottom-panel"
        )));
        app.panel_visible
    });
    live.harness().click_named(BottomPanelToggle::ID).unwrap();
    live.settle();
    assert_eq!(
        crate::test_support::editor(&mut live, |app| app.panel_visible),
        !before,
        "the live retained toggle invokes its real canonical effect"
    );
}

#[test]
fn native_text_continuation_restores_shared_resource_selection_and_view_after_offline_edits() {
    use ui::widget::Continuable;
    let (directory, mut source, _manifest) = loaded_fixture();
    let (metadata, before) = crate::test_support::with_doc(&mut source, |app, doc| {
        stage_continuation_view(app, doc);
        let metadata = app
            .continuation_metadata(doc.doc(), &automerge::ROOT)
            .unwrap();
        let snapshot = app
            .native_input_snapshot(doc.doc(), &automerge::ROOT)
            .unwrap();
        (metadata, snapshot)
    });
    assert_eq!(before["offset_y"], 9.75);
    let document = source.peer_document();
    let root_metadata =
        ui::widget::capability::capability::<dyn Continuable>(source.harness().root_widget())
            .unwrap()
            .continuation_metadata(&document, &automerge::ROOT)
            .unwrap();
    assert_eq!(root_metadata.schema_version, metadata.schema_version);
    assert_eq!(root_metadata.bytes, metadata.bytes);
    let target_path = directory.path().join("fixture.rs");
    let mut initial = source.peer_document();
    let mut target = ui::framework::testing::TestApp::<crate::CodeEditorShell>::launched_replica(
        &[target_path.to_str().unwrap()],
        &mut initial,
    )
    .unwrap();
    target
        .settings::<crate::settings::CodeSettings>()
        .set(&crate::settings::CodeSettings {
            language_servers: false,
            ..Default::default()
        })
        .unwrap();
    assert_foreign_view_refused(&mut target, &metadata);
    restore_editor_view(&mut target, metadata.clone());
    let restored = editor_snapshot(&mut target);
    assert_eq!(restored["buffer_id"], before["buffer_id"]);
    assert_eq!(restored["body_hash"], before["body_hash"]);
    assert_eq!(restored["caret_line"], before["caret_line"]);
    assert_eq!(restored["caret_column"], before["caret_column"]);
    assert_eq!(restored["selection_chars"], before["selection_chars"]);
    assert_eq!(restored["offset_y"], before["offset_y"]);
    assert_eq!(
        restored["editor_focused"], false,
        "view metadata grants no focus"
    );

    // Both actual app dispatchers edit their shared resource while disconnected.
    crate::test_support::with_doc(&mut source, |app, doc| {
        app.editor.set_caret_for_test(0, 0);
        app.editor.insert_for_test(doc, "prefix\n");
        app.dispatch_editor_text_edits(doc);
    });
    crate::test_support::with_doc(&mut target, |app, doc| {
        let text =
            EditorDoc::read_buffer_key(doc.doc(), &app.buffer_key(app.active_file_node().unwrap()))
                .unwrap();
        let line = text.matches('\n').count();
        let column = text.rsplit('\n').next().unwrap().chars().count();
        app.editor.set_caret_for_test(line, column);
        app.editor.insert_for_test(doc, "\nfrom peer");
        app.dispatch_editor_text_edits(doc);
    });
    let mut source_peer = source.peer_document();
    target.merge_peer(&mut source_peer).unwrap();
    let mut target_peer = target.peer_document();
    source.merge_peer(&mut target_peer).unwrap();
    restore_editor_view(&mut target, metadata);
    let after = editor_snapshot(&mut target);
    let source_after = editor_snapshot(&mut source);
    assert_eq!(
        after["body_hash"], source_after["body_hash"],
        "concurrent text converges through production record merges"
    );
    assert_eq!(after["buffer_id"], before["buffer_id"]);
    assert_eq!(
        after["caret_line"].as_u64(),
        before["caret_line"]
            .as_u64()
            .and_then(|line| line.checked_add(1))
    );
    assert_eq!(after["caret_column"], before["caret_column"]);
    assert_eq!(after["selection_chars"], before["selection_chars"]);
    assert_eq!(after["offset_y"], 10.75);
    assert_eq!(after["editor_focused"], false);
}

#[test]
fn live_editor_views_follow_repeated_positions_without_editing_or_focus() {
    use ui::widget::Continuable;
    let (directory, mut source, _) = loaded_fixture();
    let mut initial = source.peer_document();
    let path = directory.path().join("fixture.rs");
    let mut target = ui::framework::testing::TestApp::<crate::CodeEditorShell>::launched_replica(
        &[path.to_str().unwrap()],
        &mut initial,
    )
    .unwrap();
    target
        .settings::<crate::settings::CodeSettings>()
        .set(&crate::settings::CodeSettings {
            language_servers: false,
            ..Default::default()
        })
        .unwrap();
    target.settle();
    for (line, focused) in [(20, false), (37, true)] {
        let (metadata, expected) = crate::test_support::with_doc(&mut source, move |app, doc| {
            stage_continuation_view(app, doc);
            app.editor.set_selection_lc((line, 3), (line + 2, 5));
            (
                app.continuation_metadata(doc.doc(), &automerge::ROOT)
                    .unwrap(),
                app.native_input_snapshot(doc.doc(), &automerge::ROOT)
                    .unwrap(),
            )
        });
        crate::test_support::with_doc(&mut target, move |app, doc| {
            app.editor
                .force_geometry(Rect::new(200.0, 40.0, 800.0, 560.0), 16.0);
            app.editor.set_focus(focused);
            let before = app
                .native_input_snapshot(doc.doc(), &automerge::ROOT)
                .unwrap();
            let heads = doc.doc().clone().get_heads();
            assert!(
                app.restore_live_continuation_metadata(&metadata, doc.doc(), &automerge::ROOT,)
            );
            let after = app
                .native_input_snapshot(doc.doc(), &automerge::ROOT)
                .unwrap();
            assert_eq!(after["caret_line"], line + 2);
            assert_eq!(after["caret_column"], expected["caret_column"]);
            assert_eq!(after["selection_chars"], expected["selection_chars"]);
            assert_eq!(after["offset_y"], expected["offset_y"]);
            assert_eq!(after["editor_focused"], focused);
            assert_eq!(after["body_hash"], before["body_hash"]);
            assert_eq!(doc.doc().clone().get_heads(), heads);
            assert!(app.pending_open.is_none());
            assert!(app.pending_continuation.is_none());
            let mut position: ui::text_position::TextPosition =
                serde_json::from_slice(&metadata.bytes).unwrap();
            position.resource_id = "99999999-2222-4333-8444-555555555555".into();
            let foreign = ui::collaboration::ContinuationMetadata {
                schema_version: ui::collaboration::semantic_schema::TEXT_POSITION_V1,
                bytes: serde_json::to_vec(&position).unwrap(),
            };
            assert!(
                !app.restore_live_continuation_metadata(&foreign, doc.doc(), &automerge::ROOT,)
            );
            assert_eq!(
                app.native_input_snapshot(doc.doc(), &automerge::ROOT)
                    .unwrap(),
                after
            );
        });
    }
}

#[test]
fn cold_live_continuation_opens_existing_text_and_preserves_local_focus() {
    use tilo_actions::Action as _;
    use ui::widget::Continuable as _;
    let (_directory, mut source, _) = loaded_fixture();
    let metadata = crate::test_support::with_doc(&mut source, |app, doc| {
        stage_continuation_view(app, doc);
        app.continuation_metadata(doc.doc(), &automerge::ROOT)
            .unwrap()
    });
    let mut document = source.peer_document();
    let heads = document.get_heads();
    for focused in [false, true] {
        let mut target = CodeApp::new();
        target.editor.set_focus(focused);
        assert!(target.active_file_node().is_none());
        assert!(target.restore_live_continuation_metadata(&metadata, &document, &automerge::ROOT));
        assert!(target.active_file_node().is_some());
        let snapshot = target
            .native_input_snapshot(&document, &automerge::ROOT)
            .unwrap();
        assert_eq!(snapshot["editor_focused"], focused);
        assert!(snapshot["body_bytes"].as_u64().unwrap() > 0);
        assert_eq!(document.get_heads(), heads);
        assert!(target.pending_open.is_none());
        assert!(target.pending_continuation.is_none());
    }
    let mut position: ui::text_position::TextPosition =
        serde_json::from_slice(&metadata.bytes).unwrap();
    position.resource_id = "99999999-2222-4333-8444-555555555555".into();
    let foreign = ui::collaboration::ContinuationMetadata {
        schema_version: ui::collaboration::semantic_schema::TEXT_POSITION_V1,
        bytes: serde_json::to_vec(&position).unwrap(),
    };
    let mut target = CodeApp::new();
    assert!(!target.restore_live_continuation_metadata(&foreign, &document, &automerge::ROOT));
    assert!(target.active_file_node().is_none());
    assert_eq!(document.get_heads(), heads);
    let position: ui::text_position::TextPosition =
        serde_json::from_slice(&metadata.bytes).unwrap();
    assert!(EditorDoc::file(&document, &position.resource_id).is_some());
    crate::actions::delete_action(&position.resource_id)
        .apply(&mut document)
        .unwrap();
    let deleted_heads = document.get_heads();
    assert!(!target.restore_live_continuation_metadata(&metadata, &document, &automerge::ROOT));
    assert!(target.active_file_node().is_none());
    assert_eq!(
        document.get_heads(),
        deleted_heads,
        "deleted resources cannot be reopened by a view"
    );
}

#[test]
fn independent_cold_editors_keep_one_text_object_and_both_offline_edits() {
    let (directory, mut source, manifest) = loaded_fixture();
    let path = directory.path().join("fixture.rs");
    // No checked install or source snapshot: both actual apps create the same
    // file's record independently in their own fresh profile.
    let mut target = ui::framework::testing::TestApp::<crate::CodeEditorShell>::launched(&[path
        .to_str()
        .unwrap()])
    .unwrap();
    target.settle();
    let mut source_document = source.peer_document();
    let mut target_document = target.peer_document();
    assert_ne!(source_document.get_actor(), target_document.get_actor());
    let before = cold_editor_snapshot(&mut source);
    let other = cold_editor_snapshot(&mut target);
    assert_eq!(before["buffer_id"], other["buffer_id"]);
    let key = before["buffer_id"].as_str().unwrap();
    assert_eq!(
        EditorDoc::buffer_text_key(&source_document, key),
        EditorDoc::buffer_text_key(&target_document, key),
        "cold replicas must address the same CRDT Text before exchanging edits"
    );
    assert_eq!(other["body_hash"], manifest.body_hash);
    for (app, text) in [(&mut source, "from A\n"), (&mut target, "from B\n")] {
        crate::test_support::with_doc(app, move |app, doc| {
            app.editor.set_caret_for_test(0, 0);
            app.editor.insert_for_test(doc, text);
            app.dispatch_editor_text_edits(doc);
        });
    }
    // Local typing may focus the editor; explicitly leave it before the
    // network merge so the merge's lack of a focus grant is observable.
    crate::test_support::with_doc(&mut target, |app, _| app.editor.set_focus(false));
    assert_eq!(cold_editor_snapshot(&mut target)["editor_focused"], false);
    source_document = source.peer_document();
    target_document = target.peer_document();
    target.merge_peer(&mut source_document).unwrap();
    source.merge_peer(&mut target_document).unwrap();
    assert_eq!(
        cold_editor_snapshot(&mut source)["body_hash"],
        cold_editor_snapshot(&mut target)["body_hash"]
    );
    let merged = source.peer_document();
    let body = EditorDoc::read_buffer_key(&merged, key).unwrap();
    assert!(body.contains("from A\n"));
    assert!(body.contains("from B\n"));
    assert_eq!(body.matches("pub fn transform_0000").count(), 1);
    assert!(
        !cold_editor_snapshot(&mut target)["editor_focused"]
            .as_bool()
            .unwrap()
    );
}

fn cold_editor_snapshot(
    app: &mut ui::framework::testing::TestApp<crate::CodeEditorShell>,
) -> serde_json::Value {
    crate::test_support::with_doc(app, |app, doc| {
        app.native_input_snapshot(doc.doc(), &automerge::ROOT)
            .unwrap()
    })
}

fn assert_foreign_view_refused(
    app: &mut ui::framework::testing::TestApp<crate::CodeEditorShell>,
    metadata: &ui::collaboration::ContinuationMetadata,
) {
    use ui::widget::Continuable;
    let mut position: ui::text_position::TextPosition =
        serde_json::from_slice(&metadata.bytes).unwrap();
    position.resource_id = "99999999-2222-4333-8444-555555555555".into();
    let foreign = ui::collaboration::ContinuationMetadata {
        schema_version: ui::collaboration::semantic_schema::TEXT_POSITION_V1,
        bytes: serde_json::to_vec(&position).unwrap(),
    };
    crate::test_support::with_doc(app, move |app, doc| {
        let before = app
            .native_input_snapshot(doc.doc(), &automerge::ROOT)
            .unwrap();
        assert!(!app.restore_continuation_metadata(&foreign, doc, &automerge::ROOT));
        assert!(app.pending_continuation.is_none());
        assert_eq!(
            app.native_input_snapshot(doc.doc(), &automerge::ROOT)
                .unwrap(),
            before,
            "a foreign resource cannot change the editor view or focus"
        );
    });
}

fn stage_continuation_view(app: &mut CodeApp, doc: &mut ui::widget::Edits<'_>) {
    app.editor
        .force_geometry(Rect::new(200.0, 40.0, 800.0, 560.0), 8.0);
    app.editor.set_selection_lc((20, 3), (22, 5));
    for event in [
        ui::synthetic_input::cursor_moved(300.0, 100.0),
        ui::synthetic_input::mouse_wheel(0.0, -3.25),
    ] {
        app.editor.handle_event(
            &event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: doc.reborrow(),
                path: &automerge::ROOT,
                handled: &mut false,
                focus_target: &mut None,
                node_id: NodeId::new(0),
                taffy: &TaffyTree::new(),
            },
        );
    }
}

fn restore_editor_view(
    app: &mut ui::framework::testing::TestApp<crate::CodeEditorShell>,
    metadata: ui::collaboration::ContinuationMetadata,
) {
    use ui::widget::Continuable;
    crate::test_support::with_doc(app, move |app, doc| {
        app.editor
            .force_geometry(Rect::new(200.0, 40.0, 800.0, 560.0), 16.0);
        app.editor.set_focus(false);
        assert!(app.restore_continuation_metadata(&metadata, doc, &automerge::ROOT));
    });
    app.settle();
}

fn editor_snapshot(
    app: &mut ui::framework::testing::TestApp<crate::CodeEditorShell>,
) -> serde_json::Value {
    crate::test_support::with_doc(app, |app, doc| {
        app.native_input_snapshot(doc.doc(), &automerge::ROOT)
            .unwrap()
    })
}
