//! What an edit answers through the door an agent uses: the ids of the
//! marks it made, the area it selected, and a warning when what it drew
//! cannot be seen; and a mask that goes with its layer's marks as they move.
#![allow(
    clippy::float_cmp,
    clippy::panic,
    reason = "test assertions over exact, hand-written fixtures"
)]

use std::cell::RefCell;
use std::rc::Rc;

use canvas::App;
use canvas::render::Renderer;
use serde_json::{Value, json};
use ui::prelude::TestApp;

fn call(app: &mut TestApp<App>, action: &str, args: Value) -> Value {
    let effect = args.as_object().and_then(|args| {
        <canvas::resource::ViewOp as ui::tilo_resource::Effect>::decode(action, args).ok()
    });
    if let Some(effect) = effect {
        return app
            .effect(&effect)
            .unwrap_or_else(|error| panic!("{action}: {error}"))
            .value
            .unwrap_or(Value::Null);
    }

    app.call(action, args)
        .unwrap_or_else(|error| panic!("{action}: {error}"))
        .unwrap_or(Value::Null)
}

fn rect(id: &str, [x, y, width, height]: [f64; 4], fill: &str) -> Value {
    json!({"id": id, "shape": {"kind": "rect", "x": x, "y": y, "width": width, "height": height}, "fill": fill})
}

#[test]
fn scoped_palette_counts_all_matching_marks_even_when_none_are_listed() {
    let mut app = TestApp::<App>::new().unwrap();
    call(
        &mut app,
        "add-layer",
        json!({"id":"hut", "name":"Ferry hut"}),
    );
    call(
        &mut app,
        "draw",
        json!({"layer":"hut", "marks":[
            rect("wall", [0.0, 0.0, 10.0, 10.0], "#ABCDEF"),
            {"id":"window", "shape":{"kind":"rect","x":20,"y":0,"width":10,"height":10},
             "fill":"#abcdef", "stroke":"#ABCDEF", "stroke_width":1},
            rect("lamp", [40.0, 0.0, 10.0, 10.0], "#ffcc0080")
        ]}),
    );
    call(&mut app, "add-layer", json!({"id":"sea", "name":"Water"}));
    call(
        &mut app,
        "draw",
        json!({"layer":"sea", "marks":[rect("water", [0.0, 20.0, 60.0, 10.0], "#123456")]}),
    );
    let palette = |app: &mut TestApp<App>| {
        call(
            app,
            "inspect",
            json!({"layer":"hut", "limit":0,"palette":true}),
        )
    };
    let before = palette(&mut app);
    assert_eq!(before["listed"], 0);
    assert_eq!(
        before["palette"],
        json!([
            {"colour":"#abcdef","marks":2}, {"colour":"#ffcc0080","marks":1}
        ])
    );
    let region = call(
        &mut app,
        "inspect",
        json!({"layer":"hut", "region":[0,0,10,10],"limit":0,"palette":true}),
    );
    assert_eq!(region["palette"], json!([{"colour":"#abcdef","marks":1}]));
    call(
        &mut app,
        "edit-marks",
        json!({"ids":["wall"],"fill":"#112233"}),
    );
    assert_eq!(palette(&mut app)["palette_colours"], 3);
    call(&mut app, "undo", json!({}));
    assert_eq!(palette(&mut app)["palette"], before["palette"]);
    call(&mut app, "redo", json!({}));
    assert_eq!(palette(&mut app)["palette_colours"], 3);
    assert!(
        call(&mut app, "inspect", json!({"limit":0}))
            .get("palette")
            .is_none()
    );
}

fn bounds(app: &TestApp<App>, id: &str) -> [f64; 4] {
    let art = app.state();
    let mark = art.mark(id).unwrap_or_else(|| panic!("{id} is drawn"));
    let bounds = mark.bounds();
    [bounds.x0, bounds.y0, bounds.width(), bounds.height()]
}

