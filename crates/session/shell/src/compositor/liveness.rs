//! Whether the compositor's main loop is still turning, answered from outside it.
//!
//! A control client talks to the main loop: the listener is accepted and every
//! request is executed there. When the loop deadlocks, a request is written
//! into a socket nobody reads and the client learns nothing but a timeout,
//! which looks like a slow machine, not a hang. This module answers on its own
//! threads instead, next to the control socket, at `<control socket>.live`
//! (see [`socket_path`]).
//!
//! Requests are one JSON object per line:
//!
//! * `{"op":"heartbeat","budget_ms":500}` asks the main loop to acknowledge a
//!   wake through a calloop ping source, and answers `{"alive":{"ticks":N,
//!   "latency_ms":F}}` once it does. If it has not within `budget_ms` the
//!   answer is `{"stalled":{...}}`: the budget, the ticks so far, what the
//!   main thread did meanwhile, and every thread's state and kernel wait
//!   channel. When the main thread is blocked it also carries the backtraces,
//!   which is where a deadlock shows who waits on what; a main thread still
//!   working is slow, not stuck, and is not interrupted. The same report goes
//!   to stderr.
//! * `{"op":"threads"}` answers `{"threads":[...]}` whatever the loop is doing.
//!
//! A heartbeat costs the loop one wake when somebody asks. Nothing runs, and
//! the idle loop is not woken, while no client is connected.
//!
//! Like the control socket this exists only where
//! [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`] holds, and is bound only
//! when the control socket is. Every build, release included, also runs
//! [`super::watchdog`], which needs no client: it notices a hung loop on its
//! own, records the same thread dump and ends the process so the session
//! recovers.
//!
//! # Backtraces of other threads
//!
//! A thread cannot be unwound from outside. Each one is sent a real-time
//! signal whose handler captures its own backtrace; the answering thread
//! symbolizes them. Capturing allocates, which a signal handler may not do in
//! general: a thread interrupted inside the allocator waits on itself, for
//! ever, on the allocator lock it already holds, and every other thread that
//! allocates from that arena joins it.
//!
//! That is not hypothetical. The heartbeat once dumped on every wake over its
//! budget, and on a loaded machine a main loop merely busy for half a second
//! is usually allocating: interrupted there, the probe froze the desktop it
//! was measuring. So only a thread blocked inside a system call is
//! interrupted (`/proc/<tid>/syscall` names one), and not one inside the
//! memory system calls the allocator makes while it holds its lock. A thread
//! that is running, faulting in a page, or allocating is reported by its state
//! and wait channel alone, and the heartbeat does not dump at all while the
//! main thread is still working. A thread that does not answer within
//! [`DUMP_WAIT`] is reported as such rather than waited for.

use std::backtrace::Backtrace;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::{self, LoopHandle};

use super::native::UnlinkOnDrop;
use super::state::State;

/// The suffix that turns the control socket's path into the liveness socket's.
pub const SOCKET_SUFFIX: &str = ".live";

/// The budget of a heartbeat that names none.
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(500);

/// The most budget a request may ask for.
const MAX_BUDGET: Duration = Duration::from_secs(60);

/// How long a thread dump waits for the threads' answers.
const DUMP_WAIT: Duration = Duration::from_secs(2);

/// Where the liveness socket of the control socket at `control` is bound.
#[must_use]
pub fn socket_path(control: &Path) -> PathBuf {
    let mut name = control.as_os_str().to_owned();
    name.push(SOCKET_SUFFIX);
    PathBuf::from(name)
}

/// One request line.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Heartbeat {
        #[serde(default)]
        budget_ms: Option<u64>,
    },
    Threads,
}

/// One thread of the compositor process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadReport {
    pub tid: i32,
    /// The kernel's name for it (`comm`), at most 15 characters.
    pub name: String,
    /// The scheduler state letter: `R` running, `S` sleeping, `D` in an
    /// uninterruptible wait.
    pub state: String,
    /// The kernel function it sleeps in, when the kernel says.
    pub wait: String,
    /// Its symbolized backtrace, innermost frame first, or why there is none.
    pub backtrace: String,
}

