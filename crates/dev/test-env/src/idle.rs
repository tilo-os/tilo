//! How often idle threads wake.
//!
//! A thread that has nothing to do should be blocked until something
//! happens. Each time the kernel switches it out it counts a context switch,
//! voluntary when it blocked (a timer, a poll, a condvar that someone keeps
//! signalling) and involuntary when it was preempted while running. An idle
//! thread that keeps counting is a thread that keeps waking, and every
//! wakeup costs a laptop's CPU the chance to stay in a deep sleep state.
//!
//! Two scales:
//!
//! - one thread of the test process: [`spawned_thread`] finds the thread a
//!   service starts, [`named_thread`] one already running,
//!   [`assert_stays_asleep`] fails if it is scheduled at all
//!   while idle, and [`context_switches`] reads its count;
//! - a whole process tree, such as a compositor the test spawned and every
//!   app it started: [`idle_wakeups`] samples every thread of it over a
//!   window and returns per-thread rates, as [`Wakeups`].
//!
//! Linux only: the counts come from `/proc`. Elsewhere the process-level
//! calls return an error rather than an empty sample, so a gate that cannot
//! observe its subject fails instead of passing.

#![allow(
    clippy::panic,
    clippy::expect_used,
    reason = "test support: failing the calling test is the point"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::time::{Duration, Instant};

fn own_thread_ids() -> BTreeSet<String> {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .map(|entry| entry.expect("task").file_name().into_string().expect("tid"))
        .collect()
}

/// `(voluntary, involuntary)` context switches in a `/proc/.../status` text.
fn switch_counts(status: &str) -> (u64, u64) {
    let mut counts = (0, 0);
    for line in status.lines() {
        let value = || line.split_whitespace().nth(1)?.parse::<u64>().ok();
        if line.starts_with("voluntary_ctxt_switches") {
            counts.0 = value().unwrap_or(0);
        } else if line.starts_with("nonvoluntary_ctxt_switches") {
            counts.1 = value().unwrap_or(0);
        }
    }
    counts
}

/// Context switches, voluntary and involuntary, of thread `tid` of this
/// process so far.
///
/// # Panics
/// The thread's `/proc/self/task/<tid>/status` cannot be read: not Linux,
/// or no such thread.
pub fn context_switches(tid: &str) -> u64 {
    let status =
        std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).expect("thread status");
    let (voluntary, involuntary) = switch_counts(&status);
    voluntary.saturating_add(involuntary)
}

/// Run `start` and return its result with the id of the thread it spawned
/// under `comm` (the kernel's 15-byte truncation of the thread name).
///
/// # Panics
/// `/proc/self/task` cannot be listed, or no new thread of that name
/// appears within 5 s.
pub fn spawned_thread<T>(comm: &str, start: impl FnOnce() -> T) -> (T, String) {
    let before = own_thread_ids();
    let started = start();
    // A thread names itself once it runs, so it may not be named yet.
    let waiting = Instant::now();
    loop {
        let named = own_thread_ids()
            .difference(&before)
            .find(|tid| {
                std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                    .is_ok_and(|name| name.trim() == comm)
            })
            .cloned();
        if let Some(tid) = named {
            return (started, tid);
        }
        assert!(
            waiting.elapsed() < Duration::from_secs(5),
            "no new thread named {comm}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The id of this process's one live thread named `comm` (the kernel's
/// 15-byte truncation of the thread name).
///
/// # Panics
/// `/proc/self/task` cannot be listed, or not exactly one thread has that
/// name.
pub fn named_thread(comm: &str) -> String {
    let named: Vec<_> = own_thread_ids()
        .into_iter()
        .filter(|tid| {
            std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                .is_ok_and(|name| name.trim() == comm)
        })
        .collect();
    let [tid] = <[String; 1]>::try_from(named)
        .unwrap_or_else(|named| panic!("{} live threads named {comm}, not one", named.len()));
    tid
}

/// Fail if thread `tid` is scheduled at all during an idle 400 ms.
///
/// # Panics
/// The thread was scheduled, or its counts cannot be read.
pub fn assert_stays_asleep(tid: &str, what: &str) {
    std::thread::sleep(Duration::from_millis(50));
    let settled = context_switches(tid);
    std::thread::sleep(Duration::from_millis(400));
    let woke = context_switches(tid).saturating_sub(settled);
    assert_eq!(woke, 0, "idle {what} woke {woke} times in 400 ms");
}

/// One thread's context switches at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadSample {
    pub pid: u32,
    pub tid: u32,
    /// The process's name (`/proc/<pid>/comm`).
    pub process: String,
    /// The thread's name (`/proc/<pid>/task/<tid>/comm`).
    pub comm: String,
    pub voluntary: u64,
    pub involuntary: u64,
    /// CPU time the thread has run, user and system, in the kernel's
    /// `USER_HZ` ticks ([`CPU_TICK`] each).
    pub cpu_ticks: u64,
}

/// One `USER_HZ` tick of `/proc/.../stat` CPU time. `USER_HZ` is part of
/// the kernel's user ABI and 100 on every architecture Tilo runs on.
pub const CPU_TICK: Duration = Duration::from_millis(10);

/// User plus system CPU ticks in a `/proc/.../stat` text: fields 14 and 15,
/// counted after the parenthesised name, which may contain spaces.
fn cpu_ticks(stat: &str) -> u64 {
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return 0;
    };
    rest.split_whitespace()
        .skip(11)
        .take(2)
        .filter_map(|field| field.parse::<u64>().ok())
        .sum()
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "wakeup sampling reads /proc and needs Linux",
    )
}

