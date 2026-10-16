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
use ui::framework::testing::TestApp;

/// A photo-sized picture whose file stays under the notebook's 1 MiB cap:
/// smooth gradients and soft bands compress, yet every pixel decodes.
fn png(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        let band = ((x / 24 + y / 24 + seed) % 8) as u8 * 16;
        image::Rgb([(x * 255 / width) as u8, (y * 255 / height) as u8, band])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    let bytes = bytes.into_inner();
    assert!(bytes.len() < 1 << 20, "{} bytes", bytes.len());
    bytes
}

/// A note with `images` large pictures after a paragraph each, placed
/// through the notebook's own action and laid out at the Yoga's logical size.
fn picture_note(images: u32) -> (TestApp<NotesShell>, PageId) {
    let (mut app, page) = crate::framework_support::with_note("Pictures", "");
    app.harness()
        .relayout_themed((1472, 920), ui::theme::Theme::dark())
        .unwrap();
    for i in 0..images {
        crate::framework_support::edit_body(
            &mut app,
            page,
            &tilo_document::RichTextAction::AppendParagraph {
                text: "A paragraph between two pictures.".into(),
            },
        );
        crate::framework_support::apply(
            &mut app,
            &notes::actions::place_image_action(page, png(1400, 900, i), None),
        );
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
    // The pictures are in the note and drawn, or nothing here measures them.
    let sources: Vec<String> = crate::framework_support::body(&app, page)
        .into_iter()
        .filter_map(|text| Some(text.split("](").nth(1)?.trim_end_matches(')').to_owned()))
        .collect();
    assert_eq!(
        sources.len(),
        images as usize,
        "pasted pictures: {:?}",
        crate::framework_support::body(&app, page)
    );
    for source in &sources {
        let drawn = crate::framework_support::surface(&mut app, |notes| {
            notes.image_decoded_for_test(source)
        });
        assert!(drawn, "{source} never decoded");
    }
    app.harness().relayout();
    (app, page)
}

/// Median, 99th percentile and worst of `samples`, printed under `label`.
struct Spread {
    p50: u64,
    p99: u64,
    max: u64,
}

fn spread(label: &str, samples: &[u64]) -> Spread {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    let spread = Spread {
        p50: at(0.5),
        p99: at(0.99),
        max: sorted[sorted.len() - 1],
    };
    eprintln!(
        "perf {label}: n={} p50={}us p99={}us max={}us",
        sorted.len(),
        spread.p50,
        spread.p99,
        spread.max
    );
    spread
}

fn stat(label: &str, samples: &[u64]) -> u64 {
    spread(label, samples).p50
}

fn budget(label: &str, value: u64, limit: u64) {
    if !cfg!(debug_assertions) {
        assert!(value <= limit, "{label}: {value}us exceeds {limit}us");
    }
}

/// Mean of each frame pass over the profiled frames, printed under `label`.
fn print_passes(label: &str, frames: &[ui::profiling::FrameTiming]) {
    let n = frames.len().max(1) as u64;
    let sum = |f: fn(&ui::profiling::FrameTiming) -> u64| frames.iter().map(f).sum::<u64>() / n;
    let max = |f: fn(&ui::profiling::FrameTiming) -> u64| frames.iter().map(f).max().unwrap_or(0);
    eprintln!(
        "perf {label} passes over {} frames (mean/max us): sync={}/{} build={}/{} apply={}/{} draw={}/{} event={}/{} resources={}/{}",
        frames.len(),
        sum(|f| f.sync_us),
        max(|f| f.sync_us),
        sum(|f| f.build_layout_us),
        max(|f| f.build_layout_us),
        sum(|f| f.apply_layout_us),
        max(|f| f.apply_layout_us),
        sum(|f| f.draw_us),
        max(|f| f.draw_us),
        sum(|f| f.handle_event_us),
        max(|f| f.handle_event_us),
        sum(|f| f.resource_availability_us),
        max(|f| f.resource_availability_us),
    );
}

/// Wheel `notches` notches down the note, then as many back up, timing each.
fn scroll_through(app: &mut TestApp<NotesShell>, notches: usize) -> Vec<u64> {
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    let scrolled = |app: &mut TestApp<NotesShell>| {
        crate::framework_support::surface(app, |notes| notes.main_panel_mut().scroll_y_for_test())
    };
    let top = scrolled(app);
    let mut wall = Vec::new();
    for i in 0..notches * 2 {
        if i == notches {
            // Every notch down moved the note, or this measured no scrolling.
            let moved = scrolled(app) - top;
            assert!(
                moved >= 40.0 * (notches as f64 - 1.0),
                "scrolled only {moved}px"
            );
        }
        let t = Instant::now();
        app.harness()
            .scroll(0.0, if i < notches { -1.0 } else { 1.0 });
        wall.push(t.elapsed().as_micros() as u64);
    }
    wall
}

/// Whether a wheel notch over the body is a view-only scroll, the contract
/// under which the native host keeps the note's synced widgets and its
/// published resource tree instead of syncing and re-projecting the whole
/// note on every notch. Prints what that projection costs on this note.
fn wheel_keeps_projections(app: &mut TestApp<NotesShell>, label: &str) -> bool {
    let rect = app.harness().targets()["block_editor"];
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::cursor_moved(
            rect.x0 + 40.0,
            rect.y0 + 40.0,
        ));
    let mut publish = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        let resources = app.harness().resources();
        publish.push(t.elapsed().as_micros() as u64);
        assert!(!resources.is_empty());
    }
    spread(&format!("notes resource projection ({label})"), &publish);
    app.harness()
        .root_widget()
        .view_only_scroll(&ui::synthetic_input::mouse_wheel(0.0, -1.0))
}

