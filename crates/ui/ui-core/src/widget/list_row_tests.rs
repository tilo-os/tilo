use super::*;
use crate::app::driver::Driver;
use crate::component::{Bound, Described, Fired, Intent};
use crate::headless::InspectionNode;
use crate::widget::{Column, Element};
use std::cell::Cell;
use std::rc::Rc;
use vello::Scene;
use vello::kurbo::Affine;

fn geometry(face: &ListRowFace<'_>, rect: Rect) -> RowGeometry {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    face.geometry(rect, &RowMetrics::new(&theme), &mut text, &theme)
}

fn find<'a>(node: &'a InspectionNode, widget: &str) -> Option<&'a InspectionNode> {
    if node.widget == widget {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, widget))
}

fn rect_of(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

#[test]
fn a_title_cut_short_reads_in_full_in_the_tooltip() {
    let theme = crate::theme::Theme::dark();
    let message = "Expected `&[String]`, found `&Vec<String>` in the argument to parse_limit";
    let long = ListRow::new("long", message).meta("Ln 14, Col 29");
    let h = Driver::for_root(crate::State::new(), long, (240, 60), theme.clone());
    let row = h.root::<ListRow>();
    let band = row.text_band();
    let (rect, text) = Widget::tooltip_target_at(row, band.center()).expect("elided title");
    assert_eq!(text, message);
    assert_eq!(rect, band);

    let short = ListRow::new("short", "Unused import").meta("Ln 2, Col 5");
    let h = Driver::for_root(crate::State::new(), short, (480, 60), theme);
    let row = h.root::<ListRow>();
    assert_eq!(
        Widget::tooltip_target_at(row, row.text_band().center()),
        None,
        "a title that fits needs no tooltip"
    );
}

fn hovered() -> Interaction {
    Interaction {
        hovered: true,
        ..Interaction::default()
    }
}

tilo_test_env::scenario! {
    fn a_rows_actions_are_shown_and_run;

    phase hover_actions_replace_the_meta_but_not_the_value;
    {
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
        let actions = [
            RowAction {
                icon: Icon::Reply,
                label: "Reply",
            },
            RowAction {
                icon: Icon::Trash,
                label: "Delete message",
            },
        ];
        let idle = ListRowFace::new("Ada").meta("09:41").actions(&actions);
        assert!(idle.action_rects(rect, &theme).is_empty(), "hidden at rest");
        let hover = idle.clone().interaction(hovered());
        let rects = hover.action_rects(rect, &theme);
        assert_eq!(rects.len(), 2);
        assert!(rects[0].x1 < rects[1].x0, "in order, not touching");
        for r in &rects {
            assert!(rect.contains(r.origin()) && r.x1 <= rect.x1);
            assert!(
                (r.center().y - rect.center().y).abs() < 0.01,
                "centred on the row"
            );
        }
        let valued = ListRowFace::new("Tokyo")
            .title_value("09:41")
            .actions(&actions)
            .interaction(hovered());
        let geo = geometry(&valued, rect);
        let rects = valued.action_rects(rect, &theme);
        let value_left = geo.value_right
            - f64::from(
                TextRenderer::new()
                    .measure_style("09:41", None, &RowMetrics::new(&theme).value_title)
                    .0,
            );
        assert!(rects.last().unwrap().x1 <= value_left + 0.01);
        let touch = ListRowFace::new("Ada")
            .actions(&actions)
            .reveal_actions(true);
        assert_eq!(touch.action_rects(rect, &theme).len(), 2);
        assert!(
            hover
                .clone()
                .enabled(false)
                .action_rects(rect, &theme)
                .is_empty()
        );
    }

    phase a_rows_actions_say_where_they_stand_without_a_hover;
    {
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
        let mut row = ListRow::new("note-1", "Ada")
            .action(Icon::Plus, "New note under Ada", || {})
            .action(Icon::MoreHorizontal, "More actions for Ada", || {});
        row.metrics = Some(RowMetrics::new(&theme));
        row.theme = Some(theme.clone());
        row.rect = rect;
        let node = Widget::accessibility(&row).unwrap();
        let buttons: Vec<_> = node
            .children
            .iter()
            .filter(|child| {
                child
                    .action_target
                    .as_deref()
                    .is_some_and(|target| target.contains("/action/"))
            })
            .collect();
        assert_eq!(buttons.len(), 2, "one button per action");
        assert!(
            row.action_rects().is_empty(),
            "the row is at rest: it shows no action"
        );
        let band = row.text_band();
        let slots = row.with_face(|face| {
            face.clone()
                .interaction(hovered())
                .action_rects(band, &theme)
        });
        let bounds: Vec<Rect> = buttons
            .iter()
            .map(|button| match button.bounds {
                Some(bounds) => bounds.to_rect(),
                None => panic!("an action a reader can press says where it stands"),
            })
            .collect();
        assert_eq!(bounds.len(), slots.len());
        for (bounds, slot) in bounds.iter().zip(&slots) {
            assert!(
                (bounds.x0 - slot.x0).abs() < 0.01 && (bounds.x1 - slot.x1).abs() < 0.01,
                "the hovered row shows the action where its button stands: {bounds:?} vs {slot:?}"
            );
        }
    }

    phase a_row_says_where_its_actions_stand;
    {
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
        let mut row = ListRow::new("note-1", "Ada")
            .selected(true)
            .action(Icon::Trash, "Delete", || {});
        row.set_interaction_environment(crate::interaction::InteractionEnvironment::TOUCH);
        row.metrics = Some(RowMetrics::new(&theme));
        row.theme = Some(theme.clone());
        row.rect = rect;
        let rects = crate::widget::capability::capability::<dyn crate::widget::RowActionRects>(&row)
            .expect("the row answers where its actions stand")
            .row_action_rects();
        assert_eq!(rects.len(), 1, "one action, where the row shows it");
        assert!(rect.contains(rects[0].center()), "{rect:?} vs {rects:?}");
        assert_eq!(rects, row.action_rects(), "the same place it paints");
    }

    phase hover_actions_run_from_assistive_technology_and_the_keyboard;
    {
        let replied = Rc::new(Cell::new(0));
        let deleted = Rc::new(Cell::new(0));
        let (r, d) = (Rc::clone(&replied), Rc::clone(&deleted));
        let mut row = ListRow::new("m", "Ada")
            .on_open(|| {})
            .action(Icon::Reply, "Reply", move || r.set(r.get() + 1))
            .action(Icon::Trash, "Delete message", move || d.set(d.get() + 1));
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_action(
            &mut row,
            &crate::a11y::Action::Activate,
            &[1],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!((replied.get(), deleted.get()), (0, 1));
        row.set_focus(true);
        row.armed_action = Some(0);
        assert!(Widget::perform_action(
            &mut row,
            &crate::a11y::Action::Activate,
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(replied.get(), 1);
        row.set_focus(false);
        assert_eq!(row.armed_action, None, "leaving the row disarms");
        // A hover action that is only a callback is not a published action, so
        // the inventory reports nothing for it; the row's own activation and
        // binding the row's action are what make it visible to a native audit.
        let bindings = crate::widget::controls::ActionControls::control_action_bindings(&row);
        assert!(
            bindings
                .iter()
                .all(|binding| !matches!(binding.control_id.as_str(), "Reply" | "Delete message")),
            "the row's hover actions are callbacks, not published actions: {bindings:?}"
        );
    }

    phase a_disabled_row_is_inert_and_skipped_by_focus;
    {
        let mut row = ListRow::new("r", "Row").on_open(|| {}).with_enabled(false);
        assert!(!row.is_focusable());
        assert!(!Widget::is_enabled(&row));
        let mut doc = automerge::AutoCommit::new();
        assert!(!Widget::perform_action(
            &mut row,
            &crate::a11y::Action::Activate,
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
    }

    phase a_touch_pointer_shows_a_selected_rows_actions;
    {
        let mut row = ListRow::new("m", "Ada")
            .selected(true)
            .action(Icon::Trash, "Delete", || {});
        assert!(!row.reveal_actions());
        row.set_interaction_environment(crate::interaction::InteractionEnvironment::TOUCH);
        assert!(row.reveal_actions());
    }

    phase a_swipe_bound_to_an_action_runs_it_once_as_the_person_swipes_to_it;
    {
        let mut spec = tilo_actions::ResourceSpec::new("res-milk", "test.errand", "Errand");
        spec = spec.with_action(
            tilo_actions::ActionSpec::operation(
                "delete",
                "Delete",
                tilo_actions::OperationKind::Effect,
                "test.errand.effect",
                &(),
            )
            .expect("a test action"),
        );
        let binding = tilo_actions::ActionBinding::unit_for(&spec, "delete").expect("published");
        let address = binding.address().clone();
        let mut row =
            ListRow::new("milk", "Buy milk").swipe_binding(Icon::Trash, "Delete", true, binding);
        row.swipe.as_mut().expect("a swipe").run();
        let taken = crate::widget::controls::take_control_invocation(&mut row);
        assert_eq!(
            taken.as_ref().map(|taken| taken.address.clone()),
            Some(address),
            "one swipe runs the action once"
        );
        assert!(
            crate::widget::controls::take_control_invocation(&mut row).is_none(),
            "the swipe is taken once"
        );
    }
}

tilo_test_env::scenario! {
    fn a_row_paints_and_speaks_its_selection_tones_and_marks;





    phase an_item_nobody_has_named_shows_a_muted_placeholder_and_is_spoken_by_it;
    {
        let untitled = crate::i18n::I18nManager::lookup_plain("list-row-untitled");
        assert_ne!(untitled, "list-row-untitled", "the placeholder is a phrase");
        assert!(
            !untitled.is_empty(),
            "a row of an item nobody has named yet shows words"
        );
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let row = ListRow::new("r", "");
        ListRowFace::new("").paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 320.0, 40.0),
        );
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "an empty title paints its placeholder"
        );
        let node = Widget::accessibility(&row).unwrap();
        assert_eq!(node.name.as_deref(), Some(untitled.as_str()));

        let node = Widget::accessibility(&ListRow::new("r", "  ")).unwrap();
        assert_eq!(node.name.as_deref(), Some(untitled.as_str()));
        let node = Widget::accessibility(&ListRow::new("r", "Groceries")).unwrap();
        assert_eq!(node.name.as_deref(), Some("Groceries"));
    }

    phase the_words_a_search_found_are_marked_at_word_starts_whatever_their_case;
    {
        let line = "The glass rattled; Glassworks and fibreglass.";
        let spans = found_spans(line, &["glass"]);
        let marked: Vec<&str> = spans
            .iter()
            .filter_map(|span| line.get(span.clone()))
            .collect();
        assert_eq!(marked, ["glass", "Glass"]);
        assert!(found_spans(line, &[]).is_empty());
        assert!(found_spans(line, &[""]).is_empty());
        let spans = found_spans("Tideglass letters", &["tide", "tideglass"]);
        assert_eq!(spans, [std::ops::Range { start: 0, end: 9 }]);
        assert_eq!(
            found_spans("İstanbul", &["i̇stan"]),
            [std::ops::Range { start: 0, end: 6 }]
        );
    }

    phase a_toned_meta_is_spoken_once;
    {
        let node = ListRowFace::new("Ring the letting agent")
            .meta("Overdue since Jun 29")
            .meta_spans(&[("Overdue since Jun 29", Tone::Danger)])
            .accessibility(Rect::new(0.0, 0.0, 320.0, 40.0), "late");
        assert_eq!(node.value.as_deref(), Some("Overdue since Jun 29"));
    }

    phase a_rows_secondary_line_is_said_once_as_its_description_or_in_its_name;
    {
        let row = ListRow::new("bounce-alert", "Flash when a key is ignored")
            .secondary("Show the visual alert each time bounce keys drops a press")
            .description("Show the visual alert each time bounce keys drops a press");
        let node = Widget::accessibility(&row).unwrap();
        assert_eq!(node.name.as_deref(), Some("Flash when a key is ignored"));
        assert_eq!(
            node.description.as_deref(),
            Some("Show the visual alert each time bounce keys drops a press")
        );
        assert_eq!(node.value, None, "the explanation is said once");

        // A row's secondary line that explains nothing is its own text: a reader hears the
        // whole row as its name, as a message is its sender and its subject.
        let read = ListRow::new("path", "Home").secondary("/home/quinten");
        let node = Widget::accessibility(&read).unwrap();
        assert_eq!(node.name.as_deref(), Some("Home, /home/quinten"));
        assert_eq!(node.value, None, "the secondary line is said once, in the name");
        assert_eq!(node.description, None);
        // The title stays a field of its own, the line the row is found by.
        assert_eq!(node.title.as_deref(), Some("Home"));
    }
}

#[test]
fn a_rows_box_presses_as_far_as_the_pointer_needs() {
    use crate::interaction::InteractionEnvironment;
    let theme = Theme::dark();
    let row = || {
        ListRow::new("milk", "Milk")
            .checkbox(false, |_| {})
            .on_activate(|| {})
    };
    let mut widths = Vec::new();
    for (name, input) in [
        ("a pointer", InteractionEnvironment::POINTER),
        ("a finger", InteractionEnvironment::TOUCH),
    ] {
        let mut h = Driver::for_root(
            crate::State::new(),
            Column::new().push(Box::new(row())),
            (360, 300),
            theme.clone(),
        );
        h.set_interaction_environment(input);
        h.relayout_at((360, 300)).expect("layout");
        let inspection = h.inspection();
        let node = find(&inspection.root, "ListRow").expect("the row");
        let row_rect = rect_of(node);
        let boxed = boxed_rect(node.semantics.as_ref().expect("the row is read"));
        let target = f64::from(input.minimum_target());
        assert!(
            boxed.width() >= target - 0.5 && boxed.height() >= target - 0.5,
            "as {name} presses it, the box takes a press {target}×{target} wide: {boxed:?}"
        );
        let lift = row_rect.inflate(crate::chrome::row_bleed(&theme), 0.0);
        assert!(
            lift.x0 - 0.5 <= boxed.x0
                && boxed.x1 <= lift.x1 + 0.5
                && lift.y0 - 0.5 <= boxed.y0
                && boxed.y1 <= lift.y1 + 0.5,
            "the box's press stays inside its row as {name} presses it: {boxed:?} in {lift:?}"
        );
        assert!(
            boxed.width() > m_icon(&theme),
            "the press reaches past the box it draws, {boxed:?}"
        );
        widths.push(boxed.width());
    }
    assert!(
        widths[1] > widths[0],
        "a finger's press on the box is wider than a mouse's: {widths:?}"
    );
}

fn m_icon(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.icon_control)
}

fn boxed_rect(access: &crate::a11y::AccessNode) -> Rect {
    access
        .children
        .iter()
        .find(|child| child.role == crate::a11y::Role::Checkbox)
        .expect("the row's box is a control of its own")
        .bounds
        .expect("a box a reader presses says where it stands")
        .to_rect()
}

#[test]
fn accessibility_names_the_badge_and_reads_parts_and_actions() {
    let actions = [RowAction {
        icon: Icon::Copy,
        label: "Copy URL",
    }];
    let face = ListRowFace::new("/users")
        .leading(RowLeading::Badge("GET"))
        .meta("200")
        .tags(&[("Beta", ChipStyle::Neutral)])
        .count(3)
        .checked(true)
        .actions(&actions);
    let node = face.accessibility(Rect::new(0.0, 0.0, 300.0, 40.0), "row");
    assert_eq!(node.name.as_deref(), Some("GET /users"));
    let value = node.value.unwrap();
    assert!(value.contains("Beta") && value.contains("200") && value.contains('3'));
    assert_eq!(node.state.checked, Some(true));
    assert_eq!(node.children.len(), 1);
    assert_eq!(node.children[0].role, Role::Button);
    assert_eq!(node.children[0].name.as_deref(), Some("Copy URL"));

    let row = ListRow::new("r", "/users")
        .badge("GET")
        .leading_tooltip("Retrieves the resource");
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some("GET /users"));
    assert_eq!(node.description.as_deref(), Some("Retrieves the resource"));

    let row = ListRow::new("r", "Build server")
        .icon(Icon::SquareTerminal)
        .leading_tooltip("SSH");
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some("Build server"));
    assert_eq!(node.description.as_deref(), Some("SSH"));
}

