//! The Feeds app's engine, as a package reaches it: `engine.call`, `engine.start` and
//! `engine.cancel` answered for `so.tilo.feeds` by [`tilo_feeds`] on this device.
//!
//! The app runs sandboxed with no feed reader, so it asks by name. A refresh is a job: its feeds
//! are read away from the frame, over the network the app was granted, and the app takes each
//! feed's answer as it lands. No other app is answered by this engine.

use std::sync::{Arc, OnceLock};

use tilolang_engine::{JobOp, JobSink, Needs, Provider};

/// Register the engine once: every later ask finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| tilolang_engine::register(tilo_feeds::APP, Arc::new(Feeds)));
}

/// The registry's entry for the Feeds app.
struct Feeds;

impl Provider for Feeds {
    fn jobs(&self) -> &'static [JobOp] {
        // The feeds are read over the app's own network, and each answer is taken by a call.
        const JOBS: &[JobOp] = &[JobOp::new("refresh", Needs::NETWORK_AND_APP_STATE)];
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_feeds::call(op, input)
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        tilo_feeds::start(job, op, input, crate::engines::told(moved))
    }

    fn cancel(&self, job: &str) -> bool {
        tilo_feeds::cancel(job)
    }
}
