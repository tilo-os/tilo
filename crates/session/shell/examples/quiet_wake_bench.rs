//! What one quiet compositor wake costs the shell's model pump.
//!
//! Every wake of the native loop (a heartbeat, a timer, a protocol message
//! that changes nothing) runs `FrameBuilder::pump_apps`: the host's settle,
//! sync and update passes. On a desktop where nothing changed that work
//! produces nothing, so its cost is pure idle overhead, multiplied by every
//! periodic wake that remains. This drives the populated desktop fixture to
//! rest, then pumps it `TILO_QUIET_WAKE_SAMPLES` (default 2,000) more times
//! with nothing to do and reports the loop thread's CPU per pump, and how
//! many tree writes those pumps announced (each one sends every
//! generation-gated phase of the loop back over the tree); any is an error.
//! A pump that reports a change (a status poll that came back) is counted,
//! not refused: what it repaints is real work, not idle overhead.
//!
//! ```text
//! cargo run --locked -p shell --release --example quiet_wake_bench
//! ```

#[path = "support/populated_shell_fixture.rs"]
mod populated_shell_fixture;

use std::time::{Duration, Instant};

use automerge::{AutoCommit, ROOT};
use populated_shell_fixture::PopulatedShellFixture;

const SAMPLES_ENV: &str = "TILO_QUIET_WAKE_SAMPLES";
const DEFAULT_SAMPLES: usize = 2_000;
/// Pumps a fresh desktop needs before it stops reporting changes: first
/// sync, layout and the one-shot harvesters.
const SETTLE_LIMIT: usize = 200;

/// CPU time this thread has used.
fn thread_cpu() -> Result<Duration, String> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec for the duration of the call.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
    if status != 0 {
        return Err("the thread CPU clock is unreadable".into());
    }
    Ok(Duration::new(
        u64::try_from(now.tv_sec).unwrap_or_default(),
        u32::try_from(now.tv_nsec).unwrap_or_default(),
    ))
}

/// The sample at `per_mille` of the way through `sorted`.
fn percentile(sorted: &[Duration], per_mille: usize) -> Duration {
    let index = sorted
        .len()
        .saturating_sub(1)
        .saturating_mul(per_mille)
        .checked_div(1_000)
        .unwrap_or_default();
    sorted.get(index).copied().unwrap_or_default()
}

fn main() -> Result<(), String> {
    let samples = std::env::var(SAMPLES_ENV)
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|error| format!("{SAMPLES_ENV}: {error}"))?
        .unwrap_or(DEFAULT_SAMPLES);
    // The top bar's minute would be a real change between two pumps.
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = PopulatedShellFixture::new((1_920, 1_080), ui::theme::Theme::dark())?;
    // Every presented tab opens its own document, as on a real desktop; a
    // tab without one would sit in its failure state instead of hosting.
    let (records, tabs) = {
        let chrome = frame.chrome();
        let tabs = chrome
            .tree()
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect::<Vec<_>>();
        (chrome.records().clone(), tabs)
    };
    for tab in tabs {
        shell::shell::tab_documents::create_scratch_tab_record(&records, tab)
            .map_err(|error| format!("tab {tab} document: {error:?}"))?;
    }
    let mut document = AutoCommit::new();
    let _ = frame.paint_with_damage();
    let mut settled_after = None;
    for round in 0..SETTLE_LIMIT {
        if !frame.pump_apps(&mut document, &ROOT) {
            settled_after = Some(round);
            break;
        }
        let _ = frame.paint_with_damage();
    }
    let settled_after = settled_after
        .ok_or_else(|| format!("the desktop still changed after {SETTLE_LIMIT} pumps"))?;
    let revision = frame.chrome_revision();
    let tree = frame.chrome().tree_handle();
    let generation = tree.generation();
    let mut cpu = Vec::with_capacity(samples);
    let mut changed = 0_usize;
    let wall_started = Instant::now();
    for _ in 0..samples {
        let started = thread_cpu()?;
        changed = changed.saturating_add(usize::from(frame.pump_apps(&mut document, &ROOT)));
        cpu.push(thread_cpu()?.saturating_sub(started));
    }
    let wall = wall_started.elapsed();
    cpu.sort_unstable();
    let total: Duration = cpu.iter().sum();
    let mean = total
        .checked_div(u32::try_from(samples.max(1)).unwrap_or(u32::MAX))
        .unwrap_or_default();
    let tree_writes = tree.generation().wrapping_sub(generation);
    let revision_moved = frame.chrome_revision() != revision;
    drop(frame);
    drop(clock);
    println!(
        "quiet wake: samples={samples} settled_after={settled_after} changed={changed} revision_moved={} \
         tree_writes={tree_writes} cpu_mean={mean:?} p50={:?} p99={:?} max={:?} wall={wall:?}",
        revision_moved,
        percentile(&cpu, 500),
        percentile(&cpu, 990),
        cpu.last().copied().unwrap_or_default(),
    );
    // Every generation-gated phase of the compositor loop walks the tree
    // again after an announced write; a quiet wake must announce none.
    if tree_writes > 0 {
        return Err(format!(
            "{samples} quiet pumps announced {tree_writes} tree writes"
        ));
    }
    Ok(())
}
