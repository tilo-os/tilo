use super::*;
use tilo_lock::MutexRecover as _;
use tilo_relay_shell::ShellAuthority as _;
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

#[test]
fn checkpoint_prefix_recovery_requires_enabled_current_exact_governor_session() {
    let (root, user) = UserIdentity::generate("owner", 1);
    let (governor, _) = certified_device(&root, user.public_key, "native");
    let (other, _) = certified_device(&root, user.public_key, "browser");
    let room = uuid::Uuid::from_u128(0x51).to_string();
    let id = crate::room_workspace_id(&room).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime
        .store
        .bind_shell_authority(
            tilo_relay_protocol::retention::RetentionAuthority::new(&room, governor.cert.clone())
                .unwrap(),
        )
        .unwrap();
    runtime.admit_device(governor.device_public_key).unwrap();
    runtime.admit_device(other.device_public_key).unwrap();
    let (governor_tx, _governor_rx) = mpsc::channel(2);
    let (governor_disconnect, _governor_disconnected) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: governor.device_public_key,
        role: governor.role,
        tx: governor_tx.clone(),
        disconnect: governor_disconnect.clone(),
    });
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let source = StateSource {
        peer: governor.clone(),
        tx: governor_tx,
        disconnect: governor_disconnect,
        revocations: revocations.clone(),
        policy: Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        )),
        own_user: user.public_key,
        agents: AgentRegistry::default(),
        federation: None,
        session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
    };
    assert!(
        !checkpoint_prefix_recovery_allowed(
            &runtime,
            &governor,
            governor.device_public_key,
            &source,
        ),
        "exact governor stays serial unless its local host opts in"
    );
    runtime.checkpoint_prefix_recovery =
        crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
    assert!(checkpoint_prefix_recovery_allowed(
        &runtime,
        &governor,
        governor.device_public_key,
        &source,
    ));

    let (other_tx, _other_rx) = mpsc::channel(2);
    let (other_disconnect, _other_disconnected) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: other.device_public_key,
        role: other.role,
        tx: other_tx.clone(),
        disconnect: other_disconnect.clone(),
    });
    let other_source = StateSource {
        peer: other.clone(),
        tx: other_tx,
        disconnect: other_disconnect,
        revocations,
        policy: source.policy.clone(),
        own_user: user.public_key,
        agents: AgentRegistry::default(),
        federation: None,
        session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
    };
    assert!(!checkpoint_prefix_recovery_allowed(
        &runtime,
        &other,
        other.device_public_key,
        &other_source,
    ));
    let mut relay_peer = governor.clone();
    relay_peer.role = crate::auth::PeerRole::PeerRelay;
    assert!(!checkpoint_prefix_recovery_allowed(
        &runtime,
        &relay_peer,
        governor.device_public_key,
        &source,
    ));

    let (replacement, _replacement_rx) = mpsc::channel(2);
    runtime.add_subscriber(Subscriber {
        device_public_key: governor.device_public_key,
        role: governor.role,
        tx: replacement,
        disconnect: source.disconnect.clone(),
    });
    assert!(!checkpoint_prefix_recovery_allowed(
        &runtime,
        &governor,
        governor.device_public_key,
        &source,
    ));
}

#[tokio::test]
async fn exact_history_checkpoint_prefix_production_path_runs_one_selection() {
    use crate::store::bounded::{Step, WorkLimit};
    use tilo_relay_shell::selection::{
        self as shell_selection, Selection,
        fixtures::{producer_frame, producer_metadata},
    };
    let meta = producer_metadata();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    runtime.checkpoint_prefix_recovery =
        crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
    for row in meta["baseline"].as_array().unwrap() {
        let raw = producer_frame(&meta, row, 1);
        let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, &raw).unwrap();
    }
    let scratch = runtime.store.root().join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut initial = Selection::new(&runtime.store, scratch, None).unwrap();
    while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
    let store_root = runtime.store.root().to_path_buf();
    let selected = store_root.join("shell-selected-v1/CURRENT");
    let selected_before = std::fs::read(&selected).unwrap();

    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_value(meta["authority"].clone()).unwrap();
    let peer = AuthenticatedPeer {
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        cert,
        role: crate::auth::PeerRole::Device,
    };
    runtime.admit_device(peer.device_public_key).unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let (disconnect, _disconnected) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: peer.device_public_key,
        role: peer.role,
        tx: tx.clone(),
        disconnect: disconnect.clone(),
    });
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let source = StateSource {
        peer,
        tx,
        disconnect,
        revocations: Arc::new(std::sync::RwLock::new(
            tilo_user_identity::RevocationList::new(),
        )),
        policy: Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        )),
        own_user: serde_json::from_value::<tilo_user_identity::DeviceCert>(
            meta["authority"].clone(),
        )
        .unwrap()
        .user_public_key,
        agents: AgentRegistry::default(),
        federation: None,
        session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
    };
    let envelopes = meta["baseline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let raw = producer_frame(&meta, row, 2);
            crate::transport::decode(&raw[4..]).unwrap()
        })
        .collect::<Vec<_>>();
    let _selection_count = shell_selection::track_selection_constructions(&store_root);
    enqueue_checkpoint_prefix(
        &workspace,
        envelopes,
        source.clone(),
        crate::auth::PeerRole::Device,
        Arc::new(crate::metrics::Metrics::default()),
        vec![None, None, None, None],
    )
    .await
    .unwrap();
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert_eq!(shell_selection::selection_constructions(&store_root), 1);
    assert_ne!(std::fs::read(&selected).unwrap(), selected_before);

    let selected_before_advance = std::fs::read(&selected).unwrap();
    let advancing = meta["checkpoint2"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let raw = producer_frame(&meta, row, 3);
            crate::transport::decode(&raw[4..]).unwrap()
        })
        .collect::<Vec<_>>();
    let _selection_count = shell_selection::track_selection_constructions(&store_root);
    enqueue_checkpoint_prefix(
        &workspace,
        advancing,
        source,
        crate::auth::PeerRole::Device,
        Arc::new(crate::metrics::Metrics::default()),
        vec![None, None, None, None],
    )
    .await
    .unwrap();
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert_eq!(shell_selection::selection_constructions(&store_root), 1);
    assert_ne!(std::fs::read(selected).unwrap(), selected_before_advance);
}

