//! The database worker: a background thread that owns the live engine handle
//! and runs every blocking operation off the UI thread.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use uuid::Uuid;

use crate::engine::{LiveEngine, QueryCancellation, open_engine};
use crate::model::{ConnectionProfile, DatabaseSchema, QueryResult};

#[derive(Debug)]
pub enum DbRequest {
    /// Open a live engine for this profile (carries the in-memory password)
    Connect(Box<ConnectionProfile>),
    /// Run an arbitrary SQL statement against the open engine.
    Query {
        connection_id: Uuid,
        generation: u64,
        sql: String,
    },
    /// Read one page of a table's rows (Data tab browsing).
    Browse {
        connection_id: Uuid,
        schema: String,
        table: String,
        limit: usize,
        offset: usize,
    },
    /// Ask the thread to exit (used on drop).
    Shutdown,
}

#[derive(Debug)]
pub enum DbReply {
    /// A connection opened and its catalog was introspected.
    Connected {
        connection_id: Uuid,
        name: String,
        schema: DatabaseSchema,
    },
    /// A connection attempt failed (refused socket, bad auth, …).
    ConnectFailed {
        connection_id: Uuid,
        name: String,
        error: String,
    },
    /// A `Run`/`Query` finished. `schema` is `Some` when the statement was
    Queried {
        connection_id: Uuid,
        generation: u64,
        result: Result<QueryResult, String>,
        schema: Option<DatabaseSchema>,
    },
    /// A table page finished loading (Data tab).
    Browsed {
        connection_id: Uuid,
        schema: String,
        table: String,
        offset: usize,
        result: Result<QueryResult, String>,
    },
}

/// Handle to the background worker thread. Owns the request/reply channels and
pub struct DbWorker {
    tx: Sender<DbRequest>,
    rx: Receiver<DbReply>,
    handle: Option<JoinHandle<()>>,
    outstanding: usize,
    active_query: Arc<Mutex<Option<ActiveQuery>>>,
}

#[derive(Clone)]
struct ActiveQuery {
    connection_id: Uuid,
    generation: u64,
    cancellation: QueryCancellation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelQueryResult {
    Requested,
    NotActive,
}

impl DbWorker {
    /// Spawn the worker thread.
    ///
    /// # Panics
    /// Panics if the OS refuses to spawn a thread (extreme resource
    /// exhaustion). Every app's root widget constructor is infallible, so
    /// there is no fallible path back through [`crate::session::Session::new`]
    /// / [`crate::app::DbClientApp::new`] to report a failure to instead.
    #[allow(
        clippy::expect_used,
        reason = "thread spawn failure means the OS is out of resources; there is no fallible path back through Session::new/DbClientApp::new to report it, and no live app to hand back without a worker"
    )]
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<DbRequest>();
        let (res_tx, res_rx) = ui::wake::waking_channel::<DbReply>();
        let active_query = Arc::new(Mutex::new(None));
        let worker_active_query = Arc::clone(&active_query);
        let handle = std::thread::Builder::new()
            .name("db-worker".to_string())
            .spawn(move || worker_loop(&req_rx, &res_tx, &worker_active_query))
            .expect("spawn db worker thread");
        Self {
            tx: req_tx,
            rx: res_rx,
            handle: Some(handle),
            outstanding: 0,
            active_query,
        }
    }

    /// Hand a unit of work to the background thread. Never blocks.
    pub fn request(&mut self, req: DbRequest) {
        self.outstanding += 1;
        if self.tx.send(req).is_err() {
            // Worker thread is gone; the reply will never arrive, so don't
            // leave the count stuck (the app would say it is busy forever).
            self.outstanding = self.outstanding.saturating_sub(1);
        }
    }

    /// Collect every reply that has arrived, without blocking. Call on each
    pub fn drain(&mut self) -> Vec<DbReply> {
        let mut out = Vec::new();
        while let Ok(reply) = self.rx.try_recv() {
            self.outstanding = self.outstanding.saturating_sub(1);
            out.push(reply);
        }
        out
    }

    /// Block until every outstanding request has replied, returning them in
    /// order. Used by a capture and the headless tests to
    /// drive the real engine synchronously; the live event loop uses
    /// [`drain`](Self::drain) instead.
    pub fn block_until_idle(&mut self) -> Vec<DbReply> {
        let mut out = Vec::new();
        while self.outstanding > 0 {
            if let Ok(reply) = self.rx.recv() {
                self.outstanding -= 1;
                out.push(reply);
            } else {
                self.outstanding = 0;
                break;
            }
        }
        out
    }

    /// Whether any request is still in flight.
    pub fn busy(&self) -> bool {
        self.outstanding > 0
    }

    /// Request an interrupt only if the same connection/request generation is
    /// still executing. The selected capability is cloned under the lock and
    /// invoked after releasing it, so a completion/new query cannot be
    /// interrupted by a stale cancellation click.
    pub fn cancel_query(&self, connection_id: Uuid, generation: u64) -> CancelQueryResult {
        let cancellation = self.active_query.lock().ok().and_then(|active| {
            active.as_ref().and_then(|active| {
                (active.connection_id == connection_id && active.generation == generation)
                    .then(|| active.cancellation.clone())
            })
        });
        if let Some(cancellation) = cancellation {
            cancellation.request();
            CancelQueryResult::Requested
        } else {
            CancelQueryResult::NotActive
        }
    }
}

