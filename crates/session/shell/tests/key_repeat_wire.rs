//! A key tapped once is typed once, however long the compositor is busy.
//!
//! A Wayland client that repeats keys itself starts its repeat timer when a
//! press reaches it and stops it only when the release does. The compositor
//! dispatches input once per turn of its loop, and the phases after that
//! dispatch can outlast the repeat delay, so the loop reads input between
//! its phases and sends the release of a key a client holds at once
//! (`client_input::deliver_held_key_release`); the release itself stays
//! queued for the next turn's full dispatch. The client here repeats exactly
//! as the seat's `wl_keyboard.repeat_info` tells it to, on a clock the test
//! moves while the compositor is busy, and records every stroke it types.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyCode, PhysicalKey};

use crate::support::wayland::foreign_id;

const KEY_A: u32 = 30;
const KEY_B: u32 = 48;

/// A client that repeats held keys the way `repeat_info` asks, on its own
/// clock in milliseconds.
#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    /// `(rate in Hz, delay in ms)` as the seat last said.
    repeat: Option<(i32, i32)>,
    /// The key held down, and the client's time when its press arrived.
    held: Option<(u32, u64)>,
    now_ms: u64,
    /// Every key the client typed, its own repeats included.
    strokes: Vec<u32>,
    /// Every release the client received.
    releases: Vec<u32>,
}

impl Client {
    /// Let `ms` pass on the client's clock, typing whatever its own repeat
    /// timer fires meanwhile.
    fn wait(&mut self, ms: u64) {
        let until = self.now_ms.saturating_add(ms);
        if let (Some((key, since)), Some((rate, delay))) = (self.held, self.repeat)
            && rate > 0
        {
            let period = 1000_u64.checked_div(u64::try_from(rate).unwrap()).unwrap();
            let mut next = since.saturating_add(u64::try_from(delay).unwrap());
            while next <= until {
                if next > self.now_ms {
                    self.strokes.push(key);
                }
                next = next.saturating_add(period.max(1));
            }
        }
        self.now_ms = until;
    }
}

crate::support::wayland::test_client!(Client);

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::RepeatInfo { rate, delay } => state.repeat = Some((rate, delay)),
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(key_state),
                ..
            } => match key_state {
                wl_keyboard::KeyState::Pressed => {
                    state.held = Some((key, state.now_ms));
                    state.strokes.push(key);
                }
                wl_keyboard::KeyState::Released => {
                    state.releases.push(key);
                    if state.held.is_some_and(|(held, _)| held == key) {
                        state.held = None;
                    }
                }
                _ => {}
            },
            wl_keyboard::Event::Leave { .. } => state.held = None,
            _ => {}
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

type Harness = crate::support::wayland::Harness<Client>;

/// A key of the machine's own keyboard as the backend hands it over.
fn local_key(code: KeyCode, text: &str, pressed: bool) -> WindowEvent {
    let mut event = ui::synthetic_input::key_event(
        Key::Character(text.into()),
        Some(text.into()),
        if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        },
    );
    event.physical_key = PhysicalKey::Code(code);
    ui::synthetic_input::keyboard_input(event)
}

/// A focused client window, ready for keys.
fn focused_client() -> (Harness, shell::compositor::frame::FrameBuilder) {
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let qh = h.queue.handle();
    let _keyboard = seat.get_keyboard(&qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Editor".into());
    h.pump();
    let window = foreign_id(&h.state, "org.example.Editor");
    h.state.set_client_keyboard_focus(Some(window));
    h.pump();
    h.client.strokes.clear();
    h.client.releases.clear();
    (h, shell::compositor::frame::FrameBuilder::new((1600, 900)))
}

fn route(h: &mut Harness, frame: &mut shell::compositor::frame::FrameBuilder, event: &WindowEvent) {
    assert!(
        shell::compositor::client_input::route_input(
            &mut h.state,
            frame,
            event,
            &Modifiers::default()
        ),
        "the focused client takes the key"
    );
}

/// What the run loop does between two phases of a turn when `event` has
/// just been read (`deliver_held_key_releases!` in the run loop).
fn between_phases(h: &mut Harness, event: &WindowEvent) -> bool {
    let sent = shell::compositor::client_input::deliver_held_key_release(&mut h.state, event);
    h.pump();
    sent
}

#[test]
fn a_tap_released_while_a_slow_phase_runs_is_typed_once() {
    let (mut h, mut frame) = focused_client();
    let (rate, delay) = h.client.repeat.expect("the seat's repeat info");
    assert!(rate > 0, "clients repeat held keys themselves");
    let delay = u64::try_from(delay).unwrap();

    // This turn's dispatch delivers the press.
    let press = local_key(KeyCode::KeyA, "a", true);
    route(&mut h, &mut frame, &press);
    h.pump();
    assert_eq!(h.client.strokes, [KEY_A]);

    // A slow phase follows. The person lets go 80 ms in; the loop reads
    // the release between two of its phases.
    h.client.wait(80);
    let release = local_key(KeyCode::KeyA, "a", false);
    assert!(
        between_phases(&mut h, &release),
        "the release of a key the client holds goes out at once"
    );
    // The turn runs on far past the repeat delay before the next dispatch,
    // which takes the queued release through the full routing.
    h.client.wait(delay.saturating_add(1000));
    route(&mut h, &mut frame, &release);
    h.pump();

    assert_eq!(
        h.client.strokes,
        [KEY_A],
        "a key tapped once is typed once, however long the turn ran on"
    );
    assert_eq!(h.client.releases, [KEY_A], "and released exactly once");
}

#[test]
fn only_releases_of_keys_the_client_holds_go_ahead_of_the_queue() {
    let (mut h, mut frame) = focused_client();
    let delay = u64::try_from(h.client.repeat.expect("repeat info").1).unwrap();
    let a_down = local_key(KeyCode::KeyA, "a", true);
    route(&mut h, &mut frame, &a_down);
    h.pump();

    // Rolling over to "b" while a slow phase runs: b's press, a's release
    // and b's release all arrive before the next dispatch.
    let queued = [
        local_key(KeyCode::KeyB, "b", true),
        local_key(KeyCode::KeyA, "a", false),
        local_key(KeyCode::KeyB, "b", false),
    ];
    let sent: Vec<bool> = queued
        .iter()
        .map(|event| between_phases(&mut h, event))
        .collect();
    assert_eq!(
        sent,
        [false, true, false],
        "a press, and the release of a key whose press the client has not seen yet, wait"
    );
    assert_eq!(h.client.releases, [KEY_A]);
    h.client.wait(delay.saturating_add(1000));

    // The next turn dispatches the queue in order, and b's press and
    // release reach the client together.
    for event in &queued {
        route(&mut h, &mut frame, event);
    }
    h.pump();
    h.client.wait(delay.saturating_add(1000));
    assert_eq!(h.client.strokes, [KEY_A, KEY_B]);
    assert_eq!(h.client.releases, [KEY_A, KEY_B]);

    // A key typed afterwards is delivered and released as ever.
    route(&mut h, &mut frame, &a_down);
    route(&mut h, &mut frame, &local_key(KeyCode::KeyA, "a", false));
    h.pump();
    assert_eq!(h.client.strokes, [KEY_A, KEY_B, KEY_A]);
    assert_eq!(h.client.releases, [KEY_A, KEY_B, KEY_A]);
}
