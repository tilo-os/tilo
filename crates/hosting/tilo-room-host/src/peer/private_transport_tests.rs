use super::*;

/// Remote actions: who may still act, and what acting costs.
mod remote_action_tests;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

pub(super) fn certified_device(
    root: &ed25519_dalek::SigningKey,
    user: [u8; 32],
    label: &str,
) -> (AuthenticatedPeer, ed25519_dalek::SigningKey) {
    let (key, device) = DeviceIdentity::generate(label, 1);
    let mut cert = DeviceCert {
        user_public_key: user,
        device_public_key: device.public_key,
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    // Exercise certificate signature and nonce proof, matching the relay
    // handshake rather than constructing unchecked AuthenticatedPeer data.
    let hello = crate::auth::Hello::new(cert, [2; 32]);
    let peer = crate::auth::accept_hello(
        &hello,
        &user,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    let proof = crate::auth::build_hello_ack2(&key, &[1; 32]);
    crate::auth::verify_hello_ack2(&proof, &peer.device_public_key, &[1; 32]).unwrap();
    (peer, key)
}

/// The trust doors one test's connection answers at, kept alive beside the
/// context that borrows them.
struct Doors {
    trust: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    revocations: Arc<std::sync::RwLock<RevocationList>>,
    metrics: Arc<crate::metrics::Metrics>,
}

impl Doors {
    /// Doors that admit this relay's own person's devices and revoke nobody.
    fn open() -> Self {
        Self {
            trust: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            metrics: Arc::new(crate::metrics::Metrics::default()),
        }
    }

    /// What `who`'s connection is handled in, with every door a test does not
    /// vary left closed.
    fn ctx<'a>(
        &'a self,
        workspaces: &'a WorkspaceRegistry,
        outbound: &'a mpsc::Sender<Envelope>,
        disconnect: &'a mpsc::Sender<()>,
        who: (&'a [u8; 32], &'a DeviceCert),
    ) -> SessionCtx<'a> {
        SessionCtx {
            workspaces,
            autobind: None,
            outbound,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: who.0,
            own_device_cert: who.1,
            own_device_public_key: &who.1.device_public_key,
            app_serving: None,
            metrics: &self.metrics,
            trust_policy: &self.trust,
            disconnect,
            presence: None,
            revocations: &self.revocations,
            carried_calls: None,
            admin: None,
            here: None,
            fetch: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            member_limits: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        }
    }
}

