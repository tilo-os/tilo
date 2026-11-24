//! A call on the native shell, in two shapes: an app joins a voice channel
//! on a session that can carry one, and the person and the people in the
//! workspace hear each other; and a session with no transport ends the
//! call it cannot carry, saying why.
//!
//! The audio here is the call's own: the same encoder, live session, sealing and
//! playout a device's call runs, ending in memory instead of a speaker,
//! and a tone instead of a microphone.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::platform::fabric::Continuity;
use crate::sync::Event;
use collab_types::Ed25519PublicKey;
use collab_types::presence::{Availability, Payload, Presence};
use iroh::EndpointId;
use key_group::{KeyGroup, MemberKey, VaultKey};
use runtime_collab::native_media::{Command, Request, State};
use tilo_broker_core::collab_core::BrokerChannels;
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_network_actor::DeviceIdentity;
use tilo_workspace_keys::{
    GovernanceTransport, InboundGovernance, KeyAccess, KeyDirectory, KeyProvider, KeysConfig,
    SharedKeys, WorkspaceKeys,
};

use super::*;
use crate::sync::{Client, Entry, OwnPresence};
use crate::{AppSocket, Options, Session};
use tilo_live::live_session::Join;
use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};
use tilo_live::roster::Roster as LiveRoster;
use tilo_live::scope::Scope;

const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
const CHANNEL: &str = "c_standup";
const APP: &str = "so.tilo.chat";
const RECORD: &str = "chat";
/// What the tone's playout reads as heard, against the silence a call plays while nobody speaks.
const HEARD: f32 = 0.02;

fn public(seed: u8) -> EndpointId {
    test_server::endpoint_key(seed).public()
}

/// The person a device is a device of, in the hex everyone here knows them by.
fn person(seed: u8) -> String {
    hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

/// One device's key, as the workspace's governance names it.
fn key(seed: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(test_server::signing_key(seed).verifying_key().to_bytes())
}

/// The person all of a call's devices are devices of.
fn person_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[21; 32])
}

/// The person everyone here knows by `seed`: the root of a device's certificate and of the one the
/// devices it meets hear, as [`person`] names them.
fn person_signing(seed: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse(WORKSPACE).expect("a workspace id")
}

fn target() -> CallTarget {
    CallTarget {
        workspace: WORKSPACE.to_owned(),
        app: APP.to_owned(),
        record: RECORD.to_owned(),
        channel: CHANNEL.to_owned(),
    }
}

/// A shell showing one workspace, as this device holds it.
fn layout_tree() -> Layout {
    let workspace_id = workspace_id();
    let workspace = Workspace::new(workspace_id, "Session");
    Layout {
        workspaces: vec![workspace.clone()],
        active: workspace.id(),
        ..Layout::default()
    }
}

/// A directory for the live session's socket that only this user reaches.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the live session in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the live session's directory is this user's");
    root
}

