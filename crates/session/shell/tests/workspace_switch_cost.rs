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
//! is the worst tick across five complete switch-backs. Each sample includes
//! the entire switch, so one expensive painted transition fails the budget.
//! The relative median-peak check also compares the same switches with their
//! ordinary animation ticks; it diagnoses costly remounts across runners.
//!
//! Coming back used to build the workspace again from nothing: both apps were
//! mounted afresh (their records opened, their first sync and update) and every
//! widget drew for the first time (W16 on the VM: one 84 ms tick, pump 49 ms
//! and scene 70 ms). The desktop now keeps a workspace's content while another
//! one is shown and puts it back on the way back.

use std::path::Path;
use std::time::Duration;

use crate::idle_wakeups::{Desktop, notes_desktop};

/// Most thread CPU one tick of a switch back may take, in milliseconds: two
/// refresh intervals at 60 Hz, W16's `frame_cpu_ms_max`, held here in a debug
/// build.
///
/// Measured on CI's shared x86-64 runner (headless, llvmpipe, debug build).
/// Rebuilding the workspace, as every switch back did before its content was
/// kept: 90–108 ms in each switch back (scene 78–97 ms). Putting the kept
/// content back: the costliest tick of four timed switch backs 19.5–19.9 ms.
const SWITCH_BACK_TICK_CPU_BUDGET_MS: f64 = 33.4;

/// Maximum median switch-back peak relative to an ordinary animation tick
/// measured on the same runner. This supplements the absolute W16 budget.
const SWITCH_BACK_PEAK_PER_ANIMATION_TICK: f64 = 14.0;

/// Fixed independent observations, not retries: every round is measured and
/// reported, even if it exceeds the budget. W16 limits the worst loop-thread
/// tick, so one over-budget observation must fail here too.
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
    let ticks: Vec<Tick> = read_trace(trace)
        .into_iter()
        .filter(|tick| mark.is_none_or(|mark| tick.seq > mark))
        .collect();
    assert_eq!(
        active_workspace(desktop),
        *to,
        "Super+{key} did not switch to workspace {to}. Log:\n{}",
        desktop.log_tail()
    );
    // The preceding modifier and key requests are synthetic control-socket
    // setup. Begin at the first compositor transition frame, which includes
    // the switch's structural update and first app paint. Keep every tick
    // after that frame, including non-painted service work during settling.
    let first = ticks
        .iter()
        .position(|tick| {
            tick.painted
                && tick
                    .paint_reasons
                    .split('+')
                    .any(|reason| reason == "transition")
        })
        .unwrap_or_else(|| {
            panic!(
                "Super+{key} produced no transition frame:\n{}",
                table(&ticks)
            )
        });
    ticks.into_iter().skip(first).collect()
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

/// Require CPU on every tick so an incomplete trace cannot silently select a
/// cheaper peak. The maximum spans every tick of every complete switch-back.
fn worst_tick<'a>(rounds: &[&'a [Tick]]) -> &'a Tick {
    assert_eq!(rounds.len(), SWITCH_ROUNDS);
    for ticks in rounds {
        assert!(
            ticks.iter().any(|tick| tick.painted),
            "no switch frame observed"
        );
        assert!(
            ticks.iter().all(|tick| tick.cpu_us.is_some()),
            "thread CPU missing from the switch trace"
        );
    }
    rounds
        .iter()
        .flat_map(|ticks| ticks.iter())
        .max_by_key(|tick| tick.cpu_us)
        .expect("a painted tick exists")
}

