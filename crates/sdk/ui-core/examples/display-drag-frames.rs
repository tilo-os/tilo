//! A recorded drag on the Displays arrangement canvas, frame by frame: a
//! laptop panel and a desk monitor, the monitor pressed and moved by 240
//! pointer samples paced as a 120 Hz display presents them, each answered by
//! a frame. Prints every frame's cost against the 120 Hz budget and whether
//! any frame relaid the page out; exits non-zero when a frame is over budget
//! or relaid out. Headless: the CPU side of each frame (sync, layout, draw
//! encode), not the GPU raster.
use tilo_ui::headless::FrameHarness;
use tilo_ui::state::State;
use tilo_ui::theme::Theme;
use tilo_ui::widget::display_arrangement::{
    ArrangedDisplay, ArrangementGesture, DisplayArrangement, placement::Placement,
};

/// One frame at 120 Hz.
const BUDGET_US: u64 = 8_333;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let display = |connector: &str, label: &str, number, placement, primary| ArrangedDisplay {
        connector: connector.into(),
        movement_group: None,
        shared: None,
        label: label.into(),
        number,
        placement,
        primary,
        movable: true,
    };
    let canvas = DisplayArrangement::new(
        vec![
            display(
                "eDP-1",
                "Built-in display",
                1,
                Placement::new(0, 0, 1280, 800),
                true,
            ),
            display(
                "DP-1",
                "Dell U2724DE",
                2,
                Placement::new(1280, -320, 2560, 1440),
                false,
            ),
        ],
        Vec::new(),
        tilo_ui::LocalState::new(ArrangementGesture::default()),
    )
    .with_selection(tilo_ui::LocalState::new("eDP-1".to_owned()));
    let mut host =
        FrameHarness::new_with_theme(State::new(), Box::new(canvas), (760, 420), Theme::dark());
    host.profile_frame();
    let rect = host
        .targets()
        .get("display.arrangement")
        .copied()
        .ok_or("the canvas is mounted")?;
    // The monitor's middle, then a loop under and around the panel.
    let start = (rect.x0 + rect.width() * 0.68, rect.center().y);
    host.send(&tilo_ui::synthetic_input::cursor_moved(start.0, start.1));
    host.send(&tilo_ui::synthetic_input::mouse_press());
    let rebuilt = host.rebuilt_frames();
    host.begin_profiling();
    let mut relaid = 0_u32;
    for step in 0..240_u32 {
        let t = f64::from(step) / 240.0 * std::f64::consts::TAU;
        let x = start.0 - 180.0 * (1.0 - t.cos());
        let y = start.1 + 90.0 * t.sin();
        host.send(&tilo_ui::synthetic_input::cursor_moved(x, y));
        let relayout = host.last_relayout();
        if relayout.rebuilt || relayout.measured {
            relaid = relaid.saturating_add(1);
        }
    }
    let frames = host.take_profile_samples();
    host.end_profiling();
    host.send(&tilo_ui::synthetic_input::mouse_release());
    let mut totals: Vec<u64> = frames.iter().map(|frame| frame.total_us).collect();
    totals.sort_unstable();
    let pick = |q: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an index into a short list"
        )]
        let index = ((totals.len().saturating_sub(1)) as f64 * q).round() as usize;
        totals.get(index).copied().unwrap_or_default()
    };
    let over = totals.iter().filter(|&&us| us > BUDGET_US).count();
    println!(
        "{} frames for 240 moves; per frame p50 {} us, p99 {} us, max {} us; budget {} us at 120 Hz; {} over budget; {} relaid out; {} recomposed",
        frames.len(),
        pick(0.5),
        pick(0.99),
        totals.last().copied().unwrap_or_default(),
        BUDGET_US,
        over,
        relaid,
        host.rebuilt_frames().saturating_sub(rebuilt),
    );
    if frames.len() < 240 || over > 0 || relaid > 0 {
        return Err("a drag frame was dropped, over budget or relaid out".into());
    }
    Ok(())
}