/// An answer line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Alive {
        /// Wakes the main loop has acknowledged since it started.
        ticks: u64,
        /// How long it took this one.
        latency_ms: f64,
    },
    Stalled {
        /// What the request allowed.
        budget_ms: u64,
        ticks: u64,
        /// CPU the main thread used while the heartbeat waited, in
        /// milliseconds. Near zero, the loop is blocked (a deadlock, a lock,
        /// I/O); near the budget it is working, just not returning: slow, or
        /// spinning.
        main_thread_cpu_ms: u64,
        /// Time the main thread spent runnable but not running, waiting for a
        /// core, while the heartbeat waited, in milliseconds. A starved loop is
        /// slow, not stopped: it will run when the machine lets it. A blocked
        /// one sleeps, and waits for no core. Zero where the kernel keeps no
        /// scheduler statistics.
        #[serde(default)]
        main_thread_starved_ms: u64,
        threads: Vec<ThreadReport>,
    },
    Threads(Vec<ThreadReport>),
    Err(String),
}

impl Answer {
    /// A stall in which the main thread was still making or about to make
    /// progress: it used CPU, or waited for one, for a quarter of the budget.
    /// Slow, not hung: only a thread that neither ran nor waited to run is
    /// blocked.
    #[must_use]
    pub fn stalled_but_working(&self) -> bool {
        match self {
            Self::Stalled {
                budget_ms,
                main_thread_cpu_ms,
                main_thread_starved_ms,
                ..
            } => {
                let quarter = budget_ms / 4;
                *main_thread_cpu_ms >= quarter || *main_thread_starved_ms >= quarter
            }
            _ => false,
        }
    }
}

/// What the main loop and the answering threads share.
struct Beat {
    /// Heartbeats asked for; each is a ping to the loop.
    requested: AtomicU64,
    /// Wakes the loop has acknowledged; with `acked`'s condvar the answering
    /// thread waits on it.
    acked: Mutex<u64>,
    wake: Condvar,
}

impl Beat {
    /// Runs on the main loop: acknowledges every heartbeat asked for so far.
    fn acknowledge(&self) {
        let upto = self.requested.load(Ordering::SeqCst);
        {
            let mut acked = self.acked.lock();
            *acked = (*acked).max(upto);
        }
        self.wake.notify_all();
    }

    /// Ask the loop to acknowledge and wait up to `budget` for it.
    fn heartbeat(&self, ping: &calloop::ping::Ping, budget: Duration) -> Option<Duration> {
        let started = Instant::now();
        let want = self
            .requested
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        ping.ping();
        let deadline = started.checked_add(budget)?;
        let mut acked = self.acked.lock();
        while *acked < want {
            if self.wake.wait_until(&mut acked, deadline).timed_out() && *acked < want {
                return None;
            }
        }
        Some(started.elapsed())
    }

    fn ticks(&self) -> u64 {
        *self.acked.lock()
    }
}

/// Keeps the liveness socket bound; unlinks it when dropped.
pub struct Server {
    _unlink: UnlinkOnDrop,
}

