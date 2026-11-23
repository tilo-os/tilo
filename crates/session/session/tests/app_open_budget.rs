//! Every installed app opens within the splash budget its own manifest declares.
//!
//! An app's manifest promises, in `[entrypoint] splash-budget` (ms), how long a person waits
//! between asking for a window and its first view. This test opens each app the session's tests
//! can install from the programs their run built, and holds each open to the budget in the
//! manifest the package was installed with, read from the device's own store rather than from a
//! number written here.
//!
//! What it holds to the budget is the work of the open, not the time it took: the CPU the session
//! and the open's own processes (the launcher, the sandbox and the app) ran from the window asked
//! for to the app's first presented frame, read from the kernel's scheduler statistics for each of
//! their threads. A test job shares its node, so the wall clock of an open mostly measures how long
//! its threads waited for a core; the time they waited, and the wall time, are reported beside the
//! work for whoever reads a failure. The apps and the launcher are optimised builds, as they are on
//! a person's machine (t.toml's `starts`); the session is the tests' own build of it.
//!
//! Each app opens twice: once on an idle machine and once while one busy thread per core
//! saturates the job, so an app whose open does more work under contention (a spin, a retry, a
//! timeout that fires) is caught. Both are held to the same budget.
//!
//! The failure names each app over budget with its work, its waits and the phases the session and
//! the app spent them in, so its owner can say where the time went.
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

/// The session the run built, in a directory with the launcher it starts apps through beside it,
/// where an installed session finds its launcher. The directory is made beside that build, so the programs are linked into it rather
/// than copied where the file system allows.
fn session_beside_launcher() -> (tempfile::TempDir, PathBuf) {
    let session = tilo_test_env::sibling_binary("tilo-session", "tilo-session");
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

    let mut idle = ada.device("Ada's laptop");
    idle.run_session(session.clone());
    for app in APPS {
        install(&idle, app);
    }
    start_showing_a_workspace(&mut idle);
    let budgets: Vec<u32> = APPS.iter().map(|app| budget_ms(&idle, app)).collect();
    let idle_opens: Vec<Open> = APPS.iter().map(|app| open_and_time(&idle, app)).collect();
    idle.stop();

    let mut loaded = ada.device("Ada's busy laptop");
    loaded.run_session(session);
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
