//! Windows lifted over the desk: where a float is laid out and hit, and what
//! a drag that lifts or snaps one reports.

use std::collections::HashMap;

use super::tests::{
    FIXTURE_USABLE_PX, dispatch_at, group_grab_point, make_row_with_groups_with_taffy, move_to,
    press_at, relayout_row_owned,
};
use super::*;
use vello::kurbo::Vec2;

fn floats(pairs: &[(u64, Rect)]) -> HashMap<u64, Rect> {
    pairs.iter().copied().collect()
}

fn release(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId) {
    dispatch_at(
        row,
        taffy,
        root,
        &WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );
}

#[test]
fn a_float_is_laid_out_at_its_rect_and_leaves_the_strip_it_came_from() {
    let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
    let in_flow_right = row.groups[2].get_rect().x0;
    let mut row = row;
    let at = Rect::new(120.0, 60.0, 760.0, 540.0);
    row.set_floats(floats(&[(200, at)]));
    let (row, _taffy, _root) = relayout_row_owned(row, FIXTURE_USABLE_PX);
    let rect = row.groups[1].get_rect();
    assert!(
        (rect.x0 - at.x0).abs() < 0.5
            && (rect.y0 - at.y0).abs() < 0.5
            && (rect.width() - at.width()).abs() < 0.5
            && (rect.height() - at.height()).abs() < 0.5,
        "the floating window stands at its rect, not in the lane flow: {rect:?}"
    );
    assert!(
        row.groups[2].get_rect().x0 < in_flow_right - 1.0,
        "the windows after the float close up as if it were not there"
    );
    assert!(
        (row.groups[0].get_rect().x0 - 0.0).abs() < 0.5,
        "the windows before it stay where they were"
    );
}

#[test]
fn the_float_drawn_last_is_the_one_the_pointer_reaches_first() {
    let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
    let at = Rect::new(200.0, 100.0, 840.0, 580.0);
    row.set_floats(floats(&[(100, at), (200, at)]));
    let (row, _taffy, _root) = relayout_row_owned(row, FIXTURE_USABLE_PX);
    assert_eq!(
        row.float_group_at(at.center()),
        Some(200),
        "the float drawn on top takes the pointer"
    );
    let mut row = row;
    row.set_floats(floats(&[(100, at)]));
    assert_eq!(
        row.float_group_at(Point::new(10.0, 10.0)),
        None,
        "the strip outside every float is the strip's"
    );
}

#[test]
fn letting_a_window_go_off_its_strip_lifts_it_where_the_hand_was() {
    let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
    let grab = group_grab_point(&row, 0);
    press_at(&mut row, &taffy, root, grab);
    let away = Point::new(grab.x + 40.0, row.get_rect().y1 + 180.0);
    dispatch_at(&mut row, &taffy, root, &move_to(away));
    assert!(
        row.group_drag_active(),
        "fixture: the window is being moved"
    );
    release(&mut row, &taffy, root);

    let Some(FloatAsk::Float { group, at }) = row.take_pending_float() else {
        panic!("a window let go off its strip floats");
    };
    assert_eq!(group, 100);
    assert_eq!(
        at,
        row.float_rect_at(away),
        "the window hangs from the hand that let it go"
    );
    assert_eq!(
        row.group_ids(),
        vec![100, 200, 300],
        "it is not also reordered in the strip it left"
    );
    assert_eq!(row.take_pending_group_reorder(), None);
}

#[test]
fn letting_a_window_go_over_its_strip_still_lands_in_the_gap_it_was_dropped_in() {
    let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
    let grab = group_grab_point(&row, 0);
    press_at(&mut row, &taffy, root, grab);
    let past = Point::new(row.groups[2].get_rect().center().x + 60.0, grab.y);
    dispatch_at(&mut row, &taffy, root, &move_to(past));
    release(&mut row, &taffy, root);
    assert_eq!(row.group_ids(), vec![200, 300, 100]);
    assert_eq!(row.take_pending_group_reorder(), Some((100, 2)));
    assert_eq!(
        row.take_pending_float(),
        None,
        "a drop the strip accepts is no float"
    );
}