/// One of the person's devices, on its own loopback endpoint, taking part in the collaboration
/// live session as its certified self.
fn device(person: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> Client {
    let key = test_server::signing_key(seed);
    let cert = test_server::certified(
        person,
        &key,
        label,
        tilo_user_identity::DeviceCapabilities::member(),
    );
    Client::start(
        Entry {
            identity: Some(DeviceIdentity::kept(cert, key)),
            own: OwnPresence::default(),
            hosts: Vec::new(),
            continuity: Continuity::default(),
            wake: Arc::new(|| {}),
            relays: test_server::loopback(),
            lan: None,
        },
        None,
        None,
        Vec::new(),
        None,
        tilo_workspace_service::service::ServiceSlot::default(),
    )
    .expect("a loopback endpoint binds")
}

/// The workspace as this device knows its governance: both of a call's devices may read its
/// channels.
struct Directory {
    admitted: std::collections::BTreeSet<Ed25519PublicKey>,
}

impl KeyDirectory for Directory {
    fn allowed_devices(
        &self,
        _workspace: WorkspaceId,
        _now: u64,
    ) -> Option<std::collections::BTreeSet<Ed25519PublicKey>> {
        Some(self.admitted.clone())
    }
    fn founds(&self, _workspace: WorkspaceId) -> bool {
        true
    }
}

/// One channel's keys, as the device `device` of the workspace whose vault key is `vault` runs
/// holds them: the same service every device runs, with the channel's keys still on their way until
/// someone founds them.
fn keys(device: u8, vault: u8, admitted: &[u8]) -> (KeyProvider, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("a directory to keep channel state in");
    let checks = Arc::new(tilo_broker_core::collab_core::BrokerSealChecks(Arc::new(
        Mutex::new(BrokerChannels::default()),
    )));
    let encryption = WorkspaceKeys::open(
        KeysConfig {
            state: Arc::new(tilo_workspace_keys::KeyDir::new(
                directory.path().join("channels"),
            )),
            vault: VaultKey::new([vault; 32]),
            signer: std::sync::Arc::new(test_server::signing_key(device)),
            role: tilo_workspace_keys::KeyRole::Committer,
        },
        Box::new(Directory {
            admitted: admitted.iter().copied().map(key).collect(),
        }),
        checks,
    )
    .expect("a channel service");
    let provider = KeyProvider::default();
    provider.set(Some(SharedKeys::new(encryption)));
    (provider, directory)
}

/// Found the call's channel, as the pass a session runs does: the first device that takes part
/// creates the group once its wait is due, and the test's workspace has no other member to
/// ask.
#[allow(
    clippy::future_not_send,
    reason = "the channel service is driven from the test's own thread, and the test runtime does not need it to be Send"
)]
async fn founded(keys: &KeyProvider) {
    struct Nowhere;
    impl GovernanceTransport for Nowhere {
        fn publish(&self, _message: neighbour_wire::NeighbourMsg) -> bool {
            false
        }
        fn take_inbound(&self) -> Vec<InboundGovernance> {
            Vec::new()
        }
    }
    let access = keys.get().expect("this device's channel service");
    // Reading first is what makes the device take part, as a call does.
    access.access(workspace_id());
    let deadline = tilo_test_env::hang_deadline();
    loop {
        access.pump(&Nowhere);
        if matches!(access.access(workspace_id()).0, KeyAccess::Ready(_)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the call's channel is founded once a device's wait is due"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A session taking part in the workspace through `sync` , showing the shell.
fn session(sync: &std::rc::Rc<Client>, socket: &std::path::Path) -> Session {
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            sync: Some(std::rc::Rc::clone(sync)),
            app_socket: AppSocket::At(socket.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    session.handle().replace_tree(layout_tree());
    session
}

fn step(session: &mut Session) {
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// One thing `from` said in the workspace.
fn said(from: u8, msg: neighbour_wire::NeighbourMsg) -> Event {
    Event::Msg {
        ws: workspace_id(),
        peer: public(from),
        msg: Box::new(msg),
    }
}

/// A device's identity: the person it is a device of, and its name here.
fn hello(from: u8, label: &str) -> Event {
    let device_key = public(from);
    let root = person_signing(from);
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
        from,
        neighbour_wire::NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        },
    )
}

/// One presence `from` said of themselves on the workspace's live session.
fn presence(client: &Client, from: u8, payload: collab_types::PeerPresencePayload) {
    let payload = match payload {
        collab_types::PeerPresencePayload::Arrived => Payload::Arrived,
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
        other => panic!("the tests say no {other:?} as a desktop"),
    };
    let frame = Presence::desktop(WORKSPACE, payload);
    client.heard_presence(WORKSPACE, public(from), &frame);
}

/// How available `from` says they are, and the call they say they are in.
fn availability(client: &Client, from: u8, call: Option<String>) {
    presence(
        client,
        from,
        collab_types::PeerPresencePayload::Availability {
            state: Availability::default().id().to_owned(),
            call,
            camera: false,
            screen: false,
        },
    );
}

/// `from` is in the call `call` names, with their camera and screen as `camera` and `screen` say.
fn availability_showing(client: &Client, from: u8, call: String, camera: bool, screen: bool) {
    presence(
        client,
        from,
        collab_types::PeerPresencePayload::Availability {
            state: Availability::default().id().to_owned(),
            call: Some(call),
            camera,
            screen,
        },
    );
}

/// The call `CHANNEL` is, as a person in it publishes it.
fn call_presence() -> String {
    ui::CallPresence {
        channel: CHANNEL.to_owned(),
        name: "Standup".to_owned(),
        bridge: None,
    }
    .encode()
}

/// What a call's playout reads over `over` here: the tone's energy, or the silence a call plays
/// while nobody speaks.
async fn level(probe: &Arc<Mutex<Vec<f32>>>, over: Duration) -> f32 {
    if let Ok(mut played) = probe.lock() {
        played.clear();
    }
    tokio::time::sleep(over).await;
    let played = probe
        .lock()
        .map(|played| played.clone())
        .unwrap_or_default();
    let energy: f32 = played.iter().map(|sample| sample * sample).sum();
    let samples: f32 = played.iter().map(|_| 1.0).sum::<f32>().max(1.0);
    (energy / samples).sqrt()
}

/// The instant `bound` from now: the moment a sound a person made has to be heard by.
fn within(bound: Duration) -> Instant {
    Instant::now()
        .checked_add(bound)
        .expect("a deadline this machine reaches")
}

/// Wait until this side of the call does, or does not, hear the tone.
/// What it waits for is the call's own audio: moved by the call's worker on its own thread, between
/// two sessions' devices, so the bound is the suite's, not a second count.
#[allow(
    clippy::future_not_send,
    reason = "a session's client is not Send, and the test runtime does not need it to be"
)]
async fn wait_heard(what: &str, probe: &Arc<Mutex<Vec<f32>>>, heard: bool) {
    wait_heard_by(what, probe, heard, tilo_test_env::hang_deadline()).await;
}

/// As [`wait_heard`], by the instant `deadline` : the audio a person unmuting is heard again is
/// theirs at once, not after a bound long enough for a stream that started over to catch up with
/// where it was.
#[allow(
    clippy::future_not_send,
    reason = "a session's client is not Send, and the test runtime does not need it to be"
)]
async fn wait_heard_by(what: &str, probe: &Arc<Mutex<Vec<f32>>>, heard: bool, deadline: Instant) {
    loop {
        let playing = level(probe, Duration::from_millis(200)).await > HEARD;
        if playing == heard {
            return;
        }
        assert!(Instant::now() < deadline, "{what}");
    }
}

/// A session that carries its calls: the endpoint it binds, the channel keys, and the workspace's
/// admitted devices.
struct Carrier {
    calls: Arc<Calls>,
    factory: Factory,
    probe: Arc<Mutex<Vec<f32>>>,
    /// What carries the workspace's live session's objects to the calls, while a live session is attached.
    _heard: Option<tokio::task::JoinHandle<()>>,
}

