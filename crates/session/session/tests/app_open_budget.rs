//! Every installed app opens within the splash budget its own manifest declares.
//!
//! An app's manifest promises, in `[entrypoint] splash-budget` (ms), how long a person waits
//! between asking for a window and its first view. The session times exactly that: the window
//! asked for, the process spawned, the toplevel mapped, the first frame presented
//! (`tilo_startup_metrics`). This test opens each app the session's tests can install from the
//! programs their run built, and holds each open to the budget in the manifest the package was
//! installed with, read from the device's own store rather than from a number written here.
//!
//! Each app opens twice: once on an idle machine and once while one busy thread per core
//! saturates the job, so an app that meets its budget only when nothing else runs is caught. The
//! loaded pass is held to the same budget as the idle one. A busy thread on every core leaves a
//! solo open roughly a core's worth of the machine (the threads preempt each other), so the
//! budget is the promise an app makes on a busy machine and not only on an empty one; a multiple
//! would let an open that starves under contention pass.
//!
//! The failure names each app over budget with both times and the phases the session spent them
//! in, so its owner can say where the time went.
//!
//! The tilolang apps are not here yet: opening one needs the tilo runtime bundle on the machine,
//! which a test's own app store does not carry.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_app_package::manifest::Manifest;

use crate::common::people::{Device, Person};

/// The installed apps this test opens: the apps of other packages that the run builds for
/// `tilo-session` (t.toml's `starts`) and whose harness installs a real package and starts it in
/// a session.
const APPS: [&str; 3] = [
    "so.tilo.tasks",
    "so.tilo.passwords",
    "so.tilo.model-providers",
];

/// Where the session asks for an app's window, as a person opening it from their applications
/// does.
fn install(device: &Device, app: &str) {
    match app {
        "so.tilo.tasks" => device.install_tasks(),
        "so.tilo.passwords" => device.install_passwords(),
        "so.tilo.model-providers" => device.install_model_providers(),
        other => panic!("no install for {other}, which this test names in APPS"),
    }
}

/// Start `device`'s session and wait until it shows a workspace: what the applications action
/// opens an app into.
fn start_showing_a_workspace(device: &mut Device) {
    device.start();
    tilo_test_env::wait_for(format!("{} showing a workspace", device.label), || {
        device.showing().filter(|showing| !showing.is_empty())
    });
}

/// The splash budget `app`'s installed package declares, read from the manifest this device's
/// store registers for it.
fn budget_ms(device: &Device, app: &str) -> u32 {
    let registry = std::fs::read_to_string(device.store().join("registry.toml"))
        .unwrap_or_else(|error| panic!("{}'s app registry: {error}", device.label));
    let manifest_path = registered_manifest(&registry, app)
        .unwrap_or_else(|| panic!("{app} is installed in {}", device.store().display()));
    let manifest = Manifest::from_toml_str(
        &std::fs::read_to_string(&manifest_path).unwrap_or_else(|error| {
            panic!(
                "{app}'s installed manifest at {}: {error}",
                manifest_path.display()
            )
        }),
    )
    .unwrap_or_else(|error| panic!("{app}'s installed manifest: {error}"));
    manifest
        .entrypoint
        .unwrap_or_else(|| panic!("{app}'s manifest declares the entrypoint it opens with"))
        .splash_budget_ms
}

/// The manifest path the registry holds for the entry `app`, as an install wrote it: the rows are
/// `[[entry]]` blocks, each naming its `id` before its `manifest_path`.
fn registered_manifest(registry: &str, app: &str) -> Option<PathBuf> {
    let mut id: Option<String> = None;
    for line in registry.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("id = ") {
            id = Some(value.trim_matches('"').to_owned());
        } else if let Some(value) = line.strip_prefix("manifest_path = ")
            && id.as_deref() == Some(app)
        {
            return Some(PathBuf::from(value.trim_matches('"')));
        }
    }
    None
}

/// One app's open, as the session's launch timing reported it.
struct Open {
    /// The window asked for to the app's first presented frame: what the splash budget bounds.
    total_ms: f64,
    /// The dispatch phases before the process started (`planned`, `routed`, `registered`), as
    /// the session logged them.
    dispatch: String,
    /// The window asked for to the process starting.
    spawn_ms: f64,
    /// The window asked for to the app's toplevel being mapped.
    map_ms: f64,
    /// The steps of the app's own start, from the launcher starting to its first frame, as its
    /// startup timeline has them ([`steps`]).
    steps: String,
}

impl Open {
    /// The open the session logged for `app` in `segment`: the launch's own lines, from the
    /// window asked for through the first presented frame.
    fn logged(segment: &str, app: &str) -> Option<Self> {
        let line = |message: &str| {
            segment
                .lines()
                .find(|line| line.contains(message) && field(line, "package_id") == Some(app))
        };
        let first = line("application first frame presented")?;
        let spawned = line("application launcher spawned");
        Some(Self {
            total_ms: number(first, "request_to_first_present_us") / 1000.0,
            dispatch: spawned
                .and_then(|line| field(line, "dispatch_phases"))
                .unwrap_or("none")
                .to_owned(),
            spawn_ms: spawned.map_or(0.0, |line| number(line, "request_to_spawn_us") / 1000.0),
            map_ms: line("application toplevel mapped")
                .map_or(0.0, |line| number(line, "request_to_map_us") / 1000.0),
            steps: String::new(),
        })
    }

