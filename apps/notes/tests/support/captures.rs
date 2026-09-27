//! Render independent full-frame capture scenarios concurrently.
//!
//! Each capture is a complete, isolated production render: its own temporary
//! profile, record store, text renderer and frame harness, with any fault
//! seams held in thread-locals (`notes::capture`). Nothing is shared between
//! scenarios, so running them on separate threads proves exactly what running
//! them one after another did. The capture tests reserve the whole runner in
//! `.config/nextest.toml` (`threads-required = "num-cpus"`), so the only way to
//! use those reserved cores is inside the test; a serial loop leaves all but
//! one idle while the slowest scenarios type hundreds of keyed frames.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bounds peak memory (each render peaks near 260 MB in a debug build).
const MAX_WORKERS: usize = 8;

pub struct Capture<'a> {
    /// Names the scenario in every failure message.
    pub label: String,
    pub scenario: Option<&'a str>,
    pub viewport: (u32, u32),
    pub path: PathBuf,
}

/// Render every capture through `notes::shots::render_to_path` and assert that
/// each one succeeded and wrote a PNG. Every capture runs even when another
/// fails; the panic lists all failures by label, in table order.
pub fn render_all(captures: &[Capture<'_>]) {
    let results: Vec<std::sync::Mutex<Option<Result<(), String>>>> = captures
        .iter()
        .map(|_| std::sync::Mutex::new(None))
        .collect();
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, MAX_WORKERS)
        .min(captures.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(capture) = captures.get(index) else {
                        break;
                    };
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        render_one(capture)
                    }))
                    .unwrap_or_else(|payload| Err(panic_message(payload.as_ref())));
                    *results[index].lock().unwrap() = Some(outcome);
                }
            });
        }
    });
    let failures: Vec<String> = captures
        .iter()
        .zip(results)
        .filter_map(|(capture, result)| match result.into_inner().unwrap() {
            Some(Ok(())) => None,
            Some(Err(error)) => Some(format!("{}: {error}", capture.label)),
            None => Some(format!("{}: never rendered", capture.label)),
        })
        .collect();
    assert!(
        failures.is_empty(),
        "captures failed:\n{}",
        failures.join("\n")
    );
}

fn render_one(capture: &Capture<'_>) -> Result<(), String> {
    notes::shots::render_to_path(capture.viewport, &capture.path, capture.scenario, None).map_err(
        |error| format!("scenario must reach its requested state and render: {error:?}"),
    )?;
    let bytes = std::fs::read(&capture.path)
        .map_err(|error| format!("rendered evidence must exist: {error}"))?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("screenshot must be a PNG".into());
    }
    eprintln!(
        "notes: reviewed scenario evidence {}",
        capture.path.display()
    );
    Ok(())
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(ToString::to_string)
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .map_or_else(
            || "panicked".into(),
            |message| format!("panicked: {message}"),
        )
}
