//! Real Wayland clients against a headless compositor: windows are mapped
//! where the policy says, composited with hosted layers on top, and input
//! reaches whoever it belongs to.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

#[path = "headless/harness.rs"]
mod harness;
#[path = "headless/session_follow.rs"]
mod session_follow;
#[path = "headless/window_icons.rs"]
mod window_icons;

use harness::{Seen, TestClient, headless, mapped_client, pump};

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Logical, Point, Rectangle, Transform};
use tilo_compositor::{
    Arrangement, BackdropImage, BackdropRequest, Compositor, GestureEvent, HostDrag, HostFrame,
    HostKey, HostLayer, IconArtwork, InputEvent, KeyClaim, KeyboardLayout, OutputConfig, PadAction,
    StackPolicy, SurfaceHost, Switch, TabletEvent, TabletPadEvent, TabletPhase, TextField,
    TextFieldRefusal, TextInsertion, WindowEffect, WindowFrame, WindowIcon, WindowInfo,
    WindowPicture, WindowPolicy,
};
use wayland_client::Connection;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ContentHint, ContentPurpose,
};

const BTN_LEFT: u32 = 0x110;
const KEY_A: u32 = 30;
const KEY_ESCAPE: u32 = 1;
const KEY_LEFTCTRL: u32 = 29;
const KEY_LEFTALT: u32 = 56;
const KEY_LEFTMETA: u32 = 125;
const KEY_X: u32 = 45;
const BACKGROUND: [u8; 4] = [26, 27, 38, 255];

#[test]
fn a_window_is_composited_where_the_policy_places_it() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);

    let windows = compositor.windows();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].1, (220, 190, 200, 100), "centred on the output");

    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!((frame.width, frame.height), (640, 480));
    assert_eq!(frame.pixel(320, 240), Some([255, 0, 0, 255]));
    assert_eq!(
        frame.pixel(232, 200),
        Some([255, 0, 0, 255]),
        "inside the top left corner"
    );
    let corner = frame.pixel(221, 191).expect("a corner pixel");
    assert!(
        corner[0] < 90 && corner[1] < 90,
        "the corner is cut round, showing what is behind: {corner:?}"
    );
    assert_eq!(frame.pixel(10, 10), Some(BACKGROUND));

    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.frames_done > 0
    });
}

#[test]
fn a_window_opens_from_its_first_buffer() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.seat.is_some()
    });
    client.open_window("opening");
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    // Committed without a buffer, it is not on the desktop yet: nothing
    // stands there to be animated open.
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert!(compositor.windows().is_empty());
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(320, 240), Some(BACKGROUND));

    // Its first buffer is what it opens from: the desktop animates the
    // window it can already see.
    client.show(200, 100, [255, 0, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        !compositor.windows().is_empty()
    });
    let (info, rect) = compositor.windows()[0].clone();
    assert_eq!(rect, (220, 190, 200, 100));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(320, 240), Some([255, 0, 0, 255]));

    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            opacity: 0.5,
            scale: 0.9,
            ..WindowEffect::default()
        }),
    );
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let opening = frame.pixel(320, 240).expect("a pixel");
    assert_ne!(opening, [255, 0, 0, 255], "opening faded");
    assert_ne!(opening, BACKGROUND, "from its first buffer");
}

#[test]
fn a_scaled_output_composites_in_physical_pixels() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).unwrap();
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 1280, 960).with_scale(2.0))
        .unwrap();
    let _client = mapped_client(&mut compositor, "blue", (200, 100), [0, 0, 255, 255]);
    // A logical 640x480 output: the scale-1 buffer is centred there and
    // drawn at twice its size, physical (440, 380) to (840, 580); edges are
    // filtered, so the checks sit a few pixels inside and outside.
    assert_eq!(compositor.windows()[0].1, (220, 190, 200, 100));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(460, 400), Some([0, 0, 255, 255]));
    assert_eq!(frame.pixel(820, 560), Some([0, 0, 255, 255]));
    assert_eq!(frame.pixel(844, 584), Some(BACKGROUND));
}

#[test]
fn pointer_and_keys_reach_the_window_under_and_with_focus() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "input", (200, 100), [0, 255, 0, 255]);
    assert_eq!(
        compositor
            .focused_window()
            .and_then(|info| info.app_id)
            .as_deref(),
        Some("input")
    );

    compositor
        .input(InputEvent::PointerMotion { x: 320.0, y: 240.0 })
        .unwrap();
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .unwrap();
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        })
        .unwrap();
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
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() == 2
    });

    assert!(client.seen.pointer_inside);
    assert_eq!(
        client.seen.buttons,
        vec![(BTN_LEFT, true), (BTN_LEFT, false)]
    );
    assert!(client.seen.keyboard_focused);
    assert_eq!(client.seen.keys, vec![(KEY_A, true), (KEY_A, false)]);

    // Off the window the client loses the pointer.
    compositor
        .input(InputEvent::PointerMotion { x: 5.0, y: 5.0 })
        .unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        !clients[0].seen.pointer_inside
    });
}

#[test]
fn clients_connect_through_the_listening_socket() {
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        eprintln!("no XDG_RUNTIME_DIR: nothing to listen in");
        return;
    };
    let mut compositor = headless(Box::new(StackPolicy));
    let name = compositor.listen().unwrap();
    let socket =
        std::os::unix::net::UnixStream::connect(std::path::Path::new(&runtime).join(&name))
            .unwrap();
    let connection = Connection::from_socket(socket).unwrap();
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut seen = Seen::default();
    for _ in 0..500 {
        connection.flush().unwrap();
        compositor.dispatch(Duration::from_millis(2)).unwrap();
        if let Some(guard) = connection.prepare_read() {
            let _ = guard.read();
        }
        queue.dispatch_pending(&mut seen).unwrap();
        if seen.wm_base.is_some() {
            return;
        }
    }
    panic!("the client never saw xdg_wm_base");
}

#[test]
fn clients_find_the_protocols_they_expect() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some()
    });
    for interface in [
        "wl_compositor",
        "wl_shm",
        "wl_seat",
        "wl_output",
        "wl_data_device_manager",
        "xdg_wm_base",
        "zxdg_output_manager_v1",
        "zwp_linux_dmabuf_v1",
        "wp_viewporter",
        "wp_fractional_scale_manager_v1",
        "wp_presentation",
        "zxdg_decoration_manager_v1",
        "xdg_activation_v1",
        "zwp_primary_selection_device_manager_v1",
        "wp_cursor_shape_manager_v1",
        "zwp_relative_pointer_manager_v1",
        "zwp_pointer_constraints_v1",
        "zwp_pointer_gestures_v1",
        "zwp_tablet_manager_v2",
        "zwp_idle_inhibit_manager_v1",
        "zwp_keyboard_shortcuts_inhibit_manager_v1",
        "wp_content_type_manager_v1",
        "zwp_text_input_manager_v3",
        "wp_color_manager_v1",
    ] {
        assert!(
            client.seen.globals.iter().any(|global| global == interface),
            "{interface} is offered"
        );
    }
}

#[test]
fn a_trusted_client_locks_the_session_and_hides_every_window() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut window = mapped_client(&mut compositor, "private", (200, 100), [255, 0, 0, 255]);
    assert!(
        !window
            .seen
            .globals
            .iter()
            .any(|global| global == "ext_session_lock_manager_v1"),
        "an ordinary client cannot lock the session"
    );

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
    pump(&mut compositor, &mut [&mut locker], |_, clients| {
        clients[0].seen.lock_manager.is_some()
    });
    let _lock = locker
        .seen
        .lock_manager
        .as_ref()
        .unwrap()
        .lock(&locker.queue.handle(), ());
    pump(
        &mut compositor,
        &mut [&mut locker, &mut window],
        |compositor, _| compositor.locked(),
    );
    compositor.render().unwrap();
    pump(&mut compositor, &mut [&mut locker], |_, clients| {
        clients[0].seen.locked
    });

    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(320, 240),
        Some(BACKGROUND),
        "the window is hidden"
    );
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .unwrap();
    pump(&mut compositor, &mut [&mut window], |_, _| true);
    assert!(
        window.seen.keys.is_empty(),
        "keys do not reach windows behind the lock"
    );
}

/// Places windows left to right, 300 logical pixels apart.
#[derive(Default)]
struct Columns {
    placed: i32,
}

impl WindowPolicy for Columns {
    fn initial_size(
        &mut self,
        _: &WindowInfo,
        _: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        Some((250, 200))
    }

    fn place(
        &mut self,
        _: &WindowInfo,
        _: (i32, i32),
        _: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        let x = self.placed * 300;
        self.placed += 1;
        Point::from((x, 0))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, Rectangle<i32, Logical>)],
        _: &[Rectangle<i32, Logical>],
    ) -> Option<Vec<Arrangement>> {
        // Packed left to right by id, so closing one closes the gap.
        let mut ids: Vec<u64> = windows.iter().map(|(info, _)| info.id).collect();
        ids.sort_unstable();
        self.placed = i32::try_from(ids.len()).unwrap();
        Some(
            ids.into_iter()
                .zip(0..)
                .map(|(id, column)| Arrangement {
                    id,
                    location: Point::from((column * 300, 0)),
                    size: None,
                })
                .collect(),
        )
    }
}

#[test]
fn the_policy_places_windows_and_a_click_moves_focus() {
    let mut compositor = headless(Box::<Columns>::default());
    let mut left = mapped_client(&mut compositor, "left", (250, 200), [255, 0, 0, 255]);
    let mut right = mapped_client(&mut compositor, "right", (250, 200), [0, 0, 255, 255]);
    let rects: Vec<_> = compositor
        .windows()
        .into_iter()
        .map(|(info, rect)| (info.app_id.unwrap(), rect))
        .collect();
    assert_eq!(
        rects,
        vec![
            ("left".into(), (0, 0, 250, 200)),
            ("right".into(), (300, 0, 250, 200))
        ]
    );
    assert_eq!(
        compositor
            .focused_window()
            .and_then(|info| info.app_id)
            .as_deref(),
        Some("right")
    );

    compositor
        .input(InputEvent::PointerMotion { x: 100.0, y: 100.0 })
        .unwrap();
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .unwrap();
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        })
        .unwrap();
    pump(
        &mut compositor,
        &mut [&mut left, &mut right],
        |_, clients| clients[0].seen.keyboard_focused && !clients[1].seen.keyboard_focused,
    );
    assert_eq!(
        compositor
            .focused_window()
            .and_then(|info| info.app_id)
            .as_deref(),
        Some("left")
    );
    assert_eq!(left.seen.buttons, vec![(BTN_LEFT, true), (BTN_LEFT, false)]);
    assert!(right.seen.buttons.is_empty());

    // Closing the left window lets the policy close the gap.
    left.toplevel.take().unwrap().destroy();
    left.surface.take().unwrap().destroy();
    pump(
        &mut compositor,
        &mut [&mut left, &mut right],
        |compositor, _| compositor.windows().len() == 1,
    );
    assert_eq!(compositor.windows()[0].1, (0, 0, 250, 200));
}

