use super::*;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::{RoomId, Workspace};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// The workspace the tests' only workspace is.
const WORKSPACE: &str = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";

/// A workspace no test's desktop shows.
const OTHER_WORKSPACE: &str = "0a2c4e6f-8b1d-4f3a-9c5e-7d9b1f3a5c7e";

/// A device's publisher, from a seed: an endpoint id nobody's identity
/// certifies.
fn device(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}

fn topic(workspace: &str) -> WorkspaceId {
    workspace_topic_id(RoomId::parse(workspace).expect("a workspace id"))
}

/// A desktop showing one workspace, in `workspace`.
fn desk(workspace: &str) -> DesktopTree {
    let workspace = RoomId::parse(workspace).expect("a workspace id");
    let id = tilo_desktop_model::WorkspaceId::of_room(workspace);
    assert_eq!(topic(&workspace.as_uuid().to_string()), id.raw());
    let workspace = Workspace::in_workspace(id, workspace, "Desk");
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.id,
        ..DesktopTree::default()
    }
}

/// A device of the person's, certified for their root key, and the key
/// it signs with.
fn certified(user: u8, device_seed: u8, label: &str) -> (DeviceCert, SigningKey) {
    let person = SigningKey::from_bytes(&[user; 32]);
    let this = SigningKey::from_bytes(&[device_seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: this.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    (cert, this)
}

/// A device's workspace list for `workspace`, signed by that device.
fn list_of(
    device: &DeviceCert,
    signing: &SigningKey,
    workspace: &str,
) -> tilo_user_identity::DirectoryUpdate {
    let mut directory = tilo_user_identity::WorkspaceDirectory::new(
        device.user_public_key,
        device.device_public_key,
    );
    let workspace = RoomId::parse(workspace).expect("a workspace id");
    directory
        .add_workspace_with_topic(
            workspace_topic_id(workspace),
            &workspace.to_string(),
            "Desk",
        )
        .expect("a listed workspace");
    let update = directory
        .signed_snapshot(signing, tilo_clock::unix_now_ns())
        .expect("a signed list");
    assert_eq!(update.author_device_public_key, device.device_public_key);
    update
}

/// A session in `WORKSPACE` with no transport, and what it would have said.
fn session(
    label: &str,
    user: Option<u8>,
) -> (Client, tokio::sync::mpsc::UnboundedReceiver<Outgoing>) {
    let own = OwnPresence {
        label: label.to_owned(),
        user: user.map(person),
        ..OwnPresence::default()
    };
    let (sync, said) = Client::offline(device(0xf0), own);
    sync.follow_desktop(&desk(WORKSPACE));
    (sync, said)
}

fn sent(said: &mut tokio::sync::mpsc::UnboundedReceiver<Outgoing>) -> Vec<Outgoing> {
    let mut commands = Vec::new();
    while let Ok(command) = said.try_recv() {
        commands.push(command);
    }
    commands
}

/// One presence a peer said, on `workspace`'s topic.
fn presence(from: u8, workspace: &str, payload: PeerPresencePayload) -> NetEvent {
    let from = device(from);
    NetEvent::Msg {
        ws: topic(workspace),
        peer: from,
        membership_epoch: None,
        replayed: false,
        msg: NeighbourMsg::AppPresenceV2 {
            peer: neighbour_wire::peer_handle(from.as_bytes()),
            app_id: DESKTOP_PRESENCE_APP.to_owned(),
            cluster_id: workspace.to_owned(),
            presence: PeerPresence {
                key: workspace_key(workspace),
                payload,
            },
            colour: None,
        },
    }
}

fn here(from: u8) -> NetEvent {
    presence(from, WORKSPACE, PeerPresencePayload::Arrived)
}

/// A device's identity announcement: which person it is a device of,
/// and what to call it.
/// The person the device `seed` is a device of, in the hex everyone here
/// knows them by: the root key its certificate is signed with.
fn person(seed: u8) -> String {
    hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

fn hello(from: u8, user: u8, device_label: &str) -> NetEvent {
    let from = device(from);
    let root = ed25519_dalek::SigningKey::from_bytes(&[user; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *from.as_bytes(),
        device_label: device_label.to_owned(),
        capabilities: DeviceCapabilities {
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
    NetEvent::Msg {
        ws: topic(WORKSPACE),
        peer: from,
        membership_epoch: None,
        replayed: false,
        msg: NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        },
    }
}

/// Where a peer's pointer moved, on the tile of `tab` of `workspace`.
fn pointer_moved(from: u8, workspace: &str, tab: u64, point: (f64, f64)) -> NetEvent {
    let from = device(from);
    NetEvent::Msg {
        ws: topic(workspace),
        peer: from,
        membership_epoch: None,
        replayed: false,
        msg: NeighbourMsg::PresenceMoved {
            peer: neighbour_wire::peer_handle(from.as_bytes()),
            user: String::new(),
            key: serde_json::to_string(&PresenceKey::Pointer {
                space: PointerSpace::Tile {
                    room: workspace.to_owned(),
                    tab,
                },
            })
            .expect("a tile pointer key"),
            point,
            ts: 0,
            colour: None,
        },
    }
}

fn pointer_cleared(from: u8, workspace: &str, tab: u64) -> NetEvent {
    let from = device(from);
    NetEvent::Msg {
        ws: topic(workspace),
        peer: from,
        membership_epoch: None,
        replayed: false,
        msg: NeighbourMsg::PresenceCleared {
            peer: neighbour_wire::peer_handle(from.as_bytes()),
            key: serde_json::to_string(&PresenceKey::Pointer {
                space: PointerSpace::Tile {
                    room: workspace.to_owned(),
                    tab,
                },
            })
            .expect("a tile pointer key"),
        },
    }
}

#[test]
fn a_list_is_kept_only_from_the_device_that_said_it() {
    let (laptop, laptop_key) = certified(0x04, 0x11, "Laptop");
    let (phone, _) = certified(0x04, 0x12, "Phone");
    let by_laptop = list_of(&laptop, &laptop_key, WORKSPACE);

    let kept = author_of(&[phone.clone(), laptop.clone()], &by_laptop)
        .expect("the laptop's own list is the laptop's");
    assert_eq!(kept.device_public_key, laptop.device_public_key);

    // Someone else's device, and a list the laptop's session carried but
    // another device signed: neither says the laptop said it.
    let (stranger, _) = certified(0x55, 0x13, "Stranger");
    assert!(author_of(&[stranger], &by_laptop).is_none());
    assert!(author_of(&[phone], &by_laptop).is_none());
    assert!(author_of(&[], &by_laptop).is_none());
}

#[test]
fn a_peer_here_is_a_collaborator_of_the_active_workspace() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));

    let mut tree = desk(WORKSPACE);
    let people = sync.show(&mut tree, &|_| None);
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].label, "Framework 16");
    assert_eq!(people[0].user, Some(person(0xaa)));

    let collaborators = &tree.workspaces[0].collaborators;
    assert_eq!(collaborators.len(), 1);
    assert_eq!(collaborators[0].id, device(1).to_string());
    assert_eq!(collaborators[0].name, "Framework 16");
    assert!(collaborators[0].cursor_color.is_some());
    assert_eq!(collaborators[0].role_id, None);
}

