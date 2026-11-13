//! What scrolling a long note repaints in a window of its own: the real
//! Notes window and record, driven through `App::surface_input` and painted
//! by `App::surface_frame_with_damage` for a host that keeps its last frame
//! (`SurfaceDamage::copying`), as the native window presents.
//!
//! A scroll that moves only the note copies what stays on screen and
//! repaints the strip it exposed, the thumb's track and what reaches into
//! the body from around it, not the window. Every frame is presented over the
//! one before (copies, then damage) and checked against a full render of the
//! same scene, in dark, light and high contrast.
//!
//! Once the pointer rests over the note the app reads a clock the test steps
//! ([`FRAME_STEP`] a frame), not the machine's: the pointer brings up the
//! blocks' drag handles, which fade three seconds later, and a frame that
//! fades them is not a scroll alone and does not copy. Read off the machine,
//! how many scroll frames met the fade followed how fast the test ran.
//!
//! After the runs down and up each note is taken to its ends
//! ([`scroll_to_the_ends`]): a wheel for more than the way to the top, a wheel
//! against the top, and a wheel for more than the window is tall. Those frames
//! are presented and checked like the others.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "frame tallies and pixel counts of a bounded run over a window clamped before indexing"
)]

use std::fmt::Write as _;

use notes::NotesShell;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::framework::testing::drive_live_window;
use ui::offscreen::OffscreenRenderer;
use ui::synthetic_input::{cursor_moved, key_input, mouse_press, mouse_release};
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::winit::event::{ElementState, Ime, MouseScrollDelta, TouchPhase, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
use vello::Scene;
use vello::kurbo::Rect;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 900;

/// The time between two frames once the pointer rests over the note
/// ([`Host::step_the_clock`]). The pointer coming to rest, the note going to
/// its top and the ten frames after that take twelve frames, so the scroll
/// down begins 1.92 s after the pointer woke the drag handles: its first
/// seven frames scroll with the handles showing, the next two meet their
/// fade (3 s to 3.32 s after the pointer moved) and do not copy, and the
/// rest of the way down and all the way back up scrolls with them gone.
/// Eighteen of the twenty frames down and all ten back up copy, on every
/// run.
const FRAME_STEP: Duration = Duration::from_millis(160);

/// A wheel for one and a half windows: further than the window is tall, so
/// nothing the window showed before it stays on screen.
const JUMP: f64 = 1350.0;

/// Run `operation` at `clock` when the host steps one, else at the machine's.
fn at<T>(clock: Option<Instant>, operation: impl FnOnce() -> T) -> T {
    match clock {
        Some(now) => ui::time::testing::with_monotonic_time(now, operation),
        None => operation(),
    }
}

/// A theme to check in, by name.
type ThemeCase = (&'static str, fn() -> Theme);

const THEMES: [ThemeCase; 3] = [
    ("dark", Theme::dark),
    ("light", Theme::light),
    ("high contrast", Theme::high_contrast),
];

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

/// The window's host: it presents each frame over the last one on a GPU
/// target, copies first.
struct Host {
    damage: SurfaceDamage,
    scene: Scene,
    presented: OffscreenRenderer,
    full: OffscreenRenderer,
    background: vello::peniko::Color,
    /// Whether frames reach the GPU target. Not while the note is written:
    /// [`scroll`] begins by drawing the scene whole, which replaces whatever
    /// the frames before it presented, so the frames that write the note
    /// are framed by the app and their damage taken, and not drawn.
    presenting: bool,
    /// The instant the app reads the clock at, once the host steps it
    /// ([`Self::step_the_clock`]): the next frame's, [`FRAME_STEP`] after
    /// the frame before it.
    clock: Option<Instant>,
}

/// What a run of scroll frames did.
#[derive(Debug, Default)]
struct Run {
    frames: usize,
    /// Frames that copied, and the pixels they repainted, summed.
    copied_frames: usize,
    repainted: f64,
    /// Frames repainted whole, and why the first was.
    whole: usize,
    why: Option<&'static str>,
}

impl Host {
    fn new(theme: &Theme) -> Self {
        Self {
            damage: SurfaceDamage::copying(),
            scene: Scene::new(),
            presented: OffscreenRenderer::try_new(WIDTH, HEIGHT)
                .expect("a scroll copy is checked on the GPU the toolkit renders with"),
            full: OffscreenRenderer::new(WIDTH, HEIGHT),
            background: theme.tokens.colors.surfaces.content,
            presenting: false,
            clock: None,
        }
    }

    /// From here on the app reads the time this host gives it: now, and
    /// [`FRAME_STEP`] later with each frame.
    fn step_the_clock(&mut self) {
        self.clock = Some(ui::time::now());
    }

    /// Deliver `event` to the window, at the host's clock.
    fn send(&self, app: &mut App, event: WindowEvent) {
        at(self.clock, || {
            app.surface_input(&Window, event, false);
        });
    }

    /// One frame of the app, presented as the window presents it.
    fn frame(&mut self, app: &mut App, run: &mut Run) {
        self.scene.reset();
        let (scene, damage) = (&mut self.scene, &mut self.damage);
        at(self.clock, || {
            app.surface_frame_with_damage(&Window, scene, damage);
        });
        run.frames += 1;
        if let Some((copies, rects)) = self.damage.take_copying() {
            if !copies.is_empty() {
                run.copied_frames += 1;
                run.repainted += covered(&rects);
            }
            if self.presenting {
                let _ = self.presented.render_copies_and_damage_gpu_wait(
                    &self.scene,
                    self.background,
                    &copies,
                    &rects,
                );
            }
        } else {
            run.whole += 1;
            run.why = run.why.or_else(|| self.damage.repainted());
            if self.presenting {
                let _ = self.presented.render(&self.scene, self.background);
            }
        }
        let _ = at(self.clock, || app.refresh_semantic_tree());
        if let Some(clock) = self.clock.as_mut() {
            *clock += FRAME_STEP;
        }
    }

    fn input(&mut self, app: &mut App, event: WindowEvent) {
        self.send(app, event);
        self.frame(app, &mut Run::default());
    }

    /// The presented pixels that differ from a full render beyond
    /// anti-aliasing noise.
    fn differing(&mut self) -> (usize, Option<Rect>) {
        let expected = self.full.render(&self.scene, self.background);
        let actual = self.presented.readback_current();
        let mut spread: Option<Rect> = None;
        let row = WIDTH as usize * 4;
        // Both frames are the window's rows, so the rows hold every pixel.
        assert_eq!(actual.len(), row * HEIGHT as usize, "the presented frame");
        assert_eq!(expected.len(), actual.len(), "the full render");
        let mut differing = 0_usize;
        for (y, (actual, expected)) in actual
            .chunks_exact(row)
            .zip(expected.chunks_exact(row))
            .enumerate()
        {
            // A row presented byte for byte as the full render drew it holds
            // no differing pixel: only a row that differs is compared and
            // read pixel by pixel, which an unoptimised test build does
            // slowly.
            if actual == expected {
                continue;
            }
            differing += OffscreenRenderer::pixels_differing_beyond_aa(actual, expected);
            for (x, (a, e)) in actual
                .chunks_exact(4)
                .zip(expected.chunks_exact(4))
                .enumerate()
            {
                if a.iter().zip(e).any(|(a, e)| a.abs_diff(*e) > 1) {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "pixel indices within the window"
                    )]
                    let pixel = Rect::new(x as f64, y as f64, x as f64 + 1.0, y as f64 + 1.0);
                    spread = Some(spread.map_or(pixel, |rect| rect.union(pixel)));
                }
            }
        }
        (differing, spread)
    }
}

