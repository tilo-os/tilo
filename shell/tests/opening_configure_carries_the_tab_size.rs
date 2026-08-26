//! The size a window opens at.
//!
//! An `xdg_toplevel.configure` carrying width 0, height 0 is not a
//! formality — in xdg-shell it means "pick your own size". A client told
//! that picks one, commits a buffer at it, and draws a complete frame at
//! it. So a compositor that tiles its windows and sends that event has
//! already lost: whatever the strip decides afterwards arrives as a
//! *correction*, one frame too late, and every application on the machine
//! opens visibly undersized inside a correctly-sized frame before
//! snapping into place.
//!
//! The size is a fact about the strip, and the strip does not know it
//! until it has laid the tab out — which is after the toplevel is
//! created, not before. So the opening configure is sent from the pass
//! that runs after layout, and this test reads the wire to prove it: the
//! **first** configure a real client receives carries the tab's real
//! size, and nothing carrying no size precedes it.
//!
//! Deliberately talks to a socket. The defect lived entirely in what went
//! on the wire and when; a check that could not observe the wire could
//! not have caught it, and did not.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        _: &(),
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
        _: &(),
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
        _: &(),
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
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The event under test. A real toolkit reads exactly this to
        // decide how big to render, and treats 0x0 as "my choice".
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

/// The compositor on one end of a socket pair and a client on the other,
/// pumped by hand so the test never blocks on a message that is not
/// coming.
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

    /// Publish a tab rect for every known toplevel, the way
    /// `ForeignSurfaceWidget::apply_layout` does once the strip has laid
    /// the tab out.
    fn lay_tabs_out(&mut self, tile: Rect) {
        for toplevel in self.state.xdg_shell_state.toplevel_surfaces().to_vec() {
            let id = shell::compositor::state::surface_id(toplevel.wl_surface());
            shell::compositor::foreign::store().note_placement(id, tile);
        }
    }

    /// The configure pass the run loop runs after every layout.
    fn flush_configures(&mut self) {
        shell::compositor::foreign::flush_configures(&mut self.state);
        self.pump();
    }
}

/// Bring a client up to the point a toolkit reaches before it draws:
/// bound, surface created, toplevel created, decorations negotiated.
/// Everything but the buffer, which is what it needs the configure for.
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
    toplevel.set_app_id("org.qos.test.opening-size".into());
    h.pump();

    // Toolkits negotiate decorations in the same round trip in which they
    // create the toplevel, and that negotiation used to be what put the
    // sizeless configure on the wire. Drive it, so this test covers that
    // route too rather than only the one it was named for.
    let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
    decoration.set_mode(zxdg_toplevel_decoration_v1::Mode::ServerSide);
    h.pump();

    // Leaked on purpose: destroying these would tear the toplevel down
    // before the test has read what the compositor said about it.
    std::mem::forget((surface, xdg_surface, toplevel, decoration, wm_base, manager));
    configures
}

/// The headline. A client's **first** configure states the size of the
/// tab it is opening into — so the first frame it ever draws is already
/// the right size, and there is no second, corrected one.
#[test]
fn the_first_configure_a_client_receives_carries_its_tab_size() {
    let mut h = Harness::new();
    let configures = open_a_window(&mut h);

    // One shell frame: the strip lays the new tab out, then the
    // compositor tells the client about it.
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

/// The mechanism, stated as an invariant: nothing is put on the wire
/// while the answer would have to be "you choose".
///
/// Asserts both halves so it cannot pass by the client having been
/// disconnected or the harness having failed to talk at all — silence
/// before the layout only means something if the layout breaks it.
#[test]
fn nothing_is_configured_before_the_strip_has_laid_the_tab_out() {
    let mut h = Harness::new();
    let configures = open_a_window(&mut h);

    // A frame's worth of pumping with no tab rect published: the strip
    // has not laid this window out, so there is no size to send.
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

/// And the wait is bounded. A tab that is never laid out — collapsed
/// under a maximized sibling, or in a workspace that is not on screen —
/// must not leave its client unable to map for ever. A window nobody can
/// see is worse than a window that opened at an approximate size.
#[test]
fn a_toplevel_whose_tab_is_never_laid_out_is_configured_anyway() {
    let mut h = Harness::new();
    let configures = open_a_window(&mut h);

    // Past the grace `flush_configures` gives a tab rect to arrive in.
    std::thread::sleep(Duration::from_millis(400));
    h.flush_configures();

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