#[derive(Default)]
struct HostLog {
    motions: Vec<(f64, f64)>,
    buttons: Vec<(u32, bool)>,
    left: u32,
    keys: Vec<HostKey>,
    wants_keyboard: bool,
    /// Each Super+X the surface was offered, and whether it was told a client
    /// holds the desktop's shortcuts.
    taken: Vec<(u32, bool)>,
}

/// A hosted surface: one square of one colour.
struct Square {
    rect: (i32, i32, i32, i32),
    buffer: MemoryRenderBuffer,
    log: Rc<RefCell<HostLog>>,
    below: bool,
}

impl Square {
    fn new(rect: (i32, i32, i32, i32), rgba: [u8; 4], log: Rc<RefCell<HostLog>>) -> Self {
        let pixel = [rgba[2], rgba[1], rgba[0], rgba[3]];
        let bytes = pixel.repeat(usize::try_from(rect.2 * rect.3).unwrap());
        let buffer = MemoryRenderBuffer::from_slice(
            &bytes,
            Fourcc::Argb8888,
            (rect.2, rect.3),
            1,
            Transform::Normal,
            None,
        );
        Self {
            rect,
            buffer,
            log,
            below: false,
        }
    }

    /// The same square, under every window.
    fn below(mut self) -> Self {
        self.below = true;
        self
    }
}

impl SurfaceHost for Square {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            location: (self.rect.0, self.rect.1),
            size: (self.rect.2, self.rect.3),
            frame: HostFrame::from_buffer(
                self.buffer.clone(),
                (
                    u32::try_from(self.rect.2).expect("a whole number of pixels"),
                    u32::try_from(self.rect.3).expect("a whole number of pixels"),
                ),
            ),
        }]
    }

    fn below_windows(&self) -> bool {
        self.below
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (rx, ry, w, h) = self.rect;
        x >= f64::from(rx) && y >= f64::from(ry) && x < f64::from(rx + w) && y < f64::from(ry + h)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.log.borrow_mut().motions.push((x, y));
    }

    fn pointer_leave(&mut self) {
        self.log.borrow_mut().left += 1;
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.log.borrow_mut().buttons.push((button, pressed));
    }

    fn wants_keyboard(&self) -> bool {
        self.log.borrow().wants_keyboard
    }

    fn key(&mut self, key: &HostKey) {
        self.log.borrow_mut().keys.push(key.clone());
    }
}

/// The desktop's own key surface, as the session's is: it answers Super+X
/// wherever the keyboard is, unless a client holds the desktop's shortcuts.
struct Chords {
    log: Rc<RefCell<HostLog>>,
}

impl SurfaceHost for Chords {
    fn layers(&self) -> Vec<HostLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn claim_key(&mut self, key: &HostKey, _locked: bool, inhibited: bool) -> KeyClaim {
        if key.keycode != KEY_X || !key.modifiers.logo {
            return KeyClaim::Pass;
        }
        self.log.borrow_mut().taken.push((key.keycode, inhibited));
        if inhibited {
            KeyClaim::Pass
        } else {
            KeyClaim::Take
        }
    }

    fn key(&mut self, key: &HostKey) {
        self.log.borrow_mut().keys.push(key.clone());
    }
}

#[test]
fn a_hosted_surface_that_wants_the_keyboard_takes_keys_from_the_focused_window() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Box::new(Square::new(
        (0, 0, 10, 10),
        [0, 255, 0, 255],
        Rc::clone(&log),
    )));
    let mut client = mapped_client(&mut compositor, "typing", (200, 100), [255, 0, 0, 255]);

    log.borrow_mut().wants_keyboard = true;
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
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    let keys = log.borrow().keys.clone();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        (keys[0].keycode, keys[0].pressed, keys[0].text),
        (KEY_A, true, Some('a'))
    );
    assert_eq!(keys[0].keysym, 0x61, "XK_a");
    assert!(!keys[1].pressed);
    assert!(client.seen.keys.is_empty());

    log.borrow_mut().wants_keyboard = false;
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() == 1
    });
    assert_eq!(log.borrow().keys.len(), 2);
}

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

#[test]
fn hosted_surfaces_draw_above_windows_and_take_the_pointer_over_them() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Box::new(Square::new(
        (300, 220, 40, 40),
        [0, 255, 0, 255],
        Rc::clone(&log),
    )));
    let mut client = mapped_client(&mut compositor, "under", (200, 100), [255, 0, 0, 255]);

    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(320, 240),
        Some([0, 255, 0, 255]),
        "hosted layer on top"
    );
    assert_eq!(
        frame.pixel(250, 200),
        Some([255, 0, 0, 255]),
        "window beside it"
    );

    compositor
        .input(InputEvent::PointerMotion { x: 320.0, y: 240.0 })
        .unwrap();
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .unwrap();
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert_eq!(log.borrow().motions, vec![(320.0, 240.0)]);
    assert_eq!(log.borrow().buttons, vec![(BTN_LEFT, true)]);
    assert!(!client.seen.pointer_inside);
    assert!(client.seen.buttons.is_empty());

    // Moving onto the window hands the pointer back.
    compositor
        .input(InputEvent::PointerMotion { x: 250.0, y: 200.0 })
        .unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.pointer_inside
    });
    assert_eq!(log.borrow().left, 1);
}

#[test]
fn touches_reach_the_window_under_them_and_hosted_surfaces_as_presses() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Box::new(Square::new(
        (10, 10, 40, 40),
        [0, 255, 0, 255],
        Rc::clone(&log),
    )));
    // Centred: the window spans (220, 190) to (420, 290).
    let mut client = mapped_client(&mut compositor, "touched", (200, 100), [255, 0, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.touch.is_some()
    });

    for event in [
        InputEvent::TouchDown {
            slot: 0,
            x: 300.0,
            y: 240.0,
        },
        InputEvent::TouchFrame,
        // Dragged off the window, the finger stays the window's.
        InputEvent::TouchMotion {
            slot: 0,
            x: 500.0,
            y: 240.0,
        },
        InputEvent::TouchFrame,
        InputEvent::TouchUp { slot: 0 },
        InputEvent::TouchFrame,
    ] {
        compositor.input(event).unwrap();
    }
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.touches.len() >= 6
    });
    assert_eq!(
        client.seen.touches,
        [
            "down 0 80 50",
            "frame",
            "motion 0 280 50",
            "frame",
            "up 0",
            "frame"
        ]
    );
    assert!(client.seen.keyboard_focused, "a touch focuses like a click");

    compositor
        .input(InputEvent::TouchDown {
            slot: 1,
            x: 20.0,
            y: 20.0,
        })
        .unwrap();
    compositor
        .input(InputEvent::TouchMotion {
            slot: 1,
            x: 25.0,
            y: 20.0,
        })
        .unwrap();
    compositor.input(InputEvent::TouchUp { slot: 1 }).unwrap();
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert_eq!(log.borrow().motions, [(20.0, 20.0), (25.0, 20.0)]);
    assert_eq!(log.borrow().buttons, [(BTN_LEFT, true), (BTN_LEFT, false)]);
    assert_eq!(log.borrow().left, 1);
    assert_eq!(client.seen.touches.len(), 6, "the window saw none of it");
}

#[test]
fn a_hosted_apps_drag_carries_its_icon_under_the_pointer() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Box::new(Square::new(
        (0, 0, 40, 40),
        [0, 255, 0, 255],
        Rc::clone(&log),
    )));
    let shown = |compositor: &mut Compositor, x, y| {
        compositor.render().unwrap();
        compositor.screenshot("HEADLESS-1").unwrap().pixel(x, y)
    };
    assert_eq!(shown(&mut compositor, 10, 10), Some([0, 255, 0, 255]));

    // A 20x20 icon, its hotspot two pixels in from its top left.
    let bytes = [0, 255, 255, 255].repeat(20 * 20);
    let icon = HostFrame::from_buffer(
        MemoryRenderBuffer::from_slice(
            &bytes,
            Fourcc::Argb8888,
            (20, 20),
            1,
            Transform::Normal,
            None,
        ),
        (20, 20),
    );
    compositor
        .input(InputEvent::PointerMotion { x: 5.0, y: 5.0 })
        .unwrap();
    assert!(
        compositor
            .start_host_drag(HostDrag {
                offer: vec![("text/plain".into(), b"dragged".to_vec())],
                icon: Some((icon, Point::from((2, 2)))),
            })
            .unwrap(),
        "the hosted app's drag starts"
    );
    assert_eq!(
        shown(&mut compositor, 10, 10),
        Some([255, 255, 0, 255]),
        "the icon hangs off the pointer at its hotspot"
    );
    assert_eq!(shown(&mut compositor, 2, 2), Some([0, 255, 0, 255]));

    // It follows the pointer.
    compositor
        .input(InputEvent::PointerMotion { x: 100.0, y: 100.0 })
        .unwrap();
    assert_eq!(shown(&mut compositor, 110, 110), Some([255, 255, 0, 255]));
    assert_eq!(shown(&mut compositor, 10, 10), Some([0, 255, 0, 255]));

    // The drop ends it, icon and all.
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        })
        .unwrap();
    assert_eq!(shown(&mut compositor, 110, 110), Some(BACKGROUND));
}

/// Floating windows; takes touchpad gestures when `claims`, and records
/// what reached it.
struct Desktop {
    claims: bool,
    gestures: Rc<RefCell<Vec<GestureEvent>>>,
    switches: Rc<RefCell<Vec<(Switch, bool)>>>,
}

impl WindowPolicy for Desktop {
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
        StackPolicy.place(window, size, outputs)
    }

    fn gesture(&mut self, gesture: &GestureEvent) -> bool {
        self.gestures.borrow_mut().push(*gesture);
        self.claims
    }

    fn switch(&mut self, switch: Switch, on: bool) {
        self.switches.borrow_mut().push((switch, on));
    }
}

