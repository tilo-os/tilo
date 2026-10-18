//! Actual signed refused-cut publications through the ordered owner and wire.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use super::*;
use automerge::transaction::Transactable;
use collab_types::CommitNamed as _;
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::StatePeer;

struct Recovery {
    fixture: Fixture,
    initial: Vec<Envelope>,
    successor: Vec<Envelope>,
    tail: Envelope,
    observer: StatePeer,
    now: web_time::SystemTime,
}
fn envelope(author: [u8; 32], room: &str, message: &NeighbourMsg, checkpoint: bool) -> Envelope {
    let channel = neighbour_wire::retention(message, checkpoint).unwrap().0;
    let body = postcard::to_allocvec(message).unwrap();
    let delta = if checkpoint {
        crate::coedit::CoeditPayload::Checkpoint { channel, body }
    } else {
        crate::coedit::CoeditPayload::RetainedDelta { channel, body }
    }
    .encode()
    .unwrap();
    Envelope::Delta {
        workspace_id: crate::room_workspace_id(room).unwrap(),
        author_device_public_key: author,
        authored_at_ns: 1,
        delta,
        signature: [0; 64],
    }
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "test helper keeps the call shape of the fixtures it builds"
)]
fn signed(
    key: &ed25519_dalek::SigningKey,
    room: &str,
    mutation: collab_types::DocumentMutation,
    seq: u64,
    now: web_time::SystemTime,
) -> NeighbourMsg {
    let source = key.verifying_key().to_bytes();
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id("selector.app", room, "one"),
        collab_types::Ed25519PublicKey::from_bytes(source),
        collab_types::document_attestation_id("selector.app", room, "one"),
        collab_types::OpKind::Change,
        now,
        seq,
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(&source),
        app_id: "selector.app".into(),
        cluster_id: room.into(),
        record_id: "one".into(),
        envelope: Box::new(envelope),
    }
}
impl Recovery {
    async fn new() -> Self {
        Box::pin(Self::with_document_count(1)).await
    }
    async fn with_document_count(document_count: usize) -> Self {
        let room_id = uuid::Uuid::new_v4().to_string();
        let room = room_id.as_str();
        let mut fixture = Fixture::with_workspace_id(crate::room_workspace_id(room).unwrap());
        let key = &fixture.source_key;
        let cert = fixture.source.peer.cert.clone();
        let now = web_time::SystemTime::UNIX_EPOCH
            .checked_add(web_time::Duration::from_secs(2000))
            .unwrap();
        let mut owner =
            StatePeer::new(room, cert.clone(), vec![], RevocationList::new(), now).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.set_actor(automerge::ActorId::from(vec![70]));
        document.put(automerge::ROOT, "value", "baseline").unwrap();
        owner
            .install_seed(
                tilo_peer_state::SeedState {
                    room_changes: tilo_peer_state::seed_room(
                        room,
                        &cert,
                        std::slice::from_ref(&cert),
                    )
                    .unwrap(),
                    documents: ["one", "two"]
                        .into_iter()
                        .take(document_count)
                        .map(|record| tilo_peer_state::SeedDocument {
                            app_id: "selector.app".into(),
                            record_id: record.into(),
                            kind: "test".into(),
                            bytes: document.save(),
                        })
                        .collect(),
                },
                key,
                now,
            )
            .unwrap();
        let cp1 = owner.durable_bootstrap_messages(key, now, 1).unwrap();
        let observer = owner.clone();
        let NeighbourMsg::AppDocument { envelope: op, .. } = &cp1[1] else {
            panic!()
        };
        let seq = op.seq.saturating_add(1);
        let baseline = owner.document("selector.app", "one").unwrap();
        let mut a = automerge::AutoCommit::load(&baseline).unwrap();
        a.set_actor(automerge::ActorId::from(vec![71]));
        a.put(automerge::ROOT, "candidate", "A").unwrap();
        a.commit();
        let NeighbourMsg::AppDocument { envelope: op, .. } = signed(
            key,
            room,
            collab_types::DocumentMutation::Snapshot(a.save()),
            seq,
            now,
        ) else {
            panic!()
        };
        owner
            .admit_local_app_document("selector.app", "one", *op, now)
            .unwrap();
        let cp2 = owner.durable_bootstrap_messages(key, now, 2).unwrap();
        let mut b = automerge::AutoCommit::load(&baseline).unwrap();
        b.set_actor(automerge::ActorId::from(vec![72]));
        let heads = b.get_heads();
        b.put(automerge::ROOT, "fallback", "B").unwrap();
        b.commit_edit("fixture", "fallback");
        let tail = signed(
            key,
            room,
            collab_types::DocumentMutation::Incremental(
                b.get_changes(&heads)
                    .iter()
                    .flat_map(|c| c.raw_bytes().to_vec())
                    .collect(),
            ),
            seq,
            now,
        );
        {
            let mut runtime = fixture.workspace.lock().await;
            runtime
                .store
                .bind_shell_authority(
                    tilo_relay_protocol::retention::RetentionAuthority::new(room, cert).unwrap(),
                )
                .unwrap();
            runtime.checkpoint_prefix_recovery =
                crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
            fixture.store = runtime.store.clone();
        }
        Self {
            initial: cp1
                .iter()
                .map(|m| envelope(fixture.source.peer.device_public_key, room, m, true))
                .collect(),
            successor: cp2
                .iter()
                .map(|m| envelope(fixture.source.peer.device_public_key, room, m, true))
                .collect(),
            tail: envelope(fixture.source.peer.device_public_key, room, &tail, false),
            observer,
            now,
            fixture,
        }
    }
    async fn enqueue(&self, frame: Envelope) {
        enqueue_state(
            &self.fixture.workspace,
            frame,
            Some(self.fixture.source.clone()),
            PeerRole::Device,
            self.fixture.metrics.clone(),
        )
        .await
        .unwrap();
    }
    async fn complete(&self) {
        if self.successor.len() > 2 {
            enqueue_checkpoint_prefix(
                &self.fixture.workspace,
                self.successor[1..].to_vec(),
                self.fixture.source.clone(),
                PeerRole::Device,
                self.fixture.metrics.clone(),
                vec![None; self.successor.len().saturating_sub(1)],
            )
            .await
            .unwrap();
        } else {
            self.enqueue(self.successor[1].clone()).await;
        }
    }
    async fn prepare(&mut self) {
        for frame in self.initial.clone() {
            self.enqueue(frame).await;
        }
        self.fixture.drained_wait().await;
        let frames = self.drain().await;
        assert_eq!(frames, self.initial);
        self.enqueue(self.tail.clone()).await;
        self.fixture.drained_wait().await;
        assert_eq!(self.drain().await, vec![self.tail.clone()]);
        self.enqueue(self.successor[0].clone()).await;
        self.fixture.drained_wait().await;
        assert!(
            self.fixture.recipient_rx.try_recv().is_err(),
            "incomplete verified prefix leaked live"
        );
    }
    async fn drain(&mut self) -> Vec<Envelope> {
        let mut frames = Vec::new();
        while let Ok(delivery) = self.fixture.recipient_rx.try_recv() {
            let expected = (*delivery.envelope).clone();
            let mut wire = Vec::new();
            self.fixture.write(delivery, &mut wire).await;
            let mut reader = SessionReader::new(wire.as_slice());
            assert_eq!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(expected.clone()))
            );
            assert!(
                reader.read_next().await.unwrap().is_none(),
                "recovery emitted extra Ready"
            );
            frames.push(expected);
        }
        frames
    }
    fn prove(&mut self, frames: &[Envelope]) {
        use automerge::ReadDoc;
        assert_eq!(
            frames.len(),
            self.initial
                .len()
                .saturating_add(1)
                .saturating_add(self.successor.len())
        );
        assert_eq!(&frames[..self.initial.len()], &self.initial);
        assert_eq!(
            frames[self.initial.len()],
            self.tail,
            "safe tail must precede rejected extras"
        );
        for frame in &self.successor {
            assert_eq!(frames.iter().filter(|f| *f == frame).count(), 1);
        }
        for frame in frames {
            let Envelope::Delta { delta, .. } = frame else {
                panic!()
            };
            let payload = crate::coedit::CoeditPayload::decode(delta).unwrap();
            let body = match payload {
                crate::coedit::CoeditPayload::Checkpoint { body, .. }
                | crate::coedit::CoeditPayload::RetainedDelta { body, .. } => body,
            };
            let message: NeighbourMsg = postcard::from_bytes(&body).unwrap();
            self.observer
                .receive_from(
                    tilo_peer_state::TransportOrigin::RelayDelta(
                        self.fixture.source.peer.device_public_key,
                    ),
                    message,
                    self.now,
                )
                .unwrap();
        }
        let bytes = self.observer.document("selector.app", "one").unwrap();
        let doc = automerge::AutoCommit::load(&bytes).unwrap();
        assert!(
            doc.get(automerge::ROOT, "fallback").unwrap().is_some(),
            "native admission lost protected B tail"
        );
    }
}
#[tokio::test]
async fn recovery_serial_refused_cut_keeps_connection_and_protected_tail() {
    let mut case = Box::pin(Recovery::new()).await;
    case.prepare().await;
    let live = case.fixture.workspace.lock().await.state_sessions
        [&case.fixture.recipient.device_public_key]
        .live
        .clone();
    case.complete().await;
    case.fixture.drained_wait().await;
    let frames = case.drain().await;
    case.prove(&frames);
    let runtime = case.fixture.workspace.lock().await;
    assert!(Arc::ptr_eq(
        &live,
        &runtime.state_sessions[&case.fixture.recipient.device_public_key].live
    ));
    assert!(live.load(std::sync::atomic::Ordering::SeqCst));
    assert!(case.fixture.recipient_disconnected.try_recv().is_err());
}

