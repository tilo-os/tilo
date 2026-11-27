use super::test_support::*;
tilo_test_env::scenario! {
    /// How a slider is drawn: its knob, its thumb and the area of a shown value.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_sliders_face_knob_and_value_area;

    phase a_disabled_knob_at_either_end_is_dimmed_whole_not_clipped;
    {
        let rect = vello::kurbo::Rect::new(100.0, 0.0, 300.0, 20.0);
        for value in [0.0, 1.0] {
            let face = super::SliderFace::new(rect, value).disabled(true);
            let x = rect.x0 + value * rect.width();
            let reach = super::SliderFace::overhang() + 1.5;
            let bounds = face.paint_bounds();
            assert!(
                bounds.x0 <= x - reach && bounds.x1 >= x + reach,
                "{value}: {bounds:?}"
            );
        }
    }



}

use super::*;
use crate::testing::{Dispatch, dispatch};
use automerge::{AutoCommit, ReadDoc};

use tilo_ui_base::synthetic_input::cursor_moved;

use tilo_ui_base::synthetic_input::mouse_press;

use tilo_ui_base::synthetic_input::mouse_release;

tilo_test_env::scenario! {
    /// What the pointer, a finger and the keys do to a slider's value.
    /// Each phase was a test of its own and builds its own fixture.
    fn pointer_finger_and_keys_move_the_value_on_its_grid;

    phase slider_pointer_input_hovers_drags_and_jumps;
    {
        {
            let mut s = Slider::new("test", "v");
            let r0 = s.knob_radius_for_state();

            s.set_hovered_for_test(true);
            let r1 = s.knob_radius_for_state();

            s.set_dragging_for_test(true);
            let r2 = s.knob_radius_for_state();

            assert!(r0 < r1, "hover must grow knob: idle={r0}, hover={r1}");
            assert!(
                r1 < r2,
                "press must grow knob further: hover={r1}, press={r2}",
            );
        }

        {
            // Rect [0..200, 0..30]. The track runs flush: 200 wide.
            let mut s = slider_at(0.0, 0.0, 200.0, 30.0);

            dispatch(&mut s, &cursor_moved(20.0, 15.0));
            dispatch(&mut s, &mouse_press());
            assert!(s.is_dragging, "press inside rect must start drag");

            // Cursor leaves the rect vertically (still within the track's
            // horizontal range). The drag must continue updating `value`.
            dispatch(&mut s, &cursor_moved(150.0, 200.0));
            let expected = (150.0 / 200.0_f64).clamp(0.0, 1.0);
            assert!(
                (s.value - expected).abs() < 1e-6,
                "drag outside rect must keep tracking cursor x — got {}, expected {}",
                s.value,
                expected
            );

            dispatch(&mut s, &cursor_moved(95.0, -50.0));
            let expected2 = (95.0 / 200.0_f64).clamp(0.0, 1.0);
            assert!(
                (s.value - expected2).abs() < 1e-6,
                "drag continues to track even far outside rect — got {}, expected {}",
                s.value,
                expected2
            );

            dispatch(&mut s, &mouse_release());
            assert!(!s.is_dragging, "release must end drag");
        }

        {
            // Rect [0..200, 0..30]; the track runs flush, so x at 0.8 of
            // the width is 160.
            let mut s = slider_at(0.0, 0.0, 200.0, 30.0);

            let press_x = 0.8 * 200.0;
            dispatch(&mut s, &cursor_moved(press_x, 15.0));
            dispatch(&mut s, &mouse_press());

            assert!(s.is_dragging, "press inside rect must start drag");
            assert!(
                (s.value - 0.8).abs() < 1e-3,
                "press on track at x=0.8 of width must jump value to ≈0.8 — got {}",
                s.value
            );
        }

        {
            // Rect [0..200, 0..30]. A disabled slider must ignore press/drag.
            let mut s = slider_at(0.0, 0.0, 200.0, 30.0);
            s.set_enabled(false);
            let before = s.value;

            dispatch(&mut s, &cursor_moved(20.0, 15.0));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &cursor_moved(150.0, 15.0));

            assert!(!s.is_dragging, "disabled slider must not start a drag");
            assert_eq!(s.value, before, "disabled slider must not change its value");
        }
    }

    phase a_finger_taps_and_drags_a_slider_like_the_pointer;
    {
        use winit::event::TouchPhase;
        // Rect [0..200, 0..30]; the track runs flush, so x is 200 × value.
        let mut s = slider_at(0.0, 0.0, 200.0, 30.0);
        assert!(
            Widget::claims_raw_touch(&s, Point::new(100.0, 15.0)),
            "a finger on the track is the slider's, not a scroll"
        );
        assert!(!Widget::claims_raw_touch(&s, Point::new(100.0, 90.0)));

        assert!(dispatch(&mut s, &finger(TouchPhase::Started, 160.0, 15.0)));
        assert!(s.is_dragging, "a finger down on the track takes hold");
        assert!((s.value - 0.8).abs() < 1e-6, "got {}", s.value);
        assert!(dispatch(&mut s, &finger(TouchPhase::Ended, 160.0, 15.0)));
        assert!(!s.is_dragging, "lifting lets go");
        assert!((s.value - 0.8).abs() < 1e-6, "got {}", s.value);

        // A drag follows the finger, even off the track, and keeps the
        // contact captured until it lifts.
        dispatch(&mut s, &finger(TouchPhase::Started, 40.0, 15.0));
        assert!((s.value - 0.2).abs() < 1e-6, "got {}", s.value);
        assert!(Widget::wants_pointer_capture(&s));
        assert!(dispatch(&mut s, &finger(TouchPhase::Moved, 120.0, 80.0)));
        assert!((s.value - 0.6).abs() < 1e-6, "got {}", s.value);
        assert!(dispatch(
            &mut s,
            &finger(TouchPhase::Cancelled, 120.0, 80.0)
        ));
        assert!(!s.is_dragging && !Widget::wants_pointer_capture(&s));

        let mut hidpi = slider_at(0.0, 0.0, 200.0, 30.0);
        Dispatch::new()
            .scale_factor(2.0)
            .send(&mut hidpi, &finger(TouchPhase::Started, 100.0, 30.0));
        assert!((hidpi.value - 0.25).abs() < 1e-6, "got {}", hidpi.value);

        let mut other = slider_at(0.0, 0.0, 200.0, 30.0);
        let before = other.value;
        assert!(!dispatch(
            &mut other,
            &finger(TouchPhase::Started, 100.0, 90.0)
        ));
        dispatch(&mut other, &finger(TouchPhase::Moved, 20.0, 15.0));
        assert_eq!(other.value, before);
        other.set_enabled(false);
        assert!(!Widget::claims_raw_touch(&other, Point::new(100.0, 15.0)));
        dispatch(&mut other, &finger(TouchPhase::Started, 100.0, 15.0));
        assert_eq!(other.value, before);
    }

    phase fine_drag_uses_the_configured_range_and_step;
    {
        let state = tilo_ui_base::LocalState::new(50.0);
        let mut slider = Slider::from_state(&state).range(20.0, 120.0).step(0.5);
        slider.rect = Rect::new(0.0, 0.0, 200.0, 30.0);
        dispatch(&mut slider, &cursor_moved(60.0, 15.0));
        dispatch(&mut slider, &mouse_press());
        assert_eq!(state.get(), 50.0);
        let shift = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT);
        Dispatch::new()
            .modifiers(shift)
            .send(&mut slider, &cursor_moved(60.0, 15.0));
        Dispatch::new()
            .modifiers(shift)
            .send(&mut slider, &cursor_moved(80.0, 15.0));
        assert_eq!(
            state.get(),
            51.0,
            "Shift slows movement in the slider's units"
        );
        Dispatch::new()
            .modifiers(shift)
            .send(&mut slider, &cursor_moved(83.0, 15.0));
        assert_eq!(
            state.get(),
            51.0,
            "fine movement still honors the step grid"
        );
    }

    phase decimal_steps_land_on_the_grid_they_name;
    {
        let state = tilo_ui_base::LocalState::new(0.0);
        let mut slider = Slider::from_state(&state).step(0.1);
        slider.set_focus(true);
        let right = tilo_ui_base::synthetic_input::named_key(NamedKey::ArrowRight);
        for _ in 0..3 {
            dispatch(&mut slider, &right);
        }
        assert_eq!(state.get(), 0.3, "three tenths is 0.3");
        for _ in 0..4 {
            dispatch(&mut slider, &right);
        }
        assert_eq!(state.get(), 0.7, "seven tenths is 0.7");
    }

    phase keyboard_steps_and_endpoints_preserve_small_changes;
    {
        let state = tilo_ui_base::LocalState::new(0.5);
        let mut slider = Slider::from_state(&state).step(0.0001);
        slider.set_focus(true);
        dispatch(
            &mut slider,
            &tilo_ui_base::synthetic_input::named_key(NamedKey::ArrowRight),
        );
        assert_eq!(state.get(), 0.5001);
        slider.set_value(0.9999);
        dispatch(
            &mut slider,
            &tilo_ui_base::synthetic_input::named_key(NamedKey::End),
        );
        assert_eq!(state.get(), 1.0);
        slider.set_value(0.0001);
        dispatch(
            &mut slider,
            &tilo_ui_base::synthetic_input::named_key(NamedKey::Home),
        );
        assert_eq!(state.get(), 0.0);
    }

    phase ranged_stepped_keyboard_slider_with_a_flush_track;
    {
        let mut slider = Slider::new("delay", "")
            .range(0.0, 10.0)
            .step(0.5)
            .value_text(|seconds| format!("{seconds} s"));
        slider.set_value(3.3);
        assert!((slider.value() - 3.5).abs() < 1e-9, "snaps to the step");
        assert_eq!(
            Widget::accessibility(&slider)
                .and_then(|n| n.value)
                .as_deref(),
            Some("3.5 s")
        );
        slider.set_focus(true);
        let doc = AutoCommit::new();
        let key = |slider: &mut Slider, named: winit::keyboard::NamedKey| {
            let ev =
                tilo_ui_base::synthetic_input::key_event(Key::Named(named), None, ElementState::Pressed);
            slider.handle_key(&ev, &doc, &automerge::ROOT)
        };
        assert!(key(&mut slider, NamedKey::ArrowRight));
        assert!((slider.value() - 4.0).abs() < 1e-9);
        assert!(key(&mut slider, NamedKey::PageUp));
        assert!((slider.value() - 5.0).abs() < 1e-9);
        assert!(key(&mut slider, NamedKey::End));
        assert!((slider.value() - 10.0).abs() < 1e-9);
        assert!(key(&mut slider, NamedKey::Home));
        assert!(slider.value().abs() < 1e-9);

        let theme = Theme::dark();
        let face = SliderFace::new(Rect::new(20.0, 0.0, 220.0, 24.0), 0.0);
        assert_eq!(face.track_rect().x0, 20.0, "the track runs flush");
        assert!((face.value_at(Point::new(120.0, 12.0)) - 0.5).abs() < 1e-9);
        assert_eq!(face.value_at(Point::new(0.0, 12.0)), 0.0);
        let hit = face.hit_rect(&theme);
        assert!(
            hit.x0 < 20.0 && hit.height() >= 32.0,
            "the knob overhangs: {hit:?}"
        );
    }

    phase a_slider_with_stops_lands_on_them_and_keys_go_from_one_to_the_next;
    // The versions of a history along a track, unevenly spaced: a drag or a value lands on
    // the nearest, a key goes on to the next one even when it is far past an arrow's step,
    // and the ends of the track hold.
    {
        let mut slider = Slider::new("versions", "")
            .range(0.0, 100.0)
            .stops([0.0, 2.0, 3.0, 90.0, 140.0]);
        assert_eq!(slider.stop_values(), [0.0, 2.0, 3.0, 90.0], "only those in range");
        slider.set_value(40.0);
        assert!((slider.value() - 3.0).abs() < 1e-9, "the nearest stop");
        assert!((slider.at_fraction(0.8) - 90.0).abs() < 1e-9, "a drag lands on one");
        slider.set_focus(true);
        let doc = AutoCommit::new();
        let key = |slider: &mut Slider, named: winit::keyboard::NamedKey| {
            let ev =
                tilo_ui_base::synthetic_input::key_event(Key::Named(named), None, ElementState::Pressed);
            slider.handle_key(&ev, &doc, &automerge::ROOT)
        };
        assert!(key(&mut slider, NamedKey::ArrowRight));
        assert!((slider.value() - 90.0).abs() < 1e-9, "on to the next, however far");
        assert!(key(&mut slider, NamedKey::ArrowLeft));
        assert!((slider.value() - 3.0).abs() < 1e-9);
        assert!(key(&mut slider, NamedKey::ArrowLeft));
        assert!((slider.value() - 2.0).abs() < 1e-9, "and back one at a time");
        assert!(key(&mut slider, NamedKey::Home));
        assert!(key(&mut slider, NamedKey::ArrowLeft));
        assert!(slider.value().abs() < 1e-9, "the first stop holds");

        let theme = Theme::light();
        let mut scene = Scene::new();
        let face = SliderFace::new(Rect::new(0.0, 0.0, 200.0, 24.0), 0.5);
        paint_stops(&mut scene, Affine::IDENTITY, &theme, &face, &[0.0, 0.25, 0.9]);
        assert!(!scene.encoding().is_empty(), "the stops are drawn");
    }

    phase a_change_of_props_keeps_the_hand_on_the_knob;
    {
        let mut s = slider_at(0.0, 0.0, 200.0, 30.0);
        dispatch(&mut s, &cursor_moved(150.0, 15.0));
        dispatch(&mut s, &mouse_press());
        assert!(s.is_held(), "the press holds the knob");
        let held_from = s.drag_from;
        let bound = tilo_ui_base::component::Bound::<f64>::default();
        bound.state.set(0.2);
        let props = crate::widget::SliderProps {
            value: bound,
            ..Default::default()
        };
        assert!(
            <Slider as tilo_ui_base::component::Described>::set_props(&mut s, props),
            "a value set elsewhere is taken where it stands"
        );
        assert_eq!(s.value, 0.2, "the value the view holds shows");
        assert!(s.is_held(), "the hand still holds the knob");
        assert_eq!(
            s.drag_from, held_from,
            "the drag goes on as the one gesture it was, from where it began"
        );
        dispatch(&mut s, &mouse_release());
        assert!(!s.is_held(), "the release lets go");
    }
}