#[test]
fn gestures_of_three_fingers_are_offered_to_the_desktop() {
    let swipe = |fingers| {
        [
            GestureEvent::SwipeBegin { fingers },
            GestureEvent::SwipeUpdate { dx: 0.0, dy: -40.0 },
            GestureEvent::SwipeEnd { cancelled: false },
        ]
    };
    for claims in [true, false] {
        let gestures = Rc::new(RefCell::new(Vec::new()));
        let switches = Rc::new(RefCell::new(Vec::new()));
        let mut compositor = headless(Box::new(Desktop {
            claims,
            gestures: Rc::clone(&gestures),
            switches: Rc::clone(&switches),
        }));
        for event in swipe(2).into_iter().chain(swipe(3)) {
            compositor.input(InputEvent::Gesture(event)).unwrap();
        }
        let expected: Vec<GestureEvent> = if claims {
            swipe(3).to_vec()
        } else {
            // Asked at the begin; declined, the rest goes to clients.
            vec![GestureEvent::SwipeBegin { fingers: 3 }]
        };
        assert_eq!(
            *gestures.borrow(),
            expected,
            "two fingers stay the client's"
        );

        compositor
            .input(InputEvent::Switch {
                switch: Switch::Lid,
                on: true,
            })
            .unwrap();
        assert_eq!(*switches.borrow(), [(Switch::Lid, true)]);
    }
}

#[test]
fn a_pen_is_a_pointer_to_apps_without_tablet_support() {
    use smithay::backend::input::{TabletToolCapabilities, TabletToolDescriptor, TabletToolType};
    use smithay::wayland::tablet_manager::TabletDescriptor;

    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "drawn-on", (200, 100), [255, 0, 0, 255]);
    let pen = |phase| {
        InputEvent::Tablet(TabletEvent {
            tablet: TabletDescriptor {
                name: "Pen tablet".into(),
                usb_id: Some((0x056a, 0x0374)),
                syspath: None,
            },
            tool: TabletToolDescriptor {
                tool_type: TabletToolType::Pen,
                hardware_serial: 7,
                hardware_id_wacom: 0x802,
                capabilities: TabletToolCapabilities::PRESSURE,
            },
            phase,
            x: 300.0,
            y: 240.0,
            pressure: 0.5,
            distance: 0.0,
            tilt: (0.0, 0.0),
            rotation: 0.0,
            slider: 0.0,
            wheel: (0.0, 0),
            as_pointer: false,
            tip_button: Some(BTN_LEFT),
        })
    };
    for phase in [
        TabletPhase::ProximityIn,
        TabletPhase::TipDown,
        TabletPhase::TipUp,
        TabletPhase::Button {
            button: 0x14b,
            pressed: true,
        },
        TabletPhase::ProximityOut,
    ] {
        compositor.input(pen(phase)).unwrap();
    }
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.buttons.len() >= 3
    });
    assert!(client.seen.pointer_inside);
    assert_eq!(
        client.seen.buttons,
        [(BTN_LEFT, true), (BTN_LEFT, false), (0x111, true)],
        "the tip is the primary button, the lower barrel button the secondary"
    );
}

#[test]
fn a_tablet_pad_follows_the_keyboard_focus_to_a_client_that_takes_tablets() {
    use smithay::wayland::tablet_manager::{
        TabletDescriptor, TabletPadDescriptor, TabletPadGroupDescriptor,
    };

    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "painter", (200, 100), [255, 0, 0, 255]);
    let seat = client.seen.seat.clone().unwrap();
    client
        .seen
        .tablet_manager
        .as_ref()
        .expect("zwp_tablet_manager_v2")
        .get_tablet_seat(&seat, &client.queue.handle(), ());
    pump(&mut compositor, &mut [&mut client], |_, _| true);

    let pad = |action| {
        InputEvent::TabletPad(TabletPadEvent {
            pad: TabletPadDescriptor {
                name: "Pen tablet Pad".into(),
                syspath: None,
                buttons: 4,
                groups: vec![TabletPadGroupDescriptor {
                    buttons: vec![0, 1, 2, 3],
                    rings: vec![0],
                    strips: Vec::new(),
                    modes: 2,
                }],
            },
            tablet: TabletDescriptor {
                name: "Pen tablet".into(),
                usb_id: Some((0x056a, 0x0374)),
                syspath: None,
            },
            action,
        })
    };
    for action in [
        PadAction::Added,
        PadAction::Mode { group: 0, mode: 1 },
        PadAction::Button {
            button: 2,
            pressed: true,
        },
        PadAction::Ring {
            ring: 0,
            degrees: Some(90.0),
        },
        PadAction::Ring {
            ring: 0,
            degrees: None,
        },
    ] {
        compositor.input(pad(action)).unwrap();
    }
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.pad.last().map(String::as_str) == Some("ring stop")
    });
    assert_eq!(
        client.seen.pad,
        [
            "added",
            "group",
            "buttons 4",
            "done",
            "enter",
            "mode 0",
            "mode 1",
            "button 2 pressed",
            "ring 90",
            "ring stop",
        ],
        "the pad is announced, enters the focused window and its events follow"
    );
    assert_eq!(
        client.seen.pad_surface, client.surface,
        "the pad entered the window with the keyboard focus"
    );

    compositor.input(pad(PadAction::Removed)).unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.pad.last().map(String::as_str) == Some("removed")
    });
}

#[test]
fn a_surface_below_the_windows_shows_and_takes_the_pointer_only_around_them() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Box::new(
        Square::new((0, 0, 640, 480), [0, 0, 255, 255], Rc::clone(&log)).below(),
    ));
    let mut client = mapped_client(&mut compositor, "over", (200, 100), [255, 0, 0, 255]);

    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(320, 240),
        Some([255, 0, 0, 255]),
        "the window on top"
    );
    assert_eq!(
        frame.pixel(10, 10),
        Some([0, 0, 255, 255]),
        "the desktop around it"
    );

    compositor
        .input(InputEvent::PointerMotion { x: 320.0, y: 240.0 })
        .unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.pointer_inside
    });
    assert!(
        log.borrow().motions.is_empty(),
        "over a window the window has it"
    );

    compositor
        .input(InputEvent::PointerMotion { x: 10.0, y: 10.0 })
        .unwrap();
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        !clients[0].seen.pointer_inside
    });
    assert_eq!(log.borrow().motions, vec![(10.0, 10.0)]);
    assert_eq!(log.borrow().buttons, vec![(BTN_LEFT, true)]);
    assert!(client.seen.buttons.is_empty());
}

/// A floating stack whose focus and closes a test sets, as a desktop's own
/// controls would.
#[derive(Default)]
struct Steered {
    focus: Rc<RefCell<Option<u64>>>,
    closes: Rc<RefCell<Vec<u64>>>,
}

impl WindowPolicy for Steered {
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
        StackPolicy.place(window, size, outputs)
    }

    fn focused(&mut self) -> Option<u64> {
        *self.focus.borrow()
    }

    fn take_close_requests(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.closes.borrow_mut())
    }
}

#[test]
fn the_policy_moves_focus_and_closes_windows_through_its_own_controls() {
    let policy = Steered::default();
    let (focus, closes) = (Rc::clone(&policy.focus), Rc::clone(&policy.closes));
    let mut compositor = headless(Box::new(policy));
    let mut first = mapped_client(&mut compositor, "first", (100, 100), [255, 0, 0, 255]);
    let mut second = mapped_client(&mut compositor, "second", (100, 100), [0, 255, 0, 255]);
    let id = |compositor: &Compositor, app: &str| {
        compositor
            .windows()
            .into_iter()
            .find(|(info, _)| info.app_id.as_deref() == Some(app))
            .unwrap()
            .0
            .id
    };
    assert_eq!(
        compositor.focused_window().and_then(|info| info.app_id),
        Some("second".into())
    );

    *focus.borrow_mut() = Some(id(&compositor, "first"));
    compositor.render().unwrap();
    assert_eq!(
        compositor.focused_window().and_then(|info| info.app_id),
        Some("first".into())
    );

    closes.borrow_mut().push(id(&compositor, "second"));
    compositor.render().unwrap();
    pump(
        &mut compositor,
        &mut [&mut first, &mut second],
        |_, clients| clients[1].seen.closed,
    );
    assert!(!first.seen.closed);
}

/// How bright a pixel is to the eye, roughly.
fn luma(pixel: [u8; 4]) -> u32 {
    u32::from(pixel[0]) * 3 + u32::from(pixel[1]) * 6 + u32::from(pixel[2])
}

#[test]
fn a_window_is_drawn_with_rounded_corners_and_a_shadow_below_it() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let pixel = |x, y| frame.pixel(x, y).expect("on the output");
    let red = [255, 0, 0, 255];
    // The window stands at (220, 190) to (420, 290) and the window tile
    // rounds its corners by 14: the corner pixels are not the window's.
    assert_eq!(pixel(320, 240), red, "the middle is the window");
    assert_eq!(pixel(232, 200), red, "just inside the corner");
    assert_ne!(pixel(221, 191), red, "the corner itself is cut");
    assert_eq!(pixel(420 - 12, 290 - 20), red, "inside the far corner");
    // Its shadow falls below it, and the hairline that marks its edge runs
    // just outside it.
    assert!(
        luma(pixel(320, 292)) < luma(BACKGROUND),
        "the shadow darkens the ground below: {:?}",
        pixel(320, 292)
    );
    assert!(
        luma(pixel(219, 240)) > luma(BACKGROUND),
        "the edge is a light hairline: {:?}",
        pixel(219, 240)
    );
    assert!(
        luma(pixel(320, 380)) == luma(BACKGROUND),
        "the shadow does not reach the far side of the output"
    );
}

#[test]
fn a_window_keeps_its_corners_and_shadow_when_its_effect_scales_it() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, _) = compositor.windows()[0].clone();
    // Twice the size about its centre: (120, 140) to (520, 340).
    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            scale: 2.0,
            ..WindowEffect::default()
        }),
    );
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let pixel = |x, y| frame.pixel(x, y).expect("on the output");
    let red = [255, 0, 0, 255];
    assert_eq!(pixel(320, 240), red, "the middle is the window");
    assert_ne!(
        pixel(126, 146),
        red,
        "the corner grows with the window: it is cut 28 px in, not 14"
    );
    assert_eq!(pixel(146, 166), red, "past the cut it is the window");
    assert!(
        luma(pixel(320, 344)) < luma(BACKGROUND),
        "the shadow is twice as far below the grown window: {:?}",
        pixel(320, 344)
    );
}

