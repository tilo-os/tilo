//! What the editor repaints over a long document in a window of its own: the
//! real window, driven through `App::surface_input`, painted by
//! `App::surface_frame_with_damage` for a host that keeps its last frame and
//! presented over the frame before (copies, then damage), then checked
//! against a full render of the same scene.

use std::time::Duration;

use ui::widget::Widget as _;

use ui::app::{AppSurface, SurfaceDamage};
use ui::offscreen::OffscreenRenderer;
use ui::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release};
use vello::Scene;
use vello::kurbo::Rect;

use super::TypstApp;
use super::long_document_tests::{long_source, wheel};
use crate::resource::{TypstEditorShell, Workspace};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const WINDOW_PX: f64 = 1280.0 * 800.0;

/// One frame's budget at 60 Hz, in milliseconds.
const FRAME_BUDGET_MS: f64 = 16.667;

/// The window, in logical pixels at 100 %.
struct Window;

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(WIDTH, HEIGHT)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

/// The window's host: it presents each frame over the last one, copies
/// first, as the native window does.
struct Host {
    damage: SurfaceDamage,
    scene: Scene,
    presented: Option<OffscreenRenderer>,
    full: Option<OffscreenRenderer>,
}

/// What one frame cost and repainted.
#[derive(Clone, Debug)]
struct Frame {
    cpu_ms: f64,
    copied: bool,
    /// The pixels repainted, or `None` for a frame repainted whole.
    repainted: Option<f64>,
    damage: Vec<Rect>,
}

const GROUND: vello::peniko::Color = vello::peniko::Color::BLACK;

impl Host {
    /// A host that presents on the GPU the toolkit renders with, or one that
    /// only counts the damage.
    fn new(presenting: bool) -> Self {
        let renderer = || {
            presenting.then(|| {
                OffscreenRenderer::try_new(WIDTH, HEIGHT)
                    .expect("the frames are presented on the GPU the toolkit renders with")
            })
        };
        Self {
            damage: SurfaceDamage::copying(),
            scene: Scene::new(),
            presented: renderer(),
            full: renderer(),
        }
    }

    fn frame(&mut self, app: &mut ui::App) -> Frame {
        self.scene.reset();
        let before = ui::rt::thread_cpu_time();
        app.surface_frame_with_damage(&Window, &mut self.scene, &mut self.damage);
        let cpu_ms = cpu_since(before);
        self.present().map_or_else(
            || Frame {
                cpu_ms,
                copied: false,
                repainted: None,
                damage: Vec::new(),
            },
            |(copied, damage)| Frame {
                cpu_ms,
                copied,
                repainted: Some(covered(&damage)),
                damage,
            },
        )
    }

    /// Present the frame drawn; its copies and damage when it had them.
    fn present(&mut self) -> Option<(bool, Vec<Rect>)> {
        let frame = self.damage.take_copying();
        if let Some(presented) = self.presented.as_mut() {
            match &frame {
                Some((copies, rects)) => {
                    let _ = presented.render_copies_and_damage_gpu_wait(
                        &self.scene,
                        GROUND,
                        copies,
                        rects,
                    );
                }
                None => {
                    let _ = presented.render(&self.scene, GROUND);
                }
            }
        }
        frame.map(|(copies, rects)| (!copies.is_empty(), rects))
    }

    /// An input and the frame it brings.
    fn input(&mut self, app: &mut ui::App, event: &ui::winit::event::WindowEvent) -> Frame {
        let before = ui::rt::thread_cpu_time();
        app.surface_input(&Window, event.clone(), false);
        let input_ms = cpu_since(before);
        let mut frame = self.frame(app);
        frame.cpu_ms += input_ms;
        frame
    }

    /// The presented pixels that differ from a full render of the last
    /// frame beyond anti-aliasing.
    fn differing(&mut self) -> usize {
        let (Some(presented), Some(full)) = (self.presented.as_mut(), self.full.as_mut()) else {
            return 0;
        };
        let expected = full.render(&self.scene, GROUND);
        let actual = presented.readback_current();
        OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected)
    }
}

fn cpu_since(before: Option<Duration>) -> f64 {
    match (before, ui::rt::thread_cpu_time()) {
        (Some(before), Some(after)) => after.saturating_sub(before).as_secs_f64() * 1000.0,
        _ => 0.0,
    }
}

/// The pixels of the window `rects` cover, each counted once.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "rects are clamped to the window before becoming pixel indices"
)]
fn covered(rects: &[Rect]) -> f64 {
    let window = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
    let width = WIDTH as usize;
    let mut cells = vec![false; width * HEIGHT as usize];
    for rect in rects {
        let rect = rect.intersect(window).expand();
        for y in rect.y0 as usize..rect.y1 as usize {
            if let Some(row) =
                cells.get_mut(y * width + rect.x0 as usize..y * width + rect.x1 as usize)
            {
                row.fill(true);
            }
        }
    }
    cells.iter().filter(|cell| **cell).count() as f64
}

