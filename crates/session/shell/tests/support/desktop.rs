//! A real headless compositor, driven over its control socket, with its
//! liveness as an assertion.
//!
//! Two things make this different from the other compositor fixtures:
//!
//! * A monitor thread asks the compositor, on its liveness socket
//!   (`shell::compositor::liveness`), to acknowledge a main-loop wake every
//!   [`POLL`]. The socket is answered off the main loop, so it still answers
//!   when the loop is deadlocked. A wake not acknowledged within
//!   [`HEARTBEAT_BUDGET`] is a stall: the next thing the test does, or the
//!   call it is waiting in, fails at once with every thread's backtrace.
//! * Every control call has a hard deadline, and a call that runs out of it
//!   says whether the loop was stalled (a hang) or still turning (slow).
//!
//! A test therefore never ends in a bare "timed out" for a compositor that
//! froze.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use getrandom::{SysRng, rand_core::UnwrapErr};
use serde_json::{Value, json};
use shell::compositor::liveness::{Answer, render};
use threshold_identity::ThresholdIdentity;
use tilo_identity_ceremony::IdentityRecord;

/// How long the compositor's main loop may go without acknowledging a wake
/// before the desktop counts as frozen. A user sees a freeze at a fraction of
/// this.
pub const HEARTBEAT_BUDGET: Duration = Duration::from_millis(500);

/// How long the main loop may keep *working* without returning to its loop:
/// the shared hang bound. Over [`HEARTBEAT_BUDGET`] it is slow, which is
/// reported ([`Desktop::jank`]) however loaded the machine is; only a loop
/// that never comes back is spinning, which fails. A loop that is *blocked*
/// (waiting on a lock, a descriptor, a child) fails at the budget: work
/// finishes, a deadlock does not.
fn busy_limit() -> Duration {
    tilo_test_env::hang_bound()
}

/// How often the monitor asks for a heartbeat.
const POLL: Duration = Duration::from_millis(50);

/// The longest an ordinary control call may take: the shared hang bound.
/// Answers arrive in milliseconds; this only decides when a call that never
/// answered is reported.
fn call_deadline() -> Duration {
    tilo_test_env::hang_bound()
}

/// How long the compositor may take to deliver the report of a stall: every
/// thread's state and symbolized stack. It names the stall within
/// [`HEARTBEAT_BUDGET`] but writes its answer only once the stacks are
/// symbolized, and symbolizing a debug build's few dozen threads under the
/// pressure variant's memory load has taken longer than ten seconds. A
/// monitor that gave up sooner went quiet in the middle of a freeze, and the
/// stalled call it left behind was reported as slow work on a loop that kept
/// turning. Waiting here costs only a frozen run's time to fail.
const STALL_REPORT_DEADLINE: Duration = Duration::from_secs(120);

/// The compositor's identity seed: the same completed ceremony the other
/// compositor fixtures start from, so the desktop opens instead of onboarding.
const FIXTURE_RECORD_SCOPE: &str = "00000000-0000-0000-0000-000000000000";

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What the monitor saw.
#[derive(Default)]
struct Watch {
    /// The first stall, rendered: set once, never cleared.
    stall: Mutex<Option<String>>,
    /// The slowest heartbeat acknowledged, in microseconds.
    worst_latency_us: AtomicU64,
    /// Heartbeats acknowledged.
    beats: AtomicU64,
    /// Iterations that ran past the budget while working (not blocked), with
    /// the main thread's stack: slow, reported, not a hang.
    jank: Mutex<Vec<String>>,
    stop: AtomicBool,
    /// Ask for no heartbeats while set ([`Desktop::without_heartbeats`]).
    hold: AtomicBool,
    /// When the heartbeat the monitor is waiting on was asked for; `None`
    /// between heartbeats. One outstanding past the budget is a stall whose
    /// report is still being written.
    outstanding: Mutex<Option<Instant>>,
}

impl Watch {
    /// Whether a heartbeat has gone unacknowledged for longer than the
    /// compositor takes to say it stalled.
    fn overdue(&self) -> bool {
        self.outstanding
            .lock()
            .ok()
            .and_then(|asked| *asked)
            .is_some_and(|asked| asked.elapsed() > HEARTBEAT_BUDGET.saturating_mul(2))
    }
}

/// A started compositor.
pub struct Desktop {
    child: Mutex<Child>,
    runtime_dir: PathBuf,
    control: PathBuf,
    /// The public Wayland socket clients connect to.
    wayland_socket: PathBuf,
    log: Arc<Mutex<Vec<String>>>,
    watch: Arc<Watch>,
    monitor: Option<std::thread::JoinHandle<()>>,
    last_call: Mutex<String>,
}

/// What a compositor left alone did over a window ([`Desktop::idle`]).
pub struct Idle {
    /// The share of one core the whole process used, threads that exited
    /// in the window included.
    pub share: f64,
    /// The same window by thread name: CPU, wakeups and preemptions.
    pub threads: tilo_test_env::idle::Wakeups,
    /// What the compositor printed in the window (with `TILO_PAINT_STATS`,
    /// what woke its loop and what it painted).
    pub log: Vec<String>,
}

