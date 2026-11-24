//! A dragged window near the strip's edge pans the strip, resting on a background tab brings it
//! to the front, and a bar drag carries the window's resource on the drag bus.

use super::tests::{
    dispatch_at, group_grab_point, make_row_with_groups_with_taffy, move_to, press_at,
};
use super::*;

const RESOURCE: &str = "notes.workspace/pages/p_crit";

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

/// The window `row` stands first, carrying `RESOURCE`, taken by its bar and carried to `to`.
fn lifted(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, to: Point) {
    row.groups[0].set_carries(Some(RESOURCE.to_owned()));
    let grab = group_grab_point(row, 0);
    press_at(row, taffy, root, grab);
    dispatch_at(row, taffy, root, &move_to(to));
}

tilo_test_env::scenario! {
    fn a_dragged_window_pans_the_strip_and_raises_a_tab;

    phase moving_a_whole_window_to_an_edge_pans_the_strip_and_keeps_it_under_the_pointer;
    {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
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

    phase a_drag_resting_on_a_background_tab_brings_it_to_the_front;
    {
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
}

tilo_test_env::scenario! {
    fn a_window_lifted_by_its_bar_carries_its_resource;
    phase a_travelling_press_on_a_windows_bar_puts_its_resource_in_flight;
    {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let grab = group_grab_point(&row, 0);
        let to = Point::new(row.groups[2].get_rect().center().x + 10.0, grab.y);
        lifted(&mut row, &taffy, root, to);
        let flight = crate::widget::drag_bus::current().expect("the drag carries the window");
        assert_eq!(flight.source, "window:100", "the flight is the window's own");
        let crate::dnd::DragPayload::Custom { kind, blob } = flight.payload else {
            panic!("a window's bar carries a resource");
        };
        assert_eq!(kind, crate::widget::drag_source::RESOURCE_PAYLOAD);
        assert_eq!(
            String::from_utf8(blob).expect("a resource is text"),
            RESOURCE
        );
        assert_eq!(flight.label, "T1", "over the pointer stands the window's name");

        release(&mut row, &taffy, root);
        assert!(
            crate::widget::drag_bus::current().is_none(),
            "letting go here takes the window off the bus"
        );
        assert!(
            row.take_pending_group_reorder().is_some(),
            "and the strip still rearranges under the pointer"
        );
    }

    phase a_window_carried_off_the_screen_rearranges_nothing_where_it_left;
    {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let grab = group_grab_point(&row, 0);
        let to = Point::new(row.groups[2].get_rect().center().x + 10.0, grab.y);
        lifted(&mut row, &taffy, root, to);
        assert!(row.group_drag_active(), "fixture: the window is being moved");
        // The pointer left the screen and another of the person's devices took the drag.
        crate::widget::drag_bus::carry_away();
        release(&mut row, &taffy, root);
        assert_eq!(row.group_ids(), vec![100, 200, 300], "nothing is reordered here");
        assert_eq!(row.take_pending_group_reorder(), None);
        assert_eq!(row.take_pending_float(), None, "and no window floats");
        assert!(crate::widget::drag_bus::current().is_none());
    }
}

/// The box `scene` paints, over a transparent frame: the smallest rectangle holding every
/// pixel with any alpha.
#[allow(
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "pixel tallies inside the rendered frame"
)]
fn painted_bounds(scene: &Scene, width: u32, height: u32) -> Rect {
    let mut renderer = crate::offscreen::OffscreenRenderer::new(width, height);
    let rgba = renderer.render(scene, vello::peniko::Color::TRANSPARENT);
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0_u32, 0_u32);
    for y in 0..height {
        for x in 0..width {
            if rgba[((y * width + x) * 4 + 3) as usize] == 0 {
                continue;
            }
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    assert!(x0 <= x1, "the dragged window's visual paints something");
    Rect::new(
        f64::from(x0),
        f64::from(y0),
        f64::from(x1.saturating_add(1)),
        f64::from(y1.saturating_add(1)),
    )
}

#[test]
fn a_dragged_window_is_painted_where_the_pointer_lifted_it() {
    let mut checked = Vec::new();
    for (name, theme) in [
        ("dark", Theme::dark().with_reduce_motion(true)),
        ("light", Theme::light().with_reduce_motion(true)),
    ] {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
        let rest = row.groups[0].get_rect();
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        let to = Point::new(grab.x + 240.0, grab.y + 16.0);
        dispatch_at(&mut row, &taffy, root, &move_to(to));
        assert!(
            row.group_drag_active(),
            "{name}: fixture: the window is being moved"
        );
        let mut text = crate::font_catalog::fixture_renderer();
        let mut scene = Scene::new();
        row.draw_group_drag_preview(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        crate::testing::capture_regression(
            "drag-lift",
            name,
            &scene,
            1200,
            700,
            theme.tokens.colors.surfaces.content,
        );
        let bounds = painted_bounds(&scene, 1200, 700);
        // Lifted: drawn a little smaller than it stands, about the point it was taken by,
        // which no rest transform does.
        let lifted = scene.encoding().transforms.iter().any(|transform| {
            let [a, b, c, d] = transform.matrix;
            (0.9..1.0).contains(&a) && (a - d).abs() < 1e-4 && b.abs() < 1e-4 && c.abs() < 1e-4
        });
        checked.push((name, bounds, rest, lifted));
    }
    for (name, bounds, rest, lifted) in checked {
        assert!(
            bounds.x0 > rest.x0 + 150.0,
            "{name}: the moved window is drawn under the pointer, at {bounds:?} rather than {rest:?}"
        );
        assert!(
            lifted,
            "{name}: the visual is drawn lifted, at a scale under 1: {bounds:?}"
        );
    }
}
