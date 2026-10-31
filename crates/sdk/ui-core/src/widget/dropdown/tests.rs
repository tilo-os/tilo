use super::*;
use crate::testing::Dispatch;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, transaction::Transactable};

fn opts() -> Vec<String> {
    ["UTC", "Europe/Amsterdam", "America/New_York", "Asia/Tokyo"]
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn paint_list(dd: &mut Dropdown, renderer: &mut TextRenderer, theme: &Theme) -> f64 {
    dd.draw_overlay(
        &mut Scene::new(),
        Affine::IDENTITY,
        renderer,
        theme,
        &PresenceCtx::empty(),
    );
    dd.list_rect().width()
}

/// The open list's panel for the dropdown as it stands.
fn list_of(dd: &mut Dropdown) -> Rect {
    dd.refresh_list();
    dd.list_rect()
}

#[test]
fn open_list_repaints_without_remeasuring_every_option() {
    let options: Vec<String> = (0..512).map(|n| format!("Language {n:04}")).collect();
    let mut dd = Dropdown::new(options, "language");
    dd.rect = Rect::new(0.0, 0.0, 300.0, CONTROL_HEIGHT);
    dd.do_open();
    let mut renderer = TextRenderer::new();
    let theme = Theme::dark();
    paint_list(&mut dd, &mut renderer, &theme);
    let (hits, misses) = renderer.cache_stats();
    dd.list.set_highlight(Some(1));
    paint_list(&mut dd, &mut renderer, &theme);
    let (hits_after, misses_after) = renderer.cache_stats();
    assert_eq!(misses_after, misses, "a repaint shapes nothing new");
    assert!(
        hits_after - hits < 128,
        "a repaint looks up the visible rows, not all 512 options: {}",
        hits_after - hits
    );
}

#[test]
fn open_list_width_follows_options_text_size_and_fonts() {
    let mut dd = Dropdown::new(vec!["Small".into(), "Medium".into()], "value").open();
    dd.rect = Rect::new(0.0, 0.0, 300.0, CONTROL_HEIGHT);
    let mut renderer = TextRenderer::new();
    let theme = Theme::dark();
    let initial = paint_list(&mut dd, &mut renderer, &theme);
    dd.set_value_options(vec![(
        "one".into(),
        "A much wider replacement label for the only choice".into(),
    )]);
    let wider = paint_list(&mut dd, &mut renderer, &theme);
    assert!(wider > initial, "new labels set the width");
    let larger = Theme::dark().with_text_scale(2.0);
    let scaled = paint_list(&mut dd, &mut renderer, &larger);
    assert!(scaled > wider, "the theme's text size sets the width");
    let mut fresh = Dropdown::new(dd.options.clone(), "value").open();
    assert!((paint_list(&mut fresh, &mut TextRenderer::new(), &larger) - scaled).abs() < 0.5);
    renderer.bump_generation();
    let misses = renderer.cache_stats().1;
    paint_list(&mut dd, &mut renderer, &theme);
    assert!(
        renderer.cache_stats().1 > misses,
        "a font change remeasures the options"
    );
    dd.set_options(vec!["Small".into(), "Medium".into()]);
    assert!((paint_list(&mut dd, &mut renderer, &theme) - initial).abs() < 0.5);
}

/// A list long enough to filter: `opts` and four zones with no `a`.
fn zones() -> Vec<String> {
    let mut zones = opts();
    zones.extend(
        ["Europe/Berlin", "Europe/Oslo", "Europe/Zurich", "Etc/GMT+1"]
            .iter()
            .map(ToString::to_string),
    );
    zones
}

/// `first`, then numbered fillers up to a list that filters.
fn padded(first: &[&str]) -> Vec<String> {
    let mut options: Vec<String> = first.iter().map(ToString::to_string).collect();
    let mut n = 1;
    while options.len() < FILTER_MIN_OPTIONS {
        options.push(format!("Filler {n}"));
        n += 1;
    }
    options
}

#[test]
fn dropdown_touch_choices_use_host_targets_and_select_stable_values() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        for scale in [1.0, 1.5] {
            let selected = crate::LocalState::new(String::new());
            let mut frame = crate::headless::FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(Dropdown::from_state(zones(), &selected).with_id("language-picker")),
                (420, 320),
                theme.clone().with_ui_scale(scale),
            );
            let pointer_geometry = frame
                .with_widget_mut::<Dropdown, _>(|dropdown| {
                    (dropdown.rect.height(), dropdown.list.row_height())
                })
                .unwrap();
            frame.tap_named("language-picker").unwrap();
            let point = frame
                .with_widget_mut::<Dropdown, _>(|dropdown| {
                    assert!(dropdown.is_open());
                    let minimum = f64::from(crate::InteractionEnvironment::TOUCH.minimum_target());
                    assert!(
                        dropdown.rect.height() >= minimum,
                        "the actual touch field must use the host minimum target"
                    );
                    assert!(
                        dropdown.list.row_height() >= minimum,
                        "each actual painted and hit-tested choice must use the host touch target"
                    );
                    let row = dropdown
                        .list
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
            frame.set_interaction_environment(crate::InteractionEnvironment::default());
            frame
                .with_widget_mut::<Dropdown, _>(|dropdown| {
                    assert_eq!(
                        (dropdown.rect.height(), dropdown.list.row_height()),
                        pointer_geometry,
                        "returning to a fine pointer restores the theme's compact geometry"
                    );
                    assert_eq!(dropdown.selected(), "Europe/Amsterdam");
                })
                .unwrap();
        }
    }
}

