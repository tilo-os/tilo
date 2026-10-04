//! Working with the artwork's history through the door an agent uses:
//! catching a bad passage with diff, undoing the run's own steps, looking
//! at a past version, restoring marks from a checkpoint.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use serde_json::{Value, json};
use ui::prelude::TestApp;

const RUN: &str = "mcp:painter";

fn draw(app: &mut TestApp<App>, marks: &Value) {
    app.call_in_run(RUN, "draw", json!({ "marks": marks }))
        .expect("draw");
}

fn sky_and_sun(app: &mut TestApp<App>) {
    draw(
        app,
        &json!([
            {"id": "sky", "shape": {"kind": "rect", "x": 0, "y": 0, "width": 1200, "height": 500}, "fill": "#6fa8dc"},
            {"id": "sun", "shape": {"kind": "ellipse", "cx": 900, "cy": 160, "rx": 80, "ry": 80}, "fill": "#ffd966"},
        ]),
    );
}

fn ids(changes: &Value) -> Vec<&str> {
    changes
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect()
}

fn fill(app: &TestApp<App>, id: &str) -> Option<String> {
    app.state()
        .marks
        .iter()
        .find(|mark| mark.id == id)
        .and_then(|mark| mark.style.fill.clone())
}

#[test]
fn a_bad_passage_is_found_by_diff_and_undone_by_its_run() {
    let mut app = TestApp::<App>::new().unwrap();
    sky_and_sun(&mut app);
    app.call_in_run(RUN, "checkpoint", json!({"name": "before sky rework"}))
        .unwrap();
    // The bad passage: a muddy sky and a stray blot.
    app.call_in_run(
        RUN,
        "edit-marks",
        json!({"ids": ["sky"], "fill": "#5a4a3a"}),
    )
    .unwrap();
    draw(
        &mut app,
        &json!([{"id": "blot", "shape": {"kind": "ellipse", "cx": 300, "cy": 300, "rx": 40, "ry": 30}, "fill": "#000000"}]),
    );

    let diff = app
        .call("diff", json!({"from": "before sky rework", "look": true}))
        .unwrap()
        .unwrap();
    assert_eq!(ids(&diff["marks"]["added"]), ["blot"], "{diff}");
    assert_eq!(
        diff["marks"]["changed"],
        json!({"sky": ["fill"]}),
        "what changed about each mark: {diff}"
    );
    assert!(ids(&diff["marks"]["removed"]).is_empty(), "{diff}");
    assert_eq!(
        diff["changed_region"],
        json!([0.0, 0.0, 1200.0, 508.0]),
        "{diff}"
    );
    assert!(
        diff["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty()),
        "a before|after picture: {diff}"
    );

    // The run takes back its last two steps; history shows them undone.
    let undone = app
        .call_in_run(RUN, "undo", json!({"steps": 2}))
        .unwrap()
        .unwrap();
    assert_eq!(undone["undone"].as_array().unwrap().len(), 2, "{undone}");
    assert_eq!(fill(&app, "sky").as_deref(), Some("#6fa8dc"));
    assert!(fill(&app, "blot").is_none());
    let same = app
        .call("diff", json!({"from": "before sky rework"}))
        .unwrap()
        .unwrap();
    assert_eq!(same["same"], true, "{same}");

    let history = app.call("history", json!({})).unwrap().unwrap();
    let versions = carried(history["versions"].as_array().unwrap());
    let blot = versions
        .iter()
        .find(|version| {
            version["args"]
                .as_str()
                .is_some_and(|args| args == r#"{marks:["blot"]}"#)
                && version["undone"] == true
        })
        .unwrap_or_else(|| panic!("the blot step is listed as undone: {history}"));
    assert_eq!(blot["run_id"], RUN, "{history}");
    assert_eq!(history["checkpoints"][0]["name"], "before sky rework");
}

/// Versions with the run, author and time each carries over from the one
/// before filled in.
fn carried(versions: &[Value]) -> Vec<Value> {
    let mut before = serde_json::Map::new();
    versions
        .iter()
        .map(|version| {
            let mut version = version.as_object().unwrap().clone();
            if !["run_id", "by"]
                .iter()
                .any(|key| version.contains_key(*key))
            {
                for key in ["run", "run_id", "by"] {
                    if let Some(value) = before.get(key) {
                        version.insert(key.into(), value.clone());
                    }
                }
            }
            if !version.contains_key("time_ms")
                && let Some(time) = before.get("time_ms")
            {
                version.insert("time_ms".into(), time.clone());
            }
            before.clone_from(&version);
            Value::Object(version)
        })
        .collect()
}

#[test]
fn a_past_version_is_looked_at_and_its_marks_restored_by_id() {
    let mut app = TestApp::<App>::new().unwrap();
    sky_and_sun(&mut app);
    app.call("checkpoint", json!({"name": "good"})).unwrap();
    app.call(
        "edit-marks",
        json!({"ids": ["sky", "sun"], "fill": "#808080"}),
    )
    .unwrap();

    let now = app.call("inspect", json!({})).unwrap().unwrap();
    let then = app
        .call("inspect", json!({"version": "good"}))
        .unwrap()
        .unwrap();
    assert_eq!(then["version"].as_str().map(str::len), Some(12), "{then}");
    assert_ne!(then["revision"], now["revision"]);
    let fills = |inspected: &Value| -> Vec<String> {
        inspected["marks_bottom_first"]
            .as_array()
            .unwrap()
            .iter()
            .map(|mark| mark["fill"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(fills(&then), ["#6fa8dc", "#ffd966"]);
    assert_eq!(fills(&now), ["#808080", "#808080"]);

    let look_now = app.call("look", json!({"size": 256})).unwrap().unwrap();
    let look_then = app
        .call("look", json!({"size": 256, "version": "good"}))
        .unwrap()
        .unwrap();
    assert_ne!(look_now["content"], look_then["content"]);
    assert_eq!(look_then["version"], then["version"]);

    // Only the sun goes back; the grey sky stays.
    let restored = app
        .call("restore", json!({"version": "good", "ids": ["sun"]}))
        .unwrap()
        .unwrap();
    assert_eq!(restored["restored"], json!(["marks/sun"]), "{restored}");
    assert_eq!(fill(&app, "sun").as_deref(), Some("#ffd966"));
    assert_eq!(fill(&app, "sky").as_deref(), Some("#808080"));

    let unknown = app
        .call("look", json!({"version": "nothing-like-it"}))
        .expect_err("an unknown version is refused")
        .to_string();
    assert!(
        unknown.contains("\"good\""),
        "names the checkpoints: {unknown}"
    );
}

#[test]
fn a_person_and_an_agent_undo_only_their_own_steps() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "draw",
        json!({"marks": [{"id": "hill", "shape": {"kind": "rect", "x": 0, "y": 600, "width": 1200, "height": 300}, "fill": "#38761d"}]}),
    )
    .unwrap();
    sky_and_sun(&mut app);

    app.call("undo", json!({})).unwrap();
    assert!(fill(&app, "hill").is_none(), "the person's own step");
    assert!(fill(&app, "sun").is_some(), "the agent's stays");
    app.call("redo", json!({})).unwrap();

    app.call("undo", json!({"run": RUN})).unwrap();
    assert!(fill(&app, "sun").is_none());
    assert!(fill(&app, "hill").is_some());
    let refused = app
        .call_in_run(RUN, "undo", json!({}))
        .expect_err("nothing left of the run")
        .to_string();
    assert!(refused.contains("nothing in effect to undo"), "{refused}");
}

/// The window's width in the test.
const WIDTH: u32 = 1280;

/// The colour of the window's pixel at `(x, y)`.
fn pixel_at(app: &mut TestApp<App>, (x, y): (f64, f64)) -> [u8; 4] {
    let width = WIDTH;
    let rgba = app.harness().render_window_rgba();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a point inside the window"
    )]
    let at = (y as usize)
        .saturating_mul(width as usize)
        .saturating_add(x as usize)
        .saturating_mul(4);
    let mut pixel = [0; 4];
    pixel.copy_from_slice(
        rgba.get(at..at.saturating_add(4))
            .expect("inside the window"),
    );
    pixel
}

#[test]
fn choosing_a_past_version_shows_it_in_place_of_the_artwork() {
    // The history scenario: a landscape, with the history in the dock.
    let mut app = TestApp::<App>::capture_at(Some("history"), (WIDTH, 800)).unwrap();
    let whole = json!({"kind": "rect", "x": 0, "y": 0, "width": 1200, "height": 900});
    app.call(
        "draw",
        json!({"marks": [{"id": "flood", "shape": whole, "fill": "#d02020"}]}),
    )
    .unwrap();
    let red = app.versions().last().unwrap().hash.to_string();
    app.call("edit-marks", json!({"ids": ["flood"], "fill": "#2040d0"}))
        .unwrap();
    let canvas = app.harness().targets()["canvas"];
    let centre = (canvas.center().x, canvas.center().y);
    let now = pixel_at(&mut app, centre);
    assert!(now[2] > now[0], "the artwork is blue now: {now:?}");
    let row = app
        .harness()
        .targets()
        .keys()
        .find(|name| {
            name.strip_prefix("canvas-version-")
                .is_some_and(|change| red.starts_with(change) || change.starts_with(&red))
        })
        .cloned()
        .unwrap_or_else(|| panic!("a history row for {red}"));
    app.harness().click_named(&row).unwrap();
    let then = pixel_at(&mut app, centre);
    assert!(
        then[0] > then[2],
        "the red version is shown in place of the blue artwork: {then:?}"
    );
}
