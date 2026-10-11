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
//!   answer is `{"stalled":{...}}`: the budget, the ticks so far, and every
//!   thread's state, kernel wait channel and backtrace, which is where a
//!   deadlock shows who waits on what. The same report goes to stderr.
//! * `{"op":"threads"}` answers `{"threads":[...]}` whatever the loop is doing.
//!
//! A heartbeat costs the loop one wake when somebody asks. Nothing runs, and
//! the idle loop is not woken, while no client is connected.
//!
//! Like the control socket this exists only where
//! [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`] holds, and is bound only
//! when the control socket is.
//!
//! # Backtraces of other threads
//!
//! A thread cannot be unwound from outside. Each one is sent a real-time
//! signal whose handler captures its own backtrace; the answering thread
//! symbolizes them. Capturing allocates, which a signal handler may not do in
//! general: a thread interrupted inside the allocator would wait on itself.
//! That cannot be avoided without a stack unwinder that works on a stopped
//! process, and the dump runs only on a loop that has already stopped, so a
//! thread that does not answer within [`DUMP_WAIT`] is reported as such rather
//! than waited for.

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
        threads: Vec<ThreadReport>,
    },
    Threads(Vec<ThreadReport>),
    Err(String),
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
                let cpu_before = main_thread_cpu_ms();
                match beat.heartbeat(ping, budget) {
                    Some(latency) => Answer::Alive {
                        ticks: beat.ticks(),
                        latency_ms: latency.as_secs_f64() * 1000.0,
                    },
                    None => {
                        let threads = dump_threads();
                        let answer = Answer::Stalled {
                            budget_ms: u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
                            ticks: beat.ticks(),
                            main_thread_cpu_ms: main_thread_cpu_ms().saturating_sub(cpu_before),
                            threads,
                        };
                        eprintln!("tilo compositor: main loop stalled\n{}", render(&answer));
                        answer
                    }
                }
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

/// A stall or thread listing as the text a person reads.
#[must_use]
pub fn render(answer: &Answer) -> String {
    let (head, threads) = match answer {
        Answer::Stalled {
            budget_ms,
            ticks,
            main_thread_cpu_ms,
            threads,
        } => (
            format!(
                "main loop did not acknowledge a wake within {budget_ms} ms (acknowledged {ticks} \
                 before); the main thread used {main_thread_cpu_ms} ms of CPU meanwhile: {}",
                if *main_thread_cpu_ms < budget_ms / 4 {
                    "it is blocked"
                } else {
                    "it is busy"
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

fn gettid() -> i32 {
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

fn install_capture_handler() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: a zeroed `sigaction` is valid; the handler is an
        // `extern "C" fn(c_int)` and stays valid for the process's life.
        let status = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = capture_backtrace as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(capture_signal(), &action, std::ptr::null_mut())
        };
        if status != 0 {
            eprintln!(
                "tilo compositor: liveness backtrace handler: {}",
                std::io::Error::last_os_error()
            );
        }
    });
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

/// Every thread of this process: its scheduler state, what it waits in and its
/// backtrace.
fn dump_threads() -> Vec<ThreadReport> {
    // One dump at a time: they share the capture slot.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock();

    // SAFETY: `getpid` takes no arguments and cannot fail.
    let pid = unsafe { libc::getpid() };
    let me = gettid();
    let mut tids: Vec<i32> = std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect();
    tids.sort_unstable();

    {
        let mut captured = CAPTURED.lock();
        captured.clear();
        captured.reserve(tids.len().saturating_add(8));
    }
    let mut signalled = 0usize;
    for &tid in tids.iter().filter(|&&tid| tid != me) {
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
            let task = |file: &str| {
                std::fs::read_to_string(format!("/proc/self/task/{tid}/{file}")).unwrap_or_default()
            };
            let state = task("stat")
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_string))
                .unwrap_or_else(|| "?".into());
            let backtrace = if tid == me {
                own.take().map(|bt| frames_of(&bt))
            } else {
                captured
                    .iter()
                    .find(|(captured_tid, _)| *captured_tid == tid)
                    .map(|(_, bt)| frames_of(bt))
            };
            ThreadReport {
                tid,
                name: task("comm").trim().to_string(),
                state,
                wait: task("wchan").trim().to_string(),
                backtrace: backtrace.unwrap_or_else(|| {
                    "no backtrace: the thread did not answer the capture signal, so it was \
                     inside the allocator or loader, or had exited"
                        .into()
                }),
            }
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
        .rposition(|line| line.contains("__restore_rt") || line.contains("sigreturn"))
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
        .map_or(skip.min(lines.len()), |(index, _)| index);
    lines.get(start..).unwrap_or_default().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (ready_tx, ready) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("liveness-probe".into())
            .spawn(move || {
                ready_tx.send(()).unwrap();
                let _ = release.recv();
            })
            .unwrap();
        ready.recv().unwrap();
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
