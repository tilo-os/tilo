#![allow(
    clippy::needless_borrow,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Drives the responsive sidebar through the real `handle_event` path:
//! a wide window keeps the page tree inline, a narrow window collapses
//! it into a closed overlay drawer, the top-bar button toggles it, and
//! picking a page selects it and auto-closes the drawer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use ui::headless::{FrameOptions, Framed};

use notes::NotesApp;
use notes::sidebar::OutEvent;
use tempfile::TempDir;
use ui::theme::Theme;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

fn cursor_moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

fn mouse_press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

/// Click the shell's sidebar toggle through the production path: the toggle
/// is an action-bound control, which the host runs after the click.
fn click_toggle(app: &mut Framed<NotesApp>) {
    let btn = app.menu_btn_center_for_test();
    Framed::harness_mut(app).click_at(btn.x, btn.y);
    // The page tree joins the layout once the drawer is open.
    Framed::harness_mut(app).relayout();
}

/// Mount `app` over `state` and run the production frame at `w`x`h`.
fn settle_layout(app: NotesApp, state: ui::State, w: u16, h: u16) -> Framed<NotesApp> {
    settle_layout_with_theme(app, state, w, h, &Theme::dark())
}

/// [`settle_layout`] under an explicit theme, so a test can lay the app out
/// at an accessibility text scale.
fn settle_layout_with_theme(
    app: NotesApp,
    state: ui::State,
    w: u16,
    h: u16,
    theme: &Theme,
) -> Framed<NotesApp> {
    Framed::with_options(
        state,
        app,
        (u32::from(w), u32::from(h)),
        FrameOptions {
            theme: Some(theme.clone()),
            ..FrameOptions::default()
        },
    )
}

fn dispatch(app: &mut Framed<NotesApp>, event: &WindowEvent) -> bool {
    Framed::harness_mut(app).dispatch_to_root_with(event, &Modifiers::default())
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn narrow_windows_collapse_the_sidebar_into_a_bounded_drawer() {
    eprintln!("phase narrow_window_collapses_sidebar_into_toggleable_drawer");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let welcome = app.workspace_pages_for_test()[0].id;
        // A second root page so a selection change is observable.
        app.apply_event_for_test(OutEvent::CreateChild { parent: None });
        let other = app.selected_for_test().unwrap();
        assert_ne!(welcome, other);
        app.sync_sidebar_for_test();

        let state = ui::State::new();

        // Wide window: the sidebar stays inline; no top bar / drawer.
        let mut app = settle_layout(app, state, 1280, 720);
        assert!(
            !app.built_narrow_for_test(),
            "1280px window should keep the sidebar inline"
        );

        // Narrow window: collapses to a closed overlay drawer.
        Framed::harness_mut(&mut app)
            .relayout_at((560, 720))
            .unwrap();
        assert!(
            app.built_narrow_for_test(),
            "560px window should collapse the sidebar into the drawer"
        );
        assert!(
            !app.drawer_open_for_test(),
            "the drawer must start closed when a window becomes narrow"
        );

        // The shell's sidebar toggle opens the drawer.
        click_toggle(&mut app);
        assert!(
            app.drawer_open_for_test(),
            "clicking the sidebar toggle must open the drawer"
        );

        // Picking a page from the drawer selects it and closes the drawer.
        assert_eq!(app.selected_for_test(), Some(other));
        let row = app
            .sidebar_row_center_for_test(0)
            .expect("row 0 should be laid out");
        dispatch(&mut app, &cursor_moved(row.x, row.y));
        dispatch(&mut app, &mouse_press());
        assert_eq!(
            app.selected_for_test(),
            Some(welcome),
            "clicking a drawer row must select that page"
        );
        assert!(
            !app.drawer_open_for_test(),
            "selecting a page must auto-close the drawer"
        );
    }

    // At 2× text the scaled 264px drawer would be 528px wide — wider than a
    // 480px phone window. The drawer must clamp to the window and keep a scrim
    // band of the page visible, so its own trailing "+" button stays reachable.
    eprintln!("phase scaled_drawer_stays_inside_a_narrow_window");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        app.sync_sidebar_for_test();
        let state = ui::State::new();
        let theme = Theme::dark().with_text_scale(2.0);
        let window_w: u16 = 480;

        let mut app = settle_layout_with_theme(app, state, window_w, 900, &theme);
        assert!(
            app.built_narrow_for_test(),
            "480px at 2× is a drawer layout"
        );
        click_toggle(&mut app);
        assert!(app.drawer_open_for_test(), "the toggle opens the drawer");
        // The open drawer's structure is laid out on the next settled pass.
        Framed::harness_mut(&mut app)
            .relayout_themed((u32::from(window_w), 900), theme.clone())
            .unwrap();

        let drawer = app.sidebar_rect_for_test();
        let reveal = f64::from(theme.tokens.spacing.s12);
        assert!(
            drawer.x1 <= f64::from(window_w) - reveal + 0.5,
            "drawer must leave a {reveal}px band of the page visible, got right edge {}",
            drawer.x1
        );
        assert!(
            drawer.width() > 264.0,
            "the clamp must not undo the text-scale growth entirely: {drawer:?}"
        );
        let plus = app.sidebar_new_page_btn_center_for_test();
        assert!(
            plus.x < drawer.x1 && plus.x < f64::from(window_w),
            "the drawer's own new-page button must stay on screen: {plus:?} in {drawer:?}"
        );
    }
}