/// The pixels of the window `rects` cover, each counted once.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "rects are clamped to the 1280x900 window before becoming pixel indices"
)]
fn covered(rects: &[Rect]) -> f64 {
    let window = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
    let width = WIDTH as usize;
    let mut cells = vec![false; width * HEIGHT as usize];
    // Each pixel is counted once, as the first rect over it covers it: the
    // count reads the pixels the rects cover, not the whole window.
    let mut count = 0_usize;
    for rect in rects {
        let rect = rect.intersect(window).expand();
        for y in rect.y0 as usize..rect.y1 as usize {
            let row = &mut cells[y * width + rect.x0 as usize..y * width + rect.x1 as usize];
            count += row.iter().filter(|cell| !**cell).count();
            row.fill(true);
        }
    }
    count as f64
}

/// The bounds of the first published node named `name`.
fn bounds_of(app: &App, name: &str) -> Rect {
    app.semantic_tree()
        .iter()
        .find(|node| node.name.as_deref() == Some(name))
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("no {name:?} on screen"))
}

/// Open a new note and write `paragraphs` paragraphs into it, as a person
/// does: each a committed phrase and Enter.
fn write_a_long_note(app: &mut App, host: &mut Host, paragraphs: usize) {
    app.init_layout();
    for _ in 0..5 {
        host.frame(app, &mut Run::default());
    }
    let new_note = bounds_of(app, "New note");
    host.input(app, cursor_moved(new_note.center().x, new_note.center().y));
    host.input(app, mouse_press());
    host.input(app, mouse_release());
    for _ in 0..5 {
        host.frame(app, &mut Run::default());
    }
    for paragraph in 0..paragraphs {
        let mut text = String::new();
        let _ = write!(
            text,
            "Paragraph {paragraph}: the quick brown fox jumps over the lazy dog, \
             and a second clause so the line runs on a while."
        );
        app.surface_input(&Window, WindowEvent::Ime(Ime::Commit(text)), false);
        let enter = Key::Named(NamedKey::Enter);
        app.surface_input(&Window, key_input(&enter, ElementState::Pressed), false);
        app.surface_input(&Window, key_input(&enter, ElementState::Released), false);
        host.frame(app, &mut Run::default());
    }
    // Leave the body: a caret blinking in it is a change besides the
    // scroll. The pointer then rests over the body.
    let escape = Key::Named(NamedKey::Escape);
    host.input(app, key_input(&escape, ElementState::Pressed));
    host.input(app, key_input(&escape, ElementState::Released));
    // The pointer brings up the drag handles, which fade by the clock.
    host.step_the_clock();
    host.input(app, cursor_moved(700.0, 500.0));
    // Writing left the end of the note on screen; start from its top.
    host.input(
        app,
        WindowEvent::MouseWheel {
            device_id: ui::winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(ui::winit::dpi::PhysicalPosition::new(
                0.0, 100_000.0,
            )),
            phase: TouchPhase::Moved,
        },
    );
    for _ in 0..10 {
        host.frame(app, &mut Run::default());
    }
}