// In an unoptimized build, polling this scenario through the persistence
// owner nests deeper than the default 2 MiB test-thread stack (it needs
// more than 4 MiB when it runs alone). Run it on its own thread with an
// explicit stack instead of depending on RUST_MIN_STACK.
#[test]
fn route_prefix_beyond_the_frame_cap_uses_one_all_original_selection_then_converges() {
    std::thread::Builder::new()
        .name("route-prefix-beyond-cap".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(Box::pin(route_prefix_beyond_cap_scenario()));
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A checkpoint prefix of `PREFIX_ROUTES` record frames, followed by the
/// two records that did not fit. Production caps a prefix at
/// `RETENTION_PREFIX_FRAME_CAP`; selection treats every prefix size
/// alike, so a small cap exercises the same cut at a fraction of the
/// signing and retention work.
async fn route_prefix_beyond_cap_scenario() {
    const PREFIX_ROUTES: usize = 4;
    use crate::store::{
        bounded::{Step, WorkLimit},
        test_support::replay_frames,
    };
    use automerge::transaction::Transactable;
    use tilo_relay_shell::selection::{self as shell_selection, SelectedRead, Selection};

    fn frame(
        authority: &tilo_relay_protocol::retention::RetentionAuthority,
        message: &neighbour_wire::NeighbourMsg,
    ) -> Vec<u8> {
        let channel = neighbour_wire::retention(message, true).unwrap().0;
        let body = postcard::to_allocvec(message).unwrap();
        let source = match message {
            neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } => {
                *envelope.author.as_bytes()
            }
            _ => authority.governor().device_public_key,
        };
        crate::transport::encode(&Envelope::Delta {
            workspace_id: crate::room_workspace_id(authority.room()).unwrap(),
            author_device_public_key: source,
            authored_at_ns: 1,
            signature: [0; 64],
            delta: crate::coedit::CoeditPayload::Checkpoint { channel, body }
                .encode()
                .unwrap(),
        })
        .unwrap()
    }

    let root_key = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "route prefix beyond cap".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root_key).unwrap();
    let (observer, _observer_key) = certified_device(
        &root_key,
        root_key.verifying_key().to_bytes(),
        "route prefix observer",
    );
    let (fenced_observer, _fenced_observer_key) = certified_device(
        &root_key,
        root_key.verifying_key().to_bytes(),
        "route prefix fenced observer",
    );
    let now = web_time::SystemTime::UNIX_EPOCH
        .checked_add(web_time::Duration::from_secs(4000))
        .unwrap();
    let room = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let mut publisher = tilo_peer_state::StatePeer::new(
        room,
        cert.clone(),
        vec![],
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    let documents = (0..PREFIX_ROUTES + 2)
        .map(|index| {
            let mut document = automerge::AutoCommit::new();
            document
                .put(automerge::ROOT, "index", u64::try_from(index).unwrap())
                .unwrap();
            tilo_peer_state::SeedDocument {
                app_id: "threshold.app".into(),
                record_id: format!("record-{index:02}"),
                kind: "test".into(),
                bytes: document.save(),
            }
        })
        .collect();
    publisher
        .install_seed(
            tilo_peer_state::SeedState {
                room_changes: tilo_peer_state::seed_room(
                    room,
                    &cert,
                    &[
                        cert.clone(),
                        observer.cert.clone(),
                        fenced_observer.cert.clone(),
                    ],
                )
                .unwrap(),
                documents,
            },
            &key,
            now,
        )
        .unwrap();
    let authority =
        tilo_relay_protocol::retention::RetentionAuthority::new(room, cert.clone()).unwrap();
    let baseline = publisher
        .durable_bootstrap_messages(&key, now, 1)
        .unwrap()
        .iter()
        .map(|message| frame(&authority, message))
        .collect::<Vec<_>>();
    let next = publisher
        .durable_bootstrap_messages(&key, now, 2)
        .unwrap()
        .iter()
        .map(|message| frame(&authority, message))
        .collect::<Vec<_>>();
    let third = publisher
        .durable_bootstrap_messages(&key, now, 3)
        .unwrap()
        .iter()
        .map(|message| frame(&authority, message))
        .collect::<Vec<_>>();
    assert_eq!(baseline.len(), PREFIX_ROUTES + 3);
    assert_eq!(next.len(), PREFIX_ROUTES + 3);
    assert_eq!(third.len(), PREFIX_ROUTES + 3);

    let id = crate::room_workspace_id(room).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    runtime.checkpoint_prefix_recovery =
        crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
    for raw in &baseline {
        let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, raw).unwrap();
    }
    let scratch = runtime.store.root().join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut initial = Selection::new(&runtime.store, scratch, None).unwrap();
    while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
    let store_root = runtime.store.root().to_path_buf();

    let peer = AuthenticatedPeer {
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        cert,
        role: crate::auth::PeerRole::Device,
    };
    runtime.admit_device(peer.device_public_key).unwrap();
    let (tx, _source_rx) = mpsc::channel(512);
    let (disconnect, mut source_disconnected) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: peer.device_public_key,
        role: peer.role,
        tx: tx.clone(),
        disconnect: disconnect.clone(),
    });
    runtime.admit_device(observer.device_public_key).unwrap();
    let (observer_tx, _observer_rx) = mpsc::channel(512);
    let (observer_state_tx, mut observer_state_rx) = mpsc::channel(512);
    let (observer_disconnect, mut observer_disconnected) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: observer.device_public_key,
        role: observer.role,
        tx: observer_tx.clone(),
        disconnect: observer_disconnect,
    });
    runtime.state_sessions.insert(
        observer.device_public_key,
        StateSession {
            peer: observer.clone(),
            tx: observer_state_tx,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: None,
            lag: None,
        },
    );
    runtime
        .admit_device(fenced_observer.device_public_key)
        .unwrap();
    let (fenced_observer_tx, _fenced_observer_rx) = mpsc::channel(512);
    let (fenced_observer_state_tx, _fenced_observer_state_rx) = mpsc::channel(512);
    let (fenced_observer_disconnect, _fenced_observer_disconnected) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: fenced_observer.device_public_key,
        role: fenced_observer.role,
        tx: fenced_observer_tx,
        disconnect: fenced_observer_disconnect,
    });
    runtime.state_sessions.insert(
        fenced_observer.device_public_key,
        StateSession {
            peer: fenced_observer.clone(),
            tx: fenced_observer_state_tx,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: None,
            lag: None,
        },
    );
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let source = StateSource {
        peer,
        tx,
        disconnect,
        revocations: Arc::new(std::sync::RwLock::new(
            tilo_user_identity::RevocationList::new(),
        )),
        policy: Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        )),
        own_user: root_key.verifying_key().to_bytes(),
        agents: AgentRegistry::default(),
        federation: None,
        session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
    };
    let owner = crate::persistence::handle(&workspace).await;

    enqueue_checkpoint_prefix(
        &workspace,
        vec![crate::transport::decode(&next[0][4..]).unwrap()],
        source.clone(),
        crate::auth::PeerRole::Device,
        Arc::new(crate::metrics::Metrics::default()),
        vec![None],
    )
    .await
    .unwrap();
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    let room_delivery =
        tilo_test_env::within("a message on observer_state_rx", observer_state_rx.recv())
            .await
            .unwrap();
    assert!(
        crate::transport::encode(&room_delivery.envelope).unwrap() == next[0],
        "room Original was not published before its record checkpoint prefix"
    );
    let selection_count = shell_selection::track_selection_constructions(&store_root);
    let batch_modes = track_checkpoint_batch_modes(id);

    // Bounded, payload-free diagnostic: one count and total microseconds per
    // fixed selection phase, observable without taking the owner state lock.
    let progress = Arc::new(std::sync::Mutex::new(CheckpointProgress::default()));
    CHECKPOINT_PROGRESS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(id, progress.clone());

    // This prefix is the verified FinalBasisOmission case: one projected
    // Selection has switched CURRENT and the first exact Original is
    // already published before the injected error. Replace one captured
    // session while paused to prove that the immutable fence only removes
    // exact old senders.
    let fault =
        crate::store::test_barrier::fail(&store_root, "checkpoint-prefix-after-first-original");
    let (release, reached) =
        crate::store::test_barrier::install(&store_root, "checkpoint-prefix-after-first-original");
    enqueue_checkpoint_prefix(
        &workspace,
        next.iter()
            .skip(1)
            .take(PREFIX_ROUTES)
            .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
            .collect(),
        source.clone(),
        crate::auth::PeerRole::Device,
        Arc::new(crate::metrics::Metrics::default()),
        vec![None; PREFIX_ROUTES],
    )
    .await
    .unwrap();
    // A liveness guard, not a latency budget: a busy machine must not
    // fail this on timing.
    tokio::time::timeout(tilo_test_env::hang_bound(), reached)
        .await
        .unwrap_or_else(|_| {
            let source_disconnect = match source_disconnected.try_recv() {
                Ok(()) => "signalled",
                Err(mpsc::error::TryRecvError::Empty) => "open",
                Err(mpsc::error::TryRecvError::Disconnected) => "closed",
            };
            let modes = batch_modes.lock_recover();
            panic!(
                "all-original prefix never reached the post-first-original barrier: selection_count={} batch_modes={modes:?} source_disconnect={source_disconnect} progress={:?}",
                selection_count.load(std::sync::atomic::Ordering::SeqCst),
                progress.lock_recover(),
            );
        })
        .unwrap();
    let delivered_before_failure = tilo_test_env::within(
        "first exact Original was not visible before the injected failure",
        observer_state_rx.recv(),
    )
    .await
    .unwrap();
    assert!(
        crate::transport::encode(&delivered_before_failure.envelope).unwrap() == next[1],
        "injected failure occurred before the first exact Original publication"
    );
    let (replacement_tx, _replacement_rx) = mpsc::channel(512);
    let (replacement_state_tx, _replacement_state_rx) = mpsc::channel(512);
    let (replacement_disconnect, mut replacement_disconnected) = mpsc::channel(1);
    {
        let mut runtime = workspace.lock().await;
        runtime.add_subscriber(Subscriber {
            device_public_key: fenced_observer.device_public_key,
            role: fenced_observer.role,
            tx: replacement_tx.clone(),
            disconnect: replacement_disconnect,
        });
        runtime.state_sessions.insert(
            fenced_observer.device_public_key,
            StateSession {
                peer: fenced_observer.clone(),
                tx: replacement_state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
    }
    drop(release);
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    tilo_test_env::within(
        "post-CURRENT failure did not fence the captured observer",
        observer_disconnected.recv(),
    )
    .await
    .expect("captured observer disconnect channel closed before fencing");
    assert!(
        replacement_disconnected.try_recv().is_err(),
        "immutable-session fence disconnected the replacement session"
    );
    {
        let runtime = workspace.lock().await;
        assert!(
            !runtime
                .state_sessions
                .contains_key(&observer.device_public_key),
            "captured observer state session survived the post-CURRENT failure"
        );
        assert!(
            runtime.subscribers[&fenced_observer.device_public_key]
                .tx
                .same_channel(&replacement_tx),
            "post-CURRENT fence removed a newer replacement session"
        );
    }
    drop(fault);

    // A reconnect receives the same durable prefix without a selected
    // basis repair. Its successful suffix must converge normally.
    let (reopened_tx, _reopened_rx) = mpsc::channel(512);
    let (reopened_state_tx, mut reopened_state_rx) = mpsc::channel(512);
    let (reopened_disconnect, mut reopened_disconnected) = mpsc::channel(1);
    {
        let mut runtime = workspace.lock().await;
        runtime.add_subscriber(Subscriber {
            device_public_key: observer.device_public_key,
            role: observer.role,
            tx: reopened_tx.clone(),
            disconnect: reopened_disconnect,
        });
        runtime.state_sessions.insert(
            observer.device_public_key,
            StateSession {
                peer: observer.clone(),
                tx: reopened_state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
    }
    enqueue_checkpoint_prefix(
        &workspace,
        next.iter()
            .skip(1)
            .take(PREFIX_ROUTES)
            .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
            .collect(),
        source.clone(),
        crate::auth::PeerRole::Device,
        Arc::new(crate::metrics::Metrics::default()),
        vec![None; PREFIX_ROUTES],
    )
    .await
    .unwrap();
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert_eq!(
        selection_count.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "failed prefix and reconnect did not each use one global Selection"
    );
    assert_eq!(
        *batch_modes.lock().unwrap(),
        [
            CheckpointBatchMode::AllOriginal,
            CheckpointBatchMode::AllOriginal,
        ],
        "the failed prefix or reconnect did not take the guarded all-Original path"
    );
    for (index, expected) in next.iter().skip(1).take(PREFIX_ROUTES).enumerate() {
        let observed =
            tilo_test_env::within("a message on reopened_state_rx", reopened_state_rx.recv())
                .await
                .unwrap();
        assert!(
            crate::transport::encode(&observed.envelope).unwrap() == *expected,
            "all-Original checkpoint publication changed wire order at input {index}"
        );
    }
    assert!(
        replacement_disconnected.try_recv().is_err() && reopened_disconnected.try_recv().is_err(),
        "successful reconnect recovery disconnected a current session"
    );
    let retained_after_prefix = {
        let runtime = workspace.lock().await;
        replay_frames(&runtime.store)
            .into_iter()
            .map(|frame| crate::transport::encode(&frame).unwrap())
            .collect::<Vec<_>>()
    };
    for raw in baseline.iter().chain(next.iter().take(PREFIX_ROUTES + 1)) {
        assert!(
            retained_after_prefix.contains(raw),
            "selected CURRENT omitted old basis or an exact Original prefix frame"
        );
    }

    enqueue_checkpoint_prefix(
        &workspace,
        next.iter()
            .skip(PREFIX_ROUTES + 1)
            .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
            .collect(),
        source.clone(),
        crate::auth::PeerRole::Device,
        Arc::new(crate::metrics::Metrics::default()),
        vec![None; 2],
    )
    .await
    .unwrap();
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert_eq!(
        *batch_modes.lock().unwrap(),
        [
            CheckpointBatchMode::AllOriginal,
            CheckpointBatchMode::AllOriginal,
            CheckpointBatchMode::Represented
        ],
        "the remaining checkpoints did not complete the represented selected basis"
    );
    let after_reconnect = {
        let runtime = workspace.lock().await;
        replay_frames(&runtime.store)
            .into_iter()
            .map(|frame| crate::transport::encode(&frame).unwrap())
            .collect::<Vec<_>>()
    };
    for raw in &next {
        assert!(
            after_reconnect.contains(raw),
            "reconnect suffix did not converge to the complete next checkpoint generation"
        );
    }
    let chunks = [
        (third.iter().take(1).collect::<Vec<_>>(), vec![None]),
        (
            third.iter().skip(1).take(PREFIX_ROUTES).collect::<Vec<_>>(),
            vec![None; PREFIX_ROUTES],
        ),
        (
            third.iter().skip(PREFIX_ROUTES + 1).collect::<Vec<_>>(),
            vec![None; 2],
        ),
    ];
    for (chunk, budgets) in chunks {
        enqueue_checkpoint_prefix(
            &workspace,
            chunk
                .into_iter()
                .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
                .collect(),
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            budgets,
        )
        .await
        .unwrap();
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
    }
    assert!(
        replacement_disconnected.try_recv().is_err() && reopened_disconnected.try_recv().is_err(),
        "later automatic recovery disconnected a current session"
    );
    let runtime = workspace.lock().await;
    let final_frames = replay_frames(&runtime.store)
        .into_iter()
        .map(|frame| crate::transport::encode(&frame).unwrap())
        .collect::<Vec<_>>();
    for raw in &third {
        assert!(
            final_frames.contains(raw),
            "reconnect recovery did not retain the next complete checkpoint generation"
        );
    }
    let mut selected = SelectedRead::new(&runtime.store).unwrap().unwrap();
    while matches!(selected.step().unwrap(), Step::More) {}
}

#[tokio::test]
async fn checkpoint_stage_allocation_error_registers_candidate_for_owner_cleanup() {
    use tilo_relay_shell::selection::fixtures::{producer_frame, producer_metadata};
    let meta = producer_metadata();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        let raw = producer_frame(&meta, row, 1);
        let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, &raw).unwrap();
    }
    let raw = producer_frame(&meta, &meta["baseline"][0], 2);
    let envelope = crate::transport::decode(&raw[4..]).unwrap();
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = &envelope
    else {
        panic!()
    };
    let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
        crate::coedit::CoeditPayload::decode(delta).unwrap()
    else {
        panic!()
    };
    let lineage = runtime.store.root().join("retained/v2").join(format!(
        "{}-{}",
        hex::encode(author_device_public_key),
        hex::encode(channel)
    ));
    let current_before = std::fs::read(lineage.join("CURRENT")).unwrap();
    let entries_before = std::fs::read_dir(&lineage).unwrap().count();
    let fault = crate::store::test_barrier::fail(&lineage, "checkpoint-stage-after-allocation");
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let owner = crate::persistence::handle(&workspace).await;
    let result = owner
        .submit_sequence_wait(1, raw.len(), move |state, _| async move {
            let mut state = state.lock_recover();
            let mut batch = new_checkpoint_batch(&mut state, vec![envelope], None)?;
            loop {
                match step_checkpoint_batch(&mut state, &mut batch)? {
                    Step::More => {}
                    Step::Done(()) => return Ok(()),
                }
            }
        })
        .await
        .unwrap();
    assert!(crate::persistence::receipt(result).await.is_err());
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    drop(fault);
    assert_eq!(
        std::fs::read(lineage.join("CURRENT")).unwrap(),
        current_before
    );
    assert_eq!(
        std::fs::read_dir(&lineage).unwrap().count(),
        entries_before,
        "owner cleanup left an inactive checkpoint candidate"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reader_buffers_authenticated_checkpoint_suffix_into_one_owner_batch() {
    use crate::store::bounded::{Step, WorkLimit};
    use tilo_relay_shell::selection::{
        self as shell_selection, Selection,
        fixtures::{producer_frame, producer_metadata},
    };
    let meta = producer_metadata();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    runtime.checkpoint_prefix_recovery =
        crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
    for row in meta["baseline"].as_array().unwrap() {
        let raw = producer_frame(&meta, row, 1);
        let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, &raw).unwrap();
    }
    let scratch = runtime.store.root().join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut initial = Selection::new(&runtime.store, scratch, None).unwrap();
    while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
    let store_root = runtime.store.root().to_path_buf();
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_value(meta["authority"].clone()).unwrap();
    let peer = AuthenticatedPeer {
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        cert,
        role: crate::auth::PeerRole::Device,
    };
    runtime.admit_device(peer.device_public_key).unwrap();
    // This test occupies every normal-message permit below; give the
    // workspace its own admission budgets so that saturation cannot
    // starve concurrently running tests that share the process-wide ones.
    crate::persistence::isolate_test_admission_with_workers(
        &runtime,
        crate::persistence::isolated_test_workers(),
    );
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let (client, server) = tokio::io::duplex(1024 * 1024);
    let (read, write) = tokio::io::split(server);
    let task = tokio::spawn(run_peer_stream(
        peer.clone(),
        read,
        write,
        PeerContext {
            workspaces: registry,
            autobind: None,
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            trust_policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user_public_key: peer.user_public_key,
            own_device_cert: peer.cert.clone(),
            own_device_public_key: peer.cert.device_public_key,
            app_serving: None,
            metrics,
            live_bridge: None,
            agents: crate::peer::AgentRegistry::default(),
            positions: None,
            compactor: None,
            member_limits: None,
        },
        std::future::pending::<()>(),
    ));
    let (client_read, mut client_write) = tokio::io::split(client);
    let mut client_reader = SessionReader::new(client_read);
    client_write
        .write_all(
            &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                workspace_id: id,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    loop {
        if matches!(
            client_reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ready { .. }))
        ) {
            break;
        }
    }
    let drain = tokio::spawn(async move {
        while client_reader
            .read_next()
            .await
            .is_ok_and(|frame| frame.is_some())
        {}
    });

    // Occupy all normal-message permits in one already-admitted owner job.
    // The reader can still acquire and retain delivery budgets for later
    // frames while the first Delta waits for normal admission.
    let owner = crate::persistence::handle(&workspace).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let held = owner
        .submit_sequence_wait(
            u32::try_from(crate::persistence::NORMAL_MESSAGES).unwrap(),
            1,
            move |_, _| async move {
                let _ = entered_tx.send(());
                let _ = release_rx.await;
                Ok(())
            },
        )
        .await
        .unwrap();
    entered_rx.await.unwrap();
    let observed = track_reader_backlog(id);
    let selection_count = shell_selection::track_selection_constructions(&store_root);
    let blocker_raw = producer_frame(&meta, &meta["baseline"][0], 1);
    let blocker = crate::transport::decode(&blocker_raw[4..]).unwrap();
    let envelopes = meta["checkpoint2"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let raw = producer_frame(&meta, row, 2);
            crate::transport::decode(&raw[4..]).unwrap()
        })
        .collect::<Vec<_>>();
    client_write
        .write_all(&tilo_delivery_protocol::transport_v2::encode_state(&blocker).unwrap())
        .await
        .unwrap();
    for envelope in &envelopes {
        client_write
            .write_all(&tilo_delivery_protocol::transport_v2::encode_state(envelope).unwrap())
            .await
            .unwrap();
    }
    while observed.load(std::sync::atomic::Ordering::SeqCst) < envelopes.len() {
        assert!(
            !task.is_finished(),
            "reader ended before retaining the authenticated suffix: {:?}",
            task.await
        );
        tokio::task::yield_now().await;
    }
    // A clean FIN after complete authenticated frames must not discard
    // those frames while the preceding admission is backpressured.
    client_write.shutdown().await.unwrap();
    release_tx.send(()).unwrap();
    crate::persistence::receipt(held).await.unwrap();
    while selection_count.load(std::sync::atomic::Ordering::SeqCst) < 1 {
        assert!(
            !task.is_finished(),
            "reader suffix failed after {} selections: {:?}",
            selection_count.load(std::sync::atomic::Ordering::SeqCst),
            task.await
        );
        tokio::task::yield_now().await;
    }
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    let tail = envelopes.last().unwrap();
    tilo_test_env::within("the buffered checkpoint tail to become durable", async {
        loop {
            let retained = workspace.lock().await.store.load_retained().unwrap();
            if retained
                .iter()
                .any(|raw| crate::transport::decode(&raw[4..]).unwrap() == *tail)
            {
                break;
            }
            assert!(
                !task.is_finished(),
                "peer ended before persisting the buffered tail"
            );
            tokio::task::yield_now().await;
        }
    })
    .await;
    stop_tracking_reader_backlog(id);
    assert_eq!(shell_selection::selection_constructions(&store_root), 1);
    tilo_test_env::within("peer to finish after clean FIN", task)
        .await
        .unwrap()
        .unwrap();
    drain.abort();
    let _ = drain.await;
}