/// A thread or process that exits between listing and reading is gone, not
/// an error.
fn gone(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound) || error.raw_os_error() == Some(3)
}

fn read_comm(path: &str) -> io::Result<String> {
    Ok(std::fs::read_to_string(path)?.trim().to_owned())
}

/// Context switches of every thread of `pid`.
///
/// # Errors
/// Not Linux, or `/proc/<pid>` cannot be read (the process is gone).
pub fn sample_process(pid: u32) -> io::Result<Vec<ThreadSample>> {
    if !cfg!(target_os = "linux") {
        return Err(unsupported());
    }
    let process = read_comm(&format!("/proc/{pid}/comm"))?;
    let mut samples = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/task"))? {
        let Some(tid) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let task = format!("/proc/{pid}/task/{tid}");
        let read = read_comm(&format!("{task}/comm")).and_then(|comm| {
            let status = std::fs::read_to_string(format!("{task}/status"))?;
            let stat = std::fs::read_to_string(format!("{task}/stat"))?;
            Ok((comm, status, stat))
        });
        match read {
            Ok((comm, status, stat)) => {
                let (voluntary, involuntary) = switch_counts(&status);
                samples.push(ThreadSample {
                    pid,
                    tid,
                    process: process.clone(),
                    comm,
                    voluntary,
                    involuntary,
                    cpu_ticks: cpu_ticks(&stat),
                });
            }
            Err(error) if gone(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(samples)
}

/// The kernel's id for the calling thread, as [`ThreadSample::tid`] names it,
/// so a test can leave the thread that drives it out of a sample.
///
/// # Errors
/// Not Linux.
pub fn current_thread_id() -> io::Result<u32> {
    if !cfg!(target_os = "linux") {
        return Err(unsupported());
    }
    // `/proc/thread-self` links to `<pid>/task/<tid>`.
    std::fs::read_link("/proc/thread-self")?
        .file_name()
        .and_then(|name| name.to_str()?.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "/proc/thread-self"))
}

/// The parent of `pid`, from `/proc/<pid>/stat` (the field after the
/// parenthesised name, which may itself contain spaces and parentheses).
fn parent_of(pid: u32) -> io::Result<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("/proc/{pid}/stat")))
}

/// `root` and every process descended from it.
///
/// # Errors
/// Not Linux, `/proc` cannot be listed, or `root` does not exist.
pub fn process_tree(root: u32) -> io::Result<Vec<u32>> {
    if !cfg!(target_os = "linux") {
        return Err(unsupported());
    }
    parent_of(root)?;
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for entry in std::fs::read_dir("/proc")? {
        let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        match parent_of(pid) {
            Ok(parent) => children.entry(parent).or_default().push(pid),
            Err(error) if gone(&error) => {}
            Err(error) => return Err(error),
        }
    }
    let mut tree = vec![root];
    let mut next = 0;
    while let Some(&pid) = tree.get(next) {
        tree.extend(children.get(&pid).into_iter().flatten().copied());
        next = next.saturating_add(1);
    }
    Ok(tree)
}

