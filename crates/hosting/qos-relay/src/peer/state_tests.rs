use super::private_transport_tests::certified_device;
use super::*;
use crate::{
    auth::PeerRole,
    workspace::{Membership, WorkspaceRuntime},
};
use qos_user_identity::{RevocationList, UserIdentity};

struct Fixture {
    _tmp: tempfile::TempDir,
    root_key: ed25519_dalek::SigningKey,
    workspace: SharedWorkspace,
    registry: WorkspaceRegistry,
    store: crate::store::WorkspaceStore,
    source: StateSource,
    recipient: AuthenticatedPeer,
    recipient_tx: mpsc::Sender<Envelope>,
    recipient_rx: mpsc::Receiver<StateDelivery>,
    private: mpsc::Sender<PrivateOutbound>,
    metrics: Arc<crate::metrics::Metrics>,
    _receivers: Vec<mpsc::Receiver<Envelope>>,
}
impl Fixture {
    fn new() -> Self {
        let (root, user) = UserIdentity::generate("owner", 1);
        let (author, _) = certified_device(&root, user.public_key, "author");
        let (recipient, _) = certified_device(&root, user.public_key, "recipient");
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime =
            WorkspaceRuntime::open(tmp.path(), 7, true, Membership::default()).unwrap();
        for peer in [&author, &recipient] {
            runtime.admit_device(peer.device_public_key).unwrap();
        }
        let (source_tx, source_rx) = mpsc::channel(32);
        let (recipient_tx, recipient_state_rx) = mpsc::channel(32);
        let (delivery, recipient_rx) = mpsc::channel(32);
        let (disconnect, _) = mpsc::channel(1);
        for (peer, tx) in [(&author, &source_tx), (&recipient, &recipient_tx)] {
            runtime.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx: tx.clone(),
                disconnect: disconnect.clone(),
            });
        }
        runtime.state_sessions.insert(
            recipient.device_public_key,
            StateSession {
                peer: recipient.clone(),
                tx: delivery,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            },
        );
        let source = StateSource {
            peer: author,
            tx: source_tx,
            disconnect,
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user: user.public_key,
            federation: None,
            session_live: None,
        };
        let store = runtime.store.clone();
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(7, workspace.clone())])));
        let (private, _) = mpsc::channel(8);
        Self {
            _tmp: tmp,
            root_key: root,
            workspace,
            registry,
            store,
            source,
            recipient,
            recipient_tx,
            recipient_rx,
            private,
            metrics: Arc::new(crate::metrics::Metrics::default()),
            _receivers: vec![source_rx, recipient_state_rx],
        }
    }
    fn envelope(&self, kind: u8) -> Envelope {
        let delta = match kind {
            1 => crate::coedit::CoeditPayload::Checkpoint {
                channel: [3; 32],
                body: vec![1; 64],
            }
            .encode()
            .unwrap(),
            2 => crate::coedit::CoeditPayload::RetainedDelta {
                channel: [3; 32],
                body: vec![2; 64],
            }
            .encode()
            .unwrap(),
            _ => {
                let mut bytes = vec![kind; 64];
                bytes[0] = 255;
                bytes
            }
        };
        Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: self.source.peer.device_public_key,
            authored_at_ns: kind as u64,
            delta,
            signature: [0; 64],
        }
    }
    async fn enqueue(&self, kind: u8) {
        enqueue_state(
            &self.workspace,
            self.envelope(kind),
            Some(self.source.clone()),
            PeerRole::Device,
            self.metrics.clone(),
        )
        .await
        .unwrap();
    }
    async fn drained(&self) {
        let owner = crate::persistence::handle(&self.workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
    }
    async fn write(&self, delivery: StateDelivery, wire: &mut Vec<u8>) {
        let session = SessionCtx {
            workspaces: &self.registry,
            autobind: None,
            outbound: &self.recipient_tx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &self.source.own_user,
            metrics: &self.metrics,
            trust_policy: &self.source.policy,
            disconnect: &self.source.disconnect,
            v2: true,
            private_tx: &self.private,
            revocations: &self.source.revocations,
        };
        write_state_delivery(delivery, &self.recipient, &session, wire)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn cleanup_queued_before_recipient_replacement_retains_exact_pending() {
    let mut fixture = Fixture::new();
    fixture.enqueue(0).await;
    fixture.drained().await;
    let delivery = fixture.recipient_rx.recv().await.unwrap();
    let path = delivery.cleanup.clone().unwrap();
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "state-persist");
    fixture.enqueue(1).await;
    reached.await.unwrap();
    fixture.write(delivery, &mut Vec::new()).await;
    let (replacement, _rx) = mpsc::channel(2);
    fixture.workspace.lock().await.add_subscriber(Subscriber {
        device_public_key: fixture.recipient.device_public_key,
        role: PeerRole::Device,
        tx: replacement,
        disconnect: fixture.source.disconnect.clone(),
    });
    drop(release);
    fixture.drained().await;
    assert!(
        path.exists(),
        "old session cleanup removed a pending entry after replacement"
    );
}

#[tokio::test]
async fn state_owner_preserves_checkpoint_then_tail_and_durability_under_cancellation() {
    let mut fixture = Fixture::new();
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "state-persist");
    fixture.enqueue(1).await;
    reached.await.unwrap();
    fixture.enqueue(2).await;
    assert!(fixture.recipient_rx.try_recv().is_err());
    assert!(fixture.store.load_retained().unwrap().is_empty());
    // Discarding receipts cannot cancel either admitted transaction.
    drop(release);
    fixture.drained().await;
    assert_eq!(
        *fixture.recipient_rx.recv().await.unwrap().envelope,
        fixture.envelope(1)
    );
    assert_eq!(
        *fixture.recipient_rx.recv().await.unwrap().envelope,
        fixture.envelope(2)
    );
    assert_eq!(fixture.store.load_retained().unwrap().len(), 2);
}

