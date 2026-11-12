use super::*;
use collab_types::PeerPresencePayload;
use collab_types::presence::{DESKTOP_APP, Payload, Presence, PresenceKey};
use ed25519_dalek::SigningKey;
use tilo_desktop_model::{Workspace, WorkspaceId as ModelWorkspaceId};
use tilo_session_client::{InCall, presence::now_ms, topic_of};
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
    topic_of(ModelWorkspaceId::parse(workspace).expect("a workspace id"))
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
        issuer: None,
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
        .add_workspace_with_topic(topic_of(workspace), &workspace.to_string(), "Desk")
        .expect("a listed workspace");
    let update = directory
        .signed_snapshot(signing, tilo_clock::unix_now_ns())
        .expect("a signed list");
    assert_eq!(update.author_device_public_key, device.device_public_key);
    update
}

/// A session in `WORKSPACE` with no transport, and what it would have said.
fn session(label: &str, user: Option<u8>) -> (Client, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
    let own = OwnPresence {
        label: label.to_owned(),
        user: user.map(person),
        ..OwnPresence::default()
    };
    let (sync, said) = Client::offline(device(0xf0), own);
    sync.follow_desktop(&desk(WORKSPACE));
    (sync, said)
}

fn sent(said: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>) -> Vec<Cmd> {
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

fn hello(from: u8, user: u8, device_label: &str) -> Event {
    let from = device(from);
    let root = ed25519_dalek::SigningKey::from_bytes(&[user; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *from.as_bytes(),
        device_label: device_label.to_owned(),
        capabilities: DeviceCapabilities::nothing(),
        issued_at_ns: 0,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).expect("a certified device");
    Event::Msg {
        ws: topic(WORKSPACE),
        peer: from,
        msg: Box::new(NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        }),
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

/// A client whose transport another host brings: what it says leaves on that transport, and what
/// the transport hands it is who is here.
#[test]
fn a_transport_another_host_brings_carries_the_same_words() {
    let publisher = device(0x31);
    let (outgoing, mut said) = tokio::sync::mpsc::unbounded_channel();
    let (heard, arrived) = std::sync::mpsc::channel();
    let (_landed, enrolled) = std::sync::mpsc::channel();
    let sync = Client::over(
        Transport {
            outgoing: Arc::new(outgoing),
            arrived,
            enrolled,
            saying: None,
        },
        Device {
            publisher,
            endpoint: EndpointAddr::new(publisher),
            person: Some(root(0x04)),
            name: "Laptop".to_owned(),
        },
        OwnPresence {
            label: "Quinten".to_owned(),
            ..OwnPresence::default()
        },
    );
    sync.follow_desktop(&desk(WORKSPACE));
    let commands = sent(&mut said);
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, Cmd::Join(ws) if *ws == topic(WORKSPACE))),
        "the client takes part in the workspace its desk shows"
    );
    heard
        .send(hello(2, 0x04, "Phone"))
        .expect("the transport hands what it heard");
    sync.follow();
    // The device's own word arrives on the workspace's live plane, which this host's transport
    // carries as the desktop's does.
    here(&sync, 2);
    named(&sync, 2, "Ana");
    let here = sync.members(WORKSPACE);
    assert_eq!(here.len(), 1, "the device that said hello is here");
    assert_eq!(here[0].label, "Ana", "by the person's own word");
    assert_eq!(
        here[0].device, "Phone",
        "and its certificate says which device"
    );
}

