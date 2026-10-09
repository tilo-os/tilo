//! Wire-level keyboard focus regression tests.
//!
//! Smithay deliberately transfers every forwarded held key in the `enter`
//! accompanying a focus replacement.  The following release must therefore
//! go to the *new* focus, exactly once; sending it back to the old surface
//! would contradict the Wayland ordering and create a stuck key.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey};

use crate::support::wayland::foreign_id;

#[derive(Debug, PartialEq, Eq)]
enum KeyboardWire {
    Enter { surface: u32, keys: Vec<u32> },
    Leave { surface: u32 },
    Key { key: u32, pressed: bool },
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    keyboard: Vec<KeyboardWire>,
}

crate::support::wayland::test_client!(Client);

fn surface_id(surface: &wl_surface::WlSurface) -> u32 {
    surface.id().protocol_id()
}

/// Construct the same minimal `WindowEvent` that the DRM/test-control input
/// adapters feed to the capture path. `KeyEvent` keeps platform data private,
/// hence zero-init plus the fields consumed by the compositor.
fn captured_key_event(logical: Key, pressed: bool) -> WindowEvent {
    let mut event: KeyEvent = unsafe { std::mem::zeroed() };
    event.logical_key = logical;
    event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    event.location = KeyLocation::Standard;
    event.state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    event.repeat = false;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event,
        is_synthetic: false,
    }
}

struct EmptyControlState;

impl shell::shell::test_control::ControlState for EmptyControlState {
    fn widget_registry(&self) -> std::collections::HashMap<String, vello::kurbo::Rect> {
        std::collections::HashMap::new()
    }
}

fn route_test_control_actions(
    h: &mut Harness,
    frame: &mut shell::compositor::frame::FrameBuilder,
    actions: Vec<shell::shell::test_control::PlannedAction>,
    modifiers: Modifiers,
) {
    use shell::shell::test_control::{KeyTag, NamedKeyTag, PlannedAction};

    for action in actions {
        let event = match action {
            PlannedAction::CursorMoved { x, y } => WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(x, y),
            },
            PlannedAction::MouseLeftButton { pressed } => WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: if pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                },
                button: winit::event::MouseButton::Left,
            },
            PlannedAction::KeyboardKey {
                key: KeyTag::Named(NamedKeyTag::ArrowLeft),
                pressed,
            } => shell::shell::test_control::keyboard_input_event(
                &Key::Named(NamedKey::ArrowLeft),
                pressed,
            ),
            unexpected => panic!("test control planned an unexpected action: {unexpected:?}"),
        };
        assert!(shell::compositor::client_input::route_input(
            &mut h.state,
            frame,
            &event,
            &modifiers,
        ));
    }
}

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
            wl_keyboard::Event::Enter { surface, keys, .. } => {
                let keys = keys
                    .chunks_exact(4)
                    .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("u32 key")))
                    .collect();
                state.keyboard.push(KeyboardWire::Enter {
                    surface: surface_id(&surface),
                    keys,
                });
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                state.keyboard.push(KeyboardWire::Leave {
                    surface: surface_id(&surface),
                });
            }
            wl_keyboard::Event::Key {
                key,
                state: key_state,
                ..
            } => {
                state.keyboard.push(KeyboardWire::Key {
                    key,
                    pressed: matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed)),
                });
            }
            _ => {}
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

type Harness = crate::support::wayland::Harness<Client>;

