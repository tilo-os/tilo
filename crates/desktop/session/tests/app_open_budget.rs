//! Every installed app opens within the splash budget its own manifest declares.
//!
//! An app's manifest promises, in `[entrypoint] splash-budget` (ms), how long a person waits
//! between asking for a window and its first view. This test opens each app the session's tests
//! can install from the programs their run built, and holds each open to the budget in the
//! manifest the package was installed with, read from the device's own store rather than from a
//! number written here.
//!
//! What it holds to the budget is the work of the open, not the time it took: the CPU the session
//! and the open's own processes (the launcher, the sandbox, the app, and for a tilolang app its
//! host and window) ran from the window asked for to the app's first presented frame, read from
//! the kernel's scheduler statistics for each of their threads. A test job shares its node, so the
//! wall clock of an open mostly measures how long its threads waited for a core; the time they
//! waited, and the wall time, are reported beside the work for whoever reads a failure. The
//! session, the apps, the launcher and the tilolang runtime are optimised builds, as they are on
//! a person's machine (t.toml's `starts`).
//!
//! Each app opens twice: once on an idle machine and once while one busy thread per core
//! saturates the job, so an app whose open does more work under contention (a spin, a retry, a
//! timeout that fires) is caught. Both are held to the same budget.
//!
//! The failure names each app over budget with its work, its waits and the phases the session and
//! the app spent them in, so its owner can say where the time went.
//!
//! The seeded tilolang apps are here beside them: each is an installed package the session opens
//! through the tilolang host it starts, whose instance the host runs and whose window the tilolang
//! runtime draws. Their budgets are the `splash_budget` their `app` headers declare, which
//! `tilolang build` writes as their manifest's `splash-budget`; an app that declares none fails
//! this test rather than passing at the manifest's default, since a shipped app owes a person the
//! promise. The host and the window are optimised builds too, and a test carries the signed
//! runtime bundles they name as a device does.

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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tilo_app_package::manifest::Manifest;

use crate::common::people::{Device, Person};

/// The installed apps this test opens: the apps of other packages that the run builds for
/// `tilo-session` (t.toml's `starts`) and whose harness installs a real package and starts it in
/// a session.
const APPS: [&str; 2] = ["so.tilo.passwords", "so.tilo.model-providers"];

/// The seeded tilolang apps this test opens, by package id: the apps of `apps/tl` a device
/// carries in place of the Rust apps they replaced, each an installed package of its own.
const TL_APPS: [&str; 5] = [
    "so.tilo.notes",
    "so.tilo.tasks",
    "so.tilo.chat",
    "so.tilo.mail",
    "so.tilo.tube",
];

/// How an app is installed and opened: a Rust app's own package opens a process of its own, and a
/// tilolang app's opens through the tilolang host the session starts for it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Native,
    Tl,
}

/// Where the session asks for an app's window, as a person opening it from their applications
/// does.
fn install(device: &Device, app: &str) {
    match app {
        "so.tilo.passwords" => device.install_passwords(),
        "so.tilo.model-providers" => device.install_model_providers(),
        other => panic!("no install for {other}, which this test names in APPS"),
    }
}

/// Install every seeded tilolang app this test opens into `device`'s store, the way a device that
/// carries them holds them: signed packages from the run's build, as `tilolang run` installs a
/// development session's.
fn install_tl(device: &Device) {
    let installed = crate::common::dev_apps::install_named(device.store(), &TL_APPS);
    assert_eq!(
        installed.len(),
        TL_APPS.len(),
        "every seeded tilolang app this test opens is installed: {installed:?}"
    );
}