#[tokio::test]
async fn state_completion_rechecks_source_and_recipient_membership_and_generation() {
    for mutation in 0..6 {
        let mut fixture = Fixture::new();
        let (release, reached) =
            crate::store::test_barrier::install(fixture.store.root(), "state-persist");
        fixture.enqueue(1).await;
        reached.await.unwrap();
        if mutation < 2 {
            let keep = if mutation == 0 {
                fixture.recipient.device_public_key
            } else {
                fixture.source.peer.device_public_key
            };
            crate::workspace_io::replace_membership(&fixture.workspace, vec![keep])
                .await
                .unwrap();
        } else if mutation < 4 {
            let key = if mutation == 2 {
                fixture.source.peer.device_public_key
            } else {
                fixture.recipient.device_public_key
            };
            let (replacement, _rx) = mpsc::channel(2);
            fixture.workspace.lock().await.add_subscriber(Subscriber {
                device_public_key: key,
                role: PeerRole::Device,
                tx: replacement,
                disconnect: fixture.source.disconnect.clone(),
            });
        }
        if mutation >= 4 {
            let key = if mutation == 4 {
                fixture.source.peer.device_public_key
            } else {
                fixture.recipient.device_public_key
            };
            let mut entry = qos_user_identity::RevocationEntry {
                user_public_key: fixture.source.own_user,
                revoked_device_public_key: key,
                revoked_at_ns: 1,
                reason: qos_user_identity::RevocationReason::Lost,
                signature: [0; 64],
            };
            qos_user_identity::sign_revocation_entry(&mut entry, &fixture.root_key).unwrap();
            fixture
                .source
                .revocations
                .write()
                .unwrap()
                .insert(entry)
                .unwrap();
        }
        drop(release);
        fixture.drained().await;
        assert!(
            fixture.recipient_rx.try_recv().is_err(),
            "stale state publication after mutation {mutation}"
        );
        assert_eq!(
            fixture.store.load_retained().unwrap().len(),
            1,
            "historical committed coverage must survive loss of live authority"
        );
    }
}