#[test]
fn a_window_framed_square_is_drawn_as_its_buffer_is() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, _) = compositor.windows()[0].clone();
    // A window that fills the display stands on it: no corners, no shadow.
    compositor.set_window_frame(WindowFrame::square());
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let pixel = |x, y| frame.pixel(x, y).expect("on the output");
    assert_eq!(pixel(221, 191), [255, 0, 0, 255], "the corner is square");
    assert_eq!(pixel(220, 190), [255, 0, 0, 255], "its first pixel");
    assert!(
        luma(pixel(320, 292)) == luma(BACKGROUND),
        "and nothing is cast below it"
    );

    // A window can say so itself, leaving the rest of the desktop framed.
    compositor.set_window_frame(WindowFrame::default());
    compositor.set_window_frame_for(info.id, Some(WindowFrame::square()));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(221, 191),
        Some([255, 0, 0, 255]),
        "square again"
    );
    compositor.set_window_frame_for(info.id, None);
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        frame.pixel(221, 191),
        Some([255, 0, 0, 255]),
        "and round again"
    );
}

#[test]
fn the_desktop_moves_scales_and_fades_a_window_without_moving_it() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, rect) = compositor.windows()[0].clone();
    assert_eq!(rect, (220, 190, 200, 100));
    let red = Some([255, 0, 0, 255]);
    let shown = |compositor: &mut Compositor, x, y| {
        compositor.render().unwrap();
        compositor.screenshot("HEADLESS-1").unwrap().pixel(x, y)
    };

    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            offset: (-200.0, 0.0),
            ..WindowEffect::default()
        }),
    );
    assert_eq!(shown(&mut compositor, 30, 200), red, "drawn moved left");
    assert_eq!(shown(&mut compositor, 400, 200), Some(BACKGROUND));
    assert_eq!(
        compositor.windows()[0].1,
        rect,
        "it still stands where it was"
    );

    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            scale: 0.5,
            ..WindowEffect::default()
        }),
    );
    assert_eq!(shown(&mut compositor, 320, 240), red, "the centre stays");
    assert_eq!(
        shown(&mut compositor, 225, 195),
        Some(BACKGROUND),
        "corners shrink in"
    );
    assert_eq!(shown(&mut compositor, 275, 220), red);

    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            opacity: 0.5,
            ..WindowEffect::default()
        }),
    );
    let faded = shown(&mut compositor, 320, 240).unwrap();
    assert!(
        faded[0] > 120 && faded[0] < 160,
        "half red over the background: {faded:?}"
    );

    compositor.set_window_effect(info.id, None);
    assert_eq!(shown(&mut compositor, 320, 240), red);
}

#[test]
fn a_window_that_closed_is_still_drawn_until_the_desktop_lets_it_go() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, rect) = compositor.windows()[0].clone();
    assert_eq!(rect, (220, 190, 200, 100));
    let shown = |compositor: &mut Compositor, x, y| {
        compositor.render().unwrap();
        compositor.screenshot("HEADLESS-1").unwrap().pixel(x, y)
    };
    assert_eq!(shown(&mut compositor, 320, 240), Some([255, 0, 0, 255]));

    // The frame kept is the one the window drew last.
    client.show(200, 100, [0, 255, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert_eq!(shown(&mut compositor, 320, 240), Some([0, 255, 0, 255]));

    client.toplevel.as_ref().unwrap().destroy();
    client.surface.as_ref().unwrap().destroy();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows().is_empty()
    });

    // The window left the desktop's windows, and its last frame stays where
    // it stood, kept as an element the desktop animates away.
    let closing = compositor.closing_windows();
    assert_eq!(closing.len(), 1, "one snapshot: {closing:?}");
    assert_eq!(closing[0].0.id, info.id);
    assert_eq!(closing[0].1, rect, "where it stood");
    assert_eq!(
        shown(&mut compositor, 320, 240),
        Some([0, 255, 0, 255]),
        "its last buffer is still drawn"
    );
    assert_ne!(
        shown(&mut compositor, 221, 191).unwrap(),
        [0, 255, 0, 255],
        "and still framed"
    );

    // The close runs like any other window effect.
    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            scale: 0.5,
            ..WindowEffect::default()
        }),
    );
    assert_eq!(
        shown(&mut compositor, 320, 240),
        Some([0, 255, 0, 255]),
        "the centre stays"
    );
    assert_ne!(
        shown(&mut compositor, 221, 191).unwrap(),
        [0, 255, 0, 255],
        "it shrinks in"
    );
    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            opacity: 0.0,
            ..WindowEffect::default()
        }),
    );
    assert_eq!(
        shown(&mut compositor, 320, 240),
        Some(BACKGROUND),
        "faded out, it is gone"
    );

    // The desktop is done with it.
    compositor.release_snapshot(info.id);
    assert!(compositor.closing_windows().is_empty());
    assert_eq!(shown(&mut compositor, 320, 240), Some(BACKGROUND));
    assert_eq!(shown(&mut compositor, 220, 190), Some(BACKGROUND));
}

#[test]
fn a_closed_window_is_drawn_where_and_as_big_as_it_stood() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).unwrap();
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 1280, 960).with_scale(2.0))
        .unwrap();
    let mut client = mapped_client(&mut compositor, "blue", (200, 100), [0, 0, 255, 255]);
    let (info, _) = compositor.windows()[0].clone();
    let shown = |compositor: &mut Compositor, x, y| {
        compositor.render().unwrap();
        compositor.screenshot("HEADLESS-1").unwrap().pixel(x, y)
    };
    assert_eq!(shown(&mut compositor, 640, 480), Some([0, 0, 255, 255]));

    client.toplevel.as_ref().unwrap().destroy();
    client.surface.as_ref().unwrap().destroy();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows().is_empty()
    });
    assert_eq!(compositor.closing_windows().len(), 1);
    assert_eq!(shown(&mut compositor, 640, 480), Some([0, 0, 255, 255]));
    assert_eq!(shown(&mut compositor, 460, 400), Some([0, 0, 255, 255]));
    assert_eq!(shown(&mut compositor, 820, 560), Some([0, 0, 255, 255]));
    assert_eq!(
        shown(&mut compositor, 844, 584),
        Some(BACKGROUND),
        "it is no bigger than it stood"
    );
    compositor.release_snapshot(info.id);
    assert_eq!(shown(&mut compositor, 640, 480), Some(BACKGROUND));
}

/// A hosted surface that pictures one window.
struct Pictures(WindowPicture);

impl SurfaceHost for Pictures {
    fn layers(&self) -> Vec<HostLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn window_pictures(&self) -> Vec<WindowPicture> {
        vec![self.0]
    }
}

#[test]
fn a_hosted_surface_pictures_a_window_live_in_a_rect_of_its_own() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, _) = compositor.windows()[0].clone();
    compositor.add_host(Box::new(Pictures(WindowPicture {
        window: info.id,
        rect: (10.0, 10.0, 100.0, 100.0),
        opacity: 1.0,
        clip: None,
    })));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    // 200 x 100 fits 100 x 100 as 100 x 50, centred: rows 35..85.
    assert_eq!(frame.pixel(12, 40), Some([255, 0, 0, 255]));
    assert_eq!(frame.pixel(108, 80), Some([255, 0, 0, 255]));
    assert_eq!(frame.pixel(60, 20), Some(BACKGROUND), "above the picture");
    assert_eq!(frame.pixel(60, 95), Some(BACKGROUND), "below it");
    assert_eq!(
        frame.pixel(320, 240),
        Some([255, 0, 0, 255]),
        "the window itself"
    );

    // The picture is live: the window's next buffer shows in it.
    client.show(200, 100, [0, 255, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(60, 60), Some([0, 255, 0, 255]));
}

#[test]
fn a_picture_of_a_window_carries_its_corners_and_its_lift() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, _) = compositor.windows()[0].clone();
    compositor.add_host(Box::new(Pictures(WindowPicture {
        window: info.id,
        rect: (10.0, 10.0, 100.0, 100.0),
        opacity: 1.0,
        clip: None,
    })));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let pixel = |x, y| frame.pixel(x, y).expect("on the output");
    // Half size in a 100 x 100 rect: 100 x 50 at (10, 35), corners cut by 7.
    assert_eq!(pixel(60, 60), [255, 0, 0, 255], "the picture");
    assert_ne!(pixel(11, 36), [255, 0, 0, 255], "its corner is cut");
    assert_eq!(pixel(30, 60), [255, 0, 0, 255], "past the cut");
    assert!(
        luma(pixel(60, 88)) < luma(BACKGROUND),
        "and it casts the window's lift below: {:?}",
        pixel(60, 88)
    );

    // The picture is a view of its own: the window's effect moves the
    // window, not the picture.
    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            offset: (-200.0, 0.0),
            ..WindowEffect::default()
        }),
    );
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(60, 60),
        Some([255, 0, 0, 255]),
        "the picture stays"
    );
    assert_eq!(
        frame.pixel(320, 240),
        Some(BACKGROUND),
        "the window moved away"
    );
}

/// The pixels of a picture's rect (fits 200 x 100 into 100 x 100: 100 x 50
/// at 10, 35) and of the window itself (centred: 220, 190 to 420, 290).
const IN_PICTURE: [(u32, u32); 4] = [(15, 40), (60, 60), (105, 40), (60, 80)];
const IN_WINDOW: (u32, u32) = (320, 240);

/// A window the session keeps private is a placeholder wherever a hosted
/// surface pictures it, on the person's own display too: the desk a
/// picture stands on is what a screenshot, a recording and a share show.
#[test]
fn a_private_window_is_pictured_as_the_placeholder_its_captures_show() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(
        &mut compositor,
        "org.example.shared",
        (200, 100),
        [255, 0, 0, 255],
    );
    let (info, _) = compositor.windows()[0].clone();
    compositor.add_host(Box::new(Pictures(WindowPicture {
        window: info.id,
        rect: (10.0, 10.0, 100.0, 100.0),
        opacity: 1.0,
        clip: None,
    })));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(60, 60),
        Some([255, 0, 0, 255]),
        "nobody has marked it private yet: the picture is live"
    );

    compositor.protect_app("org.example.shared", true);
    compositor.render().unwrap();
    let display = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        display.pixel(IN_WINDOW.0, IN_WINDOW.1),
        Some([255, 0, 0, 255]),
        "the person's own display still shows the window"
    );
    let placeholder = display
        .pixel(IN_PICTURE[0].0, IN_PICTURE[0].1)
        .expect("on the output");
    assert_ne!(
        placeholder,
        [255, 0, 0, 255],
        "the picture carries none of the window"
    );
    for (x, y) in IN_PICTURE {
        assert_eq!(
            display.pixel(x, y),
            Some(placeholder),
            "the whole picture is one flat placeholder at ({x}, {y})"
        );
    }

    // A copy of the desk shows the window and its picture as that same
    // placeholder: no secret leaves with it.
    let copy = compositor
        .read_back_region("HEADLESS-1", (0, 0, 640, 480))
        .expect("a capture of the output");
    assert_eq!(copy.pixel(IN_WINDOW.0, IN_WINDOW.1), Some(placeholder));
    for (x, y) in IN_PICTURE {
        assert_eq!(
            copy.pixel(x, y),
            Some(placeholder),
            "the picture at ({x}, {y})"
        );
    }
    let red = |pixel: [u8; 4]| pixel[0] > 128 && pixel[1] < 64 && pixel[2] < 64;
    for y in 0..copy.height {
        for x in 0..copy.width {
            assert!(
                !red(copy.pixel(x, y).expect("on the capture")),
                "no secret of the window at ({x}, {y})"
            );
        }
    }

    // Shown again once the session no longer marks it private.
    compositor.protect_app("org.example.shared", false);
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(60, 60), Some([255, 0, 0, 255]), "pictured live");
}

