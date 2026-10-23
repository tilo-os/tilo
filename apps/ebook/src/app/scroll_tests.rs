//! Sustained scrolling through a long book, measured frame by frame.

use std::fmt::Write as _;
use std::time::Duration;

use ui::headless::{Framed, viewport};
use ui::theme::Theme;

use super::{BooksApp, ReadMode};
use crate::model::{Book, Settings};

/// One frame's budget at 60 Hz, in milliseconds.
const FRAME_BUDGET_MS: f64 = 1000.0 / 60.0;

/// Text runs the UI thread may shape anew in one frame of a scroll: the
/// lines coming into view and the window's labels as they change (at most
/// 23 in a sweep), well short of the 80 paragraphs of a chapter set at once.
const SHAPED_PER_FRAME: u64 = 48;

const SYLLABLES: [&str; 24] = [
    "ka", "lo", "mer", "an", "tis", "ve", "dor", "ul", "sen", "pra", "ith", "gal", "on", "ry",
    "bel", "qua", "es", "nor", "fi", "tum", "ar", "will", "ost", "e",
];

/// A book of `chapters` chapters of `paragraphs` paragraphs each, about
/// `words` words a paragraph, drawn from a vocabulary large enough that
/// shaping is not one cached word over and over.
fn prose_book(chapters: usize, paragraphs: usize, words: usize) -> Book {
    let mut seed: u32 = 0x2545_f491;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let vocabulary: Vec<String> = (0..4000)
        .map(|_| {
            let n = 1 + next() as usize % 4;
            (0..n)
                .map(|_| SYLLABLES[next() as usize % SYLLABLES.len()])
                .collect()
        })
        .collect();
    let chapters: Vec<(String, Vec<String>)> = (1..=chapters)
        .map(|chapter| {
            let body = (0..paragraphs)
                .map(|_| {
                    let mut text = String::new();
                    let n = words / 2 + next() as usize % words;
                    for i in 0..n {
                        // Common words often, rare ones now and then.
                        let pick = (next() as usize % 64).pow(2) % vocabulary.len();
                        let word = &vocabulary[(pick + next() as usize % 8) % vocabulary.len()];
                        if i > 0 {
                            text.push(' ');
                        }
                        text.push_str(word);
                        if next() % 11 == 0 {
                            text.push(',');
                        }
                    }
                    text.push('.');
                    text
                })
                .collect();
            (format!("Chapter {chapter}"), body)
        })
        .collect();
    let borrowed: Vec<(&str, Vec<String>)> = chapters
        .iter()
        .map(|(heading, paragraphs)| (heading.as_str(), paragraphs.clone()))
        .collect();
    let bytes = crate::test_epub::epub("A Long Book", &borrowed);
    crate::epub::parse_epub(&bytes, 0, "").expect("the generated book parses")
}

/// A record holding the reader's settings in `mode`.
fn record_in(mode: ReadMode) -> ui::State {
    let mut state = ui::State::new();
    let mut doc = crate::resource::doc_with(&[], &[]);
    let before: Settings = crate::resource::read_doc(&doc).settings;
    let after = Settings {
        mode,
        ..before.clone()
    };
    for edit in crate::resource::Ebook::paths()
        .settings
        .path()
        .edits(&before, &after)
    {
        crate::resource::edit_doc(&mut doc, &edit);
    }
    state.doc = doc;
    state
}

/// The reader open on `book` in `mode` in a `w`×`h` window, its first frame
/// run.
fn reader(book: Book, mode: ReadMode, (w, h): (u32, u32)) -> Framed<BooksApp> {
    let app = BooksApp::with_library(crate::resource::library_dispatcher_for_test(), vec![book]);
    Framed::new(record_in(mode), app, viewport((w, h)), Theme::dark())
}

/// How a person scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollInput {
    /// A mouse wheel spun steadily: a notch every fourth frame.
    Wheel,
    /// Two fingers on a touchpad flicking: ten frames of fast pixel deltas,
    /// then nothing for fifty.
    Fling,
    /// Two fingers held moving on a touchpad: a large pixel delta every
    /// frame, fast enough to cross chapters in seconds.
    Sweep,
}

/// One frame of a driven scroll.
#[derive(Clone, Copy, Debug)]
struct Sample {
    /// Diagnostics only: wall-clock and CPU time depend on the machine.
    cpu_ms: f64,
    wall_ms: f64,
    /// Blocks the UI thread set itself this frame.
    blocks_laid_out: usize,
    /// Text runs the UI thread's renderer shaped anew (cache misses).
    shaped: u64,
    chapter: usize,
    scroll_y: f64,
    /// No text at all on the page.
    blank: bool,
    /// Text on the page, but none in its lower part though the chapter
    /// goes on below.
    unfilled: bool,
    /// The scroll rests at the end of the chapter laid out so far.
    at_end: bool,
}

