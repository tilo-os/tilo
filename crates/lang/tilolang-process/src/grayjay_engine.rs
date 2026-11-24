#![allow(
    clippy::doc_markdown,
    reason = "YouTube and Grayjay are names, not code: they read as written in prose"
)]

//! YouTube's Grayjay link, as a package reaches it: `engine.call` and `engine.start` answered
//! for `so.tilo.tube` by [`tilolang_grayjay`].
//!
//! The app runs sandboxed with no network and no secret store, so it asks by name. Pairing and
//! syncing touch the network, so they are jobs the app follows by id; the device's Grayjay key
//! is the platform's app-scoped secret, which this engine reads where it is and never hands to
//! the app. No other app is answered by this engine.

use std::sync::{Arc, OnceLock};

use tilolang_tilo::engine::Provider;

/// Register the engine once, at the host's first ask: every later one finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        tilolang_tilo::engine::register(
            tilolang_grayjay::APP,
            Arc::new(Grayjay(tilolang_grayjay::Engine::production())),
        );
    });
}

/// The registry's entry for the YouTube app.
struct Grayjay(tilolang_grayjay::Engine);

impl Provider for Grayjay {
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