#[test]
fn paint_and_draw_answer_the_ids_they_made_and_a_group_addresses_a_passage() {
    let mut app = TestApp::<App>::new().unwrap();
    let stroke = |y: f64| json!({"points": [[10, y], [200, y]], "size": 4, "colour": "#202020"});
    let painted = call(
        &mut app,
        "paint",
        json!({"strokes": [stroke(20.0), stroke(40.0), stroke(60.0)], "group": "hatching"}),
    );
    let added = painted["added"].as_array().expect("paint answers its ids");
    assert_eq!(added.len(), 3, "{painted}");
    let art = app.state();
    for id in added {
        let mark = art
            .mark(id.as_str().unwrap())
            .expect("the answered id is a mark");
        assert_eq!(mark.group, "hatching");
    }
    assert!(painted.get("hidden").is_none(), "{painted}");

    // The passage moves as one by its group.
    call(
        &mut app,
        "edit-marks",
        json!({"ids": ["hatching"], "move_by": [0, 100]}),
    );
    let first = bounds(&app, added[0].as_str().unwrap());
    assert!((first[1] - 118.0).abs() < 1.0, "{first:?}");

    let drawn = call(
        &mut app,
        "draw",
        json!({"marks": [{"shape": {"kind": "ellipse", "cx": 5, "cy": 5, "rx": 5, "ry": 5}}]}),
    );
    assert_eq!(drawn["added"].as_array().unwrap().len(), 1, "{drawn}");
}

#[test]
fn select_answers_the_area_it_selected() {
    let mut app = TestApp::<App>::new().unwrap();
    let selected = call(&mut app, "select", json!({"rect": [100, 50, 200, 100]}));
    let summary = &selected["selection"];
    assert_eq!(
        summary["bounds"],
        json!([100.0, 50.0, 200.0, 100.0]),
        "{selected}"
    );
    assert_eq!(summary["area"], json!(20_000.0), "{selected}");
    assert_eq!(summary["parts"], json!(1), "{selected}");

    let two = call(
        &mut app,
        "select",
        json!({"rect": [500, 50, 10, 10], "mode": "add", "feather": 2}),
    );
    assert_eq!(two["selection"]["parts"], json!(2), "{two}");
    assert_eq!(two["selection"]["feather"], json!(2.0), "{two}");

    let none = call(&mut app, "select", json!({"mode": "none"}));
    assert_eq!(none, json!({"selection": null}));
}

#[test]
fn marks_drawn_where_a_mask_or_a_hidden_layer_hides_them_are_reported() {
    let mut app = TestApp::<App>::new().unwrap();
    // A mask that shows only the left half of the layer.
    call(
        &mut app,
        "edit-layer",
        json!({"id": "layer-1", "mask": "hide"}),
    );
    call(&mut app, "select", json!({"rect": [0, 0, 300, 600]}));
    call(
        &mut app,
        "use-selection",
        json!({"do": "mask", "layer": "layer-1"}),
    );
    call(&mut app, "select", json!({"mode": "none"}));

    let shown = call(
        &mut app,
        "draw",
        json!({"layer": "layer-1", "marks": [rect("left", [20.0, 20.0, 100.0, 100.0], "#c03020")]}),
    );
    assert!(shown.get("hidden").is_none(), "{shown}");
    let hidden = call(
        &mut app,
        "draw",
        json!({"layer": "layer-1", "marks": [rect("right", [500.0, 20.0, 100.0, 100.0], "#c03020")]}),
    );
    let warning = hidden["hidden"].as_str().expect("a warning");
    assert!(
        warning.contains("mask") && warning.contains("1 of 1"),
        "{warning}"
    );

    call(
        &mut app,
        "add-layer",
        json!({"id": "notes", "name": "Notes"}),
    );
    call(
        &mut app,
        "edit-layer",
        json!({"id": "notes", "visible": false}),
    );
    let unseen = call(
        &mut app,
        "draw",
        json!({"layer": "notes", "marks": [rect("note", [20.0, 20.0, 10.0, 10.0], "#000000")]}),
    );
    assert!(
        unseen["hidden"].as_str().unwrap().contains("hidden"),
        "{unseen}"
    );
}

