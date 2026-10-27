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
use ui::winit::event::{ElementState, Ime, MouseScrollDelta, TouchPhase, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
use vello::Scene;
use vello::kurbo::Rect;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 900;

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
        }
    }

    /// One frame of the app, presented as the window presents it.
    fn frame(&mut self, app: &mut App, run: &mut Run) {
        self.scene.reset();
        app.surface_frame_with_damage(&Window, &mut self.scene, &mut self.damage);
        run.frames += 1;
        if let Some((copies, rects)) = self.damage.take_copying() {
            if !copies.is_empty() {
                run.copied_frames += 1;
                run.repainted += covered(&rects);
            }
            let _ = self.presented.render_copies_and_damage_gpu_wait(
                &self.scene,
                self.background,
                &copies,
                &rects,
            );
        } else {
            run.whole += 1;
            run.why = run.why.or_else(|| self.damage.repainted());
            let _ = self.presented.render(&self.scene, self.background);
        }
        let _ = app.refresh_semantic_tree();
    }

    fn input(&mut self, app: &mut App, event: WindowEvent) {
        app.surface_input(&Window, event, false);
        self.frame(app, &mut Run::default());
    }

    /// The presented pixels that differ from a full render beyond
    /// anti-aliasing noise.
    fn differing(&mut self) -> (usize, Option<Rect>) {
        let expected = self.full.render(&self.scene, self.background);
        let actual = self.presented.readback_current();
        let mut spread: Option<Rect> = None;
        let row = WIDTH as usize * 4;
        for (y, (actual, expected)) in actual
            .chunks_exact(row)
            .zip(expected.chunks_exact(row))
            .enumerate()
        {
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
        (
            OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected),
            spread,
        )
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
    for rect in rects {
        let rect = rect.intersect(window).expand();
        for y in rect.y0 as usize..rect.y1 as usize {
            cells[y * width + rect.x0 as usize..y * width + rect.x1 as usize].fill(true);
        }
    }
    cells.iter().filter(|cell| **cell).count() as f64
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
    host.presented.render(&host.scene, host.background);
    for frame in 0..frames {
        app.surface_input(
            &Window,
            WindowEvent::MouseWheel {
                device_id: ui::winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(ui::winit::dpi::PhysicalPosition::new(
                    0.0, -step,
                )),
                phase: TouchPhase::Moved,
            },
            false,
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
        })
        .unwrap();
    }
}
