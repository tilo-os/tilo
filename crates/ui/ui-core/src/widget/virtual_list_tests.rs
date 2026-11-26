use super::test_support::*;
use super::*;
use crate::time::Duration;
use automerge::AutoCommit;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tilo_ui_base::collection_transition::ListModelChange;

use crate::testing::Dispatch;
use crate::widget::context_menu::MenuItem;

tilo_test_env::scenario! {
    fn a_lists_model_and_delegates_keep_stable_keys;

    phase semantic_rows_share_scrolled_variable_geometry_and_stable_key_actions;
    {
        {
            let activated = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&activated);
            let model = VecListModel::new(vec![10_u64, 20], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    24.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0)
            .with_semantic_key(u64::to_string)
            .on_activate(move |key| captured.borrow_mut().push(key));
            let _ = lay_out(&mut list, 300.0);

            let semantics = Widget::accessibility(&list).expect("list semantics");
            assert_eq!(
                semantics.children[1].action_target.as_deref(),
                Some("key:20")
            );
            assert!(Widget::perform_semantic_action(
                &mut list,
                "key:20",
                &tilo_ui_base::a11y::Action::Focus,
                &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(list.focused_key(), Some(&20));
            assert!(Widget::perform_semantic_action(
                &mut list,
                "key:20",
                &tilo_ui_base::a11y::Action::Activate,
                &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert!(list.selection().is_selected(&20));
            assert_eq!(&*activated.borrow(), &[20]);
        }

        {
            let opened = Rc::new(RefCell::new(Vec::new()));
            let chosen = Rc::new(RefCell::new(Vec::new()));
            let (opens, chooses) = (Rc::clone(&opened), Rc::clone(&chosen));
            let model = VecListModel::new(vec![10_u64, 20], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    24.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0)
            .with_semantic_key(u64::to_string)
            .on_select(move |key, _| chooses.borrow_mut().push(key))
            .on_activate(move |key| opens.borrow_mut().push(key));
            let _ = lay_out(&mut list, 300.0);
            assert!(Widget::perform_semantic_action(
                &mut list,
                "key:20",
                &tilo_ui_base::a11y::Action::Activate,
                &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert!(list.selection().is_selected(&20));
            assert_eq!(&*opened.borrow(), &[20], "the row opens once");
            assert!(
                chosen.borrow().is_empty(),
                "and choosing it is not reported again"
            );
        }

        {
            let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    24.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(48.0);
            list.set_scroll_offset(10.0);
            let _ = lay_out(&mut list, 300.0);

            let semantics = Widget::accessibility(&list).expect("list semantics");
            let first = semantics.children.first().expect("first mounted row");
            let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
            assert_eq!(
                first.bounds.expect("row bounds").to_rect(),
                Rect::new(0.0, -10.0, 300.0 - gutter, 14.0),
                "the list's own row box, clear of the bar's gutter",
            );
            assert_eq!(
                first.name.as_deref(),
                Some("1"),
                "the row reads as the control it holds",
            );
            assert!(
                first
                    .children
                    .iter()
                    .all(|node| node.role != tilo_ui_base::a11y::Role::Button),
                "the row's own widget is not handed over a second time"
            );
        }

        {
            let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    24.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            assert!(list.set_row_height(0, 300.0));
            list.focused_key = Some(ListKey::new(1u32));

            assert!(list.on_key(&Key::Named(NamedKey::PageDown), false, false));
            assert_eq!(list.focused_key(), Some(&1));
            assert_eq!(list.scroll_offset(), 90.0);
            assert!(list.on_key(&Key::Named(NamedKey::PageUp), false, false));
            assert_eq!(list.focused_key(), Some(&1));
            assert_eq!(list.scroll_offset(), 0.0);
        }

        {
            let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    24.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            list.focused_key = Some(ListKey::new(1u32));

            assert!(list.on_key(&Key::Named(NamedKey::PageDown), false, false));
            assert_eq!(list.focused_key(), Some(&3));
            assert!(list.on_key(&Key::Named(NamedKey::PageUp), false, false));
            assert_eq!(list.focused_key(), Some(&1));
            assert_eq!(list.scroll_offset(), 0.0);
        }
    }

}

tilo_test_env::scenario! {
    fn the_pointer_presses_opens_and_reorders_rows;

    phase pointer_input_separates_delegate_presses_drag_scroll_and_selection;
    {
        {
            let presses = Rc::new(Cell::new(0));
            let captured = Rc::clone(&presses);
            let model = VecListModel::new(vec![7_u64], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 30.0, move |_, _, key| {
                Box::new(ProbeRow::new(
                    format!("action {key}"),
                    30.0,
                    true,
                    Rc::clone(&captured),
                ))
            })
            .with_viewport_height(30.0);
            let (taffy, node) = lay_out(&mut list, 200.0);

            dispatch(&mut list, &cursor(15.0), &taffy, node);
            assert!(dispatch(
                &mut list,
                &left(ElementState::Pressed),
                &taffy,
                node
            ));
            assert_eq!(presses.get(), 1);
            assert!(!list.selection().is_selected(&7));

            let semantics = list.accessibility().unwrap();
            assert_eq!(semantics.role, tilo_ui_base::a11y::Role::List);
            assert_eq!(semantics.children[0].role, tilo_ui_base::a11y::Role::ListItem);
            assert_eq!(semantics.children[0].name.as_deref(), Some("action 7"));
            assert!(
                semantics.children[0]
                    .children
                    .iter()
                    .all(|node| node.role != tilo_ui_base::a11y::Role::Button),
                "the row's own widget is not handed over a second time"
            );
        }

        {
            let model = VecListModel::new((0_u64..100).collect(), |key| *key).unwrap();
            let mut list = VirtualList::new(model, 20.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            let (taffy, node) = lay_out(&mut list, 200.0);

            dispatch(&mut list, &cursor(50.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
            dispatch(&mut list, &cursor(10.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Released), &taffy, node);
            assert_eq!(list.scroll_offset(), 40.0);
            assert!(list.selection().is_empty(100));

            list.set_scroll_offset(0.0);
            let (taffy, node) = lay_out(&mut list, 200.0);
            dispatch(&mut list, &cursor(10.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
            dispatch(&mut list, &left(ElementState::Released), &taffy, node);
            assert!(list.selection().is_selected(&0));
        }

        {
            let model = VecListModel::new((0_u64..100).collect(), |key| *key).unwrap();
            let mut list = VirtualList::new(model, 20.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            let (taffy, node) = lay_out(&mut list, 200.0);
            list.reduce_motion = true;

            dispatch(&mut list, &cursor(50.0), &taffy, node);
            let wheel = tilo_ui_base::synthetic_input::mouse_wheel(0.0, -1.0);
            assert!(dispatch(&mut list, &wheel, &taffy, node));
            assert_eq!(list.scroll_offset(), 60.0);
            assert!(!list.scroll.momentum.is_active());

            let now = crate::time::now();
            let started = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
            let moved = now.checked_sub(Duration::from_millis(5)).unwrap_or(now);
            let offset = list.scroll_offset();
        list.drag_scroll.begin(50.0, offset, started);
            assert!(list.drag_scroll.update(10.0, moved).is_some());
            assert!(dispatch(
                &mut list,
                &left(ElementState::Released),
                &taffy,
                node
            ));
            assert!(!list.scroll.momentum.is_active());
            assert!(!list.needs_animation_frame());
        }
    }


    phase a_press_opens_the_row_of_a_list_that_opens_on_press;
    {
        let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
        let api = Rc::clone(&opened);
        let model = VecListModel::new((0_u64..3).collect(), |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(60.0)
        .with_semantic_key(ToString::to_string)
        .open_on_press()
        .on_activate(move |key| api.borrow_mut().push(key));
        let (taffy, node) = lay_out(&mut list, 200.0);

        dispatch(&mut list, &cursor(10.0), &taffy, node);
        dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
        dispatch(&mut list, &left(ElementState::Released), &taffy, node);

        assert_eq!(*opened.borrow(), vec![0], "one press opens the row");
        assert!(
            list.selection().is_selected(&0),
            "the row it opened is the choice"
        );
    }

    phase list_reorder_previews_and_commits_valid_drops_only;
    {
        {
            let mut list = VirtualList::from_items(
                vec![0_u64, 1, 2],
                20.0,
                |key| *key,
                |key, _, _| {
                    Box::new(ProbeRow::new(
                        key.to_string(),
                        20.0,
                        false,
                        Rc::new(Cell::new(0)),
                    ))
                },
            )
            .unwrap()
            .with_viewport_height(60.0)
            .reorderable();
            let (taffy, node) = lay_out(&mut list, 200.0);

            dispatch(&mut list, &cursor(10.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
            dispatch(&mut list, &cursor(55.0), &taffy, node);
            assert_eq!(
                list.reorder_target(),
                Some(DropTarget::new(2, DropPlacement::After))
            );
            assert!(list.reorder_preview().is_some());
            dispatch(&mut list, &left(ElementState::Released), &taffy, node);

            assert_eq!(list.model().items(), &[1, 2, 0]);
            assert_eq!(list.model().index_of(&0), Some(2));
            assert_eq!(list.reorder_target(), None);
        }

        {
            let model = VecListModel::new(vec![0_u64, 1], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 20.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(40.0)
            .with_reorder(|_, _, _| false, |_, _| true);
            let (taffy, node) = lay_out(&mut list, 200.0);

            dispatch(&mut list, &cursor(10.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
            dispatch(&mut list, &cursor(35.0), &taffy, node);
            assert!(list.reorder_preview().is_none());
            dispatch(&mut list, &left(ElementState::Released), &taffy, node);
            assert_eq!(list.model().items(), &[0, 1]);
        }
    }

    phase a_tap_on_a_row_wrapped_in_its_swipe_is_its_click;
    {
        let mut list = VirtualList::from_items(
            vec![0_u32, 1, 2],
            40.0,
            |item| *item,
            |item, _, _| {
                let row = Box::new(crate::widget::Text::new(&format!("row {item}"))) as Element;
                Box::new(crate::widget::SwipeRow::new(row, None, None)) as Element
            },
        )
        .unwrap()
        .with_viewport_height(200.0);
        let (taffy, node) = lay_out(&mut list, 300.0);
        let at = list.row_rect(&1).expect("the row is placed").center();
        let send = |list: &mut VirtualList<VecListModel<u32, u32>>, event: &WindowEvent| {
            Dispatch::new()
                .layout(&taffy, node)
                .send(list, event)
                .handled
        };
        let touch = |phase, at: Point| {
            WindowEvent::Touch(winit::event::Touch {
                device_id: winit::event::DeviceId::dummy(),
                phase,
                location: winit::dpi::PhysicalPosition::new(at.x, at.y),
                force: None,
                id: 7,
            })
        };
        send(&mut list, &touch(winit::event::TouchPhase::Started, at));
        send(&mut list, &tilo_ui_base::synthetic_input::cursor_moved(at.x, at.y));
        send(&mut list, &tilo_ui_base::synthetic_input::mouse_press());
        send(&mut list, &tilo_ui_base::synthetic_input::mouse_release());
        send(&mut list, &touch(winit::event::TouchPhase::Ended, at));
        assert!(
            list.selection().is_selected(&1),
            "the tapped row is the list's choice, not the swipe's"
        );
    }

    phase a_press_above_the_list_reaches_no_row_scrolled_out_of_view;
    // A row scrolled past the list's viewport is painted nowhere, so it takes no press either: a
    // press above the list reaches what stands there, never a row the list keeps out of sight.
    {
        let presses = Rc::new(Cell::new(0));
        let counted = Rc::clone(&presses);
        let model = VecListModel::new(vec![1_u64, 2], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 24.0, move |_, _, _| {
            Box::new(ProbeRow::new("row", 24.0, true, Rc::clone(&counted)))
        })
        .with_viewport_height(48.0);
        list.set_scroll_offset(10.0);
        let (taffy, node) = lay_out(&mut list, 300.0);
        // The list stands sixty pixels down its page, and its first row ten pixels above it: the row
        // is drawn nowhere, and a finger above the list lands on the page, not on it.
        list.apply_layout(&taffy, node, Point::new(0.0, 60.0));
        assert_eq!(list.get_rect(), Rect::new(0.0, 60.0, 300.0, 108.0));
        let above = cursor(55.0);
        dispatch(&mut list, &above, &taffy, node);
        dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
        assert_eq!(
            presses.get(),
            0,
            "a press above the list reaches no row of it"
        );
        // Where the list shows the row, the press is the row's.
        let on_it = cursor(70.0);
        dispatch(&mut list, &on_it, &taffy, node);
        dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
        assert_eq!(presses.get(), 1, "a press on the row reaches it");
    }
}

tilo_test_env::scenario! {
    fn the_keyboard_and_modified_clicks_choose_rows;

    phase ctrl_arrows_move_the_focused_row_through_the_reorder_commit;
    {
        fn send(
            list: &mut VirtualList<VecListModel<u32, u32>>,
            key: NamedKey,
            modifiers: winit::keyboard::ModifiersState,
            (taffy, node): (&TaffyTree<()>, NodeId),
        ) -> bool {
            Dispatch::new()
                .modifiers(modifiers)
                .layout(taffy, node)
                .send(list, &tilo_ui_base::synthetic_input::named_key(key))
                .handled
        }
        let ctrl = winit::keyboard::ModifiersState::CONTROL;
        let alt = winit::keyboard::ModifiersState::ALT;
        let plain = winit::keyboard::ModifiersState::empty();

        let mut list = plain_rows(3).reorderable();
        let (taffy, node) = lay_out(&mut list, 300.0);
        let laid_out = (&taffy, node);
        list.set_focus(true);
        assert!(list.reveal(&0));
        assert!(send(&mut list, NamedKey::ArrowDown, ctrl, laid_out));
        assert_eq!(list.model().items(), &[1, 0, 2], "Ctrl+Down moves it down");
        assert_eq!(list.focused_key(), Some(&0), "the moved row keeps focus");
        assert!(send(&mut list, NamedKey::ArrowUp, alt, laid_out));
        assert_eq!(list.model().items(), &[0, 1, 2], "Alt+Up, as a tree's rows");
        assert!(
            send(&mut list, NamedKey::ArrowUp, ctrl, laid_out),
            "the first row has nowhere up to go, and the list still takes its chord"
        );
        assert_eq!(list.model().items(), &[0, 1, 2]);
        assert_eq!(
            list.focused_key(),
            Some(&0),
            "and a refused move is no navigation"
        );
        assert!(send(&mut list, NamedKey::ArrowDown, plain, laid_out));
        assert_eq!(list.focused_key(), Some(&1), "a plain arrow still walks");
        assert_eq!(list.model().items(), &[0, 1, 2]);

        // A list that does not reorder walks under Ctrl as it always did.
        let mut fixed = plain_rows(3);
        let (taffy, node) = lay_out(&mut fixed, 300.0);
        fixed.set_focus(true);
        assert!(fixed.reveal(&0));
        assert!(send(&mut fixed, NamedKey::ArrowDown, ctrl, (&taffy, node)));
        assert_eq!(fixed.focused_key(), Some(&1));
        assert_eq!(fixed.model().items(), &[0, 1, 2]);
    }

    phase enter_chooses_a_row_that_opens_nothing;
    {
        let chosen = Rc::new(RefCell::new(Vec::new()));
        let choosing = Rc::clone(&chosen);
        let mut list = plain_rows(3).on_select(move |key, selected| {
            if selected {
                choosing.borrow_mut().push(key);
            }
        });
        let (taffy, node) = lay_out(&mut list, 300.0);
        list.set_focus(true);
        assert!(list.reveal(&1));
        assert!(
            Dispatch::new()
                .layout(&taffy, node)
                .send(
                    &mut list,
                    &tilo_ui_base::synthetic_input::named_key(NamedKey::Enter)
                )
                .handled
        );
        assert!(list.selection().is_selected(&1));
        assert_eq!(&*chosen.borrow(), &[1], "chosen once");
    }

    phase shift_arrows_extend_a_choice_and_escape_puts_it_away;
    {
        let reported = Rc::new(RefCell::new(Vec::<Vec<u32>>::new()));
        let reports = Rc::clone(&reported);
        let mut list = plain_rows(6)
            .multi_select(true)
            .on_selected(move |keys| reports.borrow_mut().push(keys.to_vec()));
        let (taffy, node) = lay_out(&mut list, 300.0);
        list.set_focus(true);
        assert!(list.reveal(&1));
        list.select_only(Some(1));
        let shift = winit::keyboard::ModifiersState::SHIFT;
        for _ in 0..2 {
            assert!(
                Dispatch::new()
                    .modifiers(shift)
                    .layout(&taffy, node)
                    .send(
                        &mut list,
                        &tilo_ui_base::synthetic_input::named_key(NamedKey::ArrowDown)
                    )
                    .handled,
                "the list takes the extended walk"
            );
        }
        assert_eq!(
            &*reported.borrow(),
            &[vec![1, 2], vec![1, 2, 3]],
            "each step tells the listener the whole choice, in row order"
        );
        assert!(
            list.selection().is_selected(&3),
            "the row the keyboard walked to is chosen"
        );
        assert!(
            Dispatch::new()
                .layout(&taffy, node)
                .send(
                    &mut list,
                    &tilo_ui_base::synthetic_input::named_key(NamedKey::Escape)
                )
                .handled,
            "Escape belongs to a list that holds a choice"
        );
        assert_eq!(
            reported.borrow_mut().pop(),
            Some(Vec::new()),
            "the choice is put away, and the listener hears of it"
        );
        assert!(
            list.selection().is_empty(6),
            "an empty choice, not one merely hidden"
        );
    }

    phase a_modified_click_changes_the_choice_without_opening_the_row;
    {
        let opened = Rc::new(RefCell::new(Vec::<u32>::new()));
        let reported = Rc::new(RefCell::new(Vec::<Vec<u32>>::new()));
        let (opens, reports) = (Rc::clone(&opened), Rc::clone(&reported));
        let mut list = plain_rows(4)
            .multi_select(true)
            .on_select(move |key, _| opens.borrow_mut().push(key))
            .on_selected(move |keys| reports.borrow_mut().push(keys.to_vec()));
        let ctrl = winit::keyboard::ModifiersState::CONTROL;
        click_row(&mut list, 1, ctrl);
        assert_eq!(
            list.selection().stored_keys().copied().collect::<Vec<_>>(),
            vec![1],
            "the modified click chooses the row it lands on"
        );
        click_row(&mut list, 3, ctrl);
        let mut chosen: Vec<u32> = list.selection().stored_keys().copied().collect();
        chosen.sort_unstable();
        assert_eq!(chosen, vec![1, 3], "and the next one adds to the choice");
        assert!(
            opened.borrow().is_empty(),
            "a click that chooses as part of a choice opens nothing: {:?}",
            opened.borrow()
        );
        assert_eq!(
            &*reported.borrow(),
            &[vec![1], vec![1, 3]],
            "a choice of one row is kept, so the next click adds to it"
        );
        click_row(&mut list, 2, winit::keyboard::ModifiersState::empty());
        assert_eq!(&*opened.borrow(), &[2], "a plain click opens its row");
        assert_eq!(
            list.selection().stored_keys().copied().collect::<Vec<_>>(),
            vec![2],
            "and leaves a choice of that row alone"
        );
    }

    phase shift_and_the_arrow_extend_the_choice_a_modified_click_gathered;
    // How a person gathers a few rows: click one, add the next with Control held, then walk
    // with Shift and the arrow. Every step reports the choice; the walk extends the rows they
    // gathered instead of replacing them, so the choice the window's bar stands for grows.
    {
        let reported = Rc::new(RefCell::new(Vec::<Vec<u32>>::new()));
        let reports = Rc::clone(&reported);
        let mut list = plain_rows(6)
            .selection_follows_focus()
            .multi_select(true)
            .on_selected(move |keys| reports.borrow_mut().push(keys.to_vec()));
        click_row(&mut list, 0, winit::keyboard::ModifiersState::empty());
        click_row(&mut list, 1, winit::keyboard::ModifiersState::CONTROL);
        list.set_focus(true);
        let (taffy, node) = lay_out(&mut list, 300.0);
        let shift = winit::keyboard::ModifiersState::SHIFT;
        for _ in 0..2 {
            assert!(
                Dispatch::new()
                    .modifiers(shift)
                    .layout(&taffy, node)
                    .send(
                        &mut list,
                        &tilo_ui_base::synthetic_input::named_key(NamedKey::ArrowDown)
                    )
                    .handled,
                "the list takes the extended walk"
            );
        }
        assert_eq!(
            &*reported.borrow(),
            &[vec![0], vec![0, 1], vec![0, 1, 2], vec![0, 1, 2, 3]],
            "the walk grows the choice the modified click added to, one row per step"
        );
    }

    phase selection_can_follow_the_keyboard_focus;
    {
        let selected = Rc::new(RefCell::new(Vec::new()));
        let record = Rc::clone(&selected);
        let mut list = plain_rows(10)
            .selection_follows_focus()
            .on_select(move |key, on| record.borrow_mut().push((key, on)));
        let (taffy, node) = lay_out(&mut list, 300.0);
        list.set_focus(true);
        assert!(list.reveal(&2));
        let down = tilo_ui_base::synthetic_input::named_key(NamedKey::ArrowDown);
        assert!(dispatch(&mut list, &down, &taffy, node));
        assert_eq!(list.focused_key(), Some(&3));
        assert!(
            list.selection().is_selected(&3),
            "the focused row is chosen"
        );
        assert!(!list.selection().is_selected(&2));
        assert_eq!(selected.borrow().last(), Some(&(3, true)));
    }
}

tilo_test_env::scenario! {
    fn a_list_places_its_rows_and_its_viewport;





    phase a_transcript_opens_at_its_end_and_follows_it_until_scrolled_away;
    {
        let mut list = plain_rows(10_000).following_end();
        for _ in 0..4 {
            let _ = lay_out(&mut list, 300.0);
        }
        assert!(list.is_at_end());
        assert!(list.max_scroll_offset() > 0.0);
        assert!((list.max_scroll_offset() - list.scroll_offset()).abs() < 0.01);
        assert!(
            list.visible_range().contains(&9_999),
            "the newest row shows"
        );

        list.update_model_changes(
            &[ListModelChange::RowsInserted {
                index: 10_000,
                count: 1,
            }],
            |model| model.push(10_000, 10_000).unwrap(),
        );
        for _ in 0..2 {
            let _ = lay_out(&mut list, 300.0);
        }
        assert!((list.max_scroll_offset() - list.scroll_offset()).abs() < 0.01);
        assert!(list.visible_range().contains(&10_000));

        // Scrolled back to read: an arrival leaves the rows where they are. (Rows as tall as
        // the estimate, so that rows settling their heights above move nothing either.)
        let mut list = VirtualList::from_items(
            (0..10_000_u32).collect(),
            40.0,
            |item| *item,
            |_, _, _| Box::new(tilo_ui_base::widget::Spacer::fixed(40.0)) as Element,
        )
        .unwrap()
        .with_viewport_height(200.0)
        .following_end();
        let _ = lay_out(&mut list, 300.0);
        let offset = list.scroll_offset() - 500.0;
        list.set_scroll_offset(offset);
        let _ = lay_out(&mut list, 300.0);
        assert!(!list.is_at_end());
        let reading = list.scroll_offset();
        list.update_model_changes(
            &[ListModelChange::RowsInserted {
                index: 10_000,
                count: 1,
            }],
            |model| model.push(10_000, 10_000).unwrap(),
        );
        let _ = lay_out(&mut list, 300.0);
        assert!(
            (list.scroll_offset() - reading).abs() < 0.01,
            "the rows read stay put"
        );
        assert!(!list.is_at_end());

        let offset = list.max_scroll_offset();

        list.set_scroll_offset(offset);
        assert!(list.is_at_end());
    }

    phase reveal_scrolls_a_row_into_view_and_focuses_it;
    {
        let mut list = plain_rows(1000);
        let _ = lay_out(&mut list, 300.0);
        assert!(list.reveal(&700));
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(list.focused_key(), Some(&700));
        assert!(list.visible_range().contains(&700));
        assert!(!list.reveal(&5000), "an unknown key reveals nothing");
    }

    phase a_row_a_view_asks_for_is_shown_and_chosen_without_the_keyboard;
    // A press that made a row asks for it: the list scrolls to it and stands it chosen,
    // and the person goes on typing where they made it. A row the model does not hold
    // yet is shown as soon as it does.
    {
        let keys = Rc::new(RefCell::new((0_u64..1000).collect::<Vec<_>>()));
        let shown: tilo_ui_base::LocalState<Option<u64>> = tilo_ui_base::LocalState::new(None);
        let mut list = VirtualList::new(
            SharedOrder {
                keys: Rc::clone(&keys),
            },
            40.0,
            |_, _, key| Box::new(crate::widget::Text::new(&format!("row {key}"))) as Element,
        )
        .reveal_when(&shown)
        .with_viewport_height(200.0);
        let _ = lay_out(&mut list, 300.0);

        shown.set(Some(700));
        let _ = lay_out(&mut list, 300.0);
        assert!(list.visible_range().contains(&700), "the row is on screen");
        assert!(list.selection().is_selected(&700), "and stands chosen");
        assert_eq!(list.focused_key(), None, "the keyboard stays where it was");

        shown.set(Some(1500));
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(list.model().index_of(&1500), None, "the model holds no such row");
        keys.borrow_mut().extend(1000..1600);
        list.notify_model_changed();
        let _ = lay_out(&mut list, 300.0);
        assert!(
            list.visible_range().contains(&1500),
            "shown once the model holds it"
        );
    }

    phase the_key_at_a_point_is_the_row_drawn_there;
    {
        let mut list = plain_rows(10);
        let _ = lay_out(&mut list, 300.0);
        let middle = |index: usize| {
            Point::new(
                list.rect.center().x,
                list.rect.y0
                    + list.viewport.item_view_top(index).unwrap()
                    + list.viewport.item_height(index).unwrap() / 2.0,
            )
        };
        let (one, four) = (middle(1), middle(4));
        assert_eq!(list.key_at_point(one), Some(1));
        assert_eq!(list.key_at_point(four), Some(4));
        assert_eq!(
            list.key_at_point(Point::new(list.rect.x0 - 1.0, one.y)),
            None,
            "outside the list is no row"
        );
    }

}

#[test]
fn a_reveal_a_click_asked_for_glides_and_the_wheel_takes_over() {
    use crate::time::testing::with_monotonic_time;
    use tilo_ui_base::tokens::MotionSource;
    let start = crate::time::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    let doc = AutoCommit::new();
    // Rows of about 40 px in a view 200 px tall: row 12 ends some 320
    // px down.
    let mut list = plain_rows(1000);
    let (taffy, node) = lay_out(&mut list, 300.0);
    with_monotonic_time(start, || {
        tilo_ui_base::motion::note_source(MotionSource::Pointer);
        assert!(list.reveal(&12));
    });
    assert_eq!(list.scroll_offset(), 0.0, "it starts where it is");
    assert_eq!(list.focused_key(), Some(&12), "the row is focused at once");
    with_monotonic_time(at(40), || {
        list.update(&doc, &automerge::ROOT);
    });
    let drawn = list.scroll_offset();
    assert!(drawn > 0.0 && drawn < 320.0, "on its way: {drawn}");
    assert_eq!(drawn.fract(), 0.0, "in whole pixels, which copy");
    with_monotonic_time(at(40), || {
        let wheel = WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::PixelDelta((0.0, 0.0).into()),
            phase: winit::event::TouchPhase::Moved,
        };
        dispatch(&mut list, &wheel, &taffy, node);
    });
    with_monotonic_time(at(400), || {
        list.update(&doc, &automerge::ROOT);
        assert!(
            !list.reveal_in_progress(at(400)),
            "nothing moves after the wheel"
        );
    });
    assert_eq!(list.scroll_offset(), drawn);

    with_monotonic_time(at(1000), || {
        tilo_ui_base::motion::note_source(MotionSource::Pointer);
        assert!(list.reveal(&20));
    });
    let bottom_of = |list: &VirtualList<VecListModel<u32, u32>>, index: usize| {
        list.viewport.item_top(index).unwrap() + list.viewport.item_height(index).unwrap()
            - list.viewport.viewport()
    };
    let target = bottom_of(&list, 20);
    with_monotonic_time(at(1400), || {
        list.update(&doc, &automerge::ROOT);
        assert!(!list.reveal_in_progress(at(1400)));
    });
    assert_eq!(list.scroll_offset(), target);

    with_monotonic_time(at(2000), || {
        tilo_ui_base::motion::note_source(MotionSource::Pointer);
        assert!(list.reveal(&900));
        assert_eq!(
            list.scroll_offset(),
            bottom_of(&list, 900),
            "no scrolling through"
        );
        assert!(list.scroll.to.opacity(at(2000)) < 0.05);
    });
    assert!(list.scroll.to.opacity(at(2400)) >= 1.0);
}

#[test]
fn a_rows_send_box_takes_the_typing_and_the_pointer() {
    crate::shared_selection::set_send_host_available(true);
    let mut list = plain_rows(10).with_context_menu(
        |_: &VecListModel<u32, u32>, _| vec![MenuItem::action("Open", || {})],
        |_, _| {},
    );
    right_click_row(&mut list, 3);
    let menu = list.menus.owned_mut().expect("the list owns its menu");
    menu.context_menu_mut().set_query("a note");
    let send_at = menu.context_menu().items().len() - 1;
    menu.context_menu_mut().click_item(send_at);
    assert!(
        menu.context_menu().is_open(),
        "the menu stays beside its box"
    );
    assert!(menu.context_menu().send_to().is_open(), "the box opened");
    let box_rect = list
        .menus
        .owned()
        .expect("the list owns its menu")
        .context_menu()
        .send_to()
        .rect();
    assert!(
        list.modal_capture()
            .is_some_and(|capture| capture.contains(box_rect.center())),
        "the box hears the pointer while it stands"
    );
    let (taffy, node) = lay_out(&mut list, 300.0);
    dispatch(
        &mut list,
        &tilo_ui_base::synthetic_input::char_key("!"),
        &taffy,
        node,
    );
    let text = list
        .menus
        .owned()
        .expect("the list owns its menu")
        .context_menu()
        .send_to()
        .text()
        .to_owned();
    assert_eq!(text, "a note!", "the box takes the typing");
    crate::shared_selection::set_send_host_available(false);
}

tilo_test_env::scenario! {
    fn a_rows_menu_opens_by_click_and_key_and_is_the_lists_one;

    phase a_second_click_activates_and_a_right_click_opens_the_rows_menu;
    {
        let activated = Rc::new(RefCell::new(Vec::new()));
        let menus = Rc::new(RefCell::new(Vec::new()));
        let (a, m) = (Rc::clone(&activated), Rc::clone(&menus));
        let mut list = plain_rows(10)
            .on_activate(move |key| a.borrow_mut().push(key))
            .with_context_menu(
                move |_: &VecListModel<u32, u32>, key| {
                    m.borrow_mut().push(key.copied());
                    vec![MenuItem::action("Open", || {})]
                },
                |_, _| {},
            );
        let (taffy, node) = lay_out(&mut list, 300.0);
        let middle = |list: &VirtualList<VecListModel<u32, u32>>, index: usize| {
            list.rect.y0
                + list.viewport.item_view_top(index).unwrap()
                + list.viewport.item_height(index).unwrap() / 2.0
        };
        let row_one = middle(&list, 1);
        let row_three = middle(&list, 3);
        let click = |list: &mut VirtualList<VecListModel<u32, u32>>| {
            dispatch(list, &cursor(row_one), &taffy, node);
            dispatch(list, &left(ElementState::Pressed), &taffy, node);
            dispatch(list, &left(ElementState::Released), &taffy, node);
        };
        click(&mut list);
        assert!(activated.borrow().is_empty(), "one click selects");
        assert!(list.selection().is_selected(&1));
        click(&mut list);
        assert_eq!(*activated.borrow(), [1], "the second click activates");
        dispatch(&mut list, &cursor(row_three), &taffy, node);
        assert!(dispatch(
            &mut list,
            &tilo_ui_base::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
            &taffy,
            node,
        ));
        let menus = menus.borrow();
        assert_eq!(menus.as_slice(), [Some(3)]);
        assert!(
            list.selection().is_selected(&3),
            "the row is selected first"
        );
        assert!(list.context_menu_is_open());
        assert!(
            list.modal_capture()
                .is_some_and(|menu| menu.contains(Point::new(20.0, row_three))),
            "the menu stands where the press was"
        );
    }

    phase the_menu_key_and_shift_f10_open_the_focused_rows_menu;
    {
        let menus = Rc::new(RefCell::new(Vec::new()));
        let m = Rc::clone(&menus);
        let mut list = plain_rows(10).with_context_menu(
            move |_: &VecListModel<u32, u32>, key| {
                m.borrow_mut().push(key.copied());
                vec![MenuItem::action("Open", || {})]
            },
            |_, _| {},
        );
        let (taffy, node) = lay_out(&mut list, 300.0);
        list.set_focus(true);
        assert!(list.reveal(&2));
        let send = |list: &mut VirtualList<VecListModel<u32, u32>>,
                    event: &WindowEvent,
                    modifiers: winit::keyboard::ModifiersState| {
            Dispatch::new()
                .modifiers(modifiers)
                .layout(&taffy, node)
                .send(list, event)
                .handled
        };
        let menu = tilo_ui_base::synthetic_input::named_key(NamedKey::ContextMenu);
        assert!(send(
            &mut list,
            &menu,
            winit::keyboard::ModifiersState::empty()
        ));
        assert_eq!(menus.borrow().len(), 1);
        assert_eq!(menus.borrow()[0], Some(2), "the row the keyboard is on");
        list.close_context_menu();
        let f10 = tilo_ui_base::synthetic_input::named_key(NamedKey::F10);
        assert!(send(
            &mut list,
            &f10,
            winit::keyboard::ModifiersState::SHIFT
        ));
        assert_eq!(menus.borrow().len(), 2, "Shift+F10, as a tree's rows");
        assert_eq!(menus.borrow()[1], Some(2));
        list.close_context_menu();
        assert!(
            !send(&mut list, &f10, winit::keyboard::ModifiersState::empty()),
            "F10 alone is no menu"
        );
        assert_eq!(menus.borrow().len(), 2);

        let mut plain = plain_rows(3);
        let (taffy, node) = lay_out(&mut plain, 300.0);
        plain.set_focus(true);
        assert!(plain.reveal(&0));
        assert!(
            !Dispatch::new()
                .layout(&taffy, node)
                .send(&mut plain, &menu)
                .handled
        );
    }

    phase a_reported_row_menu_hands_the_host_the_row_and_where_it_opens;
    {
        let reports: Rc<RefCell<Vec<(u32, Rect, Point)>>> = Rc::new(RefCell::new(Vec::new()));
        let r = Rc::clone(&reports);
        let mut list = plain_rows(10).with_row_menu_reporter(move |key, row, at| {
            r.borrow_mut().push((key, row, at));
        });
        let (taffy, node) = lay_out(&mut list, 300.0);
        let middle = |list: &VirtualList<VecListModel<u32, u32>>, index: usize| {
            list.rect.y0
                + list.viewport.item_view_top(index).unwrap()
                + list.viewport.item_height(index).unwrap() / 2.0
        };
        let row_three = middle(&list, 3);
        dispatch(&mut list, &cursor(row_three), &taffy, node);
        assert!(dispatch(
            &mut list,
            &tilo_ui_base::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
            &taffy,
            node,
        ));
        {
            let reports = reports.borrow();
            assert_eq!(reports.len(), 1, "one report for the press");
            let (key, row, at) = reports[0];
            assert_eq!(key, 3, "the row under the pointer");
            assert_eq!(Some(row), list.row_rect(&3), "its box, to anchor the menu");
            assert_eq!(at, Point::new(20.0, row_three), "where the press was");
            assert!(
                !list.selection().is_selected(&3),
                "the host's menu decides what it acts on"
            );
            assert!(!list.context_menu_is_open(), "no menu of the list's own");
        }

        list.set_focus(true);
        assert!(list.reveal(&5));
        let menu = tilo_ui_base::synthetic_input::named_key(NamedKey::ContextMenu);
        assert!(
            Dispatch::new()
                .layout(&taffy, node)
                .send(&mut list, &menu)
                .handled
        );
        let reports = reports.borrow();
        assert_eq!(reports.len(), 2);
        let (key, row, at) = reports[1];
        assert_eq!(key, 5, "the row the keyboard is on");
        assert_eq!(Some(row), list.row_rect(&5));
        assert_eq!(at, row.center(), "at the middle of the row");
    }


}

#[test]
fn a_right_click_opened_rows_menu_offers_to_send_after_the_release() {
    use crate::shared_selection::set_send_host_available;

    set_send_host_available(true);
    let mut list = plain_rows(10).with_context_menu(
        |_: &VecListModel<u32, u32>, _| vec![MenuItem::action("Open issue", || {})],
        |_, _| {},
    );
    right_click_row(&mut list, 3);
    assert!(list.context_menu_is_open(), "the menu stays open");
    let menu = list.menus.owned_mut().expect("the list owns its menu");
    assert!(
        !menu.context_menu().send_to().is_open(),
        "the menu stands alone until the row that sends is chosen"
    );
    let send_at = menu.context_menu().items().len() - 1;
    menu.context_menu_mut().click_item(send_at);
    let menu = menu.context_menu();
    assert!(menu.send_to().is_open(), "the box takes the menu's place");
    set_send_host_available(false);
}

#[test]
fn a_selection_delegate_receives_its_neighbours_and_list_rows_join() {
    let model = VecListModel::new((0..6_u32).collect(), |i| *i).unwrap();
    let mut list = VirtualList::with_selection_delegate(model, 40.0, |_, _, key, _| {
        Box::new(crate::widget::ListRow::new(
            format!("r{key}"),
            format!("Row {key}"),
        )) as Element
    })
    .multi_select(true)
    .with_viewport_height(400.0);
    let _ = lay_out(&mut list, 300.0);
    list.select_only(Some(1));
    list.selection_mut().toggle(ListKey::new(2u32));
    list.push_row_selection();
    let joins: Vec<tilo_ui_base::chrome::RowJoin> = list
        .mounted
        .iter_mut()
        .take(4)
        .map(|row| {
            row.widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<crate::widget::ListRow>())
                .unwrap()
                .join()
        })
        .collect();
    assert_eq!(joins[1], tilo_ui_base::chrome::RowJoin::new(false, true));
    assert_eq!(joins[2], tilo_ui_base::chrome::RowJoin::new(true, false));
    assert!(list.row_selection(0).below);
    assert!(list.row_selection(3).above);
}

/// A transcript shows the way back it owes its reader: to the first unread row while that is
/// above the view, and to what is being said now while it has nothing unread but is not at its
/// end. Taking it moves the reader, and where it moved them to changes what it offers next.
#[test]
fn a_transcript_offers_the_way_to_what_is_unread_or_to_what_is_now() {
    use crate::widget::conversation::Jump;
    let mut list = VirtualList::from_items(
        (0..300_u32).collect(),
        40.0,
        |item| *item,
        |_, _, _| Box::new(tilo_ui_base::widget::Spacer::fixed(40.0)) as Element,
    )
    .unwrap()
    .with_viewport_height(200.0)
    .following_end()
    .with_id("transcript")
    .unread_from(Some("250".to_owned()));
    list.set_key_presenting(|id: &str| id.parse::<u32>().ok());
    let (taffy, node) = lay_out(&mut list, 300.0);
    assert_eq!(list.jump(), Some(Jump::Unread));
    assert!(list.pill_rect().is_some(), "the pill has a rectangle");
    let _ = dispatch(
        &mut list,
        &tilo_ui_base::synthetic_input::named_key(NamedKey::Home),
        &taffy,
        node,
    );
    tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
    list.jump_to_unread();
    let _ = lay_out(&mut list, 300.0);
    assert!(
        list.visible_range().contains(&250),
        "the unread row shows: {:?}",
        list.visible_range()
    );
    assert_eq!(list.jump(), None, "nothing more to go to");

    let mut reading = VirtualList::from_items(
        (0..300_u32).collect(),
        40.0,
        |item| *item,
        |_, _, _| Box::new(tilo_ui_base::widget::Spacer::fixed(40.0)) as Element,
    )
    .unwrap()
    .with_viewport_height(200.0)
    .following_end();
    let _ = lay_out(&mut reading, 300.0);
    assert_eq!(reading.jump(), None, "at its end with nothing unread");
    let back = reading.scroll_offset() - 400.0;
    reading.set_scroll_offset(back);
    let _ = lay_out(&mut reading, 300.0);
    assert_eq!(reading.jump(), Some(Jump::Present));
    reading.jump_to_present();
    let _ = lay_out(&mut reading, 300.0);
    assert!(reading.is_at_end(), "it went back to the newest row");
    assert_eq!(reading.jump(), None);
}

/// Reading a list to its end tells its view once, and staying there tells nothing more: the
/// page such a list asks for at its end is asked for once per arrival there, never once a
/// frame and never by the frames a long list lays out while the person reads its last row.
#[test]
fn a_list_read_to_its_end_tells_once_until_the_person_reads_away_and_back() {
    let told = Rc::new(Cell::new(0_u32));
    let counting = Rc::clone(&told);
    let mut list = plain_rows(10_000);
    list.set_on_end_reached(Some(Box::new(move || {
        counting.set(counting.get().saturating_add(1));
    })));
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(told.get(), 0, "a list nobody scrolled tells nothing");
    let end = list.max_scroll_offset();
    assert!(end > 0.0, "ten thousand rows scroll");

    // The person reads to the last row: the view is told once.
    list.set_scroll_offset(end);
    assert_eq!(told.get(), 1, "the end is told as it is read");

    // Still at the end: steps of the wheel, frames of the list and its rows settling tell
    // nothing more. A page per frame would be a page a moment, not a page a reading.
    for _ in 0..5 {
        list.set_scroll_offset(end);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(told.get(), 1, "a list at its end tells once");
    }

    // A step around the boundary is still the one reading: the list is silent until the person
    // is more than the reach it told them from away from the end.
    list.set_scroll_offset(end - 150.0);
    list.set_scroll_offset(end);
    assert_eq!(told.get(), 1, "no second page for a step around the end");

    // A page arrives below the rows (older mail, one page more): the end moves on by more than
    // the person can read without going to it, and reading to that end is a new arrival.
    list.update_model_changes(
        &[ListModelChange::RowsInserted {
            index: 10_000,
            count: 100,
        }],
        |model| {
            for index in 10_000..10_100_u32 {
                model.push(index, index).unwrap();
            }
        },
    );
    let _ = lay_out(&mut list, 300.0);
    let next = list.max_scroll_offset();
    assert!(next > end, "the page stands after the rows");
    list.set_scroll_offset(end + 300.0);
    list.set_scroll_offset(next);
    assert_eq!(told.get(), 2, "the page after it is asked for once more");

    // Away from the end and back is the next reading of the same end.
    list.set_scroll_offset(next - 2_000.0);
    let _ = lay_out(&mut list, 300.0);
    list.set_scroll_offset(next);
    assert_eq!(told.get(), 3, "the next arrival at the end is told");
}

// USER REQUIREMENT: a finger held on a list row that reorders lifts it, and the finger then
// moves it in the list — the rail of a slide deck reorders under a held finger.
#[test]
fn a_finger_held_on_a_row_lifts_it_and_moves_it_in_the_list() {
    let mut list = VirtualList::from_items(
        vec![0_u64, 1, 2],
        20.0,
        |key| *key,
        |key, _, _| {
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        },
    )
    .unwrap()
    .with_viewport_height(60.0)
    .reorderable();
    let (taffy, node) = lay_out(&mut list, 200.0);
    let at = list.row_rect(&0).expect("the first row is placed").center();
    assert_eq!(
        list.touch_claim(at),
        tilo_ui_base::interaction::TouchClaim::HoldDrag,
        "a row of a list that reorders claims a held finger"
    );

    // The window turns a still finger's hold into a left press where it rests
    // (`interaction::TouchPointerEmulator`, whose own tests prove the mapping): the row is
    // lifted there, and the finger's way down the list carries it.
    let send = |list: &mut VirtualList<VecListModel<u64, u64>>, event: &WindowEvent| {
        Dispatch::new()
            .layout(&taffy, node)
            .send(list, event)
            .handled
    };
    send(
        &mut list,
        &tilo_ui_base::synthetic_input::cursor_moved(at.x, at.y),
    );
    send(&mut list, &left(ElementState::Pressed));
    send(
        &mut list,
        &tilo_ui_base::synthetic_input::cursor_moved(at.x, at.y + 45.0),
    );
    send(&mut list, &left(ElementState::Released));

    assert_eq!(
        list.model().items(),
        &[1, 2, 0],
        "the held row moved where the finger took it"
    );
}

#[test]
fn rows_the_person_moves_glide_and_a_sync_lands_at_once() {
    use tilo_ui_base::tokens::MotionSource;
    let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
    let mut list = VirtualList::new(model, 20.0, |_, _, key| {
        Box::new(ProbeRow::new(
            key.to_string(),
            20.0,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(100.0);
    let _ = lay_out(&mut list, 300.0);
    let motion = Theme::dark().motion();

    list.update_model_changes(
        &[ListModelChange::RowsInserted { index: 3, count: 1 }],
        |model| model.push(4, 4).unwrap(),
    );
    let _ = lay_out(&mut list, 300.0);
    list.start_row_motion(motion);
    assert!(list.row_motion.is_empty(), "a sync lands at once");

    tilo_ui_base::motion::note_source(MotionSource::Pointer);
    list.update_model_changes(
        &[ListModelChange::RowsMoved {
            from: 3,
            to: 0,
            count: 1,
        }],
        |model| assert!(model.move_item(3, 0)),
    );
    let _ = lay_out(&mut list, 300.0);
    list.start_row_motion(motion);
    let now = crate::time::now();
    let first = list
        .row_motion
        .get(&ListKey::new(1u64))
        .expect("row 1 moved down");
    assert!(
        first.offset(now) < 0.0,
        "drawn where it stood, above its new place"
    );
    assert!(list.needs_animation_frame());
    let later = now + Duration::from_millis(600);
    assert!(!list.row_motion.is_moving(later), "and lands");

    tilo_ui_base::motion::note_source(MotionSource::Keyboard);
    list.update_model_changes(
        &[ListModelChange::RowsMoved {
            from: 0,
            to: 3,
            count: 1,
        }],
        |model| assert!(model.move_item(0, 3)),
    );
    assert!(!list.row_motion.has_pending(), "a shortcut's result lands");
}

#[test]
fn a_selection_delegate_wrapped_in_its_swipe_takes_its_selection() {
    let model = VecListModel::new((0..3_u32).collect(), |i| *i).unwrap();
    let mut list = VirtualList::with_selection_delegate(model, 40.0, |_, _, key, _| {
        let row = crate::widget::ListRow::new(format!("r{key}"), format!("Row {key}"));
        Box::new(crate::widget::SwipeRow::new(Box::new(row), None, None)) as Element
    })
    .with_viewport_height(400.0);
    let _ = lay_out(&mut list, 300.0);
    list.select_only(Some(1));
    list.push_row_selection();
    let selected: Vec<bool> = list
        .mounted
        .iter_mut()
        .map(|row| {
            row.widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<crate::widget::SwipeRow>())
                .expect("the row's swipe")
                .content()
                .as_any()
                .and_then(|any| any.downcast_ref::<crate::widget::ListRow>())
                .expect("the row under its swipe")
                .is_selected()
        })
        .collect();
    assert_eq!(selected, [false, true, false], "only the chosen row is");
}

tilo_test_env::scenario! {
    fn a_press_after_a_fast_scroll_reaches_the_row_under_it;

    phase a_placement_that_placed_no_row_leaves_the_pointer_the_rows_that_mount;
    {
        // A scroll that outruns the rows a list mounted asks for a placement of
        // its neighbours: that placement finds no row near the box and places
        // none, so it must not narrow the pointer to nothing. The rows the frame
        // mounts stand under the pointer before they are placed, and a press
        // there is theirs.
        let keys = Rc::new(RefCell::new((0_u64..80).collect::<Vec<_>>()));
        let presses = Rc::new(Cell::new(0));
        let captured = Rc::clone(&presses);
        let mut list = VirtualList::new(
            SharedOrder {
                keys: Rc::clone(&keys),
            },
            20.0,
            move |_, index, _| {
                Box::new(ProbeRow::new(
                    format!("row {index}"),
                    20.0,
                    true,
                    Rc::clone(&captured),
                ))
            },
        )
        .with_viewport_height(60.0);
        let (taffy, node) = lay_out(&mut list, 200.0);
        assert!(!list.mounted.is_empty(), "the rows at the top show");
        assert!(list.mounted.len() < 80, "a list mounts only what it shows");

        list.set_scroll_offset(1200.0);
        let tree = tilo_ui_base::widget::layout::TreeId::of(&taffy);
        tilo_ui_base::widget::layout::placing_again_in(tree, || {
            list.apply_layout(&taffy, node, Point::ZERO);
        });

        let mut mounted = TaffyTree::new();
        let mut node_map = HashMap::new();
        let grown = list.build_layout(&mut mounted, &Theme::dark(), &mut node_map);
        assert!(!list.mounted.is_empty(), "the rows at the view show");
        dispatch(&mut list, &cursor(10.0), &mounted, grown);
        dispatch(&mut list, &left(ElementState::Pressed), &mounted, grown);
        assert_eq!(
            presses.get(),
            1,
            "the row the pointer stands on takes its press"
        );
    }
}
