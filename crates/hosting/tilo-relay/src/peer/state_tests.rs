#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::similar_names,
    reason = "paired test fixtures are named after the roles they mirror"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use super::private_transport_tests::certified_device;
use super::*;
use crate::{
    auth::PeerRole,
    workspace::{Membership, WorkspaceRuntime},
};
use tilo_user_identity::{RevocationList, UserIdentity};

mod recovery;

struct Fixture {
    workers: Arc<tokio::sync::Semaphore>,
    _tmp: tempfile::TempDir,
    root_key: ed25519_dalek::SigningKey,
    source_key: ed25519_dalek::SigningKey,
    workspace: SharedWorkspace,
    registry: WorkspaceRegistry,
    store: crate::store::WorkspaceStore,
    source: StateSource,
    recipient: AuthenticatedPeer,
    recipient_tx: mpsc::Sender<Envelope>,
    recipient_rx: mpsc::Receiver<StateDelivery>,
    recipient_disconnect: mpsc::Sender<()>,
    recipient_disconnected: mpsc::Receiver<()>,
    private: mpsc::Sender<PrivateOutbound>,
    metrics: Arc<crate::metrics::Metrics>,
    receivers: Vec<mpsc::Receiver<Envelope>>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_delivery_capacity(32)
    }
    fn with_delivery_capacity(delivery_capacity: usize) -> Self {
        Self::with_delivery_capacity_and_workers(
            delivery_capacity,
            crate::persistence::isolated_test_workers(),
        )
    }
    fn with_delivery_capacity_and_workers(
        delivery_capacity: usize,
        workers: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self::with_delivery_capacity_workers_and_workspace(delivery_capacity, workers, 7)
    }
    fn with_workspace_id(workspace_id: u64) -> Self {
        Self::with_delivery_capacity_workers_and_workspace(
            32,
            crate::persistence::isolated_test_workers(),
            workspace_id,
        )
    }
    fn with_delivery_capacity_workers_and_workspace(
        delivery_capacity: usize,
        workers: Arc<tokio::sync::Semaphore>,
        workspace_id: u64,
    ) -> Self {
        let (root, user) = UserIdentity::generate("owner", 1);
        let (author, source_key) = certified_device(&root, user.public_key, "author");
        let (recipient, _) = certified_device(&root, user.public_key, "recipient");
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime =
            WorkspaceRuntime::open(tmp.path(), workspace_id, true, Membership::default()).unwrap();
        for peer in [&author, &recipient] {
            runtime.admit_device(peer.device_public_key).unwrap();
        }
        crate::persistence::isolate_test_admission_with_workers(&runtime, workers.clone());
        let (source_tx, source_rx) = mpsc::channel(32);
        let (recipient_tx, recipient_state_rx) = mpsc::channel(32);
        let (delivery, recipient_rx) = mpsc::channel(delivery_capacity);
        let (source_disconnect, _) = mpsc::channel(1);
        let (recipient_disconnect, recipient_disconnected) = mpsc::channel(1);
        for (peer, tx, disconnect) in [
            (&author, &source_tx, &source_disconnect),
            (&recipient, &recipient_tx, &recipient_disconnect),
        ] {
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
                replay: None,
                lag: None,
            },
        );
        let source = StateSource {
            peer: author,
            tx: source_tx,
            disconnect: source_disconnect,
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
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(
            workspace_id,
            workspace.clone(),
        )])));
        let (private, _) = mpsc::channel(8);
        Self {
            workers,
            _tmp: tmp,
            root_key: root,
            source_key,
            workspace,
            registry,
            store,
            source,
            recipient,
            recipient_tx,
            recipient_rx,
            recipient_disconnect,
            recipient_disconnected,
            private,
            metrics: Arc::new(crate::metrics::Metrics::default()),
            receivers: vec![source_rx, recipient_state_rx],
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
            authored_at_ns: u64::from(kind),
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
    async fn enqueue_checkpoint(&self, authored_at_ns: u64, body: Vec<u8>) {
        enqueue_state(
            &self.workspace,
            Envelope::Delta {
                workspace_id: 7,
                author_device_public_key: self.source.peer.device_public_key,
                authored_at_ns,
                delta: crate::coedit::CoeditPayload::Checkpoint {
                    channel: [3; 32],
                    body,
                }
                .encode()
                .unwrap(),
                signature: [0; 64],
            },
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
    async fn drained_wait(&self) {
        let owner = crate::persistence::handle(&self.workspace).await;
        let receipt = owner
            .submit_resumable_wait(
                1,
                async { Ok(()) },
                |_, _| Ok(Step::Done(())),
                |result| async { result },
            )
            .await
            .unwrap();
        crate::persistence::receipt(receipt).await.unwrap();
    }
    async fn write(&self, delivery: StateDelivery, wire: &mut Vec<u8>) {
        let session = SessionCtx {
            workspaces: &self.registry,
            autobind: None,
            outbound: &self.recipient_tx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &self.source.own_user,
            own_device_cert: &self.source.peer.cert,
            own_device_public_key: &self.source.peer.cert.device_public_key,
            app_serving: None,
            metrics: &self.metrics,
            trust_policy: &self.source.policy,
            disconnect: &self.recipient_disconnect,
            v2: true,
            private_tx: &self.private,
            revocations: &self.source.revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        write_state_delivery(delivery, &self.recipient, &session, wire)
            .await
            .unwrap();
    }
}

impl Fixture {
    /// Signs a revocation of `device` under the owner root and installs it in
    /// the source session's live revocation list.
    fn revoke(&self, device: [u8; 32]) {
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: self.source.own_user,
            revoked_device_public_key: device,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &self.root_key).unwrap();
        self.source
            .revocations
            .write()
            .unwrap()
            .insert(entry)
            .unwrap();
    }
    fn revoke_source(&self) {
        self.revoke(self.source.peer.device_public_key);
    }
}

// These cases exercise the ordered persistence/publication path, including its
// checkpoint-prefix publisher, rather than only the checkpoint classifier.
impl Fixture {
    async fn for_checkpoint_path(prefix: bool) -> Self {
        if !prefix {
            return Self::new();
        }
        let room = uuid::Uuid::from_u128(0x7063).to_string();
        let mut fixture = Self::with_workspace_id(crate::room_workspace_id(&room).unwrap());
        {
            let mut runtime = fixture.workspace.lock().await;
            runtime
                .store
                .bind_shell_authority(
                    tilo_peer_state::retained_proof::RetentionAuthority::new(
                        &room,
                        fixture.source.peer.cert.clone(),
                    )
                    .unwrap(),
                )
                .unwrap();
            runtime.checkpoint_prefix_recovery =
                crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
            fixture.store = runtime.store.clone();
        }
        // A production prefix arrives only after subscription has initialized
        // the bound selector and replay scratch; retain the resulting live lane.
        fixture.recipient_rx = Box::pin(fixture.replay_to_with_deliveries(&mut Vec::new()))
            .await
            .unwrap();
        fixture
    }

    fn continuity_checkpoint(&self, snapshot: Vec<u8>) -> ([u8; 32], Vec<u8>) {
        let msg = tilo_peer_state::NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key: self.source.peer.user_public_key,
            publisher: 7,
            snapshot,
        };
        let (channel, _) = tilo_peer_state::transport::retention(&msg, false).unwrap();
        (channel, postcard::to_stdvec(&msg).unwrap())
    }

    fn checkpoint_frame(&self, time: u64, channel: [u8; 32], body: Vec<u8>) -> Envelope {
        Envelope::Delta {
            workspace_id: self.store.workspace_id(),
            author_device_public_key: self.source.peer.device_public_key,
            authored_at_ns: time,
            delta: crate::coedit::CoeditPayload::Checkpoint { channel, body }
                .encode()
                .unwrap(),
            signature: [0; 64],
        }
    }

    async fn publish_checkpoint_frame(&self, frame: Envelope, prefix: bool, authenticated: bool) {
        if prefix {
            assert!(authenticated);
            enqueue_checkpoint_prefix(
                &self.workspace,
                vec![frame],
                self.source.clone(),
                PeerRole::Device,
                self.metrics.clone(),
                vec![None],
            )
            .await
            .unwrap();
        } else {
            enqueue_state(
                &self.workspace,
                frame,
                authenticated.then(|| self.source.clone()),
                PeerRole::Device,
                self.metrics.clone(),
            )
            .await
            .unwrap();
        }
        self.drained_wait().await;
    }

    async fn assert_checkpoint_written(&mut self, expected: &Envelope) {
        let delivery = self
            .recipient_rx
            .try_recv()
            .expect("checkpoint not delivered");
        let mut wire = Vec::new();
        self.write(delivery, &mut wire).await;
        let mut reader = SessionReader::new(wire.as_slice(), true);
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(expected.clone()))
        );
        assert!(reader.read_next().await.unwrap().is_none());
    }
}

