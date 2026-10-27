//! A long document in the preview: its pages render on demand, near the
//! view first, and scrolling never waits for them.

use std::fmt::Write as _;
use std::time::Duration;

use super::*;
use crate::resource::{TypstEditorShell, Workspace};

pub(super) type Live = ui::framework::testing::TestApp<TypstEditorShell>;

/// A document of `chapters` pages written as one loop, so the source stays
/// short and a frame's cost is the preview's.
pub(super) fn looped_source(chapters: usize) -> String {
    format!(
        r#"#set page(paper: "a4", numbering: "1")
#set par(justify: true)
#for i in range(1, {last}) [
  = Chapter #i
  #lorem(260)
  #figure(
    grid(
      columns: 4,
      gutter: 6pt,
      ..range(12).map(k => if calc.rem(k + i, 3) == 0 {{
        circle(radius: 22pt, fill: rgb(30 + k * 15, 90 + calc.rem(i * 7, 120), 200 - k * 10))
      }} else {{
        rect(width: 100%, height: 44pt, radius: 4pt, fill: rgb(200 - k * 12, 60 + k * 10, 120 + calc.rem(i * 11, 100)))
      }}),
    ),
    caption: [Shapes of chapter #i],
  )
  #lorem(140)
  $ sum_(k=1)^n k^#i = integral_0^1 f(x) dif x $
  #pagebreak(weak: true)
]
"#,
        last = chapters + 1
    )
}

/// A document of `chapters` chapters, each a heading, justified text, a
/// figure of shapes and an equation, starting on a fresh page.
pub(super) fn long_source(chapters: usize) -> String {
    let mut source =
        String::from("#set page(paper: \"a4\", numbering: \"1\")\n#set par(justify: true)\n");
    for i in 1..=chapters {
        let _ = write!(
            source,
            r"= Chapter {i}
#lorem(260)
#figure(
  grid(
    columns: 4,
    gutter: 6pt,
    ..range(12).map(k => if calc.rem(k + {i}, 3) == 0 {{
      circle(radius: 22pt, fill: rgb(30 + k * 15, 90 + calc.rem({i} * 7, 120), 200 - k * 10))
    }} else {{
      rect(width: 100%, height: 44pt, radius: 4pt, fill: rgb(200 - k * 12, 60 + k * 10, 120 + calc.rem({i} * 11, 100)))
    }}),
  ),
  caption: [Shapes of chapter {i}],
)
#lorem(140)
$ sum_(k=1)^n k^{i} = integral_0^1 f(x) dif x $
#pagebreak(weak: true)
"
        );
    }
    source
}

/// Where to type into the chapter shown on the first page on screen: the
/// start of its closing paragraph.
pub(super) fn edit_point(app: &mut Live, source: &str) -> (usize, usize) {
    let chapter = preview(app, |p| {
        let first = p.pages_on_screen()[0];
        (0..=first).rev().find_map(|page| p.chapter_on(page))
    })
    .expect("a chapter heading on or above the screen");
    let heading = source.find(&format!("= Chapter {chapter}\n")).unwrap();
    let at = heading + source.get(heading..).unwrap().find("#lorem(140)").unwrap();
    (chapter, at)
}

/// The editor at 1280×800 over an empty workspace file.
pub(super) fn live() -> Live {
    let mut app = Live::new().unwrap();
    app.harness().resize((1280, 800)).unwrap();
    app.harness().relayout();
    app
}

pub(super) fn put_source(app: &mut Live, at: usize, text: &str) {
    let file = app.state().files.ids().next().unwrap().clone();
    let source = Workspace::paths().files.item(&file).source;
    app.invoke(ui::tilo_resource::Invoke::edit(&source.splice(at, 0, text)).unwrap())
        .unwrap();
}

pub(super) fn preview<T>(app: &mut Live, visit: impl FnOnce(&mut PreviewPane) -> T) -> T {
    app.with_surface::<TypstApp, _>(|view| visit(&mut view.preview))
        .expect("the editor surface")
}

pub(super) fn cursor_over_preview(app: &mut Live) {
    let centre = app
        .with_surface::<TypstApp, _>(|view| view.preview_rect.center())
        .unwrap();
    app.harness()
        .send(&ui::synthetic_input::cursor_moved(centre.x, centre.y));
}

pub(super) fn wheel(px: f64) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -px)),
        phase: winit::event::TouchPhase::Moved,
    }
}