#[test]
fn a_leading_checkbox_toggles_apart_from_activation() {
    let toggled = Rc::new(Cell::new(None));
    let activated = Rc::new(Cell::new(0));
    let (t, a) = (Rc::clone(&toggled), Rc::clone(&activated));
    let mut row = ListRow::new("m", "Ada")
        .checkbox(false, move |on| t.set(Some(on)))
        .on_open(move || a.set(a.get() + 1));
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::ToggleCheck,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(toggled.get(), Some(true));
    assert_eq!(activated.get(), 0);
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.state.checked, Some(true));
}

#[test]
fn a_view_row_is_a_certified_action_a_click_and_an_agent_both_reach() {
    let calls = Rc::new(Cell::new(0));
    let invoked = Rc::clone(&calls);
    let row = ListRow::new("contact-ada", "Ada Lovelace")
        .secondary("ada@example.org")
        .on_open(move || invoked.set(invoked.get() + 1));
    let mut driver = Driver::new(
        crate::State::new(),
        Box::new(Column::new().with_no_padding().push(Box::new(row))),
        (400, 200),
    );
    assert!(driver.native_app_audit().is_certified());
    // Opening a row only shows something, so the row's open is published where an agent reaches
    // it, as the press a screen reader sends.
    let open = driver
        .resources()
        .into_iter()
        .find(|resource| {
            resource
                .id
                .as_str()
                .starts_with(crate::headless::CONTROL_RESOURCE_PREFIX)
                && resource.label.starts_with("Ada Lovelace")
                && resource
                    .actions
                    .iter()
                    .any(|action| action.id == "activate")
        })
        .expect("the row's open is published");
    driver
        .invoke_published(open.id.as_str(), "activate", &serde_json::Map::new())
        .expect("an agent opens the row");
    assert_eq!(calls.get(), 1);
    driver
        .perform_named("contact-ada", &crate::a11y::Action::Activate)
        .expect("the row enters through its action");
    assert_eq!(calls.get(), 2);
    let [x, y, w, h] = find(&driver.inspection().root, "ListRow").unwrap().rect;
    driver.click_at(x + w / 2.0, y + h / 2.0);
    assert_eq!(calls.get(), 3, "a click runs the same action");
}

