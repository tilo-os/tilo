//! A call on the native desk, in two shapes: an app joins a voice channel
//! on a session that can carry one, and the person and the people in the
//! workspace hear each other; and a session with no transport ends the
//! call it cannot carry, saying why.
//!
//! The audio here is the call's own: the same encoder, plane, sealing and
//! playout a device's call runs, ending in memory instead of a speaker,
//! and a tone instead of a microphone.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::platform::fabric::Continuity;
use crate::sync::Event;
use channel_crypto::{ChannelGroup, MemberKey, VaultKey};
use collab_types::presence::{Availability, Payload, Presence};
use collab_types::{ChannelRoute, Ed25519PublicKey};
use iroh::EndpointId;
use runtime_collab::native_media::{Command, Request, State};
use tilo_broker_core::collab_core::BrokerChannels;
use tilo_channels::{
    ChannelAccess, ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
    ChannelKeyProvider, GovernanceTransport, InboundChannelGovernance,
};
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_network_actor::{HostIdentity, workspace_topic_id};

use super::*;
use crate::sync::{Client, Entry, OwnPresence};
use crate::{ActionPlane, Options, Session};
use tilo_live::plane::Join;
use tilo_live::relay::{Denied, Peer, Relay, RelayAuthority, SessionAuthorization};
use tilo_live::roster::Roster as LiveRoster;
use tilo_live::scope::Scope;

const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
const CHANNEL: &str = "c_standup";
const APP: &str = "so.tilo.chat";
const RECORD: &str = "chat";
/// What the tone's playout reads as heard, against the silence a call plays while nobody speaks.
const HEARD: f32 = 0.02;

fn public(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
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
    Ed25519PublicKey::from_bytes(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

/// The person all of a call's devices are devices of.
fn person_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[21; 32])
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

/// A desk showing one workspace, as this device holds it.
fn desk_tree() -> DesktopTree {
    let workspace_id = workspace_id();
    let workspace = Workspace::in_workspace(
        WorkspaceSlot::of_workspace(workspace_id),
        workspace_id,
        "Desk",
    );
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// A directory for the plane's socket that only this user reaches.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    root
}

/// One of the person's devices, on its own loopback endpoint, taking part in the collaboration
/// plane as its certified self.
fn device(person: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> Client {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, person).expect("the person signs their device");
    Client::start(
        Entry {
            identity: Some(HostIdentity::kept(cert, key)),
            own: OwnPresence::default(),
            hosts: Vec::new(),
            continuity: Continuity::default(),
            wake: Arc::new(|| {}),
        },
        None,
        None,
        Vec::new(),
        None,
    )
    .expect("a loopback endpoint binds")
}

/// The workspace as this device knows its governance: both of a call's devices may read its
/// channels.
struct Directory {
    admitted: std::collections::BTreeSet<Ed25519PublicKey>,
}

impl ChannelDirectory for Directory {
    fn allowed_devices(
        &self,
        _workspace: WorkspaceId,
        _channel: collab_types::ChannelId,
        _route: Option<&ChannelRoute>,
        _now: u64,
    ) -> Option<std::collections::BTreeSet<Ed25519PublicKey>> {
        Some(self.admitted.clone())
    }
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        _: u64,
    ) -> Option<collab_types::ChannelId> {
        Some(route.channel(&workspace.to_string()))
    }
}

/// One channel's keys, as the device `device` of the workspace whose vault key is `vault` runs
/// holds them: the same service every device runs, with the channel's keys still on their way until
/// someone founds them.
fn keys(device: u8, vault: u8, admitted: &[u8]) -> (ChannelKeyProvider, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("a directory to keep channel state in");
    let gates = Arc::new(tilo_broker_core::collab_core::BrokerGates(Arc::new(
        Mutex::new(BrokerChannels::default()),
    )));
    let encryption = ChannelEncryption::open(
        ChannelEncryptionConfig {
            state: Arc::new(tilo_channels::ChannelsDir::new(
                directory.path().join("channels"),
            )),
            vault: VaultKey::new([vault; 32]),
            device_key: ed25519_dalek::SigningKey::from_bytes(&[device; 32]),
        },
        Box::new(Directory {
            admitted: admitted.iter().copied().map(key).collect(),
        }),
        gates,
    )
    .expect("a channel service");
    let provider = ChannelKeyProvider::default();
    provider.set(Some(ChannelKeyAccess::new(encryption)));
    (provider, directory)
}

