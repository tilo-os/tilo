//! The size a window opens at.
//!
//! An `xdg_toplevel.configure` carrying 0x0 means "pick your own size" —
//! a client that gets one draws a complete frame at whatever it picked,
//! so the strip's real size arrives as a *correction* one frame too late
//! and every app opens visibly undersized before snapping. The strip
//! doesn't know a tab's size until after layout, which is after the
//! toplevel is created, so the opening configure is sent from the pass
//! that runs after layout. Reads a real socket, since the defect lives
//! entirely in what goes on the wire and when.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

/// Every `xdg_toplevel.configure` size the client was sent, in order.
type Configures = Arc<Mutex<Vec<(i32, i32)>>>;

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    configures: Configures,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // An unanswered ping is a protocol error that closes the
        // connection mid-test.
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The event under test: a real toolkit reads exactly this to decide how
        // big to render, and treats 0x0 as "my choice".
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state
                .configures
                .lock()
                .expect("configures")
                .push((width, height));
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore zxdg_decoration_manager_v1::ZxdgDecorationManagerV1);
delegate_noop!(Client: ignore zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1);

/// Compositor and client on a socket pair, pumped by hand so the test
/// never blocks on a message that is not coming.
struct Harness {
    display: Display<shell::compositor::state::State>,
    state: shell::compositor::state::State,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    // Dropping the loop invalidates the handle the compositor holds.
    _event_loop: EventLoop<'static, shell::compositor::state::State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop: EventLoop<'static, shell::compositor::state::State> =
            EventLoop::try_new().expect("calloop");
        let display: Display<shell::compositor::state::State> = Display::new().expect("display");
        let state = shell::compositor::state::State::new(event_loop.handle(), display.handle());

        let (server_side, client_side) = UnixStream::pair().expect("socketpair");
        display
            .handle()
            .insert_client(
                server_side,
                Arc::new(shell::compositor::state::ClientState::default()),
            )
            .expect("insert client");
        client_side
            .set_nonblocking(true)
            .expect("non-blocking client socket");
        let conn = Connection::from_socket(client_side).expect("client connection");
        let queue = conn.new_event_queue::<Client>();
        Self {
            display,
            state,
            queue,
            client: Client::default(),
            conn,
            _event_loop: event_loop,
        }
    }

    fn pump(&mut self) {
        for _ in 0..6 {
            let _ = self.queue.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.client);
        }
    }

    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> Option<I>
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .globals
            .iter()
            .find(|(_, i, _)| i == interface)?
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        Some(registry.bind::<I, (), Client>(
            name,
            version.min(advertised),
            &self.queue.handle(),
            (),
        ))
    }

    /// The way `ForeignSurfaceWidget::apply_layout` publishes once the strip
    /// has laid the tab out.
    fn lay_tabs_out(&self, tile: Rect) {
        for toplevel in self.state.xdg_shell_state.toplevel_surfaces().to_vec() {
            let id = shell::compositor::state::surface_id(toplevel.wl_surface());
            shell::compositor::foreign::store().note_placement(id, tile);
        }
    }

    /// The pass the run loop runs after every layout.
    fn flush_configures(&mut self) {
        shell::compositor::foreign::flush_configures(&mut self.state);
        self.pump();
    }
}

/// Bring a client to the point a toolkit reaches before it draws —
/// everything but the buffer, which is what it needs the configure for.
fn open_a_window(h: &mut Harness) -> Configures {
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let configures = h.client.configures.clone();

    let compositor = h
        .bind::<wl_compositor::WlCompositor>("wl_compositor", 4)
        .expect("wl_compositor");
    let wm_base = h
        .bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1)
        .expect("xdg_wm_base");
    let manager = h
        .bind::<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>(
            "zxdg_decoration_manager_v1",
            1,
        )
        .expect("zxdg_decoration_manager_v1");
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_app_id("org.tilo.test.opening-size".into());
    h.pump();

    // Toolkits negotiate decorations in the same round trip that creates the
    // toplevel, so drive it here and cover that route too.
    let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
    decoration.set_mode(zxdg_toplevel_decoration_v1::Mode::ServerSide);
    h.pump();

    // Leaked on purpose: destroying these would tear the toplevel down
    // before the test has read what the compositor said about it.
    std::mem::forget((surface, xdg_surface, toplevel, decoration, wm_base, manager));
    configures
}

#[test]
fn clients_are_configured_with_their_laid_out_tab_size_or_eventually_anyway() {
    // Phase `the_first_configure_a_client_receives_carries_its_tab_size`.
    {
        let mut h = Harness::new();
        let configures = open_a_window(&mut h);

        h.lay_tabs_out(Rect::new(0.0, 0.0, 940.0, 712.0));
        h.flush_configures();

        let seen = configures.lock().expect("configures").clone();
        let first = *seen.first().expect(
            "the client was never configured at all, so it can never map a \
             window — whatever else is true, that is worse than the defect \
             this test guards.",
        );
        assert_ne!(
            first,
            (0, 0),
            "the first configure carried no size. In xdg-shell that tells the \
             client to choose its own, so it commits a full frame at a size \
             the strip never asked for and is corrected afterwards — the \
             two-stage open this test exists to prevent. Configures: {seen:?}",
        );
        assert_eq!(
            first,
            (940, 712),
            "the first configure must carry the tab's laid-out size, not some \
             other size the client then has to be moved off. Configures: \
             {seen:?}",
        );
    }

    // Phase `nothing_is_configured_before_the_strip_has_laid_the_tab_out`.
    // Asserts both halves, so it can't pass by the client being
    // disconnected or the harness failing to talk at all.
    {
        let mut h = Harness::new();
        let configures = open_a_window(&mut h);

        // No tab rect published, so the strip has not laid this window out and
        // there is no size to send.
        h.flush_configures();
        let before = configures.lock().expect("configures").clone();
        assert!(
            before.is_empty(),
            "the compositor configured a client whose tab it had not laid out \
             yet, so the only size it could have sent is no size: {before:?}",
        );

        h.lay_tabs_out(Rect::new(0.0, 0.0, 800.0, 600.0));
        h.flush_configures();
        let after = configures.lock().expect("configures").clone();
        assert_eq!(
            after,
            vec![(800, 600)],
            "laying the tab out must produce exactly one configure, carrying \
             that tab's size. Anything else means the silence above was the \
             harness failing rather than the compositor waiting.",
        );
    }

    // Phase `a_toplevel_whose_tab_is_never_laid_out_is_configured_anyway`.
    // A tab that's never laid out (collapsed under a maximized sibling, or
    // off-screen) must not leave its client unable to map forever.
    {
        let mut h = Harness::new();
        let configures = open_a_window(&mut h);

        // `flush_configures` only sends the backstop once
        // `OPENING_CONFIGURE_GRACE` (250 ms) has elapsed. Poll that production
        // path instead of sleeping past a guessed 400 ms.
        tilo_test_env::wait_until(
            "a never-laid-out toplevel received its backstop configure",
            || {
                h.flush_configures();
                configures
                    .lock()
                    .expect("configures")
                    .first()
                    .is_some_and(|&(w, height)| w > 0 && height > 0)
            },
        );
        let seen = configures.lock().expect("configures").clone();
        let first = *seen.first().expect(
            "a toplevel the strip never laid out was never configured, so its \
             client can never map and the window never opens at all.",
        );
        assert!(
            first.0 > 0 && first.1 > 0,
            "the backstop configure must carry a real size — sending no size \
             would hand the client back the same choice the whole path exists \
             to take away from it. Configures: {seen:?}",
        );
    }
}
