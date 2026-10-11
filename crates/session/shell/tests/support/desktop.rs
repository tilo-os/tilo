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

/// How long the main loop may keep *working* without returning to its loop.
/// Over [`HEARTBEAT_BUDGET`] but under this it is slow, which is reported
/// ([`Desktop::jank`]); over this it is spinning, which fails. A loop that is
/// *blocked* (waiting on a lock, a descriptor, a child) fails at the budget:
/// work finishes, a deadlock does not.
pub const BUSY_LIMIT: Duration = Duration::from_secs(5);

/// How often the monitor asks for a heartbeat.
const POLL: Duration = Duration::from_millis(50);

/// The longest an ordinary control call may take. Answers arrive in
/// milliseconds; this only decides when a call that never answered is reported.
pub const CALL_DEADLINE: Duration = Duration::from_secs(10);

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
}

/// A started compositor.
pub struct Desktop {
    child: Mutex<Child>,
    runtime_dir: PathBuf,
    control: PathBuf,
    /// The public Wayland socket clients connect to.
    wayland_socket: PathBuf,
    size: (i32, i32),
    log: Arc<Mutex<Vec<String>>>,
    watch: Arc<Watch>,
    monitor: Option<std::thread::JoinHandle<()>>,
    last_call: Mutex<String>,
}

/// A Wayland client process and what it has written to stdout.
pub struct Client {
    child: Child,
    output: Arc<Mutex<String>>,
}

impl Client {
    /// Everything the client has reported so far.
    pub fn output(&self) -> String {
        self.output
            .lock()
            .map(|out| out.clone())
            .unwrap_or_default()
    }