/// Commit `text` into the focused block, as an input method does.
fn commit(app: &mut App, text: &str) {
    app.surface_input(
        &Window,
        WindowEvent::Ime(Ime::Commit(text.to_owned())),
        false,
    );
}

fn press(app: &mut App, host: &mut Host, key: NamedKey) {
    let key = Key::Named(key);
    app.surface_input(&Window, key_input(&key, ElementState::Pressed), false);
    app.surface_input(&Window, key_input(&key, ElementState::Released), false);
    host.frame(app, &mut Run::default());
}

/// Open a new note and write `sections` sections into it, each a
/// paragraph, a bulleted list, a to-do and a code block, through the
/// editor's Markdown shortcuts as a person types them; then leave the body
/// with the pointer resting over it, and go back to the top.
fn write_a_rich_note(app: &mut App, host: &mut Host, sections: usize) {
    app.init_layout();
    for _ in 0..5 {
        host.frame(app, &mut Run::default());
    }
    let new_note = bounds_of(app, "New note");
    host.input(app, cursor_moved(new_note.center().x, new_note.center().y));
    host.input(app, mouse_press());
    host.input(app, mouse_release());
    for _ in 0..5 {
        host.frame(app, &mut Run::default());
    }
    for section in 0..sections {
        commit(
            app,
            &format!("Section {section}: the quick brown fox jumps over the lazy dog."),
        );
        press(app, host, NamedKey::Enter);
        for item in 0..3 {
            if item == 0 {
                commit(app, "- ");
            }
            commit(app, &format!("Item {item} of section {section}"));
            press(app, host, NamedKey::Enter);
        }
        // Enter on the empty bullet continues the item above; a second
        // Enter ends the list.
        press(app, host, NamedKey::Enter);
        press(app, host, NamedKey::Enter);
        commit(app, "[ ] ");
        commit(app, &format!("Something to finish in section {section}"));
        press(app, host, NamedKey::Enter);
        press(app, host, NamedKey::Enter);
        press(app, host, NamedKey::Enter);
        commit(app, "```");
        press(app, host, NamedKey::Enter);
        for line in 0..3 {
            commit(app, &format!("let line_{line} = {section};"));
            press(app, host, NamedKey::Enter);
        }
        commit(app, "```");
        press(app, host, NamedKey::Enter);
    }
    let escape = Key::Named(NamedKey::Escape);
    host.input(app, key_input(&escape, ElementState::Pressed));
    host.input(app, key_input(&escape, ElementState::Released));
    // The pointer brings up the drag handles, which fade by the clock.
    host.step_the_clock();
    host.input(app, cursor_moved(700.0, 500.0));
    host.input(
        app,
        WindowEvent::MouseWheel {
            device_id: ui::winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(ui::winit::dpi::PhysicalPosition::new(
                0.0, 100_000.0,
            )),
            phase: TouchPhase::Moved,
        },
    );
    for _ in 0..10 {
        host.frame(app, &mut Run::default());
    }
}