/// An app known to hold secrets is private without asking, and its picture
/// is the placeholder its captures are.
#[test]
fn a_picture_of_an_app_that_holds_secrets_is_its_placeholder() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(
        &mut compositor,
        "so.tilo.passwords",
        (200, 100),
        [255, 0, 0, 255],
    );
    let (info, _) = compositor.windows()[0].clone();
    compositor.add_host(Box::new(Pictures(WindowPicture {
        window: info.id,
        rect: (10.0, 10.0, 100.0, 100.0),
        opacity: 1.0,
        clip: None,
    })));
    compositor.render().unwrap();
    let display = compositor.screenshot("HEADLESS-1").unwrap();
    let placeholder = display
        .pixel(IN_PICTURE[0].0, IN_PICTURE[0].1)
        .expect("on the output");
    assert_ne!(placeholder, [255, 0, 0, 255]);
    for (x, y) in IN_PICTURE {
        assert_eq!(display.pixel(x, y), Some(placeholder), "at ({x}, {y})");
    }
    let copy = compositor
        .read_back_region("HEADLESS-1", (0, 0, 640, 480))
        .expect("a capture of the output");
    for (x, y) in IN_PICTURE {
        assert_eq!(
            copy.pixel(x, y),
            Some(placeholder),
            "captured at ({x}, {y})"
        );
    }
    assert_eq!(copy.pixel(IN_WINDOW.0, IN_WINDOW.1), Some(placeholder));
}

/// A private window that closed still stands on the desk while the desktop
/// animates it away: a copy of the desk shows its placeholder until then,
/// never the frame it drew last.
#[test]
fn a_private_window_that_closed_is_a_placeholder_in_copies() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(
        &mut compositor,
        "org.example.shared",
        (200, 100),
        [255, 0, 0, 255],
    );
    compositor.protect_app("org.example.shared", true);
    // The frame it drew last is kept for its close.
    compositor.render().unwrap();
    client.toplevel.as_ref().unwrap().destroy();
    client.surface.as_ref().unwrap().destroy();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows().is_empty()
    });
    assert_eq!(
        compositor.closing_windows().len(),
        1,
        "its last frame is kept"
    );
    compositor.render().unwrap();
    let display = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        display.pixel(IN_WINDOW.0, IN_WINDOW.1),
        Some([255, 0, 0, 255]),
        "the person's own display shows it closing"
    );
    let copy = compositor
        .read_back_region("HEADLESS-1", (0, 0, 640, 480))
        .expect("a capture of the output");
    let placeholder = copy
        .pixel(IN_WINDOW.0, IN_WINDOW.1)
        .expect("on the capture");
    assert_ne!(
        placeholder,
        [255, 0, 0, 255],
        "none of the frame it drew last"
    );
    assert_ne!(
        placeholder, BACKGROUND,
        "not what it stands on either: a capture says something is withheld"
    );

    // Once the desktop lets it go, nothing of it is on the desk at all.
    let info = compositor.closing_windows()[0].0.id;
    compositor.release_snapshot(info);
    compositor.render().unwrap();
    let copy = compositor
        .read_back_region("HEADLESS-1", (0, 0, 640, 480))
        .expect("a capture of the output");
    assert_eq!(copy.pixel(IN_WINDOW.0, IN_WINDOW.1), Some(BACKGROUND));
}

/// A hosted surface of translucent material over (200, 180, 80, 40),
/// keeping the backdrops it is handed.
struct Frosted(Rc<RefCell<Vec<Option<BackdropImage>>>>);

impl SurfaceHost for Frosted {
    fn layers(&self) -> Vec<HostLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn backdrops(&self) -> Vec<BackdropRequest> {
        vec![BackdropRequest {
            rect: (200.0, 180.0, 80.0, 40.0),
            texels: (40, 20),
            blur: 3.0,
        }]
    }

    fn set_backdrops(&mut self, images: Vec<Option<BackdropImage>>) {
        self.0.borrow_mut().extend(images);
    }
}

#[test]
fn translucent_chrome_is_handed_a_blur_of_what_lies_below_it_when_that_changes() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    // The window covers 220..420 x 190..290: the region's right and lower
    // part is red, the rest background.
    let handed = Rc::new(RefCell::new(Vec::new()));
    compositor.add_host(Box::new(Frosted(Rc::clone(&handed))));
    compositor.render().unwrap();
    let first = handed.borrow_mut().pop().flatten().expect("a backdrop");
    assert_eq!(first.texels, (40, 20));
    assert_eq!(first.rgba.len(), 40 * 20 * 4);
    let at = |image: &BackdropImage, x: usize, y: usize| {
        let i = (y * 40 + x) * 4;
        [
            image.rgba[i],
            image.rgba[i + 1],
            image.rgba[i + 2],
            image.rgba[i + 3],
        ]
    };
    let window_texel = at(&first, 35, 15);
    let background_corner = at(&first, 0, 0);
    assert!(
        window_texel[0] > 200 && window_texel[1] < 60,
        "{window_texel:?}"
    );
    assert!(background_corner[0] < 60, "{background_corner:?}");
    let edge = at(&first, 10, 15);
    assert!(
        edge[0] > background_corner[0] + 20 && edge[0] < window_texel[0] - 20,
        "the window's edge is blurred: {edge:?}"
    );

    compositor.render().unwrap();
    assert!(
        handed.borrow().iter().all(Option::is_none),
        "nothing below changed, nothing is drawn again"
    );
    handed.borrow_mut().clear();

    client.show(200, 100, [0, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    compositor.render().unwrap();
    let again = handed.borrow_mut().pop().flatten().expect("a new backdrop");
    assert!(at(&again, 35, 15)[2] > 200, "now blue");
}

#[test]
fn a_backdrop_follows_the_effects_the_windows_are_drawn_with() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let handed = Rc::new(RefCell::new(Vec::new()));
    compositor.add_host(Box::new(Frosted(Rc::clone(&handed))));
    compositor.render().unwrap();
    handed.borrow_mut().clear();

    // The desktop slides the window 200 px left: it now covers the left of
    // the hosted surface's region, where nothing stood before.
    let (info, _) = compositor.windows()[0].clone();
    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            offset: (-200.0, 0.0),
            ..WindowEffect::default()
        }),
    );
    compositor.render().unwrap();
    let moved = handed
        .borrow_mut()
        .pop()
        .flatten()
        .expect("a backdrop drawn again");
    let at = |x: usize, y: usize| {
        let i = (y * 40 + x) * 4;
        [moved.rgba[i], moved.rgba[i + 1], moved.rgba[i + 2]]
    };
    let inside = at(2, 10);
    let outside = at(35, 10);
    assert!(
        inside[0] > 200 && inside[1] < 60,
        "the moved window lies below the left of it: {inside:?}"
    );
    assert!(outside[0] < 60, "and it left the right of it: {outside:?}");
}

#[test]
fn a_window_that_asks_to_be_maximized_covers_its_output_and_comes_back() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "max", (200, 100), [0, 255, 0, 255]);
    assert_eq!(compositor.windows()[0].1, (220, 190, 200, 100));
    client.toplevel.as_ref().expect("toplevel").set_maximized();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows()[0].1.0 == 0
    });
    assert_eq!(compositor.windows()[0].1.1, 0, "at the output's corner");
    client
        .toplevel
        .as_ref()
        .expect("toplevel")
        .unset_maximized();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows()[0].1.0 == 220
    });
}

#[test]
fn a_policy_that_declines_fullscreen_keeps_the_window_where_it_is() {
    struct Strips;
    impl WindowPolicy for Strips {
        fn initial_size(
            &mut self,
            _window: &WindowInfo,
            _outputs: &[Rectangle<i32, Logical>],
        ) -> Option<(i32, i32)> {
            None
        }
        fn place(
            &mut self,
            _window: &WindowInfo,
            _size: (i32, i32),
            _outputs: &[Rectangle<i32, Logical>],
        ) -> Point<i32, Logical> {
            Point::from((10, 10))
        }
        fn request(
            &mut self,
            _window: &WindowInfo,
            _request: &tilo_compositor::WindowRequest,
        ) -> bool {
            false
        }
    }
    let mut compositor = headless(Box::new(Strips));
    let mut client = mapped_client(&mut compositor, "full", (200, 100), [0, 255, 0, 255]);
    client
        .toplevel
        .as_ref()
        .expect("toplevel")
        .set_fullscreen(None);
    for _ in 0..20 {
        client.exchange();
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
    }
    assert_eq!(compositor.windows()[0].1, (10, 10, 200, 100));
}

#[test]
fn a_hosted_apps_copy_is_offered_under_every_text_name_and_read_back() {
    use std::io::Read as _;
    let mut compositor = headless(Box::new(StackPolicy));
    compositor
        .set_selection(
            tilo_compositor::Selection::Clipboard,
            vec![("text/plain".into(), b"copied".to_vec())],
        )
        .expect("offer");
    let formats = compositor.selection_formats(tilo_compositor::Selection::Clipboard);
    assert_eq!(
        formats.first().map(String::as_str),
        Some("text/plain;charset=utf-8")
    );
    assert!(formats.iter().any(|mime| mime == "UTF8_STRING"));
    let mut reader = compositor
        .receive_selection(tilo_compositor::Selection::Clipboard, "UTF8_STRING")
        .expect("read");
    let mut text = String::new();
    reader.read_to_string(&mut text).expect("bytes");
    assert_eq!(text, "copied");
    assert!(
        compositor
            .receive_selection(tilo_compositor::Selection::Primary, "text/plain")
            .is_err(),
        "nothing owns the primary selection"
    );
}

