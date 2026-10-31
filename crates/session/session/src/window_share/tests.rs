//! Two sessions in one room: one shares a window of its desk, the other
//! shows it and reaches into it, and the bar on the sharing device says so.
//! The two planes have no transport between them, so the test carries what
//! one plane would have said to the other, to the members each message was
//! for.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the sessions outlive every call"
)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use serde_json::Value;
use tilo_actions::ResourceSpec;
use tilo_compositor::{Compositor, OutputConfig};
use tilo_desktop::desktop_actions::WINDOW_KIND;
use tilo_desktop_model::{
    DesktopTree, GroupId, RoomId, Strip, StripId, Tab, TabId, TileContent, WindowGroup,
    WindowWidth, Workspace, WorkspaceId,
};
use tilo_endpoint_reachability::Reachability;
use tilo_network_actor::{NetEvent, WorkspaceId as Topic};
use tilo_window_share::streaming::InputEvent as SharedInput;
use tokio::sync::mpsc::UnboundedReceiver;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_pointer, wl_pointer::WlPointer,
    wl_registry, wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

use super::SHARES_RESOURCE;
use crate::sync::{Client, Outgoing, OwnPresence};
use crate::{Options, Session};

type Arguments = serde_json::Map<String, Value>;

const ROOM: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
const APP: &str = "so.tilo.notes";
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
fn device(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}

fn room() -> RoomId {
    RoomId::parse(ROOM).expect("a room id")
}

/// A desk with one workspace in the room, its one tab showing `tile`: what a
/// session shares a window of, or shows another member's window in.
fn desk(tile: TileContent, tab: u64) -> DesktopTree {
    let room = room();
    let id = WorkspaceId::of_room(room);
    let mut workspace = Workspace::in_room(id, room, "Desk");
    let mut strip = Strip::new(StripId::from(1), "Main");
    strip.window_groups.push(WindowGroup {
        id: GroupId::from(1_u64),
        tabs: vec![Tab {
            id: TabId::from(tab),
            title: "Notes".to_owned(),
            tile,
            app_title: None,
        }],
        active_tab: TabId::from(tab),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    workspace.strips.push(strip);
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.id,
        ..DesktopTree::default()
    }
}

/// A desk whose one visible window is this app's own.
fn own_desk() -> DesktopTree {
    desk(
        TileContent::LocalApp {
            app_id: APP.to_owned(),
            label: "Notes".to_owned(),
            note: String::new(),
        },
        1,
    )
}

/// A desk showing another device's window of `app_id` in `tab`: the tab the
/// host shows that window in, as the room's shared layout names it.
fn showing(host: EndpointId, app_id: &str, tab: u64) -> DesktopTree {
    desk(
        TileContent::HostedWindow {
            app_id: app_id.to_owned(),
            label: "Notes".to_owned(),
            host: hex::encode(host.as_bytes()),
            host_label: "Framework".to_owned(),
        },
        tab,
    )
}

/// One device of the room: its session, its plane, and the messages its
/// plane would have sent.
struct Member {
    session: Session,
    sync: Rc<Client>,
    said: UnboundedReceiver<Outgoing>,
}

fn member(seed: u8, desk: DesktopTree) -> Member {
    let (sync, said) = Client::planeless(
        device(seed),
        OwnPresence {
            label: format!("Device {seed:x}"),
            ..OwnPresence::default()
        },
        Vec::new(),
        Reachability::default(),
    );
    let sync = Rc::new(sync);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            sync: Some(Rc::clone(&sync)),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.handle().replace_tree(desk);
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    Member {
        session,
        sync,
        said,
    }
}

/// One message `from` said, as the room's topic would have carried it.
fn said(from: &Member, ws: Topic, msg: NeighbourMsg) -> NetEvent {
    NetEvent::Msg {
        ws,
        peer: from.sync.publisher(),
        membership_epoch: None,
        replayed: false,
        msg,
    }
}

/// Hand what `from` would have sent on the plane to `to`: a window's stream
/// to the members it was for, and a control message to the device it named,
/// and nothing to anyone else.
fn carry(from: &mut Member, to: &Member) {
    while let Ok(outgoing) = from.said.try_recv() {
        match outgoing {
            Outgoing::PublishSurfaceTo { ws, msg, viewers } => {
                if viewers.contains(&to.sync.peer()) {
                    to.sync.hear(said(from, ws, *msg));
                }
            }
            Outgoing::SendControl { ws, msg, target } if target == to.sync.publisher() => {
                to.sync.hear(said(from, ws, *msg));
            }
            _ => {}
        }
    }
}

/// Step both sessions, and the client whose window `a` shares, until `done`
/// holds of the two members and the client, carrying what the planes said.
#[track_caller]
fn until(
    a: &mut Member,
    client: &mut Option<TestClient>,
    b: &mut Member,
    mut done: impl FnMut(&Member, &Member, &Option<TestClient>) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(client) = client.as_mut() {
            client.exchange();
        }
        a.session.step(Duration::from_millis(2)).expect("step");
        if let Some(client) = client.as_mut() {
            client.exchange();
        }
        b.session.step(Duration::from_millis(2)).expect("step");
        carry(a, b);
        carry(b, a);
        if done(a, b, client) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "not reached in time:\n  a: {}\n  b: {}",
            shares(&a.session),
            shares(&b.session)
        );
    }
}

