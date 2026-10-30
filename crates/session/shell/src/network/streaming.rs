//! The compositor's side of remote input: a peer's [`InputEvent`]s and
//! remote-desktop actions reach the seat of a live compositor state, and
//! the input firewall bounds them. The share registry and the wire
//! vocabulary are `tilo_window_share::streaming`'s.

#[cfg(all(test, target_os = "linux"))]
#[allow(
    clippy::float_cmp,
    reason = "exact streaming transform fixtures are the behavior under test"
)]
mod tests {
    use neighbour_wire::{PeerId, SurfaceId};

    /// Put `surface` on screen, focused, unobstructed, so the firewall's checks have something to answer from.
    fn stage_shared_surface(state: &mut crate::compositor::state::State, surface: SurfaceId) {
        use crate::network::input_firewall::ShellView;
        use vello::kurbo::Rect;
        state.remote_input_firewall.publish_view(ShellView::new(
            vec![(surface, Rect::new(0.0, 0.0, 1920.0, 1080.0))],
            Some(surface),
            crate::shell::input_policy::InputCapture::NONE,
            false,
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn remote_seat_input_reaches_the_focused_client() {
        // Phase `apply_remote_input_pointer_motion_reaches_seat`.
        // `RemoteInputSink::apply` for `State` calls `PointerHandle::motion` for `PointerMotion`; asserts the pointer's location reflects the injected coordinates.
        {
            use crate::compositor::state::State;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;
            use tilo_window_share::streaming::{InputEvent, StreamingShare};

            // Minimal standalone `State` — no listening socket, no clients; same plumbing `native::run` does at startup, minus the winit/drm backend.
            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let display_handle = display.handle();
            let mut state = State::new(event_loop.handle(), display_handle);

            // Stage the (peer, surface) share, else `apply_remote_input` drops on the unknown-share path.
            let peer: PeerId = 42;
            let surface: SurfaceId = 7;
            state
                .share_registry
                .insert(peer, surface, StreamingShare::new());
            // The shared surface fills the output from its origin, so a surface-local coordinate matches its output-space translation.
            stage_shared_surface(&mut state, surface);

            // Fresh seat starts at the origin; current_location returns the last-known location.
            let initial = state
                .seat
                .get_pointer()
                .expect("seat has pointer device")
                .current_location();
            assert_eq!(
                initial.x, 0.0,
                "seat fresh start; pointer x should be 0, got {}",
                initial.x
            );
            assert_eq!(initial.y, 0.0);

            let routed = state.apply_remote_input(
                peer,
                surface,
                InputEvent::PointerMotion { x: 123.0, y: 456.0 },
            );
            assert!(routed, "share is read/write, motion must route");

            // Proves the call reached the production `motion()` path, not just stamped `last_remote_input`.
            let after = state
                .seat
                .get_pointer()
                .expect("seat still has pointer")
                .current_location();
            assert_eq!(
                (after.x, after.y),
                (123.0, 456.0),
                "D-1 regression: pointer location did not reach the seat \
                 after `apply_remote_input(PointerMotion)`; observed {:?}",
                (after.x, after.y)
            );

            // Seat is mutating, not pinned at the first value.
            let _ = state.apply_remote_input(
                peer,
                surface,
                InputEvent::PointerMotion { x: 5.0, y: 6.0 },
            );
            let after2 = state.seat.get_pointer().unwrap().current_location();
            assert_eq!((after2.x, after2.y), (5.0, 6.0));
        }
        // Phase `apply_remote_input_key_reaches_keyboard`.
        // `InputEvent::Key` must reach `KeyboardHandle::input` — proven via `forwarded_pressed_keys` after a Key event.
        {
            use crate::compositor::state::State;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;
            use tilo_window_share::streaming::{InputEvent, StreamingShare};

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let display_handle = display.handle();
            let mut state = State::new(event_loop.handle(), display_handle);

            let peer: PeerId = 7;
            let surface: SurfaceId = 42;
            state
                .share_registry
                .insert(peer, surface, StreamingShare::new());
            // The scrubber's focus guard mutes a guest's keystrokes unless the shared surface holds keyboard focus.
            stage_shared_surface(&mut state, surface);
            assert!(state.apply_remote_input(
                peer,
                surface,
                InputEvent::KeyboardFocus {
                    surface,
                    focused: true,
                },
            ));

            // Baseline: no keys pressed.
            {
                let kb = state.seat.get_keyboard().expect("keyboard on seat");
                assert!(
                    kb.pressed_keys().is_empty(),
                    "seat starts with no pressed keys",
                );
            }

            let keysym: u32 = 30; /* evdev A on a US keymap */
            let xkb_keycode = keysym + 8;
            let routed = state.apply_remote_input(
                peer,
                surface,
                InputEvent::Key {
                    keysym,
                    pressed: true,
                    modifiers: 0,
                },
            );
            assert!(routed, "read/write share, key event must route");

            // smithay's `forwarded_pressed_keys` reflects the state after `KeyboardHandle::input` runs.
            {
                let kb = state.seat.get_keyboard().expect("keyboard on seat");
                let pressed = kb.pressed_keys();
                assert!(
                    pressed.iter().any(|k| u32::from(*k) == xkb_keycode),
                    "keyboard did not observe key press; observed {pressed:?}",
                );
            }

            // After `Released`, the pressed set no longer contains the keycode.
            let _ = state.apply_remote_input(
                peer,
                surface,
                InputEvent::Key {
                    keysym,
                    pressed: false,
                    modifiers: 0,
                },
            );
            {
                let kb = state.seat.get_keyboard().expect("keyboard on seat");
                assert!(
                    !kb.pressed_keys()
                        .iter()
                        .any(|k| u32::from(*k) == xkb_keycode),
                    "keyboard still holds the released keycode",
                );
            }

            // `last_remote_input` carries the peer id so a future per-user "typing" overlay can find it in one place.
            let (last_peer, _, _) = state.last_remote_input.as_ref().expect("event stamped");
            assert_eq!(*last_peer, peer, "origin peer tag lost");
        }
        // Phase `pointer_leave_preserves_focus_until_explicit_blur`.
        // Hover and click focus are independent: pointer leave preserves the
        // caret, while an explicit blur releases keyboard focus.
        {
            use crate::compositor::state::State;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;
            use tilo_window_share::streaming::{InputEvent, StreamingShare};

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let display_handle = display.handle();
            let mut state = State::new(event_loop.handle(), display_handle);

            let peer: PeerId = 11;
            let surface: SurfaceId = 314;
            state
                .share_registry
                .insert(peer, surface, StreamingShare::new());
            stage_shared_surface(&mut state, surface);

            // No seat entry yet — the peer hasn't emitted any input.
            assert!(state.remote_seats.seat(peer).is_none());

            // PointerEnter remains a compatibility focus handshake for older
            // viewers; importantly, leaving no longer clears that focus.
            let routed =
                state.apply_remote_input(peer, surface, InputEvent::PointerEnter { surface });
            assert!(routed);
            let seat = state
                .remote_seats
                .seat(peer)
                .expect("peer's remote seat exists after enter");
            assert_eq!(seat.pointer_surface, Some(surface));
            assert_eq!(seat.focused_surface, Some(surface));

            assert!(state.apply_remote_input(
                peer,
                surface,
                InputEvent::KeyboardFocus {
                    surface,
                    focused: true,
                },
            ));

            // Leaving the tile preserves the app caret/focus.
            let _ = state.apply_remote_input(peer, surface, InputEvent::PointerLeave { surface });
            let seat = state
                .remote_seats
                .seat(peer)
                .expect("peer's remote seat still exists");
            assert_eq!(seat.pointer_surface, None);
            assert_eq!(seat.focused_surface, Some(surface));

            assert!(state.apply_remote_input(
                peer,
                surface,
                InputEvent::KeyboardFocus {
                    surface,
                    focused: false,
                },
            ));
            assert_eq!(state.remote_seats.seat(peer).unwrap().focused_surface, None);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn remote_desktop_input_reaches_the_seat_until_the_kill_switch() {
        // Phase `apply_remote_desktop_absolute_motion_reaches_seat`.
        // A `RemoteDesktop` `NotifyPointerMotionAbsolute` must reach the real smithay seat, mirroring the peer-sync motion test above.
        {
            use crate::compositor::state::State;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;
            use tilo_window_share::streaming::RemoteAction;

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());

            let applied = state.apply_remote_desktop_action(RemoteAction::PointerMotionAbsolute {
                stream: 1,
                x: 200.0,
                y: 150.0,
            });
            assert!(applied, "absolute motion must reach the seat pointer");
            let loc = state.seat.get_pointer().unwrap().current_location();
            assert_eq!((loc.x, loc.y), (200.0, 150.0));

            // Relative motion accumulates on top of the current location.
            let _ = state.apply_remote_desktop_action(RemoteAction::PointerMotionRelative {
                dx: -50.0,
                dy: 25.0,
            });
            let loc2 = state.seat.get_pointer().unwrap().current_location();
            assert_eq!((loc2.x, loc2.y), (150.0, 175.0));
        }
        // Phase `personal_fabric_motion_goes_to_the_event_loop_but_yields_to_kill_switch`.
        {
            use crate::compositor::state::State;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;
            use tilo_device_fabric::FabricInputEvent;

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());
            let controller = tilo_network_actor::fabric::DeviceId::new("controller").unwrap();
            assert!(state.apply_personal_fabric_input(
                &controller,
                FabricInputEvent::PointerMotion { x: 320, y: 180 }
            ));
            assert_eq!(
                state.take_personal_fabric_input(),
                vec![FabricInputEvent::PointerMotion { x: 320, y: 180 }],
                "admitted input is delivered as this device's own"
            );

            state.remote_input_firewall.engage_kill_switch();
            assert!(!state.apply_personal_fabric_input(
                &controller,
                FabricInputEvent::PointerMotion { x: 640, y: 360 }
            ));
            assert!(state.take_personal_fabric_input().is_empty());
        }
        // Phase `apply_remote_desktop_keycode_reaches_keyboard`.
        // A `RemoteDesktop` `NotifyKeyboardKeycode` reaches the seat keyboard, appearing in `pressed_keys()` after press and clearing on release.
        {
            use crate::compositor::state::State;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;
            use tilo_window_share::streaming::RemoteAction;

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());

            let kc: i32 = 30; // Linux evdev A maps to XKB keycode 38.
            assert!(
                state.apply_remote_desktop_action(RemoteAction::KeyboardKeycode {
                    keycode: kc,
                    pressed: true,
                })
            );
            {
                let kb = state.seat.get_keyboard().expect("keyboard on seat");
                assert!(
                    kb.pressed_keys().iter().any(|k| u32::from(*k) == 38),
                    "evdev A must reach the seat as XKB A",
                );
            }
            let _ = state.apply_remote_desktop_action(RemoteAction::KeyboardKeycode {
                keycode: kc,
                pressed: false,
            });
            let kb = state.seat.get_keyboard().expect("keyboard on seat");
            assert!(
                !kb.pressed_keys().iter().any(|k| u32::from(*k) == 38),
                "release must clear the pressed key",
            );
        }
        // Phase `remote_desktop_channel_injector_drives_the_seat`.
        // End-to-end production path: a [`ChannelInjector`] carries a decoded action into `apply_remote_desktop_action` against the real seat.
        {
            use crate::compositor::state::State;
            use crate::portal::{ChannelInjector, RemoteInputInjector};
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;
            use tilo_window_share::streaming::RemoteAction;

            let (tx, rx) = std::sync::mpsc::channel();
            let injector = ChannelInjector::new(tx);
            // Portal `Notify*` handler side: forward the decoded action.
            assert!(injector.inject(RemoteAction::PointerMotionAbsolute {
                stream: 1,
                x: 77.0,
                y: 88.0,
            }));

            // Compositor-loop side: drain + apply to the live seat.
            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());
            while let Ok(action) = rx.try_recv() {
                state.apply_remote_desktop_action(action);
            }
            let loc = state.seat.get_pointer().unwrap().current_location();
            assert_eq!(
                (loc.x, loc.y),
                (77.0, 88.0),
                "channel injector → loop drain → seat must move the pointer"
            );
        }
    }
}