#[tokio::test]
async fn recovery_hydration_preserves_same_sender_replacement_lease() {
    for count in [1, 2] {
        Box::pin(hydration_replacement(count)).await;
    }
}
async fn hydration_replacement(count: usize) {
    for (fail_after_prefix, before_result) in [(true, true), (false, false), (true, false)] {
        let mut case = Box::pin(Recovery::with_document_count(count)).await;
        case.prepare().await;
        let operation = if before_result {
            "shell-after-current"
        } else {
            "shell-recovery-read"
        };
        let (release, reached) =
            crate::store::test_barrier::install(case.fixture.store.root(), operation);
        case.complete().await;
        tilo_test_env::within("the persistence hook to be reached", reached)
            .await
            .unwrap();
        let first = if before_result {
            assert!(case.fixture.recipient_rx.try_recv().is_err());
            None
        } else {
            Some(
                case.fixture
                    .recipient_rx
                    .try_recv()
                    .expect("first recovery frame queued"),
            )
        };
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (tx, mut rx) = mpsc::channel(32);
        {
            let mut runtime = case.fixture.workspace.lock().await;
            let old = runtime
                .state_sessions
                .get_mut(&case.fixture.recipient.device_public_key)
                .unwrap();
            old.live.store(false, std::sync::atomic::Ordering::SeqCst);
            old.live = live.clone();
            old.tx = tx;
        }
        let fault = fail_after_prefix
            .then(|| crate::store::test_barrier::fail(case.fixture.store.root(), operation));
        drop(release);
        case.fixture.drained_wait().await;
        drop(fault);
        let mut wire = Vec::new();
        if let Some(first) = first {
            case.fixture.write(first, &mut wire).await;
        }
        assert!(
            wire.is_empty(),
            "stale queued prefix wrote into replacement"
        );
        assert!(
            rx.try_recv().is_err(),
            "old recovery leaked remainder into same-sender replacement"
        );
        let runtime = case.fixture.workspace.lock().await;
        assert!(
            runtime
                .state_sessions
                .get(&case.fixture.recipient.device_public_key)
                .is_some_and(|s| Arc::ptr_eq(&s.live, &live)),
            "partial recovery fenced replacement lease"
        );
        assert!(live.load(std::sync::atomic::Ordering::SeqCst));
        assert!(case.fixture.recipient_disconnected.try_recv().is_err());
    }
}