#[tokio::test]
async fn opaque_live_enqueue_is_not_receipt_and_failed_wire_preserves_fallback() {
    let mut fixture = Fixture::new();
    fixture.enqueue(0).await;
    fixture.drained().await;
    let delivery = fixture.recipient_rx.recv().await.unwrap();
    let path = delivery.cleanup.clone().unwrap();
    assert!(path.exists());
    drop(delivery); // A closed wire never creates a cleanup request.
    assert_eq!(
        fixture
            .store
            .load_pending(&fixture.recipient.device_public_key)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .store
            .load_pending(&fixture.source.peer.device_public_key)
            .unwrap()
            .len(),
        0
    );
    fixture.enqueue(3).await;
    fixture.drained().await;
    let delivery = fixture.recipient_rx.recv().await.unwrap();
    let delivered_path = delivery.cleanup.clone().unwrap();
    fixture.write(delivery, &mut Vec::new()).await;
    fixture.drained().await;
    assert!(!delivered_path.exists());
    assert!(
        path.exists(),
        "successful later frame retired an unrelated unacknowledged entry"
    );
}

#[tokio::test]
async fn state_admission_bounds_messages_and_bytes_without_obstructing_authorization() {
    let fixture = Fixture::new();
    let owner = crate::persistence::handle(&fixture.workspace).await;
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "state-persist");
    fixture.enqueue(1).await;
    reached.await.unwrap();
    let mut receipts = Vec::new();
    for _ in 0..32 {
        receipts.push(owner.read(1, |_| Ok(())).unwrap());
    }
    assert!(
        owner.read(1, |_| Ok(())).is_err(),
        "unbounded per-workspace state admission"
    );
    assert!(
        owner.read(256 * 1024 * 1024 + 1, |_| Ok(())).is_err(),
        "unbounded state bytes"
    );
    assert!(
        crate::workspace_io::fresh_membership(&fixture.workspace)
            .await
            .unwrap()
            .membership
            .contains(&fixture.recipient.device_public_key)
    );
    drop(release);
    for receipt in receipts {
        crate::persistence::receipt(receipt).await.unwrap();
    }
    fixture.drained().await;
}

#[tokio::test]
async fn aborting_state_receipt_waiter_cannot_cancel_started_transaction() {
    let fixture = Fixture::new();
    let owner = crate::persistence::handle(&fixture.workspace).await;
    let raw = crate::transport::encode(&fixture.envelope(1)).unwrap();
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "state-persist");
    let receipt = owner
        .read(raw.len(), move |state| {
            state.store.retain_checkpoint(&[3; 32], &raw)
        })
        .unwrap();
    let caller = tokio::spawn(crate::persistence::receipt(receipt));
    reached.await.unwrap();
    caller.abort();
    drop(release);
    fixture.drained().await;
    assert_eq!(fixture.store.load_retained().unwrap().len(), 1);
}

