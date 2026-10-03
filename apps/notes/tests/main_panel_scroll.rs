//! Drives `MainPanel` vertical scrolling through the real
//! `NotesApp::handle_event` → `MouseWheel` path plus the `apply_layout`
//! overflow clamp, on a live settled layout. Guards the hand-rolled
//! `scroll_y` math (wheel-to-px conversion, top clamp, content-extent
//! clamp) that has no test today.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::float_cmp,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;
use ui::headless::{FrameOptions, Framed};

use notes::NotesApp;
use tempfile::TempDir;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{Modifiers, MouseScrollDelta, TouchPhase, WindowEvent};

fn wheel(lines: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: unsafe { std::mem::zeroed() },
        delta: MouseScrollDelta::LineDelta(0.0, lines),
        phase: TouchPhase::Moved,
    }
}

/// A tall body so the panel content overflows any short viewport.
fn tall_body() -> String {
    let mut s = String::from("# Long page\n\n");
    for i in 0..60 {
        let _ = writeln!(s, "Paragraph number {i} with some words.\n");
    }
    s
}

fn settle(app: NotesApp, state: ui::State, w: u16, h: u16) -> Framed<NotesApp> {
    settle_scaled(app, state, w, h, 1.0)
}

fn settle_scaled(app: NotesApp, state: ui::State, w: u16, h: u16, scale: f32) -> Framed<NotesApp> {
    Framed::with_options(
        state,
        app,
        (u32::from(w), u32::from(h)),
        FrameOptions {
            theme: Some(Theme::dark().with_text_scale(scale)),
            text: Some(crate::component_support::renderer()),
            ..FrameOptions::default()
        },
    )
}

fn dispatch(app: &mut Framed<NotesApp>, event: &WindowEvent) {
    Framed::harness_mut(app).dispatch_to_root_with(event, &Modifiers::default());
}