/// Scroll `frames` frames of `step` logical pixels (down for a positive
/// step), each presented and checked against a full render.
fn scroll(app: &mut App, host: &mut Host, label: &str, (step, frames): (f64, usize)) -> Run {
    let mut run = Run::default();
    host.presenting = true;
    host.presented.render(&host.scene, host.background);
    for frame in 0..frames {
        host.send(
            app,
            WindowEvent::MouseWheel {
                device_id: ui::winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(ui::winit::dpi::PhysicalPosition::new(
                    0.0, -step,
                )),
                phase: TouchPhase::Moved,
            },
        );
        host.frame(app, &mut run);
        let (differing, spread) = host.differing();
        assert_eq!(
            differing, 0,
            "{label}: frame {frame} presented {differing} pixels a full render does not have, \
             within {spread:?} ({run:?})"
        );
    }
    run
}

/// How far down its note the first notes panel in `widget`'s tree shows it,
/// in logical pixels.
fn note_scroll(widget: &mut dyn ui::Widget) -> Option<f64> {
    if let Some(any) = widget.as_any_mut()
        && let Some(panel) = any.downcast_mut::<notes::MainPanel>()
    {
        return Some(panel.scroll_y_for_test());
    }
    widget.children_mut().into_iter().find_map(note_scroll)
}

/// How far down the note the window shows it, in logical pixels.
fn scrolled(app: &mut App) -> f64 {
    note_scroll(app.root_widget_mut()).expect("the window shows a note")
}

/// The ends of the note and a jump between them, each frame presented and
/// checked against a full render like any scroll frame: a wheel for more than
/// the way to the top stops the note at its top, a wheel against the top
/// moves nothing and so copies nothing, and a wheel for [`JUMP`] moves the
/// note by that or to its end. Returns how far that last wheel moved it.
fn scroll_to_the_ends(app: &mut App, host: &mut Host, name: &str) -> f64 {
    let away = scrolled(app);
    assert!(
        away > 0.0,
        "{name}: the runs down and up left the note away from its top (at {away})"
    );
    scroll(app, host, &format!("{name}: to the top"), (-100_000.0, 1));
    let top = scrolled(app);
    assert!(
        top.abs() < f64::EPSILON,
        "{name}: a wheel for more than the way to the top stops the note at its top, not at {top}"
    );
    let held = scroll(app, host, &format!("{name}: against the top"), (-40.0, 1));
    let still = scrolled(app);
    assert!(
        still.abs() < f64::EPSILON,
        "{name}: a wheel against the top leaves the note at its top, not at {still}"
    );
    assert_eq!(
        held.copied_frames, 0,
        "{name}: a wheel against the top moves nothing to copy ({held:?})"
    );
    scroll(app, host, &format!("{name}: a jump down"), (JUMP, 1));
    let jumped = scrolled(app);
    assert!(
        jumped > 0.0,
        "{name}: a wheel for {JUMP} px moves the note off its top"
    );
    jumped
}