/// Bind the liveness socket for the control socket at `control` and start
/// answering on it. `None`, with the reason on stderr, when it cannot be
/// bound: a harness without liveness still drives the desktop.
pub fn serve(control: &Path, handle: &LoopHandle<'static, State>) -> Option<Server> {
    let path = socket_path(control);
    let (ping, source) = match calloop::ping::make_ping() {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("tilo compositor: liveness ping: {error}");
            return None;
        }
    };
    let beat = Arc::new(Beat {
        requested: AtomicU64::new(0),
        acked: Mutex::new(0),
        wake: Condvar::new(),
    });
    let on_loop = Arc::clone(&beat);
    if let Err(error) = handle.insert_source(source, move |(), (), _| on_loop.acknowledge()) {
        eprintln!("tilo compositor: liveness source: {error}");
        return None;
    }
    let listener = match crate::shell::test_control_socket::bind_private(&path) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("tilo compositor: liveness bind {}: {error}", path.display());
            return None;
        }
    };
    let unlink = UnlinkOnDrop(path.clone());
    install_capture_handler();
    let spawned = std::thread::Builder::new()
        .name("tilo-liveness".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let (beat, ping) = (Arc::clone(&beat), ping.clone());
                // A connection of its own, so a slow thread dump for one
                // client never delays another's heartbeat.
                let _ = std::thread::Builder::new()
                    .name("tilo-liveness-conn".into())
                    .spawn(move || answer(stream, &beat, &ping));
            }
        });
    if let Err(error) = spawned {
        eprintln!("tilo compositor: liveness thread: {error}");
        return None;
    }
    eprintln!("tilo compositor liveness listening on {}", path.display());
    Some(Server { _unlink: unlink })
}

fn answer(stream: UnixStream, beat: &Beat, ping: &calloop::ping::Ping) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let answer = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Heartbeat { budget_ms }) => {
                let budget = budget_ms
                    .map_or(DEFAULT_BUDGET, Duration::from_millis)
                    .min(MAX_BUDGET);
                heartbeat_answer(beat, ping, budget)
            }
            Ok(Request::Threads) => Answer::Threads(dump_threads()),
            Err(error) => Answer::Err(format!("bad liveness request: {error}")),
        };
        let Ok(mut encoded) = serde_json::to_vec(&answer) else {
            return;
        };
        encoded.push(b'\n');
        if writer.write_all(&encoded).is_err() {
            return;
        }
    }
}

/// Ask the loop to acknowledge within `budget`, and answer how it went.
fn heartbeat_answer(beat: &Beat, ping: &calloop::ping::Ping, budget: Duration) -> Answer {
    let cpu_before = main_thread_cpu_ms();
    let starved_before = main_thread_starved_ms();
    if let Some(latency) = beat.heartbeat(ping, budget) {
        return Answer::Alive {
            ticks: beat.ticks(),
            latency_ms: latency.as_secs_f64() * 1000.0,
        };
    }
    // Before the dump: it takes time of its own.
    let main_thread_cpu_ms = main_thread_cpu_ms().saturating_sub(cpu_before);
    let main_thread_starved_ms = main_thread_starved_ms().saturating_sub(starved_before);
    let mut answer = Answer::Stalled {
        budget_ms: u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
        ticks: beat.ticks(),
        main_thread_cpu_ms,
        main_thread_starved_ms,
        threads: Vec::new(),
    };
    // A loop still working is slow and will be asked again; interrupting it
    // could stop it inside the allocator for good (see the module docs).
    let threads = if answer.stalled_but_working() {
        list_threads()
    } else {
        dump_threads()
    };
    if let Answer::Stalled {
        threads: listed, ..
    } = &mut answer
    {
        *listed = threads;
    }
    eprintln!("tilo compositor: main loop stalled\n{}", render(&answer));
    answer
}

/// A stall or thread listing as the text a person reads.
#[must_use]
pub fn render(answer: &Answer) -> String {
    let (head, threads) = match answer {
        Answer::Stalled {
            budget_ms,
            ticks,
            main_thread_cpu_ms,
            main_thread_starved_ms,
            threads,
        } => (
            format!(
                "main loop did not acknowledge a wake within {budget_ms} ms (acknowledged {ticks} \
                 before); the main thread used {main_thread_cpu_ms} ms of CPU and waited \
                 {main_thread_starved_ms} ms for a core meanwhile: {}",
                if *main_thread_cpu_ms >= budget_ms / 4 {
                    "it is busy"
                } else if *main_thread_starved_ms >= budget_ms / 4 {
                    "it is starved of CPU"
                } else {
                    "it is blocked"
                }
            ),
            threads,
        ),
        Answer::Threads(threads) => ("threads".to_string(), threads),
        Answer::Alive { ticks, latency_ms } => {
            return format!("main loop alive: {ticks} ticks, last wake took {latency_ms:.1} ms");
        }
        Answer::Err(message) => return message.clone(),
    };
    let mut out = head;
    for thread in threads {
        let _ = write!(
            out,
            "\n\nthread {} \"{}\" state {} waiting in {}\n{}",
            thread.tid, thread.name, thread.state, thread.wait, thread.backtrace
        );
    }
    out
}