async fn assert_personal_continuity_live_closure(prefix: bool) -> (Fixture, Envelope) {
    let mut fixture = Box::pin(Fixture::for_checkpoint_path(prefix)).await;
    let (channel, initial) = fixture.continuity_checkpoint(vec![1]);
    let initial = fixture.checkpoint_frame(1, channel, initial);
    fixture
        .publish_checkpoint_frame(initial.clone(), prefix, true)
        .await;
    fixture.assert_checkpoint_written(&initial).await;
    assert!(
        fixture
            .workspace
            .lock()
            .await
            .needs_checkpoint(&fixture.recipient.device_public_key, &channel,),
        "continuity publication consumed the ordinary checkpoint request"
    );
    // An ordinary checkpoint may already have served this channel. That
    // must not suppress the next authenticated continuity closure.
    fixture
        .workspace
        .lock()
        .await
        .mark_checkpoint_served(&fixture.recipient.device_public_key, channel);
    let (_, closed) = fixture.continuity_checkpoint(Vec::new());
    let closed = fixture.checkpoint_frame(2, channel, closed);
    fixture
        .publish_checkpoint_frame(closed.clone(), prefix, true)
        .await;
    fixture.assert_checkpoint_written(&closed).await;
    assert!(fixture.recipient_rx.try_recv().is_err());
    fixture.drained_wait().await;

    (fixture, closed)
}

#[tokio::test]
async fn personal_continuity_live_closure_uses_both_publishers() {
    for prefix in [false, true] {
        Box::pin(assert_personal_continuity_live_closure(prefix)).await;
    }
}

#[tokio::test]
async fn personal_continuity_latest_only_replay_uses_both_publishers() {
    for prefix in [false, true] {
        let (fixture, closed) = Box::pin(assert_personal_continuity_live_closure(prefix)).await;
        // Re-subscription creates a fresh live session with the same durable
        // identity and exercises the actual replay writer and final Ready.
        fixture
            .workspace
            .lock()
            .await
            .remove_subscriber(&fixture.recipient.device_public_key);
        let mut wire = Vec::new();
        Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
        let mut reader = SessionReader::new(wire.as_slice(), true);
        let mut checkpoints = Vec::new();
        let mut ready = false;
        while let Some(frame) = reader.read_next().await.unwrap() {
            match frame {
                EnvelopeV2::State(envelope @ Envelope::Delta { .. }) => checkpoints.push(envelope),
                EnvelopeV2::State(Envelope::Ready { .. }) => ready = true,
                other => panic!("unexpected continuity replay frame: {other:?}"),
            }
        }
        assert!(ready, "fresh session never completed retained replay");
        assert_eq!(
            checkpoints,
            vec![closed],
            "replay retained stale nonempty continuity state"
        );
    }
}

#[tokio::test]
async fn personal_continuity_newest_replay_preserves_all_retained_receipts() {
    for prefix in [false, true] {
        let mut fixture = Box::pin(Fixture::for_checkpoint_path(prefix)).await;
        let mut published = Vec::new();
        for (time, snapshot) in [(1, vec![1]), (2, Vec::new()), (3, vec![1])] {
            let (channel, body) = fixture.continuity_checkpoint(snapshot);
            let frame = fixture.checkpoint_frame(time, channel, body);
            fixture
                .publish_checkpoint_frame(frame.clone(), prefix, true)
                .await;
            fixture.assert_checkpoint_written(&frame).await;
            published.push(frame);
        }
        fixture.drained_wait().await;
        let retained = published
            .iter()
            .map(|frame| crate::transport::encode(frame).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            fixture.store.load_retained().unwrap(),
            retained,
            "authenticated receipt append changed bytes/order, retired history or deduplicated a new receipt by its old body"
        );
        let mut wire = Vec::new();
        Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
        let mut reader = SessionReader::new(wire.as_slice(), true);
        let mut replayed = Vec::new();
        while let Some(frame) = reader.read_next().await.unwrap() {
            match frame {
                EnvelopeV2::State(envelope @ Envelope::Delta { .. }) => replayed.push(envelope),
                EnvelopeV2::State(Envelope::Ready { .. }) => {}
                other => panic!("unexpected continuity replay frame: {other:?}"),
            }
        }
        assert_eq!(
            replayed,
            vec![published.last().unwrap().clone()],
            "replay did not choose the newest accepted receipt"
        );
        assert_eq!(
            fixture.store.load_retained().unwrap(),
            retained,
            "replay selection changed or physically retired retained continuity history"
        );
    }
}

#[tokio::test]
async fn personal_continuity_unmarked_identical_frame_cannot_replace_marked_successor() {
    let mut fixture = Fixture::new();
    let (channel, old) = fixture.continuity_checkpoint(vec![0]);
    let old = fixture.checkpoint_frame(1, channel, old);
    fixture
        .publish_checkpoint_frame(old.clone(), false, true)
        .await;
    fixture.assert_checkpoint_written(&old).await;
    let (_, body) = fixture.continuity_checkpoint(vec![1]);
    let identical = fixture.checkpoint_frame(2, channel, body);
    fixture
        .publish_checkpoint_frame(identical.clone(), false, false)
        .await;
    fixture.assert_checkpoint_written(&identical).await;
    fixture
        .publish_checkpoint_frame(identical.clone(), false, true)
        .await;
    fixture.assert_checkpoint_written(&identical).await;
    fixture.drained_wait().await;
    // load_retained is a compatibility coverage view that deduplicates bytes;
    // this assertion concerns physical receipts, including identical raw frames.
    let physical_history = || {
        let lineage = fixture.store.root().join("retained/v2").join(format!(
            "{}-{}",
            hex::encode(fixture.source.peer.device_public_key),
            hex::encode(channel),
        ));
        let current = std::fs::read_to_string(lineage.join("CURRENT")).unwrap();
        assert_eq!(current.len(), 32);
        assert!(current.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let mut records = std::fs::read_dir(lineage.join(current))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "delta")
            })
            .collect::<Vec<_>>();
        records.sort();
        records
            .into_iter()
            .map(|path| {
                let marked = path.with_extension("delta.continuity-v1").is_file();
                (std::fs::read(path).unwrap(), marked)
            })
            .collect::<Vec<_>>()
    };
    let physical = physical_history();
    assert_eq!(
        physical.len(),
        3,
        "a new authenticated receipt was confused with identical unmarked raw history"
    );
    let identical_raw = crate::transport::encode(&identical).unwrap();
    assert_eq!(
        physical,
        vec![
            (crate::transport::encode(&old).unwrap(), true),
            (identical_raw.clone(), false),
            (identical_raw, true),
        ],
        "physical receipt order or marked/unmarked association changed"
    );
    let mut wire = Vec::new();
    Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
    let mut reader = SessionReader::new(wire.as_slice(), true);
    let mut replayed = Vec::new();
    while let Some(frame) = reader.read_next().await.unwrap() {
        match frame {
            EnvelopeV2::State(envelope @ Envelope::Delta { .. }) => replayed.push(envelope),
            EnvelopeV2::State(Envelope::Ready { .. }) => {}
            other => panic!("unexpected continuity replay frame: {other:?}"),
        }
    }
    assert_eq!(
        replayed,
        vec![identical.clone(), identical],
        "unmarked history must remain independent while its marked identical successor is emitted and supersedes the old marked receipt"
    );
    assert_eq!(
        physical_history(),
        physical,
        "replay mutated physical receipt history"
    );
}

#[tokio::test]
async fn personal_continuity_unmarked_historical_checkpoint_remains_in_replay() {
    let mut fixture = Fixture::new();
    let (channel, body) = fixture.continuity_checkpoint(vec![1]);
    let historical = fixture.checkpoint_frame(1, channel, body);
    fixture
        .publish_checkpoint_frame(historical.clone(), false, false)
        .await;
    fixture.assert_checkpoint_written(&historical).await;
    let (_, body) = fixture.continuity_checkpoint(Vec::new());
    let closed = fixture.checkpoint_frame(2, channel, body);
    fixture
        .publish_checkpoint_frame(closed.clone(), false, true)
        .await;
    fixture.assert_checkpoint_written(&closed).await;
    fixture.drained_wait().await;
    let mut wire = Vec::new();
    Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
    let mut reader = SessionReader::new(wire.as_slice(), true);
    let mut replayed = Vec::new();
    while let Some(frame) = reader.read_next().await.unwrap() {
        match frame {
            EnvelopeV2::State(envelope @ Envelope::Delta { .. }) => replayed.push(envelope),
            EnvelopeV2::State(Envelope::Ready { .. }) => {}
            other => panic!("unexpected continuity replay frame: {other:?}"),
        }
    }
    assert_eq!(
        replayed,
        vec![historical, closed],
        "continuity-shaped unmarked historical bytes gained retrospective authority"
    );
}