impl Drop for DbWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(DbRequest::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The background loop: owns the single live engine handle and services one
fn worker_loop(
    rx: &Receiver<DbRequest>,
    tx: &ui::wake::WakingSender<DbReply>,
    active_query: &Arc<Mutex<Option<ActiveQuery>>>,
) {
    let mut engine: Option<LiveEngine> = None;
    while let Ok(req) = rx.recv() {
        let reply = match req {
            DbRequest::Shutdown => break,
            DbRequest::Connect(profile) => {
                let connection_id = profile.id;
                let name = profile.name.clone();
                match open_engine(&profile) {
                    Ok(mut eng) => {
                        let schema = eng.introspect().unwrap_or_default();
                        engine = Some(eng);
                        DbReply::Connected {
                            connection_id,
                            name,
                            schema,
                        }
                    }
                    Err(error) => {
                        engine = None;
                        DbReply::ConnectFailed {
                            connection_id,
                            name,
                            error,
                        }
                    }
                }
            }
            DbRequest::Query {
                connection_id,
                generation,
                sql,
            } => match engine.as_mut() {
                Some(eng) => {
                    publish_active_query(active_query, connection_id, generation, eng);
                    let result = eng.run(&sql);
                    clear_active_query(active_query, connection_id, generation);
                    let schema = match &result {
                        Ok(r) if !r.is_select => eng.introspect().ok(),
                        _ => None,
                    };
                    DbReply::Queried {
                        connection_id,
                        generation,
                        result,
                        schema,
                    }
                }
                None => DbReply::Queried {
                    connection_id,
                    generation,
                    result: Err(crate::i18n::t("db-no-connection")),
                    schema: None,
                },
            },
            DbRequest::Browse {
                connection_id,
                schema,
                table,
                limit,
                offset,
            } => {
                let result = match engine.as_mut() {
                    Some(eng) => eng.browse_table(&schema, &table, limit, offset),
                    None => Err(crate::i18n::t("db-no-connection")),
                };
                DbReply::Browsed {
                    connection_id,
                    schema,
                    table,
                    offset,
                    result,
                }
            }
        };
        if tx.send(reply).is_err() {
            break;
        }
    }
}

fn publish_active_query(
    active_query: &Mutex<Option<ActiveQuery>>,
    connection_id: Uuid,
    generation: u64,
    engine: &LiveEngine,
) {
    if let Ok(mut active) = active_query.lock() {
        *active = Some(ActiveQuery {
            connection_id,
            generation,
            cancellation: engine.cancellation_handle(),
        });
    }
}

fn clear_active_query(
    active_query: &Mutex<Option<ActiveQuery>>,
    connection_id: Uuid,
    generation: u64,
) {
    if let Ok(mut active) = active_query.lock()
        && active.as_ref().is_some_and(|active| {
            active.connection_id == connection_id && active.generation == generation
        })
    {
        *active = None;
    }
}
