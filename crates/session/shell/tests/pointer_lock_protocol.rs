//! A game locks the pointer over the wire and gets relative motion only;
//! the compositor withdraws the lock when the game loses focus and when the
//! person presses the release chord, and does not hand it back until focus
//! has moved on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{wl_compositor, wl_pointer, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1, zwp_pointer_constraints_v1,
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use shell::compositor::backend::PointerConstraintRegion;
use shell::compositor::client_input::sync_pointer_constraint;
use shell::compositor::frame::FrameBuilder;
use shell::compositor::state::State;

#[derive(Debug, PartialEq)]
enum Wire {
    Locked,
    Unlocked,
    Motion,
    Relative { dx: f64, dy: f64, ux: f64, uy: f64 },
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    wire: Vec<Wire>,
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

impl Dispatch<wl_pointer::WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Motion { .. } = event {
            state.wire.push(Wire::Motion);
        }
    }
}

impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_locked_pointer_v1::ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => state.wire.push(Wire::Locked),
            zwp_locked_pointer_v1::Event::Unlocked => state.wire.push(Wire::Unlocked),
            _ => {}
        }
    }
}

impl Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
            ..
        } = event
        {
            state.wire.push(Wire::Relative {
                dx,
                dy,
                ux: dx_unaccel,
                uy: dy_unaccel,
            });
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

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zwp_pointer_constraints_v1::ZwpPointerConstraintsV1);
delegate_noop!(Client: ignore zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1);

struct Harness {
    display: Display<State>,
    state: State,
    fb: FrameBuilder,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    _event_loop: EventLoop<'static, State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let state = State::new(event_loop.handle(), display.handle());
        // The chrome reads the very tree protocol callbacks adopt into.
        let fb = FrameBuilder::with_handle(state.workspace_tree.clone(), false, (1280, 800));
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
        let mut harness = Self {
            display,
            state,
            fb,
            queue,
            client: Client::default(),
            conn,
            _event_loop: event_loop,
        };
        let _registry = harness
            .conn
            .display()
            .get_registry(&harness.queue.handle(), ());
        harness.pump();
        harness
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

    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .unwrap_or_else(|| panic!("{interface} is advertised"))
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        registry.bind(name, version.min(advertised), &self.queue.handle(), ())
    }

    fn foreign_id(&self, app_id: &str) -> u64 {
        self.state
            .workspace_tree
            .read()
            .desktop
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

    /// One compositor tick's worth of constraint policy.
    fn tick(&mut self) -> Option<shell::compositor::pointer_lock::ConstraintChange> {
        let change = sync_pointer_constraint(&mut self.state, &self.fb, false);
        self.pump();
        change
    }
}

#[test]
fn a_locked_game_gets_relative_motion_only_and_loses_the_lock_with_focus() {
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let constraints = h.bind::<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>(
        "zwp_pointer_constraints_v1",
        1,
    );
    let relative = h.bind::<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>(
        "zwp_relative_pointer_manager_v1",
        1,
    );
    let qh = h.queue.handle();
    let pointer = seat.get_pointer(&qh, ());
    let _relative_pointer = relative.get_relative_pointer(&pointer, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Game".into());
    h.pump();
    let _ = h.fb.paint_settled();
    let game = h.foreign_id("org.example.Game");

    let _lock = constraints.lock_pointer(
        &surface,
        &pointer,
        None,
        zwp_pointer_constraints_v1::Lifetime::Persistent,
        &qh,
        (),
    );
    h.pump();
    assert_eq!(h.tick(), None, "nothing is granted to an unfocused surface");

    // Focus the game and put the local pointer over it.
    h.state.set_client_keyboard_focus(Some(game));
    h.state
        .forward_client_pointer_motion((400.0, 400.0), Some((game, (0.0, 0.0))));
    h.pump();
    h.client.wire.clear();
    let change = h
        .tick()
        .expect("the focused game under the pointer gets its lock");
    assert!(change.engaged, "the release hint is shown on grant");
    assert_eq!(change.region, PointerConstraintRegion::Locked);
    assert_eq!(h.client.wire, vec![Wire::Locked]);
    assert_eq!(h.tick(), None, "a held lock is not re-announced");
    h.client.wire.clear();

    // Physical motion: the absolute position is frozen, deltas flow.
    h.state
        .forward_client_pointer_motion((450.0, 420.0), Some((game, (0.0, 0.0))));
    assert!(
        h.state
            .forward_client_relative_motion((5.0, -2.0), (4.0, -1.5), 1_000)
    );
    h.pump();
    assert_eq!(
        h.client.wire,
        vec![Wire::Relative {
            dx: 5.0,
            dy: -2.0,
            ux: 4.0,
            uy: -1.5
        }],
        "a locked pointer reports relative motion only"
    );
    h.client.wire.clear();

    // Focus moves to the shell: the lock goes with it.
    h.state.set_client_keyboard_focus(None);
    h.pump();
    let change = h.tick().expect("focus loss withdraws the lock");
    assert_eq!(change.region, PointerConstraintRegion::Free);
    assert!(h.client.wire.contains(&Wire::Unlocked));
    h.client.wire.clear();

    // Back to the game: a persistent lock is granted again.
    h.state.set_client_keyboard_focus(Some(game));
    h.pump();
    assert!(h.tick().is_some_and(|change| change.engaged));
    assert_eq!(h.client.wire, vec![Wire::Locked]);
    h.client.wire.clear();

    // The release chord ends it and keeps it off while focus stays put.
    let change = h
        .state
        .release_pointer_constraint()
        .expect("the release chord frees a held pointer");
    assert_eq!(change.region, PointerConstraintRegion::Free);
    h.pump();
    assert_eq!(h.client.wire, vec![Wire::Unlocked]);
    assert_eq!(
        h.tick(),
        None,
        "a released game does not take the pointer straight back"
    );
    let engaged = h.state.pointer_lock.is_engaged();
    drop(h);
    assert!(!engaged);
}
