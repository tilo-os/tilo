//! Drives `MainPanel` vertical scrolling through the real
//! `MouseWheel` path into the notebook surface plus the `apply_layout`
//! overflow clamp, on a live settled layout. Guards the hand-rolled
//! `scroll_y` math (wheel-to-px conversion, top clamp, content-extent
//! clamp) that has no test today.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;
use ui::widget::Edits;

use crate::framework_support::{send, surface, with_note};
use notes::{MainPanel, NotesShell};
use ui::framework::testing::TestApp;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::WindowEvent;

fn wheel(lines: f32) -> WindowEvent {
    ui::synthetic_input::mouse_wheel(0.0, lines)
}

/// A tall body so the panel content overflows any short viewport.
fn tall_body() -> String {
    let mut s = String::from("# Long page\n\n");
    for i in 0..60 {
        let _ = writeln!(s, "Paragraph number {i} with some words.\n");
    }
    s
}

/// Notes with one note holding `body` open, in a `w` x `h` window at text
/// scale `scale`.
fn note_at(body: &str, w: u16, h: u16, scale: f32) -> TestApp<NotesShell> {
    let (mut app, _) = with_note("Long page", body);
    app.harness()
        .relayout_themed(
            (u32::from(w), u32::from(h)),
            Theme::dark().with_text_scale(scale),
        )
        .unwrap();
    app
}

fn dispatch(app: &mut TestApp<NotesShell>, event: &WindowEvent) {
    send(app, event);
}

/// Read or drive the note panel.
fn panel<T>(app: &mut TestApp<NotesShell>, visit: impl FnOnce(&mut MainPanel) -> T) -> T {
    surface(app, |notes| visit(notes.main_panel_mut()))
}

fn scroll_y(app: &mut TestApp<NotesShell>) -> f64 {
    panel(app, |panel| panel.scroll_y_for_test())
}

/// Perform `action` on the panel's semantic `target`, as assistive
/// technology does. Scrolling reads and writes no document.
fn semantic(app: &mut TestApp<NotesShell>, target: &str, action: &ui::a11y::Action) -> bool {
    panel(app, |panel| {
        panel.perform_semantic_action(
            target,
            action,
            &mut Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        )
    })
}