async fn checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
    operation: &'static str,
    force_serial_finish_gc: bool,
) {
    use crate::store::bounded::{Step, WorkLimit};
    use tilo_relay_shell::selection::{
        Selection,
        fixtures::{producer_frame, producer_metadata},
    };
    let meta = producer_metadata();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    runtime.checkpoint_prefix_recovery =
        crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
    for row in meta["baseline"].as_array().unwrap() {
        let raw = producer_frame(&meta, row, 1);
        let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, &raw).unwrap();
    }
    let scratch = runtime.store.root().join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut initial = Selection::new(&runtime.store, scratch, None).unwrap();
    while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
    let store = runtime.store.clone();

    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_value(meta["authority"].clone()).unwrap();
    let source_peer = AuthenticatedPeer {
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        cert,
        role: crate::auth::PeerRole::Device,
    };
    runtime.admit_device(source_peer.device_public_key).unwrap();
    let (source_tx, _source_rx) = mpsc::channel(32);
    let (source_disconnect, _source_disconnected) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: source_peer.device_public_key,
        role: source_peer.role,
        tx: source_tx.clone(),
        disconnect: source_disconnect.clone(),
    });

    let (root, user) = UserIdentity::generate("checkpoint prefix recipients", 1);
    let mut peers = Vec::new();
    let mut disconnects = Vec::new();
    for label in ["original", "replaced"] {
        let (peer, _) = certified_device(&root, user.public_key, label);
        runtime.admit_device(peer.device_public_key).unwrap();
        let (tx, _rx) = mpsc::channel(32);
        let (disconnect, disconnected) = mpsc::channel(1);
        let (state_tx, _state_rx) = mpsc::channel(32);
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx,
            disconnect,
        });
        runtime.state_sessions.insert(
            peer.device_public_key,
            StateSession {
                peer: peer.clone(),
                tx: state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
        peers.push(peer);
        disconnects.push(disconnected);
    }
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let source = StateSource {
        peer: source_peer,
        tx: source_tx,
        disconnect: source_disconnect,
        revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
        policy: Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        )),
        own_user: serde_json::from_value::<tilo_user_identity::DeviceCert>(
            meta["authority"].clone(),
        )
        .unwrap()
        .user_public_key,
        agents: AgentRegistry::default(),
        federation: None,
        session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
    };
    let real_envelopes = meta["checkpoint2"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let raw = producer_frame(&meta, row, 2);
            crate::transport::decode(&raw[4..]).unwrap()
        })
        .collect::<Vec<_>>();
    let mut envelopes = real_envelopes.clone();
    let fault_root = if force_serial_finish_gc {
        let Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            signature,
            delta,
        } = envelopes[0].clone()
        else {
            unreachable!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { body, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            unreachable!()
        };
        envelopes.push(Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            signature,
            delta: crate::coedit::CoeditPayload::Checkpoint {
                channel: [0xee; 32],
                body,
            }
            .encode()
            .unwrap(),
        });
        let Envelope::Delta {
            author_device_public_key,
            delta,
            ..
        } = real_envelopes.last().unwrap()
        else {
            unreachable!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(delta).unwrap()
        else {
            unreachable!()
        };
        store.root().join("retained/v2").join(format!(
            "{}-{}",
            hex::encode(author_device_public_key),
            hex::encode(channel)
        ))
    } else {
        store.root().to_path_buf()
    };
    let fault = crate::store::test_barrier::fail(&fault_root, operation);
    let (release, reached) = crate::store::test_barrier::install(&fault_root, operation);
    enqueue_checkpoint_prefix(
        &workspace,
        envelopes.clone(),
        source.clone(),
        crate::auth::PeerRole::Device,
        Arc::new(crate::metrics::Metrics::default()),
        vec![None; envelopes.len()],
    )
    .await
    .unwrap();
    // Event-driven: a prefix that never reaches the fault hangs here and
    // nextest's slow-timeout reports it; load cannot fail a wall-clock budget.
    reached
        .await
        .expect("checkpoint prefix never reached its post-CURRENT fault");

    let (new_tx, _new_rx) = mpsc::channel(32);
    let (new_disconnect, mut new_disconnected) = mpsc::channel(1);
    let (new_state_tx, _new_frames) = mpsc::channel(32);
    {
        let peer = &peers[1];
        let mut runtime = workspace.lock().await;
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: new_tx.clone(),
            disconnect: new_disconnect,
        });
        runtime.state_sessions.insert(
            peer.device_public_key,
            StateSession {
                peer: peer.clone(),
                tx: new_state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
    }
    drop(release);
    let owner = crate::persistence::handle(&workspace).await;
    // The owner runs jobs in order: this receipt completes once the
    // checkpoint prefix before it has drained.
    let done = owner
        .submit_resumable_wait(
            1,
            async { Ok(()) },
            |_, _| Ok(Step::Done(())),
            |result| async { result },
        )
        .await
        .unwrap();
    crate::persistence::receipt(done)
        .await
        .expect("checkpoint prefix owner did not drain");
    if force_serial_finish_gc {
        let promoted =
            String::from_utf8(std::fs::read(fault_root.join("CURRENT")).unwrap()).unwrap();
        assert!(
            fault_root.join(promoted).is_dir(),
            "cleanup removed the promoted lineage generation"
        );
        let selected = store.root().join("shell-selected-v1");
        let current_name =
            String::from_utf8(std::fs::read(selected.join("CURRENT")).unwrap()).unwrap();
        let current = selected.join(current_name);
        let entries = std::fs::read_dir(&selected)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(
            entries.len(),
            2,
            "abandoned projected or serial selected generation was not reclaimed: {entries:?}"
        );
        assert!(entries.contains(&selected.join("CURRENT")));
        assert!(entries.contains(&current));
    }
    drop(fault);
    enqueue_checkpoint_prefix(
        &workspace,
        real_envelopes.clone(),
        source,
        crate::auth::PeerRole::Device,
        Arc::new(crate::metrics::Metrics::default()),
        vec![None; real_envelopes.len()],
    )
    .await
    .unwrap();
    // The owner runs jobs in order: this receipt completes once the
    // checkpoint prefix before it has drained.
    let done = owner
        .submit_resumable_wait(
            1,
            async { Ok(()) },
            |_, _| Ok(Step::Done(())),
            |result| async { result },
        )
        .await
        .unwrap();
    crate::persistence::receipt(done)
        .await
        .expect("checkpoint prefix owner did not drain");

    assert!(
        disconnects[0].try_recv().is_ok(),
        "post-CURRENT {operation} failure must fence the captured prefix recipient"
    );
    {
        let runtime = workspace.lock().await;
        assert!(
            !runtime
                .state_sessions
                .contains_key(&peers[0].device_public_key)
        );
        assert!(
            runtime.subscribers[&peers[1].device_public_key]
                .tx
                .same_channel(&new_tx)
        );
    }
    assert!(
        new_disconnected.try_recv().is_err(),
        "replacement session must survive the old prefix failure"
    );
}

