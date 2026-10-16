//! Notice a compositor thread that has stopped for good, record it, and end
//! the process so the session recovers.
//!
//! A deadlocked compositor keeps DRM master and the VT. The screen freezes on
//! its last frame, Ctrl+Alt+Fn goes unread (the compositor thread reads the
//! keyboard), and nothing else on the machine can take the display back: the
//! only way out was the power button. The machine itself is fine, so this is
//! the process's own job, done on a thread of its own that the stuck one
//! cannot block.
//!
//! # What counts as hung
//!
//! The loop marks every turn: [`Watchdog::parking`] before it waits for
//! events, [`Watchdog::unparked`] when the wait returns. A loop parked in its
//! wait is idle, however long, and is never judged. A turn that has not ended
//! is judged by what the kernel says the thread is doing, sampled every
//! [`SAMPLE_PERIOD`] from `/proc/self/task/<tid>`:
//!
//! * running or runnable (`R`): working, or waiting for a core on a loaded
//!   machine. Slow is not stopped; it is never ended for it.
//! * stopped or traced (`T`, `t`), or any thread of the process under a
//!   tracer: someone is looking at it in a debugger.
//! * in uninterruptible sleep (`D`): inside the kernel, in I/O, reclaim or a
//!   driver. Ending the process could not finish before that returns (exit
//!   waits for every thread to leave the kernel), so it would hand nothing
//!   back; and a machine thrashing its memory puts a thread there for long
//!   stretches without anything being stuck. Never judged.
//! * asleep (`S`): blocked. Its context-switch count says whether it ran at
//!   all since the last sample. A wait with a deadline wakes the thread when
//!   the deadline passes, so a thread that has not run once in longer than the
//!   longest deadline any of its waits has is waiting for something that will
//!   not come: a lock it or a thread waiting on it holds, a reply that is
//!   never sent.
//!
//! That longest deadline is [`LONGEST_BOUNDED_WAIT`], the longest of the
//! waits the compositor thread makes with a bound:
//!
//! * [`SESSION_END_SAVE_FLUSH`], its own wait for queued saves at session
//!   end;
//! * [`GPU_JOB_TIMEOUT`], the GPU fences it may wait on, which the kernel's
//!   GPU scheduler signals, with an error, by its job timeout after a GPU
//!   hang and reset (ten seconds on amdgpu, shorter on the others);
//! * [`SEAT_CALL_TIMEOUT`], the calls libseat's logind backend makes to
//!   logind on this thread (taking a device on hotplug, completing a pause on
//!   a VT switch), bounded by sd-bus's default method-call timeout.
//!
//! [`HUNG_AFTER`] is one sample past it, so a wait that ends on time is
//! always seen to end.
//!
//! Evidence of a stall is only taken from samples the watchdog made on
//! time. When its own wait overran a sample period by another (the machine
//! was suspended, the whole process was stopped, or the watchdog thread
//! starved for a core) it starts over: what the thread did in the gap is
//! unknown. A session switched away to another VT parks its loop like any
//! idle one.
//!
//! # Cost
//!
//! The loop pays two atomic increments a turn and never a wake. The watchdog
//! samples once a [`SAMPLE_PERIOD`] while the loop turns, and sleeps without
//! a timer from the first sample that finds the loop parked until the loop
//! next wakes, so an idle desktop wakes it not at all.
//!
//! # What is not covered
//!
//! A source callback that blocks inside the event loop's dispatch is
//! dispatched while the loop counts as parked. The loop's callbacks only
//! raise flags for the turn's body by convention, so the work, and so the
//! hangs, are in the body.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use super::liveness::{self, Answer, ThreadReport};

/// How long the compositor waits at session end for the layout changes
/// queued for the state writer to reach the disk.
pub const SESSION_END_SAVE_FLUSH: Duration = Duration::from_secs(10);

/// The longest a GPU job may run before the kernel's GPU scheduler declares
/// a hang, resets the GPU and signals the job's fences with an error.
pub const GPU_JOB_TIMEOUT: Duration = Duration::from_secs(10);

/// sd-bus's default method-call timeout, which bounds the calls libseat's
/// logind backend makes to logind on the compositor thread.
pub const SEAT_CALL_TIMEOUT: Duration = Duration::from_secs(25);

