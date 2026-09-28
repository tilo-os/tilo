//! The live delivery path keeps each member's un-sent state within its
//! pending-queue bound while a fast publisher keeps sending.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use super::private_transport_tests::certified_device;
use super::*;
use automerge::transaction::Transactable;
use tilo_automerge::ApplyIncoming;
use tilo_user_identity::{RevocationList, UserIdentity};

/// A member whose connection stopped draining falls behind a fast publisher.
/// Its queued live state never exceeds the bound: once over it, the queued
/// changes are replaced by the pinned document, the member is asked for a
/// checkpoint (`Ready` on its connection), and it stays subscribed and is never
/// told to disconnect. Catching up from what is queued, it converges with the
/// publisher and waits on no dependency.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_members_queued_live_state_stays_bounded_and_converges() {
    const WS: u64 = 91;
    const BOUND: u64 = 4;
    let (root, user) = UserIdentity::generate("bound fixture", 1);
    let (publisher, _) = certified_device(&root, user.public_key, "publisher");
    let (slow, _) = certified_device(&root, user.public_key, "slow");
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        temp.path(),
        WS,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.set_bounds(crate::store::QueueBounds {
        pending_entries: BOUND,
        pending_bytes: 64 * 1024 * 1024,
    });
    let (publisher_tx, _publisher_rx) = mpsc::channel(8);
    let (publisher_disconnect, _publisher_disconnected) = mpsc::channel(1);
    let (slow_tx, mut slow_rx) = mpsc::channel(64);
    let (slow_disconnect, mut slow_disconnected) = mpsc::channel(1);
    // The slow member's state queue holds one delivery and is never drained.
    let (slow_state_tx, _slow_state_rx) = mpsc::channel(1);
    for (peer, tx, disconnect) in [
        (&publisher, &publisher_tx, &publisher_disconnect),
        (&slow, &slow_tx, &slow_disconnect),
    ] {
        runtime.admit_device(peer.device_public_key).unwrap();
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: disconnect.clone(),
        });
    }
    runtime.state_sessions.insert(
        slow.device_public_key,
        StateSession {
            peer: slow.clone(),
            tx: slow_state_tx,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: None,
        },
    );
    let store = runtime.store.clone();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(WS, workspace.clone())])));
    let policy = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let (private, _private_rx) = mpsc::channel(8);
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &publisher_tx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &publisher.cert,
        own_device_public_key: &publisher.cert.device_public_key,
        app_serving: None,
        own_device_signing_key: crate::peer::test_signing_key(),
        direct_rooms: crate::peer::test_direct_rooms(),
        max_subscribers_per_workspace: usize::MAX,
        metrics: &metrics,
        trust_policy: &policy,
        disconnect: &publisher_disconnect,
        v2: false,
        agent_reads_v1: false,
        private_tx: &private,
        revocations: &revocations,
    };
    let source = StateSource::new(&publisher, &session);

    let mut doc = automerge::AutoCommit::new();
    doc.set_actor(automerge::ActorId::from(b"publisher".as_slice()));
    for n in 0..24u64 {
        doc.put(automerge::ROOT, format!("k{n}"), n).unwrap();
        enqueue_state(
            &workspace,
            Envelope::Delta {
                workspace_id: WS,
                author_device_public_key: publisher.device_public_key,
                authored_at_ns: n.saturating_add(1),
                delta: doc.save_incremental(),
                signature: [0; 64],
            },
            Some(source.clone()),
            publisher.role,
            metrics.clone(),
        )
        .await
        .unwrap();
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert!(
            u64::from(store.count_pending(&slow.device_public_key).unwrap()) <= BOUND,
            "the slow member's queued live state stays within its bound"
        );
    }

    let asked = std::iter::from_fn(|| slow_rx.try_recv().ok())
        .filter(|envelope| {
            matches!(
                envelope,
                Envelope::Ready {
                    workspace_id: WS,
                    pending_deltas: 0
                }
            )
        })
        .count();
    assert!(asked >= 1, "the slow member is asked for a checkpoint");
    assert!(
        slow_disconnected.try_recv().is_err(),
        "the slow member is never told to disconnect"
    );
    assert!(
        workspace
            .lock()
            .await
            .has_subscriber(&slow.device_public_key),
        "the slow member stays subscribed"
    );

    let mut caught_up = automerge::AutoCommit::new();
    for frame in store.drain_pending(&slow.device_public_key).unwrap() {
        let Envelope::Delta { delta, .. } = crate::transport::decode(&frame[4..]).unwrap() else {
            panic!("queued live state is deltas");
        };
        caught_up.load_incoming(&delta).unwrap();
    }
    assert!(
        caught_up.get_missing_deps(&[]).is_empty(),
        "no queued change waits for a dependency"
    );
    assert_eq!(
        caught_up.get_heads(),
        doc.get_heads(),
        "the slow member converges with the publisher"
    );
}