#[tokio::test]
async fn checkpoint_prefix_post_current_failure_fences_only_original_sessions() {
    Box::pin(
        checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
            "shell-after-current",
            false,
        ),
    )
    .await;
}

#[tokio::test]
async fn checkpoint_prefix_cursor_open_failure_fences_only_original_sessions() {
    Box::pin(
        checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
            "shell-publication-open",
            false,
        ),
    )
    .await;
}

#[tokio::test]
async fn checkpoint_prefix_serial_gc_failure_fences_only_original_sessions() {
    Box::pin(
        checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
            "checkpoint-stage-finish-gc",
            true,
        ),
    )
    .await;
}

async fn activation_failure_fences_original_sessions_and_retry(operation: &'static str) {
    use crate::store::test_support::replay_frames;
    use tilo_relay_shell::selection::fixtures::{
        admit_frames, producer_fixture, producer_frame, producer_metadata,
    };
    let meta = producer_metadata();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    // Both captured sessions are already ready on this durable old basis.
    for row in meta["baseline"].as_array().unwrap() {
        let raw = producer_frame(&meta, row, 1);
        let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, &raw).unwrap();
    }
    let store = runtime.store.clone();
    let (root, user) = UserIdentity::generate("activation fault recipients", 1);
    let mut peers = Vec::new();
    #[expect(
        clippy::collection_is_never_read,
        reason = "holds the receivers open so their senders stay connected"
    )]
    let mut ordinary_receivers = Vec::new();
    let mut disconnects = Vec::new();
    let mut frames = Vec::new();
    for label in ["original", "replaced"] {
        let (peer, _) = certified_device(&root, user.public_key, label);
        runtime.admit_device(peer.device_public_key).unwrap();
        let (tx, rx) = mpsc::channel(32);
        let (disconnect, disconnected) = mpsc::channel(1);
        let (state_tx, state_rx) = mpsc::channel(32);
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx,
            disconnect,
        });
        runtime.state_sessions.insert(
            peer.device_public_key,
            StateSession {
                peer: peer.clone(),
                tx: state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
        peers.push(peer);
        ordinary_receivers.push(rx);
        disconnects.push(disconnected);
        frames.push(state_rx);
    }
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let room_half = producer_frame(&meta, &meta["tail"][0], 2);
    enqueue_state(
        &workspace,
        crate::transport::decode(&room_half[4..]).unwrap(),
        None,
        crate::auth::PeerRole::Device,
        metrics.clone(),
    )
    .await
    .unwrap();
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    let current = store.root().join("shell-selected-v1/CURRENT");
    let before = std::fs::read(&current).unwrap();
    let raw = producer_frame(&meta, &meta["tail"][1], 3);
    let fault = crate::store::test_barrier::fail(store.root(), operation);
    let (release, reached) = crate::store::test_barrier::install(store.root(), operation);
    enqueue_state(
        &workspace,
        crate::transport::decode(&raw[4..]).unwrap(),
        None,
        crate::auth::PeerRole::Device,
        metrics.clone(),
    )
    .await
    .unwrap();
    tilo_test_env::within(
        "selected activation never reached its fault barrier",
        reached,
    )
    .await
    .unwrap();
    assert_ne!(
        std::fs::read(&current).unwrap(),
        before,
        "injection must follow CURRENT switch"
    );
    // A newer session replaces one captured channel while the original job
    // is paused. Error fencing must never disconnect this replacement.
    let (new_tx, _new_rx) = mpsc::channel(32);
    let (new_disconnect, mut new_disconnected) = mpsc::channel(1);
    let (new_state_tx, mut new_frames) = mpsc::channel(32);
    {
        let peer = &peers[1];
        let mut runtime = workspace.lock().await;
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: new_tx.clone(),
            disconnect: new_disconnect,
        });
        runtime.state_sessions.insert(
            peer.device_public_key,
            StateSession {
                peer: peer.clone(),
                tx: new_state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
    }
    drop(release);
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    drop(fault);
    // Retry identical bytes after the durable cut: suppression must not
    // leave an old ready recipient connected forever without activation.
    enqueue_state(
        &workspace,
        crate::transport::decode(&raw[4..]).unwrap(),
        None,
        crate::auth::PeerRole::Device,
        metrics,
    )
    .await
    .unwrap();
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert!(
        disconnects[0].try_recv().is_ok(),
        "post-cut {operation} error must fence captured recipient before exact retry suppresses it"
    );
    assert!(frames[0].try_recv().is_err());
    {
        let runtime = workspace.lock().await;
        assert!(
            !runtime
                .state_sessions
                .contains_key(&peers[0].device_public_key)
        );
        assert!(
            runtime.subscribers[&peers[1].device_public_key]
                .tx
                .same_channel(&new_tx)
        );
    }
    assert!(
        new_disconnected.try_recv().is_err(),
        "replacement channel must survive old job failure"
    );
    assert!(
        new_frames.try_recv().is_err(),
        "exact retry remains semantically suppressed"
    );
    // Reconnect captures the real durable basis after resumed GC, admitting
    // every frame and converging to the producer's exact app heads.
    let peer = admit_frames(
        &meta,
        &producer_fixture().join("baseline-peer.postcard"),
        replay_frames(&store),
    );
    for row in meta["rows"].as_array().unwrap() {
        assert_eq!(
            peer.document_heads(
                row["app_id"].as_str().unwrap(),
                row["record_id"].as_str().unwrap()
            )
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
            serde_json::from_value::<Vec<String>>(row["heads"].clone()).unwrap()
        );
    }
}

#[tokio::test]
async fn selected_activation_post_current_failure_fences_original_sessions_on_exact_retry() {
    activation_failure_fences_original_sessions_and_retry("shell-after-current").await;
}
#[tokio::test]
async fn selected_activation_cursor_open_failure_fences_original_sessions_on_exact_retry() {
    activation_failure_fences_original_sessions_and_retry("shell-publication-open").await;
}

#[tokio::test]
async fn selected_shell_activation_publishes_complete_ordered_pair_and_suppresses_delayed_checkpoint()
 {
    use tilo_relay_shell::selection::fixtures::{
        admit_frames, producer_fixture, producer_frame, producer_metadata,
    };
    let meta = producer_metadata();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    let (root, user) = UserIdentity::generate("activation recipient", 1);
    let (receiver, _) = certified_device(&root, user.public_key, "recipient");
    runtime.admit_device(receiver.device_public_key).unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let (disconnect, _) = mpsc::channel(1);
    let (state_tx, mut state_rx) = mpsc::channel(32);
    runtime.add_subscriber(Subscriber {
        device_public_key: receiver.device_public_key,
        role: receiver.role,
        tx,
        disconnect,
    });
    runtime.state_sessions.insert(
        receiver.device_public_key,
        StateSession {
            peer: receiver,
            tx: state_tx,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: None,
            lag: None,
        },
    );
    let (slow, _) = certified_device(&root, user.public_key, "saturated recipient");
    runtime.admit_device(slow.device_public_key).unwrap();
    let (slow_tx, _slow_rx) = mpsc::channel(1);
    let (slow_disconnect, mut slow_disconnected) = mpsc::channel(1);
    let (slow_state, mut slow_frames) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: slow.device_public_key,
        role: slow.role,
        tx: slow_tx,
        disconnect: slow_disconnect,
    });
    runtime.state_sessions.insert(
        slow.device_public_key,
        StateSession {
            peer: slow,
            tx: slow_state,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: None,
            lag: None,
        },
    );
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let mut delivered_all = Vec::new();
    for (group, start, end, expected) in [
        ("baseline", 0, 4, 4),
        ("tail", 0, 2, 2),
        ("tail", 2, 3, 1),
        ("checkpoint2", 0, 4, 4),
    ] {
        for row in &meta[group].as_array().unwrap()[start..end] {
            let raw = producer_frame(&meta, row, 10);
            enqueue_state(
                &workspace,
                crate::transport::decode(&raw[4..]).unwrap(),
                None,
                crate::auth::PeerRole::Device,
                metrics.clone(),
            )
            .await
            .unwrap();
        }
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        let mut frames = Vec::new();
        while let Ok(delivery) = state_rx.try_recv() {
            frames.push((*delivery.envelope).clone());
        }
        assert_eq!(
            frames.len(),
            expected,
            "activation preserves exact selective live output for {group}[{start}..{end}]"
        );
        delivered_all.extend(frames);
        admit_frames(
            &meta,
            &producer_fixture().join("baseline-peer.postcard"),
            delivered_all.clone(),
        );
    }
    assert!(
        slow_disconnected.try_recv().is_ok(),
        "bounded activation queue saturation fences partial recipient"
    );
    assert!(slow_frames.try_recv().is_ok());
    assert!(slow_frames.try_recv().is_err());
    for row in meta["baseline"].as_array().unwrap() {
        let raw = producer_frame(&meta, row, u64::MAX - 1);
        enqueue_state(
            &workspace,
            crate::transport::decode(&raw[4..]).unwrap(),
            None,
            crate::auth::PeerRole::Device,
            metrics.clone(),
        )
        .await
        .unwrap();
    }
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert!(
        state_rx.try_recv().is_err(),
        "delayed CP1 must not invalidate a ready CP2 receiver"
    );
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
        enqueue_state(
            &workspace,
            envelope.clone(),
            None,
            crate::auth::PeerRole::Device,
            metrics.clone(),
        )
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
async fn paused_state_persistence_releases_routing_lock() {
    paused_state_private_roundtrip(true, false, false, false, None).await;
}

