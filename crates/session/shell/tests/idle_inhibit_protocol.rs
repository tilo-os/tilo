//! A video player keeping the screen on through `zwp_idle_inhibit_manager_v1`.
//!
//! The request holds the session's idle inhibitor registry — what keeps the
//! screen from blanking and locking — only while the window asking is on
//! screen, and lets go when the window is hidden, the inhibitor destroyed or
//! the client gone. Reads a real socket.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use shell::compositor::frame::FrameBuilder;
use shell::shell::state::{TileContent, Workspace, WorkspaceId, WorkspaceTree};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use tilo_power_mgmt::InhibitRegistry;
use tilo_power_mgmt::inhibit::INHIBIT_IDLE;
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1, zwp_idle_inhibitor_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
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
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1);
delegate_noop!(Client: ignore zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1);

struct Harness {
    display: Display<shell::compositor::state::State>,
    state: shell::compositor::state::State,
    frame: FrameBuilder,
    registry: InhibitRegistry,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    _event_loop: EventLoop<'static, shell::compositor::state::State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop: EventLoop<'static, shell::compositor::state::State> =
            EventLoop::try_new().expect("calloop");
        let display: Display<shell::compositor::state::State> = Display::new().expect("display");
        let tree = WorkspaceTree::bootstrap_default().shared();
        let state = shell::compositor::state::State::new_with_workspace_tree(
            event_loop.handle(),
            display.handle(),
            tree.clone(),
        );
        let frame = FrameBuilder::with_handle(tree, false, (1600, 900));
        let (server_side, client_side) = UnixStream::pair().expect("socketpair");
        display
            .handle()
            .insert_client(
                server_side,
                Arc::new(shell::compositor::state::ClientState::default()),
            )
            .expect("insert client");
        client_side.set_nonblocking(true).expect("non-blocking");
        let conn = Connection::from_socket(client_side).expect("client connection");
        let queue = conn.new_event_queue::<Client>();
        Self {
            display,
            state,
            frame,
            registry: InhibitRegistry::new(),
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

    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .globals
            .iter()
            .find(|(_, i, _)| i == interface)
            .unwrap_or_else(|| panic!("{interface} is advertised"))
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        registry.bind::<I, (), Client>(name, version.min(advertised), &self.queue.handle(), ())
    }

    /// The per-tick pass the session loop runs.
    fn tick(&mut self) {
        self.pump();
        shell::compositor::idle_inhibit::sync_idle_inhibitors(
            &mut self.state,
            &self.frame,
            &self.registry,
        );
    }

    fn adopted(&self, app_id: &str) -> bool {
        self.state
            .workspace_tree
            .read()
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .any(|tab| matches!(&tab.tile, TileContent::Foreign { app_id: a, .. } if a == app_id))
    }

    fn show_workspace(&mut self, id: WorkspaceId) {
        self.frame.with_chrome(|chrome| {
            {
                let mut tree = chrome.tree_mut();
                if !tree.desktop.workspaces.iter().any(|ws| ws.id == id) {
                    tree.desktop
                        .workspaces
                        .push(Workspace::new(id, "Elsewhere"));
                }
                tree.desktop.active = id;
            }
            chrome.bump();
        });
    }
}

#[test]
fn an_inhibitor_holds_the_screen_on_only_while_its_window_is_visible() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    let manager: zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1 =
        h.bind("zwp_idle_inhibit_manager_v1", 1);
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Videos".into());
    surface.commit();
    h.pump();
    assert!(h.adopted("org.example.Videos"));
    let home = h.frame.chrome().tree().desktop.active;

    h.tick();
    assert!(
        !h.registry.is_inhibited_for(INHIBIT_IDLE),
        "nothing asked yet, so nothing may hold the screen on"
    );

    let inhibitor = manager.create_inhibitor(&surface, &qh, ());
    h.tick();
    assert!(
        h.registry.is_inhibited_for(INHIBIT_IDLE),
        "a visible window's inhibitor must keep the screen from blanking and locking"
    );
    assert_eq!(h.registry.active_reasons_for(INHIBIT_IDLE).len(), 1);

    h.show_workspace(WorkspaceId(9_001));
    h.tick();
    assert!(
        !h.registry.is_inhibited_for(INHIBIT_IDLE),
        "a video nobody can see is no reason to keep the session unlocked"
    );

    h.show_workspace(home);
    h.tick();
    assert!(h.registry.is_inhibited_for(INHIBIT_IDLE));

    inhibitor.destroy();
    h.tick();
    let still_held = h.registry.is_inhibited_for(INHIBIT_IDLE);
    drop(h);
    assert!(
        !still_held,
        "destroying the inhibitor must release the hold"
    );
    let _keep = (toplevel, xdg);
}