#[test]
fn a_device_of_the_person_coming_up_owes_it_the_list() {
    let (sync, _said) = session("Desk", Some(0x04));
    assert!(
        !sync.sibling_arrived(),
        "no device of the person said anything yet"
    );
    sync.hear(Event::Sibling {
        peer: device(0x11),
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

    let kept = tilo_session_client::client::author_of(&[phone.clone(), laptop.clone()], &by_laptop)
        .expect("the laptop's own list is the laptop's");
    assert_eq!(kept.device_public_key, laptop.device_public_key);

    // Someone else's device, and a list the laptop's session carried but another device signed:
    // neither says the laptop said it.
    let (stranger, _) = certified(0x55, 0x13, "Stranger");
    assert!(tilo_session_client::client::author_of(&[stranger], &by_laptop).is_none());
    assert!(tilo_session_client::client::author_of(&[phone], &by_laptop).is_none());
    assert!(tilo_session_client::client::author_of(&[], &by_laptop).is_none());
}

#[test]
fn a_peer_here_is_a_collaborator_of_the_active_workspace() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);

    let mut tree = desk(WORKSPACE);
    let people = sync.show(&mut tree, &no_records(), &|_| None);
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
    let people = sync.show(&mut tree, &no_records(), &|_| None);
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
    let people = sync.show(&mut tree, &no_records(), &|_| None);
    assert_eq!(people[0].availability, ui::Availability::Away);
    assert!(people[0].availability.dims());
    let rows = tilo_desktop::collaborators::collaborator_roster_rows(&tree, "me");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].availability, ui::Availability::Away);

    // Back at their machine, and heads down: the face lightens, and says what they chose.
    heard(&sync, 1, WORKSPACE, away("focusing"));
    let mut tree = desk(WORKSPACE);
    sync.show(&mut tree, &no_records(), &|_| None);
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
    let people = theirs.show(&mut tree, &no_records(), &|_| None);
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
    let people = theirs.show(&mut tree, &no_records(), &|_| None);
    assert!(people[0].call.is_none());
}

#[test]
fn a_peer_who_leaves_leaves_the_roster() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);
    assert_eq!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .len(),
        1
    );

    heard(&sync, 1, WORKSPACE, PeerPresencePayload::Left);
    assert!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .is_empty()
    );

    here(&sync, 1);
    assert_eq!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .len(),
        1
    );
    sync.hear(Event::PeerLeft {
        ws: topic(WORKSPACE),
        peer: device(1),
    });
    assert!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .is_empty()
    );

    let mut tree = desk(WORKSPACE);
    sync.show(&mut tree, &no_records(), &|_| None);
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
    let (sync, _said) = Client::offline(device(0xf0), own);
    sync.hold_relays_with(reachability.clone());
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
    assert_eq!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .len(),
        1
    );

    // A frame that names another workspace is that other workspace's, whatever plane it was heard
    // on.
    let foreign = Presence {
        app: DESKTOP_APP.to_owned(),
        cluster: elsewhere.to_owned(),
        key: PresenceKey::Workspace {
            id: elsewhere.to_owned(),
        },
        payload: Payload::Arrived,
        on_behalf: None,
    };
    sync.heard_presence(WORKSPACE, device(3), &foreign);
    assert_eq!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .len(),
        1
    );
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
        on_behalf: None,
    };
    sync.heard_presence(WORKSPACE, device(1), &notes);
    assert!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .is_empty()
    );
}

#[test]
fn two_devices_of_one_peer_are_one_face() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);
    sync.hear(hello(2, 0xaa, "Phone"));
    here(&sync, 2);

    let people = sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None);
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].label, "Framework 16");
}

#[test]
fn another_device_of_this_person_is_this_person() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0x04, "Studio"));
    here(&sync, 1);
    assert!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .is_empty()
    );

    sync.hear(hello(2, 0xbb, "Framework 16"));
    here(&sync, 2);
    assert_eq!(
        sync.show(&mut desk(WORKSPACE), &no_records(), &|_| None)
            .len(),
        1
    );
}

/// No window of the desk shows a record.
fn no_records() -> crate::continuations::Continuations {
    crate::continuations::Continuations::default()
}

/// The tile `tab` stands at on the desk of these tests.
fn tile(tab: u64) -> tilo_live::presence::Tile {
    tilo_live::presence::Tile::new(tab, 100.0, 50.0, 400.0, 300.0)
}

