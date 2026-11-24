//! The Backups app's engine, as a package reaches it: `engine.call`, `engine.start`,
//! `engine.status` and `engine.cancel` answered for `so.tilo.backups` by
//! [`tilo_backups_engine`] on this device.
//!
//! The app runs sandboxed with no access to the file system, so it asks by name. A backup, a
//! verification and a restore are jobs: a folder can hold millions of files, and a person can
//! stop the work. No other app is answered by this engine.

use std::sync::{Arc, OnceLock};

use tilolang_tilo::engine::Provider;

/// Register the engine once, at the host's first ask: every later one finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        tilolang_tilo::engine::register(tilo_backups_engine::APP, Arc::new(Backups));
    });
}

/// The registry's entry for the Backups app.
struct Backups;

impl Provider for Backups {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_backups_engine::call(op, input)
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_backups_engine::start(op, input)
    }

    fn status(&self, job: &str) -> Result<String, String> {
        tilo_backups_engine::status(job)
    }

    fn cancel(&self, job: &str) -> bool {
        tilo_backups_engine::cancel(job)
    }
}