#[test]
fn dropdown_language_popup_stays_visible_in_a_short_scaled_window() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut frame = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(
            Dropdown::from_state(zones(), &crate::LocalState::new(String::new()))
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
            dropdown.list_rect()
        })
        .unwrap();
    assert!(
        popup.y0 >= 0.0 && popup.y1 <= 180.0,
        "the actual painted and hit-tested popup must fit its viewport: {popup:?}"
    );
}

#[test]
fn dropdown_language_shared_scrollbar_and_search_field_are_interactive() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut frame = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(
            Dropdown::from_state(zones(), &crate::LocalState::new(String::new()))
                .with_id("language-picker"),
        ),
        (420, 180),
        Theme::dark().with_ui_scale(1.5),
    );
    frame.click_named("language-picker").unwrap();
    let (thumb, track) = frame
        .with_widget_mut::<Dropdown, _>(|dropdown| dropdown.list.scrollbar_thumb().unwrap())
        .unwrap();
    frame.drag_at(
        (thumb.center().x, thumb.center().y),
        (track.center().x, track.y1),
    );
    frame
        .with_widget_mut::<Dropdown, _>(|dropdown| {
            assert!(
                dropdown.list.scroll_offset() > 0.0,
                "the actual shared scrollbar must scroll the list"
            );
            assert!(!dropdown.list.is_dragging());
            assert!(dropdown.is_open(), "dragging must not select an option");
        })
        .unwrap();
    frame.type_text("Europe");
    let query = frame
        .with_widget_mut::<Dropdown, _>(|dropdown| dropdown.query_rect)
        .unwrap();
    frame.click_at(query.x0 + 1.0, query.center().y);
    frame.type_text("X");
    let mut text = TextRenderer::new();
    let tokens = &frame.theme().tokens;
    let insets = 2.0 * f64::from(tokens.spacing.s2 + tokens.spacing.s3);
    let hint_width = f64::from(
        text.measure_style(
            &crate::i18n!("dropdown-no-matches"),
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
                dropdown.filter, "XEurope",
                "pointer caret uses the shaped query layout"
            );
            assert_eq!(dropdown.visible_count(), 0);
            assert!(
                dropdown.list_rect().width() - insets >= hint_width,
                "the localized empty-result text must fit its actual painted clip"
            );
        })
        .unwrap();
}

#[test]
fn dropdown_language_accessibility_selects_stable_ids_with_duplicate_labels() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let state = crate::LocalState::new(String::new());
    let mut dropdown = Dropdown::from_state(Vec::new(), &state);
    dropdown.set_value_options(vec![
        ("first".into(), "Same label".into()),
        ("second".into(), "Same label".into()),
    ]);
    let (mut document, root) = doc_with_tz("");
    assert!(Widget::perform_action(
        &mut dropdown,
        &crate::a11y::Action::SetValue("second".into()),
        &[],
        &mut crate::widget::Edits::lend(&mut document),
        &root
    ));
    assert_eq!(state.get(), "second");
    assert!(!Widget::perform_action(
        &mut dropdown,
        &crate::a11y::Action::SetValue("foreign".into()),
        &[],
        &mut crate::widget::Edits::lend(&mut document),
        &root
    ));
    assert_eq!(state.get(), "second");
}