/// What the page shows after a frame.
fn shown(app: &BooksApp) -> (bool, bool) {
    let blank = app.sel_runs.is_empty();
    let (_, body_y1) = app.reader_body;
    let lowest = app.sel_runs.iter().map(|r| r.y1).fold(f64::MIN, f64::max);
    let (total, view) = app.flow_extent;
    let at_end = app.scroll_y + view >= total - 1.0 && app.chapter + 1 >= app.num_chapters();
    let unfilled = !blank && !at_end && lowest < body_y1 - 120.0;
    (blank, unfilled)
}

fn wheel(dy: f64, phase: winit::event::TouchPhase) -> winit::event::WindowEvent {
    winit::event::WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
            0.0, dy,
        )),
        phase,
    }
}

/// The input of frame `frame` of `input`, or `None` for a frame without.
fn input_at(input: ScrollInput, frame: usize) -> Option<winit::event::WindowEvent> {
    use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};
    match input {
        ScrollInput::Wheel => frame.is_multiple_of(4).then(|| WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: TouchPhase::Moved,
        }),
        ScrollInput::Fling => match frame % 60 {
            0 => Some(wheel(-36.0, TouchPhase::Started)),
            1..=9 => Some(wheel(-36.0, TouchPhase::Moved)),
            10 => Some(wheel(0.0, TouchPhase::Ended)),
            _ => None,
        },
        ScrollInput::Sweep => Some(wheel(-90.0, TouchPhase::Moved)),
    }
}

/// Drive `frames` 60 Hz frames of `input` through `app`. Each frame is one
/// harness frame: the event's own when there is one, otherwise an idle one.
fn drive(app: &mut Framed<BooksApp>, input: ScrollInput, frames: usize) -> Vec<Sample> {
    let centre = app.rect.center();
    Framed::harness_mut(app).send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(centre.x, centre.y),
    });
    let period = Duration::from_micros(16_667);
    let mut samples = Vec::with_capacity(frames);
    let mut next = ui::time::now();
    for frame in 0..frames {
        next += period;
        let cpu = ui::rt::thread_cpu_time();
        let t = ui::time::now();
        let harness = Framed::harness_mut(app);
        harness.take_text_shape_stats();
        let ((), phases) = ui::tilo_resource::profiling::observe(|| match input_at(input, frame) {
            Some(event) => harness.send(&event),
            None => harness.relayout(),
        });
        let (_, shaped, _) = harness.take_text_shape_stats();
        let blocks_laid_out = phases
            .iter()
            .filter(|phase| phase.phase == crate::paginate::LAY_BLOCK_PHASE)
            .map(|phase| phase.calls)
            .sum();
        let wall_ms = t.elapsed().as_secs_f64() * 1000.0;
        let cpu_ms = match (cpu, ui::rt::thread_cpu_time()) {
            (Some(before), Some(after)) => after.saturating_sub(before).as_secs_f64() * 1000.0,
            _ => wall_ms,
        };
        let (blank, unfilled) = shown(app);
        let (total, view) = app.flow_extent;
        let at_end = app.scroll_y + view >= total - 1.0;
        samples.push(Sample {
            cpu_ms,
            wall_ms,
            blocks_laid_out,
            shaped,
            chapter: app.chapter,
            scroll_y: app.scroll_y,
            blank,
            unfilled,
            at_end,
        });
        let now = ui::time::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    samples
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    let at = ((values.len() as f64 - 1.0) * p).round() as usize;
    values.get(at).copied().unwrap_or(0.0)
}

fn quantiles(mut values: Vec<f64>) -> String {
    let p50 = percentile(&mut values, 0.5);
    let p95 = percentile(&mut values, 0.95);
    let max = values.last().copied().unwrap_or(0.0);
    format!("p50 {p50:6.2} p95 {p95:6.2} max {max:6.2}")
}

