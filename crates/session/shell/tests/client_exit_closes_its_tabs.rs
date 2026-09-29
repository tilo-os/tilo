//! A client that exits without destroying its windows — killed, crashed —
//! takes its tabs with it. Its surfaces are torn down only after the client
//! is gone, and must still be recognised as the ones its tabs were filed
//! under; otherwise the tab stays behind, showing the last frame and taking
//! clicks meant for whatever replaced it.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use shell::shell::state::{TileContent, WorkspaceTree};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
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

fn foreign_tabs(state: &shell::compositor::state::State) -> Vec<(String, u64)> {
    state
        .workspace_tree
        .read()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter_map(|tab| match &tab.tile {
            TileContent::Foreign {
                app_id, surface_id, ..
            } => Some((app_id.clone(), *surface_id)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_client_that_exits_without_destroying_its_window_loses_its_tab() {
    let event_loop: EventLoop<'static, shell::compositor::state::State> =
        EventLoop::try_new().expect("calloop");
    let mut display: Display<shell::compositor::state::State> = Display::new().expect("display");
    let mut state = shell::compositor::state::State::new_with_workspace_tree(
        event_loop.handle(),
        display.handle(),
        WorkspaceTree::bootstrap_default().shared(),
    );
    // A first client makes the exiting one's client index non-zero, as it is
    // in a real session (the id then differs from the bare protocol id).
    let (bystander, _bystander_client) = UnixStream::pair().expect("socketpair");
    display
        .handle()
        .insert_client(
            bystander,
            Arc::new(shell::compositor::state::ClientState::default()),
        )
        .expect("insert bystander");
    let (server_side, client_side) = UnixStream::pair().expect("socketpair");
    display
        .handle()
        .insert_client(
            server_side,
            Arc::new(shell::compositor::state::ClientState::default()),
        )
        .expect("insert client");
    let kill_switch = client_side.try_clone().expect("clone client socket");
    client_side.set_nonblocking(true).expect("non-blocking");
    let conn = Connection::from_socket(client_side).expect("client connection");
    let mut queue = conn.new_event_queue::<Client>();
    let qh = queue.handle();
    let mut client = Client::default();
    let mut pump = |state: &mut shell::compositor::state::State, client: &mut Client| {
        for _ in 0..6 {
            let _ = queue.flush();
            let _ = display.dispatch_clients(state);
            let _ = display.flush_clients();
            if let Some(guard) = conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = queue.dispatch_pending(client);
        }
    };

    let registry = conn.display().get_registry(&qh, ());
    pump(&mut state, &mut client);
    let bind = |interface: &str, version: u32| {
        let (name, _, advertised) = client
            .globals
            .iter()
            .find(|(_, i, _)| i == interface)
            .unwrap_or_else(|| panic!("{interface} is advertised"))
            .clone();
        (name, version.min(advertised))
    };
    let (name, version) = bind("wl_compositor", 4);
    let compositor: wl_compositor::WlCompositor = registry.bind(name, version, &qh, ());
    let (name, version) = bind("xdg_wm_base", 2);
    let wm_base: xdg_wm_base::XdgWmBase = registry.bind(name, version, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Crashes".into());
    surface.commit();
    pump(&mut state, &mut client);
    let tabs = foreign_tabs(&state);
    let (_, surface_id) = tabs
        .iter()
        .find(|(app_id, _)| app_id == "org.example.Crashes")
        .cloned()
        .unwrap_or_else(|| panic!("the window was adopted into a tab: {tabs:?}"));
    assert_ne!(
        surface_id >> 32,
        0,
        "the tab is filed under a client-qualified id"
    );

    kill_switch
        .shutdown(Shutdown::Both)
        .expect("drop the connection");
    pump(&mut state, &mut client);
    assert!(
        foreign_tabs(&state).iter().all(|(_, id)| *id != surface_id),
        "the exited client's tab must close: {:?}",
        foreign_tabs(&state)
    );
    let _keep = (xdg, toplevel);
}