#[tokio::test]
async fn outbound_federation_revocation_and_generation_replacement_fence_completion() {
    for revoke in [false, true] {
        let mut fixture = Fixture::new();
        fixture.source.peer.role = PeerRole::PeerRelay;
        fixture.source.peer.cert.capabilities.can_act_as_host = true;
        qos_user_identity::sign_device_cert(&mut fixture.source.peer.cert, &fixture.root_key)
            .unwrap();
        let lease = Arc::new(std::sync::atomic::AtomicBool::new(true));
        fixture.source.federation = Some(lease.clone());
        {
            let mut runtime = fixture.workspace.lock().await;
            runtime.record_peer_relay(fixture.source.peer.device_public_key);
            runtime
                .outbound_federation
                .insert(fixture.source.peer.device_public_key, lease.clone());
        }
        let (release, reached) =
            crate::store::test_barrier::install(fixture.store.root(), "state-persist");
        enqueue_federated(
            &fixture.workspace,
            fixture.envelope(1),
            fixture.source.clone(),
            fixture.metrics.clone(),
            false,
        )
        .await
        .unwrap();
        reached.await.unwrap();
        if revoke {
            let mut entry = qos_user_identity::RevocationEntry {
                user_public_key: fixture.source.own_user,
                revoked_device_public_key: fixture.source.peer.device_public_key,
                revoked_at_ns: 1,
                reason: qos_user_identity::RevocationReason::Lost,
                signature: [0; 64],
            };
            qos_user_identity::sign_revocation_entry(&mut entry, &fixture.root_key).unwrap();
            fixture
                .source
                .revocations
                .write()
                .unwrap()
                .insert(entry)
                .unwrap();
        } else {
            fixture.workspace.lock().await.outbound_federation.insert(
                fixture.source.peer.device_public_key,
                Arc::new(std::sync::atomic::AtomicBool::new(true)),
            );
        }
        drop(release);
        fixture.drained().await;
        assert!(fixture.recipient_rx.try_recv().is_err());
        assert_eq!(fixture.store.load_retained().unwrap().len(), 1);
        if !revoke {
            assert!(!Arc::ptr_eq(
                &fixture.workspace.lock().await.outbound_federation
                    [&fixture.source.peer.device_public_key],
                &lease
            ));
        }
    }
}

#[tokio::test]
async fn opaque_queue_closure_during_persistence_keeps_durable_fallback() {
    let mut fixture = Fixture::new();
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "pending-persist");
    fixture.enqueue(0).await;
    reached.await.unwrap();
    fixture.recipient_rx.close();
    drop(release);
    fixture.drained().await;
    let pending = fixture
        .store
        .load_pending(&fixture.recipient.device_public_key)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].bytes,
        crate::transport::encode(&fixture.envelope(0)).unwrap()
    );
}

#[tokio::test]
async fn failed_sync_fallback_does_not_advance_the_materialized_retry_fence() {
    use automerge::transaction::Transactable;
    let fixture = Fixture::new();
    let mut document = automerge::AutoCommit::new();
    document
        .put(automerge::ROOT, "value", "durable sync")
        .unwrap();
    let envelope = Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: [0; 32],
        authored_at_ns: 1,
        delta: document.save_incremental(),
        signature: [0; 64],
    };
    let pending = fixture.store.root().join("pending");
    std::fs::remove_dir_all(&pending).unwrap();
    std::fs::write(&pending, b"blocked").unwrap();
    enqueue_state_inner(
        &fixture.workspace,
        envelope.clone(),
        Some(fixture.source.clone()),
        PeerRole::PeerRelay,
        fixture.metrics.clone(),
        true,
    )
    .await
    .unwrap();
    let owner = crate::persistence::handle(&fixture.workspace).await;
    let heads = crate::persistence::receipt(owner.read(1, |state| Ok(state.heads())).unwrap())
        .await
        .unwrap();
    assert!(
        heads.is_empty(),
        "failed durable fanout advanced the anti-entropy retry fence"
    );
    std::fs::remove_file(pending).unwrap();
    enqueue_state_inner(
        &fixture.workspace,
        envelope,
        Some(fixture.source.clone()),
        PeerRole::PeerRelay,
        fixture.metrics.clone(),
        true,
    )
    .await
    .unwrap();
    fixture.drained().await;
    assert_eq!(
        fixture
            .store
            .load_pending(&fixture.recipient.device_public_key)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn unsubscribed_sync_request_cannot_publish_after_a_replacement_subscribes() {
    let mut fixture = Fixture::new();
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "state-persist");
    fixture.enqueue(1).await;
    reached.await.unwrap();
    fixture
        .workspace
        .lock()
        .await
        .remove_subscriber(&fixture.source.peer.device_public_key);
    let session = SessionCtx {
        workspaces: &fixture.registry,
        autobind: None,
        outbound: &fixture.source.tx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &fixture.source.own_user,
        metrics: &fixture.metrics,
        trust_policy: &fixture.source.policy,
        disconnect: &fixture.source.disconnect,
        v2: true,
        private_tx: &fixture.private,
        revocations: &fixture.source.revocations,
    };
    handle_sync_request(&fixture.source.peer, &session, 7, vec![])
        .await
        .unwrap();
    let (replacement, _rx) = mpsc::channel(2);
    fixture.workspace.lock().await.add_subscriber(Subscriber {
        device_public_key: fixture.source.peer.device_public_key,
        role: PeerRole::Device,
        tx: replacement,
        disconnect: fixture.source.disconnect.clone(),
    });
    drop(release);
    fixture.drained().await;
    assert!(
        fixture._receivers[0].try_recv().is_err(),
        "unsubscribed old source published a sync response after replacement"
    );
}

