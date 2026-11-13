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

/// One row of [`a_sequence_of_keys_types_what_the_layout_makes_of_it`]: the person's layout, the
/// keys they tap in order, and what each press types.
struct Typed {
    name: &'static str,
    layout: KeyboardLayout,
    taps: &'static [u32],
    types: &'static [Option<&'static str>],
}

#[test]
fn a_sequence_of_keys_types_what_the_layout_makes_of_it() {
    let german = || KeyboardLayout {
        layout: "de".into(),
        ..KeyboardLayout::default()
    };
    // Menu is the Compose key here.
    let compose = || KeyboardLayout {
        options: "compose:menu".into(),
        ..KeyboardLayout::default()
    };
    let rows = [
        Typed {
            // The accent key right of zero, then `e`: the accent types nothing on its own, and
            // the two keys type one letter.
            name: "a dead key and the key it accents type one character",
            layout: german(),
            taps: &[KEY_EQUAL, KEY_E],
            types: &[None, Some("é")],
        },
        Typed {
            // Compose, `o`, `c` is the copyright sign: the Compose key types nothing itself, and
            // a sequence still open types nothing.
            name: "the Compose key types a sequence of keys as one",
            layout: compose(),
            taps: &[KEY_COMPOSE, KEY_O, KEY_C],
            types: &[None, None, Some("©")],
        },
        Typed {
            // A key with no sequence in front of it types itself. `x` has no accented form: the
            // accent waits for a key it knows, is abandoned, and `x` types. The abandoned
            // sequence leaves nothing open.
            name: "a key outside a sequence types its own character",
            layout: german(),
            taps: &[KEY_E, KEY_EQUAL, KEY_X, KEY_E],
            types: &[Some("e"), None, Some("x"), Some("e")],
        },
        Typed {
            name: "a key after a finished Compose sequence types its own character",
            layout: compose(),
            taps: &[KEY_E, KEY_COMPOSE, KEY_O, KEY_C, KEY_E],
            types: &[Some("e"), None, None, Some("©"), Some("e")],
        },
        Typed {
            name: "no key types nothing",
            layout: german(),
            taps: &[],
            types: &[],
        },
    ];
    for row in rows {
        let mut compositor = headless(Box::new(StackPolicy));
        let log = typing_field(&mut compositor, &row.layout);
        for &keycode in row.taps {
            tap(&mut compositor, keycode);
        }
        let keys = log.borrow().keys.clone();
        assert_eq!(
            keys.len(),
            row.taps.len() * 2,
            "{}: the field heard every key press and release",
            row.name
        );
        // A tap is a press and then a release: the presses are what type.
        let typed: Vec<Option<&str>> = keys
            .iter()
            .step_by(2)
            .map(|key| key.text.as_deref())
            .collect();
        assert_eq!(typed, row.types, "{}: what each press typed", row.name);
    }
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
