//! A window closes when someone closes it: the person, or the app itself
//! while it keeps running. A client that goes away with its windows still
//! open (it crashed, was killed, or ended with the session) closed nothing:
//! its tabs keep their places, each as the app's card, which open the app
//! again into them and are saved as the windows a restart reopens. Its
//! surfaces are torn down only after the client is gone, and must still be
//! recognised as the ones its tabs were filed under; a tab left showing the
//! dead surface would show the last frame and take clicks meant for
//! whatever replaced it.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use shell::compositor::state::State;
use shell::shell::state::{TileContent, WorkspaceTree};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{wl_compositor, wl_shm};
use wayland_client::{Connection, EventQueue};
use wayland_protocols::xdg::shell::client::xdg_wm_base;

use crate::support::wayland::{Globals as _, Toplevel, create_toplevel, foreign_tabs, shm_buffer, solid};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
}

crate::support::wayland::test_client!(Client: registry, ping, configure, surface, toplevel, shm, buffer);

/// A compositor with a bystander client and the client under test, which
/// opened one window per name in `app_ids`.
struct Session {
    // Dropping the loop invalidates the handle the compositor state holds.
    _event_loop: EventLoop<'static, State>,
    display: Display<State>,
    state: State,
    conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
    /// The client's end of its socket, to end its connection the way a
    /// process that exits does.
    kill_switch: UnixStream,
    windows: Vec<(String, u64, Toplevel)>,
}

impl Session {
    fn open(app_ids: &[&str]) -> Self {
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let state = State::new_with_workspace_tree(
            event_loop.handle(),
            display.handle(),
            WorkspaceTree::bootstrap_default().shared(),
        );
        // A first client makes the tested one's client index non-zero, as it
        // is in a real session (the id then differs from the bare protocol id).
        let (bystander, _bystander_client) = UnixStream::pair().expect("socketpair");
        display
            .handle()
            .insert_client(bystander, Arc::new(shell::compositor::state::ClientState::default()))
            .expect("insert bystander");
        let (server_side, client_side) = UnixStream::pair().expect("socketpair");
        display
            .handle()
            .insert_client(server_side, Arc::new(shell::compositor::state::ClientState::default()))
            .expect("insert client");
        let kill_switch = client_side.try_clone().expect("clone client socket");
        client_side.set_nonblocking(true).expect("non-blocking");
        let conn = Connection::from_socket(client_side).expect("client connection");
        let queue = conn.new_event_queue::<Client>();
        let mut session = Self {
            _event_loop: event_loop,
            display,
            state,
            conn,
            queue,
            client: Client::default(),
            kill_switch,
            windows: Vec::new(),
        };
        let qh = session.queue.handle();
        let registry = session.conn.display().get_registry(&qh, ());
        session.pump();
        let bind = |client: &Client, interface: &str, version: u32| {
            client
                .global(interface, version)
                .unwrap_or_else(|| panic!("{interface} is advertised"))
        };
        let (name, version) = bind(&session.client, "wl_compositor", 4);
        let compositor: wl_compositor::WlCompositor = registry.bind(name, version, &qh, ());
        let (name, version) = bind(&session.client, "xdg_wm_base", 2);
        let wm_base: xdg_wm_base::XdgWmBase = registry.bind(name, version, &qh, ());
        let (name, version) = bind(&session.client, "wl_shm", 1);
        let shm: wl_shm::WlShm = registry.bind(name, version, &qh, ());
        for app_id in app_ids {
            let window = create_toplevel(&compositor, &wm_base, &qh, app_id);
            window.surface.commit();
            session.pump();
            let tabs = foreign_tabs(&session.state);
            let (_, surface_id) = tabs
                .iter()
                .find(|(id, _)| id == app_id)
                .cloned()
                .unwrap_or_else(|| panic!("{app_id}'s window was adopted into a tab: {tabs:?}"));
            assert_ne!(surface_id >> 32, 0, "the tab is filed under a client-qualified id");
            // The window is shown: configured where layout placed it, and a
            // frame drawn. (A window gone before its first frame is no
            // window anyone saw, and its place is withdrawn.)
            shell::shell::foreign::store()
                .note_placement(surface_id, vello::kurbo::Rect::new(0.0, 0.0, 640.0, 480.0));
            shell::shell::foreign::flush_configures(&mut session.state);
            session.pump();
            let buffer = shm_buffer(&shm, &qh, 64, 64, &solid(64, 64, [0, 0, 0, 255]));
            window.surface.attach(Some(&buffer), 0, 0);
            window.surface.damage(0, 0, 64, 64);
            window.surface.commit();
            session.pump();
            assert!(
                shell::shell::foreign::store().is_mapped(surface_id),
                "{app_id}'s window is shown"
            );
            session.windows.push(((*app_id).to_owned(), surface_id, window));
        }
        session
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

    /// Every tab of `app_id`, as its tile.
    fn tabs_of(&self, app_id: &str) -> Vec<TileContent> {
        self.state
            .workspace_tree
            .read()
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .filter(|tab| match &tab.tile {
                TileContent::Foreign { app_id: id, .. } | TileContent::LocalApp { app_id: id, .. } => {
                    id == app_id
                }
                _ => false,
            })
            .map(|tab| tab.tile.clone())
            .collect()
    }
}

#[test]
fn a_client_that_exits_with_its_windows_open_keeps_their_tabs() {
    let mut session = Session::open(&["org.example.Crashes", "org.example.Crashes.Second"]);
    session
        .kill_switch
        .shutdown(Shutdown::Both)
        .expect("drop the connection");
    session.pump();
    let dead: Vec<u64> = session.windows.iter().map(|(_, id, _)| *id).collect();
    assert!(
        foreign_tabs(&session.state).iter().all(|(_, id)| !dead.contains(id)),
        "no tab shows a surface of the exited client: {:?}",
        foreign_tabs(&session.state)
    );
    for (app_id, _, _) in &session.windows {
        let tabs = session.tabs_of(app_id);
        assert!(
            matches!(tabs.as_slice(), [TileContent::LocalApp { note, .. }] if !note.is_empty()),
            "{app_id}'s window keeps its place as the app's card: {tabs:?}"
        );
    }
}

#[test]
fn a_running_client_that_destroys_a_window_closes_that_tab_only() {
    let mut session = Session::open(&["org.example.Closes", "org.example.Stays"]);
    let (_, _, closing) = &session.windows[0];
    closing.toplevel.destroy();
    closing.xdg.destroy();
    closing.surface.destroy();
    session.pump();
    assert!(
        session.tabs_of("org.example.Closes").is_empty(),
        "the window the app closed takes its tab: {:?}",
        session.tabs_of("org.example.Closes")
    );
    let (_, staying, _) = &session.windows[1];
    assert!(
        foreign_tabs(&session.state).iter().any(|(_, id)| id == staying),
        "the app's other window keeps its live tab: {:?}",
        foreign_tabs(&session.state)
    );
}