    /// How the session spent this open: the phases before the spawn, the spawn, the toplevel and
    /// the first frame.
    fn phases(&self) -> String {
        format!(
            "{},spawned:{:.0}ms,mapped:{:.0}ms,first-frame:{:.0}ms\n           {}",
            self.dispatch, self.spawn_ms, self.map_ms, self.total_ms, self.steps
        )
    }
}

/// The value `name` carries on a line of the session's log, which writes its fields `name=value`.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|token| {
        token
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

/// The number `name` carries on a line, or 0 when the line does not carry it.
fn number(line: &str, name: &str) -> f64 {
    field(line, name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0)
}

/// The startup timeline `app` wrote on `device` up to its first frame: its state directory sits in
/// the directory each workspace gives the app, beside the device's store.
fn timeline(device: &Device, app: &str) -> Option<serde_json::Value> {
    let workspaces = device.store().parent()?.join("tilo-workspaces");
    std::fs::read_dir(workspaces).ok()?.find_map(|workspace| {
        let path = workspace
            .ok()?
            .path()
            .join(app)
            .join(".local/state/startup-timeline.json");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        written["events"]
            .as_array()?
            .iter()
            .any(|event| event["name"] == "first-frame-presented")
            .then_some(written)
    })
}

/// A timeline's steps, each the milliseconds since the one before it: the launcher's marks (Unix
/// epoch), the app's process starting (`exec-to-main`: from the sandbox executing it to the app's
/// own first mark), then the app's own marks up to its first frame. Steps under a millisecond are
/// left out.
fn steps(timeline: &serde_json::Value) -> String {
    let mut marks: Vec<(String, f64)> = timeline["launcher"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|mark| {
            Some((
                mark["name"].as_str()?.to_owned(),
                mark["epoch_us"].as_f64()?,
            ))
        })
        .collect();
    let origin = timeline["origin_epoch_us"].as_f64().unwrap_or(0.0);
    marks.push(("exec-to-main".to_owned(), origin));
    marks.extend(
        timeline["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|event| {
                Some((
                    event["name"].as_str()?.to_owned(),
                    origin + event["ns"].as_f64()? / 1000.0,
                ))
            }),
    );
    let mut previous = marks.first().map_or(0.0, |(_, at)| *at);
    let mut out = Vec::new();
    for (name, at) in marks {
        let elapsed = (at - previous) / 1000.0;
        previous = previous.max(at);
        if elapsed >= 1.0 {
            out.push(format!("{name}:{elapsed:.0}"));
        }
    }
    out.join(",")
}

/// Open `app` on `device` and return the open the session logged for it: the window asked for to
/// the app's first presented frame.
fn open_and_time(device: &Device, app: &str) -> Open {
    let after = device.log().lines().count();
    device.open(app);
    device.wait_for_window(app);
    let segment = tilo_test_env::wait_for(
        format!("{app}'s first frame in {}'s log", device.label),
        || {
            let logged = device.log();
            let segment = logged.lines().skip(after).collect::<Vec<_>>().join("\n");
            Open::logged(&segment, app).map(|_| segment)
        },
    );
    let mut open = Open::logged(&segment, app)
        .unwrap_or_else(|| panic!("{app}'s open is in the log it was waited for: {segment}"));
    let written = tilo_test_env::wait_for(
        format!(
            "{app}'s startup timeline up to its first frame on {}",
            device.label
        ),
        || timeline(device, app),
    );
    open.steps = steps(&written);
    open
}

/// One busy thread per core of the job, spinning until it is dropped: the CPU contention the
/// loaded pass opens under.
struct Load {
    running: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Load {
    fn start() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        let running = Arc::new(AtomicBool::new(true));
        let threads = (0..cores)
            .map(|_| {
                let running = Arc::clone(&running);
                std::thread::spawn(move || {
                    while running.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                })
            })
            .collect();
        Self { running, threads }
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Every installed app opens within the splash budget its own manifest declares, idle and while
/// the job's cores are all busy.
#[test]
fn every_installed_app_opens_within_the_budget_its_manifest_declares() {
    let _settled = ui::motion::testing::settled();
    let ada = Person::new("Ada");

    let mut idle = ada.device("Ada's laptop");
    for app in APPS {
        install(&idle, app);
    }
    start_showing_a_workspace(&mut idle);
    let budgets: Vec<u32> = APPS.iter().map(|app| budget_ms(&idle, app)).collect();
    let idle_opens: Vec<Open> = APPS.iter().map(|app| open_and_time(&idle, app)).collect();
    idle.stop();

    let mut loaded = ada.device("Ada's busy laptop");
    for app in APPS {
        install(&loaded, app);
    }
    start_showing_a_workspace(&mut loaded);
    let load = Load::start();
    let loaded_opens: Vec<Open> = APPS.iter().map(|app| open_and_time(&loaded, app)).collect();
    drop(load);
    loaded.stop();

    let mut over = Vec::new();
    for ((app, budget), (idle_open, loaded_open)) in APPS
        .iter()
        .zip(&budgets)
        .zip(idle_opens.iter().zip(&loaded_opens))
    {
        if idle_open.total_ms > f64::from(*budget) || loaded_open.total_ms > f64::from(*budget) {
            over.push(format!(
                "{app}: budget {budget} ms, idle {:.0} ms, loaded {:.0} ms\n  \
                 idle   {}\n  loaded {}",
                idle_open.total_ms,
                loaded_open.total_ms,
                idle_open.phases(),
                loaded_open.phases(),
            ));
        }
    }
    assert!(
        over.is_empty(),
        "these apps open past the splash budget their installed manifests declare:\n{}",
        over.join("\n")
    );
}
