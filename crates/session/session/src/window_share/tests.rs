//! Two sessions in one workspace: one shares a window of its desk, the other shows it and reaches
//! into it, and the bar on the sharing device says so. The window's pictures travel the
//! workspace's live plane, on an in-process relay both sessions' planes meet on; the state plane
//! has no transport between them, so the test carries what one would have said to the other, to
//! the members each control message was for.

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

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use channel_crypto::{ChannelGroup, MemberKey};
use tilo_live::plane::{Join, Plane};
use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};
use tilo_live::roster::Roster as LiveRoster;
use tilo_live::scope::Scope;

use crate::sync::Event;
use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use serde_json::Value;
use tilo_action_policy::{Role, share::is_of_share};
use tilo_actions::ResourceSpec;
use tilo_color_pipeline::hdr_display::DisplayCapabilities;
use tilo_compositor::{Compositor, InputEvent, OutputConfig, Selection};

use tilo_desktop::desk_verbs::{DESK_RESOURCE, WINDOW_KIND, tab_resource_id};
use tilo_desktop_model::window_share::window_share_id;
use tilo_desktop_model::{
    DesktopTree, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceSlot,
};
use tilo_network_actor::WorkspaceId as Topic;
use tilo_window_share::streaming::InputEvent as SharedInput;
use tokio::sync::mpsc::UnboundedReceiver;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_keyboard, wl_keyboard::WlKeyboard,
    wl_pointer, wl_pointer::WlPointer, wl_registry, wl_registry::WlRegistry, wl_seat,
    wl_seat::WlSeat, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3,
    zwp_text_input_v3::{ContentHint, ContentPurpose, ZwpTextInputV3},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2, zwp_input_method_v2::ZwpInputMethodV2,
};

use super::SHARES_RESOURCE;
use crate::sync::Cmd;
use crate::sync::{Client, OwnPresence};
use crate::{Options, Session};

type Arguments = serde_json::Map<String, Value>;

const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
const APP: &str = "so.tilo.notes";
const PHOTOS: &str = "so.tilo.photos";
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// What the test's live planes run on: one runtime, one in-process relay and the workspace's keys,
/// and the planes joined so far by device, for the whole test process.
struct LivePlanes {
    runtime: tokio::runtime::Runtime,
    relay: Arc<Relay>,
    keys: channel_crypto::ChannelContentKeys,
    joined: Mutex<Vec<([u8; 32], Arc<Plane>)>>,
}

/// The relay the test's planes meet on: in-process, with no one to admit, since a local plane is
/// its own relay's participant.
fn live_planes() -> &'static LivePlanes {
    static PLANES: OnceLock<LivePlanes> = OnceLock::new();
    PLANES.get_or_init(|| {
        struct Nobody;
        impl RelayAuthority for Nobody {
            fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
                Box::pin(std::future::ready(Err(Denied)))
            }
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the live planes' runtime");
        let relay = {
            let _entered = runtime.enter();
            Arc::new(Relay::new(Arc::new(Nobody)))
        };
        let channel = Scope::workspace(WORKSPACE.to_owned()).channel_id();
        let keys =
            ChannelGroup::create(channel, MemberKey::generate("owner").expect("a member key"))
                .expect("the workspace's channel")
                .content_keys()
                .expect("the workspace's keys");
        LivePlanes {
            runtime,
            relay,
            keys,
            joined: Mutex::new(Vec::new()),
        }
    })
}

/// `device` 's plane in the test workspace, with every plane joined before it hearing it, and it
/// hearing them.
fn live_plane(device: EndpointId) -> BTreeMap<String, Arc<Plane>> {
    let live = live_planes();
    let _entered = live.runtime.enter();
    let mut joined = live.joined.lock().expect("the joined planes");
    let devices: Vec<[u8; 32]> = joined
        .iter()
        .map(|(device, _)| *device)
        .chain([*device.as_bytes()])
        .collect();
    let plane = Arc::new(
        Plane::local(
            &live.relay,
            &Join {
                scope: Scope::workspace(WORKSPACE.to_owned()),
                device: *device.as_bytes(),
                keys: live.keys.clone(),
                roster: LiveRoster::new(devices.iter().copied()),
                publish: true,
            },
        )
        .expect("a local plane"),
    );
    for (_, other) in joined.iter() {
        other.set_roster(LiveRoster::new(devices.iter().copied()));
    }
    joined.push((*device.as_bytes(), Arc::clone(&plane)));
    BTreeMap::from([(WORKSPACE.to_owned(), plane)])
}

fn device(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse(WORKSPACE).expect("a workspace id")
}

