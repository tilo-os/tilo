use super::test_support::*;
use super::*;
use crate::testing::Dispatch;
use automerge::{AutoCommit, ReadDoc, transaction::Transactable};
use tilo_ui_base::widget::Widget;
use winit::event::{ElementState, MouseButton, WindowEvent};

tilo_test_env::scenario! {
    fn a_switch_reports_and_paints_what_it_is;

    phase toggle_identity_enabled_state_and_accessibility_are_reported;
    {
        {
            let t = Toggle::bound("any", "path");
            assert_eq!(Widget::test_id(&t), Some("toggle"));
        }

        {
            let t = Toggle::bound("any", "path").with_id("airplane_mode");
            assert_eq!(Widget::test_id(&t), Some("airplane_mode"));
        }

        {
            let t = Toggle::bound("any", "path").with_enabled(false);
            assert!(!Widget::is_enabled(&t));
            assert!(
                Widget::is_enabled(&Toggle::bound("any", "path")),
                "default is enabled"
            );
        }

        {
            use tilo_ui_base::a11y::Role;
            let mut t = Toggle::bound("any", "path");
            let off = Widget::accessibility(&t).unwrap();
            assert_eq!(off.role, Role::Switch);
            assert_eq!(off.state.checked, Some(false));

            t.control.is_checked = true; // same module → set the synced on-state directly
            let on = Widget::accessibility(&t).unwrap();
            assert_eq!(on.state.checked, Some(true));
        }

        {
            let plain = Toggle::bound("any", "path");
            assert_eq!(
                Widget::accessibility(&plain).unwrap().name.as_deref(),
                Some("Any")
            );

            let named = Toggle::bound("any", "path").with_accessible_name("Airplane mode");
            assert_eq!(
                Widget::accessibility(&named).unwrap().name.as_deref(),
                Some("Airplane mode"),
            );

            // A row's caption is text, never a message id: naming a switch by
            // the toolkit's own key says the caption, while the message-id
            // setter resolves it in the catalogue.
            let caption =
                Toggle::bound("any", "path").with_resolved_accessible_name("window-root-name");
            assert_eq!(
                Widget::accessibility(&caption).unwrap().name.as_deref(),
                Some("window-root-name"),
            );
            let key = Toggle::bound("any", "path").with_accessible_name("window-root-name");
            assert_eq!(
                Widget::accessibility(&key).unwrap().name.as_deref(),
                Some("Tilo application window"),
            );
        }
    }

}

