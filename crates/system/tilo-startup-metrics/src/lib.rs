//! Lightweight startup-timing capture used to measure the effect of shader precompile, glyph
//! atlas warmth, and other cold-start work.
//!
//! # How it works
//!
//! [`mark`] records `Instant::now()` against the module's `origin` (set at process start).
//! At process exit — or via an explicit [`dump`] call — recorded events are serialised to a
//! JSON file at `TILO_STARTUP_METRICS_PATH`, or `/tmp/tilo-startup-$pid.json` if unset **and**
//! [`init`] was told to enable capture unconditionally.
//!
//! Zero overhead when disabled: `mark` reads a single atomic pointer, and if the env var
//! wasn't set at [`init`] time, the pointer is `None` and the call returns immediately.
//!
//! # Deliberately not `tracing`
//!
//! `tracing` events + spans require the subscriber to be online at the moment of the event.
//! The compositor's `tracing_subscriber::fmt::init()` runs mid-startup, and we want to
//! measure work that happens before that init — a raw `Instant` + `Vec` is unaffected.
//!
//! # Output shape
//!
//! ```json
//! {
//!   "origin_epoch_ms": 1783250000000,
//!   "events": [
//!     { "name": "main-start",           "ns": 0 },
//!     { "name": "compositor-run-start", "ns": 42100000 },
//!     { "name": "font-system-ready",    "ns": 118400000 },
//!     { "name": "first-paint",          "ns": 305200000 }
//!   ]
//! }
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use serde::Serialize;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Instant, SystemTime};

/// Global capture state. Cheap to check on every [`mark`] call: the `OnceLock` reads a
/// single atomic pointer.
static METRICS: OnceLock<Option<Metrics>> = OnceLock::new();

struct Metrics {
    origin: Instant,
    origin_epoch_ms: u128,
    events: Mutex<Vec<Event>>,
    output_path: std::path::PathBuf,
}

#[derive(Serialize, Clone)]
struct Event {
    name: String,
    ns: u128,
}

/// Enable capture. Idempotent — subsequent calls are no-ops. Reads
/// `TILO_STARTUP_METRICS_PATH`: unset leaves the module disabled with every downstream
/// [`mark`] a zero-cost load; set means the file is written on [`dump`] or process exit
/// through a [`ScopedDumper`]. Run this at the earliest possible point in `main` so `origin`
/// is a meaningful "process start" reference.
pub fn init() {
    METRICS.get_or_init(|| {
        let path = std::env::var_os("TILO_STARTUP_METRICS_PATH")?;
        let origin = Instant::now();
        let origin_epoch_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Some(Metrics {
            origin,
            origin_epoch_ms,
            events: Mutex::new(Vec::with_capacity(16)),
            output_path: path.into(),
        })
    });
}

/// Record `name` at "now". Cheap when capture is disabled — `OnceLock::get()` is a single
/// atomic load.
#[inline]
pub fn mark(name: &str) {
    let Some(Some(m)) = METRICS.get() else { return };
    let ns = m.origin.elapsed().as_nanos();
    m.events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Event {
            name: name.to_owned(),
            ns,
        });
}

/// Serialise every recorded event to the configured file. Called automatically at process
/// exit via [`ScopedDumper`]; the explicit version lets tests dump before drop.
pub fn dump() {
    let Some(Some(m)) = METRICS.get() else { return };
    let events = m
        .events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let payload = serde_json::json!({
        "origin_epoch_ms": m.origin_epoch_ms,
        "events": events,
    });
    let _ = std::fs::write(
        &m.output_path,
        serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    );
}

/// RAII guard: dumps recorded events when it goes out of scope. Instantiate at the top of
/// `main` after [`init`].
pub struct ScopedDumper;

impl Drop for ScopedDumper {
    fn drop(&mut self) {
        dump();
    }
}

/// Measure a closure and record the elapsed under `name`. Useful for wrapping a known-slow
/// phase (font-system construction, render-context init) without threading marks around it;
/// records two events, `<name>` at entry and `<name>-end` at exit.
pub fn span<T>(name: &str, f: impl FnOnce() -> T) -> T {
    mark(name);
    let out = f();
    let end_name = format!("{name}-end");
    mark(&end_name);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default() {
        // Without TILO_STARTUP_METRICS_PATH, mark is a no-op — nothing to assert beyond "no panic".
        mark("nothing");
    }

    #[test]
    fn span_passes_through_return_value() {
        assert_eq!(span("computation", || 40 + 2), 42);
    }
}
