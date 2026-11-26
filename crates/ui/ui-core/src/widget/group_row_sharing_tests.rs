//! How windows share a strip: a window alone takes the strip at the default width, and one
//! that opens beside another is placed whole on screen with it.

use super::tests::{make_group, relayout_strip, strip_at};
use super::*;

fn lone_window_on(display_px: f64) -> GroupRow {
    let mut row = GroupRow::new(vec![make_group(100, &[1])]);
    row.set_leading_inset(STRIP_EDGE_GUTTER_PX);
    row.set_output_spans(&[OutputSpan {
        x: 0.0,
        width: display_px,
        usable: display_px - 2.0 * STRIP_EDGE_GUTTER_PX,
    }]);
    relayout_strip(&mut row, display_px);
    relayout_strip(&mut row, display_px);
    row
}

fn span_width(row: &GroupRow, index: usize) -> f64 {
    let (_, x0, x1) = row.group_canvas_spans()[index];
    x1 - x0
}

tilo_test_env::scenario! {
    fn windows_share_the_strip_as_they_open_and_float;

    phase a_lone_window_takes_the_strip_until_another_opens_beside_it;
    {
        // Phase `the_browser_shell`: 1440 px, 1376 px between the gutters.
        {
            let mut row = lone_window_on(1440.0);
            assert!(
                (span_width(&row, 0) - 1376.0).abs() < 0.5,
                "alone, the window takes the strip: {:?}",
                row.group_canvas_spans()
            );
            assert!((row.groups[0].resolved_width_px() - 1376.0).abs() < 0.5);
            let mut opened = make_group(200, &[2]);
            opened.start_spawn_animation();
            row.insert_group(opened, 1);
            assert!(row.share_with_neighbour(200), "the two share the strip");
            relayout_strip(&mut row, 1440.0);
            relayout_strip(&mut row, 1440.0);
            assert!(
                (span_width(&row, 0) - (1376.0 - 8.0) / 2.0).abs() < 1.0,
                "beside another, it takes half the strip: {:?}",
                row.group_canvas_spans()
            );
            let spans = row.group_canvas_spans();
            assert!(
                (spans[1].2 - spans[0].1 - 1376.0).abs() < 1.0,
                "the two tile the strip's usable width: {spans:?}"
            );
            assert!(
                spans[0].1 >= STRIP_EDGE_GUTTER_PX - 0.5
                    && spans[1].2 <= 1440.0 - STRIP_EDGE_GUTTER_PX + 0.5,
                "the leading gutter and the strip's own stay: {spans:?}"
            );
            assert!(row.remove_group_by_id(200));
            relayout_strip(&mut row, 1440.0);
            relayout_strip(&mut row, 1440.0);
            assert!(
                (span_width(&row, 0) - 1376.0).abs() < 0.5,
                "alone again, it takes the strip again: {:?}",
                row.group_canvas_spans()
            );
        }
        {
            let mut row = lone_window_on(1280.0);
            let mut opened = make_group(200, &[2]);
            opened.start_spawn_animation();
            row.insert_group(opened, 1);
            assert!(row.share_with_neighbour(200));
            assert!(
                matches!(row.groups[0].width(), WindowWidth::Share(_)),
                "fixture: two thirds and the newcomer do not fit, so both take half"
            );
            let _ = row.groups[0].take_pending_width_change();
            relayout_strip(&mut row, 1280.0);
            assert!(row.remove_group_by_id(200));
            relayout_strip(&mut row, 1280.0);
            relayout_strip(&mut row, 1280.0);
            assert!(
                matches!(row.groups[0].width(), WindowWidth::Share(_)),
                "the half it took to share is the shell's, not a width the person chose: {:?}",
                row.groups[0].width()
            );
            assert!(
                (span_width(&row, 0) - 1216.0).abs() < 0.5,
                "alone, it takes the strip: {:?}",
                row.group_canvas_spans()
            );
        }
        {
            let mut row = lone_window_on(1440.0);
            row.groups[0].set_width(WindowWidth::Fraction(500));
            relayout_strip(&mut row, 1440.0);
            assert!((span_width(&row, 0) - 688.0).abs() < 0.5);
        }
    }

    phase a_window_carried_alone_to_a_strip_takes_it_like_one_left_alone;
    {
        // Two paths leave a window alone on a strip, and both must take it:
        // the neighbour closing beside it (the phases above), and the window
        // itself carried to a strip of its own. A share is the shell's
        // width, not the person's, so a rebuilt window — one a move makes
        // anew — takes the strip at it just as one left alone in place does.
        let display = 1280.0;
        let usable = display - 2.0 * STRIP_EDGE_GUTTER_PX;
        let half = crate::widget::permille_for_px((usable - 8.0) / 2.0, usable);

        // In place: the share the two took, with the neighbour gone.
        let mut in_place = lone_window_on(display);
        let mut opened = make_group(200, &[2]);
        opened.start_spawn_animation();
        in_place.insert_group(opened, 1);
        assert!(in_place.share_with_neighbour(200));
        assert!(
            in_place.groups[0].width().permille().abs_diff(half) <= 1,
            "fixture: the pair share the strip in half: {:?}",
            in_place.groups[0].width()
        );
        let _ = in_place.groups[0].take_pending_width_change();
        assert!(in_place.remove_group_by_id(200));
        relayout_strip(&mut in_place, display);
        relayout_strip(&mut in_place, display);

        // Carried: the same shared width, rebuilt as a move builds a window
        // on the strip it lands on.
        let mut carried = GroupRow::new(vec![make_group(100, &[1])]);
        let mut group = carried.groups.remove(0);
        group.set_width(WindowWidth::Share(half));
        carried.insert_group(group, 0);
        carried.set_leading_inset(STRIP_EDGE_GUTTER_PX);
        carried.set_output_spans(&[OutputSpan {
            x: 0.0,
            width: display,
            usable,
        }]);
        relayout_strip(&mut carried, display);
        relayout_strip(&mut carried, display);

        let alone_in_place = span_width(&in_place, 0);
        let alone_carried = span_width(&carried, 0);
        assert!(
            (alone_in_place - usable).abs() < 0.5,
            "left alone in place, it takes the strip: {alone_in_place}"
        );
        assert!(
            (alone_carried - alone_in_place).abs() < 0.5,
            "carried alone to a strip, it takes it the same width: \
             {alone_carried} beside {alone_in_place}"
        );
    }

    phase a_window_that_opens_beside_another_is_placed_whole_on_screen_with_it;
    {
        {
            let mut row = opened_beside(1280.0, 251.0);
            assert!(
                row.share_with_neighbour(200),
                "two two-thirds windows overflow 1280 px"
            );
            relayout_strip(&mut row, 1280.0);
            relayout_strip(&mut row, 1280.0);
            let spans = row.group_canvas_spans();
            let (first, last) = (spans[0], spans[1]);
            assert!(
                last.2 - first.1 <= 1280.0,
                "both windows fit on the screen together: {spans:?}"
            );
            for (id, x0, x1) in &spans {
                assert!(
                    x1 - x0 >= tilo_desktop_model::width::SIDE_BY_SIDE_MIN_PX,
                    "window {id} opens at a usable width: {spans:?}"
                );
            }
            assert!(
                row.groups[1].take_pending_width_change().is_some(),
                "the new width is handed to the host to persist"
            );

            row.scroll_groups_into_view_animated(100, 200, crate::time::now(), MotionSource::default());
            let to = row
                .pan
                .motion
                .map_or(row.scroll_x(), |motion| motion.target());
            assert!(
                first.1 >= to - 0.5 && last.2 <= to + 1280.0 + 0.5,
                "the pan shows both whole: spans {spans:?}, scroll {to}"
            );
        }
        {
            let mut row = opened_beside(800.0, 0.0);
            assert!(
                !row.share_with_neighbour(200),
                "two windows under the side-by-side minimum would both be unusable"
            );
            assert_eq!(
                row.groups[0].width().permille(),
                WindowWidth::Normal.permille()
            );
            assert_eq!(row.groups[1].width(), WindowWidth::Normal);
        }
        {
            let mut row = strip_at(2, 800.0, 0.0, 1000);
            row.scroll_groups_into_view_animated(100, 200, crate::time::now(), MotionSource::default());
            let to = row
                .pan
                .motion
                .map_or(row.scroll_x(), |motion| motion.target());
            assert!(
                to.abs() < 0.5,
                "the window the person is in stays framed, not the new one: {to}"
            );
        }
    }

    phase a_window_floated_from_beside_a_lone_one_leaves_it_clear_of_the_card;
    {
        let display = 1280.0;
        let mut row = lone_window_on(display);
        let mut beside = make_group(200, &[2]);
        beside.set_width(WindowWidth::Fraction(300));
        row.insert_group(beside, 1);
        relayout_strip(&mut row, display);
        relayout_strip(&mut row, display);
        assert_eq!(row.groups[0].width(), WindowWidth::Normal);

        let card = 320.0 + 2.0 * crate::widget::FLOAT_CARD_MARGIN;
        assert!(row.share_with_floating_neighbour(200, card));
        let mut floats = HashMap::new();
        floats.insert(
            200,
            vello::kurbo::Rect::new(display - card, 300.0, display, 776.0),
        );
        row.set_floats(floats);
        relayout_strip(&mut row, display);
        relayout_strip(&mut row, display);
        let (_, _, right) = row.group_canvas_spans()[0];
        assert!(
            right <= display - card + 0.5,
            "the window stands clear of the card ({card} px at the edge): {:?}",
            row.group_canvas_spans()
        );
    }
}

/// A 1280 px display with the wheel disc's gutter before the strip: the
/// window in use, then one that has just opened beside it.
fn opened_beside(viewport_w: f64, inset: f64) -> GroupRow {
    let mut row = strip_at(1, viewport_w, inset, crate::widget::WIDTH_NORMAL_PERMILLE);
    let mut opened = make_group(200, &[2]);
    opened.start_spawn_animation();
    row.insert_group(opened, 1);
    row
}