#[tokio::test]
async fn personal_continuity_aborted_replay_selects_newest_pending_receipt() {
    for prefix in [false, true] {
        let mut fixture = Box::pin(Fixture::for_checkpoint_path(prefix)).await;
        let (channel, initial) = fixture.continuity_checkpoint(vec![1]);
        let initial = fixture.checkpoint_frame(1, channel, initial);
        fixture
            .publish_checkpoint_frame(initial.clone(), prefix, true)
            .await;
        fixture.assert_checkpoint_written(&initial).await;
        let fixture = Arc::new(fixture);
        let (release, reached) =
            crate::store::test_barrier::install(fixture.store.root(), "replay-manifest-prepare");
        let replaying = fixture.clone();
        let replay =
            tokio::spawn(async move { Box::pin(replaying.replay_to(&mut Vec::new())).await });
        tilo_test_env::within("replay never reached detached preparation", reached)
            .await
            .unwrap();
        let (_, intermediate) = fixture.continuity_checkpoint(vec![2]);
        fixture
            .publish_checkpoint_frame(
                fixture.checkpoint_frame(2, channel, intermediate),
                prefix,
                true,
            )
            .await;
        let (_, body) = fixture.continuity_checkpoint(Vec::new());
        let closed = fixture.checkpoint_frame(3, channel, body);
        fixture
            .publish_checkpoint_frame(closed.clone(), prefix, true)
            .await;
        assert_eq!(
            fixture
                .store
                .count_pending(&fixture.recipient.device_public_key)
                .unwrap(),
            2,
            "ACTIVE replay did not retain both post-cut continuity receipts"
        );
        replay.abort();
        assert!(replay.await.unwrap_err().is_cancelled());
        drop(release);
        fixture.drained_wait().await;
        assert_eq!(
            fixture
                .store
                .count_pending(&fixture.recipient.device_public_key)
                .unwrap(),
            2,
            "aborting replay acknowledged pending continuity receipts before successful replay"
        );
        let mut wire = Vec::new();
        Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
        let mut reader = SessionReader::new(wire.as_slice(), true);
        let mut replayed = Vec::new();
        while let Some(frame) = reader.read_next().await.unwrap() {
            match frame {
                EnvelopeV2::State(envelope @ Envelope::Delta { .. }) => replayed.push(envelope),
                EnvelopeV2::State(Envelope::Ready { .. }) => {}
                other => panic!("unexpected continuity replay frame: {other:?}"),
            }
        }
        assert_eq!(
            replayed,
            vec![closed],
            "an older pending receipt resurrected stale continuity after restart"
        );
        assert_eq!(
            fixture.store.load_retained().unwrap().len(),
            3,
            "reconnect selection physically retired continuity history"
        );
    }
}

#[tokio::test]
async fn personal_continuity_rechecks_source_revocation_after_persistence_wait() {
    let mut fixture = Fixture::new();
    let (channel, initial) = fixture.continuity_checkpoint(vec![1]);
    let initial = fixture.checkpoint_frame(1, channel, initial);
    fixture
        .publish_checkpoint_frame(initial.clone(), false, true)
        .await;
    fixture.assert_checkpoint_written(&initial).await;
    let (_, closed) = fixture.continuity_checkpoint(Vec::new());
    let closed = fixture.checkpoint_frame(2, channel, closed);
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "state-persist");
    enqueue_state(
        &fixture.workspace,
        closed,
        Some(fixture.source.clone()),
        PeerRole::Device,
        fixture.metrics.clone(),
    )
    .await
    .unwrap();
    reached.await.unwrap();
    fixture.revoke_source();
    drop(release);
    fixture.drained_wait().await;
    assert!(
        fixture.recipient_rx.try_recv().is_err(),
        "continuity bypassed current source revocation"
    );
}

#[tokio::test]
async fn personal_continuity_prefix_rechecks_source_revocation_after_persistence_wait() {
    let mut fixture = Box::pin(Fixture::for_checkpoint_path(true)).await;
    let (channel, closed) = fixture.continuity_checkpoint(Vec::new());
    let closed = fixture.checkpoint_frame(1, channel, closed);
    // With a new lineage the prefix prepares recipients before its first
    // serial-retain transaction. This barrier pauses that transaction after
    // source preparation, before durable commit and live publication.
    let (release, reached) = crate::store::test_barrier::install(
        &fixture.store.root().join("store-work-v1"),
        "retain-sort",
    );
    enqueue_checkpoint_prefix(
        &fixture.workspace,
        vec![closed],
        fixture.source.clone(),
        PeerRole::Device,
        fixture.metrics.clone(),
        vec![None],
    )
    .await
    .unwrap();
    tilo_test_env::within("prefix never reached its persistence barrier", reached)
        .await
        .unwrap();
    fixture.revoke_source();
    drop(release);
    fixture.drained_wait().await;
    assert_eq!(
        fixture.store.load_retained().unwrap().len(),
        1,
        "historical commit was lost instead of fencing live publication"
    );
    assert!(
        fixture.recipient_rx.try_recv().is_err(),
        "prefix continuity bypassed current source revocation"
    );
}

