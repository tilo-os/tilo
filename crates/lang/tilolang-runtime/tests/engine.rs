//! `engine.call`, `engine.start`, `engine.job` and `engine.cancel` end to end: an app's action
//! asks the device for work by name and reads the answer as JSON; a long job is the device's
//! record, which runs the actions its start named as it moves and ends, and which an action or a
//! view reads. A device that answers no engine work refuses, rather than pretending.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use tilolang_runtime::host_work::{EngineHost, JobHandlers};
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.engined "Engine"

type Page {
    words: Text
    job: Text = ""
    painted: Text = ""
    steps: Int = 0
}

/// What the device answered, kept where a test reads it.
action decode(p: Page) {
    match engine.call("page.decode", "first") {
        .ok(text) => {
            p.words = text
        }
        .err(why) => {
            p.words = why
        }
    }
}

/// A job: the device runs it, and runs `painted` when it ends and `moved` as it goes.
action paint(p: Page) {
    match engine.start(
        "page.paint",
        "second",
        done: j -> painted(p, j),
        progress: j -> moved(p, j),
    ) {
        .ok(job) => {
            p.job = job
        }
        .err(why) => {
            p.words = why
        }
    }
}

/// The job ended: what it made, or why it made nothing.
action painted(p: Page, j: host.Job) {
    if j.state == .done {
        p.painted = j.detail
    } else {
        p.painted = j.error
    }
}

/// A job started, and then a call of the app's own: the run replays at the call, and reads the
/// job it started the first time rather than starting another.
action paint_and_note(p: Page) {
    match engine.start("page.paint", "second", done: j -> painted(p, j)) {
        .ok(job) => {
            noted(p, job)
        }
        .err(why) => {
            p.words = why
        }
    }
}

/// Keep the job's path.
action noted(p: Page, job: Text) {
    p.job = job
}

/// The job moved on.
action moved(p: Page, j: host.Job) {
    p.steps = j.done
}

/// Where the job stands now, as an action reads it once.
action looked(p: Page) {
    if let j = engine.job(p.job) {
        p.words = "{j.op}: {j.done} of {j.total}"
    } else {
        p.words = "no such job"
    }
}

/// Stop the job.
action stop(p: Page) {
    engine.cancel(p.job)
}

/// How far the job has got, as a window shows it while it runs.
view bar(p: Page) {
    if let j = engine.job(p.job) {
        text("{j.done} of {j.total}")
    } else {
        text("not started")
    }
}

/// The picture the device drew, shown by the address it kept it at.
action drawn(p: Page) {
    match engine.picture("page.render", "third") {
        .ok(file) => {
            p.words = file.url
        }
        .err(why) => {
            p.words = why
        }
    }
}

/// A picture the device refuses reads as its words.
action drawn_unknown(p: Page) {
    match engine.picture("page.unknown", "") {
        .ok(file) => {
            p.words = file.url
        }
        .err(why) => {
            p.words = why
        }
    }
}

