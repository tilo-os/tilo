use super::*;
#[test]
fn enlarged_notification_header_keeps_the_title_readable_and_actions_separate() {
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let theme = theme.with_text_scale(2.0);
        let mut text = TextRenderer::new();
        let panel = Rect::new(0.0, 0.0, 320.0, 560.0);
        let layout = CenterLayout::new(panel, CenterLayout::metrics(&theme, &mut text));
        let title = text.measure_style(
            &t("notification-center-title"),
            None,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
        );
        let inline_width = layout.clear().x0 - layout.back().x1 - 2.0 * qs::RELATED_GAP;
        if inline_width < f64::from(title.0) {
            assert!(
                layout.header_height()
                    >= layout.actions.height + f64::from(title.1) + 3.0 * qs::RELATED_GAP,
                "the production header must reserve a separate readable title band"
            );
        }
        assert!(layout.back().x1 + qs::RELATED_GAP <= layout.clear().x0);
        assert!(layout.position_y() >= layout.back().y1 + qs::RELATED_GAP);
    }
}

#[test]
fn wrapped_header_sizing_preserves_the_complete_row_and_pager_budget() {
    let theme = Theme::dark().with_text_scale(2.0);
    let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
    for (rows, total) in [(0usize, 0), (3, 3), (7, 18)] {
        let host = super::super::stack::tests::TestHost::default();
        for _ in 0..total {
            host.log
                .borrow_mut()
                .retain(Notification::new("Recovery", "Try again."));
        }
        let height =
            NotificationCenter::new().preferred_height_for_width(&host.log(), metrics, 320.0);
        let layout = CenterLayout::new(Rect::new(0.0, 0.0, 320.0, height), metrics);
        assert_eq!(layout.visible_rows(total), rows.max(1));
        assert!(layout.title_rect().y0 >= layout.back().y1 + qs::RELATED_GAP);
        assert!(layout.title_rect().y1 + qs::RELATED_GAP <= layout.position_y());
        if rows > 0 {
            let bottom = if total > rows {
                layout.footer().y0 - qs::RELATED_GAP
            } else {
                height - qs::PANEL_PAD
            };
            assert!((layout.row(rows.saturating_sub(1)).y1 - bottom).abs() < 1e-6);
        }
    }
}

#[test]
fn normal_header_retains_its_single_band_geometry() {
    let theme = Theme::dark();
    let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
    let panel = Rect::new(0.0, 0.0, 400.0, 560.0);
    let layout = CenterLayout::new(panel, metrics);
    assert!(!layout.wrapped_header());
    assert_eq!(
        layout.header_height(),
        metrics.height + 2.0 * qs::RELATED_GAP
    );
    let host = super::super::stack::tests::TestHost::default();
    let center = NotificationCenter::new();
    assert_eq!(
        center.preferred_height_for_width(&host.log(), metrics, 400.0),
        center.preferred_height(&host.log(), metrics)
    );
}

#[test]
fn narrow_header_keeps_clear_undo_and_nested_back_journeys() {
    use super::super::stack::tests::TestHost;
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let theme = theme.with_text_scale(2.0);
        let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
        let host = TestHost::default();
        host.log
            .borrow_mut()
            .retain(Notification::new("Recovery required", "Try again."));
        let mut center = NotificationCenter::new();
        for _ in 0..2 {
            let height = center.preferred_height_for_width(&host.log(), metrics, 320.0);
            let layout = CenterLayout::new(Rect::new(0.0, 0.0, 320.0, height), metrics);
            assert_eq!(
                center.click(&host, &layout, layout.clear().center()),
                CenterOutcome::Handled
            );
            assert!(host.log().history().is_empty());
            let height = center.preferred_height_for_width(&host.log(), metrics, 320.0);
            let cleared = CenterLayout::new(Rect::new(0.0, 0.0, 320.0, height), metrics);
            assert!(center.focus_accessibility_path(&host, &[2]));
            assert_eq!(
                center.key(&host, &cleared, &Key::Named(NamedKey::Enter)),
                CenterOutcome::Handled
            );
            assert_eq!(host.log().history().len(), 1);
            let dialog = center.accessibility(&host, &layout, ui::time::now());
            let back = dialog
                .children
                .iter()
                .find(|n| n.name.as_deref() == Some(t("drill-in-back").as_str()))
                .unwrap();
            assert_eq!(back.bounds.unwrap().to_rect(), layout.back());
            let back_path = [dialog.children.len().saturating_sub(1)];
            assert_eq!(
                center.accessibility_rect_for_path(&host, &layout, &back_path),
                Some(layout.back())
            );
            assert!(center.focus_accessibility_path(&host, &back_path));
            assert_eq!(
                center.click(
                    &host,
                    &layout,
                    Point::new(layout.panel().center().x, layout.panel().y0 + 1.0)
                ),
                CenterOutcome::Ignored
            );
            assert!(
                center.back_focused,
                "an ignored input must not silently retire focus"
            );
            assert_eq!(
                center.key(&host, &layout, &Key::Named(NamedKey::Enter)),
                CenterOutcome::Back
            );
            assert_eq!(
                center.click(
                    &host,
                    &layout,
                    layout.read_control(layout.row(0), false).center()
                ),
                CenterOutcome::Handled
            );
            assert!(center.is_reading());
            assert_eq!(
                center.key(&host, &layout, &Key::Named(NamedKey::Escape)),
                CenterOutcome::Handled
            );
            assert!(!center.is_reading());
            assert!(
                !center.back_focused,
                "a pointer choice must retire the old Back focus"
            );
            assert_eq!(
                center.key(&host, &layout, &Key::Named(NamedKey::Escape)),
                CenterOutcome::Back
            );
        }
    }
}

#[test]
fn enlarged_clear_status_fits_above_undo_instead_of_wrapping_out_of_the_panel() {
    let theme = Theme::high_contrast().with_text_scale(2.0);
    let mut text = TextRenderer::new();
    let layout = CenterLayout::new(
        Rect::new(0.0, 0.0, 320.0, 560.0),
        CenterLayout::metrics(&theme, &mut text),
    );
    let label = layout.cleared_label_rect();
    let measured = text.measure_style(
        &t("notification-center-cleared"),
        None,
        &theme.text(TextRole::Caption),
    );
    assert!(f64::from(measured.0) <= label.width());
    assert!(label.y1 + qs::RELATED_GAP <= layout.undo().y0);
    assert!(layout.cleared_band().y1 <= layout.panel().y1);
}