#[test]
fn scrolling_a_long_note_copies_what_stays_on_screen() {
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            app.set_theme(theme.clone());
            let mut host = Host::new(&theme);
            write_a_long_note(app, &mut host, 120);
            let down = scroll(app, &mut host, &format!("{name}: down"), (40.0, 20));
            let up = scroll(app, &mut host, &format!("{name}: up"), (-32.0, 10));
            let window = f64::from(WIDTH) * f64::from(HEIGHT);
            for (direction, run) in [("down", &down), ("up", &up)] {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a frame count is far below f64's integer range"
                )]
                let per_frame = run.repainted / run.copied_frames.max(1) as f64;
                eprintln!(
                    "notes-scroll-damage {name} {direction} frames={} copied={} whole={} \
                     repainted_px_per_copied_frame={per_frame:.0} of {window:.0} ({:?})",
                    run.frames, run.copied_frames, run.whole, run.why
                );
                assert!(
                    run.copied_frames * 4 >= run.frames * 3,
                    "{name}: most frames scrolling {direction} copy ({run:?})"
                );
                assert!(
                    per_frame < window * 0.25,
                    "{name}: a frame scrolling {direction} repaints the exposed strip and its \
                     surroundings, not the note ({per_frame:.0} px, {run:?})"
                );
            }
            // 120 paragraphs are many windows long: the jump is not cut
            // short by the note's end, and leaves nothing on screen to copy.
            let jumped = scroll_to_the_ends(app, &mut host, name);
            assert!(
                jumped >= f64::from(HEIGHT),
                "{name}: a wheel for {JUMP} px moves a long note further than the window is \
                 tall ({jumped} px)"
            );
        })
        .unwrap();
    }
}

/// Lists, to-dos and code runs paint chrome for where the pointer rests (the
/// drag handle, a code run's copy chrome) while the text moves under it: a
/// scroll still copies, repainting that chrome where it is and was, and
/// matches a full render.
#[test]
fn scrolling_a_note_with_lists_and_code_copies_what_stays_on_screen() {
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            app.set_theme(theme.clone());
            let mut host = Host::new(&theme);
            write_a_rich_note(app, &mut host, 8);
            // The shortcuts made lists, to-dos and code runs: their markers
            // are gone from the text, which reached the note.
            let texts: Vec<String> = app
                .semantic_tree()
                .iter()
                .filter(|node| node.name.as_deref() == Some("Block text"))
                .filter_map(|node| node.value.clone())
                .collect();
            assert!(
                texts.iter().any(|text| text.starts_with("Item 0"))
                    && texts.iter().any(|text| text.starts_with("let line_0"))
                    && texts
                        .iter()
                        .any(|text| text.starts_with("Something to finish"))
                    && !texts.iter().any(|text| text.starts_with("- ")
                        || text.starts_with("```")
                        || text.starts_with("[ ]")),
                "{name}: the note holds lists, to-dos and code: {texts:?}"
            );
            let down = scroll(app, &mut host, &format!("{name}: down"), (40.0, 20));
            let up = scroll(app, &mut host, &format!("{name}: up"), (-32.0, 10));
            let window = f64::from(WIDTH) * f64::from(HEIGHT);
            for (direction, run) in [("down", &down), ("up", &up)] {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a frame count is far below f64's integer range"
                )]
                let per_frame = run.repainted / run.copied_frames.max(1) as f64;
                eprintln!(
                    "notes-scroll-damage rich {name} {direction} frames={} copied={} whole={} \
                     repainted_px_per_copied_frame={per_frame:.0} of {window:.0} ({:?})",
                    run.frames, run.copied_frames, run.whole, run.why
                );
                assert!(
                    run.copied_frames * 4 >= run.frames * 3,
                    "{name}: most frames scrolling {direction} copy ({run:?})"
                );
                assert!(
                    per_frame < window * 0.25,
                    "{name}: a frame scrolling {direction} repaints the exposed strip, the \
                     chrome that follows the pointer and the surroundings, not the note \
                     ({per_frame:.0} px, {run:?})"
                );
            }
            scroll_to_the_ends(app, &mut host, name);
        })
        .unwrap();
    }
}
