//! A dropdown's open list: its measures, its touch choices, its keyboard and
//! typing, and the options a reader and an agent meet by name.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::Size;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion"
)]
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, transaction::Transactable};

fn opts() -> Vec<String> {
    ["UTC", "Europe/Amsterdam", "America/New_York", "Asia/Tokyo"]
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn list_of(dd: &mut Dropdown) -> Rect {
    dd.refresh_list_for_tests();
    dd.list_rect_for_tests()
}

tilo_test_env::scenario! {
    /// A dropdown's open list.
    /// Each phase was a test of its own and builds its own fixture.
    fn an_open_list_measures_places_and_captures;




    phase open_list_pointer_capture_focus_and_placement;
    {
        fn losing_focus_closes_the_list() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            assert!(dd.is_open());
            dd.set_focus(false);
            assert!(!dd.is_open(), "Tab-away dismisses the dropdown");
        }

        /// Hover is a repaint, not consumed input (the rule `Button` documents):
        /// the move that changes it is left for siblings, and leaving the window
        /// clears it because no `CursorMoved` will follow.
        fn hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover() {
            let mut dd = Dropdown::new(opts(), "tz");
            dd.set_rect_for_tests(Rect::new(0.0, 0.0, 200.0, 40.0));
            let (mut doc, root) = doc_with_tz("UTC");
            let enter = tilo_ui_base::synthetic_input::cursor_moved(20.0, 20.0);
            assert!(
                !feed_key(&mut dd, &enter, &mut doc, &root),
                "a hover change must not mark the move handled"
            );
            assert!(dd.is_hovered_for_tests(), "the pointer is over the control");
            let leave = WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            };
            feed_key(&mut dd, &leave, &mut doc, &root);
            assert!(!dd.is_hovered_for_tests(), "CursorLeft clears the resting hover");
        }

        fn open_list_flips_above_when_it_would_leave_the_viewport() {
            let mut dd = Dropdown::new(opts(), "tz");
            dd.set_rect_for_tests(Rect::new(0.0, 700.0, 200.0, 740.0));
            let below = list_of(&mut dd);
            assert!(
                below.y0 >= dd.rect_for_tests().y1,
                "without a viewport the list opens below"
            );
            let presence = tilo_ui_base::presence::PresenceCtx::default();
            presence
                .viewport
                .set(Some(Rect::new(0.0, 0.0, 800.0, 760.0)));
            let mut scene = Scene::new();
            dd.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut TextRenderer::new(), &Theme::dark(), presence.draw_env()));
            assert_eq!(dd.viewport_for_tests(), Some(Rect::new(0.0, 0.0, 800.0, 760.0)));
            let mut hosted =
                Dropdown::new(opts(), "tz").with_viewport(Rect::new(0.0, 0.0, 800.0, 760.0));
            hosted.set_rect_for_tests(dd.rect_for_tests());
            assert_eq!(list_of(&mut hosted), list_of(&mut dd));
            let flipped = list_of(&mut dd);
            assert!(
                flipped.y1 <= dd.rect_for_tests().y0,
                "the list opens above the control: {flipped:?} vs {:?}",
                dd.rect_for_tests()
            );
            assert!(flipped.y0 >= 0.0, "and stays inside the viewport");
            assert!((flipped.height() - below.height()).abs() < 1e-6);
            let row = f64::from(tilo_ui_base::tokens::Metrics::BASE.menu_row_height);
            assert_eq!(
                dd.list_for_tests().row_at(Point::new(10.0, flipped.y0 + row / 2.0)),
                Some(0),
                "the first row is hit where it is painted"
            );
            dd.set_viewport(Rect::new(0.0, 690.0, 800.0, 760.0));
            assert!((list_of(&mut dd).y0 - below.y0).abs() < 1e-6);
        }

        fn dropdown_in_a_row_yields_width_to_fixed_siblings() {
            use tilo_ui::widget::{Button, Row};
            let mut row = Row::new()
                .no_wrap()
                .push(Box::new(Button::new("left", "Left")))
                .push(Box::new(Dropdown::new(opts(), "tz")))
                .push(Box::new(Button::new("right", "Right")));
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = row.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(600.0),
                        height: taffy::AvailableSpace::Definite(80.0),
                    },
                )
                .unwrap();
            row.apply_layout(&taffy, node, Point::ZERO);
            let rects: Vec<Rect> = row.children().iter().map(|c| c.get_rect()).collect();
            assert_eq!(rects.len(), 3);
            let (left, dd, right) = (rects[0], rects[1], rects[2]);
            assert!(
                (left.width() - right.width()).abs() < 0.5 && left.width() > 0.0,
                "the fixed buttons keep their size: {left:?} / {right:?}"
            );
            assert!(
                dd.width() >= tilo_ui::widget::pad_x_for_tests() + tilo_ui::widget::chevron_width_for_tests() - 0.5,
                "the dropdown keeps at least its chevron slot: {dd:?}"
            );
            assert!(
                right.x1 <= 600.0 + 0.5,
                "the trailing button stays inside the 600 px row: {right:?}"
            );
            assert!(
                left.x1 <= dd.x0 + 0.5 && dd.x1 <= right.x0 + 0.5,
                "row order without overlap: {left:?} {dd:?} {right:?}"
            );
            assert!(
                (dd.height() - tilo_ui::widget::ControlSize::Regular.height(&theme)).abs() < 0.5,
                "the control is a Regular control: {dd:?}"
            );
        }

        fn a_caption_stacks_above_the_field_and_names_it() {
            let theme = Theme::dark();
            let mut dd = Dropdown::new(opts(), "level").caption("Level");
            let mut taffy = TaffyTree::new();
            let mut node_map = HashMap::new();
            let node = dd.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(300.0),
                        height: taffy::AvailableSpace::Definite(200.0),
                    },
                )
                .unwrap();
            dd.apply_layout(&taffy, node, Point::ZERO);
            let field = tilo_ui::widget::ControlSize::Regular.height(&theme);
            assert!((dd.rect_for_tests().height() - field).abs() < 0.5, "{:?}", dd.rect_for_tests());
            assert!(dd.rect_for_tests().y0 > dd.outer_for_tests().y0, "the caption sits above the field");
            assert_eq!(Widget::get_rect(&dd), dd.outer_for_tests());
            let node = Widget::accessibility(&dd).expect("dropdown semantics");
            assert_eq!(node.name.as_deref(), Some("Level"));
        }

        let _settled = tilo_ui_base::motion::testing::settled();
        losing_focus_closes_the_list();
        hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover();
        open_list_flips_above_when_it_would_leave_the_viewport();
        dropdown_in_a_row_yields_width_to_fixed_siblings();
        a_caption_stacks_above_the_field_and_names_it();
    }

}

