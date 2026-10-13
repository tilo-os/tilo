//! What a workspace switch costs the compositor's frame loop when the
//! workspace switched back to has apps on it.
//!
//! The production compositor runs headless with a signed-in person's desktop:
//! a Notes notebook restored from the saved session and a Terminal opened
//! through the desktop's own action, both framework apps the compositor hosts,
//! and a second, empty workspace. After a first, untimed visit, Super+Page Down
//! goes to the empty workspace and Super+Page Up comes back, as
//! vm-acceptance W16 does once. The frame loop's own trace
//! (`TILO_FRAME_TRACE`) gives each tick's thread CPU and its phases; the gate
//! is the median of five switch-backs' costliest ticks. Each sample includes
//! the entire switch, so a consistently costly remount still fails, without
//! letting a single unrelated service/control wake set the frame budget.
//!
//! The gate is relative to the same run: the costliest tick may take a fixed
//! multiple of an ordinary animation tick of the same switches, the frame
//! loop's steady cost on this machine at this moment. A slow or contended
//! runner slows both, so it cannot fail the gate; a switch back that does
//! more work than putting kept content back still does.
//!
//! Coming back used to build the workspace again from nothing: both apps were
//! mounted afresh (their records opened, their first sync and update) and every
//! widget drew for the first time (W16 on the VM: one 84 ms tick, pump 49 ms
//! and scene 70 ms). The desktop now keeps a workspace's content while another
//! one is shown and puts it back on the way back.

use std::path::Path;
use std::time::Duration;

use crate::idle_wakeups::{Desktop, notes_desktop};

/// Most thread CPU the costliest tick of a switch back may take, as a multiple
/// of the median animation tick of the same switches back (a tick painted
/// for the transition's animation alone, the frame loop's steady cost).
///
/// Measured on a shared x86-64 build host (headless, llvmpipe, debug build). Putting
/// the kept content back: a costliest tick of 6.3–9 animation ticks.
/// Rebuilding the workspace, as every switch back did before its content was
/// kept: 22.6–22.8 animation ticks. On CI's shared x86-64 runner the kept
/// content's costliest tick ran 19.5–35.6 ms, which an absolute budget of
/// 33.4 ms (W16's `frame_cpu_ms_max`) failed on a slow runner.
const SWITCH_BACK_PEAK_PER_ANIMATION_TICK: f64 = 14.0;

/// Fixed independent observations, not retries: every round is measured and
/// reported, even if it exceeds the budget. Shared-runner cache contention and
/// periodic service work can inflate a single tick's actual thread CPU, not
/// just wall time. Gate the typical peak, never the median animation tick
/// (which would hide a consistently expensive first frame).
const SWITCH_ROUNDS: usize = 5;

/// The frame loop writes its trace a tick at least this long after the last
/// write, so a window is on disk once two ticks this far apart have run.
const TRACE_FLUSH: Duration = Duration::from_millis(1_050);

/// A switch's frame run is 400 ms; anything it brought in after that (an
/// app's next frame) lands well inside this.
const SWITCH_SETTLE: Duration = Duration::from_millis(1_500);

/// How long the apps must go without painting before the switches start.
const APPS_QUIET: Duration = Duration::from_secs(2);

/// The trace's per-phase wall times, as the report names them.
const PHASES: [(&str, &str); 7] = [
    ("input", "input_us"),
    ("pump", "pump_us"),
    ("layout", "layout_us"),
    ("scene", "scene_us"),
    ("submit", "submit_us"),
    ("control", "testctl_us"),
    ("clients", "clients_us"),
];

fn ms(us: u64) -> f64 {
    Duration::from_micros(us).as_secs_f64() * 1_000.0
}

/// One frame-loop tick from the trace.
#[derive(Debug)]
struct Tick {
    seq: u64,
    painted: bool,
    /// Wall time of each of [`PHASES`], in microseconds.
    phases: Vec<u64>,
    cpu_us: Option<u64>,
    input_step: String,
    paint_reasons: String,
}

impl Tick {
    fn cpu_ms(&self) -> Option<f64> {
        self.cpu_us.map(ms)
    }

