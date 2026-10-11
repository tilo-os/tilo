//! How often an idle desktop wakes the machine (TIL-614).
//!
//! A desktop nobody is touching should leave the CPU asleep. Every timer, poll
//! or self-rescheduling worker in the compositor, or in anything it started,
//! wakes a core, and on a laptop that is battery: each wakeup costs the chance
//! of a deep sleep state whether or not the work it finds is cheap. None of it
//! shows on screen, so it ships unless something counts it.
//!
//! This counts it. The production compositor binary runs headless with a
//! signed-in person's desktop: a Notes notebook restored from the saved
//! session, and a Terminal opened through the desktop's own action
//! (`window.new.terminal`), both framework apps the compositor hosts. Once both
//! are drawn it is left alone for [`SETTLE`], then every thread of the
//! compositor's process tree is sampled over [`window`] from `/proc`
//! (`tilo_test_env::idle`). The gate is how often the whole tree's threads
//! blocked per second (voluntary context switches, one per wakeup), and its
//! CPU time; the table also shows preemptions. The table names the process
//! and thread of every wakeup, and the compositor's own `paint-tick` lines
//! (`TILO_PAINT_STATS=ticks`) say what woke its frame loop and whether it
//! painted, one line per iteration inside the window.
//!
//! What it does not see: DRM-only services (power, display hotplug, the
//! rotation sensor), which a headless session does not start, and
//! out-of-process apps, which need an installed store. Those are held by
//! thread probes in their own crates.
//!
//! Lowering the budget: [`IDLE_WAKEUP_BUDGET`] is today's measured number
//! plus a margin. Every fix that removes a waker lowers it in the same pull
//! request, to the new measurement plus the same margin, so the next
//! regression cannot hide in the slack a fix left behind.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use getrandom::{SysRng, rand_core::UnwrapErr};
use threshold_identity::ThresholdIdentity;
use tilo_identity_ceremony::IdentityRecord;

/// Most times per second the idle desktop's threads may wake, across every
/// thread of the compositor and its children. The product target is 5/s.
///
/// Counted as voluntary switches, one per wakeup: preemptions measure what
/// else the runner runs (TIL-749), and the shell shard runs this beside other
/// tests. [`IDLE_CPU_BUDGET`] still catches a thread that spins without
/// blocking.
///
/// Measured on CI's shared x86-64 runner (headless, llvmpipe, debug build;
/// `docs/benchmarks/idle-wakeups-2026-10-09.md` has the tables): 60/s with
/// the frame loop parked. Before TIL-696 a run could also land in a spin of
/// up to 2008/s: an in-process app whose record was still saving at its last
/// frame left a wake deadline that only its next draw moved, so the loop ran
/// ~950 times a second without painting. The hosted pump now resamples it.
///
/// TIL-704 parked the audio-output authority watchdog while no output is
/// open (20/s before, 0/s after): the parked desktop measured 31.9/s (run
/// 89960).
///
/// TIL-707 parked the collaboration broker's service thread (20.1/s before,
/// 0.0/s after, run 90830), which comes off that parked figure too.
///
/// With all three, and TIL-702/TIL-740 quieting the network runtime, the
/// desktop measured 7.30, 7.60 and 7.30/s alone on a runner (runs 92406,
/// 92444, 92445) with the frame loop parked, and 10.29/s in the shell shard
/// beside other tests (run 92498: the collaboration shovel and the identity
/// client preempted by its neighbours).
///
/// TIL-754 let the Terminal's collaboration shovel wait for channel key
/// changes instead of asking the identity service once a second: the shovel
/// went from 3.3 to 0.4–0.5/s and the identity client from 2.0 to 0.2/s.
/// Seven runs alone on a runner then blocked 2.60–3.10/s with the frame loop
/// parked (runs 92855, 92857, 92945, 92947), and 4.90–5.00/s in the three
/// whose window held one frame-loop wake, the top bar's minute repaint
/// (92944, 92946, 92856), each also waking the settings reader and the
/// network runtime. (92856's summary line blamed startup's Terminal frames on
/// that wake; TIL-756 counts per tick.) A ten-second window holds a
/// minute boundary one run in six, so that is part of the spread: its top
/// plus a half, 7.5/s.
const IDLE_WAKEUP_BUDGET: f64 = 7.5;

