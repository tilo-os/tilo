//! Round-two alignment regressions for the floating chart / comment
//! annotation cards (`SheetsApp::draw_charts`, `draw_comment_threads`).
//!
//! Their rows used to sit at literal offsets (`21 / 26 / 86`, `22 / 48 / 84 /
//! 112`) while the text inside them followed the accessibility scale, so at
//! 2× a chart title painted under its own action band and a comment body ran
//! out of the card. The rows now come from [`ChromeMetrics::chart_card_rows`]
//! and [`ChromeMetrics::comment_card_rows`], measured line boxes plus spacing
//! tokens with the classic values as 1× floors.

use super::tests::measured_app;
use super::*;

/// Annotation card rows stay clear at every scale and painted targets follow the measured rows.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn annotation_cards_keep_rows_and_targets_clear() {
    // Case `annotation_card_rows_clear_each_other_at_every_scale`.
    // The card rhythm keeps every line box clear of its neighbours and of the
    // action band at both scales, and a 1× card is the classic size.
    {
        for scale in [1.0_f32, 2.0] {
            let (app, _tr) = measured_app(1200.0, 800.0, scale);
            let m = app.metrics.clone();

            let chart = m.chart_card_rows();
            let label_top = chart.controls_y0 + (ANNOTATION_ACTION_H - m.lh_xs) / 2.0;
            assert!(
                label_top >= chart.title_top + m.lh_sm + m.s1 - 0.01,
                "at {scale}x the chart action labels ({label_top:.1}) start inside the title line \
             ({:.1}..{:.1})",
                chart.title_top,
                chart.title_top + m.lh_sm
            );
            assert!(
                chart.controls_y0 >= chart.title_top + CHART_CONTROLS_SEAM - 0.01,
                "the classic seam is the floor"
            );
            assert!((chart.controls_y1 - chart.controls_y0 - ANNOTATION_ACTION_H).abs() < 0.01);
            assert!(
                chart.plot_y0 >= chart.controls_y1 + m.s4 - 0.01,
                "plot clears the band"
            );
            assert!(
                (chart.plot_y1 - chart.plot_y0 - CHART_PLOT_H).abs() < 0.01,
                "plot is content-sized"
            );
            assert!(
                (chart.height - chart.plot_y1 - m.s4).abs() < 0.01,
                "bottom pad is s4"
            );

            let comment = m.comment_card_rows();
            assert!(
                comment.body_top >= comment.heading_top + m.lh_sm + m.s2 - 0.01,
                "at {scale}x the body ({:.1}) overlaps the heading line",
                comment.body_top
            );
            assert!(
                comment.detail_top >= comment.body_top + m.lh_sm + m.s4 - 0.01,
                "at {scale}x the detail line ({:.1}) overlaps the body",
                comment.detail_top
            );
            assert!(
                comment.controls_y0 >= comment.detail_top + m.lh_xs + m.s3 - 0.01,
                "at {scale}x the action band ({:.1}) overlaps the detail line",
                comment.controls_y0
            );
            // The resolve/reopen target is a full action-height band around its
            // detail line that stops where the action band starts.
            assert!((comment.toggle_y1 - comment.toggle_y0 - ANNOTATION_ACTION_H).abs() < 0.01);
            assert!(
                comment.toggle_y1 <= comment.controls_y0 + 0.01,
                "toggle target never enters the band"
            );
            assert!(
                comment.toggle_y0 <= comment.detail_top + 0.01
                    && comment.toggle_y1 >= comment.detail_top + m.lh_xs - 0.01,
                "toggle target contains its detail line"
            );

            if (scale - 1.0).abs() < f32::EPSILON {
                // Classic density: the 1× cards are the sizes they always were.
                assert!(
                    (chart.height - 176.0).abs() < 0.5,
                    "1x chart card {:.2}",
                    chart.height
                );
                assert!(
                    (comment.height - 178.0).abs() < 0.5,
                    "1x comment card {:.2}",
                    comment.height
                );
                assert!(
                    (chart.controls_y0 - 26.0).abs() < 0.5,
                    "1x chart band {:.2}",
                    chart.controls_y0
                );
                assert!(
                    (comment.controls_y0 - 112.0).abs() < 0.5,
                    "1x comment band {:.2}",
                    comment.controls_y0
                );
            } else {
                assert!(chart.height > 176.0, "2x chart card grew with its text");
                assert!(comment.height > 178.0, "2x comment card grew with its text");
            }
        }
    }

    // Case `painted_annotation_targets_follow_the_measured_rows`.
    // On the production paint path the registered action targets sit inside the
    // card and below the title at 2×: this is what the pointer hits, not a
    // helper's arithmetic.
    {
        let (mut app, mut tr) = measured_app(1200.0, 800.0, 2.0);
        let theme = Theme::dark().with_text_scale(2.0);
        app.sheet_charts = vec![crate::doc::ChartSnapshot {
            id: "c1".into(),
            title: "Quarterly spend".into(),
            kind: crate::doc::ChartKind::Column,
            start: (1, 1),
            end: (4, 6),
        }];
        app.sheet_comments = vec![crate::doc::CommentThread {
            id: "t1".into(),
            start: (1, 1),
            end: (4, 1),
            author: "Alex".into(),
            body: "Check the rent increase before sending this budget.".into(),
            resolved: true,
            replies: Vec::new(),
        }];
        app.annotation_clicks.clear();
        let mut scene = Scene::new();
        app.draw_charts(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        app.draw_comment_threads(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        let m = app.metrics.clone();
        let chart_rows = m.chart_card_rows();
        // Cards float `s3` below the column-header band (never over its labels).
        // Computed here from the grid geometry, independently of the production
        // helper the painter uses, so a regression in either shows.
        let body_top = app.grid_rect.y0 + m.grid.col_header_h();
        let chart_card_top = body_top + m.s3;
        let chart_targets: Vec<Rect> = app
            .annotation_clicks
            .iter()
            .filter(|(_, action)| {
                matches!(
                    action,
                    AnnotationClick::EditChart(_) | AnnotationClick::DeleteChart(_)
                )
            })
            .map(|(rect, _)| *rect)
            .collect();
        assert_eq!(
            chart_targets.len(),
            2,
            "two chart-only actions were registered"
        );
        for rect in &chart_targets {
            let label_top = rect.y0 + (rect.height() - m.lh_xs) / 2.0;
            assert!(
                label_top >= chart_card_top + chart_rows.title_top + m.lh_sm + m.s1 - 0.01,
                "chart action {rect:?} paints its label under the 2x title"
            );
            assert!((rect.height() - ANNOTATION_ACTION_H).abs() < 0.01);
        }

        let comment_rows = m.comment_card_rows();
        let comment_card_top = body_top + m.s3;
        let toggle = app
            .annotation_clicks
            .iter()
            .find(|(_, action)| matches!(action, AnnotationClick::ToggleComment(..)))
            .map(|(rect, _)| *rect)
            .expect("resolve/reopen target");
        let reply = app
            .annotation_clicks
            .iter()
            .find(|(_, action)| matches!(action, AnnotationClick::Reply(_)))
            .map(|(rect, _)| *rect)
            .expect("reply action");
        assert!(
            toggle.y1 <= reply.y0 + 0.01,
            "toggle {toggle:?} runs into the action band {reply:?}"
        );
        let detail_top = comment_card_top + comment_rows.detail_top;
        assert!(toggle.y0 <= detail_top + 0.01 && toggle.y1 >= detail_top + m.lh_xs - 0.01);
        assert!(
            (reply.y0 - (comment_card_top + comment_rows.controls_y0)).abs() < 0.01,
            "the painted band is the one the rows describe"
        );
    }
}