#[tokio::test]
async fn recovery_partial_read_failure_fences_current_and_preserves_replay() {
    for count in [1, 2] {
        for invalidate_source in [false, true] {
            Box::pin(partial_recovery_failure(invalidate_source, count)).await;
        }
    }
}
async fn partial_recovery_failure(invalidate_source: bool, count: usize) {
    let mut case = Box::pin(Recovery::with_document_count(count)).await;
    let source_live = Arc::new(std::sync::atomic::AtomicBool::new(true));
    case.fixture.source.session_live = Some(source_live.clone());
    case.prepare().await;
    let (release, reached) =
        crate::store::test_barrier::install(case.fixture.store.root(), "shell-recovery-read");
    case.complete().await;
    tilo_test_env::within("the persistence hook to be reached", reached)
        .await
        .unwrap();
    assert_eq!(case.drain().await, vec![case.initial[0].clone()]);
    let fault = (!invalidate_source).then(|| {
        crate::store::test_barrier::fail(case.fixture.store.root(), "shell-recovery-read")
    });
    if invalidate_source {
        source_live.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    drop(release);
    case.fixture.drained_wait().await;
    drop(fault);
    assert!(case.fixture.recipient_disconnected.try_recv().is_ok());
    assert!(
        !case
            .fixture
            .workspace
            .lock()
            .await
            .state_sessions
            .contains_key(&case.fixture.recipient.device_public_key)
    );
    let mut wire = Vec::new();
    Box::pin(case.fixture.replay_to(&mut wire)).await.unwrap();
    let mut reader = SessionReader::new(wire.as_slice());
    let mut frames = Vec::new();
    let mut ready = 0_u32;
    while let Some(frame) = reader.read_next().await.unwrap() {
        match frame {
            EnvelopeV2::State(e @ Envelope::Delta { .. }) => frames.push(e),
            EnvelopeV2::State(Envelope::Ready { .. }) => ready = ready.saturating_add(1),
            _ => panic!("unexpected replay output"),
        }
    }
    assert_eq!(ready, 1);
    case.prove(&frames);
}

#[tokio::test]
async fn recovery_cursor_holds_owner_until_done_and_queued_payloads_survive_gc() {
    for count in [1, 2] {
        Box::pin(recovery_gc(count)).await;
    }
}
async fn recovery_gc(count: usize) {
    let mut case = Box::pin(Recovery::with_document_count(count)).await;
    case.prepare().await;
    let (release, reached) =
        crate::store::test_barrier::install(case.fixture.store.root(), "shell-recovery-read");
    case.complete().await;
    tilo_test_env::within("the persistence hook to be reached", reached)
        .await
        .unwrap();
    let selected = case
        .fixture
        .store
        .root()
        .join(crate::store::shell_selection::DIRECTORY);
    let generation = selected.join(std::fs::read_to_string(selected.join("CURRENT")).unwrap());
    assert!(generation.exists());
    let owner = crate::persistence::handle(&case.fixture.workspace).await;
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ran_next = ran.clone();
    let mut next = owner
        .read(1, move |_| {
            ran_next.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut next)
            .await
            .is_err()
    );
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "owner interleaved next generation/GC while cursor owns paths"
    );
    assert!(generation.exists());
    drop(release);
    crate::persistence::receipt(next).await.unwrap();
    // Hold budgeted batches across an actual subsequent selection/GenerationGc.
    let mut deliveries = Vec::new();
    while let Ok(delivery) = case.fixture.recipient_rx.try_recv() {
        deliveries.push(delivery);
    }
    let fallback_len = case
        .initial
        .len()
        .saturating_add(1)
        .saturating_add(case.successor.len());
    assert_eq!(
        deliveries.len(),
        fallback_len.saturating_add(if count > 1 { count } else { 0 })
    );
    case.complete().await;
    case.fixture.drained_wait().await;
    assert!(
        !generation.exists(),
        "subsequent actual selection did not collect old generation"
    );
    let mut frames = Vec::new();
    for delivery in deliveries {
        let expected = (*delivery.envelope).clone();
        let mut wire = Vec::new();
        case.fixture.write(delivery, &mut wire).await;
        let mut reader = SessionReader::new(wire.as_slice());
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(expected.clone()))
        );
        frames.push(expected);
    }
    case.prove(&frames[..fallback_len]);
    assert!(case.fixture.recipient_disconnected.try_recv().is_err());
}

