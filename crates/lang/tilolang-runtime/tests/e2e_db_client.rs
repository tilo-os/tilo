//! The Database Client (`apps/tl/db-client/app.tl`) end to end: a connection saved and opened
//! through its actions, a statement run as a job the device answers, its result read, copied and
//! exported, the catalog browsed page by page, and the structure of a table.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::rc::Rc;

    use crate::common::*;
    use tilo_db::Database;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/db-client/app.tl");

    /// The device's engine: the database engine the app asks by name, exactly as the session
    /// registers it, with the actions each started job named.
    #[derive(Clone, Default)]
    struct Engine {
        database: Database,
        handlers: Rc<RefCell<Vec<tilolang_runtime::host_work::JobHandlers>>>,
        jobs: DeviceJobs,
    }

    impl EngineHost for Engine {
        fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            if app != "so.tilo.db-client" {
                return Err(format!("`{app}` is not the app this engine answers for"));
            }
            self.database.call(op, input)
        }

        fn start(
            &self,
            app: &str,
            op: &str,
            input: &str,
            handlers: &tilolang_runtime::host_work::JobHandlers,
        ) -> Result<String, String> {
            if app != "so.tilo.db-client" {
                return Err(format!("`{app}` is not the app this engine answers for"));
            }
            let (job, moved) = self.jobs.begin(app, op);
            self.database.start(
                &job,
                op,
                input,
                std::sync::Arc::new(move |state, detail| {
                    let state = tilolang_engine::State::from_word(state)
                        .unwrap_or(tilolang_engine::State::Running);
                    moved.moved(state, detail);
                }),
            )?;
            self.handlers.borrow_mut().push(handlers.clone());
            Ok(job)
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            Ok(self.jobs.job(job))
        }

        fn cancel(&self, job: &str) -> bool {
            self.database.cancel(job)
        }
    }

    struct Client {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Files,
        engine: Engine,
        clip: Clipboard,
        /// The database file the test's connection opens, kept alive for the test's length.
        _directory: tempfile::TempDir,
        path: String,
    }

    impl Client {
        fn new() -> Self {
            Self::with_app(App::compile(SOURCE, &[]))
        }

        /// The client whose app is the package the image's pack path built: its module is
        /// the package's bytes, not a build of the source this test carries.
        fn packaged() -> Self {
            Self::with_app(packaged_app())
        }

        fn with_app(app: App) -> Self {
            // The app declares the effects it uses; the authority here allows them for it.
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let engine = Engine::default();
            rt.set_engine(engine.clone());
            let device = Files::default();
            rt.set_blobs(device.clone());
            let clip = Clipboard::default();
            rt.set_clipboard(clip.clone());
            rt.set_clock(|| "2026-01-02T03:04:05Z".to_owned());
            authority.trust(&app.load(&rt));
            for action in ["copy_row", "copy_result"] {
                authority.grant_outside(&app.action(action));
            }
            let root = rt.create_root("Database").unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("shop.sqlite");
            std::fs::File::create(&path).unwrap();
            Self {
                rt,
                app,
                root,
                device,
                engine,
                clip,
                _directory: directory,
                path: path.to_string_lossy().into_owned(),
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            let outcome = self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap();
            match &outcome {
                InvokeOutcome::Denied => panic!("{action} was denied"),
                InvokeOutcome::Asked(what) => panic!("{action} asks: {what}"),
                InvokeOutcome::Waits { call, .. } => panic!("{action} waits on {call}"),
                InvokeOutcome::Done(_) => outcome,
            }
        }

        fn with(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let args = self.app.args(&self.rt, action, given);
            self.call(action, args)
        }

        fn root_arg(&self) -> Value {
            Value::resource(&self.root)
        }

        fn records(&self, name: &str) -> Vec<ResourceRef> {
            let d = self.rt.read(&self.root, name).unwrap();
            match d {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("{name} reads {other:?}"),
            }
        }

        fn connections(&self) -> Vec<ResourceRef> {
            self.records("connections")
        }

        fn queries(&self) -> Vec<ResourceRef> {
            self.records("queries")
        }

        fn sessions(&self) -> Vec<ResourceRef> {
            self.records("sessions")
        }

        fn session(&self) -> ResourceRef {
            self.sessions().into_iter().next().expect("a session")
        }

        fn result(&self, held: &str) -> ResourceRef {
            let s = self.session();
            let outcomes = match self.rt.read(&s, held).unwrap() {
                Value::List(items) => items,
                other => panic!("{held} reads {other:?}"),
            };

            outcomes
                .into_iter()
                .find_map(|item| match item {
                    Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no {held} record"))
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view("window"), vec![self.root_arg()])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// The editor's node: the field the SQL is typed into.
        fn editor(window: &ViewInstance) -> UiNode {
            of_kind(window.tree(), "ui.TextInput")
                .into_iter()
                .find(|node| {
                    node.props.get("placeholder").and_then(Value::as_text)
                        == Some("Write a SQL query")
                })
                .cloned()
                .unwrap_or_else(|| {
                    panic!("the SQL editor is in the window:\n{}", dump(window.tree()))
                })
        }

        /// Types `sql` into the shared editor, as a person does.
        fn type_sql(&self, sql: &str) {
            let window = self.window();
            let editor = Self::editor(&window);
            window.edit(&editor.key, &Value::text(sql)).unwrap();
        }

        fn is_busy(&self) -> bool {
            self.sessions()
                .first()
                .is_some_and(|s| !self.text(s, "busy").is_empty())
        }

        /// Drives the job in flight the way the device does: waits for the engine to end it, then
        /// runs the `done:` action its start named, handed the record the device keeps.
        fn settle(&self) {
            for _ in 0..4000 {
                if !self.is_busy() {
                    return;
                }
                let id = self.text(&self.session(), "busy");
                let record = self
                    .engine
                    .jobs
                    .job(&id)
                    .expect("the device records the job");
                let state = record["state"].as_str().unwrap_or_default();
                if matches!(state, "queued" | "running") {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                let kept = self
                    .engine
                    .handlers
                    .borrow()
                    .last()
                    .cloned()
                    .expect("a job was started");
                let handler = kept.done.clone().expect("the start named its end");
                let call = self.rt.job_call(&handler, &record).unwrap();
                self.rt.invoke_action(&me(), call).unwrap();
            }
            let sessions = self.sessions();
            let seen = sessions.first().map(|s| {
                format!(
                    "busy={:?} kind={:?} notice={:?} status={:?} connected={}",
                    self.text(s, "busy"),
                    self.text(s, "job_kind"),
                    self.text(s, "notice"),
                    self.text(s, "status"),
                    self.flag(s, "connected"),
                ) + &format!(" working={:?}", self.text(s, "working"))
            });
            panic!(
                "the job never settled: {} sessions; {seen:?}",
                sessions.len()
            );
        }

        /// Saves a connection to the test's database file and opens it.
        fn connected(&self) {
            self.with(
                "add_connection",
                &[
                    ("d", self.root_arg()),
                    ("name", Value::text("Shop")),
                    ("engine_index", Value::Int(0)),
                    ("host", Value::text("")),
                    ("port", Value::Int(0)),
                    ("database", Value::text("")),
                    ("user", Value::text("")),
                    ("file_path", Value::text(self.path.clone())),
                    ("password", Value::text("")),
                ],
            );
            self.settle();
            assert_eq!(
                self.sessions().len(),
                1,
                "an action makes one window, not one per call"
            );
            let session = self.session();
            assert!(
                self.flag(&session, "connected"),
                "the connection opened: notice={:?} status={:?}",
                self.text(&session, "notice"),
                self.text(&session, "status"),
            );
            assert_eq!(self.text(&session, "notice"), "");
        }

        /// Runs `sql` through the editor and the Run action, confirming a change when asked.
        fn run(&self, sql: &str) {
            self.type_sql(sql);
            self.with("run", &[("d", self.root_arg())]);
            if self.flag(&self.session(), "confirming") {
                self.with("confirm_run", &[("d", self.root_arg())]);
            }
            self.settle();
        }

        fn status(&self) -> String {
            self.text(&self.session(), "status")
        }

        fn notice(&self) -> String {
            self.text(&self.session(), "notice")
        }

        /// The rows of a result.
        fn rows(&self, held: &str) -> Vec<ResourceRef> {
            let field = if held == "queries" {
                "result_rows"
            } else {
                "page_rows"
            };
            let s = self.session();
            match self.rt.read(&s, field).unwrap() {
                Value::List(rows) => rows
                    .into_iter()
                    .filter_map(|row| match row {
                        Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("rows read {other:?}"),
            }
        }

        /// One cell's text.
        fn cell(&self, held: &str, row: usize, at: usize) -> String {
            let rows = self.rows(held);
            let cells = match self.rt.read(&rows[row], "cells").unwrap() {
                Value::List(cells) => cells,
                other => panic!("cells read {other:?}"),
            };
            match cells.get(at) {
                Some(Value::Text(text)) => text.clone(),
                other => panic!("cell {at} reads {other:?}"),
            }
        }

        /// The catalog the window holds: one entry per table, as `schema.name`.
        fn tables(&self) -> Vec<String> {
            let s = self.session();
            match self.rt.read(&s, "tables").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => {
                            let t = Rc::unwrap_or_clone(r.target);
                            Some(format!(
                                "{}.{}",
                                self.text(&t, "schema"),
                                self.text(&t, "name")
                            ))
                        }
                        _ => None,
                    })
                    .collect(),
                other => panic!("tables read {other:?}"),
            }
        }

        /// Every relation's columns the window read with its catalog.
        fn columns(&self) -> Vec<ResourceRef> {
            let s = self.session();
            match self.rt.read(&s, "catalog_columns").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("catalog_columns read {other:?}"),
            }
        }

        /// The columns of the relation `schema.table`.
        fn columns_of(&self, schema: &str, table: &str) -> Vec<ResourceRef> {
            self.columns()
                .into_iter()
                .filter(|c| self.text(c, "schema") == schema && self.text(c, "table") == table)
                .collect()
        }

        /// Whether the window shows the view `tab` names (`Tab.data`).
        fn shows_tab(&self, tab: &str) -> bool {
            let case = self
                .app
                .compiled
                .ids
                .get(tab)
                .unwrap_or_else(|| panic!("no case {tab}"));
            match self.rt.read(&self.session(), "tab").unwrap() {
                Value::Enum(tab) => tab.variant == case,
                other => panic!("tab reads {other:?}"),
            }
        }

        /// The catalog's relation `schema.name`, as a record search hands over.
        fn table(&self, name: &str) -> ResourceRef {
            let s = self.session();
            match self.rt.read(&s, "tables").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .find(|t| name.ends_with(&self.text(t, "name")))
                    .unwrap_or_else(|| panic!("{name} is in the catalog")),
                other => panic!("tables read {other:?}"),
            }
        }

        /// Chooses the table the catalog lists as `schema.name`.
        fn browse(&self, name: &str) {
            let found = self.table(name);
            self.with(
                "browse",
                &[("d", self.root_arg()), ("t", Value::resource(&found))],
            );
        }
    }

    impl Reads for Client {
        fn rt(&self) -> &Runtime {
            &self.rt
        }
    }

    /// The app the image's pack path built, read from the `.tapp` `TILO_APP_PACKAGE` names
    /// (`tools/ci/pack-model-tapp.sh`): the signature verifies, the package carries its module
    /// and surface, and the module is loaded the way this runtime runs an installed app's.
    fn packaged_app() -> App {
        let path = std::env::var_os("TILO_APP_PACKAGE")
            .expect("TILO_APP_PACKAGE names no package; run it with `t test test-packaged-model`");
        let bytes = std::fs::read(Path::new(&path)).expect("the package reads");
        let mut blobs: BTreeMap<tilo_app_package::blobs::BlobHash, Vec<u8>> = BTreeMap::new();
        let package = tilo_app_package::tapp::read(bytes.as_slice(), &mut blobs)
            .expect("the bytes are a .tapp");
        assert!(
            tilo_app_package::verify::verify(&package.meta, &[])
                .manifest()
                .is_some(),
            "the package's signature verifies"
        );
        assert_eq!(
            package.meta.manifest.package.id, "so.tilo.db-client",
            "the package is Database Client's"
        );
        let entries = &package.meta.collection.entries;
        for entry in ["app.wasm", "app.ids", "surface.json"] {
            assert!(entries.contains_key(entry), "the package carries {entry}");
        }
        // The payload as an install stages it, hash-checked by the installer's own unpack.
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let payload = scratch.path().join("payload");
        tilo_app_package::unpack(&package.meta, &blobs, &payload)
            .expect("the package's payload unpacks");
        let bundle = tilolang_tilo::bundle::Bundle::read(&payload)
            .expect("the package's payload is a tilolang bundle");
        // The surface the signed package carries is the compiled form of the module's own
        // declarations: what a receiver decides the app's changes against.
        let carried = entries
            .get("surface.json")
            .and_then(|hash| blobs.get(hash))
            .expect("the package carries its surface's bytes");
        assert_eq!(
            &bundle.surface().expect("the module's surface compiles"),
            carried,
            "the package's surface is its module's"
        );
        let platform = tilolang_tilo::platform_apps::platform_packages_of(
            bundle.meta.uses.iter().map(String::as_str),
        )
        .expect("the platform's packages read");
        App {
            compiled: tilolang::Compiled {
                wasm: bundle.wasm,
                meta: bundle.meta,
                ids: tilolang::IdsLock::parse(&bundle.ids).expect("the package's ids read"),
            },
            platform,
        }
    }

    tilo_test_env::scenario! {
        fn the_client_saves_a_connection_runs_sql_and_shows_what_it_answered;

        phase a_fresh_client_has_nothing_and_says_where_to_start;
        {
            let c = Client::new();
            assert!(c.connections().is_empty(), "no fabricated connections");
            let window = c.window();
            let words = dump(window.tree());
            assert!(words.contains("No connections yet"), "{words}");
            assert!(words.contains("New connection"), "{words}");
        }

        phase a_connection_is_saved_without_its_password_and_opens_its_catalog;
        {
            let c = Client::new();
            c.with(
                "add_connection",
                &[
                    ("d", c.root_arg()),
                    ("name", Value::text("Shop")),
                    ("engine_index", Value::Int(0)),
                    ("host", Value::text("")),
                    ("port", Value::Int(0)),
                    ("database", Value::text("")),
                    ("user", Value::text("")),
                    ("file_path", Value::text(c.path.clone())),
                    ("password", Value::text("hunter2")),
                ],
            );
            c.settle();
            let saved = c.connections();
            assert_eq!(saved.len(), 1);
            assert_eq!(c.text(&saved[0], "name"), "Shop");
            let document = format!("{:?}", c.rt.read(&saved[0], "file_path").unwrap());
            assert!(!document.contains("hunter2"), "the connection is shared without its secret");
            let session = c.session();
            match c.rt.read(&session, "secrets").unwrap() {
                Value::List(secrets) => {
                    let held = secrets
                        .into_iter()
                        .find_map(|item| match item {
                            Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                            _ => None,
                        })
                        .expect("this device keeps the password");
                    assert_eq!(c.text(&held, "password"), "hunter2");
                }
                other => panic!("secrets read {other:?}"),
            }
            // The app's own actions see this person's window, and make no second one.
            let again = returned_ref(c.with("ensure_session", &[("d", c.root_arg())]));
            assert_eq!(c.sessions().len(), 1, "the app's own action sees the window it made");
            assert_eq!(again.id_string(), session.id_string());
            assert!(c.flag(&session, "connected"), "the connection opened");
            assert_eq!(c.notice(), "");
            assert_eq!(c.status(), "Connected to Shop");
        }

        phase a_statement_changes_the_database_and_answers_its_rows;
        {
            let c = Client::new();
            c.connected();
            c.run("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)");
            assert_eq!(c.notice(), "");
            c.run("INSERT INTO notes (body) VALUES ('first'), ('second')");
            c.run("SELECT id, body FROM notes ORDER BY id");
            let result = c.result("queries");
            assert!(c.flag(&result, "is_select"));
            let rows = c.rows("queries");
            assert_eq!(rows.len(), 2);
            assert_eq!(c.cell("queries", 1, 1), "second");
            assert_eq!(c.status(), "2 in under a millisecond");
            let listed = c.tables();
            assert!(
                listed.iter().any(|t| {
                    std::path::Path::new(t)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("notes"))
                }),
                "the new table reached the catalog: {listed:?}"
            );
        }

        phase a_change_asks_first_and_the_confirmed_statement_runs;
        {
            let c = Client::new();
            c.connected();
            c.type_sql("DELETE FROM notes");
            c.with("run", &[("d", c.root_arg())]);
            let session = c.session();
            assert!(c.flag(&session, "confirming"), "a change asks first");
            assert_eq!(c.text(&session, "pending"), "DELETE FROM notes");
            assert!(!c.is_busy(), "nothing ran yet");
            c.with("dismiss_run", &[("d", c.root_arg())]);
            assert!(!c.flag(&c.session(), "confirming"), "the ask is put away");
            assert_eq!(c.text(&c.session(), "pending"), "");
        }

        phase a_read_query_runs_without_asking_even_naming_a_change_in_text;
        {
            let c = Client::new();
            c.connected();
            c.type_sql("SELECT 'DELETE FROM notes' AS said");
            c.with("run", &[("d", c.root_arg())]);
            assert!(!c.flag(&c.session(), "confirming"), "a read never asks");
            c.settle();
            assert_eq!(c.cell("queries", 0, 0), "DELETE FROM notes");
        }

        phase a_statement_is_run_as_a_job_and_a_long_one_is_cancelled;
        {
            let c = Client::new();
            c.connected();
            c.type_sql(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 1000000000) SELECT sum(x) FROM n",
            );
            c.with("run", &[("d", c.root_arg())]);
            // A `WITH` statement may write, so the first run asks; confirming starts it.
            if c.flag(&c.session(), "confirming") {
                c.with("confirm_run", &[("d", c.root_arg())]);
            }
            assert!(c.is_busy(), "the statement is in flight");
            c.with("cancel", &[("d", c.root_arg())]);
            c.settle();
            assert_eq!(c.status(), "Query cancelled");
        }

        phase a_transaction_holds_its_changes_until_it_is_committed_or_rolled_back;
        {
            let c = Client::new();
            c.connected();
            c.run("CREATE TABLE ledger (id INTEGER PRIMARY KEY, amount INTEGER NOT NULL)");
            c.with("begin_transaction", &[("d", c.root_arg())]);
            c.settle();
            assert!(c.flag(&c.session(), "transaction"), "the transaction is open");
            assert_eq!(c.status(), "Transaction open");
            c.run("INSERT INTO ledger (amount) VALUES (10)");
            let window = c.window();
            let words = dump(window.tree());
            assert!(words.contains("Commit transaction"), "the window offers Commit: {words}");
            assert!(words.contains("Roll back transaction"), "{words}");
            c.with("rollback_transaction", &[("d", c.root_arg())]);
            c.settle();
            assert!(!c.flag(&c.session(), "transaction"), "the rollback closed it");
            assert_eq!(c.status(), "Transaction rolled back");
            c.run("SELECT COUNT(*) AS rows FROM ledger");
            assert_eq!(c.cell("queries", 0, 0), "0", "the insert was taken back");

            c.with("begin_transaction", &[("d", c.root_arg())]);
            c.settle();
            assert!(c.flag(&c.session(), "transaction"));
            c.run("INSERT INTO ledger (amount) VALUES (20)");
            c.with("commit_transaction", &[("d", c.root_arg())]);
            c.settle();
            assert!(!c.flag(&c.session(), "transaction"), "the commit closed it");
            assert_eq!(c.status(), "Transaction committed");
            c.run("SELECT COUNT(*) AS rows FROM ledger");
            assert_eq!(c.cell("queries", 0, 0), "1", "the insert was kept");
            // Without a transaction open, committing or rolling back is refused.
            assert!(c.rt.invoke_action(
                &me(),
                (c.app.action("commit_transaction").as_str(), vec![c.root_arg()]),
            ).is_err(), "a commit with nothing open is refused");
        }

        phase browsing_pages_a_table_and_reads_its_structure;
        {
            let c = Client::new();
            c.connected();
            c.run("CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
            c.run("INSERT INTO artists (name) VALUES ('Ada'), ('Grace'), ('Alan')");
            c.with("refresh_catalog", &[("d", c.root_arg())]);
            assert!(
                c.tables().iter().any(|t| t.ends_with(".artists")),
                "artists is in the catalog: {:?}",
                c.tables()
            );
            c.browse("main.artists");
            let rows = c.rows("pages");
            assert_eq!(rows.len(), 3, "one page of the table's rows");
            assert_eq!(c.text(&c.session(), "selected_table"), "artists");
            assert_eq!(c.number(&c.session(), "offset"), 0);
            // The page controls are dimmed when there is no other page, so nothing is asked.
            let columns = c.columns_of("main", "artists");
            assert_eq!(columns.len(), 2, "{:?}", c.tables());
            let first = &columns[0];
            assert_eq!(c.text(first, "name"), "id");
            assert!(c.flag(first, "primary_key"));
            assert_eq!(c.text(first, "decl_type"), "INTEGER");
            assert_eq!(c.text(first, "table"), "artists", "a column says its relation");
        }

        phase the_catalog_holds_every_relations_columns_and_the_structure_view_shows_the_chosen_one;
        {
            let c = Client::new();
            c.connected();
            c.run("CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
            c.run("CREATE TABLE albums (id INTEGER PRIMARY KEY, artist_id INTEGER NOT NULL, title TEXT)");
            c.with("refresh_catalog", &[("d", c.root_arg())]);
            let tables = c.tables();
            assert_eq!(tables.len(), 2, "{tables:?}");
            let columns = c.columns();
            assert_eq!(
                columns.len(),
                5,
                "every relation's columns are read: {tables:?}\n{:?}",
                columns
                    .iter()
                    .map(|one| {
                        (
                            c.text(one, "table"),
                            c.text(one, "name"),
                            c.text(one, "decl_type"),
                        )
                    })
                    .collect::<Vec<_>>()
            );
            let albums = c.columns_of("main", "albums");
            assert_eq!(
                albums
                    .iter()
                    .map(|one| c.text(one, "name"))
                    .collect::<Vec<_>>(),
                ["id", "artist_id", "title"],
                "each column is held with its table"
            );
            // The structure view draws the chosen table's columns from the same catalog.
            c.browse("main.albums");
            c.with(
                "choose_tab",
                &[("d", c.root_arg()), ("index", Value::Int(2))],
            );
            assert!(c.shows_tab("Tab.structure"), "the structure view is shown");
            let window = c.window();
            let tree = window.tree();
            let words = dump(tree);
            assert!(words.contains("Columns"), "{words}");
            let cells: Vec<String> = of_kind(tree, "ui.DataTableCell")
                .into_iter()
                .filter_map(|node| node.props.get("value").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert!(cells.iter().any(|cell| cell == "artist_id"), "{cells:?}\n{words}");
            assert!(cells.iter().any(|cell| cell == "INTEGER"), "{cells:?}\n{words}");
            assert!(
                !cells.iter().any(|cell| cell == "name"),
                "another table's columns stay out of the structure view: {cells:?}"
            );
        }

        phase a_table_search_chose_opens_on_its_rows;
        {
            let c = Client::new();
            c.connected();
            c.run("CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
            c.run("INSERT INTO artists (name) VALUES ('Ada'), ('Grace')");
            c.with("refresh_catalog", &[("d", c.root_arg())]);
            c.with(
                "choose_tab",
                &[("d", c.root_arg()), ("index", Value::Int(0))],
            );
            let window = c.window();
            let artists = c.table("main.artists");
            assert!(
                window.show(&artists).unwrap(),
                "the window answers for a table search chose"
            );
            let session = c.session();
            assert_eq!(c.text(&session, "selected_table"), "artists");
            assert_eq!(c.text(&session, "selected_schema"), "main");
            assert!(c.shows_tab("Tab.data"), "the rows the table holds are shown");
            assert_eq!(c.rows("pages").len(), 2, "the table's rows are read");
        }

        phase the_result_is_copied_and_exported_as_a_csv_file;
        {
            let c = Client::new();
            c.connected();
            c.run("SELECT 'a,b' AS said, 2 AS counted");
            let row = c.rows("queries").into_iter().next().expect("a row");
            c.with("copy_row", &[("d", c.root_arg()), ("row", Value::resource(&row))]);
            assert_eq!(c.clip.text(), "a,b\t2");
            c.with("copy_result", &[("d", c.root_arg())]);
            assert_eq!(c.clip.text(), "said\tcounted\na,b\t2");
            assert_eq!(c.status(), "Result copied");
            let file = match c.with("export_csv", &[("d", c.root_arg())]) {
                InvokeOutcome::Done(Value::Opt(Some(held))) => match &*held {
                    Value::File(file) => (*file).clone(),
                    other => panic!("export answers a file: {other:?}"),
                },
                other => panic!("export answers a file: {other:?}"),
            };
            assert_eq!(file.name, "query-results.csv");
            let bytes = String::from_utf8_lossy(
                &c.device.read(&file.hash).unwrap_or_default(),
            )
            .into_owned();
            assert_eq!(bytes, "\"said\",\"counted\"\n\"a,b\",\"2\"\n");
        }

        phase a_saved_query_loads_into_the_editor;
        {
            let c = Client::new();
            c.connected();
            c.type_sql("SELECT 7 AS seven");
            c.with("save_query", &[("d", c.root_arg()), ("name", Value::text("Seven"))]);
            let saved = c.queries();
            assert_eq!(saved.len(), 1);
            assert_eq!(c.text(&saved[0], "sql"), "SELECT 7 AS seven");
            c.type_sql("SELECT 1");
            c.with("load_query", &[("d", c.root_arg()), ("q", Value::resource(&saved[0]))]);
            assert_eq!(c.text(&c.root, "sql"), "SELECT 7 AS seven");
            c.with("run_query", &[("d", c.root_arg()), ("q", Value::resource(&saved[0]))]);
            c.settle();
            assert_eq!(c.cell("queries", 0, 0), "7");
        }

        phase a_failed_connection_is_an_honest_error_that_is_dismissed;
        {
            let c = Client::new();
            c.with(
                "add_connection",
                &[
                    ("d", c.root_arg()),
                    ("name", Value::text("Nowhere")),
                    ("engine_index", Value::Int(1)),
                    ("host", Value::text("127.0.0.1")),
                    ("port", Value::Int(1)),
                    ("database", Value::text("nope")),
                    ("user", Value::text("nobody")),
                    ("file_path", Value::text("")),
                    ("password", Value::text("")),
                ],
            );
            c.settle();
            assert!(!c.flag(&c.session(), "connected"), "no fabricated connection");
            assert!(!c.notice().is_empty(), "the refusal is said");
            c.with("dismiss", &[("d", c.root_arg())]);
            assert_eq!(c.notice(), "");
        }

        phase the_window_shows_the_views_the_editor_and_the_grid;
        {
            let c = Client::new();
            c.connected();
            c.run("SELECT 'first' AS one, 42 AS two");
            let window = c.window();
            let tree = window.tree();
            let words = dump(tree);
            assert!(words.contains("Query"), "the views are chosen: {words}");
            assert!(words.contains("Structure"), "{words}");
            assert!(words.contains("Write a SQL query"), "the editor is in the window: {words}");
            assert!(words.contains("Run query"), "the statement runs from the window: {words}");
            assert!(words.contains("one"), "the first column is drawn: {words}");
            assert!(words.contains("two"), "the second column is drawn: {words}");
            assert!(words.contains("first"), "the row is drawn: {words}");
            let kinds = of_kind(tree, "ui.DataTableColumn")
                .into_iter()
                .filter_map(|node| {
                    let header = node.props.get("header").and_then(Value::as_text)?;
                    let kind = node.props.get("kind").and_then(Value::as_text)?;
                    Some((header.to_owned(), kind.to_owned()))
                })
                .collect::<Vec<_>>();
            assert!(
                kinds.iter().any(|(header, kind)| header == "two" && kind == "numeric"),
                "a number aligns as one: {kinds:?}\n{words}"
            );
            assert!(
                of_kind(tree, "ui.DataTableCell").iter().any(|node| {
                    node.props.get("value").and_then(Value::as_text) == Some("42")
                }),
                "the number is in the grid: {words}"
            );
            assert!(words.contains("1 in under a millisecond"), "the strip says the result: {words}");
        }

        phase disconnecting_closes_the_connection_and_forgets_what_it_showed;
        {
            let c = Client::new();
            c.connected();
            c.run("CREATE TABLE gone (id INTEGER)");
            assert!(!c.tables().is_empty(), "the open connection's catalog is held");
            c.with("disconnect", &[("d", c.root_arg())]);
            let session = c.session();
            assert!(!c.flag(&session, "connected"));
            assert_eq!(c.text(&session, "open"), "");
            assert_eq!(c.text(&session, "name"), "");
            assert!(c.tables().is_empty(), "a closed connection keeps no catalog to find");
            assert!(c.columns().is_empty(), "nor the columns of its relations");
            assert!(c.connections().len() == 1, "the saved connection stays");
        }
    }

    /// The app an image installs is the one that applies its action: the module the check's
    /// package carries writes the record, its change is the app's action and the invoker's, and
    /// an argument the app's verb does not take writes nothing.
    #[test]
    #[ignore = "reads the package the image's pack path built (t test test-packaged-model)"]
    fn the_packaged_app_applies_its_action_from_the_package_an_image_installs() {
        let c = Client::packaged();
        assert_eq!(
            c.app.meta().app,
            "so.tilo.db-client",
            "the module is Database Client's"
        );
        assert_eq!(c.app.meta().title, "Database Client");
        assert!(
            c.connections().is_empty(),
            "a fresh workspace has no fabricated connections"
        );

        c.with(
            "add_connection",
            &[
                ("d", c.root_arg()),
                ("name", Value::text("Packaged")),
                ("engine_index", Value::Int(0)),
                ("host", Value::text("")),
                ("port", Value::Int(0)),
                ("database", Value::text("")),
                ("user", Value::text("")),
                ("file_path", Value::text(c.path.clone())),
                ("password", Value::text("hunter2")),
            ],
        );
        c.settle();
        let saved = c.connections();
        assert_eq!(saved.len(), 1, "the package's action saved the connection");
        assert_eq!(c.text(&saved[0], "name"), "Packaged");
        assert!(
            !format!("{:?}", c.rt.read(&saved[0], "file_path").unwrap()).contains("hunter2"),
            "the shared record keeps no password"
        );
        assert!(
            c.flag(&c.session(), "connected"),
            "the connection the action opened"
        );

        // The change is the app's own verb, and the person who invoked it.
        let action = c.app.action("add_connection");
        let versions = c.rt.versions(&c.root).unwrap();
        assert!(
            versions
                .iter()
                .any(|version| version.action == action && version.who.as_ref() == Some(&me())),
            "a version is the packaged app's action by its invoker: {versions:?}"
        );

        // An argument the verb does not take is refused, and nothing is written.
        let verb = c
            .app
            .meta()
            .actions
            .iter()
            .find(|action| action.name == "add_connection")
            .expect("the package declares the verb");
        let at = verb
            .params
            .iter()
            .position(|param| param.name == "name")
            .expect("the verb takes a name");
        let mut wrong = c.app.args(
            &c.rt,
            "add_connection",
            &[
                ("d", c.root_arg()),
                ("name", Value::text("Sneaky")),
                ("engine_index", Value::Int(0)),
                ("host", Value::text("")),
                ("port", Value::Int(0)),
                ("database", Value::text("")),
                ("user", Value::text("")),
                ("file_path", Value::text(c.path.clone())),
                ("password", Value::text("")),
            ],
        );
        wrong[at] = Value::Int(1);
        assert!(
            c.rt.invoke_action(&me(), (action.as_str(), wrong)).is_err(),
            "a wrong argument is refused before the app runs"
        );
        assert_eq!(c.connections().len(), 1, "a refused action writes nothing");
    }
}
