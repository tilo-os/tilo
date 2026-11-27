use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::internals::toggle_group::Press;
use tilo_ui::widget::internals::toggle_group::test_support::*;
use tilo_ui::widget::internals::toggle_group::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
tilo_test_env::scenario! {
    fn a_strip_chooses_by_pointer_touch_keyboard_and_action;

    phase painted_strips_refuse_removed_changed_and_interrupted_touch_owners;
    {
        use winit::event::TouchPhase::{Cancelled, Ended, Started};
        let _settled = tilo_ui_base::motion::testing::settled();
        let rects = [
            Rect::new(0.0, 0.0, 100.0, 48.0),
            Rect::new(100.0, 0.0, 200.0, 48.0),
        ];
        let group = SegmentedTouchGroup::new(42, &rects, 0);
        let mut recognizer = SegmentedTouch::default();
        let event = |id, phase, x, y| winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id,
        };
        assert_eq!(
            recognizer.handle(&event(1, Started, 180.0, 24.0), 1.0, &[group]),
            SegmentedTouchResult::Captured
        );
        assert_eq!(
            recognizer.handle(&event(1, Ended, 80.0, 24.0), 1.0, &[]),
            SegmentedTouchResult::Captured
        );
        assert!(!recognizer.is_active());
        recognizer.handle(&event(2, Started, 180.0, 24.0), 1.0, &[group]);
        let changed = SegmentedTouchGroup::new(42, &rects, 1);
        assert_eq!(
            recognizer.handle(&event(2, Ended, 80.0, 24.0), 1.0, &[changed]),
            SegmentedTouchResult::Captured
        );
        recognizer.handle(&event(3, Started, 180.0, 24.0), 1.0, &[group]);
        recognizer.handle(&event(4, Started, 180.0, 24.0), 1.0, &[group]);
        recognizer.handle(&event(3, Ended, 80.0, 24.0), 1.0, &[group]);
        recognizer.handle(&event(5, Started, 180.0, 24.0), 1.0, &[group]);
        recognizer.handle(&event(4, Cancelled, 80.0, 24.0), 1.0, &[group]);
        assert_eq!(
            recognizer.handle(&event(5, Ended, 80.0, 24.0), 1.0, &[group]),
            SegmentedTouchResult::Captured
        );
        assert!(!recognizer.is_active());
        recognizer.handle(&event(6, Started, 180.0, 24.0), 1.0, &[group]);
        assert_eq!(
            recognizer.handle(&event(6, Ended, 80.0, 24.0), 1.0, &[group]),
            SegmentedTouchResult::Selected {
                group: 42,
                index: 1
            }
        );
    }

    phase a_press_focuses_without_a_ring_until_the_keys_are_used;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut g = group_at(
            vec![Segment::new("A"), Segment::new("B")],
            0,
            Rect::new(0.0, 0.0, 200.0, 32.0),
        );
        dispatch(&mut g, &cursor_moved(150.0, 16.0));
        dispatch(&mut g, &mouse_press());
        g.set_focus(true);
        assert!(!g.focus_visible(), "a click shows no focus ring");
        dispatch(&mut g, &named_key(NamedKey::ArrowLeft));
        assert!(g.focus_visible(), "the keys bring the ring back");
        g.set_focus(false);
        g.set_focus(true);
        assert!(g.focus_visible(), "keyboard focus shows the ring");
    }

    phase a_disabled_segment_is_never_chosen_and_the_keys_step_over_it;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut g = group_at(
            vec![
                Segment::new("Whole"),
                Segment::new("Selection").with_enabled(false),
                Segment::new("View"),
            ],
            0,
            Rect::new(0.0, 0.0, 300.0, 36.0),
        );
        dispatch(&mut g, &cursor_moved(150.0, 18.0));
        dispatch(&mut g, &mouse_press());
        assert_eq!(g.selected(), 0, "a click on it chooses nothing");
        g.set_focus(true);
        assert!(dispatch(&mut g, &named_key(NamedKey::ArrowRight)));
        assert_eq!(g.selected(), 2, "the arrow steps over it");
        assert!(dispatch(&mut g, &named_key(NamedKey::ArrowLeft)));
        assert_eq!(g.selected(), 0);
        let node = g.accessibility().expect("a group node");
        assert!(
            node.children
                .get(1)
                .is_some_and(|segment| segment.state.disabled)
        );
    }

    phase toggle_group_selection_follows_pointer_touch_and_keyboard;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let g = ToggleGroup::new(vec![Segment::new("a"), Segment::new("b")], 0);
            assert_eq!(g.selected(), 0);
        }

        {
            let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            let mut g = group_at(
                vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
                0,
                Rect::new(0.0, 0.0, 300.0, 36.0),
            )
            .on_choose("choice", move |i| {
                *captured_clone.borrow_mut() = Some(i);
            });

            dispatch(&mut g, &cursor_moved(250.0, 18.0));
            dispatch(&mut g, &mouse_press());

            assert_eq!(g.selected(), 2, "click in segment 2 must select index 2");
            assert_eq!(
                *captured.borrow(),
                Some(2),
                "the published choice must fire with the new index"
            );
        }

        {
            let changes = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&changes);
            let mut group = ToggleGroup::new(
                vec![
                    Segment::new("One"),
                    Segment::new("Two"),
                    Segment::new("Three"),
                ],
                0,
            )
            .on_choose("choice", move |index| captured.borrow_mut().push(index));
            group.set_focus(true);

            assert!(dispatch(&mut group, &named_key(NamedKey::ArrowRight)));
            assert_eq!(group.selected(), 1);
            assert!(dispatch(&mut group, &named_key(NamedKey::End)));
            assert_eq!(group.selected(), 2);
            assert!(dispatch(&mut group, &named_key(NamedKey::ArrowRight)));
            assert_eq!(group.selected(), 0, "arrow navigation wraps");
            assert!(dispatch(&mut group, &named_key(NamedKey::Enter)));
            assert_eq!(&*changes.borrow(), &[1, 2, 0]);

            group.set_focus(false);
            assert!(!dispatch(&mut group, &named_key(NamedKey::ArrowRight)));
            assert_eq!(
                group.selected(),
                0,
                "unfocused controls do not consume keys"
            );
        }

        {
            let mut group = group_at(
                vec![
                    Segment::new("World"),
                    Segment::new("Alarms"),
                    Segment::new("Timers"),
                ],
                0,
                Rect::new(0.0, 0.0, 300.0, 44.0),
            );
            dispatch(
                &mut group,
                &touch(winit::event::TouchPhase::Started, Point::new(250.0, 22.0)),
            );
            assert_eq!(group.selected(), 0, "touch down does not switch a tab");
            dispatch(
                &mut group,
                &touch(winit::event::TouchPhase::Ended, Point::new(250.0, 22.0)),
            );
            assert_eq!(group.selected(), 2);

            group.set_focus(true);
            dispatch(&mut group, &named_key(NamedKey::ArrowRight));
            assert_eq!(group.selected(), 0, "right arrow wraps to the first tab");
            dispatch(&mut group, &named_key(NamedKey::End));
            assert_eq!(group.selected(), 2);
        }

        {
            let mut group = group_at(
                vec![Segment::new("Light"), Segment::new("Dark")],
                0,
                Rect::new(0.0, 0.0, 200.0, 44.0),
            )
            .with_id("appearance")
            .on_action(appearance_binding());
            // The setting's action is withdrawn: nothing the strip offers can choose it. The
            // person's press is sent to be refused with why, and the strip keeps its choice.
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                &mut group,
                &std::collections::HashSet::new(),
            );
            group.set_focus(true);
            assert!(!Widget::is_enabled(&group));

            dispatch(&mut group, &cursor_moved(150.0, 22.0));
            assert!(dispatch(&mut group, &mouse_press()));
            assert!(
                tilo_ui_base::widget::controls::take_control_invocation(&mut group).is_some(),
                "the press is sent, to be refused"
            );
            assert_eq!(group.selected(), 0);
            assert!(!dispatch(
                &mut group,
                &touch(winit::event::TouchPhase::Started, Point::new(150.0, 22.0)),
            ));
            assert!(!dispatch(&mut group, &named_key(NamedKey::ArrowRight),));
            assert_eq!(group.selected(), 0);
        }
    }

    phase accessibility_and_actions_expose_and_drive_the_selected_segment;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            use tilo_ui_base::a11y::Role;
            let g = ToggleGroup::new(
                vec![
                    Segment::new("Light"),
                    Segment::new("Dark"),
                    Segment::new("Auto"),
                ],
                1,
            );
            let node = Widget::accessibility(&g).expect("toggle group is not presentational");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name, None, "no intrinsic label on the group");
            assert_eq!(
                node.value.as_deref(),
                Some("Dark"),
                "value is the selected segment's label",
            );
            assert!(node.state.focusable);
            assert!(!node.state.disabled);
        }

        {
            let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
            let mut g = group_at(
                vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
                0,
                Rect::new(0.0, 0.0, 300.0, 36.0),
            )
            .on_choose("choice", move |i| *captured.borrow_mut() = Some(i));

            assert_eq!(
                Widget::accessibility(&g).unwrap().value.as_deref(),
                Some("a")
            );
            let segments = Widget::accessibility(&g).unwrap().children;
            assert_eq!(
                segments.get(2).and_then(|segment| segment.bounds),
                g.test_segment_rects().get(2).map(|rect| (*rect).into()),
                "each segment is placed where it is drawn"
            );

            dispatch(&mut g, &cursor_moved(250.0, 18.0));
            dispatch(&mut g, &mouse_press());
            assert_eq!(g.selected(), 2);
            assert_eq!(
                Widget::accessibility(&g).unwrap().value.as_deref(),
                Some("c")
            );
        }

        {
            let changed = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&changed);
            let mut group = ToggleGroup::new(vec![Segment::new("Light"), Segment::new("Dark")], 0)
                .on_choose("choice", move |index| captured.borrow_mut().push(index));

            assert!(Widget::perform_semantic_action(
                &mut group,
                "choice:segment:1",
                &tilo_ui_base::a11y::Action::Activate,
                &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(group.selected(), 1);
            assert_eq!(&*changed.borrow(), &[1]);
        }
    }

    phase a_multi_select_toggle_runs_its_action_with_the_chosen_indices;
    {
        use tilo_ui_base::widget::controls::ActionControls;
        let mut g = ToggleGroup::multiple(
            vec![
                Segment::new("Mon"),
                Segment::new("Tue"),
                Segment::new("Wed"),
            ],
            vec![true, false, false],
        )
        .with_id("days")
        .on_chosen_action(tilo_ui::widget::test_value_action_binding(
            tilo_actions::ParamType::Array,
        ));
        assert_eq!(
            g.control_action_bindings()
                .iter()
                .map(|binding| binding.control_id.as_str())
                .collect::<Vec<_>>(),
            ["days:segment:0", "days:segment:1", "days:segment:2"]
        );
        assert!(Widget::perform_semantic_action(
            &mut g,
            "days:segment:2",
            &tilo_ui_base::a11y::Action::ToggleCheck,
            &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        let invocation = g
            .take_control_invocation()
            .expect("the toggle runs the action");
        assert_eq!(
            invocation.arguments.get("value"),
            Some(&serde_json::json!([0, 2]))
        );
        assert_eq!(
            g.chosen().map(<[bool]>::to_vec),
            Some(vec![true, false, false]),
            "the strip shows the new set once the app hands it back"
        );
        g.refresh_control_action_availability(&std::collections::HashSet::new());
        assert!(
            !Widget::is_enabled(&g),
            "an unavailable action disables the strip"
        );
    }

    phase a_strip_of_choices_runs_the_choice_pressed_and_holds_what_the_setting_says;
    // A strip whose choices each run their own action — a setting's cases — runs the choice pressed
    // and chooses nothing itself: what it fills is the case the setting holds, which the app sets.
    {
        fn binding(edge: &str) -> tilo_ui::tilo_actions::ActionBinding<()> {
            let spec = tilo_ui::tilo_actions::ActionSpec::typed_operation(
                format!("dock_{edge}"),
                "Dock edge",
                "Move the dock",
                tilo_ui::tilo_actions::OperationKind::Effect,
                "fixture.edge",
                vec![],
                |_| tilo_ui::tilo_actions::EncodedAction::encode_operation("fixture.edge", &()).unwrap(),
            );
            let resource = tilo_ui::tilo_actions::ResourceSpec::new("fixture", "fixture", "Fixture")
                .with_action(spec.clone());
            tilo_ui::tilo_actions::ActionBinding::new(&resource, &spec, |()| serde_json::Map::new())
                .unwrap()
        }
        let edges = ["top", "bottom", "left", "right"];
        let mut group = ToggleGroup::new(edges.iter().map(|edge| Segment::new(*edge)).collect(), 1);
        group.set_presses(
            edges
                .iter()
                .map(|edge| Press::Action(tilo_ui_base::widget::ActionEmitter::new(binding(edge))))
                .collect(),
        );
        let mut text = TextRenderer::new();
        group.adopt_theme(&Theme::dark());
        group.measure_naturals(&mut text);
        group.apply_layout_external(Rect::new(0.0, 0.0, 344.0, 36.0));

        assert!(Widget::perform_semantic_action(
            &mut group,
            "segment:2",
            &tilo_ui_base::a11y::Action::Activate,
            &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(
            group.selected(),
            1,
            "the strip holds the case the setting says, not the one pressed"
        );
        let invocation = tilo_ui_base::widget::controls::take_control_invocation(&mut group)
            .expect("the choice pressed queues its own action");
        assert_eq!(
            invocation.address,
            tilo_ui::tilo_actions::ActionAddress::new("fixture", "dock_left")
        );
        assert!(
            tilo_ui_base::widget::controls::take_control_invocation(&mut group).is_none(),
            "one press runs one choice"
        );
    }
}

#[test]
fn hosted_mode_swipes_commit_once_and_reject_scroll_cancel_and_multitouch() {
    use tilo_ui::State;
    use tilo_ui_base::LocalState;
    use winit::event::TouchPhase::{Cancelled, Ended, Moved, Started};
    let _settled = tilo_ui_base::motion::testing::settled();
    let selected = LocalState::new(0_usize);
    let next = selected.clone();
    let commits = Rc::new(RefCell::new(Vec::new()));
    let receipts = Rc::clone(&commits);
    let mut frame = Driver::for_root(
        State::new(),
        ToggleGroup::new(
            vec![
                Segment::new("Photo"),
                Segment::new("Video"),
                Segment::new("Scan"),
            ],
            0,
        )
        .bind_selection(&selected)
        .on_choose("camera-modes", move |index| {
            next.set(index);
            receipts.borrow_mut().push(index);
        }),
        (360, 100),
        Theme::dark(),
    );
    let rect = frame.root::<ToggleGroup>().get_rect();
    let start = rect.center();
    frame.touch(1, Started, start.x, start.y);
    frame.touch(1, Moved, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 0, "motion alone cannot change mode");
    frame.touch(1, Ended, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 1);
    frame.touch(1, Ended, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 1, "a release commits once");
    frame.touch(2, Started, start.x, start.y);
    frame.touch(2, Ended, start.x - 20.0, start.y + 90.0);
    assert_eq!(selected.get(), 1, "vertical scrolling keeps the mode");
    frame.touch(3, Started, start.x, start.y);
    frame.touch(3, Cancelled, start.x - 90.0, start.y);
    frame.touch(3, Ended, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 1, "cancelled swipes cannot commit");
    frame.touch(4, Started, start.x, start.y);
    frame.touch(5, Started, start.x, start.y);
    frame.touch(4, Ended, start.x - 90.0, start.y);
    frame.touch(5, Ended, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 1, "multitouch cannot change modes");
    frame.touch(6, Started, start.x, start.y);
    frame.touch(6, Ended, start.x + 90.0, start.y);
    assert_eq!(selected.get(), 0, "right swipe selects the previous mode");
    assert_eq!(
        &*commits.borrow(),
        &[1, 0],
        "only two deliberate swipes invoke the real view operation"
    );
}
