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
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::framework_support::{add_note, notes, open, send as dispatch, surface};
use ui::theme::Theme;
use winit::event::{ElementState, WindowEvent};

use ui::synthetic_input::cursor_moved;

use ui::synthetic_input::mouse_press;

#[test]
fn narrow_windows_collapse_the_sidebar_into_a_bounded_drawer() {
    eprintln!("phase narrow_window_collapses_sidebar_into_toggleable_drawer");
    {
        let mut app = notes();
        let welcome = add_note(&mut app, "Welcome", "");
        // A second root page so a selection change is observable.
        let other = add_note(&mut app, "Other", "");
        open(&mut app, other);

        // Wide window: the sidebar stays inline; no top bar / drawer.
        app.harness().relayout_at((1280, 720)).unwrap();
        assert!(
            !surface(&mut app, |notes| notes.built_narrow_for_test()),
            "1280px window should keep the sidebar inline"
        );

        // Narrow window: collapses to a closed overlay drawer.
        app.harness().relayout_at((480, 720)).unwrap();
        assert!(
            surface(&mut app, |notes| notes.built_narrow_for_test()),
            "480px window should collapse the sidebar into the drawer"
        );
        assert!(
            !surface(&mut app, |notes| notes.drawer_open_for_test()),
            "the drawer must start closed when a window becomes narrow"
        );

        let btn = surface(&mut app, |notes| notes.menu_btn_center_for_test());
        app.harness().click_at(btn.x, btn.y);
        assert!(
            app.harness().control_failures().is_empty(),
            "{:?}",
            app.harness().control_failures()
        );
        assert!(
            surface(&mut app, |notes| notes.drawer_open_for_test()),
            "clicking the top-bar button must open the drawer"
        );

        assert_eq!(
            surface(&mut app, |notes| notes.selected_for_test()),
            Some(other)
        );
        let row = surface(&mut app, |notes| notes.sidebar_row_center_for_test(0))
            .expect("row 0 should be laid out");
        dispatch(&mut app, &cursor_moved(row.x, row.y));
        dispatch(&mut app, &mouse_press());
        let mut release = mouse_press();
        if let WindowEvent::MouseInput { state, .. } = &mut release {
            *state = ElementState::Released;
        }
        dispatch(&mut app, &release);
        assert_eq!(
            surface(&mut app, |notes| notes.selected_for_test()),
            Some(welcome),
            "clicking a drawer row must select that page"
        );
        assert!(
            !surface(&mut app, |notes| notes.drawer_open_for_test()),
            "selecting a page must auto-close the drawer"
        );
    }

    check_scaled_drawer_stays_inside_a_narrow_window();
}

fn check_scaled_drawer_stays_inside_a_narrow_window() {
    // At 2× text the scaled 264px drawer would be 528px wide — wider than a
    // 480px phone window. The drawer must clamp to the window and keep a scrim
    // band of the page visible, so its own trailing "+" button stays reachable.
    eprintln!("phase scaled_drawer_stays_inside_a_narrow_window");
    {
        let mut app = notes();
        let welcome = add_note(&mut app, "Welcome", "");
        open(&mut app, welcome);
        let theme = Theme::dark().with_text_scale(2.0);
        let window_w: u16 = 480;
        app.harness()
            .relayout_themed((u32::from(window_w), 900), theme.clone())
            .unwrap();
        assert!(
            surface(&mut app, |notes| notes.built_narrow_for_test()),
            "480px at 2× is a drawer layout"
        );
        let btn = surface(&mut app, |notes| notes.menu_btn_center_for_test());
        app.harness().click_at(btn.x, btn.y);
        assert!(
            app.harness().control_failures().is_empty(),
            "{:?}",
            app.harness().control_failures()
        );
        assert!(
            surface(&mut app, |notes| notes.drawer_open_for_test()),
            "the toggle opens the drawer"
        );
        app.harness()
            .relayout_themed((u32::from(window_w), 900), theme.clone())
            .unwrap();
        assert!(
            app.harness()
                .settle_for_capture(tilo_test_env::hang_bound()),
            "the drawer's opening animation and the new note's save must settle before \
             measuring its controls"
        );
        // A surface visit advances the following frame. Sample both parts
        // together so the bounds and control describe the same layout.
        let (drawer, plus) = surface(&mut app, |notes| {
            (
                notes.sidebar_rect_for_test(),
                notes.sidebar_new_page_btn_center_for_test(),
            )
        });
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
        assert!(
            plus.x < drawer.x1 && plus.x < f64::from(window_w),
            "the drawer's own new-page button must stay on screen: {plus:?} in {drawer:?}"
        );
    }
}
