use super::*;
use crate::synthetic_input::{key_event, keyboard_input};
use winit::event::DeviceId;
use winit::keyboard::Key;

fn send(region: &mut ScrollRegion, event: &WindowEvent) -> bool {
    let mut tree = taffy::TaffyTree::<()>::new();
    let node = crate::widget::TaffyTreeExt::leaf_node(&mut tree, taffy::Style::default());
    let mut handled = false;
    let mut focus_target = None;
    let mut doc = automerge::AutoCommit::new();
    region.handle_event(
        event,
        &mut crate::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &winit::event::Modifiers::default(),
            state: crate::widget::Edits::lend(&mut doc),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &tree,
        },
        &Theme::dark(),
    );
    handled
}

fn moved(point: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    }
}

fn button(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

fn cancellation_journey(cancel: WindowEvent) {
    let theme = Theme::dark();
    let mut region = ScrollRegion::vertical();
    region.set_viewport(Rect::new(0.0, 0.0, 300.0, 200.0), &theme);
    region.set_content_size(280.0, 800.0);
    let track = region.v_track(&theme).expect("vertical scrollbar");
    let thumb = region
        .vbar
        .thumb_rect(track, region.v_metrics())
        .expect("painted thumb");
    send(&mut region, &moved(thumb.center()));
    assert!(send(&mut region, &button(ElementState::Pressed)));
    assert!(region.is_dragging());
    let original = region.offset();
    send(&mut region, &cancel);
    assert!(
        !region.is_dragging(),
        "interrupted shared scrollbar must release capture"
    );
    send(&mut region, &moved(thumb.center() + Vec2::new(0.0, 50.0)));
    send(&mut region, &button(ElementState::Released));
    assert_eq!(
        region.offset(),
        original,
        "late input must not continue the cancelled drag"
    );
    send(&mut region, &moved(thumb.center()));
    assert!(send(&mut region, &button(ElementState::Pressed)));
    send(&mut region, &moved(thumb.center() + Vec2::new(0.0, 50.0)));
    assert!(
        region.offset().y > original.y,
        "a fresh scrollbar drag must still work"
    );
    send(&mut region, &button(ElementState::Released));
    assert!(!region.is_dragging());
}

#[test]
fn focus_loss_releases_shared_scrollbar_capture() {
    cancellation_journey(WindowEvent::Focused(false));
}

#[test]
fn escape_releases_shared_scrollbar_capture() {
    cancellation_journey(keyboard_input(key_event(
        Key::Named(NamedKey::Escape),
        None,
        ElementState::Pressed,
    )));
}

fn metrics_change_journey(change_content: bool) {
    let theme = Theme::dark();
    let mut region = ScrollRegion::vertical();
    let viewport = Rect::new(0.0, 0.0, 300.0, 200.0);
    region.set_viewport(viewport, &theme);
    region.set_content_size(280.0, 800.0);
    let track = region.v_track(&theme).expect("vertical scrollbar");
    let thumb = region
        .vbar
        .thumb_rect(track, region.v_metrics())
        .expect("painted thumb");
    send(&mut region, &moved(thumb.center()));
    assert!(send(&mut region, &button(ElementState::Pressed)));
    assert!(region.is_dragging());
    region.set_viewport(viewport, &theme);
    region.set_content_size(280.0, 800.0);
    assert!(
        region.is_dragging(),
        "stable layout calls must preserve an ongoing drag"
    );
    if change_content {
        region.set_content_size(280.0, 1000.0);
    } else {
        region.set_viewport(Rect::new(0.0, 0.0, 300.0, 240.0), &theme);
    }
    assert!(
        !region.is_dragging(),
        "changed scroll geometry must retire old capture"
    );
    let original = region.offset();
    send(&mut region, &moved(thumb.center() + Vec2::new(0.0, 50.0)));
    send(&mut region, &button(ElementState::Released));
    assert_eq!(
        region.offset(),
        original,
        "late input must not drag with old metrics"
    );
    let track = region.v_track(&theme).expect("updated vertical scrollbar");
    let thumb = region
        .vbar
        .thumb_rect(track, region.v_metrics())
        .expect("updated thumb");
    send(&mut region, &moved(thumb.center()));
    assert!(send(&mut region, &button(ElementState::Pressed)));
    send(&mut region, &moved(thumb.center() + Vec2::new(0.0, 50.0)));
    assert!(
        region.offset().y > original.y,
        "fresh drag must use the new geometry"
    );
    send(&mut region, &button(ElementState::Released));
    assert!(!region.is_dragging());
}

#[test]
fn viewport_change_retires_capture_but_stable_layout_preserves_it() {
    metrics_change_journey(false);
}

#[test]
fn content_change_retires_capture_but_stable_layout_preserves_it() {
    metrics_change_journey(true);
}
