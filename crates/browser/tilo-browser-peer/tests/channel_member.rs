//! The browser as a `channel-crypto` member of a channel a native device
//! stewards: it learns the steward from the genesis frame the host retained,
//! asks for access with its device key, joins from the steward's Welcome,
//! and then opens the steward's sealed documents and seals its own, with
//! the host carrying only ciphertext and governance.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; each test is one transcript between a steward and a browser"
)]

mod support;

use std::collections::{BTreeMap, BTreeSet};

use channel_crypto::{ChannelSteward, VaultKey};
use collab_types::{
    ContentRetention, DocumentFrame, DocumentMutation, Ed25519PublicKey, MlsAccessRequestV1,
    OpKind, SignedOp, document_artifact_id, document_attestation_id, document_channel_id,
};
use neighbour_wire::NeighbourMsg;
use tilo_browser_peer::{Host, PeerEvent, PeerStore as _};
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{
    ROOM, enrol, ready, route, start_host, start_peer, start_peer_with_store, wait_for, workspace,
};

/// A native steward's side of the room, driven by hand: publish a message
/// through the host the way a shell does.
fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    let (channel, retention) = neighbour_wire::retention(message, false).unwrap();
    let body = postcard::to_allocvec(message).unwrap();
    match retention {
        neighbour_wire::Retention::Checkpoint => session.publish_checkpoint(channel, body),
        neighbour_wire::Retention::Delta => session.publish_retained_delta(channel, body),
    }
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_joins_a_native_stewarded_channel_and_edits_its_sealed_record() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;

    // The laptop stewards the note's channel and announces itself.
    let route = route();
    let channel = document_channel_id(&route.app_id, &route.cluster_id, &route.record);
    let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
    let vault = VaultKey::new([7; 32]);
    let steward_dir = tempfile::tempdir().unwrap();
    let mut steward =
        ChannelSteward::open_or_create(steward_dir.path(), channel, laptop.key.clone(), &vault)
            .unwrap();
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect(
        endpoint,
        host.relay.node_addr(),
        root.verifying_key().to_bytes(),
        laptop.cert.clone(),
        laptop.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let laptop_device = laptop.key.verifying_key().to_bytes();
    let session = RelayCoeditSession::join(client, workspace(), laptop_device).unwrap();
    publish(
        &session,
        &NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(laptop.cert.clone()),
            revocations: Vec::new(),
            successions: Vec::new(),
        },
    );
    publish(
        &session,
        &NeighbourMsg::MlsGovernance {
            room: ROOM.into(),
            frame: Box::new(steward.journal().frames()[0].clone()),
            delivery: 1,
        },
    );
    // A sealed edit of the note, written before the browser arrives.
    let seal = |steward: &ChannelSteward, seq: u64, text: &str| {
        let frame = DocumentMutation::Incremental(text.as_bytes().to_vec())
            .encode()
            .unwrap();
        let (metadata, sealed) = steward
            .content_keys()
            .unwrap()
            .seal_content(artifact, ContentRetention::Delta, &frame)
            .unwrap();
        let mut op = SignedOp::new_unsigned_with_encryption(
            sealed,
            artifact,
            Ed25519PublicKey::from_bytes(laptop_device),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record),
            OpKind::Change,
            std::time::SystemTime::now(),
            seq,
            metadata,
        );
        collab_types::sign_op(&mut op, &laptop.key).unwrap();
        NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&laptop_device),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            record_id: route.record.clone(),
            envelope: Box::new(op),
        }
    };
    publish(&session, &seal(&steward, 1, "from the laptop"));

    // Publishing only queues the frames for the host. Until it retains the
    // genesis, the room's replay names no governor and reads as ungoverned,
    // so let a fresh member's replay prove all three are held first.
    let observer = enrol(&root, "observer", DeviceCapabilities::member());
    let observer_endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let observer_client = RelayClient::connect(
        observer_endpoint,
        host.relay.node_addr(),
        root.verifying_key().to_bytes(),
        observer.cert.clone(),
        observer.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let observer_session = RelayCoeditSession::join(
        observer_client,
        workspace(),
        observer.key.verifying_key().to_bytes(),
    )
    .unwrap();
    let (mut hello, mut genesis, mut sealed) = (false, false, false);
    let retained_by = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    while !(hello && genesis && sealed) {
        let inbound = tokio::time::timeout_at(retained_by, observer_session.recv())
            .await
            .expect("the host to retain the laptop's frames")
            .expect("observer session open");
        let Inbound::Delta { author, data } = inbound else {
            continue;
        };
        if author != laptop_device {
            continue;
        }
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::IdentityHello { .. }) => hello = true,
            Ok(NeighbourMsg::MlsGovernance { .. }) => genesis = true,
            Ok(NeighbourMsg::AppDocument { .. }) => sealed = true,
            _ => {}
        }
    }
    observer_session.close().await;

    // The browser joins the room over HTTPS and opens the note.
    let peer = start_peer(
        Host::Https(host.https()),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    let events = peer.events();
    // A reader of the status (the host's room actions on another thread)
    // that sees the note no longer awaiting keys finds its documents among
    // the events already: a record read as open is never read empty.
    let opened_when_unsealed = {
        let (peer, route, watched) = (peer.clone(), route.clone(), peer.events());
        std::thread::spawn(move || {
            let deadline = tilo_test_env::hang_deadline();
            let mut held = false;
            while std::time::Instant::now() < deadline {
                let awaiting = peer
                    .status()
                    .routes
                    .get(&route)
                    .map(|status| status.awaiting_keys);
                match awaiting {
                    Some(count) if count > 0 => held = true,
                    Some(0) if held => {
                        return watched.drain().into_iter().any(|event| {
                            matches!(&event, PeerEvent::Document(document)
                                if document.mutation() == Some(DocumentMutation::Incremental(
                                    b"from the laptop".to_vec())))
                        });
                    }
                    _ => {}
                }
                std::thread::yield_now();
            }
            panic!("the note was never held for its keys, then opened");
        })
    };
    peer.join(workspace()).unwrap();
    peer.open_route(route.clone()).unwrap();
    ready(&[&peer]).await;
    // Written before it holds the keys: held, then sealed once admitted.
    peer.publish(
        route.clone(),
        &DocumentMutation::Incremental(b"from the browser".to_vec()),
    )
    .unwrap();

    // The laptop admits the browser when its signed request arrives.
    let browser_device = Ed25519PublicKey::from_bytes(browser.key.verifying_key().to_bytes());
    let allowed = BTreeSet::from([Ed25519PublicKey::from_bytes(laptop_device), browser_device]);
    let mut requests: BTreeMap<Ed25519PublicKey, MlsAccessRequestV1> = BTreeMap::new();
    let mut sent_welcome = false;
    let mut browser_edit = None;
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    while browser_edit.is_none() {
        let inbound = tokio::time::timeout_at(deadline, session.recv())
            .await
            .expect("the browser's sealed edit")
            .expect("host session open");
        let Inbound::Delta { author, data } = inbound else {
            continue;
        };
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::MlsRoutedAccessRequest { request, .. }) => {
                assert_eq!(author, *browser_device.as_bytes());
                request
                    .verify()
                    .expect("signed by the browser's device key");
                requests.insert(request.device, *request);
                steward.reconcile(&allowed, &requests, &vault).unwrap();
                for outgoing in steward.outbox() {
                    for frame in outgoing
                        .frames
                        .into_iter()
                        .flat_map(|frame| frame.into_deliveries().unwrap())
                    {
                        publish(
                            &session,
                            &NeighbourMsg::MlsGovernance {
                                room: ROOM.into(),
                                frame: Box::new(frame),
                                delivery: 2,
                            },
                        );
                    }
                }
                sent_welcome = true;
            }
            Ok(NeighbourMsg::MlsGovernanceAck { ack, .. }) => {
                ack.verify().unwrap();
                steward.acknowledge(&ack).unwrap();
            }
            Ok(NeighbourMsg::AppDocument { envelope, .. }) => {
                assert_eq!(author, *browser_device.as_bytes());
                let metadata = envelope
                    .encryption
                    .expect("the browser never writes the encrypted channel in the clear");
                let clear = steward
                    .content_keys()
                    .unwrap()
                    .open_content(artifact, &metadata, &envelope.payload)
                    .unwrap();
                browser_edit = Some(clear.to_vec());
            }
            _ => {}
        }
    }
    assert!(sent_welcome);
    assert!(
        opened_when_unsealed.join().unwrap(),
        "the status said the note no longer awaited its keys before its document was out"
    );
    assert_eq!(
        DocumentFrame::decode(browser_edit.as_ref().unwrap()).unwrap(),
        DocumentFrame::Whole(DocumentMutation::Incremental(b"from the browser".to_vec()))
    );

    // What the laptop sealed before and after the browser joined opens.
    publish(&session, &seal(&steward, 2, "after the welcome"));
    let mut opened = Vec::new();
    wait_for("the laptop's sealed edits to open in the browser", || {
        for event in events.drain() {
            match event {
                PeerEvent::Document(document) => {
                    if let Some(DocumentMutation::Incremental(bytes)) = document.mutation() {
                        opened.push(String::from_utf8(bytes).unwrap());
                    }
                }
                PeerEvent::DocumentRejected(reason) => panic!("rejected: {reason}"),
                _ => {}
            }
        }
        let done = (opened.len() >= 2).then(|| opened.clone());
        async move { done }
    })
    .await;
    assert_eq!(opened, vec!["from the laptop", "after the welcome"]);

    // Another device is admitted and the laptop writes under the new epoch
    // at once. The edit reaches the browser before the commit that carries
    // that epoch's key: it waits for the key instead of being refused.
    let phone = enrol(&root, "phone", DeviceCapabilities::member());
    let phone_device = Ed25519PublicKey::from_bytes(phone.key.verifying_key().to_bytes());
    let phone_dir = tempfile::tempdir().unwrap();
    let mut phone_member = channel_crypto::ChannelMember::open(
        phone_dir.path(),
        channel,
        phone_device,
        Ed25519PublicKey::from_bytes(laptop_device),
        &vault,
    )
    .unwrap();
    let phone_request = phone_member
        .access_request(&phone.key, 1, &vault)
        .unwrap()
        .unwrap();
    requests.insert(phone_device, phone_request);
    let allowed = BTreeSet::from([
        Ed25519PublicKey::from_bytes(laptop_device),
        browser_device,
        phone_device,
    ]);
    let epoch = steward.epoch();
    steward.reconcile(&allowed, &requests, &vault).unwrap();
    assert!(steward.epoch() > epoch);
    publish(&session, &seal(&steward, 3, "under the new epoch"));
    for outgoing in steward.outbox() {
        if outgoing.recipient == browser_device {
            for frame in outgoing
                .frames
                .into_iter()
                .flat_map(|frame| frame.into_deliveries().unwrap())
            {
                publish(
                    &session,
                    &NeighbourMsg::MlsGovernance {
                        room: ROOM.into(),
                        frame: Box::new(frame),
                        delivery: 3,
                    },
                );
            }
        }
    }
    wait_for(
        "the edit under the new epoch to open in the browser",
        || {
            for event in events.drain() {
                match event {
                    PeerEvent::Document(document) => {
                        if let Some(DocumentMutation::Incremental(bytes)) = document.mutation() {
                            opened.push(String::from_utf8(bytes).unwrap());
                        }
                    }
                    PeerEvent::DocumentRejected(reason) => panic!("rejected: {reason}"),
                    _ => {}
                }
            }
            let done = (opened.len() >= 3).then(|| opened.clone());
            async move { done }
        },
    )
    .await;
    assert_eq!(opened.last().unwrap(), "under the new epoch");

    let mut clear = SignedOp::new_unsigned(
        DocumentMutation::Incremental(b"in the clear".to_vec())
            .encode()
            .unwrap(),
        artifact,
        Ed25519PublicKey::from_bytes(laptop_device),
        document_attestation_id(&route.app_id, &route.cluster_id, &route.record),
        OpKind::Change,
        std::time::SystemTime::now(),
        4,
    );
    collab_types::sign_op(&mut clear, &laptop.key).unwrap();
    publish(
        &session,
        &NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&laptop_device),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            record_id: route.record.clone(),
            envelope: Box::new(clear),
        },
    );
    let refused = wait_for("cleartext on the sealed channel to be refused", || {
        let mut refused = None;
        for event in events.drain() {
            match event {
                PeerEvent::DocumentRejected(reason) => refused = Some(reason),
                PeerEvent::Document(document) => {
                    panic!("cleartext opened: {:?}", document.mutation());
                }
                _ => {}
            }
        }
        async move { refused }
    })
    .await;
    assert_eq!(refused, "cleartext on a channel that requires it sealed");
    peer.close();
    host.relay.shutdown().await;
}

