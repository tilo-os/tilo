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
//! `shot::compose` is the same sync → layout → draw the live app runs every
//! frame, so timing it answers "is the editor snappy" without a window, a
//! display or a GPU. It deliberately excludes rasterisation: that is the
//! GPU's time, it scales with pixels rather than with the project, and
//! `compose_probe` already measures it.
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

use std::collections::HashMap;

use taffy::prelude::*;
use taffy::{AvailableSpace, Size, Style, TaffyTree};
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use ui::{PermissionContext, PresenceCtx};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use video_editor::app::ShotScenario;
use video_editor::shot;

/// One frame, broken into the five passes, so a regression can be pinned to
/// the pass that caused it rather than to "the frame".
///
/// Deliberately a copy of `shot::compose`'s body rather than a call to it:
/// the phases have to be timed separately, and production has no business
/// carrying timing hooks for a probe. If `compose` changes shape, this
/// diverges and the totals stop matching — which is the signal to update it.
fn phases(
    widget: &mut dyn Widget,
    (w, h): (u32, u32),
    theme: &ui::Theme,
    state: &mut ui::State,
    text: &mut TextRenderer,
) -> anyhow::Result<[u128; 5]> {
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();

    let t = ui::time::now();
    widget.sync(&mut state.doc, &state.root);
    let status = state.undo_status(None);
    ui::undo::set_undo_status(widget, &mut || status.clone());
    let sync_us = t.elapsed().as_micros();

    let t = ui::time::now();
    let child = widget.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;
    let build_us = t.elapsed().as_micros();

    let t = ui::time::now();
    {
        let measure_map = node_map.clone();
        let tr = &mut *text;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    let compute_us = t.elapsed().as_micros();

    let t = ui::time::now();
    widget.apply_layout(&taffy, child, Point::ORIGIN);
    let apply_us = t.elapsed().as_micros();

    let t = ui::time::now();
    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        text,
        theme,
        &presence,
        &permission,
    );
    std::hint::black_box(&scene);
    let draw_us = t.elapsed().as_micros();

    Ok([sync_us, build_us, compute_us, apply_us, draw_us])
}

/// Whether the text this frame drew was shaped again or reused.
///
/// A miss runs cosmic-text; a hit is an `Arc` clone. Text is the expensive
/// part of drawing, so a frame that re-shapes what it drew last frame is
/// paying for a cache that is not working.
fn shape_stats(
    widget: &mut dyn Widget,
    (w, h): (u32, u32),
    theme: &ui::Theme,
    state: &mut ui::State,
    text: &mut TextRenderer,
) -> anyhow::Result<(u64, u64, u128)> {
    shot::compose(widget, (w, h), theme, state, text)?;
    Ok(text.take_shape_stats())
}

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
        ("live editor", ShotScenario::Live),
        ("markers", ShotScenario::Markers),
        ("keyframes", ShotScenario::Keyframes),
        ("effects", ShotScenario::Effects),
        ("geometry", ShotScenario::Geometry),
        // The keyboard cursor is the one state that costs the draw pass
        // anything extra: placing its ring resolves the inspector's row list
        // and its layout, which no unfocused frame has to do. It is a state a
        // keyboard user is in for their whole session, so it is measured
        // rather than assumed cheap.
        ("focused inspector", ShotScenario::FocusInspector),
    ];

    let theme = ui::Theme::dark();
    println!("one frame of sync + layout + draw, 1920x1080, release\n");

    for (label, scenario) in scenarios {
        let mut widget = video_editor::VideoEditorApp::new_for_shot(scenario);
        let mut state = ui::State::new();
        let mut text = ui::text_render::TextRenderer::new();

        // Warm up: the first frames build the Taffy tree, populate the glyph
        // cache and fault in the font. Timing those measures start-up, which
        // is a different question from whether a frame is cheap.
        for _ in 0..8 {
            shot::compose(&mut widget, (1920, 1080), &theme, &mut state, &mut text)?;
        }

        let mut samples = Vec::with_capacity(120);
        for _ in 0..120 {
            let t = ui::time::now();
            let scene = shot::compose(&mut widget, (1920, 1080), &theme, &mut state, &mut text)?;
            samples.push(t.elapsed().as_micros());
            // Keep the scene alive to here, so the compiler cannot decide the
            // whole composition was dead code.
            std::hint::black_box(&scene);
        }
        report(label, samples);
    }

    // Where the time actually goes, on the busiest scenario.
    println!("\nby pass (markers, the worst p95 above):");
    let mut widget = video_editor::VideoEditorApp::new_for_shot(ShotScenario::Markers);
    let mut state = ui::State::new();
    let mut text = ui::text_render::TextRenderer::new();
    for _ in 0..8 {
        phases(&mut widget, (1920, 1080), &theme, &mut state, &mut text)?;
    }
    let mut cols: [Vec<u128>; 5] = Default::default();
    for _ in 0..120 {
        let p = phases(&mut widget, (1920, 1080), &theme, &mut state, &mut text)?;
        for (c, v) in cols.iter_mut().zip(p) {
            c.push(v);
        }
    }
    for (name, col) in [
        "sync",
        "build_layout",
        "compute_layout",
        "apply_layout",
        "draw",
    ]
    .into_iter()
    .zip(cols)
    {
        report(name, col);
    }

    // The single biggest item inside `sync`, measured on its own: rebuilding
    // the whole `Project` out of the Automerge document.
    println!("\ninside sync, on a document holding the sample project:");
    let mut doc = automerge::AutoCommit::new();
    video_editor::doc::VideoDoc::seed_sample(&mut doc);
    let mut load = Vec::with_capacity(120);
    let mut heads = Vec::with_capacity(120);
    for _ in 0..8 {
        std::hint::black_box(video_editor::doc::VideoDoc::load_project(&doc));
    }
    for _ in 0..120 {
        let t = ui::time::now();
        std::hint::black_box(video_editor::doc::VideoDoc::load_project(&doc));
        load.push(t.elapsed().as_micros());
        let t = ui::time::now();
        std::hint::black_box(doc.get_heads());
        heads.push(t.elapsed().as_micros());
    }
    report("load_project", load);
    report("get_heads", heads);

    // Is the text cache earning its keep on a steady frame?
    {
        let mut widget = video_editor::VideoEditorApp::new_for_shot(ShotScenario::Markers);
        let mut state = ui::State::new();
        let mut text = ui::text_render::TextRenderer::new();
        for _ in 0..8 {
            shape_stats(&mut widget, (1920, 1080), &theme, &mut state, &mut text)?;
        }
        let (hits, misses, ns) =
            shape_stats(&mut widget, (1920, 1080), &theme, &mut state, &mut text)?;
        println!(
            "\ntext shaping on a settled frame: {hits} hits, {misses} misses, {}us shaping",
            ns / 1000,
        );
    }

    // The other half of a frame: compositing the picture. This is the
    // production compositor, the one the preview and the export both call, so
    // the number is the per-frame render cost of an export as well as of a
    // scrub. Sources are stubbed, which isolates the composite from decode —
    // decode is the media engine's cost and is measured where it lives.
    println!("\ncompositing a 1080p frame (production compositor, no decode):");
    match video_editor::gpu::HeadlessCompositor::new() {
        Ok(mut c) => {
            let project = video_editor::doc::sample_project();
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
