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
    let answer = engine.call("page.decode", "first")
    if let text = answer {
        p.words = text
    }
}

/// A job: started, asked about, then stopped.
action paint(p: Page) {
    let started = engine.start("page.paint", "second")
    if let job = started {
        let answer = engine.status(job)
        if let said = answer {
            p.words = said
        }
        engine.cancel(job)
    }
}

/// A refusal reads as the device's words, never as a success.
action refused(p: Page) {
    let answer = engine.call("page.unknown", "")
    if let why = answer {
        p.words = why
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
