#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "the isolated performance probe intentionally uses direct percentile indexes and timing conversions"
)]

//! What one editor frame costs on the CPU, measured on the real path.
//!
//! A frame of the framework host is the same sync → layout → draw the live
//! app runs every frame, so timing it answers "is the editor snappy" without
//! a window or a display. Each scenario is staged exactly as `--shots` stages
//! it. It deliberately excludes rasterisation: that is the GPU's time, it
//! scales with pixels rather than with the project, and `compose_probe`
//! already measures it.
//!
//! Run it in **release**. A debug build says nothing useful about feel —
//! layout and text shaping are dominated by code that only optimises well
//! with optimisations on, and a debug number has been mistaken for a real
//! one in this project before:
//!
//! ```text
//! cargo run --release -p video-editor --example frame_cost
//! ```
//!
//! The budget worth holding is a frame at 120 Hz: **8.3 ms** total, of which
//! the CPU half should be a small fraction, because the GPU still has to
//! rasterise and present inside the same frame.

use ui::prelude::TestApp;
use video_editor::VideoEditor;

/// Report the shape of a timing sample, not just its mean: a frame budget is
/// missed by the slow frames, and an average hides them.
fn report(label: &str, mut us: Vec<u128>) {
    us.sort_unstable();
    let at = |q: f64| us[((us.len() - 1) as f64 * q) as usize];
    let mean = us.iter().sum::<u128>() / us.len() as u128;
    println!(
        "{label:<22} n={:<4} min={:>6}us  p50={:>6}us  p95={:>6}us  p99={:>6}us  max={:>6}us  mean={:>6}us",
        us.len(),
        us[0],
        at(0.50),
        at(0.95),
        at(0.99),
        us[us.len() - 1],
        mean,
    );
}

fn main() -> anyhow::Result<()> {
    // Every scenario the editor can be in, so a regression in one panel does
    // not hide behind the others being cheap.
    let scenarios = [
        ("live editor", None),
        ("markers", Some("markers")),
        ("keyframes", Some("keyframes")),
        ("effects", Some("effects")),
        ("geometry", Some("geometry")),
        // The keyboard cursor is the one state that costs the draw pass
        // anything extra: placing its ring resolves the inspector's row list
        // and its layout, which no unfocused frame has to do. It is a state a
        // keyboard user is in for their whole session, so it is measured
        // rather than assumed cheap.
        ("focused inspector", Some("focusinspector")),
    ];

    println!("one frame of sync + layout + draw, 1920x1080, release\n");

    for (label, scenario) in scenarios {
        let mut app = TestApp::<VideoEditor>::capture_at(scenario, (1920, 1080))
            .map_err(|error| anyhow::anyhow!("{label}: {error}"))?;
        let harness = app.harness();

        // Warm up: the first frames build the Taffy tree, populate the glyph
        // cache and fault in the font. Timing those measures start-up, which
        // is a different question from whether a frame is cheap.
        for _ in 0..8 {
            harness.recompose()?;
        }

        let mut samples = Vec::with_capacity(120);
        for _ in 0..120 {
            let t = ui::time::now();
            harness.recompose()?;
            samples.push(t.elapsed().as_micros());
            // Keep the scene alive to here, so the compiler cannot decide the
            // whole composition was dead code.
            std::hint::black_box(harness.scene());
        }
        report(label, samples);
    }

    // The single biggest item inside `sync`, measured on its own: rebuilding
    // the whole `Project` out of the Automerge document.
    println!("\ninside sync, on a document holding the sample project:");
    let mut doc = video_editor::seed::sample_document();
    let read = |doc: &automerge::AutoCommit| {
        <video_editor::Project as ui::tilo_resource::Resource>::read(doc).ok()
    };
    let mut load = Vec::with_capacity(120);
    let mut heads = Vec::with_capacity(120);
    for _ in 0..8 {
        std::hint::black_box(read(&doc));
    }
    for _ in 0..120 {
        let t = ui::time::now();
        std::hint::black_box(read(&doc));
        load.push(t.elapsed().as_micros());
        let t = ui::time::now();
        std::hint::black_box(doc.get_heads());
        heads.push(t.elapsed().as_micros());
    }
    report("load_project", load);
    report("get_heads", heads);

    // The other half of a frame: compositing the picture. This is the
    // production compositor, the one the preview and the export both call, so
    // the number is the per-frame render cost of an export as well as of a
    // scrub. Sources are stubbed, which isolates the composite from decode —
    // decode is the media engine's cost and is measured where it lives.
    println!("\ncompositing a 1080p frame (production compositor, no decode):");
    match video_editor::gpu::HeadlessCompositor::new() {
        Ok(mut c) => {
            let project = video_editor::seed::sample_project();
            let mut sources = video_editor::gpu::NoSources;
            for _ in 0..4 {
                std::hint::black_box(c.frame(&project, 6.0, 1920, 1080, &mut sources));
            }
            let mut us = Vec::with_capacity(60);
            for i in 0..60 {
                // Walk the playhead so no frame is a repeat of the last.
                let t = 2.0 + f64::from(i) * 0.05;
                let start = ui::time::now();
                let rgba = c.frame(&project, t, 1920, 1080, &mut sources);
                us.push(start.elapsed().as_micros());
                std::hint::black_box(&rgba);
            }
            report("composite 1080p", us);
        }
        // A probe, not a test: on a machine with no adapter this is a fact
        // worth printing, not a failure.
        Err(e) => println!("  no wgpu adapter here, so nothing to measure: {e}"),
    }

    Ok(())
}