#[tokio::test]
async fn personal_continuity_caller_cancellation_before_admission_leaves_no_receipt() {
    for prefix in [false, true] {
        let fixture = Arc::new(Box::pin(Fixture::for_checkpoint_path(prefix)).await);
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
        let queued = (0..32)
            .map(|_| owner.read(1, |_| Ok(())).unwrap())
            .collect::<Vec<_>>();
        let (channel, body) = fixture.continuity_checkpoint(Vec::new());
        let frame = fixture.checkpoint_frame(1, channel, body);
        let caller_fixture = fixture.clone();
        let mut caller = tokio::spawn(async move {
            caller_fixture
                .publish_checkpoint_frame(frame, prefix, true)
                .await;
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut caller)
                .await
                .is_err(),
            "full owner did not hold admission"
        );
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(release);
        for receipt in queued {
            crate::persistence::receipt(receipt).await.unwrap();
        }
        fixture.drained_wait().await;
        assert!(
            fixture.store.load_retained().unwrap().is_empty(),
            "cancelled unadmitted input acquired durable provenance"
        );
        let mut fixture = Arc::try_unwrap(fixture).ok().unwrap();
        assert!(fixture.recipient_rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn personal_continuity_caller_cancellation_after_admission_preserves_acceptance() {
    for prefix in [false, true] {
        let mut fixture = Box::pin(Fixture::for_checkpoint_path(prefix)).await;
        let (channel, body) = fixture.continuity_checkpoint(vec![1]);
        let initial = fixture.checkpoint_frame(1, channel, body);
        fixture
            .publish_checkpoint_frame(initial.clone(), prefix, true)
            .await;
        fixture.assert_checkpoint_written(&initial).await;
        let (_, body) = fixture.continuity_checkpoint(Vec::new());
        let closed = fixture.checkpoint_frame(2, channel, body);
        let fixture = Arc::new(fixture);
        let (release, reached) =
            crate::store::test_barrier::install(fixture.store.root(), "continuity-marker-durable");
        let caller_fixture = fixture.clone();
        let caller_frame = closed.clone();
        let caller = tokio::spawn(async move {
            caller_fixture
                .publish_checkpoint_frame(caller_frame, prefix, true)
                .await;
        });
        tilo_test_env::within("reached to complete", reached)
            .await
            .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(release);
        fixture.drained_wait().await;
        let mut fixture = Arc::try_unwrap(fixture).ok().unwrap();
        fixture.assert_checkpoint_written(&closed).await;
        let mut wire = Vec::new();
        Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
        let mut reader = SessionReader::new(wire.as_slice(), true);
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(closed))
        );
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ready { .. }))
        ));
        assert!(reader.read_next().await.unwrap().is_none());
        assert_eq!(fixture.store.load_retained().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn personal_continuity_session_invalidation_before_acceptance_blocks_receipt() {
    for prefix in [false, true] {
        let mut fixture = Box::pin(Fixture::for_checkpoint_path(prefix)).await;
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        fixture.source.session_live = Some(live.clone());
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
        let (channel, body) = fixture.continuity_checkpoint(Vec::new());
        let frame = fixture.checkpoint_frame(1, channel, body);
        if prefix {
            enqueue_checkpoint_prefix(
                &fixture.workspace,
                vec![frame],
                fixture.source.clone(),
                PeerRole::Device,
                fixture.metrics.clone(),
                vec![None],
            )
            .await
            .unwrap();
        } else {
            enqueue_state(
                &fixture.workspace,
                frame,
                Some(fixture.source.clone()),
                PeerRole::Device,
                fixture.metrics.clone(),
            )
            .await
            .unwrap();
        }
        live.store(false, std::sync::atomic::Ordering::SeqCst);
        drop(release);
        fixture.drained_wait().await;
        assert!(
            fixture.store.load_retained().unwrap().is_empty(),
            "invalid source was accepted after its queued wait"
        );
        assert!(fixture.recipient_rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn personal_continuity_prefix_revocation_before_promotion_keeps_prior_current() {
    let mut fixture = Box::pin(Fixture::for_checkpoint_path(true)).await;
    let (channel, body) = fixture.continuity_checkpoint(vec![1]);
    let initial = fixture.checkpoint_frame(1, channel, body);
    fixture
        .publish_checkpoint_frame(initial.clone(), true, true)
        .await;
    fixture.assert_checkpoint_written(&initial).await;
    let (_, body) = fixture.continuity_checkpoint(Vec::new());
    let closed = fixture.checkpoint_frame(2, channel, body);
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "continuity-marker-durable");
    enqueue_checkpoint_prefix(
        &fixture.workspace,
        vec![closed],
        fixture.source.clone(),
        PeerRole::Device,
        fixture.metrics.clone(),
        vec![None],
    )
    .await
    .unwrap();
    tilo_test_env::within("reached to complete", reached)
        .await
        .unwrap();
    fixture.revoke_source();
    drop(release);
    fixture.drained_wait().await;
    assert!(fixture.recipient_rx.try_recv().is_err());
    assert_eq!(
        fixture.store.load_retained().unwrap(),
        vec![crate::transport::encode(&initial).unwrap()],
        "speculative marked stage became accepted CURRENT after source revocation"
    );
    let mut wire = Vec::new();
    Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
    let mut reader = SessionReader::new(wire.as_slice(), true);
    assert_eq!(
        reader.read_next().await.unwrap(),
        Some(EnvelopeV2::State(initial))
    );
    assert!(matches!(
        reader.read_next().await.unwrap(),
        Some(EnvelopeV2::State(Envelope::Ready { .. }))
    ));
    assert!(reader.read_next().await.unwrap().is_none());
}

#[tokio::test]
async fn personal_continuity_nonmatching_checkpoints_keep_once_and_rearm_gates() {
    use crate::store::shell_selection::tests::{producer_frame, producer_metadata};
    let metadata = producer_metadata();
    let raw = producer_frame(&metadata, &metadata["baseline"][1], 1);
    let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
        panic!("signed checkpoint fixture was not a delta")
    };
    let crate::coedit::CoeditPayload::Checkpoint {
        channel: signed_channel,
        body: signed_body,
    } = crate::coedit::CoeditPayload::decode(&delta).unwrap()
    else {
        panic!("signed checkpoint fixture was not a checkpoint")
    };
    assert!(matches!(
        postcard::from_bytes::<tilo_peer_state::NeighbourMsg>(&signed_body).unwrap(),
        tilo_peer_state::NeighbourMsg::AppDocument { .. }
    ));
    for prefix in [false, true] {
        for case in [
            "opaque",
            "unknown",
            "malformed",
            "trailing",
            "outer_trailing",
            "foreign_root",
            "channel_mismatch",
            "other_variant",
            "signed_shell",
            "no_source",
        ] {
            // Prefix admission already requires a complete wrapper and a
            // StateSource. Exercise these generic fallbacks on serial ingress.
            if prefix && matches!(case, "no_source" | "outer_trailing") {
                continue;
            }
            let mut fixture = Box::pin(Fixture::for_checkpoint_path(prefix)).await;
            let (mut channel, mut body) = fixture.continuity_checkpoint(vec![1]);
            match case {
                "opaque" => body = vec![1; 64],
                "unknown" => body = vec![255; 8],
                "malformed" => {
                    body.pop();
                }
                "trailing" => body.push(0),
                "foreign_root" => {
                    let msg = tilo_peer_state::NeighbourMsg::PersonalContinuityCheckpoint {
                        user_public_key: [77; 32],
                        publisher: 7,
                        snapshot: vec![1],
                    };
                    channel = tilo_peer_state::transport::retention(&msg, false)
                        .unwrap()
                        .0;
                    body = postcard::to_stdvec(&msg).unwrap();
                }
                "channel_mismatch" => channel[0] ^= 1,
                "other_variant" => {
                    body = postcard::to_stdvec(
                        &tilo_peer_state::NeighbourMsg::PersonalContinuityRequest {
                            user_public_key: fixture.source.peer.user_public_key,
                            requester: 7,
                        },
                    )
                    .unwrap();
                }
                "signed_shell" => {
                    channel = signed_channel;
                    body = signed_body.clone();
                }
                "no_source" | "outer_trailing" => {}
                _ => unreachable!(),
            }
            let authenticated = case != "no_source";
            for (time, expected_delivery) in [(1, true), (2, false), (3, true), (4, false)] {
                if time == 3 {
                    fixture
                        .workspace
                        .lock()
                        .await
                        .request_checkpoint(&fixture.recipient.device_public_key);
                }
                let mut frame = fixture.checkpoint_frame(time, channel, body.clone());
                if case == "outer_trailing" {
                    let Envelope::Delta { delta, .. } = &mut frame else {
                        unreachable!()
                    };
                    delta.push(0);
                }
                fixture
                    .publish_checkpoint_frame(frame.clone(), prefix, authenticated)
                    .await;
                if expected_delivery {
                    assert!(
                        !fixture.recipient_rx.is_empty(),
                        "{case} checkpoint not delivered (prefix={prefix}, time={time})"
                    );
                    fixture.assert_checkpoint_written(&frame).await;
                } else {
                    assert!(
                        fixture.recipient_rx.try_recv().is_err(),
                        "{case} bypassed checkpoint gate (prefix={prefix})"
                    );
                }
                assert!(
                    !fixture
                        .workspace
                        .lock()
                        .await
                        .needs_checkpoint(&fixture.recipient.device_public_key, &channel),
                    "{case} did not mark generic checkpoint served"
                );
            }
            assert!(fixture.recipient_disconnected.try_recv().is_err());
        }
    }
}

#[tokio::test]
async fn ordered_checkpoints_reach_a_device_once_per_channel_until_rearmed() {
    let mut fixture = Fixture::new();

    fixture.enqueue_checkpoint(1, vec![1; 64]).await;
    fixture.drained().await;
    assert!(
        fixture.recipient_rx.try_recv().is_ok(),
        "initial checkpoint was not delivered"
    );

    fixture
        .enqueue_checkpoint(20, vec![2; 16 * 1024 * 1024])
        .await;
    fixture.drained().await;
    assert!(
        fixture.recipient_rx.try_recv().is_err(),
        "an already-current device received the next idle checkpoint"
    );
    fixture.enqueue(2).await;
    fixture.drained().await;
    assert!(
        fixture.recipient_rx.try_recv().is_ok(),
        "a large gated checkpoint obstructed the next ordered delta"
    );

    fixture
        .workspace
        .lock()
        .await
        .request_checkpoint(&fixture.recipient.device_public_key);
    fixture.enqueue_checkpoint(3, vec![3; 64]).await;
    fixture.drained().await;
    assert!(
        fixture.recipient_rx.try_recv().is_ok(),
        "rearmed checkpoint was not delivered"
    );

    fixture.enqueue_checkpoint(4, vec![4; 64]).await;
    fixture.drained().await;
    assert!(
        fixture.recipient_rx.try_recv().is_err(),
        "rearm delivered more than one checkpoint"
    );
    assert!(fixture.recipient_disconnected.try_recv().is_err());
}

#[tokio::test]
async fn ordered_checkpoint_is_marked_served_only_after_successful_enqueue() {
    let mut fixture = Fixture::with_delivery_capacity(1);
    fixture.enqueue(0).await;
    fixture.drained().await;

    fixture.enqueue_checkpoint(1, vec![1; 64]).await;
    fixture.drained().await;
    assert!(
        fixture
            .workspace
            .lock()
            .await
            .needs_checkpoint(&fixture.recipient.device_public_key, &[3; 32],),
        "a full delivery queue incorrectly marked the checkpoint served"
    );

    fixture.recipient_rx.try_recv().unwrap();
    fixture.enqueue_checkpoint(2, vec![2; 64]).await;
    fixture.drained().await;
    assert!(
        fixture.recipient_rx.try_recv().is_ok(),
        "the checkpoint was not retried after capacity returned"
    );
    assert!(
        !fixture
            .workspace
            .lock()
            .await
            .needs_checkpoint(&fixture.recipient.device_public_key, &[3; 32],)
    );
}

#[tokio::test]
async fn ordered_checkpoints_remain_live_for_peer_relays() {
    let mut fixture = Fixture::new();
    fixture.recipient.role = PeerRole::PeerRelay;
    fixture.recipient.cert.capabilities.can_act_as_host = true;
    tilo_user_identity::sign_device_cert(&mut fixture.recipient.cert, &fixture.root_key).unwrap();
    {
        let mut runtime = fixture.workspace.lock().await;
        runtime.record_peer_relay(fixture.recipient.device_public_key);
        runtime
            .subscribers
            .get_mut(&fixture.recipient.device_public_key)
            .unwrap()
            .role = PeerRole::PeerRelay;
        runtime
            .state_sessions
            .get_mut(&fixture.recipient.device_public_key)
            .unwrap()
            .peer = fixture.recipient.clone();
    }

    fixture.enqueue_checkpoint(1, vec![1; 64]).await;
    fixture.drained().await;
    assert!(fixture.recipient_rx.try_recv().is_ok());
    fixture.enqueue_checkpoint(2, vec![2; 64]).await;
    fixture.drained().await;
    assert!(
        fixture.recipient_rx.try_recv().is_ok(),
        "peer relay checkpoint was incorrectly gated"
    );
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
async fn a_commit_is_acknowledged_only_once_it_is_retained() {
    let fixture = Fixture::new();
    let (release, reached) =
        crate::store::test_barrier::install(fixture.store.root(), "state-persist");
    let session = SessionCtx {
        workspaces: &fixture.registry,
        autobind: None,
        outbound: &fixture.source.tx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &fixture.source.own_user,
        own_device_cert: &fixture.source.peer.cert,
        own_device_public_key: &fixture.source.peer.cert.device_public_key,
        app_serving: None,
        metrics: &fixture.metrics,
        trust_policy: &fixture.source.policy,
        disconnect: &fixture.source.disconnect,
        v2: true,
        private_tx: &fixture.private,
        revocations: &fixture.source.revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let Envelope::Delta { delta, .. } = fixture.envelope(1) else {
        unreachable!()
    };
    let commit = Envelope::Commit {
        workspace_id: 7,
        op: [5; 16],
        author_device_public_key: fixture.source.peer.device_public_key,
        authored_at_ns: 1,
        delta,
    };
    let mut committing = Box::pin(handle_commit(&fixture.source.peer, &session, &[7], commit));
    tokio::select! {
        _ = &mut committing => panic!("a commit was acknowledged before it was retained"),
        reached = reached => reached.unwrap(),
    }
    assert!(fixture.store.load_retained().unwrap().is_empty());
    drop(release);
    let acknowledged = tokio::time::timeout(std::time::Duration::from_secs(30), committing)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(acknowledged, Some(Envelope::Committed { op, .. }) if op == [5; 16]));
    assert_eq!(fixture.store.load_retained().unwrap().len(), 1);
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
async fn full_state_delivery_queue_preserves_live_session_and_durable_pending() {
    let mut fixture = Fixture::new();
    let (state_tx, state_rx) = mpsc::channel(1);
    fixture.recipient_rx = state_rx;
    fixture
        .workspace
        .lock()
        .await
        .state_sessions
        .get_mut(&fixture.recipient.device_public_key)
        .unwrap()
        .tx = state_tx;

    fixture.enqueue(0).await;
    fixture.drained().await;
    fixture.enqueue(3).await;
    fixture.drained().await;

    let runtime = fixture.workspace.lock().await;
    assert!(
        fixture.recipient_disconnected.try_recv().is_err(),
        "a full durable state queue must not disconnect its live recipient"
    );
    assert!(
        runtime
            .subscribers
            .contains_key(&fixture.recipient.device_public_key),
        "a backpressured recipient must remain subscribed"
    );
    assert!(
        runtime
            .state_sessions
            .contains_key(&fixture.recipient.device_public_key),
        "a backpressured recipient must retain its ordered state session"
    );
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        2,
        "both the queued and backpressured durable inputs must remain pending"
    );
}

#[tokio::test]
async fn closed_state_delivery_queue_disconnects_and_prunes_exact_session() {
    let mut fixture = Fixture::new();
    fixture.recipient_rx.close();

    fixture.enqueue(0).await;
    fixture.drained().await;

    let runtime = fixture.workspace.lock().await;
    assert!(
        fixture.recipient_disconnected.try_recv().is_ok(),
        "a closed state queue must disconnect its recipient"
    );
    assert!(
        !runtime
            .subscribers
            .contains_key(&fixture.recipient.device_public_key),
        "a closed recipient must be pruned"
    );
    assert!(
        !runtime
            .state_sessions
            .contains_key(&fixture.recipient.device_public_key),
        "a closed recipient's ordered state session must be pruned"
    );
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        1,
        "a closed wire cannot acknowledge its durable pending input"
    );
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
            fixture.revoke(key);
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
async fn inbound_delta_waits_for_owner_capacity_without_failing_its_session() {
    let fixture = Fixture::new();
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
    for _ in 0..32 {
        owner.read(1, |_| Ok(())).unwrap();
    }

    let workspace = fixture.workspace.clone();
    let envelope = fixture.envelope(0);
    let source = fixture.source.clone();
    let metrics = fixture.metrics.clone();
    let mut admitting = tokio::spawn(async move {
        enqueue_state(
            &workspace,
            envelope,
            Some(source),
            PeerRole::Device,
            metrics,
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut admitting)
            .await
            .is_err(),
        "a full persistence owner rejected a healthy inbound state stream"
    );

    drop(release);
    tilo_test_env::within(
        "inbound state admission did not resume when owner capacity returned",
        admitting,
    )
    .await
    .unwrap()
    .unwrap();
    fixture.drained_wait().await;
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
        tilo_user_identity::sign_device_cert(&mut fixture.source.peer.cert, &fixture.root_key)
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
            fixture.revoke_source();
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
    Box::pin(enqueue_state_inner(
        &fixture.workspace,
        envelope.clone(),
        Some(fixture.source.clone()),
        PeerRole::PeerRelay,
        fixture.metrics.clone(),
        true,
    ))
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
    Box::pin(enqueue_state_inner(
        &fixture.workspace,
        envelope,
        Some(fixture.source.clone()),
        PeerRole::PeerRelay,
        fixture.metrics.clone(),
        true,
    ))
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
async fn sync_request_waits_for_owner_capacity_and_rechecks_source() {
    use automerge::transaction::Transactable;

    for change in [
        "unchanged",
        "unsubscribed",
        "replaced",
        "revoked",
        "removed",
        "closed",
        "cancelled",
    ] {
        let mut fixture = Fixture::new();
        let mut document = automerge::AutoCommit::new();
        document
            .put(automerge::ROOT, "value", "sync after capacity")
            .unwrap();
        let expected_heads = document.get_heads();
        fixture.workspace.lock().await.doc = Some(document);
        let device = fixture.source.peer.device_public_key;
        if change == "unsubscribed" {
            fixture.workspace.lock().await.remove_subscriber(&device);
        }
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
        let mut queued = Vec::new();
        for _ in 0..32 {
            queued.push(owner.read(1, |_| Ok(())).unwrap());
        }
        let registry = fixture.registry.clone();
        let source = fixture.source.clone();
        let private = fixture.private.clone();
        let metrics = fixture.metrics.clone();
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let connection_live = live.clone();
        let mut task = tokio::spawn(async move {
            let session = SessionCtx {
                workspaces: &registry,
                autobind: None,
                outbound: &source.tx,
                state_outbound: None,
                connection_live: Some(&connection_live),
                own_user_public_key: &source.own_user,
                own_device_cert: &source.peer.cert,
                own_device_public_key: &source.peer.cert.device_public_key,
                app_serving: None,
                metrics: &metrics,
                trust_policy: &source.policy,
                disconnect: &source.disconnect,
                v2: true,
                private_tx: &private,
                revocations: &source.revocations,
                live_bridge: None,
                agents: Box::leak(Box::default()),
                positions: None,
                compactor: None,
                resumes: Box::leak(Box::default()),
                lag: Box::leak(Box::default()),
            };
            handle_sync_request(&source.peer, &session, 7, vec![]).await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut task)
                .await
                .is_err(),
            "full owner queue rejected SyncRequest ({change})"
        );
        match change {
            "replaced" => {
                let (replacement, _rx) = mpsc::channel(32);
                fixture.workspace.lock().await.add_subscriber(Subscriber {
                    device_public_key: device,
                    role: PeerRole::Device,
                    tx: replacement,
                    disconnect: fixture.source.disconnect.clone(),
                });
            }
            "revoked" => {
                fixture.revoke(device);
            }
            "removed" => crate::workspace_io::replace_membership(
                &fixture.workspace,
                vec![fixture.recipient.device_public_key],
            )
            .await
            .unwrap(),
            "closed" => live.store(false, std::sync::atomic::Ordering::SeqCst),
            "cancelled" => task.abort(),
            _ => {}
        }
        // Cancellation must remove the capacity waiter before the owner resumes.
        if change == "cancelled" {
            assert!(matches!((&mut task).await, Err(error) if error.is_cancelled()));
        }
        drop(release);
        if change != "cancelled" {
            assert!(
                tilo_test_env::within("task to complete", task)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none()
            );
        }
        for receipt in queued {
            crate::persistence::receipt(receipt).await.unwrap();
        }
        fixture.drained().await;
        if matches!(change, "unchanged" | "unsubscribed") {
            let Envelope::SyncResponse {
                workspace_id,
                delta_since,
            } = fixture.receivers[0]
                .try_recv()
                .expect("missing authorized sync response")
            else {
                panic!("unexpected sync reply")
            };
            assert_eq!(workspace_id, 7);
            let mut received = automerge::AutoCommit::new();
            tilo_automerge::ApplyIncoming::load_incoming(&mut received, &delta_since).unwrap();
            assert_eq!(received.get_heads(), expected_heads);
        }
        assert!(
            fixture.receivers[0].try_recv().is_err(),
            "SyncRequest published an extra or unauthorized response ({change})"
        );
    }
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
        own_device_cert: &fixture.source.peer.cert,
        own_device_public_key: &fixture.source.peer.cert.device_public_key,
        app_serving: None,
        metrics: &fixture.metrics,
        trust_policy: &fixture.source.policy,
        disconnect: &fixture.source.disconnect,
        v2: true,
        private_tx: &fixture.private,
        revocations: &fixture.source.revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
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
        fixture.receivers[0].try_recv().is_err(),
        "unsubscribed old source published a sync response after replacement"
    );
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
    let workers = crate::persistence::isolated_test_workers();
    let fixtures = (0..5)
        .map(|_| Fixture::with_delivery_capacity_and_workers(32, workers.clone()))
        .collect::<Vec<_>>();
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
        tilo_test_env::hang_bound(),
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
    async fn subscribe_result(
        &self,
    ) -> Result<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>), PeerError> {
        let (state_tx, _state_rx) = mpsc::channel(32);
        let session = SessionCtx {
            workspaces: &self.registry,
            autobind: None,
            outbound: &self.recipient_tx,
            state_outbound: Some(&state_tx),
            connection_live: None,
            own_user_public_key: &self.source.own_user,
            own_device_cert: &self.source.peer.cert,
            own_device_public_key: &self.source.peer.cert.device_public_key,
            app_serving: None,
            metrics: &self.metrics,
            trust_policy: &self.source.policy,
            disconnect: &self.recipient_disconnect,
            v2: true,
            private_tx: &self.private,
            revocations: &self.source.revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let mut replay = None;
        handle_subscribe(
            &self.recipient,
            &session,
            &mut Vec::new(),
            &mut replay,
            self.store.workspace_id(),
            Subscription::Whole,
        )
        .await?;
        replay.ok_or_else(|| PeerError::Protocol("subscribe produced no replay".into()))
    }

    async fn subscribe_ticket(&self) -> (u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>) {
        self.subscribe_result().await.unwrap()
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

#[tokio::test]
async fn replay_capture_waits_for_owner_capacity_and_can_be_cancelled() {
    for cancel in [false, true] {
        let fixture = Arc::new(Fixture::new());
        fixture
            .workspace
            .lock()
            .await
            .remove_subscriber(&fixture.recipient.device_public_key);
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
        let mut queued = Vec::new();
        for _ in 0..32 {
            queued.push(owner.read(1, |_| Ok(())).unwrap());
        }
        let subscribing = fixture.clone();
        let mut task = tokio::spawn(async move { subscribing.subscribe_ticket().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut task)
                .await
                .is_err(),
            "full owner queue rejected replay capture"
        );
        if cancel {
            task.abort();
        }
        drop(release);
        if cancel {
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        } else {
            let (_, ticket, _, _) = tilo_test_env::within("task to complete", task)
                .await
                .unwrap();
            assert!(
                matches!(
                    queued.last_mut().unwrap().try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                ),
                "priority capture drained the normal backlog first"
            );
            drop(ticket);
        }
        for receipt in queued {
            crate::persistence::receipt(receipt).await.unwrap();
        }
        fixture.drained().await;
        assert_eq!(
            fixture
                .workspace
                .lock()
                .await
                .subscribers
                .contains_key(&fixture.recipient.device_public_key),
            !cancel
        );
    }
}

#[tokio::test]
async fn replay_owner_wait_drains_private_replies_and_rechecks_authority() {
    // A full queue exercises admission; one free slot exercises the receipt.
    for (queued_count, replace_session) in [(32, false), (31, false), (32, true)] {
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
        let ticket = fixture.subscribe_ticket().await;
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
        let queued = (0..queued_count)
            .map(|_| owner.read(1, |_| Ok(())).unwrap())
            .collect::<Vec<_>>();
        let (private_tx, mut private_rx) = mpsc::channel(1);
        let reply = EnvelopeV2::ActionsReady {
            room: uuid::Uuid::new_v4(),
            capability: tilo_delivery_protocol::transport_v2::ActionCapability::TransportOnly,
        };
        private_tx
            .send(PrivateOutbound::Reply(reply.clone()))
            .await
            .unwrap();
        let (mut write, read) = tokio::io::duplex(4096);
        let writer = fixture.clone();
        let task = tokio::spawn(async move {
            let session = SessionCtx {
                workspaces: &writer.registry,
                autobind: None,
                outbound: &writer.recipient_tx,
                state_outbound: None,
                connection_live: None,
                own_user_public_key: &writer.source.own_user,
                own_device_cert: &writer.source.peer.cert,
                own_device_public_key: &writer.source.peer.cert.device_public_key,
                app_serving: None,
                metrics: &writer.metrics,
                trust_policy: &writer.source.policy,
                disconnect: &writer.source.disconnect,
                v2: true,
                private_tx: &writer.private,
                revocations: &writer.source.revocations,
                live_bridge: None,
                agents: Box::leak(Box::default()),
                positions: None,
                compactor: None,
                resumes: Box::leak(Box::default()),
                lag: Box::leak(Box::default()),
            };
            Box::pin(finish_replay_write(
                ReplayWrite {
                    ticket: Some(ticket),
                    reply: None,
                    replaying: true,
                },
                &writer.recipient,
                &session,
                &std::sync::atomic::AtomicUsize::new(1),
                &mut private_rx,
                &mut write,
            ))
            .await
        });
        let mut reader = tilo_delivery_protocol::transport_v2::SessionReader::new(read, true);
        assert_eq!(
            tilo_test_env::within(
                "private reply stalled behind replay owner capacity",
                reader.read_next()
            )
            .await
            .unwrap(),
            Some(reply)
        );
        assert!(!task.is_finished());
        if replace_session {
            let (replacement, _rx) = mpsc::channel(32);
            fixture.workspace.lock().await.add_subscriber(Subscriber {
                device_public_key: device,
                role: PeerRole::Device,
                tx: replacement,
                disconnect: fixture.source.disconnect.clone(),
            });
        }
        drop(release);
        tilo_test_env::within("admitted owner queue did not drain", async {
            for receipt in queued {
                crate::persistence::receipt(receipt).await.unwrap();
            }
        })
        .await;
        let result = tilo_test_env::within("task to complete", task)
            .await
            .unwrap();
        if replace_session {
            assert!(result.is_err());
            assert_eq!(fixture.store.count_pending(&device).unwrap(), 1);
            assert!(reader.read_next().await.unwrap().is_none());
        } else {
            result.unwrap();
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Delta { .. }))
            ));
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ready { .. }))
            ));
            assert_eq!(fixture.store.count_pending(&device).unwrap(), 0);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_direct_wait_keeps_replay_live_while_private_delivery_waits_for_its_lane() {
    let room = uuid::Uuid::from_u128(7);
    let room_id = crate::room_workspace_id(&room.to_string()).unwrap();
    let fixture = Arc::new(Fixture::with_workspace_id(room_id));
    let (_, ticket, _, _) = fixture.subscribe_ticket().await;
    let (source_private, _source_private_rx) = mpsc::channel(1);
    let (recipient_private, _recipient_private_rx) = mpsc::channel(1);
    {
        let mut runtime = fixture.workspace.lock().await;
        runtime.action_targets.insert(
            fixture.source.peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: fixture.source.peer.clone(),
                state_tx: fixture.source.tx.clone(),
                tx: source_private,
            },
        );
        runtime.action_targets.insert(
            fixture.recipient.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: fixture.recipient.clone(),
                state_tx: fixture.recipient_tx.clone(),
                tx: recipient_private,
            },
        );
    }
    let (private_tx, mut private_rx) = mpsc::channel(1);
    let (reply, _reply_rx) = mpsc::channel(1);
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: fixture.source.peer.device_public_key,
        target: fixture.recipient.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    let delivery = PrivateDelivery {
        certified: false,
        action: action.clone(),
        source: fixture.source.peer.clone(),
        source_tx: fixture.source.tx.clone(),
        reply,
        source_disconnect: fixture.source.disconnect.clone(),
    };
    let root = fixture.store.root().to_path_buf();
    let (release_read, read_reached) =
        crate::store::test_barrier::install(&root, "membership-read");
    let (release_pending, lane_pending) =
        crate::store::test_barrier::install(&root, "delivery-membership-lane-pending");
    let writer = fixture.clone();
    let task = tokio::spawn(async move {
        let session = SessionCtx {
            workspaces: &writer.registry,
            autobind: None,
            outbound: &writer.recipient_tx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &writer.source.own_user,
            own_device_cert: &writer.source.peer.cert,
            own_device_public_key: &writer.source.peer.cert.device_public_key,
            app_serving: None,
            metrics: &writer.metrics,
            trust_policy: &writer.source.policy,
            disconnect: &writer.source.disconnect,
            v2: true,
            private_tx: &writer.private,
            revocations: &writer.source.revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let source = StateSource::new(&writer.recipient, &session);
        let authority = ticket.authority.clone();
        let replay = async {
            let _ticket = ticket;
            authorize_replay(&writer.workspace, &source, &authority).await
        };
        let mut wire = Vec::new();
        let result = replay_direct_wait(
            replay,
            &writer.recipient,
            &session,
            &mut private_rx,
            &mut wire,
        )
        .await;
        (result, wire)
    });

    read_reached.await.unwrap();
    private_tx
        .send(PrivateOutbound::Delivery(delivery))
        .await
        .unwrap();
    lane_pending.await.unwrap();
    drop(release_pending);
    drop(release_read);

    let mut task = task;
    let joined = tokio::time::timeout(tilo_test_env::hang_bound(), &mut task).await;
    let (result, wire) = if let Ok(joined) = joined {
        joined.unwrap()
    } else {
        task.abort();
        let _ = task.await;
        panic!("private delivery stopped replay from releasing its authorization lane");
    };
    result.unwrap();
    let mut reader = SessionReader::new(wire.as_slice(), true);
    assert!(matches!(
        reader.read_next().await.unwrap(),
        Some(EnvelopeV2::Delivered {
            action: delivered,
            ..
        }) if delivered == action
    ));
    assert!(reader.read_next().await.unwrap().is_none());
}

