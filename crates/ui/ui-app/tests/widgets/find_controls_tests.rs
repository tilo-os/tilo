//! The find bar's controls over a window's find: what the person sees and what each act asks
//! the app for.

use tilo_ui_app::component_testing::fired;
use tilo_ui_app::headless::Driver;
use tilo_ui_app::widget::find_controls::{FindControls, FindControlsProps};
use tilo_ui_base::Intents;
use tilo_ui_base::component::{Described, Event, Fired, Intent, Value};
use tilo_ui_base::theme::Theme;

/// The controls over a window's find, on a window of `width`: what the person sees and what
/// each act asks the app for.
fn controls(width: u32, set: impl FnOnce(&mut FindControlsProps)) -> (Driver, Intents<Fired>) {
    let queue = Intents::default();
    let mut props = FindControlsProps::default();
    props.query.state.set("Ship".to_owned());
    props.current = Some(2);
    props.total = Some(5);
    props.acts_on = Some("document".to_owned());
    props.window = Some("Release planning".to_owned());
    props.app = Some("Notes".to_owned());
    props.changed = Event::View(Intent::new(&queue, "find", "changed"));
    props.submit = Event::View(Intent::new(&queue, "find", "submit"));
    props.step = Event::View(Intent::new(&queue, "find", "step"));
    props.option = Event::View(Intent::new(&queue, "find", "option"));
    props.replace_open = Event::View(Intent::new(&queue, "find", "replace_open"));
    props.replace = Event::View(Intent::new(&queue, "find", "replace"));
    props.replace_all = Event::View(Intent::new(&queue, "find", "replace_all"));
    props.widen = Event::View(Intent::new(&queue, "find", "widen"));
    props.find_chord = Event::View(Intent::new(&queue, "find", "find_chord"));
    props.close = Event::View(Intent::new(&queue, "find", "close"));
    set(&mut props);
    let find = FindControls::from_props("find", props);
    let mut harness =
        Driver::for_root(tilo_ui_app::State::new(), find, (width, 48), Theme::light());
    harness.relayout();
    (harness, queue)
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

/// The field shows the match the document stands on of how many it found, and says so when
/// it found none.
#[test]
fn the_field_shows_the_match_of_how_many() {
    let (harness, _) = controls(900, |_| {});
    assert!(
        harness.a11y_node_named("2 of 5").is_some(),
        "the count reads in the bar: {:?}",
        harness.control_failures()
    );
    let (harness, _) = controls(900, |props| props.total = Some(0));
    assert!(
        harness.a11y_node_named("No matches").is_some(),
        "{:?}",
        harness.control_failures()
    );
}

/// A document's find is stepped both ways and closed.
#[test]
fn a_documents_matches_step_both_ways_and_close() {
    let (mut harness, queue) = controls(900, |_| {});
    harness.click_named("find.next").expect("the next match");
    assert_eq!(
        queue.take(),
        [fired("find", "step", Some(Value::Bool(true)))]
    );
    harness
        .click_named("find.previous")
        .expect("the one before");
    assert_eq!(
        queue.take(),
        [fired("find", "step", Some(Value::Bool(false)))]
    );
    harness.click_named("find.close").expect("the close");
    assert_eq!(queue.take(), [fired("find", "close", None)]);
}

/// A list narrows and counts its rows, and has nothing to step.
#[test]
fn a_list_narrows_without_a_stepper() {
    let (harness, _) = controls(900, |props| {
        props.acts_on = Some("list".to_owned());
        props.current = Some(12);
        props.total = Some(40);
    });
    assert!(harness.a11y_node_named("Next match").is_none());
    assert!(
        harness.a11y_node_named("12 of 40").is_some(),
        "a narrowed list says how many it left: {:?}",
        harness.control_failures()
    );
}

/// What the options menu offers, and what each row reads as chosen.
fn offered(harness: &mut Driver) -> Vec<(String, Option<bool>)> {
    let mut find = harness.root_mut::<FindControls>();
    let menu = find.options_menu().expect("the options menu");
    assert!(menu.is_open(), "the options stand open");
    let menu = menu.menu_mut();
    let mut out = Vec::new();
    for item in menu.items() {
        if let Some(label) = item.label() {
            out.push((label.to_owned(), item.checked()));
        }
    }
    out
}

/// What the content allows is what the options offer, checked as the find runs them.
#[test]
fn the_options_offer_what_the_content_allows() {
    let (mut harness, _) = controls(900, |props| {
        props.replaces = true;
        props.regex = true;
        props.match_case = true;
    });
    harness.click_named("find.options").expect("the options");
    harness.relayout();
    let rows = offered(&mut harness);
    let labels: Vec<&str> = rows.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(
        labels,
        ["Match case", "Whole word", "Regular expression"],
        "an expression where the content offers one, and no hex for text"
    );
    assert_eq!(
        rows[0].1,
        Some(true),
        "matching case, as the app runs its find"
    );
    assert_eq!(rows[1].1, Some(false));

    let (mut harness, _) = controls(900, |props| {
        props.hex = true;
        props.match_hex = true;
    });
    harness.click_named("find.options").expect("the options");
    harness.relayout();
    let labels: Vec<String> = offered(&mut harness)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(
        labels,
        ["Match case", "Whole word", "Text", "Hex"],
        "text and hex for content of bytes"
    );
    let rows = offered(&mut harness);
    assert_eq!(
        rows.iter()
            .find(|(label, _)| label == "Hex")
            .map(|(_, checked)| *checked),
        Some(Some(true)),
        "hex chosen, as the app reads bytes"
    );
}

/// The menu row `named`, activated as a screen reader activates it.
fn choose(harness: &mut Driver, named: &str) {
    let id = harness
        .a11y_node_named(named)
        .unwrap_or_else(|| panic!("the menu offers {named:?}"));
    harness
        .perform_a11y_action(id, &tilo_ui_base::a11y::Action::Activate)
        .unwrap_or_else(|why| panic!("{named:?} is activated: {why}"));
    harness.relayout();
}

/// The widening steps hand the query to the app's own search and then to everything, and the
/// app is named on the first: one menu of the steps out of this view.
#[test]
fn widening_steps_out_from_the_view() {
    for (row, reach) in [
        ("Search Release planning", "window"),
        ("Search Notes", "app"),
        ("Search everywhere", "everywhere"),
    ] {
        let (mut harness, queue) = controls(900, |_| {});
        harness.click_named("find.widen").expect("the steps out");
        harness.relayout();
        choose(&mut harness, row);
        assert_eq!(
            queue.take(),
            [fired("find", "widen", Some(text(reach)))],
            "choosing {row:?} widens the search to {reach}"
        );
    }
}

/// The replacement stands only once the find is opened to replace; until then the bar offers
/// the way to open it.
#[test]
fn replace_shows_only_once_opened() {
    let (mut harness, queue) = controls(1100, |props| props.replaces = true);
    for part in ["Replace with", "Replace", "Replace all"] {
        assert!(
            harness.a11y_node_named(part).is_none(),
            "a find opened to find alone shows no {part:?}"
        );
    }
    harness
        .click_named("find.replace-open")
        .expect("the way to replace");
    assert_eq!(queue.take(), [fired("find", "replace_open", None)]);
    let (harness, _) = controls(1100, |props| {
        props.replaces = true;
        props.replacing = true;
    });
    for part in ["Replace with", "Replace", "Replace all"] {
        assert!(
            harness.a11y_node_named(part).is_some(),
            "opened to replace, the find shows {part:?}"
        );
    }
}

/// The field the find opened for holds the keyboard, so a person types into the query or, opened
/// to replace, into the replacement without reaching for the pointer.
#[test]
fn the_open_field_holds_the_keyboard() {
    let focused = |harness: &Driver| {
        harness
            .focused_access_node()
            .and_then(|node| node.name.clone())
    };
    let (harness, _) = controls(1100, |props| {
        props.replaces = true;
        props.placeholder = Some("Search this window".to_owned());
    });
    assert_eq!(
        focused(&harness).as_deref(),
        Some("Search this window"),
        "a find opened to find holds the query"
    );
    let (harness, _) = controls(1100, |props| {
        props.replaces = true;
        props.replacing = true;
    });
    assert_eq!(
        focused(&harness).as_deref(),
        Some("Replace with"),
        "opened to replace, the replacement holds it"
    );
    let (harness, _) = controls(1100, |props| {
        props.replaces = true;
        props.replacing = true;
        props.placeholder = Some("Search this window".to_owned());
        props.query.state.set(String::new());
    });
    assert_eq!(
        focused(&harness).as_deref(),
        Some("Search this window"),
        "opened to replace with nothing found yet, the query holds it so the person says what to find"
    );
}

/// With nothing to step to, the stepper and the replacement read as unavailable and run nothing;
/// with matches, both stand ready.
#[test]
fn nothing_to_step_to_disables_the_stepper() {
    let disabled = |harness: &Driver, named: &str| {
        let id = harness
            .a11y_node_named(named)
            .unwrap_or_else(|| panic!("the bar shows {named:?}"));
        harness
            .a11y_nodes()
            .into_iter()
            .find(|(node, _)| *node == id)
            .is_some_and(|(_, node)| node.is_disabled())
    };
    let (mut harness, queue) = controls(900, |props| {
        props.replaces = true;
        props.replacing = true;
        props.current = Some(0);
        props.total = Some(0);
    });
    for part in ["Next match", "Previous match", "Replace"] {
        assert!(
            disabled(&harness, part),
            "with nothing found, {part:?} reads as unavailable"
        );
    }
    // Each control stands where it always does, and pressing it runs nothing.
    harness
        .click_named("find.next")
        .expect("the stepper stands");
    harness
        .click_named("find.replace")
        .expect("the replacement stands");
    assert!(queue.take().is_empty(), "no act reaches the app");
    let (mut harness, queue) = controls(900, |props| {
        props.replaces = true;
        props.replacing = true;
    });
    for part in ["Next match", "Previous match", "Replace"] {
        assert!(
            !disabled(&harness, part),
            "with matches, {part:?} stands ready"
        );
    }
    harness
        .click_named("find.next")
        .expect("a match to step to");
    assert_eq!(
        queue.take(),
        [fired("find", "step", Some(Value::Bool(true)))]
    );
}

/// Shift+Enter steps to the match before, and Find while the field holds the keys widens one
/// step: the two chords the bar's own search always had.
#[test]
fn shift_enter_steps_back_and_find_widens() {
    let (mut harness, queue) = controls(900, |_| {});
    harness.key("Shift+Enter");
    assert_eq!(
        queue.take(),
        [fired("find", "step", Some(Value::Bool(false)))],
        "the match before the one the find stands on"
    );
    harness.key("Ctrl+F");
    assert_eq!(queue.take(), [fired("find", "find_chord", None)]);
}

/// Enter asks again and Escape lets go of the field, as the bar's own search always has.
#[test]
fn enter_asks_and_escape_lets_go() {
    let (mut harness, queue) = controls(900, |_| {});
    harness.type_text("Bug");
    assert!(
        queue.take().iter().any(|fired| fired.event == "changed"),
        "what is typed reaches the app as it is typed"
    );
    harness.key("Enter");
    assert_eq!(
        queue.take(),
        [fired("find", "submit", Some(text("ShipBug")))]
    );
    harness.key("Escape");
    assert_eq!(queue.take(), [fired("find", "close", None)]);
}

/// A find opened to replace carries what is typed in the replacement with the press, for the
/// match and for all of them.
#[test]
fn replacing_carries_what_is_typed() {
    let (mut harness, queue) = controls(1100, |props| {
        props.replaces = true;
        props.replacing = true;
    });
    harness
        .click_named("find.replacement")
        .expect("the replacement's field");
    harness.type_text("Launch");
    harness
        .click_named("find.replace-all")
        .expect("replace all");
    assert_eq!(
        queue.take(),
        [fired("find", "replace_all", Some(text("Launch")))]
    );
    harness
        .click_named("find.replace")
        .expect("replace this one");
    assert_eq!(
        queue.take(),
        [fired("find", "replace", Some(text("Launch")))]
    );
}
