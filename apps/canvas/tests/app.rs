//! The app as it runs: the host door, undo, permissions, and the views an
//! agent sees with.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::actions::NewStroke;
use canvas::{App, Artwork};
use serde_json::json;
use ui::prelude::{TestApp, tilo_resource::ActionRef};

fn stroke(colour: &str) -> NewStroke {
    NewStroke {
        id: Some("first".into()),
        name: None,
        points: vec![vec![100.0, 100.0], vec![600.0, 300.0, 0.5]],
        size: 40.0,
        colour: colour.into(),
        opacity: None,
        wet: None,
        params: None,
        ..NewStroke::default()
    }
}

/// A stroke painted through the host door is one undo step, and a viewer
/// may look but not paint.
#[test]
fn painting_is_undoable_and_viewers_cannot_paint() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Artwork::PAINT.with(canvas::actions::Paint {
        layer: None,
        strokes: vec![stroke("#204080")],
        group: None,
    }))
    .expect("paint");
    assert_eq!(app.state().marks.len(), 1);
    assert!(app.undo(), "the passage is one undo step");
    assert!(app.state().marks.is_empty());

    let mut viewer = TestApp::<App>::with_role(ui::Role::Viewer).expect("the app opens");
    assert!(
        viewer
            .invoke(Artwork::PAINT.with(canvas::actions::Paint {
                layer: None,
                strokes: vec![stroke("#204080")],
                group: None
            }))
            .is_err()
    );
    assert!(
        viewer
            .effect(&canvas::resource::ViewOp::Look {
                look: canvas::resource::Look::default()
            })
            .is_ok(),
        "a viewer may look"
    );
}

/// `look` through the host door answers with a stored JPEG and the revision
/// it shows; the revision moves when the artwork changes, so a caller can
/// tell a stale look from a fresh one. `inspect` names the marks by id.
#[test]
fn look_and_inspect_answer_through_the_host_door() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let look = canvas::resource::ViewOp::Look {
        look: canvas::resource::Look {
            size: Some(256),
            ..Default::default()
        },
    };
    let before = app.effect(&look).expect("a look finishes as a job");
    let value = before.value.clone().unwrap();
    assert_eq!(value["width"], 256, "{value}");
    let address = before
        .resources
        .first()
        .expect("the picture is stored")
        .clone();
    assert_eq!(value["content"], json!(address));
    let hash = ui::tilo_actions::content_hash(&address).unwrap();
    let jpeg = app.content().read(hash).expect("readable");
    assert!(jpeg.starts_with(&[0xff, 0xd8, 0xff]));

    app.invoke(Artwork::PAINT.with(canvas::actions::Paint {
        layer: None,
        strokes: vec![stroke("pigment:ultramarine")],
        group: None,
    }))
    .expect("paint");
    let after = app.effect(&look).unwrap();
    assert_ne!(
        after.value.unwrap()["revision"],
        value["revision"],
        "a new revision after painting"
    );

    let inspect: ActionRef<Artwork, ()> = ActionRef::new("inspect", &[]);
    let listed = app
        .operate(inspect.invoke())
        .unwrap()
        .unwrap()
        .value
        .unwrap();
    assert_eq!(listed["marks_bottom_first"][0]["id"], "first", "{listed}");
    assert_eq!(listed["layers_top_first"][0]["marks"], 1);
}

/// `inspect` lists the newest marks first asked for, says how many more
/// match, and leaves out what the caller already named or what is default.
#[test]
fn inspect_lists_the_newest_marks_within_a_limit() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let strokes: Vec<NewStroke> = (0..5)
        .map(|index| NewStroke {
            id: Some(format!("s{index}")),
            ..stroke("#204080")
        })
        .collect();
    app.invoke(Artwork::PAINT.with(canvas::actions::Paint {
        layer: None,
        strokes,
        group: None,
    }))
    .expect("paint");
    let inspect: ActionRef<Artwork, (String, u32)> = ActionRef::new("inspect", &["layer", "limit"]);
    let listed = app
        .operate(inspect.with(("layer-1".into(), 2)))
        .unwrap()
        .unwrap()
        .value
        .unwrap();
    let marks = listed["marks_bottom_first"].as_array().unwrap();
    let ids: Vec<&str> = marks
        .iter()
        .filter_map(|mark| mark["id"].as_str())
        .collect();
    assert_eq!(ids, ["s3", "s4"], "{listed}");
    assert_eq!(
        (listed["listed"].as_u64(), listed["matching"].as_u64()),
        (Some(2), Some(5))
    );
    assert!(
        listed["more"]
            .as_str()
            .is_some_and(|more| more.contains("2 of 5")),
        "{listed}"
    );
    assert!(
        marks[0].get("layer").is_none(),
        "the layer asked for is not repeated: {listed}"
    );
    assert!(
        marks[0]["bounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|value| value.as_f64().is_some_and(|value| value.fract() == 0.0)),
        "bounds in whole units: {listed}"
    );
    let layer = &listed["layers_top_first"][0];
    assert!(
        layer.get("opacity").is_none() && layer.get("visible").is_none(),
        "{listed}"
    );
    let too_many = app.operate(inspect.with(("layer-1".into(), 401)));
    assert!(too_many.is_err(), "a limit past 400 is refused");
}

