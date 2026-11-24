//! The App Store (`apps/tl/store`) end to end: the sources it reads, the changes it starts, the
//! reports it keeps and the window that shows them. The device's work is a fake engine that
//! answers as the session's does, so what the app asks and what it does with the answer are both
//! seen here.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/store/app.tl");
    const VIEWS: &str = include_str!("../../../../apps/tl/store/views.tl");

    /// The session app, as far as the Store needs it: opening an installed app.
    const SESSION_SOURCE: &str = r#"app so.tilo.session "Session"
action open_app(app_name: Text) {
}
"#;

    /// One Tilo app this workspace runs, with a newer version offered.
    const NOTES: &str = r#"{
      "identity": "so.tilo.notes", "name": "Notes", "summary": "Notes written together",
      "description": "Notes.", "developer": "Tilo", "license": "", "categories": ["Productivity"],
      "icon": "https://artwork.invalid/notes.png", "screenshots": [], "homepage": "https://tilo.sh",
      "source_code": "", "origin": "tilo", "registry": "Tilo main", "branch": "stable",
      "user_managed": true, "trust": "publisher_signed",
      "versions": ["{\"version\":\"0.2.0\",\"released\":\"\",\"root\":\"aa\",\"surface\":\"bb\",\"size\":44040192,\"runtime\":\"\",\"download\":\"\"}"],
      "available": "0.2.0", "installed": "0.1.0", "pinned": false, "fetched": "0.1.0",
      "runnable": true, "processors": "", "awaiting_developer": false, "featured": 0,
      "updated": "", "publisher": "cc", "digest": "dd", "bot": false,
      "bot_triggers": [], "bot_apps": [], "bot_connectors": [], "bot_appearance": false,
      "runtime": "", "updatable": true, "trouble": ""
    }"#;

    /// A Flathub app this computer cannot run.
    const SPOTIFY: &str = r#"{
      "identity": "com.spotify.Client", "name": "Spotify", "summary": "Music for everyone",
      "description": "Music.", "developer": "Spotify", "license": "proprietary",
      "categories": ["AudioVideo"], "icon": "https://artwork.invalid/spotify.png",
      "screenshots": [], "homepage": "", "source_code": "", "origin": "flathub",
      "registry": "Flathub", "branch": "stable", "user_managed": true, "trust": "flathub",
      "versions": [], "available": "1.2", "installed": "", "pinned": false, "fetched": "",
      "runnable": false, "processors": "x86_64", "awaiting_developer": false, "featured": 0,
      "updated": "", "publisher": "", "digest": "", "bot": false,
      "bot_triggers": [], "bot_apps": [], "bot_connectors": [], "bot_appearance": false,
      "runtime": "", "updatable": false,
      "trouble": "Not available for this computer: Spotify is only built for x86_64."
    }"#;

    /// A package a person handed the Store.
    const LOCAL: &str = r#"{
      "identity": "so.tilo.local", "name": "Local app", "summary": "", "description": "",
      "developer": "", "license": "", "categories": [], "icon": "", "screenshots": [],
      "homepage": "", "source_code": "", "origin": "local", "registry": "Local package",
      "branch": "stable", "user_managed": true, "trust": "attestor",
      "versions": ["{\"version\":\"0.1.0\",\"released\":\"\",\"root\":\"ee\",\"surface\":\"\",\"size\":10,\"runtime\":\"\",\"download\":\"\"}"],
      "available": "0.1.0", "installed": "", "pinned": false, "fetched": "",
      "runnable": true, "processors": "", "awaiting_developer": false, "featured": 0,
      "updated": "", "publisher": "ff", "digest": "", "bot": false,
      "bot_triggers": [], "bot_apps": [], "bot_connectors": [], "bot_appearance": false,
      "runtime": "", "updatable": false, "trouble": ""
    }"#;

    /// An app a configured repository offers.
    const EDITOR: &str = r#"{
      "identity": "org.example.Editor", "name": "Editor", "summary": "Edit documents",
      "description": "", "developer": "", "license": "", "categories": [],
      "icon": "", "screenshots": [], "homepage": "", "source_code": "", "origin": "remote",
      "registry": "flathub", "branch": "stable", "user_managed": true,
      "trust": "configured_remote",
      "versions": ["{\"version\":\"2.0\",\"released\":\"\",\"root\":\"\",\"surface\":\"\",\"size\":0,\"runtime\":\"\",\"download\":\"\"}"],
      "available": "2.0", "installed": "", "pinned": false, "fetched": "",
      "runnable": true, "processors": "", "awaiting_developer": false, "featured": 0,
      "updated": "", "publisher": "", "digest": "", "bot": false,
      "bot_triggers": [], "bot_apps": [], "bot_connectors": [], "bot_appearance": false,
      "runtime": "", "updatable": false, "trouble": ""
    }"#;

    fn catalogue() -> String {
        serde_json::json!({
            "items": [NOTES, SPOTIFY],
            "remotes": [],
            "status": "Store refreshed.", "trouble": "", "flatpak": true, "flatpak_trouble": "",
            "unanswered": [], "expired": []
        })
        .to_string()
    }

    fn one() -> String {
        serde_json::json!({ "item": NOTES }).to_string()
    }

    fn details() -> String {
        serde_json::json!({
            "description": "Notes you write alone or together.",
            "developer": "Tilo", "license": "MIT", "homepage": "https://tilo.sh",
            "source_code": "https://code.tilo.so/tilo/tilo", "size": 44040192,
            "screenshots": ["https://artwork.invalid/one.png"],
            "permissions": [r#"{"what":"network","broad":false}"#],
            "releases": [r#"{"version":"0.2.0","notes":"Faster search"}"#],
            "runtime": "tilo-runtime/2026.07", "runtime_registry": "Tilo main", "digest": "dd"
        })
        .to_string()
    }

    fn remotes() -> String {
        serde_json::json!({
            "remotes": [r#"{"name":"flathub","title":"Flathub","url":"https://dl.flathub.org/repo/","scope":"device","enabled":true,"trouble":""}"#],
            "flatpak": true, "trouble": ""
        })
        .to_string()
    }

    fn browse() -> String {
        serde_json::json!({ "items": [EDITOR] }).to_string()
    }

    fn review() -> String {
        serde_json::json!({ "item": LOCAL, "root": "ee", "trouble": "" }).to_string()
    }

    fn prepared() -> String {
        serde_json::json!({
            "operation": r#"{"kind":"file_report"}"#, "registry": "Tilo main", "trouble": ""
        })
        .to_string()
    }

    fn case() -> String {
        serde_json::json!({
            "case": "case-1", "status": "Quarantined", "situation": "", "trouble": ""
        })
        .to_string()
    }

    /// The device's engine: what it was asked, and the answers it gives.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String, String)>>>,
        /// The jobs started, by their id, and the state the next ask answers.
        done: Rc<RefCell<bool>>,
        /// Whether the sources answer at all.
        unreachable: Rc<RefCell<bool>>,
    }

    impl Engine {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(kind, op, _)| format!("{kind}:{op}"))
                .collect()
        }

        fn input_of(&self, op: &str) -> String {
            self.asked
                .borrow()
                .iter()
                .find(|(_, named, _)| named == op)
                .map(|(_, _, input)| input.clone())
                .unwrap_or_default()
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push(("call".to_owned(), op.to_owned(), input.to_owned()));
            if *self.unreachable.borrow() && op == "catalogue" {
                return Err("the sources are unreachable".to_owned());
            }
            match op {
                "catalogue" => Ok(catalogue()),
                "one" => Ok(one()),
                "details" => Ok(details()),
                "remotes" => Ok(remotes()),
                "browse" => Ok(browse()),
                "review" => Ok(review()),
                "reference" => Ok(review()),
                "runtime" => Ok(one()),
                "prepare-report" => Ok(prepared()),
                "report-status" => Ok(case()),
                "block" => Ok("{}".to_owned()),
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push(("start".to_owned(), op.to_owned(), input.to_owned()));
            match op {
                "catalogue" | "install" | "update" | "uninstall" | "pin" | "unpin"
                | "install-runtime" | "update-all" | "remote-add" | "remote-enable"
                | "remote-remove" => Ok(r#"{"job":"job-1","error":""}"#.to_owned()),
                other => Err(format!("`{other}` is no job this device runs")),
            }
        }

        fn status(&self, job: &str) -> Result<String, String> {
            if job != "job-1" {
                return Err(format!("no job `{job}`"));
            }
            if *self.done.borrow() {
                Ok(r#"{"state":"done","done":1,"total":1,"error":"","status":"Done."}"#.to_owned())
            } else {
                Ok(r#"{"state":"running","done":0,"total":1,"error":"","status":"Working."}"#
                    .to_owned())
            }
        }

        fn cancel(&self, job: &str) -> bool {
            job == "job-1"
        }
    }

    /// The device's files: what an action saved, and what a chosen file holds.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
            self.0
                .borrow_mut()
                .insert(request.name.clone(), content.to_vec());
            Ok(())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            self.0.borrow_mut().insert(name.to_owned(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                name.to_owned(),
            ))
        }
    }

    struct Market {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        engine: Engine,
    }

    impl Market {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let engine = Engine::default();
            // The device answers every job as done at once: what the app does with an ending is
            // this test's subject, not a wait.
            *engine.done.borrow_mut() = true;
            rt.set_engine(engine.clone());
            let device = Device::default();
            rt.set_blobs(device.clone());
            let session = App::compile(SESSION_SOURCE, &[]);
            session.load(&rt);
            let app = App::compile(&format!("{SOURCE}\n{VIEWS}"), &[session.meta().clone()]);
            app.load(&rt);
            let root = rt.create_root("Store").unwrap();
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

        fn call_root(&self, action: &str) -> InvokeOutcome {
            self.call(action, vec![Value::resource(&self.root)])
        }

        fn try_call(&self, action: &str, args: Vec<Value>) -> Result<InvokeOutcome, String> {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .map_err(|error| format!("{error:?}"))
        }

        /// Reads the sources and takes in what they found, as a window does.
        fn read(&self) {
            self.call_root("refresh");
            self.call_root("poll");
        }

        fn apps(&self) -> Vec<ResourceRef> {
            self.list(&self.root, "apps")
        }

        fn remotes(&self) -> Vec<ResourceRef> {
            self.list(&self.root, "remotes")
        }

        fn list(&self, r: &ResourceRef, field: &str) -> Vec<ResourceRef> {
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

        fn app(&self, identity: &str) -> ResourceRef {
            self.apps()
                .into_iter()
                .find(|a| self.text(a, "identity") == identity)
                .unwrap_or_else(|| panic!("no app {identity}"))
        }

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.rt.read(r, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view("catalogue"), vec![Value::resource(&self.root)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn page(&self, a: &ResourceRef) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view("app_page"), vec![Value::resource(a)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }
    }

    fn rows(tree: &tilolang_runtime::UiNode) -> Vec<String> {
        of_kind(tree, "ui.ListRow")
            .iter()
            .filter_map(|node| node.props.get("title").and_then(Value::as_text))
            .map(str::to_owned)
            .collect()
    }

    tilo_test_env::scenario! {
        fn the_store_reads_its_sources_and_shows_what_they_offer;

        phase the_sources_are_read_as_a_job_and_what_they_found_reaches_the_catalogue;
        {
            let m = Market::new();
            let started = m.call_root("refresh");
            assert!(matches!(started, InvokeOutcome::Done(_)));
            assert_eq!(m.text(&m.root, "reading"), "job-1", "the read runs as a job");
            assert!(
                m.engine.ops().contains(&"start:catalogue".to_owned()),
                "{:?}",
                m.engine.ops()
            );
            m.call_root("poll");
            assert_eq!(m.text(&m.root, "reading"), "", "the read is over");
            assert_eq!(m.text(&m.root, "status"), "Store refreshed.");
            let apps = m.apps();
            assert_eq!(
                apps.len(),
                2,
                "the Tilo app and the Flathub one; trouble: {}",
                m.text(&m.root, "trouble")
            );
            let notes = m.app("so.tilo.notes");
            assert_eq!(m.text(&notes, "name"), "Notes");
            assert_eq!(m.text(&notes, "available"), "0.2.0");
            assert_eq!(m.text(&notes, "installed"), "0.1.0");
            assert!(m.flag(&notes, "updatable"), "a newer version is offered");
            let spotify = m.app("com.spotify.Client");
            assert!(!m.flag(&spotify, "runnable"), "built for other processors");
            assert!(m.text(&spotify, "trouble").contains("only built for x86_64"));
            assert!(m.flag(&m.root, "flatpak"), "this computer has Flatpak");
        }

        phase an_app_s_page_reads_what_its_source_says_once;
        {
            let m = Market::new();
            m.read();
            let notes = m.app("so.tilo.notes");
            m.call("show_details", vec![Value::resource(&m.root), Value::resource(&notes)]);
            assert_eq!(
                m.text(&notes, "description"),
                "Notes you write alone or together."
            );
            assert_eq!(m.text(&notes, "license"), "MIT");
            assert!(m.flag(&notes, "details_read"));
            match m.rt.read(&notes, "screenshots").unwrap() {
                Value::List(shots) => assert_eq!(shots.len(), 1),
                other => panic!("screenshots read {other:?}"),
            }
            let permissions = m.list(&notes, "permissions");
            assert_eq!(permissions.len(), 1);
            assert_eq!(m.text(&permissions[0], "what"), "network");
            let releases = m.list(&notes, "releases");
            assert_eq!(m.text(&releases[0], "version"), "0.2.0");
        }

        phase a_change_is_the_exact_signed_offer_and_runs_as_a_job;
        {
            let m = Market::new();
            m.read();
            let notes = m.app("so.tilo.notes");
            m.call("update", vec![Value::resource(&notes)]);
            assert_eq!(m.text(&notes, "job"), "job-1");
            let asked: serde_json::Value =
                serde_json::from_str(&m.engine.input_of("update")).unwrap();
            assert_eq!(asked["identity"], "so.tilo.notes");
            assert_eq!(asked["kind"], "tilo");
            assert_eq!(asked["version"], "0.2.0");
            assert_eq!(asked["root"], "aa", "the exact signed root");
            assert_eq!(asked["publisher"], "cc");
            // What the sources were asked is one read, then the change the workspace follows.
            m.call_root("poll");
            assert_eq!(m.text(&notes, "job"), "", "the change ended");
            assert!(
                m.engine.ops().contains(&"call:one".to_owned()),
                "the app's row is read again: {:?}",
                m.engine.ops()
            );
        }

        phase an_app_this_computer_cannot_run_is_refused_before_anything_starts;
        {
            let m = Market::new();
            m.read();
            let spotify = m.app("com.spotify.Client");
            let before = m.engine.ops().len();
            let refused = m.try_call("get", vec![Value::resource(&spotify)]);
            assert!(refused.is_err(), "the require refuses the change");
            assert_eq!(m.engine.ops().len(), before, "nothing reached the device");
        }

        phase an_uninstall_and_a_pin_are_the_workspace_s_own_actions;
        {
            let m = Market::new();
            m.read();
            let notes = m.app("so.tilo.notes");
            m.call("pin", vec![Value::resource(&notes)]);
            assert!(m.flag(&notes, "pinned"));
            assert!(m.engine.input_of("pin").contains("so.tilo.notes"));
            m.call_root("poll");
            m.call("unpin", vec![Value::resource(&notes)]);
            assert!(!m.flag(&notes, "pinned"));
            m.call_root("poll");
            m.call("remove", vec![Value::resource(&notes)]);
            assert!(m.engine.input_of("uninstall").contains("so.tilo.notes"));
        }

        phase blocking_a_person_s_choice_hides_the_app_and_reaches_the_shared_list;
        {
            let m = Market::new();
            m.read();
            let notes = m.app("so.tilo.notes");
            m.call("block_app", vec![Value::resource(&m.root), Value::resource(&notes)]);
            let blocks = m.list(&m.root, "blocks");
            assert_eq!(blocks.len(), 1, "the person's block is a record of theirs");
            assert_eq!(m.text(&blocks[0], "identity"), "so.tilo.notes");
            let asked: serde_json::Value =
                serde_json::from_str(&m.engine.input_of("block")).unwrap();
            assert_eq!(asked["identity"], "so.tilo.notes");
            assert_eq!(asked["blocked"], true);
            m.call("unblock_app", vec![Value::resource(&m.root), Value::resource(&notes)]);
            assert!(m.list(&m.root, "blocks").is_empty());
        }

        phase a_repository_is_browsed_and_its_apps_join_the_catalogue;
        {
            let m = Market::new();
            m.read();
            m.call_root("refresh_remotes");
            let remotes = m.remotes();
            assert_eq!(remotes.len(), 1);
            assert_eq!(m.text(&remotes[0], "name"), "flathub");
            let remote = remotes[0].clone();
            m.call("browse", vec![Value::resource(&m.root), Value::resource(&remote)]);
            assert_eq!(m.text(&m.root, "trouble"), "", "browsing said why it failed");
            let editor = m.app("org.example.Editor");
            assert_eq!(m.text(&editor, "origin"), "remote");
            assert_eq!(m.text(&editor, "registry"), "flathub");
            assert!(m.flag(&remote, "loaded"));
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&m.engine.input_of("browse")).unwrap()
                    ["name"],
                "flathub"
            );
        }

        phase a_handed_package_is_reviewed_and_installed_only_after_it;
        {
            let m = Market::new();
            let file = Value::file(FileValue::new("local.tapp", "application/vnd.tilo.package", 10, "h1"));
            m.call("import_package", vec![Value::resource(&m.root), file]);
            assert_eq!(m.text(&m.root, "reviewed_app"), "so.tilo.local");
            assert_eq!(m.text(&m.root, "reviewed_root"), "ee");
            assert_eq!(
                m.text(&m.root, "status"),
                "Package verified. Review its details before installing."
            );
            m.call_root("install_reviewed");
            let asked: serde_json::Value =
                serde_json::from_str(&m.engine.input_of("install")).unwrap();
            assert_eq!(asked["kind"], "local");
            assert_eq!(asked["root"], "ee", "the reviewed root, never a new one");
            assert!(asked["file"].as_str().is_some_and(|file| !file.is_empty()));
            assert_eq!(m.text(&m.root, "reviewed_app"), "", "the review is used once");
        }

        phase a_report_is_a_draft_the_registry_answers_and_evidence_is_a_file;
        {
            let m = Market::new();
            m.read();
            let notes = m.app("so.tilo.notes");
            let report = returned_ref(m.call(
                "draft_report",
                vec![Value::resource(&m.root), Value::resource(&notes)],
            ));
            assert_eq!(m.text(&report, "status"), "Not submitted");
            m.call("prepare_report", vec![Value::resource(&m.root), Value::resource(&report), Value::Bool(false)]);
            assert!(
                m.text(&report, "operation").contains("file_report"),
                "the registry's exact operation is kept"
            );
            m.call(
                "record_moderation_case",
                vec![
                    Value::resource(&m.root),
                    Value::text("so.tilo.notes"),
                    Value::text("case-1"),
                    Value::text("Quarantined"),
                ],
            );
            assert_eq!(m.text(&report, "case"), "case-1");
            assert_eq!(m.text(&report, "status"), "Quarantined");
            m.call(
                "refresh_status",
                vec![Value::resource(&m.root), Value::resource(&report)],
            );
            assert_eq!(m.text(&report, "status"), "Quarantined");
            // The evidence a person saves is the report's own fields.
            let file = match m.call("report_file", vec![Value::resource(&report)]) {
                InvokeOutcome::Done(Value::File(file)) => (*file).clone(),
                other => panic!("the evidence is a file: {other:?}"),
            };
            assert_eq!(file.name, "so.tilo.notes-report.json");
        }

        phase the_window_lists_the_catalogue_and_an_app_s_page_shows_its_facts;
        {
            let m = Market::new();
            m.read();
            let window = m.window();
            let tree = window.tree();
            assert!(
                of_kind(tree, "ui.NavShell").len() == 1,
                "the catalogue is a window: {}",
                dump(tree)
            );
            let titles = rows(tree);
            assert!(titles.contains(&"Notes".to_owned()), "{titles:?}");
            // An app this computer cannot run is found by search, never listed among apps.
            assert!(!titles.contains(&"Spotify".to_owned()), "{titles:?}");
            let notes = m.app("so.tilo.notes");
            m.call("show_details", vec![Value::resource(&m.root), Value::resource(&notes)]);
            let page = m.page(&notes);
            let tree = page.tree();
            assert!(
                of_kind(tree, "ui.ObjectHeader").iter().any(|header| {
                    header.props.get("title").and_then(Value::as_text) == Some("Notes")
                }),
                "the page names the app: {}",
                dump(tree)
            );
            assert!(
                of_kind(tree, "ui.Fact").iter().any(|fact| {
                    fact.props
                        .get("text")
                        .and_then(Value::as_text)
                        .is_some_and(|text| text.contains("0.2.0"))
                }),
                "the page says the version: {}",
                dump(tree)
            );
            assert!(
                of_kind(tree, "ui.Button").iter().any(|button| {
                    button.props.get("label").and_then(Value::as_text) == Some("Update")
                }),
                "the one action it offers now: {}",
                dump(tree)
            );
            assert!(
                of_kind(tree, "ui.Image").iter().any(|image| {
                    image.props.get("src").and_then(Value::as_text)
                        == Some("https://artwork.invalid/one.png")
                }),
                "the screenshot its source published: {}",
                dump(tree)
            );
        }


        phase updating_everything_asks_for_each_app_s_own_offer;
        {
            let m = Market::new();
            m.read();
            m.call_root("update_all");
            let asked: serde_json::Value =
                serde_json::from_str(&m.engine.input_of("update-all")).unwrap();
            let list = asked["list"].as_array().expect("a list of changes");
            assert_eq!(list.len(), 1, "only the app with a newer version");
            assert_eq!(list[0]["identity"], "so.tilo.notes");
            assert_eq!(list[0]["version"], "0.2.0");
        }

        phase a_runtime_is_reviewed_from_its_exact_signed_offer;
        {
            let m = Market::new();
            let runtime = match m.call(
                "review_runtime",
                vec![Value::resource(&m.root), Value::text("tilo-runtime/2026.07")],
            ) {
                InvokeOutcome::Done(Value::Opt(Some(value))) => match *value {
                    Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                    other => panic!("the reviewed runtime is a record: {other:?}"),
                },
                other => panic!("the reviewed runtime is an app: {other:?}"),
            };
            assert_eq!(m.text(&runtime, "identity"), "so.tilo.notes");
            assert!(
                serde_json::from_str::<serde_json::Value>(&m.engine.input_of("runtime")).unwrap()
                    ["runtime"]
                    == "tilo-runtime/2026.07"
            );
        }

        phase a_report_has_a_page_its_person_fills_in;
        {
            let m = Market::new();
            m.read();
            let notes = m.app("so.tilo.notes");
            let report = returned_ref(m.call(
                "draft_report",
                vec![Value::resource(&m.root), Value::resource(&notes)],
            ));
            let mut window = m
                .rt
                .open_view(&me(), &m.app.view("report_page"), vec![Value::resource(&report)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            let tree = window.tree();
            assert!(
                of_kind(tree, "ui.Badge").iter().any(|badge| {
                    badge.props.get("text").and_then(Value::as_text) == Some("Not submitted")
                }),
                "the page says where the report stands: {}",
                dump(tree)
            );
            let buttons = of_kind(tree, "ui.Button")
                .iter()
                .filter_map(|button| button.props.get("label").and_then(Value::as_text))
                .collect::<Vec<_>>();
            assert!(buttons.contains(&"Submit report"), "{buttons:?}");
            assert!(buttons.contains(&"Export evidence"), "{buttons:?}");
        }

        phase the_window_says_why_the_sources_could_not_be_read;
        {
            let m = Market::new();
            *m.engine.unreachable.borrow_mut() = true;
            m.call_root("refresh");
            m.call_root("poll");
            assert_eq!(m.text(&m.root, "trouble"), "the sources are unreachable");
            let window = m.window();
            let tree = window.tree();
            let banners = of_kind(tree, "ui.Banner");
            assert_eq!(banners.len(), 1, "the window says why: {}", dump(tree));
            assert_eq!(
                banners[0].props.get("message").and_then(Value::as_text),
                Some("the sources are unreachable")
            );
        }
    }
}
