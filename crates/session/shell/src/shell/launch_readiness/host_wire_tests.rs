//! Real Wayland adoption into the ordinary owner projection, without GTK or GPU.
use super::*;
use crate::compositor::state::{ClientState, State};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use std::os::unix::net::UnixStream;
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct Client(Vec<(u32, String, u32)>);
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
            state.0.push((name, interface, version));
        }
    }
}
delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore xdg_surface::XdgSurface);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore xdg_wm_base::XdgWmBase);

struct Wire {
    display: Display<State>,
    state: State,
    connection: Connection,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    _events: EventLoop<'static, State>,
}
impl Wire {
    fn new(room: crate::shell::state::RoomId) -> Self {
        let events = EventLoop::try_new().unwrap();
        let display = Display::new().unwrap();
        let state = State::new(events.handle(), display.handle());
        state.workspace_tree.write().workspaces[0].room = room;
        let (server, client) = UnixStream::pair().unwrap();
        let pid = std::process::id();
        display
            .handle()
            .insert_client(
                server,
                Arc::new(ClientState {
                    package_id: Some("org.gnome.TextEditor.desktop".into()),
                    workspace_id: Some(room.to_string()),
                    pid: Some(pid),
                    pid_start_time: super::super::launcher::proc_stat(pid).map(|(_, start)| start),
                    ..ClientState::default()
                }),
            )
            .unwrap();
        client.set_nonblocking(true).unwrap();
        let connection = Connection::from_socket(client).unwrap();
        let queue = connection.new_event_queue::<Client>();
        Self {
            display,
            state,
            connection,
            queue,
            client: Client::default(),
            _events: events,
        }
    }
    fn pump(&mut self) {
        for _ in 0..6 {
            self.queue.flush().unwrap();
            self.display.dispatch_clients(&mut self.state).unwrap();
            self.display.flush_clients().unwrap();
            if let Some(guard) = self.connection.prepare_read() {
                let _ = guard.read();
            }
            self.queue.dispatch_pending(&mut self.client).unwrap();
        }
    }
    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .0
            .iter()
            .find(|(_, name, _)| name == interface)
            .unwrap();
        let registry = self
            .connection
            .display()
            .get_registry(&self.queue.handle(), ());
        registry.bind(*name, version.min(*advertised), &self.queue.handle(), ())
    }
}

#[tokio::test]
async fn gtk_wire_adoption_proves_original_launch_and_rejects_moved_or_closed_surface() {
    let session = super::super::launcher::LaunchSession::current();
    let room: crate::shell::state::RoomId = uuid::Uuid::new_v4().into();
    let parent = tilo_actions::InvocationId(uuid::Uuid::new_v4());
    let mut wire = Wire::new(room);
    let _registry = wire
        .connection
        .display()
        .get_registry(&wire.queue.handle(), ());
    wire.pump();
    let compositor = wire.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = wire.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    assert_eq!(
        wire.state.host_window_provenance_json()["windows"],
        serde_json::json!([])
    );
    let (token, ready) = session.startup.reserve_with_parent(Some(parent)).unwrap();
    let pid = std::process::id();
    session.startup.started(Some(token), pid);
    session.startup.host_started(
        Some(token),
        "org.gnome.TextEditor.desktop",
        &room.to_string(),
    );
    let surface = compositor.create_surface(&wire.queue.handle(), ());
    let xdg = wm.get_xdg_surface(&surface, &wire.queue.handle(), ());
    let top = xdg.get_toplevel(&wire.queue.handle(), ());
    top.set_app_id("untrusted-window-label".into());
    top.set_title("private title never exported".into());
    wire.pump();
    assert_eq!(ready.wait_with_adoption().await.unwrap(), None);
    let proof = wire.state.host_window_provenance_json();
    assert_eq!(proof["windows"].as_array().unwrap().len(), 1);
    assert_eq!(
        proof["windows"][0]["invocation"],
        serde_json::to_value(parent).unwrap()
    );
    assert_eq!(proof["windows"][0]["peer_pid"], pid);
    assert_eq!(
        proof["windows"][0]["desktop_id"],
        "org.gnome.TextEditor.desktop"
    );
    assert!(!proof.to_string().contains("private title"));
    assert!(!proof.to_string().contains("untrusted-window-label"));
    assert_eq!(
        wire.state.native_window_provenance_json()["windows"],
        serde_json::json!([])
    );
    wire.state.workspace_tree.write().workspaces[0].room = uuid::Uuid::new_v4().into();
    assert_eq!(
        wire.state.host_window_provenance_json()["windows"],
        serde_json::json!([])
    );
    wire.state.workspace_tree.write().workspaces[0].room = room;
    assert_eq!(wire.state.host_window_provenance_json(), proof);
    top.destroy();
    xdg.destroy();
    surface.destroy();
    wire.pump();
    assert_eq!(
        wire.state.host_window_provenance_json()["windows"],
        serde_json::json!([])
    );
    let replacement = compositor.create_surface(&wire.queue.handle(), ());
    let replacement_xdg = wm.get_xdg_surface(&replacement, &wire.queue.handle(), ());
    let replacement_top = replacement_xdg.get_toplevel(&wire.queue.handle(), ());
    replacement_top.set_app_id("org.gnome.TextEditor".into());
    wire.pump();
    assert_eq!(
        wire.state.host_window_provenance_json()["windows"],
        serde_json::json!([])
    );
    replacement_top.destroy();
    replacement_xdg.destroy();
    replacement.destroy();
    wire.pump();
    assert!(crate::shell::test_control::Command::StateDump.admin_permitted());
    assert!(!crate::shell::test_control::Command::StateDump.agent_permitted());
}
