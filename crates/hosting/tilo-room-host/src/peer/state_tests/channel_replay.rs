//! What a replay carries of the workspace's channel governance: a device is
//! replayed only what it can use. A Welcome reaches the devices it adds, once;
//! a device that admits no one is sent no member's request to be admitted.
use super::*;

/// A commit on a fresh channel adding `added`, the copy that carries what
/// they join from, framed as the fixture's source publishes it.
fn welcome(fixture: &Fixture, authored_at_ns: u64, added: [u8; 32]) -> Envelope {
    use collab_types::{JoinMaterial, Joining, MlsGovernanceAction, MlsGovernanceFrame};
    let message = neighbour_wire::NeighbourMsg::MlsGovernance {
        workspace: "room".into(),
        frame: Box::new(
            MlsGovernanceFrame::signed(
                collab_types::ChannelId::new(),
                [1; 32],
                1,
                MlsGovernanceAction::Commit {
                    removed: Vec::new(),
                    added: vec![collab_types::Ed25519PublicKey::from_bytes(added)],
                    commit: vec![2],
                    joining: Some(Joining::Material(JoinMaterial {
                        welcome: vec![3; 64],
                        key_history: vec![4; 16],
                    })),
                    history: None,
                },
                &fixture.source_key,
            )
            .unwrap(),
        ),
        delivery: 0,
    };
    assert!(neighbour_wire::audience(&message).is_some());
    let (channel, _) = neighbour_wire::retention(&message, false).unwrap();
    fixture.checkpoint_frame(
        authored_at_ns,
        channel,
        postcard::to_stdvec(&message).unwrap(),
    )
}

/// The deltas a replay to the fixture's recipient writes, until its `Ready`.
/// The session says first that its device only follows its channel groups
/// when `reader`.
async fn replayed(fixture: &Fixture, reader: bool) -> Vec<Envelope> {
    let (state_tx, _state_rx) = mpsc::channel(32);
    let session = fixture.ctx(Side::Recipient, Some(&state_tx), None, None);
    let mut wire = Vec::new();
    let mut subscribed = Vec::new();
    if reader {
        Box::pin(handle_inbound_frame(
            Envelope::ChannelReader,
            &fixture.recipient,
            &session,
            &mut subscribed,
            &mut wire,
        ))
        .await
        .unwrap();
    }
    Box::pin(handle_inbound_frame(
        Envelope::Subscribe {
            workspace_id: fixture.store.workspace_id(),
        },
        &fixture.recipient,
        &session,
        &mut subscribed,
        &mut wire,
    ))
    .await
    .unwrap();
    let mut reader = SessionReader::new(wire.as_slice());
    let mut deltas = Vec::new();
    loop {
        match reader.read_next().await.unwrap() {
            Some(EnvelopeV2::State(envelope @ Envelope::Delta { .. })) => deltas.push(envelope),
            Some(EnvelopeV2::State(Envelope::Ready { .. })) => return deltas,
            Some(_) => {}
            None => panic!("the replay ended without its Ready"),
        }
    }
}

fn count(replayed: &[Envelope], frame: &Envelope) -> usize {
    replayed.iter().filter(|written| *written == frame).count()
}

/// A page, which only follows its channel groups, is replayed no member's
/// request to be admitted (a newcomer to a workspace of hundreds was sent one
/// per member) and no other device's Welcome; its own Welcome it is, once.
/// A device that admits others is replayed every request.
#[tokio::test]
async fn a_replay_carries_only_the_channel_governance_its_device_uses() {
    for reader in [true, false] {
        let fixture = Fixture::new();
        let request = access_request_frame(&fixture, 1);
        let others = welcome(&fixture, 2, [9; 32]);
        let own = welcome(&fixture, 3, fixture.recipient.device_public_key);
        for frame in [&request, &others, &own] {
            fixture.publish_checkpoint_frame(frame.clone(), true).await;
        }
        assert_eq!(fixture.store.load_retained().unwrap().len(), 3);
        let replayed = replayed(&fixture, reader).await;
        assert_eq!(
            count(&replayed, &request),
            usize::from(!reader),
            "reader {reader}"
        );
        assert_eq!(count(&replayed, &others), 0, "reader {reader}");
        assert_eq!(count(&replayed, &own), 1, "reader {reader}");
    }
}

/// The Welcome committed for a device the door admitted goes ahead of its
/// replay, and its retained copy is not sent again.
#[tokio::test]
async fn a_newcomers_welcome_is_written_once() {
    let fixture = Fixture::new();
    let device = fixture.recipient.device_public_key;
    fixture.workspace.lock().await.arriving.expect(device);
    // Committed as the device's entry asked: kept for it until it subscribes.
    let own = welcome(&fixture, 1, device);
    fixture.publish_checkpoint_frame(own.clone(), true).await;
    let replayed = replayed(&fixture, true).await;
    assert_eq!(count(&replayed, &own), 1);
    assert_eq!(replayed.first(), Some(&own), "the Welcome goes first");
}

/// Live, too, a device that only follows its channel groups is sent no
/// member's request to be admitted; every other frame still reaches it.
#[tokio::test]
async fn a_reader_is_sent_no_request_live() {
    let mut fixture = Fixture::new();
    fixture
        .workspace
        .lock()
        .await
        .state_sessions
        .get_mut(&fixture.recipient.device_public_key)
        .unwrap()
        .channel_reader = true;
    enqueue_state(
        &fixture.workspace,
        access_request_frame(&fixture, 1),
        Some(fixture.source.clone()),
        fixture.metrics.clone(),
    )
    .await
    .unwrap();
    fixture.enqueue(2).await;
    fixture.drained_wait().await;
    assert_eq!(delivered_times(&mut fixture), [2]);
}
