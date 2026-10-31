//! Who draws the title bar.
//!
//! The desktop draws each window's frame itself — the tab is the frame — so
//! an app that also draws one draws a second bar inside the first. Both
//! decoration protocols have to say so, because a toolkit speaks whichever
//! of the two it knows: `xdg-decoration` for most, the older
//! `org_kde_kwin_server_decoration` for Qt5 and some GTK and Wine builds.
//!
//! Not only cosmetic: a client that keeps its own title bar keeps it out of
//! the size it was configured — configured 670x668 it commits 670x633 and
//! never commits again, leaving a 35px dead band of empty tab and stranding
//! the presentation gate.
//!
//! The frame belongs to the desktop rather than to the window's state, so
//! it is the same frame focused or not: what is checked is that neither
//! protocol changes its answer when another window takes the keyboard.

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

use smithay::utils::{Logical, Point, Rectangle};
use tilo_compositor::{Compositor, InputEvent, OutputConfig, WindowInfo, WindowPolicy};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_registry::WlRegistry, wl_shm,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, Mode as XdgMode, ZxdgToplevelDecorationV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_misc::server_decoration::client::org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager;
use wayland_protocols_misc::server_decoration::client::{
    org_kde_kwin_server_decoration::{self as kde_decoration, Mode as KdeMode},
    org_kde_kwin_server_decoration_manager::{self as kde_manager, Mode as KdeDefaultMode},
};

const BTN_LEFT: u32 = 0x110;

/// What this client heard: the globals, and every decoration answer.
#[derive(Default)]
struct Seen {
    globals: Vec<(u32, String, u32)>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    decoration_manager: Option<ZxdgDecorationManagerV1>,
    kde_manager: Option<OrgKdeKwinServerDecorationManager>,
    /// Whether the toplevel's first configure arrived.
    configured: bool,
    /// Every mode the compositor configured through `xdg-decoration`, in
    /// order.
    xdg_modes: Vec<XdgMode>,
    /// The KDE manager's `default_mode`: what it tells a client that asks
    /// for nothing.
    kde_default: Option<KdeDefaultMode>,
    /// Every mode the compositor stated through the KDE protocol, in order.
    kde_modes: Vec<KdeMode>,
}

/// One client with one toplevel.
struct Client {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: Option<WlSurface>,
    toplevel: Option<XdgToplevel>,
}

impl Client {
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

    /// Create a surface and its toplevel, and commit it: the compositor
    /// answers with the first configure.
    fn open_window(&mut self, app_id: &str) -> XdgToplevel {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor is not advertised")
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base is not advertised")
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        self.surface = Some(surface);
        self.toplevel = Some(toplevel.clone());
        toplevel
    }

    /// Ask for the frame to be drawn by the client, which is the request
    /// the desktop refuses.
    fn ask_for_client_side(&self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .decoration_manager
            .as_ref()
            .expect("zxdg_decoration_manager_v1 is not advertised");
        let toplevel = self.toplevel.as_ref().expect("toplevel");
        manager
            .get_toplevel_decoration(toplevel, &qh, ())
            .set_mode(XdgMode::ClientSide);
    }

    /// Ask for the frame to be drawn by the client over the KDE spelling.
    fn ask_for_client_frame_kde(&self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .kde_manager
            .as_ref()
            .expect("org_kde_kwin_server_decoration_manager is not advertised");
        let surface = self.surface.clone().expect("surface");
        let decoration = manager.create(&surface, &qh, ());
        decoration.request_mode(KdeMode::Client);
    }

    /// Create a KDE decoration and state no preference on it.
    fn decorate_kde(&self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .kde_manager
            .as_ref()
            .expect("org_kde_kwin_server_decoration_manager is not advertised");
        let surface = self.surface.clone().expect("surface");
        manager.create(&surface, &qh, ());
    }

    fn commit(&self) {
        self.surface.as_ref().expect("surface").commit();
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
        surface.commit();
        pool.destroy();
    }
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
            seen.globals.push((name, interface.clone(), version));
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "zxdg_decoration_manager_v1" => {
                    seen.decoration_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "org_kde_kwin_server_decoration_manager" => {
                    seen.kde_manager = Some(registry.bind(name, 1, qh, ()));
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
        // An unanswered ping is a protocol error that closes the connection.
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

impl Dispatch<ZxdgToplevelDecorationV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure {
            mode: WEnum::Value(mode),
        } = event
        {
            seen.xdg_modes.push(mode);
        }
    }
}

impl Dispatch<OrgKdeKwinServerDecorationManager, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &OrgKdeKwinServerDecorationManager,
        event: kde_manager::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let kde_manager::Event::DefaultMode {
            mode: WEnum::Value(mode),
        } = event
        {
            seen.kde_default = Some(mode);
        }
    }
}

impl Dispatch<kde_decoration::OrgKdeKwinServerDecoration, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &kde_decoration::OrgKdeKwinServerDecoration,
        event: kde_decoration::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let kde_decoration::Event::Mode {
            mode: WEnum::Value(mode),
        } = event
        {
            seen.kde_modes.push(mode);
        }
    }
}

delegate_noop!(Seen: ignore ZxdgDecorationManagerV1);
delegate_noop!(Seen: ignore XdgToplevel);
delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);

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

