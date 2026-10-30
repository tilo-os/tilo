//! Layer surfaces: the session's own surfaces are Wayland clients that
//! place themselves on a layer of an output, above or below the windows.
//! They are composited like every other client's buffer, and take the
//! pointer and the keyboard that lands on them. Only a client the session
//! connected as trusted is offered the protocol.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a failed step in a test is the assertion"
)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::Duration;

use tilo_compositor::{Compositor, InputEvent, OutputConfig, StackPolicy};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::XdgWmBase,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
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
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    /// The size the compositor gave the layer surface.
    configured: Option<(u32, u32)>,
    pointer_inside: bool,
    buttons: Vec<(u32, bool)>,
    keyboard_focused: bool,
    keys: Vec<(u32, bool)>,
    frames: u32,
}

/// One client: a layer surface, a toplevel or both.
struct Client {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: Option<WlSurface>,
    layer: Option<ZwlrLayerSurfaceV1>,
    toplevel: Option<XdgToplevel>,
}

impl Client {
    fn connect(compositor: &mut Compositor, trusted: bool) -> Self {
        let socket = if trusted {
            compositor.connect_trusted_client().expect("trusted socket")
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
            layer: None,
            toplevel: None,
        }
    }

    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("dispatch");
        self.connection.flush().expect("flush");
    }

    /// Ask for a layer surface on `layer`: `size` (0 for the whole output
    /// along an axis), anchored to `anchor`, keeping `exclusive` pixels of
    /// the edge it hangs from.
    fn layer_surface(
        &mut self,
        layer: zwlr_layer_shell_v1::Layer,
        anchor: zwlr_layer_surface_v1::Anchor,
        size: (u32, u32),
        exclusive: i32,
        keyboard: zwlr_layer_surface_v1::KeyboardInteractivity,
    ) -> (WlSurface, ZwlrLayerSurfaceV1) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let shell = self.seen.layer_shell.as_ref().expect("zwlr_layer_shell_v1");
        let layer_surface = shell.get_layer_surface(&surface, None, layer, "test".into(), &qh, ());
        layer_surface.set_anchor(anchor);
        layer_surface.set_size(size.0, size.1);
        layer_surface.set_exclusive_zone(exclusive);
        layer_surface.set_keyboard_interactivity(keyboard);
        surface.commit();
        self.surface = Some(surface.clone());
        self.layer = Some(layer_surface.clone());
        (surface, layer_surface)
    }

    /// Open a toplevel `size` across.
    fn toplevel(&mut self, app_id: &str) -> WlSurface {
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
        self.surface = Some(surface.clone());
        self.toplevel = Some(toplevel);
        surface
    }

    /// Attach a buffer of one colour (RGBA) to the surface and commit it.
    fn show(&self, surface: &WlSurface, width: i32, height: i32, rgba: [u8; 4]) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let fd =
            rustix::fs::memfd_create("tilo-layer-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let mut file = std::fs::File::from(fd);
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
    clients: &mut [&mut Client],
    mut done: impl FnMut(&Compositor, &[&mut Client]) -> bool,
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

fn headless() -> Compositor {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor
}

/// A trusted client with a surface on `layer`, drawn at the size the
/// compositor gives it.
fn layer_client(
    compositor: &mut Compositor,
    layer: zwlr_layer_shell_v1::Layer,
    anchor: zwlr_layer_surface_v1::Anchor,
    size: (u32, u32),
    exclusive: i32,
    rgba: [u8; 4],
) -> Client {
    let mut client = Client::connect(compositor, true);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.compositor.is_some()
            && clients[0].seen.shm.is_some()
            && clients[0].seen.layer_shell.is_some()
    });
    let (surface, _) = client.layer_surface(
        layer,
        anchor,
        size,
        exclusive,
        zwlr_layer_surface_v1::KeyboardInteractivity::None,
    );
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured.is_some()
    });
    let (width, height) = client.seen.configured.expect("a configured size");
    client.show(
        &surface,
        i32::try_from(width).expect("a width"),
        i32::try_from(height).expect("a height"),
        rgba,
    );
    client
}

