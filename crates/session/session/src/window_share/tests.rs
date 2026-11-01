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
use tilo_action_policy::{Role, share::is_of_share};
use tilo_actions::ResourceSpec;
use tilo_compositor::{Compositor, OutputConfig};
use tilo_desktop::desktop_actions::WINDOW_KIND;
use tilo_desktop_model::window_share::window_share_id;
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

/// `desk` under the governance the room's devices agree on: `owner` owns its
/// workspace, and each of `members` holds their role there — the policy
/// every member's messages are admitted by.
fn governed(mut desk: DesktopTree, owner: &str, members: &[(String, Role)]) -> DesktopTree {
    for workspace in &mut desk.workspaces {
        workspace.policy.owner = Some(owner.to_owned());
        for (person, role) in members {
            workspace.policy.members.insert(person.clone(), *role);
        }
    }
    desk
}

/// One device of the room: its session, its plane, and the messages its
/// plane would have sent.
struct Member {
    session: Session,
    sync: Rc<Client>,
    said: UnboundedReceiver<Outgoing>,
}

fn member(seed: u8, desk: DesktopTree) -> Member {
    member_as(seed, desk, Options::default().salt, None)
}

/// One device of the room whose plane takes part as the person its
/// certificate names: a member whose messages the workspace's policy is
/// asked about.
fn member_of(seed: u8, desk: DesktopTree) -> Member {
    member_as(seed, desk, Options::default().salt, Some(person(seed)))
}

/// One device of the room whose ids are minted under `salt`: a session with
/// an identity mints under the salt its own key derives.
fn member_minting_under(seed: u8, desk: DesktopTree, salt: u16) -> Member {
    member_as(seed, desk, salt, None)
}