/// The longest a wait on the compositor thread is bounded at. A thread that
/// has not run for longer is in a wait without a bound (see the module docs
/// for what sets it). A new bounded wait on the loop joins this list.
pub const LONGEST_BOUNDED_WAIT: Duration = longest(
    SESSION_END_SAVE_FLUSH,
    longest(GPU_JOB_TIMEOUT, SEAT_CALL_TIMEOUT),
);

const fn longest(a: Duration, b: Duration) -> Duration {
    if a.as_nanos() >= b.as_nanos() { a } else { b }
}

/// How often the watchdog looks while the loop is turning. It only sets how
/// late past [`LONGEST_BOUNDED_WAIT`] a hang is noticed.
pub const SAMPLE_PERIOD: Duration = Duration::from_secs(1);

/// How long the compositor thread must have slept without running once,
/// outside its loop's wait, to be hung.
pub const HUNG_AFTER: Duration = LONGEST_BOUNDED_WAIT.saturating_add(SAMPLE_PERIOD);

/// The exit status of a compositor its watchdog ended. The session wrapper
/// ends the login on it like on any exit it did not ask for, so the person
/// is back at the login screen rather than in front of a frozen display.
pub const HUNG_EXIT_STATUS: i32 = 70;

/// When the watchdog samples and when it judges a blocked thread hung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub period: Duration,
    pub hung_after: Duration,
    /// How long recovery waits for the thread dump. The report is one more
    /// wait on the way out and gets the same bound as any other.
    pub report_within: Duration,
}

impl Policy {
    /// The compositor's: [`SAMPLE_PERIOD`] and [`HUNG_AFTER`].
    pub const COMPOSITOR: Self = Self {
        period: SAMPLE_PERIOD,
        hung_after: HUNG_AFTER,
        report_within: LONGEST_BOUNDED_WAIT,
    };
}

/// A hang, as the watchdog found it.
#[derive(Debug, Clone)]
pub struct Hang {
    /// The watched thread.
    pub tid: i32,
    /// How long it had slept without running when judged hung (at least).
    pub blocked_for: Duration,
    /// Every thread of the process with its backtrace where one could be
    /// taken; empty when the dump did not finish in time.
    pub threads: Vec<ThreadReport>,
    /// The whole report as written to stderr.
    pub report: String,
}

/// What the loop and the watchdog thread share.
struct Shared {
    /// Bumped twice a turn: odd while the loop is parked in its wait.
    turns: AtomicU64,
    /// The watchdog sleeps without a timer until the loop next wakes.
    asleep: AtomicBool,
    stop: AtomicBool,
    lock: Mutex<()>,
    wake: Condvar,
}

impl Shared {
    fn notify(&self) {
        let _held = self.lock.lock();
        self.wake.notify_all();
    }
}