#[test]
fn the_session_trusts_a_client_with_layer_surfaces_and_no_one_else() {
    let mut compositor = headless();
    let mut ordinary = Client::connect(&mut compositor, false);
    pump(&mut compositor, &mut [&mut ordinary], |_, clients| {
        clients[0].seen.compositor.is_some()
    });
    assert!(
        !ordinary
            .seen
            .globals
            .iter()
            .any(|global| global == "zwlr_layer_shell_v1"),
        "an ordinary client cannot place a surface on a layer"
    );

    let mut trusted = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut [&mut trusted], |_, clients| {
        clients[0].seen.layer_shell.is_some()
    });
    assert!(
        trusted
            .seen
            .globals
            .iter()
            .any(|global| global == "zwlr_layer_shell_v1")
    );
}

#[test]
fn a_bar_on_the_top_layer_is_drawn_above_the_windows_and_keeps_its_height() {
    let mut compositor = headless();
    let mut bar = layer_client(
        &mut compositor,
        zwlr_layer_shell_v1::Layer::Top,
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
        (0, 32),
        32,
        [255, 0, 0, 255],
    );
    assert_eq!(bar.seen.configured, Some((640, 32)), "the whole width");

    pump(&mut compositor, &mut [&mut bar], |_, _| true);
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(320, 10), Some([255, 0, 0, 255]), "the bar");
    assert_eq!(frame.pixel(320, 200), Some(BACKGROUND), "the desk below it");
    assert_eq!(
        compositor.reserved("HEADLESS-1"),
        vec![(0, 0, 640, 32)],
        "the desktop keeps windows out of the bar"
    );

    // A window opened under it is drawn below it.
    let mut window = Client::connect(&mut compositor, false);
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        clients[0].seen.wm_base.is_some()
    });
    let surface = window.toplevel("a-window");
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        clients[0].seen.configured.is_some()
    });
    window.show(&surface, 640, 480, [0, 0, 255, 255]);
    pump(
        &mut compositor,
        &mut [&mut bar, &mut window],
        |compositor, _| !compositor.windows().is_empty(),
    );
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(320, 10), Some([255, 0, 0, 255]), "the bar wins");
}

#[test]
fn a_desk_on_the_background_layer_is_drawn_below_the_windows() {
    let mut compositor = headless();
    let mut desk = layer_client(
        &mut compositor,
        zwlr_layer_shell_v1::Layer::Background,
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Bottom
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
        (0, 0),
        0,
        [0, 255, 0, 255],
    );
    assert_eq!(desk.seen.configured, Some((640, 480)));
    pump(&mut compositor, &mut [&mut desk], |_, _| true);
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(320, 240), Some([0, 255, 0, 255]));

    let mut window = Client::connect(&mut compositor, false);
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        clients[0].seen.wm_base.is_some()
    });
    let surface = window.toplevel("a-window");
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        clients[0].seen.configured.is_some()
    });
    window.show(&surface, 200, 100, [0, 0, 255, 255]);
    pump(
        &mut compositor,
        &mut [&mut desk, &mut window],
        |compositor, _| !compositor.windows().is_empty(),
    );
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let (_, (x, y, w, h)) = compositor.windows()[0].clone();
    let (x, y) = (u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
    let (w, h) = (u32::try_from(w).unwrap(), u32::try_from(h).unwrap());
    assert_eq!(
        frame.pixel(x + w / 2, y + h / 2),
        Some([0, 0, 255, 255]),
        "the window is above the desk"
    );
}

#[test]
fn the_pointer_and_a_press_reach_the_layer_surface_that_is_above() {
    let mut compositor = headless();
    let mut bar = layer_client(
        &mut compositor,
        zwlr_layer_shell_v1::Layer::Top,
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
        (0, 32),
        32,
        [255, 0, 0, 255],
    );
    pump(&mut compositor, &mut [&mut bar], |_, clients| {
        clients[0].seen.pointer.is_some()
    });

    compositor
        .input(InputEvent::PointerMotion { x: 300.0, y: 16.0 })
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
    pump(&mut compositor, &mut [&mut bar], |_, clients| {
        clients[0].seen.buttons.len() == 2
    });
    assert!(bar.seen.pointer_inside);
    assert_eq!(bar.seen.buttons, vec![(BTN_LEFT, true), (BTN_LEFT, false)]);

    // Off the bar the pointer leaves it, even over the desk.

    compositor
        .input(InputEvent::PointerMotion { x: 300.0, y: 300.0 })
        .unwrap();
    pump(&mut compositor, &mut [&mut bar], |_, clients| {
        !clients[0].seen.pointer_inside
    });
}

