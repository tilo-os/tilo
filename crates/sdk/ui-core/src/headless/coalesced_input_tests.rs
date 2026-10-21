//! A host on its own frame clock (a browser page) takes the frame that typing
//! and pointer motion ask for on its next frame: each key reaches its widget
//! at once, and the layout runs once for all of them, not once inside every
//! event handler and again for the screen.
use super::*;
use crate::widget::TextInput;

#[test]
fn keys_typed_between_frames_lay_out_once_on_the_hosts_frame() {
    let mut frame = FrameHarness::new(State::new(), Box::new(TextInput::local()), (400, 80));
    frame.set_coalesce_input_frames(true);
    frame.root_mut::<TextInput>().set_focus(true);
    frame.relayout();
    let before = frame.relayout_frames();
    for typed in ["h", "e", "l", "l", "o", "!"] {
        frame.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(typed.into())));
    }
    frame.send_host_event(&crate::synthetic_input::named_key(
        winit::keyboard::NamedKey::Backspace,
    ));
    frame.send_host_event(&crate::synthetic_input::cursor_moved(20.0, 20.0));
    assert_eq!(
        frame.root_mut::<TextInput>().value(),
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
    let mut frame = FrameHarness::new(State::new(), Box::new(TextInput::local()), (400, 80));
    frame.root_mut::<TextInput>().set_focus(true);
    frame.relayout();
    let before = frame.relayout_frames();
    for typed in ["o", "k"] {
        frame.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(typed.into())));
    }
    assert_eq!(frame.root_mut::<TextInput>().value(), "ok");
    assert_eq!(frame.relayout_frames(), before + 2);
}
