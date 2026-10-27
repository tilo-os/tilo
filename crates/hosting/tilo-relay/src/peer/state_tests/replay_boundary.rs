//! Live deliveries that reach a session between its subscribe replay's final
//! cut and its `Ready`, while the room keeps committing.
use super::*;

/// Put the recipient's session between a replay's final cut and its `Ready`,
/// with a lag to resume from when `lag` is set (as every connection's session
/// has); returns its live flag and that lag.
async fn at_replay_boundary(
    fixture: &Fixture,
    lag: bool,
) -> (Arc<std::sync::atomic::AtomicBool>, Option<Arc<Lag>>) {
    let mut runtime = fixture.workspace.lock().await;
    let session = runtime
        .state_sessions
        .get_mut(&fixture.recipient.device_public_key)
        .unwrap();
    let catchup = Arc::new(ReplayCatchup::new(
        &fixture.store.root().join(crate::store::replay::WORK_DIR),
    ));
    catchup
        .phase
        .store(ReplayCatchup::BOUNDARY, std::sync::atomic::Ordering::SeqCst);
    session.replay = Some(catchup);
    if lag {
        session.lag = Some(Arc::new(Lag::default()));
    }
    (session.live.clone(), session.lag.clone())
}

/// A delivery of another room taking the recipient's only queue slot.
async fn fill_with_cross_workspace(fixture: &Fixture) {
    let runtime = fixture.workspace.lock().await;
    let session = &runtime.state_sessions[&fixture.recipient.device_public_key];
    let mut cross_workspace = fixture.envelope(3);
    if let Envelope::Delta { workspace_id, .. } = &mut cross_workspace {
        *workspace_id = 99;
    }
    session
        .tx
        .try_send(StateDelivery {
            envelope: LiveEnvelope::Original(Arc::new(cross_workspace)),
            cleanup: None,
            source: None,
            _budget: None,
            live: session.live.clone(),
            fanned: None,
        })
        .unwrap();
}

#[tokio::test]
async fn replay_boundary_full_live_queue_without_lag_fences_exact_session() {
    let mut fixture = Fixture::with_delivery_capacity(1);
    fill_with_cross_workspace(&fixture).await;
    let (live, _) = at_replay_boundary(&fixture, false).await;
    fixture.enqueue(4).await;
    fixture.drained().await;
    assert!(!live.load(std::sync::atomic::Ordering::SeqCst));
    tilo_test_env::within(
        "boundary overflow did not fence its connection",
        fixture.recipient_disconnected.recv(),
    )
    .await;
}

#[tokio::test]
async fn replay_boundary_live_byte_exhaustion_without_lag_fences_exact_session() {
    let mut fixture = Fixture::new();
    let (live, _) = at_replay_boundary(&fixture, false).await;
    let fault = crate::store::test_barrier::fail(fixture.store.root(), "live-delivery-budget");
    fixture.enqueue(3).await;
    fixture.drained().await;
    drop(fault);
    assert!(!live.load(std::sync::atomic::Ordering::SeqCst));
    assert!(fixture.recipient_rx.try_recv().is_err());
    tilo_test_env::within(
        "boundary byte exhaustion did not fence its connection",
        fixture.recipient_disconnected.recv(),
    )
    .await;
}

#[tokio::test]
async fn replay_boundary_full_live_queue_lags_instead_of_ending_the_session() {
    let mut fixture = Fixture::with_delivery_capacity(1);
    let (live, lag) = at_replay_boundary(&fixture, true).await;
    // The room keeps committing while the replay waits to write `Ready`.
    for kind in 3..6 {
        fixture.enqueue(kind).await;
    }
    fixture.drained().await;
    assert!(live.load(std::sync::atomic::Ordering::SeqCst));
    assert!(fixture.recipient_disconnected.try_recv().is_err());
    assert_eq!(
        lag.unwrap().next(),
        Some(7),
        "the room resumes after `Ready`"
    );
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        3,
        "every delivery stays on disk until written, the two the queue could not take included"
    );
}

