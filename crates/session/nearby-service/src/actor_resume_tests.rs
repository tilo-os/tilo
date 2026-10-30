//! Two real Iroh endpoints resume an immutable Nearby spool after fresh consent.

use super::{CHECKPOINT_BYTES, IncomingManifest, NearbyTransferStore, SecureNearbyDestination};
use iroh::{EndpointAddr, EndpointId};
use tilo_network_actor::{
    NearbyTransferCheckpoint, NetCmd, NetEvent, PERSONAL_TRANSFER_CHUNK_BYTES,
    PersonalFabricPayload, PersonalNearbyTransferFrame,
};
use tokio::sync::mpsc;

/// One of the person `root`'s devices, hearing the person's other devices,
/// keeping its host under `state`.
async fn peer(
    root: &ed25519_dalek::SigningKey,
    state: &std::path::Path,
) -> (mpsc::Sender<NetCmd>, mpsc::Receiver<NetEvent>, EndpointAddr) {
    let key = ed25519_dalek::SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(
        getrandom::SysRng,
    ));
    let device = tilo_network_actor::siblings::spawn(root, key, state).await;
    let (events, receiver) = mpsc::channel(64);
    device
        .cmd
        .send(NetCmd::WatchSiblings { on_event: events })
        .await
        .unwrap();
    (device.cmd, receiver, device.addr)
}

async fn joined(events: &mut mpsc::Receiver<NetEvent>, expected: EndpointId) {
    tilo_test_env::within("Nearby peers joined", async {
        loop {
            if matches!(events.recv().await.unwrap(), NetEvent::Sibling { peer, .. } if peer == expected) {
                return;
            }
        }
    }).await;
}

async fn receive(
    events: &mut mpsc::Receiver<NetEvent>,
    person: [u8; 32],
    source: EndpointId,
    manifest: &IncomingManifest,
    store: &mut NearbyTransferStore,
) {
    tilo_test_env::within("Nearby resumed object arrived", async {
        loop {
            let NetEvent::PersonalFabric {
                peer,
                user_public_key,
                payload: PersonalFabricPayload::NearbyTransfer(frame),
                ..
            } = events.recv().await.unwrap()
            else {
                continue;
            };
            assert_eq!(peer, source);
            assert_eq!(user_public_key, person);
            match frame {
                PersonalNearbyTransferFrame::Chunk {
                    transfer_id,
                    request_id,
                    offset,
                    bytes,
                } => {
                    assert_eq!(transfer_id, manifest.id);
                    assert_eq!(request_id, manifest.request_id);
                    assert!(offset >= CHECKPOINT_BYTES);
                    assert!(bytes.len() <= PERSONAL_TRANSFER_CHUNK_BYTES);
                    store.append(transfer_id, offset, &bytes).unwrap();
                }
                PersonalNearbyTransferFrame::Finish {
                    transfer_id,
                    request_id,
                    content_hash,
                } => {
                    assert_eq!(transfer_id, manifest.id);
                    assert_eq!(request_id, manifest.request_id);
                    assert_eq!(content_hash, manifest.content_hash);
                    return;
                }
                PersonalNearbyTransferFrame::Abort { .. } => panic!("resumed transfer aborted"),
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nearby_two_peers_resume_reopened_spools_after_fresh_receiver_consent() {
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x67; 32]);
    let states = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (sender, mut sent_events, sender_addr) = peer(&person, states.0.path()).await;
    let (receiver, mut received_events, receiver_addr) = peer(&person, states.1.path()).await;
    sender
        .send(NetCmd::Connect {
            addr: receiver_addr.clone(),
        })
        .await
        .unwrap();
    receiver
        .send(NetCmd::Connect {
            addr: sender_addr.clone(),
        })
        .await
        .unwrap();
    joined(&mut sent_events, receiver_addr.id).await;
    joined(&mut received_events, sender_addr.id).await;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("report.bin");
    let checkpoint = usize::try_from(CHECKPOINT_BYTES).unwrap();
    let bytes = vec![0x67; checkpoint + PERSONAL_TRANSFER_CHUNK_BYTES + 17];
    std::fs::write(&source, &bytes).unwrap();
    let sender_spool = root.path().join("sender");
    let mut outgoing = NearbyTransferStore::open(sender_spool.clone(), 10).unwrap();
    let staged = outgoing
        .stage(&source, receiver_addr.id.to_string(), 10)
        .unwrap();
    std::fs::write(&source, b"edited after send").unwrap();
    let receiver_spool = root.path().join("receiver");
    let mut incoming = NearbyTransferStore::open(receiver_spool.clone(), 10).unwrap();
    let mut manifest = IncomingManifest {
        id: staged.id,
        source: sender_addr.id.to_string(),
        request_id: [0x68; 16],
        name: staged.name.clone(),
        kind: staged.kind,
        total_bytes: staged.total_bytes,
        content_hash: staged.content_hash,
    };
    incoming.begin_incoming(manifest.clone(), 10).unwrap();
    incoming.append(staged.id, 0, &bytes[..checkpoint]).unwrap();
    drop(incoming);
    drop(outgoing);

    // Both durable spools are reopened; the newly received offer has
    // a new request ID and is accepted again with an explicit destination.
    let outgoing = NearbyTransferStore::open(sender_spool, 11).unwrap();
    let mut incoming = NearbyTransferStore::open(receiver_spool, 11).unwrap();
    manifest.request_id = [0x69; 16];
    let downloads = root.path().join("downloads");
    std::fs::create_dir(&downloads).unwrap();
    let collision = downloads.join(&staged.name);
    std::fs::write(&collision, b"keep existing").unwrap();
    let destination = SecureNearbyDestination::open_grant(&downloads).unwrap();
    let (resume_offset, prefix_hash) = incoming
        .accept_incoming_offer(manifest.clone(), 11)
        .unwrap();
    assert_eq!(resume_offset, CHECKPOINT_BYTES);
    sender
        .send(NetCmd::PublishNearbyTransfer {
            user_public_key: person.verifying_key().to_bytes(),
            target: receiver_addr.id,
            transfer: NearbyTransferCheckpoint {
                transfer_id: staged.id,
                request_id: manifest.request_id,
                total_bytes: staged.total_bytes,
                resume_offset,
                prefix_hash,
                content_hash: staged.content_hash,
            },
            file: std::fs::File::open(outgoing.object_path(staged.id)).unwrap(),
        })
        .await
        .unwrap();
    receive(
        &mut received_events,
        person.verifying_key().to_bytes(),
        sender_addr.id,
        &manifest,
        &mut incoming,
    )
    .await;
    let installed = incoming.finish_secure(staged.id, &destination).unwrap();
    assert_ne!(installed, collision);
    assert_eq!(std::fs::read(installed).unwrap(), bytes);
    assert_eq!(std::fs::read(collision).unwrap(), b"keep existing");
    assert!(incoming.inbox().is_empty());
    drop(sender);
    drop(receiver);
}