/// Most CPU the idle desktop's threads may use, in percent of one core over
/// the window. The wakeup count leaves out preemptions, so this is what
/// catches a thread that spins without blocking. Measured 0.1–0.5 % in the
/// same seven runs.
const IDLE_CPU_BUDGET: f64 = 1.5;

/// Most times per second one collaborating app's shovel
/// (`ui-collab-shovel`) and the identity service's thread serving it
/// (`tilo-identity-client`) may wake together. A route that holds no keys
/// asks for them on a timer backing off from 1 s to 30 s, and is told when
/// they change; asking once a second cost 5.3/s together (TIL-754, run
/// 92445); after it the Terminal's measured 0.60–0.70/s in all seven runs.
///
/// The budget is per app with a shovel: before the session waited for its
/// services, the restored Notes notebook never reached the broker, so only
/// the Terminal had one. With both connected, CI measured 2.10/s for the two
/// (PR #4197, run 94384).
const KEY_ASK_BUDGET: f64 = 1.5;

/// Most iterations per second the compositor's frame loop may make while
/// idle, from its `paint-tick` lines. A loop that wakes on a deadline that
/// never moves spins without blocking, which context switches hardly show.
/// An idle loop parks: measured 0/s parked, and 946/s in the TIL-696 spin.
/// Counted from one line per iteration, so only iterations inside the window
/// (TIL-756).
const LOOP_TICK_BUDGET: f64 = 1.0;

/// Frames the top bar's clock paints when a wall-clock minute turns: the one
/// thing an idle desktop's screen should change. A ten-second window holds a
/// turn about one run in six, and the scheduled lane's longer one always does.
/// Measured over a 65 s window (run 93205): one tick, woken by the bar's
/// widget deadline, painting one frame. The `paint-stats` summary that wake
/// printed said 19/24, startup's Terminal frames, which the old count charged
/// to the window: 2.4 ticks/s over the pull-request gate's ten seconds.
const CLOCK_MINUTE_FRAMES: u64 = 1;

/// How late after the minute turns the clock's repaint may land; a turn this
/// close before the window counts as inside it.
const CLOCK_REPAINT_LATE: Duration = Duration::from_secs(1);

/// How long the desktop is left alone after both apps are drawn, so startup
/// work (font caches, record flushes, the first save) is behind it.
pub(crate) const SETTLE: Duration = Duration::from_secs(5);

