//! What a workspace switch costs the compositor's frame loop when the
//! workspace switched back to has apps on it.
//!
//! The production compositor runs headless with a signed-in person's desktop:
//! a Notes notebook restored from the saved session and a Terminal opened
//! through the desktop's own action, both framework apps the compositor hosts,
//! and a second, empty workspace. After a first, untimed visit, Super+Page Down
//! goes to the empty workspace and Super+Page Up comes back, as
//! vm-acceptance W16 does once. The frame loop's own trace
//! (`TILO_FRAME_TRACE`) gives each tick's thread CPU and its phases, less
//! what the harness's own control requests took of it. Each switch-back's
//! sample is its costliest tick, over the whole switch; the gate
//! is the median of seven such samples, so a cost every switch back pays fails
//! and one tick a contended runner stretched does not. The relative check
//! compares the same median with the switches' ordinary animation ticks; it
//! diagnoses costly remounts across runners.
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
///
/// Held against the median switch back, not the single costliest one: on a
/// contended runner one switch back's costliest tick spreads from 14 to 62 ms
/// while the median of a run's switch backs stays at 18–31 ms (the fifteen
/// runs that failed the worst of five on 14 October 2026, 17 of 59 shell
/// jobs, with nothing changed; Super's release taken out). Those ticks are
/// CPU-bound, not waiting on the disk: all but one of the 48 over 30 ms spent
/// within 4 ms of their phases' wall time on CPU.
const SWITCH_BACK_TICK_CPU_BUDGET_MS: f64 = 33.4;

/// Maximum median switch-back peak relative to an ordinary animation tick
/// measured on the same runner. This supplements the absolute W16 budget.
const SWITCH_BACK_PEAK_PER_ANIMATION_TICK: f64 = 14.0;

/// Fixed independent observations, not retries: every round is measured and
/// reported, even if it exceeds the budget. The gate is their median, so a
/// cost paid by four of the seven switch backs fails it.
const SWITCH_ROUNDS: usize = 7;

/// The frame loop writes its trace a tick at least this long after the last
/// write, so a window is on disk once two ticks this far apart have run.
const TRACE_FLUSH: Duration = Duration::from_millis(1_050);

/// A switch's frame run is 400 ms; anything it brought in after that (an
/// app's next frame) lands well inside this.
const SWITCH_WINDOW: Duration = Duration::from_millis(1_000);

