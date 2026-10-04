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
    // On the production paint path the dock stands under the grid, the cards
    // stand inside it clear of each other and of the sheet, and the
    // registered action targets sit inside their card, below its title, each
    // at least a small control tall and clear of each other.
    {
        // Tall enough at 2× text that the dock is not given up for the grid.
        let (mut app, mut tr, theme) = measured_app(1800.0, 1600.0, 2.0);
        seed_annotations(&mut app);
        paint_frame(&mut app, &mut tr, &theme);

        let m = app.metrics.clone();
        assert_eq!(app.dock_cards.len(), 2, "the comment and the chart");
        assert!(
            (app.dock_rect.y1 - app.tabs_rect.y0).abs() < 0.01
                && (app.grid_rect.y1 - app.dock_rect.y0).abs() < 0.01,
            "the dock stands between the grid and the tabs"
        );
        for (_, card) in &app.dock_cards {
            assert!(
                card.y0 >= app.grid_rect.y1 && card.y1 <= app.dock_rect.y1 - m.s3 + 0.01,
                "card {card:?} is not inside the dock {:?}",
                app.dock_rect
            );
        }
        let cards: Vec<Rect> = app.dock_cards.iter().map(|(_, rect)| *rect).collect();
        for pair in cards.windows(2) {
            assert!(pair[0].x1 <= pair[1].x0 + 0.01, "the cards overlap");
        }

        // The comment card is the first in the row.
        let comment = cards[0];
        let (comment_buttons, comment_card): (Vec<_>, Vec<_>) = app
            .annotation_clicks
            .iter()
            .filter(|(rect, _)| rect.center().x < comment.x1)
            .partition(|(rect, _)| rect.height() < 3.0 * m.control_h);
        assert_eq!(
            comment_buttons.len(),
            4,
            "Reply, Edit, Resolve or Reopen, and Delete"
        );
        assert_eq!(comment_card.len(), 1, "the card itself opens its range");
        let card = comment_card[0].0;
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
                app.annotation_clicks.iter().find(|(rect, _)| *rect == card),
                Some((_, AnnotationClick::Open(_)))
            ),
            "the card-wide target opens the thread's range"
        );
        let comment_pos = app
            .annotation_clicks
            .iter()
            .position(|(rect, _)| *rect == card);
        let last_comment_button = app
            .annotation_clicks
            .iter()
            .rposition(|(rect, _)| comment_buttons.iter().any(|(b, _)| b == rect));
        assert!(
            comment_pos > last_comment_button,
            "the card-wide target is registered after its buttons, so a button wins where they meet"
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
                rect.y0 >= cards[1].y0 + m.lh_label,
                "chart action {rect:?} sits under the 2x title"
            );
        }
    }

    // Case `annotations_never_cover_the_sheet`.
    // At the narrow shot size every card still stands under the grid, and in
    // a window too short to give the height up there is no dock at all: the
    // annotations wait rather than cover cells.
    {
        let (mut app, mut tr, theme) = measured_app(800.0, 600.0, 1.0);
        seed_annotations(&mut app);
        app.sheet_charts.push(crate::doc::ChartSnapshot {
            id: "c2".into(),
            title: "Monthly total".into(),
            kind: crate::doc::ChartKind::Line,
            start: (1, 6),
            end: (3, 6),
        });
        app.relayout();
        paint_frame(&mut app, &mut tr, &theme);
        assert_eq!(app.dock_cards.len(), 3, "all three cards fit 800 px");
        for (_, card) in &app.dock_cards {
            assert!(card.y0 >= app.grid_rect.y1, "{card:?} covers the sheet");
            assert!(
                card.x0 >= 0.0 && card.x1 <= 800.0,
                "{card:?} leaves the window"
            );
        }
        assert!(
            app.grid_rect.height() > 6.0 * app.metrics.grid.default_row_h(),
            "the sheet keeps its rows"
        );

        let (mut short, mut tr, theme) = measured_app(800.0, 360.0, 1.0);
        seed_annotations(&mut short);
        paint_frame(&mut short, &mut tr, &theme);
        assert!(short.dock_cards.is_empty(), "no room, no dock");
        assert!(short.annotation_clicks.is_empty());
    }
}

/// A comment thread and a chart on the sheet, as the document would supply.
fn seed_annotations(app: &mut SheetsApp) {
    app.sheet_charts = vec![crate::doc::ChartSnapshot {
        id: "c1".into(),
        title: "Q1 spend by category".into(),
        kind: crate::doc::ChartKind::Column,
        start: (4, 1),
        end: (4, 5),
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
    app.relayout();
}