    fn describe(&self) -> String {
        let phases = PHASES
            .iter()
            .zip(&self.phases)
            .filter(|(_, us)| **us >= 100)
            .map(|((name, _), us)| format!("{name} {:.2} ms", ms(*us)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "tick {}: cpu {} ms; wall {phases} (input step {}; {})",
            self.seq,
            self.cpu_ms()
                .map_or_else(|| "?".to_owned(), |ms| format!("{ms:.2}")),
            self.input_step,
            if self.painted {
                format!("painted for {}", self.paint_reasons)
            } else {
                "not painted".to_owned()
            },
        )
    }
}

fn read_trace(path: &Path) -> Vec<Tick> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let columns: Vec<&str> = header.split(',').collect();
    let at = |name: &str| {
        columns
            .iter()
            .position(|column| *column == name)
            .unwrap_or_else(|| panic!("the frame trace has no {name} column: {header}"))
    };
    let (seq, painted, cpu, step, reasons) = (
        at("seq"),
        at("painted"),
        at("cpu_us"),
        at("input_step"),
        at("paint_reasons"),
    );
    let phases: Vec<usize> = PHASES.iter().map(|(_, column)| at(column)).collect();
    lines
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            let field = |index: usize| fields.get(index).copied().unwrap_or_default();
            let number = |index: usize| field(index).parse::<u64>().ok();
            Some(Tick {
                seq: number(seq)?,
                painted: field(painted) == "1",
                phases: phases
                    .iter()
                    .map(|index| number(*index).unwrap_or_default())
                    .collect(),
                cpu_us: number(cpu),
                input_step: field(step).to_owned(),
                paint_reasons: field(reasons).to_owned(),
            })
        })
        .collect()
}

fn active_workspace(desktop: &Desktop) -> serde_json::Value {
    let reply = desktop.control(&serde_json::json!({"op": "state_dump"}));
    reply.get("state").unwrap_or(&reply)["workspace_tree"]["active"].clone()
}

fn modifiers(desktop: &Desktop, logo: bool) {
    let request = serde_json::json!({"op": "modifiers", "ctrl": false, "shift": false, "alt": false, "logo": logo});
    let reply = desktop.control(&request);
    assert!(reply.get("err").is_none(), "{request} refused: {reply}");
}

/// Get every tick that began before this call onto disk: wake the loop, wait
/// out the flush interval, wake it twice more. The wake is an input that
/// changes nothing (Super is already up): a state dump would wake the loop
/// too, but costs it tens of milliseconds, which would read as the switch's.
fn sync_trace(desktop: &Desktop) {
    modifiers(desktop, false);
    std::thread::sleep(TRACE_FLUSH);
    modifiers(desktop, false);
    modifiers(desktop, false);
}

/// Wait until no app has painted for [`APPS_QUIET`]: the Terminal's shell
/// starting and printing its prompt is the app's own work, not a switch's,
/// and lands whenever the shell is ready.
fn wait_for_quiet_apps(desktop: &Desktop, trace: &Path) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        sync_trace(desktop);
        let mark = read_trace(trace).last().map(|tick| tick.seq);
        std::thread::sleep(APPS_QUIET);
        sync_trace(desktop);
        let painting: Vec<Tick> = read_trace(trace)
            .into_iter()
            .filter(|tick| mark.is_none_or(|mark| tick.seq > mark))
            .filter(|tick| tick.painted && tick.paint_reasons.split('+').any(|r| r == "apps"))
            .collect();
        if painting.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the apps kept painting with nobody touching them:\n{}",
            table(&painting)
        );
    }
}

/// The ticks of one Super+`key` switch to workspace `to`: from the chord
/// until its frame run and whatever it brought in are over.
fn switch(desktop: &Desktop, trace: &Path, key: &str, to: &serde_json::Value) -> Vec<Tick> {
    sync_trace(desktop);
    let mark = read_trace(trace).last().map(|tick| tick.seq);
    // As a person presses it: hold Super, tap the key, release.
    modifiers(desktop, true);
    let reply = desktop.control(&serde_json::json!({"op": "key", "key": key}));
    assert!(reply.get("err").is_none(), "key {key} refused: {reply}");
    modifiers(desktop, false);
    // Nothing else is asked of the loop until the switch is over.
    std::thread::sleep(SWITCH_SETTLE);
    sync_trace(desktop);
    let ticks = read_trace(trace)
        .into_iter()
        .filter(|tick| mark.is_none_or(|mark| tick.seq > mark))
        .collect();
    assert_eq!(
        active_workspace(desktop),
        *to,
        "Super+{key} did not switch to workspace {to}. Log:\n{}",
        desktop.log_tail()
    );
    ticks
}