fn zones() -> Vec<String> {
    let mut zones = opts();
    zones.extend(
        ["Europe/Berlin", "Europe/Oslo", "Europe/Zurich", "Etc/GMT+1"]
            .iter()
            .map(ToString::to_string),
    );
    zones
}

fn padded(first: &[&str]) -> Vec<String> {
    let mut options: Vec<String> = first.iter().map(ToString::to_string).collect();
    let mut n = 1;
    while options.len() < FILTER_MIN_OPTIONS {
        options.push(format!("Filler {n}"));
        n += 1;
    }
    options
}

tilo_test_env::scenario! {
    /// Choosing in a dropdown, its accessibility and its size.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_dropdown_selects_by_touch_keys_and_typing;

    phase dropdown_touch_choices_use_host_targets_and_select_stable_values;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            for scale in [1.0, 1.5] {
                let selected = tilo_ui_base::LocalState::new(String::new());
                let mut frame = Driver::new_with_theme(
                    tilo_ui::State::new(),
                    Box::new(Dropdown::from_state(zones(), &selected).with_id("language-picker")),
                    (420, 320),
                    theme.clone().with_ui_scale(scale),
                );
                let pointer_geometry = frame
                    .with_widget_mut::<Dropdown, _>(|dropdown| {
                        (dropdown.rect_for_tests().height(), dropdown.list_for_tests().row_height())
                    })
                    .unwrap();
                frame.tap_named("language-picker").unwrap();
                let point = frame
                    .with_widget_mut::<Dropdown, _>(|dropdown| {
                        assert!(dropdown.is_open());
                        let minimum = f64::from(tilo_ui_base::InteractionEnvironment::TOUCH.minimum_target());
                        assert!(
                            dropdown.rect_for_tests().height() >= minimum,
                            "the actual touch field must use the host minimum target"
                        );
                        assert!(
                            dropdown.list_for_tests().row_height() >= minimum,
                            "each actual painted and hit-tested choice must use the host touch target"
                        );
                        let row = dropdown
                            .list_for_tests()
                            .row_rect(1)
                            .expect("the second choice is in view");
                        assert!(row.height() >= minimum);
                        row.center()
                    })
                    .unwrap();
                frame.tap_at(point.x, point.y);
                frame
                    .with_widget_mut::<Dropdown, _>(|dropdown| {
                        assert!(!dropdown.is_open());
                        assert_eq!(dropdown.selected(), "Europe/Amsterdam");
                    })
                    .unwrap();
                assert_eq!(selected.get(), "Europe/Amsterdam");
                frame.set_interaction_environment(tilo_ui_base::InteractionEnvironment::default());
                frame
                    .with_widget_mut::<Dropdown, _>(|dropdown| {
                        assert_eq!(
                            (dropdown.rect_for_tests().height(), dropdown.list_for_tests().row_height()),
                            pointer_geometry,
                            "returning to a fine pointer restores the theme's compact geometry"
                        );
                        assert_eq!(dropdown.selected(), "Europe/Amsterdam");
                    })
                    .unwrap();
            }
        }
    }

    phase updating_placeholder_does_not_select_or_replace_a_private_value;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let selection = tilo_ui_base::LocalState::new(String::new());
        let mut dropdown = Dropdown::from_state(opts(), &selection);
        dropdown.set_placeholder("Waiting for tasks");
        assert_eq!(
            dropdown.accessibility().unwrap().name.as_deref(),
            Some("Waiting for tasks")
        );
        assert!(selection.with(String::is_empty));
        dropdown.set_selected("UTC");
        dropdown.set_placeholder("Choose a task");
        assert_eq!(dropdown.selected(), "UTC");
        assert_eq!(selection.with(Clone::clone), "UTC");
    }

    phase dropdown_keyboard_opens_navigates_selects_and_escapes;
    {
        fn starts_closed_and_reads_value_on_sync() {
            let mut dd = Dropdown::new(opts(), "tz");
            assert!(!dd.is_open());
            let (doc, root) = doc_with_tz("Asia/Tokyo");
            dd.sync(&doc, &root);
            assert_eq!(dd.selected(), "Asia/Tokyo");
        }

        fn down_opens_when_closed_and_focused() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            assert!(feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root
            ));
            assert!(dd.is_open());
            assert_eq!(dd.highlight_index(), 0);
        }

        fn arrows_move_highlight_while_open() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.highlight_index(), 2);
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowUp), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.highlight_index(), 1);
        }

        fn enter_emits_selection_action_and_closes() {
            let mut dd =
                Dropdown::new(opts(), "tz").on_action(tilo_ui_base::widget::test_string_action_binding());
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                &mut dd,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            assert!(feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Enter), None),
                &mut doc,
                &root
            ));
            assert!(!dd.is_open(), "selecting closes the list");
            assert_eq!(
                tilo_ui_base::widget::controls::take_control_invocation(&mut dd)
                    .unwrap()
                    .arguments["value"],
                "America/New_York"
            );
            assert_eq!(
                tilo_ui_base::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
                Some("UTC"),
                "selecting submits an action and preserves the read model"
            );
            // The chooser shows what the document holds, never the choice
            // before it lands: still UTC, then New York once it is stored.
            assert_eq!(dd.selected(), "UTC");
            let (doc, root) = doc_with_tz("America/New_York");
            dd.sync(&doc, &root);
            assert_eq!(dd.selected(), "America/New_York");
        }

        fn escape_closes_without_changing_value() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            assert!(feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Escape), None),
                &mut doc,
                &root
            ));
            assert!(!dd.is_open());
            assert_eq!(dd.selected(), "UTC", "Esc must not change the value");
            assert_eq!(
                tilo_ui_base::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
                Some("UTC")
            );
        }

        let _settled = tilo_ui_base::motion::testing::settled();
        starts_closed_and_reads_value_on_sync();
        down_opens_when_closed_and_focused();
        arrows_move_highlight_while_open();
        enter_emits_selection_action_and_closes();
        escape_closes_without_changing_value();
    }

    phase combobox_filter_editing_handles_case_spaces_graphemes_and_ime;
    {
        fn typing_filters_options_case_insensitively() {
            let mut dd = Dropdown::new(zones(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            assert_eq!(dd.visible_count(), 8);
            // "ame" matches only "America/New_York".
            feed_key(
                &mut dd,
                &key_press(Key::Character("a".into()), Some("a")),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Character("m".into()), Some("m")),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Character("e".into()), Some("e")),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), "ame");
            assert_eq!(dd.visible_count(), 1);
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Enter), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.selected(), "America/New_York");
        }

        fn space_is_part_of_an_open_combobox_query() {
            let mut dd = Dropdown::new(padded(&["New York", "New Delhi"]), "city");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            dd.set_focus(true);
            dd.do_open_for_tests();
            for (key_value, text) in [
                (Key::Character("N".into()), Some("N")),
                (Key::Character("e".into()), Some("e")),
                (Key::Character("w".into()), Some("w")),
                (Key::Named(NamedKey::Space), None),
                (Key::Character("Y".into()), Some("Y")),
            ] {
                feed_key(&mut dd, &key_press(key_value, text), &mut doc, &root);
            }
            assert_eq!(dd.filter_query(), "New Y");
            assert_eq!(dd.visible_count(), 1);
        }

        fn backspace_edits_the_filter() {
            let mut dd = Dropdown::new(zones(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            feed_key(
                &mut dd,
                &key_press(Key::Character("a".into()), Some("a")),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Character("z".into()), Some("z")),
                &mut doc,
                &root,
            );
            assert_eq!(dd.visible_count(), 0); // "az" matches nothing
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Backspace), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), "a");
            assert_eq!(dd.visible_count(), 3);
        }

        fn backspace_deletes_one_extended_grapheme_from_the_filter() {
            let mut dd = Dropdown::new(zones(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            let cluster = "👩🏽‍💻";
            feed_key(
                &mut dd,
                &key_press(Key::Character(cluster.into()), Some(cluster)),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), cluster);
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Backspace), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), "");
            assert_eq!(dd.filter_selection_for_tests(), Selection::collapsed(0));
        }

        fn typing_replaces_the_selected_filter_range() {
            let mut dd = Dropdown::new(zones(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            dd.set_filter_for_tests("Europe");
            dd.set_filter_selection_for_tests(Selection {
                anchor: 0,
                cursor: dd.filter_for_tests().len(),
            });
            feed_key(
                &mut dd,
                &key_press(Key::Character("亜".into()), Some("亜")),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), "亜");
            assert_eq!(dd.filter_selection_for_tests(), Selection::collapsed("亜".len()));
        }

        fn ime_preedit_is_visible_but_filters_only_after_commit() {
            let mut dd = Dropdown::new(padded(&["東京", "London"]), "city");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            dd.apply_layout_external(Rect::new(10.0, 10.0, 260.0, 50.0));
            dd.set_focus(true);
            dd.do_open_for_tests();

            assert!(feed_key(
                &mut dd,
                &WindowEvent::Ime(winit::event::Ime::Preedit(
                    "東京".into(),
                    Some(("東".len(), "東".len())),
                )),
                &mut doc,
                &root,
            ));
            assert_eq!(dd.filter_query(), "");
            assert_eq!(dd.visible_count(), 8);
            assert_eq!(
                Widget::accessibility(&dd).unwrap().value,
                None,
                "preedit must stay out of the accessibility value"
            );

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            Widget::draw(&mut dd, &mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark(), tilo_ui_base::presence::PresenceCtx::empty().draw_env()));
            let candidate = Widget::ime_cursor_rect(&dd).expect("open combobox publishes its caret");
            assert!(dd.get_rect().contains(candidate.center()));

            assert!(feed_key(
                &mut dd,
                &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
                &mut doc,
                &root,
            ));
            assert_eq!(dd.filter_query(), "東京");
            assert_eq!(dd.visible_count(), 1);
            assert_eq!(
                Widget::accessibility(&dd).unwrap().value.as_deref(),
                Some("東京")
            );
        }

        let _settled = tilo_ui_base::motion::testing::settled();
        typing_filters_options_case_insensitively();
        space_is_part_of_an_open_combobox_query();
        backspace_edits_the_filter();
        backspace_deletes_one_extended_grapheme_from_the_filter();
        typing_replaces_the_selected_filter_range();
        ime_preedit_is_visible_but_filters_only_after_commit();
    }

    phase committed_selection_binds_process_local_state;
    {
        fn local_state_binding_is_two_way_across_dropdowns() {
            let state = tilo_ui_base::LocalState::new(String::from("UTC"));
            let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let captured = std::rc::Rc::clone(&changes);
            let mut first = Dropdown::from_state(opts(), &state)
                .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
            let mut second = Dropdown::from_state(opts(), &state);
            let mut document = AutoCommit::new();

            state.set(String::from("Asia/Tokyo"));
            Widget::sync(&mut first, &document, &automerge::ROOT);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(first.selected(), "Asia/Tokyo");
            assert_eq!(second.selected(), "Asia/Tokyo");
            assert!(changes.borrow().is_empty(), "model sync is not user intent");

            assert!(Widget::perform_action(
                &mut first,
                &tilo_ui_base::a11y::Action::SetValue(String::from("Europe/Amsterdam")),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert_eq!(state.get(), "Europe/Amsterdam");
            assert_eq!(&*changes.borrow(), &["Europe/Amsterdam"]);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(second.selected(), "Europe/Amsterdam");

            second.set_selected("America/New_York");
            assert_eq!(state.get(), "America/New_York");
            Widget::sync(&mut first, &document, &automerge::ROOT);
            assert_eq!(first.selected(), "America/New_York");
            assert_eq!(&*changes.borrow(), &["Europe/Amsterdam"]);
        }

        let _settled = tilo_ui_base::motion::testing::settled();
        local_state_binding_is_two_way_across_dropdowns();
    }

    phase a_menu_button_reports_a_choice_without_keeping_it;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let state = tilo_ui_base::LocalState::new(String::new());
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&chosen);
        let mut menu = Dropdown::from_state(opts(), &state)
            .as_menu(tilo_ui_base::icons::Icon::Plus)
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
        menu.set_rect_for_tests(Rect::new(560.0, 100.0, 592.0, 132.0));
        let mut document = AutoCommit::new();
        let root = automerge::ROOT;

        let list = list_of(&mut menu);
        assert!((list.x1 - menu.rect_for_tests().x1).abs() < 1e-6, "{list:?}");
        assert!(list.width() >= tilo_ui::widget::menu_min_width_for_tests());
        menu.set_viewport(Rect::new(0.0, 0.0, 400.0, 800.0));
        let kept = list_of(&mut menu);
        assert!(
            kept.x1 <= 400.0 && kept.x0 >= 0.0,
            "kept in the window: {kept:?}"
        );

        menu.set_focus(true);
        assert!(feed_key(
            &mut menu,
            &key_press(Key::Named(NamedKey::Enter), None),
            &mut document,
            &root
        ));
        assert!(menu.is_open());
        feed_key(
            &mut menu,
            &key_press(Key::Character("a".into()), Some("a")),
            &mut document,
            &root,
        );
        assert_eq!(menu.visible_count(), opts().len());
        feed_key(
            &mut menu,
            &key_press(Key::Named(NamedKey::ArrowDown), None),
            &mut document,
            &root,
        );
        feed_key(
            &mut menu,
            &key_press(Key::Named(NamedKey::Enter), None),
            &mut document,
            &root,
        );

        assert_eq!(&*chosen.borrow(), &["Europe/Amsterdam"]);
        assert!(!menu.is_open());
        assert_eq!(menu.selected(), "", "a command is not kept as a value");
        assert_eq!(state.get(), "");
        let node = menu.accessibility().expect("a menu button is announced");
        assert_eq!(node.role, tilo_ui_base::a11y::Role::Button);
    }

    phase dropdown_accessibility_semantics_and_disabled_state;
    {
        fn semantic_actions_expand_collapse_and_reject_unknown_values() {
            use tilo_ui_base::a11y::Action;

            let mut dropdown = Dropdown::new(opts(), "tz");
            let (mut document, root) = doc_with_tz("UTC");
            Widget::sync(&mut dropdown, &document, &root);

            assert!(Widget::perform_action(
                &mut dropdown,
                &Action::Expand,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut document),
                &root,
            ));
            assert!(dropdown.is_open());
            assert!(Widget::perform_action(
                &mut dropdown,
                &Action::Collapse,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut document),
                &root,
            ));
            assert!(!dropdown.is_open());
            assert!(!Widget::perform_action(
                &mut dropdown,
                &Action::SetValue(String::from("not an option")),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut document),
                &root,
            ));
            assert_eq!(dropdown.selected(), "UTC");
            assert_eq!(
                tilo_ui_base::doc::read_str_or_text(&document, &root, "tz").as_deref(),
                Some("UTC")
            );
        }

        fn disabled_dropdown_does_not_open() {
            let mut dd = Dropdown::new(opts(), "tz").with_enabled(false);
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            let handled = feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            assert!(!dd.is_open(), "disabled dropdown must not open");
            assert!(!handled, "disabled dropdown must not swallow input");
            assert_eq!(
                dd.selected(),
                "UTC",
                "disabled dropdown must not change the value"
            );
        }

        fn accessibility_reports_combobox_role_value_and_collapsed() {
            use tilo_ui_base::a11y::Role;
            let mut dd = Dropdown::new(opts(), "tz");
            let (doc, root) = doc_with_tz("Asia/Tokyo");
            dd.sync(&doc, &root);
            let node = Widget::accessibility(&dd).expect("dropdown is not presentational");
            assert_eq!(node.role, Role::ComboBox);
            assert_eq!(node.name.as_deref(), Some("Asia/Tokyo"));
            assert_eq!(node.value.as_deref(), Some("Asia/Tokyo"));
            assert_eq!(
                node.state.expanded,
                Some(false),
                "closed dropdown is collapsed"
            );
            assert!(node.state.focusable);
            assert!(!node.state.disabled);
        }

        fn accessibility_unselected_uses_placeholder_name_and_no_value() {
            use tilo_ui_base::a11y::Role;
            let dd = Dropdown::new(opts(), "tz").with_placeholder("Pick a zone");
            let node = Widget::accessibility(&dd).unwrap();
            assert_eq!(node.role, Role::ComboBox);
            assert_eq!(
                node.name.as_deref(),
                Some("Pick a zone"),
                "placeholder is the name when unset"
            );
            assert_eq!(node.value, None, "no value until something is selected");
        }

        fn accessibility_reflects_open_and_disabled() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open_for_tests();
            let node = Widget::accessibility(&dd).unwrap();
            assert_eq!(node.state.expanded, Some(true), "open dropdown is expanded");
            assert!(node.state.focused);

            let dd2 = Dropdown::new(opts(), "tz").with_enabled(false);
            let node2 = Widget::accessibility(&dd2).unwrap();
            assert!(node2.state.disabled);
        }

        fn action_binding_owns_accessibility_selection() {
            let mut dropdown = Dropdown::new(opts(), "tz")
                .on_action(tilo_ui_base::widget::test_string_action_binding());
            let mut doc = AutoCommit::new();
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                &mut dropdown,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&dropdown));
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                &mut dropdown,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            assert!(Widget::is_enabled(&dropdown));
            assert!(Widget::perform_action(
                &mut dropdown,
                &tilo_ui_base::a11y::Action::SetValue("Asia/Tokyo".into()),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(doc.get(&automerge::ROOT, "tz").unwrap().is_none());
            assert_eq!(
                tilo_ui_base::widget::controls::take_control_invocation(&mut dropdown)
                    .unwrap()
                    .arguments["value"],
                "Asia/Tokyo"
            );
        }

        let _settled = tilo_ui_base::motion::testing::settled();
        semantic_actions_expand_collapse_and_reject_unknown_values();
        disabled_dropdown_does_not_open();
        accessibility_reports_combobox_role_value_and_collapsed();
        accessibility_unselected_uses_placeholder_name_and_no_value();
        accessibility_reflects_open_and_disabled();
        action_binding_owns_accessibility_selection();
    }


}

