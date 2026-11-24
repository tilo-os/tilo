//! The weather app (`apps/tl/weather/app.tl`) end to end: the places a person follows, the
//! service's answers read in the app over `web.get`, an update that reads a few places a second, a
//! place that cannot be read, the units Settings chose, and the window's list, readings and
//! search.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use serde_json::{Value as Json, json};
    use tilolang_runtime::run::WebRequest;
    use tilolang_runtime::web::{WebReply, WebTransport};
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/weather/app.tl");

    /// A forecast body for the day the window shows, with `hours` hourly points, the current
    /// reading at hour `current`, and `temperature` degrees now.
    fn forecast_body(hours: usize, current: usize, temperature: f64) -> String {
        let times: Vec<String> = (0..hours)
            .map(|h| format!("2026-11-24T{h:02}:00"))
            .collect();
        let temps: Vec<f64> = (0..hours).map(|h| (h as f64) + 2.0).collect();
        json!({
            "current": {
                "time": format!("2026-11-24T{current:02}:00"),
                "temperature_2m": temperature,
                "apparent_temperature": 18.0,
                "relative_humidity_2m": 58.0,
                "precipitation": 2.4,
                "weather_code": 3,
                "wind_speed_10m": 12.0,
                "wind_direction_10m": 210.0,
                "pressure_msl": 1012.0,
                "is_day": 1
            },
            "hourly": {
                "time": times,
                "temperature_2m": temps,
                "precipitation_probability": vec![Some(10.0); hours],
                "weather_code": vec![61; hours],
                "is_day": vec![1; hours]
            },
            "daily": {
                "time": ["2026-11-24", "2026-11-25"],
                "weather_code": [3, 61],
                "temperature_2m_max": [24.0, 19.0],
                "temperature_2m_min": [13.0, 11.0],
                "precipitation_probability_max": [40.0, null],
                "sunrise": ["2026-11-24T05:01", "2026-11-25T05:02"],
                "sunset": ["2026-11-24T21:34", "2026-11-25T21:33"],
                "wind_speed_10m_max": [20.0, 30.0],
                "uv_index_max": [5.0, null]
            }
        })
        .to_string()
    }

    /// The days of one forecast: what names each, and the weekday each falls on.
    fn days_body(dates: &[&str]) -> String {
        let count = dates.len();
        json!({
            "current": {
                "time": "2026-06-30T02:00",
                "temperature_2m": 21.4,
                "apparent_temperature": 20.1,
                "relative_humidity_2m": 58.0,
                "precipitation": 0.0,
                "weather_code": 3,
                "wind_speed_10m": 12.0,
                "wind_direction_10m": 210.0,
                "pressure_msl": 1012.0,
                "is_day": 1
            },
            "hourly": {
                "time": ["2026-06-30T02:00"],
                "temperature_2m": [21.4],
                "precipitation_probability": [10.0],
                "weather_code": [3],
                "is_day": [1]
            },
            "daily": {
                "time": dates,
                "weather_code": vec![3; count],
                "temperature_2m_max": vec![24.0; count],
                "temperature_2m_min": vec![13.0; count],
                "precipitation_probability_max": vec![40.0; count],
                "sunrise": vec!["2026-06-30T05:01"; count],
                "sunset": vec!["2026-06-30T21:34"; count],
                "wind_speed_10m_max": vec![20.0; count],
                "uv_index_max": vec![5.0; count]
            }
        })
        .to_string()
    }

    /// An air-quality reading whose ozone the service omitted.
    const AIR: &str = r#"{"current":{
        "time":"2026-06-30T02:00",
        "european_aqi":32.0,
        "us_aqi":41.0,
        "pm2_5":8.4,
        "pm10":14.2,
        "nitrogen_dioxide":18.0,
        "ozone":null
    }}"#;

    /// What the place search answers for Amsterdam: two of that name, one matched by another
    /// name, a repeat and a place that merely contains it.
    const GEOCODE: &str = r#"{"results":[
        {"name":"Amsterdam","latitude":52.374,"longitude":4.8897,"country":"Netherlands","country_code":"NL","admin1":"North Holland","timezone":"Europe/Amsterdam"},
        {"name":"Amsterdam","latitude":40.5,"longitude":-80.6,"country":"United States","country_code":"US","admin1":"Ohio","timezone":"America/New_York"},
        {"name":"eMvelo","latitude":-26.5,"longitude":30.0,"country":"South Africa","country_code":"ZA","admin1":"Mpumalanga","timezone":"Africa/Johannesburg"},
        {"name":"Amsterdam","latitude":40.5,"longitude":-80.6,"country":"United States","country_code":"US","admin1":"Ohio","timezone":"America/New_York"},
        {"name":"New Amsterdam","latitude":38.1,"longitude":-85.8,"country":"United States","country_code":"US","admin1":"Indiana","timezone":"America/Kentucky/Louisville"}
    ]}"#;

    /// The device's network, as the weather services answer it: what was asked, and the answer
    /// each service gives.
    #[derive(Clone)]
    struct Services {
        asked: Rc<RefCell<Vec<WebRequest>>>,
        /// The forecast's body and status.
        forecast: Rc<RefCell<(u16, String)>>,
        /// The air quality's body, and whether it answers at all.
        air: Rc<RefCell<(u16, String)>>,
        /// The place search's body.
        geocode: Rc<RefCell<String>>,
    }

    impl Default for Services {
        fn default() -> Self {
            Self {
                asked: Rc::new(RefCell::new(Vec::new())),
                forecast: Rc::new(RefCell::new((200, forecast_body(30, 2, 20.0)))),
                air: Rc::new(RefCell::new((200, AIR.to_owned()))),
                geocode: Rc::new(RefCell::new(GEOCODE.to_owned())),
            }
        }
    }

    impl Services {
        fn urls(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|request| request.url.clone())
                .collect()
        }

        fn serve_forecast(&self, status: u16, body: &str) {
            *self.forecast.borrow_mut() = (status, body.to_owned());
        }
    }

    impl WebTransport for Services {
        fn fetch(&self, request: &WebRequest) -> WebReply {
            self.asked.borrow_mut().push(request.clone());
            let (status, body) = if request.url.contains("air-quality-api") {
                self.air.borrow().clone()
            } else if request.url.contains("geocoding-api") {
                (200, self.geocode.borrow().clone())
            } else {
                self.forecast.borrow().clone()
            };
            WebReply {
                status,
                status_text: if status == 200 { "OK" } else { "Forbidden" }.to_owned(),
                headers: Vec::new(),
                body: body.into_bytes(),
            }
        }
    }

    struct Weather {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        services: Services,
    }

    impl Weather {
        fn new(imperial: bool) -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let services = Services::default();
            rt.set_web(services.clone());
            rt.set_clock(|| "2026-11-24T09:30:00Z".to_owned());
            // Settings is the platform's own app: its region and formats are read for the units
            // every reading is drawn in.
            let settings = tilolang_tilo::platform_apps::platform_packages()
                .expect("the platform packages read")
                .into_iter()
                .find(|package| package.app == "so.tilo.settings.sections")
                .expect("settings is a platform package");
            rt.load_foreign(settings.clone()).expect("settings loads");
            let app = App::compile(SOURCE, &[settings]);
            authority.trust(&app.load(&rt));
            let root = rt.create_root("Locations").unwrap();
            let locale = tilolang_runtime::ResourceRef::parse_id(
                "settings/locale",
                "so.tilo.settings.sections#Locale",
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
                services,
            }
        }

        fn call(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let id = self.app.action(action);
            let args = self.app.args(&self.rt, action, given);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn step(&self, name: &str) {
            self.call(name, &[("w", Value::resource(&self.root))]);
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

        /// Reads every place, as the window does once a second, until the update has read them
        /// all.
        fn refresh(&self) {
            self.step("refresh");
            for _ in 0..self.listed(&self.root, "places") {
                self.step("poll_weather");
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

        /// One field of each record a collection field holds.
        fn items_of(&self, at: &ResourceRef, field: &str, name: &str) -> Vec<Json> {
            let items = match self.rt.read(at, field).unwrap() {
                Value::List(items) => items,
                other => panic!("{field} is {other:?}"),
            };
            items
                .into_iter()
                .map(|item| {
                    let Value::Ref(held) = item else {
                        panic!("an item of {field} is no record")
                    };
                    self.rt
                        .read(&held.target, name)
                        .unwrap_or_else(|error| panic!("{name} of an item of {field}: {error:?}"))
                        .project_owned()
                })
                .collect()
        }

        /// The window's Add place, pressed, and the field it opens.
        fn open_search(&self) -> (ViewInstance, String) {
            let mut window = self.weather_window();
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
            (window, field)
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

    /// A value a record holds, read as JSON: what an item's own field says.
    trait Projected {
        fn project_owned(self) -> Json;
    }

    impl Projected for Value {
        fn project_owned(self) -> Json {
            match self {
                Value::Text(text) => json!(text),
                Value::Int(n) => json!(n),
                Value::Float(n) => json!(n),
                Value::Bool(flag) => json!(flag),
                Value::Opt(None) => Json::Null,
                Value::Opt(Some(held)) => held.project_owned(),
                other => panic!("{other:?} is no plain value"),
            }
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

    tilo_test_env::scenario! {
        fn the_weather_app_reads_its_services_over_the_web_and_shows_what_they_said;

        phase a_place_is_saved_once_and_a_nearby_one_is_that_place;
        {
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

        phase a_refresh_reads_the_forecast_and_the_air_and_the_window_draws_it_in_the_persons_units;
        {
            let weather = Weather::new(true);
            let place = weather.save("Amsterdam", 52.374, 4.8897);
            weather.step("refresh");
            assert_eq!(
                weather.field(&place, "read"),
                json!(true),
                "the first place is read in the same call: trouble {:?} note {:?}",
                weather.field(&place, "trouble"),
                weather.inner(&weather.root, "refresh", "note")
            );
            assert_eq!(
                weather.services.urls().len(),
                2,
                "one place's forecast and its air quality: {:?}",
                weather.services.urls()
            );
            assert_eq!(
                weather.inner(&place, "current", "temperature"),
                json!(20.0)
            );
            assert_eq!(weather.inner(&place, "current", "code"), json!(3));
            assert_eq!(weather.inner(&place, "current", "icon"), json!("cloud_sun"));
            assert_eq!(weather.listed(&place, "hours"), 24, "a day of hours from now");
            assert_eq!(weather.listed(&place, "days"), 2);
            assert_eq!(weather.inner(&place, "air", "european"), json!(32.0));
            assert_eq!(weather.field(&place, "has_air"), json!(true));
            assert_eq!(weather.field(&place, "stale"), json!(false));
            // The window's second asks the day where the person is, for the names of its days.
            weather.step("poll_weather");
            assert_eq!(weather.field(&weather.root, "today"), json!("2026-11-24"));
            assert_eq!(
                weather.inner(&weather.root, "refresh", "state"),
                json!("done")
            );

            // The window draws the reading in Fahrenheit, because that is what the person chose
            // in Settings, and every hour and day with it.
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

        phase a_forecast_reads_from_the_current_hour_with_the_days_beside_it;
        {
            let weather = Weather::new(false);
            weather
                .services
                .serve_forecast(200, &forecast_body(30, 2, 21.4));
            let place = weather.save("Berlin", 52.52, 13.405);
            weather.refresh();
            assert_eq!(weather.inner(&place, "current", "at"), json!("2026-11-24T02:00"));
            assert_eq!(weather.inner(&place, "current", "temperature"), json!(21.4));
            assert_eq!(weather.inner(&place, "current", "apparent"), json!(18.0));
            assert_eq!(
                weather.inner(&place, "current", "uv"),
                json!(5.0),
                "the current uv is today's highest"
            );
            // The hours start at the current one, and a day of them follows.
            let at: Vec<Json> = weather.items_of(&place, "hours", "at");
            let temperature: Vec<Json> = weather.items_of(&place, "hours", "temperature");
            let code: Vec<Json> = weather.items_of(&place, "hours", "code");
            assert_eq!(at.len(), 24, "a day of hours from now");
            assert_eq!(at[0], json!("2026-11-24T02:00"));
            assert_eq!(temperature[0], json!(4.0));
            assert_eq!(code[0], json!(61));
            let dates: Vec<Json> = weather.items_of(&place, "days", "date");
            let highs: Vec<Json> = weather.items_of(&place, "days", "high");
            let chance: Vec<Json> = weather.items_of(&place, "days", "chance");
            let uv: Vec<Json> = weather.items_of(&place, "days", "uv");
            let set: Vec<Json> = weather.items_of(&place, "days", "set");
            assert_eq!(dates.len(), 2);
            assert_eq!(highs, [json!(24.0), json!(19.0)]);
            assert_eq!(chance[1], json!(0.0), "a missing chance reads as none");
            assert_eq!(uv[1], json!(0.0));
            assert_eq!(set[0], json!("2026-11-24T21:34"));
            assert_eq!(
                weather.items_of(&place, "days", "weekday"),
                [json!(1), json!(2)],
                "2026-11-24 is a Tuesday, and the day after a Wednesday"
            );
        }

        phase a_date_names_the_weekday_it_falls_on;
        {
            let weather = Weather::new(false);
            weather.services.serve_forecast(
                200,
                &days_body(&["2026-06-29", "2028-02-29", "2026-11-28", "2026-11-29", "no date"]),
            );
            let place = weather.save("Berlin", 52.52, 13.405);
            weather.refresh();
            assert_eq!(
                weather.items_of(&place, "days", "weekday"),
                [
                    json!(0),
                    json!(1),
                    json!(5),
                    json!(6),
                    json!(0)
                ],
                "a Monday, a leap day, a Saturday, a Sunday, and text that is no date"
            );
        }

        phase a_body_that_is_no_forecast_says_so_rather_than_inventing_one;
        {
            let weather = Weather::new(false);
            weather.services.serve_forecast(200, r#"{"current": 3}"#);
            let place = weather.save("Berlin", 52.52, 13.405);
            weather.refresh();
            assert_eq!(weather.field(&place, "read"), json!(false));
            assert_eq!(
                weather.field(&place, "trouble"),
                json!("The answer wasn't a forecast")
            );
            assert_eq!(weather.listed(&place, "hours"), 0);
            let window = weather.weather_window();
            assert!(
                of_kind(window.tree(), "ui.Banner")
                    .iter()
                    .any(|banner| text(banner, "message") == "The answer wasn't a forecast")
            );
        }

        phase an_air_reading_keeps_what_the_service_omitted_absent;
        {
            let weather = Weather::new(false);
            let place = weather.save("Berlin", 52.52, 13.405);
            weather.refresh();
            assert_eq!(weather.inner(&place, "air", "european"), json!(32.0));
            assert_eq!(weather.inner(&place, "air", "pm2_5"), json!(8.4));
            assert_eq!(
                weather.inner(&place, "air", "ozone"),
                Json::Null,
                "a null pollutant stays honestly absent"
            );
            let window = weather.weather_window();
            let ozone: Vec<String> = of_kind(window.tree(), "ui.StatTile")
                .into_iter()
                .filter(|tile| text(tile, "label") == "Ozone")
                .map(|tile| text(tile, "value"))
                .collect();
            assert_eq!(ozone, ["—"]);
        }

        phase a_place_search_drops_other_names_and_repeats;
        {
            let weather = Weather::new(false);
            let (mut window, field) = weather.open_search();
            window.edit(&field, &Value::text(" amsterdam")).unwrap();
            window.poll_patches().unwrap();
            weather.step("poll_weather");
            assert_eq!(
                weather.services.urls().len(),
                1,
                "one request for the query: {:?}",
                weather.services.urls()
            );
            window.poll_patches().unwrap();
            let rows: Vec<String> = of_kind(window.tree(), "ui.Suggestion")
                .into_iter()
                .map(|row| text(row, "label"))
                .collect();
            assert_eq!(
                rows,
                [
                    "Amsterdam, North Holland, Netherlands",
                    "Amsterdam, Ohio, United States",
                    "New Amsterdam, Indiana, United States"
                ],
                "a repeat shows once, names the query matches stay, and another name goes: {}",
                dump(window.tree())
            );
        }

        phase the_addresses_ask_metric_and_carry_the_place;
        {
            let weather = Weather::new(false);
            weather.save("Berlin", 52.52, 13.405);
            weather.refresh();
            let urls = weather.services.urls();
            let forecast = &urls[0];
            assert!(forecast.contains("latitude=52.52"), "{forecast}");
            assert!(forecast.contains("longitude=13.405"), "{forecast}");
            assert!(forecast.contains("current=temperature_2m"), "{forecast}");
            assert!(forecast.contains("timezone=auto"), "{forecast}");
            assert!(forecast.contains("temperature_unit=celsius"), "{forecast}");
            assert!(forecast.contains("wind_speed_unit=kmh"), "{forecast}");
            assert!(urls[1].contains("european_aqi"), "{}", urls[1]);

            // A place name is escaped for the query string, whatever its letters.
            let (mut window, field) = weather.open_search();
            window.edit(&field, &Value::text("New York")).unwrap();
            window.poll_patches().unwrap();
            weather.step("poll_weather");
            let asked = weather.services.urls().last().cloned().unwrap_or_default();
            assert!(asked.contains("name=New%20York"), "{asked}");
            window.edit(&field, &Value::text("München")).unwrap();
            window.poll_patches().unwrap();
            weather.step("poll_weather");
            let asked = weather.services.urls().last().cloned().unwrap_or_default();
            assert!(asked.contains("name=M%C3%BCnchen"), "{asked}");
        }

        phase an_update_reads_a_few_places_a_second_and_can_be_stopped;
        {
            let weather = Weather::new(false);
            for (name, latitude, longitude) in [
                ("Amsterdam", 52.374, 4.8897),
                ("Berlin", 52.52, 13.405),
                ("Reykjavík", 64.1466, -21.9426),
                ("Oslo", 59.9139, 10.7522),
                ("Paris", 48.8566, 2.3522),
                ("Lisbon", 38.7223, -9.1393),
            ] {
                weather.save(name, latitude, longitude);
            }
            weather.step("refresh");
            assert_eq!(
                weather.inner(&weather.root, "refresh", "done"),
                json!(1),
                "the first place is read at once"
            );
            assert_eq!(
                weather.inner(&weather.root, "refresh", "state"),
                json!("running")
            );
            weather.step("poll_weather");
            assert_eq!(
                weather.inner(&weather.root, "refresh", "done"),
                json!(5),
                "four more places are read on the next second"
            );
            // Stopping leaves the place that had not been read as it was.
            weather.step("stop_refresh");
            assert_eq!(
                weather.inner(&weather.root, "refresh", "state"),
                json!("done")
            );
            assert_eq!(
                weather.services.urls().len(),
                10,
                "five places read, two services each, and the sixth left alone"
            );
        }

        phase a_place_that_cannot_be_read_says_why_and_keeps_what_it_had;
        {
            let weather = Weather::new(false);
            let place = weather.save("Reykjavík", 64.1466, -21.9426);
            weather.refresh();
            let first = weather.rt.read(&place, "current").unwrap();
            assert_eq!(weather.inner(&place, "current", "temperature"), json!(20.0));

            // The service refuses the next read: the place says why and keeps the reading it has.
            weather.services.serve_forecast(403, "forbidden");
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

        phase a_reading_older_than_two_hours_is_marked_out_of_date;
        {
            let weather = Weather::new(false);
            let place = weather.save("Amsterdam", 52.374, 4.8897);
            weather.refresh();
            assert_eq!(weather.field(&place, "stale"), json!(false));
            // The clock moves past the service's hour: the next poll says the reading is not
            // current.
            weather.rt.set_clock(|| "2026-11-24T12:00:00Z".to_owned());
            weather.step("poll_weather");
            assert_eq!(weather.field(&place, "stale"), json!(true));
            let window = weather.weather_window();
            assert!(
                drawn(&window).contains(&"Out of date".to_owned()),
                "{:?}",
                drawn(&window)
            );
        }

        phase a_search_finds_places_and_a_pick_saves_one;
        {
            let weather = Weather::new(false);
            // The window's Add place opens the search, and the field is typed into as a person
            // does.
            let (mut window, field) = weather.open_search();
            window.edit(&field, &Value::text("Amsterdam")).unwrap();
            window.poll_patches().unwrap();
            // The window's poll asks the service once for what was typed and takes the rows.
            weather.step("poll_weather");
            assert_eq!(weather.services.urls().len(), 1);
            window.poll_patches().unwrap();
            let rows: Vec<String> = of_kind(window.tree(), "ui.Suggestion")
                .into_iter()
                .map(|row| text(row, "label"))
                .collect();
            assert_eq!(
                rows,
                [
                    "Amsterdam, North Holland, Netherlands",
                    "Amsterdam, Ohio, United States",
                    "New Amsterdam, Indiana, United States"
                ],
                "{}",
                dump(window.tree())
            );

            // Taking the second row saves that Amsterdam, not the first.
            window
                .fire_with(&field, "accept", Some(&Value::text("1")))
                .unwrap();
            let place = weather.place("Amsterdam");
            assert_eq!(weather.field(&place, "region"), json!("Ohio"));
            assert_eq!(weather.field(&place, "longitude"), json!(-80.6));
        }
    }
}
