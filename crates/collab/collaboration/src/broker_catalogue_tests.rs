//! The action catalogue over the broker: what its first snapshot costs the
//! bridge's budget, how a busy owner's changes coalesce, and how a change
//! that does not apply asks for the whole catalogue again.

use super::*;

#[test]
fn initial_action_catalogue_admission_accounts_for_retained_payload() {
    let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
    let state = BridgeState::new();
    let inbound = InboundBridge::new(tx, state.clone());
    let initial = notebook_action_catalogue_fixture(Uuid::nil(), 1, 1, &"r".repeat(700 * 1024));
    admit_initial_action_catalogue(&initial, &inbound, None).unwrap();
    let queued = rx.try_recv().expect("initial catalogue reaches the owner");
    assert_eq!(event_kind(&queued.event), "ActionCatalogue");
    let retained = event_bytes(&queued.event).unwrap();
    assert_eq!(
        state.catalogue_event_bytes.0.load(Ordering::Acquire),
        retained
    );
    assert_eq!(
        state.event_bytes.0.load(Ordering::Acquire),
        0,
        "a catalogue is charged to its own budget, not the control events'"
    );
    assert!(retained >= 700 * 1024);
    drop(queued);
    assert_eq!(state.catalogue_event_bytes.0.load(Ordering::Acquire), 0);
    assert!(state.is_healthy());

    let oversized = notebook_action_catalogue_fixture(
        Uuid::nil(),
        1,
        1,
        &"r".repeat(SDK_CATALOGUE_EVENT_BYTES),
    );
    assert!(
        admit_initial_action_catalogue(&oversized, &inbound, None).is_err(),
        "a change cannot be dropped, so one that never fits fails the bridge"
    );
    assert!(!state.is_healthy());
}

/// A catalogue source that answers each read with the whole catalogue of
/// connection `reads + 1`: `initial` is connection 1, so a read ahead of
/// the owner would be visible in the connection it names.
struct NextWindow {
    reads: std::rc::Rc<std::cell::Cell<u64>>,
}

impl CatalogueSource for NextWindow {
    async fn read(&mut self) -> Result<String, ()> {
        let next = self.reads.get() + 1;
        self.reads.set(next);
        Ok(notebook_action_catalogue_fixture(
            Uuid::nil(),
            1,
            next,
            "notebook",
        ))
    }

    async fn restart(&mut self) -> Result<(), ()> {
        Err(())
    }
}

/// A catalogue source a test scripts: what each read answers, and the
/// whole catalogue each restart asks for.
struct ScriptedCatalogue {
    changes: std::collections::VecDeque<Result<String, ()>>,
    snapshots: std::collections::VecDeque<String>,
    restarts: std::rc::Rc<std::cell::Cell<usize>>,
}

impl CatalogueSource for ScriptedCatalogue {
    async fn read(&mut self) -> Result<String, ()> {
        self.changes.pop_front().unwrap_or(Err(()))
    }

    async fn restart(&mut self) -> Result<(), ()> {
        self.restarts.set(self.restarts.get() + 1);
        let Some(snapshot) = self.snapshots.pop_front() else {
            return Err(());
        };
        self.changes.push_front(Ok(snapshot));
        Ok(())
    }
}

/// A headless worker is drained only between pumps. Catalogue changes
/// (one per host publication) must not pile up in its bounded bridge:
/// one stays queued, and the broker composes the rest into the next.
#[test]
fn action_catalogue_reader_keeps_one_change_queued_for_a_busy_owner() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
    let state = BridgeState::new();
    let inbound = InboundBridge::new(tx, state.clone());
    let reads = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let counted = reads.clone();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let forwarder = inbound.clone();
        tokio::task::spawn_local(async move {
            forward_action_catalogues(
                NextWindow { reads: counted },
                notebook_action_catalogue_fixture(Uuid::nil(), 1, 1, "notebook"),
                &forwarder,
                Arc::new(tokio::sync::Semaphore::new(1)),
            )
            .await;
        });
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert!(state.is_healthy(), "a busy owner does not fail the bridge");
        let first = rx.try_recv().expect("one catalogue queued");
        assert!(
            rx.try_recv().is_err(),
            "only one catalogue change is queued at a time"
        );
        drop(first);
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            rx.try_recv(),
            Ok(QueuedCollabEvent {
                event: CollabEvent::ActionCatalogue { ref change },
                ..
            }) if change.surfaces[0].surface.peer_id == 2
        ));
        assert!(state.is_healthy());
    });
    assert_eq!(reads.get(), 2, "no read while a catalogue is queued");
}

