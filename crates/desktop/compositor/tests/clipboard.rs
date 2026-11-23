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
use tilo_session_core::surface::{ShellSurface, SurfaceLayer};

use crate::harness::{
    TestClient, headless, mapped_client, pump as pump_many, pump_one as pump, settle,
};

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

impl ShellSurface for Typed {
    fn layers(&self) -> Vec<SurfaceLayer> {
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
    compositor.add_surface(Rc::new(RefCell::new(Typed)));
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

/// Set the selection from a connection the session handed to an app keeper, with the time its
/// app made the copy.
fn claim(client: &mut TestClient, formats: &[&str], payload: &[u8], at: u32) {
    let source = client.data_source(formats, payload);
    client
        .seen
        .data_device
        .as_ref()
        .unwrap()
        .set_selection(Some(&source), at);
}

/// Read the clipboard through the compositor, letting `client` answer as the loop turns.
fn read(compositor: &mut Compositor, client: &mut TestClient) -> Vec<u8> {
    let mut pull = compositor
        .pull_selection(Selection::Clipboard, TEXT_MIME)
        .expect("the read starts");
    for _ in 0..500 {
        match pull.drain() {
            Pulled::Ready(bytes) => return bytes,
            Pulled::Pending => {
                client.exchange();
                compositor
                    .dispatch(Duration::from_millis(2))
                    .expect("dispatch");
            }
            Pulled::Lost => panic!("the copy was lost"),
        }
    }
    panic!("the copy never arrived");
}

/// A client with a window that holds the keyboard, whose own writes are not part of a test.
fn focused(compositor: &mut Compositor) -> TestClient {
    let mut client = mapped_client(compositor, "editor", (200, 100), [0, 255, 0, 255]);
    client.own_data_device(compositor);
    pump(compositor, &mut client, |_, client| {
        client.seen.keyboard_focused
    });
    client
}

/// A connection the session handed to the process `pid`, with its data device.
fn handed(compositor: &mut Compositor) -> TestClient {
    let mut client = TestClient::connect_for(compositor, std::process::id());
    pump(compositor, &mut client, |_, client| {
        client.seen.seat.is_some()
    });
    client.own_data_device(compositor);
    client
}

#[test]
fn a_kept_client_offers_its_selection_before_its_window_has_the_focus() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut editor = focused(&mut compositor);
    let mut kept = handed(&mut compositor);
    assert!(kept.seen.seat.is_some(), "a seat for the handed connection");

    // The app behind the keeper copied on its last desktop; the new one has nothing, and the app
    // has not been focused yet.
    claim(&mut kept, &[TEXT_MIME], b"kept text", 1_000);
    pump_many(
        &mut compositor,
        &mut [&mut editor, &mut kept],
        |compositor, _| {
            compositor
                .selection_formats(Selection::Clipboard)
                .iter()
                .any(|mime| mime == TEXT_MIME)
        },
    );
    assert_eq!(
        compositor.selection_formats(Selection::Clipboard),
        [TEXT_MIME.to_owned()]
    );
    assert_eq!(read(&mut compositor, &mut kept), b"kept text");
}

#[test]
fn an_ordinary_client_cannot_set_a_selection_without_focus() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _editor = focused(&mut compositor);
    let mut other = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut other, |_, client| {
        client.seen.seat.is_some()
    });
    other.own_data_device(&mut compositor);
    let before = compositor.selection_changes(Selection::Clipboard);

    let source = other.data_source(&[TEXT_MIME], b"not allowed");
    other
        .seen
        .data_device
        .as_ref()
        .unwrap()
        .set_selection(Some(&source), 0);
    settle(&mut compositor, &mut other);
    assert_eq!(
        compositor.selection_changes(Selection::Clipboard),
        before,
        "a client without the focus cannot copy"
    );
    assert!(
        compositor
            .selection_formats(Selection::Clipboard)
            .is_empty()
    );
}

#[test]
fn a_kept_clients_claim_loses_to_a_copy_made_first() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut editor = focused(&mut compositor);
    let mut kept = handed(&mut compositor);
    copy(&mut editor, &[TEXT_MIME], b"copied first");
    pump(&mut compositor, &mut editor, |compositor, _| {
        compositor
            .selection_formats(Selection::Clipboard)
            .iter()
            .any(|mime| mime == TEXT_MIME)
    });

    claim(&mut kept, &[TEXT_MIME], b"kept text", 1_000);
    settle(&mut compositor, &mut kept);
    assert_eq!(
        compositor.selection_changes(Selection::Clipboard),
        1,
        "the keeper's claim did not take the selection"
    );
    assert_eq!(read(&mut compositor, &mut editor), b"copied first");
}

#[test]
fn the_kept_client_whose_app_copied_last_wins() {
    // Two keepers claim in either order: the time the copy was made decides, not the order the
    // connections arrived in.
    for newer_first in [true, false] {
        let mut compositor = headless(Box::new(StackPolicy));
        let mut editor = focused(&mut compositor);
        let mut older = handed(&mut compositor);
        let mut newer = handed(&mut compositor);
        if newer_first {
            claim(&mut newer, &[TEXT_MIME], b"copied later", 2_000);
            settle(&mut compositor, &mut newer);
            claim(&mut older, &[TEXT_MIME], b"copied earlier", 1_000);
            settle(&mut compositor, &mut older);
        } else {
            claim(&mut older, &[TEXT_MIME], b"copied earlier", 1_000);
            settle(&mut compositor, &mut older);
            claim(&mut newer, &[TEXT_MIME], b"copied later", 2_000);
            settle(&mut compositor, &mut newer);
        }
        pump_many(
            &mut compositor,
            &mut [&mut editor, &mut older, &mut newer],
            |compositor, _| {
                compositor
                    .selection_formats(Selection::Clipboard)
                    .iter()
                    .any(|mime| mime == TEXT_MIME)
            },
        );
        assert_eq!(
            compositor.selection_formats(Selection::Clipboard),
            [TEXT_MIME.to_owned()]
        );
        assert_eq!(
            read(&mut compositor, &mut newer),
            b"copied later",
            "the app that copied last owns the clipboard, whichever keeper arrived last"
        );
    }
}

#[test]
fn a_later_copy_replaces_a_kept_clients_claim() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut editor = focused(&mut compositor);
    let mut kept = handed(&mut compositor);
    claim(&mut kept, &[TEXT_MIME], b"kept text", 1_000);
    pump_many(
        &mut compositor,
        &mut [&mut editor, &mut kept],
        |compositor, _| {
            compositor
                .selection_formats(Selection::Clipboard)
                .iter()
                .any(|mime| mime == TEXT_MIME)
        },
    );

    let before = compositor.selection_changes(Selection::Clipboard);
    copy(&mut editor, &[TEXT_MIME], b"copied later");
    pump(&mut compositor, &mut editor, |compositor, _| {
        compositor.selection_changes(Selection::Clipboard) != before
    });
    assert_eq!(read(&mut compositor, &mut editor), b"copied later");
}
