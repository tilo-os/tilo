//! A deck's speaker notes being typed as another slide is chosen: the editor that held the
//! keyboard turns to the chosen slide's notes, and lets the keyboard go, so what the person
//! types after choosing never lands in notes they did not choose.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use support::{accessible, settle};

use tilolang_tilo::runtime::{Actor, Value};
use tilolang_window::Window;

/// The app's own name, as its package declares it.
const APP: &str = "so.tilo.slides";

/// A wide window, where the rail, the slide and its properties stand side by side.
const WINDOW: (u32, u32) = (1280, 800);

/// The window at `WINDOW`, laid out and at rest.
fn open() -> Window {
    support::open_app(
        APP,
        &support::packages_of("slides"),
        WINDOW,
        tilo_ui_base::Theme::light(),
    )
}

/// Press the control `name` where it stands, as a person does.
fn press(window: &mut Window, name: &str) {
    let at = accessible(window)
        .into_iter()
        .find(|node| node.name.as_deref() == Some(name) && node.bounds.is_some())
        .map_or_else(
            || {
                let names: Vec<String> = accessible(window)
                    .into_iter()
                    .filter_map(|node| node.name)
                    .collect();
                panic!("the window offers `{name}`: {names:?}")
            },
            |node| node.bounds.expect("the control's bounds"),
        );
    window
        .harness
        .click_at(f64::midpoint(at.x0, at.x1), f64::midpoint(at.y0, at.y1));
    settle(window);
}

/// Press the rail's row numbered `number`, as a person choosing a slide does.
fn press_slide(window: &mut Window, number: &str) {
    let at = accessible(window)
        .into_iter()
        .find(|node| {
            node.role == tilo_ui_base::a11y::Role::ListItem && node.name.as_deref() == Some(number)
        })
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("the rail offers slide {number}"));
    window
        .harness
        .click_at(f64::midpoint(at.x0, at.x1), f64::midpoint(at.y0, at.y1));
    settle(window);
}

/// The bound editor the properties stand in: the notes editor, beside the slide at a wide
/// window's trailing side.
fn click_the_notes(window: &mut Window) {
    let at = accessible(window)
        .into_iter()
        .filter(|node| node.role == tilo_ui_base::a11y::Role::MultilineTextInput)
        .filter_map(|node| node.bounds)
        .filter(|bounds| bounds.x0 > 900.0 && bounds.y1 > bounds.y0)
        .max_by(|a, b| a.y0.total_cmp(&b.y0))
        .unwrap_or_else(|| {
            let names: Vec<(String, Option<tilo_ui_base::a11y::Role>)> = accessible(window)
                .into_iter()
                .map(|node| (node.name.unwrap_or_default(), Some(node.role)))
                .collect();
            panic!("the window offers a notes editor: {names:?}")
        });
    window
        .harness
        .click_at(f64::midpoint(at.x0, at.x1), f64::midpoint(at.y0, at.y1));
    settle(window);
}

/// The deck as the app reads it: its outline, with each slide's notes under it.
fn deck_outline(window: &Window) -> String {
    let host = window.instance.host().expect("the window runs an instance");
    let ty = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .roots
        .first()
        .cloned()
        .expect("the app roots a deck");
    let target = host
        .root_resource(APP, &ty)
        .unwrap_or_else(|why| panic!("the deck root: {why}"));
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .actions
        .iter()
        .find(|action| action.name == "read_deck")
        .expect("the app publishes read_deck")
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (id.as_str(), vec![Value::resource(&target)]),
        )
        .unwrap_or_else(|why| panic!("read_deck: {why}"))
    {
        tilolang_runtime::InvokeOutcome::Done(Value::Text(outline)) => outline,
        other => panic!("read_deck answered {other:?}"),
    }
}

/// Typing into the notes, choosing another slide, and typing again: what follows the choice
/// reaches no slide's notes.
#[test]
fn typing_after_choosing_a_slide_does_not_land_in_its_notes() {
    let mut window = open();
    // Two slides: the deck's starter gives way to the first added slide.
    press(&mut window, "Add slide");
    press(&mut window, "Add slide");

    click_the_notes(&mut window);
    assert!(
        window.harness.focused().is_some(),
        "a press in the notes puts the keyboard in them"
    );
    window.harness.type_text("kept");
    settle(&mut window);
    let outline = deck_outline(&window);
    assert!(
        outline.contains("Notes: kept"),
        "what was typed is the shown slide's notes: {outline}"
    );

    // Another slide is chosen in the rail: the notes editor turns to its notes and lets the
    // keyboard go, so what is typed next reaches no slide's notes.
    press_slide(&mut window, "2");
    window.harness.type_text("lost");
    settle(&mut window);
    let outline = deck_outline(&window);
    assert!(
        !outline.contains("lost"),
        "what was typed after the choice reached no slide's notes: {outline}"
    );
    assert!(
        outline.contains("Notes: kept"),
        "the slide the person wrote on keeps its words: {outline}"
    );
}