#[test]
fn what_a_peer_says_they_are_doing_is_the_roster_line() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));
    sync.hear(presence(
        1,
        WORKSPACE,
        PeerPresencePayload::Status {
            text: "  Shipping the roster  ".to_owned(),
            automatic: false,
        },
    ));

    let mut tree = desk(WORKSPACE);
    let people = sync.show(&mut tree, &|_| None);
    assert_eq!(
        people[0].status.as_ref().map(|line| line.text.as_str()),
        Some("Shipping the roster")
    );
}

#[test]
fn a_peer_in_a_call_says_which_channel_to_join() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));
    assert!(sync.calls().is_empty());

    let voice = ui::CallPresence {
        channel: "c_voice".to_owned(),
        name: "Voice".to_owned(),
    };
    let in_voice = |call: Option<String>| PeerPresencePayload::Availability {
        state: "available".to_owned(),
        call,
    };
    sync.hear(presence(1, WORKSPACE, in_voice(Some(voice.encode()))));

    let calls = sync.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].workspace, WORKSPACE);
    assert_eq!(calls[0].channel, "c_voice");
    assert_eq!(calls[0].name, "Voice");
    let who: Vec<&str> = calls[0]
        .people
        .iter()
        .map(|person| person.label.as_str())
        .collect();
    assert_eq!(who, ["Framework 16"]);

    // Someone else in the same channel is in the same call, not another.
    sync.hear(hello(2, 0xbb, "Phone"));
    sync.hear(here(2));
    sync.hear(presence(2, WORKSPACE, in_voice(Some(voice.encode()))));
    let calls = sync.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].people.len(), 2);

    // Out of the call, the channel is no one's.
    sync.hear(presence(1, WORKSPACE, in_voice(None)));
    sync.hear(presence(2, WORKSPACE, in_voice(None)));
    assert!(sync.calls().is_empty());
}

