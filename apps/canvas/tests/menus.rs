//! What a right-click offers, used as a person uses it: the menu over a
//! layer's row renames it, the one over the canvas acts on what it lands on.

use canvas::App;
use serde_json::json;
use ui::prelude::TestApp;
use ui::winit::event::{ElementState, MouseButton};

fn right_click(app: &mut TestApp<App>, (x, y): (f64, f64)) {
    let harness = app.harness();
    harness.send(&ui::synthetic_input::cursor_moved(x, y));
    for state in [ElementState::Pressed, ElementState::Released] {
        harness.send(&ui::synthetic_input::mouse_input(state, MouseButton::Right));
    }
}

#[test]
fn rename_from_a_layer_rows_menu_takes_the_typed_name() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("add-layer", json!({"name": ""})).unwrap();
    let art = app.state();
    let position = art
        .layers
        .iter()
        .rev()
        .position(|layer| layer.id == "layer-1")
        .unwrap();
    app.harness()
        .perform_semantic_named(
            "canvas-layer-tree",
            Some(&format!("key:{}", position + 1)),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    let row = app.harness().targets()["canvas-layer-name-layer-1"];
    right_click(&mut app, (row.center().x, row.center().y));
    // The menu opens with Rename ready for the keyboard.
    app.harness().key("Enter");
    app.harness().type_text("Hills");
    app.harness().key("Enter");
    let names: Vec<String> = app
        .state()
        .layers
        .iter()
        .map(|layer| layer.name.clone())
        .collect();
    assert!(names.contains(&"Hills".to_owned()), "{names:?}");
}

#[test]
fn a_right_click_on_a_mark_picks_it_and_the_menu_duplicates_it() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "draw",
        json!({"marks": [{"id": "sun", "name": "Sun", "shape": {"kind": "rect", "x": 0, "y": 0, "width": 1200, "height": 900}, "fill": "#f6b73c"}]}),
    )
    .unwrap();
    app.harness().key("v");
    let canvas = app.harness().targets()["canvas"];
    right_click(&mut app, (canvas.center().x, canvas.center().y));
    // Cut, Copy, Paste, Duplicate: the fourth entry.
    for _ in 0..3 {
        app.harness().key("ArrowDown");
    }
    app.harness().key("Enter");
    // The menu presses the canvas's own keys for it; the frame this very key
    // asked for carries them out.
    assert_eq!(app.state().marks.len(), 2, "a copy of the sun");
}

#[test]
fn rename_chosen_with_the_pointer_takes_the_typed_name() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("add-layer", json!({"name": ""})).unwrap();
    let art = app.state();
    let position = art
        .layers
        .iter()
        .rev()
        .position(|layer| layer.id == "layer-1")
        .unwrap();
    app.harness()
        .perform_semantic_named(
            "canvas-layer-tree",
            Some(&format!("key:{}", position + 1)),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    let row = app.harness().targets()["canvas-layer-name-layer-1"];
    right_click(&mut app, (row.center().x, row.center().y));
    assert!(
        app.harness()
            .targets()
            .keys()
            .any(|path| path.ends_with("/ContextMenu")),
        "the menu opened"
    );
    // The menu takes typing: its rows narrow to Rename, and Enter runs it.
    app.harness().type_text("Rename");
    app.harness().key("Enter");
    app.harness().type_text("Hills");
    app.harness().key("Enter");
    let names: Vec<String> = app
        .state()
        .layers
        .iter()
        .map(|layer| layer.name.clone())
        .collect();
    assert!(names.contains(&"Hills".to_owned()), "{names:?}");
}

#[test]
fn a_sentence_typed_into_a_context_menu_opens_the_box_about_where_it_was_opened() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "draw",
        json!({"marks": [{"id": "sun", "name": "Sun", "shape": {"kind": "rect", "x": 0, "y": 0, "width": 1200, "height": 900}, "fill": "#f6b73c"}]}),
    )
    .unwrap();
    app.harness().key("v");
    let _ = ui::shared_selection::take_send_to();
    // The toolkit's menu offers it while a desktop can send from it, with
    // no code of the app's own.
    ui::shared_selection::set_send_host_available(true);
    let canvas = app.harness().targets()["canvas"];
    let at = (canvas.center().x, canvas.center().y);
    right_click(&mut app, at);
    // A sentence none of its rows answers goes into the box, about where
    // the menu was opened.
    app.harness().type_text("make the sun warmer");
    app.harness().key("Enter");
    let box_ = ui::shared_selection::box_open();
    ui::shared_selection::set_send_host_available(false);
    assert!(box_, "the box is open with the sentence in it");
    assert!(
        ui::shared_selection::take_send_to().is_empty(),
        "nothing sent yet: the person sends it"
    );
    assert_eq!(app.state().marks.len(), 1, "no app entry ran");
}