/// A desk with one workspace, its one tab showing `tile` : what a session shares a window of, or
/// shows another member's window in.
fn desk(tile: TileContent, tab: u64) -> DesktopTree {
    let workspace_id = workspace_id();
    let slot = WorkspaceSlot::of_workspace(workspace_id);
    let mut workspace = Workspace::in_workspace(slot, workspace_id, "Desk");
    let mut strip = Strip::new(StripId::from(1), "Main");
    strip.window_groups.push(WindowGroup {
        id: GroupId::from(1_u64),
        tabs: vec![Tab {
            id: TabId::from(tab),
            title: "Notes".to_owned(),
            tile,
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: TabId::from(tab),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    workspace.strips.push(strip);
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
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

/// A desk showing another device's window of `app_id` in `tab` : the tab the host shows that window
/// in, as the workspace's shared layout names it.
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

/// A desk showing another device's windows of `apps` , one per group, in `tabs` : what a member
/// viewing two shared windows holds.
fn showing_both(host: EndpointId, first: (&str, u64), second: (&str, u64)) -> DesktopTree {
    let mut tree = showing(host, first.0, first.1);
    let strip = &mut tree.workspaces[0].strips[0];
    let tab = TabId::from(second.1);
    strip.window_groups.push(WindowGroup {
        id: GroupId::from(2_u64),
        tabs: vec![Tab {
            id: tab,
            title: "Notes".to_owned(),
            tile: TileContent::HostedWindow {
                app_id: second.0.to_owned(),
                label: "Notes".to_owned(),
                host: hex::encode(host.as_bytes()),
                host_label: "Framework".to_owned(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    tree
}

/// `desk` under the governance the workspace's devices agree on: `owner` owns its workspace, and
/// each of `members` holds their role there — the policy every member's messages are admitted by.
fn governed(mut desk: DesktopTree, owner: &str, members: &[(String, Role)]) -> DesktopTree {
    for workspace in &mut desk.workspaces {
        workspace.policy.owner = Some(owner.to_owned());
        for (person, role) in members {
            workspace.policy.members.insert(person.clone(), *role);
        }
    }
    desk
}

/// One device of the workspace: its session, its plane, and the messages its plane would have sent.
struct Member {
    session: Session,
    sync: Rc<Client>,
    said: UnboundedReceiver<Cmd>,
}

fn member(seed: u8, desk: DesktopTree) -> Member {
    member_as(seed, desk, Options::default().salt, None)
}

/// One device of the workspace whose plane takes part as the person its certificate names: a member
/// whose messages the workspace's policy is asked about.
fn member_of(seed: u8, desk: DesktopTree) -> Member {
    member_as(seed, desk, Options::default().salt, Some(person(seed)))
}

/// One device of the workspace whose ids are minted under `salt` : a session with an identity mints
/// under the salt its own key derives.
fn member_minting_under(seed: u8, desk: DesktopTree, salt: u16) -> Member {
    member_as(seed, desk, salt, None)
}

/// One device of the workspace, taking part as `person` (their root key, hex) when it says one:
/// what a session with a device identity is on this plane.
fn member_as(seed: u8, desk: DesktopTree, salt: u16, person: Option<String>) -> Member {
    member_on(
        seed,
        desk,
        salt,
        person,
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
}

/// One device of the workspace whose desk spreads over `outputs` .
fn member_on(
    seed: u8,
    desk: DesktopTree,
    salt: u16,
    person: Option<String>,
    outputs: &[OutputConfig],
) -> Member {
    let (sync, said) = Client::planeless(
        device(seed),
        OwnPresence {
            label: format!("Device {seed:x}"),
            ..OwnPresence::default()
        },
        Vec::new(),
    );
    let sync = sync.on_planes(
        live_plane(device(seed)),
        live_planes().runtime.handle().clone(),
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
        outputs,
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

/// `from` said `msg` on `to` 's plane in the workspace: what the plane carries between the two
/// devices.
fn tell(to: &Member, from: &Member, msg: NeighbourMsg) {
    let ws = to
        .sync
        .workspace_topic(WORKSPACE)
        .expect("the workspace's topic");
    to.sync.hear(said(from, ws, msg));
}

/// One message `from` said, as the workspace's topic would have carried it.
fn said(from: &Member, ws: Topic, msg: NeighbourMsg) -> Event {
    Event::Msg {
        ws,
        peer: from.sync.publisher(),
        msg: Box::new(msg),
    }
}

/// Hand what `from` would have sent on the state plane to `to`: a control message to the device it
/// named, and nothing to anyone else. The window's pictures travel the live plane.
fn carry(from: &mut Member, to: &Member) {
    while let Ok(outgoing) = from.said.try_recv() {
        match outgoing {
            Cmd::SendControl { ws, msg, target } if target == to.sync.publisher() => {
                to.sync.hear(said(from, ws, *msg));
            }
            _ => {}
        }
    }
}

/// Step both sessions, and the client whose window `a` shares, until `done` holds of the two
/// members and the client, carrying what the planes said.
#[track_caller]
fn until(
    a: &mut Member,
    client: &mut Option<TestClient>,
    b: &mut Member,
    mut done: impl FnMut(&Member, &Member, &Option<TestClient>) -> bool,
) {
    // A window is captured, encoded and decoded here: on a machine busy with the rest of the suite
    // that takes far longer than it does alone.
    let deadline = Instant::now() + Duration::from_secs(300);
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

/// What the shares resource says on `session` .
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
    fn find(resources: &[ResourceSpec], label: &str) -> Option<u64> {
        resources.iter().find_map(|resource| {
            if resource.kind == WINDOW_KIND && resource.label == label {
                resource
                    .id
                    .0
                    .rsplit(':')
                    .next()
                    .and_then(|tab| tab.parse().ok())
            } else {
                find(&resource.children, label)
            }
        })
    }
    let label = tilo_desktop_search::names::window_name(app, app);
    find(&session.resources(), &label).unwrap_or_else(|| panic!("no window of {app} on the desk"))
}

/// The compositor's identifier of the window of `app` , as a share names it.
fn identifier_of(compositor: &Compositor, app: &str) -> String {
    compositor
        .shareable_windows()
        .into_iter()
        .find(|(_, app_id, _)| app_id.as_deref() == Some(app))
        .map(|(identifier, _, _)| identifier)
        .expect("the window is shareable")
}

/// Share the window of `app` as `tab` of the workspace.
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
                "room": WORKSPACE,
            })),
        )
        .expect("the window is shared with the workspace");
}

/// Show the window another member shares, in `tab` of the workspace.
fn watch(member: &mut Member, tab: u64) {
    member
        .session
        .invoke(
            SHARES_RESOURCE,
            "watch",
            &arguments(serde_json::json!({ "tab": tab.to_string(), "room": WORKSPACE })),
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

/// Move the pointer over the window of `surface` on `host` , as a member viewing it does.
fn move_pointer(viewer: &Member, host: &Member, surface: u64, x: f64, y: f64) {
    send_input(viewer, host, surface, &SharedInput::PointerMotion { x, y });
}

/// Send one input event for the window of `surface` on `host` , as a member viewing it does.
fn send_input(viewer: &Member, host: &Member, surface: u64, event: &SharedInput) {
    let event = postcard::to_stdvec(event).expect("input");
    let ws = host
        .sync
        .workspace_topic(WORKSPACE)
        .expect("the workspace's topic");
    host.sync.hear(Event::Msg {
        ws,
        peer: viewer.sync.publisher(),
        msg: Box::new(NeighbourMsg::RemoteInput {
            peer: viewer.sync.peer(),
            surface,
            event,
        }),
    });
}

/// The start of a stream `from` shares: its own window, of `app` , shown as `tab` of the workspace.
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

/// A client with one toplevel of a solid colour: a person's window on a session's compositor.
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
    /// How often the pointer came in: a seat's event leaves it as it found it.
    enters: usize,
    motions: Vec<(f64, f64)>,
    keyboard: Option<WlKeyboard>,
    /// The keys the window's keyboard heard, as `(keycode, pressed)` .
    keys: Vec<(u32, bool)>,
    text_input_manager: Option<ZwpTextInputManagerV3>,
    text_input: Option<ZwpTextInputV3>,
    /// The input method this client runs for the seat: a text field only takes its text through
    /// one.
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    #[allow(dead_code, reason = "held so the seat keeps its input method")]
    input_method: Option<ZwpInputMethodV2>,
    /// The text its text field was given, as an input method gives it.
    committed: Vec<String>,
    closed: bool,
}

impl TestClient {
    fn connect(compositor: &mut Compositor) -> Self {
        // The session starts the apps it shows and trusts them, as it starts the input method a
        // text field is typed into.
        let socket = compositor.connect_trusted_client().expect("client socket");
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

    /// Give the window a text field: a `text-input-v3` object on its surface,
    /// waiting for typing.
    fn own_text_field(&mut self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .text_input_manager
            .as_ref()
            .expect("zwp_text_input_manager_v3");
        let seat = self.seen.seat.as_ref().expect("wl_seat");
        // The seat composes for a text field only while an input method serves it, as the person's
        // on-screen keyboard does.
        let input_method = self
            .seen
            .input_method_manager
            .as_ref()
            .expect("zwp_input_method_manager_v2")
            .get_input_method(seat, &qh, ());
        self.seen.input_method = Some(input_method);
        let field = manager.get_text_input(seat, &qh, ());
        field.set_content_type(ContentHint::empty(), ContentPurpose::Normal);
        field.enable();
        field.commit();
        self.seen.text_input = Some(field);
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
                "zwp_text_input_manager_v3" => {
                    seen.text_input_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_input_method_manager_v2" => {
                    seen.input_method_manager = Some(registry.bind(name, 1, qh, ()));
                }
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
        {
            if capabilities.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
            }
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
            wl_pointer::Event::Enter { .. } => {
                seen.pointer_inside = true;
                seen.enters = seen.enters.saturating_add(1);
            }
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

impl Dispatch<WlKeyboard, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(state),
            ..
        } = event
        {
            seen.keys
                .push((key, state == wl_keyboard::KeyState::Pressed));
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_text_input_v3::Event::CommitString { text: Some(text) } = event {
            seen.committed.push(text);
        }
    }
}

delegate_noop!(Seen: ignore ZwpTextInputManagerV3);
delegate_noop!(Seen: ignore ZwpInputMethodManagerV2);
delegate_noop!(Seen: ignore ZwpInputMethodV2);
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
        seen.wm_base.is_some()
            && seen.seat.is_some()
            && seen.text_input_manager.is_some()
            && seen.input_method_manager.is_some()
            && seen.keyboard.is_some()
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

/// A window this session does not run — the person's own program on the compositor — is read and
/// pictured through the two actions the desk publishes for it: `look` answers a picture kept in the
/// workspace's content, `read` what the window calls itself. Neither acts on the window: a window
/// is reached by the address its own app publishes, never by pixels.
#[test]
fn a_window_the_session_does_not_run_is_read_and_pictured() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut host = member(0xa4, own_desk());
    let _window = mapped(&mut host.session, APP);
    let tab = foreign_tab(&host.session);
    let workspace = host
        .session
        .desktop()
        .with_policy(|policy| {
            policy
                .handle()
                .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        })
        .expect("a workspace in front");

    // What the window calls itself, as the desk names it in its bar.
    let read = completed(
        host.session
            .invoke(&tab_resource_id(tab), "read", &Arguments::new())
            .expect("read answers"),
    );
    let answered = read.value.expect("read answers with what the window is");
    assert_eq!(answered["app"], APP);
    assert_eq!(
        answered["title"],
        tilo_desktop_search::names::window_name(APP, APP),
        "the title the desk shows: {answered}"
    );

    // The picture of the window, kept where any caller of the action protocol reads content by.
    let look = completed(
        host.session
            .invoke(&tab_resource_id(tab), "look", &Arguments::new())
            .expect("look answers"),
    );
    let answered = look.value.expect("look answers with the picture");
    let address = answered["content"].as_str().expect("the picture's address");
    let hash = tilo_actions::content_hash(address).expect("a content address");
    assert!(
        host.session.content.puts().holds(&workspace, hash),
        "the picture is kept as content of the workspace the window stands in: {answered}"
    );
    assert_eq!(
        look.resources,
        [address.to_owned()],
        "the picture is the answer's one resource"
    );
    for edge in ["width", "height"] {
        assert!(
            answered[edge].as_u64().is_some_and(|pixels| pixels > 0),
            "the picture has the window's {edge}: {answered}"
        );
    }

    // The window's row offers nothing that reaches into the window itself: the two reads, and the
    // desk's own focus and close of the tab it stands in.
    let row = host
        .session
        .resources()
        .into_iter()
        .flat_map(descendants)
        .find(|resource| resource.id.as_str() == tab_resource_id(tab))
        .expect("the window's row");
    for action in &row.actions {
        assert!(
            matches!(
                action.action_type().operation_type.as_deref(),
                Some(tilo_desktop_surface::window::LOOK | tilo_desktop_surface::window::READ)
            ) || action.id == "focus"
                || action.id == "close-tab",
            "the window's row offers {}, which reaches into another desktop's window",
            action.id
        );
    }
}

/// The tab of the one window the compositor runs but this session did not start, as the desk's tree
/// names it.
#[track_caller]
fn foreign_tab(session: &Session) -> u64 {
    session
        .desktop()
        .with_policy(|policy| {
            policy.handle().read(|tree| {
                tree.workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find_map(|tab| match tab.tile {
                        TileContent::Foreign { .. } => Some(tab.id),
                        _ => None,
                    })
            })
        })
        .expect("a window the session does not run")
}

/// `started` , or the completed answer it is.
#[track_caller]
fn completed(started: ui::host_operation::ResourceOperationStart) -> tilo_actions::ActionOutput {
    match started {
        ui::host_operation::ResourceOperationStart::Completed(output) => output,
        ui::host_operation::ResourceOperationStart::Accepted => {
            panic!("a read action answers at once")
        }
    }
}

/// `resource` and everything below it.
fn descendants(resource: ResourceSpec) -> Vec<ResourceSpec> {
    let mut all = vec![resource.clone()];
    for child in resource.children {
        all.extend(descendants(child));
    }
    all
}

/// One session shares a window of its desk and the other shows it: the viewing member's tab holds
/// the picture its sender streams, and the sharing device's bar names the member watching it.
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
        viewer
            .session
            .handle()
            .peer_surface_read(tab)
            .is_some_and(|read| read.pictured())
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
        stream["pictured"].as_bool() == Some(true),
        "the tab shows the picture it decoded: {reading}"
    );
    assert!(
        stream["width"].as_u64().unwrap_or(0) > 0 && stream["height"].as_u64().unwrap_or(0) > 0,
        "the stream says how big the window is: {reading}"
    );

    // The member says it views the window, which is how the bar names it.
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        rows(&shares(&host.session), "sharing")
            .first()
            .and_then(|shared| shared["viewers"].as_array())
            .is_some_and(|viewers| !viewers.is_empty())
    });
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

/// A window one device shares is decoded into the tab showing it on another device, and the
/// person's pointer over that picture reaches the window while its owner lets it in.
#[test]
fn a_shared_window_is_pictured_where_it_is_shown_and_the_pointer_reaches_it() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut host = member(0x41, own_desk());
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    share(&mut host, APP, tab);
    let mut viewer = member(0x42, showing(host.sync.publisher(), APP, tab));
    watch(&mut viewer, tab);

    // The tile of the tab the window stands in decodes what its sender streams, so the window shows
    // there as a picture.
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        viewer
            .session
            .handle()
            .peer_surface_read(tab)
            .is_some_and(|read| read.pictured())
    });
    let read = viewer
        .session
        .handle()
        .peer_surface_read(tab)
        .expect("the shared window has a tile");
    assert_eq!(read.tab, tab, "the tab it stands in");
    assert_eq!(read.surface, tab, "the surface its sender names");
    assert_eq!(read.app_id, APP, "the window it shows");
    assert_eq!(
        read.peer,
        neighbour_wire::peer_handle(host.sync.publisher().as_bytes()),
        "the device sharing it"
    );
    assert_eq!(
        read.standing,
        tilo_desktop_surface::Standing::Pictured,
        "the tile reads as a picture of the window"
    );
    let (width, height, bytes) = read.picture.expect("the picture the tile decoded");
    assert!(
        width > 0 && height > 0,
        "the picture keeps the sender's size"
    );
    // The member also says it views the window: the person lets the members viewing it in.
    viewing(&mut host, &mut window, &mut viewer, tab);
    assert_eq!(
        bytes,
        usize::try_from(width).unwrap() * usize::try_from(height).unwrap() * 4,
        "the picture is RGBA at that size"
    );

    // The person's pointer over the picture is this device's input for the window: it leaves the
    // tile and goes back to the device hosting it.
    let (x, y) = (f64::from(WIDTH) / 2.0, f64::from(HEIGHT) / 2.0);
    viewer
        .session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer moves over the picture");
    settle(&mut host, &mut window, &mut viewer);
    assert!(
        window
            .as_ref()
            .is_some_and(|window| !window.seen.pointer_inside && window.seen.motions.is_empty()),
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
    viewer
        .session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: x + 20.0,
            y: y + 20.0,
        })
        .expect("the pointer moves over the picture");
    until(&mut host, &mut window, &mut viewer, |_, _, client| {
        client.as_ref().is_some_and(|client| client.seen.enters > 0)
    });
    assert!(
        window.as_ref().is_some_and(|window| window.seen.enters > 0),
        "the person's pointer reached the window"
    );
}

/// A shared window's pictures are prepared for the colour of the display its tile stands on, and
/// follow the window when it moves to another.
#[test]
fn a_shared_window_is_prepared_for_the_display_it_stands_on() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut host = member(0x71, own_desk());
    let mut window = Some(mapped(&mut host.session, APP));
    let first = tab_of(&host.session, APP);
    let _other = mapped(&mut host.session, PHOTOS);
    let second = tab_of(&host.session, PHOTOS);
    share(&mut host, APP, first);
    share(&mut host, PHOTOS, second);

    // The viewer's left display shows SDR and its right one HDR, as a panel that takes PQ and is
    // driven with it says.
    let outputs = [
        OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT).at(0, 0),
        OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT)
            .at(WIDTH, 0)
            .showing(DisplayCapabilities::hdr10_baseline()),
    ];
    let mut viewer = member_on(
        0x72,
        showing_both(host.sync.publisher(), (APP, first), (PHOTOS, second)),
        Options::default().salt,
        None,
        &outputs,
    );
    watch(&mut viewer, first);
    watch(&mut viewer, second);
    let both = |viewer: &Member| {
        viewer.session.handle().peer_surface_read(first).is_some()
            && viewer.session.handle().peer_surface_read(second).is_some()
    };
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        both(viewer)
    });

    let colour = |viewer: &Member, tab: u64| {
        viewer
            .session
            .handle()
            .peer_surface_read(tab)
            .expect("the shared window has a tile")
            .display
    };
    assert_eq!(
        colour(&viewer, first),
        DisplayCapabilities::sdr_default(),
        "a window on the display that shows SDR is prepared for SDR"
    );
    assert_eq!(
        viewer.session.handle().displays(),
        vec![0, 1],
        "the viewer spreads over both displays, left to right"
    );
    assert_eq!(
        viewer
            .session
            .handle()
            .peer_surface_display(first)
            .as_deref(),
        Some("HEADLESS-1"),
        "the first window stands on the display on the left"
    );
    assert_eq!(
        colour(&viewer, second),
        DisplayCapabilities::sdr_default(),
        "both windows open on the display with focus, which shows SDR"
    );

    // The person brings the second window forward, then moves the window they work in to the
    // display on the right.
    viewer
        .session
        .invoke(
            &tab_resource_id(second),
            "focus",
            &arguments(serde_json::json!({})),
        )
        .expect("the window takes focus");
    settle(&mut host, &mut window, &mut viewer);
    viewer
        .session
        .invoke(
            DESK_RESOURCE,
            "move-to-display",
            &arguments(serde_json::json!({ "steps": 1 })),
        )
        .expect("the window moves to the next display");
    settle(&mut host, &mut window, &mut viewer);
    assert_eq!(
        viewer
            .session
            .handle()
            .peer_surface_display(second)
            .as_deref(),
        Some("HEADLESS-2"),
        "the window moved to the display on the right"
    );
    assert_eq!(
        colour(&viewer, second),
        DisplayCapabilities::hdr10_baseline(),
        "the window on the display that shows HDR is prepared for HDR"
    );
    assert_eq!(
        colour(&viewer, first),
        DisplayCapabilities::sdr_default(),
        "the window left behind stays prepared for the display it stands on"
    );
    let reading = viewer.session.read("session:displays").expect("published");
    assert_eq!(
        reading["displays"][1]["colour"]["supports_pq_output"],
        serde_json::json!(true),
        "the published display on the right says it shows HDR: {reading}"
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
        viewer
            .session
            .handle()
            .peer_surface_read(tab)
            .is_some_and(|read| read.pictured())
    });
    viewing(&mut host, &mut window, &mut viewer, tab);

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
    assert!(
        viewer.session.handle().peer_surface_read(tab).is_none(),
        "the tab holds no picture of a window that is gone"
    );
}