/// Wait until `client` 's endpoint has bound: a call runs on it.
#[allow(
    clippy::future_not_send,
    reason = "a session's client is not Send, and the test runtime does not need it to be"
)]
async fn bound(client: &Client) {
    // The endpoint binds on its own task and over the network, so the bound is the suite's, not a
    // second count.
    let deadline = tilo_test_env::hang_deadline();
    while client.endpoint().is_none() {
        assert!(Instant::now() < deadline, "the session's endpoint binds");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The relay the test's live sessions meet on: in-process, with no one to admit,
/// since a local live session is its own relay's member.
fn relay() -> Arc<Relay> {
    struct Nobody;
    impl RelayAuthority for Nobody {
        fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
            Box::pin(std::future::ready(Err(Denied)))
        }
    }
    Arc::new(Relay::new(Arc::new(Nobody)))
}

/// The workspace scope's content keys, as its own channel hands them to its members.
fn workspace_keys() -> key_group::ContentKeys {
    let channel = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    KeyGroup::create(channel, MemberKey::generate("owner").expect("a member key"))
        .expect("the workspace's channel")
        .content_keys()
        .expect("the workspace's keys")
}

/// `device` 's live session in the test workspace, on `relay` , hearing and heard by `devices` .
fn live_session(
    relay: &Relay,
    device: [u8; 32],
    devices: &[[u8; 32]],
    keys: &key_group::ContentKeys,
) -> Arc<LiveSession> {
    Arc::new(
        LiveSession::local(
            relay,
            &Join {
                scope: Scope::workspace(WORKSPACE.to_owned()),
                device,
                keys: keys.clone().into(),
                roster: LiveRoster::new(devices.iter().copied()),
                publish: true,
                presence: true,
            },
        )
        .expect("a local live session"),
    )
}

fn carrier(client: &Client, keys: &KeyProvider, heard_on: Option<Arc<LiveSession>>) -> Carrier {
    let calls = Calls::new();
    calls.follow(client, None);
    let probe = Arc::new(Mutex::new(Vec::new()));
    let devices = vec![*public(0x11).as_bytes(), *public(0x22).as_bytes()];
    let factory = Factory::new(Sources {
        calls: Arc::clone(&calls),
        endpoint: Some(client.endpoint_handle()),
        keys: Some(keys.clone()),
        roster: Some(Arc::new(move |_workspace: &str| devices.clone())),
        questions: None,
        audio: Audio::Software,
        person: None,
        camera: pictures::Camera::TestPattern,
        played: Some(Arc::clone(&probe)),
    });
    // What the session's own pass does with a live session's objects: hand them to the calls playing that
    // workspace, voice and pictures alike.
    let draining = heard_on.map(|live_session| {
        calls
            .live_sessions
            .lock_recover()
            .insert(WORKSPACE.to_owned(), Arc::clone(&live_session));
        let calls = Arc::clone(&calls);
        tokio::spawn(async move {
            loop {
                while let Some(heard) = live_session.try_recv() {
                    calls.hear(WORKSPACE, heard);
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    });
    Carrier {
        calls,
        factory,
        probe,
        _heard: draining,
    }
}

/// Two of one person's devices in one call, each hearing the other's tone: the devices' sides of
/// it.
struct Pair {
    left: Carrier,
    right: Carrier,
    here: Box<dyn MediaSession>,
    there: Box<dyn MediaSession>,
    /// The channel's keys, kept for as long as the call reads them.
    _keys: tempfile::TempDir,
}

/// One call between two of the person's devices, both heard in it.
#[allow(
    clippy::future_not_send,
    reason = "a session's client is not Send, and the test runtime does not need it to be"
)]
async fn one_call(laptop: &Client, session: &Client) -> Pair {
    laptop.connect(session.endpoint_addr());
    session.connect(laptop.endpoint_addr());
    laptop.follow_desktop(&layout_tree());
    session.follow_desktop(&layout_tree());
    // Each session hears the other say it is in the channel's call.
    for (here, there) in [(laptop, 0x22_u8), (session, 0x11_u8)] {
        here.hear(hello(there, "the other device"));
        presence(here, there, collab_types::PeerPresencePayload::Arrived);
        availability(here, there, Some(call_presence()));
    }
    bound(laptop).await;
    bound(session).await;
    let (keys, kept) = keys(0x11, 0x11, &[0x11, 0x22]);
    founded(&keys).await;
    // Voice runs on the workspace's live session: both devices publish and hear on their own live session in one
    // scope, as they do in a workspace's relay.
    let relay = relay();
    let workspace = workspace_keys();
    let devices = [*public(0x11).as_bytes(), *public(0x22).as_bytes()];
    let left = carrier(
        laptop,
        &keys,
        Some(live_session(&relay, devices[0], &devices, &workspace)),
    );
    let right = carrier(
        session,
        &keys,
        Some(live_session(&relay, devices[1], &devices, &workspace)),
    );
    let mut here = left
        .factory
        .prepare(target())
        .await
        .expect("a call the workspace's keys and transport carry");
    let mut there = right
        .factory
        .prepare(target())
        .await
        .expect("the other device's side of it");
    here.start_capture("tone").expect("the test's tone");
    there.start_capture("tone").expect("the test's tone");
    wait_heard("each device hears the other's tone", &left.probe, true).await;
    wait_heard("the other device hears this one's tone", &right.probe, true).await;
    Pair {
        left,
        right,
        here,
        there,
        _keys: kept,
    }
}

/// One device of a call leaves it and comes back: the device that stayed hears it again within a
/// second of its return, and it hears the one that stayed.
#[allow(
    clippy::future_not_send,
    reason = "a session's client is not Send, and the test runtime does not need it to be"
)]
async fn leaves_and_rejoins() {
    let person = person_key();
    let laptop = device(&person, 0x11, "Laptop");
    let session = device(&person, 0x22, "Session");
    let Pair {
        left,
        right,
        mut here,
        ..
    } = one_call(&laptop, &session).await;
    let (leaving, staying, rejoining) = (&mut here, &right, &left);
    // Heard for a while, the device owns a place in the playout of the one that stays, well past
    // the start of a stream: a stream that starts over would take that long to be heard again.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    wait_heard("the device is heard before it leaves", &staying.probe, true).await;
    // Left, this device is heard by no one, and the call lets it go.
    leaving.close();
    assert_eq!(
        leaving.tick(Instant::now()),
        Err(Issue::SessionEnded),
        "a left call is over"
    );
    wait_heard(
        "a device that left is heard by no one",
        &staying.probe,
        false,
    )
    .await;
    // Back in the call, it is heard again at once: its stream starts at the place its own playhead
    // gives it, not behind where the device that stayed was already playing the stream it lost.
    let mut back = rejoining
        .factory
        .prepare(target())
        .await
        .expect("the device rejoins the call");
    back.start_capture("tone").expect("the test's tone");
    wait_heard_by(
        "a device that rejoined is heard again",
        &staying.probe,
        true,
        within(Duration::from_secs(1)),
    )
    .await;
    wait_heard_by(
        "a device that rejoined hears the call",
        &rejoining.probe,
        true,
        within(Duration::from_secs(1)),
    )
    .await;
    back.close();
}

/// A device that leaves the call and rejoins is heard again at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_leaves_a_call_and_rejoins_is_heard_again_at_once() {
    leaves_and_rejoins().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_devices_in_one_call_hear_each_other_until_a_mute_a_deafen_or_a_leave() {
    let person = person_key();
    let laptop = device(&person, 0x11, "Laptop");
    let session = device(&person, 0x22, "Session");
    let Pair {
        left,
        right,
        mut here,
        mut there,
        ..
    } = one_call(&laptop, &session).await;
    assert_eq!(
        left.calls.callers(CHANNEL),
        vec![*public(0x22).as_bytes()],
        "the other device in the channel is in the call"
    );
    assert_eq!(
        here.tick(Instant::now()),
        Ok(State::Live),
        "a call with the channel's keys is live"
    );

    // The tone has been heard this long before this device is muted: a stream that starts over when
    // it unmutes is behind by every frame it spoke here, which is seconds of silence, not the
    // instant a stream that keeps its place is heard at once.
    assert!(
        level(&left.probe, Duration::from_secs(6)).await > HEARD,
        "the tone speaks until this device mutes"
    );

    // Muted, this device is heard by no one, while it still hears.
    here.stop_capture();
    wait_heard("a muted device is not heard", &right.probe, false).await;
    wait_heard("a muted device still hears", &left.probe, true).await;

    // Unmuted, this device is heard again at once, and deafened it hears nothing while the call
    // plays on.
    here.start_capture("tone").expect("the test's tone");
    wait_heard_by(
        "unmuting is heard again",
        &right.probe,
        true,
        Instant::now() + Duration::from_secs(3),
    )
    .await;
    here.set_deafened(true);
    wait_heard("a deafened device hears nothing", &left.probe, false).await;
    wait_heard("the call still plays for everyone else", &right.probe, true).await;
    here.set_deafened(false);
    wait_heard("undeafening hears it again", &left.probe, true).await;

    // Leaving ends the call here: the other device hears nothing of it.
    here.close();
    assert_eq!(
        here.tick(Instant::now()),
        Err(Issue::SessionEnded),
        "a left call is over"
    );
    wait_heard("a device that left is heard by no one", &right.probe, false).await;
    there.close();
}

/// Two people's devices in one call: each is the person its own certificate names, the other hears
/// it as that person and no one else, and the audio crosses between them the way it does between
/// two devices of one person.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_peoples_devices_in_one_call_hear_each_other() {
    assert_ne!(
        person_signing(0x11).verifying_key().to_bytes(),
        person_signing(0x22).verifying_key().to_bytes(),
        "the two devices are two people's"
    );
    let ana = device(&person_signing(0x11), 0x11, "Ana's laptop");
    let ben = device(&person_signing(0x22), 0x22, "Ben's session");
    let Pair {
        left,
        right,
        mut here,
        mut there,
        ..
    } = one_call(&ana, &ben).await;
    // Each side names the other device by the person it is, and by no other.
    assert_eq!(
        left.calls.persons(CHANNEL).get(public(0x22).as_bytes()),
        Some(&person(0x22)),
        "Ana's side names Ben's device as Ben"
    );
    assert_eq!(
        right.calls.persons(CHANNEL).get(public(0x11).as_bytes()),
        Some(&person(0x11)),
        "Ben's side names Ana's device as Ana"
    );
    wait_heard("Ben hears Ana", &right.probe, true).await;
    wait_heard("Ana hears Ben", &left.probe, true).await;
    // Ana mutes: Ben stops hearing her while she still hears him, as between two of one person's
    // devices.
    here.stop_capture();
    wait_heard("a muted person is not heard", &right.probe, false).await;
    wait_heard("a muted person still hears the other", &left.probe, true).await;
    here.start_capture("tone").expect("the test's tone");
    wait_heard_by(
        "unmuting is heard again",
        &right.probe,
        true,
        within(Duration::from_secs(3)),
    )
    .await;
    here.close();
    wait_heard("a person who left is heard by no one", &right.probe, false).await;
    there.close();
}

