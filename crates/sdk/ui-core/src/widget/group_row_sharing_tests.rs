//! How windows share a strip: a window alone takes the strip at the
//! default width, and one that opens beside another is placed whole on
//! screen with it.

use super::tests::{make_group, relayout_strip, strip_at};
use super::*;

/// A display `display_px` wide with the desktop's wallpaper gutter at
/// both ends of the strip, holding one window at the default width.
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

/// A window alone on its strip takes the strip rather than two thirds
/// of it beside a third of wallpaper; a window opening beside it brings
/// it back to two thirds, and closing that one widens it again, also
/// where the two had to share the display half and half.
#[test]
fn a_lone_window_takes_the_strip_until_another_opens_beside_it() {
    // Phase `the_browser_desktop`: 1440 px, 1376 px between the gutters.
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
        let _ = row.share_with_neighbour(200);
        relayout_strip(&mut row, 1440.0);
        relayout_strip(&mut row, 1440.0);
        assert!(
            (span_width(&row, 0) - WindowWidth::Normal.resolve(1376.0)).abs() < 0.5,
            "beside another, it is two thirds again: {:?}",
            row.group_canvas_spans()
        );
        assert_eq!(row.groups[0].width(), WindowWidth::Normal);
        let spans = row.group_canvas_spans();
        assert!(
            spans[1].2 - spans[0].1 <= 1440.0,
            "the two are on screen together: {spans:?}"
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
    // Phase `a_1280_display`: the pair shares it half and half.
    {
        let mut row = lone_window_on(1280.0);
        let mut opened = make_group(200, &[2]);
        opened.start_spawn_animation();
        row.insert_group(opened, 1);
        assert!(row.share_with_neighbour(200));
        assert!(
            matches!(row.groups[0].width(), WindowWidth::Fraction(_)),
            "fixture: two thirds and the newcomer do not fit, so both take half"
        );
        let _ = row.groups[0].take_pending_width_change();
        relayout_strip(&mut row, 1280.0);
        assert!(row.remove_group_by_id(200));
        relayout_strip(&mut row, 1280.0);
        relayout_strip(&mut row, 1280.0);
        assert_eq!(
            row.groups[0].width(),
            WindowWidth::Normal,
            "the half it took to share comes back to the default"
        );
        assert_eq!(
            row.groups[0].take_pending_width_change(),
            Some(WindowWidth::Normal),
            "and the host saves the default, not the fraction it resolves to"
        );
        assert!(
            (span_width(&row, 0) - 1216.0).abs() < 0.5,
            "alone, it takes the strip: {:?}",
            row.group_canvas_spans()
        );
    }
    // Phase `a_width_the_person_chose_stays`.
    {
        let mut row = lone_window_on(1440.0);
        row.groups[0].set_width(WindowWidth::Fraction(500));
        relayout_strip(&mut row, 1440.0);
        assert!((span_width(&row, 0) - 688.0).abs() < 0.5);
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

#[test]
fn a_window_that_opens_beside_another_is_placed_whole_on_screen_with_it() {
    // Phase `two_windows_share_a_1280_display`.
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

/// A window opened beside a lone one on a 1440 px display: the window in
/// use narrows to make room on its slot spring while the strip pans to the
/// new one. The pan aims at the strip as it will be, so the new window
/// lands whole with the strip's gutter after it, whether it grows in from
/// nothing or arrives at its width.
#[test]
fn a_window_revealed_while_its_neighbour_narrows_keeps_the_strips_gutter() {
    for grows_in in [false, true] {
        let mut row = lone_window_on(1440.0);
        let mut opened = make_group(200, &[2]);
        if grows_in {
            opened.start_spawn_animation();
        }
        row.insert_group(opened, 1);
        assert!(row.share_with_neighbour(200), "{grows_in}: the two share");
        relayout_strip(&mut row, 1440.0);
        assert!(
            (row.groups[0].dest_width_px() - row.groups[0].layout_width_px()).abs() > 1.0,
            "{grows_in}: fixture: the window in use is still narrowing"
        );
        row.scroll_group_into_view(200);
        row.frame_presented();
        let settled = crate::time::now() + crate::time::Duration::from_secs(2);
        crate::time::testing::with_monotonic_time(settled, || {
            relayout_strip(&mut row, 1440.0);
            row.frame_presented();
            relayout_strip(&mut row, 1440.0);
        });
        let (kept, opened) = (row.groups[0].get_rect(), row.groups[1].get_rect());
        assert!(
            kept.x0 >= -0.5,
            "{grows_in}: the window in use stays whole: {kept:?}"
        );
        assert!(
            opened.x1 <= 1440.0 - STRIP_EDGE_GUTTER_PX + 0.5,
            "{grows_in}: the new window keeps the strip's gutter: {opened:?}, scroll {}",
            row.scroll_x()
        );
    }
}
