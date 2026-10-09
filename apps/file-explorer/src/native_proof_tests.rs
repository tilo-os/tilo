/// The native input observer across fixture reload, wheel + selection,
/// logical-cursor tracking and the not-ready states — each phase on its own
/// temp directory and harness.
#[test]
fn native_observer_tracks_fixture_reload_wheel_selection_and_pointer() {
    native_fixture_reloads_real_files_and_refuses_existing_directory();
    native_observer_follows_actual_wheel_and_selection();
    native_pointer_observer_tracks_logical_cursor_and_survives_rebuild();
    native_observer_is_not_ready_before_layout_or_on_load_error();
}

fn native_fixture_reloads_real_files_and_refuses_existing_directory() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("fixture");
    let manifest = crate::native_fixture::seed(&directory).unwrap();
    // Independently derived from the documented name/kind byte encoding.
    assert_eq!(
        manifest["order_hash"],
        "b8b390ea86922b9297a211bf10ab47e3a9cff0c70b81a976448714323f35f863"
    );
    let mut h = H::new(Some(&directory), 1200.0, 720.0);
    let snapshot =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert_eq!(snapshot["entry_count"], 1200);
    assert_eq!(snapshot["order_hash"], manifest["order_hash"]);
    assert_eq!(snapshot["ready"], true);
    assert_eq!(snapshot["directory"], directory.to_string_lossy().as_ref());
    let png = std::fs::read(directory.join("entry-0000.png")).unwrap();
    assert_eq!(image::load_from_memory(&png).unwrap().width(), 64);
    assert!(crate::native_fixture::seed(&directory).is_err());
    assert_eq!(
        std::fs::read(directory.join("entry-0000.png")).unwrap(),
        png
    );
    let other = temp.path().join("other");
    assert_eq!(crate::native_fixture::seed(&other).unwrap(), manifest);
    assert_eq!(std::fs::read(other.join("entry-0000.png")).unwrap(), png);
    assert_eq!(
        std::fs::read_dir(&other)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "png"))
            .count(),
        299
    );
    let existing_file = temp.path().join("existing-file");
    std::fs::write(&existing_file, b"preserve me").unwrap();
    assert!(crate::native_fixture::seed(&existing_file).is_err());
    assert_eq!(std::fs::read(&existing_file).unwrap(), b"preserve me");
    #[cfg(unix)]
    {
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        assert!(crate::native_fixture::seed(&alias).is_err());
    }
    std::fs::remove_file(directory.join("entry-0000.png")).unwrap();
    h.w.navigate(directory);
    let changed =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert_eq!(changed["entry_count"], 1199);
    assert_ne!(changed["order_hash"], snapshot["order_hash"]);
}

fn native_observer_follows_actual_wheel_and_selection() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..1200 {
        std::fs::write(temp.path().join(format!("file-{index:04}.txt")), b"note\n").unwrap();
    }
    let mut h = H::new(Some(temp.path()), 1200.0, 720.0);
    let initial =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .expect("production observer must expose loaded native state");
    assert_eq!(initial["ready"], true);
    assert_eq!(initial["offset_y"], 0.0);
    let vp = initial["viewport_rect"].as_array().unwrap();
    let x = vp[0].as_f64().unwrap() + 50.0;
    let y = vp[1].as_f64().unwrap() + 60.0;
    h.cursor(x, y);
    h.dispatch(&winit::event::WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, -3.0),
        phase: winit::event::TouchPhase::Moved,
    });
    h.w.apply_layout(&h.taffy, h.node, Point::ORIGIN);
    let moved =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert!(moved["offset_y"].as_f64().unwrap() > 0.0);
    assert_eq!(moved["order_hash"], initial["order_hash"]);
    assert_eq!(moved["viewport_rect"], initial["viewport_rect"]);
    assert_eq!(
        moved["content_rect"][1].as_f64().unwrap() + moved["offset_y"].as_f64().unwrap(),
        initial["content_rect"][1].as_f64().unwrap()
    );
    h.left_press();
    h.left_release();
    let selected =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert_eq!(selected["selected_indices"].as_array().unwrap().len(), 1);
    let index = selected["selected_indices"][0].as_u64().unwrap() as usize;
    assert_eq!(selected["selected_names"][0], h.w.entries()[index].name);
    assert_eq!(selected["order_hash"], initial["order_hash"]);
    assert_eq!(selected["offset_y"], moved["offset_y"]);
    h.dispatch(&winit::event::WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, 3.0),
        phase: winit::event::TouchPhase::Moved,
    });
    h.w.apply_layout(&h.taffy, h.node, Point::ORIGIN);
    let reversed =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert_eq!(reversed["offset_y"], 0.0);
    assert_eq!(reversed["content_rect"], initial["content_rect"]);
    assert_eq!(reversed["selected_indices"], selected["selected_indices"]);
    // The unscrolled first row lies below the 30px header at this fixed size.
    h.cursor(x, y);
    h.left_press();
    h.left_release();
    let first =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert_eq!(first["selected_indices"], serde_json::json!([0]));
    assert_eq!(
        first["selected_names"],
        serde_json::json!(["file-0000.txt"])
    );
}

