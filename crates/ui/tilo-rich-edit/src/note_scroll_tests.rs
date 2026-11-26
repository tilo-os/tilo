//! What scrolling and typing in a long note cost, through the host's real input and frame path
//! as a window that copies what a scroll keeps on screen presents it: a line already on screen is
//! moved, not shaped or painted again, and a key paints only its own line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::panic,
    reason = "frame fixtures: failures are the assertion"
)]

use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::widget::Scrollable;
use vello::Scene;
use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};

use crate::{Block, BlockEditor};

/// The window the note fills, in logical pixels.
const WIDTH: f64 = 1280.0;
const HEIGHT: f64 = 800.0;

/// One frame of a two-finger scroll at 60 Hz, in logical pixels.
const STEP: f64 = 40.0;
/// Scroll frames measured.
const FRAMES: usize = 60;

struct Window;

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(WIDTH as u32, HEIGHT as u32)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

fn wheel(dy: f64, phase: TouchPhase) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
        phase,
    }
}

fn sentence(index: usize) -> String {
    format!("Row {index}: the quick brown fox jumps over the lazy dog, {index} times")
}

/// An app showing `root` in the window, laid out and drawn until it rests,
/// the pointer over its middle.
fn shown(root: Box<dyn Widget>) -> (App, Scene, SurfaceDamage) {
    let mut app = App::new_with_controlled_inputs(ui::State::new(), root);
    app.set_theme(Theme::dark());
    let mut damage = SurfaceDamage::copying().drawing_only_damage();
    let mut scene = Scene::new();
    for at in 0..6 {
        if at == 2 {
            app.surface_input(
                &Window,
                tilo_ui_base::synthetic_input::cursor_moved(WIDTH / 2.0, HEIGHT / 2.0),
                false,
            );
        }
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    }
    let _ = damage.take_copying();
    (app, scene, damage)
}

/// A long note: `blocks` paragraphs of prose in the rich text editor every
/// document surface of the toolkit edits, scrolled in the window.
fn note(blocks: usize) -> Box<dyn Widget> {
    let mut editor = BlockEditor::new()
        .with_wrap_lines(true)
        .with_compact_paragraph_spacing(true);
    editor.set_blocks(
        (0..blocks)
            .map(|index| Block::paragraph(&sentence(index)))
            .collect(),
    );
    Box::new(Scrollable::new_vertical().with_child(Box::new(editor)))
}

/// A note scrolled under the fingers moves on every frame and never repaints the window whole.
#[test]
fn a_scrolled_note_copies_what_it_keeps_on_screen() {
    let (mut app, mut scene, mut damage) = shown(note(600));
    for frame in 0..FRAMES {
        app.surface_input(&Window, wheel(-STEP, TouchPhase::Moved), false);
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        let (copies, repainted) = damage.take_copying().unwrap_or_else(|| {
            panic!(
                "frame {frame} of the scroll repainted the window whole: {:?}",
                damage.repainted()
            )
        });
        assert!(
            !copies.is_empty() || !repainted.is_empty(),
            "frame {frame} of the scroll changed nothing on screen"
        );
    }
}

/// A scroll of a long note draws the lines it kept again by moving what they
/// drew: no line already on screen is shaped or painted again, and a line
/// that changes is. This is the work a note pays for per frame that a plain
/// page does not.
#[test]
fn a_scroll_keeps_the_lines_it_already_drew() {
    let (mut app, mut scene, mut damage) = shown(note(200));
    let mut painted_ever: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut kept = 0usize;
    for frame in 0..FRAMES {
        let _ = app.take_shape_stats_for_test();
        let _ = crate::rich_text_line::take_face_work();
        let _ = crate::rich_text_line::take_painted_lines();
        app.surface_input(&Window, wheel(-STEP, TouchPhase::Moved), false);
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        damage.take_copying();
        let (_, shaped, _) = app.take_shape_stats_for_test();
        let (painted, moved) = crate::rich_text_line::take_face_work();
        let drawn = crate::rich_text_line::take_painted_lines();
        assert_eq!(shaped, 0, "frame {frame} of the scroll shaped text again");
        assert_eq!(
            painted,
            drawn.len(),
            "frame {frame} counted {painted} lines painted and {drawn:?}"
        );
        // A line that comes into view paints once. A line already on screen
        // that is painted again was drawn from scratch for a scroll.
        for text in drawn {
            assert!(
                painted_ever.insert(text.clone()),
                "frame {frame} of the scroll painted a line it had already drawn: {text}"
            );
        }
        assert!(
            moved > 0,
            "frame {frame} of the scroll kept no line on screen"
        );
        kept += moved;
    }
    assert!(
        kept > 20 * FRAMES,
        "the scroll kept only {kept} lines in view over {FRAMES} frames"
    );

    // The line a change reaches is drawn again, and no other.
    for _ in 0..FRAMES {
        app.surface_input(&Window, wheel(STEP, TouchPhase::Moved), false);
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        damage.take_copying();
    }
    edit_first_line(app.root_widget_mut()).expect("the note's editor is shown");
    let _ = crate::rich_text_line::take_face_work();
    scene.reset();
    app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    let (painted, moved) = crate::rich_text_line::take_face_work();
    assert_eq!(
        painted, 1,
        "an edited line painted {painted} lines, keeping {moved}"
    );
}

/// Change the first line the editor shows, as an edit to the document does.
fn edit_first_line(widget: &mut dyn Widget) -> Option<()> {
    if widget
        .as_any_mut()
        .is_some_and(|any| any.is::<BlockEditor>())
    {
        let editor = widget.as_any_mut()?.downcast_mut::<BlockEditor>()?;
        editor
            .blocks_mut()
            .first_mut()?
            .line
            .set_text("Edited by the note");
        return Some(());
    }
    for child in widget.children_mut() {
        if edit_first_line(child).is_some() {
            return Some(());
        }
    }
    None
}

/// A key typed into a long note paints the line it changed and moves what
/// every other line in view drew: the key lays the editor out again, and
/// that alone painted every line in view again.
#[test]
fn a_keystroke_in_a_long_note_paints_only_its_line() {
    let (mut app, mut scene, mut damage) = shown(note(600));
    for event in [
        tilo_ui_base::synthetic_input::cursor_moved(400.0, 200.0),
        tilo_ui_base::synthetic_input::mouse_press(),
        tilo_ui_base::synthetic_input::mouse_release(),
    ] {
        app.surface_input(&Window, event, false);
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        damage.take_copying();
    }
    for (index, letter) in "a few words typed"
        .split("")
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        let _ = crate::rich_text_line::take_face_work();
        app.surface_input(
            &Window,
            tilo_ui_base::synthetic_input::char_key(letter),
            false,
        );
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        let (painted, kept) = crate::rich_text_line::take_face_work();
        assert!(
            damage.take_copying().is_some(),
            "key {index} repainted the window whole: {:?}",
            damage.repainted()
        );
        assert_eq!(
            painted, 1,
            "key {index} painted {painted} lines and kept {kept}"
        );
        assert!(kept > 20, "key {index} kept only {kept} lines in view");
    }
}
