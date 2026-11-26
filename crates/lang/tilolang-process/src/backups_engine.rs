//! The Backups app's engine, as a package reaches it: `engine.call`, `engine.start` and
//! `engine.cancel` answered for `so.tilo.backups` by [`tilo_backups_engine`] on this device.
//!
//! The app runs sandboxed with no access to the file system, so it asks by name. A backup, a
//! verification and a restore are jobs: a folder can hold millions of files, and a person can
//! stop the work. They need nothing of the app's own, so the device's shared engine host runs
//! them. No other app is answered by this engine.

use std::sync::{Arc, OnceLock};

use tilolang_engine::{JobOp, JobSink, Needs, Provider};

/// Register the engine once: every later ask finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        tilolang_engine::register(tilo_backups_engine::APP, Arc::new(Backups));
    });
}

/// The registry's entry for the Backups app.
struct Backups;

impl Provider for Backups {
    fn jobs(&self) -> &'static [JobOp] {
        const JOBS: &[JobOp] = &[
            JobOp::new("backup", Needs::NOTHING),
            JobOp::new("verify", Needs::NOTHING),
            JobOp::new("restore", Needs::NOTHING),
        ];
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_backups_engine::call(op, input)
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        tilo_backups_engine::start(job, op, input, crate::engines::told(moved))
    }

    fn cancel(&self, job: &str) -> bool {
        tilo_backups_engine::cancel(job)
    }
}