/// Ana's `source` as `there` receives it, until `wanted` pictures came: every one the Low layer of
/// her stream for it, returning what they cost, the largest among them, and how long they took.
#[allow(
    clippy::future_not_send,
    reason = "a session's call is not Send, and the test runtime does not need it to be"
)]
async fn receive_pictures(
    there: &mut Box<dyn MediaSession>,
    cursor: &mut u64,
    wanted: usize,
    source: PictureSource,
    of: pictures::Source,
) -> (usize, usize, Duration) {
    let low = pictures::Layer::Low.size(of);
    let started = Instant::now();
    let deadline = tilo_test_env::hang_deadline();
    let mut seen = 0_usize;
    let mut bytes = 0_usize;
    let mut biggest = 0_usize;
    while seen < wanted {
        let (received, next) = there.pictures(*cursor);
        *cursor = next;
        for picture in received {
            assert_eq!(picture.person, person(0x11), "named by the person Ana is");
            assert_eq!(picture.source, source);
            assert_eq!(
                picture.stream,
                hex(public(0x11).as_bytes()),
                "named by the device that sent it"
            );
            let unit = picture.unit().expect("a picture that holds together");
            assert_eq!(
                (unit.width, unit.height),
                low,
                "the layer Ben shows, not the source's own size"
            );
            assert_eq!(
                picture.object.get(3),
                Some(&0),
                "the low layer's own pictures, and no other layer's"
            );
            bytes = bytes.saturating_add(picture.object.len());
            biggest = biggest.max(picture.object.len());
            seen = seen.saturating_add(1);
        }
        assert!(
            Instant::now() < deadline,
            "Ben receives Ana's pictures at the layer he shows"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (
        bytes,
        biggest,
        Instant::now().saturating_duration_since(started),
    )
}

/// Ana's camera, shown by Ben's app in a filmstrip tile: the call encodes the layer he shows and
/// nothing else, so what arrives is the size of that layer and its bitrate, not the camera's 720p
/// source.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_shows_anothers_camera_at_the_one_layer_it_shows() {
    let root = person_key();
    let ana = device(&root, 0x11, "Laptop");
    let ben = device(&root, 0x22, "Session");
    ana.connect(ben.endpoint_addr());
    ben.connect(ana.endpoint_addr());
    ana.follow_desktop(&layout_tree());
    ben.follow_desktop(&layout_tree());
    for (here, there) in [(&ana, 0x22_u8), (&ben, 0x11_u8)] {
        here.hear(hello(there, "the other device"));
        presence(here, there, collab_types::PeerPresencePayload::Arrived);
        availability(here, there, Some(call_presence()));
    }
    bound(&ana).await;
    bound(&ben).await;
    let (keys, _kept) = keys(0x11, 0x11, &[0x11, 0x22]);
    founded(&keys).await;
    let relay = relay();
    let workspace = workspace_keys();
    let devices = [*public(0x11).as_bytes(), *public(0x22).as_bytes()];
    let left = carrier(
        &ana,
        &keys,
        Some(live_session(&relay, devices[0], &devices, &workspace)),
    );
    let right = carrier(
        &ben,
        &keys,
        Some(live_session(&relay, devices[1], &devices, &workspace)),
    );
    assert_eq!(
        left.calls.persons(CHANNEL).get(public(0x22).as_bytes()),
        Some(&person(0x22)),
        "presence names the person Ben's device is"
    );
    let mut here = left
        .factory
        .prepare(target())
        .await
        .expect("Ana's side of the call");
    let mut there = right
        .factory
        .prepare(target())
        .await
        .expect("Ben's side of the call");
    assert_eq!(here.tick(Instant::now()), Ok(State::Live));

    // Ben's app shows Ana's camera in a tile too small for more: Low.
    there.show(vec![Shown {
        person: person(0x11),
        source: PictureSource::Camera,
        layer: PictureLayer::Low,
    }]);
    here.set_camera(true).expect("Ana's camera");

    let mut cursor = 0;
    // One layer's bitrate, not a 720p source's: what arrived over the time it took fits the layer's
    // budget, keyframes and all.
    let (bytes, biggest, elapsed) = receive_pictures(
        &mut there,
        &mut cursor,
        10,
        PictureSource::Camera,
        pictures::Source::Camera,
    )
    .await;
    let budget = usize::try_from(
        u64::from(pictures::Layer::Low.bitrate(pictures::Source::Camera))
            .saturating_mul(elapsed.as_millis().try_into().unwrap_or(0))
            / 8_000,
    )
    .unwrap_or(usize::MAX);
    assert!(
        bytes <= budget.saturating_mul(4) + 64 * 1024,
        "the pictures cost the layer's bitrate, not the 720p source's: {bytes} bytes in {elapsed:?}"
    );
    assert!(
        biggest <= 1280 * 720 * 3 / 2 / 16,
        "no picture carries a 720p frame: {biggest} bytes at most"
    );

    // Ana turns her camera off: Ben receives nothing more.
    here.set_camera(false).expect("the camera stops");
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Whatever was in flight is taken and dropped: nothing follows it.
    let (in_flight, next) = there.pictures(cursor);
    cursor = next;
    drop(in_flight);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (later, _) = there.pictures(cursor);
    assert!(
        later.is_empty(),
        "no picture after the camera stopped: {} arrived",
        later.len()
    );
    here.close();
    there.close();
}

/// Draw the shell into `share` as the session's pass does, until the test stops it: the call takes
/// each frame the share holds and sends it.
fn drawing(share: &'static Arc<ScreenShare>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            hand_screen(share, false, || {
                Ok(image::RgbaImage::from_pixel(
                    640,
                    360,
                    image::Rgba([20, 120, 200, 255]),
                ))
            });
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
}

