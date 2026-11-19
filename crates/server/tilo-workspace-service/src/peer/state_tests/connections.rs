//! Each connection of a device holds its own subscription: a sibling
//! connection entering leaves it be, and only the workspace dropping the
//! device (revoked, removed) or the connection ending takes it away.
use super::*;

/// A second connection of the fixture's source device, subscribed beside its
/// first and taking live changes on a state stream of its own.
struct Sibling {
    tx: mpsc::Sender<Envelope>,
    _rx: mpsc::Receiver<Envelope>,
    disconnect: mpsc::Sender<()>,
    deliveries: mpsc::Receiver<StateDelivery>,
}

impl Sibling {
    async fn subscribe(fixture: &Fixture) -> Self {
        let device = fixture.source.peer.device_public_key;
        let (tx, rx) = mpsc::channel(32);
        let (disconnect, _) = mpsc::channel(1);
        let (delivery, deliveries) = mpsc::channel(32);
        let mut runtime = fixture.workspace.lock().await;
        runtime.add_subscriber(Subscriber {
            device_public_key: device,
            tx: tx.clone(),
            disconnect: disconnect.clone(),
            content_waiting: Arc::default(),
        });
        runtime.set_state_session(
            &device,
            &tx,
            StateSession {
                peer: fixture.source.peer.clone(),
                tx: delivery,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
                channel_reader: false,
            },
        );
        Self {
            tx,
            _rx: rx,
            disconnect,
            deliveries,
        }
    }
}

fn committed(answer: &Result<Option<Envelope>, PeerError>, expected: [u8; 16]) -> bool {
    matches!(answer, Ok(Some(Envelope::Committed { op, .. })) if *op == expected)
}

/// Two connections of one device (a second tab, an agent's session beside its
/// person's page) write at once: both writes land, each connection is sent the
/// other's change and not its own, and another device of the workspace keeps
/// receiving both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_connections_of_one_device_write_at_once_and_both_land() {
    let mut fixture = Fixture::new();
    let mut sibling = Sibling::subscribe(&fixture).await;
    let first = fixture.ctx(Side::Source, None);
    let second = fixture.connection_ctx(&sibling.tx, &sibling.disconnect, None);
    let peer = &fixture.source.peer;
    let (a, b) = tilo_test_env::within("both commits", async {
        tokio::join!(
            handle_commit(peer, &first, &[WS], fixture.commit(1, [1; 16])),
            handle_commit(peer, &second, &[WS], fixture.commit(2, [2; 16])),
        )
    })
    .await;
    assert!(committed(&a, [1; 16]), "{a:?}");
    assert!(committed(&b, [2; 16]), "{b:?}");
    for connection in [&first, &second] {
        assert!(
            audit_subscriber_access(peer, connection, &mut vec![WS])
                .await
                .is_ok(),
            "a sibling connection's writes ended this one"
        );
    }
    let delivered = |delivery: StateDelivery| match &*delivery.envelope {
        Envelope::Delta { delta, .. } => delta.clone(),
        other => panic!("not a change: {other:?}"),
    };
    let Envelope::Delta { delta: one, .. } = fixture.envelope(1) else {
        unreachable!()
    };
    let Envelope::Delta { delta: two, .. } = fixture.envelope(2) else {
        unreachable!()
    };
    let mut other_device = Vec::new();
    tilo_test_env::within("the other device receives both", async {
        while other_device.len() < 2 {
            other_device.push(delivered(fixture.recipient_rx.recv().await.unwrap()));
        }
    })
    .await;
    other_device.sort();
    let mut both = vec![one.clone(), two];
    both.sort();
    assert_eq!(other_device, both);
    let to_sibling = tilo_test_env::within("the sibling is sent the first's change", async {
        delivered(sibling.deliveries.recv().await.unwrap())
    })
    .await;
    assert_eq!(to_sibling, one, "the sibling was sent its own change back");
    assert!(sibling.deliveries.try_recv().is_err());
}

/// A device revoked while two of its connections are live: each is refused
/// on its next write, and each ends at its next access check, whichever of
/// them entered last.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_device_is_refused_on_every_connection() {
    let fixture = Fixture::new();
    let sibling = Sibling::subscribe(&fixture).await;
    let first = fixture.ctx(Side::Source, None);
    let second = fixture.connection_ctx(&sibling.tx, &sibling.disconnect, None);
    let peer = &fixture.source.peer;
    fixture.revoke_source();
    for (connection, op) in [(&first, [1; 16]), (&second, [2; 16])] {
        let answer = handle_commit(peer, connection, &[WS], fixture.commit(1, op)).await;
        assert!(
            matches!(answer, Ok(Some(Envelope::CommitRefused { .. })) | Err(_)),
            "a revoked device's write was taken: {answer:?}"
        );
        let audit = audit_subscriber_access(peer, connection, &mut vec![WS]).await;
        assert!(
            matches!(audit, Err(PeerError::Protocol(_))),
            "a revoked device's connection stayed: {audit:?}"
        );
    }
    assert!(fixture.store.load_retained().unwrap().is_empty());
}

