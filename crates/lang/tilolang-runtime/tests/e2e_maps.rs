//! The Maps app (`apps/tl/maps/app.tl`) end to end: a search that takes in the places it found,
//! the searches a person remembers, the directions a routing service offers with their turns, the
//! journeys public transport offers, what stands around the map, the areas kept for offline use,
//! and the map window with its imagery, markers and route.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::ids::MemberId;
    use tilolang_runtime::run::WebRequest;
    use tilolang_runtime::web::{WebReply, WebTransport};
    use tilolang_runtime::{
        Actor, AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/maps/app.tl");

    /// What the geocoder answers for Munich.
    const MUNICH: &str = r#"[
        {"lat":"48.1374","lon":"11.5755","display_name":"Marienplatz, München, Bayern","name":"Marienplatz","osm_type":"node","osm_id":11,"class":"tourism","type":"attraction","address":{"road":"Marienplatz"},"extratags":{"phone":"+49 89 23300","website":"https://www.muenchen.de/marienplatz"}},
        {"lat":"48.1351","lon":"11.5820","display_name":"Café Glockenspiel, München","name":"Café Glockenspiel","osm_type":"node","osm_id":12,"class":"amenity","type":"cafe","address":{"road":"Marienplatz","house_number":"1"}}
    ]"#;

    /// What the geocoder answers for a place the tests name but it does not know.
    const UNKNOWN: &str = "[]";

    /// What the geocoder answers when two records describe one place, a dozen metres apart.
    const TWINS: &str = r#"[
        {"lat":"48.1374","lon":"11.5755","display_name":"Marienplatz, München","name":"Marienplatz","osm_type":"node","osm_id":11,"class":"tourism","type":"attraction"},
        {"lat":"48.1375","lon":"11.5756","display_name":"Marienplatz, München","name":"Marienplatz","osm_type":"way","osm_id":12,"class":"tourism","type":"attraction"}
    ]"#;

    /// What the routing service answers for two places in Munich: two routes, so the map can offer
    /// another one.
    const ROUTES: &str = r#"{"code":"Ok","routes":[
        {"distance":1200.0,"duration":600.0,"geometry":{"coordinates":[[11.5755,48.1374],[11.5820,48.1351]]},
         "legs":[{"steps":[
            {"maneuver":{"type":"depart"},"name":"Marienplatz","distance":200.0},
            {"maneuver":{"type":"turn","modifier":"left"},"name":"Sendlinger Straße","distance":1000.0},
            {"maneuver":{"type":"arrive"},"name":"Café Glockenspiel","distance":0.0}
         ]}]},
        {"distance":1400.0,"duration":700.0,"geometry":{"coordinates":[[11.5755,48.1374],[11.5700,48.1330],[11.5820,48.1351]]},
         "legs":[{"steps":[{"maneuver":{"type":"depart"},"name":"Marienplatz","distance":1400.0}]}]}
    ]}"#;

    /// What the transit service answers for the same two places.
    const JOURNEYS: &str = r##"{"itineraries":[
        {"startTime":"2026-11-24T10:00:00Z","endTime":"2026-11-24T10:25:00Z","duration":1500.0,"transfers":0,
         "legs":[{"mode":"BUS","routeShortName":"58","headsign":"Hauptbahnhof",
                  "from":{"name":"Marienplatz"},"to":{"name":"Hauptbahnhof"},
                  "startTime":"2026-11-24T10:05:00Z","endTime":"2026-11-24T10:20:00Z","duration":900.0,
                  "intermediateStops":[{},{}],"routeColor":"#0066cc","agencyName":"MVG","realTime":true,
                  "legGeometry":{"points":"_p~iF~ps|U_ulLnnqC_mqNvxq`@","precision":5}}]},
        {"startTime":"2026-11-24T10:10:00Z","endTime":"2026-11-24T10:50:00Z","duration":2400.0,"transfers":1,
         "legs":[{"mode":"WALK","from":{"name":"Marienplatz"},"to":{"name":"Sendlinger Tor"},
                  "startTime":"2026-11-24T10:10:00Z","endTime":"2026-11-24T10:15:00Z","duration":300.0}]}
    ]}"##;

    /// What the map's points of interest answer around Munich.
    const NEARBY: &str = r#"{"elements":[
        {"type":"node","id":42,"lat":48.138,"lon":11.576,
         "tags":{"name":"Café Glockenspiel","amenity":"cafe","addr:street":"Marienplatz","addr:housenumber":"1","opening_hours":"8:00-20:00"}},
        {"type":"way","id":43,"center":{"lat":48.139,"lon":11.577},
         "tags":{"name":"Münchner Stadtmuseum","tourism":"museum","website":"https://example.org"}}
    ]}"#;

    /// The device's network, as the map services answer it: what was asked, and the answer each
    /// kind of request gets.
    #[derive(Clone, Default)]
    struct Web {
        asked: Rc<RefCell<Vec<String>>>,
        /// Whether the next request fails as unreachable.
        offline: Rc<RefCell<bool>>,
        /// Whether the geocoder answers two records for one place.
        twin: Rc<RefCell<bool>>,
    }

    impl Web {
        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }

    impl WebTransport for Web {
        fn fetch(&self, request: &WebRequest) -> WebReply {
            self.asked.borrow_mut().push(request.url.clone());
            if *self.offline.borrow() {
                return WebReply::failed("no connection");
            }
            let body = if request.url.contains("nominatim") {
                if request.url.contains("q=Nowhere") {
                    UNKNOWN
                } else if *self.twin.borrow() {
                    TWINS
                } else {
                    MUNICH
                }
            } else if request.url.contains("osrm") {
                ROUTES
            } else if request.url.contains("transitous") {
                JOURNEYS
            } else if request.url.contains("overpass") {
                NEARBY
            } else {
                ""
            };
            WebReply {
                status: 200,
                status_text: "OK".to_owned(),
                headers: Vec::new(),
                body: body.as_bytes().to_vec(),
            }
        }
    }

    struct Maps {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        web: Web,
        engine: DeviceEngine,
        /// Whether the device can say where it is.
        located: Rc<std::cell::Cell<bool>>,
    }

    impl Maps {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let web = Web::default();
            let located = Rc::new(std::cell::Cell::new(false));
            let found = Rc::clone(&located);
            let engine = DeviceEngine::calls(
                move |op, _| match op {
                    "locate" if found.get() => Ok(r#"{"lat":48.1374,"lon":11.5755}"#.to_owned()),
                    "locate" => Ok(r#"{"why":"Location is unavailable right now"}"#.to_owned()),
                    "forget-area" => Ok(r#"{"freed":1024}"#.to_owned()),
                    "clear-saved" => Ok(r#"{"freed":4096}"#.to_owned()),
                    "kept-areas" => Ok("[]".to_owned()),
                    "estimate-area" => Ok(r#"{"tiles":12,"bytes":216000}"#.to_owned()),
                    _ => Err(format!("`{op}` is no op this device runs")),
                },
                &Files::default(),
            )
            .for_app("so.tilo.maps")
            .starting(&["save-area"]);
            rt.set_web(web.clone());
            rt.set_engine(engine.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let root = rt.create_root("Atlas").unwrap();
            // What this device keeps, as opening the app makes it: the areas offline and the
            // route it estimates when the routing service cannot answer.
            for name in ["KeptAreas", "Estimate"] {
                let ty = app
                    .meta()
                    .types
                    .iter()
                    .find(|declared| declared.name == name)
                    .unwrap_or_else(|| panic!("the {name} type"))
                    .id
                    .clone();
                rt.create_root_at(&format!("devices/local/{}/root-{ty}", app.meta().app), &ty)
                    .unwrap_or_else(|error| panic!("the device's {name} root: {error:?}"));
            }
            Self {
                rt,
                app,
                root,
                web,
                engine,
                located,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn read(&self, at: &ResourceRef, field: &str) -> Value {
            self.rt.read(at, field).unwrap()
        }

        fn text(&self, at: &ResourceRef, field: &str) -> String {
            match self.read(at, field) {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn float(&self, at: &ResourceRef, field: &str) -> f64 {
            match self.read(at, field) {
                Value::Float(n) => n,
                Value::Int(n) => f64::from(i32::try_from(n).unwrap_or(0)),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, at: &ResourceRef, field: &str) -> bool {
            match self.read(at, field) {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn records(&self, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.read(at, field) {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(held) => Some(Rc::unwrap_or_clone(held.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// The record values a field typed as a list of a record type holds: a route's steps,
        /// a journey's legs, which are values of the record rather than items of a collection.
        fn record_values(&self, at: &ResourceRef, field: &str) -> Vec<Value> {
            match self.read(at, field) {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Rec(_) => item,
                        other => panic!("an item of {field} reads {other:?}"),
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// The field `field` of the record value `rec`.
        fn of(&self, rec: &Value, field: &str) -> Value {
            let Value::Rec(held) = rec else {
                panic!("not a record: {rec:?}");
            };
            let meta = self.app.meta();
            let declared = meta
                .types
                .iter()
                .find(|declared| declared.id == held.ty)
                .unwrap_or_else(|| panic!("`{}` is not declared", held.ty));
            let id = declared
                .fields
                .iter()
                .find(|f| f.name == field)
                .unwrap_or_else(|| panic!("`{}` has no field `{field}`", declared.name))
                .id
                .clone();
            held.fields
                .get(&id)
                .cloned()
                .unwrap_or_else(|| panic!("the record has no `{field}`"))
        }

        fn text_of(&self, rec: &Value, field: &str) -> String {
            match self.of(rec, field) {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag_of(&self, rec: &Value, field: &str) -> bool {
            match self.of(rec, field) {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn places(&self) -> Vec<ResourceRef> {
            self.records(&self.root, "places")
        }

        fn found(&self) -> Vec<ResourceRef> {
            self.places()
                .into_iter()
                .filter(|place| self.flag(place, "found"))
                .collect()
        }

        fn saved(&self) -> Vec<ResourceRef> {
            self.places()
                .into_iter()
                .filter(|place| self.flag(place, "saved"))
                .collect()
        }

        fn recents(&self) -> Vec<ResourceRef> {
            self.records(&self.root, "recents")
        }

        fn routes(&self) -> Vec<ResourceRef> {
            self.records(&self.root, "routes")
        }

        fn journeys(&self) -> Vec<ResourceRef> {
            self.records(&self.root, "journeys")
        }

        /// This device's root of the type `name`.
        fn device_root(&self, name: &str) -> ResourceRef {
            let meta = self.app.meta();
            let ty = meta
                .types
                .iter()
                .find(|declared| declared.name == name)
                .unwrap_or_else(|| panic!("the {name} type"))
                .id
                .clone();
            ResourceRef::root(format!("devices/local/{}/root-{ty}", meta.app), ty)
        }

        /// The areas this device keeps offline, as its own root lists them.
        fn offline(&self) -> Vec<ResourceRef> {
            let root = self.device_root("KeptAreas");
            self.records(&root, "kept")
        }

        fn pois(&self) -> Vec<ResourceRef> {
            self.records(&self.root, "pois")
        }

        /// The case `name` (`TravelMode.car`) of the enum `ty` (`TravelMode`).
        fn case(&self, ty: &str, name: &str) -> Value {
            let ids = &self.app.compiled.ids;
            Value::Enum(EnumValue {
                ty: ids.get(ty).unwrap().to_owned(),
                variant: ids.get(name).unwrap().to_owned(),
                fields: BTreeMap::default(),
            })
        }

        fn search(&self, words: &str) {
            self.call(
                "search",
                vec![Value::resource(&self.root), Value::Text(words.to_owned())],
            );
        }

        fn window(&self) -> ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("map"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap()
        }
    }

    tilo_test_env::scenario! {
        fn a_map_finds_places_and_plans_routes;

    phase a_search_takes_in_the_places_it_found;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        let found = m.found();
        let all: Vec<String> = m
            .places()
            .iter()
            .map(|p| format!("{} found={} saved={}", m.text(p, "name"), m.flag(p, "found"), m.flag(p, "saved")))
            .collect();
        assert_eq!(found.len(), 2, "both places the geocoder answered are on the map: {all:?} status={:?}", m.text(&m.root, "status"));
        assert_eq!(m.text(&found[0], "name"), "Marienplatz");
        assert_eq!(m.text(&found[0], "address"), "Marienplatz, München, Bayern");
        assert!((m.float(&found[0], "latitude") - 48.1374).abs() < 1e-6);
        assert!((m.float(&found[0], "longitude") - 11.5755).abs() < 1e-6);
        assert_eq!(m.text(&found[1], "name"), "Café Glockenspiel");
        assert_eq!(
            m.text(&m.root, "status"),
            "2 results for “Marienplatz”",
            "the map says what the search found"
        );
        assert!(
            m.web.asked()[0].contains("nominatim.openstreetmap.org")
                && m.web.asked()[0].contains("q=Marienplatz"),
            "the geocoder was asked for what was typed: {:?}",
            m.web.asked()
        );
        // A search keeps the place it found apart from the ones the person keeps.
        assert!(m.saved().is_empty());
    }

    phase a_place_another_app_hands_over_is_shown_and_kept_as_it_sent_it;
    {
        let m = Maps::new();
        // Places another app or an agent found: data it has never stored in this map.
        let handed = r#"[{"name":"LX Factory","key":"pt-3","category":"shop",
            "latitude":38.7033,"longitude":-9.1789,"saved":true}]"#;
        m.call(
            "accept_places",
            vec![Value::resource(&m.root), Value::text(handed)],
        );
        let places = m.places();
        assert_eq!(places.len(), 1, "the place landed in the map");
        assert!(
            m.flag(&places[0], "found"),
            "a place handed over stands in the results"
        );
        assert!(
            m.flag(&places[0], "saved"),
            "a place handed over as kept is kept"
        );
        // The window opens on the results, with no search typed: what was handed over shows
        // there, because it is what was found.
        let mut window = m.window();
        window.poll_patches().unwrap();
        let rows: Vec<String> = of_kind(window.tree(), "ui.ListRow")
            .into_iter()
            .map(|row| props_text(row, "title"))
            .collect();
        assert!(
            rows.iter().any(|title| title == "LX Factory"),
            "the results list what was handed over: {rows:?}"
        );
        // The places kept stand under Saved, each a row of the list beside the map, as they
        // stand in the results.
        let toggle = of_kind(window.tree(), "ui.ToggleGroup")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("the places' segments: {}", dump(window.tree())));
        let key = toggle.key.clone();
        let saved = window.fire_with(&key, "change", Some(&Value::Int(2)));
        assert!(matches!(saved, Ok(InvokeOutcome::Done(_))), "{saved:?}");
        window.poll_patches().unwrap();
        let rows: Vec<String> = of_kind(window.tree(), "ui.ListRow")
            .into_iter()
            .map(|row| props_text(row, "title"))
            .collect();
        assert!(
            rows.iter().any(|title| title == "LX Factory"),
            "the kept places list under Saved: {rows:?}"
        );
    }

    phase a_found_place_keeps_its_name_and_position_across_searches;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        m.search("Marienplatz");
        let place = m
            .places()
            .into_iter()
            .find(|p| m.text(p, "name") == "Marienplatz")
            .expect("the place the geocoder answered is on the map");
        assert_eq!(
            m.text(&place, "name"),
            "Marienplatz",
            "the place's name is its own field"
        );
        assert!(
            (m.float(&place, "latitude") - 48.1374).abs() < 1e-6,
            "the place's latitude is its own field"
        );
        assert!(
            (m.float(&place, "longitude") - 11.5755).abs() < 1e-6,
            "the place's longitude is its own field"
        );
    }

    phase a_search_replaces_the_last_ones_and_keeps_what_was_saved;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        let first = m.found()[0].clone();
        m.call("save_place", vec![Value::resource(&first), Value::Bool(true)]);
        assert_eq!(m.saved().len(), 1, "the place the person kept stays saved");
        m.search("Café");
        assert_eq!(
            m.found().len(),
            2,
            "the next search finds its own places, not the last search's with them"
        );
        assert_eq!(
            m.saved().len(),
            1,
            "a place the person kept is theirs, not the search's"
        );
        let kept = m.saved()[0].clone();
        assert_eq!(m.text(&kept, "name"), "Marienplatz");
    }

    phase recent_searches_keep_the_newest_eight_and_clear;
    {
        let m = Maps::new();
        for words in [
            "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        ] {
            m.search(words);
        }
        let recents = m.recents();
        assert_eq!(recents.len(), 8, "the map remembers eight searches");
        assert_eq!(m.text(&recents[0], "query"), "ten", "newest first");
        assert_eq!(m.text(&recents[7], "query"), "three");
        m.call("clear_recents", vec![Value::resource(&m.root)]);
        assert!(m.recents().is_empty(), "cleared");
    }

    phase directions_find_a_route_with_its_turns_and_another_one;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        let routes = m.routes();
        assert_eq!(routes.len(), 2, "both routes the service offered are kept");
        let first = routes[0].clone();
        assert!((m.float(&first, "distance_m") - 1200.0).abs() < 1e-6);
        assert!((m.float(&first, "duration_s") - 600.0).abs() < 1e-6);
        assert_eq!(m.text(&first, "from_name"), "Marienplatz");
        assert_eq!(m.text(&first, "to_name"), "Café Glockenspiel");
        let steps = m.record_values(&first, "steps");
        assert_eq!(steps.len(), 3);
        assert_eq!(m.text_of(&steps[1], "instruction"), "Turn left onto Sendlinger Straße");
        assert_eq!(m.text_of(&steps[2], "instruction"), "Arrive at your destination");
        // The line the map draws runs through the route's places, latitude first.
        match m.read(&first, "geometry") {
            Value::List(points) => {
                assert_eq!(points.len(), 4);
                assert!(matches!(points[0], Value::Float(n) if (n - 48.1374).abs() < 1e-6));
                assert!(matches!(points[1], Value::Float(n) if (n - 11.5755).abs() < 1e-6));
            }
            other => panic!("geometry reads {other:?}"),
        }
        // Another route shows the one after it.
        m.call("another_route", vec![Value::resource(&m.root)]);
        assert!(matches!(m.read(&m.root, "route_at"), Value::Int(1)));
        // The route is cleared, and says so.
        m.call("clear_route", vec![Value::resource(&m.root)]);
        assert!(m.routes().is_empty());
    }

    phase directions_by_public_transport_find_journeys;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        m.call(
            "set_mode",
            vec![
                Value::resource(&m.root),
                m.case("TravelMode", "TravelMode.transit"),
            ],
        );
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        let journeys = m.journeys();
        assert_eq!(journeys.len(), 2, "both journeys are kept");
        let first = journeys[0].clone();
        assert_eq!(m.text(&first, "dep"), "10:00");
        assert_eq!(m.text(&first, "arr"), "10:25");
        let legs = m.record_values(&first, "legs");
        assert_eq!(legs.len(), 1);
        assert_eq!(m.text_of(&legs[0], "line"), "58");
        assert_eq!(m.text_of(&legs[0], "from_name"), "Marienplatz");
        assert_eq!(m.text_of(&legs[0], "colour"), "0066cc");
        assert!(m.flag_of(&legs[0], "realtime"), "the bus reports live times");
        // The leg's own line is what the map draws it along, decoded from the service's polyline.
        match m.of(&legs[0], "geometry") {
            Value::List(points) => {
                assert_eq!(points.len(), 6, "three points, latitude then longitude: {points:?}");
                assert!(matches!(points[0], Value::Float(n) if (n - 38.5).abs() < 1e-6), "{points:?}");
                assert!(matches!(points[1], Value::Float(n) if (n + 120.2).abs() < 1e-6), "{points:?}");
                assert!(matches!(points[4], Value::Float(n) if (n - 43.252).abs() < 1e-6), "{points:?}");
            }
            other => panic!("a leg's line reads {other:?}"),
        }
        assert!(
            m.web.asked().iter().any(|url| url.contains("transitous")),
            "the transit service was asked"
        );
        m.call("another_journey", vec![Value::resource(&m.root)]);
        assert!(matches!(m.read(&m.root, "journey_at"), Value::Int(1)));
        m.call("clear_transit", vec![Value::resource(&m.root)]);
        assert!(m.journeys().is_empty());
    }

    phase the_time_transit_asks_for_moves_the_departures;
    {
        let m = Maps::new();
        m.call(
            "set_transit_time",
            vec![Value::resource(&m.root), Value::Bool(true), Value::Int(30)],
        );
        m.search("Marienplatz");
        m.call(
            "set_mode",
            vec![
                Value::resource(&m.root),
                m.case("TravelMode", "TravelMode.transit"),
            ],
        );
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        let asked = m.web.asked();
        let transit = asked
            .iter()
            .find(|url| url.contains("transitous"))
            .unwrap_or_else(|| panic!("the transit service was asked: {asked:?}"));
        assert!(
            transit.contains("arriveBy=true"),
            "the service is asked for a time to arrive by: {transit}"
        );
        assert!(
            transit.contains("time=2026-11-24T09:30:00Z"),
            "the moment the person chose reaches the service: {transit}"
        );
    }

    phase nearby_takes_in_the_places_that_stand_around_and_opens_one;
    {
        let m = Maps::new();
        m.call("find_nearby", vec![Value::resource(&m.root)]);
        let pois = m.pois();
        assert_eq!(pois.len(), 2);
        assert_eq!(m.text(&pois[0], "name"), "Café Glockenspiel");
        assert_eq!(m.text(&pois[0], "hours"), "8:00-20:00");
        assert_eq!(m.text(&pois[0], "address"), "Marienplatz 1");
        let museum = pois[1].clone();
        assert_eq!(m.text(&museum, "website"), "https://example.org");
        let key = m.text(&museum, "key");
        m.call(
            "open_poi",
            vec![Value::resource(&m.root), Value::Text(key)],
        );
        assert!(
            m.found().iter().any(|p| m.text(p, "name") == "Münchner Stadtmuseum"),
            "a place of interest opens as a place the map knows"
        );
    }

    phase the_device_says_where_it_is_and_no_where_it_cannot;
    {
        let m = Maps::new();
        m.located.set(true);
        m.call("locate", vec![Value::resource(&m.root)]);
        assert_eq!(m.web.asked(), Vec::<String>::new(), "locating asks no service");
        assert!((m.float(&m.root, "centre_latitude") - 48.1374).abs() < 1e-6);
        assert!(
            m.places().iter().any(|p| m.text(p, "name") == "Current location"),
            "where the device is stands on the map"
        );
        // A device that cannot say where it is says why, and shows nothing.
        let m = Maps::new();
        m.located.set(false);
        m.call("locate", vec![Value::resource(&m.root)]);
        assert_eq!(m.text(&m.root, "status"), "Location is unavailable right now");
        assert!(
            m.places().iter().all(|p| m.text(p, "name") != "Current location"),
            "nothing is shown where the device could not say"
        );
    }

    phase a_point_shows_where_it_is_and_one_off_earth_does_not;
    {
        let m = Maps::new();
        m.call(
            "show_point",
            vec![Value::resource(&m.root), Value::Float(52.52), Value::Float(13.40)],
        );
        assert!((m.float(&m.root, "centre_latitude") - 52.52).abs() < 1e-6);
        assert!((m.float(&m.root, "centre_longitude") - 13.40).abs() < 1e-6);
        m.call(
            "show_point",
            vec![Value::resource(&m.root), Value::Float(120.0), Value::Float(13.0)],
        );
        assert_eq!(m.text(&m.root, "status"), "That point is not on Earth.");
        assert!(
            (m.float(&m.root, "centre_latitude") - 52.52).abs() < 1e-6,
            "the map stays where it was"
        );
    }

    phase an_area_is_kept_on_the_device_listed_and_removed;
    {
        let m = Maps::new();
        m.call(
            "download_view",
            vec![
                Value::resource(&m.root),
                Value::Float(48.18),
                Value::Float(48.10),
                Value::Float(11.62),
                Value::Float(11.55),
                Value::Int(10),
                Value::Int(12),
            ],
        );
        // The device's tiles engine is asked for the box on screen, its levels and the address
        // its imagery comes from.
        let Started { job, input: ask, .. } = m.engine.last_started();
        let ask: serde_json::Value = serde_json::from_str(&ask).expect("the ask reads");
        assert_eq!(ask["north"], 48.18);
        assert_eq!(ask["south"], 48.10);
        assert_eq!(ask["east"], 11.62);
        assert_eq!(ask["west"], 11.55);
        assert_eq!(ask["least_zoom"], 10);
        assert_eq!(ask["most_zoom"], 12);
        assert!(
            ask["basemap"]
                .as_str()
                .is_some_and(|address| address.contains("tile.openstreetmap.org")),
            "the area keeps the map's own imagery: {ask}"
        );
        // It is listed as this device's while it downloads, and the row stops it.
        let areas = m.offline();
        assert_eq!(areas.len(), 1, "the area is listed while it downloads");
        let area = areas[0].clone();
        assert_eq!(m.text(&area, "name"), "This view");
        assert_eq!(
            m.text(&area, "job"),
            job,
            "the area knows the job keeping it"
        );
        // What the job says of itself is what the row shows.
        m.engine.moves(&m.rt, &job, "running", &serde_json::json!({"done": 5, "total": 100}));
        let mut running = m
            .rt
            .open_view(
                &me(),
                &m.app.view("area_row"),
                vec![Value::resource(&m.root), Value::resource(&area)],
            )
            .unwrap();
        running.poll_patches().unwrap();
        let said: Vec<String> = of_kind(running.tree(), "ui.Text")
            .iter()
            .filter_map(|node| match node.props.get("value") {
                Some(Value::Text(text)) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            said.iter().any(|text| text == "5 of 100 tiles"),
            "the row reads the job's record: {said:?}"
        );
        let offered: Vec<String> = of_kind(running.tree(), "ui.Button")
            .iter()
            .filter_map(|node| match node.props.get("label") {
                Some(Value::Text(label)) => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(
            offered.iter().any(|label| label == "Stop downloading"),
            "the row stops a running download: {offered:?}"
        );
        // A download the person stops ends, and what it got stays listed and kept.
        m.call(
            "stop_offline_area",
            vec![Value::resource(&m.root), Value::Text("This view".to_owned())],
        );
        assert_eq!(
            m.engine.cancelled().as_slice(),
            std::slice::from_ref(&job)
        );
        assert!(m.flag(&area, "stopped"));
        m.engine.moves(&m.rt, &job, "cancelled", &serde_json::json!({"done": 5, "total": 100}));
        assert_eq!(
            m.float(&area, "done").to_bits(),
            5.0_f64.to_bits(),
            "what it got before the stop stays"
        );
        assert_eq!(m.float(&area, "tiles").to_bits(), 100.0_f64.to_bits());
        assert!(m.text(&m.root, "status").contains("stay available offline"));
        // A stopped area's row offers the download again, which starts a new job for the rest.
        m.call(
            "continue_offline_area",
            vec![Value::resource(&m.root), Value::Text("This view".to_owned())],
        );
        let Started { job: again, input: again_ask, .. } = m.engine.last_started();
        assert_ne!(again, job, "continuing starts a job of its own");
        let again_ask: serde_json::Value = serde_json::from_str(&again_ask).unwrap();
        assert_eq!(again_ask["id"], ask["id"], "the same footprint, so its tiles are reused");
        m.engine.moves(&m.rt, &again, "done", &serde_json::json!({"done": 100, "total": 100}));
        assert_eq!(m.float(&area, "done").to_bits(), 100.0_f64.to_bits());
        assert_eq!(
            m.text(&m.root, "status"),
            "Offline area ready (100 tiles)"
        );
        assert!(!m.flag(&area, "stopped"));
        // Deleting the area asks the device to give up its tiles and takes it off the list.
        m.call(
            "delete_offline_area",
            vec![Value::resource(&m.root), Value::Text("This view".to_owned())],
        );
        assert!(
            m.engine.was_asked("forget-area"),
            "the device gives up the area's tiles: {:?}",
            m.engine.asked()
        );
        assert!(m.offline().is_empty(), "the device lists it no more");
        // Downloading a place asks the geocoder where it is and keeps that box.
        m.call(
            "download_place",
            vec![
                Value::resource(&m.root),
                Value::Text("Café".to_owned()),
                Value::Int(10),
                Value::Int(12),
            ],
        );
        assert_eq!(m.offline().len(), 1);
        assert_eq!(m.text(&m.offline()[0], "name"), "Marienplatz");
        let Started { job: place_job, input: place_ask, .. } = m.engine.last_started();
        let place_ask: serde_json::Value = serde_json::from_str(&place_ask).unwrap();
        assert!(
            (place_ask["north"].as_f64().unwrap_or_default() - 48.1874).abs() < 1e-6,
            "the place's box is around where it is: {place_ask}"
        );
        m.engine.moves(&m.rt, &place_job, "done", &serde_json::json!({"done": 60, "total": 60}));
        assert_eq!(m.text(&m.root, "status"), "Offline area ready (60 tiles)");
        // Clearing asks the device to give up every saved picture and lists none.
        m.call("clear_offline_areas", vec![Value::resource(&m.root)]);
        assert!(
            m.engine.was_asked("clear-saved"),
            "the device gives up every saved picture: {:?}",
            m.engine.asked()
        );
        assert!(m.offline().is_empty());
    }

    phase the_map_fits_what_it_shows_and_the_directions_name_both_ends;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        // Fitting the map centres it on the route and draws in to the route's own size.
        m.call("fit_map", vec![Value::resource(&m.root)]);
        let latitude = m.float(&m.root, "centre_latitude");
        let longitude = m.float(&m.root, "centre_longitude");
        assert!(
            (latitude - 48.13625).abs() < 0.01,
            "the centre is the route's own: {latitude}"
        );
        assert!(
            (longitude - 11.57875).abs() < 0.01,
            "the centre is the route's own: {longitude}"
        );
        let zoom = m.float(&m.root, "centre_zoom");
        assert!(
            (40_075_016.686 * (latitude * std::f64::consts::PI / 180.0).cos() / (1_048_576.0 * zoom) - 0.8)
                .abs()
                < 0.4,
            "about 0.8 metres of the route stand for a screen pixel: {zoom}"
        );
        // The directions name both ends, offer a swap, and carry the map's keys. They stand in
        // the window's places column, which `2` opens.
        let mut panel = m.window();
        panel.poll_patches().unwrap();
        let directions = of_kind(panel.tree(), "ui.Button")
            .into_iter()
            .find(|button| match button.props.get("keys") {
                Some(Value::List(keys)) => keys.contains(&Value::text("2")),
                _ => false,
            })
            .expect("a control answers `2`")
            .key
            .clone();
        panel.fire(&directions, "press").unwrap();
        panel.poll_patches().unwrap();
        let placeholders: Vec<String> = of_kind(panel.tree(), "ui.TextInput")
            .iter()
            .filter_map(|node| match node.props.get("placeholder") {
                Some(Value::Text(text)) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            placeholders.iter().any(|text| text == "From")
                && placeholders.iter().any(|text| text == "To"),
            "directions name where they start and where they end: {placeholders:?}"
        );
        let offered: Vec<&UiNode> = of_kind(panel.tree(), "ui.Button");
        for wanted in ["Swap from and to", "Get directions", "Fit the map"] {
            assert!(
                offered
                    .iter()
                    .any(|node| props_text(node, "label") == wanted),
                "the panel offers `{wanted}`"
            );
        }
        let keys: Vec<String> = offered
            .iter()
            .flat_map(|node| match node.props.get("keys") {
                Some(Value::List(keys)) => keys
                    .iter()
                    .filter_map(|key| match key {
                        Value::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        assert!(
            keys.iter().any(|key| key == "d") && keys.iter().any(|key| key == "f"),
            "the window's keys reach directions and the fit: {keys:?}"
        );
    }

    phase a_saved_place_is_renamed_and_keeps_its_name;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        let place = m
            .places()
            .into_iter()
            .find(|p| m.text(p, "name") == "Marienplatz")
            .expect("the place the geocoder answered is on the map");
        m.call("save_place", vec![Value::resource(&place)]);
        let where_it_is = m.text(&place, "key");
        // A saved place is named by what the search brought until the person renames it.
        m.call(
            "rename_place",
            vec![
                Value::resource(&place),
                Value::Text("  Office  ".to_owned()),
            ],
        );
        assert_eq!(m.text(&place, "name"), "Office", "the typed name, trimmed");
        assert_eq!(
            m.text(&place, "key"),
            where_it_is,
            "renaming keeps where the place is"
        );
        // The action refuses a place with no name of its own.
        let failed = m.rt.invoke_action(
            &me(),
            (
                m.app.action("rename_place").as_str(),
                vec![Value::resource(&place), Value::text("   ")],
            ),
        );
        assert!(failed.is_err(), "a place needs a name");
        // The place's own page offers the rename beside its save and directions.
        let mut view = m
            .rt
            .open_view(&me(), &m.app.view("place"), vec![Value::resource(&place)])
            .unwrap();
        view.poll_patches().unwrap();
        let labels: Vec<String> = of_kind(view.tree(), "ui.Button")
            .iter()
            .filter_map(|node| match node.props.get("label") {
                Some(Value::Text(label)) => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(
            labels.iter().any(|label| label == "Rename"),
            "the place's page offers Rename: {labels:?}"
        );
    }

    phase a_geo_link_searches_or_shows_its_point;
    {
        let m = Maps::new();
        let opened = m.call(
            "open_link",
            vec![
                Value::resource(&m.root),
                Value::Text("geo:0,0?q=Marienplatz".to_owned()),
            ],
        );
        assert_eq!(m.text(&m.root, "query"), "Marienplatz", "the link searched");
        assert!(
            matches!(opened, InvokeOutcome::Done(Value::Ref(_))),
            "the action answers the map's resource: {opened:?}"
        );
        let m = Maps::new();
        m.call(
            "open_link",
            vec![
                Value::resource(&m.root),
                Value::Text("geo:52.52,13.40".to_owned()),
            ],
        );
        assert!(
            (m.float(&m.root, "centre_latitude") - 52.52).abs() < 1e-6,
            "the link showed its point"
        );
    }

    phase the_traffic_layer_is_the_person_s_own_and_shows_its_legend;
    {
        let m = Maps::new();
        let mut window = m.window();
        window.poll_patches().unwrap();
        let legend = |window: &ViewInstance| {
            of_kind(window.tree(), "ui.Chip")
                .into_iter()
                .filter_map(|chip| match chip.props.get("label") {
                    Some(Value::Text(label)) => Some(label.clone()),
                    _ => None,
                })
                .collect::<Vec<String>>()
        };
        assert!(
            legend(&window).is_empty(),
            "the traffic layer starts off: {:?}",
            legend(&window)
        );
        let traffic = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .find(|button| props_text(button, "label") == "Show traffic")
            .expect("the map offers the traffic layer")
            .key
            .clone();
        window.fire(&traffic, "press").unwrap();
        window.poll_patches().unwrap();
        let shown = legend(&window);
        for wanted in ["Flowing", "Slow", "Heavy", "Standstill"] {
            assert!(
                shown.iter().any(|label| label == wanted),
                "the traffic layer names `{wanted}`: {shown:?}"
            );
        }
        // The layer is the person's own state, not the workspace's: the shared map carries no
        // traffic flag for everyone to see.
        assert!(
            m.rt.read(&m.root, "traffic").is_err(),
            "the layer is not a field of the workspace's map"
        );
    }

    phase the_places_beside_the_map_are_ranked_and_say_how_far_they_are;
    {
        let m = Maps::new();
        *m.web.twin.borrow_mut() = true;
        m.search("Marienplatz");
        assert_eq!(
            m.found().len(),
            1,
            "one place is one place, however many records name it: {:?}",
            m.found().iter().map(|p| m.text(p, "name")).collect::<Vec<_>>()
        );
        let m = Maps::new();
        m.search("Marienplatz");
        let found = m.found();
        assert_eq!(m.text(&found[0], "name"), "Marienplatz");
        assert_eq!(
            m.text(&found[1], "name"),
            "Café Glockenspiel",
            "the name that answers the search stands first: {found:?}"
        );
        // A row says how far its place lies from where the camera looks.
        let mut window = m.window();
        window.poll_patches().unwrap();
        let map = of_kind(window.tree(), "ui.Map")
            .into_iter()
            .next()
            .expect("the window shows the map")
            .key
            .clone();
        window
            .fire_with(
                &map,
                "rest",
                Some(&Value::List(vec![
                    Value::Float(48.1374),
                    Value::Float(11.5755),
                    Value::Float(4.0),
                    Value::Float(48.16),
                    Value::Float(48.11),
                    Value::Float(11.61),
                    Value::Float(11.54),
                ])),
            )
            .expect("the camera rests");
        window.poll_patches().unwrap();
        let row = of_kind(window.tree(), "ui.ListRow")
            .into_iter()
            .find(|row| props_text(row, "title") == "Marienplatz")
            .expect("the place stands in the results");
        assert!(
            props_text(row, "secondary").ends_with("0 m"),
            "the row says the place is where the camera looks: {:?}",
            props_text(row, "secondary")
        );
    }

    phase the_map_s_controls_carry_the_one_and_two_keys;
    {
        let m = Maps::new();
        let mut window = m.window();
        window.poll_patches().unwrap();
        let keyed = |window: &ViewInstance, key: &str| {
            of_kind(window.tree(), "ui.Button")
                .into_iter()
                .find(|button| match button.props.get("keys") {
                    Some(Value::List(keys)) => keys.contains(&Value::text(key)),
                    _ => false,
                })
                .cloned()
        };
        let directions = keyed(&window, "2").expect("a control answers `2`");
        assert_eq!(
            props_text(&directions, "label"),
            "Directions",
            "`2` opens the directions"
        );
        assert!(keyed(&window, "1").is_some(), "a control answers `1`");
        window.fire(&directions.key, "press").unwrap();
        window.poll_patches().unwrap();
        let expanded = of_kind(window.tree(), "ui.DisclosureRow")
            .into_iter()
            .find(|node| props_text(node, "title") == "Directions")
            .and_then(|node| node.props.get("expanded").cloned())
            .unwrap_or_else(Value::none);
        assert_eq!(expanded, Value::Bool(true), "`2` opened the directions");
    }

    phase a_geo_link_reads_its_escapes_and_its_uncertainty;
    {
        let m = Maps::new();
        m.call(
            "open_link",
            vec![
                Value::resource(&m.root),
                Value::text("geo:0,0?q=Caf%C3%A9%20Glockenspiel"),
            ],
        );
        assert_eq!(
            m.text(&m.root, "query"),
            "Café Glockenspiel",
            "the percent escapes are the words the link says"
        );
        // An uncertainty is how far the point may lie from the coordinates: the map opens close
        // enough to see that much ground, and says what it shows.
        let m = Maps::new();
        m.call(
            "open_link",
            vec![
                Value::resource(&m.root),
                Value::text("geo:48.1374,11.5755;u=250"),
            ],
        );
        assert!((m.float(&m.root, "centre_latitude") - 48.1374).abs() < 1e-6);
        let near = m.float(&m.root, "centre_zoom");
        assert!(near > 10.0, "250 m of uncertainty opens a street's view: {near}");
        let m = Maps::new();
        m.call(
            "open_link",
            vec![
                Value::resource(&m.root),
                Value::text("geo:48.1374,11.5755;u=5000"),
            ],
        );
        let far = m.float(&m.root, "centre_zoom");
        assert!(far < near, "five kilometres of uncertainty opens a town's view: {far} for {near}");
        let m = Maps::new();
        m.call(
            "open_link",
            vec![
                Value::resource(&m.root),
                Value::text("geo:48.1374,11.5755;u=250"),
            ],
        );
        assert!(
            m.text(&m.root, "status").contains("250 m"),
            "it says how far the point may be: {}",
            m.text(&m.root, "status")
        );
    }

    phase a_place_s_card_says_its_phone_and_website_and_opens_them;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        let place = m.found()[0].clone();
        assert_eq!(m.text(&place, "phone"), "+49 89 23300");
        assert_eq!(m.text(&place, "website"), "https://www.muenchen.de/marienplatz");
        let mut window = m.window();
        window.poll_patches().unwrap();
        let map = of_kind(window.tree(), "ui.Map")
            .into_iter()
            .next()
            .expect("the window shows the map")
            .key
            .clone();
        window
            .fire_with(&map, "marker", Some(&Value::text("Marienplatz")))
            .expect("the marker opens the place");
        window.poll_patches().unwrap();
        let rows: Vec<(String, String)> = of_kind(window.tree(), "ui.ListRow")
            .into_iter()
            .map(|row| (props_text(row, "title"), props_text(row, "meta")))
            .collect();
        assert!(
            rows.iter().any(|(title, meta)| title == "+49 89 23300" && meta == "Call"),
            "the card names the phone number: {rows:?}"
        );
        assert!(
            rows.iter().any(|(title, meta)| title == "https://www.muenchen.de/marienplatz"
                && meta == "Website"),
            "the card names the website: {rows:?}"
        );
        // A place with neither offers neither.
        window
            .fire_with(&map, "marker", Some(&Value::text("Café Glockenspiel")))
            .expect("the marker opens the place");
        window.poll_patches().unwrap();
        let offered: Vec<(String, String)> = of_kind(window.tree(), "ui.ListRow")
            .into_iter()
            .map(|row| (props_text(row, "title"), props_text(row, "meta")))
            .collect();
        assert!(
            !offered.iter().any(|(_, meta)| meta == "Call" || meta == "Website"),
            "a place with no phone or website offers neither: {offered:?}"
        );
    }

    phase the_window_says_when_the_imagery_is_not_arriving;
    {
        let m = Maps::new();
        let mut window = m.window();
        window.poll_patches().unwrap();
        let map = of_kind(window.tree(), "ui.Map")
            .into_iter()
            .next()
            .expect("the window shows the map")
            .key
            .clone();
        let hint = |window: &ViewInstance| {
            of_kind(window.tree(), "ui.Banner")
                .into_iter()
                .any(|banner| props_text(banner, "title").contains("Map imagery"))
        };
        assert!(!hint(&window), "imagery arriving says nothing");
        window
            .fire_with(&map, "fault", Some(&Value::List(vec![Value::Int(6), Value::Int(6)])))
            .expect("the map reports its tiles");
        window.poll_patches().unwrap();
        assert!(hint(&window), "every tile failing says so quietly");
        window
            .fire_with(&map, "fault", Some(&Value::List(vec![Value::Int(0), Value::Int(6)])))
            .expect("the map reports its tiles");
        window.poll_patches().unwrap();
        assert!(!hint(&window), "imagery that arrives takes the hint away");
    }

    phase a_journey_is_drawn_along_each_leg_s_own_line;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        m.call(
            "set_mode",
            vec![
                Value::resource(&m.root),
                m.case("TravelMode", "TravelMode.transit"),
            ],
        );
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        let window = m.window();
        let tracks = of_kind(window.tree(), "ui.MapTrack");
        assert_eq!(tracks.len(), 1, "the journey's one leg is drawn: {tracks:?}");
        assert_eq!(
            props_text(tracks[0], "tone"),
            "transit",
            "the leg's line carries its mode's ink"
        );
        match tracks[0].props.get("points") {
            Some(Value::List(points)) => assert_eq!(points.len(), 6, "the leg's own line: {points:?}"),
            other => panic!("the line is drawn through its points: {other:?}"),
        }
    }

    phase navigation_follows_the_route_and_ends;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        m.call("start_navigation", vec![Value::resource(&m.root)]);
        assert!(m.flag(&m.root, "navigating"));
        // The shell's live card is the app's live view, drawn from the map.
        let live = m
            .rt
            .open_view(&me(), &live_view(&m), vec![Value::resource(&m.root)])
            .unwrap();
        assert!(
            of_kind(live.tree(), "ui.Text")
                .iter()
                .any(|node| props_text(node, "value") == "Turn left onto Sendlinger Straße"),
            "the live card says the turn ahead"
        );
        m.call("end_navigation", vec![Value::resource(&m.root)]);
        assert!(!m.flag(&m.root, "navigating"));
    }

    phase a_failed_request_says_why_in_words;
    {
        let m = Maps::new();
        *m.web.offline.borrow_mut() = true;
        m.search("Marienplatz");
        assert_eq!(
            m.text(&m.root, "status"),
            "There is no connection to the map service. Check your connection and try again."
        );
        assert!(m.found().is_empty(), "nothing was taken in");
    }

    }

    tilo_test_env::scenario! {
        fn a_map_window_draws_its_imagery_places_and_route;

    phase the_window_draws_the_map_with_its_imagery_markers_and_route;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        let first = m.found()[0].clone();
        m.call("save_place", vec![Value::resource(&first), Value::Bool(true)]);
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        let window = m.window();
        let map = of_kind(window.tree(), "ui.Map");
        assert_eq!(map.len(), 1, "the window shows the map");
        let map = &map[0];
        assert!(
            props_text(map, "basemap").contains("tile.openstreetmap.org")
                && props_text(map, "basemap").contains("%z"),
            "the map draws OpenStreetMap's imagery: {:?}",
            props_text(map, "basemap")
        );
        assert!(props_text(map, "name") == "Map");
        // The places it knows stand on it as markers, and the route as a line.
        let markers = of_kind(window.tree(), "ui.MapMarker");
        for marker in &markers {
            assert_eq!(
                marker.props.get("label"),
                Some(&Value::Bool(true)),
                "a place's marker names it: {:?}",
                marker.props.get("name")
            );
        }
        assert_eq!(
            markers.len(),
            2,
            "a kept place and the search's two places stand on the map"
        );
        let tracks = of_kind(window.tree(), "ui.MapTrack");
        assert_eq!(tracks.len(), 1, "the route is drawn");
        // A press on a marker reports the place it stands for, and the view opens it: the map
        // moves to the place the person kept, which the route it drew ended away from.
        let pressed = window
            .fire_with(map.key.as_str(), "marker", Some(&Value::text("Marienplatz")))
            .expect("the marker press runs");
        assert!(
            !matches!(pressed, InvokeOutcome::Done(Value::Error(_))),
            "the map opened the place: {pressed:?}"
        );
        assert!(
            (m.float(&m.root, "centre_latitude") - 48.1374).abs() < 1e-6,
            "the map shows the place the marker stands for"
        );
        assert_eq!(m.text(&m.root, "status"), "Marienplatz");
        // The window says who the imagery belongs to, and lets the person open the offline maps.
        let buttons: Vec<String> = of_kind(window.tree(), "ui.Button")
            .iter()
            .filter_map(|node| match node.props.get("label") {
                Some(Value::Text(label)) => Some(label.clone()),
                _ => None,
            })
            .collect();
        for wanted in [
            "Zoom in",
            "Zoom out",
            "Use my location",
            "Offline maps",
            "Attribution",
            "Show traffic",
            "Hide places",
            "Directions",
        ] {
            assert!(
                buttons.iter().any(|label| label == wanted),
                "the map offers `{wanted}`: {buttons:?}"
            );
        }
        assert!(
            of_kind(window.tree(), "ui.Text")
                .iter()
                .any(|node| props_text(node, "value").contains("OpenStreetMap contributors")),
            "the map credits its data"
        );
    }

    phase the_window_saves_what_the_camera_came_to_rest_on;
    {
        let m = Maps::new();
        let mut window = m.window();
        window.poll_patches().unwrap();
        let map = of_kind(window.tree(), "ui.Map")[0].clone();
        // The map reports where the camera rests: its middle, its zoom, and its four edges.
        window
            .fire_with(
                map.key.as_str(),
                "rest",
                Some(&Value::List(vec![
                    Value::Float(50.0),
                    Value::Float(8.0),
                    Value::Float(4.0),
                    Value::Float(50.2),
                    Value::Float(49.8),
                    Value::Float(8.3),
                    Value::Float(7.7),
                ])),
            )
            .expect("the camera at rest runs");
        window.poll_patches().unwrap();
        assert!(
            of_kind(window.tree(), "ui.Text")
                .iter()
                .any(|node| props_text(node, "value") == "500 m"),
            "the footer reads the scale where the camera rests"
        );
        // And draws it: a rule as long as that distance covers on the ground.
        let rule = of_kind(window.tree(), "ui.Rule")
            .into_iter()
            .next()
            .expect("the footer draws the scale");
        match rule.props.get("length") {
            Some(Value::Float(length)) => {
                assert!(
                    *length > 60.0 && *length < 95.0,
                    "the bar is as long as 500 m covers at this zoom: {length}"
                );
            }
            other => panic!("the scale bar is a length: {other:?}"),
        }
        // Opening the offline maps and saving the view asks for exactly that box.
        let offline = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .find(|node| props_text(node, "label") == "Offline maps")
            .expect("the window offers the offline maps")
            .key
            .clone();
        window.fire(&offline, "press").expect("the sheet opens");
        window.poll_patches().unwrap();
        let save = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .find(|node| props_text(node, "label") == "Download this view")
            .expect("the sheet offers to save what the map shows")
            .key
            .clone();
        window.fire(&save, "press").expect("the view is saved");
        let Started { job: _job, input: ask, .. } = m.engine.last_started();
        let ask: serde_json::Value = serde_json::from_str(&ask).expect("the ask reads");
        assert_eq!(ask["north"], 50.2);
        assert_eq!(ask["south"], 49.8);
        assert_eq!(ask["east"], 8.3);
        assert_eq!(ask["west"], 7.7);
        assert_eq!(ask["least_zoom"], 10);
        assert_eq!(ask["most_zoom"], 15);
    }

    phase the_attribution_sheet_says_whose_data_the_map_is;
    {
        let m = Maps::new();
        let mut window = m.window();
        window.poll_patches().unwrap();
        let attribution = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .find(|node| props_text(node, "label") == "Attribution")
            .expect("the window offers to say whose data its imagery is")
            .key
            .clone();
        window.fire(&attribution, "press").expect("the sheet opens");
        window.poll_patches().unwrap();
        let dialogs = of_kind(window.tree(), "ui.ModalDialog");
        assert_eq!(dialogs.len(), 1, "the attribution sheet stands open");
        assert_eq!(
            dialogs[0].props.get("open"),
            Some(&Value::Bool(true)),
            "the sheet is open: {:?}",
            dialogs[0].props.get("open")
        );
        assert_eq!(props_text(dialogs[0], "title"), "Attribution");
        assert!(
            of_kind(window.tree(), "ui.Text").iter().any(|node| {
                props_text(node, "value") == "Map data and map tiles © OpenStreetMap contributors."
            }),
            "the sheet says whose data the map is"
        );
    }

    phase the_window_shows_who_is_at_a_place;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        let at = m.found()[0].clone();
        // Another window's view at the place: who is there comes from the system's presence, and
        // the map docks them on the place's marker.
        let scout = Actor::from(MemberId::agent("scout", "ada"));
        m.rt.set_lenses(&at.id_string(), &[(scout, false, false)]);
        let presence = |m: &Maps| {
            let window = m.window();
            let marker = of_kind(window.tree(), "ui.MapMarker")
                .into_iter()
                .find(|node| node.props.get("name") == Some(&Value::text("Marienplatz")))
                .expect("the place stands on the map");
            marker.props.get("presence").cloned().unwrap_or_else(Value::none)
        };
        // The lens's `who`: the actor itself, or the option holding it.
        let at_the_place = presence(&m);
        let who = match &at_the_place {
            Value::Actor(actor) => Some(actor),
            Value::Opt(Some(held)) => match held.as_ref() {
                Value::Actor(actor) => Some(actor),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(
            who.and_then(Actor::member_id),
            Some(&MemberId::agent("scout", "ada")),
            "the window docks the agent the lens names: {at_the_place:?}"
        );
        // The person's own view is not someone else at the place.
        m.rt.set_lenses(
            &at.id_string(),
            &[(Actor::from(MemberId::agent("scout", "ada")), true, false)],
        );
        let own = presence(&m);
        assert!(
            matches!(own, Value::Opt(None) | Value::Missing),
            "a person's own view docks nobody on the place: {own:?}"
        );

    }

    }

    tilo_test_env::scenario! {
        fn a_map_estimates_a_refused_route_and_an_offline_area_size;

    phase a_refused_route_is_estimated_on_this_device;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        *m.web.offline.borrow_mut() = true;
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        assert!(
            m.routes().is_empty(),
            "a refused route is never written into the workspace's map"
        );
        let estimate = m.device_root("Estimate");
        let held = m.records(&estimate, "routes");
        assert_eq!(held.len(), 1, "one estimate stands while no real route does");
        let route = held[0].clone();
        assert!(
            m.float(&route, "distance_m") > 0.0,
            "the estimate is a real distance between the two places"
        );
        assert!(
            m.float(&route, "duration_s") > 0.0,
            "and a real time for the mode it travels"
        );
        let steps = m.record_values(&route, "steps");
        assert_eq!(m.text_of(&steps[0], "instruction"), "Head east to start");
        assert_eq!(
            m.text_of(&steps[1], "instruction"),
            "Turn right toward destination"
        );
        assert_eq!(
            m.text_of(&steps[2], "instruction"),
            "Arrive at your destination"
        );
        assert!(
            m.text(&m.root, "status").contains("The offline estimate is shown."),
            "the status says the route is an estimate: {}",
            m.text(&m.root, "status")
        );

        // A route the service does find replaces the estimate and clears it.
        *m.web.offline.borrow_mut() = false;
        m.call(
            "directions",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::Text("Café Glockenspiel".to_owned()),
            ],
        );
        assert!(!m.routes().is_empty(), "the service's route is kept");
        assert!(
            m.records(&estimate, "routes").is_empty(),
            "the estimate is cleared once a real route is found"
        );
    }

    phase an_offline_area_quotes_the_size_it_will_take;
    {
        let m = Maps::new();
        m.call(
            "estimate_view",
            vec![
                Value::resource(&m.root),
                Value::Float(48.18),
                Value::Float(48.10),
                Value::Float(11.62),
                Value::Float(11.55),
                Value::Int(14),
                Value::Int(14),
            ],
        );
        assert_eq!(
            m.read(&m.root, "estimate_tiles"),
            Value::Int(12),
            "the tiles engine counts the area's pictures"
        );
        assert_eq!(
            m.read(&m.root, "estimate_bytes"),
            Value::Int(216_000),
            "and quotes what they take at the size it measures"
        );
    }

    }

    /// The live view of the app's map, by its name.
    fn live_view(m: &Maps) -> String {
        m.app.view("live")
    }

    /// The text a node's prop holds.
    fn props_text(node: &UiNode, prop: &str) -> String {
        match node.props.get(prop) {
            Some(Value::Text(text)) => text.clone(),
            _ => String::new(),
        }
    }
}
