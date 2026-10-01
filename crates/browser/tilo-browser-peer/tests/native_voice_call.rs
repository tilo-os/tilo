//! A call between the owner's native device and an invited person's
//! browser in a room the relay governs. The native side is the shell's own
//! path: the joined-room owner holds the room, its relay session carries the
//! call, and a [`Participation`] starts the call, keeps the governor's grant
//! and authorizes its voice carrier. Both get their grants from the relay
//! and hear each other over its `moq-lite` carrier.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::HttpsLink;
use tilo_desktop_model::RoomId;
use tilo_identity_host::custody::HostIdentity;
use tilo_joined_room::NativeWorkspaceOwner;
use tilo_joined_room::session::{Connect, Scope};
use tilo_joined_room::voice::{Channel, Participation};
use tilo_moq::client::{
    CapturedOpus, Client, ClientAuthority, Connection, ConnectionScope, Status,
};
use tilo_moq::relay::Direction;
use tilo_peer_state::join::{self, MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use tilo_relay::RelayConfig;
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::rooms::{RoomAdminClient, RoomCommand, RoomReply};
use tilo_relay::web::{TlsSource, WebConfig};
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::VoiceGrant;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use support::{enrol, wait_for};

const CHAT: &str = "so.tilo.chat";
const CHANNEL: &str = "standup";

async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// The browser's grant is its whole authority; it checks nothing more.
struct Granted;
impl ClientAuthority for Granted {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
}

async fn until(what: &str, client: &Client, done: fn(Status) -> bool) {
    wait_for(what, || {
        let reached = done(client.status());
        async move { reached.then_some(()) }
    })
    .await;
}

/// Speak on `speaker` until `listener` hears it from `namespace`.
async fn hears(what: &str, speaker: &Client, listener: &Client, namespace: &str) {
    let mut timestamp = 0;
    wait_for(what, || {
        let _ = speaker.try_send(CapturedOpus {
            timestamp_48k: timestamp,
            duration_samples: tilo_moq::voice::FRAME_SAMPLES,
            payload: vec![0xf8, 0xff, 0xfe],
            captured_at: Instant::now(),
        });
        timestamp = timestamp.saturating_add(u64::from(tilo_moq::voice::FRAME_SAMPLES));
        let mut heard = false;
        while let Some(frame) = listener.try_recv() {
            heard |= frame.publisher.namespace == namespace;
        }
        async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            heard.then_some(())
        }
    })
    .await;
}

/// The next envelope from the browser session that `pick` accepts.
async fn browser<T>(link: &mut HttpsLink, mut pick: impl FnMut(Envelope) -> Option<T>) -> T {
    tilo_test_env::within("the host answers the browser", async {
        loop {
            if let Some(value) = pick(link.recv().await.expect("session open")) {
                return value;
            }
        }
    })
    .await
}

fn granted(envelope: Envelope) -> Option<VoiceGrant> {
    match envelope {
        Envelope::VoiceGranted { grant, .. } => Some(*grant),
        Envelope::VoiceRefused { reason, .. } => panic!("refused: {reason}"),
        _ => None,
    }
}

/// A person's native device holding a room through the joined-room owner,
/// as the shell does, with its call participations.
struct Native {
    identity: HostIdentity,
    endpoint: iroh::Endpoint,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    workspace_id: u64,
    scope: Arc<Scope>,
    session: RelayCoeditSession,
    calls: Vec<Arc<Participation>>,
}

