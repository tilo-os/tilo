//! Alignment regressions for the floating chart / comment annotation cards
//! (`SheetsApp::draw_charts`, `draw_comment_threads`).
//!
//! The card rows come from measured line boxes and spacing tokens
//! ([`ChromeMetrics::comment_rows`], [`ChromeMetrics::plot_h`]) and the
//! toolkit's `CardFace`, so at any text scale a title never paints under its
//! own actions and a comment never runs out of its card. The painted action
//! targets are what the pointer hits, not a helper's arithmetic.

use super::tests::{measured_app, paint_frame};
use super::*;

/// Annotation card rows stay clear at every scale and painted targets follow the measured rows.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn annotation_cards_keep_rows_and_targets_clear() {
    // Case `annotation_card_rows_clear_each_other_at_every_scale`.
    // The card rhythm keeps every line box clear of its neighbours at both
    // scales, and the card grows with its text.
    {
        let mut heights = Vec::new();
        for scale in [1.0_f32, 2.0] {
            let (app, _tr, _theme) = measured_app(1200.0, 800.0, scale);
            let m = app.metrics.clone();
            let rows = m.comment_rows();
            assert!(
                rows.text_top >= rows.range_h,
                "the text clears the range line"
            );
            assert!(
                rows.replies_top >= rows.text_top + rows.text_h - 0.01,
                "at {scale}x the replies line overlaps the comment text"
            );
            assert!(
                rows.actions_top >= rows.replies_top + m.lh_caption - 0.01,
                "at {scale}x the action row overlaps the replies line"
            );
            assert!(
                (rows.height - rows.actions_top - m.control_h).abs() < 0.01,
                "the card ends with its action row"
            );
            assert!(
                (m.plot_h() - (m.s16 + m.s6)).abs() < 0.01,
                "a plot is content-sized, not text-sized"
            );
            heights.push(rows.height);
        }
        assert!(heights[1] > heights[0] * 1.5, "the card grew with its text");
    }

    // Case `painted_annotation_targets_follow_the_measured_rows`.
    // On the production paint path the registered action targets sit inside
    // their card, below its title, each at least a small control tall and
    // clear of each other.
    {
        // A window wide enough for both cards side by side at 2× text.
        let (mut app, mut tr, theme) = measured_app(1800.0, 800.0, 2.0);
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
        paint_frame(&mut app, &mut tr, &theme);

        let m = app.metrics.clone();
        // Cards float `s3` below the column-header band (never over its labels).
        // Computed here from the grid geometry, independently of the painter.
        let body_top = app.grid_rect.y0 + m.grid.col_header_h();
        let card_top = body_top + m.s3;
        // The comment card floats at the sheet's leading edge, the charts at
        // its trailing edge.
        let middle = app.grid_rect.center().x;
        let (comment_buttons, comment_card): (Vec<_>, Vec<_>) = app
            .annotation_clicks
            .iter()
            .filter(|(rect, _)| rect.center().x < middle)
            .partition(|(rect, _)| rect.height() < 3.0 * m.control_h);
        assert_eq!(
            comment_buttons.len(),
            4,
            "Reply, Edit, Resolve or Reopen, and Delete"
        );
        assert_eq!(comment_card.len(), 1, "the card itself opens its range");
        let card = comment_card[0].0;
        assert!(
            (card.y0 - card_top).abs() < 0.5,
            "the card clears the header band"
        );
        for (rect, _) in &comment_buttons {
            assert!(rect.height() >= m.control_h - 0.01);
            assert!(
                card.contains(rect.center()),
                "target {rect:?} is not on its card {card:?}"
            );
            assert!(
                rect.y0 > card.y0 + m.lh_label,
                "targets sit below the title"
            );
        }
        for pair in comment_buttons.windows(2) {
            assert!(
                pair[0].0.x1 <= pair[1].0.x0 + 0.01,
                "buttons {:?} and {:?} overlap",
                pair[0].0,
                pair[1].0
            );
        }
        assert!(
            matches!(
                app.annotation_clicks.last(),
                Some((_, AnnotationClick::Open(_)))
            ),
            "the card-wide target is registered last, so a button wins where they meet"
        );

        let chart_buttons: Vec<Rect> = app
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
        assert_eq!(chart_buttons.len(), 2, "the chart's Edit and Delete");
        for rect in &chart_buttons {
            assert!(rect.height() >= m.control_h - 0.01);
            assert!(
                rect.y0 >= card_top + m.lh_label,
                "chart action {rect:?} sits under the 2x title"
            );
        }
    }
}