/// How long a switch is left alone before the harness asks the loop anything
/// again. Longer than [`SWITCH_WINDOW`], so the requests that flush the trace
/// afterwards (and whatever deferred app work their wake runs) start after the
/// measured window has closed and are never read as the switch's.
const SWITCH_SETTLE: Duration = Duration::from_millis(1_500);
const _: () = assert!(SWITCH_WINDOW.as_millis() + 250 <= SWITCH_SETTLE.as_millis());

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
    /// When the tick began, in microseconds on the loop's own clock.
    start_us: u64,
    painted: bool,
    /// Wall time of each of [`PHASES`], in microseconds.
    phases: Vec<u64>,
    /// The loop thread's CPU for the session's own work: the tick's, less
    /// what answering the harness's control requests took. A request lands
    /// in whichever tick is running when it arrives; on a slow runner the
    /// Super release that ends the chord landed in the switch's first frame
    /// and added its two relayouts (4 to 7 ms) to it. A person's keys come
    /// through the input phase, which counts. The tick that took the switch's
    /// key is charged that key too ([`switch_window`]).
    cpu_us: Option<u64>,
    /// What the control requests took of the tick, as the report shows it.
    control_cpu_us: Option<u64>,
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
        let control = self
            .control_cpu_us
            .filter(|us| *us >= 100)
            .map(|us| format!(" (control requests {:.2} ms more)", ms(us)))
            .unwrap_or_default();
        format!(
            "tick {}: cpu {} ms{control}; wall {phases} (input step {}; {})",
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
    let (seq, start, painted, cpu, control, step, reasons) = (
        at("seq"),
        at("start_us"),
        at("painted"),
        at("cpu_us"),
        at("testctl_cpu_us"),
        at("input_step"),
        at("paint_reasons"),
    );
    let phases: Vec<usize> = PHASES.iter().map(|(_, column)| at(column)).collect();
    lines
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            let field = |index: usize| fields.get(index).copied().unwrap_or_default();
            let number = |index: usize| field(index).parse::<u64>().ok();
            let control_cpu_us = number(control);
            Some(Tick {
                seq: number(seq)?,
                start_us: number(start)?,
                painted: field(painted) == "1",
                phases: phases
                    .iter()
                    .map(|index| number(*index).unwrap_or_default())
                    .collect(),
                cpu_us: number(cpu)
                    .zip(control_cpu_us)
                    .map(|(tick, control)| tick.saturating_sub(control)),
                control_cpu_us,
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
/// out the flush interval, wake it twice more. The wake is a modifier update
/// with every modifier up, which also releases the Super a switch held: a
/// state dump would wake the loop too, but costs it tens of milliseconds,
/// which would read as the switch's.
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
    // Hold Super and tap the key. Super stays held until `sync_trace` below
    // releases it, after the switch: every control request costs the loop a
    // relayout, and a release sent straight after the key landed in the
    // switch's first frame, 4-7 ms of the costliest tick of CI's failed runs
    // that a person letting go of a key never costs.
    modifiers(desktop, true);
    let reply = desktop.control(&serde_json::json!({"op": "key", "key": key}));
    assert!(reply.get("err").is_none(), "key {key} refused: {reply}");
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
    switch_window(ticks).unwrap_or_else(|ticks| {
        panic!(
            "Super+{key} produced no transition frame:\n{}",
            table(&ticks)
        )
    })
}

/// The ticks that belong to one switch, out of everything traced between the
/// harness's requests: the tick that took the key, then from its first
/// transition frame for [`SWITCH_WINDOW`].
///
/// The key arrives on the control socket, which the loop reads after it has
/// painted, so the tick that takes it swaps the workspace's content (the
/// relayout after the key) and only the next tick paints the transition. A
/// person's key arrives before the paint and does both in one tick. The tick
/// before the first transition frame is therefore the switch's, and its
/// control-request CPU with it, since that is the key and the content swap
/// it ran: without it, putting the kept content back (or building it afresh)
/// is never measured.
/// Ticks before that are the harness's own requests (the modifier, the trace
/// flush). Keep every tick that starts within the window, painted or not,
/// including service work while the switch settles. The harness's own
/// trace-flushing requests come [`SWITCH_SETTLE`] after the key, past the
/// window; the ticks that answer them (and the deferred app work their wake
/// runs) are the measurement's, not the switch's. Without a transition frame,
/// returns every tick.
fn switch_window(ticks: Vec<Tick>) -> Result<Vec<Tick>, Vec<Tick>> {
    let Some(first) = ticks.iter().position(|tick| {
        tick.painted
            && tick
                .paint_reasons
                .split('+')
                .any(|reason| reason == "transition")
    }) else {
        return Err(ticks);
    };
    let window_us = u64::try_from(SWITCH_WINDOW.as_micros()).unwrap_or(u64::MAX);
    let end_us = ticks[first].start_us.saturating_add(window_us);
    let mut window: Vec<Tick> = ticks
        .into_iter()
        .skip(first.saturating_sub(1))
        .take_while(|tick| tick.start_us < end_us)
        .collect();
    if first > 0
        && let Some(key) = window.first_mut()
    {
        let control = key.control_cpu_us.replace(0).unwrap_or_default();
        key.cpu_us = key.cpu_us.map(|cpu| cpu.saturating_add(control));
    }
    Ok(window)
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

/// Each switch-back's costliest tick, in round order. Reduce whole journeys,
/// not individual animation frames. Require CPU on every tick so an
/// incomplete trace cannot silently select a cheaper peak.
fn peaks<'a>(rounds: &[&'a [Tick]]) -> Vec<&'a Tick> {
    assert_eq!(rounds.len(), SWITCH_ROUNDS);
    rounds
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
        .collect()
}

/// The median of the switch-backs' costliest ticks: what the gate holds.
fn median_peak<'a>(rounds: &[&'a [Tick]]) -> &'a Tick {
    let mut peaks = peaks(rounds);
    peaks.sort_unstable_by_key(|tick| tick.cpu_us);
    peaks[SWITCH_ROUNDS / 2]
}

/// The costliest tick of every switch-back, for the report.
fn worst_tick<'a>(rounds: &[&'a [Tick]]) -> &'a Tick {
    peaks(rounds)
        .into_iter()
        .max_by_key(|tick| tick.cpu_us)
        .expect("a painted tick exists")
}

