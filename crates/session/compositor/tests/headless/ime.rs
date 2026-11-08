//! An engine composing for the session: it holds the seat's keyboard, the field the person types
//! into hears what it composes — a client's field or one the session draws itself — its candidate
//! window stands where the caret does, and the desk's chords still work while it composes.

use super::*;

/// A window that has the keyboard, and the seat as an engine's: a trusted client that binds the
/// input method and holds the keyboard grab.
fn window_and_engine() -> (Compositor, TestClient, TestClient) {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "form", (200, 100), [0, 255, 0, 255]);
    click(&mut compositor, 320.0, 240.0);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused && clients[0].seen.keymap.is_some()
    });
    let mut engine = TestClient::connect_trusted(&mut compositor);
    pump(
        &mut compositor,
        &mut [&mut client, &mut engine],
        |_, clients| {
            clients[1].seen.input_method_manager.is_some() && clients[1].seen.seat.is_some()
        },
    );
    engine.own_input_method();
    pump(
        &mut compositor,
        &mut [&mut client, &mut engine],
        |_, clients| clients[1].seen.input_method.is_some(),
    );
    engine.grab_keyboard();
    pump(&mut compositor, &mut [&mut client, &mut engine], |_, _| {
        true
    });
    (compositor, client, engine)
}

/// A hosted surface drawing a text field at `caret` , with `size` .
fn hosted_field(compositor: &mut Compositor, caret: (f64, f64, f64, f64)) -> Rc<RefCell<HostLog>> {
    let log = Rc::new(RefCell::new(HostLog::default()));
    log.borrow_mut().wants_keyboard = true;
    log.borrow_mut().field = Some(HostTextInput {
        hint: 0,
        purpose: 0,
        x: caret.0,
        y: caret.1,
        width: caret.2,
        height: caret.3,
    });
    compositor.add_host(Box::new(Square::new(
        (0, 0, 10, 10),
        [0, 255, 0, 255],
        Rc::clone(&log),
    )));
    log
}

/// Whether the engine was told to compose in the field it serves.
fn serving(compositor: &mut Compositor, client: &mut TestClient, engine: &mut TestClient) {
    pump(compositor, &mut [client, engine], |compositor, clients| {
        compositor.text_entry().wants_typing && clients[1].seen.ime_activated
    });
}

#[test]
fn the_engine_composes_into_a_client_field_while_it_holds_the_keyboard() {
    let (mut compositor, mut client, mut engine) = window_and_engine();
    client.own_text_field();
    serving(&mut compositor, &mut client, &mut engine);

    // What the engine composes lands in the field.
    engine.type_with_input_method("안");
    pump(
        &mut compositor,
        &mut [&mut client, &mut engine],
        |_, clients| !clients[0].seen.committed.is_empty(),
    );
    assert_eq!(client.seen.committed, vec!["안".to_owned()]);

    // Every key the person types reaches the engine first: the field hears only what the engine
    // commits, so the chord keys it would have had are the engine's.
    let before = client.seen.keys.len();
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .unwrap();
    pump(
        &mut compositor,
        &mut [&mut client, &mut engine],
        |_, clients| !clients[1].seen.grab_keys.is_empty(),
    );
    assert_eq!(engine.seen.grab_keys, vec![(KEY_A, true)]);
    assert_eq!(
        client.seen.keys.len(),
        before,
        "the engine has the keyboard, not the field"
    );
}

#[test]
fn the_engine_composes_into_a_hosted_field() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "form", (200, 100), [0, 255, 0, 255]);
    click(&mut compositor, 320.0, 240.0);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused && clients[0].seen.keymap.is_some()
    });
    let log = hosted_field(&mut compositor, (120.0, 50.0, 2.0, 20.0));
    let mut engine = TestClient::connect_trusted(&mut compositor);
    pump(
        &mut compositor,
        &mut [&mut client, &mut engine],
        |_, clients| {
            clients[1].seen.input_method_manager.is_some() && clients[1].seen.seat.is_some()
        },
    );
    engine.own_input_method();
    serving(&mut compositor, &mut client, &mut engine);
    engine.grab_keyboard();
    pump(&mut compositor, &mut [&mut client, &mut engine], |_, _| {
        true
    });

    // The field was told an engine composes in it, and where its caret is.
    assert_eq!(log.borrow().ime, vec![HostIme::Enabled]);

    // The composition in progress reaches the field, then the text chosen.
    engine.preedit_with_input_method("ㅎ", Some((3, 3)));
    pump(&mut compositor, &mut [&mut client, &mut engine], |_, _| {
        true
    });
    engine.type_with_input_method("한");
    pump(&mut compositor, &mut [&mut client, &mut engine], |_, _| {
        log.borrow()
            .ime
            .iter()
            .any(|event| matches!(event, HostIme::Commit(_)))
    });
    assert_eq!(
        log.borrow().ime,
        vec![
            HostIme::Enabled,
            HostIme::Preedit("ㅎ".to_owned(), Some((3, 3))),
            HostIme::Commit("한".to_owned()),
        ]
    );

    // The field takes no key of its own while the engine composes in it: the engine has the
    // keyboard.
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .unwrap();
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: false,
        })
        .unwrap();
    pump(&mut compositor, &mut [&mut client, &mut engine], |_, _| {
        true
    });
    assert!(log.borrow().keys.is_empty(), "{:?}", log.borrow().keys);
    assert_eq!(engine.seen.grab_keys, vec![(KEY_A, true), (KEY_A, false)]);

    // What the engine leaves for the field comes back through its keyboard,
    // routed as the person's own key.
    let keymap = client.seen.keymap.clone().expect("a keymap");
    engine.virtual_key(&keymap, KEY_A, true);
    engine.virtual_key(&keymap, KEY_A, false);
    pump(&mut compositor, &mut [&mut client, &mut engine], |_, _| {
        log.borrow().keys.len() == 2
    });
    assert_eq!(
        log.borrow()
            .keys
            .iter()
            .map(|key| (key.keycode, key.pressed))
            .collect::<Vec<_>>(),
        vec![(KEY_A, true), (KEY_A, false)]
    );
}