#[test]
fn delete_runs_the_destructive_action_the_row_swipes_to() {
    let removed = Rc::new(Cell::new(0));
    let removed_too = Rc::clone(&removed);
    let mut row = ListRow::new("milk", "Buy milk")
        .on_activate(|| {})
        .swipe_action(Icon::Trash, "Delete", true, move || {
            removed_too.set(removed_too.get() + 1)
        });
    row.set_focus(true);
    let mut harness = Driver::for_root(crate::State::new(), row, (420, 80), Theme::dark());
    harness.key("Delete");
    assert_eq!(
        removed.get(),
        1,
        "Delete takes the row away as its destructive swipe does"
    );

    let ran = Rc::new(Cell::new(0));
    let ran_too = Rc::clone(&ran);
    let mut kept = ListRow::new("milk", "Buy milk")
        .on_activate(|| {})
        .swipe_action(Icon::Star, "Pin", false, move || {
            ran_too.set(ran_too.get() + 1)
        });
    kept.set_focus(true);
    let mut harness = Driver::for_root(crate::State::new(), kept, (420, 80), Theme::dark());
    harness.key("Delete");
    assert_eq!(ran.get(), 0, "a plain row action is not destructive");
}

#[test]
fn a_menu_shortcut_runs_where_the_row_has_the_keyboard() {
    let checked = Rc::new(Cell::new(0));
    let checked_too = Rc::clone(&checked);
    let mut row = ListRow::new("milk", "Buy milk")
        .on_activate(|| {})
        .shortcuts(vec![(
            crate::platform::Shortcut::from("Ctrl+Enter"),
            Rc::new(move || checked_too.set(checked_too.get() + 1)) as Rc<dyn Fn()>,
        )]);
    row.set_focus(true);
    let mut harness = Driver::for_root(crate::State::new(), row, (420, 80), Theme::dark());
    harness.key("Enter");
    assert_eq!(checked.get(), 0, "the key on its own is not the chord");
    harness.set_modifiers(&["command".to_owned()]);
    harness.key("Enter");
    harness.set_modifiers(&[]);
    assert_eq!(
        checked.get(),
        1,
        "the declared chord runs the command its menu offers"
    );
    harness.set_modifiers(&["command".to_owned(), "shift".to_owned()]);
    harness.key("Enter");
    harness.set_modifiers(&[]);
    assert_eq!(
        checked.get(),
        1,
        "a modifier the shortcut does not name is another press"
    );
}