fn assert_switch_back_budget(rounds: &[&[Tick]], report: &str) {
    let median = median_peak(rounds);
    let cpu = median.cpu_ms().expect("every observed tick has thread CPU");
    assert!(
        cpu <= SWITCH_BACK_TICK_CPU_BUDGET_MS,
        "switching back to the workspace with Notes and a Terminal spent a median costliest tick of {cpu:.2} ms of the loop thread's CPU over {SWITCH_ROUNDS} switch-backs, over its budget of {SWITCH_BACK_TICK_CPU_BUDGET_MS} ms (median {}; worst {}):\n{report}",
        median.describe(),
        worst_tick(rounds).describe()
    );
}

fn sample(cpu_us: u64, reasons: &str) -> Tick {
    Tick {
        seq: 0,
        start_us: 0,
        painted: !reasons.is_empty(),
        phases: vec![0; PHASES.len()],
        cpu_us: Some(cpu_us),
        control_cpu_us: Some(0),
        input_step: "services".into(),
        paint_reasons: reasons.into(),
    }
}

/// The rounds of a synthetic run: each of `kinds` repeated its count of times.
fn rounds_of<'a>(kinds: &[(&'a [Tick], usize)]) -> Vec<&'a [Tick]> {
    kinds
        .iter()
        .flat_map(|(ticks, count)| std::iter::repeat_n(*ticks, *count))
        .collect()
}

/// A cost a minority of switch-backs pays (a tick a contended runner
/// stretched) is reported, not gated; one most of them pay fails.
#[test]
fn sampling_keeps_a_repeated_expensive_first_tick_but_not_one_outlier() {
    let ordinary = vec![
        sample(19_900, "session"),
        sample(2_000, "animation"),
        sample(2_000, "animation"),
    ];
    let remounted = vec![
        sample(90_000, "session"),
        sample(2_000, "animation"),
        sample(2_000, "animation"),
    ];
    let one = rounds_of(&[(&ordinary, 3), (&remounted, 1), (&ordinary, 3)]);
    assert_eq!(median_peak(&one).cpu_us, Some(19_900));
    assert_eq!(worst_tick(&one).cpu_us, Some(90_000));
    let minority = rounds_of(&[(&remounted, 3), (&ordinary, 4)]);
    assert_eq!(median_peak(&minority).cpu_us, Some(19_900));
    let rounds = rounds_of(&[(&remounted, 2), (&ordinary, 3), (&remounted, 2)]);
    assert_eq!(median_peak(&rounds).cpu_us, Some(90_000));
    assert!(
        median_peak(&rounds).cpu_ms().unwrap()
            > SWITCH_BACK_PEAK_PER_ANIMATION_TICK * animation_tick_ms(&rounds)
    );
}

