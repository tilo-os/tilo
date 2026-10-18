//! Exercise the authenticated serial intake, not just the replacement verifier.
use super::*;

#[tokio::test]
async fn device_first_baseline_retires_exact_selected_predecessor() {
    use automerge::{ROOT, transaction::Transactable};
    use std::time::SystemTime;
    let room = uuid::Uuid::from_u128(0x726574697265).to_string();
    let mut fixture = Fixture::with_workspace_id(crate::room_workspace_id(&room).unwrap());
    let cert = fixture.source.peer.cert.clone();
    let now = SystemTime::now();
    let mut peer = tilo_peer_state::StatePeer::new(
        &room,
        cert.clone(),
        vec![cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        tilo_peer_state::SeedState {
            room_changes: tilo_peer_state::seed_room(&room, &cert, &[cert.clone()]).unwrap(),
            documents: vec![],
        },
        &fixture.source_key,
        now,
    )
    .unwrap();
    peer.governance_mut()
        .reconcile_administrator_grants(&fixture.source_key, now)
        .unwrap();
    peer.media_mut()
        .reconcile_media_governance(&fixture.source_key, now)
        .unwrap();
    {
        let mut runtime = fixture.workspace.lock().await;
        runtime
            .store
            .bind_shell_authority(
                tilo_relay_protocol::retention::RetentionAuthority::new(&room, cert).unwrap(),
            )
            .unwrap();
        fixture.store = runtime.store.clone();
    }
    for message in peer
        .durable_bootstrap_messages(&fixture.source_key, now, 1)
        .unwrap()
    {
        if let Some((channel, _)) = neighbour_wire::retention(&message, true) {
            let frame =
                fixture.checkpoint_frame(1, channel, postcard::to_stdvec(&message).unwrap());
            fixture.publish_checkpoint_frame(frame, false, true).await;
        }
    }
    assert!(fixture.store.sanitized_room_anchor().unwrap().is_none());
    let (_, baseline) = peer
        .clone()
        .extract_native_task_private_baseline(&fixture.source_key, now, &Default::default())
        .unwrap();
    // A valid signature over a different predecessor must not authorize retirement.
    let mut wrong = peer.clone();
    let mut document = wrong.admitted_room().unwrap().clone();
    document.put(ROOT, "name", "Another predecessor").unwrap();
    wrong
        .install_seed(
            tilo_peer_state::SeedState {
                room_changes: document
                    .get_changes(&[])
                    .iter()
                    .map(|c| c.raw_bytes().to_vec())
                    .collect(),
                documents: vec![],
            },
            &fixture.source_key,
            now,
        )
        .unwrap();
    let (_, wrong_baseline) = wrong
        .extract_native_task_private_baseline(&fixture.source_key, now, &Default::default())
        .unwrap();
    let frame = |baseline: &tilo_peer_state::SanitizedRoomBaseline| {
        fixture.checkpoint_frame(
            2,
            *blake3::hash(b"shell-tree").as_bytes(),
            postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SanitizedRoomBaseline {
                room: room.clone(),
                baseline: postcard::to_stdvec(baseline).unwrap(),
            })
            .unwrap(),
        )
    };
    assert!(
        enqueue_state(
            &fixture.workspace,
            frame(&wrong_baseline),
            Some(fixture.source.clone()),
            PeerRole::Device,
            fixture.metrics.clone()
        )
        .await
        .is_err()
    );
    assert!(fixture.store.sanitized_room_anchor().unwrap().is_none());
    // A signed baseline without an authenticated device source is not a pin.
    assert!(
        enqueue_state(
            &fixture.workspace,
            frame(&baseline),
            None,
            PeerRole::Device,
            fixture.metrics.clone(),
        )
        .await
        .is_err()
    );
    let admitted = enqueue_state_inner(
        &fixture.workspace,
        frame(&baseline),
        Some(fixture.source.clone()),
        PeerRole::Device,
        fixture.metrics.clone(),
        false,
    )
    .await
    .unwrap();
    admitted.completion.await.unwrap().unwrap();
    fixture.drained_wait().await;
    assert_eq!(
        fixture.store.sanitized_room_anchor().unwrap(),
        Some(baseline.anchor())
    );
    // Replay uses the separately durable pin, including after reopening.
    let reopened =
        crate::store::WorkspaceStore::open(fixture._tmp.path(), fixture.store.workspace_id(), true)
            .unwrap();
    assert_eq!(
        reopened.sanitized_room_anchor().unwrap(),
        Some(baseline.anchor())
    );
}