fn press_leading_checkbox(row: &mut ListRow) {
    if let Some(Leading::Checkbox { checked, .. }) = &mut row.leading {
        *checked = !*checked;
        row.dirty = true;
    } else {
        panic!("the row has no leading checkbox");
    }
}

#[test]
fn a_press_on_the_leading_box_marks_the_row() {
    let toggled = Rc::new(Cell::new(None));
    let seen = Rc::clone(&toggled);
    let row = ListRow::new("milk", "Buy oat milk").checkbox(false, move |on| seen.set(Some(on)));
    let mut harness = Driver::new(crate::State::new(), Box::new(row), (400, 60));
    let [x, y, _, h] = find(&harness.inspection().root, "ListRow")
        .expect("the row is laid out")
        .rect;
    harness.click_at(x + 12.0, y + h / 2.0);
    assert_eq!(
        toggled.get(),
        Some(true),
        "a press on the box a person sees toggles it"
    );
}

fn bound_row(state: &crate::LocalState<bool>, intents: &crate::Intents<Fired>) -> ListRow {
    let props = ListRowProps {
        key: "milk".to_owned(),
        title: "Buy oat milk".to_owned(),
        checkbox: Some(Bound {
            state: state.clone(),
            edits: Some(Intent::new(intents, "milk", "check")),
            name: "Done".to_owned(),
            path: Vec::new(),
            identity: None,
        }),
        ..ListRowProps::default()
    };
    <ListRow as Described>::from_props("milk", props)
}