tilo_test_env::scenario! {
    /// A slider's identity, binding and accessibility.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_slider_is_bound_enabled_and_accessible;

    phase slider_identity_binding_and_enabled_state;
    {
        {
            let s = Slider::new("any", "value");
            assert_eq!(Widget::test_id(&s), Some("slider"));
        }

        {
            let s = Slider::new("any", "value").with_id("volume");
            assert_eq!(Widget::test_id(&s), Some("volume"));
        }

        {
            use tilo_ui_base::a11y::Action;

            let state = tilo_ui_base::LocalState::new(0.25);
            let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let captured = std::rc::Rc::clone(&changes);
            let mut first = Slider::from_state(&state)
                .on_change(move |value| captured.borrow_mut().push(value));
            let mut second = Slider::from_state(&state);
            let mut document = AutoCommit::new();

            state.set(0.5);
            Widget::sync(&mut first, &document, &automerge::ROOT);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(first.value(), 0.5);
            assert_eq!(second.value(), 0.5);
            assert!(changes.borrow().is_empty(), "model sync is not user intent");

            assert!(Widget::perform_action(
                &mut first,
                &Action::Increment,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert_eq!(state.get(), 0.55);
            assert_eq!(&*changes.borrow(), &[0.55]);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(second.value(), 0.55);

            second.set_value(0.8);
            assert_eq!(state.get(), 0.8);
            Widget::sync(&mut first, &document, &automerge::ROOT);
            assert_eq!(first.value(), 0.8);
            assert_eq!(&*changes.borrow(), &[0.55]);
        }

        {
            let s = Slider::new("any", "value").with_enabled(false);
            assert!(!Widget::is_enabled(&s));
            assert!(
                Widget::is_enabled(&Slider::new("any", "value")),
                "default is enabled"
            );
        }
    }

    phase slider_accessibility_reports_and_routes_value_actions;
    {
        {
            use tilo_ui_base::a11y::Role;
            // Default value is 0.5 → "50%".
            let s = Slider::new("any", "value");
            let node = Widget::accessibility(&s).unwrap();
            assert_eq!(node.role, Role::Slider);
            assert_eq!(node.value.as_deref(), Some("50%"));
            assert!(node.state.focusable);
            assert!(!node.state.disabled);
            assert_eq!(node.state.checked, None, "a slider is not checkable");
        }

        {
            let mut s = Slider::new("any", "value").with_enabled(false);
            s.value = 0.25;
            let node = Widget::accessibility(&s).unwrap();
            assert_eq!(node.value.as_deref(), Some("25%"));
            assert!(node.state.disabled);
        }

        {
            let plain = Slider::new("any", "value");
            assert_eq!(Widget::accessibility(&plain).unwrap().name, None);

            let named = Slider::new("any", "value").with_accessible_name("Volume");
            let node = Widget::accessibility(&named).unwrap();
            assert_eq!(node.name.as_deref(), Some("Volume"));
            assert_eq!(node.value.as_deref(), Some("50%"));
        }

        {
            use tilo_ui_base::a11y::Action;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut s = Slider::new("any", "value"); // default 0.5

            assert!(Widget::perform_action(
                &mut s,
                &Action::Increment,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!((s.value - 0.55).abs() < 1e-9, "increment steps +5%");
            assert!(Widget::perform_action(
                &mut s,
                &Action::Decrement,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!((s.value - 0.50).abs() < 1e-9, "decrement steps -5%");

            // SetValue accepts a 0..100 percentage.
            assert!(Widget::perform_action(
                &mut s,
                &Action::SetValue("80%".into()),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!((s.value - 0.80).abs() < 1e-9);

            // SetValue also accepts a 0..1 fraction.
            assert!(Widget::perform_action(
                &mut s,
                &Action::SetValue("0.2".into()),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!((s.value - 0.20).abs() < 1e-9);
        }

        {
            use tilo_ui_base::a11y::Action;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut s = Slider::new("any", "value").with_enabled(false);
            let before = s.value;
            assert!(!Widget::perform_action(
                &mut s,
                &Action::Increment,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &root
            ));
            assert_eq!(s.value, before, "a disabled slider must not change value");
        }

        {
            let mut slider = slider_at(0.0, 0.0, 200.0, 30.0)
                .on_action(crate::widget::test_value_action_binding::<f64>(tilo_actions::ParamType::Number))
                .commits_on_release();
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                &mut slider,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            dispatch(&mut slider, &cursor_moved(20.0, 15.0));
            dispatch(&mut slider, &mouse_press());
            dispatch(&mut slider, &cursor_moved(100.0, 15.0));
            dispatch(&mut slider, &cursor_moved(150.0, 15.0));
            assert!(
                tilo_ui_base::widget::controls::take_control_invocation(&mut slider).is_none(),
                "nothing is invoked while the drag goes on"
            );
            dispatch(&mut slider, &mouse_release());
            let invoked = tilo_ui_base::widget::controls::take_control_invocation(&mut slider)
                .expect("letting go invokes the action");
            let value = invoked.arguments["value"].as_f64().unwrap();
            assert!((value - slider.value).abs() < 1e-9);
            assert!(tilo_ui_base::widget::controls::take_control_invocation(&mut slider).is_none());
        }

        {
            let mut slider = slider_at(0.0, 0.0, 210.0, 30.0);
            let mut shift = Dispatch::new().modifiers(winit::keyboard::ModifiersState::SHIFT);
            dispatch(&mut slider, &cursor_moved(105.0, 15.0));
            dispatch(&mut slider, &mouse_press());
            let start = slider.value;
            shift.send(&mut slider, &cursor_moved(105.0, 15.0));
            let half_track = SliderFace::new(slider.rect, 0.0).track_rect().width() / 2.0;
            shift.send(&mut slider, &cursor_moved(105.0 + half_track, 15.0));
            assert!(
                (slider.value - start - 0.05).abs() < 1e-6,
                "{start} -> {}",
                slider.value
            );
            dispatch(&mut slider, &cursor_moved(105.0 + half_track, 15.0));
            assert!(
                (slider.value - 1.0).abs() < 1e-6,
                "without Shift it follows again"
            );
        }

        {
            let state = tilo_ui_base::LocalState::new(0.0);
            let mut slider = Slider::from_state(&state)
                .on_action(crate::widget::test_value_action_binding::<f64>(tilo_actions::ParamType::Number))
                .commits_on_release();
            slider.rect = Rect::new(0.0, 0.0, 200.0, 30.0);
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                &mut slider,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            dispatch(&mut slider, &cursor_moved(20.0, 15.0));
            dispatch(&mut slider, &mouse_press());
            dispatch(&mut slider, &cursor_moved(150.0, 15.0));
            assert!(
                (state.with(|value| *value) - slider.value).abs() < 1e-9,
                "the local state follows the drag"
            );
            assert!(tilo_ui_base::widget::controls::take_control_invocation(&mut slider).is_none());
            dispatch(&mut slider, &mouse_release());
            assert!(tilo_ui_base::widget::controls::take_control_invocation(&mut slider).is_some());
        }

        {
            let mut slider = Slider::new("native", "legacy")
                .on_change(|_| panic!("the callback gives way to the bound action"))
                .on_action(crate::widget::test_value_action_binding::<f64>(tilo_actions::ParamType::Number));
            let mut doc = AutoCommit::new();
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                &mut slider,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&slider));
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                &mut slider,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            assert!(Widget::is_enabled(&slider));
            assert!(Widget::perform_action(
                &mut slider,
                &tilo_ui_base::a11y::Action::SetValue("80%".into()),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(doc.get(&automerge::ROOT, "legacy").unwrap().is_none());
            assert_eq!(
                tilo_ui_base::widget::controls::take_control_invocation(&mut slider)
                    .unwrap()
                    .arguments["value"],
                0.8
            );
        }
    }
}
#[test]
fn a_press_on_the_track_glides_the_thumb_and_the_drag_after_it_is_welded_to_the_hand() {
    use crate::time::testing::with_monotonic_time;
    let ms = |t0: crate::time::Instant, ms: u64| t0 + crate::time::Duration::from_millis(ms);
    let theme = Theme::dark();
    // Rect [0..200, 0..30]; the track runs flush, so x is 200 × value.
    let mut s = slider_at(0.0, 0.0, 200.0, 30.0);
    let t0 = crate::time::now();
    with_monotonic_time(t0, || present(&mut s, &theme));
    assert!(!s.needs_animation_frame() && !s.needs_redraw(), "at rest");
    with_monotonic_time(t0, || {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        dispatch(&mut s, &cursor_moved(180.0, 15.0));
        dispatch(&mut s, &mouse_press());
        assert!((s.value - 0.9).abs() < 1e-9, "the value is set at once");
        present(&mut s, &theme);
        assert!(s.needs_animation_frame(), "the thumb glides there");
    });
    with_monotonic_time(ms(t0, 40), || {
        assert!(s.needs_redraw());
        present(&mut s, &theme);
        let drawn = s.thumb.get(ms(t0, 40));
        assert!(drawn > 0.5 && drawn < 0.9, "on its way: {drawn}");
        dispatch(&mut s, &cursor_moved(182.0, 15.0));
        assert!(s.thumb.is_moving(ms(t0, 40)));
        assert!((s.thumb.target() - 0.91).abs() < 1e-9);
    });
    with_monotonic_time(ms(t0, 48), || {
        dispatch(&mut s, &cursor_moved(150.0, 15.0));
        assert!((s.value - 0.75).abs() < 1e-9);
        assert_eq!(s.thumb.get(ms(t0, 48)), s.value);
        assert!(!s.needs_animation_frame(), "welded: nothing runs");
        dispatch(&mut s, &cursor_moved(60.0, 15.0));
        assert_eq!(s.thumb.get(ms(t0, 48)), 0.3);
        dispatch(&mut s, &mouse_release());
        present(&mut s, &theme);
        assert!(!s.needs_redraw(), "settled: no repaint");
    });
    with_monotonic_time(ms(t0, 1_000), || {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        dispatch(&mut s, &cursor_moved(64.0, 15.0));
        dispatch(&mut s, &mouse_press());
        assert!(!s.needs_animation_frame());
        assert_eq!(s.thumb.get(ms(t0, 1_000)), s.value);
        dispatch(&mut s, &mouse_release());
    });
}

#[test]
fn keys_and_reduced_motion_land_the_thumb() {
    use crate::time::testing::with_monotonic_time;
    let theme = Theme::dark();
    let mut s = slider_at(0.0, 0.0, 200.0, 30.0);
    let t0 = crate::time::now();
    with_monotonic_time(t0, || {
        present(&mut s, &theme);
        Widget::set_focus(&mut s, true);
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Keyboard);
        dispatch(
            &mut s,
            &tilo_ui_base::synthetic_input::named_key(NamedKey::End),
        );
        assert_eq!(s.value, 1.0);
        present(&mut s, &theme);
        assert_eq!(s.thumb.get(t0), 1.0, "a key lands");
        assert!(!s.needs_animation_frame());
    });
    let calm = Theme::dark().with_reduce_motion(true);
    let mut s = slider_at(0.0, 0.0, 200.0, 30.0);
    with_monotonic_time(t0, || {
        present(&mut s, &calm);
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        dispatch(&mut s, &cursor_moved(20.0, 15.0));
        dispatch(&mut s, &mouse_press());
        present(&mut s, &calm);
        assert!((s.thumb.get(t0) - 0.1).abs() < 1e-9, "reduced motion lands");
        assert!(!s.needs_animation_frame());
    });
}