/// A Wayland client process and what it has written to stdout.
pub struct Client {
    child: Mutex<Child>,
    output: Arc<Mutex<String>>,
    errors: Arc<Mutex<String>>,
}

impl Client {
    /// Everything the client has reported so far.
    pub fn output(&self) -> String {
        self.output
            .lock()
            .map(|out| out.clone())
            .unwrap_or_default()
    }

    /// What became of the client process: still running, or how it ended, with
    /// whatever it wrote to stderr. A client that vanished (crashed, was
    /// killed, or left on a protocol error) reads differently from one that
    /// is alive and silent.
    pub fn fate(&self) -> String {
        let status = match self.child.lock() {
            Ok(mut child) => match child.try_wait() {
                Ok(None) => "still running".to_owned(),
                Ok(Some(status)) => {
                    use std::os::unix::process::ExitStatusExt as _;
                    status.signal().map_or_else(
                        || format!("exited with {status}"),
                        |signal| format!("killed by signal {signal}"),
                    )
                }
                Err(error) => format!("status unreadable: {error}"),
            },
            Err(_) => "status unreadable".to_owned(),
        };
        let errors = self
            .errors
            .lock()
            .map(|out| out.clone())
            .unwrap_or_default();
        format!("the client process is {status}; its stderr:\n{errors}")
    }