#[test]
fn a_float_keeps_its_card_room_inside_the_desk() {
    let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
    let area = row.get_rect();
    let room = FLOAT_CARD_ROOM;
    let corner = Rect::from_origin_size(Point::new(area.x0, area.y0), FLOAT_SIZE);
    let placed = row.keep_float_inside(corner);
    assert_eq!(
        placed.origin(),
        Point::new(area.x0 + room, area.y0 + room),
        "a float at the desk's corner stands clear of it, so its band and ring show"
    );
    let past = Rect::from_origin_size(Point::new(area.x1, area.y1), FLOAT_SIZE);
    let past = row.keep_float_inside(past);
    assert_eq!(
        (past.x1, past.y1),
        (area.x1 - room, area.y1 - room),
        "a float pushed past the desk comes back with its card on it"
    );
    let inside = Rect::from_origin_size(Point::new(area.x0 + 120.0, area.y0 + 60.0), FLOAT_SIZE);
    assert_eq!(
        row.keep_float_inside(inside),
        inside,
        "a float with room round it stands where it was put"
    );
}

#[test]
fn a_floating_window_follows_the_hand_and_snaps_back_into_the_gap_over_the_strip() {
    let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
    let at = Rect::new(120.0, 500.0, 760.0, 900.0);
    let mut row = row;
    row.set_floats(floats(&[(300, at)]));
    let (mut row, taffy, root) = relayout_row_owned(row, FIXTURE_USABLE_PX);
    let grab = group_grab_point(&row, 2);
    press_at(&mut row, &taffy, root, grab);
    let by = Vec2::new(90.0, 260.0);
    dispatch_at(&mut row, &taffy, root, &move_to(grab + by));
    assert_eq!(
        row.floats().get(&300),
        Some(&(at + by)),
        "the floating window is carried by the hand"
    );
    assert!(
        row.group_reorder_target().is_none(),
        "off the strip there is no gap to land in yet"
    );

    let over = Point::new(
        row.groups[0].get_rect().center().x,
        row.groups[0].get_rect().y0 + 12.0,
    );
    dispatch_at(&mut row, &taffy, root, &move_to(over));
    assert!(
        row.group_reorder_target().is_some(),
        "over the strip the gap it would land in shows"
    );
    release(&mut row, &taffy, root);
    assert_eq!(
        row.take_pending_float(),
        Some(FloatAsk::Snap { group: 300 }),
        "let go over the strip, it snaps back"
    );

    row.set_floats(HashMap::new());
    let (row, _taffy, _root) = relayout_row_owned(row, FIXTURE_USABLE_PX);
    let strip_top = row.get_rect().y0;
    let snapped = row
        .groups
        .iter()
        .find(|group| group.id == 300)
        .map(|group| group.get_rect())
        .expect("fixture: the window is still there");
    assert!(
        (snapped.y0 - strip_top).abs() < 32.0,
        "it stands in the strip again, not over it: {snapped:?}"
    );
    assert_eq!(row.group_ids(), vec![100, 300, 200]);
    assert!(
        snapped.x0 < row.groups[2].get_rect().x0,
        "it landed in the gap it was dropped in, ahead of the window it passed"
    );
}

#[test]
fn a_float_let_go_off_the_strip_stays_over_the_desk_where_it_was_dropped() {
    let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
    let at = Rect::new(120.0, 60.0, 760.0, 540.0);
    let mut row = row;
    row.set_floats(floats(&[(200, at)]));
    let (mut row, taffy, root) = relayout_row_owned(row, FIXTURE_USABLE_PX);
    let grab = group_grab_point(&row, 1);
    press_at(&mut row, &taffy, root, grab);
    let away = grab + Vec2::new(250.0, 700.0);
    dispatch_at(&mut row, &taffy, root, &move_to(away));
    release(&mut row, &taffy, root);
    let ask = row.take_pending_float();
    let Some(FloatAsk::Float { group, at: moved }) = ask else {
        panic!("a float let go off the strip stays floating: {ask:?}");
    };
    assert_eq!(group, 200);
    assert_eq!(moved, row.float_rect_at(away));
    assert_eq!(
        row.floats().get(&200),
        Some(&moved),
        "the window stands where it was let go"
    );
}