/// The channel a call on `target` runs on: the workspace's record of it.
fn call_channel() -> collab_types::ChannelId {
    collab_types::document_channel_id(APP, WORKSPACE, RECORD)
}

/// Found the call's channel, as the pass a session runs does: the first device that takes part
/// creates the group once its wait is due, and the test's workspace has no other participant to
/// ask.
#[allow(
    clippy::future_not_send,
    reason = "the channel service is driven from the test's own thread, and the test runtime does not need it to be Send"
)]
async fn founded(keys: &ChannelKeyProvider) {
    struct Nowhere;
    impl GovernanceTransport for Nowhere {
        fn publish(&self, _message: neighbour_wire::NeighbourMsg) -> bool {
            false
        }
        fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
            Vec::new()
        }
    }
    let access = keys.get().expect("this device's channel service");
    // Reading first is what makes the device take part, as a call does.
    access.access(workspace_id(), call_channel());
    let deadline = tilo_test_env::hang_deadline();
    loop {
        access.pump(&Nowhere);
        if matches!(
            access.access(workspace_id(), call_channel()).0,
            ChannelAccess::Ready(_)
        ) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the call's channel is founded once a device's wait is due"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A session taking part in the workspace through `sync` , showing the desk.
fn session(sync: &std::rc::Rc<Client>, socket: &std::path::Path) -> Session {
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            sync: Some(std::rc::Rc::clone(sync)),
            action_plane: ActionPlane::At(socket.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    session.handle().replace_tree(desk_tree());
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
        ws: workspace_topic_id(workspace_id()),
        peer: public(from),
        msg: Box::new(msg),
    }
}

/// A device's identity: the person it is a device of, and its name here.
fn hello(from: u8, label: &str) -> Event {
    let device_key = public(from);
    let root = ed25519_dalek::SigningKey::from_bytes(&[from; 32]);
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

/// One presence `from` said of themselves on the workspace's live plane.
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
    /// What carries the workspace's plane's objects to the calls, while a plane is attached.
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

/// The relay the test's planes meet on: in-process, with no one to admit,
/// since a local plane is its own relay's participant.
fn relay() -> Arc<Relay> {
    struct Nobody;
    impl RelayAuthority for Nobody {
        fn admit(
            &self,
            _peer: Peer,
            _proof: &[u8],
        ) -> Result<Arc<dyn SessionAuthorization>, Denied> {
            Err(Denied)
        }
    }
    Arc::new(Relay::new(Arc::new(Nobody)))
}

/// The workspace scope's content keys, as its own channel hands them to its members.
fn workspace_keys() -> channel_crypto::ChannelContentKeys {
    let channel = Scope::workspace(WORKSPACE.to_owned()).channel_id();
    ChannelGroup::create(channel, MemberKey::generate("owner").expect("a member key"))
        .expect("the workspace's channel")
        .content_keys()
        .expect("the workspace's keys")
}

/// `device` 's plane in the test workspace, on `relay` , hearing and heard by `devices` .
fn plane(
    relay: &Relay,
    device: [u8; 32],
    devices: &[[u8; 32]],
    keys: &channel_crypto::ChannelContentKeys,
) -> Arc<Plane> {
    Arc::new(
        Plane::local(
            relay,
            &Join {
                scope: Scope::workspace(WORKSPACE.to_owned()),
                device,
                keys: keys.clone(),
                roster: LiveRoster::new(devices.iter().copied()),
                publish: true,
            },
        )
        .expect("a local plane"),
    )
}

fn carrier(client: &Client, keys: &ChannelKeyProvider, heard_on: Option<Arc<Plane>>) -> Carrier {
    let calls = Calls::new();
    calls.follow(client, None);
    let probe = Arc::new(Mutex::new(Vec::new()));
    let devices = vec![*public(0x11).as_bytes(), *public(0x22).as_bytes()];
    let factory = Factory::new(Sources {
        calls: Arc::clone(&calls),
        endpoint: Some(client.endpoint_handle()),
        keys: Some(keys.clone()),
        roster: Some(Arc::new(move |_room: &str| devices.clone())),
        questions: None,
        audio: Audio::Software,
        person: None,
        camera: pictures::Camera::TestPattern,
        played: Some(Arc::clone(&probe)),
    });
    // What the session's own pass does with a plane's objects: hand them to the calls playing that
    // workspace, voice and pictures alike.
    let draining = heard_on.map(|plane| {
        calls
            .planes
            .lock_recover()
            .insert(WORKSPACE.to_owned(), Arc::clone(&plane));
        let calls = Arc::clone(&calls);
        tokio::spawn(async move {
            loop {
                while let Some(heard) = plane.try_recv() {
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
async fn one_call(laptop: &Client, desk: &Client) -> Pair {
    laptop.connect(desk.endpoint_addr());
    desk.connect(laptop.endpoint_addr());
    laptop.follow_desktop(&desk_tree());
    desk.follow_desktop(&desk_tree());
    // Each session hears the other say it is in the channel's call.
    for (here, there) in [(laptop, 0x22_u8), (desk, 0x11_u8)] {
        here.hear(hello(there, "the other device"));
        presence(here, there, collab_types::PeerPresencePayload::Arrived);
        availability(here, there, Some(call_presence()));
    }
    bound(laptop).await;
    bound(desk).await;
    let (keys, kept) = keys(0x11, 0x11, &[0x11, 0x22]);
    founded(&keys).await;
    // Voice runs on the workspace's plane: both devices publish and hear on their own plane in one
    // scope, as they do in a workspace's relay.
    let relay = relay();
    let workspace = workspace_keys();
    let devices = [*public(0x11).as_bytes(), *public(0x22).as_bytes()];
    let left = carrier(
        laptop,
        &keys,
        Some(plane(&relay, devices[0], &devices, &workspace)),
    );
    let right = carrier(
        desk,
        &keys,
        Some(plane(&relay, devices[1], &devices, &workspace)),
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
    let desk = device(&person, 0x22, "Desk");
    let Pair {
        left,
        right,
        mut here,
        ..
    } = one_call(&laptop, &desk).await;
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
    let desk = device(&person, 0x22, "Desk");
    let Pair {
        left,
        right,
        mut here,
        mut there,
        ..
    } = one_call(&laptop, &desk).await;
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
    let ben = device(&root, 0x22, "Desk");
    ana.connect(ben.endpoint_addr());
    ben.connect(ana.endpoint_addr());
    ana.follow_desktop(&desk_tree());
    ben.follow_desktop(&desk_tree());
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
        Some(plane(&relay, devices[0], &devices, &workspace)),
    );
    let right = carrier(
        &ben,
        &keys,
        Some(plane(&relay, devices[1], &devices, &workspace)),
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

/// Draw the desk into `share` as the session's pass does, until the test stops it: the call takes
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

/// Ana's shared desktop reaches Ben's device over the plane at the layer his app shows, and a
/// locked desktop sends nothing at all until it is unlocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_screen_reaches_the_other_device_and_a_lock_silences_it() {
    let root = person_key();
    let ana = device(&root, 0x11, "Laptop");
    let ben = device(&root, 0x22, "Desk");
    ana.connect(ben.endpoint_addr());
    ben.connect(ana.endpoint_addr());
    ana.follow_desktop(&desk_tree());
    ben.follow_desktop(&desk_tree());
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
        Some(plane(&relay, devices[0], &devices, &workspace)),
    );
    let right = carrier(
        &ben,
        &keys,
        Some(plane(&relay, devices[1], &devices, &workspace)),
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
async fn a_call_joins_while_the_channels_keys_are_still_on_their_way() {
    let person = person_key();
    // This desk's device is not the one its workspace is governed by, and its channel's keys have
    // not reached it yet.
    let laptop = device(&person, 0x22, "Laptop");
    let desk = device(&person, 0x11, "Desk");
    laptop.connect(desk.endpoint_addr());
    desk.connect(laptop.endpoint_addr());
    laptop.follow_desktop(&desk_tree());
    desk.follow_desktop(&desk_tree());
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
        .expect("the desk hands its apps a media service")
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
    peer.follow_desktop(&desk_tree());
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
    mine.follow_desktop(&desk_tree());
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
        },
        camera: true,
        screen: false,
    };
    calls.follow(&mine, Some(&in_call));
    let heard = roster(&calls);
    assert_eq!(heard.people, [person(0xf0), person(0xbb), person(0xaa)]);
    assert_eq!(heard.cameras, [person(0xf0), person(0xaa)]);
    assert_eq!(heard.screens, [person(0xaa)]);
}
