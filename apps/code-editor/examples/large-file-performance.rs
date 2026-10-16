//! Explicit real-file performance probe; normal CI only compiles it.
//! Run `cargo run -p code-editor --example large-file-performance` on a builder.
//! Two unique 20 MiB Rust files exercise canonical ingest, switching, CPU
//! frames, software RGBA raster, and peer-local viewport retention separately.

use code_editor::{CodeEditorEffect, CodeEditorShell};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;
use ui::framework::testing::TestApp;

type App = TestApp<CodeEditorShell>;

fn source(seed: usize) -> Result<String, std::fmt::Error> {
    let mut source = String::with_capacity(20 * 1024 * 1024);
    let mut index = 0usize;
    while source.len() < 20 * 1024 * 1024 {
        writeln!(
            source,
            "pub fn unique_{seed}_{index}(value: u64) -> u64 {{ value.wrapping_mul({index}).rotate_left({seed}) }}"
        )?;
        index = index.saturating_add(1);
    }
    Ok(source)
}

fn snapshot(app: &mut App, text: &str) -> Result<serde_json::Value, Box<dyn Error>> {
    let snapshot = app
        .harness()
        .native_input_snapshot()
        .ok_or("actual editor observation missing")?;
    eprintln!(
        "reopen phase microseconds: {}",
        snapshot
            .get("reopen_phase_micros")
            .unwrap_or(&serde_json::Value::Null)
    );
    assert_snapshot(&snapshot, text);
    Ok(snapshot)
}

fn assert_snapshot(snapshot: &serde_json::Value, text: &str) {
    assert_eq!(
        snapshot
            .get("body_bytes")
            .and_then(serde_json::Value::as_u64),
        Some(text.len() as u64)
    );
    assert_eq!(
        snapshot
            .get("body_hash")
            .and_then(serde_json::Value::as_str),
        Some(hex::encode(Sha256::digest(text.as_bytes())).as_str())
    );
    assert_eq!(
        snapshot.get("ready").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(
        snapshot
            .get("cached_buffers")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| count <= 2)
    );
    assert!(
        snapshot
            .get("read_texts")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| count <= 1)
    );
    assert!(
        snapshot
            .get("read_text_bytes")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|bytes| bytes <= 24 * 1024 * 1024)
    );
    assert!(
        snapshot
            .get("cached_diagnostic_buffers")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| count <= 2)
    );
    assert!(
        snapshot
            .get("inactive_diagnostic_records")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| count <= 16_384)
    );
    assert!(
        snapshot
            .get("inactive_source_bytes")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|bytes| bytes <= 48 * 1024 * 1024)
    );
}

fn assert_viewport(before: &serde_json::Value, returned: &serde_json::Value) {
    assert_eq!(
        returned.get("offset_y"),
        before.get("offset_y"),
        "buffer viewport restored"
    );
    assert!(
        before
            .get("offset_y")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|offset| offset > 0.0)
    );
}

fn assert_unique(a: &str, b: &str) {
    assert_ne!(a, b, "two unique sources, not repeated-line fixtures");
}

fn assert_certified(app: &mut App) {
    assert!(app.harness().native_app_audit().is_certified());
}

fn cpu_and_raster(app: &mut App, label: &str) {
    app.harness().begin_profiling();
    app.harness().clear_profile_samples();
    let started = Instant::now();
    app.harness().profile_frame();
    let cpu = started.elapsed();
    let samples = app.harness().take_profile_samples();
    assert!(!samples.is_empty(), "actual CPU frame must be observed");
    let started = Instant::now();
    let pixels = app.harness().render_rgba();
    assert!(
        !pixels.is_empty(),
        "actual software raster must be observed"
    );
    eprintln!(
        "{label}: CPU_frame={cpu:?}, RGBA_only={:?}, passes={samples:?}",
        started.elapsed()
    );
    app.harness().end_profiling();
}