/// Step both sessions a few times without asking for anything.
fn settle(a: &mut Member, client: &mut Option<TestClient>, b: &mut Member) {
    for _ in 0..8 {
        if let Some(client) = client.as_mut() {
            client.exchange();
        }
        a.session.step(Duration::from_millis(2)).expect("step");
        b.session.step(Duration::from_millis(2)).expect("step");
        carry(a, b);
        carry(b, a);
    }
}

fn arguments(value: Value) -> Arguments {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// What the shares resource says on `session`.
fn shares(session: &Session) -> Value {
    session
        .read(SHARES_RESOURCE)
        .expect("the shares resource is published")
}

/// The rows of `array` in what the shares resource says.
fn rows<'a>(reading: &'a Value, array: &str) -> &'a [Value] {
    reading[array]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// The tab of the desk the window of `app` became.
fn tab_of(session: &Session, app: &str) -> u64 {
    fn find(resources: &[ResourceSpec], app: &str) -> Option<u64> {
        resources.iter().find_map(|resource| {
            if resource.kind == WINDOW_KIND && resource.label == app {
                resource
                    .id
                    .0
                    .rsplit(':')
                    .next()
                    .and_then(|tab| tab.parse().ok())
            } else {
                find(&resource.children, app)
            }
        })
    }
    find(&session.resources(), app).unwrap_or_else(|| panic!("no window of {app} on the desk"))
}

/// The compositor's identifier of the window of `app`, as a share names it.
fn identifier_of(compositor: &Compositor, app: &str) -> String {
    compositor
        .shareable_windows()
        .into_iter()
        .find(|(_, app_id, _)| app_id.as_deref() == Some(app))
        .map(|(identifier, _, _)| identifier)
        .expect("the window is shareable")
}

/// Share the window of `app` as `tab` of the room.
fn share(member: &mut Member, app: &str, tab: u64) {
    let window = identifier_of(member.session.compositor(), app);
    member
        .session
        .invoke(
            SHARES_RESOURCE,
            "share",
            &arguments(serde_json::json!({
                "window": window,
                "tab": tab,
                "app_id": app,
                "room": ROOM,
            })),
        )
        .expect("the window is shared with the room");
}

/// Show the window another member shares, in `tab` of the room.
fn watch(member: &mut Member, tab: u64) {
    member
        .session
        .invoke(
            SHARES_RESOURCE,
            "watch",
            &arguments(serde_json::json!({ "tab": tab, "room": ROOM })),
        )
        .expect("the window is shown on the desk");
}

/// Let the members viewing tab `tab` control it, or stop them.
fn allow_input(member: &mut Member, tab: u64, on: bool) {
    member
        .session
        .invoke(
            SHARES_RESOURCE,
            "allow-remote-input",
            &arguments(serde_json::json!({ "tab": tab, "on": on })),
        )
        .expect("the members viewing it are told");
}

/// Move the pointer over the window of `surface` on `host`, as a member
/// viewing it does.
fn move_pointer(viewer: &Member, host: &Member, surface: u64, x: f64, y: f64) {
    let event = postcard::to_stdvec(&SharedInput::PointerMotion { x, y }).expect("input");
    let ws = host.sync.room_topic(ROOM).expect("the room's topic");
    host.sync.hear(NetEvent::Msg {
        ws,
        peer: viewer.sync.publisher(),
        membership_epoch: None,
        replayed: false,
        msg: NeighbourMsg::RemoteInput {
            peer: viewer.sync.peer(),
            surface,
            event,
        },
    });
}

/// A client with one toplevel of a solid colour: a person's window on a
/// session's compositor.
struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: Option<WlSurface>,
    toplevel: Option<XdgToplevel>,
}

#[derive(Default)]
struct Seen {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    #[allow(dead_code, reason = "held so the seat keeps its pointer")]
    pointer: Option<WlPointer>,
    configured: bool,
    size: Option<(i32, i32)>,
    pointer_inside: bool,
    motions: Vec<(f64, f64)>,
    closed: bool,
}

impl TestClient {
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