/// Take the runtime bundles a device with the seeded tilolang apps carries into `device`'s store of
/// runtimes, and answer the publishers its launcher trusts. A sold device has both in its image; a
/// test bakes them from the programs its run built (`common::dev_apps`).
fn install_runtimes(device: &Device) -> PathBuf {
    crate::common::dev_apps::install_runtimes(&device.directory().join("data/tilo-runtime"));
    crate::common::dev_apps::runtimes().trusted.clone()
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
fn budget_ms(device: &Device, app: &str, kind: Kind) -> u32 {
    let registry = std::fs::read_to_string(device.store().join("registry.toml"))
        .unwrap_or_else(|error| panic!("{}'s app registry: {error}", device.label));
    let manifest_path = registered_manifest(&registry, app)
        .unwrap_or_else(|| panic!("{app} is installed in {}", device.store().display()));
    let text = std::fs::read_to_string(&manifest_path).unwrap_or_else(|error| {
        panic!(
            "{app}'s installed manifest at {}: {error}",
            manifest_path.display()
        )
    });
    if kind == Kind::Tl && !declares_splash_budget(&text) {
        panic!(
            "{app} declares no splash budget: add `splash_budget: <milliseconds>` to its `app` \
             header, in `apps/tl`, as every seeded tilolang app does, so its manifest promises a \
             person how long its open may take"
        );
    }
    Manifest::from_toml_str(&text)
        .unwrap_or_else(|error| panic!("{app}'s installed manifest: {error}"))
        .entrypoint
        .unwrap_or_else(|| panic!("{app}'s manifest declares the entrypoint it opens with"))
        .splash_budget_ms
}

/// Whether a manifest declares its own `[entrypoint] splash-budget`, rather than leaving the
/// field's default: a tilolang app owes the promise, so its absence is a failure this test names.
fn declares_splash_budget(manifest: &str) -> bool {
    let mut entrypoint = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            entrypoint = line == "[entrypoint]";
        } else if entrypoint && line.starts_with("splash-budget") {
            return true;
        }
    }
    false
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
    /// What the open's own processes (the launcher, the sandbox, the app) ran and waited.
    app: Cpu,
    /// What the session ran and waited over the same span.
    session: Cpu,
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
            app: Cpu::default(),
            session: Cpu::default(),
        })
    }

    /// The CPU the open took: what the session and the open's processes ran, in milliseconds.
    fn work_ms(&self) -> f64 {
        ms(self.app.ran_ns + self.session.ran_ns)
    }

    /// The open's work, what it waited for a core and how long it took, as a failure reports it.
    fn summary(&self) -> String {
        format!(
            "work {:.0} ms (app ran {:.0} ms, waited {:.0} ms; session ran {:.0} ms, waited \
             {:.0} ms), wall {:.0} ms",
            self.work_ms(),
            ms(self.app.ran_ns),
            ms(self.app.waited_ns),
            ms(self.session.ran_ns),
            ms(self.session.waited_ns),
            self.total_ms,
        )
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
/// own first mark), then the app's own marks up to its first frame. An app's step is
/// `name:wall/ran`, the CPU its process ran in that step beside the wall time, where its timeline
/// says. Steps under a millisecond of either are left out. The steps that ran the most follow,
/// heaviest first, so a failure says where an app's own work went.
fn steps(timeline: &serde_json::Value) -> String {
    let mut marks: Vec<(String, f64, Option<f64>)> = timeline["launcher"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|mark| {
            Some((
                mark["name"].as_str()?.to_owned(),
                mark["epoch_us"].as_f64()?,
                None,
            ))
        })
        .collect();
    let origin = timeline["origin_epoch_us"].as_f64().unwrap_or(0.0);
    marks.push(("exec-to-main".to_owned(), origin, None));
    marks.extend(
        timeline["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|event| {
                Some((
                    event["name"].as_str()?.to_owned(),
                    origin + event["ns"].as_f64()? / 1000.0,
                    event["cpu_ns"].as_f64(),
                ))
            }),
    );
    let mut previous = marks.first().map_or(0.0, |(_, at, _)| *at);
    let mut previous_cpu = 0.0;
    let mut out = Vec::new();
    let mut ran = Vec::new();
    for (name, at, cpu) in marks {
        let elapsed = (at - previous) / 1000.0;
        previous = previous.max(at);
        let step_ran = cpu.map(|cpu| {
            let step = (cpu - previous_cpu).max(0.0) / 1e6;
            previous_cpu = previous_cpu.max(cpu);
            step
        });
        match step_ran {
            Some(step) if elapsed >= 1.0 || step >= 1.0 => {
                out.push(format!("{name}:{elapsed:.0}/{step:.0}"));
                ran.push((step, name));
            }
            None if elapsed >= 1.0 => out.push(format!("{name}:{elapsed:.0}")),
            _ => {}
        }
    }
    ran.sort_by(|a, b| b.0.total_cmp(&a.0));
    let heaviest = ran
        .iter()
        .take(6)
        .map(|(step, name)| format!("{name} {step:.0}"))
        .collect::<Vec<_>>();
    if heaviest.is_empty() {
        out.join(",")
    } else {
        format!(
            "{}\n               ran most: {}",
            out.join(","),
            heaviest.join(", ")
        )
    }
}

/// Open `app` on `device` and return the open the session logged for it: the window asked for to
/// the app's first presented frame, and the work the session and the open's processes did in it.
fn open_and_time(device: &Device, app: &str) -> Open {
    let session = device
        .session_pid()
        .unwrap_or_else(|| panic!("{}'s session runs", device.label));
    let after = device.log().lines().count();
    let watch = Watch::start(session);
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
    let first_frame = first_frame_epoch_us(&written)
        .unwrap_or_else(|| panic!("{app}'s timeline names its first frame: {written}"));
    (open.app, open.session) = watch.until(first_frame);
    open
}