/// Each output and the monitor on it.
type Told = Vec<(Rectangle<i32, Logical>, String)>;

/// A policy that keeps the monitors it is told of.
struct Monitors(Rc<RefCell<Told>>);

impl WindowPolicy for Monitors {
    fn initial_size(
        &mut self,
        _window: &WindowInfo,
        _outputs: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        None
    }

    fn place(
        &mut self,
        _window: &WindowInfo,
        _size: (i32, i32),
        _outputs: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        Point::from((0, 0))
    }

    fn monitors(&mut self, monitors: &[(Rectangle<i32, Logical>, String)], _primary: Option<&str>) {
        *self.0.borrow_mut() = monitors.to_vec();
    }
}

#[test]
fn a_text_field_waiting_for_typing_is_told_apart_from_the_way_it_is_typed() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "form", (200, 100), [0, 255, 0, 255]);
    // The window takes the keyboard with a click on it.
    click(&mut compositor, 320.0, 240.0);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });
    let entry = compositor.text_entry();
    assert!(!entry.wants_typing, "nothing is being typed into yet");
    assert!(!entry.on_the_glass, "no touch has happened at all");

    // A hardware key first, then a touch on the screen: the glass is what
    // they type on.
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .unwrap();
    compositor
        .input(InputEvent::TouchDown {
            slot: 0,
            x: 320.0,
            y: 240.0,
        })
        .unwrap();
    assert!(compositor.text_entry().on_the_glass);
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .unwrap();
    assert!(
        !compositor.text_entry().on_the_glass,
        "a key hands typing back to the keyboard they hold"
    );

    // The on-screen keyboard is up, as a client the session trusts, and
    // the window holds a field that asks for typing.
    let mut keyboard = TestClient::connect_trusted(&mut compositor);
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |_, clients| {
            clients[1].seen.input_method_manager.is_some() && clients[1].seen.seat.is_some()
        },
    );
    keyboard.own_input_method();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |_, clients| clients[1].seen.input_method.is_some(),
    );
    client.own_text_field();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, clients| {
            compositor.text_entry().wants_typing
                && clients[0].seen.field_entered
                && clients[1].seen.ime_activated
        },
    );
    assert!(
        !compositor.text_entry().on_the_glass,
        "they are typing on the keyboard they hold"
    );

    // What the keyboard types lands in the field.
    keyboard.type_with_input_method("hello");
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |_, clients| !clients[0].seen.committed.is_empty(),
    );
    assert_eq!(client.seen.committed, vec!["hello".to_owned()]);

    // Reaching for the screen brings the keyboard over it back.
    compositor
        .input(InputEvent::TouchDown {
            slot: 0,
            x: 320.0,
            y: 240.0,
        })
        .unwrap();
    assert!(compositor.text_entry().on_the_glass);

    // And it stops asking when it lets go of the focus.
    client.release_text_field();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, _| !compositor.text_entry().wants_typing,
    );
}

/// A focused window that has no text field yet, and the on-screen keyboard
/// serving the seat.
fn window_and_keyboard() -> (Compositor, TestClient, TestClient) {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "form", (200, 100), [0, 255, 0, 255]);
    // The window takes the keyboard with a click on it.
    click(&mut compositor, 320.0, 240.0);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });
    let mut keyboard = TestClient::connect_trusted(&mut compositor);
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |_, clients| {
            clients[1].seen.input_method_manager.is_some() && clients[1].seen.seat.is_some()
        },
    );
    keyboard.own_input_method();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |_, clients| clients[1].seen.input_method.is_some(),
    );
    (compositor, client, keyboard)
}

/// Let both sides catch up on what was just sent.
fn settle(compositor: &mut Compositor, client: &mut TestClient, keyboard: &mut TestClient) {
    pump(compositor, &mut [client, keyboard], |_, _| true);
}

/// Wait until the compositor has taken the field's last commit: it tells the
/// input method so.
fn committed(
    compositor: &mut Compositor,
    client: &mut TestClient,
    keyboard: &mut TestClient,
    since: u32,
) {
    pump(compositor, &mut [client, keyboard], move |_, clients| {
        clients[1].seen.ime_done > since
    });
}

#[test]
fn a_field_that_commits_again_is_no_longer_the_one_dictation_bound() {
    let (mut compositor, mut client, mut keyboard) = window_and_keyboard();
    assert_eq!(
        compositor.dictation_field(),
        Err(TextFieldRefusal::NoField),
        "no field has the focus"
    );

    client.own_text_field();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, clients| {
            clients[0].seen.field_entered
                && clients[1].seen.ime_activated
                && compositor.dictation_field().is_ok()
        },
    );
    let field = compositor
        .dictation_field()
        .expect("a field to dictate into");
    assert!(!field.multiline, "an ordinary text box takes one line");

    // The caret moves: still the same field, but another revision of what
    // its client committed.
    let done = keyboard.seen.ime_done;
    client.move_caret(3);
    committed(&mut compositor, &mut client, &mut keyboard, done);
    assert!(
        compositor
            .dictation_field()
            .is_ok_and(|now| now.revision != field.revision),
        "a commit moves the field's revision"
    );
    assert_eq!(
        compositor.insert_dictation(field, "spoken words"),
        TextInsertion::Changed
    );
    settle(&mut compositor, &mut client, &mut keyboard);
    assert!(
        client.seen.committed.is_empty(),
        "a field that moved takes nothing"
    );

    // Another field of the same window enables the object again: the same
    // object on the same surface, so the same identity, and a new revision.
    let field = compositor
        .dictation_field()
        .expect("a field to dictate into");
    client.focus_another_field(ContentHint::empty(), ContentPurpose::Normal);
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, _| compositor.dictation_field().is_ok_and(|now| now != field),
    );
    assert_eq!(
        compositor.dictation_field().map(|now| now.id),
        Ok(field.id),
        "one text-input object on one surface names one field"
    );
    assert_eq!(
        compositor.insert_dictation(field, "spoken words"),
        TextInsertion::Changed
    );

    settle(&mut compositor, &mut client, &mut keyboard);
    assert!(
        client.seen.committed.is_empty(),
        "neither field took anything dictation bound before it"
    );
}

#[test]
fn a_field_that_stops_asking_for_typing_takes_no_dictated_text() {
    let (mut compositor, mut client, mut keyboard) = window_and_keyboard();
    client.own_text_field();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, clients| {
            clients[0].seen.field_entered
                && clients[1].seen.ime_activated
                && compositor.dictation_field().is_ok()
        },
    );
    let field = compositor
        .dictation_field()
        .expect("a field to dictate into");

    client.release_text_field();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, _| compositor.dictation_field().is_err(),
    );
    assert_eq!(
        compositor.dictation_field(),
        Err(TextFieldRefusal::NoField),
        "the field stopped asking for typing"
    );
    assert_eq!(
        compositor.insert_dictation(field, "spoken words"),
        TextInsertion::Refused(TextFieldRefusal::NoField)
    );

    settle(&mut compositor, &mut client, &mut keyboard);
    assert!(
        client.seen.committed.is_empty(),
        "a released field takes nothing"
    );
}

#[test]
fn dictation_keeps_out_of_secret_and_terminal_fields() {
    let (mut compositor, mut client, mut keyboard) = window_and_keyboard();
    client.own_text_field_as(
        ContentHint::HiddenText | ContentHint::SensitiveData,
        ContentPurpose::Password,
    );
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, clients| {
            clients[0].seen.field_entered
                && clients[1].seen.ime_activated
                && compositor.dictation_field().is_err()
        },
    );
    assert_eq!(compositor.dictation_field(), Err(TextFieldRefusal::Secret));

    // A password field takes no dictated text, and nothing reaches it.
    let no_field = TextField {
        id: 0,
        revision: 0,
        multiline: false,
    };
    assert_eq!(
        compositor.insert_dictation(no_field, "spoken words"),
        TextInsertion::Refused(TextFieldRefusal::Secret)
    );
    settle(&mut compositor, &mut client, &mut keyboard);
    assert!(
        client.seen.committed.is_empty(),
        "a password field takes nothing"
    );

    // Digits are a PIN pad: the on-screen keyboard treats them as secret
    // too.
    let done = keyboard.seen.ime_done;
    client.set_content_type(ContentHint::empty(), ContentPurpose::Digits);
    committed(&mut compositor, &mut client, &mut keyboard, done);
    assert_eq!(compositor.dictation_field(), Err(TextFieldRefusal::Secret));

    // A terminal's text is commands.
    let done = keyboard.seen.ime_done;
    client.set_content_type(ContentHint::empty(), ContentPurpose::Terminal);
    committed(&mut compositor, &mut client, &mut keyboard, done);
    assert_eq!(
        compositor.dictation_field(),
        Err(TextFieldRefusal::Unsupported)
    );
}

#[test]
fn dictation_tells_a_multiline_field_from_a_one_line_one() {
    let (mut compositor, mut client, mut keyboard) = window_and_keyboard();
    client.own_text_field_as(ContentHint::Multiline, ContentPurpose::Normal);
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, clients| {
            clients[0].seen.field_entered
                && clients[1].seen.ime_activated
                && compositor.dictation_field().is_ok()
        },
    );
    assert!(
        compositor
            .dictation_field()
            .expect("a field to dictate into")
            .multiline,
        "the field takes more than one line"
    );

    let done = keyboard.seen.ime_done;
    client.set_content_type(ContentHint::empty(), ContentPurpose::Normal);
    committed(&mut compositor, &mut client, &mut keyboard, done);
    assert!(
        !compositor
            .dictation_field()
            .expect("a field to dictate into")
            .multiline,
        "a one-line field is not multiline"
    );
}

#[test]
fn dictation_writes_once_into_the_field_it_bound() {
    let (mut compositor, mut client, mut keyboard) = window_and_keyboard();
    client.own_text_field();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, clients| {
            clients[0].seen.field_entered
                && clients[1].seen.ime_activated
                && compositor.dictation_field().is_ok()
        },
    );
    let field = compositor
        .dictation_field()
        .expect("a field to dictate into");

    let done = client.seen.field_done;
    assert_eq!(
        compositor.insert_dictation(field, "spoken words"),
        TextInsertion::Inserted
    );
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |_, clients| clients[0].seen.field_done > done,
    );
    assert_eq!(
        client.seen.committed,
        vec!["spoken words".to_owned()],
        "the field took exactly the committed text"
    );
}

