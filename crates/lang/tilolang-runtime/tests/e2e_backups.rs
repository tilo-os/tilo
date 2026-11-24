//! The Backups app (`apps/tl/backups/app.tl`) end to end: a folder and a destination chosen in
//! the picker, a backup the device's engine runs while the window watches, the snapshots it
//! lists with their file index, a verification and a restore, a run stopped before it finishes,
//! a failed one that says why, and the retention policy's bounds.

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

    const SOURCE: &str = include_str!("../../../../apps/tl/backups/app.tl");

    /// What the device asked for, in the words the app wrote, and the actions a job's start
    /// named as it moves and ends.
    #[derive(Clone, Default)]
    struct Engine {
        calls: Rc<RefCell<Vec<(String, String)>>>,
        /// How the job stands now: `running` until the test says otherwise.
        state: Rc<RefCell<String>>,
        /// What a finished job answers with.
        answer: Rc<RefCell<String>>,
        cancelled: Rc<RefCell<Vec<String>>>,
        handlers: Rc<RefCell<Vec<JobHandlers>>>,
    }

    impl Engine {
        fn asked(&self) -> Vec<(String, String)> {
            self.calls.borrow().clone()
        }

        /// How the job stands, as the engine that runs it says it now.
        fn said(&self) -> String {
            let state = self.state.borrow().clone();
            let (answer, error) = match state.as_str() {
                "done" => (self.answer.borrow().clone(), String::new()),
                "failed" => (String::new(), "the drive went away".to_owned()),
                "cancelled" => (String::new(), "The work was stopped.".to_owned()),
                _ => (String::new(), String::new()),
            };
            serde_json::json!({
                "state": state,
                "progress": "1430 of 9218 files, 42.0 GB",
                "answer": answer,
                "error": error,
            })
            .to_string()
        }

        fn last_input(&self) -> serde_json::Value {
            let text = self
                .calls
                .borrow()
                .last()
                .map(|(_, input)| input.clone())
                .unwrap_or_default();
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
        }

        fn done_with(&self, answer: &str) {
            *self.answer.borrow_mut() = answer.to_owned();
            *self.state.borrow_mut() = "done".to_owned();
        }

        fn failed_with(&self) {
            *self.state.borrow_mut() = "failed".to_owned();
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            if op == "catalog" {
                Ok(catalog_json())
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
            self.calls
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                "backup" | "verify" | "restore" => {
                    self.handlers.borrow_mut().push(handlers.clone());
                    Ok(format!("job-{op}"))
                }
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            if !job.starts_with("job-") {
                return Err(format!("no job `{job}` runs on this device"));
            }
            Ok(tilolang_tilo::engine::record_of(job, &self.said()))
        }

        fn cancel(&self, job: &str) -> bool {
            self.cancelled.borrow_mut().push(job.to_owned());
            job.starts_with("job-")
        }
    }

    /// This device's folders: a home holding Documents, Projects and a backup drive.
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
                    Self::entry("Documents", true),
                    Self::entry("Projects", true),
                    Self::entry("backup-drive", true),
                ]),
                _ => Ok(Vec::new()),
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

    /// The repository as the device answers it: one source, one destination, two snapshots.
    fn catalog_json() -> String {
        serde_json::json!({
            "sources": [{
                "id": "local-source",
                "name": "Documents",
                "path": "/home/you/Documents",
                "kind": "custom",
                "schedule": "on_demand",
                "enabled": true,
                "health": "unknown",
            }],
            "destinations": [{
                "id": "local-destination",
                "name": "backup-drive",
                "kind": "local_folder",
                "used_bytes": 312000000000_i64,
                "total_bytes": 500000000000_i64,
                "encrypted": false,
                "health": "unknown",
            }],
            "snapshots": [
                {
                    "id": "1795080000",
                    "source_id": "local-source",
                    "dest_id": "local-destination",
                    "taken_unix_seconds": 1795080000_i64,
                    "label": "",
                    "kind": "unknown",
                    "retention": "unknown",
                    "size_bytes": 9700000000_i64,
                    "delta_bytes": 9700000000_i64,
                    "files_changed": 312,
                    "status": "complete",
                    "failure": "",
                    "files": [
                        { "depth": 0, "name": "Documents", "size": 0, "is_dir": true },
                        { "depth": 1, "name": "plan.md", "size": 4200, "is_dir": false },
                    ],
                },
                {
                    "id": "1795076400",
                    "source_id": "local-source",
                    "dest_id": "local-destination",
                    "taken_unix_seconds": 1795076400_i64,
                    "label": "Before the upgrade",
                    "kind": "manual",
                    "retention": "kept",
                    "size_bytes": 9500000000_i64,
                    "delta_bytes": 180000000_i64,
                    "files_changed": 211,
                    "status": "complete",
                    "failure": "",
                    "files": [],
                },
            ],
        })
        .to_string()
    }

    struct Backups {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        engine: Engine,
    }

    impl Backups {
        fn new() -> Self {
            let engine = Engine::default();
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_engine(engine.clone());
            rt.set_device_files(Disk);
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            // What this device protects is the device's own: its root stands under the device.
            let ty = app
                .meta()
                .types
                .iter()
                .find(|declared| declared.name == "Backups")
                .expect("the backups type")
                .id
                .clone();
            let root = rt
                .create_root_at(&format!("devices/local/{}/root-{ty}", app.meta().app), &ty)
                .expect("this device's backups");
            Self {
                rt,
                app,
                root,
                engine,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn done(&self, action: &str, args: Vec<Value>) -> Value {
            match self.call(action, args) {
                InvokeOutcome::Done(value) => value,
                other => panic!("{action}: {other:?}"),
            }
        }

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.rt.read(r, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn number(&self, r: &ResourceRef, field: &str) -> i64 {
            match self.rt.read(r, field).unwrap() {
                Value::Int(n) => n,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn held(&self, r: &ResourceRef, field: &str) -> ResourceRef {
            match self.rt.read(r, field).unwrap() {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn records(&self, r: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(r, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
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
                .unwrap_or_else(|e| panic!("window: {e}"));
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// Chooses the folder this device protects, as a person walks the picker.
        fn choose_source(&self, folder: &str) {
            s_done(self, "choose_source", vec![Value::resource(&self.root)]);
            s_done(
                self,
                "go",
                vec![Value::resource(&self.root), Value::text(folder)],
            );
            s_done(self, "pick", vec![Value::resource(&self.root)]);
        }

        fn choose_destination(&self, folder: &str) {
            s_done(
                self,
                "choose_destination",
                vec![Value::resource(&self.root)],
            );
            s_done(
                self,
                "go",
                vec![Value::resource(&self.root), Value::text(folder)],
            );
            s_done(self, "pick", vec![Value::resource(&self.root)]);
        }

        fn set_up(&self) {
            self.choose_source("/home/you/Documents");
            self.choose_destination("/home/you/backup-drive");
        }

        /// The device runs what the running job's start named, as the job stands now: the
        /// handler it kept, filled with the record of the job the app holds.
        fn moves(&self, handler: fn(&JobHandlers) -> Option<serde_json::Value>) {
            let kept = {
                let kept = self.engine.handlers.borrow();
                handler(kept.last().expect("a start came before its job moved"))
                    .expect("the work's start named this handler")
            };
            let id = self.text(&self.root, "job");
            let record =
                tilolang_tilo::engine::record_of(&id, &self.engine.said()).expect("a job record");
            let call = self
                .rt
                .job_call(&kept, &record)
                .expect("the handler's call");
            self.rt
                .invoke_action(&me(), call)
                .expect("the handler runs");
        }

        /// The work moved on, as far as the engine says.
        fn moved(&self) {
            self.moves(|handlers| handlers.progress.clone());
        }

        /// The work ended, as the engine says it did.
        fn ended(&self) {
            self.moves(|handlers| handlers.done.clone());
        }
    }

    fn s_done(s: &Backups, action: &str, args: Vec<Value>) {
        s.done(action, args);
    }

    /// Every node of a component, by its kind.
    fn of_kind<'a>(tree: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
        let mut found = Vec::new();
        fn walk<'a>(node: &'a UiNode, kind: &str, found: &mut Vec<&'a UiNode>) {
            if node.kind == kind {
                found.push(node);
            }
            for child in &node.children {
                walk(child, kind, found);
            }
        }
        walk(tree, kind, &mut found);
        found
    }

    fn prop(node: &UiNode, name: &str) -> String {
        node.props
            .get(name)
            .map(|value| match value {
                Value::Text(text) => text.clone(),
                other => format!("{other:?}"),
            })
            .unwrap_or_default()
    }

    fn titles(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.ListRow")
            .iter()
            .map(|row| prop(row, "title"))
            .collect()
    }

    fn labels(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Button")
            .iter()
            .map(|b| prop(b, "label"))
            .collect()
    }

    #[test]
    fn a_folder_is_chosen_backed_up_and_its_snapshots_listed() {
        let s = Backups::new();
        s.set_up();
        assert_eq!(s.text(&s.root, "source"), "/home/you/Documents");
        assert_eq!(s.text(&s.root, "destination"), "/home/you/backup-drive");

        // A backup starts on the device and says how far it is.
        s.done("back_up_now", vec![Value::resource(&s.root)]);
        assert_eq!(s.text(&s.root, "working"), "backup");
        let ask = s.engine.last_input();
        assert_eq!(ask["source"], "/home/you/Documents");
        assert_eq!(ask["destination"], "/home/you/backup-drive");
        assert_eq!(ask["policy_hourly"], 24, "the default policy is sent");

        // It finishes with the repository as it now stands.
        s.engine.done_with(&catalog_json());
        s.ended();
        assert_eq!(s.text(&s.root, "job"), "", "no job runs any more");
        let snapshots = s.records(&s.root, "snapshots");
        assert_eq!(snapshots.len(), 2);
        assert_eq!(s.number(&s.root, "used_bytes"), 312_000_000_000);
        assert_eq!(s.text(&s.root, "source_name"), "Documents");
        assert_eq!(
            s.number(&s.root, "subject"),
            1_795_080_000,
            "the newest snapshot is the one the window shows"
        );
        assert_eq!(s.records(&snapshots[0], "files").len(), 2);
        assert_eq!(s.number(&snapshots[0], "size_bytes"), 9_700_000_000);
        assert_eq!(s.text(&s.root, "notice"), "The folder is backed up.");

        // The window lists the snapshots and answers for the folder it protects.
        let window = s.window();
        let rows = titles(window.tree());
        assert!(
            rows.iter().any(|row| row.contains("Before the upgrade")),
            "a named snapshot keeps its name: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("plan.md")),
            "the shown snapshot's files are in the window: {rows:?}"
        );
        assert!(
            labels(window.tree())
                .iter()
                .any(|label| label == "Back up now"),
            "the window offers a backup"
        );
    }

    #[test]
    fn a_snapshot_is_verified_and_restored_into_a_chosen_folder() {
        let s = Backups::new();
        s.set_up();
        s.done("back_up_now", vec![Value::resource(&s.root)]);
        s.engine.done_with(&catalog_json());
        s.ended();
        let snapshots = s.records(&s.root, "snapshots");

        // A verification reads the snapshot and reports what it checked.
        let snapshot = &snapshots[1];
        s.done(
            "verify",
            vec![Value::resource(&s.root), Value::resource(snapshot)],
        );
        assert_eq!(s.text(&s.root, "working"), "verify");
        let ask = s.engine.last_input();
        assert_eq!(ask["snapshot"], "1795076400");
        assert_eq!(ask["destination"], "/home/you/backup-drive");
        s.engine
            .done_with(r#"{"files": 1840, "bytes": 48200000000}"#);
        s.ended();
        assert!(s.text(&s.root, "result").contains("1840 files"));
        assert_eq!(s.text(&s.root, "notice"), "The snapshot checks out.");

        // A restore asks for a folder first, then writes the snapshot there.
        s.done(
            "ask_restore",
            vec![Value::resource(&s.root), Value::resource(snapshot)],
        );
        assert_eq!(s.text(&s.root, "picking"), "restore");
        s.done(
            "go",
            vec![Value::resource(&s.root), Value::text("/home/you/restored")],
        );
        s.done("pick", vec![Value::resource(&s.root)]);
        assert_eq!(s.text(&s.root, "restore_into"), "/home/you/restored");
        assert_eq!(s.text(&s.root, "picking"), "");

        s.done(
            "restore_now",
            vec![Value::resource(&s.root), Value::resource(snapshot)],
        );
        assert_eq!(s.text(&s.root, "working"), "restore");
        let ask = s.engine.last_input();
        assert_eq!(ask["target"], "/home/you/restored");
        assert_eq!(ask["snapshot"], "1795076400");
        assert_eq!(ask["overwrite"], false);
        s.engine
            .done_with(r#"{"files": 1840, "bytes": 48200000000}"#);
        s.ended();
        assert_eq!(s.text(&s.root, "notice"), "The snapshot is back.");
    }

    #[test]
    fn a_run_is_followed_while_it_runs_and_can_be_stopped() {
        let s = Backups::new();
        s.set_up();
        s.done("back_up_now", vec![Value::resource(&s.root)]);
        s.moved();
        assert_eq!(
            s.text(&s.root, "progress"),
            "1430 of 9218 files, 42.0 GB",
            "the work's line reaches the window"
        );
        s.done("stop_work", vec![Value::resource(&s.root)]);
        assert_eq!(s.engine.cancelled.borrow().len(), 1);
        assert_eq!(s.text(&s.root, "job"), "");
        assert!(s.text(&s.root, "notice").contains("stopped"));
    }

    #[test]
    fn a_failed_run_says_why_and_leaves_no_job() {
        let s = Backups::new();
        s.set_up();
        s.done("back_up_now", vec![Value::resource(&s.root)]);
        s.engine.failed_with();
        s.ended();
        assert_eq!(s.text(&s.root, "failure"), "the drive went away");
        assert_eq!(s.text(&s.root, "working"), "");
        assert_eq!(s.text(&s.root, "job"), "");
    }

    #[test]
    fn the_retention_policy_counts_stay_inside_their_bounds() {
        let s = Backups::new();
        s.set_up();
        // The policy is kept whole in the root, with the counts a person expects.
        let policy = s.held(&s.root, "policy");
        assert_eq!(s.number(&policy, "hourly"), 24);
        assert_eq!(s.number(&policy, "daily"), 7);
        assert_eq!(s.number(&policy, "weekly"), 4);
        assert_eq!(s.number(&policy, "monthly"), 12);

        s.done(
            "keep_more",
            vec![Value::resource(&s.root), Value::text("hourly")],
        );
        assert_eq!(s.number(&policy, "hourly"), 25);
        for _ in 0..2 {
            s.done(
                "keep_fewer",
                vec![Value::resource(&s.root), Value::text("hourly")],
            );
        }
        assert_eq!(s.number(&policy, "hourly"), 23);

        // None of the four counts goes below zero, however often one is lowered.
        for _ in 0..30 {
            s.done(
                "keep_fewer",
                vec![Value::resource(&s.root), Value::text("weekly")],
            );
        }
        assert_eq!(s.number(&policy, "weekly"), 0);

        s.done(
            "show_place",
            vec![Value::resource(&s.root), Value::text("retention")],
        );
        let window = s.window();
        let headers: Vec<String> = of_kind(window.tree(), "ui.ObjectHeader")
            .iter()
            .map(|header| prop(header, "title"))
            .collect();
        assert!(
            headers.iter().any(|title| title == "Retention"),
            "the retention page is the one shown: {headers:?}"
        );
        let facts: Vec<String> = of_kind(window.tree(), "ui.Fact")
            .iter()
            .map(|fact| prop(fact, "text"))
            .collect();
        assert!(
            facts.iter().any(|fact| fact.contains("kept at most")),
            "the retention page says how many are kept: {facts:?}"
        );
    }

    #[test]
    fn a_destination_inside_the_folder_it_copies_is_refused() {
        let s = Backups::new();
        s.done("choose_source", vec![Value::resource(&s.root)]);
        s.done(
            "go",
            vec![Value::resource(&s.root), Value::text("/home/you")],
        );
        s.done("pick", vec![Value::resource(&s.root)]);
        // The folder being protected is the home folder; its own child cannot hold the snapshots.
        s.done("choose_destination", vec![Value::resource(&s.root)]);
        s.done(
            "go",
            vec![Value::resource(&s.root), Value::text("/home/you/Documents")],
        );
        s.done("pick", vec![Value::resource(&s.root)]);
        assert_eq!(
            s.text(&s.root, "destination"),
            "",
            "the destination did not change"
        );
        assert!(
            s.text(&s.root, "trouble").contains("outside"),
            "{}",
            s.text(&s.root, "trouble")
        );
    }

    #[test]
    fn the_window_offers_the_places_the_setup_and_the_picker() {
        let s = Backups::new();
        // A first run shows the setup, not an empty timeline.
        let window = s.window();
        assert!(
            labels(window.tree())
                .iter()
                .any(|label| label.contains("Choose the folder to protect")),
            "the setup offers the chooser: {:?}",
            labels(window.tree())
        );

        s.set_up();
        let window = s.window();
        let places: Vec<String> = of_kind(window.tree(), "ui.NavItem")
            .iter()
            .map(|item| prop(item, "label"))
            .collect();
        assert!(
            places.iter().any(|place| place == "This device"),
            "the sidebar names this device: {places:?}"
        );

        // The picker walks the device's folders.
        s.done("choose_source", vec![Value::resource(&s.root)]);
        let window = s.window();
        let rows = titles(window.tree());
        assert!(
            rows.iter().any(|row| row == "Projects"),
            "the picker lists the folders: {rows:?}"
        );
    }
}