/// A peer that pipelines more private requests than the reply queue holds,
/// while its own replies are not yet read, is backpressured: the relay
/// stops reading until the writer drains, and never disconnects it.
#[tokio::test(start_paused = true)]
async fn pipelined_private_requests_backpressure_instead_of_disconnecting() {
    const REQUESTS: usize = PEER_OUTBOUND_CAPACITY * 4;
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.admit_device(a.device_public_key).unwrap();
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(runtime)),
    )])));
    let ctx = PeerContext {
        workspaces: registry,
        autobind: None,
        revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
        trust_policy: Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        )),
        own_user_public_key: user.public_key,
        own_device_cert: a.cert.clone(),
        own_device_public_key: a.cert.device_public_key,
        app_serving: None,
        metrics: Arc::new(crate::metrics::Metrics::default()),
        live_bridge: None,
        agents: crate::peer::AgentRegistry::default(),
        positions: None,
        compactor: None,
        member_limits: None,
    };
    // Requests flow through a roomy pipe; replies through a small one, so
    // the relay's writer blocks until the client reads.
    let (mut requests, server_read) = tokio::io::duplex(1 << 20);
    let (server_write, replies) = tokio::io::duplex(256);
    let (server_read, _) = tokio::io::split(server_read);
    let (_, server_write) = tokio::io::split(server_write);
    let (replies, _) = tokio::io::split(replies);
    let relay = tokio::spawn(run_peer_stream(
        a.clone(),
        server_read,
        server_write,
        ctx,
        std::future::pending::<()>(),
    ));
    let mut reader = SessionReader::new(replies);
    requests
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
    let enable =
        tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions { room }).unwrap();
    for _ in 0..REQUESTS {
        requests.write_all(&enable).await.unwrap();
    }
    // The clock is paused: this returns only once the relay has nothing
    // left to do, its writer blocked on the unread replies.
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    assert!(
        !relay.is_finished(),
        "the relay dropped a backpressured peer"
    );
    for _ in 0..REQUESTS {
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::ActionsReady { .. })
        ));
    }
    assert!(!relay.is_finished());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_state_persistence_allows_same_connection_private_roundtrip() {
    paused_state_private_roundtrip(false, false, false, false, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_replay_cleanup_with_full_reply_queue_keeps_private_reader_and_writer_live() {
    paused_state_private_roundtrip(false, true, false, false, None).await;
}

#[tokio::test]
async fn full_control_backlog_backpressures_and_recovers_after_held_writer() {
    paused_state_private_roundtrip(false, true, true, false, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overtaking_private_keeps_held_state_authorization_future_pollable() {
    paused_state_private_roundtrip(false, false, false, true, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_subscription_recovers_retained_frame_after_live_budget_exhaustion() {
    paused_state_private_roundtrip(false, false, false, false, Some(LagBoundary::Live)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_subscription_discards_stale_lag_after_explicit_unsubscribe() {
    paused_state_private_roundtrip(false, false, false, false, Some(LagBoundary::Unsubscribed))
        .await;
}

enum LagBoundary {
    Live,
    Unsubscribed,
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "each flag selects one independent fault in the shared scenario"
)]
async fn paused_state_private_roundtrip(
    check_lock: bool,
    held_cleanup: bool,
    full_backlog: bool,
    held_authorization: bool,
    plain_lag: Option<LagBoundary>,
) {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    for device in [a.device_public_key, b.device_public_key, [99; 32]] {
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
            own_device_cert: a.cert.clone(),
            own_device_public_key: a.cert.device_public_key,
            app_serving: None,
            metrics: Arc::new(crate::metrics::Metrics::default()),
            live_bridge: None,
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
        write
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions { room })
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::ActionsReady { .. })
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
            for frame in [Envelope::Unsubscribe { workspace_id: id }, Envelope::Ping] {
                bw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&frame).unwrap())
                    .await
                    .unwrap();
            }
            let closed = tilo_test_env::within("the explicit unsubscribe to complete", async {
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
        tilo_test_env::within("retained catch-up before the next peer reply", async {
            loop {
                match br.read_next().await.unwrap() {
                    Some(EnvelopeV2::State(Envelope::Pong)) => break,
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
    let envelope = Envelope::Delta {
        workspace_id: id,
        author_device_public_key: a.device_public_key,
        authored_at_ns: 1,
        signature: [0; 64],
        delta: crate::coedit::CoeditPayload::Checkpoint {
            channel: [3; 32],
            body: vec![4; 4096],
        }
        .encode()
        .unwrap(),
    };
    let (release, reached) = crate::store::test_barrier::install(
        store.root(),
        if held_authorization {
            "membership-read"
        } else if held_cleanup {
            "replay-cleanup"
        } else {
            "state-persist"
        },
    );
    if held_cleanup {
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
    } else {
        aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&envelope).unwrap())
            .await
            .unwrap();
    }
    reached.await.unwrap();
    if held_cleanup {
        for _ in 0..if full_backlog { 64 } else { 12 } {
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap(),
            )
            .await
            .unwrap();
        }
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
            assert_eq!(
                ar.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Pong))
            );
        }
        for task in tasks {
            task.abort();
        }
        return;
    }
    assert!(
        store.load_retained().unwrap().is_empty(),
        "state published before commit"
    );
    let request = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    if held_authorization {
        // The state handler holds the authorization lane while its real
        // filesystem read is paused. Confirm the nested private handler
        // has entered before releasing the read: no timing-based race.
        let (private_release, private_reached) =
            crate::store::test_barrier::install(store.root(), "private-authorization");
        aw.write_all(
            &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(request.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
        // Both waits are events, not wall-clock budgets: a private handler
        // that cannot enter lookahead, or a state future that stops being
        // polled, hangs here and nextest's slow-timeout reports it.
        private_reached
            .await
            .expect("private handler never entered lookahead");
        drop(release);
        drop(private_release);
        let mut state_seen = false;
        let mut private_seen = false;
        while !state_seen || !private_seen {
            match br.read_next().await.unwrap() {
                Some(EnvelopeV2::State(actual)) => {
                    assert_eq!(actual, envelope);
                    assert!(!state_seen);
                    state_seen = true;
                }
                Some(EnvelopeV2::Delivered { action, .. }) => {
                    assert_eq!(action, request);
                    assert!(!private_seen);
                    private_seen = true;
                }
                other => panic!(
                    "overtaking private handler stopped polling the state future holding its authorization lane: {other:?}"
                ),
            }
        }
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        return;
    }
    let progress = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        if check_lock {
            drop(workspace.lock().await);
        }
        let encoded_request =
            tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(request.clone()))
                .unwrap();
        if check_lock {
            aw.write_all(&encoded_request).await.unwrap();
        } else {
            // A partial header remains in the stateful reader while its
            // independent writer completes an unrelated private frame.
            aw.write_all(&encoded_request[..2]).await.unwrap();
            let probe = DirectedAction {
                request_id: uuid::Uuid::from_u128(9),
                source: b.device_public_key,
                target: a.device_public_key,
                ..request.clone()
            };
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(probe.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
            let Some(EnvelopeV2::Delivered { action, .. }) = ar.read_next().await.unwrap() else {
                panic!("expected private frame during partial inbound header")
            };
            assert_eq!(action, probe);
            aw.write_all(&encoded_request[2..]).await.unwrap();
        }
        let Some(EnvelopeV2::Delivered { action, .. }) = br.read_next().await.unwrap() else {
            panic!("expected private request before any uncommitted state")
        };
        assert_eq!(action, request);
        let response = DirectedAction {
            source: b.device_public_key,
            target: a.device_public_key,
            ..request.clone()
        };
        bw.write_all(
            &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(response.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
        let Some(EnvelopeV2::Delivered { action, .. }) = ar.read_next().await.unwrap() else {
            panic!("expected private response")
        };
        assert_eq!(action, response);
    })
    .await;
    // Always release the real blocking disk operation before asserting RED.
    drop(release);
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    for task in tasks {
        task.abort();
    }
    assert!(
        progress.is_ok(),
        "held state persistence blocked private request/response (routing-lock check: {check_lock})"
    );
}

#[tokio::test]
async fn capability_controls_fence_dependent_private_during_held_operation() {
    for control in 0..3 {
        capability_control_lookahead(false, control).await;
    }
}

#[tokio::test]
async fn capability_controls_fence_dependent_private_during_reply_wait() {
    for control in 0..3 {
        capability_control_lookahead(true, control).await;
    }
}

#[tokio::test]
async fn heartbeat_keeps_progress_during_unrelated_workspace_admission() {
    capability_control_lookahead(false, 3).await;
}

#[tokio::test]
async fn heartbeat_does_not_overtake_queued_capability_control() {
    capability_control_lookahead(false, 4).await;
}

async fn capability_control_lookahead(reply_wait: bool, control: u8) {
    let (root, user) = UserIdentity::generate("owner", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let rooms = [
        uuid::Uuid::from_u128(71),
        uuid::Uuid::from_u128(72),
        uuid::Uuid::from_u128(73),
    ];
    let ids = rooms.map(|room| crate::room_workspace_id(&room.to_string()).unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let mut map = HashMap::new();
    for id in ids {
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        for peer in [&a, &b] {
            runtime.admit_device(peer.device_public_key).unwrap();
        }
        map.insert(id, Arc::new(AsyncMutex::new(runtime)));
    }
    let held_workspace = map.get(&ids[2]).unwrap().clone();
    let store = held_workspace.lock().await.store.clone();
    let registry = Arc::new(AsyncMutex::new(map));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let host = uuid::Uuid::from_u128(74);
    let mut tasks = Vec::new();
    let mut wires = Vec::new();
    for (index, peer) in [&a, &b].into_iter().enumerate() {
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
                own_device_cert: a.cert.clone(),
                own_device_public_key: a.cert.device_public_key,
                app_serving: None,
                metrics: Arc::new(crate::metrics::Metrics::default()),
                live_bridge: None,
                agents: crate::peer::AgentRegistry::default(),
                positions: None,
                compactor: None,
                member_limits: None,
            },
            std::future::pending::<()>(),
        )));
        let (read, mut write) = tokio::io::split(client);
        let mut reader = SessionReader::new(read);
        for (position, room) in rooms.iter().enumerate() {
            write
                .write_all(
                    &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                        workspace_id: ids[position],
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ready { .. }))
            ));
            if position < 2 && !(index == 0 && position == 0 && control == 0) {
                write
                    .write_all(
                        &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions {
                            room: *room,
                        })
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                assert!(matches!(
                    reader.read_next().await.unwrap(),
                    Some(EnvelopeV2::ActionsReady { .. })
                ));
            }
        }
        if index == 1 && control > 0 {
            write
                .write_all(
                    &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::RegisterBroker {
                        room: rooms[0],
                        host,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::ActionsReady { .. })
            ));
        }
        wires.push((reader, write));
    }
    let (mut ar, mut aw) = wires.remove(0);
    let (mut br, mut bw) = wires.remove(0);
    if control >= 3 {
        let gated = control == 4;
        let (release, reached) =
            crate::store::test_barrier::install(store.root(), "membership-read");
        let envelope = Envelope::Delta {
            workspace_id: ids[2],
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
        assert!(store.load_retained().unwrap().is_empty());
        let request = DirectedAction {
            room: rooms[1],
            request_id: uuid::Uuid::from_u128(77),
            source: a.device_public_key,
            target: b.device_public_key,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        if gated {
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions {
                    room: rooms[0],
                })
                .unwrap(),
            )
            .await
            .unwrap();
        }
        for frame in [
            EnvelopeV2::State(Envelope::Ping),
            EnvelopeV2::Directed(request.clone()),
        ] {
            aw.write_all(&tilo_delivery_protocol::transport_v2::encode(&frame).unwrap())
                .await
                .unwrap();
        }
        let Some(EnvelopeV2::Delivered { action, .. }) =
            tilo_test_env::within("the unrelated-room private request", br.read_next())
                .await
                .unwrap()
        else {
            panic!("unrelated private request stalled during state admission")
        };
        assert_eq!(action, request);
        let response = DirectedAction {
            source: b.device_public_key,
            target: a.device_public_key,
            ..request
        };
        bw.write_all(
            &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(response.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
        // A real later private response proves this reader and writer are
        // live. Only an earlier control may hold the heartbeat behind it.
        let first = tilo_test_env::within("the heartbeat or later private reply", ar.read_next())
            .await
            .unwrap();
        let later = if !gated && first == Some(EnvelopeV2::State(Envelope::Pong)) {
            tilo_test_env::within("the private response after the heartbeat", ar.read_next())
                .await
                .unwrap()
        } else {
            None
        };
        assert!(store.load_retained().unwrap().is_empty());
        drop(release);
        let owner = crate::persistence::handle(&held_workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        let delivered_state =
            tilo_test_env::within("the state delivery after admission release", br.read_next())
                .await
                .unwrap();
        assert_eq!(delivered_state, Some(EnvelopeV2::State(envelope)));
        let after_control = if gated
            && matches!(&first, Some(EnvelopeV2::Delivered { action, .. }) if action == &response)
        {
            let ready = tilo_test_env::within("the queued capability reply", ar.read_next())
                .await
                .unwrap();
            let pong = tilo_test_env::within("the heartbeat after its control", ar.read_next())
                .await
                .unwrap();
            Some((ready, pong))
        } else {
            None
        };
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        if gated {
            assert!(
                matches!(first, Some(EnvelopeV2::Delivered { action, .. }) if action == response),
                "a heartbeat crossed its earlier capability control"
            );
            let (ready, pong) = after_control.unwrap();
            assert!(matches!(
                ready,
                Some(EnvelopeV2::ActionsReady { room, .. }) if room == rooms[0]
            ));
            assert_eq!(pong, Some(EnvelopeV2::State(Envelope::Pong)));
        } else {
            assert_eq!(
                first,
                Some(EnvelopeV2::State(Envelope::Pong)),
                "a live transport withheld its heartbeat behind unrelated state admission"
            );
            assert!(
                matches!(later, Some(EnvelopeV2::Delivered { action, .. }) if action == response),
                "heartbeat progress lost the authorized private response"
            );
        }
        return;
    }
    let (release, reached) = crate::store::test_barrier::install(
        store.root(),
        if reply_wait {
            "replay-cleanup"
        } else {
            "replay-prepare"
        },
    );
    if reply_wait {
        let pending = Envelope::Delta {
            workspace_id: ids[2],
            author_device_public_key: b.device_public_key,
            authored_at_ns: 1,
            delta: vec![255; 8],
            signature: [0; 64],
        };
        store
            .enqueue_pending(
                &a.device_public_key,
                1,
                &crate::transport::encode(&pending).unwrap(),
            )
            .unwrap();
    }
    aw.write_all(
        &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
            workspace_id: ids[2],
        })
        .unwrap(),
    )
    .await
    .unwrap();
    if reply_wait {
        assert!(matches!(
            ar.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Delta { .. }))
        ));
    }
    reached.await.unwrap();
    if reply_wait {
        for _ in 0..12 {
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap(),
            )
            .await
            .unwrap();
        }
    }
    let capability = match control {
        0 => EnvelopeV2::EnableActions { room: rooms[0] },
        1 => EnvelopeV2::EnableBrokerClient { room: rooms[0] },
        _ => EnvelopeV2::RegisterBroker {
            room: rooms[0],
            host,
        },
    };
    let dependent = DirectedAction {
        room: rooms[0],
        request_id: uuid::Uuid::from_u128(75),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    let unrelated = DirectedAction {
        room: rooms[1],
        request_id: uuid::Uuid::from_u128(76),
        ..dependent.clone()
    };
    for frame in [
        capability,
        EnvelopeV2::Directed(dependent.clone()),
        EnvelopeV2::Directed(unrelated.clone()),
    ] {
        aw.write_all(&tilo_delivery_protocol::transport_v2::encode(&frame).unwrap())
            .await
            .unwrap();
    }
    let progress = tokio::time::timeout(tilo_test_env::hang_bound(), br.read_next()).await;
    // Receiving this later unrelated frame proves the reader classified the
    // earlier capability and dependent frame while the real owner was held.
    let premature =
        tokio::time::timeout(std::time::Duration::from_millis(50), ar.read_next()).await;
    drop(release);
    let owner = crate::persistence::handle(&held_workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert!(
        matches!(progress, Ok(Ok(Some(EnvelopeV2::Delivered { action, .. }))) if action == unrelated),
        "unrelated-room private traffic stalled"
    );
    assert!(
        premature.is_err(),
        "dependent private frame overtook capability {control} (reply wait={reply_wait}): {premature:?}"
    );
    let delivered = tilo_test_env::within("the next frame from br", br.read_next())
        .await
        .unwrap()
        .unwrap();
    if control > 0 {
        assert!(matches!(&delivered, EnvelopeV2::CertifiedDelivered { .. }));
    }
    match delivered {
        EnvelopeV2::Delivered { action, .. } | EnvelopeV2::CertifiedDelivered { action, .. } => {
            assert_eq!(action, dependent);
        }
        other => panic!("dependent request did not observe capability transition: {other:?}"),
    }
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
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let (outbound, _rx) = mpsc::channel(8);
    let (disconnect, _) = mpsc::channel(1);
    let (private, _) = mpsc::channel(8);
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &outbound,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &peer.cert,
        own_device_public_key: &peer.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
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
    let (a, _a_key) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
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
    let (btx, _brx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (bp, mut bpr) = mpsc::channel(1);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, private, session_disconnect) in [
        (&a, &atx, &ap, &disconnect),
        (&b, &btx, &bp, &recipient_disconnect),
    ] {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.action_targets.insert(
            peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: peer.clone(),
                state_tx: tx.clone(),
                tx: private.clone(),
            },
        );
    }
    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &a.cert,
        own_device_public_key: &a.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let receiver = SessionCtx {
        outbound: &btx,
        private_tx: &bp,
        disconnect: &recipient_disconnect,
        ..session
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
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
                    role: crate::auth::PeerRole::Device,
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
    let result = route_private(&action, &a, &session).await.unwrap();
    assert!(
        observer.join().unwrap(),
        "fresh membership read held routing lock"
    );
    assert_eq!(result, Some(UnavailableReason::NotAuthorised));
    assert!(bpr.try_recv().is_err());
    let _ = receiver;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorization_rechecks_certificates_after_blocking_membership_read() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _a_key) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
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
    let (btx, _brx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (bp, mut bpr) = mpsc::channel(1);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, private, session_disconnect) in [
        (&a, &atx, &ap, &disconnect),
        (&b, &btx, &bp, &recipient_disconnect),
    ] {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.action_targets.insert(
            peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: peer.clone(),
                state_tx: tx.clone(),
                tx: private.clone(),
            },
        );
    }
    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &a.cert,
        own_device_public_key: &a.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let receiver = SessionCtx {
        outbound: &btx,
        private_tx: &bp,
        disconnect: &recipient_disconnect,
        ..session
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };

    assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
    let PrivateOutbound::Delivery(delivery) = bpr.recv().await.unwrap() else {
        panic!("delivery");
    };
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: user.public_key,
        revoked_device_public_key: a.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    let root_path = wr.lock().await.store.root().to_path_buf();
    for operation in 0..5 {
        *revocations.write().unwrap() = RevocationList::new();
        let (release, reached) = crate::store::test_barrier::install(&root_path, "membership-read");
        let list = revocations.clone();
        let entry = entry.clone();
        let handle = tokio::runtime::Handle::current();
        let revoke = std::thread::spawn(move || {
            handle.block_on(async move {
                reached.await.unwrap();
                list.write().unwrap().insert(entry).unwrap();
                drop(release);
            });
        });
        let denied = match operation {
            0 => {
                route_private(&action, &a, &session).await.unwrap()
                    == Some(UnavailableReason::NotAuthorised)
            }
            1 => {
                authorize_delivery(&delivery, &b, &receiver).await
                    == Err(UnavailableReason::NotAuthorised)
            }
            2 => handle_private(EnvelopeV2::EnableActions { room }, &a, &session)
                .await
                .is_err(),
            3 => handle_private(EnvelopeV2::EnableBrokerClient { room }, &a, &session)
                .await
                .is_err(),
            _ => handle_private(
                EnvelopeV2::RegisterBroker {
                    room,
                    host: uuid::Uuid::from_u128(99),
                },
                &a,
                &session,
            )
            .await
            .is_err(),
        };
        revoke.join().unwrap();
        assert!(
            denied,
            "operation {operation} authorized a certificate revoked during its membership read"
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
        _message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        None
    }
}

