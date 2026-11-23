//! Keys a person types for a window that has not shown yet wait for it: Ctrl+N pressed as an app
//! opens reaches the app once its window shows, with Ctrl held as it was, and is not lost to a
//! desktop where nothing had the keyboard.

use super::*;

use std::cell::Cell;

const KEY_LEFTCTRL: u32 = 29;
const KEY_N: u32 = 49;
/// Control's bit in the modifiers the default keymap sends.
const CONTROL_MASK: u32 = 1 << 2;

/// A stack whose next window is on its way to the person until it is placed, as the desktop's
/// strips hold the place of a launched app: the keyboard waits for it while `waits` holds.
struct Launching {
    waits: Rc<Cell<bool>>,
}

impl WindowPolicy for Launching {
    fn initial_size(
        &mut self,
        window: &WindowInfo,
        outputs: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        StackPolicy.initial_size(window, outputs)
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        size: (i32, i32),
        outputs: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        self.waits.set(false);
        StackPolicy.place(window, size, outputs)
    }

    fn keyboard_awaits_window(&mut self) -> bool {
        self.waits.get()
    }
}

fn key(compositor: &mut Compositor, keycode: u32, pressed: bool) {
    compositor
        .input(InputEvent::Key { keycode, pressed })
        .unwrap();
}

fn ctrl_n(compositor: &mut Compositor) {
    key(compositor, KEY_LEFTCTRL, true);
    key(compositor, KEY_N, true);
    key(compositor, KEY_N, false);
    key(compositor, KEY_LEFTCTRL, false);
}

#[test]
fn a_chord_typed_before_the_window_shows_reaches_it_once_it_does() {
    let waits = Rc::new(Cell::new(true));
    let mut compositor = headless(Box::new(Launching {
        waits: Rc::clone(&waits),
    }));
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.seat.is_some()
    });
    client.open_window("notes");
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });

    // The window has drawn nothing yet: the person's Ctrl+N waits for it.
    ctrl_n(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert!(
        client.seen.keys.is_empty(),
        "no key reaches a window not shown"
    );

    client.show(200, 100, [0, 255, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() == 4
    });
    assert!(client.seen.keyboard_focused, "the window took the keyboard");
    assert_eq!(
        client.seen.keys,
        [
            (KEY_LEFTCTRL, true),
            (KEY_N, true),
            (KEY_N, false),
            (KEY_LEFTCTRL, false)
        ],
        "the keys reach the window in the order they were typed"
    );
    let n = client
        .seen
        .presses
        .iter()
        .find(|(key, _)| *key == KEY_N)
        .expect("N was pressed");
    assert_ne!(n.1 & CONTROL_MASK, 0, "N went down with Ctrl held: Ctrl+N");

    // Once the window shows, keys go straight to it.
    key(&mut compositor, KEY_N, true);
    key(&mut compositor, KEY_N, false);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() == 6
    });
    assert_eq!(client.seen.presses.last(), Some(&(KEY_N, 0)));
}

#[test]
fn keys_that_waited_for_a_launch_that_gave_up_go_where_keys_go() {
    let waits = Rc::new(Cell::new(false));
    let mut compositor = headless(Box::new(Launching {
        waits: Rc::clone(&waits),
    }));
    let mut client = mapped_client(&mut compositor, "files", (200, 100), [0, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });

    // A launch in front of the person: their key waits for its window.
    waits.set(true);
    key(&mut compositor, KEY_N, true);
    key(&mut compositor, KEY_N, false);
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert!(
        client.seen.keys.is_empty(),
        "the key waits for the window on its way"
    );

    // The launch gave up: the key goes on to what has the keyboard.
    waits.set(false);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() == 2
    });
    assert_eq!(client.seen.keys, [(KEY_N, true), (KEY_N, false)]);
}