#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end scrollbar test verifies pointer, keyboard, semantic, and reload paths"
)]
#[test]
fn note_scrolling_clamps_reveals_and_drags_through_the_real_scrollbar() {
    eprintln!("phase mouse_wheel_scrolls_and_clamps_to_content");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let id = app.selected_for_test().unwrap();
        let body = tall_body();
        notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
        app.main_panel_mut().load_page(id, "Long page", &body);
        // Complete the normal legacy origin bind before measuring wheel behavior.
        // Otherwise its delayed first bind replaces the panel midway through this test.
        app.bypass_seed_grace_for_test();

        let state = ui::State::new();
        // Wide (stays inline) but short so the content overflows.
        let mut app = settle(app, state, 1000, 300);
        assert_eq!(
            app.main_panel_mut().scroll_y_for_test(),
            0.0,
            "starts at top"
        );

        // Scroll down: negative LineDelta y → positive px offset.
        dispatch(&mut app, &wheel(-3.0));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 300))
            .unwrap();
        let scrolled = app.main_panel_mut().scroll_y_for_test();
        assert!(
            scrolled > 0.0,
            "wheel-down must advance scroll_y; got {scrolled}"
        );

        // Scroll far past the bottom: scroll_y must clamp to the content
        // extent — a second huge scroll can't push it further.
        dispatch(&mut app, &wheel(-1000.0));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 300))
            .unwrap();
        let at_max = app.main_panel_mut().scroll_y_for_test();
        dispatch(&mut app, &wheel(-1000.0));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 300))
            .unwrap();
        let still_max = app.main_panel_mut().scroll_y_for_test();
        assert!(
            at_max > scrolled,
            "scrolling further must increase offset up to the clamp"
        );
        assert_eq!(
            at_max, still_max,
            "scroll_y must clamp to the content extent (can't exceed it)"
        );
        let panel = app.main_panel_mut();
        let gutter = f64::from(Theme::dark().tokens.spacing.rhythm.group);
        assert!(
            panel.editor().content_bottom() <= panel.get_rect().y1 - gutter + 1.0,
            "the final row must end a gutter above the window's bottom edge"
        );

        // Scroll back up past the top: clamps at 0.
        dispatch(&mut app, &wheel(1000.0));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 300))
            .unwrap();
        assert_eq!(
            app.main_panel_mut().scroll_y_for_test(),
            0.0,
            "wheel-up past the top must clamp scroll_y to 0"
        );
    }

    eprintln!(
        "phase bottom_scroll_leaves_one_gutter_under_the_last_row_and_the_page_runs_to_the_window_bottom"
    );
    {
        for scale in [1.0, 2.0] {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            let id = app.selected_for_test().unwrap();
            let body = tall_body();
            notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
            app.main_panel_mut().load_page(id, "Long note", &body);
            app.bypass_seed_grace_for_test();
            let state = ui::State::new();
            let mut app = settle_scaled(app, state, 1000, 1000, scale);
            dispatch(&mut app, &wheel(-1000.0));
            Framed::harness_mut(&mut app)
                .relayout_themed((1000, 1000), Theme::dark().with_text_scale(scale))
                .unwrap();
            // The note scrolls to the window's bottom edge; the transient
            // status floats over it instead of reserving a strip.
            let panel_bottom = app.main_panel_mut().get_rect().y1;
            assert!(
                (panel_bottom - app.get_rect().y1).abs() <= 1.0,
                "at {scale}x, the note panel must reach the window's bottom edge"
            );
            let content_bottom = app.main_panel_mut().editor().content_bottom();
            let gap = panel_bottom - content_bottom;
            let theme = Theme::dark().with_text_scale(scale);
            assert!(
                (gap - f64::from(theme.tokens.spacing.rhythm.group)).abs() <= 1.0,
                "at {scale}x, last row ends at {content_bottom} of {panel_bottom}: {gap}px is not one gutter"
            );
        }
    }

    eprintln!("phase note_scrollbar_drags_pages_and_exposes_the_same_position_semantically");
    {
        use ui::a11y::Action;
        use winit::event::ElementState::{Pressed, Released};
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let id = app.selected_for_test().unwrap();
        let body = tall_body();
        notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
        app.main_panel_mut().load_page(id, "Long note", &body);
        app.bypass_seed_grace_for_test();
        let state = ui::State::new();
        let mut app = settle(app, state, 1000, 600);
        let before = app.main_panel_mut().editor().to_markdown();
        let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
        assert_eq!(scroll_control(&mut app).value.as_deref(), Some("0"));
        dispatch(&mut app, &pointer(thumb.center()));
        dispatch(&mut app, &mouse(Pressed));
        dispatch(
            &mut app,
            &pointer(Point::new(thumb.center().x, thumb.center().y + 100.0)),
        );
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 600))
            .unwrap();
        assert!(
            app.main_panel_mut().scroll_y_for_test() > 0.0,
            "drag must move the note"
        );
        assert!(scroll_control(&mut app).bounds.unwrap().y0 > thumb.y0);
        dispatch(&mut app, &mouse(Released));
        let scrolled = app.main_panel_mut().scroll_y_for_test();
        dispatch(&mut app, &pointer(Point::new(thumb.center().x, 10.0)));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 600))
            .unwrap();
        assert_eq!(
            app.main_panel_mut().scroll_y_for_test(),
            scrolled,
            "release ends the drag"
        );

        let root = Framed::harness(&app).state().root.clone();
        let (notes, state) = Framed::split(&mut app);
        assert!(notes.main_panel_mut().perform_semantic_action(
            "notes.scroll-position",
            &Action::SetValue("100".into()),
            &mut state.doc,
            &root
        ));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 600))
            .unwrap();
        assert_eq!(scroll_control(&mut app).value.as_deref(), Some("100"));
        let last = app
            .main_panel_mut()
            .editor()
            .blocks()
            .last()
            .unwrap()
            .line
            .get_rect();
        assert!(
            last.y1 < app.main_panel_mut().get_rect().y1,
            "semantic scroll reveals the last line"
        );
        let bottom = app.main_panel_mut().scroll_y_for_test();
        let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
        dispatch(
            &mut app,
            &pointer(Point::new(thumb.center().x, thumb.y0 - 20.0)),
        );
        dispatch(&mut app, &mouse(Pressed));
        dispatch(&mut app, &mouse(Released));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 600))
            .unwrap();
        assert!(
            app.main_panel_mut().scroll_y_for_test() < bottom,
            "track click pages upward"
        );
        let (notes, state) = Framed::split(&mut app);
        assert!(!notes.main_panel_mut().perform_semantic_action(
            "notes.scroll-position",
            &Action::SetValue("NaN".into()),
            &mut state.doc,
            &root
        ));
        let (notes, state) = Framed::split(&mut app);
        assert!(notes.main_panel_mut().perform_semantic_action(
            "notes.scroll-position",
            &Action::Focus,
            &mut state.doc,
            &root
        ));
        let key = |key| {
            let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            event.logical_key = winit::keyboard::Key::Named(key);
            event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            event.location = winit::keyboard::KeyLocation::Standard;
            event.state = Pressed;
            event.text = None;
            WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event,
                is_synthetic: false,
            }
        };
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 600))
            .unwrap();
        dispatch(&mut app, &key(winit::keyboard::NamedKey::Home));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 600))
            .unwrap();
        assert_eq!(app.main_panel_mut().scroll_y_for_test(), 0.0);
        dispatch(&mut app, &key(winit::keyboard::NamedKey::PageDown));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 600))
            .unwrap();
        assert!(app.main_panel_mut().scroll_y_for_test() > 0.0);
        let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
        dispatch(&mut app, &pointer(thumb.center()));
        dispatch(&mut app, &mouse(Pressed));
        dispatch(&mut app, &WindowEvent::Focused(false));
        let stopped = app.main_panel_mut().scroll_y_for_test();
        dispatch(&mut app, &pointer(Point::new(thumb.center().x, 500.0)));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 600))
            .unwrap();
        assert_eq!(
            app.main_panel_mut().scroll_y_for_test(),
            stopped,
            "focus loss cancels dragging"
        );
        assert_eq!(
            app.main_panel_mut().editor().to_markdown(),
            before,
            "scrolling never edits the note"
        );
        // Build a real short-note fixture. Replacing just the live projection of
        // the long note would be overwritten by its canonical record on sync.
        let short = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(short.path().into()).unwrap();
        let id = app.selected_for_test().unwrap();
        notes::storage::write_page_body(short.path(), id, "Fits on screen").unwrap();
        app.main_panel_mut()
            .load_page(id, "Short note", "Fits on screen");
        app.bypass_seed_grace_for_test();
        let state = ui::State::new();
        let mut app = settle(app, state, 1000, 600);
        assert_eq!(app.main_panel_mut().editor().blocks().len(), 1);
        assert_eq!(
            app.main_panel_mut().editor().blocks()[0].text(),
            "Fits on screen"
        );
        assert!(
            !app.main_panel_mut()
                .accessibility()
                .unwrap()
                .children
                .iter()
                .any(|node| node.action_target.as_deref() == Some("notes.scroll-position")),
            "fitting content has no scrollbar"
        );
    }

    eprintln!(
        "phase revealing_a_deep_block_scrolls_its_caret_into_view_and_allows_manual_scrolling"
    );
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let id = app.selected_for_test().unwrap();
        let body = tall_body();
        notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
        app.main_panel_mut().load_page(id, "Long note", &body);
        app.bypass_seed_grace_for_test();
        let state = ui::State::new();
        let mut app = settle(app, state, 1000, 400);

        let last = app.main_panel_mut().editor().blocks().len() - 1;
        assert!(app.main_panel_mut().reveal_block(last));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 400))
            .unwrap();
        let panel = app.main_panel_mut();
        assert!(
            panel.scroll_y_for_test() > 0.0,
            "revealing a deep block must scroll"
        );
        let caret = panel.editor().ime_cursor_rect().unwrap();
        assert!(
            caret.y0 >= panel.get_rect().y0 && caret.y1 <= panel.get_rect().y1,
            "revealed caret {caret:?} must be inside panel {:?}",
            panel.get_rect()
        );

        dispatch(&mut app, &wheel(1000.0));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 400))
            .unwrap();
        assert_eq!(
            app.main_panel_mut().scroll_y_for_test(),
            0.0,
            "manual scrolling must be able to leave the caret behind"
        );

        assert!(app.main_panel_mut().reveal_block(last));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 400))
            .unwrap();
        assert!(app.main_panel_mut().reveal_block(0));
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 400))
            .unwrap();
        assert_eq!(
            app.main_panel_mut().scroll_y_for_test(),
            0.0,
            "revealing the first block must return to the top"
        );

        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown);
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = winit::event::ElementState::Pressed;
        event.text = None;
        let arrow_down = WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        };
        for _ in 0..last {
            Framed::harness_mut(&mut app)
                .relayout_at((1000, 400))
                .unwrap();
            dispatch(&mut app, &arrow_down);
        }
        Framed::harness_mut(&mut app)
            .relayout_at((1000, 400))
            .unwrap();
        let panel = app.main_panel_mut();
        assert!(
            panel.scroll_y_for_test() > 0.0,
            "keyboard navigation must follow the caret"
        );
        let caret = panel.editor().ime_cursor_rect().unwrap();
        assert!(
            caret.y0 >= panel.get_rect().y0 && caret.y1 <= panel.get_rect().y1,
            "keyboard caret {caret:?} must stay within panel {:?}",
            panel.get_rect()
        );
    }
}