#[test]
fn dropdown_language_empty_stable_value_keeps_its_human_label() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let state = crate::LocalState::new(String::new());
    let mut dropdown = Dropdown::from_state(Vec::new(), &state);
    dropdown.set_value_options(vec![
        (String::new(), "Session language".into()),
        ("de_DE".into(), "German".into()),
    ]);
    let mut frame =
        crate::headless::FrameHarness::new(crate::State::new(), Box::new(dropdown), (420, 180));
    frame
        .with_widget_mut::<Dropdown, _>(|dropdown| {
            assert!(dropdown.has_selected_value());
            assert_eq!(dropdown.selected_label(), "Session language");
            let node = Widget::accessibility(dropdown).unwrap();
            assert_eq!(node.value.as_deref(), Some("Session language"));
        })
        .unwrap();
}

#[test]
fn dropdown_language_revoked_action_releases_modal_capture() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut dropdown =
        Dropdown::new(opts(), "legacy").on_action(crate::widget::test_string_action_binding());
    let allowed =
        std::collections::HashSet::from([tilo_actions::ActionAddress::new("test:control", "set")]);
    crate::widget::controls::refresh_control_action_availability(&mut dropdown, &allowed);
    let mut doc = AutoCommit::new();
    assert!(Widget::perform_action(
        &mut dropdown,
        &crate::a11y::Action::Expand,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert!(Widget::modal_capture(&dropdown).is_some());
    crate::widget::controls::refresh_control_action_availability(
        &mut dropdown,
        &std::collections::HashSet::new(),
    );
    assert!(!Widget::is_enabled(&dropdown));
    assert!(!dropdown.is_open());
    assert!(Widget::modal_capture(&dropdown).is_none());
    assert!(crate::widget::controls::take_control_invocation(&mut dropdown).is_none());

    // Unavailable capture-only fixtures retain their authored open state.
    let mut capture = Dropdown::new(opts(), "legacy")
        .on_action(crate::widget::test_string_action_binding())
        .open();
    crate::widget::controls::refresh_control_action_availability(
        &mut capture,
        &std::collections::HashSet::new(),
    );
    assert!(capture.is_open());
}

/// The open list's rows sit inside the menu inset every menu shares,
/// so its armed row runs concentric with the panel: the inset is not a
/// row, and the first row starts after it.
#[test]
fn the_open_list_insets_its_rows_like_every_menu() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut dd = Dropdown::new(opts(), "tz");
    dd.rect = Rect::new(0.0, 0.0, 240.0, 32.0);
    let n = dd.options.len();
    let list = list_of(&mut dd);
    let inset = crate::widget::floating_surface::menu_inset(&Theme::dark());
    let row = f64::from(crate::tokens::Metrics::BASE.menu_row_height);
    let x = list.center().x;
    assert_eq!(dd.list.row_at(Point::new(x, list.y0 + 2.0)), None);
    assert_eq!(dd.list.row_at(Point::new(4.0, list.y0 + 20.0)), None);
    assert_eq!(
        dd.list.row_at(Point::new(x, list.y0 + inset + 1.0)),
        Some(0)
    );
    assert!(
        (list.height() - (crate::num::f64_of_usize(n) * row + 2.0 * inset)).abs() < 1e-9,
        "the list hugs its rows plus the inset: {list:?}"
    );
}

#[test]
fn updating_placeholder_does_not_select_or_replace_a_private_value() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let selection = crate::LocalState::new(String::new());
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

/// CRDT doc with `tz` seeded as a Text object holding `value`.
fn doc_with_tz(value: &str) -> (AutoCommit, ObjId) {
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    let obj = doc.put_object(&root, "tz", ObjType::Text).unwrap();
    let _ = doc.splice_text(&obj, 0, 0, value);
    (doc, root)
}

use crate::synthetic_input::key_press;

/// Drive a key through a focused dropdown with a throwaway taffy node.
fn feed_key(dd: &mut Dropdown, ev: &WindowEvent, doc: &mut AutoCommit, root: &ObjId) -> bool {
    Dispatch::new().doc(doc).path(root).send(dd, ev).handled
}

