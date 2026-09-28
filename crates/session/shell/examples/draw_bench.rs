//! Headless draw benchmark — measures how long the shell takes to
//! compose its scene at rest. Builds a default `FrameBuilder` (which
//! lays out the canonical workspace tree), then calls
//! `FrameBuilder::paint()` N times and reports the mean / min / max
//! draw-only time.
//!
//! No GPU is involved (the scene is composed but never rendered to a
//! texture), so the numbers reflect *only* the widget tree's
//! `draw` cost. With the per-widget scene caches landed in this
//! worktree (`DesktopBackdrop`, `BrowserStub`, `TopBarChrome`,
//! `SidebarWheelDisc`), the second-and-later frames are expected to
//! be substantially cheaper than the first because the caches hit on
//! every shape that hasn't changed.
//!
//! Run with:
//! ```
//! cargo run -p shell --example draw_bench --release
//! ```
//! Override iteration count: `SHELL_DRAW_ITERS=2000`.

use std::time::Instant;

use shell::compositor::frame::FrameBuilder;

#[allow(
    clippy::significant_drop_tightening,
    reason = "the shell fixture owns long-lived permits by design and lives for the whole run"
)]
fn main() {
    let iters: usize = std::env::var("SHELL_DRAW_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    let viewport = (1600u32, 900u32);
    let mut fb = FrameBuilder::new(viewport);

    // Warm-up: ensure the FrameBuilder has gone through one layout +
    // first-paint cycle so the per-widget caches are filled before
    // we start timing the steady state.
    let _ = fb.paint();

    // Measured frames. The shell tree doesn't mutate between calls,
    // so each iteration measures a pure cache-hit path. If the cache
    // landed correctly the per-frame cost should be a fraction of
    // the first-paint cost.
    let mut min_us = u128::MAX;
    let mut max_us = 0u128;
    let mut total_us = 0u128;
    for _ in 0..iters {
        let t0 = Instant::now();
        let _scene = fb.paint();
        let us = t0.elapsed().as_micros();
        min_us = min_us.min(us);
        max_us = max_us.max(us);
        total_us = total_us.saturating_add(us);
    }

    // Benchmark timing display only; precision loss in a microsecond mean at
    // these magnitudes is irrelevant to the printed 0.1us-resolution figure.
    #[allow(
        clippy::cast_precision_loss,
        reason = "benchmark timing display, not a correctness value"
    )]
    let mean = total_us as f64 / f64::from(u32::try_from(iters).unwrap_or(u32::MAX));
    println!("shell draw at rest: iters={iters} mean={mean:.1}us min={min_us}us max={max_us}us");
    println!(
        "  → {:.1} Hz steady-state draw throughput (excludes layout + GPU + present)",
        1_000_000.0 / mean,
    );
}
