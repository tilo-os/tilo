//! The clipboard on a headless compositor with real Wayland clients: what a client copies is read
//! as its bytes arrive, secret offers are kept out of the history, and only clients the session
//! trusts see every selection through data control.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::io::Read as _;
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::Duration;

use tilo_compositor::clipboard_history::{
    History, PASSWORD_MANAGER_HINT_MIME, Pulled, SECRET_MIME, TEXT_MIME, kept_mime,
};
use tilo_compositor::{Compositor, Selection, StackPolicy};
use tilo_session_core::surface::{HostLayer, SurfaceHost};

use crate::harness::{TestClient, headless, mapped_client, pump_one as pump, settle};

/// Offer `formats` as the clipboard, serving `payload` for each.
fn copy(client: &mut TestClient, formats: &[&str], payload: &[u8]) {
    let source = client.data_source(formats, payload);
    // Data control's serials and this one are not tracked: what the protocol asks of a copy is
    // that its client has the keyboard.
    client
        .seen
        .data_device
        .as_ref()
        .unwrap()
        .set_selection(Some(&source), 0);
}

/// A compositor with one output and a client whose window has the keyboard.
fn setup() -> (Compositor, TestClient) {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "editor", (200, 100), [0, 255, 0, 255]);
    client.own_data_device(&mut compositor);
    (compositor, client)
}

/// A client that binds its data device while it already holds the keyboard is offered the
/// selection waiting on the seat: a toolkit that binds the seat late still pastes.
#[test]
fn a_data_device_bound_after_the_focus_is_offered_the_selection() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "editor", (200, 100), [0, 255, 0, 255]);
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.keyboard_focused
    });
    compositor
        .set_selection(
            Selection::Clipboard,
            vec![(TEXT_MIME.to_owned(), b"copied before the bind".to_vec())],
        )
        .expect("the selection");
    assert!(
        client.seen.data_device.is_none(),
        "the client has the focus before it binds its device"
    );
    assert!(client.seen.offered.is_empty(), "nothing listens yet");

    client.own_data_device(&mut compositor);
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.offered.iter().any(|mime| mime == TEXT_MIME)
    });

    let offer = client.seen.selection.clone().expect("an offer");
    let (mut reader, writer) = std::io::pipe().expect("pipe");
    offer.receive(TEXT_MIME.into(), writer.as_fd());
    drop(writer);
    settle(&mut compositor, &mut client);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).expect("read");
    assert_eq!(bytes, b"copied before the bind");
}

/// A surface of the session's own that holds the keyboard, as the shell's search does.
struct Typed;

impl SurfaceHost for Typed {
    fn layers(&self) -> Vec<HostLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn wants_keyboard(&self) -> bool {
        true
    }
}

#[test]
fn only_a_trusted_client_is_offered_the_selection_data_control() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    let mut untrusted = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut untrusted, |_, client| {
        !client.seen.globals.is_empty()
    });
    let globals = untrusted.seen.globals.clone();
    assert!(
        globals
            .iter()
            .any(|global| global == "wl_data_device_manager"),
        "every client copies and pastes: {globals:?}"
    );
    assert!(
        !globals
            .iter()
            .any(|global| global == "zwlr_data_control_manager_v1"
                || global == "ext_data_control_manager_v1"),
        "an untrusted app must not see every selection: {globals:?}"
    );

    let mut trusted = TestClient::connect_trusted(&mut compositor);
    pump(&mut compositor, &mut trusted, |_, client| {
        !client.seen.globals.is_empty()
    });
    let globals = &trusted.seen.globals;
    assert!(
        globals
            .iter()
            .any(|global| global == "zwlr_data_control_manager_v1"),
        "a clipboard manager the session trusts sees it: {globals:?}"
    );
    assert!(
        globals
            .iter()
            .any(|global| global == "ext_data_control_manager_v1"),
        "and its successor: {globals:?}"
    );
}

#[test]
fn a_clients_copy_is_read_as_its_bytes_arrive_and_secrets_are_kept_out() {
    let (mut compositor, mut client) = setup();
    let before = compositor.selection_changes(Selection::Clipboard);

    // The shell's search takes the keys before the app's copy reaches the compositor: the copy,
    // made while the app had them, is still taken.
    compositor.add_host(Rc::new(RefCell::new(Typed)));
    compositor
        .dispatch(Duration::from_millis(2))
        .expect("dispatch");
    copy(&mut client, &[TEXT_MIME], b"copied in the editor");
    pump(&mut compositor, &mut client, |compositor, _| {
        compositor.selection_changes(Selection::Clipboard) != before
    });
    let mimes = compositor.selection_formats(Selection::Clipboard);
    assert_eq!(mimes, [TEXT_MIME.to_owned()]);
    let mime = kept_mime(&mimes).expect("text is kept");

    // The read is the loop's: nothing arrives until the app is asked, and asking never blocks the
    // loop.
    let mut pull = compositor
        .pull_selection(Selection::Clipboard, mime)
        .expect("the read starts");
    assert_eq!(
        pull.drain(),
        Pulled::Pending,
        "the app has not answered yet"
    );

    let mut bytes = None;
    for _ in 0..500 {
        match pull.drain() {
            Pulled::Ready(read) => {
                bytes = Some(read);
                break;
            }
            Pulled::Pending => {
                client.exchange();
                compositor
                    .dispatch(Duration::from_millis(2))
                    .expect("dispatch");
            }
            Pulled::Lost => panic!("the copy was lost"),
        }
    }
    let bytes = bytes.expect("the copy arrived");
    assert_eq!(bytes, b"copied in the editor");
    assert_eq!(client.seen.sent, [TEXT_MIME.to_owned()]);

    let mut history = History::default();
    assert!(history.record(&mimes, mime, bytes, 1));
    assert_eq!(
        history.copies().next().map(|copy| copy.text_preview()),
        Some(Some("copied in the editor"))
    );

    // The same app copies a password: the marker travels with the copy, and the history keeps none
    // of it.
    let marked = compositor.selection_changes(Selection::Clipboard);
    copy(
        &mut client,
        &[TEXT_MIME, SECRET_MIME, PASSWORD_MANAGER_HINT_MIME],
        b"hunter2",
    );
    pump(&mut compositor, &mut client, |compositor, _| {
        compositor.selection_changes(Selection::Clipboard) != marked
    });
    let mimes = compositor.selection_formats(Selection::Clipboard);
    assert!(mimes.iter().any(|mime| mime == SECRET_MIME));
    assert_eq!(kept_mime(&mimes), None);
    assert_eq!(
        history.len(),
        1,
        "only the copy the person may keep is kept"
    );
}