#[test]
fn dropdown_keyboard_opens_navigates_selects_and_escapes() {
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
        dd.do_open();
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
            Dropdown::new(opts(), "tz").on_action(crate::widget::test_string_action_binding());
        crate::widget::controls::refresh_control_action_availability(
            &mut dd,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                "test:control",
                "set",
            )]),
        );
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&doc, &root);
        dd.set_focus(true);
        dd.do_open();
        // Move to "America/New_York" (index 2) and select it.
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
            crate::widget::controls::take_control_invocation(&mut dd)
                .unwrap()
                .arguments["value"],
            "America/New_York"
        );
        assert_eq!(
            crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
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
        dd.do_open();
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
            crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
            Some("UTC")
        );
    }

    fn open_builder_aligns_highlight_to_selection_on_sync() {
        let mut dd = Dropdown::new(opts(), "tz").open();
        let (doc, root) = doc_with_tz("Asia/Tokyo");
        dd.sync(&doc, &root);
        assert!(dd.is_open());
        assert_eq!(
            dd.highlight_index(),
            3,
            "highlight tracks the selected option"
        );
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    starts_closed_and_reads_value_on_sync();
    down_opens_when_closed_and_focused();
    arrows_move_highlight_while_open();
    enter_emits_selection_action_and_closes();
    escape_closes_without_changing_value();
    open_builder_aligns_highlight_to_selection_on_sync();
}

