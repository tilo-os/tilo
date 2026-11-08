use super::*;
use collab_types::PeerPresencePayload;
use collab_types::presence::{DESKTOP_APP, Payload, PresenceKey};
use ed25519_dalek::SigningKey;
use tilo_desktop_model::{Workspace, WorkspaceId as ModelWorkspaceId};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// The workspace the tests' only workspace is.
const WORKSPACE: &str = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";

/// A workspace no test's desktop shows.
const OTHER_WORKSPACE: &str = "0a2c4e6f-8b1d-4f3a-9c5e-7d9b1f3a5c7e";

/// A device's publisher, from a seed: an endpoint id nobody's identity certifies.
fn device(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}

fn topic(workspace: &str) -> WorkspaceId {
    workspace_topic_id(ModelWorkspaceId::parse(workspace).expect("a workspace id"))
}

/// A desktop showing one workspace, in `workspace` .
fn desk(workspace: &str) -> DesktopTree {
    let workspace = ModelWorkspaceId::parse(workspace).expect("a workspace id");
    let slot = tilo_desktop_model::WorkspaceSlot::of_workspace(workspace);
    assert_eq!(topic(&workspace.as_uuid().to_string()), slot.raw());
    let workspace = Workspace::in_workspace(slot, workspace, "Desk");
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// A device of the person's, certified for their root key, and the key it signs with.
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

/// A device's workspace list for `workspace` , signed by that device.
fn list_of(
    device: &DeviceCert,
    signing: &SigningKey,
    workspace: &str,
) -> tilo_user_identity::DirectoryUpdate {
    let mut directory = tilo_user_identity::WorkspaceDirectory::new(
        device.user_public_key,
        device.device_public_key,
    );
    let workspace = ModelWorkspaceId::parse(workspace).expect("a workspace id");
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

/// One presence a peer said in `workspace` , as their frame on its presence track says it: this
/// session takes frames from its plane, and one handed here is the same thing.
fn presence(workspace: &str, payload: PeerPresencePayload) -> Option<Presence> {
    let payload = word(payload)?;
    Some(Presence::desktop(workspace, payload))
}

/// The presence-track payload `said` names, for the frames these tests hand a session.
fn word(said: PeerPresencePayload) -> Option<Payload> {
    Some(match said {
        PeerPresencePayload::Arrived => Payload::Arrived,
        PeerPresencePayload::Left => Payload::Left,
        PeerPresencePayload::Status { text, automatic } => Payload::Status { text, automatic },
        PeerPresencePayload::Availability {
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
        PeerPresencePayload::Following { target } => Payload::Following { target },
        PeerPresencePayload::Wave { seq, target } => Payload::Wave { seq, target },
        PeerPresencePayload::Composing { composing } => Payload::Composing { composing },
        PeerPresencePayload::Person { name } => Payload::Person { name },
        _ => return None,
    })
}

/// A peer said `said` of themselves in `workspace` , as their frame on its presence track says it.
fn heard(sync: &Client, from: u8, workspace: &str, said: PeerPresencePayload) {
    let Some(frame) = presence(workspace, said) else {
        return;
    };
    sync.heard_presence(workspace, device(from), &frame);
}

fn here(sync: &Client, from: u8) {
    heard(sync, from, WORKSPACE, PeerPresencePayload::Arrived);
}

/// `from` arrives saying what everyone here calls them: the person's name,
/// which is what their device's certificate cannot say.
fn named(sync: &Client, from: u8, name: &str) {
    heard(
        sync,
        from,
        WORKSPACE,
        PeerPresencePayload::Person {
            name: name.to_owned(),
        },
    );
    here(sync, from);
}

/// A device's identity announcement: which person it is a device of,
/// and what to call it.
/// The person the device `seed` is a device of, in the hex everyone here
/// knows them by: the root key its certificate is signed with.
/// The person `seed`'s root key, in the bytes their devices' certificates
/// carry.
fn root(seed: u8) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .to_bytes()
}

/// Where a device of the person `user` is reached for a workspace, as it signed it: `opaque` says
/// which address.
fn host_hint(
    user: u8,
    device_seed: u8,
    opaque: &str,
) -> (DeviceCert, tilo_peer_state::join::TransportHint) {
    let (cert, key) = certified(user, device_seed, "The host");
    let hint = tilo_peer_state::join::sign_transport_hint(
        tilo_peer_state::join::TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1,
        opaque,
        cert.clone(),
        key.verifying_key().to_bytes(),
        &key,
    )
    .expect("a host signs its own address");
    (cert, hint)
}

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

/// Where a peer's pointer moved, on the tile of `tab` of `workspace` .
fn pointer_moved(sync: &Client, from: u8, workspace: &str, tab: u64, point: (f32, f32)) {
    let frame = Presence::pointing(workspace, tab, point.0, point.1);
    sync.heard_presence(workspace, device(from), &frame);
}

/// Their pointer left the tile of `tab` of `workspace` .
fn pointer_cleared(sync: &Client, from: u8, workspace: &str, tab: u64) {
    let frame = Presence::pointer_left(workspace, tab);
    sync.heard_presence(workspace, device(from), &frame);
}

#[test]
fn a_device_of_the_person_coming_up_owes_it_the_list() {
    let (sync, _said) = session("Desk", Some(0x04));
    assert!(
        !sync.sibling_arrived(),
        "no device of the person said anything yet"
    );
    sync.keep_personal(&NetEvent::Sibling {
        peer: device(0x11),
        membership_epoch: tilo_network_actor::MembershipEpoch::for_test(1),
        certs: Vec::new(),
        revocations: Vec::new(),
    });
    // The pass that hears it tells the list; the next one has nothing new to say, so a device that
    // stayed up is not told it again on every pass.
    assert!(
        sync.sibling_arrived(),
        "a device that just came up has not heard it"
    );
    assert!(!sync.sibling_arrived());
}

#[test]
fn a_list_is_kept_only_from_the_device_that_said_it() {
    let (laptop, laptop_key) = certified(0x04, 0x11, "Laptop");
    let (phone, _) = certified(0x04, 0x12, "Phone");
    let by_laptop = list_of(&laptop, &laptop_key, WORKSPACE);

    let kept = author_of(&[phone.clone(), laptop.clone()], &by_laptop)
        .expect("the laptop's own list is the laptop's");
    assert_eq!(kept.device_public_key, laptop.device_public_key);

    // Someone else's device, and a list the laptop's session carried but another device signed:
    // neither says the laptop said it.
    let (stranger, _) = certified(0x55, 0x13, "Stranger");
    assert!(author_of(&[stranger], &by_laptop).is_none());
    assert!(author_of(&[phone], &by_laptop).is_none());
    assert!(author_of(&[], &by_laptop).is_none());
}

#[test]
fn a_peer_here_is_a_collaborator_of_the_active_workspace() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);

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

/// The person came to a workspace where people already are: the bar says who is here, once, not on
/// every roster change while they stay.
#[test]
fn who_is_here_is_said_when_the_person_arrives() {
    let (sync, _said) = session("Quinten", Some(0x04));
    assert!(sync.arrival_notice().is_none(), "nobody here yet");
    sync.hear(hello(1, 0xaa, "Framework 16"));
    named(&sync, 1, "Ada");
    let arrival = sync.arrival_notice().expect("the bar says who is here");
    assert_eq!(arrival.names, ["Ada"]);

    // What Ada says of herself after is not the person arriving again.
    heard(
        &sync,
        1,
        WORKSPACE,
        PeerPresencePayload::Status {
            text: "Shipping the roster".to_owned(),
            automatic: false,
        },
    );
    assert_eq!(
        sync.arrival_notice().map(|again| again.at),
        Some(arrival.at),
        "the arrival is said once"
    );
}

#[test]
fn what_a_peer_says_they_are_doing_is_the_roster_line() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);
    heard(
        &sync,
        1,
        WORKSPACE,
        PeerPresencePayload::Status {
            text: "  Shipping the roster  ".to_owned(),
            automatic: false,
        },
    );

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
    named(&sync, 1, "Ada");
    assert!(sync.calls().is_empty());

    let voice = ui::CallPresence {
        channel: "c_voice".to_owned(),
        name: "Voice".to_owned(),
    };
    let in_voice = |call: Option<String>| PeerPresencePayload::Availability {
        state: "available".to_owned(),
        call,
        camera: false,
        screen: false,
    };
    heard(&sync, 1, WORKSPACE, in_voice(Some(voice.encode())));

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
    assert_eq!(who, ["Ada"], "the call names the person, not their machine");

    // Someone else in the same channel is in the same call, not another.
    sync.hear(hello(2, 0xbb, "Phone"));
    named(&sync, 2, "Ben");
    heard(&sync, 2, WORKSPACE, in_voice(Some(voice.encode())));
    let calls = sync.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].people.len(), 2);

    // Out of the call, the channel is no one's.
    heard(&sync, 1, WORKSPACE, in_voice(None));
    heard(&sync, 2, WORKSPACE, in_voice(None));
    assert!(sync.calls().is_empty());
}

