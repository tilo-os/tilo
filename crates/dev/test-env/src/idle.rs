//! How often idle threads wake. A thread that has nothing to do should be blocked until something
//! happens, and each time the kernel switches it out counts a context switch: voluntary when it
//! blocked (a timer, a poll, a condvar that someone keeps signalling), involuntary when it was
//! preempted while running. An idle thread that keeps counting is a thread that keeps waking, and
//! every wakeup costs a laptop's CPU the chance to stay in a deep sleep state. Two scales: one
//! thread of the test process ([`spawned_thread`], [`named_thread`], [`assert_stays_asleep`],
//! [`context_switches`], [`asleep_runs`]), or a whole process tree, such as a compositor the test
//! spawned and every app it started ([`idle_wakeups`] samples every thread of it over a window, as
//! [`Wakeups`]). Linux only: the counts come from `/proc`. Elsewhere the process-level calls return
//! an error instead of an empty sample, so a gate that cannot observe its subject fails instead of
//! passing.

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

pub fn context_switches(tid: &str) -> u64 {
    let status =
        std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).expect("thread status");
    let (voluntary, involuntary) = switch_counts(&status);
    voluntary.saturating_add(involuntary)
}

pub fn spawned_thread<T>(comm: &str, start: impl FnOnce() -> T) -> (T, String) {
    let before = own_thread_ids();
    let started = start();
    let what = format_args!(
        "exactly one new thread named {comm} (a second one that stays was started by another \
         test of this process, so which is this test's is unknown)"
    );
    let tid = crate::wait_for(what, || {
        let named: Vec<String> = own_thread_ids()
            .difference(&before)
            .filter(|tid| {
                std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                    .is_ok_and(|name| name.trim() == comm)
            })
            .cloned()
            .collect();
        <[String; 1]>::try_from(named).ok().map(|[tid]| tid)
    });
    (started, tid)
}

pub fn named_thread(comm: &str) -> String {
    named_threads(comm).unwrap_or_else(|named| panic!("{named} live threads named {comm}, not one"))
}

pub fn live_named(comm: &str) -> Option<String> {
    named_threads(comm).ok()
}

fn named_threads(comm: &str) -> Result<String, usize> {
    let named: Vec<_> = own_thread_ids()
        .into_iter()
        .filter(|tid| {
            std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                .is_ok_and(|name| name.trim() == comm)
        })
        .collect();
    match <[String; 1]>::try_from(named) {
        Ok([tid]) => Ok(tid),
        Err(named) => Err(named.len()),
    }
}

struct Sample {
    asleep: bool,
    switches: u64,
    runs: u64,
}

fn state_of(status: &str) -> Option<char> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("State:"))?
        .trim_start()
        .chars()
        .next()
}

fn sample(tid: &str) -> Option<Sample> {
    let status = std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).ok()?;
    let (voluntary, involuntary) = switch_counts(&status);
    let asleep = state_of(&status) == Some('S');
    let schedstat = std::fs::read_to_string(format!("/proc/self/task/{tid}/schedstat")).ok()?;
    let runs = schedstat.split_whitespace().nth(2)?.parse().ok()?;
    Some(Sample {
        asleep,
        switches: voluntary.saturating_add(involuntary),
        runs,
    })
}

fn asleep_count(tid: &str, pick: impl Fn(&Sample) -> u64) -> u64 {
    crate::wait_for(format_args!("thread {tid} to fall asleep"), || {
        assert!(
            std::path::Path::new(&format!("/proc/self/task/{tid}")).exists(),
            "thread {tid} has exited, so it can never fall asleep"
        );
        sample(tid)
            .filter(|sample| sample.asleep)
            .map(|sample| pick(&sample))
    })
}

pub fn asleep_switches(tid: &str) -> u64 {
    asleep_count(tid, |sample| sample.switches)
}

pub fn asleep_runs(tid: &str) -> u64 {
    let runs = asleep_count(tid, |sample| sample.runs);
    assert!(
        runs > 0,
        "thread {tid} has never been scheduled in, so the kernel's scheduling statistics \
         are off and this count cannot see its wakeups"
    );
    runs
}

pub fn blocked(tid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/self/task/{tid}/status"))
        .is_ok_and(|status| state_of(&status) == Some('S'))
}

