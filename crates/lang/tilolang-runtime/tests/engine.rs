//! `engine.call`, `engine.start`, `engine.status` and `engine.cancel` end to end: an app's action
//! asks the device for work by name, reads the answer as JSON, and a long job says how far it has
//! got and can be stopped. A device that answers no engine work refuses, rather than pretending.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.engined "Engine"

type Page {
    words: Text
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

/// A job: started, asked about, then stopped.
action paint(p: Page) {
    match engine.start("page.paint", "second") {
        .ok(job) => {
            match engine.status(job) {
                .ok(said) => {
                    p.words = said
                }
                .err(why) => {
                    p.words = why
                }
            }
            engine.cancel(job)
        }
        .err(why) => {
            p.words = why
        }
    }
}

/// One request waited for: the device's answer is read here, and nothing keeps a job to ask.
action awaited(p: Page) {
    match engine.run("page.slow", "slow") {
        .ok(said) => {
            p.words = said
        }
        .err(why) => {
            p.words = why
        }
    }
}

/// What `engine.run` answers is its action's own answer: the caller reads it, not the window.
action answered(p: Page) -> Text {
    match engine.run("page.slow", "slow") {
        .ok(said) => said,
        .err(why) => why,
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

    /// What the device was asked, and how it answers.
    #[derive(Clone, Default)]
    struct Device_(Rc<RefCell<Vec<(String, String, String)>>>);

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

        fn start(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            self.0
                .borrow_mut()
                .push((app.to_owned(), op.to_owned(), input.to_owned()));
            Ok("job-1".to_owned())
        }

        fn status(&self, job: &str) -> Result<String, String> {
            Ok(format!(r#"{{"state":"done","job":"{job}"}}"#))
        }

        fn cancel(&self, job: &str) -> bool {
            job == "job-1"
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
            match self.rt.read(&self.page, "words").unwrap() {
                Value::Text(text) => text,
                other => panic!("the page's words read {other:?}"),
            }
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
    fn a_job_is_started_asked_about_and_stopped() {
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
        assert_eq!(bench.words(), r#"{"state":"done","job":"job-1"}"#);
    }

    /// A device whose job runs for a while: `status` says `running` the first `running` times,
    /// then `done` with what it made.
    #[derive(Clone, Default)]
    struct Slow {
        running: Rc<RefCell<usize>>,
        asked: Rc<RefCell<Vec<String>>>,
    }

    impl EngineHost for Slow {
        fn call(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            Err(format!("`{op}` is no call of this device"))
        }

        fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked.borrow_mut().push(format!("start {op} {input}"));
            Ok("job-slow".to_owned())
        }

        fn status(&self, job: &str) -> Result<String, String> {
            let mut asked = self.asked.borrow_mut();
            asked.push(format!("status {job}"));
            let mut running = self.running.borrow_mut();
            if *running > 0 {
                *running = running.saturating_sub(1);
                return Ok(r#"{"state":"running","done":1,"total":8}"#.to_owned());
            }
            Ok(r#"{"state":"done","answer":"201"}"#.to_owned())
        }

        fn cancel(&self, _job: &str) -> bool {
            false
        }
    }

    #[test]
    fn a_run_waits_for_the_device_and_answers_its_caller() {
        let bench = Bench::new(false);
        let slow = Slow::default();
        *slow.running.borrow_mut() = 2;
        bench.rt.set_engine(slow.clone());
        let id = bench.app.action("answered");
        let outcome = bench
            .rt
            .invoke_action(&me(), (id.as_str(), vec![Value::resource(&bench.page)]))
            .unwrap();
        let InvokeOutcome::Done(answer) = outcome else {
            panic!("the run ends with its answer, not {outcome:?}");
        };
        assert_eq!(
            answer,
            Value::text(r#"{"state":"done","answer":"201"}"#),
            "the caller reads the job's own state once it ends"
        );
        assert_eq!(
            slow.asked.borrow().as_slice(),
            [
                "start page.slow slow",
                "status job-slow",
                "status job-slow",
                "status job-slow",
            ],
            "the device starts the work once and is asked how it does until it ends"
        );
        assert_eq!(
            bench.rt.read(&bench.page, "words").unwrap(),
            Value::text(""),
            "the answer went to the caller, not into the window"
        );
    }

    #[test]
    fn a_run_keeps_its_answer_in_the_window_when_its_action_does() {
        let bench = Bench::new(false);
        let slow = Slow::default();
        bench.rt.set_engine(slow);
        bench.call("awaited");
        assert_eq!(bench.words(), r#"{"state":"done","answer":"201"}"#);
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
