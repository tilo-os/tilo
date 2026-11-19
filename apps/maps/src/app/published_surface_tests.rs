//! What the map publishes to a caller of another language: the rows its
//! actions act on are parameters of verbs of the map itself.

use super::MapWindow;
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, Availability};

use crate::MapsShell;
use crate::model::{Category, LatLon, Place};
use crate::workspace::Workspace;

#[test]
fn a_places_row_is_an_address_of_the_published_surface() {
    // What a caller of another language reads: the row-bound action is a verb
    // of the map, its parameter the row it acts on.
    let report = ui::framework::headless::surface_report::<MapsShell>();
    let verb = report["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|resource| resource["kind"] == "maps.workspace")
        .and_then(|workspace| workspace["verbs"].as_array())
        .and_then(|verbs| verbs.iter().find(|verb| verb["id"] == "open-place"))
        .unwrap_or_else(|| panic!("the map publishes open-place as a verb: {report}"));
    let place = verb["params"]
        .as_array()
        .unwrap()
        .iter()
        .find(|param| param["name"] == "place")
        .unwrap_or_else(|| panic!("open-place names the row it acts on: {report}"));
    assert_eq!(place["required"], true, "{report}");
    assert_eq!(place["type"], "text", "{report}");

    let mut app = TestApp::<MapsShell>::new().unwrap();
    let mut cafe = Place::new(
        "cafe-glueck",
        "Café Glück",
        Category::Cafe,
        "Munich",
        LatLon::new(48.137, 11.575),
    );
    cafe.saved = true;
    app.invoke(Workspace::ACCEPT_PLACES.with(vec![cafe]))
        .unwrap();
    let call = |app: &mut TestApp<MapsShell>, invoker: ActionInvoker| {
        app.call_by(
            invoker,
            "maps.workspace",
            verb["id"].as_str().unwrap(),
            serde_json::json!({ "place": "cafe-glueck" }),
        )
    };
    // The agent reads the row's address in the surface; the action answers
    // with its own rule rather than as one the map does not have.
    let refused = call(&mut app, ActionInvoker::agent("map-assistant"))
        .expect_err("showing a place is the person's own");
    assert!(
        matches!(
            &refused,
            ui::tilo_actions::ApiError::Unavailable { code, .. }
                if code == Availability::PRIVATE_VIEW
        ),
        "{refused:?}"
    );
    // The person's own caller opens the place with the same arguments.
    call(&mut app, ActionInvoker::person("test-author")).unwrap();
    app.settle();
    assert_eq!(
        app.with_surface(|window: &mut MapWindow| window.app().model.selected.clone()),
        Some(Some("cafe-glueck".to_owned()))
    );
}
