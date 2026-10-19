//! The spawner's memory watch: reads pressure and the app tree, runs the
//! policy ([`crate::memory_pressure`]) and carries out what it decides.
//!
//! It sleeps while memory is calm. PSI triggers on `/proc/pressure/memory`
//! and on the tree's `memory.pressure` (150 ms of stall within 2 s, the
//! finest window the kernel grants an unprivileged process) wake it; then it
//! samples once a second until pressure has been calm for
//! [`CALM_SAMPLES`] samples, and sleeps again. Where no trigger can be armed
//! (an old kernel) it samples every [`FALLBACK_INTERVAL`] instead.
//!
//! What it does to the tree: `memory.reclaim` on background apps, and
//! `cgroup.kill` on the one app or job the policy ends. An ended app's cause
//! is recorded before the kill, so the compositor, reaping the app's
//! launcher, learns why it ended ([`Shared::ends`]); an ended job is reported
//! to the compositor on the spawner's event socket, since nothing else of
//! the app ends with it.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::app_cgroup::{self, Leaf};
use crate::memory_pressure::{Action, Cause, Policy, Psi, Unit, parse_psi};
use crate::priority::Tier;

/// The machine's memory pressure.
pub const SYSTEM_PRESSURE: &str = "/proc/pressure/memory";

/// The trigger armed on every pressure file: 150 ms of stall within a 2 s
/// window. Unprivileged triggers need a window of whole 2 s.
pub const TRIGGER: &str = "some 150000 2000000";

/// [`TRIGGER`] as written to a pressure file, NUL-terminated. The kernel's
/// `/proc/pressure/*` writer overwrites the last byte written with a NUL, so
/// an unterminated string loses its last digit: a 200 ms window, below the
/// 500 ms minimum, refused with `EINVAL`. A cgroup's `memory.pressure` reads
/// the string up to the NUL, so the same bytes arm both.
const TRIGGER_WRITTEN: &[u8] = b"some 150000 2000000\0";

/// Samples a second while pressure is up.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Calm samples in a row after which the watch sleeps again.
pub const CALM_SAMPLES: u32 = 10;

/// How often to sample when no trigger could be armed.
pub const FALLBACK_INTERVAL: Duration = Duration::from_secs(2);

/// A job the spawner made, as a notice names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobInfo {
    pub app_id: String,
    pub label: String,
    pub created: Instant,
}

/// What the spawner's request loop and the memory watch share.
#[derive(Debug, Default)]
pub struct Shared {
    /// Why an app ended for memory, by its cgroup, until the compositor asks.
    pub ends: Mutex<HashMap<PathBuf, Cause>>,
    /// Every job the spawner made, by its cgroup.
    pub jobs: Mutex<HashMap<PathBuf, JobInfo>>,
    /// The compositor's end of the event socket, when it gave one.
    pub events: Mutex<Option<OwnedFd>>,
}

/// The most ends remembered for a compositor that never asks.
const MAX_ENDS: usize = 256;

impl Shared {
    /// Remember why the app at `app` ended; a recorded cause stands (the
    /// policy's, set before its kill, is the one the person reads).
    pub fn record_end(&self, app: &Path, cause: Cause) {
        let mut ends = self.ends.lock().unwrap_or_else(PoisonError::into_inner);
        if ends.len() >= MAX_ENDS {
            ends.clear();
        }
        ends.entry(app.to_path_buf()).or_insert(cause);
    }

    /// Why the app at `app` ended, once: what was recorded, or what its
    /// main leaf's memory events say while the cgroup is still there.
    pub fn take_end(&self, app: &Path) -> Option<Cause> {
        let recorded = self
            .ends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(app);
        recorded.or_else(|| {
            let events = app_cgroup::memory_events(&app.join(app_cgroup::MAIN_LEAF));
            (events.oom_kill > 0 || events.oom_group_kill > 0).then_some(Cause::Kernel)
        })
    }