    /// How many times the client reported `event`.
    pub fn count(&self, event: &str) -> usize {
        self.output().matches(event).count()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A `HOME` of the compositor's own, created inside its runtime directory.
///
/// `HOME` is not the test's to lend: tests run in one process, and a test
/// that needs the environment itself points `HOME` (and `XDG_*`) somewhere
/// of its own for as long as it runs ([`tilo_test_env::TestHome`]), then
/// deletes that tree. A compositor spawned meanwhile inherits that path and
/// reads it after the tree is gone, so its settings path cannot be resolved
/// and it exits with "cannot safely open session settings" — a failure in a
/// test that never touched settings. Every `XDG_*` directory is already
/// private per compositor; `HOME` has to be too.
pub fn private_home(runtime_dir: &Path) -> PathBuf {
    let home = runtime_dir.join("home");
    std::fs::create_dir_all(&home).expect("create the compositor's private home");
    home
}

impl Desktop {
    /// Boot a headless compositor over `tree` and wait until it serves.
    pub fn start(size: (i32, i32), tree: &shell::shell::state::WorkspaceTree) -> Self {
        Self::start_with_env(size, tree, &[])
    }

    /// [`Self::start`], with `env` added to the compositor's environment.
    pub fn start_with_env(
        size: (i32, i32),
        tree: &shell::shell::state::WorkspaceTree,
        env: &[(&str, &str)],
    ) -> Self {
        tilo_test_env::require_isolated_desktop();
        // SUN_LEN caps a unix socket path near 108 bytes, so the runtime dir is short.
        let runtime_dir = PathBuf::from(format!(
            "/run/user/{}/tilo-smoke-{}-{}",
            rustix::process::getuid().as_raw(),
            std::process::id(),
            next_id(),
        ));
        std::fs::create_dir_all(&runtime_dir).expect("create the private runtime dir");
        let home = private_home(&runtime_dir);
        let state_path = runtime_dir.join("state.json");
        shell::shell::persist::save_tree_to(tree, &state_path).expect("seed the desktop");
        seed_tab_records(&runtime_dir, tree);
        let identity_root = seed_identity(&runtime_dir);
        let control = runtime_dir.join("ctl");

        let mut child = Command::new(tilo_test_env::package_binary!("shell"))
            .args(["--mode", "native", "--backend", "headless"])
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_STATE_HOME", runtime_dir.join("state"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            .env("TILO_PACKAGE_ID", shell::shell::SHELL_RECORDS_PACKAGE)
            .env("TILO_WORKSPACE_ID", FIXTURE_RECORD_SCOPE)
            .env("TILO_WORKSPACE_ROOTS", "")
            .env("TILO_STATE_PATH", &state_path)
            .env("TILO_IDENTITY_ROOT", &identity_root)
            .env("TILO_TEST_CONTROL", &control)
            .env("TILO_LOCK_TEST_PASSWORD", UNLOCK_PASSWORD)
            .env("TILO_HEADLESS_SIZE", format!("{}x{}", size.0, size.1))
            .env("RUST_LOG", "warn,tilo::shortcut=info,tilo::workspace=info")
            // Every two seconds, what woke the main loop and what it painted:
            // the idle check prints the lines from its window.
            .env("TILO_PAINT_STATS", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .envs(env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the compositor under test");

        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let announced = drain_log_pipes(&mut child, &log);
        let found = wait_until_announced(&announced, &mut child, &log);
        let public = found["public"].clone();
        let mut desktop = Self {
            child: Mutex::new(child),
            wayland_socket: runtime_dir.join(public),
            control: PathBuf::from(&found["control"]),
            runtime_dir,
            log,
            watch: Arc::new(Watch::default()),
            monitor: None,
            last_call: Mutex::new("startup".into()),
        };
        desktop.wait_until_serving();
        // From here the loop must keep turning. Start-up is excluded on
        // purpose: it loads fonts and compiles shaders on the loop.
        desktop.monitor = Some(spawn_monitor(
            PathBuf::from(&found["liveness"]),
            Arc::clone(&desktop.watch),
        ));
        desktop
    }

    /// A connection to the compositor's public Wayland socket.
    pub fn connect_wayland(&self) -> UnixStream {
        UnixStream::connect(&self.wayland_socket).expect("connect to the Wayland socket")
    }

    /// Stop the heartbeat monitor. Its pings wake the main loop every 50 ms,
    /// which would hide a loop that sleeps with work pending; a test of exactly
    /// that runs without it.
    pub fn stop_monitor(&mut self) {
        self.watch.stop.store(true, Ordering::Relaxed);
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
    }

    /// The compositor's `XDG_CACHE_HOME`, where its crash records go.
    pub fn cache_home(&self) -> PathBuf {
        self.runtime_dir.join("cache")
    }

    /// The wayland socket's file name, as `WAYLAND_DISPLAY` takes it.
    pub fn display_name(&self) -> String {
        self.wayland_socket
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .expect("the public socket has a name")
    }

    /// Fail now if the main loop has stalled, with the report.
    ///
    /// A heartbeat already overdue is a stall whose report (every thread's
    /// stack) the compositor is still writing: wait for it rather than carry
    /// on as if the loop were turning.
    #[track_caller]
    pub fn check(&self) {
        let asked = Instant::now();
        while self.watch.overdue() && asked.elapsed() < STALL_REPORT_DEADLINE {
            std::thread::sleep(POLL);
        }
        let stall = self.watch.stall.lock().ok().and_then(|stall| stall.clone());
        if let Some(report) = stall {
            let last = self.last_call.lock().map(|c| c.clone()).unwrap_or_default();
            panic!(
                "DESKTOP FROZE: the compositor's main loop stalled.\n\
                 It did not acknowledge a wake within {HEARTBEAT_BUDGET:?} (or, working, within {:?}). Last control call: {last}\n\
                 {report}\n--- compositor log tail ---\n{}",
                busy_limit(),
                self.recent_log(40)
            );
        }
        let exited = self
            .child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten());
        if let Some(status) = exited {
            panic!(
                "the compositor exited ({status}) mid-test.\n--- compositor log tail ---\n{}",
                self.recent_log(60)
            );
        }
    }

    /// One control call, answered or reported as hung. Returns the whole
    /// reply (`{"ok":..}`, `{"err":..}` or a payload object).
    #[track_caller]
    pub fn call(&self, request: &Value) -> Value {
        self.call_within(request, call_deadline())
    }

    /// [`Self::call`] with an explicit deadline for calls that wait on purpose.
    #[track_caller]
    pub fn call_within(&self, request: &Value, deadline: Duration) -> Value {
        self.check();
        let op = request.get("op").and_then(Value::as_str).unwrap_or("?");
        if let Ok(mut last) = self.last_call.lock() {
            *last = request.to_string().chars().take(200).collect();
        }
        let started = Instant::now();
        let mut stream = UnixStream::connect(&self.control)
            .unwrap_or_else(|e| panic!("connect to the control socket for {op}: {e}"));
        stream
            .set_read_timeout(Some(Duration::from_millis(25)))
            .expect("short read slices");
        let mut line = serde_json::to_vec(request).expect("encode the request");
        line.push(b'\n');
        stream.write_all(&line).expect("write the request");
        let mut reply = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => panic!(
                    "the compositor closed the control connection during {op}\n{}",
                    self.recent_log(40)
                ),
                Ok(n) => reply.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => panic!("read the reply to {op}: {e}"),
            }
            if reply.last() == Some(&b'\n') {
                break;
            }
            // A frozen loop is reported by the monitor within its budget:
            // fail as that, with the stacks, rather than waiting this out.
            self.check();
            if started.elapsed() >= deadline {
                self.hung_call(op, request, deadline);
            }
        }
        serde_json::from_slice(&reply)
            .unwrap_or_else(|e| panic!("the reply to {op} was not JSON ({e}): {reply:?}"))
    }

    /// A call that outlived its deadline: give the monitor time to name a
    /// stall, and otherwise report slowness, with the threads either way.
    #[track_caller]
    fn hung_call(&self, op: &str, request: &Value, deadline: Duration) -> ! {
        let grace = Instant::now()
            .checked_add(HEARTBEAT_BUDGET.saturating_mul(8))
            .unwrap_or_else(Instant::now);
        while Instant::now() < grace {
            self.check();
            std::thread::sleep(POLL);
        }
        let threads = self.thread_dump();
        panic!(
            "control call {op} did not answer within {deadline:?} although the main loop \
             kept acknowledging wakes (slowest {:?}): the call is slow, not the loop frozen.\n\
             Request: {request}\n{threads}\n--- compositor log tail ---\n{}",
            Duration::from_micros(self.watch.worst_latency_us.load(Ordering::Relaxed)),
            self.recent_log(40)
        );
    }

    /// Every compositor thread's state and backtrace, or why there is none.
    pub fn thread_dump(&self) -> String {
        self.liveness_threads()
            .unwrap_or_else(|e| format!("(no thread dump: {e})"))
    }

    /// One request on the liveness socket, answered off the main loop.
    fn liveness(&self, request: &str) -> Result<Answer, String> {
        let path = shell::compositor::liveness::socket_path(&self.control);
        let mut stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(STALL_REPORT_DEADLINE))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(format!("{request}\n").as_bytes())
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        serde_json::from_str(&line).map_err(|e| format!("{e}: {line}"))
    }