/// How available a peer says they are is the roster's: their face dims while they are away or idle,
/// and says so in words.
#[test]
fn a_peer_away_is_away_on_the_roster() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);

    let away = |state: &str| PeerPresencePayload::Availability {
        state: state.to_owned(),
        call: None,
        camera: false,
        screen: false,
    };
    heard(&sync, 1, WORKSPACE, away("away"));

    let mut tree = desk(WORKSPACE);
    let people = sync.show(&mut tree, &|_| None);
    assert_eq!(people[0].availability, ui::Availability::Away);
    assert!(people[0].availability.dims());
    let rows = tilo_desktop::collaborators::collaborator_roster_rows(&tree, "me");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].availability, ui::Availability::Away);

    // Back at their machine, and heads down: the face lightens, and says what they chose.
    heard(&sync, 1, WORKSPACE, away("focusing"));
    let mut tree = desk(WORKSPACE);
    sync.show(&mut tree, &|_| None);
    let rows = tilo_desktop::collaborators::collaborator_roster_rows(&tree, "me");
    assert_eq!(rows[0].availability, ui::Availability::Focusing);
    assert!(!rows[0].availability.dims());
}

/// A call this device joins is said in the workspace, so everyone else's roster shows this person
/// in that channel.
#[test]
fn a_call_this_device_joins_reaches_a_peer_s_roster() {
    let (mine, mut said) = session("Quinten", Some(0x04));
    while said.try_recv().is_ok() {}
    let voice = ui::CallPresence {
        channel: "c_voice".to_owned(),
        name: "Voice".to_owned(),
    };
    assert!(mine.in_call(Some(InCall {
        call: voice.clone(),
        camera: true,
        screen: false,
    })));

    // A peer's session in the same workspace, handed everything this device said there.
    let own = OwnPresence {
        label: "Ada".into(),
        ..OwnPresence::default()
    };
    let (theirs, _theirs_said) = Client::offline(device(0xe0), own);
    theirs.follow_desktop(&desk(WORKSPACE));
    let mut said_the_call = false;
    for frame in mine.take_said() {
        if matches!(
            &frame.payload,
            Payload::Availability {
                call: Some(call),
                camera: true,
                screen: false,
                ..
            } if ui::CallPresence::decode(call).as_ref() == Some(&voice)
        ) {
            said_the_call = true;
        }
        theirs.heard_presence(WORKSPACE, mine.publisher(), &frame);
    }
    assert!(said_the_call, "this device said which channel it is in");

    let mut tree = desk(WORKSPACE);
    let people = theirs.show(&mut tree, &|_| None);
    assert_eq!(people.len(), 1);
    assert_eq!(
        people[0].call.as_ref().map(|call| call.name.as_str()),
        Some("Voice")
    );
    // What this device sends there is heard too: its camera is on.
    let calls = theirs.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0]
            .people
            .iter()
            .map(|caller| (caller.camera, caller.screen))
            .collect::<Vec<_>>(),
        [(true, false)],
        "the camera this device turned on is what the peer reads"
    );
    let rows = tilo_desktop::collaborators::collaborator_roster_rows(&tree, "me");
    assert_eq!(rows[0].call.as_deref(), Some("Voice"));

    // Out of the call, the peer's roster says nothing of a channel.
    while said.try_recv().is_ok() {}
    assert!(mine.in_call(None));
    for frame in mine.take_said() {
        theirs.heard_presence(WORKSPACE, mine.publisher(), &frame);
    }
    let mut tree = desk(WORKSPACE);
    let people = theirs.show(&mut tree, &|_| None);
    assert!(people[0].call.is_none());
}

