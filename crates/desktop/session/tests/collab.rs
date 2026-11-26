//! Who is here: the bar's faces open the roster, which lists everyone in the workspace and where
//! each of them is, and the roster reads its one feed from the peer network. A face opens
//! the person's card, from which Message opens Chat on the conversation they have.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use collab_types::presence::{Payload, Presence};
use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use runtime_collab::CollabClient;
use tilo_actions::ApiError;
use tilo_actions::remote::RemoteActionTarget;
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop_model::{
    GroupId, Layout, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId,
};
use tilo_desktop_search::SHOW_PEOPLE;
use tilo_desktop_search::providers::AppEntry;
use tilo_session::sync::{Client, Event, OwnPresence};
use tilo_session::{AppActions, AppLists, AppSocket, Launches, Lists, Options, Session};
use tilo_session_core::desktop::verbs::{
    CHROME_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND, SHELL_RESOURCE,
};

use crate::common::{press as press_named, shot, until, wait_for};

type Arguments = serde_json::Map<String, serde_json::Value>;

const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
/// The tab the person at the other device has their pointer over.
const NOTES: u64 = 7;
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

fn device(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse(WORKSPACE).expect("a workspace id")
}

/// A shell showing one workspace it takes part in, with one window called Notes: where the other
/// person is working.
fn shell() -> Layout {
    let workspace = workspace_id();
    let mut workspace = Workspace::new(workspace, "Session");
    let tab = Tab {
        id: TabId::from(NOTES),
        title: "Notes".to_owned(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".to_owned(),
            label: "Notes".to_owned(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let mut strip = Strip::new(StripId::from(1), "Main");
    strip.window_groups.push(WindowGroup {
        id: GroupId::from(1_u64),
        tabs: vec![tab],
        active_tab: TabId::from(NOTES),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    workspace.strips.push(strip);
    Layout {
        workspaces: vec![workspace.clone()],
        active: workspace.id(),
        ..Layout::default()
    }
}

/// One thing the other device said in the workspace.
fn said(from: u8, msg: NeighbourMsg) -> Event {
    Event::Msg {
        ws: workspace_id(),
        peer: device(from),
        msg: Box::new(msg),
    }
}

/// The person `seed` is, in the hex everyone here knows them by.
fn person(seed: u8) -> String {
    hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

/// A device's identity: the person it is a device of, and its name here.
fn hello(from: u8, label: &str) -> Event {
    hello_of(from, from, label)
}

/// The device `device` of the person `person` : their certificate here.
fn hello_of(person: u8, at: u8, label: &str) -> Event {
    let device_key = device(at);
    let root = ed25519_dalek::SigningKey::from_bytes(&[person; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *device_key.as_bytes(),
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
    said(
        at,
        NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        },
    )
}

/// One presence the other device said on the workspace's live session: they arrived, how available
/// they are, and so on.
fn presence(sync: &Client, from: u8, payload: collab_types::PeerPresencePayload) {
    let payload = match payload {
        collab_types::PeerPresencePayload::Arrived => Payload::Arrived,
        collab_types::PeerPresencePayload::Left => Payload::Left,
        collab_types::PeerPresencePayload::Status { text, automatic } => {
            Payload::Status { text, automatic }
        }
        collab_types::PeerPresencePayload::Availability {
            state,
            call,
            camera,
            screen,
        } => Payload::Availability {
            state,
            call,
            camera,
            screen,
        },
        collab_types::PeerPresencePayload::Person { name } => Payload::Person { name },
        collab_types::PeerPresencePayload::Following { target } => Payload::Following { target },
        collab_types::PeerPresencePayload::Wave { seq, target } => Payload::Wave { seq, target },
        other => panic!("the tests say no {other:?} as a desktop"),
    };
    sync.heard_presence(
        WORKSPACE,
        device(from),
        &Presence::desktop(WORKSPACE, payload),
    );
}

/// The device `from` arrives, and says whose device it is: the person's own name for themselves,
/// which is what everyone here calls them.
fn arrived_as(sync: &Client, from: u8, name: &str) {
    presence(
        sync,
        from,
        collab_types::PeerPresencePayload::Person {
            name: name.to_owned(),
        },
    );
    presence(sync, from, collab_types::PeerPresencePayload::Arrived);
}

/// Their pointer, over the Notes window.
fn pointer(sync: &Client, from: u8, x: f32, y: f32) {
    let frame = Presence::pointing(WORKSPACE, NOTES, x, y);
    sync.heard_presence(WORKSPACE, device(from), &frame);
}

/// Everyone's pointer as a shell showing `tiles` draws it: the tab each is over, and where it stands
/// on the surface.
#[expect(
    clippy::cast_precision_loss,
    reason = "milliseconds since the epoch are exact in an f64 for the life of a session"
)]
fn done_by(sync: &Client, tiles: &[tilo_live::presence::Tile]) -> Vec<(u64, f64, f64)> {
    let mut chats = ui::presence::CursorChats::default();
    let members = sync.members(WORKSPACE);
    let faces = sync.here().borrow().faces();
    let presence = ui::presence::PresenceCtx::empty();
    let pointers = tilo_session_core::presence::pointers(
        &members,
        &faces,
        tiles,
        &mut chats,
        &presence,
        ui::time::Instant::now(),
        ui::time::unix_now_ms() as f64,
        ui::tokens::EffectiveMotion::default(),
    );
    pointers
        .drawn
        .iter()
        .zip(pointers.tabs)
        .map(|(cursor, tab)| (tab, cursor.pos.0, cursor.pos.1))
        .collect()
}

/// A session taking part in the workspace through `sync` , showing `shell` .
fn session(sync: &Rc<Client>) -> Session {
    session_over(sync, None)
}

/// [`session`], with `apps` answering what an app asks or is asked to do.
fn session_over(sync: &Rc<Client>, apps: Option<Rc<dyn AppActions>>) -> Session {
    showing(sync, apps, shell())
}

/// A session taking part in the workspace through `sync` , showing `tree` .
fn showing(sync: &Rc<Client>, apps: Option<Rc<dyn AppActions>>, tree: Layout) -> Session {
    showing_on(WIDTH, HEIGHT, sync, apps, tree)
}

/// [`showing`] on a display `width` by `height` : a phone's, where the bar is narrow.
fn showing_on(
    width: i32,
    height: i32,
    sync: &Rc<Client>,
    apps: Option<Rc<dyn AppActions>>,
    tree: Layout,
) -> Session {
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, height)],
        Options {
            sync: Some(Rc::clone(sync)),
            apps,
            ..Options::default()
        },
    );
    session.handle().replace_tree(tree);
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    session
}

/// The window of Chat, which Ada is not in.
const CHAT_TAB: u64 = 9;

/// [`shell`], with Chat in front of Notes.
fn shell_of_two() -> Layout {
    let mut tree = shell();
    let workspace = tree.workspaces.first_mut().expect("a workspace");
    workspace.strips[0].window_groups.push(WindowGroup {
        id: GroupId::from(2_u64),
        tabs: vec![Tab {
            id: TabId::from(CHAT_TAB),
            title: "Chat".to_owned(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.chat".to_owned(),
                label: "Chat".to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: TabId::from(CHAT_TAB),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    tree.active_group
        .insert(StripId::from(1), GroupId::from(2_u64));
    tree
}

/// The window in front of the shell `session` shows.
fn front(session: &Session) -> Option<TabId> {
    session.handle().read(|tree| {
        tilo_session_core::desktop::navigation::active_route(tree).map(|route| route.tab)
    })
}

/// `device`'s page says where its windows are, and that the person works in `app`.
fn page_working_in(sync: &Client, from: u8, app: &str) {
    let shell = collab_types::presence::ShellPresence {
        seq: 1,
        sent_at_ms: 0,
        label: "Ada".to_owned(),
        agent: false,
        colour: String::new(),
        activity: None,
        place: None,
        size: [WIDTH.cast_unsigned(), HEIGHT.cast_unsigned()],
        windows: vec![collab_types::presence::ShellWindow {
            app: app.to_owned(),
            document: [0; 8],
            rect: [0, 0, 10_000, 10_000],
            focused: true,
        }],
        selection: None,
        chat: None,
        reaction: None,
    };
    sync.heard_presence(
        WORKSPACE,
        device(from),
        &Presence::desktop(WORKSPACE, Payload::Shell(Box::new(shell))),
    );
}

/// An action host that keeps what it was asked to run, and starts nothing.
#[derive(Default)]
struct Host {
    invoked: RefCell<Vec<(RemoteActionTarget, Arguments)>>,
}

impl AppActions for Host {
    fn invoke(&self, target: RemoteActionTarget, arguments: Arguments) -> Result<(), ApiError> {
        self.invoked.borrow_mut().push((target, arguments));
        Ok(())
    }
}

fn press_faces(session: &mut Session) {
    session
        .invoke(CHROME_RESOURCE, SHOW_PEOPLE, &Arguments::new())
        .expect("the bar's faces open who is here");
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The person this test's session is a device of, at another of their devices too.
const QUINTEN: u8 = 0xf5;
/// The channel a call in the workspace is in.
const CHANNEL: &str = "c_standup";

/// The channel's call, as a device in it publishes it.
fn channel_call() -> String {
    ui::CallPresence {
        channel: CHANNEL.to_owned(),
        name: "Standup".to_owned(),
        bridge: None,
    }
    .encode()
}

/// What the palette calls the channel's call row: what choosing it joins.
fn call_row_label() -> String {
    tilo_desktop_search::i18n::t_args("search-launcher-call-in", &[("channel", "Standup".into())])
}

/// The resource `id` , however deep the shell nests it.
fn find(resources: &[tilo_actions::ResourceSpec], id: &str) -> Option<tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// What the empty palette offers now, row by row: the calls in progress are among them, and what
/// choosing one joins.
fn palette_rows(session: &mut Session) -> Vec<String> {
    // The palette lists what this device saw of who is here as of the last pass, so a pass runs
    // before it opens.
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    session
        .invoke(SHELL_RESOURCE, "search", &Arguments::new())
        .expect("the palette opens");
    for _ in 0..8 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let search = find(&session.resources(), SEARCH_RESOURCE);
    search
        .map(|search| {
            search
                .children
                .iter()
                .filter(|row| row.kind == SEARCH_ROW_KIND)
                .map(|row| row.label.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The channel's call is offered to join in the empty palette, and not once this person is in it:
/// another device of theirs says so, as the presence a device publishes when it joins a call does.
#[test]
fn a_call_is_offered_until_this_person_is_in_it_on_another_device() {
    let _settled = ui::motion::testing::settled();
    let mine = OwnPresence {
        label: "Quinten".to_owned(),
        user: Some(person(QUINTEN)),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf1), mine));
    let mut session = session(&sync);

    // Someone else, here and in the channel's call with their camera on.
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    presence(
        &sync,
        0xaa,
        collab_types::PeerPresencePayload::Availability {
            state: "available".to_owned(),
            call: Some(channel_call()),
            camera: true,
            screen: false,
        },
    );
    let rows = palette_rows(&mut session);
    assert!(
        rows.contains(&call_row_label()),
        "the call in progress is offered to join: {rows:?}"
    );

    // This person's laptop joins the same call: its presence says so, and this device offers no
    // second join.
    sync.hear(hello_of(QUINTEN, 0xf0, "ThinkPad"));
    arrived_as(&sync, 0xf0, "Quinten");
    presence(
        &sync,
        0xf0,
        collab_types::PeerPresencePayload::Availability {
            state: "available".to_owned(),
            call: Some(channel_call()),
            camera: false,
            screen: false,
        },
    );
    let rows = palette_rows(&mut session);
    assert!(
        !rows.contains(&call_row_label()),
        "no join for a call this person is in: {rows:?}"
    );
}

#[test]
fn the_people_button_lists_who_is_here_and_where_their_pointers_are() {
    let _settled = ui::motion::testing::settled();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = session(&sync);

    // Another person, at their own device, in the same workspace: here, heads down, with their
    // pointer over Notes.
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    presence(
        &sync,
        0xaa,
        collab_types::PeerPresencePayload::Availability {
            state: "away".to_owned(),
            call: None,
            camera: false,
            screen: false,
        },
    );
    pointer(&sync, 0xaa, 12.0, 34.0);
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    press_faces(&mut session);
    let labels = session.accessible_labels();
    let listed = |what: &str| labels.iter().any(|label| label.contains(what));
    assert!(listed("Ada"), "who is here: {labels:?}");
    assert!(
        !listed("Framework 16"),
        "a person is named, never their machine: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Ada is in Notes"),
        "where their pointer is: {labels:?}"
    );
    assert!(listed("Away"), "how they said they are: {labels:?}");
    let pointers = done_by(
        &sync,
        &[tilo_live::presence::Tile::new(
            NOTES, 0.0, 0.0, 1000.0, 1000.0,
        )],
    );
    let [(tab, x, y)] = pointers.as_slice() else {
        panic!("their pointer is over Notes, where the session draws it: {pointers:?}");
    };
    assert_eq!((*tab, *x, *y), (NOTES, 12.0, 34.0));

    // The faces again: the roster goes back to them.
    press_faces(&mut session);
    let labels = session.accessible_labels();
    drop(session);
    assert!(
        !labels.iter().any(|label| label == "Ada is in Notes"),
        "the roster closed: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Ada"),
        "their face still names them from the bar: {labels:?}"
    );
}

#[test]
fn someone_away_is_dimmed_in_the_roster_and_someone_here_is_not() {
    let _settled = ui::motion::testing::settled();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = session(&sync);

    // Two people here: one at their machine, one away from it.
    for (seed, device, name, state) in [
        (0xaa, "Framework 16", "Ada", "available"),
        (0xbb, "ThinkPad", "Grace", "away"),
    ] {
        sync.hear(hello(seed, device));
        arrived_as(&sync, seed, name);
        presence(
            &sync,
            seed,
            collab_types::PeerPresencePayload::Availability {
                state: state.to_owned(),
                call: None,
                camera: false,
                screen: false,
            },
        );
    }
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    press_faces(&mut session);
    let said = said_by_the_shell(&session);
    assert!(
        said.iter().any(|word| word == "Ada"),
        "both are here: {said:?}"
    );
    assert!(
        said.iter().any(|word| word == "Grace"),
        "both are here: {said:?}"
    );
    assert!(
        !said.iter().any(|word| word == "Framework 16"),
        "a person is named, never their machine: {said:?}"
    );
    // How available a person is is said of them, and only of them: the one who is at their machine
    // is not told apart from the one who is away by their name alone.
    assert_eq!(
        nodes_saying(&session, "Away"),
        1,
        "one of the two is away: {said:?}"
    );

    // They are back at their machine, and say so: their face lightens.
    presence(
        &sync,
        0xbb,
        collab_types::PeerPresencePayload::Availability {
            state: "focusing".to_owned(),
            call: None,
            camera: false,
            screen: false,
        },
    );
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let said = said_by_the_shell(&session);
    assert_eq!(
        nodes_saying(&session, "Away"),
        0,
        "nobody is away now: {said:?}"
    );

    // They join a voice channel: the roster says which one, and that is what people read of them
    // rather than how available they are.
    presence(
        &sync,
        0xbb,
        collab_types::PeerPresencePayload::Availability {
            state: "available".to_owned(),
            call: Some(
                ui::CallPresence {
                    channel: "c_standup".to_owned(),
                    name: "Standup".to_owned(),
                    bridge: None,
                }
                .encode(),
            ),
            camera: false,
            screen: false,
        },
    );
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let said = said_by_the_shell(&session);
    drop(session);
    assert!(
        said.iter().any(|word| word.contains("Standup")),
        "the call they are in: {said:?}"
    );
}

/// The app that holds conversations.
const CHAT: &str = "so.tilo.chat";

/// Install `app` in `store` , as the package landing there leaves it: the manifest an open reads
/// and the registry row the catalogue lists it by.
fn install(store: &Path, app: &str, name: &str) {
    let payload = store.join("payload").join(app);
    std::fs::create_dir_all(payload.join("bin")).expect("the package's own directory");
    let manifest = payload.join("manifest.toml");
    std::fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{app}\"\n\
             version = \"1.0.0\"\n\
             kind = \"app\"\n\
             name = \"{name}\"\n\
             \n\
             [publisher]\n\
             identity = \"ed25519:{key}\"\n\
             signature = \"\"\n\
             \n\
             [sandbox]\n\
             namespaces = [\"user\"]\n\
             seccomp = \"tilo-default\"\n\
             landlock = \"strict\"\n\
             \n\
             [sandbox.net]\n\
             mode = \"off\"\n\
             \n\
             [entrypoint]\n\
             bin = \"bin/app\"\n\
             type = \"gui\"\n",
            key = "11".repeat(32),
        ),
    )
    .expect("the package's manifest");
    std::fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{app}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .expect("the store's registry");
}

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// The control `names` names, where the session shows it: the bar's face of a person, a button of
/// the roster or of a card.
fn control(session: &Session, names: &[&str]) -> Option<(f64, f64, f64, f64)> {
    session
        .controls()
        .into_iter()
        .find(|control| names.iter().any(|wanted| control.name == *wanted))
        .map(|control| control.rect)
}

/// A press at the centre of `rect` , through the compositor's own input.
fn press(session: &mut Session, (x, y, width, height): (f64, f64, f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: x + width / 2.0,
            y: y + height / 2.0,
        })
        .expect("the pointer");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Ada is here on two of her devices, heard once the shell is up: presence written before the
/// session has read its workspace is what its own start replaces, and her face in the bar is what
/// says it landed.
fn ada_arrives(session: &mut Session) {
    wait_for(session, "the bar", |session| {
        session.shell_control("Top bar")
    });
    let mut heard = 0_u32;
    until(session, "their face is in the bar", |session| {
        if session.shell_control("Ada").is_some() {
            return true;
        }
        heard = heard.saturating_add(1);
        if heard % 20 != 1 {
            return false;
        }
        // Her two devices, each with what it says of her, as the roster reads who is here: the
        // certificate names the person and the key a conversation with her is kept on.
        session
            .sync()
            .expect("the session takes part in the app wire")
            .here()
            .borrow_mut()
            .set_members(
                None,
                Some(vec![
                    tilo_session_core::desktop::here::LiveMember {
                        publisher: device(0xaa).to_string(),
                        kind: "person".to_owned(),
                        label: "Ada".to_owned(),
                        device: "Framework 16".to_owned(),
                        user: Some(person(0xaa)),
                        availability: Some("available".to_owned()),
                        ..tilo_session_core::desktop::here::LiveMember::default()
                    },
                    tilo_session_core::desktop::here::LiveMember {
                        publisher: device(0xac).to_string(),
                        kind: "person".to_owned(),
                        label: "Ada".to_owned(),
                        device: "ThinkPad".to_owned(),
                        user: Some(person(0xaa)),
                        availability: Some("focusing".to_owned()),
                        ..tilo_session_core::desktop::here::LiveMember::default()
                    },
                ]),
            );
        false
    });
}

/// What the card says of Ada: her name in its head, the person's root key for its ID, and each of
/// her devices with how it said it was, in who is here's place rather than beside it.
fn assert_ada_card(session: &Session) {
    let labels = session.accessible_labels();
    assert!(
        session.shell_says("Ada"),
        "the card is the person's: {labels:?}"
    );
    assert!(
        session.shell_says("Person ID"),
        "the card's ID row: {labels:?}"
    );
    assert!(
        session.shell_says(&tilo_session_core::desktop::profile::short_key(&person(
            0xaa
        ))),
        "the ID is the person's root key: {labels:?}"
    );
    for (device, presence) in [("Framework 16", "Available"), ("ThinkPad", "Focusing")] {
        assert!(
            session.shell_says(device),
            "each device as it calls itself: {labels:?}"
        );
        assert!(
            session.shell_says(presence),
            "and how it said it was: {labels:?}"
        );
    }
    assert!(
        !session.shell_says("People here now"),
        "the card stands in the roster's place: the roster is gone, {labels:?}"
    );
}

/// The node `label` names, as the screen reader an assistive client is reads it, and the place
/// of the window the shell exports it in.
fn a11y_node(session: &Session, label: &str) -> (usize, ui::accesskit::NodeId) {
    let windows = session.a11y_windows();
    for (place, _, tree) in &windows {
        if let Some((node, _)) = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
        {
            return (*place, *node);
        }
    }
    let said: Vec<&str> = windows
        .iter()
        .flat_map(|(_, _, tree)| tree.nodes.iter().filter_map(|(_, node)| node.label()))
        .collect();
    panic!("no node named {label:?}; the shell says {said:?}")
}

/// A press on a person's face in the bar opens their card, and Message opens
/// Chat on the conversation they have: the same card the web shell shows,
/// handed the app invocation that reveals it.
#[test]
fn a_persons_face_opens_a_card_whose_message_opens_chat_on_their_conversation() {
    message_from_the_card(press);
}

/// Message pressed as assistive technology and the session's control press it: the card and the
/// roster leave as they do for the pointer, so the keyboard goes to the conversation.
#[test]
fn message_pressed_by_name_leaves_the_roster_too() {
    message_from_the_card(|session, _| {
        // Pressed through the window the shell exports, as a screen reader activates the control
        // it reads.
        let (place, message) = a11y_node(session, "Message");
        assert!(
            session.a11y_perform(place, message, &ui::a11y::Action::Activate),
            "the shell took the press its window offers"
        );
        session.step(REFRESH).expect("a turn of the session");
    });
}

/// Open Ada's card from her face, press its Message with `press_message`, and check what came of
/// it.
fn message_from_the_card(press_message: impl FnOnce(&mut Session, (f64, f64, f64, f64))) {
    let _settled = ui::motion::testing::settled();
    let home = tempfile::tempdir().expect("a home for this test's app store");
    let store = home.path().join("tilo-apps");
    install(&store, CHAT, "Chat");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", &store);

    let host = Rc::new(Host::default());
    let mut session = crate::common::headless(Options {
        apps: Some(host.clone() as Rc<dyn AppActions>),
        // Chat stands on this device, as its lists name it: the card offers a conversation only
        // where the app that holds one is here.
        lists: Lists::fixed(AppLists {
            applications: vec![AppEntry {
                id: CHAT.to_owned(),
                title: "Chat".to_owned(),
                category: None,
            }],
            ..AppLists::default()
        }),
        ..Options::default()
    });

    // Another person, at two of their devices, in the same workspace: their face stands in the bar,
    // under their name and never their machine's.
    ada_arrives(&mut session);
    // Who is here stands open, listing the people here.
    press_faces(&mut session);
    until(&mut session, "the roster lists who is here", |session| {
        session.shell_says("People here now")
    });
    assert!(
        session.shell_control("Framework 16").is_none(),
        "the face names the person, never their machine: {:?}",
        session.accessible_labels()
    );

    // A press on their face opens their card.
    press_named(&mut session, "Ada");
    let message = wait_for(&mut session, "the card opens", |session| {
        session.shell_control("Message")
    });
    assert!(
        session.shell_control("Message").is_some(),
        "the card offers Message: {:?}",
        session.accessible_labels()
    );
    assert_ada_card(&session);
    shot(&mut session, "people-card-message");

    // Message hands Chat the conversation with them: the app's own `open-direct-conversation` on
    // the row their two root keys name.
    press_message(&mut session, message);
    until(&mut session, "Chat is handed the conversation", |_| {
        !host.invoked.borrow().is_empty()
    });
    let invoked = host.invoked.borrow();
    let [(target, arguments)] = invoked.as_slice() else {
        panic!("one invocation, not {invoked:?}");
    };
    assert!(
        target.provider.ends_with(&format!(":{CHAT}")),
        "the host reaches Chat: {target:?}"
    );
    assert_eq!(
        target.address.resource.as_str(),
        tilo_app_id::chat::CONVERSATIONS,
        "Chat's one record of the conversations"
    );
    assert_eq!(
        target.address.action.as_str(),
        tilo_app_id::chat::MESSAGE_PERSON,
        "Chat opens the conversation with them"
    );
    // The record is everyone's; the person is the one the message names, by their root key.
    assert_eq!(
        arguments
            .get("who")
            .and_then(|who| who.get("person"))
            .and_then(serde_json::Value::as_str),
        Some(person(0xaa).as_str()),
        "the conversation with them"
    );

    // The card closed behind the choice, and the roster with it: the person was taken to the
    // conversation, where the keyboard goes.
    session.step(REFRESH).expect("a turn of the session");
    assert!(
        session.shell_control("Message").is_none(),
        "the card closed: {:?}",
        session.accessible_labels()
    );
    assert!(
        !session.shell_says("People"),
        "the roster left with the card: {:?}",
        session.accessible_labels()
    );
}

/// A face's card offers everyone here: where the bar shows a face in place of its people button
/// (a phone's bar), the roster is one press from the person, not two.
#[test]
fn a_persons_card_offers_everyone_here() {
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options::default());
    ada_arrives(&mut session);
    press_faces(&mut session);
    press_named(&mut session, "Ada");
    wait_for(&mut session, "the card offers everyone here", |session| {
        session.shell_control("Back to everyone")
    });
    assert!(
        session.shell_control("Back to everyone").is_some(),
        "the card offers everyone here: {:?}",
        session.accessible_labels()
    );
    press_named(&mut session, "Back to everyone");
    // The card leaves before what is read of the roster: the choice reaches the session's
    // reading, and the shell draws the roster of it on a later frame.
    until(
        &mut session,
        "the roster stands in the card's place",
        |session| session.shell_control("Back to everyone").is_none(),
    );
    assert!(
        session.shell_control("Back to everyone").is_none(),
        "the card gave way to the roster: {:?}",
        session.accessible_labels()
    );
    assert!(
        session.shell_says("People here now"),
        "the roster stands in the card's place: {:?}",
        session.accessible_labels()
    );
    assert!(
        session.shell_says("Ada"),
        "she is a row of everyone here: {:?}",
        session.accessible_labels()
    );
}

/// Your avatar opens a card with the name others see and ways to add or manage devices.
#[test]
fn your_own_avatar_opens_your_card() {
    let _settled = ui::motion::testing::settled();
    own_card(ui::headless::fixture_runner_inputs, "own-card");
    own_card(
        || ui::application::ComponentRunnerInputs {
            theme: ui::Theme::light(),
            ..ui::headless::fixture_runner_inputs()
        },
        "own-card-light",
    );
}

fn own_card(presentation: fn() -> ui::application::ComponentRunnerInputs, capture: &str) {
    let mine = OwnPresence {
        label: "Quinten".to_owned(),
        user: Some(person(QUINTEN)),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), mine));
    let mut session = Session::headless(
        Options {
            sync: Some(sync),
            // The person at this device: the name they go by, never the account.
            person: "Quinten".to_owned(),
            presentation,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session");
    session.handle().replace_tree(shell());
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    // The bar draws whose it is: the name they gave, not the account they happen to log in with,
    // and one face drawn in one colour.
    let named = wait_for(&mut session, "the bar shows whose it is", |session| {
        session
            .accessible_labels()
            .into_iter()
            .find(|label| label.ends_with(": open your card"))
    });
    assert_eq!(
        named, "Quinten: open your card",
        "the bar names the person as they gave themselves"
    );
    press_named(&mut session, &named);
    // The card's name field holds the name they gave: a person this identity has not named is
    // asked for one, and its words are the field's name.
    until(&mut session, "your card opens", |session| {
        session.shell_control("Your name").is_some()
            || session.shell_control("Add your name").is_some()
    });
    let labels = session.accessible_labels();
    for said in [
        "How everyone here sees you",
        "Settings",
        "Add a device",
        "Devices and access",
    ] {
        assert!(
            labels.iter().any(|label| label == said),
            "your own card says {said:?}: {labels:?}"
        );
    }
    for removed in [
        "Message",
        "Person ID",
        "Copy this device’s key",
        "Your identities",
        "Back up your identity",
        "Connect an agent or computer",
    ] {
        assert!(
            !labels.iter().any(|label| label == removed),
            "your own card leaves {removed:?} elsewhere: {labels:?}"
        );
    }
    shot(&mut session, capture);

    assert!(
        session.shell_says("People"),
        "one card, the same surface another person's opens in: {:?}",
        session.accessible_labels()
    );
}

/// Following someone moves this view with them: the window of the app their page says they work in
/// comes forward, and stays where it is while their place does not move. They leaving ends the
/// following, and the workspace hears this device follows nobody.
#[test]
fn following_someone_brings_forward_the_window_of_the_app_they_work_in() {
    let _settled = ui::motion::testing::settled();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = showing(&sync, None, shell_of_two());
    assert_eq!(
        front(&session),
        Some(TabId::from(CHAT_TAB)),
        "Chat is the window the person at this device works in"
    );

    // Ada is here, and her page says she works in Notes.
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    page_working_in(&sync, 0xaa, "so.tilo.notes");
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    // This device follows her, chosen in the bar's roster: the card her row opens offers it.
    follow_from_who_is_here(&mut session, "Ada");
    until(
        &mut session,
        "the window of the app Ada works in comes forward",
        |session| front(session) == Some(TabId::from(NOTES)),
    );

    // Their place does not move: neither does the view.
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(
        front(&session),
        Some(TabId::from(NOTES)),
        "the view stays where it was brought"
    );

    // They leave: following ends with them, and the workspace hears it.
    presence(&sync, 0xaa, collab_types::PeerPresencePayload::Left);
    until(&mut session, "following ends when they leave", |_| {
        sync.own().following.is_none()
    });
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert!(
        !session.shell_says("Stopped following"),
        "someone who left takes the following with them, and the bar says nothing of it: {:?}",
        session.accessible_labels()
    );
}

/// The route the Notes window's app holds in the workspace.
fn notes_route() -> String {
    tilo_app_host::route::CrdtRoute::app("so.tilo.notes", WORKSPACE).channel_key()
}

/// The app of the Notes window: it joins the workspace's route, and says whom each follow event
/// names once it is on it.
fn the_notes_app(
    socket: &Path,
    joined: std::sync::mpsc::Sender<()>,
    told: std::sync::mpsc::Sender<Option<tilo_actions::MemberId>>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let mut stream =
            std::os::unix::net::UnixStream::connect(socket).expect("the app wire's socket");
        launcher::services::write_collab_handoff(&mut stream, "so.tilo.notes", WORKSPACE)
            .expect("the launcher names the app on the connection");
        stream
            .set_nonblocking(true)
            .expect("the connection is ready for the runtime");
        let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
        let (reader, writer) = stream.into_split();
        let (client, carrying) = CollabClient::over(reader, writer);
        tokio::spawn(carrying);
        let handle = crate::common::held::opened(&client, &notes_route())
            .await
            .expect("the app the session launched joins the workspace's route");
        let context = handle
            .subscribe_context()
            .await
            .expect("the app follows the session's context");
        joined.send(()).expect("the test waits for the app");
        while let Ok(change) = context.read().await {
            let Some(peer) = change.follow_changed else {
                continue;
            };
            if told.send(peer).is_err() {
                return;
            }
        }
    });
}

/// A session holding the fixture's workspace, its app wire on `socket`, taking part in the
/// workspace through `sync`, showing `tree`: where an app of the workspace joins and hears the
/// session's own word.
fn app_session(
    socket: &Path,
    held: &crate::common::held::Fixture,
    sync: &Rc<Client>,
    tree: Layout,
) -> crate::common::held::Hosted {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process.
    let launches = std::sync::Arc::new(Launches::default());
    launches.started(std::process::id(), "so.tilo.notes", WORKSPACE, true);
    let mut hosted = crate::common::held::hosted(
        Options {
            sync: Some(Rc::clone(sync)),
            app_socket: AppSocket::At(socket.to_path_buf()),
            launches,
            lists: Lists::default(),
            state: Some(held.state.clone()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    );
    hosted.session.handle().replace_tree(tree);
    for _ in 0..4 {
        hosted.session.step(Duration::from_millis(1)).expect("step");
    }
    hosted
}

/// Following someone reaches the window's app: the app of the workspace's window is told which
/// member's view this person follows, and told when following ends.
#[test]
fn following_someone_tells_the_window_app_whose_view_is_theirs() {
    let _settled = ui::motion::testing::settled();
    let home = crate::common::private_home();
    let socket = home.path().join("collab.sock");
    let held = crate::common::held::fixture();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = app_session(&socket, &held, &sync, shell_of_two());

    // The app of the Notes window is on the workspace's route.
    let (joined, app_joined) = std::sync::mpsc::channel();
    let (told, hears) = std::sync::mpsc::channel();
    let app = std::thread::spawn(move || the_notes_app(&socket, joined, told));
    until(&mut session, "the app joins the workspace's route", |_| {
        app_joined.try_recv().is_ok()
    });

    // Ada is here, and her page says she works in Notes.
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    page_working_in(&sync, 0xaa, "so.tilo.notes");
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    // This device follows her: the app hears whose view the person at this device follows.
    let mut own = sync.own();
    own.following = Some(device(0xaa).to_string());
    sync.set_own(own);
    let followed = wait_for(
        &mut session,
        "the app hears whom the person follows",
        |_| hears.try_recv().ok(),
    );
    assert_eq!(
        followed,
        tilo_actions::MemberId::parsed_person(&person(0xaa)),
        "the app is told the member the person follows, not the device"
    );

    // The person stops following: the app hears that following ended.
    let mut own = sync.own();
    own.following = None;
    sync.set_own(own);
    let ended = wait_for(&mut session, "the app hears following end", |_| {
        hears.try_recv().ok()
    });
    assert_eq!(ended, None, "the app is told nobody is followed");

    drop(session);
    app.join().expect("the app's thread");
}

/// `device`'s page says where its windows stand: `windows` on a screen `size` big, each window
/// showing `app`'s record `record`.
fn screen_of(
    sync: &Client,
    from: u8,
    size: [u32; 2],
    windows: Vec<(&str, [u8; 8], [u16; 4], bool)>,
) {
    let shell = collab_types::presence::ShellPresence {
        seq: 2,
        sent_at_ms: 0,
        label: "Ada".to_owned(),
        agent: false,
        colour: String::new(),
        activity: None,
        place: None,
        size,
        windows: windows
            .into_iter()
            .map(
                |(app, record, rect, focused)| collab_types::presence::ShellWindow {
                    app: app.to_owned(),
                    document: record,
                    rect,
                    focused,
                },
            )
            .collect(),
        selection: None,
        chat: None,
        reaction: None,
    };
    sync.heard_presence(
        WORKSPACE,
        device(from),
        &Presence::desktop(WORKSPACE, Payload::Shell(Box::new(shell))),
    );
}

/// The people panel as a screen reader reads it: its window and tree.
fn people(session: &Session) -> ui::accesskit::TreeUpdate {
    let windows = session.a11y_windows();
    let Some((_, _, tree)) = windows.iter().find(|(_, window, _)| window == "People") else {
        let named: Vec<&str> = windows
            .iter()
            .map(|(_, window, _)| window.as_str())
            .collect();
        panic!("the session publishes the people panel it draws: {named:?}");
    };
    tree.clone()
}

/// Every name the panel says.
fn said_by(tree: &ui::accesskit::TreeUpdate) -> Vec<String> {
    tree.nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

/// Where the control `label` of `tree` stands.
fn holding(tree: &ui::accesskit::TreeUpdate, label: &str) -> (f64, f64, f64, f64) {
    let Some((_, node)) = tree
        .nodes
        .iter()
        .find(|(_, node)| node.label().is_some_and(|said| said.contains(label)))
    else {
        panic!(
            "no control named {label:?}; the panel says {:?}",
            said_by(tree)
        );
    };
    let bounds = node.bounds().expect("a control names where it stands");
    (
        bounds.x0,
        bounds.y0,
        bounds.x1 - bounds.x0,
        bounds.y1 - bounds.y0,
    )
}

/// A person's settings are this test's own: what it keeps there never reaches another test.
fn settings_home() -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
    let root = tempfile::tempdir().expect("a directory for the settings");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", root.path().join("config"))
        .set("XDG_DATA_HOME", root.path().join("data"))
        .set("XDG_STATE_HOME", root.path().join("state"));
    (root, env)
}

/// The roster is kept on screen at an edge, with a mirror of each person's screen: pressing one
/// follows their view, and the panel's own button puts it back in the bar.
#[test]
fn the_roster_kept_on_screen_shows_who_is_here_and_follows_them() {
    let _settled = ui::motion::testing::settled();
    let (_home, _settings) = settings_home();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = session(&sync);

    // Ada, at her own device: here, with three windows on her screen, working in Chat.
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    screen_of(
        &sync,
        0xaa,
        [1600, 1000],
        vec![
            ("so.tilo.notes", [1; 8], [0, 0, 5000, 10_000], false),
            ("so.tilo.chat", [2; 8], [5000, 0, 10_000, 6000], true),
            ("so.tilo.files", [3; 8], [5000, 6000, 10_000, 10_000], false),
        ],
    );
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    // The bar's faces open the roster, and its own button keeps it on screen.
    press_faces(&mut session);
    let keep = holding(&people(&session), "Keep on screen");
    press(&mut session, keep);
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    // Kept on screen, it names who is here, and offers to follow them and to go back in the bar.
    let panel = people(&session);
    let names = said_by(&panel);
    assert!(names.iter().any(|name| name == "Ada"), "{names:?}");
    assert!(
        names.iter().any(|name| name == "Follow Ada"),
        "their card is pressed to follow them: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "Put back in the bar"),
        "{names:?}"
    );
    shot(&mut session, "roster-pinned");

    // Pressing her card follows her view, and pressing it again stops.
    let follow = holding(&people(&session), "Follow Ada");
    press(&mut session, follow);
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let ada = person(0xaa);
    assert_eq!(
        sync.own().following.as_deref(),
        Some(ada.as_str()),
        "the workspace hears whose view this is"
    );
    let panel = people(&session);
    assert!(
        said_by(&panel)
            .iter()
            .any(|name| name == "Stop following Ada"),
        "{:?}",
        said_by(&panel)
    );
    // The bar's banner says whose view this is beside the roster kept on screen: it is read
    // whether the roster is up or not.
    assert!(
        said_by(&panel).iter().any(|name| name == "Following Ada"),
        "the banner is read beside the kept roster: {:?}",
        said_by(&panel)
    );
    shot(&mut session, "roster-following");
    let stop = holding(&people(&session), "Stop following Ada");
    press(&mut session, stop);
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(sync.own().following, None, "following ends where it began");

    // Putting it back in the bar takes the panel off the shell.
    let put = holding(&people(&session), "Put back in the bar");
    press(&mut session, put);
    for _ in 0..8 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert!(
        session
            .a11y_windows()
            .iter()
            .all(|(_, window, _)| window != "People"),
        "the panel is gone from the session"
    );
}

/// While following someone the bar's chip is their face and whose view this is: a press on it,
/// away from its Stop, opens their card, as their face in the bar does, and leaves following as it
/// was.
#[test]
fn the_follow_banner_opens_the_followed_persons_card() {
    let _settled = ui::motion::testing::settled();
    let (_home, _settings) = settings_home();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = session(&sync);
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let ada = person(0xaa);
    let clock = clock_of(&session);
    // Where the clock stands: the chip that says whose view this is takes its place.
    let (clock_at, _, _, _) = wait_for(&mut session, "the bar shows the time", |session| {
        let (x, y, width, height) = session.shell_text_bounds(&clock)?;
        Some((x + width / 2.0, y, width, height))
    });
    // This device follows her, as the shell's card asks the session to: the bar says so at once.
    follow_through_the_session(&mut session, &ada);
    wait_for(
        &mut session,
        "the bar to say whose view this is",
        |session| session.shell_says("Following Ada").then_some(()),
    );
    assert!(
        !session.shell_says(&clock),
        "the clock gives way to whose view this is: {:?}",
        session.accessible_labels()
    );
    let (x, y, width, height) =
        wait_for(&mut session, "the chip of the person followed", |session| {
            session.shell_drawn_text_bounds("Following Ada")
        });
    assert!(
        (x + width / 2.0 - clock_at).abs() < 100.0,
        "the chip stands where the clock stood ({clock_at}): {x} wide {width}"
    );
    assert!(
        y + height < 80.0,
        "the chip stands in the bar: {y} + {height}"
    );
    // A press on their face, away from Stop at the chip's far end, opens their card.
    press(&mut session, (x, y, 8.0, height));
    let shown = wait_for(
        &mut session,
        "the chip's press to show a person",
        |session| {
            let reading = session.read("session:people")?;
            let showing = reading["showing"].as_str()?.to_owned();
            (!showing.is_empty()).then_some(showing)
        },
    );
    assert_eq!(
        shown,
        ada,
        "the chip opens their card, not another: {:?}",
        session.read("session:people")
    );
    // Their card stands in the roster's place at once: the head, and the way out of the view this
    // person follows. Nothing the press does may take the view back: it is a press about the
    // following itself.
    let mut trace: Vec<(String, String, bool)> = Vec::new();
    let opened = crate::common::steps_within(&mut session, Duration::from_secs(3), |session| {
        let reading = session.read("session:people");
        let said = |field: &str| {
            reading
                .as_ref()
                .and_then(|board| board[field].as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let card = session.shell_says("Stop following");
        if trace.len() < 24 || card {
            trace.push((said("following"), said("stopped"), card));
        }
        card.then_some(())
    });
    assert!(
        opened.is_some(),
        "the followed person's card opens as the chip is pressed: the trace is {trace:?}, \
         the session says {:?}",
        session.accessible_labels()
    );
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(
        sync.own().following.as_deref(),
        Some(ada.as_str()),
        "opening their card leaves following as it was"
    );
    assert!(
        session.shell_says("Following Ada"),
        "the chip stands while their card is open: {:?}",
        session.accessible_labels()
    );
    assert_eq!(
        live_of(&session, "Following Ada"),
        Some(ui::accesskit::Live::Polite),
        "whose view this is is news a screen reader hears: {:?}",
        session.accessible_labels()
    );
}

/// The window the shell draws its screen in, as the screen-reader export names it.
const SHELL_WINDOW: &str = "Desktop shell";

/// Every word the shell publishes to assistive technology in its own window: each node's name, its
/// description and its value, as a screen reader reads a row.
fn said_by_the_shell(session: &Session) -> Vec<String> {
    session
        .a11y_windows()
        .into_iter()
        .filter(|(_, name, _)| name == SHELL_WINDOW)
        .flat_map(|(_, _, tree)| tree.nodes.into_iter())
        .flat_map(|(_, node)| {
            [node.label(), node.description(), node.value()]
                .into_iter()
                .flatten()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// How many things the shell publishes say `word` to assistive technology in its own window: a
/// node counts once however many of its name, description and value say it, since one row may say
/// what it is twice for different readers.
fn nodes_saying(session: &Session, word: &str) -> usize {
    session
        .a11y_windows()
        .into_iter()
        .filter(|(_, name, _)| name == SHELL_WINDOW)
        .flat_map(|(_, _, tree)| tree.nodes.into_iter())
        .filter(|(_, node)| {
            [node.label(), node.description(), node.value()]
                .into_iter()
                .flatten()
                .any(|said| said == word)
        })
        .count()
}

/// How a screen reader is told of the news `words` name when the shell shows it: the live
/// politeness of the first node whose name holds them, in the shell's own window.
fn live_of(session: &Session, words: &str) -> Option<ui::accesskit::Live> {
    session
        .a11y_windows()
        .into_iter()
        .filter(|(_, name, _)| name == SHELL_WINDOW)
        .find_map(|(_, _, tree)| {
            tree.nodes
                .into_iter()
                .find(|(_, node)| node.label().is_some_and(|label| label.contains(words)))
                .and_then(|(_, node)| node.live())
        })
}

/// Follow the person `who` through the session's own action, as the shell's card asks it: no
/// pointer gesture, so nothing takes the view back.
fn follow_through_the_session(session: &mut Session, who: &str) {
    session
        .invoke(
            "session:people",
            "follow",
            &Arguments::from_iter([("person".to_owned(), serde_json::json!(who))]),
        )
        .expect("the session follows them");
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The time the shell's bar shows.
fn clock_of(session: &Session) -> String {
    session
        .read("session:time")
        .and_then(|time| time["clock"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("the session publishes the time"))
}

/// Open the shell's roster and follow `name` from their card: the bar's faces, their row, and the
/// card's own Follow.
#[track_caller]
fn follow_from_who_is_here(session: &mut Session, name: &str) {
    press_faces(session);
    press_named(session, name);
    wait_for(
        session,
        format!("{name}'s card to offer following them"),
        |session| session.shell_control("Follow"),
    );
    press_named(session, "Follow");
    wait_for(session, "the bar to say whose view this is", |session| {
        session
            .shell_says(&format!("Following {name}"))
            .then_some(())
    });
}

/// The chip's Stop ends the following: the workspace hears this device follows nobody once, the
/// bar stops saying whose view this is, and says "Stopped following" for a moment because the
/// person's own input ended it.
#[test]
fn stopping_the_follow_from_the_chip_ends_it_and_the_bar_says_so() {
    let _settled = ui::motion::testing::settled();
    let (_home, _settings) = settings_home();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = session(&sync);
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    follow_through_the_session(&mut session, &person(0xaa));
    wait_for(
        &mut session,
        "the bar to say whose view this is",
        |session| session.shell_says("Following Ada").then_some(()),
    );
    let (stop_x, stop_y, stop_width, stop_height) = wait_for(&mut session, "its Stop", |session| {
        session.shell_drawn_text_bounds("Stop")
    });
    press(&mut session, (stop_x, stop_y, stop_width, stop_height));
    until(
        &mut session,
        "the workspace hears nobody is followed",
        |_| sync.own().following.is_none(),
    );
    // It stays ended: the one press is the one ending.
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(sync.own().following, None, "following stays ended");
    assert!(
        !session.shell_says("Following Ada"),
        "the bar no longer says whose view this was: {:?}",
        session.accessible_labels()
    );
    // Their own input ended it, so the bar says so a moment, as news a screen reader hears
    // without the person moving focus.
    wait_for(&mut session, "the bar says the follow stopped", |session| {
        session.shell_says("Stopped following").then_some(())
    });
    assert_eq!(
        live_of(&session, "Stopped following"),
        Some(ui::accesskit::Live::Polite),
        "the notice is a polite live region: {:?}",
        session.accessible_labels()
    );
    // ...and the moment passes: the bar says nothing of their view again, the clock back.
    wait_for(&mut session, "the notice to go", |session| {
        (!session.shell_says("Stopped following")).then_some(())
    });
    let clock = clock_of(&session);
    until(&mut session, "the clock is back", |session| {
        session.shell_says(&clock)
    });
}

/// A bar too narrow to say whose view this is folds the chip into their face, with Stop beside it:
/// their face and the way to stop stay on a phone, where the bar shows no other face of them.
#[test]
fn the_follow_chip_folds_into_their_face_on_a_narrow_bar() {
    let _settled = ui::motion::testing::settled();
    let (_home, _settings) = settings_home();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = showing_on(400, 800, &sync, None, shell());
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    let clock = clock_of(&session);
    let (clock_x0, clock_x1, clock_y, clock_height) =
        wait_for(&mut session, "the bar shows the time", |session| {
            let (x, y, width, height) = session.shell_text_bounds(&clock)?;
            Some((x, x + width, y, height))
        });
    let mut own = sync.own();
    own.following = Some(person(0xaa));
    sync.set_own(own);
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    wait_for(
        &mut session,
        "the bar to say whose view this is",
        |session| session.shell_says("Following Ada").then_some(()),
    );
    assert!(
        !session.shell_says(&clock),
        "the clock gives way, even on a narrow bar: {:?}",
        session.accessible_labels()
    );
    // Folded, the way to stop is a square mark beside their face, in the bar's middle: the one
    // word the bar cannot hold is the mark that ends it.
    let (face_x, face_y, face_width, face_height) =
        wait_for(&mut session, "their face", |session| {
            session.shell_drawn_text_bounds("Following Ada")
        });
    assert!(
        face_width < 60.0,
        "folded, the chip is their face alone: {face_width} wide"
    );
    assert!(
        face_y + face_height < 80.0,
        "their face stands in the bar: {face_y} + {face_height}"
    );
    let (stop_x, stop_y, stop_width, stop_height) = wait_for(&mut session, "its Stop", |session| {
        session.shell_drawn_text_bounds("Stop")
    });
    assert!(
        (stop_width - stop_height).abs() <= 3.0,
        "Stop is the square mark beside their face: {stop_width} by {stop_height}"
    );
    assert!(
        stop_x + stop_width / 2.0 > face_x + face_width / 2.0 && stop_x + stop_width < 400.0,
        "Stop is the mark after their face, inside the bar: {stop_x}+{stop_width} after \
         {face_x}+{face_width}"
    );
    // The folded chip stands where the clock stood: their face and Stop together cover the clock's
    // place in the bar, so the bar's glance value reads theirs where it read the time.
    assert!(
        face_x <= clock_x1 && stop_x + stop_width >= clock_x0,
        "the folded chip covers the clock's place ({clock_x0}..{clock_x1}): their face {face_x}..{} and Stop {stop_x}..{}",
        face_x + face_width,
        stop_x + stop_width,
    );
    assert!(
        stop_y + stop_height < 80.0 && clock_y + clock_height < 80.0,
        "Stop and the place the clock stood are in the bar: {stop_y} + {stop_height}, {clock_y} + {clock_height}"
    );
    assert!(
        stop_y + stop_height < 80.0,
        "Stop stands in the bar: {stop_y} + {stop_height}"
    );
    assert!(
        face_y + face_height < 80.0,
        "their face stands in the bar: {face_y} + {face_height}"
    );
    // It ends the follow, and their own input says so a moment.
    press(&mut session, (stop_x, stop_y, stop_width, stop_height));
    until(&mut session, "following ends", |_| {
        sync.own().following.is_none()
    });
    wait_for(&mut session, "the bar says the follow stopped", |session| {
        session.shell_says("Stopped following").then_some(())
    });
    let reports = session.take_shell_layout_reports();
    assert!(
        reports.is_empty(),
        "the folded chip lays out inside the bar: {reports:?}"
    );
}

/// Kept on screen from the start, the roster names the workspace it is of and says who is here
/// without a label over them; one person shares their screen and one keeps where they are.
#[test]
fn the_roster_kept_on_screen_from_the_start_names_its_workspace() {
    let _settled = ui::motion::testing::settled();
    let (_home, _settings) = settings_home();
    // Kept on a device whose session is not running yet, as the CLI keeps it there: the session
    // that follows starts with the roster on screen. The overlay is the device's own setting, and
    // a device whose session never ran has not named its key yet, so its section stands where the
    // settings outside the session make it: `devices//settings/widgets`.
    let address = settings_store::placement::address("widgets", None, "");
    settings_store::outside::set_on_this_device(
        address.id.as_str(),
        &settings_store::workspace::verb("people_overlay"),
        &serde_json::json!({"pinned": true, "edge": "right"}),
    )
    .expect("the roster was kept on screen before");

    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    // Ada shares her screen, Chat in front of Notes; Cy keeps where they are to themselves. Drawn
    // at twice the scale, so the panel is captured as a person with a dense display sees it.
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH * 2, HEIGHT * 2).with_scale(2.0)],
        Options {
            sync: Some(Rc::clone(&sync)),
            scale: 2.0,
            ..Options::default()
        },
    );
    session.handle().replace_tree(shell());
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    screen_of(
        &sync,
        0xaa,
        [1600, 1000],
        vec![
            ("so.tilo.notes", [1; 8], [0, 0, 5000, 10_000], false),
            ("so.tilo.chat", [2; 8], [5000, 0, 10_000, 6000], true),
        ],
    );
    sync.hear(hello(0xcc, "Orange Pi"));
    arrived_as(&sync, 0xcc, "Cy");
    for _ in 0..8 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    shot(&mut session, "roster-kept-from-start");
    // The panel's title is the workspace it is of, and its header counts who is here.
    let panel = people(&session);
    let said: Vec<String> = panel
        .nodes
        .iter()
        .filter_map(|(_, node)| node.description().map(str::to_owned))
        .collect();
    assert!(
        said.iter().any(|said| said.starts_with("Session. ")),
        "{said:?}"
    );
    let names = said_by(&panel);
    assert!(names.iter().any(|name| name == "Ada"), "{names:?}");
}

/// What the shell shows now.
fn frame(session: &mut Session) -> tilo_compositor::Screenshot {
    session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame of the session")
}

/// How far around a panel its shadow reaches, in the shell's pixels.
const SHADOW: f64 = 32.0;

/// How many pixels differ between two frames of the shell where a panel at the rect
/// draws: the panel itself and the shadow around it.
fn differing(
    before: &tilo_compositor::Screenshot,
    after: &tilo_compositor::Screenshot,
    (x, y, width, height): (f64, f64, f64, f64),
) -> usize {
    let draws = |at: u32, down: u32| {
        let (at, down) = (f64::from(at), f64::from(down));
        at >= x - SHADOW
            && at < x + width + SHADOW
            && down >= y - SHADOW
            && down < y + height + SHADOW
    };
    (0..before.height)
        .flat_map(|down| (0..before.width).map(move |at| (at, down)))
        .filter(|&(at, down)| draws(at, down) && before.pixel(at, down) != after.pixel(at, down))
        .count()
}

/// Kept on screen with nobody else here, the roster waits off the shell with its pin kept: the shell
/// is the person's work, not a card saying there is no one to show. Someone arriving brings it in.
#[test]
fn a_pinned_roster_waits_off_the_shell_while_the_person_is_alone() {
    let _settled = ui::motion::testing::settled();
    let (_home, _settings) = settings_home();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = session(&sync);

    // Alone, the bar's faces open the roster, and its own button keeps it on screen.
    press_faces(&mut session);
    let keep = holding(&people(&session), "Keep on screen");
    press(&mut session, keep);
    for _ in 0..8 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let named: Vec<String> = session
        .a11y_windows()
        .into_iter()
        .map(|(_, window, _)| window)
        .collect::<Vec<String>>();
    assert!(
        !named.iter().any(|window| window == "People"),
        "nothing of the roster stands over the session: {named:?}"
    );
    let said = session.accessible_labels();
    assert!(
        !said
            .iter()
            .any(|name| name == "Nobody else is here right now"),
        "the session holds no card saying nobody is here: {said:?}"
    );
    shot(&mut session, "roster-waiting-alone");

    // Ada arrives, and the roster kept on screen comes in with what she is doing, its pin kept:
    // nothing is pressed again.
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada");
    for _ in 0..8 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let said = said_by(&people(&session));
    assert!(said.iter().any(|name| name == "Ada"), "{said:?}");
    assert!(
        said.iter().any(|name| name == "Put back in the bar"),
        "the roster is kept on screen without being pinned again: {said:?}"
    );
    shot(&mut session, "roster-back-for-ada");
}

/// The roster's first frame on the shell is its last: the material it is made of is blurred before
/// the frame is shown, so a still shell does not keep a flat card and repaint it a frame later.
#[test]
fn the_rosters_first_frame_on_the_shell_is_final() {
    let _settled = ui::motion::testing::settled();
    let (_home, _settings) = settings_home();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = session(&sync);
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    // The faces open the roster, and the frame that draws it is the one the person sees: the next
    // turn of the session draws nothing new of it.
    session
        .invoke(CHROME_RESOURCE, SHOW_PEOPLE, &Arguments::new())
        .expect("the bar's faces open who is here");
    session.step(Duration::from_millis(1)).expect("step");
    let first = frame(&mut session);
    session.step(Duration::from_millis(1)).expect("step");
    let second = frame(&mut session);
    // What the roster draws is where it stands, its shadow around it. The rest of the shell is the
    // shell's, which redraws on turns of its own: its dock takes a mark away under its launcher
    // a few turns after the roster opens, on whichever turn the shell hears of it, and that turn
    // is not the roster's frame.
    let roster = control(&session, &["people-panel"]).expect("the roster stands on the session");
    assert_eq!(
        differing(&first, &second, roster),
        0,
        "the roster's material is drawn in its first frame"
    );
    shot(&mut session, "roster-first-frame");
}

/// A send-to box finds the people of the workspace by the name everyone here calls them: the one
/// list it searches is what the session publishes, and a member at their own device is in it.
#[test]
fn a_send_to_box_finds_a_person_here_by_name() {
    let _settled = ui::motion::testing::settled();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), own));
    let mut session = session(&sync);
    sync.hear(hello(0xbb, "ThinkPad"));
    arrived_as(&sync, 0xbb, "Ben");
    let recipients = crate::common::wait_for(
        &mut session,
        "Ben among whom a message goes to",
        |session| {
            session.settle_services();
            let all: Vec<ui::floating_surface::Recipient> =
                serde_json::from_value(session.read("session:send-to")?["recipients"].clone())
                    .ok()?;
            let found = ui::floating_surface::recipients_matching(&all, "Ben");
            (!found.is_empty()).then_some(found)
        },
    );
    // Ben the person, by the root Chat knows him by, never the device he is at.
    assert_eq!(
        (&recipients[0].kind, recipients[0].id.as_str()),
        (
            &ui::floating_surface::RecipientKind::Person,
            person(0xbb).as_str()
        ),
        "{recipients:?}"
    );
}