/// The member showing a shared window reaches into it only while the person lets it, and the cut
/// chord takes that away at once.
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
    // The member's pointer is in the stream's own pixels: the middle of the picture the host sends.
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
        client.as_ref().is_some_and(|client| client.seen.enters > 0)
    });
    assert!(
        window.as_ref().is_some_and(|window| window.seen.enters > 0),
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

/// A member controlling a shared window types, pastes and closes through its own seat: its text
/// reaches the window's field, its clipboard becomes this desktop's, and its close closes the
/// window.
#[test]
fn a_viewers_keys_text_clipboard_and_close_reach_the_shared_window() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut host = member(0xe1, own_desk());
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    share(&mut host, APP, tab);
    let mut viewer = member(0xf1, showing(host.sync.publisher(), APP, tab));
    watch(&mut viewer, tab);
    viewing(&mut host, &mut window, &mut viewer, tab);
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

    // The window has a field, enabled at the compositor before the member writes into it.
    if let Some(window) = window.as_mut() {
        window.own_text_field();
    }
    // The field is enabled at the compositor before the member writes into it: a seat's text goes
    // to the field the person types into.
    let deadline = Instant::now() + Duration::from_secs(60);
    while host.session.compositor().dictation_field().is_err() {
        assert!(
            Instant::now() < deadline,
            "the window's text field took the focus"
        );
        if let Some(window) = window.as_mut() {
            window.exchange();
        }
        host.session.step(Duration::from_millis(2)).expect("step");
        if let Some(window) = window.as_mut() {
            window.exchange();
        }
        carry(&mut host, &viewer);
        carry(&mut viewer, &host);
    }

    // The member's keyboard reaches the window through its seat.
    send_input(
        &viewer,
        &host,
        tab,
        &SharedInput::Key {
            keysym: 30,
            pressed: true,
            modifiers: 0,
        },
    );
    send_input(
        &viewer,
        &host,
        tab,
        &SharedInput::Key {
            keysym: 30,
            pressed: false,
            modifiers: 0,
        },
    );
    settle(&mut host, &mut window, &mut viewer);
    until(&mut host, &mut window, &mut viewer, |_, _, client| {
        client
            .as_ref()
            .is_some_and(|client| client.seen.keys.len() >= 2)
    });
    let seen_keys = window.as_ref().map(|window| window.seen.keys.clone());
    assert_eq!(
        seen_keys,
        Some(vec![(30, true), (30, false)]),
        "the member's keystroke reached the window as one press and its release"
    );

    // What its input method composed is written into the field.
    send_input(
        &viewer,
        &host,
        tab,
        &SharedInput::Text {
            text: "\u{1f642}".to_owned(),
        },
    );
    until(&mut host, &mut window, &mut viewer, |_, _, client| {
        client
            .as_ref()
            .is_some_and(|client| !client.seen.committed.is_empty())
    });
    assert_eq!(
        window.as_ref().map(|window| window.seen.committed.clone()),
        Some(vec!["\u{1f642}".to_owned()]),
        "the member's composed text reached the window's field"
    );

    // What it pasted is this desktop's clipboard, for the window's own paste.
    send_input(
        &viewer,
        &host,
        tab,
        &SharedInput::ClipboardText {
            text: "pasted words".to_owned(),
        },
    );
    settle(&mut host, &mut window, &mut viewer);
    let mut reader = host
        .session
        .compositor()
        .receive_selection(Selection::Clipboard, "text/plain")
        .expect("the member's text is the clipboard");
    let mut pasted = Vec::new();
    reader
        .read_to_end(&mut pasted)
        .expect("the clipboard reads");
    assert_eq!(
        pasted, b"pasted words",
        "the member's clipboard became this desktop's"
    );

    // The member closed its view: the window is asked to close, as a person's close of its tab
    // does.
    send_input(&viewer, &host, tab, &SharedInput::Close);
    until(&mut host, &mut window, &mut viewer, |_, _, client| {
        client.as_ref().is_some_and(|client| client.seen.closed)
    });
    assert!(
        window.as_ref().is_some_and(|window| window.seen.closed),
        "the member's close closed the window"
    );
}

