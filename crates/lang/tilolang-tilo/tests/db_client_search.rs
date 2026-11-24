//! The Database Client's open connection through system search: the tables, every table's
//! columns with the table they are in, and the statements the window ran, each found by its
//! name. A connection's password is concealed: search neither holds nor finds it.

use std::cell::RefCell;
use std::rc::Rc;
use tilolang_runtime::Actor;

use tilolang_runtime::host_work::{EngineHost, JobHandlers};
use tilolang_runtime::value::Value;
use tilolang_tilo::host::Host;

const SOURCE: (&str, &str) = (
    include_str!("../../../../apps/tl/db-client/app.tl"),
    include_str!("../../../../apps/tl/db-client/app.ids"),
);
const APP: &str = "so.tilo.db-client";

/// The device's engine: the catalog the connection opened on, its columns, and the one result
/// the window ran, with what each started job named to run at its end.
#[derive(Clone, Default)]
struct Engine {
    started: Rc<RefCell<Vec<(String, JobHandlers)>>>,
}

impl EngineHost for Engine {
    fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        assert_eq!(app, APP, "an app asks its own engine");
        match op {
            "catalog" => Ok(r#"["main"]"#.to_owned()),
            "tables" => Ok(r#"[
                {"schema": "main", "name": "albums", "kind": "table", "row_count": 3},
                {"schema": "main", "name": "artists", "kind": "table", "row_count": 2}
            ]"#
            .to_owned()),
            "columns" => {
                let ask: serde_json::Value =
                    serde_json::from_str(input).map_err(|error| error.to_string())?;
                let table = ask["table"].as_str().unwrap_or_default();
                Ok(match table {
                    "albums" => {
                        r#"[
                            {"name": "id", "decl_type": "INTEGER", "nullable": false, "primary_key": true, "default_value": null},
                            {"name": "artist_id", "decl_type": "INTEGER", "nullable": false, "primary_key": false, "default_value": null},
                            {"name": "title", "decl_type": "TEXT", "nullable": true, "primary_key": false, "default_value": null}
                        ]"#
                    }
                    "artists" => {
                        r#"[
                            {"name": "id", "decl_type": "INTEGER", "nullable": false, "primary_key": true, "default_value": null},
                            {"name": "name", "decl_type": "TEXT", "nullable": false, "primary_key": false, "default_value": null}
                        ]"#
                    }
                    other => return Err(format!("no table `{other}`")),
                }
                .to_owned())
            }
            "keys" | "indexes" => Ok("[]".to_owned()),
            other => Err(format!("`{other}` is no database op this device runs")),
        }
    }

    fn start(
        &self,
        app: &str,
        op: &str,
        _input: &str,
        handlers: &JobHandlers,
    ) -> Result<String, String> {
        assert_eq!(app, APP);
        let job = format!("devices/ab/jobs/{op}-1");
        self.started
            .borrow_mut()
            .push((job.clone(), handlers.clone()));
        Ok(job)
    }

    fn job(&self, path: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(Some(Self::record(path)?))
    }

    fn cancel(&self, _job: &str) -> bool {
        false
    }
}

impl Engine {
    /// The record of the job at `path` once it ended, with what it answered.
    fn record(path: &str) -> Result<serde_json::Value, String> {
        let job = path.rsplit('/').next().unwrap_or(path);
        let result = match job {
            "open-1" => r#"{"connection":1}"#,
            "query-1" => {
                r#"{"columns":["name"],"rows":[["Ada"]],"nulls":[[false]],"numeric":[false],"rows_affected":null,"is_select":true,"elapsed_ms":0.5,"truncated":""}"#
            }
            other => return Err(format!("no job `{other}`")),
        };
        Ok(serde_json::json!({
            "id": path,
            "state": "done",
            "detail": {"state": "done", "result": result, "error": ""},
        }))
    }
}

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

struct Installed {
    host: Host,
    engine: Engine,
    root: tilolang_runtime::ResourceRef,
    actions: Vec<(String, String)>,
}

impl Installed {
    fn new() -> Self {
        let host = Host::new(me(), || None).expect("a host");
        host.set_clock(|| "2026-01-02T03:04:05Z".to_owned());
        let engine = Engine::default();
        host.runtime().set_engine(engine.clone());
        let package = host
            .load_builtin(SOURCE.0, SOURCE.1)
            .unwrap_or_else(|error| panic!("{error}"));
        let actions = package
            .meta
            .actions
            .iter()
            .map(|action| (action.name.clone(), action.id.clone()))
            .collect();
        let root = host
            .runtime()
            .create_root("Database")
            .expect("the app's root");
        Self {
            host,
            engine,
            root,
            actions,
        }
    }

    /// The job last started ends: the device runs what its start named for its end, as the
    /// person who started it, handed the job's record.
    fn job_ends(&self) {
        let (job, handlers) = self
            .engine
            .started
            .borrow()
            .last()
            .cloned()
            .expect("a job was started");
        let record = Engine::record(&job).unwrap_or_else(|why| panic!("{why}"));
        let done = handlers.done.expect("the start names what runs at its end");
        let call = self.host.runtime().job_call(&done, &record).unwrap();
        match self.host.runtime().invoke_action(&me(), call) {
            Ok(tilolang_runtime::InvokeOutcome::Done(_)) => {}
            other => panic!("the job's end runs: {other:?}"),
        }
    }

