//! Who is here: the bar's faces open the roster, which lists everyone in the
//! workspace and where each of them is, and the roster reads its one feed
//! from the collaboration plane.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::rc::Rc;
use std::time::Duration;

use collab_types::PeerPresence;
use collab_types::presence::{PointerSpace, PresenceKey};
use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use tilo_compositor::OutputConfig;
use tilo_desktop::desk_verbs::{CHROME_RESOURCE, DESK_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND};
use tilo_desktop_model::{
    DesktopTree, GroupId, RoomId, Strip, StripId, Tab, TabId, TileContent, WindowGroup,
    WindowWidth, Workspace, WorkspaceId,
};
use tilo_network_actor::{NetEvent, workspace_topic_id};
use tilo_session::sync::{Client, OwnPresence};
use tilo_session::{Options, Session};

type Arguments = serde_json::Map<String, serde_json::Value>;

const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
/// The tab the person at the other device has their pointer over.
const NOTES: u64 = 7;
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

fn device(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}

fn workspace_id() -> RoomId {
    RoomId::parse(WORKSPACE).expect("a workspace id")
}

/// A desk showing one workspace it takes part in, with one window called
/// Notes: where the other person is working.
fn desk() -> DesktopTree {
    let workspace = workspace_id();
    let id = WorkspaceId::of_room(workspace);
    let mut workspace = Workspace::in_workspace(id, workspace, "Desk");
    let tab = Tab {
        id: TabId::from(NOTES),
        title: "Notes".to_owned(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".to_owned(),
            label: "Notes".to_owned(),
            note: String::new(),
        },
        app_title: None,
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
        active: workspace.id,
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

/// One presence the other device said: they arrived, how available they are,
/// and so on.
fn presence(from: u8, payload: collab_types::PeerPresencePayload) -> NetEvent {
    let device_key = device(from);
    said(
        from,
        NeighbourMsg::AppPresenceV2 {
            peer: neighbour_wire::peer_handle(device_key.as_bytes()),
            app_id: DESKTOP_PRESENCE_APP.to_owned(),
            cluster_id: WORKSPACE.to_owned(),
            presence: PeerPresence {
                key: postcard::to_stdvec(&PresenceKey::Workspace {
                    id: WORKSPACE.to_owned(),
                })
                .expect("a key"),
                payload,
            },
            colour: None,
        },
    )
}

/// Their pointer, over the Notes window.
fn pointer(from: u8, x: f64, y: f64) -> NetEvent {
    said(
        from,
        NeighbourMsg::PresenceMoved {
            peer: neighbour_wire::peer_handle(device(from).as_bytes()),
            user: String::new(),
            key: serde_json::to_string(&PresenceKey::Pointer {
                space: PointerSpace::Tile {
                    room: WORKSPACE.to_owned(),
                    tab: NOTES,
                },
            })
            .expect("a key"),
            point: (x, y),
            ts: 0,
            colour: neighbour_wire::PresenceColour::default(),
        },
    )
}

/// A session taking part in the workspace through `sync`, showing `desk`.
fn session(sync: &Rc<Client>) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            sync: Some(Rc::clone(sync)),
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

fn press_faces(session: &mut Session) {
    session
        .invoke(CHROME_RESOURCE, "toggle-collaborators", &Arguments::new())
        .expect("the bar's faces open who is here");
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The app id the desk says a bare window carries.
const DESKTOP_PRESENCE_APP: &str = "tilo-shell";

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
    sync.hear(presence(0xaa, collab_types::PeerPresencePayload::Arrived));
    sync.hear(presence(
        0xaa,
        collab_types::PeerPresencePayload::Availability {
            state: "available".to_owned(),
            call: Some(channel_call()),
            camera: true,
            screen: false,
        },
    ));
    let rows = palette_rows(&mut session);
    assert!(
        rows.contains(&call_row_label()),
        "the call in progress is offered to join: {rows:?}"
    );

    // This person's laptop joins the same call: its presence says so, and
    // this device offers no second join.
    sync.hear(hello_of(QUINTEN, 0xf0, "ThinkPad"));
    sync.hear(presence(0xf0, collab_types::PeerPresencePayload::Arrived));
    sync.hear(presence(
        0xf0,
        collab_types::PeerPresencePayload::Availability {
            state: "available".to_owned(),
            call: Some(channel_call()),
            camera: false,
            screen: false,
        },
    ));
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
    sync.hear(presence(0xaa, collab_types::PeerPresencePayload::Arrived));
    sync.hear(presence(
        0xaa,
        collab_types::PeerPresencePayload::Availability {
            state: "away".to_owned(),
            call: None,
            camera: false,
            screen: false,
        },
    ));
    sync.hear(pointer(0xaa, 12.0, 34.0));
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    press_faces(&mut session);
    let labels = session.accessible_labels();
    let listed = |what: &str| labels.iter().any(|label| label.contains(what));
    assert!(listed("Framework 16"), "who is here: {labels:?}");
    assert!(
        labels
            .iter()
            .any(|label| label == "Framework 16 is in Notes"),
        "where their pointer is: {labels:?}"
    );
    assert!(listed("Away"), "how they said they are: {labels:?}");
    assert_eq!(
        sync.pointers(WORKSPACE, NOTES),
        [(device(0xaa).to_string(), 12.0, 34.0)],
        "their pointer is over Notes, where the desk draws it"
    );

    // The faces again: the roster goes back to them.
    press_faces(&mut session);
    let labels = session.accessible_labels();
    drop(session);
    assert!(
        !labels.iter().any(|label| label.contains("Framework 16")),
        "the roster closed: {labels:?}"
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
    for (seed, label, state) in [
        (0xaa, "Framework 16", "available"),
        (0xbb, "ThinkPad", "away"),
    ] {
        sync.hear(hello(seed, label));
        sync.hear(presence(seed, collab_types::PeerPresencePayload::Arrived));
        sync.hear(presence(
            seed,
            collab_types::PeerPresencePayload::Availability {
                state: state.to_owned(),
                call: None,
                camera: false,
                screen: false,
            },
        ));
    }
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    press_faces(&mut session);
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Framework 16"),
        "both are here: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "ThinkPad"),
        "both are here: {labels:?}"
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
    sync.hear(presence(
        0xbb,
        collab_types::PeerPresencePayload::Availability {
            state: "focusing".to_owned(),
            call: None,
            camera: false,
            screen: false,
        },
    ));
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
    sync.hear(presence(
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
    ));
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
