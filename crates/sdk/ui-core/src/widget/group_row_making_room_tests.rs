//! Making room on a strip: a window arriving between others moves the ones
//! on its right along, and one leaving lets them slide back into the gap,
//! each from where it was drawn.

use super::tests::{
    make_group, make_row_with_groups_with_taffy, relayout_row_owned, relayout_row_owned_with,
};
use super::*;

#[test]
fn making_room_slides_pushed_groups_from_their_old_place() {
    // Fresh curves are read in the instant they start: a descheduled
    // runner cannot age them.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        fn making_room_translates_the_groups_it_pushes_and_never_narrows_them() {
            let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let before: Vec<(u64, Rect)> =
                row.groups.iter().map(|g| (g.id, g.get_rect())).collect();

            row.insert_group(make_group(999, &[9]), 1);
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);

            let fresh = row
                .groups
                .iter()
                .find(|g| g.id == 999)
                .expect("the inserted group is in the row");
            let travel = fresh.get_rect().width() + HANDLE_SLOT_PX;
            assert!(travel > 1.0, "fixture: the new window has a real width");

            for (idx, (id, was)) in before.iter().enumerate() {
                let now = row
                    .groups
                    .iter()
                    .find(|g| g.id == *id)
                    .expect("no pre-existing group may disappear")
                    .get_rect();
                assert!(
                    (now.width() - was.width()).abs() < 0.5,
                    "group {id} must keep its width when room is made for \
                 another: was {} now {}",
                    was.width(),
                    now.width(),
                );
                let expected_dx = if idx == 0 { 0.0 } else { travel };
                assert!(
                    (now.x0 - was.x0 - expected_dx).abs() < 0.5,
                    "group {id} must translate by exactly the new window's \
                 footprint ({expected_dx}), not by some compressed \
                 fraction of it: moved {}",
                    now.x0 - was.x0,
                );
            }
        }

        /// And the ones that moved are *seen* moving: they paint where they
        /// used to be and travel to where they now belong.
        fn the_groups_that_moved_start_from_where_they_were() {
            let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let unmoved = row.groups[0].id;
            let pushed: Vec<u64> = row.groups[1..].iter().map(|g| g.id).collect();

            row.insert_group(make_group(999, &[9]), 1);
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            let travel = row
                .groups
                .iter()
                .find(|g| g.id == 999)
                .expect("inserted group")
                .get_rect()
                .width()
                + HANDLE_SLOT_PX;

            assert_eq!(
                row.shift_offset_for(unmoved),
                0.0,
                "a group that did not move must not be animated",
            );
            for id in pushed {
                let off = row.shift_offset_for(id);
                assert!(
                    off < 0.0,
                    "group {id} must start left of its new home, i.e. where it \
                 was; got {off}",
                );
                assert!(
                    off >= -travel - 0.5 && off.abs() > travel * 0.5,
                    "group {id} must start at (about) the position it had — a \
                 travel of {travel} px; got {off}",
                );
            }
        }

        fn appending_at_the_end_animates_nothing() {
            let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            let existing: Vec<u64> = row.groups.iter().map(|g| g.id).collect();

            let at_end = row.groups.len();
            row.insert_group(make_group(999, &[9]), at_end);
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);

            for id in existing {
                assert_eq!(
                    row.shift_offset_for(id),
                    0.0,
                    "nothing had to move, so group {id} must not animate",
                );
            }
            assert!(!row.shift_in_flight(), "and no slide is in flight");
        }

        fn a_slide_in_flight_asks_for_animation_frames() {
            let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            assert!(
                !row.needs_animation_frame(),
                "fixture: an idle strip is idle",
            );
            row.insert_group(make_group(999, &[9]), 0);
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            assert!(
                row.needs_animation_frame(),
                "a strip making room must keep redrawing until it settles",
            );
        }

        /// Reduced motion: the pushed groups are drawn where they now
        /// belong on the first frame, and nothing asks to be redrawn.
        fn reduced_motion_makes_room_without_a_slide() {
            let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            let pushed: Vec<u64> = row.groups.iter().map(|g| g.id).collect();
            row.insert_group(make_group(999, &[9]), 0);
            let theme = Theme::dark().with_reduce_motion(true);
            let (row, _taffy, _root) = relayout_row_owned_with(row, 2000.0, &theme);
            for id in pushed {
                assert_eq!(
                    row.shift_offset_for(id),
                    0.0,
                    "under reduced motion group {id} is drawn at its new home",
                );
            }
            assert!(
                !row.needs_animation_frame(),
                "under reduced motion making room asks for no animation frames",
            );
        }

        /// A second insert while the first slide is travelling bends the
        /// one motion: the group is drawn exactly where it was, moving
        /// exactly as fast, and only its destination changes.
        fn a_second_insert_mid_flight_continues_from_where_the_group_is_drawn() {
            let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let pushed = row.groups[2].id;
            row.insert_group(make_group(999, &[9]), 1);
            let (mut row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            row.shift_hold = Some(Duration::from_millis(60));
            let now = crate::time::now();
            let (drawn, velocity) = row.move_state_at(pushed, now);
            let laid_before = rect_of(&row, pushed).x0;
            assert!(
                drawn < -1.0 && velocity > 1.0,
                "fixture: 60 ms in, the group is still travelling right: {drawn} px, {velocity} px/s"
            );

            row.insert_group(make_group(998, &[8]), 0);
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            let laid_after = rect_of(&row, pushed).x0;
            assert!(
                laid_after > laid_before + 1.0,
                "fixture: the second insert pushes it on"
            );
            let (_, m) = row
                .moves
                .iter()
                .find(|(id, _)| *id == pushed)
                .copied()
                .expect("the retargeted group is still moving");
            let (from, carried) = m.sampled_after(Duration::ZERO);
            assert!(
                (laid_after + from - (laid_before + drawn)).abs() < 1e-6,
                "no snap: drawn at {} before the retarget, {} after",
                laid_before + drawn,
                laid_after + from
            );
            assert!(
                (carried - velocity).abs() < 1e-6,
                "velocity carries across the retarget: {velocity} then {carried}"
            );
        }

        /// Closing a window: the groups to its right are drawn where they
        /// were and spring left into the gap.
        fn closing_a_window_slides_its_right_neighbours_into_the_gap() {
            let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let (mut row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            let closed = row.groups[0].id;
            let footprint = rect_of(&row, closed).width() + HANDLE_SLOT_PX;
            let neighbours: Vec<u64> = row.groups[1..].iter().map(|g| g.id).collect();
            assert!(row.remove_group_by_id(closed));
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            for id in neighbours {
                let off = row.shift_offset_for(id);
                assert!(
                    off > footprint * 0.5 && off <= footprint + 0.5,
                    "group {id} must start where it was, {footprint} px right; got {off}"
                );
            }
            assert!(row.needs_animation_frame(), "the gap closes over frames");
        }

        /// A keyboard move swaps two groups; both are seen travelling, in
        /// opposite directions.
        fn a_reorder_springs_both_groups_from_their_old_places() {
            let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let (mut row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            let (a, b) = (row.groups[0].id, row.groups[1].id);
            assert!(row.reorder_group_by_id(a, 1));
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            let (off_a, off_b) = (row.shift_offset_for(a), row.shift_offset_for(b));
            assert!(
                off_a < -1.0,
                "the moved group starts left, where it was: {off_a}"
            );
            assert!(
                off_b > 1.0,
                "its neighbour starts right, where it was: {off_b}"
            );
        }

        /// Critically damped: the path approaches home from one side and
        /// arrives within the heavy spring's budget; then the row goes quiet.
        fn the_spring_never_crosses_home_and_settles_then_goes_quiet() {
            let start = crate::time::now();
            let mut m = strip_motion(-600.0);
            m.set(
                0.0,
                crate::motion::WindowMotion::MOVE.timing(
                    crate::motion::Direction::Enter,
                    crate::tokens::EffectiveMotion::default(),
                    MotionSource::Pointer,
                ),
                start,
            );
            for ms in 0..=1000_u64 {
                let (x, _) = m.sampled_after(Duration::from_millis(ms));
                assert!(x <= 0.0, "crossed home at {ms} ms: {x}");
            }
            let settled_at = m
                .settles_at()
                .expect("the spring moves")
                .saturating_duration_since(start);
            assert!(
                settled_at <= Duration::from_millis(450),
                "a 600 px move settles in under half a second, took {settled_at:?}"
            );

            let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            row.insert_group(make_group(999, &[9]), 0);
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            assert!(row.shift_in_flight(), "fixture: the strip makes room");
            let later = crate::time::now() + Duration::from_secs(2);
            let (row, _taffy, _root) = crate::time::testing::with_monotonic_time(later, || {
                relayout_row_owned(row, 2000.0)
            });
            assert!(row.moves.is_empty(), "settled moves are dropped");
            assert!(
                !row.needs_animation_frame(),
                "a settled strip asks for no frames"
            );
        }

        /// A close left of the view, with the strip panned to its far end:
        /// the pan follows the canvas the close took from before the view,
        /// in the same pass the close lands in, so no group shown moves at
        /// all, and nothing has to be pulled back into the strip's range.
        fn a_close_while_panned_starts_every_group_where_it_was_drawn() {
            fn panned_four() -> GroupRow {
                let (row, _taffy, _root) =
                    make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4]]);
                let (mut row, _taffy, _root) = relayout_row_owned(row, 1200.0);
                // Past the end: the layout clamps it to the far end.
                row.set_scroll_x_for_test(1.0e6);
                let (row, _taffy, _root) = relayout_row_owned(row, 1200.0);
                assert!(row.scroll_x() > 1.0, "fixture: the strip is panned");
                row
            }
            let mut row = panned_four();
            let drawn = |row: &GroupRow, id: u64| rect_of(row, id).x0 + row.shift_offset_for(id);
            let survivors: Vec<u64> = row.groups[1..].iter().map(|g| g.id).collect();
            let before: Vec<f64> = survivors.iter().map(|&id| drawn(&row, id)).collect();
            let scroll_before = row.scroll_x();
            let closed = row.groups[0].id;
            let footprint = rect_of(&row, closed).width() + HANDLE_SLOT_PX;
            let mut still = panned_four();
            assert!(row.remove_group_by_id(closed));
            let (row, _taffy, _root) = relayout_row_owned(row, 1200.0);
            assert!(
                !row.shift_in_flight() && row.moves.is_empty(),
                "nothing slides: the pan carries the view"
            );
            for (&id, was) in survivors.iter().zip(before) {
                let now = drawn(&row, id);
                assert!(
                    (now - was).abs() < 1.0,
                    "group {id} jumped from {was} to {now} on the close's first frame"
                );
            }
            assert!(
                (scroll_before - row.scroll_x() - footprint).abs() < 1.0,
                "the pan follows the {footprint} px the close took before the view: \
                 from {scroll_before} to {}",
                row.scroll_x()
            );
            assert!(!row.snap_in_flight(), "nothing is pulled back");

            assert!(still.remove_group_by_id(closed));
            let theme = Theme::dark().with_reduce_motion(true);
            let (still, _taffy, _root) = relayout_row_owned_with(still, 1200.0, &theme);
            assert!(
                (scroll_before - still.scroll_x() - footprint).abs() < 1.0,
                "under reduced motion the pan follows the same"
            );
            assert!(
                !still.needs_animation_frame(),
                "under reduced motion nothing travels"
            );
        }

        fn rect_of(row: &GroupRow, id: u64) -> Rect {
            row.groups
                .iter()
                .find(|g| g.id == id)
                .map(WindowGroup::get_rect)
                .expect("group in the row")
        }

        making_room_translates_the_groups_it_pushes_and_never_narrows_them();
        the_groups_that_moved_start_from_where_they_were();
        appending_at_the_end_animates_nothing();
        a_slide_in_flight_asks_for_animation_frames();
        reduced_motion_makes_room_without_a_slide();
        a_second_insert_mid_flight_continues_from_where_the_group_is_drawn();
        closing_a_window_slides_its_right_neighbours_into_the_gap();
        a_reorder_springs_both_groups_from_their_old_places();
        the_spring_never_crosses_home_and_settles_then_goes_quiet();
        a_close_while_panned_starts_every_group_where_it_was_drawn();
    });
}