#[tokio::test]
async fn revoked_device_remote_action_is_refused() {
    invalidated_device_remote_action_is_refused(false).await;
}

#[tokio::test]
async fn removed_member_remote_action_is_refused() {
    invalidated_device_remote_action_is_refused(true).await;
}

/// No access audit runs here: the handler itself refuses the next frame of
/// a device revoked or removed while its subscription is still registered.
async fn invalidated_device_remote_action_is_refused(remove_membership: bool) {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    let (a, _) = certified_device(&root, user.public_key, "a");
    let id = 11;
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
        role: a.role,
        tx: atx.clone(),
        disconnect: disconnect.clone(),
    });
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(wr)),
    )])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let counting = Arc::new(CountingPlane::default());
    let plane: Arc<dyn crate::app_serving::AppServingPlane> = counting.clone();
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &host.cert,
        own_device_public_key: &host.cert.device_public_key,
        app_serving: Some(&plane),
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
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
        to: host.device_public_key,
    };
    let served = || counting.0.load(std::sync::atomic::Ordering::SeqCst);

    // A current member reaches the plane through this very path.
    assert_eq!(
        handle_remote_action(&a, &session, &[id], route(), frame.clone()).await,
        None
    );
    assert_eq!(served(), 1);

    if remove_membership {
        let workspace = registry.lock().await[&id].clone();
        crate::workspace_io::replace_membership(&workspace, Vec::new())
            .await
            .unwrap();
        assert!(
            workspace
                .lock()
                .await
                .subscribers
                .contains_key(&a.device_public_key)
        );
    } else {
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: a.device_public_key,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        revocations.write().unwrap().insert(entry).unwrap();
    }
    let refused = handle_remote_action(&a, &session, &[id], route(), frame).await;
    let Some(Envelope::RemoteActionUndeliverable {
        reason,
        to_device_public_key,
        ..
    }) = refused
    else {
        panic!("an invalidated device is answered as undeliverable, got {refused:?}");
    };
    assert_eq!(to_device_public_key, host.device_public_key);
    assert_eq!(
        UnavailableReason::from_wire_reason(&reason),
        Some(UnavailableReason::NotAuthorised),
        "{reason}"
    );
    assert_eq!(served(), 1, "an invalidated device never reaches the plane");
}

