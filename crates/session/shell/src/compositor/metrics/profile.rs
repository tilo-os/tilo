//! Profiling the live compositor on the device admin's request.
//!
//! The image keeps debuggers and `perf` off every process
//! (`kernel.yama.ptrace_scope = 3`, `kernel.perf_event_paranoid = 3`), the
//! compositor included, and that hardening stays. So the compositor profiles
//! itself, with the instruments it already has, for a window the admin asks
//! for through the owner-only `session-diagnostics` actions:
//!
//! - `frames.csv`: every loop tick, the same columns as `TILO_FRAME_TRACE`
//!   ([`super::CSV_HEADER`]), written by the same [`super::FrameRecorder`].
//! - `phases.csv`: every pump phase and checkpoint `TILO_LOOP_TIMING` times,
//!   keyed by the tick's `seq`, not only the slow ones it logs.
//! - `work.csv`: integer work counters keyed by the same tick; unlike wall
//!   time, these can be compared across machines for an identical workload.
//! - `threads.csv`: each thread's CPU, sampled from `/proc/self/task` by a
//!   thread of the profile's own, so the presenter, GPU and worker threads
//!   are seen as well as the loop.
//! - `summary.json`, written last: the frame summary, per-phase totals and
//!   per-thread CPU totals. Its presence means the profile is complete.
//!
//! Nothing here reaches outside the process: no ptrace, no
//! `perf_event_open`, only what the process can already read about itself.
//! While no profile runs, each recording site pays only an atomic load.

use std::collections::{BTreeMap, HashMap};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::FrameSample;
use super::trace_writer::TraceWriter;

/// A profile's length when the admin names none.
pub(crate) const DEFAULT_SECONDS: u64 = 30;
/// The longest profile one request records. A profile is a deliberate
/// window, not a standing trace (the session's rolling `frame-trace.csv` is
/// that); this bounds what a forgotten profile writes.
pub(crate) const MAX_SECONDS: u64 = 600;
/// Profiles kept under the profile directory; starting one removes the
/// oldest beyond this.
const KEPT_PROFILES: usize = 8;
/// How often the thread sampler reads every thread's CPU clock.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(50);
/// How often buffered rows are handed to their writer threads.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

const PHASES_HEADER: &str = "seq,phase,elapsed_us,items";
const THREADS_HEADER: &str = "t_us,tid,thread,cpu_us";
const WORK_HEADER: &str = "seq,counter,items";

/// Whether a profile is recording: the one load the loop pays per tick.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static PROFILER: Mutex<Profiler> = Mutex::new(Profiler {
    capture: None,
    last: None,
});

/// Whether a profile is recording now.
#[inline]
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// The directory profiles go to: `$XDG_STATE_HOME/tilo/profiles`.
pub(crate) fn default_root() -> Option<PathBuf> {
    tilo_paths::xdg::state_home().map(|state| state.join("tilo").join("profiles"))
}

/// Why a profile did not start.
#[derive(Debug)]
pub(crate) enum StartError {
    /// One is already recording; stop it first.
    AlreadyRecording(PathBuf),
    /// The profile's directory or files could not be created.
    Io(io::Error),
}

/// Start recording a profile of `length` into a new directory under `root`.
pub(crate) fn start(root: &Path, length: Duration) -> Result<serde_json::Value, StartError> {
    let mut profiler = lock();
    if let Some(capture) = profiler.capture.as_ref() {
        return Err(StartError::AlreadyRecording(capture.dir.clone()));
    }
    let capture = Capture::create(root, length).map_err(StartError::Io)?;
    profiler.capture = Some(capture);
    ACTIVE.store(true, Ordering::Relaxed);
    Ok(profiler.status(Instant::now()))
}

/// Stop the recording profile, if any, and say where it went.
pub(crate) fn stop() -> serde_json::Value {
    let mut profiler = lock();
    profiler.finish(false);
    profiler.status(Instant::now())
}

/// The session is ending: complete the recording profile before the
/// process exits, waiting for its files.
pub(crate) fn shutdown() {
    lock().finish(true);
}

/// What is recording, and the last profile finished.
pub(crate) fn status() -> serde_json::Value {
    lock().status(Instant::now())
}