#[tokio::test]
async fn recovery_active_replay_pending_precedes_ready_and_later_live() {
    for count in [1, 2] {
        Box::pin(active_recovery(count)).await;
    }
}
async fn active_recovery(count: usize) {
    use automerge::ReadDoc;
    let mut case = Box::pin(Recovery::with_document_count(count)).await;
    case.prepare().await;
    let fixture = Arc::new(case.fixture);
    let (prepare_release, prepare_reached) =
        crate::store::test_barrier::install(fixture.store.root(), "replay-manifest-prepare");
    let (ready_release, ready_reached) =
        crate::store::test_barrier::install(fixture.store.root(), "replay-before-ready");
    let replaying = fixture.clone();
    let replay = tokio::spawn(async move {
        let mut wire = Vec::new();
        let rx = Box::pin(replaying.replay_to_with_deliveries(&mut wire)).await;
        (rx, wire)
    });
    tilo_test_env::within("the prepare hook to be reached", prepare_reached)
        .await
        .unwrap();
    let live = fixture.workspace.lock().await.state_sessions[&fixture.recipient.device_public_key]
        .live
        .clone();
    if count > 1 {
        enqueue_checkpoint_prefix(
            &fixture.workspace,
            case.successor[1..].to_vec(),
            fixture.source.clone(),
            PeerRole::Device,
            fixture.metrics.clone(),
            vec![None; count],
        )
        .await
        .unwrap();
    } else {
        enqueue_state(
            &fixture.workspace,
            case.successor[1].clone(),
            Some(fixture.source.clone()),
            PeerRole::Device,
            fixture.metrics.clone(),
        )
        .await
        .unwrap();
    }

    fixture.drained_wait().await;
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap()
            .saturating_add(u32::try_from(fixture.count_held()).unwrap()),
        u32::try_from(
            case.initial
                .len()
                .saturating_add(1)
                .saturating_add(case.successor.len())
                .saturating_add(if count > 1 { count } else { 0 })
        )
        .unwrap(),
        "ACTIVE recovery omitted full fallback pending frames"
    );
    drop(prepare_release);
    tilo_test_env::within("the ready hook to be reached", ready_reached)
        .await
        .unwrap();
    let after = Envelope::Delta {
        workspace_id: fixture.store.workspace_id(),
        author_device_public_key: fixture.source.peer.device_public_key,
        authored_at_ns: 999,
        delta: vec![255; 64],
        signature: [0; 64],
    };
    enqueue_state(
        &fixture.workspace,
        after.clone(),
        Some(fixture.source.clone()),
        PeerRole::Device,
        fixture.metrics.clone(),
    )
    .await
    .unwrap();
    fixture.drained_wait().await;
    drop(ready_release);
    let (rx, wire) = replay.await.unwrap();
    let mut rx = rx.unwrap();
    let mut reader = SessionReader::new(wire.as_slice());
    let mut frames = Vec::new();
    let mut ready = 0_u32;
    while let Some(frame) = reader.read_next().await.unwrap() {
        match frame {
            EnvelopeV2::State(e @ Envelope::Delta { .. }) => {
                assert_eq!(ready, 0);
                frames.push(e);
            }
            EnvelopeV2::State(Envelope::Ready { .. }) => ready = ready.saturating_add(1),
            _ => panic!("unexpected replay output"),
        }
    }
    assert_eq!(ready, 1);
    assert!(!frames.contains(&after));
    // Initial detached cut includes CP1/T2 and the held room; actual pending
    // recovery must still contain a coherent CP1/T2/extras suffix before Ready.
    let fallback_len = case
        .initial
        .len()
        .saturating_add(1)
        .saturating_add(case.successor.len());
    assert!(frames.windows(fallback_len).any(|w| {
        w[..case.initial.len()] == case.initial
            && w[case.initial.len()] == case.tail
            && case
                .successor
                .iter()
                .all(|f| w[case.initial.len().saturating_add(1)..].contains(f))
    }));
    for frame in &frames {
        let Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } = frame
        else {
            panic!()
        };
        let body = match crate::coedit::CoeditPayload::decode(delta).unwrap() {
            crate::coedit::CoeditPayload::Checkpoint { body, .. }
            | crate::coedit::CoeditPayload::RetainedDelta { body, .. } => body,
        };
        case.observer
            .receive_from(
                tilo_peer_state::TransportOrigin::RelayDelta(*author_device_public_key),
                postcard::from_bytes(&body).unwrap(),
                case.now,
            )
            .unwrap();
    }
    let doc = automerge::AutoCommit::load(&case.observer.document("selector.app", "one").unwrap())
        .unwrap();
    assert_eq!(
        doc.get_all(automerge::ROOT, "fallback").unwrap().len(),
        1,
        "ACTIVE replay lost or duplicated the protected native business mutation"
    );
    let delivery = rx
        .try_recv()
        .expect("post-boundary original queued after Ready");
    assert_eq!(*delivery.envelope, after);
    fixture.write(delivery, &mut Vec::new()).await;
    assert!(rx.try_recv().is_err());
    assert!(Arc::ptr_eq(
        &live,
        &fixture.workspace.lock().await.state_sessions[&fixture.recipient.device_public_key].live
    ));
    assert!(live.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn recovery_serial_room_then_rejected_two_app_prefix_keeps_originals_and_converges() {
    use automerge::ReadDoc;
    let mut case = Box::pin(Recovery::with_document_count(2)).await;
    assert_eq!(case.initial.len(), 3);
    case.prepare().await;
    case.fixture
        .workspace
        .lock()
        .await
        .checkpoint_prefix_recovery =
        crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
    let live = case.fixture.workspace.lock().await.state_sessions
        [&case.fixture.recipient.device_public_key]
        .live
        .clone();
    let observed = track_checkpoint_batch_modes(case.fixture.store.workspace_id());
    let apps = case.successor[1..].to_vec();
    let backlog = [EnvelopeV2::State(apps[1].clone())];
    assert_eq!(
        eligible_checkpoint_prefix(&apps[0], backlog.iter()),
        2,
        "exercise the actual >=2 distinct route reader grouping"
    );
    let accepted_before = case.fixture.metrics.snapshot().deltas_received;
    enqueue_checkpoint_prefix(
        &case.fixture.workspace,
        apps.clone(),
        case.fixture.source.clone(),
        PeerRole::Device,
        case.fixture.metrics.clone(),
        vec![None; 2],
    )
    .await
    .unwrap();
    case.fixture.drained_wait().await;
    assert_eq!(
        *observed.lock().unwrap(),
        vec![CheckpointBatchMode::AllOriginal],
        "batch dispositions must stay Original"
    );
    assert_eq!(
        case.fixture.metrics.snapshot().deltas_received - accepted_before,
        2,
        "supplemental replay must not count another input acceptance"
    );
    let frames = case.drain().await;
    assert_eq!(
        frames.len(),
        9,
        "one full seven-frame fallback plus two unchanged Original inputs"
    );
    assert!(
        frames.contains(&case.successor[0]),
        "prior serial Suppress room was lost when completing apps entered AllOriginal batch"
    );
    assert_eq!(&frames[..3], &case.initial);
    assert_eq!(
        frames[3], case.tail,
        "safe tail must precede rejected checkpoint extras and Originals"
    );
    assert_eq!(
        &frames[frames.len() - 2..],
        &apps,
        "current Original batch frames were dropped or replaced"
    );
    for frame in &frames {
        let Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } = frame
        else {
            panic!()
        };
        let body = match crate::coedit::CoeditPayload::decode(delta).unwrap() {
            crate::coedit::CoeditPayload::Checkpoint { body, .. }
            | crate::coedit::CoeditPayload::RetainedDelta { body, .. } => body,
        };
        case.observer
            .receive_from(
                tilo_peer_state::TransportOrigin::RelayDelta(*author_device_public_key),
                postcard::from_bytes(&body).unwrap(),
                case.now,
            )
            .unwrap();
    }
    let document = case.observer.document("selector.app", "one").unwrap();
    let doc = automerge::AutoCommit::load(&document).unwrap();
    assert_eq!(
        doc.get_all(automerge::ROOT, "fallback").unwrap().len(),
        1,
        "exact replay duplicates must not duplicate a business mutation"
    );
    let heads = case.observer.document_heads("selector.app", "one").unwrap();
    for frame in &apps {
        let Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } = frame
        else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { body, .. } =
            crate::coedit::CoeditPayload::decode(delta).unwrap()
        else {
            panic!()
        };
        case.observer
            .receive_from(
                tilo_peer_state::TransportOrigin::RelayDelta(*author_device_public_key),
                postcard::from_bytes(&body).unwrap(),
                case.now,
            )
            .unwrap();
    }
    assert_eq!(
        case.observer.document_heads("selector.app", "one").unwrap(),
        heads,
        "retry of Original batch changed native business state twice"
    );
    assert!(Arc::ptr_eq(
        &live,
        &case.fixture.workspace.lock().await.state_sessions
            [&case.fixture.recipient.device_public_key]
            .live
    ));
    assert!(live.load(std::sync::atomic::Ordering::SeqCst));
    assert!(case.fixture.recipient_disconnected.try_recv().is_err());
}