#[test]
fn a_peer_who_leaves_leaves_the_roster() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);

    heard(&sync, 1, WORKSPACE, PeerPresencePayload::Left);
    assert!(sync.show(&mut desk(WORKSPACE), &|_| None).is_empty());

    here(&sync, 1);
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

/// Two relays nobody listens on: this endpoint never has a home relay to be published with, so the
/// directory gate cannot open, and nothing here waits on a network.
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
    // The endpoint the real client binds, its directory gated and its relays held back.
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
    here(&sync, 1);
    assert!(!reachability.is_dormant());
    assert_eq!(
        reachability.holders(),
        (None, Some("a workspace is shown with someone else"))
    );

    // They leave: nothing here needs this device dialable again.
    heard(&sync, 1, WORKSPACE, PeerPresencePayload::Left);
    assert_eq!(reachability.holders(), (None, None));

    // Someone in a workspace this desktop does not show needs nothing of this device.
    heard(&sync, 2, OTHER_WORKSPACE, PeerPresencePayload::Arrived);
    assert_eq!(reachability.holders().1, None);
}

#[test]
fn only_the_workspace_in_front_lists_people() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);

    let elsewhere = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    heard(&sync, 2, elsewhere, PeerPresencePayload::Arrived);
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);

    // A frame that names another workspace is that other workspace's, whatever plane it was heard
    // on.
    let foreign = Presence {
        app: DESKTOP_APP.to_owned(),
        cluster: elsewhere.to_owned(),
        key: PresenceKey::Workspace {
            id: elsewhere.to_owned(),
        },
        payload: Payload::Arrived,
    };
    sync.heard_presence(WORKSPACE, device(3), &foreign);
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);
}