/// Every thread of `root`'s process tree. Processes that exit while it is
/// read are left out; `root` itself must still be there.
///
/// # Errors
/// As [`process_tree`] and [`sample_process`] for `root`.
pub fn sample_tree(root: u32) -> io::Result<Vec<ThreadSample>> {
    let mut samples = Vec::new();
    for pid in process_tree(root)? {
        match sample_process(pid) {
            Ok(threads) => samples.extend(threads),
            Err(error) if pid != root && gone(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(samples)
}

/// One process's threads of one name, over a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeupRow {
    pub process: String,
    pub thread: String,
    /// Threads of this name seen in the window.
    pub threads: usize,
    pub voluntary: u64,
    pub involuntary: u64,
    /// CPU time run in the window, in [`CPU_TICK`]s. A thread that spins
    /// without ever blocking makes few context switches; this shows it.
    pub cpu_ticks: u64,
}

impl WakeupRow {
    /// Voluntary and involuntary switches together.
    pub fn wakeups(&self) -> u64 {
        self.voluntary.saturating_add(self.involuntary)
    }
}

/// What a process tree did while idle, by process and thread name, busiest
/// first.
#[derive(Clone, Debug, PartialEq)]
pub struct Wakeups {
    pub window: Duration,
    pub rows: Vec<WakeupRow>,
    /// Threads that appeared during the window. A service that spawns a
    /// thread per tick shows up here even when each one is too short-lived
    /// to be sampled.
    pub threads_started: usize,
    /// Processes in the tree at the end of the window.
    pub processes: usize,
}

impl Wakeups {
    /// Context switches of the whole tree in the window.
    pub fn total(&self) -> u64 {
        self.rows.iter().map(WakeupRow::wakeups).sum()
    }

    /// `count` over the window, per second.
    pub fn rate(&self, count: u64) -> f64 {
        let seconds = self.window.as_secs_f64();
        if seconds > 0.0 {
            f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / seconds
        } else {
            0.0
        }
    }

    /// The whole tree's wakeups per second.
    pub fn per_second(&self) -> f64 {
        self.rate(self.total())
    }

    /// Voluntary switches of the whole tree in the window: how often its
    /// threads blocked again, which is once per wakeup.
    pub fn blocking(&self) -> u64 {
        self.rows.iter().map(|row| row.voluntary).sum()
    }

    /// The whole tree's voluntary switches per second.
    ///
    /// Unlike [`Wakeups::per_second`] this leaves out preemptions. A thread
    /// preempted while it handles one wakeup has still woken once, and how
    /// often it is preempted depends on what else the machine runs, so a
    /// gate on a shared runner that counts them measures the runner too. A
    /// thread that spins without blocking barely shows here; pair this with
    /// a bound on [`Wakeups::total_cpu_percent`].
    pub fn blocking_per_second(&self) -> f64 {
        self.rate(self.blocking())
    }

    /// Share of one core `ticks` of CPU time are over the window, in percent.
    pub fn cpu_percent(&self, ticks: u64) -> f64 {
        let seconds = self.window.as_secs_f64();
        if seconds > 0.0 {
            f64::from(u32::try_from(ticks).unwrap_or(u32::MAX)) * CPU_TICK.as_secs_f64() * 100.0
                / seconds
        } else {
            0.0
        }
    }

    /// The whole tree's CPU time over the window, in percent of one core.
    pub fn total_cpu_percent(&self) -> f64 {
        self.cpu_percent(self.rows.iter().map(|row| row.cpu_ticks).sum())
    }

    /// Wakeups per second of the threads of process `process`, by name.
    pub fn process_per_second(&self, process: &str) -> f64 {
        self.rate(
            self.rows
                .iter()
                .filter(|row| row.process == process)
                .map(WakeupRow::wakeups)
                .sum(),
        )
    }
}

impl fmt::Display for Wakeups {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "idle wakeups over {:.1} s: {:.2}/s and {:.1} % of a core across {} processes",
            self.window.as_secs_f64(),
            self.per_second(),
            self.total_cpu_percent(),
            self.processes,
        )?;
        writeln!(
            f,
            "  {:>9}  {:>9}  {:>6}  {:>7}  process / thread",
            "wakeups/s", "preempt/s", "cpu %", "threads"
        )?;
        for row in &self.rows {
            writeln!(
                f,
                "  {:>9.2}  {:>9.2}  {:>6.1}  {:>7}  {} / {}",
                self.rate(row.wakeups()),
                self.rate(row.involuntary),
                self.cpu_percent(row.cpu_ticks),
                row.threads,
                row.process,
                row.thread,
            )?;
        }
        write!(
            f,
            "  {} threads started during the window",
            self.threads_started
        )
    }
}