#[test]
fn focus_change_transfers_a_held_key_then_releases_it_once_at_the_new_focus() {
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let first = compositor.create_surface(&qh, ());
    let first_xdg = wm.get_xdg_surface(&first, &qh, ());
    let first_top = first_xdg.get_toplevel(&qh, ());
    first_top.set_app_id("org.example.First".into());
    let second = compositor.create_surface(&qh, ());
    let second_xdg = wm.get_xdg_surface(&second, &qh, ());
    let second_top = second_xdg.get_toplevel(&qh, ());
    second_top.set_app_id("org.example.Second".into());
    h.pump();

    let first_id = foreign_id(&h.state, "org.example.First");
    let second_id = foreign_id(&h.state, "org.example.Second");
    h.state.set_client_keyboard_focus(Some(first_id));
    h.pump();
    h.client.keyboard.clear();

    // evdev KEY_BACKSPACE.  Smithay receives xkb keycode 22 internally but
    // the wire protocol correctly carries the evdev value 14.
    assert!(h.state.deliver_client_key(14, true, true));
    h.pump();
    h.state.set_client_keyboard_focus(Some(second_id));
    h.pump();
    assert!(h.state.deliver_client_key(14, false, true));
    h.pump();

    assert_eq!(
        h.client.keyboard,
        vec![
            KeyboardWire::Key {
                key: 14,
                pressed: true,
            },
            KeyboardWire::Leave {
                surface: surface_id(&first),
            },
            KeyboardWire::Enter {
                surface: surface_id(&second),
                keys: vec![14],
            },
            KeyboardWire::Key {
                key: 14,
                pressed: false,
            },
        ],
        "focus replacement must hand the held key to the new surface, then exactly one release follows there"
    );
}

