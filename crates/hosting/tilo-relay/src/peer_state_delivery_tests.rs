//! Real persisted activation queues checked at the final writer boundary.
use super::private_transport_tests::certified_device;
use super::*;
use crate::store::shell_selection_tests::{producer_frame, producer_metadata};
use std::sync::atomic::{AtomicBool, Ordering};
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
        now_ns(),
    )
    .unwrap();
    let authority = qos_peer_state::retained_proof::RetentionAuthority::new(
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
        metrics: &metrics,
        trust_policy: &policy,
        disconnect: &disconnect,
        v2: false,
        private_tx: &private,
        revocations: &revocations,
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
        write_state_delivery(delivery, &recipient, &session, &mut wire)
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
        handle_inbound_frame(
            Envelope::Unsubscribe { workspace_id: id },
            &recipient,
            &session,
            &mut subscribed,
            &mut wire,
        )
        .await
        .unwrap();
        handle_inbound_frame(
            Envelope::Subscribe { workspace_id: id },
            &recipient,
            &session,
            &mut subscribed,
            &mut wire,
        )
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
    write_state_delivery(delivery, &recipient, &session, &mut wire)
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
        };
        write_state_delivery(current_delivery, &recipient, &session, &mut wire)
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
    writer_boundary(0, false, true, true).await;
}
#[tokio::test]
async fn selected_writer_fences_source_invalidation_between_frames() {
    writer_boundary(1, false, true, true).await;
}
#[tokio::test]
async fn selected_writer_preserves_same_connection_replacement() {
    writer_boundary(0, true, true, true).await;
}
#[tokio::test]
async fn ordinary_writer_preserves_same_connection_replacement() {
    writer_boundary(0, true, false, false).await;
}

#[tokio::test]
async fn selected_writer_drops_old_lease_after_resubscription() {
    writer_boundary(0, true, true, false).await;
}