#[test]
fn a_linked_mask_moves_with_its_layers_marks_and_an_unlinked_one_stays() {
    let mut app = TestApp::<App>::new().unwrap();
    call(
        &mut app,
        "draw",
        json!({"marks": [rect("photo", [100.0, 100.0, 200.0, 200.0], "#5080c0")]}),
    );
    call(&mut app, "select", json!({"ellipse": [120, 120, 160, 160]}));
    let masked = call(
        &mut app,
        "use-selection",
        json!({"do": "mask", "layer": "layer-1"}),
    );
    let cut = masked["added"][0].as_str().unwrap().to_owned();
    let before = bounds(&app, &cut);

    // The whole layer moves: its cut-out goes with it.
    call(
        &mut app,
        "edit-marks",
        json!({"ids": ["photo"], "move_by": [50, 10]}),
    );
    let after = bounds(&app, &cut);
    assert!(
        (after[0] - before[0] - 50.0).abs() < 0.01,
        "{before:?} {after:?}"
    );
    assert!(
        (after[1] - before[1] - 10.0).abs() < 0.01,
        "{before:?} {after:?}"
    );

    // Scaled about its centre, the mask scales about the same point.
    call(
        &mut app,
        "edit-marks",
        json!({"ids": ["photo"], "scale": 2}),
    );
    let photo = bounds(&app, "photo");
    let scaled = bounds(&app, &cut);
    assert!(
        (scaled[2] - after[2] * 2.0).abs() < 0.5,
        "{after:?} {scaled:?}"
    );
    let centre = |b: [f64; 4]| [b[0] + b[2] / 2.0, b[1] + b[3] / 2.0];
    let (a, b) = (centre(photo), centre(scaled));
    assert!(
        (a[0] - b[0]).abs() < 0.5 && (a[1] - b[1]).abs() < 0.5,
        "{a:?} {b:?}"
    );

    // Only part of the layer moving leaves the mask where it is.
    call(
        &mut app,
        "draw",
        json!({"marks": [rect("title", [0.0, 0.0, 50.0, 20.0], "#000000")]}),
    );
    call(
        &mut app,
        "edit-marks",
        json!({"ids": ["photo"], "move_by": [30, 0]}),
    );
    assert_eq!(bounds(&app, &cut), scaled);

    // Unlinked, it stays put even when all of the layer moves.
    call(
        &mut app,
        "edit-layer",
        json!({"id": "layer-1", "mask": "unlink"}),
    );
    call(
        &mut app,
        "edit-marks",
        json!({"ids": ["photo", "title"], "move_by": [0, 40]}),
    );
    assert_eq!(bounds(&app, &cut), scaled);
    call(
        &mut app,
        "edit-layer",
        json!({"id": "layer-1", "mask": "link"}),
    );
    call(
        &mut app,
        "edit-marks",
        json!({"ids": ["photo", "title"], "move_by": [0, 40]}),
    );
    let linked = bounds(&app, &cut);
    assert!((linked[1] - scaled[1] - 40.0).abs() < 0.01, "{linked:?}");
}

#[test]
fn the_wand_finds_colour_on_one_layer_when_asked() {
    let mut app = TestApp::<App>::new().unwrap();
    call(
        &mut app,
        "draw",
        json!({"marks": [rect("red", [100.0, 100.0, 400.0, 200.0], "#d03020")]}),
    );
    call(&mut app, "add-layer", json!({"id": "top", "name": "Top"}));
    call(
        &mut app,
        "draw",
        json!({"layer": "top", "marks": [rect("blue", [150.0, 150.0, 100.0, 100.0], "#2040c0")]}),
    );
    let renderer = Rc::new(RefCell::new(Renderer::default()));
    canvas::author::see(&renderer, &Rc::new(app.state()));

    let shown = call(
        &mut app,
        "select",
        json!({"at": [200, 200], "tolerance": 0.1}),
    );
    let area = shown["selection"]["area"].as_f64().unwrap();
    assert!(
        (area - 10_000.0).abs() < 400.0,
        "the blue square as shown: {shown}"
    );

    let alone = call(
        &mut app,
        "select",
        json!({"at": [200, 200], "tolerance": 0.1, "layer": "layer-1"}),
    );
    let area = alone["selection"]["area"].as_f64().unwrap();
    assert!(
        (area - 80_000.0).abs() < 1_500.0,
        "the red layer alone: {alone}"
    );

    let error = app
        .call("select", json!({"at": [200, 200], "layer": "nowhere"}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("layer-1") && error.contains("top"),
        "{error}"
    );
}
