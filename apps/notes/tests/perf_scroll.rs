//! Scroll cost of a note with several large pictures, per frame: the work a
//! wheel notch causes on the main thread (sync, layout, draw) once the
//! pictures have decoded. Budgets apply to optimised builds only; a debug
//! build prints the numbers.
//!
//! `cargo test -p notes --test it perf_scroll -- --nocapture`
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "timing statistics over bounded sample counts: microsecond casts, percentile indices and medians of a few hundred samples"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::panic,
    reason = "a benchmark reports on stderr and fails loudly"
)]

use std::time::Instant;

use notes::{NotesShell, PageId};
use ui::clipboard::ClipboardImage;
use ui::framework::testing::TestApp;

fn png(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        let v = x.wrapping_mul(2_654_435_761).wrapping_add(seed) ^ y.wrapping_mul(40_503);
        let [r, g, b, _] = v.to_le_bytes();
        image::Rgb([r / 2 + (x * 255 / width) as u8 / 2, g, b])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn paste(app: &mut TestApp<NotesShell>) {
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("v");
    app.harness().set_modifiers(&[]);
}

/// A note with `images` large pictures between paragraphs, laid out at the
/// Yoga's logical size.
fn picture_note(images: u32) -> (TestApp<NotesShell>, PageId) {
    let (mut app, page) = crate::framework_support::with_note("Pictures", "");
    app.harness()
        .relayout_themed((1472, 920), ui::theme::Theme::dark())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    for i in 0..images {
        app.harness().type_text("A paragraph between two pictures.");
        app.harness().key("Enter");
        recorder.copy_image_elsewhere(ClipboardImage {
            mime: "image/png",
            bytes: png(1400, 900, i),
        });
        paste(&mut app);
        app.harness().key("Enter");
    }
    // Let every picture decode before measuring scroll.
    for _ in 0..200 {
        app.harness().relayout();
        let pending =
            crate::framework_support::surface(&mut app, |notes| notes.images_pending_for_test());
        if pending == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    (app, page)
}

fn stat(label: &str, samples: &mut [u64]) -> u64 {
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    eprintln!(
        "perf {label}: n={} p50={}us p99={}us max={}us",
        samples.len(),
        at(0.5),
        at(0.99),
        samples[samples.len() - 1]
    );
    at(0.5)
}

#[test]
fn scroll_frame_with_large_pictures() {
    let (mut app, _) = picture_note(5);
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().begin_profiling();
    let mut wall = Vec::new();
    for i in 0..80 {
        let t = Instant::now();
        app.harness().scroll(0.0, if i < 40 { -1.0 } else { 1.0 });
        wall.push(t.elapsed().as_micros() as u64);
    }
    let frames = app.harness().take_profile_samples();
    let n = frames.len().max(1) as u64;
    let sum = |f: fn(&ui::profiling::FrameTiming) -> u64| frames.iter().map(f).sum::<u64>() / n;
    eprintln!(
        "perf notes scroll passes over {} frames (mean us): sync={} build={} apply={} draw={} event={} resources={}",
        frames.len(),
        sum(|f| f.sync_us),
        sum(|f| f.build_layout_us),
        sum(|f| f.apply_layout_us),
        sum(|f| f.draw_us),
        sum(|f| f.handle_event_us),
        sum(|f| f.resource_availability_us),
    );
    let p50 = stat("notes scroll step (picture note)", &mut wall);
    if !cfg!(debug_assertions) {
        assert!(p50 <= 4_000, "scroll step p50 {p50}us exceeds 4000us");
    }
}

#[test]
fn resize_step_with_large_pictures() {
    let (mut app, _) = picture_note(5);
    for i in 0..6 {
        app.harness().resize((1472 - i, 920 - i)).unwrap();
    }
    app.harness().begin_profiling();
    let mut wall = Vec::new();
    for i in 0..60u32 {
        let size = (1300 + (i * 7) % 170, 800 + (i * 5) % 110);
        let t = Instant::now();
        app.harness().resize(size).unwrap();
        wall.push(t.elapsed().as_micros() as u64);
    }
    let frames = app.harness().take_profile_samples();
    let n = frames.len().max(1) as u64;
    let sum = |f: fn(&ui::profiling::FrameTiming) -> u64| frames.iter().map(f).sum::<u64>() / n;
    eprintln!(
        "perf notes resize passes over {} frames (mean us): sync={} build={} apply={} draw={}",
        frames.len(),
        sum(|f| f.sync_us),
        sum(|f| f.build_layout_us),
        sum(|f| f.apply_layout_us),
        sum(|f| f.draw_us),
    );
    let p50 = stat("notes resize step (picture note)", &mut wall);
    if !cfg!(debug_assertions) {
        assert!(p50 <= 8_000, "resize step p50 {p50}us exceeds 8000us");
    }
}