fn assert_switch_back_budget(peak: &Tick, report: &str) {
    let cpu = peak.cpu_ms().expect("every observed tick has thread CPU");
    assert!(
        cpu <= SWITCH_BACK_TICK_CPU_BUDGET_MS,
        "switching back to the workspace with Notes and a Terminal spent a worst tick of {cpu:.2} ms of the loop thread's CPU, over its budget of {SWITCH_BACK_TICK_CPU_BUDGET_MS} ms ({}):\n{report}",
        peak.describe()
    );
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

/// The relative guard follows a runner slowdown; the absolute W16 budget
/// remains independently enforced on the worst tick.
#[test]
fn relative_gate_tracks_runner_speed_as_a_supplement() {
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
#[should_panic(expected = "spent a worst tick of 36.00 ms")]
fn a_uniformly_slow_switch_fails_the_absolute_budget_even_if_relative_passes() {
    let round = vec![
        Tick {
            seq: 0,
            painted: true,
            phases: vec![0; PHASES.len()],
            cpu_us: Some(36_000),
            input_step: "services".into(),
            paint_reasons: "session+transition".into(),
        },
        Tick {
            seq: 1,
            painted: true,
            phases: vec![0; PHASES.len()],
            cpu_us: Some(6_000),
            input_step: "services".into(),
            paint_reasons: "animation".into(),
        },
    ];
    let rounds = [&round, &round, &round, &round, &round].map(Vec::as_slice);
    let ratio = median_peak(&rounds).cpu_ms().unwrap() / animation_tick_ms(&rounds);
    assert!(ratio <= SWITCH_BACK_PEAK_PER_ANIMATION_TICK);
    assert_switch_back_budget(worst_tick(&rounds), "uniformly slow frame");
}

#[test]
#[should_panic(expected = "spent a worst tick of 79.76 ms")]
fn one_painted_outlier_fails_the_switch_back_budget() {
    let make = |cpu_us| Tick {
        seq: 0,
        painted: true,
        phases: vec![0; PHASES.len()],
        cpu_us: Some(cpu_us),
        input_step: "services".into(),
        paint_reasons: "session".into(),
    };
    let ordinary = vec![make(19_900), make(2_000), make(2_000)];
    // The painted transition recorded by CI run 103401: pump 25.76 ms,
    // scene 39.16 ms, loop-thread CPU 79.76 ms. It must fail even when four
    // other complete switch-backs stay under budget.
    let mut observed = make(79_760);
    observed.phases[1] = 25_760;
    observed.phases[3] = 39_160;
    observed.input_step = "launches".into();
    observed.paint_reasons = "session+revision+animation+transition+apps".into();
    let remounted = vec![observed, make(2_000), make(2_000)];
    let peak = worst_tick(&[&ordinary, &ordinary, &remounted, &ordinary, &ordinary]);
    assert_switch_back_budget(peak, "one painted outlier");
}

#[test]
fn switching_back_to_a_workspace_with_apps_stays_within_its_cpu_budget() {
    let trace_dir = tempfile::tempdir().expect("a directory for the frame trace");
    let trace = trace_dir.path().join("frame-trace.csv");
    let mut tree = notes_desktop();
    tree.append_empty_workspace("Elsewhere");
    let desktop = Desktop::start_with_env(tree, &[("TILO_FRAME_TRACE", trace.as_os_str())]);
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
    // made and visited it. It builds the empty workspace and lets the hosted
    // apps settle before five complete, timed switches there and back.
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
    let peak = worst_tick(&backs);
    // Both checks use the same complete switch-back observations. The worst
    // tick is the VM W16 contract; the median ratio retains main's runner-
    // normalized remount signal without masking a single slow frame.
    let median = median_peak(&backs);
    let cpu = peak.cpu_ms().expect("every observed tick has thread CPU");
    let median_cpu = median.cpu_ms().expect("every observed tick has thread CPU");
    let animation = animation_tick_ms(&backs);
    let ratio = median_cpu / animation;
    println!(
        "WORKSPACE-SWITCH-BACK tick_cpu_ms={cpu:.2} median_peak_cpu_ms={median_cpu:.2} animation_tick_cpu_ms={animation:.2} ratio={ratio:.2}"
    );
    assert_switch_back_budget(peak, &report);
    assert!(
        ratio <= SWITCH_BACK_PEAK_PER_ANIMATION_TICK,
        "switching back to the workspace with Notes and a Terminal spent a median peak of {median_cpu:.2} ms of the loop thread's CPU, {ratio:.1} times an animation tick of the same switches ({animation:.2} ms), over its limit of {SWITCH_BACK_PEAK_PER_ANIMATION_TICK} ({}):\n{report}",
        median.describe()
    );
}
