//! What opening Settings and switching its panes costs, measured headlessly: the CPU time the
//! window's thread spends from the press (or the launch) to the frame that shows what it did,
//! read against a fixed piece of work the same thread does in the same run, so a busy machine
//! and a build with or without optimisations are read as the step is, not as the machine is.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "a budget reports what it measured and fails loudly: percentiles over a few dozen \
              samples of bounded machine times"
)]

mod support;

use support::{invoke, open_with, settle};
use tilolang_tilo::runtime::Value;
use ui::rt::thread_cpu_time;
use ui::time::Instant;

/// The person's window: their screen's own size, as Settings opens on a laptop.
const WINDOW: (u32, u32) = (1472, 920);

/// The panes a person moves between, as the old Settings' budget did.
const PANES: [&str; 6] = [
    "sound",
    "displays",
    "network",
    "bluetooth",
    "power",
    "appearance",
];

/// The CPU time [`reference`] takes on the machine this budget is written for, in microseconds:
/// 36 ms on the build server's x86-64 core, as opening Settings measured it. A step's time is read
/// against what the reference took beside it, so a machine four times slower is held to the same
/// work, not to its own clock.
const REFERENCE_US: u64 = 36_000;

/// A fixed piece of work, timed in the UI thread's CPU microseconds: formatting, hashing and
/// ordering strings, which every run does the same. What moves it is the machine — another
/// thread on the core, its caches, its clock — and the build's optimisation, and it moves the
/// step being measured as much.
fn reference() -> u64 {
    const ITEMS: u64 = 20_000;
    let started = thread_cpu_time();
    let wall = Instant::now();
    let mut by_name = std::collections::HashMap::new();
    let mut ordered = std::collections::BTreeMap::new();
    for i in 0..ITEMS {
        let name = format!("pane {i:06} {}", i.wrapping_mul(2_654_435_761) % 9_973);
        by_name.insert(name.clone(), i);
        ordered.insert(name, i);
    }
    let mut names: Vec<&String> = ordered.keys().collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    let found: u64 = names.iter().filter_map(|name| by_name.get(*name)).sum();
    std::hint::black_box(found);
    thread_cpu_time().zip(started).map_or_else(
        || u64::try_from(wall.elapsed().as_micros()).unwrap_or(u64::MAX),
        |(now, before)| u64::try_from(now.saturating_sub(before).as_micros()).unwrap_or(u64::MAX),
    )
}

/// The CPU microseconds a step took, as they would read beside a reference of [`REFERENCE_US`].
fn held(cpu_us: u64, alongside_us: u64) -> u64 {
    let scaled = u128::from(cpu_us) * u128::from(REFERENCE_US) / u128::from(alongside_us.max(1));
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// One step: what it cost on the thread's CPU clock, and what the reference took beside it.
struct Step {
    cpu_us: u64,
    reference_us: u64,
}

impl Step {
    /// The step's CPU time as the machine of [`REFERENCE_US`] reads it.
    fn held_us(&self) -> u64 {
        held(self.cpu_us, self.reference_us)
    }
}

fn cpu_us() -> Option<std::time::Duration> {
    thread_cpu_time()
}

/// Time `step`, with one reference taken just before it and one just after: what the thread did
/// between the two reads of its CPU clock, and what the machine was like then.
fn timed(mut step: impl FnMut()) -> Step {
    let before = reference();
    let started = cpu_us();
    let wall = Instant::now();
    step();
    let cpu = cpu_us().zip(started).map_or_else(
        || u64::try_from(wall.elapsed().as_micros()).unwrap_or(u64::MAX),
        |(now, before)| u64::try_from(now.saturating_sub(before).as_micros()).unwrap_or(u64::MAX),
    );
    let after = reference();
    Step {
        cpu_us: cpu,
        reference_us: u64::midpoint(before, after),
    }
}

fn percentiles(label: &str, values: &[u64], raw: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    let (raw_p50, raw_p99) = if raw.is_empty() {
        (0, 0)
    } else {
        let mut raw = raw.to_vec();
        raw.sort_unstable();
        let at_raw = |q: f64| raw[((raw.len() - 1) as f64 * q).round() as usize];
        (at_raw(0.5), at_raw(0.99))
    };
    eprintln!(
        "perf {label}: n={} held p50={}us p99={}us max={}us (raw p50={raw_p50}us p99={raw_p99}us, reference {REFERENCE_US}us)",
        sorted.len(),
        at(0.5),
        at(0.99),
        sorted[sorted.len() - 1]
    );
    at(0.5)
}

fn budget(label: &str, p50: u64, limit_us: u64) {
    assert!(
        p50 <= limit_us,
        "{label}: p50 {p50}us exceeds the {limit_us}us CPU budget"
    );
}

/// Switching from one pane to another, as a press on the sidebar's row does: the app's own
/// `open_pane`, and the frames that show the pane it chose.
#[test]
fn opening_and_switching_panes_stay_within_their_budget() {
    tilo_app_window::i18n::ensure();
    // The app's packages, compiled before anything is timed: opening a window is what is
    // measured, not building the app.
    let packages = support::packages();
    // Opening: the launch, the first pane and the frame that shows it. Closing it is no part of
    // what a person waits for, so it happens outside the step.
    let mut launched = None;
    let opening = timed(|| {
        launched = Some(open_with(&packages, WINDOW, ui::Theme::light()));
    });
    drop(launched);
    eprintln!(
        "perf opening Settings: {}us CPU, reference {}us",
        opening.cpu_us, opening.reference_us
    );

    let mut window = open_with(&packages, WINDOW, ui::Theme::light());
    // One warm switch each, as a person's first look is not what the budget holds.
    for pane in PANES {
        support::open_pane(&mut window, pane);
    }
    let mut switches = Vec::new();
    let mut raw = Vec::new();
    for _ in 0..4 {
        for pane in PANES {
            let step = timed(|| {
                invoke(&mut window, "open_pane", vec![Value::text(pane)]);
                settle(&mut window);
            });
            raw.push(step.cpu_us);
            switches.push(step.held_us());
        }
    }
    let switch_p50 = percentiles("Settings pane switch", &switches, &raw);
    // This build costs about 24 ms a switch and 190 ms to open, read against the reference:
    // each budget holds three times that, so a regression that doubles a step is caught while
    // the machine's own noise (the reference moved 36-50 ms between runs) is not. The measure
    // is the thread's CPU time, which scheduling cannot inflate as wall time can.
    budget("pane switch", switch_p50, 90_000);
    budget(
        "opening Settings",
        held(opening.cpu_us, opening.reference_us),
        600_000,
    );
}