#[test]
fn another_apps_presence_is_not_the_desktops() {
    let (sync, _said) = session("Quinten", Some(0x04));
    let notes = Presence {
        app: "tilo-notes".to_owned(),
        cluster: WORKSPACE.to_owned(),
        key: PresenceKey::Workspace {
            id: WORKSPACE.to_owned(),
        },
        payload: Payload::Arrived,
    };
    sync.heard_presence(WORKSPACE, device(1), &notes);
    assert!(sync.show(&mut desk(WORKSPACE), &|_| None).is_empty());
}

#[test]
fn two_devices_of_one_peer_are_one_face() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);
    sync.hear(hello(2, 0xaa, "Phone"));
    here(&sync, 2);

    let people = sync.show(&mut desk(WORKSPACE), &|_| None);
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].label, "Framework 16");
}

#[test]
fn another_device_of_this_person_is_this_person() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0x04, "Studio"));
    here(&sync, 1);
    assert!(sync.show(&mut desk(WORKSPACE), &|_| None).is_empty());

    sync.hear(hello(2, 0xbb, "Framework 16"));
    here(&sync, 2);
    assert_eq!(sync.show(&mut desk(WORKSPACE), &|_| None).len(), 1);
}

/// The tile `tab` stands at on the desk of these tests.
fn tile(tab: u64) -> (u64, ui::kurbo::Rect) {
    (tab, ui::kurbo::Rect::new(100.0, 50.0, 500.0, 350.0))
}