/// The width and height of the stream `viewer` received, in its own pixels: the middle of the
/// picture the host sends.
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

/// The host streams `viewer` the window of `tab` , and `viewer` 's desk shows it: one member of the
/// workspace viewing another's window.
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

/// A member the workspace's policy narrowed to reading is streamed the window it asks to view, and
/// carries nothing that would change the workspace: the stream it starts and the input it sends are
/// refused at the plane.
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

    // What it sends to change anything is refused, however well formed: a stream of its own never
    // reaches the host's desk.
    tell(&host, &viewer, stream_start(&viewer, 991, 4));
    settle(&mut host, &mut window, &mut viewer);
    assert!(
        rows(&shares(&host.session), "received").is_empty(),
        "a member that may only read shares no window: {}",
        shares(&host.session)
    );

    // Nor does its input, even once the person lets the members viewing the window control it.
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

/// A member the workspace's policy lets edit is carried: the window it shares reaches the host's
/// desk, and its input reaches the host's window.
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

    // Its own stream reaches the host's desk: the window it shares is recorded there, waiting for
    // the tab the sender names. The host's desk shows no tab 4, so nothing decodes it here.
    tell(&host, &member, stream_start(&member, 991, 4));
    until(&mut host, &mut window, &mut member, |host, _, _| {
        rows(&shares(&host.session), "received")
            .first()
            .is_some_and(|stream| stream["surface"].as_u64() == Some(991))
    });
    assert_eq!(
        rows(&shares(&host.session), "received")
            .first()
            .and_then(|stream| stream["pictured"].as_bool()),
        Some(false),
        "a desk with no tab for the window shows no picture of it: {}",
        shares(&host.session)
    );
    assert_eq!(
        rows(&shares(&host.session), "received")
            .first()
            .and_then(|stream| { stream["from"].as_u64() }),
        Some(member.sync.peer()),
        "the window that arrived is the member's: {}",
        shares(&host.session)
    );

    // And its input reaches the host's window once the person lets the members viewing it control
    // it.
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
        client.as_ref().is_some_and(|client| client.seen.enters > 0)
    });
    assert!(
        window.as_ref().is_some_and(|window| window.seen.enters > 0),
        "the member's input reached the window"
    );
}