#[tokio::test]
async fn subscribe_queued_before_delta_receives_commit_after_its_replay_cut() {
    for kind in [0, 1] {
        let fixture = Fixture::new();
        fixture
            .workspace
            .lock()
            .await
            .remove_subscriber(&fixture.recipient.device_public_key);
        let (release, reached) =
            crate::store::test_barrier::install(fixture.store.root(), "replay-prepare");
        let registry = fixture.registry.clone();
        let source = fixture.source.clone();
        let recipient = fixture.recipient.clone();
        let outbound = fixture.recipient_tx.clone();
        let private = fixture.private.clone();
        let metrics = fixture.metrics.clone();
        let (state_tx, mut deliveries) = mpsc::channel(32);
        let subscribing = tokio::spawn(async move {
            let session = SessionCtx {
                workspaces: &registry,
                autobind: None,
                outbound: &outbound,
                state_outbound: Some(&state_tx),
                connection_live: None,
                own_user_public_key: &source.own_user,
                metrics: &metrics,
                trust_policy: &source.policy,
                disconnect: &source.disconnect,
                v2: true,
                private_tx: &private,
                revocations: &source.revocations,
            };
            let mut frames = None;
            let reply = handle_subscribe(&recipient, &session, &mut Vec::new(), &mut frames, 7)
                .await
                .unwrap();
            assert_eq!(reply, None);
            let (_, ticket) = frames.expect("subscribe did not return a bounded ticket");
            let workspace = registry.lock().await.get(&7).unwrap().clone();
            let owner = crate::persistence::handle(&workspace).await;
            let mut written = None;
            loop {
                let authority = ticket.authority.clone();
                match crate::persistence::receipt(
                    owner
                        .read(1, move |state| state.advance_replay(&authority, written))
                        .unwrap(),
                )
                .await
                .unwrap()
                {
                    ReplayAdvance::Preparing => written = None,
                    ReplayAdvance::Batch(batch) => {
                        assert_eq!(batch.len(), 0, "later commit leaked into replay cut");
                        written = Some(WrittenBatch(batch.number));
                    }
                    ReplayAdvance::Exhausted(summary) => {
                        assert_eq!(summary.ready_count(), 0);
                        break;
                    }
                }
            }
        });
        reached.await.unwrap();
        fixture.enqueue(kind).await;
        drop(release);
        subscribing.await.unwrap();
        fixture.drained().await;
        let delivery = deliveries
            .try_recv()
            .expect("subscriber missed the first commit after its replay cut");
        assert_eq!(*delivery.envelope, fixture.envelope(kind));
        if kind == 0 {
            assert!(delivery.cleanup.as_ref().unwrap().exists());
        }
    }
}