#[test]
fn toggle_bindings_route_activation_unless_disabled() {
    {
        use tilo_ui_base::a11y::Action;

        let state = tilo_ui_base::LocalState::new(false);
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut first = Toggle::from_state(&state)
            .on_choose("toggle", move |checked| captured.borrow_mut().push(checked));
        let mut second = Toggle::from_state(&state);
        let mut document = AutoCommit::new();

        state.set(true);
        Widget::sync(&mut first, &document, &automerge::ROOT);
        Widget::sync(&mut second, &document, &automerge::ROOT);
        assert!(first.is_checked());
        assert!(second.is_checked());
        assert!(changes.borrow().is_empty(), "model sync is not user intent");

        assert!(Widget::perform_action(
            &mut first,
            &Action::Activate,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut document),
            &automerge::ROOT,
        ));
        assert!(!state.get(), "user activation publishes typed state");
        assert_eq!(&*changes.borrow(), &[false]);

        Widget::sync(&mut second, &document, &automerge::ROOT);
        assert!(!second.is_checked(), "sibling follows the shared model");

        second.set_checked(true);
        assert!(state.get(), "programmatic mutation publishes typed state");
        Widget::sync(&mut first, &document, &automerge::ROOT);
        assert!(first.is_checked());
        assert_eq!(
            &*changes.borrow(),
            &[false],
            "programmatic mutation does not invoke the user callback"
        );
    }

    {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut toggle = Toggle::bound("any", "enabled");
        doc.put(&root, "enabled", true).unwrap();
        Widget::sync(&mut toggle, &doc, &root);
        assert!(toggle.is_checked());

        doc.delete(&root, "enabled").unwrap();
        Widget::sync(&mut toggle, &doc, &root);
        assert!(!toggle.is_checked(), "a removed path means off");

        doc.put(&root, "enabled", "not-a-boolean").unwrap();
        Widget::sync(&mut toggle, &doc, &root);
        assert!(!toggle.is_checked(), "a mistyped path cannot stay stale");
    }

    {
        let mut t = Toggle::bound("id", "on").with_enabled(false);

        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = t.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(60.0),
                height: taffy::AvailableSpace::Definite(40.0),
            },
        )
        .unwrap();
        t.apply_layout(&tree, node, Point::ZERO);

        let device = winit::event::DeviceId::dummy();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut sender = Dispatch::new().doc(&mut doc).layout(&tree, node);
        let moved = sender.send(
            &mut t,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: (10.0, 10.0).into(),
            },
        );
        let pressed = sender.send(
            &mut t,
            &WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        let focus = pressed.focus_target.or(moved.focus_target);
        let handled = moved.handled || pressed.handled;

        assert!(focus.is_none(), "disabled toggle must not request focus");
        assert!(!handled, "disabled toggle must not swallow input");
        assert!(
            doc.get(&root, "on").unwrap().is_none(),
            "disabled toggle must not write the CRDT path"
        );
    }

    {
        let mut toggle = Toggle::bound("native", "on")
            .on_action(tilo_ui_base::widget::test_bool_action_binding());
        let mut doc = AutoCommit::new();
        tilo_ui_base::widget::controls::refresh_control_action_availability(
            &mut toggle,
            &std::collections::HashSet::new(),
        );
        assert!(!Widget::is_enabled(&toggle));
        tilo_ui_base::widget::controls::refresh_control_action_availability(
            &mut toggle,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                "test:control",
                "set",
            )]),
        );
        assert!(Widget::is_enabled(&toggle));
        assert!(Widget::perform_action(
            &mut toggle,
            &tilo_ui_base::a11y::Action::ToggleCheck,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(doc.get(&automerge::ROOT, "on").unwrap().is_none());
        assert_eq!(
            tilo_ui_base::widget::controls::take_control_invocation(&mut toggle)
                .unwrap()
                .arguments["value"],
            true
        );
    }
}

/// A switch whose value comes from an action rather than a document
/// path keeps the value it was built with: syncing never finds an empty
/// key to read and turn it off.
#[test]
fn a_switch_given_its_state_reads_it_and_an_empty_document_path_is_rejected() {
    let mut doc = AutoCommit::new();
    doc.put(&automerge::ROOT, "wifi", false).unwrap();
    let mut toggle = Toggle::given("wifi", true);
    Widget::sync(&mut toggle, &doc, &automerge::ROOT);
    assert!(
        toggle.is_checked(),
        "a given switch keeps the value it was built with"
    );

    let mut action_switch =
        Toggle::given("wifi", false).on_action(tilo_ui_base::widget::test_bool_action_binding());
    tilo_ui_base::widget::controls::refresh_control_action_availability(
        &mut action_switch,
        &std::collections::HashSet::from([tilo_actions::ActionAddress::new("test:control", "set")]),
    );
    assert!(Widget::perform_action(
        &mut action_switch,
        &tilo_ui_base::a11y::Action::Activate,
        &[],
        &mut tilo_ui_base::widget::Edits::lend(&mut doc),
        &automerge::ROOT
    ));
    assert!(
        action_switch.is_checked(),
        "an action-owned switch shows the flip it asked for"
    );
}

#[test]
#[should_panic(expected = "a bound control needs a document key")]
fn binding_a_switch_to_an_empty_document_path_is_rejected() {
    let _ = Toggle::bound("wifi", "");
}