/// Run frames until the compile and every render the view waits on are
/// done, as the window does when the worker wakes it.
pub(super) fn settle(app: &mut Live) -> usize {
    let deadline = ui::time::now() + tilo_test_env::hang_bound();
    let mut frames = 0;
    loop {
        app.harness().relayout();
        frames += 1;
        let busy = app
            .with_surface::<TypstApp, _>(|view| view.has_pending_visual_work())
            .unwrap();
        if !busy || ui::time::now() > deadline {
            return frames;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The editor showing a `chapters`-chapter document, settled.
fn opened(chapters: usize) -> Live {
    let mut app = live();
    put_source(&mut app, 0, &looped_source(chapters));
    settle(&mut app);
    app
}

/// One frame of a driven scroll.
pub(super) struct ScrollFrame {
    pub(super) cpu_ms: f64,
    pub(super) scroll_y: f64,
    pub(super) blank_pages: usize,
    pub(super) bitmap_bytes: usize,
}

/// Scroll the preview down `px` per frame for `frames` frames of a 60 Hz
/// display, as a touchpad's pixel deltas arrive, one frame per step.
pub(super) fn drive_scroll(app: &mut Live, frames: usize, px: f64) -> Vec<ScrollFrame> {
    cursor_over_preview(app);
    let period = Duration::from_micros(16_667);
    let mut out = Vec::with_capacity(frames);
    let mut next = ui::time::now();
    for _ in 0..frames {
        next += period;
        let before = ui::rt::thread_cpu_time();
        let t = ui::time::now();
        app.harness().send(&wheel(px));
        let wall = t.elapsed().as_secs_f64() * 1000.0;
        let cpu_ms = match (before, ui::rt::thread_cpu_time()) {
            (Some(a), Some(b)) => b.saturating_sub(a).as_secs_f64() * 1000.0,
            _ => wall,
        };
        let (scroll_y, blank_pages, bitmap_bytes) = preview(app, |p| {
            (p.scroll_y(), p.blank_pages_on_screen(), p.bitmap_bytes())
        });
        out.push(ScrollFrame {
            cpu_ms,
            scroll_y,
            blank_pages,
            bitmap_bytes,
        });
        let now = ui::time::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    out
}

/// One frame's budget at 60 Hz, in milliseconds.
const FRAME_BUDGET_MS: f64 = 16.667;

#[test]
fn opening_a_long_document_renders_only_what_is_near_the_view() {
    let mut app = opened(60);
    let (pages, rendered, delivered, on_screen, bytes) = preview(&mut app, |p| {
        (
            p.page_count(),
            p.pages_rendered(),
            p.delivered_pages(),
            p.pages_on_screen(),
            p.bitmap_bytes(),
        )
    });
    assert!(pages >= 60, "the document has {pages} pages");
    assert!(!on_screen.is_empty());
    assert_eq!(
        delivered.first(),
        on_screen.first(),
        "the page on screen renders first"
    );
    assert!(
        delivered
            .iter()
            .all(|page| *page <= on_screen[on_screen.len() - 1] + 2),
        "only pages on or near the screen render: {delivered:?}, on screen {on_screen:?}"
    );
    // Pages on screen, a screen ahead and the thumbnails in view.
    assert!(
        rendered < 20,
        "{rendered} renders for a {pages}-page document"
    );
    let page_bytes = 1200 * 1700 * 4;
    assert!(
        bytes < 12 * page_bytes,
        "{bytes} bytes of bitmaps held, not one per page"
    );
}

#[test]
fn scrolling_never_waits_on_rendering() {
    // Five seconds of touchpad scrolling through pages the renderer has not
    // drawn yet. Every frame fits the 60 Hz budget on the UI thread and moves
    // the document: rendering happens on the worker, the window draws what
    // it holds.
    let mut app = opened(60);
    let rendered_before = preview(&mut app, |p| p.pages_rendered());
    let frames = drive_scroll(&mut app, 300, 24.0);
    for (i, frame) in frames.iter().enumerate() {
        assert!(
            frame.cpu_ms <= FRAME_BUDGET_MS,
            "frame {i} took {:.2} ms of UI-thread time, over the {FRAME_BUDGET_MS} ms budget",
            frame.cpu_ms
        );
    }
    for (i, pair) in frames.windows(2).enumerate() {
        assert!(
            pair[1].scroll_y > pair[0].scroll_y,
            "the document stood still at frame {}: {:.1} -> {:.1}",
            i + 1,
            pair[0].scroll_y,
            pair[1].scroll_y
        );
    }
    let rendered = preview(&mut app, |p| p.pages_rendered()) - rendered_before;
    assert!(
        rendered >= 3,
        "the worker rendered pages as the scroll reached them"
    );
}

/// Scroll the preview down `px` per frame for `frames` frames against a
/// renderer that delivers every page within `lead` frames of being asked
/// for it: before each frame, the frames run until nothing asked `lead` or
/// more frames ago is still pending. A page that is then blank on screen was
/// asked for too late, however loaded the machine is.
fn drive_scroll_rendering_within(
    app: &mut Live,
    frames: usize,
    px: f64,
    lead: usize,
) -> Vec<ScrollFrame> {
    cursor_over_preview(app);
    let mut asked = std::collections::BTreeMap::<usize, usize>::new();
    let mut out = Vec::with_capacity(frames);
    for frame in 0..frames {
        let deadline = ui::time::now() + tilo_test_env::hang_bound();
        while preview(app, |p| p.pending_pages())
            .iter()
            .any(|page| asked.get(page).is_some_and(|at| at + lead <= frame))
        {
            assert!(
                ui::time::now() < deadline,
                "the renderer never delivered the pages asked for before frame {frame}"
            );
            app.harness().relayout();
            std::thread::sleep(Duration::from_millis(1));
        }
        app.harness().send(&wheel(px));
        let (scroll_y, blank_pages, bitmap_bytes, pending) = preview(app, |p| {
            (
                p.scroll_y(),
                p.blank_pages_on_screen(),
                p.bitmap_bytes(),
                p.pending_pages(),
            )
        });
        asked.retain(|page, _| pending.contains(page));
        for page in pending {
            asked.entry(page).or_insert(frame);
        }
        out.push(ScrollFrame {
            cpu_ms: 0.0,
            scroll_y,
            blank_pages,
            bitmap_bytes,
        });
    }
    out
}

#[test]
fn scrolling_never_shows_a_blank_page() {
    // Pages are asked for a screen ahead of the scroll, so a renderer that
    // takes up to a sixth of a second (ten frames) per page keeps up.
    let mut app = opened(60);
    let frames = drive_scroll_rendering_within(&mut app, 300, 24.0, 10);
    let blank: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.blank_pages > 0)
        .map(|(i, _)| i)
        .collect();
    assert!(blank.is_empty(), "frames showing a blank page: {blank:?}");
    let peak = frames.iter().map(|f| f.bitmap_bytes).max().unwrap_or(0);
    let all_pages = preview(&mut app, |p| p.page_count()) * 1200 * 1700 * 4;
    assert!(
        peak < all_pages / 3,
        "{peak} bytes of bitmaps at most while scrolling"
    );
}

#[test]
fn an_edit_re_renders_visible_pages_first() {
    let source = long_source(60);
    let mut app = live();
    put_source(&mut app, 0, &source);
    settle(&mut app);
    // Scroll into the document and let the view settle.
    drive_scroll(&mut app, 60, 40.0);
    settle(&mut app);
    let rendered_before = preview(&mut app, |p| p.pages_rendered());

    // Type into the chapter on screen.
    let (chapter, at) = edit_point(&mut app, &source);
    put_source(&mut app, at, "Edited. ");
    let mut blank_frames = 0;
    let deadline = ui::time::now() + tilo_test_env::hang_bound();
    loop {
        app.harness().relayout();
        if preview(&mut app, |p| p.blank_pages_on_screen()) > 0 {
            blank_frames += 1;
        }
        let busy = app
            .with_surface::<TypstApp, _>(|view| view.has_pending_visual_work())
            .unwrap();
        if !busy || ui::time::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let (delivered, now_on_screen, rendered) = preview(&mut app, |p| {
        (p.delivered_pages(), p.pages_on_screen(), p.pages_rendered())
    });
    assert_eq!(blank_frames, 0, "the edit never blanks a page on screen");
    let first = *delivered.first().expect("the edited pages render again");
    assert!(
        now_on_screen.contains(&first),
        "page {first} rendered first after the edit, on screen are {now_on_screen:?} (chapter {chapter})"
    );
    let again = rendered - rendered_before;
    assert!(
        again <= 8,
        "an edit to one chapter rendered {again} pages and thumbnails again"
    );
}