fn table(ticks: &[Tick]) -> String {
    ticks
        .iter()
        .filter(|tick| tick.painted || tick.cpu_us.unwrap_or_default() >= 5_000)
        .map(|tick| format!("    {}", tick.describe()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The median thread CPU of the ticks painted for the animation alone, in
/// milliseconds: what one ordinary frame costs on this machine now.
fn animation_tick_ms(rounds: &[&[Tick]]) -> f64 {
    let mut cpu: Vec<u64> = rounds
        .iter()
        .flat_map(|ticks| ticks.iter())
        .filter(|tick| tick.painted && tick.paint_reasons == "animation")
        .filter_map(|tick| tick.cpu_us)
        .collect();
    assert!(
        cpu.len() >= SWITCH_ROUNDS,
        "too few animation ticks ({}) to measure an ordinary frame by",
        cpu.len()
    );
    cpu.sort_unstable();
    ms(cpu[cpu.len() / 2])
}

/// Reduce whole journeys, not individual animation frames. Require CPU on
/// every tick so an incomplete trace cannot silently select a cheaper peak.
fn median_peak<'a>(rounds: &[&'a [Tick]]) -> &'a Tick {
    assert_eq!(rounds.len(), SWITCH_ROUNDS);
    let mut peaks: Vec<&Tick> = rounds
        .iter()
        .map(|ticks| {
            assert!(
                ticks.iter().any(|tick| tick.painted),
                "no switch frame observed"
            );
            assert!(
                ticks.iter().all(|tick| tick.cpu_us.is_some()),
                "thread CPU missing from the switch trace"
            );
            ticks
                .iter()
                .max_by_key(|tick| tick.cpu_us)
                .expect("a painted tick exists")
        })
        .collect();
    peaks.sort_unstable_by_key(|tick| tick.cpu_us);
    peaks[SWITCH_ROUNDS / 2]
}

#[test]
fn sampling_keeps_a_repeated_expensive_first_tick_but_not_one_outlier() {
    let make = |cpu_us, reasons: &str| Tick {
        seq: 0,
        painted: true,
        phases: vec![0; PHASES.len()],
        cpu_us: Some(cpu_us),
        input_step: "services".into(),
        paint_reasons: reasons.into(),
    };
    let ordinary = vec![
        make(19_900, "session"),
        make(2_000, "animation"),
        make(2_000, "animation"),
    ];
    let remounted = vec![
        make(90_000, "session"),
        make(2_000, "animation"),
        make(2_000, "animation"),
    ];
    assert_eq!(
        median_peak(&[&ordinary, &ordinary, &remounted, &ordinary, &ordinary]).cpu_us,
        Some(19_900)
    );
    let rounds = [&remounted, &remounted, &ordinary, &remounted, &remounted].map(Vec::as_slice);
    assert!(
        median_peak(&rounds).cpu_ms().unwrap()
            > SWITCH_BACK_PEAK_PER_ANIMATION_TICK * animation_tick_ms(&rounds)
    );
}

/// The same switches on a runner three times slower pass as they did; the
/// same slowdown in the switch-back tick alone does not.
#[test]
fn the_gate_follows_the_runner_not_an_absolute_budget() {
    let round = |peak_us, animation_us| {
        let mut ticks = vec![Tick {
            seq: 0,
            painted: true,
            phases: vec![0; PHASES.len()],
            cpu_us: Some(peak_us),
            input_step: "services".into(),
            paint_reasons: "session+revision+animation+transition".into(),
        }];
        ticks.extend((0..20).map(|seq| Tick {
            seq,
            painted: true,
            phases: vec![0; PHASES.len()],
            cpu_us: Some(animation_us),
            input_step: "services".into(),
            paint_reasons: "animation".into(),
        }));
        ticks
    };
    let within = |peak_us, animation_us| {
        let rounds: Vec<Vec<Tick>> = (0..SWITCH_ROUNDS)
            .map(|_| round(peak_us, animation_us))
            .collect();
        let rounds: Vec<&[Tick]> = rounds.iter().map(Vec::as_slice).collect();
        median_peak(&rounds).cpu_ms().unwrap()
            <= SWITCH_BACK_PEAK_PER_ANIMATION_TICK * animation_tick_ms(&rounds)
    };
    assert!(within(12_000, 2_000));
    assert!(within(36_000, 6_000));
    assert!(!within(36_000, 2_000));
}