#[tokio::test]
async fn owner_reclaims_abandoned_work_before_first_job() {
    let fixture = Fixture::new();
    let abandoned = fixture
        .store
        .root()
        .join("store-work-v1/replay-00000000000000000000000000000001");
    std::fs::create_dir_all(abandoned.join("pins")).unwrap();
    std::fs::write(abandoned.join("pins/0000000000000001.frame"), b"old pin").unwrap();
    std::fs::write(abandoned.join("unknown"), b"preserve").unwrap();
    let owner = crate::persistence::handle(&fixture.workspace).await;
    let path = abandoned.clone();
    let remains = crate::persistence::receipt(
        owner
            .read(1, move |_| {
                Ok(path.join("pins/0000000000000001.frame").exists())
            })
            .unwrap(),
    )
    .await
    .unwrap();
    assert!(
        !remains,
        "owner admitted fresh work before reclaiming abandoned pins"
    );
    assert!(abandoned.join("unknown").exists());
}

#[tokio::test]
async fn retained_transactions_release_global_workers_between_quanta() {
    let fixtures = (0..5).map(|_| Fixture::new()).collect::<Vec<_>>();
    let mut owners = Vec::new();
    for fixture in &fixtures {
        let owner = crate::persistence::handle(&fixture.workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        owners.push(owner);
    }
    let mut first = Vec::new();
    let mut second = Vec::new();
    for fixture in &fixtures[..4] {
        let (release, reached) =
            crate::store::test_barrier::install(fixture.store.root(), "state-persist");
        first.push(release);
        let (release, _) = crate::store::test_barrier::install(fixture.store.root(), "retain-sort");
        second.push(release);
        fixture.enqueue(1).await;
        reached.await.unwrap();
    }
    let (prepared, queued) = tokio::sync::oneshot::channel();
    let fifth = owners[4]
        .submit_prepared(
            1,
            async move {
                prepared.send(()).unwrap();
                Ok(())
            },
            |_, ()| Ok(()),
            |result| async { result },
        )
        .unwrap();
    queued.await.unwrap();
    // This unrelated transaction is now waiting for the global worker pool.
    // Each retained write must yield its worker before its later sort phase.
    drop(first);
    let progress = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        crate::persistence::receipt(fifth),
    )
    .await;
    drop(second);
    for fixture in &fixtures {
        fixture.drained().await;
    }
    assert!(
        matches!(progress, Ok(Ok(()))),
        "retained continuations monopolized every global worker"
    );
}

impl Fixture {
    async fn subscribe_ticket(&self) -> (u64, ReplayTicket) {
        let (state_tx, _state_rx) = mpsc::channel(32);
        let session = SessionCtx {
            workspaces: &self.registry,
            autobind: None,
            outbound: &self.recipient_tx,
            state_outbound: Some(&state_tx),
            connection_live: None,
            own_user_public_key: &self.source.own_user,
            metrics: &self.metrics,
            trust_policy: &self.source.policy,
            disconnect: &self.source.disconnect,
            v2: true,
            private_tx: &self.private,
            revocations: &self.source.revocations,
        };
        let mut replay = None;
        handle_subscribe(&self.recipient, &session, &mut Vec::new(), &mut replay, 7)
            .await
            .unwrap();
        replay.unwrap()
    }
}

