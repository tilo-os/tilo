//! The Weather app's engine, as a package reaches it: `engine.call`, `engine.start`,
//! `engine.status` and `engine.cancel` answered for `so.tilo.weather` by [`tilo_weather`] on this
//! device.
//!
//! The app runs sandboxed with no network and no forecast reader, so it asks by name. An update
//! is a job: its places are read away from the frame, and the app takes each place's answer as it
//! lands. No other app is answered by this engine.

use std::sync::{Arc, OnceLock};

use tilolang_tilo::engine::Provider;

/// Register the engine once, at the host's first ask: every later ask finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| tilolang_tilo::engine::register(tilo_weather::APP, Arc::new(Weather)));
}

/// The registry's entry for the Weather app.
struct Weather;

impl Provider for Weather {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_weather::call(op, input)
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_weather::start(op, input)
    }

    fn status(&self, job: &str) -> Result<String, String> {
        tilo_weather::status(job)
    }

    fn cancel(&self, job: &str) -> bool {
        tilo_weather::cancel(job)
    }
}
