//! The Database Client's engine, as a package reaches it: `engine.call` and `engine.start`
//! answered for `so.tilo.db-client` by the database engine on this device.
//!
//! The app runs sandboxed with no driver, so it asks by name. A request carries the connection
//! and the statement as JSON the app wrote; the answer is JSON the app reads back into its own
//! records. A catalog and one page of a table answer now; opening a connection and a statement
//! run as jobs, which can be stopped. A connection holds the database's login, reaches the
//! database over the network, and is what the app's calls read, so its jobs run in the app's own
//! process.

use std::sync::Arc;

use tilolang_tilo::engine::{JobOp, JobSink, Needs, Provider};

/// The one app this device answers database work for.
pub const APP: &str = tilo_db::APP;

/// The device's databases, as the engine registry holds them.
pub struct Databases(tilo_db::Database);

impl Databases {
    /// This device's databases, none open yet.
    #[must_use]
    pub fn new() -> Self {
        Self(tilo_db::Database::new())
    }
}

impl Default for Databases {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for Databases {
    fn jobs(&self) -> &'static [JobOp] {
        const NEEDS: Needs = Needs {
            secret: true,
            network: true,
            app_state: true,
        };
        const JOBS: &[JobOp] = &[JobOp::new("open", NEEDS), JobOp::new("query", NEEDS)];
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        self.0.call(op, input)
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        self.0.start(job, op, input, crate::engines::told(moved))
    }

    fn cancel(&self, job: &str) -> bool {
        self.0.cancel(job)
    }
}

/// Register this device's databases for the app, once, at the host's start.
pub fn register() {
    tilolang_tilo::engine::register(APP, Arc::new(Databases::new()));
}
