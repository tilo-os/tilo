//! How often an idle desktop wakes the machine.
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

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "benchmark assertions and the shared integration fixture fail with diagnostics"
)]

// Explicit --all-targets --all-features test builds use libtest instead of main.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

use std::time::{Duration, Instant, SystemTime};

// The other consumers exercise fixture methods this benchmark does not need.
#[allow(
    dead_code,
    reason = "shared desktop fixture also serves integration tests"
)]
#[path = "../tests/idle_wakeups.rs"]
mod idle_wakeups;

use idle_wakeups::{Desktop, SETTLE, running, wait_until_gone};

fn main() -> std::process::ExitCode {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    an_idle_desktop_with_two_apps_stays_within_its_wakeup_budget();
    std::process::ExitCode::SUCCESS
}

/// Most times per second the idle desktop's threads may wake, across every
/// thread of the compositor and its children. The product target is 5/s.
///
/// Counted as voluntary switches, one per wakeup: preemptions measure what
/// else the runner runs, rather than work done by the desktop. [`IDLE_CPU_BUDGET`] still catches a thread that spins without
/// blocking.
///
/// Measured on CI's shared x86-64 runner (headless, llvmpipe, debug build;
/// `docs/benchmarks/idle-wakeups-2026-10-09.md` has the tables): 60/s with
/// the frame loop parked. Before #3986 a run could also land in a spin of
/// up to 2008/s: an in-process app whose record was still saving at its last
/// frame left a wake deadline that only its next draw moved, so the loop ran
/// ~950 times a second without painting. The hosted pump now resamples it.
///
/// #3988 parked the audio-output authority watchdog while no output is
/// open (20/s before, 0/s after): the parked desktop measured 31.9/s (run
/// 89960).
///
/// #4022 parked the collaboration broker's service thread (20.1/s before,
/// 0.0/s after, run 90830), which comes off that parked figure too.
///
/// With all three, and #4074/#4094 quieting the network runtime, the
/// desktop measured 7.30, 7.60 and 7.30/s alone on a runner (runs 92406,
/// 92444, 92445) with the frame loop parked, and 10.29/s in the shell shard
/// beside other tests (run 92498: the collaboration shovel and the identity
/// client preempted by its neighbours).
///
/// #4145 let the Terminal's collaboration shovel wait for channel key
/// changes instead of asking the identity service once a second: the shovel
/// went from 3.3 to 0.4–0.5/s and the identity client from 2.0 to 0.2/s.
/// Seven runs alone on a runner then blocked 2.60–3.10/s with the frame loop
/// parked (runs 92855, 92857, 92945, 92947), and 4.90–5.00/s in the three
/// whose window held one frame-loop wake, the top bar's minute repaint
/// (92944, 92946, 92856), each also waking the settings reader and the
/// network runtime. (92856's summary line blamed startup's Terminal frames on
/// that wake; #4163 counts per tick.) A ten-second window holds a
/// minute boundary one run in six, so that is part of the spread: its top
/// plus a half, 7.5/s.
///
/// That minute wake then grew: every one of the gate's 15 failures from run
/// 94897 to 99577 (7.59–9.70/s, in the shell shard) held it. It re-stamped
/// the device fabric's unchanged snapshot as a presence heartbeat and wrote
/// three files, synced, on the frame thread (9–10 blocking switches, plus the
/// state directory's watchers), and each of its ~20 settings reads woke the
/// settings reader. Neither happens now while nobody can hear the heartbeat
/// or the settings copy is unchanged, and the audio policy no longer polls
/// for a `PipeWire` that is not running. Alone on a runner, main before the
/// last two blocked 3.40–5.90/s (runs 100177–100181); with them, ten runs
/// blocked 2.90–4.20/s (runs 100296–100305), of it 2.20–3.40/s the
/// collaboration shovels' channel-key rounds early in the session and
/// 0.50–0.60/s the filesystem authority worker retrying a publisher the
/// headless session does not have. Their top plus a little under a half:
/// 6/s.
///
/// Those were the last wakers. The channel service now tells a route whose
/// room has no known steward when one is learned (and retries a channel it
/// failed to open on its own backoff), so a route it answered waits for
/// that push instead of asking on the timer; the authority worker waits for
/// its publisher with a creation-only watch instead of retrying it. The
/// publisher's spawn attempts had also woken `tilo-secrets-bus` 0.2/s (gone
/// with the watch alone, runs 100711–100712). Ten runs on main before blocked
/// 2.90–4.10/s (runs 100582–100591: shovels 1.40–2.10, identity client
/// 0.70–1.20, authority worker 0.40–0.60, secrets bus 0.20); with both,
/// 0.00–0.40/s (runs 100713–100722), the 0.40 the one window holding a
/// minute repaint and nothing else. Its top plus a margin that still
/// catches one thread waking once a second in the windows without a
/// minute turn: 1/s.
///
/// The privacy indicator then stopped starting `pw-dump --monitor` each time
/// the frame loop left idle, the clock's minute repaint included: running
/// capture streams are followed on the status reader's own `PipeWire`
/// connection, and an idle desktop with nothing recording starts no process
/// for them. The scheduled lane's 60 s windows, each holding one minute
/// turn, blocked 0.57–0.62/s on main before (run r366e8b0aa1, of it
/// 0.08/s the privacy worker) and 0.52–0.58/s with it (run r6fff70b7cb).
/// Its top plus a margin that still catches a thread waking once every
/// three seconds: 0.9/s.
const IDLE_WAKEUP_BUDGET: f64 = 0.9;