#[tokio::test]
async fn recovery_unknown_app_prefix_cannot_inherit_prior_verified_room_obligation() {
    let mut case = Box::pin(Recovery::with_document_count(2)).await;
    case.prepare().await;
    let mut unknown = case.successor[1..].to_vec();
    for frame in &mut unknown {
        let Envelope::Delta { delta, .. } = frame else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(delta).unwrap()
        else {
            panic!()
        };
        *delta = crate::coedit::CoeditPayload::Checkpoint {
            channel,
            body: vec![255; 64],
        }
        .encode()
        .unwrap();
    }
    enqueue_checkpoint_prefix(
        &case.fixture.workspace,
        unknown.clone(),
        case.fixture.source.clone(),
        PeerRole::Device,
        case.fixture.metrics.clone(),
        vec![None; 2],
    )
    .await
    .unwrap();
    case.fixture.drained_wait().await;
    let mut frames = Vec::new();
    while let Ok(delivery) = case.fixture.recipient_rx.try_recv() {
        assert!(
            matches!(delivery.envelope, LiveEnvelope::Original(_)),
            "unproved inputs triggered selected recovery"
        );
        frames.push((*delivery.envelope).clone());
        case.fixture.write(delivery, &mut Vec::new()).await;
    }
    assert_eq!(
        frames, unknown,
        "unknown current inputs must preserve their Original contract"
    );
    assert!(case.fixture.recipient_disconnected.try_recv().is_err());
}