/// When the recording profile ends, for the loop's park: it wakes once to
/// close it even when nothing else happens.
pub(crate) fn deadline() -> Option<Instant> {
    if !active() {
        return None;
    }
    lock().capture.as_ref().map(|capture| capture.deadline)
}

/// A tick numbered `seq` began; the phases recorded until the next carry it.
pub(crate) fn begin_tick(seq: u64) {
    if let Some(capture) = lock().capture.as_mut() {
        capture.seq = seq;
    }
}

/// One loop-timing phase or checkpoint span of the current tick.
pub(crate) fn record_phase(name: &'static str, elapsed: Duration, items: Option<usize>) {
    if !active() {
        return;
    }
    if let Some(capture) = lock().capture.as_mut() {
        capture.phase(name, elapsed, items);
    }
}

/// Record integer work, not elapsed time. Names describe operations only;
/// never include queries, resource labels or application/document identities.
/// A zero sample means observed no work; an absent counter is not a zero.
pub(crate) fn record_work(name: &'static str, items: usize) {
    if !active() {
        return;
    }
    if let Some(capture) = lock().capture.as_mut() {
        capture.work(name, items);
    }
}

/// The tick the frame recorder just closed. Ends the profile once its
/// window is over.
pub(crate) fn record_tick(sample: &FrameSample, target_period_us: u32) {
    let mut profiler = lock();
    let Some(capture) = profiler.capture.as_mut() else {
        return;
    };
    let now = Instant::now();
    capture.tick(sample, target_period_us, now);
    if now >= capture.deadline {
        profiler.finish(false);
    }
}

/// Close a profile whose window ended while the loop parked.
pub(crate) fn expire(now: Instant) {
    if !active() {
        return;
    }
    let mut profiler = lock();
    if profiler
        .capture
        .as_ref()
        .is_some_and(|capture| now >= capture.deadline)
    {
        profiler.finish(false);
    }
}

fn lock() -> std::sync::MutexGuard<'static, Profiler> {
    PROFILER.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Profiler {
    capture: Option<Capture>,
    last: Option<Finished>,
}

struct Finished {
    dir: PathBuf,
    seconds: f64,
    ticks: u64,
}

impl Profiler {
    fn finish(&mut self, wait: bool) {
        let Some(capture) = self.capture.take() else {
            return;
        };
        ACTIVE.store(false, Ordering::Relaxed);
        self.last = Some(capture.close(wait));
    }

    fn status(&self, now: Instant) -> serde_json::Value {
        let last = self.last.as_ref().map(|last| {
            serde_json::json!({
                "path": last.dir,
                "seconds": last.seconds,
                "ticks": last.ticks,
                "complete_when": last.dir.join("summary.json"),
            })
        });
        match self.capture.as_ref() {
            Some(capture) => serde_json::json!({
                "state": "recording",
                "path": capture.dir,
                "seconds_left": capture.deadline.saturating_duration_since(now).as_secs_f64(),
                "ticks": capture.ticks,
                "last": last,
            }),
            None => serde_json::json!({ "state": "idle", "last": last }),
        }
    }
}

#[derive(Default, serde::Serialize)]
struct PhaseTotal {
    count: u64,
    total_us: u64,
    max_us: u64,
}

#[derive(Default, serde::Serialize)]
struct WorkTotal {
    samples: u64,
    total: u64,
    max: u64,
}

/// One profile being recorded.
struct Capture {
    dir: PathBuf,
    started: Instant,
    deadline: Instant,
    next_flush: Instant,
    seq: u64,
    ticks: u64,
    frames: TraceWriter,
    phases: TraceWriter,
    phase_totals: BTreeMap<&'static str, PhaseTotal>,
    work: TraceWriter,
    work_totals: BTreeMap<&'static str, WorkTotal>,
    /// Every tick, for the summary. A profile is at most [`MAX_SECONDS`].
    samples: Vec<FrameSample>,
    target_period_us: u32,
    sampler: Sampler,
}