#[tokio::test]
async fn replay_boundary_live_byte_exhaustion_lags_instead_of_ending_the_session() {
    let mut fixture = Fixture::new();
    let (live, lag) = at_replay_boundary(&fixture, true).await;
    let fault = crate::store::test_barrier::fail(fixture.store.root(), "live-delivery-budget");
    fixture.enqueue(3).await;
    fixture.drained().await;
    drop(fault);
    assert!(live.load(std::sync::atomic::Ordering::SeqCst));
    assert!(fixture.recipient_rx.try_recv().is_err());
    assert!(fixture.recipient_disconnected.try_recv().is_err());
    assert_eq!(
        lag.unwrap().next(),
        Some(7),
        "the room resumes after `Ready`"
    );
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        1
    );
}

/// Start a subscribe replay of one pending delivery and hold it right before
/// its `Ready`; returns the release and the replay's task.
async fn replay_held_before_ready(
    fixture: &Arc<Fixture>,
) -> (
    crate::store::test_barrier::Release,
    tokio::task::JoinHandle<(Result<(), PeerError>, Vec<u8>)>,
) {
    fixture
        .store
        .enqueue_pending(
            &fixture.recipient.device_public_key,
            1,
            &crate::transport::encode(&fixture.envelope(3)).unwrap(),
        )
        .unwrap();
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "replay-before-ready");
    let replaying = fixture.clone();
    let task = tokio::spawn(async move {
        let mut wire = Vec::new();
        let result = Box::pin(replaying.replay_to(&mut wire)).await;
        (result, wire)
    });
    reached.await.unwrap();
    (release, task)
}

/// Whether `wire` carries a `Ready`.
async fn wrote_ready(wire: &[u8]) -> bool {
    let mut reader = SessionReader::new(wire);
    let mut ready = false;
    while let Some(frame) = reader.read_next().await.unwrap() {
        ready |= matches!(frame, EnvelopeV2::State(Envelope::Ready { .. }));
    }
    ready
}

#[tokio::test]
async fn overflow_before_final_ready_lags_and_ready_still_follows() {
    let fixture = Arc::new(Fixture::new());
    let (release, task) = replay_held_before_ready(&fixture).await;
    let (state_tx, live, lag) = {
        let runtime = fixture.workspace.lock().await;
        let session = &runtime.state_sessions[&fixture.recipient.device_public_key];
        (
            session.tx.clone(),
            session.live.clone(),
            session.lag.clone().unwrap(),
        )
    };
    // The live queue is not written until `Ready`: fill it, then the room
    // commits once more.
    while state_tx
        .try_send(StateDelivery {
            envelope: LiveEnvelope::Original(Arc::new(fixture.envelope(5))),
            cleanup: None,
            source: None,
            _budget: None,
            live: live.clone(),
            fanned: None,
        })
        .is_ok()
    {}
    fixture.enqueue(4).await;
    fixture.drained().await;
    assert!(
        live.load(std::sync::atomic::Ordering::SeqCst),
        "a full live queue before `Ready` ended the replay"
    );
    drop(release);
    let (result, wire) = task.await.unwrap();
    result.unwrap();
    assert!(wrote_ready(&wire).await, "the replay answered `Ready`");
    assert_eq!(lag.next(), Some(7), "the room resumes after `Ready`");
}

#[tokio::test]
async fn replay_fenced_before_final_ready_never_writes_ready() {
    let fixture = Arc::new(Fixture::new());
    let (release, task) = replay_held_before_ready(&fixture).await;
    // The device leaves the room while its replay waits to write `Ready`.
    let live = {
        let mut runtime = fixture.workspace.lock().await;
        let live = runtime.state_sessions[&fixture.recipient.device_public_key]
            .live
            .clone();
        runtime.remove_subscriber(&fixture.recipient.device_public_key);
        live
    };
    assert!(
        !live.load(std::sync::atomic::Ordering::SeqCst),
        "leaving the room did not fence the replay lease"
    );
    drop(release);
    let (result, wire) = task.await.unwrap();
    assert!(result.is_err());
    assert!(!wrote_ready(&wire).await, "fenced replay emitted Ready");
}