#[test]
fn a_bound_rows_checkbox_follows_a_check_made_elsewhere() {
    let state = crate::LocalState::new(false);
    let intents = crate::Intents::<Fired>::default();
    let row = bound_row(&state, &intents);
    let mut harness = Driver::for_root(crate::State::new(), row, (400, 60), Theme::dark());
    harness.poll_frame().expect("the first frame draws");
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        0.0,
        "it starts unchecked"
    );

    state.set(true);
    harness
        .recompose()
        .expect("the frame that takes the new value");
    harness.poll_frame().expect("the frame that shows it");
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        1.0,
        "a check made elsewhere shows in the row, landed rather than replayed"
    );
    let node = Widget::accessibility(harness.root::<ListRow>()).expect("a list item");
    assert_eq!(
        node.state.checked,
        Some(true),
        "assistive technology reads the check made elsewhere"
    );

    state.set(false);
    harness
        .recompose()
        .expect("the frame that takes the new value");
    harness.poll_frame().expect("the frame that shows it");
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        0.0,
        "a check taken away elsewhere clears the box"
    );
    assert!(
        intents.take().is_empty(),
        "a value set elsewhere is not the person's edit to report back"
    );
}

#[test]
fn a_checked_rows_title_is_struck_through_and_muted() {
    let face_strikes = |row: &ListRow| row.with_face(|face| face.struck);
    let mut row = ListRow::new("milk", "Buy oat milk").checkbox(false, |_| {});
    assert!(!face_strikes(&row), "an unchecked row's title is plain");
    press_leading_checkbox(&mut row);
    assert!(
        face_strikes(&row),
        "a checked row's title is struck through and drawn in the weaker ink"
    );
    press_leading_checkbox(&mut row);
    assert!(!face_strikes(&row), "a reopened row's title is plain again");
    assert!(
        face_strikes(&ListRow::new("stale", "Old errand").struck()),
        "a row of a removed item is struck the same way"
    );
}