/// Everyone's pointer as the desk draws it, over `tiles`: who, the tab they are over, and where it
/// stands on the surface.
fn drawn_on(sync: &Client, tiles: &[tilo_live::presence::Tile]) -> Vec<(u64, f64, f64)> {
    let mut chats = ui::presence::CursorChats::default();
    let pointers = tilo_session_core::presence::pointers(
        &sync.members(WORKSPACE),
        tiles,
        &mut chats,
        &ui::presence::PresenceCtx::empty(),
        ui::time::Instant::now(),
        now_ms(),
        ui::tokens::EffectiveMotion::default(),
    );
    pointers
        .drawn
        .iter()
        .zip(pointers.tabs)
        .map(|(cursor, tab)| (tab, cursor.pos.0, cursor.pos.1))
        .collect()
}

#[test]
fn pointers_are_followed_on_the_room_tile() {
    let (sync, _said) = session("Quinten", Some(0x04));
    sync.hear(hello(1, 0xaa, "Framework 16"));
    here(&sync, 1);
    assert!(drawn_on(&sync, &[tile(3)]).is_empty());

    pointer_moved(&sync, 1, WORKSPACE, 3, (12.5, 34.5));
    assert_eq!(
        drawn_on(&sync, &[tile(3)]),
        vec![(3, 112.5, 84.5)],
        "the pointer is placed where the tile stands"
    );
    assert!(drawn_on(&sync, &[tile(4)]).is_empty(), "another tab");

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
    sync.hear(Event::PeerLeft {
        ws: topic(WORKSPACE),
        peer: device(1),
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
    let people = sync.show(&mut tree, &no_records(), &|_| None);
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
        sync.own_here().wave.map(|wave| wave.seq),
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

/// This device's endpoint holds back the relays its person chose, and a session that takes part as
/// nobody relays nowhere.
#[test]
fn this_devices_endpoint_holds_back_the_relays_its_person_chose() {
    let (cert, key) = certified(1, 2, "here");
    let identity = HostIdentity::kept(cert, key);
    let own = tilo_endpoint_reachability::EndpointRelays::for_choice(
        &settings_store::relay::Relays::Own("https://relay.example.com.".into()),
    );
    let held = endpoint_for(Some(&identity), &own)
        .expect("an endpoint to bind")
        .1;
    assert_eq!(held.urls::<Vec<_>>(), own.relays.urls::<Vec<_>>());
    let nobody = endpoint_for(None, &own)
        .expect("a loopback endpoint to bind")
        .1;
    assert!(
        nobody.is_empty(),
        "a session that takes part as nobody relays nowhere"
    );
}

#[test]
fn a_host_is_dialled_before_the_workspaces_it_carries() {
    let host = device(7);
    let (sync, mut said) = Client::planeless(
        device(0xf0),
        OwnPresence::default(),
        vec![EndpointAddr::new(host)],
    );
    sync.follow_desktop(&desk(WORKSPACE));
    let commands = sent(&mut said);
    let connect = commands
        .iter()
        .position(|command| matches!(command, Cmd::Connect(addr) if addr.id == host));
    let join = commands
        .iter()
        .position(|command| matches!(command, Cmd::Join(_)));
    assert_eq!(connect, Some(0), "the host is dialled first");
    assert_eq!(join, Some(1), "the workspace is joined after its host");
}

#[test]
fn taking_part_says_where_this_person_is() {
    let (sync, mut said) = session("Quinten", None);
    let commands = sent(&mut said);
    assert!(matches!(commands[0], Cmd::Join(ws) if ws == topic(WORKSPACE)));

    assert!(
        commands
            .iter()
            .all(|command| !matches!(command, Cmd::Publish { .. })),
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
        status: Some(tilo_live::presence::Status {
            text: "Wiring the plane".to_owned(),
            automatic: false,
        }),
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
    assert!(matches!(sent(&mut said)[0], Cmd::Leave(ws) if ws == topic(WORKSPACE)));
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
    sync.hear(Event::SiblingMsg {
        peer: device(0x11),
        msg: Box::new(NeighbourMsg::PersonalHosts {
            user_public_key: root(0x04),
            hosts: vec![neighbour_wire::PersonalHost {
                workspace: WORKSPACE.to_owned(),
                hint: postcard::to_stdvec(&hint).expect("a signed address"),
            }],
        }),
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
    sync.hear(Event::SiblingMsg {
        peer: device(0x12),
        msg: Box::new(NeighbourMsg::PersonalHosts {
            user_public_key: root(0x05),
            hosts: vec![neighbour_wire::PersonalHost {
                workspace: WORKSPACE.to_owned(),
                hint: postcard::to_stdvec(&hint).expect("a signed address"),
            }],
        }),
    });
    assert!(
        sync.host_hint(WORKSPACE).is_none(),
        "another person's workspaces are theirs to enter, never this person's"
    );
}

/// Where this person is on their desk — the tile they point at and their shell — is said with the
/// rest of their word, and only what moved is said again: a desk at rest publishes nothing.
#[test]
fn where_i_am_is_said_with_the_rest_of_the_word_and_only_when_it_moves() {
    let (sync, _said) = session("Quinten", Some(0x04));
    let shell = |sent_at_ms| {
        Box::new(collab_types::presence::ShellPresence {
            seq: 0,
            sent_at_ms,
            label: "Quinten".to_owned(),
            agent: false,
            colour: "#3b82f6".to_owned(),
            activity: None,
            place: Some("1".to_owned()),
            size: [1280, 764],
            windows: vec![collab_types::presence::ShellWindow {
                app: "so.tilo.notes".to_owned(),
                record: [1; 8],
                rect: [0, 0, 10_000, 10_000],
                focused: true,
            }],
            selection: None,
            chat: None,
            reaction: None,
        })
    };
    let at = tilo_live::presence::Point {
        tab: 3,
        x: 12.5,
        y: 34.5,
    };
    assert!(sync.set_where(Some(at), Some(shell(1))));
    let said = sync.take_said();
    assert!(
        said.iter().any(
            |presence| matches!(&presence.payload, Payload::Shell(shell) if shell.windows
                .first()
                .is_some_and(|window| window.app == "so.tilo.notes"))
        ),
        "their shell is said with their word: {said:?}"
    );
    assert!(
        said.iter()
            .any(|presence| matches!(presence.payload, Payload::Pointer { .. })),
        "and the tile they point at: {said:?}"
    );

    // The same place, restamped: no word is owed.
    assert!(!sync.set_where(Some(at), Some(shell(2))));
    assert!(sync.take_said().is_empty(), "nothing moved");

    // Their pointer leaves the tile: that is said.
    assert!(sync.set_where(None, Some(shell(3))));
    assert!(!sync.take_said().is_empty());
}

/// A page names the host it was admitted by as its workspace's host itself, and its session can
/// tell no host of its own: following its workspaces again keeps the run that host speaks for, so
/// the agent stays in the bar rather than leaving on every pass until its next keep-alive.
#[test]
fn a_run_the_page_host_speaks_for_stays_while_the_page_follows_its_workspaces() {
    let publisher = device(0x33);
    let (outgoing, _said) = tokio::sync::mpsc::unbounded_channel();
    let (_heard, arrived) = std::sync::mpsc::channel();
    let (_landed, enrolled) = std::sync::mpsc::channel();
    let sync = Client::over(
        Transport {
            outgoing: Arc::new(outgoing),
            arrived,
            enrolled,
            saying: None,
        },
        Device {
            publisher,
            endpoint: EndpointAddr::new(publisher),
            person: Some(root(0x05)),
            name: "Page".to_owned(),
        },
        OwnPresence::default(),
    );
    let showing = [(topic(WORKSPACE), WORKSPACE.to_owned())];
    sync.follow_workspaces(&showing, Some(topic(WORKSPACE)));
    let host = device(0x40);
    sync.set_workspace_host(WORKSPACE, Some(*host.as_bytes()));
    let run = Presence::desktop(WORKSPACE, Payload::Arrived).spoken_for("run-1");
    sync.heard_presence(WORKSPACE, host, &run);
    assert_eq!(sync.members(WORKSPACE).len(), 1, "the run is here");

    sync.follow_workspaces(&showing, Some(topic(WORKSPACE)));
    assert_eq!(
        sync.members(WORKSPACE).len(),
        1,
        "following the same workspaces again keeps the run the host speaks for"
    );
}

/// A host that names the workspaces it shows itself — a page's mounts, each showing one — takes
/// part in each of them, says its word in the one in front, and keeps a schedule of its own for
/// each: what one of them owes is pumped without the other hearing it.
#[test]
fn a_host_that_names_its_workspaces_says_and_pumps_each_of_them() {
    let publisher = device(0x32);
    let (outgoing, mut said) = tokio::sync::mpsc::unbounded_channel();
    let (_heard, arrived) = std::sync::mpsc::channel();
    let (_landed, enrolled) = std::sync::mpsc::channel();
    let sync = Client::over(
        Transport {
            outgoing: Arc::new(outgoing),
            arrived,
            enrolled,
            saying: None,
        },
        Device {
            publisher,
            endpoint: EndpointAddr::new(publisher),
            person: Some(root(0x05)),
            name: "Page".to_owned(),
        },
        OwnPresence {
            label: "Quinten".to_owned(),
            ..OwnPresence::default()
        },
    );
    let showing = [
        (topic(WORKSPACE), WORKSPACE.to_owned()),
        (topic(OTHER_WORKSPACE), OTHER_WORKSPACE.to_owned()),
    ];
    sync.follow_workspaces(&showing, Some(topic(WORKSPACE)));
    let commands = sent(&mut said);
    for workspace in [WORKSPACE, OTHER_WORKSPACE] {
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, Cmd::Join(ws) if *ws == topic(workspace))),
            "the host takes part in every workspace it shows"
        );
    }
    let word = sync.take_said();
    for workspace in [WORKSPACE, OTHER_WORKSPACE] {
        assert!(
            word.iter().any(
                |frame| frame.cluster == workspace && matches!(frame.payload, Payload::Arrived)
            ),
            "a workspace this host joins hears it arrive there: {word:?}"
        );
    }
    // What changes after the join is said in the workspace in front alone.
    assert!(sync.set_own(OwnPresence {
        label: "Quinn".to_owned(),
        ..OwnPresence::default()
    }));
    let changed = sync.take_said();
    assert!(!changed.is_empty());
    assert!(
        changed.iter().all(|frame| frame.cluster == WORKSPACE),
        "the front workspace hears what changed: {changed:?}"
    );

    // Each of the two keeps a schedule of its own, whether or not it is in front: what one owes
    // waits on that one's clock, and the other's is untouched.
    assert!(sync.next_due_in(WORKSPACE).is_some());
    assert!(sync.next_due_in(OTHER_WORKSPACE).is_some());
    assert!(
        !sync.due_in(OTHER_WORKSPACE),
        "nothing is owed before the rate ceiling passes"
    );
    assert!(sync.take_said().is_empty());

    // A workspace this host no longer shows hears this device leave it.
    sync.follow_workspaces(
        &[(topic(WORKSPACE), WORKSPACE.to_owned())],
        Some(topic(WORKSPACE)),
    );
    let left = sync.take_said();
    assert!(
        left.iter().any(|frame| frame.cluster == OTHER_WORKSPACE
            && matches!(frame.payload, Payload::Left)),
        "the workspace this host stopped showing hears it leave: {left:?}"
    );
}

/// A page and a native desk in one workspace, with an MCP agent of the desk's person acting there:
/// each publishes its own presence on the workspace's live plane and reads everyone else's from
/// it, and both list the agent as the same one row, in its colour, with what it is doing, and
/// each other's lines as their devices said them.
#[test]
fn a_page_and_a_desk_see_the_same_rows_and_lines_of_everyone_here() {
    use tilo_desktop::collaborators::LineSource::{Observed, Written};

    let (page_device, desk_device, agent_device) = (0xa0, 0xd0, 0xc0);
    let (page, _page_said) = Client::offline(device(page_device), saying("Ada", 2, None));
    let (native, _desk_said) = Client::offline(device(desk_device), saying("Quinten", 1, None));
    for client in [&page, &native] {
        client.follow_desktop(&desk(WORKSPACE));
        client.take_said();
    }
    // Ada says what she is doing in her own words; Quinten's desk reads his line off his steps.
    page.set_own(saying("Ada", 2, Some(("Reviewing the launch plan", false))));
    native.set_own(saying(
        "Quinten",
        1,
        Some(("Editing ‘Plan’ in Notes", true)),
    ));

    // Every device's certificate, then every frame, reaches the others over the plane.
    page.hear(hello(desk_device, 1, "Framework"));
    page.hear(hello(agent_device, 1, "build-box"));
    native.hear(hello(page_device, 2, "Browser"));
    native.hear(hello(agent_device, 1, "build-box"));
    for frame in page.take_said() {
        native.heard_presence(WORKSPACE, device(page_device), &frame);
    }
    for frame in native.take_said() {
        page.heard_presence(WORKSPACE, device(desk_device), &frame);
    }
    for frame in &mcp_agent_frames(agent_device) {
        page.heard_presence(WORKSPACE, device(agent_device), frame);
        native.heard_presence(WORKSPACE, device(agent_device), frame);
    }

    let (on_page, on_desk) = (roster_rows(&page), roster_rows(&native));
    let agents = |rows: &[RosterRow]| -> Vec<RosterRow> {
        rows.iter().filter(|row| row.1).cloned().collect()
    };
    assert_eq!(
        agents(&on_page),
        vec![(
            "Claude Code".to_owned(),
            true,
            Some(("Looking at the apps".to_owned(), Written)),
            AGENT_COLOUR,
        )],
        "one row for the agent, in the colour its face announced, with its activity line: {on_page:?}"
    );
    assert_eq!(
        agents(&on_page),
        agents(&on_desk),
        "the same row everywhere"
    );
    let line_of = |rows: &[RosterRow], name: &str| rows.iter().find(|row| row.0 == name)?.2.clone();
    assert_eq!(
        line_of(&on_page, "Quinten"),
        Some(("Editing ‘Plan’ in Notes".to_owned(), Observed)),
        "the page reads the desk's line as read off his steps: {on_page:?}"
    );
    assert_eq!(
        line_of(&on_desk, "Ada"),
        Some(("Reviewing the launch plan".to_owned(), Written)),
        "the desk reads the page's line as her own words: {on_desk:?}"
    );
    assert_eq!(
        on_page.len(),
        2,
        "Quinten and his agent, never Ada: {on_page:?}"
    );
    assert_eq!(
        on_desk.len(),
        2,
        "Ada and Quinten's agent, its own row and never one of his devices: {on_desk:?}"
    );
}

/// A browser joins a workspace a desk and another page are in, then closes without a word, as a
/// tab killed with its browser does: the others stop listing it within a few seconds, on their own
/// clocks, while everyone still connected stays.
#[test]
fn a_browser_that_closes_without_a_word_leaves_who_is_here_within_seconds() {
    let (gone, other, native) = (0xa0, 0xb0, 0xd0);
    let clients = [
        (
            gone,
            Client::offline(device(gone), saying("Ada", 2, None)).0,
        ),
        (
            other,
            Client::offline(device(other), saying("Grace", 3, None)).0,
        ),
        (
            native,
            Client::offline(device(native), saying("Quinten", 1, None)).0,
        ),
    ];
    for (own, client) in &clients {
        client.follow_desktop(&desk(WORKSPACE));
        for (seed, user, label) in [
            (gone, 2, "Browser"),
            (other, 3, "Laptop"),
            (native, 1, "Desk"),
        ] {
            if seed != *own {
                client.hear(hello(seed, user, label));
            }
        }
    }
    // Everyone's word reaches everyone else over the plane, each time anyone says one.
    let carry = |clients: &[(u8, Client)]| {
        for (from, speaker) in clients {
            speaker.follow();
            for frame in speaker.take_said() {
                for (to, listener) in clients {
                    if to != from {
                        listener.heard_presence(WORKSPACE, device(*from), &frame);
                    }
                }
            }
        }
    };
    carry(&clients);
    let names = |client: &Client| -> Vec<String> {
        let mut names: Vec<String> = roster_rows(client).into_iter().map(|row| row.0).collect();
        names.sort();
        names
    };
    let [_, (_, grace), (_, quinten)] = &clients;
    assert_eq!(names(quinten), ["Ada", "Grace"], "the browser is here");
    assert_eq!(names(grace), ["Ada", "Quinten"], "the browser is here");

    // The browser closes: nothing more of it reaches anyone, and nobody said it left.
    let closed = std::time::Instant::now();
    let still = &clients[1..];
    let bound = std::time::Duration::from_secs_f64(tilo_live::presence::SILENT_MS / 1_000.0 + 2.0);
    loop {
        carry(still);
        if names(quinten) == ["Grace"] && names(grace) == ["Quinten"] {
            break;
        }
        assert!(
            closed.elapsed() < bound,
            "the closed browser is still listed after {:?}: desk {:?}, page {:?}",
            closed.elapsed(),
            names(quinten),
            names(grace),
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// What person `user`, called `label`, says of themselves: `status` their line and whether their
/// device read it off their steps.
fn saying(label: &str, user: u8, status: Option<(&str, bool)>) -> OwnPresence {
    OwnPresence {
        label: label.to_owned(),
        user: Some(person(user)),
        status: status.map(|(text, automatic)| tilo_live::presence::Status {
            text: text.to_owned(),
            automatic,
        }),
        ..OwnPresence::default()
    }
}

/// One row of who is here as a roster lists it: name, whether an agent, its line and who wrote
/// it, and its colour.
type RosterRow = (
    String,
    bool,
    Option<(String, tilo_desktop::collaborators::LineSource)>,
    (u8, u8, u8),
);

/// Who is here, as `client`'s roster lists them.
fn roster_rows(client: &Client) -> Vec<RosterRow> {
    client
        .people(&desk(WORKSPACE), &no_records(), &|_: &str| None)
        .into_iter()
        .map(|person| {
            let line = person.status.map(|line| (line.text, line.source));
            (person.label, person.agent, line, person.rgb)
        })
        .collect()
}

/// The colour an agent's face announces: what it is drawn in wherever a person reads the roster,
/// rather than a colour the surface picks for it.
const AGENT_COLOUR: (u8, u8, u8) = (0xd9, 0x77, 0x57);

/// What the agent `tilo mcp` serves for person 1 says of itself on the plane, from `device`, the
/// way every agent does: a shell that is an agent, with the line it works on.
fn mcp_agent_frames(device_seed: u8) -> Vec<Presence> {
    let mut agent =
        tilo_live::presence::Board::new(WORKSPACE.to_owned(), *device(device_seed).as_bytes());
    agent.say(&tilo_live::presence::Own {
        label: "Claude Code".to_owned(),
        user: Some(person(1)),
        shell: Some(Box::new(collab_types::presence::ShellPresence {
            seq: 1,
            sent_at_ms: 0,
            label: "Claude Code".to_owned(),
            agent: true,
            colour: format!(
                "#{:02x}{:02x}{:02x}",
                AGENT_COLOUR.0, AGENT_COLOUR.1, AGENT_COLOUR.2
            ),
            activity: Some("Looking at the apps".to_owned()),
            place: None,
            size: [0, 0],
            windows: Vec::new(),
            selection: None,
            chat: None,
            reaction: None,
        })),
        ..tilo_live::presence::Own::default()
    });
    agent
        .flush(0.0)
        .into_iter()
        .map(|(_, frame)| frame)
        .collect()
}
