//! A strip across several displays: one lane per display, each laid out,
//! panned and moved by the strip deck on its own.

use super::tests::{make_group, relayout_row_owned};
use super::*;

/// A 1000-wide display with the wheel gutter beside an 800-wide one
/// whose bottom edge is 200 rows higher.
pub(super) fn two_lanes() -> Vec<StripLane> {
    vec![
        StripLane {
            output: 0,
            area: Rect::new(0.0, 0.0, 1000.0, 600.0),
            usable: 1000.0,
            leading: true,
        },
        StripLane {
            output: 1,
            area: Rect::new(1000.0, 0.0, 1800.0, 400.0),
            usable: 800.0,
            leading: false,
        },
    ]
}

/// Groups 100..=300 on the first display, 400..=600 on the second.
pub(super) fn partitioned_row() -> GroupRow {
    let groups: Vec<WindowGroup> = (1..=6_u64)
        .map(|i| make_group(i.saturating_mul(100), &[i]))
        .collect();
    let mut row = GroupRow::new(groups);
    let assignment: HashMap<u64, u32> = (1..=6_u64)
        .map(|i| (i.saturating_mul(100), u32::from(i > 3)))
        .collect();
    row.set_lanes(&two_lanes(), &assignment);
    // The first pass learns where the row sits; lanes are built
    // against that on the second.
    let (row, _, _) = relayout_row_owned(row, 1800.0);
    let (row, _, _) = relayout_row_owned(row, 1800.0);
    row
}

fn rect_of(row: &GroupRow, id: u64) -> Rect {
    row.groups
        .iter()
        .find(|g| g.id == id)
        .map(WindowGroup::get_rect)
        .expect("group laid out")
}

#[test]
fn a_strip_across_displays_slides_on_every_display_without_a_new_layout() {
    let row = partitioned_row();
    let (mut row, taffy, root) = relayout_row_owned(row, 1800.0);
    let _ = row.take_structure_dirty();
    let rest: Vec<(u64, Rect)> = row.groups.iter().map(|g| (g.id, g.get_rect())).collect();
    for shift in [300.0, -250.0, 0.0] {
        row.set_deck_shift(shift, &[]);
        row.apply_layout(&taffy, root, Point::new(0.0, shift));
        assert!(
            !row.take_structure_dirty(),
            "moving the strip by {shift} needs no new layout"
        );
        for (id, at_rest) in &rest {
            let r = rect_of(&row, *id);
            assert!(
                (r.y0 - (at_rest.y0 + shift)).abs() < 0.5
                    && (r.x0 - at_rest.x0).abs() < 0.5
                    && (r.height() - at_rest.height()).abs() < 0.5,
                "window {id} on display {:?} moves with the strip by {shift}: {r:?} from {at_rest:?}",
                row.output_of_group(*id)
            );
        }
    }
}

#[test]
fn each_display_moves_its_own_lane_of_the_strip() {
    let row = partitioned_row();
    let (mut row, taffy, root) = relayout_row_owned(row, 1800.0);
    let _ = row.take_structure_dirty();
    let rest: Vec<(u64, Rect)> = row.groups.iter().map(|g| (g.id, g.get_rect())).collect();
    // The second display goes to the strip below; the first stays.
    row.set_deck_shift(0.0, &[(0, 0.0), (1, -400.0)]);
    row.apply_layout(&taffy, root, Point::ORIGIN);
    assert!(!row.take_structure_dirty(), "a move needs no new layout");
    for (id, at_rest) in &rest {
        let r = rect_of(&row, *id);
        let shift = if row.output_of_group(*id) == Some(1) {
            -400.0
        } else {
            0.0
        };
        assert!(
            (r.y0 - (at_rest.y0 + shift)).abs() < 0.5 && (r.x0 - at_rest.x0).abs() < 0.5,
            "window {id} moves by its own display's {shift}: {r:?} from {at_rest:?}"
        );
    }
    assert!(
        row.covers(Point::new(500.0, 300.0)),
        "the first display still shows the strip"
    );
    assert!(
        !row.covers(Point::new(1400.0, 200.0)),
        "the second display shows another strip, not this one"
    );
}

#[test]
fn a_partitioned_strip_lays_each_display_out_on_its_own() {
    let row = partitioned_row();
    let lanes = two_lanes();
    for id in [100, 200, 300] {
        let r = rect_of(&row, id);
        assert!(
            r.x0 >= lanes[0].area.x0,
            "{id} starts left of its display: {r:?}"
        );
        assert!(
            r.y1 <= lanes[0].area.y1 + 0.5,
            "{id} is taller than its display"
        );
        assert_eq!(row.output_of_group(id), Some(0));
    }
    let first_on_second = rect_of(&row, 400);
    assert!(
        (first_on_second.x0 - lanes[1].area.x0).abs() < 0.5,
        "the second display's first window starts at its own left edge: {first_on_second:?}"
    );
    for id in [400, 500, 600] {
        let r = rect_of(&row, id);
        assert!(
            r.x0 >= lanes[1].area.x0,
            "{id} leaks onto the first display: {r:?}"
        );
        assert!(
            r.y1 <= lanes[1].area.y1 + 0.5,
            "{id} runs past the shorter display's bottom edge: {r:?}"
        );
        assert_eq!(row.output_of_group(id), Some(1));
    }
    // The seam between the displays is not a resize gap.
    let seam = row.handle_at(2).expect("handle after the third group");
    assert!(seam.get_rect().area() <= 0.0, "{:?}", seam.get_rect());
    assert_eq!(row.group_ids_on(1), vec![400, 500, 600]);
}

#[test]
fn panning_one_display_leaves_the_other_where_it_was() {
    let mut row = partitioned_row();
    let before: Vec<Rect> = [100, 200, 300]
        .iter()
        .map(|&id| rect_of(&row, id))
        .collect();
    assert!(
        row.scroll_group_into_view(600),
        "the last window is off the second display"
    );
    let (row, _, _) = relayout_row_owned(row, 1800.0);
    let after: Vec<Rect> = [100, 200, 300]
        .iter()
        .map(|&id| rect_of(&row, id))
        .collect();
    assert_eq!(before, after, "the first display must not move");
    assert!(row.output_pan(1).expect("lane").scroll_x() > 0.0);
    assert_eq!(row.output_pan(0).expect("lane").scroll_x(), 0.0);
    let last = rect_of(&row, 600);
    assert!(last.x1 <= 1800.5 && last.x0 >= 1000.0, "{last:?}");
}
