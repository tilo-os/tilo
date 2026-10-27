//! A window's outer edge, grabbed where the strip would auto-scroll: it
//! resizes the window and scrolls only when pushed against the display's
//! edge.

use super::tests::{drag_edge, edge_event, edge_strip, move_to, rects, relayout_row_owned, widths};
use super::*;

#[test]
fn a_window_edge_grabbed_at_the_display_edge_and_held_stays_as_it_is() {
    // The first window's left edge is the display's left edge.
    let (row, taffy, root) = edge_strip(Some(0), None);
    let before = rects(&row);
    assert!(
        (before[0].x0 - row.get_rect().x0).abs() < 1.0,
        "fixture: the first window starts at the display's edge: {before:?}"
    );
    let from = Point::new(before[0].x0 + 2.0, before[0].center().y);
    let (mut row, taffy, root, _) = drag_edge(row, &taffy, root, from, &[0.0], false);
    let t0 = crate::time::now();
    for ms in [0, 50, 100, 150] {
        assert!(
            !row.advance_edge_autoscroll(t0 + Duration::from_millis(ms)),
            "held where it was pressed, nothing scrolls"
        );
    }
    let (mut row, _, _) = relayout_row_owned(row, 2000.0);
    assert_eq!(rects(&row), before, "no window changed while it was held");
    // Pushed on against the display's edge, it scrolls.
    let edge = Point::new(row.get_rect().x0, from.y);
    assert!(edge_event(&mut row, &taffy, root, false, &move_to(edge)));
    let _ = row.advance_edge_autoscroll(t0 + Duration::from_millis(200));
    assert!(row.advance_edge_autoscroll(t0 + Duration::from_millis(250)));
}

#[test]
fn the_first_windows_left_edge_dragged_into_the_gutter_only_widens_it() {
    let (mut row, _, _) = edge_strip(Some(0), None);
    row.set_leading_inset(STRIP_EDGE_GUTTER_PX);
    let (row, _, _) = relayout_row_owned(row, 2000.0);
    let (row, taffy, root) = relayout_row_owned(row, 2000.0);
    let before = rects(&row);
    assert!(
        (before[0].x0 - (row.get_rect().x0 + STRIP_EDGE_GUTTER_PX)).abs() < 1.0,
        "fixture: the first window rests a gutter from the display's edge: {before:?}"
    );
    let from = Point::new(before[0].x0 + 2.0, before[0].center().y);
    let into_gutter = -(STRIP_EDGE_GUTTER_PX - 12.0);
    let (mut row, _taffy, _root, frames) =
        drag_edge(row, &taffy, root, from, &[into_gutter], false);
    let t0 = crate::time::now();
    for ms in [0, 50, 100, 150] {
        assert!(
            !row.advance_edge_autoscroll(t0 + Duration::from_millis(ms)),
            "a drag into the gutter does not scroll the strip"
        );
    }
    let (row, _, _) = relayout_row_owned(row, 2000.0);
    let after = rects(&row);
    let grown = -into_gutter;
    assert!(
        (after[0].width() - (before[0].width() + grown)).abs() < 1.0,
        "it widened by the drag and no more: {before:?} -> {after:?}"
    );
    assert_eq!(after, frames[1], "and it stays that way while held");
    assert!(
        (after[0].x0 - (from.x + into_gutter - 2.0)).abs() < 1.0,
        "its left edge is under the pointer: {after:?}"
    );
    assert!(
        (after[0].x1 - before[0].x1).abs() < 1.0,
        "its right edge holds: {before:?} -> {after:?}"
    );
    assert_eq!(
        widths(&after)[1..],
        widths(&before)[1..],
        "no one else changed"
    );
}