/// A long text note: headings over paragraphs, bullet and numbered lists,
/// checklists and code, as a working note grows to.
fn long_text_body(sections: usize) -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    for section in 0..sections {
        writeln!(body, "## Section {section}\n").unwrap();
        for paragraph in 0..3 {
            writeln!(
                body,
                "Paragraph {paragraph} of section {section}: {}\n",
                "a sentence that wraps across the column like real prose does. ".repeat(4)
            )
            .unwrap();
        }
        for item in 0..4 {
            writeln!(body, "- bullet {item} with **bold** and _emphasis_").unwrap();
        }
        body.push('\n');
        for item in 0..3 {
            writeln!(body, "{}. numbered step {item}", item + 1).unwrap();
        }
        body.push('\n');
        body.push_str("- [ ] an open task\n- [x] a finished task\n\n");
        body.push_str("```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n");
    }
    body
}

#[test]
fn scroll_frame_with_large_pictures() {
    let (mut app, _) = picture_note(5);
    // Once down and back, so every picture has been drawn before measuring.
    scroll_through(&mut app, 40);
    let view_only = wheel_keeps_projections(&mut app, "picture note");
    app.harness().begin_profiling();
    let wall = scroll_through(&mut app, 40);
    print_passes(
        "notes scroll (picture note)",
        &app.harness().take_profile_samples(),
    );
    let spread = spread("notes scroll step (picture note)", &wall);
    budget("scroll step p50", spread.p50, 4_000);
    budget("scroll step p99", spread.p99, 8_000);
    assert!(
        view_only,
        "a wheel over a note of pictures only moves the view"
    );
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
    let p50 = stat("notes resize step (picture note)", &wall);
    if !cfg!(debug_assertions) {
        assert!(p50 <= 8_000, "resize step p50 {p50}us exceeds 8000us");
    }
}

/// Scrolling a long text note: every notch must cost the same however far
/// down the note it lands, so the tail is held as well as the median.
#[test]
fn scroll_frame_on_a_long_text_note() {
    let body = long_text_body(40);
    let (mut app, _) = crate::framework_support::with_note("Long", &body);
    app.harness()
        .relayout_themed((1472, 920), ui::theme::Theme::dark())
        .unwrap();
    let view_only = wheel_keeps_projections(&mut app, "long text note");
    app.harness().begin_profiling();
    let wall = scroll_through(&mut app, 60);
    print_passes(
        "notes scroll (long text note)",
        &app.harness().take_profile_samples(),
    );
    let spread = spread("notes scroll step (long text note)", &wall);
    budget("long text note scroll step p50", spread.p50, 4_000);
    budget("long text note scroll step p99", spread.p99, 6_000);
    assert!(
        view_only,
        "a wheel over a note of lists, checklists and code only moves the view"
    );
}

/// The first pass down a picture note, as pictures come into view for the
/// first time, and the frames while they decode: neither may stall.
#[test]
fn first_scroll_through_pictures() {
    let (mut app, _) = picture_note(5);
    app.harness().begin_profiling();
    let wall = scroll_through(&mut app, 40);
    print_passes(
        "notes first scroll (picture note)",
        &app.harness().take_profile_samples(),
    );
    let spread = spread("notes first scroll step (picture note)", &wall);
    budget("first picture scroll step p50", spread.p50, 4_000);
    budget("first picture scroll step p99", spread.p99, 8_000);
}
