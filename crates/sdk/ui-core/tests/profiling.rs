//! The perf harness runs a scripted scenario against an app's **real** five
//! passes and produces per-pass frame-time statistics — the production path,
//! not a mock. These tests drive [`tilo_ui::profiling::run_scenario_on`], which
//! stands up the same headless [`tilo_ui::headless::FrameHarness`] the persistence
//! harness uses.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::profiling::{FRAME_BUDGET_US, PerfScenario, PerfStep, run_scenario_on};
use tilo_ui::state::State;
use tilo_ui::widget::{Column, Scrollable, Text, Widget};

/// A tall scrollable list: content overflows the viewport, so wheel scrolls
/// actually move it, and each frame rebuilds + draws real widgets.
fn list_app(rows: usize) -> (State, Box<dyn Widget>) {
    let state = State::new();
    let mut col = Column::new().with_intrinsic_height();
    for i in 0..rows {
        col = col.push(Box::new(Text::new(&format!(
            "Row {i} — the quick brown fox jumps over the lazy dog"
        ))));
    }
    let root = Scrollable::new_vertical().with_child(Box::new(col));
    (state, Box::new(root))
}

#[test]
fn profiling_collects_per_pass_stats_and_reports_them() {
    // Case `scroll_scenario_produces_per_pass_stats`.
    // A scroll scenario collects one per-pass timing per frame and aggregates
    // them into a stats report on the real FrameHarness path.
    {
        let count = 40;
        let (state, root) = list_app(150);
        let report = run_scenario_on(state, root, &PerfScenario::scroll("test-scroll", count));

        // One measured frame per scroll notch — warmup was discarded.
        assert_eq!(report.frames, count, "one frame per scroll notch");
        assert_eq!(report.samples.len(), count);

        for (name, st) in [
            ("sync", report.sync),
            ("build_layout", report.build_layout),
            ("apply_layout", report.apply_layout),
            ("draw", report.draw),
            ("handle_event", report.handle_event),
            ("total", report.total),
        ] {
            assert_eq!(st.count, count, "{name} has one measurement per frame");
        }

        // Real, non-zero time: drawing 150 rows each frame on the production
        // path costs something.
        assert!(report.total.max_us > 0, "total frame time was measured");
        assert!(
            report.draw.max_us > 0,
            "draw pass was measured: {:?}",
            report.draw
        );
        // build_layout is deliberately not required to cost anything: a scroll
        // keeps the retained layout tree and re-measures no widget, which can
        // finish under the microsecond resolution. Its per-frame count is
        // checked above.

        assert!(report.total.p50_us <= report.total.p95_us);
        assert!(report.total.p95_us <= report.total.max_us);
        assert!(report.total.min_us <= report.total.p50_us);
        assert!(report.over_budget <= report.frames);
        assert_eq!(report.budget_us, FRAME_BUDGET_US);

        // Frame indices are monotonic and continue past the discarded warmup.
        for w in report.samples.windows(2) {
            assert!(w[1].frame > w[0].frame, "frame indices increase");
        }
        assert!(
            report.samples[0].frame >= 3,
            "warmup frames were counted then cleared"
        );
    }

    // Case `steady_state_frames_have_zero_handle_event`.
    // A steady-state scenario dispatches no input, so `handle_event` is zero for
    // every frame while the render passes are still measured — proving the passes
    // are attributed independently, not lumped together.
    {
        let (state, root) = list_app(80);
        let scenario = PerfScenario {
            name: "steady".to_string(),
            viewport: (1280, 800),
            warmup: 2,
            steps: vec![PerfStep::Frames(20)],
        };
        let report = run_scenario_on(state, root, &scenario);

        assert_eq!(report.frames, 20);
        assert_eq!(
            report.handle_event.max_us, 0,
            "no input was dispatched, so the handle_event pass is zero every frame"
        );
        assert!(report.draw.max_us > 0, "the draw pass is still measured");
        assert!(report.total.max_us > 0);
    }

    // Case `report_renders_table_and_json`.
    // The report renders a human-readable table and machine-readable JSON — the
    // two shapes a CLI / CI consumes.
    {
        let (state, root) = list_app(60);
        let report = run_scenario_on(state, root, &PerfScenario::scroll("fmt", 10));

        let table = report.to_table();
        assert!(table.contains("perf scenario"));
        assert!(table.contains("draw"));
        assert!(table.contains("TOTAL"));

        let json = report.to_json();
        assert!(json.contains("\"draw\""));
        assert!(json.contains("\"over_budget\""));
        assert!(json.contains("\"p95_us\""));
    }
}