/// A refusal reads as the device's words, never as a success.
action refused(p: Page) {
    match engine.call("page.unknown", "") {
        .ok(text) => {
            p.words = text
        }
        .err(why) => {
            p.words = why
        }
    }
}
"#;

    const JOB: &str = "devices/ab/jobs/paint-1";

    /// What the device was asked, and how it answers.
    #[derive(Clone, Default)]
    struct Device_(
        Rc<RefCell<Vec<(String, String, String)>>>,
        Rc<RefCell<Vec<JobHandlers>>>,
        Rc<RefCell<Vec<String>>>,
    );

    /// The job's record as the device that holds it says it stands.
    fn record(state: &str, detail: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": JOB,
            "op": "page.paint",
            "state": state,
            "detail": detail,
        })
    }

    impl EngineHost for Device_ {
        fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            if op == "page.unknown" {
                return Err(format!("`{op}` is not this app's: {app} asks its own ops"));
            }
            if op == "page.render" {
                self.0
                    .borrow_mut()
                    .push((app.to_owned(), op.to_owned(), input.to_owned()));
                return Ok(
                    r#"{"url":"tilo-blob://drawn","name":"page.png","media":"image/png"}"#
                        .to_owned(),
                );
            }
            self.0
                .borrow_mut()
                .push((app.to_owned(), op.to_owned(), input.to_owned()));
            Ok(format!(r#"{{"op":"{op}","input":"{input}"}}"#))
        }

        fn start(
            &self,
            app: &str,
            op: &str,
            input: &str,
            handlers: &JobHandlers,
        ) -> Result<String, String> {
            self.0
                .borrow_mut()
                .push((app.to_owned(), op.to_owned(), input.to_owned()));
            self.1.borrow_mut().push(handlers.clone());
            Ok(JOB.to_owned())
        }

        fn job(&self, path: &str) -> Result<Option<serde_json::Value>, String> {
            Ok(
                (path == JOB)
                    .then(|| record("running", serde_json::json!({"done": 2, "total": 5}))),
            )
        }

        fn cancel(&self, job: &str) -> bool {
            self.2.borrow_mut().push(job.to_owned());
            job == JOB
        }
    }

    struct Bench {
        rt: Runtime,
        app: App,
        page: ResourceRef,
        device: Device_,
    }

    impl Bench {
        fn new(answering: bool) -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device_::default();
            if answering {
                rt.set_engine(device.clone());
            }
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            let page = rt.create_root("Page").unwrap();
            Self {
                rt,
                app,
                page,
                device,
            }
        }

        fn call(&self, action: &str) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), vec![Value::resource(&self.page)]))
                .unwrap()
        }

        fn words(&self) -> String {
            self.text("words")
        }

        fn text(&self, field: &str) -> String {
            match self.rt.read(&self.page, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("the page's {field} reads {other:?}"),
            }
        }

        /// The device runs what the job's start named, now that it moved to `record`: the
        /// handler it kept, filled with the job, invoked as the person who started it.
        fn moves(
            &self,
            handler: impl Fn(&JobHandlers) -> Option<serde_json::Value>,
            record: &serde_json::Value,
        ) {
            let kept = handler(&self.device.1.borrow()[0]).expect("the start named this handler");
            let call = self.rt.job_call(&kept, record).unwrap();
            self.rt.invoke_action(&me(), call).unwrap();
        }
    }

    #[test]
    fn a_call_asks_the_device_by_name_and_keeps_its_json() {
        let bench = Bench::new(true);
        bench.call("decode");
        assert_eq!(
            bench.device.0.borrow().as_slice(),
            [(
                "so.tilo.engined".to_owned(),
                "page.decode".to_owned(),
                "first".to_owned()
            )],
            "the op, its input and the app asking reach the device"
        );
        assert_eq!(bench.words(), r#"{"op":"page.decode","input":"first"}"#);
    }

    #[test]
    fn a_job_runs_the_actions_its_start_named_as_it_moves_and_ends() {
        let bench = Bench::new(true);
        bench.call("paint");
        assert_eq!(
            bench.device.0.borrow().as_slice(),
            [(
                "so.tilo.engined".to_owned(),
                "page.paint".to_owned(),
                "second".to_owned()
            )]
        );
        assert_eq!(bench.text("job"), JOB, "the start answers the job's path");
        let kept = bench.device.1.borrow()[0].clone();
        assert!(
            kept.done.is_some() && kept.progress.is_some(),
            "the device keeps both handlers with the job: {kept:?}"
        );

        bench.moves(
            |h| h.progress.clone(),
            &record("running", serde_json::json!({"done": 3, "total": 5})),
        );
        assert_eq!(
            bench.rt.read(&bench.page, "steps").unwrap(),
            Value::Int(3),
            "`progress:` is handed the job as it moved"
        );
        assert_eq!(bench.text("painted"), "", "nothing ended yet");

        bench.moves(
            |h| h.done.clone(),
            &record("done", serde_json::json!({"url": "tilo-blob://painted"})),
        );
        assert_eq!(
            bench.text("painted"),
            r#"{"url":"tilo-blob://painted"}"#,
            "`done:` is handed the job and reads what it made"
        );

        bench.moves(
            |h| h.done.clone(),
            &record(
                "failed",
                serde_json::json!({"error": "the printer is out of paper"}),
            ),
        );
        assert_eq!(
            bench.text("painted"),
            "the printer is out of paper",
            "a job that failed says why in its error"
        );
    }

    #[test]
    fn a_replayed_run_reads_the_job_it_started_and_starts_no_other() {
        let bench = Bench::new(true);
        let outcome = bench.call("paint_and_note");
        assert!(
            matches!(outcome, InvokeOutcome::Done(_)),
            "the run ends: {outcome:?}"
        );
        assert_eq!(
            bench.device.0.borrow().len(),
            1,
            "the device starts the job once, not again when the run replays at its call: {:?}",
            bench.device.0.borrow()
        );
        assert_eq!(
            bench.device.1.borrow().len(),
            1,
            "one job keeps one set of handlers"
        );
        assert_eq!(
            bench.text("job"),
            JOB,
            "the call reads the path the start answered the first time"
        );
    }

    #[test]
    fn an_action_reads_where_a_job_stands_and_stops_it() {
        let bench = Bench::new(true);
        bench.call("paint");
        bench.call("looked");
        assert_eq!(bench.words(), "page.paint: 2 of 5");
        bench.call("stop");
        assert_eq!(bench.device.2.borrow().as_slice(), [JOB.to_owned()]);
    }

    #[test]
    fn a_window_follows_a_job_without_asking_on_a_clock() {
        let bench = Bench::new(true);
        bench.call("paint");
        let mut window = bench
            .rt
            .open_view(
                &me(),
                &bench.app.view("bar"),
                vec![Value::resource(&bench.page)],
            )
            .unwrap();
        let shown = |window: &tilolang_runtime::ViewInstance| -> Vec<String> {
            of_kind(window.tree(), "ui.Text")
                .into_iter()
                .filter_map(|node| match &node.props["value"] {
                    Value::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            shown(&window),
            ["not started"],
            "nothing is read before the host follows it"
        );
        assert_eq!(
            bench.rt.take_wanted_jobs(),
            [JOB.to_owned()],
            "the window's read asks the host to follow the job"
        );

        bench.rt.set_job(
            JOB,
            &record("running", serde_json::json!({"done": 4, "total": 5})),
        );
        assert!(
            window.woken(),
            "the job's move wakes the window that shows it"
        );
        window.poll_patches().unwrap();
        assert_eq!(shown(&window), ["4 of 5"]);
        assert!(
            bench.rt.take_wanted_jobs().is_empty(),
            "a job the host follows is asked for once"
        );
    }

    #[test]
    fn a_picture_answers_the_file_the_device_kept() {
        let bench = Bench::new(true);
        bench.call("drawn");
        assert_eq!(
            bench.device.0.borrow().as_slice(),
            [(
                "so.tilo.engined".to_owned(),
                "page.render".to_owned(),
                "third".to_owned()
            )],
            "the op, its input and the app asking reach the device"
        );
        assert_eq!(bench.words(), "tilo-blob://drawn");
    }

    #[test]
    fn a_picture_the_device_refuses_reads_as_its_words() {
        let bench = Bench::new(true);
        bench.call("drawn_unknown");
        assert_eq!(
            bench.words(),
            "`page.unknown` is not this app's: so.tilo.engined asks its own ops"
        );
    }

    #[test]
    fn a_refusal_reads_as_the_device_s_words() {
        let bench = Bench::new(true);
        bench.call("refused");
        assert_eq!(
            bench.words(),
            "`page.unknown` is not this app's: so.tilo.engined asks its own ops"
        );
    }

    #[test]
    fn a_device_that_answers_no_engine_work_refuses() {
        let bench = Bench::new(false);
        bench.call("decode");
        assert!(
            bench.words().contains("answers no engine work"),
            "the action reads the refusal, not a success: {}",
            bench.words()
        );
    }
}