/// Ana's shared desktop reaches Ben's device over the live session at the layer his app shows, and a
/// locked desktop sends nothing at all until it is unlocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_screen_reaches_the_other_device_and_a_lock_silences_it() {
    let root = person_key();
    let ana = device(&root, 0x11, "Laptop");
    let ben = device(&root, 0x22, "Session");
    ana.connect(ben.endpoint_addr());
    ben.connect(ana.endpoint_addr());
    ana.follow_desktop(&layout_tree());
    ben.follow_desktop(&layout_tree());
    for (here, there) in [(&ana, 0x22_u8), (&ben, 0x11_u8)] {
        here.hear(hello(there, "the other device"));
        presence(here, there, collab_types::PeerPresencePayload::Arrived);
        availability_showing(here, there, call_presence(), false, true);
    }
    bound(&ana).await;
    bound(&ben).await;
    let (keys, _kept) = keys(0x11, 0x11, &[0x11, 0x22]);
    founded(&keys).await;
    let relay = relay();
    let workspace = workspace_keys();
    let devices = [*public(0x11).as_bytes(), *public(0x22).as_bytes()];
    let left = carrier(
        &ana,
        &keys,
        Some(live_session(&relay, devices[0], &devices, &workspace)),
    );
    let right = carrier(
        &ben,
        &keys,
        Some(live_session(&relay, devices[1], &devices, &workspace)),
    );
    let mut here = left
        .factory
        .prepare(target())
        .await
        .expect("Ana's side of the call");
    let mut there = right
        .factory
        .prepare(target())
        .await
        .expect("Ben's side of the call");
    assert_eq!(here.tick(Instant::now()), Ok(State::Live));

    // Ben's app shows the shared desktop as a preview: Low.
    there.show(vec![Shown {
        person: person(0x11),
        source: PictureSource::Screen,
        layer: PictureLayer::Low,
    }]);
    here.set_screen(true).expect("Ana shares her desktop");
    let share = screen_share();
    let feeding = drawing(share);
    let mut cursor = 0;
    let (bytes, biggest, elapsed) = receive_pictures(
        &mut there,
        &mut cursor,
        4,
        PictureSource::Screen,
        pictures::Source::Screen,
    )
    .await;
    assert!(bytes > 0, "the shared desktop arrived over {elapsed:?}");
    assert!(
        biggest <= 1920 * 1080 * 3 / 2 / 16,
        "no picture carries a full-size screen frame: {biggest} bytes at most"
    );
    feeding.abort();

    // Locked, the desktop draws nothing and sends nothing: not the frame the share held, and none
    // after it.
    hand_screen(share, true, || panic!("a locked desktop is not drawn"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Whatever was in flight is taken and dropped: nothing follows it.
    let (in_flight, next) = there.pictures(cursor);
    cursor = next;
    drop(in_flight);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (later, next) = there.pictures(cursor);
    cursor = next;
    assert!(
        later.is_empty(),
        "a locked desktop is not shared: {} pictures arrived",
        later.len()
    );

    // Unlocked, the pass draws again and Ben sees the desktop again.
    let feeding = drawing(share);
    let (bytes, _, _) = receive_pictures(
        &mut there,
        &mut cursor,
        2,
        PictureSource::Screen,
        pictures::Source::Screen,
    )
    .await;
    assert!(bytes > 0, "an unlocked desktop shares again");
    feeding.abort();
    here.close();
    there.close();
}

/// A locked desktop's share stops at once: the frame the pass drew before the lock is never handed
/// on, and a locked pass draws nothing new.
#[test]
fn a_locked_screen_stops_the_share_at_once() {
    let share = ScreenShare::new();
    share.set_sharing(true);
    let drawn = || {
        Ok(image::RgbaImage::from_pixel(
            640,
            360,
            image::Rgba([200, 40, 40, 255]),
        ))
    };
    hand_screen(&share, false, drawn);
    assert!(
        share.take().is_some(),
        "an unlocked desktop hands the pass's frame on"
    );
    hand_screen(&share, false, drawn);
    assert!(
        !share.wants_frame(Instant::now()),
        "a frame this recent is not drawn again"
    );
    assert!(share.take().is_some(), "the frame the pass just drew");
    hand_screen(&share, false, drawn);
    assert!(share.take().is_some(), "a frame worth drawing arrives");
    // The screen locks: the frame the pass drew last is forgotten, and a locked pass hands on
    // nothing.
    hand_screen(&share, true, || panic!("a locked desktop is not drawn"));
    assert!(share.withheld(), "the share withholds what it held");
    assert!(
        share.take().is_none(),
        "nothing of the locked screen remains"
    );
    hand_screen(&share, true, || panic!("a locked desktop is not drawn"));
    assert!(share.take().is_none(), "a locked desktop shares nothing");
    // Unlocked again, the pass draws a fresh frame and the share goes on.
    share.publish_rgba(320, 180, vec![0; 320 * 180 * 4]);
    assert!(!share.withheld(), "an unlocked desktop shares again");
    assert!(share.take().is_some(), "the frame drawn after unlocking");
    share.set_sharing(false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_joins_while_the_workspace_keys_are_still_on_their_way() {
    let person = person_key();
    // This session's device is not the one its workspace is governed by, and its channel's keys have
    // not reached it yet.
    let laptop = device(&person, 0x22, "Laptop");
    let session = device(&person, 0x11, "Session");
    laptop.connect(session.endpoint_addr());
    session.connect(laptop.endpoint_addr());
    laptop.follow_desktop(&layout_tree());
    session.follow_desktop(&layout_tree());
    laptop.hear(hello(0x11, "the other device"));
    presence(&laptop, 0x11, collab_types::PeerPresencePayload::Arrived);
    availability(&laptop, 0x11, Some(call_presence()));
    bound(&laptop).await;
    let (keys, _kept) = keys(0x22, 0x11, &[0x11, 0x22]);
    let left = carrier(&laptop, &keys, None);
    let mut here = left
        .factory
        .prepare(target())
        .await
        .expect("a call whose channel's keys are on their way");
    assert_eq!(
        here.tick(Instant::now()),
        Ok(State::Joining),
        "a call waits for the channel's keys instead of ending"
    );
    here.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the session lives until the call is over, and ends in a blocking context"
)]
async fn a_session_that_cannot_carry_a_call_ends_it_and_says_why() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let sync = std::rc::Rc::new(Client::untethered(
        public(0xf0),
        OwnPresence {
            label: "Quinten".to_owned(),
            user: Some(person(0xf0)),
            ..OwnPresence::default()
        },
    ));
    let mut session = session(&sync, &socket);
    step(&mut session);
    let media = session
        .media_service()
        .expect("the session hands its apps a media service")
        .clone();

    // Chat joins a voice channel through that service, as its people's call action does. This
    // session carries no transport, so the call cannot be carried: it says so rather than
    // pretending to be live.
    let chat =
        tilo_native_media::Caller::connected(7, APP, "the-workspace", CHANNEL, Arc::new(|| Ok(())));
    let joined = media
        .command_for(
            chat.clone(),
            Request {
                version: 1,
                intent: 1,
                command: Command::Join {
                    channel_id: CHANNEL.to_owned(),
                },
            },
        )
        .expect("an app joins a voice channel");
    assert_eq!(
        joined.state,
        State::Joining,
        "a joined call waits for its carrier"
    );
    step(&mut session);
    assert!(
        sync.own().call.is_none(),
        "a session with no transport says no call in this person's presence"
    );
    let ended = media
        .command_for(
            chat,
            Request {
                version: 1,
                intent: 0,
                command: Command::Status,
            },
        )
        .expect("the app reads its call");
    assert_eq!(ended.state, State::Ended);
    assert_eq!(ended.issue, Some(Issue::Unavailable));

    // Another person's device still reads this person in a call when this person's presence says
    // so: the presence is the whole of it.
    let peer = Client::untethered(public(0xb0), OwnPresence::default());
    peer.follow_desktop(&layout_tree());
    peer.hear(hello(0xf0, "Framework 16"));
    presence(
        &peer,
        0xf0,
        collab_types::PeerPresencePayload::Person {
            name: "Quinten".to_owned(),
        },
    );
    presence(&peer, 0xf0, collab_types::PeerPresencePayload::Arrived);
    availability(&peer, 0xf0, Some(call_presence()));
    let calls = peer.calls();
    let heard = calls
        .iter()
        .find(|call| call.channel == CHANNEL)
        .expect("the peer reads the call this person is in");
    assert_eq!(heard.workspace, WORKSPACE);
    assert_eq!(
        heard
            .people
            .iter()
            .map(|caller| (caller.user.clone(), caller.label.clone()))
            .collect::<Vec<_>>(),
        [(Some(person(0xf0)), "Quinten".to_owned())],
        "the person in the call is this session's person"
    );
    // The session keeps a runtime of its own, which blocking drop ends.
    tokio::task::block_in_place(|| drop(session));
}

/// A channel's roster says who in it has a camera or a screen on, as the presence of their devices
/// says: theirs and this device's own alike.
#[test]
fn a_channels_roster_says_who_has_a_camera_or_screen_on() {
    let mine = Client::untethered(
        public(0xf0),
        OwnPresence {
            user: Some(person(0xf0)),
            ..OwnPresence::default()
        },
    );
    mine.follow_desktop(&layout_tree());
    // Two people in the channel, sending nothing: in it, with neither on.
    for (seed, label) in [(0xaa, "Framework 16"), (0xbb, "ThinkPad")] {
        mine.hear(hello(seed, label));
        presence(&mine, seed, collab_types::PeerPresencePayload::Arrived);
        availability(&mine, seed, Some(call_presence()));
    }

    let calls = Calls::new();
    calls.follow(&mine, None);
    let roster = |calls: &Calls| {
        calls
            .rosters()
            .into_iter()
            .find(|roster| roster.channel_id == CHANNEL)
            .expect("the channel the peers are in")
    };
    // A roster lists people by the id everyone here knows them by.
    let heard = roster(&calls);
    assert_eq!(heard.people, [person(0xbb), person(0xaa)]);
    assert!(heard.cameras.is_empty(), "{heard:?}");
    assert!(heard.screens.is_empty(), "{heard:?}");

    // Their camera on, their screen shared: the roster says so.
    availability_showing(&mine, 0xaa, call_presence(), true, true);
    calls.follow(&mine, None);
    let heard = roster(&calls);
    assert_eq!(heard.cameras, [person(0xaa)]);
    assert_eq!(heard.screens, [person(0xaa)]);

    // This device's own camera is in the roster its apps read too,
    // though nobody here hears it from this session.
    let in_call = sync::InCall {
        call: ui::CallPresence {
            channel: CHANNEL.to_owned(),
            name: "Standup".to_owned(),
            bridge: None,
        },
        camera: true,
        screen: false,
        muted: false,
        deafened: false,
    };
    calls.follow(&mine, Some(&in_call));
    let heard = roster(&calls);
    assert_eq!(heard.people, [person(0xf0), person(0xbb), person(0xaa)]);
    assert_eq!(heard.cameras, [person(0xf0), person(0xaa)]);
    assert_eq!(heard.screens, [person(0xaa)]);
    assert!(heard.muted.is_empty(), "{heard:?}");

    // Someone mutes, and this device deafens: everyone's roster says it.
    let muted = sync::InCall {
        muted: true,
        ..in_call.clone()
    };
    availability_showing(&mine, 0xbb, muted.encode(), false, false);
    let deafened = sync::InCall {
        deafened: true,
        ..in_call
    };
    calls.follow(&mine, Some(&deafened));
    let heard = roster(&calls);
    assert_eq!(heard.muted, [person(0xf0), person(0xbb)]);
    assert_eq!(heard.deafened, [person(0xf0)]);
}

// ---------------------------------------------------------------------------
// The pictures a call makes of what its devices show.
// ---------------------------------------------------------------------------

/// Whether this machine can encode the pictures a call sends at all.
fn has_h264_encoder() -> bool {
    media::realtime::local_capabilities().can_encode(media::realtime::StreamCodec::H264)
}

/// One frame a capture hands the encoder: `width` × `height`, filled with `fill`.
fn captured(width: u32, height: u32, fill: u8) -> pictures::Frame {
    let pixels = usize::try_from(width)
        .unwrap_or(0)
        .saturating_mul(usize::try_from(height).unwrap_or(0))
        .saturating_mul(4);
    pictures::Frame {
        pixels: Arc::from(vec![fill; pixels]),
        width,
        height,
        format: media::realtime::InputFormat::Rgba,
        captured_at: Instant::now(),
    }
}

/// What the encoder thread sent, as the call's live session would have told it to: the layers the
/// call's devices show, the sources to start over, and every picture it published.
#[derive(Default)]
struct Witness {
    asking: Mutex<pictures::Asking>,
    sent: Mutex<Vec<pictures::VideoObject>>,
    own: Mutex<Vec<pictures::VideoObject>>,
}

impl pictures::Sink for Witness {
    fn asking(&self, _source: pictures::Source) -> pictures::Asking {
        self.asking.lock_recover().clone()
    }

    fn sent(&self, _source: pictures::Source, object: &pictures::VideoObject) {
        self.sent.lock_recover().push(object.clone());
    }

    fn own(&self, object: &pictures::VideoObject) {
        self.own.lock_recover().push(object.clone());
    }
}

/// Wait until `count` pictures were sent, or fail saying what was waited for.
async fn sent(witness: &Arc<Witness>, count: usize, what: &str) -> Vec<pictures::VideoObject> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let sent = witness.sent.lock_recover().clone();
        if sent.len() >= count {
            return sent;
        }
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A layer keeps its source's shape inside its box, never grows it, and is even: what 4:2:0
/// encoders and every tile layout read.
#[test]
fn a_picture_keeps_its_sources_shape_within_its_layers_box() {
    assert_eq!(pictures::fitted((1280, 720), (320, 180)), (320, 180));
    assert_eq!(pictures::fitted((1920, 1200), (1920, 1080)), (1728, 1080));
    assert_eq!(pictures::fitted((2560, 1080), (640, 360)), (640, 270));
    assert_eq!(
        pictures::fitted((800, 600), (1920, 1080)),
        (800, 600),
        "never larger than the source"
    );
    assert_eq!(pictures::fitted((1366, 768), (320, 180)), (320, 178));
}

/// A screen of an odd size is cropped to the even rows and columns the encoder takes, and one too
/// small to crop is not encoded at all.
#[test]
fn an_odd_screen_is_cropped_to_even_rows_and_columns() {
    let rgba: Arc<[u8]> = Arc::from(vec![9; 5 * 3 * 4]);
    let frame = pictures::even_rgba(5, 3, rgba).expect("a frame the encoder takes");
    assert_eq!((frame.width, frame.height), (4, 2));
    assert_eq!(frame.pixels.len(), 4 * 2 * 4);
    assert!(pictures::even_rgba(1, 1, Arc::from(vec![0; 4])).is_none());
    assert!(pictures::even_rgba(4, 2, Arc::from(vec![0; 3])).is_none());
}

/// A still screen is encoded once: no frame arrives, so nothing more is sent while nobody new
/// shows it. A device that starts showing it is answered with the keyframe the encoder stands on
/// — the same picture — not a new encode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_still_screen_is_encoded_once_and_a_new_reader_starts_at_its_standing_keyframe() {
    if !has_h264_encoder() {
        eprintln!("no H.264 encoder on this machine: nothing could encode the pictures");
        return;
    }
    let witness = Arc::new(Witness::default());
    witness.asking.lock_recover().wanted.insert(
        pictures::Source::Screen,
        [pictures::Layer::Low].into_iter().collect(),
    );
    let sink: Arc<dyn pictures::Sink> = witness.clone();
    let sending = pictures::Sending::start(pictures::Source::Screen, pictures::SCREEN_FPS, &sink)
        .expect("a screen encoder");
    // One frame, which is all a still screen draws.
    sending.publish(captured(640, 360, 7));
    let first = sent(&witness, 1, "the still screen's one picture").await;
    assert_eq!(first.len(), 1, "a still screen is encoded once");
    assert!(first[0].keyframe, "a reader starts at a keyframe");
    assert_eq!(
        (first[0].width, first[0].height),
        pictures::Layer::Low.size(pictures::Source::Screen),
        "the layer the reader shows, not the screen's own size"
    );

    // No frame arrives and nobody new shows it: nothing more is encoded or sent.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        witness.sent.lock_recover().len(),
        1,
        "a still screen sends nothing while nobody new shows it"
    );

    // A device starts showing the screen afresh: the call asks for a keyframe and the encoder
    // answers with the picture it stands on, which costs no encode.
    witness
        .asking
        .lock_recover()
        .restart
        .insert(pictures::Source::Screen);
    sending.ask_keyframe();
    let again = sent(&witness, 2, "the standing keyframe for a new reader").await;
    assert_eq!(again.len(), 2, "the new reader is answered once");
    assert_eq!(
        again[1].sequence, first[0].sequence,
        "the picture the encoder stands on, not a new one"
    );
    assert!(again[1].keyframe);
    assert_eq!(again[1].layer, pictures::Layer::Low);
}