/// The editor in the window `app` drives.
fn typst<T>(app: &mut ui::App, visit: impl FnOnce(&mut TypstApp) -> T) -> T {
    fn find(widget: &mut dyn ui::widget::Widget) -> Option<&mut TypstApp> {
        let surface = ui::framework::testing::surface_in_root::<TypstEditorShell, TypstApp>;
        if surface(widget).is_some() {
            return surface(widget);
        }
        widget.children_mut().into_iter().find_map(find)
    }
    visit(find(app.root_widget_mut()).expect("the editor surface"))
}

/// Open the window on a 100-chapter source and wait until its pages are
/// compiled and drawn.
fn open_long_document(app: &mut ui::App, host: &mut Host) {
    app.init_layout();
    for _ in 0..3 {
        host.frame(app);
    }
    let file = typst(app, |editor| editor.current.clone()).expect("a file is open");
    let source = Workspace::paths().files.item(&file).source;
    let invoke = ui::tilo_resource::Invoke::edit(&source.splice(0, 0, long_source(100)))
        .expect("an edit of the source");
    app.invoke_local_resource_action(&invoke.address(), &invoke.arguments)
        .expect("the source takes the edit");
    settle(app, host, 100);
}

/// Run frames until the window shows at least `pages` pages of its last
/// compile and has nothing left to draw.
fn settle(app: &mut ui::App, host: &mut Host, pages: usize) {
    let deadline = ui::time::now() + Duration::from_secs(300);
    loop {
        host.frame(app);
        let settled = typst(app, |editor| {
            editor.preview.page_count() >= pages
                && !editor.compiling()
                && !editor.has_pending_visual_work()
        });
        if settled {
            break;
        }
        assert!(ui::time::now() < deadline, "the document settles");
        std::thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..3 {
        host.frame(app);
    }
}

/// Scroll `frames` touchpad steps of `px` with the pointer over `point`, at
/// a 60 Hz display's pace.
fn scroll(
    app: &mut ui::App,
    host: &mut Host,
    point: (f64, f64),
    (px, frames): (f64, usize),
) -> Vec<Frame> {
    host.input(app, &cursor_moved(point.0, point.1));
    (0..frames)
        .map(|_| {
            let frame = host.input(app, &wheel(px));
            std::thread::sleep(Duration::from_millis(16));
            frame
        })
        .collect()
}

fn median(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.into_iter().collect();
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

/// A run of scroll frames copies what stays on screen and repaints a strip
/// as tall as the step across the pane, the bars beside it and what the
/// frame drew in place (a page that arrived): not the pane, not the window.
fn assert_scroll_copies(label: &str, frames: &[Frame], pane: Rect, step: f64) {
    let whole: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.repainted.is_none() || !frame.copied)
        .map(|(i, _)| i)
        .collect();
    assert!(
        whole.is_empty(),
        "{label}: every scroll frame copies; frames {whole:?} did not"
    );
    let repainted = median(frames.iter().filter_map(|frame| frame.repainted));
    let strip = pane.width() * step;
    eprintln!(
        "{label}: {} frames, median repainted {repainted:.0} px ({:.1} % of the window, a {step} px strip across the pane is {strip:.0} px)",
        frames.len(),
        100.0 * repainted / WINDOW_PX
    );
    // A scroll frame repaints what the step exposed, the line numbers beside
    // it (every one of them moved) and the status bar's readouts: a few
    // percent of the pane, whether the step lands on one line or on several.
    // A frame that repaints the pane is what this refuses.
    assert!(
        repainted < pane.area() * 0.15,
        "{label}: a scroll frame repaints about the exposed strip, not \
         {repainted:.0} px of a {:.0} px pane (a {step} px strip is {strip:.0} px)",
        pane.area()
    );
}

#[test]
fn scrolling_the_long_source_and_its_preview_repaints_the_exposed_strip() {
    ui::framework::testing::drive_live_window::<TypstEditorShell, _>(|app| {
        let mut host = Host::new(true);
        open_long_document(app, &mut host);
        let (editor, preview) = typst(app, |view| (view.editor_rect, view.preview_rect));

        let source = scroll(
            app,
            &mut host,
            (editor.center().x, editor.center().y),
            (24.0, 40),
        );
        assert_scroll_copies("the source", &source, editor, 24.0);
        assert_eq!(
            host.differing(),
            0,
            "copied source frames equal a full repaint"
        );

        let pages = scroll(
            app,
            &mut host,
            (preview.center().x, preview.center().y),
            (24.0, 40),
        );
        assert_scroll_copies("the preview", &pages, preview, 24.0);
        assert_eq!(
            host.differing(),
            0,
            "copied preview frames equal a full repaint"
        );
    })
    .expect("the editor's window opens");
}

#[test]
fn typing_repaints_the_line_and_the_caret() {
    ui::framework::testing::drive_live_window::<TypstEditorShell, _>(|app| {
        let mut host = Host::new(true);
        open_long_document(app, &mut host);
        let editor = typst(app, |view| view.editor_rect);
        let spot = (editor.x0 + 240.0, editor.y0 + 200.0);
        host.input(app, &cursor_moved(spot.0, spot.1));
        host.input(app, &mouse_press());
        host.input(app, &mouse_release());
        for _ in 0..3 {
            host.frame(app);
        }
        // The first edit starts the document's history, and the History
        // control in the bar shows it has one.
        host.input(app, &char_key("w"));
        settle(app, &mut host, 100);
        let status = typst(app, |view| view.status_rect);
        for key in ["x", "y", "z"] {
            let frame = host.input(app, &char_key(key));
            let line = typst(app, |view| {
                let caret = view
                    .editor
                    .caret_rect()
                    .expect("the editor shows its caret");
                Rect::new(editor.x0, caret.y0, editor.x1, caret.y1)
            });
            let repainted = frame
                .repainted
                .expect("a keystroke repaints part of the window");
            eprintln!(
                "typing {key}: repainted {repainted:.0} px ({:.1} % of the window) in {:?}",
                100.0 * repainted / WINDOW_PX,
                frame.damage
            );
            for rect in &frame.damage {
                assert!(
                    status.inflate(2.0, 2.0).contains_rect(*rect)
                        || line.inflate(4.0, 4.0).contains_rect(*rect),
                    "typing {key} repaints only the edited line, the caret and the status bar, \
                     not {rect:?} (line {line:?}, status bar {status:?})"
                );
            }
            assert!(
                repainted < WINDOW_PX * 0.08,
                "typing {key} repainted {repainted:.0} px"
            );
            assert_eq!(host.differing(), 0, "the typed frame equals a full repaint");
            // The preview takes the edit in a frame of its own, once compiled.
            settle(app, &mut host, 100);
        }
    })
    .expect("the editor's window opens");
}

/// Restrict this process to two of the CPUs it may use, as a weak machine
/// runs the editor.
fn pin_to_two_cpus() {
    let status = std::fs::read_to_string("/proc/self/status").expect("the process status");
    let allowed = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .expect("the CPUs the process may use")
        .trim()
        .to_owned();
    let first = allowed
        .split(',')
        .next()
        .and_then(|part| part.split('-').next())
        .and_then(|cpu| cpu.parse::<u32>().ok())
        .expect("a CPU");
    let pinned = std::process::Command::new("taskset")
        .args([
            "-a",
            "-p",
            "-c",
            &format!("{first},{}", first + 1),
            &std::process::id().to_string(),
        ])
        .status()
        .is_ok_and(|status| status.success());
    let pinned = pinned
        || std::process::Command::new("taskset")
            .args([
                "-a",
                "-p",
                "-c",
                &first.to_string(),
                &std::process::id().to_string(),
            ])
            .status()
            .is_ok_and(|status| status.success());
    assert!(pinned, "the process is pinned");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "frame times are an optimised build's: run with --profile fast"
)]
fn every_frame_of_the_long_document_fits_the_budget_on_two_cpus() {
    pin_to_two_cpus();
    ui::framework::testing::drive_live_window::<TypstEditorShell, _>(|app| {
        let mut host = Host::new(false);
        open_long_document(app, &mut host);
        let (editor, preview) = typst(app, |view| (view.editor_rect, view.preview_rect));
        let mut frames = Vec::new();
        frames.extend(scroll(
            app,
            &mut host,
            (preview.center().x, preview.center().y),
            (24.0, 120),
        ));
        frames.extend(scroll(
            app,
            &mut host,
            (editor.center().x, editor.center().y),
            (24.0, 120),
        ));
        host.input(app, &mouse_press());
        host.input(app, &mouse_release());
        for key in "typing on a weak machine".chars() {
            frames.push(host.input(app, &char_key(&key.to_string())));
            std::thread::sleep(Duration::from_millis(16));
        }
        let over: Vec<String> = frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.cpu_ms > FRAME_BUDGET_MS)
            .map(|(i, frame)| format!("{i}: {:.2} ms", frame.cpu_ms))
            .collect();
        eprintln!(
            "two cpus: {} frames, median {:.2} ms, slowest {:.2} ms",
            frames.len(),
            median(frames.iter().map(|frame| frame.cpu_ms)),
            frames.iter().map(|frame| frame.cpu_ms).fold(0.0, f64::max)
        );
        assert!(
            over.is_empty(),
            "every frame fits the {FRAME_BUDGET_MS} ms budget on the UI thread: {over:?}"
        );
    })
    .expect("the editor's window opens");
}
