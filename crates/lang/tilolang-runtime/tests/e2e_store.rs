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
    use tilolang_runtime::run::WebRequest;
    use tilolang_runtime::web::{WebReply, WebTransport};
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

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
      "runtime": "", "updatable": true, "trouble": "",
      "supported_languages": ["en", "de"]
    }"#;

    /// A Flathub app this computer cannot run, as Flathub's own catalogue names it.
    const SPOTIFY: &str = r#"{
      "app_id": "com.spotify.Client", "name": "Spotify", "summary": "Music for everyone",
      "description": "Music for everyone.", "developer_name": "Spotify",
      "project_license": "proprietary", "main_categories": "AudioVideo",
      "icon": "https://artwork.invalid/spotify.png", "arches": ["aarch64"]
    }"#;

    /// A Flathub app this computer runs, as Flathub's own catalogue names it.
    const GLADE: &str = r#"{
      "app_id": "org.gnome.Glade", "name": "Glade", "summary": "Build GTK interfaces",
      "description": "Build GTK interfaces.", "developer_name": "The GNOME Project",
      "project_license": "GPL-2.0-or-later", "main_categories": ["Development", "Graphics"],
      "icon": "https://artwork.invalid/glade.png", "arches": ["x86_64", "aarch64"]
    }"#;

    /// A Flathub app of a Tilo app's own name: the Tilo app is the one a person gets.
    const OTHER_NOTES: &str = r#"{
      "app_id": "org.example.Notes", "name": "Notes", "summary": "Notes, elsewhere",
      "description": "Notes, elsewhere.", "developer_name": "Somebody",
      "project_license": "MIT", "main_categories": "Office",
      "icon": "https://artwork.invalid/notes.png", "arches": ["x86_64"]
    }"#;

    /// What Flathub's search answers for its one page of apps.
    fn search() -> String {
        let hit = |text: &str| serde_json::from_str::<serde_json::Value>(text).expect("a hit");
        serde_json::json!({
            "hits": [hit(SPOTIFY), hit(GLADE), hit(OTHER_NOTES)],
            "query": "", "processingTimeMs": 1, "hitsPerPage": 500, "page": 1,
            "totalPages": 1, "totalHits": 3
        })
        .to_string()
    }

    /// What Flathub says of one app: its AppStream document.
    const APPSTREAM: &str = r#"{
      "description": "Build GTK interfaces quickly.",
      "developer_name": "The GNOME Project",
      "project_license": "GPL-2.0-or-later",
      "urls": {"homepage": "https://glade.gnome.org"},
      "screenshots": [{"imgDesktopUrl": "https://artwork.invalid/glade-1.png"},
                      {"url": "https://artwork.invalid/glade-2.png"},
                      {"url": "http://insecure.invalid/no.png"}],
      "releases": [{"version": "3.40.0", "description": "Faster loading"},
                   {"version": "3.38.2", "description": "Fixes"}]
    }"#;

    /// What Flatpak says one of Flathub's apps asks for.
    const SUMMARY: &str = r#"{"arches": ["x86_64", "aarch64"], "download_size": 4194304,
        "installed_size": 12582912,
        "metadata": {"permissions": {"filesystems": ["home"], "shared": ["network", "ipc"],
                                     "sockets": ["x11"]}}}"#;

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
            "items": [NOTES],
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
            "installed_size": 0,
            "screenshots": ["https://artwork.invalid/one.png"],
            "permissions": [r#"{"what":"network","broad":false}"#],
            "releases": [r#"{"version":"0.2.0","notes":"Faster search"}"#],
            "runtime": "tilo-runtime/2026.07", "runtime_registry": "Tilo main", "digest": "dd",
            "supported_languages": ["en", "de"]
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

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            _handlers: &tilolang_runtime::host_work::JobHandlers,
        ) -> Result<String, String> {
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

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            let said = || -> Result<String, String> {
                if job != "job-1" {
                    return Err(format!("no job `{job}`"));
                }
                if *self.done.borrow() {
                    Ok(
                        r#"{"state":"done","done":1,"total":1,"error":"","status":"Done."}"#
                            .to_owned(),
                    )
                } else {
                    Ok(
                        r#"{"state":"running","done":0,"total":1,"error":"","status":"Working."}"#
                            .to_owned(),
                    )
                }
            };
            Ok(said()
                .ok()
                .and_then(|said| tilolang_tilo::engine::record_of(job, &said)))
        }

        fn cancel(&self, job: &str) -> bool {
            job == "job-1"
        }
    }

    /// Flathub, as the app reads it over the web: its catalogue and one app's page.
    #[derive(Clone, Default)]
    struct Flathub {
        asked: Rc<RefCell<Vec<WebRequest>>>,
    }

    impl WebTransport for Flathub {
        fn fetch(&self, request: &WebRequest) -> WebReply {
            self.asked.borrow_mut().push(request.clone());
            let body = if request.url.contains("/api/v2/search") {
                search()
            } else if request.url.contains("/api/v2/appstream/") {
                APPSTREAM.to_owned()
            } else if request.url.contains("/api/v2/summary/") {
                SUMMARY.to_owned()
            } else {
                "{}".to_owned()
            };
            WebReply {
                status: 200,
                status_text: "OK".to_owned(),
                headers: Vec::new(),
                body: body.into_bytes(),
            }
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
        flathub: Flathub,
    }

    impl Market {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let engine = Engine::default();
            // The device answers every job as done at once: what the app does with an ending is
            // this test's subject, not a wait.
            *engine.done.borrow_mut() = true;
            rt.set_engine(engine.clone());
            let flathub = Flathub::default();
            rt.set_web(flathub.clone());
            rt.set_clock(|| "2026-11-24T09:30:00Z".to_owned());
            let device = Device::default();
            rt.set_blobs(device.clone());
            // The platform's own packages: the session the app opens, the device whose language
            // and processor it reads, and the settings its catalogue is read in.
            let platform = tilolang_tilo::platform_apps::platform_packages()
                .expect("the platform packages read");
            let package = |app: &str| {
                platform
                    .iter()
                    .find(|package| package.app == app)
                    .unwrap_or_else(|| panic!("{app} is a platform package"))
                    .clone()
            };
            let device_package = package("so.tilo.device");
            let settings = package("so.tilo.settings.sections");
            rt.load_foreign(device_package.clone())
                .expect("the device loads");
            rt.load_foreign(settings.clone()).expect("settings loads");
            rt.set_reading(
                &ResourceRef::parse_id("host-info", "so.tilo.device#HostInfo")
                    .expect("the host's address"),
                r#"{"hostname":"Laptop","arch":"x86_64"}"#,
            )
            .expect("the host's facts read");
            rt.set_reading(
                &ResourceRef::parse_id(
                    "settings/appearance",
                    "so.tilo.settings.sections#Appearance",
                )
                .expect("the appearance's address"),
                r#"{"language":"en-US"}"#,
            )
            .expect("the language reads");
            let session = App::compile(SESSION_SOURCE, &[]);
            session.load(&rt);
            let app = App::compile(
                &format!("{SOURCE}\n{VIEWS}"),
                &[
                    session.meta().clone(),
                    device_package.clone(),
                    settings.clone(),
                ],
            );
            authority.trust(&app.load(&rt));
            let root = rt.create_root("Store").unwrap();
            Self {
                rt,
                app,
                root,
                engine,
                flathub,
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

        /// The texts a set or a list field holds.
        fn texts(&self, r: &ResourceRef, field: &str) -> Vec<String> {
            match self.rt.read(r, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|v| v.as_text().map(str::to_owned))
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
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
                .open_view(
                    &me(),
                    &self.app.view("catalogue"),
                    vec![Value::resource(&self.root)],
                )
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
                3,
                "the Tilo app and two of Flathub's, never a second Notes; trouble: {:?}, status: {:?}, web requests: {}",
                m.text(&m.root, "trouble"),
                m.text(&m.root, "status"),
                m.flathub.asked.borrow().len()
            );
            let notes = m.app("so.tilo.notes");
            assert_eq!(m.text(&notes, "name"), "Notes");
            assert_eq!(m.text(&notes, "available"), "0.2.0");
            assert_eq!(m.text(&notes, "installed"), "0.1.0");
            assert!(m.flag(&notes, "updatable"), "a newer version is offered");
            // Flathub's catalogue is read over the web, one page asked for by a POST.
            let asked = m.flathub.asked.borrow().clone();
            assert_eq!(asked.len(), 1, "one page of Flathub's catalogue");
            assert!(asked[0].url.starts_with("https://flathub.org/api/v2/search?locale=en"));
            let page: serde_json::Value =
                serde_json::from_str(asked[0].body.as_deref().expect("a body")).unwrap();
            assert_eq!(page["page"], 1);
            assert_eq!(page["hits_per_page"], 500);
            let glade = m.app("org.gnome.Glade");
            assert_eq!(m.text(&glade, "origin"), "flathub");
            assert_eq!(m.text(&glade, "registry"), "Flathub");
            assert_eq!(m.text(&glade, "summary"), "Build GTK interfaces");
            assert!(m.flag(&glade, "runnable"), "built for x86_64");
            assert_eq!(m.texts(&glade, "categories"), ["Development", "Graphics"]);
            let spotify = m.app("com.spotify.Client");
            assert!(!m.flag(&spotify, "runnable"), "built for other processors");
            assert!(m.text(&spotify, "trouble").contains("only built for aarch64"));
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

        phase a_flathub_app_s_page_reads_its_own_documents;
        {
            let m = Market::new();
            m.read();
            let glade = m.app("org.gnome.Glade");
            m.call("show_details", vec![Value::resource(&m.root), Value::resource(&glade)]);
            assert!(m.flag(&glade, "details_read"), "the page read its documents");
            assert_eq!(m.text(&glade, "description"), "Build GTK interfaces quickly.");
            assert_eq!(m.text(&glade, "developer"), "The GNOME Project");
            assert_eq!(m.text(&glade, "license"), "GPL-2.0-or-later");
            assert_eq!(m.text(&glade, "homepage"), "https://glade.gnome.org");
            assert_eq!(
                m.texts(&glade, "screenshots"),
                [
                    "https://artwork.invalid/glade-1.png",
                    "https://artwork.invalid/glade-2.png"
                ],
                "the screenshots an https address names"
            );
            match m.rt.read(&glade, "size").unwrap() {
                Value::Int(size) => assert_eq!(size, 4194304, "the bytes its source downloads"),
                other => panic!("size reads {other:?}"),
            }
            match m.rt.read(&glade, "installed_size").unwrap() {
                Value::Int(size) => assert_eq!(size, 12582912, "the bytes it takes installed"),
                other => panic!("installed_size reads {other:?}"),
            }
            let page = m.page(&glade);
            let properties: Vec<(String, String)> = of_kind(page.tree(), "ui.Property")
                .iter()
                .filter_map(|property| {
                    let label = property.props.get("label").and_then(Value::as_text)?;
                    let value = property.props.get("value").and_then(Value::as_text)?;
                    Some((label.to_owned(), value.to_owned()))
                })
                .collect();
            assert!(
                properties
                    .iter()
                    .any(|(label, value)| label == "Installed size" && value == "12 MB"),
                "the page says the bytes it takes installed: {properties:?}"
            );
            let permissions: Vec<String> = m
                .list(&glade, "permissions")
                .iter()
                .map(|p| m.text(p, "what"))
                .collect();
            assert_eq!(
                permissions,
                ["home", "network", "system"],
                "what the build asks for, each once"
            );
            let releases = m.list(&glade, "releases");
            assert_eq!(releases.len(), 2);
            assert_eq!(m.text(&releases[0], "version"), "3.40.0");
            assert_eq!(m.text(&releases[0], "notes"), "Faster loading");
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

        phase a_flatpak_reference_is_opened_not_verified_as_a_package;
        {
            let m = Market::new();
            let file = Value::file(FileValue::new(
                "app.flatpakref",
                "application/vnd.flatpak.ref",
                10,
                "h2",
            ));
            m.call("import_package", vec![Value::resource(&m.root), file]);
            let asked = m.engine.ops();
            assert!(
                asked.iter().any(|op| op == "call:reference"),
                "the reference reaches the repository's own read: {asked:?}"
            );
            assert!(
                !asked.iter().any(|op| op == "call:review"),
                "a reference is never verified as a package: {asked:?}"
            );
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
            assert!(titles.contains(&"Glade".to_owned()), "a Flathub app is listed: {titles:?}");
            // An app this computer cannot run is found by search, never listed among apps.
            assert!(!titles.contains(&"Spotify".to_owned()), "{titles:?}");
            assert!(!titles.contains(&"Notes, elsewhere".to_owned()), "{titles:?}");
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
            let properties: Vec<(String, String)> = of_kind(tree, "ui.Property")
                .iter()
                .filter_map(|property| {
                    let label = property.props.get("label").and_then(Value::as_text)?;
                    let value = property.props.get("value").and_then(Value::as_text)?;
                    Some((label.to_owned(), value.to_owned()))
                })
                .collect();
            assert!(
                properties
                    .iter()
                    .any(|(label, value)| label == "Download size" && value == "42 MB"),
                "the page says the bytes it downloads: {properties:?}"
            );
            assert!(
                properties
                    .iter()
                    .any(|(label, value)| label == "Supported languages" && value == "de, en"),
                "the page says the languages its source lists: {properties:?}"
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


        phase choosing_a_category_narrows_the_catalogue;
        {
            let m = Market::new();
            m.read();
            let mut window = m.window();
            let category = of_kind(window.tree(), "ui.NavItem")
                .into_iter()
                .find(|item| {
                    item.props.get("label").and_then(Value::as_text) == Some("Productivity")
                })
                .expect("the sidebar lists the catalogue's categories")
                .key
                .clone();
            window.fire(&category, "select").unwrap();
            window.poll_patches().unwrap();
            let shell = of_kind(window.tree(), "ui.NavShell")
                .into_iter()
                .next()
                .expect("a window");
            assert_eq!(
                shell.props.get("overview_title").and_then(Value::as_text),
                Some("Productivity"),
                "the window names the category it shows: {}",
                dump(window.tree())
            );
            let titles = rows(window.tree());
            assert_eq!(titles, ["Notes"], "only that category's apps: {titles:?}");
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