#[test]
fn keys_go_to_the_layer_surface_that_takes_them() {
    let mut compositor = headless();
    let mut prompt = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut [&mut prompt], |_, clients| {
        clients[0].seen.layer_shell.is_some() && clients[0].seen.seat.is_some()
    });
    let (surface, _) = prompt.layer_surface(
        zwlr_layer_shell_v1::Layer::Overlay,
        zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left,
        (320, 200),
        0,
        zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive,
    );
    pump(&mut compositor, &mut [&mut prompt], |_, clients| {
        clients[0].seen.configured.is_some()
    });
    prompt.show(&surface, 320, 200, [255, 255, 0, 255]);
    pump(&mut compositor, &mut [&mut prompt], |_, clients| {
        clients[0].seen.keyboard_focused
    });

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
    pump(&mut compositor, &mut [&mut prompt], |_, clients| {
        clients[0].seen.keys.len() == 2
    });
    assert_eq!(prompt.seen.keys, vec![(KEY_A, true), (KEY_A, false)]);
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
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => {
                    seen.layer_shell = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &ZwlrLayerShellV1,
        _: zwlr_layer_shell_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            surface.ack_configure(serial);
            seen.configured = Some((width, height));
        }
    }
}

impl Dispatch<WlCompositor, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WlCompositor,
        _: <WlCompositor as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSurface, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WlSurface,
        _: <WlSurface as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore XdgWmBase);

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
            if seen.configured.is_none() {
                seen.configured = Some((0, 0));
            }
        }
    }
}

delegate_noop!(Seen: ignore XdgToplevel);

impl Dispatch<WlSeat, ()> for Seen {
    fn event(
        seen: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            let WEnum::Value(wanted) = capabilities else {
                return;
            };
            if wanted.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
            if wanted.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
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
                let pressed =
                    state == wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed);
                seen.buttons.push((button, pressed));
            }
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
                let pressed = state == wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed);
                seen.keys.push((key, pressed));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlCallback,
        event: wayland_client::protocol::wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            seen.frames = seen.frames.saturating_add(1);
        }
    }
}

#[test]
fn an_idle_session_draws_nothing() {
    // A bar and a window on screen, both drawn once.
    let mut compositor = headless();
    let mut bar = layer_client(
        &mut compositor,
        zwlr_layer_shell_v1::Layer::Top,
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
        (0, 32),
        32,
        [255, 0, 0, 255],
    );
    let mut window = Client::connect(&mut compositor, false);
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        clients[0].seen.wm_base.is_some()
    });
    let surface = window.toplevel("a-window");
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        clients[0].seen.configured.is_some()
    });
    window.show(&surface, 640, 480, [0, 0, 255, 255]);
    pump(
        &mut compositor,
        &mut [&mut bar, &mut window],
        |compositor, _| !compositor.windows().is_empty(),
    );
    compositor.render().unwrap();
    pump(&mut compositor, &mut [&mut bar, &mut window], |_, _| true);
    let (bar_frames, window_frames) = (bar.seen.frames, window.seen.frames);
    assert!(bar_frames > 0, "the bar was drawn");

    // Three seconds of a desktop asking for frames at its refresh, with
    // nothing happening: the session draws no frame and asks its clients
    // for none.
    let idle = Duration::from_secs(3);
    let refresh = Duration::from_millis(16);
    let started = std::time::Instant::now();
    while started.elapsed() < idle {
        compositor.dispatch(refresh).unwrap();
        compositor.render().unwrap();
        bar.exchange();
        window.exchange();
    }
    assert_eq!(bar.seen.frames, bar_frames, "the bar was not asked again");
    assert_eq!(
        window.seen.frames, window_frames,
        "the window was not asked again"
    );
    // What it shows is the frame it drew: nothing changed.
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(320, 10), Some([255, 0, 0, 255]), "the bar");
    assert_eq!(frame.pixel(320, 200), Some([0, 0, 255, 255]), "the window");
}