/// What the desk calls the workspace's other devices, and how long it remembers them.
mod devices;

/// A window shared with its workspace, and no longer, from its own menu.
mod menu;

/// A window's share written into the workspace's own governance, and read from it on another
/// device of the workspace.
mod governed;

/// Two members showing the one shared window at the same time, each a viewer of its own.
mod viewers;

/// A window one device shares shows on another device's desk because the workspace's governance
/// says it is shared there: nobody names the tab on the other device.
#[test]
fn a_share_shows_on_another_device_from_the_workspace_governance() {
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

    // The workspace's governance says the window is shared, hosted on this device: what the
    // workspace's document carries to its other devices.
    let grants = host
        .session
        .handle()
        .read(|tree| tree.workspaces[0].policy.grants.clone());
    assert!(
        grants
            .iter()
            .any(|grant| is_of_share(grant, &window_share_id(tab))),
        "the share is in the workspace's governance: {grants:?}"
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

    // The other device shows the host's window in the tab the governance names, and its stream
    // comes there without being asked for.
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

/// The tile `session` 's desk shows in `tab` .
fn tile_of(session: &Session, tab: u64) -> Option<TileContent> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|shown| shown.id == tab)
            .map(|shown| shown.tile.clone())
    })
}

/// A person shares a window by the tab their desk shows it in, and names nothing else. The
/// identifier a window is captured under is the compositor's own, which it tells only the clients
/// it trusts: no person, script or agent at the desk's actions can name it. The workspace is the
/// one the tab stands in.
#[test]
fn a_window_is_shared_by_naming_the_tab_it_is_shown_in() {
    tilo_window_share::flatpak_share::read_encoders();
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(0x51).as_bytes());
    let mut host = member_minting_under(
        0x51,
        desk(
            TileContent::LocalApp {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
            },
            tilo_desktop_model::ids::compose_id(salt, 1),
        ),
        salt,
    );
    let mut window = Some(mapped(&mut host.session, APP));
    // The tab the desk shows the window in, which is all the person names.
    let tab = foreign_tab(&host.session);
    assert_eq!(
        tilo_desktop_model::peer_salt_of(tab),
        salt,
        "the window's tab is this device's own: {:?}",
        tile_of(&host.session, tab)
    );
    let mut viewer = member_minting_under(0x52, own_desk(), 1);

    host.session
        .invoke(
            SHARES_RESOURCE,
            "share",
            &arguments(serde_json::json!({ "tab": tab.to_string() })),
        )
        .expect("the window the tab shows is shared with the workspace the tab stands in");
    let wanted = tab.to_string();
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        rows(&shares(&host.session), "sharing")
            .iter()
            .any(|shared| {
                shared["tab"].as_str() == Some(wanted.as_str())
                    && shared["app_id"].as_str() == Some(APP)
                    && shared["room"].as_str() == Some(WORKSPACE)
            })
    });
    let grants = host
        .session
        .handle()
        .read(|tree| tree.workspaces[0].policy.grants.clone());
    assert!(
        grants
            .iter()
            .any(|grant| is_of_share(grant, &window_share_id(tab))),
        "the share is in the workspace's governance, where its other devices read it: {grants:?}"
    );

    // A tab that shows no window of this device shares nothing.
    host.session
        .invoke(
            SHARES_RESOURCE,
            "share",
            &arguments(serde_json::json!({ "tab": "7" })),
        )
        .expect("the ask is taken");
    settle(&mut host, &mut window, &mut viewer);
    let reading = shares(&host.session);
    assert_eq!(
        rows(&reading, "sharing").len() + rows(&reading, "starting").len(),
        1,
        "only the window the desk shows is shared: {reading}"
    );
}