/// An action the host runs for a member spends that member's change
/// budget, as a change they send does; reading the catalogue spends none.
#[tokio::test]
async fn a_members_actions_served_by_the_host_spend_their_change_budget() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    // A guest: another person than the host's, whose budget applies.
    let (guest_root, guest) = UserIdentity::generate("guest", 1);
    let (a, _) = certified_device(&guest_root, guest.public_key, "a");
    let id = 12;
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
        role: a.role,
        tx: atx.clone(),
        disconnect: disconnect.clone(),
    });
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(wr)),
    )])));
    let trust = Arc::new(std::sync::RwLock::new(crate::policy::TrustPolicy::AllowAny));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let counting = Arc::new(CountingPlane::default());
    let plane: Arc<dyn crate::app_serving::AppServingPlane> = counting.clone();
    let limits =
        crate::member_limits::MemberLimits::new(crate::member_limits::MemberLimitsConfig {
            changes_per_window: 1,
            window_secs: 3600,
            burst: 2,
            ..crate::member_limits::MemberLimitsConfig::default()
        });
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &host.cert,
        own_device_public_key: &host.cert.device_public_key,
        app_serving: Some(&plane),
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: Some(&limits),
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let encode = |message| {
        tilo_actions::remote::RemoteActionFrame::new(message)
            .encode()
            .unwrap()
    };
    let invoke = || {
        encode(tilo_actions::remote::RemoteActionMessage::Invoke(
            tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: "record:so.tilo.discord:".into(),
                    address: tilo_actions::ActionAddress::new("", "link-account"),
                },
                argument: tilo_actions::remote::InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            },
        ))
    };
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: host.device_public_key,
    };
    let served = || counting.0.load(std::sync::atomic::Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(
            handle_remote_action(&a, &session, &[id], route(), invoke()).await,
            None
        );
    }
    assert_eq!(served(), 2, "within the burst, the host runs them");
    let refused = handle_remote_action(&a, &session, &[id], route(), invoke()).await;
    let Some(Envelope::RemoteActionUndeliverable { reason, .. }) = refused else {
        panic!("a spent budget is refused, got {refused:?}");
    };
    assert_eq!(
        UnavailableReason::from_wire_reason(&reason),
        Some(UnavailableReason::Backpressure),
        "{reason}"
    );
    assert_eq!(served(), 2, "a refused action never reaches the host");
    let catalogue = encode(
        tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: tilo_actions::remote::CatalogueQuery::default(),
        },
    );
    assert_eq!(
        handle_remote_action(&a, &session, &[id], route(), catalogue).await,
        None
    );
    assert_eq!(served(), 3, "reading spends no change budget");
}

#[tokio::test]
async fn route_private_refuses_an_agent_whose_delegation_expired() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
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
    let (btx, _brx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (bp, mut bpr) = mpsc::channel(4);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, private, session_disconnect) in [
        (&a, &atx, &ap, &disconnect),
        (&b, &btx, &bp, &recipient_disconnect),
    ] {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.action_targets.insert(
            peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: peer.clone(),
                state_tx: tx.clone(),
                tx: private.clone(),
            },
        );
    }
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(wr)),
    )])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let agents = AgentRegistry::default();
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &a.cert,
        own_device_public_key: &a.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: &agents,
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    // Signed for this room, already past its expiry: what a delegation
    // looks like once its window closed under a live session.
    let expired = |device: [u8; 32]| {
        tilo_relay_protocol::delegation::Delegation::sign(
            &root,
            device,
            tilo_relay_protocol::delegation::Delegate::Agent {
                agent_id: "helper".into(),
            },
            "Helper",
            [1, 2, 3],
            tilo_relay_protocol::delegation::DelegateScope {
                rooms: vec![room.to_string()],
                apps: Vec::new(),
            },
            1,
            2,
        )
        .unwrap()
    };

    assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
    assert!(matches!(bpr.try_recv(), Ok(PrivateOutbound::Delivery(_))));

    for agent in [a.device_public_key, b.device_public_key] {
        agents.write().unwrap().clear();
        agents.write().unwrap().insert(agent, Some(expired(agent)));
        assert_eq!(
            route_private(&action, &a, &session).await.unwrap(),
            Some(UnavailableReason::NotAuthorised),
            "an expired agent at either end is refused per frame"
        );
        assert!(bpr.try_recv().is_err(), "nothing reaches the target");
    }
}

#[tokio::test]
async fn replacement_action_capability_requires_enable_then_durable_registration() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (source, _) = certified_device(&root, user.public_key, "source");
    let (broker, _) = certified_device(&root, user.public_key, "broker");
    let room = uuid::Uuid::from_u128(7);
    let host = uuid::Uuid::from_u128(99);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.admit_device(source.device_public_key).unwrap();
    runtime.admit_device(broker.device_public_key).unwrap();
    let (source_state, _source_state_rx) = mpsc::channel(2);
    let (old_broker_state, _old_broker_state_rx) = mpsc::channel(2);
    let (replacement_state, _replacement_state_rx) = mpsc::channel(2);
    let (source_private, _source_private_rx) = mpsc::channel(2);
    let (broker_private, mut broker_private_rx) = mpsc::channel(2);
    let (source_disconnect, _source_disconnect_rx) = mpsc::channel(1);
    let (broker_disconnect, _broker_disconnect_rx) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: source.device_public_key,
        role: source.role,
        tx: source_state.clone(),
        disconnect: source_disconnect.clone(),
    });
    runtime.add_subscriber(Subscriber {
        device_public_key: broker.device_public_key,
        role: broker.role,
        tx: old_broker_state.clone(),
        disconnect: broker_disconnect.clone(),
    });
    let runtime = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, runtime.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let source_session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &source_state,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &source.cert,
        own_device_public_key: &source.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &source_disconnect,
        presence: None,
        private_tx: &source_private,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    assert!(matches!(
        handle_private(EnvelopeV2::EnableActions { room }, &source, &source_session)
            .await
            .unwrap(),
        Some(EnvelopeV2::ActionsReady {
            capability: ActionCapability::TransportOnly,
            ..
        })
    ));
    assert!(
        handle_private(
            EnvelopeV2::EnableBrokerClient { room },
            &source,
            &source_session
        )
        .await
        .unwrap()
        .is_none()
    );

    runtime.lock().await.add_subscriber(Subscriber {
        device_public_key: broker.device_public_key,
        role: broker.role,
        tx: replacement_state.clone(),
        disconnect: broker_disconnect.clone(),
    });
    let replacement_session = SessionCtx {
        outbound: &replacement_state,
        private_tx: &broker_private,
        disconnect: &broker_disconnect,
        ..source_session
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: source.device_public_key,
        target: broker.device_public_key,
        message: ActionMessage::InvokeBound {
            host,
            invocation: tilo_actions::InvocationId::new(),
            app_peer: "1".into(),
            app_id: "so.tilo.test".into(),
            record_id: "record".into(),
            resource: "record".into(),
            action: "test".into(),
            argument: None,
            confirmation: None,
            revision: None,
        },
    };
    assert_eq!(
        route_private(&action, &source, &source_session)
            .await
            .unwrap(),
        Some(UnavailableReason::UnsupportedPeer)
    );
    assert!(broker_private_rx.try_recv().is_err());

    assert_eq!(
        handle_private(
            EnvelopeV2::EnableActions { room },
            &broker,
            &replacement_session
        )
        .await
        .unwrap(),
        Some(EnvelopeV2::ActionsReady {
            room,
            capability: ActionCapability::TransportOnly,
        })
    );
    assert_eq!(
        route_private(&action, &source, &source_session)
            .await
            .unwrap(),
        Some(UnavailableReason::AdmissionUnavailable)
    );
    assert!(broker_private_rx.try_recv().is_err());

    assert_eq!(
        handle_private(
            EnvelopeV2::RegisterBroker { room, host },
            &broker,
            &replacement_session
        )
        .await
        .unwrap(),
        Some(EnvelopeV2::ActionsReady {
            room,
            capability: ActionCapability::DurableBroker { host },
        })
    );
    assert_eq!(
        route_private(&action, &source, &source_session)
            .await
            .unwrap(),
        None
    );
    assert!(matches!(
        broker_private_rx.recv().await,
        Some(PrivateOutbound::Delivery(_))
    ));

    runtime
        .lock()
        .await
        .remove_subscriber_session(&broker.device_public_key, &old_broker_state);
    let current = runtime.lock().await;
    assert!(
        current
            .subscribers
            .get(&broker.device_public_key)
            .is_some_and(|subscriber| subscriber.tx.same_channel(&replacement_state))
    );
    assert!(
        current
            .action_targets
            .get(&broker.device_public_key)
            .is_some_and(|target| target.broker_host == Some(host)
                && target.state_tx.same_channel(&replacement_state))
    );
    drop(current);
    assert_eq!(
        route_private(&action, &source, &source_session)
            .await
            .unwrap(),
        None
    );
    assert!(matches!(
        broker_private_rx.recv().await,
        Some(PrivateOutbound::Delivery(_))
    ));
}

