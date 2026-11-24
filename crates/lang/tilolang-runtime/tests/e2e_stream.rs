//! A stream an app shows: the device's engine hands its newest frame over as each arrives, the
//! app shows it by its address, and cancelling the stream takes the frame away.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use crate::common::*;
    use tilolang_runtime::host_work::{EngineHost, Frame, Push};
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = r#"
app so.tilo.screen "Screen"

/// What the window shows: the stream it watches, and why it watches none.
type Screen {
    /// The stream whose newest frame the window shows.
    job: Text = ""
    /// What the device said when it runs no such stream.
    trouble: Text = ""
}

/// Watch the device's screen: its frames move the picture without the app asking.
action watch(s: Screen) {
    match engine.stream("screen", "") {
        .ok(job) => {
            s.job = job
            s.trouble = ""
        }
        .err(why) => {
            s.trouble = why
        }
    }
}

/// Stop watching: the last frame goes with the stream.
action stop(s: Screen) {
    if s.job != "" {
        engine.cancel(s.job)
    }
    s.job = ""
}

view window(s: Screen) {
    stack {
        ui.Image(src: "tilo-live://{s.job}", alt: "Another machine's screen")
        text(s.trouble)
    }
}
"#;

    /// The device's engine: the streams it started and the frames it sends them.
    #[derive(Clone, Default)]
    struct Device {
        streams: Rc<RefCell<Vec<(String, String)>>>,
        push: Rc<RefCell<Option<Arc<dyn Push>>>>,
        cancelled: Rc<RefCell<Vec<String>>>,
    }

    impl Device {
        fn push(&self) -> Arc<dyn Push> {
            self.push
                .borrow()
                .clone()
                .expect("the app started a stream")
        }

        /// Send the stream `slot` its `generation`th frame, `width`×`height` white pixels.
        fn send(&self, slot: &str, generation: u64, width: u32, height: u32) {
            let pixels = vec![255_u8; usize::try_from(width * height * 4).unwrap()];
            self.push().frame(Frame {
                slot: slot.to_owned(),
                generation,
                width,
                height,
                pixels: Arc::new(pixels),
            });
        }
    }

    impl EngineHost for Device {
        fn call(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            Err(format!("`{op}` is no op this device answers now"))
        }

        fn start(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            Err(format!("`{op}` is no job this device runs"))
        }

        fn stream(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            push: Arc<dyn Push>,
        ) -> Result<String, String> {
            self.streams
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            *self.push.borrow_mut() = Some(push);
            Ok("screen-1".to_owned())
        }

        fn status(&self, job: &str) -> Result<String, String> {
            if job == "screen-1" {
                Ok(r#"{"state":"running"}"#.to_owned())
            } else {
                Err(format!("no job `{job}` runs on this device"))
            }
        }

        fn cancel(&self, job: &str) -> bool {
            self.cancelled.borrow_mut().push(job.to_owned());
            job == "screen-1"
        }
    }

    /// A device that runs no streams at all: the default answers.
    struct Bare;

    impl EngineHost for Bare {
        fn call(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            Err(format!("`{op}` is no op this device answers now"))
        }

        fn start(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            Err(format!("`{op}` is no job this device runs"))
        }

        fn status(&self, job: &str) -> Result<String, String> {
            Err(format!("no job `{job}` runs on this device"))
        }

        fn cancel(&self, _job: &str) -> bool {
            false
        }
    }

    struct Screen {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Device,
    }

    impl Screen {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let device = Device::default();
            rt.set_engine(device.clone());
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let root = rt.create_root("Screen").unwrap();
            Self {
                rt,
                app,
                root,
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn window(&self) -> ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("window"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap()
        }

        fn text(&self, field: &str) -> String {
            match self.rt.read(&self.root, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn image(&self, view: &ViewInstance) -> String {
            let node: &UiNode = of_kind(view.tree(), "ui.Image")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the window shows a picture: {}", dump(view.tree())));
            match &node.props["src"] {
                Value::Text(src) => src.clone(),
                other => panic!("the picture reads {other:?}"),
            }
        }
    }

    tilo_test_env::scenario! {
        fn a_stream_shows_its_newest_frame_and_ends_with_its_job;

        phase a_watched_stream_names_its_frame_and_the_window_shows_that_address;
        {
            let s = Screen::new();
            assert_eq!(s.text("job"), "", "nothing is watched until the person watches");
            s.call("watch", vec![Value::resource(&s.root)]);
            assert_eq!(s.text("job"), "screen-1", "the device named the stream");
            assert_eq!(
                s.device.streams.borrow().as_slice(),
                &[("screen".to_owned(), String::new())],
                "the device was asked for the screen stream"
            );
            let window = s.window();
            assert_eq!(
                s.image(&window),
                "tilo-live://screen-1",
                "the window shows the stream's newest frame by its address"
            );
        }

        phase a_frame_reaches_the_device_and_wakes_the_window;
        {
            let s = Screen::new();
            let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = Arc::clone(&wakes);
            s.rt.set_frame_wake(Arc::new(move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
            s.call("watch", vec![Value::resource(&s.root)]);
            assert!(s.rt.frame("screen-1").is_none(), "no frame before the device sends one");
            assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 0);

            s.device.send("screen-1", 1, 3, 2);
            let frame = s.rt.frame("screen-1").expect("the frame the device sent");
            assert_eq!((frame.width, frame.height), (3, 2));
            assert_eq!(frame.pixels.len(), 3 * 2 * 4);
            assert_eq!(
                wakes.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "the window was woken to draw the frame"
            );

            s.device.send("screen-1", 2, 2, 2);
            let newest = s.rt.frame("screen-1").expect("the newest frame");
            assert_eq!(
                (newest.generation, newest.width, newest.height),
                (2, 2, 2),
                "a slot keeps the newest frame, never a backlog"
            );
        }

        phase stopping_the_stream_takes_its_last_frame_away;
        {
            let s = Screen::new();
            s.call("watch", vec![Value::resource(&s.root)]);
            s.device.send("screen-1", 1, 1, 1);
            assert!(s.rt.frame("screen-1").is_some());
            s.call("stop", vec![Value::resource(&s.root)]);
            assert_eq!(s.text("job"), "");
            assert_eq!(s.device.cancelled.borrow().as_slice(), &["screen-1".to_owned()]);
            assert!(
                s.rt.frame("screen-1").is_none(),
                "a stream that ended shows nothing rather than its last frame"
            );
            assert_eq!(s.image(&s.window()), "tilo-live://");
        }

        phase a_device_that_runs_no_streams_says_so;
        {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_engine(Bare);
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let root = rt.create_root("Screen").unwrap();
            let id = app.action("watch");
            rt.invoke_action(&me(), (id.as_str(), vec![Value::resource(&root)]))
                .unwrap();
            assert_eq!(
                rt.read(&root, "trouble").unwrap(),
                Value::text("this device streams nothing to its apps"),
                "a refusal is never a job that would never send a frame"
            );
            assert_eq!(rt.read(&root, "job").unwrap(), Value::text(""));
        }
    }
}