/// A received stream that falls behind starts over at its next keyframe: the pictures that would
/// not decode without the one dropped are not handed to the app.
#[test]
fn a_stream_that_falls_behind_starts_over_at_its_next_keyframe() {
    let mut inbox = pictures::Inbox::default();
    let big = pictures::INBOX_BYTES / 4;
    let slow = [7; 32];
    let fast = [8; 32];
    let picture_of = |from: [u8; 32], size: usize, keyframe: bool| pictures::Picture {
        from,
        source: pictures::Source::Camera,
        keyframe,
        object: vec![1; size],
    };
    inbox.admit(picture_of(slow, big, true));
    inbox.admit(picture_of(slow, big, false));
    inbox.admit(picture_of(fast, big, true));
    inbox.admit(picture_of(fast, big, false));
    // One more: the oldest stream gives way whole.
    inbox.admit(picture_of(fast, big, false));
    let (queued, cursor) = inbox.take(0, usize::MAX);
    assert!(queued.iter().all(|picture| picture.from == fast));
    inbox.take(cursor, usize::MAX);
    inbox.admit(picture_of(slow, 10, false));
    assert!(
        inbox.take(cursor, usize::MAX).0.is_empty(),
        "not without its keyframe"
    );
    inbox.admit(picture_of(slow, 10, true));
    inbox.admit(picture_of(slow, 10, false));
    assert_eq!(inbox.take(cursor, usize::MAX).0.len(), 2);
}
