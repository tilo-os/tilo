//! The Feeds app's engine, as a package reaches it: `engine.call`, `engine.start`,
//! `engine.status` and `engine.cancel` answered for `so.tilo.feeds` by [`tilo_feeds`] on this
//! device.
//!
//! The app runs sandboxed with no network and no feed reader, so it asks by name. A refresh is a
//! job: its feeds are read away from the frame, and the app takes each feed's answer as it lands.
//! No other app is answered by this engine.

use std::sync::{Arc, OnceLock};

use tilolang_tilo::engine::Provider;

/// Register the engine once, at the host's first instance: every later ask finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| tilolang_tilo::engine::register(tilo_feeds::APP, Arc::new(Feeds)));
}

/// The registry's entry for the Feeds app.
struct Feeds;

impl Provider for Feeds {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_feeds::call(op, input)
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_feeds::start(op, input)
    }

    fn status(&self, job: &str) -> Result<String, String> {
        tilo_feeds::status(job)
    }

    fn cancel(&self, job: &str) -> bool {
        tilo_feeds::cancel(job)
    }
}
