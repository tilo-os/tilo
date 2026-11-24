//! The Database Client's engine, as a package reaches it: `engine.call` and `engine.start`
//! answered for `so.tilo.db-client` by the database engine on this device.
//!
//! The app runs sandboxed with no driver, so it asks by name. A request carries the connection
//! and the statement as JSON the app wrote; the answer is JSON the app reads back into its own
//! records. A connection, a catalog and one page of a table answer now; a statement starts as a
//! job whose progress is asked by its id and which can be stopped.

use std::sync::Arc;

/// The one app this device answers database work for.
pub const APP: &str = tilo_db::APP;

/// The device's databases, as the engine registry holds them.
pub struct Databases(tilo_db::Database);

impl tilolang_tilo::engine::Provider for Databases {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        self.0.call(op, input)
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
        self.0.start(op, input)
    }

    fn status(&self, job: &str) -> Result<String, String> {
        self.0.status(job)
    }

    fn cancel(&self, job: &str) -> bool {
        self.0.cancel(job)
    }
}

/// Register this device's databases for the app, once, at the host's start.
pub fn register() {
    tilolang_tilo::engine::register(
        APP,
        Arc::new(Databases(tilo_db::Database::new()))
            as Arc<dyn tilolang_tilo::engine::Provider + Send + Sync>,
    );
}
