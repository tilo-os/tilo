#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Headless performance-scenario runner — the CLI face of
//! [`tilo_ui::profiling`].
//!
//! Drives a `ui` app through a scripted interaction with **no GPU and no
//! window**, timing each of the five frame passes (`sync`, `build_layout`,
//! `apply_layout`, `draw`, `handle_event`) and printing frame-time statistics
//! (mean / p50 / p95 / max per pass, plus how many frames blew the 60 fps
//! budget). These are headless CPU diagnostics, not displayed frame rates.
//!
//! Two targets are run to show the harness works against different app shapes:
//!
//!   * `list-scroll`   — a tall scrollable list, scrolled N verified moving steps. The
//!     canonical scroll scenario: it surfaces per-frame
//!     layout + draw cost.
//!   * `gallery-steady`— the reference component gallery (`common/gallery_scene`),
//!     re-rendered N frames with no input: the steady-state
//!     redraw cost of a dense screen.
//!
//! Run:
//! ```text
//! cargo run -p ui-examples --release --example ui-perf -- perf_scenario
//! cargo run -p ui-examples --release --example ui-perf -- perf_scenario --count 240 --json
//! ```
//!
//! Any app author gets the same measurement over their own root with one call:
//! `tilo_ui::profiling::run_scenario_on(state, app_root(), &scenario)`.

use std::cell::Cell;
use std::rc::Rc;

use tilo_ui::headless::Driver;
use tilo_ui::profiling::{FRAME_BUDGET_US, PerfReport, PerfScenario, PerfStep, run_scenario_on};
use tilo_ui::state::State;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Column, Scrollable, Text, Widget};
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, WindowEvent};

use super::gallery_scene;

/// A tall scrollable list — a realistic "feed / notes" app shape, and a
/// reliable scroll target (its content overflows the viewport).
fn list_app(rows: usize, offset: Rc<Cell<(f32, f32)>>) -> (State, Box<dyn Widget>) {
    let state = State::new();
    let mut col = Column::new().with_intrinsic_height();
    for i in 0..rows {
        col = col.push(Box::new(Text::new(&format!(
            "Row {i:>3} — the quick brown fox jumps over the lazy dog, then does it again"
        ))));
    }
    let root = Scrollable::new_vertical()
        .with_persisted_offset(offset)
        .with_child(Box::new(col));
    (state, Box::new(root))
}

fn verified_scroll_step(
    driver: &mut Driver,
    offset: &Cell<(f32, f32)>,
    dy: f64,
    step: usize,
) -> Result<(), String> {
    let before = offset.get().1;
    driver.scroll(0.0, dy);
    let after = offset.get().1;
    if !after.is_finite()
        || (dy < 0.0 && after <= before)
        || (dy > 0.0 && after >= before)
        || dy == 0.0
    {
        return Err(format!(
            "list-scroll step {step}: wheel dy={dy} did not move content in the expected direction ({before} -> {after})"
        ));
    }
    Ok(())
}

fn verified_list_scroll(count: usize) -> Result<PerfReport, String> {
    if count == 0 {
        return Err("list-scroll requires --count greater than zero".to_string());
    }
    let offset = Rc::new(Cell::new((0.0, 256.0)));
    let (state, root) = list_app(200, Rc::clone(&offset));
    let mut scenario = PerfScenario::scroll("list-scroll-headless-active-cpu", count);
    // Eight notches travel well within this 200-row list; reverse before
    // either edge. Every input still has to pass the observed-offset check.
    scenario.steps = (0..count)
        .map(|step| PerfStep::Scroll {
            dy: if (step / 8) % 2 == 0 { -3.0 } else { 3.0 },
            count: 1,
        })
        .collect();
    let mut driver = Driver::new(state, root, scenario.viewport);
    driver.send(&WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(
            f64::from(scenario.viewport.0) / 2.0,
            f64::from(scenario.viewport.1) / 2.0,
        ),
    });
    driver.begin_profiling();
    for _ in 0..scenario.warmup {
        driver.profile_frame();
    }
    driver.clear_profile_samples();
    for (step, input) in scenario.steps.iter().enumerate() {
        if let PerfStep::Scroll { dy, .. } = input {
            verified_scroll_step(&mut driver, &offset, *dy, step)?;
        }
    }
    let samples = driver.take_profile_samples();
    if samples.len() != count {
        return Err(format!(
            "list-scroll expected {count} moving frames, captured {}",
            samples.len()
        ));
    }
    Ok(PerfReport::from_samples(
        &scenario.name,
        samples,
        FRAME_BUDGET_US,
    ))
}

#[expect(
    clippy::panic,
    reason = "invalid benchmark workloads must fail without a timing report"
)]
pub fn run(args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let count = arg_value(args, "--count")
        .and_then(|v| v.parse().ok())
        .unwrap_or(120usize);
    let json = args.iter().any(|a| a == "--json");

    // 1) Scroll scenario against the scrollable list.
    let report = match verified_list_scroll(count) {
        Ok(report) => report,
        Err(error) => {
            panic!("invalid headless CPU workload: {error}");
        }
    };
    if json {
        // Keep every captured sample so percentile claims can be recomputed.
        // Offset movement proves input effect, not rendered pixels or presentation.
        let mut summary = serde_json::to_value(&report).expect("serialize CPU report");
        summary["samples"] = serde_json::to_value(&report.samples).expect("serialize raw frames");
        summary["movement_validation"] = serde_json::json!({
            "method": "persisted_scroll_offset_direction_per_step",
            "verified_steps": count,
            "rows": 200,
            "viewport": tilo_ui::headless::DEFAULT_VIEWPORT,
            "display_validated": false,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).expect("serialize verified report")
        );
    } else {
        print!("{report}");
    }

    // 2) Steady-state redraw against the reference component gallery.
    let mut g_state = State::new();
    let g_root = gallery_scene::build_showcase(&mut g_state, &Theme::dark());
    let gallery = PerfScenario {
        name: "gallery-steady-headless-idle-cpu".to_string(),
        viewport: (1280, 800),
        warmup: 3,
        steps: vec![PerfStep::Frames(count)],
    };
    let g_report = run_scenario_on(g_state, g_root, &gallery);
    if json {
        println!("{}", g_report.to_json());
    } else {
        print!("{g_report}");
    }
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().map(String::as_str);
        }
        if let Some(rest) = a.strip_prefix(&format!("{flag}=")) {
            return Some(rest);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_workloads_must_move_and_measure_each_step() {
        // phase: empty_scroll_workload_is_rejected
        {
            assert!(verified_list_scroll(0).is_err());
        }
        // phase: outside_pointer_scroll_cannot_return_a_successful_measurement
        {
            tilo_ui::i18n::I18nManager::init("en-US");
            let offset = Rc::new(Cell::new((0.0, 256.0)));
            let (state, root) = list_app(200, Rc::clone(&offset));
            let mut driver = Driver::new(state, root, tilo_ui::headless::DEFAULT_VIEWPORT);
            driver.send(&WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(-100.0, -100.0),
            });
            assert!(verified_scroll_step(&mut driver, &offset, -3.0, 0).is_err());
        }
        // phase: repeated_direction_reversals_produce_one_moving_frame_per_step
        {
            tilo_ui::i18n::I18nManager::init("en-US");
            let report = verified_list_scroll(40).expect("verified scrolling");
            assert_eq!(report.frames, 40);
        }
    }
}