#[test]
fn long_note_thumb_drag_survives_host_routing_and_intermediate_frames() {
    use winit::event::ElementState::{Pressed, Released};

    let mut markdown = String::new();
    let paragraph = "Orange Pi real display scrolling workload. ".repeat(6);
    for index in 0..180 {
        writeln!(markdown, "## Paragraph {index}\n\n{paragraph}\n").unwrap();
    }
    let mut app = note_at(&markdown, 1051, 952, 1.0);
    let original = panel(&mut app, |panel| panel.editor().to_markdown());
    app.harness().send(&pointer(Point::new(600.0, 300.0)));
    app.harness().scroll(0.0, -10.0);
    let start = scroll_y(&mut app);
    assert!(start > 0.0, "host-routed wheel must move the document");
    let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
    app.harness().send(&pointer(thumb.center()));
    app.harness().send(&mouse(Pressed));
    let mut previous = start;
    let mut previous_top = panel(&mut app, |panel| panel.editor().get_rect().y0);
    for step in 1..=24 {
        app.harness().send(&pointer(Point::new(
            thumb.center().x,
            thumb.center().y + f64::from(step) * 8.0,
        )));
        let current = scroll_y(&mut app);
        assert!(
            current > previous,
            "drag step {step} must advance: {previous} -> {current}"
        );
        let top = panel(&mut app, |panel| panel.editor().get_rect().y0);
        assert!(
            top < previous_top,
            "drag step {step} must move content geometry"
        );
        previous = current;
        previous_top = top;
    }
    app.harness().send(&mouse(Released));
    app.harness()
        .send(&pointer(Point::new(thumb.center().x, 800.0)));
    assert_eq!(scroll_y(&mut app), previous, "release ends the drag");
    assert_eq!(
        panel(&mut app, |panel| panel.editor().to_markdown()),
        original
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end scrollbar test verifies pointer, keyboard, semantic, and reload paths"
)]
#[test]
fn note_scrolling_clamps_reveals_and_drags_through_the_real_scrollbar() {
    eprintln!("phase mouse_wheel_scrolls_and_clamps_to_content");
    {
        let mut app = note_at(&tall_body(), 1000, 300, 1.0);
        assert_eq!(scroll_y(&mut app), 0.0, "starts at top");

        // Scroll down: negative LineDelta y → positive px offset.
        dispatch(&mut app, &wheel(-3.0));
        app.harness().relayout_at((1000, 300)).unwrap();
        let scrolled = scroll_y(&mut app);
        assert!(
            scrolled > 0.0,
            "wheel-down must advance scroll_y; got {scrolled}"
        );

        // Scroll far past the bottom: scroll_y must clamp to the content
        // extent — a second huge scroll can't push it further.
        dispatch(&mut app, &wheel(-1000.0));
        app.harness().relayout_at((1000, 300)).unwrap();
        let at_max = scroll_y(&mut app);
        dispatch(&mut app, &wheel(-1000.0));
        app.harness().relayout_at((1000, 300)).unwrap();
        let still_max = scroll_y(&mut app);
        assert!(
            at_max > scrolled,
            "scrolling further must increase offset up to the clamp"
        );
        assert_eq!(
            at_max, still_max,
            "scroll_y must clamp to the content extent (can't exceed it)"
        );
        let (content_bottom, panel_bottom) = panel(&mut app, |panel| {
            (panel.editor().content_bottom(), panel.get_rect().y1)
        });
        assert!(
            content_bottom <= panel_bottom - 44.0,
            "the final row must remain above the save status, not underneath it"
        );

        // Scroll back up past the top: clamps at 0.
        dispatch(&mut app, &wheel(1000.0));
        app.harness().relayout_at((1000, 300)).unwrap();
        assert_eq!(
            scroll_y(&mut app),
            0.0,
            "wheel-up past the top must clamp scroll_y to 0"
        );
    }

    eprintln!(
        "phase bottom_scroll_leaves_one_gutter_under_the_last_row_and_the_page_runs_to_the_window_bottom"
    );
    {
        for scale in [1.0, 2.0] {
            let mut app = note_at(&tall_body(), 1000, 1000, scale);
            dispatch(&mut app, &wheel(-1000.0));
            app.harness()
                .relayout_themed((1000, 1000), Theme::dark().with_text_scale(scale))
                .unwrap();
            let window_bottom = surface(&mut app, |notes| notes.get_rect().y1);
            let content_bottom = panel(&mut app, |panel| panel.editor().content_bottom());
            let gap = window_bottom - content_bottom;
            let theme = Theme::dark().with_text_scale(scale);
            assert!(
                (gap - f64::from(theme.tokens.spacing.rhythm.group)).abs() <= 1.0,
                "at {scale}x, last row ends at {content_bottom} of {window_bottom}: {gap}px is not one gutter"
            );
        }
    }

    eprintln!("phase note_scrollbar_drags_pages_and_exposes_the_same_position_semantically");
    {
        use ui::a11y::Action;
        use winit::event::ElementState::{Pressed, Released};
        let mut app = note_at(&tall_body(), 1000, 600, 1.0);
        let before = panel(&mut app, |panel| panel.editor().to_markdown());
        let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
        assert_eq!(scroll_control(&mut app).value.as_deref(), Some("0"));
        dispatch(&mut app, &pointer(thumb.center()));
        dispatch(&mut app, &mouse(Pressed));
        dispatch(
            &mut app,
            &pointer(Point::new(thumb.center().x, thumb.center().y + 100.0)),
        );
        app.harness().relayout_at((1000, 600)).unwrap();
        assert!(scroll_y(&mut app) > 0.0, "drag must move the note");
        assert!(scroll_control(&mut app).bounds.unwrap().y0 > thumb.y0);
        dispatch(&mut app, &mouse(Released));
        let scrolled = scroll_y(&mut app);
        dispatch(&mut app, &pointer(Point::new(thumb.center().x, 10.0)));
        app.harness().relayout_at((1000, 600)).unwrap();
        assert_eq!(scroll_y(&mut app), scrolled, "release ends the drag");

        assert!(semantic(
            &mut app,
            "notes.scroll-position",
            &Action::SetValue("100".into())
        ));
        app.harness().relayout_at((1000, 600)).unwrap();
        assert_eq!(scroll_control(&mut app).value.as_deref(), Some("100"));
        let (last, panel_bottom) = panel(&mut app, |panel| {
            (
                panel.editor().blocks().last().unwrap().line.get_rect(),
                panel.get_rect().y1,
            )
        });
        assert!(
            last.y1 < panel_bottom,
            "semantic scroll reveals the last line"
        );
        let bottom = scroll_y(&mut app);
        let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
        dispatch(
            &mut app,
            &pointer(Point::new(thumb.center().x, thumb.y0 - 20.0)),
        );
        dispatch(&mut app, &mouse(Pressed));
        dispatch(&mut app, &mouse(Released));
        app.harness().relayout_at((1000, 600)).unwrap();
        assert!(scroll_y(&mut app) < bottom, "track click pages upward");
        assert!(!semantic(
            &mut app,
            "notes.scroll-position",
            &Action::SetValue("NaN".into())
        ));
        assert!(semantic(&mut app, "notes.scroll-position", &Action::Focus));
        let key = |key| {
            let event =
                ui::synthetic_input::key_event(winit::keyboard::Key::Named(key), None, Pressed);
            ui::synthetic_input::keyboard_input(event)
        };
        app.harness().relayout_at((1000, 600)).unwrap();
        dispatch(&mut app, &key(winit::keyboard::NamedKey::Home));
        app.harness().relayout_at((1000, 600)).unwrap();
        assert_eq!(scroll_y(&mut app), 0.0);
        dispatch(&mut app, &key(winit::keyboard::NamedKey::PageDown));
        app.harness().relayout_at((1000, 600)).unwrap();
        assert!(scroll_y(&mut app) > 0.0);
        let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
        dispatch(&mut app, &pointer(thumb.center()));
        dispatch(&mut app, &mouse(Pressed));
        dispatch(&mut app, &WindowEvent::Focused(false));
        let stopped = scroll_y(&mut app);
        dispatch(&mut app, &pointer(Point::new(thumb.center().x, 500.0)));
        app.harness().relayout_at((1000, 600)).unwrap();
        assert_eq!(scroll_y(&mut app), stopped, "focus loss cancels dragging");
        assert_eq!(
            panel(&mut app, |panel| panel.editor().to_markdown()),
            before,
            "scrolling never edits the note"
        );
        // Build a real short-note fixture. Replacing just the live projection of
        // the long note would be overwritten by its canonical record on sync.
        let mut app = note_at("Fits on screen", 1000, 600, 1.0);
        let (texts, scrollbar) = panel(&mut app, |panel| {
            (
                panel
                    .editor()
                    .blocks()
                    .iter()
                    .map(|block| block.text().to_owned())
                    .collect::<Vec<_>>(),
                panel
                    .accessibility()
                    .unwrap()
                    .children
                    .iter()
                    .any(|node| node.action_target.as_deref() == Some("notes.scroll-position")),
            )
        });
        assert_eq!(texts, ["Fits on screen"]);
        assert!(!scrollbar, "fitting content has no scrollbar");
    }

    eprintln!(
        "phase revealing_a_deep_block_scrolls_its_caret_into_view_and_allows_manual_scrolling"
    );
    {
        let mut app = note_at(&tall_body(), 1000, 400, 1.0);

        let last = panel(&mut app, |panel| panel.editor().blocks().len()) - 1;
        assert!(panel(&mut app, |panel| panel.reveal_block(last)));
        app.harness().relayout_at((1000, 400)).unwrap();
        let (scrolled, caret, bounds) = panel(&mut app, |panel| {
            (
                panel.scroll_y_for_test(),
                panel.editor().ime_cursor_rect().unwrap(),
                panel.get_rect(),
            )
        });
        assert!(scrolled > 0.0, "revealing a deep block must scroll");
        assert!(
            caret.y0 >= bounds.y0 && caret.y1 <= bounds.y1,
            "revealed caret {caret:?} must be inside panel {bounds:?}"
        );

        dispatch(&mut app, &wheel(1000.0));
        app.harness().relayout_at((1000, 400)).unwrap();
        assert_eq!(
            scroll_y(&mut app),
            0.0,
            "manual scrolling must be able to leave the caret behind"
        );

        assert!(panel(&mut app, |panel| panel.reveal_block(last)));
        app.harness().relayout_at((1000, 400)).unwrap();
        assert!(panel(&mut app, |panel| panel.reveal_block(0)));
        app.harness().relayout_at((1000, 400)).unwrap();
        assert_eq!(
            scroll_y(&mut app),
            0.0,
            "revealing the first block must return to the top"
        );

        let event = ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
            None,
            winit::event::ElementState::Pressed,
        );
        let arrow_down = ui::synthetic_input::keyboard_input(event);
        for _ in 0..last {
            app.harness().relayout_at((1000, 400)).unwrap();
            dispatch(&mut app, &arrow_down);
        }
        app.harness().relayout_at((1000, 400)).unwrap();
        let (scrolled, caret, bounds) = panel(&mut app, |panel| {
            (
                panel.scroll_y_for_test(),
                panel.editor().ime_cursor_rect().unwrap(),
                panel.get_rect(),
            )
        });
        assert!(scrolled > 0.0, "keyboard navigation must follow the caret");
        assert!(
            caret.y0 >= bounds.y0 && caret.y1 <= bounds.y1,
            "keyboard caret {caret:?} must stay within panel {bounds:?}"
        );
    }
}

