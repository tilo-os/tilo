//! Real persisted activation queues checked at the final writer boundary.
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
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_relay_shell::ShellAuthority as _;
use tilo_relay_shell::selection::fixtures::{producer_frame, producer_metadata};
use tilo_user_identity::{RevocationList, UserIdentity};

async fn writer_boundary(
    written_before_invalidation: usize,
    replace: bool,
    selected: bool,
    invalidate_source: bool,
) {
    let meta = producer_metadata();
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_value(meta["authority"].clone()).unwrap();
    let source_peer = crate::auth::accept_hello(
        &crate::auth::Hello {
            cert: cert.clone(),
            client_nonce: [0; 32],
        },
        &cert.user_public_key,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        cert.clone(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        temp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        let bytes = producer_frame(&meta, row, 1);
        let Envelope::Delta { delta, .. } = crate::transport::decode(&bytes[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, &bytes).unwrap();
    }
    let (root, user) = UserIdentity::generate("writer fixture", 1);
    let (recipient, _) = certified_device(&root, user.public_key, "recipient");
    let (source_tx, _source_rx) = mpsc::channel(8);
    let (outbound, _outbound_rx) = mpsc::channel(8);
    let (source_disconnect, _source_disconnected) = mpsc::channel(1);
    let (disconnect, mut disconnected) = mpsc::channel(1);
    let (state_tx, mut state_rx) = mpsc::channel(8);
    let (private, _private_rx) = mpsc::channel(8);
    let live = Arc::new(AtomicBool::new(true));
    for (peer, tx, disconnect) in [
        (&source_peer, &source_tx, &source_disconnect),
        (&recipient, &outbound, &disconnect),
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
        recipient.device_public_key,
        StateSession {
            peer: recipient.clone(),
            tx: state_tx.clone(),
            live: live.clone(),
            replay: None,
            lag: None,
        },
    );
    let store = runtime.store.clone();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    let policy = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowList {
            users: vec![
                hex::encode(cert.user_public_key),
                hex::encode(user.public_key),
            ],
        },
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let source_live = Arc::new(AtomicBool::new(true));
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &outbound,
        state_outbound: Some(&state_tx),
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &source_peer.cert,
        own_device_public_key: &source_peer.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &policy,
        disconnect: &disconnect,
        framing: tilo_delivery_protocol::transport_v2::StateFraming::V1,
        presence: None,
        signed_actions: false,
        private_tx: &private,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let source_session = SessionCtx {
        outbound: &source_tx,
        disconnect: &source_disconnect,
        connection_live: Some(&source_live),
        ..session
    };
    let source = StateSource::new(&source_peer, &source_session);
    for row in &meta["tail"].as_array().unwrap()[..2] {
        let bytes = producer_frame(&meta, row, 2);
        enqueue_state(
            &workspace,
            crate::transport::decode(&bytes[4..]).unwrap(),
            Some(source.clone()),
            source_peer.role,
            metrics.clone(),
        )
        .await
        .unwrap();
    }
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    let mut deliveries = Vec::new();
    while let Ok(delivery) = state_rx.try_recv() {
        assert!(matches!(delivery.envelope, LiveEnvelope::Selected(_)));
        deliveries.push(delivery);
    }
    assert_eq!(
        deliveries.len(),
        2,
        "complete selected pair must be queued before invalidation"
    );
    let mut wire = Vec::new();
    for delivery in deliveries.drain(..written_before_invalidation) {
        write_state_delivery(
            delivery,
            &recipient,
            &session,
            &mut SessionCoding::default(),
            &crate::presence_mailbox::PresenceMailbox::default(),
            &mut wire,
        )
        .await
        .unwrap();
    }
    assert_eq!(wire.is_empty(), written_before_invalidation == 0);
    let mut delivery = deliveries.remove(0);
    if !selected {
        delivery.envelope = LiveEnvelope::Original(Arc::new((*delivery.envelope).clone()));
    }
    let current_envelope = (*delivery.envelope).clone();
    if replace {
        let mut subscribed = vec![id];
        Box::pin(handle_inbound_frame(
            Envelope::Unsubscribe { workspace_id: id },
            &recipient,
            &session,
            &mut subscribed,
            &mut wire,
        ))
        .await
        .unwrap();
        Box::pin(handle_inbound_frame(
            Envelope::Subscribe { workspace_id: id },
            &recipient,
            &session,
            &mut subscribed,
            &mut wire,
        ))
        .await
        .unwrap();
        assert!(!live.load(Ordering::SeqCst));
        assert!(!Arc::ptr_eq(
            &workspace.lock().await.state_sessions[&recipient.device_public_key].live,
            &live
        ));
    }
    // Both independent source fences are exercised: lifecycle before the pair,
    // current authorization between halves. Replacement tests also invalidate.
    if invalidate_source {
        if written_before_invalidation == 0 {
            source_live.store(false, Ordering::SeqCst);
        } else {
            *policy.write().unwrap() = crate::policy::TrustPolicy::AllowMatchingUser;
        }
    }
    let pending = store
        .enqueue_pending(&recipient.device_public_key, 50, b"fixture pending cleanup")
        .unwrap();
    delivery.cleanup = Some(pending.clone());
    let before = wire.clone();
    write_state_delivery(
        delivery,
        &recipient,
        &session,
        &mut SessionCoding::default(),
        &crate::presence_mailbox::PresenceMailbox::default(),
        &mut wire,
    )
    .await
    .unwrap();
    assert!(
        wire == before,
        "invalid or stale queued state must not reach the wire"
    );
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert!(
        pending.exists(),
        "dropped queued state cannot authorize cleanup"
    );
    if replace {
        assert!(
            disconnected.try_recv().is_err(),
            "old selected state cannot fence its replacement lease"
        );
        let current = workspace.lock().await.state_sessions[&recipient.device_public_key]
            .live
            .clone();
        assert!(current.load(Ordering::SeqCst));
        // A fresh delivery on the replacement lease still writes and cleans up.
        let current_delivery = StateDelivery {
            envelope: LiveEnvelope::Original(Arc::new(current_envelope)),
            cleanup: Some(pending.clone()),
            source: None,
            _budget: None,
            live: current,
            fanned: None,
        };
        write_state_delivery(
            current_delivery,
            &recipient,
            &session,
            &mut SessionCoding::default(),
            &crate::presence_mailbox::PresenceMailbox::default(),
            &mut wire,
        )
        .await
        .unwrap();
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert!(wire.len() > before.len());
        assert!(!pending.exists());
    } else {
        assert!(
            disconnected.try_recv().is_ok(),
            "current recipient must be fenced after selected source invalidation"
        );
        assert!(!live.load(Ordering::SeqCst));
        assert!(
            !workspace
                .lock()
                .await
                .state_sessions
                .contains_key(&recipient.device_public_key)
        );
    }
}

#[tokio::test]
async fn selected_writer_fences_source_invalidation_before_first_frame() {
    Box::pin(writer_boundary(0, false, true, true)).await;
}
#[tokio::test]
async fn selected_writer_fences_source_invalidation_between_frames() {
    Box::pin(writer_boundary(1, false, true, true)).await;
}
#[tokio::test]
async fn selected_writer_preserves_same_connection_replacement() {
    Box::pin(writer_boundary(0, true, true, true)).await;
}
#[tokio::test]
async fn ordinary_writer_preserves_same_connection_replacement() {
    Box::pin(writer_boundary(0, true, false, false)).await;
}

#[tokio::test]
async fn selected_writer_drops_old_lease_after_resubscription() {
    Box::pin(writer_boundary(0, true, true, false)).await;
}

#[tokio::test]
async fn replay_catchup_keeps_selected_activation_pair_without_repeating_baseline() {
    let meta = producer_metadata();
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_value(meta["authority"].clone()).unwrap();
    let source_peer = crate::auth::accept_hello(
        &crate::auth::Hello {
            cert: cert.clone(),
            client_nonce: [0; 32],
        },
        &cert.user_public_key,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        cert.clone(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        temp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        let bytes = producer_frame(&meta, row, 1);
        let Envelope::Delta { delta, .. } = crate::transport::decode(&bytes[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, &bytes).unwrap();
    }
    let initial = crate::store::test_support::replay_frames(&runtime.store);
    assert!(!initial.is_empty());

    let (root, user) = UserIdentity::generate("replay activation", 1);
    let (recipient, _) = certified_device(&root, user.public_key, "recipient");
    let (source_tx, _source_rx) = mpsc::channel(8);
    let (outbound, _outbound_rx) = mpsc::channel(8);
    let (source_disconnect, _source_disconnected) = mpsc::channel(1);
    let (disconnect, _disconnected) = mpsc::channel(1);
    let (state_tx, mut state_rx) = mpsc::channel(8);
    let (private, mut private_rx) = mpsc::channel(8);
    for (peer, tx, disconnect) in [
        (&source_peer, &source_tx, &source_disconnect),
        (&recipient, &outbound, &disconnect),
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
        recipient.device_public_key,
        StateSession {
            peer: recipient.clone(),
            tx: state_tx.clone(),
            live: Arc::new(AtomicBool::new(true)),
            replay: None,
            lag: None,
        },
    );
    let store = runtime.store.clone();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    let policy = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowList {
            users: vec![
                hex::encode(cert.user_public_key),
                hex::encode(user.public_key),
            ],
        },
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let source_live = Arc::new(AtomicBool::new(true));
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &outbound,
        state_outbound: Some(&state_tx),
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &source_peer.cert,
        own_device_public_key: &source_peer.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &policy,
        disconnect: &disconnect,
        framing: tilo_delivery_protocol::transport_v2::StateFraming::V2,
        presence: None,
        signed_actions: false,
        private_tx: &private,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let source_session = SessionCtx {
        outbound: &source_tx,
        disconnect: &source_disconnect,
        connection_live: Some(&source_live),
        ..session
    };
    let source = StateSource::new(&source_peer, &source_session);

    let mut replay = None;
    assert!(
        handle_subscribe(
            &recipient,
            &session,
            &mut Vec::new(),
            &mut replay,
            id,
            Subscription::Whole
        )
        .await
        .unwrap()
        .is_none()
    );
    for row in &meta["tail"].as_array().unwrap()[..2] {
        let bytes = producer_frame(&meta, row, 2);
        enqueue_state(
            &workspace,
            crate::transport::decode(&bytes[4..]).unwrap(),
            Some(source.clone()),
            source_peer.role,
            metrics.clone(),
        )
        .await
        .unwrap();
    }
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    let selected_after = crate::store::test_support::replay_frames(&store);
    let expected_activation = selected_after
        .iter()
        .filter(|frame| !initial.contains(frame))
        .collect::<Vec<_>>();
    assert_eq!(
        expected_activation.len(),
        2,
        "fixture did not produce the expected selected activation pair"
    );
    assert!(
        state_rx.try_recv().is_err(),
        "ACTIVE replay leaked selected activation onto the live lane"
    );

    let mut wire = Vec::new();
    Box::pin(finish_replay_write(
        ReplayWrite {
            ticket: replay,
            reply: None,
            replaying: true,
        },
        &recipient,
        &session,
        &std::sync::atomic::AtomicUsize::new(1),
        &mut private_rx,
        &mut wire,
    ))
    .await
    .unwrap();
    let mut reader =
        tilo_delivery_protocol::transport_v2::SessionReader::new(wire.as_slice(), true);
    let mut frames = Vec::new();
    let ready = loop {
        match reader.read_next().await.unwrap() {
            Some(tilo_delivery_protocol::transport_v2::EnvelopeV2::State(
                envelope @ Envelope::Delta { .. },
            )) => {
                frames.push(envelope);
            }
            Some(tilo_delivery_protocol::transport_v2::EnvelopeV2::State(Envelope::Ready {
                pending_deltas,
                ..
            })) => break pending_deltas,
            frame => panic!("unexpected activation replay frame: {frame:?}"),
        }
    };
    for baseline in &initial {
        assert_eq!(
            frames.iter().filter(|frame| *frame == baseline).count(),
            1,
            "pending catch-up repeated an unchanged selected baseline frame"
        );
    }
    let activation = frames
        .iter()
        .filter(|frame| !initial.contains(frame))
        .collect::<Vec<_>>();
    assert_eq!(
        activation, expected_activation,
        "selected activation pair changed identity or manifest order"
    );
    assert_eq!(ready as usize, frames.len());
    assert_eq!(
        store.count_pending(&recipient.device_public_key).unwrap(),
        0
    );

    let selected_root = store.root().join(crate::store::shell_selection::DIRECTORY);
    let current =
        selected_root.join(std::fs::read_to_string(selected_root.join("CURRENT")).unwrap());
    let generation = selected_root.join("pending-budget-fixture");
    std::fs::create_dir(&generation).unwrap();
    std::fs::copy(
        current.join("basis.manifest"),
        generation.join("live.manifest"),
    )
    .unwrap();
    let targets = (0..=crate::store::bounded::RUN_ENTRIES)
        .map(|_| ReplayPendingTarget {
            device: [9; 32],
            live: Arc::new(AtomicBool::new(false)),
            catchup: Arc::new(ReplayCatchup::new(std::path::Path::new("store-work-v1"))),
        })
        .collect();
    let mut work =
        ReplayPendingWork::selected(&SelectedPublication::Activation(generation), targets).unwrap();
    let budget = crate::store::replay::ReplayBudget::new(64 * 1024 * 1024);
    let work_budget = budget.clone();
    let (release, reached) =
        crate::store::test_barrier::install(store.root(), "replay-pending-frame-held");
    let receipt = owner
        .submit_resumable_wait(
            1,
            async { Ok(()) },
            move |state, _| work.step(state, &work_budget),
            |result| async { result },
        )
        .await
        .unwrap();
    let mut receipt = Box::pin(receipt);
    tilo_test_env::within(
        "selected pending frame did not reach the held-work barrier",
        async {
            tokio::select! {
                reached = reached => reached.unwrap(),
                result = &mut receipt => panic!(
                    "selected pending frame did not span a work quantum: {:?}",
                    result.unwrap()
                ),
            }
        },
    )
    .await;
    assert!(
        budget.used() > 0,
        "selected pending bytes escaped their output-budget lease across a work quantum"
    );
    drop(release);
    receipt.await.unwrap().unwrap();
    assert_eq!(
        budget.used(),
        0,
        "selected pending output-budget lease survived completed writes"
    );
}

/// A delivery its fan-out authorised under the room's lock is written
/// without taking that lock again while nothing the fan-out read has
/// changed, under the lock once anything announced a change, and not at all
/// once its lease ended.
#[tokio::test]
async fn a_fanned_delivery_skips_the_rooms_lock_until_authority_moves() {
    let temp = tempfile::tempdir().unwrap();
    let id = 7;
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        temp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    let (root, user) = UserIdentity::generate("fan-out fixture", 1);
    let (recipient, _) = certified_device(&root, user.public_key, "recipient");
    let (outbound, _outbound_rx) = mpsc::channel(8);
    let (disconnect, _disconnected) = mpsc::channel(1);
    let (state_tx, _state_rx) = mpsc::channel(8);
    let (private, _private_rx) = mpsc::channel(8);
    let live = Arc::new(AtomicBool::new(true));
    runtime.admit_device(recipient.device_public_key).unwrap();
    runtime.add_subscriber(Subscriber {
        device_public_key: recipient.device_public_key,
        role: recipient.role,
        tx: outbound.clone(),
        disconnect: disconnect.clone(),
    });
    runtime.state_sessions.insert(
        recipient.device_public_key,
        StateSession {
            peer: recipient.clone(),
            tx: state_tx.clone(),
            live: live.clone(),
            replay: None,
            lag: None,
        },
    );
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    // Only a room whose membership a live watch vouches for is stamped.
    let watch = crate::membership_watch::spawn_membership_watch(temp.path());
    tilo_test_env::within("the membership watch to vouch for the room", async {
        while !crate::workspace_io::fresh_membership(&workspace)
            .await
            .unwrap()
            .membership_is_current()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    let policy = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowList {
            users: vec![hex::encode(user.public_key)],
        },
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &outbound,
        state_outbound: Some(&state_tx),
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &recipient.cert,
        own_device_public_key: &recipient.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &policy,
        disconnect: &disconnect,
        framing: tilo_delivery_protocol::transport_v2::StateFraming::V1,
        presence: None,
        signed_actions: false,
        private_tx: &private,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let delivery = |fanned: Option<FanoutStamp>| StateDelivery {
        envelope: LiveEnvelope::Original(Arc::new(Envelope::Delta {
            workspace_id: id,
            author_device_public_key: [9; 32],
            authored_at_ns: 1,
            delta: vec![1, 2, 3],
            signature: [0; 64],
        })),
        cleanup: None,
        source: None,
        _budget: None,
        live: live.clone(),
        fanned,
    };
    let write = |delivery: StateDelivery| {
        let session = &session;
        let recipient = &recipient;
        async move {
            let mut wire = Vec::new();
            let result = write_state_delivery(
                delivery,
                recipient,
                session,
                &mut SessionCoding::default(),
                &crate::presence_mailbox::PresenceMailbox::default(),
                &mut wire,
            )
            .await;
            (result, wire)
        }
    };

    // The room stays locked, as a crowd's other turns keep it.
    let held = workspace.lock().await;
    // The epoch is process-wide: another test may move it between the stamp
    // and the write, which only sends this one the locked way. One of a few
    // fresh stamps reaches the wire while the room is held.
    let mut wire = Vec::new();
    for _ in 0..20 {
        let stamp = FanoutStamp::of(&held);
        assert!(stamp.is_some(), "a watched room's fan-out is stamped");
        if let Ok((result, written)) = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            write(delivery(stamp)),
        )
        .await
        {
            result.unwrap();
            wire = written;
            break;
        }
    }
    assert!(
        !wire.is_empty(),
        "an unchanged fan-out must be written without the room's lock"
    );

    // Anything announced since the stamp sends the writer under the lock.
    let stale = FanoutStamp::of(&held);
    crate::authority_epoch::bump();
    let mut waiting = Box::pin(write(delivery(stale)));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), &mut waiting)
            .await
            .is_err(),
        "a stamp older than an announced change must wait for the room's lock"
    );
    drop(held);
    let (result, written) = waiting.await;
    result.unwrap();
    assert!(
        !written.is_empty(),
        "the recipient is still a member, so the locked check writes it"
    );

    // An ended lease is never written, stamped or not.
    live.store(false, Ordering::SeqCst);
    let stamp = FanoutStamp::of(&*workspace.lock().await);
    let (result, written) = write(delivery(stamp)).await;
    result.unwrap();
    assert!(written.is_empty(), "an ended lease must not reach the wire");
    watch.abort();
}
