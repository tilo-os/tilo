//! The shared map through the door the host uses: verbs, field edits and
//! merging.

use super::*;
use crate::model::Category;
use ui::tilo_resource::{Edit, Invoke, Replica};

fn edit(map: &mut Replica<Workspace>, edit: &Edit<Workspace>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    map.apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn place(id: &str, name: &str) -> Place {
    Place::new(
        id,
        name,
        Category::Landmark,
        "",
        LatLon::new(48.137, 11.575),
    )
}

fn route(distance_m: f64) -> Route {
    Route {
        mode: TravelMode::Car,
        distance_m,
        duration_s: 300.0,
        geometry: vec![LatLon::new(48.13, 11.57), LatLon::new(48.14, 11.58)],
        steps: Vec::new(),
        from: LatLon::new(48.13, 11.57),
        to: LatLon::new(48.14, 11.58),
    }
}

fn accept_route(name: &str, distance_m: f64, mode: TravelMode) -> Invoke<Workspace> {
    Workspace::ACCEPT_ROUTE.with(workspace::AcceptRoute {
        route: route(distance_m),
        origin: Some(LatLon::new(1.0, 1.0)),
        origin_name: format!("{name} origin"),
        dest: Some(LatLon::new(2.0, 2.0)),
        dest_name: format!("{name} destination"),
        mode,
    })
}

/// Accepting places keeps one row per place and its saved flag; saving
/// and renaming are field edits; where a place is never changes.
#[test]
fn places_are_accepted_once_and_saved_by_field_edits() {
    let mut map = Replica::<Workspace>::new();
    map.apply(Workspace::ACCEPT_PLACES.with(vec![
        place("marienplatz", "Marienplatz"),
        place("viktualienmarkt", "Viktualienmarkt"),
    ]))
    .unwrap();
    edit(&mut map, &Workspace::save("marienplatz", true)).unwrap();
    map.apply(Workspace::ACCEPT_PLACES.with(vec![place("marienplatz", "Marienplatz (square)")]))
        .unwrap();
    let state = map.state().unwrap();
    assert_eq!(state.places.len(), 2);
    let marienplatz = state.place("marienplatz").unwrap();
    assert_eq!(marienplatz.name, "Marienplatz (square)");
    assert!(marienplatz.saved, "accepting a place again keeps it saved");
    assert!(
        edit(
            &mut map,
            &Workspace::paths()
                .places
                .item(&ui::tilo_resource::Id::from_raw("marienplatz"))
                .pos
                .lat
                .set(0.0)
        )
        .is_err(),
        "where a place is never changes"
    );
}

/// The search history holds the last searches, newest first, each once.
#[test]
fn the_search_history_dedupes_and_caps() {
    let mut map = Replica::<Workspace>::new();
    for index in 0..12 {
        map.apply(Workspace::COMMIT_SEARCH.with(format!("q{index}").as_str()))
            .unwrap();
    }
    map.apply(Workspace::COMMIT_SEARCH.with("  Q5  ")).unwrap();
    let recents = map.state().unwrap().recents;
    assert_eq!(recents.len(), MAX_RECENTS);
    assert_eq!(recents[0], "Q5");
    assert_eq!(
        recents
            .iter()
            .filter(|q| q.eq_ignore_ascii_case("q5"))
            .count(),
        1
    );
    map.apply(Workspace::CLEAR_RECENTS.invoke()).unwrap();
    assert!(map.state().unwrap().recents.is_empty());
}

/// An accepted route and its endpoints win or lose together when two
/// people accept different planner replies at once; transit options keep
/// their own.
#[test]
fn divergent_route_acceptances_converge_without_torn_context() {
    let mut ours = Replica::<Workspace>::new();
    ours.apply(Workspace::ACCEPT_PLACES.with(vec![place("a", "A")]))
        .unwrap();
    let mut theirs = ours.fork();
    ours.apply(accept_route("Alpha", 1_111.0, TravelMode::Car))
        .unwrap();
    theirs
        .apply(accept_route("Beta", 2_222.0, TravelMode::Foot))
        .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let merged = ours.state().unwrap();
    assert_eq!(merged, theirs.state().unwrap());
    let distance = merged.route().unwrap().distance_m;
    let (_, origin, _, dest) = merged.endpoints().unwrap();
    let alpha = (distance - 1_111.0).abs() < f64::EPSILON
        && origin == "Alpha origin"
        && dest == "Alpha destination";
    let beta = (distance - 2_222.0).abs() < f64::EPSILON
        && origin == "Beta origin"
        && dest == "Beta destination";
    assert!(
        alpha || beta,
        "the winning route and its labels remain one acceptance"
    );

    ours.apply(Workspace::ACCEPT_TRANSIT.with(workspace::AcceptTransit {
        transit: Vec::new(),
        origin: None,
        origin_name: "Here".into(),
        dest: None,
        dest_name: "There".into(),
    }))
    .unwrap();
    let state = ours.state().unwrap();
    assert_eq!(state.mode, TravelMode::Transit);
    assert_eq!(state.endpoints().unwrap().3, "There");
    assert!(state.route().is_some(), "the road route stays accepted");
    ours.apply(Workspace::CLEAR_TRANSIT.invoke()).unwrap();
    ours.apply(Workspace::CLEAR_ROUTE.invoke()).unwrap();
    assert!(ours.apply(Workspace::CLEAR_ROUTE.invoke()).is_err());
}
