//! A newer connection of the same device takes over its slot: the older
//! session ends as superseded, and only a real protocol failure counts.
use super::*;

/// A newer connection of the same device (a page that reconnected, a second
/// tab) taking the slot ends the older session as the handover it is, not as
/// a protocol failure; a device the room no longer lets in ends as the
/// authorization change it is, replaced or not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_session_ends_as_superseded_unless_its_device_lost_the_room() {
    for revoked in [false, true] {
        let fixture = Fixture::new();
        let device = fixture.recipient.device_public_key;
        let (replacement, _receiver) = mpsc::channel(32);
        fixture.workspace.lock().await.add_subscriber(Subscriber {
            device_public_key: device,
            role: PeerRole::Device,
            tx: replacement,
            disconnect: fixture.recipient_disconnect.clone(),
        });
        if revoked {
            fixture.revoke(device);
        }
        let (state_tx, _state_rx) = mpsc::channel(32);
        let session = fixture.recipient_session(&state_tx);
        let error = audit_subscriber_access(
            &fixture.recipient,
            &session,
            &[fixture.store.workspace_id()],
        )
        .await
        .unwrap_err();
        let lost = lost_session(
            &*fixture.workspace.lock().await,
            &fixture.recipient,
            &session,
            RoomOperation::State,
        );
        if revoked {
            assert!(matches!(error, PeerError::Protocol(_)), "{error}");
            assert!(matches!(lost, PeerError::Protocol(_)), "{lost}");
        } else {
            assert!(matches!(error, PeerError::Superseded), "{error}");
            assert!(matches!(lost, PeerError::Superseded), "{lost}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_completion_only_classifies_a_valid_replacement_as_superseded() {
    enum Mutation {
        Replacement,
        Revocation,
        MembershipRemoval,
        SlotRemoval,
        ReplacementAndRevocation,
    }

    for mutation in [
        Mutation::Replacement,
        Mutation::Revocation,
        Mutation::MembershipRemoval,
        Mutation::SlotRemoval,
        Mutation::ReplacementAndRevocation,
    ] {
        let fixture = Arc::new(Fixture::new());
        let device = fixture.recipient.device_public_key;
        let (release, reached) =
            crate::store::test_barrier::install(fixture.store.root(), "replay-prepare");
        let subscribing = fixture.clone();
        let task = tokio::spawn(async move { subscribing.subscribe_result().await });
        reached.await.unwrap();

        let replacement = if matches!(
            mutation,
            Mutation::Replacement | Mutation::ReplacementAndRevocation
        ) {
            let (replacement, receiver) = mpsc::channel(32);
            fixture.workspace.lock().await.add_subscriber(Subscriber {
                device_public_key: device,
                role: PeerRole::Device,
                tx: replacement.clone(),
                disconnect: fixture.source.disconnect.clone(),
            });
            Some((replacement, receiver))
        } else {
            None
        };
        if matches!(
            mutation,
            Mutation::Revocation | Mutation::ReplacementAndRevocation
        ) {
            fixture.revoke(device);
        }
        if matches!(mutation, Mutation::MembershipRemoval) {
            fixture
                .workspace
                .lock()
                .await
                .store
                .save_membership(&[fixture.source.peer.device_public_key])
                .unwrap();
        }
        if matches!(mutation, Mutation::SlotRemoval) {
            fixture.workspace.lock().await.remove_subscriber(&device);
        }

        drop(release);
        let result = tilo_test_env::within("task to complete", task)
            .await
            .unwrap();
        let Err(error) = result else {
            panic!("mutated replay completion unexpectedly registered")
        };
        if matches!(mutation, Mutation::Replacement) {
            assert!(matches!(error, PeerError::Superseded));
        } else {
            assert!(
                matches!(error, PeerError::Store(_)),
                "authorization loss was hidden by stale-session classification: {error}"
            );
        }
        if let Some((replacement, _receiver)) = replacement {
            assert!(
                fixture
                    .workspace
                    .lock()
                    .await
                    .subscribers
                    .get(&device)
                    .is_some_and(|current| current.tx.same_channel(&replacement)),
                "obsolete replay completion removed the replacement session"
            );
        }
    }
}

/// A device re-dials (its shell follows a changed pin) and drops the old
/// connection. The old connection's teardown releases its slot while the new
/// subscription's replay is still running; the new one must take the slot, not
/// be closed as if it had lost authorization.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_completion_takes_a_slot_its_earlier_connection_released() {
    let fixture = Arc::new(Fixture::new());
    let device = fixture.recipient.device_public_key;
    let (earlier, _earlier_rx) = mpsc::channel(32);
    fixture.workspace.lock().await.add_subscriber(Subscriber {
        device_public_key: device,
        role: PeerRole::Device,
        tx: earlier.clone(),
        disconnect: fixture.recipient_disconnect.clone(),
    });
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "replay-prepare");
    let subscribing = fixture.clone();
    let task = tokio::spawn(async move { subscribing.subscribe_result().await });
    reached.await.unwrap();

    // The earlier connection's teardown: it removes only its own slot.
    fixture
        .workspace
        .lock()
        .await
        .remove_subscriber_session(&device, &earlier);
    drop(release);

    let result = tilo_test_env::within("task to complete", task)
        .await
        .unwrap();
    assert!(
        result.is_ok(),
        "the re-dialled subscription was refused after its earlier connection ended: {:?}",
        result.err()
    );
    assert!(
        fixture
            .workspace
            .lock()
            .await
            .subscribers
            .get(&device)
            .is_some_and(|current| current.tx.same_channel(&fixture.recipient_tx)),
        "the re-dialled subscription holds the device's slot"
    );
}