/// The real-time signal whose handler captures a thread's backtrace.
fn capture_signal() -> libc::c_int {
    libc::SIGRTMIN().saturating_add(4)
}

/// Backtraces the handler left, by thread. Reserved before the signals are
/// sent so the handler never grows it.
static CAPTURED: Mutex<Vec<(i32, Backtrace)>> = Mutex::new(Vec::new());

pub(super) fn gettid() -> i32 {
    // SAFETY: `gettid` takes no arguments and cannot fail.
    i32::try_from(unsafe { libc::syscall(libc::SYS_gettid) }).unwrap_or(-1)
}

extern "C" fn capture_backtrace(_signal: libc::c_int) {
    let tid = gettid();
    let backtrace = Backtrace::force_capture();
    if let Some(mut captured) = CAPTURED.try_lock()
        && captured.len() < captured.capacity()
    {
        captured.push((tid, backtrace));
    }
}

pub(super) fn install_capture_handler() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: a zeroed `sigaction` is valid; the handler is an
        // `extern "C" fn(c_int)` and stays valid for the process's life.
        let status = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = capture_backtrace as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(capture_signal(), &raw const action, std::ptr::null_mut())
        };
        if status != 0 {
            eprintln!(
                "tilo compositor: liveness backtrace handler: {}",
                std::io::Error::last_os_error()
            );
        }
    });
}

/// Time the main thread has spent runnable and waiting for a core, in
/// milliseconds, per `/proc/<pid>/task/<tid>/schedstat` (`run_ns wait_ns
/// timeslices`). Zero when the kernel has no scheduler statistics.
fn main_thread_starved_ms() -> u64 {
    // SAFETY: `getpid` takes no arguments and cannot fail.
    let pid = unsafe { libc::getpid() };
    std::fs::read_to_string(format!("/proc/self/task/{pid}/schedstat"))
        .ok()
        .and_then(|stat| stat.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |wait_ns| wait_ns / 1_000_000)
}

/// CPU time the main thread (the thread whose id is the process id) has used,
/// in milliseconds, at the kernel's clock-tick resolution.
fn main_thread_cpu_ms() -> u64 {
    // SAFETY: `getpid` takes no arguments and cannot fail.
    let pid = unsafe { libc::getpid() };
    let stat = std::fs::read_to_string(format!("/proc/self/task/{pid}/stat")).unwrap_or_default();
    // After the parenthesised name: state is field 0, utime 11, stime 12.
    let ticks: u64 = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().skip(11).take(2))
        .into_iter()
        .flatten()
        .filter_map(|field| field.parse::<u64>().ok())
        .sum();
    // SAFETY: `sysconf` with a valid name has no preconditions.
    let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).unwrap_or(100);
    ticks
        .saturating_mul(1000)
        .checked_div(hz.max(1))
        .unwrap_or(0)
}

/// Why a thread may not be interrupted to capture its stack, or `None` when it
/// may: it is blocked inside a system call (`/proc/<tid>/syscall` gives its
/// number) other than the memory calls the allocator makes while it holds its
/// lock. A running thread, one blocked outside a system call (a page fault),
/// and one inside such a memory call may be inside the allocator, where the
/// capture would wait on itself for ever.
fn not_interruptible(syscall: &str) -> Option<String> {
    /// The system calls glibc's allocator makes with an arena locked.
    const ALLOCATOR_SYSCALLS: [libc::c_long; 6] = [
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_mprotect,
        libc::SYS_madvise,
        libc::SYS_brk,
    ];
    let first = syscall.split_whitespace().next().unwrap_or("");
    match first.parse::<libc::c_long>() {
        Ok(number) if number >= 0 && !ALLOCATOR_SYSCALLS.contains(&number) => None,
        Ok(number) if number >= 0 => Some(format!(
            "it was in memory system call {number}, which the allocator makes with its lock held"
        )),
        Ok(_) => Some("it was blocked outside a system call (a page fault)".into()),
        Err(_) if first == "running" => Some("it was running".into()),
        Err(_) => Some(format!("its system call state is unreadable ({first:?})")),
    }
}