    /// How many times the client reported `event`.
    pub fn count(&self, event: &str) -> usize {
        self.output().matches(event).count()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Desktop {
    /// Boot a headless compositor over `tree` and wait until it serves.
    pub fn start(size: (i32, i32), tree: &shell::shell::state::WorkspaceTree) -> Self {
        tilo_test_env::require_isolated_desktop();
        // SUN_LEN caps a unix socket path near 108 bytes, so the runtime dir is short.
        let runtime_dir = PathBuf::from(format!(
            "/run/user/{}/tilo-smoke-{}-{}",
            // SAFETY: `getuid` takes no arguments and cannot fail.
            unsafe { libc::getuid() },
            std::process::id(),
            next_id(),
        ));
        std::fs::create_dir_all(&runtime_dir).expect("create the private runtime dir");
        let state_path = runtime_dir.join("state.json");
        shell::shell::persist::save_tree_to(tree, &state_path).expect("seed the desktop");
        seed_tab_records(&runtime_dir, tree);
        let identity_root = seed_identity(&runtime_dir);
        let control = runtime_dir.join("ctl");

        let mut child = Command::new(env!("CARGO_BIN_EXE_shell"))
            .args(["--mode", "native", "--backend", "headless"])
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
            .env("RUST_LOG", "warn")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
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
            size,
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

    /// The headless screen, `(width, height)`.
    pub const fn size(&self) -> (i32, i32) {
        self.size
    }

    /// The wayland socket's file name, as `WAYLAND_DISPLAY` takes it.
    pub fn display_name(&self) -> String {
        self.wayland_socket
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .expect("the public socket has a name")
    }

    /// Fail now if the main loop has stalled, with the report.
    #[track_caller]
    pub fn check(&self) {
        let stall = self.watch.stall.lock().ok().and_then(|stall| stall.clone());
        if let Some(report) = stall {
            let last = self.last_call.lock().map(|c| c.clone()).unwrap_or_default();
            panic!(
                "DESKTOP FROZE: the compositor's main loop stalled.\n\
                 It did not acknowledge a wake within {HEARTBEAT_BUDGET:?} (or, working, within {BUSY_LIMIT:?}). Last control call: {last}\n\
                 {report}\n--- compositor log tail ---\n{}",
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
        self.call_within(request, CALL_DEADLINE)
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
        let threads = self
            .liveness_threads()
            .unwrap_or_else(|e| format!("(no thread dump: {e})"));
        panic!(
            "control call {op} did not answer within {deadline:?} although the main loop \
             kept acknowledging wakes (slowest {:?}): the call is slow, not the loop frozen.\n\
             Request: {request}\n{threads}\n--- compositor log tail ---\n{}",
            Duration::from_micros(self.watch.worst_latency_us.load(Ordering::Relaxed)),
            self.recent_log(40)
        );
    }

    fn liveness_threads(&self) -> Result<String, String> {
        let path = shell::compositor::liveness::socket_path(&self.control);
        let mut stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
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
    pub fn wait_for(&self, what: &str, mut condition: impl FnMut() -> bool) {
        let started = Instant::now();
        while !condition() {
            self.check();
            assert!(
                started.elapsed() < tilo_test_env::hang_bound(),
                "gave up waiting for {what} after {:?}\n{}",
                started.elapsed(),
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

    /// Type `text`.
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
            Duration::from_secs(15),
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
            .stderr(Stdio::null())
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
        Client { child, output }
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
            let budget = u64::try_from(HEARTBEAT_BUDGET.as_millis()).unwrap_or(500);
            // The compositor answers within the budget, then spends up to two
            // seconds collecting stacks; allow for both.
            let _ = stream.set_read_timeout(Some(
                HEARTBEAT_BUDGET.saturating_add(Duration::from_secs(10)),
            ));
            let Ok(mut writer) = stream.try_clone() else {
                return;
            };
            let mut reader = BufReader::new(stream);
            while !watch.stop.load(Ordering::Relaxed) {
                let request = format!("{{\"op\":\"heartbeat\",\"budget_ms\":{budget}}}\n");
                if writer.write_all(request.as_bytes()).is_err() {
                    return;
                }
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    // The compositor went away; `check` reports its exit.
                    return;
                }
                let mut answer = serde_json::from_str::<Answer>(&line);
                // A stall whose main thread was still computing is slow work:
                // note it with its stack, then hold the loop to the busy limit.
                if let Ok(Answer::Stalled {
                    budget_ms,
                    main_thread_cpu_ms,
                    threads,
                    ..
                }) = &answer
                    && *main_thread_cpu_ms >= budget_ms / 4
                {
                    let main = threads.first().map(|t| t.backtrace.as_str()).unwrap_or("");
                    let top: Vec<&str> = main.lines().filter(|l| !l.contains(" at ")).take(14).collect();
                    if let Ok(mut jank) = watch.jank.lock() {
                        jank.push(format!(
                            "main loop busy for over {budget_ms} ms ({main_thread_cpu_ms} ms CPU):\n{}",
                            top.join("\n")
                        ));
                    }
                    let limit = u64::try_from(BUSY_LIMIT.as_millis()).unwrap_or(5000);
                    let request = format!("{{\"op\":\"heartbeat\",\"budget_ms\":{limit}}}\n");
                    if writer.write_all(request.as_bytes()).is_err() {
                        return;
                    }
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    answer = serde_json::from_str::<Answer>(&line);
                }
                match answer {
                    Ok(Answer::Alive { latency_ms, .. }) => {
                        watch.beats.fetch_add(1, Ordering::Relaxed);
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
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
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
        match rx.recv_timeout(remaining) {
            Ok((key, value)) => {
                found.insert(key, value);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "the compositor did not start: announced only {found:?} after {:?}\n{}",
                    started.elapsed(),
                    log.lock().map(|l| l.join("\n")).unwrap_or_default()
                );
            }
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

/// CPU and memory pressure from inside the test process, for as long as it
/// lives: busy threads that compete with the compositor for cores, and a
/// resident working set that is rewritten continuously so the kernel keeps
/// faulting and reclaiming pages. Timing bugs that need a slow moment (a launch
/// that races its window, a wait with no wake) show up far more often under it.
pub struct Pressure {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Pressure {
    /// `busy` spinning threads and a `bytes`-sized rewritten working set.
    pub fn start(busy: usize, bytes: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        for n in 0..busy {
            let stop = Arc::clone(&stop);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("pressure-cpu-{n}"))
                    .spawn(move || {
                        let mut x = 0x9E37_79B9_7F4A_7C15_u64 ^ u64::try_from(n).unwrap_or(0);
                        while !stop.load(Ordering::Relaxed) {
                            for _ in 0..50_000 {
                                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                            }
                            std::hint::black_box(x);
                        }
                    })
                    .expect("start a busy thread"),
            );
        }
        if bytes > 0 {
            let stop = Arc::clone(&stop);
            threads.push(
                std::thread::Builder::new()
                    .name("pressure-memory".into())
                    .spawn(move || {
                        const CHUNK: usize = 32 << 20;
                        let mut chunks: Vec<Vec<u8>> = (0..bytes.div_ceil(CHUNK))
                            .map(|_| vec![1_u8; CHUNK])
                            .collect();
                        let mut turn = 0_usize;
                        while !stop.load(Ordering::Relaxed) {
                            let index = turn.checked_rem(chunks.len()).unwrap_or(0);
                            // Rewrite one chunk through every page, then give
                            // it back and fault a fresh one in.
                            for page in chunks[index].chunks_mut(4096) {
                                page[0] = page[0].wrapping_add(1);
                            }
                            chunks[index] = vec![u8::try_from(turn % 251).unwrap_or(0); CHUNK];
                            turn = turn.wrapping_add(1);
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    })
                    .expect("start the memory thread"),
            );
        }
        Self { stop, threads }
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
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