#[tokio::test]
async fn bounded_private_queue_and_delivery_recheck_never_retain_traffic() {
    Box::pin(bounded_private_queue_for_room_namespace()).await;
}

async fn bounded_private_queue_for_room_namespace() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, a_key) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
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
    let (btx, _brx) = mpsc::channel(2);
    let (ap, mut apr) = mpsc::channel(2);
    let (bp, mut bpr) = mpsc::channel(1);
    let (disconnect, mut disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, mut recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, private, session_disconnect) in [
        (&a, &atx, &ap, &disconnect),
        (&b, &btx, &bp, &recipient_disconnect),
    ] {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.action_targets.insert(
            peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: peer.clone(),
                state_tx: tx.clone(),
                tx: private.clone(),
            },
        );
    }
    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &a.cert,
        own_device_public_key: &a.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let receiver = SessionCtx {
        outbound: &btx,
        private_tx: &bp,
        disconnect: &recipient_disconnect,
        ..session
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    assert!(
        !private_may_overtake(
            &EnvelopeV2::Directed(action.clone()),
            Some(id),
            false,
            std::iter::empty(),
        ),
        "private traffic must not overtake its room's state control"
    );
    // An old-v2 caller never receives an unknown broker message, even
    // when it targets an endpoint that installed the extended facade.
    wr.lock()
        .await
        .action_targets
        .get_mut(&b.device_public_key)
        .unwrap()
        .broker_host = Some(uuid::Uuid::from_u128(99));
    assert_eq!(
        route_private(&action, &a, &session).await.unwrap(),
        Some(UnavailableReason::UnsupportedPeer)
    );
    assert!(bpr.try_recv().is_err());
    wr.lock()
        .await
        .action_targets
        .get_mut(&b.device_public_key)
        .unwrap()
        .broker_host = None;
    // State resubscribe is idempotent on the same connection. It must not
    // silently withdraw a separately negotiated private capability.
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: b.device_public_key,
        role: b.role,
        tx: btx.clone(),
        disconnect: recipient_disconnect.clone(),
    });
    assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
    assert_eq!(
        route_private(&action, &a, &session).await.unwrap(),
        Some(UnavailableReason::Backpressure)
    );
    let PrivateOutbound::Delivery(delivery) = bpr.recv().await.unwrap() else {
        panic!("expected private delivery")
    };
    assert_eq!(authorize_delivery(&delivery, &b, &receiver).await, Ok(()));
    // Sender removal also invalidates an already queued private result.
    wr.lock()
        .await
        .store
        .save_membership(&[b.device_public_key])
        .unwrap();
    assert_eq!(
        route_private(&action, &a, &session).await.unwrap(),
        Some(UnavailableReason::NotAuthorised)
    );
    let mut sender_removed_wire = Vec::new();
    write_private_delivery(&delivery, &b, &receiver, &mut sender_removed_wire)
        .await
        .unwrap();
    assert!(sender_removed_wire.is_empty());
    assert!(matches!(
        apr.recv().await,
        Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }))
    ));
    wr.lock()
        .await
        .store
        .save_membership(&[a.device_public_key, b.device_public_key])
        .unwrap();

    // Root-signed revocation between dispatch and actual writer delivery.
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: user.public_key,
        revoked_device_public_key: a.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    revocations.write().unwrap().insert(entry).unwrap();
    let mut revoked_wire = Vec::new();
    write_private_delivery(&delivery, &b, &receiver, &mut revoked_wire)
        .await
        .unwrap();
    assert!(revoked_wire.is_empty());
    assert!(matches!(
        apr.recv().await,
        Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }))
    ));
    *revocations.write().unwrap() = RevocationList::new();

    // Expiry is rechecked at the writer, after the authenticated source's
    // queued delivery was created. Re-sign the short-lived certificate so
    // the fixture still uses a valid root-issued chain.
    let mut expiring = delivery.clone();
    expiring.source.cert.expires_at_ns = Some(unix_now_ns().saturating_add(100_000_000));
    sign_device_cert(&mut expiring.source.cert, &root).unwrap();
    expiring.source = crate::auth::accept_hello(
        &crate::auth::Hello::new(expiring.source.cert.clone(), [9; 32]),
        &user.public_key,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    crate::auth::verify_hello_ack2(
        &crate::auth::build_hello_ack2(&a_key, &[8; 32]),
        &expiring.source.device_public_key,
        &[8; 32],
    )
    .unwrap();
    assert_eq!(authorize_delivery(&expiring, &b, &receiver).await, Ok(()));
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let mut expired_wire = Vec::new();
    write_private_delivery(&expiring, &b, &receiver, &mut expired_wire)
        .await
        .unwrap();
    assert!(expired_wire.is_empty());
    assert!(matches!(
        apr.recv().await,
        Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }))
    ));

    // Host certificates retain their PeerRelay role; no facade exception.
    let mut relay_peer = a.clone();
    relay_peer.cert.capabilities = DeviceCapabilities::host();
    sign_device_cert(&mut relay_peer.cert, &root).unwrap();
    relay_peer = crate::auth::accept_hello(
        &crate::auth::Hello::new(relay_peer.cert.clone(), [7; 32]),
        &user.public_key,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    crate::auth::verify_hello_ack2(
        &crate::auth::build_hello_ack2(&a_key, &[6; 32]),
        &relay_peer.device_public_key,
        &[6; 32],
    )
    .unwrap();
    assert_eq!(relay_peer.role, crate::auth::PeerRole::PeerRelay);
    assert_eq!(
        route_private(&action, &relay_peer, &session).await.unwrap(),
        Some(UnavailableReason::NotAuthorised)
    );

    // Removal after dispatch but before recipient wire delivery fails closed.
    wr.lock()
        .await
        .store
        .save_membership(&[a.device_public_key])
        .unwrap();
    assert_eq!(
        authorize_delivery(&delivery, &b, &receiver).await,
        Err(UnavailableReason::NotAuthorised)
    );
    wr.lock()
        .await
        .store
        .save_membership(&[a.device_public_key, b.device_public_key])
        .unwrap();
    // Exact room equality is checked in addition to the numeric shard.
    wr.lock()
        .await
        .action_targets
        .get_mut(&b.device_public_key)
        .unwrap()
        .room = uuid::Uuid::from_u128(9);
    assert_eq!(
        route_private(&action, &a, &session).await.unwrap(),
        Some(UnavailableReason::NotAuthorised)
    );
    wr.lock()
        .await
        .action_targets
        .get_mut(&b.device_public_key)
        .unwrap()
        .room = room;
    let (replacement, _rx) = mpsc::channel(2);
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: b.device_public_key,
        role: b.role,
        tx: replacement.clone(),
        disconnect: recipient_disconnect.clone(),
    });
    assert_eq!(
        authorize_delivery(&delivery, &b, &receiver).await,
        Err(UnavailableReason::NotAuthorised)
    );
    wr.lock()
        .await
        .remove_subscriber_session(&b.device_public_key, &btx);
    assert!(
        wr.lock()
            .await
            .subscribers
            .get(&b.device_public_key)
            .unwrap()
            .tx
            .same_channel(&replacement)
    );
    // A recipient-side rejection must reach the original source, or close
    // that exact session when its reply lane has no remaining capacity.
    let mut wire = Vec::new();
    write_private_delivery(&delivery, &b, &receiver, &mut wire)
        .await
        .unwrap();
    assert!(wire.is_empty());
    let Some(PrivateOutbound::Reply(reply)) = apr.recv().await else {
        panic!("missing rejection")
    };
    assert_eq!(
        reply,
        EnvelopeV2::unavailable(&action, UnavailableReason::NotAuthorised)
    );
    for _ in 0..2 {
        ap.try_send(PrivateOutbound::Reply(reply.clone()))
            .ok()
            .unwrap();
    }
    let (new_source, new_source_rx) = mpsc::channel(2);
    let (new_disconnect, mut new_disconnect_rx) = mpsc::channel(1);
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
        role: a.role,
        tx: new_source.clone(),
        disconnect: new_disconnect,
    });
    write_private_delivery(&delivery, &b, &receiver, &mut wire)
        .await
        .unwrap();
    assert!(
        wire.is_empty(),
        "rejected private body must not reach recipient wire"
    );
    assert_eq!(
        disconnect_rx.try_recv(),
        Ok(()),
        "a saturated rejection queue must terminate its original source session"
    );
    assert!(
        matches!(
            new_disconnect_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ),
        "replacement source session must not be terminated"
    );
    assert!(!new_source_rx.is_closed());
    assert!(
        matches!(
            recipient_disconnect_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ),
        "recipient session must not be terminated for source backpressure"
    );
    let wr = wr.lock().await;
    assert_eq!(wr.store.count_pending(&a.device_public_key).unwrap(), 0);
    assert_eq!(wr.store.count_pending(&b.device_public_key).unwrap(), 0);
    assert!(wr.store.load_retained().unwrap().is_empty());
    assert!(wr.store.load_snapshot().unwrap().is_none());
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
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
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
        role: browser.role,
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
    wr.fanout_delta(&native.device_public_key, native.role, &env, &raw, 1)
        .unwrap();
    brx.try_recv().unwrap();
    assert!(!wr.needs_checkpoint(&browser.device_public_key, &[1; 32]));

    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let (ptx, _prx) = mpsc::channel(1);
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &btx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &browser.cert,
        own_device_public_key: &browser.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &bdis,
        presence: None,
        private_tx: &ptx,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    // The browser sends a SyncReq as presence.
    handle_presence(
        &browser,
        &session,
        &[id],
        Envelope::Presence {
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
        .fanout_delta(&native.device_public_key, native.role, &env2, &raw, 2)
        .unwrap();
    assert!(brx.try_recv().is_ok());
    let env3 = checkpoint_at(3);
    wr.lock()
        .await
        .fanout_delta(&native.device_public_key, native.role, &env3, &raw, 3)
        .unwrap();
    assert!(brx.try_recv().is_err(), "satisfied by one checkpoint");

    let (native_tx, mut native_rx) = mpsc::channel(4);
    let (native_disconnect, _native_disconnect_rx) = mpsc::channel(1);
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: native.device_public_key,
        role: native.role,
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
        Envelope::Presence {
            workspace_id: id,
            payload: forged.encode().unwrap(),
        },
    )
    .await
    .unwrap();
    let Envelope::Presence { payload, .. } = native_rx.try_recv().unwrap() else {
        panic!("expected forwarded presence");
    };
    let observed = crate::AttributedPresence::decode(&payload).unwrap();
    assert_eq!(observed.author, browser.device_public_key);
    assert_eq!(observed.body, forged.body);
}