/// [`not_interruptible`] for a thread whose system call cannot be read,
/// from the kernel function it sleeps in (`/proc/<tid>/wchan`): only the
/// waits a blocked thread makes on a lock, a condvar, its event loop, a timer,
/// a pipe or a socket, none of which the allocator makes with its lock held.
fn not_interruptible_waiting_in(wchan: &str) -> Option<String> {
    const BLOCKING_WAITS: [&str; 14] = [
        "do_epoll_wait",
        "ep_poll",
        "do_sys_poll",
        "do_poll",
        "do_select",
        "core_sys_select",
        "do_nanosleep",
        "hrtimer_nanosleep",
        "pipe_read",
        "pipe_wait_readable",
        "wait_woken",
        "unix_stream_data_wait",
        "unix_stream_read_generic",
        "skb_wait_for_more_packets",
    ];
    if wchan.starts_with("futex_") || BLOCKING_WAITS.contains(&wchan) {
        None
    } else if wchan.is_empty() || wchan == "0" {
        Some("it was running, and its system call state is unreadable".into())
    } else {
        Some(format!(
            "it was waiting in {wchan}, which may be inside the allocator, and its system \
             call state is unreadable"
        ))
    }
}

/// Every thread of this process by its scheduler state and what it waits in,
/// without interrupting any of them.
fn list_threads() -> Vec<ThreadReport> {
    let tids = thread_ids();
    tids.iter()
        .map(|&tid| {
            report(
                tid,
                "not captured: the main loop is still working, so no thread is interrupted".into(),
            )
        })
        .collect()
}

/// The ids of this process's threads, in order.
fn thread_ids() -> Vec<i32> {
    let mut tids: Vec<i32> = std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect();
    tids.sort_unstable();
    tids
}

/// One thread's report: its name, state and wait channel from `/proc`, with
/// `backtrace`.
fn report(tid: i32, backtrace: String) -> ThreadReport {
    let task = |file: &str| {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/{file}")).unwrap_or_default()
    };
    let state = task("stat")
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_string))
        .unwrap_or_else(|| "?".into());
    ThreadReport {
        tid,
        name: task("comm").trim().to_string(),
        state,
        wait: task("wchan").trim().to_string(),
        backtrace,
    }
}

/// Every thread of this process: its scheduler state, what it waits in and,
/// for each one blocked where interrupting it is safe ([`not_interruptible`]),
/// its backtrace.
pub(super) fn dump_threads() -> Vec<ThreadReport> {
    // One dump at a time: they share the capture slot.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock();

    // SAFETY: `getpid` takes no arguments and cannot fail.
    let pid = unsafe { libc::getpid() };
    let me = gettid();
    let tids = thread_ids();

    {
        let mut captured = CAPTURED.lock();
        captured.clear();
        captured.reserve(tids.len().saturating_add(8));
    }
    let mut signalled = 0usize;
    let mut left_alone: Vec<(i32, String)> = Vec::new();
    for &tid in tids.iter().filter(|&&tid| tid != me) {
        // A process that holds key material makes itself undumpable, which
        // hands its `/proc` entries to root: `syscall` (0400) can no longer
        // be read, `wchan` (0444) still can.
        let why = match std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall")) {
            Ok(syscall) => not_interruptible(&syscall),
            Err(_) => not_interruptible_waiting_in(
                std::fs::read_to_string(format!("/proc/self/task/{tid}/wchan"))
                    .unwrap_or_default()
                    .trim(),
            ),
        };
        if let Some(why) = why {
            left_alone.push((tid, why));
            continue;
        }
        // SAFETY: `tgkill` on a thread id of this process; the thread may have
        // exited since the listing, which fails harmlessly.
        let sent = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, capture_signal()) };
        if sent == 0 {
            signalled = signalled.saturating_add(1);
        }
    }
    let own = Backtrace::force_capture();
    let deadline = Instant::now()
        .checked_add(DUMP_WAIT)
        .unwrap_or_else(Instant::now);
    while CAPTURED.lock().len() < signalled && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let captured = std::mem::take(&mut *CAPTURED.lock());
    let mut own = Some(own);

    tids.iter()
        .map(|&tid| {
            let backtrace = if tid == me {
                own.take().map(|bt| frames_of(&bt))
            } else if let Some((_, why)) = left_alone.iter().find(|(alone, _)| *alone == tid) {
                Some(format!("not interrupted: {why}"))
            } else {
                captured
                    .iter()
                    .find(|(captured_tid, _)| *captured_tid == tid)
                    .map(|(_, bt)| frames_of(bt))
            };
            report(
                tid,
                backtrace.unwrap_or_else(|| {
                    "no backtrace: the thread did not answer the capture signal, so it was \
                     inside the allocator or loader, or had exited"
                        .into()
                }),
            )
        })
        .collect()
}

