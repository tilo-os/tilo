//! The keyboard assists a person turns on: what a window hears of the keys they press while one is
//! on.

use super::*;

/// One row of [`an_assist_turns_the_keys_pressed_into_the_keys_a_window_hears`]: the assist the
/// person turned on (the window is named for it), the keys they press and the keys the focused
/// window hears.
struct Assisted {
    name: &'static str,
    turn_on: fn(&mut tilo_compositor::A11y),
    pressed: &'static [(u32, bool)],
    heard: &'static [(u32, bool)],
}

#[test]
fn an_assist_turns_the_keys_pressed_into_the_keys_a_window_hears() {
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_F6: u32 = 64;
    const KEY_F7: u32 = 65;
    const KEY_TAB: u32 = 15;
    const KEY_RETURN: u32 = 28;
    // Shift, pressed and let go, then a letter.
    const SHIFT_THEN_A: &[(u32, bool)] = &[
        (KEY_LEFTSHIFT, true),
        (KEY_LEFTSHIFT, false),
        (KEY_A, true),
        (KEY_A, false),
    ];
    let rows = [
        Assisted {
            name: "none",
            turn_on: |_| {},
            pressed: SHIFT_THEN_A,
            heard: SHIFT_THEN_A,
        },
        Assisted {
            // The shift stays down for the letter and comes up with it.
            name: "sticky",
            turn_on: |a11y| a11y.assists.sticky_keys = true,
            pressed: SHIFT_THEN_A,
            heard: &[
                (KEY_LEFTSHIFT, true),
                (KEY_A, true),
                (KEY_A, false),
                (KEY_LEFTSHIFT, false),
            ],
        },
        Assisted {
            // The switches become the keys that move on and activate.
            name: "switch",
            turn_on: |a11y| {
                a11y.assists.switch = Some(tilo_compositor::SwitchConfig {
                    pair: tilo_compositor::SwitchPair::F6F7,
                    period: None,
                });
            },
            pressed: &[
                (KEY_F6, true),
                (KEY_F6, false),
                (KEY_F7, true),
                (KEY_F7, false),
            ],
            heard: &[
                (KEY_TAB, true),
                (KEY_TAB, false),
                (KEY_RETURN, true),
                (KEY_RETURN, false),
            ],
        },
    ];
    for row in rows {
        let mut compositor = headless(Box::new(StackPolicy));
        let mut client = mapped_client(&mut compositor, row.name, (200, 100), [0, 255, 0, 255]);
        with_a11y(&mut compositor, row.turn_on);
        for &(keycode, pressed) in row.pressed {
            compositor
                .input(InputEvent::Key { keycode, pressed })
                .expect("input");
        }
        pump(&mut compositor, &mut [&mut client], |_, clients| {
            clients[0].seen.keys.len() == row.heard.len()
        });
        assert_eq!(
            client.seen.keys, row.heard,
            "{} keys: what the window heard",
            row.name
        );
    }
}
