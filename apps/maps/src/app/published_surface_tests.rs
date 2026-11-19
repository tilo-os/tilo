//! What the map publishes to a caller of another language: the rows its
//! actions act on are parameters of verbs of the map itself.

use super::MapWindow;
use ui::PresenceKey;
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, PresenceId};

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
    // The agent opens the place in its own view of the map: the person's
    // screen does not move, and the person sees the agent at the place.
    let agent = PresenceId::new("map-assistant-view").unwrap();
    call(
        &mut app,
        ActionInvoker::agent("map-assistant").with_presence(agent.clone()),
    )
    .unwrap();
    app.settle();
    let selected = |app: &mut TestApp<MapsShell>| {
        app.with_surface(|window: &mut MapWindow| window.app().model.selected.clone())
    };
    assert_eq!(selected(&mut app), Some(None), "the person's view stays");
    let presences = app.presences();
    let shown = presences
        .iter()
        .find(|shown| shown.presence == agent)
        .unwrap_or_else(|| panic!("the person sees the agent: {presences:?}"));
    assert!(
        matches!(&shown.key, PresenceKey::Resource { id, .. } if id == "cafe-glueck"),
        "the agent's view shows the place: {shown:?}"
    );
    // An agent with no presence of its own is answered in its default
    // view, seen the same way.
    call(&mut app, ActionInvoker::agent("map-helper")).unwrap();
    app.settle();
    assert_eq!(selected(&mut app), Some(None), "the person's view stays");
    let helper = PresenceId::new("agent:map-helper").unwrap();
    assert!(
        app.presences().iter().any(|shown| shown.presence == helper),
        "the person sees the agent's default view"
    );
    // The person's own caller opens the place with the same arguments.
    call(&mut app, ActionInvoker::person("test-author")).unwrap();
    app.settle();
    assert_eq!(selected(&mut app), Some(Some("cafe-glueck".to_owned())));
}