#[test]
fn a_float_covers_the_windows_under_it_once_it_has_arrived() {
    use crate::motion::pin_frame_at;

    let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
    let at = Rect::new(120.0, 60.0, 760.0, 540.0);
    let mut now = crate::time::now();
    let mut row = row;
    {
        let _frame = pin_frame_at(now);
        row.set_floats(floats(&[(100, at)]));
    }
    let (mut row, _taffy, _root) = relayout_row_owned(row, FIXTURE_USABLE_PX);
    let theme = Theme::dark();
    let text = &mut TextRenderer::new();
    let presence = PresenceCtx::empty();
    let mut draw = |row: &mut GroupRow, at: crate::time::Instant| {
        let _frame = pin_frame_at(at);
        let mut scene = Scene::new();
        row.draw(&mut scene, Affine::IDENTITY, text, &theme, &presence);
    };
    // The frame it first shows on fades it in over the desk: what it covers still shows through, so
    // the windows under it are drawn as they stand.
    draw(&mut row, now);
    assert!(
        row.drawn_covers().is_empty(),
        "a float still arriving covers nothing"
    );
    for _ in 0..600 {
        if !row.drawn_covers().is_empty() {
            break;
        }
        now += std::time::Duration::from_millis(16);
        draw(&mut row, now);
    }
    let covers = row.drawn_covers();
    assert_eq!(
        covers.len(),
        1,
        "the one float stands over the desk: {covers:?}"
    );
    let (tab, cover) = covers[0];
    let card = row.groups[0].get_rect();
    let bar = row.groups[0].bar_rect();
    assert_eq!(tab, 1, "the cover is of the window it shows, by its tab");
    assert_eq!(
        card.union(bar),
        card,
        "the float is one card: its bar is the head of the window it shows, inside the card: \
         bar {bar:?} against card {card:?}"
    );
    assert!(
        bar.height() > 0.0 && card.height() > bar.height(),
        "and the window's body under it: bar {bar:?} against card {card:?}"
    );
    assert_eq!(
        cover.union(card),
        cover,
        "the cover holds the whole card, bar and body: {cover:?} against {card:?}"
    );
    assert!(
        cover.width() > card.width() && cover.height() > card.height(),
        "and the lift it stands on, so the windows under it are drawn around that too: \
         {cover:?} against {card:?}"
    );
}

#[test]
fn a_floating_window_s_maximize_control_snaps_it_back_where_it_stood() {
    let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
    let at = Rect::new(120.0, 60.0, 760.0, 540.0);
    row.set_floats(floats(&[(200, at)]));
    let (mut row, taffy, root) = relayout_row_owned(row, FIXTURE_USABLE_PX);
    assert_eq!(
        row.groups[1].resize(),
        crate::widget::window_controls::Resize::Snap,
        "a floating window's maximize control offers to snap it back"
    );
    assert_eq!(
        row.groups[0].resize(),
        crate::widget::window_controls::Resize::Maximize,
        "a window in its strip keeps maximize"
    );
    let control = row.groups[1]
        .presentation_bounds()
        .maximize
        .expect("the float's bar shows its controls");
    press_at(&mut row, &taffy, root, control.center());
    release(&mut row, &taffy, root);

    assert_eq!(
        row.take_pending_float(),
        Some(FloatAsk::Snap { group: 200 }),
        "one press snaps the float back"
    );
    assert!(
        !row.groups[1].maximized && row.groups[1].take_pending_maximize_change().is_none(),
        "the press never maximizes a float"
    );
    assert_eq!(
        row.groups.iter().map(|group| group.id).collect::<Vec<_>>(),
        [100, 200, 300],
        "it goes back to the place in its strip it floated from"
    );
    assert_eq!(row.take_pending_float(), None, "the snap is asked once");
}
