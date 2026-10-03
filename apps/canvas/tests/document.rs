//! The artwork as a document a person names, starts, opens, saves and
//! exports, and the steps that leaves in its history.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use ui::prelude::TestApp;

fn labels(app: &TestApp<App>) -> Vec<String> {
    app.versions()
        .iter()
        .map(|version| format!("{version:?}"))
        .collect()
}

#[test]
fn naming_the_artwork_is_one_step_of_history() {
    let mut app = TestApp::<App>::new().unwrap();
    let before = app.versions().len();
    app.harness().click_named("canvas-title").unwrap();
    app.harness().type_text("Dawn");
    app.harness().key("Enter");
    app.harness().key("Tab");
    assert_eq!(app.state().title, "Dawn");
    let after = labels(&app);
    assert_eq!(
        after.len() - before,
        1,
        "typing a title and pressing Enter is one step: {after:#?}"
    );
}

#[test]
fn a_new_artwork_starts_from_the_dialog() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().click_named("canvas-welcome-new").unwrap();
    app.harness().click_named("canvas-new-size-a4").unwrap();
    app.harness()
        .click_named("canvas-new-start-watercolour")
        .unwrap();
    app.harness().click_named("canvas-new-title").unwrap();
    app.harness().type_text("Harbour");
    app.harness().key("Enter");
    let art = app.state();
    assert_eq!(
        (art.title.as_str(), art.width, art.height),
        ("Harbour", 3508.0, 2480.0)
    );
    assert_eq!(art.ground.texture, "cold-press");
    assert_eq!(art.layers.len(), 1);
    assert_eq!(art.layers[0].medium, "watercolour");
    assert_eq!(art.layers[0].name, "Watercolour");
}

/// What an artwork is, without the orders its marks and layers were given
/// where they were written.
fn content(art: &canvas::Artwork) -> serde_json::Value {
    let mut file = serde_json::to_value(canvas::document::ArtworkFile::of(art)).unwrap();
    file.as_object_mut().unwrap().remove("format");
    file
}

#[test]
fn an_agent_saves_starts_over_and_opens_the_artwork_again() {
    let mut app = TestApp::<App>::new().unwrap();
    for action in canvas::sample::landscape() {
        app.invoke(ui::prelude::Invoke::apply(&action).unwrap())
            .unwrap();
    }
    app.call("set-artwork", serde_json::json!({"title": "Valley"}))
        .unwrap();
    let painted = app.state();
    let saved = app
        .call("save-artwork", serde_json::json!({}))
        .unwrap()
        .unwrap();
    let address = saved["content"].as_str().unwrap().to_owned();

    // A path in Files, too.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Valley.artwork");
    app.call(
        "save-artwork",
        serde_json::json!({"path": path.display().to_string()}),
    )
    .unwrap();
    assert!(path.exists());

    app.call(
        "new-artwork",
        serde_json::json!({"size": "square", "start": "oil", "title": "Fresh"}),
    )
    .unwrap();
    let fresh = app.state();
    assert_eq!(
        (
            fresh.title.as_str(),
            fresh.width,
            fresh.height,
            fresh.marks.len()
        ),
        ("Fresh", 2048.0, 2048.0, 0)
    );
    assert_eq!(fresh.ground.texture, "linen");
    assert_eq!(fresh.layers[0].medium, "oil");

    // One undo brings the painting back.
    assert!(app.undo());
    assert_eq!(content(&app.state()), content(&painted));

    app.call("new-artwork", serde_json::json!({})).unwrap();
    app.call("open-artwork", serde_json::json!({"file": address}))
        .unwrap();
    assert_eq!(content(&app.state()), content(&painted));

    app.call("new-artwork", serde_json::json!({})).unwrap();
    app.call(
        "open-artwork",
        serde_json::json!({"file": path.display().to_string()}),
    )
    .unwrap();
    assert_eq!(content(&app.state()), content(&painted));
}

#[test]
fn a_wrong_file_or_size_says_what_is_wrong() {
    let mut app = TestApp::<App>::new().unwrap();
    let error = app
        .call("new-artwork", serde_json::json!({"size": "a3"}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("screen, a4, letter"), "{error}");
    let error = app
        .call(
            "open-artwork",
            serde_json::json!({"file": "{\"format\": \"other\"}"}),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a Canvas artwork"), "{error}");
}

#[test]
fn a_tools_key_works_after_a_click_in_the_rail_and_a_tool_click_after_a_drag_lands() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().click_named("canvas-welcome-close").unwrap();
    let canvas = app.harness().targets()["canvas"];
    let rail = app.harness().targets()["Tool"];
    let (x, y) = (canvas.center().x, canvas.center().y);
    // Click the select tool in the rail (its top segment), so the rail has
    // the focus; then R picks up the rectangle anyway.
    app.harness().click_at(rail.center().x, rail.y0 + 20.0);
    app.harness().key("r");
    app.harness()
        .drag_at((x - 60.0, y - 40.0), (x + 60.0, y + 40.0));
    let art = app.state();
    assert_eq!(
        art.marks
            .iter()
            .filter(|mark| mark.shape.kind() == "rect")
            .count(),
        1,
        "R after a click in the rail drew a rectangle"
    );
    // Right after a drag on the canvas, a click on the ellipse (third in
    // the rail) is taken, and the next drag draws an ellipse.
    app.harness()
        .drag_at((x - 100.0, y - 100.0), (x - 80.0, y - 80.0));
    let segment = rail.height() / 8.0;
    app.harness()
        .click_at(rail.center().x, rail.y0 + segment * 3.5);
    app.harness()
        .drag_at((x + 80.0, y + 60.0), (x + 160.0, y + 120.0));
    assert!(
        app.state()
            .marks
            .iter()
            .any(|mark| mark.shape.kind() == "ellipse"),
        "the ellipse tool clicked right after a drag drew an ellipse"
    );
}

#[test]
fn a_person_opens_a_recent_artwork_from_the_welcome() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Valley.artwork");
    {
        let mut painter = TestApp::<App>::new().unwrap();
        for action in canvas::sample::landscape() {
            painter
                .invoke(ui::prelude::Invoke::apply(&action).unwrap())
                .unwrap();
        }
        painter
            .call(
                "save-artwork",
                serde_json::json!({"path": path.display().to_string()}),
            )
            .unwrap();
    }
    let painted = canvas::document::read(&path).unwrap();
    canvas::document::recents::remember(canvas::document::Recent {
        path: path.display().to_string(),
        title: "Valley".into(),
        width: painted.width,
        height: painted.height,
        at_ms: 1,
        thumbnail: None,
    });
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().click_named("canvas-recent-1").unwrap();
    app.harness().key("Shift");
    assert_eq!(
        app.state().marks.len(),
        painted.marks.len(),
        "the recent artwork opened"
    );
    assert_eq!(app.versions().len(), 2, "opening is one step");
}
