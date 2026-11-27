//! A row's tooltips, presses, marks, checks, menus and choices: each reaches
//! the action the row publishes.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::Bound;
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
    let theme = tilo_ui_base::theme::Theme::dark();
    let message = "Expected `&[String]`, found `&Vec<String>` in the argument to parse_limit";
    let long = ListRow::new("long", message).meta("Ln 14, Col 29");
    let h = Driver::for_root(tilo_ui_app::State::new(), long, (240, 60), theme.clone());
    let row = h.root::<ListRow>();
    let band = row.text_band_for_tests();
    let (rect, text) = Widget::tooltip_target_at(row, band.center()).expect("elided title");
    assert_eq!(text, message);
    assert_eq!(rect, band);

    let short = ListRow::new("short", "Unused import").meta("Ln 2, Col 5");
    let h = Driver::for_root(tilo_ui_app::State::new(), short, (480, 60), theme);
    let row = h.root::<ListRow>();
    assert_eq!(
        Widget::tooltip_target_at(row, row.text_band_for_tests().center()),
        None,
        "a title that fits needs no tooltip"
    );
}

#[test]
fn a_rows_box_presses_as_far_as_the_pointer_needs() {
    use tilo_ui_base::interaction::InteractionEnvironment;
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
            tilo_ui_app::State::new(),
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
        let lift = row_rect.inflate(tilo_ui_base::chrome::row_bleed(&theme), 0.0);
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

#[test]
fn a_view_row_is_a_certified_action_a_click_and_an_agent_both_reach() {
    let calls = Rc::new(Cell::new(0));
    let invoked = Rc::clone(&calls);
    let row = ListRow::new("contact-ada", "Ada Lovelace")
        .secondary("ada@example.org")
        .on_open(move || invoked.set(invoked.get() + 1));
    let mut driver = Driver::new(
        tilo_ui_app::State::new(),
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
                .starts_with(tilo_ui_app::headless::CONTROL_RESOURCE_PREFIX)
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
        .perform_named("contact-ada", &tilo_ui_base::a11y::Action::Activate)
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
    let mut harness = Driver::for_root(tilo_ui_app::State::new(), row, (420, 80), Theme::dark());
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
    let mut harness = Driver::for_root(tilo_ui_app::State::new(), kept, (420, 80), Theme::dark());
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
            tilo_ui_base::platform::Shortcut::from("Ctrl+Enter"),
            Rc::new(move || checked_too.set(checked_too.get() + 1)) as Rc<dyn Fn()>,
        )]);
    row.set_focus(true);
    let mut harness = Driver::for_root(tilo_ui_app::State::new(), row, (420, 80), Theme::dark());
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