/// A run's numbers in a line or three.
fn report(label: &str, samples: &[Sample]) -> String {
    const EARLY: usize = 18;
    let cpu = |s: &[Sample]| s.iter().map(|s| s.cpu_ms).collect::<Vec<_>>();
    let (early, late) = samples.split_at(EARLY.min(samples.len()));
    let over = samples
        .iter()
        .filter(|s| s.cpu_ms > FRAME_BUDGET_MS)
        .count();
    let over_wall = samples
        .iter()
        .filter(|s| s.wall_ms > FRAME_BUDGET_MS)
        .count();
    let blank = samples.iter().filter(|s| s.blank).count();
    let unfilled = samples.iter().filter(|s| s.unfilled).count();
    let chapters = samples.last().map_or(0, |s| s.chapter);
    let laid_out = samples.iter().map(|s| s.blocks_laid_out).max().unwrap_or(0);
    let shaped = samples.iter().map(|s| s.shaped).max().unwrap_or(0);
    let shaped_total: u64 = samples.iter().map(|s| s.shaped).sum();
    let waited = samples
        .windows(2)
        .filter(|pair| (pair[1].chapter, pair[1].scroll_y) <= (pair[0].chapter, pair[0].scroll_y))
        .count();
    let mut out = format!(
        "SCROLL {label}: frames={} reached chapter {} at y {:.0}; UI-thread blocks set per frame \
         max {laid_out}; runs shaped per frame max {shaped}, total {shaped_total}; frames resting \
         at a chapter's end {waited}; cpu {} (first 0.3 s {}; after {}); \
         wall {}; over_16.7ms cpu={over} wall={over_wall}; blank={blank} unfilled={unfilled}",
        samples.len(),
        chapters + 1,
        samples.last().map_or(0.0, |s| s.scroll_y),
        quantiles(cpu(samples)),
        quantiles(cpu(early)),
        quantiles(cpu(late)),
        quantiles(samples.iter().map(|s| s.wall_ms).collect()),
    );
    let mut worst: Vec<(usize, &Sample)> = samples.iter().enumerate().collect();
    worst.sort_by(|a, b| b.1.cpu_ms.total_cmp(&a.1.cpu_ms));
    for (i, s) in worst.iter().take(4) {
        let _ = write!(
            out,
            "\n  frame {i:4}: cpu {:6.2} wall {:6.2} set {} shaped {} chapter {} y {:.0}",
            s.cpu_ms, s.wall_ms, s.blocks_laid_out, s.shaped, s.chapter, s.scroll_y
        );
    }
    out
}

/// Frames until the first text shows, the wall time that took, and the
/// UI-thread CPU of the slowest of those frames.
fn first_content(book: Book, mode: ReadMode) -> (Framed<BooksApp>, usize, f64, f64) {
    let start = ui::time::now();
    let cpu0 = ui::rt::thread_cpu_time();
    let mut app = reader(book, mode, (1280, 800));
    let mut worst = match (cpu0, ui::rt::thread_cpu_time()) {
        (Some(a), Some(b)) => b.saturating_sub(a).as_secs_f64() * 1000.0,
        _ => 0.0,
    };
    let mut frames = 1;
    let deadline = tilo_test_env::hang_deadline();
    while app.sel_runs.is_empty() && ui::time::now() < deadline {
        std::thread::sleep(Duration::from_millis(4));
        let cpu = ui::rt::thread_cpu_time();
        Framed::harness_mut(&mut app).relayout();
        if let (Some(a), Some(b)) = (cpu, ui::rt::thread_cpu_time()) {
            worst = worst.max(b.saturating_sub(a).as_secs_f64() * 1000.0);
        }
        frames += 1;
    }
    let wall = start.elapsed().as_secs_f64() * 1000.0;
    (app, frames, wall, worst)
}

/// Measurement: open a long book and scroll it for ten seconds by wheel,
/// touchpad fling and a held touchpad sweep. Set `EBOOK_SCROLL_MEASURE=1`;
/// run it under `taskset` to measure a machine with fewer cores.
#[test]
fn measure_long_book_scroll() {
    if std::env::var_os("EBOOK_SCROLL_MEASURE").is_none() {
        return;
    }
    for input in [ScrollInput::Wheel, ScrollInput::Fling, ScrollInput::Sweep] {
        let book = prose_book(24, 160, 90);
        let (mut app, frames, wall, worst) = first_content(book, ReadMode::Continuous);
        eprintln!(
            "FIRST {input:?}: text after {frames} frames, {wall:.1} ms; \
                 slowest of those frames {worst:.2} ms cpu"
        );
        let samples = drive(&mut app, input, 600);
        eprintln!("{}", report(&format!("{input:?}"), &samples));
    }
}

/// Run idle frames until the reader shows its chapter fully laid out.
fn settle(app: &mut Framed<BooksApp>) {
    let deadline = tilo_test_env::hang_deadline();
    while app.layout_pending() && ui::time::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
        Framed::harness_mut(app).relayout();
    }
    assert!(!app.layout_pending(), "the chapter was laid out");
}