/// A share whose window was minted by a device this session has not heard in the workspace is
/// the one case the workspace's governance is asked who it admits: a window of a device that was
/// heard, or of this one, never asks.
#[test]
fn only_a_share_of_an_unheard_device_asks_who_the_workspace_admits() {
    let host = device(0x71);
    let salt = tilo_desktop_model::ids::device_salt_of_key(host.as_bytes());
    let tab = tilo_desktop_model::ids::compose_id(salt, 3);
    let mut policy = tilo_action_policy::WorkspacePolicy::new(WORKSPACE, person(0x71));
    let heard = BTreeMap::from([(salt, hex::encode(host.as_bytes()))]);
    let unheard = BTreeMap::from([(salt.wrapping_add(1), hex::encode(device(0x72).as_bytes()))]);
    assert!(
        !super::shares_of_unheard(&policy, &unheard),
        "a workspace that shares no window asks nothing"
    );
    policy.grants.extend(
        tilo_action_policy::share::share_grants(
            WORKSPACE,
            &person(0x71),
            &window_share_id(tab),
            &tilo_desktop_model::window_share::window_node(WORKSPACE, APP, tab),
            tilo_action_policy::share::ShareAccess::View,
        )
        .expect("a window's share"),
    );
    assert!(
        super::shares_of_unheard(&policy, &unheard),
        "the device that minted the shared window was not heard here"
    );
    assert!(
        !super::shares_of_unheard(&policy, &heard),
        "the device that minted the shared window was heard here"
    );
}