#[tokio::test]
async fn cancelled_capture_cannot_register_and_idle_owner_reclaims_it() {
    let fixture = Arc::new(Fixture::new());
    fixture
        .workspace
        .lock()
        .await
        .remove_subscriber(&fixture.recipient.device_public_key);
    fixture
        .store
        .enqueue_pending(
            &fixture.recipient.device_public_key,
            1,
            &crate::transport::encode(&fixture.envelope(0)).unwrap(),
        )
        .unwrap();
    let owner = crate::persistence::handle(&fixture.workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    let workspace = fixture.workspace.clone();
    let store = fixture.store.clone();
    let device = fixture.recipient.device_public_key;
    let (release, reached) = crate::store::test_barrier::install(store.root(), "replay-prepare");
    let subscribing = fixture.clone();
    let task = tokio::spawn(async move { subscribing.subscribe_ticket().await });
    reached.await.unwrap();
    let (complete, completing) =
        crate::store::test_barrier::install(store.root(), "membership-read");
    drop(release);
    completing.await.unwrap();
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    drop(complete);
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert!(
        !workspace.lock().await.subscribers.contains_key(&device),
        "cancelled capture registered a subscriber"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if std::fs::read_dir(store.root().join("store-work-v1"))
                .unwrap()
                .next()
                .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idle owner did not reclaim cancelled capture");
    assert_eq!(store.count_pending(&device).unwrap(), 1);
}

impl Fixture {
    async fn replay_to<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        write: &mut W,
    ) -> Result<(), PeerError> {
        let (state_tx, _state_rx) = mpsc::channel(32);
        let session = SessionCtx {
            workspaces: &self.registry,
            autobind: None,
            outbound: &self.recipient_tx,
            state_outbound: Some(&state_tx),
            connection_live: None,
            own_user_public_key: &self.source.own_user,
            metrics: &self.metrics,
            trust_policy: &self.source.policy,
            disconnect: &self.source.disconnect,
            v2: true,
            private_tx: &self.private,
            revocations: &self.source.revocations,
        };
        handle_inbound_frame(
            Envelope::Subscribe { workspace_id: 7 },
            &self.recipient,
            &session,
            &mut Vec::new(),
            write,
        )
        .await
    }
}

#[tokio::test]
async fn replay_cleanup_rechecks_authority_after_waiting_for_worker() {
    for change in [0, 1, 2] {
        let fixture = Arc::new(Fixture::new());
        let device = fixture.recipient.device_public_key;
        fixture
            .store
            .enqueue_pending(
                &device,
                1,
                &crate::transport::encode(&fixture.envelope(0)).unwrap(),
            )
            .unwrap();
        let (release, reached) =
            crate::store::test_barrier::install(fixture.store.root(), "replay-cleanup");
        let writer = fixture.clone();
        let task = tokio::spawn(async move { writer.replay_to(&mut Vec::new()).await });
        reached.await.unwrap();
        match change {
            0 => {
                let mut entry = qos_user_identity::RevocationEntry {
                    user_public_key: fixture.source.own_user,
                    revoked_device_public_key: device,
                    revoked_at_ns: 1,
                    reason: qos_user_identity::RevocationReason::Lost,
                    signature: [0; 64],
                };
                qos_user_identity::sign_revocation_entry(&mut entry, &fixture.root_key).unwrap();
                fixture
                    .source
                    .revocations
                    .write()
                    .unwrap()
                    .insert(entry)
                    .unwrap();
            }
            1 => crate::workspace_io::replace_membership(
                &fixture.workspace,
                vec![fixture.source.peer.device_public_key],
            )
            .await
            .unwrap(),
            _ => {
                let (replacement, _rx) = mpsc::channel(32);
                fixture.workspace.lock().await.add_subscriber(Subscriber {
                    device_public_key: device,
                    role: PeerRole::Device,
                    tx: replacement,
                    disconnect: fixture.source.disconnect.clone(),
                });
            }
        }
        drop(release);
        assert!(task.await.unwrap().is_err());
        assert_eq!(
            fixture.store.count_pending(&device).unwrap(),
            1,
            "authority change {change} removed replay sources"
        );
    }
}

#[tokio::test]
async fn replay_finish_rejects_duplicate_completion_and_outstanding_batches() {
    let fixture = Fixture::new();
    let (_, ticket) = fixture.subscribe_ticket().await;
    let owner = crate::persistence::handle(&fixture.workspace).await;
    let authority = ticket.authority.clone();
    assert!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state.finish_replay(&authority, |_| Ok(())))
                .unwrap()
        )
        .await
        .is_err()
    );
    let batch = loop {
        let authority = ticket.authority.clone();
        match crate::persistence::receipt(
            owner
                .read(1, move |state| state.advance_replay(&authority, None))
                .unwrap(),
        )
        .await
        .unwrap()
        {
            ReplayAdvance::Preparing => {}
            ReplayAdvance::Batch(batch) => break batch,
            _ => panic!("empty replay bypassed its batch receipt"),
        }
    };
    let authority = ticket.authority.clone();
    assert!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state.advance_replay(&authority, None))
                .unwrap()
        )
        .await
        .is_err()
    );
    let authority = ticket.authority.clone();
    assert!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state
                    .advance_replay(&authority, Some(WrittenBatch(99))))
                .unwrap()
        )
        .await
        .is_err()
    );
    let number = batch.number;
    drop(batch);
    let authority = ticket.authority.clone();
    assert!(matches!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state
                    .advance_replay(&authority, Some(WrittenBatch(number))))
                .unwrap()
        )
        .await
        .unwrap(),
        ReplayAdvance::Exhausted(_)
    ));
    let authority = ticket.authority.clone();
    assert!(matches!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state.finish_replay(&authority, |_| Ok(())))
                .unwrap()
        )
        .await
        .unwrap(),
        Step::Done(_)
    ));
    let authority = ticket.authority.clone();
    assert!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state.finish_replay(&authority, |_| Ok(())))
                .unwrap()
        )
        .await
        .is_err(),
        "completed ticket accepted a duplicate finish"
    );
}