/// A change the follower cannot apply — the revisions do not join, a shape
/// it was never sent, bytes that do not decode — asks the broker for the
/// whole catalogue: the follower keeps the last one it applied, and the
/// answer is forwarded as a reset. It is never left with an empty one.
#[test]
fn a_catalogue_change_that_does_not_apply_asks_for_the_whole_catalogue() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
    let state = BridgeState::new();
    let inbound = InboundBridge::new(tx, state.clone());
    let initial = notebook_action_catalogue_fixture(Uuid::nil(), 1, 1, "notebook");
    let mut gap: CatalogueChange = serde_json::from_str(&initial).unwrap();
    gap.reset = false;
    gap.since = 5;
    let restarts = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counted = restarts.clone();
    let source = ScriptedCatalogue {
        changes: std::collections::VecDeque::from([
            Ok(serde_json::to_string(&gap).unwrap()),
            Ok("{not a catalogue".to_string()),
        ]),
        snapshots: std::collections::VecDeque::from([
            notebook_action_catalogue_fixture(Uuid::nil(), 1, 2, "notebook"),
            notebook_action_catalogue_fixture(Uuid::nil(), 1, 3, "notebook"),
        ]),
        restarts: counted,
    };
    let local = tokio::task::LocalSet::new();
    let admitted = initial.clone();
    local.block_on(&runtime, async move {
        let forwarder = inbound.clone();
        tokio::task::spawn_local(async move {
            forward_action_catalogues(
                source,
                initial,
                &forwarder,
                Arc::new(tokio::sync::Semaphore::new(1)),
            )
            .await;
        });
        let mut held = ActionCatalogue::default();
        held.apply(&serde_json::from_str(&admitted).unwrap())
            .expect("the initial catalogue");
        for peer in [2, 3] {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            let Ok(QueuedCollabEvent {
                event: CollabEvent::ActionCatalogue { change },
                ..
            }) = rx.try_recv()
            else {
                panic!("the follower is sent connection {peer}'s catalogue whole");
            };
            assert!(change.reset, "each answer is the whole catalogue");
            assert_eq!(change.surfaces[0].surface.peer_id, peer);
            held.apply(&change).unwrap();
            assert!(
                !held.surfaces().is_empty(),
                "the follower never holds an empty catalogue"
            );
        }
        assert_eq!(restarts.get(), 2, "a gap and a corrupt change are re-asked");
        assert!(state.is_healthy());
    });
}

/// A whole catalogue that does not apply to what is held is a fault on
/// the wire, not a follower that fell behind: it ends the stream instead
/// of asking for the whole catalogue again and again.
#[test]
fn a_whole_catalogue_that_does_not_apply_ends_the_stream() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
    let state = BridgeState::new();
    let inbound = InboundBridge::new(tx, state.clone());
    let initial = notebook_action_catalogue_fixture(Uuid::nil(), 1, 1, "notebook");
    let mut gap: CatalogueChange = serde_json::from_str(&initial).unwrap();
    gap.reset = false;
    gap.since = 5;
    let mut broken: CatalogueChange = serde_json::from_str(&notebook_action_catalogue_fixture(
        Uuid::nil(),
        1,
        2,
        "notebook",
    ))
    .unwrap();
    broken.templates.clear();
    let restarts = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counted = restarts.clone();
    let source = ScriptedCatalogue {
        changes: std::collections::VecDeque::from([Ok(serde_json::to_string(&gap).unwrap())]),
        snapshots: std::collections::VecDeque::from([serde_json::to_string(&broken).unwrap()]),
        restarts: counted,
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let forwarder = inbound.clone();
        let asked = restarts.clone();
        tokio::task::spawn_local(async move {
            forward_action_catalogues(
                source,
                initial,
                &forwarder,
                Arc::new(tokio::sync::Semaphore::new(1)),
            )
            .await;
        });
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert_eq!(asked.get(), 1, "the whole catalogue is asked for once");
        assert!(
            rx.try_recv().is_err(),
            "nothing that cannot be applied is forwarded"
        );
        assert!(state.is_healthy());
    });
}

/// Production SDK socket, join and initial/watch delivery; this fixture
/// does not certify broker authorization.
#[test]
#[cfg(unix)]
fn initial_catalogue_and_watch_share_one_turn_for_a_busy_native_owner() {
    let cluster = Uuid::new_v4();
    let captures = Captures {
        action_catalogue_fixture: Some(vec![
            notebook_action_catalogue_fixture(cluster, 1025, 1, "notebook"),
            notebook_action_catalogue_fixture(cluster, 1025, 2, "notebook"),
        ]),
        ..Captures::default()
    };
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(captures)),
        ..FakeBroker::default()
    };
    let (fd, _captures, worker) = stand_up_fake_broker(broker);
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: harness_presentation(),
            participant_kind: "agent",
            signer: harness_document_signer(),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::with_transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: "so.tilo.notes".into(),
        cluster_id: cluster,
        record: None,
        record_kind: None,
        handle: None,
    }));
    let admission = manager.await_primary_route();
    let state = broker_state(&manager);
    let healthy = state.is_healthy();
    let mut initial = ActionCatalogue::default();
    let first = manager.poll().into_iter().find_map(|event| match event {
        CollabEvent::ActionCatalogue { change } => Some(change),
        _ => None,
    });
    if let Some(change) = &first {
        initial.apply(change).unwrap();
    }
    // The first normal owner poll releases the initial turn. The broker
    // watch can then deliver its complete next baseline without overflow.
    let mut watched = initial.clone();
    if healthy {
        eventually(
            "the watched catalogue reaches the ordinary owner poll",
            || {
                let change = manager.poll().into_iter().find_map(|event| match event {
                    CollabEvent::ActionCatalogue { change } => Some(change),
                    _ => None,
                });
                change.is_some_and(|change| watched.apply(&change).is_ok())
            },
        );
    }
    drop(manager);
    worker.join().unwrap();
    assert!(
        admission.is_ok(),
        "startup must admit the route: {admission:?}"
    );
    assert!(
        healthy,
        "initial and watched baselines must not disconnect the application"
    );
    assert_eq!(
        initial.surfaces()[0].actions.len(),
        1025,
        "complete first baseline"
    );
    let watched = watched.surfaces();
    assert_eq!(watched[0].actions.len(), 1025, "complete watched baseline");
    assert_eq!(
        watched[0].peer_id, 2,
        "the watch delivered its next snapshot"
    );
    assert!(state.is_healthy());
}
