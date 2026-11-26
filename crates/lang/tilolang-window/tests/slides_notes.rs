//! A deck's speaker notes being typed as another slide is chosen: the editor that held the
//! keyboard turns to the chosen slide's notes, and lets the keyboard go, so what the person
//! types after choosing never lands in notes they did not choose.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::{Path, PathBuf};
use std::rc::Rc;

use tilolang_tilo::runtime::{Actor, Value};
use tilolang_window::{Opening, Packages, Window};

/// The app's own name, as its package declares it.
const APP: &str = "so.tilo.slides";

/// A wide window, where the rail, the slide and its properties stand side by side.
const WINDOW: (u32, u32) = (1280, 800);

/// The app compiled from `apps/tl/slides`, as `tilolang build` compiles it, and the platform's
/// packages it reads.
fn packages() -> Rc<Packages> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/slides");
    let search: Vec<PathBuf> = vec![dir.parent().expect("apps/tl").to_path_buf()];
    let (built, platform) = tilolang_tilo::catalogue::compile(
        &dir,
        &search,
        &mut tilolang_tilo::tilolang::OsEntropy::default(),
    )
    .expect("the app compiles");
    Rc::new(Packages::new(
        Rc::new(built.into_iter().map(|package| package.compiled).collect()),
        platform,
    ))
}

/// The window at `WINDOW`, laid out and at rest.
fn open() -> Window {
    tilo_app_window::i18n::ensure();
    let mut window = Window::open(APP, &packages(), Opening::new(WINDOW, tilo_ui_base::Theme::light()))
        .expect("the window opens");
    settle(&mut window);
    window
}

/// Let the window and its instance answer each other and the layouts that follow, as a live
/// window does after an input.
fn settle(window: &mut Window) {
    window.exchange();
    window.harness.relayout();
    window.exchange();
}

/// Every accessible node of the window, as a screen reader reads it.
fn accessible(window: &Window) -> Vec<tilo_ui_base::a11y::AccessNode> {
    let snapshot = window.harness.inspection();
    snapshot
        .root
        .nodes()
        .into_iter()
        .filter_map(|node| node.semantics.as_ref())
        .flat_map(tilo_ui_base::a11y::AccessNode::nodes)
        .cloned()
        .collect()
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
        .find(|node| node.role == tilo_ui_base::a11y::Role::ListItem && node.name.as_deref() == Some(number))
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