impl Capture {
    fn create(root: &Path, length: Duration) -> io::Result<Self> {
        DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        prune(root, KEPT_PROFILES.saturating_sub(1));
        let dir = new_profile_dir(root)?;
        let frames = csv_writer(&dir.join("frames.csv"), super::CSV_HEADER)?;
        let phases = csv_writer(&dir.join("phases.csv"), PHASES_HEADER)?;
        let work = csv_writer(&dir.join("work.csv"), WORK_HEADER)?;
        let threads = private_file(&dir.join("threads.csv"))?;
        let started = Instant::now();
        // A profile without its thread sampler could never publish a
        // complete summary, so the start fails here instead of recording a
        // profile whose threads.csv is never written.
        let sampler = Sampler::spawn(io::BufWriter::new(threads), started)?;
        Ok(Self {
            sampler,
            dir,
            started,
            deadline: started.checked_add(length).unwrap_or(started),
            next_flush: started,
            seq: 0,
            ticks: 0,
            frames,
            phases,
            phase_totals: BTreeMap::new(),
            work,
            work_totals: BTreeMap::new(),
            samples: Vec::new(),
            target_period_us: 0,
        })
    }

    fn phase(&mut self, name: &'static str, elapsed: Duration, items: Option<usize>) {
        let us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        let items = items.map(|n| n.to_string()).unwrap_or_default();
        let _ = writeln!(self.phases, "{},{name},{us},{items}", self.seq);
        let total = self.phase_totals.entry(name).or_default();
        total.count = total.count.saturating_add(1);
        total.total_us = total.total_us.saturating_add(us);
        total.max_us = total.max_us.max(us);
    }

    fn work(&mut self, name: &'static str, items: usize) {
        let items = u64::try_from(items).unwrap_or(u64::MAX);
        let _ = writeln!(self.work, "{},{name},{items}", self.seq);
        let total = self.work_totals.entry(name).or_default();
        total.samples = total.samples.saturating_add(1);
        total.total = total.total.saturating_add(items);
        total.max = total.max.max(items);
    }

    fn tick(&mut self, sample: &FrameSample, target_period_us: u32, now: Instant) {
        let _ = sample.write_csv_row(&mut self.frames);
        self.samples.push(*sample);
        self.ticks = self.ticks.saturating_add(1);
        self.target_period_us = target_period_us;
        if now >= self.next_flush {
            let _ = self.frames.flush();
            let _ = self.phases.flush();
            let _ = self.work.flush();
            self.next_flush = now.checked_add(FLUSH_INTERVAL).unwrap_or(now);
        }
    }

    /// Hand everything to the profile's own threads; the loop never waits
    /// on the disk for it.
    fn close(mut self, wait: bool) -> Finished {
        let _ = self.frames.flush();
        let _ = self.phases.flush();
        let _ = self.work.flush();
        let seconds = self.started.elapsed().as_secs_f64();
        let finished = Finished {
            dir: self.dir.clone(),
            seconds,
            ticks: self.ticks,
        };
        let summary = serde_json::json!({
            "seconds": seconds,
            "frames": super::summarize(&self.samples, self.target_period_us),
            "phases": self.phase_totals,
            "work_schema": 1,
            "work": self.work_totals,
            "build": {
                "debug_assertions": cfg!(debug_assertions),
                "target_arch": std::env::consts::ARCH,
                "target_os": std::env::consts::OS,
            },
        });
        let closing = Closing {
            frames: self.frames,
            phases: self.phases,
            work: self.work,
            summary,
            dir: self.dir,
        };
        self.sampler.close(closing, wait);
        finished
    }
}

/// What remains to write once recording ends.
struct Closing {
    frames: TraceWriter,
    phases: TraceWriter,
    work: TraceWriter,
    summary: serde_json::Value,
    dir: PathBuf,
}

