//! Wire-level presence and rejection tests for `xdg_activation_v1`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    completed_tokens: usize,
    token: Option<String>,
    keyboard_serial: Option<u32>,
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

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_activation_token_v1::XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.completed_tokens += 1;
            state.token = Some(token);
        }
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
        if let wl_keyboard::Event::Enter { serial, .. } = event {
            state.keyboard_serial = Some(serial);
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

delegate_noop!(Client: ignore xdg_activation_v1::XdgActivationV1);
delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

struct Harness {
    display: Display<shell::compositor::state::State>,
    state: shell::compositor::state::State,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    _event_loop: EventLoop<'static, shell::compositor::state::State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let state = shell::compositor::state::State::new(event_loop.handle(), display.handle());
        let (server, client) = UnixStream::pair().expect("socket pair");
        display
            .handle()
            .insert_client(
                server,
                Arc::new(shell::compositor::state::ClientState::default()),
            )
            .expect("client insert");
        client.set_nonblocking(true).expect("nonblocking");
        let conn = Connection::from_socket(client).expect("client connection");
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
            .find(|(_, name, _)| name == interface)?
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        Some(registry.bind(name, version.min(advertised), &self.queue.handle(), ()))
    }
}

fn foreign_id(state: &shell::compositor::state::State, app_id: &str) -> u64 {
    state
        .workspace_tree
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| match &tab.tile {
            shell::shell::state::TileContent::Foreign {
                app_id: actual,
                surface_id,
            } if actual == app_id => Some(*surface_id),
            _ => None,
        })
        .expect("foreign surface was adopted")
}

#[test]
fn activation_global_is_advertised_but_unattributed_tokens_are_not_authority() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let manager = h
        .bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1)
        .expect("xdg_activation_v1 must be advertised");
    let token = manager.get_activation_token(&h.queue.handle(), ());
    token.commit();
    h.pump();

    assert_eq!(
        h.client.completed_tokens, 1,
        "protocol commit must complete"
    );
    assert_eq!(
        h.state.xdg_activation_state.tokens().count(),
        0,
        "a token without seat, serial, and focused source must not enter the usable token store"
    );
}

#[test]
fn focused_serial_activates_matching_toplevel_once() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let activation = h
        .bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1)
        .expect("activation");
    let compositor = h
        .bind::<wl_compositor::WlCompositor>("wl_compositor", 4)
        .expect("compositor");
    let wm = h
        .bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1)
        .expect("xdg shell");
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat");
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let source = compositor.create_surface(&qh, ());
    let source_xdg = wm.get_xdg_surface(&source, &qh, ());
    let source_top = source_xdg.get_toplevel(&qh, ());
    source_top.set_app_id("org.example.Source".into());
    let target = compositor.create_surface(&qh, ());
    let target_xdg = wm.get_xdg_surface(&target, &qh, ());
    let target_top = target_xdg.get_toplevel(&qh, ());
    target_top.set_app_id("org.example.Target".into());
    h.pump();

    let source_id = foreign_id(&h.state, "org.example.Source");
    let target_id = foreign_id(&h.state, "org.example.Target");
    assert!(shell::compositor::tab_binding::activate_surface(
        &mut h.state.workspace_tree,
        source_id
    ));
    h.state.set_client_keyboard_focus(Some(source_id));
    h.pump();
    let serial = h.client.keyboard_serial.expect("keyboard enter serial");

    let token = activation.get_activation_token(&qh, ());
    token.set_serial(serial, &seat);
    token.set_surface(&source);
    token.set_app_id("org.example.Target".into());
    token.commit();
    h.pump();
    let token_text = h.client.token.clone().expect("activation token");
    assert_eq!(h.state.xdg_activation_state.tokens().count(), 1);

    activation.activate(token_text.clone(), &target);
    h.pump();
    assert_eq!(
        shell::compositor::client_input::active_foreign_surface(&h.state.workspace_tree),
        Some(target_id)
    );
    assert_eq!(h.state.client_input.keyboard_focus(), Some(target_id));
    assert_eq!(h.state.xdg_activation_state.tokens().count(), 0);

    assert!(shell::compositor::tab_binding::activate_surface(
        &mut h.state.workspace_tree,
        source_id
    ));
    activation.activate(token_text, &target);
    h.pump();
    assert_eq!(
        shell::compositor::client_input::active_foreign_surface(&h.state.workspace_tree),
        Some(source_id),
        "a consumed token must not be replayable"
    );
}