/// A device whose network dropped dials again while the host still holds
/// its stale connection, mid-replay: the stale one registers first, then
/// the device's new connection, with the same certificate, completes. The
/// new one replaces the stale one; it is not refused as superseded by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnected_device_replaces_its_stale_session_that_registered_first() {
    let fixture = Arc::new(Fixture::new());
    let device = fixture.recipient.device_public_key;
    let root = fixture.store.root().to_path_buf();

    // The stale connection subscribes; its replay is held.
    let (stale_tx, _stale_rx) = mpsc::channel(32);
    let (release_stale, stale_replaying) =
        crate::store::test_barrier::install(&root, "replay-prepare");
    let stale = {
        let fixture = fixture.clone();
        let stale_tx = stale_tx.clone();
        tokio::spawn(async move {
            let (state_tx, _state_rx) = mpsc::channel(32);
            let mut session = fixture.recipient_session(&state_tx);
            session.outbound = &stale_tx;
            let mut replay = None;
            handle_subscribe(
                &fixture.recipient,
                &session,
                &mut Vec::new(),
                &mut replay,
                fixture.store.workspace_id(),
                Subscription::Whole,
            )
            .await
            .map(|_| replay)
        })
    };
    stale_replaying.await.unwrap();

    // The device's new connection is let in while the stale one replays.
    let (admitted, new_admitted) = crate::store::test_barrier::install(&root, "subscribe-admitted");
    let reconnected = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.subscribe_result().await })
    };
    new_admitted.await.unwrap();
    drop(admitted);

    // The stale connection's replay completes and registers first.
    drop(release_stale);
    let stale = tilo_test_env::within("stale subscription", stale)
        .await
        .unwrap();
    assert!(
        matches!(stale, Ok(_) | Err(PeerError::Superseded)),
        "the stale connection ended as something other than a handover: {:?}",
        stale.err()
    );

    let reconnected = tilo_test_env::within("reconnected subscription", reconnected)
        .await
        .unwrap();
    assert!(
        reconnected.is_ok(),
        "the reconnected device was refused in favour of its stale session: {:?}",
        reconnected.err()
    );
    assert!(
        fixture
            .workspace
            .lock()
            .await
            .subscribers
            .get(&device)
            .is_some_and(|current| current.tx.same_channel(&fixture.recipient_tx)),
        "the reconnected connection holds the device's slot"
    );
}

#[test]
fn superseded_peer_outcome_is_quiet_but_protocol_failure_is_counted() {
    let metrics = crate::metrics::Metrics::default();
    assert!(finish_peer_outcome(Err(PeerError::Superseded), &metrics, &[7; 32]).is_ok());
    assert_eq!(metrics.snapshot().subscriber_dropped, 0);

    assert!(
        finish_peer_outcome(
            Err(PeerError::Protocol("authorization changed".into())),
            &metrics,
            &[7; 32],
        )
        .is_err()
    );
    assert_eq!(metrics.snapshot().subscriber_dropped, 1);
}
