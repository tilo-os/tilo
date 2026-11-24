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
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::ids::MemberId;
    use tilolang_runtime::run::WebRequest;
    use tilolang_runtime::web::{WebReply, WebTransport};
    use tilolang_runtime::{
        Actor, AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode,
        Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/maps/app.tl");

    /// What the geocoder answers for Munich.
    const MUNICH: &str = r#"[
        {"lat":"48.1374","lon":"11.5755","display_name":"Marienplatz, München, Bayern","name":"Marienplatz","osm_type":"node","osm_id":11,"class":"tourism","type":"attraction","address":{"road":"Marienplatz"}},
        {"lat":"48.1351","lon":"11.5820","display_name":"Café Glockenspiel, München","name":"Café Glockenspiel","osm_type":"node","osm_id":12,"class":"amenity","type":"cafe","address":{"road":"Marienplatz","house_number":"1"}}
    ]"#;

    /// What the geocoder answers for a place the tests name but it does not know.
    const UNKNOWN: &str = "[]";

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
                  "intermediateStops":[{},{}],"routeColor":"#0066cc","agencyName":"MVG","realTime":true}]},
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

    /// The device's location service, as `engine.call("maps.locate")` reaches it.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<String>>>,
        /// Whether the device can say where it is.
        found: Rc<RefCell<bool>>,
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            self.asked.borrow_mut().push(op.to_owned());
            if op != "maps.locate" {
                return Err(format!("`{op}` is no op this device runs"));
            }
            if *self.found.borrow() {
                Ok(r#"{"lat":48.1374,"lon":11.5755}"#.to_owned())
            } else {
                Ok(r#"{"why":"Location is unavailable right now"}"#.to_owned())
            }
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            _input: &str,
            _handlers: &tilolang_runtime::host_work::JobHandlers,
        ) -> Result<String, String> {
            Err(format!("`{op}` starts no job"))
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            let said = || -> Result<String, String> { Err(format!("no job `{job}`")) };
            Ok(said()
                .ok()
                .and_then(|said| tilolang_tilo::engine::record_of(job, &said)))
        }

        fn cancel(&self, _job: &str) -> bool {
            false
        }
    }

    struct Maps {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        web: Web,
        engine: Engine,
    }

    impl Maps {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let web = Web::default();
            let engine = Engine::default();
            rt.set_web(web.clone());
            rt.set_engine(engine.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let root = rt.create_root("Atlas").unwrap();
            Self {
                rt,
                app,
                root,
                web,
                engine,
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

        fn offline(&self) -> Vec<ResourceRef> {
            self.records(&self.root, "offline")
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
        assert!(
            m.web.asked().iter().any(|url| url.contains("arriveBy=true")),
            "the service is asked for a time to arrive by: {:?}",
            m.web.asked()
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
        *m.engine.found.borrow_mut() = true;
        m.call("locate", vec![Value::resource(&m.root)]);
        assert_eq!(m.web.asked(), Vec::<String>::new(), "locating asks no service");
        assert!((m.float(&m.root, "centre_latitude") - 48.1374).abs() < 1e-6);
        assert!(
            m.places().iter().any(|p| m.text(p, "name") == "Current location"),
            "where the device is stands on the map"
        );
        // A device that cannot say where it is says why, and shows nothing.
        let m = Maps::new();
        *m.engine.found.borrow_mut() = false;
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

    phase an_offline_area_counts_its_tiles_and_says_how_far_it_got;
    {
        let m = Maps::new();
        m.call(
            "download_view",
            vec![Value::resource(&m.root), Value::Int(10), Value::Int(12)],
        );
        let areas = m.offline();
        assert_eq!(areas.len(), 1);
        let area = areas[0].clone();
        let tiles = m.float(&area, "tiles");
        assert!(tiles > 0.0, "the area holds tiles: {tiles}");
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the area's tiles are far inside i64"
        )]
        let tile_count = tiles as i64;
        // The map says how far saving it has come; the last report says it is ready.
        m.call(
            "note_saved",
            vec![
                Value::resource(&m.root),
                Value::Text("This view".to_owned()),
                Value::List(vec![Value::Int(tile_count), Value::Int(tile_count)]),
            ],
        );
        assert!((m.float(&area, "done") - tiles).abs() < f64::EPSILON);
        assert_eq!(
            m.text(&m.root, "status"),
            format!("Offline area ready ({tile_count} tiles)")
        );
        m.call(
            "delete_offline_area",
            vec![
                Value::resource(&m.root),
                Value::Text("This view".to_owned()),
            ],
        );
        assert!(m.offline().is_empty());
        // Downloading a place asks the geocoder for the area it names.
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
        m.call(
            "note_saved",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::List(vec![Value::Int(5), Value::Int(100)]),
            ],
        );
        // The area's row carries the control while the download runs.
        let mut running = m
            .rt
            .open_view(
                &me(),
                &m.app.view("area_row"),
                vec![Value::resource(&m.root), Value::resource(&m.offline()[0])],
            )
            .unwrap();
        running.poll_patches().unwrap();
        let offered: Vec<String> = of_kind(running.tree(), "ui.Button")
            .iter()
            .filter_map(|node| match node.props.get("label") {
                Some(Value::Text(label)) => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(
            offered.iter().any(|label| label == "Stop downloading"),
            "the area's row stops a running download: {offered:?}"
        );
        // A download the person stops keeps what it already has and takes no more reports.
        m.call(
            "stop_offline_area",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
            ],
        );
        let kept = m.float(&m.offline()[0], "done");
        assert!((kept - 5.0).abs() < f64::EPSILON, "the tiles it got before the stop stay");
        assert_eq!(
            m.rt.read(&m.offline()[0], "stopped").unwrap(),
            Value::Bool(true)
        );
        m.call(
            "note_saved",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::List(vec![Value::Int(99), Value::Int(100)]),
            ],
        );
        assert!((m.float(&m.offline()[0], "done") - kept).abs() < f64::EPSILON, "a stopped area is not filled further");
        assert!(
            m.text(&m.root, "status").contains("stay available offline"),
            "it says what it keeps: {}",
            m.text(&m.root, "status")
        );
        // A stopped area's row offers the download again.
        let mut stopped = m
            .rt
            .open_view(
                &me(),
                &m.app.view("area_row"),
                vec![Value::resource(&m.root), Value::resource(&m.offline()[0])],
            )
            .unwrap();
        stopped.poll_patches().unwrap();
        let offered: Vec<String> = of_kind(stopped.tree(), "ui.Button")
            .iter()
            .filter_map(|node| match node.props.get("label") {
                Some(Value::Text(label)) => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(
            offered.iter().any(|label| label == "Download it again"),
            "a stopped area is downloaded again: {offered:?}"
        );
        // Starting it again lets its reports through.
        m.call(
            "continue_offline_area",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
            ],
        );
        m.call(
            "note_saved",
            vec![
                Value::resource(&m.root),
                Value::Text("Marienplatz".to_owned()),
                Value::List(vec![Value::Int(99), Value::Int(100)]),
            ],
        );
        assert!((m.float(&m.offline()[0], "done") - 99.0).abs() < f64::EPSILON);
        m.call("clear_offline_areas", vec![Value::resource(&m.root)]);
        assert!(m.offline().is_empty());
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

    phase the_traffic_layer_toggles;
    {
        let m = Maps::new();
        assert!(!m.flag(&m.root, "traffic"));
        m.call("toggle_traffic", vec![Value::resource(&m.root)]);
        assert!(m.flag(&m.root, "traffic"));
        m.call(
            "toggle_traffic",
            vec![Value::resource(&m.root), Value::Bool(false)],
        );
        assert!(!m.flag(&m.root, "traffic"));
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

    phase the_window_shows_who_is_at_a_place;
    {
        let m = Maps::new();
        m.search("Marienplatz");
        let at = m.found()[0].clone();
        // Another window's view at the place: who is there comes from the system's presence, and
        // the map docks them on the place's marker.
        let scout = Actor::from(MemberId::agent("scout"));
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
            Some(&MemberId::agent("scout")),
            "the window docks the agent the lens names: {at_the_place:?}"
        );
        // The person's own view is not someone else at the place.
        m.rt.set_lenses(
            &at.id_string(),
            &[(Actor::from(MemberId::agent("scout")), true, false)],
        );
        let own = presence(&m);
        assert!(
            matches!(own, Value::Opt(None) | Value::Missing),
            "a person's own view docks nobody on the place: {own:?}"
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
