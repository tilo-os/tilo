//! A database's engine: the connections a device opens to `SQLite`, `PostgreSQL` and `MySQL`, the
//! catalog of each, and the statements run on it.
//!
//! The drivers' decode arithmetic is bounded by their wire formats (a column's digits, a row's
//! cells), and a registry guard is held while its map is read and changed, so the two lints that
//! name those shapes are allowed here and nowhere else.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "the drivers' arithmetic is bounded by their wire formats, and a registry guard is held while its map is read and changed"
)]
//!
//! An app runs sandboxed with no driver of its own, so it asks the device by name: `engine.call`
//! for a connection, a catalog or one page of a table, and `engine.start`/
//! `engine.cancel` for a statement that may take a while and can be stopped. Each answer is JSON
//! the app reads into its own records ([`model::ResultAnswer`], [`model::DatabaseSchema`]), so
//! one engine serves every app that runs SQL and none of the words are the engine's.
//!
//! A connection's password reaches the engine with the request and is kept nowhere else: the
//! workspace's record names the connection, and the device's keyring holds its password.

pub mod engine;
mod engine_mysql;
mod engine_pg;
pub mod model;
pub mod sample;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Deserialize;

use crate::engine::{LiveEngine, QueryCancellation, open_engine};
use crate::model::{Profile, ResultAnswer};

/// The package whose engine work this device answers.
pub const APP: &str = "so.tilo.db-client";

/// The most rows a statement's result keeps; a larger result is read by querying it in parts.
pub const MAX_QUERY_ROWS: usize = 1_000;

/// The device's databases, as the session's engine registry holds them: every open connection
/// and every statement running as a job. A handle is cheap to copy, so a job keeps the one it
/// was started from without outliving or holding the registry's lock.
#[derive(Clone, Default)]
pub struct Database(Arc<Inner>);

#[derive(Default)]
struct Inner {
    live: Mutex<Live>,
    next_connection: AtomicU64,
}

#[derive(Default)]
struct Live {
    connections: HashMap<u64, Arc<Mutex<LiveEngine>>>,
    jobs: HashMap<String, Job>,
}

/// Where a job tells each move it makes: the state's word (`running`, `done`, `failed`,
/// `cancelled`) and the job's own fields (`result`, `error`).
pub type Moved = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// A statement running off the app's thread, telling each move it makes.
struct Job {
    moved: Moved,
    state: JobState,
    /// What the statement answered, as JSON, once it is `done`.
    answer: String,
    /// Why it did not run or finish, once it is `failed`.
    error: String,
    /// The engine's own interruption of exactly this statement.
    cancel: Option<QueryCancellation>,
    /// Set when the app stops the job.
    stopped: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Job {
    /// Tell how the job stands now.
    fn tell(&self) {
        (self.moved)(
            self.state.key(),
            serde_json::json!({ "result": self.answer, "error": self.error }),
        );
    }
}

impl JobState {
    fn key(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl Database {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        self.0.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One request answered now: `open`, `catalog`, `tables`, `columns`, `keys`, `indexes`,
    /// `browse` and `close`.
    ///
    /// # Errors
    ///
    /// The op is unknown, the input does not read, the connection is gone, or the driver
    /// refused.
    pub fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "open" => self.open(input),
            "catalog" => self.catalog(input),
            "tables" => self.tables(input),
            "columns" => self.columns(input),
            "keys" => self.keys(input),
            "indexes" => self.indexes(input),
            "browse" => self.browse(input),
            "close" => self.close(input),
            other => Err(format!("`{other}` is no database op this device runs")),
        }
    }

    /// One request started as the job `id`: a connection being opened, which may wait on a
    /// server, or a statement, which may run long. Each move is told to `moved`.
    ///
    /// # Errors
    ///
    /// The op is unknown, the input does not read, the engine is not one this device runs, or
    /// the connection is gone.
    pub fn start(&self, id: &str, op: &str, input: &str, moved: Moved) -> Result<(), String> {
        match op {
            "open" => {
                let profile = open_profile(input)?;
                self.job(id, moved, "tilo-db-open", move |database, job| {
                    run_open(&database, &job, &profile);
                })
            }
            "query" => {
                #[derive(Deserialize)]
                struct Ask {
                    connection: u64,
                    sql: String,
                }
                let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
                let engine = self.engine_of(ask.connection)?;
                self.job(id, moved, "tilo-db-query", move |database, job| {
                    run_job(&database, &job, &engine, &ask.sql);
                })
            }
            other => Err(format!("`{other}` is no database job this device runs")),
        }
    }