impl Closing {
    /// Wait for the rows to reach their files, then write the summary, last
    /// and whole, so its presence means the profile is complete.
    fn complete(mut self, threads: BTreeMap<String, ThreadTotal>) {
        let traces = [self.frames.close(), self.phases.close(), self.work.close()];
        if let Some(error) = traces.into_iter().find_map(Result::err) {
            eprintln!(
                "compositor-profile: incomplete traces at {}: {error}",
                self.dir.display()
            );
            return;
        }
        let mut threads = threads.into_iter().collect::<Vec<_>>();
        threads.sort_by_key(|(_, total)| std::cmp::Reverse(total.cpu_us));
        let threads = threads
            .into_iter()
            .map(|(name, total)| {
                serde_json::json!({ "thread": name, "tids": total.tids, "cpu_us": total.cpu_us })
            })
            .collect::<serde_json::Value>();
        if let Some(summary) = self.summary.as_object_mut() {
            summary.insert("threads".into(), threads);
        }
        let path = self.dir.join("summary.json");
        let partial = self.dir.join("summary.json.partial");
        let written = serde_json::to_vec_pretty(&self.summary)
            .map_err(io::Error::other)
            .and_then(|bytes| {
                let mut file = private_file(&partial)?;
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&partial, &path));
        match written {
            Ok(()) => eprintln!("compositor-profile: complete at {}", self.dir.display()),
            Err(error) => eprintln!(
                "compositor-profile: summary for {} not written: {error}",
                self.dir.display()
            ),
        }
    }
}

#[derive(Default)]
struct ThreadTotal {
    tids: u32,
    cpu_us: u64,
}

/// The thread that samples every thread's CPU clock while a profile
/// records, and completes the profile when it ends.
struct Sampler {
    closing: Sender<Closing>,
    thread: std::thread::JoinHandle<()>,
}

impl Sampler {
    /// Sample into `out` on a thread of its own. Failing to spawn it is the
    /// caller's error: without it the profile has no thread trace.
    fn spawn(out: impl Write + Send + 'static, started: Instant) -> io::Result<Self> {
        let (closing, receive) = channel();
        let thread = std::thread::Builder::new()
            .name("compositor-profile".to_owned())
            .spawn(move || sample_threads(out, started, &receive))?;
        Ok(Self { closing, thread })
    }

    /// Hand the profile's completion to the sampler thread, which finishes
    /// the thread trace and then writes the summary; `wait` for it. A sampler
    /// that is gone (it stopped on a trace write, or panicked) writes no
    /// summary: its trace is short, so the profile is incomplete.
    fn close(self, closing: Closing, wait: bool) {
        let dir = closing.dir.clone();
        if self.closing.send(closing).is_err() {
            // Nobody is left to finish the profile, and the hand-over closes
            // the trace writers as it drops. The traces may be short, so no
            // summary may claim the profile complete.
            eprintln!(
                "compositor-profile: thread sampler stopped; {} left incomplete",
                dir.display()
            );
        }
        if wait && self.thread.join().is_err() {
            eprintln!(
                "compositor-profile: thread sampler panicked; {} left incomplete",
                dir.display()
            );
        }
    }
}

/// Sample every thread's CPU into `out` until the profile ends. The trace is
/// the sampler's to its last byte: a row or the final flush that cannot be
/// written leaves it short, so no summary may claim the profile complete.
fn sample_threads(mut out: impl Write, started: Instant, closing: &Receiver<Closing>) {
    // Each thread's CPU clock at the previous sample, and its name.
    let mut seen: HashMap<u32, (String, u64)> = HashMap::new();
    let mut totals: BTreeMap<String, ThreadTotal> = BTreeMap::new();
    let mut error = writeln!(out, "{THREADS_HEADER}").err();
    loop {
        if error.is_none() {
            let t_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            for (tid, cpu_us) in thread_cpu_clocks() {
                let (name, previous) = seen
                    .entry(tid)
                    .or_insert_with(|| (thread_name(tid), cpu_us));
                let delta = cpu_us.saturating_sub(*previous);
                *previous = cpu_us;
                if delta > 0 {
                    if let Err(failed) = writeln!(out, "{t_us},{tid},{name},{delta}") {
                        error = Some(failed);
                        break;
                    }
                    let total = totals.entry(name.clone()).or_default();
                    total.cpu_us = total.cpu_us.saturating_add(delta);
                }
            }
        }
        // Wait for the profile's end, waking every interval to sample. After
        // a failed write, keep waiting here rather than exiting, so the loop
        // still hands the completion over and never waits on the disk itself.
        let done = if error.is_none() {
            match closing.recv_timeout(SAMPLE_INTERVAL) {
                Ok(done) => Some(done),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match closing.recv() {
                Ok(done) => Some(done),
                Err(_) => return,
            }
        };
        let Some(done) = done else {
            continue;
        };
        if error.is_none() {
            error = out.flush().err();
        }
        drop(out);
        match error {
            Some(error) => eprintln!(
                "compositor-profile: thread trace for {} incomplete: {error}",
                done.dir.display()
            ),
            None => {
                for (name, _) in seen.values() {
                    let total = totals.entry(name.clone()).or_default();
                    total.tids = total.tids.saturating_add(1);
                }
                done.complete(totals);
            }
        }
        return;
    }
}

/// Every thread of this process and the CPU time it has used, in
/// microseconds: `schedstat`'s nanosecond run time where the kernel keeps
/// it, else `stat`'s user and system ticks.
fn thread_cpu_clocks() -> Vec<(u32, u64)> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    tasks
        .filter_map(Result::ok)
        .filter_map(|task| {
            let tid = task.file_name().to_str()?.parse::<u32>().ok()?;
            let cpu = std::fs::read_to_string(task.path().join("schedstat"))
                .ok()
                .and_then(|text| parse_schedstat_us(&text))
                .or_else(|| {
                    std::fs::read_to_string(task.path().join("stat"))
                        .ok()
                        .and_then(|text| parse_stat_us(&text, clock_ticks_per_second()))
                })?;
            Some((tid, cpu))
        })
        .collect()
}