#[test]
fn pointers_are_followed_on_the_room_tile() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);
    assert!(sync.placed(WORKSPACE, &[tile(3)]).is_empty());

    pointer_moved(&sync, 1, WORKSPACE, 3, (12.5, 34.5));
    assert_eq!(
        sync.placed(WORKSPACE, &[tile(3)]),
        vec![(device(1).to_string(), 3, 112.5, 84.5)],
        "the pointer is placed where the tile stands"
    );
    assert!(sync.placed(WORKSPACE, &[tile(4)]).is_empty(), "another tab");
    assert!(
        sync.placed("0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c", &[tile(3)])
            .is_empty()
    );

    // A pointer resting where it is, and the peer saying it is here again,
    // are no change: the desk draws no frame.
    let drawn = sync.presence_revision();
    pointer_moved(&sync, 1, WORKSPACE, 3, (12.5, 34.5));
    here(&sync, 1);
    assert_eq!(sync.presence_revision(), drawn, "nothing new was drawn");

    // Leaving the tile, and the device going away, each draw one frame.
    pointer_cleared(&sync, 1, WORKSPACE, 3);
    let left = sync.presence_revision();
    assert_ne!(left, drawn, "a leaving pointer asked for no frame");
    sync.hear(NetEvent::PeerLeft {
        ws: topic(WORKSPACE),
        peer: device(1),
        cause: tilo_network_actor::PeerDepartureCause::HostSessionClosed,
        membership_epoch: None,
    });
    assert_ne!(
        sync.presence_revision(),
        left,
        "a departing device drew nothing"
    );
}

/// An agent is on the desk the way a person's device is: its shell frame on the workspace's
/// presence track says its face, that it works here, and what it is doing, and the People panel
/// draws that.
#[test]
fn an_agents_shell_frame_puts_its_face_on_the_desk() {
    let (sync, _said) = session("Quinten", Some(0x04));
    let said = collab_types::presence::ShellPresence {
        seq: 1,
        sent_at_ms: 1,
        label: "Writer helper".to_owned(),
        agent: true,
        colour: "#7c3aed".to_owned(),
        activity: Some("Editing the notes".to_owned()),
        place: Some("Notes".to_owned()),
        size: [1440, 900],
        windows: Vec::new(),
        selection: None,
        chat: None,
        reaction: None,
    };
    let frame = Presence::desktop(WORKSPACE, PeerPresencePayload::Shell(Box::new(said)));
    sync.heard_presence(WORKSPACE, device(1), &frame);

    let mut tree = desk(WORKSPACE);
    let people = sync.show(&mut tree, &|_| None);
    assert_eq!(people.len(), 1, "the agent is a member here");
    assert_eq!(people[0].label, "Writer helper");
    assert!(people[0].agent, "an agent draws as one, not as a person");
    assert_eq!(
        people[0].status.as_ref().map(|line| line.text.clone()),
        Some("Editing the notes".to_owned()),
        "what it is doing reads as its line: {:?}",
        people[0].status
    );

    // The same frame said again draws nothing: an agent resting at its work asks no redraw.
    let drawn = sync.presence_revision();
    sync.heard_presence(WORKSPACE, device(1), &frame);
    assert_eq!(sync.presence_revision(), drawn);
}

#[test]
fn a_wave_reaches_the_person_it_is_for() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    named(&sync, 1, "Ada");
    let me = device(0xf0).to_string();
    heard(
        &sync,
        1,
        WORKSPACE,
        PeerPresencePayload::Wave { seq: 1, target: me },
    );
    assert!(
        sync.here().borrow_mut().take_waves().is_empty(),
        "the client takes the wave and hands it to the bar"
    );
    let notice = sync.wave_notice().expect("the bar has a wave to say");
    assert_eq!(notice.from, "Ada");

    // A wave at someone else here is not this person's news: the bar keeps saying the wave at this
    // person.
    sync.hear(hello(2, 0xbb, "Desktop"));
    named(&sync, 2, "Ben");
    heard(
        &sync,
        2,
        WORKSPACE,
        PeerPresencePayload::Wave {
            seq: 1,
            target: device(9).to_string(),
        },
    );
    assert_eq!(
        sync.wave_notice().map(|notice| notice.from),
        Some("Ada".to_owned()),
        "a wave at someone else here is not this person's news"
    );
}