struct InterruptWrite {
    remaining: usize,
}
impl tokio::io::AsyncWrite for InterruptWrite {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.remaining == 0 {
            return std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }
        let written = self.remaining.min(bytes.len());
        self.remaining -= written;
        std::task::Poll::Ready(Ok(written))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
#[tokio::test]
async fn interrupted_second_batch_preserves_entire_replay_until_reconnect() {
    let fixture = Fixture::new();
    for i in 0..25 {
        fixture
            .store
            .enqueue_pending(
                &fixture.recipient.device_public_key,
                i,
                &crate::transport::encode(&fixture.envelope(i as u8)).unwrap(),
            )
            .unwrap();
    }
    let first_batch_bytes: usize = (0..8)
        .map(|i| {
            crate::transport_v2::encode_state(&fixture.envelope(i), true)
                .unwrap()
                .len()
        })
        .sum();
    assert!(
        fixture
            .replay_to(&mut InterruptWrite {
                remaining: first_batch_bytes + 3
            })
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        25
    );
    let mut wire = Vec::new();
    fixture.replay_to(&mut wire).await.unwrap();
    let mut reader = SessionReader::new(wire.as_slice(), true);
    for i in 0..25 {
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(fixture.envelope(i)))
        );
    }
    assert_eq!(
        reader.read_next().await.unwrap(),
        Some(EnvelopeV2::State(Envelope::Ready {
            workspace_id: 7,
            pending_deltas: 25
        }))
    );
    assert!(reader.read_next().await.unwrap().is_none());
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn dropped_ticket_reclaims_without_abort_admission_on_full_owner_queue() {
    let fixture = Fixture::new();
    for i in 0..257 {
        fixture
            .store
            .enqueue_pending(
                &fixture.recipient.device_public_key,
                i,
                &crate::transport::encode(&fixture.envelope(0)).unwrap(),
            )
            .unwrap();
    }
    let (_, ticket) = fixture.subscribe_ticket().await;
    let owner = crate::persistence::handle(&fixture.workspace).await;
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "hold-owner");
    owner
        .read(1, |state| {
            crate::store::test_barrier::pause(state.store.root(), "hold-owner");
            Ok(())
        })
        .unwrap();
    reached.await.unwrap();
    let mut admitted = 0;
    while owner.read(1, |_| Ok(())).is_ok() {
        admitted += 1;
    }
    assert!(admitted > 0);
    drop(ticket);
    drop(release);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if std::fs::read_dir(fixture.store.root().join("store-work-v1"))
                .unwrap()
                .next()
                .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled ticket was stranded by a full owner queue");
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        257
    );
}