/// Watches the thread that started it until dropped.
pub struct Watchdog {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    /// Watch the calling thread, a loop that marks its turns with
    /// [`Self::parking`] and [`Self::unparked`]. When it is hung the report
    /// goes to stderr and `recover` runs on the watchdog's thread; after it
    /// returns the watchdog stops.
    ///
    /// # Errors
    ///
    /// The watchdog thread could not be started.
    pub fn watch_this_thread(
        policy: Policy,
        recover: impl FnOnce(&Hang) + Send + 'static,
    ) -> io::Result<Self> {
        liveness::install_capture_handler();
        let tid = liveness::gettid();
        let shared = Arc::new(Shared {
            turns: AtomicU64::new(0),
            asleep: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            lock: Mutex::new(()),
            wake: Condvar::new(),
        });
        let watching = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("tilo-watchdog".into())
            .spawn(move || watch(&watching, tid, policy, recover))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// The loop is about to wait for events.
    pub fn parking(&self) {
        self.shared.turns.fetch_add(1, Ordering::SeqCst);
    }

    /// The loop's wait returned; the turn's work begins.
    pub fn unparked(&self) {
        self.shared.turns.fetch_add(1, Ordering::SeqCst);
        if self.shared.asleep.load(Ordering::SeqCst)
            && self.shared.asleep.swap(false, Ordering::SeqCst)
        {
            self.shared.notify();
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.asleep.store(false, Ordering::SeqCst);
        self.shared.notify();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// End this process with [`HUNG_EXIT_STATUS`], without running anything of
/// it: destructors and exit handlers could wait on the very lock the
/// compositor thread is stuck on. The kernel closes every descriptor, which
/// gives DRM master and the session's devices back, and the seat manager
/// restores the VT once its session controller is gone.
pub fn end_process(hang: &Hang) {
    crate::stderr_guard::write_final(&format!(
        "tilo compositor: ending the hung compositor (exit status {HUNG_EXIT_STATUS}) so the \
         session returns to the login screen; thread {} had not run for {:.1} s\n",
        hang.tid,
        hang.blocked_for.as_secs_f64()
    ));
    // SAFETY: `_exit` takes a status and does not return; nothing in this
    // process runs after it.
    unsafe { libc::_exit(HUNG_EXIT_STATUS) }
}

fn watch(shared: &Shared, tid: i32, policy: Policy, recover: impl FnOnce(&Hang)) {
    let mut seen = shared.turns.load(Ordering::SeqCst);
    let mut stall: Option<Stall> = None;
    while !shared.stop.load(Ordering::SeqCst) {
        let waited_from = Instant::now();
        {
            let mut held = shared.lock.lock();
            if !shared.stop.load(Ordering::SeqCst) {
                let _ = shared.wake.wait_for(&mut held, policy.period);
            }
        }
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        if waited_from.elapsed() > policy.period.saturating_mul(2) {
            // This thread did not run on time: suspended, stopped or
            // starved. The watched thread's gap is unknown; start over.
            stall = None;
        }
        let turns = shared.turns.load(Ordering::SeqCst);
        if turns != seen {
            seen = turns;
            stall = None;
            continue;
        }
        if turns & 1 == 1 {
            // Parked for a whole period: idle until the loop wakes.
            stall = None;
            sleep_until_unparked(shared, seen);
            seen = shared.turns.load(Ordering::SeqCst);
            continue;
        }
        let Some(sample) = Sample::of(tid) else {
            stall = None;
            continue;
        };
        if let Some(blocked_for) = judge(&mut stall, &sample, Instant::now(), policy.hung_after) {
            recover(&record(tid, blocked_for, policy));
            return;
        }
    }
}

fn sleep_until_unparked(shared: &Shared, parked_at: u64) {
    let mut held = shared.lock.lock();
    shared.asleep.store(true, Ordering::SeqCst);
    // The loop bumps `turns` before it looks at `asleep`: either it sees the
    // flag and notifies under the lock held here, or this sees its turn.
    while shared.asleep.load(Ordering::SeqCst)
        && shared.turns.load(Ordering::SeqCst) == parked_at
        && !shared.stop.load(Ordering::SeqCst)
    {
        shared.wake.wait(&mut held);
    }
    shared.asleep.store(false, Ordering::SeqCst);
}

/// What the kernel says about the watched thread.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sample {
    /// The scheduler state letter.
    state: char,
    /// Voluntary plus involuntary context switches: it changes whenever the
    /// thread ran and left the core again.
    switches: u64,
    /// The process is under a tracer (a debugger).
    traced: bool,
}

impl Sample {
    fn of(tid: i32) -> Option<Self> {
        let task =
            |file: &str| std::fs::read_to_string(format!("/proc/self/task/{tid}/{file}")).ok();
        let process = std::fs::read_to_string("/proc/self/status").ok()?;
        Some(Self {
            state: state_of(&task("stat")?)?,
            switches: switches_of(&task("status")?)?,
            traced: status_field(&process, "TracerPid").is_some_and(|tracer| tracer != 0),
        })
    }
}

/// The state letter of a `/proc/<tid>/stat` line: the first field after the
/// parenthesised name, which may itself contain spaces and parentheses.
fn state_of(stat: &str) -> Option<char> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .next()?
        .chars()
        .next()
}

fn status_field(status: &str, name: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == name).then(|| value.trim().parse().ok())?
    })
}

fn switches_of(status: &str) -> Option<u64> {
    Some(
        status_field(status, "voluntary_ctxt_switches")?
            .saturating_add(status_field(status, "nonvoluntary_ctxt_switches")?),
    )
}

/// Since when the thread has slept without running, and its switch count
/// then.
#[derive(Debug, Clone, Copy)]
struct Stall {
    since: Instant,
    switches: u64,
}