fn scroll(app: &mut App) -> Result<(), Box<dyn Error>> {
    let observed = app
        .harness()
        .native_input_snapshot()
        .ok_or("editor observation missing")?;
    let rect = observed
        .get("editor_rect")
        .ok_or("missing editor_rect")?
        .as_array()
        .ok_or("editor bounds missing")?;
    let number = |index: usize| {
        rect.get(index)
            .and_then(serde_json::Value::as_f64)
            .ok_or("editor bound missing")
    };
    app.harness().send(&ui::synthetic_input::cursor_moved(
        number(0)?.midpoint(number(2)?),
        number(1)?.midpoint(number(3)?),
    ));
    let started = Instant::now();
    for _ in 0..8 {
        app.harness().scroll(0.0, -100.0);
    }
    eprintln!("eight physical wheel CPU frames: {:?}", started.elapsed());
    Ok(())
}

fn switch(app: &mut App, path: &Path, text: &str) -> Result<serde_json::Value, Box<dyn Error>> {
    let started = Instant::now();
    app.effect(&CodeEditorEffect::OpenFile {
        path: Some(path.display().to_string()),
    })?;
    app.settle();
    eprintln!(
        "canonical switch to {}: {:?}",
        path.display(),
        started.elapsed()
    );
    let observed = snapshot(app, text)?;
    cpu_and_raster(app, "after switch");
    Ok(observed)
}

/// Optional single-row probe keeps the source unique while making every
/// character after the viewport offscreen. A plain-text file isolates row
/// shaping from the independently tested syntax executor.
fn long_row() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("single-row.txt");
    let text = source(11)?.replace('\n', " ");
    std::fs::write(&path, &text)?;
    let started = Instant::now();
    let mut app = App::launched(&[path.to_str().ok_or("UTF-8 fixture path")?])?;
    eprintln!(
        "unique 20 MiB single-row canonical launch: {:?}",
        started.elapsed()
    );
    snapshot(&mut app, &text)?;
    cpu_and_raster(&mut app, "single row first");
    assert_bounded_glyphs(&mut app);
    scroll(&mut app)?; // also enters the actual editor's pointer region
    app.harness().click_named("editor_view")?;
    let started = Instant::now();
    app.harness().key("End");
    eprintln!(
        "physical End to deep horizontal viewport: {:?}",
        started.elapsed()
    );
    let observed = snapshot(&mut app, &text)?;
    assert_horizontal(&observed);
    snapshot(&mut app, &text)?;
    cpu_and_raster(&mut app, "single row deep horizontal");
    assert_bounded_glyphs(&mut app);
    Ok(())
}

fn assert_horizontal(observed: &serde_json::Value) {
    assert!(
        observed
            .get("offset_x")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|x| x > 1_000_000.0),
        "the physical key must reach the actual far-right viewport"
    );
}

fn assert_bounded_glyphs(app: &mut App) {
    let glyphs = app.harness().scene().encoding().resources.glyphs.len();
    assert!(
        glyphs > 0 && glyphs < 1_000,
        "the real host must shape only visible glyphs, observed {glyphs}"
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().nth(1).as_deref() == Some("long-row") {
        return long_row();
    }
    let directory = tempfile::tempdir()?;
    let first = directory.path().join("large-first.rs");
    let second = directory.path().join("large-second.rs");
    let a = source(3)?;
    let b = source(7)?;
    assert_unique(&a, &b);
    std::fs::write(&first, &a)?;
    std::fs::write(&second, &b)?;
    let started = Instant::now();
    let mut app = App::launched(&[first.to_str().ok_or("UTF-8 fixture path")?])?;
    eprintln!(
        "unique 20 MiB canonical file launch: {:?}",
        started.elapsed()
    );
    snapshot(&mut app, &a)?;
    cpu_and_raster(&mut app, "first");
    scroll(&mut app)?;
    let before = snapshot(&mut app, &a)?;
    cpu_and_raster(&mut app, "scrolled");
    switch(&mut app, &second, &b)?;
    scroll(&mut app)?;
    let second_scroll = snapshot(&mut app, &b)?;
    let returned = switch(&mut app, &first, &a)?;
    assert_viewport(&before, &returned);
    let returned = switch(&mut app, &second, &b)?;
    assert_viewport(&second_scroll, &returned);
    let returned = switch(&mut app, &first, &a)?;
    assert_viewport(&before, &returned);
    let returned = switch(&mut app, &second, &b)?;
    assert_viewport(&second_scroll, &returned);
    assert_certified(&mut app);
    Ok(())
}