#[test]
fn a_row_holds_the_name_and_description_of_the_control_it_carries() {
    let row = ListRow::new("sdr-brightness", "Brightness")
        .subtitle("External display")
        .secondary("How bright standard content is")
        .description("How bright standard content is")
        .trailing(Box::new(crate::widget::Toggle::bound("sdr", "sdr")));
    assert_eq!(
        Widget::accessibility_child_name(&row, 1).as_deref(),
        Some("Brightness")
    );
    assert_eq!(
        Widget::accessibility_child_description(&row, 1).as_deref(),
        Some("How bright standard content is, External display")
    );
    assert_eq!(Widget::accessibility_child_name(&row, 0), None);

    let plain = ListRow::new("path", "Home");
    assert_eq!(Widget::accessibility_child_name(&plain, 1), None);
    assert_eq!(Widget::accessibility_child_description(&plain, 1), None);
}

#[test]
fn an_author_cell_holds_the_person_and_the_writer_beside_them() {
    let face = ListRowFace::new("terms.txt")
        .meta("Edited")
        .author(RowAuthor {
            person: "Ada",
            identity: "ada@example.com",
            image: None,
            writer: Some(RowWriter {
                label: "Claude Code",
                hue: PresenceHue::Violet,
            }),
        });
    let rect = Rect::new(0.0, 0.0, 520.0, 36.0);
    let placed = geometry(&face, rect);
    let slot = placed
        .author
        .expect("the author cell has a slot of its own");
    assert!(
        slot.width() > 100.0,
        "the cell holds the person and the writer: {slot:?}"
    );
    assert!(
        slot.x1 <= rect.x1,
        "the cell stays inside the row: {slot:?}"
    );
    assert!(
        placed.avatar.is_none(),
        "the cell is the face, never a second one beside it"
    );
    assert!(
        placed.text_right < slot.x0,
        "the title ends before the cell: text to {}, cell at {}",
        placed.text_right,
        slot.x0
    );

    // The writer's chip makes the cell wider than the person alone.
    let alone = ListRowFace::new("terms.txt").author(RowAuthor {
        person: "Ada",
        identity: "ada@example.com",
        image: None,
        writer: None,
    });
    let mine = geometry(&alone, rect)
        .author
        .expect("a person alone still has the cell");
    assert!(
        mine.width() < slot.width(),
        "the writer's chip adds width: {mine:?} against {slot:?}"
    );
}

