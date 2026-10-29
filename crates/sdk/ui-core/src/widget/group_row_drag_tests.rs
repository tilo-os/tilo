//! A dragged window near the strip's edge pans the strip, and resting on a
//! background tab brings it to the front.

use super::tests::{dispatch_at, group_grab_point, make_row_with_groups_with_taffy, press_at};
use super::*;

#[test]
fn moving_a_whole_window_to_an_edge_pans_the_strip_and_keeps_it_under_the_pointer() {
    let (mut row, taffy, root) =
        make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
    row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
    row.set_content_w_for_test(row.content_width());
    let grab = group_grab_point(&row, 0);
    press_at(&mut row, &taffy, root, grab);
    let edge = Point::new(698.0, grab.y);
    dispatch_at(
        &mut row,
        &taffy,
        root,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(edge.x, edge.y),
        },
    );
    assert!(
        row.group_drag_active(),
        "fixture: the window is being moved"
    );
    assert!(
        row.drag_reach_pending(),
        "resting at the edge asks for frames"
    );
    let start_before = row.group_drag.map(|d| d.start.x).unwrap();
    let t0 = crate::time::now();
    row.advance_drag_reach_for_test(t0);
    assert!(row.advance_drag_reach_for_test(t0 + Duration::from_millis(100)));
    let panned = row.scroll_x();
    assert!(panned > 0.0, "the strip panned under the moved window");
    let start_after = row.group_drag.map(|d| d.start.x).unwrap();
    assert!(
        (start_before - start_after - panned).abs() < 1e-6,
        "the moved window's travel grew by the pan, so it stays under the pointer"
    );
    assert!(row.group_reorder_target().is_some(), "the gap is re-aimed");
}

#[test]
fn a_drag_resting_on_a_background_tab_brings_it_to_the_front() {
    let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1, 2], &[3]]);
    assert_eq!(row.groups[0].active_tab_id(), 1);
    let background = row.groups[0].tab_rects()[1].center();
    let t0 = crate::time::now();

    row.set_external_content_drag_hover(Some(background));
    assert!(!row.advance_drag_reach_for_test(t0));
    assert!(
        row.drag_reach_pending(),
        "a pending spring keeps frames coming"
    );
    assert!(!row.advance_drag_reach_for_test(t0 + Duration::from_millis(300)));
    assert_eq!(
        row.groups[0].active_tab_id(),
        1,
        "passing over is not resting"
    );
    assert!(row.advance_drag_reach_for_test(t0 + Duration::from_millis(520)));
    assert_eq!(
        row.groups[0].active_tab_id(),
        2,
        "the rested-on tab is in front"
    );

    // Moving off before the delay springs nothing.
    let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1, 2], &[3]]);
    let background = row.groups[0].tab_rects()[1].center();
    row.set_external_content_drag_hover(Some(background));
    row.advance_drag_reach_for_test(t0);
    row.set_external_content_drag_hover(Some(Point::new(background.x, 400.0)));
    row.advance_drag_reach_for_test(t0 + Duration::from_millis(600));
    assert_eq!(row.groups[0].active_tab_id(), 1);

    row.set_external_content_drag_hover(None);
    row.last_cursor = background;
    assert!(!row.advance_drag_reach_for_test(t0));
    assert!(!row.advance_drag_reach_for_test(t0 + Duration::from_secs(2)));
    assert_eq!(row.groups[0].active_tab_id(), 1);
}