fn thread_name(tid: u32) -> String {
    std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
        .map_or_else(|_| tid.to_string(), |name| csv_safe(name.trim()))
}

/// A thread name can hold any byte but NUL and `/`; a comma or newline would
/// split the row.
fn csv_safe(name: &str) -> String {
    name.chars()
        .map(|c| if c == ',' || c.is_control() { '_' } else { c })
        .collect()
}

fn parse_schedstat_us(text: &str) -> Option<u64> {
    let ns: u64 = text.split_whitespace().next()?.parse().ok()?;
    Some(ns / 1_000)
}

/// `utime + stime` from a `stat` line, in microseconds. The name field may
/// contain spaces and parentheses, so fields count from the last `)`.
fn parse_stat_us(text: &str, ticks_per_second: u64) -> Option<u64> {
    let (_, rest) = text.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    // After the name: state is field 3, utime field 14, stime field 15.
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    let ticks = utime.saturating_add(stime);
    ticks
        .saturating_mul(1_000_000)
        .checked_div(ticks_per_second)
}

fn clock_ticks_per_second() -> u64 {
    // SAFETY: `sysconf` reads a constant and has no preconditions.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100)
}

fn private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

fn csv_writer(path: &Path, header: &str) -> io::Result<TraceWriter> {
    let mut file = private_file(path)?;
    writeln!(file, "{header}")?;
    Ok(TraceWriter::to_file(file, path.to_path_buf()))
}