/// Most CPU the idle desktop's threads may use, in percent of one core over
/// the window. The wakeup count leaves out preemptions, so this is what
/// catches a thread that spins without blocking. Measured 0.1–0.5 % in the
/// same seven runs.
const IDLE_CPU_BUDGET: f64 = 1.5;

/// Most rounds of channel-key questions per second each in-process app with
/// a collaboration shovel may start. A route asks when it is told its
/// channel changed; it asks on a timer (backing off from 1 s to 30 s) only
/// while nothing will tell it: a question the identity service refused, or
/// a removal. The fixture's routes are answered and pushed, so the ten runs
/// above started no round in the window, where the timer used to start up
/// to three per shovel. One round per shovel per window leaves room for a
/// pushed change; asking on the backoff timer again, or on a 1 s one (ten),
/// is caught. Each shovel starts its own rounds: the restored Notes
/// notebook and the Terminal both have one.
const KEY_ROUND_BUDGET: f64 = 0.1;

/// Most iterations per second the compositor's frame loop may make while
/// idle, from its `paint-tick` lines. A loop that wakes on a deadline that
/// never moves spins without blocking, which context switches hardly show.
/// An idle loop parks: measured 0/s parked, and 946/s in the #3986 spin.
/// Counted from one line per iteration, so only iterations inside the window.
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

/// The measured window. `TILO_IDLE_WAKEUP_WINDOW_SECS` lengthens it for the
/// scheduled performance lane; local benchmark runs keep the default.
fn window() -> Duration {
    let seconds = std::env::var("TILO_IDLE_WAKEUP_WINDOW_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(10);
    Duration::from_secs(seconds)
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
    painted: u64,
    window: Duration,
    /// What ended the park before each iteration, busiest first.
    woke: Tally,
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
/// identity service's per-client thread. Reported beside the rounds, as what
/// they cost. Fails if the shovel is not there to observe: the Terminal
/// opens a route, so a desktop without one did not measure what the rounds
/// bound. Returns their wakeups per second and how many shovels ran.
fn key_ask_wakeups(wakeups: &tilo_test_env::idle::Wakeups) -> (f64, usize) {
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
    let shovels = rows
        .iter()
        .filter(|row| row.thread == SHOVEL)
        .map(|row| row.threads)
        .sum();
    (
        wakeups.rate(rows.iter().map(|row| row.voluntary).sum()),
        shovels,
    )
}

/// The rounds of channel-key questions the desktop logged in `lines`.
fn key_rounds(lines: &[(Instant, String)]) -> usize {
    lines
        .iter()
        .filter(|(_, line)| line.contains("channel keys") && line.contains("collab: asking"))
        .count()
}

/// Most logged lines the report quotes; the count covers the rest.
const LOGGED_LINES_SHOWN: usize = 40;

/// What the desktop and its children wrote to their output in the window,
/// leaving out the frame loop's own lines, which [`LoopStats`] summarises.
/// Every line wakes the compositor's stderr relay, and a line repeated on a
/// timer names the waker the table only shows as `stderr-relay`.
fn logged_lines(lines: &[(Instant, String)], from: Instant) -> String {
    let logged: Vec<String> = lines
        .iter()
        .filter(|(_, line)| !line.contains("paint-tick: ") && !line.contains("paint-stats:"))
        .map(|(at, line)| {
            format!(
                "  +{:.3}s {line}",
                at.saturating_duration_since(from).as_secs_f64()
            )
        })
        .collect();
    let mut shown = vec![format!("{} lines logged in the window", logged.len())];
    shown.extend(logged.iter().take(LOGGED_LINES_SHOWN).cloned());
    if logged.len() > LOGGED_LINES_SHOWN {
        shown.push(format!(
            "  and {} more",
            logged.len().saturating_sub(LOGGED_LINES_SHOWN)
        ));
    }
    shown.join("\n")
}

/// Leave `desktop` alone until its startup work is behind it.
fn settle(desktop: &Desktop) {
    // GStreamer builds its plugin registry once, in a child, into the fresh
    // XDG cache; a real session keeps that cache, so it is startup work.
    // Settle until a whole settle period passed without one.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        wait_until_gone(desktop, "gst-plugin-scan");
        std::thread::sleep(SETTLE);
        if !running(desktop, "gst-plugin-scan") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gst-plugin-scan kept starting. Log:\n{}",
            desktop.log_tail()
        );
    }
}

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
    settle(&desktop);

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

    let (key_asks, shovels) = key_ask_wakeups(&wakeups);
    let rounds = key_rounds(&in_window);
    let rounds_per_second =
        f64::from(u32::try_from(rounds).unwrap_or(u32::MAX)) / wakeups.window.as_secs_f64();
    let round_budget = KEY_ROUND_BUDGET * f64::from(u32::try_from(shovels).unwrap_or(u32::MAX));
    let logged = logged_lines(&in_window, measured_from);
    let report = format!(
        "{wakeups}\nblocked {:.2} times a second; the collaboration shovel and identity client {key_asks:.2}, in {rounds} rounds of channel-key questions\ncompositor frame loop: {:.1} ticks/s ({} ticks, woken by: {}), {} frames painted (after: {}), {clock_turns} clock minute(s) turned; its paint-stats in the window:\n  {}\n{logged}",
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
        rounds_per_second <= round_budget,
        "the in-process apps asked for channel keys {rounds_per_second:.2} times a second, over {round_budget:.2}/s ({KEY_ROUND_BUDGET}/s for each of {shovels} shovels): is a route asking on a 1 s timer again?\n{report}",
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