#[tokio::test]
async fn shared_retained_metrics_count_only_newly_written_payload_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        7,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let envelope = Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: [1; 32],
        authored_at_ns: 1,
        signature: [0; 64],
        delta: crate::coedit::CoeditPayload::Checkpoint {
            channel: [2; 32],
            body: vec![3; 64],
        }
        .encode()
        .unwrap(),
    };
    let expected = crate::transport::encode(&envelope).unwrap().len() as u64;
    for _ in 0..2 {
        enqueue_state(&workspace, envelope.clone(), None, metrics.clone())
            .await
            .unwrap();
    }
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert_eq!(
        metrics.snapshot().bytes_queued,
        expected,
        "checkpoint retry was counted as another payload write"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_replay_cleanup_with_full_reply_queue_keeps_reader_and_writer_live() {
    paused_state_roundtrip(None, false).await;
}

#[tokio::test]
async fn full_control_backlog_backpressures_and_recovers_after_held_writer() {
    paused_state_roundtrip(None, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_subscription_recovers_retained_frame_after_live_budget_exhaustion() {
    paused_state_roundtrip(Some(LagBoundary::Live), false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_subscription_discards_stale_lag_after_explicit_unsubscribe() {
    paused_state_roundtrip(Some(LagBoundary::Unsubscribed), false).await;
}

/// A read-only state request whose reply queues on the state lane like any
/// other: a stand-in for replies waiting behind a held writer.
fn reply_filler(workspace_id: u64) -> Vec<u8> {
    tilo_delivery_protocol::transport_v2::encode_state(&Envelope::VoiceCalls { workspace_id })
        .unwrap()
}

enum LagBoundary {
    Live,
    Unsubscribed,
}

async fn paused_state_roundtrip(plain_lag: Option<LagBoundary>, full_backlog: bool) {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    // The relay's own device is neither of the peers it carries frames for.
    let (own, _) = certified_device(&root, user.public_key, "relay");
    let id = 7_u64;
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    for device in [
        a.device_public_key,
        b.device_public_key,
        own.device_public_key,
    ] {
        runtime.admit_device(device).unwrap();
    }
    let store = runtime.store.clone();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let mut wires = Vec::new();
    let mut tasks = Vec::new();
    for peer in [&a, &b] {
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(server);
        let ctx = PeerContext {
            workspaces: registry.clone(),
            autobind: None,
            revocations: revocations.clone(),
            trust_policy: trust.clone(),
            own_user_public_key: user.public_key,
            own_device_cert: own.cert.clone(),
            own_device_public_key: own.cert.device_public_key,
            app_serving: None,
            metrics: Arc::new(crate::metrics::Metrics::default()),
            carried_calls: None,
            admin: None,
            here: None,
            fetch: None,
            agents: crate::peer::AgentRegistry::default(),
            positions: None,
            compactor: None,
            member_limits: None,
        };
        tasks.push(tokio::spawn(run_peer_stream(
            peer.clone(),
            read,
            write,
            ctx,
            std::future::pending::<()>(),
        )));
        let (read, mut write) = tokio::io::split(client);
        let mut reader = SessionReader::new(read);
        write
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                    workspace_id: id,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ready { .. }))
        ));
        wires.push((reader, write));
    }
    let (mut ar, mut aw) = wires.remove(0);
    let (mut br, mut bw) = wires.remove(0);

    if let Some(boundary) = plain_lag {
        let lag = workspace.lock().await.state_sessions[&b.device_public_key]
            .lag
            .as_ref()
            .unwrap()
            .clone();
        let envelope = Envelope::Delta {
            workspace_id: id,
            author_device_public_key: a.device_public_key,
            authored_at_ns: 1,
            signature: [0; 64],
            delta: crate::coedit::CoeditPayload::RetainedDelta {
                channel: [3; 32],
                body: vec![4; 4096],
            }
            .encode()
            .unwrap(),
        };
        let fault = crate::store::test_barrier::fail(store.root(), "live-delivery-budget");
        let (publication_release, published) =
            crate::store::test_barrier::install(store.root(), "live-publication-complete");
        let (release, reached) =
            crate::store::test_barrier::install(store.root(), "live-delivery-budget");
        aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&envelope).unwrap())
            .await
            .unwrap();
        tilo_test_env::within("the retained publication to reach its live budget", reached)
            .await
            .expect("ordinary retained publication never reached live budget");
        drop(release);
        tilo_test_env::within("the missed live publication recipient fan-out", published)
            .await
            .expect("missed live publication never completed its recipient fan-out");
        drop(publication_release);
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        drop(fault);
        assert!(
            store
                .load_retained()
                .unwrap()
                .iter()
                .any(|bytes| { crate::transport::decode(&bytes[4..]).unwrap() == envelope }),
            "missed live frame must remain durably retained"
        );
        let unsubscribed = matches!(boundary, LagBoundary::Unsubscribed);
        if unsubscribed {
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Unsubscribe {
                    workspace_id: id,
                })
                .unwrap(),
            )
            .await
            .unwrap();
            // A heartbeat is answered ahead of a control still running, so
            // the unsubscribe's own effect is what this waits for.
            tilo_test_env::within("the explicit unsubscribe to complete", async {
                while workspace
                    .lock()
                    .await
                    .state_sessions
                    .contains_key(&b.device_public_key)
                {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await;
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap(),
            )
            .await
            .unwrap();
            let closed =
                tilo_test_env::within("the session to answer after its unsubscribe", async {
                    loop {
                        match br.read_next().await.unwrap() {
                            Some(EnvelopeV2::State(Envelope::Pong)) => break false,
                            Some(EnvelopeV2::State(
                                Envelope::Delta { .. } | Envelope::Ready { .. },
                            )) => {}
                            None => break true,
                            other => panic!("unexpected explicit-unsubscribe reply: {other:?}"),
                        }
                    }
                })
                .await;
            if closed {
                // The relay ended the session: say how, not just that it did.
                let ended = tilo_test_env::within("the closed session's outcome", tasks.remove(1))
                    .await
                    .expect("the closed session's outcome");
                panic!("the relay closed the session on explicit unsubscribe: {ended:?}");
            }
            assert!(
                !workspace
                    .lock()
                    .await
                    .state_sessions
                    .contains_key(&b.device_public_key)
            );
            // A queued hint may outlive the subscription that produced it.
            // Its ordering is fixed here after the real unsubscribe reply.
            lag.mark(id);
        }
        // Publication has completed, so the lag marker was made. Wait for
        // its actual consumption rather than guessing the polling interval.
        tilo_test_env::within("the peer to consume its lag marker", async {
            while !lag.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        bw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap())
            .await
            .unwrap();
        let mut frames = Vec::new();
        let mut ready = 0_usize;
        // The heartbeat answers at once, possibly ahead of a catch-up still
        // replaying: a live subscription waits for its replay's Ready too.
        // An unsubscribed one starts no replay, so its Pong follows every
        // reply the lag hint could have produced.
        let mut answered = false;
        tilo_test_env::within("retained catch-up before the next peer reply", async {
            while !(answered && (unsubscribed || ready > 0)) {
                match br.read_next().await.unwrap() {
                    Some(EnvelopeV2::State(Envelope::Pong)) => answered = true,
                    Some(EnvelopeV2::State(Envelope::Ready { .. })) => {
                        ready = ready.checked_add(1).unwrap();
                    }
                    Some(EnvelopeV2::State(frame @ Envelope::Delta { .. })) => {
                        frames.push(frame);
                    }
                    other => panic!("unexpected lag-recovery frame: {other:?}"),
                }
            }
        })
        .await;
        if !unsubscribed {
            assert!(
                workspace.lock().await.state_sessions[&b.device_public_key]
                    .live
                    .load(std::sync::atomic::Ordering::SeqCst),
                "ordinary byte backpressure must preserve the authorized subscription"
            );
        }
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        if unsubscribed {
            assert!(
                frames.is_empty(),
                "a stale lag hint reopened an explicitly closed subscription"
            );
            assert_eq!(
                ready, 0,
                "an explicitly closed subscription cannot become ready again"
            );
            assert!(
                !workspace
                    .lock()
                    .await
                    .state_sessions
                    .contains_key(&b.device_public_key)
            );
        } else {
            assert_eq!(
                frames,
                vec![envelope],
                "plain subscription lost its committed retained frame"
            );
            assert_eq!(
                ready, 1,
                "catch-up must complete its actual replay boundary"
            );
        }
        return;
    }

    // A catch-up replay held inside its store cleanup, as on a slow disk.
    let (release, reached) = crate::store::test_barrier::install(store.root(), "replay-cleanup");
    let pending = Envelope::Delta {
        workspace_id: id,
        author_device_public_key: b.device_public_key,
        authored_at_ns: 3,
        delta: vec![255; 64],
        signature: [0; 64],
    };
    store
        .enqueue_pending(
            &a.device_public_key,
            3,
            &crate::transport::encode(&pending).unwrap(),
        )
        .unwrap();
    aw.write_all(
        &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
            workspace_id: id,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        ar.read_next().await.unwrap(),
        Some(EnvelopeV2::State(pending))
    );
    reached.await.unwrap();
    // Replies that queue behind the held replay until the reply queue, then
    // the reader's backlog, is full.
    for _ in 0..if full_backlog { 64 } else { 12 } {
        aw.write_all(&reply_filler(id)).await.unwrap();
    }
    if full_backlog {
        // The writer is confirmed inside the held cleanup. No state reply
        // can drain yet; enough real input fills reply and control queues.
        let held =
            tokio::time::timeout(std::time::Duration::from_millis(100), ar.read_next()).await;
        drop(release);
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        if held.is_ok() {
            let ended = tasks.remove(0).await.unwrap();
            panic!(
                "full control backlog disconnected a healthy held writer: {held:?}; server result: {ended:?}"
            );
        }
        assert!(matches!(
            ar.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ready { .. }))
        ));
        for _ in 0..64 {
            assert!(matches!(
                ar.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::VoiceCallList { .. }))
            ));
        }
        for task in tasks {
            task.abort();
        }
        return;
    }
    // Behind a full reply queue and a writer held in the replay's store
    // cleanup, as on a slow disk, a heartbeat is still answered.
    aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap())
        .await
        .unwrap();
    assert_eq!(
        tilo_test_env::within("a heartbeat answered under the held replay", ar.read_next())
            .await
            .unwrap(),
        Some(EnvelopeV2::State(Envelope::Pong))
    );
    drop(release);
    for task in tasks {
        task.abort();
    }
}
#[tokio::test]
async fn retained_replay_failure_and_duplicate_retirement_are_recoverable() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (peer, _) = certified_device(&root, user.public_key, "reader");
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        7,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.admit_device(peer.device_public_key).unwrap();
    let envelope = |author, payload: crate::coedit::CoeditPayload| Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: [author; 32],
        authored_at_ns: 1,
        delta: payload.encode().unwrap(),
        signature: [0; 64],
    };
    let cp = envelope(
        1,
        crate::coedit::CoeditPayload::Checkpoint {
            channel: [9; 32],
            body: vec![1],
        },
    );
    let tail = envelope(
        1,
        crate::coedit::CoeditPayload::RetainedDelta {
            channel: [9; 32],
            body: vec![2],
        },
    );
    let other = envelope(
        2,
        crate::coedit::CoeditPayload::Checkpoint {
            channel: [9; 32],
            body: vec![3],
        },
    );
    let cp_bytes = crate::transport::encode(&cp).unwrap();
    runtime
        .store
        .retain_checkpoint(&[9; 32], &cp_bytes)
        .unwrap();
    runtime
        .store
        .retain_delta(&[9; 32], 2, &crate::transport::encode(&tail).unwrap())
        .unwrap();
    runtime
        .store
        .enqueue_pending(&peer.device_public_key, 1, &cp_bytes)
        .unwrap();
    runtime
        .store
        .enqueue_pending(
            &peer.device_public_key,
            2,
            &crate::transport::encode(&other).unwrap(),
        )
        .unwrap();
    runtime
        .store
        .enqueue_pending(&peer.device_public_key, 4, &cp_bytes)
        .unwrap();
    let opaque = b"unknown pending bytes";
    runtime
        .store
        .enqueue_pending(&peer.device_public_key, 3, opaque)
        .unwrap();
    // Matching undecodable retained bytes do NOT constitute replay coverage.
    let lineage = runtime.store.root().join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([1; 32]),
        hex::encode([9; 32])
    ));
    let active = std::fs::read_to_string(lineage.join("CURRENT")).unwrap();
    std::fs::write(lineage.join(active.trim()).join("opaque.delta"), opaque).unwrap();
    let shared = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(7, shared.clone())])));
    let (outbound, _rx) = mpsc::channel(8);
    let (disconnect, _) = mpsc::channel(1);
    let doors = Doors::open();
    let session = doors.ctx(
        &registry,
        &outbound,
        &disconnect,
        (&user.public_key, &peer.cert),
    );
    let (mut broken, reader) = tokio::io::duplex(1);
    drop(reader);
    assert!(
        Box::pin(handle_inbound_frame(
            Envelope::Subscribe { workspace_id: 7 },
            &peer,
            &session,
            &mut Vec::new(),
            &mut broken
        ))
        .await
        .is_err()
    );
    assert_eq!(
        shared
            .lock()
            .await
            .store
            .count_pending(&peer.device_public_key)
            .unwrap(),
        4
    );
    // Finish the old owner's cancelled-ticket disposal before emulating a
    // new process. Two owners over the same store are not a daemon restart.
    tilo_test_env::within("old owner did not release cancelled replay", async {
        loop {
            if std::fs::read_dir(shared.lock().await.store.root().join("store-work-v1"))
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
    // Emulate a crash during resumable duplicate cleanup: write the complete
    // replay, remove only its first duplicate, then reopen before the rest
    // of the cleanup list can be processed.
    let store = shared.lock().await.store.clone();
    let mut capture = store
        .start_replay_capture(
            crate::store::replay::ReplayId(*uuid::Uuid::new_v4().as_bytes()),
            peer.device_public_key,
        )
        .unwrap();
    let mut snapshot = loop {
        if let Step::Done(snapshot) = store
            .capture_replay_step(&mut capture, WorkLimit::default())
            .unwrap()
        {
            break snapshot;
        }
    };
    while matches!(
        store
            .prepare_replay_step(&mut snapshot, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    let budget = crate::store::replay::ReplayBudget::new(1024 * 1024);
    loop {
        let frames = store
            .read_replay_batch(
                &mut snapshot,
                crate::store::replay::ReplayLimits::default(),
                &budget,
            )
            .unwrap();
        if frames.eof {
            break;
        }
    }
    store
        .cleanup_replay_step(
            &mut snapshot,
            crate::store::replay::WorkLimit {
                entries: 1,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        shared
            .lock()
            .await
            .store
            .count_pending(&peer.device_public_key)
            .unwrap(),
        3
    );
    drop(snapshot);
    drop(capture);
    // Reopen the actual disk state after the failed replay.
    *shared.lock().await = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        7,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    let mut wire = Vec::new();
    Box::pin(handle_inbound_frame(
        Envelope::Subscribe { workspace_id: 7 },
        &peer,
        &session,
        &mut Vec::new(),
        &mut wire,
    ))
    .await
    .unwrap();
    let mut decoded = Vec::new();
    let mut reader = SessionReader::new(wire.as_slice());
    while let Some(frame) = reader.read_next().await.unwrap() {
        let EnvelopeV2::State(envelope) = frame else {
            panic!("a replay carried {frame:?}");
        };
        decoded.push(envelope);
    }
    assert_eq!(
        decoded,
        vec![
            cp,
            tail,
            other,
            Envelope::Ready {
                workspace_id: 7,
                pending_deltas: 3
            }
        ]
    );
    let pending = shared
        .lock()
        .await
        .store
        .load_pending(&peer.device_public_key)
        .unwrap();
    assert_eq!(
        pending.len(),
        1,
        "unknown pending entry was incorrectly retired"
    );
    assert_eq!(pending[0].bytes, opaque);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_membership_read_releases_routing_lock_and_fences_replaced_session() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let id = 7_u64;
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    wr.admit_device(b.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (btx, mut brx) = mpsc::channel(1);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, session_disconnect) in
        [(&a, &atx, &disconnect), (&b, &btx, &recipient_disconnect)]
    {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
    }
    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let doors = Doors::open();
    let session = doors.ctx(&registry, &atx, &disconnect, (&user.public_key, &a.cert));
    let frame = tilo_actions::remote::RemoteActionFrame::new(
        tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: tilo_actions::remote::CatalogueQuery::default(),
        },
    )
    .encode()
    .unwrap();
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: b.device_public_key,
    };

    let root_path = wr.lock().await.store.root().to_path_buf();
    let (release, reached) = crate::store::test_barrier::install(&root_path, "membership-read");
    let observed = wr.clone();
    let device = a.device_public_key;
    let replacement_disconnect = disconnect.clone();
    let handle = tokio::runtime::Handle::current();
    let observer = std::thread::spawn(move || {
        handle.block_on(async move {
            reached.await.unwrap();
            let lock = tokio::time::timeout(tilo_test_env::hang_bound(), observed.lock()).await;
            let unlocked = if let Ok(mut runtime) = lock {
                let (replacement, _rx) = mpsc::channel(2);
                runtime.add_subscriber(Subscriber {
                    device_public_key: device,
                    tx: replacement,
                    disconnect: replacement_disconnect,
                });
                true
            } else {
                false
            };
            drop(release);
            unlocked
        })
    });
    let refused = handle_remote_action(&a, &session, &[id], route(), frame).await;
    assert!(
        observer.join().unwrap(),
        "fresh membership read held routing lock"
    );
    let Some(Envelope::RemoteActionUndeliverable { reason, .. }) = refused else {
        panic!("a replaced session's action is undeliverable, got {refused:?}");
    };
    assert_eq!(
        UnavailableReason::from_wire_reason(&reason),
        Some(UnavailableReason::NotAuthorised),
        "{reason}"
    );
    assert!(brx.try_recv().is_err(), "nothing reached the target");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorization_rechecks_certificates_after_blocking_membership_read() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let id = 7_u64;
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    wr.admit_device(b.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (btx, mut brx) = mpsc::channel(1);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, session_disconnect) in
        [(&a, &atx, &disconnect), (&b, &btx, &recipient_disconnect)]
    {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.state_sessions
            .insert(peer.device_public_key, StateSession::for_test(peer.clone()));
    }
    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let doors = Doors::open();
    let session = doors.ctx(&registry, &atx, &disconnect, (&user.public_key, &a.cert));
    let frame = || {
        tilo_actions::remote::RemoteActionFrame::new(
            tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query: tilo_actions::remote::CatalogueQuery::default(),
            },
        )
        .encode()
        .unwrap()
    };
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: b.device_public_key,
    };

    assert_eq!(
        handle_remote_action(&a, &session, &[id], route(), frame()).await,
        None
    );
    assert!(brx.try_recv().is_ok(), "the target received the action");
    let revocation = |device: [u8; 32]| {
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: device,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        entry
    };
    let root_path = wr.lock().await.store.root().to_path_buf();
    // Each end is rechecked after its membership read: the sender's
    // certificate, then the target's.
    for device in [a.device_public_key, b.device_public_key] {
        *doors.revocations.write().unwrap() = RevocationList::new();
        let (release, reached) = crate::store::test_barrier::install(&root_path, "membership-read");
        let list = doors.revocations.clone();
        let entry = revocation(device);
        let handle = tokio::runtime::Handle::current();
        let revoke = std::thread::spawn(move || {
            handle.block_on(async move {
                reached.await.unwrap();
                list.write().unwrap().insert(entry).unwrap();
                drop(release);
            });
        });
        let refused = handle_remote_action(&a, &session, &[id], route(), frame()).await;
        revoke.join().unwrap();
        let Some(Envelope::RemoteActionUndeliverable { reason, .. }) = refused else {
            panic!("an action with a device revoked mid-read is undeliverable, got {refused:?}");
        };
        assert_eq!(
            UnavailableReason::from_wire_reason(&reason),
            Some(UnavailableReason::NotAuthorised),
            "{reason}"
        );
        assert!(
            brx.try_recv().is_err(),
            "an action authorized its certificate revoked during its membership read"
        );
    }
}

/// Counts what reaches the app-serving plane; needs no reply.
#[derive(Default)]
struct CountingPlane(std::sync::atomic::AtomicUsize);

impl crate::app_serving::AppServingPlane for CountingPlane {
    fn serve(
        &self,
        _workspace_id: u64,
        _caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // A request is always answered: this plane counts it and refuses it.
        tilo_actions::remote::refuse(
            &message,
            tilo_actions::remote::ActionHostError::Refused {
                reason: "counted".into(),
            },
        )
        .map(tilo_actions::remote::Pending::ready)
    }
}
#[tokio::test]
async fn a_device_presence_frame_rearms_its_live_checkpoint() {
    // Wiring for the per-device live checkpoint gate: after a device has been served one live
    // checkpoint the relay stops fanning checkpoints to it; a presence frame
    // from that device (the browser's hello / SyncReq / rejected-checkpoint
    // path) must re-arm exactly that device so the native's fresh
    // checkpoint reaches it.
    let (root, user) = UserIdentity::generate("alice", 1);
    let (native, _) = certified_device(&root, user.public_key, "native");
    let (browser, _) = certified_device(&root, user.public_key, "browser");
    let workspace = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&workspace.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(native.device_public_key).unwrap();
    wr.admit_device(browser.device_public_key).unwrap();
    let (btx, mut brx) = mpsc::channel(4);
    let (bdis, _bdis_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: browser.device_public_key,
        tx: btx.clone(),
        disconnect: bdis.clone(),
    });
    // Serve the subscribe-time need with one checkpoint from the native.
    let raw = crate::coedit::CoeditPayload::Checkpoint {
        channel: [1; 32],
        body: vec![1],
    }
    .encode()
    .unwrap();
    let checkpoint_at = |authored_at_ns: u64| Envelope::Delta {
        workspace_id: id,
        author_device_public_key: native.device_public_key,
        authored_at_ns,
        delta: raw.clone(),
        signature: [0; 64],
    };
    let env = checkpoint_at(1);
    wr.fanout_delta(&native.device_public_key, &env, &raw, 1)
        .unwrap();
    brx.try_recv().unwrap();
    assert!(!wr.needs_checkpoint(&browser.device_public_key, &[1; 32]));

    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let doors = Doors::open();
    let session = doors.ctx(&registry, &btx, &bdis, (&user.public_key, &browser.cert));
    // The browser sends a SyncReq as presence.
    handle_presence(
        &browser,
        &session,
        &[id],
        Envelope::RetiredPresence {
            workspace_id: id,
            payload: vec![9, 9],
        },
    )
    .await
    .unwrap();
    assert!(
        wr.lock()
            .await
            .needs_checkpoint(&browser.device_public_key, &[1; 32]),
        "a device presence frame must re-arm its live checkpoint"
    );
    // The native's fresh checkpoint now reaches it, once.
    let env2 = checkpoint_at(2);
    wr.lock()
        .await
        .fanout_delta(&native.device_public_key, &env2, &raw, 2)
        .unwrap();
    assert!(brx.try_recv().is_ok());
    let env3 = checkpoint_at(3);
    wr.lock()
        .await
        .fanout_delta(&native.device_public_key, &env3, &raw, 3)
        .unwrap();
    assert!(brx.try_recv().is_err(), "satisfied by one checkpoint");

    let (native_tx, mut native_rx) = mpsc::channel(4);
    let (native_disconnect, _native_disconnect_rx) = mpsc::channel(1);
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: native.device_public_key,
        tx: native_tx,
        disconnect: native_disconnect,
    });
    // The browser claims another admitted device's author identity.
    let forged = crate::AttributedPresence {
        author: native.device_public_key,
        body: b"fixture presence".to_vec(),
    };
    handle_presence(
        &browser,
        &session,
        &[id],
        Envelope::RetiredPresence {
            workspace_id: id,
            payload: forged.encode().unwrap(),
        },
    )
    .await
    .unwrap();
    let Envelope::RetiredPresence { payload, .. } = native_rx.try_recv().unwrap() else {
        panic!("expected forwarded presence");
    };
    let observed = crate::AttributedPresence::decode(&payload).unwrap();
    assert_eq!(observed.author, browser.device_public_key);
    assert_eq!(observed.body, forged.body);
}