/// A person double-clicks a file or a contact by habit: what opens opens once, while a row
/// that changes something counts each click.
#[test]
fn a_double_click_opens_once_and_changes_twice() {
    let counted = |row: fn(Rc<Cell<u32>>) -> ListRow| {
        let hits = Rc::new(Cell::new(0));
        let rows: Element = Box::new(
            Column::new()
                .with_no_padding()
                .push(Box::new(row(Rc::clone(&hits)))),
        );
        let mut harness = Driver::new(crate::State::new(), rows, (400, 200));
        let [x, y, w, h] = find(&harness.inspection().root, "ListRow")
            .expect("row laid out")
            .rect;
        harness.double_click_at(x + w / 2.0, y + h / 2.0);
        let double = hits.get();
        // Two clicks apart in time, or in place, are two clicks.
        crate::gesture::with_capture_multi_click_interval(
            crate::time::Duration::from_millis(0),
            || harness.click_at(x + w / 2.0, y + h / 2.0),
        );
        harness.double_click_at(x + 10.0, y + h / 2.0);
        harness.double_click_at(x + w - 10.0, y + h / 2.0);
        (double, hits.get())
    };
    let opened = counted(|hits| {
        ListRow::new("file-plan", "plan.csv").on_open(move || hits.set(hits.get() + 1))
    });
    assert_eq!(
        opened,
        (1, 4),
        "a double-click opens once; clicks apart open each"
    );
    let changed = counted(|hits| {
        ListRow::new("count", "Count").on_activate(move || hits.set(hits.get() + 1))
    });
    assert_eq!(
        changed,
        (2, 7),
        "a row that changes something counts every click"
    );
}