/// `compositor-<unix seconds>`, or with a suffix if two start in a second.
fn new_profile_dir(root: &Path) -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    for attempt in 0..100_u32 {
        let name = if attempt == 0 {
            format!("compositor-{stamp}")
        } else {
            format!("compositor-{stamp}-{attempt}")
        };
        let dir = root.join(name);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// Remove the oldest profiles so at most `keep` remain.
fn prune(root: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut profiles = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("compositor-")
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect::<Vec<_>>();
    if profiles.len() <= keep {
        return;
    }
    profiles.sort();
    let excess = profiles.len().saturating_sub(keep);
    for (_, path) in profiles.into_iter().take(excess) {
        if let Err(error) = std::fs::remove_dir_all(&path) {
            eprintln!(
                "compositor-profile: removing old profile {}: {error}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_counts_preserve_zero_samples_and_tick_attribution() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut capture = Capture::create(root.path(), Duration::from_secs(1)).expect("capture");
        capture.seq = 7;
        capture.work("test/candidates", 0);
        capture.work("test/candidates", 5);
        capture.seq = 9;
        capture.work("test/candidates", 2);
        let finished = capture.close(true);
        let summary: serde_json::Value = serde_json::from_slice(
            &std::fs::read(finished.dir.join("summary.json")).expect("completed profile"),
        )
        .expect("summary");
        assert_eq!(summary["work_schema"], 1);
        assert_eq!(
            summary["work"]["test/candidates"],
            serde_json::json!({"samples": 3, "total": 7, "max": 5})
        );
        assert!(summary["work"].get("not-observed").is_none());
        assert_eq!(
            std::fs::read_to_string(finished.dir.join("work.csv")).expect("work trace"),
            "seq,counter,items\n7,test/candidates,0\n7,test/candidates,5\n9,test/candidates,2\n"
        );
    }

    fn replace_sampler(capture: &mut Capture, sampler: Sampler) {
        let previous = std::mem::replace(&mut capture.sampler, sampler);
        drop(previous.closing);
        previous.thread.join().expect("previous sampler stopped");
    }

    #[test]
    fn a_failed_sampler_exits_when_its_recording_is_abandoned() {
        let (closing, receive) = channel::<Closing>();
        drop(closing);
        // A zero-sized output refuses the header. A disconnected recording
        // must still end the sampler rather than spinning on the closed channel.
        sample_threads(&mut [][..], Instant::now(), &receive);
    }

    /// A threads.csv sink that accepts every row, then refuses the last flush.
    struct RefusesFlush;

    impl Write for RefusesFlush {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("threads.csv flush failed"))
        }
    }

    #[test]
    fn a_thread_trace_that_cannot_flush_cannot_publish_a_complete_profile() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut capture = Capture::create(root.path(), Duration::from_secs(1)).expect("capture");
        replace_sampler(
            &mut capture,
            Sampler::spawn(RefusesFlush, Instant::now()).expect("sampler"),
        );
        let finished = capture.close(true);
        assert!(!finished.dir.join("summary.json").exists());
    }

    #[test]
    fn a_sampler_that_is_gone_cannot_publish_a_complete_profile() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut capture = Capture::create(root.path(), Duration::from_secs(1)).expect("capture");
        // The sampling thread stopped before the profile ended: its end of the
        // channel is gone, as it is when the thread panics. The hand-over used
        // to answer that by writing a summary with no threads in it.
        let (closing, receive) = channel::<Closing>();
        drop(receive);
        replace_sampler(
            &mut capture,
            Sampler {
                closing,
                thread: std::thread::spawn(|| {}),
            },
        );
        let finished = capture.close(false);
        assert!(!finished.dir.join("summary.json").exists());
    }

    #[test]
    fn a_failed_trace_cannot_publish_a_complete_profile() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut capture = Capture::create(root.path(), Duration::from_secs(1)).expect("capture");
        capture.work = TraceWriter::with_sink(|_| Err(io::Error::other("failed sink")));
        capture.work("test/candidates", 1);
        let finished = capture.close(true);
        assert!(!finished.dir.join("summary.json").exists());
    }

    #[test]
    fn stat_and_schedstat_parse_to_microseconds() {
        assert_eq!(parse_schedstat_us("12345678 999 7\n"), Some(12_345));
        // A thread name with spaces and a parenthesis.
        let stat = "4242 (render (gpu) 1) S 1 2 3 4 5 6 7 8 9 10 250 50 0 0 20 0";
        assert_eq!(parse_stat_us(stat, 100), Some(3_000_000));
        assert_eq!(parse_stat_us("garbage", 100), None);
        assert_eq!(csv_safe("a,b\nc"), "a_b_c");
    }

    #[test]
    fn pruning_keeps_the_newest_profiles() {
        let root = tempfile::tempdir().expect("tempdir");
        for n in 0..5_u64 {
            let profile = root.path().join(format!("compositor-{n}"));
            std::fs::create_dir(&profile).expect("mkdir");
            // Made one after another, a second apart.
            let made = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + n);
            std::fs::File::open(&profile)
                .and_then(|dir| dir.set_modified(made))
                .expect("date the profile");
        }
        std::fs::create_dir(root.path().join("unrelated")).expect("mkdir");
        prune(root.path(), 2);
        let mut left = std::fs::read_dir(root.path())
            .expect("read")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .into_string()
                    .expect("utf8")
            })
            .collect::<Vec<_>>();
        left.sort();
        assert_eq!(left, ["compositor-3", "compositor-4", "unrelated"]);
    }
}