    /// Tell the compositor that the job at `job` ended for `cause`; its app
    /// keeps running.
    pub fn report_job_end(&self, job: &Path, cause: Cause) {
        let info = self
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(job)
            .cloned();
        let Some(info) = info else {
            return;
        };
        eprintln!(
            "tilo-spawner: {} of {} ended for memory ({})",
            info.label,
            info.app_id,
            cause.wire()
        );
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(socket) = events.as_ref() {
            let fields: [&[u8]; 4] = [
                b"job-ended",
                info.app_id.as_bytes(),
                info.label.as_bytes(),
                cause.wire().as_bytes(),
            ];
            if let Err(error) = runtime_portal::jobs::send(socket.as_fd(), &fields, &[]) {
                eprintln!("tilo-spawner: the compositor missed a job's end: {error}");
            }
        }
    }
}

/// Where a sample's readings come from.
pub trait Source {
    /// The machine's memory pressure; `None` when it cannot be read.
    fn system(&mut self) -> Option<Psi>;
    /// Every app's main leaf and job.
    fn units(&mut self) -> Vec<Unit>;
}

/// What carries the policy's decisions out.
pub trait Effects {
    fn reclaim(&mut self, dir: &Path, bytes: u64);
    fn end(&mut self, unit: &Unit, growth: u64);
}

/// One sample: read, decide, act. Returns the pressure read, `None` when
/// there was none to read.
pub fn step(
    policy: &mut Policy,
    source: &mut dyn Source,
    effects: &mut dyn Effects,
) -> Option<Psi> {
    let system = source.system()?;
    let units = source.units();
    for action in policy.tick(system, &units) {
        match action {
            Action::Reclaim { dir, bytes } => effects.reclaim(&dir, bytes),
            Action::End { unit, growth } => effects.end(&unit, growth),
        }
    }
    Some(system)
}

/// The cgroup tree of a running spawner.
pub struct TreeSource {
    pub tree: PathBuf,
    /// The machine's pressure file ([`SYSTEM_PRESSURE`]).
    pub system: PathBuf,
    pub shared: Arc<Shared>,
}

fn read_psi(path: &Path) -> Option<Psi> {
    parse_psi(&std::fs::read_to_string(path).ok()?)
}

impl Source for TreeSource {
    fn system(&mut self) -> Option<Psi> {
        read_psi(&self.system).or_else(|| read_psi(&self.tree.join("memory.pressure")))
    }

    fn units(&mut self) -> Vec<Unit> {
        let jobs = self
            .shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut units = Vec::new();
        for entry in std::fs::read_dir(&self.tree)
            .into_iter()
            .flatten()
            .flatten()
        {
            let app_dir = entry.path();
            let Some(app_id) = entry
                .file_name()
                .to_str()
                .and_then(app_cgroup::app_id_of_leaf)
            else {
                continue;
            };
            // An app whose tier cannot be read is treated as the focused
            // one: never reclaimed from.
            let tier = app_cgroup::tier_of(&app_dir).unwrap_or(Tier::Foreground);
            let mut leaves = vec![(app_dir.join(app_cgroup::MAIN_LEAF), Leaf::Main)];
            leaves.extend(
                app_cgroup::jobs(&app_dir)
                    .into_iter()
                    .map(|job| (job, Leaf::Job)),
            );
            for (dir, leaf) in leaves {
                let Some(footprint) = app_cgroup::footprint(&dir) else {
                    continue;
                };
                let label = match leaf {
                    Leaf::Main => None,
                    Leaf::Job => Some(
                        jobs.get(&dir)
                            .map_or_else(String::new, |info| info.label.clone()),
                    ),
                };
                units.push(Unit {
                    charged: std::fs::read_to_string(dir.join("memory.current"))
                        .ok()
                        .and_then(|text| text.trim().parse().ok())
                        .unwrap_or(footprint),
                    pressure: read_psi(&dir.join("memory.pressure")).unwrap_or_default(),
                    dir,
                    app_dir: app_dir.clone(),
                    app_id: app_id.clone(),
                    label,
                    leaf,
                    tier,
                    footprint,
                });
            }
        }
        units
    }
}

/// Carries the policy out on the tree.
pub struct TreeEffects {
    pub shared: Arc<Shared>,
}

impl Effects for TreeEffects {
    fn reclaim(&mut self, dir: &Path, bytes: u64) {
        if let Err(error) = app_cgroup::reclaim(dir, bytes) {
            eprintln!("tilo-spawner: reclaiming from {}: {error}", dir.display());
        }
    }