/// Share of the reading column's pixels, as the CPU renderer draws the
/// frame, that are not bare page: ink, or the skeleton of lines to come.
fn marked_share(app: &Framed<BooksApp>) -> f64 {
    let rect = app.rect;
    let (w, h) = (rect.width() as u32, rect.height() as u32);
    let (y0, y1) = app.reader_body;
    let region = tilo_cpu_raster::Region::new(
        (rect.width() * 0.3) as u32,
        y0 as u32 + 48,
        (rect.width() * 0.4) as u32,
        (y1 - y0) as u32 - 96,
    );
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    {
        let mut target = tilo_cpu_raster::Target::new(&mut pixels, w, h, w as usize * 4)
            .expect("a frame-sized target");
        tilo_cpu_raster::CpuRasterizer::default().render(
            Framed::harness(app).scene().encoding(),
            &mut target,
            Some(&[region]),
            tilo_cpu_raster::color::AlphaColor::from_rgba8(0, 0, 0, 255),
        );
    }
    let px = |x: u32, y: u32| {
        let i = (y as usize * w as usize + x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    };
    let mut counts: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            *counts.entry(px(x, y)).or_default() += 1;
        }
    }
    let total: usize = counts.values().sum();
    let page = counts.values().copied().max().unwrap_or(0);
    (total - page) as f64 / total.max(1) as f64
}

/// Holding two fingers moving on a touchpad sweeps through the first chapter
/// of a long book and on into the next. Chapters are laid out on the worker,
/// the next one before it is reached, so no frame sets a single block of the
/// book on the UI thread, every frame shows a page full of text, and the
/// text only ever moves on. The work is counted, not timed: the times in the
/// report depend on the machine and are diagnostics only.
#[test]
fn sustained_scrolling_through_a_long_book_never_waits_on_layout() {
    let (mut app, _, _, _) = first_content(prose_book(4, 80, 90), ReadMode::Continuous);
    let mut samples = drive(&mut app, ScrollInput::Sweep, 360);
    // A worker slowed by a loaded machine may keep the scroll resting at the
    // first chapter's end for longer; it still reaches the next one.
    let deadline = tilo_test_env::hang_deadline();
    while samples.last().is_none_or(|s| s.chapter < 1) && ui::time::now() < deadline {
        samples.extend(drive(&mut app, ScrollInput::Sweep, 60));
    }
    let report = report("sweep", &samples);
    eprintln!("{report}");
    assert!(
        samples.last().is_some_and(|s| s.chapter >= 1),
        "the sweep reached the next chapter: {report}"
    );
    for (i, s) in samples.iter().enumerate() {
        assert_eq!(
            s.blocks_laid_out, 0,
            "frame {i} set blocks on the UI thread: {report}"
        );
        assert!(
            s.shaped <= SHAPED_PER_FRAME,
            "frame {i} shaped {} runs on the UI thread: {report}",
            s.shaped
        );
        assert!(!s.blank, "frame {i} shows no text: {report}");
        assert!(!s.unfilled, "frame {i} shows a page part empty: {report}");
    }
    for (i, pair) in samples.windows(2).enumerate() {
        let (before, after) = (&pair[0], &pair[1]);
        let moved = (after.chapter, after.scroll_y) > (before.chapter, before.scroll_y);
        // The one wait the reader has: at a chapter's end, until the worker
        // has set the next chapter's first screen.
        assert!(
            moved || (after.chapter == before.chapter && before.at_end && after.at_end),
            "the text stood still mid-chapter at frame {}: {report}",
            i + 1
        );
    }
}