/// A line that stands beside a chooser is the chooser's: the window publishes one control
/// named after the setting, and a press anywhere on the line opens the list it offers.
#[test]
fn a_choice_line_publishes_one_control_and_pressing_the_line_opens_its_list() {
    use crate::headless::ControlInput;
    use crate::widget::Dropdown;

    let chosen = crate::LocalState::new("100%".to_string());
    let line: Element = Box::new(Column::new().with_no_padding().push(Box::new(
        ListRow::new("row", "Scale").trailing(Box::new(Dropdown::from_state(
            vec!["100%".to_string(), "125%".to_string()],
            &chosen,
        ))),
    )));
    let mut harness = Driver::new(crate::State::new(), line, (480, 240));

    let named: Vec<_> = harness
        .controls()
        .into_iter()
        .filter(|control| control.name.as_deref() == Some("Scale"))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "one control is named after the setting: {named:?}"
    );
    let opener = &named[0];
    assert!(
        opener.verbs.iter().any(|verb| verb == "activate"),
        "the chooser is pressed to open its list: {opener:?}"
    );

    // The chooser stands at the line's trailing edge; the press is on the words.
    let [x, y, _w, h] = find(&harness.inspection().root, "ListRow")
        .expect("the line is laid out")
        .rect;
    harness.click_at(x + 20.0, y + h / 2.0);
    harness.relayout();
    let options: Vec<_> = harness
        .controls()
        .into_iter()
        .filter(|control| control.name.as_deref() == Some("125%"))
        .collect();
    assert_eq!(options.len(), 1, "the line's press opened its list");
    harness
        .operate_control(&options[0].id, &ControlInput::Press)
        .expect("an option is chosen by a press");
    assert_eq!(chosen.get(), "125%");
}

/// A Viewer's click on a row's check whose action their grants refuse: the press is sent, the
/// workspace refuses it and the window says why, and the box keeps its mark.
#[test]
fn a_refused_check_says_why_and_keeps_its_mark() {
    let ran = Rc::new(Cell::new(0));
    let counted = Rc::clone(&ran);
    let root = crate::widget::effect_fixture::sends_through(
        move || counted.set(counted.get() + 1),
        |binding| {
            let mut row = ListRow::new("send", "Send").checkbox(false, |_| {});
            row.check_action = Some(ActionEmitter::new(binding));
            Box::new(row)
        },
    );
    let mut driver = Driver::new_with_theme(crate::State::new(), root, (600, 400), Theme::dark());
    driver.connect_peer();
    driver.set_permission_ctx(::collaboration::permissions::PermissionContext::member(
        &::collaboration::permissions::MemberId::person("ada"),
        ::collaboration::permissions::Role::Viewer,
    ));
    driver.relayout();
    let the_check = |driver: &Driver| {
        driver
            .controls()
            .into_iter()
            .find(|control| {
                control
                    .name
                    .as_deref()
                    .is_some_and(|name| name.contains("Mark"))
            })
            .expect("the row shows its check")
    };
    let check = the_check(&driver);
    let name = check.name.clone().unwrap_or_default();
    let marked = |driver: &Driver| {
        driver
            .semantic_tree()
            .find_by_name(&name)
            .and_then(|node| node.state.checked)
    };
    assert_eq!(marked(&driver), Some(false));
    let at = check
        .bounds
        .expect("the check stands where it is drawn")
        .center();
    driver.click_at(at.x, at.y);
    driver.relayout();
    assert_eq!(ran.get(), 0, "a Viewer's check sends nothing");
    assert!(
        driver.refusal_said().is_some(),
        "the check is refused out loud: {:?}",
        driver.control_failures()
    );
    assert_eq!(marked(&driver), Some(false), "the box keeps its mark");
}