    fn open_window(&mut self, app_id: &str) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        surface.commit();
        self.surface = Some(surface);
        self.toplevel = Some(toplevel);
    }

    /// Close the window: its toplevel goes, its surface stays.
    fn close_window(&mut self) {
        if let Some(toplevel) = self.toplevel.take() {
            toplevel.destroy();
        }
    }

    /// Attach a buffer of one colour (RGBA) and commit it.
    fn show(&self, width: i32, height: i32, rgba: [u8; 4]) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).expect("a window of a size");
        let fd =
            rustix::fs::memfd_create("tilo-window-share-test", rustix::fs::MemfdFlags::CLOEXEC)
                .expect("memfd");
        let mut file = std::fs::File::from(fd);
        // ARGB8888 is B, G, R, A in memory.
        let pixel = [rgba[2], rgba[1], rgba[0], rgba[3]];
        file.write_all(&pixel.repeat(len / 4)).expect("pixels");
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
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
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

impl Dispatch<XdgToplevel, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Close => seen.closed = true,
            xdg_toplevel::Event::Configure { width, height, .. } => {
                seen.size = Some((width, height));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for Seen {
    fn event(
        seen: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Pointer)
            && seen.pointer.is_none()
        {
            seen.pointer = Some(seat.get_pointer(qh, ()));
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { .. } => seen.pointer_inside = true,
            wl_pointer::Event::Leave { .. } => seen.pointer_inside = false,
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => seen.motions.push((surface_x, surface_y)),
            _ => {}
        }
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);

/// Open a window of `app_id` on `session`, at the size it was configured to,
/// and wait until the desk shows it.
#[track_caller]
fn mapped(session: &mut Session, app_id: &str) -> TestClient {
    let mut client = TestClient::connect(session.compositor());
    until_window(session, &mut client, |_, seen| {
        seen.wm_base.is_some() && seen.seat.is_some()
    });
    client.open_window(app_id);
    until_window(session, &mut client, |_, seen| seen.configured);
    let (w, h) = client
        .seen
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((300, 200));
    client.show(w, h, [200, 40, 40, 255]);
    let app = app_id.to_owned();
    until_window(session, &mut client, move |session, _| {
        session
            .compositor()
            .windows()
            .iter()
            .any(|(info, _)| info.app_id.as_deref() == Some(app.as_str()))
    });
    client
}

/// Step `session` with `client` until `done` holds of what the client saw.
#[track_caller]
fn until_window(
    session: &mut Session,
    client: &mut TestClient,
    mut done: impl FnMut(&mut Session, &Seen) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        client.exchange();
        session.step(Duration::from_millis(2)).expect("step");
        client.exchange();
        if done(session, &client.seen) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the client's window did not show"
        );
    }
}

/// One session shares a window of its desk and the other shows it: what the
/// viewing member reads is the stream itself, from a keyframe on, and the
/// sharing device's bar names the member watching it.
#[test]
fn a_shared_window_reaches_the_member_showing_it() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut host = member(0xa0, own_desk());
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    assert!(
        host.session
            .shared
            .window_shares
            .borrow()
            .exposures()
            .is_empty(),
        "nothing is shared before the person shares a window"
    );
    share(&mut host, APP, tab);

    let mut viewer = member(0xb0, showing(host.sync.publisher(), APP, tab));
    assert!(
        shares(&viewer.session)["received"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "nothing reached the desk before the member showed the window"
    );
    watch(&mut viewer, tab);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        !rows(&shares(&host.session), "sharing").is_empty()
    });
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        rows(&shares(&viewer.session), "received")
            .first()
            .and_then(|stream| stream["units"].as_array())
            .is_some_and(|units| !units.is_empty())
    });

    let reading = shares(&viewer.session);
    let received = rows(&reading, "received");
    let stream = received.first().expect("the shared window arrived");
    assert_eq!(
        stream["app_id"].as_str(),
        Some(APP),
        "what arrived: {reading}"
    );
    assert_eq!(
        stream["from"].as_u64(),
        Some(neighbour_wire::peer_handle(
            host.sync.publisher().as_bytes()
        )),
        "the member that shares it: {reading}"
    );
    assert!(
        stream["codec"]
            .as_str()
            .is_some_and(|codec| !codec.is_empty()),
        "the stream says how it is encoded: {reading}"
    );
    assert!(
        stream["keyframe"].as_u64().is_some(),
        "the stream starts from a keyframe a decoder can begin at: {reading}"
    );
    assert!(
        stream["width"].as_u64().unwrap_or(0) > 0 && stream["height"].as_u64().unwrap_or(0) > 0,
        "the stream says how big the window is: {reading}"
    );

    let reading = shares(&host.session);
    let shared = rows(&reading, "sharing")
        .first()
        .expect("the window is shared")
        .clone();
    assert_eq!(shared["tab"].as_u64(), Some(tab), "which window: {reading}");
    let watching = shared["viewers"].as_array().expect("who is watching");
    assert_eq!(watching.len(), 1, "one member watches it: {reading}");
    assert_eq!(
        watching[0]["peer"].as_u64(),
        Some(viewer.sync.peer()),
        "the member showing it: {reading}"
    );
    assert_eq!(
        watching[0]["can_control"].as_bool(),
        Some(false),
        "a share is read only until the person allows control: {reading}"
    );

    let exposures = host.session.shared.window_shares.borrow().exposures();
    assert_eq!(
        exposures,
        vec![tilo_desktop::chrome::ActiveExposure {
            peer: viewer.sync.peer(),
            surface: tab,
            can_control: false,
        }],
        "the bar's sharing indicator says who is watching what"
    );
}

