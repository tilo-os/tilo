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
//! and thread of every wakeup, and the compositor's own `paint-stats` lines
//! (`TILO_PAINT_STATS`) say what woke its frame loop and whether it painted.
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
/// whose window held one frame-loop wake: the top bar's minute repaint
/// (92944, 92946) or a late Terminal repaint (92856), each also waking the
/// settings reader and the network runtime. A ten-second window holds a
/// minute boundary one run in six, so that is part of the spread: its top
/// plus a half, 7.5/s.
const IDLE_WAKEUP_BUDGET: f64 = 7.5;

/// Most CPU the idle desktop's threads may use, in percent of one core over
/// the window. The wakeup count leaves out preemptions, so this is what
/// catches a thread that spins without blocking. Measured 0.1–0.5 % in the
/// same seven runs.
const IDLE_CPU_BUDGET: f64 = 1.5;

/// Most rounds of channel-key questions per second the idle desktop's
/// in-process apps may start. A route that holds no keys asks on a timer
/// backing off from 1 s to 30 s, and is told when they change: from the
/// settle period on, a 10 s window holds at most three rounds of that
/// backoff, however many routes ask in each and however loaded the machine
/// is. Asking on a 1 s timer, the regression this catches (5.3 wakeups/s
/// together, TIL-754), is ten.
const KEY_ROUND_BUDGET: f64 = 0.35;

/// Most iterations per second the compositor's frame loop may make while
/// idle, from its `paint-stats` lines. A loop that wakes on a deadline that
/// never moves spins without blocking, which context switches hardly show.
/// An idle loop parks: measured 0/s parked, and 946/s in the TIL-696 spin.
const LOOP_TICK_BUDGET: f64 = 1.0;

/// How long the desktop is left alone after both apps are drawn, so startup
/// work (font caches, record flushes, the first save) is behind it.
const SETTLE: Duration = Duration::from_secs(5);

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

/// A line the compositor printed, with when the test read it.
type Log = Arc<Mutex<Vec<(Instant, String)>>>;

