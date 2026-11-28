//! `file.save(a, name: n, then: saved -> exported(p, saved))`: a save answers the app that asked
//! it what it came to — where the file was written, that the person closed the chooser, or why it
//! was refused — through the device's one save path.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, UiNode, Value, ViewInstance};

    const SOURCE: &str = r##"app so.tilo.saves "Saves"

type Saves {
    at: Text
    cancelled: Bool
    refused: Text
    asked: Bool
}

root saves: Saves

default view window for Saves

action export(s: Saves) -> File (label: "Export") {
    File.of_text("notes.md", "# notes")
}

action exported(s: Saves, saved: host.Saved) {
    s.at = saved.at
    s.cancelled = saved.cancelled
    s.refused = saved.refused
    s.asked = true
}

/// Saves without a handler: the window hears nothing, as an export that keeps no words does.
action quiet(s: Saves) -> File (label: "Quiet") {
    File.of_text("quiet.md", "quiet")
}

view window(s: Saves) {
    stack {
        ui.Button("Export") {
            on press: file.save(export(s), name: "notes.md", then: saved -> exported(s, saved))
        }
        ui.Button("Quiet") {
            on press: file.save(quiet(s), name: "quiet.md")
        }
    }
}
"##;

    /// The app on a device whose files answer what its next save says: written, cancelled, or
    /// refused.
    struct Bench {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        files: Files,
    }

    impl Bench {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let files = Files::default();
            rt.set_blobs(files.clone());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            let root = rt.create_root("Saves").unwrap();
            Self {
                rt,
                app,
                root,
                files,
            }
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("window"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap_or_else(|error| panic!("window: {error}"));
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// Press the window's button `label`, as a person does, and let the view catch up.
        fn press(window: &mut ViewInstance, label: &str) {
            let button = of_kind(window.tree(), "ui.Button")
                .into_iter()
                .find(|node| node.props.get("label").and_then(Value::as_text) == Some(label))
                .unwrap_or_else(|| panic!("the {label:?} button"));
            window
                .fire(&button.key, "press")
                .unwrap_or_else(|error| panic!("{label}: {error:?}"));
            window.poll_patches().unwrap();
        }

        fn text(&self, field: &str) -> String {
            match self.rt.read(&self.root, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, field: &str) -> bool {
            match self.rt.read(&self.root, field).unwrap() {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads {other:?}"),
            }
        }
    }

    fn prop(node: &UiNode, name: &str) -> String {
        node.props
            .get(name)
            .and_then(Value::as_text)
            .map(str::to_owned)
            .unwrap_or_default()
    }

    #[test]
    fn a_save_hands_the_app_where_it_went() {
        let bench = Bench::new();
        let mut window = bench.window();
        Bench::press(&mut window, "Export");
        assert!(bench.flag("asked"), "the handler ran");
        assert_eq!(bench.text("at"), "notes.md", "the place the device wrote");
        assert!(!bench.flag("cancelled"));
        assert_eq!(bench.text("refused"), "");
        assert_eq!(
            bench.files.save_asks(),
            vec![("notes.md".to_owned(), b"# notes".to_vec())],
            "the save wrote what the action made"
        );
    }

    #[test]
    fn a_closed_chooser_is_said_as_cancelled() {
        let bench = Bench::new();
        let mut window = bench.window();
        bench.files.cancel_next_save();
        Bench::press(&mut window, "Export");
        assert!(bench.flag("asked"));
        assert!(bench.flag("cancelled"), "the person closed the chooser");
        assert_eq!(bench.text("at"), "", "nothing was written");
        assert_eq!(bench.text("refused"), "");
        assert_eq!(
            bench.files.save_asks().len(),
            1,
            "the save was asked once and the chooser closed it"
        );
    }

    #[test]
    fn a_refused_save_hands_its_reason() {
        let bench = Bench::new();
        let mut window = bench.window();
        bench.files.refuse_next_save("the disk is full");
        Bench::press(&mut window, "Export");
        assert!(bench.flag("asked"));
        assert_eq!(bench.text("refused"), "the disk is full");
        assert!(!bench.flag("cancelled"));
        assert_eq!(bench.text("at"), "");
    }

    #[test]
    fn a_save_without_a_handler_writes_and_says_nothing() {
        let bench = Bench::new();
        let mut window = bench.window();
        Bench::press(&mut window, "Quiet");
        assert!(!bench.flag("asked"), "no handler ran");
        assert_eq!(
            bench.files.saved("quiet.md").as_deref(),
            Some(b"quiet".as_slice())
        );
    }

    #[test]
    fn a_refused_save_without_a_handler_is_the_call_s_failure() {
        let bench = Bench::new();
        let window = bench.window();
        bench.files.refuse_next_save("the disk is full");
        let button = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .find(|node| prop(node, "label") == "Quiet")
            .expect("the Quiet button")
            .key
            .clone();
        assert!(
            window.fire(&button, "press").is_err(),
            "a save with no handler and no write is refused, as before"
        );
    }
}