/// Fold one sample of a turn that has not ended into `stall`; how long the
/// thread has slept without running when that is past `hung_after`.
fn judge(
    stall: &mut Option<Stall>,
    sample: &Sample,
    now: Instant,
    hung_after: Duration,
) -> Option<Duration> {
    if sample.traced || sample.state != 'S' {
        *stall = None;
        return None;
    }
    match stall {
        Some(blocked) if blocked.switches == sample.switches => {
            let blocked_for = now.saturating_duration_since(blocked.since);
            (blocked_for > hung_after).then_some(blocked_for)
        }
        _ => {
            *stall = Some(Stall {
                since: now,
                switches: sample.switches,
            });
            None
        }
    }
}

/// Write the hang and every thread's stack to stderr, past the relay thread
/// that would not live to forward it (`stderr_guard`). The dump runs on a
/// thread of its own and gets the same bound as any wait: recovery never
/// waits on diagnostics that cannot finish.
fn record(tid: i32, blocked_for: Duration, policy: Policy) -> Hang {
    let task = |file: &str| {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/{file}"))
            .map(|text| text.trim().to_string())
            .unwrap_or_default()
    };
    let header = format!(
        "tilo compositor: main loop hung: its thread {tid} has slept without running for {:.1} \
         s, outside its event loop's wait, in {}; no wait it makes lasts longer than {} s. Every \
         thread follows.",
        blocked_for.as_secs_f64(),
        task("wchan"),
        LONGEST_BOUNDED_WAIT.as_secs()
    );
    crate::stderr_guard::write_final(&format!("{header}\n"));
    let (sent, dumped) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("tilo-hang-dump".into())
        .spawn(move || {
            let _ = sent.send(liveness::dump_threads());
        });
    let threads = spawned
        .ok()
        .and_then(|_| dumped.recv_timeout(policy.report_within).ok())
        .unwrap_or_default();
    let listing = if threads.is_empty() {
        "the thread dump did not finish".to_string()
    } else {
        liveness::render(&Answer::Threads(threads.clone()))
    };
    crate::stderr_guard::write_final(&format!("tilo compositor: hung main loop: {listing}\n"));
    Hang {
        tid,
        blocked_for,
        threads,
        report: format!("{header}\n{listing}"),
    }
}