    fn end(&mut self, unit: &Unit, growth: u64) {
        let mib = growth.checked_shr(20).unwrap_or(0);
        match unit.leaf {
            Leaf::Job => {
                eprintln!(
                    "tilo-spawner: memory is running out; ending {} of {}, which grew {mib} MiB",
                    unit.label.as_deref().unwrap_or("a job"),
                    unit.app_id
                );
                self.shared.report_job_end(&unit.dir, Cause::Runaway);
                if let Err(error) = app_cgroup::kill(&unit.dir) {
                    eprintln!("tilo-spawner: ending {}: {error}", unit.dir.display());
                }
            }
            Leaf::Main => {
                eprintln!(
                    "tilo-spawner: memory is running out; ending {}, which grew {mib} MiB",
                    unit.app_id
                );
                // Recorded first: the compositor asks as soon as the
                // launcher is reaped.
                self.shared.record_end(&unit.app_dir, Cause::Runaway);
                if let Err(error) = app_cgroup::kill(&unit.app_dir) {
                    eprintln!("tilo-spawner: ending {}: {error}", unit.app_dir.display());
                }
            }
        }
    }
}

/// Arm a PSI trigger on `path`: the descriptor becomes `POLLPRI` when it
/// fires.
fn arm(path: &Path) -> io::Result<OwnedFd> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    file.write_all(TRIGGER_WRITTEN)?;
    Ok(OwnedFd::from(file))
}

/// Watch `tree` on a thread of its own, for the life of the spawner.
pub fn spawn(tree: PathBuf, shared: Arc<Shared>) {
    let started = std::thread::Builder::new()
        .name("tilo-memory-watch".into())
        .spawn(move || watch(&tree, &shared));
    if let Err(error) = started {
        eprintln!("tilo-spawner: memory pressure goes unwatched: {error}");
    }
}

fn watch(tree: &Path, shared: &Arc<Shared>) {
    let mut triggers: Vec<OwnedFd> = [Path::new(SYSTEM_PRESSURE), &tree.join("memory.pressure")]
        .into_iter()
        .filter_map(|path| match arm(path) {
            Ok(fd) => Some(fd),
            Err(error) => {
                eprintln!(
                    "tilo-spawner: no pressure trigger on {}: {error}",
                    path.display()
                );
                None
            }
        })
        .collect();
    let mut source = TreeSource {
        tree: tree.to_path_buf(),
        system: PathBuf::from(SYSTEM_PRESSURE),
        shared: Arc::clone(shared),
    };
    let mut effects = TreeEffects {
        shared: Arc::clone(shared),
    };
    let mut policy = Policy::new();
    let mut calm_samples = CALM_SAMPLES;
    loop {
        let timeout = if calm_samples < CALM_SAMPLES {
            Some(SAMPLE_INTERVAL)
        } else if triggers.is_empty() {
            Some(FALLBACK_INTERVAL)
        } else {
            None
        };
        let fired = wait(&mut triggers, timeout);
        if !tree.exists() {
            return;
        }
        if fired {
            // Pressure the averages do not show yet: sample from now on.
            calm_samples = 0;
        }
        let Some(system) = step(&mut policy, &mut source, &mut effects) else {
            continue;
        };
        if Policy::calm(system) {
            calm_samples = calm_samples.saturating_add(1);
            if calm_samples == CALM_SAMPLES {
                // Asleep from here: start the next episode fresh.
                policy = Policy::new();
            }
        } else {
            calm_samples = 0;
        }
    }
}