/// How available a peer says they are is the roster's: their face dims
/// while they are away or idle, and says so in words.
#[test]
fn a_peer_away_is_away_on_the_roster() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));

    let away = |state: &str| PeerPresencePayload::Availability {
        state: state.to_owned(),
        call: None,
    };
    sync.hear(presence(1, WORKSPACE, away("away")));

    let mut tree = desk(WORKSPACE);
    let people = sync.show(&mut tree, &|_| None);
    assert_eq!(people[0].availability, ui::Availability::Away);
    assert!(people[0].availability.dims());
    let rows = tilo_desktop::collaborators::collaborator_roster_rows(&tree, "me");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].availability, ui::Availability::Away);

    // Back at their machine, and heads down: the face lightens, and
    // says what they chose.
    sync.hear(presence(1, WORKSPACE, away("focusing")));
    let mut tree = desk(WORKSPACE);
    sync.show(&mut tree, &|_| None);
    let rows = tilo_desktop::collaborators::collaborator_roster_rows(&tree, "me");
    assert_eq!(rows[0].availability, ui::Availability::Focusing);
    assert!(!rows[0].availability.dims());
}

/// A call this device joins is said in the workspace, so everyone else's
/// roster shows this person in that channel.
#[test]
fn a_call_this_device_joins_reaches_a_peer_s_roster() {
    let (mine, mut said) = session("Quinten", Some(0x04));
    while said.try_recv().is_ok() {}
    let voice = ui::CallPresence {
        channel: "c_voice".to_owned(),
        name: "Voice".to_owned(),
    };
    assert!(mine.in_call(Some(voice.clone())));

    // A peer's session in the same workspace, handed everything this device
    // said there.
    let own = OwnPresence {
        label: "Ada".into(),
        ..OwnPresence::default()
    };
    let (theirs, _theirs_said) = Client::offline(device(0xe0), own);
    theirs.follow_desktop(&desk(WORKSPACE));
    let mut said_the_call = false;
    while let Ok(command) = said.try_recv() {
        let Outgoing::Publish { ws, msg } = command else {
            continue;
        };
        if let NeighbourMsg::AppPresenceV2 { presence, .. } = &*msg
            && matches!(
                &presence.payload,
                PeerPresencePayload::Availability { call: Some(call), .. }
                    if ui::CallPresence::decode(call).as_ref() == Some(&voice)
            )
        {
            said_the_call = true;
        }
        theirs.hear(NetEvent::Msg {
            ws,
            peer: mine.publisher(),
            membership_epoch: None,
            replayed: false,
            msg: *msg,
        });
    }
    assert!(said_the_call, "this device said which channel it is in");

    let mut tree = desk(WORKSPACE);
    let people = theirs.show(&mut tree, &|_| None);
    assert_eq!(people.len(), 1);
    assert_eq!(
        people[0].call.as_ref().map(|call| call.name.as_str()),
        Some("Voice")
    );
    let rows = tilo_desktop::collaborators::collaborator_roster_rows(&tree, "me");
    assert_eq!(rows[0].call.as_deref(), Some("Voice"));

    // Out of the call, the peer's roster says nothing of a channel.
    while said.try_recv().is_ok() {}
    assert!(mine.in_call(None));
    while let Ok(command) = said.try_recv() {
        let Outgoing::Publish { ws, msg } = command else {
            continue;
        };
        theirs.hear(NetEvent::Msg {
            ws,
            peer: mine.publisher(),
            membership_epoch: None,
            replayed: false,
            msg: *msg,
        });
    }
    let mut tree = desk(WORKSPACE);
    let people = theirs.show(&mut tree, &|_| None);
    assert!(people[0].call.is_none());
}