/// Two devices that hold one workspace each list the other's windows already, as windows open on
/// another device: the desk's own record says so before anything is shared. A share of such a
/// window is pictured in the tab that stood for it, and when the share ends that tab stays, since
/// the window is still open there and this session never put the tab on the desk.
#[test]
fn a_window_the_desk_already_lists_is_pictured_in_its_tab_and_keeps_it_when_the_share_ends() {
    tilo_window_share::flatpak_share::read_encoders();
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(0x61).as_bytes());
    let mut host = member_minting_under(
        0x61,
        desk(
            TileContent::LocalApp {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
            },
            tilo_desktop_model::ids::compose_id(salt, 1),
        ),
        salt,
    );
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    let mut viewer = member_minting_under(
        0x62,
        desk(
            TileContent::PeerWindow {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
                missing: None,
            },
            tab,
        ),
        1,
    );
    assert!(
        matches!(
            tile_of(&viewer.session, tab),
            Some(TileContent::PeerWindow { .. })
        ),
        "the other device lists the window as one open elsewhere"
    );

    share(&mut host, APP, tab);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        !rows(&shares(&host.session), "sharing").is_empty()
    });
    carry_governance(&host.session, &viewer.session);
    heard(&viewer, 0x61, "Framework");

    // The picture comes into the tab that stood for the window, with nobody asking for it by hand.
    let wanted = tab.to_string();
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        rows(&shares(&viewer.session), "received")
            .iter()
            .any(|stream| {
                stream["tab"].as_str() == Some(wanted.as_str())
                    && stream["pictured"].as_bool() == Some(true)
            })
    });
    assert!(
        matches!(
            tile_of(&viewer.session, tab),
            Some(TileContent::PeerWindow { .. })
        ),
        "the tab is still the one the desk listed"
    );

    // The share ends: the picture goes, and the tab the desk listed stays.
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
        let reading = shares(&viewer.session);
        rows(&reading, "watching").is_empty() && rows(&reading, "received").is_empty()
    });
    assert!(
        matches!(
            tile_of(&viewer.session, tab),
            Some(TileContent::PeerWindow { .. })
        ),
        "the window is still open on its device, and the tab that says so stays on this desk"
    );
}

/// The window of another device shown on `session` 's desk as `tab` , with the key of the device
/// that hosts it: what a person sees there.
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

/// Carry `from` 's governance to `to` 's desk, as the workspace's document carries it between the
/// workspace's devices.
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

/// The person the device `seed` is a device of, in the hex everyone here knows them by: the root
/// key its certificate is signed with.
fn person(seed: u8) -> String {
    hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

/// The device `seed` said who it is on `viewer` 's plane: the certificate a session says when it
/// starts, which names the device it runs on.
fn heard(viewer: &Member, seed: u8, label: &str) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *device(seed).as_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities {
            can_own_workspaces: false,
            ..tilo_user_identity::DeviceCapabilities::member()
        },
        issued_at_ns: 0,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).expect("a certified device");
    let ws = viewer
        .sync
        .workspace_topic(WORKSPACE)
        .expect("the workspace's topic");
    viewer.sync.hear(Event::Msg {
        ws,
        peer: device(seed),
        msg: Box::new(NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        }),
    });
}

/// The wall clock in microseconds since the Unix epoch: the clock a picture's capture is told on.
fn wall_us() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros(),
    )
    .unwrap()
}

/// The median and the 95th percentile of `samples` (microseconds), in milliseconds.
fn percentiles(samples: &mut [u64]) -> (f64, f64) {
    samples.sort_unstable();
    let at = |share: usize| samples[(samples.len() - 1) * share / 100] as f64 / 1000.0;
    (at(50), at(95))
}

