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
            Some(&format!("node:{}", position + 1)),
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
    // The menu presses the canvas's own keys for it; a frame carries them out.
    let _ = app.harness().targets();
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
            Some(&format!("node:{}", position + 1)),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    let row = app.harness().targets()["canvas-layer-name-layer-1"];
    right_click(&mut app, (row.center().x, row.center().y));
    let menu = app
        .harness()
        .targets()
        .into_iter()
        .find(|(path, _)| path.ends_with("/ContextMenu"))
        .map(|(_, rect)| rect)
        .expect("the menu opened");
    // Rename is its first row.
    let rename = ui::vello::kurbo::Rect::new(menu.x0, menu.y0 + 6.0, menu.x1, menu.y0 + 30.0);
    app.harness().click_at(rename.center().x, rename.center().y);
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
