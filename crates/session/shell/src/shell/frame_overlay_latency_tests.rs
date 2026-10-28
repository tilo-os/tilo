//! Event-time layout work must follow changes, not an overlay's visibility.
use super::*;
use winit::event::{ElementState, Modifiers};
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn key(key: Key, text: Option<&str>, state: ElementState) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        key,
        text.map(Into::into),
        state,
    ))
}

#[test]
fn open_search_does_not_solve_layout_for_pointer_motion_or_key_releases() {
    let mut frame = FrameBuilder::new((1280, 800));
    let mut doc = AutoCommit::new();
    let mut focus = None;
    frame.relayout();
    frame.reconcile_native_focus(&mut focus);
    let previous_focus = focus;
    assert!(frame.dispatch_event(
        &key(Key::Named(NamedKey::Space), None, ElementState::Pressed),
        &Modifiers::from(ModifiersState::SUPER),
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &mut focus,
    ));
    frame.pump_apps(&mut doc, &automerge::ROOT);
    frame.relayout();
    assert!(frame.host().palette_visible());
    assert_eq!(focus, frame.palette_focus_target());
    let passes = frame.layout_passes;
    for x in [1.0, 2.0, 3.0, 4.0] {
        frame.dispatch_event(
            &ui::synthetic_input::cursor_moved(x, 1.0),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        );
        frame.dispatch_event(
            &key(Key::Named(NamedKey::Space), None, ElementState::Released),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        );
    }
    assert_eq!(
        frame.layout_passes, passes,
        "unchanged input must not solve the desktop"
    );
    assert_eq!(focus, frame.palette_focus_target());
    assert!(frame.dispatch_event(
        &key(Key::Character("a".into()), Some("a"), ElementState::Pressed),
        &Modifiers::default(),
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &mut focus,
    ));
    assert_eq!(frame.host().palette_query(), "a");
    assert!(
        frame.layout_passes > passes,
        "changed results settle in the same input event"
    );
    assert_eq!(focus, frame.palette_focus_target());
    let before_close = frame.layout_passes;
    assert!(frame.dispatch_event(
        &key(Key::Named(NamedKey::Escape), None, ElementState::Pressed),
        &Modifiers::default(),
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &mut focus,
    ));
    assert!(
        frame.host().palette_visible(),
        "the first Escape clears the query"
    );
    assert_eq!(frame.host().palette_query(), "");
    assert_eq!(focus, frame.palette_focus_target());
    assert!(frame.dispatch_event(
        &key(Key::Named(NamedKey::Escape), None, ElementState::Pressed),
        &Modifiers::default(),
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &mut focus,
    ));
    assert!(!frame.host().palette_visible());
    assert!(frame.layout_passes > before_close);
    assert_eq!(focus, previous_focus);
    drop(frame);
}