#[tokio::test]
async fn replay_direct_wait_finishes_active_private_frame_before_replay_error() {
    struct GatedWrite {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        release: tokio::sync::oneshot::Receiver<()>,
        bytes: Vec<u8>,
    }
    impl tokio::io::AsyncWrite for GatedWrite {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            use std::{future::Future as _, task::Poll};

            if let Some(started) = self.started.take() {
                self.bytes.push(bytes[0]);
                let _ = started.send(());
                return Poll::Ready(Ok(1));
            }
            match std::pin::Pin::new(&mut self.release).poll(cx) {
                Poll::Ready(Ok(())) => {
                    self.bytes.extend_from_slice(bytes);
                    Poll::Ready(Ok(bytes.len()))
                }
                Poll::Ready(Err(_)) => {
                    Poll::Ready(Err(std::io::Error::other("private write release dropped")))
                }
                Poll::Pending => Poll::Pending,
            }
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    let fixture = Arc::new(Fixture::new());
    let (private_tx, mut private_rx) = mpsc::channel(1);
    let frame = EnvelopeV2::ActionsReady {
        room: uuid::Uuid::from_u128(9),
        capability: tilo_delivery_protocol::transport_v2::ActionCapability::TransportOnly,
    };
    private_tx
        .send(PrivateOutbound::Reply(frame.clone()))
        .await
        .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let writer = fixture.clone();
    let task = tokio::spawn(async move {
        let session = SessionCtx {
            workspaces: &writer.registry,
            autobind: None,
            outbound: &writer.recipient_tx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &writer.source.own_user,
            own_device_cert: &writer.source.peer.cert,
            own_device_public_key: &writer.source.peer.cert.device_public_key,
            app_serving: None,
            metrics: &writer.metrics,
            trust_policy: &writer.source.policy,
            disconnect: &writer.source.disconnect,
            v2: true,
            private_tx: &writer.private,
            revocations: &writer.source.revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let operation = async move {
            complete_rx.await.unwrap();
            completed_tx.send(()).unwrap();
            Err::<(), _>(crate::persistence::error("replay failed"))
        };
        let mut wire = GatedWrite {
            started: Some(started_tx),
            release: release_rx,
            bytes: Vec::new(),
        };
        let result = replay_direct_wait(
            operation,
            &writer.recipient,
            &session,
            &mut private_rx,
            &mut wire,
        )
        .await;
        (result, wire.bytes)
    });

    started_rx.await.unwrap();
    complete_tx.send(()).unwrap();
    completed_rx.await.unwrap();
    assert!(
        !task.is_finished(),
        "replay error interrupted an active private wire frame"
    );
    release_tx.send(()).unwrap();
    let (result, bytes) = task.await.unwrap();
    assert!(result.is_err());
    let mut reader = SessionReader::new(bytes.as_slice(), true);
    assert_eq!(reader.read_next().await.unwrap(), Some(frame));
    assert!(reader.read_next().await.unwrap().is_none());
}

#[tokio::test]
async fn detached_replay_catches_up_pending_without_replaying_selected_baseline() {
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
    fixture.enqueue(1).await;
    fixture.drained_wait().await;

    let (prepare_release, prepare_reached) =
        crate::store::test_barrier::install(fixture.store.root(), "replay-manifest-prepare");
    let (ready_release, ready_reached) =
        crate::store::test_barrier::install(fixture.store.root(), "replay-before-ready");
    let (mut write, read) = tokio::io::duplex(1024 * 1024);
    let writer = fixture.clone();
    let replay =
        tokio::spawn(async move { Box::pin(writer.replay_to_with_deliveries(&mut write)).await });
    prepare_reached.await.unwrap();

    fixture.enqueue(2).await;
    for kind in 3..42 {
        fixture.enqueue(kind).await;
    }
    // The held replay barrier makes this an ordering assertion: owner work can
    // finish only if detached preparation released the mutation lane. It is
    // not a latency budget; a preparation that blocks the owner hangs here and
    // nextest's slow-timeout reports it.
    fixture.drained_wait().await;
    drop(prepare_release);
    ready_reached.await.unwrap();
    fixture.enqueue(43).await;
    fixture.drained_wait().await;
    drop(ready_release);

    let mut reader = tilo_delivery_protocol::transport_v2::SessionReader::new(read, true);
    let mut deltas = Vec::new();
    let ready = loop {
        match reader.read_next().await.unwrap() {
            Some(EnvelopeV2::State(envelope @ Envelope::Delta { .. })) => deltas.push(envelope),
            Some(EnvelopeV2::State(Envelope::Ready { pending_deltas, .. })) => {
                break pending_deltas;
            }
            frame => panic!("unexpected replay frame: {frame:?}"),
        }
    };
    let mut post_ready = replay.await.unwrap().unwrap();
    assert!(deltas.contains(&fixture.envelope(0)));
    assert_eq!(
        deltas
            .iter()
            .filter(|envelope| **envelope == fixture.envelope(1))
            .count(),
        1,
        "final catch-up replayed the already-written selected baseline"
    );
    assert!(deltas.contains(&fixture.envelope(2)));
    assert!(
        deltas
            .iter()
            .position(|envelope| *envelope == fixture.envelope(1))
            < deltas
                .iter()
                .position(|envelope| *envelope == fixture.envelope(2)),
        "selected checkpoint and post-cut tail lost coherent order"
    );
    for kind in 3..42 {
        assert!(
            deltas.contains(&fixture.envelope(kind)),
            "catch-up omitted post-cut commit {kind}"
        );
    }
    assert!(!deltas.contains(&fixture.envelope(43)));
    assert_eq!(ready as usize, deltas.len());
    let delivery = post_ready
        .try_recv()
        .expect("post-boundary commit was not queued behind Ready");
    assert_eq!(*delivery.envelope, fixture.envelope(43));
    assert_eq!(
        fixture.store.count_pending(&device).unwrap(),
        1,
        "only the post-Ready live delivery should remain pending"
    );
    assert!(
        delivery
            .cleanup
            .as_ref()
            .is_some_and(|pending| pending.exists()),
        "post-Ready live delivery lost its durable cleanup entry"
    );
    fixture.write(delivery, &mut Vec::new()).await;
    fixture.drained_wait().await;
    assert_eq!(
        fixture.store.count_pending(&device).unwrap(),
        0,
        "acknowledged initial, catch-up, and post-Ready entries were not retired"
    );
}

#[tokio::test]
async fn cancelled_before_pending_catchup_preserves_semantic_update_for_reconnect() {
    let fixture = Arc::new(Fixture::new());
    fixture.enqueue(1).await;
    fixture.drained_wait().await;
    let (prepare_release, prepare_reached) =
        crate::store::test_barrier::install(fixture.store.root(), "replay-manifest-prepare");
    let (capture_release, capture_reached) =
        crate::store::test_barrier::install(fixture.store.root(), "replay-pending-capture");
    let replaying = fixture.clone();
    let task = tokio::spawn(async move { Box::pin(replaying.replay_to(&mut Vec::new())).await });
    prepare_reached.await.unwrap();
    fixture.enqueue(2).await;
    fixture.drained_wait().await;
    drop(prepare_release);
    capture_reached.await.unwrap();
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        1,
        "post-cut retained output was not durable before catch-up"
    );
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    drop(capture_release);
    fixture.drained_wait().await;
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        1,
        "cancelled catch-up retired an unwritten semantic update"
    );

    let mut wire = Vec::new();
    Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
    let mut reader =
        tilo_delivery_protocol::transport_v2::SessionReader::new(wire.as_slice(), true);
    let mut tail = 0;
    loop {
        match reader.read_next().await.unwrap() {
            Some(EnvelopeV2::State(envelope @ Envelope::Delta { .. })) => {
                if envelope == fixture.envelope(2) {
                    tail += 1;
                }
            }
            Some(EnvelopeV2::State(Envelope::Ready { .. })) => break,
            frame => panic!("unexpected reconnect replay frame: {frame:?}"),
        }
    }
    assert_eq!(tail, 1);
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn replay_boundary_cross_workspace_full_live_queue_fences_exact_session() {
    let mut fixture = Fixture::with_delivery_capacity(1);
    let live = {
        let mut runtime = fixture.workspace.lock().await;
        let session = runtime
            .state_sessions
            .get_mut(&fixture.recipient.device_public_key)
            .unwrap();
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
            })
            .unwrap();
        let catchup = Arc::new(ReplayCatchup::new());
        catchup
            .phase
            .store(ReplayCatchup::BOUNDARY, std::sync::atomic::Ordering::SeqCst);
        session.replay = Some(catchup);
        session.live.clone()
    };
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
async fn replay_boundary_live_byte_exhaustion_fences_exact_session() {
    let mut fixture = Fixture::new();
    let live = {
        let mut runtime = fixture.workspace.lock().await;
        let session = runtime
            .state_sessions
            .get_mut(&fixture.recipient.device_public_key)
            .unwrap();
        let catchup = Arc::new(ReplayCatchup::new());
        catchup
            .phase
            .store(ReplayCatchup::BOUNDARY, std::sync::atomic::Ordering::SeqCst);
        session.replay = Some(catchup);
        session.live.clone()
    };
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
async fn replay_fenced_before_final_ready_never_writes_ready() {
    let fixture = Arc::new(Fixture::new());
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
    let (state_tx, live) = {
        let runtime = fixture.workspace.lock().await;
        let session = runtime
            .state_sessions
            .get(&fixture.recipient.device_public_key)
            .unwrap();
        (session.tx.clone(), session.live.clone())
    };
    for authored_at_ns in 0..32 {
        let mut cross_workspace = fixture.envelope(3);
        if let Envelope::Delta {
            workspace_id,
            authored_at_ns: current,
            ..
        } = &mut cross_workspace
        {
            *workspace_id = 99;
            *current = authored_at_ns;
        }
        state_tx
            .try_send(StateDelivery {
                envelope: LiveEnvelope::Original(Arc::new(cross_workspace)),
                cleanup: None,
                source: None,
                _budget: None,
                live: live.clone(),
            })
            .unwrap();
    }
    fixture.enqueue(4).await;
    fixture.drained().await;
    assert!(
        !live.load(std::sync::atomic::Ordering::SeqCst),
        "post-boundary overflow did not fence the replay lease"
    );
    drop(release);
    let (result, wire) = task.await.unwrap();
    assert!(result.is_err());
    let mut reader = SessionReader::new(wire.as_slice(), true);
    while let Some(frame) = reader.read_next().await.unwrap() {
        assert!(
            !matches!(frame, EnvelopeV2::State(Envelope::Ready { .. })),
            "fenced replay emitted Ready"
        );
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
    tilo_test_env::within("idle owner did not reclaim cancelled capture", async {
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
    .await;
    assert_eq!(store.count_pending(&device).unwrap(), 1);
}

impl Fixture {
    async fn replay_to<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        write: &mut W,
    ) -> Result<(), PeerError> {
        Box::pin(self.replay_to_with_deliveries(write))
            .await
            .map(|_| ())
    }

    async fn replay_to_with_deliveries<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        write: &mut W,
    ) -> Result<mpsc::Receiver<StateDelivery>, PeerError> {
        let (state_tx, state_rx) = mpsc::channel(32);
        let session = SessionCtx {
            workspaces: &self.registry,
            autobind: None,
            outbound: &self.recipient_tx,
            state_outbound: Some(&state_tx),
            connection_live: None,
            own_user_public_key: &self.source.own_user,
            own_device_cert: &self.source.peer.cert,
            own_device_public_key: &self.source.peer.cert.device_public_key,
            app_serving: None,
            metrics: &self.metrics,
            trust_policy: &self.source.policy,
            disconnect: &self.recipient_disconnect,
            v2: true,
            private_tx: &self.private,
            revocations: &self.source.revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        Box::pin(handle_inbound_frame(
            Envelope::Subscribe {
                workspace_id: self.store.workspace_id(),
            },
            &self.recipient,
            &session,
            &mut Vec::new(),
            write,
        ))
        .await?;
        Ok(state_rx)
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
        let task = tokio::spawn(async move { Box::pin(writer.replay_to(&mut Vec::new())).await });
        reached.await.unwrap();
        match change {
            0 => {
                fixture.revoke(device);
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
    let (_, ticket, handle, _) = fixture.subscribe_ticket().await;
    let owner = crate::persistence::handle(&fixture.workspace).await;
    let authority = ticket.authority.clone();
    let finishing = handle.clone();
    assert!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state.finish_replay(
                    &authority,
                    &finishing,
                    |_| Ok(())
                ))
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
            ReplayAdvance::Exhausted(_) => panic!("empty replay bypassed its batch receipt"),
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
    let finishing = handle.clone();
    assert!(matches!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state.finish_replay(
                    &authority,
                    &finishing,
                    |_| Ok(())
                ))
                .unwrap()
        )
        .await
        .unwrap(),
        Step::Done(_)
    ));
    let authority = ticket.authority.clone();
    let finishing = handle.clone();
    assert!(
        crate::persistence::receipt(
            owner
                .read(1, move |state| state.finish_replay(
                    &authority,
                    &finishing,
                    |_| Ok(())
                ))
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
        self.remaining = self.remaining.saturating_sub(written);
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
                &crate::transport::encode(&fixture.envelope(u8::try_from(i).unwrap())).unwrap(),
            )
            .unwrap();
    }
    let first_batch_bytes: usize = (0..8)
        .map(|i| {
            tilo_delivery_protocol::transport_v2::encode_state(&fixture.envelope(i), true)
                .unwrap()
                .len()
        })
        .sum();
    assert!(
        Box::pin(fixture.replay_to(&mut InterruptWrite {
            remaining: first_batch_bytes + 3
        }))
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
    Box::pin(fixture.replay_to(&mut wire)).await.unwrap();
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
    let (_, ticket, _, _) = fixture.subscribe_ticket().await;
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
    tilo_test_env::within(
        "cancelled ticket was stranded by a full owner queue",
        async {
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
        },
    )
    .await;
    assert_eq!(
        fixture
            .store
            .count_pending(&fixture.recipient.device_public_key)
            .unwrap(),
        257
    );
}

#[tokio::test]
async fn native_sync_refresh_uses_live_subscribed_source_and_rechecks_execution() {
    for change in [
        "current",
        "closed",
        "replaced",
        "unsubscribed",
        "revoked",
        "removed",
        "expired",
    ] {
        let fixture = Fixture::new();
        let (mailbox, mut inbox) = sync_refresh::Mailbox::bounded();
        fixture.workspace.lock().await.sync_refresh = Some(mailbox);
        let source = &fixture.source;
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let session = SessionCtx {
            workspaces: &fixture.registry,
            autobind: None,
            outbound: &source.tx,
            state_outbound: None,
            connection_live: Some(&live),
            own_user_public_key: &source.own_user,
            own_device_cert: &source.peer.cert,
            own_device_public_key: &source.peer.cert.device_public_key,
            app_serving: None,
            metrics: &fixture.metrics,
            trust_policy: &source.policy,
            disconnect: &source.disconnect,
            v2: true,
            private_tx: &fixture.private,
            revocations: &source.revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        handle_sync_request(&source.peer, &session, 7, vec![])
            .await
            .unwrap();
        let request = inbox
            .try_recv()
            .expect("authenticated SyncRequest reaches actual owner mailbox");
        match change {
            "closed" => live.store(false, std::sync::atomic::Ordering::SeqCst),
            "expired" => tokio::time::sleep(std::time::Duration::from_millis(3010)).await,
            "unsubscribed" => {
                fixture
                    .workspace
                    .lock()
                    .await
                    .remove_subscriber(&source.peer.device_public_key);
            }
            "replaced" => {
                let (replacement, _rx) = mpsc::channel(32);
                fixture.workspace.lock().await.add_subscriber(Subscriber {
                    device_public_key: source.peer.device_public_key,
                    role: source.peer.role,
                    tx: replacement,
                    disconnect: source.disconnect.clone(),
                });
            }
            "revoked" => {
                fixture.revoke_source();
            }
            "removed" => crate::workspace_io::replace_membership(
                &fixture.workspace,
                vec![fixture.recipient.device_public_key],
            )
            .await
            .unwrap(),
            _ => {}
        }
        let mut ran = false;
        let result = request
            .execute(|workspace, cert| {
                assert_eq!(workspace, 7);
                assert_eq!(cert, &source.peer.cert);
                ran = true;
                Ok(())
            })
            .await;
        assert_eq!(ran, change == "current", "{change}");
        assert_eq!(result.is_ok(), change == "current", "{change}");
    }
}
