use super::*;

#[test]
fn notification_center_enlarged_header_sizes_from_the_actual_native_width() {
    let mut h = build_harness_sized(640.0, 800.0);
    {
        let mut state = h.chrome.borrow_mut();
        for index in 0..3 {
            let id = state.push_notification_full(tilo_desktop::notifications::Notification::new(
                format!("Recovery {index}"),
                "Try again.",
            ));
            state.dismiss_notification(id);
        }
        state.overlays.quick_settings = true;
        state
            .quick_settings
            .push_level(crate::shell::quick_settings::QsLevel::Notifications);
        state.bump();
    }
    h.host.force_quick_settings_settled();
    settle(&mut h, 640.0, 800.0);
    h.host.qs_type_lines = tilo_desktop::notifications::CenterLayout::metrics(
        &Theme::dark().with_text_scale(2.0),
        &mut TextRenderer::new(),
    );
    let layout = h.host.qs_notification_layout();
    // Quick Settings is a list Panel: two tiles side by side keep their names.
    assert_eq!(
        layout.panel().width(),
        f64::from(ui::tokens::Metrics::BASE.panel_width_list)
    );
    {
        let chrome = h.host.chrome.as_ref().expect("the harness has chrome");
        let notifications = ChromeNotifications::new(chrome);
        let center = &h.host.notification_center;
        assert_eq!(center.visible_rows(&notifications, &layout), 3);
        let last = center
            .semantic_rects(&notifications, &layout, "")
            .into_iter()
            .find(|(name, _)| name == "notification_row[2]")
            .expect("the last row shows")
            .1;
        assert!(
            (last.y1 + qs::PANEL_PAD - layout.panel().y1).abs() < 1e-6,
            "the native sizing caller must reserve the wrapped header before counting rows"
        );
    }
    h.host.handle_shell_quick_settings_drill_in_click(
        layout.clear().center(),
        &crate::shell::quick_settings::QsLevel::Notifications,
    );
    assert!(h.chrome.borrow().notification_history().is_empty());
    let cleared = h.host.qs_notification_layout();
    h.host.handle_shell_quick_settings_drill_in_click(
        cleared.undo().center(),
        &crate::shell::quick_settings::QsLevel::Notifications,
    );
    assert_eq!(h.chrome.borrow().notification_history().len(), 3);
    h.host.handle_shell_quick_settings_drill_in_click(
        layout.back().center(),
        &crate::shell::quick_settings::QsLevel::Notifications,
    );
    assert!(matches!(
        h.chrome.borrow().quick_settings.current_level(),
        crate::shell::quick_settings::QsLevel::Root
    ));
}
