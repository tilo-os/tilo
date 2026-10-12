//! Lightweight startup-timing capture used to measure the effect of shader precompile, glyph
//! atlas warmth, and other cold-start work.
//!
//! # How it works
//!
//! [`mark`] records `Instant::now()` against the module's `origin` (set at process start).
//! Recorded events are serialised to a JSON file at `TILO_STARTUP_METRICS_PATH` as they
//! come — by a background thread once [`start_writer`] ran, so marking never waits for the
//! disk — and in full at process exit or an explicit [`dump`] call.
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
//!   "origin_epoch_us": 1783250000000123,
//!   "launcher": [
//!     { "name": "launcher-start",     "epoch_us": 1783249999880000 },
//!     { "name": "sandbox-argv-ready", "epoch_us": 1783249999930000 },
//!     { "name": "sandbox-entered",    "epoch_us": 1783249999950000 },
//!     { "name": "sandbox-exec",       "epoch_us": 1783249999955000 }
//!   ],
//!   "events": [
//!     { "name": "main-start",           "ns": 0 },
//!     { "name": "compositor-run-start", "ns": 42100000 },
//!     { "name": "font-system-ready",    "ns": 118400000 },
//!     { "name": "first-paint",          "ns": 305200000 }
//!   ]
//! }
//! ```
//!
//! `origin_epoch_us` is `origin_epoch_ms` at microsecond precision. `launcher` covers
//! the time before `main`, which the process's own clock cannot see: the phase marks of
//! the `tilo-launcher` process that set up this app's sandbox, in Unix-epoch
//! microseconds ([`handoff_mark`], handed over in [`LAUNCHER_MARKS_ENV`]). The launcher's
//! last own mark, `sandbox-argv-ready`, is taken just before it spawns bubblewrap; the
//! sandbox shim adds `sandbox-entered` when it starts inside the sandbox and
//! `sandbox-exec` just before it executes the app ([`handoff_entry`]), so
//! `origin_epoch_us` minus `sandbox-exec` is the dynamic loader and library
//! constructors. (The app cannot read `/proc` under Landlock, so it cannot time its own
//! exec.)

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use serde::Serialize;
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use web_time::Instant;

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
    /// Where the compositor's dispatch got to before the spawn
    /// ([`launch_phase`]), in order.
    phases: Vec<(&'static str, Instant)>,
}

/// At most this many dispatch phases are kept per launch.
const LAUNCH_PHASES_MAX: usize = 16;

static LAUNCHES: Mutex<Vec<PendingLaunch>> = Mutex::new(Vec::new());

/// The `tracing` target of the compositor's per-launch timing lines.
pub const LAUNCH_LATENCY_TARGET: &str = "tilo::launch_latency";

/// The compositor's log filter directives for a `RUST_LOG` of `rust_log`: that
/// filter (errors only when unset, as `tracing_subscriber::fmt::init` has it),
/// with the launch-latency lines always on. They are three lines per app open,
/// and the only record of how long the compositor and the launcher took before
/// the app's own clock starts; a filter that names the target keeps its choice.
#[must_use]
pub fn log_directives(rust_log: Option<&str>) -> String {
    let base = rust_log.map(str::trim).filter(|value| !value.is_empty());
    match base {
        Some(value) if value.contains(LAUNCH_LATENCY_TARGET) => value.to_owned(),
        Some(value) => format!("{value},{LAUNCH_LATENCY_TARGET}=info"),
        None => format!("error,{LAUNCH_LATENCY_TARGET}=info"),
    }
}

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
        phases: Vec::new(),
    });
    id
}

/// Note that the dispatch of the oldest unspawned launch of `key` reached
/// `phase` now. [`launch_spawned`] reports the phases with the spawn, so the
/// compositor's share of an open is split where it is spent. A launch nobody
/// requested (no [`launch_requested`]) is not timed.
pub fn launch_phase(key: &str, phase: &'static str) {
    let mut launches = launch_table();
    if let Some(launch) = launches
        .iter_mut()
        .find(|launch| launch.key == key && launch.spawned_at.is_none())
        && launch.phases.len() < LAUNCH_PHASES_MAX
    {
        launch.phases.push((phase, Instant::now()));
    }
}