/// One device of the room, taking part as `person` (their root key, hex)
/// when it says one: what a session with a device identity is on this plane.
fn member_as(seed: u8, desk: DesktopTree, salt: u16, person: Option<String>) -> Member {
    let (sync, said) = Client::planeless(
        device(seed),
        OwnPresence {
            label: format!("Device {seed:x}"),
            ..OwnPresence::default()
        },
        Vec::new(),
        Reachability::default(),
    );
    let sync = Rc::new(match person {
        Some(person) => sync.taking_part_as(&person),
        None => sync,
    });
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            salt,
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

/// `from` said `msg` on `to`'s plane in the room: what the plane carries
/// between the two devices.
fn tell(to: &Member, from: &Member, msg: NeighbourMsg) {
    let ws = to.sync.room_topic(ROOM).expect("the room's topic");
    to.sync.hear(said(from, ws, msg));
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
                "tab": tab.to_string(),
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
            &arguments(serde_json::json!({ "tab": tab.to_string(), "room": ROOM })),
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
            &arguments(serde_json::json!({ "tab": tab.to_string(), "on": on })),
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

/// The start of a stream `from` shares: its own window, of `app`, shown as
/// `tab` of the room.
fn stream_start(from: &Member, surface: u64, tab: u64) -> NeighbourMsg {
    NeighbourMsg::SurfaceStreamStart {
        surface,
        source_peer: from.sync.peer(),
        width: 64,
        height: 64,
        codec: "h264".to_owned(),
        ts: 0,
        sender_monotonic_ns_at_send: 0,
        colorimetry: None,
        hdr_static_metadata: None,
        bit_depth: 8,
        app_id: APP.to_owned(),
        tab,
    }
}

/// One encoded unit of `from`'s stream of `surface`.
fn stream_frame(from: &Member, surface: u64, seq: u64) -> NeighbourMsg {
    NeighbourMsg::SurfaceFrame {
        surface,
        source_peer: from.sync.peer(),
        encoded_h264: vec![0, 0, 0, 1, 0x65, 1],
        ts: 0,
        seq,
    }
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
    assert_eq!(
        shared["tab"].as_str(),
        Some(tab.to_string().as_str()),
        "which window: {reading}"
    );
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

/// The width and height of the stream `viewer` received, in its own pixels:
/// the middle of the picture the host sends.
#[track_caller]
fn received_centre(viewer: &Member) -> (f64, f64) {
    let reading = shares(&viewer.session);
    let stream = rows(&reading, "received")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("the shared window arrived: {reading}"));
    let side = |name: &str| stream[name].as_f64().unwrap_or(0.0);
    (side("width") / 2.0, side("height") / 2.0)
}

/// The host streams `viewer` the window of `tab`, and `viewer`'s desk shows
/// it: one member of the room viewing another's window.
#[track_caller]
fn viewing(host: &mut Member, window: &mut Option<TestClient>, viewer: &mut Member, tab: u64) {
    until(host, window, viewer, |host, viewer, _| {
        rows(&shares(&host.session), "sharing")
            .first()
            .and_then(|shared| shared["viewers"].as_array())
            .is_some_and(|viewers| {
                viewers
                    .iter()
                    .any(|watcher| watcher["peer"].as_u64() == Some(viewer.sync.peer()))
            })
    });
    let wanted = tab.to_string();
    until(host, window, viewer, |_, viewer, _| {
        rows(&shares(&viewer.session), "received")
            .first()
            .is_some_and(|stream| stream["tab"].as_str() == Some(wanted.as_str()))
    });
}

/// A member the workspace's policy narrowed to reading is streamed the window
/// it asks to view, and carries nothing that would change the workspace: the
/// stream it starts and the input it sends are refused at the plane.
#[test]
fn a_member_narrowed_to_reading_sends_no_frames_and_no_input() {
    tilo_window_share::flatpak_share::read_encoders();
    let host_person = person(0x31);
    let viewer_person = person(0x32);
    let mut host = member_of(
        0x31,
        governed(
            own_desk(),
            &host_person,
            &[(viewer_person.clone(), Role::Viewer)],
        ),
    );
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    share(&mut host, APP, tab);
    let mut viewer = member_of(
        0x32,
        governed(
            showing(host.sync.publisher(), APP, tab),
            &host_person,
            &[(viewer_person, Role::Viewer)],
        ),
    );
    // Each device's certificate is how the other knows who is here.
    heard(&host, 0x32, "Phone");
    heard(&viewer, 0x31, "Framework");

    // Asking to view is what reading covers: the host streams it the window.
    watch(&mut viewer, tab);
    viewing(&mut host, &mut window, &mut viewer, tab);

    // What it sends to change anything is refused, however well formed: a
    // stream of its own never reaches the host's desk.
    tell(&host, &viewer, stream_start(&viewer, 991, 4));
    tell(&host, &viewer, stream_frame(&viewer, 991, 1));
    settle(&mut host, &mut window, &mut viewer);
    assert!(
        rows(&shares(&host.session), "received").is_empty(),
        "a member that may only read shares no window: {}",
        shares(&host.session)
    );

    // Nor does its input, even once the person lets the members viewing the
    // window control it.
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
    let centre = received_centre(&viewer);
    move_pointer(&viewer, &host, tab, centre.0, centre.1);
    settle(&mut host, &mut window, &mut viewer);
    assert!(
        window
            .as_ref()
            .is_some_and(|window| !window.seen.pointer_inside && window.seen.motions.is_empty()),
        "the member's input does not reach the window"
    );
}

/// A member the workspace's policy lets edit is carried: the window it shares
/// reaches the host's desk, and its input reaches the host's window.
#[test]
fn a_member_the_workspace_lets_edit_shares_and_reaches_input() {
    tilo_window_share::flatpak_share::read_encoders();
    let host_person = person(0x41);
    let member_person = person(0x42);
    let mut host = member_of(
        0x41,
        governed(
            own_desk(),
            &host_person,
            &[(member_person.clone(), Role::Editor)],
        ),
    );
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    share(&mut host, APP, tab);
    let mut member = member_of(
        0x42,
        governed(
            showing(host.sync.publisher(), APP, tab),
            &host_person,
            &[(member_person, Role::Editor)],
        ),
    );
    heard(&host, 0x42, "Studio");
    heard(&member, 0x41, "Framework");

    watch(&mut member, tab);
    viewing(&mut host, &mut window, &mut member, tab);

    // Its own stream reaches the host's desk: the window it shares arrives
    // with its frames.
    tell(&host, &member, stream_start(&member, 991, 4));
    tell(&host, &member, stream_frame(&member, 991, 1));
    until(&mut host, &mut window, &mut member, |host, _, _| {
        rows(&shares(&host.session), "received")
            .first()
            .is_some_and(|stream| {
                stream["surface"].as_u64() == Some(991)
                    && stream["units"]
                        .as_array()
                        .is_some_and(|units| !units.is_empty())
            })
    });
    assert_eq!(
        rows(&shares(&host.session), "received")
            .first()
            .and_then(|stream| { stream["from"].as_u64() }),
        Some(member.sync.peer()),
        "the window that arrived is the member's: {}",
        shares(&host.session)
    );

    // And its input reaches the host's window once the person lets the
    // members viewing it control it.
    allow_input(&mut host, tab, true);
    until(&mut host, &mut window, &mut member, |host, _, _| {
        rows(&shares(&host.session), "sharing")
            .first()
            .and_then(|shared| shared["viewers"].as_array())
            .is_some_and(|viewers| {
                viewers
                    .iter()
                    .any(|viewer| viewer["can_control"].as_bool() == Some(true))
            })
    });
    let centre = received_centre(&member);
    move_pointer(&member, &host, tab, centre.0, centre.1);
    until(&mut host, &mut window, &mut member, |_, _, client| {
        client
            .as_ref()
            .is_some_and(|client| client.seen.pointer_inside)
    });
    assert!(
        window
            .as_ref()
            .is_some_and(|window| window.seen.pointer_inside),
        "the member's input reached the window"
    );
}

/// A window one device shares shows on another device's desk because the
/// room's governance says it is shared there: nobody names the tab on the
/// other device.
#[test]
fn a_share_shows_on_another_device_from_the_rooms_governance() {
    tilo_window_share::flatpak_share::read_encoders();
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(0x21).as_bytes());
    let tab = tilo_desktop_model::ids::compose_id(salt, 1);
    let mut host = member_minting_under(
        0x21,
        desk(
            TileContent::LocalApp {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
            },
            tab,
        ),
        salt,
    );
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    assert_eq!(
        tilo_desktop_model::peer_salt_of(tab),
        salt,
        "the window is this device's own"
    );
    let mut viewer = member_minting_under(0x22, own_desk(), 1);
    share(&mut host, APP, tab);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        !rows(&shares(&host.session), "sharing").is_empty()
    });

    // The room's governance says the window is shared, hosted on this
    // device: what the room's document carries to its other devices.
    let grants = host
        .session
        .handle()
        .read(|tree| tree.workspaces[0].policy.grants.clone());
    assert!(
        grants
            .iter()
            .any(|grant| is_of_share(grant, &window_share_id(tab))),
        "the share is in the room's governance: {grants:?}"
    );
    assert!(
        hosted(&viewer.session, tab).is_none(),
        "the other device shows nothing before the governance reaches it"
    );
    assert!(
        rows(&shares(&viewer.session), "watching").is_empty(),
        "nobody asked to view the window by hand"
    );
    carry_governance(&host.session, &viewer.session);
    heard(&viewer, 0x21, "Framework");

    // The other device shows the host's window in the tab the governance
    // names, and its stream comes there without being asked for.
    let host_key = hex::encode(device(0x21).as_bytes());
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        hosted(&viewer.session, tab).as_deref() == Some(host_key.as_str())
    });
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        !rows(&shares(&viewer.session), "received").is_empty()
    });

    // The share ends, and with it the tab on the other device.
    host.session
        .invoke(
            SHARES_RESOURCE,
            "stop",
            &arguments(serde_json::json!({ "tab": tab.to_string() })),
        )
        .expect("the share is stopped");
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        !host.session.handle().read(|tree| {
            tree.workspaces[0]
                .policy
                .grants
                .iter()
                .any(|grant| is_of_share(grant, &window_share_id(tab)))
        })
    });
    carry_governance(&host.session, &viewer.session);
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        hosted(&viewer.session, tab).is_none()
    });
}