#[test]
fn dropdown_language_popup_stays_visible_in_a_short_scaled_window() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut frame = Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(
            Dropdown::from_state(zones(), &tilo_ui_base::LocalState::new(String::new()))
                .with_id("language-picker"),
        ),
        (420, 180),
        Theme::light().with_ui_scale(1.5),
    );
    frame.click_named("language-picker").unwrap();
    frame.render_png(
        &std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("dropdown-review/shared-short.png"),
    );
    let popup = frame
        .with_widget_mut::<Dropdown, _>(|dropdown| {
            assert!(dropdown.is_open());
            dropdown.list_rect_for_tests()
        })
        .unwrap();
    assert!(
        popup.y0 >= 0.0 && popup.y1 <= 180.0,
        "the actual painted and hit-tested popup must fit its viewport: {popup:?}"
    );
}

/// An open dropdown's options are controls a screen reader and an agent meet
/// by name, where they stand: the list the person presses is the list the
/// window publishes.
#[test]
fn an_open_languages_options_are_controls_by_name_that_choose() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let chosen = tilo_ui_base::LocalState::new(String::new());
    let picker = Dropdown::from_state(
        vec![
            "Device".to_string(),
            "English".to_string(),
            "Deutsch".to_string(),
        ],
        &chosen,
    )
    .with_accessible_name("Language")
    .with_id("language-picker");
    let mut frame = Driver::new(tilo_ui::State::new(), Box::new(picker), (480, 260));

    let opener = frame
        .a11y_node_named("Language")
        .expect("the chooser is announced");
    frame
        .perform_a11y_action(opener, &tilo_ui_base::a11y::Action::Expand)
        .expect("the chooser opens");
    frame.relayout();

    let controls = frame.controls();
    let option = controls
        .iter()
        .find(|control| control.name.as_deref() == Some("Deutsch"))
        .unwrap_or_else(|| {
            let names: Vec<_> = controls.iter().filter_map(|c| c.name.clone()).collect();
            panic!("the open list publishes its options by name: {names:?}")
        });
    let bounds = option
        .bounds
        .expect("the option stands where the list draws it");
    assert!(bounds.width() > 0.0 && bounds.height() > 0.0);

    frame
        .operate_control(&option.id, &tilo_ui::headless::ControlInput::Press)
        .expect("the option is pressed by the name it shows");
    assert_eq!(chosen.get(), "Deutsch");
}