#[test]
fn clicking_a_foreign_window_gives_the_next_injected_key_to_that_client() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let shm = h.bind::<wl_shm::WlShm>("wl_shm", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.ClickThenType".into());
    xdg.set_window_geometry(1, 1, 2, 2);
    h.pump();
    let pixels = tempfile::tempfile().expect("temporary mapped pixels");
    pixels.set_len(4 * 4 * 4).expect("size mapped pixels");
    let pool = shm.create_pool(pixels.as_fd(), 4 * 4 * 4, &qh, ());
    let buffer = pool.create_buffer(0, 4, 4, 4 * 4, wl_shm::Format::Argb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, 4, 4);
    surface.commit();
    h.pump();

    let foreign_surface_id = foreign_id(&h.state, "org.example.ClickThenType");
    let mut frame = shell::compositor::frame::FrameBuilder::with_handle(
        h.state.workspace_tree.clone(),
        false,
        (1600, 900),
    );
    let _ = frame.paint_settled();
    let target = {
        let chrome = frame.chrome();
        shell::shell::input_policy::foreign_tiles(&chrome.tree(), frame.host())
            .into_iter()
            .find(|hit| hit.surface_id == foreign_surface_id)
            .expect("the adopted foreign window is visibly tiled")
            .rect
            .center()
    };
    assert_eq!(h.state.client_input.keyboard_focus(), None);
    let store = shell::compositor::foreign::store();
    let placement = store
        .placement(foreign_surface_id)
        .expect("paint publishes the authoritative foreign placement");
    let content = store.target_box(foreign_surface_id, placement);
    let origin = store.surface_origin(foreign_surface_id, placement);
    assert!(content.contains(target));
    assert_eq!(
        origin,
        (content.x0 - 1.0, content.y0 - 1.0),
        "the route transform retains the client's one-pixel window geometry offset"
    );
    let modifiers = Modifiers::default();
    let (_, click) = shell::shell::test_control::dispatch_command(
        &shell::shell::test_control::Command::ClickAt {
            x: target.x,
            y: target.y,
        },
        &EmptyControlState,
    );
    route_test_control_actions(&mut h, &mut frame, click, modifiers);
    h.pump();
    assert_eq!(
        h.state.client_input.keyboard_focus(),
        Some(foreign_surface_id),
        "the real mapped click must transfer the compositor keyboard seat"
    );
    h.client.keyboard.clear();

    let (_, key) = shell::shell::test_control::dispatch_command(
        &shell::shell::test_control::Command::Key {
            key: "ArrowLeft".into(),
        },
        &EmptyControlState,
    );
    route_test_control_actions(&mut h, &mut frame, key, modifiers);
    h.pump();

    assert_eq!(
        h.client.keyboard,
        vec![
            KeyboardWire::Key {
                key: 105,
                pressed: true,
            },
            KeyboardWire::Key {
                key: 105,
                pressed: false,
            },
        ],
        "a physical click must establish the compositor seat focus used by the next key"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the wire-level ownership scenario keeps both client input and compositor assertions in one transcript"
)]
fn compositor_shortcuts_and_lock_capture_own_the_keyboard_before_clients() {
    // Phase `custom_shortcut_and_f1_are_owned_before_a_focused_foreign_client`.
    // The ownership decision has to happen before a foreign client receives the
    // press: merely teaching the chrome about a custom binding is too late once
    // an Electron/RPM/Flatpak client has consumed it.  Keep the matching release
    // out of the client as well, then prove the same cached config drives the
    // actual navigation action.  F1 is intentionally non-configurable desktop
    // help and must follow the same foreign-client boundary.
    {
        let mut h = Harness::new();
        let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
        let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
        let qh = h.queue.handle();

        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id("org.example.ShortcutOwnership".into());
        h.pump();
        let foreign_surface_id = foreign_id(&h.state, "org.example.ShortcutOwnership");
        h.state.set_client_keyboard_focus(Some(foreign_surface_id));
        h.pump();
        h.client.keyboard.clear();

        let mut frame = shell::compositor::frame::FrameBuilder::new((1600, 900));
        let mut shortcuts = settings_store::hardware::ShortcutConfig::default();
        shortcuts
            .set(
                settings_store::hardware::ShortcutAction::NextTab,
                settings_store::hardware::KeyChord::parse("Ctrl+J").expect("valid custom chord"),
            )
            .expect("custom chord is not shared by another action");
        frame
            .host_mut()
            .set_shortcut_config(shortcuts)
            .expect("valid effective shortcut configuration");

        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        let bare_ctrl_press = captured_key_event(Key::Named(NamedKey::Control), true);
        let bare_ctrl_release = captured_key_event(Key::Named(NamedKey::Control), false);
        let old_tab_press = captured_key_event(Key::Named(NamedKey::Tab), true);
        let old_tab_release = captured_key_event(Key::Named(NamedKey::Tab), false);
        let custom_press = captured_key_event(Key::Character("j".into()), true);
        let custom_release = captured_key_event(Key::Character("j".into()), false);

        // Ctrl+Tab was Next Tab before the rebind.  The static compositor
        // firewall must not keep claiming that old default after the effective
        // table assigns Next Tab to Ctrl+J.
        assert!(shell::compositor::client_input::route_input(
            &mut h.state,
            &mut frame,
            &bare_ctrl_press,
            &ctrl,
        ));
        assert!(shell::compositor::client_input::route_input(
            &mut h.state,
            &mut frame,
            &old_tab_press,
            &ctrl,
        ));
        assert!(shell::compositor::client_input::route_input(
            &mut h.state,
            &mut frame,
            &old_tab_release,
            &ctrl,
        ));
        assert!(shell::compositor::client_input::route_input(
            &mut h.state,
            &mut frame,
            &bare_ctrl_release,
            &Modifiers::default(),
        ));
        h.pump();
        assert_eq!(
            h.client.keyboard,
            vec![
                KeyboardWire::Key {
                    key: 29,
                    pressed: true
                },
                KeyboardWire::Key {
                    key: 15,
                    pressed: true
                },
                KeyboardWire::Key {
                    key: 15,
                    pressed: false
                },
                KeyboardWire::Key {
                    key: 29,
                    pressed: false
                },
            ],
            "the previous Ctrl+Tab default must be a balanced client chord after rebinding"
        );
        h.client.keyboard.clear();

        let before_tab =
            frame.chrome().tree().desktop.workspaces[0].strips[0].window_groups[0].active_tab;
        assert!(
            shell::compositor::client_input::route_input(
                &mut h.state,
                &mut frame,
                &bare_ctrl_press,
                &ctrl,
            ),
            "a bare modifier must remain visible to the foreign client's xkb state"
        );
        assert!(
            !shell::compositor::client_input::route_input(
                &mut h.state,
                &mut frame,
                &custom_press,
                &ctrl,
            ),
            "Ctrl+J must be claimed before the focused foreign client"
        );
        let mut doc = automerge::AutoCommit::new();
        let mut focus = None;
        assert!(frame.dispatch_event(
            &custom_press,
            &ctrl,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert!(
            !shell::compositor::client_input::route_input(
                &mut h.state,
                &mut frame,
                &custom_release,
                &ctrl,
            ),
            "the foreign client must not receive a release for its unseen Ctrl+J press"
        );
        assert!(
            shell::compositor::client_input::route_input(
                &mut h.state,
                &mut frame,
                &bare_ctrl_release,
                &Modifiers::default(),
            ),
            "the modifier release must still balance the foreign client's xkb state"
        );
        h.pump();
        assert_eq!(
            h.client.keyboard,
            vec![
                KeyboardWire::Key {
                    key: 29,
                    pressed: true,
                },
                KeyboardWire::Key {
                    key: 29,
                    pressed: false,
                },
            ],
            "only the bare Ctrl transitions, never the custom Ctrl+J pair, may reach the foreign-client wire"
        );
        assert_ne!(
            frame.chrome().tree().desktop.workspaces[0].strips[0].window_groups[0].active_tab,
            before_tab,
            "the cached custom binding must fire Next Tab after pre-routing"
        );

        h.client.keyboard.clear();

        let f1_press = captured_key_event(Key::Named(NamedKey::F1), true);
        let f1_release = captured_key_event(Key::Named(NamedKey::F1), false);
        let no_modifiers = Modifiers::default();
        assert!(
            !shell::compositor::client_input::route_input(
                &mut h.state,
                &mut frame,
                &f1_press,
                &no_modifiers,
            ),
            "F1 desktop help must be claimed before the focused foreign client"
        );
        assert!(frame.dispatch_event(
            &f1_press,
            &no_modifiers,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert!(
            !shell::compositor::client_input::route_input(
                &mut h.state,
                &mut frame,
                &f1_release,
                &no_modifiers,
            ),
            "the foreign client must not receive an unmatched F1 release"
        );
        h.pump();
        assert!(
            h.client.keyboard.is_empty(),
            "F1 must never reach the focused foreign client"
        );
        assert!(
            frame.chrome().overlays.shortcut_help,
            "F1 must open the compositor-owned keyboard guide"
        );
    }

    // Phase `lock_capture_boundary_hides_held_keys_and_restores_a_clean_seat`.
    // This is the precise seat sequence used at the lock boundary: the client
    // first owns held modifier/editing keys, lock revokes focus but retains the
    // seat's physical state, physical ups arrive while capture is active, then an app is focused
    // again.  No release or stale modifier may leak into either locked or newly
    // restored client input.
    {
        let mut h = Harness::new();
        let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
        let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
        let qh = h.queue.handle();

        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id("org.example.LockBoundary".into());
        h.pump();
        let foreign_surface_id = foreign_id(&h.state, "org.example.LockBoundary");
        h.state.set_client_keyboard_focus(Some(foreign_surface_id));
        h.pump();
        h.client.keyboard.clear();

        assert!(h.state.deliver_client_key(42, true, true));
        assert!(h.state.deliver_client_key(14, true, true));
        h.pump();
        assert_eq!(
            h.client.keyboard,
            vec![
                KeyboardWire::Key {
                    key: 42,
                    pressed: true,
                },
                KeyboardWire::Key {
                    key: 14,
                    pressed: true,
                },
            ]
        );

        // Native `run()` calls this immediately when SessionManager enters lock:
        // client delivery stops, while Smithay retains the actual held keys until
        // the locked physical ups below.
        h.state.set_client_keyboard_focus(None);
        h.pump();
        h.client.keyboard.clear();

        // These are the physical ups which arrived while the lock owns input.
        // They advance Smithay's state but, with no focus, cannot be observed by
        // the old client.
        shell::compositor::client_input::consume_keyboard_while_captured(
            &mut h.state,
            &captured_key_event(Key::Named(NamedKey::Backspace), false),
        );
        shell::compositor::client_input::consume_keyboard_while_captured(
            &mut h.state,
            &captured_key_event(Key::Named(NamedKey::Shift), false),
        );
        h.pump();
        assert!(
            h.client.keyboard.is_empty(),
            "a locked client boundary must not emit a key up to the hidden app"
        );
        assert!(
            h.state
                .seat
                .get_keyboard()
                .expect("seat keyboard")
                .pressed_keys()
                .is_empty(),
            "Shift/Backspace must be cleared before unlock restores application focus"
        );

        h.state.set_client_keyboard_focus(Some(foreign_surface_id));
        h.pump();
        assert_eq!(
            h.client.keyboard,
            vec![KeyboardWire::Enter {
                surface: surface_id(&surface),
                keys: Vec::new(),
            }],
            "restored focus must not inherit a held modifier or editing key"
        );
        h.client.keyboard.clear();
        assert!(h.state.deliver_client_key(30, true, true));
        assert!(h.state.deliver_client_key(30, false, true));
        h.pump();
        assert_eq!(
            h.client.keyboard,
            vec![
                KeyboardWire::Key {
                    key: 30,
                    pressed: true,
                },
                KeyboardWire::Key {
                    key: 30,
                    pressed: false,
                },
            ],
            "typing after unlock must be a normal, balanced new key sequence"
        );
    }
}

/// A key event carrying a real physical position, as the DRM and winit
/// backends deliver it; `GlobalShortcuts` grants match physical keys.
fn physical_key_event(logical: Key, code: winit::keyboard::KeyCode, pressed: bool) -> WindowEvent {
    let mut event = captured_key_event(logical, pressed);
    if let WindowEvent::KeyboardInput { event, .. } = &mut event {
        event.physical_key = PhysicalKey::Code(code);
    }
    event
}

fn drain_edges(
    events: &shell::portal::global_shortcuts::GlobalShortcutsHandle,
) -> Vec<(String, shell::portal::global_shortcuts::ShortcutEdge)> {
    events
        .drain_activations()
        .into_iter()
        .map(|event| (event.shortcut_id, event.edge))
        .collect()
}

/// An approved `GlobalShortcuts` grant must fire while an app holds keyboard
/// focus: the compositor claims the press before the focused client sees
/// it, queues `Activated`, and on release queues `Deactivated` (which
/// push-to-talk clients wait for) without ever showing the client either
/// edge.
#[allow(
    clippy::too_many_lines,
    reason = "the grant, the focused client and both key edges form one wire transcript"
)]
#[test]
fn granted_global_shortcut_fires_over_a_focused_client_and_deactivates_on_release() {
    use shell::portal::global_shortcuts::{
        GlobalShortcut, GlobalShortcutBindingRequest, GlobalShortcutsHandle,
        GlobalShortcutsService as _, ShellGlobalShortcutsService, ShortcutEdge,
    };

    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Focused".into());
    h.pump();
    let focused = foreign_id(&h.state, "org.example.Focused");
    h.state.set_client_keyboard_focus(Some(focused));
    h.pump();
    h.client.keyboard.clear();

    // Grant Ctrl+Shift+M to another app through the reviewed consent path.
    let session = "/org/freedesktop/portal/desktop/session/talk";
    let dialogs = shell::shell::portal_dialog::PortalDialogQueue::default();
    let service = ShellGlobalShortcutsService::with_dialogs(dialogs.clone());
    let events =
        GlobalShortcutsHandle::with_bound_session_for_tests(session, "org.example.Talk", &["talk"]);
    service.attach_event_bridge(events.clone());
    assert!(
        service.is_reserved("CTRL+SHIFT+KEYT"),
        "reopen belongs to the shell"
    );
    assert!(
        !service.is_reserved("CTRL+SHIFT+KEYM"),
        "the reviewed grant is unreserved"
    );
    h.state.global_shortcuts = service.clone();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let (reply, approved) = tokio::sync::oneshot::channel();
        let _ = service.request_bind(
            GlobalShortcutBindingRequest {
                handle: "/request/talk".into(),
                session: session.into(),
                app_id: "org.example.Talk".into(),
                parent_window: String::new(),
                shortcuts: vec![GlobalShortcut {
                    id: "talk".into(),
                    description: Some("Push to talk".into()),
                    preferred_trigger: Some("Ctrl+Shift+M".into()),
                }],
            },
            reply,
        );
        let mut review = shell::shell::portal_dialog::PortalDialogController::new(dialogs);
        assert!(review.pump(), "the grant is reviewed by the shell");
        assert!(review.confirm());
        approved.await.expect("the review resolves");
    });

    let mut frame = shell::compositor::frame::FrameBuilder::new((1600, 900));
    let chord = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);
    let press = physical_key_event(
        Key::Character("M".into()),
        winit::keyboard::KeyCode::KeyM,
        true,
    );
    let release = physical_key_event(
        Key::Character("M".into()),
        winit::keyboard::KeyCode::KeyM,
        false,
    );
    assert!(
        !shell::compositor::client_input::route_input(&mut h.state, &mut frame, &press, &chord),
        "the granted chord must be claimed before the focused client"
    );
    h.pump();
    assert!(
        h.client.keyboard.is_empty(),
        "the focused client must not see the granted press"
    );
    assert_eq!(
        drain_edges(&events),
        vec![("talk".to_string(), ShortcutEdge::Activated)]
    );

    assert!(
        !shell::compositor::client_input::route_input(&mut h.state, &mut frame, &release, &chord),
        "the release of a granted press stays with the grant"
    );
    h.pump();
    assert!(
        h.client.keyboard.is_empty(),
        "the focused client must not see an unmatched release"
    );
    assert_eq!(
        drain_edges(&events),
        vec![("talk".to_string(), ShortcutEdge::Deactivated)]
    );
}