    /// Runs `work` on a thread of its own, with the job it was given already in the register, so
    /// the app never waits on the device.
    fn job(
        &self,
        id: &str,
        moved: Moved,
        thread: &str,
        work: impl FnOnce(Database, String) + Send + 'static,
    ) -> Result<(), String> {
        let job = id.to_owned();
        {
            let mut live = self.live();
            let held = Job {
                moved,
                state: JobState::Running,
                answer: String::new(),
                error: String::new(),
                cancel: None,
                stopped: Arc::new(AtomicBool::new(false)),
            };
            held.tell();
            live.jobs.insert(job.clone(), held);
        }
        let held = job.clone();
        let database = self.clone();
        let spawned = std::thread::Builder::new()
            .name(thread.to_owned())
            .spawn(move || work(database, held));
        if let Err(error) = spawned {
            let mut live = self.live();
            live.jobs.remove(&job);
            return Err(format!("the job could not start: {error}"));
        }
        Ok(())
    }

    /// Stop `job`: false when this device does not run it or it had finished.
    pub fn cancel(&self, job: &str) -> bool {
        let mut live = self.live();
        let Some(held) = live.jobs.get_mut(job) else {
            return false;
        };
        if held.state != JobState::Running {
            return false;
        }
        held.stopped.store(true, Ordering::Relaxed);
        if let Some(cancel) = &held.cancel {
            cancel.request();
        }
        held.state = JobState::Cancelled;
        held.tell();
        true
    }

    fn open(&self, input: &str) -> Result<String, String> {
        let profile = open_profile(input)?;
        let opened = open_engine(&profile)?;
        let id = self.0.next_connection.fetch_add(1, Ordering::Relaxed) + 1;
        self.live()
            .connections
            .insert(id, Arc::new(Mutex::new(opened)));
        Ok(serde_json::json!({ "connection": id }).to_string())
    }

    fn engine_of(&self, connection: u64) -> Result<Arc<Mutex<LiveEngine>>, String> {
        let live = self.live();
        live.connections
            .get(&connection)
            .cloned()
            .ok_or_else(|| "connection is no longer open".to_string())
    }

    /// The schema names of the open connection's catalog.
    fn catalog(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            connection: u64,
        }
        let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
        let engine = self.engine_of(ask.connection)?;
        let mut engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
        let schema = engine.introspect()?;
        let names: Vec<&str> = schema.schemas.iter().map(|one| one.name.as_str()).collect();
        serde_json::to_string(&names).map_err(|error| error.to_string())
    }

    /// Every table and view of the open connection's catalog.
    fn tables(&self, input: &str) -> Result<String, String> {
        let connection = connection_of(input)?;
        let engine = self.engine_of(connection)?;
        let mut engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
        let schema = engine.introspect()?;
        let rows: Vec<serde_json::Value> = schema
            .schemas
            .iter()
            .flat_map(|one| one.tables.iter())
            .map(|table| {
                serde_json::json!({
                    "schema": table.schema,
                    "name": table.name,
                    "kind": match table.kind {
                        crate::model::RelationKind::Table => "table",
                        crate::model::RelationKind::View => "view",
                    },
                    "row_count": table.row_count,
                })
            })
            .collect();
        serde_json::to_string(&rows).map_err(|error| error.to_string())
    }

    /// One table's columns.
    fn columns(&self, input: &str) -> Result<String, String> {
        let (connection, schema, table) = table_of(input)?;
        let engine = self.engine_of(connection)?;
        let mut engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
        let catalog = engine.introspect()?;
        let found = catalog
            .find_relation(&schema, &table)
            .ok_or_else(|| format!("`{schema}.{table}` is no longer in the catalog"))?;
        let rows: Vec<serde_json::Value> = found
            .columns
            .iter()
            .map(|column| {
                serde_json::json!({
                    "name": column.name,
                    "decl_type": column.decl_type,
                    "nullable": !column.not_null,
                    "primary_key": column.primary_key,
                    "default_value": column.default_value,
                })
            })
            .collect();
        serde_json::to_string(&rows).map_err(|error| error.to_string())
    }

    /// One table's foreign keys.
    fn keys(&self, input: &str) -> Result<String, String> {
        let (connection, schema, table) = table_of(input)?;
        let engine = self.engine_of(connection)?;
        let mut engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
        let catalog = engine.introspect()?;
        let found = catalog
            .find_relation(&schema, &table)
            .ok_or_else(|| format!("`{schema}.{table}` is no longer in the catalog"))?;
        let rows: Vec<serde_json::Value> = found
            .foreign_keys
            .iter()
            .map(|key| {
                serde_json::json!({
                    "column": key.column,
                    "ref_table": key.ref_table,
                    "ref_column": key.ref_column,
                })
            })
            .collect();
        serde_json::to_string(&rows).map_err(|error| error.to_string())
    }