tilo_test_env::scenario! {
    /// The language dropdown.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_language_dropdown_scrolls_searches_and_selects_stable_ids;

    phase dropdown_language_shared_scrollbar_and_search_field_are_interactive;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut frame = Driver::new_with_theme(
            tilo_ui::State::new(),
            Box::new(
                Dropdown::from_state(zones(), &tilo_ui_base::LocalState::new(String::new()))
                    .with_id("language-picker"),
            ),
            (420, 180),
            Theme::dark().with_ui_scale(1.5),
        );
        frame.click_named("language-picker").unwrap();
        let (thumb, track) = frame
            .with_widget_mut::<Dropdown, _>(|dropdown| dropdown.list_for_tests().scrollbar_thumb_for_tests().unwrap())
            .unwrap();
        frame.drag_at(
            (thumb.center().x, thumb.center().y),
            (track.center().x, track.y1),
        );
        frame
            .with_widget_mut::<Dropdown, _>(|dropdown| {
                assert!(
                    dropdown.list_for_tests().scroll_offset() > 0.0,
                    "the actual shared scrollbar must scroll the list"
                );
                assert!(!dropdown.list_for_tests().is_dragging());
                assert!(dropdown.is_open(), "dragging must not select an option");
            })
            .unwrap();
        frame.type_text("Europe");
        let query = frame
            .with_widget_mut::<Dropdown, _>(|dropdown| dropdown.query_rect_for_tests())
            .unwrap();
        frame.click_at(query.x0 + 1.0, query.center().y);
        frame.type_text("X");
        let mut text = TextRenderer::new();
        let tokens = &frame.theme().tokens;
        let insets = 2.0 * f64::from(tokens.spacing.s2 + tokens.spacing.s3);
        let hint_width = f64::from(
            text.measure_style(
                &widget_text("dropdown-no-matches"),
                None,
                &frame.theme().text(TextRole::Body),
            )
            .0,
        );
        frame
            .with_widget_mut::<Dropdown, _>(|dropdown| {
                assert!(
                    dropdown.is_open(),
                    "the painted search field remains editable when clicked"
                );
                assert_eq!(
                    dropdown.filter_for_tests(), "XEurope",
                    "pointer caret uses the shaped query layout"
                );
                assert_eq!(dropdown.visible_count(), 0);
                assert!(
                    dropdown.list_rect_for_tests().width() - insets >= hint_width,
                    "the localized empty-result text must fit its actual painted clip"
                );
            })
            .unwrap();
    }

    phase dropdown_language_accessibility_selects_stable_ids_with_duplicate_labels;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let state = tilo_ui_base::LocalState::new(String::new());
        let mut dropdown = Dropdown::from_state(Vec::new(), &state);
        dropdown.set_value_options(vec![
            ("first".into(), "Same label".into()),
            ("second".into(), "Same label".into()),
        ]);
        let (mut document, root) = doc_with_tz("");
        assert!(Widget::perform_action(
            &mut dropdown,
            &tilo_ui_base::a11y::Action::SetValue("second".into()),
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut document),
            &root
        ));
        assert_eq!(state.get(), "second");
        assert!(!Widget::perform_action(
            &mut dropdown,
            &tilo_ui_base::a11y::Action::SetValue("foreign".into()),
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut document),
            &root
        ));
        assert_eq!(state.get(), "second");
    }

    phase dropdown_language_empty_stable_value_keeps_its_human_label;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let state = tilo_ui_base::LocalState::new(String::new());
        let mut dropdown = Dropdown::from_state(Vec::new(), &state);
        dropdown.set_value_options(vec![
            (String::new(), "Session language".into()),
            ("de_DE".into(), "German".into()),
        ]);
        let mut frame =
            Driver::new(tilo_ui::State::new(), Box::new(dropdown), (420, 180));
        frame
            .with_widget_mut::<Dropdown, _>(|dropdown| {
                assert!(dropdown.has_selected_value_for_tests());
                assert_eq!(dropdown.selected_label_for_tests(), "Session language");
                let node = Widget::accessibility(dropdown).unwrap();
                assert_eq!(node.value.as_deref(), Some("Session language"));
            })
            .unwrap();
    }

    phase dropdown_language_revoked_action_releases_modal_capture;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut dropdown =
            Dropdown::new(opts(), "tz").on_action(tilo_ui_base::widget::test_string_action_binding());
        let allowed =
            std::collections::HashSet::from([tilo_actions::ActionAddress::new("test:control", "set")]);
        tilo_ui_base::widget::controls::refresh_control_action_availability(&mut dropdown, &allowed);
        let mut doc = AutoCommit::new();
        assert!(Widget::perform_action(
            &mut dropdown,
            &tilo_ui_base::a11y::Action::Expand,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(Widget::modal_capture(&dropdown).is_some());
        tilo_ui_base::widget::controls::refresh_control_action_availability(
            &mut dropdown,
            &std::collections::HashSet::new(),
        );
        assert!(!Widget::is_enabled(&dropdown));
        assert!(!dropdown.is_open());
        assert!(Widget::modal_capture(&dropdown).is_none());
        assert!(tilo_ui_base::widget::controls::take_control_invocation(&mut dropdown).is_none());

        // Unavailable capture-only fixtures retain their authored open state.
        let mut capture = Dropdown::new(opts(), "tz")
            .on_action(tilo_ui_base::widget::test_string_action_binding())
            .open();
        tilo_ui_base::widget::controls::refresh_control_action_availability(
            &mut capture,
            &std::collections::HashSet::new(),
        );
        assert!(capture.is_open());
    }
}