/// Make the calling thread wait for ever on a lock it holds itself, the way
/// a compositor that read-locks its workspace tree while it holds the write
/// did. For the watchdog's end-to-end test, through the development control
/// socket only.
#[inline(never)]
pub fn deadlock_this_thread_for_test() {
    let tree = parking_lot::RwLock::new(());
    let _writing = tree.write();
    let _reading = std::hint::black_box(&tree).read();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const FAST: Policy = Policy {
        period: Duration::from_millis(20),
        hung_after: Duration::from_millis(300),
        report_within: Duration::from_secs(60),
    };

    fn sample(state: char, switches: u64) -> Sample {
        Sample {
            state,
            switches,
            traced: false,
        }
    }

    #[test]
    fn only_a_thread_asleep_without_running_past_the_bound_is_hung() {
        let start = Instant::now();
        let at = |ms: u64| start.checked_add(Duration::from_millis(ms)).unwrap();
        let bound = Duration::from_millis(100);
        let mut stall = None;
        assert_eq!(judge(&mut stall, &sample('S', 5), at(0), bound), None);
        assert_eq!(
            judge(&mut stall, &sample('S', 5), at(100), bound),
            None,
            "at the bound"
        );
        assert_eq!(
            judge(&mut stall, &sample('S', 6), at(150), bound),
            None,
            "it ran: a bounded wait ended"
        );
        assert_eq!(judge(&mut stall, &sample('S', 6), at(200), bound), None);
        assert_eq!(
            judge(&mut stall, &sample('S', 6), at(251), bound),
            Some(Duration::from_millis(101)),
            "asleep since 150 without running once"
        );

        let mut stall = None;
        for ms in [0, 200, 400, 600] {
            assert_eq!(
                judge(&mut stall, &sample('D', 1), at(ms), bound),
                None,
                "in the kernel's uninterruptible sleep, which ending cannot cut short ({ms} ms)"
            );
        }

        let mut stall = None;
        for (ms, state) in [(0, 'S'), (200, 'R'), (400, 'S'), (450, 'S')] {
            assert_eq!(
                judge(&mut stall, &sample(state, 1), at(ms), bound),
                None,
                "running or runnable is working or starved, not hung ({ms} ms)"
            );
        }
        let mut stall = None;
        for (ms, state) in [(0, 'S'), (200, 't'), (400, 'T')] {
            assert_eq!(
                judge(&mut stall, &sample(state, 1), at(ms), bound),
                None,
                "stopped"
            );
        }
        let mut stall = None;
        let traced = Sample {
            traced: true,
            ..sample('S', 1)
        };
        assert_eq!(judge(&mut stall, &sample('S', 1), at(0), bound), None);
        assert_eq!(
            judge(&mut stall, &traced, at(500), bound),
            None,
            "under a debugger"
        );
    }

    #[test]
    fn proc_fields_parse() {
        assert_eq!(state_of("1234 (tilo (main) x) S 1 2 3"), Some('S'));
        assert_eq!(state_of("garbage"), None);
        let status = "Name:\tshell\nTracerPid:\t0\nvoluntary_ctxt_switches:\t40\nnonvoluntary_ctxt_switches:\t2\n";
        assert_eq!(switches_of(status), Some(42));
        assert_eq!(status_field(status, "TracerPid"), Some(0));
        assert!(
            Sample::of(liveness::gettid()).is_some(),
            "this thread samples"
        );
    }

    #[test]
    fn the_production_bound_is_past_every_bounded_wait() {
        for wait in [SESSION_END_SAVE_FLUSH, GPU_JOB_TIMEOUT, SEAT_CALL_TIMEOUT] {
            assert!(LONGEST_BOUNDED_WAIT >= wait, "{wait:?}");
        }
        assert!(HUNG_AFTER > LONGEST_BOUNDED_WAIT);
        assert_eq!(Policy::COMPOSITOR.hung_after, HUNG_AFTER);
    }

    /// The test thread holds `lock` and the watched loop blocks on it.
    #[inline(never)]
    fn blocked_on_held_lock(lock: &parking_lot::Mutex<()>) {
        drop(std::hint::black_box(lock.lock()));
    }

    #[test]
    fn a_deadlocked_turn_is_recorded_with_its_stack_and_recovered() {
        let lock = Arc::new(parking_lot::Mutex::new(()));
        let held = lock.lock();
        let (report_to, reports) = mpsc::channel::<Hang>();
        let worker_lock = Arc::clone(&lock);
        let worker = std::thread::Builder::new()
            .name("watched-loop".into())
            .spawn(move || {
                let watchdog = Watchdog::watch_this_thread(FAST, move |hang| {
                    let _ = report_to.send(hang.clone());
                })
                .unwrap();
                for _ in 0..3 {
                    watchdog.parking();
                    std::thread::sleep(Duration::from_millis(5));
                    watchdog.unparked();
                }
                blocked_on_held_lock(&worker_lock);
                drop(watchdog);
            })
            .unwrap();
        let hang = reports
            .recv_timeout(Duration::from_secs(60))
            .expect("the watchdog recovers a deadlocked loop");
        drop(held);
        worker.join().unwrap();
        assert!(hang.blocked_for > FAST.hung_after, "{hang:?}");
        let watched = hang
            .threads
            .iter()
            .find(|thread| thread.tid == hang.tid)
            .unwrap_or_else(|| panic!("the watched thread is in the dump: {}", hang.report));
        assert!(
            watched.backtrace.contains("blocked_on_held_lock"),
            "the dump shows where the loop waits:\n{}",
            hang.report
        );
        assert!(hang.report.contains("main loop hung"), "{}", hang.report);
    }

    #[test]
    fn idle_bounded_waits_and_work_are_not_hangs() {
        let (report_to, reports) = mpsc::channel::<Hang>();
        let worker = std::thread::Builder::new()
            .name("watched-loop".into())
            .spawn(move || {
                let watchdog = Watchdog::watch_this_thread(FAST, move |hang| {
                    let _ = report_to.send(hang.clone());
                })
                .unwrap();
                // Parked far past the bound: an idle desktop.
                watchdog.parking();
                std::thread::sleep(FAST.hung_after.saturating_mul(3));
                watchdog.unparked();
                // One turn of many waits, each within the bound.
                for _ in 0..8 {
                    std::thread::sleep(FAST.hung_after / 3);
                }
                // One turn of work longer than the bound.
                let busy = Instant::now();
                while busy.elapsed() < FAST.hung_after.saturating_mul(2) {
                    std::hint::spin_loop();
                }
                watchdog.parking();
                drop(watchdog);
            })
            .unwrap();
        worker.join().unwrap();
        assert!(
            reports.try_recv().is_err(),
            "idle, bounded waits and work are not hangs"
        );
    }
}