/// A bounded real-socket stress pass for the failure pattern Electron users
/// described: editing keys held while focus moves in response to a layout or
/// activation revision.  Every press must get one release, regardless of
/// which toplevel owns focus at the moment it arrives.
#[test]
fn focus_handoff_soak_has_one_release_per_backspace_press() {
    const ROUNDS: usize = 128;
    const MAX_DISPATCH: Duration = Duration::from_millis(500);

    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let first = compositor.create_surface(&qh, ());
    let first_xdg = wm.get_xdg_surface(&first, &qh, ());
    let first_top = first_xdg.get_toplevel(&qh, ());
    first_top.set_app_id("org.example.SoakFirst".into());
    let second = compositor.create_surface(&qh, ());
    let second_xdg = wm.get_xdg_surface(&second, &qh, ());
    let second_top = second_xdg.get_toplevel(&qh, ());
    second_top.set_app_id("org.example.SoakSecond".into());
    h.pump();

    let first_id = foreign_id(&h.state, "org.example.SoakFirst");
    let second_id = foreign_id(&h.state, "org.example.SoakSecond");
    h.client.keyboard.clear();
    let mut max_dispatch = Duration::ZERO;

    for round in 0..ROUNDS {
        let (old, new) = if round % 2 == 0 {
            (first_id, second_id)
        } else {
            (second_id, first_id)
        };
        h.state.set_client_keyboard_focus(Some(old));
        h.pump();
        let started = Instant::now();
        assert!(h.state.deliver_client_key(14, true, true));
        h.pump();
        h.state.set_client_keyboard_focus(Some(new));
        h.pump();
        assert!(h.state.deliver_client_key(14, false, true));
        h.pump();
        max_dispatch = max_dispatch.max(started.elapsed());
    }

    let keys: Vec<_> = h
        .client
        .keyboard
        .iter()
        .filter_map(|event| match event {
            KeyboardWire::Key { key, pressed } => Some((*key, *pressed)),
            KeyboardWire::Enter { .. } | KeyboardWire::Leave { .. } => None,
        })
        .collect();
    assert_eq!(
        keys.len(),
        ROUNDS * 2,
        "every physical press/release must reach exactly one focused client"
    );
    for pair in keys.chunks_exact(2) {
        assert_eq!(pair, [(14, true), (14, false)]);
    }
    assert!(
        h.state
            .seat
            .get_keyboard()
            .expect("seat keyboard")
            .pressed_keys()
            .is_empty(),
        "no Backspace may remain pressed after the bounded focus soak"
    );
    eprintln!(
        "keyboard focus soak: {ROUNDS} handoffs, max press-to-release dispatch {max_dispatch:?}"
    );
    assert!(
        max_dispatch < MAX_DISPATCH,
        "socket dispatch stalled for {max_dispatch:?}; limit is {MAX_DISPATCH:?}"
    );
}