/// The relative guard follows a runner slowdown; the absolute W16 budget
/// remains independently enforced on the median costliest tick.
#[test]
fn relative_gate_tracks_runner_speed_as_a_supplement() {
    let round = |peak_us, animation_us| {
        let mut ticks = vec![sample(peak_us, "session+revision+animation+transition")];
        ticks.extend((0..20).map(|_| sample(animation_us, "animation")));
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
#[should_panic(expected = "spent a median costliest tick of 36.00 ms")]
fn a_uniformly_slow_switch_fails_the_absolute_budget_even_if_relative_passes() {
    let round = vec![
        sample(36_000, "session+transition"),
        sample(6_000, "animation"),
    ];
    let rounds = rounds_of(&[(&round, SWITCH_ROUNDS)]);
    let ratio = median_peak(&rounds).cpu_ms().unwrap() / animation_tick_ms(&rounds);
    assert!(ratio <= SWITCH_BACK_PEAK_PER_ANIMATION_TICK);
    assert_switch_back_budget(&rounds, "uniformly slow frame");
}

#[test]
#[should_panic(expected = "spent a median costliest tick of 79.76 ms")]
fn a_painted_outlier_in_most_switch_backs_fails_the_budget() {
    let ordinary = vec![
        sample(19_900, "session"),
        sample(2_000, "animation"),
        sample(2_000, "animation"),
    ];
    // The painted transition recorded by CI run 103401: pump 25.76 ms,
    // scene 39.16 ms, loop-thread CPU 79.76 ms. Paid by four of the seven
    // switch-backs, it fails though three stay under budget.
    let mut observed = sample(79_760, "session+revision+animation+transition+apps");
    observed.phases[1] = 25_760;
    observed.phases[3] = 39_160;
    observed.input_step = "launches".into();
    let remounted = vec![observed, sample(2_000, "animation")];
    let rounds = rounds_of(&[(&ordinary, 2), (&remounted, 4), (&ordinary, 1)]);
    assert_switch_back_budget(&rounds, "a painted outlier in most switch-backs");
}

/// Seven switch-backs' costliest ticks as CI's contended runners recorded
/// them (failed runs 109918 and 110363, without the Super release that used
/// to land in the first frame): one reached 62.1 ms, the median 28.9 ms. The
/// worst-tick gate failed both runs; the median passes them. A 50 ms stall
/// in the switch path, which every switch back pays, fails it.
#[test]
fn a_contended_runner_passes_but_a_stall_every_switch_back_pays_fails() {
    let recorded: [u64; SWITCH_ROUNDS] = [22_100, 19_900, 23_200, 62_100, 28_900, 37_200, 45_300];
    let switches = |stall_us: u64| -> Vec<Vec<Tick>> {
        recorded
            .iter()
            .map(|peak_us| {
                vec![
                    sample(
                        peak_us + stall_us,
                        "session+revision+animation+transition+apps",
                    ),
                    sample(10_500, "animation"),
                    sample(10_500, "animation"),
                ]
            })
            .collect()
    };
    let contended = switches(0);
    let contended: Vec<&[Tick]> = contended.iter().map(Vec::as_slice).collect();
    assert_eq!(median_peak(&contended).cpu_us, Some(28_900));
    assert!(worst_tick(&contended).cpu_ms().unwrap() > SWITCH_BACK_TICK_CPU_BUDGET_MS);
    assert_switch_back_budget(&contended, "contended runner");
    let stalled = switches(50_000);
    let stalled: Vec<&[Tick]> = stalled.iter().map(Vec::as_slice).collect();
    let failure = std::panic::catch_unwind(|| assert_switch_back_budget(&stalled, "stalled"))
        .expect_err("a 50 ms stall in every switch back must fail the budget");
    let message = failure
        .downcast_ref::<String>()
        .expect("the budget's panic message");
    assert!(
        message.contains("median costliest tick of 78.90 ms"),
        "{message}"
    );
}

/// The harness's trace-flushing requests after a switch are not the switch:
/// CI run 108721 failed on such a tick (45.44 ms, pump 36.43 ms, control
/// 7.63 ms, not painted), 1.5 s after the key. Every tick of the switch itself
/// still counts, so a slow animation frame inside the window still fails.
#[test]
fn the_window_ends_before_the_harness_asks_again_but_keeps_the_whole_switch() {
    let tick = |seq, start_ms: u64, cpu_us, reasons: &str| Tick {
        seq,
        start_us: start_ms * 1_000,
        ..sample(cpu_us, reasons)
    };
    let traced = vec![
        // The modifier request before the key.
        tick(379, 0, 3_000, ""),
        // The key, and the workspace content it swapped in.
        tick(380, 2, 14_000, ""),
        tick(381, 5, 21_830, "session+animation+transition+apps"),
        tick(382, 300, 36_610, "animation"),
        tick(383, 410, 9_000, "animation"),
        // Service work while the switch settles, unpainted, still the switch's.
        tick(384, 900, 12_000, ""),
        // The harness's first trace-flushing request and the frame it causes.
        tick(403, 1_520, 45_440, ""),
        tick(404, 1_540, 24_440, "session"),
    ];
    let window = switch_window(traced).expect("a transition frame");
    assert_eq!(
        window.iter().map(|tick| tick.seq).collect::<Vec<_>>(),
        [380, 381, 382, 383, 384]
    );
    let rounds = [window.as_slice(); SWITCH_ROUNDS];
    let median = median_peak(&rounds);
    assert_eq!(median.cpu_us, Some(36_610));
    assert!(
        median.cpu_ms().unwrap() > SWITCH_BACK_TICK_CPU_BUDGET_MS,
        "a slow frame inside every switch must still be over the budget"
    );
    assert!(switch_window(vec![tick(1, 0, 1_000, "animation")]).is_err());
}

/// CI run 129512's worst tick, the first frame of a switch back: 34.77 ms
/// of thread CPU, 5.83 ms of it answering the harness's Super release, which
/// arrived while the frame ran. The switch is charged its own 28.94 ms.
#[test]
fn a_control_request_in_a_switch_frame_is_charged_to_the_harness() {
    let dir = tempfile::tempdir().expect("a directory for the trace");
    let trace = dir.path().join("frame-trace.csv");
    std::fs::write(
        &trace,
        "seq,start_us,input_us,pump_us,layout_us,scene_us,submit_us,testctl_us,clients_us,painted,cpu_us,input_step,paint_reasons,testctl_cpu_us\n\
         368,0,660,7650,2010,16920,0,5830,240,1,34770,services,session+revision+animation+transition+apps+peers,5830\n\
         369,9000,500,4000,1500,3000,0,0,100,1,9000,services,animation,0\n",
    )
    .expect("write the trace");
    let ticks = read_trace(&trace);
    assert_eq!(ticks[0].cpu_us, Some(28_940));
    assert_eq!(ticks[0].control_cpu_us, Some(5_830));
    assert_eq!(ticks[1].cpu_us, Some(9_000));
    assert!(
        ticks[0]
            .describe()
            .contains("control requests 5.83 ms more")
    );
    let window = switch_window(ticks).expect("a transition frame");
    assert_eq!(window[0].cpu_us, Some(28_940));
    let rounds = [window.as_slice(); SWITCH_ROUNDS];
    assert_switch_back_budget(&rounds, "the harness's request");
}

/// The tick that took the key is the switch's: a stall in swapping the
/// workspace's content lands there, before any transition frame is painted.
#[test]
fn the_tick_that_took_the_key_counts_as_the_switch() {
    let tick = |seq, start_ms: u64, cpu_us, reasons: &str| Tick {
        seq,
        start_us: start_ms * 1_000,
        ..sample(cpu_us, reasons)
    };
    // The trace charges the key's control dispatch, and the content swap it
    // ran, to the harness: 4 ms of the session's own work, 60 ms more.
    let mut key = tick(11, 3, 4_000, "");
    key.control_cpu_us = Some(60_000);
    let window = switch_window(vec![
        tick(10, 0, 2_000, ""),
        key,
        tick(12, 20, 18_000, "session+revision+animation+transition+apps"),
        tick(13, 36, 9_000, "animation"),
    ])
    .expect("a transition frame");
    assert_eq!(
        window.iter().map(|tick| tick.seq).collect::<Vec<_>>(),
        [11, 12, 13]
    );
    let rounds = [window.as_slice(); SWITCH_ROUNDS];
    assert_eq!(median_peak(&rounds).cpu_us, Some(64_000));
    let first = switch_window(vec![tick(1, 0, 18_000, "transition")]).expect("a transition frame");
    assert_eq!(first.len(), 1);
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
    // apps settle before seven complete, timed switches there and back.
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
    // Both checks hold the median of the same complete switch-back
    // observations: the absolute one W16's two refresh intervals, the
    // relative one main's runner-normalized remount signal. The worst tick
    // is reported beside them.
    let median = median_peak(&backs);
    let worst_cpu = worst_tick(&backs)
        .cpu_ms()
        .expect("every observed tick has thread CPU");
    let median_cpu = median.cpu_ms().expect("every observed tick has thread CPU");
    let animation = animation_tick_ms(&backs);
    let ratio = median_cpu / animation;
    println!(
        "WORKSPACE-SWITCH-BACK median_peak_cpu_ms={median_cpu:.2} worst_tick_cpu_ms={worst_cpu:.2} animation_tick_cpu_ms={animation:.2} ratio={ratio:.2}"
    );
    assert_switch_back_budget(&backs, &report);
    assert!(
        ratio <= SWITCH_BACK_PEAK_PER_ANIMATION_TICK,
        "switching back to the workspace with Notes and a Terminal spent a median peak of {median_cpu:.2} ms of the loop thread's CPU, {ratio:.1} times an animation tick of the same switches ({animation:.2} ms), over its limit of {SWITCH_BACK_PEAK_PER_ANIMATION_TICK} ({}):\n{report}",
        median.describe()
    );
}