/// An edit the browser wrote before the steward admitted it survives a
/// page reload: the reloaded page keeps it with its channel state and seals
/// and commits it once admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_held_for_the_keys_survives_a_reload() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let route = route();
    let channel = document_channel_id(&route.app_id, &route.cluster_id, &route.record);
    let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
    let vault = VaultKey::new([7; 32]);
    let steward_dir = tempfile::tempdir().unwrap();
    let mut steward =
        ChannelSteward::open_or_create(steward_dir.path(), channel, laptop.key.clone(), &vault)
            .unwrap();
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect(
        endpoint,
        host.relay.node_addr(),
        root.verifying_key().to_bytes(),
        laptop.cert.clone(),
        laptop.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let laptop_device = laptop.key.verifying_key().to_bytes();
    let session = RelayCoeditSession::join(client, workspace(), laptop_device).unwrap();
    publish(
        &session,
        &NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(laptop.cert.clone()),
            revocations: Vec::new(),
            successions: Vec::new(),
        },
    );
    publish(
        &session,
        &NeighbourMsg::MlsGovernance {
            room: ROOM.into(),
            frame: Box::new(steward.journal().frames()[0].clone()),
            delivery: 1,
        },
    );

    let store = std::sync::Arc::new(tilo_browser_peer::MemoryPeerStore::default());
    let start = || {
        start_peer_with_store(
            Host::Https(host.https()),
            browser.cert.clone(),
            host_device.cert.clone(),
            browser.key.clone(),
            store.clone(),
        )
    };
    let first = start();
    first.join(workspace()).unwrap();
    first.open_route(route.clone()).unwrap();
    ready(&[&first]).await;
    first
        .publish(
            route.clone(),
            &DocumentMutation::Incremental(b"before the reload".to_vec()),
        )
        .unwrap();
    wait_for("the held edit to be stored", || {
        let stored = store.load_channels().is_some_and(|bytes| {
            bytes
                .windows(b"before the reload".len())
                .any(|window| window == b"before the reload")
        });
        async move { stored.then_some(()) }
    })
    .await;
    first.close();

    let reloaded = start();
    reloaded.join(workspace()).unwrap();
    reloaded.open_route(route.clone()).unwrap();
    let browser_device = Ed25519PublicKey::from_bytes(browser.key.verifying_key().to_bytes());
    let allowed = BTreeSet::from([Ed25519PublicKey::from_bytes(laptop_device), browser_device]);
    let mut requests: BTreeMap<Ed25519PublicKey, MlsAccessRequestV1> = BTreeMap::new();
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    let edit = loop {
        let inbound = tokio::time::timeout_at(deadline, session.recv())
            .await
            .expect("the held edit after the reload")
            .expect("host session open");
        let Inbound::Delta { data, .. } = inbound else {
            continue;
        };
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::MlsRoutedAccessRequest { request, .. }) => {
                requests.insert(request.device, *request);
                steward.reconcile(&allowed, &requests, &vault).unwrap();
                for outgoing in steward.outbox() {
                    for frame in outgoing
                        .frames
                        .into_iter()
                        .flat_map(|frame| frame.into_deliveries().unwrap())
                    {
                        publish(
                            &session,
                            &NeighbourMsg::MlsGovernance {
                                room: ROOM.into(),
                                frame: Box::new(frame),
                                delivery: 2,
                            },
                        );
                    }
                }
            }
            Ok(NeighbourMsg::MlsGovernanceAck { ack, .. }) => {
                steward.acknowledge(&ack).unwrap();
            }
            Ok(NeighbourMsg::AppDocument { envelope, .. }) => {
                let metadata = envelope.encryption.expect("sealed");
                break steward
                    .content_keys()
                    .unwrap()
                    .open_content(artifact, &metadata, &envelope.payload)
                    .unwrap()
                    .to_vec();
            }
            _ => {}
        }
    };
    assert_eq!(
        DocumentFrame::decode(&edit).unwrap(),
        DocumentFrame::Whole(DocumentMutation::Incremental(b"before the reload".to_vec()))
    );
    reloaded.close();
    host.relay.shutdown().await;
}

