//! A host on its own frame clock (a browser page) takes the frame that typing
//! asks for on its next frame: each key reaches its widget
//! at once, and the layout runs once for all of them, not once inside every
//! event handler and again for the screen.
use super::*;
use crate::widget::TextInput;

#[test]
fn keys_typed_between_frames_lay_out_once_on_the_hosts_frame() {
    let mut frame = FrameHarness::for_root(
        State::new(),
        TextInput::local(),
        (400, 80),
        crate::Theme::dark(),
    );
    frame.set_coalesce_input_frames(true);
    frame.root_mut::<TextInput>().set_focus(true);
    // The first key switches the window to keyboard use; the host's next
    // frame lays that out.
    frame.send_host_event(&crate::synthetic_input::named_key(
        winit::keyboard::NamedKey::End,
    ));
    frame.relayout();
    let before = frame.relayout_frames();
    for typed in ["h", "e", "l", "l", "o", "!"] {
        frame.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(typed.into())));
    }
    frame.send_host_event(&crate::synthetic_input::named_key(
        winit::keyboard::NamedKey::Backspace,
    ));
    assert_eq!(
        frame.root::<TextInput>().value(),
        "hello",
        "every key reached the field as it came"
    );
    assert_eq!(
        frame.relayout_frames(),
        before,
        "no frame is laid out inside an event handler"
    );
    assert!(
        frame.frame_schedule().queued_work,
        "the input asks for a frame"
    );
    assert!(frame.poll_frame().unwrap());
    assert_eq!(
        frame.relayout_frames(),
        before + 1,
        "the host's frame lays the input out once"
    );
    assert!(
        !frame.frame_schedule().queued_work,
        "that frame took all of it"
    );
}

#[test]
fn a_host_without_its_own_frame_clock_lays_out_each_key() {
    let mut frame = FrameHarness::for_root(
        State::new(),
        TextInput::local(),
        (400, 80),
        crate::Theme::dark(),
    );
    frame.root_mut::<TextInput>().set_focus(true);
    frame.relayout();
    let before = frame.relayout_frames();
    for typed in ["o", "k"] {
        frame.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(typed.into())));
    }
    assert_eq!(frame.root::<TextInput>().value(), "ok");
    assert_eq!(frame.relayout_frames(), before + 2);
}

/// Switching from the pointer to the keyboard, or pinching, between two
/// frames lays out nothing inside the event handlers: the window draws the
/// new modality and the zoom once, on its next frame, as the live window
/// does.
#[test]
fn a_change_of_modality_and_a_pinch_wait_for_the_hosts_frame() {
    let mut frame = FrameHarness::for_root(
        State::new(),
        TextInput::local(),
        (400, 80),
        crate::Theme::dark(),
    );
    frame.set_coalesce_input_frames(true);
    frame.root_mut::<TextInput>().set_focus(true);
    frame.send_host_event(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(10.0, 10.0),
    });
    frame.relayout();
    let before = frame.relayout_frames();
    frame.send_host_event(&crate::synthetic_input::named_key(
        winit::keyboard::NamedKey::End,
    ));
    frame.send_host_event(&WindowEvent::PinchGesture {
        device_id: winit::event::DeviceId::dummy(),
        delta: 0.02,
        phase: winit::event::TouchPhase::Moved,
    });
    assert_eq!(
        frame.relayout_frames(),
        before,
        "nothing is laid out inside an event handler"
    );
    assert!(frame.poll_frame().unwrap());
    assert_eq!(
        frame.relayout_frames(),
        before + 1,
        "the host's frame lays both out once"
    );
}

/// Other people's presence arrives many times a frame while they type and
/// move. A host on its own frame clock lays none of it out on arrival: its
/// next frame draws all of it, once.
#[test]
fn presence_arriving_between_frames_lays_out_once_on_the_hosts_frame() {
    let mut frame = FrameHarness::for_root(
        State::new(),
        TextInput::local(),
        (400, 80),
        crate::Theme::dark(),
    );
    frame.set_coalesce_input_frames(true);
    frame.relayout();
    let before = frame.relayout_frames();
    for (user, name) in [("a1", "Ada"), ("b2", "Bo"), ("c3", "Cy"), ("a1", "Ada L")] {
        frame.update_presence(|presence| {
            presence.set_display_name(crate::permissions::UserId(user.into()), name);
            presence.unregister_peer("gone");
            true
        });
    }
    assert_eq!(
        frame.relayout_frames(),
        before,
        "presence is laid out by no one but the next frame"
    );
    assert!(
        frame.frame_schedule().needs_redraw,
        "the presence asks for a frame"
    );
    assert!(frame.poll_frame().unwrap());
    assert_eq!(
        frame.relayout_frames(),
        before + 1,
        "the host's frame lays all of it out once"
    );
    assert!(
        !frame.queued_work_sources().contains(&"presence redraw"),
        "that frame took all of it"
    );
}
