use super::tests::{measured_app, paint_frame};
use super::*;

#[test]
fn full_ui_scale_keeps_chart_controls_readable() {
    let (mut app, mut tr, _) = measured_app(1440.0, 810.0, 1.0);
    let theme = Theme::dark().with_ui_scale(1.5);
    app.sheet_charts = vec![
        ChartSnapshot {
            id: "spend".into(),
            title: "Quarterly spend".into(),
            kind: ChartKind::Column,
            start: (1, 0),
            end: (3, 5),
        },
        ChartSnapshot {
            id: "trend".into(),
            title: "Monthly trend".into(),
            kind: ChartKind::Line,
            start: (1, 0),
            end: (3, 5),
        },
    ];
    paint_frame(&mut app, &mut tr, &theme);
    assert_eq!(app.dock_cards.len(), 2, "charts must not disappear at 150%");
    assert!(
        app.grid_rect.height()
            >= app.metrics.grid.col_header_h() + 4.0 * app.metrics.grid.default_row_h()
    );
    for (_, card) in &app.dock_cards {
        let body = ui::widget::CardFace::new()
            .title("Quarterly spend")
            .body_rect(&mut tr, &theme, *card);
        let controls: Vec<_> = app
            .annotation_clicks
            .iter()
            .filter(|(rect, _)| card.contains(rect.center()))
            .collect();
        assert_eq!(
            controls.len(),
            1,
            "only the header management toggle is visible"
        );
        assert!(
            body.height() > app.metrics.plot_h() + app.metrics.control_h,
            "the plot reclaims the action row"
        );
        for (rect, action) in controls {
            assert!(matches!(action, AnnotationClick::ManageChart(_)));
            assert!(rect.y1 <= body.y0, "management stays in the header");
            assert!(rect.x0 >= card.x0 && rect.x1 <= card.x1 && rect.y1 <= card.y1);
        }
    }
}

#[test]
fn long_formula_scrolls_caret_and_hit_mapping_together() {
    let (mut app, mut tr, _) = measured_app(1440.0, 810.0, 1.0);
    let theme = Theme::dark().with_ui_scale(1.5);
    let formula = format!("={}B2", "SUM(B2:D2)+".repeat(25));
    app.grid.begin_edit(Some(formula.clone()));
    paint_frame(&mut app, &mut tr, &theme);
    assert!(app.formula_scroll_x > 0.0);
    let end = app.formula_char_x.last().copied().expect("end boundary");
    assert!(end <= app.formula_field_rect.x1 - app.metrics.s3 + 0.01);
    assert_eq!(app.formula_caret_at_x(end), formula.chars().count());
    app.grid.set_edit_caret(0, false);
    paint_frame(&mut app, &mut tr, &theme);
    assert!(app.formula_scroll_x.abs() < f64::EPSILON);
    assert_eq!(
        app.formula_caret_at_x(app.formula_field_rect.x0 + app.metrics.s3),
        0
    );
}
