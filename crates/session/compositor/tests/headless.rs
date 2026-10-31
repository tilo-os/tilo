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
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Logical, Point, Rectangle, Transform};
use tilo_compositor::{
    Arrangement, BackdropImage, BackdropRequest, Compositor, GestureEvent, HostDrag, HostFrame,
    HostKey, HostLayer, InputEvent, KeyboardLayout, OutputConfig, PadAction, StackPolicy,
    SurfaceHost, Switch, TabletEvent, TabletPadEvent, TabletPhase, TextField, TextFieldRefusal,
    TextInsertion, WindowEffect, WindowFrame, WindowInfo, WindowPicture, WindowPolicy,
};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface, wl_touch, wl_touch::WlTouch,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1, ext_session_lock_v1,
    ext_session_lock_v1::ExtSessionLockV1,
};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::ZwpTabletToolV2,
    zwp_tablet_v2::ZwpTabletV2,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3, zwp_text_input_v3,
    zwp_text_input_v3::{ContentHint, ContentPurpose, ZwpTextInputV3},
};
use wayland_protocols::xdg::dialog::v1::client::{
    xdg_dialog_v1::XdgDialogV1, xdg_wm_dialog_v1::XdgWmDialogV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2, zwp_input_method_v2,
    zwp_input_method_v2::ZwpInputMethodV2,
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
    text_input_manager: Option<ZwpTextInputManagerV3>,
    text_input: Option<ZwpTextInputV3>,
    /// Whether the field has the focus and has asked for typing, as the
    /// compositor told it.
    field_entered: bool,
    /// The strings the field was made to contain.
    committed: Vec<String>,
    /// How many times the compositor applied state to the field.
    field_done: u32,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    input_method: Option<ZwpInputMethodV2>,
    /// How many times the compositor applied the input method's state: what
    /// its next commit is numbered.
    ime_done: u32,
    /// Whether the compositor asked the input method to type.
    ime_activated: bool,
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
    /// The keymap the keyboard sent, as XKB text: the client's own answer to
    /// which characters its keys type.
    keymap: Option<String>,
    frames_done: u32,
    closed: bool,
    tablet_manager: Option<ZwpTabletManagerV2>,
    wm_dialog: Option<XdgWmDialogV1>,
    /// Tablet pad events as `added`, `group`, `buttons 4`, `done`,
    /// `enter`, `mode 1`, `button 2 pressed`, `ring 90`, `ring stop`,
    /// `removed`.
    pad: Vec<String>,
    /// The surface the pad entered.
    pad_surface: Option<WlSurface>,
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
        Self::connect_as(compositor, false)
    }

    /// Connect as a client the session started, which may be its input
    /// method.
    fn connect_trusted(compositor: &mut Compositor) -> Self {
        Self::connect_as(compositor, true)
    }

    fn connect_as(compositor: &mut Compositor, trusted: bool) -> Self {
        let socket = if trusted {
            compositor.connect_trusted_client().expect("client socket")
        } else {
            compositor.connect_client().expect("client socket")
        };
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

    /// Give the window a text field: a `text-input-v3` object on its
    /// surface, waiting for typing.
    fn own_text_field(&mut self) {
        self.own_text_field_as(ContentHint::empty(), ContentPurpose::Normal);
    }

    /// Give the window a text field that declares what it takes.
    fn own_text_field_as(&mut self, hint: ContentHint, purpose: ContentPurpose) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .text_input_manager
            .as_ref()
            .expect("zwp_text_input_manager_v3");
        let seat = self.seen.seat.as_ref().expect("wl_seat");
        let field = manager.get_text_input(seat, &qh, ());
        field.set_content_type(hint, purpose);
        field.enable();
        field.commit();
        self.seen.text_input = Some(field);
    }

    /// Change what the field declares itself to be, as an app does when the
    /// person moves to another box.
    fn set_content_type(&self, hint: ContentHint, purpose: ContentPurpose) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.set_content_type(hint, purpose);
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Move the field's caret: a commit that changes what the field
    /// committed, as any edit does.
    fn move_caret(&self, x: i32) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.set_cursor_rectangle(x, 0, 1, 20);
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Stop the field from asking for typing.
    fn release_text_field(&self) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.disable();
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Serve the seat as its input method (the on-screen keyboard).
    fn own_input_method(&mut self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .input_method_manager
            .as_ref()
            .expect("zwp_input_method_manager_v2");
        let seat = self.seen.seat.as_ref().expect("wl_seat");
        self.seen.input_method = Some(manager.get_input_method(seat, &qh, ()));
    }

    /// Type `text` into the field the input method serves.
    fn type_with_input_method(&self, text: &str) {
        let method = self.seen.input_method.as_ref().expect("input method");
        method.commit_string(text.to_owned());
        method.commit(self.seen.ime_done);
        self.connection.flush().expect("flush");
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
                "zwp_tablet_manager_v2" => {
                    seen.tablet_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "xdg_wm_dialog_v1" => seen.wm_dialog = Some(registry.bind(name, 1, qh, ())),
                "zwp_text_input_manager_v3" => {
                    seen.text_input_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_input_method_manager_v2" => {
                    seen.input_method_manager = Some(registry.bind(name, 1, qh, ()));
                }
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
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                let mut text = Vec::new();
                if std::fs::File::from(fd)
                    .take(u64::from(size))
                    .read_to_end(&mut text)
                    .is_ok()
                {
                    seen.keymap = Some(String::from_utf8_lossy(&text).into_owned());
                }
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

impl Dispatch<ZwpTabletSeatV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_tablet_seat_v2::Event::PadAdded { .. } = event {
            seen.pad.push("added".into());
        }
    }

    event_created_child!(Seen, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletPadV2,
        event: zwp_tablet_pad_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let line = match event {
            zwp_tablet_pad_v2::Event::Group { .. } => "group".into(),
            zwp_tablet_pad_v2::Event::Buttons { buttons } => format!("buttons {buttons}"),
            zwp_tablet_pad_v2::Event::Done => "done".into(),
            zwp_tablet_pad_v2::Event::Enter { surface, .. } => {
                seen.pad_surface = Some(surface);
                "enter".into()
            }
            zwp_tablet_pad_v2::Event::Leave { .. } => "leave".into(),
            zwp_tablet_pad_v2::Event::Button { button, state, .. } => {
                let pressed = state == WEnum::Value(zwp_tablet_pad_v2::ButtonState::Pressed);
                format!(
                    "button {button} {}",
                    if pressed { "pressed" } else { "released" }
                )
            }
            zwp_tablet_pad_v2::Event::Removed => "removed".into(),
            _ => return,
        };
        seen.pad.push(line);
    }

    event_created_child!(Seen, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<ZwpTextInputV3, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => seen.field_entered = true,
            zwp_text_input_v3::Event::Leave { .. } => seen.field_entered = false,
            zwp_text_input_v3::Event::CommitString { text: Some(text) } => {
                seen.committed.push(text);
            }
            zwp_text_input_v3::Event::Done { .. } => seen.field_done += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => seen.ime_activated = true,
            zwp_input_method_v2::Event::Deactivate => seen.ime_activated = false,
            zwp_input_method_v2::Event::Done => seen.ime_done += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletPadGroupV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletPadGroupV2,
        event: zwp_tablet_pad_group_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_tablet_pad_group_v2::Event::ModeSwitch { mode, .. } = event {
            seen.pad.push(format!("mode {mode}"));
        }
    }

    event_created_child!(Seen, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadRingV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletPadRingV2,
        event: zwp_tablet_pad_ring_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_ring_v2::Event::Angle { degrees } => {
                seen.pad.push(format!("ring {degrees}"));
            }
            zwp_tablet_pad_ring_v2::Event::Stop => seen.pad.push("ring stop".into()),
            _ => {}
        }
    }
}