#[test]
fn a_press_on_the_leading_box_marks_the_row() {
    let toggled = Rc::new(Cell::new(None));
    let seen = Rc::clone(&toggled);
    let row = ListRow::new("milk", "Buy oat milk").checkbox(false, move |on| seen.set(Some(on)));
    let mut harness = Driver::new(tilo_ui_app::State::new(), Box::new(row), (400, 60));
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

#[test]
fn a_bound_rows_checkbox_follows_a_check_made_elsewhere() {
    let state = tilo_ui_base::LocalState::new(false);
    let intents = tilo_ui_base::Intents::<Fired>::default();
    let row = bound_row(&state, &intents);
    let mut harness = Driver::for_root(tilo_ui_app::State::new(), row, (400, 60), Theme::dark());
    harness.poll_frame().expect("the first frame draws");
    assert_eq!(
        harness
            .root::<ListRow>()
            .check_level_for_tests()
            .expect("drawn"),
        0.0,
        "it starts unchecked"
    );

    state.set(true);
    harness
        .recompose()
        .expect("the frame that takes the new value");
    harness.poll_frame().expect("the frame that shows it");
    assert_eq!(
        harness
            .root::<ListRow>()
            .check_level_for_tests()
            .expect("drawn"),
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
        harness
            .root::<ListRow>()
            .check_level_for_tests()
            .expect("drawn"),
        0.0,
        "a check taken away elsewhere clears the box"
    );
    assert!(
        intents.take().is_empty(),
        "a value set elsewhere is not the person's edit to report back"
    );
}

#[test]
fn a_double_click_opens_once_and_changes_twice() {
    let counted = |row: fn(Rc<Cell<u32>>) -> ListRow| {
        let hits = Rc::new(Cell::new(0));
        let rows: Element = Box::new(
            Column::new()
                .with_no_padding()
                .push(Box::new(row(Rc::clone(&hits)))),
        );
        let mut harness = Driver::new(tilo_ui_app::State::new(), rows, (400, 200));
        let [x, y, w, h] = find(&harness.inspection().root, "ListRow")
            .expect("row laid out")
            .rect;
        harness.double_click_at(x + w / 2.0, y + h / 2.0);
        let double = hits.get();
        // Two clicks apart in time, or in place, are two clicks.
        tilo_ui_base::gesture::with_capture_multi_click_interval(
            std::time::Duration::from_millis(0),
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

#[test]
fn a_choice_line_publishes_one_control_and_pressing_the_line_opens_its_list() {
    use tilo_ui_app::headless::ControlInput;
    use tilo_ui_app::widget::Dropdown;

    let chosen = tilo_ui_base::LocalState::new("100%".to_string());
    let line: Element = Box::new(Column::new().with_no_padding().push(Box::new(
        ListRow::new("row", "Scale").trailing(Box::new(Dropdown::from_state(
            vec!["100%".to_string(), "125%".to_string()],
            &chosen,
        ))),
    )));
    let mut harness = Driver::new(tilo_ui_app::State::new(), line, (480, 240));

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

#[test]
fn a_refused_check_says_why_and_keeps_its_mark() {
    let ran = Rc::new(Cell::new(0));
    let counted = Rc::clone(&ran);
    let root = tilo_ui_app::widget::effect_fixture::sends_through(
        move || counted.set(counted.get() + 1),
        |binding| {
            let mut row = ListRow::new("send", "Send").checkbox(false, |_| {});
            row.set_check_action_for_tests(Some(ActionEmitter::new(tilo_ui::Bind::bound(binding))));
            Box::new(row)
        },
    );
    let mut driver =
        Driver::new_with_theme(tilo_ui_app::State::new(), root, (600, 400), Theme::dark());
    driver.connect_peer();
    driver.decide_shown_actions(
        tilo_ui::harness_device::HarnessDevice::member_of(
            tilo_permissions::LOCAL_WORKSPACE,
            &tilo_permissions::MemberId::person("ada"),
            tilo_permissions::Role::Viewer,
        )
        .host(),
    );
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
    let marked = |driver: &mut Driver| {
        driver
            .semantic_tree()
            .find_by_name(&name)
            .and_then(|node| node.state.checked)
    };
    assert_eq!(marked(&mut driver), Some(false));
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
    assert_eq!(marked(&mut driver), Some(false), "the box keeps its mark");
}

#[test]
fn a_check_fills_in_over_frames_unless_the_row_is_still() {
    let by_pointer = |harness: &mut Driver| {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        harness
            .root_mut::<ListRow>()
            .toggle_leading_check_for_tests();
    };
    let state = tilo_ui_base::LocalState::new(false);
    let intents = tilo_ui_base::Intents::<Fired>::default();
    for (name, levels) in [
        (
            "plain",
            check_levels(
                ListRow::new("milk", "Buy oat milk").checkbox(false, |_| {}),
                by_pointer,
            ),
        ),
        (
            "bound",
            check_levels(bound_row(&state, &intents), |harness| {
                let [x, y, _, h] = find(&harness.inspection().root, "ListRow")
                    .expect("the row is laid out")
                    .rect;
                harness.click_at(x + 12.0, y + h / 2.0);
            }),
        ),
    ] {
        assert!(
            levels
                .iter()
                .filter(|level| **level > 0.0 && **level < 1.0)
                .count()
                >= 3,
            "{name}: the box fills over several frames rather than popping: {levels:?}"
        );
        assert!(
            levels.windows(2).all(|pair| pair[1] >= pair[0] - 1e-6),
            "{name}: {levels:?}"
        );
        // The fill eases to its marked level and stops there: the last frames hold still.
        assert!(
            (levels[23] - 1.0).abs() < 2e-3,
            "{name}: it settles marked: {levels:?}"
        );
    }
    assert!(state.get(), "the binding took the person's check");
    let fired = intents.take();
    assert_eq!(fired.len(), 1, "the check is reported once");
    assert_eq!(
        (fired[0].key.as_str(), fired[0].event.as_str()),
        ("milk", "check")
    );

    let still = ListRow::new("milk", "Buy oat milk")
        .checkbox(false, |_| {})
        .with_feel(tilo_ui_base::motion::Feel::Still);
    assert_eq!(
        check_levels(still, by_pointer)[1],
        1.0,
        "a still row's mark lands at once"
    );
}

#[test]
fn a_row_tooltip_reads_anywhere_the_row_has_nothing_closer_to_say() {
    let row = ListRow::new("used", "Deploy").tooltip("Used 3 times");
    let h = Driver::for_root(tilo_ui_app::State::new(), row, (480, 60), Theme::dark());
    let row = h.root::<ListRow>();
    let (_, text) =
        Widget::tooltip_target_at(row, row.text_band_for_tests().center()).expect("a tooltip");
    assert_eq!(text, "Used 3 times");
}

fn bound_row(
    state: &tilo_ui_base::LocalState<bool>,
    intents: &tilo_ui_base::Intents<Fired>,
) -> ListRow {
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

/// The level a row's check fill stands at over 24 frames after `press` at 100 ms.
fn check_levels(row: ListRow, press: impl FnOnce(&mut Driver)) -> Vec<f64> {
    use std::time::{Duration, Instant};
    use tilo_ui_app::time::testing::with_monotonic_time;
    let start = Instant::now();
    let mut harness = with_monotonic_time(start, || {
        Driver::for_root(tilo_ui_app::State::new(), row, (400, 60), Theme::dark())
    });
    with_monotonic_time(start, || harness.poll_frame()).expect("the first frame draws");
    assert_eq!(
        harness
            .root::<ListRow>()
            .check_level_for_tests()
            .expect("drawn"),
        0.0
    );
    let pressed = start + Duration::from_millis(100);
    with_monotonic_time(pressed, || press(&mut harness));
    (1..=24_u64)
        .map(|frame| {
            with_monotonic_time(pressed + Duration::from_millis(frame * 16), || {
                harness.poll_frame()
            })
            .expect("a frame");
            f64::from(
                harness
                    .root::<ListRow>()
                    .check_level_for_tests()
                    .expect("drawn"),
            )
        })
        .collect()
}

fn m_icon(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.icon_control)
}

fn boxed_rect(access: &tilo_ui_base::a11y::AccessNode) -> Rect {
    access
        .children
        .iter()
        .find(|child| child.role == tilo_ui_base::a11y::Role::Checkbox)
        .expect("the row's box is a control of its own")
        .bounds
        .expect("a box a reader presses says where it stands")
        .to_rect()
}
