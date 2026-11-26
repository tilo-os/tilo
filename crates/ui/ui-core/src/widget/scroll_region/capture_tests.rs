use super::*;
use tilo_ui_base::{synthetic_input::key_event, synthetic_input::keyboard_input};
use winit::keyboard::Key;

fn send(region: &mut ScrollRegion, event: &WindowEvent) -> bool {
    let mut tree = taffy::TaffyTree::<()>::new();
    let node = tilo_ui_base::widget::TaffyTreeExt::leaf_node(&mut tree, taffy::Style::default());
    let mut handled = false;
    let mut focus_target = None;
    let mut doc = automerge::AutoCommit::new();
    region.handle_event(
        event,
        &mut tilo_ui_base::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &winit::event::Modifiers::default(),
            state: tilo_ui_base::widget::Edits::lend(&mut doc),
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
    tilo_ui_base::synthetic_input::cursor_moved(point.x, point.y)
}

fn button(state: ElementState) -> WindowEvent {
    tilo_ui_base::synthetic_input::mouse_input(state, MouseButton::Left)
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

tilo_test_env::scenario! {
    /// When a region lets go of a captured scrollbar.
    /// Each phase was a test of its own and builds its own fixture.
    fn shared_scrollbar_capture_is_released_or_retired;

    phase focus_loss_releases_shared_scrollbar_capture;
    {
        cancellation_journey(WindowEvent::Focused(false));
    }

    phase escape_releases_shared_scrollbar_capture;
    {
        cancellation_journey(keyboard_input(key_event(
            Key::Named(NamedKey::Escape),
            None,
            ElementState::Pressed,
        )));
    }

}