/// A connection that ends drops only its own subscription: the device's other
/// connection stays subscribed and the device stays on the workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_ending_leaves_its_siblings_subscription() {
    let fixture = Fixture::new();
    let sibling = Sibling::subscribe(&fixture).await;
    let device = fixture.source.peer.device_public_key;
    fixture
        .workspace
        .lock()
        .await
        .remove_subscriber_session(&device, &fixture.source.tx);
    let runtime = fixture.workspace.lock().await;
    assert!(!runtime.current_session(&device, &fixture.source.tx));
    assert!(runtime.current_session(&device, &sibling.tx));
    assert!(runtime.has_subscriber(&device));
}

/// A device that re-dials while its old connection still replays: both
/// subscriptions complete, neither refused for the other, and the new one
/// stays once the old one's teardown is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnected_device_and_its_stale_connection_both_subscribe() {
    let fixture = Arc::new(Fixture::new());
    let device = fixture.recipient.device_public_key;
    let root = fixture.store.root().to_path_buf();

    let (stale_tx, _stale_rx) = mpsc::channel(32);
    let (release_stale, stale_replaying) =
        crate::store::test_barrier::install(&root, "replay-prepare");
    let stale = {
        let fixture = fixture.clone();
        let stale_tx = stale_tx.clone();
        tokio::spawn(async move {
            let (state_tx, _state_rx) = mpsc::channel(32);
            let mut session = fixture.ctx(Side::Recipient, Some(&state_tx));
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
    let reconnected = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.subscribe_result().await })
    };
    drop(release_stale);
    let stale = tilo_test_env::within("stale subscription", stale)
        .await
        .unwrap();
    assert!(stale.is_ok(), "{:?}", stale.err());
    let reconnected = tilo_test_env::within("reconnected subscription", reconnected)
        .await
        .unwrap();
    assert!(reconnected.is_ok(), "{:?}", reconnected.err());

    fixture
        .workspace
        .lock()
        .await
        .remove_subscriber_session(&device, &stale_tx);
    assert!(
        fixture
            .workspace
            .lock()
            .await
            .current_session(&device, &fixture.recipient_tx),
        "the stale connection's teardown took the new one's subscription"
    );
}

#[test]
fn a_refusal_is_quiet_but_a_protocol_failure_is_counted() {
    let metrics = crate::metrics::Metrics::default();
    assert!(finish_peer_outcome(Ok(()), &metrics, &[7; 32]).is_ok());
    assert_eq!(metrics.snapshot().subscriber_dropped, 0);

    // A device the server does not admit ended with a refusal, not an error:
    // it is neither reported as a failed session nor counted as a lost one.
    assert!(finish_peer_outcome(Err(PeerError::NotAdmitted), &metrics, &[7; 32]).is_ok());
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

/// Commits become durable together and finish in any order, but the
/// workspace's governor takes them in the order they were admitted, and an
/// author hears that a commit is in only once the governor's copy holds it:
/// a copy told of a later write first passed the earlier one over for good,
/// and then refused the compaction that covered it.
#[tokio::test]
async fn the_governor_takes_commits_in_the_order_they_were_admitted() {
    let fixture = Fixture::new();
    let (tap, mut notices) = crate::workspace::governor_tap(8);
    fixture.workspace.lock().await.governor_tap = Some(tap);
    let session = fixture.ctx(Side::Source, None);
    let body = |kind: u8| governor_notice(&fixture.envelope(kind)).unwrap();
    let mut first = Box::pin(
        admitted(&fixture, &session, 1, [1; 16])
            .await
            .answer(&session),
    );
    let mut second = Box::pin(
        admitted(&fixture, &session, 2, [2; 16])
            .await
            .answer(&session),
    );

    // The later commit, durable on its own, waits for the earlier one.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut second)
            .await
            .is_err(),
        "a later commit was answered before an earlier one was told to the governor"
    );
    assert!(
        notices.try_recv().is_err(),
        "the governor was told of a later commit first"
    );

    let notice = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        tokio::select! {
            _ = &mut first => panic!("a commit was answered before the governor took it"),
            notice = notices.recv() => notice.unwrap(),
        }
    })
    .await
    .unwrap();
    assert_eq!(notice.body, body(1));
    notice.taken.send(()).unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(30), first)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(first, Some(Envelope::Committed { op, .. }) if op == [1; 16]));

    let notice = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        tokio::select! {
            _ = &mut second => panic!("a commit was answered before the governor took it"),
            notice = notices.recv() => notice.unwrap(),
        }
    })
    .await
    .unwrap();
    assert_eq!(notice.body, body(2));
    notice.taken.send(()).unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(30), second)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(second, Some(Envelope::Committed { op, .. }) if op == [2; 16]));
}

/// The fixture's commit `kind`, admitted and not answered yet.
async fn admitted(
    fixture: &Fixture,
    session: &SessionCtx<'_>,
    kind: u8,
    op: [u8; 16],
) -> super::super::commit::CommitAdmission {
    let admission = admit_commit(
        &fixture.source.peer,
        session,
        &[WS],
        fixture.commit(kind, op),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            admission,
            super::super::commit::CommitAdmission::Admitted(_)
        ),
        "the commit was answered at admission"
    );
    admission
}