/// The measured window. `TILO_IDLE_WAKEUP_WINDOW_SECS` lengthens it for the
/// scheduled performance lane; the pull-request gate keeps the default.
fn window() -> Duration {
    let seconds = std::env::var("TILO_IDLE_WAKEUP_WINDOW_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(10);
    Duration::from_secs(seconds)
}

/// Match the native session's launcher-less record scope explicitly, so
/// inherited package or workspace variables cannot redirect the documents
/// this fixture seeds.
const FIXTURE_RECORD_SCOPE: &str = "00000000-0000-0000-0000-000000000000";

/// How many desktops this process started, so each has its own runtime dir.
static STARTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A line the compositor printed, with when the test read it.
type Log = Arc<Mutex<Vec<(Instant, String)>>>;

pub(crate) struct Desktop {
    child: Child,
    control: PathBuf,
    runtime_dir: PathBuf,
    log: Log,
}

impl Desktop {
    /// The headless compositor on a fresh private runtime, signed in, with a
    /// Notes tab saved in its session; returns once its control socket
    /// answers with a laid-out frame.
    fn start() -> Self {
        Self::start_with(&notes_desktop())
    }

    /// The same with `tree` as the saved session (`idle_memory` starts an
    /// empty desktop).
    pub(crate) fn start_with(tree: &shell::shell::state::WorkspaceTree) -> Self {
        tilo_test_env::require_isolated_desktop();
        // SUN_LEN caps a unix socket path, so the runtime dir stays short.
        // One per desktop: this module's tests run beside each other.
        let runtime_dir = PathBuf::from(format!(
            "/run/user/{}/tilo-idle-{}-{}",
            unsafe { libc::getuid() },
            std::process::id(),
            STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).expect("create the private runtime dir");
        let state_path = runtime_dir.join("state.json");
        shell::shell::persist::save_tree_to(tree, &state_path).expect("seed the saved session");
        seed_tab_records(&runtime_dir, tree);
        let identity_root = seed_identity(&runtime_dir);
        let control = runtime_dir.join("ctl");

        let mut command = Command::new(env!("CARGO_BIN_EXE_shell"));
        // A person's session keeps GStreamer's plugin registry in its cache.
        // Share one across this run's test processes (tools/test-env.sh), so
        // only the first pays for the scan.
        if let Some(cache) = std::env::var_os("TILO_TEST_CACHE_DIR") {
            command.env(
                "GST_REGISTRY_1_0",
                Path::new(&cache).join("gst-registry.bin"),
            );
        }
        let mut child = command
            .args(["--mode", "native", "--backend", "headless"])
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_STATE_HOME", runtime_dir.join("state"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            .env("HOME", runtime_dir.join("home"))
            .env("TILO_PACKAGE_ID", shell::shell::SHELL_RECORDS_PACKAGE)
            .env("TILO_WORKSPACE_ID", FIXTURE_RECORD_SCOPE)
            .env("TILO_WORKSPACE_ROOTS", "")
            .env("TILO_STATE_PATH", &state_path)
            .env("TILO_IDENTITY_ROOT", &identity_root)
            .env("TILO_TEST_CONTROL", &control)
            .env("TILO_HEADLESS_SIZE", "1920x1080")
            // Nobody touches it, so it must not lock or blank in the
            // window either: those are state changes, not idling.
            .env("TILO_IDLE_LOCK_SECS", "0")
            .env("TILO_IDLE_BLANK_SECS", "0")
            // A `paint-tick:` line per loop iteration (see `LoopStats`).
            .env("TILO_PAINT_STATS", "ticks")
            .env("RUST_LOG", "warn")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the compositor under test");
        let log: Log = Arc::default();
        for pipe in [
            child
                .stdout
                .take()
                .map(|out| Box::new(out) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|err| Box::new(err) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            // Unread output would block the compositor mid-write.
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if let Ok(mut log) = log.lock() {
                        log.push((Instant::now(), line));
                    }
                }
            });
        }
        let desktop = Self {
            child,
            control,
            runtime_dir,
            log,
        };
        desktop.wait_for("a laid-out first frame", |targets| !targets.is_empty());
        desktop
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    fn log_since(&self, since: Instant) -> Vec<(Instant, String)> {
        self.log
            .lock()
            .map(|log| log.iter().filter(|(at, _)| *at >= since).cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the compositor has printed a line containing `needle`.
    fn logged(&self, needle: &str) -> bool {
        self.log
            .lock()
            .is_ok_and(|log| log.iter().any(|(_, line)| line.contains(needle)))
    }

    /// The last lines the compositor printed, leaving out its per-iteration
    /// `paint-tick` lines, which the report summarises.
    pub(crate) fn log_tail(&self) -> String {
        self.log
            .lock()
            .map(|log| {
                let mut tail: Vec<&str> = log
                    .iter()
                    .rev()
                    .map(|(_, line)| line.as_str())
                    .filter(|line| !line.contains("paint-tick: "))
                    .take(60)
                    .collect();
                tail.reverse();
                tail.join("\n")
            })
            .unwrap_or_default()
    }

    /// One request on the control socket; `None` while it does not answer.
    fn try_control(&self, request: &serde_json::Value) -> Option<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.control).ok()?;
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .ok()?;
        let mut line = serde_json::to_vec(request).ok()?;
        line.push(b'\n');
        stream.write_all(&line).ok()?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).ok()?;
        serde_json::from_str(&response).ok()
    }

    pub(crate) fn control(&self, request: &serde_json::Value) -> serde_json::Value {
        self.try_control(request).unwrap_or_else(|| {
            panic!(
                "the compositor did not answer {request}. Log:\n{}",
                self.log_tail()
            )
        })
    }

    fn target_names(&self) -> Vec<String> {
        self.try_control(&serde_json::json!({"op": "list_targets"}))
            .and_then(|reply| reply.get("targets").and_then(|t| t.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|target| target.get("name")?.as_str().map(str::to_owned))
            .collect()
    }

    /// Wait until the laid-out targets satisfy `done`, failing with the log
    /// if the compositor exits or the hang bound runs out.
    pub(crate) fn wait_for(&self, what: &str, done: impl Fn(&[String]) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let targets = self.target_names();
            if done(&targets) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "waited for {what}; targets {targets:?}. Log:\n{}",
                self.log_tail()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("compositor log tail:\n{}", self.log_tail());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// A signed-in person's desktop with one Notes notebook open, as a saved
/// session restores it.
fn notes_desktop() -> shell::shell::state::WorkspaceTree {
    let mut tree = shell::shell::state::WorkspaceTree::bootstrap_demo();
    // Keep the demo's first workspace with only its Notes tab; an untouched
    // demo tree would be migrated away as legacy sample data on load.
    tree.desktop.workspaces.truncate(1);
    let home = &mut tree.desktop.workspaces[0];
    home.name = "Idle desktop".into();
    home.collaborators.clear();
    let group = &mut home.strips[0].window_groups[0];
    group.tabs.retain(|tab| tab.title == "Notes");
    group.active_tab = group.tabs[0].id;
    tree
}

/// Create the saved tabs' documents before native startup admits only
/// existing ones, then release them so the compositor can open them.
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
            .expect("create the saved tab's document");
    }
    records
        .try_flush()
        .expect("persist the saved tabs' documents");
    drop(records);
    drop(owners);
}

/// A completed identity ceremony: without one the desktop is in first-run
/// onboarding, which is not the idle desktop a person leaves.
fn seed_identity(runtime_dir: &Path) -> PathBuf {
    let identity_dir = runtime_dir.join("config/tilo/identity");
    let identity = ThresholdIdentity::bootstrap_single_device(&mut UnwrapErr(SysRng))
        .expect("create the fixture identity");
    let record = IdentityRecord::from_threshold_identity(
        &identity,
        "Idle test",
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

/// What the compositor's own frame loop did in the window, from the
/// `paint-tick:` line it prints on every iteration (`TILO_PAINT_STATS=ticks`).
///
/// Its two-second `paint-stats:` summaries cannot say this: a summary counts
/// every tick since the previous one and is printed only on a tick, so a loop
/// that parked after startup prints the startup's ticks on its first wake in
/// the window (run 92856: 25 ticks charged to a window that held one), and
/// ticks after the window's last summary are never printed inside it.
struct LoopStats {
    /// Loop iterations that ran in the window.
    ticks: u64,
    /// Of those, the ones that painted a frame.
    painted: u64,
    window: Duration,
    /// What ended the park before each iteration, busiest first.
    woke: Tally,
    /// The same, for the iterations that painted.
    painted_after: Tally,
    /// The `paint-stats:` summaries printed in the window, for their causes.
    summaries: Vec<String>,
}

/// How many times each wake reason was seen, busiest first.
#[derive(Default)]
struct Tally(Vec<(String, u64)>);

impl Tally {
    fn note(&mut self, reason: &str) {
        match self.0.iter_mut().find(|(seen, _)| seen == reason) {
            Some((_, count)) => *count = count.saturating_add(1),
            None => self.0.push((reason.to_owned(), 1)),
        }
        self.0.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    }
}

impl std::fmt::Display for Tally {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let named: Vec<String> = self
            .0
            .iter()
            .map(|(reason, count)| format!("{reason}={count}"))
            .collect();
        f.write_str(&named.join(" "))
    }
}

impl LoopStats {
    /// Counts the `paint-tick:` lines in `lines`, which holds only what the
    /// compositor printed inside the window. Each line is timestamped as the
    /// test reads it, so a tick is charged to the window it ran in, give or
    /// take the pipe's latency.
    fn of(lines: &[(Instant, String)], window: Duration) -> Self {
        let mut stats = Self {
            ticks: 0,
            painted: 0,
            window,
            woke: Tally::default(),
            painted_after: Tally::default(),
            summaries: Vec::new(),
        };
        for (_, line) in lines {
            if line.contains("paint-stats:") {
                stats.summaries.push(line.clone());
                continue;
            }
            // `paint-tick: painted=<bool> woke=<reason>`
            let Some(rest) = line.split("paint-tick: ").nth(1) else {
                continue;
            };
            let field = |name: &str| {
                rest.split_whitespace()
                    .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
            };
            let woke = field("woke").unwrap_or("?");
            stats.ticks = stats.ticks.saturating_add(1);
            stats.woke.note(woke);
            if field("painted") == Some("true") {
                stats.painted = stats.painted.saturating_add(1);
                stats.painted_after.note(woke);
            }
        }
        stats
    }

    fn ticks_per_second(&self) -> f64 {
        let seconds = self.window.as_secs_f64();
        if seconds > 0.0 {
            f64::from(u32::try_from(self.ticks).unwrap_or(u32::MAX)) / seconds
        } else {
            0.0
        }
    }
}

/// How often the threads that ask for and answer a route's channel keys
/// blocked, per second: the in-process apps' collaboration shovel and the
/// identity service's per-client thread. Fails if the shovel is not there to
/// observe: the Terminal opens a route, so a desktop without one did not
/// measure what this bounds.
/// The shovels' and identity clients' wakeups per second, and their budget:
/// [`KEY_ASK_BUDGET`] for each app whose shovel ran.
fn key_ask_wakeups(wakeups: &tilo_test_env::idle::Wakeups) -> (f64, f64) {
    // The kernel truncates thread names to 15 bytes.
    const SHOVEL: &str = "ui-collab-shove";
    const IDENTITY_CLIENT: &str = "tilo-identity-c";
    let rows: Vec<_> = wakeups
        .rows
        .iter()
        .filter(|row| row.process == "shell")
        .filter(|row| row.thread == SHOVEL || row.thread == IDENTITY_CLIENT)
        .collect();
    assert!(
        rows.iter().any(|row| row.thread == SHOVEL),
        "no collaboration shovel ran in the compositor, so the Terminal's route was not measured:\n{wakeups}"
    );
    let shovels: usize = rows
        .iter()
        .filter(|row| row.thread == SHOVEL)
        .map(|row| row.threads)
        .sum();
    let budget = KEY_ASK_BUDGET * f64::from(u32::try_from(shovels).unwrap_or(u32::MAX));
    (
        wakeups.rate(rows.iter().map(|row| row.voluntary).sum()),
        budget,
    )
}

/// Whether a process named `name` runs in the compositor's tree.
pub(crate) fn running(desktop: &Desktop, name: &str) -> bool {
    tilo_test_env::idle::process_tree(desktop.pid())
        .expect("list the compositor's process tree")
        .into_iter()
        .any(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .is_ok_and(|comm| comm.trim() == name)
        })
}

/// Wait until no process named `name` runs in the compositor's tree.
pub(crate) fn wait_until_gone(desktop: &Desktop, name: &str) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if !running(desktop, name) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name} was still running after the hang bound. Log:\n{}",
            desktop.log_tail()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The Notes notebook the session restores publishes its actions where the