/// The frames of a captured backtrace below the signal handler: the capture's
/// own frames and the kernel's signal trampoline say nothing about the thread.
fn frames_of(backtrace: &Backtrace) -> String {
    let text = backtrace.to_string();
    let lines: Vec<&str> = text.lines().collect();
    let skip = lines
        .iter()
        .rposition(|line| {
            line.contains("__restore_rt")
                || line.contains("sigreturn")
                || line.contains(": <unknown>")
        })
        .map_or(0, |index| index.saturating_add(1));
    // A frame is `  N: symbol` possibly followed by an `at file:line` line;
    // start at the first whole frame after the trampoline.
    let start = lines
        .iter()
        .enumerate()
        .skip(skip)
        .find(|(_, line)| {
            line.trim_start()
                .split_once(':')
                .is_some_and(|(number, _)| number.trim().parse::<u32>().is_ok())
        })
        .map_or_else(|| skip.min(lines.len()), |(index, _)| index);
    lines.get(start..).unwrap_or_default().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stalled(cpu: u64, starved: u64) -> Answer {
        Answer::Stalled {
            budget_ms: 500,
            ticks: 1,
            main_thread_cpu_ms: cpu,
            main_thread_starved_ms: starved,
            threads: Vec::new(),
        }
    }

    #[test]
    fn a_loop_that_ran_or_waited_to_run_is_slow_and_one_that_did_neither_is_blocked() {
        assert!(stalled(400, 0).stalled_but_working(), "computing");
        assert!(stalled(0, 400).stalled_but_working(), "starved of a core");
        assert!(
            !stalled(0, 0).stalled_but_working(),
            "asleep and waiting for nothing: a deadlock or a missed wake"
        );
        assert!(
            !stalled(10, 20).stalled_but_working(),
            "a sliver of either does not make it working"
        );
        assert!(render(&stalled(0, 400)).contains("starved"));
        assert!(render(&stalled(0, 0)).contains("blocked"));
    }

    #[test]
    fn only_a_thread_blocked_in_a_system_call_outside_the_allocator_is_interrupted() {
        // `/proc/<tid>/syscall`: the number and arguments of the call a
        // thread is blocked in, `-1` when blocked outside one, or `running`.
        let futex = format!(
            "{} 0x7f00 0x80 0x0 0x0 0x0 0x0 0x7ffd 0x7f12",
            libc::SYS_futex
        );
        assert_eq!(
            not_interruptible(&futex),
            None,
            "waiting on a lock or a condvar"
        );
        let poll = format!(
            "{} 0x5 0x2 0xffffffff 0x0 0x0 0x0 0x7ffd 0x7f12",
            libc::SYS_ppoll
        );
        assert_eq!(
            not_interruptible(&poll),
            None,
            "asleep in the event loop's poll"
        );
        for call in [
            libc::SYS_mmap,
            libc::SYS_munmap,
            libc::SYS_madvise,
            libc::SYS_brk,
        ] {
            let line = format!("{call} 0x0 0x21000 0x3 0x22 0xffffffff 0x0 0x7ffd 0x7f12");
            assert!(
                not_interruptible(&line).is_some(),
                "the allocator makes system call {call} with its arena locked"
            );
        }
        assert!(
            not_interruptible("-1 0x7ffd 0x7f12").is_some(),
            "a page fault"
        );
        assert!(
            not_interruptible("running").is_some(),
            "on a core, perhaps allocating"
        );
        assert!(not_interruptible("").is_some(), "unreadable");
        // An undumpable process cannot read its threads' system calls; the
        // wait channel stands in.
        for wait in [
            "futex_do_wait",
            "futex_wait_queue",
            "do_epoll_wait",
            "do_nanosleep",
        ] {
            assert_eq!(not_interruptible_waiting_in(wait), None, "{wait}");
        }
        for wait in [
            "",
            "0",
            "rwsem_down_write_slowpath",
            "folio_wait_bit_common",
        ] {
            assert!(not_interruptible_waiting_in(wait).is_some(), "{wait:?}");
        }
    }

    #[test]
    fn a_running_thread_is_reported_without_being_interrupted() {
        use std::sync::atomic::AtomicBool;
        install_capture_handler();
        let stop = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let (spinning, spun) = (Arc::clone(&stop), Arc::clone(&started));
        let worker = std::thread::Builder::new()
            .name("liveness-spin".into())
            .spawn(move || {
                // On a core and never in a system call: the state a busy
                // loop is in, allocator included, when the probe asks.
                spun.store(true, Ordering::Release);
                while !spinning.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
            .unwrap();
        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let threads = dump_threads();
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        let listed = threads
            .iter()
            .find(|thread| thread.name == "liveness-spin")
            .expect("the spinning thread is listed");
        assert!(
            listed.backtrace.starts_with("not interrupted:"),
            "a thread on a core must not be interrupted: {listed:#?}"
        );
    }

    #[test]
    fn the_liveness_socket_sits_beside_the_control_socket() {
        assert_eq!(
            socket_path(Path::new("/run/user/1000/tilo-session/test-control.sock")),
            PathBuf::from("/run/user/1000/tilo-session/test-control.sock.live")
        );
    }

    #[test]
    fn a_dump_names_the_calling_thread_and_its_frames() {
        install_capture_handler();
        let threads = dump_threads();
        let me = std::thread::current();
        assert!(
            threads
                .iter()
                .any(|thread| thread.backtrace.contains("dump_threads")),
            "no thread's backtrace shows the dump: {threads:#?} (running {me:?})"
        );
    }

    #[test]
    fn another_threads_stack_is_captured_through_the_signal() {
        install_capture_handler();
        let (parked, release) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready) = std::sync::mpsc::channel::<i32>();
        let worker = std::thread::Builder::new()
            .name("liveness-probe".into())
            .spawn(move || {
                ready_tx.send(gettid()).unwrap();
                let _ = release.recv();
            })
            .unwrap();
        let tid = ready.recv().unwrap();
        // Only a thread blocked in a system call is interrupted: wait until
        // the worker is asleep in its `recv`, not still on its way there.
        let asleep = Instant::now();
        while not_interruptible(
            &std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall")).unwrap_or_default(),
        )
        .is_some()
        {
            assert!(
                asleep.elapsed() < Duration::from_secs(10),
                "the worker never blocked in its recv"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let threads = dump_threads();
        drop(parked);
        worker.join().unwrap();
        let probe = threads
            .iter()
            .find(|thread| thread.name == "liveness-probe")
            .expect("the parked thread is listed");
        assert!(
            probe.backtrace.contains("recv"),
            "the parked thread's stack should show where it waits:\n{}",
            probe.backtrace
        );
    }
}