struct Desktop {
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
        tilo_test_env::require_isolated_desktop();
        // SUN_LEN caps a unix socket path, so the runtime dir stays short.
        let runtime_dir = PathBuf::from(format!(
            "/run/user/{}/tilo-idle-{}",
            unsafe { libc::getuid() },
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).expect("create the private runtime dir");
        let state_path = runtime_dir.join("state.json");
        let tree = notes_desktop();
        shell::shell::persist::save_tree_to(&tree, &state_path).expect("seed the saved session");
        seed_tab_records(&runtime_dir, &tree);
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
            .env("TILO_PAINT_STATS", "1")
            // Every round of channel-key questions is logged, so the gate counts
            // the rounds the shovel starts, not how often contention woke it.
            .env("RUST_LOG", "warn,tilo::collab::key_rounds=debug")
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

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn log_since(&self, since: Instant) -> Vec<(Instant, String)> {
        self.log
            .lock()
            .map(|log| log.iter().filter(|(at, _)| *at >= since).cloned().collect())
            .unwrap_or_default()
    }

    fn log_tail(&self) -> String {
        self.log
            .lock()
            .map(|log| {
                let skip = log.len().saturating_sub(60);
                log.iter()
                    .skip(skip)
                    .map(|(_, line)| line.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
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

    fn control(&self, request: &serde_json::Value) -> serde_json::Value {
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
    fn wait_for(&self, what: &str, done: impl Fn(&[String]) -> bool) {
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

/// What the compositor's own frame loop did, from its `paint-stats` lines.
struct LoopStats {
    /// Loop iterations reported by the lines printed in the window.
    ticks: u64,
    /// Frames painted, from lines whose whole period lies in the window.
    painted: u64,
    window: Duration,
    /// Every `paint-stats` line printed in the window.
    lines: Vec<String>,
}

impl LoopStats {
    /// A line covers the time since the one before it and is printed on the
    /// first iteration two seconds after it, so a loop that never wakes
    /// prints nothing, which reads as zero. Ticks count from every line
    /// printed after `since`, so a spin that stops inside the window still
    /// shows; painted frames only from lines whose whole period lies after
    /// `since`, so a frame of the settle period is not charged to it.
    fn after(lines: &[(Instant, String)], since: Instant, window: Duration) -> Self {
        let mut stats = Self {
            ticks: 0,
            painted: 0,
            window,
            lines: Vec::new(),
        };
        let mut previous: Option<Instant> = None;
        for (at, line) in lines
            .iter()
            .filter(|(_, line)| line.contains("paint-stats:"))
        {
            if *at >= since {
                stats.lines.push(line.clone());
            }
            // `paint-stats: <painted>/<ticks> painted | ...`
            let Some((painted, ticks)) = line
                .split("paint-stats: ")
                .nth(1)
                .and_then(|rest| rest.split(" painted |").next())
                .and_then(|counts| counts.split_once('/'))
                .and_then(|(painted, ticks)| {
                    Some((
                        painted.trim().parse::<u64>().ok()?,
                        ticks.trim().parse::<u64>().ok()?,
                    ))
                })
            else {
                continue;
            };
            if *at >= since {
                stats.ticks = stats.ticks.saturating_add(ticks);
            }
            if previous.is_some_and(|before| before >= since) {
                stats.painted = stats.painted.saturating_add(painted);
            }
            previous = Some(*at);
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
/// identity service's per-client thread. Reported beside the rounds, as what
/// they cost. Fails if the shovel is not there to observe: the Terminal
/// opens a route, so a desktop without one did not measure what the rounds
/// bound.
fn key_ask_wakeups(wakeups: &tilo_test_env::idle::Wakeups) -> f64 {
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
    wakeups.rate(rows.iter().map(|row| row.voluntary).sum())
}

/// The rounds of channel-key questions the desktop logged in `lines`.
fn key_rounds(lines: &[(Instant, String)]) -> usize {
    lines
        .iter()
        .filter(|(_, line)| line.contains("channel keys") && line.contains("collab: asking"))
        .count()
}

/// Whether a process named `name` runs in the compositor's tree.
fn running(desktop: &Desktop, name: &str) -> bool {
    tilo_test_env::idle::process_tree(desktop.pid())
        .expect("list the compositor's process tree")
        .into_iter()
        .any(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .is_ok_and(|comm| comm.trim() == name)
        })
}

/// Wait until no process named `name` runs in the compositor's tree.
fn wait_until_gone(desktop: &Desktop, name: &str) {
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
    let wakeups = tilo_test_env::idle::idle_wakeups(desktop.pid(), Duration::ZERO, window())
        .expect("sample the compositor's process tree");
    let measured_until = Instant::now();
    let in_window: Vec<(Instant, String)> = desktop
        .log_since(measured_from)
        .into_iter()
        .filter(|(at, _)| *at <= measured_until)
        .collect();
    let frame_loop = LoopStats::after(&in_window, measured_from, wakeups.window);
    assert!(
        desktop
            .try_control(&serde_json::json!({"op": "list_targets"}))
            .is_some(),
        "the compositor stopped answering while idle. Log:\n{}",
        desktop.log_tail()
    );

    let key_asks = key_ask_wakeups(&wakeups);
    let rounds = key_rounds(&in_window);
    let rounds_per_second =
        f64::from(u32::try_from(rounds).unwrap_or(u32::MAX)) / wakeups.window.as_secs_f64();
    let report = format!(
        "{wakeups}\nblocked {:.2} times a second; the collaboration shovel and identity client {key_asks:.2}, in {rounds} rounds of channel-key questions\ncompositor frame loop: {:.1} ticks/s, {} frames painted; its paint-stats in the window:\n  {}",
        wakeups.blocking_per_second(),
        frame_loop.ticks_per_second(),
        frame_loop.painted,
        frame_loop.lines.join("\n  ")
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
        rounds_per_second <= KEY_ROUND_BUDGET,
        "the in-process apps asked for channel keys {rounds_per_second:.2} times a second, over {KEY_ROUND_BUDGET}/s: is a route asking on a 1 s timer again?\n{report}",
    );
    assert!(
        frame_loop.ticks_per_second() <= LOOP_TICK_BUDGET,
        "the idle compositor's frame loop ran {:.1} times a second, over its budget of {LOOP_TICK_BUDGET}/s:\n{report}",
        frame_loop.ticks_per_second(),
    );
    assert_eq!(
        frame_loop.painted, 0,
        "the idle desktop kept painting:\n{report}"
    );
}