impl Native {
    async fn join(
        directory: &std::path::Path,
        cert: DeviceCert,
        key: SigningKey,
        invitation: &str,
        workspace_id: u64,
    ) -> Self {
        let owner = tilo_joined_room::open_file(&directory.join("joined-rooms.json")).unwrap();
        let identity = HostIdentity {
            device_cert: cert,
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::default(),
            workspace_revocations: HashMap::default(),
            identity_paths: None,
        };
        let endpoint = endpoint(&key).await;
        let enrollment = tilo_joined_room::enroll::prepare_and_complete(
            WorkspaceJoinClient::new(endpoint.clone()),
            identity.clone(),
            workspace_id,
            None,
            invitation.to_owned(),
            owner.clone(),
        )
        .await
        .unwrap();
        owner.install(&enrollment, SystemTime::now()).unwrap();
        let room = RoomId::parse(&enrollment.bootstrap().room).unwrap();
        let (scope, session) = Self::dial(&owner, room, &identity, &endpoint, workspace_id).await;
        Self {
            identity,
            endpoint,
            owner,
            room,
            workspace_id,
            scope,
            session,
            calls: Vec::new(),
        }
    }

    /// Connect to the relay the room's current pin names, as the shell's
    /// actor does whenever that pin changes.
    async fn dial(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        identity: &HostIdentity,
        endpoint: &iroh::Endpoint,
        workspace_id: u64,
    ) -> (Arc<Scope>, RelayCoeditSession) {
        let connect = Connect::new(owner.clone(), room).unwrap();
        let (hint, device, revocations, continuity) = connect.material(identity).unwrap();
        let session = tilo_joined_room::session::dial(
            endpoint.clone(),
            workspace_id,
            &hint,
            device.clone(),
            identity.device_signing_key.clone(),
            revocations,
        )
        .await
        .unwrap();
        (
            Arc::new(Scope::new(connect, hint, device, continuity)),
            session,
        )
    }