#[test]
fn a_peer_who_leaves_leaves_the_roster() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);

    sync.hear(presence(1, WORKSPACE, PeerPresencePayload::Left));
    assert!(sync.show(&mut desk(WORKSPACE), &|_| None).is_empty());

    sync.hear(here(1));
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);
    sync.hear(NetEvent::PeerLeft {
        ws: topic(WORKSPACE),
        peer: device(1),
        cause: tilo_network_actor::PeerDepartureCause::HostSessionClosed,
        membership_epoch: None,
    });
    assert!(sync.show(&mut desk(WORKSPACE), &|_| None).is_empty());

    let mut tree = desk(WORKSPACE);
    sync.show(&mut tree, &|_| None);
    assert!(tree.workspaces[0].collaborators.is_empty());
}

/// Two relays nobody listens on: this endpoint never has a home relay
/// to be published with, so the directory gate cannot open, and nothing
/// here waits on a network.
fn unused_relays() -> iroh::RelayMap {
    ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
        .into_iter()
        .map(|url| url.parse::<iroh::RelayUrl>().expect("a relay url"))
        .collect()
}

#[test]
fn the_relays_wait_until_a_workspace_with_someone_else_is_shown() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the collaboration endpoint");
    // The endpoint the real client binds, its directory gated and its
    // relays held back.
    let gate = tilo_endpoint_reachability::collaborating();
    let (endpoint, reachability) = runtime.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .address_lookup(gate.clone())
            .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .secret_key(iroh::SecretKey::generate())
            .bind()
            .await
            .expect("a collaboration endpoint binds");
        let reachability = Reachability::defer(
            &endpoint,
            &unused_relays(),
            tokio::runtime::Handle::current(),
            Some(tilo_endpoint_reachability::REST_AFTER),
        );
        (endpoint, reachability)
    });
    let own = OwnPresence {
        label: "Quinten".into(),
        ..OwnPresence::default()
    };
    let (sync, _said) =
        Client::offline_reaching(device(0xf0), own, Vec::new(), reachability.clone());
    sync.follow_desktop(&desk(WORKSPACE));

    // Nobody else in the workspace this desktop shows: it names no relay,
    // and number 0's directory is told nothing of this device.
    assert!(reachability.is_dormant());
    assert_eq!(reachability.holders(), (None, None));
    assert!(!gate.is_open());
    assert_eq!(endpoint.addr().relay_urls().count(), 0);

    // Someone else is here: what could dial this device starts.
    sync.hear(here(1));
    assert!(!reachability.is_dormant());
    assert_eq!(
        reachability.holders(),
        (None, Some("a workspace is shown with someone else"))
    );

    // They leave: nothing here needs this device dialable again.
    sync.hear(presence(1, WORKSPACE, PeerPresencePayload::Left));
    assert_eq!(reachability.holders(), (None, None));

    // Someone in a workspace this desktop does not show needs nothing
    // of this device.
    sync.hear(presence(2, OTHER_WORKSPACE, PeerPresencePayload::Arrived));
    assert_eq!(reachability.holders().1, None);
}

#[test]
fn only_the_workspace_in_front_lists_people() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));

    let elsewhere = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    sync.hear(presence(2, elsewhere, PeerPresencePayload::Arrived));
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);

    // Presence said on this workspace's topic under another workspace's name is
    // that other workspace's.
    let mut foreign = here(3);
    if let NetEvent::Msg { msg, .. } = &mut foreign
        && let NeighbourMsg::AppPresenceV2 { cluster_id, .. } = msg
    {
        *cluster_id = elsewhere.to_owned();
    }
    sync.hear(foreign);
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);
}