    /// From now on, record every path the compositor's main thread walks
    /// (`compositor::liveness::main_thread_files`). Startup is over: the
    /// main loop opens, stats and lists nothing by path after it.
    ///
    /// # Panics
    ///
    /// When the audit cannot be armed: a desktop that cannot be watched
    /// proves nothing.
    #[track_caller]
    pub fn forbid_main_thread_files(&self) {
        match self.liveness(r#"{"op":"forbid_main_thread_files"}"#) {
            Ok(Answer::Armed) => {}
            other => panic!("arming the main-thread file audit: {other:?}"),
        }
    }

    /// Every path the main thread walked since
    /// [`Self::forbid_main_thread_files`], with where it was.
    #[track_caller]
    pub fn main_thread_files(&self) -> Vec<shell::compositor::liveness::FileWalk> {
        match self.liveness(r#"{"op":"main_thread_files"}"#) {
            Ok(Answer::MainThreadFiles(walks)) => walks,
            other => panic!("reading the main-thread file audit: {other:?}"),
        }
    }

    fn liveness_threads(&self) -> Result<String, String> {
        let path = shell::compositor::liveness::socket_path(&self.control);
        let mut stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(STALL_REPORT_DEADLINE))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(b"{\"op\":\"threads\"}\n")
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        let answer: Answer = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        Ok(render(&answer))
    }

    /// A call that must answer `{"ok":..}`.
    #[track_caller]
    pub fn ok(&self, request: &Value) -> Value {
        let reply = self.call(request);
        assert!(
            reply.get("err").is_none(),
            "{request} was refused: {reply}\n{}",
            self.recent_log(20)
        );
        reply
    }

    /// Wait for `condition` to hold, polling; a stall or the hang bound fails.
    #[track_caller]
    pub fn wait_for(&self, what: &str, condition: impl FnMut() -> bool) {
        self.wait_for_with(what, String::new, condition);
    }

    /// [`Self::wait_for`], with `context` called to add to the failure message.
    #[track_caller]
    pub fn wait_for_with(
        &self,
        what: &str,
        context: impl Fn() -> String,
        mut condition: impl FnMut() -> bool,
    ) {
        let started = Instant::now();
        while !condition() {
            self.check();
            assert!(
                started.elapsed() < tilo_test_env::hang_bound(),
                "gave up waiting for {what} after {:?}\n{}\n{}",
                started.elapsed(),
                context(),
                self.recent_log(40)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Sleep, failing early if the loop stalls meanwhile.
    #[track_caller]
    pub fn pause(&self, duration: Duration) {
        let until = Instant::now()
            .checked_add(duration)
            .unwrap_or_else(Instant::now);
        while Instant::now() < until {
            self.check();
            std::thread::sleep(POLL.min(duration));
        }
        self.check();
    }

    // Protocol helpers

    /// Move the pointer to `(x, y)`.
    #[track_caller]
    pub fn pointer_move(&self, x: f64, y: f64) {
        self.ok(&json!({"op": "pointer_move", "x": x, "y": y}));
    }

    #[track_caller]
    pub fn type_text(&self, text: &str) {
        self.ok(&json!({"op": "type", "text": text}));
    }

    /// Press and release `key`.
    #[track_caller]
    pub fn key(&self, key: &str) {
        self.ok(&json!({"op": "key", "key": key}));
    }

    /// Press and release `key` with Control held.
    #[track_caller]
    pub fn chord_ctrl(&self, key: &str) {
        self.ok(&json!({"op": "key_down", "key": "Control"}));
        self.key(key);
        self.ok(&json!({"op": "key_up", "key": "Control"}));
    }

    /// Click at `(x, y)`.
    #[track_caller]
    pub fn click_at(&self, x: f64, y: f64) {
        self.ok(&json!({"op": "click_at", "x": x, "y": y}));
    }

    /// The open windows, one row per tab.
    #[track_caller]
    pub fn windows(&self) -> Vec<Value> {
        let reply = self.ok(&json!({"op": "windows"}));
        reply
            .get("answer")
            .and_then(|a| a.get("windows").or(Some(a)))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("windows answered {reply}"))
    }

    /// The named widget rectangles, `(name, [x, y, w, h])`.
    #[track_caller]
    pub fn targets(&self) -> Vec<(String, [f64; 4])> {
        let reply = self.ok(&json!({"op": "list_targets"}));
        reply
            .get("targets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let name = entry.get("name")?.as_str()?.to_string();
                let rect = entry.get("rect")?.as_array()?;
                let mut out = [0.0; 4];
                for (slot, value) in out.iter_mut().zip(rect) {
                    *slot = value.as_f64()?;
                }
                Some((name, out))
            })
            .collect()
    }

    /// The tracked foreign (Wayland) surfaces.
    #[track_caller]
    pub fn foreign_frames(&self) -> Vec<Value> {
        let reply = self.ok(&json!({"op": "foreign_frames"}));
        reply
            .get("foreign_frames")
            .and_then(|f| f.get("frames"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The name of the workspace that is showing, from the live tree.
    #[track_caller]
    pub fn active_workspace(&self) -> String {
        let reply = self.ok(&json!({"op": "state_dump"}));
        let tree = reply
            .get("state")
            .and_then(|s| s.get("workspace_tree"))
            .unwrap_or_else(|| panic!("state_dump had no workspace_tree: {reply}"));
        let active = tree.get("active");
        tree.get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|ws| ws.get("id") == active)
            .and_then(|ws| ws.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// `chrome` of a state dump: what the shell's own UI is doing.
    #[track_caller]
    pub fn chrome(&self) -> Value {
        let reply = self.ok(&json!({"op": "state_dump"}));
        reply
            .get("state")
            .and_then(|s| s.get("chrome"))
            .cloned()
            .unwrap_or_else(|| panic!("state_dump had no chrome: {reply}"))
    }

    /// Invoke the action `(provider, id)`, waiting for it to settle. The
    /// address is resolved against a fresh catalogue, so an unknown one is
    /// refused here, not skipped.
    #[track_caller]
    pub fn invoke_action(&self, provider: &str, id: &str) -> Value {
        let reply = self.call_within(
            &json!({"op": "actions_invoke", "provider": provider, "id": id, "wait_ms": 8000}),
            call_deadline(),
        );
        assert!(
            reply.get("err").is_none(),
            "action {provider}:{id} was refused: {reply}\n{}",
            self.recent_log(20)
        );
        reply
    }

    /// Start a Wayland client of the test suite and return it. The client
    /// connects to this compositor only.
    pub fn spawn_client(&self, binary: &Path) -> Client {
        let mut child = Command::new(binary)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("WAYLAND_DISPLAY", self.display_name())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("start {}: {e}", binary.display()));
        let output = Arc::new(Mutex::new(String::new()));
        let stdout = child.stdout.take().expect("client stdout");
        let sink = Arc::clone(&output);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(mut out) = sink.lock() {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        });
        let errors = Arc::new(Mutex::new(String::new()));
        let stderr = child.stderr.take().expect("client stderr");
        let sink = Arc::clone(&errors);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(mut out) = sink.lock() {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        });
        Client {
            child: Mutex::new(child),
            output,
            errors,
        }
    }

    /// Send `request` on the control socket without waiting for a reply,
    /// for a request whose answer is the compositor's fate, not a line.
    pub fn send(&self, request: &Value) {
        let mut stream = UnixStream::connect(&self.control).expect("connect to the control socket");
        let mut line = serde_json::to_vec(request).expect("encode the request");
        line.push(b'\n');
        stream.write_all(&line).expect("write the request");
        // Held open until the compositor is gone: closing it early would
        // only race the request with a hang-up.
        std::mem::forget(stream);
    }

    /// Wait up to `within` for the compositor to exit on its own; its exit
    /// status, or `None` while it still runs.
    pub fn wait_for_exit(&self, within: Duration) -> Option<std::process::ExitStatus> {
        let started = Instant::now();
        loop {
            let exited = self
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.try_wait().ok().flatten());
            if exited.is_some() || started.elapsed() >= within {
                return exited;
            }
            std::thread::sleep(POLL);
        }
    }

    /// The last `n` lines the compositor wrote.
    pub fn recent_log(&self, n: usize) -> String {
        self.log
            .lock()
            .map(|lines| {
                let start = lines.len().saturating_sub(n);
                lines[start..].join("\n")
            })
            .unwrap_or_default()
    }

    /// The compositor's `/proc/<pid>/NAME` file (`status`, `limits`, ...).
    pub fn proc_file(&self, name: &str) -> String {
        let pid = self.child.lock().map_or(0, |child| child.id());
        std::fs::read_to_string(format!("/proc/{pid}/{name}")).unwrap_or_default()
    }

    /// CPU the compositor process used, in milliseconds, per `/proc`.
    fn compositor_cpu_ms(&self) -> u64 {
        let pid = self.child.lock().map_or(0, |child| child.id());
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let ticks: u64 = stat
            .rsplit_once(')')
            .map(|(_, rest)| rest.split_whitespace().skip(11).take(2))
            .into_iter()
            .flatten()
            .filter_map(|field| field.parse::<u64>().ok())
            .sum();
        // The kernel's clock tick is 100 Hz on every platform the suite runs on.
        ticks.saturating_mul(10)
    }

    /// What the compositor did over `window` of doing nothing.
    #[track_caller]
    pub fn idle(&self, window: Duration) -> Idle {
        let pid = self.child.lock().map_or(0, |child| child.id());
        let threads_before = tilo_test_env::idle::sample_process(pid).unwrap_or_default();
        let log_from = self.log.lock().map_or(0, |lines| lines.len());
        let started = Instant::now();
        let before = self.compositor_cpu_ms();
        self.pause(window);
        let used = self.compositor_cpu_ms().saturating_sub(before);
        let elapsed = started.elapsed();
        let threads_after = tilo_test_env::idle::sample_process(pid).unwrap_or_default();
        Idle {
            share: f64::from(u32::try_from(used).unwrap_or(u32::MAX))
                / (window.as_secs_f64() * 1000.0),
            threads: tilo_test_env::idle::Wakeups::between(threads_before, threads_after, elapsed),
            log: self
                .log
                .lock()
                .map(|lines| lines.get(log_from..).unwrap_or_default().to_vec())
                .unwrap_or_default(),
        }
    }

    /// Run `f` with the heartbeat monitor holding its pings, so only the
    /// desktop itself wakes the main loop. A loop that stalled meanwhile is
    /// reported by the first heartbeat after it.
    pub fn without_heartbeats<T>(&self, f: impl FnOnce() -> T) -> T {
        self.watch.hold.store(true, Ordering::Relaxed);
        let out = f();
        self.watch.hold.store(false, Ordering::Relaxed);
        out
    }

    /// Main-loop iterations that ran long while working, with stacks.
    pub fn jank(&self) -> Vec<String> {
        self.watch
            .jank
            .lock()
            .map(|j| j.clone())
            .unwrap_or_default()
    }

    /// `(heartbeats acknowledged, slowest)` as the monitor saw them.
    pub fn heartbeat_summary(&self) -> (u64, Duration) {
        (
            self.watch.beats.load(Ordering::Relaxed),
            Duration::from_micros(self.watch.worst_latency_us.load(Ordering::Relaxed)),
        )
    }

    fn wait_until_serving(&self) {
        let started = Instant::now();
        loop {
            let laid_out = UnixStream::connect(&self.control).ok().and_then(|mut s| {
                s.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
                s.write_all(b"{\"op\":\"list_targets\"}\n").ok()?;
                let mut line = String::new();
                BufReader::new(s).read_line(&mut line).ok()?;
                let value: Value = serde_json::from_str(&line).ok()?;
                Some(value.get("targets")?.as_array()?.len())
            });
            if laid_out.is_some_and(|n| n > 0) {
                return;
            }
            assert!(
                started.elapsed() < tilo_test_env::hang_bound(),
                "the compositor never laid out a frame\n{}",
                self.recent_log(60)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.watch.stop.store(true, Ordering::Relaxed);
        // For investigations: keep everything the compositor said.
        if let Some(path) = std::env::var_os("TILO_SMOKE_COMPOSITOR_LOG") {
            let mut named = path;
            named.push(format!(
                ".{}",
                self.runtime_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ));
            let _ = std::fs::write(named, self.recent_log(usize::MAX));
        }
        if std::thread::panicking() {
            let _ = writeln!(
                std::io::stderr().lock(),
                "--- compositor log tail (60 lines) ---\n{}",
                self.recent_log(60)
            );
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// The password the compositor accepts to unlock (it stands in for PAM).
pub const UNLOCK_PASSWORD: &str = "correct-horse-battery";

#[allow(
    clippy::too_many_lines,
    reason = "the monitor thread is one heartbeat loop with its stall reporting inline"
)]
fn spawn_monitor(liveness: PathBuf, watch: Arc<Watch>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("heartbeat-monitor".into())
        .spawn(move || {
            let Ok(stream) = UnixStream::connect(&liveness) else {
                if let Ok(mut stall) = watch.stall.lock() {
                    *stall = Some(format!(
                        "the liveness socket {} cannot be reached",
                        liveness.display()
                    ));
                }
                return;
            };
            // The compositor answers within the budget, or names the stall
            // then and answers once every thread's stack is symbolized.
            let _ = stream
                .set_read_timeout(Some(HEARTBEAT_BUDGET.saturating_add(STALL_REPORT_DEADLINE)));
            let Ok(writer) = stream.try_clone() else {
                return;
            };
            let mut liveness = Liveness {
                writer,
                reader: BufReader::new(stream),
                watch: &watch,
            };
            while !watch.stop.load(Ordering::Relaxed) {
                if watch.hold.load(Ordering::Relaxed) {
                    std::thread::sleep(POLL);
                    continue;
                }
                let mut line = String::new();
                if !liveness.heartbeat(&mut line) {
                    return;
                }
                let mut answer = serde_json::from_str::<Answer>(&line);
                // A stall whose main thread was still computing is slow work:
                // note it with its stack, then hold the loop to the busy limit.
                if let Ok(stalled @ Answer::Stalled { threads, .. }) = &answer
                    && stalled.stalled_but_working()
                {
                    let summary = render(stalled);
                    let summary = summary.lines().next().unwrap_or_default();
                    let main = threads.first().map_or("", |t| t.backtrace.as_str());
                    let top: Vec<&str> = main
                        .lines()
                        .filter(|l| !l.contains(" at "))
                        .take(14)
                        .collect();
                    if let Ok(mut jank) = watch.jank.lock() {
                        jank.push(format!("{summary}\n{}", top.join("\n")));
                    }
                    // Keep asking: it still working is fine, its turning blocked
                    // (a lock, I/O) fails at once, and working past the limit is
                    // a spin.
                    let mut waited = HEARTBEAT_BUDGET;
                    while waited < busy_limit() {
                        line.clear();
                        if !liveness.heartbeat(&mut line) {
                            return;
                        }
                        answer = serde_json::from_str::<Answer>(&line);
                        if let Ok(Answer::Stalled { threads, .. }) = &answer
                            && let Ok(mut jank) = watch.jank.lock()
                        {
                            let main = threads.first().map_or("", |t| t.backtrace.as_str());
                            let top: Vec<&str> = main
                                .lines()
                                .filter(|l| !l.contains(" at "))
                                .take(14)
                                .collect();
                            jank.push(format!(
                                "  ... still busy {waited:?} in:\n{}",
                                top.join("\n")
                            ));
                        }
                        let still_working = answer.as_ref().is_ok_and(Answer::stalled_but_working);
                        if !still_working {
                            break;
                        }
                        waited = waited.saturating_add(HEARTBEAT_BUDGET);
                    }
                }
                match answer {
                    Ok(Answer::Alive {
                        latency_ms,
                        halfway,
                        ..
                    }) => {
                        watch.beats.fetch_add(1, Ordering::Relaxed);
                        // Back within the budget, but asleep at its middle:
                        // note where, as slow work is noted.
                        if let Some(thread) = halfway
                            && let Ok(mut jank) = watch.jank.lock()
                        {
                            jank.push(format!(
                                "main loop took {latency_ms:.0} ms to acknowledge, idle at the \
                                 middle of the budget in state {} waiting in {}:\n{}",
                                thread.state, thread.wait, thread.backtrace
                            ));
                        }
                        let micros =
                            u64::try_from(Duration::from_secs_f64(latency_ms / 1000.0).as_micros())
                                .unwrap_or(u64::MAX);
                        watch.worst_latency_us.fetch_max(micros, Ordering::Relaxed);
                    }
                    Ok(stalled @ Answer::Stalled { .. }) => {
                        if let Ok(mut stall) = watch.stall.lock() {
                            *stall = Some(render(&stalled));
                        }
                        return;
                    }
                    Ok(other) => {
                        if let Ok(mut stall) = watch.stall.lock() {
                            *stall = Some(format!("unexpected liveness answer: {other:?}"));
                        }
                        return;
                    }
                    Err(e) => {
                        if let Ok(mut stall) = watch.stall.lock() {
                            *stall = Some(format!("unreadable liveness answer ({e}): {line}"));
                        }
                        return;
                    }
                }
                std::thread::sleep(POLL);
            }
        })
        .expect("start the heartbeat monitor")
}

/// The monitor's connection to the liveness socket.
struct Liveness<'a> {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    watch: &'a Watch,
}

impl Liveness<'_> {
    /// Ask for one heartbeat and read its answer into `line`. `false` ends
    /// the monitor: the compositor went away (`check` reports its exit), or
    /// it neither acknowledged the wake nor delivered its report of the
    /// stall in time, which is recorded as the stall it is.
    fn heartbeat(&mut self, line: &mut String) -> bool {
        let budget = u64::try_from(HEARTBEAT_BUDGET.as_millis()).unwrap_or(500);
        let request = format!("{{\"op\":\"heartbeat\",\"budget_ms\":{budget}}}\n");
        if let Ok(mut outstanding) = self.watch.outstanding.lock() {
            *outstanding = Some(Instant::now());
        }
        let read = self
            .writer
            .write_all(request.as_bytes())
            .and_then(|()| self.reader.read_line(line));
        let answered = match read {
            Ok(0) => false,
            Ok(_) => true,
            // The compositor closed the socket: it went away.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                false
            }
            Err(error) => {
                if let Ok(mut stall) = self.watch.stall.lock() {
                    *stall = Some(format!(
                        "the main loop did not acknowledge a wake within {HEARTBEAT_BUDGET:?}, \
                         and the compositor's report of the stall, with every thread's stack, \
                         did not arrive within {STALL_REPORT_DEADLINE:?} ({error})"
                    ));
                }
                false
            }
        };
        if let Ok(mut outstanding) = self.watch.outstanding.lock() {
            *outstanding = None;
        }
        answered
    }
}