    fn root_value(&self) -> Value {
        Value::resource(&self.root)
    }

    fn call(&self, name: &str, args: Vec<Value>) -> Value {
        let id = self
            .actions
            .iter()
            .find(|(asked, _)| asked == name)
            .map(|(_, id)| id.clone())
            .unwrap_or_else(|| panic!("no action {name}"));
        match self
            .host
            .runtime()
            .invoke_action(&me(), (id.as_str(), args))
            .unwrap_or_else(|why| panic!("{name} runs: {why}"))
        {
            tilolang_runtime::InvokeOutcome::Done(value) => value,
            other => panic!("{name} answers {other:?}"),
        }
    }

    /// The connection the window saved.
    fn connection(&self) -> tilolang_runtime::ResourceRef {
        let held = self
            .host
            .runtime()
            .read(&self.root, "connections")
            .expect("connections");
        let Value::List(items) = held else {
            panic!("connections read {held:?}");
        };
        items
            .into_iter()
            .find_map(|item| match item {
                Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                _ => None,
            })
            .expect("the saved connection")
    }
}

/// One search entry the workspace publishes, as the app's slot holds it.
fn entries(host: &Host) -> Vec<tilo_resource::search_index::IndexEntry> {
    tilolang_tilo::search::entries(host)
}

#[test]
fn the_open_connections_catalogue_is_findable_and_its_password_is_not() {
    let app = Installed::new();
    // Saves and opens a connection, with a password the window keeps on this device.
    app.call(
        "add_connection",
        vec![
            app.root_value(),
            Value::text("Shop"),
            Value::Int(0),
            Value::text(""),
            Value::Int(0),
            Value::text(""),
            Value::text(""),
            Value::text("/data/shop.sqlite"),
            Value::text("hunter2"),
        ],
    );
    app.job_ends();
    // Runs a statement, so the window holds what it answered.
    app.call(
        "load_starter",
        vec![
            app.root_value(),
            Value::text("Artists"),
            Value::text("SELECT name FROM artists"),
        ],
    );
    app.call("run", vec![app.root_value()]);
    app.job_ends();
    let connection = app.connection();
    app.call(
        "remember_password",
        vec![
            app.root_value(),
            Value::resource(&connection),
            Value::text("hunter2"),
        ],
    );

    let published = entries(&app.host);
    let titled = |title: &str| {
        published
            .iter()
            .find(|entry| entry.title == title)
            .unwrap_or_else(|| {
                panic!(
                    "`{title}` is a hit: {:?}",
                    published
                        .iter()
                        .map(|entry| &entry.title)
                        .collect::<Vec<_>>()
                )
            })
    };

    // A table is found by its name, as a table of the app.
    let table = titled("albums");
    assert_eq!(table.subtitle.as_deref(), Some("TableInfo"));
    assert_eq!(table.group.as_deref(), Some("Database Client"));

    // Every table's columns are found, each saying the table it is in.
    let column = titled("artist_id");
    assert_eq!(column.subtitle.as_deref(), Some("ColumnInfo"));
    assert!(
        column.fields.iter().any(|(text, _)| text == "albums"),
        "the column says its table: {column:?}"
    );
    let name = titled("name");
    assert!(
        name.fields.iter().any(|(text, _)| text == "artists")
            || name.fields.iter().any(|(text, _)| text == "main"),
        "a column of the second table is found too: {name:?}"
    );

    // The statement the window ran is found by what it answered.
    let ran = published
        .iter()
        .find(|entry| entry.subtitle.as_deref() == Some("Run"))
        .unwrap_or_else(|| panic!("the run is a hit: {published:#?}"));
    assert_eq!(
        ran.title, "1 in under a millisecond",
        "a run is titled by its summary"
    );

    // The password this device keeps never reaches the index.
    let held = format!("{published:#?}");
    assert!(
        !held.contains("hunter2"),
        "the password is concealed: {held}"
    );
}

#[test]
fn a_table_hit_opens_on_the_database_window() {
    let app = Installed::new();
    app.call(
        "add_connection",
        vec![
            app.root_value(),
            Value::text("Shop"),
            Value::Int(0),
            Value::text(""),
            Value::Int(0),
            Value::text(""),
            Value::text(""),
            Value::text("/data/shop.sqlite"),
            Value::text(""),
        ],
    );
    app.job_ends();
    let table = entries(&app.host)
        .into_iter()
        .find(|entry| entry.title == "albums" && entry.subtitle.as_deref() == Some("TableInfo"))
        .expect("the table is a hit");
    let action = table.action.expect("the hit opens an action");
    assert_eq!(
        action.action, "open",
        "a hit opens the record that holds it"
    );
    assert_eq!(
        action.resource,
        tilolang_tilo::publish::resource_id(APP, &app.root.id_string()),
        "the database window is the record the table is shown in"
    );
}