/// How often [`idle_wakeups`] looks at the tree within a window.
const SAMPLE_EVERY: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Seen {
    process: String,
    comm: String,
    first: (u64, u64, u64),
    last: (u64, u64, u64),
}

/// Every thread seen so far, from its first to its latest sample.
#[derive(Default)]
struct Tally {
    seen: BTreeMap<(u32, u32), Seen>,
    threads_started: usize,
    processes: usize,
}

impl Tally {
    /// Open the window: these threads count from where they are now.
    fn open(samples: Vec<ThreadSample>) -> Self {
        let mut tally = Self::default();
        for sample in samples {
            let counts = (sample.voluntary, sample.involuntary, sample.cpu_ticks);
            tally.seen.insert(
                (sample.pid, sample.tid),
                Seen {
                    process: sample.process,
                    comm: sample.comm,
                    first: counts,
                    last: counts,
                },
            );
        }
        tally
    }

    /// A later sample: known threads move on, new ones count from zero.
    fn observe(&mut self, samples: Vec<ThreadSample>) {
        self.processes = samples
            .iter()
            .map(|sample| sample.pid)
            .collect::<BTreeSet<_>>()
            .len();
        for sample in samples {
            let counts = (sample.voluntary, sample.involuntary, sample.cpu_ticks);
            let started = &mut self.threads_started;
            let entry = self
                .seen
                .entry((sample.pid, sample.tid))
                .or_insert_with(|| {
                    *started = started.saturating_add(1);
                    Seen::default()
                });
            // A thread renames itself once it runs; keep the latest name.
            entry.process = sample.process;
            entry.comm = sample.comm;
            entry.last = counts;
        }
    }

    fn close(self, window: Duration) -> Wakeups {
        let mut rows: BTreeMap<(String, String), WakeupRow> = BTreeMap::new();
        for thread in self.seen.into_values() {
            let row = rows
                .entry((thread.process.clone(), thread.comm.clone()))
                .or_insert_with(|| WakeupRow {
                    process: thread.process,
                    thread: thread.comm,
                    threads: 0,
                    voluntary: 0,
                    involuntary: 0,
                    cpu_ticks: 0,
                });
            row.threads = row.threads.saturating_add(1);
            row.voluntary = row
                .voluntary
                .saturating_add(thread.last.0.saturating_sub(thread.first.0));
            row.involuntary = row
                .involuntary
                .saturating_add(thread.last.1.saturating_sub(thread.first.1));
            row.cpu_ticks = row
                .cpu_ticks
                .saturating_add(thread.last.2.saturating_sub(thread.first.2));
        }
        let mut rows: Vec<WakeupRow> = rows.into_values().collect();
        rows.sort_by(|a, b| {
            b.wakeups()
                .cmp(&a.wakeups())
                .then_with(|| a.process.cmp(&b.process))
                .then_with(|| a.thread.cmp(&b.thread))
        });
        Wakeups {
            window,
            rows,
            threads_started: self.threads_started,
            processes: self.processes,
        }
    }
}

impl Wakeups {
    /// What happened between two samples of the same threads taken `window`
    /// apart, such as two [`sample_process`] calls around an idle stretch
    /// the caller drives itself. Threads only in `after` count from zero;
    /// threads only in `before` exited and are left out.
    pub fn between(before: Vec<ThreadSample>, after: Vec<ThreadSample>, window: Duration) -> Self {
        let live: BTreeSet<(u32, u32)> = after
            .iter()
            .map(|sample| (sample.pid, sample.tid))
            .collect();
        let mut tally = Tally::open(before);
        tally.seen.retain(|key, _| live.contains(key));
        tally.observe(after);
        tally.close(window)
    }
}

