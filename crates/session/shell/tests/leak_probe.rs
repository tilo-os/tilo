//! Diagnostic probe (not an assertion test): loops the CPU-side frame
//! pipeline (`relayout` + `paint`) on the idle bootstrap tree and prints
//! RSS every N iterations, isolating a per-frame leak in the
//! widget/taffy/text/CRDT path from GPU-side (vello/wgpu) growth.
//!
//! Run: `cargo test -p shell --test leak_probe -- --ignored --nocapture`

#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use shell::shell::frame::FrameBuilder;

fn rss_kb() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    // Field 2 (0-indexed 1) is resident set size in pages.
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let page_kb = 4; // 4 KiB pages on x86_64
    pages.saturating_mul(page_kb)
}

#[test]
#[ignore = "a diagnostic probe that prints RSS; run with --ignored --nocapture"]
fn idle_gpu_render_loop_rss_growth() {
    // The GPU path: relayout + paint + offscreen vello render (wgpu), to
    // isolate a vello-atlas / wgpu-resource per-frame leak.
    use ui::offscreen::OffscreenRenderer;
    let mut fb = FrameBuilder::new((1600, 900));
    let mut r = OffscreenRenderer::new(1600, 900);
    let iters = 2000usize;
    let sample_every = 200usize;
    let base = rss_kb();
    println!("[gpu] iter 0  rss={} MB (baseline)", base / 1024);
    for i in 1..=iters {
        fb.relayout();
        let scene = fb.paint();
        let _rgba = r.render(&scene, vello::peniko::Color::BLACK);
        if i % sample_every == 0 {
            let now = rss_kb();
            println!(
                "[gpu] iter {i}  rss={} MB  (+{} MB since baseline)",
                now / 1024,
                (now.saturating_sub(base)) / 1024
            );
        }
    }
    let end = rss_kb();
    println!(
        "[gpu] DONE {iters} iters  rss={} MB  total growth={} MB",
        end / 1024,
        (end.saturating_sub(base)) / 1024
    );
}

#[test]
#[ignore = "a diagnostic probe that prints RSS; run with --ignored --nocapture"]
fn idle_paint_loop_rss_growth() {
    let mut fb = FrameBuilder::new((1600, 900));
    let iters = 6000usize; // ~100s of real frames at 60fps
    let sample_every = 500usize;
    let base = rss_kb();
    println!("iter 0  rss={} MB (baseline)", base / 1024);
    for i in 1..=iters {
        fb.relayout();
        let _scene = fb.paint();
        if i % sample_every == 0 {
            let now = rss_kb();
            println!(
                "iter {i}  rss={} MB  (+{} MB since baseline)",
                now / 1024,
                (now.saturating_sub(base)) / 1024
            );
        }
    }
    let end = rss_kb();
    println!(
        "DONE {iters} iters  rss={} MB  total growth={} MB",
        end / 1024,
        (end.saturating_sub(base)) / 1024
    );
}