/// A heartbeat is answered while the session's writer is held behind a live
/// frame whose authorization waits for the workspace's lock: a slow store under
/// that lock never makes the host look silent.
#[tokio::test]
async fn heartbeat_is_answered_while_the_writer_waits_on_the_room() {
    let (root, user) = UserIdentity::generate("owner", 1);
    let (b, _) = certified_device(&root, user.public_key, "b");
    let workspace = uuid::Uuid::from_u128(91);
    let id = crate::room_workspace_id(&workspace.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.admit_device(b.device_public_key).unwrap();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    let (client, server) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(server);
    let session = tokio::spawn(run_peer_stream(
        b.clone(),
        read,
        write,
        PeerContext {
            workspaces: registry,
            autobind: None,
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            trust_policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user_public_key: user.public_key,
            own_device_cert: b.cert.clone(),
            own_device_public_key: b.cert.device_public_key,
            app_serving: None,
            metrics: Arc::new(crate::metrics::Metrics::default()),
            carried_calls: None,
            admin: None,
            here: None,
            fetch: None,
            agents: crate::peer::AgentRegistry::default(),
            positions: None,
            compactor: None,
            member_limits: None,
        },
        std::future::pending::<()>(),
    ));
    let (read, mut write) = tokio::io::split(client);
    let mut reader = SessionReader::new(read);
    write
        .write_all(
            &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                workspace_id: id,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        reader.read_next().await.unwrap(),
        Some(EnvelopeV2::State(Envelope::Ready { .. }))
    ));
    let outbound = workspace.lock().await.subscribers[&b.device_public_key]
        .tx
        .clone();
    // Hold the workspace: the writer's authorization of the next live frame waits
    // for it, as it waits for a slow store under it.
    let held = workspace.lock().await;
    outbound
        .send(Envelope::RetiredPresence {
            workspace_id: id,
            payload: vec![1],
        })
        .await
        .unwrap();
    tilo_test_env::within("the writer to take the live frame", async {
        while outbound.capacity() < outbound.max_capacity() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    write
        .write_all(&tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap())
        .await
        .unwrap();
    let answer = tilo_test_env::within("the heartbeat's answer while the writer waits", async {
        reader.read_next().await.unwrap()
    })
    .await;
    assert!(
        matches!(answer, Some(EnvelopeV2::State(Envelope::Pong))),
        "the heartbeat must be answered ahead of the held live frame: {answer:?}"
    );
    drop(held);
    session.abort();
    let _ = session.await;
}
/// A heartbeat is answered while another workspace's state admission is held in
/// its membership read: a slow disk on one workspace never makes the host look
/// silent on the connection.
#[tokio::test]
async fn heartbeat_keeps_progress_during_unrelated_workspace_admission() {
    let (root, user) = UserIdentity::generate("owner", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let (own, _) = certified_device(&root, user.public_key, "relay");
    let id = 73_u64;
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    for device in [
        a.device_public_key,
        b.device_public_key,
        own.device_public_key,
    ] {
        runtime.admit_device(device).unwrap();
    }
    let store = runtime.store.clone();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let mut tasks = Vec::new();
    let mut wires = Vec::new();
    for peer in [&a, &b] {
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(server);
        tasks.push(tokio::spawn(run_peer_stream(
            peer.clone(),
            read,
            write,
            PeerContext {
                workspaces: registry.clone(),
                autobind: None,
                revocations: revocations.clone(),
                trust_policy: trust.clone(),
                own_user_public_key: user.public_key,
                own_device_cert: own.cert.clone(),
                own_device_public_key: own.cert.device_public_key,
                app_serving: None,
                metrics: Arc::new(crate::metrics::Metrics::default()),
                carried_calls: None,
                admin: None,
                here: None,
                fetch: None,
                agents: crate::peer::AgentRegistry::default(),
                positions: None,
                compactor: None,
                member_limits: None,
            },
            std::future::pending::<()>(),
        )));
        let (read, mut write) = tokio::io::split(client);
        let mut reader = SessionReader::new(read);
        write
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                    workspace_id: id,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ready { .. }))
        ));
        wires.push((reader, write));
    }
    let (mut ar, mut aw) = wires.remove(0);
    let (mut br, _bw) = wires.remove(0);
    let (release, reached) = crate::store::test_barrier::install(store.root(), "membership-read");
    let envelope = Envelope::Delta {
        workspace_id: id,
        author_device_public_key: a.device_public_key,
        authored_at_ns: 1,
        signature: [0; 64],
        delta: crate::coedit::CoeditPayload::RetainedDelta {
            channel: [3; 32],
            body: vec![4; 4096],
        }
        .encode()
        .unwrap(),
    };
    aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&envelope).unwrap())
        .await
        .unwrap();
    tilo_test_env::within("the state admission membership read", reached)
        .await
        .unwrap();
    aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap())
        .await
        .unwrap();
    let answer = tilo_test_env::within("the heartbeat's answer", ar.read_next())
        .await
        .unwrap();
    assert_eq!(
        answer,
        Some(EnvelopeV2::State(Envelope::Pong)),
        "a live transport withheld its heartbeat behind a held state admission"
    );
    drop(release);
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    let delivered_state =
        tilo_test_env::within("the state delivery after admission release", br.read_next())
            .await
            .unwrap();
    assert_eq!(delivered_state, Some(EnvelopeV2::State(envelope)));
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
}