#[test]
fn another_apps_presence_is_not_the_desktops() {
    let (sync, _said) = session("Quinten", Some(0x04));
    let mut event = here(1);
    if let NetEvent::Msg { msg, .. } = &mut event
        && let NeighbourMsg::AppPresenceV2 { app_id, .. } = msg
    {
        *app_id = "tilo-notes".to_owned();
    }
    sync.hear(event);
    assert!(sync.show(&mut desk(WORKSPACE), &|_| None).is_empty());
}

#[test]
fn this_device_is_not_someone_here() {
    let (sync, _said) = session("Quinten", Some(0x04));
    let mut own = here(0xf0);
    if let NetEvent::Msg { peer, .. } = &mut own {
        *peer = device(0xf0);
    }
    sync.hear(own);
    assert!(sync.show(&mut desk(WORKSPACE), &|_| None).is_empty());
}

#[test]
fn two_devices_of_one_peer_are_one_face() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));
    sync.hear(hello(2, 0xaa, "Phone"));
    sync.hear(here(2));

    let people = sync.show(&mut desk(WORKSPACE), &|_| None);
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].label, "Framework 16");
}

#[test]
fn another_device_of_this_person_is_this_person() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0x04, "Studio"));
    sync.hear(here(1));
    assert!(sync.show(&mut desk(WORKSPACE), &|_| None).is_empty());

    sync.hear(hello(2, 0xbb, "Framework 16"));
    sync.hear(here(2));
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);
}

#[test]
fn pointers_are_followed_on_the_room_tile() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));
    assert!(sync.pointers(WORKSPACE, 3).is_empty());

    sync.hear(pointer_moved(1, WORKSPACE, 3, (12.5, 34.5)));
    assert_eq!(
        sync.pointers(WORKSPACE, 3),
        vec![(device(1).to_string(), 12.5, 34.5)]
    );
    assert!(sync.pointers(WORKSPACE, 4).is_empty());
    assert!(
        sync.pointers("0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c", 3)
            .is_empty()
    );

    sync.hear(pointer_cleared(1, WORKSPACE, 3));
    assert!(sync.pointers(WORKSPACE, 3).is_empty());
}

#[test]
fn a_wave_reaches_the_person_it_is_for() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    sync.hear(here(1));
    let me = device(0xf0).to_string();
    sync.hear(presence(
        1,
        WORKSPACE,
        PeerPresencePayload::Wave { seq: 1, target: me },
    ));
    assert!(
        sync.here().borrow_mut().take_waves().is_empty(),
        "the client takes the wave and hands it to the bar"
    );
    let notice = sync.wave_notice().expect("the bar has a wave to say");
    assert_eq!(notice.from, "Framework 16");

    // A wave at someone else here is not this person's news: the bar
    // keeps saying the wave at this person.
    sync.hear(hello(2, 0xbb, "Desktop"));
    sync.hear(here(2));
    sync.hear(presence(
        2,
        WORKSPACE,
        PeerPresencePayload::Wave {
            seq: 1,
            target: device(9).to_string(),
        },
    ));
    assert_eq!(
        sync.wave_notice().map(|notice| notice.from),
        Some("Framework 16".to_owned()),
        "a wave at someone else here is not this person's news"
    );
}

#[test]
fn waving_says_a_new_wave_each_time() {
    let (sync, mut said) = session("Quinten", None);
    sent(&mut said);
    let target = device(1).to_string();
    sync.wave(&target);
    sync.wave(&target);
    let waves: Vec<(u32, String)> = payloads(&mut said)
        .into_iter()
        .filter_map(|payload| match payload {
            PeerPresencePayload::Wave { seq, target } => Some((seq, target)),
            _ => None,
        })
        .collect();
    assert_eq!(waves, vec![(1, target.clone()), (2, target)]);
    assert_eq!(
        sync.own_member().wave.map(|wave| wave.seq),
        Some(2),
        "this person's row reads their last wave as sent"
    );

    // Waving at this person says nothing: there is no one to greet.
    sync.wave(&device(0xf0).to_string());
    assert!(
        payloads(&mut said).is_empty(),
        "a wave at this person goes nowhere"
    );
}

