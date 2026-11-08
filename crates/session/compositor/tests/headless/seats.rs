//! Seats beside the person's: two of them type into their own windows at the same time, and the
//! person's pointer, focus and keyboard are where they left them.

use super::*;
use tilo_compositor::Seat;

/// Windows side by side, three to a row, each 200 wide: so a seat's own point is over its own
/// window and no other.
struct SideBySide;

impl WindowPolicy for SideBySide {
    fn initial_size(
        &mut self,
        _window: &WindowInfo,
        _outputs: &[LayoutRect],
    ) -> Option<(i32, i32)> {
        None
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        _size: (i32, i32),
        outputs: &[LayoutRect],
    ) -> LayoutPoint {
        let Some(output) = outputs.first() else {
            return Point::from((0, 0));
        };
        // A window's id is never reused, so its place in the row is its own.
        let slot = i32::try_from(window.id % 3).unwrap_or(0);
        Point::from((output.loc.x + 20 + slot * 220, output.loc.y + 40))
    }
}

/// The client of `app` 's window: where it stands and how it is.
fn window_of(compositor: &Compositor, app: &str) -> ((i32, i32, i32, i32), u64) {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app) && info.parent.is_none())
        .map_or_else(
            || panic!("{app} has a window"),
            |(info, rect)| (rect, info.id),
        )
}

/// The person clicks into the window of `clients[index]` , which then has the keyboard.
fn person_clicks_into(
    compositor: &mut Compositor,
    clients: &mut [&mut TestClient],
    index: usize,
    app: &str,
) {
    let ((px, py, pw, ph), _) = window_of(compositor, app);
    compositor
        .input(InputEvent::PointerMotion {
            x: f64::from(px) + f64::from(pw) / 2.0,
            y: f64::from(py) + f64::from(ph) / 2.0,
        })
        .expect("motion");
    for pressed in [true, false] {
        compositor
            .input(InputEvent::PointerButton {
                button: super::BTN_LEFT,
                pressed,
            })
            .expect("the person's click");
    }
    pump(compositor, clients, |_, seen| {
        seen[index].seen.keyboard_focused
    });
}

/// The person presses and lets go of `keycode` once, their window hearing the keystroke's `heard`
/// keys in all.
fn person_types(
    compositor: &mut Compositor,
    clients: &mut [&mut TestClient],
    index: usize,
    keycode: u32,
    heard: usize,
) {
    for pressed in [true, false] {
        compositor
            .input(InputEvent::Key { keycode, pressed })
            .expect("the person's key");
    }
    pump(compositor, clients, |_, seen| {
        seen[index].seen.keys.len() == heard
    });
}

/// The person holds `keycode` down, their window hearing the keystroke's `heard` keys in all.
fn person_holds(
    compositor: &mut Compositor,
    clients: &mut [&mut TestClient],
    index: usize,
    keycode: u32,
    heard: usize,
) {
    compositor
        .input(InputEvent::Key {
            keycode,
            pressed: true,
        })
        .expect("the person's key");
    pump(compositor, clients, |_, seen| {
        seen[index].seen.keys.len() == heard
    });
}

/// `seat` presses and lets go of `keycode` in its own window.
fn seat_types(compositor: &mut Compositor, seat: Seat, keycode: u32) {
    for pressed in [true, false] {
        compositor
            .seat_input(seat, &InputEvent::Key { keycode, pressed })
            .expect("the seat's key");
    }
}