/// The emoji picker goes where the person types: the caret its card opens at
/// is the one the field reports on the window it is in, and a pick reaches
/// the field even when the field keeps its content secret — it is only not
/// remembered then.
#[test]
fn a_pick_opens_at_the_caret_and_reaches_even_a_secret_field() {
    let (mut compositor, mut client, mut keyboard) = window_and_keyboard();
    assert_eq!(
        compositor.text_caret(),
        None,
        "no field takes text: there is no caret"
    );

    client.own_text_field();
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, clients| {
            clients[0].seen.field_entered
                && clients[1].seen.ime_activated
                && compositor.dictation_field().is_ok()
        },
    );
    assert_eq!(
        compositor.text_caret(),
        None,
        "a field that never reported its caret leaves the card centred"
    );

    client.move_caret(7);
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |compositor, _| compositor.text_caret().is_some(),
    );
    let (_, (x, y, ..)) = compositor.windows()[0];
    let caret = compositor.text_caret().expect("a caret");
    assert_eq!(
        (caret.x, caret.y, caret.width, caret.height),
        (f64::from(x + 7), f64::from(y), 1.0, 20.0),
        "the caret stands inside its window as the field reported it"
    );

    let done = client.seen.field_done;
    assert_eq!(
        compositor.insert_active_text("🎉"),
        Some(false),
        "an ordinary field takes the emoji and is remembered"
    );
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |_, clients| clients[0].seen.field_done > done,
    );
    assert_eq!(client.seen.committed, vec!["🎉".to_owned()]);

    // A password field takes it too, and says so: what it holds is never
    // remembered.
    let done = keyboard.seen.ime_done;
    client.focus_another_field(ContentHint::empty(), ContentPurpose::Password);
    committed(&mut compositor, &mut client, &mut keyboard, done);
    assert_eq!(compositor.dictation_field(), Err(TextFieldRefusal::Secret));
    assert_eq!(
        compositor.insert_active_text("👍"),
        Some(true),
        "a secret field takes the emoji without being remembered"
    );
    let done = client.seen.field_done;
    pump(
        &mut compositor,
        &mut [&mut client, &mut keyboard],
        |_, clients| clients[0].seen.field_done > done,
    );
    assert_eq!(
        client.seen.committed,
        vec!["🎉".to_owned(), "👍".to_owned()],
        "the secret field took the second emoji"
    );
}

#[test]
fn the_policy_knows_each_output_by_the_monitor_on_it() {
    let told = Rc::new(RefCell::new(Vec::new()));
    let mut compositor = Compositor::headless(Box::new(Monitors(Rc::clone(&told)))).unwrap();
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480).at(0, 0))
        .unwrap();
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 320, 240).at(640, 0))
        .unwrap();
    compositor.render().unwrap();
    // Without an EDID a monitor is known by its connector.
    let names: Vec<String> = told.borrow().iter().map(|(_, name)| name.clone()).collect();
    assert_eq!(names, ["HEADLESS-1", "HEADLESS-2"]);
    assert_eq!(
        told.borrow()[1].0,
        Rectangle::new((640, 0).into(), (320, 240).into())
    );
    let outputs = compositor.outputs();
    assert_eq!(outputs[1].monitor, "HEADLESS-2");
    assert_eq!(outputs[1].label, "HEADLESS-2");
}

/// Click the left button at `(x, y)`.
fn click(compositor: &mut Compositor, x: f64, y: f64) {
    compositor
        .input(InputEvent::PointerMotion { x, y })
        .unwrap();
    for pressed in [true, false] {
        compositor
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .unwrap();
    }
}

#[test]
fn a_modal_dialog_keeps_the_keyboard_from_the_window_it_belongs_to() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "editor", (400, 300), [255, 0, 0, 255]);
    let parent = client.toplevel.take().unwrap();
    let parent_surface = client.surface.take().unwrap();
    client.seen.configured = false;
    client.open_window("save");
    let dialog = client.toplevel.clone().unwrap();
    dialog.set_parent(Some(&parent));
    let qh = client.queue.handle();
    let hint = client
        .seen
        .wm_dialog
        .as_ref()
        .expect("xdg_wm_dialog_v1")
        .get_xdg_dialog(&dialog, &qh, ());
    hint.set_modal();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    client.show(100, 60, [0, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows().len() == 2
    });
    let focused =
        |compositor: &Compositor| compositor.focused_window().and_then(|info| info.app_id);
    assert_eq!(focused(&compositor).as_deref(), Some("save"));

    // A click on the window it belongs to leaves the keyboard with it.
    click(&mut compositor, 130.0, 100.0);
    assert_eq!(focused(&compositor).as_deref(), Some("save"));

    // Not modal any more, the window takes the keyboard back.
    hint.unset_modal();
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    click(&mut compositor, 130.0, 100.0);
    assert_eq!(focused(&compositor).as_deref(), Some("editor"));
    drop(parent_surface);
}

/// The accessibility settings with `change` applied to the compositor's own.
fn with_a11y(compositor: &mut Compositor, change: impl FnOnce(&mut tilo_compositor::A11y)) {
    let mut a11y = compositor.a11y().clone();
    change(&mut a11y);
    compositor.set_a11y(a11y).expect("accessibility settings");
}

/// Every pixel of `frame` that is `wanted`.
fn count(frame: &tilo_compositor::Screenshot, wanted: [u8; 4]) -> usize {
    (0..frame.height)
        .flat_map(|y| (0..frame.width).map(move |x| (x, y)))
        .filter(|&(x, y)| frame.pixel(x, y) == Some(wanted))
        .count()
}

#[test]
fn sticky_keys_hold_a_modifier_for_one_chord() {
    const KEY_LEFTSHIFT: u32 = 42;
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "sticky", (200, 100), [0, 255, 0, 255]);
    with_a11y(&mut compositor, |a11y| a11y.assists.sticky_keys = true);

    // Shift, pressed and let go, then a letter: the shift stays down for
    // the letter and comes up with it.
    for (keycode, pressed) in [
        (KEY_LEFTSHIFT, true),
        (KEY_LEFTSHIFT, false),
        (KEY_A, true),
        (KEY_A, false),
    ] {
        compositor
            .input(InputEvent::Key { keycode, pressed })
            .expect("input");
    }
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() == 4
    });
    assert_eq!(
        client.seen.keys,
        vec![
            (KEY_LEFTSHIFT, true),
            (KEY_A, true),
            (KEY_A, false),
            (KEY_LEFTSHIFT, false)
        ]
    );
}

#[test]
fn slow_keys_wait_for_a_key_to_be_held_long_enough() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "slow", (200, 100), [0, 255, 0, 255]);
    with_a11y(&mut compositor, |a11y| {
        a11y.assists.slow_keys = Some(Duration::from_millis(300));
    });

    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .expect("input");
    compositor.dispatch(Duration::ZERO).expect("dispatch");
    client.exchange();
    assert!(
        client.seen.keys.is_empty(),
        "a brush past a key types nothing"
    );

    pump(&mut compositor, &mut [&mut client], |_, clients| {
        !clients[0].seen.keys.is_empty()
    });
    assert_eq!(client.seen.keys, vec![(KEY_A, true)], "held, so it counts");

    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: false,
        })
        .expect("input");
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() == 2
    });
    assert_eq!(client.seen.keys, vec![(KEY_A, true), (KEY_A, false)]);
}

#[test]
fn switch_keys_step_the_focus_and_activate_it() {
    const KEY_F6: u32 = 64;
    const KEY_F7: u32 = 65;
    const KEY_TAB: u32 = 15;
    const KEY_RETURN: u32 = 28;
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "switch", (200, 100), [0, 255, 0, 255]);
    with_a11y(&mut compositor, |a11y| {
        a11y.assists.switch = Some(tilo_compositor::SwitchConfig {
            pair: tilo_compositor::SwitchPair::F6F7,
            period: None,
        });
    });

    for keycode in [KEY_F6, KEY_F7] {
        for pressed in [true, false] {
            compositor
                .input(InputEvent::Key { keycode, pressed })
                .expect("input");
        }
    }
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() == 4
    });
    assert_eq!(
        client.seen.keys,
        vec![
            (KEY_TAB, true),
            (KEY_TAB, false),
            (KEY_RETURN, true),
            (KEY_RETURN, false)
        ],
        "the switches become the keys that move on and activate"
    );
}

#[test]
fn the_crosshair_guides_follow_the_pointer() {
    let mut compositor = headless(Box::new(StackPolicy));
    compositor
        .input(InputEvent::PointerMotion { x: 320.0, y: 240.0 })
        .expect("input");
    compositor.render().expect("render");
    let plain = compositor.screenshot("HEADLESS-1").expect("screenshot");
    assert_eq!(plain.pixel(10, 240), Some(BACKGROUND));

    with_a11y(&mut compositor, |a11y| a11y.cursor.crosshair = true);
    compositor.render().expect("render");
    let guided = compositor.screenshot("HEADLESS-1").expect("screenshot");
    assert_ne!(
        guided.pixel(10, 240),
        Some(BACKGROUND),
        "a guide runs across at the pointer's height"
    );
    assert_ne!(
        guided.pixel(320, 10),
        Some(BACKGROUND),
        "and down at its width"
    );
    assert_eq!(
        guided.pixel(10, 232),
        Some(BACKGROUND),
        "the guides are thin"
    );

    with_a11y(&mut compositor, |a11y| a11y.cursor.crosshair = false);
    compositor.render().expect("render");
    let plain_again = compositor.screenshot("HEADLESS-1").expect("screenshot");
    assert_eq!(plain_again.pixel(10, 240), Some(BACKGROUND));
}

#[test]
fn the_visual_alert_flashes_the_screen_and_fades() {
    let mut compositor = headless(Box::new(StackPolicy));
    compositor.render().expect("render");
    let before = compositor.screenshot("HEADLESS-1").expect("screenshot");
    assert_eq!(before.pixel(10, 10), Some(BACKGROUND));

    compositor.flash();
    compositor.render().expect("render");
    let lit = compositor.screenshot("HEADLESS-1").expect("screenshot");
    let lit = lit.pixel(10, 10).expect("pixel");
    assert!(
        lit[0] > 200 && lit[1] > 200 && lit[2] > 200,
        "the alert is on screen: {lit:?}"
    );

    // It fades out as time passes: once its run is over the frame is the
    // desktop again.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut faded = false;
    while std::time::Instant::now() < deadline {
        compositor
            .dispatch(Duration::from_millis(16))
            .expect("dispatch");
        compositor.render().expect("render");
        let frame = compositor.screenshot("HEADLESS-1").expect("screenshot");
        if frame.pixel(10, 10) == Some(BACKGROUND) {
            faded = true;
            break;
        }
    }
    assert!(faded, "and it fades out");
}

