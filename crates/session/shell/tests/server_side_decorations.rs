//! Who draws the title bar.
//!
//! Tilo draws window chrome itself, so an app that draws its own draws a
//! second one inside the first; `xdg_decoration` is the wire mechanism
//! for saying so. Not only cosmetic: a client that keeps a title bar
//! keeps it out of the size the compositor gave it — configured 670x668
//! it commits 670x633 and never commits again, leaving a 35px dead band
//! of empty tab and stranding the presentation gate.

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
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

/// The globals offered, and every decoration mode configured, in order.
#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    modes: Arc<Mutex<Vec<zxdg_toplevel_decoration_v1::Mode>>>,
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
        // An unanswered ping is a protocol error that closes the connection
        // mid-test.
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

impl Dispatch<zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event
            && let wayland_client::WEnum::Value(mode) = mode
        {
            state.modes.lock().expect("modes").push(mode);
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zxdg_decoration_manager_v1::ZxdgDecorationManagerV1);

/// Compositor and client on a socket pair, pumped by hand so the test
/// never blocks on a message that is not coming.
struct Harness {
    display: Display<shell::compositor::state::State>,
    state: shell::compositor::state::State,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    // Dropping the loop invalidates the handle the compositor state holds.
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

        // Non-blocking, so `prepare_read().read()` in `pump` reports "nothing
        // waiting" instead of parking forever on a round with nothing to say.
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

    /// A full round trip each way, repeated so a reply that depends on a
    /// request made this round still lands.
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

    /// One shell frame as the run loop runs it. A configure sent the instant
    /// a toplevel is created carries a decoration mode with **no size**,
    /// telling the client to pick its own (the bug that made every app open
    /// at the wrong size), so the mode is staged and rides out on the first
    /// size-bearing configure, which this pass sends.
    fn lay_out_and_configure(&mut self, tile: vello::kurbo::Rect) {
        use shell::compositor::foreign;
        for toplevel in self.state.xdg_shell_state.toplevel_surfaces().to_vec() {
            let id = shell::compositor::state::surface_id(toplevel.wl_surface());
            // What `ForeignSurfaceWidget::apply_layout` publishes once the strip has
            // given the tab a rect.
            foreign::store().note_placement(id, tile);
        }
        foreign::flush_configures(&mut self.state);
        self.pump();
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
}

/// Returns every decoration mode configured plus the globals, so a
/// failure can say whether the manager was missing or merely silent.
fn negotiate(
    ask_for: Option<zxdg_toplevel_decoration_v1::Mode>,
) -> (Vec<String>, Vec<zxdg_toplevel_decoration_v1::Mode>) {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();

    let names: Vec<String> = h.client.globals.iter().map(|(_, i, _)| i.clone()).collect();
    let modes = h.client.modes.clone();

    let Some(manager) = h.bind::<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>(
        "zxdg_decoration_manager_v1",
        1,
    ) else {
        return (names, Vec::new());
    };
    let compositor = h
        .bind::<wl_compositor::WlCompositor>("wl_compositor", 4)
        .expect("wl_compositor");
    let wm_base = h
        .bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1)
        .expect("xdg_wm_base");
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    h.pump();

    // The order sctk uses: create the decoration, then state a preference.
    // The compositor answers on the first, and again on the second only if
    // it changed its mind.
    let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
    h.pump();
    if let Some(mode) = ask_for {
        decoration.set_mode(mode);
        h.pump();
    }
    // The answer is staged until the strip has a size to send with it.
    h.lay_out_and_configure(vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0));

    let out = modes.lock().expect("modes").clone();
    (names, out)
}

#[test]
fn every_client_is_given_server_side_decorations() {
    // Phase `the_decoration_manager_is_advertised`.
    // A toolkit that cannot see the global never asks — it decorates
    // itself, and there is no negotiation to lose.
    {
        let (globals, _) = negotiate(None);
        assert!(
            !globals.is_empty(),
            "the client saw no globals at all — the harness failed to talk to \
             the compositor, so this test proved nothing."
        );
        assert!(
            globals.iter().any(|g| g == "zxdg_decoration_manager_v1"),
            "zxdg_decoration_manager_v1 is not advertised, so every client \
             falls back to drawing its own title bar and keeps it out of the \
             size this compositor gave it. Advertised: {globals:?}"
        );
    }

    // Phase `a_client_that_asks_for_server_side_is_given_server_side`.
    {
        let (globals, modes) = negotiate(Some(zxdg_toplevel_decoration_v1::Mode::ServerSide));
        assert!(
            !modes.is_empty(),
            "the compositor configured no decoration mode at all. Advertised: \
             {globals:?}"
        );
        assert!(
            modes
                .iter()
                .all(|m| *m == zxdg_toplevel_decoration_v1::Mode::ServerSide),
            "expected every configure to say ServerSide; got {modes:?}. A \
             client told ClientSide draws its own title bar inside the tab \
             this compositor already drew one for."
        );
    }

    // Phase `a_client_that_asks_to_decorate_itself_is_still_given_server_side`.
    {
        let (globals, modes) = negotiate(Some(zxdg_toplevel_decoration_v1::Mode::ClientSide));
        assert!(
            !modes.is_empty(),
            "the compositor configured no decoration mode at all. Advertised: \
             {globals:?}"
        );
        assert!(
            !modes.contains(&zxdg_toplevel_decoration_v1::Mode::ClientSide),
            "the compositor agreed to client-side decoration; got {modes:?}. \
             Tilo owns window chrome, so the answer to this request is no."
        );
    }
}
