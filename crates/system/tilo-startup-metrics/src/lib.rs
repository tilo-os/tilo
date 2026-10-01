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
use web_time::{Instant, SystemTime};

/// One launch's process/surface timing. Placement remains the compositor's
/// responsibility; this small crate owns only FIFO timing correlation so
/// launch diagnostics can be tested without linking the full desktop.
#[derive(Clone, Debug)]
struct PendingLaunch {
    id: u64,
    key: String,
    requested_at: Instant,
    spawned_at: Option<Instant>,
    surface_id: Option<u64>,
    mapped_at: Option<Instant>,
}

static LAUNCHES: Mutex<Vec<PendingLaunch>> = Mutex::new(Vec::new());

/// The pending-launch table. A panic while holding it leaves plain data, so a
/// poisoned lock is recovered rather than disabling timing for the session.
fn launch_table() -> std::sync::MutexGuard<'static, Vec<PendingLaunch>> {
    LAUNCHES.lock().unwrap_or_else(PoisonError::into_inner)
}
static NEXT_LAUNCH_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Begin timing one user-requested launch. Same-app launches are FIFO.
pub fn launch_requested(key: impl Into<String>) -> u64 {
    let id = NEXT_LAUNCH_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    launch_table().push(PendingLaunch {
        id,
        key: key.into(),
        requested_at: Instant::now(),
        spawned_at: None,
        surface_id: None,
        mapped_at: None,
    });
    id
}

/// Record the first spawned launcher process for the oldest matching request.
pub fn launch_spawned(id: u64, pid: u32) {
    let mut launches = launch_table();
    let Some(launch) = launches
        .iter_mut()
        .find(|launch| launch.id == id && launch.spawned_at.is_none())
    else {
        return;
    };
    let now = Instant::now();
    launch.spawned_at = Some(now);
    let (key, requested_at) = (launch.key.clone(), launch.requested_at);
    // Log after releasing the table: a subscriber must never extend the
    // critical section every launch path contends on.
    drop(launches);
    tracing::info!(
        target: "tilo::launch_latency",
        package_id = %key,
        pid,
        request_to_spawn_us = now.duration_since(requested_at).as_micros(),
        "application launcher spawned"
    );
}

/// Bind the oldest unmatched request to the exact mapped surface.
pub fn launch_mapped(id: u64, surface_id: u64) {
    let mut launches = launch_table();
    let Some(launch) = launches
        .iter_mut()
        .find(|launch| launch.id == id && launch.surface_id.is_none())
    else {
        return;
    };
    let now = Instant::now();
    launch.surface_id = Some(surface_id);
    launch.mapped_at = Some(now);
    let (key, requested_at, spawned_at) =
        (launch.key.clone(), launch.requested_at, launch.spawned_at);
    drop(launches);
    tracing::info!(
        target: "tilo::launch_latency",
        package_id = %key,
        surface_id,
        request_to_map_us = now.duration_since(requested_at).as_micros(),
        spawn_to_map_us = spawned_at.map(|spawned| now.duration_since(spawned).as_micros()),
        "application toplevel mapped"
    );
}

/// Finish timing when the mapped surface supplies its first captured frame.
pub fn launch_first_present(surface_id: u64, frame_seq: u64) {
    if frame_seq == 0 {
        return;
    }
    let mut launches = launch_table();
    let Some(index) = launches
        .iter()
        .position(|launch| launch.surface_id == Some(surface_id))
    else {
        return;
    };
    let launch = launches.remove(index);
    drop(launches);
    let Some(mapped_at) = launch.mapped_at else {
        return;
    };
    let now = Instant::now();
    tracing::info!(
        target: "tilo::launch_latency",
        package_id = %launch.key,
        surface_id,
        frame_seq,
        request_to_first_present_us = now.duration_since(launch.requested_at).as_micros(),
        spawn_to_first_present_us = launch.spawned_at.map(|spawned| now.duration_since(spawned).as_micros()),
        map_to_first_present_us = now.duration_since(mapped_at).as_micros(),
        "application first frame presented"
    );
}

/// Surfaces bound to a launch that has not yet presented its first frame.
#[must_use]
pub fn launches_awaiting_first_present() -> Vec<u64> {
    launch_table()
        .iter()
        .filter_map(|launch| launch.surface_id)
        .collect()
}

/// Retire every unclaimed/mapped metric for one failed launch identity.
pub fn launch_failed(id: u64) {
    launch_table().retain(|launch| launch.id != id);
}

/// Test-facing snapshot of in-flight launch timing. Kept deliberately small
/// so integration tests can prove correlation without reaching into storage.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn launch_snapshot() -> Vec<(u64, String, bool, Option<u64>)> {
    launch_table()
        .iter()
        .map(|launch| {
            (
                launch.id,
                launch.key.clone(),
                launch.spawned_at.is_some(),
                launch.surface_id,
            )
        })
        .collect()
}

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
    let mut events = m.events.lock().unwrap_or_else(PoisonError::into_inner);
    events.push(Event {
        name: name.to_owned(),
        ns,
    });
    // Startup failures often end in a forced reboot, so an exit-only dump is
    // precisely least useful when diagnostics matter most. Replace a small
    // JSON checkpoint after every coarse startup mark; callers deliberately
    // mark phases, not frames, so this never enters a hot render path. The
    // lock is held across the write so concurrent marks cannot interleave
    // their temporary files.
    let payload = serde_json::json!({
        "origin_epoch_ms": m.origin_epoch_ms,
        "events": *events,
    });
    let _ = tilo_files::native::atomic::write_regenerable(
        &m.output_path,
        &serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    );
    drop(events);
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
    fn startup_metrics_are_opt_in_transparent_and_bind_launches_once() {
        // Phase `disabled_by_default`.
        {
            // Without TILO_STARTUP_METRICS_PATH, mark is a no-op — nothing to assert beyond "no panic".
            mark("nothing");
        }
        // Phase `span_passes_through_return_value`.
        {
            assert_eq!(span("computation", || 40 + 2), 42);
        }
        // Phase `same_app_launches_bind_fifo_and_present_once`.
        {
            launch_table().clear();
            let first = launch_requested("dev.tilo.test");
            let second = launch_requested("dev.tilo.test");
            launch_spawned(first, 10);
            launch_spawned(second, 11);
            launch_mapped(first, 100);
            launch_mapped(second, 101);
            let surfaces: Vec<_> = launch_table()
                .iter()
                .map(|launch| launch.surface_id)
                .collect();
            assert_eq!(surfaces, [Some(100), Some(101)]);
            launch_first_present(100, 1);
            assert_eq!(launch_table().len(), 1);
            launch_first_present(100, 2);
            assert_eq!(launch_table().len(), 1);
            launch_failed(second);
        }
    }
}