/// Wait `settle`, then count every context switch of every thread of
/// `root`'s process tree over `window`.
///
/// Threads present when the window opens count from their state then;
/// threads that start inside it count from zero, and one that exits keeps
/// what it had made when last sampled. The caller's own threads are not
/// part of the tree unless the caller is `root`'s descendant.
///
/// # Errors
/// Not Linux, or `root` exits before the window closes.
pub fn idle_wakeups(root: u32, settle: Duration, window: Duration) -> io::Result<Wakeups> {
    std::thread::sleep(settle);
    let mut tally = Tally::open(sample_tree(root)?);
    let started = Instant::now();
    loop {
        let remaining = window.saturating_sub(started.elapsed());
        std::thread::sleep(remaining.min(SAMPLE_EVERY));
        tally.observe(sample_tree(root)?);
        if remaining <= SAMPLE_EVERY {
            break;
        }
    }
    Ok(tally.close(started.elapsed()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_counts_both_kinds_of_switch() {
        let status =
            "Name:\tworker\nvoluntary_ctxt_switches:\t12\nnonvoluntary_ctxt_switches:\t3\n";
        assert_eq!(switch_counts(status), (12, 3));
    }

    #[test]
    fn stat_cpu_is_user_plus_system_after_a_name_with_spaces() {
        let stat = "42 (a (b) c) S 1 42 42 0 -1 4194560 100 0 0 0 7 5 0 0 20 0 1 0 1 0 0";
        assert_eq!(cpu_ticks(stat), 12);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_polling_thread_is_counted_and_named() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let stop = Arc::new(AtomicBool::new(false));
        let polling = Arc::clone(&stop);
        let poller = std::thread::Builder::new()
            .name("idle-probe-poll".into())
            .spawn(move || {
                while !polling.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
            .expect("spawn the polling thread");
        let wakeups = idle_wakeups(
            std::process::id(),
            Duration::from_millis(50),
            Duration::from_millis(500),
        )
        .expect("sample this process");
        stop.store(true, Ordering::Relaxed);
        poller.join().expect("polling thread");

        let row = wakeups
            .rows
            .iter()
            .find(|row| row.thread == "idle-probe-poll")
            .unwrap_or_else(|| panic!("the polling thread is in the table:\n{wakeups}"));
        // 100/s nominally; a loaded runner stretches the sleeps, never
        // shortens them.
        let rate = wakeups.rate(row.wakeups());
        assert!(
            (20.0..=150.0).contains(&rate),
            "a 10 ms poll measured {rate:.1}/s:\n{wakeups}"
        );
        assert!(wakeups.per_second() >= rate, "{wakeups}");
        assert!(wakeups.to_string().contains("idle-probe-poll"));
    }

    #[test]
    fn between_counts_new_threads_from_zero_and_drops_ended_ones() {
        let thread = |tid, comm: &str, voluntary| ThreadSample {
            pid: 1,
            tid,
            process: "app".into(),
            comm: comm.into(),
            voluntary,
            involuntary: 1,
            cpu_ticks: voluntary,
        };
        let wakeups = Wakeups::between(
            vec![thread(1, "main", 10), thread(2, "gone", 5)],
            vec![thread(1, "main", 14), thread(3, "worker", 6)],
            Duration::from_secs(2),
        );
        let rate = |name: &str| {
            let row = wakeups.rows.iter().find(|row| row.thread == name).unwrap();
            wakeups.rate(row.wakeups())
        };
        assert!((rate("main") - 2.0).abs() < 1e-9, "{wakeups}");
        assert!((rate("worker") - 3.5).abs() < 1e-9, "{wakeups}");
        assert!(!wakeups.rows.iter().any(|row| row.thread == "gone"));
        assert_eq!(wakeups.threads_started, 1);
        assert_eq!(wakeups.total(), 11);
        // Without the new worker's one preemption: 4 + 6 over 2 s.
        assert_eq!(wakeups.blocking(), 10);
        assert!(
            (wakeups.blocking_per_second() - 5.0).abs() < 1e-9,
            "{wakeups}"
        );
        assert_eq!(wakeups.processes, 1);
        // 4 + 6 ticks of 10 ms over 2 s.
        assert!(
            (wakeups.total_cpu_percent() - 5.0).abs() < 1e-9,
            "{wakeups}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_tree_includes_children() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn sleep");
        let tree = process_tree(std::process::id()).expect("this process's tree");
        let _ = child.kill();
        let _ = child.wait();
        assert!(tree.contains(&std::process::id()));
        assert!(tree.contains(&child.id()), "{tree:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_calling_thread_is_one_of_this_process() {
        let me = current_thread_id().expect("this thread's id");
        let threads = sample_process(std::process::id()).expect("sample this process");
        assert!(threads.iter().any(|thread| thread.tid == me), "{threads:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_missing_root_fails_rather_than_reporting_nothing() {
        assert!(idle_wakeups(u32::MAX, Duration::ZERO, Duration::from_millis(10)).is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn without_proc_sampling_fails() {
        assert!(sample_process(std::process::id()).is_err());
        assert!(idle_wakeups(std::process::id(), Duration::ZERO, Duration::ZERO).is_err());
    }
}