/// How long a shared window's picture takes from its capture to a viewer's decoded picture, over
/// the workspace's live plane: the window is captured, encoded as a share encodes it, sealed and
/// sent on its track, opened by the viewer's plane and decoded by the tile that shows it, on a
/// thread of its own as a viewer's desk is another device's. The object's header carries the
/// capture time on the sender's clock, which is this process's here.
#[test]
fn a_shared_windows_pictures_reach_a_viewer_within_a_few_frames() {
    use tilo_live::live::Track;
    use tilo_live::video::{Layer, VideoObject};
    use tilo_window_share::flatpak_share::SharedFlatpakSession;
    const TAB: u64 = 0x5EED_0000_0000_0031;
    const PICTURES: usize = 90;
    let (w, h) = (1280_u32, 800_u32);
    tilo_window_share::flatpak_share::read_encoders();
    let host = device(0x61);
    let viewer = device(0x62);
    let sharing = Arc::clone(&live_plane(host)[WORKSPACE]);
    let watching = Arc::clone(&live_plane(viewer)[WORKSPACE]);
    let live = live_planes();
    let _entered = live.runtime.enter();
    let share = SharedFlatpakSession::start(
        1,
        APP,
        TAB,
        w,
        h,
        &tilo_window_share::config::FlatpakShareConfig::default(),
    )
    .expect("a share of the window");
    watching.show(*host.as_bytes(), &[Track::Window(TAB, Layer::High)]);
    let deadline = Instant::now() + Duration::from_secs(20);
    while sharing.wanted_windows() != [TAB] {
        assert!(Instant::now() < deadline, "the window's track is asked for");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        sharing.take_window_keyframes(),
        [TAB],
        "a new viewer starts at a keyframe"
    );

    // The viewer: what its plane opened, and when each picture was decoded.
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let viewing = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let tree = showing(host, APP, TAB);
            let mut tiles = tilo_desktop_surface::PeerSurfaces::default();
            let peer = neighbour_wire::peer_handle(host.as_bytes());
            let mut captured: BTreeMap<u64, u64> = BTreeMap::new();
            let (mut heard, mut decoded) = (Vec::new(), Vec::new());
            let mut last_seq = None;
            let deadline = Instant::now() + Duration::from_secs(120);
            while decoded.len() < PICTURES && Instant::now() < deadline {
                while let Some(object) = watching.try_recv() {
                    if object.track != Track::Window(TAB, Layer::High) {
                        continue;
                    }
                    let object = VideoObject::decode(&object.body).expect("a picture");
                    heard.push(wall_us().saturating_sub(object.timestamp_us));
                    if object.keyframe && tiles.read(TAB).is_none() {
                        let start = super::picture_start(&tree, 7, peer, TAB, &object)
                            .expect("a codec this build decodes");
                        tiles
                            .start(&tree, &start)
                            .expect("the tab shows the window");
                    }
                    captured.insert(object.sequence, object.timestamp_us);
                    tiles.push(peer, TAB, object.sequence, &object.payload);
                }
                tiles.collect_pictures();
                if let Some(read) = tiles
                    .read(TAB)
                    .filter(tilo_desktop_surface::PeerRead::pictured)
                    && last_seq != Some(read.picture_seq)
                {
                    // The desk paints what the tile holds, which lets it take the next picture.
                    if let Some(tile) = tiles.tile(TAB) {
                        tile.borrow_mut().mark_drawn();
                    }
                    last_seq = Some(read.picture_seq);
                    if let Some(at) = captured.get(&read.picture_seq) {
                        decoded.push(wall_us().saturating_sub(*at));
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            done.store(true, std::sync::atomic::Ordering::Release);
            (heard, decoded)
        })
    };

    // The host: a window that changes every frame, drawn ahead so drawing it costs nothing,
    // captured at sixty frames a second and encoded as a share encodes it.
    let frames: Vec<Vec<u8>> = (0..8_usize)
        .map(|step| {
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    let tone = ((x as usize + y as usize + step * 31) % 256) as u8;
                    rgba.extend_from_slice(&[tone, 255 - tone, (step * 29 % 256) as u8, 255]);
                }
            }
            rgba
        })
        .collect();
    let mut encoding: Vec<u64> = Vec::new();
    let interval = Duration::from_micros(16_667);
    let mut step = 0_usize;
    let mut next = Instant::now();
    while !done.load(std::sync::atomic::Ordering::Acquire) {
        if Instant::now() >= next {
            share.deposit_captured_rgba(frames[step % frames.len()].clone());
            step += 1;
            next += interval;
        }
        for encoded in share.pump(Instant::now()) {
            encoding.push(wall_us().saturating_sub(share.captured_at_us(&encoded)));
            let _ = super::send_picture(&sharing, TAB, &share, encoded);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let (mut heard, mut decoded) = viewing.join().expect("the viewer");
    assert!(
        decoded.len() >= PICTURES,
        "only {} pictures were decoded",
        decoded.len()
    );
    let (encoded_p50, encoded_p95) = percentiles(&mut encoding);
    let (heard_p50, heard_p95) = percentiles(&mut heard);
    let (p50, p95) = percentiles(&mut decoded);
    println!(
        "a shared {w}x{h} window over the live plane, {} pictures: capture to encoded p50 {encoded_p50:.1} ms p95 {encoded_p95:.1} ms; to the viewer's plane p50 {heard_p50:.1} ms p95 {heard_p95:.1} ms; to decoded p50 {p50:.1} ms p95 {p95:.1} ms",
        decoded.len()
    );
    // What the plane adds besides the encode and the decode is small next to a frame.
    assert!(
        heard_p50 - encoded_p50 < 16.7,
        "the plane carries a picture within a frame: encoded p50 {encoded_p50:.1} ms, heard p50 {heard_p50:.1} ms"
    );
}