fn native_pointer_observer_tracks_logical_cursor_and_survives_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..100 {
        std::fs::write(temp.path().join(format!("file-{index:04}.txt")), b"note\n").unwrap();
    }
    let mut h = H::new(Some(temp.path()), 1200.0, 720.0);
    let event = winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(1000.0, 400.0),
    };
    let mut handled = false;
    let mut focus = None;
    h.w.handle_event(
        &event,
        &mut ui::widget::EventCtx {
            scale_factor: 2.0,
            modifiers: &winit::event::Modifiers::default(),
            state: ui::widget::Edits::lend(&mut h.state.doc),
            path: &h.state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: h.node,
            taffy: &h.taffy,
        },
    );
    let snapshot =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert_eq!(snapshot["last_cursor"], serde_json::json!([500.0, 200.0]));
    // Force the production content composition to replace the Scrollable.
    h.w.set_view_mode(crate::model::ViewMode::Grid);
    h.lay_out(1200.0, 720.0);
    let wheel = winit::event::WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, -3.0),
        phase: winit::event::TouchPhase::Moved,
    };
    h.dispatch(&wheel);
    let moved =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert!(moved["offset_y"].as_f64().unwrap() > 0.0);
    assert_eq!(moved["last_cursor"], snapshot["last_cursor"]);
    h.cursor(10.0, 10.0);
    h.dispatch(&wheel);
    let outside =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    assert_eq!(outside["last_cursor"], serde_json::json!([10.0, 10.0]));
    assert_eq!(outside["offset_y"], moved["offset_y"]);
}

fn native_observer_is_not_ready_before_layout_or_on_load_error() {
    let mut widget = FileExplorerWidget::with_persistence(None, None);
    let doc = automerge::AutoCommit::new();
    assert_eq!(
        widget
            .native_input_snapshot(&doc, &automerge::ROOT)
            .unwrap()["ready"],
        false
    );
    let temp = tempfile::tempdir().unwrap();
    widget.navigate(temp.path().join("absent"));
    let snapshot = widget
        .native_input_snapshot(&doc, &automerge::ROOT)
        .unwrap();
    assert_eq!(snapshot["ready"], false);
    assert_eq!(snapshot["has_error"], true);
}

/// Each phase builds its own harness; `ACCESSIBILITY_BUILDS` is reset by the
/// phase that counts it.
#[test]
fn control_presence_matches_inventory_without_materializing_files() {
    control_presence_real_files_scroll_skips_file_accessibility_but_inventory_keeps_it();
    control_presence_files_collaboration_panel_matches_inventory();
}

fn control_presence_real_files_scroll_skips_file_accessibility_but_inventory_keeps_it() {
    use crate::views::file_view::ACCESSIBILITY_BUILDS;
    let temp = tempfile::tempdir().unwrap();
    for index in 0..1200 {
        std::fs::write(temp.path().join(format!("file-{index:04}.txt")), b"note").unwrap();
    }
    let mut h = H::new(Some(temp.path()), 1200.0, 720.0);
    let snapshot =
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap();
    let vp = snapshot["viewport_rect"].as_array().unwrap();
    h.cursor(
        vp[0].as_f64().unwrap() + 50.0,
        vp[1].as_f64().unwrap() + 60.0,
    );
    let expected = ui::widget::controls::control_action_bindings(&h.w)
        .iter()
        .any(|b| matches!(b.kind, ui::widget::ControlBindingKind::Action(_)));
    let body_expected = ui::widget::controls::control_action_bindings(&h.w.nav)
        .iter()
        .any(|b| matches!(b.kind, ui::widget::ControlBindingKind::Action(_)));
    ACCESSIBILITY_BUILDS.with(|count| count.set(0));
    for _ in 0..4 {
        assert_eq!(
            ui::widget::controls::has_action_control_bindings(&h.w),
            expected
        );
        assert_eq!(
            ui::widget::controls::has_action_control_bindings(&h.w.nav),
            body_expected,
            "also exercise the file body without a toolbar emitter short-circuit"
        );
        h.dispatch(&winit::event::WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, -3.0),
            phase: winit::event::TouchPhase::Moved,
        });
        h.w.apply_layout(&h.taffy, h.node, Point::ORIGIN);
        assert_eq!(
            ui::widget::controls::has_action_control_bindings(&h.w),
            expected
        );
        assert_eq!(
            ui::widget::controls::has_action_control_bindings(&h.w.nav),
            body_expected,
            "also exercise the file body without a toolbar emitter short-circuit"
        );
    }
    assert!(
        h.w.native_input_snapshot(&h.state.doc, &h.state.root)
            .unwrap()["offset_y"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert_eq!(
        ACCESSIBILITY_BUILDS.with(std::cell::Cell::get),
        0,
        "presence queries must not materialize all 1200 file nodes"
    );
    let inventory = ui::widget::controls::control_action_bindings(&h.w);
    assert_eq!(
        inventory
            .iter()
            .any(|b| matches!(b.kind, ui::widget::ControlBindingKind::Action(_))),
        expected
    );
    assert!(
        ACCESSIBILITY_BUILDS.with(std::cell::Cell::get) > 0,
        "explicit inventory must retain full semantic traversal"
    );
}

fn control_presence_files_collaboration_panel_matches_inventory() {
    let mut h = H::new(None, 1200.0, 720.0);
    for modal in [false, true, false] {
        h.w.collab_panel = modal.then(|| CollabPanel {
            title: "Review".into(),
            summary: "Summary".into(),
            lines: vec![],
            action: None,
            history: None,
        });
        let expected = ui::widget::controls::control_action_bindings(&h.w)
            .iter()
            .any(|b| matches!(b.kind, ui::widget::ControlBindingKind::Action(_)));
        assert_eq!(
            ui::widget::controls::has_action_control_bindings(&h.w),
            expected
        );
        if modal {
            assert!(!expected, "collaboration panel hides underlying emitters");
        }
    }
}