#[test]
fn the_candidate_window_stands_under_the_caret() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = hosted_field(&mut compositor, (120.0, 50.0, 2.0, 20.0));
    let mut engine = TestClient::connect_trusted(&mut compositor);
    pump(&mut compositor, &mut [&mut engine], |_, clients| {
        clients[0].seen.input_method_manager.is_some() && clients[0].seen.seat.is_some()
    });
    engine.own_input_method();
    pump(
        &mut compositor,
        &mut [&mut engine],
        |compositor, clients| compositor.text_entry().wants_typing && clients[0].seen.ime_activated,
    );
    engine.own_popup();
    pump(&mut compositor, &mut [&mut engine], |_, clients| {
        !clients[0].seen.popup_rects.is_empty()
    });
    assert_eq!(
        engine.seen.popup_rects.last(),
        Some(&(120, 50, 2, 20)),
        "the field's caret, as the compositor serves it"
    );

    // What the engine draws in it stands under the text: the bottom of the caret, not the top of
    // the display.
    engine.paint_popup(40, [255, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut engine], |_, _| true);
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(130, 80), Some([255, 0, 255, 255]));
    assert!(
        log.borrow().ime.contains(&HostIme::Enabled),
        "it is the field's candidate window"
    );
}

#[test]
fn the_desks_chords_still_work_while_an_engine_composes() {
    let mut compositor = headless(Box::new(StackPolicy));
    let chords = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Box::new(Chords {
        log: Rc::clone(&chords),
    }));
    let log = hosted_field(&mut compositor, (120.0, 50.0, 2.0, 20.0));
    let mut engine = TestClient::connect_trusted(&mut compositor);
    pump(&mut compositor, &mut [&mut engine], |_, clients| {
        clients[0].seen.input_method_manager.is_some() && clients[0].seen.seat.is_some()
    });
    engine.own_input_method();
    pump(
        &mut compositor,
        &mut [&mut engine],
        |compositor, clients| compositor.text_entry().wants_typing && clients[0].seen.ime_activated,
    );
    engine.grab_keyboard();
    pump(&mut compositor, &mut [&mut engine], |_, _| true);

    // The desk's own chords answer wherever the keyboard is, and the engine holds it: a chord is
    // the session's before it is the engine's.
    press_keys(&mut compositor, &[KEY_LEFTMETA, KEY_X]);
    pump(&mut compositor, &mut [&mut engine], |_, _| {
        !chords.borrow().taken.is_empty()
    });
    assert_eq!(chords.borrow().taken, vec![(KEY_X, false)]);
    assert!(
        !engine.seen.grab_keys.iter().any(|(key, _)| *key == KEY_X),
        "the desk took the chord: {:?}",
        engine.seen.grab_keys
    );
    assert!(
        log.borrow().keys.is_empty(),
        "the field the engine composes in takes no key of its own"
    );

    // Typing that is not a chord is the engine's, as it is without one.
    press_keys(&mut compositor, &[KEY_A]);
    pump(&mut compositor, &mut [&mut engine], |_, clients| {
        clients[0]
            .seen
            .grab_keys
            .iter()
            .any(|(key, pressed)| *key == KEY_A && *pressed)
    });
    assert_eq!(
        engine.seen.grab_keys,
        vec![
            (KEY_LEFTMETA, true),
            (KEY_LEFTMETA, false),
            (KEY_A, true),
            (KEY_A, false),
        ]
    );
}