#[test]
fn waving_says_a_new_wave_each_time() {
    let (sync, mut said) = session("Quinten", None);
    sent(&mut said);
    sync.take_said();
    let target = device(1).to_string();
    sync.wave(&target);
    sync.wave(&target);
    let waves: Vec<(u32, String)> = payloads(&sync)
        .into_iter()
        .filter_map(|payload| match payload {
            Payload::Wave { seq, target } => Some((seq, target)),
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
        payloads(&sync).is_empty(),
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

    assert!(
        commands
            .iter()
            .all(|command| !matches!(command, Outgoing::Publish { .. })),
        "presence no longer rides the workspace's messages"
    );
    let said = sync.take_said();
    assert!(!said.is_empty());
    for frame in said {
        assert_eq!(frame.app, DESKTOP_APP);
        assert_eq!(frame.cluster, WORKSPACE);
        assert_eq!(
            frame.key,
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
    sync.take_said();

    let stated = OwnPresence {
        label: "Quinten".to_owned(),
        status: Some("Wiring the plane".to_owned()),
        ..OwnPresence::default()
    };
    assert!(sync.set_own(stated.clone()));
    let said_payloads = payloads(&sync);
    assert!(said_payloads.contains(&Payload::Status {
        text: "Wiring the plane".to_owned(),
        automatic: false,
    }));

    assert!(!sync.set_own(stated));
    assert!(payloads(&sync).is_empty());
}

#[test]
fn leaving_a_workspace_leaves_its_topic() {
    let (sync, mut said) = session("Quinten", None);
    sent(&mut said);
    sync.follow_desktop(&DesktopTree::default());
    assert!(matches!(sent(&mut said)[0], Outgoing::Leave(ws) if ws == topic(WORKSPACE)));
}

/// What `sync` said on its workspaces' presence tracks since the last call,
/// by payload.
fn payloads(sync: &Client) -> Vec<Payload> {
    sync.take_said()
        .into_iter()
        .map(|frame| frame.payload)
        .collect()
}

#[test]
fn a_device_of_the_person_says_where_its_workspaces_are_reached() {
    let (sync, _said) = session("Desk", Some(0x04));
    let sync = sync.taking_part_as(&person(0x04));
    assert!(
        sync.host_hint(WORKSPACE).is_none(),
        "no device of the person said where a workspace is reached yet"
    );
    let (cert, hint) = host_hint(0x04, 0x21, "one address");
    sync.keep_personal(&NetEvent::SiblingMsg {
        peer: device(0x11),
        membership_epoch: tilo_network_actor::MembershipEpoch::for_test(1),
        msg: NeighbourMsg::PersonalHosts {
            user_public_key: root(0x04),
            hosts: vec![neighbour_wire::PersonalHost {
                workspace: WORKSPACE.to_owned(),
                hint: postcard::to_stdvec(&hint).expect("a signed address"),
            }],
        },
    });
    let kept = sync
        .host_hint(WORKSPACE)
        .expect("the device's address for the workspace is kept");
    assert_eq!(kept, hint);
    assert!(
        kept.host.device_public_key == cert.device_public_key,
        "it is the device's own answer"
    );
}

#[test]
fn another_persons_host_hints_are_not_where_this_persons_workspaces_are() {
    let (sync, _said) = session("Desk", Some(0x04));
    let sync = sync.taking_part_as(&person(0x04));
    let (_, hint) = host_hint(0x05, 0x22, "someone else's address");
    sync.keep_personal(&NetEvent::SiblingMsg {
        peer: device(0x12),
        membership_epoch: tilo_network_actor::MembershipEpoch::for_test(1),
        msg: NeighbourMsg::PersonalHosts {
            user_public_key: root(0x05),
            hosts: vec![neighbour_wire::PersonalHost {
                workspace: WORKSPACE.to_owned(),
                hint: postcard::to_stdvec(&hint).expect("a signed address"),
            }],
        },
    });
    assert!(
        sync.host_hint(WORKSPACE).is_none(),
        "another person's workspaces are theirs to enter, never this person's"
    );
}