/// A long chapter shows from its first page while the rest is still being
/// laid out, in a scroll and on pages alike.
#[test]
fn a_long_chapter_shows_before_it_is_all_laid_out() {
    for mode in [ReadMode::Continuous, ReadMode::Paged] {
        let mut app = reader(prose_book(1, 600, 90), mode, (1280, 800));
        let deadline = tilo_test_env::hang_deadline();
        let mut shown_while_laying_out = 0;
        while app.layout_pending() && ui::time::now() < deadline {
            if !app.sel_runs.is_empty() {
                shown_while_laying_out += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
            Framed::harness_mut(&mut app).relayout();
        }
        assert!(!app.layout_pending(), "{mode:?}: the chapter was laid out");
        assert!(
            shown_while_laying_out > 0,
            "{mode:?}: text showed before the chapter was all laid out"
        );
        assert!(!app.sel_runs.is_empty(), "{mode:?}: and shows after");
    }
}

/// While a chapter is laid out anew, no frame shows a blank page: resizing
/// the window keeps the text it had on screen until the new layout reaches
/// it, and a chapter not yet laid out shows the shape of its lines.
#[test]
fn no_frame_shows_a_blank_page_while_laying_out() {
    let mut app = reader(prose_book(12, 120, 90), ReadMode::Paged, (1280, 800));
    let opening = marked_share(&app);
    assert!(
        opening > 0.02,
        "the first frame of a book opened shows no blank page ({:.1}% marked)",
        opening * 100.0
    );
    settle(&mut app);
    for step in 0..40u32 {
        Framed::harness_mut(&mut app)
            .resize((1280 - step * 6, 800))
            .expect("the reader lays out");
        assert!(
            !app.sel_runs.is_empty(),
            "resize frame {step} shows the page's text"
        );
    }
    settle(&mut app);
    let far = app.num_chapters() - 2;
    app.on_goto_chapter(far);
    let deadline = tilo_test_env::hang_deadline();
    let mut frames = 0;
    loop {
        Framed::harness_mut(&mut app).relayout();
        frames += 1;
        let share = marked_share(&app);
        assert!(
            share > 0.02,
            "frame {frames} after the jump shows a blank page ({:.1}% marked)",
            share * 100.0
        );
        if !app.layout_pending() || ui::time::now() > deadline {
            break;
        }
    }
    assert_eq!(app.chapter, far);
    assert!(!app.sel_runs.is_empty(), "the chapter shows its text");
}

/// Write the whole frame `app` shows, rendered on the CPU, to `path`.
fn shoot(app: &Framed<BooksApp>, theme: &Theme, path: &std::path::Path) {
    use image::ImageEncoder as _;
    let (w, h) = (app.rect.width() as u32, app.rect.height() as u32);
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    {
        let mut target = tilo_cpu_raster::Target::new(&mut pixels, w, h, w as usize * 4)
            .expect("a frame-sized target");
        tilo_cpu_raster::CpuRasterizer::default().render(
            Framed::harness(app).scene().encoding(),
            &mut target,
            None,
            {
                let ground = theme.tokens.colors.surfaces.content.to_rgba8();
                tilo_cpu_raster::color::AlphaColor::from_rgba8(ground.r, ground.g, ground.b, 255)
            },
        );
    }
    let file = std::fs::File::create(path).expect("create the shot");
    image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file))
        .write_image(&pixels, w, h, image::ExtendedColorType::Rgba8)
        .expect("write the shot");
}

/// Shots of a chapter while its layout arrives, in dark and light: the
/// first frame of a book opened, and a page and a scroll with their first
/// lines set. Written where `EBOOK_LAYOUT_SHOTS` names.
#[test]
fn shots_of_a_chapter_being_laid_out() {
    let Some(dir) = std::env::var_os("EBOOK_LAYOUT_SHOTS").map(std::path::PathBuf::from) else {
        return;
    };
    write_layout_shots(&dir);
}

fn write_layout_shots(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).expect("the shots directory");
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let book = prose_book(2, 120, 40);
        let opening = Framed::new(
            record_in(ReadMode::Paged),
            BooksApp::with_library(
                crate::resource::library_dispatcher_for_test(),
                vec![book.clone()],
            ),
            viewport((1280, 800)),
            theme.clone(),
        );
        shoot(&opening, &theme, &dir.join(format!("opening-{name}.png")));
        for mode in [ReadMode::Paged, ReadMode::Continuous] {
            let mut app = Framed::new(
                record_in(mode),
                BooksApp::with_library(
                    crate::resource::library_dispatcher_for_test(),
                    vec![book.clone()],
                ),
                viewport((1280, 800)),
                theme.clone(),
            );
            settle(&mut app);
            let mut partial = app.layout.clone().expect("a layout");
            let set_to = partial.key.height_q as f32 * 0.4;
            partial.blocks.retain(|b| b.top < set_to);
            partial.total_height = partial.blocks.last().map_or(0.0, |b| b.bottom);
            partial.pages.truncate(1);
            partial.complete = false;
            app.layout = Some(partial);
            Framed::harness_mut(&mut app).relayout();
            let label = if mode == ReadMode::Paged {
                "page"
            } else {
                "scroll"
            };
            shoot(
                &app,
                &theme,
                &dir.join(format!("arriving-{label}-{name}.png")),
            );
        }
    }
}