/// palette, search and agents find them. The session mounts its restored
/// hosted tabs while it builds the desktop, before it binds the
/// collaboration broker and identity service they connect to; the tab found
/// no socket, stayed offline and published nothing until it was reopened
/// (fixed by the session-services handshake).
#[test]
fn the_restored_notes_notebook_publishes_its_actions() {
    let desktop = Desktop::start();
    desktop.wait_for("the restored Notes notebook", |targets| {
        targets.iter().any(|name| name == "notes.first_note")
    });
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reply = desktop.control(&serde_json::json!({
            "op": "actions_enumerate", "query": "New note", "detail": "compact", "wait_ms": 5000,
        }));
        let published = reply["answer"]["actions"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["application"] == "so.tilo.notes" && row["operation"] == "new-page")
        });
        if published {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the restored Notes tab never published new-page: {reply}\nLog:\n{}",
            desktop.log_tail()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn an_idle_desktop_with_two_apps_stays_within_its_wakeup_budget() {
    let desktop = Desktop::start();
    desktop.wait_for("the restored Notes notebook", |targets| {
        targets.iter().any(|name| name == "notes.first_note")
    });
    let reply = desktop.control(&serde_json::json!({
        "op": "actions_invoke", "provider": "actions", "id": "window.new.terminal",
    }));
    assert!(
        reply.get("err").is_none(),
        "the desktop refused to open a Terminal: {reply}"
    );
    desktop.wait_for("the Terminal", |targets| {
        targets.iter().any(|name| name == "terminal")
    });
    // GStreamer builds its plugin registry once, in a child, into the fresh
    // XDG cache; a real session keeps that cache, so it is startup work.
    // Settle until a whole settle period passed without one.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        wait_until_gone(&desktop, "gst-plugin-scan");
        std::thread::sleep(SETTLE);
        if !running(&desktop, "gst-plugin-scan") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gst-plugin-scan kept starting. Log:\n{}",
            desktop.log_tail()
        );
    }

    let measured_from = Instant::now();
    let wall_from = SystemTime::now();
    let wakeups = tilo_test_env::idle::idle_wakeups(desktop.pid(), Duration::ZERO, window())
        .expect("sample the compositor's process tree");
    let measured_until = Instant::now();
    let clock_turns = clock_minutes(wall_from, SystemTime::now());
    let in_window: Vec<(Instant, String)> = desktop
        .log_since(measured_from)
        .into_iter()
        .filter(|(at, _)| *at <= measured_until)
        .collect();
    let frame_loop = LoopStats::of(&in_window, wakeups.window);
    // A parked loop prints nothing in the window, which reads as zero; that
    // only means something if the loop prints its ticks at all.
    assert!(
        desktop.logged("paint-tick: "),
        "the compositor printed no paint-tick line, so its frame loop was not observed. Log:\n{}",
        desktop.log_tail()
    );
    assert!(
        desktop
            .try_control(&serde_json::json!({"op": "list_targets"}))
            .is_some(),
        "the compositor stopped answering while idle. Log:\n{}",
        desktop.log_tail()
    );

    let (key_asks, key_ask_budget) = key_ask_wakeups(&wakeups);
    let report = format!(
        "{wakeups}\nblocked {:.2} times a second; the collaboration shovel and identity client {key_asks:.2}\ncompositor frame loop: {:.1} ticks/s ({} ticks, woken by: {}), {} frames painted (after: {}), {clock_turns} clock minute(s) turned; its paint-stats in the window:\n  {}",
        wakeups.blocking_per_second(),
        frame_loop.ticks_per_second(),
        frame_loop.ticks,
        frame_loop.woke,
        frame_loop.painted,
        frame_loop.painted_after,
        frame_loop.summaries.join("\n  ")
    );
    eprintln!("{report}");
    // One machine-readable line for the scheduled lane's adapter
    // (tools/perf/adapters.py idle-wakeups).
    println!(
        "IDLE-WAKEUPS total_per_s={:.3} blocking_per_s={:.3} shell_per_s={:.3} loop_ticks_per_s={:.3} cpu_percent={:.3} window_ms={} threads_started={}",
        wakeups.per_second(),
        wakeups.blocking_per_second(),
        wakeups.process_per_second("shell"),
        frame_loop.ticks_per_second(),
        wakeups.total_cpu_percent(),
        wakeups.window.as_millis(),
        wakeups.threads_started,
    );
    assert!(
        wakeups.blocking_per_second() <= IDLE_WAKEUP_BUDGET,
        "the idle desktop woke {:.2} times a second, over its budget of {IDLE_WAKEUP_BUDGET}/s:\n{report}",
        wakeups.blocking_per_second(),
    );
    assert!(
        wakeups.total_cpu_percent() <= IDLE_CPU_BUDGET,
        "the idle desktop used {:.2} % of a core, over its budget of {IDLE_CPU_BUDGET} %:\n{report}",
        wakeups.total_cpu_percent(),
    );
    assert!(
        key_asks <= key_ask_budget,
        "the apps' collaboration shovels and the identity client woke {key_asks:.2} times a second, over {key_ask_budget}/s ({KEY_ASK_BUDGET}/s per app): is a route asking for its channel keys on a 1 s timer again?\n{report}",
    );
    assert!(
        frame_loop.ticks_per_second() <= LOOP_TICK_BUDGET,
        "the idle compositor's frame loop ran {:.1} times a second, over its budget of {LOOP_TICK_BUDGET}/s:\n{report}",
        frame_loop.ticks_per_second(),
    );
    let clock_frames = clock_turns.saturating_mul(CLOCK_MINUTE_FRAMES);
    assert!(
        frame_loop.painted <= clock_frames,
        "the idle desktop kept painting: {} frames, where the top bar's clock accounts for {clock_frames}:\n{report}",
        frame_loop.painted,
    );
}

/// How many wall-clock minutes turned over between `from` and `until`: the
/// top bar's clock repaints at each. A turn up to [`CLOCK_REPAINT_LATE`]
/// before `from` counts too, since its repaint can land just inside.
fn clock_minutes(from: SystemTime, until: SystemTime) -> u64 {
    let minute = |at: SystemTime| {
        at.duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() / 60)
    };
    let from = from.checked_sub(CLOCK_REPAINT_LATE).unwrap_or(from);
    minute(until).saturating_sub(minute(from))
}