/// A member's view of a shared window ends when the window closes.
#[test]
fn closing_the_shared_window_ends_its_stream() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut host = member(0xc0, own_desk());
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    share(&mut host, APP, tab);
    let mut viewer = member(0xd0, showing(host.sync.publisher(), APP, tab));
    watch(&mut viewer, tab);
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        !rows(&shares(&viewer.session), "received").is_empty()
    });

    window.as_mut().expect("client").close_window();
    until(&mut host, &mut window, &mut viewer, |host, viewer, _| {
        rows(&shares(&host.session), "sharing").is_empty()
            && rows(&shares(&viewer.session), "received").is_empty()
    });
    assert!(
        host.session
            .shared
            .window_shares
            .borrow()
            .exposures()
            .is_empty(),
        "the bar says nothing is shared any more"
    );
}

/// The member showing a shared window reaches into it only while the person
/// lets it, and the cut chord takes that away at once.
#[test]
fn remote_input_reaches_the_window_until_the_cut_chord() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut host = member(0xe0, own_desk());
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    share(&mut host, APP, tab);
    let mut viewer = member(0xf0, showing(host.sync.publisher(), APP, tab));
    watch(&mut viewer, tab);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        !rows(&shares(&host.session), "sharing").is_empty()
    });

    // The member shows the window, read only until the person lets it in.
    until(&mut host, &mut window, &mut viewer, |host, viewer, _| {
        rows(&shares(&host.session), "sharing")
            .first()
            .and_then(|shared| shared["viewers"].as_array())
            .is_some_and(|viewers| {
                viewers
                    .iter()
                    .any(|watcher| watcher["peer"].as_u64() == Some(viewer.sync.peer()))
            })
    });
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        !rows(&shares(&viewer.session), "received").is_empty()
    });
    // The member's pointer is in the stream's own pixels: the middle of the
    // picture the host sends.
    let centre = {
        let reading = shares(&viewer.session);
        let stream = rows(&reading, "received")
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("the shared window arrived: {reading}"));
        let side = |name: &str| stream[name].as_f64().unwrap_or(0.0);
        (side("width") / 2.0, side("height") / 2.0)
    };

    move_pointer(&viewer, &host, tab, centre.0, centre.1);
    settle(&mut host, &mut window, &mut viewer);
    assert!(
        window
            .as_ref()
            .is_some_and(|window| window.seen.motions.is_empty()),
        "a member the person did not let in moves nothing"
    );

    allow_input(&mut host, tab, true);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        rows(&shares(&host.session), "sharing")
            .first()
            .and_then(|shared| shared["viewers"].as_array())
            .is_some_and(|viewers| {
                viewers
                    .iter()
                    .any(|viewer| viewer["can_control"].as_bool() == Some(true))
            })
    });
    move_pointer(&viewer, &host, tab, centre.0, centre.1);
    until(&mut host, &mut window, &mut viewer, |_, _, client| {
        client
            .as_ref()
            .is_some_and(|client| client.seen.pointer_inside)
    });
    assert!(
        window
            .as_ref()
            .is_some_and(|window| window.seen.pointer_inside),
        "the member's pointer reached the window"
    );
    // Once it is over the window, moving it moves the window's own pointer.
    move_pointer(&viewer, &host, tab, centre.0 + 40.0, centre.1 + 40.0);
    until(&mut host, &mut window, &mut viewer, |_, _, client| {
        client
            .as_ref()
            .is_some_and(|client| !client.seen.motions.is_empty())
    });

    // The person on this device cuts remote input: nothing more arrives.
    host.session.compositor().cut_remote_input();
    assert!(
        host.session.compositor().remote_input_cut(),
        "the cut holds"
    );
    let before = window
        .as_ref()
        .map(|window| window.seen.motions.len())
        .unwrap_or_default();
    move_pointer(&viewer, &host, tab, centre.0 + 4.0, centre.1 + 4.0);
    settle(&mut host, &mut window, &mut viewer);
    let after = window
        .as_ref()
        .map(|window| window.seen.motions.len())
        .unwrap_or_default();
    assert_eq!(
        before, after,
        "after the cut chord the member's input no longer reaches the window"
    );
}
