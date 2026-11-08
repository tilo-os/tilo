//! Who is here: the bar's faces open the roster, which lists everyone in the
//! workspace and where each of them is, and the roster reads its one feed
//! from the collaboration plane. A face opens the person's card, from which
//! Message opens Chat on the conversation they have.

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
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use collab_types::presence::{Payload, Presence};
use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use tilo_actions::DoorError;
use tilo_actions::remote::RemoteActionTarget;
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop::desk_verbs::{CHROME_RESOURCE, DESK_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND};
use tilo_desktop_model::{
    DesktopTree, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceSlot,
};
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_network_actor::{NetEvent, workspace_topic_id};
use tilo_session::sync::{Client, OwnPresence};
use tilo_session::{AppActions, Options, Session};

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

/// A desk showing one workspace it takes part in, with one window called
/// Notes: where the other person is working.
fn desk() -> DesktopTree {
    let workspace = workspace_id();
    let slot = WorkspaceSlot::of_workspace(workspace);
    let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
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
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// One thing the other device said in the workspace.
fn said(from: u8, msg: NeighbourMsg) -> NetEvent {
    NetEvent::Msg {
        ws: workspace_topic_id(workspace_id()),
        peer: device(from),
        membership_epoch: None,
        replayed: false,
        msg,
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
fn hello(from: u8, label: &str) -> NetEvent {
    hello_of(from, from, label)
}

/// The device `device` of the person `person`: their certificate here.
fn hello_of(person: u8, at: u8, label: &str) -> NetEvent {
    let device_key = device(at);
    let root = ed25519_dalek::SigningKey::from_bytes(&[person; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *device_key.as_bytes(),
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
    said(
        at,
        NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        },
    )
}

/// One presence the other device said on the workspace's live plane: they
/// arrived, how available they are, and so on.
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

/// The device `from` arrives, and says whose device it is: the person's own
/// name for themselves, which is what everyone here calls them.
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

/// A session taking part in the workspace through `sync`, showing `desk`.
fn session(sync: &Rc<Client>) -> Session {
    session_over(sync, None)
}

/// [`session`], with `apps` answering what an app asks or is asked to do.
fn session_over(sync: &Rc<Client>, apps: Option<Rc<dyn AppActions>>) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            sync: Some(Rc::clone(sync)),
            apps,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.handle().replace_tree(desk());
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    session
}

/// An action host that keeps what it was asked to run, and starts nothing.
#[derive(Default)]
struct Host {
    invoked: RefCell<Vec<(RemoteActionTarget, Arguments)>>,
}

impl AppActions for Host {
    fn published(&self, _: &str) -> Vec<PublishedActions> {
        Vec::new()
    }

    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    fn invoke(&self, target: RemoteActionTarget, arguments: Arguments) -> Result<(), DoorError> {
        self.invoked.borrow_mut().push((target, arguments));
        Ok(())
    }

    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn open_uri(&self, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn start_host_app(&self, _: &str) -> Result<(), DoorError> {
        Ok(())
    }
}

fn press_faces(session: &mut Session) {
    session
        .invoke(CHROME_RESOURCE, "toggle-collaborators", &Arguments::new())
        .expect("the bar's faces open who is here");
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The person this test's session is a device of, at another of their
/// devices too.
const QUINTEN: u8 = 0xf5;
/// The channel a call in the workspace is in.
const CHANNEL: &str = "c_standup";

/// The channel's call, as a device in it publishes it.
fn channel_call() -> String {
    ui::CallPresence {
        channel: CHANNEL.to_owned(),
        name: "Standup".to_owned(),
    }
    .encode()
}

/// What the palette calls the channel's call row: what choosing it joins.
fn call_row_label() -> String {
    tilo_desktop_search::i18n::t_args("search-launcher-call-in", &[("channel", "Standup".into())])
}

/// The resource `id`, however deep the desk nests it.
fn find(resources: &[tilo_actions::ResourceSpec], id: &str) -> Option<tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// What the empty palette offers now, row by row: the calls in progress are
/// among them, and what choosing one joins.
fn palette_rows(session: &mut Session) -> Vec<String> {
    // The palette lists what this device saw of who is here as of the last
    // pass, so a pass runs before it opens.
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    session
        .invoke(DESK_RESOURCE, "search", &Arguments::new())
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

/// The channel's call is offered to join in the empty palette, and not once
/// this person is in it: another device of theirs says so, as the presence a
/// device publishes when it joins a call does.
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

    // This person's laptop joins the same call: its presence says so, and
    // this device offers no second join.
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

    // Another person, at their own device, in the same workspace: here, heads
    // down, with their pointer over Notes.
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
    assert_eq!(
        sync.placed(
            WORKSPACE,
            &[(NOTES, ui::kurbo::Rect::new(0.0, 0.0, 1000.0, 1000.0))]
        ),
        [(device(0xaa).to_string(), NOTES, 12.0, 34.0)],
        "their pointer is over Notes, where the desk draws it"
    );

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
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Ada"),
        "both are here: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Grace"),
        "both are here: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label == "Framework 16"),
        "a person is named, never their machine: {labels:?}"
    );
    // How available a person is is said of them, and only of them: the one
    // who is at their machine is not told apart from the one who is away by
    // their name alone.
    assert_eq!(
        labels.iter().filter(|label| *label == "Away").count(),
        1,
        "one of the two is away: {labels:?}"
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
    let labels = session.accessible_labels();
    assert!(
        !labels.iter().any(|label| label == "Away"),
        "nobody is away now: {labels:?}"
    );

    // They join a voice channel: the roster says which one, and that is what
    // people read of them rather than how available they are.
    presence(
        &sync,
        0xbb,
        collab_types::PeerPresencePayload::Availability {
            state: "available".to_owned(),
            call: Some(
                ui::CallPresence {
                    channel: "c_standup".to_owned(),
                    name: "Standup".to_owned(),
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
    let labels = session.accessible_labels();
    drop(session);
    assert!(
        labels.iter().any(|label| label.contains("Standup")),
        "the call they are in: {labels:?}"
    );
}

/// The app that holds conversations.
const CHAT: &str = "so.tilo.chat";

/// Install `app` in `store`, as the package landing there leaves it: the
/// manifest an open reads and the registry row the catalogue lists it by.
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

/// One display's refresh: how often the session asks its clients for their
/// next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// `session`, stepped until `ready` says so, and no further.
#[track_caller]
fn until(session: &mut Session, what: &str, mut ready: impl FnMut(&Session) -> bool) {
    for _ in 0..200 {
        if ready(session) {
            return;
        }
        session.step(REFRESH).expect("a turn of the session");
    }
    panic!("{what}");
}

/// The control `names` names, where the session shows it: the bar's face of a
/// person, a button of the roster or of a card.
fn control(session: &Session, names: &[&str]) -> Option<(f64, f64, f64, f64)> {
    session
        .controls()
        .into_iter()
        .find(|(name, _)| names.iter().any(|wanted| name == wanted))
        .map(|(_, rect)| rect)
}

/// A press at the centre of `rect`, through the compositor's own input.
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

/// Write what the session shows where a person can look at it, when the run
/// asked for shots: `TILO_SURFACE_SHOTS` names where they go.
fn capture(session: &mut Session, name: &str) {
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(PathBuf::from) else {
        return;
    };
    let frame = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame of the session");
    let pixels = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
        .expect("the frame's pixels fill it");
    let mut png = Vec::new();
    pixels
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the shot");
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    std::fs::write(directory.join(format!("{name}.png")), png).expect("the shot");
}

/// Ada is here on two devices, working in Notes on the first and focusing on
/// the second, each saying whose device it is.
fn ada_here_on_two_devices(sync: &Client) {
    sync.hear(hello_of(0xaa, 0xaa, "Framework 16"));
    arrived_as(sync, 0xaa, "Ada");
    presence(
        sync,
        0xaa,
        collab_types::PeerPresencePayload::Availability {
            state: "available".to_owned(),
            call: None,
            camera: false,
            screen: false,
        },
    );
    pointer(sync, 0xaa, 12.0, 34.0);
    sync.hear(hello_of(0xaa, 0xac, "ThinkPad"));
    arrived_as(sync, 0xac, "Ada");
    presence(
        sync,
        0xac,
        collab_types::PeerPresencePayload::Availability {
            state: "focusing".to_owned(),
            call: None,
            camera: false,
            screen: false,
        },
    );
}

/// What the card says of Ada: her own name for its title, the person's root
/// key for its ID, and each of her devices with how it said it was, in the
/// roster's place rather than beside it.
fn assert_ada_card(labels: &[String]) {
    assert!(
        labels.iter().any(|label| label == "Ada’s profile"),
        "the card is the person's: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Person ID"),
        "the card's ID row: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|label| label == &tilo_desktop::profile_card::short_key(&person(0xaa))),
        "the ID is the person's root key: {labels:?}"
    );
    for (device, presence) in [("Framework 16", "Available"), ("ThinkPad", "Focusing")] {
        assert!(
            labels.iter().any(|label| label == device),
            "each device as it calls itself: {labels:?}"
        );
        assert!(
            labels.iter().any(|label| label == presence),
            "and how it said it was: {labels:?}"
        );
    }
    assert!(
        !labels.iter().any(|label| label == "People"),
        "the card stands in the roster's place: the roster is gone, {labels:?}"
    );
}

/// A press on a person's face in the bar opens their card, and Message opens
/// Chat on the conversation they have: the same card the web desk shows,
/// handed the app invocation that reveals it.
#[test]
fn a_persons_face_opens_a_card_whose_message_opens_chat_on_their_conversation() {
    let _settled = ui::motion::testing::settled();
    let home = tempfile::tempdir().expect("a home for this test's app store");
    let store = home.path().join("tilo-apps");
    install(&store, CHAT, "Chat");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", &store);

    let host = Rc::new(Host::default());
    let mine = OwnPresence {
        label: "Quinten".to_owned(),
        user: Some(person(QUINTEN)),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), mine));
    let mut session = session_over(&sync, Some(host.clone() as Rc<dyn AppActions>));

    // Another person, at two of their devices, in the same workspace: their
    // face stands in the bar, under their name and never their machine's.
    ada_here_on_two_devices(&sync);
    // The pointer comes up to the bar, as the person's does to press them:
    // the bar draws who is here, and their face stands under it.
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 1100.0, y: 16.0 })
        .expect("the pointer");
    until(&mut session, "their face is in the bar", |session| {
        control(session, &["Ada"]).is_some()
    });
    // Who is here stands open, listing the people here.
    press_faces(&mut session);
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "People"),
        "the roster lists who is here: {labels:?}"
    );
    assert!(
        control(&session, &["Framework 16"]).is_none(),
        "the face names the person, never their machine: {:?}",
        session.controls()
    );

    // A press on their face opens their card.
    let face = control(&session, &["Ada"]).expect("the face the person has here");
    press(&mut session, face);
    until(&mut session, "the card opens", |session| {
        control(session, &["profile-message", "Message"]).is_some()
    });
    let message = control(&session, &["profile-message", "Message"]);
    assert!(
        message.is_some(),
        "the card offers Message: {:?}",
        session.controls()
    );
    assert_ada_card(&session.accessible_labels());
    capture(&mut session, "people-card-message");
    let message = message.expect("the card's Message");

    // Message hands Chat the conversation with them: the app's own
    // `open-direct-conversation` on the row their two root keys name.
    press(&mut session, message);
    until(&mut session, "Chat is handed the conversation", |_| {
        !host.invoked.borrow().is_empty()
    });
    let invoked = host.invoked.borrow();
    let [(target, _)] = invoked.as_slice() else {
        panic!("one invocation, not {invoked:?}");
    };
    assert!(
        target.provider.ends_with(&format!(":{CHAT}")),
        "the host reaches Chat: {target:?}"
    );
    assert_eq!(
        target.address.resource.as_str(),
        format!("chat.conversation:{}", person(0xaa)),
        "the conversation with them"
    );
    assert_eq!(
        target.address.action.as_str(),
        "open-direct-conversation",
        "Chat opens the conversation with them"
    );

    // The card closed behind the choice, and the roster is still here.
    assert!(
        control(&session, &["profile-message", "Message"]).is_none(),
        "the card closed: {:?}",
        session.controls()
    );
}

/// Your own avatar opens your card, from the surface another person's does:
/// your name and picture as everyone here sees them, and your identity, its
/// backup and pairing another device, each in one place.
#[test]
fn your_own_avatar_opens_your_card() {
    let _settled = ui::motion::testing::settled();
    let mine = OwnPresence {
        label: "Quinten".to_owned(),
        user: Some(person(QUINTEN)),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(device(0xf0), mine));
    let mut session = session_over(&sync, None);
    // The pointer comes up to the bar, as the person's does to press it.
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 1100.0, y: 16.0 })
        .expect("the pointer");
    let mut avatar = None;
    let mut named = None;
    until(&mut session, "the bar shows whose it is", |session| {
        let found = session
            .controls()
            .into_iter()
            .find(|(name, _)| name.ends_with("(you): your profile"));
        named = found.as_ref().map(|(name, _)| name.clone());
        avatar = found.map(|(_, rect)| rect);
        avatar.is_some()
    });
    // The bar's face and the card's are one: the name they gave, not the
    // account they happen to log in with, and one initial drawn in one
    // colour.
    assert_eq!(
        named.as_deref(),
        Some("Quinten (you): your profile"),
        "the bar names the person as they gave themselves"
    );
    press(&mut session, avatar.expect("the bar's own avatar"));
    until(&mut session, "your card opens", |session| {
        control(session, &["profile-name", "Your name"]).is_some()
    });
    let labels = session.accessible_labels();
    for said in [
        "How everyone here sees you",
        "Your identities",
        "Back up your identity",
        "Pair another device",
        // The device the card is drawn on is named by which one it is, and
        // its key is kept for the copy affordance beside it.
        "This computer",
        "this device",
        "Copy this device’s key",
    ] {
        assert!(
            labels.iter().any(|label| label == said),
            "your own card says {said:?}: {labels:?}"
        );
    }
    assert!(
        !labels.iter().any(|label| label == "Message"),
        "your own card has nothing to message: {labels:?}"
    );
    // Their id says who they are, so it sits quiet under their identity; the
    // card opens on what they came to change.
    let at = |said: &str| labels.iter().position(|label| label == said);
    let (identity, key, devices) = (
        at("Your identities").expect("the identity actions"),
        at("Person ID").expect("the person's own id"),
        at("Devices").expect("the devices they are here on"),
    );
    assert!(
        identity < key && key < devices,
        "the id sits under your identity, not at the top of the card: {labels:?}"
    );
    capture(&mut session, "own-card");

    assert!(
        control(&session, &["profile-card", "Your profile"]).is_some(),
        "one card, the same surface another person's opens in: {:?}",
        session.controls()
    );
}
