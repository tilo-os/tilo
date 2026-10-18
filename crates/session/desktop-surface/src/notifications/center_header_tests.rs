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
        assert!(layout.rows_top() >= layout.back().y1 + qs::RELATED_GAP);
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
        let center = NotificationCenter::new();
        let height = center.preferred_height_for_width(&host, &metrics, 320.0);
        let layout = CenterLayout::new(Rect::new(0.0, 0.0, 320.0, height), &metrics);
        let shapes = NotificationCenter::shapes(&host, &layout, &center.rows(&host));
        assert_eq!(center.visible_rows(&host, &layout), rows.max(1));
        assert!(layout.title_rect().y0 >= layout.back().y1 + qs::RELATED_GAP);
        assert!(layout.title_rect().y1 + qs::RELATED_GAP <= layout.rows_top());
        if rows > 0 {
            let bottom = if total > rows {
                layout.footer().y0 - qs::RELATED_GAP
            } else {
                height - qs::PANEL_PAD
            };
            assert!((layout.row_in(rows.saturating_sub(1), &shapes).y1 - bottom).abs() < 1e-6);
        }
    }
}

#[test]
fn normal_header_retains_its_single_band_geometry() {
    let theme = Theme::dark();
    let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
    let panel = Rect::new(0.0, 0.0, 400.0, 560.0);
    let layout = CenterLayout::new(panel, &metrics);
    assert!(!layout.wrapped_header());
    assert_eq!(
        layout.header_height(),
        metrics.height + 2.0 * qs::RELATED_GAP
    );
    let host = super::super::stack::tests::TestHost::default();
    let center = NotificationCenter::new();
    assert_eq!(
        center.preferred_height_for_width(&host, &metrics, 400.0),
        center.preferred_height(&host, &metrics)
    );
}

/// Do Not Disturb is stated where the person looks for what they missed:
/// a status line under the centre's header, which pushes the rows down
/// rather than covering them, and the first thing a reader hears.
#[test]
fn quiet_mode_is_a_status_line_under_the_header() {
    for theme in [Theme::light(), Theme::dark().with_text_scale(2.0)] {
        let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
        let host = super::super::stack::tests::TestHost::default();
        host.log
            .borrow_mut()
            .retain(Notification::new("Held", "While you were busy"));
        let center = NotificationCenter::new();
        let loud = center.preferred_height_for_width(&host, &metrics, 360.0);
        host.quiet.replace(Some("Do Not Disturb".into()));
        let quiet_height = center.preferred_height_for_width(&host, &metrics, 360.0);
        assert!(quiet_height > loud, "the status line takes its own room");
        let panel = Rect::new(0.0, 0.0, 360.0, quiet_height);
        let layout = CenterLayout::new(panel, &metrics).quiet(true);
        let status = layout.quiet_rect();
        assert!(status.height() > 0.0);
        assert!(status.y0 >= layout.back().y1);
        assert!(status.y0 >= layout.title_rect().y1);
        assert!(status.y1 <= layout.rows_top());
        assert_eq!(
            CenterLayout::new(panel, &metrics).quiet_rect().height(),
            0.0
        );
        let node = center.accessibility(&host, &layout, ui::time::now());
        assert!(
            node.description
                .as_deref()
                .is_some_and(|text| text.starts_with("Do Not Disturb. ")),
            "{:?}",
            node.description
        );
    }
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
            let height = center.preferred_height_for_width(&host, &metrics, 320.0);
            let layout = CenterLayout::new(Rect::new(0.0, 0.0, 320.0, height), &metrics);
            assert_eq!(
                center.click(&host, &layout, layout.clear().center()),
                CenterOutcome::Handled
            );
            assert!(host.log().history().is_empty());
            let height = center.preferred_height_for_width(&host, &metrics, 320.0);
            let cleared = CenterLayout::new(Rect::new(0.0, 0.0, 320.0, height), &metrics);
            assert!(center.focus_accessibility_path(&host, &layout, &[2]));
            assert_eq!(
                center.key(&host, &cleared, &Key::Named(NamedKey::Enter)),
                CenterOutcome::Handled
            );
            assert_eq!(host.log().history().len(), 1);
            let dialog = center.accessibility(&host, &layout, ui::time::now());
            let back = dialog
                .children
                .iter()
                .find(|n| n.name.as_deref() == Some(t("notification-center-close").as_str()))
                .unwrap();
            assert_eq!(back.bounds.unwrap().to_rect(), layout.back());
            let back_path = [dialog.children.len().saturating_sub(1)];
            assert_eq!(
                center.accessibility_rect_for_path(&host, &layout, &back_path),
                Some(layout.back())
            );
            assert!(center.focus_accessibility_path(&host, &layout, &back_path));
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
                center.click(&host, &layout, layout.row(0).center()),
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

#[test]
fn a_row_body_wraps_to_two_lines_and_elides_the_rest() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let style = theme.text(TextRole::Caption);
    let line = f64::from(text.measure_style("Ag", None, &style).1);
    let long = "The backup disk was disconnected halfway through. Reconnect it and the next backup picks up where this one stopped.";
    let shown = super::preview_lines(&mut text, long, 200.0, &style, super::BODY_LINES);
    assert!(shown.ends_with('…'), "{shown}");
    let height = f64::from(text.measure_style(&shown, Some(200.0), &style).1);
    assert!(height <= line * 2.5, "{shown} is {height}px");
    assert!(height > line * 1.5, "two lines are used before eliding");
    assert_eq!(
        super::preview_lines(&mut text, "Ready", 200.0, &style, super::BODY_LINES),
        "Ready"
    );
}