fn doc_with_tz(value: &str) -> (AutoCommit, ObjId) {
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    let obj = doc.put_object(&root, "tz", ObjType::Text).unwrap();
    let _ = doc.splice_text(&obj, 0, 0, value);
    (doc, root)
}

fn feed_key(dd: &mut Dropdown, ev: &WindowEvent, doc: &mut AutoCommit, root: &ObjId) -> bool {
    Dispatch::new().doc(doc).path(root).send(dd, ev).handled
}

/// In a short list, typing jumps to the option it spells instead of filtering, and the value
/// stays while the list is open.
#[test]
fn a_short_list_jumps_instead_of_filtering() {
    let mut dd = Dropdown::new(opts(), "tz");
    let (mut doc, root) = doc_with_tz("UTC");
    dd.sync(&doc, &root);
    dd.set_focus(true);
    dd.do_open_for_tests();
    assert_eq!(dd.highlight_index(), 0);
    assert!(feed_key(
        &mut dd,
        &key_press(Key::Character("a".into()), Some("a")),
        &mut doc,
        &root,
    ));
    assert_eq!(dd.filter_query(), "", "a short list never filters");
    assert_eq!(dd.visible_count(), 4);
    assert_eq!(dd.highlight_index(), 2, "the next option starting with a");
    feed_key(
        &mut dd,
        &key_press(Key::Character("a".into()), Some("a")),
        &mut doc,
        &root,
    );
    assert_eq!(dd.highlight_index(), 3, "typing again moves on");
    dd.close();
    dd.do_open_for_tests();
    assert_eq!(dd.highlight_index(), 0);
    for letter in ["a", "s"] {
        feed_key(
            &mut dd,
            &key_press(Key::Character(letter.into()), Some(letter)),
            &mut doc,
            &root,
        );
    }
    assert_eq!(dd.highlight_index(), 3, "\"as\" spells Asia");
    feed_key(
        &mut dd,
        &key_press(Key::Character("x".into()), Some("x")),
        &mut doc,
        &root,
    );
    assert_eq!(
        dd.highlight_index(),
        3,
        "a name no option begins leaves the highlight where it is"
    );
    let node = Widget::accessibility(&dd).expect("semantics");
    assert_eq!(
        node.value.as_deref(),
        Some("UTC"),
        "the value stays while the list is open"
    );
}