#[test]
fn a_frame_after_the_alerts_run_puts_the_visual_alert_away() {
    let mut compositor = headless(Box::new(StackPolicy));
    compositor.render().expect("render");
    compositor.flash();
    compositor.render().expect("render");
    let lit = compositor.screenshot("HEADLESS-1").expect("screenshot");
    let lit = lit.pixel(10, 10).expect("pixel");
    assert!(lit[0] > 200, "the alert is on screen: {lit:?}");
    // No frame is drawn while the alert runs: the next one comes after it,
    // as a busy machine's would.
    std::thread::sleep(Duration::from_millis(500));
    compositor.render().expect("render");
    let frame = compositor.screenshot("HEADLESS-1").expect("screenshot");
    assert_eq!(
        frame.pixel(10, 10),
        Some(BACKGROUND),
        "the alert is not left on screen"
    );
}

#[test]
fn the_lens_magnifies_the_scene_around_the_pointer() {
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "lens", (300, 200), GREEN);
    compositor
        .input(InputEvent::PointerMotion { x: 320.0, y: 240.0 })
        .expect("input");
    compositor.render().expect("render");
    let plain = compositor.screenshot("HEADLESS-1").expect("screenshot");
    let plain_green = count(&plain, GREEN);
    assert!(plain_green > 0, "the window is drawn");

    with_a11y(&mut compositor, |a11y| {
        a11y.magnifier = tilo_compositor::Magnifier {
            zoom: 2,
            view: tilo_compositor::MagnifierView::Lens,
        };
    });
    compositor.render().expect("render");
    let lensed = compositor.screenshot("HEADLESS-1").expect("screenshot");
    let lensed_green = count(&lensed, GREEN);
    assert!(
        lensed_green > plain_green * 3 / 2,
        "the lens shows the window twice as big: {plain_green} then {lensed_green}"
    );

    // Full screen magnifies the whole scene instead.
    with_a11y(&mut compositor, |a11y| {
        a11y.magnifier.view = tilo_compositor::MagnifierView::FullScreen;
    });
    compositor.render().expect("render");
    let full = compositor.screenshot("HEADLESS-1").expect("screenshot");
    let full_green = count(&full, GREEN);
    assert!(
        full_green > lensed_green,
        "the whole screen is magnified, past the lens's edges: {lensed_green} then {full_green}"
    );
}

fn press_button(compositor: &mut Compositor, button: u32) {
    for pressed in [true, false] {
        compositor
            .input(InputEvent::PointerButton { button, pressed })
            .expect("input");
    }
}

/// A press and its release of `keys`, in order, as fingers type a chord.
fn press_keys(compositor: &mut Compositor, keys: &[u32]) {
    for &keycode in keys {
        compositor
            .input(InputEvent::Key {
                keycode,
                pressed: true,
            })
            .expect("input");
    }
    for &keycode in keys.iter().rev() {
        compositor
            .input(InputEvent::Key {
                keycode,
                pressed: false,
            })
            .expect("input");
    }
}

#[test]
fn a_locked_pointer_only_moves_its_client_by_deltas_and_follows_focus() {
    let mut compositor = headless(Box::<Columns>::default());
    let mut game = mapped_client(&mut compositor, "game", (250, 200), [255, 0, 0, 255]);
    let mut other = mapped_client(&mut compositor, "other", (250, 200), [0, 0, 255, 255]);
    game.lock_pointer();
    game.own_relative_pointer();
    pump(&mut compositor, &mut [&mut game, &mut other], |_, _| true);
    assert!(
        !game.seen.pointer_locked,
        "the window mapped last has the keyboard, not this one"
    );

    // Under the pointer is not enough: the lock takes hold with the keyboard
    // focus, and only then.
    compositor
        .input(InputEvent::PointerMotion { x: 100.0, y: 100.0 })
        .expect("input");
    pump(&mut compositor, &mut [&mut game, &mut other], |_, _| true);
    assert!(!game.seen.pointer_locked, "under the pointer, not focused");
    press_button(&mut compositor, BTN_LEFT);
    pump(
        &mut compositor,
        &mut [&mut game, &mut other],
        |_, clients| clients[0].seen.pointer_locked,
    );

    // Absolute motion is dropped; the deltas still reach the game.
    compositor
        .input(InputEvent::PointerMotion { x: 400.0, y: 100.0 })
        .expect("input");
    compositor
        .input(InputEvent::PointerMotionRelative { dx: 6.0, dy: -4.0 })
        .expect("input");
    pump(
        &mut compositor,
        &mut [&mut game, &mut other],
        |_, clients| !clients[0].seen.relative.is_empty(),
    );
    assert_eq!(compositor.pointer_location(), (100.0, 100.0));
    assert_eq!(game.seen.relative, vec![(6.0, -4.0)]);
    assert!(
        game.seen.pointer_positions.is_empty(),
        "a locked pointer sends no absolute motion: {:?}",
        game.seen.pointer_positions
    );

    // Losing the keyboard releases it: the person is looking at a new window.
    let mut newest = mapped_client(&mut compositor, "newest", (40, 40), [0, 255, 0, 255]);
    pump(
        &mut compositor,
        &mut [&mut game, &mut other, &mut newest],
        |_, clients| !clients[0].seen.pointer_locked,
    );
    // Coming back grants it again: the client asked for it for good.
    click(&mut compositor, 100.0, 100.0);
    pump(
        &mut compositor,
        &mut [&mut game, &mut other, &mut newest],
        |_, clients| clients[0].seen.pointer_locked,
    );

    // Ctrl+Alt+Escape gives the pointer back, and keeps it back while the
    // game keeps the focus.
    press_keys(&mut compositor, &[KEY_LEFTCTRL, KEY_LEFTALT, KEY_ESCAPE]);
    pump(
        &mut compositor,
        &mut [&mut game, &mut other, &mut newest],
        |_, clients| !clients[0].seen.pointer_locked,
    );
    assert_eq!(
        compositor.pointer_location(),
        (100.0, 100.0),
        "the release moves no pointer of its own"
    );
    for _ in 0..3 {
        pump(&mut compositor, &mut [&mut game, &mut other], |_, _| true);
    }
    assert!(
        !game.seen.pointer_locked,
        "the released lock does not come back by itself"
    );
    // Focus moving on and back is what makes it the game's again.
    click(&mut compositor, 400.0, 100.0);
    pump(&mut compositor, &mut [&mut game, &mut other], |_, _| true);
    click(&mut compositor, 100.0, 100.0);
    pump(
        &mut compositor,
        &mut [&mut game, &mut other],
        |_, clients| clients[0].seen.pointer_locked,
    );
}

#[test]
fn a_confined_pointer_stops_at_the_edge_of_its_region() {
    let mut compositor = headless(Box::<Columns>::default());
    let mut client = mapped_client(&mut compositor, "editor", (250, 200), [255, 0, 0, 255]);
    compositor
        .input(InputEvent::PointerMotion { x: 20.0, y: 20.0 })
        .expect("input");
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    client.confine_pointer((0, 0, 40, 40));
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.pointer_confined
    });

    // Past the region the pointer stops at its edge, on the client's own
    // surface coordinates.
    compositor
        .input(InputEvent::PointerMotion { x: 200.0, y: 150.0 })
        .expect("input");
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert_eq!(compositor.pointer_location(), (39.0, 39.0));
    assert_eq!(client.seen.pointer_positions.last(), Some(&(39.0, 39.0)));
    // Inside it moves as it always does.
    compositor
        .input(InputEvent::PointerMotion { x: 10.0, y: 30.0 })
        .expect("input");
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert_eq!(compositor.pointer_location(), (10.0, 30.0));
}

#[test]
fn an_inhibiting_client_keeps_the_desktops_chords_until_focus_or_the_release_chord() {
    let log = Rc::new(RefCell::new(HostLog::default()));
    let mut compositor = headless(Box::<Columns>::default());
    compositor.add_host(Box::new(Chords {
        log: Rc::clone(&log),
    }));
    let mut vm = mapped_client(&mut compositor, "vm", (250, 200), [255, 0, 0, 255]);

    // The desktop's own key surface answers Super+X wherever the keyboard is.
    press_keys(&mut compositor, &[KEY_LEFTMETA, KEY_X]);
    pump(&mut compositor, &mut [&mut vm], |_, _| true);
    assert_eq!(log.borrow().taken, vec![(KEY_X, false)]);
    assert!(
        !vm.seen.keys.iter().any(|(key, _)| *key == KEY_X),
        "the desktop took the chord: {:?}",
        vm.seen.keys
    );

    vm.inhibit_shortcuts();
    pump(&mut compositor, &mut [&mut vm], |_, clients| {
        clients[0].seen.shortcuts_inhibited
    });
    press_keys(&mut compositor, &[KEY_LEFTMETA, KEY_X]);
    pump(&mut compositor, &mut [&mut vm], |_, clients| {
        clients[0].seen.keys.iter().any(|(key, _)| *key == KEY_X)
    });
    assert_eq!(
        log.borrow().taken,
        vec![(KEY_X, false), (KEY_X, true)],
        "the second press was offered with the client's hold on the shortcuts"
    );

    // Losing the focus hands them back, and taking it again takes them.
    let mut other = mapped_client(&mut compositor, "other", (250, 200), [0, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut vm, &mut other], |_, clients| {
        !clients[0].seen.shortcuts_inhibited
    });
    click(&mut compositor, 100.0, 100.0);
    pump(&mut compositor, &mut [&mut vm, &mut other], |_, clients| {
        clients[0].seen.shortcuts_inhibited
    });

    // A locked session hands them back too: the lock screen is the
    // desktop's.
    compositor.lock_now().expect("lock");
    pump(&mut compositor, &mut [&mut vm, &mut other], |_, clients| {
        !clients[0].seen.shortcuts_inhibited
    });
    compositor.unlock().expect("unlock");
    pump(&mut compositor, &mut [&mut vm, &mut other], |_, clients| {
        clients[0].seen.shortcuts_inhibited
    });

    // The release chord hands them back until focus moves on.
    press_keys(&mut compositor, &[KEY_LEFTCTRL, KEY_LEFTALT, KEY_ESCAPE]);
    pump(&mut compositor, &mut [&mut vm, &mut other], |_, clients| {
        !clients[0].seen.shortcuts_inhibited
    });
    press_keys(&mut compositor, &[KEY_LEFTMETA, KEY_X]);
    pump(&mut compositor, &mut [&mut vm, &mut other], |_, _| true);
    assert_eq!(
        log.borrow().taken.last(),
        Some(&(KEY_X, false)),
        "the desktop answers its own chord again"
    );
}