#[test]
fn switching_back_to_a_workspace_with_apps_stays_within_its_cpu_budget() {
    let trace_dir = tempfile::tempdir().expect("a directory for the frame trace");
    let trace = trace_dir.path().join("frame-trace.csv");
    let mut tree = notes_desktop();
    tree.append_empty_workspace("Elsewhere");
    let desktop = Desktop::start_with_env(&tree, &[("TILO_FRAME_TRACE", trace.as_os_str())]);
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
    // Startup work (font caches, the first save, the shell starting in the
    // Terminal) is not the switch's.
    std::thread::sleep(crate::idle_wakeups::SETTLE);
    wait_for_quiet_apps(&desktop, &trace);

    let state = desktop.control(&serde_json::json!({"op": "state_dump"}));
    let tree = state.get("state").unwrap_or(&state)["workspace_tree"].clone();
    let home = tree["active"].clone();
    let elsewhere = tree["workspaces"]
        .as_array()
        .and_then(|workspaces| {
            workspaces
                .iter()
                .map(|workspace| workspace["id"].clone())
                .find(|id| *id != home)
        })
        .unwrap_or_else(|| panic!("the desktop has no second workspace: {tree}"));

    // A first visit to the other workspace, untimed, as W16 runs after W10
    // made and visited it: it builds the empty workspace, and both apps here
    // took wakes while hidden whose frames (their own, 11–15 ms each in this
    // build) the first switch back then ran in its tick. Then five times there
    // and back, timed.
    switch(&desktop, &trace, "PageDown", &elsewhere);
    switch(&desktop, &trace, "PageUp", &home);
    wait_for_quiet_apps(&desktop, &trace);
    let mut rounds = Vec::new();
    for _ in 0..SWITCH_ROUNDS {
        let there = switch(&desktop, &trace, "PageDown", &elsewhere);
        let back = switch(&desktop, &trace, "PageUp", &home);
        rounds.push((there, back));
    }
    let report = (1..)
        .zip(&rounds)
        .map(|(round, (there, back))| {
            format!(
                "round {round}\n  to the empty workspace:\n{}\n  back:\n{}",
                table(there),
                table(back)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!("{report}");
    let backs: Vec<&[Tick]> = rounds.iter().map(|(_, back)| back.as_slice()).collect();
    let peak = median_peak(&backs);
    let cpu = peak.cpu_ms().expect("every observed tick has thread CPU");
    let animation = animation_tick_ms(&backs);
    let ratio = cpu / animation;
    assert!(
        rounds
            .iter()
            .all(|(_, back)| back.iter().any(|tick| tick.painted)),
        "a switch back painted no frame, so its cost was not observed:\n{report}"
    );
    // A second writer of the session's state file refuses every save the
    // compositor makes, and used to stall its loop for seconds besides.
    assert!(
        !desktop.logged("file lease is busy"),
        "the compositor under test did not own its state file's writer. Log:\n{}",
        desktop.log_tail()
    );
    // One machine-readable line for a scheduled lane.
    println!(
        "WORKSPACE-SWITCH-BACK tick_cpu_ms={cpu:.2} animation_tick_cpu_ms={animation:.2} ratio={ratio:.2}"
    );
    assert!(
        ratio <= SWITCH_BACK_PEAK_PER_ANIMATION_TICK,
        "switching back to the workspace with Notes and a Terminal spent a median peak of {cpu:.2} ms of the loop thread's CPU, {ratio:.1} times an animation tick of the same switches ({animation:.2} ms), over its limit of {SWITCH_BACK_PEAK_PER_ANIMATION_TICK} ({}):\n{report}",
        peak.describe()
    );
}