#[test]
fn the_leading_control_closes_a_standalone_centre_and_goes_back_in_a_drill_in() {
    let theme = Theme::dark();
    let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
    let panel = Rect::new(0.0, 0.0, 360.0, 480.0);
    let standalone = CenterLayout::new(panel, &metrics);
    let drill_in = CenterLayout::new(panel, &metrics).drill_in();
    assert_eq!(
        super::leading_label(&standalone),
        t("notification-center-close")
    );
    assert_eq!(super::leading_label(&drill_in), t("drill-in-back"));
    // A close button is square; Back carries its word.
    assert!((standalone.back().width() - standalone.back().height()).abs() < 1e-6);
    assert!(drill_in.back().width() > drill_in.back().height());
    let host = super::super::stack::tests::TestHost::default();
    let dialog = NotificationCenter::new().accessibility(&host, &standalone, ui::time::now());
    assert!(
        dialog
            .children
            .iter()
            .any(|n| n.name.as_deref() == Some(t("notification-center-close").as_str()))
    );
}

/// The painted rect of every visible row, newest first.
fn row_rects(
    center: &NotificationCenter,
    host: &dyn NotificationHost,
    layout: &CenterLayout,
) -> Vec<Rect> {
    center
        .semantic_rects(host, layout, "")
        .into_iter()
        .filter(|(name, _)| name.starts_with("notification_row["))
        .map(|(_, rect)| rect)
        .collect()
}

#[test]
fn a_row_is_as_tall_as_what_it_holds() {
    let theme = Theme::dark();
    let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
    let host = super::super::stack::tests::TestHost::default();
    // Retained oldest first; the centre lists newest first.
    for notice in [
        Notification::new(
            "Backup couldn't finish",
            "The backup disk was disconnected halfway through. Reconnect it and the next backup picks up where this one stopped.",
        ),
        Notification::new("Lena", "Pushed the floor plan"),
        Notification::new("Update installed", ""),
    ] {
        host.log.borrow_mut().retain(notice);
    }
    let center = NotificationCenter::new();
    let height = center.preferred_height_for_width(&host, &metrics, 360.0);
    let layout = CenterLayout::new(Rect::new(0.0, 0.0, 360.0, height), &metrics);
    let [bare, one_line, full] = row_rects(&center, &host, &layout)[..] else {
        panic!("all three rows show");
    };
    let line = layout.caption_lh;
    assert!(
        (full.height() - layout.row_h()).abs() < 1e-6,
        "a long body keeps the full preview"
    );
    assert!(
        (one_line.height() - (full.height() - line)).abs() < 1e-6,
        "a one-line body gets a one-line row, without an empty line under it"
    );
    assert!(
        (bare.height() - (one_line.height() - line)).abs() < 1e-6,
        "no body, no body line"
    );
    assert!(
        (full.y1 + qs::PANEL_PAD - height).abs() < 1e-6,
        "the panel ends on the padding under its last row"
    );
}

#[test]
fn a_rows_choices_sit_compact_and_trailing_like_its_banners() {
    let theme = Theme::dark();
    let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
    let host = super::super::stack::tests::TestHost::default();
    host.actions.replace(vec![
        ("not-now".into(), "Not now".into()),
        ("keep".into(), "Keep with passkey".into()),
    ]);
    host.log.borrow_mut().retain(Notification::new(
        "Keep this identity?",
        "A passkey brings you back as yourself on any browser.",
    ));
    let center = NotificationCenter::new();
    let choices = |width: f64| {
        let height = center.preferred_height_for_width(&host, &metrics, width);
        let layout = CenterLayout::new(Rect::new(0.0, 0.0, width, height), &metrics);
        let row = row_rects(&center, &host, &layout)
            .first()
            .copied()
            .expect("the ask shows");
        let targets = center.semantic_rects(&host, &layout, "");
        let rect = |name: &str| {
            targets
                .iter()
                .find(|(target, _)| target == name)
                .expect(name)
                .1
        };
        (
            row,
            rect("notification_action[0][0]"),
            rect("notification_action[0][1]"),
        )
    };
    let (row, not_now, keep) = choices(360.0);
    let height = ui::widget::toast::choice_height(&theme);
    assert!((keep.height() - height).abs() < 1e-6);
    assert!((not_now.height() - height).abs() < 1e-6);
    assert!(
        height < metrics.height,
        "the banner's compact controls, not the header's"
    );
    assert!(
        (keep.x1 - (row.x1 - qs::CONTROL_INSET)).abs() < 1e-6,
        "the answer ends the row"
    );
    assert!(
        (keep.x0 - not_now.x1 - ui::widget::toast::choice_gap(&theme)).abs() < 1e-6,
        "secondary then primary, one related step apart"
    );
    assert!(
        not_now.x0 > row.x0 + qs::CONTROL_INSET + 1.0,
        "at their own width, not a pair of slabs"
    );
    assert!(
        (row.y1 - ui::widget::toast::choice_foot(&theme) - keep.y1).abs() < 1e-6,
        "the card's padding under the choices"
    );
    // Too narrow for both at their width: equal halves.
    let (row, not_now, keep) = choices(200.0);
    assert!((not_now.x0 - (row.x0 + qs::CONTROL_INSET)).abs() < 1e-6);
    assert!((not_now.width() - keep.width()).abs() < 1e-6);
}