    /// Admit the room's deltas and hand the governor's call answers to the
    /// participations, as the shell's actor does, until `done`.
    async fn receive_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        tilo_test_env::within(what, async {
            while !done(self) {
                let inbound = tokio::select! {
                    inbound = self.session.recv() => inbound.expect("the session stays open"),
                    () = tokio::time::sleep(Duration::from_millis(50)) => continue,
                };
                match inbound {
                    Inbound::Delta { author, data } => {
                        let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                        let governor = self
                            .owner
                            .with_current(self.room, |view| Ok(view.governor().device_public_key))
                            .unwrap();
                        match self
                            .scope
                            .input(author, author == governor, message)
                            .admit(&self.identity)
                        {
                            Ok(_) => {}
                            Err(error) if error.contains("pin changed") => {
                                (self.scope, self.session) = Self::dial(
                                    &self.owner,
                                    self.room,
                                    &self.identity,
                                    &self.endpoint,
                                    self.workspace_id,
                                )
                                .await;
                                for call in &self.calls {
                                    let _ = call.rejoin(&self.session);
                                }
                            }
                            Err(error) => panic!("{what}: admission refused: {error}"),
                        }
                    }
                    Inbound::Voice(event) => {
                        for call in &self.calls {
                            if call.apply(&event, Some(&self.session), |_| false) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .await;
    }

    /// Join the call on the voice channel, starting it, as the shell does.
    async fn call(&mut self) -> Arc<Participation> {
        let channel = Channel {
            app: CHAT.into(),
            record: String::new(),
            channel: CHANNEL.into(),
        };
        loop {
            // Until the governor admits the channel, it refuses to start.
            let call = Participation::open(
                self.owner.clone(),
                self.room,
                &channel,
                &self.session,
                |_| Ok(()),
            )
            .unwrap();
            self.calls = vec![call.clone()];
            self.receive_until("the governor answers the call", |_| {
                call.grant().is_some() || call.refused().is_some()
            })
            .await;
            if call.grant().is_some() {
                return call;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Create the room's chat record with a voice channel from a replica on
/// another device of the owner that entered the room, over its own relay
/// session, which the caller keeps open: local publication never creates a
/// record.
async fn create_voice_channel(
    address: iroh::EndpointAddr,
    workspace_id: u64,
    room: &str,
    host: &DeviceCert,
    device: &support::Device,
) -> RelayCoeditSession {
    let session = RelayCoeditSession::join(
        tilo_relay::RelayClient::connect_to_pinned_host(
            endpoint(&device.key).await,
            address,
            device.cert.clone(),
            device.key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        workspace_id,
        device.cert.device_public_key,
    )
    .unwrap();
    let mut peer = StatePeer::new(
        room,
        host.clone(),
        vec![host.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    tilo_test_env::within("the replica learns the room", async {
        while peer
            .permissions(device.cert.device_public_key, SystemTime::now())
            .is_err()
        {
            if let Some(Inbound::Delta { author, data }) = session.recv().await {
                let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                peer.receive_from(
                    TransportOrigin::RetainedRelayDelta(author),
                    message,
                    SystemTime::now(),
                )
                .unwrap();
            }
        }
    })
    .await;
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let channel = chat.put_object(&channels, CHANNEL, ObjType::Map).unwrap();
    chat.put(&channel, "kind", "voice").unwrap();
    chat.put(&channel, "workspace", "native").unwrap();
    chat.commit();
    let message = peer
        .publish_incremental(
            CHAT,
            "",
            chat.save_after(&[]),
            &device.key,
            SystemTime::now(),
        )
        .unwrap();
    let (channel, kind) = tilo_peer_state::transport::retention(&message, false).unwrap();
    let body = postcard::to_stdvec(&message).unwrap();
    if kind == tilo_peer_state::transport::Retention::Checkpoint {
        session.publish_checkpoint(channel, body)
    } else {
        session.publish_retained_delta(channel, body)
    }
    .unwrap();
    session
}

fn carrier(grant: &VoiceGrant, generation: u64) -> Client {
    Client::start(
        Connection {
            scope: ConnectionScope {
                endpoint: grant.endpoint.clone(),
                fingerprints: grant.fingerprints.clone(),
                owner_epoch: grant.participation,
                carrier_generation: generation,
                publication: grant.publication.clone(),
                subscriptions: grant.subscriptions.clone(),
            },
            authorization: grant.token.clone(),
        },
        Arc::new(Granted),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owners_native_shell_and_an_invited_browser_talk_on_relay_grants() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bob = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let desk = enrol(&alice, "alice-desk", DeviceCapabilities::primary());
    let page = enrol(&bob, "bob-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            ..RelayConfig::default()
        },
        RelayIdentity {
            user_public_key: alice.verifying_key().to_bytes(),
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let web = relay
        .serve_web(WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: TlsSource::Development {
                hosts: vec!["localhost".into(), "127.0.0.1".into()],
                store: None,
            },
            allowed_origins: Vec::new(),
            plain_listen: None,
        })
        .await
        .unwrap();
    let address = relay.node_addr();

    // Alice's room on her relay; her native device holds it as its owner,
    // and Bob's person is invited.
    let admin = RoomAdminClient::new(endpoint(&laptop.key).await, laptop.cert.clone());
    let room = admin.create(address.clone(), "Standup").await.unwrap();
    assert!(matches!(
        admin
            .send(
                address.clone(),
                RoomCommand::Enter {
                    room: room.room.clone()
                }
            )
            .await
            .unwrap(),
        RoomReply::Entered { .. }
    ));
    assert!(matches!(
        RoomAdminClient::new(endpoint(&desk.key).await, desk.cert.clone())
            .send(
                address.clone(),
                RoomCommand::Enter {
                    room: room.room.clone()
                }
            )
            .await
            .unwrap(),
        RoomReply::Entered { .. }
    ));
    let own = admin
        .invite(
            address.clone(),
            &room.room,
            MemberRole::Editor,
            Some(alice.verifying_key().to_bytes()),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut native = Native::join(
        directory.path(),
        laptop.cert.clone(),
        laptop.key.clone(),
        &join::encode_invitation_text(&own.signed_invitation),
        room.workspace_id,
    )
    .await;
    let invitation = admin
        .invite(
            address.clone(),
            &room.room,
            MemberRole::Editor,
            Some(bob.verifying_key().to_bytes()),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(endpoint(&page.key).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation.clone(),
            page.cert.clone(),
            &page.key,
            [7; 32],
        )
        .await
        .unwrap();

    // The chat gets a voice channel; Alice's native device starts its call
    // and the governor grants her the first participation.
    let _replica = create_voice_channel(
        address.clone(),
        room.workspace_id,
        &room.room,
        &host.cert,
        &desk,
    )
    .await;
    native
        .receive_until("Alice's native device admits the voice channel", |native| {
            native
                .owner
                .with_current(native.room, |view| Ok(view.peer().document(CHAT, "")))
                .unwrap()
                .is_some()
        })
        .await;
    let call = native.call().await;
    let alone = call.grant().unwrap();
    assert_eq!(alone.participants.len(), 1);
    assert!(alone.subscriptions.is_empty());

    // Bob's browser joins the same call over HTTPS.
    let mut page_link = HttpsLink::connect(
        support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap()),
        &page.cert,
        host.cert.device_public_key,
        &RevocationList::new(),
        &page.key,
    )
    .await
    .unwrap();
    page_link
        .send(Envelope::Subscribe {
            workspace_id: room.workspace_id,
        })
        .unwrap();
    browser(&mut page_link, |envelope| match envelope {
        Envelope::Ready { .. } => Some(()),
        Envelope::NotAuthorised { reason, .. } => panic!("browser not admitted: {reason}"),
        _ => None,
    })
    .await;
    page_link
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: alone.call,
            participation: None,
        })
        .unwrap();
    let bob_grant = browser(&mut page_link, granted).await;
    native
        .receive_until("Alice's grant names Bob", |_| {
            call.grant()
                .is_some_and(|grant| grant.participants.len() == 2)
        })
        .await;
    let alice_grant = call.grant().unwrap();
    let alice_voice = alice_grant.publication.clone().unwrap();
    let bob_voice = bob_grant.publication.clone().unwrap();
    assert_eq!(bob_grant.subscriptions, vec![alice_voice.clone()]);
    assert_eq!(alice_grant.subscriptions, vec![bob_voice.clone()]);

    // Alice's carrier is the one her participation authorizes; Bob's is his
    // browser's. Audio flows both ways.
    let alice_carrier =
        Arc::new(Client::start(call.next_carrier().unwrap().unwrap(), call.clone()).unwrap());
    call.set_client(&alice_carrier).unwrap();
    let bob_carrier = carrier(&bob_grant, 1);
    until("Alice's carrier", &alice_carrier, |status| {
        status == Status::Live
    })
    .await;
    until("Bob's carrier", &bob_carrier, |status| {
        status == Status::Live
    })
    .await;
    alice_carrier.enable_publication().unwrap();
    bob_carrier.enable_publication().unwrap();
    hears(
        "Bob hears Alice",
        &alice_carrier,
        &bob_carrier,
        &alice_voice,
    )
    .await;
    hears("Alice hears Bob", &bob_carrier, &alice_carrier, &bob_voice).await;

    // Her heartbeat is answered with a grant for the same scope.
    let before = call.revision();
    call.heartbeat(&native.session, |_| Ok(())).unwrap();
    native
        .receive_until("the governor answers Alice's heartbeat", |_| {
            call.revision() > before
        })
        .await;
    assert!(!call.stale(), "a heartbeat keeps the carrier's scope");
    assert!(call.authorized());

    // Alice leaves: her carrier closes and Bob's roster loses her.
    call.leave(Some(&native.session));
    until("Alice's carrier closes", &alice_carrier, |status| {
        status != Status::Live
    })
    .await;
    browser(&mut page_link, |envelope| {
        granted(envelope).filter(|grant| grant.participants.len() == 1)
    })
    .await;
    drop(web);
    relay.shutdown().await;
}
