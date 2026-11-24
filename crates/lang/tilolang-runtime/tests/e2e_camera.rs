//! Camera (`apps/tl/camera`): the cameras a device offers and the one the person shoots with, the
//! address a scanned code opens, the keys a photo and a way back are on, and what a recording and
//! a self-timer show while they run.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::host_work::{EngineHost, Push};
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, Value};

    const APP: &str = include_str!("../../../../apps/tl/camera/app.tl");

    /// The device's camera engine, as `engine.call` and `engine.start` reach it: the cameras it
    /// has, the code the newest frame holds, and a recording.
    #[derive(Clone, Default)]
    struct Device {
        cameras: Rc<RefCell<String>>,
        codes: Rc<RefCell<String>>,
        asked: Rc<RefCell<Vec<String>>>,
    }

    impl Device {
        fn answers_cameras(&self, devices: &str, prompt: bool) {
            *self.cameras.borrow_mut() = format!(r#"{{"devices":{devices},"prompt":{prompt}}}"#);
        }

        fn reads(&self, code: &str) {
            *self.codes.borrow_mut() = format!(
                r#"{{"codes":[{{"text":{},"x":0.1,"y":0.1,"w":0.2,"h":0.2}}]}}"#,
                json_text(code)
            );
        }

        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }

    fn json_text(text: &str) -> String {
        serde_json::to_string(text).expect("a payload is text")
    }

    impl EngineHost for Device {
        fn call(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            self.asked.borrow_mut().push(op.to_owned());
            match op {
                "cameras" => Ok(self.cameras.borrow().clone()),
                "codes-live" => Ok(self.codes.borrow().clone()),
                "stop-record" => Ok(
                    r#"{"video":{"name":"clip.webm","duration_ms":1200,"w":1280,"h":720,"size":9}}"#
                        .to_owned(),
                ),
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            match op {
                "record" => Ok(r#"{"job":"rec-1"}"#.to_owned()),
                other => Err(format!("`{other}` is no op this device starts")),
            }
        }

        fn stream(
            &self,
            _app: &str,
            _op: &str,
            _input: &str,
            _push: std::sync::Arc<dyn Push>,
        ) -> Result<String, String> {
            Err("this device shows no moving picture".to_owned())
        }

        fn status(&self, _job: &str) -> Result<String, String> {
            Ok(r#"{"state":"running","url":"","error":""}"#.to_owned())
        }

        fn cancel(&self, _job: &str) -> bool {
            true
        }
    }

    struct Camera {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Device,
        clock: Rc<RefCell<String>>,
    }

    impl Camera {
        fn new() -> Self {
            Self::with_clock("2026-11-24T09:00:00Z")
        }

        fn with_clock(at: &str) -> Self {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let clock = Rc::new(RefCell::new(at.to_owned()));
            let read = Rc::clone(&clock);
            rt.set_clock(move || read.borrow().clone());
            let device = Device::default();
            rt.set_engine(device.clone());
            let app = App::compile(APP, &[]);
            app.load(&rt);
            let root = rt.create_root("Camera").unwrap();
            Self {
                rt,
                app,
                root,
                device,
                clock,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn read(&self, at: &ResourceRef, field: &str) -> Value {
            self.rt.read(at, field).unwrap()
        }

        fn set_clock(&self, at: &str) {
            *self.clock.borrow_mut() = at.to_owned();
        }

        /// The person's preferences, made by the app's own action.
        fn prefs(&self) -> ResourceRef {
            returned_ref(self.call("prefs_of", vec![Value::resource(&self.root)]))
        }

        fn view(&self, name: &str) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view(name),
                    vec![Value::resource(&self.root)],
                )
                .unwrap()
        }
    }

    /// The labels and links a tree shows.
    fn buttons(tree: &tilolang_runtime::UiNode) -> Vec<String> {
        of_kind(tree, "ui.Button")
            .iter()
            .filter_map(|node| match node.props.get("label") {
                Some(Value::Text(label)) => Some(label.clone()),
                _ => None,
            })
            .collect()
    }

    fn links(tree: &tilolang_runtime::UiNode) -> Vec<String> {
        of_kind(tree, "ui.Link")
            .iter()
            .filter_map(|node| match node.props.get("url") {
                Some(Value::Text(url)) => Some(url.clone()),
                _ => None,
            })
            .collect()
    }

    /// What the tree's texts say.
    fn texts(tree: &tilolang_runtime::UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .filter_map(|node| match node.props.get("value") {
                Some(Value::Text(text)) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether any prop of any node is the text `wanted`.
    fn any_prop_text(tree: &tilolang_runtime::UiNode, wanted: &str) -> bool {
        tree.props.values().any(|value| match value {
            Value::Text(text) => text.contains(wanted),
            _ => false,
        }) || tree
            .children
            .iter()
            .any(|child| any_prop_text(child, wanted))
    }

    tilo_test_env::scenario! {
        fn camera_chooses_its_camera_opens_scanned_codes_and_shows_what_it_records;

        phase the_settings_lists_each_camera_and_the_person_chooses_one;
        {
            let c = Camera::new();
            c.device.answers_cameras(
                r#"[{"device":"cam-1","label":"Built-in"},{"device":"cam-2","label":"Studio"}]"#,
                true,
            );
            c.call("list_cameras", vec![Value::resource(&c.root)]);
            let prefs = c.prefs();
            assert_eq!(
                match c.read(&prefs, "cameras") {
                    Value::List(cameras) => cameras.len(),
                    other => panic!("cameras read {other:?}"),
                },
                2,
                "each camera the device offered is kept"
            );
            let mut page = c.view("settings_page");
            page.poll_patches().unwrap();
            let labels = buttons(page.tree());
            let rows: Vec<String> = of_kind(page.tree(), "ui.ListRow")
                .iter()
                .filter_map(|row| match row.props.get("title") {
                    Some(Value::Text(title)) => Some(title.clone()),
                    _ => None,
                })
                .collect();
            assert!(
                rows.iter().any(|row| row == "Built-in") && rows.iter().any(|row| row == "Studio"),
                "one row per camera: {rows:?}"
            );
            assert!(
                rows.iter().any(|row| row == "This computer's default camera"),
                "the system's own choice is offered: {rows:?}"
            );
            assert!(
                labels.iter().any(|label| label == "Look for cameras"),
                "the page offers a refresh: {labels:?}"
            );
            assert!(
                any_prop_text(page.tree(), "asks this computer for permission"),
                "a device that asks permission says so"
            );

            // Choosing the studio camera is the camera the engine is then asked for.
            c.call(
                "use_camera",
                vec![Value::resource(&c.root), Value::text("cam-2")],
            );
            assert_eq!(c.read(&c.prefs(), "device"), Value::text("cam-2"));
            assert!(
                any_prop_text(c.view("viewfinder").tree(), "Studio"),
                "the viewfinder names the camera it shoots with"
            );
            // Asking again forgets the choice and asks the device once more.
            let asked = c.device.asked().len();
            c.call("retry_camera", vec![Value::resource(&c.root)]);
            assert_eq!(c.read(&c.prefs(), "device"), Value::text(""));
            assert_eq!(
                c.device.asked().len(),
                asked + 1,
                "the refresh asks the engine for its cameras again"
            );

            // A computer with no camera says so rather than listing none.
            c.device.answers_cameras("[]", false);
            c.call("list_cameras", vec![Value::resource(&c.root)]);
            let mut page = c.view("settings_page");
            page.poll_patches().unwrap();
            assert!(
                any_prop_text(page.tree(), "No camera found"),
                "an empty list is said: {:?}",
                buttons(page.tree())
            );
        }

        phase a_scanned_code_opens_the_address_its_kind_names;
        {
            let c = Camera::new();
            // A MATMSG code writes an address and a subject, not a `mailto:` payload.
            c.device.reads("MATMSG:TO:team@example.com;SUB:Hello there;;");
            c.call("look", vec![Value::resource(&c.root)]);
            let mut scanner = c.view("scanner_page");
            scanner.poll_patches().unwrap();
            let opened = links(scanner.tree());
            assert_eq!(
                opened,
                ["mailto:team@example.com?subject=Hello%20there"],
                "Write email opens the address the code names: {opened:?}"
            );

            // A contact card's phone is a `tel:` link, not the card's text.
            c.device.reads(
                "BEGIN:VCARD\nVERSION:3.0\nFN:Ada Lovelace\nTEL:+49301234\nEMAIL:ada@example.com\nEND:VCARD",
            );
            c.call("look", vec![Value::resource(&c.root)]);
            scanner.poll_patches().unwrap();
            let opened = links(scanner.tree());
            assert_eq!(
                opened,
                ["tel:+49301234"],
                "Call opens the phone number the card names: {opened:?}"
            );

            // A network's code carries no address to open, only what to copy.
            c.device.reads("WIFI:T:WPA;S:Studio;P:secret;;");
            c.call("look", vec![Value::resource(&c.root)]);
            scanner.poll_patches().unwrap();
            assert!(links(scanner.tree()).is_empty(), "a network opens nothing");
        }

        phase space_takes_the_photo_and_escape_is_the_windows_way_back;
        {
            let c = Camera::new();
            let shutter = c.app.meta().action_named("shutter").expect("the shutter");
            assert_eq!(
                shutter.key.as_deref(),
                Some("space"),
                "Space presses the shutter"
            );
            // The scanner and the document scanner are places the window goes into, opened with
            // `open(c, view: ...)`: the window's Back leaves them, and Escape takes that Back
            // (the toolkit's own `escape_pops_the_apps_page_stack`). The app claims no Escape of
            // its own, which would take the key from the way back.
            assert!(
                c.app
                    .meta()
                    .actions
                    .iter()
                    .all(|action| action.key.as_deref() != Some("escape")),
                "no action of the app takes Escape from the window's way back"
            );
        }

        phase a_recording_shows_its_elapsed_time;
        {
            let c = Camera::new();
            c.call("use_mode", vec![Value::resource(&c.root), Value::text("video")]);
            c.call("start_recording", vec![Value::resource(&c.root)]);
            assert_eq!(c.read(&c.prefs(), "recording"), Value::text("rec-1"));
            assert!(
                matches!(
                    c.read(&c.prefs(), "recording_since"),
                    Value::Opt(Some(_))
                ),
                "the recording names the instant it began"
            );
            let mut view = c.view("viewfinder");
            view.poll_patches().unwrap();
            let counting = of_kind(view.tree(), "ui.Countdown");
            assert_eq!(counting.len(), 1, "the recording counts as it runs");
            assert_eq!(
                counting[0].props.get("label"),
                Some(&Value::text("Recording")),
                "and says what it counts"
            );
            // Stopping it forgets the instant, so nothing counts any more.
            c.call("stop_recording", vec![Value::resource(&c.root)]);
            assert_eq!(c.read(&c.prefs(), "recording"), Value::text(""));
            assert_eq!(c.read(&c.prefs(), "recording_since"), Value::Opt(None));
        }

        phase the_self_timer_counts_visible_seconds_with_a_way_to_cancel;
        {
            let c = Camera::new();
            c.call("set_timer", vec![Value::resource(&c.root), Value::Int(3)]);
            c.call("arm_timer", vec![Value::resource(&c.root)]);
            assert!(
                matches!(c.read(&c.prefs(), "shutter_at"), Value::Opt(Some(_))),
                "the self-timer names the instant it fires"
            );
            let mut view = c.view("viewfinder");
            view.poll_patches().unwrap();
            assert!(
                any_prop_text(view.tree(), "Tap the shutter to cancel"),
                "the countdown says how to stop it"
            );
            assert!(
                texts(view.tree()).iter().any(|text| text == "3"),
                "the seconds left stand on the screen: {:?}",
                texts(view.tree())
            );
            // The window counts each second: the number on screen follows the clock.
            let every = of_kind(view.tree(), "ui.Timer")
                .iter()
                .find(|node| match node.props.get("every") {
                    Some(Value::Float(seconds)) => (*seconds - 1.0).abs() < 1e-9,
                    Some(Value::Int(seconds)) => *seconds == 1,
                    _ => false,
                })
                .map(|node| node.key.clone())
                .expect("the window counts every second");
            c.set_clock("2026-11-24T09:00:01Z");
            view.fire(&every, "tick").unwrap();
            view.poll_patches().unwrap();
            assert!(
                texts(view.tree()).iter().any(|text| text == "2"),
                "a second later the countdown reads two: {:?}",
                texts(view.tree())
            );
            // The shutter's own label offers the cancel the countdown names.
            assert!(
                buttons(view.tree()).iter().any(|label| label == "Cancel capture timer"),
                "the shutter cancels the timer: {:?}",
                buttons(view.tree())
            );
        }
    }
}