#[tokio::test]
async fn recovery_supplemental_worker_acquisition_error_preserves_replacement_lease() {
    let mut case = Box::pin(Recovery::with_document_count(2)).await;
    case.prepare().await;
    let (release, reached) =
        crate::store::test_barrier::install(case.fixture.store.root(), "shell-after-current");
    case.complete().await;
    tilo_test_env::within("the persistence hook to be reached", reached)
        .await
        .unwrap();
    let replacement = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (tx, mut rx) = mpsc::channel(32);
    {
        let mut runtime = case.fixture.workspace.lock().await;
        let old = runtime
            .state_sessions
            .get_mut(&case.fixture.recipient.device_public_key)
            .unwrap();
        old.live.store(false, std::sync::atomic::Ordering::SeqCst);
        old.live = replacement.clone();
        old.tx = tx;
    }
    // Real acquisition error on the next ordered quantum, distinct from an
    // operation returning an I/O error inside a successful blocking worker.
    case.fixture.workers.close();
    drop(release);
    let owner = crate::persistence::handle(&case.fixture.workspace).await;
    let done = owner
        .submit_resumable_wait(
            1,
            async { Ok(()) },
            |_, _| Ok(Step::Done(())),
            |r| async { r },
        )
        .await
        .unwrap();
    assert!(
        tilo_test_env::within("the persistence receipt", crate::persistence::receipt(done))
            .await
            .is_err()
    );
    assert!(rx.try_recv().is_err());
    assert!(
        case.fixture
            .workspace
            .lock()
            .await
            .state_sessions
            .get(&case.fixture.recipient.device_public_key)
            .is_some_and(|session| Arc::ptr_eq(&session.live, &replacement)),
        "outer worker error used sender-only fence for supplemental recovery"
    );
    assert!(replacement.load(std::sync::atomic::Ordering::SeqCst));
    assert!(case.fixture.recipient_disconnected.try_recv().is_err());
}