/// A build that still opens the retired private action lane ends its session
/// instead of having its frames read as state.
#[tokio::test]
async fn a_retired_private_action_frame_ends_the_session() {
    let (root, user) = UserIdentity::generate("old build", 1);
    let (source, _) = certified_device(&root, user.public_key, "source");
    let registry = Arc::new(AsyncMutex::new(HashMap::new()));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let (client, server) = tokio::io::duplex(1 << 16);
    let (server_read, server_write) = tokio::io::split(server);
    let session = tokio::spawn(run_peer_stream(
        source.clone(),
        server_read,
        server_write,
        PeerContext {
            workspaces: registry,
            autobind: None,
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            trust_policy: trust,
            own_user_public_key: user.public_key,
            own_device_cert: source.cert.clone(),
            own_device_public_key: source.device_public_key,
            app_serving: None,
            metrics: Arc::new(crate::metrics::Metrics::default()),
            carried_calls: None,
            admin: None,
            here: None,
            fetch: None,
            agents: crate::peer::AgentRegistry::default(),
            positions: None,
            compactor: None,
            member_limits: None,
        },
        std::future::pending::<()>(),
    ));
    // Every retired frame keeps its discriminant, so an old build's frame
    // still decodes; this is the first of them, framed as an old build wrote
    // it.
    let (_, mut write) = tokio::io::split(client);
    write
        .write_all(
            &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::RetiredEnableActions)
                .unwrap(),
        )
        .await
        .unwrap();
    let ended = tilo_test_env::within("the session to end", session)
        .await
        .expect("a retired frame's session outcome");
    match ended {
        Err(PeerError::Protocol(reason)) => assert!(
            reason.contains("action frames are not served"),
            "a retired frame is refused as unserved, got {reason}"
        ),
        other => panic!("a retired private action frame ended the session as {other:?}"),
    }
}
