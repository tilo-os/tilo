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

use std::cell::RefCell;
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Logical, Point, Rectangle, Transform};
use tilo_compositor::{
    Arrangement, Compositor, GestureEvent, HostKey, HostLayer, InputEvent, OutputConfig,
    StackPolicy, SurfaceHost, Switch, TabletEvent, TabletPhase, WindowInfo, WindowPolicy,
};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface, wl_touch, wl_touch::WlTouch,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1, ext_session_lock_v1,
    ext_session_lock_v1::ExtSessionLockV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

const BTN_LEFT: u32 = 0x110;
const KEY_A: u32 = 30;
const BACKGROUND: [u8; 4] = [26, 27, 38, 255];

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per event a test client waits for"
)]
struct Seen {
    globals: Vec<String>,
    lock_manager: Option<ExtSessionLockManagerV1>,
    locked: bool,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    touch: Option<WlTouch>,
    /// Touch events as `down id x y`, `motion id x y`, `up id`, `frame`.
    touches: Vec<String>,
    configured: bool,
    pointer_inside: bool,
    buttons: Vec<(u32, bool)>,
    keyboard_focused: bool,
    keys: Vec<(u32, bool)>,
    frames_done: u32,
    closed: bool,
}

/// One client with one toplevel of a solid colour.
struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: Option<WlSurface>,
    toplevel: Option<XdgToplevel>,
}

impl TestClient {
    fn connect(compositor: &mut Compositor) -> Self {
        let socket = compositor.connect_client().expect("client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Seen::default(),
            surface: None,
            toplevel: None,
        }
    }

    /// Send what is queued, read what arrived, without blocking.
    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            // Nothing to read is fine: WouldBlock.
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("dispatch");
        self.connection.flush().expect("flush");
    }

    fn open_window(&mut self, app_id: &str) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        surface.commit();
        self.surface = Some(surface);
        self.toplevel = Some(toplevel);
    }

    /// Attach a buffer of one colour (RGBA) and commit it with a frame
    /// callback.
    fn show(&self, width: i32, height: i32, rgba: [u8; 4]) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let fd = rustix::fs::memfd_create("tilo-compositor-test", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        let mut file = std::fs::File::from(fd);
        // ARGB8888 is B, G, R, A in memory.
        let pixel = [rgba[2], rgba[1], rgba[0], rgba[3]];
        file.write_all(&pixel.repeat(len / 4)).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        let surface = self.surface.as_ref().expect("surface");
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.frame(&qh, ());
        surface.commit();
        pool.destroy();
    }
}

/// Let compositor and clients talk until `done` holds.
fn pump(
    compositor: &mut Compositor,
    clients: &mut [&mut TestClient],
    mut done: impl FnMut(&Compositor, &[&mut TestClient]) -> bool,
) {
    for _ in 0..500 {
        for client in clients.iter_mut() {
            client.exchange();
        }
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        for client in clients.iter_mut() {
            client.exchange();
        }
        if done(compositor, clients) {
            return;
        }
    }
    panic!("condition not reached");
}

fn headless(policy: Box<dyn WindowPolicy>) -> Compositor {
    let mut compositor = Compositor::headless(policy).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor
}

/// Open a toplevel of `size` and colour, and wait until it is mapped.
fn mapped_client(
    compositor: &mut Compositor,
    app_id: &str,
    size: (i32, i32),
    rgba: [u8; 4],
) -> TestClient {
    let mut client = TestClient::connect(compositor);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.seat.is_some()
    });
    client.open_window(app_id);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    client.show(size.0, size.1, rgba);
    let app = app_id.to_owned();
    pump(compositor, &mut [&mut client], |compositor, _| {
        compositor
            .windows()
            .iter()
            .any(|(info, _)| info.app_id.as_deref() == Some(app.as_str()))
    });
    client
}

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
        frame.pixel(220, 190),
        Some([255, 0, 0, 255]),
        "top-left corner"
    );
    assert_eq!(frame.pixel(219, 190), Some(BACKGROUND));
    assert_eq!(frame.pixel(10, 10), Some(BACKGROUND));

    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.frames_done > 0
    });
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
    assert_eq!(frame.pixel(444, 384), Some([0, 0, 255, 255]));
    assert_eq!(frame.pixel(836, 576), Some([0, 0, 255, 255]));
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
            buffer: self.buffer.clone(),
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

impl Dispatch<WlRegistry, ()> for Seen {
    fn event(
        seen: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            seen.globals.push(interface.clone());
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "ext_session_lock_manager_v1" => {
                    seen.lock_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            seen.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, xdg_toplevel::Event::Close) {
            seen.closed = true;
        }
    }
}

impl Dispatch<WlSeat, ()> for Seen {
    fn event(
        seen: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Touch) && seen.touch.is_none() {
                seen.touch = Some(seat.get_touch(qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { .. } => seen.pointer_inside = true,
            wl_pointer::Event::Leave { .. } => seen.pointer_inside = false,
            wl_pointer::Event::Button { button, state, .. } => {
                seen.buttons.push((
                    button,
                    state == WEnum::Value(wl_pointer::ButtonState::Pressed),
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlTouch, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { id, x, y, .. } => {
                seen.touches.push(format!("down {id} {x} {y}"));
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                seen.touches.push(format!("motion {id} {x} {y}"));
            }
            wl_touch::Event::Up { id, .. } => seen.touches.push(format!("up {id}")),
            wl_touch::Event::Frame => seen.touches.push("frame".into()),
            wl_touch::Event::Cancel => seen.touches.push("cancel".into()),
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => seen.keyboard_focused = true,
            wl_keyboard::Event::Leave { .. } => seen.keyboard_focused = false,
            wl_keyboard::Event::Key { key, state, .. } => {
                seen.keys
                    .push((key, state == WEnum::Value(wl_keyboard::KeyState::Pressed)));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        seen.frames_done += 1;
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, ext_session_lock_v1::Event::Locked) {
            seen.locked = true;
        }
    }
}

delegate_noop!(Seen: ignore ExtSessionLockManagerV1);
delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);

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