#[test]
fn combobox_filter_editing_handles_case_spaces_graphemes_and_ime() {
    fn typing_filters_options_case_insensitively() {
        let mut dd = Dropdown::new(zones(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&doc, &root);
        dd.set_focus(true);
        dd.do_open();
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
        dd.do_open();
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
        dd.do_open();
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
        // "a" matches Amsterdam, America, Asia.
        assert_eq!(dd.visible_count(), 3);
    }

    fn backspace_deletes_one_extended_grapheme_from_the_filter() {
        let mut dd = Dropdown::new(zones(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&doc, &root);
        dd.set_focus(true);
        dd.do_open();
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
        assert_eq!(dd.filter_selection, Selection::collapsed(0));
    }

    fn typing_replaces_the_selected_filter_range() {
        let mut dd = Dropdown::new(zones(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&doc, &root);
        dd.set_focus(true);
        dd.do_open();
        dd.filter = "Europe".into();
        dd.filter_selection = Selection {
            anchor: 0,
            cursor: dd.filter.len(),
        };
        feed_key(
            &mut dd,
            &key_press(Key::Character("亜".into()), Some("亜")),
            &mut doc,
            &root,
        );
        assert_eq!(dd.filter_query(), "亜");
        assert_eq!(dd.filter_selection, Selection::collapsed("亜".len()));
    }

    fn ime_preedit_is_visible_but_filters_only_after_commit() {
        let mut dd = Dropdown::new(padded(&["東京", "London"]), "city");
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        dd.apply_layout_external(Rect::new(10.0, 10.0, 260.0, 50.0));
        dd.set_focus(true);
        dd.do_open();

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
        Widget::draw(
            &mut dd,
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
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

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    typing_filters_options_case_insensitively();
    space_is_part_of_an_open_combobox_query();
    backspace_edits_the_filter();
    backspace_deletes_one_extended_grapheme_from_the_filter();
    typing_replaces_the_selected_filter_range();
    ime_preedit_is_visible_but_filters_only_after_commit();
}

#[test]
fn open_list_pointer_capture_focus_and_placement() {
    fn losing_focus_closes_the_list() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (doc, root) = doc_with_tz("UTC");
        dd.sync(&doc, &root);
        dd.set_focus(true);
        dd.do_open();
        assert!(dd.is_open());
        dd.set_focus(false);
        assert!(!dd.is_open(), "Tab-away dismisses the dropdown");
    }

    fn open_dropdown_captures_pointer() {
        let mut dd = Dropdown::new(opts(), "tz");
        assert!(!dd.wants_pointer_capture());
        dd.do_open();
        assert!(dd.wants_pointer_capture());
    }

    /// Hover is a repaint, not consumed input (the rule `Button` documents):
    /// the move that changes it is left for siblings, and leaving the window
    /// clears it because no `CursorMoved` will follow.
    fn hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover() {
        let mut dd = Dropdown::new(opts(), "tz");
        dd.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
        let (mut doc, root) = doc_with_tz("UTC");
        let enter = crate::synthetic_input::cursor_moved(20.0, 20.0);
        assert!(
            !feed_key(&mut dd, &enter, &mut doc, &root),
            "a hover change must not mark the move handled"
        );
        assert!(dd.is_hovered, "the pointer is over the control");
        let leave = WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        };
        feed_key(&mut dd, &leave, &mut doc, &root);
        assert!(!dd.is_hovered, "CursorLeft clears the resting hover");
    }

    /// The open list flips above the control when opening below would leave
    /// the host viewport and there is room above; hit-testing shares the
    /// rect, so the flipped list is clickable where it is painted. The
    /// viewport arrives either through the presence context at paint or
    /// from the host via `set_viewport`; both routes are exercised.
    fn open_list_flips_above_when_it_would_leave_the_viewport() {
        let mut dd = Dropdown::new(opts(), "tz");
        dd.rect = Rect::new(0.0, 700.0, 200.0, 740.0);
        let below = list_of(&mut dd);
        assert!(
            below.y0 >= dd.rect.y1,
            "without a viewport the list opens below"
        );
        // Route 1: the paint pass sees the App's pushed window viewport.
        let presence = PresenceCtx::default();
        presence
            .viewport
            .set(Some(Rect::new(0.0, 0.0, 800.0, 760.0)));
        let mut scene = Scene::new();
        dd.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &presence,
        );
        assert_eq!(dd.viewport, Some(Rect::new(0.0, 0.0, 800.0, 760.0)));
        // Route 2: a host inside a Row/Column hands it over explicitly.
        let mut hosted =
            Dropdown::new(opts(), "tz").with_viewport(Rect::new(0.0, 0.0, 800.0, 760.0));
        hosted.rect = dd.rect;
        assert_eq!(list_of(&mut hosted), list_of(&mut dd));
        let flipped = list_of(&mut dd);
        assert!(
            flipped.y1 <= dd.rect.y0,
            "the list opens above the control: {flipped:?} vs {:?}",
            dd.rect
        );
        assert!(flipped.y0 >= 0.0, "and stays inside the viewport");
        assert!((flipped.height() - below.height()).abs() < 1e-6);
        let row = f64::from(crate::tokens::Metrics::BASE.menu_row_height);
        assert_eq!(
            dd.list.row_at(Point::new(10.0, flipped.y0 + row / 2.0)),
            Some(0),
            "the first row is hit where it is painted"
        );
        // No room above either: stay below rather than run off the top.
        dd.set_viewport(Rect::new(0.0, 690.0, 800.0, 760.0));
        assert!((list_of(&mut dd).y0 - below.y0).abs() < 1e-6);
    }

    /// The dropdown is the fluid item in a row (F2-16): laid beside fixed
    /// controls in a row too narrow for its `percent(100%)`, it gives the
    /// room back, so its siblings keep their size and the trailing one is
    /// still on screen instead of being pushed to zero width past the edge.
    fn dropdown_in_a_row_yields_width_to_fixed_siblings() {
        use crate::widget::{Button, Row};
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
            dd.width() >= PAD_X + CHEVRON_W - 0.5,
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
            (dd.height() - crate::widget::ControlSize::Regular.height(&theme)).abs() < 0.5,
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
        let field = crate::widget::ControlSize::Regular.height(&theme);
        assert!((dd.rect.height() - field).abs() < 0.5, "{:?}", dd.rect);
        assert!(dd.rect.y0 > dd.outer.y0, "the caption sits above the field");
        assert_eq!(Widget::get_rect(&dd), dd.outer);
        let node = Widget::accessibility(&dd).expect("dropdown semantics");
        assert_eq!(node.name.as_deref(), Some("Level"));
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    losing_focus_closes_the_list();
    open_dropdown_captures_pointer();
    hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover();
    open_list_flips_above_when_it_would_leave_the_viewport();
    dropdown_in_a_row_yields_width_to_fixed_siblings();
    a_caption_stacks_above_the_field_and_names_it();
}

#[test]
fn committed_selection_binds_process_local_state() {
    fn local_state_binding_is_two_way_across_dropdowns() {
        let state = crate::LocalState::new(String::from("UTC"));
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut first = Dropdown::from_state(opts(), &state)
            .on_change_view(move |value| captured.borrow_mut().push(value.to_owned()));
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
            &crate::a11y::Action::SetValue(String::from("Europe/Amsterdam")),
            &[],
            &mut crate::widget::Edits::lend(&mut document),
            &automerge::ROOT,
        ));
        assert_eq!(state.get(), "Europe/Amsterdam");
        assert_eq!(crate::testing::answer_controls(&mut first), 1);
        assert_eq!(&*changes.borrow(), &["Europe/Amsterdam"]);
        Widget::sync(&mut second, &document, &automerge::ROOT);
        assert_eq!(second.selected(), "Europe/Amsterdam");

        second.set_selected("America/New_York");
        assert_eq!(state.get(), "America/New_York");
        Widget::sync(&mut first, &document, &automerge::ROOT);
        assert_eq!(first.selected(), "America/New_York");
        assert_eq!(&*changes.borrow(), &["Europe/Amsterdam"]);
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    local_state_binding_is_two_way_across_dropdowns();
}

#[test]
fn a_menu_button_reports_a_choice_without_keeping_it() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let state = crate::LocalState::new(String::new());
    let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let captured = std::rc::Rc::clone(&chosen);
    let mut menu = Dropdown::from_state(opts(), &state)
        .as_menu(crate::icons::Icon::Plus)
        .on_change_view(move |value| captured.borrow_mut().push(value.to_owned()));
    menu.rect = Rect::new(560.0, 100.0, 592.0, 132.0);
    let mut document = AutoCommit::new();
    let root = automerge::ROOT;

    let list = list_of(&mut menu);
    assert!((list.x1 - menu.rect.x1).abs() < 1e-6, "{list:?}");
    assert!(list.width() >= menu_min_width());
    menu.set_viewport(Rect::new(0.0, 0.0, 400.0, 800.0));
    let kept = list_of(&mut menu);
    assert!(
        kept.x1 <= 400.0 && kept.x0 >= 0.0,
        "kept in the window: {kept:?}"
    );

    // Typing does not filter: a menu shows no query.
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

    assert_eq!(
        crate::testing::answer_controls(&mut menu),
        1,
        "the choice reaches its handler when it is answered"
    );
    assert_eq!(&*chosen.borrow(), &["Europe/Amsterdam"]);
    assert!(!menu.is_open());
    assert_eq!(menu.selected(), "", "a command is not kept as a value");
    assert_eq!(state.get(), "");
    let node = menu.accessibility().expect("a menu button is announced");
    assert_eq!(node.role, crate::a11y::Role::Button);
}

#[test]
fn dropdown_accessibility_semantics_and_disabled_state() {
    fn semantic_actions_expand_collapse_and_reject_unknown_values() {
        use crate::a11y::Action;

        let mut dropdown = Dropdown::new(opts(), "tz");
        let (mut document, root) = doc_with_tz("UTC");
        Widget::sync(&mut dropdown, &document, &root);

        assert!(Widget::perform_action(
            &mut dropdown,
            &Action::Expand,
            &[],
            &mut crate::widget::Edits::lend(&mut document),
            &root,
        ));
        assert!(dropdown.is_open());
        assert!(Widget::perform_action(
            &mut dropdown,
            &Action::Collapse,
            &[],
            &mut crate::widget::Edits::lend(&mut document),
            &root,
        ));
        assert!(!dropdown.is_open());
        assert!(!Widget::perform_action(
            &mut dropdown,
            &Action::SetValue(String::from("not an option")),
            &[],
            &mut crate::widget::Edits::lend(&mut document),
            &root,
        ));
        assert_eq!(dropdown.selected(), "UTC");
        assert_eq!(
            crate::doc::read_str_or_text(&document, &root, "tz").as_deref(),
            Some("UTC")
        );
    }

    fn with_enabled_false_is_reported_by_is_enabled() {
        let dd = Dropdown::new(opts(), "tz").with_enabled(false);
        assert!(!Widget::is_enabled(&dd));
        assert!(
            Widget::is_enabled(&Dropdown::new(opts(), "tz")),
            "default is enabled"
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
        use crate::a11y::Role;
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
        use crate::a11y::Role;
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
        dd.do_open();
        let node = Widget::accessibility(&dd).unwrap();
        assert_eq!(node.state.expanded, Some(true), "open dropdown is expanded");
        assert!(node.state.focused);

        let dd2 = Dropdown::new(opts(), "tz").with_enabled(false);
        let node2 = Widget::accessibility(&dd2).unwrap();
        assert!(node2.state.disabled);
    }

    fn action_binding_owns_accessibility_selection() {
        let mut dropdown = Dropdown::new(opts(), "legacy")
            .on_change(|_| panic!("legacy callback must be suppressed"))
            .on_action(crate::widget::test_string_action_binding());
        let mut doc = AutoCommit::new();
        crate::widget::controls::refresh_control_action_availability(
            &mut dropdown,
            &std::collections::HashSet::new(),
        );
        assert!(!Widget::is_enabled(&dropdown));
        crate::widget::controls::refresh_control_action_availability(
            &mut dropdown,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                "test:control",
                "set",
            )]),
        );
        assert!(Widget::is_enabled(&dropdown));
        assert!(Widget::perform_action(
            &mut dropdown,
            &crate::a11y::Action::SetValue("Asia/Tokyo".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(doc.get(&automerge::ROOT, "legacy").unwrap().is_none());
        assert_eq!(
            crate::widget::controls::take_control_invocation(&mut dropdown)
                .unwrap()
                .arguments["value"],
            "Asia/Tokyo"
        );
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    semantic_actions_expand_collapse_and_reject_unknown_values();
    with_enabled_false_is_reported_by_is_enabled();
    disabled_dropdown_does_not_open();
    accessibility_reports_combobox_role_value_and_collapsed();
    accessibility_unselected_uses_placeholder_name_and_no_value();
    accessibility_reflects_open_and_disabled();
    action_binding_owns_accessibility_selection();
}

#[test]
fn a_sections_choosers_share_one_width_and_line_up_on_both_edges() {
    use crate::widget::{Element, Row, Section, Spacer, Text};
    fn row(label: &str, dropdown: Dropdown) -> Element {
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(Text::new(label)))
                .push(Box::new(Spacer::new()))
                .push(Box::new(dropdown)),
        )
    }
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let short = || vec!["A".to_string(), "B".to_string()];
    let section = Section::titled(
        "Formats",
        vec![
            row("Zone", Dropdown::new(opts(), "tz").with_id("zone")),
            row("Size", Dropdown::new(short(), "ab").with_id("size")),
            row(
                "Filter",
                Dropdown::new(short(), "f")
                    .caption("Level")
                    .with_id("filter"),
            ),
            Box::new(Section::new(vec![row(
                "Nested",
                Dropdown::new(short(), "n").with_id("nested"),
            )])),
        ],
    );
    let app =
        crate::headless::FrameHarness::new(crate::State::new(), Box::new(section), (900, 400));
    let targets = app.targets();
    let target = |id: &str| *targets.get(id).expect(id);
    let (zone, size) = (target("zone"), target("size"));
    assert!(
        (zone.width() - size.width()).abs() < 0.5 && (zone.x1 - size.x1).abs() < 0.5,
        "the section's choosers share the widest one's width and edges: {zone:?} / {size:?}"
    );
    assert!(
        target("filter").width() < zone.width() - 1.0,
        "a captioned filter keeps its own width"
    );
    assert!(
        target("nested").width() < zone.width() - 1.0,
        "a nested section shares its own width, not the outer one's"
    );

    let cap = f64::from(Theme::dark().tokens.metrics.chooser_width);
    let section = Section::new(vec![
        row("Size", Dropdown::new(short(), "ab").with_id("size")),
        row(
            "Device",
            Dropdown::new(vec!["Built-in speakers and headphone jack".repeat(3)], "d")
                .with_id("device"),
        ),
    ]);
    let app =
        crate::headless::FrameHarness::new(crate::State::new(), Box::new(section), (900, 400));
    let targets = app.targets();
    let target = |id: &str| *targets.get(id).expect(id);
    for id in ["size", "device"] {
        assert!(
            (target(id).width() - cap).abs() < 0.5,
            "a long option caps the shared width at the chooser width: {id} {:?}",
            target(id)
        );
    }
}

#[test]
fn dropdown_sizes_to_its_options_and_keeps_its_value_while_open() {
    fn hugs_its_widest_option_unless_it_fills() {
        use crate::widget::Row;
        let row = Row::new()
            .push(Box::new(Dropdown::new(opts(), "tz").with_id("hug")))
            .push(Box::new(
                Dropdown::new(vec!["A".into(), "B".into()], "ab").with_id("tiny"),
            ));
        let app =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(row), (900, 200));
        let targets = app.targets();
        let hug = targets.get("hug").expect("hugging dropdown");
        let tiny = targets.get("tiny").expect("short dropdown");
        assert!(
            hug.width() < 400.0 && hug.width() > tiny.width(),
            "a dropdown is as wide as its options, not the row: {hug:?} / {tiny:?}"
        );
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let widest = opts()
            .iter()
            .map(|o| f64::from(text.measure_style(o, None, &theme.text(TextRole::Label)).0))
            .fold(0.0, f64::max);
        assert!(
            hug.width() >= widest + PAD_X + CHEVRON_W,
            "the widest option fits beside the chevron: {hug:?} vs {widest}"
        );

        let filled = crate::widget::Row::new()
            .push(Box::new(Dropdown::new(opts(), "tz").with_id("fill").fill()));
        let app =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(filled), (900, 200));
        let fill = *app.targets().get("fill").expect("filling dropdown");
        assert!(fill.width() > 600.0, "fill spans the row: {fill:?}");
    }

    fn a_short_list_jumps_instead_of_filtering() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&doc, &root);
        dd.set_focus(true);
        dd.do_open();
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
        // Letters typed together spell a name: from UTC, "a" arms
        // America, the first option it begins, and "as" goes on to Asia.
        dd.close();
        dd.do_open();
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

    fn a_caption_placed_externally_never_covers_the_field() {
        let mut dd = Dropdown::new(opts(), "tz").caption("Time zone");
        let theme = Theme::dark();
        let face = DropdownFace::new(Rect::ZERO, "").caption("Time zone");
        let height = face.outer_height(&theme);
        dd.apply_layout_external(Rect::new(0.0, 0.0, 200.0, height));
        assert!(
            (dd.rect.height() - crate::widget::ControlSize::Regular.height(&theme)).abs() < 0.5,
            "the field keeps its height below the caption: {:?}",
            dd.rect
        );
        assert!(dd.rect.y0 > 0.0);
    }

    fn a_resolved_name_is_spoken_as_given() {
        let dd = Dropdown::new(opts(), "tz").with_resolved_accessible_name("Zeitzone");
        let node = Widget::accessibility(&dd).expect("semantics");
        assert_eq!(node.name.as_deref(), Some("Zeitzone"));
    }

    fn an_idle_dropdown_needs_no_layout() {
        let theme = Theme::dark();
        let mut dd = Dropdown::new(opts(), "tz");
        assert!(Widget::is_layout_dirty(&dd));
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
        assert!(!Widget::is_layout_dirty(&dd));
        dd.set_options(zones());
        assert!(
            Widget::is_layout_dirty(&dd),
            "new options may change the width"
        );
    }

    fn the_face_matches_the_retained_control() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let face = DropdownFace::new(Rect::ZERO, "UTC").leading_icon(crate::icons::Icon::Globe);
        let width = face.intrinsic_width(opts().iter().map(String::as_str), &mut text, &theme);
        let mut dd = Dropdown::new(opts(), "tz").leading_icon(crate::icons::Icon::Globe);
        dd.adopt_theme(&theme);
        dd.set_selected("UTC");
        assert!((dd.measured_width(&mut text) - width).abs() < 0.5);
        assert!(
            (DropdownFace::height(crate::widget::ControlSize::Small, &theme)
                - crate::widget::ControlSize::Small.height(&theme))
            .abs()
                < 1e-9
        );
        let captioned = DropdownFace::new(Rect::new(0.0, 0.0, 120.0, 60.0), "UTC").caption("Zone");
        let field = captioned.field_rect(&theme);
        assert!(field.y0 > 0.0 && field.y1 == 60.0);
        assert!(captioned.contains(&theme, field.center()));
        assert!(!captioned.contains(&theme, Point::new(60.0, 1.0)));
        let mut scene = Scene::new();
        captioned.paint(&mut scene, Affine::IDENTITY, &mut text, &theme);
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    hugs_its_widest_option_unless_it_fills();
    a_short_list_jumps_instead_of_filtering();
    a_caption_placed_externally_never_covers_the_field();
    a_resolved_name_is_spoken_as_given();
    an_idle_dropdown_needs_no_layout();
    the_face_matches_the_retained_control();
}