#[test]
fn a_configured_host_is_read_as_an_id_or_an_address() {
    let host = device(7);
    assert_eq!(host_addr(&host.to_string()).map(|addr| addr.id), Some(host));
    let json = serde_json::to_string(&EndpointAddr::new(host)).unwrap();
    assert_eq!(host_addr(&json).map(|addr| addr.id), Some(host));
    assert!(host_addr("").is_none());
    assert!(host_addr("not an address").is_none());
}

#[test]
fn a_host_is_dialled_before_the_workspaces_it_carries() {
    let host = device(7);
    let (sync, mut said) = Client::offline_at(
        device(0xf0),
        OwnPresence::default(),
        vec![EndpointAddr::new(host)],
    );
    sync.follow_desktop(&desk(WORKSPACE));
    let commands = sent(&mut said);
    let connect = commands
        .iter()
        .position(|command| matches!(command, Outgoing::Connect(addr) if addr.id == host));
    let join = commands
        .iter()
        .position(|command| matches!(command, Outgoing::Join(_)));
    assert_eq!(connect, Some(0), "the host is dialled first");
    assert_eq!(join, Some(1), "the workspace is joined after its host");
}

#[test]
fn taking_part_says_where_this_person_is() {
    let (sync, mut said) = session("Quinten", None);
    let commands = sent(&mut said);
    assert!(matches!(commands[0], Outgoing::Join(ws) if ws == topic(WORKSPACE)));

    let published: Vec<(WorkspaceId, Box<NeighbourMsg>)> = commands
        .into_iter()
        .filter_map(|command| match command {
            Outgoing::Publish { ws, msg } => Some((ws, msg)),
            _ => None,
        })
        .collect();
    assert!(!published.is_empty());
    for (ws, msg) in published {
        assert_eq!(ws, topic(WORKSPACE));
        let NeighbourMsg::AppPresenceV2 {
            peer,
            app_id,
            cluster_id,
            presence,
            ..
        } = *msg
        else {
            panic!("not a presence");
        };
        assert_eq!(
            peer,
            neighbour_wire::peer_handle(sync.publisher().as_bytes())
        );
        assert_eq!(app_id, DESKTOP_PRESENCE_APP);
        assert_eq!(cluster_id, WORKSPACE);
        assert_eq!(
            postcard::from_bytes::<PresenceKey>(&presence.key).expect("a key"),
            PresenceKey::Workspace {
                id: WORKSPACE.to_owned()
            }
        );
    }
}

#[test]
fn changing_what_this_person_says_is_said_again() {
    let (sync, mut said) = session("Quinten", None);
    sent(&mut said);

    let stated = OwnPresence {
        label: "Quinten".to_owned(),
        status: Some("Wiring the plane".to_owned()),
        ..OwnPresence::default()
    };
    assert!(sync.set_own(stated.clone()));
    let said_payloads = payloads(&mut said);
    assert!(said_payloads.contains(&PeerPresencePayload::Status {
        text: "Wiring the plane".to_owned(),
        automatic: false,
    }));

    assert!(!sync.set_own(stated));
    assert!(payloads(&mut said).is_empty());
}

#[test]
fn leaving_a_workspace_leaves_its_topic() {
    let (sync, mut said) = session("Quinten", None);
    sent(&mut said);
    sync.follow_desktop(&DesktopTree::default());
    assert!(matches!(sent(&mut said)[0], Outgoing::Leave(ws) if ws == topic(WORKSPACE)));
}

fn payloads(
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<Outgoing>,
) -> Vec<PeerPresencePayload> {
    sent(commands)
        .into_iter()
        .filter_map(|command| match command {
            Outgoing::Publish { msg, .. } => match *msg {
                NeighbourMsg::AppPresenceV2 { presence, .. } => Some(presence.payload),
                _ => None,
            },
            _ => None,
        })
        .collect()
}