/// `phase:ms,...`: each phase's interval, ending at that phase and starting at
/// the one before (the request for the first), then `registered`, the rest of
/// the way to `spawned_at`. Milliseconds to 0.1.
fn format_launch_phases(
    requested_at: Instant,
    phases: &[(&'static str, Instant)],
    spawned_at: Instant,
) -> String {
    let mut previous = requested_at;
    let mut out = Vec::with_capacity(phases.len().saturating_add(1));
    for (name, at) in phases
        .iter()
        .copied()
        .chain(std::iter::once(("registered", spawned_at)))
    {
        let tenths = at.saturating_duration_since(previous).as_micros() / 100;
        out.push(format!("{name}:{}.{}", tenths / 10, tenths % 10));
        previous = previous.max(at);
    }
    out.join(",")
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
    let phases = std::mem::take(&mut launch.phases);
    // Log after releasing the table: a subscriber must never extend the
    // critical section every launch path contends on.
    drop(launches);
    tracing::info!(
        target: LAUNCH_LATENCY_TARGET,
        package_id = %key,
        pid,
        request_to_spawn_us = now.duration_since(requested_at).as_micros(),
        dispatch_phases = %format_launch_phases(requested_at, &phases, now),
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
        target: LAUNCH_LATENCY_TARGET,
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
        target: LAUNCH_LATENCY_TARGET,
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
    origin_epoch_us: u128,
    launcher: Vec<HandoffEvent>,
    events: Mutex<Vec<Event>>,
    output_path: std::path::PathBuf,
    /// Serialises writes of the file and holds how many events it has, so an
    /// older snapshot never replaces a newer one. Events are only appended.
    file: Mutex<Option<usize>>,
    /// The background writer, once [`start_writer`] ran; `None` when its thread
    /// could not start.
    writer: OnceLock<Option<&'static Writer>>,
}

/// Wakes the background writer: set when events arrived that it has not
/// written yet.
#[derive(Default)]
struct Writer {
    pending: Mutex<bool>,
    wake: Condvar,
}

/// How long the background writer gathers marks after writing before it
/// writes again: a burst of marks costs one write per interval, not one each.
const WRITE_SPACING: std::time::Duration = std::time::Duration::from_millis(50);

impl Metrics {
    /// Start capturing into `output_path` now. `launcher_marks` is the value of
    /// [`LAUNCHER_MARKS_ENV`], when the process that started this one set it.
    fn start(output_path: std::path::PathBuf, launcher_marks: Option<&str>) -> Self {
        let origin = Instant::now();
        let origin_epoch_us = u128::from(tilo_clock::unix_now_ns()) / 1_000;
        Self {
            origin,
            origin_epoch_ms: origin_epoch_us / 1_000,
            origin_epoch_us,
            launcher: launcher_marks.map(parse_handoff).unwrap_or_default(),
            events: Mutex::new(Vec::with_capacity(16)),
            output_path,
            file: Mutex::new(None),
            writer: OnceLock::new(),
        }
    }

    /// Replace the file with every event so far, unless it already holds
    /// them. Atomic: a reader sees the previous file or this one, whole.
    fn write_snapshot(&self) {
        let mut on_disk = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        let events = self
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if on_disk.is_some_and(|written| written >= events.len()) {
            return;
        }
        let payload = self.payload(&events);
        if tilo_files::native::atomic::write_regenerable(
            &self.output_path,
            &serde_json::to_vec_pretty(&payload).unwrap_or_default(),
        )
        .is_ok()
        {
            *on_disk = Some(events.len());
        }
    }

    /// Have the background writer write soon; `false` when there is none, and
    /// the caller writes itself.
    fn wake_writer(&self) -> bool {
        let Some(Some(writer)) = self.writer.get() else {
            return false;
        };
        *writer
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        writer.wake.notify_one();
        true
    }

    fn payload(&self, events: &[Event]) -> serde_json::Value {
        serde_json::json!({
            "origin_epoch_ms": self.origin_epoch_ms,
            "origin_epoch_us": self.origin_epoch_us,
            "launcher": self.launcher,
            "events": events,
        })
    }
}

/// The background writer's loop: write whenever marks arrived, then gather the
/// next ones for [`WRITE_SPACING`].
fn write_marks(metrics: &'static Metrics, writer: &'static Writer) -> ! {
    loop {
        let mut pending = writer
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while !*pending {
            pending = writer
                .wake
                .wait(pending)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *pending = false;
        drop(pending);
        metrics.write_snapshot();
        std::thread::sleep(WRITE_SPACING);
    }
}

#[derive(Serialize, Clone)]
struct Event {
    name: String,
    ns: u128,
}

/// The variable `tilo-launcher` sets in the sandbox it starts, holding its own
/// phase marks for the app's startup file: `name@epoch_us` entries joined by `,`.
pub const LAUNCHER_MARKS_ENV: &str = "TILO_STARTUP_LAUNCHER_MARKS";

/// At most this many handed-over marks are kept; the rest are dropped.
const HANDOFF_MAX_MARKS: usize = 32;
const HANDOFF_MAX_NAME: usize = 48;

/// One phase mark of the process that started this one, on the shared wall clock.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
struct HandoffEvent {
    name: String,
    epoch_us: u128,
}

fn handoff_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= HANDOFF_MAX_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The marks in a [`LAUNCHER_MARKS_ENV`] value. Malformed entries are dropped:
/// these are diagnostics, never a reason to fail or alter a launch.
fn parse_handoff(value: &str) -> Vec<HandoffEvent> {
    value
        .split(',')
        .filter_map(|entry| {
            let (name, epoch_us) = entry.split_once('@')?;
            let epoch_us = epoch_us.parse().ok()?;
            handoff_name_ok(name).then(|| HandoffEvent {
                name: name.to_owned(),
                epoch_us,
            })
        })
        .take(HANDOFF_MAX_MARKS)
        .collect()
}

fn encode_handoff(marks: &[HandoffEvent]) -> String {
    marks
        .iter()
        .map(|mark| format!("{}@{}", mark.name, mark.epoch_us))
        .collect::<Vec<_>>()
        .join(",")
}

/// Marks of a process that starts the measured one. `None` when the shell did
/// not ask for startup metrics.
static HANDOFF: OnceLock<Option<Mutex<Vec<HandoffEvent>>>> = OnceLock::new();

/// Record `name` now in a process that launches an app (`tilo-launcher`); the
/// marks reach the app through [`handoff_env_value`] and land in its startup
/// file. A no-op unless `TILO_STARTUP_METRICS_PATH` is set, as the shell sets
/// it for every app it launches.
pub fn handoff_mark(name: &'static str) {
    let enabled = HANDOFF.get_or_init(|| {
        std::env::var_os("TILO_STARTUP_METRICS_PATH").map(|_| Mutex::new(Vec::new()))
    });
    let Some(marks) = enabled else { return };
    if !handoff_name_ok(name) {
        return;
    }
    let epoch_us = u128::from(tilo_clock::unix_now_ns()) / 1_000;
    let mut marks = marks.lock().unwrap_or_else(PoisonError::into_inner);
    if marks.len() < HANDOFF_MAX_MARKS {
        marks.push(HandoffEvent {
            name: name.to_owned(),
            epoch_us,
        });
    }
}

/// The [`LAUNCHER_MARKS_ENV`] value holding every [`handoff_mark`] so far;
/// `None` when there is none.
#[must_use]
pub fn handoff_env_value() -> Option<String> {
    let marks = HANDOFF.get()?.as_ref()?;
    let marks = marks.lock().unwrap_or_else(PoisonError::into_inner);
    (!marks.is_empty()).then(|| encode_handoff(&marks))
}

/// One [`LAUNCHER_MARKS_ENV`] entry for `name` at now, for a process between
/// the launcher and the app (the sandbox shim) to append to the value it
/// passes on. `None` for a name the app would drop.
#[must_use]
pub fn handoff_entry(name: &str) -> Option<String> {
    handoff_name_ok(name).then(|| {
        let epoch_us = u128::from(tilo_clock::unix_now_ns()) / 1_000;
        format!("{name}@{epoch_us}")
    })
}

/// Whether the startup is still being captured: until [`close_startup`].
static STARTUP_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// [`mark`], only until the first frame is presented ([`close_startup`]): for
/// code that also runs on every later rebuild, which must not rewrite the
/// startup file each time.
#[inline]
pub fn startup_mark(name: &str) {
    if STARTUP_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
        mark(name);
    }
}

/// End the startup: later [`startup_mark`]s record nothing.
pub fn close_startup() {
    STARTUP_OPEN.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Enable capture. Idempotent — subsequent calls are no-ops. Reads
/// `TILO_STARTUP_METRICS_PATH`: unset leaves the module disabled with every downstream
/// [`mark`] a zero-cost load; set means the file is written as marks come (see [`mark`]),
/// on [`dump`] and at process exit through a [`ScopedDumper`]. Run this at the earliest
/// possible point in `main` so `origin`
/// is a meaningful "process start" reference.
pub fn init() {
    METRICS.get_or_init(|| {
        let path = std::env::var_os("TILO_STARTUP_METRICS_PATH")?;
        let launcher = std::env::var(LAUNCHER_MARKS_ENV).ok();
        Some(Metrics::start(path.into(), launcher.as_deref()))
    });
}

/// Record `name` at "now". Cheap when capture is disabled — `OnceLock::get()` is a single
/// atomic load.
///
/// The file is replaced with every mark so far — a checkpoint, since startup failures
/// often end in a forced reboot and an exit-only dump is least useful exactly then. Once
/// [`start_writer`] ran, a background thread does that, at most once per
/// [`WRITE_SPACING`], so the marking thread never waits for the disk or for another
/// thread's write; before, the marking thread writes.
#[inline]
pub fn mark(name: &str) {
    if let Some(Some(m)) = METRICS.get() {
        m.record(name);
    }
}

/// Write marks from a background thread from now on (see [`mark`]). Call it once the
/// process may have threads: the compositor after its single-threaded start, an app once
/// its runtime attachment (which can exec) is done. Idempotent; where no thread can start,
/// marks go on being written by the thread that marks.
pub fn start_writer() {
    if let Some(Some(m)) = METRICS.get() {
        m.start_writer();
    }
}

impl Metrics {
    fn record(&self, name: &str) {
        let ns = self.origin.elapsed().as_nanos();
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Event {
                name: name.to_owned(),
                ns,
            });
        if !self.wake_writer() {
            self.write_snapshot();
        }
    }

    fn start_writer(&'static self) {
        self.writer.get_or_init(|| {
            // One per process, for the life of the process.
            let writer: &'static Writer = Box::leak(Box::default());
            std::thread::Builder::new()
                .name("tilo-startup-marks".into())
                .spawn(move || write_marks(self, writer))
                .ok()
                .map(|_| writer)
        });
        // Marks made before the thread existed reach the file now, not with the next one.
        self.wake_writer();
    }
}

/// Serialise every recorded event to the configured file, now, on this thread. Called
/// automatically at process exit via [`ScopedDumper`] and after an app's first frame;
/// the explicit version lets tests dump before drop.
pub fn dump() {
    let Some(Some(m)) = METRICS.get() else { return };
    m.write_snapshot();
}

/// One line describing this process's startup so far, for a log reader:
/// `APP-STARTUP app=<app> total_ms=<ms> phases=<mark>:<ms>,...`. `total_ms`
/// is the time to the latest mark; each phase is the time from the previous
/// mark to the one it names, and phases under a millisecond are left out.
/// `None` while capture is disabled.
pub fn summary_line(app: &str) -> Option<String> {
    let Some(Some(m)) = METRICS.get() else {
        return None;
    };
    let events = m
        .events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|event| (event.name.clone(), event.ns))
        .collect::<Vec<_>>();
    Some(format_summary(app, &events))
}

fn format_summary(app: &str, events: &[(String, u128)]) -> String {
    const MS: u128 = 1_000_000;
    let mut previous = 0u128;
    let mut phases = Vec::new();
    for (name, ns) in events {
        let elapsed = ns.saturating_sub(previous);
        previous = previous.max(*ns);
        if elapsed >= MS {
            phases.push(format!("{name}:{}", elapsed / MS));
        }
    }
    format!(
        "APP-STARTUP app={app} total_ms={} phases={}",
        previous / MS,
        phases.join(",")
    )
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
        // Phase `summary_names_the_slow_phases`.
        {
            let events = [
                ("app-launch-start".to_owned(), 3_000),
                ("app-construct".to_owned(), 7_500_000),
                ("app-construct-end".to_owned(), 8_700_000_000),
                ("primary-window-created".to_owned(), 8_700_400_000),
                ("first-frame-presented".to_owned(), 8_842_000_000),
            ];
            assert_eq!(
                format_summary("so.tilo.notes", &events),
                "APP-STARTUP app=so.tilo.notes total_ms=8842 \
                 phases=app-construct:7,app-construct-end:8692,first-frame-presented:141"
            );
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
        // Phase `dispatch_phases_belong_to_the_unspawned_launch`.
        {
            launch_table().clear();
            let id = launch_requested("dev.tilo.phases");
            launch_phase("dev.tilo.phases", "preflight");
            launch_phase("dev.tilo.other", "preflight");
            assert_eq!(
                launch_table()
                    .iter()
                    .map(|launch| launch.phases.len())
                    .collect::<Vec<_>>(),
                [1]
            );
            // The spawn reports and drops them; a later phase times nothing.
            launch_spawned(id, 7);
            launch_phase("dev.tilo.phases", "late");
            assert!(launch_table().iter().all(|launch| launch.phases.is_empty()));
            launch_failed(id);
        }
    }

    #[test]
    fn launcher_marks_cross_into_the_app_file_and_reject_garbage() {
        let marks = [
            HandoffEvent {
                name: "launcher-start".into(),
                epoch_us: 1_783_250_000_000_000,
            },
            HandoffEvent {
                name: "sandbox-argv-ready".into(),
                epoch_us: 1_783_250_000_070_000,
            },
        ];
        let value = encode_handoff(&marks);
        assert_eq!(
            value,
            "launcher-start@1783250000000000,sandbox-argv-ready@1783250000070000"
        );
        assert_eq!(parse_handoff(&value), marks);
        // An app must never be able to smuggle structure into its own file.
        let parsed = parse_handoff("ok@1,Upper@2,bad name@3,no-time@,quote\"@4,,@5,x@-1,y@7");
        let names: Vec<_> = parsed.iter().map(|mark| mark.name.as_str()).collect();
        assert_eq!(names, ["ok", "y"]);
        let many = (0..100)
            .map(|i| format!("m@{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(parse_handoff(&many).len(), HANDOFF_MAX_MARKS);

        // Starting capture writes nothing yet; the path is never touched.
        let metrics = Metrics::start(std::env::temp_dir().join("unused.json"), Some(&value));
        let payload = metrics.payload(&[]);
        assert_eq!(payload["launcher"][1]["name"], "sandbox-argv-ready");
        assert_eq!(
            payload["launcher"][1]["epoch_us"].as_u64(),
            Some(1_783_250_000_070_000)
        );
        let origin_us = payload["origin_epoch_us"].as_u64().unwrap();
        assert_eq!(
            payload["origin_epoch_ms"].as_u64().unwrap(),
            origin_us / 1_000
        );
    }

    #[test]
    fn launch_latency_lines_survive_the_default_session_filter() {
        assert_eq!(log_directives(None), "error,tilo::launch_latency=info");
        assert_eq!(
            log_directives(Some("  ")),
            "error,tilo::launch_latency=info"
        );
        assert_eq!(
            log_directives(Some("warn")),
            "warn,tilo::launch_latency=info"
        );
        assert_eq!(
            log_directives(Some("info,tilo::launch_latency=off")),
            "info,tilo::launch_latency=off"
        );
    }

    /// The shim's exec mark, appended to the launcher's value, precedes the
    /// app's own origin, so origin minus it is the loader's time.
    #[test]
    fn exec_origin_precedes_init() {
        let launcher = encode_handoff(&[HandoffEvent {
            name: "sandbox-argv-ready".into(),
            epoch_us: 1,
        }]);
        let value = format!("{launcher},{}", handoff_entry("sandbox-exec").unwrap());
        let metrics = Metrics::start(std::env::temp_dir().join("unused.json"), Some(&value));
        let exec = metrics.launcher.last().unwrap();
        assert_eq!(exec.name, "sandbox-exec");
        assert!(exec.epoch_us <= metrics.origin_epoch_us);
        assert_eq!(handoff_entry("Not A Mark"), None);
    }

    fn marks_in(path: &std::path::Path) -> Vec<String> {
        let Ok(bytes) = std::fs::read(path) else {
            return Vec::new();
        };
        let doc: serde_json::Value = serde_json::from_slice(&bytes).expect("a whole JSON file");
        doc["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|event| event["name"].as_str().expect("name").to_owned())
            .collect()
    }

    /// Before the writer runs, the marking thread writes each mark; once it
    /// runs, marks reach the file with no dump and no write on the marking
    /// thread's account; a dump holds every mark; a write with nothing new
    /// leaves the file alone.
    #[test]
    fn marks_reach_the_file_through_the_writer_thread() {
        let directory = std::env::temp_dir().join(format!(
            "tilo-startup-metrics-writer-{}-{}",
            std::process::id(),
            tilo_clock::unix_now_ns()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("startup.json");
        let metrics: &'static Metrics = Box::leak(Box::new(Metrics::start(path.clone(), None)));

        metrics.record("main-start");
        assert_eq!(
            marks_in(&path),
            ["main-start"],
            "written by the marking thread"
        );

        metrics.start_writer();
        assert!(metrics.writer.get().is_some_and(Option::is_some));
        metrics.record("first-window");
        metrics.record("first-frame");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while marks_in(&path).len() < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "the writer thread never wrote: {:?}",
                marks_in(&path)
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            marks_in(&path),
            ["main-start", "first-window", "first-frame"]
        );

        // Nothing new: the file is not rewritten.
        std::fs::write(&path, br#"{"events":[{"name":"untouched"}]}"#).unwrap();
        metrics.write_snapshot();
        assert_eq!(marks_in(&path), ["untouched"]);
        // Something new: the whole history, in order.
        metrics.events.lock().unwrap().push(Event {
            name: "exit".into(),
            ns: 1,
        });
        metrics.write_snapshot();
        assert_eq!(
            marks_in(&path),
            ["main-start", "first-window", "first-frame", "exit"]
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn dispatch_phases_name_each_interval_up_to_the_spawn() {
        let requested = Instant::now();
        let at = |ms| requested + std::time::Duration::from_micros(ms);
        assert_eq!(
            format_launch_phases(
                requested,
                &[
                    ("preflight", at(400)),
                    ("command", at(12_450)),
                    ("spawned", at(13_000))
                ],
                at(20_000),
            ),
            "preflight:0.4,command:12.0,spawned:0.5,registered:7.0"
        );
        assert_eq!(
            format_launch_phases(requested, &[], at(1_500)),
            "registered:1.5"
        );
    }
}