pub fn assert_stays_asleep(tid: &str, what: &str) {
    let settled = asleep_runs(tid);
    std::thread::sleep(Duration::from_millis(400));
    let woke = asleep_runs(tid).saturating_sub(settled);
    assert_eq!(woke, 0, "idle {what} woke {woke} times in 400 ms");
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadSample {
    pub pid: u32,
    pub tid: u32,
    pub process: String,
    pub comm: String,
    pub voluntary: u64,
    pub involuntary: u64,
    pub cpu_ticks: u64,
}

pub const CPU_TICK: Duration = Duration::from_millis(10);

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

fn gone(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound) || error.raw_os_error() == Some(3)
}

fn read_comm(path: &str) -> io::Result<String> {
    Ok(std::fs::read_to_string(path)?.trim().to_owned())
}

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

pub fn current_thread_id() -> io::Result<u32> {
    if !cfg!(target_os = "linux") {
        return Err(unsupported());
    }
    std::fs::read_link("/proc/thread-self")?
        .file_name()
        .and_then(|name| name.to_str()?.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "/proc/thread-self"))
}

fn parent_of(pid: u32) -> io::Result<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("/proc/{pid}/stat")))
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeupRow {
    pub process: String,
    pub thread: String,
    pub threads: usize,
    pub voluntary: u64,
    pub involuntary: u64,
    pub cpu_ticks: u64,
}

impl WakeupRow {
    pub fn wakeups(&self) -> u64 {
        self.voluntary.saturating_add(self.involuntary)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Wakeups {
    pub window: Duration,
    pub rows: Vec<WakeupRow>,
    pub threads_started: usize,
    pub processes: usize,
}

impl Wakeups {
    pub fn total(&self) -> u64 {
        self.rows.iter().map(WakeupRow::wakeups).sum()
    }

    pub fn rate(&self, count: u64) -> f64 {
        let seconds = self.window.as_secs_f64();
        if seconds > 0.0 {
            f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / seconds
        } else {
            0.0
        }
    }

    pub fn per_second(&self) -> f64 {
        self.rate(self.total())
    }

    pub fn blocking(&self) -> u64 {
        self.rows.iter().map(|row| row.voluntary).sum()
    }

    pub fn blocking_per_second(&self) -> f64 {
        self.rate(self.blocking())
    }

    pub fn cpu_percent(&self, ticks: u64) -> f64 {
        let seconds = self.window.as_secs_f64();
        if seconds > 0.0 {
            f64::from(u32::try_from(ticks).unwrap_or(u32::MAX)) * CPU_TICK.as_secs_f64() * 100.0
                / seconds
        } else {
            0.0
        }
    }

    pub fn total_cpu_percent(&self) -> f64 {
        self.cpu_percent(self.rows.iter().map(|row| row.cpu_ticks).sum())
    }

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

const SAMPLE_EVERY: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Seen {
    process: String,
    comm: String,
    first: (u64, u64, u64),
    last: (u64, u64, u64),
}

#[derive(Default)]
struct Tally {
    seen: BTreeMap<(u32, u32), Seen>,
    threads_started: usize,
    processes: usize,
}

impl Tally {
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
    use crate::SpawnUnderEnvLock as _;

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
        assert_eq!(wakeups.blocking(), 10);
        assert!(
            (wakeups.blocking_per_second() - 5.0).abs() < 1e-9,
            "{wakeups}"
        );
        assert_eq!(wakeups.processes, 1);
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
            .spawn_locked()
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
    fn a_parked_thread_runs_once_per_wakeup() {
        use std::io::{Read as _, Write as _};
        use std::os::unix::net::UnixStream;

        let (mut driver, mut parked) = UnixStream::pair().expect("a socket pair");
        let (_, tid) = spawned_thread("idle-runs-probe", || {
            std::thread::Builder::new()
                .name("idle-runs-probe".into())
                .spawn(move || {
                    let mut byte = [0_u8; 1];
                    while parked.read(&mut byte).is_ok_and(|read| read == 1) {}
                })
                .expect("spawn the parked thread")
        });
        assert_stays_asleep(&tid, "a thread parked on a socket");
        let settled = asleep_runs(&tid);
        driver.write_all(&[1]).expect("wake the parked thread");
        assert_stays_asleep(&tid, "the thread after one wakeup");
        assert!(
            asleep_runs(&tid) > settled,
            "the write did not wake the parked thread"
        );
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
