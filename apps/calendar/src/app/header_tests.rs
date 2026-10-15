//! Exercise the real resource frame: it must not insert another History row.
use super::*;
use ui::framework::testing::TestApp;
use ui::widget::Edits;

#[test]
fn drawer_dismissal_does_not_activate_the_history_control_behind_it() {
    use ui::headless::Framed;
    use ui::synthetic_input::{cursor_moved, mouse_press, mouse_release};
    let mut root = CalendarApp::new_demo();
    root.shots_show_calendar();
    root.topbar
        .history
        .set_control(Some(ui::widget::HistoryControl::Closed));
    root.sidebar.set_open(true);
    let doc = super::tests::demo_document(&root);
    let mut frame = Framed::new(ui::State::from_doc(doc), root, (800, 810), Theme::dark());
    assert_eq!(frame.sidebar.mode(), SidebarMode::Overlay);
    frame.sidebar.set_open(true);
    Framed::harness_mut(&mut frame)
        .recompose()
        .expect("drawer opens");
    assert!(frame.sidebar.is_open());
    let point = frame.topbar.history_rect(&frame.layout_theme).center();
    for event in [
        cursor_moved(point.x, point.y),
        mouse_press(),
        mouse_release(),
    ] {
        Framed::harness_mut(&mut frame)
            .dispatch_to_root_with(&event, &winit::event::Modifiers::default());
    }
    assert!(!frame.sidebar.is_open());
    assert!(!frame.topbar.history.is_pending());
}

#[test]
fn scaled_resource_window_has_one_header_and_an_unscrolled_calendar_sidebar() {
    for theme in [Theme::dark(), Theme::light()] {
        let mut frame = TestApp::<crate::CalendarShell>::capture_at(Some("agenda"), (1440, 810))
            .expect("calendar resource frame");
        frame.harness().set_theme(theme.with_ui_scale(1.5));
        frame.harness().relayout();
        let app = frame.surface_mut::<CalendarApp>();
        assert_eq!(
            app.rect.y0, 0.0,
            "no host-only History strip above Calendar"
        );
        assert!(app.topbar.history.is_shown());
        assert!(app.topbar.switcher_visible());
        let history = app.topbar.history_rect(&app.layout_theme);
        assert!(history.y1 <= app.view_rect().y0);
        let mut views = Vec::new();
        let mut month_bottom = 0.0_f64;
        let mut calendars_top = f64::INFINITY;
        let mut calendars = 0;
        for (rect, action) in &app.hits {
            match action {
                Action::View(view) => {
                    assert!(rect.y1 <= app.view_rect().y0);
                    views.push(*view);
                }
                Action::MiniMonth(_) => month_bottom = month_bottom.max(rect.y1),
                Action::SetCalendarVisible { .. } => {
                    calendars += 1;
                    calendars_top = calendars_top.min(rect.y0);
                    assert!(app.sidebar.rect().contains_rect(*rect));
                }
                _ => {}
            }
        }
        assert_eq!(views.len(), View::ALL.len());
        assert_eq!(calendars, 4);
        assert!(month_bottom > app.view_rect().y0);
        assert!(
            month_bottom <= calendars_top,
            "mini-month precedes calendars"
        );
        assert_eq!(app.sidebar_scroll.offset_y(), 0.0);
        assert!(app.sidebar_controls().iter().all(|(rect, action)| {
            !matches!(action, Action::SetCalendarVisible { .. })
                || app.sidebar.rect().contains_rect(*rect)
        }));
        assert!(app.perform_semantic_action(
            ui::widget::HISTORY_CONTROL_ID,
            &A11yAction::Activate,
            &mut Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert!(ui::widget::controls::ActionControls::take_control_invocation(app).is_some());
    }
}