fn scroll_control(app: &mut TestApp<NotesShell>) -> ui::a11y::AccessNode {
    panel(app, |panel| {
        panel
            .accessibility()
            .unwrap()
            .children
            .into_iter()
            .find(|node| node.action_target.as_deref() == Some("notes.scroll-position"))
            .expect("overflowing notes expose their visible scrollbar to accessibility")
    })
}

fn pointer(point: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(point.x, point.y)
}

fn mouse(state: winit::event::ElementState) -> WindowEvent {
    ui::synthetic_input::mouse_input(state, ui::winit::event::MouseButton::Left)
}

#[test]
fn external_primary_region_is_measured_editor_and_matches_wheel() {
    use ui::widget::PrimaryScrollAction;
    let mut app = note_at(&tall_body(), 1000, 300, 1.0);
    let metrics = |app: &mut TestApp<NotesShell>| {
        surface(app, |notes| {
            notes.primary_vertical_scroll().unwrap().metrics
        })
    };
    let m = metrics(&mut app);
    assert!(m.max_scroll() > 0.0);
    let heads = app.peer_document().get_heads();
    surface(&mut app, |notes| {
        notes.apply_primary_vertical_scroll(PrimaryScrollAction::End);
    });
    app.harness().relayout_at((1000, 300)).unwrap();
    let m = metrics(&mut app);
    assert_eq!(m.offset, m.max_scroll());
    assert_eq!(m.offset, scroll_y(&mut app));
    dispatch(&mut app, &wheel(2.0));
    app.harness().relayout_at((1000, 300)).unwrap();
    assert!(metrics(&mut app).offset < m.offset);
    assert_eq!(app.peer_document().get_heads(), heads);
    app.harness().relayout_at((1000, 1200)).unwrap();
    let m = metrics(&mut app);
    assert!(m.offset <= m.max_scroll());
}