/// When the app presented its first frame, in microseconds since the Unix epoch, as its timeline
/// says.
fn first_frame_epoch_us(timeline: &serde_json::Value) -> Option<u128> {
    let origin = timeline["origin_epoch_us"].as_u64()?;
    let ns = timeline["events"]
        .as_array()?
        .iter()
        .find(|event| event["name"] == "first-frame-presented")?["ns"]
        .as_u64()?;
    Some(u128::from(origin) + u128::from(ns / 1000))
}

/// `ns` nanoseconds in milliseconds.
fn ms(ns: u64) -> f64 {
    Duration::from_nanos(ns).as_secs_f64() * 1000.0
}

/// What a set of threads ran on a core and waited for one, summed, from the kernel's per-thread
/// scheduler statistics (`/proc/<pid>/task/<tid>/schedstat`).
#[derive(Clone, Copy, Default)]
struct Cpu {
    ran_ns: u64,
    waited_ns: u64,
}

impl Cpu {
    fn add(self, other: Self) -> Self {
        Self {
            ran_ns: self.ran_ns + other.ran_ns,
            waited_ns: self.waited_ns + other.waited_ns,
        }
    }

    fn since(self, earlier: Self) -> Self {
        Self {
            ran_ns: self.ran_ns.saturating_sub(earlier.ran_ns),
            waited_ns: self.waited_ns.saturating_sub(earlier.waited_ns),
        }
    }
}

/// Each live thread of `pid` and what it has run and waited.
fn threads(pid: u32) -> Vec<(u32, Cpu)> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    tasks
        .filter_map(|task| {
            let task = task.ok()?;
            let tid = task.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(task.path().join("schedstat")).ok()?;
            let mut fields = stat.split_whitespace().map(str::parse::<u64>);
            Some((
                tid,
                Cpu {
                    ran_ns: fields.next()?.ok()?,
                    waited_ns: fields.next()?.ok()?,
                },
            ))
        })
        .collect()
}

