//! What a workspace switch costs the compositor's frame loop when the
//! workspace switched back to has apps on it.
//!
//! The production compositor runs headless with a signed-in person's desktop:
//! a Notes notebook restored from the saved session and a Terminal opened
//! through the desktop's own action, both framework apps the compositor hosts,
//! and a second, empty workspace. After a first, untimed visit, Super+Page Down
//! goes to the empty workspace and Super+Page Up comes back, twice, as
//! vm-acceptance W16 does once. The frame loop's own trace
//! (`TILO_FRAME_TRACE`) gives each tick's thread CPU and its phases; the gate
//! is the costliest tick of either timed switch back.
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
    // made and visited it: it builds the empty workspace, and both apps here
    // took wakes while hidden whose frames (their own, 11–15 ms each in this
    // build) the first switch back then ran in its tick. Then twice there and
    // back, timed.
    switch(&desktop, &trace, "PageDown", &elsewhere);
    switch(&desktop, &trace, "PageUp", &home);
    wait_for_quiet_apps(&desktop, &trace);
    let mut rounds = Vec::new();
    for _ in 0..2 {
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
    let worst = rounds
        .iter()
        .flat_map(|(_, back)| back)
        .max_by_key(|tick| tick.cpu_us.unwrap_or_default())
        .unwrap_or_else(|| {
            panic!(
                "the trace holds no tick of a switch back, so its cost was not observed:\n{report}"
            )
        });
    let cpu = worst.cpu_ms().unwrap_or_else(|| {
        panic!("the frame trace carries no per-tick CPU (cpu_us), so the switch back's cost was not observed:\n{report}")
    });
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
    println!("WORKSPACE-SWITCH-BACK tick_cpu_ms={cpu:.2}");
    assert!(
        cpu <= SWITCH_BACK_TICK_CPU_BUDGET_MS,
        "switching back to the workspace with Notes and a Terminal spent {cpu:.2} ms of the loop thread's CPU in one tick, over its budget of {SWITCH_BACK_TICK_CPU_BUDGET_MS} ms ({}):\n{report}",
        worst.describe()
    );
}