/// Enter at the end of a long note in a short window: every new line is
/// revealed with the page's bottom padding under it (one gutter clear of
/// the floating save status), never flush against the window's edge.
#[test]
fn enter_at_the_end_of_a_long_note_keeps_the_caret_clear_of_the_bottom() {
    let theme = Theme::dark();
    let mut app = note_at(&tall_body(), 1000, 400, 1.0);
    let last = panel(&mut app, |panel| panel.editor().blocks().len()) - 1;
    assert!(panel(&mut app, |panel| panel.reveal_block(last)));
    app.harness().relayout_at((1000, 400)).unwrap();
    let key = |named| {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(named),
            None,
            winit::event::ElementState::Pressed,
        ))
    };
    dispatch(&mut app, &key(winit::keyboard::NamedKey::End));
    app.harness().relayout_at((1000, 400)).unwrap();
    let margin = f64::from(theme.tokens.spacing.rhythm.group);
    let blocks = last + 1;
    for press in 1..=8 {
        dispatch(&mut app, &key(winit::keyboard::NamedKey::Enter));
        app.harness().relayout_at((1000, 400)).unwrap();
        let (count, caret, bounds) = panel(&mut app, |panel| {
            (
                panel.editor().blocks().len(),
                panel.editor().ime_cursor_rect().unwrap(),
                panel.get_rect(),
            )
        });
        assert_eq!(count, blocks + press, "Enter {press} adds a line");
        assert!(
            caret.y0 >= bounds.y0,
            "Enter {press}: caret {caret:?} above the panel {bounds:?}"
        );
        assert!(
            caret.y1 + margin <= bounds.y1 + 1.0,
            "Enter {press}: caret {caret:?} must keep {margin}px of the page's bottom \
             padding inside the panel {bounds:?}"
        );
    }
}

/// One touchpad (pixel) wheel event of a two-finger gesture.
fn swipe(phase: winit::event::TouchPhase, dy: f64) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
            0.0, dy,
        )),
        phase,
    }
}