/// The keys `client` heard, each once.
fn keys(client: &TestClient) -> Vec<u32> {
    let mut keys: Vec<u32> = client.seen.keys.iter().map(|(key, _)| *key).collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[test]
fn two_seats_type_into_their_own_windows_and_leave_the_persons_focus_alone() {
    let mut compositor = headless(Box::new(SideBySide));
    let mut first = mapped_client(
        &mut compositor,
        "so.tilo.test.first",
        (200, 100),
        [255, 0, 0, 255],
    );
    let mut second = mapped_client(
        &mut compositor,
        "so.tilo.test.second",
        (200, 100),
        [0, 255, 0, 255],
    );
    let mut person = mapped_client(
        &mut compositor,
        "so.tilo.test.person",
        (200, 100),
        [0, 0, 255, 255],
    );

    // The person clicks into their own window and types: it has the keyboard.
    person_clicks_into(
        &mut compositor,
        &mut [&mut first, &mut second, &mut person],
        2,
        "so.tilo.test.person",
    );
    let person_key = 30;
    person_types(
        &mut compositor,
        &mut [&mut first, &mut second, &mut person],
        2,
        person_key,
        2,
    );
    let at = compositor.pointer_location();

    // Two seats, one window each.
    let a = compositor.open_seat();
    let b = compositor.open_seat();
    let aim = compositor
        .seat_aim_app(a, Some("so.tilo.test.first"))
        .expect("the first window");
    assert!(aim.window.is_some(), "the seat aims at its window");
    compositor
        .seat_aim_app(b, Some("so.tilo.test.second"))
        .expect("the second window");

    // Each types into its own window, one after the other and at the same time as far as the
    // desktop is concerned.
    assert!(
        compositor
            .seat_text(a, "kkk")
            .expect("the first seat types")
    );
    assert!(
        compositor
            .seat_text(b, "jjj")
            .expect("the second seat types")
    );
    pump(
        &mut compositor,
        &mut [&mut first, &mut second, &mut person],
        |_, clients| clients[0].seen.keys.len() == 6 && clients[1].seen.keys.len() == 6,
    );

    let typed_first = keys(&first);
    let typed_second = keys(&second);
    assert_eq!(typed_first.len(), 1, "one key types k: {typed_first:?}");
    assert_eq!(typed_second.len(), 1, "one key types j: {typed_second:?}");
    assert_ne!(
        typed_first, typed_second,
        "each window was typed into by its own key"
    );
    assert!(
        person.seen.keys.iter().all(|(key, _)| *key == person_key),
        "the person's window typed nothing of the agents': {:?}",
        person.seen.keys
    );

    // Their pointer never moved the person's, and the person's own keyboard still reaches the
    // person's window.
    assert_eq!(
        compositor.pointer_location(),
        at,
        "the person's pointer stayed where they left it"
    );
    assert!(
        person.seen.keyboard_focused,
        "the person's window has the keyboard between the seats' events"
    );
    person_types(
        &mut compositor,
        &mut [&mut first, &mut second, &mut person],
        2,
        person_key,
        4,
    );
    assert_eq!(
        first.seen.keys.len(),
        6,
        "the first seat's window kept out of the person's typing"
    );
    assert_eq!(second.seen.keys.len(), 6, "and so did the second's");

    compositor.close_seat(a);
    compositor.close_seat(b);
}

#[test]
fn a_seats_key_is_typed_once_in_its_window_and_never_the_persons() {
    let mut compositor = headless(Box::new(SideBySide));
    let mut first = mapped_client(
        &mut compositor,
        "so.tilo.test.first",
        (200, 100),
        [255, 0, 0, 255],
    );
    let mut person = mapped_client(
        &mut compositor,
        "so.tilo.test.person",
        (200, 100),
        [0, 0, 255, 255],
    );

    // The person clicks into their own window and holds a key down.
    person_clicks_into(
        &mut compositor,
        &mut [&mut first, &mut person],
        1,
        "so.tilo.test.person",
    );
    let held = 30;
    person_holds(&mut compositor, &mut [&mut first, &mut person], 1, held, 1);

    // A seat types its own key into its own window: one press and its release, not a second press
    // on the way out.
    let seat = compositor.open_seat();
    compositor
        .seat_aim_app(seat, Some("so.tilo.test.first"))
        .expect("the first window");
    let typed = 31;
    seat_types(&mut compositor, seat, typed);
    pump(
        &mut compositor,
        &mut [&mut first, &mut person],
        |_, clients| clients[0].seen.keys.len() >= 2,
    );
    assert_eq!(
        first.seen.keys,
        vec![(typed, true), (typed, false)],
        "the seat's keystroke reached its window once"
    );

    // A release for a key the seat never held is the person's: it stays held,
    // and goes nowhere near the seat's window.
    let heard_before = first.seen.keys.len();
    let person_before = person.seen.keys.len();
    compositor
        .seat_input(
            seat,
            &InputEvent::Key {
                keycode: held,
                pressed: false,
            },
        )
        .expect("the stray release");
    pump(
        &mut compositor,
        &mut [&mut first, &mut person],
        |_, clients| clients[1].seen.keys.len() > person_before,
    );
    assert_eq!(
        first.seen.keys.len(),
        heard_before,
        "the seat's window heard nothing of the person's key"
    );
    assert_eq!(
        person.seen.keys.last(),
        Some(&(held, true)),
        "the person's key is still held: {:?}",
        person.seen.keys
    );
    assert!(
        person.seen.keys.iter().all(|(key, _)| *key == held),
        "the seat's key never reached the person's window: {:?}",
        person.seen.keys
    );

    compositor.close_seat(seat);
}

#[test]
fn a_seats_click_lands_in_its_own_window_and_focuses_nothing() {
    let mut compositor = headless(Box::new(SideBySide));
    let mut first = mapped_client(
        &mut compositor,
        "so.tilo.test.first",
        (200, 100),
        [255, 0, 0, 255],
    );
    let mut second = mapped_client(
        &mut compositor,
        "so.tilo.test.second",
        (200, 100),
        [0, 255, 0, 255],
    );

    let seat = compositor.open_seat();
    compositor
        .seat_aim_app(seat, Some("so.tilo.test.first"))
        .expect("the first window");
    for pressed in [true, false] {
        compositor
            .seat_input(
                seat,
                &InputEvent::PointerButton {
                    button: super::BTN_LEFT,
                    pressed,
                },
            )
            .expect("the seat's click");
    }
    pump(
        &mut compositor,
        &mut [&mut first, &mut second],
        |_, clients| clients[0].seen.buttons.len() == 2,
    );
    assert_eq!(
        first.seen.buttons,
        vec![(super::BTN_LEFT, true), (super::BTN_LEFT, false)],
        "the seat's click reached the window it aims at, once"
    );
    assert!(
        !first.seen.pointer_positions.is_empty(),
        "the seat's pointer went to that window first"
    );
    assert_eq!(
        first.seen.pointer_entered_at,
        Some((0.0, 0.0)),
        "at the window's own top-left, where a seat that has not moved starts"
    );
    assert!(
        second.seen.buttons.is_empty()
            && second.seen.keys.is_empty()
            && second.seen.pointer_positions.is_empty(),
        "the other window heard nothing"
    );
    assert!(
        second.seen.keyboard_focused,
        "and the person's focus was put back, not left on the window the seat aimed at"
    );
    assert!(
        !first.seen.keyboard_focused,
        "which did not keep the keyboard between the seat's events"
    );
    compositor.close_seat(seat);
}