/// Every framework app answers `render`, the whole window as a stored
/// picture, so an agent can see what a person sees through the door it acts
/// through, in the palette the window is drawn in.
#[test]
fn render_shows_the_whole_app_through_the_host_door() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness().set_theme(ui::Theme::light());
    let render: ActionRef<Artwork, ()> = ActionRef::new("render", &[]);
    let output = app
        .operate(render.invoke())
        .unwrap()
        .expect("a render finishes at once");
    let value = output.value.unwrap();
    assert_eq!(value["theme"], ui::Theme::light().name, "{value}");
    assert!(
        value["revision"]
            .as_str()
            .is_some_and(|revision| !revision.is_empty())
    );
    // The app's own content: the window less the framework's history bar.
    let (width, height) = <App as ui::prelude::ResourceApp>::shot_viewport(None);
    assert_eq!(value["width"].as_u64(), Some(width.into()));
    assert!(
        value["height"]
            .as_u64()
            .is_some_and(|drawn| drawn > u64::from(height) / 2 && drawn <= u64::from(height))
    );
    let hash = ui::tilo_actions::content_hash(output.resources.first().unwrap()).unwrap();
    let png = app.content().read(hash).expect("readable");
    assert!(png.starts_with(b"\x89PNG"));
}

/// Resizing with the pointer is the selection frame's handle dragged: one
/// `edit-marks` that scales the mark about the opposite corner, undone in
/// one step.
#[test]
fn dragging_a_selection_handle_resizes_about_the_opposite_corner() {
    use canvas::actions::NewMark;
    use canvas::model::Shape;
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Artwork::DRAW.with(canvas::actions::Draw {
        layer: None,
        marks: vec![NewMark {
            id: Some("box".into()),
            fill: Some("#336699".into()),
            ..NewMark::new(Shape::Rect {
                x: 100.0,
                y: 100.0,
                width: 200.0,
                height: 100.0,
                radius: 0.0,
            })
        }],
    }))
    .expect("draw");
    let targets = app.harness().targets();
    // V picks the select tool on the focused canvas.
    app.harness()
        .perform_named("canvas", &ui::a11y::Action::Focus)
        .expect("the canvas takes focus");
    app.harness().key("v");
    let canvas = targets
        .get("canvas")
        .copied()
        .expect("the canvas is addressable");
    // The artwork is fitted into the canvas less its 24-point padding.
    let art = app.state();
    let zoom = ((canvas.width() - 48.0) / art.width).min((canvas.height() - 48.0) / art.height);
    let origin = (
        canvas.center().x - art.width * zoom / 2.0,
        canvas.center().y - art.height * zoom / 2.0,
    );
    let screen = |x: f64, y: f64| (origin.0 + x * zoom, origin.1 + y * zoom);
    let (x, y) = screen(200.0, 150.0);
    app.harness().click_at(x, y);
    app.harness()
        .drag_at(screen(300.0, 200.0), screen(400.0, 250.0));
    let resized = app.state().mark("box").unwrap().bounds();
    assert!(
        (resized.x0 - 100.0).abs() < 2.0 && (resized.y0 - 100.0).abs() < 2.0,
        "{resized:?}"
    );
    assert!(
        (resized.x1 - 400.0).abs() < 2.0 && (resized.y1 - 250.0).abs() < 2.0,
        "{resized:?}"
    );
    assert!(app.undo(), "the resize is one undo step");
    let restored = app.state().mark("box").unwrap().bounds();
    assert!((restored.x1 - 300.0).abs() < 0.5, "{restored:?}");
}

/// The production root satisfies the Tilo-native contract: every control
/// that can cause an effect is bound to a published action.
#[test]
fn the_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}

/// The history panel lists each step; choosing an earlier one shows it, and
/// restoring it brings the artwork back to it as a new step.
#[test]
fn an_earlier_version_chosen_in_the_history_is_restored() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Artwork::PAINT.with(canvas::actions::Paint {
        layer: None,
        strokes: vec![stroke("#204080")],
        group: None,
    }))
    .expect("paint");
    let second = NewStroke {
        id: Some("second".into()),
        ..stroke("#802040")
    };
    app.invoke(Artwork::PAINT.with(canvas::actions::Paint {
        layer: None,
        strokes: vec![second],
        group: None,
    }))
    .expect("paint");
    assert_eq!(app.state().marks.len(), 2);

    // The dock's third tab is history, after Layers and Objects.
    app.harness()
        .perform_semantic_named(
            "canvas-dock",
            Some("canvas-dock:segment:2"),
            &ui::a11y::Action::Activate,
        )
        .expect("the history tab opens");
    let mut versions: Vec<(String, ui::vello::kurbo::Rect)> = app
        .harness()
        .targets()
        .into_iter()
        .filter(|(name, _)| name.starts_with("canvas-version-"))
        .collect();
    versions.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
    assert!(versions.len() >= 2, "{versions:?}");
    // Newest first: the second row is the first stroke.
    let row = versions[1].1;
    app.harness().click_at(row.center().x, row.center().y);
    let restore = app.harness().targets()["restore"];
    app.harness()
        .click_at(restore.center().x, restore.center().y);
    let art = app.state();
    assert_eq!(art.marks.len(), 1, "back to the first stroke");
    assert_eq!(art.marks[0].id, "first");
}