/// Sleep until a trigger fires or `timeout` passes. Drops a trigger that
/// broke (its cgroup went away). Returns whether a trigger fired.
fn wait(triggers: &mut Vec<OwnedFd>, timeout: Option<Duration>) -> bool {
    let mut fds: Vec<libc::pollfd> = triggers
        .iter()
        .map(|fd| libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLPRI,
            revents: 0,
        })
        .collect();
    let millis = timeout.map_or(-1, |timeout| {
        i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX)
    });
    if fds.is_empty() {
        std::thread::sleep(timeout.unwrap_or(FALLBACK_INTERVAL));
        return false;
    }
    let count = libc::nfds_t::try_from(fds.len()).unwrap_or(0);
    // SAFETY: `fds` holds `count` valid pollfd entries.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, millis) };
    if ready <= 0 {
        return false;
    }
    let mut fired = false;
    let mut broken = Vec::new();
    for (index, fd) in fds.iter().enumerate() {
        if fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            broken.push(index);
        } else if fd.revents & libc::POLLPRI != 0 {
            fired = true;
        }
    }
    for index in broken.into_iter().rev() {
        if index < triggers.len() {
            drop(triggers.remove(index));
        }
    }
    fired
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test footprints are small multiples of a mebibyte"
)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[derive(Default)]
    struct Recorded {
        reclaimed: Vec<(PathBuf, u64)>,
        ended: Vec<(PathBuf, u64)>,
    }

    impl Effects for Recorded {
        fn reclaim(&mut self, dir: &Path, bytes: u64) {
            self.reclaimed.push((dir.to_path_buf(), bytes));
        }
        fn end(&mut self, unit: &Unit, growth: u64) {
            self.ended.push((unit.dir.clone(), growth));
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn psi(some: f64, full: f64) -> String {
        format!(
            "some avg10={some:.2} avg60=0.00 avg300=0.00 total=1\n\
             full avg10={full:.2} avg60=0.00 avg300=0.00 total=1\n"
        )
    }

    /// A tree on disk as the kernel shows it, with a fake PSI file for the
    /// machine: the watch reads both exactly as it reads the real ones.
    struct FakeTree {
        dir: tempfile::TempDir,
        shared: Arc<Shared>,
    }

    impl FakeTree {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let shared = Arc::new(Shared::default());
            let tree = dir.path().join("tree");
            for (app, weight) in [("app-so.tilo.mail-1", "50"), ("app-so.tilo.code-2", "800")] {
                write(&tree.join(app).join("cpu.weight"), weight);
                write(&tree.join(app).join("main/memory.pressure"), &psi(0.0, 0.0));
            }
            write(&tree.join("spawner/cgroup.procs"), "1\n");
            let job = tree.join("app-so.tilo.code-2/job-1");
            write(&job.join("memory.pressure"), &psi(0.0, 0.0));
            shared.jobs.lock().unwrap().insert(
                job,
                JobInfo {
                    app_id: "so.tilo.code".into(),
                    label: "rust-analyzer".into(),
                    created: Instant::now(),
                },
            );
            Self { dir, shared }
        }

        fn tree(&self) -> PathBuf {
            self.dir.path().join("tree")
        }

        fn sample(&self, system: &str, mail: u64, code: u64, job: u64) {
            write(&self.dir.path().join("pressure"), system);
            let tree = self.tree();
            for (leaf, bytes) in [
                ("app-so.tilo.mail-1/main", mail),
                ("app-so.tilo.code-2/main", code),
                ("app-so.tilo.code-2/job-1", job),
            ] {
                write(
                    &tree.join(leaf).join("memory.stat"),
                    &format!("anon {bytes}\nfile 7\n"),
                );
                write(
                    &tree.join(leaf).join("memory.current"),
                    &format!("{bytes}\n"),
                );
            }
        }

        fn source(&self) -> TreeSource {
            TreeSource {
                tree: self.tree(),
                system: self.dir.path().join("pressure"),
                shared: Arc::clone(&self.shared),
            }
        }
    }

    #[test]
    fn the_watch_reads_every_app_and_job_with_its_tier_and_label() {
        let fake = FakeTree::new();
        fake.sample(&psi(1.0, 0.0), 900 * MIB, 400 * MIB, 100 * MIB);
        let mut units = fake.source().units();
        units.sort_by(|a, b| a.dir.cmp(&b.dir));
        let seen: Vec<(String, Leaf, Tier, Option<String>, u64)> = units
            .iter()
            .map(|unit| {
                (
                    unit.app_id.clone(),
                    unit.leaf,
                    unit.tier,
                    unit.label.clone(),
                    unit.footprint,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "so.tilo.code".into(),
                    Leaf::Job,
                    Tier::Foreground,
                    Some("rust-analyzer".into()),
                    100 * MIB
                ),
                (
                    "so.tilo.code".into(),
                    Leaf::Main,
                    Tier::Foreground,
                    None,
                    400 * MIB
                ),
                (
                    "so.tilo.mail".into(),
                    Leaf::Main,
                    Tier::Background,
                    None,
                    900 * MIB
                ),
            ]
        );
        let system = fake.source().system().unwrap();
        assert!((system.some_avg10 - 1.0).abs() < f64::EPSILON, "{system:?}");
    }

    // USER REQUIREMENT (2026-10-19): "failures are bugs: disk full/OOM
    // crashing or blocking login is a bug to fix, never normal". Against a
    // fake PSI source: rising pressure reclaims from the background app,
    // critical pressure that stays ends the runaway job, and the focused
    // app and the session are never touched.
    #[test]
    fn the_watch_reclaims_then_ends_the_runaway_against_a_fake_psi_source() {
        let fake = FakeTree::new();
        let mut source = fake.source();
        let mut effects = Recorded::default();
        let mut policy = Policy::new();
        fake.sample(&psi(1.0, 0.0), 900 * MIB, 400 * MIB, 100 * MIB);
        step(&mut policy, &mut source, &mut effects).unwrap();
        assert!(effects.reclaimed.is_empty() && effects.ended.is_empty());

        fake.sample(&psi(20.0, 4.0), 900 * MIB, 400 * MIB, 500 * MIB);
        step(&mut policy, &mut source, &mut effects).unwrap();
        assert_eq!(
            effects.reclaimed,
            [(fake.tree().join("app-so.tilo.mail-1/main"), 900 * MIB / 4)]
        );

        for job in [900, 1300, 1700] {
            fake.sample(&psi(90.0, 50.0), 900 * MIB, 400 * MIB, job * MIB);
            step(&mut policy, &mut source, &mut effects).unwrap();
        }
        assert_eq!(
            effects.ended,
            [(fake.tree().join("app-so.tilo.code-2/job-1"), 1600 * MIB)],
            "the job that grew, not the larger background app"
        );
        assert!(
            effects
                .reclaimed
                .iter()
                .all(|(dir, _)| dir.starts_with(fake.tree().join("app-so.tilo.mail-1"))),
            "nothing is taken from the focused app: {:?}",
            effects.reclaimed
        );
    }

    #[test]
    fn an_ended_app_says_why_once_and_a_recorded_cause_stands() {
        let shared = Shared::default();
        let app = tempfile::tempdir().unwrap();
        assert_eq!(shared.take_end(app.path()), None);
        shared.record_end(app.path(), Cause::Runaway);
        shared.record_end(app.path(), Cause::Kernel);
        assert_eq!(shared.take_end(app.path()), Some(Cause::Runaway));
        assert_eq!(shared.take_end(app.path()), None, "asked once");
        // Not recorded, but the kernel killed in it: its events say so.
        write(
            &app.path().join("main/memory.events"),
            "low 0\nhigh 0\nmax 0\noom 1\noom_kill 4\noom_group_kill 1\n",
        );
        assert_eq!(shared.take_end(app.path()), Some(Cause::Kernel));
    }

    /// The machine's pressure file keeps every byte written but the last,
    /// which it replaces with a NUL; what it parses must be the trigger.
    #[test]
    fn the_written_trigger_survives_the_kernel_dropping_its_last_byte() {
        let (last, kept) = TRIGGER_WRITTEN.split_last().expect("not empty");
        assert_eq!(*last, 0);
        assert_eq!(kept, TRIGGER.as_bytes());
    }

    /// Arming the machine-wide file never fails as malformed. A host may
    /// refuse an unprivileged trigger outright or have no PSI, but a refused
    /// trigger string (`EINVAL`) is this code's fault.
    #[test]
    fn the_machine_wide_trigger_is_never_refused_as_malformed() {
        match arm(Path::new(SYSTEM_PRESSURE)) {
            Ok(_) => {}
            Err(error) => assert_ne!(
                error.raw_os_error(),
                Some(libc::EINVAL),
                "the kernel refused the trigger string: {error}"
            ),
        }
    }
}