/// Drain the compositor's stdout and stderr on threads (unread output blocks
/// it mid-write) and report the sockets it announces.
fn drain_log_pipes(
    child: &mut Child,
    log: &Arc<Mutex<Vec<String>>>,
) -> mpsc::Receiver<(String, String)> {
    let stdout = child.stdout.take().expect("compositor stdout");
    let stdout_log = Arc::clone(log);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(mut l) = stdout_log.lock() {
                l.push(line);
            }
        }
    });
    let (tx, rx) = mpsc::channel::<(String, String)>();
    let stderr = child.stderr.take().expect("compositor stderr");
    let log_writer = Arc::clone(log);
    std::thread::spawn(move || {
        // Bytes, not `lines()`: one line that is not UTF-8 would end the
        // read and silently drop everything the compositor says after it.
        let mut reader = BufReader::new(stderr);
        let mut raw = Vec::new();
        while reader
            .read_until(b'\n', &mut raw)
            .is_ok_and(|read| read > 0)
        {
            let line = String::from_utf8_lossy(&raw)
                .trim_end_matches(['\n', '\r'])
                .to_string();
            raw.clear();
            for (marker, key) in [
                ("tilo compositor listening on ", "public"),
                ("tilo compositor test_control listening on ", "control"),
                ("tilo compositor liveness listening on ", "liveness"),
            ] {
                if let Some(rest) = line.split_once(marker).map(|(_, r)| r) {
                    let value = rest.trim().trim_matches('"').to_string();
                    let _ = tx.send((key.to_string(), value));
                }
            }
            if let Ok(mut l) = log_writer.lock() {
                l.push(line);
            }
        }
    });
    rx
}

