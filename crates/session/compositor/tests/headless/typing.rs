//! Typing with the person's keyboard: the layout they chose, and the dead keys and Compose key
//! sequences that type characters their keys do not carry themselves.

use super::*;
use smithay::input::keyboard::keysyms;

const KEY_EQUAL: u32 = 13;
const KEY_E: u32 = 18;
const KEY_O: u32 = 24;
const KEY_C: u32 = 46;
const KEY_COMPOSE: u32 = 127;
const KEY_LEFTCTRL: u32 = 29;

#[test]
fn the_keyboard_types_in_the_persons_layout() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "typing", (200, 100), [0, 255, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keymap.is_some()
    });
    let before = client.seen.keymap.clone().expect("the client has a keymap");

    compositor.set_keyboard_layout(&KeyboardLayout {
        layout: "de".into(),
        ..KeyboardLayout::default()
    });
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0]
            .seen
            .keymap
            .as_ref()
            .is_some_and(|keymap| keymap.contains("German"))
    });
    let after = client.seen.keymap.clone().expect("the new keymap");
    assert_ne!(before, after, "the client's keys type other characters now");
    assert!(
        after.contains("German"),
        "the client's keymap is the German one the person chose"
    );
    assert_eq!(compositor.keyboard_layout().as_deref(), Some("German"));
}

/// A hosted surface with the keyboard on it, in `layout` , recording the keys it types.
fn typing_field(compositor: &mut Compositor, layout: &KeyboardLayout) -> Rc<RefCell<HostLog>> {
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Rc::new(RefCell::new(Square::new(
        (0, 0, 10, 10),
        [0, 255, 0, 255],
        Rc::clone(&log),
    ))));
    log.borrow_mut().wants_keyboard = true;
    compositor.set_keyboard_layout(layout);
    log
}

/// Press and let go of the key `keycode` .
fn tap(compositor: &mut Compositor, keycode: u32) {
    for pressed in [true, false] {
        compositor
            .input(InputEvent::Key { keycode, pressed })
            .unwrap();
    }
}

#[test]
fn a_dead_key_and_the_key_it_accents_type_one_character() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = typing_field(
        &mut compositor,
        &KeyboardLayout {
            layout: "de".into(),
            ..KeyboardLayout::default()
        },
    );

    // The accent key right of zero, then `e` .
    tap(&mut compositor, KEY_EQUAL);
    tap(&mut compositor, KEY_E);
    let keys = log.borrow().keys.clone();
    assert_eq!(keys.len(), 4, "the field heard both keys press and release");
    assert_eq!(keys[0].text, None, "the accent types nothing on its own");
    assert_eq!(
        keys[2].text.as_deref(),
        Some("é"),
        "the two keys type one letter"
    );
}

#[test]
fn the_compose_key_types_a_sequence_of_keys_as_one() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = typing_field(
        &mut compositor,
        &KeyboardLayout {
            options: "compose:menu".into(),
            ..KeyboardLayout::default()
        },
    );

    // Menu is the Compose key here: Compose, `o` , `c` is the copyright sign.
    tap(&mut compositor, KEY_COMPOSE);
    tap(&mut compositor, KEY_O);
    tap(&mut compositor, KEY_C);
    let keys = log.borrow().keys.clone();
    assert_eq!(keys.len(), 6);
    assert_eq!(keys[0].text, None, "the Compose key types nothing itself");
    assert_eq!(keys[2].text, None, "a sequence still open types nothing");
    assert_eq!(keys[4].text.as_deref(), Some("©"));
}

#[test]
fn a_key_outside_a_sequence_types_its_own_character() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = typing_field(
        &mut compositor,
        &KeyboardLayout {
            layout: "de".into(),
            ..KeyboardLayout::default()
        },
    );

    tap(&mut compositor, KEY_E);
    assert_eq!(
        log.borrow().keys[0].text.as_deref(),
        Some("e"),
        "a key with no sequence in front of it types itself"
    );

    // `x` has no accented form: the accent is abandoned and `x` types.
    tap(&mut compositor, KEY_EQUAL);
    tap(&mut compositor, KEY_X);
    let keys = log.borrow().keys.clone();
    assert_eq!(keys[2].text, None, "the accent waits for a key it knows");
    assert_eq!(keys[4].text.as_deref(), Some("x"));

    tap(&mut compositor, KEY_E);
    assert_eq!(
        log.borrow().keys[6].text.as_deref(),
        Some("e"),
        "the abandoned sequence leaves nothing open"
    );
}

#[test]
fn a_chord_on_a_cyrillic_layout_names_the_latin_key() {
    // Russian types, with US behind it in the same keymap: what a shortcut is named by.
    let mut compositor = headless(Box::new(StackPolicy));
    let log = typing_field(
        &mut compositor,
        &KeyboardLayout {
            layout: "ru,us".into(),
            ..KeyboardLayout::default()
        },
    );

    // The key a US keyboard labels C types the Cyrillic letter on its own.
    tap(&mut compositor, KEY_C);
    let keys = log.borrow().keys.clone();
    assert_eq!(
        keys[0].text.as_deref(),
        Some("с"),
        "the person types Russian"
    );
    assert_eq!(
        keys[0].keysym,
        keysyms::KEY_Cyrillic_es,
        "the Russian letter's own keysym"
    );

    // Under Ctrl it is still Ctrl+C: the letter the US arrangement puts on that key names the
    // chord.
    compositor
        .input(InputEvent::Key {
            keycode: KEY_LEFTCTRL,
            pressed: true,
        })
        .unwrap();
    tap(&mut compositor, KEY_C);
    compositor
        .input(InputEvent::Key {
            keycode: KEY_LEFTCTRL,
            pressed: false,
        })
        .unwrap();
    let keys = log.borrow().keys.clone();
    let copy = keys
        .iter()
        .find(|key| key.keycode == KEY_C && key.pressed && key.modifiers.ctrl)
        .expect("the field heard Ctrl with C");
    assert_eq!(
        copy.keysym,
        keysyms::KEY_Cyrillic_es,
        "the key still types the Cyrillic letter"
    );
    assert_eq!(copy.latin_keysym, u32::from('c'), "XK_c, the chord's name");
}