/// A page that joined before the steward announced the channel cannot tell
/// the room is governed: its replay names no governor. Its edit waits, and
/// once the genesis arrives live it asks for access and seals the edit. No
/// document leaves it in the clear.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_that_joined_first_holds_its_edit_until_the_genesis_admits_it() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let route = route();
    let channel = document_channel_id(&route.app_id, &route.cluster_id, &route.record);
    let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
    let vault = VaultKey::new([7; 32]);
    let steward_dir = tempfile::tempdir().unwrap();
    let mut steward =
        ChannelSteward::open_or_create(steward_dir.path(), channel, laptop.key.clone(), &vault)
            .unwrap();
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect(
        endpoint,
        host.relay.node_addr(),
        root.verifying_key().to_bytes(),
        laptop.cert.clone(),
        laptop.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let laptop_device = laptop.key.verifying_key().to_bytes();
    let session = RelayCoeditSession::join(client, workspace(), laptop_device).unwrap();
    publish(
        &session,
        &NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(laptop.cert.clone()),
            revocations: Vec::new(),
            successions: Vec::new(),
        },
    );

    let peer = start_peer(
        Host::Https(host.https()),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    peer.join(workspace()).unwrap();
    peer.open_route(route.clone()).unwrap();
    ready(&[&peer]).await;
    peer.publish(
        route.clone(),
        &DocumentMutation::Incremental(b"before the genesis".to_vec()),
    )
    .unwrap();

    publish(
        &session,
        &NeighbourMsg::MlsGovernance {
            room: ROOM.into(),
            frame: Box::new(steward.journal().frames()[0].clone()),
            delivery: 1,
        },
    );
    let browser_device = Ed25519PublicKey::from_bytes(browser.key.verifying_key().to_bytes());
    let allowed = BTreeSet::from([Ed25519PublicKey::from_bytes(laptop_device), browser_device]);
    let mut requests: BTreeMap<Ed25519PublicKey, MlsAccessRequestV1> = BTreeMap::new();
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    let edit = loop {
        let inbound = tokio::time::timeout_at(deadline, session.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "no sealed edit after the genesis; the page's channels:\n{}",
                    peer.status().channels
                )
            })
            .expect("host session open");
        let Inbound::Delta { author, data } = inbound else {
            continue;
        };
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::MlsRoutedAccessRequest { request, .. }) => {
                assert_eq!(author, *browser_device.as_bytes());
                assert_eq!(request.channel, channel);
                requests.insert(request.device, *request);
                steward.reconcile(&allowed, &requests, &vault).unwrap();
                for outgoing in steward.outbox() {
                    for frame in outgoing
                        .frames
                        .into_iter()
                        .flat_map(|frame| frame.into_deliveries().unwrap())
                    {
                        publish(
                            &session,
                            &NeighbourMsg::MlsGovernance {
                                room: ROOM.into(),
                                frame: Box::new(frame),
                                delivery: 2,
                            },
                        );
                    }
                }
            }
            Ok(NeighbourMsg::MlsGovernanceAck { ack, .. }) => {
                steward.acknowledge(&ack).unwrap();
            }
            Ok(NeighbourMsg::AppDocument { envelope, .. }) => {
                assert_eq!(author, *browser_device.as_bytes());
                let metadata = envelope
                    .encryption
                    .expect("an edit written before the genesis never goes out in the clear");
                break steward
                    .content_keys()
                    .unwrap()
                    .open_content(artifact, &metadata, &envelope.payload)
                    .unwrap()
                    .to_vec();
            }
            _ => {}
        }
    };
    assert_eq!(
        DocumentFrame::decode(&edit).unwrap(),
        DocumentFrame::Whole(DocumentMutation::Incremental(
            b"before the genesis".to_vec()
        ))
    );
    peer.close();
    host.relay.shutdown().await;
}