fn wait_until_announced(
    rx: &mpsc::Receiver<(String, String)>,
    child: &mut Child,
    log: &Arc<Mutex<Vec<String>>>,
) -> HashMap<String, String> {
    let started = Instant::now();
    let mut found = HashMap::new();
    while found.len() < 3 {
        let remaining = tilo_test_env::hang_bound().saturating_sub(started.elapsed());
        if let Ok((key, value)) = rx.recv_timeout(remaining) {
            found.insert(key, value);
        } else {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the compositor did not start: announced only {found:?} after {:?}\n{}",
                started.elapsed(),
                log.lock().map(|l| l.join("\n")).unwrap_or_default()
            );
        }
    }
    found
}

fn seed_tab_records(runtime_dir: &Path, tree: &shell::shell::state::WorkspaceTree) {
    let mut owners = ui::record::RecordOwners::new(runtime_dir.join("data"));
    let records = owners
        .open(shell::shell::SHELL_RECORDS_PACKAGE, FIXTURE_RECORD_SCOPE)
        .expect("open the fixture's Shell record namespace");
    for tab in tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
    {
        shell::shell::tab_documents::create_scratch_tab_record(&records, tab.id)
            .expect("create the fixture tab's document");
        records
            .try_open_record(&shell::shell::tab_documents::scratch_tab_record(tab.id))
            .expect("the fixture tab must admit its existing document");
    }
    records
        .try_flush()
        .expect("persist the fixture tab documents");
    drop(records);
    drop(owners);
}

