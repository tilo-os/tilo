//! The Disk Usage Analyzer (`apps/tl/disk-usage/app.tl`) end to end: a folder measured through
//! the device's engine, its folders on the donut and in the list, walking into them and back,
//! the chooser, and a folder that cannot be read.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::files::{Entry, FilesHost};
    use tilolang_runtime::host_work::{EngineHost, JobHandlers};
    use tilolang_runtime::value::ResourceRef;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/disk-usage/app.tl");
    const RUNNING: &str = r#"{"state":"running","path":"/home/you","bytes":42000000000,"files":1430,"total":0,"unreadable":0,"error":""}"#;
    const DONE: &str = r#"{"state":"done","path":"/home/you","bytes":210300000000,"files":92127,"total":210300000000,"unreadable":3,"error":""}"#;
    const CANCELLED: &str = r#"{"state":"cancelled","path":"/home/you","bytes":42000000000,"files":1430,"total":0,"unreadable":0,"error":""}"#;

    /// The device's engine: what it was asked, how the scan it runs stands now, the folders a
    /// finished scan lists, and the actions the start named as the scan moves and ends.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        state: Rc<RefCell<String>>,
        entries: Rc<RefCell<String>>,
        cancelled: Rc<RefCell<Vec<String>>>,
        handlers: Rc<RefCell<Vec<JobHandlers>>>,
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            if op == "entries" {
                Ok(self.entries.borrow().clone())
            } else {
                Err(format!("`{op}` is no op this device answers now"))
            }
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            handlers: &JobHandlers,
        ) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            if op == "scan" {
                self.handlers.borrow_mut().push(handlers.clone());
                Ok("scan-1".to_owned())
            } else {
                Err(format!("`{op}` is no op this device runs"))
            }
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            let said = || -> Result<String, String> {
                let answer = self.state.borrow().clone();
                if job != "scan-1" || answer.is_empty() {
                    return Err(format!("no job `{job}` runs on this device"));
                }
                Ok(answer)
            };
            Ok(said()
                .ok()
                .and_then(|said| tilolang_tilo::engine::record_of(job, &said)))
        }

        fn cancel(&self, job: &str) -> bool {
            self.cancelled.borrow_mut().push(job.to_owned());
            job == "scan-1"
        }
    }

    /// This device's folders: two levels of them, one folder that refuses, and nothing else.
    struct Disk;

    impl Disk {
        fn entry(name: &str, folder: bool) -> Entry {
            Entry {
                name: name.to_owned(),
                path: format!("/home/you/{name}"),
                folder,
                size: 0,
                modified: 0,
                hidden: name.starts_with('.'),
            }
        }
    }

    impl FilesHost for Disk {
        fn list(&self, path: &str) -> Result<Vec<Entry>, String> {
            match path {
                "~" | "/home/you" => Ok(vec![
                    Self::entry("Projects", true),
                    Self::entry("Videos", true),
                    Self::entry("notes.md", false),
                ]),
                "/home/you/Projects" => Ok(vec![Self::entry("tilo", true)]),
                other => Err(format!("{other}: it is not granted")),
            }
        }

        fn stat(&self, path: &str) -> Result<Option<Entry>, String> {
            Ok(Some(Self::entry(path, true)))
        }

        fn read(&self, _path: &str) -> Result<Vec<u8>, String> {
            Err("no file".to_owned())
        }

        fn watch(&self, _path: &str, _run: &str) -> Result<String, String> {
            Ok("watch-1".to_owned())
        }
    }

    /// The app on a device: its window, its engine, and the folders it may read.
    struct Usage {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        engine: Engine,
    }

    impl Usage {
        fn new() -> Self {
            let engine = Engine::default();
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_engine(engine.clone());
            rt.set_device_files(Disk);
            // The session's own package, which Open Disks calls; nothing of it is read here.
            let session =
                tilolang_tilo::platform_apps::session_package().expect("the session's package");
            rt.load_foreign(session.clone())
                .expect("the session's package loads");
            let app = App::compile(SOURCE, &[session]);
            app.load(&rt);
            let root = rt.create_root("Analyzer").unwrap();
            Self {
                rt,
                app,
                root,
                engine,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn act(&self, action: &str, args: Vec<Value>) {
            self.call(action, args);
        }

        /// The device runs what the scan's start named, now that it stands as its engine says of
        /// it: the handler it kept, filled with the job's record.
        fn moves(&self, handler: fn(&JobHandlers) -> Option<serde_json::Value>) {
            let kept = {
                let kept = self.engine.handlers.borrow();
                handler(kept.last().expect("a start came before its job moved"))
                    .expect("the scan's start named this handler")
            };
            let said = self.engine.state.borrow().clone();
            let record = tilolang_tilo::engine::record_of("scan-1", &said).expect("a job record");
            let call = self
                .rt
                .job_call(&kept, &record)
                .expect("the handler's call");
            self.rt
                .invoke_action(&me(), call)
                .expect("the handler runs");
        }

        /// The scan moved on, as far as the engine's last word says.
        fn moved(&self) {
            self.moves(|handlers| handlers.progress.clone());
        }

        /// The scan ended, as the engine's last word says.
        fn ended(&self) {
            self.moves(|handlers| handlers.done.clone());
        }

        fn text(&self, field: &str) -> String {
            match self.rt.read(&self.root, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} is text, not {other:?}"),
            }
        }

        fn int(&self, field: &str) -> i64 {
            match self.rt.read(&self.root, field).unwrap() {
                Value::Int(n) => n,
                other => panic!("{field} is a number, not {other:?}"),
            }
        }

        fn view(&self) -> ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("analyzer"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap()
        }

        /// The folders the app keeps in `field`, with `named` of each.
        fn kept(&self, field: &str, named: &str) -> Vec<String> {
            let Value::List(items) = self.rt.read(&self.root, field).unwrap() else {
                panic!("{field} is a list");
            };
            items
                .iter()
                .map(|held| {
                    let Value::Ref(r) = held else {
                        panic!("{field} holds folders, not {held:?}")
                    };
                    match self.rt.read(&r.target, named).unwrap() {
                        Value::Text(text) => text,
                        other => panic!("{named} is text, not {other:?}"),
                    }
                })
                .collect()
        }
    }

    /// Whether any prop of any node is the text `wanted`.
    fn any_prop_text(tree: &UiNode, wanted: &str) -> bool {
        tree.props
            .values()
            .any(|value| matches!(value, Value::Text(text) if text.contains(wanted)))
            || tree
                .children
                .iter()
                .any(|child| any_prop_text(child, wanted))
    }

    /// What a finished scan of the home folder lists, largest first: ten folders.
    fn done_entries() -> String {
        let entries: [(&str, i64, i64); 10] = [
            ("Projects", 84_200_000_000, 48_310),
            ("Videos", 48_600_000_000, 1_204),
            ("Photos", 31_800_000_000, 9_870),
            ("Downloads", 19_400_000_000, 2_615),
            ("Documents", 8_700_000_000, 4_132),
            ("Music", 5_100_000_000, 1_086),
            (".cache", 3_900_000_000, 12_408),
            (".local", 3_400_000_000, 6_121),
            ("Desktop", 2_600_000_000, 214),
            ("Files in this folder", 1_200_000_000, 156),
        ];
        let entries = entries
        .iter()
        .map(|(name, bytes, files)| {
            format!(r#"{{"name":"{name}","path":"/home/you/{name}","bytes":{bytes},"files":{files}}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
        format!("[{entries}]")
    }

    tilo_test_env::scenario! {
        fn a_folder_is_measured_walked_and_remembered;

        phase opening_a_place_measures_it_as_a_job;
        {
            let t = Usage::new();
            t.act("open_place", vec![Value::resource(&t.root), Value::text("/home/you")]);
            assert_eq!(t.text("folder"), "/home/you");
            assert_eq!(t.text("label"), "you");
            assert_eq!(t.text("root"), "/home/you", "the place starts the chain");
            assert_eq!(t.text("job"), "scan-1", "the measuring runs as a job on the device");
            assert_eq!(
                t.engine.asked.borrow().as_slice(),
                [("scan".to_owned(), r#"{"path":"/home/you"}"#.to_owned())],
                "the device is asked for the folder, as the app names it"
            );
        }

        phase the_job_s_moves_write_the_folders_largest_first_and_the_rest_as_one;
        {
            let t = Usage::new();
            *t.engine.state.borrow_mut() = RUNNING.to_owned();
            t.act("open_place", vec![Value::resource(&t.root), Value::text("/home/you")]);
            t.moved();
            assert_eq!(t.text("job"), "scan-1", "a measuring still runs");
            assert_eq!(t.int("seen_bytes"), 42_000_000_000);
            assert_eq!(t.int("seen_files"), 1430);
            assert!(t.kept("entries", "name").is_empty(), "nothing shows until it ends");
            *t.engine.state.borrow_mut() = DONE.to_owned();
            *t.engine.entries.borrow_mut() = done_entries();
            t.ended();
            assert_eq!(t.text("job"), "", "the measuring ended");
            assert_eq!(t.int("total"), 210_300_000_000);
            assert_eq!(t.int("files"), 92_127);
            assert_eq!(t.int("unreadable"), 3);
            let kept = t.kept("entries", "name");
            assert_eq!(kept.len(), 9, "the eight largest folders and the rest: {kept:?}");
            assert_eq!(kept.first().map(String::as_str), Some("Projects"));
            assert_eq!(kept.last().map(String::as_str), Some("Other folders"));
            assert_eq!(t.kept("recents", "path"), ["/home/you"], "the folder is remembered");
        }

        phase walking_into_a_folder_keeps_the_way_back;
        {
            let t = Usage::new();
            *t.engine.state.borrow_mut() = DONE.to_owned();
            *t.engine.entries.borrow_mut() = done_entries();
            t.act("open_place", vec![Value::resource(&t.root), Value::text("/home/you")]);
            t.ended();
            let Value::List(items) = t.rt.read(&t.root, "entries").unwrap() else {
                panic!("entries are a list");
            };
            let Some(Value::Ref(projects)) = items.first() else {
                panic!("a folder stands first");
            };
            let projects = (*projects.target).clone();
            t.act("drill", vec![Value::resource(&t.root), Value::resource(&projects)]);
            assert_eq!(t.text("folder"), "/home/you/Projects");
            assert_eq!(t.text("root"), "/home/you", "the chain still starts at the place");
            t.act("back", vec![Value::resource(&t.root)]);
            assert_eq!(t.text("folder"), "/home/you", "back one folder");
            t.act("back", vec![Value::resource(&t.root)]);
            assert_eq!(t.text("folder"), "", "back from the place shows it again");
        }

        phase stopping_a_measuring_cancels_the_device_job;
        {
            let t = Usage::new();
            *t.engine.state.borrow_mut() = RUNNING.to_owned();
            t.act("open_place", vec![Value::resource(&t.root), Value::text("/home/you")]);
            t.act("stop", vec![Value::resource(&t.root)]);
            assert_eq!(t.engine.cancelled.borrow().as_slice(), ["scan-1".to_owned()]);
            *t.engine.state.borrow_mut() = CANCELLED.to_owned();
            t.ended();
            assert_eq!(t.text("job"), "");
            assert!(t.text("notice").contains("Stopped"), "the window says it stopped");
        }

        phase a_folder_that_cannot_be_read_says_why;
        {
            let t = Usage::new();
            *t.engine.state.borrow_mut() =
                r#"{"state":"failed","path":"/secret","bytes":0,"files":0,"total":0,"unreadable":1,"error":"it is not granted"}"#.to_owned();
            t.act("analyze", vec![
                Value::resource(&t.root),
                Value::text("/secret"),
                Value::text("secret"),
            ]);
            t.ended();
            assert_eq!(t.text("trouble"), "it is not granted");
            assert_eq!(t.text("job"), "");
        }

        phase the_chooser_walks_the_folders_and_measures_the_one_it_shows;
        {
            let t = Usage::new();
            t.act("choose", vec![Value::resource(&t.root)]);
            assert_eq!(t.text("picker"), "~");
            assert_eq!(t.kept("picks", "name"), ["Projects", "Videos"], "the home folders");
            t.act("go", vec![Value::resource(&t.root), Value::text("/home/you/Projects")]);
            t.act("pick", vec![Value::resource(&t.root)]);
            assert_eq!(t.text("folder"), "/home/you/Projects");
            assert!(
                !matches!(t.rt.read(&t.root, "choosing").unwrap(), Value::Bool(true)),
                "the chooser closed"
            );
        }

        phase the_window_shows_the_places_the_donut_and_the_folders;
        {
            let t = Usage::new();
            let mut view = t.view();
            assert!(!of_kind(view.tree(), "ui.NavShell").is_empty(), "the window is the app's shell");
            assert!(any_prop_text(view.tree(), "Home"), "the home place is offered");
            assert!(any_prop_text(view.tree(), "File system"), "the file system is offered");
            *t.engine.state.borrow_mut() = DONE.to_owned();
            *t.engine.entries.borrow_mut() = done_entries();
            t.act("open_place", vec![Value::resource(&t.root), Value::text("/home/you")]);
            t.ended();
            view.poll_patches().unwrap();
            assert!(!of_kind(view.tree(), "ui.PieChart").is_empty(), "the folders are a donut");
            assert!(any_prop_text(view.tree(), "84.2 GB"), "a folder says what it takes");
            assert!(any_prop_text(view.tree(), "Other folders"), "the rest is one part");
            assert!(
                any_prop_text(view.tree(), "3 items couldn’t be read"),
                "what could not be read is said"
            );
        }

        phase the_window_says_what_it_is_measuring_and_offers_stop;
        {
            let t = Usage::new();
            *t.engine.state.borrow_mut() = RUNNING.to_owned();
            t.act("open_place", vec![Value::resource(&t.root), Value::text("/home/you")]);
            t.moved();
            let view = t.view();
            assert!(any_prop_text(view.tree(), "Measuring"), "the window says so");
            assert!(any_prop_text(view.tree(), "42 GB"), "how far it got is said");
            assert!(any_prop_text(view.tree(), "Stop"), "and the way to stop it");
        }
    }
}