/// The parent of process `pid`: the field after the state in `/proc/<pid>/stat`, read after the
/// command name, which may hold spaces and parentheses.
fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// The processes running now.
fn processes() -> Vec<u32> {
    std::fs::read_dir("/proc")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// One reading of an open's work: when it was taken, what the session's threads have run, and
/// what the processes the open started have run.
struct Sample {
    at_us: u128,
    session: Cpu,
    started: Cpu,
}

/// Samples, every couple of milliseconds, what the session and each process it starts after the
/// watch begins have run: an open's processes, which run under the session, and nothing that ran
/// before. A thread that ends keeps what it last ran, so a short-lived process counts too, to
/// within one sample.
struct Watch {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Vec<Sample>>,
}

impl Watch {
    fn start(session: u32) -> Self {
        let before: HashSet<u32> = processes().into_iter().collect();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut under: HashMap<u32, bool> = HashMap::new();
            let mut session_threads: HashMap<u32, Cpu> = HashMap::new();
            let mut started_threads: HashMap<(u32, u32), Cpu> = HashMap::new();
            let mut samples = Vec::new();
            loop {
                for (tid, cpu) in threads(session) {
                    session_threads.insert(tid, cpu);
                }
                for pid in processes() {
                    if pid == session
                        || before.contains(&pid)
                        || !descends(pid, session, &mut under)
                    {
                        continue;
                    }
                    for (tid, cpu) in threads(pid) {
                        started_threads.insert((pid, tid), cpu);
                    }
                }
                samples.push(Sample {
                    at_us: u128::from(tilo_clock::unix_now_ns()) / 1000,
                    session: session_threads
                        .values()
                        .fold(Cpu::default(), |a, b| a.add(*b)),
                    started: started_threads
                        .values()
                        .fold(Cpu::default(), |a, b| a.add(*b)),
                });
                if stopped.load(Ordering::Relaxed) {
                    return samples;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        Self { stop, thread }
    }

    /// What the open's processes and the session ran and waited from the watch's start to the
    /// first sample at or after `epoch_us`: the sample after the moment rather than before it, so
    /// a coarse sample counts more work and never less.
    fn until(self, epoch_us: u128) -> (Cpu, Cpu) {
        self.stop.store(true, Ordering::Relaxed);
        let samples = self.thread.join().expect("the watch samples until stopped");
        let first = samples
            .first()
            .expect("the watch takes a sample before it stops");
        let at = samples
            .iter()
            .find(|sample| sample.at_us >= epoch_us)
            .or_else(|| samples.last())
            .expect("the watch takes a sample before it stops");
        (at.started, at.session.since(first.session))
    }
}

/// Whether `pid` runs under `session`, its parents walked up to it, remembered in `under`.
fn descends(pid: u32, session: u32, under: &mut HashMap<u32, bool>) -> bool {
    if let Some(known) = under.get(&pid) {
        return *known;
    }
    let answer = match parent(pid) {
        Some(parent) if parent == session => true,
        Some(parent) if parent > 1 && parent != pid => descends(parent, session, under),
        _ => false,
    };
    under.insert(pid, answer);
    answer
}

/// The session in a directory with the launcher it starts apps through beside it, where an
/// installed session finds its launcher. The session is the optimised build the run made for this
/// test (`t.toml` `starts`), whose passes are what the budget counts: the tests' own debug build
/// spends several times the work on the same pass, so a run that made no optimised one has nothing
/// to judge and says so. The directory is made beside that build, so the programs are linked into
/// it rather than copied where the file system allows.
fn session_beside_launcher() -> (tempfile::TempDir, PathBuf) {
    let session = tilo_test_env::started_variant("tilo-session", "fast", "dev-overrides")
        .unwrap_or_else(|| {
            panic!(
                "no optimised session to time: the budget is about the `fast` build of \
                 tilo-session with `dev-overrides`, which `t test` builds from t.toml's starts \
                 (`tilo-session:tilo-session@fast=dev-overrides`) and hands over as \
                 T_STARTED_BIN_tilo-session_fast_dev-overrides. Run it with `t test \
                 test/tilo-session --test app_open_budget::`, or set that variable to a session \
                 built with `--profile fast --features dev-overrides`"
            )
        });
    let launcher = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
    let directory = session
        .parent()
        .and_then(|beside| tempfile::tempdir_in(beside).ok())
        .unwrap_or_else(|| tempfile::tempdir().expect("a directory for the session's programs"));
    for program in [&session, &launcher] {
        let to = directory
            .path()
            .join(program.file_name().expect("a program's file name"));
        if std::fs::hard_link(program, &to).is_err() {
            std::fs::copy(program, &to).unwrap_or_else(|error| {
                panic!("{} copied to {}: {error}", program.display(), to.display())
            });
        }
    }
    let run = directory
        .path()
        .join(session.file_name().expect("a program's file name"));
    (directory, run)
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

/// Every installed app opens within the splash budget its own manifest declares, counted as the
/// work the open takes, idle and while the job's cores are all busy.
#[test]
fn every_installed_app_opens_within_the_budget_its_manifest_declares() {
    let _settled = ui::motion::testing::settled();
    let (_programs, session) = session_beside_launcher();
    let ada = Person::new("Ada");
    let apps: Vec<&str> = APPS.iter().chain(TL_APPS.iter()).copied().collect();

    let mut idle = ada.device("Ada's laptop");
    idle.run_session(session.clone());
    for app in APPS {
        install(&idle, app);
    }
    install_tl(&idle);
    let trusted = install_runtimes(&idle);
    idle.env("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", trusted);
    start_showing_a_workspace(&mut idle);
    let budgets: Vec<u32> = APPS
        .iter()
        .map(|app| budget_ms(&idle, app, Kind::Native))
        .chain(TL_APPS.iter().map(|app| budget_ms(&idle, app, Kind::Tl)))
        .collect();
    let idle_opens: Vec<Open> = apps.iter().map(|app| open_and_time(&idle, app)).collect();
    idle.stop();

    let mut loaded = ada.device("Ada's busy laptop");
    loaded.run_session(session);
    for app in APPS {
        install(&loaded, app);
    }
    install_tl(&loaded);
    let trusted = install_runtimes(&loaded);
    loaded.env("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", trusted);
    start_showing_a_workspace(&mut loaded);
    let load = Load::start();
    let loaded_opens: Vec<Open> = apps.iter().map(|app| open_and_time(&loaded, app)).collect();
    drop(load);
    loaded.stop();

    let mut over = Vec::new();
    for ((app, budget), (idle_open, loaded_open)) in apps
        .iter()
        .zip(&budgets)
        .zip(idle_opens.iter().zip(&loaded_opens))
    {
        // A drift upward toward a budget shows in a passing run's log as it does in a failing
        // one's: the budget and the work each open took.
        println!(
            "{app}: budget {budget} ms of work; idle {:.0} ms of work, {:.0} ms wall; loaded \
             {:.0} ms of work, {:.0} ms wall",
            idle_open.work_ms(),
            idle_open.total_ms,
            loaded_open.work_ms(),
            loaded_open.total_ms,
        );
        if idle_open.work_ms() > f64::from(*budget) || loaded_open.work_ms() > f64::from(*budget) {
            let pad = " ".repeat(9);
            over.push(format!(
                "{app}: budget {budget} ms of work\n  idle   {}\n{pad}{}\n  loaded {}\n{pad}{}",
                idle_open.summary(),
                idle_open.phases(),
                loaded_open.summary(),
                loaded_open.phases(),
            ));
        }
    }
    assert!(
        over.is_empty(),
        "these apps take more work to open than the splash budget their installed manifests \
         declare:\n{}",
        over.join("\n")
    );
}