fn seed_identity(runtime_dir: &Path) -> PathBuf {
    let identity_dir = runtime_dir.join("config/tilo/identity");
    let identity = ThresholdIdentity::bootstrap_single_device(&mut UnwrapErr(SysRng))
        .expect("create the fixture identity");
    let record = IdentityRecord::from_threshold_identity(
        &identity,
        "Desktop smoke",
        [128, 128, 128],
        SystemTime::UNIX_EPOCH,
    )
    .expect("encode the fixture identity");
    let bytes = record
        .to_json_bytes()
        .expect("serialize the fixture identity");
    let provider = runtime_identity::BrokerFileProvider::new(&identity_dir);
    runtime_identity::import_identity_with_provider(&identity_dir, "primary", &bytes, &provider)
        .expect("persist the fixture identity");
    runtime_dir.join("identity-root")
}

/// CPU and memory pressure from a process of its own (`pressure_fixture`), for
/// as long as this lives: busy threads that compete with the compositor for
/// cores, and a resident working set rewritten continuously so the kernel keeps
/// faulting and reclaiming pages. It runs at the lowest priority, as a build
/// does beside a desktop session, and is a separate process so it does not
/// share the test's own scheduling. Timing bugs that need a slow moment (a
/// launch that races its window, a wait with no wake) show up far more often
/// under it.
pub struct Pressure {
    child: Child,
}

impl Pressure {
    /// `busy` spinning threads and a `bytes`-sized rewritten working set.
    pub fn start(busy: usize, bytes: usize) -> Self {
        let child = Command::new(tilo_test_env::package_binary!("pressure_fixture"))
            .arg(busy.to_string())
            .arg(bytes.to_string())
            .stdin(Stdio::null())
            .spawn()
            .expect("start the pressure process");
        Self { child }
    }

    /// Pressure sized to this machine: a busy thread per core and a quarter of
    /// the available memory, at most 3 GiB.
    pub fn for_this_machine() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        let available_kib = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|meminfo| {
                meminfo
                    .lines()
                    .find_map(|line| line.strip_prefix("MemAvailable:"))
                    .and_then(|rest| rest.split_whitespace().next()?.parse::<usize>().ok())
            })
            .unwrap_or(4 << 20);
        let bytes = (available_kib / 4).min(3 << 20).saturating_mul(1024);
        Self::start(cores, bytes)
    }
}

impl Drop for Pressure {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