/// Two fingers lifted while moving leave the note gliding, as every other
/// scroll surface does: the body goes on in the frames the glide asks for,
/// slows and stops, and a press on the body catches it.
#[test]
fn a_touchpad_swipe_lifted_while_moving_glides_and_comes_to_rest() {
    use winit::event::TouchPhase::{Ended, Moved, Started};
    let start = ui::time::now();
    let at = |frame: u32| start + ui::time::Duration::from_millis(16 * u64::from(frame));
    let lift = |app: &mut TestApp<NotesShell>| {
        ui::time::testing::with_monotonic_time(start, || {
            app.harness().send(&pointer(Point::new(500.0, 200.0)));
            app.harness().send(&swipe(Started, 0.0));
            for _ in 0..6 {
                app.harness().send(&swipe(Moved, -30.0));
            }
            app.harness().send(&swipe(Ended, 0.0));
        });
    };

    let mut app = note_at(&tall_body(), 1000, 300, 1.0);
    lift(&mut app);
    let lifted = scroll_y(&mut app);
    assert_eq!(lifted, 180.0, "the fingers move the body 1:1");
    let mut frames = 0;
    let mut previous = lifted;
    while frames < 600 && ui::time::testing::with_monotonic_time(at(frames + 1), || app.frame()) {
        frames += 1;
        let now = scroll_y(&mut app);
        assert!(now >= previous, "a glide goes on the way the fingers went");
        previous = now;
    }
    let rest = scroll_y(&mut app);
    assert!(
        rest > lifted + 50.0,
        "lifted while moving, the note glides on: {lifted} -> {rest} over {frames} frames"
    );
    assert!(
        frames < 600,
        "the glide comes to rest and asks for no more frames"
    );

    // A press on the body catches a glide in progress.
    let mut app = note_at(&tall_body(), 1000, 300, 1.0);
    lift(&mut app);
    ui::time::testing::with_monotonic_time(at(1), || app.frame());
    app.harness()
        .send(&mouse(winit::event::ElementState::Pressed));
    app.harness()
        .send(&mouse(winit::event::ElementState::Released));
    let caught = scroll_y(&mut app);
    for frame in 2..20 {
        ui::time::testing::with_monotonic_time(at(frame), || app.frame());
    }
    assert_eq!(scroll_y(&mut app), caught, "a press stops the glide");
}

/// A finger flicked up a touchscreen and lifted while moving leaves the note
/// gliding, as a touchpad swipe does. The lift reaches the body twice, as
/// the scroll's end and as the finger's own contact: the second must not
/// catch the glide the first started. A finger put back down catches it.
#[test]
fn a_touchscreen_flick_lifted_while_moving_glides_and_a_finger_down_catches_it() {
    use winit::event::TouchPhase::{Ended, Moved, Started};
    let start = ui::time::now();
    let at = |frame: u32| start + ui::time::Duration::from_millis(16 * u64::from(frame));
    let flick = |app: &mut TestApp<NotesShell>| {
        ui::time::testing::with_monotonic_time(start, || {
            let harness = app.harness();
            harness.touch(0, Started, 500.0, 280.0);
            for step in 1..=6 {
                harness.touch(0, Moved, 500.0, 280.0 - 30.0 * f64::from(step));
            }
            harness.touch(0, Ended, 500.0, 100.0);
        });
    };

    let mut app = note_at(&tall_body(), 1000, 300, 1.0);
    flick(&mut app);
    let lifted = ui::time::testing::with_monotonic_time(start, || scroll_y(&mut app));
    assert_eq!(lifted, 180.0, "the finger moves the body 1:1");
    for frame in 1..30 {
        ui::time::testing::with_monotonic_time(at(frame), || app.frame());
    }
    let glided = scroll_y(&mut app);
    assert!(
        glided > lifted + 50.0,
        "lifted while moving, the note glides on: {lifted} -> {glided}"
    );

    let mut app = note_at(&tall_body(), 1000, 300, 1.0);
    flick(&mut app);
    ui::time::testing::with_monotonic_time(at(1), || app.frame());
    ui::time::testing::with_monotonic_time(at(1), || {
        app.harness().touch(1, Started, 500.0, 150.0);
    });
    let caught = scroll_y(&mut app);
    for frame in 2..20 {
        ui::time::testing::with_monotonic_time(at(frame), || app.frame());
    }
    assert_eq!(scroll_y(&mut app), caught, "a finger down stops the glide");
}