#[test]
fn a_key_an_assistive_action_or_a_sync_lands_the_switch_at_once() {
    use crate::time::testing::with_monotonic_time;
    use tilo_ui_base::tokens::MotionSource;
    let theme = Theme::dark();
    let state = tilo_ui_base::LocalState::new(false);
    let mut t = Toggle::from_state(&state);
    switch_at_origin(&mut t);
    let t0 = crate::time::now();
    with_monotonic_time(t0, || present(&mut t, &theme));
    let mut doc = AutoCommit::new();
    for (source, on) in [
        (MotionSource::Keyboard, true),
        (MotionSource::Action, false),
    ] {
        with_monotonic_time(t0, || {
            tilo_ui_base::motion::note_source(source);
            assert!(Widget::perform_action(
                &mut t,
                &tilo_ui_base::a11y::Action::Activate,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            present(&mut t, &theme);
            assert_eq!(t.switch.knob(t0), if on { 1.0 } else { 0.0 }, "{source:?}");
            assert!(!t.needs_animation_frame(), "{source:?} lands");
        });
    }
    with_monotonic_time(ms(t0, 2_000), || {
        state.set(true);
        Widget::sync(&mut t, &doc, &automerge::ROOT);
        present(&mut t, &theme);
        assert_eq!(t.switch.knob(ms(t0, 2_000)), 1.0);
        assert!(!t.needs_animation_frame());
    });
}

#[test]
fn a_dragged_knob_follows_the_hand_one_to_one_and_settles_on_the_nearer_side() {
    use crate::testing::dispatch;
    use crate::time::testing::with_monotonic_time;
    use tilo_ui_base::{
        synthetic_input::cursor_moved, synthetic_input::mouse_press, synthetic_input::mouse_release,
    };
    let theme = Theme::dark();
    let state = tilo_ui_base::LocalState::new(false);
    let (mut t, changes) = counted(&state);
    let t0 = crate::time::now();
    with_monotonic_time(t0, || {
        present(&mut t, &theme);
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        dispatch(&mut t, &cursor_moved(12.0, 16.0));
        dispatch(&mut t, &mouse_press());
    });
    assert!(!state.get(), "a press changes nothing yet");
    assert!(Widget::wants_pointer_capture(&t), "the hand holds the knob");
    // Within the slop it is still a click; past it the knob is the
    // hand's, 1:1 over its 20 px of travel from where the hand took
    // it, wherever the pointer goes.
    with_monotonic_time(ms(t0, 8), || dispatch(&mut t, &cursor_moved(14.0, 16.0)));
    assert!(!t.hold.unwrap().dragged);
    with_monotonic_time(ms(t0, 16), || dispatch(&mut t, &cursor_moved(20.0, 16.0)));
    for (i, (x, knob)) in [(26.0, 0.7), (30.0, 0.9), (18.0, 0.3), (13.0, 0.05)]
        .into_iter()
        .enumerate()
    {
        let at = ms(t0, 24 + 8 * u64::try_from(i).unwrap());
        with_monotonic_time(at, || {
            dispatch(&mut t, &cursor_moved(x, 60.0));
            present(&mut t, &theme);
        });
        assert!(
            (t.switch.knob(at) - knob).abs() < 1e-9,
            "at x {x}: {}",
            t.switch.knob(at)
        );
        assert!(!t.switch.is_moving(at), "nothing runs under the hand");
    }
    assert_eq!(changes.get(), 0, "the knob moves; the value does not");
    with_monotonic_time(ms(t0, 60), || dispatch(&mut t, &mouse_release()));
    assert!(!state.get(), "left on the off side");
    assert_eq!(changes.get(), 0, "a drag back to the start changes nothing");
    assert!(!Widget::wants_pointer_capture(&t));
    let rest = t.switch.knob.settles_at().map_or(ms(t0, 60), |at| at);
    assert_eq!(t.switch.knob(rest), 0.0);

    let state = tilo_ui_base::LocalState::new(false);
    let (mut t, changes) = counted(&state);
    with_monotonic_time(t0, || {
        present(&mut t, &theme);
        dispatch(&mut t, &cursor_moved(12.0, 16.0));
        dispatch(&mut t, &mouse_press());
        dispatch(&mut t, &cursor_moved(16.0, 16.0));
    });
    with_monotonic_time(ms(t0, 10), || dispatch(&mut t, &cursor_moved(30.0, 16.0)));
    assert_eq!(changes.get(), 0);
    with_monotonic_time(ms(t0, 300), || dispatch(&mut t, &mouse_release()));
    assert!(state.get(), "left on the on side");
    assert_eq!(changes.get(), 1, "a drag across changes the value once");
    assert_eq!(t.switch.knob(ms(t0, 2_000)), 1.0);

    with_monotonic_time(ms(t0, 3_000), || {
        dispatch(&mut t, &cursor_moved(30.0, 16.0));
        dispatch(&mut t, &mouse_press());
        dispatch(&mut t, &cursor_moved(31.0, 90.0));
        dispatch(&mut t, &mouse_release());
    });
    assert!(state.get());
    assert_eq!(changes.get(), 1);
}

#[test]
fn a_click_slides_the_knob_and_a_second_click_reverses_it_mid_flight() {
    use crate::testing::dispatch;
    use crate::time::testing::with_monotonic_time;
    use tilo_ui_base::tokens::MotionSource;
    use tilo_ui_base::{
        synthetic_input::cursor_moved, synthetic_input::mouse_press, synthetic_input::mouse_release,
    };
    let theme = Theme::dark();
    let state = tilo_ui_base::LocalState::new(false);
    let mut t = Toggle::from_state(&state);
    switch_at_origin(&mut t);
    let t0 = crate::time::now();
    with_monotonic_time(t0, || present(&mut t, &theme));
    assert!(!t.needs_animation_frame(), "the first paint rests");
    assert_eq!(t.switch.knob(t0), 0.0);
    with_monotonic_time(t0, || {
        tilo_ui_base::motion::note_source(MotionSource::Pointer);
        dispatch(&mut t, &cursor_moved(30.0, 16.0));
        dispatch(&mut t, &mouse_press());
        dispatch(&mut t, &mouse_release());
        assert!(state.get(), "the value changes once the click lets go");
        present(&mut t, &theme);
        assert!(t.needs_animation_frame(), "the knob slides");
    });
    let drawn = with_monotonic_time(ms(t0, 40), || {
        assert!(t.needs_redraw(), "a sliding knob repaints each frame");
        present(&mut t, &theme);
        let pose = t.switch.pose(theme.motion(), ms(t0, 40));
        assert!(pose.knob > 0.0 && pose.knob < 1.0, "on its way: {pose:?}");
        assert!(
            pose.tint > 0.0 && pose.tint < 1.0,
            "the track turns with it"
        );
        pose.knob
    });
    with_monotonic_time(ms(t0, 40), || {
        tilo_ui_base::motion::note_source(MotionSource::Pointer);
        dispatch(&mut t, &mouse_press());
        dispatch(&mut t, &mouse_release());
        assert!(!state.get());
        present(&mut t, &theme);
        let now = ms(t0, 40);
        assert!((t.switch.knob(now) - drawn).abs() < 1e-9, "no jump");
    });
    with_monotonic_time(ms(t0, 600), || {
        assert_eq!(t.switch.knob(ms(t0, 600)), 0.0, "back off");
        assert!(!t.needs_animation_frame(), "settled: no frames");
        present(&mut t, &theme);
        assert!(!t.needs_redraw(), "settled: no repaint");
    });
}

#[test]
fn reduced_motion_lands_the_knob_and_cross_fades_the_tint() {
    use crate::testing::dispatch;
    use crate::time::testing::with_monotonic_time;
    use tilo_ui_base::{
        synthetic_input::cursor_moved, synthetic_input::mouse_press, synthetic_input::mouse_release,
    };
    let theme = Theme::dark().with_reduce_motion(true);
    let state = tilo_ui_base::LocalState::new(false);
    let mut t = Toggle::from_state(&state);
    switch_at_origin(&mut t);
    let t0 = crate::time::now();
    with_monotonic_time(t0, || {
        present(&mut t, &theme);
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        dispatch(&mut t, &cursor_moved(30.0, 16.0));
        dispatch(&mut t, &mouse_press());
        dispatch(&mut t, &mouse_release());
        present(&mut t, &theme);
    });
    let pose = t.switch.pose(theme.motion(), ms(t0, 40));
    assert_eq!(pose.knob, 1.0, "the knob lands");
    assert!(
        pose.tint > 0.0 && pose.tint < 1.0,
        "the tint fades: {pose:?}"
    );
    assert!(!t.switch.is_moving(ms(t0, 400)));
}
