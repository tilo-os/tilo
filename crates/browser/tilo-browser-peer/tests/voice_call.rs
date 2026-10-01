//! A call in a room the host governs, between the room's owner on a native
//! iroh device and an invited person in a browser session: both get their
//! grants from the host, hear each other over its `moq-lite` voice carrier,
//! and lose their voice when the owner denies it or ends the call.
//!
//! The browser's session is the page's own HTTPS carrier; its voice carrier
//! is the native `moq-lite` client, dialling the same WebTransport endpoint a
//! page would.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::HttpsLink;
use tilo_moq::client::{
    CapturedOpus, Client, ClientAuthority, Connection, ConnectionScope, Status,
};
use tilo_moq::relay::Direction;
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::rooms::{RoomAdminClient, RoomCommand, RoomReply};
use tilo_relay::web::{TlsSource, WebConfig};
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{RelayClient, RelayConfig};
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, CallState, Directions, VoiceGrant};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use support::{enrol, wait_for};

const CHAT: &str = "so.tilo.chat";
const CHANNEL: &str = "standup";
const BOTH: Directions = Directions {
    publish: true,
    subscribe: true,
};

/// An endpoint whose key is the device's own, as a device dials with.
async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// The grant is the whole authority; the client checks nothing more.
struct Granted;
impl ClientAuthority for Granted {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
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

/// The next envelope from the native device that `pick` accepts.
async fn native<T>(client: &RelayClient, mut pick: impl FnMut(Envelope) -> Option<T>) -> T {
    tilo_test_env::within("the host answers the native device", async {
        loop {
            if let Some(value) = pick(client.recv().await.expect("connection open")) {
                return value;
            }
        }
    })
    .await
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

async fn call(
    client: &RelayClient,
    workspace_id: u64,
    request: u8,
    operation: CallOperation,
) -> Result<CallState, String> {
    client
        .send(Envelope::VoiceCall {
            workspace_id,
            request: [request; 16],
            operation,
        })
        .unwrap();
    native(client, |envelope| match envelope {
        Envelope::VoiceCallResult {
            request: answered,
            result,
            ..
        } if answered == [request; 16] => Some(result),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invited_browser_and_the_owners_device_talk_in_a_governed_call() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bob = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
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

    // Alice's room; Bob, a different person, redeems her invitation with his
    // browser's device (over iroh: pages have no redemption route yet).
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

    // Alice's chat gets a voice channel; the room's governor learns it.
    let session = RelayCoeditSession::join(
        RelayClient::connect_to_pinned_host(
            endpoint(&laptop.key).await,
            address.clone(),
            laptop.cert.clone(),
            laptop.key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        room.workspace_id,
        laptop.cert.device_public_key,
    )
    .unwrap();
    let mut peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    tilo_test_env::within("Alice's replica learns the room", async {
        while peer
            .permissions(laptop.cert.device_public_key, SystemTime::now())
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
            &laptop.key,
            SystemTime::now(),
        )
        .unwrap();
    let (channel, _) = tilo_peer_state::transport::retention(&message, false).unwrap();
    session
        .publish_retained_delta(channel, postcard::to_stdvec(&message).unwrap())
        .unwrap();
    let alice_link = session.into_client();

    // Alice starts a call on it; only the room's owner may.
    let started = wait_for("the governor to bind the call", || {
        let alice_link = &alice_link;
        async move {
            call(
                alice_link,
                room.workspace_id,
                1,
                CallOperation::Start {
                    app_id: CHAT.into(),
                    record_id: String::new(),
                    channel_id: CHANNEL.into(),
                    allowed: BOTH,
                },
            )
            .await
            .ok()
        }
    })
    .await;
    assert!(started.active);
    alice_link
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let alone = native(&alice_link, granted).await;
    assert_eq!(alone.participants.len(), 1);
    assert!(alone.subscriptions.is_empty());

    // Bob's browser joins over HTTPS; as a guest it may not end the call.
    let mut page_link = HttpsLink::connect(
        support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap()),
        &page.cert,
        host.cert.device_public_key,
        &RevocationList::new(),
        &page.key,
    )
    .await
    .unwrap();
    assert!(page_link.voice(), "the host advertises calls");
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
        .send(Envelope::VoiceCall {
            workspace_id: room.workspace_id,
            request: [2; 16],
            operation: CallOperation::End {
                call: started.call,
                epoch: started.epoch,
            },
        })
        .unwrap();
    let intruder = browser(&mut page_link, |envelope| match envelope {
        Envelope::VoiceCallResult { result, .. } => Some(result),
        _ => None,
    })
    .await;
    assert!(intruder.is_err(), "a guest cannot end the call");
    page_link
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let bob_grant = browser(&mut page_link, granted).await;
    let alice_grant = native(&alice_link, |envelope| {
        granted(envelope).filter(|grant| grant.participants.len() == 2)
    })
    .await;
    let alice_voice = alice_grant.publication.clone().unwrap();
    let bob_voice = bob_grant.publication.clone().unwrap();
    assert_eq!(bob_grant.subscriptions, vec![alice_voice.clone()]);
    assert_eq!(alice_grant.subscriptions, vec![bob_voice.clone()]);

    // Both carriers are admitted by the host's grants; audio flows both ways.
    let alice_carrier = carrier(&alice_grant, 1);
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

    // A forged grant is not admitted.
    let mut forged = bob_grant.clone();
    forged.token = vec![0; 32];
    let forged = carrier(&forged, 2);
    until("the forged carrier", &forged, |status| {
        matches!(status, Status::Failed | Status::Denied | Status::Closed)
    })
    .await;

    // Alice mutes Bob: his grant loses its publication and the host closes
    // the carrier that published.
    let muted = call(
        &alice_link,
        room.workspace_id,
        3,
        CallOperation::SetDenial {
            call: started.call,
            epoch: started.epoch,
            person: bob.verifying_key().to_bytes(),
            denied: Directions {
                publish: true,
                subscribe: false,
            },
        },
    )
    .await
    .unwrap();
    assert!(muted.epoch > started.epoch);
    let listening = browser(&mut page_link, |envelope| {
        granted(envelope).filter(|grant| !grant.directions.publish)
    })
    .await;
    assert!(listening.publication.is_none());
    assert!(listening.directions.subscribe);
    until("Bob's publishing carrier closes", &bob_carrier, |status| {
        status != Status::Live
    })
    .await;
    // The roster changed for Alice too: nobody left for her to hear. Each
    // side follows its new grant with a new carrier.
    let speaking = native(&alice_link, |envelope| {
        granted(envelope).filter(|grant| grant.subscriptions.is_empty())
    })
    .await;
    assert_eq!(speaking.publication.as_deref(), Some(alice_voice.as_str()));
    alice_carrier.close();
    let alice_speaker = carrier(&speaking, 2);
    let bob_listener = carrier(&listening, 2);
    until("Alice's speaking carrier", &alice_speaker, |status| {
        status == Status::Live
    })
    .await;
    until("Bob's listening carrier", &bob_listener, |status| {
        status == Status::Live
    })
    .await;
    alice_speaker.enable_publication().unwrap();
    hears(
        "Bob still hears Alice",
        &alice_speaker,
        &bob_listener,
        &alice_voice,
    )
    .await;

    // Alice ends the call: Bob is told and his carrier is closed.
    call(
        &alice_link,
        room.workspace_id,
        4,
        CallOperation::End {
            call: started.call,
            epoch: muted.epoch,
        },
    )
    .await
    .unwrap();
    browser(&mut page_link, |envelope| match envelope {
        Envelope::VoiceRefused { .. } => Some(()),
        _ => None,
    })
    .await;
    until(
        "Bob's carrier closes with the call",
        &bob_listener,
        |status| status != Status::Live,
    )
    .await;
    page_link
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let refused = browser(&mut page_link, |envelope| match envelope {
        Envelope::VoiceRefused { reason, .. } => Some(reason),
        Envelope::VoiceGranted { .. } => panic!("an ended call admits no one"),
        _ => None,
    })
    .await;
    assert!(refused.contains("ended"), "{refused}");
    drop(web);
    relay.shutdown().await;
}