fn headless(policy: Box<dyn WindowPolicy>) -> Compositor {
    let mut compositor = Compositor::headless(policy).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor
}

/// Connect and wait until the registry has told the client what exists.
fn connected(compositor: &mut Compositor) -> Client {
    let mut client = Client::connect(compositor);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some()
    });
    client
}

/// Places windows left to right, 300 logical pixels apart, at 250x200.
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
}

#[test]
fn both_decoration_managers_are_advertised() {
    let mut compositor = headless(Box::<Columns>::default());
    let client = connected(&mut compositor);

    let names: Vec<&str> = client
        .seen
        .globals
        .iter()
        .map(|(_, interface, _)| interface.as_str())
        .collect();
    assert!(
        !names.is_empty(),
        "the client saw no globals at all — the harness failed to talk to \
         the compositor, so this test proved nothing."
    );
    for interface in [
        "zxdg_decoration_manager_v1",
        "org_kde_kwin_server_decoration_manager",
    ] {
        assert!(
            names.contains(&interface),
            "{interface} is not advertised, so a toolkit that decorates \
             itself when it cannot see the manager draws its own title bar \
             and keeps it out of the size this compositor gave it. \
             Advertised: {names:?}"
        );
    }
}

#[test]
fn a_client_that_asks_to_draw_its_own_frame_is_told_the_desktop_does() {
    let mut compositor = headless(Box::<Columns>::default());
    let mut client = connected(&mut compositor);

    // The order toolkits use: create the decoration, state a preference,
    // then commit, so the answer rides on the first configure.
    client.open_window("own-frame");
    client.ask_for_client_side();
    client.commit();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        !clients[0].seen.xdg_modes.is_empty()
    });

    assert!(
        client
            .seen
            .xdg_modes
            .iter()
            .all(|mode| *mode == XdgMode::ServerSide),
        "expected every configure to say ServerSide; got {:?}. A client \
         told ClientSide draws its own title bar inside the frame the \
         desktop already drew for it.",
        client.seen.xdg_modes
    );
}

#[test]
fn a_kde_client_that_asks_to_draw_its_own_frame_is_told_server() {
    let mut compositor = headless(Box::<Columns>::default());
    let mut client = connected(&mut compositor);

    client.open_window("kde-frame");
    client.ask_for_client_frame_kde();
    client.commit();
    // Each pump lets several requests and their replies cross: an exchange
    // that feeds itself shows up as one more mode per round.
    for _ in 0..4 {
        pump(&mut compositor, &mut [&mut client], |_, _| true);
    }

    assert_eq!(
        client.seen.kde_modes,
        vec![KdeMode::Server],
        "a KDE-protocol client is told Server once, when its decoration is \
         created; answering its request again would loop forever with a \
         client that acknowledges with the mode it wants"
    );
}

#[test]
fn a_client_that_asks_for_no_frame_of_its_own_is_told_server() {
    let mut compositor = headless(Box::<Columns>::default());
    let mut client = connected(&mut compositor);

    // Binding the manager is what asks for the default, so it is one
    // exchange after the registry has been read.
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.kde_default.is_some()
    });
    assert_eq!(
        client.seen.kde_default,
        Some(KdeDefaultMode::Server),
        "a client that never states a preference must be told the desktop \
         draws the frame"
    );

    client.open_window("default-frame");
    client.decorate_kde();
    client.commit();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        !clients[0].seen.kde_modes.is_empty()
    });
    assert_eq!(
        client.seen.kde_modes,
        vec![KdeMode::Server],
        "the frame of a window that states no preference is the desktop's"
    );
}

#[test]
fn the_frame_does_not_change_when_another_window_takes_focus() {
    let mut compositor = headless(Box::<Columns>::default());
    let mut left = connected(&mut compositor);
    left.open_window("left");
    left.ask_for_client_side();
    left.commit();
    pump(&mut compositor, &mut [&mut left], |_, clients| {
        clients[0].seen.configured && !clients[0].seen.xdg_modes.is_empty()
    });
    left.show(250, 200, [255, 0, 0, 255]);
    pump(&mut compositor, &mut [&mut left], |compositor, _| {
        compositor.windows().len() == 1
    });

    let answer = left.seen.xdg_modes.clone();
    assert_eq!(answer, vec![XdgMode::ServerSide]);

    let mut right = connected(&mut compositor);
    right.open_window("right");
    right.ask_for_client_side();
    right.commit();
    pump(&mut compositor, &mut [&mut right], |_, clients| {
        clients[0].seen.configured && !clients[0].seen.xdg_modes.is_empty()
    });
    right.show(250, 200, [0, 0, 255, 255]);
    pump(
        &mut compositor,
        &mut [&mut left, &mut right],
        |compositor, _| compositor.windows().len() == 2,
    );

    // The right window stands at (300, 0) and has the focus; a press on the
    // left one moves it there.
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
        |compositor, _| {
            compositor
                .focused_window()
                .and_then(|info| info.app_id)
                .as_deref()
                == Some("left")
        },
    );

    // Focus moved, and the frame stayed where it was: the desktop's in
    // every case, so there is nothing for the focus to change.
    assert_eq!(
        left.seen.xdg_modes, answer,
        "the decoration mode changed when another window took focus; the \
         frame is the tab the desktop draws, not a state of the window"
    );
}
