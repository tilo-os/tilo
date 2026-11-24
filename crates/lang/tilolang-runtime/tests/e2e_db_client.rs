//! The Database Client (`apps/tl/db-client/app.tl`) end to end: a connection saved and opened
//! through its actions, a statement run as a job the device answers, its result read, copied and
//! exported, the catalog browsed page by page, and the structure of a table.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilo_db::Database;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/db-client/app.tl");

    /// The device's engine: the database engine the app asks by name, exactly as the session
    /// registers it.
    #[derive(Clone, Default)]
    struct Engine(Database);

    impl EngineHost for Engine {
        fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            assert_eq!(app, "so.tilo.db-client", "an app asks its own engine");
            self.0.call(op, input)
        }

        fn start(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            assert_eq!(app, "so.tilo.db-client");
            self.0.start(op, input)
        }

        fn status(&self, job: &str) -> Result<String, String> {
            self.0.status(job)
        }

        fn cancel(&self, job: &str) -> bool {
            self.0.cancel(job)
        }
    }

    /// The device's content store: what a file an action made holds.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = format!("h{}", bytes.len());
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    /// The person's clipboard, as a copy offers it.
    #[derive(Clone, Default)]
    struct Clip(Rc<RefCell<String>>);

    impl ClipboardHost for Clip {
        fn copy(&self, _mime: &str, bytes: &[u8]) -> bool {
            *self.0.borrow_mut() = String::from_utf8_lossy(bytes).into_owned();
            true
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            None
        }
    }

    struct Client {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Device,
        clip: Clip,
        /// The database file the test's connection opens, kept alive for the test's length.
        _directory: tempfile::TempDir,
        path: String,
    }

    impl Client {
        fn new() -> Self {
            // The app declares the effects it uses; the authority here allows them for it.
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            rt.set_engine(Engine::default());
            let device = Device::default();
            rt.set_blobs(device.clone());
            let clip = Clip::default();
            rt.set_clipboard(clip.clone());
            rt.set_clock(|| "2026-01-02T03:04:05Z".to_owned());
            let app = App::compile(SOURCE, &[]);
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

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.rt.read(r, field).unwrap() {
                Value::Text(text) => text,
                Value::Int(n) => n.to_string(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn number(&self, r: &ResourceRef, field: &str) -> i64 {
            match self.rt.read(r, field).unwrap() {
                Value::Int(n) => n,
                Value::Float(n) => n as i64,
                other => panic!("{field} reads {other:?}"),
            }
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
            let first = outcomes
                .into_iter()
                .find_map(|item| match item {
                    Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no {held} record"));
            first
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
        fn editor(&self, window: &ViewInstance) -> UiNode {
            of_kind(window.tree(), "ui.TextInput")
                .into_iter()
                .find(|node| {
                    node.props.get("placeholder").and_then(Value::as_text)
                        == Some("Write a SQL query")
                })
                .cloned()
                .unwrap_or_else(|| panic!("the SQL editor is in the window:\n{}", dump(window.tree())))
        }

        /// Types `sql` into the shared editor, as a person does.
        fn type_sql(&self, sql: &str) {
            let window = self.window();
            let editor = self.editor(&window);
            window.edit(&editor.key, &Value::text(sql)).unwrap();
        }

        fn is_busy(&self) -> bool {
            self.sessions()
                .first()
                .map(|s| !self.text(s, "busy").is_empty())
                .unwrap_or(false)
        }

        /// Asks the app how far its job has got, as its own trigger does, until it settles.
        fn settle(&self) {
            for _ in 0..4000 {
                if !self.is_busy() {
                    return;
                }
                self.with("check", &[("d", self.root_arg())]);
                std::thread::sleep(std::time::Duration::from_millis(1));
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
            panic!("the job never settled: {} sessions; {seen:?}", sessions.len());
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
            let field = if held == "queries" { "result_rows" } else { "page_rows" };
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
                            Some(format!("{}.{}", self.text(&t, "schema"), self.text(&t, "name")))
                        }
                        _ => None,
                    })
                    .collect(),
                other => panic!("tables read {other:?}"),
            }
        }

        /// The columns the window read for the selected table.
        fn columns(&self) -> Vec<ResourceRef> {
            let s = self.session();
            match self.rt.read(&s, "columns").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("columns read {other:?}"),
            }
        }

        /// Chooses the table the catalog lists as `schema.name`.
        fn browse(&self, name: &str) {
            let s = self.session();
            let found = match self.rt.read(&s, "tables").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .find(|t| name.ends_with(&self.text(t, "name")))
                    .unwrap_or_else(|| panic!("{name} is in the catalog")),
                other => panic!("tables read {other:?}"),
            };
            self.with("browse", &[("d", self.root_arg()), ("t", Value::resource(&found))]);
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
                        .filter_map(|item| match item {
                            Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                            _ => None,
                        })
                        .next()
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
                listed.iter().any(|t| t.ends_with(".notes")),
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
            let columns = c.columns();
            assert_eq!(columns.len(), 2, "{:?}", c.tables());
            let first = &columns[0];
            assert_eq!(c.text(first, "name"), "id");
            assert!(c.flag(first, "primary_key"));
            assert_eq!(c.text(first, "decl_type"), "INTEGER");
        }

        phase the_result_is_copied_and_exported_as_a_csv_file;
        {
            let c = Client::new();
            c.connected();
            c.run("SELECT 'a,b' AS said, 2 AS counted");
            let row = c.rows("queries").into_iter().next().expect("a row");
            c.with("copy_row", &[("d", c.root_arg()), ("row", Value::resource(&row))]);
            assert_eq!(*c.clip.0.borrow(), "a,b\t2");
            c.with("copy_result", &[("d", c.root_arg())]);
            assert_eq!(*c.clip.0.borrow(), "said\tcounted\na,b\t2");
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
                &c.device.0.borrow().get(&file.hash).cloned().unwrap_or_default(),
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
            c.with("disconnect", &[("d", c.root_arg())]);
            let session = c.session();
            assert!(!c.flag(&session, "connected"));
            assert_eq!(c.text(&session, "open"), "");
            assert_eq!(c.text(&session, "name"), "");
            assert!(c.connections().len() == 1, "the saved connection stays");
        }
    }
}