fn scroll_control(app: &mut NotesApp) -> ui::a11y::AccessNode {
    app.main_panel_mut()
        .accessibility()
        .unwrap()
        .children
        .into_iter()
        .find(|node| node.action_target.as_deref() == Some("notes.scroll-position"))
        .expect("overflowing notes expose their visible scrollbar to accessibility")
}

fn pointer(point: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    }
}

fn mouse(state: winit::event::ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state,
        button: winit::event::MouseButton::Left,
    }
}

#[test]
fn external_primary_region_is_measured_editor_and_matches_wheel() {
    use ui::widget::PrimaryScrollAction;
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let id = app.selected_for_test().unwrap();
    let body = tall_body();
    notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
    app.main_panel_mut().load_page(id, "Long page", body);
    app.bypass_seed_grace_for_test();
    let state = ui::State::new();
    let mut app = settle(app, state, 1000, 300);
    let m = app.primary_vertical_scroll().unwrap().metrics;
    assert!(m.max_scroll() > 0.0);
    let heads = Framed::split(&mut app).1.doc.get_heads();
    app.apply_primary_vertical_scroll(PrimaryScrollAction::End);
    Framed::harness_mut(&mut app)
        .relayout_at((1000, 300))
        .unwrap();
    let m = app.primary_vertical_scroll().unwrap().metrics;
    assert_eq!(m.offset, m.max_scroll());
    assert_eq!(m.offset, app.main_panel_mut().scroll_y_for_test());
    dispatch(&mut app, &wheel(2.0));
    Framed::harness_mut(&mut app)
        .relayout_at((1000, 300))
        .unwrap();
    assert!(app.primary_vertical_scroll().unwrap().metrics.offset < m.offset);
    assert_eq!(Framed::split(&mut app).1.doc.get_heads(), heads);
    Framed::harness_mut(&mut app)
        .relayout_at((1000, 1200))
        .unwrap();
    let m = app.primary_vertical_scroll().unwrap().metrics;
    assert!(m.offset <= m.max_scroll());
}
