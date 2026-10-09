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
//!   "origin_epoch_us": 1783250000000123,
//!   "exec_to_main_us": 41000,
//!   "launcher": [
//!     { "name": "launcher-start",     "epoch_us": 1783249999880000 },
//!     { "name": "sandbox-argv-ready", "epoch_us": 1783249999950000 }
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
//! `origin_epoch_us` is `origin_epoch_ms` at microsecond precision. The other two fields
//! cover the time before `main`, which the process's own clock cannot see:
//!
//! - `launcher` holds the phase marks of the `tilo-launcher` process that set up this
//!   app's sandbox, in Unix-epoch microseconds ([`handoff_mark`], handed over in
//!   [`LAUNCHER_MARKS_ENV`]). Its last mark is taken just before the sandbox is spawned,
//!   so `origin_epoch_us` minus that mark is bubblewrap, the Landlock shim and the
//!   dynamic loader.
//! - `exec_to_main_us` (Linux only, else `null`) is the time from this process's
//!   creation, as the kernel recorded it in `/proc/self/stat`, to [`init`]: mostly the
//!   dynamic loader and library constructors. The kernel's start time has clock-tick
//!   (10 ms) resolution.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use serde::Serialize;
use std::sync::{Mutex, OnceLock, PoisonError};
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
}

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
        target: LAUNCH_LATENCY_TARGET,
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
    exec_to_main_us: Option<u128>,
    launcher: Vec<HandoffEvent>,
    events: Mutex<Vec<Event>>,
    output_path: std::path::PathBuf,
}

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
            exec_to_main_us: exec_to_main_us(),
            launcher: launcher_marks.map(parse_handoff).unwrap_or_default(),
            events: Mutex::new(Vec::with_capacity(16)),
            output_path,
        }
    }

    fn payload(&self, events: &[Event]) -> serde_json::Value {
        serde_json::json!({
            "origin_epoch_ms": self.origin_epoch_ms,
            "origin_epoch_us": self.origin_epoch_us,
            "exec_to_main_us": self.exec_to_main_us,
            "launcher": self.launcher,
            "events": events,
        })
    }
}

#[derive(Serialize, Clone)]
struct Event {
    name: String,
    ns: u128,
}

/// Microseconds from this process's creation to now, on Linux: the kernel's
/// `starttime` (field 22 of `/proc/self/stat`, in clock ticks since boot)
/// against `CLOCK_BOOTTIME`, the clock it is counted on.
#[cfg(target_os = "linux")]
fn exec_to_main_us() -> Option<u128> {
    // `/proc` reports times in USER_HZ, which is 100 on every architecture
    // Tilo ships (x86_64, aarch64); the kernel's own HZ never shows here.
    const USER_HZ: u128 = 100;
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The command name (field 2) is parenthesised and may itself hold spaces
    // or parentheses; every later field follows its last `)`. Field 3 is the
    // first after it, so field 22 is the 20th.
    let (_, fields) = stat.rsplit_once(')')?;
    let ticks: u128 = fields.split_whitespace().nth(19)?.parse().ok()?;
    let started_us = ticks.checked_mul(1_000_000)?.checked_div(USER_HZ)?;
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    let now_us = u128::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(u128::try_from(now.tv_nsec).ok()? / 1_000)?;
    now_us.checked_sub(started_us)
}

#[cfg(not(target_os = "linux"))]
fn exec_to_main_us() -> Option<u128> {
    None
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

/// Enable capture. Idempotent — subsequent calls are no-ops. Reads
/// `TILO_STARTUP_METRICS_PATH`: unset leaves the module disabled with every downstream
/// [`mark`] a zero-cost load; set means the file is written on [`dump`] or process exit
/// through a [`ScopedDumper`]. Run this at the earliest possible point in `main` so `origin`
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
    let payload = m.payload(&events);
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
    let payload = m.payload(&events);
    let _ = std::fs::write(
        &m.output_path,
        serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    );
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

    #[test]
    fn exec_origin_precedes_init() {
        let metrics = Metrics::start(std::env::temp_dir().join("unused.json"), None);
        let exec_to_main = metrics.payload(&[])["exec_to_main_us"].as_u64();
        if cfg!(target_os = "linux") {
            // The harness created this process before the test ran; a loaded
            // CI machine can take long to get here, so bound the age loosely.
            let us = exec_to_main.unwrap();
            assert!(us < 600_000_000, "process started {us} us ago");
        } else {
            assert_eq!(exec_to_main, None);
        }
    }
}
