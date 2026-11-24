//! Speed Test (`apps/tl/speed-test/app.tl`) end to end: a run measuring latency, download and
//! upload over `web.get` and `web.post`, its stages followed through the app's own actions, the
//! measurements a workspace keeps and clears, and its window — the dial, the stages, the numbers,
//! the facts, the quality and the runs before.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use crate::common::*;
    use tilolang_runtime::run::WebRequest;
    use tilolang_runtime::web::{WebReply, WebTransport};
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/speed-test/app.tl");

    /// The device's network, as Cloudflare's test endpoint answers it: what was asked, how long
    /// each answer takes, and whether it answers at all.
    #[derive(Clone)]
    struct Services {
        asked: Rc<RefCell<Vec<WebRequest>>>,
        /// How long a ping's answer takes, a download chunk's, and the upload's.
        ping: Duration,
        chunk: Duration,
        upload: Duration,
        /// Whether the endpoint refuses every request.
        offline: Rc<RefCell<bool>>,
    }

    impl Default for Services {
        fn default() -> Self {
            Self {
                asked: Rc::new(RefCell::new(Vec::new())),
                ping: Duration::from_millis(10),
                chunk: Duration::from_millis(20),
                upload: Duration::from_millis(10),
                offline: Rc::new(RefCell::new(false)),
            }
        }
    }

    impl Services {
        fn asked(&self) -> Vec<WebRequest> {
            self.asked.borrow().clone()
        }
    }

    impl WebTransport for Services {
        fn fetch(&self, request: &WebRequest) -> WebReply {
            self.asked.borrow_mut().push(request.clone());
            if *self.offline.borrow() {
                return WebReply::failed("connection refused");
            }
            if request.url.ends_with("/__up") {
                std::thread::sleep(self.upload);
                return WebReply {
                    status: 200,
                    status_text: "OK".to_owned(),
                    headers: Vec::new(),
                    body: Vec::new(),
                };
            }
            let bytes = request
                .url
                .split("bytes=")
                .nth(1)
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            if bytes <= 1 {
                std::thread::sleep(self.ping);
                return WebReply {
                    status: 200,
                    status_text: "OK".to_owned(),
                    headers: vec!["cf-meta-colo: LHR".to_owned()],
                    body: b"x".to_vec(),
                };
            }
            std::thread::sleep(self.chunk);
            WebReply {
                status: 200,
                status_text: "OK".to_owned(),
                headers: Vec::new(),
                body: vec![b'x'; bytes],
            }
        }
    }

    struct Speed {
        rt: Runtime,
        app: App,
        services: Services,
        root: ResourceRef,
    }

    impl Speed {
        fn new() -> Self {
            Self::with(Services::default())
        }

        fn with(services: Services) -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            rt.set_web(services.clone());
            rt.set_clock(|| "2026-11-24T09:30:00Z".to_owned());
            // The device's own facts are the platform's: the host name a measurement keeps.
            let device = tilolang_tilo::platform_apps::platform_packages()
                .expect("the platform packages read")
                .into_iter()
                .find(|package| package.app == "so.tilo.device")
                .expect("the device is a platform package");
            rt.load_foreign(device.clone()).expect("the device loads");
            let app = App::compile(SOURCE, &[device]);
            authority.trust(&app.load(&rt));
            let root = rt.create_root("SpeedTest").unwrap();
            let host =
                tilolang_runtime::ResourceRef::parse_id("host-info", "so.tilo.device#HostInfo")
                    .expect("the host's address");
            rt.set_reading(&host, r#"{"hostname":"Laptop","arch":"x86_64"}"#)
                .expect("the host's facts read");
            Self {
                rt,
                app,
                services,
                root,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn start(&self) -> InvokeOutcome {
            self.call("start", vec![Value::resource(&self.root)])
        }

        fn stop(&self) -> InvokeOutcome {
            self.call("stop", vec![Value::resource(&self.root)])
        }

        /// Why the device refused to start another run.
        fn start_err(&self) -> String {
            self.rt
                .invoke_action(
                    &me(),
                    (
                        self.app.action("start").as_str(),
                        vec![Value::resource(&self.root)],
                    ),
                )
                .unwrap_err()
                .to_string()
        }

        /// Why the device refused to stop a run.
        fn stop_err(&self) -> String {
            self.rt
                .invoke_action(
                    &me(),
                    (
                        self.app.action("stop").as_str(),
                        vec![Value::resource(&self.root)],
                    ),
                )
                .unwrap_err()
                .to_string()
        }

        fn poll(&self) -> InvokeOutcome {
            self.call("poll", vec![Value::resource(&self.root)])
        }

        /// Polls until the run on the page has finished measuring.
        fn finish(&self) {
            for _ in 0..3 {
                self.poll();
            }
        }

        fn record(&self, ping: f64, jitter: f64, download: f64, upload: f64, server: &str) {
            self.call(
                "record",
                vec![
                    Value::resource(&self.root),
                    Value::Float(ping),
                    Value::Float(jitter),
                    Value::Float(download),
                    Value::Float(upload),
                    Value::text(server),
                    Value::text("Laptop"),
                ],
            );
        }

        /// The run the window shows, if any.
        fn run(&self) -> Value {
            match self.rt.read(&self.root, "runs").unwrap() {
                Value::List(items) => items.first().cloned().unwrap_or(Value::Opt(None)),
                other => panic!("runs reads {other:?}"),
            }
        }

        fn run_field(&self, name: &str) -> Value {
            let run = self.run();
            let Value::Ref(run) = &run else {
                panic!("a run stands on the page: {run:?}")
            };
            self.rt.read(&run.target, name).unwrap()
        }

        /// Every measurement kept, newest first.
        fn results(&self) -> Vec<(String, String, String)> {
            match self.rt.read(&self.root, "results").unwrap() {
                Value::List(items) => items
                    .iter()
                    .map(|item| {
                        let Value::Ref(r) = item else {
                            panic!("a measurement is a resource")
                        };
                        let at = &r.target;
                        (
                            said(&self.rt.read(at, "server").unwrap()),
                            said(&self.rt.read(at, "download").unwrap()),
                            said(&self.rt.read(at, "device").unwrap()),
                        )
                    })
                    .collect(),
                other => panic!("results reads {other:?}"),
            }
        }

        fn window(&self) -> ViewInstance {
            self.views("speed_test", &self.root)
        }

        fn views(&self, view: &str, on: &ResourceRef) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(view), vec![Value::resource(on)])
                .unwrap_or_else(|e| panic!("{view}: {e}"));
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn versions(&self) -> Vec<String> {
            self.rt
                .versions(&self.root)
                .unwrap()
                .into_iter()
                .map(|v| v.action)
                .collect()
        }
    }

    fn said(value: &Value) -> String {
        match value {
            Value::Text(t) => t.clone(),
            Value::Float(f) => f.to_string(),
            Value::Int(i) => i.to_string(),
            other => panic!("{other:?} is no text"),
        }
    }

    fn number(value: &Value) -> f64 {
        match value {
            Value::Float(f) => *f,
            Value::Int(i) => *i as f64,
            other => panic!("{other:?} is no number"),
        }
    }

    /// Every text of the window.
    fn texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .filter_map(|t| t.props.get("value").and_then(Value::as_text))
            .map(str::to_owned)
            .collect()
    }

    /// The buttons of the window, by the words on them.
    fn buttons(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Button")
            .iter()
            .filter_map(|b| b.props.get("label").and_then(Value::as_text))
            .map(str::to_owned)
            .collect()
    }

    /// The one dial of the window.
    fn dial(tree: &UiNode) -> &UiNode {
        let rings = of_kind(tree, "ui.ProgressRing");
        assert_eq!(rings.len(), 1, "one hero reading:\n{}", dump(tree));
        rings[0]
    }

    tilo_test_env::scenario! {
        fn speed_test_measures_over_the_web_and_shows_what_it_measured;

        phase a_run_measures_latency_download_and_upload_and_keeps_what_it_found;
        {
            let s = Speed::new();
            assert_eq!(s.start(), InvokeOutcome::Done(Value::Unit));
            // A run opens with its round trips, and the server the first answer names.
            let asked = s.services.asked();
            assert_eq!(asked.len(), 6, "six round trips: {:?}", asked.len());
            assert!(asked.iter().all(|request| request.url.ends_with("/__down?bytes=1")));
            assert_eq!(said(&s.run_field("phase")), "download");
            assert_eq!(said(&s.run_field("server")), "Cloudflare at LHR");
            let ping = number(&s.run_field("ping"));
            assert!((8.0..40.0).contains(&ping), "the round trip is measured: {ping}");

            s.poll();
            assert_eq!(said(&s.run_field("phase")), "upload", "the download is measured");
            let downloaded = number(&s.run_field("download"));
            assert!(
                (300.0..2500.0).contains(&downloaded),
                "ten megabytes over a tenth of a second: {downloaded} Mbps"
            );
            // The download came in chunks of a reply the language holds whole.
            let chunks: Vec<WebRequest> = s
                .services
                .asked()
                .into_iter()
                .filter(|request| request.url.contains("/__down?bytes=2000000"))
                .collect();
            assert_eq!(chunks.len(), 5, "five chunks of two megabytes");

            let outcome = s.poll();
            let asked: Vec<String> = s
                .services
                .asked()
                .into_iter()
                .map(|request| format!("{} bytes={:?}", request.url, request.bytes))
                .collect();
            assert_eq!(
                said(&s.run_field("phase")),
                "complete",
                "poll 2: {outcome:?}, asked {asked:?}, bytes {:?}, error {:?}",
                s.run_field("bytes"),
                s.run_field("error")
            );
            let upload = s
                .services
                .asked()
                .into_iter()
                .find(|request| request.url.ends_with("/__up"))
                .expect("the upload was sent");
            assert_eq!(upload.bytes, Some(2_000_000), "a body of two million zero bytes");
            assert_eq!(upload.body, None, "the body is no text");

            let kept = s.results();
            assert_eq!(kept.len(), 1, "the finished run is kept");
            assert_eq!(kept[0].0, "Cloudflare at LHR");
            assert_eq!(kept[0].2, "Laptop", "the device that ran it is kept");
            let download: f64 = kept[0].1.parse().expect("a number");
            assert!((300.0..2500.0).contains(&download), "{kept:?}");
            assert_eq!(number(&s.run_field("bytes")), 12_000_000.0, "ten down, two up");
        }

        phase a_measurement_is_recorded_newest_first_and_the_twentieth_lets_the_oldest_go;
        {
            let s = Speed::new();
            for index in 0..23 {
                s.record(10.0, 1.0, f64::from(index), 50.0, "Cloudflare");
            }
            let kept = s.results();
            assert_eq!(kept.len(), 20, "twenty runs are kept");
            assert!(kept[0].1.starts_with("22"), "newest first: {:?}", kept[0]);
            assert!(kept[19].1.starts_with("3"), "the oldest three went: {:?}", kept[19]);
        }

        phase stopping_ends_the_run_and_the_dial_goes_back_to_ready;
        {
            let s = Speed::new();
            s.start();
            assert_eq!(s.stop(), InvokeOutcome::Done(Value::Unit));
            assert!(matches!(s.run(), Value::Opt(None)), "no run stands on the page");
            let window = s.window();
            assert!(buttons(window.tree()).iter().any(|b| b == "Run speed test"));
            assert!(!buttons(window.tree()).iter().any(|b| b == "Stop the test"));
            assert!(texts(window.tree()).iter().any(|t| t == "Ready to test"));
            // A stopped run measures nothing more.
            let asked = s.services.asked().len();
            s.poll();
            assert_eq!(s.services.asked().len(), asked);
        }

        phase a_running_test_offers_stop_and_an_idle_one_run;
        {
            let s = Speed::new();
            let idle = s.window();
            assert!(buttons(idle.tree()).iter().any(|b| b == "Run speed test"));
            assert!(!buttons(idle.tree()).iter().any(|b| b == "Stop the test"));

            s.start();
            let refused = s.start_err();
            assert!(refused.contains("A test is running"), "{refused}");
            let busy = s.window();
            assert!(buttons(busy.tree()).iter().any(|b| b == "Stop the test"));
            assert!(!buttons(busy.tree()).iter().any(|b| b == "Run speed test"));

            s.stop();
            let refused = s.stop_err();
            assert!(refused.contains("No test is running"), "{refused}");
        }

        phase the_dial_reads_the_stage_it_is_in;
        {
            let s = Speed::new();
            s.start();
            let window = s.window();
            assert_eq!(text_prop(dial(window.tree()), "caption"), "Mbps");
            assert!(texts(window.tree()).iter().any(|t| t == "Measuring download"));
            assert_eq!(text_prop(dial(window.tree()), "readout"), "0.0");

            s.poll();
            let window = s.window();
            assert_eq!(text_prop(dial(window.tree()), "caption"), "Mbps");
            assert!(texts(window.tree()).iter().any(|t| t == "Measuring upload"));
            let filled = dial(window.tree())
                .props
                .get("value")
                .cloned()
                .expect("the dial has a value");
            assert!(
                matches!(filled, Value::Float(f) if f > 0.0),
                "the download fills the dial: {filled:?}"
            );
        }

        phase the_stages_follow_the_run;
        {
            let s = Speed::new();
            let idle = s.window();
            assert!(of_kind(idle.tree(), "ui.Stepper").is_empty(), "no stage before a run");
            s.start();
            let window = s.window();
            let stepper = of_kind(window.tree(), "ui.Stepper");
            assert_eq!(stepper.len(), 1);
            assert_eq!(
                stepper[0].props.get("current"),
                Some(&Value::Int(1)),
                "the download stage is current"
            );
        }

        phase a_finished_run_shows_its_numbers;
        {
            let s = Speed::new();
            s.start();
            s.finish();
            let window = s.window();
            let tiles = of_kind(window.tree(), "ui.StatTile");
            assert_eq!(tiles.len(), 3, "ping, download and upload");
            let values: Vec<String> = tiles
                .iter()
                .map(|t| {
                    format!(
                        "{} {}",
                        text_prop(t, "value"),
                        t.props.get("unit").and_then(Value::as_text).unwrap_or_default()
                    )
                })
                .collect();
            assert_eq!(values[0].split(' ').nth(1), Some("ms"));
            assert_eq!(values[1].split(' ').nth(1), Some("Mbps"));
            assert_eq!(values[2].split(' ').nth(1), Some("Mbps"));
            let facts = of_kind(window.tree(), "ui.Property");
            let said: Vec<String> = facts
                .iter()
                .map(|p| format!("{}: {}", text_prop(p, "label"), text_prop(p, "value")))
                .collect();
            assert!(said.contains(&"Server: Cloudflare at LHR".to_owned()), "{said:?}");
            assert!(said.contains(&"Data used: 12 MB".to_owned()), "{said:?}");
            assert!(texts(window.tree()).iter().any(|t| t == "Test complete"));
        }

        phase the_connection_quality_follows_the_numbers;
        {
            let s = Speed::new();
            s.start();
            s.finish();
            let window = s.window();
            let badges: Vec<String> = of_kind(window.tree(), "ui.Badge")
                .iter()
                .filter_map(|b| b.props.get("text").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert_eq!(badges, ["Excellent", "Excellent", "Excellent"], "a fast connection");

            // A slow connection: a measurement of one whose numbers are slow. It is recorded
            // after the run so the page's newest measurement is the slow one.
            let slow = Speed::new();
            slow.start();
            slow.finish();
            slow.record(120.0, 20.0, 10.0, 1.0, "Cloudflare at FRA");
            let window = slow.window();
            let badges: Vec<String> = of_kind(window.tree(), "ui.Badge")
                .iter()
                .filter_map(|b| b.props.get("text").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert_eq!(badges, ["Limited", "Limited", "Limited"], "a slow connection");

            let fair = Speed::new();
            fair.start();
            fair.finish();
            fair.record(40.0, 10.0, 30.0, 5.0, "Cloudflare");
            let window = fair.window();
            let badges: Vec<String> = of_kind(window.tree(), "ui.Badge")
                .iter()
                .filter_map(|b| b.props.get("text").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert_eq!(
                badges,
                ["Good", "Good", "Good"],
                "a connection that carries each use adequately reads as good, not excellent"
            );
        }

        phase recent_tests_read_newest_first_without_the_one_on_the_dial;
        {
            let s = Speed::new();
            s.record(14.1, 2.2, 584.6, 109.3, "Cloudflare at LHR");
            s.record(12.4, 1.8, 618.2, 112.9, "Cloudflare at LHR");
            s.start();
            s.finish();
            let window = s.window();
            let rows: Vec<String> = of_kind(window.tree(), "ui.ListRow")
                .iter()
                .map(|r| text_prop(r, "title"))
                .collect();
            assert_eq!(
                rows,
                ["618.2 Mbps down", "584.6 Mbps down"],
                "the runs before this one, newest first"
            );
        }

        phase a_failed_run_says_why;
        {
            let s = Speed::new();
            *s.services.offline.borrow_mut() = true;
            s.start();
            let run = s.run();
            let Value::Ref(run) = &run else { panic!("a run stands on the page: {run:?}") };
            assert_eq!(said(&s.rt.read(&run.target, "phase").unwrap()), "error");
            let window = s.window();
            let banner = of_kind(window.tree(), "ui.Banner");
            assert_eq!(banner.len(), 1, "{}", dump(window.tree()));
            assert_eq!(text_prop(banner[0], "title"), "Test interrupted");
            assert!(
                text_prop(banner[0], "message").contains("connection refused"),
                "{}",
                text_prop(banner[0], "message")
            );
            assert!(texts(window.tree()).iter().any(|t| t == "Ready to try again"));
            assert!(of_kind(window.tree(), "ui.Stepper").is_empty(), "no stage after a failure");
            assert!(s.results().is_empty(), "and nothing was measured");
        }

        phase a_server_that_stops_answering_ends_the_run_without_a_measurement;
        {
            let s = Speed::new();
            s.start();
            *s.services.offline.borrow_mut() = true;
            s.poll();
            assert_eq!(said(&s.run_field("phase")), "error");
            assert!(s.results().is_empty(), "nothing was measured");
        }

        phase the_ready_page_centres_the_test_and_names_the_server_once;
        {
            let s = Speed::new();
            let window = s.window();
            let named = texts(window.tree())
                .iter()
                .filter(|t| t.contains("Cloudflare"))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                named,
                ["Measures against the nearest Cloudflare server"],
                "named once"
            );
            // The bar carries the app's primary action as the window's button too; the page's
            // own action stands under the dial.
            assert!(
                buttons(window.tree()).iter().any(|b| b == "Run speed test"),
                "the window offers the run"
            );
            assert!(!buttons(window.tree()).iter().any(|b| b == "Stop the test"));
            assert_eq!(text_prop(dial(window.tree()), "readout"), "", "an empty dial");
        }

        phase clearing_forgets_every_measurement_and_the_dial_starts_over;
        {
            let s = Speed::new();
            s.record(12.0, 1.5, 640.0, 118.0, "Cloudflare");
            s.call("clear", vec![Value::resource(&s.root)]);
            assert!(s.results().is_empty());
            let window = s.window();
            assert!(buttons(window.tree()).iter().any(|b| b == "Run speed test"));
            let refused = s
                .rt
                .invoke_action(
                    &me(),
                    (
                        s.app.action("clear").as_str(),
                        vec![Value::resource(&s.root)],
                    ),
                )
                .unwrap_err()
                .to_string();
            assert!(refused.contains("No test has been measured"), "{refused}");
        }

        phase history_says_what_a_run_measured;
        {
            let s = Speed::new();
            s.record(12.0, 1.5, 640.0, 118.0, "Cloudflare");
            s.call("clear", vec![Value::resource(&s.root)]);
            let actions: Vec<String> = s
                .versions()
                .into_iter()
                .filter(|action| action != "$create")
                .collect();
            let recorded = s.app.action("record");
            let cleared = s.app.action("clear");
            assert_eq!(actions, [recorded, cleared], "the record's own changes, in order");
            let did = |name: &str| {
                s.app
                    .meta()
                    .actions
                    .iter()
                    .find(|a| a.name == name)
                    .and_then(|a| a.did.clone())
            };
            assert_eq!(did("record").as_deref(), Some("recorded a measurement"));
            assert_eq!(did("clear").as_deref(), Some("cleared the measurements"));
        }
    }
}