/// The window of another device shown on `session`'s desk as `tab`, with the
/// key of the device that hosts it: what a person sees there.
fn hosted(session: &Session, tab: u64) -> Option<String> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|shown| shown.id == tab)
            .and_then(|shown| match &shown.tile {
                TileContent::HostedWindow { host, .. } => Some(host.clone()),
                _ => None,
            })
    })
}

/// Carry `from`'s governance to `to`'s desk, as the room's document carries
/// it between the room's devices.
fn carry_governance(from: &Session, to: &Session) {
    let grants = from.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| workspace.policy.grants.clone())
            .collect::<Vec<_>>()
    });
    let mut desk = to.handle().read(Clone::clone);
    for workspace in &mut desk.workspaces {
        workspace.policy.grants.clone_from(&grants);
    }
    to.handle().replace_tree(desk);
}

/// The person the device `seed` is a device of, in the hex everyone here
/// knows them by: the root key its certificate is signed with.
fn person(seed: u8) -> String {
    hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

/// The device `seed` said who it is on `viewer`'s plane: the certificate a
/// session says when it starts, which names the device it runs on.
fn heard(viewer: &Member, seed: u8, label: &str) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *device(seed).as_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: 0,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).expect("a certified device");
    let ws = viewer.sync.room_topic(ROOM).expect("the room's topic");
    viewer.sync.hear(NetEvent::Msg {
        ws,
        peer: device(seed),
        membership_epoch: None,
        replayed: false,
        msg: NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        },
    });
}
