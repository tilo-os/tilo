//! While the session is locked the keyboard is the lock's: a window that opens or asks for the
//! keyboard behind the lock never takes it, and an input method composing when the lock came takes
//! no key the person types on the lock. Nothing the person types after the lock goes anywhere
//! but to what unlocks.

use super::*;

use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::ExtSessionLockV1;

/// The session locked by a trusted client, as the lock screen locks it: the client, and the lock it
/// holds.
fn locked(
    compositor: &mut Compositor,
    others: &mut [&mut TestClient],
) -> (TestClient, ExtSessionLockV1) {
    let socket = compositor.connect_trusted_client().unwrap();
    let connection = Connection::from_socket(socket).unwrap();
    let queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut locker = TestClient {
        connection,
        queue,
        seen: Seen::default(),
        surface: None,
        toplevel: None,
    };
    pump(compositor, &mut [&mut locker], |_, clients| {
        clients[0].seen.lock_manager.is_some()
    });
    let lock = locker
        .seen
        .lock_manager
        .as_ref()
        .unwrap()
        .lock(&locker.queue.handle(), ());
    pump(compositor, &mut [&mut locker], |compositor, _| {
        compositor.locked()
    });
    // The lock holds once a frame without the desktop is drawn: the client is told so.
    compositor.draw_frame().expect("a frame");
    pump(compositor, &mut [&mut locker], |_, clients| {
        clients[0].seen.locked
    });
    for other in others {
        pump(compositor, &mut [&mut **other], |_, _| true);
    }
    (locker, lock)
}

/// A key pressed and let go, and the clients given what came of it.
fn tap(compositor: &mut Compositor, clients: &mut [&mut TestClient]) {
    for pressed in [true, false] {
        compositor
            .input(InputEvent::Key {
                keycode: KEY_A,
                pressed,
            })
            .unwrap();
    }
    pump(compositor, clients, |_, _| true);
}

#[test]
fn a_window_that_opens_behind_the_lock_takes_no_key() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut window = mapped_client(&mut compositor, "private", (200, 100), [255, 0, 0, 255]);
    let (mut locker, _lock) = locked(&mut compositor, &mut [&mut window]);
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        !clients[0].seen.keyboard_focused
    });

    // A window opens while the session is locked: the policy would give it the keyboard.
    let mut late = mapped_client(&mut compositor, "late", (200, 100), [0, 0, 255, 255]);
    tap(&mut compositor, &mut [&mut late, &mut window, &mut locker]);
    assert!(
        !late.seen.keyboard_focused && late.seen.keys.is_empty(),
        "a window that opened behind the lock has no key"
    );
    assert!(
        window.seen.keys.is_empty(),
        "nor has the window the lock covered"
    );
}

#[test]
fn an_input_method_composing_when_the_lock_comes_takes_no_key_typed_on_it() {
    let (mut compositor, mut client, mut engine) = super::ime::window_and_engine();
    client.own_text_field();
    super::ime::serving(&mut compositor, &mut client, &mut engine);
    let composed = engine.seen.grab_keys.len();
    let typed = client.seen.keys.len();

    let (mut locker, lock) = locked(&mut compositor, &mut [&mut client, &mut engine]);
    tap(
        &mut compositor,
        &mut [&mut client, &mut engine, &mut locker],
    );
    assert_eq!(
        engine.seen.grab_keys.len(),
        composed,
        "the input method hears no key typed on the lock"
    );
    assert_eq!(
        client.seen.keys.len(),
        typed,
        "nor does the field it composed in"
    );

    // Unlocked, the input method composes again: it has the keyboard back.
    lock.unlock_and_destroy();
    pump(&mut compositor, &mut [&mut locker], |compositor, _| {
        !compositor.locked()
    });
    tap(
        &mut compositor,
        &mut [&mut client, &mut engine, &mut locker],
    );
    assert!(
        engine.seen.grab_keys.len() > composed,
        "the input method hears the keys again once the session is unlocked"
    );
}