    /// One table's indexes, their columns written as one text.
    fn indexes(&self, input: &str) -> Result<String, String> {
        let (connection, schema, table) = table_of(input)?;
        let engine = self.engine_of(connection)?;
        let mut engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
        let catalog = engine.introspect()?;
        let found = catalog
            .find_relation(&schema, &table)
            .ok_or_else(|| format!("`{schema}.{table}` is no longer in the catalog"))?;
        let rows: Vec<serde_json::Value> = found
            .indexes
            .iter()
            .map(|index| {
                serde_json::json!({
                    "name": index.name,
                    "unique": index.unique,
                    "columns": index.columns.join(", "),
                })
            })
            .collect();
        serde_json::to_string(&rows).map_err(|error| error.to_string())
    }

    fn browse(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            connection: u64,
            schema: String,
            table: String,
            #[serde(default = "default_page")]
            limit: usize,
            #[serde(default)]
            offset: usize,
        }
        fn default_page() -> usize {
            100
        }
        let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
        let engine = self.engine_of(ask.connection)?;
        let mut engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
        let result: ResultAnswer = engine
            .browse_table(&ask.schema, &ask.table, ask.limit, ask.offset)?
            .answer();
        serde_json::to_string(&result).map_err(|error| error.to_string())
    }

    fn close(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            connection: u64,
        }
        let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
        self.live().connections.remove(&ask.connection);
        Ok("{}".to_owned())
    }
}

/// Runs one statement on its own thread and tells what it answered.
fn run_job(database: &Database, job: &str, engine: &Arc<Mutex<LiveEngine>>, sql: &str) {
    let stopped = {
        let live = database.live();
        live.jobs.get(job).map(|held| Arc::clone(&held.stopped))
    };
    let mut engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
    // The capability belongs to exactly this statement: it is taken before the statement
    // starts, so a stop asked for in that instant still reaches it.
    let cancel = engine.cancellation_handle();
    {
        let mut live = database.live();
        if let Some(held) = live.jobs.get_mut(job) {
            held.cancel = Some(cancel);
        }
    }
    let stopped = stopped.unwrap_or_default();
    if stopped.load(Ordering::Relaxed) {
        finish(
            database,
            job,
            JobState::Cancelled,
            String::new(),
            String::new(),
        );
        return;
    }
    match engine.run(sql) {
        Ok(result) => {
            let answer = serde_json::to_string(&result.answer()).unwrap_or_default();
            let state = if stopped.load(Ordering::Relaxed) {
                JobState::Cancelled
            } else {
                JobState::Done
            };
            finish(database, job, state, answer, String::new());
        }
        Err(error) => {
            let state = if stopped.load(Ordering::Relaxed) {
                JobState::Cancelled
            } else {
                JobState::Failed
            };
            finish(database, job, state, String::new(), error);
        }
    }
}

/// The connection a request names.
fn connection_of(input: &str) -> Result<u64, String> {
    #[derive(Deserialize)]
    struct Ask {
        connection: u64,
    }
    let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
    Ok(ask.connection)
}

/// The connection and the table a request names.
fn table_of(input: &str) -> Result<(u64, String, String), String> {
    #[derive(Deserialize)]
    struct Ask {
        connection: u64,
        schema: String,
        table: String,
    }
    let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
    Ok((ask.connection, ask.schema, ask.table))
}

/// Opens a connection on its own thread and keeps its catalog for the app.
fn run_open(database: &Database, job: &str, profile: &Profile) {
    match open_engine(profile) {
        Ok(mut opened) => match opened.introspect() {
            Ok(_) => {
                let id = database.0.next_connection.fetch_add(1, Ordering::Relaxed) + 1;
                database
                    .live()
                    .connections
                    .insert(id, Arc::new(Mutex::new(opened)));
                let answer = serde_json::json!({ "connection": id }).to_string();
                finish(database, job, JobState::Done, answer, String::new());
            }
            Err(error) => finish(database, job, JobState::Failed, String::new(), error),
        },
        Err(error) => finish(database, job, JobState::Failed, String::new(), error),
    }
}

/// The connection a request names, as the engine opens it.
fn open_profile(input: &str) -> Result<Profile, String> {
    #[derive(Deserialize)]
    struct Ask {
        engine: String,
        #[serde(default)]
        host: String,
        #[serde(default)]
        port: u16,
        #[serde(default)]
        database: String,
        #[serde(default)]
        user: String,
        #[serde(default)]
        password: String,
        #[serde(default)]
        file_path: String,
        #[serde(default)]
        sample: bool,
    }
    let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
    let engine = crate::model::Engine::from_key(&ask.engine)
        .ok_or_else(|| format!("There is no engine called {}.", ask.engine))?;
    Ok(Profile {
        engine,
        host: ask.host,
        port: ask.port,
        database: ask.database,
        user: ask.user,
        password: ask.password,
        file_path: ask.file_path,
        sample: ask.sample,
    })
}

fn finish(database: &Database, job: &str, state: JobState, answer: String, error: String) {
    let mut live = database.live();
    if let Some(mut held) = live.jobs.remove(job) {
        held.state = state;
        held.answer = answer;
        held.error = error;
        held.cancel = None;
        held.tell();
    }
}
