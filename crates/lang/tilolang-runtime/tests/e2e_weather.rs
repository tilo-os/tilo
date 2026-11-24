//! The weather app (`apps/tl/weather/app.tl`) end to end: the places a person follows, a refresh
//! whose answers land place by place, a place that cannot be read, the units Settings chose, and
//! the window's list, readings and search.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/weather/app.tl");

    /// What the device answered, and what it was asked: the engine's side of an update and a
    /// search, played over the app's own JSON, so both ends of the contract are read here.
    #[derive(Clone, Default)]
    struct Device {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        /// The places of the update, as the app asked for them.
        places: Rc<RefCell<Vec<Json>>>,
        /// The places whose answer the job still holds.
        ready: Rc<RefCell<Vec<String>>>,
        /// Whether the places' answers are refusals.
        failing: Rc<RefCell<bool>>,
        /// What the search answered.
        found: Rc<RefCell<Vec<Json>>>,
    }

    impl Device {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }

        fn inputs(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(_, input)| input.clone())
                .collect()
        }
    }

    impl EngineHost for Device {
        fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            assert_eq!(app, "so.tilo.weather", "the engine answers its own app");
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                "take" => {
                    let ready = std::mem::take(&mut *self.ready.borrow_mut());
                    let failing = *self.failing.borrow();
                    let answers: Vec<String> = ready.iter().map(|id| answer(id, failing)).collect();
                    let places: Vec<String> = if ready.is_empty() {
                        self.found
                            .borrow()
                            .iter()
                            .map(ToString::to_string)
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Ok(json!({ "answers": answers, "places": places, "error": "" }).to_string())
                }
                other => Err(format!("`{other}` is no op this test's engine runs")),
            }
        }

        fn start(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            assert_eq!(app, "so.tilo.weather");
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            let ask: Json = serde_json::from_str(input).expect("the ask reads as JSON");
            match op {
                "refresh" => {
                    let places = ask["places"].as_array().cloned().unwrap_or_default();
                    self.ready.borrow_mut().extend(
                        places
                            .iter()
                            .filter_map(|place| place["place_id"].as_str().map(str::to_owned)),
                    );
                    *self.places.borrow_mut() = places;
                    Ok("weather-refresh-1".to_owned())
                }
                "search" => Ok("weather-search-1".to_owned()),
                other => Err(format!("`{other}` is no op this test's engine runs")),
            }
        }

        fn status(&self, job: &str) -> Result<String, String> {
            assert!(
                job == "weather-refresh-1" || job == "weather-search-1",
                "no job {job}"
            );
            let total = if job == "weather-search-1" {
                1
            } else {
                self.places.borrow().len()
            };
            let ready = self.ready.borrow().len();
            Ok(json!({
                "state": if job == "weather-search-1" || ready == 0 { "done" } else { "running" },
                "done": if job == "weather-search-1" { 1 } else { total - ready },
                "total": total,
            })
            .to_string())
        }

        fn cancel(&self, job: &str) -> bool {
            job == "weather-refresh-1" || job == "weather-search-1"
        }
    }

    /// One place's answer as the engine words it: its reading, a day of hours, a week of days and
    /// its air, or the refusal of a service that will not answer.
    fn answer(id: &str, failing: bool) -> String {
        if failing {
            return json!({
                "place_id": id, "ok": false, "reason": "refused", "status": 403,
                "message": "HTTP 403", "fetched": 0,
                "hour_at": [], "hour_temp": [], "hour_chance": [], "hour_code": [],
                "hour_day": [], "day_date": [], "day_weekday": [], "day_high": [],
                "day_low": [], "day_chance": [], "day_code": [], "day_wind": [],
                "day_uv": [], "day_rise": [], "day_set": [], "air_ok": false,
            })
            .to_string();
        }
        let hours: Vec<String> = (9..12).map(|h| format!("2026-11-24T{h:02}:00")).collect();
        json!({
            "place_id": id,
            "ok": true,
            "reason": "",
            "status": 0,
            "message": "",
            // Two hours before the test's clock (2026-11-24T09:30Z): still current.
            "fetched": 1_795_505_401,
            "time": "2026-11-24T09:00",
            "temperature": 20.0,
            "apparent": 18.0,
            "humidity": 58.0,
            "precipitation": 2.4,
            "code": 3,
            "wind": 12.0,
            "wind_from": 210.0,
            "pressure": 1012.0,
            "uv": 5.0,
            "is_day": true,
            "hour_at": hours,
            "hour_temp": [20.0, 21.0, 22.0],
            "hour_chance": [10.0, 20.0, 30.0],
            "hour_code": [3, 61, 0],
            "hour_day": [true, true, true],
            "day_date": ["2026-11-24", "2026-11-25"],
            "day_weekday": [1, 2],
            "day_high": [24.0, 19.0],
            "day_low": [13.0, 11.0],
            "day_chance": [40.0, 0.0],
            "day_code": [3, 61],
            "day_wind": [20.0, 30.0],
            "day_uv": [5.0, 0.0],
            "day_rise": ["2026-11-24T08:09", "2026-11-25T08:11"],
            "day_set": ["2026-11-24T16:38", "2026-11-25T16:36"],
            "air_ok": true,
            "air_at": "2026-11-24T09:00",
            "air_european": 32.0,
            "air_us": 41.0,
            "air_pm2_5": 8.4,
            "air_pm10": 14.2,
            "air_no2": 18.0,
            "air_ozone": null,
        })
        .to_string()
    }

    struct Weather {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Device,
    }

    impl Weather {
        fn new(imperial: bool) -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device::default();
            rt.set_engine(device.clone());
            rt.set_clock(|| "2026-11-24T09:30:00Z".to_owned());
            // Settings is the platform's own app: its region and formats are read for the units
            // every reading is drawn in.
            let settings = tilolang_tilo::platform_apps::platform_packages()
                .expect("the platform packages read")
                .into_iter()
                .find(|package| package.app == "so.tilo.settings")
                .expect("settings is a platform package");
            rt.load_foreign(settings.clone()).expect("settings loads");
            let app = App::compile(SOURCE, &[settings]);
            authority.trust(&app.load(&rt));
            let root = rt.create_root("Locations").unwrap();
            let locale = tilolang_runtime::ResourceRef::parse_id(
                "settings/locale",
                "so.tilo.settings#Locale",
            )
            .expect("the locale's address");
            rt.set_reading(
                &locale,
                if imperial {
                    r#"{"measurement": "Imperial"}"#
                } else {
                    r#"{"measurement": "Metric"}"#
                },
            )
            .expect("the region and formats read");
            Self {
                rt,
                app,
                root,
                device,
            }
        }

        fn call(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let id = self.app.action(action);
            let args = self.app.args(&self.rt, action, given);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn save(&self, name: &str, latitude: f64, longitude: f64) -> ResourceRef {
            returned_ref(self.call(
                "save_place",
                &[
                    ("w", Value::resource(&self.root)),
                    ("name", Value::text(name)),
                    ("latitude", Value::Float(latitude)),
                    ("longitude", Value::Float(longitude)),
                ],
            ))
        }

        /// Reads every place and takes what came back, as the window does each second.
        fn refresh(&self) {
            let step = |name: &str| {
                let id = self.app.action(name);
                let args = self
                    .app
                    .args(&self.rt, name, &[("w", Value::resource(&self.root))]);
                self.rt
                    .invoke_action(&me(), (id.as_str(), args))
                    .expect(name)
            };
            step("refresh");
            for _ in 0..3 {
                step("poll_weather");
            }
        }

        fn field(&self, at: &ResourceRef, field: &str) -> Json {
            self.rt.project(&Value::resource(at))["fields"][field].clone()
        }

        /// How many items a collection field holds, as the store projects it.
        fn listed(&self, at: &ResourceRef, field: &str) -> usize {
            let projected = self.field(at, field);
            match projected["count"].as_u64() {
                Some(count) => usize::try_from(count).expect("a count fits"),
                None => panic!("{field} is {projected}"),
            }
        }

        /// What a record field holds, as the store projects it: the nested record it names.
        fn inner(&self, at: &ResourceRef, field: &str, name: &str) -> Json {
            match self.rt.read(at, field).unwrap() {
                Value::Ref(held) => {
                    self.rt.project(&Value::resource(&held.target))["fields"][name].clone()
                }
                other => panic!("{field} is {other:?}"),
            }
        }

        fn window(&self, view: &str, args: Vec<Value>) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn weather_window(&self) -> ViewInstance {
            self.window("weather_window", vec![Value::resource(&self.root)])
        }

        /// The place the sidebar lists under `name`.
        fn place(&self, name: &str) -> ResourceRef {
            let window = self.weather_window();
            of_kind(window.tree(), "ui.NavItem")
                .into_iter()
                .filter_map(|item| {
                    item.props.get("item").and_then(|item| match item {
                        Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                        _ => None,
                    })
                })
                .find(|place| self.field(place, "name") == json!(name))
                .unwrap_or_else(|| panic!("no place called {name} in the sidebar"))
        }
    }

    /// The text of a node's property, whatever option carries it.
    fn text(node: &tilolang_runtime::UiNode, name: &str) -> String {
        match node.props.get(name) {
            Some(Value::Text(text)) => text.clone(),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Text(text) => text.clone(),
                other => panic!("{name} of {} is {other:?}", node.kind),
            },
            other => panic!("{name} of {} is {other:?}", node.kind),
        }
    }

    /// Every text the tree shows, in document order.
    fn drawn(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.Text")
            .into_iter()
            .filter_map(|node| match node.props.get("value") {
                Some(Value::Text(text)) => Some(text.clone()),
                Some(Value::Opt(Some(held))) => match &**held {
                    Value::Text(text) => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_place_is_saved_once_and_a_nearby_one_is_that_place() {
        let weather = Weather::new(false);
        let amsterdam = weather.save("Amsterdam", 52.374, 4.8897);
        assert_eq!(weather.field(&amsterdam, "name"), json!("Amsterdam"));
        assert_eq!(
            weather.field(&weather.root, "default")["id"],
            json!(amsterdam.id_string()),
            "a new place is the one windows show first"
        );
        // A place within a twentieth of a degree is the place already saved.
        let again = weather.save("Amsterdam CS", 52.3741, 4.8897);
        assert_eq!(again.id_string(), amsterdam.id_string());

        let berlin = weather.save("Berlin", 52.52, 13.405);
        assert_eq!(
            weather.field(&weather.root, "default")["id"],
            json!(berlin.id_string()),
            "the place saved last is the one shown first"
        );
        weather.call(
            "use_as_default",
            &[
                ("w", Value::resource(&weather.root)),
                ("place", Value::resource(&amsterdam)),
            ],
        );
        weather.call(
            "move_to_top",
            &[
                ("w", Value::resource(&weather.root)),
                ("place", Value::resource(&berlin)),
            ],
        );
        let window = weather.weather_window();
        let listed: Vec<String> = of_kind(window.tree(), "ui.NavItem")
            .into_iter()
            .map(|item| text(item, "label"))
            .collect();
        assert_eq!(listed, ["Berlin", "Amsterdam"], "{:?}", dump(window.tree()));

        weather.call(
            "remove_place",
            &[
                ("w", Value::resource(&weather.root)),
                ("place", Value::resource(&berlin)),
            ],
        );
        assert_eq!(
            weather.field(&weather.root, "default")["id"],
            json!(amsterdam.id_string()),
            "removing another place leaves the one shown first"
        );
    }

    #[test]
    fn a_refresh_lands_each_place_and_the_readings_draw_in_the_persons_units() {
        let weather = Weather::new(true);
        let place = weather.save("Amsterdam", 52.374, 4.8897);
        weather.call("refresh", &[("w", Value::resource(&weather.root))]);
        weather.call("poll_weather", &[("w", Value::resource(&weather.root))]);
        assert_eq!(
            weather.field(&place, "read"),
            json!(true),
            "the first take lands the place: trouble {:?} note {:?} failed {:?}",
            weather.field(&place, "trouble"),
            weather.inner(&weather.root, "refresh", "note"),
            weather.inner(&weather.root, "refresh", "failed")
        );
        weather.call("poll_weather", &[("w", Value::resource(&weather.root))]);
        let note = weather.inner(&weather.root, "refresh", "note");
        assert_eq!(note, json!(""), "the update says why nothing read: {note}");
        assert_eq!(weather.device.ops(), ["refresh", "take", "take"]);
        let asked: Json = serde_json::from_str(&weather.device.inputs()[0]).unwrap();
        assert_eq!(asked["places"][0]["latitude"], json!(52.374));
        assert_eq!(
            asked["places"][0]["place_id"],
            json!(place.id_string()),
            "the answer names the place it is about"
        );

        assert_eq!(weather.inner(&place, "current", "temperature"), json!(20.0));
        assert_eq!(weather.inner(&place, "current", "code"), json!(3));
        assert_eq!(weather.listed(&place, "hours"), 3);
        assert_eq!(weather.listed(&place, "days"), 2);
        assert_eq!(weather.inner(&place, "air", "european"), json!(32.0));
        assert_eq!(weather.field(&place, "stale"), json!(false));
        assert_eq!(weather.field(&place, "read"), json!(true));
        assert_eq!(weather.field(&place, "has_air"), json!(true));
        assert_eq!(weather.field(&weather.root, "today"), json!("2026-11-24"));

        // The window draws the reading in Fahrenheit, because that is what the person chose in
        // Settings, and every hour and day with it.
        let window = weather.weather_window();
        let texts = drawn(&window);
        assert!(texts.contains(&"68°".to_owned()), "{texts:?}");
        assert!(texts.contains(&"Overcast".to_owned()), "{texts:?}");
        assert!(texts.contains(&"Tomorrow".to_owned()), "{texts:?}");
        assert!(texts.contains(&"Now".to_owned()), "{texts:?}");
        let sections: Vec<String> = of_kind(window.tree(), "ui.Section")
            .into_iter()
            .map(|section| text(section, "title"))
            .collect();
        assert_eq!(
            sections,
            ["Next 24 hours", "The week ahead", "Air quality"],
            "{:?}",
            dump(window.tree())
        );
        let tiles: Vec<String> = of_kind(window.tree(), "ui.StatTile")
            .into_iter()
            .map(|tile| text(tile, "label"))
            .collect();
        assert!(tiles.contains(&"Wind".to_owned()), "{tiles:?}");
        assert!(tiles.contains(&"Feels like".to_owned()), "{tiles:?}");
        assert!(tiles.contains(&"Air quality".to_owned()), "{tiles:?}");
    }

    #[test]
    fn a_place_that_cannot_be_read_says_why_and_keeps_what_it_had() {
        let weather = Weather::new(false);
        let place = weather.save("Reykjavík", 64.1466, -21.9426);
        weather.refresh();
        let first = weather.rt.read(&place, "current").unwrap();
        assert_eq!(weather.inner(&place, "current", "temperature"), json!(20.0));

        // The service refuses the next read: the place says why and keeps the reading it has.
        *weather.device.failing.borrow_mut() = true;
        weather.refresh();
        assert_eq!(
            weather.field(&place, "trouble"),
            json!("The weather service refused the request (HTTP 403)")
        );
        assert_eq!(weather.rt.read(&place, "current").unwrap(), first);
        let window = weather.weather_window();
        let banners: Vec<(String, String)> = of_kind(window.tree(), "ui.Banner")
            .into_iter()
            .map(|banner| (text(banner, "title"), text(banner, "message")))
            .collect();
        assert_eq!(
            banners,
            [
                (
                    "1 place could not be updated".to_owned(),
                    "What was read before still stands; try again later.".to_owned()
                ),
                (
                    "Couldn't update this forecast".to_owned(),
                    "The weather service refused the request (HTTP 403)".to_owned()
                )
            ],
            "the update and the place say why, over the reading that keeps standing"
        );
        // The forecast that stood before still stands.
        assert!(
            drawn(&window).contains(&"20°".to_owned()),
            "{:?}",
            drawn(&window)
        );
        let failed = weather.rt.read(&weather.root, "refresh").unwrap();
        assert_eq!(weather.rt.project(&failed)["fields"]["failed"], json!(1));
    }

    #[test]
    fn a_reading_older_than_two_hours_is_marked_out_of_date() {
        let weather = Weather::new(false);
        let place = weather.save("Amsterdam", 52.374, 4.8897);
        weather.refresh();
        assert_eq!(weather.field(&place, "stale"), json!(false));
        // The clock moves past the service's hour: the next poll says the reading is not current.
        weather.rt.set_clock(|| "2026-11-24T12:00:00Z".to_owned());
        weather.call("poll_weather", &[("w", Value::resource(&weather.root))]);
        assert_eq!(weather.field(&place, "stale"), json!(true));
        let window = weather.weather_window();
        assert!(
            drawn(&window).contains(&"Out of date".to_owned()),
            "{:?}",
            drawn(&window)
        );
    }

    #[test]
    fn a_search_finds_places_and_a_pick_saves_one() {
        let weather = Weather::new(false);
        *weather.device.found.borrow_mut() = vec![
            json!({"name": "Amsterdam", "latitude": 52.374, "longitude": 4.8897,
                   "country": "Netherlands", "country_code": "NL",
                   "region": "North Holland", "timezone": "Europe/Amsterdam"}),
            json!({"name": "Amsterdam", "latitude": 42.94, "longitude": -74.19,
                   "country": "United States", "country_code": "US",
                   "region": "New York", "timezone": "America/New_York"}),
        ];
        // The window's Add place opens the search, and the field is typed into as a person does.
        let mut window = weather.weather_window();
        let add = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .find(|button| text(button, "label") == "Add place")
            .map(|button| button.key.clone())
            .unwrap_or_else(|| panic!("no Add place in {}", dump(window.tree())));
        window.press(&add).unwrap();
        window.poll_patches().unwrap();
        let field = of_kind(window.tree(), "ui.Autocomplete")
            .into_iter()
            .next()
            .map(|field| field.key.clone())
            .unwrap_or_else(|| panic!("no search field in {}", dump(window.tree())));
        window.edit(&field, &Value::text("Amsterdam")).unwrap();
        window.poll_patches().unwrap();
        // The window's poll asks the device once for what was typed and takes the rows.
        weather.call("poll_weather", &[("w", Value::resource(&weather.root))]);
        assert_eq!(weather.device.ops(), ["search", "take"]);
        window.poll_patches().unwrap();
        let rows: Vec<String> = of_kind(window.tree(), "ui.Suggestion")
            .into_iter()
            .map(|row| text(row, "label"))
            .collect();
        assert_eq!(
            rows,
            [
                "Amsterdam, North Holland, Netherlands",
                "Amsterdam, New York, United States"
            ],
            "{}",
            dump(window.tree())
        );

        // Taking the second row saves that Amsterdam, not the first.
        window
            .fire_with(&field, "accept", Some(&Value::text("1")))
            .unwrap();
        let place = weather.place("Amsterdam");
        assert_eq!(weather.field(&place, "region"), json!("New York"));
        assert_eq!(weather.field(&place, "longitude"), json!(-74.19));
    }
}
