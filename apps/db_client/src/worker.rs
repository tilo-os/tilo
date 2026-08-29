//! The database worker: a background thread that owns the live engine handle
//! and runs every blocking operation off the UI thread.
//!
//! Connecting to a remote server (a 10 s socket timeout), introspecting the
//! catalog and running a query all block. Doing them on the render thread
//! would freeze the whole window, so they run here instead: the widget
//! [`request`](DbWorker::request)s work over a channel, the worker executes it
//! and sends a [`DbReply`] back, and the widget [`drain`](DbWorker::drain)s the
//! replies each frame (see [`crate::session`] and the app's `needs_animation_frame`
//! pump). The engine never crosses the thread boundary — it is created and used
//! entirely on the worker — so no live driver handle has to be `Send`/`Sync`.
//!
//! The same channel path is driven synchronously by
//! [`block_until_idle`](DbWorker::block_until_idle) for the hermetic
//! `--shots`/demo build and the headless tests, so those exercise the identical
//! production engine code and only differ in that they wait for the reply
//! instead of pumping frames.

use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use crate::engine::{LiveEngine, open_engine};
use crate::model::{ConnectionProfile, DatabaseSchema, QueryResult};

/// Work handed to the background thread.
#[derive(Debug)]
pub enum DbRequest {
    /// Open a live engine for this profile (carries the in-memory password)
    /// and introspect its catalog.
    Connect(Box<ConnectionProfile>),
    /// Run an arbitrary SQL statement against the open engine.
    Query(String),
    /// Read one page of a table's rows (Data tab browsing).
    Browse {
        table: String,
        limit: usize,
        offset: usize,
    },
    /// Drop the open engine.
    Disconnect,
    /// Ask the thread to exit (used on drop).
    Shutdown,
}

/// A result handed back from the background thread.
#[derive(Debug)]
pub enum DbReply {
    /// A connection opened and its catalog was introspected.
    Connected {
        name: String,
        schema: DatabaseSchema,
    },
    /// A connection attempt failed (refused socket, bad auth, …).
    ConnectFailed { name: String, error: String },
    /// A `Run`/`Query` finished. `schema` is `Some` when the statement was
    /// DML/DDL, so the caller can re-adopt the possibly-changed catalog.
    Queried {
        result: Result<QueryResult, String>,
        schema: Option<DatabaseSchema>,
    },
    /// A table page finished loading (Data tab).
    Browsed { result: Result<QueryResult, String> },
    /// The engine was dropped.
    Disconnected,
}

/// Handle to the background worker thread. Owns the request/reply channels and
/// tracks how many requests are still outstanding so the UI can keep pumping
/// frames until every reply has landed.
pub struct DbWorker {
    tx: Sender<DbRequest>,
    rx: Receiver<DbReply>,
    handle: Option<JoinHandle<()>>,
    outstanding: usize,
}

impl DbWorker {
    /// Spawn the worker thread.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<DbRequest>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<DbReply>();
        let handle = std::thread::Builder::new()
            .name("db-worker".to_string())
            .spawn(move || worker_loop(req_rx, res_tx))
            .expect("spawn db worker thread");
        Self {
            tx: req_tx,
            rx: res_rx,
            handle: Some(handle),
            outstanding: 0,
        }
    }

    /// Hand a unit of work to the background thread. Never blocks.
    pub fn request(&mut self, req: DbRequest) {
        self.outstanding += 1;
        if self.tx.send(req).is_err() {
            // Worker thread is gone; the reply will never arrive, so don't
            // leave the count stuck (which would spin the animation pump).
            self.outstanding = self.outstanding.saturating_sub(1);
        }
    }

    /// Collect every reply that has arrived, without blocking. Call each frame.
    pub fn drain(&mut self) -> Vec<DbReply> {
        let mut out = Vec::new();
        while let Ok(reply) = self.rx.try_recv() {
            self.outstanding = self.outstanding.saturating_sub(1);
            out.push(reply);
        }
        out
    }

    /// Block until every outstanding request has replied, returning them in
    /// order. Used by the hermetic demo/shots build and the headless tests to
    /// drive the real engine synchronously; the live event loop uses
    /// [`drain`](Self::drain) instead.
    pub fn block_until_idle(&mut self) -> Vec<DbReply> {
        let mut out = Vec::new();
        while self.outstanding > 0 {
            if let Ok(reply) = self.rx.recv() {
                self.outstanding -= 1;
                out.push(reply);
            } else {
                // Sender hung up; nothing more will arrive.
                self.outstanding = 0;
                break;
            }
        }
        out
    }

    /// Whether any request is still in flight (drives `needs_animation_frame`).
    pub fn busy(&self) -> bool {
        self.outstanding > 0
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
/// request at a time.
fn worker_loop(rx: Receiver<DbRequest>, tx: Sender<DbReply>) {
    let mut engine: Option<LiveEngine> = None;
    while let Ok(req) = rx.recv() {
        let reply = match req {
            DbRequest::Shutdown => break,
            DbRequest::Connect(profile) => {
                let name = profile.name.clone();
                match open_engine(&profile) {
                    Ok(mut eng) => {
                        let schema = eng.introspect().unwrap_or_default();
                        engine = Some(eng);
                        DbReply::Connected { name, schema }
                    }
                    Err(error) => {
                        engine = None;
                        DbReply::ConnectFailed { name, error }
                    }
                }
            }
            DbRequest::Query(sql) => match engine.as_mut() {
                Some(eng) => {
                    let result = eng.run(&sql);
                    // A DML/DDL statement may have changed the catalog; a
                    // failed or row-returning statement leaves it untouched.
                    let schema = match &result {
                        Ok(r) if !r.is_select => eng.introspect().ok(),
                        _ => None,
                    };
                    DbReply::Queried { result, schema }
                }
                None => DbReply::Queried {
                    result: Err("No live connection".to_string()),
                    schema: None,
                },
            },
            DbRequest::Browse {
                table,
                limit,
                offset,
            } => {
                let result = match engine.as_mut() {
                    Some(eng) => eng.browse_table(&table, limit, offset),
                    None => Err("No live connection".to_string()),
                };
                DbReply::Browsed { result }
            }
            DbRequest::Disconnect => {
                engine = None;
                DbReply::Disconnected
            }
        };
        if tx.send(reply).is_err() {
            // UI side is gone.
            break;
        }
    }
}