delegate_noop!(Seen: ignore ZwpTabletManagerV2);
delegate_noop!(Seen: ignore ZwpTabletV2);
delegate_noop!(Seen: ignore ZwpTabletToolV2);
delegate_noop!(Seen: ignore ZwpTabletPadStripV2);
delegate_noop!(Seen: ignore ExtSessionLockManagerV1);
delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore XdgWmDialogV1);
delegate_noop!(Seen: ignore ZwpTextInputManagerV3);
delegate_noop!(Seen: ignore ZwpInputMethodManagerV2);
delegate_noop!(Seen: ignore ZwpInputMethodKeyboardGrabV2);
delegate_noop!(Seen: ignore XdgDialogV1);
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

    fn monitors(&mut self, monitors: &[(Rectangle<i32, Logical>, String)]) {
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
    let field = compositor.dictation_field().expect("a field to dictate into");
    assert!(!field.multiline, "an ordinary text box takes one line");

    // The caret moves: the same field, another revision of what it
    // committed.
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
    let field = compositor.dictation_field().expect("a field to dictate into");

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
    assert!(client.seen.committed.is_empty(), "a released field takes nothing");
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
    let field = compositor.dictation_field().expect("a field to dictate into");

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

#[test]
fn the_policy_knows_each_output_by_the_monitor_on_it() {
    let told = Rc::new(RefCell::new(Vec::new()));
    let mut compositor = Compositor::headless(Box::new(Monitors(Rc::clone(&told)))).unwrap();
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
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
