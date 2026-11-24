//! Speed Test (`apps/tl/speed-test/app.tl`) end to end: a run measured by the device's engine and
//! followed through the app's own actions, its workspaces measurements kept and cleared, and its
//! window — the dial, the stages, the numbers, the facts, the quality and the runs before.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/speed-test/app.tl");

    /// The device's engine: the answers `status` gives, in order, and what it was asked.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        answers: Rc<RefCell<Vec<String>>>,
        cancelled: Rc<RefCell<Vec<String>>>,
    }

    impl Engine {
        /// The answer the next `status` gives, or the device saying it knows no such job.
        fn answer(&self, state: &str) {
            self.answers.borrow_mut().push(state.to_owned());
        }

        fn asked(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            Err(format!("`{op}` is no op this device runs"))
        }

        fn start(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            self.asked.borrow_mut().push((op.to_owned(), String::new()));
            if op == "run" {
                Ok(r#"{"job":"job-1"}"#.to_owned())
            } else {
                Err(format!("`{op}` is no job this device starts"))
            }
        }

        fn status(&self, job: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push(("status".to_owned(), job.to_owned()));
            if self.answers.borrow().is_empty() {
                return Err(format!("no job `{job}` runs on this device"));
            }
            let mut answers = self.answers.borrow_mut();
            Ok(answers.remove(0))
        }

        fn cancel(&self, job: &str) -> bool {
            self.cancelled.borrow_mut().push(job.to_owned());
            true
        }
    }

    /// A run the engine is measuring: the state line its `status` answers.
    fn running(phase: &str, live: f64, bytes: i64, server: &str) -> String {
        format!(
            r#"{{"state":"running","phase":"{phase}","live":{live},"bytes":{bytes},"server":"{server}"}}"#
        )
    }

    /// A run the engine finished.
    fn done(ping: f64, jitter: f64, download: f64, upload: f64, bytes: i64) -> String {
        format!(
            r#"{{"state":"done","phase":"upload","live":{upload},"bytes":{bytes},"server":"Cloudflare at LHR","ping":{ping},"jitter":{jitter},"download":{download},"upload":{upload},"device":"Laptop"}}"#
        )
    }

    struct Speed {
        rt: Runtime,
        app: App,
        engine: Engine,
        root: ResourceRef,
    }

    impl Speed {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let engine = Engine::default();
            rt.set_engine(engine.clone());
            let root = rt.create_root("SpeedTest").unwrap();
            Self {
                rt,
                app,
                engine,
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
        fn speed_test_measures_and_shows_what_it_measured;

        phase a_run_follows_the_device_and_keeps_what_it_measured;
        {
            let s = Speed::new();
            assert_eq!(s.start(), InvokeOutcome::Done(Value::Unit));
            assert_eq!(s.engine.asked(), ["run"], "the device measures");
            s.engine.answer(&running("download", 386.4, 2000000, "Cloudflare at LHR"));
            s.poll();
            let run = s.run();
            let Value::Ref(run) = &run else { panic!("a run stands on the page: {run:?}") };
            assert_eq!(
                said(&s.rt.read(&run.target, "phase").unwrap()),
                "download",
                "the dial follows the stage",
            );
            s.engine
                .answer(&done(11.8, 1.6, 642.7, 118.4, 12000000));
            s.poll();
            let kept = s.results();
            assert_eq!(kept.len(), 1, "the finished run is kept");
            assert_eq!(kept[0].0, "Cloudflare at LHR");
            assert_eq!(kept[0].2, "Laptop", "the device that ran it is kept");
            assert!(kept[0].1.starts_with("642.7"), "what it measured down: {kept:?}");
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
            assert_eq!(*s.engine.cancelled.borrow(), ["job-1"], "the device stops measuring");
            assert!(matches!(s.run(), Value::Opt(None)), "no run stands on the page");
            let window = s.window();
            assert!(buttons(window.tree()).iter().any(|b| b == "Run speed test"));
            assert!(!buttons(window.tree()).iter().any(|b| b == "Stop the test"));
            assert!(texts(window.tree()).iter().any(|t| t == "Ready to test"));
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
            s.engine.answer(&running("ping", 13.0, 0, ""));
            s.poll();
            let window = s.window();
            assert_eq!(text_prop(dial(window.tree()), "readout"), "13");
            assert_eq!(text_prop(dial(window.tree()), "caption"), "ms");
            assert!(texts(window.tree()).iter().any(|t| t == "Measuring latency"));

            s.engine.answer(&running("upload", 90.0, 12000000, "Cloudflare at LHR"));
            s.poll();
            let window = s.window();
            assert_eq!(text_prop(dial(window.tree()), "readout"), "90.0");
            assert_eq!(text_prop(dial(window.tree()), "caption"), "Mbps");
            assert!(texts(window.tree()).iter().any(|t| t == "Measuring upload"));
            let filled = dial(window.tree())
                .props
                .get("value")
                .cloned()
                .expect("the dial has a value");
            assert!(
                matches!(filled, Value::Float(f) if (f - 0.5).abs() < 1e-9),
                "90 of 180 marks half the arc: {filled:?}"
            );
        }

        phase the_stages_follow_the_run;
        {
            let s = Speed::new();
            let idle = s.window();
            assert!(of_kind(idle.tree(), "ui.Stepper").is_empty(), "no stage before a run");
            s.start();
            s.engine.answer(&running("download", 300.0, 1000, ""));
            s.poll();
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
            s.engine.answer(&done(11.8, 1.6, 642.7, 118.4, 12000000));
            s.poll();
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
            assert_eq!(values, ["12 ms", "642.7 Mbps", "118.4 Mbps"]);
            let facts = of_kind(window.tree(), "ui.Property");
            let said: Vec<String> = facts
                .iter()
                .map(|p| format!("{}: {}", text_prop(p, "label"), text_prop(p, "value")))
                .collect();
            assert!(said.contains(&"Server: Cloudflare at LHR".to_owned()), "{said:?}");
            assert!(said.contains(&"Jitter: 1.6 ms".to_owned()), "{said:?}");
            assert!(said.contains(&"Data used: 12 MB".to_owned()), "{said:?}");
            assert!(texts(window.tree()).iter().any(|t| t == "Test complete"));
        }

        phase the_connection_quality_follows_the_numbers;
        {
            let s = Speed::new();
            s.record(11.0, 1.0, 600.0, 100.0, "Cloudflare");
            s.rt
                .invoke_action(
                    &me(),
                    (
                        s.app.action("start").as_str(),
                        vec![Value::resource(&s.root)],
                    ),
                )
                .unwrap();
            s.engine
                .answer(&done(11.0, 1.0, 600.0, 100.0, 12000000));
            s.poll();
            let window = s.window();
            let badges: Vec<String> = of_kind(window.tree(), "ui.Badge")
                .iter()
                .filter_map(|b| b.props.get("text").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert_eq!(badges, ["Excellent", "Excellent", "Excellent"], "a fast connection");

            let slow = Speed::new();
            slow.record(120.0, 20.0, 10.0, 1.0, "Cloudflare");
            slow.start();
            slow.engine
                .answer(&done(120.0, 20.0, 10.0, 1.0, 12000000));
            slow.poll();
            let window = slow.window();
            let badges: Vec<String> = of_kind(window.tree(), "ui.Badge")
                .iter()
                .filter_map(|b| b.props.get("text").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert_eq!(badges, ["Limited", "Limited", "Limited"], "a slow connection");
        }

        phase recent_tests_read_newest_first_without_the_one_on_the_dial;
        {
            let s = Speed::new();
            s.record(14.1, 2.2, 584.6, 109.3, "Cloudflare at LHR");
            s.record(12.4, 1.8, 618.2, 112.9, "Cloudflare at LHR");
            s.start();
            s.engine
                .answer(&done(11.8, 1.6, 642.7, 118.4, 12000000));
            s.poll();
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
            s.start();
            s.engine.answer(
                r#"{"state":"failed","phase":"ping","failure":"Could not reach the test server: connection refused"}"#,
            );
            s.poll();
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
        }

        phase a_device_that_stops_answering_ends_the_run_without_a_measurement;
        {
            let s = Speed::new();
            s.start();
            s.poll();
            assert!(matches!(s.run(), Value::Opt(None)), "the run is over");
            assert!(s.results().is_empty(), "and nothing was measured");
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
